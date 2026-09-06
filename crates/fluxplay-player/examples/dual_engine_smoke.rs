//! Headless: open local file on FFmpeg and/or libmpv, pull RGBA frames, report counts.
//!
//!   FLUXPLAY_TEST_URL=file:///tmp/fluxplay-fhd60.mp4 \
//!   FLUXPLAY_TEST_BACKEND=ffmpeg|mpv|both \
//!   cargo run -p fluxplay-player --example dual_engine_smoke --release

use fluxplay_core::models::{Channel, ContentKind, PlayerBackendPref};
use fluxplay_player::{PlayOptions, PlaybackState, StreamSession, VideoRect};
use std::env;
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fluxplay_player=info".into()),
        )
        .try_init();

    let url = env::var("FLUXPLAY_TEST_URL").unwrap_or_else(|_| {
        "file:///tmp/fluxplay-fhd60.mp4".into()
    });
    let which = env::var("FLUXPLAY_TEST_BACKEND").unwrap_or_else(|_| "both".into());
    let backends: Vec<PlayerBackendPref> = match which.to_ascii_lowercase().as_str() {
        "ffmpeg" | "ff" => vec![PlayerBackendPref::Ffmpeg],
        "mpv" | "libmpv" => vec![PlayerBackendPref::Mpv],
        _ => vec![PlayerBackendPref::Ffmpeg, PlayerBackendPref::Mpv],
    };

    let mut failed = 0usize;
    for pref in backends {
        if let Err(e) = run_one(&url, pref) {
            eprintln!("FAIL {:?}: {e}", pref);
            failed += 1;
        }
    }
    if failed > 0 {
        std::process::exit(1);
    }
    println!("ALL ENGINES OK");
}

fn run_one(url: &str, pref: PlayerBackendPref) -> Result<(), String> {
    let remote = url.starts_with("http://") || url.starts_with("https://");
    let secs: u64 = env::var("FLUXPLAY_TEST_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if remote { 25 } else { 8 });
    let min_frames: u32 = env::var("FLUXPLAY_TEST_MIN_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if remote { 8 } else { 10 });

    println!("── engine={pref:?} url={url} wait={secs}s min_frames={min_frames}");
    let mut opts = PlayOptions::default();
    opts.preferred = pref;
    opts.hwdec = true;
    // Match real IPTV clients — some CDNs are picky / flaky on UA.
    opts.user_agent = Some(
        env::var("FLUXPLAY_TEST_UA").unwrap_or_else(|_| "IPTVSmartersPlayer".into()),
    );

    let mut session = StreamSession::with_options(opts);
    session.set_video_rect(VideoRect::overlay(0, 0, 1280, 720));

    let ch = Channel {
        id: format!("smoke-{pref:?}"),
        name: format!("Smoke {pref:?}"),
        stream_url: url.to_string(),
        logo: None,
        group: Some("Test".into()),
        tvg_id: None,
        tvg_name: None,
        tvg_logo: None,
        epg_channel_id: None,
        scheme: None,
        source_id: None,
        kind: ContentKind::Vod,
        catchup: None,
    };

    session
        .open_channel(ch)
        .map_err(|e| format!("open_channel: {e}"))?;
    if session.state != PlaybackState::Playing {
        return Err(format!("expected Playing, got {:?}", session.state));
    }
    println!("  backend={:?} embedded={}", session.backend, session.has_embedded_video());

    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut frames = 0u32;
    let mut bright_frames = 0u32;
    let mut last_wh = (0u32, 0u32);
    let mut sought = false;
    while Instant::now() < deadline {
        if !session.native.is_running() {
            return Err("player died mid-run".into());
        }
        // Remote VOD often opens on black titles / logos — jump into the content.
        if remote && !sought && frames >= 1 {
            session.seek_relative(90.0);
            sought = true;
            thread::sleep(Duration::from_millis(800));
        }
        if session.frame_needs_redraw() {
            if let Some((w, h, rgba)) = session.pull_video_frame(1280, 720) {
                if rgba.len() != (w as usize) * (h as usize) * 4 {
                    return Err(format!("bad rgba len {} for {w}x{h}", rgba.len()));
                }
                let sample = rgba.iter().step_by(64).filter(|&&b| b > 8).count();
                if sample > 0 {
                    bright_frames += 1;
                }
                last_wh = (w, h);
                frames += 1;
            }
        }
        thread::sleep(Duration::from_millis(8));
    }

    if frames < min_frames {
        return Err(format!("too few frames pulled: {frames} (last {last_wh:?})"));
    }
    // Local SDR must look alive; remote HDR may still be dark without tonemap —
    // require at least one non-near-black sample after optional seek.
    if bright_frames == 0 {
        return Err(format!(
            "frames look all-black (pulled={frames}, bright=0, sought={sought})"
        ));
    }

    session.pause();
    thread::sleep(Duration::from_millis(200));
    session.resume();
    session.seek_relative(1.0);
    thread::sleep(Duration::from_millis(400));
    session.stop();

    println!("  OK frames={frames} size={last_wh:?}");
    Ok(())
}
