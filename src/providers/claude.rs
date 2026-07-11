use crate::model::{Credits, RateWindow, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tracing::{debug, warn};

pub struct ClaudeProvider;

impl ClaudeProvider {
    pub fn new() -> Self {
        Self
    }
}

// ── credential helpers ────────────────────────────────────────────────────────

fn credentials_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".claude").join(".credentials.json"))
}

#[derive(Debug)]
struct Credentials {
    access_token: String,
    refresh_token: String,
    expires_at_ms: i64,
    subscription_type: Option<String>,
    /// The full parsed JSON value so we can round-trip all fields.
    raw: serde_json::Value,
}

fn read_credentials() -> anyhow::Result<Credentials> {
    let path = credentials_path().context("cannot determine home directory")?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let raw: serde_json::Value =
        serde_json::from_str(&text).with_context(|| "credentials.json is not valid JSON")?;

    let oauth = raw
        .get("claudeAiOauth")
        .context("credentials.json missing 'claudeAiOauth'")?;

    let access_token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())
        .context("claudeAiOauth.accessToken missing or not a string")?
        .to_string();
    let refresh_token = oauth
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .context("claudeAiOauth.refreshToken missing or not a string")?
        .to_string();
    let expires_at_ms = oauth
        .get("expiresAt")
        .and_then(|v| v.as_i64())
        .context("claudeAiOauth.expiresAt missing or not a number")?;
    let subscription_type = oauth
        .get("subscriptionType")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(Credentials {
        access_token,
        refresh_token,
        expires_at_ms,
        subscription_type,
        raw,
    })
}

fn write_credentials(
    path: &PathBuf,
    raw: &mut serde_json::Value,
    new_access: &str,
    new_refresh: Option<&str>,
    new_expires_at_ms: i64,
) -> anyhow::Result<()> {
    if let Some(oauth) = raw.get_mut("claudeAiOauth") {
        if let Some(obj) = oauth.as_object_mut() {
            obj.insert(
                "accessToken".to_string(),
                serde_json::Value::String(new_access.to_string()),
            );
            if let Some(r) = new_refresh {
                obj.insert(
                    "refreshToken".to_string(),
                    serde_json::Value::String(r.to_string()),
                );
            }
            obj.insert(
                "expiresAt".to_string(),
                serde_json::Value::Number(serde_json::Number::from(new_expires_at_ms)),
            );
        }
    }

    let serialized = serde_json::to_string_pretty(raw)?;

    // Atomic write: temp file in same directory + rename
    let dir = path.parent().context("credentials path has no parent")?;
    let tmp_path = dir.join(format!(".credentials.json.tmp.{}", std::process::id()));

    std::fs::write(&tmp_path, &serialized)
        .with_context(|| format!("cannot write temp file {}", tmp_path.display()))?;

    // Set file permissions to 0600
    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| "cannot set 0600 permissions on temp credentials file")?;

    std::fs::rename(&tmp_path, path)
        .with_context(|| "cannot atomically replace credentials file")?;

    Ok(())
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

pub(crate) fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("cannot build HTTP client")
}

pub(crate) async fn http_get(url: &str, headers: &[(&str, &str)]) -> anyhow::Result<(u16, String)> {
    let mut req = http_client()?.get(url);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    Ok((status, body))
}

async fn http_post_form(url: &str, form_body: String) -> anyhow::Result<(u16, String)> {
    let resp = http_client()?
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(form_body)
        .send()
        .await
        .with_context(|| format!("POST {url} failed"))?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    Ok((status, body))
}

// ── token refresh ─────────────────────────────────────────────────────────────

const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const OAUTH_REFRESH_URL: &str = "https://platform.claude.com/v1/oauth/token";

async fn refresh_token(creds: &mut Credentials) -> anyhow::Result<()> {
    let form_body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        urlencoded(&creds.refresh_token),
        OAUTH_CLIENT_ID
    );

    let (status, body) = http_post_form(OAUTH_REFRESH_URL, form_body).await?;

    if status != 200 {
        bail!("Claude token expired — run `claude` to re-authenticate.");
    }

    let json: serde_json::Value =
        serde_json::from_str(&body).context("OAuth refresh response is not JSON")?;

    let new_access = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .context("OAuth refresh response missing access_token")?
        .to_string();

    let new_refresh = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let expires_in = json
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);

    let now_ms = Utc::now().timestamp_millis();
    let new_expires_at_ms = now_ms + expires_in * 1000;

    let path = credentials_path().unwrap();
    write_credentials(
        &path,
        &mut creds.raw,
        &new_access,
        new_refresh.as_deref(),
        new_expires_at_ms,
    )?;

    creds.access_token = new_access;
    if let Some(r) = new_refresh {
        creds.refresh_token = r;
    }
    creds.expires_at_ms = new_expires_at_ms;

    Ok(())
}

