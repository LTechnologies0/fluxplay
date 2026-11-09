//! Dedicated video player — full-bleed stage + mpvEx-inspired overlay chrome.

use iced::widget::{
    button, column, container, mouse_area, row, scrollable, slider, stack, text, text_input,
    Row, Space,
};
use iced::widget::image::Handle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Theme,
};
use fluxplay_core::models::VideoQualityPref;
use fluxplay_player::{BackendCaps, PlaybackState, StreamSession};

use crate::theme::{
    elevation_shadow, stage_black, UiTheme, LOADING_SIZE, RADIUS_EXTRA_LARGE, RADIUS_FULL,
    RADIUS_LG, SLIDER_S_HEIGHT, SPACE_MD, SPACE_SM, SPACE_XS, TYPE_LABEL_L, TYPE_LABEL_M,
    TYPE_LABEL_S, TYPE_TITLE_M,
};
#[cfg(target_os = "android")]
use crate::theme::{SPACE_XXS, TOOLBAR_OUTER_PAD};
#[cfg(not(target_os = "android"))]
use crate::theme::radius_fab;
use crate::app::{Message, PasteTarget};
use crate::icons::{self, Icon};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayerPanel {
    #[default]
    None,
    More,
    Advanced,
    Goto,
    Remote,
}

pub struct PlayerChrome<'a> {
    pub ui: UiTheme,
    pub title: &'a str,
    pub meta: &'a str,
    /// Kept for callers; the dock no longer prints the playing title.
    #[allow(dead_code)]
    pub status: &'a str,
    pub session: &'a StreamSession,
    /// Seek bar position while dragged (the seek is sent on release).
    pub seek_drag: Option<f64>,
    pub art: Option<&'a Handle>,
    pub video: Option<&'a Handle>,
    pub active: bool,
    pub panel: PlayerPanel,
    pub goto_draft: &'a str,
    pub sleep_mins: Option<u32>,
    pub pip: bool,
    // Read only by the desktop soft-present toolbar height; Android chrome is overlay-only.
    /// Retained for layout callers. The dock sizes itself from its buttons.
    #[allow(dead_code)]
    pub chrome_h: f32,
    /// When false, chrome is fading out / hidden (see `chrome_alpha`).
    pub chrome_visible: bool,
    /// Soft fade 0..=1 for overlay backgrounds (interactive if > 0.05).
    pub chrome_alpha: f32,
    pub fullscreen: bool,
    /// Title-bar close: ask whether to leave the player or the whole app.
    pub exit_ask: bool,
    /// libmpv / libav* software-render embeds frames in iced; CLI fallback uses an OS window.
    pub embedded_video: bool,
    /// Android MediaCodec Surface under iced (transparent stage punch-through).
    pub surface_video: bool,
    /// The GPU video stage already shows a frame of this playback (desktop).
    pub stage_picture: bool,
    pub backend_label: &'a str,
    pub caps: BackendCaps,
    /// Present resolution chosen in the player menu and in settings.
    pub quality: VideoQualityPref,
    /// Persisted: jump the opening and the end credits without asking.
    pub skip_auto: bool,
    /// A phone is paired as a remote.
    pub remote_on: bool,
    pub remote_qr: Option<&'a Handle>,
    pub remote_qr_side: f32,
    pub remote_link: &'a str,
    pub remote_draft: &'a str,
    pub remote_status: &'a str,
    /// System safe-area insets (l,t,r,b) — chrome overlays only, not the stage.
    pub safe: Padding,
}

