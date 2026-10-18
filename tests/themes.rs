use std::process::Command;

#[test]
fn theme_cli_is_usable_without_a_model_or_server() {
    let root = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_strata-wrap");
    let output = Command::new(binary)
        .arg("--list-themes")
        .env("XDG_CONFIG_HOME", root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().lines().count(),
        11
    );
    for theme in [
        "ember",
        "monochrome",
        "solarized-light",
        "ayu-dark",
        "rose-pine",
        "everforest",
    ] {
        let output = Command::new(binary)
            .args([
                "--preview",
                "--theme",
                theme,
                "--snapshot",
                "80x24",
                "--cells",
                "--strata-dir",
                "/nonexistent-strata-theme-preview",
            ])
            .env("XDG_CONFIG_HOME", root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cells: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(cells["width"], 80);
        assert_eq!(cells["height"], 24);
    }
    let output = Command::new(binary)
        .args(["--preview", "--theme", "missing"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
