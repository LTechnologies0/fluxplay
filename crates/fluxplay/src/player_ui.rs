//! Dedicated video player — cinema stage + compact dock + overflow sheets.
//! Primary chrome stays minimal (Netflix / TiviMate); power features live in panels.

use iced::widget::{
    button, column, container, row, slider, text, text_input, Row, Space,
};
use iced::widget::image::Handle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Shadow, Theme,
};
use fluxplay_player::{PlaybackState, StreamSession};

use crate::theme::{
    stage_black, UiTheme, PLAYER_CHROME_H, RADIUS_FULL, RADIUS_LG, RADIUS_XL, RADIUS_XXL,
};
use crate::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayerPanel {
    #[default]
    None,
    More,
    Advanced,
    Goto,
}

pub struct PlayerChrome<'a> {
    pub ui: UiTheme,
    pub title: &'a str,
    pub meta: &'a str,
    pub status: &'a str,
    pub session: &'a StreamSession,
    pub art: Option<&'a Handle>,
    pub active: bool,
    pub panel: PlayerPanel,
    pub goto_draft: &'a str,
    pub sleep_mins: Option<u32>,
    pub pip: bool,
}

pub fn player_window(p: PlayerChrome<'_>) -> Element<'_, Message> {
    let ui = p.ui;
    let stage = stage_panel(ui, p.art, p.title, p.session);
    let dock = control_dock(&p);
    let sheet: Element<'_, Message> = match p.panel {
        PlayerPanel::None => Space::new().height(0).into(),
        PlayerPanel::More => more_sheet(&p),
        PlayerPanel::Advanced => advanced_sheet(&p),
        PlayerPanel::Goto => goto_sheet(ui, p.goto_draft),
    };

    container(
        column![stage, sheet, dock]
            .spacing(0)
            .height(Fill),
    )
    .width(Fill)
    .height(Fill)
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(stage_black())),
        ..Default::default()
    })
    .into()
}

fn stage_panel<'a>(
    ui: UiTheme,
    art: Option<&'a Handle>,
    title: &str,
    session: &StreamSession,
) -> Element<'a, Message> {
    let playing = matches!(
        session.state,
        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
    );
    let live = session.is_live();

    let center: Element<'a, Message> = if playing {
        column![
            text(if live { "DIRECT" } else { "LECTURE" })
                .size(12)
                .color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.35)),
        ]
        .align_x(Alignment::Center)
        .into()
    } else if let Some(handle) = art {
        iced::widget::image(handle)
            .width(Fill)
            .height(Fill)
            .content_fit(iced::ContentFit::Contain)
            .into()
    } else {
        column![
            text("▶").size(56).color(ui.accent()),
            text(title.to_string()).size(22).color(Color::WHITE),
        ]
        .spacing(14)
        .align_x(Alignment::Center)
        .into()
    };

    let badge = if live {
        chip_live()
    } else {
        soft_chip(ui, "VOD / SÉRIE")
    };
    let hint = match session.state {
        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering => {
            "Espace pause · ←→ seek · M mute · F plein écran · ⋯ plus"
        }
        PlaybackState::Opening => "Ouverture du flux…",
        PlaybackState::Error => "Erreur — Stop puis réessayez",
        PlaybackState::Idle => "En attente d’un média",
    };

    container(
        column![
            row![
                badge,
                Space::new().width(Fill),
                text(hint)
                    .size(11)
                    .color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.42)),
            ]
            .padding(Padding::from([10, 14])),
            container(center)
                .width(Fill)
                .height(Fill)
                .center_x(Fill)
                .center_y(Fill),
        ]
        .height(Fill),
    )
    .width(Fill)
    .height(Fill)
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(stage_black())),
        ..Default::default()
    })
    .into()
}

