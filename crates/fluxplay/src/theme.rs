//! Material 3 Expressive tokens — accent presets + day/night surfaces.
//! Inspired by M3 Expressive and IPTV clients (TiviMate / MYTV / Smarters).

use fluxplay_core::models::AccentPreset;
use iced::theme::{Palette, Theme};
use iced::Color;

/// Resolved palette for one paint pass (Copy — cheap to pass through views).
#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub day: bool,
    pub accent: AccentPreset,
}

impl UiTheme {
    pub fn new(day: bool, accent: AccentPreset) -> Self {
        Self { day, accent }
    }

    pub fn iced_theme(self) -> Theme {
        let (bg, text, primary) = match (self.day, self.accent) {
            (true, a) => (
                self.shell(),
                Color::from_rgb8(0x0F, 0x17, 0x22),
                a.primary_day(),
            ),
            (false, a) => (
                self.shell(),
                Color::from_rgb8(0xE8, 0xEF, 0xF6),
                a.primary_night(),
            ),
        };
        Theme::custom(
            if self.day { "FluxPlay Day" } else { "FluxPlay Night" },
            Palette {
                background: bg,
                text,
                primary,
                success: Color::from_rgb8(0x1B, 0x8A, 0x3E),
                warning: Color::from_rgb8(0xC4, 0x6B, 0x00),
                danger: Color::from_rgb8(0xBA, 0x1A, 0x1A),
            },
        )
    }

    // --- Primary (accent) ---
    pub fn accent(self) -> Color {
        self.primary()
    }

    pub fn primary(self) -> Color {
        if self.day {
            self.accent.primary_day()
        } else {
            self.accent.primary_night()
        }
    }

    pub fn on_primary(self) -> Color {
        if self.day {
            Color::from_rgb8(0xFF, 0xFF, 0xFF)
        } else {
            self.accent.on_primary_night()
        }
    }

    pub fn primary_container(self) -> Color {
        if self.day {
            self.accent.container_day()
        } else {
            self.accent.container_night()
        }
    }

    pub fn on_primary_container(self) -> Color {
        if self.day {
            self.accent.on_container_day()
        } else {
            self.accent.on_container_night()
        }
    }

    // --- Secondary (selection / nav / chips) ---
    pub fn secondary(self) -> Color {
        let p = self.primary();
        if self.day {
            Color::from_rgb(
                (p.r * 0.35 + 0.25).min(1.0),
                (p.g * 0.35 + 0.28).min(1.0),
                (p.b * 0.35 + 0.32).min(1.0),
            )
        } else {
            Color::from_rgb(
                (p.r * 0.55 + 0.35).min(1.0),
                (p.g * 0.55 + 0.38).min(1.0),
                (p.b * 0.55 + 0.42).min(1.0),
            )
        }
    }

    pub fn on_secondary(self) -> Color {
        self.on_primary()
    }

    pub fn secondary_container(self) -> Color {
        let p = self.primary();
        if self.day {
            Color::from_rgba(p.r, p.g, p.b, 0.18)
        } else {
            // Opaque enough for onSecondaryContainer contrast on dark shell.
            Color::from_rgb(
                (p.r * 0.35 + 0.08).min(1.0),
                (p.g * 0.28 + 0.07).min(1.0),
                (p.b * 0.22 + 0.06).min(1.0),
            )
        }
    }

    pub fn on_secondary_container(self) -> Color {
        if self.day {
            self.ink()
        } else {
            Color::from_rgba(self.ink().r, self.ink().g, self.ink().b, 0.92)
        }
    }

    // --- Tertiary (favorites / soft badges) ---
    pub fn tertiary(self) -> Color {
        self.favorite()
    }

    pub fn on_tertiary(self) -> Color {
        if self.day {
            Color::from_rgb8(0x3A, 0x2A, 0x00)
        } else {
            Color::from_rgb8(0x2A, 0x1E, 0x00)
        }
    }

    pub fn tertiary_container(self) -> Color {
        let t = self.favorite();
        Color::from_rgba(t.r, t.g, t.b, if self.day { 0.22 } else { 0.28 })
    }

    pub fn on_tertiary_container(self) -> Color {
        self.on_tertiary()
    }

