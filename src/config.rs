use crate::{Args, hardware, policy::GrowthMode, theme::Theme};
use anyhow::{Context, Result, bail, ensure};
use clap::{ArgMatches, parser::ValueSource};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Gpu {
    Index(u32),
    Uuid(String),
}
impl std::fmt::Display for Gpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Index(i) => write!(f, "{i}"),
            Self::Uuid(s) => f.write_str(s),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    pub age_seconds: bool,
    pub profiles: std::collections::BTreeMap<String, serde_json::Value>,
    pub basalt_dir: Option<PathBuf>,
    pub model: Option<String>,
    pub port: Option<u16>,
    pub unit: String,
    pub gpu: Option<Gpu>,
    pub theme: Option<Theme>,
    pub vram: Vram,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            age_seconds: false,
            profiles: Default::default(),
            basalt_dir: None,
            model: None,
            port: None,
            unit: "basalt-wrap".into(),
            gpu: None,
            theme: None,
            vram: Vram::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Vram {
    pub enabled: bool,
    pub min_free_mib: Option<u64>,
    pub grow_below_mib: Option<u64>,
    pub grow_delay_seconds: f64,
    pub poll_seconds: f64,
    pub growth_mode: GrowthMode,
    pub step_mib: u64,
    pub accounting_margin_mib: u64,
    pub no_op_retry_seconds: f64,
}
impl Default for Vram {
    fn default() -> Self {
        Self {
            enabled: true,
            min_free_mib: None,
            grow_below_mib: None,
            grow_delay_seconds: 60.,
            poll_seconds: 2.,
            growth_mode: GrowthMode::Idle,
            step_mib: 512,
            accounting_margin_mib: 16,
            no_op_retry_seconds: 60.,
        }
    }
}

pub fn default_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| Path::new(p).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::home().join(".config"))
        .join("basalt-wrap/config.json")
}

pub fn load(path: &Path, required: bool) -> Result<Settings> {
    load_selected(path, required, None)
}

fn merge(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    if let (Some(base), Some(overlay)) = (base.as_object_mut(), overlay.as_object()) {
        for (key, value) in overlay {
            if key == "vram" {
                merge(
                    base.entry(key).or_insert_with(|| serde_json::json!({})),
                    value,
                );
            } else {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

pub fn load_selected(path: &Path, required: bool, profile: Option<&str>) -> Result<Settings> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut raw: serde_json::Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse wrapper config {}", path.display()))?;
            if let Some(profiles) = raw.get("profiles").and_then(serde_json::Value::as_object) {
                for (name, value) in profiles {
                    ensure!(
                        value.is_object() && value.get("profiles").is_none(),
                        "profile {name} must be an object without nested profiles"
                    );
                    let profile: Settings = serde_json::from_value(value.clone())
                        .with_context(|| format!("profile {name}"))?;
                    validate(&profile).with_context(|| format!("profile {name}"))?;
                }
            }
            if let Some(name) = profile {
                let overlay = raw
                    .pointer("/profiles")
                    .and_then(|p| p.get(name))
                    .with_context(|| format!("unknown profile {name}"))?
                    .clone();
                merge(&mut raw, &overlay);
            }
            let settings: Settings = serde_json::from_value(raw)
                .with_context(|| format!("parse wrapper config {}", path.display()))?;
            validate(&settings)?;
            Ok(settings)
        }
        Err(e) if !required && profile.is_none() && e.kind() == std::io::ErrorKind::NotFound => {
            Ok(Settings::default())
        }
        Err(e) => Err(e).with_context(|| format!("read wrapper config {}", path.display())),
    }
}

fn validate(settings: &Settings) -> Result<()> {
    ensure!(
        settings.model.as_deref() != Some("install"),
        "the default model cannot be the install command"
    );
    ensure!(
        settings.schema_version == 1,
        "unsupported wrapper config schema_version {}",
        settings.schema_version
    );
    ensure!(
        settings.vram.min_free_mib != Some(0) && settings.vram.grow_below_mib != Some(0),
        "VRAM thresholds must be positive or null for automatic sizing"
    );
    let v = &settings.vram;
    ensure!(
        v.grow_delay_seconds.is_finite()
            && (0.0..=86400.).contains(&v.grow_delay_seconds)
            && v.poll_seconds.is_finite()
            && (0.2..=60.).contains(&v.poll_seconds)
            && (64..=65536).contains(&v.step_mib)
            && v.accounting_margin_mib <= 4096
            && v.no_op_retry_seconds.is_finite()
            && (1.0..=86400.).contains(&v.no_op_retry_seconds),
        "invalid VRAM thresholds or polling interval in wrapper config"
    );
    ensure!(settings.port != Some(0), "port must be between 1 and 65535");
    ensure!(
        !settings.unit.is_empty()
            && settings
                .unit
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
        "invalid systemd unit name in wrapper config"
    );
    if let Some(gpu) = &settings.gpu {
        let selector = gpu.to_string();
        ensure!(
            selector.parse::<u32>().is_ok()
                || (selector.starts_with("GPU-") && !selector.chars().any(char::is_whitespace)),
            "GPU must be an NVIDIA index or GPU UUID"
        );
    }
    Ok(())
}

pub fn initialize(path: &Path) -> Result<()> {
    initialize_with(path, &Settings::default())
}

pub fn initialize_with(path: &Path, settings: &Settings) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().context("config needs a directory")?)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| {
            format!(
                "create {} (existing files are never overwritten)",
                path.display()
            )
        })?;
    writeln!(file, "{}", serde_json::to_string_pretty(settings)?)?;
    println!("Created {}", path.display());
    Ok(())
}

