//! Xtream short EPG — same path as IPTV Smarters (`get_short_epg` / `get_simple_data_table`).
//! Titles/descriptions arrive base64-encoded on most panels.

use std::collections::HashSet;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use fluxplay_core::models::EpgProgramme;
use serde_json::Value;
use tracing::warn;

use crate::xtream::{XtreamClient, SMARTERS_UA};
use crate::{http_client, Result};

impl XtreamClient {
    /// Now/next style listings for one live stream (Smarters default).
    pub async fn get_short_epg(&self, stream_id: &str, limit: u32) -> Result<Vec<EpgProgramme>> {
        let mut u = self.api_url(Some("get_short_epg"))?;
        u.query_pairs_mut()
            .append_pair("stream_id", stream_id)
            .append_pair("limit", &limit.to_string());
        let client = http_client()?;
        let value: Value = client
            .get(u)
            .header(reqwest::header::USER_AGENT, SMARTERS_UA)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_epg_listings(&value, stream_id))
    }

    /// Full day (or multi-day) table for one stream — heavier; use sparingly.
    pub async fn get_simple_data_table(&self, stream_id: &str) -> Result<Vec<EpgProgramme>> {
        let mut u = self.api_url(Some("get_simple_data_table"))?;
        u.query_pairs_mut().append_pair("stream_id", stream_id);
        let client = http_client()?;
        let value: Value = client
            .get(u)
            .header(reqwest::header::USER_AGENT, SMARTERS_UA)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_epg_listings(&value, stream_id))
    }

    /// Concurrent short-EPG fetch for many streams (capped parallelism).
    pub async fn fetch_short_epg_batch(
        &self,
        stream_ids: &[String],
        limit_per: u32,
        max_parallel: usize,
    ) -> Vec<EpgProgramme> {
        let mut out = Vec::new();
        let mut seen_keys = HashSet::new();
        let parallel = max_parallel.max(1);

        for chunk in stream_ids.chunks(parallel) {
            let mut handles = Vec::with_capacity(chunk.len());
            for sid in chunk {
                let sid = sid.clone();
                let portal = self.portal.clone();
                let user = self.username.clone();
                let pass = self.password.clone();
                handles.push(tokio::spawn(async move {
                    let client = match XtreamClient::from_credentials(
                        uuid::Uuid::nil(),
                        portal.as_str(),
                        user,
                        pass,
                    ) {
                        Ok(c) => c,
                        Err(_) => return Vec::new(),
                    };
                    client
                        .get_short_epg(&sid, limit_per)
                        .await
                        .unwrap_or_default()
                }));
            }
            for h in handles {
                match h.await {
                    Ok(list) => {
                        for p in list {
                            let key = format!("{}|{}|{}", p.channel_id, p.start, p.title);
                            if seen_keys.insert(key) {
                                out.push(p);
                            }
                        }
                    }
                    Err(e) => warn!(error = %e, "short epg task join failed"),
                }
            }
        }
        out
    }
}

fn parse_epg_listings(value: &Value, stream_id: &str) -> Vec<EpgProgramme> {
    let Some(arr) = value
        .get("epg_listings")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array())
    else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let title_raw = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let title = decode_maybe_b64(&title_raw);
        if title.is_empty() {
            continue;
        }
        let description = item
            .get("description")
            .and_then(|v| v.as_str())
            .map(decode_maybe_b64)
            .filter(|s| !s.is_empty());

        let channel_id = item
            .get("channel_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                item.get("epg_id")
                    .and_then(|v| v.as_str().map(str::to_string).or_else(|| {
                        v.as_u64().map(|n| n.to_string())
                    }))
            })
            .unwrap_or_else(|| stream_id.to_string());

        let start = parse_listing_time(item, "start_timestamp", "start")
            .or_else(|| parse_listing_time(item, "start", "start"));
        let stop = parse_listing_time(item, "stop_timestamp", "end")
            .or_else(|| parse_listing_time(item, "end", "end"))
            .or_else(|| parse_listing_time(item, "stop", "stop"));

        let (Some(start), Some(stop)) = (start, stop) else {
            continue;
        };

        // Index under XMLTV id and under stream id so `now_next` always finds it.
        out.push(EpgProgramme {
            channel_id: channel_id.clone(),
            title: title.clone(),
            description: description.clone(),
            start,
            stop,
            category: None,
        });
        if channel_id != stream_id {
            out.push(EpgProgramme {
                channel_id: stream_id.to_string(),
                title,
                description,
                start,
                stop,
                category: None,
            });
        }
    }
    out
}

fn decode_maybe_b64(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    if let Ok(bytes) = B64.decode(t) {
        if let Ok(s) = String::from_utf8(bytes) {
            if !s.is_empty() && s.chars().any(|c| !c.is_control() || c == '\n' || c == '\t') {
                return s;
            }
        }
    }
    t.to_string()
}

fn parse_listing_time(item: &Value, primary: &str, fallback: &str) -> Option<DateTime<Utc>> {
    if let Some(v) = item.get(primary) {
        if let Some(n) = v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)) {
            return Utc.timestamp_opt(n, 0).single();
        }
        if let Some(s) = v.as_str() {
            if let Ok(n) = s.parse::<i64>() {
                return Utc.timestamp_opt(n, 0).single();
            }
            if let Some(dt) = parse_xc_datetime(s) {
                return Some(dt);
            }
        }
    }
    if primary != fallback {
        if let Some(v) = item.get(fallback) {
            if let Some(s) = v.as_str() {
                return parse_xc_datetime(s);
            }
        }
    }
    None
}

fn parse_xc_datetime(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| DateTime::<Utc>::from_naive_utc_and_offset(n, Utc))
        .or_else(|| {
            DateTime::parse_from_rfc3339(raw)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        })
}
