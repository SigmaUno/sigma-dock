//! A deterministic default: GPUI's Linux font lookup expects a concrete family.
use gpui::{App, Global};
use std::borrow::Cow;

pub const DEFAULT_FAMILY: &str = "JetBrains Mono";
struct Registered;
impl Global for Registered {}

/// Register the embedded font once for an application, including terminal previews.
pub fn register(cx: &mut App) {
    if !cx.has_global::<Registered>() {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf")),
                Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf")),
                Cow::Borrowed(include_bytes!(
                    "../assets/fonts/JetBrainsMono-BoldItalic.ttf"
                )),
            ])
            .expect("register bundled terminal fonts");
        cx.set_global(Registered);
    }
}

pub(crate) fn family(requested: &str) -> &str {
    if requested.trim().eq_ignore_ascii_case("monospace") {
        DEFAULT_FAMILY
    } else {
        requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generic_family_uses_the_same_concrete_font_for_measurement_and_painting() {
        let renderer = crate::render::TerminalRenderer::new(
            "monospace".into(),
            gpui::px(14.),
            1.2,
            crate::colors::ColorPalette::default(),
        );
        assert_eq!(renderer.font_family, DEFAULT_FAMILY);
        assert_eq!(family(" Monospace "), DEFAULT_FAMILY);
        assert_eq!(family("Fira Code"), "Fira Code");
    }
}