fn configured_path(path: &Path, parent: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        crate::home().join(rest)
    } else if path.is_absolute() {
        path.to_owned()
    } else {
        parent.join(path)
    }
}

pub fn apply(args: &mut Args, matches: &ArgMatches) -> Result<()> {
    let path = args.wrapper_config.clone().unwrap_or_else(default_path);
    args.settings_path = path.clone();
    args.auto_min_free = matches.value_source("min_free_mib") != Some(ValueSource::CommandLine);
    args.auto_grow_below = matches.value_source("grow_below_mib") != Some(ValueSource::CommandLine);
    if args.worker || args.no_config || args.init_config || args.setup {
        return Ok(());
    }
    let settings = load_selected(
        &path,
        args.wrapper_config.is_some(),
        args.profile.as_deref(),
    )?;
    let from_cli = |name: &str| matches.value_source(name) == Some(ValueSource::CommandLine);
    let parent = path.parent().unwrap_or(Path::new("."));
    if !from_cli("basalt_dir")
        && let Some(p) = &settings.basalt_dir
    {
        args.basalt_dir = configured_path(p, parent);
    }
    if args.model.is_none()
        && let Some(model) = &settings.model
    {
        args.model = Some(if matches!(model.as_str(), "iq3_s" | "ud-iq3_s") {
            model.clone()
        } else {
            configured_path(Path::new(model), parent)
                .to_string_lossy()
                .into_owned()
        });
    }
    if args.port.is_none() {
        args.port = settings.port;
    }
    if !from_cli("age_seconds") {
        args.age_seconds = settings.age_seconds;
    }
    if !from_cli("unit") {
        args.unit = settings.unit;
    }
    if args.gpu.is_none() {
        args.gpu = settings.gpu.map(|g| g.to_string());
    }
    if args.theme.is_none() {
        args.theme = settings.theme;
    }
    args.auto_min_free = !from_cli("min_free_mib") && settings.vram.min_free_mib.is_none();
    args.auto_grow_below = !from_cli("grow_below_mib") && settings.vram.grow_below_mib.is_none();
    if !from_cli("min_free_mib")
        && let Some(v) = settings.vram.min_free_mib
    {
        args.min_free_mib = v;
    }
    if !from_cli("grow_below_mib")
        && let Some(v) = settings.vram.grow_below_mib
    {
        args.grow_below_mib = v;
    }
    if !from_cli("grow_delay") {
        args.grow_delay = settings.vram.grow_delay_seconds;
    }
    if !from_cli("poll") {
        args.poll = settings.vram.poll_seconds;
    }
    if !from_cli("growth_mode") {
        args.growth_mode = settings.vram.growth_mode;
    }
    if !from_cli("step_mib") {
        args.step_mib = settings.vram.step_mib;
    }
    if !from_cli("accounting_margin_mib") {
        args.accounting_margin_mib = settings.vram.accounting_margin_mib;
    }
    if !from_cli("no_op_retry") {
        args.no_op_retry = settings.vram.no_op_retry_seconds;
    }
    args.no_vram_management |= !settings.vram.enabled;
    Ok(())
}

