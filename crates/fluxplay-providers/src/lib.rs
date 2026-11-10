//! IPTV access providers: M3U fetch, Xtream Codes, Stalker Portal, XMLTV.

pub mod api_cache;
pub mod health;
pub mod http_client;
pub mod m3u_source;
pub mod servers;
pub mod stalker;
pub mod user_agents;
pub mod xtream;
mod xtream_epg;
#[cfg(test)]
mod prop_xtream;
pub mod xtream_url;

use fluxplay_core::models::{MediaSource, PlaylistBundle, SourceKind};
use fluxplay_core::xmltv;
use fluxplay_core::Stopwatch;
use thiserror::Error;
use tracing::{debug, info, warn};

pub use health::{check_xtream_portal, format_health, PortalHealth};
pub use api_cache::configure_runtime_policy;
pub use http_client::{
    app_http, apply_network_settings, bootstrap_lookup_ip, current_network_settings, emergency_http,
    path_policy_blocked_msg, probe_dns, probe_egress_and_path, probe_egress_ip,
    probe_http_path_policy, probe_network_diag, read_body_capped, required_proxy,
    set_socks_proxy, shared_http, socks_proxy, HttpPathPolicy,
};
pub use m3u_source::load_m3u_source;
pub use stalker::StalkerClient;
pub use xtream::XtreamClient;
pub use xtream_url::parse_xtream_get_php;

/// Load VOD items for several Xtream categories concurrently.
pub async fn load_xtream_vod_categories(
    source: &MediaSource,
    category_ids: &[String],
    parallel: usize,
) -> Vec<fluxplay_core::models::VodItem> {
    let _prof = Stopwatch::start("load_xtream_vod_categories");
    debug!(
        source_id = %source.id,
        cats = category_ids.len(),
        parallel,
        "load_xtream_vod_categories"
    );
    let Some(mut client) = xtream_client_for(source) else {
        warn!(source_id = %source.id, "no Xtream client for VOD categories");
        return Vec::new();
    };
    if let Err(e) = client.ensure_stream_base().await {
        warn!(source_id = %source.id, error = %e, "Xtream auth before category load failed");
        return Vec::new();
    }
    client
        .load_vod_categories_parallel(category_ids, parallel)
        .await
}

/// Load series lists for several Xtream categories concurrently.
pub async fn load_xtream_series_categories(
    source: &MediaSource,
    category_ids: &[String],
    parallel: usize,
) -> Vec<fluxplay_core::models::SeriesItem> {
    let _prof = Stopwatch::start("load_xtream_series_categories");
    debug!(
        source_id = %source.id,
        cats = category_ids.len(),
        parallel,
        "load_xtream_series_categories"
    );
    let Some(mut client) = xtream_client_for(source) else {
        warn!(source_id = %source.id, "no Xtream client for series categories");
        return Vec::new();
    };
    if let Err(e) = client.ensure_stream_base().await {
        warn!(source_id = %source.id, error = %e, "Xtream auth before category load failed");
        return Vec::new();
    }
    client
        .load_series_categories_parallel(category_ids, parallel)
        .await
}

/// Load VOD items for one Xtream category into a partial bundle.
pub async fn load_xtream_vod_category(
    source: &MediaSource,
    category_id: &str,
) -> Result<Vec<fluxplay_core::models::VodItem>> {
    debug!(source_id = %source.id, %category_id, "load_xtream_vod_category");
    let mut client = xtream_client_for(source).ok_or_else(|| {
        ProviderError::Message("source Xtream requise pour charger le VOD".into())
    })?;
    // Without `server_info` the URLs point at the API portal instead of the media CDN.
    client.ensure_stream_base().await?;
    client.load_vod_category(category_id).await
}

/// Load series list for one Xtream category.
pub async fn load_xtream_series_category(
    source: &MediaSource,
    category_id: &str,
) -> Result<Vec<fluxplay_core::models::SeriesItem>> {
    debug!(source_id = %source.id, %category_id, "load_xtream_series_category");
    let mut client = xtream_client_for(source).ok_or_else(|| {
        ProviderError::Message("source Xtream requise pour charger les séries".into())
    })?;
    // Without `server_info` the URLs point at the API portal instead of the media CDN.
    client.ensure_stream_base().await?;
    client.load_series_category(category_id).await
}

