//! Requires env: `FLUXPLAY_XTREAM_URL`, `FLUXPLAY_XTREAM_USER`, `FLUXPLAY_XTREAM_PASS`.
use fluxplay_core::models::{MediaSource, SourceKind};
use fluxplay_providers::load_source_with_epg;

#[tokio::main]
async fn main() {
    let endpoint = std::env::var("FLUXPLAY_XTREAM_URL").expect("FLUXPLAY_XTREAM_URL");
    let user = std::env::var("FLUXPLAY_XTREAM_USER").expect("FLUXPLAY_XTREAM_USER");
    let pass = std::env::var("FLUXPLAY_XTREAM_PASS").expect("FLUXPLAY_XTREAM_PASS");
    let mut src = MediaSource::new("xtream-env", SourceKind::Xtream, endpoint);
    src.username = Some(user);
    src.password = Some(pass);

    let b = load_source_with_epg(&src).await.expect("load");
    println!(
        "channels={} vod={} series={} cats={} epg={}",
        b.channels.len(),
        b.vod.len(),
        b.series.len(),
        b.categories.len(),
        b.epg.len()
    );
}
