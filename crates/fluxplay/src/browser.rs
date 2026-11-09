//! Media-center chrome: dense panes, capped lists, reliable selection.
//! Inspired by TiviMate / MYTV / IPTVnator / Smarters + Material 3 Expressive.
//!
//! Pure view helpers — operational logs live in `main` Message handlers.
//! GLES rule: never put nav labels inside styled `button` / heavy container chrome.

use iced::widget::{
    column, container, image, mouse_area, pin, row, scrollable, stack, text, text_input, Column,
    Row, Space,
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
    RADIUS_LG, RADIUS_MD, RADIUS_SM, RADIUS_XL, RADIUS_XXL, SPACE_MD, SPACE_SM, SPACE_XL, SPACE_XS, SPACE_XXS,
    TYPE_BODY_M, TYPE_LABEL_L, TYPE_LABEL_M, TYPE_TITLE_M, TYPE_TITLE_S,
};
use crate::app::{Message, PasteTarget, Shelf};

/// One screen of posters. Each « Voir plus » / « Afficher plus » loads the next screen only.
pub const LIST_PAGE: usize = 24;
/// Alias used by RAM invariants / mosaic page unlocks.
pub const MOSAIC_PAGE_ITEMS: usize = LIST_PAGE;
/// Chaînes et guide : un écran n'en montre qu'une vingtaine, un bloc en charge quatre.
pub const LIVE_PAGE: usize = 96;
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
/// Tile geometry only — keep tiny so posters stay on-screen (RAM).
pub const VIRTUAL_OVERSCAN: usize = 4;

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
/// Episodes listed via the same virtual window (cap reserved for non-virtual fallbacks).
#[allow(dead_code)]
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

    #[test]
    fn pin_offset_tracks_the_row_under_the_scrollbar() {
        let row_h = 200.0;
        let view_h = 600.0;
        let rows = 40usize;
        let slice = virtual_slice(250.0, view_h, row_h, rows, 0);
        let off = mosaic_pin_offset(250.0, view_h, row_h, rows, slice);
        assert!((off - 50.0).abs() < 0.5, "off={off} slice={slice:?}");
        let top = virtual_slice(0.0, view_h, row_h, rows, 0);
        assert!(mosaic_pin_offset(0.0, view_h, row_h, rows, top) < 0.5);
    }
}

/// Height of one channel card (logo beside the name and the programme).
pub const CHANNEL_CARD_H: f32 = 96.0;

/// Channel grid stride. The logo no longer sets the row height.
pub fn channel_row_height() -> f32 {
    CHANNEL_CARD_H + MOSAIC_GAP
}

/// Caption under a poster: date and age, then the categories. Larger than a label
/// so it stays readable, without shrinking the poster or the title.
const MOSAIC_META: f32 = 15.0;

/// Mosaic row stride (poster + title + two caption lines + gap) for virtualization.
pub fn mosaic_row_height(tile_w: f32) -> f32 {
    let w = tile_w.max(96.0);
    let poster_h = (w * 1.5).round();
    // iced line-height is taller than the font size; short rows overlap the next poster.
    poster_h
        + SPACE_SM
        + TYPE_TITLE_S * 1.35
        + SPACE_SM
        + MOSAIC_META * 1.35
        + 2.0
        + MOSAIC_META * 1.35
        + MOSAIC_GAP
}

/// One line. Whole categories are kept; the rest ends with ` [...]` when that marker fits.
fn fit_one_line(text: &str, width: f32, size: f32) -> String {
    let px = size * 0.50;
    let budget = width.max(48.0);
    let width_of = |s: &str| s.chars().count() as f32 * px;
    if width_of(text) <= budget {
        return text.to_string();
    }
    let parts: Vec<&str> = text
        .split(" · ")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() <= 1 {
        let max = ((budget / px).floor() as usize).saturating_sub(6).max(1);
        let mut s: String = text.chars().take(max).collect();
        while s.ends_with([' ', '·', ',', '-', '/', '|']) {
            s.pop();
        }
        s.push_str(" [...]");
        return s;
    }
    let mut shown: Vec<&str> = Vec::new();
    for part in &parts {
        let mut trial = shown.clone();
        trial.push(*part);
        if width_of(&trial.join(" · ")) <= budget {
            shown.push(*part);
        } else {
            break;
        }
    }
    if shown.is_empty() {
        shown.push(parts[0]);
    }
    if shown.len() < parts.len() {
        let marked = format!("{} [...]", shown.join(" · "));
        if width_of(&marked) <= budget {
            return marked;
        }
    }
    shown.join(" · ")
}

/// How far the pinned mosaic must shift up so the row under the scrollbar
/// is the one on screen (the overlay itself does not scroll).
pub fn mosaic_pin_offset(
    scroll_y: f32,
    view_h: f32,
    row_h: f32,
    total_rows: usize,
    slice: VirtualSlice,
) -> f32 {
    if total_rows == 0 || row_h <= 1.0 {
        return 0.0;
    }
    let natural = total_rows as f32 * row_h;
    if natural <= MAX_VIRTUAL_CONTENT_PX {
        let content_y = slice.start as f32 * row_h;
        return (scroll_y - content_y).clamp(0.0, row_h * 4.0);
    }
    let content_h = virtual_content_height(total_rows, view_h, row_h);
    let max_scroll = (content_h - view_h).max(1.0);
    let t = (scroll_y / max_scroll).clamp(0.0, 1.0);
    let visible = ((view_h.max(row_h) / row_h).ceil() as usize)
        .max(1)
        .min(total_rows);
    let max_start = total_rows.saturating_sub(visible);
    if max_start == 0 {
        return 0.0;
    }
    let exact = t * max_start as f32;
    (exact - slice.start as f32).clamp(0.0, 1.0) * row_h
}

/// Place `mosaic` inside the viewport, shifted by `offset` px (scroll fraction).
pub fn pin_mosaic<'a>(mosaic: Element<'a, Message>, offset: f32) -> Element<'a, Message> {
    pin(mosaic)
        .y(-offset.max(0.0))
        .width(Fill)
        .height(Fill)
        .into()
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
        .clip(true)
        .into()
}

/// Scroll when needed, but do not steal leftover column height (avoids empty “grey bar”).
#[allow(dead_code)]
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