pub fn thresholds(
    total_mib: u64,
    min_free: Option<u64>,
    grow_below: Option<u64>,
) -> Result<(u64, u64)> {
    let reserve = min_free.unwrap_or((total_mib / 16).max(1024));
    let ceiling =
        grow_below.unwrap_or_else(|| total_mib.saturating_sub(reserve.saturating_add(1024)));
    ensure!(
        reserve < total_mib && ceiling > 0 && ceiling < total_mib,
        "VRAM thresholds must fit the selected GPU ({total_mib} MiB)"
    );
    ensure!(
        ceiling <= total_mib - reserve,
        "grow_below_mib must leave at least min_free_mib free"
    );
    Ok((reserve, ceiling))
}

pub fn resolve_model(args: &mut Args, model: &serde_json::Value) -> Result<()> {
    let selector = if let Some(gpu) = &args.gpu {
        gpu.clone()
    } else {
        match option(model, "server", "gpu") {
            None | Some(serde_json::Value::Null) => "0".to_owned(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            Some(serde_json::Value::String(s)) if !s.contains(',') => s.clone(),
            _ => bail!(
                "elastic VRAM management supports one NVIDIA GPU; choose a single GPU in the model config"
            ),
        }
    };
    match hardware::gpus() {
        Ok(devices) => {
            let device = devices
                .iter()
                .find(|d| d.index.to_string() == selector || d.uuid == selector)
                .with_context(|| format!("NVIDIA GPU {selector} not found; use --list-gpus"))?;
            args.gpu = Some(device.index.to_string());
            let (reserve, ceiling) = thresholds(
                device.total_mib,
                (!args.auto_min_free).then_some(args.min_free_mib),
                (!args.auto_grow_below).then_some(args.grow_below_mib),
            )?;
            args.min_free_mib = reserve;
            args.grow_below_mib = ceiling;
        }
        Err(_) if selector.parse::<u32>().is_ok() => args.gpu = Some(selector),
        Err(e) => return Err(e).context("resolving GPU UUID requires NVML"),
    }
    Ok(())
}

pub fn option<'a>(
    model: &'a serde_json::Value,
    section: &str,
    name: &str,
) -> Option<&'a serde_json::Value> {
    model
        .get(section)?
        .as_object()?
        .iter()
        .rev()
        .find_map(|(key, value)| {
            (key.trim()
                .to_lowercase()
                .replace('_', "-")
                .trim_start_matches("--")
                == name)
                .then_some(value)
        })
}

pub fn port(args: &Args, model: &serde_json::Value) -> u16 {
    args.port.unwrap_or_else(|| {
        option(model, "server", "port")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(8080) as u16
    })
}

pub fn model_name(model: &serde_json::Value) -> &str {
    option(model, "server", "alias")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Basalt")
}

pub fn model_path(args: &Args, model: &serde_json::Value) -> Result<PathBuf> {
    let path = option(model, "server", "model")
        .and_then(serde_json::Value::as_str)
        .context("server.model must name a .basalt file")?;
    Ok(configured_path(
        Path::new(path),
        args.config().parent().context("config needs a directory")?,
    ))
}

