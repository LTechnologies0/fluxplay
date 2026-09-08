//! Dedicated video player — full-bleed stage + mpvEx-inspired overlay chrome.

use iced::widget::{
    column, container, mouse_area, row, scrollable, slider, stack, text, text_input, Row,
    Space,
};
use iced::widget::image::Handle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Theme,
};
use fluxplay_player::{BackendCaps, PlaybackState, StreamSession};

use crate::theme::{
    elevation_shadow, radius_dock, radius_fab, stage_black, UiTheme, FAB_MEDIUM, LOADING_SIZE,
    RADIUS_EXTRA_LARGE, RADIUS_FULL, RADIUS_LG, SLIDER_HANDLE_W, SLIDER_S_HEIGHT, SLIDER_S_TRACK,
    SPACE_MD, SPACE_SM, SPACE_XS, SPACE_XXS, TOOLBAR_H, TOOLBAR_OUTER_PAD, TYPE_LABEL_L,
    TYPE_LABEL_M, TYPE_LABEL_S, TYPE_TITLE_M,
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
    /// When false, chrome is fading out / hidden (see `chrome_alpha`).
    pub chrome_visible: bool,
    /// Soft fade 0..=1 for overlay backgrounds (interactive if > 0.05).
    pub chrome_alpha: f32,
    pub fullscreen: bool,
    /// libmpv / libav* software-render embeds frames in iced; CLI fallback uses an OS window.
    pub embedded_video: bool,
    pub backend_label: &'a str,
    pub caps: BackendCaps,
}

