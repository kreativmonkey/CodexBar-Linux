use crate::model::{RateWindow, UsageSnapshot};
use crate::providers::Provider;
use anyhow::{bail, Context};
use tracing::debug;

// HTTP transport shared with other providers.
use super::claude::http_get;

/// Percent-encode a string for use in a URL query parameter.
/// Only encodes characters that are not unreserved (RFC 3986).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            other => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{:02X}", other);
            }
        }
    }
    out
}

pub struct GroqProvider;

impl GroqProvider {
    pub fn new() -> Self {
        Self
    }
}

fn api_key() -> Option<String> {
    crate::config::api_key("groq", "GROQ_API_KEY")
}

// ── Prometheus query helpers ──────────────────────────────────────────────────

/// Base URL for the Groq Prometheus metrics API.
const GROQ_METRICS_BASE: &str = "https://api.groq.com/v1/metrics/prometheus/api/v1/query";

/// Parses a Prometheus instant-query scalar response.
///
/// The response shape is:
/// ```json
/// {
///   "status": "success",
///   "data": {
///     "result": [
///       { "value": [<timestamp>, "<value_string>"] }
///     ]
///   }
/// }
/// ```
/// We sum all series values (matches `sum(…)` queries) and default to 0 if no
/// series are present (metric not yet recorded for this account).
fn parse_prometheus_scalar(body: &str) -> anyhow::Result<f64> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("Groq Prometheus response is not JSON")?;

    let status = json
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    if status != "success" {
        let error_msg = json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        bail!("Groq Prometheus query failed: {error_msg}");
    }

    let result = json
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_array());

    let Some(result) = result else {
        return Ok(0.0);
    };

    // Each series has a `value` array: [timestamp, "value_string"].
    // We take the last element of each value array (the actual metric value).
    let sum = result.iter().fold(0.0_f64, |acc, series| {
        let value = series
            .get("value")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.last())
            .and_then(|v| {
                // Could be a JSON number or a string.
                v.as_f64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0.0);
        acc + value
    });

    Ok(sum)
}

/// Query a single Prometheus metric and return the scalar value.
async fn query_metric(query: &str, api_key: &str) -> anyhow::Result<f64> {
    // URL-encode the query string manually (only `(`, `)`, `:`, space need encoding here).
    let encoded_query = url_encode(query);
    let url = format!("{GROQ_METRICS_BASE}?query={encoded_query}");

    let auth_header = format!("Bearer {api_key}");
    let headers = [
        ("Authorization", auth_header.as_str()),
        ("Accept", "application/json"),
    ];

    let (status, body) = http_get(&url, &headers).await?;

    match status {
        200 => parse_prometheus_scalar(&body),
        401 | 403 => bail!("Groq API key invalid or expired — check GROQ_API_KEY."),
        429 => bail!("Groq rate-limited; try again shortly."),
        other => bail!("Groq Prometheus query: HTTP {other}"),
    }
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for GroqProvider {
    fn id(&self) -> &'static str {
        "groq"
    }

    fn display_name(&self) -> &'static str {
        "Groq"
    }

    fn is_configured(&self) -> bool {
        api_key().is_some()
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        let key = api_key().context(
            "Groq API key not set — export GROQ_API_KEY or add \
             keys.groq to ~/.config/codexbar/config.toml",
        )?;

        // Fetch all four 5-minute-rate metrics in parallel.
        // These are the same PromQL expressions used by the macOS Swift reference.
        let (requests_res, tokens_in_res, tokens_out_res, cache_hits_res) = tokio::join!(
            query_metric("sum(model_project_id_status_code:requests:rate5m)", &key),
            query_metric("sum(model_project_id:tokens_in:rate5m)", &key),
            query_metric("sum(model_project_id:tokens_out:rate5m)", &key),
            query_metric("sum(model_project_id:prompt_cache_hits:rate5m)", &key),
        );

        // Propagate errors from required queries (requests/tokens).
        let requests_per_sec = requests_res?;
        let tokens_in_per_sec = tokens_in_res?;
        let tokens_out_per_sec = tokens_out_res?;
        // Cache hits are optional; default to 0 on failure.
        let cache_hits_per_sec = cache_hits_res.unwrap_or(0.0);

        let requests_per_min = requests_per_sec * 60.0;
        let tokens_per_min = (tokens_in_per_sec + tokens_out_per_sec) * 60.0;
        let cache_hits_per_min = cache_hits_per_sec * 60.0;

        debug!(
            "groq: req/min={requests_per_min:.2} tok/min={tokens_per_min:.2} \
             cache/min={cache_hits_per_min:.2}"
        );

        // Groq has no hard per-key rate limits exposed via this API — these are
        // throughput metrics, not quota utilisation percentages.  We present them
        // as informational windows with used_percent=0.
        let mut windows = vec![
            RateWindow {
                label: format!("{} req/min", format_decimal(requests_per_min)),
                used_percent: 0.0,
                resets_at: None,
            },
            RateWindow {
                label: format!("{} tok/min", format_decimal(tokens_per_min)),
                used_percent: 0.0,
                resets_at: None,
            },
        ];

        if cache_hits_per_min > 0.0 {
            windows.push(RateWindow {
                label: format!("{} cache/min", format_decimal(cache_hits_per_min)),
                used_percent: 0.0,
                resets_at: None,
            });
        }

        Ok(UsageSnapshot {
            windows,
            fetched_at: Some(chrono::Utc::now()),
            ..Default::default()
        })
    }
}

