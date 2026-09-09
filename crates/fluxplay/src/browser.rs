//! Media-center chrome: dense panes, capped lists, reliable selection.
//! Inspired by TiviMate / MYTV / IPTVnator / Smarters + Material 3 Expressive.
//!
//! Pure view helpers — operational logs live in `main` Message handlers.
//! GLES rule: never put nav labels inside styled `button` / heavy container chrome.

use iced::widget::{
    column, container, image, mouse_area, row, scrollable, stack, text, text_input, Column, Row,
    Space,
};
use iced::widget::scrollable::Viewport;
use iced::widget::image::Handle;
use iced::{
    mouse, Alignment, Background, Border, Color, Element, Fill, Length, Padding, Theme,
};
use std::borrow::Cow;

use crate::theme::{
    elevation_shadow, radius_fab, radius_list_item, radius_nav_pill, radius_poster, type_font,
    type_style, TypeRole, UiTheme, MOSAIC_GAP, CARD_RADIUS, RADIUS_FULL, RADIUS_LARGE_INCREASED,
    RADIUS_LG, RADIUS_MD, RADIUS_XL, RADIUS_XXL, SPACE_MD, SPACE_SM, SPACE_XL, SPACE_XS, SPACE_XXS,
    TYPE_BODY_M, TYPE_LABEL_L, TYPE_LABEL_M, TYPE_TITLE_M,
};
use crate::app::Message;

pub const LIST_PAGE: usize = 48;
/// Sidebar categories (Live / VOD / Series) — show the full catalog list.
pub const CAT_PAGE: usize = 2000;
/// Iced breaks on multi‑million‑px scroll content. Cap the *layout* height and
/// map scrollbar position across the full item count (true virtual scroll).
/// Higher = less sensitive scrollbar on huge catalogs, but iced hates
/// multi‑million‑px layouts — keep a hard ceiling.
pub const MAX_VIRTUAL_CONTENT_PX: f32 = 480_000.0;
/// Target scroll travel per virtual row (px) before hitting the cap.
/// With the height ceiling this stays smooth up to ~1M mosaic rows.
const VIRTUAL_ROW_TRAVEL_PX: f32 = 8.0;
/// Rows rendered above/below the viewport (fast flings need deeper lead-in).
pub const VIRTUAL_OVERSCAN: usize = 14;

/// Indices into `bundle.channels` / `vod` / `series` for the active browse filter.
///
/// `Identity` avoids allocating a million-entry `Vec` for « All » + empty search —
/// browse position `i` maps straight to catalog index `i`.
#[derive(Debug, Clone, Default)]
pub enum BrowseIndex {
    #[default]
    Empty,
    /// Contiguous catalog `0..len` (VOD/Series « All », or Live-only bundles).
    Identity {
        len: usize,
    },
    /// Filtered / searched / favorites subset.
    Mapped(Vec<usize>),
}

impl BrowseIndex {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Identity { len } => *len,
            Self::Mapped(v) => v.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        *self = Self::Empty;
    }

    pub fn identity(len: usize) -> Self {
        if len == 0 {
            Self::Empty
        } else {
            Self::Identity { len }
        }
    }

    pub fn mapped(v: Vec<usize>) -> Self {
        if v.is_empty() {
            Self::Empty
        } else {
            Self::Mapped(v)
        }
    }

    /// Catalog index for browse position, or `None` if out of range.
    #[inline]
    pub fn get(&self, browse_pos: usize) -> Option<usize> {
        match self {
            Self::Empty => None,
            Self::Identity { len } => (browse_pos < *len).then_some(browse_pos),
            Self::Mapped(v) => v.get(browse_pos).copied(),
        }
    }

    /// Catalog indices covering `[start, end)` browse positions (viewport-sized).
    pub fn window(&self, start: usize, end: usize) -> Vec<usize> {
        let end = end.min(self.len());
        let start = start.min(end);
        match self {
            Self::Empty => Vec::new(),
            Self::Identity { .. } => (start..end).collect(),
            Self::Mapped(v) => v[start..end].to_vec(),
        }
    }
}
/// Episodes listed via the same virtual window.
pub const EPISODE_PAGE: usize = 10_000;

pub fn shell_background(ui: UiTheme) -> Color {
    // Body canvas = surfaceDim; navigation panes use surfaceContainer separately.
    ui.surface_dim()
}

fn soft_scroll_style(ui: UiTheme) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style {
    move |theme: &Theme, status| {
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
    }
}

/// Thin, muted scrollbar — media-center feel (not OS-fat defaults).
pub fn soft_scroll<'a>(
    ui: UiTheme,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    #[cfg(target_os = "android")]
    let (rail, scroller) = (12.0_f32, 12.0_f32);
    #[cfg(not(target_os = "android"))]
    let (rail, scroller) = (8.0_f32, 8.0_f32);
    scrollable(body)
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(rail)
                .scroller_width(scroller)
                .margin(2),
        ))
        .style(soft_scroll_style(ui))
        .width(Fill)
        .height(Fill)
        .into()
}

