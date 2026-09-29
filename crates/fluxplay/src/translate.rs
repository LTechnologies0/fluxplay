//! Synopsis translation into the viewer language.
//!
//! 1. Google Translate web endpoint (`translate.googleapis.com`, `client=gtx`, no key).
//!    Unofficial: rate-gated, 10 min cooldown on 429/403, chunks ≤ 1500 chars.
//! 2. MyMemory (`api.mymemory.translated.net`, no key, ~5000 chars/day) as fallback.
//!
//! Only called for detail pages the viewer opens; results are cached in SQLite
//! (`translations` table) so a synopsis is fetched once per language.

use std::sync::OnceLock;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

const GOOGLE_CHUNK: usize = 1500;
const MYMEMORY_CHUNK: usize = 450;

fn http() -> reqwest::Client {
    fluxplay_providers::app_http("Mozilla/5.0 (X11; Linux x86_64) FluxPlay/0.2", 12)
        .unwrap_or_else(|_| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(12))
                .build()
                .expect("translate HTTP client")
        })
}

struct Gate {
    min_interval: Duration,
    last: Mutex<Option<Instant>>,
    cooldown_until: Mutex<Option<Instant>>,
}

impl Gate {
    const fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last: Mutex::new(None),
            cooldown_until: Mutex::new(None),
        }
    }

    fn blocked(&self) -> bool {
        matches!(self.cooldown_until.lock().ok().and_then(|g| *g), Some(t) if Instant::now() < t)
    }

    fn trip(&self, secs: u64, who: &str) {
        if let Ok(mut g) = self.cooldown_until.lock() {
            *g = Some(Instant::now() + Duration::from_secs(secs));
        }
        warn!(api = who, secs, "translation rate limited — cooling down");
    }

    async fn wait_turn(&self) {
        let wait = match self.last.lock() {
            Ok(mut last) => {
                let now = Instant::now();
                let wait = last
                    .map(|t| self.min_interval.saturating_sub(now.saturating_duration_since(t)))
                    .unwrap_or(Duration::ZERO);
                *last = Some(now + wait);
                wait
            }
            Err(_) => Duration::ZERO,
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

fn google_gate() -> &'static Gate {
    static G: OnceLock<Gate> = OnceLock::new();
    G.get_or_init(|| Gate::new(Duration::from_millis(450)))
}

fn mymemory_gate() -> &'static Gate {
    static G: OnceLock<Gate> = OnceLock::new();
    G.get_or_init(|| Gate::new(Duration::from_millis(600)))
}