/// Historique / Favoris, indented under the open section. Same fill as the rail when selected.
fn rail_sub<'a>(
    ui: UiTheme,
    label: String,
    active: bool,
    msg: Message,
    label_size: f32,
) -> Element<'a, Message> {
    let fg = if active {
        ui.on_primary_container()
    } else {
        ui.on_surface_variant()
    };
    let bg = if active {
        ui.primary_container()
    } else {
        Color::TRANSPARENT
    };
    mouse_area(
        container(text(label).size((label_size - 1.0).max(13.0)).color(fg))
            .width(Fill)
            .padding(Padding {
                top: 8.0,
                right: 12.0,
                bottom: 8.0,
                left: 36.0,
            })
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

pub fn mode_rail<'a>(
    ui: UiTheme,
    width: f32,
    label_size: f32,
    items: impl IntoIterator<Item = (crate::icons::Icon, &'a str, Message, bool)>,
    subs: Vec<(String, bool, Message)>,
) -> Element<'a, Message> {
    let mut nav = Column::new().spacing(SPACE_XS).width(Fill);
    // Brand sits in the middle of the rail, a step larger than the nav labels.
    let brand_size = (label_size + 12.0).clamp(24.0, 30.0);
    let tag_size = (label_size - 1.0).clamp(14.0, 16.0);
    nav = nav.push(
        column![
            text("FluxPlay")
                .size(brand_size)
                .font(type_font(true))
                .color(ui.primary())
                .width(Fill)
                .align_x(Alignment::Center),
            text("Media center")
                .size(tag_size)
                .color(ui.on_surface_variant())
                .width(Fill)
                .align_x(Alignment::Center),
        ]
        .spacing(2)
        .width(Fill)
        .align_x(Alignment::Center)
        .padding(Padding::from([10, 4])),
    );
    nav = nav.push(Space::new().height(SPACE_MD));
    for (ic, label, msg, active) in items {
        // Active item follows the chosen color, not the complementary secondary.
        let fg = if active {
            ui.on_primary_container()
        } else {
            ui.on_surface_variant()
        };
        let bg = if active {
            ui.primary_container()
        } else {
            Color::TRANSPARENT
        };
        let indicator = container(Space::new().width(4.0).height(label_size + 14.0))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(if active {
                    ui.primary()
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
        if active && !subs.is_empty() {
            for (label, on, msg) in subs.iter() {
                nav = nav.push(rail_sub(ui, label.clone(), *on, msg.clone(), label_size));
            }
        }
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
                ui.on_primary_container()
            } else {
                ui.on_surface()
            };
            let bg = if active {
                ui.primary_container()
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
                    color: ui.primary(),
                    width: 1.5,
                    radius: RADIUS_XXL.into(),
                },
                ..Default::default()
            })
            .into();
    }

    #[cfg(not(target_os = "android"))]
    {
    let cell_h = (TOUCH_TARGET + label_size + 10.0).clamp(64.0, 72.0);
    let cell_w = 108.0_f32;

    let cell = |ic: crate::icons::Icon, label: &'a str, msg: Message, active: bool| {
        let fg = if active {
            ui.on_primary_container()
        } else {
            ui.on_surface_variant()
        };
        let pill_bg = if active {
            ui.primary_container()
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
                .spacing(2)
                .align_x(Alignment::Center)
                .width(Fill),
            )
            .width(Length::Fixed(cell_w))
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

    // Fixed-width cells that wrap and stay centered. Fill cells were stretching
    // each destination across the whole window ("options étalées").
    let _strip = landscape_strip;
    let mut nav_row = Row::new().spacing(SPACE_SM).width(Fill);
    for (ic, label, msg, active) in collected {
        nav_row = nav_row.push(cell(ic, label, msg, active));
    }
    container(
        nav_row
            .wrap()
            .vertical_spacing(if _strip { SPACE_XXS } else { SPACE_XS })
            .align_x(iced::alignment::Horizontal::Center),
    )
    .padding(Padding::from([6, 8]))
    .width(Fill)
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

fn paste_affix(ui: UiTheme, target: PasteTarget) -> Element<'static, Message> {
    use crate::icons::{self, Icon};
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
    .on_press(Message::PasteInto(target))
    .into()
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
    // Deliberately unlike the content search pill (rounded-rect, smaller, no icon):
    // this narrows the category list, not the catalogue.
    let filter_input = text_input("Filtrer les catégories…", filter)
        .on_input(Message::CatFilterChanged)
        .padding(Padding::from([6, 10]))
        .size(12)
        .style(move |theme: &Theme, status| {
            let mut s = text_input::default(theme, status);
            s.border.radius = RADIUS_SM.into();
            s.border.color = ui.outline_variant();
            s.background = Background::Color(ui.surface_container());
            s
        });
    let filter_row = row![
        container(filter_input).width(Fill),
        paste_affix(ui, PasteTarget::CatFilter),
    ]
    .spacing(SPACE_XS)
    .align_y(Alignment::Center)
    .width(Fill);

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
            #[cfg(target_os = "android")]
            let label = sanitize_android_ui_text(&label);
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
            row![
                text(format!("Catégories · {title}"))
                    .size(TYPE_LABEL_L)
                    .font(type_style(TypeRole::TitleM, true).1)
                    .color(ui.on_surface())
                    .width(Fill),
                text(format!("{}", total.saturating_sub(1)))
                    .size(TYPE_LABEL_M)
                    .color(ui.ink_muted()),
            ]
            .align_y(Alignment::Center),
            filter_row,
            soft_scroll_on(ui, list.width(Fill), "flux-cats", Message::CatScrolled),
        ]
        .spacing(SPACE_SM)
        .width(Fill)
        .height(Fill),
    )
}

/// Round handle beside the category list. The column is only as wide as the circle.
pub fn category_tab<'a>(ui: UiTheme, open: bool) -> Element<'a, Message> {
    let glyph = if open {
        crate::icons::Icon::ChevronLeft
    } else {
        crate::icons::Icon::ChevronRight
    };
    let side = 52.0;
    let tab = mouse_area(
        container(crate::icons::icon(glyph, 32.0, ui.primary()))
            .width(Length::Fixed(side))
            .height(Length::Fixed(side))
            .center_x(Length::Fixed(side))
            .center_y(Length::Fixed(side))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container_highest())),
                border: Border {
                    color: ui.primary(),
                    width: 1.5,
                    radius: RADIUS_FULL.into(),
                },
                ..Default::default()
            }),
    )
    .on_release(Message::ToggleCategories)
    .interaction(mouse::Interaction::Pointer);
    container(
        column![Space::new().height(Fill), tab, Space::new().height(Fill)]
            .width(Length::Fixed(side))
            .height(Fill)
            .align_x(Alignment::Center),
    )
    .width(Length::Fixed(side))
    .height(Fill)
    .into()
}

/// Compact category chips when the sidebar is collapsed (phone).
pub fn category_chips<'a>(
    ui: UiTheme,
    filter: &str,
    entries: &'a [(String, String, bool, usize)],
) -> Element<'a, Message> {
    use crate::theme::TOUCH_TARGET;
    let strip_h = TOUCH_TARGET + 8.0;
    let filter_input = text_input("Catégorie…", filter)
        .on_input(Message::CatFilterChanged)
        .padding(SPACE_SM as u16)
        .size(13)
        .width(Length::Fixed(112.0))
        .style(move |theme: &Theme, status| {
            let mut s = text_input::default(theme, status);
            s.border.radius = RADIUS_SM.into();
            s.border.color = ui.outline_variant();
            s.background = Background::Color(ui.surface_container_low());
            s
        });
    let filter_row = row![filter_input, paste_affix(ui, PasteTarget::CatFilter)]
        .spacing(SPACE_XS)
        .align_y(Alignment::Center);

    let mut chips = Row::new()
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
        .height(Length::Fixed(strip_h));
    chips = chips.push(filter_row);
    // Phone strip: hard cap — horizontal scroll of 1500 chips is unusable anyway.
    for (id, name, active, count) in entries.iter().take(48) {
        let label = if *count > 0 {
            format!("{} · {count}", name.replace(';', " · "))
        } else {
            name.replace(';', " · ")
        };
        #[cfg(target_os = "android")]
        let label = sanitize_android_ui_text(&label);
        let fg = if *active {
            ui.on_primary_container()
        } else {
            ui.on_surface()
        };
        let bg = if *active {
            ui.primary_container()
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
        ui.on_primary_container()
    } else {
        ui.on_surface()
    };
    let bg = if active {
        ui.primary_container()
    } else {
        Color::TRANSPARENT
    };
    let bar = container(Space::new().width(if active { 4.0 } else { 0.0 }).height(22.0))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(if active {
                ui.primary()
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
    .on_release(msg)
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
    content_header_with(ui, title, subtitle, search, "Rechercher…", search_width, title_size, stack)
}

/// [`content_header`] with a contextual search placeholder ("Rechercher une série…").
#[allow(clippy::too_many_arguments)]
pub fn content_header_with<'a>(
    ui: UiTheme,
    title: String,
    subtitle: String,
    search: &str,
    placeholder: &str,
    search_width: f32,
    title_size: f32,
    stack: bool,
) -> Element<'a, Message> {
    #[cfg(target_os = "android")]
    let title = sanitize_android_ui_text(&title);
    #[cfg(target_os = "android")]
    let subtitle = sanitize_android_ui_text(&subtitle);
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

    let active = !search.is_empty();
    let search_el = text_input(placeholder, search)
        .on_input(Message::SearchChanged)
        .padding(if stack { 12 } else { 14 })
        .size(TYPE_BODY_M)
        .width(Fill)
        .style(move |theme: &Theme, status| {
            let mut s = text_input::default(theme, status);
            s.border.radius = RADIUS_FULL.into();
            let focused = matches!(status, text_input::Status::Focused { .. });
            s.border.width = if focused || active { 2.0 } else { 1.0 };
            s.border.color = if focused || active {
                ui.primary()
            } else {
                ui.outline()
            };
            s.background = Background::Color(ui.surface_container_highest());
            s
        });
    let clear: Element<'a, Message> = if active {
        mouse_area(
            container(crate::icons::icon(
                crate::icons::Icon::Close,
                18.0,
                ui.on_surface_variant(),
            ))
            .padding(Padding::from([0, 4])),
        )
        .on_press(Message::SearchChanged(String::new()))
        .into()
    } else {
        Space::new().width(0).into()
    };
    let search_row = row![
        container(crate::icons::icon(
            crate::icons::Icon::Search,
            22.0,
            if active { ui.primary() } else { ui.on_surface_variant() },
        ))
        .padding(Padding::from([0, 4])),
        container(search_el).width(Fill),
        clear,
        paste_affix(ui, PasteTarget::Search),
        accent_suffix(ui),
    ]
    .spacing(SPACE_XS)
    .align_y(Alignment::Center)
    .width(if stack {
        Fill
    } else {
        Length::Fixed(search_width.clamp(100.0, 720.0))
    });

    if stack {
        column![titles, search_row]
            .spacing(SPACE_SM)
            .width(Fill)
            .into()
    } else {
        // Title stays on the left. Equal flexible space on both sides puts the
        // search pill in the middle of the films / series pane.
        row![titles, search_row, Space::new().width(Fill)]
            .spacing(SPACE_MD)
            .align_y(Alignment::Center)
            .width(Fill)
            .into()
    }
}

