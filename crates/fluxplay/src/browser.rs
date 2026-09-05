//! Media-center chrome: dense panes, capped lists, reliable selection.
//! Inspired by TiviMate / MYTV / IPTVnator / Smarters + Material 3 Expressive.
//!
//! Pure view helpers — operational logs live in `main` Message handlers.

use iced::widget::{
    button, column, container, image, row, scrollable, text, text_input, Column, Row, Space,
};
use iced::widget::image::Handle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Shadow, Theme,
};

use crate::theme::{
    UiTheme, MOSAIC_GAP, RADIUS_FULL, RADIUS_MD, RADIUS_SM, RADIUS_XL,
};
use crate::Message;

pub const LIST_PAGE: usize = 24;
pub const CAT_PAGE: usize = 60;

pub fn shell_background(ui: UiTheme) -> Color {
    ui.shell()
}

pub fn pane<'a>(
    ui: UiTheme,
    width: Length,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    container(body)
        .width(width)
        .height(Fill)
        .padding(12)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface())),
            border: Border {
                color: ui.outline(),
                width: 1.0,
                radius: RADIUS_XL.into(),
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, if ui.day { 0.05 } else { 0.35 }),
                offset: iced::Vector::new(0.0, 8.0),
                blur_radius: 24.0,
            },
            ..Default::default()
        })
        .into()
}

pub fn mode_rail<'a>(
    ui: UiTheme,
    width: f32,
    items: impl IntoIterator<Item = (&'a str, Message, bool)>,
) -> Element<'a, Message> {
    let mut col = Column::new().spacing(6).width(Fill);
    col = col.push(
        column![
            text("FluxPlay").size(22).color(ui.accent()),
            text("Media Center").size(11).color(ui.ink_muted()),
        ]
        .spacing(2),
    );
    col = col.push(Space::new().height(10));
    for (label, msg, active) in items {
        col = col.push(rail_btn(label, msg, ui, active));
    }
    pane(ui, Length::Fixed(width), col)
}

fn rail_btn<'a>(label: &'a str, msg: Message, ui: UiTheme, active: bool) -> Element<'a, Message> {
    let fg = if active {
        ui.on_primary_container()
    } else {
        ui.ink()
    };
    button(text(label).size(14).color(fg))
        .on_press(msg)
        .padding(Padding::from([12, 14]))
        .width(Fill)
        .style(move |_theme: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered);
            let bg = if active {
                ui.primary_container()
            } else if hovered {
                ui.surface_muted()
            } else {
                Color::TRANSPARENT
            };
            button::Style {
                background: Some(Background::Color(bg)),
                text_color: fg,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: RADIUS_FULL.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

pub fn category_sidebar<'a>(
    ui: UiTheme,
    width: f32,
    title: &'a str,
    filter: &str,
    entries: Vec<(String, String, bool, usize)>,
) -> Element<'a, Message> {
    let filter_input = text_input("Filtrer…", filter)
        .on_input(Message::CatFilterChanged)
        .padding(10)
        .size(13)
        .style(move |theme: &Theme, status| {
            let mut s = text_input::default(theme, status);
            s.border.radius = RADIUS_MD.into();
            s.background = Background::Color(ui.surface_muted());
            s
        });

    let mut list = Column::new().spacing(2).width(Fill);
    if entries.is_empty() {
        list = list.push(text("Aucune catégorie").size(12).color(ui.ink_muted()));
    }
    for (id, name, active, count) in entries {
        let label = if count > 0 {
            format!("{name}  · {count}")
        } else {
            name
        };
        list = list.push(cat_row(label, Message::SelectBrowseCategory(id), ui, active));
    }

    pane(
        ui,
        Length::Fixed(width),
        column![
            text(title).size(14).color(ui.ink_muted()),
            filter_input,
            scrollable(list).height(Fill),
        ]
        .spacing(8)
        .height(Fill),
    )
}

