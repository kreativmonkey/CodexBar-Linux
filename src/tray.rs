use crate::model::{EngineCommand, ProviderDisplay, UiEvent};

/// Handle to the StatusNotifierItem; lets the engine push icon updates.
pub struct TrayHandle;

/// Spawn the tray item on the current tokio runtime.
///
/// - Left click (Activate) sends `UiEvent::TogglePopover`.
/// - Menu: Refresh -> `EngineCommand::RefreshAll`, Quit -> `EngineCommand::Quit`.
pub async fn spawn(
    _ui_tx: async_channel::Sender<UiEvent>,
    _cmd_tx: async_channel::Sender<EngineCommand>,
) -> anyhow::Result<TrayHandle> {
    // TODO: real ksni implementation
    Ok(TrayHandle)
}

impl TrayHandle {
    /// Re-render the tray icon/tooltip from the latest provider state.
    pub async fn update(&self, _displays: &[ProviderDisplay]) {}
}
