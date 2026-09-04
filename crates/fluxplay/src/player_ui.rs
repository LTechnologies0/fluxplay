//! Video player chrome — transport, scrubber, volume, state chips.
//! Material Expressive teal; compact dock (not a dashboard of cards).

use iced::widget::{
    button, column, container, image, row, slider, text, Space,
};
use iced::widget::image::Handle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Shadow, Theme,
};
use fluxplay_player::PlaybackState;

use crate::theme::{
    accent, ink_muted, on_primary, outline, surface, surface_elevated, surface_muted, RADIUS_LG,
    RADIUS_MD, RADIUS_SM,
};
use crate::Message;

pub struct PlayerChrome<'a> {
    pub day: bool,
    pub title: &'a str,
    pub meta: &'a str,
    pub status: &'a str,
    pub state: PlaybackState,
    pub backend: &'a str,
    pub live: bool,
    pub muted: bool,
    pub volume: f32,
    pub progress: f64,
    pub time_label: String,
    pub art: Option<&'a Handle>,
    pub active: bool,
}

pub fn player_dock(p: PlayerChrome<'_>) -> Element<'_, Message> {
    let day = p.day;
    let art = art_block(day, p.art);
    let header = meta_header(day, &p);
    let scrub = scrubber(day, p.live, p.progress, p.time_label, p.active);
    let transport = transport_row(day, p.state, p.active, p.live);
    let volume = volume_block(day, p.muted, p.volume);
    let extras = extras_row(day, p.active);

    container(
        column![
            row![art, header].spacing(12).align_y(Alignment::Center),
            scrub,
            row![transport, Space::new().width(8), volume, Space::new().width(Fill), extras]
                .spacing(8)
                .align_y(Alignment::Center),
        ]
        .spacing(8)
        .padding(Padding::from([10, 14])),
    )
    .width(Fill)
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(surface(day))),
        border: Border {
            color: outline(day),
            width: 1.0,
            radius: RADIUS_LG.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if day { 0.06 } else { 0.35 }),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 14.0,
        },
        ..Default::default()
    })
    .into()
}

fn art_block<'a>(day: bool, art: Option<&'a Handle>) -> Element<'a, Message> {
    let inner: Element<'a, Message> = if let Some(handle) = art {
        image(handle)
            .width(Length::Fixed(72.0))
            .height(Length::Fixed(42.0))
            .content_fit(iced::ContentFit::Cover)
            .into()
    } else {
        container(text("FP").size(14).color(accent(day)))
            .width(Length::Fixed(72.0))
            .height(Length::Fixed(42.0))
            .center_x(Fill)
            .center_y(Fill)
            .into()
    };
    container(inner)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(surface_muted(day))),
            border: Border {
                color: outline(day),
                width: 1.0,
                radius: RADIUS_MD.into(),
            },
            ..Default::default()
        })
        .into()
}

fn meta_header<'a>(day: bool, p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let title_color = if day {
        Color::from_rgb8(0x12, 0x1A, 0x24)
    } else {
        Color::WHITE
    };
    let chips = row![
        state_chip(day, p.state),
        if p.live {
            live_chip()
        } else if p.active {
            soft_chip(day, "VOD")
        } else {
            soft_chip(day, "Idle")
        },
        soft_chip(day, p.backend),
    ]
    .spacing(6);

    column![
        row![
            text(p.title.to_string())
                .size(15)
                .color(title_color)
                .width(Fill),
            chips,
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        text(format!("{} · {}", p.meta, p.status))
            .size(11)
            .color(ink_muted(day)),
    ]
    .spacing(2)
    .width(Fill)
    .into()
}

fn state_chip(day: bool, state: PlaybackState) -> Element<'static, Message> {
    let (label, bg, fg) = match state {
        PlaybackState::Playing => (
            "LECTURE",
            Color::from_rgba8(0x0D, 0x94, 0x88, if day { 0.18 } else { 0.28 }),
            accent(day),
        ),
        PlaybackState::Paused => (
            "PAUSE",
            Color::from_rgba8(0xD9, 0x77, 0x06, 0.20),
            Color::from_rgb8(0xD9, 0x77, 0x06),
        ),
        PlaybackState::Buffering | PlaybackState::Opening => (
            "BUFFER",
            Color::from_rgba8(0x2D, 0xD4, 0xBF, 0.18),
            accent(day),
        ),
        PlaybackState::Error => (
            "ERREUR",
            Color::from_rgba8(0xD4, 0x3B, 0x3B, 0.20),
            Color::from_rgb8(0xD4, 0x3B, 0x3B),
        ),
        PlaybackState::Idle => ("PRET", surface_muted(day), ink_muted(day)),
    };
    container(text(label).size(10).color(fg))
        .padding(Padding::from([3, 7]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: RADIUS_SM.into(),
            },
            ..Default::default()
        })
        .into()
}

fn live_chip() -> Element<'static, Message> {
    container(
        row![
            text("●").size(9).color(Color::from_rgb8(0xE1, 0x1D, 0x48)),
            text(" LIVE").size(10).color(Color::from_rgb8(0xE1, 0x1D, 0x48)),
        ]
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([3, 7]))
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(Color::from_rgba8(0xE1, 0x1D, 0x48, 0.12))),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: RADIUS_SM.into(),
        },
        ..Default::default()
    })
    .into()
}

fn soft_chip(day: bool, label: &str) -> Element<'static, Message> {
    container(text(label.to_string()).size(10).color(ink_muted(day)))
        .padding(Padding::from([3, 7]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(surface_elevated(day))),
            border: Border {
                color: outline(day),
                width: 1.0,
                radius: RADIUS_SM.into(),
            },
            ..Default::default()
        })
        .into()
}

