use crate::model::UsageSnapshot;
use crate::providers::Provider;

pub struct CopilotProvider;

impl CopilotProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Provider for CopilotProvider {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn display_name(&self) -> &'static str {
        "Copilot"
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        anyhow::bail!("not implemented")
    }
}