    // --- Error / LIVE ---
    pub fn error(self) -> Color {
        self.live()
    }

    pub fn on_error(self) -> Color {
        Color::from_rgb8(0xFF, 0xFF, 0xFF)
    }

    pub fn error_container(self) -> Color {
        let e = self.live();
        Color::from_rgba(e.r, e.g, e.b, if self.day { 0.18 } else { 0.28 })
    }

    pub fn on_error_container(self) -> Color {
        self.live()
    }

    /// LIVE badge / recording indicator (`error` role).
    pub fn live(self) -> Color {
        if self.day {
            Color::from_rgb8(0xBA, 0x1A, 0x1A)
        } else {
            Color::from_rgb8(0xFF, 0xB4, 0xAB)
        }
    }

    /// Favorite star (`tertiary`).
    pub fn favorite(self) -> Color {
        if self.day {
            Color::from_rgb8(0xC9, 0x8A, 0x00)
        } else {
            Color::from_rgb8(0xF5, 0xC5, 0x42)
        }
    }

    // --- Surfaces ---
    /// M3 `surfaceDim` — shell canvas behind panes / nav.
    pub fn shell(self) -> Color {
        self.surface_dim()
    }

    pub fn surface_dim(self) -> Color {
        if self.day {
            Color::from_rgb8(0xE4, 0xEA, 0xF2)
        } else {
            Color::from_rgb8(0x05, 0x08, 0x0E)
        }
    }

    pub fn surface(self) -> Color {
        if self.day {
            Color::from_rgb8(0xFF, 0xFF, 0xFF)
        } else {
            Color::from_rgb8(0x11, 0x16, 0x1F)
        }
    }

    pub fn surface_bright(self) -> Color {
        if self.day {
            Color::from_rgb8(0xFF, 0xFF, 0xFF)
        } else {
            Color::from_rgb8(0x1C, 0x25, 0x34)
        }
    }

    pub fn on_surface(self) -> Color {
        self.ink()
    }

    pub fn on_surface_variant(self) -> Color {
        self.ink_muted()
    }

    pub fn ink(self) -> Color {
        if self.day {
            Color::from_rgb8(0x0F, 0x17, 0x22)
        } else {
            Color::from_rgb8(0xE8, 0xEF, 0xF6)
        }
    }

    pub fn ink_muted(self) -> Color {
        if self.day {
            Color::from_rgba8(0x0F, 0x17, 0x22, 0.58)
        } else {
            Color::from_rgba8(0xE8, 0xEF, 0xF6, 0.55)
        }
    }

    pub fn surface_container_lowest(self) -> Color {
        if self.day {
            Color::from_rgb8(0xFF, 0xFF, 0xFF)
        } else {
            Color::from_rgb8(0x08, 0x0B, 0x12)
        }
    }

    pub fn surface_container_low(self) -> Color {
        if self.day {
            Color::from_rgb8(0xF0, 0xF4, 0xF8)
        } else {
            Color::from_rgb8(0x0E, 0x13, 0x1B)
        }
    }

    /// Default nav / chrome container (`surfaceContainer`).
    pub fn surface_container(self) -> Color {
        if self.day {
            Color::from_rgb8(0xEA, 0xF0, 0xF6)
        } else {
            Color::from_rgb8(0x14, 0x1A, 0x24)
        }
    }

    pub fn surface_container_high(self) -> Color {
        if self.day {
            Color::from_rgb8(0xE6, 0xEC, 0xF3)
        } else {
            Color::from_rgb8(0x1A, 0x22, 0x30)
        }
    }

    pub fn surface_container_highest(self) -> Color {
        if self.day {
            Color::from_rgb8(0xDE, 0xE5, 0xEE)
        } else {
            Color::from_rgb8(0x22, 0x2B, 0x3A)
        }
    }

    /// Alias — nested / muted surface.
    pub fn surface_muted(self) -> Color {
        self.surface_container_low()
    }

    pub fn surface_elevated(self) -> Color {
        self.surface_container_high()
    }

    /// Nav / top bar chrome — always `surfaceContainer` (stable across breakpoints).
    pub fn chrome_surface(self) -> Color {
        self.surface_container()
    }

