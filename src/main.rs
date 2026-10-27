mod app;
mod config;
mod control;
mod doctor;
mod hardware;
mod install;
mod model;
mod policy;
mod runtime;
mod theme;
mod ui;

use anyhow::{Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser};
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(
    version,
    about = "A Basalt inference dashboard and elastic VRAM supervisor"
)]
pub struct Args {
    /// Validate the checkout, model, systemd user manager, GPU, and effective policy
    #[arg(long)]
    pub doctor: bool,
    /// Create a config for an existing checkout/model and detected GPU; never overwrite
    #[arg(long)]
    pub setup: bool,
    /// Monitor an existing worker or server; quitting never stops it
    #[arg(long)]
    pub attach: bool,
    /// Select a named model/policy profile from the wrapper config
    #[arg(long)]
    pub profile: Option<String>,
    #[arg(long)]
    pub list_profiles: bool,
    /// Generate a completion script for your shell
    #[arg(long, value_enum)]
    pub completions: Option<clap_complete::Shell>,

    /// Show request ages with seconds (toggle with a)
    #[arg(long)]
    pub age_seconds: bool,
    /// Per-user wrapper settings (distinct from the positional Basalt model JSON)
    #[arg(long = "config")]
    pub wrapper_config: Option<PathBuf>,
    /// Use built-in settings without reading the user config
    #[arg(long)]
    pub no_config: bool,
    /// Create a starter config without overwriting an existing file
    #[arg(long)]
    pub init_config: bool,
    /// Validate user settings without starting Basalt
    #[arg(long)]
    pub check_config: bool,
    /// List NVIDIA GPU indices, UUIDs, and memory sizes
    #[arg(long)]
    pub list_gpus: bool,
    /// Read the running server and GPU to explain growth eligibility; never resize or start a server
    #[arg(long)]
    pub diagnose: bool,
    /// NVIDIA GPU index or UUID; applied to both the engine and monitoring
    #[arg(long)]
    pub gpu: Option<String>,
    #[arg(long, value_enum, default_value = "idle")]
    pub growth_mode: policy::GrowthMode,
    #[arg(long, default_value_t = 512)]
    pub step_mib: u64,
    #[arg(long, default_value_t = 16)]
    pub accounting_margin_mib: u64,
    #[arg(long, default_value_t = 60.)]
    pub no_op_retry: f64,
    #[arg(long)]
    pub no_vram_management: bool,
    #[arg(skip)]
    pub settings_path: PathBuf,
    #[arg(skip)]
    pub auto_min_free: bool,
    #[arg(skip)]
    pub auto_grow_below: bool,
    /// iq3_s, ud-iq3_s, a model JSON path, or 'install'
    pub model: Option<String>,
    #[arg(long, default_value_os_t = home().join("apps/basalt"))]
    pub basalt_dir: PathBuf,
    #[arg(long)]
    pub port: Option<u16>,
    #[arg(long, default_value = "basalt-wrap")]
    pub unit: String,
    #[arg(long, default_value_t = 1024)]
    pub min_free_mib: u64,
    #[arg(long, default_value_t = 14336)]
    pub grow_below_mib: u64,
    #[arg(long, default_value_t = 60.0)]
    pub grow_delay: f64,
    #[arg(long, default_value_t = 2.0)]
    pub poll: f64,
    /// Print summaries without entering the terminal UI
    #[arg(long)]
    pub plain: bool,
    /// Include unfiltered server output in plain mode
    #[arg(long)]
    pub raw: bool,
    /// Preview the dashboard with sample data, without starting Basalt
    #[arg(long, visible_alias = "preview")]
    pub demo: bool,
    /// Choose a palette for this launch (overrides the saved preference)
    #[arg(long, value_enum)]
    pub theme: Option<theme::Theme>,
    /// List palettes without starting Basalt
    #[arg(long)]
    pub list_themes: bool,
    /// Render sample data at WIDTHxHEIGHT to plain text (or JSON with --cells)
    #[arg(long)]
    pub snapshot: Option<String>,
    #[arg(long, hide = true)]
    pub cells: bool,
    #[arg(long, hide = true)]
    pub worker: bool,
    #[arg(long, hide = true)]
    pub owner_pid: Option<u32>,
    #[arg(long, hide = true)]
    pub owner_start: Option<String>,
    #[arg(last = true)]
    pub server_args: Vec<String>,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME is set")
}

