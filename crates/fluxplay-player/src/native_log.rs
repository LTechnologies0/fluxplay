//! Native backend verbosity (libmpv / libav* / CLI).
//!
//! Controlled by env (checked at player open):
//! - `FLUXPLAY_VERBOSE=1` — master switch (mpv + ffmpeg verbose)
//! - `FLUXPLAY_MPV_LOG=v|debug|trace|no` — override mpv `msg-level`
//! - `FLUXPLAY_FFMPEG_LOG=quiet|error|warning|info|verbose|debug|trace` — `av_log` level
//!
//! Rust/iced side: set `RUST_LOG` (or let `FLUXPLAY_VERBOSE` seed a default in the UI crate).

use std::path::PathBuf;

use tracing::info;

/// True when master verbose switch is on.
pub fn verbose_master() -> bool {
    matches!(
        std::env::var("FLUXPLAY_VERBOSE").ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// mpv `msg-level` token (`all=v`, `all=debug`, …). `None` = keep quiet defaults.
pub fn mpv_msg_level() -> Option<&'static str> {
    match std::env::var("FLUXPLAY_MPV_LOG")
        .ok()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("no") | Some("off") | Some("quiet") | Some("0") => None,
        Some("status") | Some("info") => Some("all=status"),
        Some("v") | Some("verbose") => Some("all=v"),
        Some("debug") | Some("d") => Some("all=debug"),
        Some("trace") | Some("t") => Some("all=trace"),
        Some(_) => Some("all=v"),
        None if verbose_master() => Some("all=v"),
        None => None,
    }
}

/// Path for mpv verbose log file when verbosity is on.
pub fn mpv_verbose_log_path() -> PathBuf {
    std::env::temp_dir().join("fluxplay-mpv-verbose.log")
}

/// FFmpeg `AV_LOG_*` level integer, or `None` to leave library default.
pub fn ffmpeg_av_log_level() -> Option<i32> {
    // Mirror libavutil/log.h values.
    const AV_LOG_QUIET: i32 = -8;
    const AV_LOG_ERROR: i32 = 16;
    const AV_LOG_WARNING: i32 = 24;
    const AV_LOG_INFO: i32 = 32;
    const AV_LOG_VERBOSE: i32 = 40;
    const AV_LOG_DEBUG: i32 = 48;
    const AV_LOG_TRACE: i32 = 56;

    match std::env::var("FLUXPLAY_FFMPEG_LOG")
        .ok()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("quiet") | Some("off") | Some("0") => Some(AV_LOG_QUIET),
        Some("error") | Some("err") => Some(AV_LOG_ERROR),
        Some("warning") | Some("warn") => Some(AV_LOG_WARNING),
        Some("info") => Some(AV_LOG_INFO),
        Some("verbose") | Some("v") => Some(AV_LOG_VERBOSE),
        Some("debug") | Some("d") => Some(AV_LOG_DEBUG),
        Some("trace") | Some("t") => Some(AV_LOG_TRACE),
        Some(_) => Some(AV_LOG_VERBOSE),
        None if verbose_master() => Some(AV_LOG_VERBOSE),
        None => None,
    }
}

/// Log once what native verbosity will apply (call from UI boot).
pub fn log_native_verbosity_banner() {
    let master = verbose_master();
    let mpv = mpv_msg_level();
    let ff = ffmpeg_av_log_level();
    if master || mpv.is_some() || ff.is_some() {
        info!(
            target: "fluxplay_player::native_log",
            FLUXPLAY_VERBOSE = master,
            mpv_msg_level = ?mpv,
            ffmpeg_av_log = ?ff,
            mpv_log_file = %mpv_verbose_log_path().display(),
            "native verbose logging armed"
        );
    }
}