/// Logo or poster inside a fixed frame. Portrait art covers the frame;
/// logos and banners stay fully visible (no center-crop, no stretch).
fn framed_image<'a>(
    handle: &'a Handle,
    w: f32,
    h: f32,
    radius: impl Into<iced::border::Radius> + Copy,
    bg: Color,
) -> Element<'a, Message> {
    let fit = crate::images::art_content_fit(handle);
    let contain = matches!(fit, iced::ContentFit::Contain);
    let (iw, ih) = if contain {
        ((w * 0.86).max(16.0), (h * 0.86).max(16.0))
    } else {
        (w, h)
    };
    let radius = radius.into();
    container(
        image(handle)
            .width(Length::Fixed(iw))
            .height(Length::Fixed(ih))
            .content_fit(fit)
            .border_radius(radius),
    )
    .width(Length::Fixed(w))
    .height(Length::Fixed(h))
    .center_x(Length::Fixed(w))
    .center_y(Length::Fixed(h))
    .clip(true)
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(bg)),
        border: Border {
            radius,
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

// UI row builder: each arg maps to a distinct visual slot; a builder struct adds noise.
#[allow(clippy::too_many_arguments)]
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
    #[cfg(target_os = "android")]
    let title = sanitize_android_ui_text(&title);
    #[cfg(target_os = "android")]
    let subtitle = sanitize_android_ui_text(&subtitle);
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
            framed_image(handle, ts, ts, CARD_RADIUS, ui.surface_container_low())
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
            let fav_w = TOUCH_TARGET;
            let title_w = (row_w - ts - fav_w - play_w - 12.0).max(64.0);
            let fav_icon = favorite_mark(ui, is_fav, 22.0);
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
        framed_image(handle, ts, ts, CARD_RADIUS, ui.surface_container_low())
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
        row![
            mark,
            Space::new().width(SPACE_SM),
            thumb_el,
            Space::new().width(SPACE_SM),
            labels,
            Space::new().width(SPACE_SM),
            mouse_area(
                container(favorite_mark(ui, is_fav, 22.0))
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
    .on_press(on_open.clone());

    let edit = mouse_area(
        container(text("Modifier").size(13).color(ui.on_surface()))
            .padding(Padding::from([8, 12]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: ui.primary(),
                    width: 1.5,
                },
                ..Default::default()
            }),
    )
    .on_press(on_open.clone());

    let reload = mouse_area(
        container(icons::icon(Icon::Refresh, 22.0, ui.on_surface()))
            .padding(10)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: ui.primary(),
                    width: 1.5,
                },
                ..Default::default()
            }),
    )
    .on_press(on_reload);

    let export = mouse_area(
        container(icons::icon(Icon::FolderOpen, 22.0, ui.on_surface()))
            .padding(10)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: ui.primary(),
                    width: 1.5,
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
        row![open, edit, reload, export, remove]
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

/// Pagination control kept for TV / non-virtual fallbacks (`Message::LoadMore`).
#[allow(dead_code)]
pub fn load_more_btn(ui: UiTheme, remaining: Option<usize>) -> Element<'static, Message> {
    // Outlined / tonal L pill — mouse_area (styled `button` drops glyphs on GLES).
    let label = match remaining {
        Some(n) if n > 0 => format!("Afficher plus (+{n})"),
        _ => "Afficher plus".into(),
    };
    mouse_area(
        container(text(label).size(TYPE_LABEL_L).color(ui.on_surface()))
            .padding(Padding::from([16, 24]))
            .width(Fill)
            .align_x(Alignment::Center)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(Color::TRANSPARENT)),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: ui.primary(),
                    width: 1.0,
                },
                ..Default::default()
            }),
    )
    .on_press(Message::LoadMore)
    .into()
}

/// Corner control on a poster: filled heart, or a remove mark on one history card.
#[derive(Clone)]
pub enum TileMark {
    Favorite { on: bool, toggle: Message },
    Remove(Message),
}

/// Catalogue / Historique / Favoris for the open section.
pub fn shelf_switch<'a>(
    ui: UiTheme,
    shelf: Shelf,
    history_label: String,
    fav_label: String,
    clear: Option<Message>,
) -> Element<'a, Message> {
    let mut bar = row![
        shelf_chip(ui, "Catalogue".into(), shelf == Shelf::Catalog, Message::ShowShelf(Shelf::Catalog)),
        shelf_chip(ui, history_label, shelf == Shelf::History, Message::ShowShelf(Shelf::History)),
        shelf_chip(ui, fav_label, shelf == Shelf::Favorites, Message::ShowShelf(Shelf::Favorites)),
    ]
    .spacing(SPACE_SM)
    .align_y(Alignment::Center);
    if let Some(msg) = clear {
        bar = bar.push(Space::new().width(Fill));
        bar = bar.push(shelf_chip(ui, "Effacer".into(), false, msg));
    }
    bar.width(Fill).into()
}

fn shelf_chip<'a>(ui: UiTheme, label: String, on: bool, msg: Message) -> Element<'a, Message> {
    let bg = ui.surface_container();
    let fg = ui.on_surface();
    let border = if on { ui.primary() } else { ui.outline_variant() };
    let border_w = if on { 2.0 } else { 1.0 };
    mouse_area(
        container(text(label).size(14).color(fg))
            .padding(Padding::from([8, 14]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    radius: radius_nav_pill(),
                    color: border,
                    width: border_w,
                },
                ..Default::default()
            }),
    )
    .on_press(msg)
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
    mosaic_tile_marked(title, meta_line, id, on_open, ui, tile_w, thumb, pressed, None)
}

