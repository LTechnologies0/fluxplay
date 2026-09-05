//! Metadata enrichment — TVMaze (series, no key) + OMDb/IMDb (`OMDB_API_KEY`).
//!
//! OMDb is the public gateway to IMDb title data (no official IMDb API for apps).
//! Endpoints used:
//! - `?t=Title&type=movie|series&plot=full` — lookup by name
//! - `?i=tt…&plot=full` — lookup by IMDb id
//! - `?s=query` — search (available; used when exact title miss)
//!
//! Fields: Plot, Actors, Director, Writer, Runtime, Rated, Genre, Year,
//! Awards, Language, Country, Poster, imdbRating, imdbID.

use std::sync::OnceLock;

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
    let mut cleaned = String::new();
    for w in s.split_whitespace() {
        let t = w.trim_matches(|c: char| matches!(c, '-' | ',' | ';' | '|' | '/'));
        if t.is_empty() {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        if is_noise_token(&lower) {
            continue;
        }
        if year.is_none() {
            if let Some(y) = parse_year_token(t) {
                year = Some(y);
                continue;
            }
        } else if parse_year_token(t).is_some() {
            continue;
        }
        if !cleaned.is_empty() {
            cleaned.push(' ');
        }
        cleaned.push_str(t);
    }

    let title = if cleaned.is_empty() {
        raw.trim().to_string()
    } else {
        cleaned
    };
    TitleQuery { title, year }
}

/// Back-compat: title only.
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
    if i < 2 || i > 3 {
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
            | "BR" | "LAT" | "BIA" | "BAT" | "ONE" | "ALF"
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
        if other.imdb_id.is_some() {
            self.imdb_id = other.imdb_id.clone();
        }
        if other.rating.is_some() && (self.rating.is_none() || self.imdb_id.is_some()) {
            self.rating = other.rating.clone();
        }
        fill!(actors);
        fill!(director);
        fill!(writer);
        fill!(runtime);
        fill!(rated);
        fill!(awards);
        fill!(language);
        fill!(country);
        // Prefer longer / fuller plot (OMDb `plot=full` over short blurbs).
        match (&self.plot, &other.plot) {
            (Some(a), Some(b)) if b.len() > a.len() => self.plot = other.plot,
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
        set!(genre);
        set!(plot);
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
        set!(genre);
        set!(plot);
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

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(14))
            .user_agent("FluxPlay/0.2 (+https://github.com/fluxplay/fluxplay)")
            .pool_max_idle_per_host(4)
            .build()
            .expect("metadata HTTP client")
    })
}

