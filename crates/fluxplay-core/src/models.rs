use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};
use uuid::Uuid;

use crate::protocol::StreamScheme;

/// How the user authenticates / discovers content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    M3u,
    M3uPlus,
    Xtream,
    Stalker,
    Xmltv,
    DirectUrl,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::M3u => "M3U",
            Self::M3uPlus => "M3U Plus",
            Self::Xtream => "Xtream Codes",
            Self::Stalker => "Stalker Portal",
            Self::Xmltv => "XMLTV / EPG",
            Self::DirectUrl => "URL directe",
        }
    }
}

/// Persisted IPTV source (playlist / portal / EPG).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaSource {
    pub id: Uuid,
    pub name: String,
    pub kind: SourceKind,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Never log this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Stalker MAC address (AA:BB:CC:DD:EE:FF).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epg_url: Option<String>,
    /// Per-playlist User-Agent (IPTVnator-style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_referer: Option<String>,
    #[serde(default)]
    pub auto_refresh: bool,
    pub created_at: DateTime<Utc>,
    #[serde(default = "default_source_enabled")]
    pub enabled: bool,
}

fn default_source_enabled() -> bool {
    true
}

impl MediaSource {
    pub fn new(name: impl Into<String>, kind: SourceKind, endpoint: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            endpoint: endpoint.into(),
            username: None,
            password: None,
            mac: None,
            epg_url: None,
            user_agent: None,
            http_referer: None,
            auto_refresh: true,
            created_at: Utc::now(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub stream_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvg_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvg_logo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epg_channel_id: Option<String>,
    #[serde(default)]
    pub scheme: Option<StreamScheme>,
    #[serde(default)]
    pub source_id: Option<Uuid>,
    #[serde(default)]
    pub kind: ContentKind,
    /// Catch-up / archive (M3U Plus / IPTVnator).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catchup: Option<CatchupInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatchupInfo {
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    #[default]
    Live,
    Vod,
    Series,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub content: ContentKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VodItem {
    pub id: String,
    pub name: String,
    pub stream_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actors: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub director: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awards: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<String>,
    pub source_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesItem {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actors: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub director: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awards: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default)]
    pub seasons: Vec<SeriesSeason>,
    pub source_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesSeason {
    pub season_number: u32,
    pub episodes: Vec<SeriesEpisode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesEpisode {
    pub id: String,
    pub title: String,
    pub stream_url: String,
    pub episode_num: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub airdate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub still: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpgProgramme {
    pub channel_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub start: DateTime<Utc>,
    pub stop: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistBundle {
    pub channels: Vec<Channel>,
    pub categories: Vec<Category>,
    pub vod: Vec<VodItem>,
    pub series: Vec<SeriesItem>,
    pub epg: Vec<EpgProgramme>,
}

impl PlaylistBundle {
    pub fn group_names(&self) -> Vec<String> {
        let mut groups: Vec<String> = self
            .channels
            .iter()
            .filter_map(|c| c.group.clone())
            .collect();
        groups.sort();
        groups.dedup();
        groups
    }

    pub fn live_in_group<'a>(&'a self, group: Option<&str>) -> Vec<&'a Channel> {
        self.channels
            .iter()
            .filter(|c| c.kind == ContentKind::Live)
            .filter(|c| match group {
                None | Some("Tous") => true,
                Some(g) => c.group.as_deref() == Some(g),
            })
            .collect()
    }

    /// Current + next programme for a channel (tvg-id / epg id / stream id match).
    /// Single pass — no per-call Vec alloc/sort (hot path: live list paint).
    pub fn now_next(&self, channel: &Channel, at: DateTime<Utc>) -> (Option<&EpgProgramme>, Option<&EpgProgramme>) {
        let k0 = channel.epg_channel_id.as_deref();
        let k1 = channel.tvg_id.as_deref();
        let k2 = channel.id.as_str();
        let matches = |cid: &str| {
            k0 == Some(cid) || k1 == Some(cid) || cid == k2
        };

        let mut now: Option<&EpgProgramme> = None;
        let mut next: Option<&EpgProgramme> = None;
        for p in &self.epg {
            if !matches(&p.channel_id) {
                continue;
            }
            if p.start <= at && at < p.stop {
                // Prefer the tightest window if duplicates exist.
                now = Some(match now {
                    Some(n) if n.start >= p.start => n,
                    _ => p,
                });
            } else if p.start >= at {
                next = Some(match next {
                    Some(n) if n.start <= p.start => n,
                    _ => p,
                });
            }
        }
        if let Some(n) = now {
            // Next must start at/after current programme end.
            next = self
                .epg
                .iter()
                .filter(|p| matches(&p.channel_id) && p.start >= n.stop)
                .min_by_key(|p| p.start);
        }
        (now, next)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Day,
    Night,
    System,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "Jour",
            Self::Night => "Nuit",
            Self::System => "Système",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Day => Self::Night,
            Self::Night => Self::System,
            Self::System => Self::Day,
        }
    }
}

/// Accent / brand color family for the GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccentPreset {
    #[default]
    Teal,
    Ocean,
    Ember,
    Violet,
    Forest,
    Rose,
    Slate,
    // Extended mosaic palette
    Cyan,
    Sky,
    Azure,
    Indigo,
    Grape,
    Magenta,
    HotPink,
    Coral,
    Scarlet,
    Crimson,
    Wine,
    Peach,
    Amber,
    Gold,
    Lime,
    Mint,
    Jade,
    Olive,
    Sand,
    Chocolate,
    Charcoal,
    Ice,
    Neon,
}

impl AccentPreset {
    pub fn label(self) -> &'static str {
        match self {
            Self::Teal => "Teal",
            Self::Ocean => "Océan",
            Self::Ember => "Ember",
            Self::Violet => "Violet",
            Self::Forest => "Forêt",
            Self::Rose => "Rose",
            Self::Slate => "Ardoise",
            Self::Cyan => "Cyan",
            Self::Sky => "Ciel",
            Self::Azure => "Azur",
            Self::Indigo => "Indigo",
            Self::Grape => "Raisin",
            Self::Magenta => "Magenta",
            Self::HotPink => "Rose vif",
            Self::Coral => "Corail",
            Self::Scarlet => "Écarlate",
            Self::Crimson => "Cramoisi",
            Self::Wine => "Vin",
            Self::Peach => "Pêche",
            Self::Amber => "Ambre",
            Self::Gold => "Or",
            Self::Lime => "Citron vert",
            Self::Mint => "Menthe",
            Self::Jade => "Jade",
            Self::Olive => "Olive",
            Self::Sand => "Sable",
            Self::Chocolate => "Chocolat",
            Self::Charcoal => "Charbon",
            Self::Ice => "Glace",
            Self::Neon => "Néon",
        }
    }

    pub fn all() -> &'static [AccentPreset] {
        &[
            Self::Teal,
            Self::Cyan,
            Self::Mint,
            Self::Jade,
            Self::Forest,
            Self::Lime,
            Self::Olive,
            Self::Ocean,
            Self::Sky,
            Self::Azure,
            Self::Indigo,
            Self::Violet,
            Self::Grape,
            Self::Magenta,
            Self::HotPink,
            Self::Rose,
            Self::Coral,
            Self::Scarlet,
            Self::Crimson,
            Self::Wine,
            Self::Ember,
            Self::Peach,
            Self::Amber,
            Self::Gold,
            Self::Sand,
            Self::Chocolate,
            Self::Slate,
            Self::Charcoal,
            Self::Ice,
            Self::Neon,
        ]
    }

