use crate::{
    Args,
    hardware::Collector,
    model::{Event, Metrics, number, truth},
    policy::{Policy, Resize, Sample},
};
use anyhow::{Context, Result, bail};
use reqwest::{
    blocking::Client,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::{CommandExt, ExitStatusExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn signals() -> Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        signal_hook::flag::register(sig, flag.clone())?;
    }
    Ok(flag)
}

pub fn pid_start(pid: u32) -> Result<String> {
    let line = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    Ok(line
        .rsplit_once(')')
        .context("invalid process stat")?
        .1
        .split_whitespace()
        .nth(19)
        .context("missing process start time")?
        .into())
}

pub fn emit(event: Event) {
    if let Ok(line) = serde_json::to_string(&event) {
        let _ = writeln!(std::io::stdout().lock(), "{line}");
    }
}

pub struct Log {
    path: PathBuf,
    file: Mutex<File>,
}
impl Log {
    pub fn new(model: &str) -> Result<Self> {
        let dir = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::home().join(".local/state"))
            .join("strata-wrap");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{model}.log"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }
    pub fn write(&self, message: &str, raw: bool) {
        let mut file = self.file.lock().unwrap_or_else(|p| p.into_inner());
        if file.metadata().is_ok_and(|m| m.len() > 8 * 1024 * 1024) {
            for n in (1..=3).rev() {
                let src = if n == 1 {
                    self.path.clone()
                } else {
                    self.path.with_extension(format!("log.{}", n - 1))
                };
                let _ = fs::rename(src, self.path.with_extension(format!("log.{n}")));
            }
            if let Ok(next) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                *file = next;
            }
        }
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = writeln!(
            file,
            "{at} [{}] {message}",
            if raw { "server" } else { "wrapper" }
        );
        drop(file);
        emit(Event::Log {
            message: message.into(),
            raw,
        });
    }
}

#[derive(Clone)]
pub struct Api {
    client: Client,
    pub url: String,
}
impl Api {
    pub fn new(port: u16, key: &str) -> Result<Self> {
        let mut headers = HeaderMap::new();
        if !key.is_empty() {
            let mut value = HeaderValue::from_str(&format!("Bearer {key}"))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        Ok(Self {
            client: Client::builder()
                .no_proxy()
                .default_headers(headers)
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(2))
                .build()?,
            url: format!("http://127.0.0.1:{port}"),
        })
    }
    fn get(&self, path: &str) -> Result<Value> {
        Ok(self
            .client
            .get(format!("{}{path}", self.url))
            .send()?
            .error_for_status()?
            .json()?)
    }
    fn resize(&self, action: &Resize) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}/v1/vram", self.url))
            .timeout(Duration::from_secs(430))
            .json(&json!({"reserve_mib":action.reserve}))
            .send()?;
        let status = response.status();
        let reply: Value = response.json()?;
        if !status.is_success() {
            bail!(
                "HTTP {status}: {}",
                reply
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("resize refused")
            );
        }
        if reply["status"] != "ok" || reply["expert_cache_mib"].as_u64().is_none() {
            bail!("resize response did not report a completed cache resize");
        }
        Ok(reply)
    }
}

enum Input {
    Metrics(Result<Metrics, String>),
    Hardware(Box<crate::hardware::Hardware>),
    Resized(Result<Value, String>),
}

