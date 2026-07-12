use crate::model::{Credits, RateWindow, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use tracing::debug;

// HTTP transport shared with the Claude provider.
use super::claude::http_get;

pub struct CursorProvider;

impl CursorProvider {
    pub fn new() -> Self {
        Self
    }
}

// ── session token resolution ──────────────────────────────────────────────────

/// Resolve the `WorkosCursorSessionToken` cookie value.
///
/// Source priority (matches Swift `CursorStatusProbe` fallback chain):
///  1. `CURSOR_SESSION_TOKEN` environment variable / `[keys] cursor` config.
///  2. The locally installed Cursor app's global state DB
///     (`~/.config/Cursor/User/globalStorage/state.vscdb`) — same source the
///     macOS original reads; the cookie is `<userID>%3A%3A<accessToken>`
///     where userID is the last `|`-segment of the JWT `sub` claim.
fn resolve_session_token() -> Option<String> {
    crate::config::api_key("cursor", "CURSOR_SESSION_TOKEN").or_else(app_session_token)
}

fn app_db_path() -> Option<std::path::PathBuf> {
    Some(
        dirs::config_dir()?
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
    )
}

/// Build the session cookie from the Cursor app's own credential store.
fn app_session_token() -> Option<String> {
    let path = app_db_path()?;
    if !path.exists() {
        return None;
    }
    let db =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    db.busy_timeout(std::time::Duration::from_millis(250))
        .ok()?;
    let token: String = db
        .query_row(
            "SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok()?;
    session_cookie_from_jwt(token.trim())
}

/// `<userID>%3A%3A<jwt>` from a Cursor access-token JWT; None when the token
/// is malformed or expires within the next minute.
fn session_cookie_from_jwt(token: &str) -> Option<String> {
    if token.is_empty() {
        return None;
    }
    let payload_b64 = token.split('.').nth(1)?;
    let mut payload = payload_b64.replace('-', "+").replace('_', "/");
    let rem = payload.len() % 4;
    if rem > 0 {
        payload.push_str(&"=".repeat(4 - rem));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&super::gemini::base64_decode(&payload)?).ok()?;

    let exp = json.get("exp").and_then(|v| v.as_i64())?;
    if exp <= chrono::Utc::now().timestamp() + 60 {
        tracing::debug!("cursor: app access token expired");
        return None;
    }
    let user_id = json
        .get("sub")
        .and_then(|v| v.as_str())?
        .split('|')
        .rfind(|s| !s.is_empty())?
        .to_string();
    Some(format!("{user_id}%3A%3A{token}"))
}

// ── response parsing ──────────────────────────────────────────────────────────

/// Parse `/api/usage-summary` response.
///
/// JSON shape (from CursorStatusProbe.swift `parseUsageSummary`):
/// ```json
/// {
///   "billingCycleStart": "2025-01-01T00:00:00.000Z",
///   "billingCycleEnd":   "2025-02-01T00:00:00.000Z",
///   "membershipType": "pro",
///   "individualUsage": {
///     "plan": {
///       "used": 1500, "limit": 2000,
///       "autoPercentUsed": 45.0, "apiPercentUsed": 25.0,
///       "totalPercentUsed": 35.0,
///       "breakdown": { "included": 2000, "bonus": 0, "total": 2000 }
///     },
///     "onDemand": { "used": 350, "limit": 1000 }
///   }
/// }
/// ```
/// All monetary values are in US cents (divide by 100 to get USD).
fn parse_usage_summary_response(
    body: &str,
    account_email: Option<&str>,
) -> anyhow::Result<UsageSnapshot> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("Cursor usage-summary response is not JSON")?;

    let membership_type = json
        .get("membershipType")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let billing_cycle_end = json
        .get("billingCycleEnd")
        .and_then(|v| v.as_str())
        .and_then(parse_iso8601);

    let individual = json.get("individualUsage");
    let plan_obj = individual.and_then(|u| u.get("plan"));

    // Cents → USD helper
    let cents_to_usd = |v: i64| v as f64 / 100.0;

    let plan_used_cents = plan_obj
        .and_then(|p| p.get("used"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let plan_limit_cents = plan_obj
        .and_then(|p| p.get("limit"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // Headline "Total" percent — mirrors Swift precedence order
    let auto_pct = plan_obj
        .and_then(|p| p.get("autoPercentUsed"))
        .and_then(|v| v.as_f64())
        .map(|v| v.clamp(0.0, 100.0));
    let api_pct = plan_obj
        .and_then(|p| p.get("apiPercentUsed"))
        .and_then(|v| v.as_f64())
        .map(|v| v.clamp(0.0, 100.0));
    let total_pct_raw = plan_obj
        .and_then(|p| p.get("totalPercentUsed"))
        .and_then(|v| v.as_f64());

    let plan_percent_used: f64 = if let Some(total) = total_pct_raw {
        total.clamp(0.0, 100.0)
    } else if let (Some(auto), Some(api)) = (auto_pct, api_pct) {
        ((auto + api) / 2.0).clamp(0.0, 100.0)
    } else if let Some(api) = api_pct {
        api
    } else if let Some(auto) = auto_pct {
        auto
    } else if plan_limit_cents > 0 {
        (plan_used_cents as f64 / plan_limit_cents as f64 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };

    // On-demand spend (individual)
    let on_demand_obj = individual.and_then(|u| u.get("onDemand"));
    let on_demand_used_cents = on_demand_obj
        .and_then(|od| od.get("used"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let on_demand_limit_cents = on_demand_obj
        .and_then(|od| od.get("limit"))
        .and_then(|v| v.as_i64());

    // Build windows in display order: Total → Auto → API (named model)
    let mut windows: Vec<RateWindow> = Vec::new();

    let cycle_end = billing_cycle_end;

    windows.push(RateWindow {
        label: "Premium (Monthly)".to_string(),
        used_percent: plan_percent_used,
        resets_at: cycle_end,
    });

    if let Some(auto) = auto_pct {
        windows.push(RateWindow {
            label: "Auto".to_string(),
            used_percent: auto,
            resets_at: cycle_end,
        });
    }

    if let Some(api) = api_pct {
        windows.push(RateWindow {
            label: "API (named model)".to_string(),
            used_percent: api,
            resets_at: cycle_end,
        });
    }

    // On-demand credits: surface when there is a limit or non-zero spend.
    let credits =
        if on_demand_used_cents > 0 || on_demand_limit_cents.map(|l| l > 0).unwrap_or(false) {
            Some(Credits {
                balance: cents_to_usd(on_demand_used_cents),
                currency: Some("USD".to_string()),
            })
        } else {
            None
        };

    let plan = membership_type
        .as_deref()
        .map(format_membership_type)
        .or_else(|| {
            if plan_limit_cents > 0 || plan_used_cents > 0 {
                Some("Cursor".to_string())
            } else {
                None
            }
        });

    debug!("cursor: parsed {} windows", windows.len());

    // Log plan metrics (helpful for debugging)
    debug!(
        "cursor: plan used={:.2} USD  limit={:.2} USD  on_demand used={:.2} USD",
        cents_to_usd(plan_used_cents),
        cents_to_usd(plan_limit_cents),
        cents_to_usd(on_demand_used_cents),
    );

    Ok(UsageSnapshot {
        plan,
        account: account_email.map(|s| s.to_string()),
        windows,
        credits,
        fetched_at: Some(Utc::now()),
    })
}

fn format_membership_type(t: &str) -> String {
    match t.to_lowercase().as_str() {
        "enterprise" => "Cursor Enterprise".to_string(),
        "pro" => "Cursor Pro".to_string(),
        "hobby" => "Cursor Hobby".to_string(),
        "team" => "Cursor Team".to_string(),
        _ => format!("Cursor {}", capitalize(t)),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn parse_iso8601(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for CursorProvider {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn display_name(&self) -> &'static str {
        "Cursor"
    }

    fn is_configured(&self) -> bool {
        resolve_session_token().is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        fetch_usage().await
    }
}

async fn fetch_usage() -> anyhow::Result<UsageSnapshot> {
    let session_token = resolve_session_token().ok_or_else(|| {
        anyhow::anyhow!(
            "Cursor: set CURSOR_SESSION_TOKEN or [keys] cursor in \
             ~/.config/codexbar/config.toml. \
             Copy your WorkosCursorSessionToken cookie from cursor.com."
        )
    })?;

    let cookie_header = format!("WorkosCursorSessionToken={session_token}");

    // Fetch /api/auth/me and /api/usage-summary in parallel.
    let headers_owned: Vec<(String, String)> = vec![
        ("Accept".to_string(), "application/json".to_string()),
        ("Cookie".to_string(), cookie_header.clone()),
    ];
    let header_refs: Vec<(&str, &str)> = headers_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // Fetch user info (best-effort; failure should not block usage display).
    let (me_result, usage_result) = tokio::join!(
        http_get("https://cursor.com/api/auth/me", &header_refs),
        http_get("https://cursor.com/api/usage-summary", &header_refs),
    );

    let email: Option<String> = me_result
        .ok()
        .filter(|(status, _)| *status == 200)
        .and_then(|(_, body)| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|json| {
            json.get("email")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    let (status, body) = usage_result.context("Cursor: usage-summary request failed")?;

    match status {
        200 => parse_usage_summary_response(&body, email.as_deref()),
        401 | 403 => bail!(
            "Cursor: session token rejected. \
             Set CURSOR_SESSION_TOKEN or [keys] cursor in \
             ~/.config/codexbar/config.toml with a fresh WorkosCursorSessionToken cookie."
        ),
        other => bail!("Cursor: usage-summary API returned HTTP {other}."),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn b64url(s: &str) -> String {
        // Tests only need URL-safe chars; build via the std alphabet map.
        let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let bytes = s.as_bytes();
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(table[(n >> 18) as usize & 63] as char);
            out.push(table[(n >> 12) as usize & 63] as char);
            if chunk.len() > 1 {
                out.push(table[(n >> 6) as usize & 63] as char);
            }
            if chunk.len() > 2 {
                out.push(table[n as usize & 63] as char);
            }
        }
        out
    }

    fn fake_jwt(sub: &str, exp: i64) -> String {
        format!(
            "{}.{}.sig",
            b64url(r#"{"alg":"none"}"#),
            b64url(&format!(r#"{{"sub":"{sub}","exp":{exp}}}"#))
        )
    }

    #[test]
    fn session_cookie_from_valid_jwt() {
        let exp = chrono::Utc::now().timestamp() + 3600;
        let jwt = fake_jwt("auth0|user_01ABC", exp);
        let cookie = session_cookie_from_jwt(&jwt).unwrap();
        assert_eq!(cookie, format!("user_01ABC%3A%3A{jwt}"));
    }

    #[test]
    fn session_cookie_rejects_expired_jwt() {
        let jwt = fake_jwt("auth0|user_01ABC", chrono::Utc::now().timestamp() - 10);
        assert!(session_cookie_from_jwt(&jwt).is_none());
    }

    #[test]
    fn session_cookie_rejects_malformed_token() {
        assert!(session_cookie_from_jwt("").is_none());
        assert!(session_cookie_from_jwt("not-a-jwt").is_none());
    }

    const PRO_PLAN_RESPONSE: &str = r#"{
        "billingCycleStart": "2025-01-01T00:00:00.000Z",
        "billingCycleEnd": "2025-02-01T00:00:00.000Z",
        "membershipType": "pro",
        "individualUsage": {
            "plan": {
                "used": 1000,
                "limit": 2000,
                "autoPercentUsed": 40.0,
                "apiPercentUsed": 20.0,
                "totalPercentUsed": 30.0,
                "breakdown": { "included": 2000, "bonus": 0, "total": 2000 }
            },
            "onDemand": { "used": 0, "limit": 0 }
        }
    }"#;

    const NO_PERCENT_RESPONSE: &str = r#"{
        "billingCycleStart": "2025-01-01T00:00:00.000Z",
        "billingCycleEnd": "2025-02-01T00:00:00.000Z",
        "membershipType": "hobby",
        "individualUsage": {
            "plan": {
                "used": 500,
                "limit": 1000
            }
        }
    }"#;

    const WITH_ON_DEMAND_RESPONSE: &str = r#"{
        "billingCycleStart": "2025-01-01T00:00:00.000Z",
        "billingCycleEnd": "2025-02-01T00:00:00.000Z",
        "membershipType": "enterprise",
        "individualUsage": {
            "plan": {
                "used": 3000,
                "limit": 5000,
                "totalPercentUsed": 60.0
            },
            "onDemand": { "used": 2500, "limit": 10000 }
        }
    }"#;

    const PARTIAL_NO_BILLING_CYCLE: &str = r#"{
        "membershipType": "pro",
        "individualUsage": {
            "plan": {
                "used": 100,
                "limit": 200,
                "totalPercentUsed": 50.0
            }
        }
    }"#;

    #[test]
    fn test_pro_plan_happy_path() {
        let snap =
            parse_usage_summary_response(PRO_PLAN_RESPONSE, Some("user@example.com")).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Cursor Pro"));
        assert_eq!(snap.account.as_deref(), Some("user@example.com"));
        // Primary window: totalPercentUsed = 30%
        assert_eq!(snap.windows[0].label, "Premium (Monthly)");
        assert!((snap.windows[0].used_percent - 30.0).abs() < 0.01);
        assert!(snap.windows[0].resets_at.is_some());

        // Auto window
        assert_eq!(snap.windows[1].label, "Auto");
        assert!((snap.windows[1].used_percent - 40.0).abs() < 0.01);

        // API window
        assert_eq!(snap.windows[2].label, "API (named model)");
        assert!((snap.windows[2].used_percent - 20.0).abs() < 0.01);

        // No on-demand spend
        assert!(snap.credits.is_none());
    }

    #[test]
    fn test_no_percent_derived_from_cents() {
        // No totalPercentUsed/auto/api → derived from used/limit ratio
        let snap = parse_usage_summary_response(NO_PERCENT_RESPONSE, None).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Cursor Hobby"));
        // 500 / 1000 = 50%
        assert!((snap.windows[0].used_percent - 50.0).abs() < 0.01);
        assert_eq!(snap.windows.len(), 1); // no auto/api percent
    }

    #[test]
    fn test_on_demand_credits() {
        let snap = parse_usage_summary_response(WITH_ON_DEMAND_RESPONSE, None).unwrap();
        assert_eq!(snap.plan.as_deref(), Some("Cursor Enterprise"));
        assert!((snap.windows[0].used_percent - 60.0).abs() < 0.01);

        let credits = snap.credits.unwrap();
        // 2500 cents = $25.00
        assert!((credits.balance - 25.0).abs() < 0.001);
        assert_eq!(credits.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn test_no_billing_cycle_dates() {
        let snap = parse_usage_summary_response(PARTIAL_NO_BILLING_CYCLE, None).unwrap();
        assert!(snap.windows[0].resets_at.is_none());
    }

    #[test]
    fn test_percent_clamped_to_100() {
        let body = r#"{
            "membershipType": "pro",
            "individualUsage": {
                "plan": { "used": 9999, "limit": 1000, "totalPercentUsed": 150.0 }
            }
        }"#;
        let snap = parse_usage_summary_response(body, None).unwrap();
        assert_eq!(snap.windows[0].used_percent, 100.0);
    }

    #[test]
    fn test_unknown_fields_tolerated() {
        let body = r#"{
            "membershipType": "pro",
            "future_field": true,
            "individualUsage": {
                "plan": { "used": 100, "limit": 1000, "totalPercentUsed": 10.0,
                          "new_unknown": "ignored" }
            }
        }"#;
        let snap = parse_usage_summary_response(body, None).unwrap();
        assert!((snap.windows[0].used_percent - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_format_membership_type() {
        assert_eq!(format_membership_type("pro"), "Cursor Pro");
        assert_eq!(format_membership_type("enterprise"), "Cursor Enterprise");
        assert_eq!(format_membership_type("hobby"), "Cursor Hobby");
        assert_eq!(format_membership_type("team"), "Cursor Team");
        assert_eq!(format_membership_type("business"), "Cursor Business");
    }

    #[test]
    fn test_zero_plan_limit_gives_zero_percent() {
        let body = r#"{
            "membershipType": "pro",
            "individualUsage": { "plan": { "used": 0, "limit": 0 } }
        }"#;
        let snap = parse_usage_summary_response(body, None).unwrap();
        assert_eq!(snap.windows[0].used_percent, 0.0);
    }
}
