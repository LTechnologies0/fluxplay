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
    tracing::info!(target: "fluxplay::boot", "FluxPlay starting (android iced)");
    if fluxplay_core::profiling_enabled() {
        tracing::info!(
            target: "fluxplay::profile",
            overlay = fluxplay_core::overlay_enabled(),
            "interaction profiler ON"
        );
    }
    fluxplay_core::profiler!("boot");
    app::run_daemon_android(android_app)
}

fn init_tracing() {
    // Full diagnostic by default (FLUXPLAY_QUIET=1 → quiet). Seeds RUST_LOG + native verbose.
    if fluxplay_core::full_logs_enabled() {
        if std::env::var_os("RUST_LOG").is_none() {
            std::env::set_var("RUST_LOG", fluxplay_core::FULL_ENV_FILTER);
        }
        if std::env::var_os("FLUXPLAY_VERBOSE").is_none() {
            std::env::set_var("FLUXPLAY_VERBOSE", "1");
        }
    } else if fluxplay_player::verbose_master() && std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var(
            "RUST_LOG",
            "fluxplay=debug,fluxplay_core=debug,fluxplay_providers=debug,\
             fluxplay_player=debug,fluxplay::ui=debug,fluxplay::net=debug,\
             iced=info,iced_winit=info,iced_wgpu=warn,iced_runtime=info,profiler=trace",
        );
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            if fluxplay_core::full_logs_enabled() {
                fluxplay_core::FULL_ENV_FILTER.into()
            } else {
                fluxplay_core::DEFAULT_ENV_FILTER.into()
            }
        }))
        .try_init();
    fluxplay_player::log_native_verbosity_banner();
}

#[cfg(target_os = "android")]
fn init_tracing_android() {
    // Full diagnostic on Android unless FLUXPLAY_QUIET=1 (logcat is the only signal).
    let full = fluxplay_core::full_logs_enabled();
    if full {
        if std::env::var_os("RUST_LOG").is_none() {
            std::env::set_var("RUST_LOG", fluxplay_core::FULL_ENV_FILTER);
        }
        if std::env::var_os("FLUXPLAY_VERBOSE").is_none() {
            std::env::set_var("FLUXPLAY_VERBOSE", "1");
        }
        if std::env::var_os("FLUXPLAY_MPV_LOG").is_none() {
            std::env::set_var("FLUXPLAY_MPV_LOG", "v");
        }
        if std::env::var_os("FLUXPLAY_FFMPEG_LOG").is_none() {
            std::env::set_var("FLUXPLAY_FFMPEG_LOG", "verbose");
        }
    }
    let verbose = full
        || fluxplay_player::verbose_master()
        || fluxplay_core::profiling_enabled();

    // Keep android_logger below Trace — naga/wgpu use `log` and would flood logcat.
    let log_max = if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log_max)
            .with_tag("FluxPlay"),
    );
    let _ = tracing_log::LogTracer::init();

    use std::io::Write;
    /// Fmt sink: prefer leading tracing level token, else INFO.
    struct AndroidLogWriter;
    impl Write for AndroidLogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Ok(s) = std::str::from_utf8(buf) {
                for line in s.lines().filter(|l| !l.is_empty()) {
                    let level = {
                        let mut tok = line.split_whitespace();
                        let mut found = None;
                        for _ in 0..4 {
                            let Some(t) = tok.next() else { break };
                            found = match t {
                                "ERROR" => Some(log::Level::Error),
                                "WARN" => Some(log::Level::Warn),
                                "INFO" => Some(log::Level::Info),
                                "DEBUG" => Some(log::Level::Debug),
                                "TRACE" => Some(log::Level::Trace),
                                _ => None,
                            };
                            if found.is_some() {
                                break;
                            }
                        }
                        found.unwrap_or(log::Level::Info)
                    };
                    match level {
                        log::Level::Error => log::error!("{line}"),
                        log::Level::Warn => log::warn!("{line}"),
                        log::Level::Info => log::info!("{line}"),
                        log::Level::Debug => log::debug!("{line}"),
                        log::Level::Trace => log::trace!("{line}"),
                    }
                }
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if full {
            fluxplay_core::FULL_ENV_FILTER.into()
        } else {
            format!(
                "{},fluxplay::ui=info,fluxplay::icons=info,fluxplay::images=info,\
                 fluxplay::android=info,iced_winit=info,iced_wgpu=warn,\
                 wgpu_hal=warn,wgpu=warn,naga=warn,profiler=info",
                fluxplay_core::DEFAULT_ENV_FILTER
            )
            .into()
        }
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true)
        .with_level(true)
        .with_writer(|| AndroidLogWriter)
        .try_init();
    tracing::info!(
        target: "fluxplay::boot",
        filter = %std::env::var("RUST_LOG").unwrap_or_else(|_| "(default)".into()),
        verbose,
        full_logs = full,
        "android logcat bridge ready"
    );
    fluxplay_player::log_native_verbosity_banner();
}
