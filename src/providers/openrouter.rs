use crate::model::UsageSnapshot;
use crate::providers::Provider;

pub struct OpenRouterProvider;

impl OpenRouterProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Provider for OpenRouterProvider {
    fn id(&self) -> &'static str {
        "openrouter"
    }

    fn display_name(&self) -> &'static str {
        "OpenRouter"
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        anyhow::bail!("not implemented")
    }
}
