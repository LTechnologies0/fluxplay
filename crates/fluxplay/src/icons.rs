//! Material Symbols (Rounded) — Apache-2.0, Google.
//! SVGs under `assets/icons/` (see NOTICE). PNGs under `assets/icons/png/` (Android GLES).

#[cfg(not(target_os = "android"))]
use iced::widget::svg::{self, Handle as SvgHandle, Svg};
use iced::Length;
use iced::{Color, Element};
#[cfg(target_os = "android")]
use tracing::{trace, warn};

use crate::app::Message;

/// Named icons used across FluxPlay chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    LiveTv,
    Movie,
    Series,
    Favorite,
    Epg,
    Sources,
    Settings,
    Add,
    Delete,
    Refresh,
    Paste,
    Play,
    Pause,
    VolumeUp,
    VolumeOff,
    Fullscreen,
    Close,
    More,
    Search,
    FolderOpen,
    Diagnose,
    Check,
    Replay10,
    Forward10,
    Stop,
    Playlist,
}

impl Icon {
    #[cfg(not(target_os = "android"))]
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::LiveTv => include_bytes!("../assets/icons/live_tv.svg"),
            Self::Movie => include_bytes!("../assets/icons/movie.svg"),
            Self::Series => include_bytes!("../assets/icons/video_library.svg"),
            Self::Favorite => include_bytes!("../assets/icons/favorite.svg"),
            Self::Epg => include_bytes!("../assets/icons/calendar_month.svg"),
            Self::Sources => include_bytes!("../assets/icons/dns.svg"),
            Self::Settings => include_bytes!("../assets/icons/settings.svg"),
            Self::Add => include_bytes!("../assets/icons/add.svg"),
            Self::Delete => include_bytes!("../assets/icons/delete.svg"),
            Self::Refresh => include_bytes!("../assets/icons/refresh.svg"),
            Self::Paste => include_bytes!("../assets/icons/content_paste.svg"),
            Self::Play => include_bytes!("../assets/icons/play_arrow.svg"),
            Self::Pause => include_bytes!("../assets/icons/pause.svg"),
            Self::VolumeUp => include_bytes!("../assets/icons/volume_up.svg"),
            Self::VolumeOff => include_bytes!("../assets/icons/volume_off.svg"),
            Self::Fullscreen => include_bytes!("../assets/icons/fullscreen.svg"),
            Self::Close => include_bytes!("../assets/icons/close.svg"),
            Self::More => include_bytes!("../assets/icons/more_horiz.svg"),
            Self::Search => include_bytes!("../assets/icons/search.svg"),
            Self::FolderOpen => include_bytes!("../assets/icons/folder_open.svg"),
            Self::Diagnose => include_bytes!("../assets/icons/science.svg"),
            Self::Check => include_bytes!("../assets/icons/check.svg"),
            Self::Replay10 => include_bytes!("../assets/icons/replay_10.svg"),
            Self::Forward10 => include_bytes!("../assets/icons/forward_10.svg"),
            Self::Stop => include_bytes!("../assets/icons/stop.svg"),
            Self::Playlist => include_bytes!("../assets/icons/playlist_play.svg"),
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn handle(self) -> SvgHandle {
        SvgHandle::from_memory(self.bytes())
    }

    /// Raster PNG for Android GLES (white silhouette + alpha; tinted at draw time).
    #[cfg(target_os = "android")]
    pub fn png_bytes(self) -> Option<&'static [u8]> {
        Some(match self {
            Self::LiveTv => include_bytes!("../assets/icons/png/live_tv.png"),
            Self::Movie => include_bytes!("../assets/icons/png/movie.png"),
            Self::Series => include_bytes!("../assets/icons/png/video_library.png"),
            Self::Favorite => include_bytes!("../assets/icons/png/favorite.png"),
            Self::Epg => include_bytes!("../assets/icons/png/calendar_month.png"),
            Self::Sources => include_bytes!("../assets/icons/png/dns.png"),
            Self::Settings => include_bytes!("../assets/icons/png/settings.png"),
            Self::Add => include_bytes!("../assets/icons/png/add.png"),
            Self::Delete => include_bytes!("../assets/icons/png/delete.png"),
            Self::Refresh => include_bytes!("../assets/icons/png/refresh.png"),
            Self::Paste => include_bytes!("../assets/icons/png/content_paste.png"),
            Self::Play => include_bytes!("../assets/icons/png/play_arrow.png"),
            Self::Pause => include_bytes!("../assets/icons/png/pause.png"),
            Self::VolumeUp => include_bytes!("../assets/icons/png/volume_up.png"),
            Self::VolumeOff => include_bytes!("../assets/icons/png/volume_off.png"),
            Self::Fullscreen => include_bytes!("../assets/icons/png/fullscreen.png"),
            Self::Close => include_bytes!("../assets/icons/png/close.png"),
            Self::More => include_bytes!("../assets/icons/png/more_horiz.png"),
            Self::Search => include_bytes!("../assets/icons/png/search.png"),
            Self::FolderOpen => include_bytes!("../assets/icons/png/folder_open.png"),
            Self::Diagnose => include_bytes!("../assets/icons/png/science.png"),
            Self::Check => include_bytes!("../assets/icons/png/check.png"),
            Self::Replay10 => include_bytes!("../assets/icons/png/replay_10.png"),
            Self::Forward10 => include_bytes!("../assets/icons/png/forward_10.png"),
            Self::Stop => include_bytes!("../assets/icons/png/stop.png"),
            Self::Playlist => include_bytes!("../assets/icons/png/playlist_play.png"),
        })
    }

    /// ASCII-safe glyphs (Fira Sans / NativeActivity — no emoji).
    pub fn text_glyph(self) -> &'static str {
        match self {
            Self::LiveTv => "TV",
            Self::Movie => "VID",
            Self::Series => "SER",
            Self::Favorite => "*",
            Self::Epg => "EPG",
            Self::Sources => "SRC",
            Self::Settings => "SET",
            Self::Add => "+",
            Self::Delete => "x",
            Self::Refresh => "R",
            Self::Paste => "P",
            Self::Play => ">",
            Self::Pause => "||",
            Self::VolumeUp => "VOL",
            Self::VolumeOff => "MUT",
            Self::Fullscreen => "FS",
            Self::Close => "X",
            Self::More => "...",
            Self::Search => "?",
            Self::FolderOpen => "DIR",
            Self::Diagnose => "DIA",
            Self::Check => "OK",
            Self::Replay10 => "-10",
            Self::Forward10 => "+10",
            Self::Stop => "[]",
            Self::Playlist => "PL",
        }
    }
}

