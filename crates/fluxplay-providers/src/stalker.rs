//! Stalker Middleware (MAG / portal.php) client — handshake + channel list.

use fluxplay_core::models::{Channel, ContentKind, MediaSource, PlaylistBundle, SourceKind};
use fluxplay_core::protocol::StreamScheme;
use fluxplay_core::Stopwatch;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, error, info, trace, warn};
use url::Url;

use crate::{http_client, ProviderError, Result};

#[derive(Clone)]
pub struct StalkerClient {
    pub portal: Url,
    pub mac: String,
    pub source_id: uuid::Uuid,
    pub token: Option<String>,
}

impl std::fmt::Debug for StalkerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StalkerClient")
            .field("portal", &self.portal.as_str())
            .field("has_token", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

impl StalkerClient {
    pub fn from_source(source: &MediaSource) -> Result<Self> {
        if source.kind != SourceKind::Stalker {
            return Err(ProviderError::Unsupported(source.kind));
        }
        let mac = source
            .mac
            .clone()
            .ok_or_else(|| {
                error!(source_id = %source.id, "Stalker MAC missing");
                ProviderError::Auth("Stalker MAC required".into())
            })?;
        let mac = normalize_mac(&mac)?;

        let mut base = source.endpoint.trim().to_string();
        if !base.contains("://") {
            base = format!("http://{base}");
        }
        let mut portal = Url::parse(&base).map_err(|e| ProviderError::Message(e.to_string()))?;
        // Ensure we point at the API script if a directory was given. `/c/` is the
        // MAG web client; its API sits next to it, at `/portal.php`.
        let path = portal.path().trim_end_matches('/');
        if !path.ends_with("portal.php") && !path.ends_with("load.php") {
            let dir = path.strip_suffix("/c").unwrap_or(path);
            portal.set_path(&format!("{dir}/portal.php"));
        }

        // Never log MAC address.
        info!(source_id = %source.id, portal = %portal, "Stalker client created");
        Ok(Self {
            portal,
            mac,
            source_id: source.id,
            token: None,
        })
    }

    fn cookie_mac(&self) -> String {
        format!("mac={}", self.mac.replace(':', "%3A"))
    }

    /// `kind` is the Stalker module: `stb` (handshake, profile) or `itv` (live TV).
    async fn request(&self, kind: &str, action: &str, extra: &[(&str, &str)]) -> Result<Value> {
        let client = http_client()?;
        let mut url = self.portal.clone();
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("type", kind);
            q.append_pair("action", action);
            q.append_pair("JsHttpRequest", "1-xml");
            for (k, v) in extra {
                q.append_pair(k, v);
            }
        }

        trace!(%action, extras = extra.len(), "Stalker request");
        let mut req = client
            .get(url)
            .header(reqwest::header::COOKIE, self.cookie_mac())
            .header("X-User-Agent", "Model: MAG250; Link: Ethernet")
            .header("User-Agent", "Mozilla/5.0 (QtEmbedded; U; Linux; C) AppleWebKit/533.3");

        if let Some(token) = &self.token {
            // Token value is secret — never log it.
            req = req.header("Authorization", format!("Bearer {token}"));
            trace!(%action, "Stalker request with bearer token");
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(%action, error = %crate::redact_error(&e), "Stalker request network error");
                return Err(e.into());
            }
        };
        let resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(e) => {
                warn!(%action, error = %crate::redact_error(&e), "Stalker request HTTP error");
                return Err(e.into());
            }
        };
        let value: Value = resp.json().await?;
        Ok(value)
    }

    pub async fn handshake(&mut self) -> Result<()> {
        let _prof = Stopwatch::start("stalker_handshake");
        debug!(portal = %self.portal, "Stalker handshake start");
        let value = self.request("stb", "handshake", &[("token", "")]).await?;
        let token = value
            .pointer("/js/token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                error!(portal = %self.portal, "Stalker handshake: no token");
                ProviderError::Auth("Stalker handshake: no token".into())
            })?
            .to_string();
        self.token = Some(token);
        // Never log token contents.
        info!(portal = %self.portal, "Stalker handshake OK");

        // get_profile is required by many portals after handshake.
        let _ = self
            .request(
                "stb",
                "get_profile",
                &[
                    ("hd", "1"),
                    ("ver", "ImageDescription: 0.2.18-r14-pub-250;"),
                    ("num_banks", "2"),
                    ("sn", &device_sn(&self.mac)),
                    ("stb_type", "MAG250"),
                    ("image_version", "218"),
                    ("device_id", &device_id(&self.mac)),
                    ("hw_version", "1.7-BD-00"),
                ],
            )
            .await;
        debug!(portal = %self.portal, "Stalker get_profile done");
        Ok(())
    }

    pub async fn load_bundle(&mut self) -> Result<PlaylistBundle> {
        let _prof = Stopwatch::start("stalker_load_bundle");
        info!(source_id = %self.source_id, portal = %self.portal, "Stalker load_bundle start");
        if self.token.is_none() {
            self.handshake().await?;
        }

        // Genre id → title, for channels that only carry `tv_genre_id`.
        let genres: std::collections::HashMap<String, String> = self
            .request("itv", "get_genres", &[])
            .await
            .ok()
            .and_then(|v| v.pointer("/js").and_then(|j| j.as_array()).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|g| {
                let id = g.get("id").map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })?;
                let title = g.get("title").and_then(|t| t.as_str())?.trim().to_string();
                (!title.is_empty() && id != "*").then_some((id, title))
            })
            .collect();

        // ITV ordered list, page by page (a page is 14–20 channels on most portals).
        const MAX_PAGES: u32 = 500;
        let mut arr = Vec::new();
        for page in 1..=MAX_PAGES {
            let p = page.to_string();
            let value = self
                .request(
                    "itv",
                    "get_ordered_list",
                    &[
                        ("genre", "*"),
                        ("force_ch_link_check", ""),
                        ("fav", "0"),
                        ("sortby", "number"),
                        ("hd", "0"),
                        ("p", &p),
                    ],
                )
                .await;
            let value = match value {
                Ok(v) => v,
                Err(e) if page > 1 => {
                    warn!(page, error = %e, "Stalker page failed — keeping the pages loaded");
                    break;
                }
                Err(e) => return Err(e),
            };
            // Some portals nest under /js/data, others /js
            let data = value
                .pointer("/js/data")
                .or_else(|| value.pointer("/js"))
                .cloned()
                .unwrap_or(Value::Null);
            let items = data
                .as_array()
                .cloned()
                .or_else(|| data.get("data").and_then(|d| d.as_array()).cloned())
                .unwrap_or_default();
            if items.is_empty() {
                break;
            }
            arr.extend(items);
            let total = value
                .pointer("/js/total_items")
                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
            match total {
                Some(t) if arr.len() as u64 >= t => break,
                None => break,
                _ => {}
            }
        }

        let mut channels = Vec::new();
        for item in arr {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Channel")
                .to_string();
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| item.get("id").and_then(|v| v.as_u64()).map(|n| n.to_string()))
                .unwrap_or_else(|| format!("stalker-{}", channels.len()));
            let cmd = item
                .get("cmd")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let logo = item
                .get("logo")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let group = item
                .get("genre_title")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    let gid = item.get("tv_genre_id").map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })?;
                    genres.get(&gid).cloned()
                });

            // cmd may be "ffmpeg http://..." — strip player prefix.
            let stream_url = strip_cmd_prefix(&cmd);
            if stream_url.is_empty() {
                continue;
            }
            let scheme = StreamScheme::parse(&stream_url);

            channels.push(Channel {
                id,
                name,
                stream_url,
                logo,
                group,
                tvg_id: None,
                tvg_name: None,
                tvg_logo: None,
                epg_channel_id: None,
                scheme: Some(scheme),
                source_id: Some(self.source_id),
                kind: ContentKind::Live,
                catchup: None,
            });
        }

        info!(
            source_id = %self.source_id,
            channels = channels.len(),
            "Stalker catalog loaded"
        );
        Ok(PlaylistBundle {
            channels,
            ..Default::default()
        })
    }
}

fn normalize_mac(raw: &str) -> Result<String> {
    let hex: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        error!("invalid Stalker MAC length");
        return Err(ProviderError::Auth(
            "MAC must contain 12 hex digits (AA:BB:CC:DD:EE:FF)".into(),
        ));
    }
    let parts: Vec<_> = hex
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap_or("00").to_ascii_lowercase())
        .collect();
    Ok(parts.join(":"))
}

fn strip_cmd_prefix(cmd: &str) -> String {
    let cmd = cmd.trim();
    for prefix in ["ffmpeg ", "ffrt ", "ffrt2 ", "ffrt3 ", "auto ", "rtp ", "rtsp "] {
        if let Some(rest) = cmd.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    cmd.to_string()
}

fn device_sn(mac: &str) -> String {
    let mut h = Sha256::new();
    h.update(mac.as_bytes());
    hex::encode(h.finalize())[..13].to_string()
}

fn device_id(mac: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("fluxplay-{mac}").as_bytes());
    hex::encode(h.finalize())
}