// UI tile builder: each arg maps to a distinct visual slot; a builder struct adds noise.
#[allow(clippy::too_many_arguments)]
pub fn mosaic_tile_marked<'a>(
    title: String,
    meta_line: String,
    id: String,
    on_open: Message,
    ui: UiTheme,
    tile_w: f32,
    thumb: Option<&'a Handle>,
    pressed: bool,
    mark: Option<TileMark>,
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
        if let Some(handle) = thumb {
            framed_image(handle, w, h, radius_poster(), ui.surface_container_high())
        } else {
            let letter = title
                .chars()
                .find(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .unwrap_or('#');
            mosaic_empty_poster(ui, w, h, letter)
        }
    };
    let poster = match mark {
        Some(mark) => {
            let (icon_el, msg) = match mark {
                TileMark::Favorite { on, toggle } => (favorite_mark(ui, on, 16.0), toggle),
                TileMark::Remove(msg) => (
                    crate::icons::icon(crate::icons::Icon::Close, 14.0, ui.on_surface()),
                    msg,
                ),
            };
            let badge = mouse_area(
                container(icon_el)
                    .width(Length::Fixed(32.0))
                    .height(Length::Fixed(32.0))
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center)
                    .style(move |_t: &Theme| {
                        let mut bg = ui.surface_container_highest();
                        bg.a = 0.92;
                        container::Style {
                            background: Some(Background::Color(bg)),
                            border: Border {
                                radius: RADIUS_FULL.into(),
                                color: ui.outline_variant(),
                                width: 1.0,
                            },
                            ..Default::default()
                        }
                    }),
            )
            .on_press(msg)
            .on_release(Message::ShelfHold);
            stack![
                poster,
                container(badge)
                    .width(Length::Fixed(w))
                    .height(Length::Fixed(h))
                    .align_x(Alignment::End)
                    .align_y(Alignment::Start)
                    .padding(8),
            ]
            .into()
        }
        None => poster,
    };

    // Title stays where it was. The caption under it is two lines: date · age,
    // then every category that fits on one line.
    let max_chars = ((w / 7.2).floor() as usize).clamp(10, 48);
    let title_s = truncate_ui(&title, max_chars).into_owned();
    let mut parts = meta_line.splitn(2, '\n');
    let facts = fit_one_line(parts.next().unwrap_or("").trim(), w - 4.0, MOSAIC_META);
    let cats_raw = parts.next().unwrap_or("").trim();
    let cats = if cats_raw.is_empty() {
        String::new()
    } else {
        fit_one_line(cats_raw, w - 4.0, MOSAIC_META)
    };
    let (tile_sz, tile_font) = type_style(TypeRole::TitleS, false);
    let title_el = text(title_s)
        .size(tile_sz)
        .font(tile_font)
        .color(ui.on_surface())
        .width(Length::Fixed(w));
    let facts_el = container(text(facts).size(MOSAIC_META).color(ui.on_surface()))
        .width(Length::Fixed(w))
        .height(Length::Fixed(MOSAIC_META * 1.35))
        .clip(true);
    let cats_el = container(text(cats).size(MOSAIC_META).color(ui.on_surface()))
        .width(Length::Fixed(w))
        .height(Length::Fixed(MOSAIC_META * 1.35))
        .clip(true);

    let body = container(
        column![
            poster,
            title_el,
            column![facts_el, cats_el].spacing(2).width(Length::Fixed(w))
        ]
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

/// Channel card: a modest logo beside the name and what is on (and next, in the guide).
pub fn channel_tile<'a>(
    name: String,
    now_playing: String,
    next_playing: Option<String>,
    id: String,
    on_open: Message,
    on_fav: Option<(bool, Message)>,
    ui: UiTheme,
    tile_w: f32,
    thumb: Option<&'a Handle>,
    pressed: bool,
    selected: bool,
) -> Element<'a, Message> {
    let w = tile_w.max(112.0);
    let logo_side = if w < 190.0 { 40.0 } else { 64.0 };
    let poster: Element<'a, Message> = if let Some(handle) = thumb {
        framed_image(
            handle,
            logo_side,
            logo_side,
            RADIUS_MD,
            ui.surface_container_highest(),
        )
    } else {
        let letter = name
            .chars()
            .find(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .unwrap_or('#');
        mosaic_empty_poster(ui, logo_side, logo_side, letter)
    };
    let text_w = (w - logo_side - 12.0 - 28.0 - 20.0).max(72.0);
    let name_chars = ((text_w / 7.6).floor() as usize).clamp(12, 36);
    let name_s = truncate_ui(&name, name_chars).into_owned();
    let now_lines = if next_playing.is_some() { 1 } else { 2 };
    let now_s = truncate_ui(&now_playing, name_chars.saturating_mul(now_lines) + 4).into_owned();
    let (name_sz, name_font) = type_style(TypeRole::TitleM, true);
    let (now_sz, now_font) = type_style(TypeRole::BodyM, false);
    let now_color = if now_playing == "En direct" {
        ui.on_surface_variant()
    } else {
        ui.on_surface()
    };
    let mut lines = column![
        text(name_s)
            .size(name_sz)
            .font(name_font)
            .color(ui.on_surface())
            .width(Fill),
        text(now_s)
            .size(now_sz)
            .font(now_font)
            .color(now_color)
            .width(Fill),
    ]
    .spacing(3)
    .width(Fill);
    if let Some(next) = next_playing {
        let next_s = truncate_ui(&next, name_chars + 8).into_owned();
        let (next_sz, next_font) = type_style(TypeRole::LabelL, false);
        lines = lines.push(
            text(next_s)
                .size(next_sz)
                .font(next_font)
                .color(ui.on_surface_variant())
                .width(Fill),
        );
    }
    let play = mouse_area(
        row![poster, lines]
            .spacing(12)
            .align_y(Alignment::Center)
            .width(Fill),
    )
    .on_press(Message::MosaicPress(id))
    .on_release(on_open);
    let heart: Element<'a, Message> = if let Some((is_fav, fav_msg)) = on_fav {
        mouse_area(container(favorite_mark(ui, is_fav, 18.0)))
            .on_press(fav_msg)
            .into()
    } else {
        Space::new().width(0).into()
    };
    container(
        row![play, heart]
            .spacing(6)
            .align_y(Alignment::Center)
            .padding(Padding::from([8, 10]))
            .width(Fill)
            .height(Fill),
    )
    .width(Length::Fixed(w))
    .height(Length::Fixed(CHANNEL_CARD_H))
    .clip(true)
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(if pressed {
            ui.surface_container_high()
        } else {
            ui.surface_container()
        })),
        border: Border {
            color: if selected {
                ui.accent()
            } else {
                ui.outline_variant()
            },
            width: if selected { 1.5 } else { 1.0 },
            radius: RADIUS_LG.into(),
        },
        ..Default::default()
    })
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
    .center_x(Length::Fixed(w))
    .center_y(Length::Fixed(h))
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
    #[cfg(target_os = "android")]
    {
        // Fira Sans under NativeActivity has no emoji / superscript / many symbols → tofu.
        let cleaned = sanitize_android_ui_text(s);
        return Cow::Owned(truncate_chars(&cleaned, max));
    }
    #[cfg(not(target_os = "android"))]
    {
        for (count, (i, _)) in s.char_indices().enumerate() {
            if count >= max {
                let mut t = s[..i].to_string();
                t.push('…');
                return Cow::Owned(t);
            }
        }
        Cow::Borrowed(s)
    }
}

#[cfg(target_os = "android")]
fn truncate_chars(s: &str, max: usize) -> String {
    for (count, (i, _)) in s.char_indices().enumerate() {
        if count >= max {
            let mut t = s[..i].to_string();
            t.push('…');
            return t;
        }
    }
    s.to_string()
}

/// Strip glyphs Fira Sans cannot render on Android (emoji, UHD superscripts, dingbats).
#[cfg(target_os = "android")]
fn sanitize_android_ui_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        let keep = matches!(c, ' '..='~' | '\u{00A0}'..='\u{024F}')
            || matches!(
                c,
                '·' | '•' | '…' | '—' | '–' | '«' | '»' | '€' | '°' | '×' | '÷'
            );
        let ch = if keep { c } else { ' ' };
        if ch == ' ' {
            if prev_space || out.is_empty() {
                continue;
            }
            prev_space = true;
            out.push(' ');
        } else {
            prev_space = false;
            out.push(ch);
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

#[allow(dead_code)]
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

/// Color name and a round sample, sitting at the end of a search bar.
pub fn accent_suffix<'a>(ui: UiTheme) -> Element<'a, Message> {
    row![
        text(ui.accent_label())
            .size(13)
            .color(ui.on_surface_variant()),
        color_dot(ui.primary(), 14.0),
    ]
    .spacing(SPACE_XS)
    .align_y(Alignment::Center)
    .into()
}

fn color_dot(sample: Color, side: f32) -> Element<'static, Message> {
    container(Space::new().width(side).height(side))
        .width(Length::Fixed(side))
        .height(Length::Fixed(side))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(sample)),
            border: Border {
                radius: RADIUS_FULL.into(),
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.18),
                width: 1.0,
            },
            ..Default::default()
        })
        .into()
}

