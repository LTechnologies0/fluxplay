//! Media-center chrome: dense panes, capped lists, reliable selection.
//! Inspired by TiviMate / MYTV / IPTVnator / Smarters + Material 3 Expressive.
//!
//! Pure view helpers — operational logs live in `main` Message handlers.
//! GLES rule: never put nav labels inside styled `button` / heavy container chrome.

use iced::widget::{
    button, column, container, image, mouse_area, row, scrollable, text, text_input, Column, Row,
    Space,
};
use iced::widget::image::Handle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Theme,
};

use crate::theme::{
    elevation_shadow, radius_fab, radius_list_item, radius_nav_pill, radius_poster, UiTheme,
    MOSAIC_GAP, CARD_RADIUS, RADIUS_FULL, RADIUS_LG, RADIUS_MD, RADIUS_XL, RADIUS_XXL, SPACE_MD,
    SPACE_SM, SPACE_XL, SPACE_XS, SPACE_XXS, TYPE_DISPLAY_S, TYPE_HEADLINE_S, TYPE_BODY_M,
    TYPE_LABEL_L, TYPE_LABEL_M, TYPE_TITLE_M,
};
use crate::app::Message;

pub const LIST_PAGE: usize = 24;
pub const CAT_PAGE: usize = 60;
/// Hard cap — LoadMore must not grow an unbounded iced widget tree.
pub const LIST_MAX: usize = 96;
/// Episodes shown per series detail page before “Afficher plus”.
pub const EPISODE_PAGE: usize = 40;

pub fn shell_background(ui: UiTheme) -> Color {
    // Body canvas = surfaceDim; navigation panes use surfaceContainer separately.
    ui.surface_dim()
}

/// Thin, muted scrollbar — media-center feel (not OS-fat defaults).
pub fn soft_scroll<'a>(
    ui: UiTheme,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    scrollable(body)
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(8)
                .scroller_width(8)
                .margin(2),
        ))
        .style(move |theme: &Theme, status| {
            let mut s = scrollable::default(theme, status);
            let rail = Background::Color(ui.surface_muted());
            let thumb = Background::Color(Color::from_rgba(
                ui.accent().r,
                ui.accent().g,
                ui.accent().b,
                if ui.day { 0.35 } else { 0.45 },
            ));
            s.vertical_rail.background = Some(rail);
            s.vertical_rail.border.radius = RADIUS_FULL.into();
            s.vertical_rail.scroller.background = thumb;
            s.vertical_rail.scroller.border.radius = RADIUS_FULL.into();
            s.horizontal_rail.background = Some(rail);
            s.horizontal_rail.scroller.background = thumb;
            s
        })
        .width(Fill)
        .height(Fill)
        .into()
}

/// Scroll when needed, but do not steal leftover column height (avoids empty “grey bar”).
pub fn soft_scroll_fit<'a>(
    ui: UiTheme,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    scrollable(body)
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(8)
                .scroller_width(8)
                .margin(2),
        ))
        .style(move |theme: &Theme, status| {
            let mut s = scrollable::default(theme, status);
            let rail = Background::Color(ui.surface_muted());
            let thumb = Background::Color(Color::from_rgba(
                ui.accent().r,
                ui.accent().g,
                ui.accent().b,
                if ui.day { 0.35 } else { 0.45 },
            ));
            s.vertical_rail.background = Some(rail);
            s.vertical_rail.border.radius = RADIUS_FULL.into();
            s.vertical_rail.scroller.background = thumb;
            s.vertical_rail.scroller.border.radius = RADIUS_FULL.into();
            s.horizontal_rail.background = Some(rail);
            s.horizontal_rail.scroller.background = thumb;
            s
        })
        .width(Fill)
        .into()
}

pub fn pane<'a>(
    ui: UiTheme,
    width: Length,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    pane_sized(ui, width, Fill, body)
}

