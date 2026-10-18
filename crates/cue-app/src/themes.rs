//! The overlay's built-in color themes. All of them are dark, because the question box
//! and code blocks use the app's dark component theme.

use gpui_kit::{Hsla, Rgba, rgb, rgba};

pub struct OverlayTheme {
    pub id: &'static str,
    pub name: &'static str,
    /// Panel background; the opacity setting is applied on top.
    background: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
    pub error: u32,
}

pub const THEMES: [OverlayTheme; 6] = [
    OverlayTheme {
        id: "midnight",
        name: "Midnight",
        background: 0x101218,
        text: 0xe8eaed,
        muted: 0x9aa0a6,
        accent: 0x8ab4f8,
        error: 0xf28b82,
    },
    OverlayTheme {
        id: "graphite",
        name: "Graphite",
        background: 0x1c1c1e,
        text: 0xf2f2f7,
        muted: 0x98989f,
        accent: 0x64a8ff,
        error: 0xff6961,
    },
    OverlayTheme {
        id: "ocean",
        name: "Ocean",
        background: 0x0b1626,
        text: 0xe1ecf7,
        muted: 0x8aa4c0,
        accent: 0x4fc3f7,
        error: 0xff8a80,
    },
    OverlayTheme {
        id: "forest",
        name: "Forest",
        background: 0x0f1a14,
        text: 0xe3f1e8,
        muted: 0x8fb39d,
        accent: 0x6fcf97,
        error: 0xff8a80,
    },
    OverlayTheme {
        id: "ember",
        name: "Ember",
        background: 0x1d120e,
        text: 0xf6e9e2,
        muted: 0xbf9c8c,
        accent: 0xff9966,
        error: 0xff6b6b,
    },
    OverlayTheme {
        id: "high-contrast",
        name: "High contrast",
        background: 0x000000,
        text: 0xffffff,
        muted: 0xd0d0d0,
        accent: 0xffd60a,
        error: 0xff5c5c,
    },
];

/// The theme with this id, or the default for unknown ids.
pub fn find(id: &str) -> &'static OverlayTheme {
    THEMES
        .iter()
        .find(|theme| theme.id == id)
        .unwrap_or(&THEMES[0])
}

impl OverlayTheme {
    pub fn background(&self, opacity: f32) -> Rgba {
        let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
        rgba(self.background << 8 | alpha)
    }

    /// A solid swatch of the background, for the theme picker.
    pub fn swatch(&self) -> Hsla {
        rgb(self.background).into()
    }

    pub fn border(&self) -> Rgba {
        rgba(self.text << 8 | 0x26)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_ids_get_the_default_theme() {
        assert_eq!(find("ocean").name, "Ocean");
        assert_eq!(find("no-such-theme").id, "midnight");
    }

    #[test]
    fn opacity_becomes_the_background_alpha() {
        let theme = find("midnight");
        assert_eq!(theme.background(1.0), rgba(0x101218ff));
        assert_eq!(theme.background(0.5), rgba(0x10121880));
    }
}
