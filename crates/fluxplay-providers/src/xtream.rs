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

/// Hard cap for `player_api` JSON bodies (CWE-400).
const MAX_API_JSON: usize = 48 * 1024 * 1024;
/// Soft caps after JSON parse — truncate with warn (panels can dump millions of rows).
const MAX_CATEGORIES: usize = 10_000;
const MAX_CATALOG_ITEMS: usize = 100_000;

#[derive(Clone)]
pub struct XtreamClient {
    /// Portal base used for API calls (may include :8080 and path prefix `/c`).
    pub portal: Url,
    /// Base used for media URLs (from server_info when available).
    pub stream_base: Url,
    pub username: String,
    pub password: String,
    pub source_id: uuid::Uuid,
    pub prefer_m3u8: bool,
    /// True after a successful auth + `apply_server_info`.
    server_info_loaded: bool,
}

impl std::fmt::Debug for XtreamClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XtreamClient")
            .field("portal", &self.portal.as_str())
            .field("stream_base", &self.stream_base.as_str())
            .field("username", &"***")
            .field("password", &"***")
            .field("prefer_m3u8", &self.prefer_m3u8)
            .finish_non_exhaustive()
    }
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
        let parsed = Url::parse(&base).map_err(|e| ProviderError::Message(e.to_string()))?;
        let url = crate::xtream_url::portal_base_from_url(&parsed);
        // Never log username/password (portal host only).
        debug!(
            source_id = %source_id,
            portal = %url,
            "XtreamClient from_credentials"
        );
        Ok(Self {
            stream_base: url.clone(),
            portal: url,
            username,
            password,
            source_id,
            prefer_m3u8: true,
            server_info_loaded: false,
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
        let mut u = crate::xtream_url::join_portal_script(&self.portal, "player_api.php");
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
        self.fetch_api_json(action, &[], false).await
    }

    pub(crate) async fn get_json_action(
        &self,
        action: &str,
        extra: &[(&str, &str)],
    ) -> Result<Value> {
        let extras: Vec<&str> = extra.iter().flat_map(|(k, v)| [*k, *v]).collect();
        self.fetch_api_json(Some(action), &extras, false).await
    }

    /// Same as [`Self::get_json_action`], but this call is the page the viewer opened.
    /// It waits out a 429 quiet window instead of failing into an empty fiche.
    pub(crate) async fn get_json_action_priority(
        &self,
        action: &str,
        extra: &[(&str, &str)],
    ) -> Result<Value> {
        let extras: Vec<&str> = extra.iter().flat_map(|(k, v)| [*k, *v]).collect();
        self.fetch_api_json(Some(action), &extras, true).await
    }

    async fn fetch_api_json(
        &self,
        action: Option<&str>,
        extras: &[&str],
        priority: bool,
    ) -> Result<Value> {
        let _prof = Stopwatch::start("xtream_get_json");
        let action_label = action.unwrap_or("auth");
        // Do not log password or full auth URL — action + portal only.
        debug!(
            portal = %self.portal,
            %action_label,
            extras = extras.len(),
            "xtream get_json"
        );
        // The password is part of the key: after a credential change the old answer is not ours.
        let account = format!("{}\0{}", self.username, self.password);
        let key = crate::api_cache::key(self.portal.as_str(), &account, action_label, extras);
        let ttl = crate::api_cache::ttl_for_action(action);
        if let Some(cached) = crate::api_cache::get_fresh(&key, ttl) {
            // An empty series_info must not stick for 24h — reopen would stay blank forever.
            let empty_series = action == Some("get_series_info")
                && !crate::api_cache::series_info_has_episodes(&cached);
            if empty_series {
                crate::api_cache::forget(&key);
                warn!(%action_label, "dropping empty series_info cache");
            } else {
                tracing::info!(
                    target: "fluxplay::net",
                    action = %action_label,
                    portal = %self.portal,
                    "net.cache_hit"
                );
                return Ok(cached);
            }
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
            let t0 = std::time::Instant::now();
            // Redact credentials from logged URL.
            let log_url = {
                let mut u = url.clone();
                let pairs: Vec<(String, String)> = u
                    .query_pairs()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                u.set_query(None);
                {
                    let mut q = u.query_pairs_mut();
                    for (k, v) in pairs {
                        if k.eq_ignore_ascii_case("password")
                            || k.eq_ignore_ascii_case("pass")
                            || k.eq_ignore_ascii_case("token")
                            || k.eq_ignore_ascii_case("username")
                            || k.eq_ignore_ascii_case("user")
                        {
                            q.append_pair(&k, "***");
                        } else {
                            q.append_pair(&k, &v);
                        }
                    }
                }
                u.to_string()
            };
            tracing::info!(
                target: "fluxplay::net",
                method = "GET",
                action = %action_label,
                portal = %self.portal,
                url = %log_url,
                "net.request"
            );
            let resp = client
                .get(url.clone())
                .header(reqwest::header::USER_AGENT, SMARTERS_UA)
                .send()
                .await
                .map_err(|e| ProviderError::Http(e))?;
            let status = resp.status();
            let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
            tracing::info!(
                target: "fluxplay::net",
                method = "GET",
                action = %action_label,
                status = status.as_u16(),
                elapsed_ms = format!("{elapsed_ms:.1}"),
                "net.response"
            );
            if status.as_u16() == 429 {
                warn!(%action_label, "HTTP 429 from portal");
                return Err(ProviderError::Message(
                    "HTTP 429 Too Many Requests — ralentissez / cache".into(),
                ));
            }
            if !status.is_success() {
                warn!(%action_label, status = %status, "xtream API HTTP error");
                return Err(crate::xtream_url::status_error(status.as_u16()));
            }
            // Cap JSON body before serde (panels can be tens of MB).
            let raw = crate::http_client::read_body_capped(resp, MAX_API_JSON)
                .await
                .map_err(map_xtream_body_err)?;
            serde_json::from_slice(&raw).map_err(|e| {
                ProviderError::Message(format!("xtream JSON parse: {e}"))
            })
        };

        // A film or series page is the request the viewer is waiting on. Other
        // calls must not touch the portal during a 429 quiet window: each extra
        // hit restarts the ban and the fiche stays empty.
        let user_waiting = priority || action == Some("get_series_info");
        if !user_waiting && !crate::api_cache::portal_quiet_remaining().is_zero() {
            if let Some(stale) = crate::api_cache::get_stale(&key) {
                let empty_series = action == Some("get_series_info")
                    && !crate::api_cache::series_info_has_episodes(&stale);
                if !empty_series {
                    warn!(%action_label, "portal quiet — stale cache");
                    return Ok(stale);
                }
            }
            return Err(ProviderError::Message(
                "HTTP 429 Too Many Requests — ralentissez / cache".into(),
            ));
        }
        let attempts = if user_waiting { 2 } else { 1 };
        for attempt in 0..attempts {
            // The page the viewer opened tries at once. A full quiet sleep (30s)
            // made the synopsis look stuck. One short retry is enough after a 429.
            if user_waiting && attempt > 0 {
                let wait = crate::api_cache::portal_quiet_remaining()
                    .min(std::time::Duration::from_secs(2));
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
            }
            match crate::api_cache::with_portal_limit(&fetch).await {
                Ok(value) => {
                    if action.is_some() {
                        let skip_empty_series = action == Some("get_series_info")
                            && !crate::api_cache::series_info_has_episodes(&value);
                        if skip_empty_series {
                            warn!(%action_label, "not caching empty series_info");
                        } else {
                            crate::api_cache::put(&key, &value);
                        }
                    } else if check_auth(&value).is_ok() {
                        // The auth answer echoes the password; the disk cache does not need it.
                        let mut stored = value.clone();
                        if let Some(ui) = stored.get_mut("user_info").and_then(|u| u.as_object_mut())
                        {
                            ui.remove("password");
                        }
                        crate::api_cache::put(&key, &stored);
                    }
                    trace!(%action_label, "xtream get_json OK");
                    return Ok(value);
                }
                Err(e) => {
                    let rate_limited = e.to_string().contains("429");
                    if rate_limited {
                        crate::api_cache::note_portal_backoff();
                    }
                    if let Some(stale) = crate::api_cache::get_stale(&key) {
                        let empty_series = action == Some("get_series_info")
                            && !crate::api_cache::series_info_has_episodes(&stale);
                        if !empty_series {
                            warn!(error = %e, %action_label, "serving stale xtream cache");
                            return Ok(stale);
                        }
                        crate::api_cache::forget(&key);
                    }
                    if rate_limited && attempt + 1 < attempts {
                        warn!(%action_label, attempt, "portal 429, series info will retry after quiet");
                        continue;
                    }
                    return Err(self.remap_path_policy_err(e).await);
                }
            }
        }
        Err(ProviderError::Message(
            "HTTP 429 Too Many Requests — ralentissez / cache".into(),
        ))
    }

    /// After TCP RST on player_api, distinguish dead host vs filtered-egress path block.
    async fn remap_path_policy_err(&self, e: ProviderError) -> ProviderError {
        let s = e.to_string().to_ascii_lowercase();
        let looks_rst = s.contains("reset")
            || s.contains("broken pipe")
            || s.contains("tcp-reset")
            || s.contains("os { code: 104");
        if !looks_rst {
            return e;
        }
        let origin = {
            let mut o = format!("{}://{}", self.portal.scheme(), self.portal.host_str().unwrap_or(""));
            if let Some(p) = self.portal.port() {
                o.push(':');
                o.push_str(&p.to_string());
            }
            o
        };
        match crate::http_client::probe_http_path_policy(&origin).await {
            crate::http_client::HttpPathPolicy::RootOnly => {
                ProviderError::Message(crate::http_client::path_policy_blocked_msg(&origin))
            }
            _ => e,
        }
    }

    /// Auth once so `stream_base` / `prefer_m3u8` match the panel CDN (needed for series/VOD URLs).
    pub async fn ensure_stream_base(&mut self) -> Result<()> {
        if self.server_info_loaded {
            return Ok(());
        }
        let auth = self.get_json(None).await?;
        check_auth(&auth)?;
        self.apply_server_info(&auth);
        self.server_info_loaded = true;
        Ok(())
    }

    /// Live auth round-trip (never the API cache: a dead server must look dead).
    /// Applies `server_info`; returns `user_info.max_connections` when known.
    pub async fn probe_auth(&mut self, timeout: std::time::Duration) -> Result<Option<u32>> {
        let client = crate::api_cache::shared_http()?;
        let resp = match client
            .get(self.api_url(None)?)
            .header(reqwest::header::USER_AGENT, SMARTERS_UA)
            .timeout(timeout)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Err(self
                    .remap_path_policy_err(ProviderError::Message(crate::redact_error(&e)))
                    .await);
            }
        };
        if !resp.status().is_success() {
            return Err(ProviderError::Message(format!("HTTP {}", resp.status())));
        }
        // Same JSON budget as `fetch_api_json` — probe must not bypass DoS caps.
        let raw = crate::http_client::read_body_capped(resp, MAX_API_JSON)
            .await
            .map_err(map_xtream_body_err)?;
        let auth: Value = serde_json::from_slice(&raw).map_err(|e| {
            ProviderError::Message(format!("xtream JSON parse: {e}"))
        })?;
        if auth_flag(&auth) != Some(1) {
            return Err(ProviderError::Auth("Xtream auth failed".into()));
        }
        check_auth(&auth)?;
        self.apply_server_info(&auth);
        self.server_info_loaded = true;
        Ok(auth
            .pointer("/user_info/max_connections")
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0))
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
        let port_of = |key: &str| {
            si.get(key)
                .and_then(|v| v.as_str().map(|s| s.trim().to_string()).or_else(|| v.as_u64().map(|n| n.to_string())))
                .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        };
        let port = if proto.eq_ignore_ascii_case("https") {
            port_of("https_port").unwrap_or_else(|| "443".into())
        } else {
            port_of("port").unwrap_or_else(|| "80".into())
        };

        // Keep portal port if server advertises :80 but we reached API on :8080
        // and streams also work on the portal port (common CDN split). Prefer portal
        // when its host matches. Reject arbitrary CDN hosts (trust-panel SSRF class).
        let portal_host = self.portal.host_str().unwrap_or("");
        if portal_host.eq_ignore_ascii_case(host) {
            // Same host: use portal origin (preserves :8080 when needed).
            self.stream_base = self.portal.clone();
        } else if !server_info_host_allowed(portal_host, host) {
            warn!(
                portal = %portal_host,
                advertised = %host,
                "Xtream server_info host rejected (SSRF/private) — keeping portal stream_base"
            );
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

    /// Movies and episodes are files: HLS is a live-only output on most panels.
    fn vod_ext<'a>(&self, container: Option<&'a str>) -> &'a str {
        container.map(str::trim).filter(|c| !c.is_empty()).unwrap_or("mp4")
    }

    /// `{stream_base}/{kind}/{user}/{pass}/{id}.{ext}` with each segment percent-encoded,
    /// so credentials containing `/`, `?`, `#` or spaces still produce a valid path.
    fn media_url(&self, kind: &str, stream_id: &str, ext: &str) -> String {
        let mut u = self.stream_base.clone();
        u.set_query(None);
        u.set_fragment(None);
        let file = format!("{stream_id}.{ext}");
        if let Ok(mut seg) = u.path_segments_mut() {
            seg.pop_if_empty()
                .extend([kind, self.username.as_str(), self.password.as_str(), file.as_str()]);
        }
        u.to_string()
    }

    pub fn live_stream_url(&self, stream_id: &str, ext: &str) -> String {
        self.media_url("live", stream_id, ext)
    }

    pub fn vod_stream_url(&self, stream_id: &str, ext: &str) -> String {
        self.media_url("movie", stream_id, ext)
    }

    pub fn series_stream_url(&self, stream_id: &str, ext: &str) -> String {
        self.media_url("series", stream_id, ext)
    }

    pub async fn load_bundle(&mut self) -> Result<PlaylistBundle> {
        let _prof = Stopwatch::start("xtream_load_bundle");
        info!(
            source_id = %self.source_id,
            portal = %self.portal,
            "Xtream load_bundle start"
        );
        // Auth + server_info first (exactly like Smarters login).
        let auth = self.get_json(None).await?;
        if let Err(e) = check_auth(&auth) {
            error!(portal = %self.portal, error = %e, "Xtream auth failed");
            return Err(e);
        }
        debug!(portal = %self.portal, "Xtream auth OK");
        self.apply_server_info(&auth);
        self.server_info_loaded = true;

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
            for item in take_capped(arr, MAX_CATEGORIES, "live_categories") {
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
            for item in take_capped(arr, MAX_CATEGORIES, "vod_categories") {
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
            for item in take_capped(arr, MAX_CATEGORIES, "series_categories") {
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
            let arr = take_capped(arr, MAX_CATALOG_ITEMS, "live_streams");
            let client = self.clone();
            match tokio::task::spawn_blocking(move || client.map_live_streams(arr)).await {
                Ok(mapped) => channels = mapped,
                Err(e) => warn!(error = %e, "live streams map join failed"),
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

    fn map_live_streams(&self, arr: Vec<Value>) -> Vec<Channel> {
        let mut channels = Vec::with_capacity(arr.len());
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
                http_user_agent: None,
                http_referer: None,
            });
        }
        channels
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
        let arr = take_capped(arr, MAX_CATALOG_ITEMS, "vod_streams");
        let client = self.clone();
        match tokio::task::spawn_blocking(move || client.map_vod_dump(arr)).await {
            Ok(out) => {
                if let Some(n) = out.as_ref().map(|v| v.len()) {
                    info!(n, "VOD full dump loaded");
                }
                out
            }
            Err(e) => {
                warn!(error = %e, "VOD dump map join failed");
                None
            }
        }
    }

    fn map_vod_dump(&self, arr: Vec<Value>) -> Option<Vec<VodItem>> {
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let Ok(s) = serde_json::from_value::<XcVodStream>(item) else {
                continue;
            };
            let ext = self.vod_ext(s.container_extension.as_deref());
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
                year: s.year.or(s.release_date).or(s.release_date_camel),
                rating: s.rating,
                genre: s.genre,
                imdb_id: s.imdb_id.filter(|id| !id.is_empty() && id != "0"),
                tmdb_id: None,
                actors: None,
                director: None,
                writer: None,
                runtime: None,
                rated: None,
                awards: None,
                language: None,
                country: None,
                category_id: s.category_id,
                added: s.added,
                source_id: Some(self.source_id),
            });
        }
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
        let arr = take_capped(arr, MAX_CATALOG_ITEMS, "series");
        let client = self.clone();
        match tokio::task::spawn_blocking(move || client.map_series_dump(arr)).await {
            Ok(out) => {
                if let Some(n) = out.as_ref().map(|v| v.len()) {
                    info!(n, "series full dump loaded");
                }
                out
            }
            Err(e) => {
                warn!(error = %e, "series dump map join failed");
                None
            }
        }
    }

    fn map_series_dump(&self, arr: Vec<Value>) -> Option<Vec<SeriesItem>> {
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
                year: s.year.or(s.release_date).or(s.release_date_camel),
                rating: s.rating,
                genre: s.genre,
                imdb_id: s.imdb_id.filter(|id| !id.is_empty() && id != "0"),
                tmdb_id: None,
                actors: s.cast,
                director: s.director,
                writer: None,
                runtime: s.episode_run_time,
                rated: None,
                awards: None,
                language: None,
                country: None,
                seasons: Vec::new(),
                source_id: Some(self.source_id),
                category_id: s.category_id,
                added: s.added,
            });
        }
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
        let Value::Array(arr) = value else {
            // Object / error JSON must not look like an empty catalog — callers
            // used to wipe the shelf on `Ok([])`.
            return Err(ProviderError::Message(
                "xtream JSON: réponse get_vod_streams invalide (tableau attendu)".into(),
            ));
        };
        let arr = take_capped(arr, MAX_CATALOG_ITEMS, "vod_category");
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let Ok(s) = serde_json::from_value::<XcVodStream>(item) else {
                continue;
            };
            let ext = self.vod_ext(s.container_extension.as_deref());
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
                year: s.year.or(s.release_date).or(s.release_date_camel),
                rating: s.rating,
                genre: s.genre,
                imdb_id: s.imdb_id.filter(|id| !id.is_empty() && id != "0"),
                tmdb_id: None,
                actors: None,
                director: None,
                writer: None,
                runtime: None,
                rated: None,
                awards: None,
                language: None,
                country: None,
                category_id: s.category_id.or_else(|| Some(category_id.to_string())),
                added: s.added,
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
        let Value::Array(arr) = value else {
            return Err(ProviderError::Message(
                "xtream JSON: réponse get_series invalide (tableau attendu)".into(),
            ));
        };
        let arr = take_capped(arr, MAX_CATALOG_ITEMS, "series_category");
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
                year: s.year.or(s.release_date).or(s.release_date_camel),
                rating: s.rating,
                genre: s.genre,
                imdb_id: s.imdb_id.filter(|id| !id.is_empty() && id != "0"),
                tmdb_id: None,
                actors: s.cast,
                director: s.director,
                writer: None,
                runtime: s.episode_run_time,
                rated: None,
                awards: None,
                language: None,
                country: None,
                seasons: Vec::new(),
                source_id: Some(self.source_id),
                category_id: s.category_id.or_else(|| Some(category_id.to_string())),
                added: s.added,
            });
        }
        debug!(%category_id, n = out.len(), "load_series_category done");
        Ok(out)
    }

    pub async fn series_info(&self, series_id: &str) -> Result<SeriesItem> {
        debug!(%series_id, "series_info");
        // Same priority path as `vod_info_now`: the viewer is waiting on this page.
        let value = self
            .get_json_action_priority("get_series_info", &[("series_id", series_id)])
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
        let plot = media_plot(&info);
        // "2025-03-01" → "2025"
        let year = info
            .get("releaseDate")
            .or_else(|| info.get("release_date"))
            .or_else(|| info.get("year"))
            .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())))
            .map(|s| s.trim().chars().take(4).collect::<String>())
            .filter(|s| s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()));
        let rating = info
            .get("rating")
            .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_f64().map(|n| format!("{n:.1}"))));
        let runtime = info.get("episode_run_time").and_then(json_text);
        let rated = content_age(&info);
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

        let mut seasons = self.parse_series_episodes(value.get("episodes"));
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
            tmdb_id: None,
            actors: media_people(&info),
            director: info
                .get("director")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            writer: None,
            runtime,
            rated,
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
            added: None,
        })
    }

    /// Panels disagree on the `episodes` JSON shape:
    /// - object of season → array of episodes (Xtream Codes classic)
    /// - object of season → object of ep_key → episode
    /// - flat array with a `season` / `season_number` field on each row
    fn parse_series_episodes(&self, episodes: Option<&Value>) -> Vec<SeriesSeason> {
        let Some(episodes) = episodes else {
            return Vec::new();
        };
        let mut by_season: std::collections::BTreeMap<u32, Vec<SeriesEpisode>> =
            std::collections::BTreeMap::new();

        let push_ep = |season_number: u32, ep: &Value, map: &mut std::collections::BTreeMap<u32, Vec<SeriesEpisode>>| {
            let Some(parsed) = self.parse_one_episode(ep) else {
                return;
            };
            map.entry(season_number).or_default().push(parsed);
        };

        match episodes {
            Value::Object(seasons) => {
                for (season_key, eps) in seasons {
                    let season_number = season_key.parse().unwrap_or(0);
                    match eps {
                        Value::Array(arr) => {
                            for ep in arr {
                                push_ep(season_number, ep, &mut by_season);
                            }
                        }
                        Value::Object(map) => {
                            for (_k, ep) in map {
                                push_ep(season_number, ep, &mut by_season);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Value::Array(arr) => {
                for ep in arr {
                    let season_number = ep
                        .get("season")
                        .or_else(|| ep.get("season_number"))
                        .or_else(|| ep.get("season_num"))
                        .and_then(|v| {
                            v.as_u64()
                                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                        })
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(1);
                    push_ep(season_number, ep, &mut by_season);
                }
            }
            _ => {}
        }

        by_season
            .into_iter()
            .map(|(season_number, mut list)| {
                list.sort_by_key(|e| e.episode_num);
                SeriesSeason {
                    season_number,
                    episodes: list,
                }
            })
            .collect()
    }

    fn parse_one_episode(&self, ep: &Value) -> Option<SeriesEpisode> {
        let id = ep
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| ep.get("id").and_then(|v| v.as_u64()).map(|n| n.to_string()))
            .or_else(|| {
                ep.get("episode_id")
                    .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())))
            })
            .unwrap_or_default();
        if id.trim().is_empty() {
            return None;
        }
        let title = ep
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Episode")
            .to_string();
        let episode_num = ep
            .get("episode_num")
            .or_else(|| ep.get("episode_number"))
            .or_else(|| ep.get("ep"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0);
        let ext = self.vod_ext(
            ep.get("container_extension")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        );
        let stream_url = ep
            .get("direct_source")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.series_stream_url(&id, ext));
        let (plot, still, rating, runtime, airdate, rated) = episode_details(&self.portal, ep);
        Some(SeriesEpisode {
            id,
            title,
            stream_url,
            episode_num,
            plot,
            rating,
            runtime,
            airdate,
            rated,
            still,
        })
    }

    /// Full VOD metadata (`get_vod_info`) — plot, cast, director like series_info.
    pub async fn vod_info(&self, vod_id: &str) -> Result<VodItem> {
        self.vod_info_with(vod_id, false).await
    }

    /// Same fiche, but this is the title the viewer just opened: wait out a 429.
    pub async fn vod_info_now(&self, vod_id: &str) -> Result<VodItem> {
        self.vod_info_with(vod_id, true).await
    }

    async fn vod_info_with(&self, vod_id: &str, priority: bool) -> Result<VodItem> {
        debug!(%vod_id, priority, "vod_info");
        let value = if priority {
            self.get_json_action_priority("get_vod_info", &[("vod_id", vod_id)])
                .await?
        } else {
            self.get_json_action("get_vod_info", &[("vod_id", vod_id)])
                .await?
        };

        let info = value.get("info").cloned().unwrap_or(Value::Null);
        let movie = value.get("movie_data").cloned().unwrap_or(Value::Null);

        let name = info
            .get("name")
            .or_else(|| movie.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or(vod_id)
            .to_string();
        let ext = self.vod_ext(movie.get("container_extension").and_then(|v| v.as_str()));
        let stream_url = self.vod_stream_url(vod_id, ext);

        let poster = info
            .get("movie_image")
            .or_else(|| info.get("cover_big"))
            .or_else(|| info.get("cover"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let plot = media_plot(&info);

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
            .and_then(|s| portal_year(&s));

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

        let actors = media_people(&info);

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
            tmdb_id: None,
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
            added: None,
        })
    }

    /// Series list already on disk (`plot`, `cast`, `director`). No portal call.
    pub fn cached_series_list(&self) -> Option<Vec<SeriesItem>> {
        let account = format!("{}\0{}", self.username, self.password);
        let key = crate::api_cache::key(self.portal.as_str(), &account, "get_series", &[]);
        let value = crate::api_cache::get_fresh(
            &key,
            crate::api_cache::ttl_for_action(Some("get_series")),
        )?;
        let Value::Array(arr) = value else {
            return None;
        };
        self.map_series_dump(arr)
    }
}

/// `2024-05-02` and `08 May 2026` both carry a year. The first four characters do not.
fn portal_year(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    let mut i = 0;
    while i + 4 <= b.len() {
        if b[i..i + 4].iter().all(|c| c.is_ascii_digit()) && (i == 0 || !b[i - 1].is_ascii_digit())
        {
            let y = &raw[i..i + 4];
            if let Ok(n) = y.parse::<u16>() {
                if (1900..=2100).contains(&n) {
                    return Some(y.to_string());
                }
            }
        }
        i += 1;
    }
    None
}

fn is_adult_category(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.contains("XXX") || n.contains("ADULT") || n.contains("FOR ADULTS") || n.contains("+18")
}

fn map_xtream_body_err(e: ProviderError) -> ProviderError {
    match e {
        ProviderError::Message(m) if m.contains("trop volumineuse") => {
            ProviderError::Message(format!("xtream JSON oversize: {m}"))
        }
        other => other,
    }
}

fn take_capped(mut arr: Vec<Value>, max: usize, label: &str) -> Vec<Value> {
    if arr.len() > max {
        warn!(n = arr.len(), max, %label, "xtream catalog array truncated");
        arr.truncate(max);
    }
    arr
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
    #[serde(default, deserialize_with = "de_name")]
    category_name: String,
}

#[derive(Debug, Deserialize)]
struct XcLiveStream {
    #[serde(deserialize_with = "de_id")]
    stream_id: String,
    #[serde(default, deserialize_with = "de_name")]
    name: String,
    #[serde(default, deserialize_with = "de_opt_strish")]
    stream_icon: Option<String>,
    #[serde(default, deserialize_with = "de_opt_id")]
    category_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    epg_channel_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    container_extension: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    direct_source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XcVodStream {
    #[serde(deserialize_with = "de_id")]
    stream_id: String,
    #[serde(default, deserialize_with = "de_name")]
    name: String,
    #[serde(default, deserialize_with = "de_opt_strish")]
    stream_icon: Option<String>,
    #[serde(default, deserialize_with = "de_opt_id")]
    category_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    container_extension: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    direct_source: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    plot: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    year: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    release_date: Option<String>,
    /// `get_series` spells it `releaseDate`.
    #[serde(default, rename = "releaseDate", deserialize_with = "de_opt_strish")]
    release_date_camel: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    rating: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    genre: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    imdb_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_i64")]
    added: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct XcSeries {
    #[serde(deserialize_with = "de_id")]
    series_id: String,
    #[serde(default, deserialize_with = "de_name")]
    name: String,
    #[serde(default, deserialize_with = "de_opt_strish")]
    cover: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    cover_big: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    plot: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    year: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    release_date: Option<String>,
    /// `get_series` spells it `releaseDate`.
    #[serde(default, rename = "releaseDate", deserialize_with = "de_opt_strish")]
    release_date_camel: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    rating: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    genre: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    imdb_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    cast: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    director: Option<String>,
    #[serde(default, deserialize_with = "de_opt_strish")]
    episode_run_time: Option<String>,
    #[serde(default, deserialize_with = "de_opt_id")]
    category_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_i64")]
    added: Option<i64>,
}

/// `user_info.auth` as a number, whether the panel sends `1` or `"1"`.
fn auth_flag(auth: &Value) -> Option<i64> {
    auth.pointer("/user_info/auth")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

/// Refused credentials (`auth: 0`) and dead accounts (Expired / Banned / Disabled) fail here
/// instead of producing a catalog whose every stream answers 401/403.
fn check_auth(auth: &Value) -> Result<()> {
    if auth_flag(auth) == Some(0) {
        return Err(ProviderError::Auth("Xtream auth failed (identifiants ou panel)".into()));
    }
    let status = auth
        .pointer("/user_info/status")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    for dead in ["expired", "banned", "disabled"] {
        if status.eq_ignore_ascii_case(dead) {
            return Err(ProviderError::Auth(format!("compte Xtream {status}")));
        }
    }
    Ok(())
}

fn de_id<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::String(s) if !s.trim().is_empty() => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        // An item without id cannot be played: drop it rather than build `…/null.ts`.
        other => Err(serde::de::Error::custom(format!("invalid id: {other}"))),
    }
}

/// Names may arrive as `null` or a number on some panels; never drop the item for it.
fn de_name<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(de_opt_strish(deserializer)?.unwrap_or_default())
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
fn de_opt_i64<'de, D>(deserializer: D) -> std::result::Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?;
    Ok(match v {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                t.parse().ok()
            }
        }
        Some(_) => None,
    })
}

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