    /// Selected row fill (`primaryContainer` soft).
    pub fn selection_fill(self) -> Color {
        let c = self.primary_container();
        Color::from_rgba(c.r, c.g, c.b, if self.day { 0.55 } else { 0.45 })
    }

    // --- Outline ---
    pub fn outline(self) -> Color {
        if self.day {
            Color::from_rgba8(0x0F, 0x17, 0x22, 0.38)
        } else {
            Color::from_rgba8(0xE8, 0xEF, 0xF6, 0.38)
        }
    }

    pub fn outline_variant(self) -> Color {
        if self.day {
            Color::from_rgba8(0x0F, 0x17, 0x22, 0.14)
        } else {
            Color::from_rgba8(0xE8, 0xEF, 0xF6, 0.16)
        }
    }

    pub fn divider(self) -> Color {
        self.outline_variant()
    }

    // --- Inverse (snackbars) ---
    pub fn inverse_surface(self) -> Color {
        if self.day {
            Color::from_rgb8(0x2F, 0x30, 0x33)
        } else {
            Color::from_rgb8(0xE3, 0xE2, 0xE6)
        }
    }

    pub fn inverse_on_surface(self) -> Color {
        if self.day {
            Color::from_rgb8(0xF0, 0xF0, 0xF4)
        } else {
            Color::from_rgb8(0x1B, 0x1B, 0x1F)
        }
    }

    pub fn inverse_primary(self) -> Color {
        self.primary_container()
    }

    /// Modal / sheet scrim @ 32%.
    pub fn scrim(self) -> Color {
        Color::from_rgba(0.0, 0.0, 0.0, 0.32)
    }

    pub fn on_accent(self) -> Color {
        self.on_primary()
    }
}

trait AccentColors {
    fn primary_day(self) -> Color;
    fn primary_night(self) -> Color;
    fn container_day(self) -> Color;
    fn container_night(self) -> Color;
    fn on_container_day(self) -> Color;
    fn on_container_night(self) -> Color;
    fn on_primary_night(self) -> Color;
}

impl AccentColors for AccentPreset {
    fn primary_day(self) -> Color {
        let (r, g, b) = accent_tokens(self).day;
        Color::from_rgb8(r, g, b)
    }

    fn primary_night(self) -> Color {
        let (r, g, b) = accent_tokens(self).night;
        Color::from_rgb8(r, g, b)
    }

    fn container_day(self) -> Color {
        let (r, g, b) = accent_tokens(self).container_day;
        Color::from_rgb8(r, g, b)
    }

    fn container_night(self) -> Color {
        let (r, g, b) = accent_tokens(self).container_night;
        Color::from_rgb8(r, g, b)
    }

    fn on_container_day(self) -> Color {
        let (r, g, b) = accent_tokens(self).on_day;
        Color::from_rgb8(r, g, b)
    }

    fn on_container_night(self) -> Color {
        self.primary_night()
    }

    fn on_primary_night(self) -> Color {
        let (r, g, b) = accent_tokens(self).on_primary_night;
        Color::from_rgb8(r, g, b)
    }
}

struct AccentTokens {
    day: (u8, u8, u8),
    night: (u8, u8, u8),
    container_day: (u8, u8, u8),
    container_night: (u8, u8, u8),
    on_day: (u8, u8, u8),
    on_primary_night: (u8, u8, u8),
}