/// Like [`soft_scroll`], but reports viewport Y / height for virtualized lists.
/// `scroll_id` must stay stable across rebuilds so iced keeps the scroll offset
/// (otherwise spacers desync and the mosaic looks empty).
pub fn soft_scroll_on<'a>(
    ui: UiTheme,
    body: impl Into<Element<'a, Message>>,
    scroll_id: impl Into<iced::widget::Id>,
    on_scroll: impl Fn(f32, f32) -> Message + 'a,
) -> Element<'a, Message> {
    // Phone / GLES: thicker scroller for thumb-driven flings.
    #[cfg(target_os = "android")]
    let (rail, scroller) = (12.0_f32, 12.0_f32);
    #[cfg(not(target_os = "android"))]
    let (rail, scroller) = (8.0_f32, 8.0_f32);
    scrollable(body)
        .id(scroll_id)
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(rail)
                .scroller_width(scroller)
                .margin(2),
        ))
        .on_scroll(move |vp: Viewport| {
            let y = vp.absolute_offset().y.max(0.0);
            let h = vp.bounds().height.max(1.0);
            on_scroll(y, h)
        })
        .style(soft_scroll_style(ui))
        .width(Fill)
        .height(Fill)
        .into()
}

/// Visible slice of a virtual list / mosaic (start inclusive, end exclusive).
#[derive(Debug, Clone, Copy)]
pub struct VirtualSlice {
    pub start: usize,
    pub end: usize,
    pub top_spacer: f32,
    pub bottom_spacer: f32,
}

/// Which items to mount given scroll position and fixed item stride.
///
/// For huge catalogs the natural content height (`total * item_h`) is compressed
/// to [`MAX_VIRTUAL_CONTENT_PX`] so iced stays healthy, while the scrollbar still
/// spans the full list via proportional row mapping.
pub fn virtual_slice(
    scroll_y: f32,
    view_h: f32,
    item_h: f32,
    total: usize,
    overscan: usize,
) -> VirtualSlice {
    if total == 0 || item_h <= 1.0 {
        return VirtualSlice {
            start: 0,
            end: 0,
            top_spacer: 0.0,
            bottom_spacer: 0.0,
        };
    }
    let view_h = view_h.max(item_h);
    let natural = total as f32 * item_h;
    let visible = ((view_h / item_h).ceil() as usize)
        .saturating_add(overscan.saturating_mul(2))
        .max(1);

    if natural <= MAX_VIRTUAL_CONTENT_PX {
        let max_y = (natural - view_h * 0.25).max(0.0);
        let scroll_y = scroll_y.clamp(0.0, max_y);
        let first = ((scroll_y / item_h).floor() as isize - overscan as isize).max(0) as usize;
        let end = (first + visible).min(total);
        let start = first.min(end);
        let (start, end) = if start >= end && total > 0 {
            (0, visible.min(total))
        } else {
            (start, end)
        };
        return VirtualSlice {
            start,
            end,
            top_spacer: start as f32 * item_h,
            bottom_spacer: (total.saturating_sub(end)) as f32 * item_h,
        };
    }

    // Compressed mode: keep layout height == content_h always
    // (spacers must absorb the difference vs real item_h — otherwise scroll
    // offset desyncs and fast flings show blank / jumpy mosaics).
    let content_h = (total as f32 * VIRTUAL_ROW_TRAVEL_PX)
        .clamp(view_h * 4.0, MAX_VIRTUAL_CONTENT_PX)
        .max(view_h + item_h);
    let max_scroll = (content_h - view_h).max(1.0);
    let scroll_y = scroll_y.clamp(0.0, max_scroll);
    let t = (scroll_y / max_scroll).clamp(0.0, 1.0);
    // Never let the mounted window eat the whole scroll range — otherwise
    // proportional spacers collapse and flings land in empty spacer zones.
    let max_mid = (content_h - view_h).max(view_h + item_h);
    let mut visible = visible;
    while (visible as f32) * item_h > max_mid && visible > 1 {
        visible -= 1;
    }
    let max_start = total.saturating_sub(visible.min(total));
    // f64 keeps start exact past ~16M when catalogs hit the million-title range.
    let start = ((f64::from(t) * max_start as f64).round() as usize).min(max_start);
    let end = (start + visible).min(total);
    let mid = (end.saturating_sub(start)) as f32 * item_h;
    let remain = (content_h - mid).max(0.0);
    let top_frac = if max_start == 0 {
        0.0
    } else {
        start as f32 / max_start as f32
    };
    let top_prop = (top_frac * remain).clamp(0.0, remain);
    // Clamp the tile band so it always covers the viewport. Pure proportional
    // spacers drift under fast flings when mid >> view_h or overscan changes
    // mid-scroll — leaving a black mosaic with the thumb still moving.
    let top_lo = (scroll_y + view_h - mid).clamp(0.0, remain);
    let top_hi = scroll_y.clamp(0.0, remain);
    let (lo, hi) = if top_lo <= top_hi {
        (top_lo, top_hi)
    } else {
        (top_hi, top_lo)
    };
    let top_spacer = top_prop.clamp(lo, hi);
    let bottom_spacer = (remain - top_spacer).max(0.0);
    VirtualSlice {
        start,
        end,
        top_spacer,
        bottom_spacer,
    }
}

#[cfg(test)]
mod virtual_scroll_tests {
    use super::*;

    #[test]
    fn compressed_layout_height_matches_content() {
        let total = 40_000usize;
        let item_h = 210.0;
        let view_h = 900.0;
        for y in [0.0, 50_000.0, 200_000.0, 800_000.0] {
            let s = virtual_slice(y, view_h, item_h, total, 10);
            let mid = (s.end - s.start) as f32 * item_h;
            let layout = s.top_spacer + mid + s.bottom_spacer;
            let content_h = (total as f32 * VIRTUAL_ROW_TRAVEL_PX)
                .clamp(view_h * 4.0, MAX_VIRTUAL_CONTENT_PX)
                .max(view_h + item_h);
            assert!(
                (layout - content_h).abs() < 1.0,
                "y={y} layout={layout} content_h={content_h} slice={s:?}"
            );
            assert!(s.end > s.start);
            assert!(s.end <= total);
        }
    }