/// Expand seasons/episodes for one series.
pub async fn load_xtream_series_info(
    source: &MediaSource,
    series_id: &str,
) -> Result<fluxplay_core::models::SeriesItem> {
    debug!(source_id = %source.id, %series_id, "load_xtream_series_info");
    let mut client = xtream_client_for(source).ok_or_else(|| {
        ProviderError::Message("source Xtream requise pour les détails série".into())
    })?;
    client.ensure_stream_base().await?;
    client.series_info(series_id).await
}

/// Best-effort VOD play URL when the catalog row has an empty `stream_url`.
pub fn guess_xtream_vod_url(source: &MediaSource, vod_id: &str) -> Option<String> {
    let client = xtream_client_for(source)?;
    let url = client.vod_stream_url(vod_id, "mp4");
    (!url.trim().is_empty()).then_some(url)
}

/// Best-effort series episode URL when the fiche row has an empty `stream_url`.
pub fn guess_xtream_series_url(source: &MediaSource, episode_id: &str) -> Option<String> {
    let id = episode_id.trim();
    if id.is_empty() {
        return None;
    }
    let client = xtream_client_for(source)?;
    let url = client.series_stream_url(id, "mp4");
    (!url.trim().is_empty()).then_some(url)
}

/// Full VOD metadata (plot / cast / director) for one movie.
pub async fn load_xtream_vod_info(
    source: &MediaSource,
    vod_id: &str,
) -> Result<fluxplay_core::models::VodItem> {
    debug!(source_id = %source.id, %vod_id, "load_xtream_vod_info");
    let mut client = xtream_client_for(source).ok_or_else(|| {
        ProviderError::Message("source Xtream requise pour les détails VOD".into())
    })?;
    client.ensure_stream_base().await?;
    client.vod_info(vod_id).await
}

/// Film fiche for the title the viewer just opened. Waits out a portal 429.
pub async fn load_xtream_vod_info_now(
    source: &MediaSource,
    vod_id: &str,
) -> Result<fluxplay_core::models::VodItem> {
    debug!(source_id = %source.id, %vod_id, "load_xtream_vod_info_now");
    let mut client = xtream_client_for(source).ok_or_else(|| {
        ProviderError::Message("source Xtream requise pour les détails VOD".into())
    })?;
    client.ensure_stream_base().await?;
    client.vod_info_now(vod_id).await
}

