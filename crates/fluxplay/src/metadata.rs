//! Free metadata enrichment — TVMaze (no key) + optional OMDb (`OMDB_API_KEY`).

use std::sync::OnceLock;

use fluxplay_core::models::{SeriesItem, VodItem};
use serde::Deserialize;
use tracing::{debug, trace, warn};

#[derive(Debug, Clone, Default)]
pub struct MetaPatch {
    pub year: Option<String>,
    pub genre: Option<String>,
    pub plot: Option<String>,
    pub poster: Option<String>,
    pub rating: Option<String>,
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(12))
            .user_agent("FluxPlay/0.2")
            .pool_max_idle_per_host(4)
            .build()
            .expect("metadata HTTP client")
    })
}

pub async fn enrich_series(name: &str) -> Option<MetaPatch> {
    debug!(%name, "enrich_series");
    if let Some(p) = tvmaze_show(name).await {
        return Some(p);
    }
    omdb_lookup(name, "series").await
}

pub async fn enrich_vod(name: &str) -> Option<MetaPatch> {
    debug!(%name, "enrich_vod");
    omdb_lookup(name, "movie").await
}

pub fn needs_series_enrich(s: &SeriesItem) -> bool {
    s.genre.is_none() || s.year.is_none() || s.plot.is_none()
}

pub fn needs_vod_enrich(v: &VodItem) -> bool {
    v.genre.is_none() || v.year.is_none()
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
    debug!(%name, ?year, ?genre, "tvmaze hit");
    Some(MetaPatch {
        year,
        genre,
        plot,
        poster,
        rating,
    })
}

async fn omdb_lookup(name: &str, kind: &str) -> Option<MetaPatch> {
    let key = match std::env::var("OMDB_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            trace!(%name, %kind, "omdb skipped — no OMDB_API_KEY");
            return None;
        }
    };
    let url = format!(
        "https://www.omdbapi.com/?apikey={key}&t={}&type={kind}",
        urlencoding_lite(name)
    );
    let resp = http().get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        warn!(%name, %kind, status = %resp.status(), "omdb HTTP");
        return None;
    }
    let body: OmdbResp = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            warn!(%name, error = %e, "omdb JSON");
            return None;
        }
    };
    if body.response.as_deref() == Some("False") {
        trace!(%name, %kind, "omdb miss");
        return None;
    }
    debug!(%name, %kind, year = ?body.year, "omdb hit");
    Some(MetaPatch {
        year: body.year.filter(|y| y != "N/A"),
        genre: body.genre.filter(|g| g != "N/A"),
        plot: body.plot.filter(|p| p != "N/A"),
        poster: body.poster.filter(|p| p != "N/A"),
        rating: body.imdb_rating.filter(|r| r != "N/A"),
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
    #[serde(default)]
    genres: Vec<String>,
    premiered: Option<String>,
    summary: Option<String>,
    image: Option<TvMazeImage>,
    rating: Option<TvMazeRating>,
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
}
