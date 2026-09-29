//! Several servers for one subscription.
//!
//! Panels often publish mirror hosts that accept the same account and serve
//! the same stream ids. [`rank`] probes them all at once (live Xtream auth,
//! never the API cache) and keeps the answering ones fastest first;
//! [`media_candidates`] rewrites a catalog URL onto each of them so playback
//! can fail over and downloads can spread their connections.
//!
//! A source without mirrors never probes and never rewrites anything.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use fluxplay_core::models::{MediaSource, SourceKind};
use tracing::{info, warn};
use url::Url;
use uuid::Uuid;

use crate::{parse_xtream_get_php, XtreamClient};

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// Re-probe after this long ([`rank_if_stale`]).
const RANKING_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRoute {
    /// As configured (may be a full `get.php` URL).
    pub endpoint: String,
    /// Prefix of media URLs on this server (`http://host:port/`, ends with `/`).
    pub media_base: String,
    /// Auth round-trip; `None` when the kind is not probed (M3U, Stalker).
    pub latency: Option<Duration>,
}

#[derive(Debug, Clone, Default)]
pub struct Ranking {
    /// Reachable servers, fastest first (configured order when not probed).
    pub up: Vec<ServerRoute>,
    /// Servers that failed the probe, configured order.
    pub down: Vec<ServerRoute>,
    /// Every base a catalog URL of this account may start with (media and
    /// API bases of all servers), longest first.
    prefixes: Vec<String>,
    /// `user_info.max_connections` of the account (shared by all servers).
    pub max_connections: Option<u32>,
}

impl Ranking {
    /// Endpoints to try in order: reachable ones first, then the others.
    pub fn endpoint_order(&self) -> Vec<String> {
        self.up
            .iter()
            .chain(&self.down)
            .map(|r| r.endpoint.clone())
            .collect()
    }

    fn add_prefix(&mut self, base: &str) {
        let base = with_slash(base);
        if !self.prefixes.contains(&base) {
            self.prefixes.push(base);
            self.prefixes.sort_by_key(|p| std::cmp::Reverse(p.len()));
        }
    }
}

struct Cached {
    at: Instant,
    ranking: Ranking,
}

fn cache() -> &'static Mutex<HashMap<Uuid, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<Uuid, Cached>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn has_mirrors(source: &MediaSource) -> bool {
    source.endpoints().len() > 1
}

fn is_xtream(source: &MediaSource) -> bool {
    source.kind == SourceKind::Xtream || parse_xtream_get_php(&source.endpoint).is_some()
}

/// Last ranking of `source_id`, fresh or not (never blocks on the network).
pub fn cached(source_id: Uuid) -> Option<Ranking> {
    cache()
        .lock()
        .ok()?
        .get(&source_id)
        .map(|c| c.ranking.clone())
}

/// Drop the ranking (servers edited, profile removed).
pub fn forget(source_id: Uuid) {
    if let Ok(mut c) = cache().lock() {
        c.remove(&source_id);
    }
}

/// `user_info.max_connections` from the last probe.
pub fn max_connections(source_id: Uuid) -> Option<u32> {
    cached(source_id).and_then(|r| r.max_connections)
}

/// Endpoint for API calls: fastest reachable server, else the configured one.
pub fn best_endpoint(source: &MediaSource) -> String {
    if has_mirrors(source) {
        if let Some(r) = cached(source.id).and_then(|r| r.up.into_iter().next()) {
            return r.endpoint;
        }
    }
    source.endpoint.clone()
}

/// [`rank`] unless a ranking younger than [`RANKING_TTL`] exists.
pub async fn rank_if_stale(source: &MediaSource) -> Ranking {
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&source.id).filter(|c| c.at.elapsed() < RANKING_TTL) {
            return hit.ranking.clone();
        }
    }
    rank(source).await
}

/// Probe every server of `source` concurrently and cache the ranking.
pub async fn rank(source: &MediaSource) -> Ranking {
    let ranking = if is_xtream(source) {
        rank_xtream(source).await
    } else {
        static_ranking(source)
    };
    info!(
        source_id = %source.id,
        up = ranking.up.len(),
        down = ranking.down.len(),
        max_connections = ?ranking.max_connections,
        "servers ranked"
    );
    if let Ok(mut c) = cache().lock() {
        c.insert(
            source.id,
            Cached {
                at: Instant::now(),
                ranking: ranking.clone(),
            },
        );
    }
    ranking
}

