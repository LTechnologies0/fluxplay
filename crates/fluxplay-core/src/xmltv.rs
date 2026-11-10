//! Minimal XMLTV / EPG parser.
//!
//! Resource posture (CWE-400 / CAPEC-197):
//! - `quick-xml` only expands predefined entities (`&amp;` …); DTD / custom entity
//!   refs (`Event::DocType` / `GeneralRef`) are ignored — no billion-laughs expansion.
//! - Programme count + field length soft-caps bound RSS after a capped HTTP body.

use chrono::{DateTime, NaiveDateTime, Utc};
use quick_xml::escape::{resolve_predefined_entity, unescape};
use quick_xml::events::{BytesText, Event};
use quick_xml::{Reader, XmlVersion};
use tracing::{debug, error, info, trace, warn};

use crate::error::{Error, Result};
use crate::models::EpgProgramme;
use crate::Stopwatch;

/// Soft cap — partial guide better than OOM (W3-10 F9 / WAVE4).
const MAX_PROGRAMMES: usize = 250_000;
/// Title / desc / category / channel_id after scrub.
const MAX_FIELD_CHARS: usize = 2 * 1024;

/// Parse XMLTV document bytes into programme list.
pub fn parse_xmltv(xml: &str) -> Result<Vec<EpgProgramme>> {
    parse_xmltv_limited(xml, MAX_PROGRAMMES, MAX_FIELD_CHARS)
}