struct Server(Child);
impl Server {
    fn stop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        // SAFETY: kill only sends a signal to the child PID retained by Child.
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn worker(args: &Args) -> Result<i32> {
    let config: Value =
        serde_json::from_slice(&fs::read(args.config()).context("read model config")?)?;
    if config["vram_elastic"] != true {
        bail!("enable vram_elastic in {}", args.config().display());
    }
    if config["vram_segment_mib"].as_u64().unwrap_or(512) != 512 {
        bail!("512 MiB cache segments are required");
    }
    let port = args
        .port
        .unwrap_or_else(|| config["port"].as_u64().unwrap_or(8080) as u16);
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(runtime.join(format!("strata-wrap-{port}.lock")))?;
    // SAFETY: flock uses a live descriptor and does not retain a Rust pointer.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("another wrapper owns port {port}");
    }
    if TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse()?,
        Duration::from_millis(250),
    )
    .is_ok()
    {
        bail!("port {port} already has a server; stop it first");
    }
    let owner = if let Some(pid) = args.owner_pid {
        if Some(pid_start(pid)?) != args.owner_start {
            bail!("launcher has exited");
        }
        // SAFETY: pidfd_open returns a new owned descriptor, checked before construction.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Some(unsafe { OwnedFd::from_raw_fd(fd) })
    } else {
        None
    };
    let stop = signals()?;
    let log = Arc::new(Log::new(
        args.config()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("strata"),
    )?);
    log.write(
        &format!(
            "Loading {} on 127.0.0.1:{port}",
            config["model_name"].as_str().unwrap_or("Strata")
        ),
        false,
    );
    log.write(&format!("Raw output: {}", log.path.display()), false);
    let api = Api::new(port, config["api_key"].as_str().unwrap_or(""))?;
    let mut command = Command::new(args.strata_dir.join(".venv/bin/python"));
    command
        .args(["-u", "serve/server.py", "--engine", "strata", "--config"])
        .arg(args.config())
        .args(&args.server_args)
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .current_dir(&args.strata_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut server = Server(command.spawn().context("start Strata")?);
    for pipe in [
        server
            .0
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        server
            .0
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let log = log.clone();
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                log.write(&line, true);
            }
        });
    }
    let (tx, rx) = mpsc::channel();
    {
        let api = api.clone();
        let tx = tx.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let data = (|| -> Result<Metrics> {
                    Ok(Metrics {
                        status: api.get("/v1/status")?,
                        data: api.get("/metrics")?,
                    })
                })();
                if tx
                    .send(Input::Metrics(data.map_err(|e| e.to_string())))
                    .is_err()
                {
                    break;
                }
                for _ in 0..5 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }
    {
        let tx = tx.clone();
        let stop = stop.clone();
        let poll = args.poll;
        let gpu = config["gpu"].as_u64().unwrap_or(0) as u32;
        thread::spawn(move || {
            let mut collector = Collector::new(gpu);
            while !stop.load(Ordering::Relaxed) {
                if tx
                    .send(Input::Hardware(Box::new(collector.sample())))
                    .is_err()
                {
                    break;
                }
                let until = Instant::now() + Duration::from_secs_f64(poll);
                while Instant::now() < until {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }
    let origin = Instant::now();
    let mut policy = Policy::new(
        args.min_free_mib,
        args.grow_below_mib,
        args.grow_delay,
        args.poll,
    );
    let mut metrics: Option<(Metrics, Instant)> = None;
    let mut disabled = None::<String>;
    let mut last_state = String::new();
    let mut next_policy = Instant::now();
    let mut result = 0;
    let mut last_failure = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Some(owner) = &owner {
            let mut poll = libc::pollfd {
                fd: owner.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll references one initialized descriptor for this call.
            if unsafe { libc::poll(&mut poll, 1, 0) } > 0 {
                log.write("Launcher exited; stopping Strata", false);
                break;
            }
        }
        if let Some(status) = server.0.try_wait()? {
            result = status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
            break;
        }
        let now = origin.elapsed().as_secs_f64();
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Input::Metrics(Ok(data))) => {
                policy.activity(data.activity(), data.busy());
                emit(Event::Metrics(data.clone()));
                metrics = Some((data, Instant::now()));
            }
            Ok(Input::Metrics(Err(error))) => {
                policy.interrupt();
                if metrics.is_some() && last_failure.elapsed().as_secs() >= 30 {
                    log.write(&format!("Monitoring interrupted: {error}"), false);
                    last_failure = Instant::now();
                }
            }
            Ok(Input::Hardware(hw)) => {
                if let Some((data, updated)) = &metrics {
                    if updated.elapsed().as_secs() < 3
                        && truth(&data.status, "/loaded")
                        && truth(&data.status, "/vram/elastic")
                        && disabled.is_none()
                        && hw.gpu_error.is_none()
                    {
                        let sample = Sample {
                            used: hw.gpu_used >> 20,
                            free: hw.gpu_free >> 20,
                            cache: data.cache(),
                            full: data.full_cache(),
                            activity: data.activity(),
                            busy: data.busy(),
                        };
                        if let Some(action) = policy.observe(now, sample) {
                            log.write(
                                &format!(
                                    "VRAM {} requested: target {} MiB free",
                                    if action.grow { "grow" } else { "shrink" },
                                    action.reserve
                                ),
                                false,
                            );
                            let api = api.clone();
                            let tx = tx.clone();
                            thread::spawn(move || {
                                let reply = api.resize(&action).map_err(|e| e.to_string());
                                let _ = tx.send(Input::Resized(reply));
                            });
                        }
                    } else {
                        policy.interrupt();
                    }
                }
                emit(Event::Hardware(hw));
            }
            Ok(Input::Resized(reply)) => {
                let before = policy.pending.as_ref().map(|(a, _)| a.before).unwrap_or(0);
                match reply {
                    Ok(reply) => {
                        let cache = reply["expert_cache_mib"].as_u64();
                        let changed = policy.complete(now, cache);
                        log.write(
                            &format!(
                                "VRAM cache {before} → {} MiB · {} MiB free{}",
                                cache.unwrap_or(0),
                                number(&reply, "/vram_free_mib"),
                                if changed {
                                    ""
                                } else {
                                    " · no change; waiting for conditions to change"
                                }
                            ),
                            false,
                        );
                        // Do not wait for the next metrics poll to learn the new cache size.
                        if let Some((data, _)) = &mut metrics {
                            data.status["vram"] = reply;
                            data.status["vram"]["elastic"] = json!(true);
                        }
                    }
                    Err(error) => {
                        policy.complete(now, None);
                        disabled = Some("Paused · resize failed; restart to resume".into());
                        log.write(&format!("VRAM management paused: {error}"), false);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= next_policy {
            let state = if let Some(reason) = &disabled {
                reason.clone()
            } else if policy.pending.is_some() {
                "Resizing · waiting for Strata".into()
            } else if metrics
                .as_ref()
                .is_some_and(|(m, _)| !truth(&m.status, "/loaded"))
            {
                "Model unloaded · waiting for a request".into()
            } else if metrics
                .as_ref()
                .is_none_or(|(_, at)| at.elapsed().as_secs() >= 3)
            {
                "Waiting for monitoring data".into()
            } else if metrics
                .as_ref()
                .is_some_and(|(m, _)| !truth(&m.status, "/vram/elastic"))
            {
                "Unavailable · engine is not elastic".into()
            } else if metrics.as_ref().is_some_and(|(m, _)| m.busy()) {
                "Holding · inference active".into()
            } else if metrics
                .as_ref()
                .is_some_and(|(m, _)| m.cache() >= m.full_cache())
            {
                "Holding · full expert cache".into()
            } else if let Some(left) = policy.quiet_left(now) {
                format!("Growing in {:.0}s of spare capacity", left.ceil())
            } else {
                "Holding · watching free memory".into()
            };
            if state != last_state || policy.quiet_left(now).is_some() {
                emit(Event::Policy {
                    state: state.clone(),
                    quiet_left: policy.quiet_left(now),
                    pending: policy.pending.is_some(),
                });
                last_state = state;
            }
            next_policy = Instant::now() + Duration::from_millis(500);
        }
    }
    stop.store(true, Ordering::Relaxed);
    log.write("Stopping Strata", false);
    server.stop();
    log.write("Stopped", false);
    Ok(result)
}

pub fn owned_unit(args: &Args) -> bool {
    Command::new("systemctl")
        .args([
            "--user",
            "show",
            &args.unit_name(),
            "--property=ExecStart",
            "--value",
        ])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .contains(&format!("--owner-pid {} ", std::process::id()))
        })
}
pub fn stop_unit(args: &Args, force: bool) {
    if owned_unit(args) {
        let mut cmd = Command::new("systemctl");
        cmd.args(["--user"]);
        if force {
            cmd.args(["kill", "--kill-whom=all", "--signal=SIGKILL"]);
        } else {
            cmd.args(["stop", "--no-block"]);
        }
        let _ = cmd
            .arg(args.unit_name())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub fn read_config(args: &Args) -> Result<Value> {
    Ok(serde_json::from_slice(
        &fs::read(args.config()).with_context(|| format!("read {}", args.config().display()))?,
    )?)
}
pub fn executable() -> Result<PathBuf> {
    std::env::current_exe().context("locate wrapper executable")
}

pub fn launch_command(args: &Args) -> Result<Command> {
    let mut cmd = Command::new("systemd-run");
    cmd.args([
        "--user",
        "--quiet",
        "--collect",
        "--wait",
        "--pipe",
        "--service-type=exec",
        "--expand-environment=no",
    ])
    .arg(format!("--unit={}", args.unit_name()))
    .args([
        "--property=KillMode=mixed",
        "--property=TimeoutStopSec=20s",
        "--property=SendSIGKILL=yes",
        "--property=Restart=no",
    ]);
    for (key, value) in std::env::vars() {
        if key.starts_with("STRATA_")
            || [
                "TERM",
                "NO_COLOR",
                "COLORTERM",
                "CUDA_VISIBLE_DEVICES",
                "XDG_STATE_HOME",
            ]
            .contains(&key.as_str())
        {
            cmd.arg(format!("--setenv={key}={value}"));
        }
    }
    let model = args.model.as_deref().context("model name missing")?;
    let model_arg = if matches!(model, "iq3_s" | "ud-iq3_s") {
        std::path::PathBuf::from(model)
    } else {
        args.config()
    };
    cmd.arg(executable()?)
        .arg(model_arg)
        .args([
            "--worker",
            "--owner-pid",
            &std::process::id().to_string(),
            "--owner-start",
            &pid_start(std::process::id())?,
        ])
        .arg("--strata-dir")
        .arg(std::path::absolute(&args.strata_dir)?)
        .args([
            "--unit",
            &args.unit,
            "--min-free-mib",
            &args.min_free_mib.to_string(),
            "--grow-below-mib",
            &args.grow_below_mib.to_string(),
            "--grow-delay",
            &args.grow_delay.to_string(),
            "--poll",
            &args.poll.to_string(),
        ]);
    if let Some(port) = args.port {
        cmd.args(["--port", &port.to_string()]);
    }
    if !args.server_args.is_empty() {
        cmd.arg("--").args(&args.server_args);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    Ok(cmd)
}
