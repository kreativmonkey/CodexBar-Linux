pub mod claude;
pub mod codex;

use crate::model::UsageSnapshot;
use std::sync::Arc;

/// One AI provider whose usage/limits we can fetch.
///
/// Implementations must be cheap to construct and hold no live connections;
/// `fetch` is called from the tokio runtime every refresh cycle.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Stable machine id, e.g. "claude". Used in config and logs.
    fn id(&self) -> &'static str;
    /// Display name, e.g. "Claude".
    fn display_name(&self) -> &'static str;
    /// Whether local credentials for this provider exist (auto-detect).
    fn is_configured(&self) -> bool;
    /// Fetch a fresh usage snapshot. Errors are shown in the UI verbatim,
    /// so make messages actionable ("Run `claude` to re-authenticate.").
    async fn fetch(&self) -> anyhow::Result<UsageSnapshot>;
}

/// All known providers in display order.
pub fn all_providers() -> Vec<Arc<dyn Provider>> {
    vec![
        Arc::new(claude::ClaudeProvider::new()),
        Arc::new(codex::CodexProvider::new()),
    ]
}

/// Providers to actually poll: the configured subset, or auto-detected.
pub fn enabled_providers(configured: &[String]) -> Vec<Arc<dyn Provider>> {
    let all = all_providers();
    if configured.is_empty() {
        all.into_iter().filter(|p| p.is_configured()).collect()
    } else {
        all.into_iter()
            .filter(|p| configured.iter().any(|c| c == p.id()))
            .collect()
    }
}
