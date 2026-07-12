use crate::model::{RateWindow, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use tracing::debug;

// HTTP client shared with the Claude provider.

pub struct GeminiProvider;

impl GeminiProvider {
    pub fn new() -> Self {
        Self
    }
}

// ── credential helpers ────────────────────────────────────────────────────────

fn credentials_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".gemini").join("oauth_creds.json"))
}

fn settings_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".gemini").join("settings.json"))
}

#[derive(Debug)]
struct OAuthCredentials {
    access_token: Option<String>,
    id_token: Option<String>,
    refresh_token: Option<String>,
    /// Milliseconds since epoch (matches Gemini CLI's `expiry_date` field).
    expiry_date_ms: Option<i64>,
}

fn read_credentials() -> anyhow::Result<OAuthCredentials> {
    let path = credentials_path().context("cannot determine home directory")?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).context("oauth_creds.json is not valid JSON")?;

    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let id_token = json
        .get("id_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let refresh_token = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Gemini CLI stores expiry_date as epoch-milliseconds (floating-point Number).
    let expiry_date_ms = json
        .get("expiry_date")
        .and_then(|v| v.as_f64())
        .map(|f| f as i64);

    Ok(OAuthCredentials {
        access_token,
        id_token,
        refresh_token,
        expiry_date_ms,
    })
}

/// Auth type detected from settings.json.
#[derive(Debug, PartialEq)]
enum AuthType {
    OAuthPersonal,
    ApiKey,
    VertexAi,
    Unknown,
}

