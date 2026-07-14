use std::collections::HashMap;
use std::f32::consts::PI;

use ksni::{Icon, MenuItem, ToolTip, Tray, TrayMethods};
use resvg::tiny_skia::{Color, Paint, PathBuilder, Pixmap, Stroke, Transform};
use tokio::sync::Mutex;
use tracing::warn;

use crate::model::{EngineCommand, ProviderDisplay, ProviderState, UiEvent};

// ---------------------------------------------------------------------------
// Colour helpers
// ---------------------------------------------------------------------------

/// Returns the RGB components (0–255) for the progress arc based on utilisation.
fn arc_colour_u8(pct: f64) -> (u8, u8, u8) {
    if pct >= 90.0 {
        // #f38ba8 – red
        (0xf3, 0x8b, 0xa8)
    } else if pct >= 70.0 {
        // #fab387 – orange
        (0xfa, 0xb3, 0x87)
    } else {
        // #a6e3a1 – green
        (0xa6, 0xe3, 0xa1)
    }
}

// Keep the f64 version for backward-compat with tests.
#[cfg(test)]
fn arc_colour(pct: f64) -> (f64, f64, f64) {
    let (r, g, b) = arc_colour_u8(pct);
    (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)
}

// ---------------------------------------------------------------------------
// Icon rendering — pure tiny_skia, no Cairo
// ---------------------------------------------------------------------------