pub fn pane_sized<'a>(
    ui: UiTheme,
    width: Length,
    height: Length,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    // Elev 0 tonal panes; soft elev 1 only on desktop (Android: flat).
    let shadow = elevation_shadow(0, ui.day);
    let radius = if cfg!(target_os = "android") {
        RADIUS_LG
    } else {
        RADIUS_XXL
    };
    #[cfg(target_os = "android")]
    let pad = SPACE_SM as u16;
    #[cfg(not(target_os = "android"))]
    let pad = SPACE_MD as u16;

    container(body)
        .width(width)
        .height(height)
        .padding(pad)
        .align_x(Alignment::Start)
        .align_y(Alignment::Start)
        .clip(true)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: radius.into(),
            },
            shadow,
            ..Default::default()
        })
        .into()
}

/// Navigation region — always `surfaceContainer` (M3 pairing, stable breakpoints).
pub fn nav_pane_sized<'a>(
    ui: UiTheme,
    width: Length,
    height: Length,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let radius = if cfg!(target_os = "android") {
        RADIUS_LG
    } else {
        RADIUS_XXL
    };
    let pad = SPACE_SM as u16;
    container(body)
        .width(width)
        .height(height)
        .padding(pad)
        .align_x(Alignment::Start)
        .align_y(Alignment::Start)
        .clip(true)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: radius.into(),
            },
            shadow: elevation_shadow(0, ui.day),
            ..Default::default()
        })
        .into()
}

pub fn mode_rail<'a>(
    ui: UiTheme,
    width: f32,
    label_size: f32,
    items: impl IntoIterator<Item = (crate::icons::Icon, &'a str, Message, bool)>,
) -> Element<'a, Message> {
    let mut nav = Column::new().spacing(SPACE_XS).width(Fill);
    // Brand: displaySmall emphasized (scaled to rail).
    let brand_size = (label_size + 10.0).min(TYPE_DISPLAY_S * 0.55).max(label_size + 6.0);
    nav = nav.push(
        column![
            text("FluxPlay")
                .size(brand_size)
                .color(ui.primary()),
            text("Media center")
                .size(TYPE_LABEL_M)
                .color(ui.on_surface_variant()),
        ]
        .spacing(SPACE_XXS)
        .padding(Padding::from([4, 8])),
    );
    nav = nav.push(Space::new().height(SPACE_MD));
    for (ic, label, msg, active) in items {
        // Active: secondaryContainer fill + onSecondaryContainer ink (readable contrast).
        let fg = if active {
            ui.on_secondary_container()
        } else {
            ui.on_surface_variant()
        };
        let bg = if active {
            ui.secondary_container()
        } else {
            Color::TRANSPARENT
        };
        let indicator = container(Space::new().width(4.0).height(label_size + 14.0))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(if active {
                    ui.secondary()
                } else {
                    Color::TRANSPARENT
                })),
                border: Border {
                    radius: radius_nav_pill(),
                    ..Default::default()
                },
                ..Default::default()
            });
        let hit = container(
            row![
                indicator,
                Space::new().width(SPACE_SM),
                crate::icons::icon(ic, 22.0, fg),
                Space::new().width(SPACE_SM),
                text(label)
                    .size(if active { label_size + 1.0 } else { label_size })
                    .color(fg)
                    .width(Fill),
            ]
            .align_y(Alignment::Center)
            .width(Fill)
            .padding(Padding::from([12, 10])),
        )
        .width(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                radius: RADIUS_XL.into(),
                ..Default::default()
            },
            ..Default::default()
        });
        nav = nav.push(mouse_area(hit).on_press(msg));
    }
    nav_pane_sized(ui, Length::Fixed(width.max(100.0)), Fill, nav)
}

/// Phone / narrow: compact two-column tab grid (Row+mouse_area drops siblings on GLES).
pub fn mode_top_nav<'a>(
    ui: UiTheme,
    label_size: f32,
    items: impl IntoIterator<Item = (crate::icons::Icon, &'a str, Message, bool)>,
) -> Element<'a, Message> {
    mode_top_nav_ex(ui, label_size, false, items)
}