/// Searchable color menu. The closed bar ends with the active name and a sample.
pub fn accent_menu<'a>(
    ui: UiTheme,
    open: bool,
    query: &'a str,
    picker_open: bool,
    cursor: (f32, f32),
) -> Element<'a, Message> {
    let sample = ui.primary();
    let bar = mouse_area(
        container(
            row![
                container(crate::icons::icon(
                    if open {
                        crate::icons::Icon::ExpandMore
                    } else {
                        crate::icons::Icon::ChevronRight
                    },
                    20.0,
                    ui.primary(),
                ))
                .width(Length::Fixed(20.0))
                .center_y(Length::Fixed(20.0)),
                text(if open {
                    "Choisir une couleur"
                } else {
                    "Couleur"
                })
                .size(14)
                .color(ui.on_surface_variant()),
                Space::new().width(Fill),
                accent_suffix(ui),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .padding(Padding::from([10, 14]))
            .width(Fill),
        )
        .width(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_highest())),
            border: Border {
                radius: RADIUS_FULL.into(),
                color: if open { sample } else { ui.outline() },
                width: if open { 2.0 } else { 1.0 },
            },
            ..Default::default()
        }),
    )
    .on_press(Message::ToggleAccentMenu);

    let mut col = Column::new().spacing(SPACE_SM).push(bar).width(Fill);
    if open {
        let q = query.trim().to_lowercase();
        let mut presets: Vec<_> = fluxplay_core::models::AccentPreset::all().to_vec();
        presets.sort_by(|a, b| {
            let lum = |p| {
                let c = UiTheme::new(ui.day, p).primary();
                0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
            };
            lum(*a)
                .partial_cmp(&lum(*b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let matches: Vec<_> = presets
            .into_iter()
            .filter(|p| q.is_empty() || p.label().to_lowercase().contains(&q))
            .take(if q.is_empty() { 8 } else { 14 })
            .collect();
        col = col.push(
            text_input("Rechercher une couleur…", query)
                .on_input(Message::AccentQuery)
                .padding(12)
                .size(TYPE_BODY_M)
                .width(Fill),
        );
        let qn = q.replace(['é', 'è'], "e");
        if qn.is_empty() || "personnalise".contains(qn.as_str()) {
            let active = ui.custom.is_some();
            col = col.push(
                mouse_area(
                    container(
                        row![
                            text("Personnalisé")
                                .size(15)
                                .color(if active { ui.ink() } else { ui.on_surface_variant() })
                                .width(Fill),
                            color_dot(
                                ui.custom
                                    .map(|[r, g, b]| Color::from_rgb8(r, g, b))
                                    .unwrap_or(ui.primary()),
                                16.0,
                            ),
                        ]
                        .align_y(Alignment::Center)
                        .padding(Padding::from([8, 12]))
                        .width(Fill),
                    )
                    .width(Fill)
                    .style(move |_t: &Theme| container::Style {
                        background: Some(Background::Color(if active {
                            ui.surface_container_high()
                        } else {
                            Color::TRANSPARENT
                        })),
                        border: Border {
                            radius: RADIUS_MD.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                )
                .on_press(Message::ToggleCustomColor),
            );
            if picker_open {
                col = col.push(accent_picker(ui, cursor));
            }
        }
        if matches.is_empty() && !qn.is_empty() && !"personnalise".contains(qn.as_str()) {
            col = col.push(
                text("Aucune couleur pour cette recherche.")
                    .size(13)
                    .color(ui.ink_muted()),
            );
        }
        for preset in matches {
            let swatch = UiTheme::new(ui.day, preset).primary();
            let active = ui.custom.is_none() && preset == ui.accent;
            col = col.push(
                mouse_area(
                    container(
                        row![
                            text(preset.label())
                                .size(15)
                                .color(if active { ui.ink() } else { ui.on_surface_variant() })
                                .width(Fill),
                            color_dot(swatch, 16.0),
                        ]
                        .align_y(Alignment::Center)
                        .padding(Padding::from([8, 12]))
                        .width(Fill),
                    )
                    .width(Fill)
                    .style(move |_t: &Theme| container::Style {
                        background: Some(Background::Color(if active {
                            ui.surface_container_high()
                        } else {
                            Color::TRANSPARENT
                        })),
                        border: Border {
                            radius: RADIUS_MD.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                )
                .on_press(Message::SetAccent(preset)),
            );
        }
    }
    col.into()
}

fn ink_dot(rgb: [u8; 3], on: bool, ui: UiTheme) -> Element<'static, Message> {
    container(Space::new().width(14.0).height(14.0))
        .width(Length::Fixed(18.0))
        .height(Length::Fixed(18.0))
        .center_x(Length::Fixed(18.0))
        .center_y(Length::Fixed(18.0))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(Color::from_rgb8(rgb[0], rgb[1], rgb[2]))),
            border: Border {
                color: if on { ui.primary() } else { ui.outline_variant() },
                width: if on { 2.0 } else { 1.0 },
                radius: RADIUS_FULL.into(),
            },
            ..Default::default()
        })
        .into()
}

/// Text color: a few readable presets, plus the same gradient square.
pub fn ink_menu<'a>(
    ui: UiTheme,
    current: Option<[u8; 3]>,
    picker_open: bool,
    cursor: (f32, f32),
) -> Element<'a, Message> {
    let default_rgb = if ui.day {
        [0x0F, 0x17, 0x22]
    } else {
        [0xD0, 0xD7, 0xE0]
    };
    let presets: [(&str, Option<[u8; 3]>); 3] = [
        ("Défaut", None),
        ("Blanc", Some([0xF4, 0xF6, 0xF8])),
        ("Gris", Some([0x9A, 0xA3, 0xB0])),
    ];
    let custom = current.is_some() && presets.iter().all(|(_, rgb)| *rgb != current);
    let mut choices = Row::new().spacing(8).align_y(Alignment::Center);
    for (label, rgb) in presets {
        let sample = rgb.unwrap_or(default_rgb);
        let on = current == rgb;
        choices = choices.push(
            mouse_area(
                container(
                    row![
                        ink_dot(sample, on, ui),
                        text(label).size(13).color(ui.on_surface()),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([6, 10]))
                .style(move |_t: &Theme| container::Style {
                    background: Some(Background::Color(if on {
                        ui.primary_container()
                    } else {
                        ui.surface_container()
                    })),
                    border: Border {
                        color: if on { ui.primary() } else { ui.outline_variant() },
                        width: 1.0,
                        radius: RADIUS_FULL.into(),
                    },
                    ..Default::default()
                }),
            )
            .on_press(Message::SetInk(rgb)),
        );
    }
    let sample = current.filter(|_| custom).unwrap_or(default_rgb);
    choices = choices.push(
        mouse_area(
            container(
                row![
                    ink_dot(sample, custom || picker_open, ui),
                    text("Personnalisé")
                        .size(13)
                        .color(ui.on_surface()),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([6, 10]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(if custom || picker_open {
                    ui.primary_container()
                } else {
                    ui.surface_container()
                })),
                border: Border {
                    color: if custom || picker_open {
                        ui.primary()
                    } else {
                        ui.outline_variant()
                    },
                    width: 1.0,
                    radius: RADIUS_FULL.into(),
                },
                ..Default::default()
            }),
        )
        .on_press(Message::ToggleInkPicker),
    );
    let mut col = column![
        text("Police").size(13).color(ui.on_surface_variant()),
        choices,
    ]
    .spacing(6)
    .width(Fill);
    if picker_open {
        col = col.push(accent_picker(ui, cursor));
    }
    col.into()
}

/// Hue left to right, brightness top to bottom. Painted as cells because a
/// generated image stayed invisible under the desktop theme.
fn spectrum_grid<'a>() -> Element<'a, Message> {
    const COLS: usize = 22;
    const ROWS: usize = 22;
    const CELL: f32 = 10.0;
    let mut grid = Column::new().spacing(0).width(Length::Shrink);
    for y in 0..ROWS {
        let ny = y as f32 / (ROWS - 1) as f32;
        let mut line = Row::new().spacing(0);
        for x in 0..COLS {
            let nx = x as f32 / (COLS - 1) as f32;
            let [r, g, b] = crate::theme::pick_color(nx, ny);
            let sample = Color::from_rgb8(r, g, b);
            line = line.push(
                container(Space::new().width(CELL).height(CELL))
                    .width(Length::Fixed(CELL))
                    .height(Length::Fixed(CELL))
                    .style(move |_t: &Theme| container::Style {
                        background: Some(Background::Color(sample)),
                        ..Default::default()
                    }),
            );
        }
        grid = grid.push(line);
    }
    grid.into()
}

/// Gradient square. Drag the ring to pick a color.
fn accent_picker<'a>(ui: UiTheme, cursor: (f32, f32)) -> Element<'a, Message> {
    let side = crate::theme::ACCENT_PICK;
    let dot = 16.0;
    let (nx, ny) = cursor;
    let cx = (nx.clamp(0.0, 1.0) * side - dot / 2.0).clamp(0.0, side - dot);
    let cy = (ny.clamp(0.0, 1.0) * side - dot / 2.0).clamp(0.0, side - dot);
    let ring = container(Space::new().width(dot - 6.0).height(dot - 6.0))
        .width(Length::Fixed(dot))
        .height(Length::Fixed(dot))
        .padding(2)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(Color::TRANSPARENT)),
            border: Border {
                color: Color::WHITE,
                width: 2.0,
                radius: RADIUS_FULL.into(),
            },
            ..Default::default()
        });
    let cursor_layer = column![
        Space::new().height(Length::Fixed(cy)),
        row![Space::new().width(Length::Fixed(cx)), ring].height(Length::Fixed(dot)),
    ]
    .width(Length::Fixed(side))
    .height(Length::Fixed(side));
    let square = stack![spectrum_grid(), cursor_layer]
        .width(Length::Fixed(side))
        .height(Length::Fixed(side));
    column![
        mouse_area(square)
            .on_move(move |p| {
                Message::AccentPoint(
                    (p.x / side).clamp(0.0, 1.0),
                    (p.y / side).clamp(0.0, 1.0),
                )
            })
            .on_press(Message::AccentDrag(true))
            .on_release(Message::AccentDrag(false))
            .interaction(mouse::Interaction::Crosshair),
        text("Glisse le curseur pour la couleur")
            .size(12)
            .color(ui.ink_muted()),
    ]
    .spacing(6)
    .width(Fill)
    .into()
}

/// Compact accent tile for the settings mosaic (fixed size — never Fill).
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Tonal pill for the download action (detail hero, or `compact` on episode rows).
pub fn download_pill<'a>(
    ui: UiTheme,
    label: String,
    msg: Message,
    compact: bool,
) -> Element<'a, Message> {
    let (size, pad) = if compact {
        (12, Padding::from([8, 12]))
    } else {
        (14, Padding::from([12, 16]))
    };
    mouse_area(
        container(text(label).size(size).color(ui.on_surface()))
            .padding(pad)
            .width(Length::Shrink)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container())),
                border: Border {
                    radius: radius_fab(),
                    color: ui.primary(),
                    width: 1.5,
                },
                ..Default::default()
            }),
    )
    .on_press(msg)
    .into()
}

