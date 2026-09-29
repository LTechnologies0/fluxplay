use std::fs;
use std::path::{Path, PathBuf};

use fluxplay_core::models::{AppSettings, MediaSource};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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
        // Fail-closed: never write to cwd/dirs::* on Android (lost after clear-data).
        let dir = PathBuf::from("/data/local/tmp/fluxplay-orphan");
        warn!(path = %dir.display(), "config_dir: ANDROID_APP unset — orphan path");
        return dir;
    }
    #[cfg(not(target_os = "android"))]
    {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("fluxplay");
        debug!(path = %dir.display(), "config_dir");
        dir
    }
}

/// App data root (catalogs / profiles) — desktop: data_dir, Android: internal_data.
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        if let Some(app) = iced::android::ANDROID_APP.get() {
            if let Some(base) = app.internal_data_path() {
                return base.join("fluxplay");
            }
        }
        PathBuf::from("/data/local/tmp/fluxplay-orphan")
    }
    #[cfg(not(target_os = "android"))]
    {
        dirs::data_dir()
            .or_else(dirs::cache_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("fluxplay")
    }
}

/// Where film/episode downloads land: `custom` (settings) or the default.
/// `~/…` expands to the home folder.
pub fn downloads_dir(custom: &str) -> PathBuf {
    let custom = custom.trim();
    if custom.is_empty() {
        return default_downloads_dir();
    }
    if custom == "~" || custom.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(custom.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    PathBuf::from(custom)
}

/// Check a user-typed download folder: absolute, creatable and writable.
/// Returns the normalised path to store (empty input = default).
pub fn validate_downloads_dir(custom: &str) -> Result<String, String> {
    let custom = custom.trim();
    if custom.is_empty() {
        return Ok(String::new());
    }
    let dir = downloads_dir(custom);
    if !dir.is_absolute() {
        return Err(format!("Chemin absolu requis (ex. /home/…/Films) : {custom}"));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("Impossible de créer {} : {e}", dir.display()))?;
    let probe = dir.join(".fluxplay-write-test");
    std::fs::write(&probe, b"")
        .map_err(|e| format!("Dossier non inscriptible {} : {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(dir.to_string_lossy().into_owned())
}

/// `/home/me/x` → `~/x` for labels.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            };
        }
    }
    path.display().to_string()
}

/// Default download folder (visible to the user).
/// Desktop: `~/Downloads/FluxPlay` (XDG). Android has no shared folder here yet —
/// files stay in app storage.
pub fn default_downloads_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        data_dir().join("downloads")
    }
    #[cfg(not(target_os = "android"))]
    {
        dirs::download_dir()
            .or_else(dirs::home_dir)
            .map(|d| d.join("FluxPlay"))
            .unwrap_or_else(|| data_dir().join("downloads"))
    }
}

pub fn profiles_dir() -> PathBuf {
    data_dir().join("profiles")
}

pub fn profile_dir(source_id: Uuid) -> PathBuf {
    profiles_dir().join(source_id.to_string())
}

pub fn profile_catalog_path(source_id: Uuid) -> PathBuf {
    profile_dir(source_id).join("catalog.sqlite3")
}

pub fn profile_images_dir(source_id: Uuid) -> PathBuf {
    profile_dir(source_id).join("images")
}

pub fn profile_source_json_path(source_id: Uuid) -> PathBuf {
    profile_dir(source_id).join("source.json")
}

pub fn legacy_catalog_path() -> PathBuf {
    data_dir().join("catalog.sqlite3")
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
                // Keep the unreadable file: the next save would otherwise erase the
                // sources and favorites for good.
                let backup = path.with_extension(format!(
                    "json.corrupt-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ));
                let _ = fs::copy(&path, &backup);
                warn!(path = %path.display(), backup = %backup.display(), error = %e, "state JSON corrupt — defaults");
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

/// Replace `path` atomically (a crash leaves the old or the new file, never half of one),
/// readable by the owner only: these files hold portal credentials.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn save(state: &PersistedState) {
    let dir = config_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        error!(path = %dir.display(), error = %e, "config dir create failed");
        return;
    }
    match serde_json::to_string_pretty(state) {
        Ok(raw) => {
            if let Err(e) = write_private(&state_path(), raw.as_bytes()) {
                error!(error = %e, "state write failed");
            } else {
                debug!(sources = state.sources.len(), "state saved");
            }
        }
        Err(e) => error!(error = %e, "state serialize failed"),
    }
}

pub fn write_profile_source_json(source: &MediaSource) {
    let dir = profile_dir(source.id);
    let _ = fs::create_dir_all(&dir);
    if let Ok(raw) = serde_json::to_string_pretty(source) {
        let _ = write_private(&profile_source_json_path(source.id), raw.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_dir_default_tilde_and_absolute() {
        assert_eq!(downloads_dir("  "), default_downloads_dir());
        if let Some(home) = dirs::home_dir() {
            assert_eq!(downloads_dir("~/Films"), home.join("Films"));
            assert_eq!(display_path(&home.join("Films")), "~/Films");
        }
        assert_eq!(downloads_dir("/srv/media"), PathBuf::from("/srv/media"));
    }

    #[test]
    fn validate_downloads_dir_rejects_relative_and_creates_absolute() {
        assert_eq!(validate_downloads_dir(""), Ok(String::new()));
        assert!(validate_downloads_dir("films").is_err());
        let dir = std::env::temp_dir().join(format!("fluxplay-dl-{}", std::process::id()));
        let stored = validate_downloads_dir(dir.to_str().unwrap()).unwrap();
        assert_eq!(PathBuf::from(&stored), dir);
        assert!(dir.is_dir());
        assert!(!dir.join(".fluxplay-write-test").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
