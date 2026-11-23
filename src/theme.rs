//! Design tokens: one palette per Catppuccin flavour, selected at runtime.
//!
//! Colours are looked up through [`palette`] instead of being constants, so the
//! whole UI can switch between the dark flavours and the light one without a
//! restart. Values come from the official Catppuccin palette; the `*_soft`
//! tints and `accent_dim` are app-specific blends used for chips and borders.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

/// Type scale. Nothing renders below `TEXT_SM`: dense small text is what made
/// the first pass hard to read on a high-density display.
pub const TEXT_SM: f32 = 12.0;
pub const TEXT_BASE: f32 = 13.0;
pub const TEXT_LG: f32 = 15.0;

pub const STATUS_H: f32 = 26.0;

/// Windows draws with its own fonts. Elsewhere there is no font every
/// system has, so the app brings Inter and JetBrains Mono (both SIL OFL,
/// licenses in assets/fonts); scripts they lack (CJK) fall back to the
/// system's fonts.
#[cfg(windows)]
pub const FONT_UI: &str = "Segoe UI";
#[cfg(windows)]
pub const FONT_MONO: &str = "Consolas";
#[cfg(not(windows))]
pub const FONT_UI: &str = "Inter";
#[cfg(not(windows))]
pub const FONT_MONO: &str = "JetBrains Mono";

/// Registers the bundled fonts with GPUI, where the app uses them.
pub fn load_fonts(cx: &gpui::App) {
    #[cfg(not(windows))]
    {
        use std::borrow::Cow;
        let fonts: Vec<Cow<'static, [u8]>> = vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
        ];
        if let Err(error) = cx.text_system().add_fonts(fonts) {
            eprintln!("cannot load the bundled fonts: {error}");
        }
    }
    #[cfg(windows)]
    let _ = cx;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ThemeName {
    /// Follow the desktop's light/dark setting.
    #[default]
    System,
    Latte,
    Frappe,
    Macchiato,
    Mocha,
}

impl ThemeName {
    pub const ALL: [ThemeName; 5] = [
        ThemeName::System,
        ThemeName::Latte,
        ThemeName::Frappe,
        ThemeName::Macchiato,
        ThemeName::Mocha,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ThemeName::System => t!("theme.system"),
            ThemeName::Latte => "latte",
            ThemeName::Frappe => "frappe",
            ThemeName::Macchiato => "macchiato",
            ThemeName::Mocha => "mocha",
        }
    }

    /// The concrete flavour to use, resolving `System` against the desktop.
    pub fn resolve(self, dark_desktop: bool) -> ThemeName {
        match self {
            ThemeName::System => {
                if dark_desktop {
                    ThemeName::Mocha
                } else {
                    ThemeName::Latte
                }
            }
            other => other,
        }
    }

    fn index(self) -> usize {
        match self {
            ThemeName::Latte | ThemeName::System => 0,
            ThemeName::Frappe => 1,
            ThemeName::Macchiato => 2,
            ThemeName::Mocha => 3,
        }
    }
}

pub struct Palette {
    pub bg: u32,
    pub titlebar: u32,
    pub panel: u32,
    pub elevated: u32,
    pub hover: u32,
    pub border: u32,
    pub border_strong: u32,
    pub track: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_faint: u32,
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_dim: u32,
    pub accent_soft: u32,
    /// Text drawn on top of `accent`.
    pub on_accent: u32,
    pub danger: u32,
    pub danger_soft: u32,
    pub warn: u32,
    pub warn_soft: u32,
    pub info: u32,
    pub info_soft: u32,
}

pub const LATTE: Palette = Palette {
    bg: 0xeff1f5,
    titlebar: 0xdce0e8,
    panel: 0xe6e9ef,
    elevated: 0xccd0da,
    hover: 0xbcc0cc,
    border: 0xccd0da,
    border_strong: 0xacb0be,
    track: 0xccd0da,
    text: 0x4c4f69,
    text_dim: 0x5c5f77,
    text_faint: 0x6c6f85,
    accent: 0x40a02b,
    accent_hover: 0x179299,
    accent_dim: 0x7fb877,
    accent_soft: 0xd9e8d3,
    on_accent: 0x1e1e2e,
    danger: 0xd20f39,
    danger_soft: 0xf2d9df,
    warn: 0xdf8e1d,
    warn_soft: 0xf6e8c8,
    info: 0x1e66f5,
    info_soft: 0xdbe6fb,
};

pub const FRAPPE: Palette = Palette {
    bg: 0x303446,
    titlebar: 0x232634,
    panel: 0x292c3c,
    elevated: 0x414559,
    hover: 0x51576d,
    border: 0x414559,
    border_strong: 0x51576d,
    track: 0x414559,
    text: 0xc6d0f5,
    text_dim: 0xa5adce,
    text_faint: 0x949cbb,
    accent: 0xa6d189,
    accent_hover: 0x81c8be,
    accent_dim: 0x4f7a4a,
    accent_soft: 0x2c3b32,
    on_accent: 0x232634,
    danger: 0xe78284,
    danger_soft: 0x3f2a30,
    warn: 0xe5c890,
    warn_soft: 0x3b3529,
    info: 0x8caaee,
    info_soft: 0x252f47,
};

