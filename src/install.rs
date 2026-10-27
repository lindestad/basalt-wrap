use crate::{home, runtime};
use anyhow::{Context, Result, bail};
use std::{fs, os::unix::fs::symlink};

pub fn run() -> Result<()> {
    let launcher = runtime::executable()?;
    let link = home().join(".local/bin/basalt-wrap");
    if fs::symlink_metadata(&link).is_ok() {
        if link.is_symlink() && fs::read_link(&link)? == launcher {
            println!("Already installed at {}", link.display());
            return Ok(());
        }
        bail!(
            "{} already exists and points somewhere else",
            link.display()
        );
    }
    fs::create_dir_all(link.parent().context("missing launcher directory")?)?;
    symlink(launcher, &link)?;
    println!("Installed launcher at {}", link.display());
    Ok(())
}
