//! Media-center chrome: dense panes, capped lists, reliable selection.

use iced::widget::{
    button, column, container, image, row, scrollable, text, text_input, Column, Row, Space,
};
use iced::widget::image::Handle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Shadow, Theme,
};

use crate::theme::{
    accent, ink_muted, on_primary, outline, surface, surface_elevated, surface_muted, RADIUS_LG,
    RADIUS_MD, RADIUS_SM,
};
use crate::Message;

pub const LIST_PAGE: usize = 48;
pub const CAT_PAGE: usize = 80;

pub fn shell_background(day: bool) -> Color {
    if day {
        Color::from_rgb8(0xE6, 0xEC, 0xF2)
    } else {
        Color::from_rgb8(0x08, 0x0D, 0x14)
    }
}

pub fn pane<'a>(
    day: bool,
    width: Length,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    container(body)
        .width(width)
        .height(Fill)
        .padding(10)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(surface(day))),
            border: Border {
                color: outline(day),
                width: 1.0,
                radius: RADIUS_LG.into(),
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, if day { 0.04 } else { 0.25 }),
                offset: iced::Vector::new(0.0, 4.0),
                blur_radius: 16.0,
            },
            ..Default::default()
        })
        .into()
}

pub fn mode_rail<'a>(
    day: bool,
    items: impl IntoIterator<Item = (&'a str, Message, bool)>,
) -> Element<'a, Message> {
    let mut col = Column::new().spacing(4).width(Fill);
    col = col.push(text("FluxPlay").size(16).color(accent(day)));
    col = col.push(Space::new().height(8));
    for (label, msg, active) in items {
        col = col.push(rail_btn(label, msg, day, active));
    }
    pane(day, Length::Fixed(112.0), col)
}

fn rail_btn<'a>(label: &'a str, msg: Message, day: bool, active: bool) -> Element<'a, Message> {
    let fg = if active {
        on_primary(day)
    } else if day {
        Color::from_rgb8(0x12, 0x1A, 0x24)
    } else {
        Color::from_rgb8(0xE8, 0xEE, 0xF5)
    };
    button(text(label).size(13).color(fg))
        .on_press(msg)
        .padding(Padding::from([10, 12]))
        .width(Fill)
        .style(move |theme: &Theme, status| {
            let mut s = if active {
                button::primary(theme, status)
            } else {
                button::text(theme, status)
            };
            s.border.radius = RADIUS_MD.into();
            if active {
                s.background = Some(Background::Color(accent(day)));
                s.text_color = on_primary(day);
            } else if matches!(status, button::Status::Hovered) {
                s.background = Some(Background::Color(surface_muted(day)));
            }
            s
        })
        .into()
}

pub fn category_sidebar<'a>(
    day: bool,
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
            s.background = Background::Color(surface_muted(day));
            s
        });

    let mut list = Column::new().spacing(2).width(Fill);
    if entries.is_empty() {
        list = list.push(text("Aucune catégorie").size(12).color(ink_muted(day)));
    }
    for (id, name, active, count) in entries {
        let label = if count > 0 {
            format!("{name}  · {count}")
        } else {
            name
        };
        list = list.push(cat_row(label, Message::SelectBrowseCategory(id), day, active));
    }

    pane(
        day,
        Length::Fixed(240.0),
        column![
            text(title).size(14).color(ink_muted(day)),
            filter_input,
            scrollable(list).height(Fill),
        ]
        .spacing(8)
        .height(Fill),
    )
}

fn cat_row(label: String, msg: Message, day: bool, active: bool) -> Element<'static, Message> {
    let fg = if active {
        accent(day)
    } else if day {
        Color::from_rgb8(0x12, 0x1A, 0x24)
    } else {
        Color::from_rgb8(0xE8, 0xEE, 0xF5)
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
            s.background = Some(Background::Color(surface_elevated(day)));
            s.border.width = 1.0;
            s.border.color = accent(day);
        } else if matches!(status, button::Status::Hovered) {
            s.background = Some(Background::Color(surface_muted(day)));
        }
        s
    })
    .into()
}

pub fn content_header<'a>(
    day: bool,
    title: String,
    subtitle: String,
    search: &str,
) -> Element<'a, Message> {
    row![
        column![
            text(title).size(22),
            text(subtitle).size(12).color(ink_muted(day)),
        ]
        .spacing(2)
        .width(Fill),
        text_input("Rechercher…", search)
            .on_input(Message::SearchChanged)
            .padding(12)
            .size(14)
            .width(Length::Fixed(280.0))
            .style(move |theme: &Theme, status| {
                let mut s = text_input::default(theme, status);
                s.border.radius = RADIUS_MD.into();
                s.background = Background::Color(surface_muted(day));
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
    day: bool,
    active: bool,
    thumb: Option<&'a Handle>,
) -> Element<'a, Message> {
    let title_c = if active {
        accent(day)
    } else if day {
        Color::from_rgb8(0x12, 0x1A, 0x24)
    } else {
        Color::WHITE
    };

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
            background: Some(Background::Color(surface_muted(day))),
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
                background: Some(Background::Color(surface_muted(day))),
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
        text(subtitle).size(11).color(ink_muted(day)),
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
                    surface_elevated(day)
                } else {
                    Color::TRANSPARENT
                };
                if matches!(status, button::Status::Hovered) {
                    bg = surface_muted(day);
                }
                s.background = Some(Background::Color(bg));
                if active {
                    s.border.width = 1.5;
                    s.border.color = accent(day);
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
                    s.background = Some(Background::Color(surface_muted(day)));
                    s.text_color = if is_fav {
                        Color::from_rgb8(0xF5, 0xBF, 0x2A)
                    } else {
                        ink_muted(day)
                    };
                    s
                }),
        );
    }
    row.into()
}

pub fn empty_hint(day: bool, msg: impl Into<String>) -> Element<'static, Message> {
    container(
        text(msg.into())
            .size(14)
            .color(ink_muted(day)),
    )
    .padding(24)
    .width(Fill)
    .center_x(Fill)
    .into()
}

pub fn load_more_btn(day: bool, remaining: usize) -> Element<'static, Message> {
    button(text(format!("Afficher plus (+{remaining})")).size(13))
        .on_press(Message::LoadMore)
        .padding(12)
        .width(Fill)
        .style(move |theme: &Theme, status| {
            let mut s = button::secondary(theme, status);
            s.border.radius = RADIUS_MD.into();
            s.background = Some(Background::Color(surface_muted(day)));
            s.text_color = accent(day);
            s
        })
        .into()
}
