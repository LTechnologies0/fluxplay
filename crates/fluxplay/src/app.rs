use chrono::Local;
use fluxplay_core::models::{
    AccentPreset, AppSettings, Channel, ContentKind, FpsCapPref, MediaSource, NetworkSettings,
    PlaylistBundle, PlayerBackendPref, SeriesItem, SourceKind, ThemeMode, VodItem,
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

use crate::browser::{BrowseIndex, CAT_PAGE, LIST_PAGE};
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
use std::sync::Mutex;

#[cfg(target_os = "android")]
static PENDING_CATALOG_BOOT: Mutex<Option<crate::catalog_db::CatalogDb>> = Mutex::new(None);

pub(crate) fn run_daemon() -> iced::Result {
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
        .subscription(FluxPlay::subscription)
        .antialiasing(false)
        .default_font(iced::Font::with_name("Fira Sans"))
        .window(window::Settings {
            // Activity / freeform bounds drive layout — avoid fake 1920×1080.
            // Do not request iced Fullscreen / maximize-to-monitor: Waydroid freeform
            // collapses the NativeActivity surface (Requested h=0).
            size: Size::new(960.0, 720.0),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
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
    session: StreamSession,
    status: String,
    loading: bool,
    // Add-source form
    form_name: String,
    form_kind: SourceKind,
    form_endpoint: String,
    form_user: String,
    form_pass: String,
    form_mac: String,
    form_epg: String,
    form_omdb_key: String,
    form_dns_servers: String,
    form_doh_url: String,
    form_dot_server: String,
    form_wg_paste: String,
    network_probe: String,
    system_dark: bool,
    cat_filter: String,
    list_limit: usize,
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
    /// Last known outer position of the player window (legacy CLI overlay sizing).
    player_pos: Option<Point>,
    /// Embedded video frame (libffmpeg/libmpv soft RGBA → iced image).
    video_frame: Option<ImageHandle>,
    /// Pins GPU atlas memory so the displayed frame never async-flickers.
    video_allocation: Option<ImageAllocation>,
    video_frame_wh: (u32, u32),
    /// One in-flight `image::allocate` — drop intermediate soft frames.
    video_upload_busy: bool,
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
    prefetch_armed_for: Option<String>,
    /// Lazy Xtream `get_vod_info` enrich — never burst at boot (ban risk).
    xtream_vod_enrich_started: bool,
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
    /// Coalesce ImageLoaded → one iced view refresh per burst (fast scroll).
    pending_images: Vec<(String, Option<Uuid>, Vec<u8>)>,
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
enum EpisodeFlat {
    Header(u32),
    Ep { season_idx: usize, ep_idx: usize },
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
    FormOmdbKey,
    FormDnsServers,
    FormDohUrl,
    FormDotServer,
    FormWgPaste,
    Search,
    CatFilter,
    Goto,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Tab(Tab),
    SearchChanged(String),
    /// Apply debounced search (`search_debounce_gen` must still match).
    SearchApply(u64),
    PlayChannel(Channel),
    /// Browse lists: id only (no Channel clone on every virtual row paint).
    PlayChannelId(String),
    PlayVod {
        name: String,
        url: String,
        kind: ContentKind,
        poster: Option<String>,
    },
    Stop,
    TogglePause,
    ToggleMute,
    VolumeChanged(f32),
    SeekRel(i32),
    SeekPercent(f64),
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
    VideoFrameAllocated(Result<ImageAllocation, iced_image::Error>),
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
    FormOmdbKey(String),
    FormDnsServers(String),
    FormDohUrl(String),
    FormDotServer(String),
    FormWgPaste(String),
    SaveOmdbKey,
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
    BundleCacheReady(Result<PlaylistBundle, String>),
    OpenExternal,
    PickPlaylistFile,
    PlaylistFilePicked(Option<String>),
    ToggleFavorite(String),
    CycleBackend,
    ToggleHwdec,
    ToggleLowLatency,
    TogglePrefetchNext,
    CycleFpsGui,
    CycleFpsVideo,
    RefreshDisplayCaps,
    DiagnosePortals,
    PrefetchDone(Result<String, String>),
    DiagnoseDone(String),
    EpgFetched(Vec<fluxplay_core::models::EpgProgramme>),
    VodCategoryLoaded {
        category_id: String,
        result: Result<Vec<VodItem>, String>,
    },
    SeriesCategoryLoaded {
        category_id: String,
        result: Result<Vec<SeriesItem>, String>,
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
    LoadMore,
    /// Virtualized browse scroll: absolute Y + viewport height.
    BrowseScrolled(f32, f32),
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
    ImageLoaded(Result<(String, Option<uuid::Uuid>, Vec<u8>), (String, String)>),
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
    // ── Extended player controls ───────────────────────────────────────────
    PlayerPanel(PlayerPanel),
    CycleSpeed,
    ToggleLoop,
    Screenshot,
    GotoDraftChanged(String),
    GotoSubmit,
    ToggleSubVisibility,
    CycleAspect,
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
}

#[derive(Debug, Clone, Copy)]
enum PlayerHotkey {
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
                size: Size::new(1920.0, 1080.0),
                position: window::Position::Centered,
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
            session,
            status,
            loading: true, // cache load / sync always off-UI; cleared in BundleCacheReady / ingest done
            form_name: String::new(),
            form_kind: SourceKind::M3uPlus,
            form_endpoint: String::new(),
            form_user: String::new(),
            form_pass: String::new(),
            form_mac: String::new(),
            form_epg: String::new(),
            form_omdb_key: String::new(), // filled below from settings
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
            #[cfg(target_os = "android")]
            player_embedded: false,
            #[cfg(target_os = "android")]
            main_size: Size::new(360.0, 720.0),
            #[cfg(not(target_os = "android"))]
            main_size: Size::new(1920.0, 1080.0),
            layout_cache: None,
            backends_cache: None,
            player_pos: None,
            video_frame: None,
            video_allocation: None,
            video_frame_wh: (0, 0),
            video_upload_busy: false,
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
            prefetch_armed_for: None,
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
            episode_flat: Vec::new(),
            browse_scroll_y: 0.0,
            browse_view_h: 720.0,
            browse_slice: (0, 0),
            cat_scroll_y: 0.0,
            cat_view_h: 720.0,
            cat_slice: (0, 0),
            cat_entries: Vec::new(),
            pending_images: Vec::new(),
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
            browse_focus: 0,
            browse_drag_last_dy: 0.0,
            browse_coast_vy: 0.0,
            pressed_mosaic: None,
            #[cfg(target_os = "android")]
            saf_kind: None,
            #[cfg(target_os = "android")]
            system_insets: (0.0, 0.0, 0.0, 24.0),
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
        app.form_dns_servers = app.settings.network.dns_servers.clone();
        app.form_doh_url = app.settings.network.doh_url.clone();
        app.form_dot_server = app.settings.network.dot_server.clone();
        crate::network::apply_to_http(&app.settings.network);
        crate::metadata::set_omdb_api_key(if app.settings.omdb_api_key.is_empty() {
            None
        } else {
            Some(app.settings.omdb_api_key.clone())
        });

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
        let stage = self
            .session
            .native
            .video_rect()
            .map(|r| (r.w, r.h))
            .filter(|(w, h)| *w >= 2 && *h >= 2)
            .or_else(|| {
                let (w, h) = self.video_frame_wh;
                (w >= 2 && h >= 2).then_some((w, h))
            });
        self.display_caps = display_caps::resolve_caps(
            self.settings.fps_gui,
            self.settings.fps_video,
            stage,
            self.session.content_fps(),
            Some(&self.display_probe),
        );
        self.apply_host_tuning();
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
    fn layout_metrics(&self) -> LayoutMetrics {
        if let Some((sz, m)) = self.layout_cache {
            if (sz.width - self.main_size.width).abs() < 0.5
                && (sz.height - self.main_size.height).abs() < 0.5
            {
                return m;
            }
        }
        LayoutMetrics::compute(self.main_size.width, self.main_size.height)
    }

    fn refresh_layout_cache(&mut self) {
        let m = LayoutMetrics::compute(self.main_size.width, self.main_size.height);
        self.layout_cache = Some((self.main_size, m));
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
            let soft_video = self.session.has_embedded_video()
                && matches!(
                    self.session.state,
                    PlaybackState::Playing | PlaybackState::Buffering
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
            // Soft-render: poll at display_caps.video_hz; skip work when !dirty.
            let period_ms = if soft_video {
                self.display_caps.video_period_ms()
            } else if self.sleep_until.is_some() {
                1000
            } else if playing_like && player_open {
                // Lightweight time/chrome poll while paused / CLI backend.
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
        // Chrome autohide + alpha lerp paced by GUI FPS cap (not a second heavy video pull).
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
            // Keep ticking while fading out (visible=false but alpha still > 0).
            let animating =
                self.player_chrome_visible || self.chrome_alpha > 0.05 || self.player_panel != PlayerPanel::None;
            if player_open && animating {
                iced::time::every(std::time::Duration::from_millis(
                    self.display_caps.gui_period_ms().max(100),
                ))
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
                position: window::Position::Centered,
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
        // Fullscreen settle: compositor emits a burst of sizes — keep stage sticky.
        if let Some(until) = self.player_layout_freeze_until {
            if std::time::Instant::now() < until {
                return;
            }
            self.player_layout_freeze_until = None;
        }
        // Hysteresis: ignore ±4px noise from compositor / scale rounding.
        if let Some(prev) = self.session.native.video_rect() {
            let dw = (prev.w as i32 - w as i32).unsigned_abs();
            let dh = (prev.h as i32 - h as i32).unsigned_abs();
            if dw <= 4 && dh <= 4 {
                return;
            }
            // Ignore tiny proportional jitter (<3%) that still causes soft-frame flicker.
            let pw = prev.w.max(1) as f32;
            let ph = prev.h.max(1) as f32;
            if (w as f32 - pw).abs() / pw < 0.03 && (h as f32 - ph).abs() / ph < 0.03 {
                return;
            }
        }
        tracing::debug!(?rect, %scale, "player embed stage size");
        self.session.set_video_rect(rect);
    }

    fn close_player_window(&mut self) -> Task<Message> {
        self.player_fullscreen = false;
        self.player_chrome_visible = true;
        self.player_panel = PlayerPanel::None;
        #[cfg(target_os = "android")]
        {
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
                        if let Some(ch) = self.bundle.channels.first().cloned() {
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
                }
                let layout = self.sync_player_layout_task(id);
                if std::env::var_os("FLUXPLAY_AUTO_FULLSCREEN").is_some() {
                    return Task::batch([layout, Task::done(Message::ToggleFullscreen)]);
                }
                return layout;
            }
            Message::PlayerLayoutDirty(id) => {
                if self.player_id == Some(id) {
                    return self.sync_player_layout_task(id);
                }
            }
            Message::WindowResized { id, size } => {
                if self.main_id.is_none() {
                    self.main_id = Some(id);
                }
                if self.main_id == Some(id) {
                    // Guard against zero / garbage sizes from early surface churn.
                    if size.width >= 32.0 && size.height >= 32.0 {
                        self.main_size = size;
                        self.refresh_layout_cache();
                        // Approx content viewport until the first scrollable on_scroll.
                        self.browse_view_h = (size.height * 0.62).max(240.0);
                        #[cfg(target_os = "android")]
                        {
                            self.system_insets = crate::android_bridge::system_insets_dp();
                        }
                        tracing::info!(w = size.width, h = size.height, "main window size");
                    } else {
                        tracing::warn!(
                            w = size.width,
                            h = size.height,
                            "ignored degenerate window size"
                        );
                    }
                }
                if self.player_id == Some(id) {
                    // Skip resize storms while fullscreen mode is settling.
                    if self
                        .player_layout_freeze_until
                        .is_some_and(|t| std::time::Instant::now() < t)
                    {
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
                    self.video_frame_wh = (0, 0);
                    self.video_upload_busy = false;
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
                // Stop may already have run (e.g. deferred after Message::Stop).
                self.session.stop();
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
                self.list_limit = LIST_PAGE;
                self.series_detail = None;
                self.vod_detail = None;
                self.detail_meta_loading = false;
                if tab == Tab::Settings && self.backends_cache.is_none() {
                    self.backends_cache = Some(detect_backends());
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
                self.list_limit = LIST_PAGE;
                return Task::batch([self.rebuild_browse_index(), self.refresh_browse_art()]);
            }
            Message::CatFilterChanged(s) => {
                self.cat_filter = s;
                self.rebuild_cat_entries();
            }
            Message::LoadMore => {
                tracing::debug!(list_limit = self.list_limit, "load more");
                self.list_limit = (self.list_limit.saturating_add(LIST_PAGE)).min(10_000);
                return self.refresh_browse_art();
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
                if flinging {
                    return Task::none();
                }
                return Task::batch([
                    self.flush_pending_images(),
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
                return iced::widget::operation::scroll_by(
                    iced::widget::Id::from(self.browse_scroll_widget_id()),
                    AbsoluteOffset { x: 0.0, y: dy },
                );
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
                if key == self.cat_slice && (view_h - self.cat_view_h).abs() < 8.0 {
                    return Task::none();
                }
                self.cat_scroll_y = y;
                self.cat_view_h = view_h;
                self.cat_slice = key;
            }
            Message::SelectBrowseCategory(id) => {
                self.list_limit = LIST_PAGE;
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
                        let follow = if id == "*" {
                            self.refresh_browse_art()
                        } else if self
                            .bundle
                            .vod
                            .iter()
                            .any(|v| v.category_id.as_deref() == Some(id.as_str()))
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
                        let follow = if id == "*" {
                            self.refresh_browse_art()
                        } else if self
                            .bundle
                            .series
                            .iter()
                            .any(|s| s.category_id.as_deref() == Some(id.as_str()))
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
            Message::PlayChannelId(id) => {
                if self.consume_browse_drag_suppress() {
                    return Task::none();
                }
                let Some(ch) = self.bundle.channels.iter().find(|c| c.id == id).cloned() else {
                    self.status = "Chaîne introuvable".into();
                    return Task::none();
                };
                return Task::done(Message::PlayChannel(ch));
            }
            Message::PlayChannel(ch) => {
                if self.consume_browse_drag_suppress() {
                    return Task::none();
                }
                tracing::info!(id = %ch.id, name = %ch.name, "play channel");
                self.selected_channel = Some(ch.id.clone());
                self.apply_source_headers_for(&ch);
                self.settings.push_recent(&ch);
                let epg_task = self.fetch_epg_for_ids(vec![ch.id.clone()]);
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

                match self.session.open_channel(ch.clone()) {
                    Ok(()) => {
                        self.status = self.session.status_line();
                        self.persist();
                        #[cfg(target_os = "android")]
                        {
                            // Prefer in-process libmpv RGBA embed (desktop parity).
                            // Only fall back to ACTION_VIEW when embed is unavailable.
                            if !self.session.has_embedded_video() {
                                if let Err(e) =
                                    crate::android_intent::open_stream_url(&ch.stream_url)
                                {
                                    self.status = format!("Intent: {e}");
                                } else {
                                    self.status = format!("Lecteur système · {}", ch.name);
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
                                        self.session.backend = Some(BackendId::External);
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
                    source_id: self.xtream_source().map(|s| s.id),
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
                match self.session.open_channel(ch) {
                    Ok(()) => {
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
                                    if e.to_string().contains("EXTERNAL_NEEDS_INTENT") {
                                        self.session.backend = Some(BackendId::External);
                                    }
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
                self.refresh_system_dark();
                self.system_insets = crate::android_bridge::system_insets_dp();
                self.pip_mode = crate::android_bridge::poll_pip_mode();
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
                if self.series_detail.is_some() {
                    self.series_detail = None;
                    self.episode_flat.clear();
                    self.status = "Catalogue".into();
                    return Task::none();
                }
                if self.vod_detail.is_some() {
                    self.vod_detail = None;
                    self.status = "Catalogue".into();
                    return Task::none();
                }
                // Root: finish Activity (Back at home).
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
                return Task::done(Message::PlayChannelId(ch.id.clone()));
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
                self.video_frame = None;
                self.video_allocation = None;
                self.video_frame_wh = (0, 0);
                self.video_upload_busy = false;
                self.status = "Arrêté".into();
                // Defer window close so libmpv teardown finishes cleanly.
                return Task::perform(
                    async {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    },
                    |_| Message::ClosePlayerWindow,
                );
            }
            Message::TogglePause => {
                match self.session.state {
                    PlaybackState::Playing => self.session.pause(),
                    PlaybackState::Paused => self.session.resume(),
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
                if self.session.muted && v > 0.0 {
                    self.session.muted = false;
                }
                self.persist();
            }
            Message::SeekRel(secs) => {
                self.session.seek_relative(secs as f64);
                self.status = format!("Seek {secs:+}s · {}", self.session.elapsed_label());
            }
            Message::SeekPercent(pct) => {
                self.session.seek_percent(pct * 100.0);
                self.status = format!("Position {}", self.session.elapsed_label());
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
                    crate::android_bridge::set_immersive_mode(self.player_fullscreen);
                    self.status = if self.player_fullscreen {
                        "Plein écran (chrome masqué)".into()
                    } else {
                        "Fenêtre".into()
                    };
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
            }
            Message::PlayerChromeTick => {
                self.maybe_autohide_player_chrome();
                self.lerp_chrome_alpha();
            }
            Message::CycleAudio => {
                self.session.cycle_audio();
                self.status = "Piste audio suivante".into();
            }
            Message::CycleSubtitles => {
                self.session.cycle_subtitles();
                self.status = "Sous-titres suivants".into();
            }
            Message::PlayerTick => {
                self.refresh_system_dark();
                self.refresh_display_caps(false);
                #[cfg(target_os = "android")]
                {
                    let fg = iced::android::is_foreground();
                    let in_pip = crate::android_bridge::poll_pip_mode();
                    self.pip_mode = in_pip;
                    if !fg && !self.lifecycle_paused && !in_pip {
                        if matches!(
                            self.session.state,
                            PlaybackState::Playing | PlaybackState::Buffering
                        ) {
                            self.session.pause();
                            self.lifecycle_paused = true;
                            crate::android_bridge::set_keep_screen_on(false);
                            crate::android_bridge::abandon_audio_focus();
                        }
                    } else if fg && self.lifecycle_paused {
                        self.session.resume();
                        self.lifecycle_paused = false;
                    }
                    let keep = (fg || in_pip)
                        && matches!(
                            self.session.state,
                            PlaybackState::Playing | PlaybackState::Buffering
                        );
                    crate::android_bridge::set_keep_screen_on(keep);
                    if keep {
                        crate::android_bridge::request_audio_focus();
                    } else if matches!(
                        self.session.state,
                        PlaybackState::Paused | PlaybackState::Idle | PlaybackState::Error
                    ) {
                        crate::android_bridge::abandon_audio_focus();
                    }
                }
                self.session.refresh_times();
                if !self.session.native.is_running()
                    && matches!(
                        self.session.state,
                        PlaybackState::Playing | PlaybackState::Paused
                    )
                {
                    self.session.state = PlaybackState::Idle;
                    self.video_frame = None;
                    self.video_allocation = None;
                    self.video_upload_busy = false;
                    self.status = "Lecture terminée".into();
                }
                let mut tasks = Vec::new();
                // Soft-render: pull RGBA, then GPU-allocate BEFORE swapping the
                // displayed Handle — iced async-uploads otherwise flash black
                // between unique frame IDs (documented flicker for animated images).
                // Present rate is capped by display_caps.video_hz via subscription period;
                // still skip when mpv reports no new frame.
                if !self.video_upload_busy
                    && self.session.has_embedded_video()
                    && matches!(
                        self.session.state,
                        PlaybackState::Playing | PlaybackState::Buffering
                    )
                    && self.session.frame_needs_redraw()
                {
                    let frozen = self
                        .player_layout_freeze_until
                        .is_some_and(|t| std::time::Instant::now() < t);
                    let (fw, fh) = self
                        .session
                        .native
                        .video_rect()
                        .map(|r| (r.w, r.h))
                        .unwrap_or((1280, 720));
                    let uhd = std::env::var_os("FLUXPLAY_SOFT_UHD").is_some();
                    let max_w = if uhd { 3840u32 } else { 1920 };
                    let max_h = if uhd { 2160u32 } else { 1080 };
                    let scale = (max_w as f32 / fw.max(1) as f32)
                        .min(max_h as f32 / fh.max(1) as f32)
                        .min(1.0);
                    let rw = ((fw as f32 * scale).round() as u32).max(2) & !1;
                    let rh = ((fh as f32 * scale).round() as u32).max(2) & !1;
                    let (rw, rh) = if self.video_frame_wh.0 >= 2 && self.video_frame_wh.1 >= 2 {
                        let (pw, ph) = self.video_frame_wh;
                        if frozen {
                            (pw, ph)
                        } else {
                            let dw = (pw as i32 - rw as i32).unsigned_abs();
                            let dh = (ph as i32 - rh as i32).unsigned_abs();
                            if dw <= 8 && dh <= 8 {
                                (pw, ph)
                            } else if (rw as f32 - pw as f32).abs() / (pw.max(1) as f32) < 0.05
                                && (rh as f32 - ph as f32).abs() / (ph.max(1) as f32) < 0.05
                            {
                                (pw, ph)
                            } else {
                                (rw, rh)
                            }
                        }
                    } else {
                        (rw, rh)
                    };
                    if let Some((w, h, rgba)) = self.session.pull_video_frame(rw, rh) {
                        self.video_frame_wh = (w, h);
                        self.video_upload_busy = true;
                        let handle = ImageHandle::from_rgba(w, h, rgba);
                        tasks.push(
                            iced_image::allocate(handle).map(Message::VideoFrameAllocated),
                        );
                    }
                }
                if let Some(deadline) = self.sleep_until {
                    if std::time::Instant::now() >= deadline {
                        self.sleep_until = None;
                        self.sleep_mins = None;
                        self.session.pause();
                        self.status = "Veille — lecture en pause".into();
                    }
                }
                self.maybe_autohide_player_chrome();
                self.lerp_chrome_alpha();
                if self.browse_coast_vy.abs() >= 2.0 {
                    tasks.push(self.apply_browse_coast_step());
                }
                if let Some(prefetch) = self.maybe_prefetch_next_episode() {
                    tasks.push(prefetch);
                }
                if !tasks.is_empty() {
                    return Task::batch(tasks);
                }
            }
            Message::VideoFrameAllocated(result) => {
                self.video_upload_busy = false;
                match result {
                    Ok(allocation) => {
                        self.video_frame = Some(allocation.handle().clone());
                        self.video_allocation = Some(allocation);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "video frame GPU allocate failed");
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
                self.session.toggle_sub_visibility();
                self.status = "Visibilité sous-titres basculée".into();
            }
            Message::CycleAspect => {
                self.session.cycle_aspect();
                self.status = format!("Aspect {}", self.session.aspect.label());
            }
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
                    self.pip_mode = !self.pip_mode;
                    if self.pip_mode {
                        let (w, h) = if self.video_frame_wh.0 > 0 {
                            self.video_frame_wh
                        } else {
                            (16, 9)
                        };
                        crate::android_bridge::enter_pip(w.max(1) as i32, h.max(1) as i32);
                        crate::android_bridge::request_audio_focus();
                        self.status = "PiP système".into();
                    } else {
                        self.status = "PiP off — revenez à FluxPlay".into();
                    }
                    return Task::none();
                }
                #[cfg(not(target_os = "android"))]
                {
                    self.pip_mode = !self.pip_mode;
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
                            self.sync_player_layout_task(id),
                        ]);
                    }
                }
            }
            Message::ChapterStep(d) => self.session.chapter_step(d),
            Message::PlaylistPrev => {
                if let Some(ch) = self.playlist_neighbor(-1) {
                    return Task::done(Message::PlayChannel(ch));
                }
            }
            Message::PlaylistNext => {
                if let Some(ch) = self.playlist_neighbor(1) {
                    return Task::done(Message::PlayChannel(ch));
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
                self.session.mark_ab_a();
                self.status = "Point A".into();
            }
            Message::MarkAbB => {
                self.session.mark_ab_b();
                self.status = "Point B".into();
            }
            Message::ClearAbLoop => {
                self.session.clear_ab_loop();
                self.status = "A–B off".into();
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
                self.status = if self.session.night_vf {
                    "Mode nuit image ON".into()
                } else {
                    "Mode nuit image off".into()
                };
            }
            Message::CycleCache => {
                self.settings.cache_ms = match self.settings.cache_ms {
                    0..=1999 => 4000,
                    2000..=5999 => 8000,
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
                    12..=24 => 24.0,
                    _ => 4.0,
                };
                self.resync_player_options();
                self.persist();
                self.status = format!(
                    "Buffer demux {:.0}s (prochain flux)",
                    self.settings.demux_secs
                );
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
                        if self.session.state == PlaybackState::Paused {
                            self.session.resume();
                        } else {
                            self.session.pause();
                        }
                        self.status = self.session.status_line();
                    }
                    PlayerHotkey::SeekBack => self.session.seek_relative(-10.0),
                    PlayerHotkey::SeekFwd => self.session.seek_relative(10.0),
                    PlayerHotkey::SeekBackBig => self.session.seek_relative(-30.0),
                    PlayerHotkey::SeekFwdBig => self.session.seek_relative(30.0),
                    PlayerHotkey::VolumeUp => self.session.volume_delta(0.05),
                    PlayerHotkey::VolumeDown => self.session.volume_delta(-0.05),
                    PlayerHotkey::Mute => self.session.toggle_mute(),
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
                        self.session.stop();
                        self.status = "Arrêté".into();
                        return self.close_player_window();
                    }
                    PlayerHotkey::Stop => {
                        self.session.stop();
                        self.status = "Arrêté".into();
                    }
                    PlayerHotkey::Restart => self.session.restart(),
                    PlayerHotkey::Speed => {
                        self.session.cycle_speed();
                        self.status = format!("Vitesse {:.2}×", self.session.speed);
                    }
                    PlayerHotkey::Loop => {
                        self.session.toggle_loop();
                    }
                    PlayerHotkey::Screenshot => {
                        return Task::done(Message::Screenshot);
                    }
                    PlayerHotkey::FrameStep => self.session.frame_step(),
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
            Message::FormKind(k) => self.form_kind = k,
            Message::FormEndpoint(s) => self.form_endpoint = s,
            Message::FormUser(s) => self.form_user = s,
            Message::FormPass(s) => self.form_pass = s,
            Message::FormMac(s) => self.form_mac = s,
            Message::FormEpg(s) => self.form_epg = s,
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
                    PasteTarget::FormOmdbKey => self.form_omdb_key = cleaned,
                    PasteTarget::FormDnsServers => self.form_dns_servers = cleaned,
                    PasteTarget::FormDohUrl => self.form_doh_url = cleaned,
                    PasteTarget::FormDotServer => self.form_dot_server = cleaned,
                    PasteTarget::FormWgPaste => self.form_wg_paste = cleaned,
                    PasteTarget::Search => {
                        self.search = cleaned;
                        self.list_limit = LIST_PAGE;
                    }
                    PasteTarget::CatFilter => self.cat_filter = cleaned,
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
                    async { fluxplay_providers::probe_dns("cloudflare.com").await },
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
                self.status = format!("Playlist publique ajoutée — sync…");
                self.loading = true;
                return self.reload_one_task(id);
            }
            Message::AddSource => {
                if self.form_name.trim().is_empty() || self.form_endpoint.trim().is_empty() {
                    tracing::warn!("add source rejected — name/endpoint required");
                    self.status = "Nom et endpoint requis".into();
                    return Task::none();
                }
                if matches!(self.form_kind, SourceKind::Xtream)
                    && (self.form_user.trim().is_empty() || self.form_pass.trim().is_empty())
                {
                    tracing::warn!("add Xtream rejected — username/password required");
                    self.status = "Xtream : identifiant et mot de passe requis".into();
                    return Task::none();
                }
                tracing::info!(
                    name = %self.form_name.trim(),
                    kind = ?self.form_kind,
                    "add source"
                );
                let mut src =
                    MediaSource::new(self.form_name.trim(), self.form_kind, self.form_endpoint.trim());
                if !self.form_user.is_empty() {
                    src.username = Some(self.form_user.clone());
                }
                if !self.form_pass.is_empty() {
                    src.password = Some(self.form_pass.clone());
                }
                if !self.form_mac.is_empty() {
                    src.mac = Some(self.form_mac.clone());
                }
                if !self.form_epg.is_empty() {
                    src.epg_url = Some(self.form_epg.clone());
                }
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
                self.form_name.clear();
                self.form_endpoint.clear();
                self.form_user.clear();
                self.form_pass.clear();
                self.form_mac.clear();
                self.form_epg.clear();
                self.persist();
                self.status = "Source ajoutée — sync catalogue…".into();
                self.loading = true;
                return self.reload_one_task(id);
            }
            Message::RemoveSource(id) => {
                tracing::info!(%id, "remove source");
                self.sources.retain(|s| s.id != id);
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
            Message::BundleCacheReady(result) => {
                self.loading = false;
                match result {
                    Ok(bundle) => {
                        self.apply_bundle_cache(bundle);
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
                        return Task::batch(tasks);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "catalog reload failed");
                        self.bundle = PlaylistBundle::default();
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
                    ..
                } = report;
                tracing::info!(ok, err, unchanged, "sources batch ingest done");
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
                return Task::batch([
                    self.snap_browse_scroll_task(),
                    self.refresh_browse_art(),
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
            Message::VodInfoBatch(items) => {
                for item in items {
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
                return self.prefetch_catalog_art();
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
                        let _ = open::that(&ch.stream_url);
                        self.status = "Ouvert dans le lecteur externe".into();
                    }
                }
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
            Message::PlaylistFilePicked(Some(path)) => {
                self.form_kind = SourceKind::M3uPlus;
                self.form_endpoint = path;
                if self.form_name.is_empty() {
                    self.form_name = "Playlist locale".into();
                }
            }
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
                self.refresh_display_caps(false);
                self.persist();
                self.status = format!(
                    "FPS GUI : {} → {} fps",
                    self.settings.fps_gui.label(),
                    self.display_caps.gui_hz
                );
            }
            Message::CycleFpsVideo => {
                self.settings.fps_video = self.settings.fps_video.cycle();
                self.refresh_display_caps(false);
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
            Message::PrefetchDone(Ok(path)) => {
                tracing::info!(%path, "next episode prefetched");
                self.status = format!("Épisode suivant préchargé");
                let _ = path;
            }
            Message::PrefetchDone(Err(e)) => {
                tracing::debug!(error = %e, "prefetch skipped");
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
                self.status = if report.len() > 320 {
                    format!("{}…", &report[..317])
                } else {
                    report
                };
            }
            Message::EpgFetched(programmes) => {
                if !programmes.is_empty() {
                    let n = programmes.len();
                    fluxplay_providers::merge_epg(&mut self.bundle.epg, programmes.clone());
                    if let Some(src) = self.xtream_source() {
                        if let Some(db) = &self.catalog_db {
                            if let Err(e) = db.merge_epg(src.id, &programmes) {
                                tracing::warn!(error = %e, "persist epg failed");
                            }
                        }
                    }
                    if self.tab == Tab::Epg {
                        self.status = format!("Guide TV · {n} programmes");
                    }
                }
            }
            Message::VodCategoryLoaded {
                category_id,
                result,
            } => {
                self.selected_vod_category = Some(category_id.clone());
                match result {
                    Ok(items) => {
                        self.bundle.vod.retain(|v| v.category_id.as_deref() != Some(&category_id));
                        let n = items.len();
                        self.bundle.vod.extend(items);
                        self.status = format!("VOD catégorie · {n} films");
                        return Task::batch([
                            self.rebuild_browse_index(),
                            self.refresh_browse_art(),
                        ]);
                    }
                    Err(e) => self.status = format!("VOD: {e}"),
                }
            }
            Message::SeriesCategoryLoaded {
                category_id,
                result,
            } => {
                self.selected_series_category = Some(category_id.clone());
                match result {
                    Ok(items) => {
                        self.bundle
                            .series
                            .retain(|s| s.category_id.as_deref() != Some(&category_id));
                        let n = items.len();
                        self.bundle.series.extend(items);
                        self.status = format!("Séries catégorie · {n} titres");
                        return Task::batch([
                            self.rebuild_browse_index(),
                            self.refresh_browse_art(),
                        ]);
                    }
                    Err(e) => self.status = format!("Séries: {e}"),
                }
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
                if let Some(existing) = self.bundle.series.iter().find(|s| s.id == id) {
                    self.series_detail = Some(existing.clone());
                    self.rebuild_episode_flat();
                }
                self.status = "Chargement épisodes…".into();
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
                        return self.enrich_open_detail_task(true);
                    }
                    return Task::none();
                };
                return Task::perform(
                    async move {
                        fluxplay_providers::load_xtream_series_info(&src, &id)
                            .await
                            .map_err(|e| e.to_string())
                    },
                    Message::SeriesDetailLoaded,
                );
            }
            Message::SeriesDetailLoaded(result) => match result {
                Ok(item) => {
                    let eps: usize = item.seasons.iter().map(|s| s.episodes.len()).sum();
                    self.status = format!(
                        "{} · {} saisons · {eps} épisodes",
                        item.name,
                        item.seasons.len()
                    );
                    if let Some(existing) = self.bundle.series.iter_mut().find(|s| s.id == item.id) {
                        existing.seasons = item.seasons.clone();
                        existing.plot = item.plot.clone().or(existing.plot.clone());
                        existing.cover = item.cover.clone().or(existing.cover.clone());
                        existing.banner = item.banner.clone().or(existing.banner.clone());
                        existing.year = item.year.clone().or(existing.year.clone());
                        existing.genre = item.genre.clone().or(existing.genre.clone());
                        existing.rating = item.rating.clone().or(existing.rating.clone());
                        existing.imdb_id = item.imdb_id.clone().or(existing.imdb_id.clone());
                        existing.actors = item.actors.clone().or(existing.actors.clone());
                        existing.director = item.director.clone().or(existing.director.clone());
                    }
                    self.series_detail = Some(item);
                    self.browse_scroll_y = 0.0;
                    self.rebuild_episode_flat();
                    return Task::batch([
                        self.prefetch_visible_art(),
                        self.enrich_open_detail_task(true),
                        self.enrich_episodes_task(),
                    ]);
                }
                Err(e) => self.status = format!("Série: {e}"),
            },
            Message::SeriesEpisodesEnriched(item) => {
                if let Some(existing) = self.bundle.series.iter_mut().find(|s| s.id == item.id) {
                    existing.seasons = item.seasons.clone();
                }
                if self.series_detail.as_ref().map(|d| d.id.as_str()) == Some(item.id.as_str()) {
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
                let Some(item) = self.bundle.vod.iter().find(|v| v.id == id).cloned() else {
                    self.status = "Film introuvable".into();
                    return Task::none();
                };
                self.detail_meta_loading = true;
                self.status = format!("{} — fiche", item.name);
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
                            fluxplay_providers::load_xtream_vod_info(&src, &vod_id)
                                .await
                                .map_err(|e| e.to_string())
                        },
                        Message::VodDetailLoaded,
                    )
                } else {
                    self.enrich_open_detail_task(false)
                };
                return Task::batch([self.prefetch_visible_art(), xtream]);
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
                    if self.vod_detail.as_ref().map(|d| d.id.as_str()) == Some(item.id.as_str()) {
                        let stream_url = self
                            .vod_detail
                            .as_ref()
                            .map(|d| d.stream_url.clone())
                            .unwrap_or_else(|| item.stream_url.clone());
                        let poster = self
                            .vod_detail
                            .as_ref()
                            .and_then(|d| d.poster.clone())
                            .or(item.poster.clone());
                        let mut merged = item;
                        merged.stream_url = stream_url;
                        if merged.poster.is_none() {
                            merged.poster = poster;
                        }
                        self.status = if merged.plot.is_some() {
                            format!("{} · fiche Xtream", merged.name)
                        } else {
                            format!("{} — enrichissement…", merged.name)
                        };
                        self.vod_detail = Some(merged);
                    }
                    self.detail_meta_loading = false;
                    return Task::batch([
                        self.prefetch_visible_art(),
                        self.enrich_open_detail_task(false),
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
                            self.status = format!("{} · fiche IMDb", detail.name);
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
                            self.status = format!("{} · fiche IMDb", detail.name);
                        }
                    }
                }
                return self.refresh_browse_art();
            }
            Message::ImageLoaded(Ok((url, source_id, bytes))) => {
                self.jobs.images.tick();
                self.pending_images.push((url, source_id, bytes));
                // During fling: buffer only — no iced view rebuild (LOD / culling).
                if self.browse_flinging() {
                    return Task::none();
                }
                if self.pending_images.len() >= 10 {
                    return Task::batch([
                        self.flush_pending_images(),
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
                return self.continue_art_warm();
            }
            Message::FlushPendingImages => {
                self.image_flush_armed = false;
                if self.browse_flinging() {
                    // Keep buffering; settle path will flush.
                    return Task::none();
                }
                return Task::batch([
                    self.flush_pending_images(),
                    self.continue_art_warm(),
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
                    if let Some(joined) = set.join_next().await {
                        if let Ok((is_series, id, sid, name, patch)) = joined {
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
        for v in self
            .bundle
            .vod
            .iter()
            .filter(|v| crate::metadata::needs_vod_enrich(v))
            .take(12)
        {
            let src = v
                .source_id
                .and_then(|id| {
                    self.sources.iter().find(|s| {
                        s.id == id && s.enabled && s.kind == SourceKind::Xtream
                    })
                })
                .or_else(|| {
                    self.sources
                        .iter()
                        .find(|s| s.enabled && s.kind == SourceKind::Xtream)
                });
            if let Some(src) = src {
                jobs.push((src.clone(), v.id.clone()));
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
                    if let Some(joined) = set.join_next().await {
                        if let Ok(Some(item)) = joined {
                            out.push(item);
                        }
                    }
                }
                tracing::info!(n = out.len(), "xtream vod_info batch done");
                Message::VodInfoBatch(out)
            },
            |m| m,
        )
    }

    fn mosaic_meta_line(
        year: Option<&str>,
        genre: Option<&str>,
        rating: Option<&str>,
        fallback: &str,
    ) -> String {
        let mut out = String::with_capacity(48);
        let mut first = true;
        let mut push = |s: &str| {
            if s.is_empty() {
                return;
            }
            if !first {
                out.push_str(" · ");
            }
            out.push_str(s);
            first = false;
        };
        if let Some(y) = year.filter(|s| !s.is_empty()) {
            push(y);
        }
        if let Some(g) = genre.filter(|s| !s.is_empty()) {
            let g0 = g.split(',').next().unwrap_or(g).trim();
            push(g0);
        }
        if let Some(r) = rating.filter(|s| !s.is_empty() && *s != "0" && *s != "0.0") {
            if !first {
                out.push_str(" · ");
            }
            out.push('★');
            out.push(' ');
            out.push_str(r);
            first = false;
        }
        if first {
            fallback.to_string()
        } else {
            out
        }
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
        // Identity: full catalog order — no allocation, instant ready for 1M titles.
        if q.is_empty() {
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
                Tab::Live if matches!(self.selected_group.as_deref(), None | Some("Tous") | Some("*")) =>
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
        let source_ids: Vec<Uuid> = self.sources.iter().map(|s| s.id).collect();

        // Slim rows: avoid cloning titles when we only need category / id match.
        let live_rows: Vec<(usize, String, Option<String>)> = if matches!(tab, Tab::Live) {
            self.bundle
                .channels
                .iter()
                .enumerate()
                .filter(|(_, c)| c.kind == ContentKind::Live)
                .map(|(i, c)| (i, c.name.clone(), c.group.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let vod_rows: Vec<(usize, String, String, Option<String>)> = if matches!(tab, Tab::Vod) {
            if q.is_empty() {
                // Category filter only — skip name clone.
                self.bundle
                    .vod
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i, String::new(), String::new(), v.category_id.clone()))
                    .collect()
            } else {
                self.bundle
                    .vod
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i, v.id.clone(), v.name.clone(), v.category_id.clone()))
                    .collect()
            }
        } else {
            Vec::new()
        };
        let series_rows: Vec<(usize, String, String, Option<String>)> =
            if matches!(tab, Tab::Series) {
                if q.is_empty() {
                    self.bundle
                        .series
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (i, String::new(), String::new(), s.category_id.clone()))
                        .collect()
                } else {
                    self.bundle
                        .series
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (i, s.id.clone(), s.name.clone(), s.category_id.clone()))
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
                        source_ids,
                        live_rows,
                        vod_rows,
                        series_rows,
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
        self.browse_slice = self.content_virtual_slice(0.0, self.browse_view_h);
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
                    if cats.is_empty() {
                        self.bundle
                            .group_names()
                            .into_iter()
                            .take(CAT_PAGE)
                            .map(|n| (n.clone(), n))
                            .collect()
                    } else {
                        cats.into_iter()
                            .take(CAT_PAGE)
                            .map(|c| (c.name.clone(), c.name.clone()))
                            .collect()
                    }
                };
                let all_active = self.selected_group.is_none();
                self.cat_entries.push((
                    "*".into(),
                    "Toutes".into(),
                    all_active,
                    self.bundle.channels.len(),
                ));
                for (id, name) in cats {
                    let active = self.selected_group.as_deref() == Some(name.as_str());
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
                let all_active = matches!(self.selected_vod_category.as_deref(), None | Some("*"));
                self.cat_entries.push((
                    "*".into(),
                    "All".into(),
                    all_active,
                    self.bundle.vod.len(),
                ));
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
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect();
                let all_active =
                    matches!(self.selected_series_category.as_deref(), None | Some("*"));
                self.cat_entries.push((
                    "*".into(),
                    "All".into(),
                    all_active,
                    self.bundle.series.len(),
                ));
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
                let rows = self.browse_index.len().div_ceil(cols);
                let s = browser::virtual_slice(
                    scroll_y,
                    view_h,
                    browser::mosaic_row_height(m.tile_w),
                    rows,
                    overscan,
                );
                (s.start, s.end)
            }
            Tab::Series if self.series_detail.is_none() => {
                let cols = m.cols.max(1);
                let rows = self.browse_index.len().div_ceil(cols);
                let s = browser::virtual_slice(
                    scroll_y,
                    view_h,
                    browser::mosaic_row_height(m.tile_w),
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

    fn visible_browse_window(&self, cols: usize, tile_w: f32) -> std::ops::Range<usize> {
        self.art_prefetch_window(cols, tile_w)
    }

    /// Visible mosaic/list indices plus velocity-based lookahead for fast flings.
    fn art_prefetch_window(&self, cols: usize, tile_w: f32) -> std::ops::Range<usize> {
        let cols = cols.max(1);
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
                let rows = self.browse_index.len().div_ceil(cols);
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
                let end_i = (end.saturating_mul(cols)).min(self.browse_index.len());
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

    fn flush_pending_images(&mut self) -> Task<Message> {
        if self.pending_images.is_empty() {
            return Task::none();
        }
        let batch = std::mem::take(&mut self.pending_images);
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
        }
        Task::none()
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
            return Task::none();
        }
        for url in urls {
            if tasks.len() >= room {
                break;
            }
            if let crate::images::RequestOutcome::Fetch { url: u, source_id } =
                self.images.request(&url, None)
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
            let (d, t) = self.jobs.images.snapshot();
            self.jobs.images.set_total(t.saturating_add(n).max(d + n));
            Task::batch(tasks)
        }
    }

    /// Prefetch posters for the current browse selection at profile load/reload.
    /// Walks `browse_index` in order (what the user will scroll) — not catalog tip.
    fn prefetch_catalog_art(&mut self) -> Task<Message> {
        self.art_warm_cursor = 0;
        self.art_warm_active = !self.browse_index.is_empty();
        self.refresh_browse_art()
    }

    /// Visible window + background warm of the current filter.
    fn refresh_browse_art(&mut self) -> Task<Message> {
        Task::batch([self.prefetch_visible_art(), self.pump_art_warm()])
    }

    fn continue_art_warm(&mut self) -> Task<Message> {
        if !self.art_warm_active || self.browse_flinging() {
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
            Tab::Live | Tab::Favorites => {
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
                crate::images::pick_art(None, v.poster.as_deref(), None, None)
            }
            Tab::Series if self.series_detail.is_none() => {
                let s = self.bundle.series.get(i)?;
                crate::images::pick_art(None, None, s.cover.as_deref(), s.banner.as_deref())
            }
            _ => None,
        }
    }

    fn art_url_source_at(&self, pos: usize) -> Option<Uuid> {
        let i = self.browse_index.get(pos)?;
        match self.tab {
            Tab::Live | Tab::Favorites => self.bundle.channels.get(i).and_then(|c| c.source_id),
            Tab::Vod if self.vod_detail.is_none() => {
                self.bundle.vod.get(i).and_then(|v| v.source_id)
            }
            Tab::Series if self.series_detail.is_none() => {
                self.bundle.series.get(i).and_then(|s| s.source_id)
            }
            _ => None,
        }
    }

    /// Fill inflight slots from browse_index[art_warm_cursor..] until warm target.
    fn pump_art_warm(&mut self) -> Task<Message> {
        if !self.art_warm_active {
            return Task::none();
        }
        let target = self.art_warm_target();
        if self.art_warm_cursor >= target {
            self.art_warm_active = false;
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
        if self.art_warm_cursor >= target {
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
        let mut urls = Vec::new();
        let m = self.layout_metrics();
        let win = self.visible_browse_window(m.cols, m.tile_w);
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
                        if let Some(u) =
                            crate::images::pick_art(None, v.poster.as_deref(), None, None)
                        {
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
                        if let Some(u) = crate::images::pick_art(
                            None,
                            None,
                            s.cover.as_deref(),
                            s.banner.as_deref(),
                        ) {
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
        *self.session.native.options_mut() = play_options_from(&self.settings);
    }

    fn apply_source_headers_for(&mut self, ch: &Channel) {
        let opts = self.session.native.options_mut();
        *opts = play_options_from(&self.settings);
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

    fn fetch_epg_for_visible_task(&self) -> Task<Message> {
        let ids: Vec<String> = self
            .bundle
            .live_in_group(self.selected_group.as_deref())
            .into_iter()
            .take(12)
            .map(|c| c.id.clone())
            .collect();
        self.fetch_epg_for_ids(ids)
    }

    fn fetch_epg_for_ids(&self, ids: Vec<String>) -> Task<Message> {
        if ids.is_empty() {
            return Task::none();
        }
        // Prefer an enabled Xtream source that owns these channels.
        let src = self
            .sources
            .iter()
            .find(|s| s.enabled && s.kind == SourceKind::Xtream)
            .cloned();
        let Some(src) = src else {
            return Task::none();
        };
        Task::perform(
            async move { fluxplay_providers::fetch_short_epg(&src, &ids).await },
            Message::EpgFetched,
        )
    }

    fn pick_autoplay_channel(&self) -> Option<Channel> {
        let group = self.selected_group.as_deref();
        self.bundle
            .live_in_group(group)
            .into_iter()
            .find(|c| !is_adult_cat(c.group.as_deref().unwrap_or("")))
            .cloned()
            .or_else(|| self.bundle.channels.first().cloned())
    }

    fn xtream_source(&self) -> Option<MediaSource> {
        self.sources
            .iter()
            .find(|s| s.enabled && s.kind == SourceKind::Xtream)
            .cloned()
    }

    fn load_vod_category_task(&self, category_id: String) -> Task<Message> {
        let Some(src) = self.xtream_source() else {
            return Task::none();
        };
        let cid = category_id.clone();
        Task::perform(
            async move {
                fluxplay_providers::load_xtream_vod_category(&src, &cid)
                    .await
                    .map_err(|e| e.to_string())
            },
            move |result| Message::VodCategoryLoaded {
                category_id,
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
                let rows = self.browse_index.len().div_ceil(cols);
                browser::virtual_content_height(rows, view_h, browser::mosaic_row_height(m.tile_w))
            }
            Tab::Series if self.series_detail.is_none() => {
                let cols = m.cols.max(1);
                let rows = self.browse_index.len().div_ceil(cols);
                browser::virtual_content_height(rows, view_h, browser::mosaic_row_height(m.tile_w))
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
        let Some(src) = self.xtream_source() else {
            return Task::none();
        };
        let cid = category_id.clone();
        Task::perform(
            async move {
                fluxplay_providers::load_xtream_series_category(&src, &cid)
                    .await
                    .map_err(|e| e.to_string())
            },
            move |result| Message::SeriesCategoryLoaded {
                category_id,
                result,
            },
        )
    }

    /// Persist portal dump: checksum short-circuit + enrichment-preserving upsert.
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
        let ids: Vec<Uuid> = self.sources.iter().map(|s| s.id).collect();
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
                    crate::catalog_db::load_bundle_blocking(&ids)
                })
                .await
                .unwrap_or_else(|e| Err(e))
            },
            Message::BundleCacheReady,
        )
    }

    fn apply_bundle_cache(&mut self, bundle: PlaylistBundle) {
        self.bundle = bundle;
        tracing::info!(
            channels = self.bundle.channels.len(),
            vod = self.bundle.vod.len(),
            series = self.bundle.series.len(),
            "rebuilt bundle from catalog db"
        );
    }

    fn arm_series_queue_for_url(&mut self, current_url: &str) {
        self.series_queue.clear();
        self.series_queue_idx = 0;
        let Some(detail) = &self.series_detail else {
            return;
        };
        for season in &detail.seasons {
            for ep in &season.episodes {
                self.series_queue.push((
                    format!("{} — {}", detail.name, ep.title),
                    ep.stream_url.clone(),
                ));
            }
        }
        if let Some(i) = self
            .series_queue
            .iter()
            .position(|(_, u)| u == current_url)
        {
            self.series_queue_idx = i;
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
        let ua = play_options_from(&self.settings)
            .user_agent
            .unwrap_or_else(|| "IPTVSmartersPlayer".into());
        Some(Task::perform(
            async move { prefetch_episode_file(next, ua).await },
            Message::PrefetchDone,
        ))
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
                container(browser::mode_top_nav_ex(
                    ui,
                    m.rail_size,
                    m.nav_strip,
                    tab_items,
                ))
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

        // Android: dynamic WindowInsets (fallback 56dp bottom).
        #[cfg(target_os = "android")]
        let (top_inset, bottom_inset) = (self.system_insets.1, self.system_insets.3.max(24.0));
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
        let backend_label = self
            .session
            .backend
            .map(|b| b.label())
            .unwrap_or("—");

        player_ui::player_window(player_ui::PlayerChrome {
            ui,
            title,
            meta,
            status: &self.status,
            session: &self.session,
            art,
            video: self.video_frame.as_ref(),
            active,
            panel: self.player_panel,
            goto_draft: &self.goto_draft,
            sleep_mins: self.sleep_mins,
            pip: self.pip_mode,
            chrome_h: self.layout_metrics().player_chrome_h,
            chrome_visible: self.player_chrome_visible,
            chrome_alpha: self.chrome_alpha,
            fullscreen: self.player_fullscreen,
            embedded_video,
            backend_label,
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
            .unwrap_or(2500);
        if at.elapsed() >= std::time::Duration::from_millis(idle_ms)
            && self.player_chrome_visible
        {
            self.player_chrome_visible = false;
            // Stamp hide time so wake events right after hide are ignored (Wayland
            // enter/leave spam when the overlay tree changes).
            self.player_pointer_at = Some(std::time::Instant::now());
        }
    }

    fn playlist_neighbor(&self, delta: i32) -> Option<Channel> {
        let cur = self.session.channel.as_ref()?;
        let list: Vec<&Channel> = self
            .bundle
            .channels
            .iter()
            .filter(|c| {
                if let Some(g) = &self.selected_group {
                    c.group.as_deref() == Some(g.as_str())
                } else {
                    true
                }
            })
            .collect();
        if list.is_empty() {
            return None;
        }
        let idx = list.iter().position(|c| c.id == cur.id).unwrap_or(0) as i32;
        let n = list.len() as i32;
        let next = (idx + delta).rem_euclid(n) as usize;
        Some(list[next].clone())
    }

    fn filtered_categories(
        &self,
        kind: ContentKind,
    ) -> Vec<&fluxplay_core::models::Category> {
        let q = self.cat_filter.to_ascii_lowercase();
        self.bundle
            .categories
            .iter()
            .filter(|c| c.content == kind)
            .filter(|c| q.is_empty() || c.name.to_ascii_lowercase().contains(&q))
            .collect()
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
            // Never scan EPG on the browse paint path — it blocks scroll.
            for bi in self.browse_index.window(slice.start, slice.end) {
                let Some(ch) = self.bundle.channels.get(bi) else {
                    continue;
                };
                let fav = self.settings.is_favorite(&ch.id);
                let subtitle = ch.group.clone().unwrap_or_else(|| "Live".into());
                let thumb = ch
                    .logo
                    .as_deref()
                    .and_then(|u| self.images.get(u));
                rows.push(browser::media_row(
                    ch.name.clone(),
                    subtitle,
                    Message::PlayChannelId(ch.id.clone()),
                    Some((fav, Message::ToggleFavorite(ch.id.clone()))),
                    ui,
                    self.selected_channel.as_deref() == Some(ch.id.as_str()),
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

        let header = browser::content_header(
            ui,
            self.selected_group
                .clone()
                .unwrap_or_else(|| "Toutes les chaînes".into()),
            format!("{total} chaînes · scroll virtuel"),
            &self.search,
            m.search_w,
            m.title_size,
            m.stack_header,
        );

        let scroll_id = format!(
            "flux-live-{}",
            self.selected_group.as_deref().unwrap_or("*")
        );
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
        let n_rows = total.div_ceil(cols);
        let row_h = browser::mosaic_row_height(tile_w);
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            n_rows,
            self.browse_overscan(),
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
                    if idx >= total {
                        break;
                    }
                    let Some(v) = self.browse_index.get(idx).and_then(|i| self.bundle.vod.get(i))
                    else {
                        continue;
                    };
                    // Always paint titles + cached art (blanking during fling looked broken).
                    let meta = Self::mosaic_meta_line(
                        v.year.as_deref(),
                        v.genre.as_deref(),
                        v.rating.as_deref(),
                        "Film",
                    );
                    let thumb = crate::images::pick_art(None, v.poster.as_deref(), None, None)
                        .and_then(|u| self.images.get(&u));
                    r = r.push(browser::mosaic_tile(
                        v.name.clone(),
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
        }

        let selected = self.selected_vod_category.as_deref();
        let cat_name = if matches!(selected, None | Some("*")) {
            "All".into()
        } else {
            self.bundle
                .categories
                .iter()
                .find(|c| Some(c.id.as_str()) == selected)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "VOD".into())
        };

        let header = browser::content_header(
            ui,
            cat_name,
            format!("{total} titres · scroll virtuel"),
            &self.search,
            m.search_w,
            m.title_size,
            m.stack_header,
        );
        let scroll_id = format!(
            "flux-vod-{}",
            self.selected_vod_category.as_deref().unwrap_or("*")
        );
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
        let content = browser::pane(
            ui,
            Length::Fill,
            column![header, scroller]
                .spacing(m.gap)
                .width(Fill)
                .height(Fill),
        );
        self.with_categories(ui, m, "Films / VOD", &self.cat_entries, content)
    }

    fn view_vod_detail(
        &self,
        ui: UiTheme,
        m: crate::theme::LayoutMetrics,
        detail: &VodItem,
    ) -> Element<'_, Message> {
        let poster = crate::images::pick_art(None, detail.poster.as_deref(), None, None)
            .and_then(|u| self.images.get(&u));
        let imdb_query = detail
            .imdb_id
            .clone()
            .unwrap_or_else(|| detail.name.clone());
        browser::media_detail_page(
            ui,
            &detail.name,
            poster,
            detail.year.as_deref(),
            detail.genre.as_deref(),
            detail.rating.as_deref(),
            detail.runtime.as_deref(),
            detail.rated.as_deref(),
            detail.plot.as_deref(),
            detail.actors.as_deref(),
            detail.director.as_deref(),
            detail.writer.as_deref(),
            detail.imdb_id.as_deref(),
            imdb_query,
            Some("▶ Lire le film".into()),
            Some(Message::PlayVod {
                name: detail.name.clone(),
                url: detail.stream_url.clone(),
                kind: ContentKind::Vod,
                poster: detail.poster.clone(),
            }),
            Message::CloseVodDetail,
            None,
            m.bp.is_narrow(),
            self.detail_meta_loading,
        )
    }

    fn view_series_detail(
        &self,
        ui: UiTheme,
        m: crate::theme::LayoutMetrics,
        detail: &SeriesItem,
    ) -> Element<'_, Message> {
        let poster = crate::images::pick_art(
            None,
            None,
            detail.cover.as_deref(),
            detail.banner.as_deref(),
        )
        .and_then(|u| self.images.get(&u));
        let first_play = detail
            .seasons
            .iter()
            .flat_map(|s| s.episodes.iter().map(move |ep| (s.season_number, ep)))
            .next()
            .map(|(season_num, ep)| {
                (
                    format!("▶ Lire · S{season_num}E{}", ep.episode_num),
                    Message::PlayVod {
                        name: format!("{} — {}", detail.name, ep.title),
                        url: ep.stream_url.clone(),
                        kind: ContentKind::Series,
                        poster: detail.cover.clone().or(detail.banner.clone()),
                    },
                )
            });
        let (play_label, play_msg) = match first_play {
            Some((l, m)) => (Some(l), Some(m)),
            None => (None, None),
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
                    eps_rows.push(
                        text(format!("Saison {n}"))
                            .size(14)
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
                    let sub = match ep.plot.as_deref().filter(|s| !s.is_empty()) {
                        Some(p) => {
                            let mut it = p.chars();
                            let short: String = it.by_ref().take(90).collect();
                            if it.next().is_some() {
                                format!("E{} · {short}…", ep.episode_num)
                            } else {
                                format!("E{} · {short}", ep.episode_num)
                            }
                        }
                        None => format!("Épisode {}", ep.episode_num),
                    };
                    eps_rows.push(browser::media_row(
                        ep.title.clone(),
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
                        m.content_w.max(120.0),
                    ));
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
            .unwrap_or_else(|| detail.name.clone());
        browser::media_detail_page(
            ui,
            &detail.name,
            poster,
            detail.year.as_deref(),
            detail.genre.as_deref(),
            detail.rating.as_deref(),
            detail.runtime.as_deref(),
            detail.rated.as_deref(),
            detail.plot.as_deref(),
            detail.actors.as_deref(),
            detail.director.as_deref(),
            detail.writer.as_deref(),
            detail.imdb_id.as_deref(),
            imdb_query,
            play_label,
            play_msg,
            Message::CloseSeriesDetail,
            episodes,
            m.bp.is_narrow(),
            self.detail_meta_loading,
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
        let n_rows = total.div_ceil(cols);
        let row_h = browser::mosaic_row_height(tile_w);
        let slice = browser::virtual_slice(
            self.browse_scroll_y,
            self.browse_view_h,
            row_h,
            n_rows,
            self.browse_overscan(),
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
                    let meta = Self::mosaic_meta_line(
                        s.year.as_deref(),
                        s.genre.as_deref(),
                        s.rating.as_deref(),
                        "Série",
                    );
                    let thumb =
                        crate::images::pick_art(None, None, s.cover.as_deref(), s.banner.as_deref())
                            .and_then(|u| self.images.get(&u));
                    r = r.push(browser::mosaic_tile(
                        s.name.clone(),
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
        }

        let selected = self.selected_series_category.as_deref();
        let cat_name = if matches!(selected, None | Some("*")) {
            "All".into()
        } else {
            self.bundle
                .categories
                .iter()
                .find(|c| Some(c.id.as_str()) == selected)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "Séries".into())
        };
        let header = browser::content_header(
            ui,
            cat_name,
            format!("{total} séries · scroll virtuel"),
            &self.search,
            m.search_w,
            m.title_size,
            m.stack_header,
        );
        let scroll_id = format!(
            "flux-series-{}",
            self.selected_series_category.as_deref().unwrap_or("*")
        );
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
        let content = browser::pane(
            ui,
            Length::Fill,
            column![header, scroller]
                .spacing(m.gap)
                .width(Fill)
                .height(Fill),
        );
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
            for bi in self.browse_index.window(slice.start, slice.end) {
                let Some(ch) = self.bundle.channels.get(bi) else {
                    continue;
                };
                rows.push(browser::media_row(
                    ch.name.clone(),
                    ch.group.clone().unwrap_or_else(|| "Live".into()),
                    Message::PlayChannelId(ch.id.clone()),
                    Some((true, Message::ToggleFavorite(ch.id.clone()))),
                    ui,
                    self.selected_channel.as_deref() == Some(ch.id.as_str()),
                    None,
                    0.0,
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
                    format!("{total} chaînes · scroll virtuel"),
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
            text(profile.notes).size(12).color(ui.ink_muted()),
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
            ]
            .spacing(10)
            .align_y(Alignment::Center),
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
            text("3. Material 3 Expressive")
                .size(13)
                .color(ui.ink()),
            {
                #[cfg(target_os = "android")]
                {
                    text(
                        "Android : profondeur tonale (surface containers), icônes raster PNG, \
                         pas d’ombres d’élévation desktop — contour outline_variant pour le relief GLES.",
                    )
                    .size(12)
                    .color(ui.ink_muted())
                }
                #[cfg(not(target_os = "android"))]
                {
                    text(
                        "Tokens branchés : rôles typo, dock asymétrique, motion tick, sélection secondary \
                         (chips / rail) — pas du marketing vapor.",
                    )
                    .size(12)
                    .color(ui.ink_muted())
                }
            },
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
            section(
                "5. Affichage & FPS",
                "Plafonds dérivés du moniteur + GPU (Auto), ou forçage manuel. Env : FLUXPLAY_GUI_FPS / FLUXPLAY_VIDEO_FPS / FLUXPLAY_MONITOR_HZ.",
            ),
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
        let channels: Vec<&Channel> = if let Some(id) = &self.selected_channel {
            self.bundle
                .channels
                .iter()
                .filter(|c| &c.id == id)
                .collect()
        } else {
            self.bundle
                .live_in_group(self.selected_group.as_deref())
                .into_iter()
                .take(40)
                .collect()
        };

        if channels.is_empty() || self.bundle.epg.is_empty() {
            items = items.push(browser::empty_hint(
                ui,
                "Ouvrez Live, choisissez une catégorie, puis revenez ici — ou lancez une chaîne.",
            ));
        }

        for ch in channels {
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
                ch.name.clone(),
                format!("{cur_s}  {next_s}"),
                Message::PlayChannelId(ch.id.clone()),
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
                m.thumb,
                m.content_w.max(120.0),
            ));
        }

        browser::pane(
            ui,
            Length::Fill,
            column![
                browser::content_header(
                    ui,
                    "Guide EPG".into(),
                    format!("{} programmes en cache", self.bundle.epg.len()),
                    &self.search,
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

        let form = column![
            text("Nouvelle source").size(16),
            kind_row,
            field("Nom", &self.form_name, Message::FormName, Some(PasteTarget::FormName), ui),
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
            field(
                "EPG XMLTV (optionnel)",
                &self.form_epg,
                Message::FormEpg,
                Some(PasteTarget::FormEpg),
                ui,
            ),
            row![
                pill_button_primary(
                    crate::icons::icon_label(crate::icons::Icon::Add, "Ajouter", 14.0, ui.on_primary()),
                    Message::AddSource,
                    ui,
                ),
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
            let meta = format!("{} · {} · {}", s.kind.label(), state, redact_endpoint(&s.endpoint));
            list = list.push(browser::source_card(
                s.name.clone(),
                meta,
                Message::ReloadSource(s.id),
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

fn sanitize_sources(sources: &mut Vec<MediaSource>) {
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

fn build_browse_index_blocking(
    tab: Tab,
    q: String,
    group: Option<String>,
    vod_cat: Option<String>,
    series_cat: Option<String>,
    source_ids: Vec<Uuid>,
    live_rows: Vec<(usize, String, Option<String>)>,
    vod_rows: Vec<(usize, String, String, Option<String>)>,
    series_rows: Vec<(usize, String, String, Option<String>)>,
    progress: std::sync::Arc<crate::async_jobs::JobProgress>,
) -> (BrowseIndex, Vec<EpisodeFlat>) {
    use crate::async_jobs::DEFAULT_CHUNK;
    let q_lc = if q.is_empty() {
        None
    } else {
        Some(q.to_ascii_lowercase())
    };
    let index = match tab {
        Tab::Live => {
            progress.set_total(live_rows.len() as u64);
            let mut out = Vec::with_capacity(live_rows.len().min(65_536));
            for chunk in live_rows.chunks(DEFAULT_CHUNK) {
                for (i, name, g) in chunk {
                    let group_ok = match group.as_deref() {
                        None | Some("Tous") | Some("*") => true,
                        Some(want) => g.as_deref() == Some(want),
                    };
                    let q_ok = q_lc
                        .as_deref()
                        .map(|ql| name.to_ascii_lowercase().contains(ql))
                        .unwrap_or(true);
                    if group_ok && q_ok {
                        out.push(*i);
                    }
                }
                progress.add_done(chunk.len() as u64);
            }
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
                        let found = db.search_vod(&q, cat, 50_000);
                        let want: std::collections::HashSet<&str> =
                            found.iter().map(|v| v.id.as_str()).collect();
                        progress.set_total(vod_rows.len() as u64);
                        let mut out = Vec::with_capacity(found.len());
                        for chunk in vod_rows.chunks(DEFAULT_CHUNK) {
                            for (i, id, _name, c) in chunk {
                                let cat_ok = cat
                                    .map(|cid| c.as_deref() == Some(cid))
                                    .unwrap_or(true);
                                if cat_ok && want.contains(id.as_str()) {
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
                    let cat_ok = cat.map(|cid| c.as_deref() == Some(cid)).unwrap_or(true);
                    let q_ok = q_lc
                        .as_deref()
                        .map(|ql| {
                            if name.is_empty() {
                                true
                            } else {
                                name.to_ascii_lowercase().contains(ql)
                            }
                        })
                        .unwrap_or(true);
                    if cat_ok && q_ok {
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
                    let found = db.search_series(&q, cat, 50_000);
                    let want: std::collections::HashSet<&str> =
                        found.iter().map(|s| s.id.as_str()).collect();
                    progress.set_total(series_rows.len() as u64);
                    let mut out = Vec::with_capacity(found.len());
                    for chunk in series_rows.chunks(DEFAULT_CHUNK) {
                        for (i, id, _name, c) in chunk {
                            let cat_ok = cat
                                .map(|cid| c.as_deref() == Some(cid))
                                .unwrap_or(true);
                            if cat_ok && want.contains(id.as_str()) {
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
                    let cat_ok = cat.map(|cid| c.as_deref() == Some(cid)).unwrap_or(true);
                    let q_ok = q_lc
                        .as_deref()
                        .map(|ql| {
                            if name.is_empty() {
                                true
                            } else {
                                name.to_ascii_lowercase().contains(ql)
                            }
                        })
                        .unwrap_or(true);
                    if cat_ok && q_ok {
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
    PlayOptions {
        user_agent: Some("IPTVSmartersPlayer".into()),
        referer: None,
        extra_headers: Vec::new(),
        hwdec: settings.hwdec,
        cache_ms: settings.cache_ms,
        demux_secs: settings.demux_secs,
        volume: settings.volume,
        low_latency: settings.low_latency,
        preferred,
        http_proxy: crate::wg_tunnel::socks_proxy_url(),
    }
}

async fn load_one(src: MediaSource) -> Result<PlaylistBundle, String> {
    fluxplay_providers::load_source_with_epg(&src)
        .await
        .map_err(|e| e.to_string())
}

/// Warm next episode bytes to disk (cap ~48 MiB) so resume is non-blocking.
async fn prefetch_episode_file(url: String, ua: String) -> Result<String, String> {
    let dest_dir = crate::storage::data_dir().join("prefetch");
    tokio::fs::create_dir_all(&dest_dir)
        .await
        .map_err(|e| e.to_string())?;
    let name = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(url.as_bytes());
        format!("{:x}.bin", h.finalize())
    };
    let path = dest_dir.join(name);
    if path.is_file() {
        return Ok(path.display().to_string());
    }
    // Same DNS/proxy stack as catalog HTTP (Custom/DoH/DoT + SOCKS when up).
    let client = fluxplay_providers::app_http(&ua, 90).map_err(|e| e.to_string())?;
    let mut resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let mut out = tokio::fs::File::create(&path)
        .await
        .map_err(|e| e.to_string())?;
    let mut written = 0u64;
    const CAP: u64 = 48 * 1024 * 1024;
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        written += chunk.len() as u64;
        if written > CAP {
            break;
        }
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    out.flush().await.map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

fn merge_xtream_vod_fields(existing: &mut VodItem, item: &VodItem) {
    if item.plot.is_some() {
        existing.plot = item.plot.clone();
    }
    if item.actors.is_some() {
        existing.actors = item.actors.clone();
    }
    if item.director.is_some() {
        existing.director = item.director.clone();
    }
    if item.writer.is_some() {
        existing.writer = item.writer.clone();
    }
    if item.genre.is_some() {
        existing.genre = item.genre.clone();
    }
    if item.year.is_some() {
        existing.year = item.year.clone();
    }
    if item.rating.is_some() {
        existing.rating = item.rating.clone();
    }
    if item.runtime.is_some() {
        existing.runtime = item.runtime.clone();
    }
    if item.imdb_id.is_some() {
        existing.imdb_id = item.imdb_id.clone();
    }
    if item.poster.is_some() && existing.poster.is_none() {
        existing.poster = item.poster.clone();
    }
    if item.rated.is_some() {
        existing.rated = item.rated.clone();
    }
    if item.language.is_some() {
        existing.language = item.language.clone();
    }
    if item.country.is_some() {
        existing.country = item.country.clone();
    }
    if item.awards.is_some() {
        existing.awards = item.awards.clone();
    }
}

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

fn redact_endpoint(endpoint: &str) -> String {
    if endpoint.len() > 64 {
        format!("{}…", &endpoint[..64])
    } else if endpoint.contains("#EXTM3U") {
        "playlist inline".into()
    } else {
        endpoint.to_string()
    }
}


fn profile_message_label(message: &Message) -> &'static str {
    match message {
        Message::PlayerTick => "tick.player",
        Message::VideoFrameAllocated(_) => "tick.video_frame",
        Message::PlayerChromeTick => "tick.chrome",
        Message::ImageLoaded(_) => "async.image",
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
        Message::SourceLoaded { .. } => "async.source",
        Message::SourcesBatchLoaded(_) => "async.sources_batch",
        Message::BundleCacheReady(_) => "async.bundle_cache",
        Message::CatalogIngestDone(_) => "async.catalog_ingest",
        Message::CatalogIngestOneDone { .. } => "async.catalog_ingest_one",
        Message::BrowseIndexReady { .. } => "async.browse_index",
        Message::EpgFetched(_) => "async.epg",
        Message::PrefetchDone(_) => "async.prefetch",
        Message::DiagnoseDone(_) => "async.diagnose",
        Message::ClipboardText(_, _) => "async.clipboard",
        Message::PlaylistFilePicked(_) => "async.file_pick",
        Message::BrowseScrolled(_, _) => "scroll.browse",
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

fn ui_action_label(message: &Message) -> Option<&'static str> {
    Some(match message {
        Message::PlayerTick
        | Message::VideoFrameAllocated(_)
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
        Message::Tab(_) => "nav.onglet",
        Message::CatFilterChanged(_) => "nav.filtre",
        #[cfg(target_os = "android")]
        Message::NavBack => "nav.retour",
        #[cfg(target_os = "android")]
        Message::BrowseFocusDelta(_) => "nav.browse_focus",
        #[cfg(target_os = "android")]
        Message::BrowseActivate => "nav.browse_activate",
        Message::PlayChannel(_) | Message::PlayChannelId(_) => "lecteur.play_chaine",
        Message::PlayVod { .. } => "lecteur.play_vod",
        Message::Stop => "lecteur.stop",
        Message::TogglePause => "lecteur.pause",
        Message::ToggleMute => "lecteur.mute",
        Message::VolumeChanged(_) => "lecteur.volume",
        Message::SeekRel(_) => "lecteur.seek_rel",
        Message::SeekPercent(_) => "lecteur.seek_pct",
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
        Message::AddSource | Message::AddPublicDemo { .. } => "sources.ajout",
        Message::RemoveSource(_) => "sources.suppr",
        Message::ReloadSource(_) => "sources.reload",
        Message::OpenExternal => "lecteur.externe",
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
        Message::LoadMore => "nav.load_more",
        Message::ClosePlayerWindow => "lecteur.fermer",
        Message::PlayerHotkey(_) => "lecteur.hotkey",
        Message::MainWindowOpened(_) => "fenetre.main_open",
        Message::PlayerWindowOpened(_) => "fenetre.player_open",
        Message::WindowClosed(_) => "fenetre.close",
        _ => "ui.autre",
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
    _id: window::Id,
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
        Key::Character(c) => match c.as_str() {
            "m" | "M" => PlayerHotkey::Mute,
            "f" | "F" => PlayerHotkey::Fullscreen,
            "r" | "R" => PlayerHotkey::Restart,
            "[" => PlayerHotkey::Speed,
            "l" | "L" => PlayerHotkey::Loop,
            "s" | "S" if modifiers.control() => PlayerHotkey::Screenshot,
            "." => PlayerHotkey::FrameStep,
            _ => return None,
        },
        _ => return None,
    };
    let _ = Modifiers::empty();
    Some(Message::PlayerHotkey(hk))
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

