mod browser;
mod catalog_db;
mod demo;
mod images;
mod metadata;
mod player_ui;
mod storage;
mod theme;

use chrono::Local;
use fluxplay_core::models::{
    AccentPreset, AppSettings, Channel, ContentKind, MediaSource, PlaylistBundle, SeriesItem,
    SourceKind, ThemeMode, VodItem,
};
use fluxplay_player::{
    detect_backends, target_profile, PlayOptions, PlaybackState, StreamSession, VideoRect,
};
use iced::widget::{
    button, column, container, row, scrollable, text, text_input, Column, Row, Space,
};
use iced::window;
use iced::{
    Alignment, Background, Element, Fill, Length, Padding, Point, Size, Subscription, Task, Theme,
};
use uuid::Uuid;

use crate::browser::{CAT_PAGE, LIST_PAGE};
use crate::player_ui::PlayerPanel;
use crate::theme::{
    mosaic_cols, mosaic_tile_width, UiTheme, RADIUS_LG, RADIUS_MD, PLAYER_CHROME_H, PLAYER_PAD,
};
use iced::event::{self, Event};
use iced::keyboard::{self, Key, Modifiers};
use iced::keyboard::key::Named;

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                fluxplay_core::DEFAULT_ENV_FILTER.into()
            }),
        )
        .init();
    tracing::info!("FluxPlay starting");
    fluxplay_core::profiler!("boot");

    iced::daemon(FluxPlay::new, FluxPlay::update, FluxPlay::view)
        .title(FluxPlay::title)
        .theme(FluxPlay::theme_for)
        .subscription(FluxPlay::subscription)
        .run()
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
    Protocols,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Live => "Live",
            Self::Favorites => "Favoris",
            Self::Vod => "VOD",
            Self::Series => "Séries",
            Self::Epg => "EPG",
            Self::Sources => "Sources",
            Self::Settings => "Réglages",
            Self::Protocols => "Protocoles",
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
    /// Logical size of the main browser window (updated on resize).
    main_size: Size,
    /// Last known outer position of the player window (for mpv overlay when Wayland omits pos).
    player_pos: Option<Point>,
    player_panel: PlayerPanel,
    goto_draft: String,
    sleep_until: Option<std::time::Instant>,
    sleep_mins: Option<u32>,
    pip_mode: bool,
}

#[derive(Debug, Clone)]
enum Message {
    Tab(Tab),
    SearchChanged(String),
    SelectGroup(Option<String>),
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
    VolumeDelta(f32),
    SeekRel(i32),
    SeekPercent(f64),
    RestartStream,
    ToggleFullscreen,
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
    RemoveSource(Uuid),
    ReloadSource(Uuid),
    ReloadAll,
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
    DiagnosePortals,
    DiagnoseDone(String),
    EpgFetched(Vec<fluxplay_core::models::EpgProgramme>),
    SelectVodCategory(String),
    SelectSeriesCategory(String),
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
    CloseSeriesDetail,
    CatFilterChanged(String),
    SelectBrowseCategory(String),
    LoadMore,
    ImageLoaded(Result<(String, Vec<u8>), (String, String)>),
    MetaEnriched {
        series: Vec<(String, Option<uuid::Uuid>, crate::metadata::MetaPatch)>,
        vod: Vec<(String, Option<uuid::Uuid>, crate::metadata::MetaPatch)>,
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
    CycleRotate,
    NudgeZoom(f64),
    ToggleNightVf,
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
        let system_dark = matches!(dark_light::detect(), Ok(dark_light::Mode::Dark));

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
        let (main_id, open_main) = window::open(window::Settings {
            size: Size::new(1280.0, 860.0),
            position: window::Position::Centered,
            exit_on_close_request: true,
            ..Default::default()
        });
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
            session: StreamSession::with_options(opts),
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
            main_id: Some(main_id),
            player_id: None,
            main_size: Size::new(1280.0, 860.0),
            player_pos: None,
            player_panel: PlayerPanel::None,
            goto_draft: String::new(),
            sleep_until: None,
            sleep_mins: None,
            pip_mode: false,
        };

        // Offline-first: skip portal storm when SQLite catalog is still fresh.
        let mut boot = vec![open_main.map(Message::MainWindowOpened)];
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

