use crate::{Args, config, control::Effective, hardware, runtime};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, process::Command};

pub fn inspect(args: &Args) -> Value {
    let mut checks = Vec::new();
    let mut check = |name: &str, result: Result<String>| match result {
        Ok(detail) => checks.push(json!({"check":name,"ok":true,"detail":detail})),
        Err(error) => checks.push(json!({"check":name,"ok":false,"detail":format!("{error:#}")})),
    };
    check(
        "systemd user manager",
        (|| -> Result<String> {
            ensure!(
                Command::new("systemctl")
                    .args(["--user", "show-environment"])
                    .output()?
                    .status
                    .success(),
                "systemd user manager unavailable"
            );
            Ok("reachable".into())
        })(),
    );
    check(
        "Basalt server",
        (|| -> Result<String> {
            ensure!(
                args.basalt_dir.join("serve/server.py").is_file(),
                "missing serve/server.py in {}",
                args.basalt_dir.display()
            );
            Ok(args.basalt_dir.display().to_string())
        })(),
    );
    check(
        "Python",
        (|| -> Result<String> {
            let path = args.basalt_dir.join(".venv/bin/python");
            let metadata = path.metadata().context("missing .venv/bin/python")?;
            ensure!(
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
                "Python is not executable"
            );
            Ok(path.display().to_string())
        })(),
    );
    let mut resolved = args.clone();
    check(
        "model and policy",
        (|| -> Result<String> {
            ensure!(
                args.model.is_some(),
                "choose a model JSON, built-in name, or --profile"
            );
            let model = runtime::read_config(args)?;
            config::resolve_model(&mut resolved, &model)?;
            config::validate_model(&resolved, &model)?;
            Ok("model JSON and resize segment size are compatible".into())
        })(),
    );
    check(
        "native engine",
        (|| -> Result<String> {
            let exe = args.basalt_dir.join("bin/basalt");
            ensure!(
                exe.is_file()
                    && exe
                        .metadata()
                        .context("native engine missing")?
                        .permissions()
                        .mode()
                        & 0o111
                        != 0,
                "native engine is not executable"
            );
            Ok(exe.display().to_string())
        })(),
    );
    check(
        "model file",
        (|| -> Result<String> {
            let model = runtime::read_config(args)?;
            let path = config::model_path(args, &model)?;
            ensure!(path.is_file(), "model file missing: {}", path.display());
            Ok(path.display().to_string())
        })(),
    );
    check(
        "server port",
        (|| -> Result<String> {
            let model = runtime::read_config(args)?;
            let port = config::port(args, &model);
            let busy = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                std::time::Duration::from_millis(150),
            )
            .is_ok();
            if busy {
                Ok(format!(
                    "{port} already in use; use --attach or stop the existing server before launching"
                ))
            } else {
                Ok(format!("{port} not currently listening"))
            }
        })(),
    );
    check(
        "NVIDIA GPU and thresholds",
        (|| -> Result<String> {
            let devices = hardware::gpus()?;
            let selector = resolved.gpu.as_deref().unwrap_or("0");
            let gpu = devices
                .iter()
                .find(|g| g.index.to_string() == selector || g.uuid == selector)
                .context("selected GPU not found")?;
            config::thresholds(
                gpu.total_mib,
                (!resolved.auto_min_free).then_some(resolved.min_free_mib),
                (!resolved.auto_grow_below).then_some(resolved.grow_below_mib),
            )?;
            Ok(format!("{}: {} MiB measured", gpu.name, gpu.total_mib))
        })(),
    );
    let ok = checks.iter().all(|c| c["ok"] == true);
    json!({"ok":ok,"read_only":true,"checks":checks,"effective":Effective::new(&resolved),"config_path":args.settings_path,"profile":args.profile})
}
pub fn run(args: &Args) -> Result<i32> {
    let report = inspect(args);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(if report["ok"] == true { 0 } else { 1 })
}

pub fn setup(args: &Args) -> Result<i32> {
    ensure!(
        args.profile.is_none(),
        "setup creates a global config; add named profiles afterward"
    );
    ensure!(
        args.model.is_some(),
        "choose a model JSON or built-in name for --setup"
    );
    let mut selected = args.clone();
    let model = runtime::read_config(args)?;
    config::resolve_model(&mut selected, &model)?;
    let gpu_index = selected.gpu.as_deref().unwrap_or("0");
    let devices = hardware::gpus()?;
    let device = devices
        .iter()
        .find(|g| g.index.to_string() == gpu_index)
        .context("GPU not found")?;
    let settings = config::Settings {
        basalt_dir: Some(std::path::absolute(&args.basalt_dir)?),
        model: Some(
            if matches!(args.model.as_deref(), Some("iq3_s" | "ud-iq3_s")) {
                args.model.clone().unwrap()
            } else {
                args.config().display().to_string()
            },
        ),
        gpu: Some(config::Gpu::Uuid(device.uuid.clone())),
        theme: args.theme,
        age_seconds: args.age_seconds,
        port: args.port,
        unit: args.unit.clone(),
        vram: config::Vram {
            growth_mode: args.growth_mode,
            enabled: !args.no_vram_management,
            min_free_mib: (!args.auto_min_free).then_some(args.min_free_mib),
            grow_below_mib: (!args.auto_grow_below).then_some(args.grow_below_mib),
            grow_delay_seconds: args.grow_delay,
            poll_seconds: args.poll,
            step_mib: args.step_mib,
            accounting_margin_mib: args.accounting_margin_mib,
            no_op_retry_seconds: args.no_op_retry,
        },
        ..Default::default()
    };
    config::initialize_with(&args.settings_path, &settings)?;
    println!("Run --doctor to check model elastic mode and resize compatibility before launching.");
    Ok(0)
}
