//! FluxPlay desktop UI (iced) — also the Android / Android TV shell.

mod app;
mod browser;
mod catalog_db;
mod demo;
mod images;
mod metadata;
mod player_ui;
mod storage;
mod theme;

#[cfg(target_os = "android")]
mod android_intent;

use tracing_subscriber::EnvFilter;

/// Desktop / Linux entry used by the `fluxplay` binary.
pub fn run() -> iced::Result {
    init_tracing();
    tracing::info!("FluxPlay starting (desktop)");
    if fluxplay_core::profiling_enabled() {
        tracing::info!(
            target: "fluxplay::profile",
            overlay = fluxplay_core::overlay_enabled(),
            "interaction profiler ON (wall_ns/ms, cpu%/core, rss, fps)"
        );
    }
    fluxplay_core::profiler!("boot");
    app::run_daemon()
}

/// Android / Android TV entry — iced daemon with NativeActivity event loop.
#[cfg(target_os = "android")]
pub fn run_android(android_app: android_activity::AndroidApp) -> iced::Result {
    init_tracing_android();
    tracing::info!("FluxPlay starting (android iced)");
    fluxplay_core::profiler!("boot");
    app::run_daemon_android(android_app)
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            fluxplay_core::DEFAULT_ENV_FILTER.into()
        }))
        .try_init();
}

#[cfg(target_os = "android")]
fn init_tracing_android() {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("FluxPlay"),
    );
    let _ = tracing_log::LogTracer::init();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            "fluxplay=info,fluxplay_player=info,iced_wgpu=warn".into()
        }))
        .with_writer(std::io::sink)
        .try_init();
}
