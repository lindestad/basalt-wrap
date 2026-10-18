//! Real user-manager tests. The fake engine deliberately escapes its parent's
//! process group and ignores SIGTERM, just like a stubborn native subprocess.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn live(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        s.rsplit_once(')')
            .is_some_and(|(_, tail)| !tail.trim_start().starts_with('Z'))
    })
}

struct Fixture {
    temp: TempDir,
    unit: String,
    child: Child,
    pids: Vec<u32>,
}
impl Fixture {
    fn new(label: &str) -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".venv/bin")).unwrap();
        fs::create_dir(root.join("serve")).unwrap();
        let script = format!(
            "#!/bin/sh\nexec '{}' --ignored --exact fixture_process --nocapture\n",
            std::env::current_exe().unwrap().display()
        );
        fs::write(root.join(".venv/bin/python"), script).unwrap();
        fs::set_permissions(
            root.join(".venv/bin/python"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(
            root.join("strata-iq3_s.json"),
            json!({"vram_elastic":true,"port":18089}).to_string(),
        )
        .unwrap();
        let unit = format!("strata-wrap-rust-test-{}-{label}", std::process::id());
        let output = fs::File::create(root.join("output.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_strata-wrap"))
            .arg("iq3_s")
            .arg("--strata-dir")
            .arg(root)
            .args(["--unit", &unit, "--plain", "--no-config"])
            .env("STRATA_WRAP_FIXTURE_ROOT", root)
            .env("XDG_STATE_HOME", root.join("state"))
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .process_group(0)
            .spawn()
            .unwrap();
        let mut fixture = Self {
            temp,
            unit,
            child,
            pids: vec![],
        };
        let until = Instant::now() + Duration::from_secs(12);
        while Instant::now() < until {
            if let Ok(content) = fs::read_to_string(fixture.temp.path().join("pids.json"))
                && let Ok(pids) = serde_json::from_str(&content)
            {
                fixture.pids = pids;
                return fixture;
            }
            if let Some(status) = fixture.child.try_wait().unwrap() {
                panic!("launcher exited {status}: {}", fixture.output());
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("fake server did not start: {}", fixture.output());
    }
    fn output(&self) -> String {
        fs::read_to_string(self.temp.path().join("output.log")).unwrap_or_default()
    }
    fn signal(&self, signal: i32) {
        unsafe {
            libc::kill(self.child.id() as i32, signal);
        }
    }
    fn ctl(&self, args: &[&str]) {
        let output = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .arg(&self.unit)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn clean(&mut self) {
        let until = Instant::now() + Duration::from_secs(25);
        while Instant::now() < until {
            if self.child.try_wait().unwrap().is_some() && !self.pids.iter().any(|pid| live(*pid)) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("surviving processes {:?}: {}", self.pids, self.output());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &self.unit])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// A fixed port keeps failures easy to inspect. All process tests are combined
// into one test to prevent the test harness from running them concurrently.
#[test]
fn lifecycle_cleanup_and_console() {
    for (label, signal) in [
        ("interrupt", libc::SIGINT),
        ("hangup", libc::SIGHUP),
        ("terminate", libc::SIGTERM),
        ("launcher-kill", libc::SIGKILL),
    ] {
        let mut f = Fixture::new(label);
        f.signal(signal);
        f.clean();
    }
    for (label, args) in [
        (
            "worker-kill",
            vec!["kill", "--kill-whom=main", "--signal=SIGKILL"],
        ),
        ("service-stop", vec!["stop"]),
    ] {
        let mut f = Fixture::new(label);
        f.ctl(&args);
        f.clean();
    }
    {
        let mut f = Fixture::new("server-crash");
        unsafe {
            libc::kill(f.pids[0] as i32, libc::SIGKILL);
        };
        f.clean();
        assert!(!f.child.wait().unwrap().success());
    }
    {
        let mut f = Fixture::new("duplicate");
        let duplicate = Command::new(env!("CARGO_BIN_EXE_strata-wrap"))
            .args(["iq3_s", "--strata-dir"])
            .arg(f.temp.path())
            .args(["--unit", &f.unit, "--plain", "--no-config"])
            .output()
            .unwrap();
        assert!(!duplicate.status.success());
        assert!(f.pids.iter().all(|pid| live(*pid)));
        f.signal(libc::SIGINT);
        f.clean();
    }
}

#[test]
#[ignore = "child fixture launched only by lifecycle tests"]
fn fixture_process() {
    let Some(root) = std::env::var_os("STRATA_WRAP_FIXTURE_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let mut command = Command::new("sh");
    command.args(["-c", "trap '' TERM; exec sleep 600"]);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    fs::write(
        root.join("pids.json"),
        json!([std::process::id(), child.id()]).to_string(),
    )
    .unwrap();
    let server = tiny_http::Server::http("127.0.0.1:18089").unwrap();
    for request in server.incoming_requests() {
        let data = if request.url() == "/v1/status" {
            json!({"service":"strata","loaded":true,"started":1,"activity":{"requests":0,"in_flight":0},"vram":{"elastic":true}})
        } else {
            json!({"engine":{"model":"fake","expert_cache_mib":100},"live":{"state":"idle"},"requests":[],"totals":{}})
        };
        let _ = request.respond(
            tiny_http::Response::from_string(data.to_string()).with_header(
                tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
            ),
        );
    }
    let _ = child.wait();
}

#[test]
fn installer_preserves_local_settings() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    fs::create_dir_all(home.join("apps/Strata")).unwrap();
    fs::create_dir_all(home.join(".config/fish")).unwrap();
    for model in ["iq3_s", "ud-iq3_s"] {
        fs::write(home.join(format!("apps/Strata/strata-{model}.json")),json!({"api_key":"test-only","pricing":{"prompt":0.1},"args":["--max-context","196608"]}).to_string()).unwrap();
    }
    fs::write(
        home.join(".config/fish/.config.local.fish"),
        "# local\nabbr --add sa 'old'\nabbr --add saud 'old-ud'\nset keep untouched\n",
    )
    .unwrap();
    let exe = env!("CARGO_BIN_EXE_strata-wrap");
    let status = Command::new(exe)
        .arg("install")
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    for model in ["iq3_s", "ud-iq3_s"] {
        let data: Value = serde_json::from_slice(
            &fs::read(home.join(format!("apps/Strata/strata-{model}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(data["api_key"], "test-only");
        assert_eq!(data["pricing"]["prompt"], 0.1);
        assert_eq!(data["vram_elastic"], true);
        assert_eq!(data["vram_segment_mib"], 512);
    }
    let fish = fs::read_to_string(home.join(".config/fish/.config.local.fish")).unwrap();
    assert!(fish.contains("set keep untouched"));
    assert!(fish.contains("strata-wrap ud-iq3_s"));
    assert!(home.join(".local/bin/strata-wrap").is_symlink());
    assert_eq!(
        fs::read_link(home.join(".local/bin/strata-wrap")).unwrap(),
        Path::new(exe)
    );
    let output = Command::new(home.join(".local/bin/strata-wrap"))
        .arg("--list-themes")
        .output()
        .unwrap();
    assert!(output.status.success());
}