/// Split on sentence / line boundaries so each piece stays under `max` bytes.
fn chunks(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let push_piece = |piece: &str, cur: &mut String, out: &mut Vec<String>| {
        if cur.len() + piece.len() > max && !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
        if piece.len() > max {
            // One huge sentence: hard split on char boundaries.
            let mut start = 0;
            while start < piece.len() {
                let mut end = (start + max).min(piece.len());
                while !piece.is_char_boundary(end) {
                    end -= 1;
                }
                out.push(piece[start..end].to_string());
                start = end;
            }
        } else {
            cur.push_str(piece);
        }
    };
    let mut last = 0;
    for (i, c) in text.char_indices() {
        if matches!(c, '.' | '!' | '?' | '\n' | '。') {
            let end = i + c.len_utf8();
            push_piece(&text[last..end], &mut cur, &mut out);
            last = end;
        }
    }
    if last < text.len() {
        push_piece(&text[last..], &mut cur, &mut out);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Translated text + detected source language (ISO 639-1 when known).
#[derive(Debug, Clone)]
pub struct Translated {
    pub text: String,
    pub source_lang: Option<String>,
}

async fn google_chunk(text: &str, target: &str) -> Result<(String, Option<String>), bool> {
    let gate = google_gate();
    if gate.blocked() {
        return Err(false);
    }
    gate.wait_turn().await;
    let resp = http()
        .get("https://translate.googleapis.com/translate_a/single")
        .query(&[
            ("client", "gtx"),
            ("sl", "auto"),
            ("tl", target),
            ("dt", "t"),
            ("q", text),
        ])
        .send()
        .await
        .map_err(|e| {
            debug!(error = %e, "google translate request");
            false
        })?;
    let status = resp.status();
    if !status.is_success() {
        if matches!(status.as_u16(), 429 | 403 | 503) {
            gate.trip(600, "google");
        } else {
            debug!(%status, "google translate HTTP");
        }
        return Err(false);
    }
    let v: serde_json::Value = resp.json().await.map_err(|_| false)?;
    let mut out = String::new();
    for seg in v.get(0).and_then(|s| s.as_array()).into_iter().flatten() {
        if let Some(s) = seg.get(0).and_then(|x| x.as_str()) {
            out.push_str(s);
        }
    }
    let src = v.get(2).and_then(|x| x.as_str()).map(str::to_string);
    if out.trim().is_empty() {
        return Err(false);
    }
    Ok((out, src))
}

async fn mymemory_chunk(text: &str, source: &str, target: &str) -> Option<String> {
    let gate = mymemory_gate();
    if gate.blocked() {
        return None;
    }
    gate.wait_turn().await;
    let pair = format!("{source}|{target}");
    let resp = http()
        .get("https://api.mymemory.translated.net/get")
        .query(&[("q", text), ("langpair", pair.as_str())])
        .send()
        .await
        .ok()?;
    if resp.status().as_u16() == 429 {
        gate.trip(3600, "mymemory");
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let status = v
        .get("responseStatus")
        .and_then(|s| s.as_u64().or_else(|| s.as_str().and_then(|x| x.parse().ok())))
        .unwrap_or(0);
    if status == 429 || status == 403 {
        gate.trip(3600, "mymemory");
        return None;
    }
    if status != 200 {
        return None;
    }
    let t = v.pointer("/responseData/translatedText")?.as_str()?.to_string();
    // Quota / error notices come back as the "translation".
    if t.starts_with("MYMEMORY WARNING") || t.contains("QUERY LENGTH LIMIT") {
        gate.trip(3600, "mymemory");
        return None;
    }
    Some(t)
}

/// Translate `text` into `target` (ISO 639-1). `None` when every backend failed.
pub async fn translate(text: &str, target: &str) -> Option<Translated> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let target = crate::names::translate_code(target);

    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut src = None;
    let mut google_ok = true;
    for piece in chunks(text, GOOGLE_CHUNK) {
        match google_chunk(&piece, target).await {
            Ok((t, s)) => {
                out.push_str(&t);
                if !t.ends_with(char::is_whitespace) {
                    out.push(' ');
                }
                src = src.or(s);
            }
            Err(_) => {
                google_ok = false;
                break;
            }
        }
    }
    if google_ok {
        return Some(Translated {
            text: out.trim().to_string(),
            source_lang: src,
        });
    }

    let source = guess_lang(text).unwrap_or("en");
    if source == target {
        return None;
    }
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    for piece in chunks(text, MYMEMORY_CHUNK) {
        let t = mymemory_chunk(&piece, source, target).await?;
        out.push_str(&t);
        out.push(' ');
    }
    Some(Translated {
        text: out.trim().to_string(),
        source_lang: Some(source.to_string()),
    })
}

/// Cheap language guess (script + stop words) to skip texts already in the target.
pub fn guess_lang(text: &str) -> Option<&'static str> {
    let mut latin = 0usize;
    let mut scripts = [0usize; 8]; // ar, ru, el, he, zh/ja, ko, th, hi
    for c in text.chars().take(600) {
        match c as u32 {
            0x0041..=0x024F => latin += 1,
            0x0600..=0x06FF => scripts[0] += 1,
            0x0400..=0x04FF => scripts[1] += 1,
            0x0370..=0x03FF => scripts[2] += 1,
            0x0590..=0x05FF => scripts[3] += 1,
            0x3040..=0x30FF | 0x4E00..=0x9FFF => scripts[4] += 1,
            0xAC00..=0xD7AF => scripts[5] += 1,
            0x0E00..=0x0E7F => scripts[6] += 1,
            0x0900..=0x097F => scripts[7] += 1,
            _ => {}
        }
    }
    let (best, n) = scripts
        .iter()
        .enumerate()
        .max_by_key(|(_, n)| **n)
        .map(|(i, n)| (i, *n))
        .unwrap_or((0, 0));
    if n > latin {
        return Some(["ar", "ru", "el", "he", "zh", "ko", "th", "hi"][best]);
    }

    const STOP: &[(&str, &[&str])] = &[
        ("fr", &["le", "la", "les", "des", "une", "est", "et", "dans", "pour", "qui", "sur", "avec", "son", "sa", "au", "du", "il", "elle"]),
        ("en", &["the", "and", "of", "to", "is", "in", "his", "her", "with", "for", "a", "an", "who", "on", "when", "their"]),
        ("de", &["der", "die", "das", "und", "ist", "ein", "eine", "mit", "sich", "den", "von", "zu", "nicht", "auf", "seine"]),
        ("es", &["el", "los", "las", "una", "es", "con", "por", "para", "que", "su", "del", "se", "y", "lo"]),
        ("it", &["il", "gli", "una", "è", "con", "per", "che", "di", "della", "suo", "sua", "nel", "si", "non"]),
        ("pt", &["o", "os", "uma", "é", "com", "para", "que", "do", "da", "seu", "sua", "não", "em", "no"]),
        ("nl", &["de", "het", "een", "en", "is", "van", "met", "zijn", "die", "op", "niet", "voor", "hij", "zij"]),
        ("tr", &["bir", "ve", "bu", "ile", "için", "olan", "da", "de", "ama", "çok", "gibi", "onun"]),
    ];
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .take(120)
        .map(|w| w.to_lowercase())
        .collect();
    if words.len() < 4 {
        return None;
    }
    let mut best: Option<(&str, usize)> = None;
    for (lang, stops) in STOP {
        let hits = words.iter().filter(|w| stops.contains(&w.as_str())).count();
        if best.map(|(_, b)| hits > b).unwrap_or(true) {
            best = Some((lang, hits));
        }
    }
    best.filter(|(_, hits)| *hits * 8 >= words.len().min(80))
        .map(|(l, _)| l)
}

/// Stable cache key for (target language, source text).
pub fn cache_key(target: &str, text: &str) -> String {
    let text = text.trim();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{target}:{h:016x}:{}", text.len())
}

/// Translate several synopses (episode list) in as few requests as possible:
/// blocks are joined with a blank line and split back; a block count mismatch
/// falls back to one request per text.
pub async fn translate_many(texts: &[String], target: &str) -> Vec<(String, String)> {
    const SEP: &str = "\n\n";
    let mut out = Vec::new();
    let mut batch: Vec<&String> = Vec::new();
    let mut batch_len = 0usize;
    let mut batches: Vec<Vec<&String>> = Vec::new();
    for t in texts {
        if batch_len + t.len() + SEP.len() > GOOGLE_CHUNK && !batch.is_empty() {
            batches.push(std::mem::take(&mut batch));
            batch_len = 0;
        }
        batch_len += t.len() + SEP.len();
        batch.push(t);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    for b in batches {
        let joined = b
            .iter()
            .map(|t| t.replace('\n', " "))
            .collect::<Vec<_>>()
            .join(SEP);
        let Some(tr) = translate(&joined, target).await else {
            break;
        };
        let parts: Vec<&str> = tr
            .text
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() == b.len() {
            for (src, dst) in b.iter().zip(parts) {
                out.push(((*src).clone(), dst.to_string()));
            }
        } else {
            for src in b {
                match translate(src, target).await {
                    Some(t) => out.push((src.clone(), t.text)),
                    None => return out,
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses() {
        assert_eq!(
            guess_lang("Un jeune homme part à la recherche de son père dans une ville qui a oublié le passé."),
            Some("fr")
        );
        assert_eq!(
            guess_lang("A young man sets out to find his father in a city that forgot the past."),
            Some("en")
        );
        assert_eq!(guess_lang("مسلسل رمضان 2024 دراما عائلية جديدة"), Some("ar"));
        assert_eq!(guess_lang("Drama"), None);
    }

    #[test]
    fn chunking_keeps_text() {
        let s = "One. Two! Three? ".repeat(200);
        let parts = chunks(&s, 100);
        assert!(parts.iter().all(|p| p.len() <= 100));
        assert_eq!(parts.concat(), s);
    }

    #[test]
    fn keys_differ_by_lang() {
        assert_ne!(cache_key("fr", "Hello"), cache_key("de", "Hello"));
        assert_eq!(cache_key("fr", " Hello "), cache_key("fr", "Hello"));
    }
}