pub fn player_window(p: PlayerChrome<'_>) -> Element<'_, Message> {
    let stage = stage_panel(&p);
    let live = p.session.is_live();
    let alpha = p.chrome_alpha.clamp(0.0, 1.0);
    let interactive = (p.chrome_visible && alpha > 0.05) || p.panel != PlayerPanel::None;

    // Full-bleed video; ALL chrome is overlay (never reflows the image stage → no flicker).
    let overlay: Element<'_, Message> = if interactive {
        let badge = if live {
            chip_live(p.ui)
        } else {
            soft_chip(p.ui, "VOD")
        };
        // Over video the header is always a dark scrim with light ink, whatever the
        // app theme (the inverse surface was a light-grey slab in dark mode).
        let mut title_ink = Color::WHITE;
        title_ink.a *= alpha;
        let mut meta_ink = Color::WHITE;
        meta_ink.a *= alpha * 0.72;
        let title_block = column![
            text(truncate(p.title, 64))
                .size(TYPE_TITLE_M)
                .color(title_ink),
            text(truncate(p.meta, 72))
                .size(TYPE_LABEL_S)
                .color(meta_ink),
        ]
        .spacing(2)
        .width(Fill);
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
            row![badge, title_block, Space::new().width(Fill), fs]
                .spacing(SPACE_MD)
                .padding(Padding {
                    top: 12.0 + p.safe.top,
                    right: 16.0 + p.safe.right,
                    bottom: 12.0,
                    left: 16.0 + p.safe.left,
                })
                .align_y(Alignment::Center),
        )
        .width(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.62 * alpha))),
            text_color: Some(Color::WHITE),
            ..Default::default()
        });

        let sheet: Element<'_, Message> = match p.panel {
            PlayerPanel::None => Space::new().height(0).into(),
            PlayerPanel::More => more_sheet(&p),
            PlayerPanel::Advanced => advanced_sheet(&p),
            PlayerPanel::Goto => goto_sheet(p.ui, p.goto_draft),
            PlayerPanel::Remote => remote_sheet(&p),
        };
        let middle: Element<'_, Message> = if p.panel == PlayerPanel::More
            || p.panel == PlayerPanel::Remote
        {
            // Compact card above the dock. The rest of the picture stays visible
            // and a click there closes the menu.
            stack![
                mouse_area(
                    container(Space::new().width(Fill).height(Fill))
                        .width(Fill)
                        .height(Fill),
                )
                .on_press(Message::PlayerPanel(PlayerPanel::None)),
                column![
                    Space::new().height(Fill),
                    {
                        let anchored = if p.panel == PlayerPanel::Remote {
                            row![sheet, Space::new().width(Fill)]
                        } else {
                            row![Space::new().width(Fill), sheet]
                        };
                        anchored
                    }
                    .padding(Padding {
                        top: 0.0,
                        right: 18.0 + p.safe.right,
                        bottom: 8.0,
                        left: 18.0 + p.safe.left,
                    })
                    .width(Fill),
                ]
                .width(Fill)
                .height(Fill),
            ]
            .width(Fill)
            .height(Fill)
            .into()
        } else if p.panel != PlayerPanel::None {
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
            Space::new().width(Fill).height(Fill).into()
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
        // Mouse movement is handled on the window, so the bar shows without a click.
        mouse_area(Space::new().width(Fill).height(Fill))
            .on_press(Message::PlayerStageClick)
            .on_double_click(Message::ToggleFullscreen)
            .interaction(iced::mouse::Interaction::Hidden)
            .into()
    };

    let picture = mouse_area(
        stack![stage, overlay]
            .width(Fill)
            .height(Fill),
    )
    // Movement is watched on the player window (throttled). A click still wakes
    // the bar, and a double-click toggles fullscreen. Dock buttons capture the press.
    .on_press(Message::PlayerStageClick)
    .on_double_click(Message::ToggleFullscreen);

    // Above the video and the bar. The old slot sat in a column that collapsed,
    // so the control was detected in the log and never drawn.
    let picture: Element<'_, Message> = match p.session.skip_prompt() {
        Some(label) => stack![picture, skip_pill(label, interactive, p.safe)]
            .width(Fill)
            .height(Fill)
            .into(),
        None => picture.into(),
    };
    if p.exit_ask {
        stack![picture, exit_dialog()]
            .width(Fill)
            .height(Fill)
            .into()
    } else {
        picture
    }
}

