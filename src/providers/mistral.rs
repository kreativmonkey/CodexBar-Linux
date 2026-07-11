use crate::model::UsageSnapshot;
use crate::providers::Provider;

pub struct MistralProvider;

impl MistralProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Provider for MistralProvider {
    fn id(&self) -> &'static str {
        "mistral"
    }

    fn display_name(&self) -> &'static str {
        "Mistral"
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        anyhow::bail!("not implemented")
    }
}
