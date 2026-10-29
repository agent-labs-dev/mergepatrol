//! Colours for each system appearance.

use gpui::{Hsla, WindowAppearance, rgb, rgba};

#[derive(Clone, Copy)]
pub struct Theme {
    /// Over the glass, which takes the colour of whatever is behind the window: keeps the
    /// text readable over a white page in dark mode, and a black one in light mode.
    pub tint: Hsla,
    pub fg: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub line: Hsla,
    pub track: Hsla,
    pub chip: Hsla,
    pub chip_hover: Hsla,
    pub good: Hsla,
    pub warn: Hsla,
    pub bad: Hsla,
    pub accent: Hsla,
    pub pill_bg: Hsla,
    pub pill_fg: Hsla,
}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self {
                tint: rgba(0x1e1e21b3).into(),
                fg: rgb(0xf2f2f2).into(),
                muted: rgb(0xa0a0a6).into(),
                faint: rgb(0x6e6e74).into(),
                line: rgba(0xffffff1a).into(),
                track: rgba(0xffffff1f).into(),
                chip: rgba(0xffffff14).into(),
                chip_hover: rgba(0xffffff22).into(),
                good: rgb(0x3fcf7f).into(),
                warn: rgb(0xf5b83d).into(),
                bad: rgb(0xff6b6b).into(),
                accent: rgb(0x6ea8ff).into(),
                pill_bg: rgb(0xffffff).into(),
                pill_fg: rgb(0x111111).into(),
            },
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self {
                tint: rgba(0xf4f4f6b3).into(),
                fg: rgb(0x1a1a1c).into(),
                muted: rgb(0x5f5f66).into(),
                faint: rgb(0x9a9aa0).into(),
                line: rgba(0x0000001a).into(),
                track: rgba(0x0000001a).into(),
                chip: rgba(0x0000000d).into(),
                chip_hover: rgba(0x00000017).into(),
                good: rgb(0x1f9d55).into(),
                warn: rgb(0xc88a12).into(),
                bad: rgb(0xd03b3b).into(),
                accent: rgb(0x2f6fdb).into(),
                pill_bg: rgb(0x1a1a1c).into(),
                pill_fg: rgb(0xffffff).into(),
            },
        }
    }
}
