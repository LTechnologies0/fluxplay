//! Metadata enrichment — multi-provider, rate-limited.
//!
//! Order (film/série):
//! 1. OMDb / IMDb (`OMDB_API_KEY`) — plot, cast, poster
//! 2. iTunes Search API (no key) — artwork + year
//! 3. TVMaze (series, no key) — gated + 429 backoff
//! 4. Wikipedia REST summary — plot fallback
//!
//! IPTV portals are **not** queried here (see Xtream `get_vod_info`).

use std::sync::OnceLock;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fluxplay_core::models::{SeriesItem, VodItem};
use serde::{Deserialize, Serialize};
use tracing::{debug, trace, warn};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaPatch {
    pub year: Option<String>,
    pub genre: Option<String>,
    pub plot: Option<String>,
    pub poster: Option<String>,
    pub rating: Option<String>,
    pub imdb_id: Option<String>,
    /// TMDB numeric id — used by The Open Skip Database for intro/outro.
    pub tmdb_id: Option<u32>,
    pub actors: Option<String>,
    pub director: Option<String>,
    pub writer: Option<String>,
    pub runtime: Option<String>,
    pub rated: Option<String>,
    pub awards: Option<String>,
    pub language: Option<String>,
    pub country: Option<String>,
}

/// One other title an actor appears in (IMDb id + poster when Wikidata has them).
#[derive(Debug, Clone)]
pub struct ActorCredit {
    pub title: String,
    pub year: Option<String>,
    pub poster: Option<String>,
    pub imdb_id: Option<String>,
    /// Filled by the app once the local catalog has been checked.
    pub in_catalog: bool,
}

/// Headshot plus filmography. Photo comes from IMDb's public suggestion API
/// (same image host as OMDb posters). Credits come from Wikidata (P161).
#[derive(Debug, Clone)]
pub struct ActorCard {
    pub name: String,
    pub photo: Option<String>,
    pub known_for: Option<String>,
    pub credits: Vec<ActorCredit>,
}

/// Recommended age, mapped from OMDb / portal ratings (`PG-13` → `13+`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgeRating {
    pub label: String,
    pub code: String,
}

/// Parsed IPTV title ready for OMDb / TVMaze (`t=` + optional `y=`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleQuery {
    pub title: String,
    pub year: Option<String>,
}

impl TitleQuery {
    pub fn cache_key(&self, kind: &str) -> String {
        format!(
            "{}:{}:{}",
            kind,
            self.title.to_ascii_lowercase(),
            self.year.as_deref().unwrap_or("-")
        )
    }
}

/// Strip IPTV catalog noise → searchable title + year.
///
/// Real DB patterns (FluxPlay catalog):
/// - `EN - Title - 2019` / `IN-EN - Title - 1992` (~84% VOD)
/// - `Title (2014)` / `Title (2024)_sub` (series)
/// - suffixes `_sub` `-fr` `_eng` `--it` `-DE`
/// - tags `4K` `4KL` `[Multi-Sub]` `NF -`
pub fn parse_title_query(raw: &str) -> TitleQuery {
    // Drop C0 controls early — iced labels / OMDB queries choke on embedded NUL.
    let mut s: String = raw
        .trim()
        .chars()
        .filter(|c| *c == '\t' || !c.is_control())
        .collect();
    // Normalize separators before suffix stripping
    s = s.replace(['\u{2013}', '\u{2014}'], "-");

    let mut year = extract_year(&s);

    // Lang / catalog prefix: "EN - ", "IN-EN - ", "XXX - ", "NF - ", "FR -4KL "
    if let Some(rest) = strip_lang_prefix(&s) {
        s = rest;
    }

    // Leading quality glued to title: "4KL American Sniper"
    s = strip_leading_quality(&s);

    // Trailing locale / sub tags before removing punctuation
    s = strip_trailing_locale_tags(&s);

    // Capture year again if prefix-only pass missed dash form after prefix strip
    if year.is_none() {
        year = extract_year(&s);
    }

    // Drop bracket / paren blocks (year already captured)
    s = strip_bracket_blocks(&s);

    // Dashed year tail: "Title - 2019" / "Title - 2017-sub" (sub already gone)
    if let Some((title, y)) = split_dashed_year(&s) {
        s = title;
        if year.is_none() {
            year = Some(y);
        }
    }

    s = s.replace(['.', '_', '*'], " ");
    s = s.replace(':', " ");
    // Collapse "  " and trim punctuation leftovers
    let mut words: Vec<&str> = s
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, '-' | ',' | ';' | '|' | '/')))
        .filter(|t| !t.is_empty() && !is_noise_token(&t.to_ascii_lowercase()))
        .collect();
    // A bare number is a year only as the trailing word of a longer title:
    // "1917", "2012" or "Blade Runner 2049 (2017)" keep their digits.
    if year.is_none() && words.len() > 1 {
        if let Some(y) = words.last().and_then(|t| parse_year_token(t)) {
            year = Some(y);
            words.pop();
        }
    }
    let cleaned = words.join(" ");

    let title = if cleaned.is_empty() {
        s
    } else {
        cleaned
    };
    TitleQuery { title, year }
}

/// Back-compat: title only.
#[allow(dead_code)]
pub fn clean_title(raw: &str) -> String {
    parse_title_query(raw).title
}

fn extract_year(s: &str) -> Option<String> {
    // Prefer (YYYY) then [YYYY]
    for (open, close) in [('(', ')'), ('[', ']')] {
        let mut search_from = 0usize;
        while let Some(rel) = s[search_from..].find(open) {
            let a = search_from + rel;
            if let Some(b) = s[a + 1..].find(close) {
                let inner = s[a + 1..a + 1 + b].trim();
                if let Some(y) = parse_year_token(inner) {
                    return Some(y);
                }
                search_from = a + 1 + b + 1;
            } else {
                break;
            }
        }
    }
    // " - 2019" or " - 2019_sub"
    for (i, _) in s.match_indices(" - ") {
        let rest = &s[i + 3..];
        let tok = rest
            .split(|c: char| c.is_whitespace() || c == '_' || c == '-' || c == ']')
            .next()
            .unwrap_or("");
        if let Some(y) = parse_year_token(tok) {
            return Some(y);
        }
    }
    None
}

fn parse_year_token(t: &str) -> Option<String> {
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() == 4 {
        if let Ok(y) = digits.parse::<u16>() {
            if (1900..=2100).contains(&y) {
                return Some(digits);
            }
        }
    }
    None
}

fn strip_lang_prefix(s: &str) -> Option<String> {
    // EN - / IN-EN - / ALB - / NRC - / XXX -
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if !(2..=3).contains(&i) {
        // allow IN-EN
        if let Some(rest) = s.split_once(" - ") {
            let head = rest.0.trim();
            if is_lang_tag(head) {
                return Some(rest.1.trim().to_string());
            }
        }
        return None;
    }
    // First token letters only length 2-3 — check for IN-EN form
    if let Some(rest) = s.split_once(" - ") {
        let head = rest.0.trim();
        if is_lang_tag(head) {
            return Some(rest.1.trim().to_string());
        }
    }
    // "FR -4KL Title" (no space after dash)
    if let Some(rest) = s.split_once('-') {
        let head = rest.0.trim();
        if is_lang_tag(head) {
            return Some(rest.1.trim().to_string());
        }
    }
    None
}

fn is_lang_tag(head: &str) -> bool {
    let u = head.to_ascii_uppercase();
    if matches!(
        u.as_str(),
        "EN" | "FR" | "DE" | "IT" | "ES" | "PL" | "AR" | "SE" | "NL" | "GR" | "IN" | "PT"
            | "TR" | "RU" | "XX" | "XXX" | "NF" | "NRC" | "DK" | "SC" | "AF" | "ALB" | "PK"
            | "ID" | "MX" | "US" | "UK" | "VO" | "VF" | "MULTI" | "EX" | "DS" | "NO" | "FI"
            | "HU" | "RO" | "CZ" | "SK" | "BG" | "HR" | "RS" | "UA" | "JP" | "KR" | "CN"
            | "BR" | "LAT" | "BIA" | "BAT" | "ONE" | "ALF" | "AZ" | "ZA" | "HQ" | "HE"
            | "SUB" | "RAW" | "CAM" | "WEB" | "BLU"
    ) {
        return true;
    }
    // IN-EN, EN-FR, …
    if let Some((a, b)) = u.split_once('-') {
        return a.len() <= 3
            && b.len() <= 3
            && a.chars().all(|c| c.is_ascii_alphabetic())
            && b.chars().all(|c| c.is_ascii_alphabetic());
    }
    false
}

fn strip_leading_quality(s: &str) -> String {
    let mut parts: Vec<&str> = s.split_whitespace().collect();
    while let Some(first) = parts.first() {
        let l = first.to_ascii_lowercase();
        if matches!(l.as_str(), "4k" | "4kl" | "uhd" | "hdr" | "hd" | "sd" | "cam" | "ts") {
            parts.remove(0);
        } else {
            break;
        }
    }
    parts.join(" ")
}

fn strip_trailing_locale_tags(s: &str) -> String {
    let mut out = s.trim().to_string();
    // Repeat: Title_sub / Title-fr / Title--it / Title_eng / Title-DE
    for _ in 0..3 {
        let lower = out.to_ascii_lowercase();
        let suffixes = [
            "_sub", "-sub", "_fr", "-fr", "_eng", "-eng", "_vo", "-vo", "_vf", "-vf", "_vostfr",
            "-vostfr", "--it", "-it", "--de", "-de", "--es", "-es", "-mx", "_mx", "-us", "_multi",
            "-multi", " multi-sub", "[multi-sub]",
        ];
        let mut stripped = false;
        for suf in suffixes {
            if lower.ends_with(suf) {
                out = out[..out.len() - suf.len()].trim().to_string();
                stripped = true;
                break;
            }
        }
        // Trailing spaced lang: "Guyane (2017) FR"
        if !stripped {
            if let Some((a, b)) = out.rsplit_once(' ') {
                if is_lang_tag(b) && b.len() <= 3 {
                    out = a.trim().to_string();
                    stripped = true;
                }
            }
        }
        if !stripped {
            break;
        }
    }
    out
}

fn strip_bracket_blocks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth_paren = 0i32;
    let mut depth_brack = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth_paren += 1,
            ')' => depth_paren = (depth_paren - 1).max(0),
            '[' => depth_brack += 1,
            ']' => depth_brack = (depth_brack - 1).max(0),
            _ if depth_paren == 0 && depth_brack == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn split_dashed_year(s: &str) -> Option<(String, String)> {
    if let Some(i) = s.rfind(" - ") {
        let (left, right) = s.split_at(i);
        let tok = right.trim_start_matches(" - ").trim();
        if let Some(y) = parse_year_token(tok) {
            return Some((left.trim().to_string(), y));
        }
    }
    None
}

fn is_noise_token(t: &str) -> bool {
    matches!(
        t,
        "1080p"
            | "720p"
            | "480p"
            | "2160p"
            | "4k"
            | "4kl"
            | "uhd"
            | "hdr"
            | "hdr10"
            | "dv"
            | "webrip"
            | "web-dl"
            | "bluray"
            | "blu-ray"
            | "bdrip"
            | "hdtv"
            | "hdtvrip"
            | "dvdrip"
            | "x264"
            | "x265"
            | "h264"
            | "h265"
            | "hevc"
            | "aac"
            | "dts"
            | "truehd"
            | "atmos"
            | "multi"
            | "vostfr"
            | "vf"
            | "vo"
            | "proper"
            | "repack"
            | "extended"
            | "unrated"
            | "directors"
            | "cut"
            | "remux"
            | "nf"
            | "amzn"
            | "dsnp"
            | "hmax"
            | "sub"
            | "multisub"
            | "multi-sub"
    )
}

/// Below this many chars a synopsis is a blurb that a fuller source may replace.
const SHORT_PLOT: usize = 80;

fn has_text(s: &Option<String>) -> bool {
    s.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false)
}

/// Keep the current synopsis unless it is missing / a blurb and the patch has more.
fn keeps_plot(current: Option<&str>, patch: Option<&str>) -> bool {
    let Some(new) = patch.filter(|p| !p.trim().is_empty()) else {
        return true;
    };
    match current.map(str::trim).filter(|c| !c.is_empty()) {
        None => false,
        Some(cur) => {
            let cur_en = crate::translate::guess_lang(cur) == Some("en");
            let new_en = crate::translate::guess_lang(new) == Some("en");
            if cur_en && !new_en {
                return false;
            }
            if !cur_en && new_en {
                return true;
            }
            cur.chars().count() >= SHORT_PLOT || cur.len() >= new.len()
        }
    }
}

impl MetaPatch {
    fn merge(&mut self, other: MetaPatch) {
        macro_rules! fill {
            ($field:ident) => {
                if self.$field.is_none() {
                    self.$field = other.$field.clone();
                }
            };
        }
        fill!(year);
        fill!(genre);
        fill!(poster);
        // First match wins: a later title search must not swap in another film's id.
        fill!(imdb_id);
        fill!(tmdb_id);
        fill!(rating);
        fill!(actors);
        fill!(director);
        fill!(writer);
        fill!(runtime);
        fill!(rated);
        fill!(awards);
        fill!(language);
        fill!(country);
        // Sources merge in trust order (OMDb → TVMaze → iTunes → Wikipedia): a later,
        // longer text only replaces a blurb — a same-name Wikipedia article or a
        // loose iTunes hit must not override the matched film's synopsis.
        match (&self.plot, &other.plot) {
            (Some(a), Some(b)) if a.chars().count() < SHORT_PLOT && b.len() > a.len() => {
                self.plot = other.plot
            }
            (None, Some(_)) => self.plot = other.plot,
            _ => {}
        }
    }