    /// Responsive chrome metrics from the current main window width.
    fn layout_metrics(&self) -> (f32, f32, f32, usize, f32, f32) {
        let w = self.main_size.width.max(640.0);
        let rail = if w < 960.0 {
            104.0
        } else if w < 1280.0 {
            120.0
        } else {
            136.0
        };
        let cat = if w < 960.0 {
            176.0
        } else if w < 1280.0 {
            220.0
        } else if w < 1600.0 {
            248.0
        } else {
            272.0
        };
        // Outer pad 12×2 + row gap between rail/body + gap sidebar/content + pane pads.
        let chrome = 24.0 + 10.0 + 10.0 + 24.0 + 24.0;
        let content_w = (w - rail - cat - chrome).max(280.0);
        let cols = mosaic_cols(content_w);
        let tile_w = mosaic_tile_width(content_w, cols);
        let search_w = (content_w * 0.32).clamp(160.0, 360.0);
        (rail, cat, content_w, cols, tile_w, search_w)
    }

    fn subscription(&self) -> Subscription<Message> {
        let closes = window::close_events().map(Message::WindowClosed);
        let moves = window::events().filter_map(|(id, event)| match event {
            window::Event::Resized(size) => Some(Message::WindowResized { id, size }),
            window::Event::Moved(_) => Some(Message::PlayerLayoutDirty(id)),
            _ => None,
        });
        let tick = if matches!(
            self.session.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
        ) || self.sleep_until.is_some()
        {
            iced::time::every(std::time::Duration::from_millis(400)).map(|_| Message::PlayerTick)
        } else {
            Subscription::none()
        };
        let keys = event::listen_with(map_player_hotkeys);
        Subscription::batch([closes, moves, tick, keys])
    }

    fn open_or_focus_player(&self) -> Task<Message> {
        if let Some(id) = self.player_id {
            return Task::batch([window::gain_focus(id), self.sync_player_layout_task(id)]);
        }
        let (_id, open) = window::open(window::Settings {
            size: Size::new(1120.0, 800.0),
            position: window::Position::Centered,
            exit_on_close_request: true,
            ..Default::default()
        });
        open.map(Message::PlayerWindowOpened)
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
        let Some(pos) = self.player_pos else {
            // Wayland: often no absolute coords — still size the borderless surface.
            let stage_h = (size.height - PLAYER_CHROME_H).max(160.0);
            let rect = VideoRect {
                x: 80,
                y: 80,
                w: ((size.width - 2.0 * PLAYER_PAD).max(120.0) * scale).round() as u32,
                h: ((stage_h - PLAYER_PAD).max(120.0) * scale).round() as u32,
            };
            tracing::debug!(?rect, %scale, "player video rect (no absolute pos)");
            self.session.set_video_rect(rect);
            return;
        };
        let stage_h = (size.height - PLAYER_CHROME_H).max(160.0);
        let rect = VideoRect {
            x: ((pos.x + PLAYER_PAD) * scale).round() as i32,
            y: ((pos.y + PLAYER_PAD) * scale).round() as i32,
            w: ((size.width - 2.0 * PLAYER_PAD).max(120.0) * scale).round() as u32,
            h: ((stage_h - PLAYER_PAD).max(120.0) * scale).round() as u32,
        };
        tracing::debug!(?rect, %scale, "player video rect");
        self.session.set_video_rect(rect);
    }