fn cat_row(label: String, msg: Message, ui: UiTheme, active: bool) -> Element<'static, Message> {
    let fg = if active {
        ui.accent()
    } else {
        ui.ink()
    };
    button(
        container(text(label).size(13).color(fg))
            .width(Fill)
            .padding(Padding::from([8, 10])),
    )
    .on_press(msg)
    .padding(0)
    .width(Fill)
    .style(move |theme: &Theme, status| {
        let mut s = button::text(theme, status);
        s.border.radius = RADIUS_SM.into();
        s.text_color = fg;
        if active {
            s.background = Some(Background::Color(ui.surface_elevated()));
            s.border.width = 1.0;
            s.border.color = ui.accent();
        } else if matches!(status, button::Status::Hovered) {
            s.background = Some(Background::Color(ui.surface_muted()));
        }
        s
    })
    .into()
}

pub fn content_header<'a>(
    ui: UiTheme,
    title: String,
    subtitle: String,
    search: &str,
    search_width: f32,
) -> Element<'a, Message> {
    row![
        column![
            text(title).size(22),
            text(subtitle).size(12).color(ui.ink_muted()),
        ]
        .spacing(2)
        .width(Fill),
        text_input("Rechercher…", search)
            .on_input(Message::SearchChanged)
            .padding(12)
            .size(14)
            .width(Length::Fixed(search_width.clamp(140.0, 420.0)))
            .style(move |theme: &Theme, status| {
                let mut s = text_input::default(theme, status);
                s.border.radius = RADIUS_MD.into();
                s.background = Background::Color(ui.surface_muted());
                s
            }),
    ]
    .spacing(12)
    .align_y(Alignment::Center)
    .into()
}

pub fn media_row<'a>(
    title: String,
    subtitle: String,
    on_open: Message,
    on_fav: Option<(bool, Message)>,
    ui: UiTheme,
    active: bool,
    thumb: Option<&'a Handle>,
) -> Element<'a, Message> {
    let title_c = if active { ui.accent() } else { ui.ink() };

    let thumb_el: Element<'a, Message> = if let Some(handle) = thumb {
        container(
            image(handle)
                .width(Length::Fixed(56.0))
                .height(Length::Fixed(56.0))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Length::Fixed(56.0))
        .height(Length::Fixed(56.0))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_muted())),
            border: Border {
                radius: RADIUS_SM.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
    } else {
        container(text(" ").size(10))
            .width(Length::Fixed(56.0))
            .height(Length::Fixed(56.0))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_muted())),
                border: Border {
                    radius: RADIUS_SM.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into()
    };

    let body = column![
        text(title).size(14).color(title_c),
        text(subtitle).size(11).color(ui.ink_muted()),
    ]
    .spacing(2)
    .width(Fill);

    let mut row = Row::new().spacing(8).align_y(Alignment::Center).width(Fill);
    row = row.push(thumb_el);
    row = row.push(
        button(container(body).padding(Padding::from([8, 10])).width(Fill))
            .on_press(on_open)
            .padding(0)
            .width(Fill)
            .style(move |theme: &Theme, status| {
                let mut s = button::text(theme, status);
                s.border.radius = RADIUS_MD.into();
                s.text_color = title_c;
                let mut bg = if active {
                    ui.surface_elevated()
                } else {
                    Color::TRANSPARENT
                };
                if matches!(status, button::Status::Hovered) {
                    bg = ui.surface_muted();
                }
                s.background = Some(Background::Color(bg));
                if active {
                    s.border.width = 1.5;
                    s.border.color = ui.accent();
                }
                s
            }),
    );
    if let Some((is_fav, fav_msg)) = on_fav {
        row = row.push(
            button(text(if is_fav { "★" } else { "☆" }).size(16))
                .on_press(fav_msg)
                .padding(10)
                .style(move |theme: &Theme, status| {
                    let mut s = button::secondary(theme, status);
                    s.border.radius = RADIUS_MD.into();
                    s.background = Some(Background::Color(ui.surface_muted()));
                    s.text_color = if is_fav {
                        Color::from_rgb8(0xF5, 0xBF, 0x2A)
                    } else {
                        ui.ink_muted()
                    };
                    s
                }),
        );
    }
    row.into()
}

pub fn empty_hint(ui: UiTheme, msg: impl Into<String>) -> Element<'static, Message> {
    container(
        text(msg.into())
            .size(14)
            .color(ui.ink_muted()),
    )
    .padding(24)
    .width(Fill)
    .center_x(Fill)
    .into()
}