    fn is_empty(&self) -> bool {
        self.year.is_none()
            && self.genre.is_none()
            && self.plot.is_none()
            && self.poster.is_none()
            && self.rating.is_none()
            && self.imdb_id.is_none()
            && self.tmdb_id.is_none()
            && self.actors.is_none()
            && self.director.is_none()
            && self.writer.is_none()
            && self.runtime.is_none()
            && self.rated.is_none()
            && self.awards.is_none()
            && self.language.is_none()
            && self.country.is_none()
    }

    pub fn apply_vod(&self, item: &mut VodItem) {
        macro_rules! set {
            ($field:ident) => {
                if self.$field.is_some() {
                    item.$field = self.$field.clone();
                }
            };
        }
        set!(year);
        // Portal genre / synopsis are often already in the viewer's language.
        if !has_text(&item.genre) {
            item.genre = self.genre.clone();
        }
        if !keeps_plot(item.plot.as_deref(), self.plot.as_deref()) {
            item.plot = self.plot.clone();
        }
        if self.poster.is_some() && item.poster.is_none() {
            item.poster = self.poster.clone();
        }
        set!(rating);
        set!(imdb_id);
        set!(tmdb_id);
        set!(actors);
        set!(director);
        set!(writer);
        set!(runtime);
        set!(rated);
        set!(awards);
        set!(language);
        set!(country);
    }

    pub fn apply_series(&self, item: &mut SeriesItem) {
        macro_rules! set {
            ($field:ident) => {
                if self.$field.is_some() {
                    item.$field = self.$field.clone();
                }
            };
        }
        set!(year);
        if !has_text(&item.genre) {
            item.genre = self.genre.clone();
        }
        if !keeps_plot(item.plot.as_deref(), self.plot.as_deref()) {
            item.plot = self.plot.clone();
        }
        if self.poster.is_some() && item.cover.is_none() {
            item.cover = self.poster.clone();
        }
        set!(rating);
        set!(imdb_id);
        set!(tmdb_id);
        set!(actors);
        set!(director);
        set!(writer);
        set!(runtime);
        set!(rated);
        set!(awards);
        set!(language);
        set!(country);
    }
}

fn http() -> reqwest::Client {
    fluxplay_providers::app_http("FluxPlay/0.2 (+metadata; public APIs only)", 14).unwrap_or_else(
        |_| fluxplay_providers::emergency_http("FluxPlay/0.2 (+metadata; public APIs only)", 14),
    )
}

/// Shared throttle for a single public API host (min interval + 429 cooldown).
struct ApiGate {
    name: &'static str,
    min_interval: Duration,
    last: Mutex<Option<Instant>>,
    cooldown_until: Mutex<Option<Instant>>,
}

impl ApiGate {
    fn new(name: &'static str, min_interval_ms: u64) -> Self {
        Self {
            name,
            min_interval: Duration::from_millis(min_interval_ms),
            last: Mutex::new(None),
            cooldown_until: Mutex::new(None),
        }
    }

    fn blocked(&self) -> bool {
        let Ok(g) = self.cooldown_until.lock() else {
            return false;
        };
        matches!(*g, Some(t) if Instant::now() < t)
    }

    async fn wait_turn(&self) {
        let cooldown_wait = match self.cooldown_until.lock() {
            Ok(g) => (*g).and_then(|until| {
                let now = Instant::now();
                if until > now {
                    Some(until - now)
                } else {
                    None
                }
            }),
            Err(_) => None,
        };
        if let Some(wait) = cooldown_wait {
            debug!(api = self.name, ?wait, "api cooldown");
            tokio::time::sleep(wait).await;
        }
        let sleep_for = match self.last.lock() {
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
        if !sleep_for.is_zero() {
            tokio::time::sleep(sleep_for).await;
        }
    }

    fn trip_429(&self, secs: u64) {
        if let Ok(mut g) = self.cooldown_until.lock() {
            *g = Some(Instant::now() + Duration::from_secs(secs));
        }
        warn!(api = self.name, secs, "rate limited — cooling down");
    }
}

fn tvmaze_gate() -> &'static ApiGate {
    static G: OnceLock<ApiGate> = OnceLock::new();
    G.get_or_init(|| ApiGate::new("tvmaze", crate::policy::get().meta.gate_tvmaze_ms))
}

fn omdb_gate() -> &'static ApiGate {
    static G: OnceLock<ApiGate> = OnceLock::new();
    G.get_or_init(|| ApiGate::new("omdb", crate::policy::get().meta.gate_omdb_ms))
}

fn tmdb_gate() -> &'static ApiGate {
    static G: OnceLock<ApiGate> = OnceLock::new();
    G.get_or_init(|| ApiGate::new("tmdb", crate::policy::get().meta.gate_tmdb_ms))
}

fn itunes_gate() -> &'static ApiGate {
    static G: OnceLock<ApiGate> = OnceLock::new();
    G.get_or_init(|| ApiGate::new("itunes", crate::policy::get().meta.gate_itunes_ms))
}


pub fn imdb_title_url(imdb_id: &str) -> String {
    let id = imdb_id.trim();
    if id.starts_with("tt") {
        format!("https://www.imdb.com/title/{id}/")
    } else {
        format!("https://www.imdb.com/find/?q={}", urlencoding_lite(id))
    }
}

/// Photo (IMDb suggestion, same CDN as OMDb posters) and other titles (Wikidata).
/// `except_title` is the film or series already on screen, dropped from the list.
/// One IMDb suggestion, then a Wikipedia portrait. No Wikidata or AniList chain.
pub async fn person_headshot_fast(name: &str) -> Option<String> {
    let name = name.trim();
    if name.len() < 2 {
        return None;
    }
    if let Some(url) = imdb_person(name).await.and_then(|hit| hit.photo) {
        return Some(url);
    }
    wiki_portrait(name).await
}

/// French lead paragraph from fr.wikipedia. Kept only when the page title
/// matches the film or series, so a random article cannot replace the fiche.
pub async fn french_plot(title: &str) -> Option<String> {
    let title = title.trim();
    if title.len() < 2 {
        return None;
    }
    let q = urlencoding_lite(title);
    let url = format!(
        "https://fr.wikipedia.org/w/api.php?action=query&generator=search&gsrsearch={q}&gsrlimit=3&prop=extracts&exintro=1&explaintext=1&exchars=700&format=json&utf8=1"
    );
    let resp = http()
        .get(&url)
        .header("Accept", "application/json")
        .header("Accept-Language", "fr")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let pages = body.get("query")?.get("pages")?.as_object()?;
    let mut best: Option<(i64, String)> = None;
    for page in pages.values() {
        let index = page.get("index").and_then(|v| v.as_i64()).unwrap_or(99);
        let page_title = page.get("title")?.as_str().unwrap_or("");
        let extract = page.get("extract")?.as_str()?.trim();
        if extract.len() < 80 || !title_overlaps(page_title, title) {
            continue;
        }
        if crate::translate::guess_lang(extract) == Some("en") {
            continue;
        }
        if best.as_ref().map(|(i, _)| index < *i).unwrap_or(true) {
            best = Some((index, extract.to_string()));
        }
    }
    best.map(|(_, extract)| extract)
}

fn title_overlaps(page: &str, query: &str) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 4)
            .map(|w| w.to_lowercase())
            .collect()
    };
    let q = words(query);
    if q.is_empty() {
        return false;
    }
    let page = page.to_lowercase();
    let hits = q.iter().filter(|w| page.contains(w.as_str())).count();
    hits >= 1 && (q.len() == 1 || hits * 2 >= q.len())
}

async fn wiki_portrait(name: &str) -> Option<String> {
    let title = urlencoding_lite(name).replace('+', "%20");
    for host in ["fr.wikipedia.org", "en.wikipedia.org"] {
        let url = format!("https://{host}/api/rest_v1/page/summary/{title}");
        let resp = http()
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .ok();
        let Some(resp) = resp else { continue };
        if !resp.status().is_success() {
            continue;
        }
        let Some(body) = resp.json::<serde_json::Value>().await.ok() else {
            continue;
        };
        if let Some(src) = body
            .get("thumbnail")
            .and_then(|t| t.get("source"))
            .and_then(|s| s.as_str())
        {
            if src.starts_with("http") {
                return Some(src.to_string());
            }
        }
    }
    None
}

/// Headshot URL only (IMDb suggestion). Used to paint the cast on a title page.
pub async fn person_headshot(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Some(url) = imdb_person(name).await.and_then(|hit| hit.photo) {
        return Some(url);
    }
    if let Some(url) = wiki_person_photo(name).await {
        return Some(url);
    }
    if let Some(qid) = wikidata_person_qid(name).await {
        if let Some(url) = wikidata_portrait(&qid).await {
            return Some(url);
        }
    }
    // Anime voice actors often have no IMDb or Wikipedia portrait.
    anilist_staff_photo(name).await
}

/// Commons portrait (Wikidata P18) when IMDb has no suggestion image.
async fn wikidata_portrait(qid: &str) -> Option<String> {
    if !is_qid(qid) {
        return None;
    }
    let sparql = format!("SELECT ?img WHERE {{ wd:{qid} wdt:P18 ?img }} LIMIT 1");
    let url = format!(
        "https://query.wikidata.org/sparql?format=json&query={}",
        urlencoding_lite(&sparql)
    );
    let resp = http()
        .get(&url)
        .header("Accept", "application/sparql-results+json")
        .header("User-Agent", "FluxPlay/0.3 (cast photos)")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let img = body
        .get("results")?
        .get("bindings")?
        .as_array()?
        .first()?
        .get("img")?
        .get("value")?
        .as_str()?;
    Some(commons_thumb(img))
}

fn anilist_gate() -> &'static ApiGate {
    static G: OnceLock<ApiGate> = OnceLock::new();
    G.get_or_init(|| ApiGate::new("anilist", crate::policy::get().meta.gate_anilist_ms))
}

/// AniList staff portrait. The search hit is kept only when the name matches.
async fn anilist_staff_photo(name: &str) -> Option<String> {
    let gate = anilist_gate();
    if gate.blocked() {
        return None;
    }
    gate.wait_turn().await;
    let query = "query ($s: String) { Staff(search: $s) { name { full } image { large } } }";
    let resp = http()
        .post("https://graphql.anilist.co")
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "query": query,
            "variables": { "s": name }
        }))
        .send()
        .await
        .ok()?;
    if resp.status().as_u16() == 429 {
        gate.trip_429(60);
        return None;
    }
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let staff = body.get("data")?.get("Staff")?;
    let found = staff.get("name")?.get("full")?.as_str()?;
    if !credit_names_match(name, found) {
        return None;
    }
    let url = staff.get("image")?.get("large")?.as_str()?;
    if url.starts_with("http") && !url.contains("/default") {
        Some(url.to_string())
    } else {
        None
    }
}

/// "Ayasa Ito" matches AniList's "Ayasa Itou"; extra spaces are ignored.
fn credit_names_match(query: &str, found: &str) -> bool {
    let fold = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .map(|w| {
                w.chars()
                    .filter(|c| c.is_alphanumeric())
                    .flat_map(|c| c.to_lowercase())
                    .collect::<String>()
            })
            .filter(|w| !w.is_empty())
            .collect()
    };
    let q = fold(query);
    let f = fold(found);
    if q.is_empty() || q.len() != f.len() {
        return false;
    }
    q.iter().zip(f.iter()).all(|(a, b)| {
        a == b || a.trim_end_matches('u') == b.trim_end_matches('u')
    })
}

/// Wikipedia portrait when IMDb's suggestion has no picture.
async fn wiki_person_photo(name: &str) -> Option<String> {
    let page = name.split_whitespace().collect::<Vec<_>>().join("_");
    if page.chars().count() < 3 {
        return None;
    }
    for lang in ["en", "fr"] {
        let Some(summary) = wiki_fetch(lang, &page).await else {
            continue;
        };
        let desc = summary.description.as_deref().unwrap_or("");
        if !looks_like_person(desc) {
            continue;
        }
        let Some(src) = summary.thumbnail.as_ref().and_then(|t| t.source.clone()) else {
            continue;
        };
        if src.starts_with("http") && !src.to_ascii_lowercase().contains(".svg") {
            return Some(src);
        }
    }
    None
}

pub async fn lookup_actor(name: &str, except_title: &str) -> ActorCard {
    let name = name.trim();
    let mut card = ActorCard {
        name: name.to_string(),
        photo: None,
        known_for: None,
        credits: Vec::new(),
    };
    if name.is_empty() {
        return card;
    }
    if let Some(hit) = imdb_person(name).await {
        if !hit.name.is_empty() {
            card.name = hit.name;
        }
        card.photo = hit.photo;
        card.known_for = hit.known_for;
    }
    if let Some(qid) = wikidata_person_qid(name).await {
        if card.photo.is_none() {
            card.photo = wikidata_portrait(&qid).await;
        }
        if let Some(credits) = wikidata_credits(&qid, except_title).await {
            card.credits = credits;
        }
    }
    if card.credits.is_empty() {
        if let Some(credits) = wiki_filmography(name, except_title).await {
            card.credits = credits;
        }
    }
    // Posters for the titles we only have as names. A handful, so the grid
    // matches the catalog instead of a list of empty frames.
    let mut filled = 0u8;
    for credit in &mut card.credits {
        if filled >= 18 {
            break;
        }
        if let Some((year, poster, imdb)) = imdb_title_art(&credit.title).await {
            if credit.year.is_none() {
                credit.year = year;
            }
            if poster.is_some() {
                credit.poster = poster;
            }
            if credit.imdb_id.is_none() {
                credit.imdb_id = imdb;
            }
            filled += 1;
        }
    }
    card
}

