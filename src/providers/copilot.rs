use crate::model::{RateWindow, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use tracing::debug;

// HTTP transport shared with the Claude provider.
use super::claude::http_get;

pub struct CopilotProvider;

impl CopilotProvider {
    pub fn new() -> Self {
        Self
    }
}

// ── credential helpers ────────────────────────────────────────────────────────

/// Paths where `gh auth login --scopes copilot` stores a GitHub OAuth token.
/// On Linux the gh CLI stores credentials in `~/.config/gh/hosts.json`.
/// `~/.config/github-copilot/apps.json` is written by VS Code / JetBrains.
fn find_github_token() -> Option<String> {
    // 1. gh CLI hosts.json: ~/.config/gh/hosts.json
    if let Some(token) = read_gh_hosts_token() {
        return Some(token);
    }
    // 2. github-copilot apps.json: ~/.config/github-copilot/apps.json
    if let Some(token) = read_copilot_apps_token() {
        return Some(token);
    }
    None
}

fn gh_hosts_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".config").join("gh").join("hosts.json"))
}

fn copilot_apps_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".config")
            .join("github-copilot")
            .join("apps.json"),
    )
}

/// Read the OAuth token from `gh` CLI's hosts.json.
/// Format: `{ "github.com": { "oauth_token": "gho_…", … } }`
fn read_gh_hosts_token() -> Option<String> {
    let path = gh_hosts_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    // Support both github.com and enterprise hosts
    for (_host, host_obj) in json.as_object()? {
        if let Some(token) = host_obj
            .get("oauth_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(token.to_string());
        }
    }
    None
}

/// Read the OAuth token from VS Code / JetBrains apps.json.
/// Format varies; typically `[{ "oauth_token": "gho_…" }]` or
/// `{ "github.com:…": { "oauth_token": "gho_…" } }`
fn read_copilot_apps_token() -> Option<String> {
    let path = copilot_apps_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;

    // Try array form first
    if let Some(arr) = json.as_array() {
        for entry in arr {
            if let Some(token) = entry
                .get("oauth_token")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                return Some(token.to_string());
            }
        }
    }

    // Try object form
    if let Some(obj) = json.as_object() {
        for (_key, val) in obj {
            if let Some(token) = val
                .get("oauth_token")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                return Some(token.to_string());
            }
        }
    }

    None
}

// ── response parsing ──────────────────────────────────────────────────────────

/// Parse the `/copilot_internal/user` JSON response.
///
/// JSON shape (from CopilotUsageModels.swift):
/// ```json
/// {
///   "copilot_plan": "individual",
///   "token_based_billing": false,
///   "quota_reset_date": "2025-02-01",
///   "quota_snapshots": {
///     "premium_interactions": {
///       "entitlement": 300,
///       "remaining": 245,
///       "percent_remaining": 81.67,
///       "quota_id": "premium_interactions",
///       "unlimited": false
///     },
///     "chat": {
///       "entitlement": 0, "remaining": 0,
///       "percent_remaining": 0, "quota_id": "chat", "unlimited": true
///     }
///   }
/// }
/// ```
fn parse_usage_response(body: &str) -> anyhow::Result<UsageSnapshot> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("Copilot usage response is not JSON")?;

    let copilot_plan = json
        .get("copilot_plan")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let token_based_billing = json
        .get("token_based_billing")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let quota_reset_date = json
        .get("quota_reset_date")
        .and_then(|v| v.as_str())
        .and_then(parse_reset_date);

    let snapshots = json.get("quota_snapshots");

    let premium_window = snapshots
        .and_then(|s| s.get("premium_interactions"))
        .and_then(|snap| parse_quota_window(snap, "Premium requests", quota_reset_date));

    let chat_window = snapshots
        .and_then(|s| s.get("chat"))
        .and_then(|snap| parse_quota_window(snap, "Chat", quota_reset_date));

    let mut windows: Vec<RateWindow> = Vec::new();

    match (premium_window, chat_window) {
        (Some(p), maybe_chat) => {
            windows.push(p);
            if let Some(c) = maybe_chat {
                windows.push(c);
            }
        }
        (None, Some(c)) => {
            windows.push(c);
        }
        (None, None) if token_based_billing => {
            // Token-based Copilot Business has no per-request quota on this endpoint.
            // Surface the plan name without fake usage (mirrors Swift behaviour).
        }
        (None, None) => {
            bail!("Copilot: no usable quota data in response.");
        }
    }

    debug!("copilot: parsed {} windows", windows.len());

    Ok(UsageSnapshot {
        plan: Some(capitalize(&copilot_plan)),
        account: None,
        windows,
        credits: None,
        fetched_at: Some(Utc::now()),
    })
}

