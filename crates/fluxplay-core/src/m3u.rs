//! M3U / M3U Plus / M3U8 playlist parser (IPTV `#EXTINF` dialect).

use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::models::{CatchupInfo, Channel, ContentKind, PlaylistBundle};
use crate::protocol::StreamScheme;
use crate::Stopwatch;

/// Skip absurd lines (VLC historically froze on multi-MB lines).
const MAX_LINE_BYTES: usize = 8 * 1024;
/// Soft cap — partial playlist better than OOM.
const MAX_CHANNELS: usize = 100_000;
/// Cap individual labels / attrs after scrub (amplification within an accepted line).
const MAX_FIELD_CHARS: usize = 2 * 1024;
/// Stream URLs may be longer than labels but still bounded by the line budget.
const MAX_URL_CHARS: usize = 8 * 1024;
/// HTTP header values (UA / Referer) — keep injection surface small.
const MAX_HEADER_CHARS: usize = 512;

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

    let mut channels = Vec::new();
    let mut pending_meta: Option<ExtInf> = None;
    let mut catchup: Option<String> = None;
    // HD/SD variants often share one tvg-id; each still needs its own channel id.
    let mut seen_ids: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut truncated = false;

    for raw_line in text.lines() {
        if raw_line.len() > MAX_LINE_BYTES {
            warn!(len = raw_line.len(), "parse_m3u skip overlong line");
            continue;
        }
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXTM3U") {
            continue;
        }
        if line.starts_with("#EXTVLCOPT:") {
            let opt = line.trim_start_matches("#EXTVLCOPT:").trim();
            let meta = pending_meta.get_or_insert_with(ExtInf::default);
            apply_extvlcopt(meta, opt);
            continue;
        }
        if line.starts_with("#EXTGRP:") {
            if let Some(meta) = pending_meta.as_mut() {
                meta.group = Some(scrub_iptv_field(
                    line.trim_start_matches("#EXTGRP:").trim(),
                ));
            }
            continue;
        }
        if line.starts_with("#EXTINF:") {
            // EXTVLCOPT may precede EXTINF; merge so headers are not wiped.
            let mut parsed = parse_extinf(line);
            if let Some(prev) = pending_meta.take() {
                if parsed.http_user_agent.is_none() {
                    parsed.http_user_agent = prev.http_user_agent;
                }
                if parsed.http_referer.is_none() {
                    parsed.http_referer = prev.http_referer;
                }
                if parsed.group.is_none() {
                    parsed.group = prev.group;
                }
            }
            pending_meta = Some(parsed);
            continue;
        }
        if line.starts_with('#') {
            // Catchup / timeshift tags used by M3U Plus providers.
            if line.to_ascii_lowercase().contains("catchup") {
                catchup = Some(line.to_string());
            }
            continue;
        }

        let url = scrub_iptv_url(line);
        if url.is_empty() || !looks_like_url(&url) {
            trace!(%url, "parse_m3u skip non-url line");
            continue;
        }

        if channels.len() >= MAX_CHANNELS {
            truncated = true;
            break;
        }

        let meta = pending_meta.take().unwrap_or_default();
        let scheme = StreamScheme::parse(&url);
        let base_id = meta
            .tvg_id
            .clone()
            .unwrap_or_else(|| format!("ch-{}", channels.len() + 1));
        let dup = seen_ids.entry(base_id.clone()).or_insert(0);
        *dup += 1;
        let id = if *dup == 1 { base_id } else { format!("{base_id}#{dup}") };

        // A catchup tag line belongs to the entry it precedes, never to the next one.
        let tag = catchup.take();
        let catchup_info = meta.catchup.or_else(|| {
            tag.map(|raw| CatchupInfo {
                mode: "tag".into(),
                source: Some(raw),
                days: meta.catchup_days,
            })
        });

        channels.push(Channel {
            id,
            name: meta
                .name
                .unwrap_or_else(|| format!("Channel {}", channels.len() + 1)),
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
            http_user_agent: meta.http_user_agent,
            http_referer: meta.http_referer,
        });
    }
    if truncated {
        warn!(
            channels = channels.len(),
            cap = MAX_CHANNELS,
            "parse_m3u truncated at channel cap"
        );
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
    http_user_agent: Option<String>,
    http_referer: Option<String>,
}

fn apply_extvlcopt(meta: &mut ExtInf, opt: &str) {
    let opt = scrub_iptv_text(opt);
    let (key, val) = match opt.split_once('=') {
        Some((k, v)) => (k.trim(), v.trim().trim_matches('"')),
        None => return,
    };
    if val.is_empty() {
        return;
    }
    let val = scrub_iptv_header(val);
    if val.is_empty() {
        return;
    }
    if key.eq_ignore_ascii_case("http-user-agent") || key.eq_ignore_ascii_case("user-agent") {
        meta.http_user_agent = Some(val);
    } else if key.eq_ignore_ascii_case("http-referrer")
        || key.eq_ignore_ascii_case("http-referer")
        || key.eq_ignore_ascii_case("referrer")
        || key.eq_ignore_ascii_case("referer")
    {
        meta.http_referer = Some(val);
    }
}

