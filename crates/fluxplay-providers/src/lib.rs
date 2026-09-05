//! IPTV access providers: M3U fetch, Xtream Codes, Stalker Portal, XMLTV.

pub mod api_cache;
pub mod health;
pub mod m3u_source;
pub mod stalker;
pub mod user_agents;
pub mod xtream;
mod xtream_epg;
pub mod xtream_url;

use fluxplay_core::models::{MediaSource, PlaylistBundle, SourceKind};
use fluxplay_core::xmltv;
use fluxplay_core::Stopwatch;
use thiserror::Error;
use tracing::{debug, info, warn};

pub use health::{check_xtream_portal, format_health, PortalHealth};
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
    let Some(client) = xtream_client_for(source) else {
        warn!(source_id = %source.id, "no Xtream client for VOD categories");
        return Vec::new();
    };
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
    let Some(client) = xtream_client_for(source) else {
        warn!(source_id = %source.id, "no Xtream client for series categories");
        return Vec::new();
    };
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
    let client = xtream_client_for(source).ok_or_else(|| {
        ProviderError::Message("source Xtream requise pour charger le VOD".into())
    })?;
    client.load_vod_category(category_id).await
}

/// Load series list for one Xtream category.
pub async fn load_xtream_series_category(
    source: &MediaSource,
    category_id: &str,
) -> Result<Vec<fluxplay_core::models::SeriesItem>> {
    debug!(source_id = %source.id, %category_id, "load_xtream_series_category");
    let client = xtream_client_for(source).ok_or_else(|| {
        ProviderError::Message("source Xtream requise pour charger les séries".into())
    })?;
    client.load_series_category(category_id).await
}

/// Expand seasons/episodes for one series.
pub async fn load_xtream_series_info(
    source: &MediaSource,
    series_id: &str,
) -> Result<fluxplay_core::models::SeriesItem> {
    debug!(source_id = %source.id, %series_id, "load_xtream_series_info");
    let client = xtream_client_for(source).ok_or_else(|| {
        ProviderError::Message("source Xtream requise pour les détails série".into())
    })?;
    client.series_info(series_id).await
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("core: {0}")]
    Core(#[from] fluxplay_core::Error),
    #[error("auth: {0}")]
    Auth(String),
    #[error("unsupported source kind: {0:?}")]
    Unsupported(SourceKind),
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, ProviderError>;

pub(crate) fn http_client() -> Result<reqwest::Client> {
    api_cache::shared_http()
}

/// Clear on-disk Xtream API cache (call on manual full reload).
pub fn clear_xtream_cache() {
    info!("clear_xtream_cache");
    api_cache::clear_all();
}

/// Load a [`MediaSource`] into a unified [`PlaylistBundle`].
pub async fn load_source(source: &MediaSource) -> Result<PlaylistBundle> {
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
                    warn!(error = %e, "XMLTV fetch failed — empty EPG");
                    return Ok(PlaylistBundle::default());
                }
            };
            if !resp.status().is_success() {
                warn!(status = %resp.status(), "XMLTV HTTP error — empty EPG");
                return Ok(PlaylistBundle::default());
            }
            if let Some(len) = resp.content_length() {
                if len > 8_000_000 {
                    warn!(
                        len,
                        "XMLTV trop volumineux — skip (utiliser get_short_epg Xtream)"
                    );
                    return Ok(PlaylistBundle::default());
                }
            }
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "XMLTV body failed");
                    return Ok(PlaylistBundle::default());
                }
            };
            if bytes.len() > 8_000_000 {
                warn!(len = bytes.len(), "XMLTV body too large — skip");
                return Ok(PlaylistBundle::default());
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            match xmltv::parse_xmltv(&text) {
                Ok(epg) => {
                    info!(programmes = epg.len(), "XMLTV source parsed");
                    Ok(PlaylistBundle {
                        epg,
                        ..Default::default()
                    })
                }
                Err(e) => {
                    warn!(error = %e, "XMLTV parse failed — empty EPG");
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
        warn!(%epg_url, "skipping full xmltv.php attach (use Xtream short EPG)");
        return Ok(bundle);
    }
    debug!(source_id = %source.id, "attach_epg fetch");
    let client = http_client()?;
    let resp = match client.get(epg_url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "EPG attach fetch failed");
            return Ok(bundle);
        }
    };
    if let Some(len) = resp.content_length() {
        if len > 8_000_000 {
            warn!(len, "EPG too large — skipped");
            return Ok(bundle);
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "EPG body failed");
            return Ok(bundle);
        }
    };
    if bytes.len() > 8_000_000 {
        warn!(len = bytes.len(), "EPG too large — skipped");
        return Ok(bundle);
    }
    let text = String::from_utf8_lossy(&bytes);
    if let Ok(epg) = xmltv::parse_xmltv(&text) {
        let n = epg.len();
        merge_epg(&mut bundle.epg, epg);
        info!(programmes = n, total = bundle.epg.len(), "EPG attached");
    } else {
        warn!("EPG attach parse failed");
    }
    Ok(bundle)
}

/// Deduping merge of EPG programmes.
pub fn merge_epg(
    into: &mut Vec<fluxplay_core::models::EpgProgramme>,
    fresh: Vec<fluxplay_core::models::EpgProgramme>,
) {
    let before = into.len();
    let mut seen: std::collections::HashSet<String> = into
        .iter()
        .map(|p| format!("{}|{}|{}", p.channel_id, p.start, p.title))
        .collect();
    for p in fresh {
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
    let capped: Vec<String> = stream_ids.iter().take(12).cloned().collect();
    debug!(
        source_id = %source.id,
        requested = stream_ids.len(),
        capped = capped.len(),
        "fetch_short_epg"
    );
    client.fetch_short_epg_batch(&capped, 4, 2).await
}

fn xtream_client_for(source: &MediaSource) -> Option<XtreamClient> {
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