/// Convert one `quota_snapshots` entry to a `RateWindow`.
///
/// Mirrors `CopilotUsageFetcher.makeRateWindow` logic:
/// - Returns `None` if the snapshot is a placeholder (entitlement=0 and remaining=0)
///   or if `unlimited` is true (no finite quota to display).
/// - `used_percent = 100 - percent_remaining`, derived from `remaining / entitlement * 100`
///   when `percent_remaining` is absent.
fn parse_quota_window(
    snap: &serde_json::Value,
    label: &str,
    resets_at: Option<DateTime<Utc>>,
) -> Option<RateWindow> {
    let unlimited = snap
        .get("unlimited")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if unlimited {
        return None;
    }

    let entitlement = decode_number(snap.get("entitlement")).unwrap_or(0.0);
    let remaining = decode_number(snap.get("remaining")).unwrap_or(0.0);
    let percent_remaining_raw = decode_number(snap.get("percent_remaining"));

    // Detect placeholder: both entitlement and remaining are zero.
    let entitlement_present = snap.get("entitlement").is_some();
    let remaining_present = snap.get("remaining").is_some();
    if entitlement_present && remaining_present && entitlement == 0.0 && remaining == 0.0 {
        return None;
    }

    // Derive percent_remaining if not directly provided.
    let percent_remaining = if let Some(p) = percent_remaining_raw {
        p
    } else if entitlement > 0.0 {
        (remaining / entitlement) * 100.0
    } else {
        return None; // cannot compute percent
    };

    let used_percent = (100.0 - percent_remaining).clamp(0.0, 100.0);

    Some(RateWindow {
        label: label.to_string(),
        used_percent,
        resets_at,
        caption: None,
    })
}

/// Decode a JSON value as f64 accepting Number, String-encoded number.
fn decode_number(v: Option<&serde_json::Value>) -> Option<f64> {
    let v = v?;
    if let Some(f) = v.as_f64() {
        return Some(f);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<f64>().ok();
    }
    None
}

/// Parse reset date in "yyyy-MM-dd" or ISO-8601 format.
fn parse_reset_date(s: &str) -> Option<DateTime<Utc>> {
    // Try full ISO-8601 first.
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // Try bare date "2025-02-01" → treat as UTC midnight.
    if let Ok(nd) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return nd
            .and_hms_opt(0, 0, 0)
            .map(|ndt| chrono::TimeZone::from_utc_datetime(&Utc, &ndt));
    }
    None
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for CopilotProvider {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn display_name(&self) -> &'static str {
        "Copilot"
    }

    fn is_configured(&self) -> bool {
        // Check env var first, then file-based tokens.
        crate::config::api_key("copilot", "COPILOT_API_TOKEN").is_some()
            || find_github_token().is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        fetch_usage().await
    }
}

