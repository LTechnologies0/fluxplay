use std::path::Path;

use fluxplay_core::codes::{FpCode, HasFpCode};
use fluxplay_core::m3u;
use fluxplay_core::models::{MediaSource, PlaylistBundle};
use fluxplay_core::Stopwatch;
use tracing::{debug, error, info, warn};

use crate::user_agents::agents_for;
use crate::{http_client, xtream_url, ProviderError, Result};

/// Hard budget for HTTP / local / inline M3U bodies (CWE-400).
const MAX_M3U_BYTES: usize = 32 * 1024 * 1024;

pub async fn load_m3u_source(source: &MediaSource) -> Result<PlaylistBundle> {
    let _prof = Stopwatch::start("load_m3u_source");
    let endpoint = source.endpoint.trim();
    debug!(source_id = %source.id, "load_m3u_source start");
    let body = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        fetch_playlist_body(endpoint, source).await?
    } else if Path::new(endpoint).exists() {
        debug!("reading playlist from local path");
        read_local_playlist(endpoint).await?
    } else {
        debug!("treating endpoint as inline playlist body");
        enforce_m3u_body_budget(endpoint.as_bytes().len())?;
        endpoint.to_string()
    };

    if body.trim().is_empty() {
        error!(source_id = %source.id, "playlist empty");
        return Err(m3u_empty_err(
            "playlist vide (serveur a renvoyé un corps vide)",
        ));
    }

    // The XMLTV guide is attached once, by `load_source_with_epg`.
    let bundle = m3u::parse_m3u(&body, Some(source.id))?;
    info!(
        source_id = %source.id,
        channels = bundle.channels.len(),
        "M3U parsed"
    );
    Ok(bundle)
}

async fn read_local_playlist(path: &str) -> Result<String> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| ProviderError::Message(format!("read playlist: {e}")))?;
    if meta.len() as usize > MAX_M3U_BYTES {
        return Err(m3u_oversize_err(format!(
            "M3U playlist oversize (local file {} o, max {MAX_M3U_BYTES})",
            meta.len()
        )));
    }
    let body = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| ProviderError::Message(format!("read playlist: {e}")))?;
    // TOCTOU: size can grow between metadata and read.
    enforce_m3u_body_budget(body.len())?;
    Ok(body)
}

fn enforce_m3u_body_budget(len: usize) -> Result<()> {
    if len > MAX_M3U_BYTES {
        Err(m3u_oversize_err(format!(
            "M3U playlist oversize ({len} o, max {MAX_M3U_BYTES})"
        )))
    } else {
        Ok(())
    }
}

fn m3u_oversize_err(msg: impl Into<String>) -> ProviderError {
    ProviderError::Message(FpCode::CatM3uOversize.ensure_coded(msg.into()))
}

fn m3u_empty_err(msg: impl Into<String>) -> ProviderError {
    ProviderError::Message(FpCode::CatM3uEmpty.ensure_coded(msg.into()))
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
                        if code == 429 {
                            // Rate limited: hammering with the next UA only extends the ban.
                            tokio::time::sleep(std::time::Duration::from_secs(1 << i.min(3))).await;
                        }
                        continue;
                    }
                    return Err(ProviderError::Message(last_err));
                }
                // Cap playlist download (hostile/huge get.php → OOM before parse).
                let raw = match http_client::read_body_capped(resp, MAX_M3U_BYTES).await {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(error = %e, %ua, attempt = i + 1, "playlist body rejected");
                        last_err = e.to_string();
                        // Do not rotate UA on oversize — same body under another UA.
                        if last_err.contains("trop volumineuse")
                            || last_err.contains("oversize")
                            || e.fp_code() == FpCode::CatM3uOversize
                        {
                            return Err(m3u_oversize_err(format!(
                                "M3U playlist oversize (max {MAX_M3U_BYTES} o)"
                            )));
                        }
                        continue;
                    }
                };
                let body = String::from_utf8_lossy(&raw).into_owned();
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
                last_err = crate::redact_error(&e);
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
    Err(ProviderError::Message(format!(
        "échec get.php/M3U après {} User-Agents (dernier: HTTP {last_status} — {last_err}). \
         Si c'est un panel Xtream, FluxPlay bascule sur player_api.",
        agents.len()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversize_error_carries_fp_code() {
        let e = m3u_oversize_err("M3U playlist oversize (max 1)");
        let s = e.to_string();
        assert!(s.contains("[FP-CAT-M3U-OVERSIZE]"), "{s}");
        assert_eq!(e.fp_code(), FpCode::CatM3uOversize);
    }

    #[test]
    fn empty_error_carries_fp_code() {
        let e = m3u_empty_err("playlist vide");
        let s = e.to_string();
        assert!(s.contains("[FP-CAT-M3U-EMPTY]"), "{s}");
        assert_eq!(e.fp_code(), FpCode::CatM3uEmpty);
    }

    #[test]
    fn inline_budget_rejects_huge_body() {
        let err = enforce_m3u_body_budget(MAX_M3U_BYTES + 1).unwrap_err();
        assert_eq!(err.fp_code(), FpCode::CatM3uOversize);
    }
}