pub fn load_more_btn(ui: UiTheme, remaining: usize) -> Element<'static, Message> {
    button(text(format!("Afficher plus (+{remaining})")).size(13))
        .on_press(Message::LoadMore)
        .padding(12)
        .width(Fill)
        .style(move |theme: &Theme, status| {
            let mut s = button::secondary(theme, status);
            s.border.radius = RADIUS_MD.into();
            s.background = Some(Background::Color(ui.surface_muted()));
            s.text_color = ui.accent();
            s
        })
        .into()
}

/// MYTV-style poster tile: vertical art + title + "year, genre".
pub fn mosaic_tile<'a>(
    title: String,
    meta_line: String,
    on_open: Message,
    ui: UiTheme,
    tile_w: f32,
    thumb: Option<&'a Handle>,
) -> Element<'a, Message> {
    let w = tile_w.max(120.0);
    // Classic poster aspect ~2:3
    let h = (w * 1.5).round();

    let poster: Element<'a, Message> = if let Some(handle) = thumb {
        container(
            image(handle)
                .width(Length::Fixed(w))
                .height(Length::Fixed(h))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Length::Fixed(w))
        .height(Length::Fixed(h))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_muted())),
            border: Border {
                radius: RADIUS_XL.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            ..Default::default()
        })
        .into()
    } else {
        container(Space::new().width(w).height(h))
            .width(Length::Fixed(w))
            .height(Length::Fixed(h))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_muted())),
                border: Border {
                    radius: RADIUS_XL.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into()
    };

    let title_el = text(title).size(13).color(ui.ink());
    let meta_el = text(meta_line).size(11).color(ui.ink_muted());

    let body = column![poster, title_el, meta_el]
        .spacing(4)
        .width(Length::Fixed(w));

    button(body)
        .on_press(on_open)
        .padding(4)
        .width(Length::Fixed(w + 8.0))
        .style(move |theme: &Theme, status| {
            let mut s = button::text(theme, status);
            s.border.radius = RADIUS_MD.into();
            if matches!(status, iced::widget::button::Status::Hovered) {
                s.background = Some(Background::Color(ui.surface_elevated()));
            }
            s
        })
        .into()
}

pub fn mosaic_grid<'a>(tiles: Vec<Element<'a, Message>>, cols: usize) -> Element<'a, Message> {
    let cols = cols.max(1);
    let mut col = Column::new().spacing(MOSAIC_GAP).width(Fill);
    let mut row_tiles: Vec<Element<'a, Message>> = Vec::new();
    for tile in tiles {
        row_tiles.push(tile);
        if row_tiles.len() == cols {
            let mut r = Row::new().spacing(MOSAIC_GAP).width(Fill);
            for t in row_tiles.drain(..) {
                r = r.push(t);
            }
            // Stretch leftover space so the row fills the content pane.
            r = r.push(Space::new().width(Fill));
            col = col.push(r);
        }
    }
    if !row_tiles.is_empty() {
        let mut r = Row::new().spacing(MOSAIC_GAP).width(Fill);
        for t in row_tiles {
            r = r.push(t);
        }
        r = r.push(Space::new().width(Fill));
        col = col.push(r);
    }
    col.into()
}

/// Accent color swatch for settings.
pub fn accent_swatch(
    ui: UiTheme,
    preset: fluxplay_core::models::AccentPreset,
    active: bool,
) -> Element<'static, Message> {
    let sample = UiTheme::new(ui.day, preset).accent();
    button(
        container(Space::new().width(28).height(28))
            .width(Length::Fixed(36.0))
            .height(Length::Fixed(36.0))
            .center_x(Fill)
            .center_y(Fill)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(sample)),
                border: Border {
                    color: if active {
                        ui.ink()
                    } else {
                        ui.outline()
                    },
                    width: if active { 2.5 } else { 1.0 },
                    radius: RADIUS_FULL.into(),
                },
                ..Default::default()
            }),
    )
    .on_press(Message::SetAccent(preset))
    .padding(4)
    .style(move |theme: &Theme, status| {
        let mut s = button::text(theme, status);
        s.border.radius = RADIUS_FULL.into();
        if matches!(status, button::Status::Hovered) {
            s.background = Some(Background::Color(ui.surface_muted()));
        }
        s
    })
    .into()
}
