use crate::config;
use crate::model::{Credits, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use chrono::Utc;
use reqwest::redirect::Policy;
use serde_json::Value;
use std::time::Duration;

pub struct OpenCodeZenProvider;

impl OpenCodeZenProvider {
    pub fn new() -> Self {
        Self
    }
}

const CONSOLE_ORGS_URL: &str = "https://opencode.ai/console/api/orgs";
const CONSOLE_BILLING_URL: &str = "https://opencode.ai/console/api/billing/status";
const BILLING_SCALE: f64 = 100_000_000.0;
const CONSOLE_SESSION_COOKIE: &str = "__Host-console_session";

fn configured_cookie() -> Option<String> {
    std::env::var("OPENCODE_CONSOLE_COOKIE")
        .ok()
        .and_then(|value| sanitize_cookie_header(&value))
        .or_else(|| {
            config::opencode_zen_config()
                .console_cookie
                .and_then(|value| sanitize_cookie_header(&value))
        })
}

fn configured_workspace_id() -> Option<String> {
    std::env::var("OPENCODE_WORKSPACE_ID")
        .ok()
        .and_then(|value| normalize_workspace_id(&value))
        .or_else(|| {
            config::opencode_zen_config()
                .workspace_id
                .and_then(|value| normalize_workspace_id(&value))
        })
}

fn sanitize_cookie_header(raw: &str) -> Option<String> {
    let cookies: Vec<&str> = raw
        .split(';')
        .filter_map(|part| {
            let part = part.trim();
            let (name, value) = part.split_once('=')?;
            (name == CONSOLE_SESSION_COOKIE && !value.is_empty()).then_some(part)
        })
        .collect();
    (!cookies.is_empty()).then(|| cookies.join("; "))
}

fn normalize_workspace_id(raw: &str) -> Option<String> {
    let value = raw.trim();
    let valid_prefix = value.starts_with("wrk_") || value.starts_with("org_");
    let valid_chars = value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    (valid_prefix && valid_chars).then(|| value.to_string())
}

fn parse_workspace_ids(body: &str) -> Vec<String> {
    let Ok(rows) = serde_json::from_str::<Vec<Value>>(body) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| row.get("id")?.as_str())
        .filter_map(normalize_workspace_id)
        .collect()
}

fn parse_balance(body: &str) -> anyhow::Result<f64> {
    let json: Value = serde_json::from_str(body).context("invalid Console billing JSON")?;
    let billing_mode = json.get("billingMode").and_then(Value::as_str);
    let mode = json.get("mode").and_then(Value::as_str);
    if billing_mode != Some("prepaid") || mode != Some("pay-as-you-go") {
        bail!("workspace does not use supported prepaid Zen billing");
    }

    let raw = json
        .get("balanceMicroCents")
        .context("Console billing response has no balance")?;
    let micro_cents = match raw {
        Value::String(value) => value
            .parse::<f64>()
            .context("invalid Console balance value")?,
        Value::Number(value) => value.as_f64().context("invalid Console balance number")?,
        _ => bail!("invalid Console balance type"),
    };
    if !micro_cents.is_finite() {
        bail!("invalid Console balance value");
    }
    Ok(micro_cents / BILLING_SCALE)
}

fn build_snapshot(balance: f64, workspace_id: String) -> UsageSnapshot {
    UsageSnapshot {
        plan: Some("Zen PAYG".to_string()),
        account: Some(workspace_id),
        credits: Some(Credits::from_balance(balance, Some("$".to_string()))),
        fetched_at: Some(Utc::now()),
        ..Default::default()
    }
}

async fn console_get(
    url: &str,
    cookie: &str,
    workspace_id: Option<&str>,
) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(Policy::none())
        .build()
        .context("cannot build OpenCode Console HTTP client")?;
    let mut request = client
        .get(url)
        .header("Cookie", cookie)
        .header("Accept", "application/json")
        .header("User-Agent", "CodexBar-Linux");
    if let Some(workspace_id) = workspace_id {
        request = request.header("x-org-id", workspace_id);
    }
    let response = request
        .send()
        .await
        .context("OpenCode Console request failed")?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .context("cannot read OpenCode Console response")?;
    match status {
        200 => Ok(body),
        401 => bail!("OpenCode Console session expired — copy a fresh session cookie"),
        403 => bail!("OpenCode Console denied access to this workspace"),
        300..=399 => bail!("OpenCode Console redirected to sign-in — refresh the session cookie"),
        other => bail!("OpenCode Console: HTTP {other}"),
    }
}

#[async_trait::async_trait]
impl Provider for OpenCodeZenProvider {
    fn id(&self) -> &'static str {
        "opencode_zen"
    }

    fn display_name(&self) -> &'static str {
        "OpenCode Zen"
    }

    fn is_configured(&self) -> bool {
        configured_cookie().is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        let cookie = configured_cookie().context(
            "OpenCode Console cookie not set — configure [opencode_zen].console_cookie or OPENCODE_CONSOLE_COOKIE",
        )?;
        let workspace_id = match configured_workspace_id() {
            Some(workspace_id) => workspace_id,
            None => {
                let body = console_get(CONSOLE_ORGS_URL, &cookie, None).await?;
                let workspace_ids = parse_workspace_ids(&body);
                match workspace_ids.as_slice() {
                    [] => bail!("OpenCode Console returned no workspace"),
                    [workspace_id] => workspace_id.clone(),
                    _ => bail!(
                        "OpenCode Console returned multiple workspaces — set [opencode_zen].workspace_id"
                    ),
                }
            }
        };
        let body = console_get(CONSOLE_BILLING_URL, &cookie, Some(&workspace_id)).await?;
        Ok(build_snapshot(parse_balance(&body)?, workspace_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_header_keeps_only_opencode_sessions() {
        assert_eq!(
            sanitize_cookie_header("tracking=no; auth=legacy; __Host-console_session=console"),
            Some("__Host-console_session=console".to_string())
        );
        assert_eq!(sanitize_cookie_header("auth=legacy"), None);
        assert_eq!(sanitize_cookie_header("tracking=no"), None);
    }

    #[test]
    fn workspace_parser_rejects_untrusted_values() {
        assert_eq!(
            parse_workspace_ids(r#"[{"id":"https://evil.invalid"},{"id":"wrk_test-1"}]"#),
            vec!["wrk_test-1"]
        );
        assert_eq!(normalize_workspace_id("wrk_bad/value"), None);
    }

    #[test]
    fn parses_prepaid_console_balance() {
        let body = r#"{
            "billingMode": "prepaid",
            "mode": "pay-as-you-go",
            "balanceMicroCents": "2786781005",
            "availableMicroCents": "9999999999"
        }"#;
        assert!((parse_balance(body).unwrap() - 27.86781005).abs() < 1e-9);
    }

    #[test]
    fn rejects_available_credit_as_balance() {
        let body = r#"{
            "billingMode": "prepaid",
            "mode": "pay-as-you-go",
            "availableMicroCents": "9999999999"
        }"#;
        assert!(parse_balance(body).is_err());
    }

    #[test]
    fn rejects_unsupported_billing_mode() {
        let body = r#"{
            "billingMode": "seat",
            "mode": "invoiceable",
            "balanceMicroCents": "2786781005"
        }"#;
        assert!(parse_balance(body).is_err());
    }

    #[test]
    fn test_provider_metadata() {
        let provider = OpenCodeZenProvider::new();
        assert_eq!(provider.id(), "opencode_zen");
        assert_eq!(provider.display_name(), "OpenCode Zen");
    }
}
