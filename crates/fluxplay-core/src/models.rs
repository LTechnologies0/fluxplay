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

/// How FluxPlay resolves hostnames for catalog / metadata / art HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsMode {
    /// OS resolver (default).
    #[default]
    System,
    /// Classic DNS (UDP/TCP) to the listed servers.
    Custom,
    /// DNS over HTTPS (RFC 8484).
    Doh,
    /// DNS over TLS (RFC 7858).
    Dot,
}

impl DnsMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Système",
            Self::Custom => "DNS classique",
            Self::Doh => "DNS over HTTPS",
            Self::Dot => "DNS over TLS",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::System => Self::Custom,
            Self::Custom => Self::Doh,
            Self::Doh => Self::Dot,
            Self::Dot => Self::System,
        }
    }
}

/// App-scoped network prefs (HTTP DNS + optional WireGuard profile).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSettings {
    #[serde(default)]
    pub dns_mode: DnsMode,
    /// Comma/space separated hosts, e.g. `1.1.1.1, 9.9.9.9` or `1.1.1.1:53`.
    #[serde(default)]
    pub dns_servers: String,
    /// DoH endpoint, e.g. `https://cloudflare-dns.com/dns-query`.
    #[serde(default = "default_doh_url")]
    pub doh_url: String,
    /// DoT host or `host:853`, e.g. `1.1.1.1` / `dns.google`.
    #[serde(default = "default_dot_server")]
    pub dot_server: String,
    /// Use the imported WireGuard profile for this app (app-scoped SOCKS tunnel).
    #[serde(default)]
    pub wireguard_enabled: bool,
    /// Absolute path to the active `.conf` (copied under the FluxPlay config dir).
    #[serde(default)]
    pub wireguard_profile_path: String,
    /// Display name from the last imported profile.
    #[serde(default)]
    pub wireguard_profile_name: String,
    /// `DNS=` from the WG profile — used only to resolve the peer Endpoint before the tunnel is up.
    /// Never used as the app's day-to-day resolver (that is `dns_mode` / `dns_servers` / DoH / DoT).
    #[serde(default)]
    pub wireguard_bootstrap_dns: String,
}

fn default_doh_url() -> String {
    "https://cloudflare-dns.com/dns-query".into()
}

fn default_dot_server() -> String {
    "1.1.1.1".into()
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            dns_mode: DnsMode::System,
            dns_servers: "1.1.1.1, 9.9.9.9".into(),
            doh_url: default_doh_url(),
            dot_server: default_dot_server(),
            wireguard_enabled: false,
            wireguard_profile_path: String::new(),
            wireguard_profile_name: String::new(),
            wireguard_bootstrap_dns: String::new(),
        }
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

/// Soft / present resolution ceiling (360p → 4K). Auto = SoC SoftBudget / Surface full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoQualityPref {
    #[default]
    Auto,
    P360,
    P480,
    P720,
    P1080,
    P1440,
    P2160,
}

impl VideoQualityPref {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (SoC)",
            Self::P360 => "360p",
            Self::P480 => "480p",
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P1440 => "1440p",
            Self::P2160 => "4K / UHD",
        }
    }

    /// Max soft-present size when forced (even dims).
    pub fn max_wh(self) -> Option<(u32, u32)> {
        match self {
            Self::Auto => None,
            Self::P360 => Some((640, 360)),
            Self::P480 => Some((854, 480)),
            Self::P720 => Some((1280, 720)),
            Self::P1080 => Some((1920, 1080)),
            Self::P1440 => Some((2560, 1440)),
            Self::P2160 => Some((3840, 2160)),
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::P360,
            Self::P360 => Self::P480,
            Self::P480 => Self::P720,
            Self::P720 => Self::P1080,
            Self::P1080 => Self::P1440,
            Self::P1440 => Self::P2160,
            Self::P2160 => Self::Auto,
        }
    }
}

/// Panel tone profile — LED/LCD vivid vs AMOLED true-black.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayPanelPref {
    #[default]
    Auto,
    LedLcd,
    Amoled,
}