fn control_dock<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let s = p.session;
    let live = s.is_live();
    let active = p.active;
    let paused = s.state == PlaybackState::Paused || s.state == PlaybackState::Idle;
    let play_label = if paused { "▶" } else { "⏸" };
    let can_seek = active && !live;
    let mute_label = if s.muted { "🔇" } else { "🔊" };
    let progress = s.progress_ratio();
    let time_label = s.elapsed_label();
    let vol = if s.muted { 0.0 } else { s.volume };

    let title_row = row![
        column![
            text(p.title.to_string()).size(18).color(ui.ink()),
            text(format!("{} · {}", p.meta, p.status))
                .size(11)
                .color(ui.ink_muted()),
        ]
        .spacing(2)
        .width(Fill),
        soft_chip(ui, s.backend.map(|b| b.label()).unwrap_or("—")),
        if live { chip_live() } else { soft_chip(ui, "VOD") },
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let scrub: Element<'a, Message> = if !active {
        text("Choisissez un média").size(12).color(ui.ink_muted()).into()
    } else if live {
        text(format!("●  {time_label}"))
            .size(12)
            .color(ui.ink_muted())
            .into()
    } else {
        row![
            slider(0.0..=1.0, progress as f32, |v| Message::SeekPercent(v as f64))
                .step(0.001)
                .style(move |theme: &Theme, status| {
                    let mut st = slider::default(theme, status);
                    st.rail.backgrounds.0 = Background::Color(ui.accent());
                    st.rail.backgrounds.1 = Background::Color(ui.surface_muted());
                    st.handle.background = Background::Color(ui.accent());
                    st.handle.shape = slider::HandleShape::Circle { radius: 7.0 };
                    st
                }),
            text(time_label)
                .size(12)
                .color(ui.ink_muted())
                .width(Length::Fixed(96.0)),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
    };

    let transport = row![
        icon_btn(ui, "−10", Message::SeekRel(-10), can_seek),
        primary_round(ui, play_label, Message::TogglePause, active),
        icon_btn(ui, "+10", Message::SeekRel(10), can_seek),
        icon_btn(ui, "■", Message::Stop, active),
        Space::new().width(8),
        icon_btn(ui, mute_label, Message::ToggleMute, true),
        container(
            slider(0.0..=1.0, vol, Message::VolumeChanged)
                .step(0.01)
                .style(move |theme: &Theme, status| {
                    let mut st = slider::default(theme, status);
                    st.rail.backgrounds.0 = Background::Color(ui.accent());
                    st.rail.backgrounds.1 = Background::Color(ui.surface_muted());
                    st.handle.background = Background::Color(ui.accent());
                    st.handle.shape = slider::HandleShape::Circle { radius: 6.0 };
                    st
                }),
        )
        .width(Length::Fixed(110.0)),
        Space::new().width(Fill),
        icon_btn(ui, "⛶", Message::ToggleFullscreen, active),
        icon_btn(
            ui,
            "⋯",
            Message::PlayerPanel(if p.panel == PlayerPanel::More {
                PlayerPanel::None
            } else {
                PlayerPanel::More
            }),
            true,
        ),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    container(
        column![title_row, scrub, transport]
            .spacing(10)
            .padding(Padding::from([14, 18])),
    )
    .width(Fill)
    .height(Length::Fixed(PLAYER_CHROME_H - 40.0))
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(if ui.day {
            Color::from_rgb8(0xF2, 0xF6, 0xFA)
        } else {
            Color::from_rgb8(0x12, 0x18, 0x22)
        })),
        border: Border {
            color: ui.outline(),
            width: 0.0,
            radius: iced::border::Radius {
                top_left: RADIUS_XXL,
                top_right: RADIUS_XXL,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if ui.day { 0.10 } else { 0.45 }),
            offset: iced::Vector::new(0.0, -8.0),
            blur_radius: 28.0,
        },
        ..Default::default()
    })
    .into()
}

