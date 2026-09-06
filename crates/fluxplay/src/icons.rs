//! Material Symbols (Rounded) — Apache-2.0, Google.
//! SVGs under `assets/icons/` (see NOTICE).

use iced::widget::svg::{self, Handle as SvgHandle, Svg};
use iced::{Color, Element, Length};

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

    pub fn handle(self) -> SvgHandle {
        SvgHandle::from_memory(self.bytes())
    }
}

/// Tinted Material Symbol at a fixed size (M3 icon button / rail).
pub fn icon<'a>(kind: Icon, size: f32, color: Color) -> Element<'a, Message> {
    Svg::new(kind.handle())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_theme, _status| svg::Style {
            color: Some(color),
        })
        .into()
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
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}