pub fn validate_model(args: &Args, model: &serde_json::Value) -> Result<()> {
    let object = model
        .as_object()
        .context("model config must be a JSON object")?;
    ensure!(
        object
            .keys()
            .all(|k| matches!(k.as_str(), "server" | "engine")),
        "Basalt model config must contain only server and engine blocks"
    );
    for section in ["server", "engine"] {
        if let Some(value) = model.get(section) {
            ensure!(value.is_object(), "{section} must be an object");
            ensure!(
                value.as_object().unwrap().keys().all(|k| !k
                    .chars()
                    .filter(|c| c.is_ascii_alphabetic())
                    .collect::<String>()
                    .to_lowercase()
                    .starts_with("apikey")),
                "Basalt does not accept api-key in JSON; use BASALT_WRAP_API_KEY"
            );
        }
    }
    if let Some(port) = option(model, "server", "port") {
        ensure!(
            port.as_u64().is_some_and(|p| (1..=65535).contains(&p)),
            "model port must be an integer between 1 and 65535"
        );
    }
    let selector = option(model, "server", "gpu");
    ensure!(
        args.gpu.is_some()
            || selector
                .is_none_or(|v| v.as_u64().is_some()
                    || v.as_str().is_some_and(|s| s.parse::<u32>().is_ok())),
        "the wrapper supports one NVIDIA GPU; set server.gpu to a single index"
    );
    if !args.no_vram_management {
        ensure!(
            option(model, "engine", "vram-elastic") == Some(&serde_json::Value::Bool(true)),
            "enable engine.vram-elastic in {} (or use --no-vram-management)",
            args.config().display()
        );
        let segment = option(model, "engine", "vram-segment-mib")
            .map(|v| {
                v.as_u64()
                    .context("engine.vram-segment-mib must be an integer")
            })
            .transpose()?
            .unwrap_or(512);
        ensure!(
            segment >= 64 && args.step_mib.is_multiple_of(segment),
            "step_mib must be a multiple of engine.vram-segment-mib ({segment})"
        );
    }
    Ok(())
}