    #[test]
    fn compressed_viewport_always_covers_tiles() {
        // Series All-scale catalog: fast fling must never park the viewport
        // inside pure spacer (black mosaic).
        let total = 8_766usize;
        let item_h = 250.0;
        let view_h = 900.0;
        let content_h = (total as f32 * VIRTUAL_ROW_TRAVEL_PX)
            .clamp(view_h * 4.0, MAX_VIRTUAL_CONTENT_PX)
            .max(view_h + item_h);
        let max_scroll = (content_h - view_h).max(1.0);
        for overscan in [6usize, 14, 28] {
            for i in 0..200 {
                let y = (i as f32 / 199.0) * max_scroll;
                let s = virtual_slice(y, view_h, item_h, total, overscan);
                let mid = (s.end - s.start) as f32 * item_h;
                let covers = y < s.top_spacer + mid && y + view_h > s.top_spacer;
                assert!(
                    covers,
                    "blank viewport y={y} overscan={overscan} slice={s:?} mid={mid}"
                );
            }
        }
    }

    #[test]
    fn million_row_mosaic_stays_viewport_sized() {
        // 1M titles @ 5 cols ≈ 200k mosaic rows — only a few dozen widgets mount.
        let total_rows = 1_000_000usize / 5;
        let item_h = 220.0;
        let view_h = 1080.0;
        let content_h = virtual_content_height(total_rows, view_h, item_h);
        assert!(content_h <= MAX_VIRTUAL_CONTENT_PX + 1.0);
        let max_scroll = (content_h - view_h).max(1.0);
        for i in 0..500 {
            let y = (i as f32 / 499.0) * max_scroll;
            let s = virtual_slice(y, view_h, item_h, total_rows, 16);
            let mounted = s.end.saturating_sub(s.start);
            assert!(mounted > 0 && mounted <= 64, "mounted={mounted} y={y} {s:?}");
            let mid = mounted as f32 * item_h;
            let covers = y < s.top_spacer + mid && y + view_h > s.top_spacer;
            assert!(covers, "blank @ y={y} {s:?}");
            let layout = s.top_spacer + mid + s.bottom_spacer;
            assert!((layout - content_h).abs() < 2.0);
        }
    }

    #[test]
    fn browse_index_identity_is_allocation_free() {
        let idx = BrowseIndex::identity(1_000_000);
        assert_eq!(idx.len(), 1_000_000);
        assert_eq!(idx.get(0), Some(0));
        assert_eq!(idx.get(999_999), Some(999_999));
        assert_eq!(idx.get(1_000_000), None);
        let win = idx.window(100, 108);
        assert_eq!(win, (100..108).collect::<Vec<_>>());
    }
}

/// Mosaic row stride (poster + title + meta + gap) for virtualization.
pub fn mosaic_row_height(tile_w: f32) -> f32 {
    let w = tile_w.max(96.0);
    let poster_h = (w * 1.5).round();
    poster_h + SPACE_SM + TYPE_LABEL_L + SPACE_SM + TYPE_LABEL_M + MOSAIC_GAP + 6.0
}

/// Compact live / episode / category row stride (includes inter-row gap).
pub fn list_row_height(thumb: f32) -> f32 {
    let th = if thumb > 1.0 { thumb } else { 44.0 };
    // Single-line row: thumb + vertical padding (touch-friendly ≥48).
    th.max(crate::theme::TOUCH_TARGET) + 16.0
}

/// Category sidebar row stride.
pub fn cat_row_height() -> f32 {
    36.0
}

/// Column with top/bottom spacers so only `items` are real widgets.
pub fn virtual_column<'a>(
    slice: VirtualSlice,
    items: impl IntoIterator<Item = Element<'a, Message>>,
) -> Column<'a, Message> {
    let mut col = Column::new().spacing(0).width(Fill);
    col = push_spacer_chunks(col, slice.top_spacer);
    for item in items {
        col = col.push(item);
    }
    col = push_spacer_chunks(col, slice.bottom_spacer);
    col
}

/// iced struggles with a single multi‑hundred‑kpx `Space` — chunk them.
pub fn push_spacer_chunks<'a>(
    mut col: Column<'a, Message>,
    height: f32,
) -> Column<'a, Message> {
    let mut left = height.max(0.0);
    while left > 0.5 {
        let chunk = left.min(24_000.0);
        col = col.push(
            Space::new()
                .height(Length::Fixed(chunk))
                .width(Fill),
        );
        left -= chunk;
    }
    col
}

/// Layout height for a virtualized list / mosaic (natural or compressed).
pub fn virtual_content_height(total: usize, view_h: f32, item_h: f32) -> f32 {
    let view_h = view_h.max(1.0);
    if total == 0 || item_h <= 1.0 {
        return view_h;
    }
    let natural = total as f32 * item_h;
    if natural <= MAX_VIRTUAL_CONTENT_PX {
        natural
    } else {
        (total as f32 * VIRTUAL_ROW_TRAVEL_PX)
            .clamp(view_h * 4.0, MAX_VIRTUAL_CONTENT_PX)
            .max(view_h + item_h)
    }
}

