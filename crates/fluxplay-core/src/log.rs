//! Shared logging helpers for all FluxPlay crates.
//!
//! Levels (via `tracing`):
//! - **TRACE** — fine-grained flow (`RUST_LOG=…=trace`)
//! - **PROFILER** — timing / spans (`target: "profiler"`, enable with `profiler=trace`)
//! - **DEBUG** — operational detail
//! - **INFO** — milestones
//! - **WARN** — recoverable failures
//! - **ERROR** — hard failures
//!
//! Default filter (desktop): covers every crate target so modules are visible when raised.

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

/// Instant elapsed helper for ad-hoc profiler logs.
pub struct Stopwatch {
    name: &'static str,
    start: std::time::Instant,
}

impl Stopwatch {
    pub fn start(name: &'static str) -> Self {
        tracing::trace!(target: "profiler", name, "start");
        Self {
            name,
            start: std::time::Instant::now(),
        }
    }

    pub fn lap(&self, step: &str) {
        tracing::trace!(
            target: "profiler",
            name = self.name,
            step,
            elapsed_ms = self.start.elapsed().as_secs_f64() * 1000.0,
            "lap"
        );
    }
}

impl Drop for Stopwatch {
    fn drop(&mut self) {
        tracing::trace!(
            target: "profiler",
            name = self.name,
            elapsed_ms = self.start.elapsed().as_secs_f64() * 1000.0,
            "done"
        );
    }
}
