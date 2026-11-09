use chrono::Local;
#[cfg(target_os = "android")]
use fluxplay_core::models::AndroidPresentPref;
use fluxplay_core::models::{
    AccentPreset, AppSettings, AspectPref, Channel, ContentKind,
    DeinterlacePref, FpsCapPref, MediaSource, NetworkSettings, PlaylistBundle, PlayerBackendPref,
    SeriesItem, SourceKind, ThemeMode, UpscalePref, VideoQualityPref, VodItem,
};
use std::sync::Arc;
use fluxplay_player::{
    detect_backends, target_profile, BackendId, PlayOptions, PlaybackState, StreamSession,
    VideoRect,
};
use iced::widget::{
    column, container, mouse_area, row, stack, text, text_input, Column, Row, Space,
};
use iced::widget::scrollable::AbsoluteOffset;
use iced::widget::image::{
    self as iced_image, Allocation as ImageAllocation, FilterMethod, Handle as ImageHandle,
};
use iced::window;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Point, Size, Subscription,
    Task, Theme,
};
use uuid::Uuid;

use crate::browser::{BrowseIndex, CAT_PAGE, LIST_PAGE, LIVE_PAGE};
use crate::player_ui::PlayerPanel;
use crate::theme::{
    type_style, LayoutMetrics, TypeRole, UiTheme, MOTION_COAST_FRICTION, RADIUS_LARGE, RADIUS_LARGE_INCREASED, RADIUS_MD, SPACE_SM,
};
use crate::{browser, demo, display_caps, player_ui, storage};
use crate::display_caps::{DisplayCaps, DisplayProbe};
use iced::event::{self, Event};
use iced::keyboard::{self, Key, Modifiers};
use iced::keyboard::key::Named;
#[cfg(target_os = "android")]
use std::sync::Mutex;

#[cfg(target_os = "android")]
static PENDING_CATALOG_BOOT: Mutex<Option<crate::catalog_db::CatalogDb>> = Mutex::new(None);

pub(crate) fn run_daemon() -> iced::Result {
    // Adapter pick happens before FluxPlay::new. Export the saved GPU first.
    let saved = crate::storage::load();
    export_gpu_env(&saved.settings.gpu_choice);
    iced::daemon(FluxPlay::new, FluxPlay::update, FluxPlay::view)
        .title(FluxPlay::title)
        .theme(FluxPlay::theme_for)
        .subscription(FluxPlay::subscription)
        .run()
}

#[cfg(target_os = "android")]
pub(crate) fn run_daemon_android(app: android_activity::AndroidApp) -> iced::Result {
    // Waydroid / many Android Vulkan surfaces reject the first iced wgpu adapter pick.
    std::env::set_var("WGPU_BACKEND", "gl");

    // iced Settings (docs): default_font + fira-sans feature → reliable glyphs on Android
    // (system font probing is incomplete under NativeActivity / cosmic-text).
    iced::application(FluxPlay::new, FluxPlay::update, FluxPlay::view_android)
        .title(FluxPlay::title_android)
        .theme(FluxPlay::theme_android)
        .style(FluxPlay::style_android)
        .subscription(FluxPlay::subscription)
        .antialiasing(false)
        .default_font(iced::Font::with_name("Fira Sans"))
        .window(window::Settings {
            // Activity / freeform bounds drive layout — avoid fake 1920×1080.
            // Do not request iced Fullscreen / maximize-to-monitor: Waydroid freeform
            // collapses the NativeActivity surface (Requested h=0).
            size: Size::new(360.0, 720.0),
            maximized: false,
            fullscreen: false,
            decorations: false,
            resizable: true,
            visible: true,
            exit_on_close_request: true,
            ..Default::default()
        })
        .run_android(app)
}

/// Quiet time after the last scroll event before a fling counts as stopped.
const BROWSE_SETTLE_AFTER: std::time::Duration = std::time::Duration::from_millis(180);

/// Window / taskbar icon (the `.exe` icon itself is embedded by `build.rs`).
#[cfg(not(target_os = "android"))]
fn app_icon() -> Option<window::Icon> {
    window::icon::from_file_data(include_bytes!("../assets/fluxplay-256.png"), None).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Live,
    Favorites,
    Vod,
    Series,
    Epg,
    /// Phone remote and shared screening room.
    Cinema,
    Sources,
    Settings,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Live => "Télévision",
            Self::Favorites => "Mes favoris",
            Self::Vod => "Films & VOD",
            Self::Series => "Séries",
            Self::Epg => "Guide TV",
            Self::Cinema => "Salle de cinéma",
            Self::Sources => "Mes sources",
            Self::Settings => "Réglages",
        }
    }

    /// Compact labels for phone top-nav (horizontal strip).
    fn short_label(self) -> &'static str {
        match self {
            Self::Live => "TV",
            Self::Favorites => "Fav",
            Self::Vod => "VOD",
            Self::Series => "Series",
            Self::Epg => "EPG",
            Self::Cinema => "Ciné",
            Self::Sources => "Src",
            Self::Settings => "Regl",
        }
    }

    fn icon(self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self {
            Self::Live => Icon::LiveTv,
            Self::Favorites => Icon::Favorite,
            Self::Vod => Icon::Movie,
            Self::Series => Icon::Series,
            Self::Epg => Icon::Epg,
            Self::Cinema => Icon::Fullscreen,
            Self::Sources => Icon::Sources,
            Self::Settings => Icon::Settings,
        }
    }

    fn all() -> &'static [Tab] {
        &[
            Tab::Live,
            Tab::Vod,
            Tab::Series,
            Tab::Epg,
            Tab::Cinema,
            Tab::Settings,
        ]
    }
}

/// What the open section is showing: the catalog, or that kind's history / favorites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shelf {
    Catalog,
    History,
    Favorites,
}

const SHELF_HISTORY: &str = "__history";
const SHELF_FAVORITES: &str = "__favorites";

/// One collapsible block on the settings page. Sources live here, not in the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    Appearance,
    Sources,
    Meta,
    Language,
    Network,
    Playback,
    Remote,
    Picture,
    Library,
    Backends,
}

impl SettingsSection {
    fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Apparence",
            Self::Sources => "Mes sources",
            Self::Meta => "API",
            Self::Language => "Langue",
            Self::Network => "Réseau",
            Self::Playback => "Lecture",
            Self::Remote => "Télécommande",
            Self::Picture => "Image",
            Self::Library => "Séries et téléchargements",
            Self::Backends => "Backends",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Appearance => "Thème et couleur, les plus sombres en premier.",
            Self::Sources => "Portails, playlists et profils.",
            Self::Meta => "Changer, effacer ou créer ton jeton.",
            Self::Language => "Langue affichée et traduction des fiches.",
            Self::Network => "DNS et tunnel WireGuard.",
            Self::Playback => "Décodeur, cache et reprise.",
            Self::Remote => "Téléphone ou tablette, sur ce réseau ou hors du réseau.",
            Self::Picture => "GPU, cadence, HDR et format.",
            Self::Library => "Âge, épisode suivant et dossier des téléchargements.",
            Self::Backends => "Décodeurs détectés sur cette machine.",
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::Appearance => "theme couleur accent sombre nuit jour",
            Self::Sources => "source playlist xtream m3u portail stalker xmltv profil",
            Self::Meta => "api tmdb omdb imdb cle jeton changer effacer assistant synopsis acteurs age",
            Self::Language => "langue traduction synopsis genre filtre",
            Self::Network => "dns wireguard vpn doh tunnel proxy",
            Self::Playback => "backend cache reprise volume hw latence lecteur",
            Self::Remote => "telecommande qr telephone tablette associer hors reseau",
            Self::Picture => "gpu hdr fps qualite upscale nuit format",
            Self::Library => "telechargement dossier precharge episode serie age enfant adulte masquer",
            Self::Backends => "decodeur mpv ffmpeg backend",
        }
    }
}

fn credit_key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn split_credits(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for part in raw.split([',', ';']) {
        let name = part.trim().trim_matches(|c: char| c == '"' || c == '\'');
        if name.is_empty()
            || name.eq_ignore_ascii_case("n/a")
            || name == "-"
            || name.eq_ignore_ascii_case("null")
        {
            continue;
        }
        if out.iter().any(|e: &String| e.eq_ignore_ascii_case(name)) {
            continue;
        }
        out.push(name.to_string());
        if out.len() >= 24 {
            break;
        }
    }
    out
}

fn season_title(n: u32, count: usize) -> String {
    let word = if count == 1 { "épisode" } else { "épisodes" };
    if n == 0 {
        format!("Spéciaux · {count} {word}")
    } else {
        format!("Saison {n} · {count} {word}")
    }
}

struct FluxPlay {
    settings: AppSettings,
    sources: Vec<MediaSource>,
    bundle: PlaylistBundle,
    tab: Tab,
    /// Catalog, or the history / favorites of the section that is open.
    shelf: Shelf,
    /// stream url → poster, filled when the history shelf opens.
    history_art: Vec<(String, String)>,
    search: String,
    /// Filters the settings menus so the page does not have to be scrolled open.
    settings_query: String,
    settings_open: Option<SettingsSection>,
    accent_open: bool,
    accent_query: String,
    /// Gradient picker unfolded under « Personnalisé ».
    custom_open: bool,
    /// Same square, writing the text color instead of the accent.
    ink_open: bool,
    /// Normalized position on the gradient square.
    accent_cursor: (f32, f32),
    accent_hover: (f32, f32),
    accent_dragging: bool,
    /// Category column. A tongue stays visible when this is false.
    cats_open: bool,
    selected_group: Option<String>,
    selected_channel: Option<String>,
    selected_vod_category: Option<String>,
    selected_series_category: Option<String>,
    series_detail: Option<SeriesItem>,
    /// Season shown on the series page. `None` until the detail has seasons.
    selected_season: Option<u32>,
    season_menu_open: bool,
    /// Episode card under the pointer, so its poster can show a play mark.
    episode_hover: Option<String>,
    /// Lowercased credit name → headshot URL (IMDb suggestion).
    cast_photos: std::collections::HashMap<String, String>,
    cast_photo_asked: std::collections::HashSet<String>,
    /// Series ids already asked for an age this session, including misses.
    age_asked: std::collections::HashSet<String>,
    age_lookup_busy: bool,
    vod_detail: Option<VodItem>,
    /// Actor sheet opened from a film or series page.
    actor_sheet: Option<ActorSheet>,
    /// Full IMDb/OMDb credits fetch in flight for the open detail page.
    detail_meta_loading: bool,
    /// Source text → text in `settings.pref_lang` (synopses, genres, episode plots).
    translations: std::collections::HashMap<String, String>,
    translating: std::collections::HashSet<String>,
    session: StreamSession,
    status: String,
    loading: bool,
    // Add-source form
    form_name: String,
    /// Inline validation message shown in the source form (status bar is easy to miss).
    form_error: Option<String>,
    form_kind: SourceKind,
    form_endpoint: String,
    form_user: String,
    form_pass: String,
    form_mac: String,
    form_epg: String,
    /// Extra servers of the profile, as typed (spaces / commas / new lines).
    form_mirrors: String,
    /// Profile being edited by the source form (`None` = new profile).
    editing_source: Option<Uuid>,
    form_omdb_key: String,
    form_tmdb_key: String,
    /// The API assistant is open. Not saved: it does not come back on the next launch.
    api_guide: bool,
    /// Settings draft for `settings.download_dir` (empty = default folder).
    form_download_dir: String,
    form_dns_servers: String,
    form_doh_url: String,
    form_dot_server: String,
    form_wg_paste: String,
    network_probe: String,
    system_dark: bool,
    cat_filter: String,
    /// Legacy page cap (pre–virtual-scroll). Kept for [`Message::LoadMore`] reserve path.
    #[allow(dead_code)]
    list_limit: usize,
    images: crate::images::ImageCache,
    catalog_db: Option<crate::catalog_db::CatalogDb>,
    autoplay_done: bool,
    main_id: Option<window::Id>,
    player_id: Option<window::Id>,
    /// Browser window is hidden and its grid is not built while a film or series plays.
    browse_parked: bool,
    /// On Android, player UI is shown inside the main window (no multi-window).
    #[cfg(target_os = "android")]
    player_embedded: bool,
    /// Logical size of the main browser window (updated on resize).
    main_size: Size,
    /// Cached layout metrics for `main_size` (invalidate on resize).
    layout_cache: Option<(Size, LayoutMetrics)>,
    /// Cached `detect_backends()` — probing PATH every Settings paint is expensive.
    backends_cache: Option<Vec<fluxplay_player::BackendInfo>>,
    /// System players (OS default first), refreshed with `backends_cache`.
    external_players: Vec<fluxplay_player::ExternalPlayer>,
    /// Last known outer position of the player window (legacy CLI overlay sizing).
    player_pos: Option<Point>,
    /// Last logical client size of the player window.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    player_logical_size: Size,
    /// Source display size the floating player was already fitted to.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    player_fitted_wh: Option<(u32, u32)>,
    /// Embedded video frame (libffmpeg/libmpv soft RGBA → iced image).
    video_frame: Option<ImageHandle>,
    /// Pins GPU atlas memory so the displayed frame never async-flickers.
    video_allocation: Option<ImageAllocation>,
    /// Previous atlas entries kept behind so iced never paints a freed/reset texture.
    /// Depth matches the 5 worker video atlases (on-screen + present queue + upload).
    video_allocation_hold: Option<ImageAllocation>,
    video_allocation_hold2: Option<ImageAllocation>,
    video_allocation_hold3: Option<ImageAllocation>,
    video_allocation_hold4: Option<ImageAllocation>,
    video_frame_wh: (u32, u32),
    /// One in-flight `image::allocate` — drop intermediate soft frames.
    video_upload_busy: bool,
    /// Bumped on Stop/close so late `VideoFrameAllocated` cannot resurrect a cleared stage.
    video_upload_gen: u64,
    /// Latest soft frame waiting while GPU upload is in flight (at most one).
    video_pending: Option<(u32, u32, Vec<u8>, u64)>,
    /// The GPU video stage (shader, desktop wgpu) shows a frame of this playback.
    stage_picture: bool,
    /// User downloads keyed by stream URL (running, finished, or failed this session).
    downloads: std::collections::HashMap<String, crate::downloads::DownloadState>,
    /// Stream URLs waiting for a free download slot, in click order.
    download_queue: std::collections::VecDeque<String>,
    /// Resumable `.part` files on disk, keyed by [`crate::downloads::url_key`].
    download_partials: std::collections::HashMap<String, crate::downloads::Partial>,
    /// Finished downloads (persisted): played instead of the stream.
    download_library: crate::downloads::Library,
    /// First tick Instant when embedded backend looked dead (time debounce, not tick count).
    playback_ended_since: Option<std::time::Instant>,
    /// Last time position / buffering / alive were read from the player.
    /// Doing that on every video frame locks libmpv and stutters picture, sound, and the bar.
    playback_clock_poll: Option<std::time::Instant>,
    /// Android AudioManager focus held (edge-trigger request/abandon).
    #[cfg(target_os = "android")]
    audio_focus_held: bool,
    /// Why playback is paused — avoids abandoning focus after LOSS (stuck pause).
    #[cfg(target_os = "android")]
    pause_cause: PauseCause,
    /// Last Surface generation — rebind mpv wid when Java bumps gen (rotate).
    #[cfg(target_os = "android")]
    android_surface_gen: u32,
    /// Consecutive ticks with Surface not ready — drives mid-play compat demotion.
    #[cfg(target_os = "android")]
    android_surface_misses: u32,
    /// Throttle healthy Surface maintain (JNI) — not every PlayerTick.
    #[cfg(target_os = "android")]
    android_maintain_tick: u32,
    /// Sticky Soft after Surface demotion (bind skips Surface ladder until Stop).
    #[cfg(target_os = "android")]
    android_force_soft: bool,
    /// Soft started because Surface bind missed — promote when Surface becomes ready.
    #[cfg(target_os = "android")]
    android_want_surface_upgrade: bool,
    /// Bumped on Play / manual close so deferred Stop→close cannot kill a new session.
    player_close_gen: u64,
    player_panel: PlayerPanel,
    /// The player window's title-bar close is waiting for a choice.
    player_exit_ask: bool,
    goto_draft: String,
    sleep_until: Option<std::time::Instant>,
    sleep_mins: Option<u32>,
    pip_mode: bool,
    /// True when the player window is in OS fullscreen mode.
    player_fullscreen: bool,
    /// Overlay dock / sheets visible (auto-hides on pointer idle).
    player_chrome_visible: bool,
    /// Soft fade for player chrome (1 = fully shown, 0 = hidden).
    chrome_alpha: f32,
    /// Velocity of the player-bar spring (Rune `Spring::sample`).
    chrome_vel: f32,
    player_pointer_at: Option<std::time::Instant>,
    /// Position and the moment it last moved, so a frozen episode can be nudged.
    play_stall_at: Option<(f64, std::time::Instant)>,
    play_stall_kicks: u8,
    /// Floating notices on the left of the screen.
    notices: Vec<Notice>,
    notice_seq: u64,
    /// Episode alerts already asked of the portal during this catalog generation.
    alert_checked: std::collections::HashSet<String>,
    /// Ignore video_rect / soft-size churn while OS fullscreen is settling.
    player_layout_freeze_until: Option<std::time::Instant>,
    /// Bumped so a late fullscreen apply cannot land after the user left it.
    player_fs_gen: u64,
    /// Last border-drag settle. Older timers must not resize the picture.
    player_resize_gen: u64,
    /// Series episode URLs for next / previous (current index in list).
    series_queue: Vec<(String, String)>,
    series_queue_idx: usize,
    /// +1 or -1 asked while the episode list was still loading.
    series_neighbor_pending: Option<i32>,
    /// Episode URL whose series list is being fetched.
    series_queue_loading: Option<String>,
    /// Catalog URL of the film or episode currently open. Progress is stored on this, not on a `file:` rewrite.
    watch_key: Option<String>,
    /// Seconds to seek once the new playback reports a timeline.
    resume_at: Option<f64>,
    resume_not_before: Option<std::time::Instant>,
    resume_until: Option<std::time::Instant>,
    watch_saved_at: Option<std::time::Instant>,
    /// Opening a series should land on the season that was in progress.
    resume_season_pending: bool,
    seek_drag: Option<f64>,
    prefetch_armed_for: Option<String>,
    /// Next-episode preload in flight: catalog URL + abort handle.
    prefetch_job: Option<(String, iced::task::Handle)>,
    /// Preloaded episode ready on disk: catalog URL → file.
    prefetched: Option<(String, std::path::PathBuf)>,
    /// Lazy Xtream `get_vod_info` enrich — never burst at boot (ban risk).
    xtream_vod_enrich_started: bool,
    /// Series `cast` from the portal list was written into the open catalog.
    series_credits_filled: bool,
    party: crate::party::Party,
    /// Stable image id. A new handle every frame makes the QR blink.
    remote_qr: Option<ImageHandle>,
    remote_qr_key: String,
    remote_qr_side: f32,
    /// After a portal 429, film metadata waits so series pages can load.
    film_info_pause_until: Option<std::time::Instant>,
    /// Real portal shelves for a film title, so a 4K / VOST copy can show
    /// the category of its other entry. Empty until the film grid is open.
    vod_shelf_by_title: std::collections::HashMap<String, String>,
    /// Cached hardware probe (monitor + GPU).
    display_probe: DisplayProbe,
    display_probe_at: std::time::Instant,
    /// Resolved GUI / video FPS ceilings from prefs + probe + stage.
    display_caps: DisplayCaps,
    /// Indices into `bundle.channels` / `vod` / `series` for the current browse filter.
    /// `BrowseIndex::Identity` for « All » avoids a million-entry Vec.
    browse_index: BrowseIndex,
    /// Flat series-detail rows (headers + episodes) for virtual scroll.
    episode_flat: Vec<EpisodeFlat>,
    /// Scroll offset / viewport height for the active virtualized list.
    browse_scroll_y: f32,
    browse_view_h: f32,
    /// Last mounted content window — skip App mutate when scroll stays in-window.
    browse_slice: (usize, usize),
    /// Category sidebar virtual scroll.
    cat_scroll_y: f32,
    cat_view_h: f32,
    cat_slice: (usize, usize),
    /// Cached sidebar rows — rebuilt on tab / filter / catalog, not on content scroll.
    cat_entries: Vec<(String, String, bool, usize)>,
    /// EPG channel id → "HH:MM  title" of the programme on air.
    epg_now: std::collections::HashMap<String, String>,
    /// EPG channel id → "HH:MM  title" of the following programme.
    epg_next: std::collections::HashMap<String, String>,
    epg_now_built: Option<std::time::Instant>,
    /// Coalesce ImageLoaded → one iced view refresh per burst (fast scroll).
    pending_images: Vec<(String, Option<Uuid>, Vec<u8>)>,
    /// Image URLs skipped because the inflight cap was full (cast photos).
    art_wait: Vec<String>,
    image_flush_armed: bool,
    /// Scroll velocity (px/s) for art lookahead / fling LOD.
    browse_scroll_vy: f32,
    browse_scroll_at: Option<std::time::Instant>,
    /// Last frame was a fast fling — used to detect settle → art flush.
    browse_was_flinging: bool,
    /// Finger / mouse drag on mosaic overlay → scroll_by.
    browse_dragging: bool,
    browse_drag_last_y: Option<f32>,
    /// Cumulative |dy| during a browse drag (tap vs scroll discrimination).
    browse_drag_accum: f32,
    /// True once finger moved past slop — suppress tile/row open on release.
    browse_drag_moved: bool,
    /// Background warm of `browse_index` art after profile load (not scroll-gated).
    art_warm_cursor: usize,
    art_warm_active: bool,
    /// Debounce search → index rebuild (generation discards stale timers).
    search_debounce_gen: u64,
    /// Atomic job meters (ingest / index / images) + browse generation.
    jobs: crate::async_jobs::JobMeters,
    /// True when we paused playback because Android Suspended (resume on foreground).
    #[cfg(target_os = "android")]
    lifecycle_paused: bool,
    /// Defer portal sync until WireGuard SOCKS is applied (boot gate).
    pending_portal_sync_after_wg: bool,
    /// Boot already starts a sync; the idle checker waits this out.
    catalog_sync_not_before: std::time::Instant,
    catalog_load_gen: u64,
    catalog_parts: crate::catalog_db::CatalogParts,
    /// Position when sleep recovery ran. A later check reloads the file only
    /// if playback was running and the clock did not move.
    wake_pos: Option<f64>,
    /// Last wall-clock second seen by [`Message::WakeProbe`].
    wake_unix: u64,
    stage_click_gen: u64,
    /// D-pad / TV browse focus index into `browse_index`.
    browse_focus: usize,
    /// Last finger drag Δy for mosaic fling coast seed.
    browse_drag_last_dy: f32,
    /// Multi-tick fling coast velocity (px/tick); decays by MOTION_COAST_FRICTION.
    browse_coast_vy: f32,
    /// Mosaic tile currently pressed (press feedback).
    pressed_mosaic: Option<String>,
    /// The category column just collapsed under the pointer. The release that
    /// follows must not open the tile that slid underneath.
    swallow_tile_release: bool,
    /// « Voir plus » rebuilds the grid on press. The release then lands on the
    /// poster that replaced the button. Ignore that open briefly.
    suppress_open_until: Option<std::time::Instant>,
    /// Pending SAF operation (Android document picker).
    #[cfg(target_os = "android")]
    saf_kind: Option<SafKind>,
    /// Cached system insets (dp) from WindowInsets / content_rect.
    #[cfg(target_os = "android")]
    system_insets: (f32, f32, f32, f32),
}

/// One row in the virtualized series episode list.
#[derive(Debug, Clone)]
pub(crate) enum EpisodeFlat {
    Header(u32),
    Ep { season_idx: usize, ep_idx: usize },
}

/// Why we paused on Android (user vs system) — drives focus abandon/resume.
#[cfg(target_os = "android")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PauseCause {
    #[default]
    None,
    User,
    AudioFocus,
    Lifecycle,
}

/// Android SAF document picker purpose.
#[cfg(target_os = "android")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SafKind {
    WireGuard,
    Playlist,
    ProfileImport,
    ProfileExport,
}

/// Which text field should receive a clipboard paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasteTarget {
    FormName,
    FormEndpoint,
    FormUser,
    FormPass,
    FormMac,
    FormEpg,
    FormMirrors,
    FormOmdbKey,
    FormTmdbKey,
    FormDnsServers,
    FormDohUrl,
    FormDotServer,
    FormWgPaste,
    /// Browser search field (`content_header`).
    Search,
    /// Category filter (sidebar / chips).
    CatFilter,
    /// Player “Aller à” timecode sheet.
    Goto,
    /// Phone remote / cinema link (`fp1|…`).
    PartyLink,
}

/// Poster/art fetch result: (key, source uuid, bytes) or (key, error).
type ImageLoadResult = Result<(String, Option<uuid::Uuid>, Vec<u8>), (String, String)>;

/// Year, recommended age and genres learned for one poster.
#[derive(Debug, Clone)]
pub(crate) struct PosterCaption {
    id: String,
    source_id: Option<Uuid>,
    series: bool,
    year: Option<String>,
    rated: Option<String>,
    genre: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Tab(Tab),
    SearchChanged(String),
    /// Apply debounced search (`search_debounce_gen` must still match).
    SearchApply(u64),
    PlayChannel(Channel),
    /// Browse lists: id only (no Channel clone on every virtual row paint).
    /// `(channel id, profile)`: stream ids of two panels overlap.
    PlayChannelId(String, Option<Uuid>),
    PlayVod {
        name: String,
        url: String,
        kind: ContentKind,
        poster: Option<String>,
    },
    /// Episode list for the series that is playing, when it was not on screen.
    SeriesQueueReady {
        for_url: String,
        title: String,
        result: Result<SeriesItem, String>,
    },
    /// Save the film or episode to the user's Downloads folder (no autoplay).
    DownloadMedia {
        name: String,
        url: String,
    },
    DownloadEvent {
        url: String,
        event: crate::downloads::DownloadEvent,
    },
    CancelDownload(String),
    /// Queue every episode of the open series (`None`) or of one season.
    DownloadSeason(Option<u32>),
    /// Cancel the queued / running episodes of the open series or one season.
    CancelSeasonDownloads(Option<u32>),
    /// Open or close the season list on the series page.
    ToggleSeasonMenu,
    EpisodeHover { key: String, on: bool },
    /// Show one season's episodes.
    SelectSeason(u32),
    /// Headshot for one cast or crew name (`None` when IMDb has no photo).
    CastPhoto { name: String, url: Option<String> },
    /// Open the folder holding a finished download.
    RevealDownload(std::path::PathBuf),
    FormDownloadDir(String),
    SaveDownloadDir,
    PickDownloadDir,
    DownloadDirPicked(Option<std::path::PathBuf>),
    ResetDownloadDir,
    OpenDownloadsDir,
    Stop,
    TogglePause,
    ToggleMute,
    VolumeChanged(f32),
    /// Slider let go: the volume is written to disk once, not on every drag step.
    VolumeReleased,
    SeekRel(i32),
    /// Seek bar dragged to this ratio (preview only).
    SeekPercent(f64),
    /// Seek bar let go: one seek request instead of one per drag step.
    SeekReleased,
    RestartStream,
    ToggleFullscreen,
    PlayerPointerActivity,
    /// Click on the picture. Play/pause is armed, then cancelled if this was a double-click.
    PlayerStageClick,
    PlayerStageClickFire(u64),
    /// Wall clock moved: the PC woke from sleep.
    WakeProbe,
    /// Second audio-device reload, once Windows has brought the device back.
    WakeAudio,
    PlayerChromeTick,
    CycleAudio,
    CycleSubtitles,
    /// Pick one audio track by its mpv id.
    SelectAudio(i64),
    /// `None` turns subtitles off.
    SelectSubtitle(Option<i64>),
    /// Seek past the end credits of the current film or episode.
    SkipGeneric,
    /// Seek past a studio card or a series opening. Not shown on the picture.
    SkipIntro,
    /// Finish entering fullscreen once the window is no longer maximized.
    PlayerFullscreenCommit(window::Id, u64),
    /// The player window moved, possibly onto another monitor.
    PlayerScreenChanged(window::Id),
    /// Border drag finished: snap the picture to the new window size.
    PlayerResizeSettled { id: window::Id, gen: u64 },
    /// Remember whether the player should skip those on its own.
    ToggleSkipCredits,
    /// Hide ratings at or below this age. `None` shows them.
    SetHideAgeUnder(Option<u8>),
    /// Hide ratings at or above this age. `None` shows them.
    SetHideAgeOver(Option<u8>),
    /// Ages found for the series currently on screen.
    AgesLoaded(Vec<(String, Option<Uuid>, Option<String>)>),
    /// Year, age and genres for the posters on screen.
    CaptionLoaded(Vec<PosterCaption>),
    SetSpeed(f64),
    PlayerTick,
    /// Drain the remote / cinema listener.
    PartyPoll,
    PartyCreate,
    PartyJoin,
    PartyLeave,
    PartyBroadcast,
    PartyStop,
    /// 0 = source (1080p or higher), 720, 480.
    PartyQuality(u16),
    PartyDraft(String),
    PartyJoinDraft(String),
    PartyPair(String),
    PartyRemote(crate::party::RemoteOp),
    PartyCopy,
    /// Drop one floating notice.
    DismissNotice(u64),
    /// Drop notices whose time is up.
    NoticeTick,
    /// Bell on the open series: tell me when a new episode is out.
    ToggleEpisodeAlert,
    PartyHttp(Result<(String, String), String>),
    /// While the app stays open, pull new and updated portal rows once the catalog is stale.
    CatalogFreshnessCheck,
    /// Deferred CatalogDb::open finished (DB parked in PENDING_CATALOG_BOOT).
    #[cfg(target_os = "android")]
    CatalogBootReady {
        counts: (usize, usize, usize),
        sync_fresh: bool,
    },
    /// Android system Back while browsing (close detail → ignore Activity finish).
    #[cfg(target_os = "android")]
    NavBack,
    /// TV / D-pad browse focus step (when player not open).
    #[cfg(target_os = "android")]
    BrowseFocusDelta(i32),
    /// TV / D-pad activate focused browse row.
    #[cfg(target_os = "android")]
    BrowseActivate,
    /// Poll SAF inbox (document picker / create).
    #[cfg(target_os = "android")]
    SafPoll,
    #[cfg(target_os = "android")]
    SafResult {
        kind: SafKind,
        path: Option<std::path::PathBuf>,
        name: String,
        err: Option<String>,
    },
    /// Soft-frame GPU upload finished — safe to swap without atlas flicker.
    VideoFrameAllocated {
        gen: u64,
        w: u32,
        h: u32,
        result: Result<ImageAllocation, iced_image::Error>,
    },
    CycleTheme,
    SetAccent(AccentPreset),
    SettingsQuery(String),
    ToggleSettings(SettingsSection),
    ToggleAccentMenu,
    AccentQuery(String),
    ToggleCategories,
    ToggleCustomColor,
    SetInk(Option<[u8; 3]>),
    ToggleInkPicker,
    AccentDrag(bool),
    AccentPoint(f32, f32),
    FormName(String),
    FormKind(SourceKind),
    FormEndpoint(String),
    FormUser(String),
    FormPass(String),
    FormMac(String),
    FormEpg(String),
    FormMirrors(String),
    /// Load a profile into the source form.
    EditSource(Uuid),
    CancelEditSource,
    /// Servers of a profile probed (fastest first).
    ServersRanked(Uuid, usize, usize),
    FormOmdbKey(String),
    FormTmdbKey(String),
    FormDnsServers(String),
    FormDohUrl(String),
    FormDotServer(String),
    FormWgPaste(String),
    SaveOmdbKey,
    SaveTmdbKey,
    ClearTmdbKey,
    /// First launch, no TMDB token: skip the fiches.
    ApiPromptSkip,
    /// First launch: open the settings assistant.
    ApiPromptSetup,
    /// Clicks on the dimmed page behind the prompt.
    ApiPromptHold,
    /// Show or hide the API assistant. It stays off until this is pressed.
    ApiGuideToggle,
    OpenWeb(String),
    /// Viewer language (ISO 639-1); empty = none.
    SetPrefLang(String),
    ToggleOnlyPrefLang,
    /// (source text, translated text) pairs for `settings.pref_lang`.
    TranslationsReady(String, Vec<(String, String)>),
    CycleDnsMode,
    SaveNetworkDns,
    ProbeDns,
    DnsProbeDone(String),
    ToggleWireGuard,
    WireGuardTunnelDone(Result<Option<String>, String>),
    PickWireGuardProfile,
    WireGuardProfilePicked(Option<std::path::PathBuf>),
    ImportWireGuardPaste,
    ClearWireGuardProfile,
    ApplyWireGuardDns,
    /// Touch-friendly paste (Android has no text-input context menu).
    PasteInto(PasteTarget),
    ClipboardText(PasteTarget, Option<String>),
    AddSource,
    AddPublicDemo { name: String, endpoint: String },
    RemoveSource(Uuid),
    ReloadSource(Uuid),
    /// Export profile → `.fluxplay` archive (credentials + DB + images).
    ExportProfile(Uuid),
    ImportProfile,
    ProfileExportDone(Result<String, String>),
    ProfileImportDone(Result<MediaSource, String>),
    SourceLoaded {
        source_id: Uuid,
        result: Result<Arc<PlaylistBundle>, String>,
    },
    SourcesBatchLoaded(Vec<(Uuid, Result<Arc<PlaylistBundle>, String>)>),
    /// Full catalog reload finished off the UI thread (avoids ANR on huge SQLite).
    /// One tab's rows, read from SQLite. `gen` drops a load the user already left.
    BundleCacheReady {
        gen: u64,
        parts: crate::catalog_db::CatalogParts,
        result: Result<PlaylistBundle, String>,
    },
    OpenExternal,
    SetExternalPlayer(String),
    PickPlaylistFile,
    /// Desktop rfd file picker result; Android uses SAF (`SafResult`) instead.
    #[cfg(not(target_os = "android"))]
    PlaylistFilePicked(Option<String>),
    ToggleFavorite(String),
    ShowShelf(Shelf),
    ClearHistory,
    RemoveHistory(String),
    /// Swallows the release of a heart or a remove mark so the poster does not open.
    ShelfHold,
    CycleBackend,
    ToggleHwdec,
    ToggleLowLatency,
    TogglePrefetchNext,
    CycleFpsGui,
    CycleFpsVideo,
    RefreshDisplayCaps,
    CycleVideoQuality,
    SetVideoQuality(VideoQualityPref),
    CycleGpu,
    CycleHdrMode,
    CycleDisplayPanel,
    CycleAndroidPresentPref,
    ToggleTonemapHdr,
    ToggleRememberPosition,
    CycleDefaultAspect,
    CycleDefaultDeinterlace,
    CycleDefaultUpscale,
    ToggleDefaultNightMode,
    DiagnosePortals,
    PrefetchEvent {
        url: String,
        event: crate::downloads::DownloadEvent,
    },
    DiagnoseDone(String),
    EpgFetched(Uuid, Vec<fluxplay_core::models::EpgProgramme>),
    /// `sources`: profiles that answered (their old items of the category are replaced).
    VodCategoryLoaded {
        category_id: String,
        sources: Vec<Uuid>,
        result: Result<Vec<VodItem>, String>,
    },
    SeriesCategoryLoaded {
        category_id: String,
        sources: Vec<Uuid>,
        result: Result<Vec<SeriesItem>, String>,
    },
    OpenSeries(String),
    SeriesDetailLoaded(Result<SeriesItem, String>),
    /// `(source, id, actors, director, runtime)` taken from the cached series list.
    SeriesCreditsReady(Vec<(Uuid, Vec<(String, Option<String>, Option<String>, Option<String>)>)>),
    SeriesEpisodesEnriched(SeriesItem),
    CloseSeriesDetail,
    OpenVodDetail(String),
    VodDetailLoaded(Result<VodItem, String>),
    /// French Wikipedia lead, only when the portal text is missing or English.
    FrenchPlotReady {
        series: bool,
        id: String,
        plot: Option<String>,
    },
    /// Films enriched from the portal. `skipped` failed for a reason other than
    /// a rate limit. `throttled` means the portal answered 429: stop asking.
    VodInfoBatch {
        items: Vec<VodItem>,
        skipped: Vec<String>,
        throttled: bool,
    },
    CloseVodDetail,
    DetailMetaLoaded {
        is_series: bool,
        id: String,
        patch: Option<crate::metadata::MetaPatch>,
    },
    CatFilterChanged(String),
    SelectBrowseCategory(String),
    /// Reserved: pagination UI before virtual scroll (`browser::load_more_btn`).
    #[allow(dead_code)]
    LoadMore,
    /// Virtualized browse scroll: absolute Y + viewport height.
    BrowseScrolled(f32, f32),
    /// Deferred check after scrolling: wheel/trackpad sends no "stopped" event.
    BrowseSettle,
    /// Wheel / trackpad on mosaic overlay → scroll the stable spacer surface.
    BrowseScrollBy(f32),
    BrowseDragStart,
    BrowseDragAt(f32),
    BrowseDragEnd,
    /// Multi-tick fling coast step (subscription while |browse_coast_vy| ≥ 2).
    BrowseCoastTick,
    /// Mosaic tile press feedback (id = vod/series id).
    MosaicPress(String),
    /// Category sidebar scroll (virtualized).
    CatScrolled(f32, f32),
    ImageLoaded(ImageLoadResult),
    /// Apply coalesced poster bytes to the RAM cache (one view rebuild).
    FlushPendingImages,
    /// SQLite ingest finished off the UI thread.
    CatalogIngestDone(crate::catalog_db::IngestBatchReport),
    CatalogIngestOneDone {
        source_id: Uuid,
        result: Result<crate::catalog_db::BundleApplyKind, String>,
    },
    /// Async browse index (stale gens ignored).
    BrowseIndexReady {
        gen: u64,
        index: BrowseIndex,
        episode_flat: Vec<EpisodeFlat>,
    },
    MetaEnriched {
        series: Vec<(String, Option<uuid::Uuid>, crate::metadata::MetaPatch)>,
        vod: Vec<(String, Option<uuid::Uuid>, crate::metadata::MetaPatch)>,
    },
    OpenImdb(String),
    /// Actor chip on a film / series page.
    OpenActor(String),
    ActorLoaded {
        name: String,
        card: crate::metadata::ActorCard,
    },
    CloseActor,
    /// A title on the actor sheet: open it in the catalog, or on IMDb.
    OpenActorTitle {
        title: String,
        imdb: Option<String>,
    },
    MainWindowOpened(window::Id),
    PlayerWindowOpened(window::Id),
    PlayerLayoutDirty(window::Id),
    PlayerLayout {
        position: Option<Point>,
        size: Size,
        scale: f32,
    },
    WindowResized {
        id: window::Id,
        size: Size,
    },
    WindowClosed(window::Id),
    ClosePlayerWindow,
    /// Title-bar close on a window. The player asks before it goes away.
    PlayerCloseRequested(window::Id),
    PlayerDismissExit,
    PlayerQuitApp,
    /// Deferred close after Stop — ignored if `player_close_gen` advanced (new play).
    ClosePlayerWindowDeferred(u64),
    // ── Extended player controls ───────────────────────────────────────────
    PlayerPanel(PlayerPanel),
    CycleSpeed,
    ToggleLoop,
    Screenshot,
    GotoDraftChanged(String),
    GotoSubmit,
    ToggleSubVisibility,
    CycleAspect,
    /// Desktop window level toggle; not offered in the Android UI.
    #[cfg(not(target_os = "android"))]
    ToggleOntop,
    TogglePip,
    ChapterStep(i32),
    PlaylistPrev,
    PlaylistNext,
    AddBookmark,
    JumpBookmark(usize),
    CycleSleepTimer,
    MarkAbA,
    MarkAbB,
    ClearAbLoop,
    SubDelay(f64),
    AudioDelay(f64),
    CycleAudioMode,
    CycleEq,
    ToggleLoudnorm,
    ToggleDeinterlace,
    CycleUpscale,
    CycleRotate,
    NudgeZoom(f64),
    ToggleNightVf,
    CycleCache,
    CycleDemux,
    PlayerHotkey(PlayerHotkey),
    /// Key press seen in window `Id`; only the player window drives the player.
    PlayerHotkeyIn(window::Id, PlayerHotkey),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PlayerHotkey {
    TogglePause,
    SeekBack,
    SeekFwd,
    SeekBackBig,
    SeekFwdBig,
    VolumeUp,
    VolumeDown,
    Mute,
    Fullscreen,
    Escape,
    Stop,
    Restart,
    Speed,
    Loop,
    Screenshot,
    FrameStep,
}

struct Notice {
    id: u64,
    text: String,
    until: std::time::Instant,
}

impl FluxPlay {
    fn new() -> (Self, Task<Message>) {
        let persisted = storage::load();
        let mut sources = persisted.sources;
        sanitize_sources(&mut sources);
        demo::strip_demo_if_real(&mut sources);
        if sources.is_empty() {
            sources.push(demo::demo_source());
        }
        let mut settings = persisted.settings;
        settings.fold_series_history();
        storage::save(&storage::PersistedState {
            settings: settings.clone(),
            sources: sources.clone(),
        });
        let system_dark = {
            #[cfg(target_os = "android")]
            {
                crate::android_bridge::is_night_mode()
            }
            #[cfg(not(target_os = "android"))]
            {
                matches!(dark_light::detect(), Ok(dark_light::Mode::Dark))
            }
        };

        #[cfg(target_os = "android")]
        {
            fluxplay_providers::api_cache::set_cache_root(
                storage::data_dir().join("xtream-api"),
            );
        }

        let source_ids: Vec<Uuid> = sources.iter().map(|s| s.id).collect();
        // Desktop: open DB on boot (OK). Android: defer open+counts off UI (ANR).
        #[cfg(not(target_os = "android"))]
        let catalog_db = crate::catalog_db::CatalogDb::open(&source_ids);
        #[cfg(target_os = "android")]
        let catalog_db: Option<crate::catalog_db::CatalogDb> = None;
        // Do NOT load_bundle() on the UI/boot thread — 100k–1M rows → ANR.
        // Counts are COUNT(*) only; the full PlaylistBundle arrives via BundleCacheReady.
        let bundle = PlaylistBundle::default();
        #[cfg(not(target_os = "android"))]
        let (ch_n, vod_n, ser_n) = catalog_db
            .as_ref()
            .map(|db| db.counts())
            .unwrap_or((0, 0, 0));
        #[cfg(target_os = "android")]
        let (ch_n, vod_n, ser_n) = (0usize, 0usize, 0usize);
        let has_real = sources.iter().any(|s| !demo::is_demo(s));
        #[cfg(not(target_os = "android"))]
        let sync_fresh = catalog_db
            .as_ref()
            .map(|db| db.is_sync_fresh())
            .unwrap_or(false);
        #[cfg(target_os = "android")]
        let sync_fresh = false;
        let status = if has_real && sync_fresh {
            format!("Chargement cache · {ch_n} live · {vod_n} VOD · {ser_n} séries…")
        } else if has_real && (ch_n + vod_n + ser_n) > 0 {
            format!("DB locale · {ch_n} live · {vod_n} VOD · {ser_n} séries — sync…")
        } else if has_real {
            "Synchronisation catalogue (live / VOD / séries)…".into()
        } else {
            "Démo FluxPlay (ajoutez une source Xtream)".into()
        };

        let opts = play_options_from(&settings);
        #[cfg(target_os = "android")]
        let open_main: Task<Message> = Task::none();
        #[cfg(target_os = "android")]
        let main_id: Option<window::Id> = None;
        #[cfg(not(target_os = "android"))]
        let (main_id, open_main) = {
            let (id, open) = window::open(window::Settings {
                // Fits a 1080p screen minus the taskbar; start maximized.
                size: Size::new(1280.0, 800.0),
                min_size: Some(Size::new(720.0, 480.0)),
                maximized: true,
                position: window::Position::Centered,
                icon: app_icon(),
                exit_on_close_request: true,
                ..Default::default()
            });
            (Some(id), open)
        };
        let mut session = StreamSession::with_options(opts);
        session.set_preferred_audio_lang(&system_ui_lang());
        let accent_cursor = settings
            .custom_accent
            .map(crate::theme::cursor_for_rgb)
            .unwrap_or((0.02, 0.28));
        let mut app = Self {
            settings,
            sources,
            bundle,
            tab: Tab::Live,
            shelf: Shelf::Catalog,
            history_art: Vec::new(),
            search: String::new(),
            settings_query: String::new(),
            settings_open: None,
            accent_open: false,
            accent_query: String::new(),
            custom_open: false,
            ink_open: false,
            accent_cursor,
            accent_hover: accent_cursor,
            accent_dragging: false,
            cats_open: true,
            selected_group: None,
            selected_channel: None,
            selected_vod_category: Some("*".into()),
            selected_series_category: Some("*".into()),
            series_detail: None,
            selected_season: None,
            season_menu_open: false,
            episode_hover: None,
            cast_photos: std::collections::HashMap::new(),
            cast_photo_asked: std::collections::HashSet::new(),
            age_asked: std::collections::HashSet::new(),
            age_lookup_busy: false,
            vod_detail: None,
            actor_sheet: None,
            detail_meta_loading: false,
            translations: std::collections::HashMap::new(),
            translating: std::collections::HashSet::new(),
            session,
            status,
            loading: true, // cache load / sync always off-UI; cleared in BundleCacheReady / ingest done
            form_name: String::new(),
            form_error: None,
            form_kind: SourceKind::M3uPlus,
            form_endpoint: String::new(),
            form_user: String::new(),
            form_pass: String::new(),
            form_mac: String::new(),
            form_epg: String::new(),
            form_mirrors: String::new(),
            editing_source: None,
            form_omdb_key: String::new(), // filled below from settings
            form_tmdb_key: String::new(),
            api_guide: false,
            form_download_dir: String::new(),
            form_dns_servers: String::new(),
            form_doh_url: String::new(),
            form_dot_server: String::new(),
            form_wg_paste: String::new(),
            network_probe: String::new(),
            system_dark,
            cat_filter: String::new(),
            list_limit: LIST_PAGE,
            images: crate::images::ImageCache::default(),
            catalog_db,
            autoplay_done: false,
            main_id,
            player_id: None,
            browse_parked: false,
            #[cfg(target_os = "android")]
            player_embedded: false,
            #[cfg(target_os = "android")]
            main_size: Size::new(360.0, 720.0),
            #[cfg(not(target_os = "android"))]
            main_size: Size::new(1920.0, 1080.0),
            layout_cache: None,
            backends_cache: None,
            external_players: Vec::new(),
            player_pos: None,
            player_logical_size: Size::new(1280.0, 720.0),
            player_fitted_wh: None,
            video_frame: None,
            video_allocation: None,
            video_allocation_hold: None,
            video_allocation_hold2: None,
            video_allocation_hold3: None,
            video_allocation_hold4: None,
            video_frame_wh: (0, 0),
            video_upload_busy: false,
            video_upload_gen: 0,
            video_pending: None,
            stage_picture: false,
            downloads: std::collections::HashMap::new(),
            download_queue: std::collections::VecDeque::new(),
            download_partials: std::collections::HashMap::new(), // filled below from settings
            download_library: crate::downloads::Library::load(
                crate::storage::data_dir().join("downloads.index"),
            ),
            playback_ended_since: None,
            playback_clock_poll: None,
            #[cfg(target_os = "android")]
            audio_focus_held: false,
            #[cfg(target_os = "android")]
            pause_cause: PauseCause::None,
            #[cfg(target_os = "android")]
            android_surface_gen: 0,
            #[cfg(target_os = "android")]
            android_surface_misses: 0,
            #[cfg(target_os = "android")]
            android_maintain_tick: 0,
            #[cfg(target_os = "android")]
            android_force_soft: false,
            #[cfg(target_os = "android")]
            android_want_surface_upgrade: false,
            player_close_gen: 0,
            player_panel: PlayerPanel::None,
            player_exit_ask: false,
            goto_draft: String::new(),
            sleep_until: None,
            sleep_mins: None,
            pip_mode: false,
            player_fullscreen: false,
            player_chrome_visible: true,
            chrome_alpha: 1.0,
            chrome_vel: 0.0,
            player_pointer_at: None,
            play_stall_at: None,
            play_stall_kicks: 0,
            notices: Vec::new(),
            notice_seq: 0,
            alert_checked: std::collections::HashSet::new(),
            player_layout_freeze_until: None,
            player_fs_gen: 0,
            player_resize_gen: 0,
            series_queue: Vec::new(),
            series_queue_idx: 0,
            series_neighbor_pending: None,
            series_queue_loading: None,
            watch_key: None,
            resume_at: None,
            resume_not_before: None,
            resume_until: None,
            watch_saved_at: None,
            resume_season_pending: false,
            seek_drag: None,
            prefetch_armed_for: None,
            prefetch_job: None,
            prefetched: None,
            xtream_vod_enrich_started: false,
            series_credits_filled: false,
            party: crate::party::Party::start(),
            remote_qr: None,
            remote_qr_key: String::new(),
            remote_qr_side: 0.0,
            film_info_pause_until: None,
            vod_shelf_by_title: std::collections::HashMap::new(),
            display_probe: display_caps::boot_probe(),
            display_probe_at: std::time::Instant::now(),
            display_caps: display_caps::resolve_caps(
                FpsCapPref::Auto,
                FpsCapPref::Auto,
                None,
                None,
                None,
            ),
            browse_index: BrowseIndex::Empty,
            episode_flat: Vec::new(),
            browse_scroll_y: 0.0,
            browse_view_h: 720.0,
            browse_slice: (0, 0),
            cat_scroll_y: 0.0,
            cat_view_h: 720.0,
            cat_slice: (0, 0),
            cat_entries: Vec::new(),
            epg_now: std::collections::HashMap::new(),
            epg_next: std::collections::HashMap::new(),
            epg_now_built: None,
            pending_images: Vec::new(),
            art_wait: Vec::new(),
            image_flush_armed: false,
            browse_scroll_vy: 0.0,
            browse_scroll_at: None,
            browse_was_flinging: false,
            browse_dragging: false,
            browse_drag_last_y: None,
            browse_drag_accum: 0.0,
            browse_drag_moved: false,
            art_warm_cursor: 0,
            art_warm_active: false,
            search_debounce_gen: 0,
            jobs: crate::async_jobs::JobMeters::new(),
            #[cfg(target_os = "android")]
            lifecycle_paused: false,
            pending_portal_sync_after_wg: false,
            catalog_sync_not_before: std::time::Instant::now()
                + std::time::Duration::from_secs(180),
            catalog_load_gen: 0,
            catalog_parts: crate::catalog_db::CatalogParts::live(),
            wake_pos: None,
            wake_unix: 0,
            stage_click_gen: 0,
            browse_focus: 0,
            browse_drag_last_dy: 0.0,
            browse_coast_vy: 0.0,
            pressed_mosaic: None,
            swallow_tile_release: false,
            suppress_open_until: None,
            #[cfg(target_os = "android")]
            saf_kind: None,
            #[cfg(target_os = "android")]
            system_insets: (0.0, 0.0, 0.0, 0.0),
        };
        app.display_caps = display_caps::resolve_caps(
            app.settings.fps_gui,
            app.settings.fps_video,
            None,
            None,
            Some(&app.display_probe),
        );
        app.apply_host_tuning();
        app.form_omdb_key = app.settings.omdb_api_key.clone();
        app.form_tmdb_key = app.settings.tmdb_api_key.clone();
        app.form_download_dir = app.settings.download_dir.clone();
        app.download_partials = crate::downloads::scan_partials(&app.downloads_dir());
        if let Some(root) = app.downloads_dir().to_str() {
            std::env::set_var("FLUXPLAY_DOWNLOAD_ROOT", root);
        }
        app.form_dns_servers = app.settings.network.dns_servers.clone();
        app.form_doh_url = app.settings.network.doh_url.clone();
        app.form_dot_server = app.settings.network.dot_server.clone();
        crate::network::apply_to_http(&app.settings.network);
        crate::metadata::set_omdb_api_key(if app.settings.omdb_api_key.is_empty() {
            None
        } else {
            Some(app.settings.omdb_api_key.clone())
        });
        crate::metadata::set_tmdb_token(if app.settings.tmdb_api_key.is_empty() {
            None
        } else {
            Some(app.settings.tmdb_api_key.clone())
        });
        crate::metadata::set_pref_lang(Some(app.settings.pref_lang.clone()));

        // Offline-first: skip portal storm when SQLite catalog is still fresh.
        let mut boot = Vec::new();
        #[cfg(target_os = "android")]
        {
            let ids = source_ids.clone();
            boot.push(Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let db = crate::catalog_db::CatalogDb::open(&ids);
                        let counts = db.as_ref().map(|d| d.counts()).unwrap_or((0, 0, 0));
                        let sync_fresh =
                            db.as_ref().map(|d| d.is_sync_fresh()).unwrap_or(false);
                        if let Ok(mut slot) = PENDING_CATALOG_BOOT.lock() {
                            *slot = db;
                        }
                        (counts, sync_fresh)
                    })
                    .await
                    .unwrap_or(((0, 0, 0), false))
                },
                |(counts, sync_fresh)| Message::CatalogBootReady {
                    counts,
                    sync_fresh,
                },
            ));
        }
        let starting_wg = app.settings.network.wireguard_enabled
            && !app.settings.network.wireguard_profile_path.is_empty();
        if starting_wg {
            let path = app.settings.network.wireguard_profile_path.clone();
            let bootstrap = app.settings.network.wireguard_bootstrap_dns.clone();
            boot.push(Task::perform(
                async move {
                    crate::wg_tunnel::start_tunnel_from_file(
                        std::path::Path::new(&path),
                        &bootstrap,
                    )
                    .await
                    .map(Some)
                },
                Message::WireGuardTunnelDone,
            ));
            // Hold portal sync until SOCKS is applied (or tunnel fails).
            app.pending_portal_sync_after_wg = !sync_fresh
                && (has_real || !app.sources.is_empty())
                && std::env::var_os("FLUXPLAY_AUTO_URL").is_none();
        }
        #[cfg(not(target_os = "android"))]
        {
            boot.push(app.rebuild_bundle_from_cache_task());
        }
        // Android: CatalogBootReady opens DB then rebuilds (avoid empty-DB race).
        // Index after BundleCacheReady — empty Identity until then.
        #[cfg(not(target_os = "android"))]
        {
            boot.push(open_main.map(Message::MainWindowOpened));
        }
        #[cfg(target_os = "android")]
        {
            let _ = open_main;
            // Bind to real Activity surface size + force immersive fullscreen.
            boot.push(android_bind_window());
        }
        // Portal sync can overlap cache load; BundleCacheReady refreshes RAM again after.
        // When WG starts at boot, defer reload until WireGuardTunnelDone.
        if !starting_wg
            && !sync_fresh
            && (has_real || !app.sources.is_empty())
            && std::env::var_os("FLUXPLAY_AUTO_URL").is_none()
        {
            boot.push(app.reload_all_task());
        }
        if let Ok(url) = std::env::var("FLUXPLAY_AUTO_URL") {
            let url = url.trim().to_string();
            if !url.is_empty() {
                app.autoplay_done = true;
                let name = std::env::var("FLUXPLAY_AUTO_TITLE")
                    .unwrap_or_else(|_| "Auto · 4K test".into());
                let ch = Channel {
                    id: "fluxplay-auto".into(),
                    name,
                    stream_url: url,
                    logo: None,
                    group: Some("Local".into()),
                    tvg_id: None,
                    tvg_name: None,
                    tvg_logo: None,
                    epg_channel_id: None,
                    scheme: None,
                    source_id: None,
                    kind: Default::default(),
                    catchup: None,
                };
                boot.push(Task::done(Message::PlayChannel(ch)));
            }
        }
        let ids: Vec<Uuid> = app.sources.iter().map(|s| s.id).collect();
        boot.extend(ids.into_iter().map(|id| app.rank_servers_task(id)));
        // A preload from a previous run is stale (and possibly partial).
        boot.push(
            Task::perform(async { tokio::fs::remove_dir_all(prefetch_dir()).await.ok() }, |_| ())
                .discard(),
        );
        let task = Task::batch(boot);
        tracing::info!(
            sources = app.sources.len(),
            channels = app.bundle.channels.len(),
            vod = app.bundle.vod.len(),
            series = app.bundle.series.len(),
            sync_fresh,
            has_real,
            "FluxPlay::new ready"
        );
        (app, task)
    }

    fn title(&self, id: window::Id) -> String {
        if self.player_id == Some(id) {
            self.session
                .channel
                .as_ref()
                .map(|c| format!("FluxPlay Lecteur · {}", c.name))
                .unwrap_or_else(|| "FluxPlay Lecteur".into())
        } else {
            "FluxPlay".into()
        }
    }

    fn theme_for(&self, _id: window::Id) -> Option<Theme> {
        Some(self.theme())
    }

    #[cfg(target_os = "android")]
    fn title_android(&self) -> String {
        self.title(self.main_id.unwrap_or_else(window::Id::unique))
    }

    #[cfg(target_os = "android")]
    fn theme_android(&self) -> Option<Theme> {
        self.theme_for(self.main_id.unwrap_or_else(window::Id::unique))
    }

    /// Window base style. When the video Surface session is live, the window
    /// must clear fully TRANSPARENT so the behind-window SurfaceView (video)
    /// shows through the punch-through hole; the player chrome then draws as
    /// an overlay layer on top. Everywhere else the normal theme background.
    #[cfg(target_os = "android")]
    fn style_android(&self, theme: &Theme) -> iced::theme::Style {
        use iced::theme::Base;
        let mut style = theme.base();
        let player_open = self.player_embedded || self.player_id.is_some();
        if player_open && self.session.native.android_surface_present() {
            style.background_color = iced::Color::TRANSPARENT;
        }
        style
    }

    #[cfg(target_os = "android")]
    fn view_android(&self) -> Element<'_, Message> {
        self.view(self.main_id.unwrap_or_else(window::Id::unique))
    }

    fn is_day(&self) -> bool {
        match self.settings.theme {
            ThemeMode::Day => true,
            ThemeMode::Night => false,
            ThemeMode::System => !self.system_dark,
        }
    }

    /// Re-detect OS day/night when theme is System (no persist).
    fn refresh_system_dark(&mut self) {
        if self.settings.theme != ThemeMode::System {
            return;
        }
        let dark = {
            #[cfg(target_os = "android")]
            {
                crate::android_bridge::is_night_mode()
            }
            #[cfg(not(target_os = "android"))]
            {
                matches!(dark_light::detect(), Ok(dark_light::Mode::Dark))
            }
        };
        if dark != self.system_dark {
            self.system_dark = dark;
        }
    }

    /// Ease the player bar with Rune's perceptual spring. Showing uses the
    /// enter duration; hiding is shorter, the same ratio Rune uses for exits.
    fn lerp_chrome_alpha(&mut self) {
        let show = self.player_chrome_visible || self.player_panel != PlayerPanel::None;
        let target = if show { 1.0_f32 } else { 0.0_f32 };
        if (self.chrome_alpha - target).abs() < 0.004 && self.chrome_vel.abs() < 0.02 {
            self.chrome_alpha = target;
            self.chrome_vel = 0.0;
            return;
        }
        let dt = (self.display_caps.gui_period_ms().max(16) as f32 / 1000.0).clamp(0.016, 0.05);
        let duration = if show { 0.38 } else { 0.28 };
        let (x, v) = crate::rune_motion::Spring::new(duration, 0.0).sample(
            self.chrome_alpha,
            self.chrome_vel,
            target,
            dt,
        );
        self.chrome_alpha = x.clamp(0.0, 1.0);
        self.chrome_vel = v;
    }

    /// Apply one coast friction step; returns BrowseScrollBy task when moving.
    fn apply_browse_coast_step(&mut self) -> Task<Message> {
        let vy = self.browse_coast_vy;
        if vy.abs() < 2.0 {
            self.browse_coast_vy = 0.0;
            return Task::none();
        }
        self.browse_coast_vy *= MOTION_COAST_FRICTION;
        if self.browse_coast_vy.abs() < 2.0 {
            self.browse_coast_vy = 0.0;
        }
        Task::done(Message::BrowseScrollBy(vy))
    }

    /// One-shot: after a mosaic fling, the next tile `on_release` must not open.
    /// Returns true when the open should be skipped (and clears the sticky flag).
    fn consume_browse_drag_suppress(&mut self) -> bool {
        if self.browse_drag_moved {
            self.browse_drag_moved = false;
            true
        } else {
            false
        }
    }

    fn ui_theme(&self) -> UiTheme {
        UiTheme::new(self.is_day(), self.settings.accent)
            .with_custom(self.settings.custom_accent)
            .with_ink(self.settings.custom_ink)
    }

    fn refresh_display_caps(&mut self, force_probe: bool) {
        if force_probe || self.display_probe_at.elapsed() >= display_caps::probe_stale_after() {
            self.display_probe = display_caps::boot_probe();
            self.display_probe_at = std::time::Instant::now();
        }
        // Soft budget must use soft RGBA pixels (≤1080p unless UHD), NOT raw window
        // size — a 4K stage with 1080p soft present was wrongly forced to 30 fps.
        let stage = {
            let (w, h) = self.soft_present_wh();
            (w >= 2 && h >= 2).then_some((w, h))
        };
        let prev_video_hz = self.display_caps.video_hz;
        self.display_caps = display_caps::resolve_caps(
            self.settings.fps_gui,
            self.settings.fps_video,
            stage,
            self.session.content_fps(),
            Some(&self.display_probe),
        );
        // Soft budget cliffs + ±px stage noise — dampen tiny jitter only.
        // Never block upgrades (30→60) or large intentional drops.
        if !force_probe {
            let a = prev_video_hz as i32;
            let b = self.display_caps.video_hz as i32;
            let delta = b - a;
            if a > 0 && delta != 0 && delta.abs() <= 5 {
                self.display_caps.video_hz = prev_video_hz;
            }
        }
        self.apply_host_tuning();
        #[cfg(target_os = "android")]
        {
            // Only snap display mode while a Surface session is live. Soft present
            // was calling this every caps refresh → 30↔60 preferredDisplayMode thrash.
            if self.session.native.android_surface_present() {
                let panel = self.display_caps.probe.monitor_hz as f32;
                let content = self.session.content_fps().unwrap_or(0.0) as f32;
                let modes = crate::android_bridge::poll_android_device_caps()
                    .map(|c| c.refresh_modes)
                    .unwrap_or_default();
                let hz = fluxplay_player::AndroidDeviceCaps::snap_present_hz_with_modes(
                    content, panel, &modes,
                );
                crate::android_bridge::set_video_frame_rate(hz);
                // Content fps is known now — re-tune Surface A/V offset (fix judder).
                self.session.native.sync_android_video_timing();
                // Push buffer + display geometry — Java cover-fits the Surface
                // buffer to the screen via SurfaceControl transform (exact ratio).
                let disp = self.session.native.video_wh();
                let buf = self.session.native.video_buffer_wh().or(disp);
                if let (Some((bw, bh)), Some((dw, dh))) = (buf, disp) {
                    crate::android_bridge::set_video_buffer_size(bw, bh, dw, dh);
                }
            }
        }
    }

    /// Push host-derived ceilings into image cache / knobs that live outside DisplayCaps.
    fn apply_host_tuning(&mut self) {
        let t = self.display_caps.tuning;
        self.images.max_inflight = t.image_inflight;
        self.images.decode_edge_px = t.decode_edge_px;
    }

    fn virtual_overscan(&self) -> usize {
        self.display_caps.tuning.virtual_overscan
    }

    fn theme(&self) -> Theme {
        self.ui_theme().iced_theme()
    }

    /// Responsive chrome metrics from the current main window size.
    /// Android: subtract system insets so mosaic cols/tiles match the padded shell.
    fn apply_video_quality(&mut self, quality: VideoQualityPref) {
        self.settings.video_quality = quality;
        self.resync_player_options();
        self.persist();
        if self.session.channel.is_some() {
            #[cfg(target_os = "android")]
            {
                let (rw, rh) = self.soft_present_wh();
                self.session.native.options_mut().android_soft_vf = Some(
                    fluxplay_player::SoftBudget {
                        max_w: rw,
                        max_h: rh,
                        video_hz: 30,
                        gui_hz: 60,
                    }
                    .vf_scale(),
                );
            }
            self.apply_saved_video_defaults_for_current_play();
        }
        self.status = format!("Qualité d'affichage : {}", quality.short_label());
    }

    fn layout_metrics(&self) -> LayoutMetrics {
        let (iw, ih) = self.layout_content_size();
        if let Some((sz, m)) = self.layout_cache {
            if (sz.width - iw).abs() < 0.5 && (sz.height - ih).abs() < 0.5 {
                return m;
            }
        }
        LayoutMetrics::compute(iw, ih)
    }

    fn layout_content_size(&self) -> (f32, f32) {
        #[cfg(target_os = "android")]
        {
            let (l, t, r, b) = self.system_insets;
            (
                (self.main_size.width - l - r).max(120.0),
                (self.main_size.height - t - b).max(120.0),
            )
        }
        #[cfg(not(target_os = "android"))]
        {
            (self.main_size.width, self.main_size.height)
        }
    }

    fn refresh_layout_cache(&mut self) {
        let (iw, ih) = self.layout_content_size();
        let m = LayoutMetrics::compute(iw, ih);
        self.layout_cache = Some((Size::new(iw, ih), m));
    }

    /// Category sidebar or phone chips + content pane.
    fn with_categories<'a>(
        &'a self,
        ui: UiTheme,
        m: LayoutMetrics,
        title: &'a str,
        entries: &'a [(String, String, bool, usize)],
        content: Element<'a, Message>,
    ) -> Element<'a, Message> {
        if m.cat_w <= 1.0 {
            column![
                container(browser::category_chips(ui, &self.cat_filter, entries))
                    .width(Fill)
                    .height(Length::Shrink),
                container(content).width(Fill).height(Fill).clip(true),
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill)
            .into()
        } else {
            let tab = browser::category_tab(ui, self.cats_open);
            let body = if self.cats_open {
                row![
                    row![
                        browser::category_sidebar(
                            ui,
                            m.cat_w,
                            title,
                            &self.cat_filter,
                            entries,
                            self.cat_scroll_y,
                            self.cat_view_h,
                        ),
                        tab,
                    ]
                    .spacing(0)
                    .height(Fill),
                    content,
                ]
            } else {
                row![tab, content]
            };
            body.spacing(m.gap)
                .width(Fill)
                .height(Fill)
                .align_y(Alignment::Start)
                .into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let closes = window::close_events().map(Message::WindowClosed);
        let close_req = window::close_requests().map(Message::PlayerCloseRequested);
        // Android application() creates one window — catch it even if boot bind raced.
        #[cfg(target_os = "android")]
        let opens = window::open_events().map(Message::MainWindowOpened);
        #[cfg(not(target_os = "android"))]
        let opens = Subscription::<Message>::none();
        let moves = window::events().filter_map(|(id, event)| match event {
            window::Event::Resized(size) => Some(Message::WindowResized { id, size }),
            window::Event::Moved(_) => Some(Message::PlayerScreenChanged(id)),
            _ => None,
        });
        let tick = {
            let surface_video = {
                #[cfg(target_os = "android")]
                {
                    self.session.native.android_surface_present()
                }
                #[cfg(not(target_os = "android"))]
                {
                    false
                }
            };
            let soft_video = self.session.has_embedded_video()
                && !surface_video
                && matches!(
                    self.session.state,
                    PlaybackState::Playing
                        | PlaybackState::Buffering
                        | PlaybackState::Paused
                );
            let playing_like = matches!(
                self.session.state,
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
            );
            let player_open = {
                #[cfg(target_os = "android")]
                {
                    self.player_embedded || self.player_id.is_some()
                }
                #[cfg(not(target_os = "android"))]
                {
                    self.player_id.is_some()
                }
            };
            // Soft-render: poll at display_caps.video_hz; Surface path: light time/chrome only.
            let period_ms = if soft_video {
                // Sample every monitor refresh. A content-fps tick phase-drifts
                // against the snapped 3-2 cadence and shows up as micro-stutter.
                crate::display_caps::period_ms(self.display_probe.monitor_hz.max(24))
            } else if self.sleep_until.is_some() {
                1000
            } else if playing_like && player_open {
                // Surface / CLI: don't spin at soft video_hz (battery + UI wakeups).
                self.display_caps.gui_period_ms().max(200)
            } else {
                0
            };
            if period_ms > 0 {
                iced::time::every(std::time::Duration::from_millis(period_ms))
                    .map(|_| Message::PlayerTick)
            } else {
                Subscription::none()
            }
        };
        // Chrome autohide + alpha lerp + caps refresh. Keep alive during soft video even
        // when chrome is fully hidden — otherwise video_hz/content_fps never update mid-play.
        let chrome_tick = {
            let player_open = {
                #[cfg(target_os = "android")]
                {
                    self.player_embedded || self.player_id.is_some()
                }
                #[cfg(not(target_os = "android"))]
                {
                    self.player_id.is_some()
                }
            };
            let soft_video = {
                let surface_video = {
                    #[cfg(target_os = "android")]
                    {
                        self.session.native.android_surface_present()
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        false
                    }
                };
                self.session.has_embedded_video()
                    && !surface_video
                    && matches!(
                        self.session.state,
                        PlaybackState::Playing
                            | PlaybackState::Buffering
                            | PlaybackState::Paused
                    )
            };
            let animating =
                self.player_chrome_visible || self.chrome_alpha > 0.05 || self.player_panel != PlayerPanel::None;
            if player_open && (animating || soft_video) {
                let ms = if soft_video && !animating {
                    // Caps / prefetch only — don't spin at full GUI Hz on a black overlay.
                    self.display_caps.gui_period_ms().max(250)
                } else {
                    self.display_caps.gui_period_ms().max(100)
                };
                iced::time::every(std::time::Duration::from_millis(ms))
                    .map(|_| Message::PlayerChromeTick)
            } else {
                Subscription::none()
            }
        };
        // Mosaic fling coast — independent of player tick so browse works while idle.
        let coast_tick = if self.browse_coast_vy.abs() >= 2.0 {
            iced::time::every(std::time::Duration::from_millis(
                self.display_caps.gui_period_ms().max(16),
            ))
            .map(|_| Message::BrowseCoastTick)
        } else {
            Subscription::none()
        };
        let keys = {
            #[cfg(target_os = "android")]
            let player_open = self.player_embedded || self.player_id.is_some();
            #[cfg(not(target_os = "android"))]
            let player_open = self.player_id.is_some();
            if player_open {
                event::listen_with(map_player_hotkeys)
            } else {
                #[cfg(target_os = "android")]
                {
                    event::listen_with(map_android_back)
                }
                #[cfg(not(target_os = "android"))]
                {
                    Subscription::none()
                }
            }
        };
        #[cfg(target_os = "android")]
        let saf = iced::time::every(std::time::Duration::from_millis(250)).map(|_| Message::SafPoll);
        #[cfg(not(target_os = "android"))]
        let saf = Subscription::<Message>::none();
        if let Ok(mut slot) = PLAYER_PTR_WIN.lock() {
            *slot = self.player_id;
        }
        let pointer = if self.player_id.is_some() {
            event::listen_with(map_player_pointer)
        } else {
            Subscription::none()
        };
        let catalog_sync = iced::time::every(std::time::Duration::from_secs(60))
            .map(|_| Message::CatalogFreshnessCheck);
        let notices = if self.notices.is_empty() {
            Subscription::none()
        } else {
            iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::NoticeTick)
        };
        let wake = iced::time::every(std::time::Duration::from_secs(2)).map(|_| Message::WakeProbe);
        let party = iced::time::every(std::time::Duration::from_millis(200))
            .map(|_| Message::PartyPoll);
        Subscription::batch([
            closes, close_req, opens, moves, tick, chrome_tick, coast_tick, keys, pointer, saf,
            catalog_sync, wake, party, notices,
        ])
    }

    fn open_or_focus_player(&self) -> Task<Message> {
        #[cfg(target_os = "android")]
        {
            if let Some(id) = self.main_id {
                return Task::done(Message::PlayerWindowOpened(id));
            }
            return Task::none();
        }
        #[cfg(not(target_os = "android"))]
        {
            if let Some(id) = self.player_id {
                // The window is already on screen. Focusing it again is
                // SetForegroundWindow: the next episode would jump in front
                // of whatever the viewer was doing.
                return self.sync_player_layout_task(id);
            }
            let (_id, open) = window::open(window::Settings {
                // 16:9 client. A mismatched box (the old 1120×720) letterboxes
                // every broadcast before the picture's own ratio is known.
                size: Size::new(1280.0, 720.0),
                min_size: Some(Size::new(480.0, 270.0)),
                position: window::Position::Centered,
                icon: app_icon(),
                exit_on_close_request: false,
                level: if std::env::var_os("FLUXPLAY_AUTO_PLAY").is_some() {
                    window::Level::AlwaysOnTop
                } else {
                    window::Level::Normal
                },
                ..Default::default()
            });
            open.map(Message::PlayerWindowOpened)
        }
    }

    /// Leave maximized state, then fill the monitor the window is actually on.
    fn commit_player_fullscreen(&self, id: window::Id, gen: u64) -> Task<Message> {
        window::maximize(id, false).chain(Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(220)).await;
                (id, gen)
            },
            |(id, gen)| Message::PlayerFullscreenCommit(id, gen),
        ))
    }

    fn sync_player_layout_task(&self, id: window::Id) -> Task<Message> {
        window::position(id).then(move |position| {
            window::size(id).then(move |size| {
                window::scale_factor(id).map(move |scale| Message::PlayerLayout {
                    position,
                    size,
                    scale,
                })
            })
        })
    }

    fn apply_player_layout(&mut self, position: Option<Point>, size: Size, scale: f32) {
        let scale = if scale > 0.05 { scale } else { 1.0 };
        if let Some(p) = position {
            self.player_pos = Some(p);
        }
        self.player_logical_size = size;
        // Soft-render uses the full window — chrome is an overlay and must NOT shrink the
        // stage (shrinking caused size oscillation / flicker when chrome toggled).
        let w = ((size.width).max(160.0) * scale).round() as u32;
        let h = ((size.height).max(120.0) * scale).round() as u32;
        let w = (w & !1).max(2);
        let h = (h & !1).max(2);
        let rect = VideoRect::detached(w, h);
        // Detect portrait↔landscape even during fullscreen settle freeze.
        let mut orient_flip = false;
        if let Some(prev) = self.session.native.video_rect() {
            orient_flip = (prev.w >= prev.h) != (w >= h);
        }
        // Fullscreen settle: compositor emits a burst of sizes — keep stage sticky
        // for tiny jitter, but never across orientation flips / PiP size jumps.
        if let Some(until) = self.player_layout_freeze_until {
            let large_delta = self.session.native.video_rect().is_some_and(|prev| {
                let dw = (prev.w as i32 - w as i32).unsigned_abs();
                let dh = (prev.h as i32 - h as i32).unsigned_abs();
                dw > 64 || dh > 64
            });
            if std::time::Instant::now() < until
                && !orient_flip
                && !large_delta
                && !self.pip_mode
            {
                return;
            }
            if std::time::Instant::now() >= until {
                self.player_layout_freeze_until = None;
            }
        }
        // Hysteresis: must match soft-pull hysteresis (±16px / 8%) or video_rect drifts
        // while pull stays sticky → FFmpeg want_w mismatch discards frames (black flashes).
        // Bypass when aspect ratio diverges (FS/PiP shrink) — Fill would non-uniform stretch.
        if !orient_flip {
            if let Some(prev) = self.session.native.video_rect() {
                let aspect_delta = {
                    let pa = prev.w.max(1) as f32 / prev.h.max(1) as f32;
                    let na = w.max(1) as f32 / h.max(1) as f32;
                    ((pa - na) / pa).abs() > 0.05
                };
                if !aspect_delta {
                    let dw = (prev.w as i32 - w as i32).unsigned_abs();
                    let dh = (prev.h as i32 - h as i32).unsigned_abs();
                    if dw <= 16 && dh <= 16 {
                        return;
                    }
                    let pw = prev.w.max(1) as f32;
                    let ph = prev.h.max(1) as f32;
                    if (w as f32 - pw).abs() / pw < 0.08 && (h as f32 - ph).abs() / ph < 0.08 {
                        return;
                    }
                }
            }
        }
        tracing::debug!(?rect, %scale, "player embed stage size");
        self.session.set_video_rect(rect);
        #[cfg(target_os = "android")]
        if self.session.native.android_surface_present() {
            let chrome = if self.player_chrome_visible && !self.pip_mode {
                self.layout_metrics().player_chrome_h
                    + crate::theme::TOOLBAR_OUTER_PAD
                    + self.system_insets.3
            } else {
                0.0
            };
            crate::android_bridge::layout_video_surface_chrome_inset_dp(chrome);
        }
        if orient_flip {
            // Drop stale GPU frame — Fill into the new stage would non-uniform stretch.
            self.invalidate_soft_stage(true);
        }
        // Keep soft vf ceiling aligned with live stage (portrait rotate used to keep landscape scale=).
        #[cfg(target_os = "android")]
        if !self.session.native.android_surface_present()
            && self.session.channel.is_some()
            && matches!(
                self.session.state,
                PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Paused
            )
        {
            let (rw, rh) = self.soft_present_wh();
            let vf = fluxplay_player::SoftBudget {
                max_w: rw,
                max_h: rh,
                video_hz: 30,
                gui_hz: 60,
            }
            .vf_scale();
            self.session.native.options_mut().android_soft_vf = Some(vf.clone());
            self.session.soft_vf_prefix = Some(vf);
            let _ = self.session.refresh_soft_filters();
        }
    }

    /// Sticky soft RGBA request size (window-scaled, capped, hysteresis vs last frame).
    fn soft_present_wh(&self) -> (u32, u32) {
        let frozen = self
            .player_layout_freeze_until
            .is_some_and(|t| std::time::Instant::now() < t);
        let main_w = (self.main_size.width.max(160.0).round() as u32).max(2) & !1;
        let main_h = (self.main_size.height.max(120.0).round() as u32).max(2) & !1;
        let (fw, fh) = self
            .session
            .native
            .video_rect()
            .map(|r| (r.w, r.h))
            .filter(|(rw, rh)| {
                // Stale landscape rect after portrait WindowResized → Fill stretches.
                (*rw >= *rh) == (main_w >= main_h)
            })
            .unwrap_or((main_w, main_h));
        #[cfg(target_os = "android")]
        let (max_w, max_h) = {
            let caps = crate::android_bridge::poll_android_device_caps().unwrap_or_else(|| {
                let cores = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4) as u32;
                fluxplay_player::AndroidDeviceCaps {
                    cores,
                    mediacodec_video: true,
                    refresh_hz: 60,
                    ..Default::default()
                }
            });
            let mut budget = caps.soft_budget_with_quality(self.settings.video_quality.max_wh());
            if std::env::var_os("FLUXPLAY_SOFT_UHD").is_some() {
                budget.max_w = budget.max_w.max(1920);
                budget.max_h = budget.max_h.max(1080);
            }
            (budget.max_w, budget.max_h)
        };
        #[cfg(not(target_os = "android"))]
        let (max_w, max_h) = {
            let uhd = std::env::var_os("FLUXPLAY_SOFT_UHD").is_some();
            let (qw, qh) = self
                .settings
                .video_quality
                .max_wh()
                .unwrap_or(if uhd { (3840, 2160) } else { (1920, 1080) });
            let mw = self.display_probe.monitor_w;
            let mh = self.display_probe.monitor_h;
            if mw >= 320 && mh >= 240 {
                (qw.min(mw & !1), qh.min(mh & !1))
            } else {
                (qw, qh)
            }
        };
        let scale = (max_w as f32 / fw.max(1) as f32)
            .min(max_h as f32 / fh.max(1) as f32)
            .min(1.0);
        let rw = ((fw as f32 * scale).round() as u32).max(2) & !1;
        let rh = ((fh as f32 * scale).round() as u32).max(2) & !1;
        if self.video_frame_wh.0 >= 2 && self.video_frame_wh.1 >= 2 {
            let (pw, ph) = self.video_frame_wh;
            // Portrait↔landscape must never sticky-reuse the old soft buffer.
            let orient_flip = (pw >= ph) != (rw >= rh);
            // Fullscreen settle freeze sticks size, but never across orient flips.
            if frozen && !orient_flip {
                return (pw, ph);
            }
            if !orient_flip {
                let aspect_delta = {
                    let pa = pw.max(1) as f32 / ph.max(1) as f32;
                    let na = rw.max(1) as f32 / rh.max(1) as f32;
                    ((pa - na) / pa).abs() > 0.05
                };
                if !aspect_delta {
                    let dw = (pw as i32 - rw as i32).unsigned_abs();
                    let dh = (ph as i32 - rh as i32).unsigned_abs();
                    if dw <= 16 && dh <= 16 {
                        return (pw, ph);
                    }
                    if (rw as f32 - pw as f32).abs() / (pw.max(1) as f32) < 0.08
                        && (rh as f32 - ph as f32).abs() / (ph.max(1) as f32) < 0.08
                    {
                        return (pw, ph);
                    }
                }
            }
        }
        (rw, rh)
    }

    fn clear_video_pending(&mut self) {
        if let Some((_, _, rgba, _)) = self.video_pending.take() {
            self.session.recycle_soft_rgba(rgba);
        }
    }

    /// Invalidate soft present pipeline (Stop / close / channel switch).
    /// Bumps gen so in-flight `iced_image::allocate` cannot resurrect a stale frame.
    fn invalidate_soft_stage(&mut self, clear_displayed: bool) {
        self.video_upload_busy = false;
        self.video_upload_gen = self.video_upload_gen.wrapping_add(1);
        self.clear_video_pending();
        self.playback_ended_since = None;
        if clear_displayed {
            self.player_fitted_wh = None;
            self.video_frame = None;
            self.video_allocation = None;
            self.video_allocation_hold = None;
            self.video_allocation_hold2 = None;
            self.video_allocation_hold3 = None;
            self.video_allocation_hold4 = None;
            self.video_frame_wh = (0, 0);
            self.clear_stage_picture();
            // Soft RGBA used the shared GPU image cache — bump poster handles (new iced
            // ids) then warm-fetch anything missing; art_warm alone skips RAM hits.
            self.refresh_browse_poster_gpu_textures();
            self.art_warm_cursor = 0;
            self.art_warm_active = !self.browse_index.is_empty();
        } else if !self.browse_index.is_empty() {
            // Playback ended or zapped without clearing the stage — mosaic can still
            // sample stale atlas regions from soft RGBA uploads.
            self.refresh_browse_poster_gpu_textures();
            self.art_warm_cursor = 0;
            self.art_warm_active = true;
        }
        #[cfg(target_os = "android")]
        {
            // Abandon focus only when tearing down display (Stop/close), not mid-zap.
            if clear_displayed && self.audio_focus_held {
                crate::android_bridge::abandon_audio_focus();
                self.audio_focus_held = false;
            }
            if clear_displayed {
                self.pause_cause = PauseCause::None;
            }
        }
    }

    /// Pull one soft frame and start GPU allocate. Caller must ensure `!video_upload_busy`.
    fn enqueue_soft_video_frame(&mut self) -> Option<Task<Message>> {
        let (rw, rh) = self.soft_present_wh();
        let hz = self.display_probe.monitor_hz.max(self.display_caps.video_hz);
        self.session.set_present_hz(hz);
        let (w, h, rgba) = self.session.pull_video_frame(rw, rh)?;
        // Keep sticky pull size; only commit WH to layout/caps after GPU Ok.
        let gen = self.video_upload_gen;
        self.video_upload_busy = true;
        let handle = ImageHandle::from_rgba(w, h, rgba);
        Some(iced_image::allocate(handle).map(move |result| Message::VideoFrameAllocated {
            gen,
            w,
            h,
            result,
        }))
    }

    /// GPU stage path: hand the newest decoded frame to the shader stage and
    /// give uploaded buffers back to the player.
    #[cfg(not(target_os = "android"))]
    fn present_stage_frame(&mut self) {
        for spent in crate::video_stage::take_spent() {
            self.session.recycle_video_frame(spent);
        }
        if !self.session.has_embedded_video()
            || !matches!(
                self.session.state,
                PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Paused
            )
            || !self.session.frame_needs_redraw()
        {
            return;
        }
        let (rw, rh) = self.soft_present_wh();
        let hz = self.display_probe.monitor_hz.max(self.display_caps.video_hz);
        self.session.set_present_hz(hz);
        crate::video_stage::set_aspect(self.session.aspect.ratio());
        let Some(frame) = self.session.pull_frame(rw, rh) else {
            return;
        };
        if frame.layout == fluxplay_player::PixelLayout::Rgba {
            // libmpv renders at the requested size: keep it sticky.
            self.video_frame_wh = (frame.width, frame.height);
            if let fluxplay_player::FrameData::Cpu(px) = &frame.data {
                self.session
                    .observe_opening_frame(frame.width, frame.height, frame.pitch, px);
            }
        }
        if let Some(unshown) = crate::video_stage::present(frame) {
            self.session.recycle_video_frame(unshown);
        }
        if !self.stage_picture {
            self.stage_picture = true;
            // The image path is done for this playback: free its atlas pixels.
            self.video_upload_gen = self.video_upload_gen.wrapping_add(1);
            self.video_upload_busy = false;
            self.clear_video_pending();
            self.video_frame = None;
            self.video_allocation = None;
            self.video_allocation_hold = None;
            self.video_allocation_hold2 = None;
            self.video_allocation_hold3 = None;
            self.video_allocation_hold4 = None;
        }
    }

    fn clear_stage_picture(&mut self) {
        self.stage_picture = false;
        #[cfg(not(target_os = "android"))]
        if let Some(frame) = crate::video_stage::clear() {
            self.session.recycle_video_frame(frame);
        }
    }

    /// Snap the floating player to the picture's display ratio once per source.
    /// The decode size stays the source; only the window changes, so 16:9 TV
    /// and scope films aren't letterboxed by a 14:9 box. Fullscreen and a
    /// near-monitor window are left alone (cropping those bars would cut the image).
    fn fit_player_to_video(&mut self) -> Task<Message> {
        #[cfg(target_os = "android")]
        {
            return Task::none();
        }
        #[cfg(not(target_os = "android"))]
        {
            if self.player_fullscreen || self.pip_mode {
                return Task::none();
            }
            let Some(id) = self.player_id else {
                return Task::none();
            };
            let Some((dw, dh)) = self.session.native.video_source_wh() else {
                return Task::none();
            };
            if dw < 16 || dh < 16 || self.player_fitted_wh == Some((dw, dh)) {
                return Task::none();
            }
            let aspect = dw as f32 / dh as f32;
            if !(0.45..=3.2).contains(&aspect) {
                self.player_fitted_wh = Some((dw, dh));
                return Task::none();
            }
            let size = self.player_logical_size;
            let (nw, nh, skip) = player_client_for_aspect(size.width, size.height, aspect);
            self.player_fitted_wh = Some((dw, dh));
            if skip {
                return Task::none();
            }
            self.player_layout_freeze_until =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(450));
            window::resize(id, Size::new(nw, nh))
        }
    }

    /// Film or series in the player: the catalog window can leave the desktop.
    fn library_playback(&self) -> bool {
        self.player_id.is_some()
            && self.session.channel.as_ref().is_some_and(|c| {
                matches!(c.kind, ContentKind::Vod | ContentKind::Series)
            })
            && matches!(
                self.session.state,
                PlaybackState::Opening
                    | PlaybackState::Playing
                    | PlaybackState::Buffering
                    | PlaybackState::Paused
            )
    }

    /// Hide the browser and drop poster RAM. Catalog data stays, so coming back is immediate.
    /// Live TV is left on screen. Android keeps a single window, so this is a no-op there.
    fn park_browser_for_library(&mut self) -> Task<Message> {
        #[cfg(target_os = "android")]
        {
            Task::none()
        }
        #[cfg(not(target_os = "android"))]
        {
            if self.browse_parked || !self.library_playback() {
                return Task::none();
            }
            let Some(main) = self.main_id else {
                return Task::none();
            };
            let Some(player) = self.player_id else {
                return Task::none();
            };
            self.browse_parked = true;
            self.browse_coast_vy = 0.0;
            self.art_warm_active = false;
            self.pending_images.clear();
            // Keep decoded posters. Dropping them made the grid and the episode
            // stills come back blank; the restore path only repaints new handles.
            window::set_mode::<Message>(main, window::Mode::Hidden)
                .chain(window::gain_focus(player))
        }
    }

    /// Show the browser again the moment the player window is gone.
    fn restore_browser(&mut self) -> Task<Message> {
        #[cfg(target_os = "android")]
        {
            Task::none()
        }
        #[cfg(not(target_os = "android"))]
        {
            if !self.browse_parked {
                return Task::none();
            }
            self.browse_parked = false;
            let warm = self.warm_after_restore();
            let Some(main) = self.main_id else {
                return warm;
            };
            Task::batch([
                window::set_mode::<Message>(main, window::Mode::Windowed)
                    .chain(window::gain_focus(main)),
                warm,
            ])
        }
    }

    /// Posters of the page that was open, from disk. The catalog itself never left memory.
    fn warm_after_restore(&mut self) -> Task<Message> {
        let mut urls = Vec::new();
        if let Some(v) = &self.vod_detail {
            let sid = v.source_id;
            if let Some(u) = crate::images::vod_poster_url(v) {
                urls.push((u, sid));
            }
        }
        if let Some(s) = &self.series_detail {
            let sid = s.source_id;
            if let Some(u) = crate::images::series_cover_url(s) {
                urls.push((u, sid));
            }
        }
        for url in self.cast_photos.values() {
            urls.push((url.clone(), None));
        }
        let detail = self.prefetch_art(urls, 24);
        // Video frames share the GPU atlas with posters. New handle ids force
        // a re-upload, including the episode stills of the open series.
        self.refresh_browse_poster_gpu_textures();
        Task::batch([
            self.prefetch_visible_art(),
            self.prefetch_episode_stills(),
            detail,
        ])
    }

    /// Nothing to paint while the browser is parked. Keeps player ticks off the mosaic.
    fn view_parked_browser(&self) -> Element<'_, Message> {
        container(Space::new())
            .width(Fill)
            .height(Fill)
            .into()
    }

    fn close_player_window(&mut self) -> Task<Message> {
        self.player_fullscreen = false;
        self.player_chrome_visible = true;
        self.player_panel = PlayerPanel::None;
        #[cfg(target_os = "android")]
        {
            self.android_force_soft = false;
            self.android_want_surface_upgrade = false;
            crate::android_bridge::set_immersive_mode(false);
            crate::android_bridge::set_keep_screen_on(false);
            crate::android_bridge::set_force_landscape(false);
            if self.audio_focus_held {
                crate::android_bridge::abandon_audio_focus();
                self.audio_focus_held = false;
            }
            self.player_embedded = false;
            self.player_id = None;
            return Task::none();
        }
        #[cfg(not(target_os = "android"))]
        {
            if let Some(id) = self.player_id.take() {
                return window::close(id);
            }
            Task::none()
        }
    }

    fn persist(&self) {
        storage::save(&storage::PersistedState {
            settings: self.settings.clone(),
            sources: self.sources.clone(),
        });
    }

    fn reload_all_task(&self) -> Task<Message> {
        let sources: Vec<MediaSource> = self.sources.iter().filter(|s| s.enabled).cloned().collect();
        if sources.is_empty() {
            return Task::none();
        }
        // A warm API cache would replay yesterday's list, checksum-match, and
        // hide every film and series added since.
        fluxplay_providers::clear_xtream_cache();
        // Sequential: one portal connection budget — parallel reload hammers 429.
        Task::perform(
            async move {
                let mut results = Vec::with_capacity(sources.len());
                for src in sources {
                    let id = src.id;
                    let result = load_one(src).await.map(Arc::new);
                    results.push((id, result));
                }
                results
            },
            |results| {
                Message::SourcesBatchLoaded(results)
            },
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let _prof = fluxplay_core::InteractionGuard::begin("update", profile_message_label(&message));
        if let Some(action) = ui_action_label(&message) {
            tracing::info!(target: "fluxplay::ui", %action, "ui.interaction");
        }
        if let Some(until) = self.suppress_open_until {
            if std::time::Instant::now() >= until {
                self.suppress_open_until = None;
            } else if matches!(
                message,
                Message::OpenSeries(_)
                    | Message::OpenVodDetail(_)
                    | Message::PlayChannelId(..)
                    | Message::PlayChannel(_)
            ) {
                self.pressed_mosaic = None;
                return Task::none();
            }
        }
        if self.swallow_tile_release {
            // Image loads and player ticks arrive between the category press and
            // the release that lands on the poster underneath. Those must not
            // spend the one-shot, or the poster still opens.
            if !swallow_noise(&message) {
                self.swallow_tile_release = false;
                if matches!(
                    message,
                    Message::OpenSeries(_)
                        | Message::OpenVodDetail(_)
                        | Message::PlayChannelId(..)
                        | Message::PlayChannel(_)
                ) {
                    self.pressed_mosaic = None;
                    return Task::none();
                }
            }
        }
        match message {
            Message::MainWindowOpened(id) => {
                tracing::debug!(?id, "main window opened");
                self.main_id = Some(id);
                #[cfg(target_os = "android")]
                {
                    // Idle browse must not hold the OLED awake (onCreate no longer sets the
                    // flag; clear any leftover from a prior play session).
                    crate::android_bridge::set_keep_screen_on(false);
                    crate::android_bridge::set_immersive_mode(false);
                    // First paint before SafPoll (250ms) / Resized must already know cutout +
                    // gesture-bar insets — else chrome sits under Pixel bars for a frame.
                    crate::android_bridge::refresh_system_insets();
                    self.system_insets = crate::android_bridge::system_insets_dp();
                    self.layout_cache = None;
                    // Sync real Activity/freeform bounds only.
                    // Never resize-to-monitor: on Waydroid freeform it collapses the surface (h=0).
                    return android_sync_size(id);
                }
                #[cfg(not(target_os = "android"))]
                {
                    let size_task =
                        window::size(id).map(move |size| Message::WindowResized { id, size });
                    self.refresh_display_caps(true);
                    if std::env::var_os("FLUXPLAY_AUTO_PLAY").is_some() {
                        if let Some(ch) = self.pick_autoplay_channel() {
                            tracing::info!(name = %ch.name, "FLUXPLAY_AUTO_PLAY → opening player");
                            return Task::batch([size_task, Task::done(Message::PlayChannel(ch))]);
                        }
                    }
                    if let Ok(vod_id) = std::env::var("FLUXPLAY_AUTO_VOD") {
                        if let Some(v) = self
                            .bundle
                            .vod
                            .iter()
                            .find(|v| v.id == vod_id || v.name.contains(&vod_id))
                            .cloned()
                        {
                            tracing::info!(name = %v.name, "FLUXPLAY_AUTO_VOD → opening detail");
                            self.tab = Tab::Vod;
                            return Task::batch([
                                size_task,
                                Task::done(Message::OpenVodDetail(v.id)),
                            ]);
                        }
                    }
                    return size_task;
                }
            }
            Message::PlayerWindowOpened(id) => {
                tracing::debug!(?id, "player window opened");
                self.player_id = Some(id);
                self.player_chrome_visible = true;
                self.player_pointer_at = Some(std::time::Instant::now());
                self.player_fullscreen = false;
                self.refresh_display_caps(true);
                if std::env::var("FLUXPLAY_AUTO_PANEL").ok().as_deref() == Some("more") {
                    self.player_panel = PlayerPanel::More;
                }
                #[cfg(target_os = "android")]
                {
                    self.player_embedded = true;
                    if self.main_id.is_none() {
                        self.main_id = Some(id);
                    }
                    // Soft video is edge-to-edge — hide system bars while playing.
                    self.player_fullscreen = true;
                    self.player_chrome_visible = true;
                    self.chrome_alpha = 1.0;
                    self.chrome_vel = 0.0;
                    self.player_pointer_at = Some(std::time::Instant::now());
                    crate::android_bridge::set_immersive_mode(true);
                    // Force landscape UX while playing (portrait mode dropped).
                    crate::android_bridge::set_force_landscape(true);
                    // Do NOT show SurfaceView here — Z-order on-top blacks the browse UI.
                    // Bind warms it only at play start.
                }
                let layout = self.sync_player_layout_task(id);
                let park = self.park_browser_for_library();
                if std::env::var_os("FLUXPLAY_AUTO_FULLSCREEN").is_some() {
                    return Task::batch([layout, park, Task::done(Message::ToggleFullscreen)]);
                }
                return Task::batch([layout, park]);
            }
            Message::PlayerLayoutDirty(id) => {
                // Catch-up after fullscreen settle — never drop for freeze (that's the point).
                #[cfg(target_os = "android")]
                let dirty = self.player_id == Some(id)
                    || (self.player_embedded && self.main_id == Some(id));
                #[cfg(not(target_os = "android"))]
                let dirty = self.player_id == Some(id);
                if dirty {
                    return self.sync_player_layout_task(id);
                }
            }
            Message::WindowResized { id, size } => {
                if self.main_id.is_none() {
                    self.main_id = Some(id);
                }
                // Pixel BLAST re-emits same-size NativeWindowResized at display Hz.
                // iced_winit skips redraw for identical physical size, but the event still
                // reaches here — must not sync layout / chrome SurfaceView every tick.
                let size_changed = if self.main_id == Some(id) {
                    (self.main_size.width - size.width).abs() > 0.5
                        || (self.main_size.height - size.height).abs() > 0.5
                } else {
                    true
                };
                if self.main_id == Some(id) {
                    // Guard against zero / garbage sizes from early surface churn.
                    if size.width >= 32.0 && size.height >= 32.0 {
                        self.main_size = size;
                        if size_changed {
                            self.refresh_layout_cache();
                            // Approx content viewport until the first scrollable on_scroll.
                            self.browse_view_h = (size.height * 0.62).max(240.0);
                        }
                        #[cfg(target_os = "android")]
                        {
                            if size_changed {
                                crate::android_bridge::refresh_system_insets();
                                let prev_insets = self.system_insets;
                                self.system_insets = crate::android_bridge::system_insets_dp();
                                // Rotate / fold: refresh caps + rebind Surface session.
                                if self.system_insets != prev_insets || size_changed {
                                    self.layout_cache = None;
                                    crate::android_bridge::stabilize_android_session();
                                    self.refresh_display_caps(true);
                                    self.maintain_android_surface_session(true);
                                }
                            }
                        }
                        if size_changed {
                            tracing::info!(w = size.width, h = size.height, "main window size");
                        }
                    } else {
                        tracing::warn!(
                            w = size.width,
                            h = size.height,
                            "ignored degenerate window size"
                        );
                    }
                }
                if self.player_id == Some(id) {
                    // Skip resize storms while fullscreen mode is settling —
                    // but never drop portrait↔landscape (Fill would stretch).
                    let flip = self.session.native.video_rect().is_some_and(|prev| {
                        (prev.w >= prev.h) != (size.width >= size.height)
                    });
                    if self
                        .player_layout_freeze_until
                        .is_some_and(|t| std::time::Instant::now() < t)
                        && !flip
                    {
                        return Task::none();
                    }
                    if !size_changed && !flip {
                        return Task::none();
                    }
                    // While the border is dragged, the last frame is scaled to
                    // the window. A new decode size on every pixel froze the picture.
                    self.player_resize_gen = self.player_resize_gen.wrapping_add(1);
                    let gen = self.player_resize_gen;
                    return Task::perform(
                        async move {
                            tokio::time::sleep(std::time::Duration::from_millis(180)).await;
                            (id, gen)
                        },
                        |(id, gen)| Message::PlayerResizeSettled { id, gen },
                    );
                }
            }
            Message::PlayerLayout {
                position,
                size,
                scale,
            } => {
                self.apply_player_layout(position, size, scale);
                self.refresh_display_caps(false);
            }
            Message::WindowClosed(id) => {
                let player_gone = self.player_id == Some(id)
                    || (self.browse_parked && self.main_id != Some(id));
                if player_gone {
                    self.store_watch_progress(true);
                    if self.player_id == Some(id) {
                        self.player_id = None;
                    }
                    self.player_pos = None;
                    self.video_frame = None;
                    self.video_allocation = None;
                    self.video_allocation_hold = None;
                    self.video_allocation_hold2 = None;
                    self.video_allocation_hold3 = None;
                    self.video_allocation_hold4 = None;
                    self.video_frame_wh = (0, 0);
                    self.invalidate_soft_stage(true);
                    self.player_fullscreen = false;
                    self.player_chrome_visible = true;
                    self.player_panel = PlayerPanel::None;
                    self.session.stop();
                    self.status = "Lecteur fermé".into();
                    return self.restore_browser();
                }
                if self.main_id == Some(id) {
                    self.store_watch_progress(true);
                    self.main_id = None;
                    self.session.stop();
                    if let Some(pid) = self.player_id.take() {
                        return Task::batch([window::close(pid), iced::exit()]);
                    }
                    return iced::exit();
                }
            }
            Message::PlayerCloseRequested(id) => {
                if self.player_id == Some(id) {
                    self.player_exit_ask = true;
                    self.player_chrome_visible = true;
                    self.player_pointer_at = Some(std::time::Instant::now());
                    return Task::none();
                }
                if self.main_id == Some(id) {
                    self.store_watch_progress(true);
                    self.session.stop();
                    return iced::exit();
                }
            }
            Message::PlayerDismissExit => {
                self.player_exit_ask = false;
            }
            Message::PlayerQuitApp => {
                self.player_exit_ask = false;
                self.store_watch_progress(true);
                self.session.stop();
                self.invalidate_soft_stage(true);
                return iced::exit();
            }
            Message::ClosePlayerWindow => {
                self.player_exit_ask = false;
                // Cancel any deferred Stop→close and tear down now.
                self.player_close_gen = self.player_close_gen.wrapping_add(1);
                self.store_watch_progress(true);
                self.session.stop();
                self.invalidate_soft_stage(true);
                #[cfg(target_os = "android")]
                {
                    crate::android_bridge::release_video_surface_wid();
                    crate::android_bridge::set_hdr_color_mode(false);
                }
                if self.status != "Arrêté" {
                    self.status = "Arrêté".into();
                }
                // The catalog window is hidden during a film. Show it again
                // before the player window goes, or FluxPlay looks closed.
                let restore = self.restore_browser();
                let close = self.close_player_window();
                return restore.chain(close);
            }
            Message::ClosePlayerWindowDeferred(gen) => {
                if gen != self.player_close_gen {
                    tracing::debug!(
                        gen,
                        current = self.player_close_gen,
                        "deferred player close ignored — newer play/stop"
                    );
                    return Task::none();
                }
                self.session.stop();
                self.invalidate_soft_stage(true);
                #[cfg(target_os = "android")]
                {
                    crate::android_bridge::release_video_surface_wid();
                    crate::android_bridge::set_hdr_color_mode(false);
                }
                if self.status != "Arrêté" {
                    self.status = "Arrêté".into();
                }
                return self.close_player_window();
            }
            Message::PartyPoll => return self.on_party_poll(),
            Message::PartyCreate => {
                if self.party.room_code.is_empty() {
                    let code = crate::party::make_code();
                    self.party.room_code = code.clone();
                    self.party.owner = true;
                    self.party.people = 1;
                    self.party.order(crate::party::Order::Room(code));
                    self.party.refresh_qr();
                    self.cache_remote_qr();
                }
                self.party.status = format!(
                    "Salle {} — jusqu'à 10 personnes. Lancez un film puis Diffusez.",
                    self.party.room_code
                );
            }
            Message::PartyJoin => {
                let draft = self.party.join_draft.trim().to_string();
                if draft.starts_with("fp1|") {
                    return self.update(Message::PartyPair(draft));
                }
                let Some(link) = self.party.paired.clone() else {
                    self.party.status = "Associez d'abord la télécommande dans Réglages.".into();
                    return Task::none();
                };
                let code = if draft.is_empty() {
                    link.room.clone()
                } else {
                    draft
                };
                if code.is_empty() {
                    self.party.status = "Indiquez le code de la salle.".into();
                    return Task::none();
                }
                let mut link = link;
                link.room = code;
                return party_http(link, |l| {
                    format!("/v1/cinema/join?k={}&c={}", l.token, l.room)
                });
            }
            Message::PartyLeave => {
                self.party.owner = false;
                self.party.broadcasting = false;
                self.party.guest_play = None;
                self.session.playback_url = None;
                if let Some(link) = self.party.paired.clone() {
                    if !link.room.is_empty() {
                        return party_http(link, |l| {
                            format!("/v1/cinema/leave?k={}&c={}", l.token, l.room)
                        });
                    }
                }
                self.party.order(crate::party::Order::CloseRoom);
                self.party.room_code.clear();
                self.party.people = 0;
                self.party.refresh_qr();
                self.cache_remote_qr();
                self.party.status = "Salle quittée.".into();
            }
            Message::PartyBroadcast => {
                let Some(ch) = self.session.channel.clone() else {
                    self.party.status = "Lancez d'abord un film, une série ou une chaîne.".into();
                    return Task::none();
                };
                if ch.stream_url.contains("/v1/cinema/live.ts") {
                    self.party.status = "Vous regardez déjà la salle.".into();
                    return Task::none();
                }
                if self.party.room_code.is_empty() {
                    let code = crate::party::make_code();
                    self.party.room_code = code.clone();
                    self.party.owner = true;
                    self.party.people = 1;
                    self.party.order(crate::party::Order::Room(code));
                    self.party.refresh_qr();
                    self.cache_remote_qr();
                }
                self.party.owner = true;
                self.party.broadcasting = true;
                self.party.want_broadcast = false;
                match self.open_with_failover(ch) {
                    Ok(()) => {
                        self.party.status = "Diffusion lancée. Les invités collent le lien ou le code.".into();
                    }
                    Err(e) => {
                        self.party.broadcasting = false;
                        self.session.playback_url = None;
                        self.party.status = e.to_string();
                    }
                }
            }
            Message::PartyStop => {
                self.party.broadcasting = false;
                self.party.want_broadcast = false;
                self.session.playback_url = None;
                self.party.order(crate::party::Order::StopBroadcast);
                self.party.status = "Diffusion arrêtée.".into();
            }
            Message::PartyQuality(h) => {
                self.party.quality = h;
                self.party.status = match h {
                    720 => "Qualité reçue : 720p".into(),
                    480 => "Qualité reçue : 480p".into(),
                    _ => "Qualité reçue : 1080p (source de l'hôte)".into(),
                };
                if self.party.owner && self.party.broadcasting {
                    if let Some(url) = self.party.relay_url() {
                        self.session.playback_url = Some(url);
                        if let Some(ch) = self.session.channel.clone() {
                            let _ = self.session.open_channel(ch);
                        }
                    }
                } else if let Some(url) = self.party.guest_play.clone() {
                    let play = with_height(&url, h);
                    let play = crate::party::playable(&play);
                    return self.play_cinema_guest(&play);
                }
            }
            Message::PartyDraft(s) => self.party.draft = s,
            Message::PartyJoinDraft(s) => self.party.join_draft = s,
            Message::PartyPair(text) => {
                let Some(link) = crate::party::parse_link(&text) else {
                    self.party.status = "Lien illisible. Collez le texte fp1|… affiché sous le QR.".into();
                    return Task::none();
                };
                self.party.paired = Some(link.clone());
                if !link.room.is_empty() {
                    self.party.room_code = link.room.clone();
                    self.party.join_draft = link.room.clone();
                }
                if link.room.is_empty() {
                    self.party.status = "Télécommande associée. Lecture, pause, volume et épisode.".into();
                    return Task::none();
                }
                self.party.status = format!("Lien reçu, entrée dans la salle {}…", link.room);
                return party_http(link, |l| format!("/v1/cinema/join?k={}&c={}", l.token, l.room));
            }
            Message::PartyRemote(op) => {
                if let Some(link) = self.party.paired.clone() {
                    let q = remote_query(&link, op);
                    return Task::perform(
                        async move {
                            let (tx, rx) = std::sync::mpsc::channel();
                            std::thread::spawn(move || {
                                let r = crate::party::http_get(&link, &q).map(|(b, v)| (b, v));
                                let _ = tx.send(r);
                            });
                            loop {
                                if let Ok(r) = rx.try_recv() {
                                    return r;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                            }
                        },
                        Message::PartyHttp,
                    );
                }
                return self.apply_remote(op);
            }
            Message::PartyCopy => {
                let payload = self.party.payload();
                if payload.is_empty() {
                    self.party.status = "Le lien n'est pas encore prêt.".into();
                } else {
                    self.party.status = "Lien copié.".into();
                    return iced::clipboard::write(payload);
                }
            }
            Message::DismissNotice(id) => {
                self.notices.retain(|n| n.id != id);
            }
            Message::NoticeTick => {
                let now = std::time::Instant::now();
                self.notices.retain(|n| n.until > now);
            }
            Message::ToggleEpisodeAlert => {
                let Some(detail) = self.series_detail.clone() else {
                    return Task::none();
                };
                let count = detail
                    .seasons
                    .iter()
                    .map(|s| s.episodes.len() as u32)
                    .sum::<u32>();
                if count == 0 {
                    self.status = "Cette série n'a pas encore d'épisode.".into();
                    return Task::none();
                }
                if let Some(i) = self
                    .settings
                    .episode_alerts
                    .iter()
                    .position(|a| a.series_id == detail.id)
                {
                    self.settings.episode_alerts.remove(i);
                    self.status = "Alerte épisodes retirée".into();
                } else {
                    self.settings.episode_alerts.push(
                        fluxplay_core::models::EpisodeAlert {
                            series_id: detail.id,
                            name: detail.name,
                            episodes: count,
                        },
                    );
                    self.status = "Tu seras prévenu quand un épisode sort.".into();
                }
                self.persist();
            }
            Message::PartyHttp(result) => {
                match result {
                    Ok((body, via)) => {
                        if body.starts_with("ok") && body.contains("play=") {
                            if let Some(link) = self.party.paired.clone() {
                                if let Some(play) = crate::party::materialize_play(&body, &via, &link) {
                                    self.party.guest_play = Some(play.clone());
                                    self.party.owner = false;
                                    self.party.broadcasting = false;
                                    let people = body
                                        .lines()
                                        .find_map(|l| l.strip_prefix("people="))
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(0);
                                    self.party.people = people;
                                    if let Some(code) = body.lines().find_map(|l| l.strip_prefix("code=")) {
                                        self.party.room_code = code.to_string();
                                    }
                                    let play = with_height(&play, self.party.quality);
                                    let play = crate::party::playable(&play);
                                    self.party.status = format!(
                                        "Dans la salle {} · {people} personne(s). Qualité réglable en bas.",
                                        self.party.room_code
                                    );
                                    return self.play_cinema_guest(&play);
                                }
                            }
                        } else if body.starts_with("ok") {
                            self.party.status = "Commande envoyée.".into();
                        } else {
                            self.party.status = body.chars().take(160).collect();
                        }
                    }
                    Err(e) => self.party.status = e,
                }
            }
            Message::Tab(tab) => {
                tracing::debug!(?tab, "tab");
                let tab = if tab == Tab::Sources {
                    self.settings_open = Some(SettingsSection::Sources);
                    Tab::Settings
                } else {
                    tab
                };
                self.browse_scroll_y = 0.0;
                self.browse_scroll_vy = 0.0;
                self.cat_scroll_y = 0.0;
                self.tab = tab;
                self.shelf = Shelf::Catalog;
                self.browse_drag_moved = false;
                self.browse_dragging = false;
                self.cat_filter.clear();
                // Each tab has its own search ("Rechercher une série…"): a query typed
                // in Séries silently filtered Films / TV too.
                self.search.clear();
                self.search_debounce_gen = self.search_debounce_gen.wrapping_add(1);
                self.list_limit = self.browse_page();
                self.series_detail = None;
                self.vod_detail = None;
                self.detail_meta_loading = false;
                if tab == Tab::Settings && self.backends_cache.is_none() {
                    self.backends_cache = Some(detect_backends());
                    self.external_players = fluxplay_player::detect_external_players();
                }
                // Always land on All when opening VOD / Series (predictable browse).
                if tab == Tab::Vod {
                    self.selected_vod_category = Some("*".into());
                }
                if tab == Tab::Series {
                    self.selected_series_category = Some("*".into());
                }
                let mut tasks = vec![self.snap_browse_scroll_task(), self.snap_cat_scroll_task()];
                let parts = self.parts_for_tab();
                if self.catalog_parts != parts || self.slice_missing(parts) {
                    tasks.push(self.load_catalog_task(parts));
                } else {
                    tasks.push(self.rebuild_browse_index());
                    tasks.push(self.refresh_browse_art());
                    if tab == Tab::Series {
                        if let Some(fill) = self.backfill_series_credits_task() {
                            tasks.push(fill);
                        }
                    }
                }
                if matches!(tab, Tab::Vod | Tab::Series) {
                    if let Some(refresh) = self.catalog_refresh_if_due() {
                        tasks.push(refresh);
                    }
                }
                if tab == Tab::Vod && !self.xtream_vod_enrich_started {
                    self.xtream_vod_enrich_started = true;
                }
                if tab == Tab::Live || tab == Tab::Epg {
                    tasks.push(self.fetch_epg_for_visible_task());
                }
                return Task::batch(tasks);
            }
            Message::SearchChanged(s) => {
                self.search = s;
                self.search_debounce_gen = self.search_debounce_gen.wrapping_add(1);
                let gen = self.search_debounce_gen;
                return Task::perform(
                    async {
                        tokio::time::sleep(std::time::Duration::from_millis(220)).await;
                    },
                    move |_| Message::SearchApply(gen),
                );
            }
            Message::SearchApply(gen) => {
                if gen != self.search_debounce_gen {
                    return Task::none();
                }
                self.browse_scroll_y = 0.0;
                self.browse_scroll_vy = 0.0;
                self.list_limit = self.browse_page();
                return Task::batch([self.rebuild_browse_index(), self.refresh_browse_art()]);
            }
            Message::CatFilterChanged(s) => {
                self.cat_filter = s;
                self.rebuild_cat_entries();
            }
            Message::LoadMore => {
                self.suppress_open_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(350));
                self.pressed_mosaic = None;
                tracing::debug!(list_limit = self.list_limit, "load more");
                let page = self.browse_page();
                let base = self.list_limit.max(page);
                self.list_limit = base.saturating_add(page).min(10_000);
                self.art_warm_active = true;
                return Task::batch([self.refresh_browse_art(), self.queue_visible_ages()]);
            }
            Message::BrowseScrolled(y, view_h) => {
                let view_h = view_h.max(1.0);
                let now = std::time::Instant::now();
                if let Some(prev) = self.browse_scroll_at {
                    let dt = now.saturating_duration_since(prev).as_secs_f32().max(0.001);
                    let vy = (y - self.browse_scroll_y) / dt;
                    // Heavier smoothing → less LOD flicker on jittery trackpads.
                    self.browse_scroll_vy = self.browse_scroll_vy * 0.72 + vy * 0.28;
                } else {
                    self.browse_scroll_vy = 0.0;
                }
                // Decay leftover velocity so "fling" ends quickly after the finger stops.
                if (y - self.browse_scroll_y).abs() < 0.5 {
                    self.browse_scroll_vy *= 0.35;
                }
                self.browse_scroll_at = Some(now);
                // Always track scroll position — stale Y freezes virtualization mid-fling.
                self.browse_scroll_y = y;
                self.browse_view_h = view_h;

                let flinging = self.browse_flinging();
                let settled = self.browse_was_flinging && !flinging;
                self.browse_was_flinging = flinging;

                let slice = self.content_virtual_slice(y, view_h);
                if slice == self.browse_slice {
                    if flinging {
                        return Self::browse_settle_task();
                    }
                    if settled {
                        return Task::batch([
                            self.flush_pending_images(),
                            self.refresh_browse_art(),
                        ]);
                    }
                    return Task::none();
                }
                self.browse_slice = slice;
                // During fling: remount window (state already updated) but skip art storm.
                // Velocity only decays on new scroll events, so schedule a settle check:
                // without it the last fast wheel turn left art loading off for good.
                if flinging {
                    // Still fetch the posters now under the cursor. Waiting for
                    // the wheel to stop left films and series blank.
                    let art = if matches!(self.tab, Tab::Vod | Tab::Series)
                        && self.vod_detail.is_none()
                        && self.series_detail.is_none()
                    {
                        self.art_wait.clear();
                        self.prefetch_visible_art()
                    } else {
                        Task::none()
                    };
                    return Task::batch([Self::browse_settle_task(), art]);
                }
                let mut tasks = vec![self.flush_pending_images(), self.refresh_browse_art()];
                if matches!(self.tab, Tab::Live | Tab::Epg) {
                    self.ensure_epg_now();
                    tasks.push(self.fetch_epg_for_visible_task());
                }
                return Task::batch(tasks);
            }
            Message::BrowseSettle => {
                tracing::debug!(
                    vy = self.browse_scroll_vy,
                    pending = self.pending_images.len(),
                    "browse settle check"
                );
                let idle = self
                    .browse_scroll_at
                    .is_none_or(|at| at.elapsed() >= BROWSE_SETTLE_AFTER);
                if !idle {
                    // The check that woke too early must arm the next one.
                    // Otherwise the last tick of a fling never loads posters.
                    return Self::browse_settle_task();
                }
                self.browse_scroll_vy = 0.0;
                self.browse_was_flinging = false;
                self.browse_slice =
                    self.content_virtual_slice(self.browse_scroll_y, self.browse_view_h);
                let mut tasks = vec![self.flush_pending_images(), self.refresh_browse_art()];
                if matches!(self.tab, Tab::Live | Tab::Epg) {
                    self.ensure_epg_now();
                    tasks.push(self.fetch_epg_for_visible_task());
                }
                return Task::batch(tasks);
            }
            Message::BrowseScrollBy(dy) => {
                if dy.abs() < 0.01 {
                    return Task::none();
                }
                // Optimistic Y: scroll_by does not fire on_scroll until the next
                // redraw — update the mosaic overlay immediately so it never blanks.
                let content_h = self.browse_virtual_content_h();
                let max_scroll = (content_h - self.browse_view_h).max(1.0);
                self.browse_scroll_y =
                    (self.browse_scroll_y + dy).clamp(0.0, max_scroll);
                self.browse_slice =
                    self.content_virtual_slice(self.browse_scroll_y, self.browse_view_h);
                // The wheel is handled here, not by the scrollable. Load the rows
                // now; waiting for settle left the new posters blank.
                self.browse_scroll_at = Some(std::time::Instant::now());
                let art = if matches!(self.tab, Tab::Vod | Tab::Series)
                    && self.vod_detail.is_none()
                    && self.series_detail.is_none()
                {
                    self.prefetch_visible_art()
                } else {
                    Task::none()
                };
                return Task::batch([
                    iced::widget::operation::scroll_by(
                        iced::widget::Id::from(self.browse_scroll_widget_id()),
                        AbsoluteOffset { x: 0.0, y: dy },
                    ),
                    Self::browse_settle_task(),
                    art,
                ]);
            }
            Message::BrowseDragStart => {
                // iced mouse_area: content (tiles) update first; tiles use on_release
                // so press reaches this overlay → drag start without opening.
                self.browse_dragging = true;
                self.browse_drag_last_y = None;
                self.browse_drag_accum = 0.0;
                self.browse_drag_moved = false;
                self.browse_drag_last_dy = 0.0;
                self.browse_coast_vy = 0.0;
            }
            Message::BrowseDragAt(y) => {
                if !self.browse_dragging {
                    return Task::none();
                }
                if let Some(prev) = self.browse_drag_last_y {
                    let dy = prev - y;
                    self.browse_drag_last_y = Some(y);
                    if dy.abs() < 0.25 {
                        return Task::none();
                    }
                    // Touch slop (~12dp): below this, treat as tap; above, scroll.
                    self.browse_drag_accum += dy.abs();
                    const BROWSE_DRAG_SLOP: f32 = 12.0;
                    if self.browse_drag_accum < BROWSE_DRAG_SLOP {
                        return Task::none();
                    }
                    self.browse_drag_moved = true;
                    self.pressed_mosaic = None;
                    self.browse_drag_last_dy = dy;
                    // Rough px/s for art LOD (assume ~16ms between move events).
                    self.browse_scroll_vy = self.browse_scroll_vy * 0.5 + (dy / 0.016) * 0.5;
                    let content_h = self.browse_virtual_content_h();
                    let max_scroll = (content_h - self.browse_view_h).max(1.0);
                    self.browse_scroll_y =
                        (self.browse_scroll_y + dy).clamp(0.0, max_scroll);
                    self.browse_slice = self
                        .content_virtual_slice(self.browse_scroll_y, self.browse_view_h);
                    return iced::widget::operation::scroll_by(
                        iced::widget::Id::from(self.browse_scroll_widget_id()),
                        AbsoluteOffset { x: 0.0, y: dy },
                    );
                }
                self.browse_drag_last_y = Some(y);
            }
            Message::BrowseDragEnd => {
                self.browse_dragging = false;
                self.browse_drag_last_y = None;
                // Keep browse_drag_moved so the sibling mosaic on_release in this
                // frame is suppressed; open handlers *consume* the flag (one-shot)
                // so PlayVod / detail FABs are not stuck forever after a fling.
                let coast = self.browse_drag_last_dy * 10.0;
                self.browse_drag_last_dy = 0.0;
                if self.browse_drag_moved && coast.abs() > 8.0 {
                    self.pressed_mosaic = None;
                    self.browse_coast_vy = coast;
                    return self.apply_browse_coast_step();
                }
            }
            Message::BrowseCoastTick => {
                return self.apply_browse_coast_step();
            }
            Message::MosaicPress(id) => {
                self.pressed_mosaic = Some(id);
            }
            Message::CatScrolled(y, view_h) => {
                let view_h = view_h.max(1.0);
                let slice = browser::virtual_slice(
                    y,
                    view_h,
                    browser::cat_row_height(),
                    self.cat_entries.len(),
                    self.virtual_overscan(),
                );
                let key = (slice.start, slice.end);
                self.cat_scroll_y = y;
                if key == self.cat_slice && (view_h - self.cat_view_h).abs() < 8.0 {
                    return Task::none();
                }
                self.cat_view_h = view_h;
                self.cat_slice = key;
            }
            Message::SelectBrowseCategory(id) => {
                if self.cats_open {
                    self.swallow_tile_release = true;
                }
                self.cats_open = false;
                self.list_limit = self.browse_page();
                if id == SHELF_HISTORY || id == SHELF_FAVORITES {
                    if self.tab == Tab::Epg {
                        return Task::none();
                    }
                    self.shelf = if id == SHELF_HISTORY {
                        Shelf::History
                    } else {
                        Shelf::Favorites
                    };
                    self.series_detail = None;
                    self.vod_detail = None;
                    self.browse_scroll_y = 0.0;
                    self.rebuild_cat_entries();
                    return Task::batch([
                        self.prefetch_shelf_art(),
                        self.snap_browse_scroll_task(),
                    ]);
                }
                self.shelf = Shelf::Catalog;
                self.browse_scroll_y = 0.0;
                self.browse_scroll_vy = 0.0;
                let task = match self.tab {
                    Tab::Live | Tab::Epg => {
                        self.selected_group = if id == "*" {
                            None
                        } else {
                            Some(id)
                        };
                        Task::batch([
                            self.rebuild_browse_index(),
                            self.fetch_epg_for_visible_task(),
                        ])
                    }
                    Tab::Vod => {
                        self.selected_vod_category = Some(id.clone());
                        self.vod_detail = None;
                        self.detail_meta_loading = false;
                        let base = crate::names::category_shelf(&id).0.to_string();
                        let idx = self.rebuild_browse_index();
                        let follow = if id == "*"
                            || self
                                .bundle
                                .vod
                                .iter()
                                .any(|v| v.category_id.as_deref() == Some(base.as_str()))
                        {
                            self.refresh_browse_art()
                        } else {
                            self.load_vod_category_task(base)
                        };
                        Task::batch([idx, follow])
                    }
                    Tab::Series => {
                        self.selected_series_category = Some(id.clone());
                        self.series_detail = None;
                        self.detail_meta_loading = false;
                        let base = crate::names::category_shelf(&id).0.to_string();
                        let idx = self.rebuild_browse_index();
                        let follow = if id == "*"
                            || self
                                .bundle
                                .series
                                .iter()
                                .any(|s| s.category_id.as_deref() == Some(base.as_str()))
                        {
                            self.refresh_browse_art()
                        } else {
                            self.load_series_category_task(base)
                        };
                        Task::batch([idx, follow])
                    }
                    _ => self.rebuild_browse_index(),
                };
                return Task::batch([
                    task,
                    self.refresh_browse_art(),
                    self.snap_browse_scroll_task(),
                ]);
            }
            Message::PlayChannelId(id, source_id) => {
                if self.consume_browse_drag_suppress() {
                    return Task::none();
                }
                let found = self
                    .bundle
                    .channels
                    .iter()
                    .find(|c| c.id == id && c.source_id == source_id)
                    .or_else(|| self.bundle.channels.iter().find(|c| c.id == id))
                    .cloned();
                let Some(ch) = found else {
                    self.status = "Chaîne introuvable".into();
                    return Task::none();
                };
                return Task::done(Message::PlayChannel(ch));
            }
            Message::DownloadMedia { name, url } => return self.request_download(name, url),
            Message::DownloadSeason(season) => {
                let Some(detail) = &self.series_detail else {
                    return Task::none();
                };
                let wanted: Vec<(String, String)> = detail
                    .seasons
                    .iter()
                    .filter(|s| season.is_none_or(|n| s.season_number == n))
                    .flat_map(|s| {
                        s.episodes.iter().map(move |ep| {
                            (
                                crate::downloads::episode_title(
                                    &detail.name,
                                    s.season_number,
                                    ep.episode_num,
                                    &ep.title,
                                ),
                                ep.stream_url.clone(),
                            )
                        })
                    })
                    .filter(|(_, url)| {
                        !url.trim().is_empty()
                            && !self.download_library.contains(url)
                            && !matches!(
                                self.downloads.get(url),
                                Some(
                                    crate::downloads::DownloadState::Running { .. }
                                        | crate::downloads::DownloadState::Queued { .. }
                                        | crate::downloads::DownloadState::Done { .. }
                                )
                            )
                    })
                    .collect();
                let n = wanted.len();
                let tasks: Vec<_> = wanted
                    .into_iter()
                    .map(|(name, url)| self.request_download(name, url))
                    .collect();
                let what = match season {
                    Some(s) => format!("Saison {s}"),
                    None => "Série".to_string(),
                };
                self.status = match n {
                    0 => format!("{what} : rien de plus à télécharger"),
                    1 => format!("{what} : 1 épisode en téléchargement"),
                    _ => format!(
                        "{what} : {n} épisodes — {} à la fois, les autres en file d'attente",
                        crate::downloads::MAX_PARALLEL
                    ),
                };
                return Task::batch(tasks);
            }
            Message::CancelSeasonDownloads(season) => {
                let Some(detail) = &self.series_detail else {
                    return Task::none();
                };
                let urls: Vec<String> = detail
                    .seasons
                    .iter()
                    .filter(|s| season.is_none_or(|n| s.season_number == n))
                    .flat_map(|s| s.episodes.iter().map(|ep| ep.stream_url.clone()))
                    .collect();
                let n = urls.iter().filter(|u| self.cancel_download(u)).count();
                self.status = format!("{n} téléchargement(s) annulé(s)");
                return self.pump_download_queue();
            }
            Message::ToggleSeasonMenu => {
                self.season_menu_open = !self.season_menu_open;
            }
            Message::SelectSeason(n) => {
                self.selected_season = Some(n);
                self.season_menu_open = false;
                self.episode_hover = None;
                self.browse_scroll_y = 0.0;
                return Task::batch([self.snap_browse_scroll_task(), self.prefetch_episode_stills()]);
            }
            Message::EpisodeHover { key, on } => {
                if on {
                    self.episode_hover = Some(key);
                } else if self.episode_hover.as_deref() == Some(key.as_str()) {
                    self.episode_hover = None;
                }
            }
            Message::CastPhoto { name, url } => {
                let key = credit_key(&name);
                if let Some(url) = url {
                    self.cast_photos.insert(key, url.clone());
                    return self.prefetch_urls(std::iter::once(url));
                }
            }
            Message::DownloadEvent { url, event } => {
                use crate::downloads::{DownloadEvent, DownloadState};
                let Some(DownloadState::Running {
                    name,
                    done,
                    total,
                    part,
                    ..
                }) = self.downloads.get_mut(&url)
                else {
                    // Cancelled: late events from the aborted stream are ignored.
                    return Task::none();
                };
                match event {
                    DownloadEvent::Started { part: p } => {
                        if let Some(dir) = p.parent() {
                            self.status = format!(
                                "Téléchargement — {name} → {}",
                                crate::storage::display_path(dir)
                            );
                        }
                        *part = Some(p);
                    }
                    DownloadEvent::Progress {
                        done: d,
                        total: t,
                        rate,
                        connections,
                    } => {
                        *done = d;
                        *total = t;
                        let links = match connections {
                            0 if rate == 0 => " · en attente d'une connexion libre".to_string(),
                            0 | 1 => String::new(),
                            n => format!(" · {n} connexions"),
                        };
                        self.status = format!(
                            "Téléchargement — {name} · {}{links}",
                            crate::downloads::progress_detail(d, t, rate)
                        );
                    }
                    DownloadEvent::Waiting => {
                        self.status = format!(
                            "Téléchargement — {name} · en attente : le compte télécharge déjà autant de titres que le panel l'autorise"
                        );
                    }
                    DownloadEvent::Retrying {
                        attempt,
                        wait,
                        error,
                    } => {
                        self.status = format!(
                            "Téléchargement — {name} · connexion perdue ({error}), reprise dans {} s (essai {attempt})",
                            wait.as_secs().max(1)
                        );
                    }
                    DownloadEvent::Finished(Ok(path)) => {
                        self.status = format!("Téléchargé — {}", path.display());
                        crate::downloads::forget_partial(&mut self.download_partials, &url);
                        self.download_library.insert(&url, path.clone());
                        self.downloads.insert(url, DownloadState::Done { path });
                        return self.pump_download_queue();
                    }
                    DownloadEvent::Finished(Err(error)) => {
                        self.status = format!("Téléchargement échoué — {name} : {error}");
                        self.download_partials =
                            crate::downloads::scan_partials(&self.downloads_dir());
                        self.downloads.insert(url, DownloadState::Failed);
                        return self.pump_download_queue();
                    }
                }
            }
            Message::CancelDownload(url) => {
                if self.cancel_download(&url) {
                    return self.pump_download_queue();
                }
            }
            Message::RevealDownload(path) => {
                let dir = path
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|| self.downloads_dir());
                #[cfg(not(target_os = "android"))]
                if let Err(e) = open::that(&dir) {
                    self.status = format!("Impossible d'ouvrir {} : {e}", dir.display());
                    return Task::none();
                }
                self.status = format!("Téléchargé — {}", path.display());
            }
            Message::FormDownloadDir(s) => self.form_download_dir = s,
            Message::SaveDownloadDir => {
                match crate::storage::validate_downloads_dir(&self.form_download_dir) {
                    Ok(dir) => {
                        self.settings.download_dir = dir;
                        self.form_download_dir = self.settings.download_dir.clone();
                        self.download_partials =
                            crate::downloads::scan_partials(&self.downloads_dir());
                        self.persist();
                        self.status = format!(
                            "Téléchargements → {}",
                            crate::storage::display_path(&self.downloads_dir())
                        );
                    }
                    Err(e) => self.status = e,
                }
            }
            Message::PickDownloadDir => {
                #[cfg(not(target_os = "android"))]
                {
                    let start = self.downloads_dir();
                    return Task::perform(
                        async move {
                            rfd::AsyncFileDialog::new()
                                .set_title("Dossier des téléchargements")
                                .set_directory(start)
                                .pick_folder()
                                .await
                                .map(|f| f.path().to_path_buf())
                        },
                        Message::DownloadDirPicked,
                    );
                }
            }
            Message::DownloadDirPicked(path) => {
                if let Some(path) = path {
                    self.form_download_dir = path.to_string_lossy().into_owned();
                    return self.update(Message::SaveDownloadDir);
                }
            }
            Message::ResetDownloadDir => {
                self.form_download_dir.clear();
                return self.update(Message::SaveDownloadDir);
            }
            Message::OpenDownloadsDir => {
                let dir = self.downloads_dir();
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    self.status = format!("Impossible de créer {} : {e}", dir.display());
                    return Task::none();
                }
                #[cfg(not(target_os = "android"))]
                if let Err(e) = open::that(&dir) {
                    self.status = format!("Impossible d'ouvrir {} : {e}", dir.display());
                    return Task::none();
                }
                self.status = format!("Téléchargements : {}", dir.display());
            }
            Message::PlayChannel(ch) => {
                if self.consume_browse_drag_suppress() {
                    return Task::none();
                }
                self.store_watch_progress(true);
                // History and resume play a series episode as a channel, with no
                // episode list. Next / previous need that list.
                let mut queue_task = if ch.kind == ContentKind::Series {
                    self.arm_series_queue(&ch.stream_url, &ch.name)
                } else if ch.kind == ContentKind::Vod {
                    self.clear_series_queue();
                    None
                } else {
                    None
                };
                // Invalidate deferred Stop→close so a quick zap cannot kill this play.
                self.player_close_gen = self.player_close_gen.wrapping_add(1);
                tracing::info!(id = %ch.id, name = %ch.name, "play channel");
                self.selected_channel = Some(ch.id.clone());
                self.apply_source_headers_for(&ch);
                self.remember_play(&ch, &ch.stream_url);
                let epg_task = self.fetch_epg_for(vec![(ch.id.clone(), ch.source_id)]);
                let art = crate::images::pick_art(
                    ch.logo.as_deref().or(ch.tvg_logo.as_deref()),
                    None,
                    None,
                    None,
                );
                let art_task = art
                    .map(|u| self.prefetch_urls(std::iter::once(u)))
                    .unwrap_or_else(Task::none);

                // Android External: Intent before NativePlayer (player crate has no JNI).
                #[cfg(target_os = "android")]
                {
                    let prefer_external = matches!(
                        play_options_from(&self.settings).preferred,
                        PlayerBackendPref::External
                    );
                    if prefer_external {
                        match crate::android_intent::open_stream_url(&ch.stream_url) {
                            Ok(()) => {
                                self.session.stop();
                                self.invalidate_soft_stage(true);
                                crate::android_bridge::release_video_surface_wid();
                                self.session.channel = Some(ch.clone());
                                self.session.backend = Some(BackendId::External);
                                self.session.state = PlaybackState::Playing;
                                self.status = format!("Lecteur système · {}", ch.name);
                                self.persist();
                                return take_queue_task(
                                    vec![epg_task, art_task, self.open_or_focus_player()],
                                    &mut queue_task,
                                );
                            }
                            Err(e) => {
                                self.status = format!("Intent: {e}");
                                self.persist();
                                return take_queue_task(
                                    vec![epg_task, art_task, self.open_or_focus_player()],
                                    &mut queue_task,
                                );
                            }
                        }
                    }
                }

                let res = {
                    self.invalidate_soft_stage(false);
                    #[cfg(target_os = "android")]
                    {
                        // Grab focus before libmpv opens OpenSLES — otherwise a stale
                        // held=false flag / late request leaves AO started→stopped mute.
                        crate::android_bridge::request_audio_focus();
                        if crate::android_bridge::poll_audio_focus_held() == Some(true) {
                            self.audio_focus_held = true;
                        }
                        self.pause_cause = PauseCause::None;
                    }
                    self.open_with_failover(ch.clone())
                }; match res {
                    Ok(()) => {
                        self.arm_resume(&ch.stream_url, ch.kind);
                        self.apply_saved_video_defaults_for_current_play();
                        self.status = self.session.status_line();
                        self.persist();
                        #[cfg(target_os = "android")]
                        {
                            // Prefer in-process libmpv RGBA embed (desktop parity).
                            // Only fall back to ACTION_VIEW when embed is unavailable.
                            if !self.session.has_embedded_video() {
                                match crate::android_intent::open_stream_url(&ch.stream_url) {
                                    Ok(()) => {
                                        self.session.stop();
                                        self.invalidate_soft_stage(true);
                                        crate::android_bridge::release_video_surface_wid();
                                        self.session.channel = Some(ch.clone());
                                        self.session.backend = Some(BackendId::External);
                                        self.session.state = PlaybackState::Playing;
                                        self.status = format!("Lecteur système · {}", ch.name);
                                    }
                                    Err(e) => {
                                        self.status = format!("Intent: {e}");
                                    }
                                }
                            }
                        }
                        return take_queue_task(
                            vec![epg_task, art_task, self.open_or_focus_player()],
                            &mut queue_task,
                        );
                    }
                    Err(e) => {
                        #[cfg(target_os = "android")]
                        {
                            let needs_intent = e.to_string().contains("EXTERNAL_NEEDS_INTENT");
                            if needs_intent || !self.session.has_embedded_video() {
                                match crate::android_intent::open_stream_url(&ch.stream_url) {
                                    Ok(()) => {
                                        self.session.stop();
                                        self.invalidate_soft_stage(true);
                                        crate::android_bridge::release_video_surface_wid();
                                        self.session.channel = Some(ch.clone());
                                        self.session.backend = Some(BackendId::External);
                                        self.session.state = PlaybackState::Playing;
                                        self.status =
                                            format!("Lecteur système · {}", ch.name);
                                    }
                                    Err(ie) => {
                                        self.status = format!("{e} / Intent: {ie}");
                                    }
                                }
                            } else {
                                self.status = format!("{e}");
                            }
                            self.persist();
                            return take_queue_task(
                                vec![epg_task, art_task, self.open_or_focus_player()],
                                &mut queue_task,
                            );
                        }
                        #[cfg(not(target_os = "android"))]
                        {
                            self.status = format!("{e} — essayez Externe ou installez mpv");
                            self.persist();
                            return take_queue_task(
                                vec![epg_task, art_task, self.open_or_focus_player()],
                                &mut queue_task,
                            );
                        }
                    }
                }
            }
            Message::PlayVod {
                name,
                url,
                kind,
                poster,
            } => {
                // Detail FAB / episode rows must always work; mosaic fling only
                // suppresses the accidental tile release that follows a drag.
                if self.series_detail.is_none()
                    && self.vod_detail.is_none()
                    && self.consume_browse_drag_suppress()
                {
                    return Task::none();
                }
                self.browse_drag_moved = false;
                self.status = format!("Ouverture — {name}…");
                // Invalidate deferred Stop→close so a quick play cannot be killed.
                self.player_close_gen = self.player_close_gen.wrapping_add(1);
                if url.trim().is_empty() {
                    self.status = format!("URL vide — {name}");
                    return Task::none();
                }
                self.store_watch_progress(true);
                let art_url = poster.clone();
                if kind == ContentKind::Series {
                    self.follow_playing_season(&url);
                }
                let mut queue_task = if kind == ContentKind::Series {
                    self.arm_series_queue(&url, &name)
                } else {
                    self.clear_series_queue();
                    None
                };
                self.prefetch_armed_for = None;
                let stream_url = url.clone();
                // A downloaded copy plays from disk: no stream, no panel slot.
                let local = self
                    .downloaded_file(&url, &name, kind)
                    .and_then(|file| {
                        let play = crate::downloads::local_play_url(&file)?;
                        Some(play)
                    });
                let url = match local {
                    Some(play) => {
                        if let Some(root) = self.downloads_dir().to_str() {
                            std::env::set_var("FLUXPLAY_DOWNLOAD_ROOT", root);
                        }
                        tracing::info!(name = %name, "playing downloaded file");
                        self.status = format!("Fichier local — {name}");
                        play
                    }
                    None => url,
                };
                let ch = Channel {
                    id: format!("vod-{}", Uuid::new_v4()),
                    name: name.clone(),
                    stream_url: url,
                    logo: poster.clone(),
                    group: Some(match kind {
                        ContentKind::Series => "Séries".into(),
                        _ => "VOD".into(),
                    }),
                    tvg_id: None,
                    tvg_name: None,
                    tvg_logo: poster.clone(),
                    epg_channel_id: None,
                    scheme: None,
                    source_id: self.source_for_url(&stream_url).map(|s| s.id),
                    kind,
                    catchup: None,
                };
                self.apply_source_headers_for(&ch);
                #[cfg(target_os = "android")]
                {
                    let prefer_external = matches!(
                        play_options_from(&self.settings).preferred,
                        PlayerBackendPref::External
                    );
                    if prefer_external {
                        match crate::android_intent::open_stream_url(&stream_url) {
                            Ok(()) => {
                                self.session.stop();
                                self.invalidate_soft_stage(true);
                                crate::android_bridge::release_video_surface_wid();
                                self.session.channel = Some(ch);
                                self.session.backend = Some(BackendId::External);
                                self.session.state = PlaybackState::Playing;
                                self.status = format!("Lecteur système · {name}");
                                return take_queue_task(
                                    vec![self.open_or_focus_player()],
                                    &mut queue_task,
                                );
                            }
                            Err(e) => {
                                self.status = format!("Intent: {e}");
                                return take_queue_task(
                                    vec![self.open_or_focus_player()],
                                    &mut queue_task,
                                );
                            }
                        }
                    }
                }
                self.remember_play(&ch, &stream_url);
                let res = {
                    self.invalidate_soft_stage(false);
                    #[cfg(target_os = "android")]
                    {
                        crate::android_bridge::request_audio_focus();
                        if crate::android_bridge::poll_audio_focus_held() == Some(true) {
                            self.audio_focus_held = true;
                        }
                        self.pause_cause = PauseCause::None;
                    }
                    self.open_with_failover(ch)
                }; match res {
                    Ok(()) => {
                        self.arm_resume(&stream_url, kind);
                        self.apply_saved_video_defaults_for_current_play();
                        self.status = self.session.status_line();
                        self.persist();
                        let art = crate::images::pick_art(
                            art_url.as_deref(),
                            art_url.as_deref(),
                            None,
                            None,
                        );
                        let art_task = art
                            .map(|u| self.prefetch_urls(std::iter::once(u)))
                            .unwrap_or_else(Task::none);
                        return take_queue_task(
                            vec![
                                art_task,
                                self.open_or_focus_player(),
                                self.park_browser_for_library(),
                            ],
                            &mut queue_task,
                        );
                    }
                    Err(e) => {
                        #[cfg(target_os = "android")]
                        {
                            match crate::android_intent::open_stream_url(&stream_url) {
                                Ok(()) => {
                                    self.session.stop();
                                    self.invalidate_soft_stage(true);
                                    crate::android_bridge::release_video_surface_wid();
                                    self.session.backend = Some(BackendId::External);
                                    self.session.state = PlaybackState::Playing;
                                    self.status = format!("Lecteur système · {name}");
                                }
                                Err(ie) => {
                                    self.status = format!("{e} / Intent: {ie}");
                                }
                            }
                            return take_queue_task(
                                vec![self.open_or_focus_player()],
                                &mut queue_task,
                            );
                        }
                        #[cfg(not(target_os = "android"))]
                        {
                            let _ = stream_url;
                            self.status = format!(
                                "{e} — 1 connexion max: Stop puis réessayez, ou Externe"
                            );
                            return take_queue_task(
                                vec![self.open_or_focus_player()],
                                &mut queue_task,
                            );
                        }
                    }
                }
            }
            #[cfg(target_os = "android")]
            Message::SafPoll => {
                self.refresh_system_dark();
                crate::android_bridge::refresh_system_insets();
                let prev_insets = self.system_insets;
                self.system_insets = crate::android_bridge::system_insets_dp();
                if self.system_insets != prev_insets {
                    // Immersive / fold / keyboard: size may be unchanged while safe area moves.
                    self.layout_cache = None;
                    self.refresh_layout_cache();
                }
                let prev_pip = self.pip_mode;
                self.pip_mode = crate::android_bridge::poll_pip_mode();
                if self.pip_mode != prev_pip {
                    if let Some(id) = self.player_id.or(self.main_id) {
                        return Task::done(Message::PlayerLayoutDirty(id));
                    }
                }
                let Some(inbox) = crate::android_bridge::poll_saf_inbox() else {
                    return Task::none();
                };
                let kind = self.saf_kind.take();
                if inbox.status == "cancel" {
                    self.status = "Annulé".into();
                    return Task::none();
                }
                if inbox.status == "error" {
                    self.status = format!("Fichier: {}", inbox.name);
                    return Task::none();
                }
                let path = if inbox.path.is_empty() {
                    crate::android_bridge::default_picked_path()
                } else {
                    Some(std::path::PathBuf::from(&inbox.path))
                };
                return Task::done(Message::SafResult {
                    kind: kind.unwrap_or(SafKind::Playlist),
                    path,
                    name: inbox.name,
                    err: None,
                });
            }
            #[cfg(target_os = "android")]
            Message::SafResult {
                kind,
                path,
                name,
                err,
            } => {
                if let Some(e) = err {
                    self.status = e;
                    return Task::none();
                }
                let Some(path) = path else {
                    self.status = "Fichier introuvable".into();
                    return Task::none();
                };
                match kind {
                    SafKind::WireGuard => {
                        return Task::done(Message::WireGuardProfilePicked(Some(path)));
                    }
                    SafKind::Playlist => {
                        self.form_kind = SourceKind::M3uPlus;
                        self.form_endpoint = path.display().to_string();
                        if self.form_name.is_empty() {
                            self.form_name = if name.is_empty() {
                                "Playlist locale".into()
                            } else {
                                name
                            };
                        }
                        self.status = "Playlist sélectionnée".into();
                    }
                    SafKind::ProfileImport => {
                        return Task::perform(
                            async move {
                                tokio::task::spawn_blocking(move || {
                                    crate::profile_io::import_profile(&path)
                                })
                                .await
                                .map_err(|e| e.to_string())?
                            },
                            Message::ProfileImportDone,
                        );
                    }
                    SafKind::ProfileExport => {
                        self.status = format!("Export enregistré ({name})");
                    }
                }
            }
            #[cfg(target_os = "android")]
            Message::NavBack => {
                if self.player_embedded {
                    return Task::done(Message::PlayerHotkey(PlayerHotkey::Escape));
                }
                // Same paths as the on-screen buttons: they also rebuild the browse index
                // and the sidebar, which a bare field reset left pointing at the old tab.
                if self.series_detail.is_some() {
                    return self.update(Message::CloseSeriesDetail);
                }
                if self.vod_detail.is_some() {
                    return self.update(Message::CloseVodDetail);
                }
                // Nested tabs → Live first; only finish Activity from Live root.
                if !matches!(self.tab, Tab::Live) {
                    return self.update(Message::Tab(Tab::Live));
                }
                crate::android_bridge::finish_activity();
                return Task::none();
            }
            #[cfg(target_os = "android")]
            Message::BrowseFocusDelta(delta) => {
                let len = self.browse_index.len();
                if len == 0 {
                    return Task::none();
                }
                let next = if delta < 0 {
                    self.browse_focus.saturating_sub((-delta) as usize)
                } else {
                    (self.browse_focus + delta as usize).min(len.saturating_sub(1))
                };
                self.browse_focus = next;
                if matches!(self.tab, Tab::Live | Tab::Favorites) {
                    if let Some(bi) = self.browse_index.get(next) {
                        if let Some(ch) = self.bundle.channels.get(bi) {
                            self.selected_channel = Some(ch.id.clone());
                        }
                    }
                }
                return Task::none();
            }
            #[cfg(target_os = "android")]
            Message::BrowseActivate => {
                if !matches!(self.tab, Tab::Live | Tab::Favorites) {
                    return Task::none();
                }
                let Some(bi) = self.browse_index.get(self.browse_focus) else {
                    return Task::none();
                };
                let Some(ch) = self.bundle.channels.get(bi) else {
                    return Task::none();
                };
                return Task::done(Message::PlayChannelId(ch.id.clone(), ch.source_id));
            }
            #[cfg(target_os = "android")]
            Message::CatalogBootReady {
                counts: (ch_n, vod_n, ser_n),
                sync_fresh,
            } => {
                if let Ok(mut slot) = PENDING_CATALOG_BOOT.lock() {
                    self.catalog_db = slot.take();
                }
                let has_real = self.sources.iter().any(|s| !demo::is_demo(s));
                self.status = if has_real && sync_fresh {
                    format!("Chargement cache · {ch_n} live · {vod_n} VOD · {ser_n} séries…")
                } else if has_real && (ch_n + vod_n + ser_n) > 0 {
                    format!("DB locale · {ch_n} live · {vod_n} VOD · {ser_n} séries — sync…")
                } else if has_real {
                    "Synchronisation catalogue (live / VOD / séries)…".into()
                } else {
                    self.status.clone()
                };
                return self.rebuild_bundle_from_cache_task();
            }
            Message::Stop => {
                self.store_watch_progress(true);
                self.session.stop();
                self.invalidate_soft_stage(true);
                #[cfg(target_os = "android")]
                {
                    self.android_force_soft = false;
                    self.android_want_surface_upgrade = false;
                    crate::android_bridge::release_video_surface_wid();
                    crate::android_bridge::set_hdr_color_mode(false);
                }
                self.status = "Arrêté".into();
                // Defer window close so libmpv teardown finishes cleanly.
                self.player_close_gen = self.player_close_gen.wrapping_add(1);
                let gen = self.player_close_gen;
                return Task::perform(
                    async {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    },
                    move |_| Message::ClosePlayerWindowDeferred(gen),
                );
            }
            Message::TogglePause => {
                match self.session.state {
                    PlaybackState::Playing => {
                        self.session.pause();
                        #[cfg(target_os = "android")]
                        {
                            self.pause_cause = PauseCause::User;
                        }
                    }
                    PlaybackState::Paused => {
                        #[cfg(target_os = "android")]
                        {
                            if !self.audio_focus_held {
                                crate::android_bridge::request_audio_focus();
                                if crate::android_bridge::poll_audio_focus_held() == Some(true) {
                                    self.audio_focus_held = true;
                                }
                            }
                            self.pause_cause = PauseCause::None;
                        }
                        self.session.resume();
                    }
                    _ => {}
                }
                self.status = self.session.status_line();
                if self.party.broadcasting {
                    let paused = self.session.state == PlaybackState::Paused;
                    self.party.order(crate::party::Order::Pause(paused));
                }
            }
            Message::ToggleMute => {
                self.session.toggle_mute();
                self.status = if self.session.muted {
                    "Muet".into()
                } else {
                    self.session.status_line()
                };
            }
            Message::VolumeChanged(v) => {
                self.session.set_volume(v);
                self.settings.volume = self.session.volume;
            }
            Message::VolumeReleased => self.persist(),
            Message::SeekRel(secs) => {
                self.session.seek_relative(secs as f64);
                self.status = format!("Seek {secs:+}s · {}", self.session.elapsed_label());
            }
            Message::SeekPercent(pct) => self.seek_drag = Some(pct),
            Message::SeekReleased => {
                if let Some(pct) = self.seek_drag.take() {
                    self.session.seek_percent(pct * 100.0);
                    self.status = format!("Position {}", self.session.elapsed_label());
                }
            }
            Message::RestartStream => {
                self.session.restart();
                self.status = self.session.status_line();
            }
            Message::ToggleFullscreen => {
                self.stage_click_gen = self.stage_click_gen.wrapping_add(1);
                #[cfg(target_os = "android")]
                {
                    // Shared NativeActivity surface — never Mode::Fullscreen (Waydroid h=0).
                    self.player_fullscreen = !self.player_fullscreen;
                    self.player_chrome_visible = !self.player_fullscreen;
                    // Freeze soft size while immersive bounds settle (same class as desktop FS).
                    self.player_layout_freeze_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                    crate::android_bridge::set_immersive_mode(self.player_fullscreen);
                    self.status = if self.player_fullscreen {
                        "Plein écran (chrome masqué)".into()
                    } else {
                        "Fenêtre".into()
                    };
                    // Catch up soft video_rect after freeze (WindowResized is dropped mid-freeze).
                    if let Some(id) = self.player_id.or(self.main_id) {
                        return Task::perform(
                            async {
                                tokio::time::sleep(std::time::Duration::from_millis(650)).await;
                            },
                            move |_| Message::PlayerLayoutDirty(id),
                        );
                    }
                    return Task::none();
                }
                #[cfg(not(target_os = "android"))]
                {
                // Never fullscreen the browse window — only the player surface.
                let Some(id) = self.player_id else {
                    self.status = "Ouvrez d’abord le lecteur".into();
                    return Task::none();
                };
                self.player_chrome_visible = true;
                self.player_pointer_at = Some(std::time::Instant::now());
                // Freeze soft-stage while the compositor animates mode change (anti-flicker).
                self.player_layout_freeze_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(900));
                if self.player_fullscreen {
                    self.player_fullscreen = false;
                    self.player_fs_gen = self.player_fs_gen.wrapping_add(1);
                    self.status = "Fenêtre".into();
                    return Task::batch([
                        window::set_mode(id, window::Mode::Windowed),
                        Task::perform(
                            async {
                                tokio::time::sleep(std::time::Duration::from_millis(650)).await;
                            },
                            move |_| Message::PlayerLayoutDirty(id),
                        ),
                    ]);
                }
                self.player_fullscreen = true;
                self.player_fs_gen = self.player_fs_gen.wrapping_add(1);
                self.status = "Plein écran".into();
                // A maximized window on another monitor ignores the size change and
                // only drops its frame. Restore first, then go borderless.
                return self.commit_player_fullscreen(id, self.player_fs_gen);
                }
            }
            Message::WakeProbe => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let prev = self.wake_unix;
                self.wake_unix = now;
                if prev > 0 && now.saturating_sub(prev) >= 8 {
                    return self.recover_after_sleep();
                }
            }
            Message::WakeAudio => {
                if self.session.channel.is_none() {
                    self.wake_pos = None;
                    return Task::none();
                }
                let paused = matches!(self.session.state, PlaybackState::Paused);
                self.session.wake_audio();
                let stuck = self.wake_pos.take().is_some_and(|pos| {
                    !paused
                        && !self.session.is_live()
                        && (self.session.position_secs - pos).abs() < 0.25
                });
                if stuck {
                    tracing::info!("playback still frozen after wake, reloading the file");
                    self.session.restart();
                    self.status = self.session.status_line();
                }
            }
            Message::PlayerStageClick => {
                if self.player_exit_ask {
                    return Task::none();
                }
                self.player_chrome_visible = true;
                self.player_pointer_at = Some(std::time::Instant::now());
                self.stage_click_gen = self.stage_click_gen.wrapping_add(1);
                let gen = self.stage_click_gen;
                return Task::perform(
                    async {
                        tokio::time::sleep(std::time::Duration::from_millis(240)).await;
                    },
                    move |_| Message::PlayerStageClickFire(gen),
                );
            }
            Message::PlayerStageClickFire(gen) => {
                if gen != self.stage_click_gen {
                    return Task::none();
                }
                return self.update(Message::TogglePause);
            }
            Message::PlayerPointerActivity => {
                let now = std::time::Instant::now();
                // Just after the bar hides, ignore the move the overlay itself caused.
                if !self.player_chrome_visible {
                    if let Some(at) = self.player_pointer_at {
                        if now.duration_since(at) < std::time::Duration::from_millis(200) {
                            return Task::none();
                        }
                    }
                } else if let Some(at) = self.player_pointer_at {
                    if now.duration_since(at) < std::time::Duration::from_millis(200) {
                        // Still moving: keep the idle clock current without a redraw storm.
                        self.player_pointer_at = Some(now);
                        return Task::none();
                    }
                }
                self.player_chrome_visible = true;
                self.player_pointer_at = Some(now);
                #[cfg(target_os = "android")]
                if self.session.native.android_surface_present() && !self.pip_mode {
                    let chrome = self.layout_metrics().player_chrome_h
                        + crate::theme::TOOLBAR_OUTER_PAD
                        + self.system_insets.3;
                    crate::android_bridge::layout_video_surface_chrome_inset_dp(chrome);
                }
            }
            Message::PlayerChromeTick => {
                self.session.refresh_track_cache();
                self.maybe_autohide_player_chrome();
                self.lerp_chrome_alpha();
                // Off the soft-video path — theme/caps at GUI rate only.
                self.refresh_system_dark();
                self.refresh_display_caps(false);
                if let Some(prefetch) = self.maybe_prefetch_next_episode() {
                    return prefetch;
                }
            }
            Message::CycleAudio => {
                self.status = if self.session.cycle_audio() {
                    "Piste audio suivante".into()
                } else {
                    "Piste audio non supportée".into()
                };
            }
            Message::SelectAudio(id) => {
                self.status = if self.session.set_audio_track(id) {
                    "Piste audio".into()
                } else {
                    "Piste audio non supportée".into()
                };
            }
            Message::CycleSubtitles => {
                self.status = if self.session.cycle_subtitles() {
                    "Sous-titres suivants".into()
                } else {
                    "Sous-titres non supportés".into()
                };
            }
            Message::SelectSubtitle(id) => {
                self.status = if self.session.set_subtitle_track(id) {
                    if id.is_none() {
                        "Sous-titres désactivés".into()
                    } else {
                        "Sous-titres".into()
                    }
                } else {
                    "Sous-titres non supportés".into()
                };
            }
            Message::SkipGeneric => {
                self.status = if self.session.skip_generic() {
                    "Générique passé".into()
                } else {
                    "Pas de générique à passer".into()
                };
            }
            Message::SkipIntro => {
                self.status = if self.session.skip_generic() {
                    "Générique passé".into()
                } else {
                    "Pas de générique à passer".into()
                };
            }
            Message::PlayerFullscreenCommit(id, gen) => {
                if gen != self.player_fs_gen
                    || self.player_id != Some(id)
                    || !self.player_fullscreen
                {
                    return Task::none();
                }
                self.player_layout_freeze_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(700));
                return Task::batch([
                    window::set_mode(id, window::Mode::Fullscreen),
                    Task::perform(
                        async {
                            tokio::time::sleep(std::time::Duration::from_millis(650)).await;
                        },
                        move |_| Message::PlayerLayoutDirty(id),
                    ),
                ]);
            }
            Message::PlayerResizeSettled { id, gen } => {
                if gen != self.player_resize_gen || self.player_id != Some(id) {
                    return Task::none();
                }
                return self.sync_player_layout_task(id);
            }
            Message::PlayerScreenChanged(id) => {
                // A plain move does not change the stage. Re-running layout on
                // every drag step stopped presents, so the picture stayed put.
                let settling = self
                    .player_layout_freeze_until
                    .is_some_and(|t| std::time::Instant::now() < t);
                if self.player_id == Some(id) && self.player_fullscreen && !settling {
                    self.player_fs_gen = self.player_fs_gen.wrapping_add(1);
                    let gen = self.player_fs_gen;
                    self.player_layout_freeze_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(900));
                    return self
                        .sync_player_layout_task(id)
                        .chain(self.commit_player_fullscreen(id, gen));
                }
                return Task::none();
            }
            Message::ToggleSkipCredits => {
                self.settings.skip_credits_auto = !self.settings.skip_credits_auto;
                self.persist();
                self.status = if self.settings.skip_credits_auto {
                    "Le générique est passé automatiquement".into()
                } else {
                    "Le générique se passe à la main".into()
                };
            }
            Message::SetHideAgeUnder(age) => {
                self.settings.hide_age_under = age;
                self.persist();
                self.status = "Filtre d'âge mis à jour".into();
                if matches!(self.tab, Tab::Live | Tab::Epg | Tab::Vod | Tab::Series) {
                    return self.rebuild_browse_index();
                }
            }
            Message::SetHideAgeOver(age) => {
                self.settings.hide_age_over = age;
                self.persist();
                self.status = "Filtre d'âge mis à jour".into();
                if matches!(self.tab, Tab::Live | Tab::Epg | Tab::Vod | Tab::Series) {
                    return self.rebuild_browse_index();
                }
            }
            Message::AgesLoaded(rows) => {
                self.age_lookup_busy = false;
                let mut changed = false;
                for (id, source_id, rated) in rows {
                    let Some(code) = rated.filter(|s| !s.trim().is_empty()) else {
                        continue;
                    };
                    let Some(item) = self.bundle.series.iter_mut().find(|s| s.id == id) else {
                        continue;
                    };
                    if crate::metadata::age_rating(item.rated.as_deref()).is_some() {
                        continue;
                    }
                    item.rated = Some(code);
                    changed = true;
                    if let (Some(db), Some(sid)) = (&self.catalog_db, source_id.or(item.source_id))
                    {
                        let mut saved = item.clone();
                        saved.source_id = Some(sid);
                        let _ = db.persist_series(&saved);
                    }
                }
                if let Some(detail) = &self.series_detail {
                    if let Some(updated) = self.bundle.series.iter().find(|s| s.id == detail.id) {
                        let mut d = updated.clone();
                        if d.seasons.is_empty() {
                            d.seasons = detail.seasons.clone();
                        }
                        self.series_detail = Some(d);
                    }
                }
                let more = self.queue_visible_ages();
                if changed && self.age_filter_on() {
                    return Task::batch([self.rebuild_browse_index(), more]);
                }
                return more;
            }
            Message::CaptionLoaded(rows) => {
                self.age_lookup_busy = false;
                let mut changed = false;
                for row in rows {
                    changed |= self.apply_poster_caption(&row);
                }
                let more = self.queue_visible_ages();
                if changed && self.age_filter_on() {
                    return Task::batch([self.rebuild_browse_index(), more]);
                }
                return more;
            }
            Message::CatalogFreshnessCheck => {
                if self.loading || std::time::Instant::now() < self.catalog_sync_not_before {
                    return Task::none();
                }
                let playing = matches!(
                    self.session.state,
                    PlaybackState::Opening
                        | PlaybackState::Playing
                        | PlaybackState::Buffering
                        | PlaybackState::Paused
                );
                if playing
                    || !fluxplay_providers::api_cache::portal_quiet_remaining().is_zero()
                {
                    return Task::none();
                }
                let fresh = !self.catalog_sync_stale(crate::catalog_db::SYNC_FRESH_SECS);
                if fresh || self.sources.iter().all(|s| !s.enabled) {
                    return self.episode_alert_task();
                }
                self.alert_checked.clear();
                self.catalog_sync_not_before =
                    std::time::Instant::now() + std::time::Duration::from_secs(180);
                self.loading = true;
                self.status = "Mise à jour du catalogue…".into();
                return self.reload_all_task();
            }
            Message::PlayerTick => {
                // Deferred init: both are assigned unconditionally at the top of the
                // Android block below before any read.
                #[cfg(target_os = "android")]
                let mut allow_soft_present;
                #[cfg(target_os = "android")]
                let pip_layout_dirty;
                #[cfg(target_os = "android")]
                {
                    let fg = iced::android::is_foreground();
                    let in_pip = crate::android_bridge::poll_pip_mode();
                    pip_layout_dirty = in_pip != self.pip_mode;
                    self.pip_mode = in_pip;
                    // Never soft-render when backgrounded — mpv SW on UI thread → FocusEvent ANR.
                    allow_soft_present = (fg || in_pip) && !self.lifecycle_paused;
                    // Surface present owns pixels — skip iced RGBA pull entirely.
                    if self.session.native.android_surface_present() {
                        allow_soft_present = false;
                        // Healthy Surface: poll gen/size every ~1s; urgent if recovering.
                        self.android_maintain_tick =
                            self.android_maintain_tick.wrapping_add(1);
                        let urgent = self.android_surface_misses > 0;
                        if urgent || self.android_maintain_tick % 5 == 0 {
                            self.maintain_android_surface_session(false);
                        }
                    } else if self.android_want_surface_upgrade
                        && !self.android_force_soft
                        && !matches!(
                            self.settings.android_present,
                            AndroidPresentPref::Soft
                        )
                        && matches!(
                            self.session.state,
                            PlaybackState::Playing | PlaybackState::Buffering
                        )
                    {
                        // Soft emergency: keep Surface warming under iced, promote when ready.
                        self.android_maintain_tick =
                            self.android_maintain_tick.wrapping_add(1);
                        if self.android_maintain_tick % 5 == 0 {
                            crate::android_bridge::set_video_surface_z_on_top(false);
                            crate::android_bridge::set_video_surface_visible(true);
                            if crate::android_bridge::is_video_surface_ready() {
                                if let Some(ch) = self.session.channel.clone() {
                                    tracing::info!(
                                        "android Surface ready — promoting Soft → mediacodec_embed"
                                    );
                                    self.android_want_surface_upgrade = false;
                                    self.player_close_gen =
                                        self.player_close_gen.wrapping_add(1);
                                    self.invalidate_soft_stage(false);
                                    self.apply_source_headers_for(&ch);
                                    crate::android_bridge::request_audio_focus();
                                    if crate::android_bridge::poll_audio_focus_held()
                                        == Some(true)
                                    {
                                        self.audio_focus_held = true;
                                    }
                                    match self.session.open_channel(ch) {
                                        Ok(()) => {
                                            self.apply_saved_video_defaults_for_current_play();
                                            self.status =
                                                "Surface HQ — MediaCodec".into();
                                        }
                                        Err(e) => {
                                            self.android_want_surface_upgrade = true;
                                            self.status =
                                                format!("Promotion Surface: {e}");
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(held) = crate::android_bridge::poll_audio_focus_held() {
                        // Only react to LOSS after we actually held focus. A stale
                        // audio_focus.json with held=false must NOT pause a fresh play
                        // (that killed OpenSL within milliseconds — silent video).
                        if !held
                            && self.audio_focus_held
                            && matches!(
                                self.session.state,
                                PlaybackState::Playing | PlaybackState::Buffering
                            )
                            && !matches!(self.session.backend, Some(BackendId::External))
                        {
                            self.session.pause();
                            self.pause_cause = PauseCause::AudioFocus;
                            self.status = "Pause — focus audio perdu".into();
                            self.audio_focus_held = false;
                        } else if held {
                            let rising = !self.audio_focus_held;
                            self.audio_focus_held = true;
                            // Delayed GAIN: OpenSL may have started muted — re-apply volume.
                            if rising
                                && matches!(
                                    self.session.state,
                                    PlaybackState::Playing
                                        | PlaybackState::Buffering
                                        | PlaybackState::Paused
                                )
                            {
                                if self.session.muted {
                                    self.session.toggle_mute();
                                }
                                self.session.set_volume(self.settings.volume);
                            }
                            // LOSS→GAIN: resume only if we paused for focus (not user).
                            if self.pause_cause == PauseCause::AudioFocus
                                && self.session.state == PlaybackState::Paused
                                && (fg || in_pip)
                            {
                                self.session.resume();
                                self.pause_cause = PauseCause::None;
                                self.status = self.session.status_line();
                            }
                        }
                    }
                    // Want sound while foreground with an owned session — request focus
                    // even when Paused(AudioFocus) so GAIN can unblock resume.
                    let want_focus = (fg || in_pip)
                        && !matches!(self.session.backend, Some(BackendId::External))
                        && (matches!(
                            self.session.state,
                            PlaybackState::Playing | PlaybackState::Buffering
                        ) || (self.session.state == PlaybackState::Paused
                            && self.pause_cause == PauseCause::AudioFocus));
                    if want_focus && !self.audio_focus_held {
                        crate::android_bridge::request_audio_focus();
                        if crate::android_bridge::poll_audio_focus_held() == Some(true) {
                            self.audio_focus_held = true;
                            if self.pause_cause == PauseCause::AudioFocus
                                && self.session.state == PlaybackState::Paused
                            {
                                self.session.resume();
                                self.pause_cause = PauseCause::None;
                                self.status = self.session.status_line();
                            }
                        }
                    }
                    if !fg && !self.lifecycle_paused && !in_pip {
                        if matches!(
                            self.session.state,
                            PlaybackState::Playing | PlaybackState::Buffering
                        ) {
                            self.session.pause();
                            self.pause_cause = PauseCause::Lifecycle;
                            self.lifecycle_paused = true;
                            allow_soft_present = false;
                            crate::android_bridge::set_keep_screen_on(false);
                            if self.audio_focus_held {
                                crate::android_bridge::abandon_audio_focus();
                                self.audio_focus_held = false;
                            }
                        }
                    } else if (fg || in_pip) && self.lifecycle_paused {
                        self.lifecycle_paused = false;
                        // wgpu surface was dropped on Suspend — drop iced GPU handle
                        // so next pull re-allocates (don't abandon audio focus).
                        self.video_upload_busy = false;
                        self.video_upload_gen = self.video_upload_gen.wrapping_add(1);
                        self.clear_video_pending();
                        self.video_frame = None;
                        self.video_allocation = None;
                        self.video_allocation_hold = None;
                        self.video_allocation_hold2 = None;
                        self.video_allocation_hold3 = None;
                        self.video_allocation_hold4 = None;
                        self.video_frame_wh = (0, 0);
                        crate::android_bridge::stabilize_android_session();
                        self.maintain_android_surface_session(true);
                        if self.player_fullscreen && !in_pip {
                            crate::android_bridge::set_immersive_mode(true);
                        }
                        if self.pause_cause == PauseCause::Lifecycle {
                            crate::android_bridge::request_audio_focus();
                            if crate::android_bridge::poll_audio_focus_held() == Some(true) {
                                self.audio_focus_held = true;
                                self.session.resume();
                                self.pause_cause = PauseCause::None;
                            } else {
                                // Wait for focus grant on a later tick.
                                self.pause_cause = PauseCause::AudioFocus;
                            }
                        }
                        allow_soft_present = true;
                    }
                    // Surface owns pixels — never soft-pull after lifecycle resume either.
                    if self.session.native.android_surface_present() {
                        allow_soft_present = false;
                    }
                    let keep = (fg || in_pip)
                        && !matches!(self.session.backend, Some(BackendId::External))
                        && matches!(
                            self.session.state,
                            PlaybackState::Playing | PlaybackState::Buffering
                        );
                    crate::android_bridge::set_keep_screen_on(keep);
                    if !keep
                        && self.audio_focus_held
                        && matches!(
                            self.session.state,
                            PlaybackState::Paused | PlaybackState::Idle | PlaybackState::Error
                        )
                        // Never abandon after LOSS — same tick would re-request and stick paused.
                        && !matches!(
                            self.pause_cause,
                            PauseCause::AudioFocus | PauseCause::Lifecycle
                        )
                    {
                        crate::android_bridge::abandon_audio_focus();
                        self.audio_focus_held = false;
                    }
                }
                #[cfg(not(target_os = "android"))]
                let allow_soft_present = true;
                let poll_player = self
                    .playback_clock_poll
                    .map(|t| t.elapsed() >= std::time::Duration::from_millis(250))
                    .unwrap_or(true);
                if poll_player {
                    self.playback_clock_poll = Some(std::time::Instant::now());
                    self.session.refresh_times();
                    self.nudge_frozen_playback();
                    self.session.refresh_buffering_state();
                    self.session.poll_opening();
                    if self.settings.skip_credits_auto {
                        self.session.auto_skip_generic();
                    }
                    self.try_resume_seek();
                    self.store_watch_progress(false);
                }
                // External / Intent: no owned process — never treat as EOF.
                let owned_playback = !matches!(self.session.backend, Some(BackendId::External));
                if poll_player {
                    if owned_playback
                        && !self.session.native.is_running()
                        && matches!(
                            self.session.state,
                            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
                        )
                    {
                        let since = self
                            .playback_ended_since
                            .get_or_insert_with(std::time::Instant::now);
                        // Time-based debounce — independent of video_hz (12 ticks @120Hz was ~100ms).
                        // A stream that has not shown a picture yet can sit idle
                        // for a few seconds while the portal answers. Cutting it
                        // at 400 ms made every film and episode look dead.
                        let cold = self.session.position_secs < 0.5
                            && self.video_frame.is_none()
                            && !self.stage_picture;
                        let wait = if cold {
                            std::time::Duration::from_secs(8)
                        } else {
                            std::time::Duration::from_millis(400)
                        };
                        if since.elapsed() >= wait {
                            // A finished episode starts the next one, including
                            // the first episode of the following season.
                            if self.series_episode_finished() {
                                self.playback_ended_since = None;
                                self.store_watch_progress(true);
                                let task = self.play_neighbor(1);
                                let stuck = self.status == "Pas d'épisode suivant"
                                    || self.status == "Impossible de charger les épisodes";
                                if !stuck {
                                    return task;
                                }
                            }
                            self.store_watch_progress(true);
                            self.session.stop();
                            self.invalidate_soft_stage(false);
                            // Keep last GPU frame on stage (no black wipe).
                            self.status = if self.video_frame.is_none() && !self.stage_picture {
                                "Échec lecture — flux inaccessible".into()
                            } else {
                                "Lecture terminée".into()
                            };
                        }
                    } else {
                        self.playback_ended_since = None;
                    }
                }
                let mut tasks = Vec::new();
                #[cfg(target_os = "android")]
                if pip_layout_dirty {
                    if let Some(id) = self.player_id.or(self.main_id) {
                        tasks.push(Task::done(Message::PlayerLayoutDirty(id)));
                    }
                }
                #[cfg(not(target_os = "android"))]
                let gpu_stage = crate::video_stage::is_ready();
                #[cfg(target_os = "android")]
                let gpu_stage = false;
                #[cfg(not(target_os = "android"))]
                if gpu_stage && allow_soft_present {
                    self.present_stage_frame();
                }
                // While the GPU takes the current frame, keep only the newest
                // pixels. The upload callback sends them without waiting a tick.
                if allow_soft_present
                    && !gpu_stage
                    && self.video_upload_busy
                    && self.session.has_embedded_video()
                    && self.session.frame_needs_redraw()
                {
                    let (rw, rh) = self.soft_present_wh();
                    let hz = self.display_probe.monitor_hz.max(self.display_caps.video_hz);
                    self.session.set_present_hz(hz);
                    if self.video_pending.is_some() {
                        /* Keep the frame already waiting. Pulling again dropped it
                         * and the picture jumped. */
                    } else if let Some((w, h, rgba)) = self.session.pull_video_frame(rw, rh) {
                        let gen = self.video_upload_gen;
                        self.video_pending = Some((w, h, rgba, gen));
                    }
                }
                // Soft-render: SoftPump owns mpv SW off-UI. Never pull/render on the
                // iced thread while GPU upload is busy (double-render + ANR path).
                if allow_soft_present
                    && !gpu_stage
                    && !self.video_upload_busy
                    && self.session.has_embedded_video()
                    && matches!(
                        self.session.state,
                        PlaybackState::Playing
                            | PlaybackState::Buffering
                            | PlaybackState::Paused
                    )
                    && self.session.frame_needs_redraw()
                {
                    if let Some(task) = self.enqueue_soft_video_frame() {
                        tasks.push(task);
                    }
                }
                if let Some(deadline) = self.sleep_until {
                    if std::time::Instant::now() >= deadline {
                        self.sleep_until = None;
                        self.sleep_mins = None;
                        self.session.pause();
                        #[cfg(target_os = "android")]
                        {
                            self.pause_cause = PauseCause::User;
                        }
                        self.status = "Veille — lecture en pause".into();
                    }
                }
                let fit = self.fit_player_to_video();
                if !tasks.is_empty() {
                    tasks.push(fit);
                    return Task::batch(tasks);
                }
                return fit;
            }
            Message::VideoFrameAllocated { gen, w, h, result } => {
                if gen != self.video_upload_gen {
                    // Stop/close invalidated this upload — do not resurrect the stage.
                    return Task::none();
                }
                self.video_upload_busy = false;
                match result {
                    Ok(allocation) => {
                        self.video_frame_wh = (w, h);
                        self.video_frame = Some(allocation.handle().clone());
                        // Keep prior GPU textures so a slow draw cannot sample a
                        // worker atlas that was already reset for a newer frame.
                        self.video_allocation_hold4 = self.video_allocation_hold3.take();
                        self.video_allocation_hold3 = self.video_allocation_hold2.take();
                        self.video_allocation_hold2 = self.video_allocation_hold.take();
                        self.video_allocation_hold = self.video_allocation.take();
                        self.video_allocation = Some(allocation);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "video frame GPU allocate failed");
                    }
                }
                // Drain pending upload only. On Android never SW-pull here — that
                // bypasses video_hz and caused FocusEvent ANRs. Next PlayerTick pulls.
                if let Some((pw, ph, rgba, pgen)) = self.video_pending.take() {
                    if pgen == self.video_upload_gen {
                        self.video_upload_busy = true;
                        let handle = ImageHandle::from_rgba(pw, ph, rgba);
                        let gen = self.video_upload_gen;
                        return iced_image::allocate(handle).map(move |result| {
                            Message::VideoFrameAllocated {
                                gen,
                                w: pw,
                                h: ph,
                                result,
                            }
                        });
                    }
                    self.session.recycle_soft_rgba(rgba);
                }
                #[cfg(not(target_os = "android"))]
                {
                    // SoftPump already has pixels — do not wait a full monitor tick.
                    if !crate::video_stage::is_ready()
                        && self.session.has_embedded_video()
                        && self.session.frame_needs_redraw()
                        && matches!(
                            self.session.state,
                            PlaybackState::Playing
                                | PlaybackState::Buffering
                                | PlaybackState::Paused
                        )
                    {
                        if let Some(task) = self.enqueue_soft_video_frame() {
                            return task;
                        }
                    }
                }
            }
            Message::PlayerPanel(panel) => {
                self.player_panel = panel;
                if panel == PlayerPanel::More {
                    self.session.refresh_track_cache();
                }
                if panel == PlayerPanel::Remote {
                    self.cache_remote_qr();
                }
            }
            Message::CycleSpeed => {
                self.session.cycle_speed();
                self.status = format!("Vitesse {:.2}×", self.session.speed);
            }
            Message::SetSpeed(speed) => {
                self.session.set_speed(speed);
                self.status = format!("Vitesse {:.2}×", self.session.speed);
            }
            Message::ToggleLoop => {
                self.session.toggle_loop();
                self.status = if self.session.loop_file {
                    "Boucle activée".into()
                } else {
                    "Boucle off".into()
                };
            }
            Message::Screenshot => {
                let dir = {
                    #[cfg(target_os = "android")]
                    {
                        storage::data_dir().join("screenshots")
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        dirs::picture_dir()
                            .or_else(dirs::download_dir)
                            .unwrap_or_else(|| std::path::PathBuf::from("."))
                    }
                };
                let _ = std::fs::create_dir_all(&dir);
                let path = dir.join(format!(
                    "fluxplay-{}.png",
                    Local::now().format("%Y%m%d-%H%M%S")
                ));
                if self.session.screenshot_to_path(&path) {
                    self.status = format!("Capture · {}", path.display());
                } else if let Some((w, h, rgba)) = self.session.soft_rgba_snapshot() {
                    match image::RgbaImage::from_raw(w, h, rgba) {
                        Some(img) => match img.save(&path) {
                            Ok(()) => self.status = format!("Capture · {}", path.display()),
                            Err(e) => {
                                tracing::warn!(error = %e, "soft PNG save failed");
                                self.status = "Capture échouée".into();
                            }
                        },
                        None => self.status = "Capture échouée".into(),
                    }
                } else {
                    self.status = "Capture échouée".into();
                }
            }
            Message::GotoDraftChanged(s) => self.goto_draft = s,
            Message::GotoSubmit => {
                if let Some(secs) = parse_timecode(&self.goto_draft) {
                    self.session.seek_absolute(secs);
                    self.player_panel = PlayerPanel::None;
                    self.status = format!("Seek → {}", self.goto_draft);
                } else {
                    self.status = "Timecode invalide (mm:ss)".into();
                }
            }
            Message::ToggleSubVisibility => {
                self.status = if self.session.toggle_sub_visibility() {
                    "Visibilité sous-titres basculée".into()
                } else {
                    "Sous-titres non supportés".into()
                };
            }
            Message::CycleAspect => {
                self.status = if self.session.cycle_aspect() {
                    format!("Aspect {}", self.session.aspect.label())
                } else {
                    "Aspect non supporté".into()
                };
            }
            #[cfg(not(target_os = "android"))]
            Message::ToggleOntop => {
                self.session.toggle_ontop();
                if let Some(id) = self.player_id {
                    self.status = if self.session.ontop {
                        "Fenêtre lecteur toujours au-dessus".into()
                    } else {
                        "Toujours au-dessus off".into()
                    };
                    return window::set_level(
                        id,
                        if self.session.ontop {
                            window::Level::AlwaysOnTop
                        } else {
                            window::Level::Normal
                        },
                    );
                }
            }
            Message::TogglePip => {
                #[cfg(target_os = "android")]
                {
                    // Drive from live OS state — never invent pip_mode that fights isInPip.
                    let in_pip = crate::android_bridge::poll_pip_mode();
                    self.player_layout_freeze_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                    if !in_pip {
                        let (mut w, mut h) = if self.video_frame_wh.0 > 0 {
                            self.video_frame_wh
                        } else {
                            (16, 9)
                        };
                        // Android PiP aspect must stay within ~1:2.39 … 2.39:1.
                        let aspect = w.max(1) as f32 / h.max(1) as f32;
                        if aspect > 2.39 {
                            h = ((w as f32 / 2.39).round() as u32).max(1);
                        } else if aspect < 1.0 / 2.39 {
                            w = ((h as f32 / 2.39).round() as u32).max(1);
                        }
                        crate::android_bridge::enter_pip(w.max(1) as i32, h.max(1) as i32);
                        if !self.audio_focus_held {
                            crate::android_bridge::request_audio_focus();
                            if crate::android_bridge::poll_audio_focus_held() == Some(true) {
                                self.audio_focus_held = true;
                            }
                        }
                        self.status = "PiP système".into();
                    } else {
                        crate::android_bridge::exit_pip();
                        self.status = "Sortie PiP…".into();
                    }
                    self.pip_mode = crate::android_bridge::poll_pip_mode();
                    if let Some(id) = self.player_id.or(self.main_id) {
                        return Task::perform(
                            async {
                                tokio::time::sleep(std::time::Duration::from_millis(650)).await;
                            },
                            move |_| Message::PlayerLayoutDirty(id),
                        );
                    }
                    return Task::none();
                }
                #[cfg(not(target_os = "android"))]
                {
                    self.pip_mode = !self.pip_mode;
                    self.player_layout_freeze_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                    if let Some(id) = self.player_id {
                        let size = if self.pip_mode {
                            Size::new(480.0, 320.0)
                        } else {
                            Size::new(1280.0, 720.0)
                        };
                        self.session.ontop = self.pip_mode;
                        let _ = self.session.native.set_ontop(self.pip_mode);
                        self.status = if self.pip_mode {
                            "PiP".into()
                        } else {
                            "PiP off".into()
                        };
                        return Task::batch([
                            window::resize(id, size),
                            window::set_level(
                                id,
                                if self.pip_mode {
                                    window::Level::AlwaysOnTop
                                } else {
                                    window::Level::Normal
                                },
                            ),
                            Task::perform(
                                async {
                                    tokio::time::sleep(std::time::Duration::from_millis(650)).await;
                                },
                                move |_| Message::PlayerLayoutDirty(id),
                            ),
                        ]);
                    }
                }
            }
            Message::ChapterStep(d) => self.session.chapter_step(d),
            Message::PlaylistPrev => return self.play_neighbor(-1),
            Message::PlaylistNext => return self.play_neighbor(1),
            Message::SeriesQueueReady {
                for_url,
                title,
                result,
            } => {
                if self.series_queue_loading.as_deref() == Some(for_url.as_str()) {
                    self.series_queue_loading = None;
                }
                let same = self.session.channel.as_ref().is_some_and(|c| {
                    c.kind == ContentKind::Series && (c.stream_url == for_url || c.name == title)
                });
                if !same {
                    self.series_neighbor_pending = None;
                    return Task::none();
                }
                let Ok(item) = result else {
                    if self.series_neighbor_pending.take().is_some() {
                        self.status = "Impossible de charger les épisodes".into();
                    }
                    return Task::none();
                };
                if let (Some(db), Some(sid)) = (&self.catalog_db, item.source_id) {
                    let mut saved = item.clone();
                    saved.source_id = Some(sid);
                    let _ = db.persist_series(&saved);
                }
                if let Some((queue, idx)) = queue_from_series(&item, &for_url, &title) {
                    self.series_queue = queue;
                    self.series_queue_idx = idx;
                    if let Some(delta) = self.series_neighbor_pending.take() {
                        if let Some(msg) = self.playlist_neighbor(delta) {
                            return Task::done(msg);
                        }
                        self.status = if delta < 0 {
                            "Pas d'épisode précédent".into()
                        } else {
                            "Pas d'épisode suivant".into()
                        };
                    }
                } else if self.series_neighbor_pending.take().is_some() {
                    self.status = "Pas d'épisode suivant".into();
                }
            }
            Message::AddBookmark => {
                self.session.add_bookmark();
                self.status = "Signet ajouté".into();
            }
            Message::JumpBookmark(i) => self.session.jump_bookmark(i),
            Message::CycleSleepTimer => {
                self.sleep_mins = match self.sleep_mins {
                    None => Some(15),
                    Some(15) => Some(30),
                    Some(30) => Some(60),
                    _ => None,
                };
                self.sleep_until = self
                    .sleep_mins
                    .map(|m| std::time::Instant::now() + std::time::Duration::from_secs(m as u64 * 60));
                self.status = match self.sleep_mins {
                    Some(m) => format!("Veille dans {m} min"),
                    None => "Veille off".into(),
                };
            }
            Message::MarkAbA => {
                self.status = if self.session.mark_ab_a() {
                    "Point A".into()
                } else {
                    "A–B non supporté".into()
                };
            }
            Message::MarkAbB => {
                self.status = if self.session.mark_ab_b() {
                    "Point B".into()
                } else {
                    "A–B non supporté".into()
                };
            }
            Message::ClearAbLoop => {
                self.status = if self.session.clear_ab_loop() {
                    "A–B off".into()
                } else {
                    "A–B non supporté".into()
                };
            }
            Message::SubDelay(d) => {
                self.session.nudge_sub_delay(d);
                self.status = format!("ST {:+.1}s", self.session.sub_delay);
            }
            Message::AudioDelay(d) => {
                self.session.nudge_audio_delay(d);
                self.status = format!("A/V {:+.1}s", self.session.audio_delay);
            }
            Message::CycleAudioMode => {
                self.session.cycle_audio_mode();
                self.status = self.session.audio_mode.label().into();
            }
            Message::CycleEq => {
                self.session.cycle_eq();
                self.status = self.session.eq_preset.label().into();
            }
            Message::ToggleLoudnorm => {
                self.session.toggle_loudnorm();
                self.status = if self.session.loudnorm {
                    "Normalisation ON".into()
                } else {
                    "Normalisation off".into()
                };
            }
            Message::ToggleDeinterlace => {
                self.session.toggle_deinterlace();
                self.status = self.session.deinterlace.label().into();
            }
            Message::CycleUpscale => {
                self.session.cycle_upscale();
                self.status = self.session.upscale.label().into();
            }
            Message::CycleRotate => {
                self.session.cycle_rotate();
                self.status = format!("Rotation {}°", self.session.rotate_deg);
            }
            Message::NudgeZoom(d) => {
                self.session.nudge_zoom(d);
                self.status = format!("Zoom {:+.2}", self.session.zoom);
            }
            Message::ToggleNightVf => {
                self.session.toggle_night_vf();
                self.settings.night_mode = self.session.night_vf;
                self.persist();
                self.status = if self.session.night_vf {
                    "Mode nuit image ON".into()
                } else {
                    "Mode nuit image off".into()
                };
            }
            Message::CycleCache => {
                self.settings.cache_ms = match self.settings.cache_ms {
                    0..=2999 => 4000,
                    3000..=5999 => 8000,
                    6000..=11999 => 16000,
                    _ => 2000,
                };
                self.resync_player_options();
                self.persist();
                self.status = format!("Cache réseau {} ms (prochain flux)", self.settings.cache_ms);
            }
            Message::CycleDemux => {
                self.settings.demux_secs = match self.settings.demux_secs as i32 {
                    0..=5 => 8.0,
                    6..=11 => 16.0,
                    12..=23 => 24.0,
                    _ => 4.0,
                };
                self.resync_player_options();
                self.persist();
                self.status = format!(
                    "Buffer demux {:.0}s (prochain flux)",
                    self.settings.demux_secs
                );
            }
            Message::PlayerHotkeyIn(id, hk) => {
                #[cfg(not(target_os = "android"))]
                if self.player_id != Some(id) {
                    return Task::none();
                }
                #[cfg(target_os = "android")]
                let _ = id;
                return Task::done(Message::PlayerHotkey(hk));
            }
            Message::PlayerHotkey(hk) => {
                self.player_chrome_visible = true;
                self.player_pointer_at = Some(std::time::Instant::now());
                if self.session.channel.is_none()
                    && !matches!(hk, PlayerHotkey::Mute | PlayerHotkey::VolumeUp | PlayerHotkey::VolumeDown)
                {
                    // Still allow volume when idle after play
                }
                match hk {
                    PlayerHotkey::TogglePause => {
                        if self.session.caps().pause {
                            if self.session.state == PlaybackState::Paused {
                                self.session.resume();
                            } else {
                                self.session.pause();
                            }
                            self.status = self.session.status_line();
                        }
                    }
                    PlayerHotkey::SeekBack if self.session.caps().seek_rel => {
                        self.session.seek_relative(-10.0)
                    }
                    PlayerHotkey::SeekFwd if self.session.caps().seek_rel => {
                        self.session.seek_relative(10.0)
                    }
                    PlayerHotkey::SeekBackBig if self.session.caps().seek_rel => {
                        self.session.seek_relative(-30.0)
                    }
                    PlayerHotkey::SeekFwdBig if self.session.caps().seek_rel => {
                        self.session.seek_relative(30.0)
                    }
                    PlayerHotkey::VolumeUp if self.session.caps().volume_live => {
                        self.session.volume_delta(0.05);
                        self.settings.volume = self.session.volume;
                        self.persist();
                    }
                    PlayerHotkey::VolumeDown if self.session.caps().volume_live => {
                        self.session.volume_delta(-0.05);
                        self.settings.volume = self.session.volume;
                        self.persist();
                    }
                    PlayerHotkey::Mute if self.session.caps().mute => self.session.toggle_mute(),
                    PlayerHotkey::SeekBack
                    | PlayerHotkey::SeekFwd
                    | PlayerHotkey::SeekBackBig
                    | PlayerHotkey::SeekFwdBig
                    | PlayerHotkey::VolumeUp
                    | PlayerHotkey::VolumeDown
                    | PlayerHotkey::Mute => {}
                    PlayerHotkey::Fullscreen => {
                        return Task::done(Message::ToggleFullscreen);
                    }
                    PlayerHotkey::Escape => {
                        self.player_chrome_visible = true;
                        self.player_pointer_at = Some(std::time::Instant::now());
                        if self.player_fullscreen {
                            return Task::done(Message::ToggleFullscreen);
                        }
                        if self.player_panel != PlayerPanel::None {
                            self.player_panel = PlayerPanel::None;
                            return Task::none();
                        }
                        return Task::done(Message::ClosePlayerWindow);
                    }
                    PlayerHotkey::Stop => {
                        if self.session.caps().owned {
                            return Task::done(Message::Stop);
                        }
                    }
                    PlayerHotkey::Restart => {
                        if self.session.caps().owned {
                            self.session.restart();
                        }
                    }
                    PlayerHotkey::Speed => {
                        if self.session.caps().speed_loop {
                            self.session.cycle_speed();
                            self.status = format!("Vitesse {:.2}×", self.session.speed);
                        }
                    }
                    PlayerHotkey::Loop => {
                        if self.session.caps().speed_loop {
                            self.session.toggle_loop();
                        }
                    }
                    PlayerHotkey::Screenshot => {
                        if self.session.caps().screenshot {
                            return Task::done(Message::Screenshot);
                        }
                    }
                    PlayerHotkey::FrameStep => {
                        let _ = self.session.frame_step();
                    }
                }
            }
            Message::CycleTheme => {
                self.settings.theme = self.settings.theme.cycle();
                tracing::debug!(theme = ?self.settings.theme, "cycle theme");
                self.persist();
            }
            Message::SetAccent(preset) => {
                self.settings.accent = preset;
                self.settings.custom_accent = None;
                self.custom_open = false;
                self.accent_open = false;
                self.accent_query.clear();
                tracing::debug!(accent = ?preset, "set accent");
                self.persist();
            }
            Message::ToggleCategories => {
                // The tongue fires on release. If that release also hits the poster
                // that slides under the cursor, the one-shot below drops it.
                let closing = self.cats_open;
                self.cats_open = !self.cats_open;
                if closing {
                    self.swallow_tile_release = true;
                }
            }
            Message::ToggleCustomColor => {
                self.custom_open = !self.custom_open;
                if self.custom_open {
                    self.ink_open = false;
                }
            }
            Message::SetInk(rgb) => {
                self.settings.custom_ink = rgb;
                if rgb.is_none() {
                    self.ink_open = false;
                }
                self.persist();
            }
            Message::ToggleInkPicker => {
                self.ink_open = !self.ink_open;
                if self.ink_open {
                    self.custom_open = false;
                    if let Some(rgb) = self.settings.custom_ink {
                        self.accent_cursor = crate::theme::cursor_for_rgb(rgb);
                        self.accent_hover = self.accent_cursor;
                    }
                }
            }
            Message::AccentDrag(down) => {
                self.accent_dragging = down;
                if down {
                    let (x, y) = self.accent_hover;
                    self.accent_cursor = (x, y);
                    let rgb = crate::theme::pick_color(x, y);
                    if self.ink_open {
                        self.settings.custom_ink = Some(rgb);
                    } else {
                        self.settings.custom_accent = Some(rgb);
                    }
                } else if self.ink_open {
                    if self.settings.custom_ink.is_some() {
                        self.persist();
                    }
                } else if self.settings.custom_accent.is_some() {
                    self.persist();
                }
            }
            Message::AccentPoint(x, y) => {
                let x = x.clamp(0.0, 1.0);
                let y = y.clamp(0.0, 1.0);
                self.accent_hover = (x, y);
                if self.accent_dragging {
                    self.accent_cursor = (x, y);
                    let rgb = crate::theme::pick_color(x, y);
                    if self.ink_open {
                        self.settings.custom_ink = Some(rgb);
                    } else {
                        self.settings.custom_accent = Some(rgb);
                    }
                }
            }
            Message::SettingsQuery(q) => self.settings_query = q,
            Message::ToggleSettings(section) => {
                self.settings_open = if self.settings_open == Some(section) {
                    None
                } else {
                    Some(section)
                };
            }
            Message::ToggleAccentMenu => {
                self.accent_open = !self.accent_open;
                if self.accent_open && self.settings.custom_accent.is_some() {
                    self.custom_open = true;
                }
            }
            Message::AccentQuery(q) => self.accent_query = q,
            Message::FormName(s) => self.form_name = s,
            Message::FormKind(k) => {
                self.form_kind = k;
                self.form_error = None;
            }
            // The error banner stays until the next submit: dropping it on the first
            // keystroke reflowed the form and moved focus to the next field mid-typing.
            Message::FormEndpoint(s) => self.form_endpoint = s,
            Message::FormUser(s) => self.form_user = s,
            Message::FormPass(s) => self.form_pass = s,
            Message::FormMac(s) => self.form_mac = s,
            Message::FormEpg(s) => self.form_epg = s,
            Message::FormMirrors(s) => self.form_mirrors = s,
            Message::EditSource(id) => {
                if let Some(s) = self.sources.iter().find(|s| s.id == id) {
                    self.form_name = s.name.clone();
                    self.form_kind = s.kind;
                    self.form_endpoint = s.endpoint.clone();
                    self.form_user = s.username.clone().unwrap_or_default();
                    self.form_pass = s.password.clone().unwrap_or_default();
                    self.form_mac = s.mac.clone().unwrap_or_default();
                    self.form_epg = s.epg_url.clone().unwrap_or_default();
                    self.form_mirrors = s.mirrors.join("\n");
                    self.editing_source = Some(id);
                    self.tab = Tab::Settings;
                    self.settings_open = Some(SettingsSection::Sources);
                    self.status = format!("Modification — {}", s.name);
                }
            }
            Message::CancelEditSource => {
                self.clear_source_form();
                self.status = "Modification annulée".into();
            }
            Message::ServersRanked(id, up, total) => {
                if let Some(s) = self.sources.iter().find(|s| s.id == id) {
                    self.status = if up == 0 {
                        format!("{} : aucun serveur ne répond ({total} essayés)", s.name)
                    } else {
                        format!("{} : {up}/{total} serveurs joignables", s.name)
                    };
                }
            }
            Message::FormOmdbKey(s) => self.form_omdb_key = s,
            Message::FormTmdbKey(s) => self.form_tmdb_key = s,
            Message::FormDnsServers(s) => self.form_dns_servers = s,
            Message::FormDohUrl(s) => self.form_doh_url = s,
            Message::FormDotServer(s) => self.form_dot_server = s,
            Message::FormWgPaste(s) => self.form_wg_paste = s,
            Message::PasteInto(target) => {
                return iced::clipboard::read()
                    .map(move |text| Message::ClipboardText(target, text));
            }
            Message::ClipboardText(target, text) => {
                let Some(text) = text.filter(|t| !t.is_empty()) else {
                    self.status = "Presse-papiers vide".into();
                    return Task::none();
                };
                // Strip control chars (same filter as iced text_input paste).
                let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
                match target {
                    PasteTarget::FormName => self.form_name = cleaned,
                    PasteTarget::FormEndpoint => self.form_endpoint = cleaned,
                    PasteTarget::FormUser => self.form_user = cleaned,
                    PasteTarget::FormPass => self.form_pass = cleaned,
                    PasteTarget::FormMac => self.form_mac = cleaned,
                    PasteTarget::FormEpg => self.form_epg = cleaned,
                    // A pasted list keeps its separators.
                    PasteTarget::FormMirrors => {
                        self.form_mirrors = text
                            .chars()
                            .map(|c| if c.is_control() { ' ' } else { c })
                            .collect()
                    }
                    PasteTarget::FormOmdbKey => self.form_omdb_key = cleaned,
                    PasteTarget::FormTmdbKey => self.form_tmdb_key = cleaned,
                    PasteTarget::FormDnsServers => self.form_dns_servers = cleaned,
                    PasteTarget::FormDohUrl => self.form_doh_url = cleaned,
                    PasteTarget::FormDotServer => self.form_dot_server = cleaned,
                    PasteTarget::FormWgPaste => self.form_wg_paste = cleaned,
                    PasteTarget::Search => {
                        self.search = cleaned;
                        self.list_limit = self.browse_page();
                        self.search_debounce_gen = self.search_debounce_gen.wrapping_add(1);
                        let gen = self.search_debounce_gen;
                        self.status = "Texte collé".into();
                        return Task::perform(
                            async {
                                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                            },
                            move |_| Message::SearchApply(gen),
                        );
                    }
                    PasteTarget::CatFilter => {
                        self.cat_filter = cleaned;
                        self.rebuild_cat_entries();
                    }
                    PasteTarget::Goto => self.goto_draft = cleaned,
                    PasteTarget::PartyLink => {
                        self.party.draft = cleaned;
                        return self.update(Message::PartyPair(self.party.draft.clone()));
                    }
                }
                self.status = "Texte collé".into();
            }
            Message::SaveOmdbKey => {
                self.settings.omdb_api_key = self.form_omdb_key.trim().to_string();
                crate::metadata::set_omdb_api_key(if self.settings.omdb_api_key.is_empty() {
                    None
                } else {
                    Some(self.settings.omdb_api_key.clone())
                });
                self.persist();
                self.status = if crate::metadata::omdb_configured() {
                    "Clé OMDb enregistrée — rouvrez une fiche film/série".into()
                } else {
                    "Clé OMDb effacée".into()
                };
            }
            Message::SaveTmdbKey => {
                self.settings.tmdb_api_key = self.form_tmdb_key.trim().to_string();
                self.settings.tmdb_prompt_done = true;
                crate::metadata::set_tmdb_token(if self.settings.tmdb_api_key.is_empty() {
                    None
                } else {
                    Some(self.settings.tmdb_api_key.clone())
                });
                self.persist();
                self.status = if crate::metadata::tmdb_configured() {
                    "Jeton remplacé — rouvrez une fiche".into()
                } else {
                    "Jeton effacé".into()
                };
            }
            Message::ClearTmdbKey => {
                self.form_tmdb_key.clear();
                self.settings.tmdb_api_key.clear();
                self.settings.tmdb_prompt_done = true;
                crate::metadata::set_tmdb_token(None);
                self.persist();
                self.status = "Jeton effacé".into();
            }
            Message::ApiPromptHold => {}
            Message::ApiPromptSkip => {
                self.settings.tmdb_prompt_done = true;
                self.api_guide = false;
                self.persist();
                self.status = "Fiches du portail, sans clé TMDB".into();
            }
            Message::ApiPromptSetup => {
                self.settings.tmdb_prompt_done = true;
                self.api_guide = true;
                self.tab = Tab::Settings;
                self.settings_open = Some(SettingsSection::Meta);
                self.settings_query.clear();
                self.persist();
                self.status = "Assistant API — environ 10 minutes".into();
            }
            Message::ApiGuideToggle => {
                self.api_guide = !self.api_guide;
                if self.api_guide {
                    self.tab = Tab::Settings;
                    self.settings_open = Some(SettingsSection::Meta);
                    self.settings_query.clear();
                }
            }
            Message::OpenWeb(url) => {
                #[cfg(target_os = "android")]
                {
                    match crate::android_intent::open_url(&url, None) {
                        Ok(()) => self.status = "Page ouverte".into(),
                        Err(e) => self.status = format!("Navigateur: {e}"),
                    }
                }
                #[cfg(not(target_os = "android"))]
                {
                    let _ = open::that(&url);
                    self.status = "Page ouverte dans le navigateur".into();
                }
            }
            Message::SetPrefLang(code) => {
                if self.settings.pref_lang == code {
                    return Task::none();
                }
                self.settings.pref_lang = code;
                if self.settings.pref_lang.is_empty() {
                    self.settings.only_pref_lang = false;
                }
                crate::metadata::set_pref_lang(Some(self.settings.pref_lang.clone()));
                self.translations.clear();
                self.translating.clear();
                self.persist();
                self.status = match crate::names::lang_label(&self.settings.pref_lang) {
                    Some(l) => format!("Langue : {l} — catégories dans votre langue en premier"),
                    None => "Langue : aucune — synopsis d’origine, ordre du portail".into(),
                };
                return Task::batch([
                    self.rebuild_browse_index(),
                    self.translate_open_detail_task(),
                ]);
            }
            Message::ToggleOnlyPrefLang => {
                if self.settings.pref_lang.is_empty() {
                    self.status = "Choisissez d’abord votre langue".into();
                    return Task::none();
                }
                self.settings.only_pref_lang = !self.settings.only_pref_lang;
                self.persist();
                self.status = if self.settings.only_pref_lang {
                    "Catalogue limité à votre langue".into()
                } else {
                    "Catalogue complet".into()
                };
                return self.rebuild_browse_index();
            }
            Message::TranslationsReady(lang, pairs) => {
                for (src, dst) in pairs {
                    self.translating.remove(&src);
                    if lang == self.settings.pref_lang {
                        self.translations.insert(src, dst);
                    }
                }
                if self.translations.len() > 4000 {
                    self.translations.clear();
                    return self.translate_open_detail_task();
                }
            }
            Message::CycleDnsMode => {
                self.settings.network.dns_mode = self.settings.network.dns_mode.cycle();
                crate::network::apply_to_http(&self.settings.network);
                self.persist();
                self.status = format!("DNS : {}", self.settings.network.dns_mode.label());
            }
            Message::SaveNetworkDns => {
                self.settings.network.dns_servers = self.form_dns_servers.trim().to_string();
                self.settings.network.doh_url = self.form_doh_url.trim().to_string();
                self.settings.network.dot_server = self.form_dot_server.trim().to_string();
                if self.settings.network.doh_url.is_empty() {
                    self.settings.network.doh_url = NetworkSettings::default().doh_url;
                }
                if self.settings.network.dot_server.is_empty() {
                    self.settings.network.dot_server = NetworkSettings::default().dot_server;
                }
                crate::network::apply_to_http(&self.settings.network);
                self.persist();
                self.status = "Paramètres DNS enregistrés".into();
            }
            Message::ProbeDns => {
                self.network_probe = "Test DNS…".into();
                return Task::perform(
                    async { crate::wg_tunnel::probe_dns("cloudflare.com").await },
                    Message::DnsProbeDone,
                );
            }
            Message::DnsProbeDone(s) => {
                self.network_probe = s;
            }
            Message::ToggleWireGuard => {
                if self.settings.network.wireguard_profile_path.is_empty() {
                    self.status = "Importez d’abord un profil WireGuard (.conf)".into();
                    return Task::none();
                }
                let enable = !self.settings.network.wireguard_enabled;
                self.settings.network.wireguard_enabled = enable;
                self.persist();
                if !enable {
                    crate::wg_tunnel::stop_tunnel();
                    crate::network::apply_to_http(&self.settings.network);
                    *self.session.native.options_mut() = play_options_from(&self.settings);
                    self.status = "Tunnel WireGuard arrêté".into();
                    return Task::none();
                }
                crate::network::apply_to_http(&self.settings.network);
                *self.session.native.options_mut() = play_options_from(&self.settings);
                self.status = "Démarrage tunnel WireGuard (app only)…".into();
                let path = self.settings.network.wireguard_profile_path.clone();
                let bootstrap = self.settings.network.wireguard_bootstrap_dns.clone();
                return Task::perform(
                    async move {
                        crate::wg_tunnel::start_tunnel_from_file(
                            std::path::Path::new(&path),
                            &bootstrap,
                        )
                        .await
                        .map(Some)
                    },
                    Message::WireGuardTunnelDone,
                );
            }
            Message::WireGuardTunnelDone(result) => {
                let pending_sync = self.pending_portal_sync_after_wg;
                self.pending_portal_sync_after_wg = false;
                match result {
                    Ok(Some(url)) => {
                        crate::network::apply_to_http(&self.settings.network);
                        *self.session.native.options_mut() = play_options_from(&self.settings);
                        self.status = format!("Tunnel app actif — {url}");
                        if pending_sync {
                            return self.reload_all_task();
                        }
                    }
                    Ok(None) => {
                        crate::wg_tunnel::stop_tunnel();
                        crate::network::apply_to_http(&self.settings.network);
                        *self.session.native.options_mut() = play_options_from(&self.settings);
                        self.status = "Tunnel arrêté".into();
                        if pending_sync {
                            return self.reload_all_task();
                        }
                    }
                    Err(e) => {
                        self.settings.network.wireguard_enabled = false;
                        crate::wg_tunnel::stop_tunnel();
                        crate::network::apply_to_http(&self.settings.network);
                        *self.session.native.options_mut() = play_options_from(&self.settings);
                        self.persist();
                        self.status = format!("Échec tunnel WireGuard: {e}");
                        // Still reload so the app works without the tunnel.
                        if pending_sync {
                            return self.reload_all_task();
                        }
                    }
                }
            }
            Message::PickWireGuardProfile => {
                #[cfg(target_os = "android")]
                {
                    self.saf_kind = Some(SafKind::WireGuard);
                    self.status = "Choisissez un fichier .conf…".into();
                    crate::android_bridge::start_saf_open("*/*");
                    return Task::none();
                }
                #[cfg(not(target_os = "android"))]
                {
                    return Task::perform(
                        async {
                            rfd::AsyncFileDialog::new()
                                .add_filter("WireGuard", &["conf"])
                                .set_title("Profil WireGuard")
                                .pick_file()
                                .await
                                .map(|f| f.path().to_path_buf())
                        },
                        Message::WireGuardProfilePicked,
                    );
                }
            }
            Message::WireGuardProfilePicked(path) => {
                let Some(path) = path else {
                    return Task::none();
                };
                match crate::network::import_wireguard_profile(&self.settings.network, &path) {
                    Ok(net) => {
                        self.settings.network = net;
                        self.form_dns_servers = self.settings.network.dns_servers.clone();
                        crate::network::apply_to_http(&self.settings.network);
                        self.persist();
                        self.status = format!(
                            "Profil WireGuard importé : {}",
                            self.settings.network.wireguard_profile_name
                        );
                    }
                    Err(e) => self.status = e,
                }
            }
            Message::ImportWireGuardPaste => {
                match crate::network::import_wireguard_text(
                    &self.settings.network,
                    &self.form_wg_paste,
                    "collé.conf",
                ) {
                    Ok(net) => {
                        self.settings.network = net;
                        self.form_dns_servers = self.settings.network.dns_servers.clone();
                        self.form_wg_paste.clear();
                        crate::network::apply_to_http(&self.settings.network);
                        self.persist();
                        self.status = "Profil WireGuard collé et enregistré".into();
                    }
                    Err(e) => self.status = e,
                }
            }
            Message::ClearWireGuardProfile => {
                self.settings.network =
                    crate::network::clear_wireguard_profile(&self.settings.network);
                crate::network::apply_to_http(&self.settings.network);
                self.persist();
                self.status = "Profil WireGuard supprimé".into();
            }
            Message::ApplyWireGuardDns => {
                match crate::network::refresh_bootstrap_dns(&self.settings.network) {
                    Ok(net) => {
                        self.settings.network = net;
                        self.persist();
                        self.status = format!(
                            "DNS profil mémorisé (bootstrap Endpoint uniquement) : {}",
                            self.settings.network.wireguard_bootstrap_dns
                        );
                    }
                    Err(e) => self.status = e,
                }
            }
            Message::AddPublicDemo { name, endpoint } => {
                let mut src = MediaSource::new(name.trim(), SourceKind::M3uPlus, endpoint.trim());
                src.enabled = true;
                let id = src.id;
                self.sources.push(src);
                demo::strip_demo_if_real(&mut self.sources);
                {
                    let ids: Vec<Uuid> = self.sources.iter().map(|s| s.id).collect();
                    self.catalog_db = crate::catalog_db::CatalogDb::open(&ids);
                    if let Some(db) = &mut self.catalog_db {
                        let _ = db.ensure_source(id);
                    }
                }
                if let Some(s) = self.sources.iter().find(|s| s.id == id) {
                    crate::storage::write_profile_source_json(s);
                }
                self.persist();
                self.status = "Playlist publique ajoutée — sync…".to_string();
                self.loading = true;
                return self.reload_one_task(id);
            }
            Message::AddSource => {
                let missing = if self.form_endpoint.trim().is_empty() {
                    Some(match self.form_kind {
                        SourceKind::Xtream => "Indiquez l’URL du serveur Xtream.",
                        SourceKind::Stalker => "Indiquez l’URL du portail Stalker.",
                        SourceKind::Xmltv => "Indiquez l’URL du guide XMLTV.",
                        _ => "Indiquez l’URL ou le fichier de la playlist M3U.",
                    })
                } else if matches!(self.form_kind, SourceKind::Xtream)
                    && (self.form_user.trim().is_empty() || self.form_pass.trim().is_empty())
                {
                    Some("Xtream : identifiant et mot de passe requis.")
                } else if self.form_kind == SourceKind::Stalker && self.form_mac.trim().is_empty() {
                    Some("Stalker : adresse MAC requise.")
                } else {
                    None
                };
                if let Some(msg) = missing {
                    tracing::warn!(%msg, "add source rejected");
                    self.form_error = Some(msg.into());
                    self.status = msg.into();
                    return Task::none();
                }
                self.form_error = None;
                // Name is optional: derive one from the endpoint instead of blocking.
                if self.form_name.trim().is_empty() {
                    self.form_name = default_source_name(
                        &self.form_endpoint,
                        &self.form_user,
                        self.sources.len() + 1,
                    );
                }
                let opt = |s: &str| {
                    let s = s.trim();
                    (!s.is_empty()).then(|| s.to_string())
                };
                let mirrors = MediaSource::parse_mirror_list(&self.form_mirrors);
                if let Some(id) = self.editing_source {
                    let Some(src) = self.sources.iter_mut().find(|s| s.id == id) else {
                        self.clear_source_form();
                        return Task::none();
                    };
                    tracing::info!(%id, kind = ?self.form_kind, mirrors = mirrors.len(), "edit source");
                    let before = (
                        src.kind,
                        src.endpoint.clone(),
                        src.mirrors.clone(),
                        src.username.clone(),
                        src.password.clone(),
                        src.mac.clone(),
                        src.epg_url.clone(),
                    );
                    src.name = self.form_name.trim().to_string();
                    src.kind = self.form_kind;
                    src.endpoint = self.form_endpoint.trim().to_string();
                    src.mirrors = mirrors;
                    src.username = opt(&self.form_user);
                    src.password = opt(&self.form_pass);
                    src.mac = opt(&self.form_mac);
                    src.epg_url = opt(&self.form_epg);
                    let changed = before
                        != (
                            src.kind,
                            src.endpoint.clone(),
                            src.mirrors.clone(),
                            src.username.clone(),
                            src.password.clone(),
                            src.mac.clone(),
                            src.epg_url.clone(),
                        );
                    crate::storage::write_profile_source_json(src);
                    self.clear_source_form();
                    self.persist();
                    if !changed {
                        self.status = "Profil enregistré".into();
                        return self.rebuild_bundle_from_cache_task();
                    }
                    fluxplay_providers::servers::forget(id);
                    self.status = "Profil modifié — sync catalogue…".into();
                    self.loading = true;
                    return self.reload_one_task(id);
                }
                tracing::info!(
                    name = %self.form_name.trim(),
                    kind = ?self.form_kind,
                    "add source"
                );
                let mut src =
                    MediaSource::new(self.form_name.trim(), self.form_kind, self.form_endpoint.trim());
                src.username = opt(&self.form_user);
                src.password = opt(&self.form_pass);
                src.mac = opt(&self.form_mac);
                src.epg_url = opt(&self.form_epg);
                src.mirrors = mirrors;
                let id = src.id;
                self.sources.push(src);
                demo::strip_demo_if_real(&mut self.sources);
                {
                    let ids: Vec<Uuid> = self.sources.iter().map(|s| s.id).collect();
                    self.catalog_db = crate::catalog_db::CatalogDb::open(&ids);
                    if let Some(db) = &mut self.catalog_db {
                        let _ = db.ensure_source(id);
                    }
                }
                if let Some(s) = self.sources.iter().find(|s| s.id == id) {
                    crate::storage::write_profile_source_json(s);
                }
                self.clear_source_form();
                self.persist();
                self.status = "Source ajoutée — sync catalogue…".into();
                self.loading = true;
                return self.reload_one_task(id);
            }
            Message::RemoveSource(id) => {
                tracing::info!(%id, "remove source");
                self.sources.retain(|s| s.id != id);
                fluxplay_providers::servers::forget(id);
                if self.editing_source == Some(id) {
                    self.clear_source_form();
                }
                if let Some(db) = &mut self.catalog_db {
                    if let Err(e) = db.delete_source(id) {
                        tracing::warn!(error = %e, %id, "catalog delete_source failed");
                    }
                }
                self.persist();
                self.status = "Source retirée".into();
                return self.rebuild_bundle_from_cache_task();
            }
            Message::ReloadSource(id) => {
                self.loading = true;
                self.status = "Rechargement… comparaison checksum".into();
                // Force fresh portal JSON. Identical spines skip the rewrite;
                // anything else upserts new rows and the ones already stored.
                fluxplay_providers::clear_xtream_cache();
                return self.reload_one_task(id);
            }
            Message::ExportProfile(id) => {
                let Some(src) = self.sources.iter().find(|s| s.id == id).cloned() else {
                    self.status = "Source introuvable".into();
                    return Task::none();
                };
                crate::storage::write_profile_source_json(&src);
                self.status = "Export profil…".into();
                #[cfg(not(target_os = "android"))]
                {
                    return Task::perform(
                        async move {
                            let name = crate::profile_io::suggested_export_name(&src);
                            match tokio::task::spawn_blocking(move || {
                                let dest = rfd::FileDialog::new()
                                    .set_file_name(&name)
                                    .add_filter("FluxPlay profile", &["fluxplay"])
                                    .set_directory(crate::profile_io::default_export_dir())
                                    .save_file();
                                let Some(dest) = dest else {
                                    return Err("Export annulé".into());
                                };
                                crate::profile_io::export_profile(&src, &dest)?;
                                Ok(dest.display().to_string())
                            })
                            .await
                            {
                                Ok(r) => r,
                                Err(e) => Err(e.to_string()),
                            }
                        },
                        Message::ProfileExportDone,
                    );
                }
                #[cfg(target_os = "android")]
                {
                    if let Some(db) = &self.catalog_db {
                        db.wal_checkpoint_all();
                    }
                    let dest = crate::storage::data_dir()
                        .join("exports")
                        .join(crate::profile_io::suggested_export_name(&src));
                    self.saf_kind = Some(SafKind::ProfileExport);
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                if let Some(parent) = dest.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                crate::profile_io::export_profile(&src, &dest)?;
                                Ok(dest.display().to_string())
                            })
                            .await
                            .map_err(|e| e.to_string())?
                        },
                        |result| match result {
                            Ok(path) => {
                                crate::android_bridge::start_saf_create(
                                    "application/zip",
                                    &path,
                                );
                                Message::ProfileExportDone(Ok(format!(
                                    "{path} — choisissez où l’enregistrer"
                                )))
                            }
                            Err(e) => Message::ProfileExportDone(Err(e)),
                        },
                    );
                }
            }
            Message::ImportProfile => {
                self.status = "Import profil…".into();
                #[cfg(target_os = "android")]
                {
                    self.saf_kind = Some(SafKind::ProfileImport);
                    crate::android_bridge::start_saf_open("*/*");
                    return Task::none();
                }
                #[cfg(not(target_os = "android"))]
                {
                    return Task::perform(
                        async move {
                            let path = match tokio::task::spawn_blocking(|| {
                                rfd::FileDialog::new()
                                    .add_filter("FluxPlay profile", &["fluxplay"])
                                    .pick_file()
                            })
                            .await
                            {
                                Ok(Some(p)) => p,
                                Ok(None) => return Err("Import annulé".into()),
                                Err(e) => return Err(e.to_string()),
                            };
                            match tokio::task::spawn_blocking(move || {
                                crate::profile_io::import_profile(&path)
                            })
                            .await
                            {
                                Ok(r) => r,
                                Err(e) => Err(e.to_string()),
                            }
                        },
                        Message::ProfileImportDone,
                    );
                }
            }
            Message::ProfileExportDone(Ok(path)) => {
                self.status = format!("Profil exporté · {path} (contient identifiants)");
            }
            Message::ProfileExportDone(Err(e)) => {
                self.status = format!("Export: {e}");
            }
            Message::ProfileImportDone(Ok(src)) => {
                let id = src.id;
                let fav_path = crate::storage::profile_dir(id).join("favorites.json");
                if let Ok(raw) = std::fs::read_to_string(&fav_path) {
                    if let Ok(favs) = serde_json::from_str::<Vec<String>>(&raw) {
                        for f in favs {
                            if !self.settings.favorites.iter().any(|x| x == &f) {
                                self.settings.favorites.push(f);
                            }
                        }
                    }
                }
                if !self.sources.iter().any(|s| s.id == id) {
                    self.sources.push(src);
                    demo::strip_demo_if_real(&mut self.sources);
                }
                self.persist();
                if let Some(db) = &mut self.catalog_db {
                    let _ = db.ensure_source(id);
                    db.mark_full_sync_now(id);
                }
                self.status = "Profil importé — lecture catalogue…".into();
                // No portal reload — SQLite + images already on disk.
                return self.rebuild_bundle_from_cache_task();
            }
            Message::ProfileImportDone(Err(e)) => {
                self.status = format!("Import: {e}");
            }
            Message::BundleCacheReady { gen, parts, result } => {
                if gen != self.catalog_load_gen {
                    return Task::none();
                }
                self.loading = false;
                match result {
                    Ok(bundle) => {
                        self.apply_bundle_cache(bundle, parts);
                        if self.selected_group.is_none() {
                            self.selected_group = pick_default_live_group(&self.bundle);
                        }
                        if self.selected_vod_category.is_none() {
                            self.selected_vod_category = Some("*".into());
                        }
                        if self.selected_series_category.is_none() {
                            self.selected_series_category = Some("*".into());
                        }
                        let sync_fresh = self
                            .catalog_db
                            .as_ref()
                            .map(|db| db.is_sync_fresh())
                            .unwrap_or(false);
                        self.status = if sync_fresh {
                            format!(
                                "Offline-ready · {} live · {} VOD · {} séries (cache chaud)",
                                self.bundle.channels.len(),
                                self.bundle.vod.len(),
                                self.bundle.series.len(),
                            )
                        } else {
                            format!(
                                "DB locale · {} live · {} VOD · {} séries",
                                self.bundle.channels.len(),
                                self.bundle.vod.len(),
                                self.bundle.series.len(),
                            )
                        };
                        let mut tasks = vec![self.rebuild_browse_index()];
                        if sync_fresh && std::env::var_os("FLUXPLAY_AUTO_URL").is_none() {
                            tasks.push(self.after_catalog_ready_tasks());
                        } else {
                            tasks.push(self.prefetch_catalog_art());
                        }
                        if parts.series {
                            if let Some(fill) = self.backfill_series_credits_task() {
                                tasks.push(fill);
                            }
                        }
                        return Task::batch(tasks);
                    }
                    Err(e) => {
                        // A transient SQLite error ("database is locked") must not empty the UI.
                        tracing::warn!(error = %e, "catalog reload failed — keeping the current catalog");
                        self.status = format!("Échec lecture catalogue: {e}");
                    }
                }
            }
            Message::SourcesBatchLoaded(results) => {
                self.loading = true;
                self.status = "Ingest SQLite (chunké)…".into();
                let progress = self.jobs.ingest.clone();
                let mapped: Vec<(Uuid, Result<PlaylistBundle, String>)> = results
                    .into_iter()
                    .map(|(id, r)| (id, r.map(Arc::unwrap_or_clone)))
                    .collect();
                return Task::perform(
                    async move {
                        crate::async_jobs::run_blocking(move || {
                            crate::catalog_db::ingest_sources_blocking(mapped, progress)
                        })
                        .await
                        .unwrap_or_default()
                    },
                    Message::CatalogIngestDone,
                );
            }
            Message::CatalogIngestDone(report) => {
                self.loading = false;
                // Apply RAM bundle off UI thread — mark_all removed (ingest marks successes).
                let crate::catalog_db::IngestBatchReport {
                    ok,
                    err,
                    unchanged,
                    preserved: _,
                    source_ids,
                } = report;
                tracing::info!(
                    ok,
                    err,
                    unchanged,
                    sources = source_ids.len(),
                    "sources batch ingest done"
                );
                let ck_owned = if unchanged == ok && err == 0 && ok > 0 {
                    " · checksum OK".to_string()
                } else if unchanged > 0 {
                    format!(" · {unchanged} inchangé(s)")
                } else {
                    String::new()
                };
                self.status = format!(
                    "Ingest OK · {ok} source(s){}{} — relecture SQLite…",
                    if err > 0 {
                        format!(" ({err} échec)")
                    } else {
                        String::new()
                    },
                    ck_owned,
                );
                return self.rebuild_bundle_from_cache_task();
            }
            Message::SourceLoaded { source_id, result } => {
                self.loading = true;
                self.status = "Ingest SQLite…".into();
                let progress = self.jobs.ingest.clone();
                match result {
                    Ok(part) => {
                        let part = Arc::unwrap_or_clone(part);
                        return Task::perform(
                            async move {
                                match crate::async_jobs::run_blocking(move || {
                                    crate::catalog_db::ingest_one_blocking(
                                        source_id, part, progress,
                                    )
                                })
                                .await
                                {
                                    Ok(inner) => inner,
                                    Err(e) => Err(e),
                                }
                            },
                            move |result| Message::CatalogIngestOneDone { source_id, result },
                        );
                    }
                    Err(e) => {
                        self.loading = false;
                        self.status = format!("Échec chargement: {e}");
                    }
                }
            }
            Message::CatalogIngestOneDone { source_id, result } => {
                self.loading = false;
                match result {
                    Ok(kind) => {
                        let name = self
                            .sources
                            .iter()
                            .find(|s| s.id == source_id)
                            .map(|s| s.name.clone())
                            .unwrap_or_else(|| "source".into());
                        let tag = match kind {
                            crate::catalog_db::BundleApplyKind::Unchanged => "à jour · checksum",
                            crate::catalog_db::BundleApplyKind::Updated { preserved_meta } => {
                                if preserved_meta > 0 {
                                    "DB · meta conservée"
                                } else {
                                    "DB · mise à jour"
                                }
                            }
                        };
                        self.status = format!("{name} · {tag} — relecture SQLite…");
                        // ingest_one_blocking already marked this source_id only.
                        return self.rebuild_bundle_from_cache_task();
                    }
                    Err(e) => {
                        self.status = format!("Échec ingest: {e}");
                    }
                }
            }
            Message::BrowseIndexReady {
                gen,
                index,
                episode_flat,
            } => {
                if gen != self.jobs.browse_gen() {
                    return Task::none();
                }
                self.browse_index = index;
                self.episode_flat = episode_flat;
                if self.browse_focus >= self.browse_index.len() {
                    self.browse_focus = self.browse_index.len().saturating_sub(1);
                }
                self.finish_browse_index_rebuild();
                if self.tab == Tab::Vod {
                    self.ensure_vod_shelf_index();
                }
                return Task::batch([
                    self.snap_browse_scroll_task(),
                    self.refresh_browse_art(),
                    self.queue_visible_ages(),
                ]);
            }
            Message::MetaEnriched { series, vod } => {
                for (id, source_id, patch) in series {
                    if let Some(item) = self.bundle.series.iter_mut().find(|s| s.id == id) {
                        patch.apply_series(item);
                        if let (Some(db), Some(sid)) = (&self.catalog_db, source_id.or(item.source_id))
                        {
                            let mut saved = item.clone();
                            saved.source_id = Some(sid);
                            let _ = db.persist_series(&saved);
                        }
                    }
                }
                for (id, source_id, patch) in vod {
                    if let Some(item) = self.bundle.vod.iter_mut().find(|v| v.id == id) {
                        patch.apply_vod(item);
                        if let (Some(db), Some(sid)) = (&self.catalog_db, source_id.or(item.source_id))
                        {
                            let mut saved = item.clone();
                            saved.source_id = Some(sid);
                            let _ = db.persist_vod(&saved);
                        }
                    }
                }
                // Refresh open series detail if it was enriched.
                if let Some(detail) = &self.series_detail {
                    if let Some(updated) = self.bundle.series.iter().find(|s| s.id == detail.id) {
                        let mut d = updated.clone();
                        if d.seasons.is_empty() {
                            d.seasons = detail.seasons.clone();
                        }
                        self.series_detail = Some(d);
                    }
                }
                if let Some(detail) = &self.vod_detail {
                    if let Some(updated) = self.bundle.vod.iter().find(|v| v.id == detail.id) {
                        self.vod_detail = Some(updated.clone());
                    }
                }
                return self.prefetch_catalog_art();
            }
            Message::VodInfoBatch {
                items,
                skipped,
                throttled,
            } => {
                self.age_lookup_busy = false;
                for id in skipped {
                    self.age_asked.insert(id);
                }
                if throttled {
                    self.film_info_pause_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(20));
                }
                for item in items {
                    self.age_asked.insert(item.id.clone());
                    if let Some(existing) = self.bundle.vod.iter_mut().find(|v| v.id == item.id) {
                        merge_xtream_vod_fields(existing, &item);
                        if let (Some(db), Some(sid)) = (&self.catalog_db, existing.source_id) {
                            let mut saved = existing.clone();
                            saved.source_id = Some(sid);
                            let _ = db.persist_vod(&saved);
                        }
                    }
                    if self.vod_detail.as_ref().map(|d| d.id.as_str()) == Some(item.id.as_str()) {
                        if let Some(updated) = self.bundle.vod.iter().find(|v| v.id == item.id) {
                            self.vod_detail = Some(updated.clone());
                        }
                    }
                }
                if throttled {
                    self.status = "Portail saturé — nouvel essai des fiches dans un instant.".into();
                    return self.prefetch_catalog_art();
                }
                return Task::batch([self.prefetch_catalog_art(), self.queue_visible_ages()]);
            }
            Message::OpenActor(name) => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Task::none();
                }
                let except = self
                    .vod_detail
                    .as_ref()
                    .map(|d| crate::names::display_title(&d.name))
                    .or_else(|| {
                        self.series_detail
                            .as_ref()
                            .map(|d| crate::names::display_title(&d.name))
                    })
                    .unwrap_or_default();
                self.actor_sheet = Some(ActorSheet {
                    name: name.clone(),
                    loading: true,
                    card: None,
                });
                let query = name.clone();
                return Task::perform(
                    async move { crate::metadata::lookup_actor(&query, &except).await },
                    move |card| Message::ActorLoaded { name, card },
                );
            }
            Message::ActorLoaded { name, mut card } => {
                let matches = self
                    .actor_sheet
                    .as_ref()
                    .is_some_and(|s| s.name == name);
                if !matches {
                    return Task::none();
                }
                self.mark_in_catalog(&mut card);
                let mut urls = Vec::new();
                if let Some(photo) = card.photo.clone() {
                    urls.push(photo);
                }
                for credit in &card.credits {
                    if let Some(poster) = credit.poster.clone() {
                        urls.push(poster);
                    }
                }
                if let Some(sheet) = self.actor_sheet.as_mut() {
                    sheet.loading = false;
                    sheet.card = Some(card);
                }
                return self.prefetch_urls(urls);
            }
            Message::CloseActor => {
                self.actor_sheet = None;
            }
            Message::OpenActorTitle { title, imdb } => {
                self.actor_sheet = None;
                if let Some(hit) = self.catalog_title(&title) {
                    return match hit {
                        CatalogHit::Vod(id) => {
                            self.tab = Tab::Vod;
                            self.update(Message::OpenVodDetail(id))
                        }
                        CatalogHit::Series(id) => {
                            self.tab = Tab::Series;
                            self.update(Message::OpenSeries(id))
                        }
                    };
                }
                if let Some(id) = imdb.filter(|s| s.starts_with("tt")) {
                    return self.update(Message::OpenImdb(id));
                }
                self.status = format!("{title} — pas dans le catalogue");
            }
            Message::OpenImdb(id_or_query) => {
                let url = crate::metadata::imdb_title_url(&id_or_query);
                #[cfg(target_os = "android")]
                {
                    match crate::android_intent::open_url(&url, None) {
                        Ok(()) => self.status = format!("IMDb · {id_or_query}"),
                        Err(e) => self.status = format!("IMDb: {e}"),
                    }
                }
                #[cfg(not(target_os = "android"))]
                {
                    let _ = open::that(&url);
                    self.status = format!("IMDb · {id_or_query}");
                }
            }
            Message::OpenExternal => {
                if let Some(ch) = &self.session.channel {
                    #[cfg(target_os = "android")]
                    {
                        match crate::android_intent::open_stream_url(&ch.stream_url) {
                            Ok(()) => self.status = "Ouvert dans le lecteur système".into(),
                            Err(e) => self.status = format!("Intent: {e}"),
                        }
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        let tunnel_up = crate::wg_tunnel::tunnel_is_up()
                            || fluxplay_providers::socks_proxy().is_some();
                        if tunnel_up {
                            let allow = std::env::var("FLUXPLAY_ALLOW_CLEARNET_EXTERNAL")
                                .map(|v| v == "1")
                                .unwrap_or(false);
                            if !allow {
                                self.status = "Lecteur externe bloqué — tunnel SOCKS actif (FLUXPLAY_ALLOW_CLEARNET_EXTERNAL=1 pour forcer)".into();
                                return Task::none();
                            }
                        }
                        let _ = ch;
                        self.status = match self.session.open_external() {
                            Ok(name) => format!("Ouvert dans {name}"),
                            Err(e) => format!("Lecteur externe : {e}"),
                        };
                    }
                }
            }
            Message::SetExternalPlayer(id) => {
                self.settings.external_player = id;
                self.resync_player_options();
                self.persist();
                let label = fluxplay_player::pick_external_player(Some(
                    self.settings.external_player.as_str(),
                ))
                .map(|p| p.label())
                .unwrap_or_else(|| "aucun".into());
                self.status = format!("Lecteur système : {label} (prochain flux)");
            }
            Message::PickPlaylistFile => {
                #[cfg(target_os = "android")]
                {
                    self.saf_kind = Some(SafKind::Playlist);
                    self.status = "Choisissez une playlist M3U…".into();
                    crate::android_bridge::start_saf_open("*/*");
                    return Task::none();
                }
                #[cfg(not(target_os = "android"))]
                {
                    return Task::perform(
                        async {
                            rfd::AsyncFileDialog::new()
                                .add_filter("Playlist", &["m3u", "m3u8", "txt"])
                                .pick_file()
                                .await
                                .map(|f| f.path().display().to_string())
                        },
                        Message::PlaylistFilePicked,
                    );
                }
            }
            #[cfg(not(target_os = "android"))]
            Message::PlaylistFilePicked(Some(path)) => {
                self.form_kind = SourceKind::M3uPlus;
                self.form_endpoint = path;
                if self.form_name.is_empty() {
                    self.form_name = "Playlist locale".into();
                }
            }
            #[cfg(not(target_os = "android"))]
            Message::PlaylistFilePicked(None) => {}
            Message::ToggleFavorite(id) => {
                self.settings.toggle_favorite(&id);
                self.persist();
                self.refresh_shelf_labels();
            }
            Message::ShowShelf(shelf) => {
                self.shelf = shelf;
                self.series_detail = None;
                self.vod_detail = None;
                self.browse_scroll_y = 0.0;
                self.rebuild_cat_entries();
                return if shelf == Shelf::Catalog {
                    Task::batch([
                        self.rebuild_browse_index(),
                        self.refresh_browse_art(),
                        self.snap_browse_scroll_task(),
                    ])
                } else {
                    Task::batch([self.prefetch_shelf_art(), self.snap_browse_scroll_task()])
                };
            }
            Message::ClearHistory => {
                let Some(kind) = self.section_kind() else {
                    return Task::none();
                };
                if kind == ContentKind::Live {
                    return Task::none();
                }
                self.settings.clear_recent_kind(kind);
                self.history_art.clear();
                self.persist();
                self.refresh_shelf_labels();
                self.status = "Historique effacé".into();
            }
            Message::RemoveHistory(key) => {
                self.settings.remove_recent(&key);
                self.history_art.retain(|(url, _)| url != &key);
                self.persist();
                self.refresh_shelf_labels();
            }
            Message::ShelfHold => {}
            Message::CycleBackend => {
                self.settings.player_backend = self.settings.player_backend.cycle();
                self.resync_player_options();
                self.persist();
                self.status = format!("Backend: {}", self.settings.player_backend.label());
            }
            Message::ToggleHwdec => {
                self.settings.hwdec = !self.settings.hwdec;
                self.resync_player_options();
                self.persist();
                self.status = if self.settings.hwdec {
                    "Décodage matériel activé (prochain flux)".into()
                } else {
                    "Décodage matériel désactivé (prochain flux)".into()
                };
            }
            Message::ToggleLowLatency => {
                self.settings.low_latency = !self.settings.low_latency;
                self.resync_player_options();
                self.persist();
                self.status = if self.settings.low_latency {
                    "Faible latence activée (prochain flux)".into()
                } else {
                    "Faible latence désactivée".into()
                };
            }
            Message::TogglePrefetchNext => {
                self.settings.prefetch_next_episode = !self.settings.prefetch_next_episode;
                self.persist();
                self.status = if self.settings.prefetch_next_episode {
                    "Précharge épisode suivant activée".into()
                } else {
                    "Précharge épisode suivant désactivée".into()
                };
            }
            Message::CycleFpsGui => {
                self.settings.fps_gui = self.settings.fps_gui.cycle();
                self.refresh_display_caps(true);
                self.persist();
                self.status = format!(
                    "FPS GUI : {} → {} fps",
                    self.settings.fps_gui.label(),
                    self.display_caps.gui_hz
                );
            }
            Message::CycleFpsVideo => {
                self.settings.fps_video = self.settings.fps_video.cycle();
                self.refresh_display_caps(true);
                self.persist();
                self.status = format!(
                    "FPS vidéo : {} → {} fps",
                    self.settings.fps_video.label(),
                    self.display_caps.video_hz
                );
            }
            Message::RefreshDisplayCaps => {
                self.refresh_display_caps(true);
                self.status = self.display_caps.summary_line();
            }
            Message::CycleGpu => {
                let devices = self.display_caps.probe.gpu_topology.devices.clone();
                if devices.is_empty() {
                    self.status = "Aucun GPU détecté".into();
                } else {
                    let cur = self.settings.gpu_choice.clone();
                    let idx = devices.iter().position(|d| d.name == cur).unwrap_or(usize::MAX);
                    let next = &devices[(idx.wrapping_add(1)) % devices.len()];
                    self.settings.gpu_choice = next.name.clone();
                    export_gpu_env(&self.settings.gpu_choice);
                    self.resync_player_options();
                    self.persist();
                    self.status = format!(
                        "GPU : {}. Relance l'application pour que l'affichage change aussi.",
                        next.label()
                    );
                }
            }
            Message::CycleVideoQuality => {
                self.apply_video_quality(self.settings.video_quality.cycle());
            }
            Message::SetVideoQuality(q) => {
                self.apply_video_quality(q);
            }
            Message::CycleHdrMode => {
                self.settings.hdr_mode = self.settings.hdr_mode.cycle();
                self.resync_player_options();
                self.persist();
                #[cfg(target_os = "android")]
                if self.session.channel.is_some() {
                    let caps = crate::android_bridge::poll_android_device_caps()
                        .unwrap_or_default();
                    let hdr_plus = caps.has_hdr10_plus() || caps.has_dolby_vision();
                    let color_mode = self.settings.hdr_mode.android_color_mode(
                        caps.hdr_capable || caps.mediacodec_hdr,
                        hdr_plus,
                    );
                    crate::android_bridge::set_display_color_mode(color_mode);
                }
                self.status = format!("Mode HDR/couleur : {}", self.settings.hdr_mode.label());
            }
            Message::CycleDisplayPanel => {
                self.settings.display_panel = self.settings.display_panel.cycle();
                self.persist();
                if self.session.channel.is_some() {
                    self.apply_saved_video_defaults_for_current_play();
                }
                self.status = format!("Profil écran : {}", self.settings.display_panel.label());
            }
            Message::CycleAndroidPresentPref => {
                self.settings.android_present = self.settings.android_present.cycle();
                self.resync_player_options();
                self.persist();
                self.status = format!(
                    "Présent Android : {} (prochain flux)",
                    self.settings.android_present.label()
                );
            }
            Message::ToggleTonemapHdr => {
                self.settings.tonemap_hdr = !self.settings.tonemap_hdr;
                self.resync_player_options();
                self.persist();
                if self.session.channel.is_some() {
                    self.apply_saved_video_defaults_for_current_play();
                }
                self.status = if self.settings.tonemap_hdr {
                    "Tonemap HDR→SDR actif".into()
                } else {
                    "Tonemap HDR→SDR inactif".into()
                };
            }
            Message::ToggleRememberPosition => {
                self.settings.remember_position = !self.settings.remember_position;
                self.persist();
                self.status = if self.settings.remember_position {
                    "Reprise position activée".into()
                } else {
                    "Reprise position désactivée".into()
                };
            }
            Message::CycleDefaultAspect => {
                self.settings.aspect = self.settings.aspect.cycle();
                self.persist();
                self.status = format!("Format par défaut : {}", self.settings.aspect.label());
            }
            Message::CycleDefaultDeinterlace => {
                self.settings.deinterlace = self.settings.deinterlace.cycle();
                self.persist();
                self.status = format!(
                    "Désentrelacement défaut : {}",
                    self.settings.deinterlace.label()
                );
            }
            Message::CycleDefaultUpscale => {
                self.settings.upscale = self.settings.upscale.cycle();
                self.persist();
                self.status = format!("Upscale par défaut : {}", self.settings.upscale.label());
            }
            Message::ToggleDefaultNightMode => {
                self.settings.night_mode = !self.settings.night_mode;
                self.persist();
                self.status = if self.settings.night_mode {
                    "Mode nuit par défaut activé".into()
                } else {
                    "Mode nuit par défaut désactivé".into()
                };
            }
            Message::PrefetchEvent { url, event } => {
                use crate::downloads::DownloadEvent;
                if let DownloadEvent::Finished(result) = event {
                    if self.prefetch_job.as_ref().is_some_and(|(u, _)| *u == url) {
                        self.prefetch_job = None;
                    }
                    match result {
                        Ok(path) => {
                            tracing::info!("next episode preloaded");
                            self.prefetched = Some((url, path));
                            self.status = "Épisode suivant préchargé".to_string();
                        }
                        Err(e) => tracing::debug!(error = %e, "preload skipped"),
                    }
                }
            }
            Message::DiagnosePortals => {
                self.status = "Diagnostic portails…".into();
                let jobs: Vec<_> = self
                    .sources
                    .iter()
                    .filter(|s| s.enabled && s.kind == SourceKind::Xtream)
                    .filter_map(|s| {
                        let u = s.username.clone()?;
                        let p = s.password.clone()?;
                        Some((s.name.clone(), s.endpoint.clone(), u, p))
                    })
                    .collect();
                return Task::perform(
                    async move {
                        let mut lines = Vec::new();
                        for (name, endpoint, user, pass) in jobs {
                            match fluxplay_providers::check_xtream_portal(&endpoint, &user, &pass)
                                .await
                            {
                                Ok(h) => lines.push(format!(
                                    "{name}: {}",
                                    fluxplay_providers::format_health(&h)
                                )),
                                Err(e) => lines.push(format!("{name}: erreur {e}")),
                            }
                        }
                        if lines.is_empty() {
                            "Aucun portail Xtream activé".into()
                        } else {
                            lines.join(" ‖ ")
                        }
                    },
                    Message::DiagnoseDone,
                );
            }
            Message::DiagnoseDone(report) => {
                tracing::info!(%report, "portal diagnose");
                self.status = if report.chars().count() > 320 {
                    format!("{}…", report.chars().take(317).collect::<String>())
                } else {
                    report
                };
            }
            Message::EpgFetched(source_id, programmes) => {
                if !programmes.is_empty() {
                    let n = programmes.len();
                    fluxplay_providers::merge_epg(&mut self.bundle.epg, programmes.clone());
                    if let Some(db) = &self.catalog_db {
                        if let Err(e) = db.merge_epg(source_id, &programmes) {
                            tracing::warn!(error = %e, "persist epg failed");
                        }
                    }
                    if self.tab == Tab::Epg {
                        self.status = format!("Guide TV · {n} programmes");
                    }
                    self.rebuild_epg_now();
                }
            }
            Message::VodCategoryLoaded {
                category_id,
                sources,
                result,
            } => {
                // The user may have moved on: merge the items, but only report on
                // (and redraw) the category still on screen.
                let current = self.selected_vod_category.as_deref().is_some_and(|id| {
                    crate::names::category_shelf(id).0 == category_id.as_str()
                });
                match result {
                    Ok(items) => {
                        self.bundle.vod.retain(|v| {
                            v.category_id.as_deref() != Some(&category_id)
                                || !v.source_id.is_some_and(|s| sources.contains(&s))
                        });
                        let n = items.len();
                        self.bundle.vod.extend(items);
                        if current {
                            self.status = format!("VOD catégorie · {n} films");
                            return Task::batch([
                                self.rebuild_browse_index(),
                                self.refresh_browse_art(),
                            ]);
                        }
                    }
                    Err(e) if current => self.status = format!("VOD: {e}"),
                    Err(_) => {}
                }
            }
            Message::SeriesCategoryLoaded {
                category_id,
                sources,
                result,
            } => {
                let current = self.selected_series_category.as_deref().is_some_and(|id| {
                    crate::names::category_shelf(id).0 == category_id.as_str()
                });
                match result {
                    Ok(items) => {
                        self.bundle.series.retain(|s| {
                            s.category_id.as_deref() != Some(&category_id)
                                || !s.source_id.is_some_and(|id| sources.contains(&id))
                        });
                        let n = items.len();
                        self.bundle.series.extend(items);
                        if current {
                            self.status = format!("Séries catégorie · {n} titres");
                            return Task::batch([
                                self.rebuild_browse_index(),
                                self.refresh_browse_art(),
                            ]);
                        }
                    }
                    Err(e) if current => self.status = format!("Séries: {e}"),
                    Err(_) => {}
                }
            }
            Message::OpenSeries(id) => {
                self.pressed_mosaic = None;
                if self.consume_browse_drag_suppress() {
                    return Task::none();
                }
                self.actor_sheet = None;
                self.vod_detail = None;
                self.detail_meta_loading = false;
                self.browse_scroll_y = 0.0;
                self.browse_view_h = 0.0;
                self.selected_season = None;
                self.season_menu_open = false;
                self.resume_season_pending = true;
                self.cast_photos.clear();
                self.cast_photo_asked.clear();
                if let Some(existing) = self.bundle.series.iter().find(|s| s.id == id) {
                    let mut detail = existing.clone();
                    if detail.seasons.is_empty() {
                        if let Some(raw) = self
                            .catalog_db
                            .as_ref()
                            .and_then(|db| db.payload_by_id("series", &id))
                        {
                            if let Ok(full) = serde_json::from_str::<SeriesItem>(&raw) {
                                detail.seasons = full.seasons;
                                if detail.plot.is_none() {
                                    detail.plot = full.plot;
                                }
                            }
                        }
                    }
                    self.series_detail = Some(detail);
                    self.sync_selected_season();
                    self.focus_resume_season();
                    self.rebuild_episode_flat();
                }
                self.status = "Chargement épisodes…".into();
                let translate = self.translate_open_detail_task();
                let wanted = self
                    .bundle
                    .series
                    .iter()
                    .find(|s| s.id == id)
                    .and_then(|s| s.source_id);
                let src = self
                    .sources
                    .iter()
                    .find(|s| s.enabled && wanted.map(|w| w == s.id).unwrap_or(false))
                    .cloned()
                    .or_else(|| {
                        self.sources
                            .iter()
                            .find(|s| {
                                s.enabled
                                    && (s.kind == SourceKind::Xtream
                                        || fluxplay_providers::parse_xtream_get_php(&s.endpoint)
                                            .is_some())
                            })
                            .cloned()
                    });
                let Some(src) = src else {
                    self.status = "Aucune source Xtream pour les séries".into();
                    if self.series_detail.is_some() {
                        return Task::batch([
                            self.request_cast_photos(),
                            self.french_plot_task(),
                            self.enrich_open_detail_task(true),
                            translate,
                        ]);
                    }
                    return translate;
                };
                return Task::batch([
                    self.request_cast_photos(),
                    self.french_plot_task(),
                    translate,
                    Task::perform(
                        async move {
                            fluxplay_providers::load_xtream_series_info(&src, &id)
                                .await
                                .map_err(|e| e.to_string())
                        },
                        Message::SeriesDetailLoaded,
                    ),
                ]);
            }
            Message::SeriesDetailLoaded(result) => match result {
                Ok(item) => {
                    let eps: usize = item.seasons.iter().map(|s| s.episodes.len()).sum();
                    let showing = self
                        .series_detail
                        .as_ref()
                        .is_some_and(|d| d.id == item.id);
                    let quiet = !showing
                        && self
                            .settings
                            .episode_alerts
                            .iter()
                            .any(|a| a.series_id == item.id);
                    if !quiet {
                        self.status = format!(
                            "{} · {} saisons · {eps} épisodes",
                            crate::names::display_title(&item.name),
                            item.seasons.len()
                        );
                    }
                    if !item.seasons.is_empty() {
                        if let (Some(db), Some(sid)) = (&self.catalog_db, item.source_id) {
                            let mut saved = item.clone();
                            saved.source_id = Some(sid);
                            let _ = db.persist_series(&saved);
                        }
                    }
                    let in_catalog =
                        match self.bundle.series.iter_mut().find(|s| s.id == item.id) {
                            Some(existing) => {
                                merge_xtream_series_fields(existing, &item);
                                true
                            }
                            None => false,
                        };
                    // Late answer for a page the viewer already left (or closed): catalog only.
                    self.alert_checked.insert(item.id.clone());
                    self.consider_episode_alert(&item);
                    let open = self.series_detail.as_ref().map(|d| d.id.as_str());
                    if open.map(|id| id != item.id).unwrap_or(in_catalog) {
                        return Task::none();
                    }
                    match self.series_detail.as_mut() {
                        Some(detail) => merge_xtream_series_fields(detail, &item),
                        None => self.series_detail = Some(item),
                    }
                    self.browse_scroll_y = 0.0;
                    self.sync_selected_season();
                    self.focus_resume_season();
                    self.rebuild_episode_flat();
                    return Task::batch([
                        self.prefetch_episode_stills(),
                        self.request_cast_photos(),
                        self.french_plot_task(),
                        self.enrich_open_detail_task(true),
                        self.enrich_episodes_task(),
                        self.translate_open_detail_task(),
                    ]);
                }
                Err(e) => {
                    self.status = if e.contains("429") {
                        "Portail saturé — les épisodes arrivent dès qu'il répond. Réouvre la série si la page reste vide.".into()
                    } else {
                        format!("Série: {e}")
                    };
                }
            },
            Message::SeriesCreditsReady(groups) => {
                use std::collections::HashMap;
                let mut by_source: HashMap<Uuid, HashMap<String, (Option<String>, Option<String>, Option<String>)>> =
                    HashMap::new();
                for (source_id, rows) in groups {
                    let entry = by_source.entry(source_id).or_default();
                    for (id, actors, director, runtime) in rows {
                        entry.insert(id, (actors, director, runtime));
                    }
                }
                for series in &mut self.bundle.series {
                    let Some(source_id) = series.source_id else {
                        continue;
                    };
                    let Some(row) = by_source.get(&source_id).and_then(|m| m.get(&series.id)) else {
                        continue;
                    };
                    if series.actors.as_ref().is_some_and(|s| !s.trim().is_empty()) {
                        continue;
                    }
                    series.actors = row.0.clone();
                    if series.director.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true) {
                        series.director = row.1.clone();
                    }
                    if series.runtime.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true) {
                        series.runtime = row.2.clone();
                    }
                }
                if let Some(detail) = &mut self.series_detail {
                    if let Some(source_id) = detail.source_id {
                        if let Some(row) = by_source.get(&source_id).and_then(|m| m.get(&detail.id)) {
                            if detail.actors.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true) {
                                detail.actors = row.0.clone();
                                if detail.director.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true)
                                {
                                    detail.director = row.1.clone();
                                }
                                if detail.runtime.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true)
                                {
                                    detail.runtime = row.2.clone();
                                }
                            }
                        }
                    }
                }
                if self.series_detail.as_ref().is_some_and(|d| {
                    d.actors.as_ref().is_some_and(|s| !s.trim().is_empty())
                }) {
                    return self.request_cast_photos();
                }
            }
            Message::SeriesEpisodesEnriched(item) => {
                if let Some(existing) = self.bundle.series.iter_mut().find(|s| s.id == item.id) {
                    existing.seasons = item.seasons.clone();
                }
                let open = self.series_detail.as_ref().map(|d| d.id.as_str()) == Some(item.id.as_str());
                if open {
                    if let Some(detail) = &mut self.series_detail {
                        detail.seasons = item.seasons.clone();
                    }
                    self.rebuild_episode_flat();
                    self.focus_resume_season();
                    self.rebuild_episode_flat();
                }
                if let (Some(db), Some(sid)) = (&self.catalog_db, item.source_id) {
                    let mut saved = item;
                    saved.source_id = Some(sid);
                    let _ = db.persist_series(&saved);
                }
                if open {
                    return Task::batch([
                        self.prefetch_episode_stills(),
                        self.translate_open_detail_task(),
                    ]);
                }
            },
            Message::CloseSeriesDetail => {
                self.series_detail = None;
                self.resume_season_pending = false;
                self.selected_season = None;
                self.season_menu_open = false;
                self.episode_hover = None;
                self.cast_photos.clear();
                self.cast_photo_asked.clear();
                self.actor_sheet = None;
                self.detail_meta_loading = false;
                self.episode_flat.clear();
                self.browse_scroll_y = 0.0;
                return Task::batch([
                    self.rebuild_browse_index(),
                    self.refresh_browse_art(),
                ]);
            }
            Message::OpenVodDetail(id) => {
                self.pressed_mosaic = None;
                if self.consume_browse_drag_suppress() {
                    return Task::none();
                }
                self.actor_sheet = None;
                self.series_detail = None;
                self.selected_season = None;
                self.season_menu_open = false;
                self.cast_photos.clear();
                self.cast_photo_asked.clear();
                self.browse_scroll_y = 0.0;
                self.browse_view_h = 0.0;
                let Some(item) = self.bundle.vod.iter().find(|v| v.id == id).cloned() else {
                    self.status = "Film introuvable".into();
                    return Task::none();
                };
                self.detail_meta_loading = true;
                self.status = format!("{} — fiche", crate::names::display_title(&item.name));
                let vod_id = item.id.clone();
                let wanted = item.source_id;
                self.vod_detail = Some(item);
                let src = self
                    .sources
                    .iter()
                    .find(|s| s.enabled && wanted.map(|w| w == s.id).unwrap_or(false))
                    .cloned()
                    .or_else(|| {
                        self.sources
                            .iter()
                            .find(|s| {
                                s.enabled
                                    && (s.kind == SourceKind::Xtream
                                        || fluxplay_providers::parse_xtream_get_php(&s.endpoint)
                                            .is_some())
                            })
                            .cloned()
                    });
                let xtream = if let Some(src) = src {
                    Task::perform(
                        async move {
                            fluxplay_providers::load_xtream_vod_info_now(&src, &vod_id)
                                .await
                                .map_err(|e| e.to_string())
                        },
                        Message::VodDetailLoaded,
                    )
                } else {
                    self.detail_meta_loading = false;
                    Task::none()
                };
                return Task::batch([
                    self.prefetch_visible_art(),
                    self.french_plot_task(),
                    self.request_cast_photos(),
                    self.enrich_open_detail_task(false),
                    xtream,
                ]);
            }
            Message::VodDetailLoaded(result) => match result {
                Ok(item) => {
                    if let Some(existing) = self.bundle.vod.iter_mut().find(|v| v.id == item.id) {
                        merge_xtream_vod_fields(existing, &item);
                        if let (Some(db), Some(sid)) = (&self.catalog_db, existing.source_id) {
                            let mut saved = existing.clone();
                            saved.source_id = Some(sid);
                            let _ = db.persist_vod(&saved);
                        }
                    }
                    // Merge into the open page: the panel answer lacks fields the catalog /
                    // OMDb already filled (writer, awards, …) and may carry empty ones.
                    if let Some(detail) = self.vod_detail.as_mut().filter(|d| d.id == item.id) {
                        merge_xtream_vod_fields(detail, &item);
                        let name = crate::names::display_title(&detail.name);
                        self.status = format!("{name} · fiche");
                    }
                    self.detail_meta_loading = false;
                    return Task::batch([
                        self.prefetch_visible_art(),
                        self.french_plot_task(),
                        self.request_cast_photos(),
                    ]);
                }
                Err(e) => {
                    tracing::warn!(%e, "vod_info failed");
                    self.detail_meta_loading = false;
                    self.status = format!("Fiche panel: {e}");
                }
            },
            Message::FrenchPlotReady { series, id, plot } => {
                let Some(plot) = plot.filter(|p| {
                    crate::translate::guess_lang(p) != Some("en") && p.trim().len() >= 80
                }) else {
                    return Task::none();
                };
                let already_fr = |current: &Option<String>| {
                    current
                        .as_deref()
                        .is_some_and(|p| crate::translate::guess_lang(p) == Some("fr"))
                };
                if series {
                    let Some(detail) = self.series_detail.as_mut().filter(|d| d.id == id) else {
                        return Task::none();
                    };
                    if already_fr(&detail.plot) {
                        return Task::none();
                    }
                    detail.plot = Some(plot.clone());
                    let mut saved = detail.clone();
                    if let Some(existing) = self.bundle.series.iter_mut().find(|s| s.id == id) {
                        existing.plot = Some(plot);
                    }
                    if let (Some(db), Some(sid)) = (&self.catalog_db, saved.source_id) {
                        saved.source_id = Some(sid);
                        let _ = db.persist_series(&saved);
                    }
                } else {
                    let Some(detail) = self.vod_detail.as_mut().filter(|d| d.id == id) else {
                        return Task::none();
                    };
                    if already_fr(&detail.plot) {
                        return Task::none();
                    }
                    detail.plot = Some(plot.clone());
                    let mut saved = detail.clone();
                    if let Some(existing) = self.bundle.vod.iter_mut().find(|v| v.id == id) {
                        existing.plot = Some(plot);
                    }
                    if let (Some(db), Some(sid)) = (&self.catalog_db, saved.source_id) {
                        saved.source_id = Some(sid);
                        let _ = db.persist_vod(&saved);
                    }
                }
                let name = if series {
                    self.series_detail.as_ref().map(|d| d.name.clone())
                } else {
                    self.vod_detail.as_ref().map(|d| d.name.clone())
                };
                if let Some(name) = name {
                    self.status = format!("{} · fiche", crate::names::display_title(&name));
                }
            }
            Message::CloseVodDetail => {
                self.vod_detail = None;
                self.cast_photos.clear();
                self.cast_photo_asked.clear();
                self.actor_sheet = None;
                self.detail_meta_loading = false;
                self.browse_scroll_y = 0.0;
                return Task::batch([
                    self.rebuild_browse_index(),
                    self.refresh_browse_art(),
                ]);
            }
            Message::DetailMetaLoaded {
                is_series,
                id,
                patch,
            } => {
                self.detail_meta_loading = false;
                let Some(patch) = patch else {
                    // Negative-cache the miss so we don't hammer OMDb on every open.
                    if let Some(db) = &self.catalog_db {
                        let (name, kind) = if is_series {
                            (
                                self.series_detail
                                    .as_ref()
                                    .filter(|d| d.id == id)
                                    .map(|d| d.name.clone()),
                                "series",
                            )
                        } else {
                            (
                                self.vod_detail
                                    .as_ref()
                                    .filter(|d| d.id == id)
                                    .map(|d| d.name.clone()),
                                "movie",
                            )
                        };
                        if let Some(name) = name {
                            let q = crate::metadata::parse_title_query(&name);
                            db.meta_cache_put(&q.cache_key(kind), kind, &q, None);
                        }
                    }
                    return Task::none();
                };
                if let Some(db) = &self.catalog_db {
                    let name = if is_series {
                        self.series_detail
                            .as_ref()
                            .filter(|d| d.id == id)
                            .map(|d| d.name.clone())
                    } else {
                        self.vod_detail
                            .as_ref()
                            .filter(|d| d.id == id)
                            .map(|d| d.name.clone())
                    };
                    if let Some(name) = name {
                        let kind = if is_series { "series" } else { "movie" };
                        let q = crate::metadata::parse_title_query(&name);
                        db.meta_cache_put(&q.cache_key(kind), kind, &q, Some(&patch));
                    }
                }
                if is_series {
                    if let Some(item) = self.bundle.series.iter_mut().find(|s| s.id == id) {
                        patch.apply_series(item);
                        if let (Some(db), Some(sid)) = (&self.catalog_db, item.source_id) {
                            let mut saved = item.clone();
                            saved.source_id = Some(sid);
                            let _ = db.persist_series(&saved);
                        }
                    }
                    if let Some(detail) = &mut self.series_detail {
                        if detail.id == id {
                            let seasons = detail.seasons.clone();
                            patch.apply_series(detail);
                            if detail.seasons.is_empty() {
                                detail.seasons = seasons;
                            }
                            self.status =
                                format!("{} · fiche IMDb", crate::names::display_title(&detail.name));
                        }
                    }
                } else {
                    if let Some(item) = self.bundle.vod.iter_mut().find(|v| v.id == id) {
                        patch.apply_vod(item);
                        if let (Some(db), Some(sid)) = (&self.catalog_db, item.source_id) {
                            let mut saved = item.clone();
                            saved.source_id = Some(sid);
                            let _ = db.persist_vod(&saved);
                        }
                    }
                    if let Some(detail) = &mut self.vod_detail {
                        if detail.id == id {
                            patch.apply_vod(detail);
                            self.status =
                                format!("{} · fiche IMDb", crate::names::display_title(&detail.name));
                        }
                    }
                }
                return Task::batch([
                    self.request_cast_photos(),
                    self.refresh_browse_art(),
                    self.translate_open_detail_task(),
                ]);
            }
            Message::ImageLoaded(Ok((url, source_id, bytes))) => {
                if self.browse_parked {
                    // The browser is hidden, but the bytes have to be here when it
                    // comes back. Dropping them left the tiles empty.
                    self.jobs.images.tick();
                    let _ = source_id;
                    self.images.insert_ui_bytes(url, bytes);
                    return Task::none();
                }
                self.jobs.images.tick();
                self.pending_images.push((url, source_id, bytes));
                // Keep fetching the rows on screen. A hard fling used to stop the
                // queue after the first batch, so posters stayed blank until settle.
                if self.browse_flinging() {
                    let more = self.prefetch_visible_art();
                    if self.pending_images.len() >= 6 {
                        return Task::batch([self.flush_pending_images(), more]);
                    }
                    return more;
                }
                if self.pending_images.len() >= 10 {
                    return Task::batch([
                        self.flush_pending_images(),
                        self.prefetch_visible_art(),
                        self.continue_art_warm(),
                    ]);
                }
                if !self.image_flush_armed {
                    self.image_flush_armed = true;
                    return Task::perform(
                        async {
                            tokio::time::sleep(std::time::Duration::from_millis(18)).await;
                        },
                        |_| Message::FlushPendingImages,
                    );
                }
            }
            Message::ImageLoaded(Err((url, err))) => {
                if self.browse_parked {
                    self.jobs.images.tick();
                    self.images.mark_soft_failed(url);
                    return Task::none();
                }
                self.jobs.images.tick();
                tracing::debug!(%url, %err, "image fetch failed");
                // Permanent: HTTP 4xx (except 408/429 already retried), garbage body, too large.
                let permanent = (err.starts_with("HTTP 4")
                    && !err.contains("HTTP 408")
                    && !err.contains("HTTP 429"))
                    || err.contains("html body")
                    || err.contains("image too large")
                    || err.contains("invalid image");
                if permanent {
                    self.images.mark_failed(url);
                } else {
                    // Soft-fail + host circuit — stops retry storms on dead CDNs / SOCKS.
                    self.images.mark_soft_failed(url);
                }
                return Task::batch([self.prefetch_visible_art(), self.continue_art_warm()]);
            }
            Message::FlushPendingImages => {
                self.image_flush_armed = false;
                if self.browse_flinging() {
                    // Keep buffering; settle path will flush.
                    return Task::none();
                }
                return Task::batch([
                    self.flush_pending_images(),
                    self.prefetch_visible_art(),
                    self.continue_art_warm(),
                ]);
            }
        }
        Task::none()
    }

    fn enrich_episodes_task(&self) -> Task<Message> {
        // Episode text stays the portal copy. TVMaze summaries are English and
        // were then machine-translated.
        Task::none()
    }

    fn translation_on(&self) -> bool {
        self.settings.translate_meta && !self.settings.pref_lang.is_empty()
    }

    /// Text in the viewer language when a translation is known.
    fn tr<'s>(&'s self, text: &'s str) -> &'s str {
        if !self.translation_on() {
            return text;
        }
        self.translations
            .get(text.trim())
            .map(String::as_str)
            .unwrap_or(text)
    }

    /// Translation state of a displayed text: `Some(true)` translated, `Some(false)` pending.
    fn tr_state(&self, text: &str) -> Option<bool> {
        if !self.translation_on() {
            return None;
        }
        let key = text.trim();
        if self.translating.contains(key) {
            return Some(false);
        }
        self.translations
            .get(key)
            .filter(|t| t.as_str() != key)
            .map(|_| true)
    }

    /// Synopsis / genre / episode plots stay in the language the portal sent.
    fn translate_open_detail_task(&mut self) -> Task<Message> {
        Task::none()
    }

    fn request_translations(&mut self, texts: Vec<String>) -> Task<Message> {
        if !self.translation_on() {
            return Task::none();
        }
        let lang = self.settings.pref_lang.clone();
        let mut todo = Vec::new();
        for t in texts {
            let t = t.trim().to_string();
            if t.is_empty() || self.translations.contains_key(&t) || self.translating.contains(&t) {
                continue;
            }
            if crate::translate::guess_lang(&t) == Some(lang.as_str()) {
                self.translations.insert(t.clone(), t);
                continue;
            }
            let cached = self
                .catalog_db
                .as_ref()
                .and_then(|db| db.translation_get(&crate::translate::cache_key(&lang, &t)));
            if let Some(hit) = cached {
                self.translations.insert(t, hit);
                continue;
            }
            self.translating.insert(t.clone());
            todo.push(t);
        }
        if todo.is_empty() {
            return Task::none();
        }
        Task::perform(
            async move {
                let db = crate::catalog_db::CatalogDb::open(&[]);
                let (long, short): (Vec<String>, Vec<String>) =
                    todo.iter().cloned().partition(|t| t.len() > 600);
                let mut done: Vec<(String, String)> = Vec::new();
                for t in long {
                    if let Some(tr) = crate::translate::translate(&t, &lang).await {
                        if let Some(db) = &db {
                            db.translation_put(
                                &crate::translate::cache_key(&lang, &t),
                                &tr.text,
                                tr.source_lang.as_deref(),
                            );
                        }
                        done.push((t, tr.text));
                    }
                }
                for (src, dst) in crate::translate::translate_many(&short, &lang).await {
                    if let Some(db) = &db {
                        db.translation_put(&crate::translate::cache_key(&lang, &src), &dst, None);
                    }
                    done.push((src, dst));
                }
                // Failures map to themselves for this session (no retry storm).
                for t in todo {
                    if !done.iter().any(|(s, _)| *s == t) {
                        done.push((t.clone(), t));
                    }
                }
                Message::TranslationsReady(lang, done)
            },
            |m| m,
        )
    }

    /// The series list already carries cast. Write it once into SQLite and the
    /// open catalog so a fiche does not wait on `get_series_info` or OMDb.
    fn backfill_series_credits_task(&mut self) -> Option<Task<Message>> {
        if self.series_credits_filled || self.bundle.series.len() < 50 {
            return None;
        }
        let have = self
            .bundle
            .series
            .iter()
            .filter(|s| s.actors.as_ref().is_some_and(|a| !a.trim().is_empty()))
            .count();
        if have.saturating_mul(20) >= self.bundle.series.len() {
            self.series_credits_filled = true;
            return None;
        }
        self.series_credits_filled = true;
        let sources: Vec<MediaSource> = self
            .sources
            .iter()
            .filter(|s| s.enabled)
            .cloned()
            .collect();
        let ids: Vec<Uuid> = sources.iter().map(|s| s.id).collect();
        Some(Task::perform(
            async move {
                crate::async_jobs::run_blocking(move || {
                    let db = crate::catalog_db::CatalogDb::open(&ids);
                    let mut out = Vec::new();
                    for src in sources {
                        let credits = fluxplay_providers::cached_series_credits(&src);
                        if credits.is_empty() {
                            continue;
                        }
                        if let Some(db) = &db {
                            match db.patch_series_credits(src.id, &credits) {
                                Ok(n) => tracing::info!(n, source_id = %src.id, "series credits stored"),
                                Err(e) => tracing::warn!(error = %e, "series credits store failed"),
                            }
                        }
                        out.push((src.id, credits));
                    }
                    out
                })
                .await
                .unwrap_or_default()
            },
            Message::SeriesCreditsReady,
        ))
    }

    fn enrich_open_detail_task(&mut self, is_series: bool) -> Task<Message> {
        if !crate::metadata::tmdb_configured() {
            return Task::none();
        }
        let (id, name, imdb) = if is_series {
            let Some(detail) = &self.series_detail else {
                return Task::none();
            };
            (detail.id.clone(), detail.name.clone(), detail.imdb_id.clone())
        } else {
            let Some(detail) = &self.vod_detail else {
                return Task::none();
            };
            (detail.id.clone(), detail.name.clone(), detail.imdb_id.clone())
        };
        let kind = if is_series { "series" } else { "movie" };
        Task::perform(
            async move {
                let patch = crate::metadata::tmdb_fiche(&name, kind, imdb.as_deref()).await;
                Message::DetailMetaLoaded {
                    is_series,
                    id,
                    patch,
                }
            },
            |m| m,
        )
    }

    /// After catalog load/reload: visible art only. English metadata lookups
    /// used to rewrite synopses that the portal had already sent.
    fn after_catalog_ready_tasks(&mut self) -> Task<Message> {
        self.xtream_vod_enrich_started = false;
        Task::batch([
            self.prefetch_catalog_art(),
            self.fetch_epg_for_visible_task(),
        ])
    }

    fn enrich_metadata_task(&self) -> Task<Message> {
        let mut series_cached = Vec::new();
        let mut series_fetch = Vec::new();
        for s in self
            .bundle
            .series
            .iter()
            .filter(|s| crate::metadata::needs_series_enrich(s))
            .take(12)
        {
            let q = crate::metadata::parse_title_query(&s.name);
            let key = q.cache_key("series");
            if let Some(db) = &self.catalog_db {
                if let Some((miss, patch)) = db.meta_cache_get(&key) {
                    if !miss {
                        series_cached.push((s.id.clone(), s.source_id, patch));
                    }
                    continue;
                }
            }
            series_fetch.push((s.id.clone(), s.source_id, s.name.clone()));
        }
        let mut vod_cached = Vec::new();
        let mut vod_fetch = Vec::new();
        for v in self
            .bundle
            .vod
            .iter()
            .filter(|v| crate::metadata::needs_vod_enrich(v))
            .take(24)
        {
            let q = crate::metadata::parse_title_query(&v.name);
            let key = q.cache_key("movie");
            if let Some(db) = &self.catalog_db {
                if let Some((miss, patch)) = db.meta_cache_get(&key) {
                    if !miss {
                        vod_cached.push((v.id.clone(), v.source_id, patch));
                    }
                    continue;
                }
            }
            vod_fetch.push((v.id.clone(), v.source_id, v.name.clone()));
        }

        if series_fetch.is_empty() && vod_fetch.is_empty() {
            if series_cached.is_empty() && vod_cached.is_empty() {
                return Task::none();
            }
            return Task::done(Message::MetaEnriched {
                series: series_cached,
                vod: vod_cached,
            });
        }

        let meta_parallel = self.display_caps.tuning.meta_parallel;
        Task::perform(
            async move {
                let mut series_out = series_cached;
                let mut vod_out = vod_cached;
                let db = crate::catalog_db::CatalogDb::open(&[]);
                let mut set = tokio::task::JoinSet::new();
                // OMDb / TVMaze / iTunes share a global gate — host caps clamp this.

                let mut si = 0usize;
                let mut vi = 0usize;
                // Prefer VOD (movies) first — user-facing catalog art/synopsis.
                while si < series_fetch.len() || vi < vod_fetch.len() || !set.is_empty() {
                    while set.len() < meta_parallel
                        && (si < series_fetch.len() || vi < vod_fetch.len())
                    {
                        if vi < vod_fetch.len() {
                            let (id, sid, name) = vod_fetch[vi].clone();
                            vi += 1;
                            set.spawn(async move {
                                let patch = crate::metadata::enrich_vod(&name).await;
                                (false, id, sid, name, patch)
                            });
                        } else if si < series_fetch.len() {
                            let (id, sid, name) = series_fetch[si].clone();
                            si += 1;
                            set.spawn(async move {
                                let patch = crate::metadata::enrich_series(&name).await;
                                (true, id, sid, name, patch)
                            });
                        }
                    }
                    if let Some(Ok((is_series, id, sid, name, patch))) = set.join_next().await {
                        let kind = if is_series { "series" } else { "movie" };
                        let q = crate::metadata::parse_title_query(&name);
                        let key = q.cache_key(kind);
                        if let Some(db) = &db {
                            db.meta_cache_put(&key, kind, &q, patch.as_ref());
                        }
                        if let Some(patch) = patch {
                            if is_series {
                                series_out.push((id, sid, patch));
                            } else {
                                vod_out.push((id, sid, patch));
                            }
                        }
                    }
                }
                Message::MetaEnriched {
                    series: series_out,
                    vod: vod_out,
                }
            },
            |m| m,
        )
    }

    fn queue_visible_film_info(&mut self) -> Task<Message> {
        // One film fiche at a time, when the viewer opens it. A background
        // batch of get_vod_info was tripping the portal limit and leaving
        // the open page empty.
        Task::none()
    }

    /// Other copies of the same title already sit in Action, Comédie, Horreur…
    /// A 4K or VOST copy can show that shelf without another request.
    fn ensure_vod_shelf_index(&mut self) {
        if !self.vod_shelf_by_title.is_empty() {
            return;
        }
        let mut captions: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for cat in &self.bundle.categories {
            if cat.content != ContentKind::Vod {
                continue;
            }
            if let Some(caption) = shelf_caption(&cat.name) {
                captions.insert(cat.id.clone(), caption);
            }
        }
        let mut grouped: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for vod in &self.bundle.vod {
            let Some(id) = vod.category_id.as_deref() else {
                continue;
            };
            let Some(caption) = captions.get(id) else {
                continue;
            };
            let key = film_shelf_key(&vod.name);
            if key.len() < 6 {
                continue;
            }
            let entry = grouped.entry(key).or_default();
            if entry.iter().any(|e| e == caption) || entry.len() >= 2 {
                continue;
            }
            entry.push(caption.clone());
        }
        self.vod_shelf_by_title = grouped
            .into_iter()
            .map(|(k, v)| (k, v.join(" · ")))
            .collect();
    }

    /// Two lines under a poster: `année · âge` (the year first), then the subject
    /// categories. A shelf named only « Films » or « Films 4K » is not a category.
    fn poster_caption(
        &self,
        year: Option<&str>,
        genre: Option<&str>,
        rated: Option<&str>,
        category_id: Option<&str>,
        name: &str,
    ) -> String {
        let year = crate::metadata::fmt_year(year);
        let age = self.resolved_age_label(rated, genre, category_id, name);
        let facts = match (year, age) {
            (Some(y), Some(a)) => Some(format!("{y} · {a}")),
            (Some(y), None) => Some(y),
            (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        let mut cats = self.category_line(genre, category_id);
        if cats.is_empty() {
            let key = film_shelf_key(name);
            if let Some(extra) = self.vod_shelf_by_title.get(&key) {
                cats = extra.clone();
            }
        }
        match (facts, cats.is_empty()) {
            (Some(f), true) => f,
            (Some(f), false) => format!("{f}\n{cats}"),
            (None, false) => cats,
            (None, true) => String::new(),
        }
    }

    fn category_line(&self, genre: Option<&str>, category_id: Option<&str>) -> String {
        let mut parts: Vec<String> = Vec::new();
        let push = |parts: &mut Vec<String>, piece: &str| {
            for label in crate::metadata::display_genres(piece) {
                if is_generic_shelf(&label) {
                    continue;
                }
                if parts.iter().any(|e| {
                    e.eq_ignore_ascii_case(&label)
                        || label.to_lowercase().contains(&e.to_lowercase())
                }) {
                    continue;
                }
                parts.push(label);
            }
        };
        if let Some(g) = genre {
            push(&mut parts, g);
        }
        if let Some(id) = category_id.filter(|s| !s.is_empty()) {
            if let Some(cat) = self.bundle.categories.iter().find(|c| c.id == id) {
                let label = crate::names::category_label(&cat.name);
                let piece = label
                    .rsplit('·')
                    .next()
                    .unwrap_or(label.as_str())
                    .trim();
                push(&mut parts, piece);
            }
        }
        parts.join(" · ")
    }

    /// Coarse age when the category itself is kids or adult. Ordinary shelves stay blank.
    fn category_age(&self, category_id: Option<&str>) -> Option<String> {
        let id = category_id.filter(|s| !s.is_empty())?;
        let cat = self.bundle.categories.iter().find(|c| c.id == id)?;
        shelf_age_label(&cat.name)
    }

    fn resolved_age_label(
        &self,
        rated: Option<&str>,
        genre: Option<&str>,
        category_id: Option<&str>,
        name: &str,
    ) -> Option<String> {
        crate::metadata::age_rating(rated)
            .map(|a| a.label)
            .or_else(|| crate::metadata::age_in_text(name).map(|a| a.label))
            .or_else(|| genre.and_then(|g| crate::metadata::age_in_text(g).map(|a| a.label)))
            .or_else(|| self.category_age(category_id))
    }

    fn age_filter_on(&self) -> bool {
        self.settings.hide_age_under.is_some() || self.settings.hide_age_over.is_some()
    }

    fn age_is_hidden(&self, label: Option<&str>) -> bool {
        let Some(label) = label else {
            return false;
        };
        let Some(rank) = crate::metadata::age_rank(label) else {
            return false;
        };
        self.settings.hide_age_under.is_some_and(|under| rank <= under)
            || self.settings.hide_age_over.is_some_and(|over| rank >= over)
    }

    fn media_age_visible(
        &self,
        rated: Option<&str>,
        genre: Option<&str>,
        category_id: Option<&str>,
        name: &str,
    ) -> bool {
        if !self.age_filter_on() {
            return true;
        }
        !self.age_is_hidden(
            self.resolved_age_label(rated, genre, category_id, name)
                .as_deref(),
        )
    }

    fn live_channel_visible(&self, channel: &Channel) -> bool {
        if !self.age_filter_on() {
            return true;
        }
        let shelf = channel
            .group
            .as_deref()
            .and_then(shelf_age_label);
        !self.age_is_hidden(shelf.as_deref())
    }

    /// Fill only the blank lines. A shelf that already names the genre is left as it is.
    fn apply_poster_caption(&mut self, row: &PosterCaption) -> bool {
        let snap = if row.series {
            self.bundle.series.iter().find(|s| s.id == row.id).map(|s| {
                (
                    s.year.clone(),
                    s.genre.clone(),
                    s.rated.clone(),
                    s.category_id.clone(),
                    s.name.clone(),
                    s.source_id,
                )
            })
        } else {
            self.bundle.vod.iter().find(|v| v.id == row.id).map(|v| {
                (
                    v.year.clone(),
                    v.genre.clone(),
                    v.rated.clone(),
                    v.category_id.clone(),
                    v.name.clone(),
                    v.source_id,
                )
            })
        };
        let Some((year, genre, rated, category_id, name, source_id)) = snap else {
            return false;
        };
        let year = if crate::metadata::fmt_year(year.as_deref()).is_none() {
            row.year.clone()
        } else {
            None
        };
        let rated = if self
            .resolved_age_label(rated.as_deref(), genre.as_deref(), category_id.as_deref(), &name)
            .is_none()
        {
            row.rated.clone()
        } else {
            None
        };
        let genre = if self
            .category_line(genre.as_deref(), category_id.as_deref())
            .is_empty()
        {
            row.genre.clone().filter(|g| !g.trim().is_empty())
        } else {
            None
        };
        if year.is_none() && rated.is_none() && genre.is_none() {
            return false;
        }
        let sid = row.source_id.or(source_id);
        if row.series {
            let Some(item) = self.bundle.series.iter_mut().find(|s| s.id == row.id) else {
                return false;
            };
            if let Some(year) = year {
                item.year = Some(year);
            }
            if let Some(rated) = rated {
                item.rated = Some(rated);
            }
            if let Some(genre) = genre {
                item.genre = Some(genre);
            }
            if let (Some(db), Some(sid)) = (&self.catalog_db, sid) {
                let mut saved = item.clone();
                saved.source_id = Some(sid);
                let _ = db.persist_series(&saved);
            }
        } else if let Some(item) = self.bundle.vod.iter_mut().find(|v| v.id == row.id) {
            if let Some(year) = year {
                item.year = Some(year);
            }
            if let Some(rated) = rated {
                item.rated = Some(rated);
            }
            if let Some(genre) = genre {
                item.genre = Some(genre);
            }
            if let (Some(db), Some(sid)) = (&self.catalog_db, sid) {
                let mut saved = item.clone();
                saved.source_id = Some(sid);
                let _ = db.persist_vod(&saved);
            }
        }
        let id = row.id.clone();
        if row.series {
            let seasons = self
                .series_detail
                .as_ref()
                .filter(|d| d.id == id)
                .map(|d| d.seasons.clone());
            if let Some(seasons) = seasons {
                if let Some(updated) = self.bundle.series.iter().find(|s| s.id == id) {
                    let mut d = updated.clone();
                    if d.seasons.is_empty() {
                        d.seasons = seasons;
                    }
                    self.series_detail = Some(d);
                }
            }
        } else if self.vod_detail.as_ref().is_some_and(|v| v.id == id) {
            if let Some(updated) = self.bundle.vod.iter().find(|v| v.id == id) {
                self.vod_detail = Some(updated.clone());
            }
        }
        true
    }

    /// Date, age and genres for the posters on screen. One lookup each, then the
    /// card updates. A miss is remembered for the session so it is not retried.
    fn queue_visible_ages(&mut self) -> Task<Message> {
        if self.browse_parked
            || self.age_lookup_busy
            || self.shelf != Shelf::Catalog
            || !matches!(self.tab, Tab::Vod | Tab::Series)
            || self.vod_detail.is_some()
            || self.series_detail.is_some()
        {
            return Task::none();
        }
        if self.tab == Tab::Vod {
            return self.queue_visible_film_info();
        }
        let series = self.tab == Tab::Series;
        let shown = self.shown_count(self.browse_index.len());
        let mut jobs = Vec::new();
        for pos in 0..shown {
            let (id, source_id, name, _year, genre, rated, category_id) = if series {
                let Some(s) = self
                    .browse_index
                    .get(pos)
                    .and_then(|i| self.bundle.series.get(i))
                else {
                    continue;
                };
                (
                    s.id.clone(),
                    s.source_id,
                    s.name.clone(),
                    s.year.clone(),
                    s.genre.clone(),
                    s.rated.clone(),
                    s.category_id.clone(),
                )
            } else {
                let Some(v) = self
                    .browse_index
                    .get(pos)
                    .and_then(|i| self.bundle.vod.get(i))
                else {
                    continue;
                };
                (
                    v.id.clone(),
                    v.source_id,
                    v.name.clone(),
                    v.year.clone(),
                    v.genre.clone(),
                    v.rated.clone(),
                    v.category_id.clone(),
                )
            };
            let missing_age = self
                .resolved_age_label(
                    rated.as_deref(),
                    genre.as_deref(),
                    category_id.as_deref(),
                    &name,
                )
                .is_none();
            // The portal category is the genre line. Only the age is missing
            // from the list, and a film has no age source without an OMDb key.
            if !missing_age
                || (!series
                    && !crate::metadata::omdb_configured()
                    && !crate::metadata::tmdb_configured())
            {
                continue;
            }
            if self.age_asked.contains(&id) {
                continue;
            }
            jobs.push((id, source_id, name, series));
            if jobs.len() == 8 {
                break;
            }
        }
        if jobs.is_empty() {
            return Task::none();
        }
        for (id, _, _, _) in &jobs {
            self.age_asked.insert(id.clone());
        }
        self.age_lookup_busy = true;
        Task::perform(
            async move {
                let mut out = Vec::with_capacity(jobs.len());
                let mut set = tokio::task::JoinSet::new();
                const PARALLEL: usize = 2;
                let mut i = 0usize;
                while i < jobs.len() || !set.is_empty() {
                    while set.len() < PARALLEL && i < jobs.len() {
                        let (id, sid, name, series) = jobs[i].clone();
                        i += 1;
                        set.spawn(async move {
                            let facts = crate::metadata::lookup_poster_facts(&name, series).await;
                            PosterCaption {
                                id,
                                source_id: sid,
                                series,
                                year: facts.year,
                                rated: facts.rated,
                                genre: facts.genre,
                            }
                        });
                    }
                    if let Some(Ok(row)) = set.join_next().await {
                        out.push(row);
                    }
                }
                Message::CaptionLoaded(out)
            },
            |m| m,
        )
    }

    /// Films and series stay on a short page. Chaînes and the guide load a taller block.
    fn browse_page(&self) -> usize {
        match self.tab {
            Tab::Live | Tab::Epg => LIVE_PAGE,
            _ => LIST_PAGE,
        }
    }

    /// Cap for the cards on screen. A live tab opened with the film page size still
    /// mounts one full channel block.
    fn mounted_limit(&self) -> usize {
        self.list_limit.max(self.browse_page()).min(10_000)
    }

    /// How many catalog cards are mounted. The rest wait behind « Voir plus ».
    fn shown_count(&self, total: usize) -> usize {
        total.min(self.mounted_limit())
    }

    /// Rebuild the browse index for the current tab / category / search.
    /// Heavy filters run on `spawn_blocking`; stale gens are ignored.
    ///
    /// Fast path: « All » + empty search → [`BrowseIndex::Identity`] (O(1), no million-Vec).
    fn rebuild_browse_index(&mut self) -> Task<Message> {
        // Keep the scroll position. Callers that open another category or tab
        // zero it themselves. A refresh must not throw the page back to the top.
        self.browse_was_flinging = false;
        // Keep previous `browse_index` until BrowseIndexReady — avoids empty mosaic flash.
        self.art_warm_cursor = 0;
        self.rebuild_cat_entries();

        if matches!(self.tab, Tab::Series) && self.series_detail.is_some() {
            self.episode_flat.clear();
            self.rebuild_episode_flat();
            self.finish_browse_index_rebuild();
            return self.snap_browse_scroll_task();
        }
        if matches!(self.tab, Tab::Favorites) {
            self.browse_index = BrowseIndex::mapped(
                self.bundle
                    .channels
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| self.settings.is_favorite(&c.id))
                    .map(|(i, _)| i)
                    .collect(),
            );
            self.finish_browse_index_rebuild();
            return self.snap_browse_scroll_task();
        }
        if !matches!(self.tab, Tab::Live | Tab::Epg | Tab::Vod | Tab::Series) {
            self.browse_index.clear();
            self.episode_flat.clear();
            self.finish_browse_index_rebuild();
            return Task::none();
        }
        if matches!(self.tab, Tab::Vod) && self.vod_detail.is_some() {
            self.finish_browse_index_rebuild();
            return Task::none();
        }

        let q = self.search.trim();
        let lang = self.lang_ctx();
        // Identity: full catalog order — no allocation, instant ready for 1M titles.
        if q.is_empty() && lang.is_none() && !self.age_filter_on() {
            match self.tab {
                Tab::Vod
                    if matches!(self.selected_vod_category.as_deref(), None | Some("*")) =>
                {
                    self.browse_index = BrowseIndex::identity(self.bundle.vod.len());
                    self.finish_browse_index_rebuild();
                    return Task::batch([
                        self.snap_browse_scroll_task(),
                        self.refresh_browse_art(),
                    ]);
                }
                Tab::Series
                    if matches!(self.selected_series_category.as_deref(), None | Some("*")) =>
                {
                    self.browse_index = BrowseIndex::identity(self.bundle.series.len());
                    self.finish_browse_index_rebuild();
                    return Task::batch([
                        self.snap_browse_scroll_task(),
                        self.refresh_browse_art(),
                    ]);
                }
                Tab::Live | Tab::Epg
                    if matches!(self.selected_group.as_deref(), None | Some("Tous") | Some("*")) =>
                {
                    // Live-only contiguous catalog → identity; mixed → cheap index of Live kinds.
                    let all_live = self
                        .bundle
                        .channels
                        .iter()
                        .all(|c| c.kind == ContentKind::Live);
                    self.browse_index = if all_live {
                        BrowseIndex::identity(self.bundle.channels.len())
                    } else {
                        BrowseIndex::mapped(
                            self.bundle
                                .channels
                                .iter()
                                .enumerate()
                                .filter(|(_, c)| c.kind == ContentKind::Live)
                                .map(|(i, _)| i)
                                .collect(),
                        )
                    };
                    self.finish_browse_index_rebuild();
                    return Task::batch([
                        self.snap_browse_scroll_task(),
                        self.refresh_browse_art(),
                    ]);
                }
                _ => {}
            }
        }

        let gen = self.jobs.bump_browse_gen();
        let progress = self.jobs.browse_index.clone();
        let tab = self.tab;
        let q = q.to_string();
        let group = self.selected_group.clone();
        let vod_cat = self.selected_vod_category.clone();
        let series_cat = self.selected_series_category.clone();
        let (adult_pool, strip_adult) = self.adult_shelf_context(tab);
        let pull_adult = !adult_pool.is_empty();
        let source_ids: Vec<Uuid> = self.sources.iter().map(|s| s.id).collect();

        // Slim rows: avoid cloning titles when we only need category / id match.
        let live_rows: Vec<(usize, String, Option<String>)> = if matches!(tab, Tab::Live | Tab::Epg) {
            self.bundle
                .channels
                .iter()
                .enumerate()
                .filter(|(_, c)| c.kind == ContentKind::Live && self.live_channel_visible(c))
                .map(|(i, c)| (i, c.name.clone(), c.group.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let vod_rows: Vec<(usize, String, String, Option<String>)> = if matches!(tab, Tab::Vod) {
            let split_shelf = vod_cat.as_deref().is_some_and(|id| {
                !matches!(
                    crate::names::category_shelf(id).1,
                    crate::names::CategoryShelf::Plain
                )
            });
            if q.is_empty() && lang.is_none() && !split_shelf && !strip_adult && !pull_adult {
                // Category filter only — skip name clone.
                self.bundle
                    .vod
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| {
                        self.media_age_visible(
                            v.rated.as_deref(),
                            v.genre.as_deref(),
                            v.category_id.as_deref(),
                            &v.name,
                        )
                    })
                    .map(|(i, v)| (i, String::new(), String::new(), v.category_id.clone()))
                    .collect()
            } else {
                self.bundle
                    .vod
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| {
                        self.media_age_visible(
                            v.rated.as_deref(),
                            v.genre.as_deref(),
                            v.category_id.as_deref(),
                            &v.name,
                        )
                    })
                    .map(|(i, v)| {
                        (
                            i,
                            v.id.clone(),
                            shelf_probe(&v.name, v.genre.as_deref()),
                            v.category_id.clone(),
                        )
                    })
                    .collect()
            }
        } else {
            Vec::new()
        };
        let series_rows: Vec<(usize, String, String, Option<String>)> =
            if matches!(tab, Tab::Series) {
                let split_shelf = series_cat.as_deref().is_some_and(|id| {
                    !matches!(
                        crate::names::category_shelf(id).1,
                        crate::names::CategoryShelf::Plain
                    )
                });
                if q.is_empty() && lang.is_none() && !split_shelf && !strip_adult && !pull_adult {
                    self.bundle
                        .series
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| {
                            self.media_age_visible(
                                s.rated.as_deref(),
                                s.genre.as_deref(),
                                s.category_id.as_deref(),
                                &s.name,
                            )
                        })
                        .map(|(i, s)| (i, String::new(), String::new(), s.category_id.clone()))
                        .collect()
                } else {
                    self.bundle
                        .series
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| {
                            self.media_age_visible(
                                s.rated.as_deref(),
                                s.genre.as_deref(),
                                s.category_id.as_deref(),
                                &s.name,
                            )
                        })
                        .map(|(i, s)| {
                            (
                                i,
                                s.id.clone(),
                                shelf_probe(&s.name, s.genre.as_deref()),
                                s.category_id.clone(),
                            )
                        })
                        .collect()
                }
            } else {
                Vec::new()
            };

        progress.reset(1);
        Task::perform(
            async move {
                crate::async_jobs::run_blocking(move || {
                    build_browse_index_blocking(
                        tab,
                        q,
                        group,
                        vod_cat,
                        series_cat,
                        adult_pool,
                        strip_adult,
                        source_ids,
                        live_rows,
                        vod_rows,
                        series_rows,
                        lang,
                        progress,
                    )
                })
                .await
                .unwrap_or_default()
            },
            move |(index, episode_flat)| Message::BrowseIndexReady {
                gen,
                index,
                episode_flat,
            },
        )
    }


    fn finish_browse_index_rebuild(&mut self) {
        self.art_warm_active = !self.browse_index.is_empty()
            && self.vod_detail.is_none()
            && !(matches!(self.tab, Tab::Series) && self.series_detail.is_some());
        self.browse_slice =
            self.content_virtual_slice(self.browse_scroll_y, self.browse_view_h);
    }

    fn rebuild_episode_flat(&mut self) {
        self.episode_flat.clear();
        let Some(detail) = &self.series_detail else {
            return;
        };
        for (si, season) in detail.seasons.iter().enumerate() {
            self.episode_flat
                .push(EpisodeFlat::Header(season.season_number));
            for ei in 0..season.episodes.len() {
                self.episode_flat.push(EpisodeFlat::Ep {
                    season_idx: si,
                    ep_idx: ei,
                });
            }
        }
    }

    fn section_kind(&self) -> Option<ContentKind> {
        match self.tab {
            Tab::Live => Some(ContentKind::Live),
            Tab::Vod => Some(ContentKind::Vod),
            Tab::Series => Some(ContentKind::Series),
            _ => None,
        }
    }

    fn shelf_counts(&self) -> (usize, usize) {
        let history = match self.section_kind() {
            Some(ContentKind::Vod) | Some(ContentKind::Series) => {
                let kind = self.section_kind().unwrap();
                self.settings
                    .recent
                    .iter()
                    .filter(|r| r.is_media() && r.media_kind() == kind)
                    .count()
            }
            _ => 0,
        };
        let favorites = match self.tab {
            Tab::Live => self
                .bundle
                .channels
                .iter()
                .filter(|c| self.settings.is_favorite(&c.id))
                .count(),
            Tab::Vod => self
                .bundle
                .vod
                .iter()
                .filter(|v| self.settings.is_favorite(&v.id))
                .count(),
            Tab::Series => self
                .bundle
                .series
                .iter()
                .filter(|s| self.settings.is_favorite(&s.id))
                .count(),
            _ => 0,
        };
        (history, favorites)
    }

    /// Historique and Favoris live under the open section in the rail, not in the category list.
    fn rail_subs(&self) -> Vec<(String, bool, Message)> {
        if !matches!(self.tab, Tab::Live | Tab::Vod | Tab::Series) {
            return Vec::new();
        }
        let (history, favorites) = self.shelf_counts();
        let history_label = if history > 0 {
            format!("Historique · {history}")
        } else {
            "Historique".into()
        };
        let fav_label = if favorites > 0 {
            format!("Favoris · {favorites}")
        } else {
            "Favoris".into()
        };
        vec![
            (
                history_label,
                self.shelf == Shelf::History,
                Message::ShowShelf(Shelf::History),
            ),
            (
                fav_label,
                self.shelf == Shelf::Favorites,
                Message::ShowShelf(Shelf::Favorites),
            ),
        ]
    }

    fn refresh_shelf_labels(&mut self) {
        let (history, favorites) = self.shelf_counts();
        for (id, _name, active, count) in &mut self.cat_entries {
            if id == SHELF_HISTORY {
                *active = self.shelf == Shelf::History;
                *count = history;
            } else if id == SHELF_FAVORITES {
                *active = self.shelf == Shelf::Favorites;
                *count = favorites;
            }
        }
    }

    fn shelf_clear(&self) -> Option<Message> {
        if self.shelf != Shelf::History {
            return None;
        }
        let kind = self.section_kind()?;
        if kind == ContentKind::Live {
            return None;
        }
        Some(Message::ClearHistory)
    }

    fn shelf_pills(&self, ui: UiTheme) -> Element<'_, Message> {
        let (history, favorites) = self.shelf_counts();
        let history_label = if history > 0 {
            format!("Historique · {history}")
        } else {
            "Historique".into()
        };
        let fav_label = if favorites > 0 {
            format!("Favoris · {favorites}")
        } else {
            "Favoris".into()
        };
        browser::shelf_switch(ui, self.shelf, history_label, fav_label, self.shelf_clear())
    }

    fn with_shelf_pills<'a>(
        &'a self,
        pills: bool,
        header: Element<'a, Message>,
        scroller: Element<'a, Message>,
        gap: f32,
    ) -> Element<'a, Message> {
        let mut col = Column::new().spacing(gap).width(Fill).height(Fill);
        if pills {
            col = col.push(self.shelf_pills(self.ui_theme()));
        }
        col.push(header).push(scroller).into()
    }

    /// Poster for an older history row that was saved before logos were kept.
    fn lookup_history_poster(&self, name: &str, kind: ContentKind) -> Option<String> {
        let raw = name.split(" — ").next().unwrap_or(name);
        let needle = crate::names::display_title(raw);
        if needle.trim().is_empty() {
            return None;
        }
        match kind {
            ContentKind::Series => self.bundle.series.iter().find(|s| {
                crate::names::display_title(&s.name).eq_ignore_ascii_case(&needle)
            })
            .and_then(crate::images::series_cover_url),
            ContentKind::Vod => self
                .bundle
                .vod
                .iter()
                .find(|v| crate::names::display_title(&v.name).eq_ignore_ascii_case(&needle))
                .and_then(crate::images::vod_poster_url),
            ContentKind::Live => None,
        }
    }

    fn remember_history_art(&mut self) {
        self.history_art.clear();
        for r in &self.settings.recent {
            if !r.is_media() {
                continue;
            }
            let poster = r
                .logo
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| self.lookup_history_poster(&r.name, r.media_kind()));
            if let Some(poster) = poster {
                self.history_art.push((r.stream_url.clone(), poster));
            }
        }
    }

    fn prefetch_shelf_art(&mut self) -> Task<Message> {
        self.remember_history_art();
        let urls: Vec<String> = match (self.tab, self.shelf) {
            (_, Shelf::History) => self.history_art.iter().map(|(_, url)| url.clone()).collect(),
            (Tab::Series, Shelf::Favorites) => self
                .bundle
                .series
                .iter()
                .filter(|s| self.settings.is_favorite(&s.id))
                .filter_map(crate::images::series_cover_url)
                .collect(),
            (Tab::Vod, Shelf::Favorites) => self
                .bundle
                .vod
                .iter()
                .filter(|v| self.settings.is_favorite(&v.id))
                .filter_map(crate::images::vod_poster_url)
                .collect(),
            (Tab::Live, Shelf::Favorites) => self
                .bundle
                .channels
                .iter()
                .filter(|c| self.settings.is_favorite(&c.id))
                .filter_map(|c| c.logo.clone().or_else(|| c.tvg_logo.clone()))
                .collect(),
            _ => Vec::new(),
        };
        self.prefetch_urls(urls)
    }

    /// Sidebar categories for the active tab (cached — not rebuilt on content scroll).
    fn rebuild_cat_entries(&mut self) {
        self.cat_slice = (0, 0);
        self.cat_entries.clear();
        match self.tab {
            Tab::Live | Tab::Epg => {
                let cats: Vec<(String, String)> = {
                    let listed = self.filtered_categories(ContentKind::Live);
                    if listed.is_empty() {
                        self.bundle
                            .group_names()
                            .into_iter()
                            .filter(|n| self.keep_live_group(n))
                            .take(CAT_PAGE)
                            .map(|n| (n.clone(), crate::names::category_label(&n)))
                            .collect()
                    } else {
                        listed
                            .into_iter()
                            .take(CAT_PAGE)
                            .map(|c| (c.name.clone(), crate::names::category_label(&c.name)))
                            .collect()
                    }
                };
                let catalog = self.shelf == Shelf::Catalog;
                let all_active = catalog && self.selected_group.is_none();
                let all_count = if self.age_filter_on() {
                    self.bundle
                        .channels
                        .iter()
                        .filter(|c| c.kind == ContentKind::Live && self.live_channel_visible(c))
                        .count()
                } else {
                    self.bundle.channels.len()
                };
                self.cat_entries
                    .push(("*".into(), "Toutes".into(), all_active, all_count));
                for (id, name) in cats {
                    // `selected_group` holds the raw group name (the id), not its label.
                    let active = catalog && self.selected_group.as_deref() == Some(id.as_str());
                    self.cat_entries.push((id, name, active, 0));
                }
            }
            Tab::Vod => {
                let cats: Vec<(String, String)> = self
                    .filtered_categories(ContentKind::Vod)
                    .into_iter()
                    .take(CAT_PAGE)
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect();
                let catalog = self.shelf == Shelf::Catalog;
                let all_active = catalog
                    && matches!(self.selected_vod_category.as_deref(), None | Some("*"));
                let all_count = if self.lang_filter_on() {
                    0
                } else if self.age_filter_on() {
                    self.bundle
                        .vod
                        .iter()
                        .filter(|v| {
                            self.media_age_visible(
                                v.rated.as_deref(),
                                v.genre.as_deref(),
                                v.category_id.as_deref(),
                                &v.name,
                            )
                        })
                        .count()
                } else {
                    self.bundle.vod.len()
                };
                self.cat_entries
                    .push(("*".into(), "Toutes".into(), all_active, all_count));
                for (id, raw) in cats {
                    self.push_media_category(ContentKind::Vod, id, &raw);
                }
            }
            Tab::Series => {
                // Historique and Favoris are under Séries in the rail.
                let cats: Vec<(String, String)> = self
                    .filtered_categories(ContentKind::Series)
                    .into_iter()
                    .take(CAT_PAGE)
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect();
                let catalog = self.shelf == Shelf::Catalog;
                let all_active = catalog
                    && matches!(self.selected_series_category.as_deref(), None | Some("*"));
                let all_count = if self.lang_filter_on() {
                    0
                } else if self.age_filter_on() {
                    self.bundle
                        .series
                        .iter()
                        .filter(|s| {
                            self.media_age_visible(
                                s.rated.as_deref(),
                                s.genre.as_deref(),
                                s.category_id.as_deref(),
                                &s.name,
                            )
                        })
                        .count()
                } else {
                    self.bundle.series.len()
                };
                self.cat_entries
                    .push(("*".into(), "Toutes".into(), all_active, all_count));
                for (id, raw) in cats {
                    self.push_media_category(ContentKind::Series, id, &raw);
                }
            }
            _ => {}
        }
        let cs = browser::virtual_slice(
            self.cat_scroll_y,
            self.cat_view_h,
            browser::cat_row_height(),
            self.cat_entries.len(),
            self.virtual_overscan(),
        );
        self.cat_slice = (cs.start, cs.end);
    }

    /// One sidebar row, or two when the portal mixed manga with adult animation.
    fn push_media_category(&mut self, kind: ContentKind, id: String, raw: &str) {
        let selected = match kind {
            ContentKind::Vod => self.selected_vod_category.as_deref(),
            _ => self.selected_series_category.as_deref(),
        };
        if crate::names::is_manga_category(raw) {
            let manga = format!("{id}::manga");
            let adult = format!("{id}::adulte");
            let catalog = self.shelf == Shelf::Catalog;
            let manga_on = catalog && selected == Some(manga.as_str());
            let adult_on = catalog && selected == Some(adult.as_str());
            self.cat_entries.push((
                manga,
                crate::names::shelf_label(raw, false),
                manga_on,
                0,
            ));
            self.cat_entries
                .push((adult, crate::names::shelf_label(raw, true), adult_on, 0));
        } else {
            let label = crate::names::category_label(raw);
            let active = self.shelf == Shelf::Catalog && selected == Some(id.as_str());
            self.cat_entries.push((id, label, active, 0));
        }
    }

    /// Title above the mosaic. Split shelves keep their own name.
    fn shelf_heading(&self, kind: ContentKind, selected: Option<&str>, fallback: &str) -> String {
        let Some(selected) = selected else {
            return fallback.to_string();
        };
        let (base, shelf) = crate::names::category_shelf(selected);
        let Some(raw) = self
            .bundle
            .categories
            .iter()
            .find(|c| c.content == kind && c.id == base)
            .map(|c| c.name.as_str())
        else {
            return fallback.to_string();
        };
        match shelf {
            crate::names::CategoryShelf::Manga => crate::names::shelf_label(raw, false),
            crate::names::CategoryShelf::Adult => crate::names::shelf_label(raw, true),
            crate::names::CategoryShelf::Plain => crate::names::category_label(raw),
        }
    }

    /// Adult cartoons misfiled under family / comedy join the language's
    /// adult shelf, and leave that donor shelf.
    fn adult_shelf_context(&self, tab: Tab) -> (Vec<String>, bool) {
        let kind = match tab {
            Tab::Vod => ContentKind::Vod,
            Tab::Series => ContentKind::Series,
            _ => return (Vec::new(), false),
        };
        let selected = match kind {
            ContentKind::Vod => self.selected_vod_category.as_deref(),
            _ => self.selected_series_category.as_deref(),
        };
        let Some(selected) = selected.filter(|id| *id != "*") else {
            return (Vec::new(), false);
        };
        let (base, shelf) = crate::names::category_shelf(selected);
        let lang = self
            .bundle
            .categories
            .iter()
            .find(|c| c.content == kind && (c.id == base || c.id == selected))
            .and_then(|c| crate::names::parse_category(&c.name).code);
        let Some(lang) = lang else {
            return (Vec::new(), false);
        };
        let has_manga = self.bundle.categories.iter().any(|c| {
            c.content == kind
                && crate::names::is_manga_category(&c.name)
                && crate::names::parse_category(&c.name).code.as_deref() == Some(lang.as_str())
        });
        if !has_manga {
            return (Vec::new(), false);
        }
        match shelf {
            crate::names::CategoryShelf::Adult => {
                let pool = self
                    .bundle
                    .categories
                    .iter()
                    .filter(|c| {
                        c.content == kind
                            && c.id != base
                            && crate::names::parse_category(&c.name).code.as_deref()
                                == Some(lang.as_str())
                            && crate::names::is_adult_donor_category(&c.name)
                    })
                    .map(|c| c.id.clone())
                    .collect();
                (pool, false)
            }
            crate::names::CategoryShelf::Plain => {
                let strip = self.bundle.categories.iter().any(|c| {
                    c.content == kind
                        && c.id == selected
                        && crate::names::is_adult_donor_category(&c.name)
                });
                (Vec::new(), strip)
            }
            crate::names::CategoryShelf::Manga => (Vec::new(), false),
        }
    }

    fn browse_settle_task() -> Task<Message> {
        Task::perform(
            async { tokio::time::sleep(BROWSE_SETTLE_AFTER).await },
            |_| Message::BrowseSettle,
        )
    }

    /// Fast fling → defer *new* art fetches (LOD). Threshold in scroll px/s.
    fn browse_flinging(&self) -> bool {
        self.browse_scroll_vy.abs() >= 2_400.0
    }

    fn browse_overscan(&self) -> usize {
        if self.browse_flinging() {
            // Deeper window while flinging — shrinking overscan mid-scroll
            // jumps spacer geometry and blanks the mosaic.
            self.virtual_overscan().saturating_mul(2).max(20)
        } else {
            self.virtual_overscan()
        }
    }

    /// Content virtual window for current tab (rows for mosaic, items for lists).
    fn content_virtual_slice(&self, scroll_y: f32, view_h: f32) -> (usize, usize) {
        let m = self.layout_metrics();
        let overscan = self.browse_overscan();
        match self.tab {
            Tab::Vod if self.vod_detail.is_none() => {
                let (cols, tile_w) = self.poster_grid();
                let rows = self.shown_count(self.browse_index.len()).div_ceil(cols);
                let s = browser::virtual_slice(
                    scroll_y,
                    view_h,
                    browser::mosaic_row_height(tile_w),
                    rows,
                    overscan,
                );
                (s.start, s.end)
            }
            Tab::Series if self.series_detail.is_none() => {
                let (cols, tile_w) = self.poster_grid();
                let rows = self.shown_count(self.browse_index.len()).div_ceil(cols);
                let s = browser::virtual_slice(
                    scroll_y,
                    view_h,
                    browser::mosaic_row_height(tile_w),
                    rows,
                    overscan,
                );
                (s.start, s.end)
            }
            Tab::Series if self.series_detail.is_some() => {
                let s = browser::virtual_slice(
                    scroll_y,
                    view_h,
                    browser::list_row_height(m.thumb),
                    self.episode_flat.len(),
                    overscan,
                );
                (s.start, s.end)
            }
            Tab::Live | Tab::Epg => {
                let (cols, _tile_w) = self.live_grid();
                let rows = self.shown_count(self.browse_index.len()).div_ceil(cols);
                let s = browser::virtual_slice(
                    scroll_y,
                    view_h,
                    browser::channel_row_height(),
                    rows,
                    overscan,
                );
                (s.start, s.end)
            }
            Tab::Favorites => {
                let s = browser::virtual_slice(
                    scroll_y,
                    view_h,
                    browser::list_row_height(0.0),
                    self.browse_index.len(),
                    overscan,
                );
                (s.start, s.end)
            }
            _ => (0, 0),
        }
    }

    /// Browse positions actually inside the viewport (no overscan / lookahead).
    fn onscreen_browse_range(&self, _cols: usize, tile_w: f32) -> std::ops::Range<usize> {
        let len = self.shown_count(self.browse_index.len());
        let (_cols, tile_w, row_h, per_row) = if matches!(self.tab, Tab::Live | Tab::Epg) {
            let (c, w) = self.live_grid();
            (c, w, browser::channel_row_height(), c)
        } else if matches!(self.tab, Tab::Vod | Tab::Series)
            && self.vod_detail.is_none()
            && self.series_detail.is_none()
        {
            let (c, w) = self.poster_grid();
            (c, w, browser::mosaic_row_height(w), c)
        } else {
            (1, tile_w, browser::list_row_height(0.0), 1)
        };
        let _ = tile_w;
        let rows = len.div_ceil(per_row);
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            rows,
            0,
        );
        (slice.start * per_row).min(len)..(slice.end * per_row).min(len)
    }

    fn visible_browse_window(&self, cols: usize, tile_w: f32) -> std::ops::Range<usize> {
        self.art_prefetch_window(cols, tile_w)
    }

    /// Visible mosaic/list indices plus velocity-based lookahead for fast flings.
    fn art_prefetch_window(&self, _cols: usize, tile_w: f32) -> std::ops::Range<usize> {
        let row_h = match self.tab {
            Tab::Vod | Tab::Series if self.series_detail.is_none() && self.vod_detail.is_none() => {
                browser::mosaic_row_height(tile_w)
            }
            _ => browser::list_row_height(0.0),
        };
        // Lead by ~0.35s of fling distance, clamped to 2–16 rows.
        let lead_rows = ((self.browse_scroll_vy.abs() * 0.35) / row_h.max(1.0))
            .round()
            .clamp(2.0, 16.0) as usize;
        let ahead = self.browse_scroll_vy > 40.0;
        let behind = self.browse_scroll_vy < -40.0;

        match self.tab {
            Tab::Vod | Tab::Series if self.series_detail.is_none() && self.vod_detail.is_none() => {
                let (cols, tile_w) = self.poster_grid();
                let row_h = browser::mosaic_row_height(tile_w);
                let rows = self.shown_count(self.browse_index.len()).div_ceil(cols);
                let slice = browser::virtual_slice(
                    self.browse_scroll_y,
                    self.browse_view_h,
                    row_h,
                    rows,
                    self.virtual_overscan().saturating_add(lead_rows / 2),
                );
                let mut start = slice.start;
                let mut end = slice.end;
                if ahead {
                    end = (end + lead_rows).min(rows);
                } else if behind {
                    start = start.saturating_sub(lead_rows);
                } else {
                    end = (end + lead_rows / 2).min(rows);
                    start = start.saturating_sub(lead_rows / 2);
                }
                let start_i = start.saturating_mul(cols);
                let end_i = (end.saturating_mul(cols)).min(self.shown_count(self.browse_index.len()));
                start_i..end_i
            }
            Tab::Live | Tab::Epg => {
                let (cols, _tile_w) = self.live_grid();
                let row_h = browser::channel_row_height();
                let lead_rows = ((self.browse_scroll_vy.abs() * 0.35) / row_h.max(1.0))
                    .round()
                    .clamp(2.0, 16.0) as usize;
                let rows = self.shown_count(self.browse_index.len()).div_ceil(cols);
                let slice = browser::virtual_slice(
                    self.browse_scroll_y,
                    self.browse_view_h,
                    row_h,
                    rows,
                    self.virtual_overscan().saturating_add(lead_rows / 2),
                );
                let mut start = slice.start;
                let mut end = slice.end;
                if ahead {
                    end = (end + lead_rows).min(rows);
                } else if behind {
                    start = start.saturating_sub(lead_rows);
                } else {
                    end = (end + lead_rows / 2).min(rows);
                    start = start.saturating_sub(lead_rows / 2);
                }
                let start_i = start.saturating_mul(cols);
                let end_i = end
                    .saturating_mul(cols)
                    .min(self.shown_count(self.browse_index.len()));
                return start_i..end_i;
            }
            Tab::Favorites => {
                let slice = browser::virtual_slice(
                    self.browse_scroll_y,
                    self.browse_view_h,
                    row_h,
                    self.browse_index.len(),
                    self.virtual_overscan().saturating_add(lead_rows / 2),
                );
                let mut start = slice.start;
                let mut end = slice.end;
                if ahead {
                    end = (end + lead_rows).min(self.browse_index.len());
                } else if behind {
                    start = start.saturating_sub(lead_rows);
                }
                start..end
            }
            _ => 0..0,
        }
    }

    fn flush_pending_images(&mut self) -> Task<Message> {
        if self.browse_parked {
            self.pending_images.clear();
            return Task::none();
        }
        if self.pending_images.is_empty() {
            let waiting = std::mem::take(&mut self.art_wait);
            return if waiting.is_empty() {
                Task::none()
            } else {
                self.prefetch_urls(waiting)
            };
        }
        let batch = std::mem::take(&mut self.pending_images);
        // Warm-ahead batches are newer than what is on screen: bump the visible art
        // first so LRU eviction drops off-screen posters, not the ones being shown
        // (they were never re-requested and stayed as letter placeholders).
        self.promote_visible_art();
        for (url, source_id, bytes) in batch {
            let n = bytes.len();
            self.images.insert_ui_bytes(url.clone(), bytes);
            if let (Some(db), Some(sid), Some(path)) = (
                &self.catalog_db,
                source_id,
                crate::images::disk_path_for_url(&url, source_id),
            ) {
                db.touch_image(sid, &url, &path, n);
            }
        }
        if let Some(db) = &self.catalog_db {
            db.evict_old_images(4_000);
            db.evict_old_meta(30 * 24 * 60 * 60);
        }
        let waiting = std::mem::take(&mut self.art_wait);
        if waiting.is_empty() {
            Task::none()
        } else {
            self.prefetch_urls(waiting)
        }
    }

    fn defer_art(&mut self, url: String) {
        let url = url.trim().to_string();
        if url.is_empty() || self.art_wait.iter().any(|u| u == &url) {
            return;
        }
        if self.art_wait.len() >= 48 {
            self.art_wait.remove(0);
        }
        self.art_wait.push(url);
    }

    fn promote_visible_art(&mut self) {
        if self.vod_detail.is_some() || self.series_detail.is_some() {
            return;
        }
        let m = self.layout_metrics();
        let win = self.visible_browse_window(m.cols, m.tile_w);
        for pos in win.start..win.end.min(self.browse_index.len()) {
            if let Some(url) = self.art_url_for_browse_pos(pos) {
                self.images.promote(&url);
            }
        }
    }

    fn prefetch_urls(&mut self, urls: impl IntoIterator<Item = String>) -> Task<Message> {
        // Fast scroll: fill the mosaic window quickly (cap via ImageCache inflight).
        let max = self.display_caps.tuning.image_prefetch;
        self.prefetch_urls_capped(urls, max)
    }

    fn prefetch_urls_capped(
        &mut self,
        urls: impl IntoIterator<Item = String>,
        max: usize,
    ) -> Task<Message> {
        self.prefetch_art(
            urls.into_iter().map(|url| (url, None)).collect(),
            max,
        )
    }

    /// `source_id` hits the profile disk cache. `None` misses it and re-downloads.
    fn prefetch_art(
        &mut self,
        urls: Vec<(String, Option<uuid::Uuid>)>,
        max: usize,
    ) -> Task<Message> {
        let mut tasks = Vec::new();
        // The rows on screen go first. The wait queue used to spend every free
        // slot on posters from the top of the catalogue.
        let urls: Vec<(String, Option<uuid::Uuid>)> = urls
            .into_iter()
            .chain(
                std::mem::take(&mut self.art_wait)
                    .into_iter()
                    .map(|url| (url, None)),
            )
            .collect();
        let room = self.images.max_inflight
            .saturating_sub(self.images.inflight_len())
            .min(max);
        if room == 0 {
            // Headshots arrive while the mosaic still fills the inflight cap.
            // Keep them and retry when a fetch finishes, instead of dropping them.
            let queued = urls.len();
            for (url, _) in urls {
                self.defer_art(url);
            }
            tracing::debug!(
                target: "fluxplay::images",
                inflight = self.images.inflight_len(),
                queued,
                y = self.browse_scroll_y,
                "prefetch deferred — no inflight room"
            );
            return Task::none();
        }
        for (url, sid) in urls {
            if tasks.len() >= room {
                self.defer_art(url);
                continue;
            }
            if let crate::images::RequestOutcome::Fetch { url: u, source_id } =
                self.images.request(&url, sid)
            {
                tasks.push(Task::perform(
                    crate::images::fetch_image_prepared_edged(u, source_id, self.images.decode_edge_px),
                    Message::ImageLoaded,
                ));
            }
        }
        if tasks.is_empty() {
            Task::none()
        } else {
            let n = tasks.len() as u64;
            tracing::debug!(
                target: "fluxplay::images",
                n,
                y = self.browse_scroll_y,
                "poster prefetch"
            );
            let (d, t) = self.jobs.images.snapshot();
            self.jobs.images.set_total(t.saturating_add(n).max(d + n));
            Task::batch(tasks)
        }
    }

    /// Prefetch posters for the current browse selection at profile load/reload.
    /// Walks `browse_index` in order (what the user will scroll) — not catalog tip.
    fn prefetch_catalog_art(&mut self) -> Task<Message> {
        if self.browse_parked {
            return Task::none();
        }
        self.art_warm_cursor = 0;
        self.art_warm_active = !self.browse_index.is_empty();
        self.refresh_browse_art()
    }

    /// Visible window + background warm of the current filter.
    fn refresh_browse_art(&mut self) -> Task<Message> {
        Task::batch([self.prefetch_visible_art(), self.pump_art_warm()])
    }

    fn continue_art_warm(&mut self) -> Task<Message> {
        if self.browse_parked || !self.art_warm_active || self.browse_flinging() {
            return Task::none();
        }
        self.pump_art_warm()
    }

    /// How many browse_index entries to warm after load (~8–12 mosaic screens).
    fn art_warm_target(&self) -> usize {
        let cols = self.layout_metrics().cols.max(1);
        (cols.saturating_mul(56)).clamp(160, 560).min(self.browse_index.len())
    }

    fn art_url_for_browse_pos(&self, pos: usize) -> Option<String> {
        let i = self.browse_index.get(pos)?;
        match self.tab {
            Tab::Live | Tab::Epg | Tab::Favorites => {
                let ch = self.bundle.channels.get(i)?;
                crate::images::pick_art(
                    ch.logo.as_deref().or(ch.tvg_logo.as_deref()),
                    None,
                    None,
                    None,
                )
            }
            Tab::Vod if self.vod_detail.is_none() => {
                let v = self.bundle.vod.get(i)?;
                crate::images::vod_poster_url(v)
            }
            Tab::Series if self.series_detail.is_none() => {
                let s = self.bundle.series.get(i)?;
                crate::images::series_cover_url(s)
            }
            _ => None,
        }
    }

    fn art_url_source_at(&self, pos: usize) -> Option<Uuid> {
        let i = self.browse_index.get(pos)?;
        match self.tab {
            Tab::Live | Tab::Epg | Tab::Favorites => {
                self.bundle.channels.get(i).and_then(|c| c.source_id)
            }
            Tab::Vod if self.vod_detail.is_none() => {
                self.bundle.vod.get(i).and_then(|v| v.source_id)
            }
            Tab::Series if self.series_detail.is_none() => {
                self.bundle.series.get(i).and_then(|s| s.source_id)
            }
            _ => None,
        }
    }

    /// After soft video, shared atlas UVs may point at stale pixels — new Handle ids force re-upload.
    fn refresh_browse_poster_gpu_textures(&mut self) {
        let range = match self.tab {
            Tab::Vod | Tab::Series => {
                let (cols, tile_w) = self.poster_grid();
                let screen = self.onscreen_browse_range(cols, tile_w);
                let end = screen
                    .end
                    .saturating_add(cols)
                    .min(self.browse_index.len());
                screen.start..end
            }
            Tab::Live | Tab::Epg | Tab::Favorites => {
                let m = self.layout_metrics();
                let win = self.visible_browse_window(m.cols, m.tile_w);
                win.start..win.end.min(self.browse_index.len())
            }
            _ => 0..0,
        };
        for i in range {
            if let Some(url) = self.poster_url_at(i) {
                let sid = self.poster_source_at(i);
                self.images.refresh_gpu_texture(&url, sid);
            }
        }
        self.refresh_open_detail_gpu();
    }

    /// Poster URL for a browse row, including while a film or series page is open.
    fn poster_url_at(&self, pos: usize) -> Option<String> {
        let i = self.browse_index.get(pos)?;
        match self.tab {
            Tab::Live | Tab::Epg | Tab::Favorites => {
                let ch = self.bundle.channels.get(i)?;
                crate::images::pick_art(
                    ch.logo.as_deref().or(ch.tvg_logo.as_deref()),
                    None,
                    None,
                    None,
                )
            }
            Tab::Vod => {
                let v = self.bundle.vod.get(i)?;
                crate::images::vod_poster_url(v)
            }
            Tab::Series => {
                let s = self.bundle.series.get(i)?;
                crate::images::series_cover_url(s)
            }
            _ => None,
        }
    }

    fn poster_source_at(&self, pos: usize) -> Option<Uuid> {
        let i = self.browse_index.get(pos)?;
        match self.tab {
            Tab::Live | Tab::Epg | Tab::Favorites => {
                self.bundle.channels.get(i).and_then(|c| c.source_id)
            }
            Tab::Vod => self.bundle.vod.get(i).and_then(|v| v.source_id),
            Tab::Series => self.bundle.series.get(i).and_then(|s| s.source_id),
            _ => None,
        }
    }

    /// Series cover, episode stills and the open film poster.
    fn refresh_open_detail_gpu(&mut self) {
        let mut urls: Vec<(String, Option<Uuid>)> = Vec::new();
        if let Some(v) = &self.vod_detail {
            let sid = v.source_id;
            if let Some(u) = crate::images::vod_poster_url(v) {
                urls.push((u, sid));
            }
        }
        if let Some(s) = &self.series_detail {
            let sid = s.source_id;
            if let Some(u) = crate::images::series_cover_url(s) {
                urls.push((u, sid));
            }
            let want = self.selected_season;
            if let Some(season) = s
                .seasons
                .iter()
                .find(|se| Some(se.season_number) == want)
                .or_else(|| s.seasons.iter().find(|se| se.season_number > 0))
                .or_else(|| s.seasons.first())
            {
                for ep in &season.episodes {
                    if let Some(still) = ep.still.clone().filter(|u| u.starts_with("http")) {
                        urls.push((still, sid));
                    }
                }
            }
        }
        for url in self.cast_photos.values() {
            urls.push((url.clone(), None));
        }
        for (url, sid) in urls {
            self.images.refresh_gpu_texture(&url, sid);
        }
    }

    /// Fill inflight slots from browse_index[art_warm_cursor..] until warm target.
    fn pump_art_warm(&mut self) -> Task<Message> {
        if !self.art_warm_active {
            return Task::none();
        }
        // Films and series: stay on the rows in front of the user. Warming the
        // catalog from the top filled every slot, so scrolled posters never started.
        let posters = matches!(self.tab, Tab::Vod | Tab::Series)
            && self.vod_detail.is_none()
            && self.series_detail.is_none();
        let target = if posters {
            let (cols, tile_w) = self.poster_grid();
            let screen = self.onscreen_browse_range(cols, tile_w);
            let ahead = screen
                .end
                .saturating_add(cols)
                .min(self.mounted_limit())
                .min(self.browse_index.len());
            if self.art_warm_cursor + cols < screen.start || self.art_warm_cursor > ahead {
                self.art_warm_cursor = screen.start;
            }
            ahead
        } else {
            self.art_warm_target().min(self.mounted_limit())
        };
        if self.art_warm_cursor >= target {
            if !posters || target >= self.browse_index.len() {
                self.art_warm_active = false;
            }
            return Task::none();
        }
        let mut tasks = Vec::new();
        let room = self.images.max_inflight
            .saturating_sub(self.images.inflight_len())
            .min(18);
        if room == 0 {
            return Task::none();
        }
        while self.art_warm_cursor < target && tasks.len() < room {
            if self.images.inflight_len() + tasks.len() >= self.images.max_inflight {
                break;
            }
            let Some(url) = self.art_url_for_browse_pos(self.art_warm_cursor) else {
                self.art_warm_cursor += 1;
                continue;
            };
            let sid = self.art_url_source_at(self.art_warm_cursor);
            match self.images.request(&url, sid) {
                crate::images::RequestOutcome::Fetch { url: u, source_id } => {
                    self.art_warm_cursor += 1;
                    tasks.push(Task::perform(
                        crate::images::fetch_image_prepared_edged(u, source_id, self.images.decode_edge_px),
                        Message::ImageLoaded,
                    ));
                }
                crate::images::RequestOutcome::Ready
                | crate::images::RequestOutcome::Pending
                | crate::images::RequestOutcome::Skip => {
                    self.art_warm_cursor += 1;
                }
            }
        }
        if self.art_warm_cursor >= target && (!posters || target >= self.browse_index.len()) {
            self.art_warm_active = false;
        }
        if tasks.is_empty() {
            return Task::none();
        }
        let n = tasks.len() as u64;
        let (d, t) = self.jobs.images.snapshot();
        self.jobs.images.set_total(t.saturating_add(n).max(d + n));
        Task::batch(tasks)
    }

    fn prefetch_visible_art(&mut self) -> Task<Message> {
        if self.browse_parked {
            return Task::none();
        }
        let mut urls = Vec::new();
        let m = self.layout_metrics();
        let win = self.visible_browse_window(m.cols, m.tile_w);
        let in_grid = match self.tab {
            Tab::Vod => self.vod_detail.is_none(),
            Tab::Series => self.series_detail.is_none(),
            Tab::Live | Tab::Epg | Tab::Favorites => true,
            _ => false,
        };
        if in_grid {
            // On-screen tiles first, then the overscan/lookahead band; the request
            // cap covers at least a full screen. Walking the band top-down spent the
            // whole budget on rows above the viewport, so visible tiles never loaded.
            let (grid_cols, grid_tile) = if matches!(self.tab, Tab::Vod | Tab::Series) {
                self.poster_grid()
            } else if matches!(self.tab, Tab::Live | Tab::Epg) {
                self.live_grid()
            } else {
                (m.cols, m.tile_w)
            };
            if matches!(self.tab, Tab::Vod | Tab::Series) {
                self.art_wait.clear();
            }
            let screen = self.onscreen_browse_range(grid_cols, grid_tile);
            let order = screen
                .clone()
                .chain(win.start..screen.start.max(win.start))
                .chain(screen.end.min(win.end)..win.end);
            let mut queued = Vec::new();
            for pos in order {
                if let Some(u) = self.art_url_for_browse_pos(pos) {
                    self.images.promote(&u);
                    queued.push((u, self.art_url_source_at(pos)));
                }
            }
            let cap = if matches!(self.tab, Tab::Vod | Tab::Series) {
                screen
                    .len()
                    .saturating_add(grid_cols)
                    .clamp(20, self.images.max_inflight)
            } else {
                self.display_caps
                    .tuning
                    .image_prefetch
                    .max(screen.len())
            };
            return self.prefetch_art(queued, cap);
        }
        match self.tab {
            Tab::Live | Tab::Favorites => {
                for i in self.browse_index.window(win.start, win.end) {
                    let Some(ch) = self.bundle.channels.get(i) else {
                        continue;
                    };
                    if let Some(u) = crate::images::pick_art(
                        ch.logo.as_deref().or(ch.tvg_logo.as_deref()),
                        None,
                        None,
                        None,
                    ) {
                        urls.push(u);
                    }
                }
            }
            Tab::Vod => {
                if let Some(d) = &self.vod_detail {
                    if let Some(u) = crate::images::pick_art(None, d.poster.as_deref(), None, None) {
                        urls.push(u);
                    }
                } else {
                    for i in self.browse_index.window(win.start, win.end) {
                        let Some(v) = self.bundle.vod.get(i) else {
                            continue;
                        };
                        if let Some(u) = crate::images::vod_poster_url(v) {
                            self.images.promote(&u);
                            urls.push(u);
                        }
                    }
                }
            }
            Tab::Series => {
                if let Some(d) = &self.series_detail {
                    if let Some(u) =
                        crate::images::pick_art(None, None, d.cover.as_deref(), d.banner.as_deref())
                    {
                        urls.push(u);
                    }
                } else {
                    for i in self.browse_index.window(win.start, win.end) {
                        let Some(s) = self.bundle.series.get(i) else {
                            continue;
                        };
                        if let Some(u) = crate::images::series_cover_url(s) {
                            self.images.promote(&u);
                            urls.push(u);
                        }
                    }
                }
            }
            _ => {}
        }
        self.prefetch_urls(urls)
    }

    fn resync_player_options(&mut self) {
        let prev = self.session.native.options_mut().clone();
        #[allow(unused_mut)] // mutated under android cfg
        let mut next = play_options_from(&self.settings);
        // Never clobber a live Surface bind / present mode mid-play — that leaves
        // punch-through on with Soft flags (or vice versa) → black hole.
        #[cfg(target_os = "android")]
        if self.session.channel.is_some() {
            next.android_present = prev.android_present;
            next.android_surface_wid = prev.android_surface_wid;
            next.android_surface_wh = prev.android_surface_wh;
            // Soft vf may still update for soft path quality changes.
            if prev.android_present.uses_surface() {
                next.android_soft_vf = prev.android_soft_vf.clone();
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = &prev;
        }
        // Headers come from the playing profile, not from the settings being changed.
        if self.session.channel.is_some() {
            next.user_agent = prev.user_agent.clone();
            next.referer = prev.referer.clone();
        }
        *self.session.native.options_mut() = next;
    }

    fn apply_saved_video_defaults_for_current_play(&mut self) {
        #[cfg(target_os = "android")]
        let oled_hint = crate::android_bridge::poll_android_device_caps()
            .map(|c| c.panel_oled)
            .unwrap_or(false);
        #[cfg(not(target_os = "android"))]
        let oled_hint = false;
        let panel_eq = self
            .settings
            .display_panel
            .eq_filter(oled_hint)
            .map(ToString::to_string);
        let color_adjust = self
            .settings
            .display_panel
            .color_adjust(oled_hint, false);
        let surface = self.session.native.android_surface_present();
        let soft_vf = if surface {
            None
        } else {
            #[cfg(target_os = "android")]
            {
                let (rw, rh) = self.soft_present_wh();
                Some(
                    fluxplay_player::SoftBudget {
                        max_w: rw,
                        max_h: rh,
                        video_hz: 30,
                        gui_hz: 60,
                    }
                    .vf_scale(),
                )
            }
            #[cfg(not(target_os = "android"))]
            {
                self.settings.video_quality.max_wh().map(|(w, h)| {
                    let w = w.max(2) & !1;
                    let h = h.max(2) & !1;
                    if w >= h {
                        format!("scale={w}:-2:flags=fast_bilinear,format=yuv420p")
                    } else {
                        format!("scale=-2:{h}:flags=fast_bilinear,format=yuv420p")
                    }
                })
            }
        };
        self.session.apply_saved_video_prefs(
            match self.settings.aspect {
                AspectPref::Auto => fluxplay_player::AspectMode::Auto,
                AspectPref::R16x9 => fluxplay_player::AspectMode::R16x9,
                AspectPref::R4x3 => fluxplay_player::AspectMode::R4x3,
                AspectPref::R235 => fluxplay_player::AspectMode::R235,
            },
            match self.settings.deinterlace {
                DeinterlacePref::Off => fluxplay_player::DeinterlaceMode::Off,
                DeinterlacePref::On => fluxplay_player::DeinterlaceMode::Yes,
                DeinterlacePref::Auto => fluxplay_player::DeinterlaceMode::Auto,
            },
            match self.settings.upscale {
                UpscalePref::Auto => fluxplay_player::UpscaleMode::Auto,
                UpscalePref::Bilinear => fluxplay_player::UpscaleMode::Bilinear,
                UpscalePref::Lanczos => fluxplay_player::UpscaleMode::Lanczos,
                UpscalePref::EwaLanczos => fluxplay_player::UpscaleMode::EwaLanczos,
                UpscalePref::Nearest => fluxplay_player::UpscaleMode::Nearest,
            },
            self.settings.night_mode,
            panel_eq,
            soft_vf,
            color_adjust,
        );
    }

    fn apply_source_headers_for(&mut self, ch: &Channel) {
        {
            let force_soft = {
                #[cfg(target_os = "android")]
                {
                    let v = self.android_force_soft;
                    // Sticky until Stop/close — one-shot caused demote↔Surface reopen loops.
                    v
                }
                #[cfg(not(target_os = "android"))]
                {
                    false
                }
            };
            let soft_wh = self.soft_present_wh();
            let opts = self.session.native.options_mut();
            *opts = play_options_from(&self.settings);
            let want_upgrade =
                bind_android_surface_for_play(opts, &self.settings, force_soft, soft_wh);
            #[cfg(target_os = "android")]
            {
                self.android_want_surface_upgrade = want_upgrade;
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = want_upgrade;
            }
            if let Some(sid) = ch.source_id {
                if let Some(src) = self.sources.iter().find(|s| s.id == sid) {
                    if let Some(ua) = &src.user_agent {
                        opts.user_agent = Some(ua.clone());
                    }
                    if let Some(r) = &src.http_referer {
                        opts.referer = Some(r.clone());
                    }
                }
            }
        }
        #[cfg(target_os = "android")]
        {
            self.android_surface_gen = crate::android_bridge::poll_surface_state().gen;
            self.android_surface_misses = 0;
        }
    }

    /// Keep MediaCodec Surface + wid alive across rotate / NativeWindow recreate.
    /// Uses live `refresh_modes` + caps; demotes punch-through if Surface stays dead.
    #[cfg(target_os = "android")]
    fn maintain_android_surface_session(&mut self, force: bool) {
        let _ = force; // resize/resume still call with true; gen gate decides rebind
        if !self.session.native.android_surface_present() {
            return;
        }
        let st = crate::android_bridge::poll_surface_state();
        if !st.ready {
            // Detach immediately (mpv-android): never keep wid on a dead Surface.
            if self.android_surface_misses == 0 {
                self.session.native.detach_android_surface();
            }
            self.android_surface_misses = self.android_surface_misses.saturating_add(1);
            crate::android_bridge::set_video_surface_visible(true);
            crate::android_bridge::set_window_punch_through(false);
            // Don't spam stabilize every tick — once per ~1s.
            if self.android_surface_misses == 1 || self.android_surface_misses % 5 == 0 {
                crate::android_bridge::stabilize_android_session();
            }
            // After ~12s of dead Surface (~60×200ms), demote to soft reopen.
            if self.android_surface_misses >= 60 {
                if let Some((wid, wh)) = crate::android_bridge::prepare_surface_present() {
                    if self.session.native.rebind_android_surface(wid, Some(wh)) {
                        self.android_surface_gen =
                            crate::android_bridge::poll_surface_state().gen;
                        self.android_surface_misses = 0;
                        tracing::info!(wid, ?wh, "android surface re-prepared after miss streak");
                        return;
                    }
                    tracing::warn!("android surface rebind failed — demoting to soft");
                } else {
                    tracing::warn!(
                        misses = self.android_surface_misses,
                        "android surface dead — demoting to soft reopen"
                    );
                }
                self.session.native.detach_android_surface();
                crate::android_bridge::release_video_surface_wid();
                crate::android_bridge::set_window_punch_through(false);
                self.session.native.force_android_soft_present();
                self.android_force_soft = true;
                self.android_surface_misses = 0;
                if let Some(ch) = self.session.channel.clone() {
                    self.player_close_gen = self.player_close_gen.wrapping_add(1);
                    self.invalidate_soft_stage(false);
                    self.apply_source_headers_for(&ch);
                    crate::android_bridge::request_audio_focus();
                    if crate::android_bridge::poll_audio_focus_held() == Some(true) {
                        self.audio_focus_held = true;
                    }
                    match self.session.open_channel(ch) {
                        Ok(()) => {
                            self.apply_saved_video_defaults_for_current_play();
                            self.status = "Surface HS — bascule soft".into();
                        }
                        Err(e) => {
                            self.status = format!("Surface HS / soft: {e}");
                        }
                    }
                }
            }
            return;
        }
        self.android_surface_misses = 0;
        let gen_changed = st.gen != 0 && st.gen != self.android_surface_gen;
        let prev_wh = self
            .session
            .native
            .options_mut()
            .android_surface_wh
            .unwrap_or((0, 0));
        let size_changed =
            st.w >= 64 && st.h >= 64 && (st.w != prev_wh.0 || st.h != prev_wh.1);
        // Size-only churn (chrome inset / BLAST) must NOT acquire a new wid GlobalRef —
        // that tears down zero-copy MediaCodec mid-play → autoconvert nv12 / "no video".
        if size_changed && !gen_changed {
            if let Some(mpv_wh) = (st.w >= 64 && st.h >= 64).then_some((st.w, st.h)) {
                let _ = self.session.native.rebind_android_surface_size(mpv_wh);
            }
            // `force` from resize/resume with same Surface gen: keep wid.
            return;
        }
        if !gen_changed {
            // force without gen bump — never replace GlobalRef (Player open resize storm).
            return;
        }
        if let Some(wid) = crate::android_bridge::acquire_video_surface_wid() {
            let wh = (st.w >= 64 && st.h >= 64).then_some((st.w, st.h));
            if self.session.native.rebind_android_surface(wid, wh) {
                self.android_surface_gen = st.gen;
            }
        }
        let caps = crate::android_bridge::poll_android_device_caps();
        let panel = caps
            .as_ref()
            .map(|c| c.refresh_hz as f32)
            .filter(|&h| h >= 24.0)
            .unwrap_or(self.display_caps.probe.monitor_hz as f32);
        let content = self.session.content_fps().unwrap_or(0.0) as f32;
        let modes = caps
            .as_ref()
            .map(|c| c.refresh_modes.as_slice())
            .unwrap_or(&[]);
        let hz = fluxplay_player::AndroidDeviceCaps::snap_present_hz_with_modes(
            content, panel, modes,
        );
        crate::android_bridge::set_video_frame_rate(hz);
    }

    fn fetch_epg_for_visible_task(&self) -> Task<Message> {
        if self.tab != Tab::Live && self.tab != Tab::Epg {
            return Task::none();
        }
        let (cols, _) = self.live_grid();
        let range = if matches!(self.tab, Tab::Live | Tab::Epg) {
            let (c, w) = self.live_grid();
            let on = self.onscreen_browse_range(c, w);
            on.start..(on.end + c).min(self.browse_index.len())
        } else {
            0..self.browse_index.len().min(cols.saturating_mul(4))
        };
        let chans: Vec<(String, Option<Uuid>)> = range
            .filter_map(|pos| {
                let i = self.browse_index.get(pos)?;
                let ch = self.bundle.channels.get(i)?;
                if self.epg_now_title(ch).is_some() {
                    return None;
                }
                Some((ch.id.clone(), ch.source_id))
            })
            .take(4)
            .collect();
        self.fetch_epg_for(chans)
    }

    /// Width of the browse pane. The metrics still reserve the category
    /// column after the tongue hides it; give that width back, minus the tongue.
    fn browse_span(&self) -> f32 {
        let m = self.layout_metrics();
        if self.shelf != Shelf::Catalog && m.cat_w > 1.0 {
            (m.content_w + m.cat_w + m.gap).max(160.0)
        } else if !self.cats_open && m.cat_w > 1.0 {
            (m.content_w + m.cat_w + m.gap - 52.0).max(160.0)
        } else {
            m.content_w
        }
    }

    fn fill_tile(&self, cols: usize) -> f32 {
        let cols_f = cols.max(1) as f32;
        let gap = crate::theme::MOSAIC_GAP;
        let usable = (self.browse_span() - 12.0).max(80.0);
        ((usable - gap * (cols_f - 1.0)) / cols_f - 4.0).max(96.0)
    }

    fn categories_folded(&self) -> bool {
        !self.cats_open && self.layout_metrics().cat_w > 1.0
    }

    /// Ten across once the category column is hidden, and on the history /
    /// favorites shelves. Fewer only when a tile would drop under 112 px.
    fn dense_grid(&self) -> (usize, f32) {
        const WANT: usize = 10;
        let usable = (self.browse_span() - 12.0).max(80.0);
        let gap = crate::theme::MOSAIC_GAP;
        let min_tile = 112.0;
        let fit = ((usable + gap) / (min_tile + gap)).floor() as usize;
        let cols = fit.clamp(1, WANT);
        (cols, self.fill_tile(cols))
    }

    /// Folded categories and library shelves: up to 10 columns. Open: 4, or
    /// fewer when the pane cannot hold them.
    fn live_grid(&self) -> (usize, f32) {
        if self.categories_folded() || self.shelf != Shelf::Catalog {
            return self.dense_grid();
        }
        let span = self.browse_span();
        let gap = crate::theme::MOSAIC_GAP;
        let cols = if span >= 4.0 * 140.0 + 3.0 * gap {
            4
        } else if span >= 2.0 * 140.0 + gap {
            2
        } else {
            1
        };
        (cols, self.fill_tile(cols).max(110.0))
    }

    /// Posters. Folded categories and library shelves use the dense grid.
    fn poster_grid(&self) -> (usize, f32) {
        if self.categories_folded() || self.shelf != Shelf::Catalog {
            self.dense_grid()
        } else {
            let m = self.layout_metrics();
            let cols = m.cols.max(1);
            (cols, m.tile_w)
        }
    }

    fn ensure_epg_now(&mut self) {
        let stale = self
            .epg_now_built
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(45));
        if stale {
            self.rebuild_epg_now();
        }
    }

    fn rebuild_epg_now(&mut self) {
        let at = chrono::Utc::now();
        let mut now_map: std::collections::HashMap<
            String,
            (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>, String),
        > = std::collections::HashMap::new();
        for p in &self.bundle.epg {
            if p.title.trim().is_empty() || p.start > at || at >= p.stop {
                continue;
            }
            now_map
                .entry(p.channel_id.clone())
                .and_modify(|cur| {
                    if p.start >= cur.0 {
                        *cur = (p.start, p.stop, p.title.clone());
                    }
                })
                .or_insert((p.start, p.stop, p.title.clone()));
        }
        let mut next_map: std::collections::HashMap<
            String,
            (chrono::DateTime<chrono::Utc>, String),
        > = std::collections::HashMap::new();
        for p in &self.bundle.epg {
            if p.title.trim().is_empty() {
                continue;
            }
            let after = now_map
                .get(&p.channel_id)
                .map(|(_, stop, _)| *stop)
                .unwrap_or(at);
            if p.start < after {
                continue;
            }
            next_map
                .entry(p.channel_id.clone())
                .and_modify(|cur| {
                    if p.start < cur.0 {
                        *cur = (p.start, p.title.clone());
                    }
                })
                .or_insert((p.start, p.title.clone()));
        }
        let clock = |t: chrono::DateTime<chrono::Utc>| {
            t.with_timezone(&Local).format("%H:%M").to_string()
        };
        self.epg_now = now_map
            .into_iter()
            .map(|(k, (start, _, title))| (k, format!("{}  {title}", clock(start))))
            .collect();
        self.epg_next = next_map
            .into_iter()
            .map(|(k, (start, title))| (k, format!("{}  {title}", clock(start))))
            .collect();
        self.epg_now_built = Some(std::time::Instant::now());
    }

    fn epg_slot<'a>(
        &'a self,
        map: &'a std::collections::HashMap<String, String>,
        ch: &fluxplay_core::models::Channel,
    ) -> Option<&'a str> {
        ch.epg_channel_id
            .as_deref()
            .and_then(|k| map.get(k))
            .or_else(|| ch.tvg_id.as_deref().and_then(|k| map.get(k)))
            .or_else(|| map.get(&ch.id))
            .map(String::as_str)
    }

    fn epg_now_title<'a>(&'a self, ch: &fluxplay_core::models::Channel) -> Option<&'a str> {
        self.epg_slot(&self.epg_now, ch)
    }

    fn epg_next_title<'a>(&'a self, ch: &fluxplay_core::models::Channel) -> Option<&'a str> {
        self.epg_slot(&self.epg_next, ch)
    }

    /// Short EPG of `(channel id, profile)` pairs, asked to the panel each channel comes from
    /// (stream ids of two panels overlap).
    fn fetch_epg_for(&self, chans: Vec<(String, Option<Uuid>)>) -> Task<Message> {
        let xtream = self.xtream_sources();
        let mut by_source: Vec<(MediaSource, Vec<String>)> = Vec::new();
        for (id, source_id) in chans {
            let src = match source_id {
                Some(sid) => xtream.iter().find(|s| s.id == sid),
                None => xtream.first(),
            };
            let Some(src) = src else {
                continue;
            };
            match by_source.iter_mut().find(|(s, _)| s.id == src.id) {
                Some((_, ids)) => ids.push(id),
                None => by_source.push((src.clone(), vec![id])),
            }
        }
        Task::batch(by_source.into_iter().map(|(src, ids)| {
            let sid = src.id;
            Task::perform(
                async move { fluxplay_providers::fetch_short_epg(&src, &ids).await },
                move |programmes| Message::EpgFetched(sid, programmes),
            )
        }))
    }

    // FLUXPLAY_AUTO_PLAY is honored in the desktop window-open path only.
    #[cfg(not(target_os = "android"))]
    fn pick_autoplay_channel(&self) -> Option<Channel> {
        let group = self.selected_group.as_deref();
        self.bundle
            .live_in_group(group)
            .into_iter()
            .find(|c| !is_adult_cat(c.group.as_deref().unwrap_or("")))
            .cloned()
            .or_else(|| self.bundle.channels.first().cloned())
    }

    /// Profile a stream URL belongs to: Xtream credentials in the path, then
    /// the catalog entry, then a server host of the profile.
    fn source_for_url(&self, url: &str) -> Option<MediaSource> {
        let enabled = || self.sources.iter().filter(|s| s.enabled);
        let by_creds = enabled().find(|s| {
            let creds = fluxplay_providers::parse_xtream_get_php(&s.endpoint)
                .map(|c| (c.username, c.password))
                .or_else(|| Some((s.username.clone()?, s.password.clone()?)));
            creds.is_some_and(|(u, p)| {
                url.contains(&format!("/{u}/{p}/"))
                    || url.contains(&format!(
                        "/{}/{}/",
                        fluxplay_providers::xtream_url::path_segment(&u),
                        fluxplay_providers::xtream_url::path_segment(&p)
                    ))
            })
        });
        if let Some(s) = by_creds {
            return Some(s.clone());
        }
        let catalog_id = self
            .bundle
            .vod
            .iter()
            .find(|v| v.stream_url == url)
            .and_then(|v| v.source_id)
            .or_else(|| {
                self.bundle
                    .channels
                    .iter()
                    .find(|c| c.stream_url == url)
                    .and_then(|c| c.source_id)
            });
        if let Some(s) = catalog_id.and_then(|id| enabled().find(|s| s.id == id)) {
            return Some(s.clone());
        }
        let host = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
        enabled()
            .find(|s| {
                s.endpoints().iter().any(|e| {
                    let e = if e.contains("://") { e.to_string() } else { format!("http://{e}") };
                    url::Url::parse(&e)
                        .ok()
                        .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case(&host)))
                        .unwrap_or(false)
                })
            })
            .cloned()
    }

    /// Rebuild the QR image only when the link text changes, so the picture
    /// keeps one iced id and the camera can read it.
    fn cache_remote_qr(&mut self) {
        let key = self.party.payload();
        if key == self.remote_qr_key {
            return;
        }
        self.remote_qr_key = key;
        let Some((side, px)) = crate::qr::rgba(&self.remote_qr_key) else {
            self.remote_qr = None;
            self.remote_qr_side = 0.0;
            return;
        };
        self.remote_qr_side = side as f32;
        self.remote_qr = Some(ImageHandle::from_rgba(side, side, px));
    }

    fn on_party_poll(&mut self) -> Task<Message> {
        let events = self.party.poll();
        let mut task = Task::none();
        for ev in events {
            match ev {
                crate::party::PartyEvent::Info(info) => {
                    self.party.info = Some(info);
                    self.party.refresh_qr();
                }
                crate::party::PartyEvent::Status(s) => {
                    if s.contains("introuvable") {
                        self.party.broadcasting = false;
                        self.session.playback_url = None;
                    }
                    self.party.status = s;
                }
                crate::party::PartyEvent::Room {
                    code,
                    people,
                    broadcasting,
                } => {
                    if !code.is_empty() {
                        self.party.room_code = code;
                    }
                    self.party.people = people;
                    self.party.broadcasting = broadcasting;
                    self.party.refresh_qr();
                }
                crate::party::PartyEvent::Remote(op) => {
                    task = self.apply_remote(op);
                }
            }
        }
        self.cache_remote_qr();
        task
    }

    fn apply_remote(&mut self, op: crate::party::RemoteOp) -> Task<Message> {
        use crate::party::RemoteOp::*;
        match op {
            Toggle => self.update(Message::TogglePause),
            Mute => self.update(Message::ToggleMute),
            Seek(secs) => self.update(Message::SeekRel(secs)),
            Volume(delta) => {
                let v = (self.session.volume + delta as f32 / 100.0).clamp(0.0, 1.0);
                self.update(Message::VolumeChanged(v))
            }
            Next => self.play_neighbor(1),
            Prev => self.play_neighbor(-1),
        }
    }

    fn play_cinema_guest(&mut self, url: &str) -> Task<Message> {
        let ch = Channel {
            id: format!("cinema-{}", self.party.room_code),
            name: format!("Salle {}", self.party.room_code),
            stream_url: url.to_string(),
            logo: None,
            group: Some("Cinéma".into()),
            tvg_id: None,
            tvg_name: None,
            tvg_logo: None,
            epg_channel_id: None,
            scheme: None,
            source_id: None,
            kind: ContentKind::Live,
            catchup: None,
        };
        self.update(Message::PlayChannel(ch))
    }

    /// Open `ch` on the fastest server of its profile; when that server
    /// refuses, the next one (profiles with mirrors only).
    fn open_with_failover(&mut self, mut ch: Channel) -> fluxplay_player::Result<()> {
        if ch.stream_url.starts_with("file:") {
            self.session.playback_url = None;
            return self.session.open_channel(ch);
        }
        if ch.stream_url.contains("/v1/cinema/live.ts") {
            self.session.playback_url = None;
            return self.session.open_channel(ch);
        }
        if self.party.broadcasting {
            let source = self.source_for_url(&ch.stream_url);
            let candidates = match &source {
                Some(s) => fluxplay_providers::servers::media_candidates(s, &ch.stream_url),
                None => vec![ch.stream_url.clone()],
            };
            let portal = candidates
                .into_iter()
                .next()
                .unwrap_or_else(|| ch.stream_url.clone());
            if portal.is_empty() {
                return Err(fluxplay_player::PlayerError::Message("URL vide".into()));
            }
            ch.stream_url = portal.clone();
            self.party.order(crate::party::Order::Broadcast(portal));
            if let Some(url) = self.party.relay_url() {
                self.session.playback_url = Some(url);
            }
            return self.session.open_channel(ch);
        }
        self.session.playback_url = None;
        let source = self.source_for_url(&ch.stream_url);
        let candidates = match &source {
            Some(s) => fluxplay_providers::servers::media_candidates(s, &ch.stream_url),
            None => vec![ch.stream_url.clone()],
        };
        let mut last = None;
        for (i, url) in candidates.iter().enumerate() {
            ch.stream_url = url.clone();
            match self.session.open_channel(ch.clone()) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if let (Some(s), true) = (&source, i + 1 < candidates.len()) {
                        tracing::warn!(error = %e, "server refused the stream — next server");
                        fluxplay_providers::servers::demote(s.id, url);
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| fluxplay_player::PlayerError::Message("URL vide".into())))
    }

    /// Enabled profiles speaking the Xtream API (category ids are per panel, so a
    /// category is asked to each of them).
    fn xtream_sources(&self) -> Vec<MediaSource> {
        self.sources
            .iter()
            .filter(|s| {
                s.enabled
                    && (s.kind == SourceKind::Xtream
                        || fluxplay_providers::parse_xtream_get_php(&s.endpoint).is_some())
            })
            .cloned()
            .collect()
    }

    fn load_vod_category_task(&self, category_id: String) -> Task<Message> {
        let srcs = self.xtream_sources();
        if srcs.is_empty() {
            return Task::none();
        }
        let cid = category_id.clone();
        Task::perform(
            async move {
                let mut items = Vec::new();
                let mut ok = Vec::new();
                let mut err = None;
                for src in &srcs {
                    match fluxplay_providers::load_xtream_vod_category(src, &cid).await {
                        Ok(mut part) => {
                            for v in &mut part {
                                v.source_id.get_or_insert(src.id);
                            }
                            items.extend(part);
                            ok.push(src.id);
                        }
                        Err(e) => err = Some(e.to_string()),
                    }
                }
                match err {
                    Some(e) if ok.is_empty() => (ok, Err(e)),
                    _ => (ok, Ok(items)),
                }
            },
            move |(sources, result)| Message::VodCategoryLoaded {
                category_id,
                sources,
                result,
            },
        )
    }

    /// Force iced scrollables back to top — prevents empty mosaic when
    /// `browse_scroll_y` was reset but the widget still sat in the spacer zone.
    fn browse_scroll_widget_id(&self) -> String {
        match self.tab {
            Tab::Vod => format!(
                "flux-vod-{}",
                self.selected_vod_category.as_deref().unwrap_or("*")
            ),
            Tab::Series if self.series_detail.is_some() => "flux-episodes".into(),
            Tab::Series => format!(
                "flux-series-{}",
                self.selected_series_category.as_deref().unwrap_or("*")
            ),
            Tab::Live => format!(
                "flux-live-{}",
                self.selected_group.as_deref().unwrap_or("*")
            ),
            Tab::Epg => format!(
                "flux-epg-{}",
                self.selected_group.as_deref().unwrap_or("*")
            ),
            Tab::Favorites => "flux-browse-fav".into(),
            _ => "flux-browse".into(),
        }
    }

    /// Room under the last row so « Voir plus » can sit at the end of the list.
    fn more_tail(&self) -> f32 {
        let shown = self.shown_count(self.browse_index.len());
        if self.shelf == Shelf::Catalog && shown < self.browse_index.len() {
            72.0
        } else {
            0.0
        }
    }

    fn browse_virtual_content_h(&self) -> f32 {
        let view_h = self.browse_view_h.max(1.0);
        match self.tab {
            Tab::Vod if self.vod_detail.is_none() => {
                let (cols, tile_w) = self.poster_grid();
                let rows = self.shown_count(self.browse_index.len()).div_ceil(cols);
                browser::virtual_content_height(rows, view_h, browser::mosaic_row_height(tile_w))
                    + self.more_tail()
            }
            Tab::Series if self.series_detail.is_none() => {
                let (cols, tile_w) = self.poster_grid();
                let rows = self.shown_count(self.browse_index.len()).div_ceil(cols);
                browser::virtual_content_height(rows, view_h, browser::mosaic_row_height(tile_w))
                    + self.more_tail()
            }
            Tab::Series if self.series_detail.is_some() => {
                let n = self
                    .series_detail
                    .as_ref()
                    .and_then(|d| {
                        let want = self.selected_season;
                        d.seasons
                            .iter()
                            .find(|s| Some(s.season_number) == want)
                            .or_else(|| d.seasons.first())
                    })
                    .map(|s| s.episodes.len())
                    .unwrap_or(0);
                // One season of tall cards, plus the synopsis and the cast under them.
                (n as f32).mul_add(280.0, 1400.0).max(view_h)
            },
            Tab::Live | Tab::Epg => {
                let (cols, _tile_w) = self.live_grid();
                let rows = self.shown_count(self.browse_index.len()).div_ceil(cols);
                browser::virtual_content_height(rows, view_h, browser::channel_row_height())
                    + self.more_tail()
            }
            Tab::Favorites => browser::virtual_content_height(
                self.browse_index.len(),
                view_h,
                browser::list_row_height(0.0),
            ),
            _ => view_h * 4.0,
        }
    }

    fn snap_browse_scroll_task(&self) -> Task<Message> {
        let id = self.browse_scroll_widget_id();
        if matches!(self.tab, Tab::Settings | Tab::Sources) {
            return Task::none();
        }
        iced::widget::operation::scroll_to(
            iced::widget::Id::from(id),
            AbsoluteOffset {
                x: None,
                y: Some(self.browse_scroll_y),
            },
        )
    }

    fn snap_cat_scroll_task(&self) -> Task<Message> {
        iced::widget::operation::scroll_to(
            iced::widget::Id::new("flux-cats"),
            AbsoluteOffset {
                x: None,
                y: Some(0.0),
            },
        )
    }

    fn load_series_category_task(&self, category_id: String) -> Task<Message> {
        let srcs = self.xtream_sources();
        if srcs.is_empty() {
            return Task::none();
        }
        let cid = category_id.clone();
        Task::perform(
            async move {
                let mut items = Vec::new();
                let mut ok = Vec::new();
                let mut err = None;
                for src in &srcs {
                    match fluxplay_providers::load_xtream_series_category(src, &cid).await {
                        Ok(mut part) => {
                            for s in &mut part {
                                s.source_id.get_or_insert(src.id);
                            }
                            items.extend(part);
                            ok.push(src.id);
                        }
                        Err(e) => err = Some(e.to_string()),
                    }
                }
                match err {
                    Some(e) if ok.is_empty() => (ok, Err(e)),
                    _ => (ok, Ok(items)),
                }
            },
            move |(sources, result)| Message::SeriesCategoryLoaded {
                category_id,
                sources,
                result,
            },
        )
    }

    /// Persist portal dump: checksum short-circuit + enrichment-preserving upsert.
    /// Progressive batch ingest is the hot path; this remains the single-source sync fallback.
    #[allow(dead_code)]
    fn ingest_source_bundle(
        &mut self,
        source_id: Uuid,
        part: PlaylistBundle,
    ) -> crate::catalog_db::BundleApplyKind {
        if let Some(db) = &mut self.catalog_db {
            let epg = part.epg.clone();
            match db.apply_source_bundle(source_id, part.clone()) {
                Ok(res) => {
                    if let Err(e) = db.merge_epg(source_id, &epg) {
                        tracing::warn!(error = %e, "catalog epg merge failed");
                    }
                    db.mark_full_sync_now(source_id);
                    match res.kind {
                        crate::catalog_db::BundleApplyKind::Unchanged => {
                            if !epg.is_empty() {
                                fluxplay_providers::merge_epg(&mut self.bundle.epg, epg);
                            }
                            return res.kind;
                        }
                        crate::catalog_db::BundleApplyKind::Updated { .. } => {
                            if let Some(merged) = res.merged {
                                replace_source_bundle(&mut self.bundle, source_id, merged);
                            }
                            return res.kind;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "catalog apply_source_bundle failed");
                    replace_source_bundle(&mut self.bundle, source_id, part);
                    return crate::catalog_db::BundleApplyKind::Updated { preserved_meta: 0 };
                }
            }
        }
        // No DB: merge enrichment from RAM then replace.
        let merged = merge_part_with_existing(&self.bundle, source_id, part);
        replace_source_bundle(&mut self.bundle, source_id, merged);
        crate::catalog_db::BundleApplyKind::Updated {
            preserved_meta: 0,
        }
    }

    /// Probe the servers of a profile with mirrors (playback and downloads
    /// then prefer the fastest reachable one).
    fn rank_servers_task(&self, id: Uuid) -> Task<Message> {
        let Some(src) = self
            .sources
            .iter()
            .find(|s| s.id == id && s.enabled && fluxplay_providers::servers::has_mirrors(s))
            .cloned()
        else {
            return Task::none();
        };
        Task::perform(
            async move {
                let r = fluxplay_providers::servers::rank(&src).await;
                (r.up.len(), r.up.len() + r.down.len())
            },
            move |(up, total)| Message::ServersRanked(id, up, total),
        )
    }

    fn clear_source_form(&mut self) {
        self.form_error = None;
        self.form_name.clear();
        self.form_endpoint.clear();
        self.form_user.clear();
        self.form_pass.clear();
        self.form_mac.clear();
        self.form_epg.clear();
        self.form_mirrors.clear();
        self.editing_source = None;
    }

    fn reload_one_task(&self, id: Uuid) -> Task<Message> {
        let Some(src) = self.sources.iter().find(|s| s.id == id).cloned() else {
            return Task::none();
        };
        Task::perform(async move { load_one(src).await.map(Arc::new) }, move |result| {
            Message::SourceLoaded {
                source_id: id,
                result,
            }
        })
    }

    fn rebuild_bundle_from_cache_task(&mut self) -> Task<Message> {
        self.load_catalog_task(self.parts_for_tab())
    }

    fn parts_for_tab(&self) -> crate::catalog_db::CatalogParts {
        match self.tab {
            Tab::Vod => crate::catalog_db::CatalogParts::vod(),
            Tab::Series => crate::catalog_db::CatalogParts::series(),
            Tab::Live | Tab::Epg | Tab::Favorites => crate::catalog_db::CatalogParts::live(),
            _ => self.catalog_parts,
        }
    }

    fn slice_missing(&self, parts: crate::catalog_db::CatalogParts) -> bool {
        (parts.channels && self.bundle.channels.is_empty())
            || (parts.vod && self.bundle.vod.is_empty())
            || (parts.series && self.bundle.series.is_empty())
    }

    fn load_catalog_task(&mut self, parts: crate::catalog_db::CatalogParts) -> Task<Message> {
        let ids: Vec<Uuid> = self.sources.iter().filter(|s| s.enabled).map(|s| s.id).collect();
        if self.catalog_db.is_none() {
            self.catalog_db = crate::catalog_db::CatalogDb::open(&ids);
        }
        self.catalog_load_gen = self.catalog_load_gen.wrapping_add(1);
        let gen = self.catalog_load_gen;
        self.catalog_parts = parts;
        self.loading = true;
        self.status = if parts.series {
            "Chargement des séries…".into()
        } else if parts.vod {
            "Chargement des films…".into()
        } else {
            "Chargement des chaînes…".into()
        };
        Task::perform(
            async move {
                crate::async_jobs::run_blocking(move || {
                    crate::catalog_db::load_bundle_parts_blocking(&ids, parts)
                })
                .await
                .unwrap_or_else(Err)
            },
            move |result| Message::BundleCacheReady { gen, parts, result },
        )
    }

    /// True when an enabled source has never fully synced, or that sync is older than `secs`.
    fn catalog_sync_stale(&self, secs: u64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let Some(db) = &self.catalog_db else {
            return true;
        };
        self.sources.iter().filter(|s| s.enabled).any(|s| {
            match db.last_full_sync_secs(s.id) {
                None => true,
                Some(ts) => now.saturating_sub(ts) >= secs,
            }
        })
    }

    /// Films and series lists: pull the portal when the last sync is older than 10 minutes.
    fn catalog_refresh_if_due(&mut self) -> Option<Task<Message>> {
        if self.loading {
            return None;
        }
        let playing = matches!(
            self.session.state,
            PlaybackState::Opening
                | PlaybackState::Playing
                | PlaybackState::Buffering
                | PlaybackState::Paused
        );
        if playing || !fluxplay_providers::api_cache::portal_quiet_remaining().is_zero() {
            return None;
        }
        // Only the sources actually enabled. A leftover profile folder must not
        // keep the portal downloading forever (one connection, so playback dies).
        if !self.catalog_sync_stale(10 * 60) {
            return None;
        }
        self.loading = true;
        self.catalog_sync_not_before =
            std::time::Instant::now() + std::time::Duration::from_secs(180);
        self.status = "Actualisation des nouveautés…".into();
        Some(self.reload_all_task())
    }

    fn nudge_frozen_playback(&mut self) {
        let live = self
            .session
            .channel
            .as_ref()
            .is_some_and(|c| c.kind == ContentKind::Live);
        let running = matches!(
            self.session.state,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        if !running || live || self.session.position_secs < 1.0 {
            self.play_stall_at = None;
            self.play_stall_kicks = 0;
            return;
        }
        let pos = self.session.position_secs;
        match self.play_stall_at {
            Some((prev, at)) if (pos - prev).abs() < 0.35 => {
                if at.elapsed() >= std::time::Duration::from_secs(18) && self.play_stall_kicks < 2
                {
                    self.play_stall_kicks = self.play_stall_kicks.saturating_add(1);
                    self.play_stall_at = Some((pos, std::time::Instant::now()));
                    tracing::info!(pos, kicks = self.play_stall_kicks, "playback frozen, nudging");
                    if self.play_stall_kicks >= 2 {
                        self.session.restart();
                        self.session.seek_absolute(pos);
                    } else {
                        self.session.wake_audio();
                        self.session.seek_absolute((pos + 0.4).max(0.0));
                    }
                    self.status = "Image figée — reprise".into();
                }
            }
            _ => {
                self.play_stall_at = Some((pos, std::time::Instant::now()));
                self.play_stall_kicks = 0;
            }
        }
    }

    fn push_notice(&mut self, text: String) {
        self.notice_seq = self.notice_seq.wrapping_add(1);
        self.notices.push(Notice {
            id: self.notice_seq,
            text,
            until: std::time::Instant::now() + std::time::Duration::from_secs(8),
        });
        if self.notices.len() > 4 {
            let extra = self.notices.len() - 4;
            self.notices.drain(0..extra);
        }
    }

    fn consider_episode_alert(&mut self, item: &SeriesItem) {
        let count = item
            .seasons
            .iter()
            .map(|s| s.episodes.len() as u32)
            .sum::<u32>();
        if count == 0 {
            return;
        }
        let Some(idx) = self
            .settings
            .episode_alerts
            .iter()
            .position(|a| a.series_id == item.id)
        else {
            return;
        };
        let prev = self.settings.episode_alerts[idx].episodes;
        if count == prev {
            return;
        }
        let name = crate::names::display_title(&self.settings.episode_alerts[idx].name);
        self.settings.episode_alerts[idx].episodes = count;
        self.persist();
        if count > prev {
            let extra = count - prev;
            let text = if extra == 1 {
                format!("{name} · un nouvel épisode")
            } else {
                format!("{name} · {extra} nouveaux épisodes")
            };
            self.push_notice(text);
        }
    }

    /// One watched series per catalog check, and never while a stream is open.
    fn episode_alert_task(&mut self) -> Task<Message> {
        let playing = matches!(
            self.session.state,
            PlaybackState::Opening
                | PlaybackState::Playing
                | PlaybackState::Buffering
                | PlaybackState::Paused
        );
        if playing {
            return Task::none();
        }
        let Some(alert) = self
            .settings
            .episode_alerts
            .iter()
            .find(|a| !self.alert_checked.contains(&a.series_id))
            .cloned()
        else {
            return Task::none();
        };
        self.alert_checked.insert(alert.series_id.clone());
        let Some(series) = self
            .bundle
            .series
            .iter()
            .find(|s| s.id == alert.series_id)
        else {
            return Task::none();
        };
        let Some(src) = series
            .source_id
            .and_then(|id| self.sources.iter().find(|s| s.id == id))
            .cloned()
        else {
            return Task::none();
        };
        let id = series.id.clone();
        Task::perform(
            async move {
                fluxplay_providers::load_xtream_series_info(&src, &id)
                    .await
                    .map_err(|e| e.to_string())
            },
            Message::SeriesDetailLoaded,
        )
    }

    fn notice_layer(&self, _ui: UiTheme) -> Element<'_, Message> {
        let mut col = Column::new().spacing(8);
        for notice in &self.notices {
            let id = notice.id;
            let card = container(
                row![
                    text(notice.text.clone())
                        .size(13)
                        .color(Color::WHITE)
                        .width(Length::Fixed(240.0)),
                    mouse_area(
                        container(crate::icons::icon(crate::icons::Icon::Close, 16.0, Color::WHITE))
                            .padding(4),
                    )
                    .on_press(Message::DismissNotice(id)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .width(Length::Fixed(300.0))
            .padding(Padding::from([10, 12]))
            .style(|_t: &Theme| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.08, 0.08, 0.1, 0.94))),
                border: Border {
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.28),
                    width: 1.0,
                    radius: 14.0.into(),
                },
                ..Default::default()
            });
            col = col.push(card);
        }
        container(col).padding(16).into()
    }

    fn with_notices<'a>(
        &'a self,
        page: Element<'a, Message>,
        ui: UiTheme,
    ) -> Element<'a, Message> {
        if self.notices.is_empty() {
            page
        } else {
            stack![page, self.notice_layer(ui)]
                .width(Fill)
                .height(Fill)
                .into()
        }
    }

    fn recover_after_sleep(&mut self) -> Task<Message> {
        tracing::info!("resume after system sleep");
        // Do not loadfile here. The network and the audio device are not back
        // yet, and replacing the file freezes the picture on the last frame.
        let paused = matches!(self.session.state, PlaybackState::Paused);
        if self.session.channel.is_some() {
            self.wake_pos = Some(self.session.position_secs);
            self.session.wake_output(paused);
            self.status = if paused {
                "En pause".into()
            } else {
                self.session.status_line()
            };
        } else {
            self.wake_pos = None;
            self.status = "Reprise après veille".into();
        }
        Task::perform(
            async {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            },
            |_| Message::WakeAudio,
        )
    }

    fn apply_bundle_cache(
        &mut self,
        bundle: PlaylistBundle,
        parts: crate::catalog_db::CatalogParts,
    ) {
        if !bundle.categories.is_empty() {
            self.bundle.categories = bundle.categories;
        }
        if parts.channels {
            self.bundle.channels = bundle.channels;
            self.bundle.vod.clear();
            self.bundle.series.clear();
        }
        if parts.vod {
            self.bundle.vod = bundle.vod;
            self.bundle.vod.sort_by(|a, b| {
                b.added.unwrap_or(0).cmp(&a.added.unwrap_or(0))
            });
            self.bundle.series.clear();
            self.bundle.channels.clear();
        }
        if parts.series {
            self.bundle.series = bundle.series;
            self.bundle.series.sort_by(|a, b| {
                b.added.unwrap_or(0).cmp(&a.added.unwrap_or(0))
            });
            self.bundle.vod.clear();
            self.bundle.channels.clear();
        }
        if parts.epg {
            self.bundle.epg = bundle.epg;
        } else if parts.vod || parts.series {
            self.bundle.epg.clear();
        }
        self.vod_shelf_by_title.clear();
        self.rebuild_epg_now();
        tracing::info!(
            channels = self.bundle.channels.len(),
            vod = self.bundle.vod.len(),
            series = self.bundle.series.len(),
            "rebuilt bundle from catalog db"
        );
    }

    /// The episode ran to the credits (not a failed open). Used to start the next one.
    fn series_episode_finished(&self) -> bool {
        if self.session.loop_file {
            return false;
        }
        let Some(ch) = self.session.channel.as_ref() else {
            return false;
        };
        if ch.kind != ContentKind::Series {
            return false;
        }
        let dur = self.session.duration_secs;
        let pos = self.session.position_secs;
        if dur > 20.0 {
            return pos + 15.0 >= dur || (dur > 0.0 && pos / dur >= 0.90);
        }
        let saw_picture = self.video_frame.is_some() || self.stage_picture;
        saw_picture && pos >= 45.0
    }

    /// One history row per series. Films stay one row per title.
    fn remember_play(&mut self, ch: &Channel, catalog_url: &str) {
        if ch.kind == ContentKind::Series {
            self.note_series_history(&ch.name, catalog_url, ch.logo.clone());
        } else if ch.kind != ContentKind::Live {
            let mut saved = ch.clone();
            saved.stream_url = catalog_url.to_string();
            self.settings.push_recent(&saved);
        }
    }

    fn note_series_history(&mut self, playing: &str, episode_url: &str, fallback: Option<String>) {
        let key = fluxplay_core::models::series_history_key(playing);
        let from_open = self.series_detail.as_ref().and_then(|d| {
            let hit = fluxplay_core::models::series_history_key(&d.name) == key
                || d.seasons.iter().any(|s| {
                    s.episodes.iter().any(|e| e.stream_url == episode_url)
                });
            hit.then(|| {
                (
                    d.id.clone(),
                    d.name.clone(),
                    d.cover.clone().or(d.banner.clone()),
                )
            })
        });
        let (id, series_name, poster) = if let Some((id, name, poster)) = from_open {
            (id, name, poster.or(fallback))
        } else if let Some(s) = self.series_matches(playing).into_iter().next() {
            (
                s.id,
                s.name,
                s.cover.or(s.banner).or(fallback),
            )
        } else {
            let head = playing
                .split(" — ")
                .next()
                .unwrap_or(playing)
                .split(" – ")
                .next()
                .unwrap_or(playing)
                .trim()
                .to_string();
            (
                fluxplay_core::models::series_history_key(&head),
                head,
                fallback,
            )
        };
        let note = self.episode_code(&id, episode_url);
        self.settings
            .push_series_recent(&id, &series_name, episode_url, poster, note);
    }

    fn episode_code(&self, series_id: &str, episode_url: &str) -> Option<String> {
        let code = |item: &SeriesItem| -> Option<String> {
            for season in &item.seasons {
                for ep in &season.episodes {
                    if ep.stream_url == episode_url {
                        return Some(format!(
                            "S{:02}E{:02}",
                            season.season_number, ep.episode_num
                        ));
                    }
                }
            }
            None
        };
        if let Some(d) = &self.series_detail {
            if let Some(found) = code(d) {
                return Some(found);
            }
        }
        let raw = self.catalog_db.as_ref()?.payload_by_id("series", series_id)?;
        code(&serde_json::from_str::<SeriesItem>(&raw).ok()?)
    }

    fn store_watch_progress(&mut self, force: bool) {
        if !self.settings.remember_position {
            return;
        }
        if self.resume_at.is_some() && !force {
            return;
        }
        let Some(key) = self.watch_key.clone() else {
            return;
        };
        let pos = self.session.position_secs;
        let dur = self.session.duration_secs;
        if pos < 8.0 {
            return;
        }
        if !force
            && self
                .watch_saved_at
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(12))
        {
            return;
        }
        if let Some(prev) = self.settings.watch_get(&key) {
            let moved = (prev.position_secs - pos).abs() >= 4.0
                || (dur > 1.0 && prev.duration_secs < 1.0);
            if !moved && (!force || (prev.position_secs - pos).abs() < 1.5) {
                return;
            }
        }
        self.settings.note_watch(&key, pos, dur);
        self.watch_saved_at = Some(std::time::Instant::now());
        self.persist();
    }

    fn arm_resume(&mut self, catalog_url: &str, kind: ContentKind) {
        self.resume_at = None;
        self.resume_not_before = None;
        self.resume_until = None;
        self.watch_saved_at = None;
        if kind == ContentKind::Live || catalog_url.is_empty() {
            self.watch_key = None;
            return;
        }
        self.watch_key = Some(catalog_url.to_string());
        if !self.settings.remember_position {
            return;
        }
        let Some(w) = self.settings.watch_get(catalog_url) else {
            return;
        };
        let position = w.position_secs;
        let duration = w.duration_secs;
        if position < 20.0 {
            return;
        }
        if duration > 20.0 {
            let remain = duration - position;
            if remain < 30.0 || position / duration >= 0.95 {
                return;
            }
        }
        let now = std::time::Instant::now();
        self.resume_at = Some(position);
        self.resume_not_before = Some(now + std::time::Duration::from_millis(700));
        self.resume_until = Some(now + std::time::Duration::from_secs(12));
    }

    fn try_resume_seek(&mut self) {
        let Some(at) = self.resume_at else {
            return;
        };
        let now = std::time::Instant::now();
        if self.resume_until.is_some_and(|t| now > t) {
            self.resume_at = None;
            return;
        }
        if self.resume_not_before.is_some_and(|t| now < t) {
            return;
        }
        if !matches!(
            self.session.state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Paused
        ) {
            return;
        }
        let dur = self.session.duration_secs;
        let pos = self.session.position_secs;
        if dur <= 1.0 && pos < 0.5 {
            return;
        }
        let target = if dur > 20.0 {
            at.min((dur - 8.0).max(0.0))
        } else {
            at
        };
        if target < 8.0 {
            self.resume_at = None;
            return;
        }
        if (pos - target).abs() < 4.0 {
            self.resume_at = None;
            return;
        }
        self.session.seek_absolute(target);
        self.resume_not_before = Some(now + std::time::Duration::from_millis(900));
    }

    fn focus_resume_season(&mut self) {
        if !self.resume_season_pending {
            return;
        }
        let Some(detail) = self.series_detail.clone() else {
            return;
        };
        if detail.seasons.is_empty() {
            return;
        }
        self.resume_season_pending = false;
        let Some(target) = resume_target(&self.settings, &detail) else {
            return;
        };
        if self.selected_season != Some(target.season) {
            self.selected_season = Some(target.season);
            self.season_menu_open = false;
        }
    }

    fn history_series_id(&self, channel_id: &str, name: &str) -> Option<String> {
        if self.bundle.series.iter().any(|s| s.id == channel_id) {
            return Some(channel_id.to_string());
        }
        let key = fluxplay_core::models::series_history_key(name);
        self.bundle
            .series
            .iter()
            .find(|s| fluxplay_core::models::series_history_key(&s.name) == key)
            .map(|s| s.id.clone())
    }

    /// Title, subtitle, click, poster. A series opens its page instead of one episode.
    fn history_presentation(
        &self,
        r: &fluxplay_core::models::RecentChannel,
    ) -> (String, String, Message, Option<String>) {
        let kind = r.media_kind();
        let series_id = if kind == ContentKind::Series {
            self.history_series_id(&r.channel_id, &r.name)
        } else {
            None
        };
        let title = if kind == ContentKind::Series {
            let head = r.name.split(" — ").next().unwrap_or(r.name.as_str());
            let head = head.split(" – ").next().unwrap_or(head).trim();
            crate::names::display_title(head)
        } else {
            crate::names::display_title(&r.name)
        };
        let meta = match kind {
            ContentKind::Series => r.note.clone().unwrap_or_else(|| "Série".into()),
            ContentKind::Vod => "Film".into(),
            ContentKind::Live => "Direct".into(),
        };
        let art = series_id
            .as_ref()
            .and_then(|id| {
                self.bundle.series.iter().find(|s| &s.id == id).and_then(|s| {
                    s.cover
                        .clone()
                        .filter(|u| !u.is_empty())
                        .or(s.banner.clone().filter(|u| !u.is_empty()))
                })
            })
            .or_else(|| r.logo.clone().filter(|s| !s.is_empty()));
        let open = if let Some(id) = series_id {
            Message::OpenSeries(id)
        } else {
            Message::PlayChannel(Channel {
                id: r.channel_id.clone(),
                name: r.name.clone(),
                stream_url: r.stream_url.clone(),
                logo: art.clone(),
                group: r.group.clone(),
                tvg_id: None,
                tvg_name: None,
                tvg_logo: art.clone(),
                epg_channel_id: None,
                scheme: None,
                source_id: None,
                kind,
                catchup: None,
            })
        };
        (title, meta, open, art)
    }

    /// Keep the season menu on the episode that just started.
    fn follow_playing_season(&mut self, url: &str) {
        let Some(detail) = &self.series_detail else {
            return;
        };
        let Some(n) = detail.seasons.iter().find_map(|s| {
            s.episodes
                .iter()
                .any(|e| e.stream_url == url)
                .then_some(s.season_number)
        }) else {
            return;
        };
        if self.selected_season != Some(n) {
            self.selected_season = Some(n);
            self.season_menu_open = false;
        }
    }

    fn clear_series_queue(&mut self) {
        self.series_queue.clear();
        self.series_queue_idx = 0;
        self.series_neighbor_pending = None;
        self.series_queue_loading = None;
    }

    /// Build the episode list for what just started. History, favorites resume,
    /// and a closed series page have no open detail, so the list is read from
    /// the catalog. One portal request only when that copy has no episodes yet.
    fn arm_series_queue(&mut self, current_url: &str, title: &str) -> Option<Task<Message>> {
        if let Some(i) = self.series_queue.iter().position(|(_, u)| u == current_url) {
            self.series_queue_idx = i;
            return None;
        }
        if let Some(detail) = self.series_detail.clone() {
            if let Some((queue, idx)) = queue_from_series(&detail, current_url, title) {
                self.series_queue = queue;
                self.series_queue_idx = idx;
                return None;
            }
        }
        if let Some((queue, idx)) = self.queue_from_catalog(current_url, title) {
            self.series_queue = queue;
            self.series_queue_idx = idx;
            return None;
        }
        self.series_queue.clear();
        self.series_queue_idx = 0;
        self.fetch_series_queue(current_url, title)
    }

    fn queue_from_catalog(&self, current_url: &str, title: &str) -> Option<(Vec<(String, String)>, usize)> {
        for series in self.series_matches(title) {
            let Some(raw) = self
                .catalog_db
                .as_ref()
                .and_then(|db| db.payload_by_id("series", &series.id))
            else {
                continue;
            };
            let Ok(full) = serde_json::from_str::<SeriesItem>(&raw) else {
                continue;
            };
            if let Some(queue) = queue_from_series(&full, current_url, title) {
                return Some(queue);
            }
        }
        None
    }

    /// Longest catalog title that is the start of the playing name
    /// (`Série — épisode`).
    fn series_matches(&self, playing: &str) -> Vec<SeriesItem> {
        let mut best = 0usize;
        let mut out = Vec::new();
        for series in &self.bundle.series {
            let shown = crate::names::display_title(&series.name);
            let len = if title_opens_series(&series.name, playing) {
                series.name.len()
            } else if title_opens_series(&shown, playing) {
                shown.len()
            } else {
                continue;
            };
            if len > best {
                best = len;
                out.clear();
            }
            if len == best {
                out.push(series.clone());
            }
        }
        out
    }

    fn fetch_series_queue(&mut self, current_url: &str, title: &str) -> Option<Task<Message>> {
        if self.series_queue_loading.as_deref() == Some(current_url) {
            return None;
        }
        let matches = self.series_matches(title);
        let prefer = self.source_for_url(current_url).map(|s| s.id);
        let series = matches
            .iter()
            .find(|s| prefer.is_some_and(|id| s.source_id == Some(id)))
            .cloned()
            .or_else(|| matches.into_iter().next())?;
        let wanted = series.source_id;
        let src = self
            .sources
            .iter()
            .find(|s| s.enabled && wanted.map(|id| id == s.id).unwrap_or(false))
            .cloned()
            .or_else(|| {
                self.sources.iter().find(|s| {
                    s.enabled
                        && (s.kind == SourceKind::Xtream
                            || fluxplay_providers::parse_xtream_get_php(&s.endpoint).is_some())
                }).cloned()
            })?;
        let id = series.id.clone();
        let for_url = current_url.to_string();
        let title = title.to_string();
        self.series_queue_loading = Some(for_url.clone());
        Some(Task::perform(
            async move {
                fluxplay_providers::load_xtream_series_info(&src, &id)
                    .await
                    .map_err(|e| e.to_string())
            },
            move |result| Message::SeriesQueueReady {
                for_url: for_url.clone(),
                title: title.clone(),
                result,
            },
        ))
    }

    /// Previous / next. A series started from history has no list until the
    /// catalog (or one series request) answers; the click is kept and played then.
    fn play_neighbor(&mut self, delta: i32) -> Task<Message> {
        let series = self.session.channel.as_ref().map(|c| c.kind) == Some(ContentKind::Series);
        if series {
            if let Some(ch) = self.session.channel.clone() {
                let known = self.series_queue.iter().any(|(_, url)| url == &ch.stream_url);
                if !known {
                    let loading = if delta < 0 {
                        "Chargement de l'épisode précédent…"
                    } else {
                        "Chargement de l'épisode suivant…"
                    };
                    match self.arm_series_queue(&ch.stream_url, &ch.name) {
                        Some(task) => {
                            self.series_neighbor_pending = Some(delta);
                            self.status = loading.into();
                            return task;
                        }
                        None if self.series_queue_loading.is_some() => {
                            self.series_neighbor_pending = Some(delta);
                            self.status = loading.into();
                            return Task::none();
                        }
                        None => {}
                    }
                }
            }
        }
        if let Some(msg) = self.playlist_neighbor(delta) {
            self.series_neighbor_pending = None;
            return Task::done(msg);
        }
        if series {
            self.status = if delta < 0 {
                "Pas d'épisode précédent".into()
            } else {
                "Pas d'épisode suivant".into()
            };
        }
        Task::none()
    }

    fn maybe_prefetch_next_episode(&mut self) -> Option<Task<Message>> {
        if !self.settings.prefetch_next_episode {
            return None;
        }
        if self.session.channel.as_ref().map(|c| c.kind) != Some(ContentKind::Series) {
            return None;
        }
        let progress = self.session.progress_ratio();
        if progress < 0.75 {
            return None;
        }
        let cur = self.session.channel.as_ref()?.stream_url.clone();
        if self.prefetch_armed_for.as_deref() == Some(cur.as_str()) {
            return None;
        }
        let next = self.series_queue.get(self.series_queue_idx + 1)?.1.clone();
        self.prefetch_armed_for = Some(cur);
        if self.download_library.contains(&next)
            || self.prefetched.as_ref().is_some_and(|(u, _)| *u == next)
            || self.prefetch_job.as_ref().is_some_and(|(u, _)| *u == next)
        {
            return None;
        }
        if let Some((_, handle)) = self.prefetch_job.take() {
            handle.abort();
        }
        // One preloaded episode at a time: drop the previous one.
        self.prefetched = None;
        let dir = prefetch_dir();
        let _ = std::fs::remove_dir_all(&dir);
        let ua = play_options_from(&self.settings)
            .user_agent
            .unwrap_or_else(|| "IPTVSmartersPlayer".into());
        let req = crate::downloads::DownloadRequest {
            name: format!("episode-{}", &crate::downloads::url_key(&next)[..12]),
            url: next.clone(),
            source: self.source_for_url(&next),
            user_agent: ua,
            dir,
            background: true,
        };
        let key = next.clone();
        let (task, handle) = Task::run(crate::downloads::run(req), move |event| {
            Message::PrefetchEvent {
                url: key.clone(),
                event,
            }
        })
        .abortable();
        self.prefetch_job = Some((next, handle));
        Some(task)
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        let view_name = {
            #[cfg(target_os = "android")]
            {
                let _ = id;
                if self.player_embedded {
                    "player"
                } else {
                    "browser"
                }
            }
            #[cfg(not(target_os = "android"))]
            {
                if self.player_id == Some(id) {
                    "player"
                } else {
                    "browser"
                }
            }
        };
        let _prof = fluxplay_core::InteractionGuard::begin_view(view_name);
        #[cfg(target_os = "android")]
        {
            if self.player_embedded {
                return self.view_player_window();
            }
            return self.view_browser();
        }
        #[cfg(not(target_os = "android"))]
        {
            if self.player_id == Some(id) {
                return self.view_player_window();
            }
            if self.browse_parked {
                return self.view_parked_browser();
            }
            self.view_browser()
        }
    }

    fn view_remote(&self, ui: UiTheme) -> Element<'_, Message> {
        let qr: Element<'_, Message> = if let Some(handle) = self.remote_qr.clone() {
            let side = self.remote_qr_side;
            container(
                iced_image::Image::new(handle)
                    .filter_method(FilterMethod::Nearest)
                    .width(Length::Fixed(side))
                    .height(Length::Fixed(side)),
            )
            .padding(12)
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
        let payload = self.party.payload();
        let link: Element<'_, Message> = if payload.is_empty() {
            Space::new().height(0).into()
        } else {
            text(payload).size(11).color(ui.ink_muted()).into()
        };
        let controls: Element<'_, Message> = if self.party.paired.is_some() {
            column![
                text("Associée").size(12).color(ui.ink_muted()),
                row![
                    pill_button(
                        text("Lecture"),
                        Message::PartyRemote(crate::party::RemoteOp::Toggle),
                        ui,
                        false,
                    ),
                    pill_button(
                        text("−10 s"),
                        Message::PartyRemote(crate::party::RemoteOp::Seek(-10)),
                        ui,
                        false,
                    ),
                    pill_button(
                        text("+10 s"),
                        Message::PartyRemote(crate::party::RemoteOp::Seek(10)),
                        ui,
                        false,
                    ),
                    pill_button(
                        text("Muet"),
                        Message::PartyRemote(crate::party::RemoteOp::Mute),
                        ui,
                        false,
                    ),
                ]
                .spacing(8)
                .wrap(),
                row![
                    pill_button(
                        text("Précédent"),
                        Message::PartyRemote(crate::party::RemoteOp::Prev),
                        ui,
                        false,
                    ),
                    pill_button(
                        text("Suivant"),
                        Message::PartyRemote(crate::party::RemoteOp::Next),
                        ui,
                        false,
                    ),
                    pill_button(
                        text("Volume −"),
                        Message::PartyRemote(crate::party::RemoteOp::Volume(-8)),
                        ui,
                        false,
                    ),
                    pill_button(
                        text("Volume +"),
                        Message::PartyRemote(crate::party::RemoteOp::Volume(8)),
                        ui,
                        false,
                    ),
                ]
                .spacing(8)
                .wrap(),
            ]
            .spacing(8)
            .width(Fill)
            .into()
        } else {
            Space::new().height(0).into()
        };
        settings_card(
            ui,
            column![
                qr,
                link,
                row![
                    pill_button(text("Copier le lien"), Message::PartyCopy, ui, false),
                    pill_button_primary(
                        text("Associer"),
                        Message::PartyPair(self.party.draft.clone()),
                        ui,
                    ),
                ]
                .spacing(8)
                .wrap(),
                field(
                    "Coller le lien fp1|…",
                    &self.party.draft,
                    Message::PartyDraft,
                    Some(PasteTarget::PartyLink),
                    ui,
                ),
                controls,
                text(self.party.status.clone())
                    .size(12)
                    .color(ui.ink_muted()),
            ]
            .spacing(8)
            .width(Fill),
        )
    }

    fn view_cinema(&self, ui: UiTheme) -> Element<'_, Message> {
        let room = if self.party.room_code.is_empty() {
            "Aucune salle".to_string()
        } else {
            format!(
                "Salle {} · {} / 10",
                self.party.room_code,
                self.party.people.max(1)
            )
        };
        let quality = self.party.quality;
        let body = column![
            column![
                text("Salle de cinéma").size(16).color(ui.ink()),
                text("L'hôte diffuse sa lecture en direct, de 2 à 10 personnes. 1080p de son côté. Chaque invité choisit 720p ou 480p.")
                    .size(12)
                    .color(ui.ink_muted()),
            ]
            .spacing(2)
            .width(Fill),
            settings_card(
                ui,
                column![
                    text(room).size(14).color(ui.ink()),
                    row![
                        pill_button(
                            text("Créer"),
                            Message::PartyCreate,
                            ui,
                            self.party.owner && !self.party.room_code.is_empty(),
                        ),
                        pill_button(
                            text("Diffuser"),
                            Message::PartyBroadcast,
                            ui,
                            self.party.broadcasting,
                        ),
                        pill_button(text("Arrêter"), Message::PartyStop, ui, false),
                        pill_button(text("Quitter"), Message::PartyLeave, ui, false),
                    ]
                    .spacing(8)
                    .wrap(),
                    field(
                        "Code de la salle",
                        &self.party.join_draft,
                        Message::PartyJoinDraft,
                        None,
                        ui,
                    ),
                    pill_button_primary(text("Rejoindre"), Message::PartyJoin, ui),
                ]
                .spacing(8)
                .width(Fill),
            ),
            settings_card(
                ui,
                column![
                    text("Qualité reçue").size(12).color(ui.ink_muted()),
                    row![
                        pill_button(text("1080p"), Message::PartyQuality(0), ui, quality == 0),
                        pill_button(text("720p"), Message::PartyQuality(720), ui, quality == 720),
                        pill_button(text("480p"), Message::PartyQuality(480), ui, quality == 480),
                    ]
                    .spacing(8)
                    .wrap(),
                ]
                .spacing(8)
                .width(Fill),
            ),
            text(self.party.status.clone())
                .size(12)
                .color(ui.ink_muted()),
        ]
        .spacing(10)
        .padding(Padding::new(16.0))
        .width(Fill);
        iced::widget::scrollable(body).width(Fill).height(Fill).into()
    }

    fn needs_api_prompt(&self) -> bool {
        !self.settings.tmdb_prompt_done && !crate::metadata::tmdb_configured()
    }

    fn view_browser(&self) -> Element<'_, Message> {
        let ui = self.ui_theme();
        let m = self.layout_metrics();
        let tab_items = Tab::all().iter().map(|t| {
            let label = if m.top_nav {
                t.short_label()
            } else {
                t.label()
            };
            (t.icon(), label, Message::Tab(*t), self.tab == *t)
        });

        let body = match self.tab {
            Tab::Live => self.view_channel_grid(ui, false),
            Tab::Vod => self.view_browse_vod(ui),
            Tab::Series => self.view_browse_series(ui),
            Tab::Favorites => self.view_favorites(ui),
            Tab::Epg => self.view_channel_grid(ui, true),
            Tab::Cinema => self.view_cinema(ui),
            Tab::Sources | Tab::Settings => self.view_settings(ui),
        };

        let main_col: Element<'_, Message> = if m.top_nav || m.rail_w <= 1.0 {
            // Compact: content, then the nav (last, above the system gesture bar).
            column![
                container(body)
                    .width(Fill)
                    .height(Fill)
                    .align_x(Alignment::Start)
                    .align_y(Alignment::Start)
                    .clip(true),
                container({
                    let nav = if m.nav_strip {
                        browser::mode_top_nav_ex(ui, m.rail_size, true, tab_items)
                    } else {
                        browser::mode_top_nav(ui, m.rail_size, tab_items)
                    };
                    nav
                })
                .width(Fill)
                .height(Length::Shrink),
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill)
            .into()
        } else {
            row![
                browser::mode_rail(ui, m.rail_w, m.rail_size, tab_items, self.rail_subs()),
                container(body)
                    .width(Fill)
                    .height(Fill)
                    .align_x(Alignment::Start)
                    .align_y(Alignment::Start)
                    .clip(true),
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill)
            .align_y(Alignment::Start)
            .into()
        };

        // Android: dynamic WindowInsets (gesture nav may report 0 — do not invent 48dp).
        #[cfg(target_os = "android")]
        let (top_inset, bottom_inset) = (self.system_insets.1, self.system_insets.3);
        #[cfg(not(target_os = "android"))]
        let (top_inset, bottom_inset) = (0.0_f32, 0.0_f32);
        let page = container(main_col)
            .padding(Padding {
                top: m.pad + top_inset,
                right: m.pad + {
                    #[cfg(target_os = "android")]
                    {
                        self.system_insets.2
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        0.0
                    }
                },
                bottom: m.pad + bottom_inset,
                left: m.pad + {
                    #[cfg(target_os = "android")]
                    {
                        self.system_insets.0
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        0.0
                    }
                },
            })
            .width(Fill)
            .height(Fill)
            .align_x(Alignment::Start)
            .align_y(Alignment::Start)
            .clip(true)
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(browser::shell_background(ui))),
                ..Default::default()
            });
        let page = if self.needs_api_prompt() {
            stack![page, api_prompt(ui)].width(Fill).height(Fill).into()
        } else {
            page.into()
        };
        self.with_notices(page, ui)
    }

    fn view_player_window(&self) -> Element<'_, Message> {
        // Menus follow the chosen text color; the player keeps its own ink.
        let ui = self.ui_theme().with_ink(None);
        let title = self
            .session
            .channel
            .as_ref()
            .map(|c| c.name.as_str())
            .unwrap_or("FluxPlay Lecteur");
        let meta = self
            .session
            .channel
            .as_ref()
            .and_then(|c| c.group.as_deref())
            .unwrap_or("—");
        let art = self.session.channel.as_ref().and_then(|c| {
            crate::images::pick_art(
                c.logo.as_deref().or(c.tvg_logo.as_deref()),
                None,
                None,
                None,
            )
            .and_then(|u| self.images.get(&u))
        });
        let active = !matches!(self.session.state, PlaybackState::Idle)
            || self.session.channel.is_some();
        let embedded_video = self.session.has_embedded_video();
        let surface_video = self.session.native.android_surface_present();
        let backend_label = self.session.backend_display_label();
        let caps = self.session.caps();

        let player = player_ui::player_window(player_ui::PlayerChrome {
            seek_drag: self.seek_drag,
            ui,
            title,
            meta,
            status: &self.status,
            session: &self.session,
            art,
            video: if surface_video {
                None
            } else {
                self.video_frame.as_ref()
            },
            active,
            panel: self.player_panel,
            goto_draft: &self.goto_draft,
            sleep_mins: self.sleep_mins,
            pip: self.pip_mode,
            chrome_h: self.layout_metrics().player_chrome_h,
            chrome_visible: self.player_chrome_visible,
            chrome_alpha: self.chrome_alpha,
            fullscreen: self.player_fullscreen,
            exit_ask: self.player_exit_ask,
            embedded_video,
            surface_video,
            stage_picture: self.stage_picture,
            backend_label,
            caps,
            quality: self.settings.video_quality,
            skip_auto: self.settings.skip_credits_auto,
            remote_on: self.party.paired.is_some(),
            remote_qr: self.remote_qr.as_ref(),
            remote_qr_side: self.remote_qr_side,
            remote_link: &self.remote_qr_key,
            remote_draft: &self.party.draft,
            remote_status: &self.party.status,
            #[cfg(target_os = "android")]
            safe: if self.pip_mode {
                Padding::ZERO
            } else {
                Padding {
                    top: self.system_insets.1,
                    right: self.system_insets.2,
                    bottom: self.system_insets.3,
                    left: self.system_insets.0,
                }
            },
            #[cfg(not(target_os = "android"))]
            safe: Padding::ZERO,
        });
        self.with_notices(player, ui)
    }

    fn maybe_autohide_player_chrome(&mut self) {
        if self.player_panel != PlayerPanel::None {
            self.player_chrome_visible = true;
            return;
        }
        let playing = matches!(
            self.session.state,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        if !playing {
            self.player_chrome_visible = true;
            return;
        }
        if self.player_exit_ask {
            self.player_chrome_visible = true;
            return;
        }
        let Some(at) = self.player_pointer_at else {
            return;
        };
        let idle_ms: u64 = std::env::var("FLUXPLAY_CHROME_IDLE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or({
                #[cfg(target_os = "android")]
                {
                    // Soft present is heavy — keep chrome longer so controls stay reachable.
                    5000
                }
                #[cfg(not(target_os = "android"))]
                {
                    4000
                }
            });
        if at.elapsed() >= std::time::Duration::from_millis(idle_ms)
            && self.player_chrome_visible
        {
            self.player_chrome_visible = false;
            // Stamp hide time so wake events right after hide are ignored (Wayland
            // enter/leave spam when the overlay tree changes).
            self.player_pointer_at = Some(std::time::Instant::now());
            #[cfg(target_os = "android")]
            if self.session.native.android_surface_present() {
                // Expand SurfaceView to full bleed when chrome hides (was inset).
                crate::android_bridge::layout_video_surface_chrome_inset_dp(0.0);
            }
        }
    }

    /// Previous / next item of what is playing: the neighbouring episode of a series,
    /// the neighbouring channel of the live group, nothing for a film.
    fn playlist_neighbor(&self, delta: i32) -> Option<Message> {
        let cur = self.session.channel.as_ref()?;
        match cur.kind {
            ContentKind::Series => {
                let i = usize::try_from(self.series_queue_idx as i64 + delta as i64).ok()?;
                let (name, url) = self.series_queue.get(i)?;
                Some(Message::PlayVod {
                    name: name.clone(),
                    url: url.clone(),
                    kind: ContentKind::Series,
                    poster: cur.logo.clone(),
                })
            }
            ContentKind::Vod => None,
            ContentKind::Live => {
                let same = |c: &&Channel| c.id == cur.id && c.source_id == cur.source_id;
                let mut list = self.bundle.live_in_group(self.selected_group.as_deref());
                if !list.iter().any(same) {
                    // Started from favorites / recents / search: zap within its own group.
                    list = self.bundle.live_in_group(cur.group.as_deref());
                }
                let idx = list.iter().position(same)? as i32;
                let next = (idx + delta).rem_euclid(list.len() as i32) as usize;
                Some(Message::PlayChannel(list[next].clone()))
            }
        }
    }

    fn filtered_categories(
        &self,
        kind: ContentKind,
    ) -> Vec<&fluxplay_core::models::Category> {
        let q = self.cat_filter.to_lowercase();
        let pref = self.settings.pref_lang.as_str();
        let only = self.settings.only_pref_lang && !pref.is_empty();
        let mut ranked: Vec<(u8, usize, u8, &fluxplay_core::models::Category)> = self
            .bundle
            .categories
            .iter()
            .filter(|c| c.content == kind)
            .filter(|c| {
                q.is_empty()
                    || c.name.to_lowercase().contains(&q)
                    || crate::names::category_label(&c.name).to_lowercase().contains(&q)
                    || (crate::names::is_manga_category(&c.name)
                        && (crate::names::shelf_label(&c.name, false)
                            .to_lowercase()
                            .contains(&q)
                            || crate::names::shelf_label(&c.name, true)
                                .to_lowercase()
                                .contains(&q)))
            })
            .enumerate()
            .filter_map(|(i, c)| {
                let info = crate::names::parse_category(&c.name);
                if self.age_is_hidden(shelf_age_label(&c.name).as_deref()) {
                    return None;
                }
                if only && !info.lang.matches(pref) {
                    // Live groups are often "France FHD | TV" with no language tag.
                    // Dropping those hid every TV and Guide category.
                    let untagged = !info.lang.is_known();
                    if !(kind == ContentKind::Live && untagged) {
                        return None;
                    }
                }
                // Films and series: inside each language, new releases, then
                // children's cartoons, superhero animation, manga / anime.
                // Live TV keeps the provider order.
                let audience = match kind {
                    ContentKind::Vod | ContentKind::Series => {
                        crate::names::category_audience(&c.name)
                    }
                    _ => 0,
                };
                Some((crate::names::lang_rank(&info.lang, pref), i, audience, c))
            })
            .collect();
        // First portal index of each language, so Italian kids stay with Italian
        // and do not jump into the French block.
        let mut block_start: std::collections::HashMap<(u8, String), usize> =
            std::collections::HashMap::new();
        for (rank, i, _, c) in &ranked {
            let code = crate::names::parse_category(&c.name)
                .code
                .unwrap_or_default()
                .to_ascii_uppercase();
            block_start.entry((*rank, code)).or_insert(*i);
        }
        ranked.sort_by_key(|(rank, i, audience, c)| {
            let code = crate::names::parse_category(&c.name)
                .code
                .unwrap_or_default()
                .to_ascii_uppercase();
            let start = block_start.get(&(*rank, code)).copied().unwrap_or(*i);
            (*rank, start, *audience, *i)
        });
        ranked.into_iter().map(|(_, _, _, c)| c).collect()
    }

    fn lang_filter_on(&self) -> bool {
        self.settings.only_pref_lang && !self.settings.pref_lang.is_empty()
    }

    /// Live groups with no language tag stay visible. A named foreign language does not.
    fn keep_live_group(&self, name: &str) -> bool {
        if self.age_is_hidden(shelf_age_label(name).as_deref()) {
            return false;
        }
        if !self.lang_filter_on() {
            return true;
        }
        let info = crate::names::parse_category(name);
        !info.lang.is_known() || info.lang.matches(&self.settings.pref_lang)
    }

    /// Language filter context for the browse index (`None` = show everything).
    fn lang_ctx(&self) -> Option<std::sync::Arc<LangCtx>> {
        if !self.lang_filter_on() {
            return None;
        }
        let pref = self.settings.pref_lang.clone();
        let kind = match self.tab {
            Tab::Live => ContentKind::Live,
            Tab::Vod => ContentKind::Vod,
            Tab::Series => ContentKind::Series,
            _ => return None,
        };
        let cats = self
            .bundle
            .categories
            .iter()
            .filter(|c| c.content == kind)
            .map(|c| {
                let key = if kind == ContentKind::Live {
                    c.name.clone()
                } else {
                    c.id.clone()
                };
                (key, crate::names::parse_category(&c.name).lang)
            })
            .collect();
        Some(std::sync::Arc::new(LangCtx { pref, cats }))
    }

    fn view_channel_grid(&self, ui: UiTheme, guide: bool) -> Element<'_, Message> {
        if !guide && self.shelf != Shelf::Catalog {
            return self.view_library(ui);
        }
        let m = self.layout_metrics();
        let (cols, tile_w) = self.live_grid();
        let catalog_total = self.browse_index.len();
        let total = self.shown_count(catalog_total);
        let n_rows = total.div_ceil(cols);
        let row_h = browser::channel_row_height();
        // Same as films: the mosaic is a viewport overlay, so extra rows
        // would sit at the top and show the wrong channels.
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            n_rows,
            0,
        );

        let mut mosaic = Column::new().spacing(0).width(Fill);
        let paint_end = if total == 0 {
            slice.end
        } else {
            (slice.end + 1).min(n_rows)
        };
        if total == 0 {
            mosaic = mosaic.push(browser::empty_hint(
                ui,
                "Aucune chaîne dans cette sélection.",
            ));
        } else {
            for row_i in slice.start..paint_end {
                let mut r = Row::new().spacing(crate::theme::MOSAIC_GAP).width(Fill);
                for c in 0..cols {
                    let idx = row_i * cols + c;
                    if idx >= total {
                        break;
                    }
                    let Some(ch) = self
                        .browse_index
                        .get(idx)
                        .and_then(|i| self.bundle.channels.get(i))
                    else {
                        continue;
                    };
                    let thumb = crate::images::pick_art(
                        ch.logo.as_deref().or(ch.tvg_logo.as_deref()),
                        None,
                        None,
                        None,
                    )
                    .and_then(|k| self.images.get(&k));
                    let now = self
                        .epg_now_title(ch)
                        .unwrap_or("En direct")
                        .to_string();
                    let next = if guide {
                        self.epg_next_title(ch)
                            .map(|t| format!("Ensuite  {t}"))
                    } else {
                        None
                    };
                    r = r.push(browser::channel_tile(
                        crate::names::display_channel(&ch.name),
                        now,
                        next,
                        ch.id.clone(),
                        Message::PlayChannelId(ch.id.clone(), ch.source_id),
                        Some((
                            self.settings.is_favorite(&ch.id),
                            Message::ToggleFavorite(ch.id.clone()),
                        )),
                        ui,
                        tile_w,
                        thumb,
                        self.pressed_mosaic.as_deref() == Some(ch.id.as_str()),
                        self.selected_channel.as_deref() == Some(ch.id.as_str()),
                    ));
                }
                r = r.push(Space::new().width(Fill));
                mosaic = mosaic.push(r.height(Length::Fixed(row_h)));
            }
            if total < catalog_total && slice.end + 1 >= n_rows {
                mosaic = mosaic.push(browser::load_more_row(ui, catalog_total - total));
            }
        }

        let (heading, hint) = if guide {
            (
                "Guide TV".to_string(),
                format!("{catalog_total} chaînes · {} programmes", self.bundle.epg.len()),
            )
        } else {
            (
                self.selected_group
                    .as_deref()
                    .filter(|g| !matches!(*g, "*" | "Tous"))
                    .map(crate::names::category_label)
                    .unwrap_or_else(|| "Toutes les chaînes".into()),
                format!("{catalog_total} chaînes"),
            )
        };
        let header = browser::content_header_with(
            ui,
            heading,
            hint,
            &self.search,
            "Rechercher une chaîne…",
            m.search_w,
            m.title_size,
            m.stack_header,
        );

        let scroll_id = format!(
            "flux-{}-{}",
            if guide { "epg" } else { "live" },
            self.selected_group.as_deref().unwrap_or("*")
        );
        let mut content_h = browser::virtual_content_height(n_rows, self.browse_view_h, row_h);
        if total < catalog_total {
            content_h += 72.0;
        }
        let scroller = if total == 0 {
            browser::soft_scroll_on(ui, mosaic.width(Fill), scroll_id, Message::BrowseScrolled)
        } else {
            let offset = browser::mosaic_pin_offset(
                self.browse_scroll_y,
                self.browse_view_h,
                row_h,
                n_rows,
                slice,
            );
            browser::soft_scroll_mosaic(
                ui,
                scroll_id,
                content_h,
                browser::pin_mosaic(mosaic.width(Fill).into(), offset),
                Message::BrowseScrolled,
            )
        };
        let content = browser::pane(
            ui,
            Length::Fill,
            self.with_shelf_pills(false, header, scroller, m.gap),
        );

        self.with_categories(
            ui,
            m,
            if guide { "Guide" } else { "Chaînes" },
            &self.cat_entries,
            content,
        )
    }

    fn view_browse_vod(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        if let Some(detail) = &self.vod_detail {
            return self.view_vod_detail(ui, m, detail);
        }
        if self.shelf != Shelf::Catalog {
            return self.view_library(ui);
        }
        let (cols, tile_w) = self.poster_grid();

        let catalog_total = self.browse_index.len();
        let total = self.shown_count(catalog_total);
        let n_rows = total.div_ceil(cols);
        let row_h = browser::mosaic_row_height(tile_w);
        // Paint with no overscan: the mosaic is a viewport overlay, so extra
        // rows would sit at the top and show the wrong films.
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            n_rows,
            0,
        );

        let mut mosaic = Column::new().spacing(0).width(Fill);
        let paint_end = if total == 0 {
            slice.end
        } else {
            (slice.end + 1).min(n_rows)
        };
        if total == 0 {
            mosaic = mosaic.push(browser::empty_hint(
                ui,
                "Aucun film — sync en cours ou changez de catégorie.",
            ));
        } else {
            for row_i in slice.start..paint_end {
                let mut r = Row::new().spacing(crate::theme::MOSAIC_GAP).width(Fill);
                for c in 0..cols {
                    let idx = row_i * cols + c;
                    if idx >= total {
                        break;
                    }
                    let Some(v) = self.browse_index.get(idx).and_then(|i| self.bundle.vod.get(i))
                    else {
                        continue;
                    };
                    // Always paint titles + cached art (blanking during fling looked broken).
                    let ti = crate::names::parse_item_title(&v.name);
                    let meta = self.poster_caption(
                        v.year
                            .as_deref()
                            .filter(|y| !y.trim().is_empty())
                            .or(ti.year.as_deref()),
                        v.genre.as_deref(),
                        v.rated.as_deref(),
                        v.category_id.as_deref(),
                        &v.name,
                    );
                    let thumb = crate::images::vod_poster_url(v)
                        .and_then(|u| self.images.get(&u));
                    r = r.push(browser::mosaic_tile(
                        ti.title,
                        meta,
                        v.id.clone(),
                        Message::OpenVodDetail(v.id.clone()),
                        ui,
                        tile_w,
                        thumb,
                        self.pressed_mosaic.as_deref() == Some(v.id.as_str()),
                    ));
                }
                r = r.push(Space::new().width(Fill));
                mosaic = mosaic.push(r.height(Length::Fixed(row_h)));
            }
            if total < catalog_total && slice.end + 1 >= n_rows {
                mosaic = mosaic.push(browser::load_more_row(ui, catalog_total - total));
            }
        }

        let selected = self.selected_vod_category.as_deref();
        let cat_name = if matches!(selected, None | Some("*")) {
            "Tous les films".into()
        } else {
            self.shelf_heading(ContentKind::Vod, selected, "Films")
        };

        let header = browser::content_header_with(
            ui,
            cat_name,
            format!("{catalog_total} films"),
            &self.search,
            "Rechercher un film…",
            m.search_w,
            m.title_size,
            m.stack_header,
        );
        let scroll_id = format!(
            "flux-vod-{}",
            self.selected_vod_category.as_deref().unwrap_or("*")
        );
        let mut content_h = browser::virtual_content_height(n_rows, self.browse_view_h, row_h);
        if total < catalog_total {
            content_h += 72.0;
        }
        let scroller = if total == 0 {
            browser::soft_scroll_on(ui, mosaic.width(Fill), scroll_id, Message::BrowseScrolled)
        } else {
            let offset = browser::mosaic_pin_offset(
                self.browse_scroll_y,
                self.browse_view_h,
                row_h,
                n_rows,
                slice,
            );
            browser::soft_scroll_mosaic(
                ui,
                scroll_id,
                content_h,
                browser::pin_mosaic(mosaic.width(Fill).into(), offset),
                Message::BrowseScrolled,
            )
        };
        let content = browser::pane(
            ui,
            Length::Fill,
            self.with_shelf_pills(false, header, scroller, m.gap),
        );
        self.with_categories(ui, m, "Films / VOD", &self.cat_entries, content)
    }

    /// Current download root (settings or platform default).
    fn downloads_dir(&self) -> std::path::PathBuf {
        crate::storage::downloads_dir(&self.settings.download_dir)
    }

    /// Where `url` is written. Always inside [`Self::downloads_dir`]:
    /// `films/` for a film, `series/<titre>/` for an episode (even a single one).
    /// A series folder that already exists under the current root is reused.
    /// Changing the general folder does not send new files outside it.
    fn download_destination(&self, url: &str) -> std::path::PathBuf {
        let root = self.downloads_dir();
        if let Some(series) = self.series_owning_url(url) {
            return crate::downloads::series_title_dir(&root, &series);
        }
        if self.is_film_url(url) {
            return crate::downloads::films_dir(&root);
        }
        root
    }

    fn series_owning_url(&self, url: &str) -> Option<String> {
        let detail = self.series_detail.as_ref()?;
        let hit = detail
            .seasons
            .iter()
            .any(|s| s.episodes.iter().any(|ep| ep.stream_url == url));
        hit.then(|| detail.name.clone())
    }

    fn is_film_url(&self, url: &str) -> bool {
        self.vod_detail
            .as_ref()
            .is_some_and(|v| v.stream_url == url)
            || url.contains("/movie/")
    }

    /// Start `url` now, or queue it when [`crate::downloads::MAX_PARALLEL`]
    /// transfers are already running.
    fn request_download(&mut self, name: String, url: String) -> Task<Message> {
        use crate::downloads::DownloadState;
        if url.trim().is_empty() {
            self.status = format!("URL vide — {name}");
            return Task::none();
        }
        if matches!(
            self.downloads.get(&url),
            Some(DownloadState::Running { .. } | DownloadState::Queued { .. })
        ) {
            return Task::none();
        }
        if self.running_downloads() >= crate::downloads::MAX_PARALLEL {
            self.status = format!(
                "En file d'attente — {name} ({} en attente)",
                self.download_queue.len() + 1
            );
            self.downloads
                .insert(url.clone(), DownloadState::Queued { name });
            self.download_queue.push_back(url);
            return Task::none();
        }
        self.start_download(name, url)
    }

    fn running_downloads(&self) -> usize {
        self.downloads
            .values()
            .filter(|s| matches!(s, crate::downloads::DownloadState::Running { .. }))
            .count()
    }

    fn start_download(&mut self, name: String, url: String) -> Task<Message> {
        self.status = if crate::downloads::partial_for(&self.download_partials, &url).is_some() {
            format!("Reprise du téléchargement — {name}…")
        } else {
            format!("Téléchargement — {name}…")
        };
        let ua = play_options_from(&self.settings)
            .user_agent
            .unwrap_or_else(|| "IPTVSmartersPlayer".into());
        let key = url.clone();
        let req = crate::downloads::DownloadRequest {
            name: name.clone(),
            url: url.clone(),
            source: self.source_for_url(&url),
            user_agent: ua,
            dir: self.download_destination(&url),
            background: false,
        };
        let (task, handle) = Task::run(
            crate::downloads::run(req),
            move |event| Message::DownloadEvent {
                url: key.clone(),
                event,
            },
        )
        .abortable();
        self.downloads.insert(
            url,
            crate::downloads::DownloadState::Running {
                name,
                done: 0,
                total: None,
                part: None,
                handle,
            },
        );
        task
    }

    /// Fill free download slots from the queue.
    fn pump_download_queue(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        while self.running_downloads() < crate::downloads::MAX_PARALLEL {
            let Some(url) = self.download_queue.pop_front() else {
                break;
            };
            if let Some(crate::downloads::DownloadState::Queued { name }) =
                self.downloads.remove(&url)
            {
                tasks.push(self.start_download(name, url));
            }
        }
        Task::batch(tasks)
    }

    /// Stop a running or queued download (the partial file is deleted).
    fn cancel_download(&mut self, url: &str) -> bool {
        use crate::downloads::DownloadState;
        match self.downloads.remove(url) {
            Some(DownloadState::Running {
                name, part, handle, ..
            }) => {
                handle.abort();
                if let Some(part) = part {
                    crate::downloads::discard_partial(&part);
                }
                crate::downloads::forget_partial(&mut self.download_partials, url);
                self.status = format!("Téléchargement annulé — {name}");
                true
            }
            Some(DownloadState::Queued { name }) => {
                self.download_queue.retain(|u| u != url);
                self.status = format!("Retiré de la file — {name}");
                true
            }
            Some(other) => {
                self.downloads.insert(url.to_string(), other);
                false
            }
            None => false,
        }
    }

    /// Name the download of `url` was saved under (episodes get `S01E02` tags).
    fn download_name_for(&self, url: &str, name: &str, kind: ContentKind) -> String {
        if kind == ContentKind::Series {
            if let Some(detail) = &self.series_detail {
                for s in &detail.seasons {
                    for ep in &s.episodes {
                        let title = ep.title.trim();
                        let same_title = title.len() >= 3 && name.contains(title);
                        if ep.stream_url == url || same_title {
                            return crate::downloads::episode_title(
                                &detail.name,
                                s.season_number,
                                ep.episode_num,
                                &ep.title,
                            );
                        }
                    }
                }
            }
        }
        name.to_string()
    }

    /// Finished local copy of `url`, if any: library first, else a same-named
    /// file in the download folder (downloads made before the library).
    fn downloaded_file(&mut self, url: &str, name: &str, kind: ContentKind) -> Option<std::path::PathBuf> {
        if let Some((u, path)) = &self.prefetched {
            if u == url && path.is_file() {
                return Some(path.clone());
            }
        }
        if let Some(path) = self.download_library.get(url) {
            if path.is_file() {
                return Some(path.to_path_buf());
            }
            self.download_library.remove(url);
        }
        let want = crate::downloads::url_key(url);
        for (saved_url, state) in &self.downloads {
            if let crate::downloads::DownloadState::Done { path } = state {
                if path.is_file()
                    && (saved_url == url || crate::downloads::url_key(saved_url) == want)
                {
                    return Some(path.clone());
                }
            }
        }
        let running = self.downloads.iter().any(|(saved_url, state)| {
            matches!(state, crate::downloads::DownloadState::Running { .. })
                && (saved_url == url || crate::downloads::url_key(saved_url) == want)
        });
        if running {
            return None;
        }
        let saved_as = self.download_name_for(url, name, kind);
        let file = self.find_named_download(&saved_as).or_else(|| {
            (kind != ContentKind::Series)
                .then(|| crate::names::display_title(&saved_as))
                .filter(|clean| *clean != saved_as)
                .and_then(|clean| self.find_named_download(&clean))
        })?;
        self.download_library.insert(url, file.clone());
        Some(file)
    }

    /// A finished file named `name` under the download root, including
    /// `films/` and `series/<titre>/`.
    fn find_named_download(&self, name: &str) -> Option<std::path::PathBuf> {
        let root = self.downloads_dir();
        if let Some(path) = crate::downloads::find_by_name(&root, name) {
            return Some(path);
        }
        if let Some(path) = crate::downloads::find_by_name(&crate::downloads::films_dir(&root), name)
        {
            return Some(path);
        }
        let series = root.join("series");
        let Ok(entries) = std::fs::read_dir(&series) else {
            return None;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = crate::downloads::find_by_name(&path, name) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// Detail-page download button plus where the file goes (or went).
    fn download_hero(&self, name: String, url: &str) -> (String, Message, String) {
        let (label, msg) = self.download_action(name, url, false);
        let hint = match self.download_library.get(url) {
            Some(path) => format!(
                "Téléchargé — « Lire » ouvre le fichier : {}",
                crate::storage::display_path(path)
            ),
            None => format!(
                "Enregistré dans {} — modifiable dans Réglages › Téléchargements",
                crate::storage::display_path(&self.download_destination(url))
            ),
        };
        (label, msg, hint)
    }

    /// Button for the whole open series (`None`) or one season: download what
    /// is missing, cancel what is queued / running, or open the folder.
    fn season_download_action(
        &self,
        detail: &SeriesItem,
        season: Option<u32>,
        compact: bool,
    ) -> (String, Message) {
        use crate::downloads::DownloadState;
        let urls = || {
            detail
                .seasons
                .iter()
                .filter(move |s| season.is_none_or(|n| s.season_number == n))
                .flat_map(|s| s.episodes.iter().map(|ep| ep.stream_url.as_str()))
        };
        let total = urls().count();
        let done = urls().filter(|u| self.download_library.contains(u)).count();
        let active = urls()
            .filter(|u| {
                matches!(
                    self.downloads.get(*u),
                    Some(DownloadState::Running { .. } | DownloadState::Queued { .. })
                )
            })
            .count();
        let (what, done_label) = if season.is_some() {
            ("la saison", "Saison téléchargée · Dossier")
        } else {
            ("la série", "Série téléchargée · Dossier")
        };
        if active > 0 {
            return (
                format!("{done}/{total} · Annuler"),
                Message::CancelSeasonDownloads(season),
            );
        }
        if total > 0 && done == total {
            if let Some(path) = urls().find_map(|u| self.download_library.get(u)) {
                return (
                    if compact { "Téléchargée" } else { done_label }.into(),
                    Message::RevealDownload(path.to_path_buf()),
                );
            }
        }
        let label = match (compact, done) {
            (true, 0) => "Télécharger".to_string(),
            (true, d) => format!("Compléter {d}/{total}"),
            (false, 0) => format!("Télécharger {what} ({total} ép.)"),
            (false, d) => format!("Compléter {what} ({d}/{total})"),
        };
        (label, Message::DownloadSeason(season))
    }

    /// Label + action of the download button for `url`, following its state.
    /// `compact` = short labels for episode rows.
    fn download_action(&self, name: String, url: &str, compact: bool) -> (String, Message) {
        use crate::downloads::{progress_text, DownloadState};
        let resume = crate::downloads::partial_for(&self.download_partials, url)
            .filter(|p| p.done > 0)
            .map(|p| {
                if compact {
                    "Reprendre".to_string()
                } else {
                    format!("Reprendre · {}", progress_text(p.done, p.total))
                }
            });
        if let Some(path) = self.download_library.get(url) {
            return (
                if compact { "Dossier" } else { "Ouvrir le dossier" }.into(),
                Message::RevealDownload(path.to_path_buf()),
            );
        }
        match self.downloads.get(url) {
            Some(DownloadState::Queued { .. }) => (
                if compact { "En attente" } else { "En attente · Annuler" }.into(),
                Message::CancelDownload(url.to_string()),
            ),
            Some(DownloadState::Running { done, total, .. }) => (
                format!("{} · Annuler", progress_text(*done, *total)),
                Message::CancelDownload(url.to_string()),
            ),
            Some(DownloadState::Done { path }) => (
                if compact { "Dossier" } else { "Ouvrir le dossier" }.into(),
                Message::RevealDownload(path.clone()),
            ),
            Some(DownloadState::Failed) => (
                resume.unwrap_or_else(|| {
                    if compact { "Réessayer" } else { "Réessayer le téléchargement" }.into()
                }),
                Message::DownloadMedia {
                    name,
                    url: url.to_string(),
                },
            ),
            None => (
                resume.unwrap_or_else(|| "Télécharger".into()),
                Message::DownloadMedia {
                    name,
                    url: url.to_string(),
                },
            ),
        }
    }

    /// Detail-page fields normalized for display (portal noise removed) and, when
    /// enabled, synopsis / genre in the viewer language.
    #[allow(clippy::too_many_arguments)]
    fn detail_text(
        &self,
        name: &str,
        year: Option<&str>,
        genre: Option<&str>,
        rating: Option<&str>,
        runtime: Option<&str>,
        rated: Option<&str>,
        plot: Option<&str>,
        people: [Option<&str>; 3],
        facts: [Option<&str>; 3],
    ) -> DetailText {
        use crate::metadata as md;
        let title = crate::names::display_title(name);
        let plot_src = md::fmt_plot(plot, &title);
        let genre_src = md::fmt_genre(genre);
        let lang_label = crate::names::lang_label(&self.settings.pref_lang).unwrap_or("");
        let plot_note = plot_src.as_deref().and_then(|p| match self.tr_state(p) {
            Some(true) => Some("Traduit automatiquement (Google Traduction)".to_string()),
            Some(false) => Some(format!("Traduction en {} …", lang_label.to_lowercase())),
            None => None,
        });
        let [country, language, awards] = facts;
        DetailText {
            year: md::fmt_year(year).or_else(|| crate::names::parse_item_title(name).year),
            genre: genre_src.as_deref().map(|g| self.tr(g).to_string()),
            rating: md::fmt_rating(rating),
            runtime: md::fmt_runtime(runtime),
            rated: md::fmt_rated(rated),
            plot: plot_src.as_deref().map(|p| self.tr(p).to_string()),
            plot_note,
            actors: md::fmt_people(people[0]),
            director: md::fmt_people(people[1]),
            writer: md::fmt_people(people[2]),
            facts: [
                ("Pays", md::fmt_people(country)),
                ("Langue", md::fmt_people(language)),
                ("Récompenses", md::fmt_rated(awards)),
            ]
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect(),
            title,
        }
    }

    fn view_vod_detail(
        &self,
        ui: UiTheme,
        m: crate::theme::LayoutMetrics,
        detail: &VodItem,
    ) -> Element<'_, Message> {
        if self.actor_sheet.is_some() {
            return self.view_actor_sheet(ui);
        }
        let poster = crate::images::pick_art(None, detail.poster.as_deref(), None, None)
            .and_then(|u| self.images.get(&u));
        let imdb_query = detail
            .imdb_id
            .clone()
            .unwrap_or_else(|| crate::names::display_title(&detail.name));
        let cats = self.category_line(detail.genre.as_deref(), detail.category_id.as_deref());
        let t = self.detail_text(
            &detail.name,
            detail.year.as_deref(),
            if cats.is_empty() { None } else { Some(cats.as_str()) },
            detail.rating.as_deref(),
            detail.runtime.as_deref(),
            detail.rated.as_deref(),
            detail.plot.as_deref(),
            [
                detail.actors.as_deref(),
                detail.director.as_deref(),
                detail.writer.as_deref(),
            ],
            [
                detail.country.as_deref(),
                detail.language.as_deref(),
                detail.awards.as_deref(),
            ],
        );
        browser::media_detail_page(
            ui,
            &t.title,
            poster,
            t.year.as_deref(),
            t.genre.as_deref(),
            t.rating.as_deref(),
            t.runtime.as_deref(),
            t.rated.as_deref(),
            t.plot.as_deref(),
            t.actors.as_deref(),
            t.director.as_deref(),
            t.writer.as_deref(),
            detail.imdb_id.as_deref(),
            imdb_query,
            #[cfg(target_os = "android")]
            Some("Lire le film".into()),
            #[cfg(not(target_os = "android"))]
            Some("▶ Lire le film".into()),
            Some(Message::PlayVod {
                name: detail.name.clone(),
                url: detail.stream_url.clone(),
                kind: ContentKind::Vod,
                poster: detail.poster.clone(),
            }),
            Some(self.download_hero(detail.name.clone(), &detail.stream_url)),
            Message::CloseVodDetail,
            None,
            m.bp.is_narrow(),
            self.detail_meta_loading,
            t.facts,
            t.plot_note,
            Some(self.credit_sections(ui)),
            false,
            None,
            Some((
                self.settings.is_favorite(&detail.id),
                Message::ToggleFavorite(detail.id.clone()),
            )),
            None,
        )
    }

    fn sync_selected_season(&mut self) {
        let Some(detail) = &self.series_detail else {
            self.selected_season = None;
            return;
        };
        let valid = self
            .selected_season
            .is_some_and(|n| detail.seasons.iter().any(|s| s.season_number == n));
        if !valid {
            self.selected_season = detail
                .seasons
                .iter()
                .find(|s| s.season_number > 0)
                .or_else(|| detail.seasons.first())
                .map(|s| s.season_number);
            self.season_menu_open = false;
        }
    }

    fn prefetch_episode_stills(&mut self) -> Task<Message> {
        let Some(detail) = &self.series_detail else {
            return Task::none();
        };
        let want = self.selected_season;
        let season = detail
            .seasons
            .iter()
            .find(|s| Some(s.season_number) == want)
            .or_else(|| detail.seasons.iter().find(|s| s.season_number > 0))
            .or_else(|| detail.seasons.first());
        let mut urls = Vec::new();
        if let Some(cover) = detail.cover.clone().or(detail.banner.clone()) {
            urls.push(cover);
        }
        if let Some(season) = season {
            for ep in &season.episodes {
                if let Some(still) = ep.still.clone().filter(|u| u.starts_with("http")) {
                    urls.push(still);
                }
            }
        }
        let sid = detail.source_id;
        self.prefetch_art(urls.into_iter().map(|url| (url, sid)).collect(), 24)
    }

    fn open_credit_names(&self) -> Vec<String> {
        let (actors, director, writer) = if let Some(d) = &self.series_detail {
            (d.actors.as_deref(), d.director.as_deref(), d.writer.as_deref())
        } else if let Some(d) = &self.vod_detail {
            (d.actors.as_deref(), d.director.as_deref(), d.writer.as_deref())
        } else {
            return Vec::new();
        };
        let mut names = split_credits(actors);
        for extra in split_credits(director).into_iter().chain(split_credits(writer)) {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(&extra)) {
                names.push(extra);
            }
        }
        names.truncate(20);
        names
    }

    fn request_cast_photos(&mut self) -> Task<Message> {
        let names = self.open_credit_names();
        let mut tasks = Vec::new();
        for name in names.into_iter().take(10) {
            let key = credit_key(&name);
            if !self.cast_photo_asked.insert(key) {
                continue;
            }
            tasks.push(Task::perform(
                async move {
                    let url = crate::metadata::person_headshot_fast(&name).await;
                    Message::CastPhoto { name, url }
                },
                |m| m,
            ));
        }
        Task::batch(tasks)
    }

    /// Portal text when it is already French. Otherwise one French Wikipedia call.
    fn french_plot_task(&self) -> Task<Message> {
        let (series, id, name, plot) = if let Some(d) = &self.series_detail {
            (true, d.id.clone(), d.name.clone(), d.plot.clone())
        } else if let Some(d) = &self.vod_detail {
            (false, d.id.clone(), d.name.clone(), d.plot.clone())
        } else {
            return Task::none();
        };
        if plot
            .as_deref()
            .is_some_and(|p| crate::translate::guess_lang(p) == Some("fr"))
        {
            return Task::none();
        }
        let title = crate::names::display_title(&name);
        Task::perform(
            async move {
                let plot = crate::metadata::french_plot(&title).await;
                Message::FrenchPlotReady { series, id, plot }
            },
            |m| m,
        )
    }

    fn credit_people<'a>(
        &'a self,
        raw: Option<&str>,
    ) -> Vec<(String, Option<&'a iced::widget::image::Handle>)> {
        split_credits(raw)
            .into_iter()
            .map(|name| {
                let photo = self
                    .cast_photos
                    .get(&credit_key(&name))
                    .and_then(|url| self.images.get(url));
                (name, photo)
            })
            .collect()
    }

    fn credit_sections(&self, ui: UiTheme) -> Element<'_, Message> {
        let (actors, director, writer) = if let Some(d) = &self.series_detail {
            (d.actors.as_deref(), d.director.as_deref(), d.writer.as_deref())
        } else if let Some(d) = &self.vod_detail {
            (d.actors.as_deref(), d.director.as_deref(), d.writer.as_deref())
        } else {
            (None, None, None)
        };
        let mut col = Column::new().spacing(crate::theme::SPACE_MD).width(Fill);
        let actors_people = self.credit_people(actors);
        if actors_people.is_empty() {
            let line = if self.detail_meta_loading {
                "Chargement de la distribution…"
            } else {
                "Distribution indisponible pour ce titre."
            };
            col = col.push(browser::copy_section(ui, "Acteurs", line));
        } else {
            col = col.push(browser::people_block(ui, "Acteurs", actors_people));
        }
        let mut crew = self.credit_people(director);
        for person in self.credit_people(writer) {
            if !crew.iter().any(|(n, _)| n.eq_ignore_ascii_case(&person.0)) {
                crew.push(person);
            }
        }
        if !crew.is_empty() {
            col = col.push(browser::people_block(ui, "Équipe", crew));
        }
        col.into()
    }

    fn view_series_detail(
        &self,
        ui: UiTheme,
        m: crate::theme::LayoutMetrics,
        detail: &SeriesItem,
    ) -> Element<'_, Message> {
        if self.actor_sheet.is_some() {
            return self.view_actor_sheet(ui);
        }
        let poster = crate::images::pick_art(
            None,
            None,
            detail.cover.as_deref(),
            detail.banner.as_deref(),
        )
        .and_then(|u| self.images.get(&u));
        // Season 0 holds specials (or unparsable keys): "Lire" starts at S01E01 when it exists.
        let episodes_of = |regular: bool| {
            detail
                .seasons
                .iter()
                .filter(move |s| (s.season_number > 0) == regular)
                .flat_map(|s| s.episodes.iter().map(move |ep| (s.season_number, ep)))
        };
        let first_play = episodes_of(true)
            .next()
            .or_else(|| episodes_of(false).next())
            .map(|(season_num, ep)| {
                (
                    {
                        #[cfg(target_os = "android")]
                        {
                            format!("Lire · S{season_num:02}E{:02}", ep.episode_num)
                        }
                        #[cfg(not(target_os = "android"))]
                        {
                            format!("▶ Lire · S{season_num:02}E{:02}", ep.episode_num)
                        }
                    },
                    Message::PlayVod {
                        name: format!("{} — {}", detail.name, ep.title),
                        url: ep.stream_url.clone(),
                        kind: ContentKind::Series,
                        poster: detail.cover.clone().or(detail.banner.clone()),
                    },
                    {
                        let (label, msg) = self.season_download_action(detail, None, false);
                        let hint = format!(
                            "Épisodes enregistrés dans {} ({} à la fois) — un épisode téléchargé se lit depuis le disque",
                            crate::storage::display_path(&crate::downloads::series_title_dir(
                                &self.downloads_dir(),
                                &detail.name
                            )),
                            crate::downloads::MAX_PARALLEL
                        );
                        (label, msg, hint)
                    },
                )
            });
        let (play_label, play_msg) = if let Some(target) = resume_target(&self.settings, detail) {
            (
                Some(resume_button_label(
                    target.kind,
                    target.season,
                    target.episode_num,
                )),
                Some(Message::PlayVod {
                    name: format!("{} — {}", detail.name, target.title),
                    url: target.url,
                    kind: ContentKind::Series,
                    poster: detail.cover.clone().or(detail.banner.clone()),
                }),
            )
        } else {
            match first_play {
                Some((l, play, _)) => (Some(l), Some(play)),
                None => (None, None),
            }
        };

        let imdb_query = detail
            .imdb_id
            .clone()
            .unwrap_or_else(|| crate::names::display_title(&detail.name));
        let cats = self.category_line(detail.genre.as_deref(), detail.category_id.as_deref());
        let t = self.detail_text(
            &detail.name,
            detail.year.as_deref(),
            if cats.is_empty() { None } else { Some(cats.as_str()) },
            detail.rating.as_deref(),
            detail.runtime.as_deref(),
            detail.rated.as_deref(),
            detail.plot.as_deref(),
            [
                detail.actors.as_deref(),
                detail.director.as_deref(),
                detail.writer.as_deref(),
            ],
            [
                detail.country.as_deref(),
                detail.language.as_deref(),
                detail.awards.as_deref(),
            ],
        );

        let season = detail
            .seasons
            .iter()
            .find(|s| Some(s.season_number) == self.selected_season)
            .or_else(|| detail.seasons.iter().find(|s| s.season_number > 0))
            .or_else(|| detail.seasons.first());
        let (episodes, season_picker) = if detail.seasons.is_empty() {
            (None, None)
        } else {
            let shown = season.map(|s| s.season_number);
            let label = season
                .map(|s| season_title(s.season_number, s.episodes.len()))
                .unwrap_or_else(|| "Saisons".into());
            let (dl_label, dl_msg) = self.season_download_action(detail, shown, true);
            let season_dl = if dl_label.to_lowercase().contains("dossier") {
                None
            } else {
                Some((dl_label, dl_msg))
            };
            let mut picker = Column::new().spacing(0).width(Fill);
            picker = picker.push(browser::season_bar(
                ui,
                label,
                self.season_menu_open,
                season_dl,
            ));
            if self.season_menu_open {
                for s in &detail.seasons {
                    picker = picker.push(browser::season_choice(
                        ui,
                        season_title(s.season_number, s.episodes.len()),
                        s.season_number,
                        Some(s.season_number) == shown,
                    ));
                }
            }
            let mut col = Column::new().spacing(crate::theme::SPACE_SM).width(Fill);
            if let Some(season) = season {
                let fallback = detail.cover.clone().or(detail.banner.clone());
                for ep in &season.episodes {
                    let still_url = ep.still.clone().filter(|u| u.starts_with("http"));
                    let still = still_url
                        .as_deref()
                        .and_then(|u| self.images.get(u))
                        .or(poster);
                    let title =
                        crate::names::episode_display(&detail.name, &ep.title, ep.episode_num);
                    let plot = match crate::metadata::fmt_plot(ep.plot.as_deref(), &ep.title) {
                        Some(p) => self.tr(&p).replace('\n', " "),
                        None => "Pas de résumé pour cet épisode.".into(),
                    };
                    let series_age = crate::metadata::age_rating(t.rated.as_deref())
                        .map(|age| age.label);
                    let facts = browser::EpisodeFacts {
                        duration: crate::metadata::fmt_runtime(ep.runtime.as_deref()),
                        date: crate::metadata::fmt_airdate(ep.airdate.as_deref()),
                        stars: crate::metadata::fmt_rating(ep.rating.as_deref()),
                        age: crate::metadata::age_rating(ep.rated.as_deref())
                            .map(|age| age.label)
                            .or(series_age),
                    };
                    let (ep_dl, ep_msg) = self.download_action(
                        crate::downloads::episode_title(
                            &detail.name,
                            season.season_number,
                            ep.episode_num,
                            &ep.title,
                        ),
                        &ep.stream_url,
                        true,
                    );
                    let ep_dl = if ep_dl.to_lowercase().contains("dossier") {
                        None
                    } else {
                        Some((ep_dl, ep_msg))
                    };
                    let hover_key = ep.id.clone();
                    col = col.push(browser::episode_card(
                        ui,
                        still,
                        ep.episode_num,
                        title,
                        plot,
                        facts,
                        hover_key.clone(),
                        self.episode_hover.as_deref() == Some(hover_key.as_str()),
                        Message::PlayVod {
                            name: format!("{} — {}", detail.name, ep.title),
                            url: ep.stream_url.clone(),
                            kind: ContentKind::Series,
                            poster: still_url.or(fallback.clone()),
                        },
                        ep_dl,
                        m.bp.is_narrow(),
                        self.settings.watch_ratio(&ep.stream_url),
                    ));
                }
            }
            let facts: Vec<String> = t
                .facts
                .iter()
                .filter(|(_, v)| !v.trim().is_empty())
                .map(|(k, v)| format!("{k} · {v}"))
                .collect();
            if !facts.is_empty() {
                col = col.push(browser::copy_section(ui, "Informations", &facts.join("\n")));
            }
            (Some(col.into()), Some(picker.into()))
        };

        browser::media_detail_page(
            ui,
            &t.title,
            poster,
            t.year.as_deref(),
            t.genre.as_deref(),
            t.rating.as_deref(),
            t.runtime.as_deref(),
            t.rated.as_deref(),
            t.plot.as_deref(),
            t.actors.as_deref(),
            t.director.as_deref(),
            t.writer.as_deref(),
            detail.imdb_id.as_deref(),
            imdb_query,
            play_label,
            play_msg,
            None,
            Message::CloseSeriesDetail,
            episodes,
            m.bp.is_narrow(),
            self.detail_meta_loading,
            Vec::new(),
            t.plot_note.clone(),
            Some(self.credit_sections(ui)),
            true,
            season_picker,
            Some((
                self.settings.is_favorite(&detail.id),
                Message::ToggleFavorite(detail.id.clone()),
            )),
            (detail
                .seasons
                .iter()
                .map(|s| s.episodes.len())
                .sum::<usize>()
                > 0)
                .then(|| {
                    (
                        self.settings
                            .episode_alerts
                            .iter()
                            .any(|a| a.series_id == detail.id),
                        Message::ToggleEpisodeAlert,
                    )
                }),
        )
    }

    fn view_browse_series(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        let (cols, tile_w) = self.poster_grid();
        if let Some(detail) = &self.series_detail {
            return self.view_series_detail(ui, m, detail);
        }
        if self.shelf != Shelf::Catalog {
            return self.view_library(ui);
        }

        let catalog_total = self.browse_index.len();
        let total = self.shown_count(catalog_total);
        let n_rows = total.div_ceil(cols);
        let row_h = browser::mosaic_row_height(tile_w);
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            n_rows,
            0,
        );

        let mut mosaic = Column::new().spacing(0).width(Fill);
        let paint_end = if total == 0 {
            slice.end
        } else {
            (slice.end + 1).min(n_rows)
        };
        if total == 0 {
            mosaic = mosaic.push(browser::empty_hint(
                ui,
                "Aucune série — sync en cours ou changez de catégorie.",
            ));
        } else {
            for row_i in slice.start..paint_end {
                let mut r = Row::new().spacing(crate::theme::MOSAIC_GAP).width(Fill);
                for c in 0..cols {
                    let idx = row_i * cols + c;
                    if idx >= total {
                        break;
                    }
                    let Some(s) = self
                        .browse_index
                        .get(idx)
                        .and_then(|i| self.bundle.series.get(i))
                    else {
                        continue;
                    };
                    let ti = crate::names::parse_item_title(&s.name);
                    let meta = self.poster_caption(
                        s.year
                            .as_deref()
                            .filter(|y| !y.trim().is_empty())
                            .or(ti.year.as_deref()),
                        s.genre.as_deref(),
                        s.rated.as_deref(),
                        s.category_id.as_deref(),
                        &s.name,
                    );
                    let thumb = crate::images::series_cover_url(s)
                        .and_then(|u| self.images.get(&u));
                    r = r.push(browser::mosaic_tile(
                        ti.title,
                        meta,
                        s.id.clone(),
                        Message::OpenSeries(s.id.clone()),
                        ui,
                        tile_w,
                        thumb,
                        self.pressed_mosaic.as_deref() == Some(s.id.as_str()),
                    ));
                }
                r = r.push(Space::new().width(Fill));
                mosaic = mosaic.push(r.height(Length::Fixed(row_h)));
            }
            if total < catalog_total && slice.end + 1 >= n_rows {
                mosaic = mosaic.push(browser::load_more_row(ui, catalog_total - total));
            }
        }

        let selected = self.selected_series_category.as_deref();
        let cat_name = if matches!(selected, None | Some("*")) {
            "Toutes les séries".into()
        } else {
            self.shelf_heading(ContentKind::Series, selected, "Séries")
        };
        let header = browser::content_header_with(
            ui,
            cat_name,
            format!("{catalog_total} séries"),
            &self.search,
            "Rechercher une série…",
            m.search_w,
            m.title_size,
            m.stack_header,
        );
        let scroll_id = format!(
            "flux-series-{}",
            self.selected_series_category.as_deref().unwrap_or("*")
        );
        let mut content_h = browser::virtual_content_height(n_rows, self.browse_view_h, row_h);
        if total < catalog_total {
            content_h += 72.0;
        }
        let scroller = if total == 0 {
            browser::soft_scroll_on(ui, mosaic.width(Fill), scroll_id, Message::BrowseScrolled)
        } else {
            let offset = browser::mosaic_pin_offset(
                self.browse_scroll_y,
                self.browse_view_h,
                row_h,
                n_rows,
                slice,
            );
            browser::soft_scroll_mosaic(
                ui,
                scroll_id,
                content_h,
                browser::pin_mosaic(mosaic.width(Fill).into(), offset),
                Message::BrowseScrolled,
            )
        };
        let content = browser::pane(
            ui,
            Length::Fill,
            self.with_shelf_pills(false, header, scroller, m.gap),
        );
        self.with_categories(ui, m, "Séries", &self.cat_entries, content)
    }

    /// History or favorites of the open section, in the same cards as the catalog.
    fn view_library(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        let q = self.search.to_lowercase();
        let kind = self.section_kind().unwrap_or(ContentKind::Live);
        let history = self.shelf == Shelf::History;
        let (_section, placeholder, noun) = match self.tab {
            Tab::Vod => ("Films", "Rechercher un film…", "films"),
            Tab::Series => ("Séries", "Rechercher une série…", "séries"),
            _ => ("Chaînes", "Rechercher une chaîne…", "chaînes"),
        };
        let title = if history { "Historique" } else { "Favoris" };

        let mut channel_ix: Vec<usize> = Vec::new();
        struct Card {
            title: String,
            meta: String,
            id: String,
            open: Message,
            art: Option<String>,
            mark: browser::TileMark,
        }
        let mut cards: Vec<Card> = Vec::new();
        let mut empty = String::new();

        if self.tab == Tab::Live {
            if history {
                empty = "Les chaînes en direct ne sont pas gardées dans l'historique.".into();
            } else {
                channel_ix = self
                    .bundle
                    .channels
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| self.settings.is_favorite(&c.id))
                    .filter(|(_, c)| {
                        q.is_empty()
                            || crate::names::display_channel(&c.name).to_lowercase().contains(&q)
                    })
                    .map(|(i, _)| i)
                    .collect();
                if channel_ix.is_empty() {
                    empty = if q.is_empty() {
                        "Aucune chaîne en favori. Le cœur sur une chaîne l'ajoute ici.".into()
                    } else {
                        "Aucun résultat.".into()
                    };
                }
            }
        } else if history {
            for r in &self.settings.recent {
                if !r.is_media() || r.media_kind() != kind {
                    continue;
                }
                let (title, meta, open, mut art) = self.history_presentation(r);
                if !q.is_empty() && !title.to_lowercase().contains(&q) {
                    continue;
                }
                if art.is_none() {
                    art = self
                        .history_art
                        .iter()
                        .find(|(url, _)| url == &r.stream_url)
                        .map(|(_, poster)| poster.clone());
                }
                let key = if r.stream_url.is_empty() {
                    r.channel_id.clone()
                } else {
                    r.stream_url.clone()
                };
                cards.push(Card {
                    title,
                    meta,
                    id: r.channel_id.clone(),
                    open,
                    art,
                    mark: browser::TileMark::Remove(Message::RemoveHistory(key)),
                });
            }
            if cards.is_empty() {
                empty = if q.is_empty() {
                    format!("Aucun titre dans l'historique des {noun}.")
                } else {
                    "Aucun résultat.".into()
                };
            }
        } else if self.tab == Tab::Series {
            for s in &self.bundle.series {
                if !self.settings.is_favorite(&s.id) {
                    continue;
                }
                if !self.media_age_visible(
                    s.rated.as_deref(),
                    s.genre.as_deref(),
                    s.category_id.as_deref(),
                    &s.name,
                ) {
                    continue;
                }
                let ti = crate::names::parse_item_title(&s.name);
                if !q.is_empty() && !ti.title.to_lowercase().contains(&q) {
                    continue;
                }
                let meta = self.poster_caption(
                    s.year.as_deref().filter(|y| !y.trim().is_empty()).or(ti.year.as_deref()),
                    s.genre.as_deref(),
                    s.rated.as_deref(),
                    s.category_id.as_deref(),
                    &s.name,
                );
                cards.push(Card {
                    title: ti.title,
                    meta,
                    id: s.id.clone(),
                    open: Message::OpenSeries(s.id.clone()),
                    art: crate::images::series_cover_url(s),
                    mark: browser::TileMark::Favorite {
                        on: true,
                        toggle: Message::ToggleFavorite(s.id.clone()),
                    },
                });
            }
            if cards.is_empty() {
                empty = if q.is_empty() {
                    "Aucune série en favori. Ouvrez une série et touchez le cœur.".into()
                } else {
                    "Aucun résultat.".into()
                };
            }
        } else {
            for v in &self.bundle.vod {
                if !self.settings.is_favorite(&v.id) {
                    continue;
                }
                if !self.media_age_visible(
                    v.rated.as_deref(),
                    v.genre.as_deref(),
                    v.category_id.as_deref(),
                    &v.name,
                ) {
                    continue;
                }
                let ti = crate::names::parse_item_title(&v.name);
                if !q.is_empty() && !ti.title.to_lowercase().contains(&q) {
                    continue;
                }
                let meta = self.poster_caption(
                    v.year.as_deref().filter(|y| !y.trim().is_empty()).or(ti.year.as_deref()),
                    v.genre.as_deref(),
                    v.rated.as_deref(),
                    v.category_id.as_deref(),
                    &v.name,
                );
                cards.push(Card {
                    title: ti.title,
                    meta,
                    id: v.id.clone(),
                    open: Message::OpenVodDetail(v.id.clone()),
                    art: crate::images::vod_poster_url(v),
                    mark: browser::TileMark::Favorite {
                        on: true,
                        toggle: Message::ToggleFavorite(v.id.clone()),
                    },
                });
            }
            if cards.is_empty() {
                empty = if q.is_empty() {
                    "Aucun film en favori. Ouvrez un film et touchez le cœur.".into()
                } else {
                    "Aucun résultat.".into()
                };
            }
        }

        let total = if self.tab == Tab::Live && !history {
            channel_ix.len()
        } else {
            cards.len()
        };
        let channels = self.tab == Tab::Live && !history;
        let (cols, tile_w) = if channels {
            self.live_grid()
        } else {
            self.poster_grid()
        };
        let row_h = if channels {
            browser::channel_row_height()
        } else {
            browser::mosaic_row_height(tile_w)
        };
        let n_rows = if total == 0 { 0 } else { total.div_ceil(cols) };
        let slice = browser::virtual_slice(self.browse_scroll_y, self.browse_view_h, row_h, n_rows, 0);
        let mut mosaic = Column::new().spacing(0).width(Fill);
        if total == 0 {
            mosaic = mosaic.push(browser::empty_hint(ui, empty));
        } else {
            let paint_end = (slice.end + 1).min(n_rows);
            for row_i in slice.start..paint_end {
                let mut r = Row::new().spacing(crate::theme::MOSAIC_GAP).width(Fill);
                for c in 0..cols {
                    let idx = row_i * cols + c;
                    if idx >= total {
                        break;
                    }
                    if channels {
                        let Some(ch) = channel_ix.get(idx).and_then(|i| self.bundle.channels.get(*i))
                        else {
                            continue;
                        };
                        let thumb = crate::images::pick_art(
                            ch.logo.as_deref().or(ch.tvg_logo.as_deref()),
                            None,
                            None,
                            None,
                        )
                        .and_then(|k| self.images.get(&k));
                        let now = self.epg_now_title(ch).unwrap_or("En direct").to_string();
                        r = r.push(browser::channel_tile(
                            crate::names::display_channel(&ch.name),
                            now,
                            None,
                            ch.id.clone(),
                            Message::PlayChannelId(ch.id.clone(), ch.source_id),
                            Some((true, Message::ToggleFavorite(ch.id.clone()))),
                            ui,
                            tile_w,
                            thumb,
                            self.pressed_mosaic.as_deref() == Some(ch.id.as_str()),
                            self.selected_channel.as_deref() == Some(ch.id.as_str()),
                        ));
                    } else if let Some(card) = cards.get(idx) {
                        let thumb = card.art.as_deref().and_then(|u| self.images.get(u));
                        r = r.push(browser::mosaic_tile_marked(
                            card.title.clone(),
                            card.meta.clone(),
                            card.id.clone(),
                            card.open.clone(),
                            ui,
                            tile_w,
                            thumb,
                            self.pressed_mosaic.as_deref() == Some(card.id.as_str()),
                            Some(card.mark.clone()),
                        ));
                    }
                }
                r = r.push(Space::new().width(Fill));
                mosaic = mosaic.push(r.height(Length::Fixed(row_h)));
            }
        }

        let header = browser::content_header_with(
            ui,
            title.into(),
            format!("{total} {noun}"),
            &self.search,
            placeholder,
            m.search_w,
            m.title_size,
            m.stack_header,
        );
        let header: Element<'_, Message> = if let Some(msg) = self.shelf_clear() {
            column![
                header,
                browser::download_pill(ui, "Supprimer l'historique".into(), msg, false),
            ]
            .spacing(8)
            .width(Fill)
            .into()
        } else {
            header
        };
        let scroll_id = format!(
            "flux-shelf-{}-{}",
            match self.tab {
                Tab::Vod => "vod",
                Tab::Series => "series",
                _ => "live",
            },
            if history { "h" } else { "f" }
        );
        let content_h = browser::virtual_content_height(n_rows, self.browse_view_h, row_h);
        let scroller = if total == 0 {
            browser::soft_scroll_on(ui, mosaic.width(Fill), scroll_id, Message::BrowseScrolled)
        } else {
            let offset = browser::mosaic_pin_offset(
                self.browse_scroll_y,
                self.browse_view_h,
                row_h,
                n_rows,
                slice,
            );
            browser::soft_scroll_mosaic(
                ui,
                scroll_id,
                content_h,
                browser::pin_mosaic(mosaic.width(Fill).into(), offset),
                Message::BrowseScrolled,
            )
        };
        let content = browser::pane(
            ui,
            Length::Fill,
            self.with_shelf_pills(false, header, scroller, m.gap),
        );
        content
    }

    /// Last played films and series. Live channels are not listed.
    fn history_block(&self, ui: UiTheme, row_w: f32) -> Element<'_, Message> {
        let media: Vec<_> = self
            .settings
            .recent
            .iter()
            .filter(|r| r.is_media())
            .take(8)
            .collect();
        if media.is_empty() {
            return Space::new().height(0).into();
        }
        let total = self.settings.recent.iter().filter(|r| r.is_media()).count();
        let mut rows: Vec<Element<'_, Message>> = Vec::with_capacity(media.len() + 1);
        rows.push(
            text(format!("Historique · {total}"))
                .size(14)
                .color(ui.ink())
                .into(),
        );
        for r in media {
            let known = self
                .bundle
                .channels
                .iter()
                .find(|c| c.id == r.channel_id)
                .cloned();
            let (title, subtitle, open, art) = self.history_presentation(r);
            let thumb = art.as_deref().and_then(|k| {
                crate::images::pick_art(Some(k), None, None, None)
                    .and_then(|url| self.images.get(&url))
                    .or_else(|| self.images.get(k))
            });
            let fav = known.as_ref().map(|c| {
                (
                    self.settings.is_favorite(&c.id),
                    Message::ToggleFavorite(c.id.clone()),
                )
            });
            rows.push(browser::media_row(
                title,
                subtitle,
                open,
                fav,
                ui,
                false,
                thumb,
                40.0,
                row_w,
            ));
        }
        Column::with_children(rows).spacing(4).width(Fill).into()
    }

    fn view_favorites(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        let total = self.browse_index.len();
        let row_h = browser::list_row_height(0.0);
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            total,
            self.browse_overscan(),
        );
        let mut rows: Vec<Element<'_, Message>> = Vec::new();
        if total == 0 {
            rows.push(browser::empty_hint(
                ui,
                "Aucun favori — appuyez sur ★ dans la liste TV.",
            ));
        } else {
            for bi in self.browse_index.window(slice.start, slice.end) {
                let Some(ch) = self.bundle.channels.get(bi) else {
                    continue;
                };
                // Same key as the art prefetch (normalised URL, `tvg-logo` fallback).
                let thumb = crate::images::pick_art(
                    ch.logo.as_deref().or(ch.tvg_logo.as_deref()),
                    None,
                    None,
                    None,
                )
                .and_then(|k| self.images.get(&k));
                // Same cleaned labels as the Live list ("|FR| TF1 FHD" → "TF1 FHD").
                rows.push(browser::media_row(
                    crate::names::display_channel(&ch.name),
                    ch.group
                        .as_deref()
                        .map(crate::names::category_label)
                        .unwrap_or_else(|| "Live".into()),
                    Message::PlayChannelId(ch.id.clone(), ch.source_id),
                    Some((true, Message::ToggleFavorite(ch.id.clone()))),
                    ui,
                    self.selected_channel.as_deref() == Some(ch.id.as_str()),
                    thumb,
                    40.0,
                    m.content_w.max(120.0),
                ));
            }
        }
        let items = if total == 0 {
            Column::with_children(rows).spacing(4).width(Fill)
        } else {
            #[cfg(target_os = "android")]
            {
                Column::with_children(rows).spacing(4).width(Fill)
            }
            #[cfg(not(target_os = "android"))]
            {
                browser::virtual_column(slice, rows)
            }
        };
        let content_h = browser::virtual_content_height(total, self.browse_view_h, row_h);
        let scroller = if total == 0 {
            browser::soft_scroll_on(
                ui,
                items.width(Fill),
                "flux-browse-fav",
                Message::BrowseScrolled,
            )
        } else {
            #[cfg(target_os = "android")]
            {
                browser::soft_scroll_mosaic(
                    ui,
                    "flux-browse-fav",
                    content_h,
                    items.width(Fill),
                    Message::BrowseScrolled,
                )
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = content_h;
                browser::soft_scroll_on(
                    ui,
                    items.width(Fill),
                    "flux-browse-fav",
                    Message::BrowseScrolled,
                )
            }
        };
        browser::pane(
            ui,
            Length::Fill,
            column![
                browser::content_header(
                    ui,
                    "Mes favoris".into(),
                    format!("{total} chaînes"),
                    &self.search,
                    m.search_w,
                    m.title_size,
                    m.stack_header,
                ),
                self.history_block(ui, m.content_w.max(120.0)),
                scroller,
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill),
        )
    }

    /// Players used by the "Système" backend and the "lecteur externe" button.
    fn view_external_players(&self, ui: UiTheme) -> Element<'_, Message> {
        if cfg!(target_os = "android") {
            return text("Lecteur système : sélecteur Android (Intent) à chaque lecture.")
                .size(12)
                .color(ui.ink_muted())
                .into();
        }
        let players = &self.external_players;
        if players.is_empty() {
            return text(if cfg!(windows) {
                "Lecteur externe (optionnel) : aucun trouvé — le lecteur intégré suffit ; \
                 VLC ou mpv installés apparaîtront ici."
            } else if cfg!(target_os = "macos") {
                "Lecteur externe (optionnel) : aucun trouvé — installez VLC, mpv ou IINA."
            } else {
                "Lecteur système : aucun lecteur vidéo trouvé — installez VLC, mpv ou Haruna \
                 (paquet de la distribution ou Flatpak)."
            })
            .size(12)
            .color(ui.ink_muted())
            .into();
        }
        let choice = self.settings.external_player.as_str();
        let chosen_exists = players.iter().any(|p| p.id == choice);
        let auto_label = format!("Auto — {}", players[0].label());
        let mut row = Row::new().spacing(6).push(pill_button(
            text(auto_label),
            Message::SetExternalPlayer(String::new()),
            ui,
            choice.is_empty() || !chosen_exists,
        ));
        for p in players {
            let mut label = p.label();
            if p.is_default {
                label.push_str(" · par défaut");
            }
            if !p.forwards_headers() {
                label.push_str(" · sans en-têtes");
            }
            row = row.push(pill_button(
                text(label),
                Message::SetExternalPlayer(p.id.clone()),
                ui,
                p.id == choice,
            ));
        }
        column![
            text("Lecteur système (backend « Système » et bouton lecteur externe)")
                .size(12)
                .color(ui.ink()),
            row.wrap(),
            text(
                "« sans en-têtes » : le lecteur ne reçoit pas le user-agent / referer du fournisseur ; \
                 certains panels IPTV refusent alors le flux — préférez VLC ou mpv.",
            )
            .size(11)
            .color(ui.ink_muted()),
        ]
        .spacing(6)
        .into()
    }

    fn view_settings(&self, ui: UiTheme) -> Element<'_, Message> {
        let profile = target_profile();
        let empty = Vec::new();
        let backends = self.backends_cache.as_deref().unwrap_or(&empty);
        let mut be_list = Column::new().spacing(4).width(Fill);
        if backends.is_empty() {
            be_list = be_list.push(
                text("Détection backends au prochain affichage…")
                    .size(12)
                    .color(ui.ink_muted()),
            );
        }
        for b in backends {
            let mark = if b.available { "Disponible" } else { "Absent" };
            be_list = be_list.push(
                text(format!("• {} — {} ({})", b.id.label(), b.detail, mark))
                    .size(12)
                    .color(if b.available {
                        ui.ink()
                    } else {
                        ui.ink_muted()
                    }),
            );
        }

        let (title_sz, title_font) = type_style(TypeRole::TitleL, true);
        let settings_search = row![
            container(crate::icons::icon(
                crate::icons::Icon::Search,
                20.0,
                ui.on_surface_variant(),
            ))
            .padding(Padding::from([0, 4])),
            text_input("Rechercher un réglage…", &self.settings_query)
                .on_input(Message::SettingsQuery)
                .padding(12)
                .size(15)
                .width(Fill),
            browser::accent_suffix(ui),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .width(Fill);
        let body = column![
            column![
                text("Réglages")
                    .size(title_sz)
                    .font(title_font)
                    .color(ui.on_surface()),
                text(format!(
                    "{} · {}",
                    profile.platform.label(),
                    profile.ui_shell
                ))
                .size(12)
                .color(ui.on_surface_variant()),
                settings_search,
            ]
            .spacing(6)
            .width(Fill),
            {
                let appearance = settings_card(
                    ui,
                    column![
                        pill_button(
                            text(format!("Thème : {}", self.settings.theme.label())),
                            Message::CycleTheme,
                            ui,
                            false,
                        ),
                        browser::accent_menu(
                            ui,
                            self.accent_open,
                            &self.accent_query,
                            self.custom_open,
                            self.accent_cursor,
                        ),
                        browser::ink_menu(
                            ui,
                            self.settings.custom_ink,
                            self.ink_open,
                            self.accent_cursor,
                        ),
                    ]
                    .spacing(8)
                    .width(Fill),
                );
                let meta = settings_card(
                    ui,
                    column![
                        text(if crate::metadata::tmdb_configured() {
                            "Un jeton est enregistré. Colle-en un autre pour le remplacer."
                        } else {
                            "Aucun jeton. Colle le tien, ou crée-le avec l'assistant."
                        })
                        .size(12)
                        .color(ui.ink_muted()),
                        pill_button(
                            text(if self.api_guide {
                                "Fermer l'assistant"
                            } else {
                                "Créer mon API"
                            }),
                            Message::ApiGuideToggle,
                            ui,
                            self.api_guide,
                        ),
                        api_guide(ui, self.api_guide),
                        field(
                            "Jeton TMDB",
                            &self.form_tmdb_key,
                            Message::FormTmdbKey,
                            Some(PasteTarget::FormTmdbKey),
                            ui,
                        ),
                        row![
                            pill_button_primary(text("Enregistrer le jeton"), Message::SaveTmdbKey, ui),
                            pill_button(text("Effacer"), Message::ClearTmdbKey, ui, false),
                            text(if crate::metadata::tmdb_configured() {
                                "TMDB actif"
                            } else {
                                "TMDB inactif"
                            })
                            .size(12)
                            .color(if crate::metadata::tmdb_configured() {
                                ui.on_surface()
                            } else {
                                ui.ink_muted()
                            }),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .wrap(),
                        text("Clé sur https://www.omdbapi.com/apikey.aspx")
                            .size(12)
                            .color(ui.ink_muted()),
                        field(
                            "Clé API OMDb",
                            &self.form_omdb_key,
                            Message::FormOmdbKey,
                            Some(PasteTarget::FormOmdbKey),
                            ui,
                        ),
                        row![
                            pill_button_primary(text("Enregistrer la clé"), Message::SaveOmdbKey, ui),
                            text(if crate::metadata::omdb_configured() {
                                "OMDb actif"
                            } else {
                                "OMDb inactif"
                            })
                            .size(12)
                            .color(if crate::metadata::omdb_configured() {
                                ui.on_surface()
                            } else {
                                ui.ink_muted()
                            }),
                            {
                                let check: Element<'_, Message> = if crate::metadata::omdb_configured() {
                                    container(crate::icons::icon(
                                        crate::icons::Icon::Check,
                                        18.0,
                                        ui.on_surface(),
                                    ))
                                    .into()
                                } else {
                                    Space::new().width(0).into()
                                };
                                check
                            },
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .wrap(),
                    ]
                    .spacing(8)
                    .width(Fill),
                );
                let language = settings_card(
                    ui,
                    column![
                        {
                            let mut langs = Row::new().spacing(6).push(pill_button(
                                text("Aucune"),
                                Message::SetPrefLang(String::new()),
                                ui,
                                self.settings.pref_lang.is_empty(),
                            ));
                            for (code, label) in crate::names::LANGUAGES {
                                langs = langs.push(pill_button(
                                    text(*label),
                                    Message::SetPrefLang((*code).to_string()),
                                    ui,
                                    self.settings.pref_lang == *code,
                                ));
                            }
                            langs.wrap()
                        },
                        iced::widget::checkbox(self.settings.only_pref_lang)
                            .label("Uniquement les contenus de cette langue")
                            .on_toggle(|_| Message::ToggleOnlyPrefLang)
                            .size(18)
                            .text_size(13)
                            .spacing(8),
                        text(if self.settings.pref_lang.is_empty() {
                            "Choisissez une langue pour le filtre et l’ordre des catégories."
                        } else {
                            "Les synopsis viennent du portail, de TMDB et de Wikipédia dans cette langue ; aucune traduction automatique."
                        })
                        .size(11)
                        .color(ui.ink_muted()),
                    ]
                    .spacing(8)
                    .width(Fill),
                );
                let network = settings_card(
                    ui,
                    column![
                        pill_button(
                            text(format!(
                                "Mode DNS : {}",
                                self.settings.network.dns_mode.label()
                            )),
                            Message::CycleDnsMode,
                            ui,
                            false,
                        ),
                        text(crate::network::dns_status_line(&self.settings.network))
                            .size(12)
                            .color(ui.on_surface_variant()),
                        field(
                            "Serveurs DNS (UDP/TCP)",
                            &self.form_dns_servers,
                            Message::FormDnsServers,
                            Some(PasteTarget::FormDnsServers),
                            ui,
                        ),
                        field(
                            "URL DNS over HTTPS",
                            &self.form_doh_url,
                            Message::FormDohUrl,
                            Some(PasteTarget::FormDohUrl),
                            ui,
                        ),
                        field(
                            "Serveur DNS over TLS",
                            &self.form_dot_server,
                            Message::FormDotServer,
                            Some(PasteTarget::FormDotServer),
                            ui,
                        ),
                        row![
                            pill_button_primary(text("Enregistrer DNS"), Message::SaveNetworkDns, ui),
                            pill_button(text("Tester DNS"), Message::ProbeDns, ui, false),
                        ]
                        .spacing(8)
                        .wrap(),
                        text(if self.network_probe.is_empty() {
                            "Test : résout cloudflare.com.".into()
                        } else {
                            self.network_probe.clone()
                        })
                        .size(12)
                        .color(ui.ink_muted()),
                        text(crate::network::wireguard_status_line(&self.settings.network))
                            .size(12)
                            .color(ui.ink()),
                        row![
                            pill_button(
                                text(if self.settings.network.wireguard_enabled {
                                    if crate::wg_tunnel::tunnel_is_up() {
                                        "WireGuard : ON"
                                    } else {
                                        "WireGuard : démarrage…"
                                    }
                                } else {
                                    "WireGuard : OFF"
                                }),
                                Message::ToggleWireGuard,
                                ui,
                                self.settings.network.wireguard_enabled
                                    && crate::wg_tunnel::tunnel_is_up(),
                            ),
                            pill_button(
                                text("Importer .conf…"),
                                Message::PickWireGuardProfile,
                                ui,
                                false,
                            ),
                            pill_button(
                                text("DNS bootstrap"),
                                Message::ApplyWireGuardDns,
                                ui,
                                false,
                            ),
                            pill_button(
                                text("Supprimer profil"),
                                Message::ClearWireGuardProfile,
                                ui,
                                false,
                            ),
                        ]
                        .spacing(8)
                        .wrap(),
                        field(
                            "Coller un profil WireGuard (.conf)",
                            &self.form_wg_paste,
                            Message::FormWgPaste,
                            Some(PasteTarget::FormWgPaste),
                            ui,
                        ),
                        pill_button(
                            text("Importer le texte collé"),
                            Message::ImportWireGuardPaste,
                            ui,
                            false,
                        ),
                    ]
                    .spacing(8)
                    .width(Fill),
                );
                let playback = settings_card(
                    ui,
                    column![
                        pill_button(
                            text(format!(
                                "Backend : {}",
                                self.settings.player_backend.label()
                            )),
                            Message::CycleBackend,
                            ui,
                            false,
                        ),
                        self.view_external_players(ui),
                        row![
                            pill_button(
                                text(if self.settings.hwdec {
                                    "HW : activé"
                                } else {
                                    "HW : désactivé"
                                }),
                                Message::ToggleHwdec,
                                ui,
                                self.settings.hwdec,
                            ),
                            pill_button(
                                text(if self.settings.low_latency {
                                    "Faible latence : activée"
                                } else {
                                    "Faible latence : désactivée"
                                }),
                                Message::ToggleLowLatency,
                                ui,
                                self.settings.low_latency,
                            ),
                        ]
                        .spacing(8)
                        .wrap(),
                        row![
                            pill_button(
                                text(format!("Cache : {} ms", self.settings.cache_ms)),
                                Message::CycleCache,
                                ui,
                                false,
                            ),
                            pill_button(
                                text(format!("Demux : {:.0} s", self.settings.demux_secs)),
                                Message::CycleDemux,
                                ui,
                                false,
                            ),
                            pill_button(
                                text(if self.settings.remember_position {
                                    "Reprise : activée"
                                } else {
                                    "Reprise : désactivée"
                                }),
                                Message::ToggleRememberPosition,
                                ui,
                                self.settings.remember_position,
                            ),
                        ]
                        .spacing(8)
                        .wrap(),
                        text(format!("Volume par défaut : {:.0}%", self.settings.volume * 100.0))
                            .size(12)
                            .color(ui.ink_muted()),
                    ]
                    .spacing(8)
                    .width(Fill),
                );
                let picture = settings_card(
                    ui,
                    column![
                        pill_button(
                            text({
                                let choice = if self.settings.gpu_choice.is_empty() {
                                    self.display_caps.probe.gpu_topology.display.label()
                                } else {
                                    self.display_caps
                                        .probe
                                        .gpu_topology
                                        .devices
                                        .iter()
                                        .find(|d| d.name == self.settings.gpu_choice)
                                        .map(|d| d.label())
                                        .unwrap_or_else(|| self.settings.gpu_choice.clone())
                                };
                                format!("GPU : {choice}")
                            }),
                            Message::CycleGpu,
                            ui,
                            false,
                        ),
                        text(self.display_caps.summary_line())
                            .size(12)
                            .color(ui.on_surface_variant()),
                        row![
                            pill_button(
                                text(format!(
                                    "GUI {} fps",
                                    self.display_caps.gui_hz
                                )),
                                Message::CycleFpsGui,
                                ui,
                                false,
                            ),
                            pill_button(
                                text(format!(
                                    "Vidéo {} fps",
                                    self.display_caps.video_hz
                                )),
                                Message::CycleFpsVideo,
                                ui,
                                false,
                            ),
                            pill_button(
                                text("Rescan"),
                                Message::RefreshDisplayCaps,
                                ui,
                                false,
                            ),
                        ]
                        .spacing(8)
                        .wrap(),
                        row![
                            pill_button(
                                text(format!("{}", self.settings.video_quality.label())),
                                Message::CycleVideoQuality,
                                ui,
                                false,
                            ),
                            pill_button(
                                text(format!("{}", self.settings.hdr_mode.label())),
                                Message::CycleHdrMode,
                                ui,
                                false,
                            ),
                            pill_button(
                                text(format!("{}", self.settings.display_panel.label())),
                                Message::CycleDisplayPanel,
                                ui,
                                false,
                            ),
                        ]
                        .spacing(8)
                        .wrap(),
                        row![
                            if cfg!(target_os = "android") {
                                pill_button(
                                    text(format!(
                                        "Présent : {}",
                                        self.settings.android_present.label()
                                    )),
                                    Message::CycleAndroidPresentPref,
                                    ui,
                                    false,
                                )
                            } else {
                                Space::new().width(0).into()
                            },
                            pill_button(
                                text(if self.settings.tonemap_hdr {
                                    "Tonemap HDR : activé"
                                } else {
                                    "Tonemap HDR : désactivé"
                                }),
                                Message::ToggleTonemapHdr,
                                ui,
                                self.settings.tonemap_hdr,
                            ),
                            pill_button(
                                text(format!("Format : {}", self.settings.aspect.label())),
                                Message::CycleDefaultAspect,
                                ui,
                                false,
                            ),
                            pill_button(
                                text(format!("{}", self.settings.deinterlace.label())),
                                Message::CycleDefaultDeinterlace,
                                ui,
                                false,
                            ),
                            pill_button(
                                text(format!("Upscale : {}", self.settings.upscale.label())),
                                Message::CycleDefaultUpscale,
                                ui,
                                false,
                            ),
                            pill_button(
                                text(if self.settings.night_mode {
                                    "Mode nuit : ON"
                                } else {
                                    "Mode nuit : off"
                                }),
                                Message::ToggleDefaultNightMode,
                                ui,
                                self.settings.night_mode,
                            ),
                        ]
                        .spacing(8)
                        .wrap(),
                    ]
                    .spacing(8)
                    .width(Fill),
                );
                let library = settings_card(
                    ui,
                    column![
                        text("Masquer par âge")
                            .size(14)
                            .color(ui.on_surface()),
                        text("Les plus jeunes, puis les plus âgés. Un titre sans âge reste visible.")
                            .size(12)
                            .color(ui.ink_muted()),
                        text("Masquer les plus jeunes")
                            .size(12)
                            .color(ui.on_surface_variant()),
                        {
                            let young = [
                                (None, "Aucun"),
                                (Some(0), "Tout public"),
                                (Some(7), "7+"),
                                (Some(10), "10+"),
                                (Some(12), "12+"),
                                (Some(13), "13+"),
                            ];
                            let mut row = Row::new().spacing(8);
                            for (age, label) in young {
                                row = row.push(pill_button(
                                    text(label),
                                    Message::SetHideAgeUnder(age),
                                    ui,
                                    self.settings.hide_age_under == age,
                                ));
                            }
                            row.wrap()
                        },
                        text("Masquer les plus âgés")
                            .size(12)
                            .color(ui.on_surface_variant()),
                        {
                            let older = [
                                (None, "Aucun"),
                                (Some(13), "13+"),
                                (Some(16), "16+"),
                                (Some(18), "18+"),
                            ];
                            let mut row = Row::new().spacing(8);
                            for (age, label) in older {
                                row = row.push(pill_button(
                                    text(label),
                                    Message::SetHideAgeOver(age),
                                    ui,
                                    self.settings.hide_age_over == age,
                                ));
                            }
                            row.wrap()
                        },
                        pill_button(
                            text(if self.settings.skip_credits_auto {
                                "Passer le générique : automatique"
                            } else {
                                "Passer le générique : à la main"
                            }),
                            Message::ToggleSkipCredits,
                            ui,
                            self.settings.skip_credits_auto,
                        ),
                        text("Au début d'un film ou d'une série, et sur les dernières secondes. Le direct n'est pas touché.")
                            .size(12)
                            .color(ui.ink_muted()),
                        pill_button(
                            text(if self.settings.prefetch_next_episode {
                                "Précharge épisode suivant : activée"
                            } else {
                                "Précharge épisode suivant : désactivée"
                            }),
                            Message::TogglePrefetchNext,
                            ui,
                            self.settings.prefetch_next_episode,
                        ),
                        text("Qualité d'affichage")
                            .size(14)
                            .color(ui.on_surface()),
                        text("Le lecteur en cours et les prochaines lectures.")
                            .size(12)
                            .color(ui.ink_muted()),
                        {
                            let mut choices = Row::new().spacing(8);
                            for quality in VideoQualityPref::all() {
                                choices = choices.push(pill_button(
                                    text(quality.short_label()),
                                    Message::SetVideoQuality(*quality),
                                    ui,
                                    self.settings.video_quality == *quality,
                                ));
                            }
                            choices.wrap()
                        },
                        text(format!(
                            "Dossier : {}{}",
                            self.downloads_dir().display(),
                            if self.settings.download_dir.is_empty() {
                                " (par défaut)"
                            } else {
                                ""
                            }
                        ))
                        .size(12)
                        .color(ui.on_surface_variant()),
                        text("Les films vont dans films, chaque série dans series\\titre, même pour un seul épisode. Tout reste dans ce dossier.")
                            .size(12)
                            .color(ui.ink_muted()),
                        field(
                            "Dossier des téléchargements",
                            &self.form_download_dir,
                            Message::FormDownloadDir,
                            None,
                            ui,
                        ),
                        {
                            let mut actions = row![pill_button_primary(
                                text("Enregistrer"),
                                Message::SaveDownloadDir,
                                ui
                            )]
                            .spacing(8);
                            #[cfg(not(target_os = "android"))]
                            {
                                actions = actions.push(pill_button(
                                    text("Parcourir…"),
                                    Message::PickDownloadDir,
                                    ui,
                                    false,
                                ));
                                actions = actions.push(pill_button(
                                    text("Ouvrir"),
                                    Message::OpenDownloadsDir,
                                    ui,
                                    false,
                                ));
                            }
                            if !self.settings.download_dir.is_empty() {
                                actions = actions.push(pill_button(
                                    text("Dossier par défaut"),
                                    Message::ResetDownloadDir,
                                    ui,
                                    false,
                                ));
                            }
                            actions.wrap()
                        },
                    ]
                    .spacing(8)
                    .width(Fill),
                );
                let backends_card = settings_card(
                    ui,
                    column![
                        be_list,
                        text(
                            "Image en lecture : panneau ⋯ du lecteur. FLUXPLAY_VERBOSE=1 pour les logs.",
                        )
                        .size(11)
                        .color(ui.ink_muted()),
                    ]
                    .spacing(8)
                    .width(Fill),
                );
                let q = self.settings_query.trim().to_lowercase();
                let sources = self.view_sources(ui);
                let entries = [
                    (SettingsSection::Appearance, appearance),
                    (SettingsSection::Sources, sources),
                    (SettingsSection::Meta, meta),
                    (SettingsSection::Language, language),
                    (SettingsSection::Network, network),
                    (SettingsSection::Playback, playback),
                    (SettingsSection::Remote, self.view_remote(ui)),
                    (SettingsSection::Picture, picture),
                    (SettingsSection::Library, library),
                    (SettingsSection::Backends, backends_card),
                ];
                let mut menus = Column::new().spacing(8).width(Fill);
                let mut any = false;
                for (section, body) in entries {
                    let blob = format!(
                        "{} {} {}",
                        section.title(),
                        section.hint(),
                        section.keywords()
                    )
                    .to_lowercase();
                    if !q.is_empty() && !blob.contains(&q) {
                        continue;
                    }
                    any = true;
                    let open = !q.is_empty()
                        || self.settings_open == Some(section)
                        || (section == SettingsSection::Sources && self.tab == Tab::Sources);
                    let hint = if section == SettingsSection::Meta {
                        if crate::metadata::tmdb_configured() {
                            "Jeton enregistré. Ouvre pour le changer ou l'effacer."
                        } else {
                            "Aucun jeton. Ouvre pour en coller un."
                        }
                    } else {
                        section.hint()
                    };
                    menus = menus.push(settings_fold(ui, section, open, hint, body));
                }
                if !any {
                    menus = menus.push(
                        text("Aucun réglage ne correspond à cette recherche.")
                            .size(14)
                            .color(ui.ink_muted()),
                    );
                }
                let cols: Element<'_, Message> = menus.into();
                cols
            },
        ]
        .spacing(10)
        .width(Fill);
        browser::pane(
            ui,
            Length::Fill,
            browser::soft_scroll(ui, body),
        )
    }

    fn view_sources(&self, ui: UiTheme) -> Element<'_, Message> {
        let kinds = [
            SourceKind::M3u,
            SourceKind::M3uPlus,
            SourceKind::Xtream,
            SourceKind::Stalker,
            SourceKind::Xmltv,
            SourceKind::DirectUrl,
        ];
        let kind_row = Row::with_children(kinds.into_iter().map(|k| {
            chip(
                k.label().to_string(),
                Message::FormKind(k),
                ui,
                self.form_kind == k,
            )
        }))
        .spacing(8)
        .wrap();

        let editing = self
            .editing_source
            .and_then(|id| self.sources.iter().find(|s| s.id == id));
        let form_title = match editing {
            Some(s) => format!("Modifier — {}", s.name),
            None => "Nouvelle source".to_string(),
        };
        let mirrors_field: Element<'_, Message> = if self.form_kind == SourceKind::Xmltv {
            Space::new().height(0).into()
        } else {
            field(
                "Serveurs supplémentaires (optionnel, même abonnement — séparés par des espaces)",
                &self.form_mirrors,
                Message::FormMirrors,
                Some(PasteTarget::FormMirrors),
                ui,
            )
        };
        let submit_label = if editing.is_some() { "Enregistrer" } else { "Ajouter" };
        let cancel_edit: Element<'_, Message> = if editing.is_some() {
            pill_button(text("Annuler"), Message::CancelEditSource, ui, false)
        } else {
            Space::new().width(0).into()
        };
        let form = column![
            text(form_title).size(16),
            kind_row,
            match &self.form_error {
                Some(err) => Element::from(
                    container(text(err.clone()).size(13).color(ui.error()))
                        .padding([8, 12])
                        .width(Length::Fill)
                        .style(move |_| container::Style {
                            background: Some(Background::Color(ui.error_container())),
                            border: Border {
                                radius: 10.0.into(),
                                ..Border::default()
                            },
                            ..container::Style::default()
                        }),
                ),
                None => Space::new().height(0).into(),
            },
            field(
                "Nom (optionnel — déduit de l’URL)",
                &self.form_name,
                Message::FormName,
                Some(PasteTarget::FormName),
                ui,
            ),
            field(
                match self.form_kind {
                    SourceKind::Xtream => "URL serveur",
                    SourceKind::Stalker => "URL portail",
                    SourceKind::Xmltv => "URL XMLTV",
                    _ => "URL / chemin / corps M3U",
                },
                &self.form_endpoint,
                Message::FormEndpoint,
                Some(PasteTarget::FormEndpoint),
                ui,
            ),
            if matches!(self.form_kind, SourceKind::Xtream) {
                row![
                    field(
                        "Utilisateur",
                        &self.form_user,
                        Message::FormUser,
                        Some(PasteTarget::FormUser),
                        ui,
                    ),
                    field(
                        "Mot de passe",
                        &self.form_pass,
                        Message::FormPass,
                        Some(PasteTarget::FormPass),
                        ui,
                    ),
                ]
                .spacing(8)
                .into()
            } else if self.form_kind == SourceKind::Stalker {
                field(
                    "Adresse MAC",
                    &self.form_mac,
                    Message::FormMac,
                    Some(PasteTarget::FormMac),
                    ui,
                )
            } else {
                Space::new().height(0).into()
            },
            mirrors_field,
            field(
                "EPG XMLTV (optionnel)",
                &self.form_epg,
                Message::FormEpg,
                Some(PasteTarget::FormEpg),
                ui,
            ),
            row![
                pill_button_primary(
                    crate::icons::icon_label(crate::icons::Icon::Add, submit_label, 14.0, ui.on_surface()),
                    Message::AddSource,
                    ui,
                ),
                cancel_edit,
                pill_button(
                    crate::icons::icon_label(
                        crate::icons::Icon::FolderOpen,
                        "Fichier M3U…",
                        14.0,
                        ui.on_surface(),
                    ),
                    Message::PickPlaylistFile,
                    ui,
                    false,
                ),
                pill_button(
                    crate::icons::icon_label(
                        crate::icons::Icon::Diagnose,
                        "Diagnostiquer",
                        14.0,
                        ui.on_surface(),
                    ),
                    Message::DiagnosePortals,
                    ui,
                    false,
                ),
            ]
            .spacing(8)
            .wrap(),
            text("Playlists publiques (iptv-org — free-to-air, pas de Xtream pirate)")
                .size(12)
                .color(ui.ink_muted()),
            Row::with_children(demo::public_demo_catalog().into_iter().map(|(name, url)| {
                pill_button(
                    text(name.clone()),
                    Message::AddPublicDemo {
                        name,
                        endpoint: url,
                    },
                    ui,
                    false,
                )
            }))
            .spacing(8)
            .wrap(),
        ]
        .spacing(8);

        let editing_id = self.editing_source;
        let mut form_slot = Some(form);
        let mut list = Column::new().spacing(SPACE_SM).width(Fill);
        for s in &self.sources {
            let state = if s.enabled { "actif" } else { "désactivé" };
            let mut meta = format!("{} · {} · {}", s.kind.label(), state, redact_endpoint(&s.endpoint));
            let extra = s.endpoints().len().saturating_sub(1);
            if extra > 0 {
                meta.push_str(&format!(
                    " · +{extra} serveur{}",
                    if extra == 1 { "" } else { "s" }
                ));
            }
            list = list.push(browser::source_card(
                s.name.clone(),
                meta,
                Message::EditSource(s.id),
                Message::ReloadSource(s.id),
                Message::ExportProfile(s.id),
                Message::RemoveSource(s.id),
                ui,
            ));
            if editing_id == Some(s.id) {
                if let Some(form) = form_slot.take() {
                    list = list.push(form);
                }
            }
        }
        let new_form: Element<'_, Message> = match form_slot.take() {
            Some(form) => form.into(),
            None => Space::new().height(0).into(),
        };

        let list_block: Element<'_, Message> = if self.sources.is_empty() {
            browser::empty_hint(ui, "Aucune source — ajoutez un portail ou une playlist.")
        } else {
            list.width(Fill).into()
        };

        column![
            text(format!(
                "{} source{}",
                self.sources.len(),
                if self.sources.len() == 1 { "" } else { "s" }
            ))
            .size(13)
            .color(ui.ink_muted()),
            new_form,
            row![
                text("Sources enregistrées")
                    .size(14)
                    .color(ui.on_surface_variant()),
                Space::new().width(Fill),
                pill_button(
                    crate::icons::icon_label(
                        crate::icons::Icon::FolderOpen,
                        "Importer un profil",
                        14.0,
                        ui.on_surface(),
                    ),
                    Message::ImportProfile,
                    ui,
                    false,
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text("L’export .fluxplay contient les identifiants portail")
                .size(11)
                .color(ui.ink_muted()),
            list_block,
        ]
        .spacing(8)
        .width(Fill)
        .into()
    }
}

/// A portal shelf that only names the format (« Films », « Films 4K », « VOST »),
/// not a subject. Those words are not shown under the poster.
/// Kids and adult shelves only. A comedy shelf is not an age.
/// Title plus genre, so a new hentai filed under manga still reaches Animé adulte.
fn shelf_probe(name: &str, genre: Option<&str>) -> String {
    match genre.map(str::trim).filter(|g| !g.is_empty()) {
        Some(genre) => format!("{name} {genre}"),
        None => name.to_string(),
    }
}

fn shelf_age_label(name: &str) -> Option<String> {
    let n = name.to_lowercase();
    if n.contains("adulte")
        || n.contains("+18")
        || n.contains("xxx")
        || n.contains("porn")
        || n.contains("hentai")
    {
        Some("18+".into())
    } else if (n.contains("enfant") || n.contains("jeunesse") || n.contains("kids"))
        && !n.contains("animation")
        && !n.contains("manga")
        && !n.contains("anime")
    {
        // "Animation | Familial" is a mixed shelf, not a rating.
        Some("Tous".into())
    } else {
        None
    }
}

fn film_shelf_key(name: &str) -> String {
    let title = crate::metadata::parse_title_query(name).title;
    title
        .chars()
        .filter_map(|c| match c {
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => Some('e'),
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'À' | 'Á' | 'Â' => Some('a'),
            'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' => Some('u'),
            'î' | 'ï' | 'Î' | 'Ï' => Some('i'),
            'ô' | 'ö' | 'õ' | 'Ô' | 'Ö' => Some('o'),
            'ç' | 'Ç' => Some('c'),
            other if other.is_ascii_alphanumeric() => Some(other.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

fn shelf_caption(name: &str) -> Option<String> {
    let label = crate::names::category_label(name);
    let piece = label.rsplit('·').next().unwrap_or(label.as_str()).trim();
    let labels: Vec<String> = crate::metadata::display_genres(piece)
        .into_iter()
        .filter(|l| !is_generic_shelf(l))
        .take(3)
        .collect();
    if labels.is_empty() {
        None
    } else {
        Some(labels.join(" · "))
    }
}

fn is_generic_shelf(label: &str) -> bool {
    let folded: String = label
        .chars()
        .map(|c| match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'à' | 'á' | 'â' | 'ä' | 'ã' => 'a',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'î' | 'ï' => 'i',
            'ô' | 'ö' | 'õ' => 'o',
            'ç' => 'c',
            'ñ' => 'n',
            other => other.to_ascii_lowercase(),
        })
        .collect();
    let words: Vec<&str> = folded
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return true;
    }
    words.iter().all(|w| is_shelf_bucket_word(w))
}

fn is_shelf_bucket_word(w: &str) -> bool {
    if w.len() <= 1 || w.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    matches!(
        w,
        "film"
            | "films"
            | "filme"
            | "filmes"
            | "filmler"
            | "filmi"
            | "movie"
            | "movies"
            | "serie"
            | "series"
            | "seri"
            | "dizi"
            | "4k"
            | "uhd"
            | "fhd"
            | "hd"
            | "hevc"
            | "hdr"
            | "dv"
            | "light"
            | "vost"
            | "vostfr"
            | "vf"
            | "multi"
            | "sub"
            | "nouveaute"
            | "nouveautes"
            | "recenti"
            | "recente"
            | "recentes"
            | "nuovo"
            | "nuovi"
            | "ancien"
            | "anciens"
            | "ancienne"
            | "anciennes"
            | "antico"
            | "antichi"
            | "yerli"
            | "80s"
            | "90s"
            | "70s"
            | "60s"
            | "2000s"
    )
}

fn sanitize_sources(sources: &mut [MediaSource]) {
    let has_xtream = sources.iter().any(|s| s.kind == SourceKind::Xtream && s.enabled);
    if has_xtream {
        for s in sources.iter_mut() {
            if matches!(
                s.kind,
                SourceKind::M3u | SourceKind::M3uPlus | SourceKind::DirectUrl
            ) && s.endpoint.contains("get.php")
            {
                s.enabled = false;
            }
        }
    }
    // Same XC account on 2 portals = duplicate 40k catalog — keep first only.
    let mut seen_creds = std::collections::HashSet::new();
    for s in sources.iter_mut() {
        if s.kind == SourceKind::Xtream && s.enabled {
            if let (Some(u), Some(p)) = (&s.username, &s.password) {
                let key = format!("{u}\0{p}");
                if !seen_creds.insert(key) {
                    s.enabled = false;
                }
            }
        }
    }
}

/// Adult shelf searches the whole catalog, then keeps the matching cartoons.
/// Every other shelf stays inside its portal category.
fn shelf_db_category(selected: Option<&str>) -> Option<&str> {
    let id = selected?;
    match crate::names::category_shelf(id).1 {
        crate::names::CategoryShelf::Adult => None,
        _ => Some(crate::names::category_shelf(id).0),
    }
}

// Off-thread index build; each arg is an owned snapshot — a params struct adds indirection only.
#[allow(clippy::too_many_arguments)]
fn build_browse_index_blocking(
    tab: Tab,
    q: String,
    group: Option<String>,
    vod_cat: Option<String>,
    series_cat: Option<String>,
    adult_pool: Vec<String>,
    strip_adult: bool,
    source_ids: Vec<Uuid>,
    live_rows: Vec<(usize, String, Option<String>)>,
    vod_rows: Vec<(usize, String, String, Option<String>)>,
    series_rows: Vec<(usize, String, String, Option<String>)>,
    lang: Option<std::sync::Arc<LangCtx>>,
    progress: std::sync::Arc<crate::async_jobs::JobProgress>,
) -> (BrowseIndex, Vec<EpisodeFlat>) {
    use crate::async_jobs::DEFAULT_CHUNK;
    let q_lc = if q.is_empty() {
        None
    } else {
        Some(q.to_lowercase())
    };
    let index = match tab {
        Tab::Live | Tab::Epg => {
            progress.set_total(live_rows.len() as u64);
            let mut out = Vec::with_capacity(live_rows.len().min(65_536));
            let mut idle = Vec::new();
            for chunk in live_rows.chunks(DEFAULT_CHUNK) {
                for (i, name, g) in chunk {
                    let group_ok = match group.as_deref() {
                        None | Some("Tous") | Some("*") => true,
                        Some(want) => g.as_deref() == Some(want),
                    };
                    let q_ok = q_lc
                        .as_deref()
                        .map(|ql| name.to_lowercase().contains(ql))
                        .unwrap_or(true);
                    let lang_ok = group_ok
                        && q_ok
                        && lang
                            .as_deref()
                            .map(|l| l.keeps_channel(name, g.as_deref()))
                            .unwrap_or(true);
                    if lang_ok {
                        // Empty PPV / event slots go after the real channels.
                        if crate::names::is_placeholder_channel(name) {
                            idle.push(*i);
                        } else {
                            out.push(*i);
                        }
                    }
                }
                progress.add_done(chunk.len() as u64);
            }
            out.extend(idle);
            BrowseIndex::mapped(out)
        }
        Tab::Vod => {
            let cat = match vod_cat.as_deref() {
                None | Some("*") => None,
                Some(id) => Some(id),
            };
            if let Some(ql) = q_lc.as_deref().filter(|s| !s.is_empty()) {
                let _ = ql;
                if !q.is_empty() {
                    if let Some(db) = crate::catalog_db::CatalogDb::open(&source_ids) {
                        let found = db.search_vod_ids(&q, shelf_db_category(cat), 50_000);
                        let want: std::collections::HashSet<&str> =
                            found.iter().map(String::as_str).collect();
                        progress.set_total(vod_rows.len() as u64);
                        let mut out = Vec::with_capacity(found.len());
                        for chunk in vod_rows.chunks(DEFAULT_CHUNK) {
                            for (i, id, name, c) in chunk {
                                let cat_ok = cat
                                    .map(|cid| {
                                        crate::names::in_category_shelf(
                                            cid,
                                            c.as_deref(),
                                            name,
                                            &adult_pool,
                                            strip_adult,
                                        )
                                    })
                                    .unwrap_or(true);
                                if cat_ok
                                    && want.contains(id.as_str())
                                    && lang
                                        .as_deref()
                                        .map(|l| l.keeps_title(name, c.as_deref()))
                                        .unwrap_or(true)
                                {
                                    out.push(*i);
                                }
                            }
                            progress.add_done(chunk.len() as u64);
                        }
                        return (BrowseIndex::mapped(out), Vec::new());
                    }
                }
            }
            progress.set_total(vod_rows.len() as u64);
            let mut out = Vec::with_capacity(vod_rows.len().min(65_536));
            for chunk in vod_rows.chunks(DEFAULT_CHUNK) {
                for (i, _id, name, c) in chunk {
                    let cat_ok = cat
                        .map(|cid| {
                            crate::names::in_category_shelf(
                                cid,
                                c.as_deref(),
                                name,
                                &adult_pool,
                                strip_adult,
                            )
                        })
                        .unwrap_or(true);
                    let q_ok = q_lc
                        .as_deref()
                        .map(|ql| {
                            if name.is_empty() {
                                true
                            } else {
                                name.to_lowercase().contains(ql)
                            }
                        })
                        .unwrap_or(true);
                    let lang_ok = || {
                        lang.as_deref()
                            .map(|l| l.keeps_title(name, c.as_deref()))
                            .unwrap_or(true)
                    };
                    if cat_ok && q_ok && lang_ok() {
                        out.push(*i);
                    }
                }
                progress.add_done(chunk.len() as u64);
            }
            BrowseIndex::mapped(out)
        }
        Tab::Series => {
            let cat = match series_cat.as_deref() {
                None | Some("*") => None,
                Some(id) => Some(id),
            };
            if !q.is_empty() {
                if let Some(db) = crate::catalog_db::CatalogDb::open(&source_ids) {
                    let found = db.search_series_ids(&q, shelf_db_category(cat), 50_000);
                    let want: std::collections::HashSet<&str> =
                        found.iter().map(String::as_str).collect();
                    progress.set_total(series_rows.len() as u64);
                    let mut out = Vec::with_capacity(found.len());
                    for chunk in series_rows.chunks(DEFAULT_CHUNK) {
                        for (i, id, name, c) in chunk {
                            let cat_ok = cat
                                .map(|cid| {
                                    crate::names::in_category_shelf(
                                        cid,
                                        c.as_deref(),
                                        name,
                                        &adult_pool,
                                        strip_adult,
                                    )
                                })
                                .unwrap_or(true);
                            if cat_ok
                                && want.contains(id.as_str())
                                && lang
                                    .as_deref()
                                    .map(|l| l.keeps_title(name, c.as_deref()))
                                    .unwrap_or(true)
                            {
                                out.push(*i);
                            }
                        }
                        progress.add_done(chunk.len() as u64);
                    }
                    return (BrowseIndex::mapped(out), Vec::new());
                }
            }
            progress.set_total(series_rows.len() as u64);
            let mut out = Vec::with_capacity(series_rows.len().min(65_536));
            for chunk in series_rows.chunks(DEFAULT_CHUNK) {
                for (i, _id, name, c) in chunk {
                    let cat_ok = cat
                        .map(|cid| {
                            crate::names::in_category_shelf(
                                cid,
                                c.as_deref(),
                                name,
                                &adult_pool,
                                strip_adult,
                            )
                        })
                        .unwrap_or(true);
                    let q_ok = q_lc
                        .as_deref()
                        .map(|ql| {
                            if name.is_empty() {
                                true
                            } else {
                                name.to_lowercase().contains(ql)
                            }
                        })
                        .unwrap_or(true);
                    let lang_ok = || {
                        lang.as_deref()
                            .map(|l| l.keeps_title(name, c.as_deref()))
                            .unwrap_or(true)
                    };
                    if cat_ok && q_ok && lang_ok() {
                        out.push(*i);
                    }
                }
                progress.add_done(chunk.len() as u64);
            }
            BrowseIndex::mapped(out)
        }
        _ => BrowseIndex::Empty,
    };
    let (d, t) = progress.snapshot();
    progress.set_done(t.max(d));
    (index, Vec::new())
}

fn pick_default_live_group(bundle: &PlaylistBundle) -> Option<String> {
    let live: Vec<_> = bundle
        .categories
        .iter()
        .filter(|c| c.content == ContentKind::Live && !is_adult_cat(&c.name))
        .collect();
    live.iter()
        .find(|c| {
            let n = c.name.to_ascii_uppercase();
            n.contains("|FR|") || n.contains(" FR ") || n.starts_with("FR")
        })
        .or_else(|| live.first())
        .map(|c| c.name.clone())
        .or_else(|| bundle.group_names().into_iter().find(|n| !is_adult_cat(n)))
}

/// Windows UI language, so a French system starts on the French audio track.
fn system_ui_lang() -> String {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetUserDefaultUILanguage() -> u16;
        }
        let primary = unsafe { GetUserDefaultUILanguage() } & 0x3FF;
        let code = match primary {
            0x0C => "fr",
            0x09 => "en",
            0x07 => "de",
            0x0A => "es",
            0x10 => "it",
            0x11 => "ja",
            0x12 => "ko",
            0x16 => "pt",
            0x04 => "zh",
            0x13 => "nl",
            0x19 => "ru",
            0x01 => "ar",
            _ => "",
        };
        if !code.is_empty() {
            return code.to_string();
        }
    }
    std::env::var("LANG")
        .ok()
        .and_then(|lang| {
            let code = lang.split(['_', '.', '@']).next()?.trim().to_ascii_lowercase();
            (!code.is_empty() && code != "c").then_some(code)
        })
        .unwrap_or_else(|| "en".into())
}

fn play_options_from(settings: &AppSettings) -> PlayOptions {
    let preferred = match std::env::var("FLUXPLAY_BACKEND")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "mpv" | "libmpv" => PlayerBackendPref::Mpv,
        "ffmpeg" | "ffplay" => PlayerBackendPref::Ffmpeg,
        "auto" => PlayerBackendPref::Auto,
        "external" => PlayerBackendPref::External,
        _ => settings.player_backend,
    };
    let mut opts = PlayOptions {
        user_agent: Some("IPTVSmartersPlayer".into()),
        referer: None,
        extra_headers: Vec::new(),
        hwdec: settings.hwdec,
        cache_ms: settings.cache_ms,
        demux_secs: settings.demux_secs,
        volume: settings.volume,
        low_latency: settings.low_latency,
        preferred,
        // Enabled but not up yet: a refused proxy, never the clearnet.
        http_proxy: crate::wg_tunnel::player_proxy_url().or_else(|| {
            settings.network
                .wireguard_enabled
                .then(|| "http://127.0.0.1:9".to_string())
        }),
        android_present: Default::default(),
        android_surface_wid: None,
        android_surface_wh: None,
        android_soft_vf: None,
        vaapi_device: None,
        hwdec_force_copy: false,
        tonemap_hdr: settings.tonemap_hdr,
        video_max_wh: settings.video_quality.max_wh(),
        hdr_mode: settings.hdr_mode,
        display_panel: settings.display_panel,
        external_player: Some(settings.external_player.clone()).filter(|s| !s.is_empty()),
        media_title: None,
    };
    apply_gpu_topology_opts(&mut opts, &settings.gpu_choice);
    // Mode only — never acquire Surface here (called from FluxPlay::new / settings).
    apply_android_present_mode(&mut opts, settings);
    opts
}

fn export_gpu_env(gpu_choice: &str) {
    #[cfg(not(target_os = "android"))]
    {
        let dc = crate::display_caps::resolve_caps(
            fluxplay_core::models::FpsCapPref::Auto,
            fluxplay_core::models::FpsCapPref::Auto,
            None,
            None,
            None,
        );
        let topo = &dc.probe.gpu_topology;
        let dev = topo
            .devices
            .iter()
            .find(|d| d.name == gpu_choice)
            .unwrap_or(&topo.display);
        std::env::set_var("FLUXPLAY_GPU", &dev.name);
        match &dev.render_node {
            Some(node) => std::env::set_var("FLUXPLAY_GPU_NODE", node),
            None => std::env::remove_var("FLUXPLAY_GPU_NODE"),
        }
    }
    #[cfg(target_os = "android")]
    {
        let _ = gpu_choice;
    }
}

/// Decode and display stay on the GPU the user picked (default: the panel GPU).
fn apply_gpu_topology_opts(opts: &mut PlayOptions, gpu_choice: &str) {
    #[cfg(not(target_os = "android"))]
    {
        let dc = crate::display_caps::resolve_caps(
            fluxplay_core::models::FpsCapPref::Auto,
            fluxplay_core::models::FpsCapPref::Auto,
            None,
            None,
            None,
        );
        let topo = &dc.probe.gpu_topology;
        let dev = topo
            .devices
            .iter()
            .find(|d| d.name == gpu_choice)
            .unwrap_or(&topo.display);
        opts.hwdec_force_copy = false;
        opts.vaapi_device = dev.render_node.clone();
        export_gpu_env(&dev.name);
        tracing::info!(gpu = %dev.label(), "single GPU for decode and display");
    }
    #[cfg(target_os = "android")]
    {
        let _ = (opts, gpu_choice);
        // Android Surface = same SoC for decode+display (zero-copy). Soft = copy on SoC.
    }
}

/// Select Soft vs SurfaceEmbed without touching the SurfaceView (safe at boot).
fn apply_android_present_mode(opts: &mut PlayOptions, settings: &AppSettings) {
    #[cfg(target_os = "android")]
    {
        let caps = crate::android_bridge::poll_android_device_caps().unwrap_or_else(|| {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4) as u32;
            fluxplay_player::AndroidDeviceCaps {
                cores,
                mediacodec_video: true,
                refresh_hz: 60,
                ..Default::default()
            }
        });
        let quality = settings.video_quality.max_wh();
        let budget = caps.soft_budget_with_quality(quality);
        opts.android_soft_vf = Some(budget.vf_scale());
        opts.android_present = match settings.android_present {
            AndroidPresentPref::Auto => caps.select_present_mode(),
            AndroidPresentPref::Surface => fluxplay_player::AndroidPresentMode::SurfaceEmbed,
            AndroidPresentPref::Soft => fluxplay_player::AndroidPresentMode::SoftRgba,
            AndroidPresentPref::GpuEgl => fluxplay_player::AndroidPresentMode::GpuEgl,
        };
        opts.android_surface_wid = None;
        opts.android_surface_wh = None;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (opts, settings);
    }
}

/// Phase A–C: walk present ladder, attach wid / HDR / Hz, soft vf budget before loadfile.
/// Returns true when Soft was selected only because Surface bind missed — caller may promote later.
fn bind_android_surface_for_play(
    opts: &mut PlayOptions,
    settings: &AppSettings,
    force_soft: bool,
    soft_wh: (u32, u32),
) -> bool {
    #[cfg(target_os = "android")]
    {
        use fluxplay_player::{AndroidPresentMode, SoftBudget};
        let caps = crate::android_bridge::poll_android_device_caps().unwrap_or_else(|| {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4) as u32;
            fluxplay_player::AndroidDeviceCaps {
                cores,
                mediacodec_video: true,
                refresh_hz: 60,
                ..Default::default()
            }
        });
        let budget = caps.soft_budget_with_quality(settings.video_quality.max_wh());
        // Seed vf from live stage (portrait Pixel), not landscape-only soft budget.
        let (sw, sh) = soft_wh;
        let seed = SoftBudget {
            max_w: sw.min(budget.max_w).max(2) & !1,
            max_h: sh.min(budget.max_h).max(2) & !1,
            video_hz: budget.video_hz,
            gui_hz: budget.gui_hz,
        };
        opts.android_soft_vf = Some(seed.vf_scale());

        if force_soft {
            opts.android_present = AndroidPresentMode::SoftRgba;
            opts.android_surface_wid = None;
            opts.android_surface_wh = None;
            crate::android_bridge::release_video_surface_wid();
            crate::android_bridge::set_video_surface_visible(false);
            tracing::info!(
                soft_vf = ?opts.android_soft_vf,
                "android soft present forced (surface demotion)"
            );
            return false;
        }

        let mut bound = false;
        let mut missed_surface = false;
        // Auto/Surface → SurfaceView MediaCodec first (mpv-android). Soft = fallback ≤720p.
        let ladder = match settings.android_present {
            AndroidPresentPref::Soft => {
                vec![fluxplay_player::AndroidPresentMode::SoftRgba]
            }
            AndroidPresentPref::Auto | AndroidPresentPref::Surface => {
                caps.present_ladder()
            }
            AndroidPresentPref::GpuEgl => vec![
                fluxplay_player::AndroidPresentMode::GpuEgl,
                fluxplay_player::AndroidPresentMode::SurfaceEmbed,
                fluxplay_player::AndroidPresentMode::SoftRgba,
            ],
        };
        for mode in ladder {
            opts.android_present = mode;
            if !mode.uses_surface() {
                opts.android_surface_wid = None;
                opts.android_surface_wh = None;
                // Soft after a failed Surface attempt — warm under iced and promote later.
                if missed_surface
                    && !matches!(settings.android_present, AndroidPresentPref::Soft)
                {
                    crate::android_bridge::release_video_surface_wid();
                    crate::android_bridge::set_video_surface_z_on_top(false);
                    crate::android_bridge::set_video_surface_visible(true);
                    tracing::info!(
                        present = mode.label(),
                        soft_vf = ?opts.android_soft_vf,
                        "android soft present after Surface miss — warming for upgrade"
                    );
                    return true;
                }
                crate::android_bridge::release_video_surface_wid();
                crate::android_bridge::set_video_surface_visible(false);
                bound = true;
                tracing::info!(
                    present = mode.label(),
                    soft_vf = ?opts.android_soft_vf,
                    tier = ?caps.compat_tier(),
                    "android soft present selected"
                );
                break;
            }
            // Wait for SurfaceHolder async create; require stable size before wid acquire.
            crate::android_bridge::layout_video_surface_chrome_inset_dp(140.0);
            crate::android_bridge::set_video_surface_z_on_top(true);
            crate::android_bridge::set_video_surface_visible(true);
            crate::android_bridge::stabilize_android_session();
            let mut last_wh = (0u32, 0u32);
            let mut stable = 0u32;
            for _ in 0..150 {
                if crate::android_bridge::is_video_surface_ready() {
                    if let Some(wh) = crate::android_bridge::video_surface_size() {
                        if wh == last_wh && wh.0 >= 64 && wh.1 >= 64 {
                            stable = stable.saturating_add(1);
                            if stable >= 4 {
                                break;
                            }
                        } else {
                            last_wh = wh;
                            stable = 0;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
            if let Some((wid, wh)) = crate::android_bridge::prepare_surface_present() {
                opts.android_surface_wid = Some(wid);
                opts.android_surface_wh = Some(wh);
                // Lock AFTER wid acquire — setFixedSize in surfaceChanged invalidated wid.
                crate::android_bridge::lock_video_surface_size();
                let hdr_plus = caps.has_hdr10_plus() || caps.has_dolby_vision();
                let color_mode = settings
                    .hdr_mode
                    .android_color_mode(caps.hdr_capable || caps.mediacodec_hdr, hdr_plus);
                crate::android_bridge::set_display_color_mode(color_mode);
                let panel = if caps.refresh_hz >= 24 {
                    caps.refresh_hz as f32
                } else {
                    60.0
                };
                let hz = fluxplay_player::AndroidDeviceCaps::snap_present_hz_with_modes(
                    0.0,
                    panel,
                    &caps.refresh_modes,
                );
                crate::android_bridge::set_video_frame_rate(hz);
                tracing::info!(
                    present = mode.label(),
                    wid,
                    ?wh,
                    color_mode,
                    hdr_plus,
                    hz,
                    modes = ?caps.refresh_modes,
                    mc_video = caps.mediacodec_video,
                    soft_vf = ?opts.android_soft_vf,
                    "android present prepared"
                );
                bound = true;
                break;
            }
            missed_surface = true;
            tracing::warn!(
                present = mode.label(),
                "android present bind failed — trying next ladder step"
            );
        }
        if !bound {
            opts.android_present = AndroidPresentMode::SoftRgba;
            opts.android_surface_wid = None;
            opts.android_surface_wh = None;
            crate::android_bridge::release_video_surface_wid();
            // Warm Surface under iced (z below) so Soft paints and we can promote soon.
            if missed_surface
                && !matches!(settings.android_present, AndroidPresentPref::Soft)
            {
                crate::android_bridge::set_video_surface_z_on_top(false);
                crate::android_bridge::set_video_surface_visible(true);
                tracing::info!("android soft fallback — warming Surface under iced for upgrade");
                return true;
            }
            crate::android_bridge::set_video_surface_visible(false);
            return false;
        }
        if opts.android_present.uses_soft_rgba() {
            crate::android_bridge::release_video_surface_wid();
            crate::android_bridge::set_video_surface_visible(false);
        }
        false
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (opts, settings, force_soft, soft_wh);
        false
    }
}

async fn load_one(src: MediaSource) -> Result<PlaylistBundle, String> {
    fluxplay_providers::load_source_with_epg(&src)
        .await
        .map_err(|e| e.to_string())
}

/// Warm next episode bytes to disk (cap ~48 MiB) so resume is non-blocking.
/// Next-episode preloads (one at a time, wiped at start-up).
fn prefetch_dir() -> std::path::PathBuf {
    crate::storage::data_dir().join("prefetch")
}

/// Viewer language + per-category language (Live: keyed by group name, VOD/series: by id).
struct LangCtx {
    pref: String,
    cats: std::collections::HashMap<String, crate::names::LangInfo>,
}

impl LangCtx {
    fn keeps_channel(&self, name: &str, group: Option<&str>) -> bool {
        let cat = group.and_then(|g| self.cats.get(g)).copied().unwrap_or_default();
        crate::names::item_matches(crate::names::parse_channel(name).lang, cat, &self.pref)
    }

    fn keeps_title(&self, name: &str, category_id: Option<&str>) -> bool {
        let cat = category_id
            .and_then(|c| self.cats.get(c))
            .copied()
            .unwrap_or_default();
        crate::names::item_matches(crate::names::parse_item_title(name).lang, cat, &self.pref)
    }
}

struct ActorSheet {
    name: String,
    loading: bool,
    card: Option<crate::metadata::ActorCard>,
}

enum CatalogHit {
    Vod(String),
    Series(String),
}

impl FluxPlay {
    fn view_actor_sheet(&self, ui: UiTheme) -> Element<'_, Message> {
        let Some(sheet) = &self.actor_sheet else {
            return Space::new().into();
        };
        let name = sheet.name.clone();
        let loading = sheet.loading;
        let card = sheet.card.clone();
        browser::actor_page(ui, &name, loading, card.as_ref(), |url| {
            self.images.get(url)
        })
    }

    fn mark_in_catalog(&self, card: &mut crate::metadata::ActorCard) {
        if card.credits.is_empty() {
            return;
        }
        let wants: Vec<String> = card.credits.iter().map(|c| title_key(&c.title)).collect();
        let mut found = vec![false; wants.len()];
        for name in self
            .bundle
            .vod
            .iter()
            .map(|v| v.name.as_str())
            .chain(self.bundle.series.iter().map(|s| s.name.as_str()))
        {
            if found.iter().all(|f| *f) {
                break;
            }
            let key = title_key(name);
            for (i, want) in wants.iter().enumerate() {
                if !found[i] && titles_match(want, &key) {
                    found[i] = true;
                }
            }
        }
        for (credit, hit) in card.credits.iter_mut().zip(found) {
            credit.in_catalog = hit;
            if hit && credit.poster.is_none() {
                credit.poster = self.catalog_poster(&credit.title);
            }
        }
    }

    fn catalog_poster(&self, title: &str) -> Option<String> {
        let want = title_key(title);
        if want.len() < 2 {
            return None;
        }
        for v in &self.bundle.vod {
            if titles_match(&want, &title_key(&v.name)) {
                return crate::images::vod_poster_url(v);
            }
        }
        for s in &self.bundle.series {
            if titles_match(&want, &title_key(&s.name)) {
                return crate::images::series_cover_url(s);
            }
        }
        None
    }

    fn catalog_title(&self, title: &str) -> Option<CatalogHit> {
        let want = title_key(title);
        if want.len() < 2 {
            return None;
        }
        for v in &self.bundle.vod {
            if titles_match(&want, &title_key(&v.name)) {
                return Some(CatalogHit::Vod(v.id.clone()));
            }
        }
        for s in &self.bundle.series {
            if titles_match(&want, &title_key(&s.name)) {
                return Some(CatalogHit::Series(s.id.clone()));
            }
        }
        None
    }
}

fn cache_patch_enough(
    patch: &crate::metadata::MetaPatch,
    need_credits: bool,
    missing_age: bool,
) -> bool {
    let credits_ok = !need_credits
        || !crate::metadata::needs_full_credits(patch.actors.as_deref(), patch.plot.as_deref());
    let age_ok = !missing_age || crate::metadata::age_rating(patch.rated.as_deref()).is_some();
    credits_ok && age_ok
}

fn title_key(raw: &str) -> String {
    crate::names::parse_item_title(raw)
        .title
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn titles_match(want: &str, have: &str) -> bool {
    if want.is_empty() || have.is_empty() {
        return false;
    }
    if want == have {
        return true;
    }
    let (short, long) = if want.len() <= have.len() {
        (want, have)
    } else {
        (have, want)
    };
    short.len() >= 8
        && (long.starts_with(&format!("{short} ")) || long.starts_with(&format!("{short}:")))
}

/// Display-ready detail page fields ([`FluxPlay::detail_text`]).
struct DetailText {
    title: String,
    year: Option<String>,
    genre: Option<String>,
    rating: Option<String>,
    runtime: Option<String>,
    rated: Option<String>,
    plot: Option<String>,
    plot_note: Option<String>,
    actors: Option<String>,
    director: Option<String>,
    writer: Option<String>,
    facts: Vec<(&'static str, String)>,
}

/// Copy `src` into `dst` when it carries real text (panels send "", "N/A", "0").
fn take_text(dst: &mut Option<String>, src: &Option<String>) {
    if let Some(s) = src.as_deref().map(str::trim) {
        if !matches!(s.to_ascii_lowercase().as_str(), "" | "n/a" | "null" | "0") {
            *dst = Some(s.to_string());
        }
    }
}

/// A French synopsis already on the page is kept when the portal answer is English.
fn take_plot(dst: &mut Option<String>, src: &Option<String>) {
    let Some(incoming) = src.as_deref().map(str::trim).filter(|s| {
        !matches!(s.to_ascii_lowercase().as_str(), "" | "n/a" | "null" | "0")
    }) else {
        return;
    };
    let have_fr = dst
        .as_deref()
        .is_some_and(|p| crate::translate::guess_lang(p) == Some("fr"));
    if have_fr && crate::translate::guess_lang(incoming) == Some("en") {
        return;
    }
    *dst = Some(incoming.to_string());
}

fn fiche_ready(plot: &Option<String>, actors: &Option<String>) -> bool {
    plot.as_ref().is_some_and(|p| p.trim().len() >= 40)
        && actors.as_ref().is_some_and(|a| !a.trim().is_empty())
}

fn merge_xtream_vod_fields(existing: &mut VodItem, item: &VodItem) {
    take_plot(&mut existing.plot, &item.plot);
    take_text(&mut existing.actors, &item.actors);
    take_text(&mut existing.director, &item.director);
    take_text(&mut existing.writer, &item.writer);
    take_text(&mut existing.genre, &item.genre);
    take_text(&mut existing.year, &item.year);
    take_text(&mut existing.rating, &item.rating);
    take_text(&mut existing.runtime, &item.runtime);
    take_text(&mut existing.imdb_id, &item.imdb_id);
    if existing.poster.is_none() {
        take_text(&mut existing.poster, &item.poster);
    }
    take_text(&mut existing.rated, &item.rated);
    take_text(&mut existing.language, &item.language);
    take_text(&mut existing.country, &item.country);
    take_text(&mut existing.awards, &item.awards);
}

fn merge_xtream_series_fields(existing: &mut SeriesItem, item: &SeriesItem) {
    if !item.seasons.is_empty() {
        existing.seasons = item.seasons.clone();
    }
    take_plot(&mut existing.plot, &item.plot);
    take_text(&mut existing.cover, &item.cover);
    take_text(&mut existing.banner, &item.banner);
    take_text(&mut existing.year, &item.year);
    take_text(&mut existing.genre, &item.genre);
    take_text(&mut existing.rating, &item.rating);
    take_text(&mut existing.imdb_id, &item.imdb_id);
    take_text(&mut existing.actors, &item.actors);
    take_text(&mut existing.director, &item.director);
    take_text(&mut existing.country, &item.country);
    take_text(&mut existing.runtime, &item.runtime);
    take_text(&mut existing.rated, &item.rated);
}

/// Sync-fallback helpers for [`FluxPlay::ingest_source_bundle`] (batch path uses SQLite progressive ingest).
#[allow(dead_code)]
fn replace_source_bundle(into: &mut PlaylistBundle, source_id: Uuid, part: PlaylistBundle) {
    into.channels.retain(|c| c.source_id != Some(source_id));
    into.vod.retain(|v| v.source_id != Some(source_id));
    into.series.retain(|s| s.source_id != Some(source_id));
    // Replace overlapping category ids (per content kind) from this reload.
    into.categories.retain(|c| {
        !part
            .categories
            .iter()
            .any(|p| p.content == c.content && p.id == c.id)
    });
    into.categories.extend(part.categories);
    into.channels.extend(part.channels);
    into.vod.extend(part.vod);
    into.series.extend(part.series);
    into.epg.extend(part.epg);
    let mut ch_seen = std::collections::HashSet::new();
    into.channels.retain(|c| ch_seen.insert(c.id.clone()));
    let mut vod_seen = std::collections::HashSet::new();
    into.vod.retain(|v| vod_seen.insert(v.id.clone()));
    let mut ser_seen = std::collections::HashSet::new();
    into.series.retain(|s| ser_seen.insert(s.id.clone()));
}

/// Keep OMDb/plot/actors from the previous in-memory items when portal fields are empty.
#[allow(dead_code)]
fn merge_part_with_existing(
    existing: &PlaylistBundle,
    source_id: Uuid,
    mut part: PlaylistBundle,
) -> PlaylistBundle {
    use std::collections::HashMap;
    let vod_old: HashMap<&str, &VodItem> = existing
        .vod
        .iter()
        .filter(|v| v.source_id == Some(source_id))
        .filter(|v| {
            v.imdb_id.is_some()
                || v.actors.is_some()
                || v.plot.as_ref().map(|p| p.len() >= 40).unwrap_or(false)
        })
        .map(|v| (v.id.as_str(), v))
        .collect();
    let ser_old: HashMap<&str, &SeriesItem> = existing
        .series
        .iter()
        .filter(|s| s.source_id == Some(source_id))
        .filter(|s| {
            s.imdb_id.is_some()
                || s.actors.is_some()
                || s.plot.as_ref().map(|p| p.len() >= 40).unwrap_or(false)
        })
        .map(|s| (s.id.as_str(), s))
        .collect();
    for v in &mut part.vod {
        v.source_id = Some(source_id);
        if let Some(old) = vod_old.get(v.id.as_str()) {
            if v.plot.as_ref().map(|p| p.len()).unwrap_or(0)
                < old.plot.as_ref().map(|p| p.len()).unwrap_or(0)
            {
                v.plot = old.plot.clone();
            }
            if v.actors.is_none() {
                v.actors = old.actors.clone();
            }
            if v.director.is_none() {
                v.director = old.director.clone();
            }
            if v.imdb_id.is_none() {
                v.imdb_id = old.imdb_id.clone();
            }
            if v.rating.is_none() {
                v.rating = old.rating.clone();
            }
            if v.poster.is_none() {
                v.poster = old.poster.clone();
            }
            if v.genre.is_none() {
                v.genre = old.genre.clone();
            }
            if v.year.is_none() {
                v.year = old.year.clone();
            }
        }
    }
    for s in &mut part.series {
        s.source_id = Some(source_id);
        if let Some(old) = ser_old.get(s.id.as_str()) {
            if s.plot.as_ref().map(|p| p.len()).unwrap_or(0)
                < old.plot.as_ref().map(|p| p.len()).unwrap_or(0)
            {
                s.plot = old.plot.clone();
            }
            if s.actors.is_none() {
                s.actors = old.actors.clone();
            }
            if s.director.is_none() {
                s.director = old.director.clone();
            }
            if s.imdb_id.is_none() {
                s.imdb_id = old.imdb_id.clone();
            }
            if s.rating.is_none() {
                s.rating = old.rating.clone();
            }
            if s.cover.is_none() {
                s.cover = old.cover.clone();
            }
            if s.banner.is_none() {
                s.banner = old.banner.clone();
            }
            if s.genre.is_none() {
                s.genre = old.genre.clone();
            }
            if s.year.is_none() {
                s.year = old.year.clone();
            }
            if s.seasons.is_empty() && !old.seasons.is_empty() {
                s.seasons = old.seasons.clone();
            }
        }
    }
    for ch in &mut part.channels {
        ch.source_id = Some(source_id);
    }
    part
}

fn is_adult_cat(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.contains("XXX") || n.contains("ADULT") || n.contains("FOR ADULTS") || n.contains("+18")
}

/// Endpoint as shown on a source card: scheme, host and port only. The path and query
/// of `get.php?username=&password=` or `/user/pass/` URLs carry the credentials.
fn redact_endpoint(endpoint: &str) -> String {
    let e = endpoint.trim();
    if e.contains("#EXTM3U") || e.contains("#EXTINF") {
        return "playlist inline".into();
    }
    if let Ok(u) = url::Url::parse(e) {
        if let Some(host) = u.host_str() {
            let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
            let more = u.path().len() > 1 || u.query().is_some();
            return format!("{}://{host}{port}{}", u.scheme(), if more { "/…" } else { "" });
        }
    }
    if e.chars().count() > 64 {
        format!("{}…", e.chars().take(64).collect::<String>())
    } else {
        e.to_string()
    }
}


fn take_queue_task(
    mut tasks: Vec<Task<Message>>,
    queue: &mut Option<Task<Message>>,
) -> Task<Message> {
    if let Some(task) = queue.take() {
        tasks.push(task);
    }
    Task::batch(tasks)
}

/// `playing` is `Series name — episode`, the name stored when the episode started.
fn title_opens_series(series_name: &str, playing: &str) -> bool {
    let series_name = series_name.trim();
    if series_name.is_empty() {
        return false;
    }
    if playing == series_name {
        return true;
    }
    let Some(rest) = playing.strip_prefix(series_name) else {
        return false;
    };
    let rest = rest.trim_start();
    rest.starts_with('—') || rest.starts_with('–') || rest.starts_with('-')
}

fn episode_suffix<'a>(series_name: &str, playing: &'a str) -> Option<&'a str> {
    let rest = playing.strip_prefix(series_name.trim())?.trim_start();
    let rest = rest
        .strip_prefix('—')
        .or_else(|| rest.strip_prefix('–'))
        .or_else(|| rest.strip_prefix('-'))?
        .trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

enum ResumeKind {
    Continue,
    Next,
    Rewatch,
}

struct ResumeTarget {
    season: u32,
    episode_num: u32,
    title: String,
    url: String,
    kind: ResumeKind,
}

/// Latest episode with a saved position. A finished one continues on the next.
fn resume_target(settings: &AppSettings, detail: &SeriesItem) -> Option<ResumeTarget> {
    let mut best: Option<(i64, u32, u32, String, String)> = None;
    for season in &detail.seasons {
        for ep in &season.episodes {
            let Some(w) = settings.watch_get(&ep.stream_url) else {
                continue;
            };
            let take = best.as_ref().map(|(at, ..)| w.at >= *at).unwrap_or(true);
            if take {
                best = Some((
                    w.at,
                    season.season_number,
                    ep.episode_num,
                    ep.title.clone(),
                    ep.stream_url.clone(),
                ));
            }
        }
    }
    let (_, season, episode_num, title, url) = best?;
    let w = settings.watch_get(&url)?;
    let finished = w.duration_secs > 30.0
        && (w.position_secs + 20.0 >= w.duration_secs || w.position_secs / w.duration_secs >= 0.92);
    if !finished {
        return Some(ResumeTarget {
            season,
            episode_num,
            title,
            url,
            kind: ResumeKind::Continue,
        });
    }
    if let Some((queue, idx)) =
        queue_from_series(detail, &url, &format!("{} — {title}", detail.name))
    {
        if let Some((_, next_url)) = queue.get(idx + 1) {
            if let Some((sn, ep_num, ep_title, ep_url)) = detail.seasons.iter().find_map(|s| {
                s.episodes.iter().find_map(|ep| {
                    (ep.stream_url == *next_url).then(|| {
                        (
                            s.season_number,
                            ep.episode_num,
                            ep.title.clone(),
                            ep.stream_url.clone(),
                        )
                    })
                })
            }) {
                let mid = settings
                    .watch_ratio(&ep_url)
                    .is_some_and(|ratio| ratio < 0.92);
                return Some(ResumeTarget {
                    season: sn,
                    episode_num: ep_num,
                    title: ep_title,
                    url: ep_url,
                    kind: if mid {
                        ResumeKind::Continue
                    } else {
                        ResumeKind::Next
                    },
                });
            }
        }
    }
    Some(ResumeTarget {
        season,
        episode_num,
        title,
        url,
        kind: ResumeKind::Rewatch,
    })
}

fn resume_button_label(kind: ResumeKind, season: u32, episode: u32) -> String {
    let verb = match kind {
        ResumeKind::Continue => "Reprendre",
        ResumeKind::Next => "Lire",
        ResumeKind::Rewatch => "Revoir",
    };
    #[cfg(target_os = "android")]
    {
        format!("{verb} · S{season:02}E{episode:02}")
    }
    #[cfg(not(target_os = "android"))]
    {
        format!("▶ {verb} · S{season:02}E{episode:02}")
    }
}

fn queue_from_series(
    item: &SeriesItem,
    url: &str,
    playing: &str,
) -> Option<(Vec<(String, String)>, usize)> {
    let mut seasons: Vec<&fluxplay_core::models::SeriesSeason> = item.seasons.iter().collect();
    seasons.sort_by_key(|s| s.season_number);
    let mut queue = Vec::new();
    for season in seasons {
        let mut episodes: Vec<&fluxplay_core::models::SeriesEpisode> =
            season.episodes.iter().collect();
        episodes.sort_by_key(|e| e.episode_num);
        for ep in episodes {
            if ep.stream_url.trim().is_empty() {
                continue;
            }
            queue.push((
                format!("{} — {}", item.name, ep.title),
                ep.stream_url.clone(),
            ));
        }
    }
    if queue.is_empty() {
        return None;
    }
    let idx = if let Some(i) = queue.iter().position(|(_, episode_url)| episode_url == url) {
        i
    } else {
        let shown = crate::names::display_title(&item.name);
        let ep_title = episode_suffix(&item.name, playing)
            .or_else(|| episode_suffix(&shown, playing))?;
        queue.iter().position(|(name, _)| {
            name.rsplit_once(" — ")
                .is_some_and(|(_, title)| title == ep_title)
        })?
    };
    Some((queue, idx))
}

fn profile_message_label(message: &Message) -> &'static str {
    match message {
        Message::PlayerTick => "tick.player",
        Message::CatalogFreshnessCheck => "tick.catalog",
        Message::VideoFrameAllocated { .. } => "tick.video_frame",
        Message::PlayerChromeTick => "tick.chrome",
        Message::ImageLoaded(_) => "async.image",
        Message::MetaEnriched { .. } => "async.meta",
        Message::VodInfoBatch { .. } => "async.vod_info_batch",
        Message::PlayerLayout { .. } => "layout.player",
        Message::PlayerLayoutDirty(_) => "layout.player_dirty",
        Message::WindowResized { .. } | Message::PlayerResizeSettled { .. } => "layout.resize",
        Message::PlayerPointerActivity => "input.pointer",
        Message::DetailMetaLoaded { .. } => "async.detail_meta",
        Message::SeriesDetailLoaded(_) => "async.series_detail",
        Message::SeriesCreditsReady(_) => "async.series_credits",
        Message::SeriesEpisodesEnriched(_) => "async.series_episodes",
        Message::VodDetailLoaded(_) => "async.vod_detail",
        Message::VodCategoryLoaded { .. } => "async.vod_category",
        Message::SeriesCategoryLoaded { .. } => "async.series_category",
        Message::SourceLoaded { .. } => "async.source",
        Message::SourcesBatchLoaded(_) => "async.sources_batch",
        Message::BundleCacheReady { .. } => "async.bundle_cache",
        Message::CatalogIngestDone(_) => "async.catalog_ingest",
        Message::CatalogIngestOneDone { .. } => "async.catalog_ingest_one",
        Message::BrowseIndexReady { .. } => "async.browse_index",
        Message::EpgFetched(..) => "async.epg",
        Message::PrefetchEvent { .. } => "async.prefetch",
        Message::DiagnoseDone(_) => "async.diagnose",
        Message::ClipboardText(_, _) => "async.clipboard",
        #[cfg(not(target_os = "android"))]
        Message::PlaylistFilePicked(_) => "async.file_pick",
        Message::BrowseScrolled(_, _) => "scroll.browse",
        Message::BrowseSettle => "scroll.browse_settle",
        Message::BrowseScrollBy(_) => "scroll.browse_by",
        Message::BrowseDragStart | Message::BrowseDragAt(_) | Message::BrowseDragEnd => {
            "scroll.browse_drag"
        }
        Message::BrowseCoastTick => "scroll.browse_coast",
        Message::MosaicPress(_) => "input.mosaic_press",
        Message::CatScrolled(_, _) => "scroll.cats",
        Message::FlushPendingImages => "async.image_flush",
        Message::SearchApply(_) => "nav.filtre_apply",
        other => ui_action_label(other).unwrap_or("ui.autre"),
    }
}

/// Background work that must not consume the “ignore the next poster click” flag.
fn swallow_noise(message: &Message) -> bool {
    matches!(
        message,
        Message::PlayerTick
            | Message::CatalogFreshnessCheck
            | Message::WakeProbe
            | Message::WakeAudio
            | Message::PlayerStageClick
            | Message::PlayerStageClickFire(_)
            | Message::PlayerChromeTick
            | Message::ImageLoaded(_)
            | Message::FlushPendingImages
            | Message::BrowseCoastTick
            | Message::BrowseScrolled(_, _)
            | Message::BrowseSettle
            | Message::CatScrolled(_, _)
            | Message::VideoFrameAllocated { .. }
            | Message::PlayerLayout { .. }
            | Message::PlayerLayoutDirty(_)
            | Message::MetaEnriched { .. }
            | Message::DetailMetaLoaded { .. }
            | Message::CastPhoto { .. }
            | Message::PrefetchEvent { .. }
            | Message::EpgFetched(..)
            | Message::VodInfoBatch { .. }
            | Message::AgesLoaded(_)
            | Message::CaptionLoaded(_)
            | Message::SeriesEpisodesEnriched(_)
            | Message::SeriesQueueReady { .. }
            | Message::SearchApply(_)
    )
}

fn ui_action_label(message: &Message) -> Option<&'static str> {
    Some(match message {
        Message::PlayerTick
        | Message::CatalogFreshnessCheck
        | Message::VideoFrameAllocated { .. }
        | Message::ImageLoaded(_)
        | Message::FlushPendingImages
        | Message::BrowseScrolled(_, _)
        | Message::BrowseScrollBy(_)
        | Message::BrowseDragStart
        | Message::BrowseDragAt(_)
        | Message::BrowseDragEnd
        | Message::BrowseCoastTick
        | Message::MosaicPress(_)
        | Message::CatScrolled(_, _)
        | Message::SearchChanged(_)
        | Message::SearchApply(_)
        | Message::MetaEnriched { .. } => {
            return None;
        }
        Message::PlayerLayout { .. } | Message::PlayerLayoutDirty(_) | Message::WindowResized { .. } => {
            return None;
        }
        #[cfg(target_os = "android")]
        Message::SafPoll | Message::SafResult { .. } => return None,
        Message::Tab(_) => "nav.onglet",
        Message::CatFilterChanged(_) => "nav.filtre",
        #[cfg(target_os = "android")]
        Message::NavBack => "nav.retour",
        #[cfg(target_os = "android")]
        Message::BrowseFocusDelta(_) => "nav.browse_focus",
        #[cfg(target_os = "android")]
        Message::BrowseActivate => "nav.browse_activate",
        Message::PlayChannel(_) | Message::PlayChannelId(..) => "lecteur.play_chaine",
        Message::PlayVod { .. } => "lecteur.play_vod",
        Message::DownloadMedia { .. } => "detail.download",
        Message::DownloadEvent { .. } => "detail.download_event",
        Message::CancelDownload(_) => "detail.download_cancel",
        Message::DownloadSeason(_) => "detail.download_season",
        Message::CancelSeasonDownloads(_) => "detail.download_season_cancel",
        Message::RevealDownload(_) => "detail.download_reveal",
        Message::FormDownloadDir(_) => "reglages.download_dir_field",
        Message::SaveDownloadDir
        | Message::PickDownloadDir
        | Message::DownloadDirPicked(_)
        | Message::ResetDownloadDir => "reglages.download_dir",
        Message::OpenDownloadsDir => "reglages.download_dir_open",
        Message::Stop => "lecteur.stop",
        Message::TogglePause => "lecteur.pause",
        Message::ToggleMute => "lecteur.mute",
        Message::VolumeChanged(_) => "lecteur.volume",
        Message::VolumeReleased => return None,
        Message::SeekRel(_) => "lecteur.seek_rel",
        Message::SeekPercent(_) | Message::SeekReleased => "lecteur.seek_pct",
        Message::RestartStream => "lecteur.reprise",
        Message::ToggleFullscreen => "lecteur.plein_ecran",
        Message::PlayerPointerActivity | Message::PlayerChromeTick => return None,
        Message::CycleAudio | Message::SelectAudio(_) => "lecteur.piste_audio",
        Message::CycleSubtitles | Message::SelectSubtitle(_) => "lecteur.sous_titres",
        Message::SkipGeneric | Message::SkipIntro | Message::ToggleSkipCredits => {
            "lecteur.generique"
        }
        Message::PlayerFullscreenCommit(..) | Message::PlayerScreenChanged(_) => {
            "lecteur.plein_ecran"
        }
        Message::SetHideAgeUnder(_) | Message::SetHideAgeOver(_) => "reglages.age",
        Message::AgesLoaded(_) | Message::CaptionLoaded(_) => return None,
        Message::PlayerPanel(_) => "lecteur.panneau",
        Message::CycleSpeed | Message::SetSpeed(_) => "lecteur.vitesse",
        Message::ToggleLoop => "lecteur.boucle",
        Message::Screenshot => "lecteur.capture",
        Message::GotoDraftChanged(_) => "lecteur.goto_draft",
        Message::GotoSubmit => "lecteur.goto",
        Message::ToggleSubVisibility => "lecteur.st_visibilite",
        Message::CycleAspect => "lecteur.aspect",
        #[cfg(not(target_os = "android"))]
        Message::ToggleOntop => "lecteur.ontop",
        Message::TogglePip => "lecteur.pip",
        Message::ChapterStep(_) => "lecteur.chapitre",
        Message::PlaylistPrev => "lecteur.piste_prec",
        Message::PlaylistNext => "lecteur.piste_suiv",
        Message::AddBookmark => "lecteur.signet_add",
        Message::JumpBookmark(_) => "lecteur.signet_jump",
        Message::CycleSleepTimer => "lecteur.veille",
        Message::MarkAbA => "lecteur.ab_a",
        Message::MarkAbB => "lecteur.ab_b",
        Message::ClearAbLoop => "lecteur.ab_clear",
        Message::SubDelay(_) => "lecteur.st_delay",
        Message::AudioDelay(_) => "lecteur.av_delay",
        Message::CycleAudioMode => "lecteur.canaux_audio",
        Message::CycleEq => "lecteur.eq",
        Message::ToggleLoudnorm => "lecteur.loudnorm",
        Message::ToggleDeinterlace => "lecteur.desentrelacement",
        Message::CycleUpscale => "lecteur.upscale",
        Message::CycleRotate => "lecteur.rotation",
        Message::NudgeZoom(_) => "lecteur.zoom",
        Message::ToggleNightVf => "lecteur.mode_nuit",
        Message::CycleCache => "reglages.cache",
        Message::CycleDemux => "reglages.demux",
        Message::CycleTheme => "reglages.theme",
        Message::FormOmdbKey(_) => "reglages.omdb_key",
        Message::FormTmdbKey(_) => "reglages.tmdb_key",
        Message::FormDnsServers(_) | Message::FormDohUrl(_) | Message::FormDotServer(_) => {
            "reglages.dns_field"
        }
        Message::FormWgPaste(_) => "reglages.wg_paste",
        Message::SaveOmdbKey => "reglages.omdb_save",
        Message::SaveTmdbKey => "reglages.tmdb_save",
        Message::ClearTmdbKey => "reglages.tmdb_clear",
        Message::ApiPromptSkip => "reglages.api_sans",
        Message::ApiPromptSetup => "reglages.api_avec",
        Message::ApiPromptHold => "reglages.api_prompt",
        Message::ApiGuideToggle => "reglages.api_assistant",
        Message::OpenWeb(_) => "reglages.api_lien",
        Message::SetPrefLang(_) => "reglages.langue",
        Message::ToggleOnlyPrefLang => "reglages.filtre_langue",
        Message::TranslationsReady(..) => "fiche.traduction",
        Message::CycleDnsMode => "reglages.dns_mode",
        Message::SaveNetworkDns => "reglages.dns_save",
        Message::ProbeDns | Message::DnsProbeDone(_) => "reglages.dns_probe",
        Message::ToggleWireGuard => "reglages.wg_toggle",
        Message::WireGuardTunnelDone(_) => "reglages.wg_tunnel",
        Message::PickWireGuardProfile | Message::WireGuardProfilePicked(_) => "reglages.wg_pick",
        Message::ImportWireGuardPaste => "reglages.wg_paste_import",
        Message::ClearWireGuardProfile => "reglages.wg_clear",
        Message::ApplyWireGuardDns => "reglages.wg_dns",
        Message::PasteInto(_) | Message::ClipboardText(_, _) => "presse_papiers.coller",
        Message::SetAccent(_) => "reglages.accent",
        Message::CycleBackend => "reglages.backend",
        Message::ToggleHwdec => "reglages.hwdec",
        Message::ToggleLowLatency => "reglages.low_latency",
        Message::TogglePrefetchNext => "reglages.prefetch",
        Message::CycleFpsGui => "reglages.fps_gui",
        Message::CycleFpsVideo => "reglages.fps_video",
        Message::RefreshDisplayCaps => "reglages.display_caps",
        Message::CycleVideoQuality | Message::SetVideoQuality(_) => "reglages.video_quality",
        Message::CycleGpu => "reglages.gpu",
        Message::CycleHdrMode => "reglages.hdr_mode",
        Message::CycleDisplayPanel => "reglages.panel",
        Message::CycleAndroidPresentPref => "reglages.present_android",
        Message::ToggleTonemapHdr => "reglages.tonemap",
        Message::ToggleRememberPosition => "reglages.remember_pos",
        Message::CycleDefaultAspect => "reglages.default_aspect",
        Message::CycleDefaultDeinterlace => "reglages.default_deint",
        Message::CycleDefaultUpscale => "reglages.default_upscale",
        Message::ToggleDefaultNightMode => "reglages.default_night",
        Message::AddSource | Message::AddPublicDemo { .. } => "sources.ajout",
        Message::RemoveSource(_) => "sources.suppr",
        Message::ReloadSource(_) => "sources.reload",
        Message::OpenExternal => "lecteur.externe",
        Message::SetExternalPlayer(_) => "reglages.lecteur_systeme",
        Message::DiagnosePortals => "diag.portals",
        Message::ToggleFavorite(_) => "fav.toggle",
        Message::ShowShelf(_) => "nav.etagere",
        Message::ClearHistory => "nav.historique_effacer",
        Message::RemoveHistory(_) => "nav.historique_retirer",
        Message::ShelfHold => return None,
        Message::SelectBrowseCategory(_) => "nav.categorie",
        Message::OpenSeries(_) => "series.open",
        Message::SeriesEpisodesEnriched(_) => "series.episodes_meta",
        Message::ToggleSeasonMenu => "series.season_menu",
        Message::SelectSeason(_) => "series.season",
        Message::CastPhoto { .. } => return None,
        Message::CloseSeriesDetail => "series.close",
        Message::OpenVodDetail(_) => "vod.open_detail",
        Message::VodDetailLoaded(_) => "vod.detail_loaded",
        Message::VodInfoBatch { .. } => "vod.info_batch",
        Message::CloseVodDetail => "vod.close_detail",
        Message::DetailMetaLoaded { .. } => "meta.detail",
        Message::LoadMore => "nav.load_more",
        Message::ClosePlayerWindow => "lecteur.fermer",
        Message::PlayerHotkey(_) | Message::PlayerHotkeyIn(..) => "lecteur.hotkey",
        Message::MainWindowOpened(_) => "fenetre.main_open",
        Message::PlayerWindowOpened(_) => "fenetre.player_open",
        Message::WindowClosed(_) => "fenetre.close",
        // High-frequency / async bookkeeping — do not spam ui.interaction.
        _ => return None,
    })
}

fn api_prompt(ui: UiTheme) -> Element<'static, Message> {
    let scrim = mouse_area(
        container(Space::new().width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(|_t: &Theme| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.55))),
                ..Default::default()
            }),
    )
    .on_press(Message::ApiPromptHold);
    let card = container(
        column![
            text("Aucune clé API")
                .size(22)
                .color(ui.on_surface()),
            text("Les fiches détaillées (synopsis, acteurs, âge) passent par ton compte TMDB. Sans clé, FluxPlay garde le texte du portail.")
                .size(14)
                .color(ui.ink_muted()),
            row![
                pill_button(text("Continuer sans"), Message::ApiPromptSkip, ui, false),
                pill_button_primary(text("Continuer avec"), Message::ApiPromptSetup, ui),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(12)
        .width(Fill),
    )
    .width(Length::Fixed(460.0))
    .padding(Padding::from([18, 20]))
    .style(move |_t: &Theme| container::Style {
        background: Some(Background::Color(ui.surface_container())),
        border: Border {
            color: ui.outline_variant(),
            width: 1.0,
            radius: RADIUS_LARGE.into(),
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
    .into()
}

fn guide_step<'a>(ui: UiTheme, n: &str, title: &'a str, body: &'a str) -> Element<'a, Message> {
    column![
        text(format!("{n}. {title}"))
            .size(14)
            .color(ui.on_surface()),
        text(body).size(13).color(ui.ink_muted()),
    ]
    .spacing(4)
    .width(Fill)
    .into()
}

fn api_guide(ui: UiTheme, open: bool) -> Element<'static, Message> {
    if !open {
        return Space::new().height(0).into();
    }
    column![
        text("Compte gratuit, environ 10 minutes. La clé reste la tienne : FluxPlay ne la partage pas.")
            .size(13)
            .color(ui.on_surface()),
        guide_step(
            ui,
            "1",
            "Créer le compte",
            "Ouvre l'inscription. Un email, un mot de passe. Ouvre ensuite le mail et confirme le compte. Sans cette confirmation, la page API reste vide.",
        ),
        pill_button(
            text("Ouvrir l'inscription"),
            Message::OpenWeb("https://www.themoviedb.org/signup".into()),
            ui,
            false,
        ),
        guide_step(
            ui,
            "2",
            "Demander la clé",
            "Une fois connecté, ouvre la page API sur un ordinateur : elle n'est pas faite pour un téléphone. Clique sur demander une clé. Type : Personnel. Nom : FluxPlay. Adresse : https://www.themoviedb.org (une adresse est obligatoire, une page perso convient aussi). Résumé : lecteur personnel, fiches en français, la base n'est pas redistribuée. Accepte les conditions et envoie.",
        ),
        pill_button(
            text("Ouvrir la page API"),
            Message::OpenWeb("https://www.themoviedb.org/settings/api".into()),
            ui,
            false,
        ),
        guide_step(
            ui,
            "3",
            "Copier le jeton de lecture",
            "La page affiche deux secrets. Copie « API Read Access Token », le jeton d'accès en lecture. C'est la longue chaîne qui commence par eyJ. La clé courte de 32 caractères marche aussi. Ne l'envoie à personne.",
        ),
        guide_step(
            ui,
            "4",
            "Le coller ici",
            "Colle le jeton dans le champ juste en dessous, puis Enregistrer le jeton. Le statut passe à TMDB actif. Rouvre un film : synopsis, acteurs et âge, quand TMDB a une classification.",
        ),
    ]
    .spacing(8)
    .width(Fill)
    .into()
}

fn settings_fold<'a>(
    ui: UiTheme,
    section: SettingsSection,
    open: bool,
    hint: &'a str,
    body: Element<'a, Message>,
) -> Element<'a, Message> {
    let mark = crate::icons::icon(
        if open {
            crate::icons::Icon::ExpandMore
        } else {
            crate::icons::Icon::ChevronRight
        },
        22.0,
        ui.primary(),
    );
    let header = mouse_area(
        container(
            row![
                container(mark)
                    .width(Length::Fixed(22.0))
                    .center_x(Length::Fixed(22.0)),
                column![
                    text(section.title()).size(16).color(ui.ink()),
                    text(hint).size(12).color(ui.ink_muted()),
                ]
                .spacing(2)
                .width(Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .width(Fill),
        )
        .width(Fill)
        .padding(Padding::from([12, 14]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_low())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: RADIUS_LARGE.into(),
            },
            ..Default::default()
        }),
    )
    .on_press(Message::ToggleSettings(section));
    let mut col = column![header].spacing(8).width(Fill);
    if open {
        col = col.push(body);
    }
    col.into()
}

/// Grouped settings block — keeps options in a card instead of a full-width stack.
fn settings_card<'a>(ui: UiTheme, body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(body.into())
        .width(Fill)
        .padding(Padding::from([12, 14]))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_low())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: RADIUS_LARGE.into(),
            },
            ..Default::default()
        })
        .into()
}

/// Secondary selection grammar (settings toggles / cycle pills).
/// `active` → secondary_container + LARGE_INCREASED; idle → surface_container_low + outline.
fn pill_button<'a>(
    label: impl Into<Element<'a, Message>>,
    on_press: Message,
    ui: UiTheme,
    active: bool,
) -> Element<'a, Message> {
    pill_button_ex(label, on_press, ui, active, false)
}

/// Rare primary CTA (Enregistrer / Ajouter) — accent fill, not selection secondary.
fn pill_button_primary<'a>(
    label: impl Into<Element<'a, Message>>,
    on_press: Message,
    ui: UiTheme,
) -> Element<'a, Message> {
    pill_button_ex(label, on_press, ui, true, true)
}

fn pill_button_ex<'a>(
    label: impl Into<Element<'a, Message>>,
    on_press: Message,
    ui: UiTheme,
    active: bool,
    active_primary: bool,
) -> Element<'a, Message> {
    // mouse_area + container — styled `button` drops text glyphs on Android GLES.
    let (fg, bg, radius, border_w, border_c) = if active || active_primary {
        (
            ui.on_surface(),
            ui.surface_container(),
            RADIUS_LARGE_INCREASED,
            2.0,
            ui.primary(),
        )
    } else {
        (
            ui.on_surface(),
            ui.surface_container_low(),
            RADIUS_LARGE,
            1.0,
            ui.outline_variant(),
        )
    };
    mouse_area(
        container(label.into())
            .padding(Padding::from([10, 18]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(bg)),
                text_color: Some(fg),
                border: Border {
                    radius: radius.into(),
                    color: border_c,
                    width: border_w,
                },
                ..Default::default()
            }),
    )
    .on_press(on_press)
    .into()
}

fn chip(label: String, msg: Message, ui: UiTheme, active: bool) -> Element<'static, Message> {
    // Same GLES-safe pattern as pill_button — secondary selection (not primary accent).
    let (fg, bg, radius, border_w, border_c) = if active {
        (
            ui.on_surface(),
            ui.surface_container(),
            RADIUS_LARGE_INCREASED,
            2.0,
            ui.primary(),
        )
    } else {
        (
            ui.on_surface(),
            ui.surface_container_low(),
            RADIUS_LARGE,
            1.0,
            ui.outline_variant(),
        )
    };
    mouse_area(
        container(text(label).size(13).color(fg))
            .padding(Padding::from([8, 14]))
            .style(move |_t: &Theme| container::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    radius: radius.into(),
                    color: border_c,
                    width: border_w,
                },
                ..Default::default()
            }),
    )
    .on_press(msg)
    .into()
}


fn party_http(
    link: crate::party::ParsedLink,
    path: impl Fn(&crate::party::ParsedLink) -> String,
) -> Task<Message> {
    let path = path(&link);
    Task::perform(
        async move {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(crate::party::http_get(&link, &path));
            });
            loop {
                if let Ok(r) = rx.try_recv() {
                    return r;
                }
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
        },
        Message::PartyHttp,
    )
}

fn remote_query(link: &crate::party::ParsedLink, op: crate::party::RemoteOp) -> String {
    use crate::party::RemoteOp::*;
    let k = &link.token;
    match op {
        Toggle => format!("/v1/cmd?k={k}&op=toggle"),
        Mute => format!("/v1/cmd?k={k}&op=mute"),
        Next => format!("/v1/cmd?k={k}&op=next"),
        Prev => format!("/v1/cmd?k={k}&op=prev"),
        Seek(secs) => format!("/v1/cmd?k={k}&op=seek&secs={secs}"),
        Volume(delta) => format!("/v1/cmd?k={k}&op=volume&delta={delta}"),
    }
}

fn with_height(url: &str, height: u16) -> String {
    match url.split_once("&h=") {
        Some((head, rest)) => {
            let tail = rest.split_once('&').map(|(_, t)| t).unwrap_or("");
            if tail.is_empty() {
                format!("{head}&h={height}")
            } else {
                format!("{head}&h={height}&{tail}")
            }
        }
        None => format!("{url}&h={height}"),
    }
}

fn field<'a>(
    label: &'a str,
    value: &str,
    on_input: impl Fn(String) -> Message + 'a,
    paste: Option<PasteTarget>,
    ui: UiTheme,
) -> Element<'a, Message> {
    let input = text_input(label, value)
        .on_input(on_input)
        .padding(12)
        .size(14)
        .style(move |theme: &Theme, status| {
            let mut s = text_input::default(theme, status);
            s.border.radius = RADIUS_MD.into();
            s.background = Background::Color(ui.surface_muted());
            s
        });
    let input_row: Element<'a, Message> = if let Some(target) = paste {
        row![
            container(input).width(Fill),
            pill_button(
                crate::icons::icon_label(crate::icons::Icon::Paste, "Coller", 13.0, ui.on_surface()),
                Message::PasteInto(target),
                ui,
                false,
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Fill)
        .into()
    } else {
        input.into()
    };
    column![
        text(label).size(12).color(ui.ink_muted()),
        input_row,
    ]
    .spacing(4)
    .width(Fill)
    .into()
}

static PLAYER_PTR_WIN: std::sync::Mutex<Option<window::Id>> = std::sync::Mutex::new(None);

fn map_player_pointer(event: Event, _status: event::Status, id: window::Id) -> Option<Message> {
    let want = PLAYER_PTR_WIN.lock().ok().and_then(|slot| *slot);
    if want != Some(id) {
        return None;
    }
    let moved = matches!(
        event,
        Event::Mouse(iced::mouse::Event::CursorMoved { .. } | iced::mouse::Event::CursorEntered)
    );
    if !moved {
        return None;
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST_MS: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let prev = LAST_MS.load(Ordering::Relaxed);
    if now.saturating_sub(prev) < 120 {
        return None;
    }
    LAST_MS.store(now, Ordering::Relaxed);
    Some(Message::PlayerPointerActivity)
}

fn map_player_hotkeys(
    event: Event,
    status: event::Status,
    id: window::Id,
) -> Option<Message> {
    if status == event::Status::Captured {
        return None;
    }
    let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    // Ignore typing into text fields when modifiers include nothing special — Captured handles most.
    let hk = match key {
        Key::Named(Named::Space)
        | Key::Named(Named::MediaPlayPause)
        | Key::Named(Named::Play)
        | Key::Named(Named::Pause)
        | Key::Named(Named::Enter) => PlayerHotkey::TogglePause,
        Key::Named(Named::ArrowLeft) if modifiers.shift() => PlayerHotkey::SeekBackBig,
        Key::Named(Named::ArrowRight) if modifiers.shift() => PlayerHotkey::SeekFwdBig,
        Key::Named(Named::ArrowLeft) => PlayerHotkey::SeekBack,
        Key::Named(Named::ArrowRight) => PlayerHotkey::SeekFwd,
        Key::Named(Named::ArrowUp) => PlayerHotkey::VolumeUp,
        Key::Named(Named::ArrowDown) => PlayerHotkey::VolumeDown,
        Key::Named(Named::Escape)
        | Key::Named(Named::GoBack)
        | Key::Named(Named::BrowserBack) => PlayerHotkey::Escape,
        Key::Named(Named::MediaStop) => PlayerHotkey::Stop,
        Key::Character(c) => match c.as_str() {
            "m" | "M" => PlayerHotkey::Mute,
            "f" | "F" => PlayerHotkey::Fullscreen,
            "r" | "R" => PlayerHotkey::Restart,
            "x" | "X" => PlayerHotkey::Stop,
            "[" => PlayerHotkey::Speed,
            "l" | "L" => PlayerHotkey::Loop,
            "s" | "S" if modifiers.control() => PlayerHotkey::Screenshot,
            "." => PlayerHotkey::FrameStep,
            _ => return None,
        },
        _ => return None,
    };
    let _ = Modifiers::empty();
    Some(Message::PlayerHotkeyIn(id, hk))
}

#[cfg(target_os = "android")]
fn map_android_back(
    event: Event,
    status: event::Status,
    _id: window::Id,
) -> Option<Message> {
    if status == event::Status::Captured {
        return None;
    }
    let Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) = event else {
        return None;
    };
    match key {
        Key::Named(Named::GoBack) | Key::Named(Named::BrowserBack) | Key::Named(Named::Escape) => {
            Some(Message::NavBack)
        }
        Key::Named(Named::ArrowUp) => Some(Message::BrowseFocusDelta(-1)),
        Key::Named(Named::ArrowDown) => Some(Message::BrowseFocusDelta(1)),
        Key::Named(Named::ArrowLeft) => Some(Message::BrowseFocusDelta(-1)),
        Key::Named(Named::ArrowRight) => Some(Message::BrowseFocusDelta(1)),
        Key::Named(Named::Enter) => Some(Message::BrowseActivate),
        _ => None,
    }
}

/// Logical client size that fills `aspect` (width / height) without growing
/// the window. `skip` means the current size is already close enough.
#[cfg_attr(target_os = "android", allow(dead_code))]
fn player_client_for_aspect(width: f32, height: f32, aspect: f32) -> (f32, f32, bool) {
    let aspect = if aspect.is_finite() && aspect > 0.2 {
        aspect
    } else {
        16.0 / 9.0
    };
    let cur = width.max(1.0) / height.max(1.0);
    if ((cur - aspect) / aspect).abs() < 0.025 {
        return (width, height, true);
    }
    let mut nw = width.clamp(640.0, 1600.0);
    let mut nh = (nw / aspect).round();
    if nh < 360.0 {
        nh = 360.0;
        nw = (nh * aspect).round();
    } else if nh > 900.0 {
        nh = 900.0;
        nw = (nh * aspect).round();
    }
    (nw, nh, false)
}

/// Parse `mm:ss`, `hh:mm:ss`, or raw seconds.
fn parse_timecode(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(secs) = raw.parse::<f64>() {
        return Some(secs.max(0.0));
    }
    let parts: Vec<&str> = raw.split(':').collect();
    match parts.as_slice() {
        [m, s] => {
            let m: f64 = m.parse().ok()?;
            let s: f64 = s.parse().ok()?;
            Some(m * 60.0 + s)
        }
        [h, m, s] => {
            let h: f64 = h.parse().ok()?;
            let m: f64 = m.parse().ok()?;
            let s: f64 = s.parse().ok()?;
            Some(h * 3600.0 + m * 60.0 + s)
        }
        _ => None,
    }
}

/// Android: attach to the Activity window once it exists and sync size.
#[cfg(target_os = "android")]
fn android_bind_window() -> Task<Message> {
    window::latest().then(|id| {
        let Some(id) = id else {
            // Surface not ready yet — `open_events` / Resized will finish the bind.
            return Task::none();
        };
        Task::batch([
            Task::done(Message::MainWindowOpened(id)),
            android_sync_size(id),
        ])
    })
}

/// Publish the real window size so layout_metrics matches the Activity surface.
#[cfg(target_os = "android")]
fn android_sync_size(id: window::Id) -> Task<Message> {
    window::size(id).map(move |size| Message::WindowResized { id, size })
}


/// Display name for a source added without one: the server host (`www.` dropped),
/// the playlist file name, or the account name — else "Source N".
fn default_source_name(endpoint: &str, user: &str, n: usize) -> String {
    let endpoint = endpoint.trim();
    if let Ok(u) = url::Url::parse(endpoint) {
        if let Some(host) = u.host_str().filter(|h| !h.is_empty()) {
            return host.trim_start_matches("www.").to_string();
        }
    }
    let path = std::path::Path::new(endpoint);
    if !endpoint.contains('\n') && path.extension().is_some() {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            return stem.to_string();
        }
    }
    let user = user.trim();
    if !user.is_empty() {
        return user.to_string();
    }
    format!("Source {n}")
}

#[cfg(test)]
mod tests {
    use super::default_source_name;

    #[test]
    fn source_name_defaults_from_endpoint() {
        assert_eq!(
            default_source_name("http://www.example.tv:8080/get.php?username=a&password=b", "", 1),
            "example.tv"
        );
        assert_eq!(default_source_name("/home/me/france.m3u", "", 2), "france");
        #[cfg(windows)]
        assert_eq!(default_source_name(r"C:\Playlists\france.m3u", "", 2), "france");
        // Pasted playlist body: no host, no file name.
        assert_eq!(default_source_name("#EXTM3U\n#EXTINF:-1,x\nhttp://a/b", "", 3), "Source 3");
        assert_eq!(default_source_name("not a url", "bob", 4), "bob");
        assert_eq!(default_source_name("not a url", " ", 5), "Source 5");
    }

    #[test]
    fn next_episode_crosses_seasons_in_order() {
        let item: fluxplay_core::models::SeriesItem = serde_json::from_str(
            r#"{
                "id": "1",
                "name": "Show",
                "source_id": null,
                "seasons": [
                    {"season_number": 2, "episodes": [
                        {"id": "a", "title": "Deux", "stream_url": "http://s2e2", "episode_num": 2},
                        {"id": "b", "title": "Un", "stream_url": "http://s2e1", "episode_num": 1}
                    ]},
                    {"season_number": 1, "episodes": [
                        {"id": "c", "title": "Fin", "stream_url": "http://s1e2", "episode_num": 2},
                        {"id": "d", "title": "Debut", "stream_url": "http://s1e1", "episode_num": 1}
                    ]}
                ]
            }"#,
        )
        .unwrap();
        let (queue, idx) = super::queue_from_series(&item, "http://s1e2", "Show — Fin").unwrap();
        assert_eq!(idx, 1);
        assert_eq!(
            queue.iter().map(|(_, url)| url.as_str()).collect::<Vec<_>>(),
            vec!["http://s1e1", "http://s1e2", "http://s2e1", "http://s2e2"]
        );
    }

    fn show() -> fluxplay_core::models::SeriesItem {
        serde_json::from_str(
            r#"{
                "id": "1",
                "name": "Show",
                "source_id": null,
                "seasons": [
                    {"season_number": 1, "episodes": [
                        {"id": "d", "title": "Debut", "stream_url": "http://s1e1", "episode_num": 1},
                        {"id": "c", "title": "Fin", "stream_url": "http://s1e2", "episode_num": 2}
                    ]},
                    {"season_number": 2, "episodes": [
                        {"id": "b", "title": "Un", "stream_url": "http://s2e1", "episode_num": 1},
                        {"id": "a", "title": "Deux", "stream_url": "http://s2e2", "episode_num": 2}
                    ]}
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn series_history_is_one_row_and_resume_follows_progress() {
        use fluxplay_core::models::{series_history_key, AppSettings, ContentKind, RecentChannel};
        use chrono::Utc;
        let mut settings = AppSettings::default();
        settings.push_series_recent("1", "Show", "http://s1e1", None, Some("S01E01".into()));
        settings.push_series_recent("1", "Show", "http://s1e2", None, Some("S01E02".into()));
        assert_eq!(settings.recent.len(), 1);
        assert_eq!(settings.recent[0].stream_url, "http://s1e2");
        assert_eq!(settings.recent[0].note.as_deref(), Some("S01E02"));
        settings.recent.insert(
            0,
            RecentChannel {
                channel_id: "vod-old".into(),
                name: "Show — Debut".into(),
                stream_url: "http://s1e1".into(),
                played_at: Utc::now(),
                group: None,
                kind: ContentKind::Series,
                logo: None,
                note: None,
            },
        );
        settings.fold_series_history();
        assert_eq!(settings.recent.len(), 1);
        assert_eq!(series_history_key("Show — Debut"), series_history_key("Show"));

        let item = show();
        settings.watch.push(fluxplay_core::models::WatchPoint {
            url: "http://s1e2".into(),
            position_secs: 400.0,
            duration_secs: 1000.0,
            at: 1,
        });
        let mid = super::resume_target(&settings, &item).unwrap();
        assert_eq!(mid.url, "http://s1e2");
        assert!(matches!(mid.kind, super::ResumeKind::Continue));

        settings.watch[0].position_secs = 990.0;
        settings.watch[0].at = 2;
        let next = super::resume_target(&settings, &item).unwrap();
        assert_eq!(next.url, "http://s2e1");
        assert!(matches!(next.kind, super::ResumeKind::Next));

        settings.watch.push(fluxplay_core::models::WatchPoint {
            url: "http://s2e2".into(),
            position_secs: 990.0,
            duration_secs: 1000.0,
            at: 3,
        });
        let end = super::resume_target(&settings, &item).unwrap();
        assert_eq!(end.url, "http://s2e2");
        assert!(matches!(end.kind, super::ResumeKind::Rewatch));
    }

    #[test]
    fn player_window_follows_picture_aspect() {
        let wide = 16.0 / 9.0;
        let (w, h, skip) = super::player_client_for_aspect(1280.0, 720.0, wide);
        assert!(skip);
        assert_eq!((w, h), (1280.0, 720.0));
        let (_, h, skip) = super::player_client_for_aspect(1120.0, 720.0, wide);
        assert!(!skip);
        assert!((h - 630.0).abs() < 1.0);
        let (w, h, skip) = super::player_client_for_aspect(1280.0, 720.0, 2.39);
        assert!(!skip);
        assert!((w - 1280.0).abs() < 1.0);
        assert!((h - 536.0).abs() < 2.0);
    }
}
