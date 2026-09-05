use std::path::Path;

use fluxplay_core::m3u;
use fluxplay_core::models::{MediaSource, PlaylistBundle};
use fluxplay_core::Stopwatch;
use tracing::{debug, error, info, warn};

use crate::user_agents::agents_for;
use crate::{http_client, xtream_url, Result};

pub async fn load_m3u_source(source: &MediaSource) -> Result<PlaylistBundle> {
    let _prof = Stopwatch::start("load_m3u_source");
    let endpoint = source.endpoint.trim();
    debug!(source_id = %source.id, "load_m3u_source start");
    let body = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        fetch_playlist_body(endpoint, source).await?
    } else if Path::new(endpoint).exists() {
        debug!("reading playlist from local path");
        tokio::fs::read_to_string(endpoint)
            .await
            .map_err(|e| crate::ProviderError::Message(format!("read playlist: {e}")))?
    } else {
        debug!("treating endpoint as inline playlist body");
        endpoint.to_string()
    };

    if body.trim().is_empty() {
        error!(source_id = %source.id, "playlist empty");
        return Err(crate::ProviderError::Message(
            "playlist vide (serveur a renvoyé un corps vide)".into(),
        ));
    }

    let mut bundle = m3u::parse_m3u(&body, Some(source.id))?;
    info!(
        source_id = %source.id,
        channels = bundle.channels.len(),
        "M3U parsed"
    );
    bundle = crate::attach_epg(source, bundle).await?;
    Ok(bundle)
}

/// Download playlist text, rotating IPTV User-Agents on 885 / 403 / empty body.
pub async fn fetch_playlist_body(endpoint: &str, source: &MediaSource) -> Result<String> {
    let _prof = Stopwatch::start("fetch_playlist_body");
    let client = http_client()?;
    let agents = agents_for(source.user_agent.as_deref());
    let mut last_status = 0u16;
    let mut last_err = String::new();
    debug!(
        source_id = %source.id,
        agents = agents.len(),
        "fetching playlist with UA rotation"
    );

    for (i, ua) in agents.iter().enumerate() {
        let mut req = client.get(endpoint).header(reqwest::header::USER_AGENT, ua);
        if let (Some(u), Some(p)) = (&source.username, &source.password) {
            req = req.basic_auth(u, Some(p));
        }
        if let Some(r) = &source.http_referer {
            req = req.header(reqwest::header::REFERER, r);
        }
        // Mimic players a bit more.
        req = req
            .header(reqwest::header::ACCEPT, "*/*")
            .header("Icy-MetaData", "1");

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                let code = status.as_u16();
                last_status = code;
                if !status.is_success() {
                    warn!(code, %ua, attempt = i + 1, "playlist fetch rejected");
                    last_err = xtream_url::http_status_hint(code)
                        .map(|h| format!("HTTP {code}: {h}"))
                        .unwrap_or_else(|| format!("HTTP {code}"));
                    // Rotate UA on WAF-ish / panel codes.
                    if matches!(code, 401 | 403 | 429 | 885 | 886 | 887) {
                        continue;
                    }
                    return Err(crate::ProviderError::Message(last_err));
                }
                let body = resp.text().await?;
                if body.trim().is_empty() {
                    warn!(%ua, attempt = i + 1, "playlist empty body — trying next UA");
                    last_err = "corps vide".into();
                    continue;
                }
                if !body.contains("#EXTM3U") && !body.contains("#EXTINF") && !body.contains("://")
                {
                    warn!(%ua, attempt = i + 1, "playlist body not M3U-like — trying next UA");
                    last_err = "réponse non-M3U".into();
                    continue;
                }
                info!(%ua, attempt = i + 1, bytes = body.len(), "playlist fetched");
                return Ok(body);
            }
            Err(e) => {
                last_err = e.to_string();
                warn!(error = %last_err, %ua, attempt = i + 1, "playlist network error");
            }
        }
    }

    error!(
        source_id = %source.id,
        agents = agents.len(),
        last_status,
        last_err = %last_err,
        "all User-Agents failed for playlist fetch"
    );
    Err(crate::ProviderError::Message(format!(
        "échec get.php/M3U après {} User-Agents (dernier: HTTP {last_status} — {last_err}). \
         Si c'est un panel Xtream, FluxPlay bascule sur player_api.",
        agents.len()
    )))
}