/// Title-bar close: the window stays open until the viewer picks.
fn exit_dialog() -> Element<'static, Message> {
    let scrim = mouse_area(
        container(Space::new().width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(|_t: &Theme| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.55))),
                ..Default::default()
            }),
    )
    .on_press(Message::PlayerDismissExit);
    let choice = |label: &'static str, msg: Message| -> Element<'static, Message> {
        mouse_area(
            container(text(label).size(15).color(Color::WHITE))
                .padding(Padding::from([12, 16]))
                .width(Fill)
                .style(|_t: &Theme| container::Style {
                    background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.12))),
                    border: Border {
                        radius: 12.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        )
        .on_press(msg)
        .into()
    };
    let card = container(
        column![
            text("Fermer").size(20).color(Color::WHITE),
            text("Fermer seulement le lecteur, ou quitter FluxPlay ?")
                .size(14)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.78)),
            choice("Fermer le lecteur", Message::ClosePlayerWindow),
            choice("Quitter FluxPlay", Message::PlayerQuitApp),
            choice("Annuler", Message::PlayerDismissExit),
        ]
        .spacing(10)
        .width(Length::Fixed(320.0)),
    )
    .padding(18)
    .style(|_t: &Theme| container::Style {
        background: Some(Background::Color(Color::from_rgba(0.09, 0.09, 0.11, 0.96))),
        border: Border {
            radius: 16.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    stack![
        scrim,
        container(card)
            .width(Fill)
            .height(Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
    ]
    .width(Fill)
    .height(Fill)
    .into()
}

/// Bottom-right pill, drawn above the video. Sits clear of the dock while
/// the bar is open, and stays on screen after the bar hides.
fn skip_pill<'a>(label: &'static str, chrome_visible: bool, safe: Padding) -> Element<'a, Message> {
    let skip = button(text(label).size(16).color(Color::WHITE))
        .on_press(Message::SkipGeneric)
        .padding(Padding {
            top: 12.0,
            right: 20.0,
            bottom: 12.0,
            left: 20.0,
        })
        .style(|_t: &Theme, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(Background::Color(Color::from_rgba(
                    0.04,
                    0.04,
                    0.05,
                    if hot { 0.96 } else { 0.88 },
                ))),
                text_color: Color::WHITE,
                border: Border {
                    color: Color::WHITE,
                    width: 1.5,
                    radius: RADIUS_FULL.into(),
                },
                ..Default::default()
            }
        });
    let bottom = if chrome_visible { 112.0 } else { 28.0 };
    container(skip)
        .width(Fill)
        .height(Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .align_y(iced::alignment::Vertical::Bottom)
        .padding(Padding {
            top: 0.0,
            right: 28.0 + safe.right,
            bottom: bottom + safe.bottom,
            left: 0.0,
        })
        .into()
}

/// Centered pill while the embedded player holds to refill its network buffer.
fn rebuffer_badge<'a>(ui: UiTheme, session: &StreamSession) -> Element<'a, Message> {
    let Some(b) = session
        .buffer
        .filter(|b| b.rebuffering && session.state == PlaybackState::Buffering)
    else {
        return Space::new().width(Fill).height(Fill).into();
    };
    let mut body = column![text(format!("Mise en mémoire tampon… {} %", b.fill_percent()))
        .size(TYPE_LABEL_L)
        .color(ui.on_primary_container())]
    .spacing(SPACE_XS)
    .align_x(Alignment::Center);
    if b.link_too_slow() {
        body = body.push(
            text(format!(
                "Connexion {:.0} Mbit/s, ce film en demande {:.0} : \
                 une version HD ou le téléchargement évitent les pauses.",
                b.net_mbps, b.media_mbps
            ))
            .size(TYPE_LABEL_M)
            .color(ui.on_primary_container()),
        );
    }
    let pill = container(body)
        .max_width(520)
        .padding(Padding::from([14, 22]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.primary_container())),
            border: Border {
                radius: RADIUS_EXTRA_LARGE.into(),
                ..Default::default()
            },
            shadow: elevation_shadow(2, ui.day),
            ..Default::default()
        });
    container(pill)
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill)
        .into()
}

#[cfg(not(target_os = "android"))]
fn gpu_stage_available() -> bool {
    true
}

#[cfg(target_os = "android")]
fn gpu_stage_available() -> bool {
    false
}

#[cfg(not(target_os = "android"))]
fn gpu_stage_view<'a>() -> Element<'a, Message> {
    crate::video_stage::view()
}

#[cfg(target_os = "android")]
fn gpu_stage_view<'a>() -> Element<'a, Message> {
    Space::new().width(Fill).height(Fill).into()
}

/// Soft RGBA frame as an iced image (no GPU stage).
fn soft_image<'a>(frame: &'a Handle) -> Element<'a, Message> {
    // Soft RGBA is already letterboxed into the stage-shaped buffer (mpv keepaspect /
    // aspect-safe vf). Contain would double-letterbox after rotate.
    iced::widget::image(frame)
        .width(Fill)
        .height(Fill)
        .content_fit(iced::ContentFit::Fill)
        .filter_method(iced::widget::image::FilterMethod::Linear)
        .into()
}

/// Until the first frame, the poster fills the stage (Rune shows a poster
/// before the decoder starts). The loader card is only there when no poster
/// has arrived. The bar and the menus are unchanged.
fn waiting_stage<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let Some(handle) = p.art else {
        return container(video_loader(p))
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill)
            .into();
    };
    stack![
        iced::widget::image(handle)
            .width(Fill)
            .height(Fill)
            .content_fit(iced::ContentFit::Contain),
        container(text("Chargement vidéo…").size(TYPE_LABEL_L).color(Color::WHITE))
            .width(Fill)
            .height(Fill)
            .align_y(Alignment::End)
            .padding(Padding {
                top: 0.0,
                right: 20.0,
                bottom: 28.0,
                left: 20.0,
            }),
    ]
    .width(Fill)
    .height(Fill)
    .into()
}