#[cfg(target_os = "android")]
fn color_key(color: Color) -> u32 {
    let r = (color.r.clamp(0.0, 1.0) * 255.0).round() as u32;
    let g = (color.g.clamp(0.0, 1.0) * 255.0).round() as u32;
    let b = (color.b.clamp(0.0, 1.0) * 255.0).round() as u32;
    let a = (color.a.clamp(0.0, 1.0) * 255.0).round() as u32;
    (r << 24) | (g << 16) | (b << 8) | a
}

/// Tint a white/black silhouette PNG with `color` (alpha preserved).
#[cfg(target_os = "android")]
fn tinted_rgba_handle(png_bytes: &[u8], color: Color) -> Option<iced::widget::image::Handle> {
    let img = image::load_from_memory(png_bytes).ok()?.into_rgba8();
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    let cr = (color.r.clamp(0.0, 1.0) * 255.0) as u8;
    let cg = (color.g.clamp(0.0, 1.0) * 255.0) as u8;
    let cb = (color.b.clamp(0.0, 1.0) * 255.0) as u8;
    let ca = color.a.clamp(0.0, 1.0);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in img.pixels() {
        let [r, g, b, a] = px.0;
        let lum = ((r as u16 + g as u16 + b as u16) / 3) as u8;
        let shape = if lum < 16 {
            a
        } else {
            ((a as f32) * (lum as f32 / 255.0)) as u8
        };
        let aa = ((shape as f32) * ca).round().clamp(0.0, 255.0) as u8;
        out.extend_from_slice(&[cr, cg, cb, aa]);
    }
    Some(iced::widget::image::Handle::from_rgba(w, h, out))
}

/// Cached tinted handles — decode/tint once per (icon, color), not every frame.
#[cfg(target_os = "android")]
fn cached_tinted_handle(kind: Icon, color: Color) -> Option<iced::widget::image::Handle> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<(u8, u32), iced::widget::image::Handle>>> =
        OnceLock::new();
    let key = (kind as u8, color_key(color));
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(h) = guard.get(&key) {
            return Some(h.clone());
        }
    }
    let bytes = kind.png_bytes()?;
    let handle = tinted_rgba_handle(bytes, color)?;
    if let Ok(mut guard) = cache.lock() {
        // Cap cache — themes don't create unbounded colors in practice.
        if guard.len() > 256 {
            guard.clear();
        }
        guard.insert(key, handle.clone());
        trace!(
            target: "fluxplay::icons",
            ?kind,
            cache = guard.len(),
            "icon tint cached"
        );
    }
    Some(handle)
}

/// Raster icon tinted for Android GLES.
#[cfg(target_os = "android")]
pub fn icon_raster<'a>(kind: Icon, size: f32, color: Color) -> Option<Element<'a, Message>> {
    let handle = match cached_tinted_handle(kind, color) {
        Some(h) => h,
        None => {
            warn!(target: "fluxplay::icons", ?kind, "png decode failed — glyph fallback");
            return None;
        }
    };
    use iced::widget::image;
    Some(
        image(handle)
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .content_fit(iced::ContentFit::Contain)
            .into(),
    )
}

/// Tinted Material Symbol at a fixed size (M3 icon button / rail).
pub fn icon<'a>(kind: Icon, size: f32, color: Color) -> Element<'a, Message> {
    #[cfg(target_os = "android")]
    {
        if let Some(el) = icon_raster(kind, size, color) {
            return el;
        }
        use iced::widget::text;
        text(kind.text_glyph())
            .size((size * 0.55).clamp(9.0, 18.0))
            .color(color)
            .into()
    }
    #[cfg(not(target_os = "android"))]
    {
        Svg::new(kind.handle())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(move |_theme, _status| svg::Style {
                color: Some(color),
            })
            .into()
    }
}

/// Icon + label row for primary/tonal pills.
pub fn icon_label<'a>(
    kind: Icon,
    label: impl Into<String>,
    size: f32,
    color: Color,
) -> Element<'a, Message> {
    use iced::widget::{row, text};
    use iced::Alignment;

    row![
        icon(kind, size, color),
        text(label.into()).size(size.max(12.0)).color(color),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}
