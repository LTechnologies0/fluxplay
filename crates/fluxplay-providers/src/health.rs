//! Portal / stream health checks (Xtream panels).

use fluxplay_core::Stopwatch;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::{http_client, ProviderError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalHealth {
    pub portal: String,
    pub ok: bool,
    pub auth: bool,
    pub status: String,
    pub active_cons: u32,
    pub max_connections: u32,
    pub live_hint: Option<u64>,
    pub get_php_ok: bool,
    pub stream_ok: bool,
    pub stream_final_url: Option<String>,
    pub notes: Vec<String>,
}

/// Quick health scan for an Xtream portal (Smarters-compatible).
pub async fn check_xtream_portal(portal: &str, username: &str, password: &str) -> Result<PortalHealth> {
    let _prof = Stopwatch::start("check_xtream_portal");
    let mut notes = Vec::new();
    let client = http_client()?;

    let mut base = portal.trim().trim_end_matches('/').to_string();
    if !base.contains("://") {
        base = format!("http://{base}");
    }
    // Never log password — portal + username only.
    info!(portal = %base, user = %username, "portal health check start");

    let auth_url = format!(
        "{base}/player_api.php?username={username}&password={password}"
    );
    let auth_resp = match client.get(&auth_url).send().await {
        Ok(r) => r,
        Err(e) => {
            error!(portal = %base, error = %e, "portal auth request failed");
            return Err(e.into());
        }
    };
    if !auth_resp.status().is_success() {
        error!(
            portal = %base,
            status = %auth_resp.status(),
            "portal auth HTTP failure"
        );
        return Ok(PortalHealth {
            portal: base,
            ok: false,
            auth: false,
            status: format!("HTTP {}", auth_resp.status()),
            active_cons: 0,
            max_connections: 0,
            live_hint: None,
            get_php_ok: false,
            stream_ok: false,
            stream_final_url: None,
            notes: vec!["player_api injoignable".into()],
        });
    }
    let auth: serde_json::Value = auth_resp.json().await?;
    let ui = auth.get("user_info").cloned().unwrap_or_default();
    let auth_ok = matches!(
        ui.get("auth").and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|i| i as u64))),
        Some(1)
    );
    let status = ui
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let active_cons = ui
        .get("active_cons")
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64()))
        .unwrap_or(0) as u32;
    let max_connections = ui
        .get("max_connections")
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64()))
        .unwrap_or(0) as u32;

    if !auth_ok {
        error!(portal = %base, user = %username, %status, "portal auth rejected");
    } else {
        debug!(
            portal = %base,
            %status,
            active_cons,
            max_connections,
            "portal auth OK"
        );
    }

    if max_connections == 1 {
        notes.push("max_connections=1 — une seule lecture à la fois (comme Smarters)".into());
    }
    if active_cons >= max_connections && max_connections > 0 {
        notes.push("connexion déjà utilisée ailleurs — stoppez l'autre player".into());
    }

    // get.php probe (often intentionally disabled → HTTP 885)
    let get_url = format!(
        "{base}/get.php?username={username}&password={password}&type=m3u_plus&output=ts"
    );
    let get_php_ok = match client.get(&get_url).send().await {
        Ok(r) if r.status().is_success() => {
            let n = r.bytes().await.map(|b| b.len()).unwrap_or(0);
            debug!(bytes = n, "get.php probe succeeded");
            n > 64
        }
        Ok(r) => {
            let code = r.status().as_u16();
            if code == 885 {
                notes.push(
                    "get.php HTTP 885: export M3U désactivé — normal; utiliser l'API Xtream".into(),
                );
                debug!("get.php HTTP 885 (expected for many panels)");
            } else {
                notes.push(format!("get.php HTTP {code}"));
                warn!(code, "get.php probe non-success");
            }
            false
        }
        Err(e) => {
            notes.push(format!("get.php erreur: {e}"));
            warn!(error = %e, "get.php probe network error");
            false
        }
    };

    // Stream probe: skip PPV categories; try a few streams until HLS OK
    let mut stream_ok = false;
    let mut stream_final_url = None;
    let cats_url = format!(
        "{base}/player_api.php?username={username}&password={password}&action=get_live_categories"
    );
    if let Ok(r) = client.get(&cats_url).send().await {
        if let Ok(v) = r.json::<serde_json::Value>().await {
            let cats = v.as_array().cloned().unwrap_or_default();
            let cat_ids: Vec<(String, String)> = cats
                .iter()
                .filter_map(|c| {
                    let name = c
                        .get("category_name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_uppercase();
                    if name.contains("PPV") || name.contains("ADULT") {
                        return None;
                    }
                    let id = c.get("category_id").and_then(|x| {
                        x.as_str()
                            .map(str::to_string)
                            .or_else(|| x.as_u64().map(|n| n.to_string()))
                    })?;
                    Some((id, name))
                })
                .take(4)
                .collect();
            debug!(categories = cat_ids.len(), "probing live streams for health");

            'probe: for (cid, _) in cat_ids {
                let streams_url = format!(
                    "{base}/player_api.php?username={username}&password={password}&action=get_live_streams&category_id={cid}"
                );
                let Ok(sr) = client.get(&streams_url).send().await else {
                    continue;
                };
                let Ok(sv) = sr.json::<serde_json::Value>().await else {
                    continue;
                };
                let ids: Vec<String> = sv
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| {
                        s.get("stream_id").and_then(|x| {
                            x.as_str()
                                .map(str::to_string)
                                .or_else(|| x.as_u64().map(|n| n.to_string()))
                        })
                    })
                    .take(5)
                    .collect();

                for sid in ids {
                    // Stream URL embeds password — never log the full URL.
                    let stream_url = format!("{base}/live/{username}/{password}/{sid}.m3u8");
                    match client.get(&stream_url).send().await {
                        Ok(r) if r.status().is_success() => {
                            let final_u = r.url().to_string();
                            let body = r.text().await.unwrap_or_default();
                            if body.contains("#EXTM3U") {
                                if body.contains("/hls/") {
                                    notes.push(
                                        "HLS OK (CDN /hls/ relatif — players doivent suivre 302)"
                                            .into(),
                                    );
                                }
                                stream_ok = true;
                                // Store for diagnostics but do not log (may contain creds).
                                stream_final_url = Some(final_u);
                                debug!(stream_id = %sid, "stream probe HLS OK");
                                break 'probe;
                            }
                        }
                        Ok(r) if r.status().as_u16() == 402 => {
                            // PPV / locked — try next
                            continue;
                        }
                        Ok(_) | Err(_) => continue,
                    }
                }
            }
            if !stream_ok {
                notes.push("aucun stream HLS testable (PPV/402 ou CDN)".into());
                warn!(portal = %base, "no testable HLS stream found");
            }
        }
    } else {
        notes.push("catégories live injoignables".into());
        warn!(portal = %base, "live categories unreachable during health check");
    }

    // xmltv size warning (often 50MB+)
    let xml_url = format!("{base}/xmltv.php?username={username}&password={password}");
    if let Ok(head) = client.head(&xml_url).send().await {
        if let Some(len) = head.content_length() {
            if len > 8_000_000 {
                notes.push(format!(
                    "xmltv.php ≈ {:.0} Mo — trop gros; préférer get_short_epg / ne pas charger tout",
                    len as f64 / 1_000_000.0
                ));
                debug!(len, "xmltv.php oversized");
            }
        }
    }

    let ok = auth_ok && status.eq_ignore_ascii_case("Active") && stream_ok;
    info!(
        portal = %base,
        auth = auth_ok,
        %status,
        stream_ok,
        get_php_ok,
        ok,
        "portal health check done"
    );
    Ok(PortalHealth {
        portal: base,
        ok,
        auth: auth_ok,
        status,
        active_cons,
        max_connections,
        live_hint: None,
        get_php_ok,
        stream_ok,
        stream_final_url,
        notes,
    })
}

pub fn format_health(h: &PortalHealth) -> String {
    let mut s = format!(
        "{} — auth={} status={} stream={} get.php={} cons={}/{}",
        h.portal,
        if h.auth { "OK" } else { "KO" },
        h.status,
        if h.stream_ok { "OK" } else { "KO" },
        if h.get_php_ok { "OK" } else { "bloqué" },
        h.active_cons,
        h.max_connections
    );
    for n in &h.notes {
        s.push_str(" | ");
        s.push_str(n);
    }
    debug!(portal = %h.portal, ok = h.ok, "format_health");
    s
}

#[allow(dead_code)]
pub fn health_error(msg: impl Into<String>) -> ProviderError {
    let msg = msg.into();
    error!(%msg, "health_error");
    ProviderError::Message(msg)
}