/// Minimal percent-encoding for OAuth form fields (encodes everything except
/// unreserved chars A-Z a-z 0-9 - _ . ~).
fn urlencoded(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

// ── response parsing ─────────────────────────────────────────────────────────

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn plan_name(subscription_type: Option<&str>) -> Option<String> {
    subscription_type.map(|s| match s.to_lowercase().as_str() {
        "pro" => "Pro".to_string(),
        "max" => "Max".to_string(),
        "team" => "Team".to_string(),
        "enterprise" => "Enterprise".to_string(),
        other => capitalize(other),
    })
}

fn parse_resets_at(v: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    v.and_then(|v| v.as_str()).and_then(|s| {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    })
}

fn parse_usage_response(
    body: &str,
    subscription_type: Option<&str>,
) -> anyhow::Result<UsageSnapshot> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("usage response is not JSON")?;

    let mut windows: Vec<RateWindow> = Vec::new();

    // Try the top-level window fields first
    let named_windows = [
        ("five_hour", "Session"),
        ("seven_day", "Weekly"),
        ("seven_day_opus", "Weekly (Opus)"),
        ("seven_day_sonnet", "Weekly (Sonnet)"),
    ];

    let mut any_top_level = false;
    for (field, label) in &named_windows {
        if let Some(w) = json.get(field) {
            if w.is_null() {
                continue;
            }
            any_top_level = true;
            // Be lossy: skip if utilization missing or not a number
            let utilization = match w.get("utilization").and_then(|v| v.as_f64()) {
                Some(u) => u.clamp(0.0, 100.0),
                None => {
                    warn!("claude: window '{}' missing utilization, skipping", field);
                    continue;
                }
            };
            let resets_at = parse_resets_at(w.get("resets_at"));
            windows.push(RateWindow {
                label: label.to_string(),
                used_percent: utilization,
                resets_at,
            });
        }
    }

    // Merge the limits array. Unscoped session/weekly entries duplicate the
    // top-level windows, so they only count when those are absent; scoped
    // entries (e.g. a per-model weekly limit like "Fable") exist ONLY here
    // and are always added. `is_active` marks the currently binding limit,
    // not validity — never filter on it.
    if let Some(limits) = json.get("limits").and_then(|v| v.as_array()) {
        for limit in limits {
            let kind = limit
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let group = limit
                .get("group")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let percent = match limit.get("percent").and_then(|v| v.as_f64()) {
                Some(p) => p.clamp(0.0, 100.0),
                None => continue,
            };
            let resets_at = parse_resets_at(limit.get("resets_at"));

            let model_display = limit
                .get("scope")
                .and_then(|s| s.get("model"))
                .and_then(|m| m.get("display_name"))
                .and_then(|v| v.as_str());

            let label = match model_display {
                Some(name) => format!("Weekly ({})", name),
                None => {
                    if any_top_level {
                        continue; // unscoped duplicates of five_hour/seven_day
                    }
                    if group == "five_hour" || group == "session" || kind.contains("session") {
                        "Session".to_string()
                    } else {
                        "Weekly".to_string()
                    }
                }
            };

            windows.push(RateWindow {
                label,
                used_percent: percent,
                resets_at,
            });
        }
    }

    // credits from extra_usage
    let credits = json.get("extra_usage").and_then(|eu| {
        let enabled = eu
            .get("is_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            return None;
        }
        let balance = eu.get("used_credits").and_then(|v| v.as_f64())?;
        let currency = eu
            .get("currency")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Some(Credits { balance, currency })
    });

    debug!("claude: parsed {} windows", windows.len());

    Ok(UsageSnapshot {
        plan: plan_name(subscription_type),
        account: None,
        windows,
        credits,
        fetched_at: Some(Utc::now()),
    })
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for ClaudeProvider {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude"
    }

    fn is_configured(&self) -> bool {
        let Some(path) = credentials_path() else {
            return false;
        };
        if !path.exists() {
            return false;
        }
        // Quick check: file is readable and contains claudeAiOauth
        let Ok(text) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            return false;
        };
        json.get("claudeAiOauth").is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        fetch_usage().await
    }
}

