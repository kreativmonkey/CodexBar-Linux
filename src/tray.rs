use std::f64::consts::PI;
use std::sync::Arc;

use gtk4::cairo::{Context, FontSlant, FontWeight, Format, ImageSurface};
use ksni::{Icon, MenuItem, ToolTip, Tray, TrayMethods};
use tokio::sync::Mutex;
use tracing::warn;

use crate::model::{EngineCommand, ProviderDisplay, ProviderState, UiEvent};

// ---------------------------------------------------------------------------
// Colour helpers
// ---------------------------------------------------------------------------

/// Returns the RGB components (0.0–1.0) for the arc based on utilisation %.
fn arc_colour(pct: f64) -> (f64, f64, f64) {
    if pct >= 90.0 {
        // #f38ba8 – red
        (
            0xf3 as f64 / 255.0,
            0x8b as f64 / 255.0,
            0xa8 as f64 / 255.0,
        )
    } else if pct >= 70.0 {
        // #fab387 – orange
        (
            0xfa as f64 / 255.0,
            0xb3 as f64 / 255.0,
            0x87 as f64 / 255.0,
        )
    } else {
        // #a6e3a1 – green
        (
            0xa6 as f64 / 255.0,
            0xe3 as f64 / 255.0,
            0xa1 as f64 / 255.0,
        )
    }
}

// ---------------------------------------------------------------------------
// Icon rendering
// ---------------------------------------------------------------------------