/// Heading + body used under the episode list (synopsis, facts).
pub fn copy_section<'a>(ui: UiTheme, heading: &str, body: &str) -> Element<'a, Message> {
    section_block(ui, heading, body)
}

/// One person on the cast row: headshot when we have one, otherwise their initial.
pub fn people_block<'a>(
    ui: UiTheme,
    heading: &str,
    people: Vec<(String, Option<&'a Handle>)>,
) -> Element<'a, Message> {
    let mut chips: Vec<Element<'a, Message>> = Vec::new();
    for (name, photo) in people {
        let letter = name
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .unwrap_or('#');
        let art: Element<'a, Message> = if let Some(handle) = photo {
            framed_image(handle, 84.0, 112.0, radius_poster(), ui.surface_muted())
        } else {
            container(text(letter.to_string()).size(22).color(ui.ink_muted()))
                .width(Length::Fixed(84.0))
                .height(Length::Fixed(112.0))
                .center_x(Length::Fixed(84.0))
                .center_y(Length::Fixed(112.0))
                .style(move |_t: &Theme| container::Style {
                    background: Some(Background::Color(ui.surface_container_high())),
                    border: Border {
                        radius: radius_poster(),
                        color: ui.outline_variant(),
                        width: 1.0,
                    },
                    ..Default::default()
                })
                .into()
        };
        let card = column![
            art,
            container(text(name.clone()).size(13).color(ui.ink()))
                .width(Length::Fixed(84.0))
                .center_x(Length::Fixed(84.0)),
        ]
        .spacing(SPACE_XS)
        .width(Length::Fixed(84.0))
        .align_x(Alignment::Center);
        chips.push(mouse_area(card).on_press(Message::OpenActor(name)).into());
    }
    column![
        section_heading(heading),
        container(Row::with_children(chips).spacing(SPACE_SM).wrap())
            .width(Fill)
            .align_x(Alignment::Center),
    ]
    .spacing(SPACE_SM)
    .width(Fill)
    .into()
}

/// Duration, air date, star rating and recommended age shown under an episode synopsis.
#[derive(Clone, Default)]
pub struct EpisodeFacts {
    pub duration: Option<String>,
    pub date: Option<String>,
    pub stars: Option<String>,
    pub age: Option<String>,
}

/// Outline control under a grid. Loads the next block of titles, nothing else.
pub fn load_more_row<'a>(ui: UiTheme, left: usize) -> Element<'a, Message> {
    let label = format!("Voir plus · {left}");
    container(
        mouse_area(
            container(
                text(label)
                    .size(16)
                    .font(type_font(true))
                    .color(ui.on_surface()),
            )
            .padding(Padding {
                top: 10.0,
                right: 22.0,
                bottom: 10.0,
                left: 22.0,
            })
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container())),
                border: Border {
                    color: ui.primary(),
                    width: 1.5,
                    radius: RADIUS_FULL.into(),
                },
                ..Default::default()
            }),
        )
        .on_press(Message::LoadMore),
    )
    .width(Length::Fill)
    .align_x(Alignment::Center)
    .padding(Padding {
        top: 12.0,
        right: 0.0,
        bottom: 8.0,
        left: 0.0,
    })
    .into()
}

/// Outline until the title is a favorite, then a filled heart in the chosen theme color.
fn favorite_mark<'a>(ui: UiTheme, is_fav: bool, size: f32) -> Element<'a, Message> {
    if is_fav {
        crate::icons::icon(crate::icons::Icon::FavoriteFilled, size, ui.primary())
    } else {
        let mut c = ui.on_surface_variant();
        c.a *= 0.55;
        crate::icons::icon(crate::icons::Icon::Favorite, size, c)
    }
}

fn episode_facts<'a>(ui: UiTheme, episode_num: u32, facts: &EpisodeFacts) -> Element<'a, Message> {
    let mut chips: Vec<Element<'a, Message>> = Vec::new();
    if let Some(duration) = facts.duration.clone() {
        chips.push(fact_chip(ui, "Durée", duration));
    }
    if let Some(date) = facts.date.clone() {
        chips.push(fact_chip(ui, "Date", date));
    }
    if let Some(stars) = facts.stars.clone() {
        chips.push(fact_chip(ui, "Note", format!("★ {stars}/10")));
    }
    if let Some(age) = facts.age.clone() {
        chips.push(fact_chip(ui, "Âge", age));
    }
    if chips.is_empty() {
        chips.push(fact_chip(ui, "Épisode", episode_num.to_string()));
    }
    Row::with_children(chips).spacing(SPACE_SM).wrap().into()
}

/// Theme color sits on the outline. The star text stays the normal ink.
fn fact_chip<'a>(ui: UiTheme, label: &str, value: String) -> Element<'a, Message> {
    container(
        row![
            text(label.to_string()).size(13).color(ui.ink_muted()),
            text(value).size(16).color(ui.on_surface()),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([6, 10]))
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(ui.surface_container())),
        border: Border {
            radius: RADIUS_FULL.into(),
            color: ui.primary(),
            width: 1.5,
        },
        ..Default::default()
    })
    .into()
}

/// Thin bar along the bottom of the still. Hidden until a few percent are watched.
fn episode_progress<'a>(
    art: Element<'a, Message>,
    w: f32,
    h: f32,
    watched: Option<f32>,
    ui: UiTheme,
) -> Element<'a, Message> {
    let Some(ratio) = watched.filter(|r| *r >= 0.03) else {
        return art;
    };
    let inset = 8.0;
    let bar_w = ((w - inset * 2.0) * ratio).clamp(6.0, w - inset * 2.0);
    let fill = container(Space::new())
        .width(Length::Fixed(bar_w))
        .height(Length::Fixed(3.0))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.primary())),
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    let layer = container(fill)
        .width(Length::Fixed(w))
        .height(Length::Fixed(h))
        .align_x(Alignment::Start)
        .align_y(Alignment::End)
        .padding(Padding {
            top: 0.0,
            right: inset,
            bottom: 7.0,
            left: inset,
        });
    container(
        stack![art, layer]
            .width(Length::Fixed(w))
            .height(Length::Fixed(h)),
    )
    .width(Length::Fixed(w))
    .height(Length::Fixed(h))
    .clip(true)
    .into()
}

