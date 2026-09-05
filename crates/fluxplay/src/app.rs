use chrono::Local;
use fluxplay_core::models::{
    AccentPreset, AppSettings, Channel, ContentKind, MediaSource, PlaylistBundle, SeriesItem,
    SourceKind, ThemeMode, VodItem,
};
use fluxplay_player::{
    detect_backends, target_profile, PlayOptions, PlaybackState, StreamSession, VideoRect,
};
use iced::widget::{
    button, column, container, row, text, text_input, Column, Row, Space,
};
use iced::widget::image::Handle as ImageHandle;
use iced::window;
use iced::{
    Alignment, Background, Border, Element, Fill, Length, Padding, Point, Size, Subscription, Task,
    Theme,
};
use uuid::Uuid;

use crate::browser::{CAT_PAGE, LIST_PAGE};
use crate::player_ui::PlayerPanel;
use crate::theme::{
    LayoutMetrics, UiTheme, RADIUS_FULL, RADIUS_MD, PLAYER_PAD,
};
use crate::{browser, demo, player_ui, storage};
use iced::event::{self, Event};
use iced::keyboard::{self, Key, Modifiers};
use iced::keyboard::key::Named;

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

    iced::application(FluxPlay::new, FluxPlay::update, FluxPlay::view_android)
        .title(FluxPlay::title_android)
        .theme(FluxPlay::theme_android)
        .subscription(FluxPlay::subscription)
        .antialiasing(false)
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
            Self::Series => "Séries",
            Self::Epg => "EPG",
            Self::Sources => "Sources",
            Self::Settings => "Régl.",
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
    /// Last known outer position of the player window (legacy CLI overlay sizing).
    player_pos: Option<Point>,
    /// Embedded video frame (libmpv software render → iced image).
    video_frame: Option<ImageHandle>,
    video_frame_wh: (u32, u32),
    player_panel: PlayerPanel,
    goto_draft: String,
    sleep_until: Option<std::time::Instant>,
    sleep_mins: Option<u32>,
    pip_mode: bool,
    /// True when the player window is in OS fullscreen mode.
    player_fullscreen: bool,
    /// Overlay dock / sheets visible (auto-hides on pointer idle).
    player_chrome_visible: bool,
    player_pointer_at: Option<std::time::Instant>,
    /// Series episode URLs for next-episode prefetch (current index in list).
    series_queue: Vec<(String, String)>,
    series_queue_idx: usize,
    prefetch_armed_for: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Tab(Tab),
    SearchChanged(String),
    PlayChannel(Channel),
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
    AddSource,
    AddPublicDemo { name: String, endpoint: String },
    RemoveSource(Uuid),
    ReloadSource(Uuid),
    SourceLoaded {
        source_id: Uuid,
        result: Result<PlaylistBundle, String>,
    },
    SourcesBatchLoaded(Vec<(Uuid, Result<PlaylistBundle, String>)>),
    OpenExternal,
    PickPlaylistFile,
    PlaylistFilePicked(Option<String>),
    ToggleFavorite(String),
    CycleBackend,
    ToggleHwdec,
    ToggleLowLatency,
    TogglePrefetchNext,
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
    OpenVodDetail(VodItem),
    CloseVodDetail,
    DetailMetaLoaded {
        is_series: bool,
        id: String,
        patch: Option<crate::metadata::MetaPatch>,
    },
    CatFilterChanged(String),
    SelectBrowseCategory(String),
    LoadMore,
    ImageLoaded(Result<(String, Vec<u8>), (String, String)>),
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
                true
            }
            #[cfg(not(target_os = "android"))]
            {
                matches!(dark_light::detect(), Ok(dark_light::Mode::Dark))
            }
        };

        let catalog_db = crate::catalog_db::CatalogDb::open();
        let bundle = catalog_db
            .as_ref()
            .and_then(|db| db.load_bundle().ok())
            .unwrap_or_default();
        let (ch_n, vod_n, ser_n) = catalog_db
            .as_ref()
            .map(|db| db.counts())
            .unwrap_or((0, 0, 0));
        let has_real = sources.iter().any(|s| !demo::is_demo(s));
        let sync_fresh = catalog_db
            .as_ref()
            .map(|db| db.is_sync_fresh())
            .unwrap_or(false);
        let status = if has_real && sync_fresh {
            format!("Offline-ready · {ch_n} live · {vod_n} VOD · {ser_n} séries (cache chaud)")
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
                size: Size::new(1280.0, 860.0),
                position: window::Position::Centered,
                exit_on_close_request: true,
                ..Default::default()
            });
            (Some(id), open)
        };
        let session = StreamSession::with_options(opts);
        let app = Self {
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
            loading: !sync_fresh && has_real,
            form_name: String::new(),
            form_kind: SourceKind::M3uPlus,
            form_endpoint: String::new(),
            form_user: String::new(),
            form_pass: String::new(),
            form_mac: String::new(),
            form_epg: String::new(),
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
            main_size: Size::new(1280.0, 720.0),
            player_pos: None,
            video_frame: None,
            video_frame_wh: (0, 0),
            player_panel: PlayerPanel::None,
            goto_draft: String::new(),
            sleep_until: None,
            sleep_mins: None,
            pip_mode: false,
            player_fullscreen: false,
            player_chrome_visible: true,
            player_pointer_at: None,
            series_queue: Vec::new(),
            series_queue_idx: 0,
            prefetch_armed_for: None,
        };

        // Offline-first: skip portal storm when SQLite catalog is still fresh.
        let mut boot = Vec::new();
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
        if sync_fresh {
            boot.push(app.prefetch_visible_art_boot());
            boot.push(app.enrich_metadata_task());
        } else if has_real || !app.sources.is_empty() {
            boot.push(app.reload_all_task());
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

    /// Prefetch without &mut self (boot path) — only schedules known URLs after load.
    fn prefetch_visible_art_boot(&self) -> Task<Message> {
        // Lightweight: art loads on first paint via Tab/LoadMore; avoid duplicate storms.
        Task::none()
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

    fn ui_theme(&self) -> UiTheme {
        UiTheme::new(self.is_day(), self.settings.accent)
    }

    fn theme(&self) -> Theme {
        self.ui_theme().iced_theme()
    }

    /// Responsive chrome metrics from the current main window size.
    fn layout_metrics(&self) -> LayoutMetrics {
        LayoutMetrics::compute(self.main_size.width, self.main_size.height)
    }

    /// Category sidebar or phone chips + content pane.
    fn with_categories<'a>(
        &'a self,
        ui: UiTheme,
        m: LayoutMetrics,
        title: &'a str,
        entries: Vec<(String, String, bool, usize)>,
        content: Element<'a, Message>,
    ) -> Element<'a, Message> {
        if m.cat_w <= 1.0 {
            column![
                browser::category_chips(ui, &self.cat_filter, entries),
                content,
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill)
            .into()
        } else {
            row![
                browser::category_sidebar(ui, m.cat_w, title, &self.cat_filter, entries),
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
        let tick = if matches!(
            self.session.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
        ) || self.sleep_until.is_some()
            || (self.player_id.is_some() && self.player_chrome_visible)
        {
            iced::time::every(std::time::Duration::from_millis(42)).map(|_| Message::PlayerTick)
        } else {
            Subscription::none()
        };
        let chrome_tick = if self.player_id.is_some()
            && self.player_chrome_visible
            && self.player_panel == PlayerPanel::None
            && matches!(self.session.state, PlaybackState::Playing | PlaybackState::Buffering)
        {
            iced::time::every(std::time::Duration::from_millis(400))
                .map(|_| Message::PlayerChromeTick)
        } else {
            Subscription::none()
        };
        let keys = event::listen_with(map_player_hotkeys);
        Subscription::batch([closes, opens, moves, tick, chrome_tick, keys])
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
        let chrome = LayoutMetrics::compute(size.width, size.height).player_chrome_h;
        let stage_h = (size.height - chrome).max(160.0);
        let w = ((size.width - 2.0 * PLAYER_PAD).max(160.0) * scale).round() as u32;
        let h = ((stage_h - PLAYER_PAD).max(120.0) * scale).round() as u32;
        // Size only — video is drawn into iced via software render, not an OS overlay.
        let rect = VideoRect::detached(w, h);
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
                    let result = load_one(src).await;
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
                    if std::env::var_os("FLUXPLAY_AUTO_PLAY").is_some() {
                        if let Some(ch) = self.bundle.channels.first().cloned() {
                            tracing::info!(name = %ch.name, "FLUXPLAY_AUTO_PLAY → opening player");
                            return Task::batch([size_task, Task::done(Message::PlayChannel(ch))]);
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
                    return self.sync_player_layout_task(id);
                }
            }
            Message::PlayerLayout {
                position,
                size,
                scale,
            } => {
                self.apply_player_layout(position, size, scale);
            }
            Message::WindowClosed(id) => {
                if self.player_id == Some(id) {
                    self.player_id = None;
                    self.player_pos = None;
                    self.video_frame = None;
                    self.video_frame_wh = (0, 0);
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
                self.cat_filter.clear();
                self.list_limit = LIST_PAGE;
                self.series_detail = None;
                self.vod_detail = None;
                self.detail_meta_loading = false;
                if tab == Tab::Vod && self.selected_vod_category.is_none() {
                    self.selected_vod_category = Some("*".into());
                }
                if tab == Tab::Series && self.selected_series_category.is_none() {
                    self.selected_series_category = Some("*".into());
                }
                return self.prefetch_visible_art();
            }
            Message::SearchChanged(s) => {
                self.search = s;
                self.list_limit = LIST_PAGE;
            }
            Message::CatFilterChanged(s) => {
                self.cat_filter = s;
            }
            Message::LoadMore => {
                tracing::debug!(list_limit = self.list_limit, "load more");
                self.list_limit = self.list_limit.saturating_add(LIST_PAGE);
                return self.prefetch_visible_art();
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
                        self.fetch_epg_for_visible_task()
                    }
                    Tab::Vod => {
                        self.selected_vod_category = Some(id.clone());
                        self.vod_detail = None;
                        self.detail_meta_loading = false;
                        if id == "*" {
                            self.prefetch_visible_art()
                        } else if self
                            .bundle
                            .vod
                            .iter()
                            .any(|v| v.category_id.as_deref() == Some(id.as_str()))
                        {
                            self.prefetch_visible_art()
                        } else {
                            self.load_vod_category_task(id)
                        }
                    }
                    Tab::Series => {
                        self.selected_series_category = Some(id.clone());
                        self.series_detail = None;
                        self.detail_meta_loading = false;
                        if id == "*" {
                            self.prefetch_visible_art()
                        } else if self
                            .bundle
                            .series
                            .iter()
                            .any(|s| s.category_id.as_deref() == Some(id.as_str()))
                        {
                            self.prefetch_visible_art()
                        } else {
                            self.load_series_category_task(id)
                        }
                    }
                    _ => Task::none(),
                };
                return Task::batch([task, self.prefetch_visible_art()]);
            }
            Message::PlayChannel(ch) => {
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
                match self.session.open_channel(ch.clone()) {
                    Ok(()) => {
                        self.status = self.session.status_line();
                        self.persist();
                        #[cfg(target_os = "android")]
                        {
                            if let Err(e) = crate::android_intent::open_stream_url(&ch.stream_url) {
                                self.status = format!("Intent: {e}");
                            } else {
                                self.status = format!("Lecteur système · {}", ch.name);
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
                            match crate::android_intent::open_stream_url(&ch.stream_url) {
                                Ok(()) => {
                                    self.status = format!("Lecteur système · {}", ch.name);
                                }
                                Err(ie) => {
                                    self.status = format!("{e} / Intent: {ie}");
                                }
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
                self.status = format!("Ouverture — {name}…");
                let art_url = poster.clone();
                if kind == ContentKind::Series {
                    self.arm_series_queue_for_url(&url);
                } else {
                    self.series_queue.clear();
                    self.series_queue_idx = 0;
                }
                self.prefetch_armed_for = None;
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
                        self.status = format!(
                            "{e} — 1 connexion max: Stop puis réessayez, ou Externe"
                        );
                        return self.open_or_focus_player();
                    }
                }
            }
            Message::Stop => {
                self.session.stop();
                self.video_frame = None;
                self.video_frame_wh = (0, 0);
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
                let Some(id) = self.player_id.or(self.main_id) else {
                    self.status = "Ouvrez d’abord le lecteur".into();
                    return Task::none();
                };
                self.player_chrome_visible = true;
                self.player_pointer_at = Some(std::time::Instant::now());
                if self.player_fullscreen {
                    self.player_fullscreen = false;
                    self.status = "Fenêtre".into();
                    return window::set_mode(id, window::Mode::Windowed);
                }
                self.player_fullscreen = true;
                self.status = "Plein écran".into();
                return window::set_mode(id, window::Mode::Fullscreen);
            }
            Message::PlayerPointerActivity => {
                self.player_chrome_visible = true;
                self.player_pointer_at = Some(std::time::Instant::now());
            }
            Message::PlayerChromeTick => {
                self.maybe_autohide_player_chrome();
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
                self.session.refresh_times();
                if !self.session.native.is_running()
                    && matches!(
                        self.session.state,
                        PlaybackState::Playing | PlaybackState::Paused
                    )
                {
                    self.session.state = PlaybackState::Idle;
                    self.video_frame = None;
                    self.status = "Lecture terminée".into();
                }
                // Pull embedded frame into iced stage.
                if self.session.has_embedded_video()
                    && matches!(
                        self.session.state,
                        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
                    )
                {
                    let (fw, fh) = self
                        .session
                        .native
                        .video_rect()
                        .map(|r| (r.w, r.h))
                        .unwrap_or((960, 540));
                    // Cap CPU: render at most ~960px wide.
                    let scale = if fw > 960 {
                        960.0 / fw as f32
                    } else {
                        1.0
                    };
                    let rw = ((fw as f32 * scale).round() as u32).max(2);
                    let rh = ((fh as f32 * scale).round() as u32).max(2);
                    if let Some((w, h, rgba)) = self.session.pull_video_frame(rw, rh) {
                        self.video_frame = Some(ImageHandle::from_rgba(w, h, rgba));
                        self.video_frame_wh = (w, h);
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
                let mut tasks = Vec::new();
                if let Some(prefetch) = self.maybe_prefetch_next_episode() {
                    tasks.push(prefetch);
                }
                if !tasks.is_empty() {
                    return Task::batch(tasks);
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
                let dir = dirs::picture_dir()
                    .or_else(dirs::download_dir)
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
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
            Message::AddPublicDemo { name, endpoint } => {
                let mut src = MediaSource::new(name.trim(), SourceKind::M3uPlus, endpoint.trim());
                src.enabled = true;
                let id = src.id;
                self.sources.push(src);
                demo::strip_demo_if_real(&mut self.sources);
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
                if let Some(db) = &self.catalog_db {
                    if let Err(e) = db.delete_source(id) {
                        tracing::warn!(error = %e, %id, "catalog delete_source failed");
                    }
                }
                self.rebuild_bundle_from_cache();
                self.persist();
                self.status = "Source retirée".into();
            }
            Message::ReloadSource(id) => {
                self.loading = true;
                self.status = "Rechargement…".into();
                fluxplay_providers::clear_xtream_cache();
                return self.reload_one_task(id);
            }
            Message::SourcesBatchLoaded(results) => {
                self.loading = false;
                let mut ok = 0usize;
                let mut err = 0usize;
                for (source_id, result) in results {
                    match result {
                        Ok(part) => {
                            if let Some(db) = &self.catalog_db {
                                if let Err(e) = db.replace_source_bundle(source_id, &part) {
                                    tracing::warn!(error = %e, "catalog db write failed");
                                }
                                if let Err(e) = db.merge_epg(&part.epg) {
                                    tracing::warn!(error = %e, "catalog epg merge failed");
                                }
                            }
                            replace_source_bundle(&mut self.bundle, source_id, part);
                            ok += 1;
                        }
                        Err(e) => {
                            err += 1;
                            tracing::error!(%source_id, error = %e, "source load failed");
                        }
                    }
                }
                tracing::info!(ok, err, "sources batch loaded");
                if self.selected_group.is_none() {
                    self.selected_group = pick_default_live_group(&self.bundle);
                }
                if self.selected_vod_category.is_none() {
                    self.selected_vod_category = Some("*".into());
                }
                if self.selected_series_category.is_none() {
                    self.selected_series_category = Some("*".into());
                }
                self.status = format!(
                    "DB locale · {ok} source(s){} · {} chaînes · {} VOD · {} séries",
                    if err > 0 {
                        format!(" ({err} échec)")
                    } else {
                        String::new()
                    },
                    self.bundle.channels.len(),
                    self.bundle.vod.len(),
                    self.bundle.series.len(),
                );
                if let Some(db) = &self.catalog_db {
                    db.mark_full_sync_now();
                }
                return Task::batch([self.prefetch_visible_art(), self.enrich_metadata_task()]);
            }
            Message::SourceLoaded { source_id, result } => {
                self.loading = false;
                match result {
                    Ok(part) => {
                        if let Some(db) = &self.catalog_db {
                            if let Err(e) = db.replace_source_bundle(source_id, &part) {
                                tracing::warn!(error = %e, "catalog db write failed");
                            }
                            let _ = db.merge_epg(&part.epg);
                        }
                        replace_source_bundle(&mut self.bundle, source_id, part);
                        let name = self
                            .sources
                            .iter()
                            .find(|s| s.id == source_id)
                            .map(|s| s.name.clone())
                            .unwrap_or_else(|| "source".into());
                        self.status = format!(
                            "{name} · DB · {} chaînes · {} VOD · {} séries",
                            self.bundle.channels.len(),
                            self.bundle.vod.len(),
                            self.bundle.series.len(),
                        );
                        if self.selected_group.is_none() {
                            self.selected_group = pick_default_live_group(&self.bundle);
                        }
                        if self.selected_vod_category.is_none() {
                            self.selected_vod_category = Some("*".into());
                        }
                        if self.selected_series_category.is_none() {
                            self.selected_series_category = Some("*".into());
                        }
                        if let Some(db) = &self.catalog_db {
                            db.mark_full_sync_now();
                        }
                        let mut tasks = vec![
                            self.prefetch_visible_art(),
                            self.enrich_metadata_task(),
                        ];
                        if !self.autoplay_done
                            && std::env::var_os("FLUXPLAY_AUTOPLAY").is_some()
                        {
                            if let Some(ch) = self.pick_autoplay_channel() {
                                self.autoplay_done = true;
                                tasks.push(Task::done(Message::PlayChannel(ch)));
                            }
                        }
                        return Task::batch(tasks);
                    }
                    Err(e) => {
                        self.status = format!("Échec chargement: {e}");
                    }
                }
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
                return self.prefetch_visible_art();
            }
            Message::OpenImdb(id_or_query) => {
                let url = crate::metadata::imdb_title_url(&id_or_query);
                #[cfg(target_os = "android")]
                {
                    match crate::android_intent::open_stream_url(&url) {
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
                    self.status = "Import fichier: collez une URL M3U (pas de picker Android)".into();
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
                    fluxplay_providers::merge_epg(&mut self.bundle.epg, programmes);
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
                        return self.prefetch_visible_art();
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
                        return self.prefetch_visible_art();
                    }
                    Err(e) => self.status = format!("Séries: {e}"),
                }
            }
            Message::OpenSeries(id) => {
                self.vod_detail = None;
                self.detail_meta_loading = false;
                if let Some(existing) = self.bundle.series.iter().find(|s| s.id == id) {
                    self.series_detail = Some(existing.clone());
                }
                self.status = "Chargement épisodes…".into();
                let src = self
                    .sources
                    .iter()
                    .find(|s| s.enabled && s.kind == SourceKind::Xtream)
                    .cloned();
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
                    }
                    self.series_detail = Some(item);
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
            }
            Message::OpenVodDetail(item) => {
                self.series_detail = None;
                // Keep catalog list in sync when the tile came from a DB search hit.
                if let Some(existing) = self.bundle.vod.iter_mut().find(|v| v.id == item.id) {
                    *existing = item.clone();
                } else {
                    self.bundle.vod.push(item.clone());
                }
                self.status = format!("{} — fiche", item.name);
                self.vod_detail = Some(item);
                return Task::batch([
                    self.prefetch_visible_art(),
                    self.enrich_open_detail_task(false),
                ]);
            }
            Message::CloseVodDetail => {
                self.vod_detail = None;
                self.detail_meta_loading = false;
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
                return self.prefetch_visible_art();
            }
            Message::ImageLoaded(Ok((url, bytes))) => {
                self.images.insert_bytes(url, bytes);
            }
            Message::ImageLoaded(Err((url, err))) => {
                tracing::debug!(%url, %err, "image fetch failed");
                self.images.mark_failed(url);
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

    fn enrich_metadata_task(&self) -> Task<Message> {
        let mut series_cached = Vec::new();
        let mut series_fetch = Vec::new();
        for s in self
            .bundle
            .series
            .iter()
            .filter(|s| crate::metadata::needs_series_enrich(s))
            .take(24)
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
            .take(20)
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

        Task::perform(
            async move {
                let mut series_out = series_cached;
                let mut vod_out = vod_cached;
                let db = crate::catalog_db::CatalogDb::open();
                let mut set = tokio::task::JoinSet::new();
                const META_PARALLEL: usize = 3;

                let mut si = 0usize;
                let mut vi = 0usize;
                while si < series_fetch.len() || vi < vod_fetch.len() || !set.is_empty() {
                    while set.len() < META_PARALLEL
                        && (si < series_fetch.len() || vi < vod_fetch.len())
                    {
                        if si < series_fetch.len() {
                            let (id, sid, name) = series_fetch[si].clone();
                            si += 1;
                            set.spawn(async move {
                                let patch = crate::metadata::enrich_series(&name).await;
                                (true, id, sid, name, patch)
                            });
                        } else if vi < vod_fetch.len() {
                            let (id, sid, name) = vod_fetch[vi].clone();
                            vi += 1;
                            set.spawn(async move {
                                let patch = crate::metadata::enrich_vod(&name).await;
                                (false, id, sid, name, patch)
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
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                }
                Message::MetaEnriched {
                    series: series_out,
                    vod: vod_out,
                }
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
        let mut parts: Vec<String> = Vec::new();
        if let Some(y) = year.filter(|s| !s.is_empty()) {
            parts.push(y.to_string());
        }
        if let Some(g) = genre.filter(|s| !s.is_empty()) {
            let g0 = g.split(',').next().unwrap_or(g).trim();
            if !g0.is_empty() {
                parts.push(g0.to_string());
            }
        }
        if let Some(r) = rating.filter(|s| !s.is_empty() && *s != "0" && *s != "0.0") {
            parts.push(format!("★ {r}"));
        }
        if parts.is_empty() {
            fallback.to_string()
        } else {
            parts.join(" · ")
        }
    }

    fn vod_items_for_view(&self) -> Vec<VodItem> {
        let q = self.search.trim();
        let cat = match self.selected_vod_category.as_deref() {
            None | Some("*") => None,
            Some(id) => Some(id),
        };
        if !q.is_empty() {
            if let Some(db) = &self.catalog_db {
                return db.search_vod(q, cat, self.list_limit.max(LIST_PAGE));
            }
        }
        self.bundle
            .vod
            .iter()
            .filter(|v| {
                cat.map(|id| v.category_id.as_deref() == Some(id))
                    .unwrap_or(true)
                    && (q.is_empty() || v.name.to_ascii_lowercase().contains(&q.to_ascii_lowercase()))
            })
            .take(self.list_limit)
            .cloned()
            .collect()
    }

    fn series_items_for_view(&self) -> Vec<SeriesItem> {
        let q = self.search.trim();
        let cat = match self.selected_series_category.as_deref() {
            None | Some("*") => None,
            Some(id) => Some(id),
        };
        if !q.is_empty() {
            if let Some(db) = &self.catalog_db {
                return db.search_series(q, cat, self.list_limit.max(LIST_PAGE));
            }
        }
        self.bundle
            .series
            .iter()
            .filter(|s| {
                cat.map(|id| s.category_id.as_deref() == Some(id))
                    .unwrap_or(true)
                    && (q.is_empty() || s.name.to_ascii_lowercase().contains(&q.to_ascii_lowercase()))
            })
            .take(self.list_limit)
            .cloned()
            .collect()
    }

    fn prefetch_urls(&mut self, urls: impl IntoIterator<Item = String>) -> Task<Message> {
        let mut tasks = Vec::new();
        for url in urls {
            if tasks.len() >= 8 {
                break;
            }
            if let crate::images::RequestOutcome::Fetch(u) = self.images.request(&url) {
                tasks.push(Task::perform(
                    crate::images::fetch_image_bytes(u),
                    Message::ImageLoaded,
                ));
            }
        }
        if tasks.is_empty() {
            Task::none()
        } else {
            Task::batch(tasks)
        }
    }

    fn prefetch_visible_art(&mut self) -> Task<Message> {
        let mut urls = Vec::new();
        match self.tab {
            Tab::Live => {
                for ch in self
                    .bundle
                    .live_in_group(self.selected_group.as_deref())
                    .into_iter()
                    .take(self.list_limit)
                {
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
                    let selected = self.selected_vod_category.as_deref();
                    for v in self.bundle.vod.iter().filter(|v| {
                        selected
                            .map(|id| v.category_id.as_deref() == Some(id))
                            .unwrap_or(true)
                    }).take(self.list_limit)
                    {
                        if let Some(u) =
                            crate::images::pick_art(None, v.poster.as_deref(), None, None)
                        {
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
                    let selected = self.selected_series_category.as_deref();
                    for s in self.bundle.series.iter().filter(|s| {
                        selected
                            .map(|id| s.category_id.as_deref() == Some(id))
                            .unwrap_or(true)
                    }).take(self.list_limit)
                    {
                        if let Some(u) = crate::images::pick_art(
                            None,
                            None,
                            s.cover.as_deref(),
                            s.banner.as_deref(),
                        ) {
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

    fn reload_one_task(&self, id: Uuid) -> Task<Message> {
        let Some(src) = self.sources.iter().find(|s| s.id == id).cloned() else {
            return Task::none();
        };
        Task::perform(async move { load_one(src).await }, move |result| {
            Message::SourceLoaded {
                source_id: id,
                result,
            }
        })
    }

    fn rebuild_bundle_from_cache(&mut self) {
        let Some(db) = &self.catalog_db else {
            self.bundle = PlaylistBundle::default();
            return;
        };
        match db.load_bundle() {
            Ok(bundle) => {
                self.bundle = bundle;
                tracing::info!(
                    channels = self.bundle.channels.len(),
                    vod = self.bundle.vod.len(),
                    series = self.bundle.series.len(),
                    "rebuilt bundle from catalog db"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "catalog reload after remove failed");
                self.bundle = PlaylistBundle::default();
            }
        }
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
        #[cfg(target_os = "android")]
        {
            if self.player_embedded {
                return self.view_player_window();
            }
            let _ = id;
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
            (label, Message::Tab(*t), self.tab == *t)
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

        let status_bar = container(
            text(if self.loading {
                format!("Chargement… · {}", self.status)
            } else {
                self.status.clone()
            })
            .size((m.body_size - 1.0).max(11.0))
            .color(ui.ink_muted()),
        )
        .padding(Padding::from([10, 16]))
        .width(Fill)
        .clip(true)
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ui.chrome_surface())),
            border: Border {
                color: ui.outline_variant(),
                width: 1.0,
                radius: RADIUS_FULL.into(),
            },
            ..Default::default()
        });

        let main_row: Element<'_, Message> = if m.top_nav || m.rail_w <= 1.0 {
            column![
                browser::mode_top_nav(ui, m.rail_size, tab_items),
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
            .into()
        } else {
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
            .align_y(Alignment::Start)
            .into()
        };

        container(
            column![main_row, status_bar]
            .spacing(m.gap)
            .padding(Padding::new(m.pad))
            .width(Fill)
            .height(Fill),
        )
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
            fullscreen: self.player_fullscreen,
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
        if at.elapsed() >= std::time::Duration::from_millis(idle_ms) {
            self.player_chrome_visible = false;
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
        let q = self.search.to_ascii_lowercase();
        let cats = self.filtered_categories(ContentKind::Live);
        // Fallback: derive from channel groups if categories empty
        let derived: Vec<(String, String)> = if cats.is_empty() {
            self.bundle
                .group_names()
                .into_iter()
                .map(|n| (n.clone(), n))
                .collect()
        } else {
            Vec::new()
        };

        let mut cat_entries: Vec<(String, String, bool, usize)> = Vec::new();
        let all_active = self.selected_group.is_none();
        cat_entries.push(("*".into(), "Toutes".into(), all_active, self.bundle.channels.len()));

        if cats.is_empty() {
            for (id, name) in derived.into_iter().take(CAT_PAGE) {
                let active = self.selected_group.as_deref() == Some(name.as_str());
                let count = self
                    .bundle
                    .channels
                    .iter()
                    .filter(|c| c.group.as_deref() == Some(name.as_str()))
                    .count();
                cat_entries.push((id, name, active, count));
            }
        } else {
            for c in cats.into_iter().take(CAT_PAGE) {
                let active = self.selected_group.as_deref() == Some(c.name.as_str());
                // Cheap count skip for speed — show 0; name is enough
                cat_entries.push((c.name.clone(), c.name.clone(), active, 0));
            }
        }

        let filtered: Vec<&Channel> = self
            .bundle
            .live_in_group(self.selected_group.as_deref())
            .into_iter()
            .filter(|c| q.is_empty() || c.name.to_ascii_lowercase().contains(&q))
            .collect();
        let total = filtered.len();
        let page = filtered
            .iter()
            .take(self.list_limit)
            .copied()
            .collect::<Vec<_>>();

        let mut items = Column::new().spacing(4).width(Fill);
        let show_list = true; // Paginated via list_limit — always show (Toutes / group / search).
        if !show_list {
            items = items.push(browser::empty_hint(
                ui,
                "Sélectionnez une catégorie à gauche, ou lancez une recherche.",
            ));
        } else if page.is_empty() {
            items = items.push(browser::empty_hint(
                ui,
                "Aucune chaîne dans cette sélection.",
            ));
        } else {
            for ch in &page {
                let now = chrono::Utc::now();
                let (epg_now, _) = self.bundle.now_next(ch, now);
                let epg_bit = epg_now
                    .map(|p| format!(" · {}", p.title))
                    .unwrap_or_default();
                let fav = self.settings.is_favorite(&ch.id);
                let subtitle = format!(
                    "{}{}",
                    ch.group.as_deref().unwrap_or("Live"),
                    epg_bit
                );
                items = items.push(browser::media_row(
                    ch.name.clone(),
                    subtitle,
                    Message::PlayChannel((*ch).clone()),
                    Some((fav, Message::ToggleFavorite(ch.id.clone()))),
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
                ));
            }
            if total > page.len() {
                items = items.push(browser::load_more_btn(ui, total - page.len()));
            }
        }

        let header = browser::content_header(
            ui,
            self.selected_group
                .clone()
                .unwrap_or_else(|| "Toutes les chaînes".into()),
            format!("{total} chaînes · clic = lecture"),
            &self.search,
            m.search_w,
            m.title_size,
            m.stack_header,
        );

        // Pin list width — unbounded Fill rows expand across the whole window on GLES.
        let list_w = m.content_w.max(120.0);
        let content = browser::pane(
            ui,
            Length::Fill,
            column![
                header,
                browser::soft_scroll(ui, items.width(Length::Fixed(list_w)))
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill),
        );

        self.with_categories(ui, m, "Chaînes", cat_entries, content)
    }

    fn view_browse_vod(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        if let Some(detail) = &self.vod_detail {
            return self.view_vod_detail(ui, m, detail);
        }
        let cols = m.cols;
        let tile_w = m.tile_w;
        let cats = self.filtered_categories(ContentKind::Vod);
        let mut cat_entries: Vec<(String, String, bool, usize)> = Vec::new();
        let all_active = matches!(self.selected_vod_category.as_deref(), None | Some("*"));
        cat_entries.push(("*".into(), "All".into(), all_active, self.bundle.vod.len()));
        for c in cats.into_iter().take(CAT_PAGE) {
            let active = self.selected_vod_category.as_deref() == Some(c.id.as_str());
            cat_entries.push((c.id.clone(), c.name.clone(), active, 0));
        }
        let page = self.vod_items_for_view();
        let total = if self.search.trim().is_empty() {
            let cat = match self.selected_vod_category.as_deref() {
                None | Some("*") => None,
                Some(id) => Some(id),
            };
            self.bundle
                .vod
                .iter()
                .filter(|v| {
                    cat.map(|id| v.category_id.as_deref() == Some(id))
                        .unwrap_or(true)
                })
                .count()
        } else {
            page.len()
        };

        let mut tiles = Vec::with_capacity(page.len());
        for v in &page {
            let meta = Self::mosaic_meta_line(
                v.year.as_deref(),
                v.genre.as_deref(),
                v.rating.as_deref(),
                "Film",
            );
            tiles.push(browser::mosaic_tile(
                v.name.clone(),
                meta,
                Message::OpenVodDetail(v.clone()),
                ui,
                tile_w,
                crate::images::pick_art(None, v.poster.as_deref(), None, None)
                    .and_then(|u| self.images.get(&u)),
            ));
        }
        let mut body = Column::new().spacing(10).width(Fill);
        if page.is_empty() {
            body = body.push(browser::empty_hint(
                ui,
                "Aucun film — sync en cours ou changez de catégorie.",
            ));
        } else {
            body = body.push(browser::mosaic_grid(tiles, cols));
        }
        if total > page.len() {
            body = body.push(browser::load_more_btn(ui, total - page.len()));
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
            format!("{total} titres · mosaïque · recherche locale"),
            &self.search,
            m.search_w,
            m.title_size,
            m.stack_header,
        );
        let content = browser::pane(
            ui,
            Length::Fill,
            column![
                header,
                browser::soft_scroll(ui, body.width(Fill))
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill),
        );
        self.with_categories(ui, m, "Films / VOD", cat_entries, content)
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

        let mut eps = Column::new().spacing(4).width(Fill);
        for season in &detail.seasons {
            eps = eps.push(text(format!("Saison {}", season.season_number)).size(14));
            for ep in &season.episodes {
                let sub = match ep.plot.as_deref().filter(|s| !s.is_empty()) {
                    Some(p) => {
                        let chars: Vec<char> = p.chars().collect();
                        if chars.len() > 90 {
                            let short: String = chars.into_iter().take(90).collect();
                            format!("E{} · {short}…", ep.episode_num)
                        } else {
                            format!("E{} · {p}", ep.episode_num)
                        }
                    }
                    None => format!("Épisode {}", ep.episode_num),
                };
                eps = eps.push(browser::media_row(
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
                ));
            }
        }
        let episodes = if detail.seasons.is_empty() {
            None
        } else {
            Some(eps.into())
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
        let cols = m.cols;
        let tile_w = m.tile_w;
        if let Some(detail) = &self.series_detail {
            return self.view_series_detail(ui, m, detail);
        }

        let cats = self.filtered_categories(ContentKind::Series);
        let mut cat_entries: Vec<(String, String, bool, usize)> = Vec::new();
        let all_active = matches!(self.selected_series_category.as_deref(), None | Some("*"));
        cat_entries.push(("*".into(), "All".into(), all_active, self.bundle.series.len()));
        for c in cats.into_iter().take(CAT_PAGE) {
            let active = self.selected_series_category.as_deref() == Some(c.id.as_str());
            cat_entries.push((c.id.clone(), c.name.clone(), active, 0));
        }
        // categories applied via with_categories below

        let page = self.series_items_for_view();
        let total = if self.search.trim().is_empty() {
            let cat = match self.selected_series_category.as_deref() {
                None | Some("*") => None,
                Some(id) => Some(id),
            };
            self.bundle
                .series
                .iter()
                .filter(|s| {
                    cat.map(|id| s.category_id.as_deref() == Some(id))
                        .unwrap_or(true)
                })
                .count()
        } else {
            page.len()
        };

        let mut tiles = Vec::with_capacity(page.len());
        for s in &page {
            let meta = Self::mosaic_meta_line(
                s.year.as_deref(),
                s.genre.as_deref(),
                s.rating.as_deref(),
                "Série",
            );
            tiles.push(browser::mosaic_tile(
                s.name.clone(),
                meta,
                Message::OpenSeries(s.id.clone()),
                ui,
                tile_w,
                crate::images::pick_art(None, None, s.cover.as_deref(), s.banner.as_deref())
                    .and_then(|u| self.images.get(&u)),
            ));
        }
        let mut body = Column::new().spacing(10).width(Fill);
        if page.is_empty() {
            body = body.push(browser::empty_hint(
                ui,
                "Aucune série — sync en cours ou changez de catégorie.",
            ));
        } else {
            body = body.push(browser::mosaic_grid(tiles, cols));
        }
        if total > page.len() {
            body = body.push(browser::load_more_btn(ui, total - page.len()));
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
            format!("{total} séries · mosaïque · recherche locale"),
            &self.search,
            m.search_w,
            m.title_size,
            m.stack_header,
        );
        let content = browser::pane(
            ui,
            Length::Fill,
            column![
                header,
                browser::soft_scroll(ui, body.width(Fill))
            ]
            .spacing(m.gap)
            .width(Fill)
            .height(Fill),
        );
        self.with_categories(ui, m, "Séries", cat_entries, content)
    }

    fn view_favorites(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        let favs: Vec<&Channel> = self
            .bundle
            .channels
            .iter()
            .filter(|c| self.settings.is_favorite(&c.id))
            .take(self.list_limit)
            .collect();
        let mut items = Column::new().spacing(4).width(Fill);
        if favs.is_empty() {
            items = items.push(browser::empty_hint(
                ui,
                "Aucun favori — utilisez ★ sur une chaîne Live.",
            ));
        }
        for ch in favs {
            items = items.push(browser::media_row(
                ch.name.clone(),
                ch.group.clone().unwrap_or_else(|| "Favori".into()),
                Message::PlayChannel(ch.clone()),
                Some((true, Message::ToggleFavorite(ch.id.clone()))),
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
            ));
        }
        browser::pane(
            ui,
            Length::Fill,
            column![
                browser::content_header(
                    ui,
                    "Favoris".into(),
                    format!("{} épinglés", self.settings.favorites.len()),
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

    fn view_settings(&self, ui: UiTheme) -> Element<'_, Message> {
        let m = self.layout_metrics();
        let profile = target_profile();
        let backends = detect_backends();
        let mut be_list = Column::new().spacing(4).width(Fill);
        for b in &backends {
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
            column![
                text(title).size(16).color(ui.ink()),
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
                    true,
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
                "2. Métadonnées IMDb",
                "Enrichissement VOD/Séries : TVMaze + OMDb (IMDb) — synopsis, acteurs, notes.",
            ),
            text(if crate::metadata::omdb_configured() {
                "OMDb configuré (OMDB_API_KEY) — fiche détail complète (plot, cast, ★)."
            } else {
                "Sans OMDB_API_KEY : séries via TVMaze. Clé .env pour films IMDb complets."
            })
            .size(12)
            .color(if crate::metadata::omdb_configured() {
                ui.accent()
            } else {
                ui.ink_muted()
            }),
            text("3. Material 3 Expressive")
                .size(13)
                .color(ui.ink()),
            text(
                "Chrome : NavigationRail · FilterChips · SearchBar pill · FAB play · DockedToolbar · \
                 formes asymétriques (poster / dock) · surfaces tonales.",
            )
            .size(12)
            .color(ui.ink_muted()),
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
                true,
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
                    false,
                ),
                pill_button(
                    text(if self.settings.low_latency {
                        "Faible latence live : activée"
                    } else {
                        "Faible latence live : désactivée"
                    }),
                    Message::ToggleLowLatency,
                    ui,
                    false,
                ),
            ]
            .spacing(8)
            .wrap(),
            section(
                "4. Buffer & réseau",
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
                "Volume par défaut : {:.0}% · mémoriser position : {}",
                self.settings.volume * 100.0,
                if self.settings.remember_position {
                    "oui"
                } else {
                    "non"
                }
            ))
            .size(12)
            .color(ui.ink_muted()),
            section(
                "5. Séries",
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
                false,
            ),
            section(
                "6. Backends détectés sur cette machine",
                "État des décodeurs disponibles (libmpv, mpv CLI, ffplay).",
            ),
            be_list,
            section(
                "6. Image pendant la lecture",
                "Désentrelacement, upscaling, format, zoom : panneau ⋯ → Avancé dans le lecteur.",
            ),
            text(
                "Astuce : RUST_LOG=fluxplay::ui=info,fluxplay_player=info cargo run — logs de chaque bouton.",
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
                Message::PlayChannel(ch.clone()),
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
            field("Nom", &self.form_name, Message::FormName, ui),
            field(
                match self.form_kind {
                    SourceKind::Xtream => "URL serveur",
                    SourceKind::Stalker => "URL portail",
                    SourceKind::Xmltv => "URL XMLTV",
                    _ => "URL / chemin / corps M3U",
                },
                &self.form_endpoint,
                Message::FormEndpoint,
                ui,
            ),
            if matches!(self.form_kind, SourceKind::Xtream) {
                row![
                    field("Utilisateur", &self.form_user, Message::FormUser, ui),
                    field("Mot de passe", &self.form_pass, Message::FormPass, ui),
                ]
                .spacing(8)
                .into()
            } else if self.form_kind == SourceKind::Stalker {
                field("Adresse MAC", &self.form_mac, Message::FormMac, ui)
            } else {
                Space::new().height(0).into()
            },
            field("EPG XMLTV (optionnel)", &self.form_epg, Message::FormEpg, ui),
            row![
                pill_button(text("Ajouter"), Message::AddSource, ui, true),
                pill_button(text("Fichier M3U…"), Message::PickPlaylistFile, ui, false),
                pill_button(text("Diagnostiquer"), Message::DiagnosePortals, ui, false),
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

        let mut list = Column::new().spacing(6).width(Fill);
        for s in &self.sources {
            let state = if s.enabled { "actif" } else { "désactivé" };
            let meta = format!("{} · {} · {}", s.kind.label(), state, redact_endpoint(&s.endpoint));
            list = list.push(
                row![
                    browser::media_row(
                        s.name.clone(),
                        meta,
                        Message::ReloadSource(s.id),
                        None,
                        ui,
                        s.enabled,
                        None,
                        m.thumb,
                    ),
                    pill_button(text("↻"), Message::ReloadSource(s.id), ui, false),
                    pill_button(text("✕"), Message::RemoveSource(s.id), ui, false),
                ]
                .spacing(6)
                .align_y(Alignment::Center)
                .width(Fill)
                .wrap(),
            );
        }

        browser::pane(
            ui,
            Length::Fill,
            column![
                browser::content_header(
                    ui,
                    "Sources".into(),
                    format!("{} enregistrées", self.sources.len()),
                    &self.search,
                    m.search_w,
                    m.title_size,
                    m.stack_header,
                ),
                form,
                text("Sources enregistrées").size(14),
                browser::soft_scroll(ui, list.width(Fill)),
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
    PlayOptions {
        user_agent: Some("IPTVSmartersPlayer".into()),
        referer: None,
        extra_headers: Vec::new(),
        hwdec: settings.hwdec,
        cache_ms: settings.cache_ms,
        demux_secs: settings.demux_secs,
        volume: settings.volume,
        low_latency: settings.low_latency,
        preferred: settings.player_backend,
    }
}

async fn load_one(src: MediaSource) -> Result<PlaylistBundle, String> {
    fluxplay_providers::load_source_with_epg(&src)
        .await
        .map_err(|e| e.to_string())
}

/// Warm next episode bytes to disk (cap ~48 MiB) so resume is non-blocking.
async fn prefetch_episode_file(url: String, ua: String) -> Result<String, String> {
    let dest_dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("fluxplay")
        .join("prefetch");
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .user_agent(ua)
        .build()
        .map_err(|e| e.to_string())?;
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


fn ui_action_label(message: &Message) -> Option<&'static str> {
    Some(match message {
        Message::PlayerTick | Message::ImageLoaded(_) | Message::MetaEnriched { .. } => {
            return None;
        }
        Message::PlayerLayout { .. } | Message::PlayerLayoutDirty(_) | Message::WindowResized { .. } => {
            return None;
        }
        Message::Tab(_) => "nav.onglet",
        Message::SearchChanged(_) | Message::CatFilterChanged(_) => "nav.filtre",
        Message::PlayChannel(_) => "lecteur.play_chaine",
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
        Message::CycleAccent | Message::SetAccent(_) => "reglages.accent",
        Message::CycleBackend => "reglages.backend",
        Message::ToggleHwdec => "reglages.hwdec",
        Message::ToggleLowLatency => "reglages.low_latency",
        Message::TogglePrefetchNext => "reglages.prefetch",
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

fn pill_button<'a>(
    label: impl Into<Element<'a, Message>>,
    on_press: Message,
    ui: UiTheme,
    primary: bool,
) -> Element<'a, Message> {
    let label = label.into();
    button(label)
        .padding(Padding::from([10, 18]))
        .on_press(on_press)
        .style(move |theme: &Theme, status| {
            let mut base = if primary {
                button::primary(theme, status)
            } else {
                button::secondary(theme, status)
            };
            base.border.radius = RADIUS_FULL.into(); // Expressive pill button
            if primary {
                base.text_color = ui.on_primary();
                base.background = Some(Background::Color(ui.accent()));
            } else {
                base.background = Some(Background::Color(ui.secondary_container()));
                base.border.color = ui.outline_variant();
                base.border.width = 1.0;
                base.text_color = ui.accent();
            }
            base
        })
        .into()
}


fn chip(label: String, msg: Message, ui: UiTheme, active: bool) -> Element<'static, Message> {
    button(text(label).size(13))
        .padding(Padding::from([8, 14]))
        .on_press(msg)
        .style(move |theme: &Theme, status| {
            let mut s = if active {
                button::primary(theme, status)
            } else {
                button::secondary(theme, status)
            };
            s.border.radius = RADIUS_FULL.into();
            if active {
                s.text_color = ui.on_primary();
                s.background = Some(Background::Color(ui.accent()));
            } else {
                s.background = Some(Background::Color(ui.surface_muted()));
                s.text_color = ui.ink();
                s.border.color = ui.divider();
                s.border.width = 1.0;
            }
            s
        })
        .into()
}


fn field<'a>(
    label: &'a str,
    value: &str,
    on_input: impl Fn(String) -> Message + 'a,
    ui: UiTheme,
) -> Element<'a, Message> {
    column![
        text(label).size(12).color(ui.ink_muted()),
        text_input(label, value)
            .on_input(on_input)
            .padding(12)
            .size(14)
            .style(move |theme: &Theme, status| {
                let mut s = text_input::default(theme, status);
                s.border.radius = RADIUS_MD.into();
                s.background = Background::Color(ui.surface_muted());
                s
            }),
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
        Key::Named(Named::Space) => PlayerHotkey::TogglePause,
        Key::Named(Named::ArrowLeft) if modifiers.shift() => PlayerHotkey::SeekBackBig,
        Key::Named(Named::ArrowRight) if modifiers.shift() => PlayerHotkey::SeekFwdBig,
        Key::Named(Named::ArrowLeft) => PlayerHotkey::SeekBack,
        Key::Named(Named::ArrowRight) => PlayerHotkey::SeekFwd,
        Key::Named(Named::ArrowUp) => PlayerHotkey::VolumeUp,
        Key::Named(Named::ArrowDown) => PlayerHotkey::VolumeDown,
        Key::Named(Named::Escape) => PlayerHotkey::Escape,
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

