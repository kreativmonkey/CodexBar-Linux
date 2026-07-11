use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Refresh interval in seconds.
    pub refresh_secs: u64,
    /// Providers to enable. Empty = auto-detect (every provider whose
    /// credentials are present on this machine).
    pub providers: Vec<String>,
    /// API keys per provider id, e.g. `[keys] openrouter = "sk-…"`.
    /// Environment variables (e.g. OPENROUTER_API_KEY) take precedence.
    pub keys: HashMap<String, String>,
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
            keys: HashMap::new(),
            popover_margin_top: 8,
            popover_margin_right: 8,
        }
    }
}

static GLOBAL: OnceLock<Config> = OnceLock::new();

/// Make the loaded config available to providers (call once at startup).
pub fn init_global(cfg: Config) {
    let _ = GLOBAL.set(cfg);
}

/// Resolve an API key for a provider: environment variable first, then the
/// `[keys]` table of the config file. Empty strings count as unset.
pub fn api_key(provider_id: &str, env_var: &str) -> Option<String> {
    std::env::var(env_var)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            GLOBAL
                .get()
                .and_then(|c| c.keys.get(provider_id).cloned())
                .filter(|s| !s.trim().is_empty())
        })
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
