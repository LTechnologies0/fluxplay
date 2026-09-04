mod browser;
mod demo;
mod images;
mod player_ui;
mod storage;
mod theme;

use chrono::Local;
use fluxplay_core::models::{
    AppSettings, Channel, ContentKind, MediaSource, PlaylistBundle, SeriesItem, SourceKind,
    ThemeMode, VodItem,
};
use fluxplay_player::{
    detect_backends, target_profile, PlayOptions, PlaybackState, StreamSession,
};
use iced::widget::{
    button, column, container, row, scrollable, text, text_input, Column, Row, Space,
};
use iced::window;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding, Shadow, Size,
    Subscription, Task, Theme,
};
use uuid::Uuid;

use crate::browser::{CAT_PAGE, LIST_PAGE};
use crate::theme::{
    flux_day, flux_night, ink_muted, on_primary, outline, surface, surface_muted, RADIUS_LG,
    RADIUS_MD,
};

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fluxplay=info".into()),
        )
        .init();

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
    autoplay_done: bool,
    main_id: Option<window::Id>,
    player_id: Option<window::Id>,
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
    /// Background warm-up: more VOD/series categories merged into the UI.
    CatalogWarm {
        vod: Vec<VodItem>,
        series: Vec<SeriesItem>,
    },
    MainWindowOpened(window::Id),
    PlayerWindowOpened(window::Id),
    WindowClosed(window::Id),
    ClosePlayerWindow,
}

impl FluxPlay {
    fn new() -> (Self, Task<Message>) {
        let persisted = storage::load();
        let mut sources = persisted.sources;
        sanitize_sources(&mut sources);
        if sources.is_empty() {
            sources.push(demo::demo_source());
        }
        let settings = persisted.settings;
        storage::save(&storage::PersistedState {
            settings: settings.clone(),
            sources: sources.clone(),
        });
        let system_dark = matches!(dark_light::detect(), Ok(dark_light::Mode::Dark));

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
            bundle: PlaylistBundle::default(),
            tab: Tab::Live,
            search: String::new(),
            selected_group: None,
            selected_channel: None,
            selected_vod_category: None,
            selected_series_category: None,
            series_detail: None,
            session: StreamSession::with_options(opts),
            status: "Chargement de la démo…".into(),
            loading: true,
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
            autoplay_done: false,
            main_id: Some(main_id),
            player_id: None,
        };

        let task = Task::batch([
            open_main.map(Message::MainWindowOpened),
            app.reload_all_task(),
        ]);
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

    fn is_day(&self) -> bool {
        match self.settings.theme {
            ThemeMode::Day => true,
            ThemeMode::Night => false,
            ThemeMode::System => !self.system_dark,
        }
    }

