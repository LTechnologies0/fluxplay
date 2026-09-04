use std::fs;
use std::path::PathBuf;

use fluxplay_core::models::{AppSettings, MediaSource};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    pub settings: AppSettings,
    pub sources: Vec<MediaSource>,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fluxplay")
}

pub fn state_path() -> PathBuf {
    config_dir().join("state.json")
}

pub fn load() -> PersistedState {
    let path = state_path();
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => PersistedState::default(),
    }
}

pub fn save(state: &PersistedState) {
    let dir = config_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(raw) = serde_json::to_string_pretty(state) {
        let _ = fs::write(state_path(), raw);
    }
}