/// Phone / narrow: M3 NavigationBar — equal destinations with active indicator.
pub fn mode_top_nav_ex<'a>(
    ui: UiTheme,
    label_size: f32,
    landscape_strip: bool,
    items: impl IntoIterator<Item = (crate::icons::Icon, &'a str, Message, bool)>,
) -> Element<'a, Message> {
    let collected: Vec<_> = items.into_iter().collect();
    // Brand row above the bar (M3: brand stays visible, destinations in bar).
    let brand = text("FluxPlay")
        .size(if landscape_strip {
            label_size + 2.0
        } else {
            (label_size + 8.0).min(TYPE_HEADLINE_S)
        })
        .color(ui.primary());

    let mut bar = Row::new().spacing(SPACE_XXS).align_y(Alignment::Center).width(Fill);
    for (ic, label, msg, active) in collected {
        let fg = if active {
            ui.on_secondary_container()
        } else {
            ui.on_surface_variant()
        };
        let pill_bg = if active {
            ui.secondary_container()
        } else {
            Color::TRANSPARENT
        };
        let indicator = container(Space::new().width(if active { 28.0 } else { 0.0 }).height(4.0))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(if active {
                    ui.secondary()
                } else {
                    Color::TRANSPARENT
                })),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        let cell = mouse_area(
            container(
                column![
                    indicator,
                    Space::new().height(4.0),
                    crate::icons::icon(ic, 20.0, fg),
                    text(label)
                        .size(if active {
                            label_size + 0.5
                        } else {
                            label_size
                        })
                        .color(fg),
                ]
                .spacing(2)
                .align_x(Alignment::Center)
                .width(Fill),
            )
            .padding(Padding::from([8, 4]))
            .width(Fill)
            .center_x(Fill)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(pill_bg)),
                border: Border {
                    radius: RADIUS_XL.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .on_press(msg);
        bar = bar.push(cell);
    }

    let chrome = container(bar)
        .padding(Padding::from([6, 8]))
        .width(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: RADIUS_XXL.into(),
            },
            ..Default::default()
        });

    column![brand, chrome]
        .spacing(SPACE_SM)
        .width(Fill)
        .padding(Padding::from([2, 0]))
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
        .padding(SPACE_SM as u16)
        .size(13)
        .style(move |theme: &Theme, status| {
            let mut s = text_input::default(theme, status);
            s.border.radius = RADIUS_FULL.into(); // SearchBar / FilterChip field
            s.border.color = ui.outline_variant();
            s.background = Background::Color(ui.surface_container_low());
            s
        });

    let mut list = Column::new().spacing(SPACE_XXS).width(Fill);
    if entries.is_empty() {
        list = list.push(text("Aucune catégorie").size(12).color(ui.ink_muted()));
    }
    for (id, name, active, count) in entries {
        let label = if count > 0 {
            format!("{name}  · {count}")
        } else {
            name
        };
        list = list.push(cat_row(
            label.replace(';', " · "),
            Message::SelectBrowseCategory(id),
            ui,
            active,
        ));
    }

    pane(
        ui,
        Length::Fixed(width.max(120.0)),
        column![
            text(title).size(13).color(ui.ink_muted()),
            filter_input,
            soft_scroll(ui, list.width(Fill)),
        ]
        .spacing(SPACE_SM)
        .width(Fill)
        .height(Fill),
    )
}