/// Formats a rate value with adaptive decimal places (matches Swift reference).
fn format_decimal(value: f64) -> String {
    if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SCALAR_RESPONSE_NONZERO: &str = r#"{
        "status": "success",
        "data": {
            "resultType": "vector",
            "result": [
                {
                    "metric": {},
                    "value": [1700000000, "2.5"]
                }
            ]
        }
    }"#;

    const SCALAR_RESPONSE_ZERO: &str = r#"{
        "status": "success",
        "data": {
            "resultType": "vector",
            "result": []
        }
    }"#;

    const SCALAR_MULTI_SERIES: &str = r#"{
        "status": "success",
        "data": {
            "resultType": "vector",
            "result": [
                { "value": [1700000000, "1.0"] },
                { "value": [1700000001, "2.0"] },
                { "value": [1700000002, "0.5"] }
            ]
        }
    }"#;

    const ERROR_RESPONSE: &str = r#"{
        "status": "error",
        "errorType": "bad_data",
        "error": "unknown metric name"
    }"#;

    #[test]
    fn test_parse_nonzero_scalar() {
        let value = parse_prometheus_scalar(SCALAR_RESPONSE_NONZERO).unwrap();
        assert!((value - 2.5).abs() < 0.001);
    }

    #[test]
    fn test_parse_empty_result_returns_zero() {
        let value = parse_prometheus_scalar(SCALAR_RESPONSE_ZERO).unwrap();
        assert_eq!(value, 0.0);
    }

    #[test]
    fn test_parse_multi_series_summed() {
        let value = parse_prometheus_scalar(SCALAR_MULTI_SERIES).unwrap();
        // 1.0 + 2.0 + 0.5 = 3.5
        assert!((value - 3.5).abs() < 0.001);
    }

    #[test]
    fn test_parse_error_status() {
        let result = parse_prometheus_scalar(ERROR_RESPONSE);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unknown metric name"));
    }

    #[test]
    fn test_format_decimal() {
        assert_eq!(format_decimal(0.12), "0.12");
        assert_eq!(format_decimal(9.99), "9.99");
        assert_eq!(format_decimal(10.0), "10.0");
        assert_eq!(format_decimal(99.9), "99.9");
        assert_eq!(format_decimal(100.0), "100");
        assert_eq!(format_decimal(1234.5), "1234");
    }

    #[test]
    fn test_unknown_fields_tolerated() {
        let body = r#"{
            "status": "success",
            "extra": "ignored",
            "data": {
                "resultType": "vector",
                "result": [
                    { "metric": {"model": "llama3"}, "value": [1234, "5.0"], "extra_field": true }
                ]
            }
        }"#;
        let value = parse_prometheus_scalar(body).unwrap();
        assert!((value - 5.0).abs() < 0.001);
    }
}