/// Plot, still, rating, runtime and air date from one `get_series_info` episode.
/// Panels nest them under `info` (`movie_image`, `plot`, `duration`).
/// `mpaa_rating`, `age`, and the usual aliases. Star ratings stay out of this.
fn content_age(info: &Value) -> Option<String> {
    for key in [
        "mpaa_rating",
        "age",
        "rating_mpaa",
        "content_rating",
        "age_rating",
    ] {
        if let Some(s) = info.get(key).and_then(json_text) {
            return Some(s);
        }
    }
    None
}

fn media_plot(info: &Value) -> Option<String> {
    for key in ["plot", "description", "overview", "synopsis"] {
        if let Some(text) = info.get(key).and_then(loose_text) {
            return Some(text);
        }
    }
    None
}

fn media_people(info: &Value) -> Option<String> {
    for key in ["actors", "cast", "casting"] {
        if let Some(text) = info.get(key).and_then(loose_text) {
            return Some(text);
        }
    }
    None
}

/// String, list of names, or `{ "name": "…" }` from a panel fiche.
fn loose_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() || t.eq_ignore_ascii_case("n/a") || t == "0" {
                None
            } else {
                Some(t.to_string())
            }
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(loose_text).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
        Value::Object(obj) => ["name", "actor", "plot", "description"]
            .iter()
            .find_map(|key| obj.get(*key).and_then(loose_text)),
        _ => None,
    }
}