fn accent_tokens(preset: AccentPreset) -> AccentTokens {
    use AccentPreset::*;
    let t = |day, night, cd, cn, od, opn| AccentTokens {
        day,
        night,
        container_day: cd,
        container_night: cn,
        on_day: od,
        on_primary_night: opn,
    };
    match preset {
        Teal => t((0x00,0x6A,0x62),(0x4F,0xDB,0xC8),(0x9A,0xF2,0xE6),(0x00,0x4F,0x49),(0x00,0x2A,0x27),(0x00,0x33,0x2F)),
        Ocean => t((0x0B,0x57,0xD0),(0x8A,0xB4,0xF8),(0xD3,0xE3,0xFD),(0x08,0x42,0xA0),(0x04,0x1E,0x49),(0x06,0x2E,0x6F)),
        Ember => t((0xC4,0x4B,0x00),(0xFF,0xB6,0x8A),(0xFF,0xDB,0xCC),(0x7A,0x2E,0x00),(0x3A,0x16,0x00),(0x4A,0x1A,0x00)),
        Violet => t((0x6B,0x4E,0xAA),(0xD0,0xBC,0xFF),(0xE9,0xDD,0xFF),(0x4A,0x37,0x7A),(0x2A,0x17,0x4F),(0x38,0x1E,0x72)),
        Forest => t((0x1B,0x7A,0x4A),(0x7C,0xE0,0xA8),(0xC8,0xF0,0xD8),(0x0F,0x4A,0x2E),(0x05,0x28,0x16),(0x0A,0x3A,0x22)),
        Rose => t((0xB3,0x26,0x5A),(0xFF,0xB0,0xC8),(0xFF,0xD9,0xE2),(0x6B,0x1A,0x3A),(0x3F,0x00,0x1F),(0x4A,0x00,0x28)),
        Slate => t((0x45,0x5A,0x64),(0xB0,0xBE,0xC5),(0xDC,0xE4,0xE8),(0x37,0x47,0x4F),(0x1C,0x25,0x29),(0x1A,0x24,0x28)),
        Cyan => t((0x00,0x7A,0x8A),(0x4D,0xE8,0xF5),(0xB2,0xF0,0xF8),(0x00,0x4A,0x55),(0x00,0x2A,0x30),(0x00,0x35,0x3C)),
        Sky => t((0x02,0x77,0xBD),(0x7D,0xD3,0xFC),(0xC8,0xEA,0xFF),(0x03,0x4E,0x7A),(0x01,0x2A,0x45),(0x02,0x38,0x58)),
        Azure => t((0x15,0x65,0xC0),(0x64,0xB5,0xF6),(0xBB,0xDE,0xFB),(0x0D,0x47,0xA1),(0x05,0x22,0x4F),(0x0A,0x30,0x6A)),
        Indigo => t((0x39,0x49,0xAB),(0x9F,0xA8,0xDA),(0xC5,0xCA,0xE9),(0x28,0x35,0x93),(0x12,0x18,0x4A),(0x1A,0x23,0x6B)),
        Grape => t((0x7B,0x1F,0xA2),(0xCE,0x93,0xD8),(0xE1,0xBE,0xE7),(0x4A,0x14,0x6C),(0x2A,0x08,0x40),(0x38,0x0E,0x55)),
        Magenta => t((0xC2,0x18,0x5B),(0xF4,0x8F,0xB1),(0xF8,0xBB,0xD0),(0x88,0x0E,0x4F),(0x4A,0x00,0x28),(0x5C,0x00,0x32)),
        HotPink => t((0xD8,0x1B,0x60),(0xFF,0x80,0xAB),(0xFF,0xCD,0xD2),(0xAD,0x14,0x57),(0x5C,0x00,0x2A),(0x6E,0x00,0x34)),
        Coral => t((0xE6,0x4A,0x19),(0xFF,0xAB,0x91),(0xFF,0xCC,0xBC),(0xBF,0x36,0x0C),(0x4E,0x14,0x04),(0x5F,0x1A,0x06)),
        Scarlet => t((0xD3,0x2F,0x2F),(0xEF,0x9A,0x9A),(0xFF,0xCD,0xD2),(0xB7,0x1C,0x1C),(0x4A,0x00,0x00),(0x5C,0x0A,0x0A)),
        Crimson => t((0xC6,0x28,0x28),(0xE5,0x73,0x73),(0xFF,0xCD,0xD2),(0x8E,0x00,0x00),(0x3E,0x00,0x00),(0x4E,0x08,0x08)),
        Wine => t((0x88,0x0E,0x4F),(0xF0,0x62,0x92),(0xF8,0xBB,0xD0),(0x56,0x00,0x2F),(0x2A,0x00,0x16),(0x3A,0x00,0x1E)),
        Peach => t((0xEF,0x6C,0x00),(0xFF,0xCC,0x80),(0xFF,0xE0,0xB2),(0xE6,0x51,0x00),(0x4A,0x22,0x00),(0x5C,0x2C,0x00)),
        Amber => t((0xFF,0x8F,0x00),(0xFF,0xD5,0x4F),(0xFF,0xEC,0xB3),(0xFF,0x6F,0x00),(0x4A,0x2E,0x00),(0x5C,0x3A,0x00)),
        Gold => t((0xF9,0xA8,0x25),(0xFF,0xE0,0x82),(0xFF,0xF8,0xE1),(0xF5,0x7F,0x17),(0x3E,0x2A,0x00),(0x4E,0x36,0x00)),
        Lime => t((0x9E,0x9D,0x24),(0xD4,0xE1,0x57),(0xF0,0xF4,0xC3),(0x82,0x7E,0x17),(0x2A,0x2A,0x00),(0x38,0x38,0x00)),
        Mint => t((0x00,0x89,0x7B),(0x80,0xCB,0xC4),(0xB2,0xDF,0xDB),(0x00,0x69,0x5C),(0x00,0x2E,0x28),(0x00,0x3D,0x36)),
        Jade => t((0x00,0x89,0x7B),(0x4D,0xB6,0xAC),(0xB2,0xDF,0xDB),(0x00,0x4D,0x40),(0x00,0x28,0x22),(0x00,0x35,0x2E)),
        Olive => t((0x55,0x8B,0x2F),(0xAE,0xD5,0x81),(0xDC,0xED,0xC8),(0x33,0x69,0x1E),(0x14,0x28,0x08),(0x1E,0x38,0x0E)),
        Sand => t((0xA1,0x88,0x7F),(0xD7,0xCC,0xC8),(0xEF,0xEB,0xE9),(0x6D,0x4C,0x41),(0x2A,0x1A,0x14),(0x3A,0x24,0x1C)),
        Chocolate => t((0x6D,0x4C,0x41),(0xBC,0xAA,0xA4),(0xD7,0xCC,0xC8),(0x4E,0x34,0x2E),(0x22,0x12,0x0E),(0x2E,0x1A,0x14)),
        Charcoal => t((0x37,0x47,0x4F),(0x90,0xA4,0xAE),(0xCF,0xD8,0xDC),(0x26,0x32,0x38),(0x10,0x14,0x16),(0x18,0x1E,0x22)),
        Ice => t((0x54,0x6E,0x7A),(0xB0,0xBE,0xC5),(0xEC,0xEF,0xF1),(0x37,0x47,0x4F),(0x14,0x1C,0x20),(0x1C,0x26,0x2C)),
        Neon => t((0x00,0xC8,0x53),(0x69,0xF0,0xAE),(0xB9,0xF6,0xCA),(0x00,0xA0,0x3F),(0x00,0x3A,0x18),(0x00,0x4A,0x20)),
    }
}