async fn rank_xtream(source: &MediaSource) -> Ranking {
    let mut set = tokio::task::JoinSet::new();
    for (order, ep) in source.endpoints().into_iter().enumerate() {
        let Some(mut client) = xtream_client(source, ep) else {
            continue;
        };
        let ep = ep.to_string();
        set.spawn(async move {
            let api_base = client.portal.to_string();
            let t0 = Instant::now();
            let probe = client.probe_auth(PROBE_TIMEOUT).await;
            let route = ServerRoute {
                endpoint: ep,
                media_base: with_slash(client.stream_base.as_str()),
                latency: Some(t0.elapsed()),
            };
            (order, route, api_base, probe)
        });
    }
    let mut results = Vec::new();
    while let Some(joined) = set.join_next().await {
        if let Ok(r) = joined {
            results.push(r);
        }
    }
    results.sort_by_key(|(order, ..)| *order);

    let mut ranking = Ranking::default();
    for (_, route, api_base, probe) in results {
        ranking.add_prefix(&api_base);
        ranking.add_prefix(&route.media_base);
        match probe {
            Ok(max) => {
                ranking.max_connections = ranking.max_connections.or(max);
                ranking.up.push(route);
            }
            Err(e) => {
                warn!(server = %api_base, error = %e, "server probe failed");
                ranking.down.push(route);
            }
        }
    }
    ranking.up.sort_by_key(|r| r.latency);
    ranking
}

/// Configured order, no network: M3U / Stalker mirrors, or before a probe.
fn static_ranking(source: &MediaSource) -> Ranking {
    let mut ranking = Ranking::default();
    for ep in source.endpoints() {
        let Some(base) = origin_of(ep) else {
            continue;
        };
        ranking.add_prefix(&base);
        ranking.up.push(ServerRoute {
            endpoint: ep.to_string(),
            media_base: base,
            latency: None,
        });
    }
    ranking
}

/// Xtream client for one server of `source`. A mirror given as a bare host
/// borrows the credentials of the profile (or of its `get.php` URL).
pub fn xtream_client(source: &MediaSource, endpoint: &str) -> Option<XtreamClient> {
    if let Some(creds) = parse_xtream_get_php(endpoint) {
        return XtreamClient::from_credentials(source.id, &creds.base, creds.username, creds.password)
            .ok();
    }
    if source.kind == SourceKind::Xtream {
        let (user, pass) = (source.username.clone()?, source.password.clone()?);
        return XtreamClient::from_credentials(source.id, endpoint, user, pass).ok();
    }
    let creds = parse_xtream_get_php(&source.endpoint)?;
    XtreamClient::from_credentials(source.id, endpoint, creds.username, creds.password).ok()
}

/// `source` pointed at one of its servers (mirrors cleared). A bare-host
/// mirror of a playlist URL keeps the playlist path and query.
pub fn source_for_endpoint(source: &MediaSource, endpoint: &str) -> MediaSource {
    let mut s = source.clone();
    s.mirrors.clear();
    s.endpoint = endpoint.to_string();
    if endpoint != source.endpoint && source.kind != SourceKind::Xtream {
        if let (Ok(primary), Some(mirror)) = (Url::parse(source.endpoint.trim()), parse_loose(endpoint)) {
            let bare = matches!(mirror.path(), "" | "/") && mirror.query().is_none();
            if bare && primary.path().len() > 1 {
                let mut u = primary.clone();
                if u.set_scheme(mirror.scheme()).is_ok()
                    && u.set_host(mirror.host_str()).is_ok()
                    && u.set_port(mirror.port()).is_ok()
                {
                    s.endpoint = u.to_string();
                }
            }
        }
    }
    s
}

/// URLs to try for `url`, best server first, the original included. Only a
/// URL on one of the account's servers is rewritten; anything else (another
/// CDN, a local file) comes back alone.
pub fn media_candidates(source: &MediaSource, url: &str) -> Vec<String> {
    if !has_mirrors(source) {
        return vec![url.to_string()];
    }
    let ranking = cached(source.id).unwrap_or_else(|| static_ranking(source));
    let Some(prefix) = ranking.prefixes.iter().find(|p| url.starts_with(p.as_str())) else {
        return vec![url.to_string()];
    };
    let tail = &url[prefix.len()..];
    let mut out: Vec<String> = Vec::new();
    let mut push = |u: String| {
        if !out.contains(&u) {
            out.push(u);
        }
    };
    for r in &ranking.up {
        push(format!("{}{tail}", r.media_base));
    }
    push(url.to_string());
    for r in &ranking.down {
        push(format!("{}{tail}", r.media_base));
    }
    out
}

