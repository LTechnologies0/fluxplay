//! Minimal XMLTV / EPG parser.

use chrono::{DateTime, NaiveDateTime, Utc};
use quick_xml::events::Event;
use quick_xml::Reader;
use tracing::{debug, error, info, trace, warn};

use crate::error::{Error, Result};
use crate::models::EpgProgramme;
use crate::Stopwatch;

/// Parse XMLTV document bytes into programme list.
pub fn parse_xmltv(xml: &str) -> Result<Vec<EpgProgramme>> {
    let _prof = Stopwatch::start("parse_xmltv");
    debug!(bytes = xml.len(), "parse_xmltv start");
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut programmes = Vec::new();
    // Programmes without `stop` (optional in the XMLTV DTD), closed after parsing.
    let mut open_ended: Vec<EpgProgramme> = Vec::new();
    let mut buf = Vec::new();

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
                            let val = ax
                                .unescape_value()
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            if key == b"channel" {
                                channel_id = val.trim().to_string();
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
                    text_buf.push_str(&t.unescape().unwrap_or_default());
                }
            }
            Ok(Event::CData(t)) => {
                if capture.is_some() {
                    text_buf.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                let name = name.as_ref();
                // Several `<title lang=…>` / `<desc>`: the first one (main language) wins.
                let text = std::mem::take(&mut text_buf).trim().to_string();
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

    if programmes.is_empty() {
        warn!("parse_xmltv produced zero programmes");
    } else {
        info!(programmes = programmes.len(), "parse_xmltv done");
    }
    Ok(programmes)
}

/// A programme without `stop` ends when the next one of its channel starts
/// (or after one hour for the last one).
fn close_open_ended(programmes: &mut Vec<EpgProgramme>, mut open_ended: Vec<EpgProgramme>) {
    if open_ended.is_empty() {
        return;
    }
    let mut starts: std::collections::HashMap<&str, Vec<DateTime<Utc>>> = std::collections::HashMap::new();
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
    if s.eq_ignore_ascii_case("z") || s.eq_ignore_ascii_case("utc") || s.eq_ignore_ascii_case("gmt") {
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
        4 => (digits[..2].parse::<i64>().ok()?, digits[2..].parse::<i64>().ok()?),
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
}
