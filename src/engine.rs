use crate::model::{EngineCommand, ProviderDisplay, ProviderState, UiEvent};
use crate::providers::Provider;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Periodically fetches all enabled providers and pushes display state to the UI.
pub async fn run(
    providers: Vec<Arc<dyn Provider>>,
    refresh_secs: u64,
    ui_tx: async_channel::Sender<UiEvent>,
    cmd_rx: async_channel::Receiver<EngineCommand>,
) {
    let mut states: HashMap<&'static str, ProviderState> = providers
        .iter()
        .map(|p| (p.id(), ProviderState::Loading))
        .collect();
    push_state(&providers, &states, &ui_tx).await;

    let mut ticker = tokio::time::interval(Duration::from_secs(refresh_secs.max(30)));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                refresh_all(&providers, &mut states, &ui_tx).await;
            }
            cmd = cmd_rx.recv() => match cmd {
                Ok(EngineCommand::RefreshAll) => {
                    refresh_all(&providers, &mut states, &ui_tx).await;
                    ticker.reset();
                }
                Ok(EngineCommand::Quit) | Err(_) => {
                    let _ = ui_tx.send(UiEvent::Quit).await;
                    return;
                }
            },
        }
    }
}

async fn refresh_all(
    providers: &[Arc<dyn Provider>],
    states: &mut HashMap<&'static str, ProviderState>,
    ui_tx: &async_channel::Sender<UiEvent>,
) {
    let fetches = providers.iter().map(|p| {
        let p = p.clone();
        async move {
            let state = match p.fetch().await {
                Ok(snapshot) => ProviderState::Ready(snapshot),
                Err(err) => {
                    tracing::warn!("{}: fetch failed: {err:#}", p.id());
                    ProviderState::Error(format!("{err:#}"))
                }
            };
            (p.id(), state)
        }
    });
    for (id, state) in futures_join_all(fetches).await {
        states.insert(id, state);
    }
    push_state(providers, states, ui_tx).await;
}

async fn push_state(
    providers: &[Arc<dyn Provider>],
    states: &HashMap<&'static str, ProviderState>,
    ui_tx: &async_channel::Sender<UiEvent>,
) {
    let displays: Vec<ProviderDisplay> = providers
        .iter()
        .map(|p| ProviderDisplay {
            id: p.id(),
            name: p.display_name(),
            state: states
                .get(p.id())
                .cloned()
                .unwrap_or(ProviderState::Loading),
        })
        .collect();
    let _ = ui_tx.send(UiEvent::StateChanged(displays)).await;
}

/// Tiny join_all so we don't pull in the futures crate for one call site.
async fn futures_join_all<F, T>(iter: impl Iterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handles: Vec<_> = iter.map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        if let Ok(v) = h.await {
            out.push(v);
        }
    }
    out
}
