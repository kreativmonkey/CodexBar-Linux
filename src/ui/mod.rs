use crate::config::Config;
use crate::model::{EngineCommand, UiEvent};

/// Run the GTK application (blocks the main thread until quit).
///
/// Listens on `ui_rx`:
/// - `StateChanged(displays)` -> rebuild popover content
/// - `TogglePopover` -> show/hide the layer-shell popover
/// - `Quit` -> exit the application
pub fn run(
    _cfg: Config,
    _ui_rx: async_channel::Receiver<UiEvent>,
    _cmd_tx: async_channel::Sender<EngineCommand>,
) -> anyhow::Result<()> {
    // TODO: real GTK4 + layer-shell implementation
    Ok(())
}