/// Stable spacer column (fixed chunk count) — keeps the scrollable widget tree
/// identity steady while the mosaic overlay swaps tiles.
fn stable_scroll_body<'a>(content_h: f32) -> Column<'a, Message> {
    const CHUNKS: usize = 18;
    let h = content_h.max(1.0);
    let each = h / CHUNKS as f32;
    let mut col = Column::new().spacing(0).width(Fill);
    for i in 0..CHUNKS {
        let chunk = if i + 1 == CHUNKS {
            (h - each * (CHUNKS as f32 - 1.0)).max(0.0)
        } else {
            each
        };
        col = col.push(Space::new().height(Length::Fixed(chunk)).width(Fill));
    }
    col
}

/// Virtual mosaic: scroll surface is a *stable* tall spacer; tiles sit in a
/// viewport-fixed overlay. Avoids the classic blank-frame bug where spacers
/// remount under iced's scroll offset during fast flings.
pub fn soft_scroll_mosaic<'a>(
    ui: UiTheme,
    scroll_id: impl Into<iced::widget::Id>,
    content_h: f32,
    mosaic: impl Into<Element<'a, Message>>,
    on_scroll: impl Fn(f32, f32) -> Message + 'a,
) -> Element<'a, Message> {
    #[cfg(target_os = "android")]
    let rail = 12.0_f32;
    #[cfg(not(target_os = "android"))]
    let rail = 8.0_f32;
    let rail_pad = rail + 4.0;

    let scroll = soft_scroll_on(ui, stable_scroll_body(content_h), scroll_id, on_scroll);

    // Overlay owns finger drag (BrowseDrag*); mosaic tiles / rows use on_release
    // so press reaches this mouse_area first (iced: content updates, then parent if
    // not captured). Tap vs scroll: app suppresses open once drag past ~12px slop.
    let overlay = mouse_area(
        row![
            mosaic.into(),
            Space::new().width(Length::Fixed(rail_pad)).height(Fill),
        ]
        .width(Fill)
        .height(Fill),
    )
    .on_scroll(|delta| {
        let dy = match delta {
            mouse::ScrollDelta::Lines { y, .. } => -y * 96.0,
            mouse::ScrollDelta::Pixels { y, .. } => -y,
        };
        Message::BrowseScrollBy(dy)
    })
    .on_press(Message::BrowseDragStart)
    .on_move(|p| Message::BrowseDragAt(p.y))
    .on_release(Message::BrowseDragEnd);

    stack![scroll, overlay]
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
    // Elev 1: Android tonal fill + outline; desktop surface + shadow.
    let (bg, mut border, shadow) = ui.elevated_style(1);
    let radius = if cfg!(target_os = "android") {
        RADIUS_LG
    } else {
        RADIUS_XXL
    };
    border.radius = radius.into();
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
            background: Some(Background::Color(bg)),
            border,
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
    // Brand: DisplayS (Bold) scaled to rail width.
    let (disp_base, _) = type_style(TypeRole::DisplayS, false);
    let brand_size = (label_size + 10.0).min(disp_base * 0.55).max(label_size + 6.0);
    nav = nav.push(
        column![
            text("FluxPlay")
                .size(brand_size)
                .font(type_font(true))
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
                radius: radius_list_item(active),
                ..Default::default()
            },
            ..Default::default()
        });
        nav = nav.push(mouse_area(hit).on_press(msg));
    }
    nav_pane_sized(ui, Length::Fixed(width.max(100.0)), Fill, nav)
}

