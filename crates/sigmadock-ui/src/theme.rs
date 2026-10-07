//! Neutral light chrome with Catppuccin Latte terminal colors, and Catppuccin Mocha for dark.
//! Catppuccin source and MIT license: packaging/licenses/Catppuccin.txt.
use crate::preferences::Appearance;
use gpui::WindowAppearance;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub base: u32,
    pub sidebar: u32,
    pub panel: u32,
    pub card: u32,
    pub button: u32,
    pub selection: u32,
    pub border: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
    pub success: u32,
    pub warning: u32,
    pub error: u32,
    pub focus: u32,
    pub link: u32,
    /// Board card background.
    pub surface: u32,
    /// Agent badge background.
    pub chip: u32,
    /// "Needs you" lane.
    pub attention: u32,
    /// "In review" lane.
    pub review: u32,
    pub ansi: [u32; 16],
}
impl Theme {
    pub fn light() -> Self {
        Self {
            base: 0xf6f7f9,
            sidebar: 0xf1f2f5,
            panel: 0xeceef2,
            card: 0xe6e8ec,
            button: 0xe6e8ec,
            selection: 0xdfe8fb,
            border: 0xdfe2e7,
            text: 0x1f2329,
            muted: 0x5c636e,
            accent: 0x1a64d6,
            success: 0x2b8a3e,
            warning: 0xb35c00,
            error: 0xcf222e,
            focus: 0x3b82f6,
            link: 0x1a64d6,
            surface: 0xffffff,
            chip: 0xeceef1,
            attention: 0xc75f00,
            review: 0x7c5cd6,
            ansi: [
                0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
                0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
            ],
        }
    }
    pub fn mocha() -> Self {
        Self {
            base: 0x1e1e2e,
            sidebar: 0x11111b,
            panel: 0x181825,
            card: 0x313244,
            button: 0x45475a,
            selection: 0x585b70,
            border: 0x6c7086,
            text: 0xcdd6f4,
            muted: 0xbac2de,
            accent: 0xcba6f7,
            success: 0xa6e3a1,
            warning: 0xf9e2af,
            error: 0xf38ba8,
            focus: 0xb4befe,
            link: 0x89b4fa,
            surface: 0x181825,
            chip: 0x313244,
            attention: 0xfab387,
            review: 0xcba6f7,
            ansi: [
                0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
                0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
            ],
        }
    }
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::mocha(),
        }
    }
    pub fn terminal(self, appearance: &Appearance) -> Appearance {
        let mut effective = appearance.clone();
        if effective.follow_system {
            effective.background = self.base;
            effective.foreground = self.text;
            effective.ansi = self.ansi;
        }
        effective
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn system_modes_choose_the_matching_palette() {
        assert_eq!(
            Theme::for_appearance(WindowAppearance::Light).base,
            0xf6f7f9
        );
        assert_eq!(Theme::for_appearance(WindowAppearance::Dark).base, 0x1e1e2e);
        assert_eq!(
            Theme::for_appearance(WindowAppearance::VibrantLight),
            Theme::light()
        );
        assert_eq!(
            Theme::for_appearance(WindowAppearance::VibrantDark),
            Theme::mocha()
        );
    }
    #[test]
    fn custom_terminal_colors_survive_system_changes() {
        let mut appearance = Appearance {
            background: 0x123456,
            follow_system: false,
            ..Appearance::default()
        };
        assert_eq!(Theme::light().terminal(&appearance).background, 0x123456);
        assert_eq!(Theme::mocha().terminal(&appearance).background, 0x123456);
        appearance.follow_system = true;
        assert_eq!(
            Theme::light().terminal(&appearance).background,
            Theme::light().base
        );
        assert_eq!(
            Theme::mocha().terminal(&appearance).background,
            Theme::mocha().base
        );
    }
    #[test]
    fn main_text_and_accent_button_contrast_is_readable() {
        fn luminance(hex: u32) -> f64 {
            let channel = |shift| {
                let s = ((hex >> shift) & 255u32) as f64 / 255.;
                if s <= 0.04045 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
        }
        let contrast = |a, b| {
            let a = luminance(a);
            let b = luminance(b);
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        };
        for theme in [Theme::light(), Theme::mocha()] {
            assert!(contrast(theme.text, theme.base) >= 4.5);
            assert!(contrast(theme.muted, theme.panel) >= 4.5);
            assert!(contrast(theme.base, theme.accent) >= 4.5);
            assert!(contrast(theme.muted, theme.surface) >= 4.5);
            for lane in [theme.link, theme.attention, theme.review, theme.success] {
                assert!(contrast(lane, theme.surface) >= 3.0);
            }
        }
    }
}