fn json_text(v: &Value) -> Option<String> {
    let s = v
        .as_str()
        .map(str::to_string)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
        .or_else(|| v.as_i64().map(|n| n.to_string()))?;
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("n/a") || s == "0" {
        None
    } else {
        Some(s.to_string())
    }
}

fn episode_details(
    portal: &Url,
    ep: &Value,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let nested = ep.get("info").unwrap_or(&Value::Null);
    let text = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| {
            let v = nested.get(*k).or_else(|| ep.get(*k))?;
            let s = v
                .as_str()
                .map(str::to_string)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
                .or_else(|| v.as_i64().map(|n| n.to_string()))
                .or_else(|| {
                    v.as_f64().map(|n| {
                        if n.fract() == 0.0 {
                            format!("{}", n as i64)
                        } else {
                            format!("{n:.1}")
                        }
                    })
                })?;
            let s = s.trim();
            if s.is_empty() || s.eq_ignore_ascii_case("n/a") || s == "0" {
                None
            } else {
                Some(s.to_string())
            }
        })
    };
    let still = text(&["movie_image", "cover_big", "stream_icon"]).and_then(|raw| {
        if raw.starts_with("http://") || raw.starts_with("https://") {
            return Some(raw);
        }
        if let Some(path) = raw.strip_prefix("//") {
            return Some(format!("https://{path}"));
        }
        raw.strip_prefix('/').and_then(|path| {
            let mut u = portal.clone();
            u.set_path("");
            u.join(path).ok().map(|u| u.to_string())
        })
    });
    let runtime = text(&["duration", "duration_secs", "info_duration"]).map(|raw| {
        if raw.contains(':') {
            return raw;
        }
        match raw.parse::<u32>() {
            Ok(n) if (60..20_000).contains(&n) => format!("{} min", n / 60),
            Ok(n) if (1..60).contains(&n) => format!("{n} min"),
            _ => raw,
        }
    });
    (
        text(&["plot", "description", "overview"]),
        still,
        text(&["rating"]),
        runtime,
        airdate_text(nested, ep),
        text(&["age", "mpaa_rating", "rating_mpaa", "content_rating"]),
    )
}