/// Phone / narrow: M3 NavigationBar — equal destinations with active indicator.
///
/// On phone we use a **2-row grid** so 7 destinations keep readable labels and
/// ≥[`TOUCH_TARGET`] hit areas (a single row of 7 collapses labels on GLES).
///
/// GLES rules baked in:
/// - Android: **same chip chrome as category_chips** (proven to keep glyphs)
/// - Desktop: icon chrome + label outside the active pill
/// - every layer is `Shrink` / `Fixed` height — never `Fill` (active tab used to stretch)
pub fn mode_top_nav_ex<'a>(
    ui: UiTheme,
    label_size: f32,
    landscape_strip: bool,
    items: impl IntoIterator<Item = (crate::icons::Icon, &'a str, Message, bool)>,
) -> Element<'a, Message> {
    use crate::theme::TOUCH_TARGET;
    let collected: Vec<_> = items.into_iter().collect();

    #[cfg(target_os = "android")]
    {
        let lbl = label_size.max(10.0);
        // Icon above short label; taller chip for touch + glyph.
        let chip_h = TOUCH_TARGET + 12.0;
        let chip_for = |ic: crate::icons::Icon, label: &'a str, msg: Message, active: bool| {
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
            let chip = container(
                column![
                    crate::icons::icon(ic, 18.0, fg),
                    text(label)
                        .size(lbl)
                        .color(fg)
                        .width(Fill)
                        .align_x(Alignment::Center),
                ]
                .spacing(2)
                .align_x(Alignment::Center)
                .width(Fill),
            )
            .padding(Padding::from([4, 2]))
            .width(Fill)
            .height(Length::Fixed(chip_h))
            .center_x(Fill)
            .align_y(Alignment::Center)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    color: if active {
                        Color::TRANSPARENT
                    } else {
                        ui.outline_variant()
                    },
                    width: if active { 0.0 } else { 1.0 },
                    // XL not FULL: 7 equal cells are near-square → FULL became circles.
                    radius: RADIUS_XL.into(),
                },
                ..Default::default()
            });
            mouse_area(chip).on_press(msg)
        };

        if landscape_strip {
            // Landscape short: single equal-width NavigationBar strip.
            let strip_h = chip_h + 4.0;
            let mut row = Row::new()
                .spacing(SPACE_XXS)
                .align_y(Alignment::Center)
                .width(Fill)
                .height(Length::Fixed(strip_h));
            for (ic, label, msg, active) in collected {
                row = row.push(
                    container(chip_for(ic, label, msg, active))
                        .width(Length::FillPortion(1))
                        .height(Length::Fixed(strip_h))
                        .center_x(Fill)
                        .align_y(Alignment::Center),
                );
            }
            let nav_h = strip_h + 8.0;
            return container(row)
                .padding(Padding::from([4, 4]))
                .width(Fill)
                .height(Length::Fixed(nav_h))
                .clip(true)
                .style(move |_t: &Theme| container::Style {
                    background: Some(Background::Color(ui.surface_container())),
                    border: Border {
                        color: ui.outline_variant(),
                        width: 1.0,
                        radius: RADIUS_XXL.into(),
                    },
                    ..Default::default()
                })
                .into();
        }

        // Portrait / non-strip: same chip chrome, single equal-width row (current).
        let strip_h = chip_h + 4.0;
        let mut row = Row::new()
            .spacing(SPACE_XXS)
            .align_y(Alignment::Center)
            .width(Fill)
            .height(Length::Fixed(strip_h));
        for (ic, label, msg, active) in collected {
            row = row.push(
                container(chip_for(ic, label, msg, active))
                    .width(Length::FillPortion(1))
                    .height(Length::Fixed(strip_h))
                    .center_x(Fill)
                    .align_y(Alignment::Center),
            );
        }
        let nav_h = strip_h + 8.0;
        return container(row)
            .padding(Padding::from([4, 4]))
            .width(Fill)
            .height(Length::Fixed(nav_h))
            .clip(true)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container())),
                border: Border {
                    color: ui.outline_variant(),
                    width: 1.0,
                    radius: RADIUS_XXL.into(),
                },
                ..Default::default()
            })
            .into();
    }

    #[cfg(not(target_os = "android"))]
    {
    let cell_h = (TOUCH_TARGET + label_size + 10.0).clamp(64.0, 76.0);

    let cell = |ic: crate::icons::Icon, label: &'a str, msg: Message, active: bool| {
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
        let icon_chrome = container(crate::icons::icon(ic, 22.0, fg))
            .padding(Padding::from([6, 12]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(pill_bg)),
                border: Border {
                    radius: RADIUS_XL.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        mouse_area(
            container(
                column![
                    icon_chrome,
                    text(label).size(label_size.max(11.0)).color(fg),
                ]
                .spacing(4)
                .align_x(Alignment::Center)
                .width(Fill),
            )
            .padding(Padding::from([4, 2]))
            .width(Fill)
            .height(Length::Fixed(cell_h))
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
        )
        .on_press(msg)
    };

    let nav_chrome = move |_t: &Theme| container::Style {
        background: Some(Background::Color(ui.surface_container())),
        border: Border {
            color: ui.outline_variant(),
            width: 1.0,
            radius: RADIUS_XXL.into(),
        },
        ..Default::default()
    };

    if landscape_strip {
        let mut row = Row::new()
            .spacing(SPACE_SM)
            .align_y(Alignment::Center)
            .width(Fill)
            .height(Length::Fixed(cell_h));
        for (ic, label, msg, active) in collected {
            row = row.push(cell(ic, label, msg, active));
        }
        let nav_h = cell_h + 12.0;
        return container(row)
            .padding(Padding::from([6, 8]))
            .width(Fill)
            .height(Length::Fixed(nav_h))
            .style(nav_chrome)
            .into();
    }

    let mid = collected.len().div_ceil(2).max(1);
    let mut row1 = Row::new()
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
        .width(Fill)
        .height(Length::Fixed(cell_h));
    let mut row2 = Row::new()
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
        .width(Fill)
        .height(Length::Fixed(cell_h));
    for (i, (ic, label, msg, active)) in collected.into_iter().enumerate() {
        let c = cell(ic, label, msg, active);
        if i < mid {
            row1 = row1.push(c);
        } else {
            row2 = row2.push(c);
        }
    }

    let nav_h = cell_h * 2.0 + SPACE_XXS + 12.0;
    container(
        column![row1, row2]
            .spacing(SPACE_XXS)
            .width(Fill)
            .height(Length::Shrink),
    )
    .padding(Padding::from([6, 8]))
    .width(Fill)
    .height(Length::Fixed(nav_h))
    .style(nav_chrome)
    .into()
    }
}

/// Phone / narrow: compact two-column tab grid (Row+mouse_area drops siblings on GLES).
pub fn mode_top_nav<'a>(
    ui: UiTheme,
    label_size: f32,
    items: impl IntoIterator<Item = (crate::icons::Icon, &'a str, Message, bool)>,
) -> Element<'a, Message> {
    mode_top_nav_ex(ui, label_size, false, items)
}

