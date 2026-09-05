use fluxplay_core::models::{MediaSource, SourceKind};
use tracing::{debug, info};

pub const DEMO_NAME: &str = "Démo FluxPlay";

/// Built-in demo playlist — only used when no real source is registered.
pub fn demo_source() -> MediaSource {
    let body = r#"#EXTM3U
#EXTINF:-1 tvg-id="demo.news" tvg-logo="" group-title="Demo · News",FluxPlay News
https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8
#EXTINF:-1 tvg-id="demo.sport" group-title="Demo · Sport",FluxPlay Sport HLS
https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_fmp4/master.m3u8
#EXTINF:-1 tvg-id="demo.art" group-title="Demo · Culture",Big Buck Bunny (VOD HLS)
https://test-streams.mux.dev/test_001/stream.m3u8
"#;
    let mut src = MediaSource::new(DEMO_NAME, SourceKind::M3uPlus, body);
    src.enabled = true;
    info!("injecting demo source");
    src
}

pub fn is_demo(src: &MediaSource) -> bool {
    src.name == DEMO_NAME
}

/// Drop the built-in demo whenever at least one real source exists.
pub fn strip_demo_if_real(sources: &mut Vec<MediaSource>) {
    let has_real = sources.iter().any(|s| !is_demo(s));
    if has_real {
        let before = sources.len();
        sources.retain(|s| !is_demo(s));
        if sources.len() != before {
            info!(removed = before - sources.len(), "stripped demo source");
        }
    } else {
        debug!("no real sources — keeping demo if present");
    }
}
