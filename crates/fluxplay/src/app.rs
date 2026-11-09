use chrono::Local;
#[cfg(target_os = "android")]
use fluxplay_core::models::AndroidPresentPref;
use fluxplay_core::models::{
    AccentPreset, AppSettings, AspectPref, Channel, ContentKind,
    DeinterlacePref, FpsCapPref, MediaSource, NetworkSettings, PlaylistBundle, PlayerBackendPref,
    SeriesItem, SourceKind, ThemeMode, UpscalePref, VodItem,
};
use std::sync::Arc;
use fluxplay_player::{
    detect_backends, target_profile, BackendId, PlayOptions, PlaybackState, StreamSession,
    VideoRect,
};
use iced::widget::{
    column, container, mouse_area, row, text, text_input, Column, Row, Space,
};
use iced::widget::scrollable::AbsoluteOffset;
use iced::widget::image::{
    self as iced_image, Allocation as ImageAllocation, Handle as ImageHandle,
};
use iced::window;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Point, Size, Subscription,
    Task, Theme,
};
use uuid::Uuid;

use crate::browser::{BrowseIndex, CAT_PAGE};
use crate::player_ui::PlayerPanel;
use crate::theme::{
    type_style, LayoutMetrics, TypeRole, UiTheme, MOTION_COAST_FRICTION, MOTION_SHORT_MS,
    RADIUS_LARGE, RADIUS_LARGE_INCREASED, RADIUS_MD, SPACE_SM,
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
            Self::Sources => Icon::Sources,
            Self::Settings => Icon::Settings,
        }
    }

    fn all() -> &'static [Tab] {
        &[
            Tab::Live,
            Tab::Vod,
            Tab::Series,
            Tab::Favorites,
            Tab::Epg,
            Tab::Sources,
            Tab::Settings,
        ]
    }
}

struct FluxPlay {
    settings: AppSettings,
    sources: Vec<MediaSource>,
    bundle: PlaylistBundle,
    tab: Tab,
    search: String,
    selected_group: Option<String>,
    selected_channel: Option<String>,
    selected_vod_category: Option<String>,
    selected_series_category: Option<String>,
    series_detail: Option<SeriesItem>,
    vod_detail: Option<VodItem>,
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
    /// Settings draft for `settings.download_dir` (empty = default folder).
    form_download_dir: String,
    form_dns_servers: String,
    form_doh_url: String,
    form_dot_server: String,
    form_wg_paste: String,
    network_probe: String,
    system_dark: bool,
    cat_filter: String,
    images: crate::images::ImageCache,
    catalog_db: Option<crate::catalog_db::CatalogDb>,
    autoplay_done: bool,
    main_id: Option<window::Id>,
    player_id: Option<window::Id>,
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
    /// Embedded video frame (libffmpeg/libmpv soft RGBA → iced image).
    video_frame: Option<ImageHandle>,
    /// Pins GPU atlas memory so the displayed frame never async-flickers.
    video_allocation: Option<ImageAllocation>,
    /// Previous atlas entries kept behind so iced never paints a freed/reset texture.
    /// Depth matches the 5 worker video atlases (on-screen + present queue + upload).
    video_allocation_hold: Option<ImageAllocation>,
    video_allocation_hold2: Option<ImageAllocation>,
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
    player_pointer_at: Option<std::time::Instant>,
    /// Ignore video_rect / soft-size churn while OS fullscreen is settling.
    player_layout_freeze_until: Option<std::time::Instant>,
    /// Series episode URLs for next-episode prefetch (current index in list).
    series_queue: Vec<(String, String)>,
    series_queue_idx: usize,
    seek_drag: Option<f64>,
    prefetch_armed_for: Option<String>,
    /// Next-episode preload in flight: catalog URL + abort handle.
    prefetch_job: Option<(String, iced::task::Handle)>,
    /// Preloaded episode ready on disk: catalog URL → file.
    prefetched: Option<(String, std::path::PathBuf)>,
    /// Lazy Xtream `get_vod_info` enrich — never burst at boot (ban risk).
    xtream_vod_enrich_started: bool,
    /// Cached hardware probe (monitor + GPU).
    display_probe: DisplayProbe,
    display_probe_at: std::time::Instant,
    /// Resolved GUI / video FPS ceilings from prefs + probe + stage.
    display_caps: DisplayCaps,
    /// Browse positions for the active tab/filter (`Identity` = SQLite catalog order).
    browse_index: BrowseIndex,
    /// Slim mosaic rows (LRU); full payloads stay in SQLite.
    tile_cache: crate::browse_tiles::BrowseTileCache,
    /// Catalog sizes after spine load (bundle vod/series/channels vecs stay empty).
    channel_count: usize,
    vod_count: usize,
    series_count: usize,
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
    /// Coalesce ImageLoaded → one iced view refresh per burst (fast scroll).
    /// Hard-capped — unbounded growth during fling was a multi-hundred-MB leak.
    pending_images: Vec<(String, Option<Uuid>, Vec<u8>)>,
    image_flush_armed: bool,
    /// Bumped on browse rebuild / category switch — stale ImageLoaded are dropped.
    art_fetch_gen: u64,
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
    /// Background warm of `browse_index` art — kept off; visible-only loads posters.
    art_warm_cursor: usize,
    art_warm_active: bool,
    /// How many mosaic items are unlocked (page growth via « Afficher plus »).
    mosaic_shown: usize,
    /// Debounce search → index rebuild (generation discards stale timers).
    search_debounce_gen: u64,
    /// Atomic job meters (ingest / index / images) + browse generation.
    jobs: crate::async_jobs::JobMeters,
    /// True when we paused playback because Android Suspended (resume on foreground).
    #[cfg(target_os = "android")]
    lifecycle_paused: bool,
    /// Defer portal sync until WireGuard SOCKS is applied (boot gate).
    pending_portal_sync_after_wg: bool,
    /// D-pad / TV browse focus index into `browse_index`.
    browse_focus: usize,
    /// Last finger drag Δy for mosaic fling coast seed.
    browse_drag_last_dy: f32,
    /// Multi-tick fling coast velocity (px/tick); decays by MOTION_COAST_FRICTION.
    browse_coast_vy: f32,
    /// Mosaic tile currently pressed (press feedback).
    pressed_mosaic: Option<String>,
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
}

/// Poster/art fetch result: (key, source uuid, bytes) or (key, error).
type ImageLoadResult = Result<(String, Option<uuid::Uuid>, Vec<u8>), (String, String)>;

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
    PlayerChromeTick,
    CycleAudio,
    CycleSubtitles,
    PlayerTick,
    /// Deferred CatalogDb::open finished (DB parked in PENDING_CATALOG_BOOT).
    #[cfg(target_os = "android")]
    CatalogBootReady {
        counts: (usize, usize, usize),
        sync_fresh: bool,
    },
    /// Android system Back while browsing (close detail → ignore Activity finish).
    #[cfg(target_os = "android")]
    NavBack,
    /// Remote / keyboard mosaic focus step (`dcol` / `drow` in grid cells).
    BrowseFocusStep { dcol: i32, drow: i32 },
    /// Activate the focused mosaic / live row (Enter / DPAD_CENTER).
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
    CycleAccent,
    SetAccent(AccentPreset),
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
    FormDnsServers(String),
    FormDohUrl(String),
    FormDotServer(String),
    FormWgPaste(String),
    SaveOmdbKey,
    /// Viewer language (ISO 639-1); empty = none.
    SetPrefLang(String),
    ToggleTranslateMeta,
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
    /// Spine reload finished off the UI thread (categories + EPG + counts only).
    BundleCacheReady(Result<crate::catalog_db::CatalogSpine, String>),
    OpenExternal,
    SetExternalPlayer(String),
    PickPlaylistFile,
    /// Desktop rfd file picker result; Android uses SAF (`SafResult`) instead.
    #[cfg(not(target_os = "android"))]
    PlaylistFilePicked(Option<String>),
    ToggleFavorite(String),
    CycleBackend,
    ToggleHwdec,
    ToggleLowLatency,
    TogglePrefetchNext,
    CycleFpsGui,
    CycleFpsVideo,
    RefreshDisplayCaps,
    CycleVideoQuality,
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
    /// Category rows written off-UI; rebuild spine only if `category_id` still selected.
    CategoryPersistDone {
        category_id: String,
        is_series: bool,
    },
    OpenSeries(String),
    SeriesDetailLoaded(Result<SeriesItem, String>),
    SeriesEpisodesEnriched(SeriesItem),
    CloseSeriesDetail,
    OpenVodDetail(String),
    VodDetailLoaded(Result<VodItem, String>),
    /// Parallel Xtream `get_vod_info` batch after catalog load/reload.
    VodInfoBatch(Vec<VodItem>),
    CloseVodDetail,
    DetailMetaLoaded {
        is_series: bool,
        id: String,
        patch: Option<crate::metadata::MetaPatch>,
    },
    CatFilterChanged(String),
    SelectBrowseCategory(String),
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
    /// Unlock the next mosaic page (posters + tiles) — RAM-safe pagination.
    MosaicShowMore,
    /// Category sidebar scroll (virtualized).
    CatScrolled(f32, f32),
    ImageLoaded(u64, ImageLoadResult),
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
        /// Pre-filled tiles for SQL search (browse pos = vec index).
        search_tiles: Vec<crate::browse_tiles::BrowseTile>,
    },
    MetaEnriched {
        series: Vec<(String, Option<uuid::Uuid>, crate::metadata::MetaPatch)>,
        vod: Vec<(String, Option<uuid::Uuid>, crate::metadata::MetaPatch)>,
    },
    OpenImdb(String),
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