impl DisplayPanelPref {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (détecté)",
            Self::LedLcd => "LED / LCD (vif)",
            Self::Amoled => "AMOLED (noirs profonds)",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::LedLcd,
            Self::LedLcd => Self::Amoled,
            Self::Amoled => Self::Auto,
        }
    }

    /// lavfi `eq=` fragment when a tone curve should run (soft path only).
    pub fn eq_filter(self, oled_hint: bool) -> Option<&'static str> {
        let kind = match self {
            Self::Auto => {
                if oled_hint {
                    Self::Amoled
                } else {
                    return None; // neutral — no forced LED pop
                }
            }
            other => other,
        };
        match kind {
            Self::Auto => None,
            Self::LedLcd => Some("eq=saturation=1.10:contrast=1.06:gamma=1.0"),
            Self::Amoled => Some("eq=gamma=0.92:contrast=1.10:saturation=1.02:brightness=-0.02"),
        }
    }

    /// mpv color properties (−100…100). Safe on MediaCodec Surface (no vf).
    pub fn color_adjust(self, oled_hint: bool, night: bool) -> (i32, i32, i32, i32) {
        let kind = match self {
            Self::Auto if oled_hint => Self::Amoled,
            Self::Auto => Self::LedLcd,
            other => other,
        };
        let (mut b, mut c, mut s, mut g) = match (self, kind) {
            (Self::Auto, Self::LedLcd) => (0, 0, 0, 0),
            (_, Self::LedLcd) => (0, 6, 10, 0),
            (_, Self::Amoled) => (-2, 10, 2, -8),
            _ => (0, 0, 0, 0),
        };
        if night {
            b -= 12;
            s -= 15;
            g -= 12;
            c += 5;
        }
        (b.clamp(-100, 100), c.clamp(-100, 100), s.clamp(-100, 100), g.clamp(-100, 100))
    }
}

/// mpv HDR / gamut options applied on desktop and Android.
#[derive(Debug, Clone, Copy)]
pub struct HdrMpvHints {
    pub target_colorspace_hint: bool,
    pub tone_mapping: Option<&'static str>,
    pub target_prim: Option<&'static str>,
    pub target_trc: Option<&'static str>,
    pub hdr_compute_peak: bool,
}

/// Window / Surface color mode preference (Android COLOR_MODE_* + tonemap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrPref {
    #[default]
    Auto,
    Off,
    WideGamut,
    Hdr,
    HdrPlus,
}

impl HdrPref {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "HDR Auto",
            Self::Off => "SDR forcé",
            Self::WideGamut => "Wide color (P3)",
            Self::Hdr => "HDR10 / HLG",
            Self::HdrPlus => "HDR10+ / Dolby Vision",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Off,
            Self::Off => Self::WideGamut,
            Self::WideGamut => Self::Hdr,
            Self::Hdr => Self::HdrPlus,
            Self::HdrPlus => Self::Auto,
        }
    }

    /// mpv color-management hints (desktop VO + Android fallback).
    pub fn mpv_color_hints(self, tonemap_hdr: bool) -> HdrMpvHints {
        match self {
            Self::Off => HdrMpvHints {
                target_colorspace_hint: false,
                tone_mapping: Some(if tonemap_hdr { "hable" } else { "clip" }),
                target_prim: Some("bt.709"),
                target_trc: Some("bt.1886"),
                hdr_compute_peak: false,
            },
            Self::WideGamut => HdrMpvHints {
                target_colorspace_hint: true,
                tone_mapping: None,
                target_prim: Some("display-p3"),
                target_trc: None,
                hdr_compute_peak: true,
            },
            Self::Hdr | Self::HdrPlus => HdrMpvHints {
                // Soft/iced canvas is SDR — keep PQ in the bitstream, map to display.
                target_colorspace_hint: true,
                tone_mapping: Some(if tonemap_hdr { "hable" } else { "clip" }),
                target_prim: Some("bt.2020"),
                target_trc: Some("bt.1886"),
                hdr_compute_peak: true,
            },
            Self::Auto => HdrMpvHints {
                target_colorspace_hint: true,
                tone_mapping: if tonemap_hdr { Some("hable") } else { None },
                target_prim: None,
                target_trc: None,
                hdr_compute_peak: true,
            },
        }
    }

    /// JNI mode string for [`setDisplayColorMode`].
    pub fn android_color_mode(self, panel_hdr: bool, has_hdr_plus: bool) -> &'static str {
        match self {
            Self::Off => "default",
            Self::WideGamut => "wide",
            Self::Hdr => "hdr",
            Self::HdrPlus => {
                if has_hdr_plus || panel_hdr {
                    "hdr"
                } else {
                    "wide"
                }
            }
            Self::Auto => {
                if panel_hdr {
                    "hdr"
                } else {
                    "wide"
                }
            }
        }
    }
}

/// Android present path preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AndroidPresentPref {
    #[default]
    Auto,
    Surface,
    Soft,
    GpuEgl,
}

impl AndroidPresentPref {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Présent Auto",
            Self::Surface => "Surface MediaCodec",
            Self::Soft => "Soft RGBA",
            Self::GpuEgl => "GPU / EGL",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Surface,
            Self::Surface => Self::Soft,
            Self::Soft => Self::GpuEgl,
            Self::GpuEgl => Self::Auto,
        }
    }

    pub fn env_override(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Surface => Some("surface"),
            Self::Soft => Some("soft"),
            Self::GpuEgl => Some("gpu"),
        }
    }
}

/// Default image prefs applied at each play (also editable live in the player).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AspectPref {
    #[default]
    Auto,
    R16x9,
    R4x3,
    R235,
}

