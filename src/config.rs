use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Refresh interval in seconds.
    pub refresh_secs: u64,
    /// Providers to enable. Empty = auto-detect (every provider whose
    /// credentials are present on this machine).
    pub providers: Vec<String>,
    /// Pixel gap between the top screen edge (below the bar) and the popover.
    pub popover_margin_top: i32,
    /// Pixel gap between the right screen edge and the popover.
    pub popover_margin_right: i32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_secs: 300,
            providers: Vec::new(),
            popover_margin_top: 8,
            popover_margin_right: 8,
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("codexbar/config.toml"))
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => match toml::from_str(&raw) {
                Ok(cfg) => cfg,
                Err(err) => {
                    tracing::warn!("invalid {}: {err} — using defaults", path.display());
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }
}