pub fn reload_policy(args: &Args) -> Result<crate::control::Effective> {
    ensure!(!args.no_config, "reload unavailable with --no-config");
    let v = load_selected(&args.settings_path, true, args.profile.as_deref())?.vram;
    let mut updated = args.clone();
    updated.auto_min_free = v.min_free_mib.is_none();
    updated.auto_grow_below = v.grow_below_mib.is_none();
    updated.min_free_mib = v.min_free_mib.unwrap_or(1024);
    updated.grow_below_mib = v.grow_below_mib.unwrap_or(14336);
    updated.grow_delay = v.grow_delay_seconds;
    updated.poll = v.poll_seconds;
    updated.growth_mode = v.growth_mode;
    updated.step_mib = v.step_mib;
    updated.accounting_margin_mib = v.accounting_margin_mib;
    updated.no_op_retry = v.no_op_retry_seconds;
    updated.no_vram_management = !v.enabled;
    let model = crate::runtime::read_config(args)?;
    resolve_model(&mut updated, &model)?;
    validate_model(&updated, &model)?;
    updated.validate()?;
    Ok(crate::control::Effective::new(&updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, FromArgMatches};
    #[test]
    fn basalt_config_reads_nested_options_and_checks_elastic_segments() {
        use clap::Parser;
        let args = Args::parse_from(["basalt-wrap", "/tmp/config/model.json"]);
        let model = serde_json::json!({"server":{"model":"weights.basalt","port":18088,"alias":"test","gpu":"0"},"engine":{"vram_elastic":true,"vram_segment_mib":256}});
        validate_model(&args, &model).unwrap();
        assert_eq!(port(&args, &model), 18088);
        assert_eq!(model_name(&model), "test");
        assert_eq!(
            model_path(&args, &model).unwrap(),
            PathBuf::from("/tmp/config/weights.basalt")
        );
        let mut args = args;
        args.port = Some(19000);
        assert_eq!(port(&args, &model), 19000);
        for invalid in [
            serde_json::json!({"port":8080,"vram_elastic":true}),
            serde_json::json!({"server":{"port":65536},"engine":{"vram-elastic":true}}),
            serde_json::json!({"server":{"gpu":"0,1"},"engine":{"vram-elastic":true}}),
            serde_json::json!({"server":{"API_KEY":"secret"},"engine":{"vram-elastic":true}}),
            serde_json::json!({"engine":{"vram-elastic":true,"vram-segment-mib":384}}),
            serde_json::json!({"engine":{}}),
        ] {
            assert!(
                validate_model(&args, &invalid).is_err(),
                "accepted {invalid}"
            );
        }
        args.no_vram_management = true;
        validate_model(&args, &serde_json::json!({"engine":{}})).unwrap();
    }

    #[test]
    fn cli_overrides_saved_values_and_relative_paths_follow_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path,r#"{"model":"models/model.json","basalt_dir":"engine","gpu":2,"theme":"ember","vram":{"min_free_mib":1024,"grow_below_mib":10000,"growth_mode":"capacity","step_mib":256}}"#).unwrap();
        let matches = Args::command()
            .try_get_matches_from([
                "basalt-wrap",
                "--config",
                path.to_str().unwrap(),
                "--min-free-mib",
                "1536",
                "--growth-mode",
                "idle",
            ])
            .unwrap();
        let mut args = Args::from_arg_matches(&matches).unwrap();
        apply(&mut args, &matches).unwrap();
        args.validate().unwrap();
        assert_eq!(args.min_free_mib, 1536);
        assert_eq!(args.grow_below_mib, 10000);
        assert_eq!(args.growth_mode, GrowthMode::Idle);
        assert_eq!(args.step_mib, 256);
        assert_eq!(args.gpu.as_deref(), Some("2"));
        assert_eq!(args.theme, Some(Theme::Ember));
        assert_eq!(args.basalt_dir, dir.path().join("engine"));
        assert_eq!(
            args.model.as_deref(),
            dir.path().join("models/model.json").to_str()
        );
        assert!(!args.auto_min_free && !args.auto_grow_below);
    }
    #[test]
    fn no_config_bypasses_a_broken_file_and_preserves_auto_sizing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.json");
        std::fs::write(&path, "broken").unwrap();
        let matches = Args::command()
            .try_get_matches_from([
                "basalt-wrap",
                "--config",
                path.to_str().unwrap(),
                "--no-config",
            ])
            .unwrap();
        let mut args = Args::from_arg_matches(&matches).unwrap();
        apply(&mut args, &matches).unwrap();
        assert!(args.auto_min_free && args.auto_grow_below);
    }
    #[test]
    fn capacities_scale_and_explicit_policy_is_preserved() {
        assert_eq!(thresholds(12288, None, None).unwrap(), (1024, 10240));
        assert_eq!(thresholds(16384, None, None).unwrap(), (1024, 14336));
        assert_eq!(thresholds(32768, None, None).unwrap(), (2048, 29696));
        assert_eq!(
            thresholds(16303, Some(1024), Some(14336)).unwrap(),
            (1024, 14336)
        );
        assert!(thresholds(12288, Some(1024), Some(14336)).is_err());
        assert!(thresholds(12288, Some(2048), Some(11000)).is_err());
    }
    #[test]
    fn config_creation_refuses_overwrite_and_rejects_typos() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        initialize(&path).unwrap();
        assert!(initialize(&path).is_err());
        assert_eq!(load(&path, true).unwrap().schema_version, 1);
        std::fs::write(&path, r#"{"vram":{"grow_dealy_seconds":2}}"#).unwrap();
        assert!(load(&path, true).is_err());
    }
    #[test]
    fn profiles_inherit_policy_and_cli_still_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path,r#"{"vram":{"min_free_mib":1536,"step_mib":256},"profiles":{"desktop":{"model":"model.json","vram":{"growth_mode":"capacity"}}}}"#).unwrap();
        let matches = Args::command()
            .try_get_matches_from([
                "basalt-wrap",
                "--config",
                path.to_str().unwrap(),
                "--profile",
                "desktop",
                "--step-mib",
                "512",
            ])
            .unwrap();
        let mut args = Args::from_arg_matches(&matches).unwrap();
        apply(&mut args, &matches).unwrap();
        assert_eq!(args.step_mib, 512);
        assert_eq!(args.min_free_mib, 1536);
        assert_eq!(args.growth_mode, GrowthMode::Capacity);
        assert_eq!(
            args.model.as_deref(),
            dir.path().join("model.json").to_str()
        );
        assert!(load_selected(&path, true, Some("missing")).is_err());
    }
    #[test]
    fn setup_can_create_an_explicit_new_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new/config.json");
        let matches = Args::command()
            .try_get_matches_from([
                "basalt-wrap",
                "iq3_s",
                "--setup",
                "--config",
                path.to_str().unwrap(),
            ])
            .unwrap();
        let mut args = Args::from_arg_matches(&matches).unwrap();
        apply(&mut args, &matches).unwrap();
        assert_eq!(args.settings_path, path);
    }
}