// Prefer `UiTheme` — free helpers kept for stage black / radii / spacing / type / elev.

pub fn stage_black() -> Color {
    Color::from_rgb8(0x00, 0x00, 0x00)
}

/// M3 spacing scale (space100 = 8dp baseline).
pub const SPACE_XXS: f32 = 2.0; // space25
pub const SPACE_XS: f32 = 4.0; // space50
pub const SPACE_SM: f32 = 8.0; // space100
pub const SPACE_MD: f32 = 12.0; // space150
pub const SPACE_LG: f32 = 16.0; // space200
pub const SPACE_XL: f32 = 24.0; // space300
pub const SPACE_XXL: f32 = 32.0; // space400
pub const SPACE_XXXL: f32 = 48.0; // space600

/// M3 shape corner-radius scale (10 steps).
pub const RADIUS_NONE: f32 = 0.0;
pub const RADIUS_EXTRA_SMALL: f32 = 4.0;
pub const RADIUS_SMALL: f32 = 8.0;
pub const RADIUS_MEDIUM: f32 = 12.0;
pub const RADIUS_LARGE: f32 = 16.0;
pub const RADIUS_LARGE_INCREASED: f32 = 20.0;
pub const RADIUS_EXTRA_LARGE: f32 = 28.0;
pub const RADIUS_EXTRA_LARGE_INCREASED: f32 = 32.0;
pub const RADIUS_EXTRA_EXTRA_LARGE: f32 = 48.0;
pub const RADIUS_FULL: f32 = 999.0;

