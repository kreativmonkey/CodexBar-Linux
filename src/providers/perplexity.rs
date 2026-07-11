use crate::model::UsageSnapshot;
use crate::providers::Provider;

pub struct PerplexityProvider;

impl PerplexityProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Provider for PerplexityProvider {
    fn id(&self) -> &'static str {
        "perplexity"
    }

    fn display_name(&self) -> &'static str {
        "Perplexity"
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        anyhow::bail!("not implemented")
    }
}