pub fn category_sidebar<'a>(
    ui: UiTheme,
    width: f32,
    title: &'a str,
    filter: &str,
    entries: &'a [(String, String, bool, usize)],
    scroll_y: f32,
    view_h: f32,
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

    let total = entries.len();
    let row_h = cat_row_height();
    let slice = virtual_slice(scroll_y, view_h, row_h, total, VIRTUAL_OVERSCAN);
    let mut rows: Vec<Element<'a, Message>> = Vec::with_capacity(slice.end.saturating_sub(slice.start));
    if total == 0 {
        rows.push(text("Aucune catégorie").size(12).color(ui.ink_muted()).into());
    } else {
        for (id, name, active, count) in &entries[slice.start..slice.end] {
            let label = if *count > 0 {
                format!("{name}  · {count}")
            } else {
                name.clone()
            };
            rows.push(cat_row(
                label.replace(';', " · "),
                Message::SelectBrowseCategory(id.clone()),
                ui,
                *active,
            ));
        }
    }
    let list = if total == 0 {
        Column::with_children(rows).spacing(SPACE_XXS).width(Fill)
    } else {
        virtual_column(slice, rows)
    };

    pane(
        ui,
        Length::Fixed(width.max(120.0)),
        column![
            text(title).size(13).color(ui.ink_muted()),
            filter_input,
            soft_scroll_on(ui, list.width(Fill), "flux-cats", Message::CatScrolled),
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
    entries: &'a [(String, String, bool, usize)],
) -> Element<'a, Message> {
    use crate::theme::TOUCH_TARGET;
    let strip_h = TOUCH_TARGET + 8.0;
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

    let mut chips = Row::new()
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
        .height(Length::Fixed(strip_h));
    chips = chips.push(filter_input);
    // Phone strip: hard cap — horizontal scroll of 1500 chips is unusable anyway.
    for (id, name, active, count) in entries.iter().take(48) {
        let label = if *count > 0 {
            format!("{} · {count}", name.replace(';', " · "))
        } else {
            name.replace(';', " · ")
        };
        let fg = if *active {
            ui.on_secondary_container()
        } else {
            ui.on_surface()
        };
        let bg = if *active {
            ui.secondary_container()
        } else {
            ui.surface_container_low()
        };
        // Fixed ≤48 + Shrink width: selected morphs to LARGE_INCREASED; else FULL (stretch-safe).
        let chip_radius = if *active {
            RADIUS_LARGE_INCREASED.into()
        } else {
            RADIUS_FULL.into()
        };
        let chip = container(text(label).size(TYPE_LABEL_L).color(fg))
            .padding(Padding::from([10, 14]))
            .height(Length::Fixed(TOUCH_TARGET))
            .align_y(Alignment::Center)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    color: if *active {
                        Color::TRANSPARENT
                    } else {
                        ui.outline_variant()
                    },
                    width: if *active { 0.0 } else { 1.0 },
                    radius: chip_radius,
                },
                ..Default::default()
            });
        chips = chips.push(mouse_area(chip).on_press(Message::SelectBrowseCategory(id.clone())));
    }

    // Fixed strip height — Shrink+Fill parents on Android stretched chips into giant capsules.
    scrollable(chips)
        .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::hidden()))
        .width(Fill)
        .height(Length::Fixed(strip_h))
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
                radius: radius_list_item(active),
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
    let (title_sz, title_font) = if title_size >= 24.0 {
        type_style(TypeRole::HeadlineS, true)
    } else if title_size >= 20.0 {
        type_style(TypeRole::TitleL, true)
    } else {
        type_style(TypeRole::TitleM, true)
    };
    let (sub_sz, sub_font) = type_style(TypeRole::LabelM, false);
    let titles = column![
        text(title)
            .size(title_sz)
            .font(title_font)
            .color(ui.on_surface()),
        text(subtitle)
            .size(sub_sz)
            .font(sub_font)
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
    // Outer row width in px (Fill under mouse_area collapses on GLES).
    row_w: f32,
) -> Element<'a, Message> {
    use crate::theme::TOUCH_TARGET;
    let (title_sz, title_font) = type_style(TypeRole::TitleM, active);
    let title_c = ui.on_surface();
    let row_bg = if active {
        ui.selection_fill()
    } else {
        Color::TRANSPARENT
    };
    let row_w = row_w.max(120.0);

    // Android GLES: flat row — letter/thumb sibling (not nested under titles).
    // Prefer letter avatar when thumb missing; image only as peer of text.
    #[cfg(target_os = "android")]
    {
        let (sub_sz, _) = type_style(TypeRole::LabelM, false);
        let sub_c = ui.on_surface_variant();
        let ts = thumb_size.clamp(36.0, 56.0);
        let glyph = title
            .chars()
            .find(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .unwrap_or('#');
        let thumb_el: Element<'a, Message> = if let Some(handle) = thumb {
            tracing::trace!(target: "fluxplay::images", "media_row thumb");
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
            container(
                text(glyph.to_string())
                    .size((ts * 0.42).clamp(14.0, 22.0))
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
        let titles: Element<'a, Message> = if subtitle.is_empty() {
            text(title)
                .size(title_sz)
                .font(title_font)
                .color(title_c)
                .into()
        } else {
            column![
                text(title)
                    .size(title_sz)
                    .font(title_font)
                    .color(title_c),
                text(subtitle).size(sub_sz).color(sub_c),
            ]
            .spacing(SPACE_XXS)
            .into()
        };
        let row_h = list_row_height(ts);
        let row_style = move |_t: &Theme| container::Style {
            background: Some(Background::Color(row_bg)),
            border: Border {
                color: if active {
                    ui.outline_variant()
                } else {
                    Color::TRANSPARENT
                },
                width: if active { 1.0 } else { 0.0 },
                radius: radius_list_item(active),
            },
            ..Default::default()
        };
        let play_w = TOUCH_TARGET;
        let play_el = mouse_area(
            container(crate::icons::icon(
                crate::icons::Icon::Play,
                22.0,
                ui.primary(),
            ))
            .width(Length::Fixed(play_w))
            .height(Length::Fixed(row_h))
            .center_x(Fill)
            .center_y(Fill),
        )
        .on_press(on_open.clone());
        if let Some((is_fav, fav_msg)) = on_fav {
            let fav_c = if is_fav {
                ui.tertiary()
            } else {
                ui.on_surface_variant()
            };
            let fav_w = TOUCH_TARGET;
            let title_w = (row_w - ts - fav_w - play_w - 12.0).max(64.0);
            let fav_icon = crate::icons::icon(crate::icons::Icon::Favorite, 22.0, fav_c);
            return row![
                thumb_el,
                mouse_area(
                    container(fav_icon)
                        .width(Length::Fixed(fav_w))
                        .height(Length::Fixed(row_h))
                        .center_x(Fill)
                        .center_y(Fill),
                )
                .on_press(fav_msg),
                mouse_area(
                    container(titles)
                        .width(Length::Fixed(title_w))
                        .height(Length::Fixed(row_h))
                        .padding(Padding::from([10, 12]))
                        .align_y(Alignment::Center)
                        .style(row_style),
                )
                .on_release(on_open),
                play_el,
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .width(Length::Fixed(row_w))
            .into();
        }
        let title_w = (row_w - ts - play_w - 12.0).max(64.0);
        return row![
            thumb_el,
            mouse_area(
                container(titles)
                    .width(Length::Fixed(title_w))
                    .height(Length::Fixed(row_h))
                    .padding(Padding::from([10, 12]))
                    .align_y(Alignment::Center)
                    .style(row_style),
            )
            .on_release(on_open),
            play_el,
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fixed(row_w))
        .into();
    }

    #[cfg(not(target_os = "android"))]
    {
    let ts = thumb_size.clamp(32.0, 72.0).max(TOUCH_TARGET - 8.0);
    let fav_w = if on_fav.is_some() { 36.0 } else { 0.0 };
    let label_w = (row_w - ts - fav_w - 40.0).max(64.0);
    let use_raster_thumb = thumb.is_some();

    let thumb_el: Element<'a, Message> = if use_raster_thumb {
        let handle = thumb.expect("use_raster_thumb");
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
        text(title).size(title_sz).font(title_font).color(title_c),
        text(subtitle).size(TYPE_LABEL_M).color(ui.on_surface_variant()),
    ]
    .spacing(SPACE_XXS)
    .width(Length::Fixed(label_w));

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

    let main: Element<'a, Message> = if let Some((is_fav, fav_msg)) = on_fav {
        let star = if is_fav { "+" } else { "-" };
        let fav_c = ui.tertiary();
        row![
            mark,
            Space::new().width(SPACE_SM),
            thumb_el,
            Space::new().width(SPACE_SM),
            labels,
            Space::new().width(SPACE_SM),
            mouse_area(
                container(text(star).size(TYPE_TITLE_M).color(fav_c))
                    .width(Length::Fixed(TOUCH_TARGET - 8.0))
                    .height(Length::Fixed(TOUCH_TARGET - 8.0))
                    .center_x(Fill)
                    .center_y(Fill),
            )
            .on_press(fav_msg),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fixed(row_w))
        .into()
    } else {
        row![
            mark,
            Space::new().width(SPACE_SM),
            thumb_el,
            Space::new().width(SPACE_SM),
            labels,
        ]
        .align_y(Alignment::Center)
        .width(Length::Fixed(row_w))
        .into()
    };

    mouse_area(
        container(main)
            .width(Length::Fixed(row_w))
            .height(Length::Fixed(list_row_height(ts)))
            .padding(Padding::from([8, 8]))
            .center_y(Fill)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(row_bg)),
                border: Border {
                    radius: radius_list_item(active),
                    ..Default::default()
                },
                ..Default::default()
            }),
    )
    .on_press(on_open)
    .into()
    }
}