/// One episode: still (or the series poster), title, synopsis, then runtime / date.
/// The download control sits on the bottom line so it does not squeeze the text.
pub fn episode_card<'a>(
    ui: UiTheme,
    still: Option<&'a Handle>,
    episode_num: u32,
    title: String,
    plot: String,
    facts: EpisodeFacts,
    hover_key: String,
    hovered: bool,
    play: Message,
    download: Option<(String, Message)>,
    narrow: bool,
    watched: Option<f32>,
) -> Element<'a, Message> {
    let (iw, ih) = if narrow { (148.0, 84.0) } else { (240.0, 135.0) };
    let poster: Element<'a, Message> = if let Some(handle) = still {
        framed_image(handle, iw, ih, RADIUS_MD, ui.surface_muted())
    } else {
        container(text(format!("E{episode_num}")).size(18).color(ui.ink_muted()))
            .width(Length::Fixed(iw))
            .height(Length::Fixed(ih))
            .center_x(Length::Fixed(iw))
            .center_y(Length::Fixed(ih))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container_highest())),
                border: Border {
                    radius: RADIUS_MD.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into()
    };
    let art: Element<'a, Message> = if hovered {
        let mark = container(crate::icons::icon(crate::icons::Icon::Play, 22.0, Color::WHITE))
            .width(Length::Fixed(40.0))
            .height(Length::Fixed(40.0))
            .center_x(Length::Fixed(40.0))
            .center_y(Length::Fixed(40.0))
            .style(|_t: &Theme| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.55))),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.85),
                    width: 1.0,
                },
                ..Default::default()
            });
        stack![
            poster,
            container(mark)
                .width(Length::Fixed(iw))
                .height(Length::Fixed(ih))
                .center_x(Length::Fixed(iw))
                .center_y(Length::Fixed(ih))
                .style(|_t: &Theme| container::Style {
                    background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.22))),
                    border: Border {
                        radius: RADIUS_MD.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        ]
        .width(Length::Fixed(iw))
        .height(Length::Fixed(ih))
        .into()
    } else {
        poster
    };
    let art = episode_progress(art, iw, ih, watched, ui);
    let mut facts_row = row![container(episode_facts(ui, episode_num, &facts)).width(Fill)]
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
        .width(Fill);
    if let Some((dl_label, dl_msg)) = download {
        facts_row = facts_row.push(download_pill(ui, dl_label, dl_msg, true));
    }
    let copy = column![
        mouse_area(
            column![
                text(title).size(16).color(ui.on_surface()),
                text(plot).size(15).color(ui.ink_muted()),
            ]
            .spacing(SPACE_XS)
            .width(Fill),
        )
        .on_press(play.clone()),
        facts_row,
    ]
    .spacing(SPACE_SM)
    .width(Fill);
    let played = mouse_area(art).on_press(play.clone());
    let inner: Element<'a, Message> = if narrow {
        column![played, copy].spacing(SPACE_SM).width(Fill).into()
    } else {
        row![played, copy]
            .spacing(SPACE_MD)
            .align_y(Alignment::Start)
            .width(Fill)
            .into()
    };
    mouse_area(
        container(inner)
            .padding(SPACE_SM)
            .width(Fill)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container_low())),
                border: Border {
                    radius: RADIUS_LG.into(),
                    width: 0.0,
                    ..Default::default()
                },
                ..Default::default()
            }),
    )
    .on_enter(Message::EpisodeHover {
        key: hover_key.clone(),
        on: true,
    })
    .on_exit(Message::EpisodeHover {
        key: hover_key,
        on: false,
    })
    .on_press(play)
    .into()
}

/// Closed season bar. `choices`, when set, are the open menu rows under it.
pub fn season_bar<'a>(
    ui: UiTheme,
    label: String,
    open: bool,
    download: Option<(String, Message)>,
) -> Element<'a, Message> {
    let mark = crate::icons::icon(
        if open {
            crate::icons::Icon::ExpandMore
        } else {
            crate::icons::Icon::ChevronRight
        },
        20.0,
        ui.primary(),
    );
    let opener: Element<'a, Message> = mouse_area(
        container(
            row![
                mark,
                text(label).size(16).color(ui.on_surface()),
            ]
            .spacing(SPACE_SM)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([10, 14]))
        .width(Fill),
    )
    .on_press(Message::ToggleSeasonMenu)
    .into();
    let mut bar = row![opener]
        .spacing(SPACE_SM)
        .align_y(Alignment::Center)
        .width(Fill);
    if let Some((dl, msg)) = download {
        bar = bar.push(download_pill(ui, dl, msg, true));
    }
    container(bar)
        .padding(Padding::from([0, 4]))
        .width(Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_high())),
            border: Border {
                radius: RADIUS_MD.into(),
                width: 0.0,
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// One row inside the open season menu.
pub fn season_choice<'a>(ui: UiTheme, label: String, season: u32, active: bool) -> Element<'a, Message> {
    let fg = ui.on_surface();
    mouse_area(
        container(text(label).size(15).color(fg))
            .padding(Padding::from([8, 16]))
            .width(Fill)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container())),
                border: Border {
                    color: if active { ui.primary() } else { Color::TRANSPARENT },
                    width: if active { 1.5 } else { 0.0 },
                    radius: RADIUS_MD.into(),
                },
                ..Default::default()
            }),
    )
    .on_press(Message::SelectSeason(season))
    .into()
}