/// Film titles from the Wikipedia filmography page (`''[[Title]]''`).
async fn wiki_filmography(name: &str, except_title: &str) -> Option<Vec<ActorCredit>> {
    let pages = [
        format!("{name} filmography"),
        format!("{name}"),
    ];
    for page in pages {
        let Some(text) = wiki_wikitext(&page).await else {
            continue;
        };
        let credits = filmography_titles(&text, except_title);
        if !credits.is_empty() {
            return Some(credits);
        }
    }
    None
}

async fn wiki_wikitext(page: &str) -> Option<String> {
    let url = format!(
        "https://en.wikipedia.org/w/api.php?action=parse&page={}&prop=wikitext&format=json&redirects=1",
        urlencoding_lite(page)
    );
    let resp = http()
        .get(&url)
        .header("User-Agent", "FluxPlay/0.3 (cast photos)")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("parse")?
        .get("wikitext")?
        .get("*")?
        .as_str()
        .map(|s| s.to_string())
}

fn filmography_titles(wikitext: &str, except_title: &str) -> Vec<ActorCredit> {
    let mut credits = Vec::new();
    let mut seen = Vec::new();
    let mut rest = wikitext;
    while let Some(pos) = rest.find("''[[") {
        let after = &rest[pos + 4..];
        let Some(end) = after.find("]]") else {
            break;
        };
        rest = &after[end + 2..];
        if end == 0 || end > 80 {
            continue;
        }
        let raw = &after[..end];
        let title = raw
            .split('|')
            .next()
            .unwrap_or(raw)
            .split('#')
            .next()
            .unwrap_or(raw)
            .trim();
        if title.contains(':') || title.len() < 2 {
            continue;
        }
        let lower = title.to_ascii_lowercase();
        if lower.contains("award")
            || lower.contains("golden globe")
            || lower.contains("academy")
            || lower.contains("film festival")
        {
            continue;
        }
        if seen.iter().any(|s: &String| s.eq_ignore_ascii_case(title)) {
            continue;
        }
        if titles_same(title, except_title) {
            continue;
        }
        seen.push(title.to_string());
        credits.push(ActorCredit {
            title: title.to_string(),
            year: None,
            poster: None,
            imdb_id: None,
            in_catalog: false,
        });
        if credits.len() == 18 {
            break;
        }
    }
    credits
}

/// Poster and year for one title, from IMDb's public suggestion API.
async fn imdb_title_art(title: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let letter = suggest_letter(title);
    let q = urlencoding_lite(title).replace('+', "%20");
    let url = format!("https://v3.sg.media-imdb.com/suggestion/{letter}/{q}.json");
    let resp = http()
        .get(&url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: ImdbSuggest = resp.json().await.ok()?;
    let is_title = |it: &ImdbSuggestItem| it.id.as_deref().unwrap_or("").starts_with("tt");
    let item = body
        .d
        .iter()
        .find(|it| {
            is_title(it)
                && it.l.as_deref().is_some_and(|l| {
                    l.eq_ignore_ascii_case(title) || titles_same(l, title)
                })
        })
        .or_else(|| {
            body.d.iter().find(|it| {
                is_title(it)
                    && matches!(
                        it.qid.as_deref(),
                        Some("movie") | Some("tvSeries") | Some("tvMovie") | Some("tvMiniSeries")
                    )
            })
        })?
        .clone();
    let year = item.y.map(|y| y.to_string());
    let poster = item
        .i
        .as_ref()
        .and_then(|i| i.image_url.clone())
        .filter(|u| u.starts_with("http"))
        .map(|u| imdb_portrait(&u));
    let imdb = item.id.filter(|id| id.starts_with("tt"));
    Some((year, poster, imdb))
}

struct ImdbPersonHit {
    name: String,
    photo: Option<String>,
    known_for: Option<String>,
}

async fn imdb_person(name: &str) -> Option<ImdbPersonHit> {
    let letter = suggest_letter(name);
    let q = urlencoding_lite(name).replace('+', "%20");
    let url = format!("https://v3.sg.media-imdb.com/suggestion/{letter}/{q}.json");
    let resp = http()
        .get(&url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: ImdbSuggest = resp.json().await.ok()?;
    let people: Vec<_> = body
        .d
        .into_iter()
        .filter(|it| it.id.as_deref().is_some_and(|id| id.starts_with("nm")))
        .collect();
    let item = people
        .iter()
        .find(|it| {
            it.l.as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case(name))
        })
        .or_else(|| people.first())?;
    let known = item.s.as_deref().and_then(clean_known_for);
    Some(ImdbPersonHit {
        name: item.l.clone().unwrap_or_else(|| name.to_string()),
        photo: item
            .i
            .as_ref()
            .and_then(|i| i.image_url.clone())
            .filter(|u| u.starts_with("http"))
            .map(|u| imdb_portrait(&u)),
        known_for: known,
    })
}

/// IMDb's `._V1_UX320_.jpg` resize is a progressive JPEG. iced paints some of
/// those as an empty frame (the file is on disk, the chip stays blank).
/// `QL75` asks Amazon for a baseline JPEG the renderer actually uploads.
fn imdb_portrait(url: &str) -> String {
    if let Some(i) = url.find("._V1") {
        return format!("{}._V1_QL75_UX240_.jpg", &url[..i]);
    }
    url.to_string()
}

fn suggest_letter(name: &str) -> char {
    name.chars()
        .find_map(|c| {
            let folded = fold_letter(c);
            folded.is_ascii_alphabetic().then_some(folded)
        })
        .unwrap_or('a')
}

fn fold_letter(c: char) -> char {
    match c {
        'É' | 'È' | 'Ê' | 'Ë' | 'é' | 'è' | 'ê' | 'ë' => 'e',
        'À' | 'Á' | 'Â' | 'Ä' | 'à' | 'á' | 'â' | 'ä' => 'a',
        'Î' | 'Ï' | 'î' | 'ï' => 'i',
        'Ô' | 'Ö' | 'ô' | 'ö' => 'o',
        'Ù' | 'Û' | 'Ü' | 'ù' | 'û' | 'ü' => 'u',
        'Ç' | 'ç' => 'c',
        other if other.is_ascii_alphabetic() => other.to_ascii_lowercase(),
        other => other,
    }
}

fn clean_known_for(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    for prefix in ["actor, ", "actress, ", "acteur, ", "actrice, ", "producer, "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = s[s.len() - rest.len()..].trim();
            return (!rest.is_empty()).then(|| rest.to_string());
        }
    }
    Some(s.to_string())
}

async fn wikidata_person_qid(name: &str) -> Option<String> {
    let url = format!(
        "https://www.wikidata.org/w/api.php?action=wbsearchentities&search={}&language=fr&uselang=fr&format=json&type=item&limit=5",
        urlencoding_lite(name)
    );
    let resp = http().get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: WikiSearch = resp.json().await.ok()?;
    let mut best: Option<&WikiHit> = None;
    for hit in &body.search {
        let label_ok = hit
            .label
            .as_deref()
            .is_some_and(|l| l.eq_ignore_ascii_case(name));
        let person = hit
            .description
            .as_deref()
            .is_some_and(looks_like_person);
        if label_ok && person {
            return hit.id.clone().filter(|id| is_qid(id));
        }
        if best.is_none() && (person || label_ok) {
            best = Some(hit);
        }
    }
    best.and_then(|h| h.id.clone()).filter(|id| is_qid(id))
}

fn looks_like_person(desc: &str) -> bool {
    let d = desc.to_ascii_lowercase();
    [
        "actor", "actress", "acteur", "actrice", "réalisat", "realisat", "director",
        "producer", "producteur", "productrice", "comédien", "comedien", "comedian",
        "screenwriter", "scénariste", "doubleur", "doubleuse", "seiyu", "voice",
    ]
    .iter()
    .any(|w| d.contains(w))
}

fn is_qid(id: &str) -> bool {
    let rest = id.strip_prefix('Q').unwrap_or("");
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

async fn wikidata_credits(qid: &str, except_title: &str) -> Option<Vec<ActorCredit>> {
    if !is_qid(qid) {
        return None;
    }
    let sparql = format!(
        "SELECT ?itemLabel (MAX(?year) AS ?y) (SAMPLE(?imdb) AS ?tt) (SAMPLE(COALESCE(?poster, ?image)) AS ?img) WHERE {{ ?item wdt:P161 wd:{qid} . OPTIONAL {{ ?item wdt:P577 ?date . BIND(YEAR(?date) AS ?year) }} OPTIONAL {{ ?item wdt:P345 ?imdb }} OPTIONAL {{ ?item wdt:P3383 ?poster }} OPTIONAL {{ ?item wdt:P18 ?image }} SERVICE wikibase:label {{ bd:serviceParam wikibase:language \"fr,en\". }} }} GROUP BY ?item ?itemLabel ORDER BY DESC(?y) LIMIT 24"
    );
    let url = format!(
        "https://query.wikidata.org/sparql?format=json&query={}",
        urlencoding_lite(&sparql)
    );
    let resp = http()
        .get(&url)
        .header("Accept", "application/sparql-results+json")
        .header("User-Agent", "FluxPlay/0.3 (cast photos)")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: SparqlResp = resp.json().await.ok()?;
    let mut credits = Vec::new();
    let mut seen = Vec::new();
    for row in body.results.bindings {
        let Some(title) = row.item_label.and_then(|b| b.value) else {
            continue;
        };
        if title.starts_with('Q') && title[1..].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let key = title.to_ascii_lowercase();
        if seen.iter().any(|s: &String| s == &key) {
            continue;
        }
        if titles_same(&title, except_title) {
            continue;
        }
        seen.push(key);
        let imdb_id = row.tt.and_then(|b| b.value).filter(|s| s.starts_with("tt"));
        let poster = row.img.and_then(|b| b.value).map(|u| commons_thumb(&u));
        let year = row.y.and_then(|b| b.value).filter(|y| y.len() == 4);
        credits.push(ActorCredit {
            title,
            year,
            poster,
            imdb_id,
            in_catalog: false,
        });
        if credits.len() == 16 {
            break;
        }
    }
    Some(credits)
}

fn rating_query_title(title: &str) -> String {
    let cleaned = title.replace(['|', '"', '\\', '{', '}'], " ");
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let start = if words.len() > 1
        && words[0].len() == 2
        && words[0].chars().all(|c| c.is_ascii_alphabetic())
    {
        1
    } else {
        0
    };
    words[start..].join(" ")
}

async fn wikidata_rating(title: &str, year: Option<&str>, _series: bool) -> Option<String> {
    let title = rating_query_title(title);
    if title.len() < 2 {
        return None;
    }
    let year_filter = match year {
        Some(y) if y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()) => {
            format!("FILTER(!BOUND(?date) || YEAR(?date) = {y})")
        }
        _ => String::new(),
    };
    // P31 is not required: anime is not typed as a plain television series, and a
    // missing publication date must not hide a rating we already matched by title.
    let sparql = format!(
        "SELECT ?itemLabel WHERE {{ ?film rdfs:label \"{title}\"@en . OPTIONAL {{ ?film wdt:P577 ?date . }} {year_filter} ?film wdt:P1657 ?item . SERVICE wikibase:label {{ bd:serviceParam wikibase:language \"en\". }} }} LIMIT 1"
    );
    let url = format!(
        "https://query.wikidata.org/sparql?format=json&query={}",
        urlencoding_lite(&sparql)
    );
    let resp = http().get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: SparqlResp = resp.json().await.ok()?;
    let code = body
        .results
        .bindings
        .into_iter()
        .find_map(|row| row.item_label.and_then(|b| b.value))?;
    let Some(age) = age_rating(Some(&code)) else {
        return None;
    };
    tracing::info!(%title, code = %age.code, label = %age.label, "age rating");
    Some(age.code)
}

/// Kitsu age for a series (`PG-13`, `Teens 13 or older`). No key, one search.
async fn kitsu_age(title: &str) -> Option<String> {
    let title = title.trim();
    if title.len() < 2 {
        return None;
    }
    let url = format!(
        "https://kitsu.io/api/edge/anime?filter[text]={}&page[limit]=4",
        urlencoding_lite(title)
    );
    let resp = http()
        .get(&url)
        .header("Accept", "application/vnd.api+json")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: KitsuResp = resp.json().await.ok()?;
    let hit = body.data.into_iter().find(|item| {
        item.attributes
            .as_ref()
            .and_then(|a| a.canonical_title.as_deref())
            .is_some_and(|name| title_overlap(title, name))
    })?;
    let attr = hit.attributes?;
    let guide = attr.age_rating_guide.as_deref().unwrap_or("");
    let code = attr.age_rating.as_deref().unwrap_or("");
    let raw = if first_age_number(guide).is_some() {
        guide
    } else if !code.is_empty() {
        code
    } else {
        return None;
    };
    let age = age_rating(Some(raw))?;
    tracing::info!(%title, code = %age.code, label = %age.label, "kitsu age");
    Some(age.code)
}

fn title_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(|w| w.to_lowercase())
        .collect()
}

fn title_overlap(want: &str, have: &str) -> bool {
    let want = title_words(want);
    let have = title_words(have);
    if want.is_empty() || have.is_empty() {
        return false;
    }
    want.iter().any(|t| {
        have.iter()
            .any(|h| h == t || h.contains(t.as_str()) || t.contains(h.as_str()))
    })
}