    fn theme(&self) -> Theme {
        if self.is_day() {
            flux_day()
        } else {
            flux_night()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let closes = window::close_events().map(Message::WindowClosed);
        let tick = if matches!(
            self.session.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
        ) {
            iced::time::every(std::time::Duration::from_millis(500)).map(|_| Message::PlayerTick)
        } else {
            Subscription::none()
        };
        Subscription::batch([closes, tick])
    }

    fn open_or_focus_player(&self) -> Task<Message> {
        if let Some(id) = self.player_id {
            return window::gain_focus(id);
        }
        let (_id, open) = window::open(window::Settings {
            size: Size::new(1000.0, 720.0),
            position: window::Position::Centered,
            exit_on_close_request: true,
            ..Default::default()
        });
        open.map(Message::PlayerWindowOpened)
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
        Task::batch(sources.into_iter().map(|src| {
            let id = src.id;
            Task::perform(async move { load_one(src).await }, move |result| {
                Message::SourceLoaded {
                    source_id: id,
                    result,
                }
            })
        }))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::MainWindowOpened(id) => {
                self.main_id = Some(id);
            }
            Message::PlayerWindowOpened(id) => {
                self.player_id = Some(id);
            }
            Message::WindowClosed(id) => {
                if self.player_id == Some(id) {
                    self.player_id = None;
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
                self.tab = tab;
                self.cat_filter.clear();
                self.list_limit = LIST_PAGE;
                self.series_detail = None;
                if tab == Tab::Vod && self.selected_vod_category.is_none() {
                    if let Some(c) = self.first_category(ContentKind::Vod) {
                        return self.load_vod_category_task(c.id.clone());
                    }
                }
                if tab == Tab::Series && self.selected_series_category.is_none() {
                    if let Some(c) = self.first_category(ContentKind::Series) {
                        return self.load_series_category_task(c.id.clone());
                    }
                }
            }
            Message::SearchChanged(s) => {
                self.search = s;
                self.list_limit = LIST_PAGE;
            }
            Message::CatFilterChanged(s) => {
                self.cat_filter = s;
            }
            Message::LoadMore => {
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
                        self.load_vod_category_task(id)
                    }
                    Tab::Series => {
                        self.selected_series_category = Some(id.clone());
                        self.series_detail = None;
                        self.load_series_category_task(id)
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
            }
            Message::CycleTheme => {
                self.settings.theme = self.settings.theme.cycle();
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
                    self.status = "Nom et endpoint requis".into();
                    return Task::none();
                }
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
                self.form_name.clear();
                self.form_endpoint.clear();
                self.form_user.clear();
                self.form_pass.clear();
                self.form_mac.clear();
                self.form_epg.clear();
                self.persist();
                self.status = "Source ajoutée — chargement…".into();
                self.loading = true;
                return self.reload_one_task(id);
            }
            Message::RemoveSource(id) => {
                self.sources.retain(|s| s.id != id);
                self.rebuild_bundle_from_cache();
                self.persist();
                self.status = "Source retirée".into();
            }
            Message::ReloadSource(id) => {
                self.loading = true;
                self.status = "Rechargement…".into();
                return self.reload_one_task(id);
            }
            Message::ReloadAll => {
                self.bundle = PlaylistBundle::default();
                self.loading = true;
                self.status = "Rechargement de toutes les sources…".into();
                return self.reload_all_task();
            }
            Message::SourceLoaded { source_id, result } => {
                self.loading = false;
                match result {
                    Ok(part) => {
                        replace_source_bundle(&mut self.bundle, source_id, part);
                        let name = self
                            .sources
                            .iter()
                            .find(|s| s.id == source_id)
                            .map(|s| s.name.clone())
                            .unwrap_or_else(|| "source".into());
                        self.status = format!(
                            "{name} · {} chaînes · {} VOD · {} séries · {} EPG",
                            self.bundle.channels.len(),
                            self.bundle.vod.len(),
                            self.bundle.series.len(),
                            self.bundle.epg.len()
                        );
                        // Auto-select a useful live category (prefer FR).
                        if self.selected_group.is_none() {
                            self.selected_group = pick_default_live_group(&self.bundle);
                        }
                        if self.selected_vod_category.is_none() {
                            if let Some(c) = self.first_category(ContentKind::Vod) {
                                self.selected_vod_category = Some(c.id.clone());
                            }
                        }
                        if self.selected_series_category.is_none() {
                            if let Some(c) = self.first_category(ContentKind::Series) {
                                self.selected_series_category = Some(c.id.clone());
                            }
                        }
                        let mut tasks = vec![
                            self.fetch_epg_for_visible_task(),
                            self.prefetch_visible_art(),
                            self.warm_catalog_task(),
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
            Message::CatalogWarm { vod, series } => {
                let mut added_v = 0usize;
                let mut added_s = 0usize;
                let mut seen_v: std::collections::HashSet<_> =
                    self.bundle.vod.iter().map(|v| v.id.clone()).collect();
                for v in vod {
                    if seen_v.insert(v.id.clone()) {
                        self.bundle.vod.push(v);
                        added_v += 1;
                    }
                }
                let mut seen_s: std::collections::HashSet<_> =
                    self.bundle.series.iter().map(|s| s.id.clone()).collect();
                for s in series {
                    if seen_s.insert(s.id.clone()) {
                        self.bundle.series.push(s);
                        added_s += 1;
                    }
                }
                if added_v + added_s > 0 {
                    self.status = format!(
                        "Catalogue +{added_v} films · +{added_s} séries · total {}/{}",
                        self.bundle.vod.len(),
                        self.bundle.series.len()
                    );
                    return self.prefetch_visible_art();
                }
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
                return self.load_vod_category_task(id);
            }
            Message::SelectSeriesCategory(id) => {
                self.selected_series_category = Some(id.clone());
                self.series_detail = None;
                self.list_limit = LIST_PAGE;
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

    fn warm_catalog_task(&self) -> Task<Message> {
        let Some(src) = self.xtream_source() else {
            return Task::none();
        };
        let loaded_vod: std::collections::HashSet<_> = self
            .bundle
            .vod
            .iter()
            .filter_map(|v| v.category_id.clone())
            .collect();
        let loaded_series: std::collections::HashSet<_> = self
            .bundle
            .series
            .iter()
            .filter_map(|s| s.category_id.clone())
            .collect();
        let vod_ids: Vec<String> = self
            .bundle
            .categories
            .iter()
            .filter(|c| c.content == ContentKind::Vod && !is_adult_cat(&c.name))
            .map(|c| c.id.clone())
            .filter(|id| !loaded_vod.contains(id))
            .take(8)
            .collect();
        let series_ids: Vec<String> = self
            .bundle
            .categories
            .iter()
            .filter(|c| c.content == ContentKind::Series && !is_adult_cat(&c.name))
            .map(|c| c.id.clone())
            .filter(|id| !loaded_series.contains(id))
            .take(8)
            .collect();
        if vod_ids.is_empty() && series_ids.is_empty() {
            return Task::none();
        }
        Task::perform(
            async move {
                let (vod, series) = tokio::join!(
                    fluxplay_providers::load_xtream_vod_categories(&src, &vod_ids, 4),
                    fluxplay_providers::load_xtream_series_categories(&src, &series_ids, 4),
                );
                Message::CatalogWarm { vod, series }
            },
            |m| m,
        )
    }

    fn prefetch_urls(&mut self, urls: impl IntoIterator<Item = String>) -> Task<Message> {
        let mut tasks = Vec::new();
        for url in urls {
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
            .take(30)
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
        // Sources removed — clear and ask user to reload.
        self.bundle = PlaylistBundle::default();
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if self.player_id == Some(id) {
            return self.view_player_window();
        }
        self.view_browser()
    }

    fn view_browser(&self) -> Element<'_, Message> {
        let day = self.is_day();
        let rail = browser::mode_rail(
            day,
            Tab::all()
                .iter()
                .map(|t| (t.label(), Message::Tab(*t), self.tab == *t)),
        );

        let body = match self.tab {
            Tab::Live => self.view_browse_live(day),
            Tab::Vod => self.view_browse_vod(day),
            Tab::Series => self.view_browse_series(day),
            Tab::Favorites => self.view_favorites(day),
            Tab::Epg => self.view_epg(day),
            Tab::Sources => self.view_sources(day),
            Tab::Settings | Tab::Protocols => self.view_settings(day),
        };

        let status_bar = container(
            text(if self.loading {
                format!("Chargement… · {}", self.status)
            } else {
                self.status.clone()
            })
            .size(12)
            .color(ink_muted(day)),
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
            background: Some(Background::Color(browser::shell_background(day))),
            ..Default::default()
        })
        .into()
    }

    fn view_player_window(&self) -> Element<'_, Message> {
        let day = self.is_day();
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
        let backend_label = self.session.backend.map(|b| b.label()).unwrap_or("—");
        let active = !matches!(
            self.session.state,
            PlaybackState::Idle
        ) || self.session.channel.is_some();

        player_ui::player_window(player_ui::PlayerChrome {
            day,
            title,
            meta,
            status: &self.status,
            state: self.session.state,
            backend: backend_label,
            live: self.session.is_live(),
            muted: self.session.muted,
            volume: self.session.volume,
            progress: self.session.progress_ratio(),
            time_label: self.session.elapsed_label(),
            art,
            active,
        })
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

    fn view_browse_live(&self, day: bool) -> Element<'_, Message> {
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

        let sidebar = browser::category_sidebar(day, "Chaînes", &self.cat_filter, cat_entries);

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
                day,
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
                    day,
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
                items = items.push(browser::load_more_btn(day, total - page.len()));
            }
        }

        let header = browser::content_header(
            day,
            self.selected_group
                .clone()
                .unwrap_or_else(|| "Télévision".into()),
            format!("{total} chaînes · clic = lecture"),
            &self.search,
        );

        let content = browser::pane(
            day,
            Length::Fill,
            column![header, scrollable(items).height(Fill)]
                .spacing(10)
                .height(Fill),
        );

        row![sidebar, content].spacing(10).height(Fill).into()
    }

    fn view_browse_vod(&self, day: bool) -> Element<'_, Message> {
        let q = self.search.to_ascii_lowercase();
        let cats = self.filtered_categories(ContentKind::Vod);
        let cat_entries: Vec<(String, String, bool, usize)> = cats
            .into_iter()
            .take(CAT_PAGE)
            .map(|c| {
                let active = self.selected_vod_category.as_deref() == Some(c.id.as_str());
                (c.id.clone(), c.name.clone(), active, 0)
            })
            .collect();
        let sidebar = browser::category_sidebar(day, "Films / VOD", &self.cat_filter, cat_entries);

        let selected = self.selected_vod_category.as_deref();
        let filtered: Vec<&VodItem> = self
            .bundle
            .vod
            .iter()
            .filter(|v| {
                selected
                    .map(|id| v.category_id.as_deref() == Some(id))
                    .unwrap_or(true)
                    && (q.is_empty() || v.name.to_ascii_lowercase().contains(&q))
            })
            .collect();
        let total = filtered.len();
        let page: Vec<&VodItem> = filtered.into_iter().take(self.list_limit).collect();

        let mut items = Column::new().spacing(4).width(Fill);
        if selected.is_none() {
            items = items.push(
                text("Choisissez une catégorie VOD — chargement à la demande.")
                    .size(13)
                    .color(ink_muted(day)),
            );
        }
        for v in &page {
            let sub = format!(
                "{} · {}",
                v.year.as_deref().unwrap_or("Film"),
                v.rating.as_deref().unwrap_or("VOD")
            );
            items = items.push(browser::media_row(
                v.name.clone(),
                sub,
                Message::PlayVod {
                    name: v.name.clone(),
                    url: v.stream_url.clone(),
                    kind: ContentKind::Vod,
                    poster: v.poster.clone(),
                },
                None,
                day,
                false,
                crate::images::pick_art(None, v.poster.as_deref(), None, None)
                    .and_then(|u| self.images.get(&u)),
            ));
        }
        if total > page.len() {
            items = items.push(browser::load_more_btn(day, total - page.len()));
        }

        let cat_name = self
            .bundle
            .categories
            .iter()
            .find(|c| Some(c.id.as_str()) == selected)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "VOD".into());

        let header = browser::content_header(
            day,
            cat_name,
            format!("{total} titres · clic = lecture"),
            &self.search,
        );
        let content = browser::pane(
            day,
            Length::Fill,
            column![header, scrollable(items).height(Fill)]
                .spacing(10)
                .height(Fill),
        );
        row![sidebar, content].spacing(10).height(Fill).into()
    }

    fn view_browse_series(&self, day: bool) -> Element<'_, Message> {
        if let Some(detail) = &self.series_detail {
            let mut eps = Column::new().spacing(4).width(Fill);
            eps = eps.push(
                button(text("← Catalogue").size(13))
                    .on_press(Message::CloseSeriesDetail)
                    .padding(10),
            );
            if let Some(plot) = &detail.plot {
                eps = eps.push(text(plot).size(12).color(ink_muted(day)));
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
                        day,
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
                day,
                Length::Fill,
                column![
                    text(&detail.name).size(22),
                    scrollable(eps).height(Fill),
                ]
                .spacing(10)
                .height(Fill),
            );
        }

        let q = self.search.to_ascii_lowercase();
        let cats = self.filtered_categories(ContentKind::Series);
        let cat_entries: Vec<(String, String, bool, usize)> = cats
            .into_iter()
            .take(CAT_PAGE)
            .map(|c| {
                let active = self.selected_series_category.as_deref() == Some(c.id.as_str());
                (c.id.clone(), c.name.clone(), active, 0)
            })
            .collect();
        let sidebar = browser::category_sidebar(day, "Séries", &self.cat_filter, cat_entries);

        let selected = self.selected_series_category.as_deref();
        let filtered: Vec<&SeriesItem> = self
            .bundle
            .series
            .iter()
            .filter(|s| {
                selected
                    .map(|id| s.category_id.as_deref() == Some(id))
                    .unwrap_or(true)
                    && (q.is_empty() || s.name.to_ascii_lowercase().contains(&q))
            })
            .collect();
        let total = filtered.len();
        let page: Vec<&SeriesItem> = filtered.into_iter().take(self.list_limit).collect();

        let mut items = Column::new().spacing(4).width(Fill);
        for s in &page {
            items = items.push(browser::media_row(
                s.name.clone(),
                s.plot
                    .clone()
                    .unwrap_or_else(|| "Ouvrir les épisodes".into()),
                Message::OpenSeries(s.id.clone()),
                None,
                day,
                false,
                crate::images::pick_art(None, None, s.cover.as_deref(), s.banner.as_deref())
                    .and_then(|u| self.images.get(&u)),
            ));
        }
        if total > page.len() {
            items = items.push(browser::load_more_btn(day, total - page.len()));
        }

        let cat_name = self
            .bundle
            .categories
            .iter()
            .find(|c| Some(c.id.as_str()) == selected)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Séries".into());
        let header = browser::content_header(
            day,
            cat_name,
            format!("{total} séries · clic = épisodes"),
            &self.search,
        );
        let content = browser::pane(
            day,
            Length::Fill,
            column![header, scrollable(items).height(Fill)]
                .spacing(10)
                .height(Fill),
        );
        row![sidebar, content].spacing(10).height(Fill).into()
    }