/// Cast, director and runtime already stored with the series list. Empty when
/// that list is not in the local API cache.
pub fn cached_series_credits(
    source: &MediaSource,
) -> Vec<(String, Option<String>, Option<String>, Option<String>)> {
    let Some(client) = xtream_client_for(source) else {
        return Vec::new();
    };
    client
        .cached_series_list()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| {
            let actors = s.actors.filter(|a| !a.trim().is_empty())?;
            Some((s.id, Some(actors), s.director, s.runtime))
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("{}", display_http(.0))]
    Http(#[from] reqwest::Error),
    #[error("{}", display_core(.0))]
    Core(#[from] fluxplay_core::Error),
    #[error("{}", display_auth(.0))]
    Auth(String),
    #[error("{}", display_unsupported(.0))]
    Unsupported(SourceKind),
    #[error("{}", display_message(.0))]
    Message(String),
}

pub type Result<T> = std::result::Result<T, ProviderError>;

fn auth_code(msg: &str) -> fluxplay_core::FpCode {
    let s = msg.to_ascii_lowercase();
    if s.contains("stalker") || s.contains("mac") {
        fluxplay_core::FpCode::CatStalkerAuth
    } else {
        fluxplay_core::FpCode::CatXtreamAuth
    }
}

fn display_http(e: &reqwest::Error) -> String {
    // Include source chain so UI/logs show "connection reset" not only "error sending request".
    let detail = redact_urls(&reqwest_error_blob(e));
    classify_reqwest(e).ensure_coded(format!("http: {detail}"))
}

fn display_core(e: &fluxplay_core::Error) -> String {
    // Core Display already carries `[FP-…]`.
    format!("core: {e}")
}

fn display_auth(msg: &str) -> String {
    auth_code(msg).ensure_coded(format!("auth: {msg}"))
}

fn display_unsupported(kind: &SourceKind) -> String {
    fluxplay_core::FpCode::CatUnsupported
        .ensure_coded(format!("unsupported source kind: {kind:?}"))
}

fn display_message(msg: &str) -> String {
    fluxplay_core::classify_message(msg).ensure_coded(msg)
}

impl fluxplay_core::HasFpCode for ProviderError {
    fn fp_code(&self) -> fluxplay_core::FpCode {
        use fluxplay_core::FpCode;
        match self {
            Self::Http(e) => classify_reqwest(e),
            Self::Core(e) => fluxplay_core::HasFpCode::fp_code(e),
            Self::Auth(m) => auth_code(m),
            Self::Unsupported(_) => FpCode::CatUnsupported,
            Self::Message(m) => fluxplay_core::classify_message(m),
        }
    }
}

impl ProviderError {
    /// Display already includes `[FP-…]` (and redacts HTTP URLs).
    pub fn coded(&self) -> String {
        self.to_string()
    }

    pub fn coded_auth(msg: impl Into<String>) -> Self {
        Self::Auth(msg.into())
    }
}

/// Flatten reqwest + `source()` chain (top-level Display often omits "Connection reset").
fn reqwest_error_blob(e: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut parts = vec![e.to_string()];
    let mut cur = e.source();
    while let Some(s) = cur {
        parts.push(s.to_string());
        cur = s.source();
    }
    parts.join(" | ").to_ascii_lowercase()
}

/// Map reqwest failures to stable net codes (no URL leak).
pub fn classify_reqwest(e: &reqwest::Error) -> fluxplay_core::FpCode {
    use fluxplay_core::FpCode;
    let s = reqwest_error_blob(e);
    if e.is_timeout() || s.contains("timed out") || s.contains("timeout") {
        return FpCode::NetHttpTimeout;
    }
    if e.is_redirect() {
        return FpCode::NetHttpRedirect;
    }
    // Prefer precise TCP codes from the io chain before is_connect()/is_request() buckets.
    if s.contains("connection reset") || s.contains("os { code: 104") {
        return FpCode::NetTcpReset;
    }
    if s.contains("connection refused") || s.contains("os { code: 111") {
        if current_network_settings().wireguard_enabled && socks_proxy().is_none() {
            return FpCode::NetProxyDead;
        }
        if s.contains("proxy") || s.contains("socks") || s.contains("127.0.0.1:9") {
            return FpCode::NetProxyRefuse;
        }
        return FpCode::NetTcpConnect;
    }
    if s.contains("broken pipe") || s.contains("os { code: 32") {
        return FpCode::NetTcpReset;
    }
    if e.is_connect() {
        if s.contains("dns") || s.contains("resolve") || s.contains("name or service") {
            return FpCode::NetDnsServfail;
        }
        if s.contains("certificate") || s.contains("cert") {
            return FpCode::NetHttpsCert;
        }
        if s.contains("tls") || s.contains("ssl") {
            return FpCode::NetHttpsTls;
        }
        if current_network_settings().wireguard_enabled && socks_proxy().is_none() {
            return FpCode::NetProxyDead;
        }
        if s.contains("proxy") || s.contains("socks") || s.contains("127.0.0.1:9") {
            return FpCode::NetProxyRefuse;
        }
        return FpCode::NetTcpConnect;
    }
    if e.is_request() && (s.contains("body") || s.contains("decode")) {
        return FpCode::NetHttpBody;
    }
    if let Some(status) = e.status() {
        if !status.is_success() {
            return FpCode::NetHttpStatus;
        }
    }
    if s.contains("certificate") {
        FpCode::NetHttpsCert
    } else if s.contains("tls") || s.contains("ssl") {
        FpCode::NetHttpsTls
    } else if s.contains("error sending request") {
        // Mid-request drop without a clearer io string — still NET, never CAT via player_api.
        FpCode::NetTcpConnect
    } else {
        fluxplay_core::classify_message(&redact_urls(&s))
    }
}

/// Error text without the request URL (Xtream URLs carry the credentials).
pub fn redact_error(e: &reqwest::Error) -> String {
    // Same chain as Display(Http) so probes don't hide "connection reset by peer".
    classify_reqwest(e).ensure_coded(redact_urls(&reqwest_error_blob(e)))
}

#[cfg(test)]
mod code_tests {
    use super::*;
    use fluxplay_core::HasFpCode;

    #[test]
    fn auth_display_carries_xtream_code() {
        let e = ProviderError::Auth("Xtream auth failed".into());
        let s = e.to_string();
        assert!(s.contains("[FP-CAT-XTREAM-AUTH]"), "{s}");
        assert_eq!(e.fp_code(), fluxplay_core::FpCode::CatXtreamAuth);
    }

    #[test]
    fn stalker_auth_code() {
        let e = ProviderError::Auth("Stalker MAC required".into());
        assert_eq!(e.fp_code(), fluxplay_core::FpCode::CatStalkerAuth);
        assert!(e.to_string().contains("[FP-CAT-STALKER-AUTH]"));
    }

    #[test]
    fn stalker_api_http_message_code() {
        let e = ProviderError::Message("stalker API HTTP 403".into());
        assert_eq!(e.fp_code(), fluxplay_core::FpCode::CatStalkerApi);
        assert!(e.to_string().contains("[FP-CAT-STALKER-API]"));
    }

    #[test]
    fn message_http_status_classified() {
        let e = ProviderError::Message("HTTP 403".into());
        assert_eq!(e.fp_code(), fluxplay_core::FpCode::NetHttpStatus);
    }

    #[test]
    fn xtream_json_parse_carries_json_code() {
        let e = ProviderError::Message("xtream JSON parse: EOF".into());
        let s = e.to_string();
        assert!(s.contains("[FP-CAT-XTREAM-JSON]"), "{s}");
        assert_eq!(e.fp_code(), fluxplay_core::FpCode::CatXtreamJson);
    }

    #[test]
    fn xtream_json_oversize_carries_json_code() {
        let e = ProviderError::Message(
            "xtream JSON oversize: réponse trop volumineuse (max 1 o)".into(),
        );
        assert_eq!(e.fp_code(), fluxplay_core::FpCode::CatXtreamJson);
    }
}

/// Re-export — URL credential scrub for Display / logs / fuzz targets.
pub use fluxplay_core::redact_urls;

pub(crate) fn http_client() -> Result<reqwest::Client> {
    shared_http()
}

/// Clear on-disk Xtream API cache (call on manual full reload).
pub fn clear_xtream_cache() {
    info!("clear_xtream_cache");
    api_cache::clear_all();
}

/// Load a [`MediaSource`] into a unified [`PlaylistBundle`]. With mirrors,
/// the servers are ranked first and tried fastest first until one answers.
pub async fn load_source(source: &MediaSource) -> Result<PlaylistBundle> {
    if !servers::has_mirrors(source) {
        return load_single(source).await;
    }
    let order = servers::rank_if_stale(source).await.endpoint_order();
    let mut first_err = None;
    for ep in order {
        let one = servers::source_for_endpoint(source, &ep);
        match load_single(&one).await {
            Ok(bundle) => return Ok(bundle),
            Err(e) => {
                warn!(source_id = %source.id, error = %e, "server failed — trying the next one");
                first_err.get_or_insert(e);
            }
        }
    }
    Err(first_err.unwrap_or_else(|| ProviderError::Message("aucun serveur configuré".into())))
}

async fn load_single(source: &MediaSource) -> Result<PlaylistBundle> {
    let _prof = Stopwatch::start("load_source");
    info!(source_id = %source.id, kind = ?source.kind, "load_source start");
    match source.kind {
        SourceKind::M3u | SourceKind::M3uPlus | SourceKind::DirectUrl => {
            // IPTV Smarters Pro/Expert: if the link is XC get.php, they parse
            // user/pass and use player_api — they do NOT download the M3U file.
            // Hammering get.php (HTTP 885) also burns the single connection slot.
            if let Some(creds) = parse_xtream_get_php(&source.endpoint) {
                info!(
                    host = %creds.base,
                    "Smarters-mode: get.php → Xtream player_api (skip M3U dump)"
                );
                let mut client = XtreamClient::from_credentials(
                    source.id,
                    &creds.base,
                    creds.username,
                    creds.password,
                )?;
                return client.load_bundle().await;
            }
            load_m3u_source(source).await
        }
        SourceKind::Xtream => {
            let mut client = XtreamClient::from_source(source)?;
            client.load_bundle().await
        }
        SourceKind::Stalker => {
            let mut client = StalkerClient::from_source(source)?;
            client.load_bundle().await
        }
        SourceKind::Xmltv => {
            let client = http_client()?;
            let resp = match client.get(&source.endpoint).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        error = %fluxplay_core::FpCode::CatXmltvFetch.ensure_coded(e.to_string()),
                        "XMLTV fetch failed — empty EPG"
                    );
                    return Ok(PlaylistBundle::default());
                }
            };
            if !resp.status().is_success() {
                warn!(
                    status = %resp.status(),
                    code = %fluxplay_core::FpCode::CatXmltvFetch.as_str(),
                    "XMLTV HTTP error — empty EPG"
                );
                return Ok(PlaylistBundle::default());
            }
            const MAX_XMLTV: usize = 8_000_000;
            let bytes = match read_body_capped(resp, MAX_XMLTV).await {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("xmltv {e}");
                    warn!(
                        error = %fluxplay_core::classify_message(&msg).ensure_coded(&msg),
                        "XMLTV body rejected/failed — empty EPG"
                    );
                    return Ok(PlaylistBundle::default());
                }
            };
            let Some(text) = decode_epg_bytes(&bytes) else {
                return Ok(PlaylistBundle::default());
            };
            match tokio::task::spawn_blocking(move || xmltv::parse_xmltv(&text)).await {
                Ok(Ok(epg)) => {
                    info!(programmes = epg.len(), "XMLTV source parsed");
                    Ok(PlaylistBundle {
                        epg,
                        ..Default::default()
                    })
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "XMLTV parse failed — empty EPG");
                    Ok(PlaylistBundle::default())
                }
                Err(e) => {
                    warn!(error = %e, "XMLTV parse join failed — empty EPG");
                    Ok(PlaylistBundle::default())
                }
            }
        }
    }
}