fn parse_xmltv_limited(
    xml: &str,
    max_programmes: usize,
    max_field_chars: usize,
) -> Result<Vec<EpgProgramme>> {
    let _prof = Stopwatch::start("parse_xmltv");
    debug!(bytes = xml.len(), "parse_xmltv start");
    let mut reader = Reader::from_str(xml);
    // Do not trim_text: spaces adjacent to GeneralRef (`A &amp; B`) would be dropped.
    // Field-level `.trim()` on End still strips leading/trailing whitespace.

    let mut programmes = Vec::new();
    // Programmes without `stop` (optional in the XMLTV DTD), closed after parsing.
    let mut open_ended: Vec<EpgProgramme> = Vec::new();
    let mut buf = Vec::new();
    let mut truncated = false;

    let mut in_programme = false;
    let mut channel_id = String::new();
    let mut start: Option<DateTime<Utc>> = None;
    let mut stop: Option<DateTime<Utc>> = None;
    let mut title: Option<String> = None;
    let mut description: Option<String> = None;
    let mut category: Option<String> = None;
    let mut capture: Option<&'static str> = None;
    let mut text_buf = String::new();

    loop {
        if programmes.len() + open_ended.len() >= max_programmes {
            truncated = true;
            warn!(
                cap = max_programmes,
                "parse_xmltv truncated at programme cap"
            );
            break;
        }
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let name = name.as_ref();
                if name == b"programme" {
                    in_programme = true;
                    channel_id.clear();
                    start = None;
                    stop = None;
                    title = None;
                    description = None;
                    category = None;
                    for ax in e.attributes().flatten() {
                        let key = ax.key.as_ref();
                        // Predefined entities only (depth=1) — no custom DTD resolver.
                        let val = ax
                            .normalized_value(XmlVersion::Implicit1_0)
                            .map(|v| v.into_owned())
                            .unwrap_or_default();
                        if key == b"channel" {
                            channel_id =
                                truncate_field(scrub_xmltv_text(val.trim()), max_field_chars);
                        } else if key == b"start" {
                            start = parse_xmltv_time(&val);
                        } else if key == b"stop" {
                            stop = parse_xmltv_time(&val);
                        }
                    }
                } else if in_programme && name == b"title" {
                    capture = Some("title");
                    text_buf.clear();
                } else if in_programme && name == b"desc" {
                    capture = Some("desc");
                    text_buf.clear();
                } else if in_programme && name == b"category" {
                    capture = Some("category");
                    text_buf.clear();
                }
            }
            Ok(Event::Text(t)) => {
                if capture.is_some() {
                    append_capped(&mut text_buf, &xmltv_text_content(&t), max_field_chars);
                }
            }
            Ok(Event::CData(t)) => {
                if capture.is_some() {
                    append_capped(
                        &mut text_buf,
                        &String::from_utf8_lossy(&t),
                        max_field_chars,
                    );
                }
            }
            // CAPEC-197: DTD ignored; GeneralRef only for predefined / numeric refs.
            Ok(Event::DocType(_)) => {}
            Ok(Event::GeneralRef(r)) => {
                if capture.is_some() {
                    if let Some(piece) = safe_general_ref(&r) {
                        append_capped(&mut text_buf, &piece, max_field_chars);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                let name = name.as_ref();
                // Several `<title lang=…>` / `<desc>`: the first one (main language) wins.
                let text = truncate_field(
                    scrub_xmltv_text(std::mem::take(&mut text_buf).trim()),
                    max_field_chars,
                );
                if name == b"title" && capture == Some("title") {
                    if title.is_none() && !text.is_empty() {
                        title = Some(text);
                    }
                    capture = None;
                } else if name == b"desc" && capture == Some("desc") {
                    if description.is_none() && !text.is_empty() {
                        description = Some(text);
                    }
                    capture = None;
                } else if name == b"category" && capture == Some("category") {
                    if category.is_none() && !text.is_empty() {
                        category = Some(text);
                    }
                    capture = None;
                } else if name == b"programme" && in_programme {
                    if let (Some(st), Some(ti)) = (start.take(), title.take()) {
                        let p = EpgProgramme {
                            channel_id: channel_id.clone(),
                            title: ti,
                            description: description.take(),
                            start: st,
                            stop: stop.take().unwrap_or(st),
                            category: category.take(),
                        };
                        if p.stop > p.start {
                            programmes.push(p);
                        } else {
                            open_ended.push(p);
                        }
                    }
                    in_programme = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                // A truncated or partly malformed guide still yields what came before.
                if programmes.is_empty() && open_ended.is_empty() {
                    error!(error = %e, "parse_xmltv read failed");
                    return Err(Error::Parse(format!("xmltv: {e}")));
                }
                warn!(error = %e, parsed = programmes.len(), "parse_xmltv stopped early");
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    close_open_ended(&mut programmes, open_ended);

    if programmes.len() > max_programmes {
        programmes.truncate(max_programmes);
    }

    if programmes.is_empty() {
        warn!("parse_xmltv produced zero programmes");
    } else {
        info!(programmes = programmes.len(), truncated, "parse_xmltv done");
    }
    Ok(programmes)
}

fn truncate_field(s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s;
    }
    s.chars().take(max_chars).collect()
}

fn append_capped(buf: &mut String, chunk: &str, max_chars: usize) {
    if buf.chars().count() >= max_chars {
        return;
    }
    let room = max_chars.saturating_sub(buf.chars().count());
    if chunk.chars().count() <= room {
        buf.push_str(chunk);
    } else {
        buf.extend(chunk.chars().take(room));
    }
}

/// Drop C0 controls (NUL, DEL, …) that break UI labels / FFI while keeping text.
fn scrub_xmltv_text(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\t' || *c == '\n' || !c.is_control())
        .collect()
}

/// Decode XML text nodes: encoding/EOL normalize + predefined-entity unescape only.
fn xmltv_text_content(t: &BytesText<'_>) -> String {
    let raw = t
        .xml10_content()
        .or_else(|_| t.decode())
        .map(|c| c.into_owned())
        .unwrap_or_default();
    match unescape(&raw) {
        Ok(c) => c.into_owned(),
        Err(_) => raw,
    }
}

/// Resolve `&amp;` / `&#…;` only — custom DTD entities return `None` (no expansion).
fn safe_general_ref(r: &quick_xml::events::BytesRef<'_>) -> Option<String> {
    let name = r.decode().ok()?.into_owned();
    if let Some(pre) = resolve_predefined_entity(&name) {
        return Some(pre.to_string());
    }
    // Numeric character references (`#38`, `#x26`) via the same predefined-safe unescape.
    if name.starts_with('#') {
        let wrapped = format!("&{name};");
        match unescape(&wrapped) {
            Ok(c) => Some(c.into_owned()),
            Err(_) => None,
        }
    } else {
        None
    }
}

/// A programme without `stop` ends when the next one of its channel starts
/// (or after one hour for the last one).
fn close_open_ended(programmes: &mut Vec<EpgProgramme>, mut open_ended: Vec<EpgProgramme>) {
    if open_ended.is_empty() {
        return;
    }
    let mut starts: std::collections::HashMap<&str, Vec<DateTime<Utc>>> =
        std::collections::HashMap::new();
    for p in programmes.iter().chain(open_ended.iter()) {
        starts.entry(p.channel_id.as_str()).or_default().push(p.start);
    }
    for v in starts.values_mut() {
        v.sort_unstable();
    }
    let stops: Vec<DateTime<Utc>> = open_ended
        .iter()
        .map(|p| {
            starts
                .get(p.channel_id.as_str())
                .and_then(|v| v.iter().find(|s| **s > p.start).copied())
                .unwrap_or(p.start + chrono::Duration::hours(1))
        })
        .collect();
    for (p, stop) in open_ended.iter_mut().zip(stops) {
        p.stop = stop;
    }
    programmes.extend(open_ended);
}

/// XMLTV times are typically `YYYYMMDDHHmmss +ZZZZ` or without TZ (treated as UTC).
fn parse_xmltv_time(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    let digit_len = raw.bytes().take_while(|c| c.is_ascii_digit()).count();
    if digit_len < 12 {
        trace!(%raw, "parse_xmltv_time too short");
        return None;
    }
    let digits = &raw[..digit_len];
    let naive = NaiveDateTime::parse_from_str(&digits[..14.min(digit_len)], "%Y%m%d%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(&digits[..12], "%Y%m%d%H%M"))
        .ok()?;

    // Offset like " +0000" / "+0200"
    let offset_part = raw[digit_len..].trim();
    if offset_part.is_empty() {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }
    match parse_offset_secs(offset_part) {
        Some(secs) => Some(
            DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc) - chrono::Duration::seconds(secs),
        ),
        None => Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc)),
    }
}

/// `+0200`, `+02:00`, `+02`, `-0530`, `Z` → offset east of UTC in seconds.
fn parse_offset_secs(raw: &str) -> Option<i64> {
    let s: String = raw.chars().filter(|c| !c.is_whitespace() && *c != ':').collect();
    if s.eq_ignore_ascii_case("z") || s.eq_ignore_ascii_case("utc") || s.eq_ignore_ascii_case("gmt")
    {
        return Some(0);
    }
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    if !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (h, m) = match digits.len() {
        1 | 2 => (digits.parse::<i64>().ok()?, 0),
        4 => (
            digits[..2].parse::<i64>().ok()?,
            digits[2..].parse::<i64>().ok()?,
        ),
        _ => return None,
    };
    (h <= 14 && m < 60).then_some(sign * (h * 3600 + m * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_xmltv() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<tv>
  <programme start="20260904180000 +0000" stop="20260904190000 +0000" channel="tf1.fr">
    <title>Journal</title>
    <desc>Le JT</desc>
    <category>News</category>
  </programme>
</tv>"#;
        let list = parse_xmltv(xml).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Journal");
        assert_eq!(list[0].channel_id, "tf1.fr");
    }

    #[test]
    fn open_ended_programmes_and_short_offsets() {
        let xml = r#"<tv>
  <programme start="20260904180000 +02" channel=" tf1.fr "><title lang="fr"><![CDATA[Journal]]></title><title lang="en">News</title></programme>
  <programme start="20260904183000 +02:00" channel="tf1.fr"><title>Météo</title></programme>
</tv>"#;
        let mut list = parse_xmltv(xml).unwrap();
        list.sort_by_key(|p| p.start);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].title, "Journal");
        assert_eq!(list[0].channel_id, "tf1.fr");
        assert_eq!(list[0].start.format("%H:%M").to_string(), "16:00");
        assert_eq!(list[0].stop, list[1].start);
        assert_eq!(list[1].stop - list[1].start, chrono::Duration::hours(1));
    }

    #[test]
    fn billion_laughs_dtd_does_not_expand() {
        // CAPEC-197: nested custom entities must not amplify into title text.
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE tv [
  <!ENTITY lol "lol">
  <!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
  <!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">
]>
<tv>
  <programme start="20240101120000 +0000" stop="20240101130000 +0000" channel="x">
    <title>&lol3;</title>
  </programme>
</tv>"#;
        let list = parse_xmltv(xml).unwrap();
        // Custom entity ignored → empty/missing title → programme dropped, or literal short.
        for p in &list {
            assert!(
                p.title.len() < 64,
                "entity bomb expanded into title: {} bytes",
                p.title.len()
            );
            assert!(!p.title.contains("lollollol"));
        }
    }

    #[test]
    fn predefined_entities_unescaped() {
        let xml = r#"<tv>
  <programme start="20240101120000 +0000" stop="20240101130000 +0000" channel="c">
    <title>A &amp; B</title>
  </programme>
</tv>"#;
        let list = parse_xmltv(xml).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "A & B");
    }

    #[test]
    fn field_length_capped() {
        let cap = 32usize;
        let huge = "Z".repeat(cap + 500);
        let xml = format!(
            r#"<tv>
  <programme start="20240101120000 +0000" stop="20240101130000 +0000" channel="{huge}">
    <title>{huge}</title>
    <desc>{huge}</desc>
  </programme>
</tv>"#
        );
        let list = parse_xmltv_limited(&xml, MAX_PROGRAMMES, cap).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title.chars().count(), cap);
        assert_eq!(list[0].channel_id.chars().count(), cap);
        assert_eq!(list[0].description.as_ref().unwrap().chars().count(), cap);
    }

    #[test]
    fn programme_cap_stops_early() {
        let cap = 5usize;
        let mut xml = String::from("<tv>");
        for i in 0..20 {
            xml.push_str(&format!(
                r#"<programme start="2024010112{i:02}00 +0000" stop="2024010112{i:02}30 +0000" channel="c"><title>t{i}</title></programme>"#
            ));
        }
        xml.push_str("</tv>");
        let list = parse_xmltv_limited(&xml, cap, MAX_FIELD_CHARS).unwrap();
        assert_eq!(list.len(), cap);
    }
}
