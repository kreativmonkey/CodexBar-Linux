use crate::model::{EngineCommand, ProviderDisplay, ProviderState, UiEvent};
use crate::providers::Provider;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Periodically fetches all enabled providers and pushes display state to the UI.
///
/// With `auto_detect`, each cycle re-checks which providers have local
/// credentials — logging in to a CLI makes its provider appear on the next
/// refresh, logging out removes it.
pub async fn run(
    providers: Vec<Arc<dyn Provider>>,
    auto_detect: bool,
    refresh_secs: u64,
    ui_tx: async_channel::Sender<UiEvent>,
    cmd_rx: async_channel::Receiver<EngineCommand>,
) {
    let mut states: HashMap<&'static str, ProviderState> =
        active_providers(&providers, auto_detect)
            .map(|p| (p.id(), ProviderState::Loading))
            .collect();
    push_state(&providers, &states, &ui_tx).await;

    let mut ticker = tokio::time::interval(Duration::from_secs(refresh_secs.max(30)));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                refresh_all(&providers, auto_detect, &mut states, &ui_tx).await;
            }
            cmd = cmd_rx.recv() => match cmd {
                Ok(EngineCommand::RefreshAll) => {
                    refresh_all(&providers, auto_detect, &mut states, &ui_tx).await;
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

fn active_providers(
    providers: &[Arc<dyn Provider>],
    auto_detect: bool,
) -> impl Iterator<Item = &Arc<dyn Provider>> {
    providers
        .iter()
        .filter(move |p| !auto_detect || p.is_configured())
}

async fn refresh_all(
    providers: &[Arc<dyn Provider>],
    auto_detect: bool,
    states: &mut HashMap<&'static str, ProviderState>,
    ui_tx: &async_channel::Sender<UiEvent>,
) {
    let active: Vec<Arc<dyn Provider>> =
        active_providers(providers, auto_detect).cloned().collect();
    // Drop providers whose credentials disappeared since the last cycle.
    states.retain(|id, _| active.iter().any(|p| p.id() == *id));

    let fetches = active.iter().map(|p| {
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
    // Only providers with a state are shown — in auto-detect mode the ones
    // without local credentials have none and stay hidden.
    let displays: Vec<ProviderDisplay> = providers
        .iter()
        .filter_map(|p| {
            states.get(p.id()).map(|state| ProviderDisplay {
                id: p.id(),
                name: p.display_name(),
                state: state.clone(),
            })
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
