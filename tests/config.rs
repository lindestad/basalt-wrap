use std::{
    fs,
    process::{Command, Output},
};

fn run(root: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_strata-wrap"))
        .args(args)
        .env("XDG_CONFIG_HOME", root)
        .output()
        .unwrap()
}

#[test]
fn config_commands_validate_and_never_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("strata-wrap/config.json");
    assert!(run(root.path(), &["--init-config"]).status.success());
    let original = fs::read(&path).unwrap();
    assert!(!run(root.path(), &["--init-config"]).status.success());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(run(root.path(), &["--check-config"]).status.success());
    for content in [
        r#"{"schema_version":2}"#,
        r#"{"vram":{"poll_seconds":0}}"#,
        r#"{"vram":{"growth_mode":"busy"}}"#,
        r#"{"vram":{"step_mib":0}}"#,
        r#"{"vram":{"min_free_mib":0}}"#,
        r#"{"typo":true}"#,
    ] {
        fs::write(&path, content).unwrap();
        assert!(
            !run(root.path(), &["--check-config"]).status.success(),
            "accepted {content}"
        );
    }
    fs::write(&path, r#"{"vram":{"poll_seconds":0}}"#).unwrap();
    assert!(
        !run(root.path(), &["--check-config", "--poll", "2"])
            .status
            .success()
    );
    assert!(
        run(root.path(), &["--no-config", "--snapshot", "80x24"])
            .status
            .success()
    );
}

#[test]
fn saved_theme_is_used_cli_wins_and_preview_preserves_config() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("strata-wrap");
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join("config.json");
    let config = r#"{"theme":"ember","vram":{"growth_mode":"capacity","step_mib":256}}"#;
    fs::write(&path, config).unwrap();
    let saved = run(root.path(), &["--snapshot", "100x30", "--cells"]);
    let explicit = run(
        root.path(),
        &["--snapshot", "100x30", "--cells", "--theme", "ember"],
    );
    assert!(saved.status.success() && explicit.status.success());
    assert_eq!(saved.stdout, explicit.stdout);
    let override_theme = run(
        root.path(),
        &["--snapshot", "100x30", "--cells", "--theme", "monochrome"],
    );
    assert!(override_theme.status.success());
    assert_ne!(saved.stdout, override_theme.stdout);
    assert_eq!(fs::read_to_string(path).unwrap(), config);
}