    pub fn cycle(self) -> Self {
        let all = Self::all();
        let i = all.iter().position(|a| *a == self).unwrap_or(0);
        all[(i + 1) % all.len()]
    }
}

/// Preferred native decode engine (desktop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerBackendPref {
    #[default]
    Auto,
    Mpv,
    Ffmpeg,
    External,
}

impl PlayerBackendPref {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (libmpv → FFmpeg)",
            Self::Mpv => "libmpv",
            Self::Ffmpeg => "FFmpeg / ffplay",
            Self::External => "Système",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Mpv,
            Self::Mpv => Self::Ffmpeg,
            Self::Ffmpeg => Self::External,
            Self::External => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub theme: ThemeMode,
    #[serde(default)]
    pub accent: AccentPreset,
    pub last_source_id: Option<Uuid>,
    pub volume: f32,
    pub remember_position: bool,
    #[serde(default)]
    pub player_backend: PlayerBackendPref,
    #[serde(default = "default_true")]
    pub hwdec: bool,
    #[serde(default = "default_cache_ms")]
    pub cache_ms: u32,
    #[serde(default = "default_demux_secs")]
    pub demux_secs: f32,
    #[serde(default)]
    pub low_latency: bool,
    /// After ~75% of a series episode, prefetch the next episode to disk.
    #[serde(default = "default_true")]
    pub prefetch_next_episode: bool,
    #[serde(default)]
    pub favorites: Vec<String>,
    #[serde(default)]
    pub recent: Vec<RecentChannel>,
    /// OMDb API key (IMDb gateway). Env `OMDB_API_KEY` overrides when set.
    #[serde(default)]
    pub omdb_api_key: String,
}

fn default_true() -> bool {
    true
}
fn default_cache_ms() -> u32 {
    4000
}
fn default_demux_secs() -> f32 {
    8.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentChannel {
    pub channel_id: String,
    pub name: String,
    pub stream_url: String,
    pub played_at: DateTime<Utc>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            accent: AccentPreset::Teal,
            last_source_id: None,
            volume: 0.85,
            remember_position: true,
            player_backend: PlayerBackendPref::Auto,
            hwdec: true,
            cache_ms: 4000,
            demux_secs: 8.0,
            low_latency: false,
            prefetch_next_episode: true,
            favorites: Vec::new(),
            recent: Vec::new(),
            omdb_api_key: String::new(),
        }
    }
}

impl AppSettings {
    pub fn toggle_favorite(&mut self, channel_id: &str) {
        if let Some(i) = self.favorites.iter().position(|f| f == channel_id) {
            self.favorites.remove(i);
            debug!(%channel_id, favorite = false, "toggle_favorite");
        } else {
            self.favorites.push(channel_id.to_string());
            debug!(%channel_id, favorite = true, "toggle_favorite");
        }
    }

    pub fn is_favorite(&self, channel_id: &str) -> bool {
        self.favorites.iter().any(|f| f == channel_id)
    }

    pub fn push_recent(&mut self, ch: &Channel) {
        trace!(channel_id = %ch.id, name = %ch.name, "push_recent");
        self.recent.retain(|r| r.channel_id != ch.id);
        self.recent.insert(
            0,
            RecentChannel {
                channel_id: ch.id.clone(),
                name: ch.name.clone(),
                stream_url: ch.stream_url.clone(),
                played_at: Utc::now(),
            },
        );
        self.recent.truncate(30);
    }
}