/// Render a per-provider tray icon at `size` pixels.
///
/// Layout:
/// - circular track (white, 22 % alpha) on the full icon
/// - arc filled by max_used_percent, coloured by threshold
/// - provider logo centred at ~62 % of the icon size (inner circle)
/// - small red dot bottom-right on Error state
/// - grey track only while Loading
///
/// Returns `ksni::Icon` with data in **network-byte-order ARGB32** (A,R,G,B in memory).
fn render_provider_icon(size: i32, display: &ProviderDisplay) -> Option<Icon> {
    let sz = size as f32;
    let cx = sz / 2.0;
    let cy = sz / 2.0;

    let scale = sz / 22.0;
    let track_w = 2.5 * scale;
    let ring_r = cx - track_w / 2.0 - 1.0 * scale;

    let has_error = matches!(display.state, ProviderState::Error(_));
    let max_pct = if let ProviderState::Ready(ref snap) = display.state {
        snap.max_used_percent()
    } else {
        None
    };

    let mut pixmap = Pixmap::new(size as u32, size as u32)?;

    // ---- Full-circle track: white 22 % alpha ----
    {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba(1.0, 1.0, 1.0, 0.22).unwrap());
        paint.anti_alias = true;
        let path = circle_path(cx, cy, ring_r)?;
        let stroke = Stroke {
            width: track_w,
            ..Default::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    match max_pct {
        Some(pct) => {
            // ---- Coloured progress arc ----
            let (r, g, b) = arc_colour_u8(pct);
            let fraction = (pct / 100.0).clamp(0.0, 1.0) as f32;
            let mut paint = Paint::default();
            paint.set_color(
                Color::from_rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
                    .unwrap(),
            );
            paint.anti_alias = true;
            if let Some(arc_path) = arc_path(cx, cy, ring_r, fraction, 64) {
                let stroke = Stroke {
                    width: track_w,
                    line_cap: resvg::tiny_skia::LineCap::Round,
                    ..Default::default()
                };
                pixmap.stroke_path(&arc_path, &paint, &stroke, Transform::identity(), None);
            }
        }
        None => {
            // Loading: overdraw track with grey
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba(0.6, 0.6, 0.6, 0.5).unwrap());
            paint.anti_alias = true;
            if let Some(path) = circle_path(cx, cy, ring_r) {
                let stroke = Stroke {
                    width: track_w,
                    ..Default::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
    }

    // ---- Provider logo centred, ~62 % of icon size ----
    let logo_size = (sz * 0.62) as u32;
    if logo_size > 0 {
        if let Some(logo) = crate::icons::logo_pixmap(display.id, logo_size, Some([230, 232, 239]))
        {
            let lx = ((sz - logo_size as f32) / 2.0) as i32;
            let ly = ((sz - logo_size as f32) / 2.0) as i32;
            // Blit logo onto pixmap using source-over
            blit_pixmap(&mut pixmap, &logo, lx, ly);
        }
    }

    // ---- Error dot: small red circle bottom-right ----
    if has_error {
        let dot_r = 2.5 * scale;
        let angle = PI / 4.0; // 45° = bottom-right
        let dot_cx = cx + ring_r * angle.cos();
        let dot_cy = cy + ring_r * angle.sin();
        let mut paint = Paint::default();
        paint.set_color(
            Color::from_rgba(
                0xf3 as f32 / 255.0,
                0x8b as f32 / 255.0,
                0xa8 as f32 / 255.0,
                1.0,
            )
            .unwrap(),
        );
        paint.anti_alias = true;
        if let Some(path) = filled_circle_path(dot_cx, dot_cy, dot_r) {
            pixmap.fill_path(
                &path,
                &paint,
                resvg::tiny_skia::FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    // ---- Convert premultiplied RGBA → network-byte-order ARGB32 ----
    let data = pixmap_to_ksni_argb32(pixmap.data(), size as usize, size as usize);

    Some(Icon {
        width: size,
        height: size,
        data,
    })
}

/// Convert a premultiplied RGBA8 buffer (tiny_skia native) to
/// network-byte-order ARGB32: memory layout [A, R, G, B] per pixel,
/// with alpha un-premultiplied.
///
/// tiny_skia pixels: [R, G, B, A] in memory (RGBA, premultiplied).
fn pixmap_to_ksni_argb32(raw: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 4);
    for chunk in raw.chunks_exact(4) {
        let r = chunk[0];
        let g = chunk[1];
        let b = chunk[2];
        let a = chunk[3];

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

        // Network byte order: A R G B
        out.push(a);
        out.push(r);
        out.push(g);
        out.push(b);
    }
    out
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// A full circle path (for stroking as a ring).
fn circle_path(cx: f32, cy: f32, r: f32) -> Option<resvg::tiny_skia::Path> {
    arc_path(cx, cy, r, 1.0, 64)
}

/// An arc from 12-o'clock sweeping clockwise by `fraction` of a full circle.
/// Approximated with `segments` line segments.
fn arc_path(
    cx: f32,
    cy: f32,
    r: f32,
    fraction: f32,
    segments: usize,
) -> Option<resvg::tiny_skia::Path> {
    let n = ((segments as f32 * fraction).ceil() as usize).max(1);
    let total_angle = 2.0 * PI * fraction;
    let start_angle = -PI / 2.0;

    let mut pb = PathBuilder::new();
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let angle = start_angle + total_angle * t;
        let x = cx + r * angle.cos();
        let y = cy + r * angle.sin();
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.finish()
}

/// A filled circle path.
fn filled_circle_path(cx: f32, cy: f32, r: f32) -> Option<resvg::tiny_skia::Path> {
    // Approximate circle with cubic beziers (standard 4-arc approach)
    let k = 0.552_284_8_f32; // magic constant for cubic bezier circle
    let mut pb = PathBuilder::new();
    pb.move_to(cx, cy - r);
    pb.cubic_to(cx + r * k, cy - r, cx + r, cy - r * k, cx + r, cy);
    pb.cubic_to(cx + r, cy + r * k, cx + r * k, cy + r, cx, cy + r);
    pb.cubic_to(cx - r * k, cy + r, cx - r, cy + r * k, cx - r, cy);
    pb.cubic_to(cx - r, cy - r * k, cx - r * k, cy - r, cx, cy - r);
    pb.close();
    pb.finish()
}

/// Blit `src` onto `dst` at offset (ox, oy) using source-over compositing.
/// Both pixmaps use premultiplied RGBA8 (tiny_skia native).
fn blit_pixmap(dst: &mut Pixmap, src: &Pixmap, ox: i32, oy: i32) {
    let dst_w = dst.width() as i32;
    let dst_h = dst.height() as i32;
    let src_w = src.width() as i32;
    let src_h = src.height() as i32;

    // Collect pixels to blit so we don't hold simultaneous borrows.
    let mut ops: Vec<(usize, u32, u32, u32, u32)> = Vec::new();

    {
        let src_data = src.data();
        for sy in 0..src_h {
            let dy = oy + sy;
            if dy < 0 || dy >= dst_h {
                continue;
            }
            for sx in 0..src_w {
                let dx = ox + sx;
                if dx < 0 || dx >= dst_w {
                    continue;
                }
                let si = (sy * src_w + sx) as usize * 4;
                let sa = src_data[si + 3] as u32;
                if sa == 0 {
                    continue;
                }
                let di = (dy * dst_w + dx) as usize * 4;
                let sr = src_data[si] as u32;
                let sg = src_data[si + 1] as u32;
                let sb = src_data[si + 2] as u32;
                ops.push((di, sr, sg, sb, sa));
            }
        }
    }

    let dst_data = dst.data_mut();
    for (di, sr, sg, sb, sa) in ops {
        // Source-over in premultiplied space:
        // out = src + dst * (1 - src_alpha / 255)
        let inv_a = 255 - sa;
        let dr = dst_data[di] as u32;
        let dg = dst_data[di + 1] as u32;
        let db = dst_data[di + 2] as u32;
        let da = dst_data[di + 3] as u32;

        dst_data[di] = ((sr * 255 + dr * inv_a) / 255).min(255) as u8;
        dst_data[di + 1] = ((sg * 255 + dg * inv_a) / 255).min(255) as u8;
        dst_data[di + 2] = ((sb * 255 + db * inv_a) / 255).min(255) as u8;
        dst_data[di + 3] = ((sa * 255 + da * inv_a) / 255).min(255) as u8;
    }
}

// ---------------------------------------------------------------------------
// Tooltip formatting
// ---------------------------------------------------------------------------

fn format_tooltip_description(d: &ProviderDisplay) -> String {
    match &d.state {
        ProviderState::Loading => "loading…".to_string(),
        ProviderState::Error(msg) => format!("error — {msg}"),
        ProviderState::Ready(snap) => {
            if snap.windows.is_empty() {
                return "no data".to_string();
            }
            snap.windows
                .iter()
                .map(|w| {
                    format!(
                        "{}%\u{a0}{}",
                        w.used_percent.round() as i64,
                        w.label.to_lowercase()
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ")
        }
    }
}

// Keep the multi-provider formatter for the existing tests that call it directly.
#[cfg(test)]
fn format_tooltip_description_multi(displays: &[ProviderDisplay]) -> String {
    if displays.is_empty() {
        return "No providers configured".to_string();
    }
    displays
        .iter()
        .map(format_provider_line)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
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
// ksni Tray implementation — one item per provider
// ---------------------------------------------------------------------------

struct ProviderTray {
    display: ProviderDisplay,
    ui_tx: async_channel::Sender<UiEvent>,
    cmd_tx: async_channel::Sender<EngineCommand>,
}

impl Tray for ProviderTray {
    fn id(&self) -> String {
        format!("codexbar-{}", self.display.id)
    }

    fn title(&self) -> String {
        format!("{} — CodexBar", self.display.name)
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let id = self.display.id.to_string();
        if let Err(e) = self.ui_tx.try_send(UiEvent::ShowProvider(id)) {
            warn!("tray activate: send failed: {e}");
        }
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
            title: format!("{} — CodexBar", self.display.name),
            description: format_tooltip_description(&self.display),
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let mut icons = Vec::new();
        if let Some(icon22) = render_provider_icon(22, &self.display) {
            icons.push(icon22);
        }
        if let Some(icon44) = render_provider_icon(44, &self.display) {
            icons.push(icon44);
        }
        icons
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let name = self.display.name.to_string();
        vec![
            MenuItem::Standard(ksni::menu::StandardItem {
                label: format!("Show {name}"),
                activate: Box::new(move |this: &mut Self| {
                    let send_id = this.display.id.to_string();
                    if let Err(e) = this.ui_tx.try_send(UiEvent::ShowProvider(send_id)) {
                        warn!("tray menu ShowProvider: send failed: {e}");
                    }
                }),
                ..Default::default()
            }),
            MenuItem::Standard(ksni::menu::StandardItem {
                label: "Refresh all".into(),
                activate: Box::new(|this: &mut Self| {
                    if let Err(e) = this.cmd_tx.try_send(EngineCommand::RefreshAll) {
                        warn!("tray menu RefreshAll: send failed: {e}");
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
// Per-provider handle bookkeeping
// ---------------------------------------------------------------------------

struct ProviderEntry {
    handle: ksni::Handle<ProviderTray>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Handle to all per-provider StatusNotifierItems.
pub struct TrayHandle {
    entries: std::sync::Arc<Mutex<HashMap<String, ProviderEntry>>>,
    ui_tx: async_channel::Sender<UiEvent>,
    cmd_tx: async_channel::Sender<EngineCommand>,
    /// Warn only once when the SNI bus is unavailable.
    warned_no_bus: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Spawn the tray manager. Creates no items yet — they appear on first `update()`.
///
/// Public signature kept identical to the old single-icon API.
pub async fn spawn(
    ui_tx: async_channel::Sender<UiEvent>,
    cmd_tx: async_channel::Sender<EngineCommand>,
) -> anyhow::Result<TrayHandle> {
    Ok(TrayHandle {
        entries: std::sync::Arc::new(Mutex::new(HashMap::new())),
        ui_tx,
        cmd_tx,
        warned_no_bus: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    })
}

impl TrayHandle {
    /// Reconcile tray items with the current provider list.
    ///
    /// - New provider → spawn a ksni item.
    /// - Removed provider → shut down its item.
    /// - Existing provider → push updated state.
    pub async fn update(&self, displays: &[ProviderDisplay]) {
        let mut entries = self.entries.lock().await;

        // Determine which ids are now active.
        let active_ids: HashMap<String, &ProviderDisplay> =
            displays.iter().map(|d| (d.id.to_string(), d)).collect();

        // Remove items whose provider is no longer active.
        let to_remove: Vec<String> = entries
            .keys()
            .filter(|id| !active_ids.contains_key(*id))
            .cloned()
            .collect();

        for id in to_remove {
            if let Some(entry) = entries.remove(&id) {
                entry.handle.shutdown();
            }
        }

        // Update existing items and spawn new ones.
        for (id, display) in &active_ids {
            if let Some(entry) = entries.get(id) {
                // Update existing item with latest display state.
                let display_clone = (*display).clone();
                entry
                    .handle
                    .update(move |tray| {
                        tray.display = display_clone;
                    })
                    .await;
            } else {
                // Spawn a new item for this provider.
                let tray = ProviderTray {
                    display: (*display).clone(),
                    ui_tx: self.ui_tx.clone(),
                    cmd_tx: self.cmd_tx.clone(),
                };
                match tray.spawn().await {
                    Ok(handle) => {
                        entries.insert(id.clone(), ProviderEntry { handle });
                    }
                    Err(e) => {
                        if !self
                            .warned_no_bus
                            .swap(true, std::sync::atomic::Ordering::Relaxed)
                        {
                            warn!("StatusNotifierItem unavailable (no SNI watcher on the bus): {e:#}; tray icons will not appear");
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Diffing helper (testable without a bus)
// ---------------------------------------------------------------------------

/// Compute the set of provider ids that would be added and removed given
/// the current active set and the new display list.
///
/// Returns `(to_add, to_remove)` as `Vec<String>`.
#[cfg(test)]
pub fn diff_providers(current: &[String], next: &[&str]) -> (Vec<String>, Vec<String>) {
    use std::collections::HashSet;
    let current_set: HashSet<&str> = current.iter().map(|s| s.as_str()).collect();
    let next_set: HashSet<&str> = next.iter().copied().collect();

    let to_add: Vec<String> = next_set
        .difference(&current_set)
        .map(|s| s.to_string())
        .collect();
    let to_remove: Vec<String> = current_set
        .difference(&next_set)
        .map(|s| s.to_string())
        .collect();
    (to_add, to_remove)
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

    // --- pixel conversion round-trip (RGBA source, tiny_skia native) ---

    #[test]
    fn pixel_conversion_opaque() {
        // Opaque red pixel in premultiplied RGBA8: [R=0xFF, G=0x00, B=0x00, A=0xFF]
        let raw = [0xFF_u8, 0x00, 0x00, 0xFF];
        let out = pixmap_to_ksni_argb32(&raw, 1, 1);
        // un-premultiply no-op (a==255), network byte order [A, R, G, B]
        assert_eq!(out, [0xFF, 0xFF, 0x00, 0x00]);
    }

    #[test]
    fn pixel_conversion_semitransparent() {
        // 50% transparent green: premultiplied R=0, G=0x80, B=0, A=0x80
        let raw = [0x00_u8, 0x80, 0x00, 0x80];
        let out = pixmap_to_ksni_argb32(&raw, 1, 1);
        assert_eq!(out[0], 0x80, "alpha");
        assert_eq!(out[1], 0x00, "red");
        assert!(out[2] >= 0xFE, "green un-premultiplied: {}", out[2]);
        assert_eq!(out[3], 0x00, "blue");
    }

    #[test]
    fn pixel_conversion_fully_transparent() {
        let raw = [0x00_u8, 0x00, 0x00, 0x00];
        let out = pixmap_to_ksni_argb32(&raw, 1, 1);
        assert_eq!(out, [0x00, 0x00, 0x00, 0x00]);
    }

    // --- tooltip formatting ---

    fn make_ready(name: &'static str, pcts: &[(&str, f64)]) -> ProviderDisplay {
        let windows = pcts
            .iter()
            .map(|(label, pct)| RateWindow {
                label: label.to_string(),
                used_percent: *pct,
                resets_at: None,
                caption: None,
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
        let desc = format_tooltip_description_multi(&[]);
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
        let desc = format_tooltip_description_multi(&displays);
        assert!(desc.contains("Claude:"), "desc={desc}");
        assert!(desc.contains("OpenAI:"), "desc={desc}");
        assert!(desc.contains('\n'), "should have newline: desc={desc}");
    }

    // --- per-provider tooltip ---

    #[test]
    fn per_provider_tooltip_session_weekly() {
        let d = make_ready("Claude", &[("Session", 62.0), ("Weekly", 31.0)]);
        let desc = format_tooltip_description(&d);
        assert!(desc.contains("62%"), "desc={desc}");
        assert!(desc.contains("31%"), "desc={desc}");
        assert!(desc.contains('·'), "should have separator: desc={desc}");
    }

    #[test]
    fn per_provider_tooltip_error() {
        let d = ProviderDisplay {
            id: "claude",
            name: "Claude",
            state: ProviderState::Error("timeout".into()),
        };
        let desc = format_tooltip_description(&d);
        assert!(desc.contains("error"), "desc={desc}");
        assert!(desc.contains("timeout"), "desc={desc}");
    }

    // --- diffing logic (no bus required) ---

    #[test]
    fn diff_adds_new_provider() {
        let (to_add, to_remove) = diff_providers(&[], &["claude"]);
        assert!(to_add.contains(&"claude".to_string()));
        assert!(to_remove.is_empty());
    }

    #[test]
    fn diff_removes_gone_provider() {
        let (to_add, to_remove) =
            diff_providers(&["claude".to_string(), "openai".to_string()], &["claude"]);
        assert!(to_add.is_empty());
        assert!(to_remove.contains(&"openai".to_string()));
    }

    #[test]
    fn diff_no_change() {
        let (to_add, to_remove) = diff_providers(&["claude".to_string()], &["claude"]);
        assert!(to_add.is_empty());
        assert!(to_remove.is_empty());
    }

    #[test]
    fn diff_swap_providers() {
        let (to_add, to_remove) = diff_providers(&["claude".to_string()], &["gemini"]);
        assert!(to_add.contains(&"gemini".to_string()));
        assert!(to_remove.contains(&"claude".to_string()));
    }

    // --- icon rendering smoke test (no bus, no display needed) ---

    #[test]
    fn render_icon_loading_returns_some() {
        let d = ProviderDisplay {
            id: "claude",
            name: "Claude",
            state: ProviderState::Loading,
        };
        let icon = render_provider_icon(22, &d);
        assert!(icon.is_some(), "expected an icon for loading state");
        let icon = icon.unwrap();
        assert_eq!(icon.width, 22);
        assert_eq!(icon.height, 22);
        assert_eq!(icon.data.len(), 22 * 22 * 4);
    }

    #[test]
    fn render_icon_ready_returns_some() {
        let d = make_ready("claude", &[("Session", 62.0)]);
        let icon = render_provider_icon(22, &d);
        assert!(icon.is_some());
        // At least some non-transparent pixels expected (ring is drawn)
        let icon = icon.unwrap();
        let has_visible = icon.data.chunks_exact(4).any(|px| px[0] > 0);
        assert!(has_visible, "icon should have visible pixels");
    }

    #[test]
    fn render_icon_error_has_red_dot() {
        let d = ProviderDisplay {
            id: "claude",
            name: "Claude",
            state: ProviderState::Error("fail".into()),
        };
        let icon = render_provider_icon(22, &d).unwrap();
        // Red channel should dominate in some pixel (error dot is #f38ba8)
        let has_reddish = icon.data.chunks_exact(4).any(|px| {
            // px is [A, R, G, B]; check a clearly reddish pixel with significant alpha
            px[0] > 100 && px[1] > 200 && px[2] < 200
        });
        assert!(has_reddish, "error icon should have reddish dot pixels");
    }
}
