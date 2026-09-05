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

    pub fn shell(self) -> Color {
        if self.day {
            // M3 surface dim — canvas behind elevated panes
            Color::from_rgb8(0xE4, 0xEA, 0xF2)
        } else {
            Color::from_rgb8(0x05, 0x08, 0x0E)
        }
    }

    pub fn accent(self) -> Color {
        if self.day {
            self.accent.primary_day()
        } else {
            self.accent.primary_night()
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

    pub fn surface(self) -> Color {
        if self.day {
            Color::from_rgb8(0xFF, 0xFF, 0xFF)
        } else {
            Color::from_rgb8(0x11, 0x16, 0x1F)
        }
    }

    pub fn surface_muted(self) -> Color {
        if self.day {
            Color::from_rgb8(0xE2, 0xE9, 0xF1)
        } else {
            Color::from_rgb8(0x18, 0x1F, 0x2C)
        }
    }

    pub fn surface_elevated(self) -> Color {
        if self.day {
            Color::from_rgb8(0xF7, 0xF9, 0xFC)
        } else {
            Color::from_rgb8(0x1C, 0x25, 0x34)
        }
    }

    pub fn outline(self) -> Color {
        if self.day {
            Color::from_rgba8(0x0F, 0x17, 0x22, 0.10)
        } else {
            Color::from_rgba8(0xE8, 0xEF, 0xF6, 0.10)
        }
    }

    pub fn on_primary(self) -> Color {
        if self.day {
            Color::from_rgb8(0xFF, 0xFF, 0xFF)
        } else {
            self.accent.on_primary_night()
        }
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

    /// Player / bottom chrome surface (slightly lifted from shell).
    pub fn chrome_surface(self) -> Color {
        if self.day {
            Color::from_rgb8(0xF2, 0xF6, 0xFA)
        } else {
            Color::from_rgb8(0x12, 0x18, 0x22)
        }
    }

    /// Selected row / category fill (M3 primary-container feel).
    pub fn selection_fill(self) -> Color {
        if self.day {
            Color::from_rgba(
                self.primary_container().r,
                self.primary_container().g,
                self.primary_container().b,
                0.55,
            )
        } else {
            Color::from_rgba(
                self.primary_container().r,
                self.primary_container().g,
                self.primary_container().b,
                0.45,
            )
        }
    }

    /// LIVE badge / recording indicator.
    pub fn live(self) -> Color {
        if self.day {
            Color::from_rgb8(0xC4, 0x14, 0x3A)
        } else {
            Color::from_rgb8(0xFF, 0x5A, 0x7A)
        }
    }

    /// Favorite star.
    pub fn favorite(self) -> Color {
        if self.day {
            Color::from_rgb8(0xC9, 0x8A, 0x00)
        } else {
            Color::from_rgb8(0xF5, 0xC5, 0x42)
        }
    }

    /// Hairline divider between chrome regions.
    pub fn divider(self) -> Color {
        if self.day {
            Color::from_rgba8(0x0F, 0x17, 0x22, 0.08)
        } else {
            Color::from_rgba8(0xE8, 0xEF, 0xF6, 0.08)
        }
    }

    /// M3 `outlineVariant` — softer borders for chips / fields.
    pub fn outline_variant(self) -> Color {
        if self.day {
            Color::from_rgba8(0x0F, 0x17, 0x22, 0.14)
        } else {
            Color::from_rgba8(0xE8, 0xEF, 0xF6, 0.16)
        }
    }

    /// M3 `surfaceContainerLow` — nested lists / mosaic canvas.
    pub fn surface_container_low(self) -> Color {
        if self.day {
            Color::from_rgb8(0xF0, 0xF4, 0xF8)
        } else {
            Color::from_rgb8(0x0E, 0x13, 0x1B)
        }
    }

    /// M3 `surfaceContainerHigh` — selected / hovered containment.
    pub fn surface_container_high(self) -> Color {
        if self.day {
            Color::from_rgb8(0xE6, 0xEC, 0xF3)
        } else {
            Color::from_rgb8(0x1A, 0x22, 0x30)
        }
    }

    /// Secondary tonal fill for inactive chips (Expressive FilterChip).
    pub fn secondary_container(self) -> Color {
        if self.day {
            Color::from_rgba(
                self.accent().r,
                self.accent().g,
                self.accent().b,
                0.12,
            )
        } else {
            Color::from_rgba(
                self.accent().r,
                self.accent().g,
                self.accent().b,
                0.18,
            )
        }
    }

    /// Inverse / on-accent for filled chips & FAB labels.
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


// Prefer `UiTheme` — free helpers kept only for stage black / radii / spacing.

pub fn stage_black() -> Color {
    Color::from_rgb8(0x00, 0x00, 0x00)
}

/// Spacing scale (logical px) — prefer these over magic numbers.
pub const SPACE_XXS: f32 = 2.0;
pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;
pub const SPACE_XL: f32 = 24.0;
pub const SPACE_XXL: f32 = 32.0;

// M3 Expressive shape scale — mix round + sharp for tension (not uniform radii).
pub const RADIUS_XS: f32 = 8.0;
pub const RADIUS_SM: f32 = 12.0;
pub const RADIUS_MD: f32 = 16.0;
pub const RADIUS_LG: f32 = 20.0;
pub const RADIUS_XL: f32 = 28.0; // largeIncreased
pub const RADIUS_XXL: f32 = 36.0; // extraLargeIncreased
pub const RADIUS_FULL: f32 = 999.0;

/// Poster tile: softer top, tighter bottom (Expressive image crop).
pub fn radius_poster() -> iced::border::Radius {
    iced::border::Radius {
        top_left: 20.0,
        top_right: 20.0,
        bottom_right: 12.0,
        bottom_left: 12.0,
    }
}

/// Docked / floating toolbar — only top corners soft.
pub fn radius_dock() -> iced::border::Radius {
    iced::border::Radius {
        top_left: RADIUS_XL,
        top_right: RADIUS_XL,
        bottom_right: 0.0,
        bottom_left: 0.0,
    }
}

/// FAB / primary play — near-circle squircle.
pub fn radius_fab() -> iced::border::Radius {
    iced::border::Radius {
        top_left: 22.0,
        top_right: 22.0,
        bottom_right: 22.0,
        bottom_left: 22.0,
    }
}

/// NavigationRail selected indicator capsule.
pub fn radius_nav_pill() -> iced::border::Radius {
    iced::border::Radius {
        top_left: RADIUS_FULL,
        top_right: RADIUS_FULL,
        bottom_right: RADIUS_FULL,
        bottom_left: RADIUS_FULL,
    }
}

pub const PLAYER_PAD: f32 = 16.0;

/// Minimum mosaic tile width (logical px) before dropping a column.
pub const MOSAIC_TILE_MIN: f32 = 120.0;
pub const MOSAIC_GAP: f32 = 14.0;

/// Screen class from window width — drives chrome density phone → TV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    Phone,   // < 600
    Tablet,  // 600–899
    Laptop,  // 900–1199
    Desktop, // 1200–1599
    Tv,      // ≥ 1600
}

impl Breakpoint {
    pub fn from_width(w: f32) -> Self {
        if w < 600.0 {
            Self::Phone
        } else if w < 900.0 {
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
            Breakpoint::Phone => 8.0,
            Breakpoint::Tablet => 10.0,
            _ => 12.0,
        };
        let gap = match bp {
            Breakpoint::Phone => if short { 4.0 } else { 6.0 },
            Breakpoint::Tablet => 8.0,
            _ => 10.0,
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
                if short { 16.0 } else { 18.0 },
                if short { 12.0 } else { 13.0 },
                if short { 12.0 } else { 14.0 },
                if short { 36.0 } else { 40.0 },
                if short { 72.0 } else { 100.0 },
            ),
            Breakpoint::Tablet => (20.0, 13.0, 14.0, 44.0, 104.0),
            Breakpoint::Laptop => (22.0, 14.0, 15.0, 48.0, 108.0),
            Breakpoint::Desktop => (22.0, 14.0, 15.0, 52.0, 108.0),
            Breakpoint::Tv => (26.0, 16.0, 18.0, 64.0, 128.0),
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
