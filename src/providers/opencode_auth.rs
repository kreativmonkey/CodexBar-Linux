//! Read API keys stored by the [OpenCode](https://opencode.ai) CLI.
//!
//! Credentials live in `~/.local/share/opencode/auth.json` (or `OPENCODE_AUTH_PATH`).
//! Newer installs also mirror them in `account.json` with an `active` pointer.

use std::path::PathBuf;

const AUTH_FILE: &str = "auth.json";
const ACCOUNT_FILE: &str = "account.json";

/// OpenCode Zen is stored under the `opencode` service id.
pub const ZEN_SERVICE_ID: &str = "opencode";

/// Resolve the OpenCode data directory (`~/.local/share/opencode` by default).
pub fn data_dir() -> Option<PathBuf> {
    std::env::var("XDG_DATA_HOME")
        .ok()
        .map(PathBuf::from)
        .map(|d| d.join("opencode"))
        .or_else(|| dirs::data_local_dir().map(|d| d.join("opencode")))
}

/// Resolve the credentials file path (honours `OPENCODE_AUTH_PATH`).
pub fn auth_path() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("OPENCODE_AUTH_PATH") {
        let path = PathBuf::from(raw);
        return Some(if path.is_absolute() {
            path
        } else {
            data_dir()?.join(path)
        });
    }
    data_dir().map(|d| d.join(AUTH_FILE))
}

fn account_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join(ACCOUNT_FILE))
}

/// Read an API key for `service_id` from OpenCode's on-disk credential store.
pub fn provider_api_key(service_id: &str) -> Option<String> {
    if let Some(path) = auth_path() {
        if let Some(key) = super::cli_agent_auth::read_auth_file(&path, service_id) {
            return Some(key);
        }
    }
    if let Some(path) = account_path() {
        if let Ok(body) = std::fs::read_to_string(&path) {
            if let Some(key) = super::cli_agent_auth::parse_account_json(&body, service_id) {
                return Some(key);
            }
        }
    }
    None
}

pub fn zen_key() -> Option<String> {
    provider_api_key(ZEN_SERVICE_ID)
}

#[cfg(test)]
mod tests {
    use crate::providers::cli_agent_auth;

    const AUTH_JSON: &str = r#"{
        "opencode": { "type": "api", "key": "sk-zen" },
        "openrouter": { "type": "api", "key": "sk-or-test" }
    }"#;

    const ACCOUNT_JSON: &str = r#"{
        "version": 2,
        "accounts": {
            "acc1": {
                "id": "acc1",
                "serviceID": "opencode",
                "credential": { "type": "api", "key": "sk-zen-account" }
            },
            "acc2": {
                "id": "acc2",
                "serviceID": "openrouter",
                "credential": { "type": "api", "key": "sk-or-account" }
            }
        },
        "active": {
            "opencode": "acc1",
            "openrouter": "acc2"
        }
    }"#;

    #[test]
    fn parse_auth_json_extracts_keys() {
        assert_eq!(
            cli_agent_auth::parse_auth_json(AUTH_JSON, "openrouter").as_deref(),
            Some("sk-or-test")
        );
        assert_eq!(
            cli_agent_auth::parse_auth_json(AUTH_JSON, "opencode").as_deref(),
            Some("sk-zen")
        );
    }

    #[test]
    fn parse_account_json_uses_active_pointer() {
        assert_eq!(
            cli_agent_auth::parse_account_json(ACCOUNT_JSON, "openrouter").as_deref(),
            Some("sk-or-account")
        );
    }
}