/// Optionally merge XMLTV from `source.epg_url` into an existing bundle.
/// Never fails hard: oversized / xmltv.php / network errors → leave bundle unchanged.
pub async fn attach_epg(source: &MediaSource, mut bundle: PlaylistBundle) -> Result<PlaylistBundle> {
    let _prof = Stopwatch::start("attach_epg");
    let Some(epg_url) = source.epg_url.as_deref() else {
        debug!(source_id = %source.id, "no epg_url to attach");
        return Ok(bundle);
    };
    if epg_url.trim().is_empty() {
        return Ok(bundle);
    }
    // Full xmltv.php on big panels is often 50–70MB — refuse rather than OOM.
    if epg_url.contains("xmltv.php") {
        warn!(source_id = %source.id, "skipping full xmltv.php attach (use Xtream short EPG)");
        return Ok(bundle);
    }
    debug!(source_id = %source.id, "attach_epg fetch");
    let client = http_client()?;
    let resp = match client.get(epg_url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %redact_error(&e), "EPG attach fetch failed");
            return Ok(bundle);
        }
    };
    const MAX_EPG: usize = 8_000_000;
    let bytes = match read_body_capped(resp, MAX_EPG).await {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("epg {e}");
            warn!(
                error = %fluxplay_core::classify_message(&msg).ensure_coded(&msg),
                "EPG body rejected/failed — skipped"
            );
            return Ok(bundle);
        }
    };
    let Some(text) = decode_epg_bytes(&bytes) else {
        return Ok(bundle);
    };
    if let Ok(epg) = xmltv::parse_xmltv(&text) {
        let n = epg.len();
        merge_epg(&mut bundle.epg, epg);
        info!(programmes = n, total = bundle.epg.len(), "EPG attached");
    } else {
        warn!(
            code = %fluxplay_core::FpCode::CatXmltvParse.as_str(),
            "EPG attach parse failed"
        );
    }
    Ok(bundle)
}