/// Loader until the first frame — avoid a black gap when the clock leads
/// video, and never flip poster↔frame (scintillation).
fn video_loader<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    container(
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
    })
    .into()
}

fn stage_panel<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let session = p.session;
    let playing = matches!(
        session.state,
        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
    );

    let center: Element<'a, Message> = if p.surface_video {
        // SurfaceView under iced — never paint art/loader (opaque) over MediaCodec.
        Space::new().width(Fill).height(Fill).into()
    } else if playing && p.embedded_video && gpu_stage_available() {
        // The shader stage stays in the tree for the whole playback: drawing it
        // once builds the pipeline, which switches the player to native YUV.
        // Until its first frame the image path (or the loader) covers it.
        let top: Element<'a, Message> = if p.stage_picture {
            rebuffer_badge(ui, session)
        } else if let Some(frame) = p.video {
            stack![soft_image(frame), rebuffer_badge(ui, session)]
                .width(Fill)
                .height(Fill)
                .into()
        } else {
            waiting_stage(p)
        };
        stack![gpu_stage_view(), top].width(Fill).height(Fill).into()
    } else if let Some(frame) = p.video {
        // Same tree with or without the badge: no image relayout when buffering toggles.
        stack![soft_image(frame), rebuffer_badge(ui, session)]
            .width(Fill)
            .height(Fill)
            .into()
    } else if playing && p.embedded_video {
        waiting_stage(p)
    } else if playing && !p.embedded_video {
        // CLI mpv/ffplay fallback: video is in an external OS window — never spin forever here.
        let art_block: Element<'a, Message> = if let Some(handle) = p.art {
            let fit = crate::images::art_content_fit(handle);
            iced::widget::image(handle)
                .width(Length::Fixed(220.0))
                .height(Length::Fixed(320.0))
                .content_fit(fit)
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
    let stage_bg = if p.surface_video {
        Color::TRANSPARENT
    } else {
        stage_black()
    };
    container(center)
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill)
        .clip(true)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(stage_bg)),
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
    let _can_seek = active && !live && p.caps.seek_abs;
    let can_seek_rel = active && !live && p.caps.seek_rel;
    let mute_glyph = if s.muted { Icon::VolumeOff } else { Icon::VolumeUp };
    let progress = p.seek_drag.unwrap_or_else(|| s.progress_ratio());
    let time_label = s.elapsed_label();
    let vol = s.volume;
    let vol_enabled = active && p.caps.volume_live;
    let mute_enabled = active && p.caps.mute;
    let pause_enabled = active && p.caps.pause;
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
            text(time_label)
                .size(TYPE_LABEL_S)
                .color(Color::from_rgba(0.96, 0.96, 0.97, 0.78)),
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
        let pos_l = clock(s.position_secs);
        let dur_l = if s.duration_secs > 1.0 {
            clock(s.duration_secs)
        } else {
            String::new()
        };
        row![
            dock_time(&pos_l, true),
            slider(0.0..=1.0, progress as f32, |v| Message::SeekPercent(v as f64))
                .on_release(Message::SeekReleased)
                .step(0.001_f32)
                .width(Fill)
                .height(20.0)
                .style(move |theme: &Theme, status| {
                    let mut st = slider::default(theme, status);
                    st.rail.backgrounds.0 =
                        Background::Color(Color::from_rgba(0.96, 0.97, 0.98, 0.95));
                    st.rail.backgrounds.1 =
                        Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.22));
                    st.rail.width = 4.0;
                    st.rail.border.radius = 4.0.into();
                    st.handle.background = Background::Color(Color::from_rgb8(0xF4, 0xF5, 0xF7));
                    st.handle.border_color = Color::TRANSPARENT;
                    st.handle.border_width = 0.0;
                    st.handle.shape = slider::HandleShape::Circle { radius: 6.0 };
                    st
                }),
            dock_time(&dur_l, false),
        ]
        .spacing(8)
        .padding(Padding {
            top: 0.0,
            right: 22.0,
            bottom: 0.0,
            left: 22.0,
        })
        .align_y(Alignment::Center)
        .width(Fill)
        .into()
    };

    // Transport: −10 | play | +10. Phone dock only; desktop uses the YouTube row.
    #[cfg(target_os = "android")]
    let transport = row![
        toolbar_svg(ui, Icon::Replay10, Message::SeekRel(-10), can_seek_rel, false),
        play_fab(ui, play_glyph, Message::TogglePause, pause_enabled),
        toolbar_svg(ui, Icon::Forward10, Message::SeekRel(10), can_seek_rel, false),
    ]
    .spacing(SPACE_XS)
    .align_y(Alignment::Center);

    #[cfg(target_os = "android")]
    {
        // Phone width: never pack prev/vol/transport/more into one overflowing row.
        // Row 1 — channel skip + transport (centered).
        // Row 2 — mute + volume | more / fullscreen / close.
        let transport_row = row![
            toolbar_svg(ui, Icon::SkipPrevious, Message::PlaylistPrev, true, false),
            transport,
            toolbar_svg(ui, Icon::SkipNext, Message::PlaylistNext, true, false),
        ]
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
        .width(Length::Shrink);

        let vol_slider: Element<'a, Message> = if vol_enabled {
            container(
                slider(0.0..=1.0, vol, Message::VolumeChanged).on_release(Message::VolumeReleased)
                    .step(0.01_f32)
                    .height(32.0)
                    .style(dock_slider_style),
            )
            .width(Fill)
            .into()
        } else {
            Space::new().width(Fill).into()
        };

        let actions = row![
            toolbar_svg(ui, mute_glyph, Message::ToggleMute, mute_enabled, s.muted),
            vol_slider,
            toolbar_svg(
                ui,
                Icon::More,
                if p.panel == PlayerPanel::More {
                    Message::PlayerPanel(PlayerPanel::None)
                } else {
                    Message::PlayerPanel(PlayerPanel::More)
                },
                true,
                p.panel == PlayerPanel::More,
            ),
            toolbar_svg(ui, Icon::Fullscreen, Message::ToggleFullscreen, true, p.fullscreen),
            toolbar_svg(ui, Icon::Close, Message::ClosePlayerWindow, true, false),
        ]
        .spacing(SPACE_XS)
        .align_y(Alignment::Center)
        .width(Fill);

        let floating = container(
            column![
                scrub,
                container(transport_row)
                    .width(Fill)
                    .center_x(Fill)
                    .padding(Padding::from([SPACE_XXS as u16, 0])),
                actions,
            ]
            .spacing(SPACE_XXS)
            .padding(Padding {
                top: SPACE_XS,
                right: SPACE_SM,
                bottom: SPACE_XS,
                left: SPACE_SM,
            })
            .height(Length::Shrink),
        )
        .width(Fill)
        .height(Length::Shrink)
        .style(move |_t: &Theme| {
            container::Style {
                background: Some(Background::Color(dock_fill(alpha))),
                border: Border {
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.08),
                    width: 1.0,
                    radius: 16.0.into(),
                },
                shadow: Default::default(),
                ..Default::default()
            }
        });

        return container(floating)
            .width(Fill)
            .height(Length::Shrink)
            .padding(Padding {
                top: 0.0,
                right: TOOLBAR_OUTER_PAD + p.safe.right,
                bottom: TOOLBAR_OUTER_PAD + p.safe.bottom,
                left: TOOLBAR_OUTER_PAD + p.safe.left,
            })
            .style(move |_t: &Theme| container::Style {
                background: None,
                ..Default::default()
            })
            .into();
    }

    #[cfg(not(target_os = "android"))]
    let vol_slider: Element<'a, Message> = if vol_enabled {
        container(
            slider(0.0..=1.0, vol, Message::VolumeChanged)
                .on_release(Message::VolumeReleased)
                .step(0.01_f32)
                .width(Length::Fixed(84.0))
                .height(20.0)
                .style(dock_slider_style),
        )
        .width(Length::Fixed(84.0))
        .into()
    } else {
        Space::new().width(0).into()
    };

    // Previous episode, −10s, play, +10s and next episode stay centered.
    // The times live on the scrub bar, not in this row.
    #[cfg(not(target_os = "android"))]
    let fs_icon = if p.fullscreen {
        Icon::FullscreenExit
    } else {
        Icon::Fullscreen
    };
    #[cfg(not(target_os = "android"))]
    let side_cluster = row![
        toolbar_svg(ui, mute_glyph, Message::ToggleMute, mute_enabled, s.muted),
        vol_slider,
        toolbar_svg(ui, fs_icon, Message::ToggleFullscreen, true, false),
        toolbar_svg(
            ui,
            Icon::Settings,
            if p.panel == PlayerPanel::More {
                Message::PlayerPanel(PlayerPanel::None)
            } else {
                Message::PlayerPanel(PlayerPanel::More)
            },
            true,
            p.panel == PlayerPanel::More,
        ),
    ]
    .spacing(SPACE_XS)
    .align_y(Alignment::Center);
    #[cfg(not(target_os = "android"))]
    let center_cluster = row![
        toolbar_svg(ui, Icon::SkipPrevious, Message::PlaylistPrev, true, false),
        toolbar_svg(ui, Icon::Replay10, Message::SeekRel(-10), can_seek_rel, false),
        yt_glyph(play_glyph, Message::TogglePause, pause_enabled),
        toolbar_svg(ui, Icon::Forward10, Message::SeekRel(10), can_seek_rel, false),
        toolbar_svg(ui, Icon::SkipNext, Message::PlaylistNext, true, false),
    ]
    .spacing(SPACE_SM)
    .align_y(Alignment::Center);
    #[cfg(not(target_os = "android"))]
    let remote_label = if p.remote_on {
        "Télécommande : on"
    } else {
        "Télécommande : off"
    };
    #[cfg(not(target_os = "android"))]
    let remote_open = p.panel == PlayerPanel::Remote;
    #[cfg(not(target_os = "android"))]
    let remote_on = p.remote_on;
    #[cfg(not(target_os = "android"))]
    let remote_btn = button(text(remote_label).size(14).color(Color::WHITE))
        .padding(Padding::from([8, 16]))
        .on_press(if remote_open {
            Message::PlayerPanel(PlayerPanel::None)
        } else {
            Message::PlayerPanel(PlayerPanel::Remote)
        })
        .style(move |_t: &Theme, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let lit = hot || remote_open || remote_on;
            button::Style {
                background: Some(Background::Color(Color::from_rgba(
                    0.04,
                    0.04,
                    0.05,
                    if lit { 0.96 } else { 0.88 },
                ))),
                text_color: Color::WHITE,
                border: Border {
                    color: Color::from_rgba(1.0, 1.0, 1.0, if remote_on { 1.0 } else { 0.72 }),
                    width: if remote_on { 1.5 } else { 1.0 },
                    radius: RADIUS_FULL.into(),
                },
                ..Default::default()
            }
        });
    #[cfg(not(target_os = "android"))]
    let toolbar_row = row![
        container(remote_btn)
            .width(Fill)
            .align_x(Alignment::Start)
            .align_y(Alignment::Center),
        center_cluster,
        container(side_cluster)
            .width(Fill)
            .align_x(Alignment::End)
            .align_y(Alignment::Center),
    ]
    .align_y(Alignment::Center)
    .width(Fill);

    #[cfg(not(target_os = "android"))]
    let floating = container(
        column![scrub, toolbar_row]
            .spacing(4)
            .padding(Padding {
                top: 4.0,
                right: 8.0,
                bottom: 6.0,
                left: 4.0,
            })
            .height(Length::Shrink),
    )
    .width(Fill)
    .height(Length::Shrink)
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.72 * alpha))),
        border: Border {
            radius: 0.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });

    #[cfg(not(target_os = "android"))]
    {
        container(floating)
            .width(Fill)
            .height(Length::Shrink)
            .padding(Padding {
                top: 0.0,
                right: 16.0 + p.safe.right,
                bottom: 14.0 + p.safe.bottom,
                left: 16.0 + p.safe.left,
            })
            .style(move |_t: &Theme| container::Style {
                background: None,
                ..Default::default()
            })
            .into()
    }
}

