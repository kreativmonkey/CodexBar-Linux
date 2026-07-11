use crate::model::UsageSnapshot;
use crate::providers::Provider;

pub struct OpenAIProvider;

impl OpenAIProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn display_name(&self) -> &'static str {
        "OpenAI"
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        anyhow::bail!("not implemented")
    }
}
