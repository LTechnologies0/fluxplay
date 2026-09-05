//! Xtream Codes API client — aligned with IPTV Smarters Pro / Expert behaviour.
//!
//! Smarters does **not** rely on `get.php` M3U dumps. It uses `player_api.php`,
//! then builds `/live|movie|series/{user}/{pass}/{id}.{m3u8|ts}` from `server_info`.

use fluxplay_core::models::{
    Category, Channel, ContentKind, MediaSource, PlaylistBundle, SeriesEpisode, SeriesItem,
    SeriesSeason, SourceKind, VodItem,
};
use fluxplay_core::protocol::StreamScheme;
use fluxplay_core::Stopwatch;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, error, info, trace, warn};
use url::Url;

use crate::{ProviderError, Result};

/// Same UA string IPTV Smarters Pro sends on many panels.
pub const SMARTERS_UA: &str = "IPTVSmartersPlayer";

#[derive(Debug, Clone)]
pub struct XtreamClient {
    /// Portal base used for API calls (may include :8080).
    pub portal: Url,
    /// Base used for media URLs (from server_info when available).
    pub stream_base: Url,
    pub username: String,
    pub password: String,
    pub source_id: uuid::Uuid,
    pub prefer_m3u8: bool,
}

impl XtreamClient {
    pub fn from_credentials(
        source_id: uuid::Uuid,
        base: &str,
        username: String,
        password: String,
    ) -> Result<Self> {
        let mut base = base.trim().to_string();
        if !base.contains("://") {
            base = format!("http://{base}");
        }
        let mut url = Url::parse(&base).map_err(|e| ProviderError::Message(e.to_string()))?;
        url.set_path("");
        url.set_query(None);
        url.set_fragment(None);
        // Ensure trailing slash semantics for format!
        // Never log password.
        debug!(
            source_id = %source_id,
            portal = %url,
            user = %username,
            "XtreamClient from_credentials"
        );
        Ok(Self {
            stream_base: url.clone(),
            portal: url,
            username,
            password,
            source_id,
            prefer_m3u8: true,
        })
    }

    pub fn from_source(source: &MediaSource) -> Result<Self> {
        if source.kind != SourceKind::Xtream {
            return Err(ProviderError::Unsupported(source.kind));
        }
        let username = source.username.clone().ok_or_else(|| {
            error!(source_id = %source.id, "Xtream username missing");
            ProviderError::Auth("Xtream username required".into())
        })?;
        let password = source.password.clone().ok_or_else(|| {
            error!(source_id = %source.id, "Xtream password missing");
            ProviderError::Auth("Xtream password required".into())
        })?;
        Self::from_credentials(source.id, &source.endpoint, username, password)
    }