fn dock_fill(alpha: f32) -> Color {
    Color::from_rgba(0.06, 0.07, 0.09, 0.88 * alpha.clamp(0.0, 1.0))
}

fn dock_time<'a>(label: &str, align_end: bool) -> Element<'a, Message> {
    container(
        text(label.to_string())
            .size(TYPE_LABEL_S)
            .color(Color::from_rgba(0.96, 0.96, 0.97, 0.78)),
    )
    .width(Length::Fixed(52.0))
    .height(Length::Fixed(18.0))
    .align_y(iced::alignment::Vertical::Center)
    .align_x(if align_end {
        iced::alignment::Horizontal::Right
    } else {
        iced::alignment::Horizontal::Left
    })
    .into()
}

fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

fn dock_slider_style(theme: &Theme, status: slider::Status) -> slider::Style {
    let mut st = slider::default(theme, status);
    st.rail.backgrounds.0 = Background::Color(Color::from_rgba(0.96, 0.97, 0.98, 0.95));
    st.rail.backgrounds.1 = Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.22));
    st.rail.width = 3.0;
    st.handle.background = Background::Color(Color::from_rgb8(0xF4, 0xF5, 0xF7));
    st.handle.border_color = Color::TRANSPARENT;
    st.handle.border_width = 0.0;
    st.handle.shape = slider::HandleShape::Circle { radius: 5.0 };
    st
}