/// Compact category chips when the sidebar is collapsed (phone).
pub fn category_chips<'a>(
    ui: UiTheme,
    filter: &str,
    entries: Vec<(String, String, bool, usize)>,
) -> Element<'a, Message> {
    let filter_input = text_input("Filtrer…", filter)
        .on_input(Message::CatFilterChanged)
        .padding(SPACE_SM as u16)
        .size(13)
        .width(Length::Fixed(112.0))
        .style(move |theme: &Theme, status| {
            let mut s = text_input::default(theme, status);
            s.border.radius = RADIUS_FULL.into();
            s.border.color = ui.outline_variant();
            s.background = Background::Color(ui.surface_container_low());
            s
        });

    let mut chips = Row::new().spacing(SPACE_SM).align_y(Alignment::Center);
    chips = chips.push(filter_input);
    for (id, name, active, count) in entries {
        let label = if count > 0 {
            format!("{} · {count}", name.replace(';', " · "))
        } else {
            name.replace(';', " · ")
        };
        // M3 FilterChip: selected = secondaryContainer + squircle; unselected = full pill.
        let fg = if active {
            ui.on_secondary_container()
        } else {
            ui.on_surface()
        };
        let bg = if active {
            ui.secondary_container()
        } else {
            ui.surface_container_low()
        };
        let radius = if active { RADIUS_MD } else { RADIUS_FULL };
        let chip = container(text(label).size(TYPE_LABEL_L).color(fg))
            .padding(Padding::from([10, 14]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    color: if active {
                        Color::TRANSPARENT
                    } else {
                        ui.outline_variant()
                    },
                    width: if active { 0.0 } else { 1.0 },
                    radius: radius.into(),
                },
                ..Default::default()
            });
        chips = chips.push(mouse_area(chip).on_press(Message::SelectBrowseCategory(id)));
    }

    scrollable(chips)
        .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::hidden()))
        .width(Fill)
        .height(Length::Shrink)
        .into()
}

fn cat_row(label: String, msg: Message, ui: UiTheme, active: bool) -> Element<'static, Message> {
    let fg = if active {
        ui.on_secondary_container()
    } else {
        ui.on_surface()
    };
    let bg = if active {
        ui.secondary_container()
    } else {
        Color::TRANSPARENT
    };
    let bar = container(Space::new().width(if active { 4.0 } else { 0.0 }).height(22.0))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(if active {
                ui.secondary()
            } else {
                Color::TRANSPARENT
            })),
            border: Border {
                radius: radius_nav_pill(),
                ..Default::default()
            },
            ..Default::default()
        });
    mouse_area(
        container(
            row![
                bar,
                Space::new().width(SPACE_SM),
                text(label)
                    .size(if active { TYPE_LABEL_L + 1.0 } else { TYPE_LABEL_L })
                    .color(fg)
                    .width(Fill),
            ]
            .align_y(Alignment::Center)
            .width(Fill)
            .padding(Padding::from([12, 10])),
        )
        .width(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                radius: if active {
                    RADIUS_LG.into()
                } else {
                    RADIUS_XL.into()
                },
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .on_press(msg)
    .into()
}

pub fn content_header<'a>(
    ui: UiTheme,
    title: String,
    subtitle: String,
    search: &str,
    search_width: f32,
    title_size: f32,
    stack: bool,
) -> Element<'a, Message> {
    let titles = column![
        text(title).size(title_size.max(TYPE_TITLE_M)).color(ui.on_surface()),
        text(subtitle)
            .size(TYPE_LABEL_M)
            .color(ui.on_surface_variant()),
    ]
    .spacing(SPACE_XXS)
    .width(Fill);

    let search_el = text_input("Rechercher…", search)
        .on_input(Message::SearchChanged)
        .padding(if stack { 12 } else { 14 })
        .size(TYPE_BODY_M)
        .width(if stack {
            Fill
        } else {
            Length::Fixed(search_width.clamp(100.0, 720.0))
        })
        .style(move |theme: &Theme, status| {
            let mut s = text_input::default(theme, status);
            s.border.radius = RADIUS_FULL.into();
            s.border.color = if matches!(status, text_input::Status::Focused { .. }) {
                ui.outline()
            } else {
                ui.outline_variant()
            };
            s.background = Background::Color(ui.surface_container_highest());
            s
        });

    if stack {
        column![titles, search_el]
            .spacing(SPACE_SM)
            .width(Fill)
            .into()
    } else {
        row![titles, search_el]
            .spacing(SPACE_MD)
            .align_y(Alignment::Center)
            .width(Fill)
            .into()
    }
}