fn more_sheet<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let s = p.session;
    let active = p.active;
    let speed = format!("{:.2}×", s.speed);
    let loop_l = if s.loop_file { "Loop ON" } else { "Loop" };
    let ontop_l = if s.ontop { "Ontop ON" } else { "Ontop" };
    let pip_l = if p.pip { "PiP ON" } else { "PiP" };
    let sleep_l = match p.sleep_mins {
        Some(m) => format!("Veille {m}m"),
        None => "Veille".into(),
    };

    let row1 = row![
        chip_btn(ui, &format!("Vitesse {speed}"), Message::CycleSpeed, active),
        chip_btn(ui, loop_l, Message::ToggleLoop, active),
        chip_btn(ui, "Capture", Message::Screenshot, active),
        chip_btn(ui, "Aller à…", Message::PlayerPanel(PlayerPanel::Goto), active && !s.is_live()),
        chip_btn(ui, "Reprise", Message::RestartStream, active),
    ]
    .spacing(6)
    .wrap();

    let row2 = row![
        chip_btn(ui, "Audio", Message::CycleAudio, active),
        chip_btn(ui, "ST", Message::CycleSubtitles, active),
        chip_btn(ui, "ST on/off", Message::ToggleSubVisibility, active),
        chip_btn(ui, s.aspect.label(), Message::CycleAspect, active),
        chip_btn(ui, ontop_l, Message::ToggleOntop, active),
        chip_btn(ui, pip_l, Message::TogglePip, true),
    ]
    .spacing(6)
    .wrap();

    let row3 = row![
        chip_btn(ui, "◀ Chap", Message::ChapterStep(-1), active && !s.is_live()),
        chip_btn(ui, "Chap ▶", Message::ChapterStep(1), active && !s.is_live()),
        chip_btn(ui, "◀ Piste", Message::PlaylistPrev, true),
        chip_btn(ui, "Piste ▶", Message::PlaylistNext, true),
        chip_btn(ui, "★ Signet", Message::AddBookmark, active && !s.is_live()),
        chip_btn(ui, &sleep_l, Message::CycleSleepTimer, true),
        chip_btn(ui, "Avancé", Message::PlayerPanel(PlayerPanel::Advanced), true),
        chip_btn(ui, "Externe", Message::OpenExternal, active),
        chip_btn(ui, "Thème", Message::CycleTheme, true),
        chip_btn(ui, "Fermer", Message::ClosePlayerWindow, true),
    ]
    .spacing(6)
    .wrap();

    let bookmarks: Element<'a, Message> = {
        let mut chips: Vec<Element<'a, Message>> = Vec::new();
        if s.bookmarks.is_empty() {
            chips.push(text("Aucun signet").size(11).color(ui.ink_muted()).into());
        } else {
            for (i, b) in s.bookmarks.iter().enumerate() {
                chips.push(chip_btn(ui, &b.label, Message::JumpBookmark(i), active));
            }
        }
        Row::with_children(chips).spacing(6).wrap().into()
    };

    sheet_box(
        ui,
        column![
            text("Plus").size(13).color(ui.ink_muted()),
            row1,
            row2,
            row3,
            text("Signets").size(12).color(ui.ink_muted()),
            bookmarks,
        ]
        .spacing(8),
    )
}

fn advanced_sheet<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let s = p.session;
    let active = p.active;
    let ab = match (s.ab_a, s.ab_b) {
        (Some(a), Some(b)) => format!("A–B {:.0}–{:.0}s", a, b),
        (Some(a), None) => format!("A={:.0}s · set B", a),
        _ => "A–B".into(),
    };
    let night = if s.night_vf { "Nuit ON" } else { "Nuit VF" };
    let deint = if s.deinterlace { "Désentr. ON" } else { "Désentr." };
    let loud = if s.loudnorm { "Norm ON" } else { "Norm" };

    sheet_box(
        ui,
        column![
            text("Avancé").size(13).color(ui.ink_muted()),
            row![
                chip_btn(ui, "A ←", Message::MarkAbA, active && !s.is_live()),
                chip_btn(ui, "B →", Message::MarkAbB, active && !s.is_live()),
                chip_btn(ui, &ab, Message::ClearAbLoop, active),
                chip_btn(ui, &format!("ST {:+.1}s", s.sub_delay), Message::SubDelay(-0.1), active),
                chip_btn(ui, "ST+", Message::SubDelay(0.1), active),
                chip_btn(ui, &format!("A/V {:+.1}s", s.audio_delay), Message::AudioDelay(-0.1), active),
                chip_btn(ui, "A/V+", Message::AudioDelay(0.1), active),
            ]
            .spacing(6)
            .wrap(),
            row![
                chip_btn(ui, s.audio_mode.label(), Message::CycleAudioMode, active),
                chip_btn(ui, s.eq_preset.label(), Message::CycleEq, active),
                chip_btn(ui, loud, Message::ToggleLoudnorm, active),
                chip_btn(ui, deint, Message::ToggleDeinterlace, active),
                chip_btn(ui, &format!("Rot {}", s.rotate_deg), Message::CycleRotate, active),
                chip_btn(ui, "Zoom−", Message::NudgeZoom(-0.1), active),
                chip_btn(ui, "Zoom+", Message::NudgeZoom(0.1), active),
                chip_btn(ui, night, Message::ToggleNightVf, active),
            ]
            .spacing(6)
            .wrap(),
            row![
                chip_btn(ui, "Retour ⋯", Message::PlayerPanel(PlayerPanel::More), true),
                chip_btn(ui, "Fermer panneau", Message::PlayerPanel(PlayerPanel::None), true),
            ]
            .spacing(6),
        ]
        .spacing(8),
    )
}