// Legacy aliases used across browser / player / app.
pub const RADIUS_XS: f32 = RADIUS_SMALL;
pub const RADIUS_SM: f32 = RADIUS_MEDIUM;
pub const RADIUS_MD: f32 = RADIUS_LARGE;
pub const RADIUS_LG: f32 = RADIUS_LARGE_INCREASED;
pub const RADIUS_XL: f32 = RADIUS_EXTRA_LARGE;
pub const RADIUS_XXL: f32 = RADIUS_EXTRA_LARGE_INCREASED;
pub const RADIUS_XXXL: f32 = RADIUS_EXTRA_EXTRA_LARGE;

/// Poster tile: softer top, tighter bottom (Expressive image crop).
pub fn radius_poster() -> iced::border::Radius {
    iced::border::Radius {
        top_left: RADIUS_LARGE_INCREASED,
        top_right: RADIUS_LARGE_INCREASED,
        bottom_right: RADIUS_MEDIUM,
        bottom_left: RADIUS_MEDIUM,
    }
}

/// Floating toolbar — all corners XXL (Expressive floating).
pub fn radius_floating_toolbar() -> iced::border::Radius {
    iced::border::Radius {
        top_left: RADIUS_EXTRA_EXTRA_LARGE,
        top_right: RADIUS_EXTRA_EXTRA_LARGE,
        bottom_right: RADIUS_EXTRA_EXTRA_LARGE,
        bottom_left: RADIUS_EXTRA_EXTRA_LARGE,
    }
}

/// Docked / floating toolbar — only top corners soft (sheets / docked).
pub fn radius_dock() -> iced::border::Radius {
    iced::border::Radius {
        top_left: RADIUS_EXTRA_LARGE,
        top_right: RADIUS_EXTRA_LARGE,
        bottom_right: 0.0,
        bottom_left: 0.0,
    }
}

/// Medium FAB — near-circle (Expressive medium FAB ~56–80dp).
pub fn radius_fab() -> iced::border::Radius {
    iced::border::Radius {
        top_left: RADIUS_FULL,
        top_right: RADIUS_FULL,
        bottom_right: RADIUS_FULL,
        bottom_left: RADIUS_FULL,
    }
}

/// NavigationRail / filter selected indicator capsule.
pub fn radius_nav_pill() -> iced::border::Radius {
    iced::border::Radius {
        top_left: RADIUS_FULL,
        top_right: RADIUS_FULL,
        bottom_right: RADIUS_FULL,
        bottom_left: RADIUS_FULL,
    }
}

/// List selection morph: unselected = soft outer, selected = 16 all.
pub fn radius_list_item(selected: bool) -> iced::border::Radius {
    if selected {
        RADIUS_LARGE.into()
    } else {
        iced::border::Radius {
            top_left: RADIUS_EXTRA_SMALL,
            top_right: RADIUS_LARGE,
            bottom_right: RADIUS_LARGE,
            bottom_left: RADIUS_EXTRA_SMALL,
        }
    }
}

// --- Typography (baseline sizes; emphasized = weight bump in callers) ---
pub const TYPE_DISPLAY_L: f32 = 57.0;
pub const TYPE_DISPLAY_M: f32 = 45.0;
pub const TYPE_DISPLAY_S: f32 = 36.0;
pub const TYPE_HEADLINE_L: f32 = 32.0;
pub const TYPE_HEADLINE_M: f32 = 28.0;
pub const TYPE_HEADLINE_S: f32 = 24.0;
pub const TYPE_TITLE_L: f32 = 22.0;
pub const TYPE_TITLE_M: f32 = 16.0;
pub const TYPE_TITLE_S: f32 = 14.0;
pub const TYPE_BODY_L: f32 = 16.0;
pub const TYPE_BODY_M: f32 = 14.0;
pub const TYPE_BODY_S: f32 = 12.0;
pub const TYPE_LABEL_L: f32 = 14.0;
pub const TYPE_LABEL_M: f32 = 12.0;
pub const TYPE_LABEL_S: f32 = 11.0;

