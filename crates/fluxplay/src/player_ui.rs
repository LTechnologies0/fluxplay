//! Dedicated video player — full-bleed stage + mpvEx-inspired overlay chrome.

use iced::widget::{
    button, column, container, mouse_area, row, scrollable, slider, stack, text, text_input, Row,
    Space,
};
use iced::widget::image::Handle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Theme,
};
use fluxplay_player::{PlaybackState, StreamSession};

use crate::theme::{
    elevation_shadow, radius_fab, radius_floating_toolbar, stage_black, UiTheme, FAB_MEDIUM,
    LOADING_SIZE, RADIUS_EXTRA_LARGE, RADIUS_FULL, RADIUS_LG, SLIDER_HANDLE_W, SLIDER_S_HEIGHT,
    SLIDER_S_TRACK, SPACE_MD, SPACE_SM, SPACE_XS, SPACE_XXS, TOOLBAR_H, TOOLBAR_OUTER_PAD,
    TYPE_LABEL_L, TYPE_LABEL_M, TYPE_LABEL_S, TYPE_TITLE_M,
};
use crate::app::Message;
use crate::icons::{self, Icon};

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
    /// libmpv / libav* software-render embeds frames in iced; CLI fallback uses an OS window.
    pub embedded_video: bool,
    pub backend_label: &'a str,
}

pub fn player_window(p: PlayerChrome<'_>) -> Element<'_, Message> {
    let stage = stage_panel(&p);
    let live = p.session.is_live();

    // Full-bleed video; ALL chrome is overlay (never reflows the image stage → no flicker).
    let overlay: Element<'_, Message> = if p.chrome_visible || p.panel != PlayerPanel::None {
        let badge = if live {
            chip_live(p.ui)
        } else {
            soft_chip(p.ui, "VOD")
        };
        let title = text(truncate(p.title, 64))
            .size(TYPE_TITLE_M)
            .color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.95));
        let fs = if p.fullscreen {
            soft_chip(p.ui, "Échap pour quitter")
        } else {
            Space::new().width(0).into()
        };
        let top = container(
            row![badge, title, Space::new().width(Fill), fs]
                .spacing(SPACE_MD)
                .padding(Padding::from([12, 16]))
                .align_y(Alignment::Center),
        )
        .width(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.45))),
            ..Default::default()
        });

        let sheet: Element<'_, Message> = match p.panel {
            PlayerPanel::None => Space::new().height(0).into(),
            PlayerPanel::More => more_sheet(&p),
            PlayerPanel::Advanced => advanced_sheet(&p),
            PlayerPanel::Goto => goto_sheet(p.ui, p.goto_draft),
        };
        let dock = control_dock(&p);
        container(
            column![top, Space::new().height(Fill), sheet, dock]
                .spacing(0)
                .width(Fill)
                .height(Fill),
        )
        .width(Fill)
        .height(Fill)
        .into()
    } else {
        // Transparent hit target to wake chrome — does not resize the stage.
        mouse_area(Space::new().width(Fill).height(Fill))
            .on_press(Message::PlayerPointerActivity)
            .into()
    };

    mouse_area(
        stack![stage, overlay]
            .width(Fill)
            .height(Fill),
    )
    // Avoid on_move (flood). Also skip on_enter spam after autohide: press/wake only
    // when chrome is already visible; when hidden, overlay Space handles press.
    .on_press(Message::PlayerPointerActivity)
    .into()
}