    fn close_player_window(&mut self) -> Task<Message> {
        if let Some(id) = self.player_id.take() {
            return window::close(id);
        }
        Task::none()
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
        match message {
            Message::MainWindowOpened(id) => {
                tracing::debug!(?id, "main window opened");
                self.main_id = Some(id);
                return window::size(id).map(move |size| Message::WindowResized { id, size });
            }
            Message::PlayerWindowOpened(id) => {
                tracing::debug!(?id, "player window opened");
                self.player_id = Some(id);
                return self.sync_player_layout_task(id);
            }
            Message::PlayerLayoutDirty(id) => {
                if self.player_id == Some(id) {
                    return self.sync_player_layout_task(id);
                }
            }
            Message::WindowResized { id, size } => {
                if self.main_id == Some(id) {
                    self.main_size = size;
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
                self.session.stop();
                self.status = "Arrêté".into();
                return self.close_player_window();
            }
            Message::Tab(tab) => {
                tracing::debug!(?tab, "tab");
                self.tab = tab;
                self.cat_filter.clear();
                self.list_limit = LIST_PAGE;
                self.series_detail = None;
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
            Message::SelectGroup(g) => {
                self.selected_group = g;
                self.list_limit = LIST_PAGE;
                return self.fetch_epg_for_visible_task();
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
                match self.session.open_channel(ch) {
                    Ok(()) => {
                        self.status = self.session.status_line();
                        self.persist();
                        return Task::batch([
                            epg_task,
                            art_task,
                            self.open_or_focus_player(),
                        ]);
                    }
                    Err(e) => {
                        self.status = format!("{e} — essayez Externe ou installez mpv");
                        self.persist();
                        return Task::batch([epg_task, art_task, self.open_or_focus_player()]);
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
                self.status = "Arrêté".into();
                return self.close_player_window();
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
            Message::VolumeDelta(d) => {
                self.session.volume_delta(d);
                self.settings.volume = self.session.volume;
                if self.session.muted && self.session.volume > 0.0 {
                    self.session.muted = false;
                    let _ = self.session.native.set_mute(false);
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
                self.session.toggle_fullscreen();
                self.status = "Plein écran basculé".into();
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
                    self.status = "Lecture terminée".into();
                }
                if let Some(deadline) = self.sleep_until {
                    if std::time::Instant::now() >= deadline {
                        self.sleep_until = None;
                        self.sleep_mins = None;
                        self.session.pause();
                        self.status = "Veille — lecture en pause".into();
                    }
                }
                if let Some(id) = self.player_id {
                    return self.sync_player_layout_task(id);
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
            Message::ToggleSubVisibility => self.session.toggle_sub_visibility(),
            Message::CycleAspect => {
                self.session.cycle_aspect();
                self.status = format!("Aspect {}", self.session.aspect.label());
            }
            Message::ToggleOntop => {
                self.session.toggle_ontop();
                self.status = if self.session.ontop {
                    "Toujours au-dessus".into()
                } else {
                    "Ontop off".into()
                };
            }
            Message::TogglePip => {
                self.pip_mode = !self.pip_mode;
                if let Some(id) = self.player_id {
                    let size = if self.pip_mode {
                        Size::new(480.0, 320.0)
                    } else {
                        Size::new(1120.0, 800.0)
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
            }
            Message::CycleRotate => self.session.cycle_rotate(),
            Message::NudgeZoom(d) => self.session.nudge_zoom(d),
            Message::ToggleNightVf => {
                self.session.toggle_night_vf();
                self.status = if self.session.night_vf {
                    "Mode nuit vidéo".into()
                } else {
                    "Mode nuit off".into()
                };
            }
            Message::PlayerHotkey(hk) => {
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
                    PlayerHotkey::Fullscreen => self.session.toggle_fullscreen(),
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
            Message::ReloadAll => {
                tracing::info!("reload all sources");
                self.bundle = PlaylistBundle::default();
                self.loading = true;
                self.status = "Rechargement de toutes les sources…".into();
                fluxplay_providers::clear_xtream_cache();
                return self.reload_all_task();
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
                        if patch.year.is_some() {
                            item.year = patch.year.clone();
                        }
                        if patch.genre.is_some() {
                            item.genre = patch.genre.clone();
                        }
                        if patch.plot.is_some() {
                            item.plot = patch.plot.clone();
                        }
                        if patch.poster.is_some() && item.cover.is_none() {
                            item.cover = patch.poster.clone();
                        }
                        if patch.rating.is_some() {
                            item.rating = patch.rating.clone();
                        }
                        if let (Some(db), Some(sid)) = (&self.catalog_db, source_id.or(item.source_id)) {
                            let _ = db.update_series_meta(
                                sid,
                                &id,
                                patch.year.as_deref(),
                                patch.genre.as_deref(),
                                patch.plot.as_deref(),
                                patch.poster.as_deref(),
                            );
                        }
                    }
                }
                for (id, source_id, patch) in vod {
                    if let Some(item) = self.bundle.vod.iter_mut().find(|v| v.id == id) {
                        if patch.year.is_some() {
                            item.year = patch.year.clone();
                        }
                        if patch.genre.is_some() {
                            item.genre = patch.genre.clone();
                        }
                        if patch.plot.is_some() {
                            item.plot = patch.plot.clone();
                        }
                        if patch.poster.is_some() && item.poster.is_none() {
                            item.poster = patch.poster.clone();
                        }
                        if patch.rating.is_some() {
                            item.rating = patch.rating.clone();
                        }
                        if let (Some(db), Some(sid)) = (&self.catalog_db, source_id.or(item.source_id)) {
                            let _ = db.update_vod_meta(
                                sid,
                                &id,
                                patch.year.as_deref(),
                                patch.genre.as_deref(),
                                patch.plot.as_deref(),
                                patch.poster.as_deref(),
                            );
                        }
                    }
                }
                return self.prefetch_visible_art();
            }
            Message::OpenExternal => {
                if let Some(ch) = &self.session.channel {
                    let _ = open::that(&ch.stream_url);
                    self.status = "Ouvert dans le lecteur externe".into();
                }
            }
            Message::PickPlaylistFile => {
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
            }
            Message::ToggleLowLatency => {
                self.settings.low_latency = !self.settings.low_latency;
                self.resync_player_options();
                self.persist();
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
            Message::SelectVodCategory(id) => {
                self.selected_vod_category = Some(id.clone());
                self.list_limit = LIST_PAGE;
                if self
                    .bundle
                    .vod
                    .iter()
                    .any(|v| v.category_id.as_deref() == Some(id.as_str()))
                {
                    return self.prefetch_visible_art();
                }
                return self.load_vod_category_task(id);
            }
            Message::SelectSeriesCategory(id) => {
                self.selected_series_category = Some(id.clone());
                self.series_detail = None;
                self.list_limit = LIST_PAGE;
                if self
                    .bundle
                    .series
                    .iter()
                    .any(|s| s.category_id.as_deref() == Some(id.as_str()))
                {
                    return self.prefetch_visible_art();
                }
                return self.load_series_category_task(id);
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
                self.status = "Chargement épisodes…".into();
                let src = self
                    .sources
                    .iter()
                    .find(|s| s.enabled && s.kind == SourceKind::Xtream)
                    .cloned();
                let Some(src) = src else {
                    self.status = "Aucune source Xtream pour les séries".into();
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
                        "{} · {} saisons · {eps} épisodes — cliquez Lire ou un épisode",
                        item.name,
                        item.seasons.len()
                    );
                    // Keep list entry in sync
                    if let Some(existing) = self.bundle.series.iter_mut().find(|s| s.id == item.id) {
                        existing.seasons = item.seasons.clone();
                        existing.plot = item.plot.clone();
                        existing.cover = item.cover.clone();
                        existing.banner = item.banner.clone();
                    }
                    self.series_detail = Some(item);
                    return self.prefetch_visible_art();
                }
                Err(e) => self.status = format!("Série: {e}"),
            },
            Message::CloseSeriesDetail => {
                self.series_detail = None;
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

    fn enrich_metadata_task(&self) -> Task<Message> {
        let series: Vec<(String, Option<Uuid>, String)> = self
            .bundle
            .series
            .iter()
            .filter(|s| crate::metadata::needs_series_enrich(s))
            .take(10)
            .map(|s| (s.id.clone(), s.source_id, s.name.clone()))
            .collect();
        let vod: Vec<(String, Option<Uuid>, String)> = self
            .bundle
            .vod
            .iter()
            .filter(|v| crate::metadata::needs_vod_enrich(v))
            .take(6)
            .map(|v| (v.id.clone(), v.source_id, v.name.clone()))
            .collect();
        if series.is_empty() && vod.is_empty() {
            return Task::none();
        }
        Task::perform(
            async move {
                let mut series_out = Vec::new();
                let mut vod_out = Vec::new();
                let mut set = tokio::task::JoinSet::new();
                const META_PARALLEL: usize = 3;

                let mut si = 0usize;
                let mut vi = 0usize;
                while si < series.len() || vi < vod.len() || !set.is_empty() {
                    while set.len() < META_PARALLEL && (si < series.len() || vi < vod.len()) {
                        if si < series.len() {
                            let (id, sid, name) = series[si].clone();
                            si += 1;
                            set.spawn(async move {
                                let patch = crate::metadata::enrich_series(&name).await;
                                (true, id, sid, patch)
                            });
                        } else if vi < vod.len() {
                            let (id, sid, name) = vod[vi].clone();
                            vi += 1;
                            set.spawn(async move {
                                let patch = crate::metadata::enrich_vod(&name).await;
                                (false, id, sid, patch)
                            });
                        }
                    }
                    if let Some(joined) = set.join_next().await {
                        if let Ok((is_series, id, sid, Some(patch))) = joined {
                            if is_series {
                                series_out.push((id, sid, patch));
                            } else {
                                vod_out.push((id, sid, patch));
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

    fn mosaic_meta_line(year: Option<&str>, genre: Option<&str>, fallback: &str) -> String {
        match (year.filter(|s| !s.is_empty()), genre.filter(|s| !s.is_empty())) {
            (Some(y), Some(g)) => format!("{y}, {g}"),
            (Some(y), None) => y.to_string(),
            (None, Some(g)) => g.to_string(),
            _ => fallback.to_string(),
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
                let selected = self.selected_vod_category.as_deref();
                for v in self.bundle.vod.iter().filter(|v| {
                    selected
                        .map(|id| v.category_id.as_deref() == Some(id))
                        .unwrap_or(true)
                }).take(self.list_limit)
                {
                    if let Some(u) = crate::images::pick_art(None, v.poster.as_deref(), None, None)
                    {
                        urls.push(u);
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

    fn first_category(&self, kind: ContentKind) -> Option<&fluxplay_core::models::Category> {
        self.bundle
            .categories
            .iter()
            .find(|c| c.content == kind && !is_adult_cat(&c.name))
            .or_else(|| self.bundle.categories.iter().find(|c| c.content == kind))
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

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if self.player_id == Some(id) {
            return self.view_player_window();
        }
        self.view_browser()
    }

    fn view_browser(&self) -> Element<'_, Message> {
        let ui = self.ui_theme();
        let (rail_w, _, _, _, _, _) = self.layout_metrics();
        let rail = browser::mode_rail(
            ui,
            rail_w,
            Tab::all()
                .iter()
                .map(|t| (t.label(), Message::Tab(*t), self.tab == *t)),
        );

        let body = match self.tab {
            Tab::Live => self.view_browse_live(ui),
            Tab::Vod => self.view_browse_vod(ui),
            Tab::Series => self.view_browse_series(ui),
            Tab::Favorites => self.view_favorites(ui),
            Tab::Epg => self.view_epg(ui),
            Tab::Sources => self.view_sources(ui),
            Tab::Settings | Tab::Protocols => self.view_settings(ui),
        };

        let status_bar = container(
            text(if self.loading {
                format!("Chargement… · {}", self.status)
            } else {
                self.status.clone()
            })
            .size(12)
            .color(ui.ink_muted()),
        )
        .padding(Padding::from([6, 10]))
        .width(Fill);

        container(
            column![row![rail, body].spacing(10).height(Fill), status_bar]
                .spacing(8)
                .padding(12),
        )
        .width(Fill)
        .height(Fill)
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
            active,
            panel: self.player_panel,
            goto_draft: &self.goto_draft,
            sleep_mins: self.sleep_mins,
            pip: self.pip_mode,
        })
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
        let (_, cat_w, _, _, _, search_w) = self.layout_metrics();
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

        let sidebar = browser::category_sidebar(ui, cat_w, "Chaînes", &self.cat_filter, cat_entries);

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
        let show_list = self.selected_group.is_some() || !q.is_empty();
        if !show_list {
            items = items.push(browser::empty_hint(
                ui,
                "Sélectionnez une catégorie à gauche, ou lancez une recherche.",
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
                .unwrap_or_else(|| "Télévision".into()),
            format!("{total} chaînes · clic = lecture"),
            &self.search,
            search_w,
        );

        let content = browser::pane(
            ui,
            Length::Fill,
            column![header, scrollable(items).height(Fill)]
                .spacing(10)
                .height(Fill),
        );

        row![sidebar, content].spacing(10).height(Fill).into()
    }

    fn view_browse_vod(&self, ui: UiTheme) -> Element<'_, Message> {
        let (_, cat_w, _, cols, tile_w, search_w) = self.layout_metrics();
        let cats = self.filtered_categories(ContentKind::Vod);
        let mut cat_entries: Vec<(String, String, bool, usize)> = Vec::new();
        let all_active = matches!(self.selected_vod_category.as_deref(), None | Some("*"));
        cat_entries.push(("*".into(), "All".into(), all_active, self.bundle.vod.len()));
        for c in cats.into_iter().take(CAT_PAGE) {
            let active = self.selected_vod_category.as_deref() == Some(c.id.as_str());
            cat_entries.push((c.id.clone(), c.name.clone(), active, 0));
        }
        let sidebar = browser::category_sidebar(ui, cat_w, "Films / VOD", &self.cat_filter, cat_entries);

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
                v.rating.as_deref().unwrap_or("Film"),
            );
            tiles.push(browser::mosaic_tile(
                v.name.clone(),
                meta,
                Message::PlayVod {
                    name: v.name.clone(),
                    url: v.stream_url.clone(),
                    kind: ContentKind::Vod,
                    poster: v.poster.clone(),
                },
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
                search_w,
        );
        let content = browser::pane(
            ui,
            Length::Fill,
            column![header, scrollable(body).height(Fill)]
                .spacing(10)
                .height(Fill),
        );
        row![sidebar, content].spacing(10).height(Fill).into()
    }

    fn view_browse_series(&self, ui: UiTheme) -> Element<'_, Message> {
        let (_, cat_w, _, cols, tile_w, search_w) = self.layout_metrics();
        if let Some(detail) = &self.series_detail {
            let mut eps = Column::new().spacing(4).width(Fill);
            eps = eps.push(
                button(text("← Catalogue").size(13))
                    .on_press(Message::CloseSeriesDetail)
                    .padding(10),
            );
            if let Some(plot) = &detail.plot {
                eps = eps.push(text(plot).size(12).color(ui.ink_muted()));
            }
            if let Some(ep) = detail
                .seasons
                .iter()
                .flat_map(|s| s.episodes.iter())
                .next()
            {
                eps = eps.push(
                    button(
                        text(format!("Lire · {} (S{}E{})", ep.title, 
                            detail.seasons.first().map(|s| s.season_number).unwrap_or(1),
                            ep.episode_num))
                            .size(14),
                    )
                    .on_press(Message::PlayVod {
                        name: format!("{} — {}", detail.name, ep.title),
                        url: ep.stream_url.clone(),
                        kind: ContentKind::Series,
                        poster: detail.cover.clone().or(detail.banner.clone()),
                    })
                    .padding(Padding::from([10, 16]))
                    .style(move |theme: &Theme, status| {
                        let mut s = button::primary(theme, status);
                        s.border.radius = RADIUS_MD.into();
                        s
                    }),
                );
            }
            for season in &detail.seasons {
                eps = eps.push(text(format!("Saison {}", season.season_number)).size(14));
                for ep in &season.episodes {
                    eps = eps.push(browser::media_row(
                        ep.title.clone(),
                        format!("Épisode {}", ep.episode_num),
                        Message::PlayVod {
                            name: format!("{} — {}", detail.name, ep.title),
                            url: ep.stream_url.clone(),
                            kind: ContentKind::Series,
                            poster: detail.cover.clone().or(detail.banner.clone()),
                        },
                        None,
                        ui,
                        false,
                        self.series_detail.as_ref().and_then(|d| {
                            crate::images::pick_art(
                                None,
                                None,
                                d.cover.as_deref(),
                                d.banner.as_deref(),
                            )
                            .and_then(|u| self.images.get(&u))
                        }),
                    ));
                }
            }
            return browser::pane(
                ui,
                Length::Fill,
                column![
                    text(&detail.name).size(22),
                    scrollable(eps).height(Fill),
                ]
                .spacing(10)
                .height(Fill),
            );
        }

        let cats = self.filtered_categories(ContentKind::Series);
        let mut cat_entries: Vec<(String, String, bool, usize)> = Vec::new();
        let all_active = matches!(self.selected_series_category.as_deref(), None | Some("*"));
        cat_entries.push(("*".into(), "All".into(), all_active, self.bundle.series.len()));
        for c in cats.into_iter().take(CAT_PAGE) {
            let active = self.selected_series_category.as_deref() == Some(c.id.as_str());
            cat_entries.push((c.id.clone(), c.name.clone(), active, 0));
        }
        let sidebar = browser::category_sidebar(ui, cat_w, "Séries", &self.cat_filter, cat_entries);

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
                "Series",
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
                search_w,
        );
        let content = browser::pane(
            ui,
            Length::Fill,
            column![header, scrollable(body).height(Fill)]
                .spacing(10)
                .height(Fill),
        );
        row![sidebar, content].spacing(10).height(Fill).into()
    }

    fn view_favorites(&self, ui: UiTheme) -> Element<'_, Message> {
        let (_, _, _, _, _, search_w) = self.layout_metrics();
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
                    search_w,
                ),
                scrollable(items).height(Fill),
            ]
            .spacing(10)
            .height(Fill),
        )
    }

    fn view_settings(&self, ui: UiTheme) -> Element<'_, Message> {
        let (_, _, _, _, _, search_w) = self.layout_metrics();
        let profile = target_profile();
        let backends = detect_backends();
        let mut be_list = Column::new().spacing(4).width(Fill);
        for b in &backends {
            let mark = if b.available { "✓" } else { "○" };
            be_list = be_list.push(browser::media_row(
                format!("{} {}", mark, b.id.label()),
                b.detail.clone(),
                Message::CycleBackend,
                None,
                ui,
                false,
                None,
            ));
        }

        let accent_row = Row::with_children(
            AccentPreset::all()
                .iter()
                .copied()
                .map(|p| browser::accent_swatch(ui, p, self.settings.accent == p)),
        )
        .spacing(8)
        .wrap();

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
                search_w,
            ),
            text(profile.notes).size(12).color(ui.ink_muted()),
            text("Apparence").size(14),
            row![
                pill_button(
                    text(format!("Thème: {}", self.settings.theme.label())),
                    Message::CycleTheme,
                    ui,
                    true,
                ),
                pill_button(
                    text(format!("Accent: {}", self.settings.accent.label())),
                    Message::CycleAccent,
                    ui,
                    false,
                ),
            ]
            .spacing(8)
            .wrap(),
            text("Couleur d’accent").size(12).color(ui.ink_muted()),
            accent_row,
            text("Lecture").size(14),
            row![
                pill_button(
                    text(format!("Backend: {}", self.settings.player_backend.label())),
                    Message::CycleBackend,
                    ui,
                    true,
                ),
                pill_button(
                    text(if self.settings.hwdec {
                        "HW decode ON"
                    } else {
                        "HW decode OFF"
                    }),
                    Message::ToggleHwdec,
                    ui,
                    false,
                ),
                pill_button(
                    text(if self.settings.low_latency {
                        "Low-latency ON"
                    } else {
                        "Low-latency OFF"
                    }),
                    Message::ToggleLowLatency,
                    ui,
                    false,
                ),
            ]
            .spacing(8)
            .wrap(),
            text(format!(
                "Cache {} ms · demux {:.0}s · volume {:.0}%",
                self.settings.cache_ms,
                self.settings.demux_secs,
                self.settings.volume * 100.0
            ))
            .size(12)
            .color(ui.ink_muted()),
            text("Backends détectés").size(14),
            scrollable(be_list).height(Fill),
        ]
        .spacing(10)
        .height(Fill);

        browser::pane(ui, Length::Fill, body)
    }

    fn view_epg(&self, ui: UiTheme) -> Element<'_, Message> {
        let (_, _, _, _, _, search_w) = self.layout_metrics();
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
                    search_w,
                ),
                scrollable(items).height(Fill),
            ]
            .spacing(10)
            .height(Fill),
        )
    }

    fn view_sources(&self, ui: UiTheme) -> Element<'_, Message> {
        let (_, _, _, _, _, search_w) = self.layout_metrics();
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
            .spacing(8),
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
                    ),
                    pill_button(text("↻"), Message::ReloadSource(s.id), ui, false),
                    pill_button(text("✕"), Message::RemoveSource(s.id), ui, false),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
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
                    search_w,
                ),
                form,
                text("Sources enregistrées").size(14),
                scrollable(list).height(Fill),
            ]
            .spacing(10)
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


fn pill_button<'a>(
    label: impl Into<Element<'a, Message>>,
    on_press: Message,
    ui: UiTheme,
    primary: bool,
) -> Element<'a, Message> {
    let label = label.into();
    button(label)
        .padding(Padding::from([10, 16]))
        .on_press(on_press)
        .style(move |theme: &Theme, status| {
            let mut base = if primary {
                button::primary(theme, status)
            } else {
                button::secondary(theme, status)
            };
            base.border.radius = RADIUS_LG.into();
            if primary {
                base.text_color = ui.on_primary();
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
            s.border.radius = RADIUS_LG.into();
            if active {
                s.text_color = ui.on_primary();
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
        Key::Named(Named::Escape) => PlayerHotkey::Stop,
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