/// Move the server serving `url` to the back of the reachable list (it just
/// failed a playback or a download).
pub fn demote(source_id: Uuid, url: &str) {
    let Ok(mut c) = cache().lock() else {
        return;
    };
    let Some(hit) = c.get_mut(&source_id) else {
        return;
    };
    let up = &mut hit.ranking.up;
    if let Some(i) = up.iter().position(|r| url.starts_with(r.media_base.as_str())) {
        let r = up.remove(i);
        up.push(r);
    }
}

fn with_slash(base: &str) -> String {
    let mut s = base.trim().to_string();
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

fn parse_loose(endpoint: &str) -> Option<Url> {
    let e = endpoint.trim();
    if e.contains("://") {
        Url::parse(e).ok()
    } else {
        Url::parse(&format!("http://{e}")).ok()
    }
}

/// `scheme://host[:port]/` of `endpoint`.
fn origin_of(endpoint: &str) -> Option<String> {
    let u = parse_loose(endpoint)?;
    let origin = u.origin();
    origin
        .is_tuple()
        .then(|| with_slash(&origin.ascii_serialization()))
}

#[cfg(test)]
pub(crate) fn install_ranking(source_id: Uuid, ranking: Ranking) {
    cache().lock().unwrap().insert(
        source_id,
        Cached {
            at: Instant::now(),
            ranking,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xtream_source() -> MediaSource {
        let mut s = MediaSource::new("t", SourceKind::Xtream, "http://a.example:8080");
        s.username = Some("u".into());
        s.password = Some("p".into());
        s.mirrors = vec!["b.example".into(), " http://a.example:8080/ ".into()];
        s
    }

    #[test]
    fn endpoints_dedup_and_trim() {
        let s = xtream_source();
        assert_eq!(s.endpoints(), vec!["http://a.example:8080", "b.example"]);
        assert!(has_mirrors(&s));
    }

    #[test]
    fn candidates_rewrite_only_known_servers() {
        let s = xtream_source();
        let mut r = Ranking::default();
        for (ep, base, ms) in [
            ("b.example", "http://b.example/", 20),
            ("http://a.example:8080", "http://a.example:8080/", 90),
        ] {
            r.add_prefix(base);
            r.up.push(ServerRoute {
                endpoint: ep.into(),
                media_base: base.into(),
                latency: Some(Duration::from_millis(ms)),
            });
        }
        install_ranking(s.id, r);
        let url = "http://a.example:8080/movie/u/p/42.mkv";
        assert_eq!(
            media_candidates(&s, url),
            vec!["http://b.example/movie/u/p/42.mkv", url]
        );
        let foreign = "http://cdn.other/x.ts";
        assert_eq!(media_candidates(&s, foreign), vec![foreign]);

        demote(s.id, "http://b.example/movie/u/p/42.mkv");
        assert_eq!(media_candidates(&s, url)[0], url);
        assert_eq!(best_endpoint(&s), "http://a.example:8080");
        forget(s.id);
    }

    #[test]
    fn single_server_is_left_alone() {
        let s = MediaSource::new("t", SourceKind::M3u, "http://a.example/list.m3u");
        assert_eq!(media_candidates(&s, "http://a.example/1.ts"), vec!["http://a.example/1.ts"]);
    }

    #[test]
    fn bare_mirror_keeps_playlist_path() {
        let mut s = MediaSource::new(
            "t",
            SourceKind::M3u,
            "http://a.example:8080/get.php?username=u&password=p&type=m3u_plus",
        );
        s.mirrors = vec!["https://b.example".into()];
        let m = source_for_endpoint(&s, "https://b.example");
        assert_eq!(
            m.endpoint,
            "https://b.example/get.php?username=u&password=p&type=m3u_plus"
        );
        assert!(m.mirrors.is_empty());
        let client = xtream_client(&s, "https://b.example").expect("creds from get.php");
        assert_eq!(client.portal.host_str(), Some("b.example"));
    }

    #[test]
    fn static_ranking_uses_origins() {
        let mut s = MediaSource::new("t", SourceKind::M3u, "http://a.example/list.m3u");
        s.mirrors = vec!["http://b.example:81/list.m3u".into()];
        let r = static_ranking(&s);
        assert_eq!(
            r.up.iter().map(|r| r.media_base.as_str()).collect::<Vec<_>>(),
            vec!["http://a.example/", "http://b.example:81/"]
        );
        assert_eq!(
            media_candidates(&s, "http://a.example/live/1.ts"),
            vec!["http://a.example/live/1.ts", "http://b.example:81/live/1.ts"]
        );
    }
}
