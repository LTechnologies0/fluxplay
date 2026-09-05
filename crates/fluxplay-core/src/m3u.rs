//! M3U / M3U Plus / M3U8 playlist parser (IPTV `#EXTINF` dialect).

use regex::Regex;
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::models::{CatchupInfo, Channel, ContentKind, PlaylistBundle};
use crate::protocol::StreamScheme;
use crate::Stopwatch;

/// Parse an M3U / M3U Plus body into a [`PlaylistBundle`].
pub fn parse_m3u(body: &str, source_id: Option<Uuid>) -> Result<PlaylistBundle> {
    let _prof = Stopwatch::start("parse_m3u");
    let text = body.strip_prefix('\u{feff}').unwrap_or(body);
    debug!(bytes = text.len(), ?source_id, "parse_m3u start");
    if !text.lines().next().map(|l| l.trim_start().starts_with("#EXTM3U")).unwrap_or(false)
        && !text.contains("#EXTINF")
    {
        // Allow bare URL lists used by some IPTV exporters.
        if text.lines().any(|l| looks_like_url(l.trim())) {
            info!("parse_m3u bare-URL fallback");
            let bundle = parse_bare_urls(text, source_id);
            info!(channels = bundle.channels.len(), "parse_m3u bare done");
            return Ok(bundle);
        }
        warn!("parse_m3u missing #EXTM3U / #EXTINF header");
        return Err(Error::InvalidPlaylist(
            "missing #EXTM3U / #EXTINF header".into(),
        ));
    }

    let attr_re = Regex::new(r#"([\w-]+)\s*=\s*"([^"]*)""#).expect("regex");
    let mut channels = Vec::new();
    let mut pending_meta: Option<ExtInf> = None;
    let mut catchup: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXTM3U") {
            continue;
        }
        if line.starts_with("#EXTVLCOPT:") {
            continue;
        }
        if line.starts_with("#EXTGRP:") {
            if let Some(meta) = pending_meta.as_mut() {
                meta.group = Some(line.trim_start_matches("#EXTGRP:").trim().to_string());
            }
            continue;
        }
        if line.starts_with("#EXTINF:") {
            pending_meta = Some(parse_extinf(line, &attr_re));
            continue;
        }
        if line.starts_with('#') {
            // Catchup / timeshift tags used by M3U Plus providers.
            if line.to_ascii_lowercase().contains("catchup") {
                catchup = Some(line.to_string());
            }
            continue;
        }

        let url = line.to_string();
        if !looks_like_url(&url) {
            trace!(%url, "parse_m3u skip non-url line");
            continue;
        }

        let meta = pending_meta.take().unwrap_or_default();
        let scheme = StreamScheme::parse(&url);
        let id = meta
            .tvg_id
            .clone()
            .unwrap_or_else(|| format!("ch-{}", channels.len() + 1));

        let catchup_info = meta.catchup.or_else(|| {
            catchup.take().map(|raw| CatchupInfo {
                mode: "tag".into(),
                source: Some(raw),
                days: meta.catchup_days,
            })
        });

        channels.push(Channel {
            id,
            name: meta.name.unwrap_or_else(|| format!("Channel {}", channels.len() + 1)),
            stream_url: url,
            logo: meta.tvg_logo.clone().or(meta.logo),
            group: meta.group,
            tvg_id: meta.tvg_id.clone(),
            tvg_name: meta.tvg_name,
            tvg_logo: meta.tvg_logo,
            epg_channel_id: meta.tvg_id,
            scheme: Some(scheme),
            source_id,
            kind: ContentKind::Live,
            catchup: catchup_info,
        });
    }

    if channels.is_empty() {
        warn!("parse_m3u produced zero channels");
    } else {
        info!(channels = channels.len(), "parse_m3u done");
    }

    Ok(PlaylistBundle {
        channels,
        ..Default::default()
    })
}

#[derive(Default)]
struct ExtInf {
    name: Option<String>,
    group: Option<String>,
    logo: Option<String>,
    tvg_id: Option<String>,
    tvg_name: Option<String>,
    tvg_logo: Option<String>,
    catchup: Option<CatchupInfo>,
    catchup_days: Option<u32>,
}

fn parse_extinf(line: &str, attr_re: &Regex) -> ExtInf {
    // #EXTINF:-1 tvg-id="..." group-title="...", Channel Name
    let mut meta = ExtInf::default();
    let after = line.trim_start_matches("#EXTINF:");
    let (attrs_part, name_part) = match after.rsplit_once(',') {
        Some((a, n)) => (a, n.trim()),
        None => (after, ""),
    };
    if !name_part.is_empty() {
        meta.name = Some(name_part.to_string());
    }
    let mut catchup_mode: Option<String> = None;
    let mut catchup_source: Option<String> = None;
    for cap in attr_re.captures_iter(attrs_part) {
        let key = cap[1].to_ascii_lowercase();
        let val = cap[2].to_string();
        match key.as_str() {
            "tvg-id" => meta.tvg_id = Some(val),
            "tvg-name" => meta.tvg_name = Some(val),
            "tvg-logo" => meta.tvg_logo = Some(val),
            "group-title" => meta.group = Some(val),
            "logo" => meta.logo = Some(val),
            "catchup" => catchup_mode = Some(val),
            "catchup-source" => catchup_source = Some(val),
            "catchup-days" => meta.catchup_days = val.parse().ok(),
            _ => {}
        }
    }
    if catchup_mode.is_some() || catchup_source.is_some() {
        meta.catchup = Some(CatchupInfo {
            mode: catchup_mode.unwrap_or_else(|| "default".into()),
            source: catchup_source,
            days: meta.catchup_days,
        });
    }
    meta
}

fn looks_like_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("http")
        || lower.ends_with(".m3u8")
        || lower.ends_with(".ts")
}

fn parse_bare_urls(text: &str, source_id: Option<Uuid>) -> PlaylistBundle {
    let mut channels = Vec::new();
    for line in text.lines() {
        let url = line.trim();
        if !looks_like_url(url) {
            continue;
        }
        let n = channels.len() + 1;
        channels.push(Channel {
            id: format!("ch-{n}"),
            name: format!("Stream {n}"),
            stream_url: url.to_string(),
            logo: None,
            group: None,
            tvg_id: None,
            tvg_name: None,
            tvg_logo: None,
            epg_channel_id: None,
            scheme: Some(StreamScheme::parse(url)),
            source_id,
            kind: ContentKind::Live,
            catchup: None,
        });
    }
    PlaylistBundle {
        channels,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_classic_m3u_plus() {
        let body = r#"#EXTM3U
#EXTINF:-1 tvg-id="tf1.fr" tvg-logo="http://logo/tf1.png" group-title="France",TF1
https://cdn.example.com/tf1/index.m3u8
#EXTINF:-1 group-title="Sport",BeIN
http://cdn.example.com/bein.ts
"#;
        let bundle = parse_m3u(body, None).unwrap();
        assert_eq!(bundle.channels.len(), 2);
        assert_eq!(bundle.channels[0].name, "TF1");
        assert_eq!(bundle.channels[0].group.as_deref(), Some("France"));
        assert_eq!(bundle.channels[0].tvg_id.as_deref(), Some("tf1.fr"));
    }
}