#[derive(Debug, Deserialize)]
struct KitsuResp {
    #[serde(default)]
    data: Vec<KitsuItem>,
}

#[derive(Debug, Deserialize)]
struct KitsuItem {
    attributes: Option<KitsuAttr>,
}

#[derive(Debug, Deserialize)]
struct KitsuAttr {
    #[serde(rename = "canonicalTitle")]
    canonical_title: Option<String>,
    #[serde(rename = "ageRating")]
    age_rating: Option<String>,
    #[serde(rename = "ageRatingGuide")]
    age_rating_guide: Option<String>,
}

fn titles_same(a: &str, b: &str) -> bool {
    let a = fold_title(a);
    let b = fold_title(b);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    short.len() >= 8 && long.contains(short.as_str())
}

fn fold_title(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn commons_thumb(url: &str) -> String {
    let url = url.trim().replace("http://", "https://");
    if url.contains("Special:FilePath/") && !url.contains("width=") {
        format!("{url}?width=240")
    } else {
        url
    }
}

#[derive(Debug, Deserialize)]
struct ImdbSuggest {
    #[serde(default)]
    d: Vec<ImdbSuggestItem>,
}

#[derive(Debug, Clone, Deserialize)]
struct ImdbSuggestItem {
    id: Option<String>,
    l: Option<String>,
    s: Option<String>,
    #[serde(default)]
    y: Option<i64>,
    #[serde(default)]
    qid: Option<String>,
    i: Option<ImdbSuggestImage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ImdbSuggestImage {
    #[serde(rename = "imageUrl")]
    image_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WikiSearch {
    #[serde(default)]
    search: Vec<WikiHit>,
}

#[derive(Debug, Deserialize)]
struct WikiHit {
    id: Option<String>,
    label: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SparqlResp {
    results: SparqlResults,
}

#[derive(Debug, Deserialize)]
struct SparqlResults {
    #[serde(default)]
    bindings: Vec<SparqlRow>,
}

#[derive(Debug, Deserialize)]
struct SparqlRow {
    #[serde(rename = "itemLabel")]
    item_label: Option<SparqlValue>,
    y: Option<SparqlValue>,
    tt: Option<SparqlValue>,
    img: Option<SparqlValue>,
}

#[derive(Debug, Deserialize)]
struct SparqlValue {
    value: Option<String>,
}

pub async fn enrich_series(name: &str) -> Option<MetaPatch> {
    enrich_title_full(name, "series", None).await
}

pub async fn enrich_vod(name: &str) -> Option<MetaPatch> {
    enrich_title_full(name, "movie", None).await
}

/// Full media-page detail (synopsis + cast). Prefer `i=tt…` when known.
///
/// Strategy (keyless first — works with zero API keys):
/// 1. TVMaze (series) / iTunes / Wikipedia / Wikidata / Kitsu
/// 2. TMDB when a user token is configured
/// 3. OMDb when a user key is configured
pub async fn enrich_title_full(
    name: &str,
    kind: &str,
    imdb_id: Option<&str>,
) -> Option<MetaPatch> {
    let q = parse_title_query(name);
    debug!(%name, title = %q.title, year = ?q.year, %kind, "enrich_title_full");
    let mut patch = MetaPatch::default();

    // --- Keyless public APIs (no signup) ---------------------------------
    if kind == "series" {
        if let Some(p) = tvmaze_show(&q.title).await {
            patch.merge(p);
        }
    }
    if patch.poster.is_none() || patch.year.is_none() {
        if let Some(p) = itunes_lookup(&q, kind == "series").await {
            patch.merge(p);
        }
    }
    if patch.plot.as_ref().map(|p| p.len() < 40).unwrap_or(true) {
        if let Some(p) = wikipedia_summary(&q, kind == "series").await {
            patch.merge(p);
        }
    }
    if patch.rated.is_none() {
        if let Some(code) =
            wikidata_rating(&q.title, q.year.as_deref(), kind == "series").await
        {
            patch.rated = Some(code);
        }
    }
    if patch.rated.is_none() && kind == "series" {
        if let Some(code) = kitsu_age(&rating_query_title(&q.title)).await {
            patch.rated = Some(code);
        }
    }
    if patch.year.is_none() {
        patch.year = q.year.clone();
    }

    // --- Optional keyed providers (user-owned keys only) -----------------
    if tmdb_configured() {
        if let Some(p) = tmdb_fiche(name, kind, imdb_id).await {
            patch.merge(p);
        }
    }

    if omdb_configured()
        && (needs_full_credits(patch.actors.as_deref(), patch.plot.as_deref())
            || patch.imdb_id.is_none())
    {
        if let Some(id) = imdb_id.filter(|s| s.starts_with("tt")) {
            if let Some(p) = omdb_by_id(id).await {
                patch.merge(p);
            }
        }
        if needs_full_credits(patch.actors.as_deref(), patch.plot.as_deref()) {
            if let Some(p) = omdb_lookup(&q, kind).await {
                patch.merge(p);
            }
        }
        if needs_full_credits(patch.actors.as_deref(), patch.plot.as_deref()) && !kind.is_empty() {
            if let Some(p) = omdb_lookup(&q, "").await {
                patch.merge(p);
            }
        }
        if needs_full_credits(patch.actors.as_deref(), patch.plot.as_deref())
            || patch.imdb_id.is_none()
        {
            if let Some(hit) = omdb_search_best(&q, kind).await {
                if let Some(id) = hit.imdb_id.clone() {
                    if let Some(full) = omdb_by_id(&id).await {
                        patch.merge(full);
                    } else {
                        patch.merge(hit);
                    }
                } else {
                    patch.merge(hit);
                }
            }
        }
    }

    if patch.is_empty() {
        None
    } else {
        Some(patch)
    }
}

/// Fill episode plot/airdate/still from TVMaze when seasons are loaded.
pub async fn enrich_series_episodes(series_name: &str, item: &mut SeriesItem) {
    let q = parse_title_query(series_name);
    let Some(eps) = tvmaze_episodes(&q.title).await else {
        return;
    };
    for season in &mut item.seasons {
        for ep in &mut season.episodes {
            let Some(src) = eps
                .iter()
                .find(|e| e.season == Some(season.season_number) && e.number == Some(ep.episode_num))
            else {
                continue;
            };
            if ep.plot.is_none() {
                if let Some(sum) = src.summary.as_ref() {
                    let p = strip_html(sum);
                    if !p.is_empty() {
                        ep.plot = Some(p);
                    }
                }
            }
            if ep.airdate.is_none() {
                ep.airdate = src.airdate.clone();
            }
            if ep.runtime.is_none() {
                ep.runtime = src.runtime.map(|m| format!("{m} min"));
            }
            if ep.rating.is_none() {
                ep.rating = src
                    .rating
                    .as_ref()
                    .and_then(|r| r.average)
                    .map(|a| format!("{a:.1}"));
            }
            if ep.still.is_none() {
                ep.still = src
                    .image
                    .as_ref()
                    .and_then(|i| i.medium.clone().or(i.original.clone()));
            }
        }
    }
}

pub fn needs_series_enrich(s: &SeriesItem) -> bool {
    s.imdb_id.is_none()
        || s.plot.as_ref().map(|p| p.len() < 40).unwrap_or(true)
        || s.actors.is_none()
        || s.director.is_none()
        || s.rating.is_none()
        || (s.cover.is_none() && s.banner.is_none())
}

pub fn needs_vod_enrich(v: &VodItem) -> bool {
    v.imdb_id.is_none()
        || v.plot.as_ref().map(|p| p.len() < 40).unwrap_or(true)
        || v.actors.is_none()
        || v.director.is_none()
        || v.rating.is_none()
        || v.poster.is_none()
}

pub fn needs_full_credits(actors: Option<&str>, plot: Option<&str>) -> bool {
        actors.map(|s| s.is_empty()).unwrap_or(true)
        || plot.map(|s| s.len() < 40).unwrap_or(true)
}

async fn tvmaze_show(name: &str) -> Option<MetaPatch> {
    let gate = tvmaze_gate();
    if gate.blocked() {
        trace!(%name, "tvmaze skipped (cooldown)");
        return None;
    }
    gate.wait_turn().await;
    let url = format!(
        "https://api.tvmaze.com/singlesearch/shows?q={}",
        urlencoding_lite(name)
    );
    let resp = http().get(&url).send().await.ok()?;
    let status = resp.status();
    if !status.is_success() {
        if status.as_u16() == 429 {
            gate.trip_429(90);
        } else if status.as_u16() == 404 {
            trace!(%name, "tvmaze miss");
        } else {
            warn!(%name, status = %status, "tvmaze HTTP");
        }
        return None;
    }
    let show: TvMazeShow = match resp.json().await {
        Ok(s) => s,
        Err(e) => {
            warn!(%name, error = %e, "tvmaze JSON");
            return None;
        }
    };
    let genre = if show.genres.is_empty() {
        None
    } else {
        Some(show.genres.join(", "))
    };
    let year = show
        .premiered
        .as_deref()
        .and_then(|d| d.get(0..4))
        .map(str::to_string);
    let plot = show
        .summary
        .map(|s| strip_html(&s))
        .filter(|s| !s.is_empty());
    let poster = show.image.and_then(|i| i.medium.or(i.original));
    let rating = show.rating.and_then(|r| r.average).map(|a| format!("{a:.1}"));
    let imdb_id = show.externals.and_then(|e| e.imdb).filter(|id| !id.is_empty());
    let actors = if let Some(id) = show.id {
        tvmaze_cast(id).await
    } else {
        None
    };
    debug!(%name, ?year, ?genre, ?imdb_id, "tvmaze hit");
    Some(MetaPatch {
        year,
        genre,
        plot,
        poster,
        rating,
        imdb_id,
        actors,
        ..Default::default()
    })
}

async fn tvmaze_cast(show_id: u64) -> Option<String> {
    let gate = tvmaze_gate();
    if gate.blocked() {
        return None;
    }
    gate.wait_turn().await;
    let url = format!("https://api.tvmaze.com/shows/{show_id}/cast");
    let resp = http().get(&url).send().await.ok()?;
    if resp.status().as_u16() == 429 {
        gate.trip_429(90);
        return None;
    }
    if !resp.status().is_success() {
        return None;
    }
    let cast: Vec<TvMazeCastEntry> = resp.json().await.ok()?;
    let names: Vec<String> = cast
        .into_iter()
        .filter_map(|c| c.person.and_then(|p| p.name))
        .take(8)
        .collect();
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

async fn tvmaze_episodes(name: &str) -> Option<Vec<TvMazeEpisode>> {
    let gate = tvmaze_gate();
    if gate.blocked() {
        return None;
    }
    gate.wait_turn().await;
    let url = format!(
        "https://api.tvmaze.com/singlesearch/shows?q={}&embed=episodes",
        urlencoding_lite(name)
    );
    let resp = http().get(&url).send().await.ok()?;
    if resp.status().as_u16() == 429 {
        gate.trip_429(90);
        return None;
    }
    if !resp.status().is_success() {
        return None;
    }
    let show: TvMazeShowEmbed = resp.json().await.ok()?;
    show.embedded.map(|e| e.episodes).filter(|v| !v.is_empty())
}

async fn omdb_lookup(q: &TitleQuery, kind: &str) -> Option<MetaPatch> {
    let key = omdb_key()?;
    let mut url = format!(
        "https://www.omdbapi.com/?apikey={}&t={}&plot=full",
        urlencoding_lite(&key),
        urlencoding_lite(&q.title)
    );
    if !kind.is_empty() {
        url.push_str("&type=");
        url.push_str(kind);
    }
    if let Some(y) = &q.year {
        url.push_str("&y=");
        url.push_str(y);
    }
    omdb_get(&url, &q.title).await
}

async fn omdb_by_id(imdb_id: &str) -> Option<MetaPatch> {
    let key = omdb_key()?;
    let url = format!(
        "https://www.omdbapi.com/?apikey={}&i={}&plot=full",
        urlencoding_lite(&key),
        urlencoding_lite(imdb_id)
    );
    omdb_get(&url, imdb_id).await
}

/// Search (`s=`) then pick the best row (year + title similarity), not blindly the first.
async fn omdb_search_best(q: &TitleQuery, kind: &str) -> Option<MetaPatch> {
    let key = omdb_key()?;
    let mut candidates = Vec::new();

    // Try full title, then a shortened form (drop trailing subtitle noise).
    let mut queries = vec![q.title.clone()];
    if let Some((head, _)) = q.title.split_once(':') {
        let h = head.trim();
        if h.len() >= 3 {
            queries.push(h.to_string());
        }
    }
    let words: Vec<_> = q.title.split_whitespace().collect();
    if words.len() > 4 {
        queries.push(words[..4].join(" "));
    }

    for title_q in queries {
        let mut url = format!(
            "https://www.omdbapi.com/?apikey={}&s={}",
            urlencoding_lite(&key),
            urlencoding_lite(&title_q)
        );
        if !kind.is_empty() {
            url.push_str("&type=");
            url.push_str(kind);
        }
        if let Some(y) = &q.year {
            url.push_str("&y=");
            url.push_str(y);
        }
        let Some(resp) = http().get(&url).send().await.ok() else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(body) = resp.json::<OmdbSearchResp>().await else {
            continue;
        };
        if body.response.as_deref() == Some("False") {
            // Retry without year — OMDb year filter is strict.
            if q.year.is_some() {
                let mut url2 = format!(
                    "https://www.omdbapi.com/?apikey={}&s={}",
                    urlencoding_lite(&key),
                    urlencoding_lite(&title_q)
                );
                if !kind.is_empty() {
                    url2.push_str("&type=");
                    url2.push_str(kind);
                }
                if let Ok(resp2) = http().get(&url2).send().await {
                    if let Ok(body2) = resp2.json::<OmdbSearchResp>().await {
                        if let Some(list) = body2.search {
                            candidates.extend(list);
                        }
                    }
                }
            }
            continue;
        }
        if let Some(list) = body.search {
            candidates.extend(list);
        }
        if !candidates.is_empty() {
            break;
        }
    }

    if candidates.is_empty() {
        return None;
    }

    let want = q.title.to_ascii_lowercase();
    let want_year = q.year.clone();
    candidates.sort_by_key(|c| {
        let title = c.title.as_deref().unwrap_or("").to_ascii_lowercase();
        let year_ok = match (&want_year, c.year.as_deref()) {
            (Some(wy), Some(cy)) if cy.starts_with(wy.as_str()) => 0i32,
            (Some(_), _) => 2,
            _ => 1,
        };
        let exact = if title == want { 0 } else { 1 };
        let starts = if title.starts_with(&want) || want.starts_with(&title) {
            0
        } else {
            1
        };
        (year_ok, exact, starts, title.len() as i32)
    });

    let best = candidates.into_iter().next()?;
    debug!(
        query = %q.title,
        hit = ?best.title,
        year = ?best.year,
        id = ?best.imdb_id,
        "omdb search best"
    );
    Some(MetaPatch {
        year: best.year.filter(|y| y != "N/A"),
        poster: best.poster.filter(|p| p != "N/A"),
        imdb_id: best.imdb_id.filter(|id| id != "N/A"),
        ..Default::default()
    })
}

static OMDB_KEY_RUNTIME: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Push the settings / UI key so OMDb works without restarting for env alone.
pub fn set_omdb_api_key(key: Option<String>) {
    let cell = OMDB_KEY_RUNTIME.get_or_init(|| Mutex::new(None));
    let cleaned = key.and_then(|k| {
        let t = k.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    });
    if let Ok(mut g) = cell.lock() {
        *g = cleaned;
    }
}

pub fn omdb_configured() -> bool {
    omdb_key().is_some()
}

fn omdb_key() -> Option<String> {
    if let Ok(k) = std::env::var("OMDB_API_KEY") {
        let t = k.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let cell = OMDB_KEY_RUNTIME.get_or_init(|| Mutex::new(None));
    cell.lock().ok().and_then(|g| g.clone()).filter(|k| !k.is_empty())
}

static TMDB_TOKEN_RUNTIME: OnceLock<Mutex<Option<String>>> = OnceLock::new();

pub fn set_tmdb_token(key: Option<String>) {
    let cell = TMDB_TOKEN_RUNTIME.get_or_init(|| Mutex::new(None));
    let cleaned = key.and_then(|k| {
        let t = k
            .trim()
            .trim_start_matches("Bearer ")
            .trim_start_matches("bearer ")
            .trim()
            .to_string();
        if t.is_empty() { None } else { Some(t) }
    });
    if let Ok(mut g) = cell.lock() {
        *g = cleaned;
    }
}

pub fn tmdb_configured() -> bool {
    tmdb_token().is_some()
}

fn tmdb_token() -> Option<String> {
    if let Ok(k) = std::env::var("TMDB_API_KEY") {
        let t = k.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let cell = TMDB_TOKEN_RUNTIME.get_or_init(|| Mutex::new(None));
    cell.lock().ok().and_then(|g| g.clone()).filter(|k| !k.is_empty())
}

fn tmdb_lang() -> String {
    let code = pref_lang().unwrap_or_else(|| "fr".into());
    let code = if code.len() >= 2 { &code[..2] } else { "fr" };
    match code {
        "fr" => "fr-FR".into(),
        "en" => "en-US".into(),
        "pt" => "pt-PT".into(),
        "zh" => "zh-CN".into(),
        other => format!("{other}-{}", other.to_ascii_uppercase()),
    }
}

/// French overview, cast and age. Bearer token when the value contains a dot, otherwise v3 `api_key`.
/// Movie age is the theatrical `release_dates` certificate. Series age is `content_ratings`.
/// France first, then nearby countries. An English overview is not kept.
pub async fn tmdb_fiche(name: &str, kind: &str, imdb_id: Option<&str>) -> Option<MetaPatch> {
    if !tmdb_configured() {
        return None;
    }
    let q = parse_title_query(name);
    let series = kind == "series";
    let from_imdb = if let Some(tt) = imdb_id.filter(|s| s.starts_with("tt")) {
        tmdb_find_id(tt, series).await
    } else {
        None
    };
    let id = match from_imdb {
        Some(id) => id,
        None => tmdb_search_id(&q, series).await?,
    };
    let lang = tmdb_lang();
    let path = if series {
        format!("/tv/{id}?language={lang}&append_to_response=credits,content_ratings,external_ids")
    } else {
        format!("/movie/{id}?language={lang}&append_to_response=credits,release_dates")
    };
    let body = tmdb_json(&path).await?;
    if from_imdb.is_none() {
        let labels = [
            body.get("title").and_then(|v| v.as_str()),
            body.get("name").and_then(|v| v.as_str()),
            body.get("original_title").and_then(|v| v.as_str()),
            body.get("original_name").and_then(|v| v.as_str()),
        ];
        let matched = labels.iter().flatten().any(|label| {
            titles_same(label, &q.title) || title_overlap(&q.title, label)
        });
        if !matched {
            return None;
        }
    }
    let (actors, director, writer) = tmdb_people(&body);
    let patch = MetaPatch {
        year: tmdb_year(&body).or_else(|| q.year.clone()),
        genre: tmdb_joined(&body, "genres", "name"),
        plot: tmdb_overview(&body),
        poster: tmdb_poster(&body),
        rating: tmdb_vote(&body),
        imdb_id: tmdb_imdb(&body).or_else(|| imdb_id.filter(|s| s.starts_with("tt")).map(str::to_string)),
        tmdb_id: u32::try_from(id).ok(),
        actors,
        director,
        writer,
        runtime: tmdb_runtime(&body),
        rated: if series {
            tmdb_tv_cert(&body)
        } else {
            tmdb_movie_cert(&body)
        }
        .and_then(|code| age_rating(Some(&code)).map(|age| age.label)),
        language: tmdb_spoken(&body),
        country: tmdb_country(&body),
        ..Default::default()
    };
    if patch.is_empty() { None } else { Some(patch) }
}

/// Public TMDB id resolver for The Open Skip Database lookups.
pub async fn tmdb_id_for_imdb(imdb_id: &str, series: bool) -> Option<u32> {
    let id = tmdb_find_id(imdb_id, series).await?;
    u32::try_from(id).ok()
}

async fn tmdb_json(path: &str) -> Option<serde_json::Value> {
    let token = tmdb_token()?;
    let gate = tmdb_gate();
    if gate.blocked() {
        return None;
    }
    gate.wait_turn().await;
    let url = format!("https://api.themoviedb.org/3{path}");
    let req = http().get(&url).header("Accept", "application/json");
    let req = if token.contains('.') {
        req.bearer_auth(&token)
    } else {
        req.query(&[("api_key", token.as_str())])
    };
    let resp = req.send().await.ok()?;
    let status = resp.status();
    if status.as_u16() == 429 {
        gate.trip_429(30);
        return None;
    }
    if !status.is_success() {
        debug!(status = %status, "tmdb");
        return None;
    }
    resp.json().await.ok()
}

async fn tmdb_find_id(tt: &str, series: bool) -> Option<i64> {
    let path = format!("/find/{tt}?external_source=imdb_id&language={}", tmdb_lang());
    let body = tmdb_json(&path).await?;
    let primary = if series { "tv_results" } else { "movie_results" };
    let secondary = if series { "movie_results" } else { "tv_results" };
    tmdb_first_id(&body, primary).or_else(|| tmdb_first_id(&body, secondary))
}

fn tmdb_first_id(body: &serde_json::Value, key: &str) -> Option<i64> {
    body.get(key)?.as_array()?.first()?.get("id")?.as_i64()
}

async fn tmdb_search_id(q: &TitleQuery, series: bool) -> Option<i64> {
    let lang = tmdb_lang();
    let query = urlencoding_lite(&q.title);
    let year = q
        .year
        .as_deref()
        .filter(|y| y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()));
    let mut path = if series {
        format!("/search/tv?query={query}&language={lang}&include_adult=false")
    } else {
        format!("/search/movie?query={query}&language={lang}&include_adult=false")
    };
    if let Some(y) = year {
        path.push_str(if series { "&first_air_date_year=" } else { "&year=" });
        path.push_str(y);
    }
    if let Some(id) = tmdb_search_pick(&path, q).await {
        return Some(id);
    }
    if year.is_some() {
        let path = if series {
            format!("/search/tv?query={query}&language={lang}&include_adult=false")
        } else {
            format!("/search/movie?query={query}&language={lang}&include_adult=false")
        };
        return tmdb_search_pick(&path, q).await;
    }
    None
}

async fn tmdb_search_pick(path: &str, q: &TitleQuery) -> Option<i64> {
    let body = tmdb_json(path).await?;
    tmdb_pick_id(body.get("results")?.as_array()?, q)
}

fn tmdb_pick_id(results: &[serde_json::Value], q: &TitleQuery) -> Option<i64> {
    let mut best: Option<(i32, i64)> = None;
    for hit in results {
        let labels = [
            hit.get("title").and_then(|v| v.as_str()),
            hit.get("name").and_then(|v| v.as_str()),
            hit.get("original_title").and_then(|v| v.as_str()),
            hit.get("original_name").and_then(|v| v.as_str()),
        ];
        let exact = labels.iter().flatten().any(|label| titles_same(label, &q.title));
        let loose = exact || labels.iter().flatten().any(|label| title_overlap(&q.title, label));
        if !loose {
            continue;
        }
        let hit_year = hit
            .get("release_date")
            .and_then(|v| v.as_str())
            .or_else(|| hit.get("first_air_date").and_then(|v| v.as_str()))
            .and_then(|d| d.get(0..4));
        let year_pen = match (q.year.as_deref(), hit_year) {
            (Some(w), Some(h)) if w == h => 0,
            (Some(w), Some(h)) => {
                let (Ok(wd), Ok(hd)) = (w.parse::<i32>(), h.parse::<i32>()) else {
                    continue;
                };
                if (wd - hd).abs() <= 1 { 1 } else { continue; }
            }
            _ => 2,
        };
        let score = year_pen * 10 + if exact { 0 } else { 3 };
        let Some(id) = hit.get("id").and_then(|v| v.as_i64()) else {
            continue;
        };
        if best.as_ref().map(|(s, _)| score < *s).unwrap_or(true) {
            best = Some((score, id));
        }
    }
    best.map(|(_, id)| id)
}

fn tmdb_overview(body: &serde_json::Value) -> Option<String> {
    let text = body.get("overview")?.as_str()?.trim();
    if text.chars().count() < 40 {
        return None;
    }
    if pref_lang().as_deref() != Some("en") && crate::translate::guess_lang(text) == Some("en") {
        return None;
    }
    Some(text.to_string())
}

fn tmdb_people(body: &serde_json::Value) -> (Option<String>, Option<String>, Option<String>) {
    let credits = body.get("credits");
    let actors = credits.and_then(|c| tmdb_names(c.get("cast"), 8, |_| true));
    let director = credits.and_then(|c| {
        tmdb_names(c.get("crew"), 3, |item| {
            item.get("job").and_then(|v| v.as_str()) == Some("Director")
        })
    });
    let mut writer = credits.and_then(|c| {
        tmdb_names(c.get("crew"), 4, |item| {
            matches!(
                item.get("job").and_then(|v| v.as_str()),
                Some("Writer" | "Screenplay" | "Story" | "Teleplay")
            )
        })
    });
    if writer.is_none() {
        writer = tmdb_names(body.get("created_by"), 3, |_| true);
    }
    (actors, director, writer)
}

fn tmdb_person_name(item: &serde_json::Value) -> Option<String> {
    let original = item.get("original_name").and_then(|v| v.as_str()).unwrap_or("").trim();
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
    let pick = if original.chars().any(|c| c.is_ascii_alphabetic()) {
        original
    } else {
        name
    };
    if pick.is_empty() { None } else { Some(pick.to_string()) }
}

fn tmdb_names(
    arr: Option<&serde_json::Value>,
    limit: usize,
    keep: impl Fn(&serde_json::Value) -> bool,
) -> Option<String> {
    let mut out = Vec::new();
    for item in arr?.as_array()? {
        if !keep(item) {
            continue;
        }
        let Some(name) = tmdb_person_name(item) else { continue };
        if out.iter().any(|n: &String| n.eq_ignore_ascii_case(&name)) {
            continue;
        }
        out.push(name);
        if out.len() == limit {
            break;
        }
    }
    (!out.is_empty()).then(|| out.join(", "))
}

fn tmdb_movie_cert(body: &serde_json::Value) -> Option<String> {
    let mut pairs = Vec::new();
    for country in body.get("release_dates")?.get("results")?.as_array()? {
        let iso = country.get("iso_3166_1")?.as_str()?.to_string();
        let Some(cert) = tmdb_best_release(country.get("release_dates")?.as_array()?) else {
            continue;
        };
        pairs.push((iso, cert));
    }
    prefer_cert(&pairs)
}

fn tmdb_best_release(dates: &[serde_json::Value]) -> Option<String> {
    for ty in [3_i64, 4, 5, 2, 1, 6] {
        for date in dates {
            if date.get("type").and_then(|v| v.as_i64()) != Some(ty) {
                continue;
            }
            if let Some(cert) = date.get("certification").and_then(|v| v.as_str()).and_then(usable_cert) {
                return Some(cert);
            }
        }
    }
    None
}

fn tmdb_tv_cert(body: &serde_json::Value) -> Option<String> {
    let mut pairs = Vec::new();
    for row in body.get("content_ratings")?.get("results")?.as_array()? {
        let iso = row.get("iso_3166_1")?.as_str()?.to_string();
        let Some(cert) = row.get("rating").and_then(|v| v.as_str()).and_then(usable_cert) else {
            continue;
        };
        pairs.push((iso, cert));
    }
    prefer_cert(&pairs)
}

fn prefer_cert(pairs: &[(String, String)]) -> Option<String> {
    const ORDER: &[&str] = &["FR", "BE", "CH", "DE", "CA", "GB", "US"];
    for iso in ORDER {
        if let Some((_, cert)) = pairs.iter().find(|(code, _)| code.eq_ignore_ascii_case(iso)) {
            return Some(cert.clone());
        }
    }
    pairs.first().map(|(_, cert)| cert.clone())
}

fn usable_cert(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let age = age_rating(Some(raw))?;
    age_rank(&age.label)?;
    Some(age.code)
}

fn tmdb_poster(body: &serde_json::Value) -> Option<String> {
    let path = body.get("poster_path")?.as_str()?;
    path.starts_with('/').then(|| format!("https://image.tmdb.org/t/p/w500{path}"))
}

fn tmdb_year(body: &serde_json::Value) -> Option<String> {
    let date = body
        .get("release_date")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("first_air_date").and_then(|v| v.as_str()))?;
    let year = date.get(0..4)?;
    (year.len() == 4 && year.chars().all(|c| c.is_ascii_digit())).then(|| year.to_string())
}

fn tmdb_vote(body: &serde_json::Value) -> Option<String> {
    let vote = body.get("vote_average")?.as_f64()?;
    (vote > 0.0).then(|| format!("{vote:.1}"))
}

fn tmdb_runtime(body: &serde_json::Value) -> Option<String> {
    if let Some(n) = body.get("runtime").and_then(|v| v.as_i64()).filter(|n| *n > 0) {
        return Some(format!("{n} min"));
    }
    let n = body
        .get("episode_run_time")?
        .as_array()?
        .iter()
        .find_map(|v| v.as_i64())
        .filter(|n| *n > 0)?;
    Some(format!("{n} min"))
}

fn tmdb_joined(body: &serde_json::Value, key: &str, field: &str) -> Option<String> {
    let names: Vec<String> = body
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(|item| {
            item.get(field)?.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
        })
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
}

fn tmdb_country(body: &serde_json::Value) -> Option<String> {
    if let Some(names) = tmdb_joined(body, "production_countries", "name") {
        return Some(names);
    }
    let names: Vec<String> = body
        .get("origin_country")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
}

fn tmdb_spoken(body: &serde_json::Value) -> Option<String> {
    let names: Vec<String> = body
        .get("spoken_languages")?
        .as_array()?
        .iter()
        .filter_map(|item| {
            item.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                .or_else(|| item.get("english_name").and_then(|v| v.as_str()))
                .map(str::to_string)
        })
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
}

fn tmdb_imdb(body: &serde_json::Value) -> Option<String> {
    body.get("imdb_id")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("external_ids").and_then(|e| e.get("imdb_id")).and_then(|v| v.as_str()))
        .filter(|s| s.starts_with("tt"))
        .map(str::to_string)
}

async fn omdb_get(url: &str, label: &str) -> Option<MetaPatch> {
    let gate = omdb_gate();
    if gate.blocked() {
        return None;
    }
    gate.wait_turn().await;
    let resp = http().get(url).send().await.ok()?;
    let status = resp.status();
    if !status.is_success() {
        if status.as_u16() == 429 {
            gate.trip_429(60);
        } else {
            warn!(%label, status = %status, "omdb HTTP");
        }
        return None;
    }
    let body: OmdbResp = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            // The request URL carries the OMDb key.
            warn!(%label, error = %fluxplay_providers::redact_error(&e), "omdb JSON");
            return None;
        }
    };
    if body.response.as_deref() == Some("False") {
        debug!(%label, err = ?body.error, "omdb miss");
        return None;
    }
    let na = |s: Option<String>| s.filter(|v| v != "N/A" && !v.is_empty());
    debug!(
        %label,
        year = ?body.year,
        actors = ?body.actors,
        plot_len = body.plot.as_ref().map(|p| p.len()),
        "omdb/IMDb hit"
    );
    Some(MetaPatch {
        year: na(body.year),
        genre: na(body.genre),
        plot: na(body.plot),
        poster: na(body.poster),
        rating: na(body.imdb_rating),
        imdb_id: na(body.imdb_id),
        tmdb_id: None,
        actors: na(body.actors),
        director: na(body.director),
        writer: na(body.writer),
        runtime: na(body.runtime),
        rated: na(body.rated),
        awards: na(body.awards),
        language: na(body.language),
        country: na(body.country),
    })
}

/// Free iTunes Search — artwork + year when OMDb/TVMaze miss.
async fn itunes_lookup(q: &TitleQuery, series: bool) -> Option<MetaPatch> {
    let gate = itunes_gate();
    if gate.blocked() {
        return None;
    }
    gate.wait_turn().await;
    let entity = if series { "tvSeason" } else { "movie" };
    let mut url = format!(
        "https://itunes.apple.com/search?term={}&entity={}&limit=5",
        urlencoding_lite(&q.title),
        entity
    );
    if let Some(y) = &q.year {
        url.push_str("&year=");
        url.push_str(y);
    }
    let resp = http().get(&url).send().await.ok()?;
    if resp.status().as_u16() == 429 {
        gate.trip_429(45);
        return None;
    }
    if !resp.status().is_success() {
        return None;
    }
    let body: ItunesSearchResp = resp.json().await.ok()?;
    let results = body.results?;
    let want = q.title.to_ascii_lowercase();
    let (best_score, best) = results.into_iter().map(|r| {
        let name = r
            .track_name
            .as_deref()
            .or(r.collection_name.as_deref())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut score = 0i32;
        if name == want {
            score += 100;
        } else if name.contains(&want) || want.contains(&name) {
            score += 40;
        }
        if let (Some(y), Some(ry)) = (q.year.as_deref(), r.release_date.as_deref()) {
            if ry.starts_with(y) {
                score += 30;
            }
        }
        (score, r)
    })
    .max_by_key(|(score, _)| *score)?;
    // A year-only match is some other film: its art and synopsis would be wrong.
    if best_score < 40 {
        debug!(title = %q.title, best_score, "itunes: no title match");
        return None;
    }
    let poster = best.artwork_url_100.as_ref().map(|u| {
        u.replace("100x100bb", "600x600bb")
    });
    let year = best
        .release_date
        .as_deref()
        .and_then(|d| d.get(0..4))
        .map(str::to_string)
        .or_else(|| q.year.clone());
    let plot = best.long_description.or(best.short_description);
    let genre = best
        .primary_genre_name
        .filter(|s| !s.trim().is_empty())
        .map(|s| display_genres(&s).join(", "))
        .filter(|s| !s.is_empty());
    let rated = best
        .content_advisory_rating
        .as_deref()
        .and_then(advisory_code);
    debug!(title = %q.title, ?year, ?genre, ?rated, has_poster = poster.is_some(), "itunes hit");
    Some(MetaPatch {
        year,
        genre,
        plot,
        poster,
        rated,
        ..Default::default()
    })
}

static PREF_LANG: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Viewer language (settings) — localized Wikipedia is tried before English.
pub fn set_pref_lang(lang: Option<String>) {
    let cell = PREF_LANG.get_or_init(|| Mutex::new(None));
    if let Ok(mut g) = cell.lock() {
        *g = lang.map(|l| l.trim().to_ascii_lowercase()).filter(|l| l.len() == 2);
    }
}

fn pref_lang() -> Option<String> {
    PREF_LANG.get_or_init(|| Mutex::new(None)).lock().ok().and_then(|g| g.clone())
}

fn wiki_gate() -> &'static ApiGate {
    static G: OnceLock<ApiGate> = OnceLock::new();
    G.get_or_init(|| ApiGate::new("wikipedia", crate::policy::get().meta.gate_wikipedia_ms))
}

/// Disambiguated page titles per wiki (`Title (2016 film)`, `Titre (série télévisée)`).
fn wiki_candidates(lang: &str, title: &str, year: Option<&str>, series: bool) -> Vec<String> {
    let mut v = Vec::new();
    let y = year.unwrap_or("");
    match (lang, series) {
        ("en", false) => {
            if !y.is_empty() {
                v.push(format!("{title} ({y} film)"));
            }
            v.push(format!("{title} (film)"));
        }
        ("en", true) => {
            if !y.is_empty() {
                v.push(format!("{title} ({y} TV series)"));
            }
            v.push(format!("{title} (TV series)"));
        }
        ("fr", false) => {
            if !y.is_empty() {
                v.push(format!("{title} (film, {y})"));
            }
            v.push(format!("{title} (film)"));
        }
        ("fr", true) => v.push(format!("{title} (série télévisée)")),
        ("de", false) => v.push(format!("{title} (Film)")),
        ("de", true) => v.push(format!("{title} (Fernsehserie)")),
        ("es", false) => v.push(format!("{title} (película)")),
        ("es", true) => v.push(format!("{title} (serie de televisión)")),
        ("it", false) => {
            if !y.is_empty() {
                v.push(format!("{title} (film {y})"));
            }
            v.push(format!("{title} (film)"));
        }
        ("it", true) => v.push(format!("{title} (serie televisiva)")),
        ("pt", false) => v.push(format!("{title} (filme)")),
        ("pt", true) => v.push(format!("{title} (série de televisão)")),
        ("nl", false) => v.push(format!("{title} (film)")),
        ("nl", true) => v.push(format!("{title} (televisieserie)")),
        _ => {}
    }
    v.push(title.to_string());
    v
}

/// Wikidata short description names a film / series (any of the catalog languages).
fn wiki_is_media(description: &str) -> bool {
    const WORDS: &[&str] = &[
        "film", "movie", "películ", "pelicul", "filme", "série", "serie", "sitcom", "anime",
        "animated", "animé", "documentar", "telenovela", "fernseh", "miniseries", "show",
        "dizi", "фильм", "сериал", "فيلم", "مسلسل", "ταινία", "σειρά",
    ];
    let d = description.to_lowercase();
    WORDS.iter().any(|w| d.contains(w))
}

async fn wiki_fetch(lang: &str, page: &str) -> Option<WikiSummary> {
    let gate = wiki_gate();
    if gate.blocked() {
        return None;
    }
    gate.wait_turn().await;
    let url = format!(
        "https://{lang}.wikipedia.org/api/rest_v1/page/summary/{}",
        urlencoding_lite(page).replace('+', "_")
    );
    let resp = http()
        .get(&url)
        .header("Api-User-Agent", "FluxPlay/0.2 (metadata fallback)")
        .send()
        .await
        .ok()?;
    if resp.status().as_u16() == 429 {
        gate.trip_429(60);
        return None;
    }
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

/// Wikipedia REST summary — plot fallback (no key). Viewer-language wiki first,
/// then English; the page must describe a film / series (plain titles like
/// "Soul" otherwise land on the music genre).
async fn wikipedia_summary(q: &TitleQuery, series: bool) -> Option<MetaPatch> {
    let t = q.title.trim();
    if t.chars().count() < 2 {
        return None;
    }
    let mut wikis = Vec::new();
    if let Some(l) = pref_lang().filter(|l| l != "en" && l != "sh") {
        wikis.push(l);
    }
    wikis.push("en".to_string());
    for lang in wikis {
        let pages = wiki_candidates(&lang, t, q.year.as_deref(), series);
        let n = pages.len();
        for (i, page) in pages.into_iter().enumerate() {
            let Some(body) = wiki_fetch(&lang, &page).await else {
                continue;
            };
            if body.typ.as_deref() == Some("disambiguation") {
                continue;
            }
            let plain = i + 1 == n;
            let media = body.description.as_deref().map(wiki_is_media);
            // Disambiguated pages are media by construction; plain titles must prove it.
            if media == Some(false) || (plain && media != Some(true)) {
                continue;
            }
            let Some(plot) = body.extract.filter(|s| s.len() >= 40) else {
                continue;
            };
            let poster = body.thumbnail.and_then(|t| t.source);
            debug!(title = %t, %lang, %page, plot_len = plot.len(), "wikipedia hit");
            return Some(MetaPatch {
                plot: Some(plot),
                poster,
                ..Default::default()
            });
        }
    }
    None
}

fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn strip_html(raw: &str) -> String {
    clean_text(raw)
}

/// Decode HTML entities (`&amp;` `&#39;` `&#x27;` `&nbsp;` `&hellip;` …).
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail[1..].find(';').map(|j| j + 1).filter(|j| *j <= 10);
        let decoded = end.and_then(|j| {
            let ent = &tail[1..j];
            let c = match ent {
                "amp" => '&',
                "quot" => '"',
                "apos" => '\'',
                "lt" => '<',
                "gt" => '>',
                "nbsp" => ' ',
                "hellip" => '…',
                "ndash" => '–',
                "mdash" => '—',
                "rsquo" | "lsquo" => '\'',
                "rdquo" | "ldquo" => '"',
                "laquo" => '«',
                "raquo" => '»',
                "eacute" => 'é',
                "egrave" => 'è',
                "agrave" => 'à',
                "ccedil" => 'ç',
                _ => {
                    let num = ent.strip_prefix('#')?;
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => num.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, j + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Portal / API text → display text: tags removed (`<br>` → newline), entities
/// decoded, blank runs collapsed, paragraph breaks kept.
pub fn clean_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c.is_control() && c != '\n' && c != '\t' {
            continue;
        }
        if c == '<' {
            if let Some(j) = raw[i..].find('>') {
                let tag = raw[i + 1..i + j].trim().to_ascii_lowercase();
                if tag.starts_with("br") || tag.starts_with("/p") || tag == "p" {
                    out.push('\n');
                }
                while let Some((k, _)) = chars.peek() {
                    if *k > i + j {
                        break;
                    }
                    chars.next();
                }
                continue;
            }
        }
        out.push(c);
    }
    let decoded = decode_entities(&out).replace('\r', "");
    let mut lines: Vec<String> = Vec::new();
    for line in decoded.split('\n') {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() {
            if lines.last().map(|p| !p.is_empty()).unwrap_or(false) {
                lines.push(String::new());
            }
        } else {
            lines.push(l);
        }
    }
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.join("\n")
}

fn is_placeholder_value(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "" | "n/a" | "na" | "null" | "none" | "-" | "0" | "unknown" | "not rated" | "no description"
            | "no plot" | "pas de description" | "aucune description"
    )
}

/// Synopsis for display; placeholders (`N/A`, `0`) and title echoes dropped.
pub fn fmt_plot(plot: Option<&str>, title: &str) -> Option<String> {
    let p = clean_text(plot?);
    if is_placeholder_value(&p) || p.chars().count() < 3 {
        return None;
    }
    if p.eq_ignore_ascii_case(title.trim()) {
        return None;
    }
    Some(p)
}

/// `7`, `6.5`, `7.2/10`, `72` (/100) → `7.2`; `0` / garbage → `None`.
pub fn fmt_rating(r: Option<&str>) -> Option<String> {
    let r = r?.trim();
    let head = r.split('/').next().unwrap_or(r).trim().replace(',', ".");
    let mut v: f32 = head.parse().ok()?;
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    if v > 10.0 && v <= 100.0 {
        v /= 10.0;
    }
    if v > 10.0 {
        return None;
    }
    let s = format!("{v:.1}");
    Some(s.strip_suffix(".0").map(str::to_string).unwrap_or(s))
}

/// `02:16:18`, `136 min`, `136`, `PT2H16M` → `2 h 16`; `45 min` stays; zero → `None`.
pub fn fmt_runtime(r: Option<&str>) -> Option<String> {
    let r = r?.trim();
    if is_placeholder_value(r) {
        return None;
    }
    let minutes: u32 = if r.contains(':') {
        let parts: Vec<u32> = r.split(':').filter_map(|p| p.trim().parse().ok()).collect();
        match parts.as_slice() {
            [h, m, _s] => h.saturating_mul(60).saturating_add(*m),
            [a, b] if *a < 10 => a * 60 + b,
            [a, _b] => *a,
            _ => return Some(r.to_string()),
        }
    } else if let Some(iso) = r.strip_prefix("PT") {
        let h = iso.split('H').next().filter(|_| iso.contains('H')).and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
        let m = iso.rsplit('H').next().unwrap_or(iso).trim_end_matches('M').parse::<u32>().unwrap_or(0);
        h.saturating_mul(60).saturating_add(m)
    } else {
        let digits: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
        match digits.parse::<u32>() {
            Ok(n) if r[digits.len()..].trim().is_empty() || r.to_ascii_lowercase().contains("min") => n,
            _ => return Some(r.to_string()),
        }
    };
    match minutes {
        0 => None,
        m if m < 60 => Some(format!("{m} min")),
        m if m % 60 == 0 => Some(format!("{} h", m / 60)),
        m => Some(format!("{} h {:02}", m / 60, m % 60)),
    }
}

/// `2020-01-02` / `02/01/2020` → `2 janv. 2020`. Anything else is kept as written.
pub fn fmt_airdate(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() || is_placeholder_value(raw) {
        return None;
    }
    let head = raw.split(['T', ' ']).next().unwrap_or(raw);
    let parts: Vec<&str> = head.split(['-', '/', '.']).filter(|p| !p.is_empty()).collect();
    if parts.len() == 3 {
        let formatted = if parts[0].len() == 4 {
            parse_ymd(parts[0], parts[1], parts[2])
        } else if parts[2].len() == 4 {
            parse_ymd(parts[2], parts[1], parts[0])
        } else {
            None
        };
        if let Some(label) = formatted {
            return Some(label);
        }
    }
    Some(raw.to_string())
}

fn parse_ymd(year: &str, month: &str, day: &str) -> Option<String> {
    let year: u16 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    let day: u32 = day.parse().ok()?;
    if !(1900..=2100).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    const MONTHS: [&str; 12] = [
        "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
        "déc.",
    ];
    Some(format!("{day} {} {year}", MONTHS[(month - 1) as usize]))
}

/// `2025-03-01` / `2019–2023` / `2024` → `2024`-style year (series keep ranges).
pub fn fmt_year(y: Option<&str>) -> Option<String> {
    let y = y?.trim();
    let digits: String = y.chars().take(4).collect();
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: u16 = digits.parse().ok()?;
    if !(1900..=2100).contains(&n) {
        return None;
    }
    // OMDb series range `2016–2025` / `2016–`
    let rest = &y[4..];
    if let Some(r) = rest.strip_prefix('–').or_else(|| rest.strip_prefix('-')) {
        let end: String = r.chars().take(4).collect();
        if r.is_empty() && !y.contains('-') {
            return Some(format!("{digits}–"));
        }
        if end.len() == 4 && end.chars().all(|c| c.is_ascii_digit()) && r.len() == 4 {
            return Some(format!("{digits}–{end}"));
        }
    }
    Some(digits)
}

/// `Crime / Drama`, `Drama,Crime`, `Drama | Comedy` → `Crime, Drama`; deduped.
pub fn fmt_genre(g: Option<&str>) -> Option<String> {
    fmt_list(g, &[',', '/', '|', ';'])
}

/// Comma lists of people: trimmed, deduped, placeholders removed.
pub fn fmt_people(p: Option<&str>) -> Option<String> {
    fmt_list(p, &[','])
}

fn fmt_list(s: Option<&str>, seps: &[char]) -> Option<String> {
    let s = decode_entities(s?);
    let mut seen: Vec<String> = Vec::new();
    for part in s.split(seps) {
        let t = part.split_whitespace().collect::<Vec<_>>().join(" ");
        if is_placeholder_value(&t) {
            continue;
        }
        if !seen.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
            seen.push(t);
        }
    }
    (!seen.is_empty()).then(|| seen.join(", "))
}

/// Content rating (`PG-13`, `TV-MA`, `16+`); `N/A` / `Not Rated` / `0` dropped.
pub fn fmt_rated(r: Option<&str>) -> Option<String> {
    let r = r?.trim();
    (!is_placeholder_value(r) && !is_unrated(r)).then(|| r.to_string())
}

fn is_unrated(r: &str) -> bool {
    matches!(
        r.trim().to_ascii_lowercase().as_str(),
        "unrated" | "nr" | "not rated" | "non classé" | "non classe" | "unrated "
    )
}

/// Map a raw rating to a recommended age. Unknown codes stay visible as-is.
/// Age written in a title or a genre (`16+`, `PG-13`). Years are ignored.
pub fn age_in_text(raw: &str) -> Option<AgeRating> {
    let lower = raw.to_lowercase();
    const TOKENS: &[&str] = &[
        "tv-ma", "nc-17", "pg-13", "tv-14", "tv-pg", "tv-y7", "tv-g", "18+", "16+", "15+", "14+",
        "13+", "12+", "10+", "7+",
    ];
    for token in TOKENS {
        if lower.contains(token) {
            return age_rating(Some(token));
        }
    }
    None
}

pub fn age_rating(raw: Option<&str>) -> Option<AgeRating> {
    let code = fmt_rated(raw)?;
    let key: String = code
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect();
    let key = key.replace('–', "-");
    let label = match key.as_str() {
        "g" | "u" | "tv-y" | "tvy" | "tv-g" | "tvg" | "touspublics" | "tp" | "approved"
        | "passed" | "al" => "Tous publics",
        "tv-y7" | "tvy7" | "7" | "7+" | "-7" => "7+",
        "pg" | "tv-pg" | "tvpg" | "10" | "10+" | "-10" => "10+",
        "12" | "12+" | "-12" | "12a" => "12+",
        "pg-13" | "pg13" | "13" | "13+" | "-13" | "13teensorolder" => "13+",
        "tv-14" | "tv14" | "14" | "14+" | "-14" | "14a" | "ma14+" => "14+",
        "15" | "15+" | "-15" | "ma15+" | "ma15" => "15+",
        "16" | "16+" | "-16" | "r" | "fsk16" => "16+",
        "nc-17" | "nc17" | "tv-ma" | "tvma" | "18" | "18+" | "-18" | "x" | "18a" | "r18" => "18+",
        _ => {
            if let Some(n) = first_age_number(&code) {
                return Some(AgeRating {
                    label: format!("{n}+"),
                    code,
                });
            }
            return Some(AgeRating {
                label: code.clone(),
                code,
            });
        }
    };
    Some(AgeRating {
        label: label.to_string(),
        code,
    })
}

/// `Tous` is 0, `16+` is 16. Unknown wording stays unknown.
pub fn age_rank(label: &str) -> Option<u8> {
    let n = label.trim().to_lowercase();
    if n.contains("tous") || n == "g" || n == "u" || n == "tp" {
        return Some(0);
    }
    first_age_number(label).and_then(|v| u8::try_from(v).ok())
}

/// Age for a series poster. Kitsu only: one search, no portal, no OMDb.
pub async fn lookup_series_age(name: &str) -> Option<String> {
    let q = parse_title_query(name);
    kitsu_age(&rating_query_title(&q.title)).await
}

/// Age / year for one poster. Portal supplies the genre shelf; optional keys
/// and keyless public APIs fill the rest.
pub struct PosterFacts {
    pub year: Option<String>,
    pub rated: Option<String>,
    pub genre: Option<String>,
}

pub async fn lookup_poster_facts(name: &str, series: bool) -> PosterFacts {
    let q = parse_title_query(name);
    let mut facts = PosterFacts {
        year: None,
        rated: None,
        genre: None,
    };
    if tmdb_configured() {
        if let Some(patch) = tmdb_fiche(name, if series { "series" } else { "movie" }, None).await {
            facts.year = patch.year;
            facts.rated = patch.rated;
        }
    } else if omdb_configured() {
        let kind = if series { "series" } else { "movie" };
        if let Some(patch) = omdb_lookup(&q, kind).await {
            facts.rated = patch.rated.as_deref().and_then(advisory_code);
            if facts.year.is_none() {
                facts.year = patch.year;
            }
        }
    }
    // Keyless: iTunes year + Wikidata certificate (films and series).
    if facts.year.is_none() {
        if let Some(p) = itunes_lookup(&q, series).await {
            facts.year = p.year;
        }
    }
    if facts.year.is_none() {
        facts.year = q.year.clone();
    }
    if facts.rated.is_none() {
        facts.rated = wikidata_rating(&q.title, q.year.as_deref(), series).await;
    }
    // Kitsu answers with prose ("Teens 13 or older") that was shown as the age
    // when TMDB already had a certificate. Only use it without a TMDB token.
    if series && facts.rated.is_none() && !tmdb_configured() {
        facts.rated = kitsu_age(&rating_query_title(&q.title)).await;
    }
    facts
}

/// `Action, Adventure` / `Action et Aventure` → `Action`, `Aventure`.
pub fn display_genres(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in genre_chunks(raw) {
        let label = genre_word_fr(&chunk);
        if label.is_empty() {
            continue;
        }
        if out.iter().any(|e: &String| {
            e.eq_ignore_ascii_case(&label) || label.to_lowercase().contains(&e.to_lowercase())
        }) {
            continue;
        }
        out.push(label);
    }
    out
}

fn genre_chunks(raw: &str) -> Vec<String> {
    let scrubbed: String = raw
        .chars()
        .filter(|c| *c == '\t' || !c.is_control())
        .collect();
    let mut out = Vec::new();
    for piece in scrubbed.split([',', '/', '|', ';', '&']) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        split_genre_link(piece, &mut out);
    }
    out
}

fn split_genre_link(piece: &str, out: &mut Vec<String>) {
    let mut rest = piece;
    loop {
        let lower = rest.to_lowercase();
        let found = [" et ", " and "]
            .iter()
            .find_map(|sep| lower.find(sep).map(|i| (i, sep.len())));
        let Some((i, n)) = found else {
            let t = rest.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
            return;
        };
        if !rest.is_char_boundary(i) || !rest.is_char_boundary(i + n) {
            let t = rest.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
            return;
        }
        let head = rest[..i].trim();
        if !head.is_empty() {
            out.push(head.to_string());
        }
        rest = &rest[i + n..];
    }
}

fn genre_word_fr(raw: &str) -> String {
    let key = fold_genre_key(raw);
    let mapped = match key.as_str() {
        "action" => "Action",
        "adventure" | "aventure" => "Aventure",
        "comedy" | "comedie" => "Comédie",
        "drama" | "drame" => "Drame",
        "dramatique" => "Dramatique",
        "horror" | "horreur" => "Horreur",
        "epouvante" => "Épouvante",
        "thriller" => "Thriller",
        "romance" | "romantic" | "romantique" => "Romance",
        "animation" => "Animation",
        "documentary" | "documentaire" | "documentaires" => "Documentaire",
        "family" | "familial" | "famille" | "familles" => "Familial",
        "fantasy" | "fantastique" => "Fantastique",
        "scifi" | "sciencefiction" | "sf" => "Science-fiction",
        "crime" => "Crime",
        "mystery" | "mystere" => "Mystère",
        "western" => "Western",
        "war" | "guerre" => "Guerre",
        "survie" | "survival" => "Survie",
        "biographique" | "biographical" => "Histoire",
        "policier" => "Policier",
        "history" | "histoire" | "biographie" | "biopic" => "Histoire",
        "music" | "musical" => "Musical",
        "sport" | "sports" => "Sport",
        "kids" | "enfant" | "enfants" | "jeunesse" => "Jeunesse",
        "manga" => "Manga",
        "anime" => "Animé",
        _ => return raw.trim().to_string(),
    };
    mapped.to_string()
}

fn fold_genre_key(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'à' | 'á' | 'â' | 'ä' | 'ã' => 'a',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'î' | 'ï' => 'i',
            'ô' | 'ö' | 'õ' => 'o',
            'ç' => 'c',
            'ñ' => 'n',
            other => other.to_ascii_lowercase(),
        })
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// iTunes `Unrated` / `NR` is not an age.
fn advisory_code(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty()
        || lower == "nr"
        || lower.contains("unrated")
        || lower.contains("not rated")
    {
        return None;
    }
    let age = age_rating(Some(raw))?;
    if age_rank(&age.label).is_none() {
        return None;
    }
    Some(age.code)
}