/// Strip embedded NUL / other C0 controls that break FFI players and UI labels.
fn scrub_iptv_text(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\t' || !c.is_control())
        .collect()
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        s.chars().take(max).collect()
    }
}

fn scrub_iptv_field(s: &str) -> String {
    truncate_chars(scrub_iptv_text(s), MAX_FIELD_CHARS)
}

fn scrub_iptv_url(s: &str) -> String {
    truncate_chars(scrub_iptv_text(s), MAX_URL_CHARS)
}

fn scrub_iptv_header(s: &str) -> String {
    // Drop anything after embedded CR/LF so values cannot smuggle extra headers.
    let cut = s.split(['\r', '\n']).next().unwrap_or(s);
    truncate_chars(scrub_iptv_text(cut), MAX_HEADER_CHARS)
}

fn parse_extinf(line: &str) -> ExtInf {
    // #EXTINF:-1 tvg-id="..." group-title="...", Channel Name
    let mut meta = ExtInf::default();
    let after = line.trim_start_matches("#EXTINF:");
    // The title starts after the first comma outside quotes: it may itself hold commas
    // ("Movie, The") and attribute values may too (group-title="News, FR").
    let mut in_quotes = false;
    let split = after.char_indices().find_map(|(i, c)| {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => return Some(i),
            _ => {}
        }
        None
    });
    let (attrs_part, name_part) = match split {
        Some(i) => (&after[..i], after[i + 1..].trim()),
        None => (after, ""),
    };
    if !name_part.is_empty() {
        meta.name = Some(scrub_iptv_field(name_part));
    }
    let mut catchup_mode: Option<String> = None;
    let mut catchup_source: Option<String> = None;
    for_each_quoted_attr(attrs_part, |key, val| {
        let raw = val.trim();
        if raw.is_empty() {
            return;
        }
        match key {
            k if k.eq_ignore_ascii_case("tvg-id") => meta.tvg_id = Some(scrub_iptv_field(raw)),
            k if k.eq_ignore_ascii_case("tvg-name") => {
                meta.tvg_name = Some(scrub_iptv_field(raw))
            }
            k if k.eq_ignore_ascii_case("tvg-logo") => {
                meta.tvg_logo = Some(scrub_iptv_url(raw))
            }
            k if k.eq_ignore_ascii_case("group-title") => {
                meta.group = Some(scrub_iptv_field(raw))
            }
            k if k.eq_ignore_ascii_case("logo") => meta.logo = Some(scrub_iptv_url(raw)),
            k if k.eq_ignore_ascii_case("catchup") => {
                catchup_mode = Some(scrub_iptv_field(raw))
            }
            k if k.eq_ignore_ascii_case("catchup-source") => {
                catchup_source = Some(scrub_iptv_url(raw))
            }
            k if k.eq_ignore_ascii_case("catchup-days") || k.eq_ignore_ascii_case("timeshift") => {
                meta.catchup_days = scrub_iptv_field(raw).parse().ok()
            }
            k if k.eq_ignore_ascii_case("user-agent")
                || k.eq_ignore_ascii_case("http-user-agent") =>
            {
                let h = scrub_iptv_header(raw);
                if !h.is_empty() {
                    meta.http_user_agent = Some(h);
                }
            }
            k if k.eq_ignore_ascii_case("referrer")
                || k.eq_ignore_ascii_case("referer")
                || k.eq_ignore_ascii_case("http-referrer")
                || k.eq_ignore_ascii_case("http-referer") =>
            {
                let h = scrub_iptv_header(raw);
                if !h.is_empty() {
                    meta.http_referer = Some(h);
                }
            }
            _ => {}
        }
    });
    if catchup_mode.is_some() || catchup_source.is_some() {
        meta.catchup = Some(CatchupInfo {
            mode: catchup_mode.unwrap_or_else(|| "default".into()),
            source: catchup_source,
            days: meta.catchup_days,
        });
    }
    meta
}