impl Args {
    pub fn config(&self) -> PathBuf {
        match self.model.as_deref().unwrap_or("iq3_s") {
            "iq3_s" => self.basalt_dir.join("basalt-iq3_s.json"),
            "ud-iq3_s" => self.basalt_dir.join("basalt-ud-iq3_s.json"),
            path => std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path)),
        }
    }

    pub fn unit_name(&self) -> String {
        format!("{}.service", self.unit.trim_end_matches(".service"))
    }

    fn validate(&self) -> Result<()> {
        if let Some(gpu) = &self.gpu {
            anyhow::ensure!(
                gpu.parse::<u32>().is_ok()
                    || (gpu.starts_with("GPU-") && !gpu.chars().any(char::is_whitespace)),
                "GPU must be an NVIDIA index or GPU UUID"
            );
        }
        if !self.poll.is_finite()
            || !(0.2..=60.).contains(&self.poll)
            || !self.grow_delay.is_finite()
            || !(0.0..=86400.).contains(&self.grow_delay)
            || self.grow_below_mib == 0
            || self.min_free_mib == 0
            || self.step_mib < 64
            || self.step_mib > 65536
            || self.accounting_margin_mib > 4096
            || !self.no_op_retry.is_finite()
            || !(1.0..=86400.).contains(&self.no_op_retry)
        {
            bail!("invalid VRAM thresholds or polling interval");
        }
        anyhow::ensure!(self.port != Some(0), "port must be between 1 and 65535");
        if self.unit.is_empty()
            || !self
                .unit
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        {
            bail!("invalid systemd unit name");
        }
        if self.server_args.iter().any(|v| {
            [
                "--host",
                "--port",
                "--config",
                "--engine",
                "--api-key",
                "--gpu",
                "--gpus",
                "--model",
                "--engine-bin",
                "--python",
            ]
            .contains(&v.split('=').next().unwrap_or(""))
        }) {
            bail!(
                "the wrapper owns host, port, model, engine and API key; use the model JSON and BASALT_WRAP_API_KEY"
            );
        }
        Ok(())
    }
}

fn run() -> Result<i32> {
    let matches = Args::command().get_matches();
    let mut args = Args::from_arg_matches(&matches)?;
    if let Some(shell) = args.completions {
        clap_complete::generate(
            shell,
            &mut Args::command(),
            "basalt-wrap",
            &mut std::io::stdout(),
        );
        return Ok(0);
    }
    if let Err(error) = config::apply(&mut args, &matches) {
        if (args.diagnose || args.attach) && control::socket_path(&args).exists() {
            args.validate()?;
            return if args.diagnose {
                runtime::diagnose(&args)
            } else {
                app::attach(&args)
            };
        }
        return Err(error);
    }
    args.validate()?;
    if args.init_config {
        config::initialize(&args.settings_path)?;
        return Ok(0);
    }
    if args.list_profiles {
        for name in config::load(&args.settings_path, true)?.profiles.keys() {
            println!("{name}");
        }
        return Ok(0);
    }
    if args.setup {
        return doctor::setup(&args);
    }
    if args.doctor {
        return doctor::run(&args);
    }
    if args.attach {
        return app::attach(&args);
    }
    if args.check_config {
        config::load(&args.settings_path, true)?;
        if args.model.is_some() {
            return doctor::run(&args);
        }
        println!(
            "Valid wrapper settings: {} (use --doctor MODEL to check the setup)",
            args.settings_path.display()
        );
        return Ok(0);
    }
    if args.list_gpus {
        for gpu in hardware::gpus()? {
            println!(
                "{}  {}  {}  {} MiB",
                gpu.index, gpu.uuid, gpu.name, gpu.total_mib
            );
        }
        return Ok(0);
    }
    if args.list_themes {
        for theme in theme::Theme::ALL {
            println!("{:<18} {}", theme.name(), theme.description());
        }
        return Ok(0);
    }
    if args.model.as_deref() == Some("install") {
        install::run()?;
        return Ok(0);
    }
    if args.demo || args.snapshot.is_some() {
        return app::demo(&args);
    }
    if args.diagnose && crate::control::socket_path(&args).exists() {
        return runtime::diagnose(&args);
    }
    if args.model.is_none() {
        bail!("choose iq3_s, ud-iq3_s, or a model JSON path (use --help)");
    }
    if args.worker {
        runtime::worker(&args)
    } else {
        let model = runtime::read_config(&args)?;
        config::resolve_model(&mut args, &model)?;
        if !args.diagnose {
            config::validate_model(&args, &model)?;
        }
        if args.diagnose {
            runtime::diagnose(&args)
        } else {
            app::launch(&args)
        }
    }
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("basalt-wrap: {error:#}");
            1
        }
    };
    std::process::exit(code);
}
