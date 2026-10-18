mod app;
mod hardware;
mod install;
mod model;
mod policy;
mod runtime;
mod ui;

use anyhow::{Result, bail};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(
    version,
    about = "A Strata inference dashboard and elastic VRAM supervisor"
)]
pub struct Args {
    /// iq3_s, ud-iq3_s, a model JSON path, or 'install'
    pub model: Option<String>,
    #[arg(long, default_value_os_t = home().join("apps/Strata"))]
    pub strata_dir: PathBuf,
    #[arg(long)]
    pub port: Option<u16>,
    #[arg(long, default_value = "strata-wrap")]
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
    /// Preview the dashboard with sample data, without starting Strata
    #[arg(long)]
    pub demo: bool,
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
            "iq3_s" => self.strata_dir.join("strata-iq3_s.json"),
            "ud-iq3_s" => self.strata_dir.join("strata-ud-iq3_s.json"),
            path => std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path)),
        }
    }

    pub fn unit_name(&self) -> String {
        format!("{}.service", self.unit.trim_end_matches(".service"))
    }

    fn validate(&self) -> Result<()> {
        if !self.poll.is_finite()
            || self.poll < 0.2
            || !self.grow_delay.is_finite()
            || self.grow_delay < 0.0
            || self.grow_below_mib == 0
        {
            bail!("invalid VRAM thresholds or polling interval");
        }
        if self.unit.is_empty()
            || !self
                .unit
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        {
            bail!("invalid systemd unit name");
        }
        if self.server_args.iter().any(|v| {
            ["--host", "--port", "--config", "--engine", "--api-key"]
                .contains(&v.split('=').next().unwrap_or(""))
        }) {
            bail!(
                "the wrapper owns host, port and engine; set the model and API key in its JSON config"
            );
        }
        Ok(())
    }
}

fn run() -> Result<i32> {
    let args = Args::parse();
    args.validate()?;
    if args.model.as_deref() == Some("install") {
        install::run()?;
        return Ok(0);
    }
    if args.demo || args.snapshot.is_some() {
        return app::demo(&args);
    }
    if args.model.is_none() {
        bail!("choose iq3_s, ud-iq3_s, or a model JSON path (use --help)");
    }
    if args.worker {
        runtime::worker(&args)
    } else {
        app::launch(&args)
    }
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("strata-wrap: {error:#}");
            1
        }
    };
    std::process::exit(code);
}
