use crate::model::UsageSnapshot;
use crate::providers::Provider;

pub struct GroqProvider;

impl GroqProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Provider for GroqProvider {
    fn id(&self) -> &'static str {
        "groq"
    }

    fn display_name(&self) -> &'static str {
        "Groq"
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        anyhow::bail!("not implemented")
    }
}