fn goto_sheet(ui: UiTheme, draft: &str) -> Element<'_, Message> {
    sheet_box(
        ui,
        column![
            text("Aller à (mm:ss ou hh:mm:ss)").size(13).color(ui.ink_muted()),
            row![
                text_input("01:30", draft)
                    .on_input(Message::GotoDraftChanged)
                    .on_submit(Message::GotoSubmit)
                    .padding(10)
                    .width(Length::Fixed(160.0)),
                chip_btn(ui, "OK", Message::GotoSubmit, true),
                chip_btn(ui, "Annuler", Message::PlayerPanel(PlayerPanel::None), true),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8),
    )
}

fn sheet_box<'a>(ui: UiTheme, body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(body)
        .width(Fill)
        .padding(Padding::from([12, 18]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_elevated())),
            border: Border {
                color: ui.outline(),
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn chip_live() -> Element<'static, Message> {
    container(
        row![
            text("●").size(10).color(Color::from_rgb8(0xE1, 0x1D, 0x48)),
            text(" LIVE").size(11).color(Color::from_rgb8(0xE1, 0x1D, 0x48)),
        ]
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([5, 12]))
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(Color::from_rgba8(0xE1, 0x1D, 0x48, 0.14))),
        border: Border {
            radius: RADIUS_FULL.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

fn soft_chip(ui: UiTheme, label: &str) -> Element<'static, Message> {
    container(text(label.to_string()).size(11).color(ui.ink_muted()))
        .padding(Padding::from([5, 12]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_elevated())),
            border: Border {
                color: ui.outline(),
                width: 1.0,
                radius: RADIUS_FULL.into(),
            },
            ..Default::default()
        })
        .into()
}

fn primary_round(
    ui: UiTheme,
    label: impl Into<String>,
    msg: Message,
    enabled: bool,
) -> Element<'static, Message> {
    let mut b = button(text(label.into()).size(16))
        .padding(Padding::from([10, 18]))
        .style(move |_theme: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(Background::Color(if !enabled {
                    ui.surface_muted()
                } else if hovered {
                    ui.primary_container()
                } else {
                    ui.accent()
                })),
                text_color: if enabled {
                    ui.on_primary()
                } else {
                    ui.ink_muted()
                },
                border: Border {
                    radius: RADIUS_XL.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        });
    if enabled {
        b = b.on_press(msg);
    }
    b.into()
}

fn icon_btn(
    ui: UiTheme,
    label: impl Into<String>,
    msg: Message,
    enabled: bool,
) -> Element<'static, Message> {
    let mut b = button(text(label.into()).size(13))
        .padding(Padding::from([9, 12]))
        .style(move |_theme: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(Background::Color(if !enabled {
                    ui.surface_elevated()
                } else if hovered {
                    ui.primary_container()
                } else {
                    ui.surface_muted()
                })),
                text_color: if enabled {
                    if hovered {
                        ui.on_primary_container()
                    } else {
                        ui.ink()
                    }
                } else {
                    ui.ink_muted()
                },
                border: Border {
                    radius: RADIUS_LG.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        });
    if enabled {
        b = b.on_press(msg);
    }
    b.into()
}

fn chip_btn(
    ui: UiTheme,
    label: &str,
    msg: Message,
    enabled: bool,
) -> Element<'static, Message> {
    icon_btn(ui, label.to_string(), msg, enabled)
}
