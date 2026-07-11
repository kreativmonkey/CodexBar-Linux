mod config;
mod engine;
mod model;
mod providers;
mod tray;
mod ui;

use config::Config;
use model::{EngineCommand, UiEvent};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "codexbar=info".into()),
        )
        .init();

    let cfg = Config::load();
    let (providers, auto_detect) = providers::enabled_providers(&cfg.providers);
    if providers.is_empty() {
        tracing::warn!("no providers match the config — popover will be empty");
    }

    // UI events for the GTK main thread; engine events are fanned out below.
    let (ui_tx, ui_rx) = async_channel::unbounded::<UiEvent>();
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<EngineCommand>();

    let refresh_secs = cfg.refresh_secs;
    {
        let ui_tx = ui_tx.clone();
        let cmd_tx = cmd_tx.clone();
        std::thread::Builder::new()
            .name("codexbar-engine".into())
            .spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
                rt.block_on(async move {
                    // Engine publishes on an internal bus; we fan out to the
                    // GTK thread and to the tray icon.
                    let (bus_tx, bus_rx) = async_channel::unbounded::<UiEvent>();
                    let tray = match tray::spawn(ui_tx.clone(), cmd_tx).await {
                        Ok(t) => Some(t),
                        Err(err) => {
                            tracing::warn!("tray unavailable: {err:#}");
                            None
                        }
                    };

                    let forward = {
                        let ui_tx = ui_tx.clone();
                        async move {
                            while let Ok(event) = bus_rx.recv().await {
                                if let (Some(tray), UiEvent::StateChanged(displays)) =
                                    (&tray, &event)
                                {
                                    tray.update(displays).await;
                                }
                                if ui_tx.send(event).await.is_err() {
                                    break;
                                }
                            }
                        }
                    };

                    tokio::join!(
                        engine::run(providers, auto_detect, refresh_secs, bus_tx, cmd_rx),
                        forward,
                    );
                });
            })
            .expect("spawn engine thread");
    }

    ui::run(cfg, ui_rx, cmd_tx)
}