fn first_age_number(s: &str) -> Option<u32> {
    let digits: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let n: u32 = digits.parse().ok()?;
    (1..=18).contains(&n).then_some(n)
}

#[derive(Debug, Deserialize)]
struct TvMazeShow {
    id: Option<u64>,
    #[serde(default)]
    genres: Vec<String>,
    premiered: Option<String>,
    summary: Option<String>,
    image: Option<TvMazeImage>,
    rating: Option<TvMazeRating>,
    externals: Option<TvMazeExternals>,
}

#[derive(Debug, Deserialize)]
struct TvMazeShowEmbed {
    #[serde(rename = "_embedded")]
    embedded: Option<TvMazeEmbedded>,
}

#[derive(Debug, Deserialize)]
struct TvMazeEmbedded {
    #[serde(default)]
    episodes: Vec<TvMazeEpisode>,
}

#[derive(Debug, Deserialize)]
struct TvMazeEpisode {
    season: Option<u32>,
    number: Option<u32>,
    summary: Option<String>,
    airdate: Option<String>,
    runtime: Option<u32>,
    rating: Option<TvMazeRating>,
    image: Option<TvMazeImage>,
}

#[derive(Debug, Deserialize)]
struct TvMazeImage {
    medium: Option<String>,
    original: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TvMazeRating {
    average: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct TvMazeExternals {
    imdb: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TvMazeCastEntry {
    person: Option<TvMazePerson>,
}

#[derive(Debug, Deserialize)]
struct TvMazePerson {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ItunesSearchResp {
    results: Option<Vec<ItunesResult>>,
}

#[derive(Debug, Deserialize)]
struct ItunesResult {
    #[serde(rename = "trackName")]
    track_name: Option<String>,
    #[serde(rename = "collectionName")]
    collection_name: Option<String>,
    #[serde(rename = "releaseDate")]
    release_date: Option<String>,
    #[serde(rename = "artworkUrl100")]
    artwork_url_100: Option<String>,
    #[serde(rename = "longDescription")]
    long_description: Option<String>,
    #[serde(rename = "shortDescription")]
    short_description: Option<String>,
    #[serde(default, rename = "primaryGenreName")]
    primary_genre_name: Option<String>,
    #[serde(default, rename = "contentAdvisoryRating")]
    content_advisory_rating: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WikiSummary {
    #[serde(rename = "type")]
    typ: Option<String>,
    description: Option<String>,
    extract: Option<String>,
    thumbnail: Option<WikiThumb>,
}

#[derive(Debug, Deserialize)]
struct WikiThumb {
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OmdbResp {
    #[serde(rename = "Response")]
    response: Option<String>,
    #[serde(rename = "Error")]
    error: Option<String>,
    #[serde(rename = "Year")]
    year: Option<String>,
    #[serde(rename = "Genre")]
    genre: Option<String>,
    #[serde(rename = "Plot")]
    plot: Option<String>,
    #[serde(rename = "Poster")]
    poster: Option<String>,
    #[serde(rename = "imdbRating")]
    imdb_rating: Option<String>,
    #[serde(rename = "imdbID")]
    imdb_id: Option<String>,
    #[serde(rename = "Actors")]
    actors: Option<String>,
    #[serde(rename = "Director")]
    director: Option<String>,
    #[serde(rename = "Writer")]
    writer: Option<String>,
    #[serde(rename = "Runtime")]
    runtime: Option<String>,
    #[serde(rename = "Rated")]
    rated: Option<String>,
    #[serde(rename = "Awards")]
    awards: Option<String>,
    #[serde(rename = "Language")]
    language: Option<String>,
    #[serde(rename = "Country")]
    country: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OmdbSearchResp {
    #[serde(rename = "Response")]
    response: Option<String>,
    #[serde(rename = "Search")]
    search: Option<Vec<OmdbSearchItem>>,
}

#[derive(Debug, Deserialize)]
struct OmdbSearchItem {
    #[serde(rename = "Title")]
    title: Option<String>,
    #[serde(rename = "Year")]
    year: Option<String>,
    #[serde(rename = "Poster")]
    poster: Option<String>,
    #[serde(rename = "imdbID")]
    imdb_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imdb_portrait_asks_for_baseline_jpeg() {
        let raw = "https://m.media-amazon.com/images/M/MV5B@@._V1_.jpg";
        assert_eq!(
            imdb_portrait(raw),
            "https://m.media-amazon.com/images/M/MV5B@@._V1_QL75_UX240_.jpg"
        );
    }

    #[test]
    fn strips_vod_lang_prefix_and_year() {
        let q = parse_title_query("EN - Guardians of the Galaxy Vol. 2 - 2017-sub");
        assert_eq!(q.title, "Guardians of the Galaxy Vol 2");
        assert_eq!(q.year.as_deref(), Some("2017"));
    }

    #[test]
    fn strips_az_catalog_prefix_idea_of_you() {
        let q = parse_title_query("AZ - The Idea of You - 2024");
        assert_eq!(q.title, "The Idea of You");
        assert_eq!(q.year.as_deref(), Some("2024"));
        // Must not strip trailing "You" as a fake lang tag.
        let q2 = parse_title_query("The Idea of You");
        assert_eq!(q2.title, "The Idea of You");
    }

    #[test]
    fn strips_in_en_and_paren_year() {
        let q = parse_title_query("IN-EN - Home Alone 2: Lost in New York - 1992");
        assert_eq!(q.title, "Home Alone 2 Lost in New York");
        assert_eq!(q.year.as_deref(), Some("1992"));
    }

    #[test]
    fn strips_series_locale_suffix() {
        let q = parse_title_query("Disclaimer (2024)_sub");
        assert_eq!(q.title, "Disclaimer");
        assert_eq!(q.year.as_deref(), Some("2024"));
        let q2 = parse_title_query("The Purge_fr");
        assert_eq!(q2.title, "The Purge");
        let q3 = parse_title_query("Sonic Boom--it");
        assert_eq!(q3.title, "Sonic Boom");
        let q4 = parse_title_query("Guyane (2017) FR");
        assert_eq!(q4.title, "Guyane");
        assert_eq!(q4.year.as_deref(), Some("2017"));
    }

    #[test]
    fn strips_4kl_and_multi_sub() {
        let q = parse_title_query("FR -4KL American Sniper");
        assert_eq!(q.title, "American Sniper");
        let q2 = parse_title_query("DK - Tusen timmar ] [Multi-Sub] [2022]");
        assert_eq!(q2.title, "Tusen timmar");
        assert_eq!(q2.year.as_deref(), Some("2022"));
    }

    #[test]
    fn cleans_classic_iptv_noise() {
        let q = parse_title_query("The.Matrix.1999.1080p.BluRay.x264");
        assert_eq!(q.title, "The Matrix");
        assert_eq!(q.year.as_deref(), Some("1999"));
    }

    #[test]
    fn numeric_titles_keep_their_digits() {
        let q = parse_title_query("1917");
        assert_eq!(q.title, "1917");
        assert_eq!(q.year, None);
        let q = parse_title_query("Blade Runner 2049 (2017)");
        assert_eq!(q.title, "Blade Runner 2049");
        assert_eq!(q.year.as_deref(), Some("2017"));
        assert_eq!(fmt_runtime(Some("4294967295:00:00")).is_some(), true);
    }

    #[test]
    fn display_formats() {
        assert_eq!(fmt_rating(Some("0")), None);
        assert_eq!(fmt_rating(Some("")), None);
        assert_eq!(fmt_rating(Some("7")).as_deref(), Some("7"));
        assert_eq!(fmt_rating(Some("6.54")).as_deref(), Some("6.5"));
        assert_eq!(fmt_rating(Some("7.2/10")).as_deref(), Some("7.2"));
        assert_eq!(fmt_rating(Some("72")).as_deref(), Some("7.2"));

        assert_eq!(fmt_runtime(Some("02:16:18")).as_deref(), Some("2 h 16"));
        assert_eq!(fmt_runtime(Some("01:40:00")).as_deref(), Some("1 h 40"));
        assert_eq!(fmt_runtime(Some("136 min")).as_deref(), Some("2 h 16"));
        assert_eq!(fmt_runtime(Some("45 min")).as_deref(), Some("45 min"));
        assert_eq!(fmt_runtime(Some("120")).as_deref(), Some("2 h"));
        assert_eq!(fmt_runtime(Some("00:00:00")), None);
        assert_eq!(fmt_runtime(Some("N/A")), None);

        assert_eq!(fmt_airdate(Some("2020-01-02")).as_deref(), Some("2 janv. 2020"));
        assert_eq!(fmt_airdate(Some("02/03/2021")).as_deref(), Some("2 mars 2021"));
        assert_eq!(fmt_airdate(Some("N/A")), None);
        assert_eq!(fmt_year(Some("2025-03-01")).as_deref(), Some("2025"));
        assert_eq!(fmt_year(Some("2016–2025")).as_deref(), Some("2016–2025"));
        assert_eq!(fmt_year(Some("")), None);

        assert_eq!(fmt_genre(Some("Crime / Drama")).as_deref(), Some("Crime, Drama"));
        assert_eq!(fmt_genre(Some("Drama,Drama, Comedy")).as_deref(), Some("Drama, Comedy"));
        assert_eq!(fmt_people(Some("A, B ,, N/A, a")).as_deref(), Some("A, B"));
        assert_eq!(fmt_rated(Some("N/A")), None);
        assert_eq!(age_rating(Some("PG-13")).unwrap().label, "13+");
        assert_eq!(age_rating(Some("TV-MA")).unwrap().label, "18+");
        assert_eq!(age_rating(Some("Tous publics")).unwrap().label, "Tous publics");
        assert_eq!(age_rating(Some("-12")).unwrap().label, "12+");
        assert_eq!(age_rating(Some("Not Rated")), None);
        assert_eq!(
            age_rating(Some("Déconseillé aux moins de 16 ans"))
                .unwrap()
                .label,
            "16+"
        );
    }

    #[test]
    fn cleans_synopsis_text() {
        assert_eq!(
            clean_text("L&#39;histoire&nbsp;d&apos;un <b>homme</b>.<br>Suite &amp; fin&hellip;"),
            "L'histoire d'un homme.\nSuite & fin…"
        );
        assert_eq!(clean_text("Tom &#x26; Jerry"), "Tom & Jerry");
        assert_eq!(clean_text("AT&T rocks"), "AT&T rocks");
        assert_eq!(fmt_plot(Some("N/A"), "X"), None);
        assert_eq!(fmt_plot(Some("  "), "X"), None);
        assert_eq!(fmt_plot(Some("Olga"), "Olga"), None);
    }

    #[test]
    fn plot_merge_keeps_trusted_text() {
        let mut p = MetaPatch {
            plot: Some("A".repeat(120)),
            ..Default::default()
        };
        p.merge(MetaPatch {
            plot: Some("B".repeat(400)),
            ..Default::default()
        });
        assert!(p.plot.as_deref().unwrap().starts_with('A'));

        let mut short = MetaPatch {
            plot: Some("Short blurb.".into()),
            ..Default::default()
        };
        short.merge(MetaPatch {
            plot: Some("B".repeat(200)),
            ..Default::default()
        });
        assert!(short.plot.as_deref().unwrap().starts_with('B'));

        assert!(keeps_plot(Some(&"Portail ".repeat(20)), Some(&"OMDb ".repeat(60))));
        assert!(!keeps_plot(Some("Court."), Some(&"OMDb ".repeat(60))));
        assert!(!keeps_plot(None, Some("x")));
    }

    #[test]
    fn wiki_media_check() {
        assert!(wiki_is_media("2020 American animated film"));
        assert!(wiki_is_media("film d'animation américain"));
        assert!(wiki_is_media("série télévisée américaine"));
        assert!(!wiki_is_media("music genre"));
    }

}
