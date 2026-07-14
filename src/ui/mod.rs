use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Local, Utc};
use gtk4::gdk::MemoryFormat;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GBox, Button, CssProvider, EventControllerKey, Image,
    Label, LevelBar, Orientation, Separator,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::config::{config_path, Config};
use crate::model::{EngineCommand, ProviderDisplay, ProviderState, UiEvent};
use crate::reset_time::format_reset_time;

const STYLE: &str = include_str!("style.css");

// ---------------------------------------------------------------------------
// Icon helpers
// ---------------------------------------------------------------------------

/// Build a GTK Image from a provider logo at `size` logical pixels.
/// Tint: #e6e8ef. Returns None if rasterization fails.
fn make_logo_image(provider_id: &str, size: u32) -> Option<Image> {
    let pixmap = crate::icons::logo_pixmap(provider_id, size, Some([0xe6, 0xe8, 0xef]))?;
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let stride = (pixmap.width() * 4) as usize;
    let bytes = glib::Bytes::from(pixmap.data());
    let texture = gtk4::gdk::MemoryTexture::new(
        width,
        height,
        MemoryFormat::R8g8b8a8Premultiplied,
        &bytes,
        stride,
    );
    let image = Image::from_paintable(Some(&texture));
    image.set_pixel_size(size as i32);
    Some(image)
}

// ---------------------------------------------------------------------------
// Status-dot CSS class
// ---------------------------------------------------------------------------

