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
        let busy = status == reqwest::StatusCode::CONFLICT
            && reply.pointer("/error/type").and_then(Value::as_str) == Some("model_busy");
        let batch_busy = status == reqwest::StatusCode::BAD_REQUEST
            && reply.pointer("/error/message").and_then(Value::as_str)
                == Some("VRAM: not while batch slots are decoding");
        if busy || batch_busy {
            bail!("RETRYABLE: engine is busy; waiting for a resize opportunity");
        }
        if !status.is_success() {
            bail!(
                "HTTP {status}: {}",
                reply
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("resize refused")
            );
        }
        if reply["status"] == "not loaded" {
            bail!("RETRYABLE: model unloaded before resize");
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
    if !args.no_vram_management && config["vram_elastic"] != true {
        bail!("enable vram_elastic in {}", args.config().display());
    }
    let segment = config["vram_segment_mib"].as_u64().unwrap_or(512);
    if !args.no_vram_management && (segment < 64 || !args.step_mib.is_multiple_of(segment)) {
        bail!("step_mib must be a multiple of the model's vram_segment_mib ({segment})");
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
    log.write(
        &format!(
            "Wrapper settings: {}",
            if args.no_config {
                "built-in defaults and CLI overrides".to_owned()
            } else {
                args.settings_path.display().to_string()
            }
        ),
        false,
    );
    log.write(&format!("VRAM policy: GPU {}; reserve {} MiB; grow below {} MiB used; {:?} {}s; step {} MiB; margin {} MiB; no-op retry {}s{}",
        args.gpu.as_deref().unwrap_or("0"), args.min_free_mib, args.grow_below_mib, args.growth_mode,
        args.grow_delay, args.step_mib,args.accounting_margin_mib,args.no_op_retry,
        if args.no_vram_management { "; management disabled" } else { "" }),false);
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
    if let Some(gpu) = &args.gpu {
        command.args(["--gpu", gpu]);
    }
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
        let gpu = args
            .gpu
            .as_deref()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
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
    policy.growth_mode = args.growth_mode;
    policy.step = args.step_mib;
    policy.margin = args.accounting_margin_mib;
    policy.retry_delay = args.no_op_retry;
    let mut metrics: Option<(Metrics, Instant)> = None;
    let mut disabled = args
        .no_vram_management
        .then(|| "VRAM management disabled in settings".to_owned());
    let mut last_state = String::new();
    let mut gpu_error = None::<String>;
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
                gpu_error = hw.gpu_error.clone();
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
                                    " · no change in requested direction; retrying after cooldown"
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
                    Err(error) if error.starts_with("RETRYABLE:") => {
                        policy.complete(now, None);
                        log.write(
                            &format!(
                                "VRAM resize deferred: {}; retry after {}s",
                                error.trim_start_matches("RETRYABLE: "),
                                args.no_op_retry
                            ),
                            false,
                        );
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
            } else if let Some(error) = &gpu_error {
                format!("Unavailable · GPU telemetry: {error}")
            } else {
                policy.status(now)
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

pub fn diagnose(args: &Args) -> Result<i32> {
    let config = read_config(args)?;
    let port = args
        .port
        .unwrap_or_else(|| config["port"].as_u64().unwrap_or(8080) as u16);
    let api = Api::new(port, config["api_key"].as_str().unwrap_or(""))?;
    let m = Metrics {
        status: api.get("/v1/status")?,
        data: api.get("/metrics")?,
    };
    let index = args.gpu.as_deref().unwrap_or("0").parse::<u32>()?;
    let gpu = crate::hardware::gpus()?
        .into_iter()
        .find(|g| g.index == index)
        .context("GPU not found")?;
    let mut reasons = Vec::<String>::new();
    if args.no_vram_management {
        reasons.push("management disabled".into());
    }
    if !truth(&m.status, "/loaded") {
        reasons.push("model unloaded".into());
    }
    if !truth(&m.status, "/vram/elastic") {
        reasons.push("engine not elastic".into());
    }
    if m.cache() >= m.full_cache() {
        reasons.push("full expert cache".into());
    }
    if m.busy() && args.growth_mode == crate::policy::GrowthMode::Idle {
        reasons.push(
            "idle mode requires a full quiet period without active or queued inference".into(),
        );
    }
    if gpu.used_mib >= args.grow_below_mib {
        reasons.push("GPU usage is at or above the growth threshold".into());
    }
    if gpu.free_mib
        < args
            .min_free_mib
            .saturating_add(args.step_mib)
            .saturating_add(args.accounting_margin_mib)
    {
        reasons.push("not enough free memory to preserve the reserve after one growth step".into());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "read_only":true,"gpu":{"index":index,"name":gpu.name,"uuid":gpu.uuid,
                "total_mib":gpu.total_mib,"used_mib":gpu.used_mib,"free_mib":gpu.free_mib,"driver_reserved_mib":gpu.reserved_mib},
            "policy":{"growth_mode":args.growth_mode,"min_free_mib":args.min_free_mib,"grow_below_mib":args.grow_below_mib,
                "grow_delay_seconds":args.grow_delay,"step_mib":args.step_mib,"accounting_margin_mib":args.accounting_margin_mib},
            "engine":{"busy":m.busy(),"cache_mib":m.cache(),"full_cache_mib":m.full_cache(),"activity":m.activity()},
            "growth_blockers":reasons,
            "timing_note":"This snapshot cannot read the running worker's timer. Capacity mode waits for sustained spare VRAM; idle mode also waits for inference to stop. Strata applies queued resizes between requests."
        }))?
    );
    Ok(0)
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
            "--growth-mode",
            match args.growth_mode {
                crate::policy::GrowthMode::Idle => "idle",
                crate::policy::GrowthMode::Capacity => "capacity",
            },
            "--step-mib",
            &args.step_mib.to_string(),
            "--accounting-margin-mib",
            &args.accounting_margin_mib.to_string(),
            "--no-op-retry",
            &args.no_op_retry.to_string(),
        ]);
    if args.no_vram_management {
        cmd.arg("--no-vram-management");
    }
    if args.no_config {
        cmd.arg("--no-config");
    }
    if !args.settings_path.as_os_str().is_empty() {
        cmd.arg("--config").arg(&args.settings_path);
    }
    if let Some(gpu) = &args.gpu {
        cmd.args(["--gpu", gpu]);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn resize_reply(status: u16, reply: Value) -> Result<Value> {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let task = thread::spawn(move || {
            let mut request = server.recv().unwrap();
            assert_eq!(request.url(), "/v1/vram");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap()["reserve_mib"],
                2000
            );
            request
                .respond(
                    tiny_http::Response::from_string(reply.to_string()).with_status_code(status),
                )
                .unwrap();
        });
        let result = Api::new(port, "").unwrap().resize(&Resize {
            grow: true,
            reserve: 2000,
            before: 4096,
        });
        task.join().unwrap();
        result
    }

    #[test]
    fn known_busy_responses_defer_but_uncertain_failures_pause() {
        for (status, reply) in [
            (409, json!({"error":{"type":"model_busy"}})),
            (
                400,
                json!({"error":{"message":"VRAM: not while batch slots are decoding"}}),
            ),
            (200, json!({"status":"not loaded"})),
        ] {
            assert!(
                resize_reply(status, reply)
                    .unwrap_err()
                    .to_string()
                    .starts_with("RETRYABLE:")
            );
        }
        for (status, reply) in [
            (400, json!({"error":{"message":"not elastic"}})),
            (503, json!({"error":{"message":"engine did not answer"}})),
            (200, json!({"status":"ok"})),
            (200, json!({"status":"deferred", "expert_cache_mib":4608})),
        ] {
            assert!(
                !resize_reply(status, reply)
                    .unwrap_err()
                    .to_string()
                    .starts_with("RETRYABLE:")
            );
        }
        assert_eq!(
            resize_reply(200, json!({"status":"ok", "expert_cache_mib":4608})).unwrap()["expert_cache_mib"],
            4608
        );
    }
    #[test]
    fn worker_receives_resolved_settings_without_reloading_the_file() {
        use clap::{CommandFactory, FromArgMatches, Parser};
        let mut original = Args::parse_from([
            "strata-wrap",
            "iq3_s",
            "--growth-mode",
            "capacity",
            "--step-mib",
            "256",
            "--accounting-margin-mib",
            "32",
            "--no-op-retry",
            "90",
            "--gpu",
            "2",
            "--min-free-mib",
            "2048",
            "--grow-below-mib",
            "29000",
            "--grow-delay",
            "70",
            "--poll",
            "3",
            "--no-vram-management",
        ]);
        let dir = tempfile::tempdir().unwrap();
        original.settings_path = dir.path().join("config.json");
        fs::write(&original.settings_path, "invalid").unwrap();
        let command = launch_command(&original).unwrap();
        let argv: Vec<_> = command.get_args().map(|s| s.to_os_string()).collect();
        let start = argv.iter().position(|s| s == "iq3_s").unwrap();
        let matches = Args::command()
            .try_get_matches_from(
                std::iter::once(std::ffi::OsString::from("strata-wrap"))
                    .chain(argv[start..].iter().cloned()),
            )
            .unwrap();
        let mut worker = Args::from_arg_matches(&matches).unwrap();
        crate::config::apply(&mut worker, &matches).unwrap();
        worker.validate().unwrap();
        assert!(worker.worker && worker.no_vram_management);
        assert_eq!(worker.growth_mode, original.growth_mode);
        assert_eq!(worker.step_mib, 256);
        assert_eq!(worker.accounting_margin_mib, 32);
        assert_eq!(worker.no_op_retry, 90.);
        assert_eq!(worker.gpu.as_deref(), Some("2"));
        assert_eq!(worker.min_free_mib, 2048);
        assert_eq!(worker.grow_below_mib, 29000);
        assert_eq!(worker.grow_delay, 70.);
        assert_eq!(worker.poll, 3.);
    }
}
