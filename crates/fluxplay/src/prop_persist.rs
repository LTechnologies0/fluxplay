//! Property tests for persistence + TOSD JSON (no disk / no network).
#![cfg(test)]

use proptest::prelude::*;

use crate::skip_db::{parse_movie_json, parse_series_json};
use crate::storage::{parse_persisted_state, PersistedState};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn persisted_state_parse_total(raw in "\\PC{0,2000}") {
        let _ = parse_persisted_state(&raw);
    }

    #[test]
    fn tosd_series_parse_total(raw in "\\PC{0,800}") {
        let _ = parse_series_json(&raw);
    }

    #[test]
    fn tosd_movie_parse_total(raw in "\\PC{0,800}") {
        let _ = parse_movie_json(&raw);
    }

    #[test]
    fn default_state_roundtrip(_unit in Just(())) {
        let s = PersistedState::default();
        let j = serde_json::to_string(&s).expect("ser");
        let back: PersistedState = serde_json::from_str(&j).expect("de");
        prop_assert_eq!(back.sources.len(), 0);
    }
}

#[test]
fn tosd_series_valid() {
    let hit = parse_series_json(r#"{"intro_start":10.0,"intro_end":70.0,"outro_start":1200.0}"#)
        .expect("hit");
    assert_eq!(hit.intro, Some((10.0, 70.0)));
    assert_eq!(hit.outro_start, Some(1200.0));
}

#[test]
fn tosd_rejects_absurd_intro() {
    assert!(parse_series_json(r#"{"intro_start":0.0,"intro_end":400.0}"#).is_none());
}

#[test]
fn empty_json_object_is_default_state() {
    let s = parse_persisted_state("{}").expect("empty object");
    assert!(s.sources.is_empty());
}