/// Registered source card — full-width list item with trailing icon actions (M3).
pub fn source_card<'a>(
    title: String,
    subtitle: String,
    on_open: Message,
    on_reload: Message,
    on_export: Message,
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

    let export = mouse_area(
        container(icons::icon(Icon::FolderOpen, 22.0, ui.primary()))
            .padding(10)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.primary_container())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
    )
    .on_press(on_export);

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
        row![open, reload, export, remove]
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
    // Outlined / tonal L pill — mouse_area (styled `button` drops glyphs on GLES).
    let label = match remaining {
        Some(n) if n > 0 => format!("Afficher plus (+{n})"),
        _ => "Afficher plus".into(),
    };
    mouse_area(
        container(text(label).size(TYPE_LABEL_L).color(ui.primary()))
            .padding(Padding::from([16, 24]))
            .width(Fill)
            .align_x(Alignment::Center)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(Color::TRANSPARENT)),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: ui.outline(),
                    width: 1.0,
                },
                ..Default::default()
            }),
    )
    .on_press(Message::LoadMore)
    .into()
}

/// MYTV-style poster tile: vertical art + title + "year, genre".
pub fn mosaic_tile<'a>(
    title: String,
    meta_line: String,
    id: String,
    on_open: Message,
    ui: UiTheme,
    tile_w: f32,
    thumb: Option<&'a Handle>,
    pressed: bool,
) -> Element<'a, Message> {
    let w = tile_w.max(96.0);
    // Classic poster aspect ~2:3
    let h = (w * 1.5).round();
    let press_alpha = if pressed { 0.85_f32 } else { 1.0 };

    let poster: Element<'a, Message> = {
        // Android: show poster art by default; skip only if FLUXPLAY_ANDROID_ART=0/off.
        #[cfg(target_os = "android")]
        let thumb: Option<&Handle> = {
            let skip = std::env::var("FLUXPLAY_ANDROID_ART")
                .ok()
                .map(|v| {
                    let v = v.trim();
                    v.eq_ignore_ascii_case("0") || v.eq_ignore_ascii_case("off")
                })
                .unwrap_or(false);
            if skip {
                None
            } else {
                thumb
            }
        };
        #[cfg(not(target_os = "android"))]
        let thumb = thumb;
        if let Some(handle) = thumb {
            container(
                image(handle)
                    .width(Length::Fixed(w))
                    .height(Length::Fixed(h))
                    .content_fit(iced::ContentFit::Cover)
                    .opacity(press_alpha),
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
            let letter = title
                .chars()
                .find(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .unwrap_or('#');
            mosaic_empty_poster(ui, w, h, letter)
        }
    };

    // Title/meta clipped to tile width — prevent mosaic text bleed into neighbors.
    let max_chars = ((w / 7.2).floor() as usize).clamp(10, 48);
    let title_s = truncate_ui(&title, max_chars).into_owned();
    let meta_s = truncate_ui(&meta_line, max_chars + 4).into_owned();
    let (tile_sz, tile_font) = type_style(TypeRole::TitleS, false);
    let (meta_sz, meta_font) = type_style(TypeRole::LabelM, false);
    let title_el = text(title_s)
        .size(tile_sz)
        .font(tile_font)
        .color(ui.on_surface())
        .width(Length::Fixed(w));
    let meta_el = text(meta_s)
        .size(meta_sz)
        .font(meta_font)
        .color(ui.on_surface_variant())
        .width(Length::Fixed(w));

    let body = container(
        column![poster, title_el, meta_el]
            .spacing(SPACE_SM)
            .width(Length::Fixed(w)),
    )
    .padding(Padding::from([0, 2]))
    .width(Length::Fixed(w))
    .clip(true)
    .style(move |_t: &Theme| container::Style {
        background: if pressed {
            let mut c = ui.surface_container_high();
            c.a *= press_alpha;
            Some(Background::Color(c))
        } else {
            None
        },
        ..Default::default()
    });

    mouse_area(body)
        .on_press(Message::MosaicPress(id))
        .on_release(on_open)
        .into()
}

