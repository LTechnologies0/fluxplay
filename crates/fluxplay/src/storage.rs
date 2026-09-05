use std::fs;
use std::path::PathBuf;

use fluxplay_core::models::{AppSettings, MediaSource};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    pub settings: AppSettings,
    pub sources: Vec<MediaSource>,
}

pub fn config_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
            if let Some(app) = iced::android::ANDROID_APP.get() {
                if let Some(base) = app.internal_data_path() {
                    let dir = base.join("fluxplay");
                    debug!(path = %dir.display(), "config_dir (android)");
                    return dir;
                }
            }
    }
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fluxplay");
    debug!(path = %dir.display(), "config_dir");
    dir
}

pub fn state_path() -> PathBuf {
    config_dir().join("state.json")
}

pub fn load() -> PersistedState {
    let path = state_path();
    match fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str(&raw) {
            Ok(state) => {
                info!(path = %path.display(), "state loaded");
                state
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "state JSON corrupt — defaults");
                PersistedState::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!(path = %path.display(), "no state file — defaults");
            PersistedState::default()
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "state read failed — defaults");
            PersistedState::default()
        }
    }
}

pub fn save(state: &PersistedState) {
    let dir = config_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        error!(path = %dir.display(), error = %e, "config dir create failed");
        return;
    }
    match serde_json::to_string_pretty(state) {
        Ok(raw) => {
            if let Err(e) = fs::write(state_path(), raw) {
                error!(error = %e, "state write failed");
            } else {
                debug!(sources = state.sources.len(), "state saved");
            }
        }
        Err(e) => error!(error = %e, "state serialize failed"),
    }
}