fn stage_panel<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let session = p.session;
    let playing = matches!(
        session.state,
        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
    );

    let center: Element<'a, Message> = if let Some(frame) = p.video {
        iced::widget::image(frame)
            .width(Fill)
            .height(Fill)
            .content_fit(iced::ContentFit::Contain)
            .into()
    } else if playing && p.embedded_video {
        // Contained loader only while buffering / waiting for first VOD frame.
        // LIVE often has progress≈0 forever — never stick the card on live Playing.
        // Once Playing with clock advance: keep black stage (no poster fallback) —
        // flipping art↔frame caused visible scintillement.
        let live = session.is_live();
        let waiting = matches!(session.state, PlaybackState::Buffering)
            || (!live && session.progress_ratio() < 0.000_5);
        if waiting {
            let indicator = container(
                column![
                    text("◌")
                        .size(LOADING_SIZE * 0.55)
                        .color(ui.on_primary_container()),
                    text("Chargement vidéo…")
                        .size(TYPE_LABEL_L)
                        .color(ui.on_primary_container()),
                    text(p.backend_label)
                        .size(TYPE_LABEL_M)
                        .color(ui.on_primary_container()),
                ]
                .spacing(SPACE_SM)
                .align_x(Alignment::Center)
                .padding(Padding::from([20, 28])),
            )
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.primary_container())),
                border: Border {
                    radius: RADIUS_EXTRA_LARGE.into(),
                    ..Default::default()
                },
                shadow: elevation_shadow(2, ui.day),
                ..Default::default()
            });
            indicator.into()
        } else {
            // Keep a black stage (not a status flash) while the first GPU allocation lands.
            Space::new().width(0).height(0).into()
        }
    } else if playing && !p.embedded_video {
        // CLI mpv/ffplay fallback: video is in an external OS window — never spin forever here.
        let art_block: Element<'a, Message> = if let Some(handle) = p.art {
            iced::widget::image(handle)
                .width(Length::Fixed(220.0))
                .height(Length::Fixed(320.0))
                .content_fit(iced::ContentFit::Cover)
                .into()
        } else {
            text("▶").size(56).color(ui.accent()).into()
        };
        column![
            art_block,
            text(p.title.to_string())
                .size(18)
                .color(Color::WHITE),
            text(format!("Lecture · {}", p.backend_label))
                .size(14)
                .color(ui.accent()),
            text("La vidéo s’affiche dans la fenêtre lecteur externe.")
                .size(12)
                .color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.55)),
            text("Contrôles FluxPlay → xdotool / IPC quand disponible.")
                .size(11)
                .color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.35)),
        ]
        .spacing(10)
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

    // Always full-bleed: never put title chrome in this column (it resized the image → flicker).
    container(center)
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill)
        .clip(true)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(stage_black())),
            ..Default::default()
        })
        .into()
}

