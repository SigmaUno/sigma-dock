//! Local appearance preferences. No network access or agent-session ownership.
use anyhow::{Context, Result, bail};
use gpui::{Edges, px, rgb};
use serde::{Deserialize, Serialize};
use sigma_dock_terminal::{ColorPalette, CursorShape, TerminalConfig};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum Cursor {
    #[default]
    Block,
    Underline,
    Beam,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    pub font: String,
    pub size: f32,
    pub line_height: f32,
    pub padding: f32,
    pub foreground: u32,
    pub background: u32,
    pub ansi: [u32; 16],
    pub cursor: Cursor,
    pub blink: bool,
}
impl Default for Appearance {
    fn default() -> Self {
        let palette = ColorPalette::default();
        let hex = |color: gpui::Hsla| {
            let c = color.to_rgb();
            ((c.r * 255.).round() as u32) << 16
                | ((c.g * 255.).round() as u32) << 8
                | (c.b * 255.).round() as u32
        };
        Self {
            font: "monospace".into(),
            size: 14.,
            line_height: 1.2,
            padding: 0.,
            foreground: hex(palette.foreground()),
            background: hex(palette.background()),
            ansi: palette.ansi_colors().map(hex),
            cursor: Cursor::Block,
            blink: false,
        }
    }
}
impl Appearance {
    pub fn validate(&self) -> Result<()> {
        if self.font.trim().is_empty()
            || self.font.len() > 128
            || self.font.chars().any(char::is_control)
            || !self.size.is_finite()
            || !(8. ..=40.).contains(&self.size)
            || !self.line_height.is_finite()
            || !(1. ..=2.).contains(&self.line_height)
            || !self.padding.is_finite()
            || !(0. ..=32.).contains(&self.padding)
            || [self.background, self.foreground]
                .into_iter()
                .chain(self.ansi)
                .any(|c| c > 0xffffff)
        {
            bail!("Invalid terminal appearance preferences");
        }
        Ok(())
    }
    pub fn apply(&self, mut config: TerminalConfig) -> TerminalConfig {
        let bytes = |color: u32| ((color >> 16) as u8, (color >> 8) as u8, color as u8);
        let (r, g, b) = bytes(self.background);
        let builder = ColorPalette::builder().background(r, g, b);
        let (r, g, b) = bytes(self.foreground);
        config.colors = builder
            .foreground(r, g, b)
            .cursor(r, g, b)
            .ansi_colors(self.ansi.map(|color| rgb(color).into()))
            .build();
        config.font_family = self.font.clone();
        config.font_size = px(self.size);
        config.line_height_multiplier = self.line_height;
        config.padding = Edges::all(px(self.padding));
        config.cursor_shape = match self.cursor {
            Cursor::Block => CursorShape::Block,
            Cursor::Underline => CursorShape::Underline,
            Cursor::Beam => CursorShape::Beam,
        };
        config.cursor_blink = self.blink;
        config
    }
    pub fn light() -> Self {
        Self {
            background: 0xf7f7f7,
            foreground: 0x202020,
            ansi: [
                0x202020, 0xb02020, 0x267020, 0x806000, 0x2040a0, 0x803080, 0x007080, 0x909090,
                0x606060, 0xd03030, 0x308020, 0x907000, 0x3050c0, 0x904090, 0x008090, 0xffffff,
            ],
            ..Self::default()
        }
    }
    pub fn color(&self, index: usize) -> u32 {
        match index {
            0 => self.background,
            1 => self.foreground,
            _ => self.ansi[index - 2],
        }
    }
    pub fn set_color(&mut self, index: usize, value: u32) {
        match index {
            0 => self.background = value,
            1 => self.foreground = value,
            _ => self.ansi[index - 2] = value,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Preferences {
    pub appearance: Appearance,
}
impl Preferences {
    pub fn load(path: &Path) -> Result<Self> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            bail!("Preferences file is too large");
        }
        let preferences: Self = serde_json::from_slice(&bytes).context("Read preferences")?;
        preferences.appearance.validate()?;
        Ok(preferences)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.appearance.validate()?;
        fs::create_dir_all(path.parent().context("Preferences path has no parent")?)?;
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut file = options.open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.context("Save preferences")
    }
}
pub fn parse_color(text: &str) -> Option<u32> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    (hex.len() == 6 && hex.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colors_require_a_complete_rgb_value() {
        assert_eq!(parse_color("#aBc123"), Some(0xabc123));
        for value in ["123", "#gg0000", "1234567", " 112233"] {
            assert_eq!(parse_color(value), None);
        }
    }
    #[test]
    fn reject_unsafe_or_unusable_preferences() {
        let mut value = Appearance {
            size: f32::NAN,
            ..Appearance::default()
        };
        assert!(value.validate().is_err());
        value = Appearance::default();
        value.font.clear();
        assert!(value.validate().is_err());
        value = Appearance::default();
        value.ansi[15] = 0x1000000;
        assert!(value.validate().is_err());
    }
    #[test]
    fn preferences_survive_restart_and_apply_to_new_terminal() {
        let path = std::env::temp_dir().join(format!(
            "sigma-prefs-{}-{:?}/preferences.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let preferences = Preferences {
            appearance: Appearance {
                cursor: Cursor::Beam,
                ..Appearance::light()
            },
        };
        preferences.save(&path).unwrap();
        assert_eq!(preferences, Preferences::load(&path).unwrap());
        let config = preferences.appearance.apply(TerminalConfig::default());
        assert_eq!(config.cursor_shape, CursorShape::Beam);
        assert_eq!(
            config.colors.ansi_colors(),
            &preferences.appearance.ansi.map(|c| rgb(c).into())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn applying_appearance_keeps_dimensions_and_scrollback() {
        let config = TerminalConfig {
            cols: 123,
            rows: 45,
            scrollback: 9876,
            ..TerminalConfig::default()
        };
        let updated = Appearance::light().apply(config);
        assert_eq!(
            (updated.cols, updated.rows, updated.scrollback),
            (123, 45, 9876)
        );
    }
}