/// Render a single-size ARGB icon (22 or 44 px).
///
/// Returns `ksni::Icon` with data in network-byte-order ARGB32.
fn render_icon(size: i32, displays: &[ProviderDisplay]) -> Option<Icon> {
    // Gather state:
    //  - has_error: any provider is in Error state
    //  - max_pct:   highest used_percent across Ready snapshots (None = loading)
    let has_error = displays
        .iter()
        .any(|d| matches!(d.state, ProviderState::Error(_)));

    let max_pct = displays.iter().find_map(|d| {
        if let ProviderState::Ready(ref snap) = d.state {
            snap.max_used_percent()
        } else {
            None
        }
    });
    // If there are multiple ready providers pick the highest
    let max_pct = displays.iter().fold(max_pct, |acc, d| {
        if let ProviderState::Ready(ref snap) = d.state {
            match (acc, snap.max_used_percent()) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            }
        } else {
            acc
        }
    });

    // --- Cairo surface ---
    let surface = ImageSurface::create(Format::ARgb32, size, size).ok()?;
    let ctx = Context::new(&surface).ok()?;

    let sz = size as f64;
    let cx = sz / 2.0;
    let cy = sz / 2.0;

    // Scale stroke widths relative to a 22-px baseline.
    let scale = sz / 22.0;
    let track_width = 2.5 * scale;
    let ring_radius = cx - track_width / 2.0 - 1.0 * scale;

    // Transparent background (ARGB32 surface is already zeroed).
    ctx.set_operator(gtk4::cairo::Operator::Source);
    ctx.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    ctx.paint().ok()?;
    ctx.set_operator(gtk4::cairo::Operator::Over);

    // Full-circle track: white at 25% alpha
    ctx.set_line_width(track_width);
    ctx.set_source_rgba(1.0, 1.0, 1.0, 0.25);
    ctx.arc(cx, cy, ring_radius, 0.0, 2.0 * PI);
    ctx.stroke().ok()?;

    // Progress arc (if we have data)
    if let Some(pct) = max_pct {
        let (r, g, b) = arc_colour(pct);
        let fraction = (pct / 100.0).clamp(0.0, 1.0);
        // Start at 12 o'clock = -PI/2; sweep clockwise
        let start_angle = -PI / 2.0;
        let end_angle = start_angle + 2.0 * PI * fraction;

        ctx.set_source_rgba(r, g, b, 1.0);
        ctx.set_line_cap(gtk4::cairo::LineCap::Round);
        ctx.arc(cx, cy, ring_radius, start_angle, end_angle);
        ctx.stroke().ok()?;

        // Centred percent label
        let label = format!("{}", pct.round() as i64);
        let font_size = 7.5 * scale;
        ctx.select_font_face("sans", FontSlant::Normal, FontWeight::Bold);
        ctx.set_font_size(font_size);
        if let Ok(ext) = ctx.text_extents(&label) {
            let tx = cx - ext.width() / 2.0 - ext.x_bearing();
            let ty = cy - ext.height() / 2.0 - ext.y_bearing();
            ctx.move_to(tx, ty);
            ctx.set_source_rgba(1.0, 1.0, 1.0, 1.0);
            let _ = ctx.show_text(&label);
        }
    } else {
        // Loading: grey ring, no arc, no text — track already drawn in grey-ish
        // Overdraw the track with a slightly lighter grey to indicate "no data".
        ctx.set_source_rgba(0.6, 0.6, 0.6, 0.5);
        ctx.set_line_width(track_width);
        ctx.arc(cx, cy, ring_radius, 0.0, 2.0 * PI);
        ctx.stroke().ok()?;
    }

    // Error indicator: small red dot at bottom-right
    if has_error {
        let dot_r = 2.5 * scale;
        let dot_cx = cx + ring_radius * (PI / 4.0_f64).cos();
        let dot_cy = cy + ring_radius * (PI / 4.0_f64).sin();
        ctx.set_source_rgba(
            0xf3 as f64 / 255.0,
            0x8b as f64 / 255.0,
            0xa8 as f64 / 255.0,
            1.0,
        );
        ctx.arc(dot_cx, dot_cy, dot_r, 0.0, 2.0 * PI);
        ctx.fill().ok()?;
    }

    // Drop context to release reference on surface before calling data()
    drop(ctx);

    // Extract pixel data and convert cairo ARGB32 (premultiplied, native-endian)
    // → ksni network-byte-order ARGB32 (A,R,G,B in memory).
    //
    // On little-endian x86 cairo stores pixels as B,G,R,A in memory (native u32 = 0xAARRGGBB).
    // ksni expects A,R,G,B in memory order.
    let stride = surface.stride() as usize;
    let width = surface.width() as usize;
    let height = surface.height() as usize;
    let mut out = Vec::with_capacity(width * height * 4);

    surface
        .with_data(|raw| {
            for row in 0..height {
                let row_start = row * stride;
                for col in 0..width {
                    let offset = row_start + col * 4;
                    // Little-endian memory: [B, G, R, A]
                    let b = raw[offset];
                    let g = raw[offset + 1];
                    let r = raw[offset + 2];
                    let a = raw[offset + 3];

                    // Un-premultiply alpha
                    let (r, g, b) = if a == 0 {
                        (0u8, 0u8, 0u8)
                    } else if a == 255 {
                        (r, g, b)
                    } else {
                        let alpha = a as u32;
                        let unp = |c: u8| ((c as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                        (unp(r), unp(g), unp(b))
                    };

                    // Network byte order: [A, R, G, B]
                    out.push(a);
                    out.push(r);
                    out.push(g);
                    out.push(b);
                }
            }
        })
        .ok()?;

    Some(Icon {
        width: size,
        height: size,
        data: out,
    })
}

// ---------------------------------------------------------------------------
// Tooltip formatting
// ---------------------------------------------------------------------------

fn format_tooltip_description(displays: &[ProviderDisplay]) -> String {
    if displays.is_empty() {
        return "No providers configured".to_string();
    }
    displays
        .iter()
        .map(format_provider_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_provider_line(d: &ProviderDisplay) -> String {
    match &d.state {
        ProviderState::Loading => format!("{}: loading", d.name),
        ProviderState::Error(msg) => format!("{}: error — {}", d.name, msg),
        ProviderState::Ready(snap) => {
            if snap.windows.is_empty() {
                return format!("{}: no data", d.name);
            }
            let parts: Vec<String> = snap
                .windows
                .iter()
                .map(|w| {
                    format!(
                        "{}%\u{a0}{}",
                        w.used_percent.round() as i64,
                        w.label.to_lowercase()
                    )
                })
                .collect();
            format!("{}: {}", d.name, parts.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------
// ksni Tray implementation
// ---------------------------------------------------------------------------

struct CodexBarTray {
    displays: Vec<ProviderDisplay>,
    ui_tx: async_channel::Sender<UiEvent>,
    cmd_tx: async_channel::Sender<EngineCommand>,
}

impl Tray for CodexBarTray {
    fn id(&self) -> String {
        "codexbar".into()
    }

    fn title(&self) -> String {
        "CodexBar".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        if let Err(e) = self.ui_tx.try_send(UiEvent::TogglePopover) {
            warn!("tray activate: send failed: {e}");
        }
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
            title: "CodexBar".into(),
            description: format_tooltip_description(&self.displays),
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let mut icons = Vec::new();
        if let Some(icon22) = render_icon(22, &self.displays) {
            icons.push(icon22);
        }
        if let Some(icon44) = render_icon(44, &self.displays) {
            icons.push(icon44);
        }
        icons
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            MenuItem::Standard(ksni::menu::StandardItem {
                label: "Refresh".into(),
                activate: Box::new(|this: &mut Self| {
                    if let Err(e) = this.cmd_tx.try_send(EngineCommand::RefreshAll) {
                        warn!("tray menu Refresh: send failed: {e}");
                    }
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(ksni::menu::StandardItem {
                label: "Quit".into(),
                activate: Box::new(|this: &mut Self| {
                    if let Err(e) = this.cmd_tx.try_send(EngineCommand::Quit) {
                        warn!("tray menu Quit: send failed: {e}");
                    }
                }),
                ..Default::default()
            }),
        ]
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Handle to the StatusNotifierItem; lets the engine push icon updates.
pub struct TrayHandle {
    handle: Arc<Mutex<ksni::Handle<CodexBarTray>>>,
}

/// Spawn the tray item on the current tokio runtime.
///
/// - Left click (Activate) sends `UiEvent::TogglePopover`.
/// - Menu: Refresh -> `EngineCommand::RefreshAll`, Quit -> `EngineCommand::Quit`.
pub async fn spawn(
    ui_tx: async_channel::Sender<UiEvent>,
    cmd_tx: async_channel::Sender<EngineCommand>,
) -> anyhow::Result<TrayHandle> {
    let tray = CodexBarTray {
        displays: Vec::new(),
        ui_tx,
        cmd_tx,
    };

    let handle = tray
        .spawn()
        .await
        .map_err(|e| anyhow::anyhow!("StatusNotifierItem: {e}"))?;

    Ok(TrayHandle {
        handle: Arc::new(Mutex::new(handle)),
    })
}

impl TrayHandle {
    /// Re-render the tray icon/tooltip from the latest provider state.
    pub async fn update(&self, displays: &[ProviderDisplay]) {
        let displays = displays.to_vec();
        let handle = self.handle.lock().await;
        handle
            .update(move |tray| {
                tray.displays = displays;
            })
            .await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RateWindow, UsageSnapshot};

    // --- colour selection ---

    #[test]
    fn colour_green_below_70() {
        let (r, g, b) = arc_colour(0.0);
        assert!((r - 0xa6 as f64 / 255.0).abs() < 1e-6);
        assert!((g - 0xe3 as f64 / 255.0).abs() < 1e-6);
        assert!((b - 0xa1 as f64 / 255.0).abs() < 1e-6);

        let (r, g, b) = arc_colour(69.9);
        assert!((r - 0xa6 as f64 / 255.0).abs() < 1e-6, "r={r}");
        let _ = (g, b);
    }

    #[test]
    fn colour_orange_70_to_90() {
        let (r, g, b) = arc_colour(70.0);
        assert!((r - 0xfa as f64 / 255.0).abs() < 1e-6, "r={r}");
        assert!((g - 0xb3 as f64 / 255.0).abs() < 1e-6, "g={g}");
        assert!((b - 0x87 as f64 / 255.0).abs() < 1e-6, "b={b}");

        let (r2, _, _) = arc_colour(89.9);
        assert!((r2 - 0xfa as f64 / 255.0).abs() < 1e-6, "r2={r2}");
    }

    #[test]
    fn colour_red_above_90() {
        let (r, g, b) = arc_colour(90.0);
        assert!((r - 0xf3 as f64 / 255.0).abs() < 1e-6, "r={r}");
        assert!((g - 0x8b as f64 / 255.0).abs() < 1e-6, "g={g}");
        assert!((b - 0xa8 as f64 / 255.0).abs() < 1e-6, "b={b}");

        let (r2, _, _) = arc_colour(100.0);
        assert!((r2 - 0xf3 as f64 / 255.0).abs() < 1e-6, "r2={r2}");
    }

    // --- pixel conversion round-trip ---

    #[test]
    fn pixel_conversion_opaque() {
        // Opaque fully-saturated red pixel in premultiplied ARGB32 native-endian
        // on little-endian: memory = [B=0x00, G=0x00, R=0xFF, A=0xFF]
        let b: u8 = 0x00;
        let g: u8 = 0x00;
        let r: u8 = 0xFF;
        let a: u8 = 0xFF;

        // un-premultiply (a==255 path: no-op)
        let (ro, go, bo) = (r, g, b);
        // network byte order output should be [A, R, G, B]
        let out = [a, ro, go, bo];
        assert_eq!(out, [0xFF, 0xFF, 0x00, 0x00]);
    }

    #[test]
    fn pixel_conversion_semitransparent() {
        // 50% transparent green: premultiplied G = 0xFF * 0x80 / 0xFF = 0x80
        let a: u8 = 0x80;
        let r: u8 = 0x00;
        let g: u8 = 0x80; // premultiplied
        let b: u8 = 0x00;

        let alpha = a as u32;
        let unp = |c: u8| ((c as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
        let (ro, go, bo) = (unp(r), unp(g), unp(b));

        // un-premultiplied green should be close to 0xFF
        assert_eq!(ro, 0x00);
        assert!(go >= 0xFE, "go={go}");
        assert_eq!(bo, 0x00);

        let out = [a, ro, go, bo];
        assert_eq!(out[0], 0x80);
        assert_eq!(out[2], go);
    }

    #[test]
    fn pixel_conversion_fully_transparent() {
        let a: u8 = 0;
        let (r, g, b) = (0u8, 0u8, 0u8); // should stay 0 regardless
        let (ro, go, bo) = if a == 0 { (0u8, 0u8, 0u8) } else { (r, g, b) };
        let out = [a, ro, go, bo];
        assert_eq!(out, [0, 0, 0, 0]);
    }

    // --- tooltip formatting ---

    fn make_ready(name: &'static str, pcts: &[(&str, f64)]) -> ProviderDisplay {
        let windows = pcts
            .iter()
            .map(|(label, pct)| RateWindow {
                label: label.to_string(),
                used_percent: *pct,
                resets_at: None,
            })
            .collect();
        ProviderDisplay {
            id: name,
            name,
            state: ProviderState::Ready(UsageSnapshot {
                windows,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn tooltip_loading() {
        let d = ProviderDisplay {
            id: "claude",
            name: "Claude",
            state: ProviderState::Loading,
        };
        let line = format_provider_line(&d);
        assert_eq!(line, "Claude: loading");
    }

    #[test]
    fn tooltip_error() {
        let d = ProviderDisplay {
            id: "claude",
            name: "Claude",
            state: ProviderState::Error("rate limit".into()),
        };
        let line = format_provider_line(&d);
        assert!(line.contains("error"), "line={line}");
        assert!(line.contains("rate limit"), "line={line}");
    }

    #[test]
    fn tooltip_ready_single_window() {
        let d = make_ready("Claude", &[("Session", 62.0)]);
        let line = format_provider_line(&d);
        assert!(line.contains("62%"), "line={line}");
        assert!(line.contains("session"), "line={line}");
    }

    #[test]
    fn tooltip_ready_multi_window() {
        let d = make_ready("Claude", &[("Session", 62.0), ("Weekly", 31.0)]);
        let line = format_provider_line(&d);
        assert!(line.starts_with("Claude:"), "line={line}");
        assert!(line.contains("62%"), "line={line}");
        assert!(line.contains("31%"), "line={line}");
    }

    #[test]
    fn tooltip_empty_providers() {
        let desc = format_tooltip_description(&[]);
        assert!(desc.contains("No providers"), "desc={desc}");
    }

    #[test]
    fn tooltip_multiple_providers() {
        let displays = vec![
            make_ready("Claude", &[("Session", 62.0), ("Weekly", 31.0)]),
            ProviderDisplay {
                id: "openai",
                name: "OpenAI",
                state: ProviderState::Loading,
            },
        ];
        let desc = format_tooltip_description(&displays);
        assert!(desc.contains("Claude:"), "desc={desc}");
        assert!(desc.contains("OpenAI:"), "desc={desc}");
        assert!(desc.contains('\n'), "should have newline: desc={desc}");
    }
}
