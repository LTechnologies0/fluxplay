//! Dedicated video player — full-bleed stage + overlay dock (auto-hide capable).

use iced::widget::{
    button, column, container, mouse_area, row, scrollable, slider, stack, text, text_input, Row,
    Space,
};
use iced::widget::image::Handle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Shadow, Theme,
};
use fluxplay_player::{PlaybackState, StreamSession};

use crate::theme::{
    radius_dock, radius_fab, stage_black, UiTheme, RADIUS_FULL, RADIUS_LG, SPACE_SM,
};
use crate::app::Message;

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
    pub video: Option<&'a Handle>,
    pub active: bool,
    pub panel: PlayerPanel,
    pub goto_draft: &'a str,
    pub sleep_mins: Option<u32>,
    pub pip: bool,
    pub chrome_h: f32,
    /// When false, only the video stage is shown (pointer idle).
    pub chrome_visible: bool,
    pub fullscreen: bool,
}

pub fn player_window(p: PlayerChrome<'_>) -> Element<'_, Message> {
    let stage = stage_panel(&p);

    // Full-bleed video; chrome overlays the bottom (YouTube / VLC pattern).
    let overlay: Element<'_, Message> = if p.chrome_visible || p.panel != PlayerPanel::None {
        let sheet: Element<'_, Message> = match p.panel {
            PlayerPanel::None => Space::new().height(0).into(),
            PlayerPanel::More => more_sheet(&p),
            PlayerPanel::Advanced => advanced_sheet(&p),
            PlayerPanel::Goto => goto_sheet(p.ui, p.goto_draft),
        };
        let dock = control_dock(&p);
        container(
            column![sheet, dock]
                .spacing(0)
                .width(Fill),
        )
        .width(Fill)
        .height(Fill)
        .align_y(Alignment::End)
        .into()
    } else {
        // Full-stage tap / click wakes chrome (move alone also works via outer mouse_area).
        mouse_area(Space::new().width(Fill).height(Fill))
            .on_press(Message::PlayerPointerActivity)
            .into()
    };

    mouse_area(
        stack![stage, overlay]
            .width(Fill)
            .height(Fill),
    )
    .on_move(|_| Message::PlayerPointerActivity)
    .on_enter(Message::PlayerPointerActivity)
    .into()
}