fn truncate(s: &str, max: usize) -> String {
    #[cfg(target_os = "android")]
    let cleaned: String = {
        s.chars()
            .map(|c| {
                if matches!(c, ' '..='~' | '\u{00A0}'..='\u{024F}')
                    || matches!(
                        c,
                        '·' | '•' | '…' | '—' | '–' | '«' | '»' | '€' | '°' | '×' | '÷'
                    )
                {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    #[cfg(target_os = "android")]
    let s = cleaned.as_str();
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
    let can_tracks = active && p.caps.tracks_filters;
    let can_speed = active && p.caps.speed_loop;
    let audio = s.audio_tracks();
    let subs = s.subtitle_tracks();
    let subs_off = subs.iter().all(|t| !t.selected);

    let audio_chips: Vec<Element<'static, Message>> = if audio.is_empty() {
        vec![text("aucune").size(12).color(ui.ink_muted()).into()]
    } else {
        audio
            .into_iter()
            .map(|t| {
                choice_chip(
                    ui,
                    &t.label,
                    Message::SelectAudio(t.id),
                    can_tracks,
                    t.selected,
                )
            })
            .collect()
    };
    let mut sub_chips = vec![choice_chip(
        ui,
        "Aucun",
        Message::SelectSubtitle(None),
        can_tracks,
        subs_off,
    )];
    for t in subs {
        sub_chips.push(choice_chip(
            ui,
            &t.label,
            Message::SelectSubtitle(Some(t.id)),
            can_tracks,
            t.selected,
        ));
    }
    let quality = p.quality;
    let quality_chips: Vec<_> = VideoQualityPref::all()
        .iter()
        .copied()
        .map(|q| {
            choice_chip(
                ui,
                q.short_label(),
                Message::SetVideoQuality(q),
                true,
                quality == q,
            )
        })
        .collect();
    let speeds = [0.5_f64, 0.75, 1.0, 1.25, 1.5, 2.0];
    let speed_chips: Vec<_> = speeds
        .into_iter()
        .map(|v| {
            choice_chip(
                ui,
                &speed_label(v),
                Message::SetSpeed(v),
                can_speed,
                (s.speed - v).abs() < 0.06,
            )
        })
        .collect();

    let line = |label: &'static str, chips: Vec<Element<'static, Message>>| {
        row![
            container(text(label).size(12).color(ui.ink_muted()))
                .width(Length::Fixed(88.0))
                .align_y(Alignment::Center),
            Row::with_children(chips).spacing(8).width(Fill).wrap(),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Fill)
    };

    // Swallow presses so the click-outside layer under the card does not
    // close and reopen it while the pointer sits on a chip.
    mouse_area(
        container(
            column![
                line("Audio", audio_chips),
                line("Sous-titres", sub_chips),
                line("Vitesse", speed_chips),
                line("Qualité", quality_chips),
                line(
                    "Générique",
                    vec![choice_chip(
                        ui,
                        "Automatique",
                        Message::ToggleSkipCredits,
                        true,
                        p.skip_auto,
                    )],
                ),
            ]
            .spacing(12)
            .width(Fill),
        )
        .width(Length::Fixed(440.0))
        .padding(Padding::from([14, 16]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_high())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: RADIUS_LG.into(),
            },
            ..Default::default()
        }),
    )
    .on_press(Message::PlayerPanel(PlayerPanel::More))
    .into()
}

fn remote_sheet<'a>(p: &PlayerChrome<'a>) -> Element<'a, Message> {
    let ui = p.ui;
    let qr: Element<'_, Message> = if let Some(handle) = p.remote_qr {
        container(
            iced::widget::image::Image::new(handle.clone())
                .filter_method(iced::widget::image::FilterMethod::Nearest)
                .width(Length::Fixed(p.remote_qr_side))
                .height(Length::Fixed(p.remote_qr_side)),
        )
        .padding(8)
        .style(|_t: &Theme| container::Style {
            background: Some(Background::Color(Color::WHITE)),
            border: Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
    } else {
        text("Le QR se prépare avec le lien.")
            .size(12)
            .color(ui.ink_muted())
            .into()
    };
    let state = if p.remote_on {
        "Télécommande : on"
    } else {
        "Télécommande : off"
    };
    mouse_area(
        container(
            column![
                text(state).size(16).color(ui.on_surface()),
                text("Scanne le QR avec FluxPlay sur le téléphone, ou colle un lien fp1|…")
                    .size(12)
                    .color(ui.ink_muted()),
                qr,
                text(p.remote_link).size(11).color(ui.ink_muted()),
                text_input("Coller le lien fp1|…", p.remote_draft).on_input(Message::PartyDraft),
                row![
                    chip_btn(ui, "Copier le lien", Message::PartyCopy, true),
                    chip_btn(
                        ui,
                        "Associer",
                        Message::PartyPair(p.remote_draft.to_string()),
                        !p.remote_draft.trim().is_empty(),
                    ),
                ]
                .spacing(8),
                text(p.remote_status).size(12).color(ui.ink_muted()),
            ]
            .spacing(8)
            .width(Fill),
        )
        .width(Length::Fixed(360.0))
        .padding(Padding::from([14, 16]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_high())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: RADIUS_LG.into(),
            },
            ..Default::default()
        }),
    )
    .on_press(Message::PlayerPanel(PlayerPanel::Remote))
    .into()
}