    pub(crate) fn api_url(&self, action: Option<&str>) -> Result<Url> {
        let mut u = self.portal.clone();
        u.set_path("player_api.php");
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("username", &self.username);
            q.append_pair("password", &self.password);
            if let Some(a) = action {
                q.append_pair("action", a);
            }
        }
        Ok(u)
    }

    async fn get_json(&self, action: Option<&str>) -> Result<Value> {
        self.fetch_api_json(action, &[]).await
    }

    pub(crate) async fn get_json_action(
        &self,
        action: &str,
        extra: &[(&str, &str)],
    ) -> Result<Value> {
        let extras: Vec<&str> = extra.iter().flat_map(|(k, v)| [*k, *v]).collect();
        self.fetch_api_json(Some(action), &extras).await
    }

    async fn fetch_api_json(&self, action: Option<&str>, extras: &[&str]) -> Result<Value> {
        let _prof = Stopwatch::start("xtream_get_json");
        let action_label = action.unwrap_or("auth");
        // Do not log password or full auth URL — action + portal only.
        debug!(
            portal = %self.portal,
            %action_label,
            extras = extras.len(),
            "xtream get_json"
        );
        let key = crate::api_cache::key(
            self.portal.as_str(),
            &self.username,
            action_label,
            extras,
        );
        let ttl = crate::api_cache::ttl_for_action(action);
        if let Some(cached) = crate::api_cache::get_fresh(&key, ttl) {
            return Ok(cached);
        }

        let mut url = self.api_url(action)?;
        {
            let mut q = url.query_pairs_mut();
            let mut it = extras.iter();
            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                q.append_pair(k, v);
            }
        }

        let fetch = || async {
            let client = crate::api_cache::shared_http()?;
            let resp = client
                .get(url.clone())
                .header(reqwest::header::USER_AGENT, SMARTERS_UA)
                .send()
                .await?;
            let status = resp.status();
            if status.as_u16() == 429 {
                warn!(%action_label, "HTTP 429 from portal");
                return Err(ProviderError::Message(
                    "HTTP 429 Too Many Requests — ralentissez / cache".into(),
                ));
            }
            if !status.is_success() {
                warn!(%action_label, status = %status, "xtream API HTTP error");
                return Err(crate::xtream_url::map_http_error(
                    resp.error_for_status().unwrap_err(),
                ));
            }
            Ok(resp.json::<Value>().await?)
        };

        match crate::api_cache::with_portal_limit(fetch).await {
            Ok(value) => {
                crate::api_cache::put(&key, &value);
                trace!(%action_label, "xtream get_json OK");
                Ok(value)
            }
            Err(e) => {
                if let Some(stale) = crate::api_cache::get_stale(&key) {
                    warn!(error = %e, %action_label, "serving stale xtream cache");
                    Ok(stale)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Apply `server_info` like Smarters (host/port/protocol for media paths).
    fn apply_server_info(&mut self, auth: &Value) {
        let Some(si) = auth.get("server_info") else {
            return;
        };
        let host = si
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if host.is_empty() {
            return;
        }
        let proto = si
            .get("server_protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("http");
        let port = si
            .get("port")
            .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| v.as_u64().map(|n| n.to_string())))
            .unwrap_or_else(|| "80".into());

        // Keep portal port if server advertises :80 but we reached API on :8080
        // and streams also work on the portal port (common CDN split). Prefer portal
        // when its host matches.
        let portal_host = self.portal.host_str().unwrap_or("");
        if portal_host.eq_ignore_ascii_case(host) {
            // Same host: use portal origin (preserves :8080 when needed).
            self.stream_base = self.portal.clone();
        } else if let Ok(mut u) = Url::parse(&format!("{proto}://{host}:{port}/")) {
            let _ = u.set_host(Some(host));
            self.stream_base = u;
        }

        if let Some(formats) = auth
            .pointer("/user_info/allowed_output_formats")
            .and_then(|v| v.as_array())
        {
            let has_m3u8 = formats.iter().any(|f| f.as_str() == Some("m3u8"));
            let has_ts = formats.iter().any(|f| f.as_str() == Some("ts"));
            self.prefer_m3u8 = has_m3u8 || !has_ts;
        }

        info!(
            portal = %self.portal,
            stream_base = %self.stream_base,
            prefer_m3u8 = self.prefer_m3u8,
            "Xtream server_info applied (Smarters-compatible)"
        );
    }

    fn media_ext<'a>(&'a self, container: Option<&'a str>) -> &'a str {
        if let Some(c) = container {
            if !c.is_empty() {
                return c;
            }
        }
        if self.prefer_m3u8 {
            "m3u8"
        } else {
            "ts"
        }
    }

    fn base_str(&self) -> String {
        let mut s = self.stream_base.as_str().to_string();
        if !s.ends_with('/') {
            s.push('/');
        }
        s
    }

    pub fn live_stream_url(&self, stream_id: &str, ext: &str) -> String {
        format!(
            "{}live/{}/{}/{}.{}",
            self.base_str(),
            self.username,
            self.password,
            stream_id,
            ext
        )
    }

    pub fn vod_stream_url(&self, stream_id: &str, ext: &str) -> String {
        format!(
            "{}movie/{}/{}/{}.{}",
            self.base_str(),
            self.username,
            self.password,
            stream_id,
            ext
        )
    }

    pub fn series_stream_url(&self, stream_id: &str, ext: &str) -> String {
        format!(
            "{}series/{}/{}/{}.{}",
            self.base_str(),
            self.username,
            self.password,
            stream_id,
            ext
        )
    }

    pub async fn load_bundle(&mut self) -> Result<PlaylistBundle> {
        let _prof = Stopwatch::start("xtream_load_bundle");
        info!(
            source_id = %self.source_id,
            portal = %self.portal,
            user = %self.username,
            "Xtream load_bundle start"
        );
        // Auth + server_info first (exactly like Smarters login).
        let auth = self.get_json(None).await?;
        if auth.pointer("/user_info/auth").and_then(|v| v.as_u64()) == Some(0)
            || auth.pointer("/user_info/auth").and_then(|v| v.as_i64()) == Some(0)
        {
            error!(
                portal = %self.portal,
                user = %self.username,
                "Xtream auth failed"
            );
            return Err(ProviderError::Auth(
                "Xtream auth failed (identifiants ou panel)".into(),
            ));
        }
        debug!(portal = %self.portal, "Xtream auth OK");
        self.apply_server_info(&auth);

        // Parallel catalog headers (live streams + all category lists).
        fluxplay_core::profiler!(step = "catalog_headers", "parallel live/vod/series categories");
        let (live_cats, live, vod_cats, series_cats) = tokio::join!(
            self.get_json(Some("get_live_categories")),
            self.get_json(Some("get_live_streams")),
            self.get_json(Some("get_vod_categories")),
            self.get_json(Some("get_series_categories")),
        );
        let live_cats = live_cats.ok();
        let live = live?;
        let vod_cats = vod_cats.ok();
        let series_cats = series_cats.ok();
        debug!(
            live_cats = live_cats.is_some(),
            vod_cats = vod_cats.is_some(),
            series_cats = series_cats.is_some(),
            "catalog headers fetched"
        );

        let mut categories = Vec::new();
        if let Some(Value::Array(arr)) = live_cats {
            for item in arr {
                if let Ok(c) = serde_json::from_value::<XcCategory>(item) {
                    categories.push(Category {
                        id: c.category_id,
                        name: c.category_name,
                        content: ContentKind::Live,
                    });
                }
            }
        }
        let mut vod_cat_list = Vec::new();
        if let Some(Value::Array(arr)) = vod_cats {
            for item in arr {
                if let Ok(c) = serde_json::from_value::<XcCategory>(item) {
                    vod_cat_list.push(c.category_id.clone());
                    categories.push(Category {
                        id: c.category_id,
                        name: c.category_name,
                        content: ContentKind::Vod,
                    });
                }
            }
        }
        let mut series_cat_list = Vec::new();
        if let Some(Value::Array(arr)) = series_cats {
            for item in arr {
                if let Ok(c) = serde_json::from_value::<XcCategory>(item) {
                    series_cat_list.push(c.category_id.clone());
                    categories.push(Category {
                        id: c.category_id,
                        name: c.category_name,
                        content: ContentKind::Series,
                    });
                }
            }
        }

        let mut channels = Vec::new();
        if let Value::Array(arr) = live {
            channels.reserve(arr.len());
            for item in arr {
                let Ok(s) = serde_json::from_value::<XcLiveStream>(item) else {
                    continue;
                };
                let ext = self.media_ext(s.container_extension.as_deref());
                let url = if let Some(ds) = s.direct_source.filter(|d| !d.is_empty()) {
                    ds
                } else {
                    self.live_stream_url(&s.stream_id, ext)
                };
                channels.push(Channel {
                    id: s.stream_id.clone(),
                    name: s.name,
                    stream_url: url,
                    logo: s.stream_icon,
                    group: s.category_id.clone(),
                    tvg_id: s.epg_channel_id.clone(),
                    tvg_name: None,
                    tvg_logo: None,
                    epg_channel_id: s.epg_channel_id,
                    scheme: Some(StreamScheme::Http),
                    source_id: Some(self.source_id),
                    kind: ContentKind::Live,
                    catchup: None,
                });
            }
        }

        let live_cat_map: std::collections::HashMap<_, _> = categories
            .iter()
            .filter(|c| c.content == ContentKind::Live)
            .map(|c| (c.id.clone(), c.name.clone()))
            .collect();
        for ch in &mut channels {
            if let Some(gid) = &ch.group {
                if let Some(name) = live_cat_map.get(gid) {
                    ch.group = Some(name.clone());
                }
            }
        }

        // Prefetch: try full dumps first (1 call each), else all non-adult categories.
        let (vod, series) = self.load_vod_and_series_full(&categories).await;

        info!(
            channels = channels.len(),
            vod = vod.len(),
            series = series.len(),
            categories = categories.len(),
            vod_cats = vod_cat_list.len(),
            series_cats = series_cat_list.len(),
            "Xtream catalog loaded (live + VOD/series categories + parallel prefetch)"
        );

        Ok(PlaylistBundle {
            channels,
            categories,
            vod,
            series,
            epg: Vec::new(),
        })
    }

    /// Prefer panel-wide dumps; fall back to walking every non-adult category.
    async fn load_vod_and_series_full(
        &self,
        categories: &[Category],
    ) -> (Vec<VodItem>, Vec<SeriesItem>) {
        let _prof = Stopwatch::start("xtream_vod_series_full");
        let (vod_dump, series_dump) = tokio::join!(
            self.try_load_all_vod(),
            self.try_load_all_series(),
        );

        let mut vod = vod_dump.unwrap_or_default();
        let mut series = series_dump.unwrap_or_default();

        let vod_ids: Vec<String> = if vod.is_empty() {
            categories
                .iter()
                .filter(|c| c.content == ContentKind::Vod && !is_adult_category(&c.name))
                .map(|c| c.id.clone())
                .collect()
        } else {
            Vec::new()
        };
        let series_ids: Vec<String> = if series.is_empty() {
            categories
                .iter()
                .filter(|c| c.content == ContentKind::Series && !is_adult_category(&c.name))
                .map(|c| c.id.clone())
                .collect()
        } else {
            Vec::new()
        };

        if !vod_ids.is_empty() || !series_ids.is_empty() {
            info!(
                vod_cats = vod_ids.len(),
                series_cats = series_ids.len(),
                "dumps empty — syncing categories in parallel"
            );
            let (vod_fb, series_fb) = tokio::join!(
                async {
                    if vod_ids.is_empty() {
                        Vec::new()
                    } else {
                        self.load_vod_categories_parallel(&vod_ids, 2).await
                    }
                },
                async {
                    if series_ids.is_empty() {
                        Vec::new()
                    } else {
                        self.load_series_categories_parallel(&series_ids, 2).await
                    }
                },
            );
            if vod.is_empty() {
                vod = vod_fb;
            }
            if series.is_empty() {
                series = series_fb;
            }
        }

        (vod, series)
    }

    async fn try_load_all_vod(&self) -> Option<Vec<VodItem>> {
        let value = self.get_json(Some("get_vod_streams")).await.ok()?;
        let Value::Array(arr) = value else {
            return None;
        };
        if arr.is_empty() || arr.len() < 8 {
            // Tiny payloads often mean "need category_id".
            return None;
        }
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let Ok(s) = serde_json::from_value::<XcVodStream>(item) else {
                continue;
            };
            let ext = self.media_ext(s.container_extension.as_deref());
            let url = if let Some(ds) = s.direct_source.filter(|d| !d.is_empty()) {
                ds
            } else {
                self.vod_stream_url(&s.stream_id, ext)
            };
            out.push(VodItem {
                id: s.stream_id,
                name: s.name,
                stream_url: url,
                poster: s.stream_icon,
                plot: s.plot,
                year: s.year.or(s.release_date),
                rating: s.rating,
                genre: s.genre,
                imdb_id: s.imdb_id.filter(|id| !id.is_empty() && id != "0"),
                actors: None,
                director: None,
                writer: None,
                runtime: None,
                rated: None,
                awards: None,
                language: None,
                country: None,
                category_id: s.category_id,
                source_id: Some(self.source_id),
            });
        }
        info!(n = out.len(), "VOD full dump loaded");
        Some(out)
    }

    async fn try_load_all_series(&self) -> Option<Vec<SeriesItem>> {
        let value = self.get_json(Some("get_series")).await.ok()?;
        let Value::Array(arr) = value else {
            return None;
        };
        if arr.is_empty() || arr.len() < 8 {
            return None;
        }
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let Ok(s) = serde_json::from_value::<XcSeries>(item) else {
                continue;
            };
            out.push(SeriesItem {
                id: s.series_id,
                name: s.name,
                cover: s.cover.or(s.cover_big.clone()),
                banner: s.cover_big,
                plot: s.plot,
                year: s.year.or(s.release_date),
                rating: s.rating,
                genre: s.genre,
                imdb_id: s.imdb_id.filter(|id| !id.is_empty() && id != "0"),
                actors: None,
                director: None,
                writer: None,
                runtime: None,
                rated: None,
                awards: None,
                language: None,
                country: None,
                seasons: Vec::new(),
                source_id: Some(self.source_id),
                category_id: s.category_id,
            });
        }
        info!(n = out.len(), "Series full dump loaded");
        Some(out)
    }

    /// Concurrent VOD category loads (capped). Safe: each task uses a cloned client.
    pub async fn load_vod_categories_parallel(
        &self,
        category_ids: &[String],
        parallel: usize,
    ) -> Vec<VodItem> {
        parallel_map_categories(self, category_ids, parallel.max(1), true)
            .await
            .0
    }

    /// Concurrent series category loads (capped).
    pub async fn load_series_categories_parallel(
        &self,
        category_ids: &[String],
        parallel: usize,
    ) -> Vec<SeriesItem> {
        parallel_map_categories(self, category_ids, parallel.max(1), false)
            .await
            .1
    }

    /// VOD streams for one category (`get_vod_streams&category_id=`).
    pub async fn load_vod_category(&self, category_id: &str) -> Result<Vec<VodItem>> {
        debug!(%category_id, "load_vod_category");
        let value = self
            .get_json_action("get_vod_streams", &[("category_id", category_id)])
            .await?;
        let mut out = Vec::new();
        let Value::Array(arr) = value else {
            return Ok(out);
        };
        out.reserve(arr.len());
        for item in arr {
            let Ok(s) = serde_json::from_value::<XcVodStream>(item) else {
                continue;
            };
            let ext = self.media_ext(s.container_extension.as_deref());
            let url = if let Some(ds) = s.direct_source.filter(|d| !d.is_empty()) {
                ds
            } else {
                self.vod_stream_url(&s.stream_id, ext)
            };
            out.push(VodItem {
                id: s.stream_id,
                name: s.name,
                stream_url: url,
                poster: s.stream_icon,
                plot: s.plot,
                year: s.year.or(s.release_date),
                rating: s.rating,
                genre: s.genre,
                imdb_id: s.imdb_id.filter(|id| !id.is_empty() && id != "0"),
                actors: None,
                director: None,
                writer: None,
                runtime: None,
                rated: None,
                awards: None,
                language: None,
                country: None,
                category_id: s.category_id.or_else(|| Some(category_id.to_string())),
                source_id: Some(self.source_id),
            });
        }
        debug!(%category_id, n = out.len(), "load_vod_category done");
        Ok(out)
    }

    /// Series list for one category (`get_series&category_id=`).
    pub async fn load_series_category(&self, category_id: &str) -> Result<Vec<SeriesItem>> {
        debug!(%category_id, "load_series_category");
        let value = self
            .get_json_action("get_series", &[("category_id", category_id)])
            .await?;
        let mut out = Vec::new();
        let Value::Array(arr) = value else {
            return Ok(out);
        };
        out.reserve(arr.len());
        for item in arr {
            let Ok(s) = serde_json::from_value::<XcSeries>(item) else {
                continue;
            };
            out.push(SeriesItem {
                id: s.series_id,
                name: s.name,
                cover: s.cover.or(s.cover_big.clone()),
                banner: s.cover_big,
                plot: s.plot,
                year: s.year.or(s.release_date),
                rating: s.rating,
                genre: s.genre,
                imdb_id: s.imdb_id.filter(|id| !id.is_empty() && id != "0"),
                actors: None,
                director: None,
                writer: None,
                runtime: None,
                rated: None,
                awards: None,
                language: None,
                country: None,
                seasons: Vec::new(),
                source_id: Some(self.source_id),
                category_id: s.category_id.or_else(|| Some(category_id.to_string())),
            });
        }
        debug!(%category_id, n = out.len(), "load_series_category done");
        Ok(out)
    }

    pub async fn series_info(&self, series_id: &str) -> Result<SeriesItem> {
        debug!(%series_id, "series_info");
        let value = self
            .get_json_action("get_series_info", &[("series_id", series_id)])
            .await?;

        let info = value.get("info").cloned().unwrap_or(Value::Null);
        let name = info
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(series_id)
            .to_string();
        let cover = info
            .get("cover")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                info.get("cover_big")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        let banner = info
            .get("backdrop_path")
            .and_then(|v| {
                if let Some(s) = v.as_str() {
                    Some(s.to_string())
                } else {
                    v.as_array()
                        .and_then(|a| a.first())
                        .and_then(|x| x.as_str())
                        .map(str::to_string)
                }
            })
            .or_else(|| {
                info.get("cover_big")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        let plot = info
            .get("plot")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let year = info
            .get("releaseDate")
            .or_else(|| info.get("release_date"))
            .or_else(|| info.get("year"))
            .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())));
        let rating = info
            .get("rating")
            .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_f64().map(|n| format!("{n:.1}"))));
        let genre = info.get("genre").and_then(|v| match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Array(a) => {
                let p: Vec<_> = a
                    .iter()
                    .filter_map(|x| x.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if p.is_empty() {
                    None
                } else {
                    Some(p.join(", "))
                }
            }
            _ => None,
        });
        let imdb_id = info
            .get("imdb_id")
            .or_else(|| info.get("imdbid"))
            .or_else(|| info.get("IMDB_ID"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "0")
            .map(str::to_string);

        let mut seasons = Vec::new();
        if let Some(episodes) = value.get("episodes").and_then(|e| e.as_object()) {
            for (season_key, eps) in episodes {
                let season_number = season_key.parse().unwrap_or(0);
                let mut list = Vec::new();
                if let Some(arr) = eps.as_array() {
                    for ep in arr {
                        let id = ep
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .or_else(|| {
                                ep.get("id")
                                    .and_then(|v| v.as_u64())
                                    .map(|n| n.to_string())
                            })
                            .unwrap_or_default();
                        let title = ep
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Episode")
                            .to_string();
                        let episode_num =
                            ep.get("episode_num").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let ext = ep
                            .get("container_extension")
                            .and_then(|v| v.as_str())
                            .unwrap_or_else(|| self.media_ext(None));
                        list.push(SeriesEpisode {
                            id: id.clone(),
                            title,
                            stream_url: self.series_stream_url(&id, ext),
                            episode_num,
                            plot: None,
                            rating: None,
                            runtime: None,
                            airdate: None,
                            still: None,
                        });
                    }
                }
                seasons.push(SeriesSeason {
                    season_number,
                    episodes: list,
                });
            }
        }
        seasons.sort_by_key(|s| s.season_number);

        info!(
            %series_id,
            seasons = seasons.len(),
            episodes = seasons.iter().map(|s| s.episodes.len()).sum::<usize>(),
            "series_info loaded"
        );
        Ok(SeriesItem {
            id: series_id.to_string(),
            name,
            cover,
            banner,
            plot,
            year,
            rating,
            genre,
            imdb_id,
            actors: info
                .get("cast")
                .or_else(|| info.get("actors"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            director: info
                .get("director")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            writer: None,
            runtime: None,
            rated: None,
            awards: None,
            language: None,
            country: info
                .get("country")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            seasons,
            source_id: Some(self.source_id),
            category_id: None,
        })
    }

    /// Full VOD metadata (`get_vod_info`) — plot, cast, director like series_info.
    pub async fn vod_info(&self, vod_id: &str) -> Result<VodItem> {
        debug!(%vod_id, "vod_info");
        let value = self
            .get_json_action("get_vod_info", &[("vod_id", vod_id)])
            .await?;

        let info = value.get("info").cloned().unwrap_or(Value::Null);
        let movie = value.get("movie_data").cloned().unwrap_or(Value::Null);

        let name = info
            .get("name")
            .or_else(|| movie.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or(vod_id)
            .to_string();
        let ext = movie
            .get("container_extension")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| self.media_ext(None));
        let stream_url = self.vod_stream_url(vod_id, ext);

        let poster = info
            .get("movie_image")
            .or_else(|| info.get("cover_big"))
            .or_else(|| info.get("cover"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let plot = info
            .get("plot")
            .or_else(|| info.get("description"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let year = info
            .get("releasedate")
            .or_else(|| info.get("releaseDate"))
            .or_else(|| info.get("release_date"))
            .or_else(|| info.get("year"))
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .map(|s| {
                // "2024-05-02" → "2024"
                s.chars().take(4).collect::<String>()
            })
            .filter(|s| s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()));

        let rating = info
            .get("rating")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_f64().map(|n| format!("{n:.1}")))
            });

        let genre = info.get("genre").and_then(|v| match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Array(a) => {
                let p: Vec<_> = a
                    .iter()
                    .filter_map(|x| x.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if p.is_empty() {
                    None
                } else {
                    Some(p.join(", "))
                }
            }
            _ => None,
        });

        let actors = info
            .get("actors")
            .or_else(|| info.get("cast"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let director = info
            .get("director")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let runtime = info
            .get("duration")
            .or_else(|| info.get("episode_run_time"))
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_u64().map(|n| format!("{n} min")))
            });

        let imdb_id = info
            .get("imdb_id")
            .or_else(|| info.get("imdbid"))
            .or_else(|| info.get("IMDB_ID"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "0")
            .map(str::to_string);

        let country = info
            .get("country")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let rated = info
            .get("mpaa_rating")
            .or_else(|| info.get("age"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        info!(
            %vod_id,
            has_plot = plot.is_some(),
            has_actors = actors.is_some(),
            "vod_info loaded"
        );

        Ok(VodItem {
            id: vod_id.to_string(),
            name,
            stream_url,
            poster,
            plot,
            year,
            rating,
            genre,
            imdb_id,
            actors,
            director,
            writer: None,
            runtime,
            rated,
            awards: None,
            language: None,
            country,
            category_id: movie
                .get("category_id")
                .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string()))),
            source_id: Some(self.source_id),
        })
    }
}

