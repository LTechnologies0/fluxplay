#![no_main]
//! Persisted state.json + TOSD JSON bodies — no disk / no network.
use libfuzzer_sys::fuzz_target;

use fluxplay::fuzz_api::{parse_movie_json, parse_persisted_state, parse_series_json};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    if s.len() > 256 * 1024 {
        return;
    }
    let _ = parse_persisted_state(s);
    let _ = parse_series_json(s);
    let _ = parse_movie_json(s);
});