/// Floating vibrant toolbar + seek S (M3 Expressive hero).
fn control_dock<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let s = p.session;
    let live = s.is_live();
    let active = p.active;
    let paused = s.state == PlaybackState::Paused || s.state == PlaybackState::Idle;
    let play_glyph = if paused { Icon::Play } else { Icon::Pause };
    let can_seek = active && !live;
    let mute_glyph = if s.muted { Icon::VolumeOff } else { Icon::VolumeUp };
    let progress = s.progress_ratio();
    let time_label = s.elapsed_label();
    let vol = if s.muted { 0.0 } else { s.volume };
    let on_tb = ui.on_primary_container();
    let primary = ui.primary();

    let scrub: Element<'a, Message> = if !active {
        container(
            text("En attente")
                .size(TYPE_LABEL_M)
                .color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.55)),
        )
        .width(Fill)
        .height(Length::Fixed(SLIDER_S_HEIGHT))
        .center_y(Fill)
        .into()
    } else if live {
        container(
            text(format!("●  Direct · {time_label}"))
                .size(TYPE_LABEL_L)
                .color(ui.on_error_container()),
        )
        .width(Fill)
        .height(Length::Fixed(SLIDER_S_HEIGHT))
        .center_y(Fill)
        .into()
    } else {
        row![
            container(
                text(time_label.clone())
                    .size(TYPE_LABEL_M)
                    .color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.75)),
            )
            .width(Length::Fixed(100.0))
            .height(Length::Fixed(SLIDER_S_HEIGHT))
            .center_y(Fill),
            slider(0.0..=1.0, progress as f32, |v| Message::SeekPercent(v as f64))
                .step(0.001)
                .width(Fill)
                .height(SLIDER_S_HEIGHT)
                .style(move |theme: &Theme, status| {
                    let mut st = slider::default(theme, status);
                    st.rail.backgrounds.0 = Background::Color(primary);
                    st.rail.backgrounds.1 =
                        Background::Color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.38));
                    st.rail.width = SLIDER_S_TRACK;
                    st.handle.background = Background::Color(ui.on_primary());
                    st.handle.border_color = primary;
                    st.handle.border_width = 0.0;
                    st.handle.shape = slider::HandleShape::Rectangle {
                        width: SLIDER_HANDLE_W,
                        border_radius: RADIUS_FULL.into(),
                    };
                    st
                }),
        ]
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
        .width(Fill)
        .into()
    };

    // Standard button group (−10 / Play / +10) — not connected/segmented.
    let transport = row![
        toolbar_svg(ui, Icon::Replay10, Message::SeekRel(-10), can_seek, false),
        play_fab(ui, play_glyph, Message::TogglePause, active),
        toolbar_svg(ui, Icon::Forward10, Message::SeekRel(10), can_seek, false),
    ]
    .spacing(SPACE_SM)
    .align_y(Alignment::Center);

    let left = row![
        toolbar_svg(ui, mute_glyph, Message::ToggleMute, true, s.muted),
        container(
            slider(0.0..=1.0, vol, Message::VolumeChanged)
                .step(0.01_f32)
                .height(32.0)
                .style(move |theme: &Theme, status| {
                    let mut st = slider::default(theme, status);
                    st.rail.backgrounds.0 = Background::Color(ui.primary());
                    st.rail.backgrounds.1 = Background::Color(Color::from_rgba(
                        on_tb.r,
                        on_tb.g,
                        on_tb.b,
                        0.28,
                    ));
                    st.rail.width = 8.0;
                    st.handle.background = Background::Color(on_tb);
                    st.handle.shape = slider::HandleShape::Circle { radius: 6.0 };
                    st
                }),
        )
        .width(Length::Fixed(96.0)),
    ]
    .spacing(SPACE_SM)
    .align_y(Alignment::Center);

    let right = row![
        toolbar_svg(
            ui,
            Icon::More,
            Message::PlayerPanel(PlayerPanel::More),
            true,
            p.panel == PlayerPanel::More,
        ),
        toolbar_svg(ui, Icon::Fullscreen, Message::ToggleFullscreen, true, p.fullscreen),
        toolbar_svg(ui, Icon::Close, Message::ClosePlayerWindow, true, false),
    ]
    .spacing(SPACE_XS)
    .align_y(Alignment::Center);

    let toolbar_row = row![
        left,
        Space::new().width(Fill),
        transport,
        Space::new().width(Fill),
        right,
    ]
    .align_y(Alignment::Center)
    .width(Fill)
    .height(Length::Fixed(TOOLBAR_H))
    .padding(Padding::from([0, SPACE_MD as u16]));

    // Compact floating chrome — MUST Shrink or iced stretches the container
    // to leftover overlay height (huge empty primaryContainer band).
    let floating = container(
        column![scrub, toolbar_row]
            .spacing(SPACE_XXS)
            .padding(Padding {
                top: SPACE_XS,
                right: SPACE_MD,
                bottom: SPACE_XS,
                left: SPACE_MD,
            })
            .height(Length::Shrink),
    )
    .width(Fill)
    .height(Length::Shrink)
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(ui.primary_container())),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: radius_floating_toolbar(),
        },
        shadow: elevation_shadow(2, ui.day),
        ..Default::default()
    });

    container(floating)
        .width(Fill)
        .height(Length::Shrink)
        .padding(Padding {
            top: 0.0,
            right: TOOLBAR_OUTER_PAD,
            bottom: TOOLBAR_OUTER_PAD,
            left: TOOLBAR_OUTER_PAD,
        })
        .style(move |_t: &Theme| container::Style {
            background: None,
            ..Default::default()
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
            chip_btn(ui, "Stop", Message::Stop, active),
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
    let active = p.active;
    let mut chips = Vec::new();
    chips.push(chip_btn(
        ui,
        "Ajouter ici",
        Message::AddBookmark,
        active && !p.session.is_live(),
    ));
    for (i, b) in p.session.bookmarks.iter().enumerate().take(6) {
        chips.push(chip_btn(
            ui,
            &format!("Aller à {}", b.label),
            Message::JumpBookmark(i),
            active,
        ));
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
        // Modal sheet elev 1 (tonal surface).
        background: Some(Background::Color(ui.surface_container_high())),
        border: Border {
            color: ui.outline_variant(),
            width: 0.0,
            radius: iced::border::Radius {
                top_left: RADIUS_LG,
                top_right: RADIUS_LG,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
        },
        shadow: elevation_shadow(1, ui.day),
        ..Default::default()
    })
    .into()
}

fn chip_live(ui: UiTheme) -> Element<'static, Message> {
    let fill = ui.error_container();
    let ink = ui.on_error_container();
    container(
        row![
            text("●").size(TYPE_LABEL_S).color(ink),
            text(" LIVE").size(TYPE_LABEL_M).color(ink),
        ]
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([6, 12]))
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(fill)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: RADIUS_FULL.into(),
        },
        ..Default::default()
    })
    .into()
}

fn soft_chip(_ui: UiTheme, label: &str) -> Element<'static, Message> {
    container(text(label.to_string()).size(11).color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.85)))
        .padding(Padding::from([6, 12]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.4))),
            border: Border {
                color: Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.12),
                width: 1.0,
                radius: RADIUS_FULL.into(),
            },
            ..Default::default()
        })
        .into()
}

fn chip_btn(
    ui: UiTheme,
    label: &str,
    msg: Message,
    enabled: bool,
) -> Element<'static, Message> {
    let label = label.to_string();
    let btn = button(text(label).size(12))
        .padding(Padding::from([8, 12]))
        .style(move |theme: &Theme, status| {
            let mut s = button::secondary(theme, status);
            s.border.radius = RADIUS_FULL.into();
            s.border.color = ui.outline_variant();
            s.background = Some(Background::Color(ui.secondary_container()));
            s.text_color = ui.ink();
            s
        });
    if enabled {
        btn.on_press(msg).into()
    } else {
        btn.into()
    }
}

