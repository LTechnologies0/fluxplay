//! FluxPlay desktop UI (iced) — also the Android / Android TV shell.

mod app;
mod async_jobs;
mod browser;
mod catalog_db;
mod demo;
mod display_caps;
mod icons;
mod images;
mod metadata;
mod network;
mod wg_tunnel;
mod player_ui;
mod profile_io;
mod storage;
mod theme;

#[cfg(target_os = "android")]
mod android_intent;
#[cfg(target_os = "android")]
mod android_bridge;

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
    // FLUXPLAY_VERBOSE seeds a richer RUST_LOG when the user did not set one.
    if fluxplay_player::verbose_master() && std::env::var_os("RUST_LOG").is_none() {
        // iced_* at info catches window/resize/wgpu surface issues without drowning in TRACE.
        std::env::set_var(
            "RUST_LOG",
            "fluxplay=debug,fluxplay_core=debug,fluxplay_providers=debug,\
             fluxplay_player=debug,fluxplay::ui=debug,fluxplay::net=debug,\
             iced=info,iced_winit=info,iced_wgpu=warn,iced_runtime=info",
        );
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            fluxplay_core::DEFAULT_ENV_FILTER.into()
        }))
        .try_init();
    fluxplay_player::log_native_verbosity_banner();
}

#[cfg(target_os = "android")]
fn init_tracing_android() {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("FluxPlay"),
    );
    let _ = tracing_log::LogTracer::init();

    // Bridge tracing → android logcat (fmt→sink hid all player/libmpv diagnostics).
    use std::io::Write;
    struct AndroidLogWriter;
    impl Write for AndroidLogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Ok(s) = std::str::from_utf8(buf) {
                for line in s.lines().filter(|l| !l.is_empty()) {
                    log::info!("{line}");
                }
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            "fluxplay=info,fluxplay_player=info,fluxplay_core=warn,iced_wgpu=warn".into()
        }))
        .with_ansi(false)
        .with_writer(|| AndroidLogWriter)
        .try_init();
}