/// Inflate file-style `.gz` EPG (no Content-Encoding) with a 64 MiB expand cap.
/// `Content-Encoding: gzip` is already handled by reqwest before `read_body_capped`.
fn decode_epg_bytes(bytes: &[u8]) -> Option<String> {
    use std::io::Read;
    const MAX_XML: u64 = 64_000_000;
    let decoded: std::borrow::Cow<'_, [u8]> = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        let read = flate2::read::MultiGzDecoder::new(bytes)
            .take(MAX_XML + 1)
            .read_to_end(&mut out);
        match read {
            Ok(n) if n as u64 <= MAX_XML => std::borrow::Cow::Owned(out),
            Ok(_) => {
                let msg = "epg gzip oversize expands past 64 MB — skipped";
                warn!(
                    error = %fluxplay_core::FpCode::CatXmltvOversize.ensure_coded(msg),
                    "EPG gzip oversize"
                );
                return None;
            }
            Err(e) => {
                let msg = format!("epg gzip decode failed: {e}");
                warn!(
                    error = %fluxplay_core::FpCode::CatXmltvParse.ensure_coded(&msg),
                    "EPG gzip decode failed"
                );
                return None;
            }
        }
    } else if bytes.len() as u64 > MAX_XML {
        // Plain XML already capped at 8 MiB on the wire; belt-and-braces for callers.
        let msg = format!("xmltv plain body past {MAX_XML} o — skipped");
        warn!(
            error = %fluxplay_core::FpCode::CatXmltvOversize.ensure_coded(&msg),
            "XMLTV plain oversize"
        );
        return None;
    } else {
        std::borrow::Cow::Borrowed(bytes)
    };
    Some(String::from_utf8_lossy(&decoded).into_owned())
}

