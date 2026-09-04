use fluxplay_core::models::{MediaSource, SourceKind};

/// Built-in demo playlist so first launch is not empty.
pub fn demo_source() -> MediaSource {
    let body = r#"#EXTM3U
#EXTINF:-1 tvg-id="demo.news" tvg-logo="" group-title="Demo · News",FluxPlay News
https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8
#EXTINF:-1 tvg-id="demo.sport" group-title="Demo · Sport",FluxPlay Sport HLS
https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_fmp4/master.m3u8
#EXTINF:-1 tvg-id="demo.art" group-title="Demo · Culture",Big Buck Bunny (VOD HLS)
https://test-streams.mux.dev/test_001/stream.m3u8
"#;
    let mut src = MediaSource::new("Démo FluxPlay", SourceKind::M3uPlus, body);
    src.enabled = true;
    src
}
