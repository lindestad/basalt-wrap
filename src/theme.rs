use anyhow::{Context, Result};
use clap::ValueEnum;
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    Strata,
    Ember,
    Monochrome,
    Nord,
    Dracula,
    Gruvbox,
    Catppuccin,
    SolarizedLight,
    AyuDark,
    RosePine,
    Everforest,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color,
    pub card: Color,
    pub border: Color,
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
    pub secondary: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub track: Color,
}

impl Theme {
    pub const ALL: [Self; 11] = [
        Self::Strata,
        Self::Ember,
        Self::Monochrome,
        Self::Nord,
        Self::Dracula,
        Self::Gruvbox,
        Self::Catppuccin,
        Self::SolarizedLight,
        Self::AyuDark,
        Self::RosePine,
        Self::Everforest,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Strata => "strata",
            Self::Ember => "ember",
            Self::Monochrome => "monochrome",
            Self::Nord => "nord",
            Self::Dracula => "dracula",
            Self::Gruvbox => "gruvbox",
            Self::Catppuccin => "catppuccin",
            Self::SolarizedLight => "solarized-light",
            Self::AyuDark => "ayu-dark",
            Self::RosePine => "rose-pine",
            Self::Everforest => "everforest",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Strata => "Midnight blue · sea glass & lavender",
            Self::Ember => "Carbon black · white & burnt orange",
            Self::Monochrome => "Pure grayscale · no color accents",
            Self::Nord => "Polar slate · frost & aurora",
            Self::Dracula => "Charcoal violet · neon pastels",
            Self::Gruvbox => "Warm charcoal · earthy gold & aqua",
            Self::Catppuccin => "Mocha · soft teal & mauve",
            Self::SolarizedLight => "Warm ivory · blue & violet",
            Self::AyuDark => "Ink black · amber & sky blue",
            Self::RosePine => "Dusk violet · rose & iris",
            Self::Everforest => "Forest charcoal · sage & sea green",
        }
    }

    pub fn palette(self) -> Palette {
        // Background, surface, edge, text, muted, primary, secondary,
        // success, warning, error, selection / gauge track.
        let colors = match self {
            Self::Strata => [
                0x0f141e, 0x141c29, 0x2b394d, 0xd7e1ef, 0x8093ab, 0x6fd7cf, 0xb0a1f1, 0x9fd5aa,
                0xedc78b, 0xee8f99, 0x232f41,
            ],
            Self::Ember => [
                0x080808, 0x101010, 0x383838, 0xe8e8e8, 0x999999, 0xe87924, 0xbfbfbf, 0xd8d8d8,
                0xf3a35c, 0xff8933, 0x303030,
            ],
            Self::Monochrome => [
                0x090909, 0x121212, 0x404040, 0xe8e8e8, 0x9b9b9b, 0xf0f0f0, 0xbababa, 0xd4d4d4,
                0xffffff, 0xffffff, 0x333333,
            ],
            Self::Nord => [
                0x2e3440, 0x343b49, 0x4c566a, 0xe5e9f0, 0xa7b1c2, 0x88c0d0, 0xb48ead, 0xa3be8c,
                0xebcb8b, 0xbf616a, 0x434c5e,
            ],
            Self::Dracula => [
                0x22232e, 0x282a36, 0x53566d, 0xf8f8f2, 0xa4acc8, 0x8be9fd, 0xbd93f9, 0x50fa7b,
                0xf1fa8c, 0xff5555, 0x44475a,
            ],
            Self::Gruvbox => [
                0x1d2021, 0x282828, 0x665c54, 0xebdbb2, 0xbdae93, 0xfabd2f, 0x8ec07c, 0xb8bb26,
                0xfe8019, 0xfb4934, 0x504945,
            ],
            Self::Catppuccin => [
                0x181825, 0x1e1e2e, 0x45475a, 0xcdd6f4, 0xa6adc8, 0x94e2d5, 0xcba6f7, 0xa6e3a1,
                0xf9e2af, 0xf38ba8, 0x45475a,
            ],
            Self::SolarizedLight => [
                0xeee8d5, 0xfdf6e3, 0xa8b4b2, 0x3c5158, 0x586e75, 0x006da8, 0x6c60aa, 0x526600,
                0x986400, 0xc52c29, 0xd9dfd3,
            ],
            Self::AyuDark => [
                0x0b0e14, 0x0f131a, 0x3c414a, 0xbfbdb6, 0x8a9199, 0xe6b450, 0x59c2ff, 0xaad94c,
                0xff8f40, 0xf07178, 0x243447,
            ],
            Self::RosePine => [
                0x191724, 0x1f1d2e, 0x403d52, 0xe0def4, 0x908caa, 0xebbcba, 0xc4a7e7, 0x9ccfd8,
                0xf6c177, 0xeb6f92, 0x403d52,
            ],
            Self::Everforest => [
                0x1e2326, 0x272e33, 0x495156, 0xd3c6aa, 0x9da9a0, 0xa7c080, 0x7fbbb3, 0xa7c080,
                0xdbbc7f, 0xe67e80, 0x374145,
            ],
        };
        let [
            bg,
            card,
            border,
            fg,
            muted,
            accent,
            secondary,
            success,
            warning,
            error,
            track,
        ] = colors.map(|hex| Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8));
        Palette {
            bg,
            card,
            border,
            fg,
            muted,
            accent,
            secondary,
            success,
            warning,
            error,
            track,
        }
    }
}

