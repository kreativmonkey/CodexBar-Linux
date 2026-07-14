use crate::model::UsageSnapshot;
use crate::providers::Provider;
use anyhow::Context;
use tracing::debug;

pub struct GrokProvider;

impl GrokProvider {
    pub fn new() -> Self {
        Self
    }
}

fn api_key() -> Option<String> {
    crate::config::api_key("grok", "XAI_API_KEY")
        .or_else(|| super::cli_agent_auth::provider_api_key("xai"))
}

// ── Provider impl ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl Provider for GrokProvider {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn display_name(&self) -> &'static str {
        "Grok"
    }

    /// Returns true when `XAI_API_KEY` is set, indicating the user intends to
    /// use xAI / Grok.  Note that usage data cannot be fetched with this key
    /// (see `fetch` for details).
    fn is_configured(&self) -> bool {
        if api_key().is_some() {
            debug!(
                "grok: XAI_API_KEY is present but xAI's public REST API \
                 (api.x.ai) does not expose a usage/billing endpoint. \
                 The macOS reference fetches billing via gRPC-web to grok.com \
                 (browser session cookies) or via `grok agent stdio` (CLI RPC). \
                 Neither path is available without a signed-in grok.com session."
            );
            true
        } else {
            false
        }
    }

    /// xAI does not expose a billing/credits REST endpoint for `XAI_API_KEY`.
    ///
    /// The macOS CodexBar implementation fetches usage via two paths — both
    /// require an authenticated grok.com browser session or the `grok login` CLI:
    ///
    /// 1. `grok agent stdio` (JSON-RPC 2.0 subprocess) → `x.ai/billing` method.
    /// 2. gRPC-web POST to `grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig`
    ///    with a cookie header or short-lived Bearer token from `grok login`.
    ///
    /// The `XAI_API_KEY` is scoped to inference at `api.x.ai` and is not
    /// accepted by either billing surface.  Until xAI publishes a REST billing
    /// endpoint for API keys, this provider always returns an actionable error.
    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        // Validate the key is present so the error message is accurate.
        let _ = api_key().context(
            "Grok API key not set — export XAI_API_KEY or add \
             keys.grok to ~/.config/codexbar/config.toml",
        )?;

        anyhow::bail!(
            "Grok usage is not available via XAI_API_KEY. \
             xAI's public REST API does not expose billing/credits. \
             Sign in to grok.com in Chrome and run `grok login`, or wait \
             until xAI publishes a REST usage endpoint."
        )
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_id() {
        let p = GrokProvider::new();
        assert_eq!(p.id(), "grok");
        assert_eq!(p.display_name(), "Grok");
    }

    #[test]
    fn test_is_configured_without_key() {
        // When XAI_API_KEY is not set, is_configured must be false.
        // We can only guarantee this when the env var is unset; in CI it typically is.
        if std::env::var("XAI_API_KEY").is_err() {
            let p = GrokProvider::new();
            assert!(!p.is_configured());
        }
    }
}