impl AspectPref {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Format auto",
            Self::R16x9 => "16:9",
            Self::R4x3 => "4:3",
            Self::R235 => "2.35:1",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::R16x9,
            Self::R16x9 => Self::R4x3,
            Self::R4x3 => Self::R235,
            Self::R235 => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeinterlacePref {
    #[default]
    Off,
    On,
    Auto,
}

impl DeinterlacePref {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Désentrelacement off",
            Self::On => "Désentrelacement ON",
            Self::Auto => "Désentrelacement auto",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::On,
            Self::On => Self::Auto,
            Self::Auto => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpscalePref {
    #[default]
    Auto,
    Bilinear,
    Lanczos,
    EwaLanczos,
    Nearest,
}

impl UpscalePref {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Upscale auto",
            Self::Bilinear => "Bilinéaire",
            Self::Lanczos => "Lanczos",
            Self::EwaLanczos => "EWA Lanczos (HQ)",
            Self::Nearest => "Nearest",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Bilinear,
            Self::Bilinear => Self::Lanczos,
            Self::Lanczos => Self::EwaLanczos,
            Self::EwaLanczos => Self::Nearest,
            Self::Nearest => Self::Auto,
        }
    }
}

/// User FPS ceiling for GUI timers / soft video present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FpsCapPref {
    /// Derive from monitor Hz + GPU tier (+ content FPS for video).
    #[default]
    Auto,
    Hz30,
    Hz60,
    Hz90,
    Hz120,
    Hz144,
    Hz165,
    Hz240,
}

impl FpsCapPref {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Hz30 => "30",
            Self::Hz60 => "60",
            Self::Hz90 => "90",
            Self::Hz120 => "120",
            Self::Hz144 => "144",
            Self::Hz165 => "165",
            Self::Hz240 => "240",
        }
    }

    pub fn fixed_hz(self) -> Option<u32> {
        match self {
            Self::Auto => None,
            Self::Hz30 => Some(30),
            Self::Hz60 => Some(60),
            Self::Hz90 => Some(90),
            Self::Hz120 => Some(120),
            Self::Hz144 => Some(144),
            Self::Hz165 => Some(165),
            Self::Hz240 => Some(240),
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Hz30,
            Self::Hz30 => Self::Hz60,
            Self::Hz60 => Self::Hz90,
            Self::Hz90 => Self::Hz120,
            Self::Hz120 => Self::Hz144,
            Self::Hz144 => Self::Hz165,
            Self::Hz165 => Self::Hz240,
            Self::Hz240 => Self::Auto,
        }
    }
}

/// GPU class used only as a soft-budget hint (not a hard FPS oracle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuTier {
    #[default]
    Unknown,
    Integrated,
    Discrete,
}

impl GpuTier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "inconnu",
            Self::Integrated => "intégré",
            Self::Discrete => "dédié",
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
    /// Cap for GUI periodic work (chrome autohide, prefetch pacing).
    #[serde(default)]
    pub fps_gui: FpsCapPref,
    /// Cap for embedded video present (PlayerTick soft-frame pulls).
    #[serde(default)]
    pub fps_video: FpsCapPref,
    /// Soft/Surface resolution ceiling (360p → 4K).
    #[serde(default)]
    pub video_quality: VideoQualityPref,
    /// LED/LCD vs AMOLED tone curve.
    #[serde(default)]
    pub display_panel: DisplayPanelPref,
    /// SDR / wide gamut / HDR / HDR+.
    #[serde(default)]
    pub hdr_mode: HdrPref,
    /// Android present path (ignored on desktop).
    #[serde(default)]
    pub android_present: AndroidPresentPref,
    /// Default image aspect for new playback.
    #[serde(default)]
    pub aspect: AspectPref,
    #[serde(default)]
    pub deinterlace: DeinterlacePref,
    #[serde(default)]
    pub upscale: UpscalePref,
    /// Soft night eq (gamma/sat) as default.
    #[serde(default)]
    pub night_mode: bool,
    /// Soft tonemap for HDR→SDR on soft present.
    #[serde(default = "default_true")]
    pub tonemap_hdr: bool,
    #[serde(default)]
    pub favorites: Vec<String>,
    #[serde(default)]
    pub recent: Vec<RecentChannel>,
    /// OMDb API key (IMDb gateway). Env `OMDB_API_KEY` overrides when set.
    #[serde(default)]
    pub omdb_api_key: String,
    /// DNS / WireGuard prefs for app HTTP (catalog, art, metadata).
    #[serde(default)]
    pub network: NetworkSettings,
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
            fps_gui: FpsCapPref::Auto,
            fps_video: FpsCapPref::Auto,
            video_quality: VideoQualityPref::Auto,
            display_panel: DisplayPanelPref::Auto,
            hdr_mode: HdrPref::Auto,
            android_present: AndroidPresentPref::Auto,
            aspect: AspectPref::Auto,
            deinterlace: DeinterlacePref::Off,
            upscale: UpscalePref::Auto,
            night_mode: false,
            tonemap_hdr: true,
            favorites: Vec::new(),
            recent: Vec::new(),
            omdb_api_key: String::new(),
            network: NetworkSettings::default(),
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

