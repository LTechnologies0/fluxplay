//! Requires env: `FLUXPLAY_XTREAM_URL`, `FLUXPLAY_XTREAM_USER`, `FLUXPLAY_XTREAM_PASS`.
use fluxplay_core::models::{MediaSource, SourceKind};
use fluxplay_providers::{fetch_short_epg, load_source_with_epg};

#[tokio::main]
async fn main() {
    let endpoint = std::env::var("FLUXPLAY_XTREAM_URL").expect("FLUXPLAY_XTREAM_URL");
    let user = std::env::var("FLUXPLAY_XTREAM_USER").expect("FLUXPLAY_XTREAM_USER");
    let pass = std::env::var("FLUXPLAY_XTREAM_PASS").expect("FLUXPLAY_XTREAM_PASS");
    let mut src = MediaSource::new("xtream-env", SourceKind::Xtream, endpoint);
    src.username = Some(user);
    src.password = Some(pass);

    let b = load_source_with_epg(&src).await.expect("load");
    let ids: Vec<_> = b.channels.iter().take(12).map(|c| c.id.clone()).collect();
    let epg = fetch_short_epg(&src, &ids).await;
    println!("short EPG programmes={} for {} streams", epg.len(), ids.len());
}