fn is_adult_category(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.contains("XXX") || n.contains("ADULT") || n.contains("FOR ADULTS") || n.contains("+18")
}

async fn parallel_map_categories(
    client: &XtreamClient,
    category_ids: &[String],
    parallel: usize,
    vod: bool,
) -> (Vec<VodItem>, Vec<SeriesItem>) {
    let _prof = Stopwatch::start("xtream_parallel_categories");
    let mut vod_out = Vec::new();
    let mut series_out = Vec::new();
    if category_ids.is_empty() {
        return (vod_out, series_out);
    }
    info!(
        cats = category_ids.len(),
        parallel,
        vod,
        "parallel category prefetch start"
    );
    let mut set = tokio::task::JoinSet::new();
    let mut idx = 0usize;
    let spawn = |set: &mut tokio::task::JoinSet<_>, cid: String, c: XtreamClient, vod: bool| {
        set.spawn(async move {
            if vod {
                match c.load_vod_category(&cid).await {
                    Ok(items) => (cid, Some(items), None, None),
                    Err(e) => (cid, None, None, Some(e.to_string())),
                }
            } else {
                match c.load_series_category(&cid).await {
                    Ok(items) => (cid, None, Some(items), None),
                    Err(e) => (cid, None, None, Some(e.to_string())),
                }
            }
        });
    };
    while idx < category_ids.len() && set.len() < parallel {
        spawn(&mut set, category_ids[idx].clone(), client.clone(), vod);
        idx += 1;
    }
    while let Some(joined) = set.join_next().await {
        if idx < category_ids.len() {
            spawn(&mut set, category_ids[idx].clone(), client.clone(), vod);
            idx += 1;
        }
        match joined {
            Ok((cid, vod_items, series_items, err)) => {
                if let Some(mut items) = vod_items {
                    vod_out.append(&mut items);
                }
                if let Some(mut items) = series_items {
                    series_out.append(&mut items);
                }
                if let Some(e) = err {
                    warn!(category = %cid, error = %e, vod, "category prefetch failed");
                }
            }
            Err(e) => warn!(error = %e, "category join failed"),
        }
    }
    info!(
        vod = vod_out.len(),
        series = series_out.len(),
        "parallel category prefetch done"
    );
    (vod_out, series_out)
}