fn speed_label(v: f64) -> String {
    let s = if (v - v.round()).abs() < 0.001 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .replace('.', ",")
    };
    format!("{s}×")
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
                mouse_area(
                    container(icons::icon(Icon::Paste, 18.0, ui.accent()))
                        .padding(8)
                        .style(move |_t: &Theme| container::Style {
                            background: Some(Background::Color(ui.surface_container_highest())),
                            border: Border {
                                radius: RADIUS_FULL.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                )
                .on_press(Message::PasteInto(PasteTarget::Goto)),
                chip_btn(ui, "OK", Message::GotoSubmit, true),
                chip_btn(ui, "Annuler", Message::PlayerPanel(PlayerPanel::None), true),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(10),
    )
}

#[allow(dead_code)]
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

fn choice_chip(
    ui: UiTheme,
    label: &str,
    msg: Message,
    enabled: bool,
    selected: bool,
) -> Element<'static, Message> {
    let label = label.to_string();
    let (bg, ink, border) = if selected {
        (ui.accent(), ui.on_primary(), ui.accent())
    } else {
        (ui.surface_container_highest(), ui.ink(), ui.outline_variant())
    };
    let body = container(text(label).size(13).color(ink))
        .padding(Padding::from([7, 12]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                radius: RADIUS_FULL.into(),
                color: border,
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

/// White YouTube-style glyph, no disc behind the play button.
#[cfg(not(target_os = "android"))]
fn yt_glyph(kind: Icon, msg: Message, enabled: bool) -> Element<'static, Message> {
    let ink = Color::from_rgba(1.0, 1.0, 1.0, if enabled { 1.0 } else { 0.35 });
    let body = container(icons::icon(kind, 30.0, ink)).padding(Padding::from([4, 6]));
    if enabled {
        mouse_area(body).on_press(msg).into()
    } else {
        body.into()
    }
}

/// Icon button on vibrant floating toolbar (`onPrimaryContainer`).
fn toolbar_svg(
    _ui: UiTheme,
    kind: Icon,
    msg: Message,
    enabled: bool,
    active: bool,
) -> Element<'static, Message> {
    let ink = Color::from_rgba(0.96, 0.96, 0.97, if enabled { 0.94 } else { 0.32 });
    let radius = if active {
        RADIUS_LG.into()
    } else {
        RADIUS_FULL.into()
    };
    let bg = if active {
        Color::from_rgba(1.0, 1.0, 1.0, 0.16)
    } else {
        Color::TRANSPARENT
    };
    let body = container(icons::icon(kind, 22.0, ink))
        .padding({
            #[cfg(target_os = "android")]
            {
                Padding::from([8, 8])
            }
            #[cfg(not(target_os = "android"))]
            {
                Padding::from([10, 12])
            }
        })
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
#[allow(dead_code)]
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

/// Play / pause disc. Light on the dark dock, not the app accent.
fn play_fab(ui: UiTheme, kind: Icon, msg: Message, enabled: bool) -> Element<'static, Message> {
    let fill = Color::from_rgb8(0xF4, 0xF5, 0xF7);
    let ink = Color::from_rgb8(0x14, 0x16, 0x1C);
    #[cfg(target_os = "android")]
    let fab_r = RADIUS_FULL.into();
    #[cfg(not(target_os = "android"))]
    let fab_r = radius_fab();
    // Prefer `.center(FAB_MEDIUM)` — iced's `center_x(Fill)` *replaces* width with Fill
    // (not merely aligns), which stretched the FAB into a full-width stadium pill.
    let body = container(icons::icon(kind, 22.0, ink))
        .center(40.0)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(fill)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: fab_r,
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
