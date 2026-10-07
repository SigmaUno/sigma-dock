//! Catppuccin Latte/Mocha. Source and MIT license: packaging/licenses/Catppuccin.txt.
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
    pub ansi: [u32; 16],
}
impl Theme {
    pub fn latte() -> Self {
        Self {
            base: 0xeff1f5,
            sidebar: 0xdce0e8,
            panel: 0xe6e9ef,
            card: 0xccd0da,
            button: 0xbcc0cc,
            selection: 0xacb0be,
            border: 0x9ca0b0,
            text: 0x4c4f69,
            muted: 0x5c5f77,
            accent: 0x8839ef,
            success: 0x40a02b,
            warning: 0xdf8e1d,
            error: 0xd20f39,
            focus: 0x7287fd,
            link: 0x1e66f5,
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
            ansi: [
                0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
                0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
            ],
        }
    }
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::latte(),
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
    fn system_modes_choose_the_official_palette() {
        assert_eq!(
            Theme::for_appearance(WindowAppearance::Light).base,
            0xeff1f5
        );
        assert_eq!(Theme::for_appearance(WindowAppearance::Dark).base, 0x1e1e2e);
        assert_eq!(
            Theme::for_appearance(WindowAppearance::VibrantLight),
            Theme::latte()
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
        assert_eq!(Theme::latte().terminal(&appearance).background, 0x123456);
        assert_eq!(Theme::mocha().terminal(&appearance).background, 0x123456);
        appearance.follow_system = true;
        assert_eq!(
            Theme::latte().terminal(&appearance).background,
            Theme::latte().base
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
        for theme in [Theme::latte(), Theme::mocha()] {
            assert!(contrast(theme.text, theme.base) >= 4.5);
            assert!(contrast(theme.muted, theme.panel) >= 4.5);
            assert!(contrast(theme.base, theme.accent) >= 4.5);
        }
    }
}