async fn fetch_usage() -> anyhow::Result<UsageSnapshot> {
    let mut creds = read_credentials()?;

    // Refresh if token expires within 60 seconds
    let now_ms = Utc::now().timestamp_millis();
    if creds.expires_at_ms <= now_ms + 60_000 {
        debug!("claude: token expiring soon, refreshing");
        refresh_token(&mut creds).await?;
    }

    let subscription_type = creds.subscription_type.as_deref();

    let auth_header = format!("Bearer {}", creds.access_token);
    let headers_owned: Vec<(String, String)> = vec![
        ("Authorization".to_string(), auth_header),
        ("Accept".to_string(), "application/json".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("anthropic-beta".to_string(), "oauth-2025-04-20".to_string()),
        ("User-Agent".to_string(), "claude-code/2.1.0".to_string()),
    ];

    let header_refs: Vec<(&str, &str)> = headers_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let (status, body) =
        http_get("https://api.anthropic.com/api/oauth/usage", &header_refs).await?;

    match status {
        200 => parse_usage_response(&body, subscription_type),
        401 => bail!("Claude session unauthorized — run `claude` to re-authenticate."),
        429 => bail!("Anthropic is rate limiting the usage endpoint — try again in a few minutes."),
        other => bail!("Claude usage: HTTP {}", other),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const HAPPY_PATH_RESPONSE: &str = r#"{
        "five_hour": {
            "utilization": 42.5,
            "resets_at": "2025-01-15T14:00:00Z"
        },
        "seven_day": {
            "utilization": 78.0,
            "resets_at": "2025-01-20T00:00:00Z"
        },
        "seven_day_opus": {
            "utilization": 15.0,
            "resets_at": "2025-01-20T00:00:00Z"
        },
        "seven_day_sonnet": {
            "utilization": 60.0,
            "resets_at": "2025-01-20T00:00:00Z"
        },
        "extra_usage": {
            "is_enabled": true,
            "monthly_limit": 100.0,
            "used_credits": 25.50,
            "utilization": 25.5,
            "currency": "USD"
        }
    }"#;

    const PARTIAL_NULL_RESPONSE: &str = r#"{
        "five_hour": {
            "utilization": 55.0,
            "resets_at": "2025-01-15T14:00:00Z"
        },
        "seven_day": null,
        "seven_day_opus": null,
        "seven_day_sonnet": null
    }"#;

    const LIMITS_FALLBACK_RESPONSE: &str = r#"{
        "limits": [
            {
                "kind": "session_limit",
                "group": "five_hour",
                "percent": 30.0,
                "resets_at": "2025-01-15T14:00:00Z",
                "is_active": true,
                "scope": {}
            },
            {
                "kind": "weekly_limit",
                "group": "seven_day",
                "percent": 65.0,
                "resets_at": "2025-01-20T00:00:00Z",
                "is_active": true,
                "scope": {
                    "model": {
                        "display_name": "Claude Opus 4"
                    }
                }
            },
            {
                "kind": "weekly_limit",
                "group": "seven_day",
                "percent": 20.0,
                "resets_at": "2025-01-20T00:00:00Z",
                "is_active": false,
                "scope": {}
            }
        ]
    }"#;

    #[test]
    fn test_happy_path_windows() {
        let snap = parse_usage_response(HAPPY_PATH_RESPONSE, Some("pro")).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Pro"));
        assert_eq!(snap.windows.len(), 4);

        let session = &snap.windows[0];
        assert_eq!(session.label, "Session");
        assert!((session.used_percent - 42.5).abs() < 0.01);
        assert!(session.resets_at.is_some());

        let weekly = &snap.windows[1];
        assert_eq!(weekly.label, "Weekly");
        assert!((weekly.used_percent - 78.0).abs() < 0.01);

        let opus = &snap.windows[2];
        assert_eq!(opus.label, "Weekly (Opus)");
        assert!((opus.used_percent - 15.0).abs() < 0.01);

        let sonnet = &snap.windows[3];
        assert_eq!(sonnet.label, "Weekly (Sonnet)");

        let credits = snap.credits.unwrap();
        assert!((credits.balance - 25.50).abs() < 0.01);
        assert_eq!(credits.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn test_partial_null_response() {
        let snap = parse_usage_response(PARTIAL_NULL_RESPONSE, Some("max")).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Max"));
        // Only five_hour should produce a window; null windows are skipped
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "Session");
        assert!((snap.windows[0].used_percent - 55.0).abs() < 0.01);
    }

    #[test]
    fn test_limits_fallback() {
        let snap = parse_usage_response(LIMITS_FALLBACK_RESPONSE, Some("team")).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Team"));
        // is_active marks the binding limit, not validity — nothing is skipped.
        assert_eq!(snap.windows.len(), 3);
        assert_eq!(snap.windows[0].label, "Session");
        assert!((snap.windows[0].used_percent - 30.0).abs() < 0.01);
        assert_eq!(snap.windows[1].label, "Weekly (Claude Opus 4)");
        assert!((snap.windows[1].used_percent - 65.0).abs() < 0.01);
        assert_eq!(snap.windows[2].label, "Weekly");
        assert!((snap.windows[2].used_percent - 20.0).abs() < 0.01);
    }

    /// Regression: scoped per-model limits (e.g. the promotional "Fable"
    /// window) live ONLY in `limits` and must be merged even when the
    /// top-level windows are present; their unscoped siblings duplicate
    /// five_hour/seven_day and must not be doubled.
    #[test]
    fn test_scoped_limits_merged_with_top_level_windows() {
        let body = r#"{
            "five_hour": {"utilization": 24.0, "resets_at": "2026-07-12T03:30:00+00:00"},
            "seven_day": {"utilization": 17.0, "resets_at": "2026-07-13T08:00:00+00:00"},
            "limits": [
                {"kind": "session", "group": "session", "percent": 24,
                 "resets_at": "2026-07-12T03:30:00+00:00", "scope": null, "is_active": false},
                {"kind": "weekly_all", "group": "weekly", "percent": 17,
                 "resets_at": "2026-07-13T08:00:00+00:00", "scope": null, "is_active": false},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 26,
                 "resets_at": "2026-07-13T08:00:00+00:00",
                 "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null},
                 "is_active": true}
            ]
        }"#;
        let snap = parse_usage_response(body, Some("pro")).unwrap();
        let labels: Vec<&str> = snap.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, vec!["Session", "Weekly", "Weekly (Fable)"]);
        assert!((snap.windows[2].used_percent - 26.0).abs() < 0.01);
    }

    #[test]
    fn test_plan_capitalization() {
        assert_eq!(plan_name(Some("pro")).as_deref(), Some("Pro"));
        assert_eq!(plan_name(Some("max")).as_deref(), Some("Max"));
        assert_eq!(plan_name(Some("team")).as_deref(), Some("Team"));
        assert_eq!(plan_name(Some("enterprise")).as_deref(), Some("Enterprise"));
        // Unknown type: capitalize raw
        assert_eq!(plan_name(Some("business")).as_deref(), Some("Business"));
        assert_eq!(plan_name(None), None);
    }

    #[test]
    fn test_percent_clamping() {
        let body = r#"{"five_hour": {"utilization": 150.0, "resets_at": null}}"#;
        let snap = parse_usage_response(body, None).unwrap();
        assert_eq!(snap.windows[0].used_percent, 100.0);
    }

    #[test]
    fn test_unknown_fields_tolerated() {
        let body = r#"{
            "five_hour": {"utilization": 10.0, "resets_at": "2025-01-15T14:00:00Z", "unknown_field": "ignored"},
            "future_field": {"whatever": true}
        }"#;
        let snap = parse_usage_response(body, None).unwrap();
        assert_eq!(snap.windows.len(), 1);
        assert!((snap.windows[0].used_percent - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_urlencoded() {
        assert_eq!(urlencoded("hello"), "hello");
        assert_eq!(urlencoded("a+b=c&d"), "a%2Bb%3Dc%26d");
        assert_eq!(urlencoded("abc123-_.~"), "abc123-_.~");
    }
}