fn scrubber(
    day: bool,
    live: bool,
    progress: f64,
    time_label: String,
    active: bool,
) -> Element<'static, Message> {
    let track: Element<'static, Message> = if !active {
        container(
            text("Sélectionnez une chaîne, un film ou un épisode pour démarrer")
                .size(11)
                .color(ink_muted(day)),
        )
        .width(Fill)
        .padding(Padding::from([4, 0]))
        .into()
    } else if live {
        container(
            row![
                text("●").size(11).color(Color::from_rgb8(0xE1, 0x1D, 0x48)),
                text(format!("  {time_label} — seek désactivé en direct"))
                    .size(11)
                    .color(ink_muted(day)),
            ]
            .align_y(Alignment::Center),
        )
        .width(Fill)
        .padding(Padding::from([4, 0]))
        .into()
    } else {
        slider(0.0..=1.0, progress as f32, |v| Message::SeekPercent(v as f64))
            .step(0.001)
            .style(move |theme: &Theme, status| {
                let mut s = slider::default(theme, status);
                s.rail.backgrounds.0 = Background::Color(accent(day));
                s.rail.backgrounds.1 = Background::Color(surface_muted(day));
                s.handle.background = Background::Color(accent(day));
                s
            })
            .into()
    };

    row![
        track,
        text(if active { time_label } else { "00:00".into() })
            .size(12)
            .color(ink_muted(day))
            .width(Length::Fixed(100.0)),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn transport_row(
    day: bool,
    state: PlaybackState,
    active: bool,
    live: bool,
) -> Element<'static, Message> {
    let paused = state == PlaybackState::Paused || state == PlaybackState::Idle;
    let play_label = if paused { "Play" } else { "Pause" };
    let can_seek = active && !live;

    row![
        ctrl_btn(day, "-30s", Message::SeekRel(-30), can_seek),
        ctrl_btn(day, "-10s", Message::SeekRel(-10), can_seek),
        primary_btn(day, play_label, Message::TogglePause, active),
        ctrl_btn(day, "Stop", Message::Stop, active),
        ctrl_btn(day, "+10s", Message::SeekRel(10), can_seek),
        ctrl_btn(day, "+30s", Message::SeekRel(30), can_seek),
    ]
    .spacing(5)
    .align_y(Alignment::Center)
    .into()
}

fn volume_block(day: bool, muted: bool, volume: f32) -> Element<'static, Message> {
    let mute_label = if muted { "Muet" } else { "Son" };
    let shown = if muted { 0.0 } else { volume };

    row![
        ctrl_btn(day, mute_label, Message::ToggleMute, true),
        container(
            slider(0.0..=1.0, shown, Message::VolumeChanged)
                .step(0.01)
                .style(move |theme: &Theme, status| {
                    let mut s = slider::default(theme, status);
                    s.rail.backgrounds.0 = Background::Color(accent(day));
                    s.rail.backgrounds.1 = Background::Color(surface_muted(day));
                    s.handle.background = Background::Color(accent(day));
                    s
                }),
        )
        .width(Length::Fixed(100.0)),
        text(format!("{:.0}%", shown * 100.0))
            .size(11)
            .color(ink_muted(day))
            .width(Length::Fixed(34.0)),
        ctrl_btn(day, "-", Message::VolumeDelta(-0.05), true),
        ctrl_btn(day, "+", Message::VolumeDelta(0.05), true),
    ]
    .spacing(5)
    .align_y(Alignment::Center)
    .into()
}

fn extras_row(day: bool, active: bool) -> Element<'static, Message> {
    row![
        ctrl_btn(day, "Reprise", Message::RestartStream, active),
        ctrl_btn(day, "Plein ecran", Message::ToggleFullscreen, active),
        ctrl_btn(day, "Audio", Message::CycleAudio, active),
        ctrl_btn(day, "ST", Message::CycleSubtitles, active),
        ctrl_btn(day, "Externe", Message::OpenExternal, active),
        ctrl_btn(day, "Theme", Message::CycleTheme, true),
    ]
    .spacing(5)
    .into()
}

fn primary_btn(
    day: bool,
    label: impl Into<String>,
    msg: Message,
    enabled: bool,
) -> Element<'static, Message> {
    let mut b = button(text(label.into()).size(13))
        .padding(Padding::from([8, 14]))
        .style(move |_theme: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered);
            let bg = if !enabled {
                surface_muted(day)
            } else if hovered {
                Color::from_rgb8(0x0A, 0x7A, 0x70)
            } else {
                accent(day)
            };
            button::Style {
                background: Some(Background::Color(bg)),
                text_color: if enabled {
                    on_primary(day)
                } else {
                    ink_muted(day)
                },
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: RADIUS_MD.into(),
                },
                ..Default::default()
            }
        });
    if enabled {
        b = b.on_press(msg);
    }
    b.into()
}

fn ctrl_btn(
    day: bool,
    label: impl Into<String>,
    msg: Message,
    enabled: bool,
) -> Element<'static, Message> {
    let mut b = button(text(label.into()).size(11))
        .padding(Padding::from([7, 10]))
        .style(move |theme: &Theme, status| {
            let mut s = button::secondary(theme, status);
            s.border.radius = RADIUS_MD.into();
            if !enabled {
                s.background = Some(Background::Color(surface_elevated(day)));
                s.text_color = ink_muted(day);
            } else if matches!(status, button::Status::Hovered) {
                s.background = Some(Background::Color(accent(day)));
                s.text_color = on_primary(day);
            } else {
                s.background = Some(Background::Color(surface_muted(day)));
            }
            s
        });
    if enabled {
        b = b.on_press(msg);
    }
    b.into()
}