fn dot_class(display: &ProviderDisplay) -> &'static str {
    match &display.state {
        ProviderState::Loading => "dot-gray",
        ProviderState::Error(_) => "dot-red",
        ProviderState::Ready(snap) => match snap.max_used_percent() {
            None => "dot-gray",
            Some(p) if p >= 90.0 => "dot-red",
            Some(p) if p >= 70.0 => "dot-orange",
            _ => "dot-green",
        },
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the GTK application (blocks the main thread until quit).
///
/// Listens on `ui_rx`:
/// - `StateChanged(displays)` → rebuild popover content
/// - `ShowProvider(id)`      → select provider view, toggle visibility
/// - `ShowProvider(id)`      → select provider + show; toggle if already shown
/// - `Quit`                  → exit
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

        // Selected provider id.  None → fall back to first provider.
        let selection: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

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
            let selection_weak = Rc::downgrade(&selection);
            let card_weak = card.downgrade();
            let window_weak = window.downgrade();
            let cmd_tx_timer = cmd_tx.clone();
            glib::timeout_add_local(std::time::Duration::from_secs(30), move || {
                let Some(window) = window_weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if window.is_visible() {
                    if let (Some(st), Some(sel), Some(c)) = (
                        state_weak.upgrade(),
                        selection_weak.upgrade(),
                        card_weak.upgrade(),
                    ) {
                        rebuild_card(&c, &st, &sel, &cmd_tx_timer);
                    }
                }
                glib::ControlFlow::Continue
            });
        }

        // Event loop consuming ui_rx.
        {
            let state = Rc::clone(&state);
            let selection = Rc::clone(&selection);
            let window_weak = window.downgrade();
            let card_weak = card.downgrade();
            let cmd_tx_loop = cmd_tx.clone();
            let app_weak = app.downgrade();
            let ui_rx = ui_rx.clone();

            glib::spawn_future_local(async move {
                while let Ok(event) = ui_rx.recv().await {
                    match event {
                        UiEvent::StateChanged(displays) => {
                            // Keep selection if that provider still exists; else fall back.
                            {
                                let mut sel = selection.borrow_mut();
                                let still_exists = sel
                                    .as_ref()
                                    .is_some_and(|id| displays.iter().any(|d| d.id == id.as_str()));
                                if !still_exists {
                                    *sel = displays.first().map(|d| d.id.to_string());
                                }
                            }
                            *state.borrow_mut() = displays;
                            if let Some(c) = card_weak.upgrade() {
                                rebuild_card(&c, &state, &selection, &cmd_tx_loop);
                            }
                        }
                        UiEvent::ShowProvider(id) => {
                            if let Some(window) = window_weak.upgrade() {
                                let already_selected =
                                    selection.borrow().as_deref().is_some_and(|s| s == id);
                                let is_visible = window.is_visible();

                                if is_visible && already_selected {
                                    // Toggle off: same provider tray-icon clicked again.
                                    window.set_visible(false);
                                } else {
                                    // Switch to provider (if known), rebuild, show.
                                    {
                                        let displays = state.borrow();
                                        if displays.iter().any(|d| d.id == id.as_str()) {
                                            *selection.borrow_mut() = Some(id);
                                        } else if selection.borrow().is_none() {
                                            *selection.borrow_mut() =
                                                displays.first().map(|d| d.id.to_string());
                                        }
                                    }
                                    if let Some(c) = card_weak.upgrade() {
                                        rebuild_card(&c, &state, &selection, &cmd_tx_loop);
                                    }
                                    window.set_visible(true);
                                }
                            }
                        }
                        UiEvent::TogglePopover => {
                            if let Some(window) = window_weak.upgrade() {
                                if window.is_visible() {
                                    window.set_visible(false);
                                } else {
                                    if selection.borrow().is_none() {
                                        *selection.borrow_mut() =
                                            state.borrow().first().map(|d| d.id.to_string());
                                    }
                                    if let Some(c) = card_weak.upgrade() {
                                        rebuild_card(&c, &state, &selection, &cmd_tx_loop);
                                    }
                                    window.set_visible(true);
                                }
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

/// Remove all children of `card` and rebuild from live `state`.
fn rebuild_card(
    card: &GBox,
    state: &Rc<RefCell<Vec<ProviderDisplay>>>,
    selection: &Rc<RefCell<Option<String>>>,
    cmd_tx: &async_channel::Sender<EngineCommand>,
) {
    // Clear existing children.
    while let Some(child) = card.first_child() {
        card.remove(&child);
    }

    let displays = state.borrow();

    if displays.is_empty() {
        let lbl = Label::builder()
            .label("No providers detected.\nLog in with `claude` or `codex`.")
            .css_classes(["empty-label"])
            .halign(gtk4::Align::Start)
            .wrap(true)
            .build();
        card.append(&lbl);
        return;
    }

    // Resolve the effective selected id.
    let effective_id: String = {
        let sel = selection.borrow();
        match sel.as_ref() {
            Some(id) if displays.iter().any(|d| d.id == id.as_str()) => id.clone(),
            _ => displays[0].id.to_string(),
        }
    };

    // Provider icon bar — hidden when only one provider.
    if displays.len() > 1 {
        let switcher = build_switcher(&displays, &effective_id, state, selection, card, cmd_tx);
        card.append(&switcher);

        let sep = Separator::new(Orientation::Horizontal);
        card.append(&sep);
    }

    // Detail tile for the selected provider only.
    let mut newest_fetched_at: Option<DateTime<Utc>> = None;
    if let Some(display) = displays.iter().find(|d| d.id == effective_id.as_str()) {
        let tile = build_tile(display, &mut newest_fetched_at);
        card.append(&tile);
    }

    // Footer.
    let foot_sep = Separator::new(Orientation::Horizontal);
    card.append(&foot_sep);
    let footer = build_footer(cmd_tx, newest_fetched_at);
    card.append(&footer);
}

// ---------------------------------------------------------------------------
// Provider switcher bar
// ---------------------------------------------------------------------------

fn build_switcher(
    displays: &[ProviderDisplay],
    effective_id: &str,
    state: &Rc<RefCell<Vec<ProviderDisplay>>>,
    selection: &Rc<RefCell<Option<String>>>,
    card: &GBox,
    cmd_tx: &async_channel::Sender<EngineCommand>,
) -> GBox {
    let switcher = GBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(4)
        .css_classes(["switcher"])
        .halign(gtk4::Align::Center)
        .build();

    for display in displays {
        let provider_id = display.id;

        // Each tab: vertical box with logo + status dot.
        let tab_box = GBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(2)
            .halign(gtk4::Align::Center)
            .build();

        // Logo image, or fallback: first letter of provider name.
        if let Some(img) = make_logo_image(provider_id, 18) {
            tab_box.append(&img);
        } else {
            let first_char = display
                .name
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "?".to_string());
            let fallback = Label::builder()
                .label(first_char.as_str())
                .css_classes(["tab-fallback"])
                .build();
            tab_box.append(&fallback);
        }

        // Status dot below the logo.
        let dot = Label::builder()
            .label("●")
            .css_classes(["status-dot", dot_class(display)])
            .halign(gtk4::Align::Center)
            .build();
        tab_box.append(&dot);

        // Flat toggle button wrapping the tab content.
        let btn = Button::builder().css_classes(["provider-tab"]).build();
        btn.set_child(Some(&tab_box));

        if provider_id == effective_id {
            btn.add_css_class("active");
        }

        // Click: update selection and rebuild the card immediately.
        {
            let selection = Rc::clone(selection);
            let state = Rc::clone(state);
            let card_weak = card.downgrade();
            let cmd_tx = cmd_tx.clone();
            let id_str = provider_id.to_string();
            btn.connect_clicked(move |_| {
                *selection.borrow_mut() = Some(id_str.clone());
                if let Some(c) = card_weak.upgrade() {
                    rebuild_card(&c, &state, &selection, &cmd_tx);
                }
            });
        }

        switcher.append(&btn);
    }

    switcher
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

        // Plan badge and fetch-time tracking.
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
                bar.remove_offset_value(Some("low"));
                bar.remove_offset_value(Some("high"));
                bar.remove_offset_value(Some("full"));
                bar.set_css_classes(&["bar", sev]);
                tile.append(&bar);

                // Caption: reset countdown or custom detail.
                if let Some(caption) = &window.caption {
                    let caption_lbl = Label::builder()
                        .label(caption.as_str())
                        .css_classes(["caption"])
                        .halign(gtk4::Align::Start)
                        .build();
                    tile.append(&caption_lbl);
                }
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
                let credits_text = match (credits.used, credits.limit) {
                    (Some(used), Some(limit)) => format!(
                        "{}{:.2} / {}{:.2} ({}{:.2} left)",
                        symbol, used, symbol, limit, symbol, credits.balance
                    ),
                    _ => format!("Credits: {}{:.2}", symbol, credits.balance),
                };
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

/// Open the config file in the user's default editor/viewer.
fn open_config_file() {
    let Some(path) = config_path() else {
        tracing::warn!("cannot determine config path");
        return;
    };

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
# tray_icon_mode = \"per_provider\"   # or \"combined\" for a single tray icon
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