/// Scan `key="value"` pairs without compiling a regex per playlist.
fn for_each_quoted_attr(attrs: &str, mut f: impl FnMut(&str, &str)) {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
        {
            i += 1;
        }
        if i == key_start {
            i += 1;
            continue;
        }
        let key = &attrs[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'"' {
            continue;
        }
        i += 1;
        let val_start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        let val = &attrs[val_start..i];
        if i < bytes.len() {
            i += 1;
        }
        f(key, val);
    }
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
    for raw_line in text.lines() {
        if raw_line.len() > MAX_LINE_BYTES {
            warn!(len = raw_line.len(), "parse_m3u bare skip overlong line");
            continue;
        }
        let url = scrub_iptv_url(raw_line.trim());
        if url.is_empty() || !looks_like_url(&url) {
            continue;
        }
        let n = channels.len() + 1;
        if channels.len() >= MAX_CHANNELS {
            break;
        }
        channels.push(Channel {
            id: format!("ch-{n}"),
            name: format!("Stream {n}"),
            stream_url: url.clone(),
            logo: None,
            group: None,
            tvg_id: None,
            tvg_name: None,
            tvg_logo: None,
            epg_channel_id: None,
            scheme: Some(StreamScheme::parse(&url)),
            source_id,
            kind: ContentKind::Live,
            catchup: None,
            http_user_agent: None,
            http_referer: None,
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
    use proptest::prelude::*;

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

    #[test]
    fn strips_embedded_nul_from_url_and_attrs() {
        let body = "#EXTM3U\n#EXTINF:-1 group-title=\"News\\0FR\",Chan\0nel\nhttp://ex.test/li\\0ve.ts\n"
            .replace("\\0", "\0");
        let ch = parse_m3u(&body, None).unwrap().channels;
        assert_eq!(ch.len(), 1);
        assert!(!ch[0].stream_url.contains('\0'));
        assert!(!ch[0].name.contains('\0'));
        assert_eq!(ch[0].group.as_deref(), Some("NewsFR"));
    }

    #[test]
    fn titles_with_commas_and_shared_or_empty_ids() {
        let body = r#"#EXTM3U
#EXTINF:-1 tvg-id="tf1.fr" group-title="News, FR",Movie, The
http://cdn.example/1.ts
#EXTINF:-1 tvg-id="tf1.fr",TF1 SD
http://cdn.example/2.ts
#EXTINF:-1 tvg-id="",Radio
http://cdn.example/3.ts
"#;
        let ch = parse_m3u(body, None).unwrap().channels;
        assert_eq!(ch[0].name, "Movie, The");
        assert_eq!(ch[0].group.as_deref(), Some("News, FR"));
        assert_eq!(ch[0].id, "tf1.fr");
        assert_eq!(ch[1].id, "tf1.fr#2");
        assert_eq!(ch[1].epg_channel_id.as_deref(), Some("tf1.fr"));
        assert_eq!(ch[2].tvg_id, None);
        assert_eq!(ch[2].id, "ch-3");
    }

    #[test]
    fn extvlcopt_sets_http_user_agent_and_referer() {
        let body = r#"#EXTM3U
#EXTINF:-1 tvg-id="gated",Gated
#EXTVLCOPT:http-user-agent=VLC/3.0
#EXTVLCOPT:http-referrer=https://portal.example/
https://cdn.example/gated.ts
#EXTINF:-1 user-agent="AttrUA" http-referer="https://attr.example/",AttrHdr
http://cdn.example/attr.ts
"#;
        let ch = parse_m3u(body, None).unwrap().channels;
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[0].http_user_agent.as_deref(), Some("VLC/3.0"));
        assert_eq!(
            ch[0].http_referer.as_deref(),
            Some("https://portal.example/")
        );
        assert_eq!(ch[1].http_user_agent.as_deref(), Some("AttrUA"));
        assert_eq!(
            ch[1].http_referer.as_deref(),
            Some("https://attr.example/")
        );
    }

    #[test]
    fn extvlcopt_before_extinf_and_strips_header_controls() {
        // EXTVLCOPT before EXTINF must merge; CR/LF must not survive (header injection).
        let body = format!(
            "#EXTM3U\n#EXTVLCOPT:user-agent=Safe{cr}Tail\n#EXTINF:-1,Ch\nhttp://ex.test/a.ts\n",
            cr = "\r"
        );
        let ch = parse_m3u(&body, None).unwrap().channels;
        assert_eq!(ch.len(), 1);
        let ua = ch[0]
            .http_user_agent
            .as_deref()
            .expect("UA from EXTVLCOPT before EXTINF");
        assert!(!ua.contains('\r') && !ua.contains('\n'));
        assert_eq!(ua, "SafeTail");
    }

    #[test]
    fn skips_overlong_lines_and_truncates_fields() {
        let long_name: String = "N".repeat(MAX_FIELD_CHARS + 64);
        let long_line = format!("#{}", "X".repeat(MAX_LINE_BYTES + 8));
        let body = format!(
            "#EXTM3U\n{long_line}\n#EXTINF:-1 group-title=\"{long_name}\",{long_name}\nhttp://cdn.example/ok.ts\n"
        );
        let ch = parse_m3u(&body, None).unwrap().channels;
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].name.chars().count(), MAX_FIELD_CHARS);
        assert_eq!(ch[0].group.as_ref().unwrap().chars().count(), MAX_FIELD_CHARS);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Fuzz-style: arbitrary bytes must not panic the M3U parser.
        #[test]
        fn parse_m3u_never_panics(body in "\\PC{0,2000}") {
            let _ = parse_m3u(&body, None);
        }

        #[test]
        fn valid_skeleton_always_ok(
            name in "[A-Za-z0-9 ]{1,40}",
            url_host in "[a-z0-9.-]{1,20}",
        ) {
            let body = format!(
                "#EXTM3U\n#EXTINF:-1,{name}\nhttps://{url_host}/stream.ts\n"
            );
            let bundle = parse_m3u(&body, None);
            prop_assert!(bundle.is_ok(), "{bundle:?}");
            prop_assert_eq!(bundle.unwrap().channels.len(), 1);
        }
    }
}