pub fn media_row<'a>(
    title: String,
    subtitle: String,
    on_open: Message,
    on_fav: Option<(bool, Message)>,
    ui: UiTheme,
    active: bool,
    thumb: Option<&'a Handle>,
    thumb_size: f32,
) -> Element<'a, Message> {
    let title_c = if active {
        ui.on_primary_container()
    } else {
        ui.on_surface()
    };
    let row_bg = if active {
        // Soft primaryContainer — full vibrant fill was overpowering on lists.
        let c = ui.primary_container();
        Color::from_rgba(c.r, c.g, c.b, if ui.day { 0.55 } else { 0.42 })
    } else {
        Color::TRANSPARENT
    };
    let ts = thumb_size.clamp(32.0, 72.0);

    let thumb_el: Element<'a, Message> = if let Some(handle) = thumb {
        container(
            image(handle)
                .width(Length::Fixed(ts))
                .height(Length::Fixed(ts))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Length::Fixed(ts))
        .height(Length::Fixed(ts))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_low())),
            border: Border {
                radius: CARD_RADIUS.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
    } else {
        let glyph = title
            .chars()
            .find(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .unwrap_or('#');
        container(
            text(glyph.to_string())
                .size((ts * 0.42).clamp(14.0, 28.0))
                .color(ui.on_surface_variant()),
        )
        .width(Length::Fixed(ts))
        .height(Length::Fixed(ts))
        .center_x(Fill)
        .center_y(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_highest())),
            border: Border {
                radius: CARD_RADIUS.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
    };

    let labels = column![
        text(title).size(TYPE_TITLE_M).color(title_c),
        text(subtitle).size(TYPE_LABEL_M).color(ui.on_surface_variant()),
    ]
    .spacing(SPACE_XXS)
    .width(Fill);

    let mark = container(Space::new().width(3).height(ts.max(28.0)))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(if active {
                ui.primary()
            } else {
                Color::TRANSPARENT
            })),
            border: Border {
                radius: RADIUS_FULL.into(),
                ..Default::default()
            },
            ..Default::default()
        });

    let open_hit = mouse_area(
        row![mark, Space::new().width(SPACE_SM), thumb_el, labels]
            .spacing(SPACE_SM)
            .align_y(Alignment::Center)
            .width(Fill)
            .padding(Padding::from([10, 8])),
    )
    .on_press(on_open);

    let row_body: Element<'a, Message> = if let Some((is_fav, fav_msg)) = on_fav {
        let star = if is_fav { "★" } else { "☆" };
        let fav_c = ui.tertiary();
        row![
            open_hit,
            mouse_area(text(star).size(TYPE_TITLE_M).color(fav_c)).on_press(fav_msg),
        ]
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
        .width(Fill)
        .into()
    } else {
        open_hit.into()
    };

    // Expressive list selection: soft primaryContainer + morph corners.
    container(row_body)
        .width(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(row_bg)),
            border: Border {
                radius: radius_list_item(active),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Registered source card — full-width list item with trailing icon actions (M3).
pub fn source_card<'a>(
    title: String,
    subtitle: String,
    on_open: Message,
    on_reload: Message,
    on_remove: Message,
    ui: UiTheme,
) -> Element<'a, Message> {
    use crate::icons::{self, Icon};

    let labels = column![
        text(title).size(TYPE_TITLE_M).color(ui.on_surface()),
        text(subtitle).size(TYPE_LABEL_M).color(ui.on_surface_variant()),
    ]
    .spacing(SPACE_XXS)
    .width(Fill);

    let open = mouse_area(
        row![
            icons::icon(Icon::Sources, 28.0, ui.primary()),
            Space::new().width(SPACE_MD),
            labels,
        ]
        .align_y(Alignment::Center)
        .width(Fill)
        .padding(Padding::from([4, 0])),
    )
    .on_press(on_open);

    let reload = mouse_area(
        container(icons::icon(Icon::Refresh, 22.0, ui.on_surface_variant()))
            .padding(10)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container_highest())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
    )
    .on_press(on_reload);

    let remove = mouse_area(
        container(icons::icon(Icon::Delete, 22.0, ui.error()))
            .padding(10)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.error_container())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
    )
    .on_press(on_remove);

    container(
        row![open, reload, remove]
            .spacing(SPACE_SM)
            .align_y(Alignment::Center)
            .width(Fill)
            .padding(Padding::from([14, 16])),
    )
    .width(Fill)
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(ui.surface_container_high())),
        border: Border {
            radius: RADIUS_XL.into(),
            width: 1.0,
            color: ui.outline_variant(),
        },
        shadow: elevation_shadow(1, ui.day),
        ..Default::default()
    })
    .into()
}