fn stage_panel<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let session = p.session;
    let playing = matches!(
        session.state,
        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
    );
    let live = session.is_live();

    let center: Element<'a, Message> = if let Some(frame) = p.video {
        iced::widget::image(frame)
            .width(Fill)
            .height(Fill)
            .content_fit(iced::ContentFit::Contain)
            .into()
    } else if playing {
        column![
            text("Chargement vidéo…")
                .size(14)
                .color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.45)),
        ]
        .align_x(Alignment::Center)
        .into()
    } else if let Some(handle) = p.art {
        iced::widget::image(handle)
            .width(Fill)
            .height(Fill)
            .content_fit(iced::ContentFit::Contain)
            .into()
    } else {
        column![
            text("▶").size(48).color(ui.accent()),
            text(p.title.to_string()).size(18).color(Color::WHITE),
        ]
        .spacing(10)
        .align_x(Alignment::Center)
        .into()
    };

    let top: Element<'a, Message> = if p.chrome_visible || p.panel != PlayerPanel::None {
        let badge = if live {
            chip_live(ui)
        } else {
            soft_chip(ui, "VOD / SÉRIE")
        };
        let fs = if p.fullscreen {
            soft_chip(ui, "Plein écran · Échap pour quitter")
        } else {
            Space::new().width(0).into()
        };
        row![badge, Space::new().width(Fill), fs]
            .padding(Padding::from([10, 14]))
            .align_y(Alignment::Center)
            .into()
    } else {
        Space::new().height(0).into()
    };

    container(
        column![
            top,
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
    .clip(true)
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
    let mute_label = if s.muted { "Muet" } else { "Son" };
    let progress = s.progress_ratio();
    let time_label = s.elapsed_label();
    let vol = if s.muted { 0.0 } else { s.volume };
    let day = ui.day;
    let fs_label = if p.fullscreen {
        "Quitter plein écran"
    } else {
        "Plein écran"
    };

    let title_line = row![
        column![
            text(truncate(p.title, 56)).size(15).color(ui.ink()),
            text(format!("{} · {}", p.meta, truncate(p.status, 40)))
                .size(11)
                .color(ui.ink_muted()),
        ]
        .spacing(2)
        .width(Fill),
        soft_chip(ui, s.backend.map(|b| b.label()).unwrap_or("—")),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .width(Fill);

    let scrub: Element<'a, Message> = if !active {
        text("En attente").size(11).color(ui.ink_muted()).into()
    } else if live {
        text(format!("●  Direct · {time_label}"))
            .size(12)
            .color(ui.live())
            .into()
    } else {
        row![
            text(time_label.clone())
                .size(11)
                .color(ui.ink_muted())
                .width(Length::Fixed(64.0)),
            slider(0.0..=1.0, progress as f32, |v| Message::SeekPercent(v as f64))
                .step(0.001)
                .width(Fill)
                .style(move |theme: &Theme, status| {
                    let mut st = slider::default(theme, status);
                    st.rail.backgrounds.0 = Background::Color(ui.accent());
                    st.rail.backgrounds.1 = Background::Color(ui.surface_muted());
                    st.handle.background = Background::Color(ui.accent());
                    st.handle.shape = slider::HandleShape::Circle { radius: 7.0 };
                    st
                }),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .width(Fill)
        .into()
    };

    // Transport: primary cluster left, utilities right — no wrap overflow.
    let transport = row![
        dock_btn(ui, "−10", Message::SeekRel(-10), can_seek, false),
        primary_btn(ui, play_label, Message::TogglePause, active),
        dock_btn(ui, "+10", Message::SeekRel(10), can_seek, false),
        dock_btn(ui, "Stop", Message::Stop, active, false),
        Space::new().width(SPACE_SM),
        dock_btn(ui, mute_label, Message::ToggleMute, true, false),
        container(
            slider(0.0..=1.0, vol, Message::VolumeChanged)
                .step(0.01_f32)
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
        dock_btn(ui, "Options", Message::PlayerPanel(PlayerPanel::More), true, false),
        dock_btn(ui, fs_label, Message::ToggleFullscreen, true, p.fullscreen),
        dock_btn(ui, "Fermer", Message::ClosePlayerWindow, true, false),
    ]
    .spacing(SPACE_SM)
    .align_y(Alignment::Center)
    .width(Fill);

    let chrome_h = p.chrome_h.max(148.0);
    container(
        column![title_line, scrub, transport]
            .spacing(8)
            .padding(Padding::from([12, 16])),
    )
    .width(Fill)
    .height(Length::Fixed(chrome_h))
    .clip(true)
    .style(move |_t: &Theme| {
        #[cfg(target_os = "android")]
        let shadow = Shadow::default();
        #[cfg(not(target_os = "android"))]
        let shadow = Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if day { 0.12 } else { 0.5 }),
            offset: iced::Vector::new(0.0, -8.0),
            blur_radius: 28.0,
        };
        container::Style {
            background: Some(Background::Color(ui.chrome_surface())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: radius_dock(),
            },
            shadow,
            ..Default::default()
        }
    })
    .into()
}

fn truncate(s: &str, max: usize) -> String {
    let mut t = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            t.push('…');
            break;
        }
        t.push(ch);
    }
    t
}