async fn fetch_usage() -> anyhow::Result<UsageSnapshot> {
    // Token resolution: env/config first, then file-based gh / apps.json.
    let token = crate::config::api_key("copilot", "COPILOT_API_TOKEN")
        .or_else(find_github_token)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Copilot: no GitHub token found. Set COPILOT_API_TOKEN or run `gh auth login`."
            )
        })?;

    let auth_header = format!("token {token}");
    let headers_owned: Vec<(String, String)> = vec![
        ("Authorization".to_string(), auth_header),
        ("Accept".to_string(), "application/json".to_string()),
        ("Editor-Version".to_string(), "vscode/1.96.2".to_string()),
        (
            "Editor-Plugin-Version".to_string(),
            "copilot-chat/0.26.7".to_string(),
        ),
        (
            "User-Agent".to_string(),
            "GitHubCopilotChat/0.26.7".to_string(),
        ),
        ("X-Github-Api-Version".to_string(), "2025-04-01".to_string()),
    ];
    let header_refs: Vec<(&str, &str)> = headers_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let (status, body) =
        http_get("https://api.github.com/copilot_internal/user", &header_refs).await?;

    match status {
        200 => parse_usage_response(&body),
        401 | 403 => bail!(
            "Copilot: GitHub token is invalid or lacks Copilot scope. \
             Set COPILOT_API_TOKEN or run `gh auth login --scopes copilot`."
        ),
        404 => bail!("Copilot: account not found or Copilot not enabled on this account."),
        other => bail!("Copilot: usage API returned HTTP {other}."),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const INDIVIDUAL_PLAN_RESPONSE: &str = r#"{
        "copilot_plan": "individual",
        "token_based_billing": false,
        "quota_reset_date": "2025-02-01",
        "quota_snapshots": {
            "premium_interactions": {
                "entitlement": 300,
                "remaining": 245,
                "percent_remaining": 81.67,
                "quota_id": "premium_interactions",
                "unlimited": false
            },
            "chat": {
                "entitlement": 0,
                "remaining": 0,
                "percent_remaining": 0,
                "quota_id": "chat",
                "unlimited": true
            }
        }
    }"#;

    const BUSINESS_TOKEN_BASED_RESPONSE: &str = r#"{
        "copilot_plan": "business",
        "token_based_billing": true,
        "quota_snapshots": {
            "premium_interactions": {
                "entitlement": 0, "remaining": 0,
                "percent_remaining": 0, "quota_id": "premium_interactions", "unlimited": false
            }
        }
    }"#;

    const BOTH_WINDOWS_RESPONSE: &str = r#"{
        "copilot_plan": "enterprise",
        "token_based_billing": false,
        "quota_reset_date": "2025-02-01T00:00:00Z",
        "quota_snapshots": {
            "premium_interactions": {
                "entitlement": 1000,
                "remaining": 600,
                "percent_remaining": 60.0,
                "quota_id": "premium_interactions",
                "unlimited": false
            },
            "chat": {
                "entitlement": 500,
                "remaining": 100,
                "percent_remaining": 20.0,
                "quota_id": "chat",
                "unlimited": false
            }
        }
    }"#;

    const DERIVED_PERCENT_RESPONSE: &str = r#"{
        "copilot_plan": "individual",
        "token_based_billing": false,
        "quota_reset_date": "2025-02-01",
        "quota_snapshots": {
            "premium_interactions": {
                "entitlement": 300,
                "remaining": 150,
                "quota_id": "premium_interactions",
                "unlimited": false
            }
        }
    }"#;

    #[test]
    fn test_individual_plan_premium_only() {
        let snap = parse_usage_response(INDIVIDUAL_PLAN_RESPONSE).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Individual"));
        assert_eq!(snap.windows.len(), 1);

        let w = &snap.windows[0];
        assert_eq!(w.label, "Premium requests");
        // used = 100 - 81.67 = 18.33%
        assert!((w.used_percent - 18.33).abs() < 0.1);
        assert!(w.resets_at.is_some());
    }

    #[test]
    fn test_business_token_based_empty_windows() {
        // token_based_billing with placeholder quota → no windows, plan still present.
        let snap = parse_usage_response(BUSINESS_TOKEN_BASED_RESPONSE).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Business"));
        assert_eq!(snap.windows.len(), 0);
    }

    #[test]
    fn test_both_windows_present() {
        let snap = parse_usage_response(BOTH_WINDOWS_RESPONSE).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Enterprise"));
        assert_eq!(snap.windows.len(), 2);

        let premium = &snap.windows[0];
        assert_eq!(premium.label, "Premium requests");
        // used = 100 - 60 = 40%
        assert!((premium.used_percent - 40.0).abs() < 0.01);
        assert!(premium.resets_at.is_some());

        let chat = &snap.windows[1];
        assert_eq!(chat.label, "Chat");
        // used = 100 - 20 = 80%
        assert!((chat.used_percent - 80.0).abs() < 0.01);
    }

    #[test]
    fn test_derived_percent_remaining() {
        // percent_remaining absent → derived from remaining/entitlement.
        let snap = parse_usage_response(DERIVED_PERCENT_RESPONSE).unwrap();
        assert_eq!(snap.windows.len(), 1);
        // 150/300 = 50% remaining → 50% used
        assert!((snap.windows[0].used_percent - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_used_percent_clamped() {
        let body = r#"{
            "copilot_plan": "individual",
            "quota_snapshots": {
                "premium_interactions": {
                    "entitlement": 100, "remaining": 0,
                    "percent_remaining": 0, "quota_id": "p", "unlimited": false
                }
            }
        }"#;
        let snap = parse_usage_response(body).unwrap();
        assert_eq!(snap.windows[0].used_percent, 100.0);
    }

    #[test]
    fn test_unknown_fields_tolerated() {
        let body = r#"{
            "copilot_plan": "individual",
            "future_field": "ignored",
            "quota_snapshots": {
                "premium_interactions": {
                    "entitlement": 100, "remaining": 80,
                    "percent_remaining": 80.0, "quota_id": "p",
                    "unlimited": false, "new_unknown_field": true
                }
            }
        }"#;
        let snap = parse_usage_response(body).unwrap();
        assert_eq!(snap.windows.len(), 1);
        assert!((snap.windows[0].used_percent - 20.0).abs() < 0.01);
    }

    #[test]
    fn test_reset_date_parsing() {
        // Bare date
        let dt = parse_reset_date("2025-02-01").unwrap();
        assert_eq!(dt.date_naive().to_string(), "2025-02-01");

        // ISO-8601
        let dt2 = parse_reset_date("2025-02-01T00:00:00Z").unwrap();
        assert_eq!(dt2.date_naive().to_string(), "2025-02-01");

        assert!(parse_reset_date("not-a-date").is_none());
    }
}
