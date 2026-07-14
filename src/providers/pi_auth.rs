//! Read API keys stored by the [Pi](https://github.com/badlogic/pi-mono) coding agent.
//!
//! Credentials live in `~/.pi/agent/auth.json` (or `$PI_CODING_AGENT_DIR/auth.json`).

use std::path::PathBuf;

const AUTH_FILE: &str = "auth.json";

/// Resolve Pi's agent directory (`~/.pi/agent` by default).
pub fn data_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("PI_CODING_AGENT_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|d| d.join(".pi").join("agent"))
}

pub fn auth_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join(AUTH_FILE))
}

/// Read an API key for `service_id` from Pi's on-disk credential store.
pub fn provider_api_key(service_id: &str) -> Option<String> {
    let path = auth_path()?;
    super::cli_agent_auth::read_auth_file(&path, service_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_path_defaults_to_home_pi_agent() {
        let path = auth_path().unwrap();
        assert!(path.ends_with(".pi/agent/auth.json"));
    }
}
