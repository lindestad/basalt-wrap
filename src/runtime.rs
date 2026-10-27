use crate::{
    Args,
    hardware::Collector,
    model::{Event, Metrics, number, truth},
    policy::{Resize, Sample},
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
        atomic::{AtomicBool, AtomicU64, Ordering},
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
            .join("basalt-wrap");
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
    pub(crate) fn get(&self, path: &str) -> Result<Value> {
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

pub fn api_key() -> String {
    std::env::var("BASALT_WRAP_API_KEY").unwrap_or_default()
}

pub fn server_command(args: &Args, port: u16) -> Command {
    let mut command = Command::new(args.basalt_dir.join("bin/basalt"));
    command
        .args(["serve", "--config"])
        .arg(args.config())
        .args(&args.server_args)
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .current_dir(&args.basalt_dir)
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(gpu) = &args.gpu {
        command.args(["--gpu", gpu]);
    }
    let key = api_key();
    if !key.is_empty() {
        command.args(["--api-key", &key]);
    }
    command
}

pub(crate) enum Input {
    Control(crate::control::Request),
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

fn resize_opportunity(
    policy: &mut crate::policy::Policy,
    now: f64,
    metrics: &Metrics,
    metrics_age: f64,
    hardware: &crate::hardware::Hardware,
    enabled: bool,
) -> Option<Resize> {
    if metrics_age >= 3.
        || !truth(&metrics.status, "/loaded")
        || !truth(&metrics.status, "/vram/elastic")
        || !enabled
        || hardware.gpu_error.is_some()
        || hardware.gpu_total == 0
    {
        policy.interrupt();
        return None;
    }
    policy.observe(
        now,
        Sample {
            used: hardware.gpu_used >> 20,
            free: hardware.gpu_free >> 20,
            cache: metrics.cache(),
            full: metrics.full_cache(),
            activity: metrics.activity(),
            busy: metrics.busy(),
        },
    )
}

pub fn worker(args: &Args) -> Result<i32> {
    let config: Value =
        serde_json::from_slice(&fs::read(args.config()).context("read model config")?)?;
    crate::config::validate_model(args, &config)?;
    let port = crate::config::port(args, &config);
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(runtime.join(format!("basalt-wrap-{port}.lock")))?;
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
            .unwrap_or("basalt"),
    )?);
    log.write(
        &format!(
            "Loading {} on 127.0.0.1:{port}",
            crate::config::model_name(&config)
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
    let api = Api::new(port, &api_key())?;
    let mut command = server_command(args, port);
    let mut server = Server(command.spawn().context("start Basalt")?);
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
    let (tx, rx) = mpsc::sync_channel(64);
    let shared = Arc::new(Mutex::new(crate::control::Snapshot::new(args, port)));
    let _control = crate::control::Server::start(args, shared.clone(), tx.clone())?;
    let sample_interval = Arc::new(AtomicU64::new((args.poll * 1000.) as u64));
    {
        let api = api.clone();
        let tx = tx.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let data = (|| -> Result<Metrics> {
                    Ok(Metrics::new(api.get("/metrics")?, api.get("/v1/status")?))
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
        let interval = sample_interval.clone();
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
                let until =
                    Instant::now() + Duration::from_millis(interval.load(Ordering::Relaxed));
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
    let mut effective = crate::control::Effective::new(args);
    let mut policy = effective.policy();
    let mut resize_started = None::<Instant>;
    let mut metrics: Option<(Metrics, Instant)> = None;
    let mut disabled = args
        .no_vram_management
        .then(|| "VRAM management disabled in settings".to_owned());
    let mut last_state = String::new();
    let mut last_snapshot = Instant::now();
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
                log.write("Launcher exited; stopping Basalt", false);
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
                {
                    let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                    state.metrics = Some(data.clone());
                    state.metrics_at = crate::control::epoch();
                }
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
                {
                    let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                    state.hardware = Some((*hw).clone());
                    state.hardware_at = crate::control::epoch();
                    if hw.gpu_error.is_none() {
                        state.memory_history.push_back((
                            hw.gpu_free >> 20,
                            metrics.as_ref().map_or(0, |(m, _)| m.cache()),
                        ));
                        if state.memory_history.len() > 120 {
                            state.memory_history.pop_front();
                        }
                    }
                }
                if let Some((data, updated)) = &metrics
                    && let Some(action) = resize_opportunity(
                        &mut policy,
                        now,
                        data,
                        updated.elapsed().as_secs_f64(),
                        &hw,
                        disabled.is_none(),
                    )
                {
                    resize_started = Some(Instant::now());
                    {
                        let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                        state
                            .resize_history
                            .push_back(crate::control::ResizeRecord {
                                at: crate::control::epoch(),
                                direction: if action.grow {
                                    "grow".into()
                                } else {
                                    "shrink".into()
                                },
                                target_free_mib: action.reserve,
                                before_mib: action.before,
                                after_mib: None,
                                free_mib: None,
                                wait_seconds: 0.,
                                result: "pending".into(),
                            });
                        if state.resize_history.len() > 100 {
                            state.resize_history.pop_front();
                        }
                    }
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
                emit(Event::Hardware(hw));
            }
            Ok(Input::Resized(reply)) => {
                {
                    let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(record) = state.resize_history.back_mut() {
                        record.wait_seconds = resize_started
                            .take()
                            .map_or(0., |t| t.elapsed().as_secs_f64());
                        match &reply {
                            Ok(value) => {
                                record.after_mib = value["expert_cache_mib"].as_u64();
                                record.free_mib = value["vram_free_mib"].as_u64();
                                record.result = match record.after_mib {
                                    Some(after) if after == record.before_mib => "no-op",
                                    Some(after)
                                        if (after > record.before_mib)
                                            == (record.direction == "grow") =>
                                    {
                                        "applied"
                                    }
                                    Some(_) => "opposite direction: pressure changed",
                                    None => "invalid reply",
                                }
                                .into();
                            }
                            Err(error) => record.result = error.clone(),
                        }
                    }
                }
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
                                effective.no_op_retry_seconds
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
            Ok(Input::Control(request)) => {
                use crate::control::Command as Control;
                let response = (|| -> Result<Value> {
                    match request.command {
                        Control::Status => unreachable!(),
                        Control::PauseGrowth(paused) => {
                            policy.growth_paused = paused;
                            policy.interrupt();
                            Ok(
                                json!({"growth_paused":paused,"pending_resize_unchanged":policy.pending.is_some()}),
                            )
                        }
                        Control::PreviewReload => Ok(
                            json!({"current":effective,"proposed":crate::config::reload_policy(args)?,"scope":"VRAM policy only; GPU, model, port and unit require restart"}),
                        ),
                        command => {
                            anyhow::ensure!(
                                policy.pending.is_none(),
                                "wait for the pending resize to complete"
                            );
                            let state = shared.lock().unwrap_or_else(|p| p.into_inner());
                            anyhow::ensure!(
                                metrics
                                    .as_ref()
                                    .is_some_and(|(m, at)| at.elapsed().as_secs_f64() < 3.
                                        && truth(&m.status, "/loaded")
                                        && truth(&m.status, "/vram/elastic")),
                                "fresh, loaded elastic engine state required"
                            );
                            anyhow::ensure!(
                                crate::control::epoch() - state.hardware_at
                                    < effective.poll_seconds * 3. + 1.
                                    && state
                                        .hardware
                                        .as_ref()
                                        .is_some_and(
                                            |hw| hw.gpu_error.is_none() && hw.gpu_total > 0
                                        ),
                                "fresh GPU state required"
                            );
                            drop(state);
                            if let Control::Reload(proposed) = command {
                                let fresh = crate::config::reload_policy(args)?;
                                anyhow::ensure!(
                                    fresh == proposed,
                                    "config changed since preview; preview again"
                                );
                                effective = fresh;
                                sample_interval.store(
                                    (effective.poll_seconds * 1000.) as u64,
                                    Ordering::Relaxed,
                                );
                            } else {
                                anyhow::ensure!(
                                    effective.enabled,
                                    "management is disabled in config; enable it and reload"
                                );
                            }
                            let paused = policy.growth_paused;
                            policy = effective.policy();
                            policy.growth_paused = paused;
                            disabled = (!effective.enabled)
                                .then(|| "VRAM management disabled in settings".into());
                            log.write("VRAM policy reconciled; growth timer restarted", false);
                            Ok(json!({"effective":effective}))
                        }
                    }
                })();
                let _ = request.reply.send(response.map_err(|e| e.to_string()));
                next_policy = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= next_policy {
            let state = if let Some(reason) = &disabled {
                reason.clone()
            } else if policy.pending.is_some() {
                policy.status(now)
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
            {
                let mut snapshot = shared.lock().unwrap_or_else(|p| p.into_inner());
                snapshot.policy = state.clone();
                snapshot.pending = policy.pending.is_some();
                snapshot.growth_paused = policy.growth_paused;
                snapshot.effective = effective.clone();
                if let Some(started) = resize_started
                    && let Some(record) = snapshot.resize_history.back_mut()
                {
                    record.wait_seconds = started.elapsed().as_secs_f64();
                }
                snapshot.updated_at = crate::control::epoch();
            }
            if state != last_state || policy.quiet_left(now).is_some() {
                emit(Event::Policy {
                    state: state.clone(),
                    quiet_left: policy.quiet_left(now),
                    pending: policy.pending.is_some(),
                });

                last_state = state;
            }
            if last_snapshot.elapsed().as_secs_f64() >= 1. {
                emit(Event::Supervisor(Box::new(
                    shared.lock().unwrap_or_else(|p| p.into_inner()).clone(),
                )));
                last_snapshot = Instant::now();
            }
            next_policy = Instant::now() + Duration::from_millis(500);
        }
    }
    stop.store(true, Ordering::Relaxed);
    log.write("Stopping Basalt", false);
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
    let value: Value = serde_json::from_slice(
        &fs::read(args.config()).with_context(|| format!("read {}", args.config().display()))?,
    )?;
    anyhow::ensure!(value.is_object(), "model config must be a JSON object");
    Ok(value)
}

pub fn diagnose(args: &Args) -> Result<i32> {
    if crate::control::socket_path(args).exists() {
        let snapshot: crate::control::Snapshot =
            serde_json::from_value(crate::control::call(args, crate::control::Command::Status)?)?;
        let engine = snapshot.metrics.as_ref().map(
            |m| json!({"busy":m.busy(),"cache_mib":m.cache(),"full_cache_mib":m.full_cache()}),
        );
        let gpu=snapshot.hardware.as_ref().map(|h|json!({"name":h.gpu_name,"total_mib":h.gpu_total>>20,"free_mib":h.gpu_free>>20,"used_mib":h.gpu_used>>20,"error":h.gpu_error}));
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"source":"running-worker","read_only":true,
            "version":snapshot.version,"worker_pid":snapshot.worker_pid,"config_path":snapshot.config_path,"profile":snapshot.profile,
            "effective":snapshot.effective,"policy":snapshot.policy,"pending":snapshot.pending,"growth_paused":snapshot.growth_paused,
            "metrics_age_seconds":crate::control::epoch()-snapshot.metrics_at,"hardware_age_seconds":crate::control::epoch()-snapshot.hardware_at,
            "engine":engine,"gpu":gpu,"resize_history":snapshot.resize_history}))?
        );
        return Ok(0);
    }
    let config = read_config(args)?;
    let port = crate::config::port(args, &config);
    let api = Api::new(port, &api_key())?;
    let m = Metrics::new(api.get("/metrics")?, api.get("/v1/status")?);
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
            "source":"server-and-config","worker_state_available":false,"read_only":true,"gpu":{"index":index,"name":gpu.name,"uuid":gpu.uuid,
                "total_mib":gpu.total_mib,"used_mib":gpu.used_mib,"free_mib":gpu.free_mib,"driver_reserved_mib":gpu.reserved_mib},
            "policy":{"growth_mode":args.growth_mode,"min_free_mib":args.min_free_mib,"grow_below_mib":args.grow_below_mib,
                "grow_delay_seconds":args.grow_delay,"step_mib":args.step_mib,"accounting_margin_mib":args.accounting_margin_mib},
            "engine":{"busy":m.busy(),"cache_mib":m.cache(),"full_cache_mib":m.full_cache(),"activity":m.activity()},
            "growth_blockers":reasons,
            "timing_note":"This snapshot cannot read the running worker's timer. Capacity mode waits for sustained spare VRAM; idle mode also waits for inference to stop. Basalt applies queued resizes between requests."
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
        if key.starts_with("BASALT_")
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
        .arg("--basalt-dir")
        .arg(std::path::absolute(&args.basalt_dir)?)
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
    if let Some(profile) = &args.profile {
        cmd.args(["--profile", profile]);
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
    fn launches_basalt_serve_with_the_selected_config_and_loopback() {
        use clap::Parser;
        let args = Args::parse_from([
            "basalt-wrap",
            "/tmp/model.json",
            "--basalt-dir",
            "/tmp/basalt",
            "--gpu",
            "2",
            "--",
            "--idle-unload",
            "120",
        ]);
        let command = server_command(&args, 18080);
        assert_eq!(command.get_program(), "/tmp/basalt/bin/basalt");
        let argv: Vec<_> = command.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            &argv[..11],
            [
                "serve",
                "--config",
                "/tmp/model.json",
                "--idle-unload",
                "120",
                "--host",
                "127.0.0.1",
                "--port",
                "18080",
                "--gpu",
                "2"
            ]
            .as_slice()
        );
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
            "basalt-wrap",
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
                std::iter::once(std::ffi::OsString::from("basalt-wrap"))
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
    #[test]
    fn worker_resize_path_handles_busy_inference_staleness_and_pressure_while_pending() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let task = thread::spawn(move || {
            for (i, mut request) in server.incoming_requests().take(4).enumerate() {
                let reply = match i {
                    0 => {
                        assert_eq!(request.url(), "/metrics");
                        json!({"live":{"state":"generating"},"requests":(0..40).map(|i|json!({"time":i})).collect::<Vec<_>>()})
                    }
                    1 => {
                        assert_eq!(request.url(), "/v1/status");
                        json!({"loaded":true,"activity":{"in_flight":2,"requests":1},"vram":{"elastic":true,"expert_cache_mib":4096,"expert_cache_full_mib":8192}})
                    }
                    _ => {
                        assert_eq!(request.url(), "/v1/vram");
                        let mut body = String::new();
                        request.as_reader().read_to_string(&mut body).unwrap();
                        let value: Value = serde_json::from_str(&body).unwrap();
                        assert_eq!(value["reserve_mib"], if i == 2 { 2472 } else { 1028 });
                        json!({"status":"ok","expert_cache_mib":if i==2 {4608}else{4096},"vram_free_mib":if i==2 {2488}else{1028}})
                    }
                };
                request
                    .respond(tiny_http::Response::from_string(reply.to_string()))
                    .unwrap();
            }
        });
        let api = Api::new(port, "").unwrap();
        let mut m = Metrics::new(api.get("/metrics").unwrap(), api.get("/v1/status").unwrap());
        assert_eq!(m.data["requests"].as_array().unwrap().len(), 40);
        let mut hw = crate::hardware::Hardware {
            gpu_total: 16384 << 20,
            gpu_used: 13384 << 20,
            gpu_free: 3000 << 20,
            ..Default::default()
        };
        let mut policy = crate::policy::Policy::new(1024, 14336, 6., 2.);
        policy.growth_mode = crate::policy::GrowthMode::Capacity;
        for (time, age) in [(0., 0.), (2., 0.), (4., 4.), (6., 0.), (8., 0.), (10., 0.)] {
            assert!(resize_opportunity(&mut policy, time, &m, age, &hw, true).is_none());
        }
        let grow = resize_opportunity(&mut policy, 12., &m, 0., &hw, true).unwrap();
        assert!(grow.grow);
        hw.gpu_free = 500 << 20;
        hw.gpu_used = hw.gpu_total - hw.gpu_free;
        assert!(resize_opportunity(&mut policy, 14., &m, 0., &hw, true).is_none());
        let reply = api.resize(&grow).unwrap();
        assert!(policy.complete(15., reply["expert_cache_mib"].as_u64()));
        m.status["vram"]["expert_cache_mib"] = json!(4608);
        assert!(resize_opportunity(&mut policy, 16., &m, 0., &hw, true).is_none());
        let shrink = resize_opportunity(&mut policy, 18., &m, 0., &hw, true).unwrap();
        assert!(!shrink.grow);
        let reply = api.resize(&shrink).unwrap();
        assert!(policy.complete(19., reply["expert_cache_mib"].as_u64()));
        task.join().unwrap();
    }
}