/// Deduping merge of EPG programmes.
/// Hard ceiling for in-memory + repeated attach merges (matches xmltv parse cap).
const MAX_MERGED_EPG: usize = 250_000;

pub fn merge_epg(
    into: &mut Vec<fluxplay_core::models::EpgProgramme>,
    fresh: Vec<fluxplay_core::models::EpgProgramme>,
) {
    let before = into.len();
    if into.len() >= MAX_MERGED_EPG {
        warn!(
            before,
            cap = MAX_MERGED_EPG,
            "merge_epg skipped — lifetime programme cap reached"
        );
        return;
    }
    let mut seen: std::collections::HashSet<String> = into
        .iter()
        .map(|p| format!("{}|{}|{}", p.channel_id, p.start, p.title))
        .collect();
    for p in fresh {
        if into.len() >= MAX_MERGED_EPG {
            warn!(
                before,
                after = into.len(),
                cap = MAX_MERGED_EPG,
                "merge_epg truncated at lifetime programme cap"
            );
            break;
        }
        let key = format!("{}|{}|{}", p.channel_id, p.start, p.title);
        if seen.insert(key) {
            into.push(p);
        }
    }
    debug!(
        before,
        after = into.len(),
        added = into.len() - before,
        "merge_epg"
    );
}

/// Load source + soft EPG (XMLTV attach and/or Xtream short EPG prefetch).
pub async fn load_source_with_epg(source: &MediaSource) -> Result<PlaylistBundle> {
    let _prof = Stopwatch::start("load_source_with_epg");
    info!(source_id = %source.id, kind = ?source.kind, "load_source_with_epg start");
    let mut bundle = load_source(source).await?;
    bundle = attach_epg(source, bundle).await?;

    if let Some(client) = xtream_client_for(source) {
        let ids: Vec<String> = bundle
            .channels
            .iter()
            .filter(|c| c.epg_channel_id.is_some() || c.tvg_id.is_some())
            .take(12)
            .map(|c| c.id.clone())
            .collect();
        if !ids.is_empty() {
            let epg = client.fetch_short_epg_batch(&ids, 4, 2).await;
            info!(programmes = epg.len(), streams = ids.len(), "short EPG prefetched");
            merge_epg(&mut bundle.epg, epg);
        }
    }

    info!(
        source_id = %source.id,
        channels = bundle.channels.len(),
        epg = bundle.epg.len(),
        "load_source_with_epg done"
    );
    Ok(bundle)
}

/// Fetch short EPG for arbitrary stream ids (UI group / channel focus).
pub async fn fetch_short_epg(
    source: &MediaSource,
    stream_ids: &[String],
) -> Vec<fluxplay_core::models::EpgProgramme> {
    let _prof = Stopwatch::start("fetch_short_epg");
    let Some(client) = xtream_client_for(source) else {
        debug!(source_id = %source.id, "fetch_short_epg: no Xtream client");
        return Vec::new();
    };
    if stream_ids.is_empty() {
        return Vec::new();
    }
    // Cap batch size — UI may pass many visible channels.
    let capped: Vec<String> = stream_ids.iter().take(16).cloned().collect();
    debug!(
        source_id = %source.id,
        requested = stream_ids.len(),
        capped = capped.len(),
        "fetch_short_epg"
    );
    client.fetch_short_epg_batch(&capped, 4, 1).await
}

fn xtream_client_for(source: &MediaSource) -> Option<XtreamClient> {
    if servers::has_mirrors(source) {
        return servers::xtream_client(source, &servers::best_endpoint(source));
    }
    if let Some(creds) = parse_xtream_get_php(&source.endpoint) {
        return XtreamClient::from_credentials(
            source.id,
            &creds.base,
            creds.username,
            creds.password,
        )
        .ok();
    }
    if source.kind == SourceKind::Xtream {
        return XtreamClient::from_source(source).ok();
    }
    None
}