fn dock_btn(
    ui: UiTheme,
    label: &str,
    msg: Message,
    enabled: bool,
    active: bool,
) -> Element<'static, Message> {
    // Sheet actions: tonal round icon-button (L target).
    icon_btn(ui, label, msg, enabled, active)
}

/// Round / tonal icon button on sheets (surface context).
fn icon_btn(
    ui: UiTheme,
    label: &str,
    msg: Message,
    enabled: bool,
    active: bool,
) -> Element<'static, Message> {
    let label = label.to_string();
    let radius = if active {
        RADIUS_LG.into()
    } else {
        RADIUS_FULL.into()
    };
    let btn = button(text(label).size(TYPE_LABEL_L))
        .padding(Padding::from([12, 14]))
        .style(move |_theme: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let (bg, ink) = if active {
                (ui.secondary_container(), ui.on_secondary_container())
            } else if hovered {
                (ui.surface_container_highest(), ui.ink())
            } else {
                (ui.surface_container(), ui.ink())
            };
            button::Style {
                background: Some(Background::Color(bg)),
                text_color: ink,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius,
                },
                ..Default::default()
            }
        });
    if enabled {
        btn.on_press(msg).into()
    } else {
        btn.into()
    }
}

/// Icon button on vibrant floating toolbar (`onPrimaryContainer`).
fn toolbar_svg(
    ui: UiTheme,
    kind: Icon,
    msg: Message,
    enabled: bool,
    active: bool,
) -> Element<'static, Message> {
    let ink = ui.on_primary_container();
    let radius = if active {
        RADIUS_LG.into()
    } else {
        RADIUS_FULL.into()
    };
    let btn = button(icons::icon(kind, 22.0, ink))
        .padding(Padding::from([10, 12]))
        .style(move |_theme: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let bg = if active {
                Color::from_rgba(ink.r, ink.g, ink.b, 0.22)
            } else if hovered {
                Color::from_rgba(ink.r, ink.g, ink.b, 0.12)
            } else {
                Color::TRANSPARENT
            };
            button::Style {
                background: Some(Background::Color(bg)),
                text_color: ink,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius,
                },
                ..Default::default()
            }
        });
    if enabled {
        btn.on_press(msg).into()
    } else {
        btn.into()
    }
}

/// Icon button on vibrant floating toolbar (`onPrimaryContainer`) — text fallback.
fn toolbar_icon(
    ui: UiTheme,
    label: &str,
    msg: Message,
    enabled: bool,
    active: bool,
) -> Element<'static, Message> {
    let label = label.to_string();
    let ink = ui.on_primary_container();
    // Expressive morph: unselected full pill → selected squircle.
    let radius = if active {
        RADIUS_LG.into()
    } else {
        RADIUS_FULL.into()
    };
    let btn = button(text(label).size(TYPE_LABEL_L))
        .padding(Padding::from([10, 12]))
        .style(move |_theme: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let bg = if active {
                Color::from_rgba(ink.r, ink.g, ink.b, 0.22)
            } else if hovered {
                Color::from_rgba(ink.r, ink.g, ink.b, 0.12)
            } else {
                Color::TRANSPARENT
            };
            button::Style {
                background: Some(Background::Color(bg)),
                text_color: ink,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius,
                },
                ..Default::default()
            }
        });
    if enabled {
        btn.on_press(msg).into()
    } else {
        btn.into()
    }
}

/// Medium FAB — primary / primaryContainer (never Surface). Elev 3.
fn play_fab(ui: UiTheme, kind: Icon, msg: Message, enabled: bool) -> Element<'static, Message> {
    let fill = ui.primary();
    let ink = ui.on_primary();
    let inner = container(icons::icon(kind, 28.0, ink))
        .width(Length::Fixed(FAB_MEDIUM))
        .height(Length::Fixed(FAB_MEDIUM))
        .center_x(Fill)
        .center_y(Fill);
    let btn = button(inner)
        .padding(0)
        .style(move |_theme: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let (bg, fg) = if hovered {
                (ui.primary_container(), ui.on_primary_container())
            } else {
                (fill, ink)
            };
            let _ = fg;
            button::Style {
                background: Some(Background::Color(bg)),
                text_color: ink,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: radius_fab(),
                },
                shadow: elevation_shadow(3, ui.day),
                ..Default::default()
            }
        });
    if enabled {
        btn.on_press(msg).into()
    } else {
        btn.into()
    }
}