/// A full day when the panel has one. `2023-01-01` with a year-only sibling stays the year:
/// panels use 1 January as an empty date.
fn airdate_text(nested: &Value, ep: &Value) -> Option<String> {
    let mut full: Option<String> = None;
    let mut year: Option<String> = None;
    for key in ["releasedate", "release_date", "air_date"] {
        let Some(v) = nested.get(key).or_else(|| ep.get(key)) else {
            continue;
        };
        let raw = json_text(v).or_else(|| v.get("date").and_then(json_text));
        let Some(raw) = raw else { continue };
        let day = raw.split(['T', ' ']).next().unwrap_or(raw.as_str());
        if day.len() >= 10 && day.as_bytes().get(4) == Some(&b'-') {
            if day.ends_with("-01-01") {
                year = year.or_else(|| Some(day[..4].to_string()));
            } else if full.is_none() {
                full = Some(day.to_string());
            }
        } else if year.is_none() && raw.chars().take(4).all(|c| c.is_ascii_digit()) && raw.len() >= 4 {
            year = Some(raw.chars().take(4).collect());
        }
    }
    full.or(year)
}

/// Whether `server_info.url` may replace the portal media base.
///
/// Panels routinely advertise a separate CDN hostname (Smarters model) — that
/// stays allowed. Block localhost / private / link-local / cloud-metadata IPs
/// so a hostile portal cannot steer media GETs at the LAN or IMDS.
fn server_info_host_allowed(portal_host: &str, candidate: &str) -> bool {
    let c = candidate.trim().trim_matches(|ch| ch == '[' || ch == ']');
    if c.is_empty() {
        return false;
    }
    if portal_host.eq_ignore_ascii_case(c) {
        return true;
    }
    let cl = c.to_ascii_lowercase();
    if cl == "localhost" || cl.ends_with(".localhost") || cl.ends_with(".local") || cl.ends_with(".internal")
    {
        return false;
    }
    if c.parse::<std::net::IpAddr>().is_ok() {
        // Literal IP retarget only when it is the portal itself (self-hosted).
        // Any other IP (LAN / IMDS / CGNAT) is refused — use a hostname CDN instead.
        return false;
    }
    // Public hostname CDN retarget (Smarters).
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn client() -> XtreamClient {
        XtreamClient::from_credentials(uuid::Uuid::nil(), "http://panel.example:8080", "u".into(), "p".into())
            .unwrap()
    }

    #[test]
    fn odd_field_types_keep_the_item() {
        let vod: XcVodStream = serde_json::from_value(json!({
            "stream_id": 7, "name": null, "rating": 7.5, "year": 2021,
            "stream_icon": "", "category_id": 3, "container_extension": ""
        }))
        .unwrap();
        assert_eq!(vod.name, "");
        assert_eq!(vod.rating.as_deref(), Some("7.5"));
        assert_eq!(vod.year.as_deref(), Some("2021"));
        assert_eq!(vod.stream_icon, None);
        assert_eq!(client().vod_ext(vod.container_extension.as_deref()), "mp4");

        let series: XcSeries = serde_json::from_value(json!({
            "series_id": "9", "name": 1917, "release_date": "2019", "releaseDate": "2019-12-25"
        }))
        .unwrap();
        assert_eq!(series.name, "1917");
        assert_eq!(series.release_date_camel.as_deref(), Some("2019-12-25"));

        assert!(serde_json::from_value::<XcVodStream>(json!({ "stream_id": null })).is_err());
    }

    #[test]
    fn credentials_are_encoded_in_stream_urls() {
        let mut c = client();
        c.password = "a/b?c#d e".into();
        let url = c.vod_stream_url("42", "mkv");
        assert!(url.ends_with("/movie/u/a%2Fb%3Fc%23d%20e/42.mkv"), "{url}");
        assert!(url.contains(&format!("/u/{}/", crate::xtream_url::path_segment(&c.password))));
        c.password = "p".into();
        assert!(c.live_stream_url("1", "ts").ends_with(":8080/live/u/p/1.ts"));
    }

    #[test]
    fn dead_accounts_are_refused() {
        assert!(check_auth(&json!({ "user_info": { "auth": 1, "status": "Active" } })).is_ok());
        assert!(check_auth(&json!({ "user_info": { "auth": "0" } })).is_err());
        assert!(check_auth(&json!({ "user_info": { "auth": 1, "status": "Expired" } })).is_err());
        assert!(check_auth(&json!({ "user_info": { "auth": 1, "status": "Banned" } })).is_err());
    }

    #[test]
    fn https_server_info_uses_the_https_port() {
        let mut c = client();
        c.apply_server_info(&json!({ "server_info": {
            "url": "cdn.example", "port": "80", "https_port": "8443", "server_protocol": "https"
        }}));
        assert_eq!(c.stream_base.as_str(), "https://cdn.example:8443/");
    }

    #[test]
    fn server_info_rejects_private_and_metadata_hosts() {
        assert!(server_info_host_allowed("panel.example", "cdn.example"));
        assert!(server_info_host_allowed("panel.example", "panel.example"));
        assert!(!server_info_host_allowed("panel.example", "127.0.0.1"));
        assert!(!server_info_host_allowed("panel.example", "10.0.0.5"));
        assert!(!server_info_host_allowed("panel.example", "192.168.1.1"));
        assert!(!server_info_host_allowed("panel.example", "169.254.169.254"));
        assert!(!server_info_host_allowed("panel.example", "localhost"));
        assert!(!server_info_host_allowed("panel.example", "foo.local"));
        // Same literal IP is fine (self-hosted).
        assert!(server_info_host_allowed("203.0.113.9", "203.0.113.9"));
    }

    #[test]
    fn server_info_keeps_portal_when_host_forbidden() {
        let mut c = client();
        let before = c.stream_base.clone();
        c.apply_server_info(&json!({ "server_info": {
            "url": "169.254.169.254", "port": "80", "server_protocol": "http"
        }}));
        assert_eq!(c.stream_base, before);
    }

    #[test]
    fn episode_info_keeps_plot_and_still() {
        let portal = Url::parse("http://panel.example:8080/").unwrap();
        let (plot, still, rating, runtime, air, rated) = episode_details(
            &portal,
            &json!({
                "title": "Pilot",
                "info": {
                    "plot": "Le premier jour.",
                    "movie_image": "http://cdn.example/ep1.jpg",
                    "rating": 8.2,
                    "duration_secs": 2580,
                    "releasedate": "2020-01-02",
                    "age": "12"
                }
            }),
        );
        assert_eq!(plot.as_deref(), Some("Le premier jour."));
        assert_eq!(still.as_deref(), Some("http://cdn.example/ep1.jpg"));
        assert_eq!(rating.as_deref(), Some("8.2"));
        assert_eq!(runtime.as_deref(), Some("43 min"));
        assert_eq!(air.as_deref(), Some("2020-01-02"));
        assert_eq!(rated.as_deref(), Some("12"));
        let (_, still, _, _, _, _) = episode_details(
            &portal,
            &json!({ "info": { "movie_image": "/images/ep.jpg" } }),
        );
        assert_eq!(still.as_deref(), Some("http://panel.example:8080/images/ep.jpg"));
    }

    #[test]
    fn series_episodes_accept_object_map_and_flat_array() {
        let c = client();
        let classic = c.parse_series_episodes(Some(&json!({
            "1": [{ "id": "10", "episode_num": 1, "title": "Pilot", "container_extension": "mkv" }],
            "2": { "1": { "id": 20, "episode_num": "1", "title": "S2E1" } }
        })));
        assert_eq!(classic.len(), 2);
        assert_eq!(classic[0].episodes.len(), 1);
        assert_eq!(classic[1].episodes.len(), 1);
        assert!(classic[0].episodes[0].stream_url.contains("/series/"));

        let flat = c.parse_series_episodes(Some(&json!([
            { "id": "30", "season": 1, "episode_num": 2, "title": "Two" },
            { "id": "31", "season_number": "1", "episode_num": 1, "title": "One" }
        ])));
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].episodes.len(), 2);
        assert_eq!(flat[0].episodes[0].title, "One");
        assert_eq!(flat[0].episodes[1].title, "Two");

        assert!(!crate::api_cache::series_info_has_episodes(&json!({ "info": {} })));
        assert!(crate::api_cache::series_info_has_episodes(&json!({
            "episodes": { "1": [{ "id": "1" }] }
        })));
    }
}