pub fn empty_hint(ui: UiTheme, msg: impl Into<String>) -> Element<'static, Message> {
    container(
        column![
            text("∅").size(22).color(ui.ink_muted()),
            text(msg.into()).size(14).color(ui.ink_muted()),
        ]
        .spacing(SPACE_SM)
        .align_x(Alignment::Center),
    )
    .padding(SPACE_XL as u16)
    .width(Fill)
    .center_x(Fill)
    .into()
}

pub fn load_more_btn(ui: UiTheme, remaining: Option<usize>) -> Element<'static, Message> {
    // Outlined / tonal L pill — M3 Expressive “show more”.
    let label = match remaining {
        Some(n) if n > 0 => format!("Afficher plus (+{n})"),
        _ => "Afficher plus".into(),
    };
    button(text(label).size(TYPE_LABEL_L))
        .on_press(Message::LoadMore)
        .padding(Padding::from([16, 24]))
        .width(Fill)
        .style(move |theme: &Theme, status| {
            let mut s = button::secondary(theme, status);
            s.border.radius = RADIUS_FULL.into();
            s.border.color = ui.outline();
            s.border.width = 1.0;
            s.background = Some(Background::Color(Color::TRANSPARENT));
            s.text_color = ui.primary();
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
    let w = tile_w.max(96.0);
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
        .clip(true)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_high())),
            border: Border {
                radius: radius_poster(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            ..Default::default()
        })
        .into()
    } else {
        container(
            column![
                text("▣")
                    .size((w * 0.22).clamp(18.0, 36.0))
                    .color(ui.ink_muted()),
                text("Jaquette")
                    .size(11.0)
                    .color(ui.ink_muted()),
            ]
            .spacing(SPACE_XS)
            .align_x(Alignment::Center),
        )
        .width(Length::Fixed(w))
        .height(Length::Fixed(h))
        .center_x(Fill)
        .center_y(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_high())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: radius_poster(),
            },
            ..Default::default()
        })
        .into()
    };

    // Title/meta clipped to tile width — prevent mosaic text bleed into neighbors.
    let max_chars = ((w / 7.2).floor() as usize).clamp(10, 48);
    let title_el = text(truncate_ui(&title, max_chars))
        .size(TYPE_LABEL_L)
        .color(ui.on_surface())
        .width(Length::Fixed(w));
    let meta_el = text(truncate_ui(&meta_line, max_chars + 4))
        .size(TYPE_LABEL_M)
        .color(ui.on_surface_variant())
        .width(Length::Fixed(w));

    let body = container(
        column![poster, title_el, meta_el]
            .spacing(SPACE_SM)
            .width(Length::Fixed(w)),
    )
    .padding(Padding::from([0, 2]))
    .width(Length::Fixed(w))
    .clip(true);

    mouse_area(body).on_press(on_open).into()
}

fn truncate_ui(s: &str, max: usize) -> String {
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

/// Compact accent tile for the settings mosaic (fixed size — never Fill).
pub fn accent_swatch(
    ui: UiTheme,
    preset: fluxplay_core::models::AccentPreset,
    active: bool,
    tile: f32,
) -> Element<'static, Message> {
    let sample = UiTheme::new(ui.day, preset).accent();
    let side = tile.clamp(28.0, 56.0);
    let inner = (side - 8.0).max(18.0);
    // mouse_area + plain container — avoids GLES glyph/layout quirks on styled buttons.
    mouse_area(
        container(
            container(Space::new().width(inner).height(inner))
                .width(Length::Fixed(inner))
                .height(Length::Fixed(inner))
                .style(move |_t: &Theme| container::Style {
                    background: Some(Background::Color(sample)),
                    border: Border {
                        color: if active { ui.ink() } else { Color::TRANSPARENT },
                        width: if active { 2.5 } else { 0.0 },
                        radius: RADIUS_FULL.into(), // Expressive cookie/circle swatch
                    },
                    ..Default::default()
                }),
        )
        .width(Length::Fixed(side))
        .height(Length::Fixed(side))
        .center_x(Length::Fixed(side))
        .center_y(Length::Fixed(side))
        .padding(2)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(if active {
                ui.surface_elevated()
            } else {
                ui.surface_muted()
            })),
            border: Border {
                color: if active { ui.accent() } else { ui.outline_variant() },
                width: if active { 2.0 } else { 1.0 },
                radius: RADIUS_MD.into(),
            },
            ..Default::default()
        }),
    )
    .on_press(Message::SetAccent(preset))
    .into()
}