/// Component measurement tokens (M3 Expressive).
pub const TOOLBAR_H: f32 = 48.0;
pub const TOOLBAR_OUTER_PAD: f32 = 12.0;
pub const TOOLBAR_ITEM_GAP: f32 = 8.0;
/// Seek track thickness (not hit target).
pub const SLIDER_S_TRACK: f32 = 6.0;
pub const SLIDER_S_HEIGHT: f32 = 28.0;
pub const SLIDER_HANDLE_W: u16 = 4;
pub const FAB_MEDIUM: f32 = 48.0;
pub const SEARCH_BAR_H: f32 = 56.0;
pub const LOADING_SIZE: f32 = 48.0;
pub const TOUCH_TARGET: f32 = 48.0;
pub const CARD_RADIUS: f32 = RADIUS_MEDIUM;
pub const CARD_PAD: f32 = SPACE_LG;
pub const CARD_GAP: f32 = SPACE_SM;

/// Elevation levels 0–5 → soft desktop shadow (Android: use `Shadow::default()`).
pub fn elevation_shadow(level: u8, day: bool) -> iced::Shadow {
    #[cfg(target_os = "android")]
    {
        let _ = (level, day);
        return iced::Shadow::default();
    }
    #[cfg(not(target_os = "android"))]
    {
        let (blur, y, a) = match level {
            0 => return iced::Shadow::default(),
            1 => (4.0, 1.0, if day { 0.08 } else { 0.35 }),
            2 => (8.0, 2.0, if day { 0.12 } else { 0.40 }),
            3 => (12.0, 4.0, if day { 0.16 } else { 0.45 }),
            4 => (16.0, 6.0, if day { 0.18 } else { 0.50 }),
            _ => (20.0, 8.0, if day { 0.20 } else { 0.55 }),
        };
        iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, a),
            offset: iced::Vector::new(0.0, y),
            blur_radius: blur,
        }
    }
}

pub const PLAYER_PAD: f32 = SPACE_LG;

/// Minimum mosaic tile width (logical px) before dropping a column.
pub const MOSAIC_TILE_MIN: f32 = 120.0;
pub const MOSAIC_GAP: f32 = 14.0;

/// Screen class from window width — M3 window size classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    Phone,   // compact < 600
    Tablet,  // medium 600–839
    Laptop,  // expanded 840–1199
    Desktop, // large 1200–1599
    Tv,      // extra-large ≥ 1600
}

impl Breakpoint {
    pub fn from_width(w: f32) -> Self {
        if w < 600.0 {
            Self::Phone
        } else if w < 840.0 {
            Self::Tablet
        } else if w < 1200.0 {
            Self::Laptop
        } else if w < 1600.0 {
            Self::Desktop
        } else {
            Self::Tv
        }
    }

    pub fn is_narrow(self) -> bool {
        matches!(self, Self::Phone | Self::Tablet)
    }
}

/// One-shot layout budget for a paint pass.
#[derive(Debug, Clone, Copy)]
pub struct LayoutMetrics {
    pub bp: Breakpoint,
    pub width: f32,
    pub height: f32,
    /// Side rail width; 0 → use horizontal top tabs.
    pub rail_w: f32,
    /// Category sidebar width; 0 → chips above content.
    pub cat_w: f32,
    pub content_w: f32,
    pub cols: usize,
    pub tile_w: f32,
    pub search_w: f32,
    pub pad: f32,
    pub gap: f32,
    pub title_size: f32,
    pub body_size: f32,
    pub rail_size: f32,
    pub thumb: f32,
    pub player_chrome_h: f32,
    pub stack_header: bool,
    pub top_nav: bool,
    pub short: bool,
    /// Phone landscape: single-row tab strip instead of 2-col grid.
    pub nav_strip: bool,
}