pub fn player_window(p: PlayerChrome<'_>) -> Element<'_, Message> {
    let stage = stage_panel(&p);
    let live = p.session.is_live();
    let alpha = p.chrome_alpha.clamp(0.0, 1.0);
    let interactive = alpha > 0.05 || p.panel != PlayerPanel::None;

    // Full-bleed video; ALL chrome is overlay (never reflows the image stage → no flicker).
    let overlay: Element<'_, Message> = if interactive {
        let badge = if live {
            chip_live(p.ui)
        } else {
            soft_chip(p.ui, "VOD")
        };
        let mut title_ink = p.ui.inverse_on_surface();
        title_ink.a *= alpha;
        let title = text(truncate(p.title, 64))
            .size(TYPE_TITLE_M)
            .color(title_ink);
        let fs = if p.fullscreen {
            #[cfg(target_os = "android")]
            {
                soft_chip(p.ui, "Retour pour quitter")
            }
            #[cfg(not(target_os = "android"))]
            {
                soft_chip(p.ui, "Échap pour quitter")
            }
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
            background: Some(Background::Color({
                let mut c = p.ui.inverse_surface();
                // Soft dim over video; keep readable inverse ink.
                c.a = 0.82 * alpha;
                c
            })),
            text_color: Some(p.ui.inverse_on_surface()),
            ..Default::default()
        });

        let sheet: Element<'_, Message> = match p.panel {
            PlayerPanel::None => Space::new().height(0).into(),
            PlayerPanel::More => more_sheet(&p),
            PlayerPanel::Advanced => advanced_sheet(&p),
            PlayerPanel::Goto => goto_sheet(p.ui, p.goto_draft),
        };
        let middle: Element<'_, Message> = if p.panel != PlayerPanel::None {
            // Scrim fills space above the sheet; sheet sits at the bottom of this band.
            stack![
                container(Space::new().width(Fill).height(Fill))
                    .width(Fill)
                    .height(Fill)
                    .style(move |_t: &Theme| {
                        let mut bg = p.ui.scrim();
                        bg.a *= alpha;
                        container::Style {
                            background: Some(Background::Color(bg)),
                            ..Default::default()
                        }
                    }),
                column![Space::new().height(Fill), sheet]
                    .width(Fill)
                    .height(Fill),
            ]
            .width(Fill)
            .height(Fill)
            .into()
        } else {
            Space::new().height(Fill).into()
        };
        let dock = control_dock(&p, alpha);
        container(
            column![top, middle, dock]
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
        // Loader until first GPU frame — avoid black gap when clock leads video,
        // and never flip poster↔frame (scintillation).
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
                .color(ui.inverse_on_surface()),
            text(format!("Lecture · {}", p.backend_label))
                .size(14)
                .color(ui.accent()),
            text("La vidéo s’affiche dans la fenêtre lecteur externe.")
                .size(12)
                .color(Color::from_rgba(
                    ui.inverse_on_surface().r,
                    ui.inverse_on_surface().g,
                    ui.inverse_on_surface().b,
                    0.55,
                )),
            text("Contrôles FluxPlay → xdotool / IPC quand disponible.")
                .size(11)
                .color(Color::from_rgba(
                    ui.inverse_on_surface().r,
                    ui.inverse_on_surface().g,
                    ui.inverse_on_surface().b,
                    0.35,
                )),
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
            text(p.title.to_string())
                .size(18)
                .color(ui.inverse_on_surface()),
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
fn control_dock<'a>(p: &PlayerChrome<'a>, chrome_alpha: f32) -> Element<'a, Message> {
    let ui = p.ui;
    let s = p.session;
    let live = s.is_live();
    let active = p.active;
    let paused = s.state == PlaybackState::Paused || s.state == PlaybackState::Idle;
    let play_glyph = if paused { Icon::Play } else { Icon::Pause };
    let can_seek = active && !live && p.caps.seek_abs;
    let can_seek_rel = active && !live && p.caps.seek_rel;
    let mute_glyph = if s.muted { Icon::VolumeOff } else { Icon::VolumeUp };
    let progress = s.progress_ratio();
    let time_label = s.elapsed_label();
    let vol = s.volume;
    let vol_enabled = active && p.caps.volume_live;
    let mute_enabled = active && p.caps.mute;
    let pause_enabled = active && p.caps.pause;
    let on_tb = ui.on_primary_container();
    let primary = ui.primary();
    let alpha = chrome_alpha.clamp(0.0, 1.0);

    let scrub: Element<'a, Message> = if !active {
        container(
            text("En attente")
                .size(TYPE_LABEL_M)
                .color(Color::from_rgba(
                    ui.inverse_on_surface().r,
                    ui.inverse_on_surface().g,
                    ui.inverse_on_surface().b,
                    0.55,
                )),
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
    } else if !p.caps.seek_abs {
        container(
            text(if p.caps.times {
                time_label.clone()
            } else {
                format!("Lecture · {}", p.backend_label)
            })
            .size(TYPE_LABEL_L)
            .color(Color::from_rgba(
                ui.inverse_on_surface().r,
                ui.inverse_on_surface().g,
                ui.inverse_on_surface().b,
                0.75,
            )),
        )
        .width(Fill)
        .height(Length::Fixed(SLIDER_S_HEIGHT))
        .center_y(Fill)
        .into()
    } else {
        let muted_on = Color::from_rgba(
            ui.inverse_on_surface().r,
            ui.inverse_on_surface().g,
            ui.inverse_on_surface().b,
            0.75,
        );
        let rail_rest = Color::from_rgba(
            ui.inverse_on_surface().r,
            ui.inverse_on_surface().g,
            ui.inverse_on_surface().b,
            0.38,
        );
        row![
            container(
                text(time_label.clone())
                    .size(TYPE_LABEL_M)
                    .color(muted_on),
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
                    st.rail.backgrounds.1 = Background::Color(rail_rest);
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

    // Connected transport segment (−10 | Play FAB | +10), gap 0, shared soft shell.
    let transport = container(
        row![
            toolbar_svg(ui, Icon::Replay10, Message::SeekRel(-10), can_seek_rel, false),
            play_fab(ui, play_glyph, Message::TogglePause, pause_enabled),
            toolbar_svg(ui, Icon::Forward10, Message::SeekRel(10), can_seek_rel, false),
        ]
        .spacing(0)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([2, 2]))
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(Color::from_rgba(
            on_tb.r,
            on_tb.g,
            on_tb.b,
            0.12,
        ))),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: RADIUS_EXTRA_LARGE.into(),
        },
        ..Default::default()
    });

    let left = if vol_enabled {
        row![
            toolbar_svg(ui, mute_glyph, Message::ToggleMute, mute_enabled, s.muted),
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
        .align_y(Alignment::Center)
    } else {
        row![toolbar_svg(
            ui,
            mute_glyph,
            Message::ToggleMute,
            mute_enabled,
            s.muted
        )]
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
    };

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
    .style(move |_t: &Theme| {
        let mut fill = ui.primary_container();
        fill.a *= alpha;
        #[cfg(target_os = "android")]
        {
            // GLES: primary_container fill + outline for depth (no desktop elev shadows).
            container::Style {
                background: Some(Background::Color(fill)),
                border: Border {
                    color: ui.outline_variant(),
                    width: 1.0,
                    radius: radius_dock(),
                },
                shadow: Default::default(),
                ..Default::default()
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            container::Style {
                background: Some(Background::Color(fill)),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: radius_dock(),
                },
                shadow: elevation_shadow(2, ui.day),
                ..Default::default()
            }
        }
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
    #[cfg(not(target_os = "android"))]
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
        #[cfg(target_os = "android")]
        {
            "Quitter plein écran (Retour)"
        }
        #[cfg(not(target_os = "android"))]
        {
            "Quitter plein écran (Échap)"
        }
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
            chip_btn(ui, &speed, Message::CycleSpeed, active && p.caps.speed_loop),
            chip_btn(ui, loop_l, Message::ToggleLoop, active && p.caps.speed_loop),
            chip_btn(
                ui,
                "Capture d’écran",
                Message::Screenshot,
                active && p.caps.screenshot,
            ),
            chip_btn(
                ui,
                "Aller à un timecode…",
                Message::PlayerPanel(PlayerPanel::Goto),
                active && !s.is_live() && p.caps.seek_abs,
            ),
            chip_btn(
                ui,
                "Reprendre depuis le début",
                Message::RestartStream,
                active && p.caps.owned,
            ),
            chip_btn(ui, "Stop", Message::Stop, active && p.caps.owned),
        ]),
        hdr("Pistes audio / sous-titres / format"),
        chip_row(vec![
            chip_btn(
                ui,
                "Piste audio suivante",
                Message::CycleAudio,
                active && p.caps.tracks_filters,
            ),
            chip_btn(
                ui,
                "Piste sous-titres suivante",
                Message::CycleSubtitles,
                active && p.caps.tracks_filters,
            ),
            chip_btn(
                ui,
                "Afficher / masquer sous-titres",
                Message::ToggleSubVisibility,
                active && p.caps.tracks_filters,
            ),
            chip_btn(
                ui,
                s.aspect.label(),
                Message::CycleAspect,
                active && p.caps.tracks_filters,
            ),
        ]),
        hdr("Fenêtre"),
        chip_row({
            let mut chips = Vec::new();
            #[cfg(not(target_os = "android"))]
            {
                chips.push(chip_btn(
                    ui,
                    ontop_l,
                    Message::ToggleOntop,
                    active && p.caps.tracks_filters,
                ));
            }
            chips.push(chip_btn(ui, pip_l, Message::TogglePip, true));
            chips.push(chip_btn(ui, fs_l, Message::ToggleFullscreen, true));
            chips.push(chip_btn(
                ui,
                "Ouvrir dans un lecteur externe",
                Message::OpenExternal,
                active,
            ));
            chips
        }),
        hdr("Navigation & outils"),
        chip_row(vec![
            chip_btn(
                ui,
                "Chapitre précédent",
                Message::ChapterStep(-1),
                active && !s.is_live() && p.caps.tracks_filters,
            ),
            chip_btn(
                ui,
                "Chapitre suivant",
                Message::ChapterStep(1),
                active && !s.is_live() && p.caps.tracks_filters,
            ),
            chip_btn(ui, "Chaîne / piste précédente", Message::PlaylistPrev, true),
            chip_btn(ui, "Chaîne / piste suivante", Message::PlaylistNext, true),
            chip_btn(
                ui,
                "Ajouter un signet",
                Message::AddBookmark,
                active && !s.is_live() && p.caps.times,
            ),
            chip_btn(ui, &sleep_l, Message::CycleSleepTimer, true),
            chip_btn(
                ui,
                "Paramètres image & audio…",
                Message::PlayerPanel(PlayerPanel::Advanced),
                active && p.caps.tracks_filters,
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
    let active = p.active && p.caps.tracks_filters;
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
        active && !p.session.is_live() && p.caps.times,
    ));
    for (i, b) in p.session.bookmarks.iter().enumerate().take(6) {
        chips.push(chip_btn(
            ui,
            &format!("Aller à {}", b.label),
            Message::JumpBookmark(i),
            active && p.caps.seek_abs,
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

fn soft_chip(ui: UiTheme, label: &str) -> Element<'static, Message> {
    let fill = ui.surface_container_highest();
    let ink = ui.on_surface();
    container(text(label.to_string()).size(11).color(ink))
        .padding(Padding::from([6, 12]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(fill)),
            border: Border {
                color: ui.outline_variant(),
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
    let body = container(text(label).size(12).color(ui.ink()))
        .padding(Padding::from([8, 12]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.secondary_container())),
            border: Border {
                radius: RADIUS_FULL.into(),
                color: ui.outline_variant(),
                width: 1.0,
            },
            ..Default::default()
        });
    if enabled {
        mouse_area(body).on_press(msg).into()
    } else {
        body.into()
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
    let (bg, ink) = if active {
        (ui.secondary_container(), ui.on_secondary_container())
    } else {
        (ui.surface_container(), ui.ink())
    };
    let body = container(text(label).size(TYPE_LABEL_L).color(ink))
        .padding(Padding::from([12, 14]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius,
            },
            ..Default::default()
        });
    if enabled {
        mouse_area(body).on_press(msg).into()
    } else {
        body.into()
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
    let bg = if active {
        Color::from_rgba(ink.r, ink.g, ink.b, 0.22)
    } else {
        Color::TRANSPARENT
    };
    let body = container(icons::icon(kind, 22.0, ink))
        .padding(Padding::from([10, 12]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius,
            },
            ..Default::default()
        });
    if enabled {
        mouse_area(body).on_press(msg).into()
    } else {
        body.into()
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
    let bg = if active {
        Color::from_rgba(ink.r, ink.g, ink.b, 0.22)
    } else {
        Color::TRANSPARENT
    };
    let body = container(text(label).size(TYPE_LABEL_L).color(ink))
        .padding(Padding::from([10, 12]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius,
            },
            ..Default::default()
        });
    if enabled {
        mouse_area(body).on_press(msg).into()
    } else {
        body.into()
    }
}

/// Medium FAB — primary / primaryContainer (never Surface). Elev 3.
fn play_fab(ui: UiTheme, kind: Icon, msg: Message, enabled: bool) -> Element<'static, Message> {
    let fill = ui.primary();
    let ink = ui.on_primary();
    let body = container(icons::icon(kind, 28.0, ink))
        .width(Length::Fixed(FAB_MEDIUM))
        .height(Length::Fixed(FAB_MEDIUM))
        .center_x(Fill)
        .center_y(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(fill)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: radius_fab(),
            },
            shadow: elevation_shadow(3, ui.day),
            ..Default::default()
        });
    if enabled {
        mouse_area(body).on_press(msg).into()
    } else {
        body.into()
    }
}
