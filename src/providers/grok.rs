use crate::model::UsageSnapshot;
use crate::providers::Provider;

pub struct GrokProvider;

impl GrokProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Provider for GrokProvider {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn display_name(&self) -> &'static str {
        "Grok"
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        anyhow::bail!("not implemented")
    }
}
