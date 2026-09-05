//! Shared logging helpers for all FluxPlay crates.
//!
//! Levels (via `tracing`):
//! - **TRACE** — fine-grained flow (`RUST_LOG=…=trace`)
//! - **PROFILER** — timing / CPU / RSS / FPS (`target: "profiler"`, or `FLUXPLAY_PROFILE=1`)
//! - **DEBUG** — operational detail
//! - **INFO** — milestones
//! - **WARN** — recoverable failures
//! - **ERROR** — hard failures

/// EnvFilter default when `RUST_LOG` is unset.
/// All workspace crates at `info`; profiler off until explicitly enabled.
pub const DEFAULT_ENV_FILTER: &str = concat!(
    "fluxplay=info,",
    "fluxplay_core=info,",
    "fluxplay_providers=info,",
    "fluxplay_player=info,",
    "fluxplay_ffi=info,",
    "profiler=off"
);

/// Emit a **profiler** line (TRACE on target `profiler`).
#[macro_export]
macro_rules! profiler {
    ($($tt:tt)*) => {
        ::tracing::trace!(target: "profiler", $($tt)*)
    };
}

/// Enter a named profiler span for the rest of the scope.
#[macro_export]
macro_rules! profile_scope {
    ($name:expr) => {
        let _fluxplay_profile_guard = ::tracing::span!(
            target: "profiler",
            ::tracing::Level::TRACE,
            "profile",
            name = $name
        )
        .entered();
    };
}

/// Instant elapsed helper for ad-hoc profiler logs (wall + CPU + RSS when enabled).
pub struct Stopwatch {
    inner: crate::profiler::ResourceStopwatch,
}

impl Stopwatch {
    pub fn start(name: &'static str) -> Self {
        Self {
            inner: crate::profiler::ResourceStopwatch::start(name),
        }
    }

    pub fn lap(&self, step: &str) {
        self.inner.lap(step);
    }
}
