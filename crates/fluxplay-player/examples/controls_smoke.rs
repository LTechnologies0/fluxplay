//! Smoke-test native transport controls.
//! Optional: `FLUXPLAY_TEST_URL` (defaults to a public HLS test stream).
use fluxplay_core::models::{Channel, ContentKind};
use fluxplay_player::{PlayOptions, PlaybackState, StreamSession};
use std::thread;
use std::time::Duration;

fn main() {
    let url = std::env::var("FLUXPLAY_TEST_URL").unwrap_or_else(|_| {
        "https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8".into()
    });

    let mut opts = PlayOptions::default();
    opts.user_agent = Some("IPTVSmartersPlayer".into());
    let mut session = StreamSession::with_options(opts);

    let ch = Channel {
        id: "smoke-live".into(),
        name: "Smoke Live".into(),
        stream_url: url,
        logo: None,
        group: Some("Demo".into()),
        tvg_id: None,
        tvg_name: None,
        tvg_logo: None,
        epg_channel_id: None,
        scheme: None,
        source_id: None,
        kind: ContentKind::Live,
        catchup: None,
    };

    println!("open…");
    session.open_channel(ch).expect("open_channel");
    assert_eq!(session.state, PlaybackState::Playing);
    println!("ok playing backend={:?}", session.backend);
    thread::sleep(Duration::from_secs(2));

    println!("pause…");
    session.pause();
    assert_eq!(session.state, PlaybackState::Paused);
    thread::sleep(Duration::from_millis(600));

    println!("resume…");
    session.resume();
    assert_eq!(session.state, PlaybackState::Playing);

    println!("volume…");
    session.set_volume(0.4);
    session.volume_delta(0.1);

    println!("mute…");
    session.toggle_mute();
    session.toggle_mute();

    println!("seek (best-effort)…");
    session.seek_relative(10.0);

    println!("restart…");
    session.restart();
    thread::sleep(Duration::from_secs(1));

    println!("stop…");
    session.stop();
    assert_eq!(session.state, PlaybackState::Idle);
    println!("ALL CONTROLS OK");
}
