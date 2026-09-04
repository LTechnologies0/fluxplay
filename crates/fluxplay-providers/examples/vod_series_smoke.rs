//! Requires env: `FLUXPLAY_XTREAM_URL`, `FLUXPLAY_XTREAM_USER`, `FLUXPLAY_XTREAM_PASS`.
use fluxplay_core::models::{ContentKind, MediaSource, SourceKind};
use fluxplay_providers::{load_xtream_series_category, load_xtream_vod_category};

#[tokio::main]
async fn main() {
    let endpoint = std::env::var("FLUXPLAY_XTREAM_URL").expect("FLUXPLAY_XTREAM_URL");
    let user = std::env::var("FLUXPLAY_XTREAM_USER").expect("FLUXPLAY_XTREAM_USER");
    let pass = std::env::var("FLUXPLAY_XTREAM_PASS").expect("FLUXPLAY_XTREAM_PASS");
    let mut src = MediaSource::new("xtream-env", SourceKind::Xtream, endpoint);
    src.username = Some(user);
    src.password = Some(pass);

    let vod_cat = std::env::var("FLUXPLAY_VOD_CATEGORY").unwrap_or_default();
    let series_cat = std::env::var("FLUXPLAY_SERIES_CATEGORY").unwrap_or_default();
    if vod_cat.is_empty() || series_cat.is_empty() {
        eprintln!("Set FLUXPLAY_VOD_CATEGORY and FLUXPLAY_SERIES_CATEGORY");
        std::process::exit(2);
    }

    let vod = load_xtream_vod_category(&src, &vod_cat).await.expect("vod");
    let series = load_xtream_series_category(&src, &series_cat)
        .await
        .expect("series");
    println!(
        "vod={} series={} ({:?}/{:?})",
        vod.len(),
        series.len(),
        ContentKind::Vod,
        ContentKind::Series
    );
}