fn mosaic_empty_poster<'a>(ui: UiTheme, w: f32, h: f32, letter: char) -> Element<'a, Message> {
    // Single ASCII letter — emoji/multi-glyph often vanish on Android GLES.
    container(
        text(letter.to_string())
            .size((w * 0.28).clamp(18.0, 40.0))
            .color(ui.ink_muted()),
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
}

fn truncate_ui(s: &str, max: usize) -> Cow<'_, str> {
    let mut count = 0usize;
    for (i, _) in s.char_indices() {
        if count >= max {
            let mut t = s[..i].to_string();
            t.push('…');
            return Cow::Owned(t);
        }
        count += 1;
    }
    Cow::Borrowed(s)
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
        // SplitButton / FAB primary — mouse_area (GLES-safe).
        action_els.push(
            mouse_area(
                container(text(label).size(15).color(ui.on_primary()))
                    .padding(Padding::from([14, 22]))
                    .style(move |_t: &Theme| container::Style {
                        background: Some(Background::Color(ui.primary())),
                        border: Border {
                            radius: radius_fab(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
            )
            .on_press(msg)
            .into(),
        );
    }
    let imdb_label = imdb_id
        .filter(|s| !s.is_empty())
        .map(|id| format!("IMDb · {id}"))
        .unwrap_or_else(|| "IMDb".into());
    action_els.push(
        mouse_area(
            container(text(imdb_label).size(13).color(ui.accent()))
                .padding(Padding::from([12, 16]))
                .style(move |_t: &Theme| container::Style {
                    background: Some(Background::Color(ui.secondary_container())),
                    border: Border {
                        radius: RADIUS_FULL.into(),
                        color: ui.outline_variant(),
                        width: 1.0,
                    },
                    ..Default::default()
                }),
        )
        .on_press(Message::OpenImdb(imdb_query))
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

    let back = mouse_area(
        container(text("<- Catalogue").size(13).color(ui.ink()))
            .padding(Padding::from([10, 16]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container_low())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: ui.outline_variant(),
                    width: 1.0,
                },
                ..Default::default()
            }),
    )
    .on_press(back);

    // Header outside the scroll so the page always has a visible chrome
    // (avoids a zero-height scroll-only layout on some window sizes).
    // Episodes get a guaranteed FillPortion so a tall synopsis cannot steal
    // the whole column (phone: missing episode rows / play affordance).
    let page: Element<'a, Message> = if let Some(eps) = episodes {
        column![
            back,
            container(soft_scroll_fit(
                ui,
                body.padding(Padding::from([0, 4])).width(Fill),
            ))
            .width(Fill)
            .height(Length::FillPortion(2)),
            text("Épisodes").size(16),
            container(soft_scroll_on(
                ui,
                eps,
                "flux-episodes",
                Message::BrowseScrolled,
            ))
            .width(Fill)
            .height(Length::FillPortion(3)),
        ]
        .spacing(SPACE_MD)
        .width(Fill)
        .height(Fill)
        .into()
    } else {
        column![
            back,
            soft_scroll(ui, body.padding(Padding::from([0, 4])).width(Fill))
        ]
        .spacing(SPACE_MD)
        .width(Fill)
        .height(Fill)
        .into()
    };

    pane(ui, Length::Fill, page)
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