fn current_auth_type() -> AuthType {
    let Some(path) = settings_path() else {
        return AuthType::Unknown;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return AuthType::Unknown;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return AuthType::Unknown;
    };
    let selected_type = json
        .get("security")
        .and_then(|s| s.get("auth"))
        .and_then(|a| a.get("selectedType"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match selected_type {
        "api-key" => AuthType::ApiKey,
        "oauth-personal" => AuthType::OAuthPersonal,
        "vertex-ai" => AuthType::VertexAi,
        _ => AuthType::Unknown,
    }
}

// ── OAuth token refresh ───────────────────────────────────────────────────────

/// Gemini CLI's public OAuth client credentials — extracted from the
/// installed `@google/gemini-cli` bundle (an "installed app" OAuth client;
/// shipped in the open-source CLI, not a secret). Verified against
/// oauth2.googleapis.com with a live refresh token.
const GEMINI_OAUTH_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GEMINI_OAUTH_CLIENT_SECRET: &str = "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";
const GEMINI_TOKEN_REFRESH_URL: &str = "https://oauth2.googleapis.com/token";

async fn refresh_access_token(refresh_token: &str) -> anyhow::Result<String> {
    // Allow env-var override for testability and enterprise setups.
    let client_id = std::env::var("GEMINI_OAUTH_CLIENT_ID")
        .unwrap_or_else(|_| GEMINI_OAUTH_CLIENT_ID.to_string());
    let client_secret = std::env::var("GEMINI_OAUTH_CLIENT_SECRET")
        .unwrap_or_else(|_| GEMINI_OAUTH_CLIENT_SECRET.to_string());

    let body = format!(
        "client_id={}&client_secret={}&refresh_token={}&grant_type=refresh_token",
        urlencoded(&client_id),
        urlencoded(&client_secret),
        urlencoded(refresh_token),
    );

    let (status, resp_body) = http_post_form(GEMINI_TOKEN_REFRESH_URL, body).await?;

    if status != 200 {
        bail!("Gemini: token refresh failed (HTTP {status}). Run `gemini` to log in.");
    }

    let json: serde_json::Value =
        serde_json::from_str(&resp_body).context("Gemini: token refresh response is not JSON")?;

    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .context("Gemini: token refresh response missing access_token")?
        .to_string();

    debug!("gemini: token refreshed successfully");
    Ok(access_token)
}

async fn http_post_form(url: &str, body: String) -> anyhow::Result<(u16, String)> {
    let client = super::claude::http_client()?;
    let resp = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await
        .with_context(|| format!("POST {url} failed"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}

async fn http_post_json(
    url: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> anyhow::Result<(u16, String)> {
    let client = super::claude::http_client()?;
    let mut req = client.post(url).body(body.to_string());
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("POST {url} failed"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}

fn urlencoded(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

// ── JWT email extraction ──────────────────────────────────────────────────────

fn extract_email_from_id_token(id_token: Option<&str>) -> Option<String> {
    let token = id_token?;
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let mut payload = parts[1].replace('-', "+").replace('_', "/");
    let rem = payload.len() % 4;
    if rem > 0 {
        payload.push_str(&"=".repeat(4 - rem));
    }
    let data = base64_decode(&payload)?;
    let json: serde_json::Value = serde_json::from_slice(&data).ok()?;
    json.get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Minimal base64 decoder (standard alphabet, padded input).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [0u8; 256];
    for (i, &c) in alphabet.iter().enumerate() {
        table[c as usize] = i as u8;
    }
    // Keep padding chars so we can chunk into complete groups of 4.
    let bytes: Vec<u8> = s.bytes().collect();
    // Strip trailing '=' to calculate effective byte count.
    let non_pad: Vec<u8> = bytes.iter().copied().filter(|&b| b != b'=').collect();
    if non_pad.is_empty() {
        return Some(vec![]);
    }
    let mut out = Vec::with_capacity(non_pad.len() * 3 / 4);
    for chunk in non_pad.chunks(4) {
        let a = table[chunk[0] as usize] as u32;
        let b = chunk.get(1).map(|&x| table[x as usize] as u32).unwrap_or(0);
        let c = chunk.get(2).map(|&x| table[x as usize] as u32).unwrap_or(0);
        let d = chunk.get(3).map(|&x| table[x as usize] as u32).unwrap_or(0);
        let n = (a << 18) | (b << 12) | (c << 6) | d;
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

// ── API response parsing ──────────────────────────────────────────────────────

/// Classify a model ID into one of the three tiers used by the Swift reference.
fn model_tier(model_id: &str) -> &'static str {
    let id = model_id.to_lowercase();
    if id.contains("flash-lite") {
        "flash-lite"
    } else if id.contains("flash") {
        "flash"
    } else if id.contains("pro") {
        "pro"
    } else {
        "other"
    }
}

fn parse_quota_response(
    body: &str,
    email: Option<&str>,
    plan: Option<&str>,
) -> anyhow::Result<UsageSnapshot> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("Gemini quota response is not JSON")?;

    let buckets = json
        .get("buckets")
        .and_then(|v| v.as_array())
        .context("Gemini quota response missing 'buckets'")?;

    if buckets.is_empty() {
        bail!("Gemini: no quota buckets returned. Run `gemini` to re-authenticate.");
    }

    // Group by model, keep lowest remainingFraction per model (worst case).
    let mut model_map: std::collections::HashMap<String, (f64, Option<String>)> =
        std::collections::HashMap::new();

    for bucket in buckets {
        let model_id = match bucket.get("modelId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let fraction = match bucket.get("remainingFraction").and_then(|v| v.as_f64()) {
            Some(f) => f,
            None => continue,
        };
        let reset_time = bucket
            .get("resetTime")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        model_map
            .entry(model_id)
            .and_modify(|(existing, existing_reset)| {
                if fraction < *existing {
                    *existing = fraction;
                    *existing_reset = reset_time.clone();
                }
            })
            .or_insert((fraction, reset_time));
    }

    // Find worst (lowest remaining) per tier: pro, flash, flash-lite.
    let mut best_by_tier: std::collections::HashMap<&'static str, (f64, Option<String>)> =
        std::collections::HashMap::new();

    for (model_id, (fraction, reset_str)) in &model_map {
        let tier = model_tier(model_id);
        best_by_tier
            .entry(tier)
            .and_modify(|(existing, existing_reset)| {
                if fraction < existing {
                    *existing = *fraction;
                    *existing_reset = reset_str.clone();
                }
            })
            .or_insert((*fraction, reset_str.clone()));
    }

    let mut windows: Vec<RateWindow> = Vec::new();

    // Emit in fixed order: Pro → Flash → Flash Lite
    for (tier, label) in &[
        ("pro", "Pro"),
        ("flash", "Flash"),
        ("flash-lite", "Flash Lite"),
    ] {
        if let Some((fraction, reset_str)) = best_by_tier.get(*tier) {
            let used_percent = ((1.0 - fraction.clamp(0.0, 1.0)) * 100.0).clamp(0.0, 100.0);
            let resets_at = reset_str.as_deref().and_then(parse_iso8601);
            windows.push(RateWindow {
                label: label.to_string(),
                used_percent,
                resets_at,
            });
        }
    }

    if windows.is_empty() {
        bail!("Gemini: no recognisable model quotas (pro/flash/flash-lite) returned.");
    }

    debug!("gemini: parsed {} windows", windows.len());

    Ok(UsageSnapshot {
        plan: plan.map(|s| s.to_string()),
        account: email.map(|s| s.to_string()),
        windows,
        credits: None,
        fetched_at: Some(Utc::now()),
    })
}

fn parse_iso8601(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn display_name(&self) -> &'static str {
        "Gemini"
    }

    fn is_configured(&self) -> bool {
        let Some(path) = credentials_path() else {
            return false;
        };
        if !path.exists() {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            return false;
        };
        // Needs a non-empty refresh_token to be able to (re-)authenticate.
        json.get("refresh_token")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        fetch_usage().await
    }
}

async fn fetch_usage() -> anyhow::Result<UsageSnapshot> {
    match current_auth_type() {
        AuthType::ApiKey => {
            bail!("Gemini: API-key auth is not supported here. Use OAuth (run `gemini` to log in).")
        }
        AuthType::VertexAi => {
            bail!(
                "Gemini: Vertex AI auth is not supported here. Use OAuth (run `gemini` to log in)."
            )
        }
        AuthType::OAuthPersonal | AuthType::Unknown => {}
    }

    let mut creds =
        read_credentials().map_err(|_| anyhow::anyhow!("Gemini: run `gemini` to log in."))?;

    // Refresh if no access token or if token is expired.
    let now_ms = Utc::now().timestamp_millis();
    let needs_refresh = creds.access_token.is_none()
        || creds
            .expiry_date_ms
            .map(|exp| exp < now_ms)
            .unwrap_or(false);

    if needs_refresh {
        debug!("gemini: access token missing or expired, refreshing");
        let refresh_token = creds
            .refresh_token
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Gemini: no refresh token. Run `gemini` to log in."))?;
        let new_token = refresh_access_token(refresh_token).await?;
        creds.access_token = Some(new_token);
        // We do NOT write back to the file; the token is in-memory only (Swift reference
        // does write back, but skipping it here is safe — Gemini CLI will refresh again
        // on next CLI invocation, and so will we on next poll cycle).
    }

    let access_token = creds
        .access_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Gemini: no access token. Run `gemini` to log in."))?;

    let email = extract_email_from_id_token(creds.id_token.as_deref());

    // Step 1: loadCodeAssist to get project ID and tier for accurate quota.
    let (project_id, plan) = load_code_assist_status(access_token)
        .await
        .unwrap_or((None, None));

    // Step 2: retrieveUserQuota (POST)
    let auth_header = format!("Bearer {access_token}");
    let headers_owned: Vec<(String, String)> = vec![
        ("Authorization".to_string(), auth_header),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let header_refs: Vec<(&str, &str)> = headers_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let request_body = match &project_id {
        Some(pid) => format!("{{\"project\": \"{pid}\"}}"),
        None => "{}".to_string(),
    };

    let (status, body) = http_post_json(
        "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota",
        &request_body,
        &header_refs,
    )
    .await?;

    match status {
        200 => parse_quota_response(&body, email.as_deref(), plan.as_deref()),
        401 => bail!("Gemini: session expired. Run `gemini` to log in."),
        other => bail!("Gemini: quota API returned HTTP {other}."),
    }
}

/// Call `loadCodeAssist` to discover the project ID and plan tier.
async fn load_code_assist_status(access_token: &str) -> Option<(Option<String>, Option<String>)> {
    let auth_header = format!("Bearer {access_token}");
    let body = r#"{"metadata":{"ideType":"GEMINI_CLI","pluginType":"GEMINI"}}"#;
    let headers_owned: Vec<(String, String)> = vec![
        ("Authorization".to_string(), auth_header),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let header_refs: Vec<(&str, &str)> = headers_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let (status, resp_body) = http_post_json(
        "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist",
        body,
        &header_refs,
    )
    .await
    .ok()?;

    if status != 200 {
        return None;
    }

    let json: serde_json::Value = serde_json::from_str(&resp_body).ok()?;

    // cloudaicompanionProject can be a String or an object with id/projectId.
    let project_id: Option<String> = {
        let raw = json.get("cloudaicompanionProject");
        if let Some(s) = raw.and_then(|v| v.as_str()) {
            Some(s.to_string()).filter(|s| !s.is_empty())
        } else {
            raw.and_then(|v| {
                v.get("id")
                    .or_else(|| v.get("projectId"))
                    .and_then(|v2| v2.as_str())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
            })
        }
    };

    let tier_id = json
        .get("currentTier")
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // paidTier.name is the most specific plan label.
    let paid_tier_name = json
        .get("paidTier")
        .and_then(|pt| pt.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let plan = if let Some(name) = paid_tier_name {
        Some(name)
    } else {
        match tier_id {
            "standard-tier" => Some("Paid".to_string()),
            "free-tier" => Some("Free".to_string()),
            "legacy-tier" => Some("Legacy".to_string()),
            _ => None,
        }
    };

    Some((project_id, plan))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const HAPPY_PATH_RESPONSE: &str = r#"{
        "buckets": [
            {"modelId": "gemini-2.5-pro", "remainingFraction": 0.7, "resetTime": "2025-01-15T14:00:00Z", "tokenType": "input"},
            {"modelId": "gemini-2.5-pro", "remainingFraction": 0.8, "resetTime": "2025-01-15T14:00:00Z", "tokenType": "output"},
            {"modelId": "gemini-2.5-flash", "remainingFraction": 0.5, "resetTime": "2025-01-15T14:00:00Z", "tokenType": "input"},
            {"modelId": "gemini-2.0-flash-lite", "remainingFraction": 0.9, "resetTime": "2025-01-15T14:00:00Z", "tokenType": "input"}
        ]
    }"#;

    const PRO_ONLY_RESPONSE: &str = r#"{
        "buckets": [
            {"modelId": "gemini-2.5-pro-preview-0506", "remainingFraction": 0.42, "resetTime": "2025-01-15T14:00:00Z"}
        ]
    }"#;

    const EMPTY_BUCKETS_RESPONSE: &str = r#"{"buckets": []}"#;

    const MISSING_MODEL_ID: &str = r#"{
        "buckets": [
            {"remainingFraction": 0.5, "resetTime": "2025-01-15T14:00:00Z"},
            {"modelId": "gemini-2.5-pro", "remainingFraction": 0.6, "resetTime": "2025-01-15T14:00:00Z"}
        ]
    }"#;

    #[test]
    fn test_happy_path_three_tiers() {
        let snap =
            parse_quota_response(HAPPY_PATH_RESPONSE, Some("test@example.com"), Some("Free"))
                .unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Free"));
        assert_eq!(snap.account.as_deref(), Some("test@example.com"));
        // Pro, Flash, Flash Lite all present
        assert_eq!(snap.windows.len(), 3);

        assert_eq!(snap.windows[0].label, "Pro");
        // lowest fraction for pro is 0.7 → used = 30%
        assert!((snap.windows[0].used_percent - 30.0).abs() < 0.01);
        assert!(snap.windows[0].resets_at.is_some());

        assert_eq!(snap.windows[1].label, "Flash");
        // flash fraction = 0.5 → used = 50%
        assert!((snap.windows[1].used_percent - 50.0).abs() < 0.01);

        assert_eq!(snap.windows[2].label, "Flash Lite");
        // flash-lite fraction = 0.9 → used = 10%
        assert!((snap.windows[2].used_percent - 10.0).abs() < 0.01);

        assert!(snap.credits.is_none());
    }

    #[test]
    fn test_pro_only() {
        let snap = parse_quota_response(PRO_ONLY_RESPONSE, None, Some("Paid")).unwrap();
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "Pro");
        // 0.42 remaining → 58% used
        assert!((snap.windows[0].used_percent - 58.0).abs() < 0.01);
    }

    #[test]
    fn test_empty_buckets_errors() {
        assert!(parse_quota_response(EMPTY_BUCKETS_RESPONSE, None, None).is_err());
    }

    #[test]
    fn test_missing_model_id_skipped() {
        // Bucket without modelId is skipped; the pro bucket still parses.
        let snap = parse_quota_response(MISSING_MODEL_ID, None, None).unwrap();
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "Pro");
    }

    #[test]
    fn test_worst_fraction_per_model_kept() {
        // Two buckets for the same model — lowest fraction drives the window.
        let body = r#"{
            "buckets": [
                {"modelId": "gemini-2.5-pro", "remainingFraction": 0.7, "resetTime": "2025-01-15T14:00:00Z"},
                {"modelId": "gemini-2.5-pro", "remainingFraction": 0.95, "resetTime": "2025-01-15T14:00:00Z"}
            ]
        }"#;
        let snap = parse_quota_response(body, None, None).unwrap();
        assert_eq!(snap.windows.len(), 1);
        assert!((snap.windows[0].used_percent - 30.0).abs() < 0.01);
    }

    #[test]
    fn test_used_percent_clamped() {
        // remainingFraction > 1.0 → clamp to 0% used.
        let body = r#"{
            "buckets": [
                {"modelId": "gemini-2.5-pro", "remainingFraction": 1.5, "resetTime": "2025-01-15T14:00:00Z"}
            ]
        }"#;
        let snap = parse_quota_response(body, None, None).unwrap();
        assert_eq!(snap.windows[0].used_percent, 0.0);
    }

    #[test]
    fn test_model_tier_classification() {
        assert_eq!(model_tier("gemini-2.5-pro"), "pro");
        assert_eq!(model_tier("gemini-2.5-flash"), "flash");
        assert_eq!(model_tier("gemini-2.0-flash-lite"), "flash-lite");
        assert_eq!(model_tier("gemini-1.5-pro-001"), "pro");
        assert_eq!(model_tier("gemini-1.5-flash-8b"), "flash");
    }

    #[test]
    fn test_base64_decode() {
        // "hello" in base64 = "aGVsbG8="
        let decoded = base64_decode("aGVsbG8=").unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn test_unknown_fields_tolerated() {
        let body = r#"{
            "buckets": [
                {"modelId": "gemini-2.5-pro", "remainingFraction": 0.5, "resetTime": "2025-01-15T14:00:00Z",
                 "unknownNewField": true}
            ],
            "futureField": "ignored"
        }"#;
        let snap = parse_quota_response(body, None, None).unwrap();
        assert_eq!(snap.windows.len(), 1);
    }
}
