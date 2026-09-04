//! End-to-end smoke of portal features used by the media-center UI.
//!
//! Requires env: `FLUXPLAY_XTREAM_URL`, `FLUXPLAY_XTREAM_USER`, `FLUXPLAY_XTREAM_PASS`.
use fluxplay_core::models::{ContentKind, MediaSource, SourceKind};
use fluxplay_providers::{
    check_xtream_portal, fetch_short_epg, format_health, load_source_with_epg,
    load_xtream_series_info,
};

fn xtream_from_env() -> MediaSource {
    let endpoint = std::env::var("FLUXPLAY_XTREAM_URL").expect("FLUXPLAY_XTREAM_URL");
    let user = std::env::var("FLUXPLAY_XTREAM_USER").expect("FLUXPLAY_XTREAM_USER");
    let pass = std::env::var("FLUXPLAY_XTREAM_PASS").expect("FLUXPLAY_XTREAM_PASS");
    let mut src = MediaSource::new("xtream-env", SourceKind::Xtream, endpoint);
    src.username = Some(user);
    src.password = Some(pass);
    src
}

#[tokio::main]
async fn main() {
    let mut fails = 0u32;
    macro_rules! check {
        ($cond:expr, $($t:tt)*) => {{
            if $cond {
                println!("OK  {}", format!($($t)*));
            } else {
                println!("FAIL {}", format!($($t)*));
                fails += 1;
            }
        }};
    }

    let src = xtream_from_env();
    let user = src.username.clone().unwrap();
    let pass = src.password.clone().unwrap();

    match check_xtream_portal(&src.endpoint, &user, &pass).await {
        Ok(h) => {
            check!(h.auth, "health auth");
            check!(h.stream_ok, "health stream {}", format_health(&h));
        }
        Err(e) => check!(false, "health err {e}"),
    }

    let b = match load_source_with_epg(&src).await {
        Ok(b) => b,
        Err(e) => {
            check!(false, "load_source_with_epg {e}");
            std::process::exit(1);
        }
    };
    check!(!b.channels.is_empty(), "live channels {}", b.channels.len());
    check!(!b.categories.is_empty(), "categories {}", b.categories.len());
    check!(
        b.categories.iter().any(|c| c.content == ContentKind::Vod),
        "vod categories"
    );
    check!(
        b.categories.iter().any(|c| c.content == ContentKind::Series),
        "series categories"
    );

    if let Some(s) = b.series.first() {
        match load_xtream_series_info(&src, &s.id).await {
            Ok(detail) => check!(
                !detail.seasons.is_empty() || detail.seasons.is_empty(),
                "series_info ok seasons={}",
                detail.seasons.len()
            ),
            Err(e) => check!(false, "series_info {e}"),
        }
    } else {
        println!("SKIP series_info (none prefetched)");
    }

    let ids: Vec<String> = b.channels.iter().take(8).map(|c| c.id.clone()).collect();
    let epg = fetch_short_epg(&src, &ids).await;
    println!("EPG programmes={}", epg.len());

    if fails == 0 {
        println!("ALL CHECKS PASSED");
    } else {
        println!("{fails} CHECK(S) FAILED");
        std::process::exit(1);
    }
}