    fn view_favorites(&self, day: bool) -> Element<'_, Message> {
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
                day,
                "Aucun favori — utilisez ★ sur une chaîne Live.",
            ));
        }
        for ch in favs {
            items = items.push(browser::media_row(
                ch.name.clone(),
                ch.group.clone().unwrap_or_else(|| "Favori".into()),
                Message::PlayChannel(ch.clone()),
                Some((true, Message::ToggleFavorite(ch.id.clone()))),
                day,
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
            day,
            Length::Fill,
            column![
                browser::content_header(
                    day,
                    "Favoris".into(),
                    format!("{} épinglés", self.settings.favorites.len()),
                    &self.search,
                ),
                scrollable(items).height(Fill),
            ]
            .spacing(10)
            .height(Fill),
        )
    }

    fn view_settings(&self, day: bool) -> Element<'_, Message> {
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
                day,
                false,
                None,
            ));
        }

        let body = column![
            browser::content_header(
                day,
                "Réglages".into(),
                format!(
                    "{} · {}",
                    profile.platform.label(),
                    profile.ui_shell
                ),
                &self.search,
            ),
            text(profile.notes).size(12).color(ink_muted(day)),
            row![
                pill_button(
                    text(format!("Backend: {}", self.settings.player_backend.label())),
                    Message::CycleBackend,
                    day,
                    true,
                ),
                pill_button(
                    text(if self.settings.hwdec {
                        "HW decode ON"
                    } else {
                        "HW decode OFF"
                    }),
                    Message::ToggleHwdec,
                    day,
                    false,
                ),
                pill_button(
                    text(if self.settings.low_latency {
                        "Low-latency ON"
                    } else {
                        "Low-latency OFF"
                    }),
                    Message::ToggleLowLatency,
                    day,
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
            .color(ink_muted(day)),
            text("Backends détectés").size(14),
            scrollable(be_list).height(Fill),
        ]
        .spacing(10)
        .height(Fill);

        browser::pane(day, Length::Fill, body)
    }

    fn view_epg(&self, day: bool) -> Element<'_, Message> {
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
                day,
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
                day,
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
            day,
            Length::Fill,
            column![
                browser::content_header(
                    day,
                    "Guide EPG".into(),
                    format!("{} programmes en cache", self.bundle.epg.len()),
                    &self.search,
                ),
                scrollable(items).height(Fill),
            ]
            .spacing(10)
            .height(Fill),
        )
    }

    fn view_sources(&self, day: bool) -> Element<'_, Message> {
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
                day,
                self.form_kind == k,
            )
        }))
        .spacing(8)
        .wrap();

        let form = column![
            text("Nouvelle source").size(16),
            kind_row,
            field("Nom", &self.form_name, Message::FormName, day),
            field(
                match self.form_kind {
                    SourceKind::Xtream => "URL serveur",
                    SourceKind::Stalker => "URL portail",
                    SourceKind::Xmltv => "URL XMLTV",
                    _ => "URL / chemin / corps M3U",
                },
                &self.form_endpoint,
                Message::FormEndpoint,
                day,
            ),
            if matches!(self.form_kind, SourceKind::Xtream) {
                row![
                    field("Utilisateur", &self.form_user, Message::FormUser, day),
                    field("Mot de passe", &self.form_pass, Message::FormPass, day),
                ]
                .spacing(8)
                .into()
            } else if self.form_kind == SourceKind::Stalker {
                field("Adresse MAC", &self.form_mac, Message::FormMac, day)
            } else {
                Space::new().height(0).into()
            },
            field("EPG XMLTV (optionnel)", &self.form_epg, Message::FormEpg, day),
            row![
                pill_button(text("Ajouter"), Message::AddSource, day, true),
                pill_button(text("Fichier M3U…"), Message::PickPlaylistFile, day, false),
                pill_button(text("Diagnostiquer"), Message::DiagnosePortals, day, false),
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
                        day,
                        s.enabled,
                        None,
                    ),
                    pill_button(text("↻"), Message::ReloadSource(s.id), day, false),
                    pill_button(text("✕"), Message::RemoveSource(s.id), day, false),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }

        browser::pane(
            day,
            Length::Fill,
            column![
                browser::content_header(
                    day,
                    "Sources".into(),
                    format!("{} enregistrées", self.sources.len()),
                    &self.search,
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


fn muted_fg(day: bool) -> Color {
    if day {
        Color::from_rgba8(0x12, 0x1A, 0x24, 0.55)
    } else {
        Color::from_rgba8(0xE8, 0xEE, 0xF5, 0.55)
    }
}


fn pill_button<'a>(
    label: impl Into<Element<'a, Message>>,
    on_press: Message,
    day: bool,
    primary: bool,
) -> Element<'a, Message> {
    let label = label.into();
    button(label)
        .padding(Padding::from([10, 16]))
        .on_press(on_press)
        .style(move |theme: &Theme, status| {
            let palette = theme.extended_palette();
            let mut base = if primary {
                button::primary(theme, status)
            } else {
                button::secondary(theme, status)
            };
            base.border.radius = RADIUS_LG.into();
            if primary {
                base.text_color = on_primary(day);
            }
            let _ = palette;
            base
        })
        .into()
}


fn chip(label: String, msg: Message, day: bool, active: bool) -> Element<'static, Message> {
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
                s.text_color = on_primary(day);
            }
            s
        })
        .into()
}


fn field<'a>(
    label: &'a str,
    value: &str,
    on_input: impl Fn(String) -> Message + 'a,
    day: bool,
) -> Element<'a, Message> {
    column![
        text(label).size(12).color(muted_fg(day)),
        text_input(label, value)
            .on_input(on_input)
            .padding(12)
            .size(14)
            .style(move |theme: &Theme, status| {
                let mut s = text_input::default(theme, status);
                s.border.radius = RADIUS_MD.into();
                s.background = Background::Color(surface_muted(day));
                s
            }),
    ]
    .spacing(4)
    .width(Fill)
    .into()
}


