//! Material Expressive day / night palettes for FluxPlay media center.

use iced::theme::{Palette, Theme};
use iced::Color;

/// Brand seed: deep teal — broadcast / IPTV, not purple.
pub fn flux_day() -> Theme {
    Theme::custom(
        "FluxPlay Day",
        Palette {
            background: Color::from_rgb8(0xE6, 0xEC, 0xF2),
            text: Color::from_rgb8(0x12, 0x1A, 0x24),
            primary: Color::from_rgb8(0x0D, 0x94, 0x88),
            success: Color::from_rgb8(0x2E, 0xA4, 0x3A),
            warning: Color::from_rgb8(0xD9, 0x77, 0x06),
            danger: Color::from_rgb8(0xD4, 0x3B, 0x3B),
        },
    )
}

pub fn flux_night() -> Theme {
    Theme::custom(
        "FluxPlay Night",
        Palette {
            background: Color::from_rgb8(0x08, 0x0D, 0x14),
            text: Color::from_rgb8(0xE8, 0xEE, 0xF5),
            primary: Color::from_rgb8(0x2D, 0xD4, 0xBF),
            success: Color::from_rgb8(0x4A, 0xDE, 0x80),
            warning: Color::from_rgb8(0xFB, 0xBF, 0x24),
            danger: Color::from_rgb8(0xF8, 0x71, 0x71),
        },
    )
}

pub fn accent(day: bool) -> Color {
    if day {
        Color::from_rgb8(0x0D, 0x94, 0x88)
    } else {
        Color::from_rgb8(0x2D, 0xD4, 0xBF)
    }
}

pub fn surface(day: bool) -> Color {
    if day {
        Color::from_rgb8(0xFF, 0xFF, 0xFF)
    } else {
        Color::from_rgb8(0x11, 0x18, 0x24)
    }
}

pub fn surface_muted(day: bool) -> Color {
    if day {
        Color::from_rgb8(0xDD, 0xE5, 0xED)
    } else {
        Color::from_rgb8(0x1A, 0x23, 0x33)
    }
}

pub fn surface_elevated(day: bool) -> Color {
    if day {
        Color::from_rgb8(0xF4, 0xF8, 0xFB)
    } else {
        Color::from_rgb8(0x1E, 0x2A, 0x3D)
    }
}

pub fn outline(day: bool) -> Color {
    if day {
        Color::from_rgba8(0x12, 0x1A, 0x24, 0.10)
    } else {
        Color::from_rgba8(0xE8, 0xEE, 0xF5, 0.08)
    }
}

pub fn on_primary(day: bool) -> Color {
    if day {
        Color::from_rgb8(0xFF, 0xFF, 0xFF)
    } else {
        Color::from_rgb8(0x05, 0x2E, 0x2B)
    }
}

pub fn ink_muted(day: bool) -> Color {
    if day {
        Color::from_rgba8(0x12, 0x1A, 0x24, 0.55)
    } else {
        Color::from_rgba8(0xE8, 0xEE, 0xF5, 0.50)
    }
}

pub const RADIUS_XL: f32 = 22.0;
pub const RADIUS_LG: f32 = 16.0;
pub const RADIUS_MD: f32 = 12.0;
pub const RADIUS_SM: f32 = 8.0;
