//! Embedded provider logos (SVGs from the upstream CodexBar project, MIT)
//! rasterized for the tray and the popover.

use resvg::tiny_skia::Pixmap;
use resvg::usvg;

fn svg_bytes(provider_id: &str) -> Option<&'static [u8]> {
    Some(match provider_id {
        "claude" => include_bytes!("../assets/provider-icons/claude.svg"),
        "codex" => include_bytes!("../assets/provider-icons/codex.svg"),
        // Upstream has no dedicated OpenAI glyph; it reuses the Codex one.
        "openai" => include_bytes!("../assets/provider-icons/codex.svg"),
        "gemini" => include_bytes!("../assets/provider-icons/gemini.svg"),
        "copilot" => include_bytes!("../assets/provider-icons/copilot.svg"),
        "cursor" => include_bytes!("../assets/provider-icons/cursor.svg"),
        "openrouter" => include_bytes!("../assets/provider-icons/openrouter.svg"),
        "mistral" => include_bytes!("../assets/provider-icons/mistral.svg"),
        "deepseek" => include_bytes!("../assets/provider-icons/deepseek.svg"),
        "groq" => include_bytes!("../assets/provider-icons/groq.svg"),
        "grok" => include_bytes!("../assets/provider-icons/grok.svg"),
        "perplexity" => include_bytes!("../assets/provider-icons/perplexity.svg"),
        _ => return None,
    })
}

/// Rasterize a provider logo to `size`x`size`. The result is RGBA8
/// **premultiplied** (tiny-skia native). With `tint`, every pixel's color is
/// replaced by the tint while keeping the alpha (logos are monochrome).
pub fn logo_pixmap(provider_id: &str, size: u32, tint: Option<[u8; 3]>) -> Option<Pixmap> {
    let data = svg_bytes(provider_id)?;
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let mut pixmap = Pixmap::new(size, size)?;

    let view = tree.size();
    let scale = (size as f32 / view.width()).min(size as f32 / view.height());
    let tx = (size as f32 - view.width() * scale) / 2.0;
    let ty = (size as f32 - view.height() * scale) / 2.0;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    if let Some([r, g, b]) = tint {
        for px in pixmap.data_mut().chunks_exact_mut(4) {
            let a = px[3] as u16;
            // Premultiplied: channel = tint * alpha / 255.
            px[0] = ((r as u16 * a) / 255) as u8;
            px[1] = ((g as u16 * a) / 255) as u8;
            px[2] = ((b as u16 * a) / 255) as u8;
        }
    }
    Some(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_logos_render_nonempty() {
        for id in ["claude", "codex", "gemini", "copilot", "openai"] {
            let pm = logo_pixmap(id, 22, Some([255, 255, 255])).expect(id);
            assert!(
                pm.data().chunks_exact(4).any(|px| px[3] > 0),
                "{id}: all pixels transparent"
            );
        }
    }

    #[test]
    fn unknown_logo_is_none() {
        assert!(logo_pixmap("nope", 22, None).is_none());
    }
}
