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
                Color::from_rgb8(0xED, 0xF2, 0xF7),
                Color::from_rgb8(0x0F, 0x17, 0x22),
                a.primary_day(),
            ),
            (false, a) => (
                Color::from_rgb8(0x0A, 0x0E, 0x14),
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
            Color::from_rgb8(0xED, 0xF2, 0xF7)
        } else {
            Color::from_rgb8(0x0A, 0x0E, 0x14)
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
            Color::from_rgb8(0x12, 0x18, 0x22)
        }
    }

    pub fn surface_muted(self) -> Color {
        if self.day {
            Color::from_rgb8(0xDF, 0xE7, 0xEF)
        } else {
            Color::from_rgb8(0x1A, 0x22, 0x30)
        }
    }

    pub fn surface_elevated(self) -> Color {
        if self.day {
            Color::from_rgb8(0xF5, 0xF8, 0xFB)
        } else {
            Color::from_rgb8(0x1E, 0x28, 0x38)
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
        match self {
            Self::Teal => Color::from_rgb8(0x00, 0x6A, 0x62),
            Self::Ocean => Color::from_rgb8(0x0B, 0x57, 0xD0),
            Self::Ember => Color::from_rgb8(0xC4, 0x4B, 0x00),
            Self::Violet => Color::from_rgb8(0x6B, 0x4E, 0xAA),
            Self::Forest => Color::from_rgb8(0x1B, 0x7A, 0x4A),
            Self::Rose => Color::from_rgb8(0xB3, 0x26, 0x5A),
            Self::Slate => Color::from_rgb8(0x45, 0x5A, 0x64),
        }
    }

    fn primary_night(self) -> Color {
        match self {
            Self::Teal => Color::from_rgb8(0x4F, 0xDB, 0xC8),
            Self::Ocean => Color::from_rgb8(0x8A, 0xB4, 0xF8),
            Self::Ember => Color::from_rgb8(0xFF, 0xB6, 0x8A),
            Self::Violet => Color::from_rgb8(0xD0, 0xBC, 0xFF),
            Self::Forest => Color::from_rgb8(0x7C, 0xE0, 0xA8),
            Self::Rose => Color::from_rgb8(0xFF, 0xB0, 0xC8),
            Self::Slate => Color::from_rgb8(0xB0, 0xBE, 0xC5),
        }
    }

    fn container_day(self) -> Color {
        match self {
            Self::Teal => Color::from_rgb8(0x9A, 0xF2, 0xE6),
            Self::Ocean => Color::from_rgb8(0xD3, 0xE3, 0xFD),
            Self::Ember => Color::from_rgb8(0xFF, 0xDB, 0xCC),
            Self::Violet => Color::from_rgb8(0xE9, 0xDD, 0xFF),
            Self::Forest => Color::from_rgb8(0xC8, 0xF0, 0xD8),
            Self::Rose => Color::from_rgb8(0xFF, 0xD9, 0xE2),
            Self::Slate => Color::from_rgb8(0xDC, 0xE4, 0xE8),
        }
    }

    fn container_night(self) -> Color {
        match self {
            Self::Teal => Color::from_rgb8(0x00, 0x4F, 0x49),
            Self::Ocean => Color::from_rgb8(0x08, 0x42, 0xA0),
            Self::Ember => Color::from_rgb8(0x7A, 0x2E, 0x00),
            Self::Violet => Color::from_rgb8(0x4A, 0x37, 0x7A),
            Self::Forest => Color::from_rgb8(0x0F, 0x4A, 0x2E),
            Self::Rose => Color::from_rgb8(0x6B, 0x1A, 0x3A),
            Self::Slate => Color::from_rgb8(0x37, 0x47, 0x4F),
        }
    }

    fn on_container_day(self) -> Color {
        match self {
            Self::Teal => Color::from_rgb8(0x00, 0x2A, 0x27),
            Self::Ocean => Color::from_rgb8(0x04, 0x1E, 0x49),
            Self::Ember => Color::from_rgb8(0x3A, 0x16, 0x00),
            Self::Violet => Color::from_rgb8(0x2A, 0x17, 0x4F),
            Self::Forest => Color::from_rgb8(0x05, 0x28, 0x16),
            Self::Rose => Color::from_rgb8(0x3F, 0x00, 0x1F),
            Self::Slate => Color::from_rgb8(0x1C, 0x25, 0x29),
        }
    }

    fn on_container_night(self) -> Color {
        self.primary_night()
    }

    fn on_primary_night(self) -> Color {
        match self {
            Self::Teal => Color::from_rgb8(0x00, 0x33, 0x2F),
            Self::Ocean => Color::from_rgb8(0x06, 0x2E, 0x6F),
            Self::Ember => Color::from_rgb8(0x4A, 0x1A, 0x00),
            Self::Violet => Color::from_rgb8(0x38, 0x1E, 0x72),
            Self::Forest => Color::from_rgb8(0x0A, 0x3A, 0x22),
            Self::Rose => Color::from_rgb8(0x4A, 0x00, 0x28),
            Self::Slate => Color::from_rgb8(0x1A, 0x24, 0x28),
        }
    }
}

// Prefer `UiTheme` — free helpers kept only for stage black / radii.

pub fn stage_black() -> Color {
    Color::from_rgb8(0x00, 0x00, 0x00)
}

pub const RADIUS_XS: f32 = 4.0;
pub const RADIUS_SM: f32 = 8.0;
pub const RADIUS_MD: f32 = 12.0;
pub const RADIUS_LG: f32 = 16.0;
pub const RADIUS_XL: f32 = 28.0;
pub const RADIUS_XXL: f32 = 36.0;
pub const RADIUS_FULL: f32 = 999.0;

pub const PLAYER_CHROME_H: f32 = 272.0;
pub const PLAYER_PAD: f32 = 16.0;

/// Minimum mosaic tile width (logical px) before dropping a column.
pub const MOSAIC_TILE_MIN: f32 = 148.0;
pub const MOSAIC_GAP: f32 = 14.0;

/// How many mosaic columns fit in `content_width`.
pub fn mosaic_cols(content_width: f32) -> usize {
    let w = content_width.max(MOSAIC_TILE_MIN);
    let cols = ((w + MOSAIC_GAP) / (MOSAIC_TILE_MIN + MOSAIC_GAP)).floor() as usize;
    cols.clamp(2, 14)
}

pub fn mosaic_tile_width(content_width: f32, cols: usize) -> f32 {
    let cols = cols.max(1) as f32;
    let gaps = MOSAIC_GAP * (cols - 1.0);
    ((content_width - gaps) / cols).max(120.0)
}