#[derive(Debug, Deserialize)]
struct XcCategory {
    #[serde(deserialize_with = "de_id")]
    category_id: String,
    category_name: String,
}

#[derive(Debug, Deserialize)]
struct XcLiveStream {
    #[serde(deserialize_with = "de_id")]
    stream_id: String,
    name: String,
    #[serde(default)]
    stream_icon: Option<String>,
    #[serde(default, deserialize_with = "de_opt_id")]
    category_id: Option<String>,
    #[serde(default)]
    epg_channel_id: Option<String>,
    #[serde(default)]
    container_extension: Option<String>,
    #[serde(default)]
    direct_source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XcVodStream {
    #[serde(deserialize_with = "de_id")]
    stream_id: String,
    name: String,
    #[serde(default)]
    stream_icon: Option<String>,
    #[serde(default, deserialize_with = "de_opt_id")]
    category_id: Option<String>,
    #[serde(default)]
    container_extension: Option<String>,
    #[serde(default)]
    direct_source: Option<String>,
    #[serde(default)]
    plot: Option<String>,
    #[serde(default)]
    year: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    rating: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    genre: Option<String>,
    #[serde(default)]
    imdb_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XcSeries {
    #[serde(deserialize_with = "de_id")]
    series_id: String,
    name: String,
    #[serde(default)]
    cover: Option<String>,
    #[serde(default)]
    cover_big: Option<String>,
    #[serde(default)]
    plot: Option<String>,
    #[serde(default)]
    year: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    rating: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    genre: Option<String>,
    #[serde(default)]
    imdb_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_id")]
    category_id: Option<String>,
}

fn de_id<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        other => Ok(other.to_string()),
    }
}

fn de_opt_id<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?;
    Ok(match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => Some(s),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(other) => Some(other.to_string()),
    })
}

/// Genre may arrive as string or array of strings.
fn de_opt_strish<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?;
    Ok(match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Some(Value::Array(arr)) => {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
        Some(other) => {
            let s = other.to_string();
            if s.is_empty() || s == "null" {
                None
            } else {
                Some(s.trim_matches('"').to_string())
            }
        }
    })
}