fn more_sheet<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let s = p.session;
    let active = p.active;
    let speed = format!("Vitesse {:.2}×", s.speed);
    let loop_l = if s.loop_file {
        "Boucle fichier : ON"
    } else {
        "Boucle fichier : off"
    };
    let ontop_l = if s.ontop {
        "Toujours au-dessus : ON"
    } else {
        "Toujours au-dessus : off"
    };
    let pip_l = if p.pip {
        "Mini-lecteur (PiP) : ON"
    } else {
        "Mini-lecteur (PiP) : off"
    };
    let sleep_l = match p.sleep_mins {
        Some(m) => format!("Minuterie veille : {m} min"),
        None => "Minuterie veille : off".into(),
    };
    let fs_l = if p.fullscreen {
        "Quitter plein écran (Échap)"
    } else {
        "Plein écran"
    };

    let hdr = |t: &'static str| text(t).size(12).color(ui.ink_muted());

    let body = column![
        row![
            text("Plus d’options").size(16).color(ui.ink()),
            Space::new().width(Fill),
            dock_btn(ui, "Fermer", Message::PlayerPanel(PlayerPanel::None), true, false),
        ]
        .align_y(Alignment::Center),
        hdr("Lecture"),
        chip_row(vec![
            chip_btn(ui, &speed, Message::CycleSpeed, active),
            chip_btn(ui, loop_l, Message::ToggleLoop, active),
            chip_btn(ui, "Capture d’écran", Message::Screenshot, active),
            chip_btn(
                ui,
                "Aller à un timecode…",
                Message::PlayerPanel(PlayerPanel::Goto),
                active && !s.is_live(),
            ),
            chip_btn(ui, "Reprendre depuis le début", Message::RestartStream, active),
        ]),
        hdr("Pistes audio / sous-titres / format"),
        chip_row(vec![
            chip_btn(ui, "Piste audio suivante", Message::CycleAudio, active),
            chip_btn(ui, "Piste sous-titres suivante", Message::CycleSubtitles, active),
            chip_btn(ui, "Afficher / masquer sous-titres", Message::ToggleSubVisibility, active),
            chip_btn(ui, s.aspect.label(), Message::CycleAspect, active),
        ]),
        hdr("Fenêtre"),
        chip_row(vec![
            chip_btn(ui, ontop_l, Message::ToggleOntop, true),
            chip_btn(ui, pip_l, Message::TogglePip, true),
            chip_btn(ui, fs_l, Message::ToggleFullscreen, true),
            chip_btn(ui, "Ouvrir dans un lecteur externe", Message::OpenExternal, active),
        ]),
        hdr("Navigation & outils"),
        chip_row(vec![
            chip_btn(ui, "Chapitre précédent", Message::ChapterStep(-1), active && !s.is_live()),
            chip_btn(ui, "Chapitre suivant", Message::ChapterStep(1), active && !s.is_live()),
            chip_btn(ui, "Chaîne / piste précédente", Message::PlaylistPrev, true),
            chip_btn(ui, "Chaîne / piste suivante", Message::PlaylistNext, true),
            chip_btn(ui, "Ajouter un signet", Message::AddBookmark, active && !s.is_live()),
            chip_btn(ui, &sleep_l, Message::CycleSleepTimer, true),
            chip_btn(
                ui,
                "Paramètres image & audio…",
                Message::PlayerPanel(PlayerPanel::Advanced),
                true,
            ),
            chip_btn(ui, "Changer le thème UI", Message::CycleTheme, true),
            chip_btn(ui, "Fermer le lecteur", Message::ClosePlayerWindow, true),
        ]),
        hdr("Signets"),
        bookmarks_row(p),
    ]
    .spacing(10)
    .width(Fill)
    .padding(Padding::from([4, 2]));

    sheet_scroll(ui, body)
}

