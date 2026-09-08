//! Export / import a full IPTV profile as a `.fluxplay` ZIP archive.
//! Contents: manifest.json (MediaSource) + catalog.sqlite3 + images/.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fluxplay_core::models::MediaSource;
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileManifest {
    pub format: u32,
    pub exported_at: String,
    pub source: MediaSource,
}

/// Pack `profiles/{id}/` into a `.fluxplay` zip at `dest`.
pub fn export_profile(source: &MediaSource, dest: &Path) -> Result<(), String> {
    let dir = crate::storage::profile_dir(source.id);
    let catalog = crate::storage::profile_catalog_path(source.id);
    if !catalog.is_file() {
        return Err("Catalogue local introuvable — synchronisez d’abord la source".into());
    }
    // Flush WAL into the main DB file so the zip is complete on all platforms.
    if let Ok(conn) = rusqlite::Connection::open(&catalog) {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
    crate::storage::write_profile_source_json(source);

    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = File::create(dest).map_err(|e| format!("création archive: {e}"))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let manifest = ProfileManifest {
        format: FORMAT_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        source: source.clone(),
    };
    let manifest_raw =
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("manifest: {e}"))?;
    zip.start_file("manifest.json", opts)
        .map_err(|e| format!("zip: {e}"))?;
    zip.write_all(&manifest_raw)
        .map_err(|e| format!("zip write: {e}"))?;

    zip.start_file("catalog.sqlite3", opts)
        .map_err(|e| format!("zip: {e}"))?;
    let mut cat = File::open(&catalog).map_err(|e| format!("lecture catalog: {e}"))?;
    std::io::copy(&mut cat, &mut zip).map_err(|e| format!("copie catalog: {e}"))?;

    // Include favorites that belong to this source (IDs present in catalog channels/vod).
    {
        let state = crate::storage::load();
        let favs: Vec<String> = state.settings.favorites.clone();
        if !favs.is_empty() {
            let raw = serde_json::to_vec_pretty(&favs).unwrap_or_default();
            zip.start_file("favorites.json", opts)
                .map_err(|e| format!("zip: {e}"))?;
            zip.write_all(&raw)
                .map_err(|e| format!("zip write: {e}"))?;
        }
    }

    let images = crate::storage::profile_images_dir(source.id);
    if images.is_dir() {
        for ent in fs::read_dir(&images).map_err(|e| format!("images: {e}"))? {
            let ent = ent.map_err(|e| format!("images: {e}"))?;
            let path = ent.path();
            if !path.is_file() {
                continue;
            }
            let name = ent.file_name();
            let name = name.to_string_lossy();
            zip.start_file(format!("images/{name}"), opts)
                .map_err(|e| format!("zip: {e}"))?;
            let mut f = File::open(&path).map_err(|e| format!("image: {e}"))?;
            std::io::copy(&mut f, &mut zip).map_err(|e| format!("image copy: {e}"))?;
        }
    }

    // Optional source.json mirror
    let sj = dir.join("source.json");
    if sj.is_file() {
        zip.start_file("source.json", opts)
            .map_err(|e| format!("zip: {e}"))?;
        let mut f = File::open(&sj).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut zip).map_err(|e| e.to_string())?;
    }

    zip.finish().map_err(|e| format!("zip finish: {e}"))?;
    info!(path = %dest.display(), %source.id, "profile exported");
    Ok(())
}

/// Unpack a `.fluxplay` archive into a **new** profile UUID. Returns the imported source.
pub fn import_profile(archive: &Path) -> Result<MediaSource, String> {
    let file = File::open(archive).map_err(|e| format!("ouverture: {e}"))?;
    let mut zip = ZipArchive::new(file).map_err(|e| format!("zip invalide: {e}"))?;

    let mut manifest = {
        let mut mf = zip
            .by_name("manifest.json")
            .map_err(|_| "manifest.json manquant".to_string())?;
        let mut raw = String::new();
        mf.read_to_string(&mut raw)
            .map_err(|e| format!("manifest read: {e}"))?;
        serde_json::from_str::<ProfileManifest>(&raw).map_err(|e| format!("manifest JSON: {e}"))?
    };
    if manifest.format > FORMAT_VERSION {
        return Err(format!(
            "Format {} non supporté (max {FORMAT_VERSION})",
            manifest.format
        ));
    }

    let new_id = Uuid::new_v4();
    let old_id = manifest.source.id;
    manifest.source.id = new_id;

    let dest_dir = crate::storage::profile_dir(new_id);
    let _ = fs::create_dir_all(&dest_dir);
    let _ = fs::create_dir_all(crate::storage::profile_images_dir(new_id));

    // Re-open archive for full extract (by_name consumed borrow awkwardly on some zip versions)
    drop(zip);
    let file = File::open(archive).map_err(|e| format!("réouverture: {e}"))?;
    let mut zip = ZipArchive::new(file).map_err(|e| format!("zip: {e}"))?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        let name = entry.name().to_string();
        if name == "manifest.json" || name == "source.json" {
            continue;
        }
        let out_path = if name == "catalog.sqlite3" {
            crate::storage::profile_catalog_path(new_id)
        } else if name == "favorites.json" {
            dest_dir.join("favorites.json")
        } else if let Some(rest) = name.strip_prefix("images/") {
            if rest.is_empty() || rest.contains("..") {
                continue;
            }
            crate::storage::profile_images_dir(new_id).join(rest)
        } else {
            continue;
        };
        if let Some(parent) = out_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut out = File::create(&out_path).map_err(|e| format!("write {name}: {e}"))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("copy {name}: {e}"))?;
    }

    // Rewrite source_id inside SQLite payloads is heavy; rows already tagged with old_id.
    // Remap source_id columns to new_id.
    remap_catalog_source_id(&crate::storage::profile_catalog_path(new_id), old_id, new_id)?;

    crate::storage::write_profile_source_json(&manifest.source);
    info!(%new_id, from = %old_id, "profile imported");
    Ok(manifest.source)
}

fn remap_catalog_source_id(path: &Path, old: Uuid, new: Uuid) -> Result<(), String> {
    let conn = rusqlite::Connection::open(path).map_err(|e| format!("sqlite: {e}"))?;
    let old_s = old.to_string();
    let new_s = new.to_string();
    for table in ["categories", "channels", "vod", "series"] {
        let sql = format!("UPDATE {table} SET source_id = ?1 WHERE source_id = ?2");
        let _ = conn.execute(&sql, rusqlite::params![new_s, old_s]);
    }
    // Patch JSON payloads that embed source_id
    for table in ["channels", "vod", "series"] {
        let sql = format!("SELECT id, payload FROM {table}");
        let updates = {
            let Ok(mut stmt) = conn.prepare(&sql) else {
                continue;
            };
            let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            }) else {
                continue;
            };
            let mut updates = Vec::new();
            for row in rows.flatten() {
                let (id, mut payload) = row;
                if payload.contains(&old_s) {
                    payload = payload.replace(&old_s, &new_s);
                    updates.push((id, payload));
                }
            }
            updates
        };
        let upd = format!("UPDATE {table} SET payload = ?1 WHERE id = ?2");
        for (id, payload) in updates {
            let _ = conn.execute(&upd, rusqlite::params![payload, id]);
        }
    }
    Ok(())
}

/// Default export filename suggestion.
pub fn suggested_export_name(source: &MediaSource) -> String {
    let safe: String = source
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{safe}.fluxplay")
}

pub fn default_export_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}