pub fn omdb_configured() -> bool {
    std::env::var("OMDB_API_KEY")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
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
    let q = parse_title_query(name);
    debug!(%name, title = %q.title, year = ?q.year, "enrich_series");
    let mut patch = MetaPatch::default();
    if let Some(p) = tvmaze_show(&q.title).await {
        patch.merge(p);
    }
    if let Some(p) = omdb_lookup(&q, "series").await {
        patch.merge(p);
    } else if patch.imdb_id.is_none() {
        if let Some(p) = omdb_lookup(&q, "").await {
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

pub async fn enrich_vod(name: &str) -> Option<MetaPatch> {
    let q = parse_title_query(name);
    debug!(%name, title = %q.title, year = ?q.year, "enrich_vod");
    let mut patch = MetaPatch::default();
    if let Some(p) = omdb_lookup(&q, "movie").await {
        patch.merge(p);
    } else if let Some(p) = omdb_lookup(&q, "").await {
        patch.merge(p);
    } else if let Some(p) = omdb_search_first(&q, "movie").await {
        if let Some(id) = p.imdb_id.clone() {
            if let Some(full) = omdb_by_id(&id).await {
                patch.merge(full);
            } else {
                patch.merge(p);
            }
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

/// Full IMDb-via-OMDb detail for the media page (synopsis + cast). Prefer `i=tt…`.
pub async fn enrich_title_full(
    name: &str,
    kind: &str,
    imdb_id: Option<&str>,
) -> Option<MetaPatch> {
    let q = parse_title_query(name);
    let mut patch = MetaPatch::default();
    if let Some(id) = imdb_id.filter(|s| s.starts_with("tt")) {
        if let Some(p) = omdb_by_id(id).await {
            patch.merge(p);
        }
    }
    if patch.actors.is_none() || patch.plot.is_none() {
        if let Some(p) = omdb_lookup(&q, kind).await {
            patch.merge(p);
        }
    }
    if patch.imdb_id.is_none() {
        if let Some(p) = omdb_search_first(&q, kind).await {
            if let Some(id) = p.imdb_id.clone() {
                if let Some(full) = omdb_by_id(&id).await {
                    patch.merge(full);
                } else {
                    patch.merge(p);
                }
            }
        }
    }
    if kind == "series" && patch.actors.is_none() {
        if let Some(p) = tvmaze_show(&q.title).await {
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
        || s.rating.is_none()
}

pub fn needs_vod_enrich(v: &VodItem) -> bool {
    v.imdb_id.is_none()
        || v.plot.as_ref().map(|p| p.len() < 40).unwrap_or(true)
        || v.actors.is_none()
        || v.rating.is_none()
}

pub fn needs_full_credits(actors: Option<&str>, plot: Option<&str>) -> bool {
    actors.map(|s| s.is_empty()).unwrap_or(true)
        || plot.map(|s| s.len() < 80).unwrap_or(true)
}

async fn tvmaze_show(name: &str) -> Option<MetaPatch> {
    let url = format!(
        "https://api.tvmaze.com/singlesearch/shows?q={}",
        urlencoding_lite(name)
    );
    let resp = http().get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        if resp.status().as_u16() == 404 {
            trace!(%name, "tvmaze miss");
        } else {
            warn!(%name, status = %resp.status(), "tvmaze HTTP");
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
    // Optional cast embed path via show id
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
    let url = format!("https://api.tvmaze.com/shows/{show_id}/cast");
    let resp = http().get(&url).send().await.ok()?;
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
    let url = format!(
        "https://api.tvmaze.com/singlesearch/shows?q={}&embed=episodes",
        urlencoding_lite(name)
    );
    let resp = http().get(&url).send().await.ok()?;
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

async fn omdb_search_first(q: &TitleQuery, kind: &str) -> Option<MetaPatch> {
    let key = omdb_key()?;
    let mut url = format!(
        "https://www.omdbapi.com/?apikey={}&s={}",
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
    let resp = http().get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: OmdbSearchResp = resp.json().await.ok()?;
    if body.response.as_deref() == Some("False") {
        return None;
    }
    let first = body.search?.into_iter().next()?;
    Some(MetaPatch {
        year: first.year.filter(|y| y != "N/A"),
        poster: first.poster.filter(|p| p != "N/A"),
        imdb_id: first.imdb_id.filter(|id| id != "N/A"),
        ..Default::default()
    })
}

fn omdb_key() -> Option<String> {
    match std::env::var("OMDB_API_KEY") {
        Ok(k) if !k.trim().is_empty() => Some(k),
        _ => {
            trace!("omdb skipped — no OMDB_API_KEY");
            None
        }
    }
}

async fn omdb_get(url: &str, label: &str) -> Option<MetaPatch> {
    let resp = http().get(url).send().await.ok()?;
    if !resp.status().is_success() {
        warn!(%label, status = %resp.status(), "omdb HTTP");
        return None;
    }
    let body: OmdbResp = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!(%label, error = %e, "omdb JSON");
            return None;
        }
    };
    if body.response.as_deref() == Some("False") {
        trace!(%label, "omdb miss");
        return None;
    }
    let na = |s: Option<String>| s.filter(|v| v != "N/A" && !v.is_empty());
    debug!(%label, year = ?body.year, actors = ?body.actors, "omdb/IMDb hit");
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
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
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
struct OmdbResp {
    #[serde(rename = "Response")]
    response: Option<String>,
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
    #[serde(rename = "Year")]
    year: Option<String>,
    #[serde(rename = "Poster")]
    poster: Option<String>,
    #[serde(rename = "imdbID")]
    imdb_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::parse_title_query;

    #[test]
    fn strips_vod_lang_prefix_and_year() {
        let q = parse_title_query("EN - Guardians of the Galaxy Vol. 2 - 2017-sub");
        assert_eq!(q.title, "Guardians of the Galaxy Vol 2");
        assert_eq!(q.year.as_deref(), Some("2017"));
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
}