impl LayoutMetrics {
    pub fn compute(width: f32, height: f32) -> Self {
        let w = width.max(280.0);
        let h = height.max(200.0);
        // Phone landscape is often ≥600 wide but ~360 tall — width-only breakpoints
        // wrongly switch to rail + category sidebar and crush the UI.
        let short = h < 520.0;
        let landscape = w > h * 1.05;
        let compact = w.min(h) < 520.0 || h < 480.0 || (landscape && h < 560.0);
        let bp = if compact {
            Breakpoint::Phone
        } else {
            Breakpoint::from_width(w)
        };

        #[cfg(target_os = "android")]
        let pad = if short { 6.0_f32 } else { 8.0_f32 };
        #[cfg(not(target_os = "android"))]
        let pad = match bp {
            Breakpoint::Phone => SPACE_SM,
            Breakpoint::Tablet => SPACE_MD,
            Breakpoint::Laptop => SPACE_LG,
            Breakpoint::Desktop | Breakpoint::Tv => SPACE_XL,
        };
        let gap = match bp {
            Breakpoint::Phone => if short { SPACE_XS } else { SPACE_SM },
            Breakpoint::Tablet => SPACE_SM,
            _ => SPACE_MD,
        };

        // Compact / phone always uses top tabs + category chips (never side rail).
        let (top_nav, rail_w, cat_w) = if compact || matches!(bp, Breakpoint::Phone) {
            (true, 0.0, 0.0)
        } else {
            match bp {
                Breakpoint::Tablet => (false, 120.0, (w * 0.26).clamp(140.0, 180.0)),
                Breakpoint::Laptop => (false, 148.0, 200.0),
                Breakpoint::Desktop => (false, 168.0, 240.0),
                Breakpoint::Tv => (false, 200.0, 280.0),
                Breakpoint::Phone => (true, 0.0, 0.0),
            }
        };

        let mut chrome = pad * 2.0;
        if rail_w > 0.0 {
            chrome += gap + pad * 2.0;
        }
        if cat_w > 0.0 {
            chrome += gap + pad * 2.0;
        } else {
            chrome += pad * 2.0;
        }
        let content_w = (w - rail_w - cat_w - chrome).max(120.0);

        let max_cols = match bp {
            Breakpoint::Phone if short && landscape => 3,
            Breakpoint::Phone => 2,
            Breakpoint::Tablet => 4,
            Breakpoint::Laptop => 6,
            Breakpoint::Desktop => 8,
            Breakpoint::Tv => 8,
        };
        let cols = mosaic_cols(content_w).min(max_cols).max(1);
        let tile_w = mosaic_tile_width(content_w, cols);
        let search_w = match bp {
            Breakpoint::Phone => content_w.max(100.0),
            _ => (content_w * 0.34).clamp(100.0, 380.0),
        };

        let (title_size, body_size, rail_size, thumb, player_chrome_h) = match bp {
            Breakpoint::Phone => (
                if short { TYPE_TITLE_M } else { TYPE_TITLE_L },
                if short { TYPE_BODY_S } else { TYPE_BODY_M },
                if short { TYPE_LABEL_M } else { TYPE_LABEL_L },
                if short { 36.0 } else { 40.0 },
                // floating toolbar ~48 + seek ~28 + pads
                if short { 96.0 } else { 108.0 },
            ),
            Breakpoint::Tablet => (TYPE_TITLE_L, TYPE_BODY_M, TYPE_LABEL_L, 44.0, 112.0),
            Breakpoint::Laptop => (TYPE_HEADLINE_S, TYPE_BODY_M, TYPE_TITLE_S, 48.0, 116.0),
            Breakpoint::Desktop => (TYPE_HEADLINE_S, TYPE_BODY_M, TYPE_TITLE_S, 52.0, 116.0),
            Breakpoint::Tv => (TYPE_HEADLINE_M, TYPE_BODY_L, TYPE_TITLE_M, 64.0, 128.0),
        };

        Self {
            bp,
            width: w,
            height: h,
            rail_w,
            cat_w,
            content_w,
            cols,
            tile_w,
            search_w,
            pad,
            gap,
            title_size,
            body_size,
            rail_size,
            thumb,
            player_chrome_h,
            stack_header: matches!(bp, Breakpoint::Phone) || search_w >= content_w * 0.85,
            top_nav,
            short,
            nav_strip: top_nav && short && landscape,
        }
    }
}

/// How many mosaic columns fit in `content_width`.
pub fn mosaic_cols(content_width: f32) -> usize {
    let w = content_width.max(80.0);
    let cols = ((w + MOSAIC_GAP) / (MOSAIC_TILE_MIN + MOSAIC_GAP)).floor() as usize;
    cols.clamp(1, 14)
}

pub fn mosaic_tile_width(content_width: f32, cols: usize) -> f32 {
    let cols = cols.max(1) as f32;
    let gaps = MOSAIC_GAP * (cols - 1.0);
    // Slack so button padding cannot overflow the row.
    ((content_width - gaps) / cols - 4.0).clamp(72.0, 320.0)
}