pub fn preferences_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| Path::new(p).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::home().join(".config"))
        .join("strata-wrap/preferences.json")
}

pub fn load(path: &Path) -> Result<Theme> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Theme::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let value: serde_json::Value =
        serde_json::from_slice(&data).with_context(|| format!("parsing {}", path.display()))?;
    anyhow::ensure!(value.is_object(), "preferences must be a JSON object");
    match value.get("theme") {
        Some(value) => serde_json::from_value(value.clone()).context("unknown saved theme"),
        None => Ok(Theme::default()),
    }
}

pub fn save(path: &Path, theme: Theme) -> Result<()> {
    save_profile(path, theme, None)
}

pub fn save_profile(path: &Path, theme: Theme, profile: Option<&str>) -> Result<()> {
    let mut data = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };
    anyhow::ensure!(data.is_object(), "preferences must be a JSON object");
    if let Some(name) = profile {
        let target = data
            .get_mut("profiles")
            .and_then(|p| p.get_mut(name))
            .context("selected profile no longer exists")?;
        anyhow::ensure!(target.is_object(), "profile must be an object");
        target["theme"] = serde_json::to_value(theme)?;
    } else {
        data["theme"] = serde_json::to_value(theme)?;
    }
    let parent = path.parent().context("preferences need a directory")?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".preferences-{}.tmp", std::process::id()));
    let result = (|| {
        std::fs::write(&temp, format!("{}\n", serde_json::to_string_pretty(&data)?))?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_preserves_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings/preferences.json");
        assert_eq!(load(&path).unwrap(), Theme::Strata);
        save(&path, Theme::Ember).unwrap();
        assert_eq!(load(&path).unwrap(), Theme::Ember);
        std::fs::write(&path, r#"{"theme":"nord","other":42}"#).unwrap();
        save(&path, Theme::Monochrome).unwrap();
        assert_eq!(load(&path).unwrap(), Theme::Monochrome);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value["other"], 42);
    }

    #[test]
    fn malformed_preferences_are_reported_and_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        std::fs::write(&path, "broken").unwrap();
        assert!(load(&path).is_err());
        assert!(save(&path, Theme::Ember).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
    }
    #[test]
    fn profile_theme_updates_only_that_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path,r#"{"theme":"nord","profiles":{"ud":{"theme":"ember","model":"ud-iq3_s"},"regular":{"theme":"dracula"}},"vram":{"step_mib":512}}"#).unwrap();
        save_profile(&path, Theme::Monochrome, Some("ud")).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["profiles"]["ud"]["theme"], "monochrome");
        assert_eq!(value["theme"], "nord");
        assert_eq!(value["profiles"]["regular"]["theme"], "dracula");
        assert_eq!(value["vram"]["step_mib"], 512);
    }
}
