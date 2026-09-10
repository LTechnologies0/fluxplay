//! Stalker Middleware (MAG / portal.php) client — handshake + channel list.

use fluxplay_core::models::{Channel, ContentKind, MediaSource, PlaylistBundle, SourceKind};
use fluxplay_core::protocol::StreamScheme;
use fluxplay_core::Stopwatch;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, error, info, trace, warn};
use url::Url;

use crate::{http_client, ProviderError, Result};

#[derive(Debug, Clone)]
pub struct StalkerClient {
    pub portal: Url,
    pub mac: String,
    pub source_id: uuid::Uuid,
    pub token: Option<String>,
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
        // Ensure we point at portal.php if a directory was given.
        let path = portal.path().trim_end_matches('/');
        if !path.ends_with("portal.php") {
            let new_path = if path.is_empty() || path == "/" {
                "/portal.php".into()
            } else {
                format!("{path}/portal.php")
            };
            portal.set_path(&new_path);
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

    async fn request(&self, action: &str, extra: &[(&str, &str)]) -> Result<Value> {
        let client = http_client()?;
        let mut url = self.portal.clone();
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("type", "stb");
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
                warn!(%action, error = %e, "Stalker request network error");
                return Err(e.into());
            }
        };
        let resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(e) => {
                warn!(%action, error = %e, "Stalker request HTTP error");
                return Err(e.into());
            }
        };
        let value: Value = resp.json().await?;
        Ok(value)
    }

    pub async fn handshake(&mut self) -> Result<()> {
        let _prof = Stopwatch::start("stalker_handshake");
        debug!(portal = %self.portal, "Stalker handshake start");
        let value = self.request("handshake", &[("token", "")]).await?;
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

        // Prefer ITV (live) ordered list.
        let value = self
            .request(
                "get_ordered_list",
                &[
                    ("genre", "*"),
                    ("force_ch_link_check", ""),
                    ("fav", "0"),
                    ("sortby", "number"),
                    ("hd", "0"),
                    ("p", "0"),
                ],
            )
            .await?;

        // Some portals nest under /js/data, others /js
        let data = value
            .pointer("/js/data")
            .or_else(|| value.pointer("/js"))
            .cloned()
            .unwrap_or(Value::Null);

        let mut channels = Vec::new();
        let arr = data
            .as_array()
            .cloned()
            .or_else(|| {
                data.get("data")
                    .and_then(|d| d.as_array())
                    .cloned()
            })
            .unwrap_or_default();

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
                .or_else(|| item.get("tv_genre_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string);

            // cmd may be "ffmpeg http://..." — strip player prefix.
            let stream_url = strip_cmd_prefix(&cmd);
            if stream_url.is_empty() {
                continue;
            }

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
                scheme: Some(StreamScheme::parse(&cmd)),
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
    for prefix in ["ffmpeg ", "ffrt ", "rtp ", "rtsp "] {
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