/// Accent mosaic — as many presets as possible in a dense grid.
pub fn accent_mosaic(
    ui: UiTheme,
    active: fluxplay_core::models::AccentPreset,
    content_w: f32,
) -> Element<'static, Message> {
    let gap = 8.0_f32;
    let tile = 40.0_f32;
    let cols = (((content_w.max(160.0) + gap) / (tile + gap)).floor() as usize).clamp(4, 12);
    let tiles: Vec<_> = fluxplay_core::models::AccentPreset::all()
        .iter()
        .copied()
        .map(|p| accent_swatch(ui, p, active == p, tile))
        .collect();
    mosaic_grid(tiles, cols)
}

/// Film / series detail: poster, synopsis, cast, primary play, optional episodes.
pub fn media_detail_page<'a>(
    ui: UiTheme,
    title: &str,
    poster: Option<&'a Handle>,
    year: Option<&str>,
    genre: Option<&str>,
    rating: Option<&str>,
    runtime: Option<&str>,
    rated: Option<&str>,
    plot: Option<&str>,
    actors: Option<&str>,
    director: Option<&str>,
    writer: Option<&str>,
    imdb_id: Option<&str>,
    imdb_query: String,
    play_label: Option<String>,
    play_msg: Option<Message>,
    back: Message,
    episodes: Option<Element<'a, Message>>,
    narrow: bool,
    loading_meta: bool,
) -> Element<'a, Message> {
    let poster_w = if narrow { 140.0 } else { 200.0 };
    let poster_h = poster_w * 1.5;

    let art: Element<'_, Message> = match poster {
        Some(h) => container(
            image(h)
                .width(Length::Fixed(poster_w))
                .height(Length::Fixed(poster_h))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Length::Fixed(poster_w))
        .height(Length::Fixed(poster_h))
        .clip(true)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_muted())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: radius_poster(),
            },
            ..Default::default()
        })
        .into(),
        None => container(
            text("No art")
                .size(13)
                .color(ui.ink_muted()),
        )
        .width(Length::Fixed(poster_w))
        .height(Length::Fixed(poster_h))
        .center_x(Length::Fixed(poster_w))
        .center_y(Length::Fixed(poster_h))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_high())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: radius_poster(),
            },
            ..Default::default()
        })
        .into(),
    };

    let mut meta_bits: Vec<String> = Vec::new();
    if let Some(y) = year.filter(|s| !s.is_empty()) {
        meta_bits.push(y.to_string());
    }
    if let Some(g) = genre.filter(|s| !s.is_empty()) {
        meta_bits.push(g.to_string());
    }
    if let Some(r) = rating.filter(|s| !s.is_empty()) {
        meta_bits.push(format!("★ {r}"));
    }
    if let Some(rt) = runtime.filter(|s| !s.is_empty()) {
        meta_bits.push(rt.to_string());
    }
    if let Some(rd) = rated.filter(|s| !s.is_empty()) {
        meta_bits.push(rd.to_string());
    }
    let meta_line = meta_bits.join(" · ");

    let mut info = Column::new().spacing(SPACE_SM).width(Fill);
    info = info.push(text(title.to_string()).size(if narrow { 20 } else { 26 }));
    if !meta_line.is_empty() {
        info = info.push(text(meta_line).size(13).color(ui.accent()));
    }
    if loading_meta {
        info = info.push(
            text("IMDb / OMDb…")
                .size(12)
                .color(ui.ink_muted()),
        );
    }

    let mut action_els: Vec<Element<'_, Message>> = Vec::new();
    if let (Some(label), Some(msg)) = (play_label, play_msg) {
        // SplitButton / FAB primary — large soft corner (radius_fab)
        action_els.push(
            button(text(label).size(15))
                .on_press(msg)
                .padding(Padding::from([14, 22]))
                .style(move |theme: &Theme, status| {
                    let mut s = button::primary(theme, status);
                    s.border.radius = radius_fab();
                    s
                })
                .into(),
        );
    }
    let imdb_label = imdb_id
        .filter(|s| !s.is_empty())
        .map(|id| format!("IMDb · {id}"))
        .unwrap_or_else(|| "IMDb".into());
    action_els.push(
        button(text(imdb_label).size(13))
            .on_press(Message::OpenImdb(imdb_query))
            .padding(Padding::from([12, 16]))
            .style(move |theme: &Theme, status| {
                let mut s = button::secondary(theme, status);
                s.border.radius = RADIUS_FULL.into();
                s.border.color = ui.outline_variant();
                s.background = Some(Background::Color(ui.secondary_container()));
                s.text_color = ui.accent();
                s
            })
            .into(),
    );
    info = info.push(Row::with_children(action_els).spacing(SPACE_SM).wrap());

    let hero: Element<'_, Message> = if narrow {
        column![art, info].spacing(SPACE_MD).width(Fill).into()
    } else {
        row![art, info]
            .spacing(SPACE_XL)
            .align_y(Alignment::Start)
            .width(Fill)
            .into()
    };

    let mut body = Column::new().spacing(SPACE_MD).width(Fill);
    body = body.push(hero);

    if let Some(p) = plot.filter(|s| !s.trim().is_empty()) {
        body = body.push(section_block(ui, "Synopsis", p));
    } else if loading_meta {
        body = body.push(section_block(
            ui,
            "Synopsis",
            "Chargement de la fiche (panel / IMDb)…",
        ));
    } else {
        body = body.push(section_block(
            ui,
            "Synopsis",
            "Synopsis indisponible pour ce titre.",
        ));
    }
    if let Some(a) = actors.filter(|s| !s.trim().is_empty()) {
        body = body.push(section_block(ui, "Acteurs", a));
    } else if loading_meta {
        body = body.push(section_block(ui, "Acteurs", "Chargement…"));
    }
    let mut crew: Vec<String> = Vec::new();
    if let Some(d) = director.filter(|s| !s.trim().is_empty()) {
        crew.push(format!("Réalisation · {d}"));
    }
    if let Some(w) = writer.filter(|s| !s.trim().is_empty()) {
        crew.push(format!("Scénario · {w}"));
    }
    if !crew.is_empty() {
        body = body.push(section_block(ui, "Équipe", &crew.join("\n")));
    }

    if let Some(eps) = episodes {
        body = body.push(text("Épisodes").size(16));
        body = body.push(eps);
    }

    let back = button(text("← Catalogue").size(13))
        .on_press(back)
        .padding(Padding::from([10, 16]))
        .style(move |theme: &Theme, status| {
            let mut s = button::secondary(theme, status);
            s.border.radius = RADIUS_FULL.into();
            s.border.color = ui.outline_variant();
            s.background = Some(Background::Color(ui.surface_container_low()));
            s.text_color = ui.ink();
            s
        });

    // Header outside the scroll so the page always has a visible chrome
    // (avoids a zero-height scroll-only layout on some window sizes).
    pane(
        ui,
        Length::Fill,
        column![
            back,
            soft_scroll(ui, body.padding(Padding::from([0, 4])).width(Fill))
        ]
        .spacing(SPACE_MD)
        .width(Fill)
        .height(Fill),
    )
}

fn section_block<'a>(ui: UiTheme, heading: &str, body: &str) -> Element<'a, Message> {
    column![
        text(heading.to_string()).size(16),
        text(body.to_string()).size(13).color(ui.ink_muted()),
    ]
    .spacing(SPACE_XS)
    .width(Fill)
    .into()
}
