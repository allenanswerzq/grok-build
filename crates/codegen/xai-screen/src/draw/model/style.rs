//! The desktop's look that a drawing follows: theme colors, line width, rounding, and font.

use serde::{Deserialize, Serialize};

use super::Color;

/// An RGB color.
pub type Rgb = [u8; 3];

/// Theme colors. [`Color`] names pick one; `background` and `foreground` style text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    pub accent: Rgb,
    pub red: Rgb,
    pub yellow: Rgb,
    pub green: Rgb,
    pub blue: Rgb,
    pub background: Rgb,
    pub foreground: Rgb,
}

impl Default for Palette {
    /// Tokyo Night, Omarchy's default theme.
    fn default() -> Self {
        Self {
            accent: [0x7a, 0xa2, 0xf7],
            red: [0xf7, 0x76, 0x8e],
            yellow: [0xe0, 0xaf, 0x68],
            green: [0x9e, 0xce, 0x6a],
            blue: [0x7a, 0xa2, 0xf7],
            background: [0x1a, 0x1b, 0x26],
            foreground: [0xa9, 0xb1, 0xd6],
        }
    }
}

impl Palette {
    /// The RGB value of a theme color name.
    pub fn rgb(&self, color: Color) -> Rgb {
        match color {
            Color::Accent => self.accent,
            Color::Red => self.red,
            Color::Yellow => self.yellow,
            Color::Green => self.green,
            Color::Blue => self.blue,
            Color::Rgb(rgb) => rgb,
        }
    }
}

/// The RGB value of `#rrggbb`.
pub fn hex(value: &str) -> Option<Rgb> {
    let digits = value
        .strip_prefix('#')
        .filter(|digits| digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let [_, r, g, b] = u32::from_str_radix(digits, 16).ok()?.to_be_bytes();
    Some([r, g, b])
}

/// How drawings look. `xai-screen` reads it from the desktop on every draw, so theme changes
/// apply at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    /// Line width in logical units: Hyprland's window border size.
    pub line_width: f64,
    /// Corner radius in logical units: Hyprland's window rounding.
    pub rounding: f64,
    /// Font family for labels and text: the system monospace font.
    pub font: String,
    pub font_size: f64,
    pub palette: Palette,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            line_width: 2.0,
            rounding: 0.0,
            font: "monospace".to_owned(),
            font_size: 14.0,
            palette: Palette::default(),
        }
    }
}
