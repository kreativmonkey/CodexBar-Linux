//! Shared `auth.json` parsing for CLI coding agents (OpenCode, Pi, …).

use std::path::Path;

/// Parse `{ "<service>": { "key": "…" } }` from an auth file body.
pub fn parse_auth_json(body: &str, service_id: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    parse_service_key(&json, service_id)
}

/// Parse OpenCode-style `account.json` v2 (active account pointer).
pub fn parse_account_json(body: &str, service_id: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let active_id = json.get("active")?.get(service_id)?.as_str()?;
    let account = json.get("accounts")?.get(active_id)?;
    if account.get("serviceID")?.as_str()? != service_id {
        return None;
    }
    let key = account.get("credential")?.get("key")?.as_str()?;
    non_empty(key)
}

pub fn read_auth_file(path: &Path, service_id: &str) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    parse_auth_json(&body, service_id)
}

fn parse_service_key(json: &serde_json::Value, service_id: &str) -> Option<String> {
    let key = json.get(service_id)?.get("key")?.as_str()?;
    non_empty(key)
}

fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.starts_with('!') {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// OpenCode first, then Pi — after config/env keys are exhausted.
pub fn provider_api_key(service_id: &str) -> Option<String> {
    super::opencode_auth::provider_api_key(service_id)
        .or_else(|| super::pi_auth::provider_api_key(service_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTH_JSON: &str = r#"{
        "openrouter": { "type": "api_key", "key": "sk-or-test" }
    }"#;

    #[test]
    fn parse_auth_json_extracts_key() {
        assert_eq!(
            parse_auth_json(AUTH_JSON, "openrouter").as_deref(),
            Some("sk-or-test")
        );
    }

    #[test]
    fn parse_auth_json_rejects_shell_command_key() {
        assert!(parse_auth_json(
            r#"{ "openrouter": { "key": "!security find-generic-password ..." } }"#,
            "openrouter"
        )
        .is_none());
    }
}
