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
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "programme" => {
                        in_programme = true;
                        channel_id.clear();
                        start = None;
                        stop = None;
                        title = None;
                        description = None;
                        category = None;
                        for ax in e.attributes().flatten() {
                            let key = String::from_utf8_lossy(ax.key.as_ref());
                            let val = ax
                                .unescape_value()
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            match key.as_ref() {
                                "channel" => channel_id = val,
                                "start" => start = parse_xmltv_time(&val),
                                "stop" => stop = parse_xmltv_time(&val),
                                _ => {}
                            }
                        }
                    }
                    "title" if in_programme => {
                        capture = Some("title");
                        text_buf.clear();
                    }
                    "desc" if in_programme => {
                        capture = Some("desc");
                        text_buf.clear();
                    }
                    "category" if in_programme => {
                        capture = Some("category");
                        text_buf.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if capture.is_some() {
                    text_buf.push_str(&t.unescape().unwrap_or_default());
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "title" if capture == Some("title") => {
                        title = Some(text_buf.clone());
                        capture = None;
                    }
                    "desc" if capture == Some("desc") => {
                        description = Some(text_buf.clone());
                        capture = None;
                    }
                    "category" if capture == Some("category") => {
                        category = Some(text_buf.clone());
                        capture = None;
                    }
                    "programme" if in_programme => {
                        if let (Some(st), Some(sp), Some(ti)) = (start, stop, title.clone()) {
                            programmes.push(EpgProgramme {
                                channel_id: channel_id.clone(),
                                title: ti,
                                description: description.clone(),
                                start: st,
                                stop: sp,
                                category: category.clone(),
                            });
                        }
                        in_programme = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                error!(error = %e, "parse_xmltv read failed");
                return Err(Error::Parse(format!("xmltv: {e}")));
            }
            _ => {}
        }
        buf.clear();
    }

    if programmes.is_empty() {
        warn!("parse_xmltv produced zero programmes");
    } else {
        info!(programmes = programmes.len(), "parse_xmltv done");
    }
    Ok(programmes)
}

/// XMLTV times are typically `YYYYMMDDHHmmss +ZZZZ` or without TZ (treated as UTC).
fn parse_xmltv_time(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    let digits: String = raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 12 {
        trace!(%raw, "parse_xmltv_time too short");
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(&digits[..14.min(digits.len())], "%Y%m%d%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(&digits[..12], "%Y%m%d%H%M"))
        .ok()?;

    // Offset like " +0000" / "+0200"
    let offset_part = raw[digits.len()..].trim();
    if offset_part.is_empty() {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }
    let cleaned = offset_part.replace(' ', "");
    if let Ok(fixed) = chrono::DateTime::parse_from_str(
        &format!("{} {}", naive.format("%Y-%m-%d %H:%M:%S"), cleaned),
        "%Y-%m-%d %H:%M:%S %z",
    ) {
        return Some(fixed.with_timezone(&Utc));
    }
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
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
}
