use crate::model::UsageSnapshot;
use crate::providers::Provider;

pub struct GeminiProvider;

impl GeminiProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Provider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn display_name(&self) -> &'static str {
        "Gemini"
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        anyhow::bail!("not implemented")
    }
}
