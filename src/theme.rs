//! Appearance: dark/light/system mode + accent color, persisted in <config>/zym-player/settings.json.
//! Panels, menus, popovers and dialogs read colors from `t()`. The video area and the control bar
//! over it stay dark in both modes (white icons on video need a dark backing); the accent applies everywhere.

use std::sync::{PoisonError, RwLock};

use gpui::{Rgba, WindowAppearance, rgb, rgba};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    System,
    Dark,
    Light,
}

/// Accent presets (name, 0xRRGGBB). Picked for contrast on both dark and light surfaces.
pub const ACCENTS: [(&str, u32); 8] = [
    ("Blue", 0x4f8cff),
    ("Violet", 0x8b5cf6),
    ("Pink", 0xec4899),
    ("Red", 0xef4444),
    ("Orange", 0xf97316),
    ("Green", 0x22c55e),
    ("Teal", 0x14b8a6),
    ("Slate", 0x64748b),
];

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default = "default_accent")]
    pub accent: u32,
}

fn default_accent() -> u32 {
    ACCENTS[0].1
}

impl Default for Prefs {
    fn default() -> Self {
        Self { mode: Mode::System, accent: default_accent() }
    }
}

/// Resolved palette (0xRRGGBB / 0xRRGGBBAA).
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    #[cfg_attr(not(test), allow(dead_code))]
    pub dark: bool,
    pub accent: u32,
    pub fg: u32,
    pub muted: u32,
    pub dim: u32,
    /// Translucent panel background (playlist, explorer, popovers) — RGBA.
    pub surface: u32,
    /// Opaque surface (menus, dialogs).
    pub surface_solid: u32,
    /// Overlays tinted from the foreground: hover/pressed/border/field — RGBA.
    pub hover: u32,
    pub pressed: u32,
    pub border: u32,
    pub field: u32,
    /// Error text on the theme's surfaces.
    pub error: u32,
}

impl Theme {
    pub fn new(dark: bool, accent: u32) -> Self {
        if dark {
            Self {
                dark,
                accent,
                fg: 0xe8e8e8,
                muted: 0x9a9a9a,
                dim: 0x767676,
                surface: 0x141414ea,
                surface_solid: 0x1f1f1f,
                hover: 0xffffff12,
                pressed: 0xffffff24,
                border: 0xffffff1a,
                field: 0xffffff10,
                error: 0xff8080,
            }
        } else {
            Self {
                dark,
                accent,
                fg: 0x1c1c1e,
                muted: 0x5c5c63,
                dim: 0x86868c,
                surface: 0xf4f4f6ea, // same 92% as dark; much lower and video-area text bleeds through
                surface_solid: 0xffffff,
                hover: 0x0000000d,
                pressed: 0x0000001c,
                border: 0x00000020,
                field: 0x0000000a,
                error: 0xc62828,
            }
        }
    }

    pub fn fg(&self) -> Rgba { rgb(self.fg) }
    pub fn muted(&self) -> Rgba { rgb(self.muted) }
    pub fn dim(&self) -> Rgba { rgb(self.dim) }
    pub fn surface(&self) -> Rgba { rgba(self.surface) }
    pub fn surface_solid(&self) -> Rgba { rgb(self.surface_solid) }
    pub fn hover(&self) -> Rgba { rgba(self.hover) }
    pub fn pressed(&self) -> Rgba { rgba(self.pressed) }
    pub fn border(&self) -> Rgba { rgba(self.border) }
    pub fn field(&self) -> Rgba { rgba(self.field) }
    pub fn error(&self) -> Rgba { rgb(self.error) }
    pub fn accent(&self) -> Rgba { rgb(self.accent) }
    /// Accent at `alpha` (0..=255), for selected-row / chip backgrounds.
    pub fn accent_a(&self, alpha: u8) -> Rgba { rgba(self.accent << 8 | alpha as u32) }
}

static PREFS: RwLock<Prefs> = RwLock::new(Prefs { mode: Mode::System, accent: 0x4f8cff });
static CURRENT: RwLock<Option<Theme>> = RwLock::new(None);

/// Current palette (dark until the first `resolve`).
pub fn t() -> Theme {
    CURRENT.read().unwrap_or_else(PoisonError::into_inner).unwrap_or_else(|| Theme::new(true, prefs().accent))
}

pub fn prefs() -> Prefs {
    *PREFS.read().unwrap_or_else(PoisonError::into_inner)
}

/// Re-resolve against the OS appearance (for System mode). Cheap; called every UI tick.
pub fn resolve(os: WindowAppearance) {
    let p = prefs();
    let dark = match p.mode {
        Mode::Dark => true,
        Mode::Light => false,
        Mode::System => matches!(os, WindowAppearance::Dark | WindowAppearance::VibrantDark),
    };
    *CURRENT.write().unwrap_or_else(PoisonError::into_inner) = Some(Theme::new(dark, p.accent));
}

/// Load saved prefs (missing/invalid -> defaults).
pub fn load() {
    let p = crate::settings::get("appearance").and_then(|a| serde_json::from_value(a).ok()).unwrap_or_default();
    *PREFS.write().unwrap_or_else(PoisonError::into_inner) = p;
}

/// Change + persist.
pub fn set(p: Prefs) {
    *PREFS.write().unwrap_or_else(PoisonError::into_inner) = p;
    crate::settings::put("appearance", serde_json::to_value(p).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_resolve_and_prefs_roundtrip() {
        *PREFS.write().unwrap_or_else(PoisonError::into_inner) = Prefs { mode: Mode::System, accent: 0x22c55e };
        resolve(WindowAppearance::Light);
        assert!(!t().dark);
        resolve(WindowAppearance::VibrantDark);
        assert!(t().dark);
        *PREFS.write().unwrap_or_else(PoisonError::into_inner) = Prefs { mode: Mode::Light, accent: 0x22c55e };
        resolve(WindowAppearance::Dark);
        assert!(!t().dark, "explicit Light ignores the OS");
        assert_eq!(t().accent, 0x22c55e);
        assert_eq!(t().accent_a(0x40), rgba(0x22c55e40));

        let json = serde_json::to_string(&Prefs { mode: Mode::Dark, accent: 0x8b5cf6 }).unwrap();
        assert_eq!(json, r#"{"mode":"dark","accent":9133302}"#);
        let back: Prefs = serde_json::from_str(r#"{"mode":"light"}"#).unwrap();
        assert_eq!((back.mode, back.accent), (Mode::Light, 0x4f8cff)); // missing accent -> default
    }
}
