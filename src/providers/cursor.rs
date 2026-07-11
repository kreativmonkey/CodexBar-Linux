use crate::model::UsageSnapshot;
use crate::providers::Provider;

pub struct CursorProvider;

impl CursorProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Provider for CursorProvider {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn display_name(&self) -> &'static str {
        "Cursor"
    }

    fn is_configured(&self) -> bool {
        false
    }

    async fn fetch(&self) -> anyhow::Result<UsageSnapshot> {
        anyhow::bail!("not implemented")
    }
}