impl FluxPlay {
    fn new() -> (Self, Task<Message>) {
        let persisted = storage::load();
        let mut sources = persisted.sources;
        sanitize_sources(&mut sources);
        demo::strip_demo_if_real(&mut sources);
        if sources.is_empty() {
            sources.push(demo::demo_source());
        }
        let settings = persisted.settings;
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
        let session = StreamSession::with_options(opts);
        let mut app = Self {
            settings,
            sources,
            bundle,
            tab: Tab::Live,
            search: String::new(),
            selected_group: None,
            selected_channel: None,
            selected_vod_category: Some("*".into()),
            selected_series_category: Some("*".into()),
            series_detail: None,
            vod_detail: None,
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
            form_download_dir: String::new(),
            form_dns_servers: String::new(),
            form_doh_url: String::new(),
            form_dot_server: String::new(),
            form_wg_paste: String::new(),
            network_probe: String::new(),
            system_dark,
            cat_filter: String::new(),
            images: crate::images::ImageCache::default(),
            catalog_db,
            autoplay_done: false,
            main_id,
            player_id: None,
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
            video_frame: None,
            video_allocation: None,
            video_allocation_hold: None,
            video_allocation_hold2: None,
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
            goto_draft: String::new(),
            sleep_until: None,
            sleep_mins: None,
            pip_mode: false,
            player_fullscreen: false,
            player_chrome_visible: true,
            chrome_alpha: 1.0,
            player_pointer_at: None,
            player_layout_freeze_until: None,
            series_queue: Vec::new(),
            series_queue_idx: 0,
            seek_drag: None,
            prefetch_armed_for: None,
            prefetch_job: None,
            prefetched: None,
            xtream_vod_enrich_started: false,
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
            tile_cache: crate::browse_tiles::BrowseTileCache::default(),
            channel_count: 0,
            vod_count: 0,
            series_count: 0,
            episode_flat: Vec::new(),
            browse_scroll_y: 0.0,
            browse_view_h: 720.0,
            browse_slice: (0, 0),
            cat_scroll_y: 0.0,
            cat_view_h: 720.0,
            cat_slice: (0, 0),
            cat_entries: Vec::new(),
            pending_images: Vec::new(),
            art_fetch_gen: 0,
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
            mosaic_shown: browser::MOSAIC_PAGE_ITEMS,
            search_debounce_gen: 0,
            jobs: crate::async_jobs::JobMeters::new(),
            #[cfg(target_os = "android")]
            lifecycle_paused: false,
            pending_portal_sync_after_wg: false,
            browse_focus: 0,
            browse_drag_last_dy: 0.0,
            browse_coast_vy: 0.0,
            pressed_mosaic: None,
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
        app.form_download_dir = app.settings.download_dir.clone();
        app.download_partials = crate::downloads::scan_partials(&app.downloads_dir());
        app.form_dns_servers = app.settings.network.dns_servers.clone();
        app.form_doh_url = app.settings.network.doh_url.clone();
        app.form_dot_server = app.settings.network.dot_server.clone();
        crate::network::apply_to_http(&app.settings.network);
        crate::metadata::set_omdb_api_key(if app.settings.omdb_api_key.is_empty() {
            None
        } else {
            Some(app.settings.omdb_api_key.clone())
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
            channels = app.channel_count,
            vod = app.vod_count,
            series = app.series_count,
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

    /// Step chrome_alpha toward show/hide over ~MOTION_SHORT_MS.
    fn lerp_chrome_alpha(&mut self) {
        let show = self.player_chrome_visible || self.player_panel != PlayerPanel::None;
        let target = if show { 1.0_f32 } else { 0.0_f32 };
        if (self.chrome_alpha - target).abs() < 0.001 {
            self.chrome_alpha = target;
            return;
        }
        // ~0.15/tick ≈ MOTION_SHORT_MS at ~100–120ms GUI period.
        let step = (self.display_caps.gui_period_ms().max(16) as f32 / MOTION_SHORT_MS as f32)
            .clamp(0.12, 0.35);
        if self.chrome_alpha < target {
            self.chrome_alpha = (self.chrome_alpha + step).min(1.0);
        } else {
            self.chrome_alpha = (self.chrome_alpha - step).max(0.0);
        }
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
                content,
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill)
            .align_y(Alignment::Start)
            .into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let closes = window::close_events().map(Message::WindowClosed);
        // Android application() creates one window — catch it even if boot bind raced.
        #[cfg(target_os = "android")]
        let opens = window::open_events().map(Message::MainWindowOpened);
        #[cfg(not(target_os = "android"))]
        let opens = Subscription::<Message>::none();
        let moves = window::events().filter_map(|(id, event)| match event {
            window::Event::Resized(size) => Some(Message::WindowResized { id, size }),
            window::Event::Moved(_) => Some(Message::PlayerLayoutDirty(id)),
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
                event::listen_with(map_browse_keys)
            }
        };
        #[cfg(target_os = "android")]
        let saf = iced::time::every(std::time::Duration::from_millis(250)).map(|_| Message::SafPoll);
        #[cfg(not(target_os = "android"))]
        let saf = Subscription::<Message>::none();
        Subscription::batch([closes, opens, moves, tick, chrome_tick, coast_tick, keys, saf])
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
                return Task::batch([window::gain_focus(id), self.sync_player_layout_task(id)]);
            }
            let (_id, open) = window::open(window::Settings {
                size: Size::new(1120.0, 720.0),
                min_size: Some(Size::new(480.0, 270.0)),
                position: window::Position::Centered,
                icon: app_icon(),
                exit_on_close_request: true,
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
            // Auto soft present capped at 720p for the ~100 Mo RSS budget.
            // FLUXPLAY_SOFT_UHD lifts Auto to 1080p; explicit quality prefs still apply.
            let uhd = std::env::var_os("FLUXPLAY_SOFT_UHD").is_some();
            let (qw, qh) = self.settings.video_quality.max_wh().unwrap_or(if uhd {
                (1920, 1080)
            } else {
                (1280, 720)
            });
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
        debug_assert!(
            {
                let uhd = std::env::var_os("FLUXPLAY_SOFT_UHD").is_some()
                    || self.settings.video_quality.max_wh().is_some();
                crate::invariants::soft_present_within_budget(rw, rh, uhd)
                    || rw <= 1920 && rh <= 1080
            },
            "soft present {rw}x{rh} outside budget"
        );
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
            self.video_frame = None;
            self.video_allocation = None;
            self.video_allocation_hold = None;
            self.video_allocation_hold2 = None;
            self.video_frame_wh = (0, 0);
            self.clear_stage_picture();
            // Soft RGBA used the shared GPU image cache — bump poster handles (new iced
            // ids) then reload only on-screen art (no off-screen warm).
            self.refresh_browse_poster_gpu_textures();
            self.art_warm_cursor = 0;
            self.art_warm_active = false;
        } else if !self.browse_index.is_empty() {
            // Playback ended or zapped without clearing the stage — mosaic can still
            // sample stale atlas regions from soft RGBA uploads.
            self.refresh_browse_poster_gpu_textures();
            self.art_warm_cursor = 0;
            self.art_warm_active = false;
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
        }
    }

    fn clear_stage_picture(&mut self) {
        self.stage_picture = false;
        #[cfg(not(target_os = "android"))]
        if let Some(frame) = crate::video_stage::clear() {
            self.session.recycle_video_frame(frame);
        }
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
                    self.player_pointer_at = Some(std::time::Instant::now());
                    crate::android_bridge::set_immersive_mode(true);
                    // Phones: lock landscape while playing. TV is already landscape.
                    if !self.layout_metrics().surface.is_television() {
                        crate::android_bridge::set_force_landscape(true);
                    }
                    // Do NOT show SurfaceView here — Z-order on-top blacks the browse UI.
                    // Bind warms it only at play start.
                }
                let layout = self.sync_player_layout_task(id);
                if std::env::var_os("FLUXPLAY_AUTO_FULLSCREEN").is_some() {
                    return Task::batch([layout, Task::done(Message::ToggleFullscreen)]);
                }
                return layout;
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
                    return self.sync_player_layout_task(id);
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
                if self.player_id == Some(id) {
                    self.player_id = None;
                    self.player_pos = None;
                    self.video_frame = None;
                    self.video_allocation = None;
                    self.video_allocation_hold = None;
                    self.video_allocation_hold2 = None;
                    self.video_frame_wh = (0, 0);
                    self.invalidate_soft_stage(true);
                    self.player_fullscreen = false;
                    self.player_chrome_visible = true;
                    self.player_panel = PlayerPanel::None;
                    self.session.stop();
                    self.status = "Lecteur fermé".into();
                }
                if self.main_id == Some(id) {
                    self.main_id = None;
                    self.session.stop();
                    if let Some(pid) = self.player_id.take() {
                        return Task::batch([window::close(pid), iced::exit()]);
                    }
                    return iced::exit();
                }
            }
            Message::ClosePlayerWindow => {
                // Cancel any deferred Stop→close and tear down now.
                self.player_close_gen = self.player_close_gen.wrapping_add(1);
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
            Message::Tab(tab) => {
                tracing::debug!(?tab, "tab");
                self.tab = tab;
                self.browse_drag_moved = false;
                self.browse_dragging = false;
                self.cat_filter.clear();
                // Each tab has its own search ("Rechercher une série…"): a query typed
                // in Séries silently filtered Films / TV too.
                self.search.clear();
                self.search_debounce_gen = self.search_debounce_gen.wrapping_add(1);
                self.series_detail = None;
                self.vod_detail = None;
                self.episode_flat.clear();
                self.series_queue.clear();
                self.detail_meta_loading = false;
                self.trim_ui_session_caches();
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
                let mut tasks = vec![
                    self.rebuild_browse_index(),
                    self.refresh_browse_art(),
                    self.snap_browse_scroll_task(),
                    self.snap_cat_scroll_task(),
                ];
                if tab == Tab::Vod && !self.xtream_vod_enrich_started {
                    self.xtream_vod_enrich_started = true;
                    tasks.push(self.enrich_xtream_vod_batch_task());
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
                return Task::batch([self.rebuild_browse_index(), self.refresh_browse_art()]);
            }
            Message::CatFilterChanged(s) => {
                self.cat_filter = s;
                self.rebuild_cat_entries();
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
                        // Titles stay sync-cheap; only art waits for settle.
                        let _ = self.fill_visible_browse_tiles();
                        return Self::browse_settle_task();
                    }
                    if settled {
                        return Task::batch([
                            self.flush_pending_images(),
                            self.fill_visible_browse_tiles(),
                            self.refresh_browse_art(),
                        ]);
                    }
                    // Slow drag: keep tile holes filled without art thrash.
                    return self.fill_visible_browse_tiles();
                }
                self.browse_slice = slice;
                // Tile metadata sync (~ms); skip image storm while flinging.
                let _ = self.fill_visible_browse_tiles();
                if flinging {
                    return Self::browse_settle_task();
                }
                return Task::batch([
                    self.flush_pending_images(),
                    self.refresh_browse_art(),
                ]);
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
                    // Still scrolling: a later scroll event re-arms the check.
                    return Task::none();
                }
                self.browse_scroll_vy = 0.0;
                self.browse_was_flinging = false;
                self.browse_slice =
                    self.content_virtual_slice(self.browse_scroll_y, self.browse_view_h);
                return Task::batch([
                    self.flush_pending_images(),
                    self.fill_visible_browse_tiles(),
                    self.refresh_browse_art(),
                ]);
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
                // Titles immediately; art once the wheel rests (BrowseSettle).
                let _ = self.fill_visible_browse_tiles();
                self.browse_scroll_at = Some(std::time::Instant::now());
                return Task::batch([
                    iced::widget::operation::scroll_by(
                        iced::widget::Id::from(self.browse_scroll_widget_id()),
                        AbsoluteOffset { x: 0.0, y: dy },
                    ),
                    Self::browse_settle_task(),
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
            Message::MosaicShowMore => {
                let total = self.browse_index.len();
                if !matches!(self.tab, Tab::Vod | Tab::Series)
                    || self.vod_detail.is_some()
                    || self.series_detail.is_some()
                    || self.mosaic_shown >= total
                {
                    return Task::none();
                }
                self.mosaic_shown = (self.mosaic_shown + browser::MOSAIC_PAGE_ITEMS).min(total);
                return Task::batch([
                    self.fill_visible_browse_tiles(),
                    self.prefetch_visible_art(),
                ]);
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
                if key == self.cat_slice && (view_h - self.cat_view_h).abs() < 8.0 {
                    return Task::none();
                }
                self.cat_scroll_y = y;
                self.cat_view_h = view_h;
                self.cat_slice = key;
            }
            Message::SelectBrowseCategory(id) => {
                let task = match self.tab {
                    Tab::Live => {
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
                        let idx = self.rebuild_browse_index();
                        let follow = if id == "*"
                            || self
                                .catalog_db
                                .as_ref()
                                .map(|db| {
                                    db.count_vod(&self.enabled_source_ids(), Some(id.as_str())) > 0
                                })
                                .unwrap_or(false)
                        {
                            self.refresh_browse_art()
                        } else {
                            self.load_vod_category_task(id)
                        };
                        Task::batch([idx, follow])
                    }
                    Tab::Series => {
                        self.selected_series_category = Some(id.clone());
                        self.series_detail = None;
                        self.detail_meta_loading = false;
                        let idx = self.rebuild_browse_index();
                        let follow = if id == "*"
                            || self
                                .catalog_db
                                .as_ref()
                                .map(|db| {
                                    db.count_series(&self.enabled_source_ids(), Some(id.as_str()))
                                        > 0
                                })
                                .unwrap_or(false)
                        {
                            self.refresh_browse_art()
                        } else {
                            self.load_series_category_task(id)
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
                let mut ch = self
                    .catalog_db
                    .as_ref()
                    .and_then(|db| db.channel_by_id(&id));
                if let Some(ch) = ch.as_mut() {
                    if ch.source_id.is_none() {
                        ch.source_id = source_id;
                    }
                    return Task::done(Message::PlayChannel(ch.clone()));
                }
                self.status = "Chaîne introuvable".into();
                return Task::none();
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
                        self.trim_download_session_map();
                        return self.pump_download_queue();
                    }
                    DownloadEvent::Finished(Err(error)) => {
                        self.status = format!("Téléchargement échoué — {name} : {error}");
                        self.download_partials =
                            crate::downloads::scan_partials(&self.downloads_dir());
                        self.downloads.insert(url, DownloadState::Failed);
                        self.trim_download_session_map();
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
                // Invalidate deferred Stop→close so a quick zap cannot kill this play.
                self.player_close_gen = self.player_close_gen.wrapping_add(1);
                tracing::info!(id = %ch.id, name = %ch.name, "play channel");
                self.selected_channel = Some(ch.id.clone());
                self.apply_source_headers_for(&ch);
                self.settings.push_recent(&ch);
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
                                return Task::batch([
                                    epg_task,
                                    art_task,
                                    self.open_or_focus_player(),
                                ]);
                            }
                            Err(e) => {
                                self.status = format!("Intent: {e}");
                                self.persist();
                                return Task::batch([
                                    epg_task,
                                    art_task,
                                    self.open_or_focus_player(),
                                ]);
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
                        return Task::batch([
                            epg_task,
                            art_task,
                            self.open_or_focus_player(),
                        ]);
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
                            return Task::batch([epg_task, art_task, self.open_or_focus_player()]);
                        }
                        #[cfg(not(target_os = "android"))]
                        {
                            self.status = format!("{e} — essayez Externe ou installez mpv");
                            self.persist();
                            return Task::batch([epg_task, art_task, self.open_or_focus_player()]);
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
                let art_url = poster.clone();
                if kind == ContentKind::Series {
                    self.arm_series_queue_for_url(&url);
                } else {
                    self.series_queue.clear();
                    self.series_queue_idx = 0;
                }
                self.prefetch_armed_for = None;
                let stream_url = url.clone();
                // A downloaded copy plays from disk: no stream, no panel slot.
                let local = self
                    .downloaded_file(&url, &name, kind)
                    .and_then(|file| {
                        let play = crate::downloads::local_play_url(&file)?;
                        Some((file, play))
                    });
                let url = match local {
                    Some((file, play)) => {
                        // The players only open `file:` under this root.
                        if let Some(dir) = file.parent().and_then(|d| d.to_str()) {
                            std::env::set_var("FLUXPLAY_DOWNLOAD_ROOT", dir);
                        }
                        tracing::info!(name = %name, "playing downloaded file");
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
                                return self.open_or_focus_player();
                            }
                            Err(e) => {
                                self.status = format!("Intent: {e}");
                                return self.open_or_focus_player();
                            }
                        }
                    }
                }
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
                        self.apply_saved_video_defaults_for_current_play();
                        self.status = self.session.status_line();
                        let art = crate::images::pick_art(
                            art_url.as_deref(),
                            art_url.as_deref(),
                            None,
                            None,
                        );
                        let art_task = art
                            .map(|u| self.prefetch_urls(std::iter::once(u)))
                            .unwrap_or_else(Task::none);
                        return Task::batch([art_task, self.open_or_focus_player()]);
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
                            return self.open_or_focus_player();
                        }
                        #[cfg(not(target_os = "android"))]
                        {
                            let _ = stream_url;
                            self.status = format!(
                                "{e} — 1 connexion max: Stop puis réessayez, ou Externe"
                            );
                            return self.open_or_focus_player();
                        }
                    }
                }
            }
            #[cfg(target_os = "android")]
            Message::SafPoll => {
                crate::android_platform::ensure_post_notifications_permission();
                crate::android_platform::register_default_callback();
                if let Some(action) = crate::android_platform::take_pending() {
                    let id = self.player_id.or(self.main_id).unwrap_or(window::Id::unique());
                    let hk = match action {
                        crate::android_platform::MediaAction::TogglePause => {
                            PlayerHotkey::TogglePause
                        }
                        crate::android_platform::MediaAction::Play => PlayerHotkey::TogglePause,
                        crate::android_platform::MediaAction::Pause => PlayerHotkey::TogglePause,
                        crate::android_platform::MediaAction::Stop => PlayerHotkey::Stop,
                    };
                    return Task::done(Message::PlayerHotkeyIn(id, hk));
                }
                if crate::android_platform::take_reconnect_pulse() {
                    if let Some(ch) = self.session.channel.clone() {
                        if matches!(
                            self.session.state,
                            PlaybackState::Playing
                                | PlaybackState::Buffering
                                | PlaybackState::Paused
                                | PlaybackState::Error
                        ) {
                            self.status = "Réseau OK — reconnexion…".into();
                            return Task::done(Message::PlayChannel(ch));
                        }
                    }
                }
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
            Message::BrowseFocusStep { dcol, drow } => {
                let len = self.mosaic_display_len();
                if len == 0 || (dcol == 0 && drow == 0) {
                    return Task::none();
                }
                let m = self.layout_metrics();
                let cols = m.cols.max(1) as i32;
                let cur = self.browse_focus as i32;
                let row = cur / cols;
                let col = cur % cols;
                let nrow = (row + drow).max(0);
                let ncol = (col + dcol).clamp(0, cols - 1);
                let next = (nrow * cols + ncol).clamp(0, len.saturating_sub(1) as i32) as usize;
                self.browse_focus = next;
                // Keep the focused cell near mid-viewport on 10-foot surfaces.
                if m.surface.is_television() {
                    let row_h = browser::mosaic_row_height_for(m.tile_w, m.touch_target);
                    let target_y = (next as f32 / cols as f32) * row_h
                        - (self.browse_view_h * 0.35).max(0.0);
                    self.browse_scroll_y = target_y.max(0.0);
                }
                if matches!(self.tab, Tab::Live | Tab::Favorites) {
                    if let Some(tile) = self.tile_cache.peek(next) {
                        self.selected_channel = Some(tile.id.clone());
                    }
                }
                let _ = self.fill_visible_browse_tiles();
                return Task::none();
            }
            Message::BrowseActivate => {
                let Some(tile) = self.tile_cache.peek(self.browse_focus) else {
                    return Task::none();
                };
                return match self.tab {
                    Tab::Live | Tab::Favorites => Task::done(Message::PlayChannelId(
                        tile.id.clone(),
                        tile.source_id,
                    )),
                    Tab::Vod => Task::done(Message::OpenVodDetail(tile.id.clone())),
                    Tab::Series => Task::done(Message::OpenSeries(tile.id.clone())),
                    _ => Task::none(),
                };
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
                self.session.stop();
                self.invalidate_soft_stage(true);
                #[cfg(target_os = "android")]
                {
                    self.android_force_soft = false;
                    self.android_want_surface_upgrade = false;
                    crate::android_bridge::release_video_surface_wid();
                    crate::android_bridge::set_hdr_color_mode(false);
                    crate::android_platform::hide_playback_notification();
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
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                if self.player_fullscreen {
                    self.player_fullscreen = false;
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
                self.status = "Plein écran".into();
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
            }
            Message::PlayerPointerActivity => {
                let now = std::time::Instant::now();
                // After autohide, ignore wake for 700ms (compositor enter/leave flicker loop).
                if !self.player_chrome_visible {
                    if let Some(at) = self.player_pointer_at {
                        if now.duration_since(at) < std::time::Duration::from_millis(700) {
                            return Task::none();
                        }
                    }
                } else if let Some(at) = self.player_pointer_at {
                    if now.duration_since(at) < std::time::Duration::from_millis(250) {
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
            Message::CycleSubtitles => {
                self.status = if self.session.cycle_subtitles() {
                    "Sous-titres suivants".into()
                } else {
                    "Sous-titres non supportés".into()
                };
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
                        // Stale audio_focus.json must NOT pause a fresh play
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
                    self.session.refresh_buffering_state();
                    #[cfg(target_os = "android")]
                    {
                        let playing = matches!(
                            self.session.state,
                            PlaybackState::Playing | PlaybackState::Buffering
                        );
                        let title = self
                            .session
                            .channel
                            .as_ref()
                            .map(|c| c.name.as_str())
                            .unwrap_or("FluxPlay");
                        let pos_ms =
                            (self.session.position_secs.max(0.0) * 1000.0).round() as i64;
                        if !matches!(self.session.backend, Some(BackendId::External)) {
                            crate::android_platform::sync_playback_notification(
                                playing, title, pos_ms,
                            );
                        }
                    }
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
                        if since.elapsed() >= std::time::Duration::from_millis(400) {
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
                if !tasks.is_empty() {
                    return Task::batch(tasks);
                }
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
                        // Keep 3 GPU soft frames (current + 2 holds) for the RSS budget.
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
            }
            Message::CycleSpeed => {
                self.session.cycle_speed();
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
                            Size::new(1120.0, 720.0)
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
            Message::PlaylistPrev => {
                if let Some(msg) = self.playlist_neighbor(-1) {
                    return Task::done(msg);
                }
            }
            Message::PlaylistNext => {
                if let Some(msg) = self.playlist_neighbor(1) {
                    return Task::done(msg);
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
            Message::CycleAccent => {
                self.settings.accent = self.settings.accent.cycle();
                tracing::debug!(accent = ?self.settings.accent, "cycle accent");
                self.persist();
            }
            Message::SetAccent(preset) => {
                self.settings.accent = preset;
                tracing::debug!(accent = ?preset, "set accent");
                self.persist();
            }
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
                    PasteTarget::FormDnsServers => self.form_dns_servers = cleaned,
                    PasteTarget::FormDohUrl => self.form_doh_url = cleaned,
                    PasteTarget::FormDotServer => self.form_dot_server = cleaned,
                    PasteTarget::FormWgPaste => self.form_wg_paste = cleaned,
                    PasteTarget::Search => {
                        self.search = cleaned;
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
            Message::ToggleTranslateMeta => {
                self.settings.translate_meta = !self.settings.translate_meta;
                self.persist();
                self.status = if self.settings.translate_meta {
                    "Traduction des synopsis activée".into()
                } else {
                    "Traduction des synopsis désactivée".into()
                };
                return self.translate_open_detail_task();
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
                // Synopses across many detail pages grow without bound — keep a small LRU-ish set.
                const TRANSLATIONS_CAP: usize = 384;
                if self.translations.len() > TRANSLATIONS_CAP {
                    self.translations.clear();
                    self.translating.clear();
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
                // Force fresh portal JSON; DB write still skipped if spine checksum matches.
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
                if let Some(db) = &self.catalog_db {
                    db.wal_checkpoint_all();
                }
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
                            if self.settings.favorites.len() < 2_000
                                && !self.settings.favorites.iter().any(|x| x == &f)
                            {
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
            Message::BundleCacheReady(result) => {
                self.loading = false;
                match result {
                    Ok(spine) => {
                        self.apply_spine_cache(spine);
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
                                self.channel_count,
                                self.vod_count,
                                self.series_count,
                            )
                        } else {
                            format!(
                                "DB locale · {} live · {} VOD · {} séries",
                                self.channel_count,
                                self.vod_count,
                                self.series_count,
                            )
                        };
                        let mut tasks = vec![self.rebuild_browse_index(), self.fill_visible_browse_tiles()];
                        if sync_fresh && std::env::var_os("FLUXPLAY_AUTO_URL").is_none() {
                            tasks.push(self.after_catalog_ready_tasks());
                        } else {
                            tasks.push(self.prefetch_catalog_art());
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
                search_tiles,
            } => {
                if gen != self.jobs.browse_gen() {
                    return Task::none();
                }
                self.browse_index = index;
                self.episode_flat = episode_flat;
                if !search_tiles.is_empty() {
                    self.tile_cache.clear();
                    let pairs: Vec<_> = search_tiles
                        .into_iter()
                        .enumerate()
                        .map(|(i, t)| (i, t))
                        .collect();
                    self.tile_cache.store_at(pairs);
                }
                if self.browse_focus >= self.browse_index.len() {
                    self.browse_focus = self.browse_index.len().saturating_sub(1);
                }
                self.finish_browse_index_rebuild();
                return Task::batch([
                    self.snap_browse_scroll_task(),
                    self.fill_visible_browse_tiles(),
                    self.refresh_browse_art(),
                ]);
            }
            Message::MetaEnriched { series, vod } => {
                for (id, source_id, patch) in series {
                    if let Some(db) = &self.catalog_db {
                        if let Some(mut item) = db.series_item_by_id(&id) {
                            patch.apply_series(&mut item);
                            if let Some(sid) = source_id.or(item.source_id) {
                                item.source_id = Some(sid);
                                let _ = db.persist_series(&item);
                            }
                        }
                    }
                }
                for (id, source_id, patch) in vod {
                    if let Some(db) = &self.catalog_db {
                        if let Some(mut item) = db.vod_item_by_id(&id) {
                            patch.apply_vod(&mut item);
                            if let Some(sid) = source_id.or(item.source_id) {
                                item.source_id = Some(sid);
                                let _ = db.persist_vod(&item);
                            }
                        }
                    }
                }
                if let Some(detail) = &mut self.series_detail {
                    if let Some(db) = &self.catalog_db {
                        if let Some(updated) = db.series_item_by_id(&detail.id) {
                            let seasons = detail.seasons.clone();
                            *detail = updated;
                            if detail.seasons.is_empty() {
                                detail.seasons = seasons;
                            }
                        }
                    }
                }
                if let Some(detail) = &mut self.vod_detail {
                    if let Some(db) = &self.catalog_db {
                        if let Some(updated) = db.vod_item_by_id(&detail.id) {
                            *detail = updated;
                        }
                    }
                }
                // Visible band only — full catalog warm after meta was a RAM ratchet.
                return self.light_art_refresh_if_browsing();
            }
            Message::VodInfoBatch(items) => {
                for item in items {
                    if let Some(db) = &self.catalog_db {
                        if let Some(mut saved) = db.vod_item_by_id(&item.id) {
                            merge_xtream_vod_fields(&mut saved, &item);
                            if saved.source_id.is_some() {
                                let _ = db.persist_vod(&saved);
                            }
                        }
                    }
                    if self.vod_detail.as_ref().map(|d| d.id.as_str()) == Some(item.id.as_str()) {
                        if let Some(detail) = self.vod_detail.as_mut() {
                            merge_xtream_vod_fields(detail, &item);
                        }
                    }
                }
                return self.light_art_refresh_if_browsing();
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
                if self.tab == Tab::Favorites {
                    return Task::batch([
                        self.rebuild_browse_index(),
                        self.refresh_browse_art(),
                    ]);
                }
            }
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
                self.settings.video_quality = self.settings.video_quality.cycle();
                self.resync_player_options();
                self.persist();
                if self.session.channel.is_some() {
                    #[cfg(target_os = "android")]
                    {
                        let (rw, rh) = self.soft_present_wh();
                        self.session.native.options_mut().android_soft_vf =
                            Some(
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
                self.status = format!("Qualité vidéo : {}", self.settings.video_quality.label());
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
                    // SQLite always; RAM only while Live/EPG/Favorites need now/next.
                    if matches!(self.tab, Tab::Live | Tab::Epg | Tab::Favorites) {
                        fluxplay_providers::merge_epg(&mut self.bundle.epg, programmes.clone());
                    }
                    if let Some(db) = &self.catalog_db {
                        if let Err(e) = db.merge_epg(source_id, &programmes) {
                            tracing::warn!(error = %e, "persist epg failed");
                        }
                    }
                    if self.tab == Tab::Epg {
                        self.status = format!("Guide TV · {n} programmes");
                    }
                }
            }
            Message::VodCategoryLoaded {
                category_id,
                sources,
                result,
            } => {
                // Persist always (SQLite is the source of truth). Rebuild + art warm
                // only when this category is still selected — stale loads used to
                // chain spine reloads and re-warm mosaics for every previous click.
                let current = self.selected_vod_category.as_deref() == Some(category_id.as_str());
                match result {
                    Ok(items) => {
                        let n = items.len();
                        let sid0 = sources.first().copied();
                        let source_ids = self.enabled_source_ids();
                        let cid = category_id.clone();
                        if current {
                            self.status = format!("VOD catégorie · {n} films — écriture SQLite…");
                        }
                        return Task::perform(
                            async move {
                                let _ = crate::async_jobs::run_blocking(move || {
                                    if let Some(db) = crate::catalog_db::CatalogDb::open(&source_ids)
                                    {
                                        for mut item in items {
                                            if item.source_id.is_none() {
                                                item.source_id = sid0;
                                            }
                                            let _ = db.persist_vod(&item);
                                        }
                                    }
                                })
                                .await;
                                cid
                            },
                            |category_id| Message::CategoryPersistDone {
                                category_id,
                                is_series: false,
                            },
                        );
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
                let current =
                    self.selected_series_category.as_deref() == Some(category_id.as_str());
                match result {
                    Ok(items) => {
                        let n = items.len();
                        let sid0 = sources.first().copied();
                        let source_ids = self.enabled_source_ids();
                        let cid = category_id.clone();
                        if current {
                            self.status = format!("Séries catégorie · {n} titres — écriture SQLite…");
                        }
                        return Task::perform(
                            async move {
                                let _ = crate::async_jobs::run_blocking(move || {
                                    if let Some(db) = crate::catalog_db::CatalogDb::open(&source_ids)
                                    {
                                        for mut item in items {
                                            if item.source_id.is_none() {
                                                item.source_id = sid0;
                                            }
                                            let _ = db.persist_series(&item);
                                        }
                                    }
                                })
                                .await;
                                cid
                            },
                            |category_id| Message::CategoryPersistDone {
                                category_id,
                                is_series: true,
                            },
                        );
                    }
                    Err(e) if current => self.status = format!("Séries: {e}"),
                    Err(_) => {}
                }
            }
            Message::CategoryPersistDone {
                category_id,
                is_series,
            } => {
                let current = if is_series {
                    self.selected_series_category.as_deref() == Some(category_id.as_str())
                } else {
                    self.selected_vod_category.as_deref() == Some(category_id.as_str())
                };
                if !current {
                    return Task::none();
                }
                self.status = "Catégorie prête — relecture SQLite…".into();
                return self.rebuild_bundle_from_cache_task();
            }
            Message::OpenSeries(id) => {
                self.pressed_mosaic = None;
                if self.consume_browse_drag_suppress() {
                    return Task::none();
                }
                self.vod_detail = None;
                self.detail_meta_loading = false;
                self.browse_scroll_y = 0.0;
                self.browse_view_h = 0.0;
                if let Some(mut detail) = self
                    .catalog_db
                    .as_ref()
                    .and_then(|db| db.series_item_by_id(&id))
                {
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
                    self.rebuild_episode_flat();
                }
                self.status = "Chargement épisodes…".into();
                let translate = self.translate_open_detail_task();
                let wanted = self
                    .series_detail
                    .as_ref()
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
                        return Task::batch([self.enrich_open_detail_task(true), translate]);
                    }
                    return translate;
                };
                return Task::batch([
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
                    self.status = format!(
                        "{} · {} saisons · {eps} épisodes",
                        crate::names::display_title(&item.name),
                        item.seasons.len()
                    );
                    if let Some(db) = &self.catalog_db {
                        if let Some(mut saved) = db.series_item_by_id(&item.id) {
                            merge_xtream_series_fields(&mut saved, &item);
                            if saved.source_id.is_some() {
                                let _ = db.persist_series(&saved);
                            }
                        }
                    }
                    let open = self.series_detail.as_ref().map(|d| d.id.as_str());
                    if open.map(|id| id != item.id).unwrap_or(false) {
                        return Task::none();
                    }
                    match self.series_detail.as_mut() {
                        Some(detail) => merge_xtream_series_fields(detail, &item),
                        None => self.series_detail = Some(item),
                    }
                    self.browse_scroll_y = 0.0;
                    self.rebuild_episode_flat();
                    return Task::batch([
                        self.prefetch_visible_art(),
                        self.enrich_open_detail_task(true),
                        self.enrich_episodes_task(),
                        self.translate_open_detail_task(),
                    ]);
                }
                Err(e) => self.status = format!("Série: {e}"),
            },
            Message::SeriesEpisodesEnriched(item) => {
                let open = self.series_detail.as_ref().map(|d| d.id.as_str()) == Some(item.id.as_str());
                if open {
                    if let Some(detail) = &mut self.series_detail {
                        detail.seasons = item.seasons.clone();
                    }
                    self.rebuild_episode_flat();
                }
                if let (Some(db), Some(sid)) = (&self.catalog_db, item.source_id) {
                    let mut saved = item;
                    saved.source_id = Some(sid);
                    let _ = db.persist_series(&saved);
                }
                if open {
                    return self.translate_open_detail_task();
                }
            },
            Message::CloseSeriesDetail => {
                self.series_detail = None;
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
                self.series_detail = None;
                self.browse_scroll_y = 0.0;
                self.browse_view_h = 0.0;
                let Some(item) = self
                    .catalog_db
                    .as_ref()
                    .and_then(|db| db.vod_item_by_id(&id))
                else {
                    self.status = "Film introuvable".into();
                    return Task::none();
                };
                self.detail_meta_loading = true;
                self.status = format!("{} — fiche", crate::names::display_title(&item.name));
                let vod_id = item.id.clone();
                let wanted = item.source_id;
                self.vod_detail = Some(item);
                let translate = self.translate_open_detail_task();
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
                            fluxplay_providers::load_xtream_vod_info(&src, &vod_id)
                                .await
                                .map_err(|e| e.to_string())
                        },
                        Message::VodDetailLoaded,
                    )
                } else {
                    self.enrich_open_detail_task(false)
                };
                return Task::batch([self.prefetch_visible_art(), xtream, translate]);
            }
            Message::VodDetailLoaded(result) => match result {
                Ok(item) => {
                    if let Some(db) = &self.catalog_db {
                        if let Some(mut saved) = db.vod_item_by_id(&item.id) {
                            merge_xtream_vod_fields(&mut saved, &item);
                            if saved.source_id.is_some() {
                                let _ = db.persist_vod(&saved);
                            }
                        }
                    }
                    // Merge into the open page: the panel answer lacks fields the catalog /
                    // OMDb already filled (writer, awards, …) and may carry empty ones.
                    if let Some(detail) = self.vod_detail.as_mut().filter(|d| d.id == item.id) {
                        merge_xtream_vod_fields(detail, &item);
                        let name = crate::names::display_title(&detail.name);
                        self.status = if detail.plot.is_some() {
                            format!("{name} · fiche Xtream")
                        } else {
                            format!("{name} — enrichissement…")
                        };
                    }
                    self.detail_meta_loading = false;
                    return Task::batch([
                        self.prefetch_visible_art(),
                        self.enrich_open_detail_task(false),
                        self.translate_open_detail_task(),
                    ]);
                }
                Err(e) => {
                    tracing::warn!(%e, "vod_info failed — falling back to OMDb/Wikipedia");
                    self.status = format!("Fiche panel: {e}");
                    return self.enrich_open_detail_task(false);
                }
            },
            Message::CloseVodDetail => {
                self.vod_detail = None;
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
                let still_open = if is_series {
                    self.series_detail.as_ref().is_some_and(|d| d.id == id)
                } else {
                    self.vod_detail.as_ref().is_some_and(|d| d.id == id)
                };
                // Late response for a previous fiche must not clear the spinner of the new one.
                if still_open {
                    self.detail_meta_loading = false;
                }
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
                    if let Some(db) = &self.catalog_db {
                        if let Some(mut item) = db.series_item_by_id(&id) {
                            patch.apply_series(&mut item);
                            let _ = db.persist_series(&item);
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
                    if let Some(db) = &self.catalog_db {
                        if let Some(mut item) = db.vod_item_by_id(&id) {
                            patch.apply_vod(&mut item);
                            let _ = db.persist_vod(&item);
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
                return Task::batch([self.refresh_browse_art(), self.translate_open_detail_task()]);
            }
            Message::ImageLoaded(gen, Ok((url, source_id, bytes))) => {
                self.jobs.images.tick();
                if gen != self.art_fetch_gen {
                    // Category / tab moved on — drop stale decode (was a major RSS leak).
                    return Task::none();
                }
                const PENDING_CAP: usize = 24;
                if self.pending_images.len() >= PENDING_CAP {
                    // Prefer flushing what we have over unbounded decode buffers mid-fling.
                    let _ = self.flush_pending_images();
                }
                self.pending_images.push((url, source_id, bytes));
                // During fling: buffer only — no iced view rebuild (LOD / culling).
                if self.browse_flinging() {
                    return Task::none();
                }
                if self.pending_images.len() >= 10 {
                    return Task::batch([
                        self.flush_pending_images(),
                        self.prefetch_visible_art(),
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
            Message::ImageLoaded(gen, Err((url, err))) => {
                self.jobs.images.tick();
                if gen != self.art_fetch_gen {
                    return Task::none();
                }
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
                return self.prefetch_visible_art();
            }
            Message::FlushPendingImages => {
                self.image_flush_armed = false;
                if self.browse_flinging() {
                    // Cap buffers even mid-fling — settle path will promote visibles.
                    if self.pending_images.len() >= 24 {
                        let _ = self.flush_pending_images();
                    }
                    return Task::none();
                }
                return Task::batch([
                    self.flush_pending_images(),
                    self.prefetch_visible_art(),
                ]);
            }
        }
        Task::none()
    }

    fn enrich_episodes_task(&self) -> Task<Message> {
        let Some(detail) = self.series_detail.clone() else {
            return Task::none();
        };
        let already = detail
            .seasons
            .iter()
            .flat_map(|s| s.episodes.iter())
            .any(|e| e.plot.is_some());
        if already {
            return Task::none();
        }
        Task::perform(
            async move {
                let mut item = detail;
                let name = item.name.clone();
                crate::metadata::enrich_series_episodes(&name, &mut item).await;
                Message::SeriesEpisodesEnriched(item)
            },
            |m| m,
        )
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

    /// Synopsis / genre / episode plots of the open detail page → viewer language.
    fn translate_open_detail_task(&mut self) -> Task<Message> {
        if !self.translation_on() {
            return Task::none();
        }
        let mut texts = Vec::new();
        if let Some(d) = &self.vod_detail {
            texts.extend(crate::metadata::fmt_plot(d.plot.as_deref(), &d.name));
            texts.extend(crate::metadata::fmt_genre(d.genre.as_deref()));
        }
        if let Some(d) = &self.series_detail {
            texts.extend(crate::metadata::fmt_plot(d.plot.as_deref(), &d.name));
            texts.extend(crate::metadata::fmt_genre(d.genre.as_deref()));
            texts.extend(
                d.seasons
                    .iter()
                    .flat_map(|s| s.episodes.iter())
                    .filter_map(|e| crate::metadata::fmt_plot(e.plot.as_deref(), &e.title))
                    .take(120),
            );
        }
        self.request_translations(texts)
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

    fn enrich_open_detail_task(&mut self, is_series: bool) -> Task<Message> {
        let (id, name, imdb_id, need) = if is_series {
            let Some(d) = &self.series_detail else {
                return Task::none();
            };
            (
                d.id.clone(),
                d.name.clone(),
                d.imdb_id.clone(),
                crate::metadata::needs_full_credits(d.actors.as_deref(), d.plot.as_deref()),
            )
        } else {
            let Some(d) = &self.vod_detail else {
                return Task::none();
            };
            (
                d.id.clone(),
                d.name.clone(),
                d.imdb_id.clone(),
                crate::metadata::needs_full_credits(d.actors.as_deref(), d.plot.as_deref()),
            )
        };
        if !need && imdb_id.is_some() {
            return Task::none();
        }
        let kind = if is_series { "series" } else { "movie" };
        let q = crate::metadata::parse_title_query(&name);
        let key = q.cache_key(kind);
        // Prefer local meta_cache (by imdb id or cleaned title) before network.
        if let Some(db) = &self.catalog_db {
            if let Some(tt) = imdb_id.as_deref().filter(|s| s.starts_with("tt")) {
                if let Some(patch) = db.meta_cache_get_imdb(tt) {
                    if !crate::metadata::needs_full_credits(
                        patch.actors.as_deref(),
                        patch.plot.as_deref(),
                    ) {
                        return Task::done(Message::DetailMetaLoaded {
                            is_series,
                            id: id.clone(),
                            patch: Some(patch),
                        });
                    }
                }
            }
            if let Some((miss, patch)) = db.meta_cache_get(&key) {
                if !miss
                    && !crate::metadata::needs_full_credits(
                        patch.actors.as_deref(),
                        patch.plot.as_deref(),
                    )
                {
                    return Task::done(Message::DetailMetaLoaded {
                        is_series,
                        id,
                        patch: Some(patch),
                    });
                }
            }
        }
        self.detail_meta_loading = true;
        Task::perform(
            async move {
                let patch =
                    crate::metadata::enrich_title_full(&name, kind, imdb_id.as_deref()).await;
                Message::DetailMetaLoaded {
                    is_series,
                    id,
                    patch,
                }
            },
            |m| m,
        )
    }

    /// After catalog load/reload: visible art + light meta.
    /// Xtream `get_vod_info` batch is deferred to first VOD tab open (portal ban risk).
    fn after_catalog_ready_tasks(&mut self) -> Task<Message> {
        self.xtream_vod_enrich_started = false;
        Task::batch([
            self.prefetch_catalog_art(),
            self.enrich_metadata_task(),
            self.fetch_epg_for_visible_task(),
        ])
    }

    fn enrich_metadata_task(&self) -> Task<Message> {
        let source_ids = self.enabled_source_ids();
        let mut series_cached = Vec::new();
        let mut series_fetch = Vec::new();
        let mut vod_cached = Vec::new();
        let mut vod_fetch = Vec::new();
        if let Some(db) = &self.catalog_db {
            for (sid, id) in db.series_ids_needing_enrich(&source_ids, 12) {
                let Some(s) = db.series_item_by_id(&id) else {
                    continue;
                };
                if !crate::metadata::needs_series_enrich(&s) {
                    continue;
                }
                let q = crate::metadata::parse_title_query(&s.name);
                let key = q.cache_key("series");
                if let Some((miss, patch)) = db.meta_cache_get(&key) {
                    if !miss {
                        series_cached.push((s.id.clone(), Some(sid).or(s.source_id), patch));
                    }
                    continue;
                }
                series_fetch.push((s.id.clone(), Some(sid).or(s.source_id), s.name.clone()));
            }
            for (sid, id) in db.vod_ids_needing_enrich(&source_ids, 24) {
                let Some(v) = db.vod_item_by_id(&id) else {
                    continue;
                };
                if !crate::metadata::needs_vod_enrich(&v) {
                    continue;
                }
                let q = crate::metadata::parse_title_query(&v.name);
                let key = q.cache_key("movie");
                if let Some((miss, patch)) = db.meta_cache_get(&key) {
                    if !miss {
                        vod_cached.push((v.id.clone(), Some(sid).or(v.source_id), patch));
                    }
                    continue;
                }
                vod_fetch.push((v.id.clone(), Some(sid).or(v.source_id), v.name.clone()));
            }
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

    /// Parallel Xtream `get_vod_info` for movies missing plot/cast/director/poster.
    /// Kept small + low concurrency — portals ban aggressive parallel scrapes.
    fn enrich_xtream_vod_batch_task(&self) -> Task<Message> {
        let mut jobs: Vec<(MediaSource, String)> = Vec::new();
        let source_ids = self.enabled_source_ids();
        if let Some(db) = &self.catalog_db {
            for (sid, id) in db.vod_ids_needing_enrich(&source_ids, 12) {
                let Some(v) = db.vod_item_by_id(&id) else {
                    continue;
                };
                if !crate::metadata::needs_vod_enrich(&v) {
                    continue;
                }
                let src = self
                    .sources
                    .iter()
                    .find(|s| s.id == sid && s.enabled && s.kind == SourceKind::Xtream)
                    .or_else(|| {
                        self.sources
                            .iter()
                            .find(|s| s.enabled && s.kind == SourceKind::Xtream)
                    });
                if let Some(src) = src {
                    jobs.push((src.clone(), v.id));
                }
            }
        }
        if jobs.is_empty() {
            return Task::none();
        }
        tracing::info!(n = jobs.len(), "xtream vod_info batch start");
        Task::perform(
            async move {
                let mut out = Vec::new();
                let mut set = tokio::task::JoinSet::new();
                const PARALLEL: usize = 2;
                let mut i = 0usize;
                while i < jobs.len() || !set.is_empty() {
                    while set.len() < PARALLEL && i < jobs.len() {
                        let (src, id) = jobs[i].clone();
                        i += 1;
                        set.spawn(async move {
                            fluxplay_providers::load_xtream_vod_info(&src, &id)
                                .await
                                .ok()
                        });
                    }
                    if let Some(Ok(Some(item))) = set.join_next().await {
                        out.push(item);
                    }
                }
                tracing::info!(n = out.len(), "xtream vod_info batch done");
                Message::VodInfoBatch(out)
            },
            |m| m,
        )
    }

    /// Fill mosaic cells for the visible browse window (+ overscan).
    ///
    /// Sync on the already-open catalog — a page is a few ms. Holes paint as "…"
    /// until filled; [`BrowseTileCache::viewport_health`] detects that in tests.
    fn fill_visible_browse_tiles(&mut self) -> Task<Message> {
        use crate::browse_tiles::BROWSE_TILE_FETCH_LIMIT;

        if self.browse_index.is_empty() {
            return Task::none();
        }
        if self.vod_detail.is_some() {
            return Task::none();
        }
        if matches!(self.tab, Tab::Series) && self.series_detail.is_some() {
            return Task::none();
        }
        let kind = match self.tab {
            Tab::Vod => ContentKind::Vod,
            Tab::Series => ContentKind::Series,
            Tab::Live | Tab::Favorites => ContentKind::Live,
            _ => return Task::none(),
        };
        self.tile_cache.set_kind(kind);
        let m = self.layout_metrics();
        let win = self.visible_browse_window(m.cols, m.tile_w);
        if win.start >= win.end {
            return Task::none();
        }

        // Already coherent: just refresh LRU order for the on-screen band.
        if self.tile_cache.viewport_health(win.start, win.end).is_coherent() {
            for pos in win.start..win.end {
                self.tile_cache.touch(pos);
            }
            return Task::none();
        }

        let Some(db) = self.catalog_db.as_ref() else {
            return Task::none();
        };
        let source_ids = self.enabled_source_ids();
        // Own filter strings so SQLite pages don't borrow `self` across cache mutates.
        let cat_vod = self.vod_cat_sql().map(str::to_owned);
        let cat_series = self.series_cat_sql().map(str::to_owned);
        let group_live = self.live_group_sql().map(str::to_owned);

        // At most two contiguous SQLite pages per frame — keeps scroll under budget.
        for _ in 0..2 {
            let Some((browse_start, browse_end)) =
                self.tile_cache.first_unfilled_run(win.start, win.end)
            else {
                break;
            };
            let catalog_start = self
                .browse_index
                .get(browse_start)
                .unwrap_or(browse_start);
            let span = (browse_end - browse_start).min(BROWSE_TILE_FETCH_LIMIT);
            let tiles = match kind {
                ContentKind::Vod => {
                    db.browse_tiles_vod(&source_ids, catalog_start, span, cat_vod.as_deref())
                }
                ContentKind::Series => {
                    db.browse_tiles_series(&source_ids, catalog_start, span, cat_series.as_deref())
                }
                ContentKind::Live => {
                    db.browse_tiles_channels(&source_ids, catalog_start, span, group_live.as_deref())
                }
            };
            if tiles.is_empty() {
                break;
            }
            self.tile_cache.store_contiguous(browse_start, tiles);
        }

        debug_assert!(
            self.tile_cache.len() <= crate::browse_tiles::BROWSE_TILE_CACHE_CAP,
            "BrowseTileCache exceeded soft RAM cap"
        );
        debug_assert!(
            {
                let h = self.tile_cache.viewport_health(win.start, win.end);
                // Incomplete only when SQLite returned fewer rows than the hole (EOF).
                h.holes == 0 || h.coverage() > 0.0
            },
            "browse fill left an empty viewport — mosaic would show blank rows"
        );
        Task::none()
    }

    /// Rebuild the browse index for the current tab / category / search.
    /// Heavy filters run on `spawn_blocking`; stale gens are ignored.
    ///
    /// Fast path: « All » + empty search → [`BrowseIndex::Identity`] (O(1), no million-Vec).
    fn rebuild_browse_index(&mut self) -> Task<Message> {
        self.browse_scroll_y = 0.0;
        self.browse_scroll_vy = 0.0;
        self.browse_was_flinging = false;
        self.browse_slice = (0, 0);
        // Keep previous `browse_index` until BrowseIndexReady — avoids empty mosaic flash.
        self.art_warm_cursor = 0;
        self.art_warm_active = false;
        self.mosaic_shown = browser::MOSAIC_PAGE_ITEMS;
        // Category switch: drop previous mosaic posters + pending decodes so RSS
        // cannot ratchet 200→600 Mo across clicks (iced atlas retains Handles).
        self.art_fetch_gen = self.art_fetch_gen.wrapping_add(1);
        self.pending_images.clear();
        self.image_flush_armed = false;
        self.images.clear_handles();
        // Reset image job meter — otherwise totals ratchet forever across categories.
        self.jobs.images.reset(0);
        self.tile_cache.clear();
        self.trim_ui_session_caches();
        self.rebuild_cat_entries();

        if matches!(self.tab, Tab::Series) && self.series_detail.is_some() {
            self.episode_flat.clear();
            self.rebuild_episode_flat();
            self.finish_browse_index_rebuild();
            return self.snap_browse_scroll_task();
        }
        if matches!(self.tab, Tab::Favorites) {
            let fav_ids: Vec<String> = self
                .settings
                .favorites
                .iter()
                .filter(|id| !id.is_empty())
                .cloned()
                .collect();
            let n = fav_ids.len();
            self.browse_index = BrowseIndex::identity(n);
            if let Some(db) = &self.catalog_db {
                let source_ids = self.enabled_source_ids();
                let tiles = db.slim_channels_by_ids(&source_ids, &fav_ids);
                let pairs: Vec<_> = tiles.into_iter().enumerate().map(|(i, t)| (i, t)).collect();
                self.tile_cache.set_kind(ContentKind::Live);
                self.tile_cache.store_at(pairs);
            }
            self.finish_browse_index_rebuild();
            return Task::batch([
                self.snap_browse_scroll_task(),
                self.fill_visible_browse_tiles(),
            ]);
        }
        if !matches!(self.tab, Tab::Live | Tab::Vod | Tab::Series) {
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
        let source_ids = self.enabled_source_ids();
        // Identity: SQLite catalog order — no million-entry Vec.
        if q.is_empty() && lang.is_none() {
            match self.tab {
                Tab::Vod => {
                    let len = self
                        .catalog_db
                        .as_ref()
                        .map(|db| db.count_vod(&source_ids, self.vod_cat_sql()))
                        .unwrap_or(self.vod_count);
                    self.browse_index = BrowseIndex::identity(len);
                    self.finish_browse_index_rebuild();
                    return Task::batch([
                        self.snap_browse_scroll_task(),
                        self.fill_visible_browse_tiles(),
                        self.refresh_browse_art(),
                    ]);
                }
                Tab::Series => {
                    let len = self
                        .catalog_db
                        .as_ref()
                        .map(|db| db.count_series(&source_ids, self.series_cat_sql()))
                        .unwrap_or(self.series_count);
                    self.browse_index = BrowseIndex::identity(len);
                    self.finish_browse_index_rebuild();
                    return Task::batch([
                        self.snap_browse_scroll_task(),
                        self.fill_visible_browse_tiles(),
                        self.refresh_browse_art(),
                    ]);
                }
                Tab::Live => {
                    let len = self
                        .catalog_db
                        .as_ref()
                        .map(|db| db.count_channels(&source_ids, self.live_group_sql()))
                        .unwrap_or(self.channel_count);
                    self.browse_index = BrowseIndex::identity(len);
                    self.finish_browse_index_rebuild();
                    return Task::batch([
                        self.snap_browse_scroll_task(),
                        self.fill_visible_browse_tiles(),
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
                        source_ids,
                        lang,
                        progress,
                    )
                })
                .await
                .unwrap_or_default()
            },
            move |(index, episode_flat, search_tiles)| Message::BrowseIndexReady {
                gen,
                index,
                episode_flat,
                search_tiles,
            },
        )
    }


    fn finish_browse_index_rebuild(&mut self) {
        // Visible-only posters — never re-arm off-screen warm after index rebuild.
        self.art_warm_active = false;
        self.art_warm_cursor = 0;
        self.browse_slice = self.content_virtual_slice(0.0, self.browse_view_h);
    }

    /// How many VOD/Series mosaic items are unlocked (page growth via « Afficher plus »).
    fn mosaic_display_len(&self) -> usize {
        let mosaic = matches!(
            (self.tab, self.vod_detail.is_some(), self.series_detail.is_some()),
            (Tab::Vod, false, _) | (Tab::Series, _, false)
        );
        if mosaic {
            self.browse_index
                .len()
                .min(self.mosaic_shown.max(browser::MOSAIC_PAGE_ITEMS))
        } else {
            self.browse_index.len()
        }
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

    /// Sidebar categories for the active tab (cached — not rebuilt on content scroll).
    fn rebuild_cat_entries(&mut self) {
        self.cat_scroll_y = 0.0;
        self.cat_slice = (0, 0);
        self.cat_entries.clear();
        match self.tab {
            Tab::Live => {
                let cats: Vec<(String, String)> = {
                    let cats = self.filtered_categories(ContentKind::Live);
                    if cats.is_empty() && !self.settings.only_pref_lang {
                        self.bundle
                            .group_names()
                            .into_iter()
                            .take(CAT_PAGE)
                            .map(|n| (n.clone(), crate::names::category_label(&n)))
                            .collect()
                    } else {
                        cats.into_iter()
                            .take(CAT_PAGE)
                            .map(|c| (c.name.clone(), crate::names::category_label(&c.name)))
                            .collect()
                    }
                };
                let all_active = self.selected_group.is_none();
                let all_count = if self.lang_filter_on() {
                    0
                } else {
                    let ids = self.enabled_source_ids();
                    self.catalog_db
                        .as_ref()
                        .map(|db| db.count_channels(&ids, None))
                        .unwrap_or(self.channel_count)
                };
                self.cat_entries
                    .push(("*".into(), "Toutes".into(), all_active, all_count));
                for (id, name) in cats {
                    // `selected_group` holds the raw group name (the id), not its label.
                    let active = self.selected_group.as_deref() == Some(id.as_str());
                    self.cat_entries.push((id, name, active, 0));
                }
            }
            Tab::Vod => {
                let cats: Vec<(String, String)> = self
                    .filtered_categories(ContentKind::Vod)
                    .into_iter()
                    .take(CAT_PAGE)
                    .map(|c| (c.id.clone(), crate::names::category_label(&c.name)))
                    .collect();
                let all_active = matches!(self.selected_vod_category.as_deref(), None | Some("*"));
                let all_count = if self.lang_filter_on() {
                    0
                } else {
                    let ids = self.enabled_source_ids();
                    self.catalog_db
                        .as_ref()
                        .map(|db| db.count_vod(&ids, None))
                        .unwrap_or(self.vod_count)
                };
                self.cat_entries
                    .push(("*".into(), "Toutes".into(), all_active, all_count));
                for (id, name) in cats {
                    let active = self.selected_vod_category.as_deref() == Some(id.as_str());
                    self.cat_entries.push((id, name, active, 0));
                }
            }
            Tab::Series => {
                let cats: Vec<(String, String)> = self
                    .filtered_categories(ContentKind::Series)
                    .into_iter()
                    .take(CAT_PAGE)
                    .map(|c| (c.id.clone(), crate::names::category_label(&c.name)))
                    .collect();
                let all_active =
                    matches!(self.selected_series_category.as_deref(), None | Some("*"));
                let all_count = if self.lang_filter_on() {
                    0
                } else {
                    let ids = self.enabled_source_ids();
                    self.catalog_db
                        .as_ref()
                        .map(|db| db.count_series(&ids, None))
                        .unwrap_or(self.series_count)
                };
                self.cat_entries
                    .push(("*".into(), "Toutes".into(), all_active, all_count));
                for (id, name) in cats {
                    let active = self.selected_series_category.as_deref() == Some(id.as_str());
                    self.cat_entries.push((id, name, active, 0));
                }
            }
            _ => {}
        }
        let cs = browser::virtual_slice(
            0.0,
            self.cat_view_h,
            browser::cat_row_height(),
            self.cat_entries.len(),
            self.virtual_overscan(),
        );
        self.cat_slice = (cs.start, cs.end);
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
                let cols = m.cols.max(1);
                let rows = self.mosaic_display_len().div_ceil(cols);
                let s = browser::virtual_slice(
                    scroll_y,
                    view_h,
                    browser::mosaic_row_height_for(m.tile_w, m.touch_target),
                    rows,
                    overscan,
                );
                (s.start, s.end)
            }
            Tab::Series if self.series_detail.is_none() => {
                let cols = m.cols.max(1);
                let rows = self.mosaic_display_len().div_ceil(cols);
                let s = browser::virtual_slice(
                    scroll_y,
                    view_h,
                    browser::mosaic_row_height_for(m.tile_w, m.touch_target),
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
            Tab::Live | Tab::Favorites => {
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
    fn onscreen_browse_range(&self, cols: usize, tile_w: f32) -> std::ops::Range<usize> {
        let cols = cols.max(1);
        let len = self.mosaic_display_len();
        let touch = self.layout_metrics().touch_target;
        let (row_h, per_row) = match self.tab {
            Tab::Vod | Tab::Series => (browser::mosaic_row_height_for(tile_w, touch), cols),
            _ => (browser::list_row_height_for(0.0, touch), 1),
        };
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
    fn art_prefetch_window(&self, cols: usize, tile_w: f32) -> std::ops::Range<usize> {
        let cols = cols.max(1);
        let touch = self.layout_metrics().touch_target;
        let row_h = match self.tab {
            Tab::Vod | Tab::Series if self.series_detail.is_none() && self.vod_detail.is_none() => {
                browser::mosaic_row_height_for(tile_w, touch)
            }
            _ => browser::list_row_height_for(0.0, touch),
        };
        // Lead by ~0.35s of fling distance, clamped to 2–16 rows.
        let lead_rows = ((self.browse_scroll_vy.abs() * 0.35) / row_h.max(1.0))
            .round()
            .clamp(2.0, 16.0) as usize;
        let ahead = self.browse_scroll_vy > 40.0;
        let behind = self.browse_scroll_vy < -40.0;

        match self.tab {
            Tab::Vod | Tab::Series if self.series_detail.is_none() && self.vod_detail.is_none() => {
                let len = self.mosaic_display_len();
                let rows = len.div_ceil(cols);
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
                let end_i = (end.saturating_mul(cols)).min(len);
                start_i..end_i
            }
            Tab::Live | Tab::Favorites => {
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

    /// Drop session UI maps that ratchet across browsing (translations, failed downloads…).
    fn trim_ui_session_caches(&mut self) {
        const TRANSLATIONS_SOFT: usize = 256;
        if self.translations.len() > TRANSLATIONS_SOFT {
            self.translations.clear();
            self.translating.clear();
        }
        self.trim_download_session_map();
        // Favorites / recent are user data — hard-cap pathological imports only.
        const FAVORITES_CAP: usize = 2_000;
        if self.settings.favorites.len() > FAVORITES_CAP {
            self.settings.favorites.truncate(FAVORITES_CAP);
        }
        crate::images::trim_global_aux_caches();
    }

    /// Keep only active / queued download rows — Done/Failed UI rows pile up forever otherwise.
    fn trim_download_session_map(&mut self) {
        const SESSION_CAP: usize = 48;
        if self.downloads.len() <= SESSION_CAP {
            return;
        }
        let drop_keys: Vec<String> = self
            .downloads
            .iter()
            .filter_map(|(k, st)| match st {
                crate::downloads::DownloadState::Done { .. }
                | crate::downloads::DownloadState::Failed => Some(k.clone()),
                _ => None,
            })
            .collect();
        // Leave enough finished rows for the panel; drop the oldest surplus.
        let surplus = self.downloads.len().saturating_sub(SESSION_CAP);
        for key in drop_keys.into_iter().take(surplus) {
            self.downloads.remove(&key);
        }
    }

    fn flush_pending_images(&mut self) -> Task<Message> {
        if self.pending_images.is_empty() {
            return Task::none();
        }
        let batch = std::mem::take(&mut self.pending_images);
        // Warm-ahead batches are newer than what is on screen: bump the visible art
        // first so LRU eviction drops off-screen posters, not the ones being shown
        // (they were never re-requested and stayed as letter placeholders).
        self.promote_visible_art();
        let wanted = self.wanted_art_urls();
        for (url, source_id, bytes) in batch {
            if !wanted.is_empty() && !wanted.contains(&url) {
                continue;
            }
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
        // After inserting, drop handles that left the current band (category residue).
        if !wanted.is_empty() {
            self.images.retain_urls(wanted.iter().cloned());
        }
        debug_assert!(
            self.images.handle_count() <= crate::images::MAX_RAM_HANDLES,
            "ImageCache leaked past MAX_RAM_HANDLES after flush"
        );
        if let Some(db) = &self.catalog_db {
            db.evict_old_images(4_000);
            db.evict_old_meta(30 * 24 * 60 * 60);
        }
        Task::none()
    }

    /// URLs for the painted window only — used to reject stale inserts.
    fn wanted_art_urls(&self) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        if self.vod_detail.is_some() || self.series_detail.is_some() {
            return out;
        }
        let m = self.layout_metrics();
        let win = self.visible_browse_window(m.cols, m.tile_w);
        let len = self.mosaic_display_len();
        for pos in win.start..win.end.min(len) {
            if let Some(url) = self.art_url_for_browse_pos(pos) {
                out.insert(url);
            }
        }
        out
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
        let mut tasks = Vec::new();
        let room = self.images.max_inflight
            .saturating_sub(self.images.inflight_len())
            .min(max);
        if room == 0 {
            tracing::debug!(
                target: "fluxplay::images",
                inflight = self.images.inflight_len(),
                pending = self.pending_images.len(),
                "prefetch skipped — no inflight room"
            );
            return Task::none();
        }
        for url in urls {
            if tasks.len() >= room {
                break;
            }
            if let crate::images::RequestOutcome::Fetch { url: u, source_id } =
                self.images.request(&url, None)
            {
                let gen = self.art_fetch_gen;
                let edge = self.images.decode_edge_px;
                tasks.push(Task::perform(
                    crate::images::fetch_image_prepared_edged(u, source_id, edge),
                    move |r| Message::ImageLoaded(gen, r),
                ));
            }
        }
        if tasks.is_empty() {
            Task::none()
        } else {
            let n = tasks.len() as u64;
            let (d, t) = self.jobs.images.snapshot();
            self.jobs.images.set_total(t.saturating_add(n).max(d + n));
            Task::batch(tasks)
        }
    }

    /// Prefetch posters for the current browse selection at profile load/reload.
    /// Visible mosaic only — no off-screen warm (RAM ceiling).
    fn prefetch_catalog_art(&mut self) -> Task<Message> {
        self.art_warm_cursor = 0;
        self.art_warm_active = false;
        self.refresh_browse_art()
    }

    /// Visible mosaic only — used after background meta/xtream batches so they
    /// cannot restart a full art warm (and re-inflate the iced atlas).
    fn light_art_refresh_if_browsing(&mut self) -> Task<Message> {
        if self.vod_detail.is_some() || self.series_detail.is_some() {
            return Task::none();
        }
        match self.tab {
            Tab::Vod | Tab::Series | Tab::Live | Tab::Favorites => self.prefetch_visible_art(),
            _ => Task::none(),
        }
    }

    /// Visible window only (off-screen warm disabled — mosaïque paginée).
    fn refresh_browse_art(&mut self) -> Task<Message> {
        self.art_warm_active = false;
        self.prefetch_visible_art()
    }

    /// Soft-stage GPU refresh window — unlocked mosaic page only.
    fn art_warm_target(&self) -> usize {
        self.mosaic_display_len()
            .min(crate::images::MAX_RAM_HANDLES.saturating_sub(16))
    }

    fn art_url_for_browse_pos(&self, pos: usize) -> Option<String> {
        if let Some(tile) = self.tile_cache.peek(pos) {
            return tile.art_url.clone().filter(|u| !u.is_empty());
        }
        None
    }

    fn art_url_source_at(&self, pos: usize) -> Option<Uuid> {
        self.tile_cache.peek(pos).and_then(|t| t.source_id)
    }

    /// After soft video, shared atlas UVs may point at stale pixels — new Handle ids force re-upload.
    fn refresh_browse_poster_gpu_textures(&mut self) {
        if self.vod_detail.is_some() || self.series_detail.is_some() {
            return;
        }
        match self.tab {
            Tab::Vod | Tab::Series => {}
            _ => return,
        }
        let target = self.art_warm_target().min(self.browse_index.len());
        for i in 0..target {
            if let Some(url) = self.art_url_for_browse_pos(i) {
                let sid = self.art_url_source_at(i);
                self.images.refresh_gpu_texture(&url, sid);
            }
        }
    }

    fn prefetch_visible_art(&mut self) -> Task<Message> {
        let mut urls = Vec::new();
        let m = self.layout_metrics();
        let win = self.visible_browse_window(m.cols, m.tile_w);
        let in_grid = match self.tab {
            Tab::Vod => self.vod_detail.is_none(),
            Tab::Series => self.series_detail.is_none(),
            Tab::Live | Tab::Favorites => true,
            _ => false,
        };
        if in_grid {
            // On-screen tiles first, then the overscan/lookahead band; the request
            // cap covers at least a full screen. Walking the band top-down spent the
            // whole budget on rows above the viewport, so visible tiles never loaded.
            let screen = self.onscreen_browse_range(m.cols, m.tile_w);
            let order = screen
                .clone()
                .chain(win.start..screen.start.max(win.start))
                .chain(screen.end.min(win.end)..win.end);
            for pos in order {
                if let Some(u) = self.art_url_for_browse_pos(pos) {
                    self.images.promote(&u);
                    urls.push(u);
                }
            }
            let cap = self
                .display_caps
                .tuning
                .image_prefetch
                .max(screen.len());
            return self.prefetch_urls_capped(urls, cap);
        }
        match self.tab {
            Tab::Live | Tab::Favorites | Tab::Vod | Tab::Series => {
                if let Some(d) = &self.vod_detail {
                    if let Some(u) = crate::images::vod_poster_url(d) {
                        urls.push(u);
                    }
                } else if let Some(d) = &self.series_detail {
                    if let Some(u) = crate::images::series_cover_url(d) {
                        urls.push(u);
                    }
                } else {
                    for pos in win.start..win.end.min(self.browse_index.len()) {
                        if let Some(u) = self.art_url_for_browse_pos(pos) {
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
        let chans: Vec<(String, Option<Uuid>)> = self
            .catalog_db
            .as_ref()
            .map(|db| {
                db.channels_in_group(
                    &self.enabled_source_ids(),
                    self.selected_group.as_deref(),
                    None,
                    12,
                )
                .into_iter()
                .map(|c| (c.id.clone(), c.source_id))
                .collect()
            })
            .unwrap_or_default();
        self.fetch_epg_for(chans)
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
        let ids = self.enabled_source_ids();
        let db = self.catalog_db.as_ref()?;
        let group_sql = match group {
            None | Some("Tous") | Some("*") => None,
            Some(g) => Some(g),
        };
        for tile in db.browse_tiles_channels(&ids, 0, 64, group_sql) {
            if !is_adult_cat(tile.group.as_deref().unwrap_or("")) {
                if let Some(ch) = db.channel_by_id(&tile.id) {
                    return Some(ch);
                }
            }
        }
        db.browse_tiles_channels(&ids, 0, 1, group_sql)
            .first()
            .and_then(|t| db.channel_by_id(&t.id))
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
        if let Some(sid) = self
            .vod_detail
            .as_ref()
            .filter(|v| v.stream_url == url)
            .and_then(|v| v.source_id)
            .or_else(|| {
                self.session
                    .channel
                    .as_ref()
                    .filter(|c| c.stream_url == url)
                    .and_then(|c| c.source_id)
            })
            .or_else(|| self.tile_cache.peek(self.browse_focus).and_then(|t| t.source_id))
        {
            if let Some(s) = enabled().find(|s| s.id == sid) {
                return Some(s.clone());
            }
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

    /// Open `ch` on the fastest server of its profile; when that server
    /// refuses, the next one (profiles with mirrors only).
    fn open_with_failover(&mut self, mut ch: Channel) -> fluxplay_player::Result<()> {
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

    /// Stable iced scrollable ids — never encode category/group in the id.
    /// Per-category ids remounted hundreds of scroll States / operation targets
    /// across browsing and ratcheted UI RAM. Offset reset uses `scroll_to(0)`.
    fn browse_scroll_widget_id(&self) -> String {
        match self.tab {
            Tab::Vod => "flux-vod".into(),
            Tab::Series if self.series_detail.is_some() => "flux-episodes".into(),
            Tab::Series => "flux-series".into(),
            Tab::Live => "flux-live".into(),
            Tab::Favorites => "flux-browse-fav".into(),
            _ => "flux-browse".into(),
        }
    }

    fn browse_virtual_content_h(&self) -> f32 {
        let m = self.layout_metrics();
        let view_h = self.browse_view_h.max(1.0);
        match self.tab {
            Tab::Vod if self.vod_detail.is_none() => {
                let cols = m.cols.max(1);
                let rows = self.mosaic_display_len().div_ceil(cols);
                browser::virtual_content_height(rows, view_h, browser::mosaic_row_height_for(m.tile_w, m.touch_target))
            }
            Tab::Series if self.series_detail.is_none() => {
                let cols = m.cols.max(1);
                let rows = self.mosaic_display_len().div_ceil(cols);
                browser::virtual_content_height(rows, view_h, browser::mosaic_row_height_for(m.tile_w, m.touch_target))
            }
            Tab::Series if self.series_detail.is_some() => browser::virtual_content_height(
                self.episode_flat.len(),
                view_h,
                browser::list_row_height(m.thumb),
            ),
            Tab::Live | Tab::Favorites => browser::virtual_content_height(
                self.browse_index.len(),
                view_h,
                browser::list_row_height(0.0),
            ),
            _ => view_h * 4.0,
        }
    }

    fn snap_browse_scroll_task(&self) -> Task<Message> {
        let id = self.browse_scroll_widget_id();
        if matches!(self.tab, Tab::Settings | Tab::Sources | Tab::Epg) {
            return Task::none();
        }
        iced::widget::operation::scroll_to(
            iced::widget::Id::from(id),
            AbsoluteOffset {
                x: None,
                y: Some(0.0),
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
        // Reuse open handle when present (Android CatalogBootReady already opened it).
        let ids: Vec<Uuid> = self.sources.iter().filter(|s| s.enabled).map(|s| s.id).collect();
        if self.catalog_db.is_none() {
            self.catalog_db = crate::catalog_db::CatalogDb::open(&ids);
        }
        self.loading = true;
        self.status = format!(
            "Lecture SQLite… · {} profil(s)",
            ids.len().max(1)
        );
        Task::perform(
            async move {
                crate::async_jobs::run_blocking(move || {
                    crate::catalog_db::load_spine_blocking(&ids)
                })
                .await
                .unwrap_or_else(Err)
            },
            Message::BundleCacheReady,
        )
    }

    fn apply_spine_cache(&mut self, spine: crate::catalog_db::CatalogSpine) {
        self.bundle.categories = spine.categories;
        self.bundle.epg = spine.epg;
        self.bundle.vod.clear();
        self.bundle.series.clear();
        self.bundle.channels.clear();
        self.channel_count = spine.channel_count;
        self.vod_count = spine.vod_count;
        self.series_count = spine.series_count;
        self.tile_cache.clear();
        self.art_fetch_gen = self.art_fetch_gen.wrapping_add(1);
        self.pending_images.clear();
        self.images.clear_handles();
        self.jobs.images.reset(0);
        tracing::info!(
            channels = self.channel_count,
            vod = self.vod_count,
            series = self.series_count,
            "rebuilt spine from catalog db"
        );
    }

    fn enabled_source_ids(&self) -> Vec<Uuid> {
        self.sources.iter().filter(|s| s.enabled).map(|s| s.id).collect()
    }

    fn vod_cat_sql(&self) -> Option<&str> {
        match self.selected_vod_category.as_deref() {
            None | Some("*") => None,
            Some(id) => Some(id),
        }
    }

    fn series_cat_sql(&self) -> Option<&str> {
        match self.selected_series_category.as_deref() {
            None | Some("*") => None,
            Some(id) => Some(id),
        }
    }

    fn live_group_sql(&self) -> Option<&str> {
        match self.selected_group.as_deref() {
            None | Some("Tous") | Some("*") => None,
            Some(g) => Some(g),
        }
    }

    fn arm_series_queue_for_url(&mut self, current_url: &str) {
        let Some(detail) = &self.series_detail else {
            // Detail page closed (next/previous from the player): keep the queue.
            match self.series_queue.iter().position(|(_, u)| u == current_url) {
                Some(i) => self.series_queue_idx = i,
                None => self.series_queue.clear(),
            }
            return;
        };
        self.series_queue.clear();
        self.series_queue_idx = 0;
        for season in &detail.seasons {
            for ep in &season.episodes {
                self.series_queue.push((
                    format!("{} — {}", detail.name, ep.title),
                    ep.stream_url.clone(),
                ));
            }
        }
        match self.series_queue.iter().position(|(_, u)| u == current_url) {
            Some(i) => self.series_queue_idx = i,
            // Not an episode of the open series: "next" would jump to its episode 2.
            None => self.series_queue.clear(),
        }
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
            self.view_browser()
        }
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
            Tab::Live => self.view_browse_live(ui),
            Tab::Vod => self.view_browse_vod(ui),
            Tab::Series => self.view_browse_series(ui),
            Tab::Favorites => self.view_favorites(ui),
            Tab::Epg => self.view_epg(ui),
            Tab::Sources => self.view_sources(ui),
            Tab::Settings => self.view_settings(ui),
        };

        let job_sfx = self.jobs.status_suffix_fr();
        let status_text = if let Some(prof) = fluxplay_core::overlay_status_line() {
            if self.loading {
                format!("Chargement… · {}{} · {prof}", self.status, job_sfx)
            } else {
                format!("{}{} · {prof}", self.status, job_sfx)
            }
        } else if self.loading {
            format!("Chargement… · {}{}", self.status, job_sfx)
        } else {
            format!("{}{}", self.status, job_sfx)
        };
        let status_bar = container(
            text(status_text)
            .size((m.body_size - 1.0).max(11.0))
            .color(ui.ink_muted()),
        )
        .padding(Padding::from([10, 16]))
        .width(Fill)
        .clip(true)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.surface_container_low())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: RADIUS_LARGE.into(),
            },
            ..Default::default()
        });

        let main_col: Element<'_, Message> = if m.top_nav || m.rail_w <= 1.0 {
            // Compact: content → status → nav (nav last, above system gesture bar).
            column![
                container(body)
                    .width(Fill)
                    .height(Fill)
                    .align_x(Alignment::Start)
                    .align_y(Alignment::Start)
                    .clip(true),
                status_bar,
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
            // Medium+: rail + content; status under the content column.
            column![
                row![
                    browser::mode_rail(ui, m.rail_w, m.rail_size, tab_items),
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
                .align_y(Alignment::Start),
                status_bar,
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill)
            .into()
        };

        // Android: dynamic WindowInsets (gesture nav may report 0 — do not invent 48dp).
        #[cfg(target_os = "android")]
        let (top_inset, bottom_inset) = (self.system_insets.1, self.system_insets.3);
        #[cfg(not(target_os = "android"))]
        let (top_inset, bottom_inset) = (0.0_f32, 0.0_f32);
        container(main_col)
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
            })
            .into()
    }

    fn view_player_window(&self) -> Element<'_, Message> {
        let ui = self.ui_theme();
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

        player_ui::player_window(player_ui::PlayerChrome {
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
            #[cfg(not(target_os = "android"))]
            chrome_h: self.layout_metrics().player_chrome_h,
            chrome_visible: self.player_chrome_visible,
            chrome_alpha: self.chrome_alpha,
            fullscreen: self.player_fullscreen,
            embedded_video,
            surface_video,
            stage_picture: self.stage_picture,
            backend_label,
            caps,
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
        })
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
                    2500
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
                let db = self.catalog_db.as_ref()?;
                let ids = self.enabled_source_ids();
                let ch = db
                    .live_channel_neighbor(&ids, self.selected_group.as_deref(), &cur.id, cur.source_id, delta)
                    .or_else(|| {
                        db.live_channel_neighbor(
                            &ids,
                            cur.group.as_deref(),
                            &cur.id,
                            cur.source_id,
                            delta,
                        )
                    })?;
                Some(Message::PlayChannel(ch))
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
        let mut ranked: Vec<(u8, usize, &fluxplay_core::models::Category)> = self
            .bundle
            .categories
            .iter()
            .filter(|c| c.content == kind)
            .filter(|c| {
                q.is_empty()
                    || c.name.to_lowercase().contains(&q)
                    || crate::names::category_label(&c.name).to_lowercase().contains(&q)
            })
            .enumerate()
            .filter_map(|(i, c)| {
                let lang = crate::names::parse_category(&c.name).lang;
                if only && !lang.matches(pref) {
                    return None;
                }
                Some((crate::names::lang_rank(&lang, pref), i, c))
            })
            .collect();
        // Viewer language first, multi-language next, adult last; portal order inside.
        ranked.sort_by_key(|(rank, i, _)| (*rank, *i));
        ranked.into_iter().map(|(_, _, c)| c).collect()
    }

    fn lang_filter_on(&self) -> bool {
        self.settings.only_pref_lang && !self.settings.pref_lang.is_empty()
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

    fn view_browse_live(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        let list_w = m.content_w.max(120.0);
        let total = self.browse_index.len();
        let row_h = browser::list_row_height(0.0);
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            total,
            self.browse_overscan(),
        );

        let mut rows: Vec<Element<'_, Message>> = Vec::with_capacity(slice.end - slice.start);
        if total == 0 {
            rows.push(browser::empty_hint(
                ui,
                "Aucune chaîne dans cette sélection.",
            ));
        } else {
            for pos in slice.start..slice.end.min(self.browse_index.len()) {
                let tile = self.tile_cache.peek(pos);
                let name = tile.map(|t| t.name.as_str()).unwrap_or("…");
                let id = tile.map(|t| t.id.as_str()).unwrap_or("");
                let sid = tile.and_then(|t| t.source_id);
                let fav = !id.is_empty() && self.settings.is_favorite(id);
                let subtitle = tile
                    .and_then(|t| t.group.as_deref())
                    .map(crate::names::category_label)
                    .unwrap_or_else(|| "Live".into());
                let thumb = tile
                    .and_then(|t| t.art_url.as_deref())
                    .and_then(|k| self.images.get(k));
                rows.push(browser::media_row(
                    if tile.is_some() {
                        crate::names::display_channel(name)
                    } else {
                        "…".into()
                    },
                    subtitle,
                    Message::PlayChannelId(id.to_string(), sid),
                    if id.is_empty() {
                        None
                    } else {
                        Some((fav, Message::ToggleFavorite(id.to_string())))
                    },
                    ui,
                    self.selected_channel.as_deref() == Some(id),
                    thumb,
                    40.0,
                    list_w,
                ));
            }
        }

        let items = if total == 0 {
            Column::with_children(rows).spacing(4).width(Fill)
        } else {
            // Overlay list (no spacer siblings in the same column as rows) —
            // GLES drops text when virtual spacers share the scrollable tree.
            #[cfg(target_os = "android")]
            {
                Column::with_children(rows).spacing(4).width(Fill)
            }
            #[cfg(not(target_os = "android"))]
            {
                browser::virtual_column(slice, rows)
            }
        };

        let header = browser::content_header_with(
            ui,
            self.selected_group
                .as_deref()
                .filter(|g| !matches!(*g, "*" | "Tous"))
                .map(crate::names::category_label)
                .unwrap_or_else(|| "Toutes les chaînes".into()),
            format!("{total} chaînes"),
            &self.search,
            "Rechercher une chaîne…",
            m.search_w,
            m.title_size,
            m.stack_header,
        );

        let scroll_id = self.browse_scroll_widget_id();
        let content_h = browser::virtual_content_height(total, self.browse_view_h, row_h);
        let scroller = if total == 0 {
            browser::soft_scroll_on(
                ui,
                items.width(Length::Fixed(list_w)),
                scroll_id,
                Message::BrowseScrolled,
            )
        } else {
            #[cfg(target_os = "android")]
            {
                browser::soft_scroll_mosaic(
                    ui,
                    scroll_id,
                    content_h,
                    items.width(Length::Fixed(list_w)),
                    Message::BrowseScrolled,
                )
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = content_h;
                browser::soft_scroll_on(
                    ui,
                    items.width(Length::Fixed(list_w)),
                    scroll_id,
                    Message::BrowseScrolled,
                )
            }
        };
        let content = browser::pane(
            ui,
            Length::Fill,
            column![header, scroller]
                .spacing(m.gap)
                .width(Fill)
                .height(Fill),
        );

        self.with_categories(ui, m, "Chaînes", &self.cat_entries, content)
    }

    fn view_browse_vod(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        if let Some(detail) = &self.vod_detail {
            return self.view_vod_detail(ui, m, detail);
        }
        let cols = m.cols.max(1);
        let tile_w = m.tile_w;

        let total = self.browse_index.len();
        let shown = self.mosaic_display_len();
        let n_rows = shown.div_ceil(cols);
        let row_h = browser::mosaic_row_height_for(tile_w, m.touch_target);
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
        if total == 0 {
            mosaic = mosaic.push(browser::empty_hint(
                ui,
                "Aucun film — sync en cours ou changez de catégorie.",
            ));
        } else {
            for row_i in slice.start..slice.end {
                let mut r = Row::new().spacing(crate::theme::MOSAIC_GAP).width(Fill);
                for c in 0..cols {
                    let idx = row_i * cols + c;
                    if idx >= shown {
                        break;
                    }
                    let tile = self.tile_cache.peek(idx);
                    let name = tile.map(|t| t.name.as_str()).unwrap_or("…");
                    let ti = crate::names::parse_item_title(name);
                    let meta = tile
                        .map(|t| {
                            t.meta_line("Film")
                        })
                        .unwrap_or_else(|| "…".into());
                    let id = tile.map(|t| t.id.as_str()).unwrap_or("");
                    let thumb = tile
                        .and_then(|t| t.art_url.as_deref())
                        .and_then(|u| self.images.get(u));
                    r = r.push(browser::mosaic_tile(
                        if tile.is_some() {
                            ti.title
                        } else {
                            "…".into()
                        },
                        meta,
                        id.to_string(),
                        Message::OpenVodDetail(id.to_string()),
                        ui,
                        tile_w,
                        thumb,
                        self.pressed_mosaic.as_deref() == Some(id),
                        m.focus_chrome && idx == self.browse_focus,
                    ));
                }
                r = r.push(Space::new().width(Fill));
                mosaic = mosaic.push(r.height(Length::Fixed(row_h)));
            }
        }

        let selected = self.selected_vod_category.as_deref();
        let cat_name = if matches!(selected, None | Some("*")) {
            "Tous les films".into()
        } else {
            self.bundle
                .categories
                .iter()
                .find(|c| c.content == ContentKind::Vod && Some(c.id.as_str()) == selected)
                .map(|c| crate::names::category_label(&c.name))
                .unwrap_or_else(|| "Films".into())
        };

        let header = browser::content_header_with(
            ui,
            cat_name,
            format!("{shown}/{total} films"),
            &self.search,
            "Rechercher un film…",
            m.search_w,
            m.title_size,
            m.stack_header,
        );
        let scroll_id = self.browse_scroll_widget_id();
        let content_h =
            browser::virtual_content_height(n_rows, self.browse_view_h, row_h);
        let scroller = if total == 0 {
            browser::soft_scroll_on(ui, mosaic.width(Fill), scroll_id, Message::BrowseScrolled)
        } else {
            browser::soft_scroll_mosaic(
                ui,
                scroll_id,
                content_h,
                mosaic.width(Fill).height(Fill),
                Message::BrowseScrolled,
            )
        };
        let mut body = column![header, scroller].spacing(m.gap).width(Fill).height(Fill);
        if total > 0 {
            body = body.push(browser::mosaic_show_more(
                ui,
                shown,
                total,
                browser::MOSAIC_PAGE_ITEMS,
            ));
        }
        let content = browser::pane(ui, Length::Fill, body);
        self.with_categories(ui, m, "Films / VOD", &self.cat_entries, content)
    }

    /// Current download root (settings or platform default).
    fn downloads_dir(&self) -> std::path::PathBuf {
        crate::storage::downloads_dir(&self.settings.download_dir)
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
            dir: self.downloads_dir(),
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
                    if let Some(ep) = s.episodes.iter().find(|ep| ep.stream_url == url) {
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
        let running = matches!(
            self.downloads.get(url),
            Some(crate::downloads::DownloadState::Running { .. })
        );
        if running {
            return None;
        }
        let dir = self.downloads_dir();
        let saved_as = self.download_name_for(url, name, kind);
        let file = crate::downloads::find_by_name(&dir, &saved_as).or_else(|| {
            (kind != ContentKind::Series)
                .then(|| crate::names::display_title(&saved_as))
                .filter(|clean| *clean != saved_as)
                .and_then(|clean| crate::downloads::find_by_name(&dir, &clean))
        })?;
        self.download_library.insert(url, file.clone());
        Some(file)
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
                crate::storage::display_path(&self.downloads_dir())
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
        let poster = crate::images::vod_poster_url(detail).and_then(|u| self.images.get(&u));
        let imdb_query = detail
            .imdb_id
            .clone()
            .unwrap_or_else(|| crate::names::display_title(&detail.name));
        let t = self.detail_text(
            &detail.name,
            detail.year.as_deref(),
            detail.genre.as_deref(),
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
        )
    }

    fn view_series_detail(
        &self,
        ui: UiTheme,
        m: crate::theme::LayoutMetrics,
        detail: &SeriesItem,
    ) -> Element<'_, Message> {
        let poster = crate::images::series_cover_url(detail).and_then(|u| self.images.get(&u));
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
                            crate::storage::display_path(&self.downloads_dir()),
                            crate::downloads::MAX_PARALLEL
                        );
                        (label, msg, hint)
                    },
                )
            });
        let (play_label, play_msg, download) = match first_play {
            Some((l, m, d)) => (Some(l), Some(m), Some(d)),
            None => (None, None, None),
        };

        let mut eps_rows: Vec<Element<'_, Message>> = Vec::new();
        let total_eps = self.episode_flat.len();
        let row_h = browser::list_row_height(m.thumb);
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            total_eps,
            self.virtual_overscan(),
        );
        for row in &self.episode_flat[slice.start..slice.end] {
            match row {
                EpisodeFlat::Header(n) => {
                    let (label, msg) = self.season_download_action(detail, Some(*n), true);
                    eps_rows.push(
                        row![
                            text(format!("Saison {n}")).size(14),
                            Space::new().width(Fill),
                            browser::download_pill(ui, label, msg, true),
                        ]
                        .align_y(Alignment::Center)
                        .width(Length::Fixed(m.content_w.max(252.0)))
                        .into(),
                    );
                }
                EpisodeFlat::Ep { season_idx, ep_idx } => {
                    let Some(season) = detail.seasons.get(*season_idx) else {
                        continue;
                    };
                    let Some(ep) = season.episodes.get(*ep_idx) else {
                        continue;
                    };
                    let ep_title =
                        crate::names::episode_display(&detail.name, &ep.title, ep.episode_num);
                    let sub = match crate::metadata::fmt_plot(ep.plot.as_deref(), &ep.title) {
                        Some(p) => {
                            let p = self.tr(&p).replace('\n', " ");
                            let mut it = p.chars();
                            let short: String = it.by_ref().take(90).collect();
                            if it.next().is_some() {
                                format!("E{} · {short}…", ep.episode_num)
                            } else {
                                format!("E{} · {short}", ep.episode_num)
                            }
                        }
                        None => match crate::metadata::fmt_runtime(ep.runtime.as_deref()) {
                            Some(rt) => format!("Épisode {} · {rt}", ep.episode_num),
                            None => format!("Épisode {}", ep.episode_num),
                        },
                    };
                    const DOWNLOAD_W: f32 = 132.0;
                    let row_w = m.content_w.max(120.0 + DOWNLOAD_W);
                    let (dl_label, dl_msg) = self.download_action(
                        crate::downloads::episode_title(
                            &detail.name,
                            season.season_number,
                            ep.episode_num,
                            &ep.title,
                        ),
                        &ep.stream_url,
                        true,
                    );
                    eps_rows.push(
                        row![
                            browser::media_row(
                                ep_title,
                                sub,
                                Message::PlayVod {
                                    name: format!("{} — {}", detail.name, ep.title),
                                    url: ep.stream_url.clone(),
                                    kind: ContentKind::Series,
                                    poster: detail.cover.clone().or(detail.banner.clone()),
                                },
                                None,
                                ui,
                                false,
                                poster,
                                m.thumb,
                                row_w - DOWNLOAD_W,
                            ),
                            container(browser::download_pill(ui, dl_label, dl_msg, true))
                                .width(Length::Fixed(DOWNLOAD_W))
                                .center_x(Length::Fixed(DOWNLOAD_W)),
                        ]
                        .align_y(Alignment::Center)
                        .into(),
                    );
                }
            }
        }
        let episodes = if detail.seasons.is_empty() {
            None
        } else {
            // Always mount virtual spacers so scroll height matches the full episode
            // list (Android previously omitted spacers → blank / truncated pane).
            Some(browser::virtual_column(slice, eps_rows).into())
        };

        let imdb_query = detail
            .imdb_id
            .clone()
            .unwrap_or_else(|| crate::names::display_title(&detail.name));
        let t = self.detail_text(
            &detail.name,
            detail.year.as_deref(),
            detail.genre.as_deref(),
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
            play_label,
            play_msg,
            download,
            Message::CloseSeriesDetail,
            episodes,
            m.bp.is_narrow(),
            self.detail_meta_loading,
            t.facts,
            t.plot_note,
        )
    }

    fn view_browse_series(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        let cols = m.cols.max(1);
        let tile_w = m.tile_w;
        if let Some(detail) = &self.series_detail {
            return self.view_series_detail(ui, m, detail);
        }

        let total = self.browse_index.len();
        let shown = self.mosaic_display_len();
        let n_rows = shown.div_ceil(cols);
        let row_h = browser::mosaic_row_height_for(tile_w, m.touch_target);
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            n_rows,
            0,
        );

        let mut mosaic = Column::new().spacing(0).width(Fill);
        if total == 0 {
            mosaic = mosaic.push(browser::empty_hint(
                ui,
                "Aucune série — sync en cours ou changez de catégorie.",
            ));
        } else {
            for row_i in slice.start..slice.end {
                let mut r = Row::new().spacing(crate::theme::MOSAIC_GAP).width(Fill);
                for c in 0..cols {
                    let idx = row_i * cols + c;
                    if idx >= shown {
                        break;
                    }
                    let tile = self.tile_cache.peek(idx);
                    let name = tile.map(|t| t.name.as_str()).unwrap_or("…");
                    let ti = crate::names::parse_item_title(name);
                    let meta = tile
                        .map(|t| t.meta_line("Série"))
                        .unwrap_or_else(|| "…".into());
                    let id = tile.map(|t| t.id.as_str()).unwrap_or("");
                    let thumb = tile
                        .and_then(|t| t.art_url.as_deref())
                        .and_then(|u| self.images.get(u));
                    r = r.push(browser::mosaic_tile(
                        if tile.is_some() {
                            ti.title
                        } else {
                            "…".into()
                        },
                        meta,
                        id.to_string(),
                        Message::OpenSeries(id.to_string()),
                        ui,
                        tile_w,
                        thumb,
                        self.pressed_mosaic.as_deref() == Some(id),
                        m.focus_chrome && idx == self.browse_focus,
                    ));
                }
                r = r.push(Space::new().width(Fill));
                mosaic = mosaic.push(r.height(Length::Fixed(row_h)));
            }
        }

        let selected = self.selected_series_category.as_deref();
        let cat_name = if matches!(selected, None | Some("*")) {
            "Toutes les séries".into()
        } else {
            self.bundle
                .categories
                .iter()
                .find(|c| c.content == ContentKind::Series && Some(c.id.as_str()) == selected)
                .map(|c| crate::names::category_label(&c.name))
                .unwrap_or_else(|| "Séries".into())
        };
        let header = browser::content_header_with(
            ui,
            cat_name,
            format!("{shown}/{total} séries"),
            &self.search,
            "Rechercher une série…",
            m.search_w,
            m.title_size,
            m.stack_header,
        );
        let scroll_id = self.browse_scroll_widget_id();
        let content_h =
            browser::virtual_content_height(n_rows, self.browse_view_h, row_h);
        let scroller = if total == 0 {
            browser::soft_scroll_on(ui, mosaic.width(Fill), scroll_id, Message::BrowseScrolled)
        } else {
            browser::soft_scroll_mosaic(
                ui,
                scroll_id,
                content_h,
                mosaic.width(Fill).height(Fill),
                Message::BrowseScrolled,
            )
        };
        let mut body = column![header, scroller].spacing(m.gap).width(Fill).height(Fill);
        if total > 0 {
            body = body.push(browser::mosaic_show_more(
                ui,
                shown,
                total,
                browser::MOSAIC_PAGE_ITEMS,
            ));
        }
        let content = browser::pane(ui, Length::Fill, body);
        self.with_categories(ui, m, "Séries", &self.cat_entries, content)
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
            for pos in slice.start..slice.end.min(self.browse_index.len()) {
                let tile = self.tile_cache.peek(pos);
                let name = tile.map(|t| t.name.as_str()).unwrap_or("…");
                let id = tile.map(|t| t.id.as_str()).unwrap_or("");
                let sid = tile.and_then(|t| t.source_id);
                let thumb = tile
                    .and_then(|t| t.art_url.as_deref())
                    .and_then(|k| self.images.get(k));
                rows.push(browser::media_row(
                    if tile.is_some() {
                        crate::names::display_channel(name)
                    } else {
                        "…".into()
                    },
                    tile.and_then(|t| t.group.as_deref())
                        .map(crate::names::category_label)
                        .unwrap_or_else(|| "Live".into()),
                    Message::PlayChannelId(id.to_string(), sid),
                    if id.is_empty() {
                        None
                    } else {
                        Some((true, Message::ToggleFavorite(id.to_string())))
                    },
                    ui,
                    self.selected_channel.as_deref() == Some(id),
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
        let m = self.layout_metrics();
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

        let accent_mosaic = browser::accent_mosaic(ui, self.settings.accent, m.content_w);

        let section = |title: &'static str, hint: &'static str| {
            let (title_sz, title_font) = type_style(TypeRole::TitleM, true);
            column![
                text(title).size(title_sz).font(title_font).color(ui.ink()),
                text(hint).size(11).color(ui.ink_muted()),
            ]
            .spacing(2)
        };

        let body = column![
            browser::content_header(
                ui,
                "Réglages".into(),
                format!(
                    "{} · {}",
                    profile.platform.label(),
                    profile.ui_shell
                ),
                &self.search,
                m.search_w,
                m.title_size,
                m.stack_header,
            ),
            section(
                "1. Apparence",
                "Thème clair/sombre et couleur d’accent de l’interface.",
            ),
            row![
                pill_button(
                    text(format!("Thème : {}", self.settings.theme.label())),
                    Message::CycleTheme,
                    ui,
                    false,
                ),
                pill_button(
                    text(format!("Accent : {}", self.settings.accent.label())),
                    Message::CycleAccent,
                    ui,
                    false,
                ),
            ]
            .spacing(8)
            .wrap(),
            text(format!(
                "Palette d’accent · {} teintes (mosaïque)",
                AccentPreset::all().len()
            ))
            .size(12)
            .color(ui.ink_muted()),
            accent_mosaic,
            section(
                "2. Métadonnées IMDb (OMDb)",
                "API officielle OMDb → synopsis, acteurs, notes IMDb. Gratuit : https://www.omdbapi.com/apikey.aspx",
            ),
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
                    "OMDb inactif — collez une clé puis Enregistrer"
                })
                .size(12)
                .color(if crate::metadata::omdb_configured() {
                    ui.accent()
                } else {
                    ui.ink_muted()
                }),
                {
                    let check: Element<'_, Message> = if crate::metadata::omdb_configured() {
                        container(crate::icons::icon(
                            crate::icons::Icon::Check,
                            18.0,
                            ui.accent(),
                        ))
                        .into()
                    } else {
                        Space::new().width(0).into()
                    };
                    check
                },
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            section(
                "2b. Langue & traduction",
                "Votre langue : ses catégories passent en premier et les synopsis / genres des fiches y sont traduits (Google Traduction, sans clé ; texte envoyé à Google à l’ouverture d’une fiche, mis en cache).",
            ),
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
            pill_button(
                text(if self.settings.translate_meta {
                    "Traduire synopsis et genres : activé"
                } else {
                    "Traduire synopsis et genres : désactivé"
                }),
                Message::ToggleTranslateMeta,
                ui,
                self.settings.translate_meta && !self.settings.pref_lang.is_empty(),
            ),
            iced::widget::checkbox(self.settings.only_pref_lang)
                .label(
                    "Afficher uniquement les chaînes TV, les VOD et séries disponibles dans votre langue",
                )
                .on_toggle(|_| Message::ToggleOnlyPrefLang)
                .size(18)
                .text_size(13)
                .spacing(10),
            text(if self.settings.pref_lang.is_empty() {
                "Choisissez une langue pour activer la traduction et le filtre.".to_string()
            } else {
                format!(
                    "Langue détectée d’après les noms du portail (|FR|, FR - , _fr, VOSTFR, MULTI…). Contenus MULTI inclus ; langue inconnue masquée quand le filtre est actif. Traductions en cache : {}.",
                    self.translations.len()
                )
            })
            .size(11)
            .color(ui.ink_muted()),
            section(
                "3. Réseau, DNS & WireGuard",
                "Tunnel WireGuard userspace (SOCKS local, sans routes système) — même chemin desktop et Android. DNS= du profil = bootstrap Endpoint ; DNS app (Custom/DoH/DoT) tunnelisé quand le VPN est ON.",
            ),
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
                .color(ui.accent()),
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
                "Test : résout cloudflare.com via le mode choisi.".into()
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
                            "Tunnel app WireGuard : ON"
                        } else {
                            "Tunnel app WireGuard : démarrage…"
                        }
                    } else {
                        "Tunnel app WireGuard : OFF"
                    }),
                    Message::ToggleWireGuard,
                    ui,
                    self.settings.network.wireguard_enabled && crate::wg_tunnel::tunnel_is_up(),
                ),
                pill_button(
                    text("Importer .conf…"),
                    Message::PickWireGuardProfile,
                    ui,
                    false,
                ),
                pill_button(
                    text("Mémoriser DNS bootstrap"),
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
            section(
                "4. Moteur de lecture",
                "Choix du décodeur et accélération matérielle. Appliqué au prochain flux.",
            ),
            pill_button(
                text(format!(
                    "Backend lecture : {}",
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
                        "Décodage matériel (HW) : activé"
                    } else {
                        "Décodage matériel (HW) : désactivé"
                    }),
                    Message::ToggleHwdec,
                    ui,
                    self.settings.hwdec,
                ),
                pill_button(
                    text(if self.settings.low_latency {
                        "Faible latence live : activée"
                    } else {
                        "Faible latence live : désactivée"
                    }),
                    Message::ToggleLowLatency,
                    ui,
                    self.settings.low_latency,
                ),
            ]
            .spacing(8)
            .wrap(),
            section(
                "4b. Buffer lecture",
                "Taille du cache avant lecture. Plus grand = plus stable, démarrage un peu plus lent.",
            ),
            row![
                pill_button(
                    text(format!("Cache réseau : {} ms", self.settings.cache_ms)),
                    Message::CycleCache,
                    ui,
                    false,
                ),
                pill_button(
                    text(format!("Buffer demux : {:.0} s", self.settings.demux_secs)),
                    Message::CycleDemux,
                    ui,
                    false,
                ),
            ]
            .spacing(8)
            .wrap(),
            text(format!(
                "Volume par défaut : {:.0}%{}",
                self.settings.volume * 100.0,
                {
                    #[cfg(target_os = "android")]
                    {
                        String::new()
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        format!(
                            " · mémoriser position : {}",
                            if self.settings.remember_position {
                                "oui"
                            } else {
                                "non"
                            }
                        )
                    }
                }
            ))
            .size(12)
            .color(ui.ink_muted()),
            row![pill_button(
                text(if self.settings.remember_position {
                    "Reprise position : activée"
                } else {
                    "Reprise position : désactivée"
                }),
                Message::ToggleRememberPosition,
                ui,
                self.settings.remember_position
            )]
            .spacing(8)
            .wrap(),
            section(
                "5. Affichage & FPS",
                "Un seul GPU fait le décodage et l'affichage. Le changement d'écran demande un redémarrage.",
            ),
            row![pill_button(
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
            )]
            .spacing(8)
            .wrap(),
            text(self.display_caps.summary_line())
                .size(12)
                .color(ui.accent()),
            row![
                pill_button(
                    text(format!(
                        "FPS GUI : {} ({} fps)",
                        self.settings.fps_gui.label(),
                        self.display_caps.gui_hz
                    )),
                    Message::CycleFpsGui,
                    ui,
                    false,
                ),
                pill_button(
                    text(format!(
                        "FPS vidéo : {} ({} fps)",
                        self.settings.fps_video.label(),
                        self.display_caps.video_hz
                    )),
                    Message::CycleFpsVideo,
                    ui,
                    false,
                ),
                pill_button(
                    text("Rescan moniteur / GPU"),
                    Message::RefreshDisplayCaps,
                    ui,
                    false,
                ),
            ]
            .spacing(8)
            .wrap(),
            section(
                "5b. Qualité vidéo & couleur",
                "360p/480p/720p/1080p/4K, SDR/HDR/HDR+, LED/LCD vs AMOLED, present Android.",
            ),
            row![
                pill_button(
                    text(format!("Qualité : {}", self.settings.video_quality.label())),
                    Message::CycleVideoQuality,
                    ui,
                    false,
                ),
                pill_button(
                    text(format!("HDR / gamut : {}", self.settings.hdr_mode.label())),
                    Message::CycleHdrMode,
                    ui,
                    false,
                ),
                pill_button(
                    text(format!("Profil écran : {}", self.settings.display_panel.label())),
                    Message::CycleDisplayPanel,
                    ui,
                    false,
                ),
            ]
            .spacing(8)
            .wrap(),
            row![
                // Android-only presentation path: hidden on desktop.
                if cfg!(target_os = "android") {
                    pill_button(
                        text(format!(
                            "Présent Android : {}",
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
                        "Tonemap HDR→SDR : activé"
                    } else {
                        "Tonemap HDR→SDR : désactivé"
                    }),
                    Message::ToggleTonemapHdr,
                    ui,
                    self.settings.tonemap_hdr,
                ),
            ]
            .spacing(8)
            .wrap(),
            section(
                "5c. Défauts image du lecteur",
                "Appliqués automatiquement à chaque lecture (et modifiables en direct via ⋯ → Avancé).",
            ),
            row![
                pill_button(
                    text(format!("Format par défaut : {}", self.settings.aspect.label())),
                    Message::CycleDefaultAspect,
                    ui,
                    false,
                ),
                pill_button(
                    text(format!(
                        "Désentrelacement : {}",
                        self.settings.deinterlace.label()
                    )),
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
                        "Mode nuit défaut : ON"
                    } else {
                        "Mode nuit défaut : off"
                    }),
                    Message::ToggleDefaultNightMode,
                    ui,
                    self.settings.night_mode,
                ),
            ]
            .spacing(8)
            .wrap(),
            section(
                "6. Séries",
                "Précharge l’épisode suivant sur disque à ~75% de progression.",
            ),
            pill_button(
                text(if self.settings.prefetch_next_episode {
                    "Précharge épisode suivant : activée (seuil 75%)"
                } else {
                    "Précharge épisode suivant : désactivée"
                }),
                Message::TogglePrefetchNext,
                ui,
                self.settings.prefetch_next_episode,
            ),
            section(
                "6b. Téléchargements",
                "Films et épisodes téléchargés depuis leur fiche. Vide = dossier par défaut.",
            ),
            text(format!(
                "Dossier actuel : {}{}",
                self.downloads_dir().display(),
                if self.settings.download_dir.is_empty() {
                    " (par défaut)"
                } else {
                    ""
                }
            ))
            .size(12)
            .color(ui.accent()),
            field(
                "Dossier des téléchargements (vide = défaut)",
                &self.form_download_dir,
                Message::FormDownloadDir,
                None,
                ui,
            ),
            {
                let mut actions = row![pill_button_primary(
                    text("Enregistrer le dossier"),
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
                        text("Ouvrir le dossier"),
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
            section(
                "7. Backends détectés sur cette machine",
                "État des décodeurs disponibles (libmpv, mpv CLI, ffplay).",
            ),
            be_list,
            section(
                "7. Image pendant la lecture",
                "Désentrelacement, upscaling, format, zoom : panneau ⋯ → Avancé dans le lecteur.",
            ),
            text(
                "Astuce : FLUXPLAY_VERBOSE=1 cargo run — logs mpv/FFmpeg/iced. Ou RUST_LOG=fluxplay::ui=debug,fluxplay_player=debug.",
            )
            .size(11)
            .color(ui.ink_muted()),
        ]
        .spacing(12)
        .width(Fill);

        browser::pane(
            ui,
            Length::Fill,
            browser::soft_scroll(ui, body),
        )
    }

    fn view_epg(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        let now = chrono::Utc::now();
        let mut items = Column::new().spacing(4).width(Fill);
        let channels: Vec<Channel> = if let Some(id) = &self.selected_channel {
            self.catalog_db
                .as_ref()
                .and_then(|db| db.channel_by_id(id))
                .into_iter()
                .collect()
        } else {
            let q = self.search.trim();
            self.catalog_db
                .as_ref()
                .map(|db| {
                    db.channels_in_group(
                        &self.enabled_source_ids(),
                        self.selected_group.as_deref(),
                        if q.is_empty() { None } else { Some(q) },
                        40,
                    )
                })
                .unwrap_or_default()
        };

        if channels.is_empty() || self.bundle.epg.is_empty() {
            items = items.push(browser::empty_hint(
                ui,
                "Ouvrez Live, choisissez une catégorie, puis revenez ici — ou lancez une chaîne.",
            ));
        }

        for ch in &channels {
            let (cur, next) = self.bundle.now_next(ch, now);
            let cur_s = cur
                .map(|p| {
                    format!(
                        "▶ {} ({})",
                        p.title,
                        p.start.with_timezone(&Local).format("%H:%M")
                    )
                })
                .unwrap_or_else(|| "Pas de programme".into());
            let next_s = next
                .map(|p| {
                    format!(
                        "Ensuite {} ({})",
                        p.title,
                        p.start.with_timezone(&Local).format("%H:%M")
                    )
                })
                .unwrap_or_default();
            items = items.push(browser::media_row(
                crate::names::display_channel(&ch.name),
                format!("{cur_s}  {next_s}"),
                Message::PlayChannelId(ch.id.clone(), ch.source_id),
                None,
                ui,
                self.selected_channel.as_deref() == Some(ch.id.as_str()),
                crate::images::pick_art(
                    ch.logo.as_deref().or(ch.tvg_logo.as_deref()),
                    None,
                    None,
                    None,
                )
                .and_then(|u| self.images.get(&u)),
                // Same logo size as the Live list: larger thumbs overflowed the rows.
                40.0,
                m.content_w.max(120.0),
            ));
        }

        browser::pane(
            ui,
            Length::Fill,
            column![
                browser::content_header_with(
                    ui,
                    "Guide TV".into(),
                    format!("{} programmes en cache", self.bundle.epg.len()),
                    &self.search,
                    "Rechercher une chaîne…",
                    m.search_w,
                    m.title_size,
                    m.stack_header,
                ),
                browser::soft_scroll(ui, items.width(Fill)),
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill),
        )
    }

    fn view_sources(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
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
                    crate::icons::icon_label(crate::icons::Icon::Add, submit_label, 14.0, ui.on_primary()),
                    Message::AddSource,
                    ui,
                ),
                cancel_edit,
                pill_button(
                    crate::icons::icon_label(
                        crate::icons::Icon::FolderOpen,
                        "Fichier M3U…",
                        14.0,
                        ui.accent(),
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
                        ui.accent(),
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
        }

        let list_block = if self.sources.is_empty() {
            browser::empty_hint(ui, "Aucune source — ajoutez un portail ou une playlist.")
        } else {
            browser::soft_scroll_fit(ui, list.width(Fill))
        };

        browser::pane(
            ui,
            Length::Fill,
            column![
                browser::content_header(
                    ui,
                    "Sources".into(),
                    format!(
                        "{} enregistrée{}",
                        self.sources.len(),
                        if self.sources.len() == 1 { "" } else { "s" }
                    ),
                    &self.search,
                    m.search_w,
                    m.title_size,
                    m.stack_header,
                ),
                form,
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
                            ui.accent(),
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
            .spacing(m.gap)
            .width(Fill)
            .height(Fill),
        )
    }
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

// Off-thread index build; row snapshots come from SQLite, not RAM bundle.
fn build_browse_index_blocking(
    tab: Tab,
    q: String,
    group: Option<String>,
    vod_cat: Option<String>,
    series_cat: Option<String>,
    source_ids: Vec<Uuid>,
    lang: Option<std::sync::Arc<LangCtx>>,
    progress: std::sync::Arc<crate::async_jobs::JobProgress>,
) -> (
    BrowseIndex,
    Vec<EpisodeFlat>,
    Vec<crate::browse_tiles::BrowseTile>,
) {
    use crate::async_jobs::DEFAULT_CHUNK;
    let q_lc = if q.is_empty() {
        None
    } else {
        Some(q.to_lowercase())
    };
    let Some(db) = crate::catalog_db::CatalogDb::open(&source_ids) else {
        return (BrowseIndex::Empty, Vec::new(), Vec::new());
    };
    let group_sql = match group.as_deref() {
        None | Some("Tous") | Some("*") => None,
        Some(g) => Some(g),
    };
    let index = match tab {
        Tab::Live => {
            if !q.is_empty() {
                let found = db.search_channel_ids(&q, group_sql, 2_000);
                let tiles = db.slim_channels_by_ids(&source_ids, &found);
                progress.set_done(1);
                return (
                    BrowseIndex::identity(tiles.len()),
                    Vec::new(),
                    tiles,
                );
            }
            let live_rows = db.collect_live_browse_rows(&source_ids, group_sql);
            progress.set_total(live_rows.len() as u64);
            let mut out = Vec::with_capacity(live_rows.len().min(65_536));
            let mut idle = Vec::new();
            for chunk in live_rows.chunks(DEFAULT_CHUNK) {
                for (i, name, g) in chunk {
                    let q_ok = q_lc
                        .as_deref()
                        .map(|ql| name.to_lowercase().contains(ql))
                        .unwrap_or(true);
                    let lang_ok = q_ok
                        && lang
                            .as_deref()
                            .map(|l| l.keeps_channel(name, g.as_deref()))
                            .unwrap_or(true);
                    if lang_ok {
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
            if !q.is_empty() {
                let found = db.search_vod_ids(&q, cat, 2_000);
                let mut tiles = db.slim_vod_by_ids(&source_ids, &found);
                if let Some(l) = lang.as_deref() {
                    tiles.retain(|t| l.keeps_title(&t.name, t.category_id.as_deref()));
                }
                progress.set_done(1);
                return (
                    BrowseIndex::identity(tiles.len()),
                    Vec::new(),
                    tiles,
                );
            }
            let vod_rows = db.collect_vod_browse_rows(&source_ids, cat);
            progress.set_total(vod_rows.len() as u64);
            let mut out = Vec::with_capacity(vod_rows.len().min(65_536));
            for chunk in vod_rows.chunks(DEFAULT_CHUNK) {
                for (i, _id, name, c) in chunk {
                    let q_ok = q_lc
                        .as_deref()
                        .map(|ql| name.to_lowercase().contains(ql))
                        .unwrap_or(true);
                    let lang_ok = lang
                        .as_deref()
                        .map(|l| l.keeps_title(name, c.as_deref()))
                        .unwrap_or(true);
                    if q_ok && lang_ok {
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
                let found = db.search_series_ids(&q, cat, 2_000);
                let mut tiles = db.slim_series_by_ids(&source_ids, &found);
                if let Some(l) = lang.as_deref() {
                    tiles.retain(|t| l.keeps_title(&t.name, t.category_id.as_deref()));
                }
                progress.set_done(1);
                return (
                    BrowseIndex::identity(tiles.len()),
                    Vec::new(),
                    tiles,
                );
            }
            let series_rows = db.collect_series_browse_rows(&source_ids, cat);
            progress.set_total(series_rows.len() as u64);
            let mut out = Vec::with_capacity(series_rows.len().min(65_536));
            for chunk in series_rows.chunks(DEFAULT_CHUNK) {
                for (i, _id, name, c) in chunk {
                    let q_ok = q_lc
                        .as_deref()
                        .map(|ql| name.to_lowercase().contains(ql))
                        .unwrap_or(true);
                    let lang_ok = lang
                        .as_deref()
                        .map(|l| l.keeps_title(name, c.as_deref()))
                        .unwrap_or(true);
                    if q_ok && lang_ok {
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
    (index, Vec::new(), Vec::new())
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

fn merge_xtream_vod_fields(existing: &mut VodItem, item: &VodItem) {
    take_text(&mut existing.plot, &item.plot);
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
    take_text(&mut existing.plot, &item.plot);
    take_text(&mut existing.cover, &item.cover);
    take_text(&mut existing.banner, &item.banner);
    take_text(&mut existing.year, &item.year);
    take_text(&mut existing.genre, &item.genre);
    take_text(&mut existing.rating, &item.rating);
    take_text(&mut existing.imdb_id, &item.imdb_id);
    take_text(&mut existing.actors, &item.actors);
    take_text(&mut existing.director, &item.director);
    take_text(&mut existing.country, &item.country);
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


fn profile_message_label(message: &Message) -> &'static str {
    match message {
        Message::PlayerTick => "tick.player",
        Message::VideoFrameAllocated { .. } => "tick.video_frame",
        Message::PlayerChromeTick => "tick.chrome",
        Message::ImageLoaded(..) => "async.image",
        Message::MetaEnriched { .. } => "async.meta",
        Message::VodInfoBatch(_) => "async.vod_info_batch",
        Message::PlayerLayout { .. } => "layout.player",
        Message::PlayerLayoutDirty(_) => "layout.player_dirty",
        Message::WindowResized { .. } => "layout.resize",
        Message::PlayerPointerActivity => "input.pointer",
        Message::DetailMetaLoaded { .. } => "async.detail_meta",
        Message::SeriesDetailLoaded(_) => "async.series_detail",
        Message::SeriesEpisodesEnriched(_) => "async.series_episodes",
        Message::VodDetailLoaded(_) => "async.vod_detail",
        Message::VodCategoryLoaded { .. } => "async.vod_category",
        Message::SeriesCategoryLoaded { .. } => "async.series_category",
        Message::CategoryPersistDone { .. } => "async.category_persist",
        Message::SourceLoaded { .. } => "async.source",
        Message::SourcesBatchLoaded(_) => "async.sources_batch",
        Message::BundleCacheReady(_) => "async.bundle_cache",
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
        Message::MosaicShowMore => "nav.mosaic_more",
        Message::CatScrolled(_, _) => "scroll.cats",
        Message::FlushPendingImages => "async.image_flush",
        Message::SearchApply(_) => "nav.filtre_apply",
        other => ui_action_label(other).unwrap_or("ui.autre"),
    }
}

fn ui_action_label(message: &Message) -> Option<&'static str> {
    Some(match message {
        Message::PlayerTick
        | Message::VideoFrameAllocated { .. }
        | Message::ImageLoaded(..)
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
        Message::MosaicShowMore => "nav.mosaic_more",
        Message::PlayerLayout { .. } | Message::PlayerLayoutDirty(_) | Message::WindowResized { .. } => {
            return None;
        }
        #[cfg(target_os = "android")]
        Message::SafPoll | Message::SafResult { .. } => return None,
        Message::Tab(_) => "nav.onglet",
        Message::CatFilterChanged(_) => "nav.filtre",
        #[cfg(target_os = "android")]
        Message::NavBack => "nav.retour",
        Message::BrowseFocusStep { .. } => "nav.browse_focus",
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
        Message::CycleAudio => "lecteur.piste_audio",
        Message::CycleSubtitles => "lecteur.sous_titres",
        Message::PlayerPanel(_) => "lecteur.panneau",
        Message::CycleSpeed => "lecteur.vitesse",
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
        Message::FormDnsServers(_) | Message::FormDohUrl(_) | Message::FormDotServer(_) => {
            "reglages.dns_field"
        }
        Message::FormWgPaste(_) => "reglages.wg_paste",
        Message::SaveOmdbKey => "reglages.omdb_save",
        Message::SetPrefLang(_) => "reglages.langue",
        Message::ToggleTranslateMeta => "reglages.traduction",
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
        Message::CycleAccent | Message::SetAccent(_) => "reglages.accent",
        Message::CycleBackend => "reglages.backend",
        Message::ToggleHwdec => "reglages.hwdec",
        Message::ToggleLowLatency => "reglages.low_latency",
        Message::TogglePrefetchNext => "reglages.prefetch",
        Message::CycleFpsGui => "reglages.fps_gui",
        Message::CycleFpsVideo => "reglages.fps_video",
        Message::RefreshDisplayCaps => "reglages.display_caps",
        Message::CycleVideoQuality => "reglages.video_quality",
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
        Message::SelectBrowseCategory(_) => "nav.categorie",
        Message::OpenSeries(_) => "series.open",
        Message::SeriesEpisodesEnriched(_) => "series.episodes_meta",
        Message::CloseSeriesDetail => "series.close",
        Message::OpenVodDetail(_) => "vod.open_detail",
        Message::VodDetailLoaded(_) => "vod.detail_loaded",
        Message::VodInfoBatch(_) => "vod.info_batch",
        Message::CloseVodDetail => "vod.close_detail",
        Message::DetailMetaLoaded { .. } => "meta.detail",
        Message::ClosePlayerWindow => "lecteur.fermer",
        Message::PlayerHotkey(_) | Message::PlayerHotkeyIn(..) => "lecteur.hotkey",
        Message::MainWindowOpened(_) => "fenetre.main_open",
        Message::PlayerWindowOpened(_) => "fenetre.player_open",
        Message::WindowClosed(_) => "fenetre.close",
        // High-frequency / async bookkeeping — do not spam ui.interaction.
        _ => return None,
    })
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
    let (fg, bg, radius, border_w, border_c) = if active_primary {
        (
            ui.on_primary(),
            ui.accent(),
            RADIUS_LARGE_INCREASED,
            0.0,
            Color::TRANSPARENT,
        )
    } else if active {
        (
            ui.on_secondary_container(),
            ui.secondary_container(),
            RADIUS_LARGE_INCREASED,
            0.0,
            Color::TRANSPARENT,
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
            ui.on_secondary_container(),
            ui.secondary_container(),
            RADIUS_LARGE_INCREASED,
            0.0,
            Color::TRANSPARENT,
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
                crate::icons::icon_label(crate::icons::Icon::Paste, "Coller", 13.0, ui.accent()),
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

/// D-pad / arrow browse when the player is closed (Android TV + desktop couch UI).
fn map_browse_keys(
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
        #[cfg(target_os = "android")]
        Key::Named(Named::GoBack) | Key::Named(Named::BrowserBack) | Key::Named(Named::Escape) => {
            Some(Message::NavBack)
        }
        Key::Named(Named::ArrowUp) => Some(Message::BrowseFocusStep { dcol: 0, drow: -1 }),
        Key::Named(Named::ArrowDown) => Some(Message::BrowseFocusStep { dcol: 0, drow: 1 }),
        Key::Named(Named::ArrowLeft) => Some(Message::BrowseFocusStep { dcol: -1, drow: 0 }),
        Key::Named(Named::ArrowRight) => Some(Message::BrowseFocusStep { dcol: 1, drow: 0 }),
        Key::Named(Named::Enter) => Some(Message::BrowseActivate),
        _ => None,
    }
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
}
