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
    pub actors: Option<String>,
    pub director: Option<String>,
    pub writer: Option<String>,
    pub runtime: Option<String>,
    pub rated: Option<String>,
    pub awards: Option<String>,
    pub language: Option<String>,
    pub country: Option<String>,
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
    let mut s = raw.trim().to_string();
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
        raw.trim().to_string()
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
        Some(cur) => cur.chars().count() >= SHORT_PLOT || cur.len() >= new.len(),
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
            && self.actors.is_none()
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
        |_| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(14))
                .user_agent("FluxPlay/0.2 (+metadata; public APIs only)")
                .build()
                .expect("metadata HTTP client")
        },
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
    G.get_or_init(|| ApiGate::new("tvmaze", 750))
}

fn omdb_gate() -> &'static ApiGate {
    static G: OnceLock<ApiGate> = OnceLock::new();
    G.get_or_init(|| ApiGate::new("omdb", 280))
}

fn itunes_gate() -> &'static ApiGate {
    static G: OnceLock<ApiGate> = OnceLock::new();
    G.get_or_init(|| ApiGate::new("itunes", 200))
}

pub fn imdb_title_url(imdb_id: &str) -> String {
    let id = imdb_id.trim();
    if id.starts_with("tt") {
        format!("https://www.imdb.com/title/{id}/")
    } else {
        format!("https://www.imdb.com/find/?q={}", urlencoding_lite(id))
    }
}

pub async fn enrich_series(name: &str) -> Option<MetaPatch> {
    enrich_title_full(name, "series", None).await
}

pub async fn enrich_vod(name: &str) -> Option<MetaPatch> {
    enrich_title_full(name, "movie", None).await
}

/// Full IMDb-via-OMDb detail for the media page (synopsis + cast). Prefer `i=tt…`.
///
/// Strategy:
/// 1. OMDb `i=` / `t=` / `s=` (rate-gated)
/// 2. iTunes Search (poster/year)
/// 3. TVMaze (series cast/plot)
/// 4. Wikipedia summary (plot)
pub async fn enrich_title_full(
    name: &str,
    kind: &str,
    imdb_id: Option<&str>,
) -> Option<MetaPatch> {
    let q = parse_title_query(name);
    debug!(%name, title = %q.title, year = ?q.year, %kind, "enrich_title_full");
    let mut patch = MetaPatch::default();

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

    // Untyped title pass (some OMDb rows omit type=movie).
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

    // Series cast fallback when OMDb has no Actors field.
    if kind == "series" && needs_full_credits(patch.actors.as_deref(), patch.plot.as_deref()) {
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

    if patch.year.is_none() {
        patch.year = q.year.clone();
    }
    if patch.is_empty() {
        None
    } else {
        Some(patch)
    }
}

/// Fill episode plot/airdate/still from TVMaze when seasons are loaded.
pub async fn enrich_series_episodes(series_name: &str, item: &mut fluxplay_core::models::SeriesItem) {
    let q = parse_title_query(series_name);
    let Some(eps) = tvmaze_episodes(&q.title).await else {
        return;
    };
    for season in &mut item.seasons {
        for ep in &mut season.episodes {
            if ep.plot.is_some() {
                continue;
            }
            if let Some(src) = eps
                .iter()
                .find(|e| e.season == Some(season.season_number) && e.number == Some(ep.episode_num))
            {
                if let Some(sum) = src.summary.as_ref() {
                    let p = strip_html(sum);
                    if !p.is_empty() {
                        ep.plot = Some(p);
                    }
                }
                ep.airdate = src.airdate.clone();
                ep.runtime = src.runtime.map(|m| format!("{m} min"));
                ep.rating = src
                    .rating
                    .as_ref()
                    .and_then(|r| r.average)
                    .map(|a| format!("{a:.1}"));
                ep.still = src.image.as_ref().and_then(|i| i.medium.clone().or(i.original.clone()));
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
        || plot.map(|s| s.len() < 80).unwrap_or(true)
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
    debug!(title = %q.title, ?year, has_poster = poster.is_some(), "itunes hit");
    Some(MetaPatch {
        year,
        plot,
        poster,
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
    G.get_or_init(|| ApiGate::new("wikipedia", 150))
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
    (!is_placeholder_value(r)).then(|| r.to_string())
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

        assert_eq!(fmt_year(Some("2025-03-01")).as_deref(), Some("2025"));
        assert_eq!(fmt_year(Some("2016–2025")).as_deref(), Some("2016–2025"));
        assert_eq!(fmt_year(Some("")), None);

        assert_eq!(fmt_genre(Some("Crime / Drama")).as_deref(), Some("Crime, Drama"));
        assert_eq!(fmt_genre(Some("Drama,Drama, Comedy")).as_deref(), Some("Drama, Comedy"));
        assert_eq!(fmt_people(Some("A, B ,, N/A, a")).as_deref(), Some("A, B"));
        assert_eq!(fmt_rated(Some("N/A")), None);
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
