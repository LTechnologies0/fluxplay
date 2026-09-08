//! Material Symbols (Rounded) — Apache-2.0, Google.
//! SVGs under `assets/icons/` (see NOTICE). Optional PNGs under `assets/icons/png/`.

#[cfg(not(target_os = "android"))]
use iced::widget::svg::{self, Handle as SvgHandle, Svg};
use iced::Length;
use iced::{Color, Element};

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

    /// Optional raster PNG for Android GLES (when present under `assets/icons/png/`).
    #[cfg(target_os = "android")]
    pub fn png_bytes(self) -> Option<&'static [u8]> {
        match self {
            Self::LiveTv => Some(include_bytes!("../assets/icons/png/live_tv.png")),
            Self::Movie => Some(include_bytes!("../assets/icons/png/movie.png")),
            Self::Series => Some(include_bytes!("../assets/icons/png/video_library.png")),
            Self::Favorite => Some(include_bytes!("../assets/icons/png/favorite.png")),
            Self::Epg => Some(include_bytes!("../assets/icons/png/calendar_month.png")),
            Self::Sources => Some(include_bytes!("../assets/icons/png/dns.png")),
            Self::Settings => Some(include_bytes!("../assets/icons/png/settings.png")),
            Self::Play => Some(include_bytes!("../assets/icons/png/play_arrow.png")),
            Self::Pause => Some(include_bytes!("../assets/icons/png/pause.png")),
            Self::Replay10 => Some(include_bytes!("../assets/icons/png/replay_10.png")),
            Self::Forward10 => Some(include_bytes!("../assets/icons/png/forward_10.png")),
            Self::Search => Some(include_bytes!("../assets/icons/png/search.png")),
            Self::More => Some(include_bytes!("../assets/icons/png/more_horiz.png")),
            _ => None,
        }
    }

    /// Unicode symbols for Android GLES fallback (prefer symbols over ASCII acronyms).
    pub fn text_glyph(self) -> &'static str {
        match self {
            Self::LiveTv => "▣",
            Self::Movie => "▶",
            Self::Series => "☰",
            Self::Favorite => "★",
            Self::Epg => "▦",
            Self::Sources => "◎",
            Self::Settings => "⚙",
            Self::Add => "＋",
            Self::Delete => "✕",
            Self::Refresh => "↻",
            Self::Paste => "⧉",
            Self::Play => "▶",
            Self::Pause => "⏸",
            Self::VolumeUp => "🔊",
            Self::VolumeOff => "🔇",
            Self::Fullscreen => "⛶",
            Self::Close => "✕",
            Self::More => "⋯",
            Self::Search => "🔍",
            Self::FolderOpen => "▤",
            Self::Diagnose => "⚗",
            Self::Check => "✓",
            Self::Replay10 => "↺",
            Self::Forward10 => "↻",
            Self::Stop => "⏹",
            Self::Playlist => "☰",
        }
    }
}

/// Raster icon when a PNG exists for `kind`; otherwise `None`.
#[cfg(target_os = "android")]
pub fn icon_raster<'a>(kind: Icon, size: f32) -> Option<Element<'a, Message>> {
    let bytes = kind.png_bytes()?;
    use iced::widget::image;
    use iced::widget::image::Handle;
    Some(
        image(Handle::from_bytes(bytes))
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
        if let Some(el) = icon_raster(kind, size) {
            return el;
        }
        use iced::widget::text;
        text(kind.text_glyph())
            .size((size * 0.72).clamp(10.0, 22.0))
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

    // Android GLES: text/image icon + text label (no SVG+text pairing).
    #[cfg(target_os = "android")]
    {
        row![
            icon(kind, size, color),
            text(label.into()).size(size.max(12.0)).color(color),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
    }
    #[cfg(not(target_os = "android"))]
    {
        row![
            icon(kind, size, color),
            text(label.into()).size(size.max(12.0)).color(color),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    }
}