pub const MACCHIATO: Palette = Palette {
    bg: 0x24273a,
    titlebar: 0x181926,
    panel: 0x1e2030,
    elevated: 0x363a4f,
    hover: 0x494d64,
    border: 0x363a4f,
    border_strong: 0x494d64,
    track: 0x363a4f,
    text: 0xcad3f5,
    text_dim: 0xa5adcb,
    text_faint: 0x939ab7,
    accent: 0xa6da95,
    accent_hover: 0x8bd5ca,
    accent_dim: 0x4a7a52,
    accent_soft: 0x26352f,
    on_accent: 0x181926,
    danger: 0xed8796,
    danger_soft: 0x3c2833,
    warn: 0xeed49f,
    warn_soft: 0x3b3529,
    info: 0x8aadf4,
    info_soft: 0x202b44,
};

pub const MOCHA: Palette = Palette {
    bg: 0x1e1e2e,
    titlebar: 0x11111b,
    panel: 0x181825,
    elevated: 0x313244,
    hover: 0x45475a,
    border: 0x313244,
    border_strong: 0x45475a,
    track: 0x313244,
    text: 0xcdd6f4,
    text_dim: 0xa6adc8,
    text_faint: 0x9399b2,
    accent: 0xa6e3a1,
    accent_hover: 0x94e2d5,
    accent_dim: 0x3f7a4a,
    accent_soft: 0x22362c,
    on_accent: 0x11111b,
    danger: 0xf38ba8,
    danger_soft: 0x3a2430,
    warn: 0xf9e2af,
    warn_soft: 0x3a3325,
    info: 0x89b4fa,
    info_soft: 0x1e2a44,
};

pub const PALETTES: [Palette; 4] = [LATTE, FRAPPE, MACCHIATO, MOCHA];

static CURRENT: AtomicUsize = AtomicUsize::new(3);

/// The active palette. Cheap: one relaxed atomic load.
pub fn palette() -> &'static Palette {
    &PALETTES[CURRENT.load(Ordering::Relaxed)]
}

pub fn apply(flavour: ThemeName) {
    CURRENT.store(flavour.resolve(false).index(), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_flavour_has_its_own_palette() {
        let mut backgrounds = Vec::new();
        for flavour in [
            ThemeName::Latte,
            ThemeName::Frappe,
            ThemeName::Macchiato,
            ThemeName::Mocha,
        ] {
            backgrounds.push(PALETTES[flavour.index()].bg);
        }
        backgrounds.sort_unstable();
        backgrounds.dedup();
        assert_eq!(backgrounds.len(), 4, "flavours must not share a background");
    }

    /// Luminance of a packed `0xRRGGBB` colour, WCAG definition.
    fn luminance(colour: u32) -> f32 {
        let channel = |value: u32| {
            let value = value as f32 / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel((colour >> 16) & 0xff)
            + 0.7152 * channel((colour >> 8) & 0xff)
            + 0.0722 * channel(colour & 0xff)
    }

    #[test]
    fn latte_is_light_and_the_others_are_dark() {
        let light = luminance(LATTE.bg);
        assert!(light > 0.7, "latte background is not light: {light:.2}");
        for flavour in [FRAPPE, MACCHIATO, MOCHA] {
            let dark = luminance(flavour.bg);
            assert!(dark < 0.12, "dark background is not dark: {dark:.2}");
        }
    }

    #[test]
    fn system_follows_the_desktop() {
        assert_eq!(ThemeName::System.resolve(true), ThemeName::Mocha);
        assert_eq!(ThemeName::System.resolve(false), ThemeName::Latte);
        // An explicit choice is never overridden.
        assert_eq!(ThemeName::Frappe.resolve(false), ThemeName::Frappe);
        assert_eq!(ThemeName::Frappe.resolve(true), ThemeName::Frappe);
    }

    /// Body text must stay readable against the background it sits on.
    #[test]
    fn text_contrast_is_comfortable_in_every_flavour() {
        fn contrast(a: u32, b: u32) -> f32 {
            let (la, lb) = (luminance(a), luminance(b));
            let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
            (hi + 0.05) / (lo + 0.05)
        }

        for flavour in [
            ThemeName::Latte,
            ThemeName::Frappe,
            ThemeName::Macchiato,
            ThemeName::Mocha,
        ] {
            let palette = &PALETTES[flavour.index()];
            assert!(
                contrast(palette.text, palette.bg) >= 7.0,
                "{} body text is too dim: {:.1}:1",
                flavour.label(),
                contrast(palette.text, palette.bg)
            );
            // Secondary text carries hints and figures at 12px, so it has to
            // clear the normal-text threshold.
            assert!(
                contrast(palette.text_dim, palette.bg) >= 4.5,
                "{} secondary text is too dim: {:.1}:1",
                flavour.label(),
                contrast(palette.text_dim, palette.bg)
            );
            // Hints sit at the faint tier: not body copy, but still text a
            // person has to read, so it must not fall below AA for large text.
            assert!(
                contrast(palette.text_faint, palette.bg) >= 4.0,
                "{} hint text is washed out: {:.1}:1",
                flavour.label(),
                contrast(palette.text_faint, palette.bg)
            );
            assert!(
                contrast(palette.on_accent, palette.accent) >= 4.5,
                "{} accent text is unreadable: {:.1}:1",
                flavour.label(),
                contrast(palette.on_accent, palette.accent)
            );
        }
    }

    #[test]
    fn applying_a_flavour_switches_the_active_palette() {
        apply(ThemeName::Latte);
        assert_eq!(palette().bg, LATTE.bg);
        apply(ThemeName::Mocha);
        assert_eq!(palette().bg, MOCHA.bg);
    }
}
