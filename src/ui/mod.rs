use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Local, Utc};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GBox, Button, CssProvider, EventControllerKey, Label,
    LevelBar, Orientation, Separator,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::config::{config_path, Config};
use crate::model::{EngineCommand, ProviderDisplay, ProviderState, UiEvent};

const STYLE: &str = include_str!("style.css");

/// Run the GTK application (blocks the main thread until quit).
///
/// Listens on `ui_rx`:
/// - `StateChanged(displays)` -> rebuild popover content
/// - `TogglePopover` -> show/hide the layer-shell popover
/// - `Quit` -> exit the application
pub fn run(
    cfg: Config,
    ui_rx: async_channel::Receiver<UiEvent>,
    cmd_tx: async_channel::Sender<EngineCommand>,
) -> anyhow::Result<()> {
    let app = Application::builder()
        .application_id("dev.sebastian.codexbar")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let cfg = Rc::new(cfg);

    app.connect_activate(move |app| {
        // Keep the app alive even without visible windows.
        let _hold = app.hold();

        // Load CSS.
        let provider = CssProvider::new();
        provider.load_from_data(STYLE);
        gtk4::style_context_add_provider_for_display(
            &gtk4::gdk::Display::default().expect("no GDK display"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        // Create the layer-shell popover window.
        let window = ApplicationWindow::builder()
            .application(app)
            .default_width(340)
            .resizable(false)
            .decorated(false)
            .build();

        // Set up layer-shell before the window is realized.
        window.init_layer_shell();
        window.set_layer(Layer::Top);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Right, true);
        window.set_margin(Edge::Top, cfg.popover_margin_top);
        window.set_margin(Edge::Right, cfg.popover_margin_right);
        window.set_keyboard_mode(KeyboardMode::OnDemand);
        window.set_namespace(Some("codexbar"));

        // Shared provider state.
        let state: Rc<RefCell<Vec<ProviderDisplay>>> = Rc::new(RefCell::new(Vec::new()));

        // Content box (card).
        let card = GBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(0)
            .css_classes(["card"])
            .build();
        window.set_child(Some(&card));

        // Esc hides the popover.
        let key_ctrl = EventControllerKey::new();
        {
            let window_weak = window.downgrade();
            key_ctrl.connect_key_pressed(move |_, key, _, _| {
                if key == gtk4::gdk::Key::Escape {
                    if let Some(w) = window_weak.upgrade() {
                        w.set_visible(false);
                    }
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
        window.add_controller(key_ctrl);

        // Periodic refresh of countdown labels (every 30 s).
        {
            let state_weak = Rc::downgrade(&state);
            let card_weak = card.downgrade();
            let window_weak = window.downgrade();
            let cmd_tx_timer = cmd_tx.clone();
            glib::timeout_add_local(std::time::Duration::from_secs(30), move || {
                let Some(window) = window_weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if window.is_visible() {
                    if let (Some(st), Some(c)) = (state_weak.upgrade(), card_weak.upgrade()) {
                        rebuild_card(&c, &st.borrow(), &cmd_tx_timer);
                    }
                }
                glib::ControlFlow::Continue
            });
        }

        // Event loop consuming ui_rx.
        {
            let state = Rc::clone(&state);
            let window_weak = window.downgrade();
            let card_weak = card.downgrade();
            let cmd_tx_loop = cmd_tx.clone();
            let app_weak = app.downgrade();
            let ui_rx = ui_rx.clone();

            glib::spawn_future_local(async move {
                while let Ok(event) = ui_rx.recv().await {
                    match event {
                        UiEvent::StateChanged(displays) => {
                            *state.borrow_mut() = displays;
                            if let Some(c) = card_weak.upgrade() {
                                rebuild_card(&c, &state.borrow(), &cmd_tx_loop);
                            }
                        }
                        UiEvent::TogglePopover | UiEvent::ShowProvider(_) => {
                            // TODO: ShowProvider should select that provider's
                            // view; for now both toggle visibility.
                            if let Some(window) = window_weak.upgrade() {
                                let now_visible = window.is_visible();
                                if !now_visible {
                                    // Rebuild countdown texts when about to show.
                                    if let Some(c) = card_weak.upgrade() {
                                        rebuild_card(&c, &state.borrow(), &cmd_tx_loop);
                                    }
                                }
                                window.set_visible(!now_visible);
                            }
                        }
                        UiEvent::Quit => {
                            if let Some(app) = app_weak.upgrade() {
                                app.quit();
                            }
                            break;
                        }
                    }
                }
            });
        }

        // Window starts hidden.
        window.set_visible(false);
    });

    app.run_with_args::<&str>(&[]);
    Ok(())
}

// ---------------------------------------------------------------------------
// Card builder
// ---------------------------------------------------------------------------

/// Remove all children of `card` and rebuild from `displays`.
fn rebuild_card(
    card: &GBox,
    displays: &[ProviderDisplay],
    cmd_tx: &async_channel::Sender<EngineCommand>,
) {
    // Clear existing children.
    while let Some(child) = card.first_child() {
        card.remove(&child);
    }

    if displays.is_empty() {
        let lbl = Label::builder()
            .label("No providers detected.\nLog in with `claude` or `codex`.")
            .css_classes(["empty-label"])
            .halign(gtk4::Align::Start)
            .wrap(true)
            .build();
        card.append(&lbl);
    } else {
        // Collect the newest fetched_at across all ready providers for the footer.
        let mut newest_fetched_at: Option<DateTime<Utc>> = None;

        for (i, display) in displays.iter().enumerate() {
            if i > 0 {
                let sep = Separator::new(Orientation::Horizontal);
                card.append(&sep);
            }
            let tile = build_tile(display, &mut newest_fetched_at);
            card.append(&tile);
        }

        // Footer.
        let foot_sep = Separator::new(Orientation::Horizontal);
        card.append(&foot_sep);
        let footer = build_footer(cmd_tx, newest_fetched_at);
        card.append(&footer);
    }
}

// ---------------------------------------------------------------------------
// Tile builder
// ---------------------------------------------------------------------------

fn build_tile(display: &ProviderDisplay, newest_fetched_at: &mut Option<DateTime<Utc>>) -> GBox {
    let tile = GBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .css_classes(["tile"])
        .build();

    // Header row: name + optional plan badge.
    {
        let header = GBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(6)
            .css_classes(["tile-header"])
            .build();

        let name_lbl = Label::builder()
            .label(display.name)
            .css_classes(["provider-name"])
            .halign(gtk4::Align::Start)
            .build();
        header.append(&name_lbl);

        // Spacer.
        let spacer = GBox::builder().hexpand(true).build();
        header.append(&spacer);

        // Plan badge (only when known) and fetch-time tracking.
        if let ProviderState::Ready(ref snap) = display.state {
            if let Some(ref plan) = snap.plan {
                let badge = Label::builder()
                    .label(plan.as_str())
                    .css_classes(["badge"])
                    .halign(gtk4::Align::End)
                    .build();
                header.append(&badge);
            }
            if let Some(fetched) = snap.fetched_at {
                match newest_fetched_at {
                    None => *newest_fetched_at = Some(fetched),
                    Some(existing) => {
                        if fetched > *existing {
                            *newest_fetched_at = Some(fetched);
                        }
                    }
                }
            }
        }

        tile.append(&header);
    }

    // Body depending on state.
    match &display.state {
        ProviderState::Loading => {
            let lbl = Label::builder()
                .label("loading…")
                .css_classes(["caption"])
                .halign(gtk4::Align::Start)
                .build();
            tile.append(&lbl);
        }

        ProviderState::Error(msg) => {
            let lbl = Label::builder()
                .label(msg.as_str())
                .css_classes(["error"])
                .halign(gtk4::Align::Start)
                .wrap(true)
                .lines(3)
                .build();
            tile.append(&lbl);
        }

        ProviderState::Ready(snap) => {
            for window in &snap.windows {
                let pct = window.used_percent.clamp(0.0, 100.0);
                let sev = severity(pct);

                // Row: label + percentage.
                let row = GBox::builder()
                    .orientation(Orientation::Horizontal)
                    .spacing(4)
                    .build();

                let win_lbl = Label::builder()
                    .label(window.label.as_str())
                    .css_classes(["win-label"])
                    .halign(gtk4::Align::Start)
                    .hexpand(true)
                    .build();
                row.append(&win_lbl);

                let pct_str = format!("{:.0}%", pct);
                let pct_lbl = Label::builder()
                    .label(pct_str.as_str())
                    .halign(gtk4::Align::End)
                    .build();
                pct_lbl.set_css_classes(&["win-pct", sev]);
                row.append(&pct_lbl);

                tile.append(&row);

                // Progress bar (LevelBar).
                let bar = LevelBar::builder()
                    .min_value(0.0)
                    .max_value(100.0)
                    .value(pct)
                    .build();
                // Remove the default thresholds so we get a simple single-block bar.
                bar.remove_offset_value(Some("low"));
                bar.remove_offset_value(Some("high"));
                bar.remove_offset_value(Some("full"));
                // Tag with severity so CSS can target `levelbar.ok block.filled` etc.
                bar.set_css_classes(&["bar", sev]);
                tile.append(&bar);

                // Caption: reset countdown.
                if let Some(resets_at) = window.resets_at {
                    let caption_text = format_reset_time(resets_at);
                    let caption = Label::builder()
                        .label(caption_text.as_str())
                        .css_classes(["caption"])
                        .halign(gtk4::Align::Start)
                        .build();
                    tile.append(&caption);
                }
            }

            // Credits row.
            if let Some(ref credits) = snap.credits {
                let symbol = credits.currency.as_deref().unwrap_or("$");
                let credits_text = format!("Credits: {}{:.2}", symbol, credits.balance);
                let credits_lbl = Label::builder()
                    .label(credits_text.as_str())
                    .css_classes(["caption"])
                    .halign(gtk4::Align::Start)
                    .build();
                tile.append(&credits_lbl);
            }
        }
    }

    tile
}

// ---------------------------------------------------------------------------
// Footer
// ---------------------------------------------------------------------------

fn build_footer(
    cmd_tx: &async_channel::Sender<EngineCommand>,
    newest_fetched_at: Option<DateTime<Utc>>,
) -> GBox {
    let footer = GBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(4)
        .css_classes(["footer"])
        .build();

    // Refresh button.
    let refresh_btn = Button::builder()
        .label("⟳ Refresh")
        .css_classes(["footer-btn"])
        .build();
    {
        let cmd_tx = cmd_tx.clone();
        refresh_btn.connect_clicked(move |_| {
            let _ = cmd_tx.try_send(EngineCommand::RefreshAll);
        });
    }
    footer.append(&refresh_btn);

    // Settings button.
    let settings_btn = Button::builder()
        .label("⚙ Settings")
        .css_classes(["footer-btn"])
        .build();
    settings_btn.connect_clicked(move |_| {
        open_config_file();
    });
    footer.append(&settings_btn);

    // Spacer.
    let spacer = GBox::builder().hexpand(true).build();
    footer.append(&spacer);

    // Last-updated label.
    if let Some(fetched) = newest_fetched_at {
        let local: chrono::DateTime<Local> = fetched.into();
        let text = format!("Last updated {}", local.format("%H:%M"));
        let lbl = Label::builder()
            .label(text.as_str())
            .css_classes(["last-updated"])
            .halign(gtk4::Align::End)
            .build();
        footer.append(&lbl);
    }

    footer
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn severity(pct: f64) -> &'static str {
    if pct >= 90.0 {
        "crit"
    } else if pct >= 70.0 {
        "warn"
    } else {
        "ok"
    }
}

/// Format a UTC reset time as a human-readable string.
///
/// - If < 24 h away: "resets in 2 h 14 m"
/// - Otherwise: "resets Fri 09:00"
fn format_reset_time(resets_at: DateTime<Utc>) -> String {
    let now = Utc::now();
    if resets_at <= now {
        return "resets soon".to_string();
    }
    let delta = resets_at - now;
    let total_secs = delta.num_seconds().max(0);
    if total_secs < 24 * 3600 {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        if hours > 0 {
            format!("resets in {} h {} m", hours, mins)
        } else {
            format!("resets in {} m", mins)
        }
    } else {
        let local: chrono::DateTime<Local> = resets_at.into();
        format!("resets {} {}", local.format("%a"), local.format("%H:%M"))
    }
}

/// Open the config file in the user's default editor/viewer.
/// Creates the file (with commented defaults) if it doesn't exist.
fn open_config_file() {
    let Some(path) = config_path() else {
        tracing::warn!("cannot determine config path");
        return;
    };

    // Ensure parent directory and file exist.
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("cannot create config dir: {e}");
            return;
        }
    }
    if !path.exists() {
        let defaults = "\
# CodexBar configuration
# refresh_secs = 300
# providers = []   # empty = auto-detect
# popover_margin_top = 8
# popover_margin_right = 8
";
        if let Err(e) = std::fs::write(&path, defaults) {
            tracing::warn!("cannot write default config: {e}");
            return;
        }
    }

    let uri = format!("file://{}", path.display());
    if let Err(e) =
        gtk4::gio::AppInfo::launch_default_for_uri(&uri, gtk4::gio::AppLaunchContext::NONE)
    {
        tracing::warn!("cannot open config file: {e}");
    }
}