/// Film / series detail: poster, synopsis, cast, primary play, optional episodes.
// Detail-page renderer: args are independent view slots; grouping would just move the noise.
#[allow(clippy::too_many_arguments)]
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
    // Label follows the download state (Télécharger / 42 % · Annuler / Ouvrir le dossier).
    // The path string is unused on the film page: the synopsis takes that spot.
    download: Option<(String, Message, String)>,
    back: Message,
    episodes: Option<Element<'a, Message>>,
    narrow: bool,
    loading_meta: bool,
    // Extra facts under the crew (Pays, Langue, Récompenses).
    facts: Vec<(&'static str, String)>,
    // Shown under the synopsis ("Traduit automatiquement de l’anglais", "Traduction…").
    plot_note: Option<String>,
    // Photo cast (actors + crew). Replaces the text chips when set.
    people: Option<Element<'a, Message>>,
    // Series keeps the extra facts under the episode list. Synopsis and cast stay up top.
    defer_copy: bool,
    // Stays above the episode list: "Saison 1" and the open season menu.
    season_picker: Option<Element<'a, Message>>,
    // Heart next to play. Filled with the theme color when the title is a favorite.
    favorite: Option<(bool, Message)>,
    // Bell: tell the viewer when a new episode of this series is out.
    alert: Option<(bool, Message)>,
) -> Element<'a, Message> {
    let poster_w = if narrow { 140.0 } else { 200.0 };
    let poster_h = poster_w * 1.5;

    let art: Element<'_, Message> = match poster {
        Some(h) => framed_image(h, poster_w, poster_h, radius_poster(), ui.surface_muted()),
        None => container(
            text("Pas d’affiche")
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
    if let Some(r) = rating.filter(|s| !s.is_empty()) {
        #[cfg(target_os = "android")]
        meta_bits.push(format!("* {r}/10"));
        #[cfg(not(target_os = "android"))]
        meta_bits.push(format!("★ {r}/10"));
    }
    if let Some(rt) = runtime.filter(|s| !s.is_empty()) {
        meta_bits.push(rt.to_string());
    }
    let meta_line = meta_bits.join(" · ");
    let age = crate::metadata::age_rating(rated);

    let mut info = Column::new().spacing(SPACE_SM).width(Fill);
    info = info.push(text(title.to_string()).size(if narrow { 20 } else { 26 }));
    if !meta_line.is_empty() {
        info = info.push(text(meta_line).size(15).color(ui.ink_muted()));
    }
    if let Some(g) = genre.filter(|s| !s.is_empty()) {
        let budget = if narrow { 280.0 } else { 720.0 };
        info = info.push(
            container(text(fit_one_line(g, budget, 16.0)).size(16).color(ui.on_surface()))
                .width(Fill)
                .height(Length::Fixed(22.0))
                .clip(true),
        );
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
        // Primary CTA — Shrink so wrap/Fill parents never stretch it full-width.
        action_els.push(
            mouse_area(
                container(text(label).size(15).color(ui.on_surface()))
                    .padding(Padding::from([12, 20]))
                    .width(Length::Shrink)
                    .style(move |_t: &Theme| container::Style {
                        background: Some(Background::Color(ui.surface_container())),
                        border: Border {
                            radius: radius_fab(),
                            color: ui.primary(),
                            width: 1.5,
                        },
                        ..Default::default()
                    }),
            )
            .on_press(msg)
            .into(),
        );
    }
    if let Some((on, msg)) = favorite {
        action_els.push(
            mouse_area(
                container(favorite_mark(ui, on, 20.0))
                    .width(Length::Fixed(46.0))
                    .height(Length::Fixed(46.0))
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center)
                    .style(move |_t: &Theme| container::Style {
                        // The disk stays the surface color. Only the heart glyph takes the accent.
                        background: Some(Background::Color(ui.surface_container())),
                        border: Border {
                            radius: radius_fab(),
                            color: ui.primary(),
                            width: 1.5,
                        },
                        ..Default::default()
                    }),
            )
            .on_press(msg)
            .into(),
        );
    }
    if let Some((on, msg)) = alert {
        let ink = if on { ui.primary() } else { ui.on_surface() };
        action_els.push(
            mouse_area(
                container(crate::icons::icon(crate::icons::Icon::Bell, 20.0, ink))
                    .width(Length::Fixed(46.0))
                    .height(Length::Fixed(46.0))
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center)
                    .style(move |_t: &Theme| container::Style {
                        background: Some(Background::Color(ui.surface_container())),
                        border: Border {
                            radius: radius_fab(),
                            color: if on { ui.primary() } else { ui.outline_variant() },
                            width: if on { 1.5 } else { 1.0 },
                        },
                        ..Default::default()
                    }),
            )
            .on_press(msg)
            .into(),
        );
    }
    if let Some((label, msg, _hint)) = download {
        let folder = label.to_lowercase().contains("dossier");
        if !(defer_copy && folder) {
            action_els.push(download_pill(ui, label, msg, false));
        }
    }
    if !defer_copy {
        let imdb_label = imdb_id
            .filter(|s| !s.is_empty())
            .map(|id| format!("IMDb · {id}"))
            .unwrap_or_else(|| "IMDb".into());
        action_els.push(
            mouse_area(
                container(
                    text(imdb_label)
                        .size(16)
                        .font(type_font(true))
                        .color(ui.on_surface()),
                )
                .padding(Padding::from([10, 14]))
                .style(move |_t: &Theme| container::Style {
                    background: Some(Background::Color(ui.surface_container())),
                    border: Border {
                        radius: RADIUS_FULL.into(),
                        color: ui.primary(),
                        width: 1.5,
                    },
                    ..Default::default()
                }),
            )
            .on_press(Message::OpenImdb(imdb_query))
            .into(),
        );
    }
    if let Some(age) = age.as_ref() {
        let label = age.label.clone();
        action_els.push(
            container(
                row![
                    text("Âge").size(15).color(ui.ink_muted()),
                    text(label)
                        .size(18)
                        .font(type_font(true))
                        .color(ui.on_surface()),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([10, 14]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: ui.primary(),
                    width: 1.5,
                },
                ..Default::default()
            })
            .into(),
        );
    }
    info = info.push(Row::with_children(action_els).spacing(SPACE_SM).wrap());
    info = info.push(Space::new().height(14.0));
    // The paragraph beside the poster is the synopsis; it does not need a title.
    let plot_text = plot
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(if loading_meta {
            "Chargement de la fiche (panel / IMDb)…"
        } else {
            "Synopsis indisponible pour ce titre."
        });
    info = info.push(text(plot_text.to_string()).size(16).color(ui.ink_muted()).width(Fill));
    if let Some(note) = plot_note.as_deref() {
        info = info.push(text(note.to_string()).size(11).color(ui.ink_muted()));
    }

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
    let cast = people;

    if cast.is_none() && !defer_copy {
        if let Some(a) = actors.filter(|s| !s.trim().is_empty()) {
            body = body.push(actor_block(ui, a));
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
    }
    if !defer_copy {
        let facts: Vec<String> = facts
            .into_iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, v)| format!("{k} · {v}"))
            .collect();
        if !facts.is_empty() {
            body = body.push(section_block(ui, "Informations", &facts.join("\n")));
        }
    } else {
        let _ = (actors, director, writer, facts);
    }

    let back = mouse_area(
        container(text("<- Catalogue").size(13).color(ui.ink()))
            .padding(Padding::from([10, 16]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container_low())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: ui.primary(),
                    width: 1.5,
                },
                ..Default::default()
            }),
    )
    .on_press(back);

    // One scroller for the whole series page. A nested episode list trapped
    // the wheel, so the poster and the episodes could not be scrolled together.
    let page: Element<'a, Message> = if let Some(eps) = episodes {
        let mut page = Column::new().spacing(SPACE_MD).width(Fill);
        page = page.push(back);
        page = page.push(body.padding(Padding::from([0, 4])).width(Fill));
        page = page.push(section_heading("Épisodes"));
        if let Some(picker) = season_picker {
            page = page.push(picker);
        }
        page = page.push(eps);
        if let Some(cast) = cast {
            page = page.push(cast);
        }
        soft_scroll(ui, page).into()
    } else {
        if let Some(cast) = cast {
            body = body.push(cast);
        }
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

fn section_heading<'a>(heading: &str) -> Element<'a, Message> {
    container(text(heading.to_string()).size(20))
        .center_x(Fill)
        .into()
}

fn section_block<'a>(ui: UiTheme, heading: &str, body: &str) -> Element<'a, Message> {
    column![
        section_heading(heading),
        text(body.to_string()).size(16).color(ui.ink_muted()),
    ]
    .spacing(SPACE_XS)
    .width(Fill)
    .into()
}

fn actor_block<'a>(ui: UiTheme, actors: &str) -> Element<'a, Message> {
    let mut chips: Vec<Element<'a, Message>> = Vec::new();
    for name in actors.split(',') {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let label = name.to_string();
        chips.push(
            mouse_area(
                container(text(label.clone()).size(16).color(ui.ink()))
                    .padding(Padding::from([8, 14]))
                    .style(move |_t: &Theme| container::Style {
                        background: Some(Background::Color(ui.surface_container_high())),
                        border: Border {
                            radius: RADIUS_FULL.into(),
                            color: ui.outline_variant(),
                            width: 1.0,
                        },
                        ..Default::default()
                    }),
            )
            .on_press(Message::OpenActor(label))
            .into(),
        );
    }
    column![
        section_heading("Acteurs"),
        Row::with_children(chips).spacing(SPACE_SM).wrap(),
    ]
    .spacing(SPACE_SM)
    .width(Fill)
    .into()
}

/// Actor sheet: headshot, known-for line, other titles (poster when Wikidata has one).
pub fn actor_page<'a>(
    ui: UiTheme,
    name: &str,
    loading: bool,
    card: Option<&crate::metadata::ActorCard>,
    mut image_for: impl FnMut(&str) -> Option<&'a Handle>,
) -> Element<'a, Message> {
    let shown = card.map(|c| c.name.as_str()).unwrap_or(name);
    let mut body = Column::new().spacing(SPACE_MD).width(Fill);
    let photo = card.and_then(|c| c.photo.as_deref()).and_then(&mut image_for);
    let art: Element<'a, Message> = if let Some(handle) = photo {
        container(framed_image(handle, 140.0, 180.0, radius_poster(), ui.surface_muted()))
            .center_x(Fill)
            .into()
    } else {
        let letter = shown
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .unwrap_or('#');
        container(
            container(text(letter.to_string()).size(36).color(ui.ink_muted()))
                .width(Length::Fixed(140.0))
                .height(Length::Fixed(180.0))
                .center_x(Length::Fixed(140.0))
                .center_y(Length::Fixed(180.0))
                .style(move |_t: &Theme| container::Style {
                    background: Some(Background::Color(ui.surface_container_high())),
                    border: Border {
                        radius: radius_poster(),
                        color: ui.outline_variant(),
                        width: 1.0,
                    },
                    ..Default::default()
                }),
        )
        .center_x(Fill)
        .into()
    };
    body = body.push(art);
    body = body.push(
        container(text(shown.to_string()).size(26))
            .center_x(Fill),
    );
    if let Some(known) = card.and_then(|c| c.known_for.as_deref()) {
        body = body.push(
            container(text(known.to_string()).size(16).color(ui.on_surface_variant()))
                .center_x(Fill),
        );
    }
    if loading {
        body = body.push(
            container(text("Recherche des autres apparitions…").size(15).color(ui.ink_muted()))
                .center_x(Fill),
        );
    }
    body = body.push(section_heading("Autres apparitions"));
    let credits = card.map(|c| c.credits.as_slice()).unwrap_or(&[]);
    if credits.is_empty() && !loading {
        body = body.push(
            container(
                text("Aucune autre apparition trouvée pour ce nom.")
                    .size(16)
                    .color(ui.ink_muted()),
            )
            .center_x(Fill),
        );
    } else {
        let tile_w = 140.0;
        let mut tiles: Vec<Element<'a, Message>> = Vec::new();
        for credit in credits {
            let year = credit.year.clone().unwrap_or_default();
            let meta = if credit.in_catalog {
                if year.is_empty() {
                    "Catalogue".into()
                } else {
                    format!("{year}\nCatalogue")
                }
            } else if year.is_empty() {
                String::new()
            } else {
                year
            };
            let thumb = credit.poster.as_deref().and_then(&mut image_for);
            let title = credit.title.clone();
            let imdb = credit.imdb_id.clone();
            tiles.push(mosaic_tile(
                title.clone(),
                meta,
                title.clone(),
                Message::OpenActorTitle { title, imdb },
                ui,
                tile_w,
                thumb,
                false,
            ));
        }
        body = body.push(
            Row::with_children(tiles)
                .spacing(MOSAIC_GAP)
                .align_y(Alignment::Start)
                .width(Fill)
                .wrap(),
        );
    }

    let back = mouse_area(
        container(text("<- Fiche").size(14).color(ui.ink()))
            .padding(Padding::from([10, 16]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(ui.surface_container_low())),
                border: Border {
                    radius: RADIUS_FULL.into(),
                    color: ui.primary(),
                    width: 1.5,
                },
                ..Default::default()
            }),
    )
    .on_press(Message::CloseActor);

    pane(
        ui,
        Length::Fill,
        column![back, soft_scroll(ui, body.width(Fill))]
            .spacing(SPACE_MD)
            .width(Fill)
            .height(Fill),
    )
}