fn advanced_sheet<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let s = p.session;
    let active = p.active;
    let ab = match (s.ab_a, s.ab_b) {
        (Some(a), Some(b)) => format!("Boucle A–B : {:.0}s → {:.0}s", a, b),
        (Some(a), None) => format!("Point A = {:.0}s — définir B", a),
        _ => "Boucle A–B : inactive".into(),
    };
    let night = if s.night_vf {
        "Mode nuit image : ON"
    } else {
        "Mode nuit image : off"
    };
    let loud = if s.loudnorm {
        "Normalisation volume : ON"
    } else {
        "Normalisation volume : off"
    };
    let hdr = |t: &'static str| text(t).size(12).color(ui.ink_muted());

    let body = column![
        row![
            text("Paramètres image & audio").size(16).color(ui.ink()),
            Space::new().width(Fill),
            dock_btn(ui, "← Options", Message::PlayerPanel(PlayerPanel::More), true, false),
            dock_btn(ui, "Fermer", Message::PlayerPanel(PlayerPanel::None), true, false),
        ]
        .spacing(SPACE_SM)
        .align_y(Alignment::Center),
        hdr("Image — format, désentrelacement, upscaling"),
        chip_row(vec![
            chip_btn(ui, s.aspect.label(), Message::CycleAspect, active),
            chip_btn(ui, s.deinterlace.label(), Message::ToggleDeinterlace, active),
            chip_btn(ui, s.upscale.label(), Message::CycleUpscale, active),
            chip_btn(
                ui,
                &format!("Rotation : {}°", s.rotate_deg),
                Message::CycleRotate,
                active,
            ),
            chip_btn(ui, "Zoom −", Message::NudgeZoom(-0.1), active),
            chip_btn(ui, &format!("Zoom {:+.2}", s.zoom), Message::NudgeZoom(0.1), active),
            chip_btn(ui, "Zoom +", Message::NudgeZoom(0.1), active),
            chip_btn(ui, night, Message::ToggleNightVf, active),
        ]),
        hdr("Synchronisation sous-titres / audio"),
        chip_row(vec![
            chip_btn(ui, "Point A (boucle)", Message::MarkAbA, active && !s.is_live()),
            chip_btn(ui, "Point B (boucle)", Message::MarkAbB, active && !s.is_live()),
            chip_btn(ui, &ab, Message::ClearAbLoop, active),
            chip_btn(
                ui,
                &format!("Retard sous-titres {:+.1}s (−)", s.sub_delay),
                Message::SubDelay(-0.1),
                active,
            ),
            chip_btn(ui, "Sous-titres +0,1 s", Message::SubDelay(0.1), active),
            chip_btn(
                ui,
                &format!("Retard audio {:+.1}s (−)", s.audio_delay),
                Message::AudioDelay(-0.1),
                active,
            ),
            chip_btn(ui, "Audio +0,1 s", Message::AudioDelay(0.1), active),
        ]),
        hdr("Audio — canaux, égaliseur, normalisation"),
        chip_row(vec![
            chip_btn(
                ui,
                &format!("Canaux : {}", s.audio_mode.label()),
                Message::CycleAudioMode,
                active,
            ),
            chip_btn(
                ui,
                &format!("Égaliseur : {}", s.eq_preset.label()),
                Message::CycleEq,
                active,
            ),
            chip_btn(ui, loud, Message::ToggleLoudnorm, active),
        ]),
    ]
    .spacing(10)
    .width(Fill)
    .padding(Padding::from([4, 2]));

    sheet_scroll(ui, body)
}

fn goto_sheet(ui: UiTheme, draft: &str) -> Element<'_, Message> {
    sheet_scroll(
        ui,
        column![
            text("Aller à (mm:ss ou hh:mm:ss)").size(14).color(ui.ink()),
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
        .spacing(10),
    )
}

fn bookmarks_row<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let s = p.session;
    let active = p.active;
    let mut chips: Vec<Element<'a, Message>> = Vec::new();
    if s.bookmarks.is_empty() {
        chips.push(
            text("Aucun signet pour ce média")
                .size(11)
                .color(ui.ink_muted())
                .into(),
        );
    } else {
        for (i, b) in s.bookmarks.iter().enumerate() {
            chips.push(chip_btn(
                ui,
                &format!("Aller à {}", b.label),
                Message::JumpBookmark(i),
                active,
            ));
        }
    }
    chip_row(chips)
}

