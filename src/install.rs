use crate::{home, runtime};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn run() -> Result<()> {
    let home = home();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let legacy_launcher = root.join("strata-wrap");
    let launcher = runtime::executable()?;
    let link = home.join(".local/bin/strata-wrap");
    if fs::symlink_metadata(&link).is_ok()
        && (!link.is_symlink()
            || !fs::read_link(&link).is_ok_and(|v| v == launcher || v == legacy_launcher))
    {
        bail!(
            "{} already exists and points somewhere else",
            link.display()
        );
    }
    let mut changes = vec![];
    for model in ["iq3_s", "ud-iq3_s"] {
        let path = home.join(format!("apps/Strata/strata-{model}.json"));
        let mut data: Value = serde_json::from_slice(
            &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )?;
        data["vram_elastic"] = true.into();
        data["vram_segment_mib"] = 512.into();
        changes.push((path, format!("{}\n", serde_json::to_string_pretty(&data)?)));
    }
    let fish = home.join(".config/fish/.config.local.fish");
    let mut lines = vec![];
    let mut counts = [0, 0];
    for line in fs::read_to_string(&fish)?.lines() {
        if line.starts_with("abbr --add sa ") {
            counts[0] += 1;
            lines.push("abbr --add sa '$HOME/.local/bin/strata-wrap iq3_s'".into());
        } else if line.starts_with("abbr --add saud ") {
            counts[1] += 1;
            lines.push("abbr --add saud '$HOME/.local/bin/strata-wrap ud-iq3_s'".into());
        } else {
            lines.push(line.to_owned());
        }
    }
    if counts != [1, 1] {
        bail!(
            "expected exactly one sa and saud abbreviation in {}",
            fish.display()
        );
    }
    changes.push((fish, format!("{}\n", lines.join("\n"))));
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let backup = home.join(format!(".local/state/strata-wrap/backups/{stamp}"));
    fs::create_dir_all(&backup)?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o700))?;
    for (path, data) in changes {
        fs::copy(
            &path,
            backup.join(path.file_name().context("missing filename")?),
        )?;
        fs::write(&path, data)?;
    }
    fs::create_dir_all(link.parent().context("missing launcher directory")?)?;
    if link.is_symlink() {
        fs::remove_file(&link)?;
    }
    symlink(launcher, &link)?;
    println!("Installed Rust launcher at {}", link.display());
    println!(
        "Elastic mode enabled for both models; sa and saud updated. Backups: {}",
        backup.display()
    );
    println!("Reload Fish: source ~/.config/fish/.config.local.fish");
    Ok(())
}
