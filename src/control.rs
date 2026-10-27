use crate::{Args, hardware::Hardware, model::Metrics, policy::GrowthMode};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Effective {
    pub gpu: String,
    pub min_free_mib: u64,
    pub grow_below_mib: u64,
    pub grow_delay_seconds: f64,
    pub poll_seconds: f64,
    pub growth_mode: GrowthMode,
    pub step_mib: u64,
    pub accounting_margin_mib: u64,
    pub no_op_retry_seconds: f64,
    pub enabled: bool,
}
impl Effective {
    pub fn new(a: &Args) -> Self {
        Self {
            gpu: a.gpu.clone().unwrap_or_else(|| "0".into()),
            min_free_mib: a.min_free_mib,
            grow_below_mib: a.grow_below_mib,
            grow_delay_seconds: a.grow_delay,
            poll_seconds: a.poll,
            growth_mode: a.growth_mode,
            step_mib: a.step_mib,
            accounting_margin_mib: a.accounting_margin_mib,
            no_op_retry_seconds: a.no_op_retry,
            enabled: !a.no_vram_management,
        }
    }
    pub fn policy(&self) -> crate::policy::Policy {
        let mut p = crate::policy::Policy::new(
            self.min_free_mib,
            self.grow_below_mib,
            self.grow_delay_seconds,
            self.poll_seconds,
        );
        p.growth_mode = self.growth_mode;
        p.step = self.step_mib;
        p.margin = self.accounting_margin_mib;
        p.retry_delay = self.no_op_retry_seconds;
        p
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResizeRecord {
    pub at: f64,
    pub direction: String,
    pub target_free_mib: u64,
    pub before_mib: u64,
    pub after_mib: Option<u64>,
    pub free_mib: Option<u64>,
    pub wait_seconds: f64,
    pub result: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: String,
    pub worker_pid: u32,
    pub config_path: PathBuf,
    pub profile: Option<String>,
    pub port: u16,
    pub model: String,
    pub effective: Effective,
    pub metrics: Option<Metrics>,
    pub hardware: Option<Hardware>,
    pub policy: String,
    pub pending: bool,
    pub growth_paused: bool,
    pub updated_at: f64,
    pub metrics_at: f64,
    pub hardware_at: f64,
    pub resize_history: VecDeque<ResizeRecord>,
    pub memory_history: VecDeque<(u64, u64)>,
}
impl Snapshot {
    pub fn new(args: &Args, port: u16) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            worker_pid: std::process::id(),
            config_path: args.settings_path.clone(),
            profile: args.profile.clone(),
            port,
            model: args.model.clone().unwrap_or_default(),
            effective: Effective::new(args),
            metrics: None,
            hardware: None,
            policy: "Starting".into(),
            pending: false,
            growth_paused: false,
            updated_at: epoch(),
            metrics_at: 0.,
            hardware_at: 0.,
            resize_history: VecDeque::new(),
            memory_history: VecDeque::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", content = "value", rename_all = "kebab-case")]
pub enum Command {
    Status,
    PauseGrowth(bool),
    Resume,
    PreviewReload,
    Reload(Effective),
}
pub struct Request {
    pub command: Command,
    pub reply: SyncSender<Result<Value, String>>,
}

pub fn socket_path(args: &Args) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
        .join("basalt-wrap")
        .join(format!("{}.sock", args.unit_name()))
}

pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
}
impl Server {
    pub fn start(
        args: &Args,
        snapshot: Arc<Mutex<Snapshot>>,
        tx: SyncSender<crate::runtime::Input>,
    ) -> Result<Self> {
        let path = socket_path(args);
        let parent = path.parent().context("socket directory")?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        if let Ok(meta) = fs::symlink_metadata(&path) {
            ensure!(
                meta.file_type().is_socket() && meta.uid() == unsafe { libc::getuid() },
                "refusing to replace foreign status socket"
            );
            ensure!(
                UnixStream::connect(&path).is_err(),
                "worker status socket already active"
            );
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path).context("bind worker status socket")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let task = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                        let response = (|| -> Result<Value> {
                            let mut line = String::new();
                            BufReader::new((&stream).take(65536)).read_line(&mut line)?;
                            let command: Command = serde_json::from_str(&line)?;
                            if matches!(command, Command::Status) {
                                let copy =
                                    snapshot.lock().unwrap_or_else(|p| p.into_inner()).clone();
                                return Ok(serde_json::to_value(copy)?);
                            }
                            let (reply, rx) = mpsc::sync_channel(1);
                            tx.try_send(crate::runtime::Input::Control(Request { command, reply }))
                                .context("worker busy; try again")?;
                            rx.recv_timeout(Duration::from_secs(2))?
                                .map_err(anyhow::Error::msg)
                        })();
                        let reply = match response {
                            Ok(value) => json!({"ok":value}),
                            Err(error) => json!({"error":error.to_string()}),
                        };
                        let _ = writeln!(stream, "{reply}");
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            path,
            stop,
            task: Some(task),
        })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

pub fn call(args: &Args, command: Command) -> Result<Value> {
    let mut stream = UnixStream::connect(socket_path(args))
        .context("connect to worker; it may be stopped or running an older wrapper")?;
    stream.set_read_timeout(Some(Duration::from_secs(4)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    writeln!(stream, "{}", serde_json::to_string(&command)?)?;
    let mut line = String::new();
    BufReader::new(stream.take(8 * 1024 * 1024)).read_line(&mut line)?;
    let response: Value = serde_json::from_str(&line)?;
    if let Some(error) = response["error"].as_str() {
        bail!("{error}");
    }
    Ok(response["ok"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn socket_reports_live_state_controls_and_cleans_up() {
        let temp = tempfile::tempdir().unwrap();
        let mut args = Args::parse_from(["basalt-wrap"]);
        args.unit = format!(
            "basalt-wrap-socket-test-{}",
            temp.path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .replace('.', "-")
        );
        let state = Arc::new(Mutex::new(Snapshot::new(&args, 18099)));
        let (tx, rx) = mpsc::sync_channel(2);
        let server = Server::start(&args, state.clone(), tx).unwrap();
        let path = socket_path(&args);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            call(&args, Command::Status).unwrap()["worker_pid"],
            std::process::id()
        );
        let task = thread::spawn(move || {
            let crate::runtime::Input::Control(r) = rx.recv().unwrap() else {
                panic!("control expected")
            };
            assert!(matches!(r.command, Command::PauseGrowth(true)));
            state.lock().unwrap().growth_paused = true;
            r.reply.send(Ok(json!({"growth_paused":true}))).unwrap();
        });
        assert_eq!(
            call(&args, Command::PauseGrowth(true)).unwrap()["growth_paused"],
            true
        );
        task.join().unwrap();
        assert_eq!(call(&args, Command::Status).unwrap()["growth_paused"], true);
        drop(server);
        assert!(!path.exists());
    }
}