fn chip_row(chips: Vec<Element<'_, Message>>) -> Element<'_, Message> {
    Row::with_children(chips)
        .spacing(6)
        .width(Fill)
        .wrap()
        .into()
}

fn sheet_scroll<'a>(
    ui: UiTheme,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    // Cap sheet height so every option stays reachable via scroll.
    container(
        scrollable(body)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(6)
                    .scroller_width(6)
                    .margin(2),
            ))
            .height(Length::Fixed(340.0))
            .width(Fill),
    )
    .width(Fill)
    .padding(Padding::from([12, 14]))
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(ui.surface_container_high())),
        border: Border {
            color: ui.outline_variant(),
            width: 1.0,
            radius: iced::border::Radius {
                top_left: RADIUS_LG,
                top_right: RADIUS_LG,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
        },
        ..Default::default()
    })
    .into()
}

fn chip_live(ui: UiTheme) -> Element<'static, Message> {
    let c = ui.live();
    container(
        row![
            text("●").size(10).color(c),
            text(" LIVE").size(11).color(c),
        ]
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([6, 12]))
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(Color::from_rgba(c.r, c.g, c.b, 0.18))),
        border: Border {
            color: Color::from_rgba(c.r, c.g, c.b, 0.35),
            width: 1.0,
            radius: RADIUS_FULL.into(),
        },
        ..Default::default()
    })
    .into()
}

fn soft_chip(ui: UiTheme, label: &str) -> Element<'static, Message> {
    container(text(label.to_string()).size(11).color(ui.ink_muted()))
        .padding(Padding::from([6, 12]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.secondary_container())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: RADIUS_FULL.into(),
            },
            ..Default::default()
        })
        .into()
}

fn primary_btn(
    ui: UiTheme,
    label: impl Into<String>,
    msg: Message,
    enabled: bool,
) -> Element<'static, Message> {
    let mut b = button(text(label.into()).size(16))
        .padding(Padding::from([12, 18]))
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
                    ui.on_accent()
                } else {
                    ui.ink_muted()
                },
                border: Border {
                    radius: radius_fab(),
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

fn dock_btn(
    ui: UiTheme,
    label: impl Into<String>,
    msg: Message,
    enabled: bool,
    emphasized: bool,
) -> Element<'static, Message> {
    let label = label.into();
    #[cfg(target_os = "android")]
    {
        // GLES: plain text hit targets — styled buttons drop glyphs.
        let fg = if !enabled {
            ui.ink_muted()
        } else if emphasized {
            ui.accent()
        } else {
            ui.ink()
        };
        let hit = text(label).size(12).color(fg);
        if enabled {
            mouse_area(hit).on_press(msg).into()
        } else {
            hit.into()
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let mut b = button(text(label).size(12))
            .padding(Padding::from([8, 12]))
            .style(move |_theme: &Theme, status| {
                let hovered = matches!(status, button::Status::Hovered);
                button::Style {
                    background: Some(Background::Color(if emphasized {
                        ui.secondary_container()
                    } else if hovered {
                        ui.surface_container_high()
                    } else {
                        ui.surface_muted()
                    })),
                    text_color: if enabled {
                        if emphasized {
                            ui.accent()
                        } else {
                            ui.ink()
                        }
                    } else {
                        ui.ink_muted()
                    },
                    border: Border {
                        color: ui.outline_variant(),
                        width: 1.0,
                        radius: RADIUS_FULL.into(),
                    },
                    ..Default::default()
                }
            });
        if enabled {
            b = b.on_press(msg);
        }
        b.into()
    }
}

fn chip_btn(
    ui: UiTheme,
    label: &str,
    msg: Message,
    enabled: bool,
) -> Element<'static, Message> {
    dock_btn(ui, label.to_string(), msg, enabled, false)
}
