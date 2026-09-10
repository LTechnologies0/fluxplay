//! Local SQLite catalog — offline-first Single Source of Truth.
//! Tuned PRAGMAs (WAL / NORMAL sync / mmap / page cache) + FTS5 search.
//!
//! Public helpers include sync-fallback / tooling APIs not always on the hot path.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fluxplay_core::models::{
    Category, Channel, ContentKind, EpgProgramme, PlaylistBundle, SeriesItem, VodItem,
};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use crate::async_jobs::{JobProgress, DEFAULT_CHUNK};

/// Skip portal re-sync when last full sync is younger than this.
pub const SYNC_FRESH_SECS: u64 = 12 * 3600;

/// Result of applying a portal dump to the local catalog.
#[derive(Debug)]
pub struct BundleApplyResult {
    pub kind: BundleApplyKind,
    pub checksum: String,
    /// Merged catalog slice for this source (enrichment preserved). `None` if unchanged.
    pub merged: Option<PlaylistBundle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleApplyKind {
    /// Portal spine checksum matches DB — no rewrite.
    Unchanged,
    /// Rows upserted / orphans removed; enrichment kept where possible.
    Updated { preserved_meta: usize },
}

pub struct CatalogDb {
    /// One SQLite connection per IPTV profile (`MediaSource.id`).
    conns: HashMap<Uuid, Connection>,
}

impl CatalogDb {
    /// Open profile DBs for the given sources (creates empty DBs as needed).
    /// Runs one-shot migration from legacy global `catalog.sqlite3` first.
    pub fn open(source_ids: &[Uuid]) -> Option<Self> {
        migrate_legacy_catalog(source_ids);
        repair_incomplete_profiles(source_ids);
        let mut db = Self {
            conns: HashMap::new(),
        };
        for &id in source_ids {
            if let Err(e) = db.ensure_source(id) {
                warn!(%id, error = %e, "catalog open source failed");
            }
        }
        // Also open any profile dirs present on disk (imported profiles).
        let profiles = crate::storage::profiles_dir();
        if let Ok(entries) = std::fs::read_dir(&profiles) {
            for ent in entries.flatten() {
                let name = ent.file_name();
                let Some(s) = name.to_str() else { continue };
                let Ok(id) = Uuid::parse_str(s) else { continue };
                if db.conns.contains_key(&id) {
                    continue;
                }
                if let Err(e) = db.ensure_source(id) {
                    warn!(%id, error = %e, "catalog open disk profile failed");
                }
            }
        }
        info!(profiles = db.conns.len(), "catalog db open (per-profile)");
        Some(db)
    }

    /// Ensure a profile catalog exists and is migrated.
    pub fn ensure_source(&mut self, source_id: Uuid) -> rusqlite::Result<()> {
        if self.conns.contains_key(&source_id) {
            return Ok(());
        }
        let path = crate::storage::profile_catalog_path(source_id);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::create_dir_all(crate::storage::profile_images_dir(source_id));
        let conn = Connection::open(&path)?;
        migrate_schema(&conn)?;
        self.conns.insert(source_id, conn);
        debug!(%source_id, path = %path.display(), "profile catalog ready");
        Ok(())
    }

    pub fn source_ids(&self) -> Vec<Uuid> {
        self.conns.keys().copied().collect()
    }

    fn conn(&self, source_id: Uuid) -> rusqlite::Result<&Connection> {
        self.conns.get(&source_id).ok_or(rusqlite::Error::InvalidQuery)
    }

    fn any_conn(&self) -> Option<&Connection> {
        self.conns.values().next()
    }

    fn meta_get_conn(conn: &Connection, key: &str) -> Option<String> {
        conn.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    fn meta_set_conn(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn meta_get_for(&self, source_id: Uuid, key: &str) -> Option<String> {
        self.conn(source_id)
            .ok()
            .and_then(|c| Self::meta_get_conn(c, key))
    }

    pub fn last_full_sync_secs(&self, source_id: Uuid) -> Option<u64> {
        self.meta_get_for(source_id, "last_full_sync")?.parse().ok()
    }

    pub fn mark_full_sync_now(&self, source_id: Uuid) {
        let Ok(conn) = self.conn(source_id) else {
            return;
        };
        let now = now_secs().to_string();
        let _ = Self::meta_set_conn(conn, "last_full_sync", &now);
        let _ = conn.execute_batch("PRAGMA optimize;");
    }

    pub fn mark_all_full_sync_now(&self) {
        for id in self.source_ids() {
            self.mark_full_sync_now(id);
        }
    }

    /// True when every open profile is warm enough to skip portal re-sync.
    pub fn is_sync_fresh(&self) -> bool {
        if self.conns.is_empty() {
            return false;
        }
        let (ch, vod, series) = self.counts();
        if ch + vod + series == 0 {
            debug!("catalog sync not fresh — empty");
            return false;
        }
        for id in self.source_ids() {
            let Some(ts) = self.last_full_sync_secs(id) else {
                debug!(%id, "catalog sync not fresh — no last_full_sync");
                return false;
            };
            if now_secs().saturating_sub(ts) >= SYNC_FRESH_SECS {
                debug!(%id, "catalog sync not fresh — stale");
                return false;
            }
        }
        true
    }

    /// Remove profile DB + on-disk folder.
    pub fn delete_source(&mut self, source_id: Uuid) -> rusqlite::Result<()> {
        self.conns.remove(&source_id);
        let dir = crate::storage::profile_dir(source_id);
        if dir.is_dir() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        debug!(%source_id, "catalog profile deleted");
        Ok(())
    }

    /// Apply a portal dump: skip when checksum matches; otherwise upsert + keep OMDb/meta.
    pub fn apply_source_bundle(
        &mut self,
        source_id: Uuid,
        part: PlaylistBundle,
    ) -> rusqlite::Result<BundleApplyResult> {
        self.apply_source_bundle_progressive(source_id, part, None)
    }

    /// Same as [`apply_source_bundle`] with optional atomic progress (chunked upserts).
    pub fn apply_source_bundle_progressive(
        &mut self,
        source_id: Uuid,
        mut part: PlaylistBundle,
        progress: Option<&JobProgress>,
    ) -> rusqlite::Result<BundleApplyResult> {
        self.ensure_source(source_id)?;
        let checksum = portal_checksum(&part);
        let key = checksum_meta_key(source_id);
        if self.meta_get_for(source_id, &key).as_deref() == Some(checksum.as_str()) {
            info!(
                %source_id,
                checksum = &checksum[..12.min(checksum.len())],
                channels = part.channels.len(),
                vod = part.vod.len(),
                series = part.series.len(),
                "catalog checksum match — skip rewrite"
            );
            return Ok(BundleApplyResult {
                kind: BundleApplyKind::Unchanged,
                checksum,
                merged: None,
            });
        }

        let sid = source_id.to_string();
        let enriched_vod = self.load_enriched_vod_map(source_id, &sid)?;
        let enriched_series = self.load_enriched_series_map(source_id, &sid)?;
        let preserved_meta = enriched_vod.len() + enriched_series.len();

        for v in &mut part.vod {
            v.source_id = Some(source_id);
            if let Some(old) = enriched_vod.get(&v.id) {
                merge_vod_keep_meta(v, old);
            }
        }
        for s in &mut part.series {
            s.source_id = Some(source_id);
            if let Some(old) = enriched_series.get(&s.id) {
                merge_series_keep_meta(s, old);
            }
        }
        for ch in &mut part.channels {
            ch.source_id = Some(source_id);
        }

        let merged = part;
        let total_rows = (merged.categories.len()
            + merged.channels.len()
            + merged.vod.len()
            + merged.series.len()) as u64;
        if let Some(p) = progress {
            p.reset(total_rows.max(1));
        }

        let conn = self.conn(source_id)?;
        let tx = conn.unchecked_transaction()?;

        // Track keep-ids for orphan deletion.
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS _keep_cat(id TEXT, content TEXT, PRIMARY KEY(id, content));
             CREATE TEMP TABLE IF NOT EXISTS _keep_ch(id TEXT PRIMARY KEY);
             CREATE TEMP TABLE IF NOT EXISTS _keep_vod(id TEXT PRIMARY KEY);
             CREATE TEMP TABLE IF NOT EXISTS _keep_ser(id TEXT PRIMARY KEY);
             DELETE FROM _keep_cat;
             DELETE FROM _keep_ch;
             DELETE FROM _keep_vod;
             DELETE FROM _keep_ser;",
        )?;

        {
            let mut keep = tx.prepare("INSERT OR IGNORE INTO _keep_cat(id, content) VALUES(?1,?2)")?;
            let mut stmt = tx.prepare(
                "INSERT INTO categories (source_id, id, name, content) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(source_id, id, content) DO UPDATE SET name=excluded.name",
            )?;
            for chunk in merged.categories.chunks(DEFAULT_CHUNK) {
                for c in chunk {
                    let content = match c.content {
                        ContentKind::Live => "live",
                        ContentKind::Vod => "vod",
                        ContentKind::Series => "series",
                    };
                    keep.execute(params![c.id, content])?;
                    stmt.execute(params![sid, c.id, c.name, content])?;
                }
                if let Some(p) = progress {
                    p.add_done(chunk.len() as u64);
                }
            }
        }
        {
            let mut keep = tx.prepare("INSERT OR IGNORE INTO _keep_ch(id) VALUES(?1)")?;
            let mut stmt = tx.prepare(
                "INSERT INTO channels (source_id, id, payload, name) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(source_id, id) DO UPDATE SET payload=excluded.payload, name=excluded.name",
            )?;
            for chunk in merged.channels.chunks(DEFAULT_CHUNK) {
                for ch in chunk {
                    let payload = serde_json::to_string(ch).unwrap_or_default();
                    keep.execute(params![ch.id])?;
                    stmt.execute(params![sid, ch.id, payload, ch.name])?;
                }
                if let Some(p) = progress {
                    p.add_done(chunk.len() as u64);
                }
            }
        }
        {
            let mut keep = tx.prepare("INSERT OR IGNORE INTO _keep_vod(id) VALUES(?1)")?;
            let mut stmt = tx.prepare(
                "INSERT INTO vod (source_id, id, payload, name, year, genre, category_id, meta_ok)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(source_id, id) DO UPDATE SET
                   payload=excluded.payload,
                   name=excluded.name,
                   year=excluded.year,
                   genre=excluded.genre,
                   category_id=excluded.category_id,
                   meta_ok=excluded.meta_ok",
            )?;
            for chunk in merged.vod.chunks(DEFAULT_CHUNK) {
                for v in chunk {
                    let payload = serde_json::to_string(v).unwrap_or_default();
                    let meta_ok = if vod_meta_ok(v) { 1i64 } else { 0 };
                    keep.execute(params![v.id])?;
                    stmt.execute(params![
                        sid,
                        v.id,
                        payload,
                        v.name,
                        v.year,
                        v.genre,
                        v.category_id,
                        meta_ok
                    ])?;
                }
                if let Some(p) = progress {
                    p.add_done(chunk.len() as u64);
                }
            }
        }
        {
            let mut keep = tx.prepare("INSERT OR IGNORE INTO _keep_ser(id) VALUES(?1)")?;
            let mut stmt = tx.prepare(
                "INSERT INTO series (source_id, id, payload, name, year, genre, category_id, meta_ok)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(source_id, id) DO UPDATE SET
                   payload=excluded.payload,
                   name=excluded.name,
                   year=excluded.year,
                   genre=excluded.genre,
                   category_id=excluded.category_id,
                   meta_ok=excluded.meta_ok",
            )?;
            for chunk in merged.series.chunks(DEFAULT_CHUNK) {
                for s in chunk {
                    let payload = serde_json::to_string(s).unwrap_or_default();
                    let meta_ok = if series_meta_ok(s) { 1i64 } else { 0 };
                    keep.execute(params![s.id])?;
                    stmt.execute(params![
                        sid,
                        s.id,
                        payload,
                        s.name,
                        s.year,
                        s.genre,
                        s.category_id,
                        meta_ok
                    ])?;
                }
                if let Some(p) = progress {
                    p.add_done(chunk.len() as u64);
                }
            }
        }

        tx.execute(
            "DELETE FROM categories WHERE source_id = ?1
             AND NOT EXISTS (
               SELECT 1 FROM _keep_cat k WHERE k.id = categories.id AND k.content = categories.content
             )",
            params![sid],
        )?;
        tx.execute(
            "DELETE FROM channels WHERE source_id = ?1
             AND id NOT IN (SELECT id FROM _keep_ch)",
            params![sid],
        )?;
        tx.execute(
            "DELETE FROM vod WHERE source_id = ?1
             AND id NOT IN (SELECT id FROM _keep_vod)",
            params![sid],
        )?;
        tx.execute(
            "DELETE FROM series WHERE source_id = ?1
             AND id NOT IN (SELECT id FROM _keep_ser)",
            params![sid],
        )?;

        // Checksum only after full successful commit — all-or-nothing per source.
        tx.commit()?;

        if let Ok(conn) = self.conn(source_id) {
            let _ = Self::meta_set_conn(conn, &key, &checksum);
            let _ = conn.execute_batch(
                "INSERT INTO vod_fts(vod_fts) VALUES('rebuild');
                 INSERT INTO series_fts(series_fts) VALUES('rebuild');",
            );
        }

        if let Some(p) = progress {
            let (d, t) = p.snapshot();
            p.set_done(t.max(d));
        }

        info!(
            %source_id,
            checksum = &checksum[..12.min(checksum.len())],
            channels = merged.channels.len(),
            vod = merged.vod.len(),
            series = merged.series.len(),
            preserved_meta,
            "catalog db upserted source"
        );

        Ok(BundleApplyResult {
            kind: BundleApplyKind::Updated { preserved_meta },
            checksum,
            merged: Some(merged),
        })
    }

    /// Legacy full replace — prefers [`apply_source_bundle`].
    pub fn replace_source_bundle(
        &mut self,
        source_id: Uuid,
        part: &PlaylistBundle,
    ) -> rusqlite::Result<()> {
        let _ = self.apply_source_bundle(source_id, part.clone())?;
        Ok(())
    }

    fn load_enriched_vod_map(
        &self,
        source_id: Uuid,
        sid: &str,
    ) -> rusqlite::Result<HashMap<String, VodItem>> {
        let mut map = HashMap::new();
        let conn = self.conn(source_id)?;
        let mut stmt = conn.prepare(
            "SELECT id, payload FROM vod WHERE source_id = ?1 AND meta_ok = 1",
        )?;
        let rows = stmt.query_map(params![sid], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows.flatten() {
            let (id, raw) = row;
            if let Ok(item) = serde_json::from_str::<VodItem>(&raw) {
                map.insert(id, item);
            }
        }
        Ok(map)
    }

    fn load_enriched_series_map(
        &self,
        source_id: Uuid,
        sid: &str,
    ) -> rusqlite::Result<HashMap<String, SeriesItem>> {
        let mut map = HashMap::new();
        let conn = self.conn(source_id)?;
        let mut stmt = conn.prepare(
            "SELECT id, payload FROM series WHERE source_id = ?1 AND meta_ok = 1",
        )?;
        let rows = stmt.query_map(params![sid], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows.flatten() {
            let (id, raw) = row;
            if let Ok(item) = serde_json::from_str::<SeriesItem>(&raw) {
                map.insert(id, item);
            }
        }
        Ok(map)
    }

    /// Index a downloaded image path (disk already written).
    pub fn touch_image(&self, source_id: Uuid, url: &str, path: &Path, bytes: usize) {
        let Ok(conn) = self.conn(source_id) else {
            return;
        };
        let mut h = Sha256::new();
        h.update(url.as_bytes());
        let url_hash = hex::encode(h.finalize());
        let path_s = path.to_string_lossy();
        let _ = conn.execute(
            "INSERT INTO image_cache (url_hash, url, path, bytes, accessed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(url_hash) DO UPDATE SET
               accessed_at=excluded.accessed_at,
               bytes=excluded.bytes,
               path=excluded.path,
               url=excluded.url",
            params![url_hash, url, path_s.as_ref(), bytes as i64, now_secs() as i64],
        );
    }

    /// Drop oldest disk images when over `max_files` (best-effort, all profiles).
    pub fn evict_old_images(&self, max_files_per_profile: usize) {
        for id in self.source_ids() {
            let Ok(conn) = self.conn(id) else {
                continue;
            };
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM image_cache", [], |r| r.get(0))
                .unwrap_or(0);
            let excess = (count as usize).saturating_sub(max_files_per_profile);
            if excess == 0 {
                continue;
            }
            let Ok(mut stmt) = conn.prepare(
                "SELECT url_hash, path FROM image_cache ORDER BY accessed_at ASC LIMIT ?1",
            ) else {
                continue;
            };
            let Ok(rows) = stmt.query_map(params![excess as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            }) else {
                continue;
            };
            let doomed: Vec<_> = rows.flatten().collect();
            drop(stmt);
            for (hash, path) in doomed {
                let _ = std::fs::remove_file(&path);
                let _ = conn.execute("DELETE FROM image_cache WHERE url_hash = ?1", params![hash]);
            }
        }
    }

    /// Drop meta_cache rows older than `ttl_secs` (best-effort, all profiles).
    pub fn evict_old_meta(&self, ttl_secs: u64) {
        let cutoff = now_secs().saturating_sub(ttl_secs) as i64;
        for conn in self.conns.values() {
            let _ = conn.execute(
                "DELETE FROM meta_cache WHERE fetched_at < ?1",
                params![cutoff],
            );
        }
    }

    pub fn merge_epg(&self, source_id: Uuid, programmes: &[EpgProgramme]) -> rusqlite::Result<()> {
        let conn = self.conn(source_id)?;
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO epg (channel_id, start_ts, title, payload) VALUES (?1,?2,?3,?4)",
            )?;
            for p in programmes {
                let payload = serde_json::to_string(p).unwrap_or_default();
                stmt.execute(params![
                    p.channel_id,
                    p.start.to_rfc3339(),
                    p.title,
                    payload
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_bundle(&self) -> rusqlite::Result<PlaylistBundle> {
        let _prof = fluxplay_core::Stopwatch::start("catalog_load_bundle");
        let mut bundle = PlaylistBundle::default();
        for id in self.source_ids() {
            let Ok(conn) = self.conn(id) else {
                continue;
            };
            Self::load_bundle_from_conn(conn, &mut bundle)?;
        }
        info!(
            categories = bundle.categories.len(),
            channels = bundle.channels.len(),
            vod = bundle.vod.len(),
            series = bundle.series.len(),
            epg = bundle.epg.len(),
            "catalog load_bundle"
        );
        Ok(bundle)
    }

    fn load_bundle_from_conn(conn: &Connection, bundle: &mut PlaylistBundle) -> rusqlite::Result<()> {
        {
            let mut stmt = conn.prepare("SELECT id, name, content FROM categories")?;
            let rows = stmt.query_map([], |row| {
                let content: String = row.get(2)?;
                Ok(Category {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    content: match content.as_str() {
                        "vod" => ContentKind::Vod,
                        "series" => ContentKind::Series,
                        _ => ContentKind::Live,
                    },
                })
            })?;
            for r in rows.flatten() {
                bundle.categories.push(r);
            }
        }
        {
            let mut stmt = conn.prepare("SELECT payload FROM channels")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for raw in rows.flatten() {
                if let Ok(ch) = serde_json::from_str::<Channel>(&raw) {
                    bundle.channels.push(ch);
                }
            }
        }
        {
            let mut stmt = conn.prepare("SELECT payload FROM vod")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for raw in rows.flatten() {
                if let Ok(v) = serde_json::from_str::<VodItem>(&raw) {
                    bundle.vod.push(v);
                }
            }
        }
        {
            let mut stmt = conn.prepare("SELECT payload FROM series")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for raw in rows.flatten() {
                if let Ok(s) = serde_json::from_str::<SeriesItem>(&raw) {
                    bundle.series.push(s);
                }
            }
        }
        {
            let mut stmt = conn.prepare(
                "SELECT payload FROM epg ORDER BY start_ts DESC LIMIT 8000",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for raw in rows.flatten() {
                if let Ok(p) = serde_json::from_str::<EpgProgramme>(&raw) {
                    bundle.epg.push(p);
                }
            }
        }
        Ok(())
    }

    pub fn search_vod(&self, query: &str, category_id: Option<&str>, limit: usize) -> Vec<VodItem> {
        let items = self.search_fts_or_like("vod", query, category_id, limit);
        trace!(%query, n = items.len(), "search_vod");
        items
    }

    pub fn search_series(
        &self,
        query: &str,
        category_id: Option<&str>,
        limit: usize,
    ) -> Vec<SeriesItem> {
        let items = self.search_fts_or_like("series", query, category_id, limit);
        trace!(%query, n = items.len(), "search_series");
        items
    }

    fn search_fts_or_like<T: serde::de::DeserializeOwned>(
        &self,
        table: &str,
        query: &str,
        category_id: Option<&str>,
        limit: usize,
    ) -> Vec<T> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        // Prefer FTS5; fall back to LIKE.
        if let Some(items) = self.search_fts::<T>(table, q, category_id, limit) {
            if !items.is_empty() {
                return items;
            }
        }
        self.search_like(table, q, category_id, limit)
    }

    fn search_fts<T: serde::de::DeserializeOwned>(
        &self,
        table: &str,
        query: &str,
        category_id: Option<&str>,
        limit: usize,
    ) -> Option<Vec<T>> {
        let fts = format!("{table}_fts");
        // Escape FTS special chars lightly.
        let token = query
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '-' || *c == '\'')
            .collect::<String>();
        let token = token.trim();
        if token.is_empty() {
            return None;
        }
        let match_q = format!("{}*", token.split_whitespace().next().unwrap_or(token));
        let sql = if category_id.is_some() {
            format!(
                "SELECT t.payload FROM {table} t
                 JOIN {fts} f ON t.rowid = f.rowid
                 WHERE {fts} MATCH ?1 AND t.category_id = ?2
                 LIMIT ?3"
            )
        } else {
            format!(
                "SELECT t.payload FROM {table} t
                 JOIN {fts} f ON t.rowid = f.rowid
                 WHERE {fts} MATCH ?1
                 LIMIT ?2"
            )
        };
        let mut out = Vec::new();
        for conn in self.conns.values() {
            if out.len() >= limit {
                break;
            }
            let remain = limit - out.len();
            let Ok(mut stmt) = conn.prepare(&sql) else {
                continue;
            };
            let payloads: Vec<String> = if let Some(cid) = category_id {
                let Ok(mut rows) = stmt.query(params![match_q, cid, remain as i64]) else {
                    continue;
                };
                let mut collected = Vec::new();
                while let Ok(Some(row)) = rows.next() {
                    if let Ok(raw) = row.get::<_, String>(0) {
                        collected.push(raw);
                    }
                }
                collected
            } else {
                let Ok(mut rows) = stmt.query(params![match_q, remain as i64]) else {
                    continue;
                };
                let mut collected = Vec::new();
                while let Ok(Some(row)) = rows.next() {
                    if let Ok(raw) = row.get::<_, String>(0) {
                        collected.push(raw);
                    }
                }
                collected
            };
            for raw in payloads {
                if let Ok(item) = serde_json::from_str(&raw) {
                    out.push(item);
                }
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn search_like<T: serde::de::DeserializeOwned>(
        &self,
        table: &str,
        query: &str,
        category_id: Option<&str>,
        limit: usize,
    ) -> Vec<T> {
        let q = format!("%{}%", query.trim().to_ascii_lowercase());
        let mut out = Vec::new();
        for conn in self.conns.values() {
            if out.len() >= limit {
                break;
            }
            let remain = limit - out.len();
            let payloads = match category_id {
                Some(cid) => {
                    let sql = format!(
                        "SELECT payload FROM {table} WHERE lower(name) LIKE ?1 AND category_id = ?2 LIMIT ?3"
                    );
                    let Ok(mut stmt) = conn.prepare(&sql) else {
                        continue;
                    };
                    let Ok(mut rows) = stmt.query(params![q, cid, remain as i64]) else {
                        continue;
                    };
                    let mut collected = Vec::new();
                    while let Ok(Some(row)) = rows.next() {
                        if let Ok(raw) = row.get::<_, String>(0) {
                            collected.push(raw);
                        }
                    }
                    collected
                }
                None => {
                    let sql =
                        format!("SELECT payload FROM {table} WHERE lower(name) LIKE ?1 LIMIT ?2");
                    let Ok(mut stmt) = conn.prepare(&sql) else {
                        continue;
                    };
                    let Ok(mut rows) = stmt.query(params![q, remain as i64]) else {
                        continue;
                    };
                    let mut collected = Vec::new();
                    while let Ok(Some(row)) = rows.next() {
                        if let Ok(raw) = row.get::<_, String>(0) {
                            collected.push(raw);
                        }
                    }
                    collected
                }
            };
            for raw in payloads {
                if let Ok(item) = serde_json::from_str(&raw) {
                    out.push(item);
                }
            }
        }
        out
    }

    // Column-per-arg mirrors the DB schema; a params struct would duplicate the row type.
    #[allow(clippy::too_many_arguments)]
    pub fn update_vod_meta(
        &self,
        source_id: Uuid,
        id: &str,
        year: Option<&str>,
        genre: Option<&str>,
        plot: Option<&str>,
        poster: Option<&str>,
        rating: Option<&str>,
        imdb_id: Option<&str>,
    ) -> rusqlite::Result<()> {
        let sid = source_id.to_string();
        let Ok(conn) = self.conn(source_id) else {
            return Ok(());
        };
        let raw: Option<String> = conn
            .query_row(
                "SELECT payload FROM vod WHERE source_id = ?1 AND id = ?2",
                params![sid, id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(());
        };
        let mut item: VodItem = serde_json::from_str(&raw).unwrap_or_else(|_| VodItem {
            id: id.to_string(),
            name: String::new(),
            stream_url: String::new(),
            poster: None,
            plot: None,
            year: None,
            rating: None,
            genre: None,
            imdb_id: None,
            actors: None,
            director: None,
            writer: None,
            runtime: None,
            rated: None,
            awards: None,
            language: None,
            country: None,
            category_id: None,
            source_id: Some(source_id),
        });
        if year.is_some() {
            item.year = year.map(str::to_string);
        }
        if genre.is_some() {
            item.genre = genre.map(str::to_string);
        }
        if plot.is_some() {
            item.plot = plot.map(str::to_string);
        }
        if poster.is_some() {
            item.poster = poster.map(str::to_string);
        }
        if rating.is_some() {
            item.rating = rating.map(str::to_string);
        }
        if imdb_id.is_some() {
            item.imdb_id = imdb_id.map(str::to_string);
        }
        self.persist_vod(&item)
    }

    /// Write full VOD JSON payload (credits, ratings, IMDb id, …).
    pub fn persist_vod(&self, item: &VodItem) -> rusqlite::Result<()> {
        let Some(source_id) = item.source_id else {
            return Ok(());
        };
        let sid = source_id.to_string();
        let payload = serde_json::to_string(item).unwrap_or_default();
        let Ok(conn) = self.conn(source_id) else {
            return Ok(());
        };
        conn.execute(
            "UPDATE vod SET payload = ?1, year = ?2, genre = ?3, meta_ok = 1
             WHERE source_id = ?4 AND id = ?5",
            params![payload, item.year, item.genre, sid, item.id],
        )?;
        Ok(())
    }

    // Column-per-arg mirrors the DB schema; a params struct would duplicate the row type.
    #[allow(clippy::too_many_arguments)]
    pub fn update_series_meta(
        &self,
        source_id: Uuid,
        id: &str,
        year: Option<&str>,
        genre: Option<&str>,
        plot: Option<&str>,
        cover: Option<&str>,
        rating: Option<&str>,
        imdb_id: Option<&str>,
    ) -> rusqlite::Result<()> {
        let sid = source_id.to_string();
        let Ok(conn) = self.conn(source_id) else {
            return Ok(());
        };
        let raw: Option<String> = conn
            .query_row(
                "SELECT payload FROM series WHERE source_id = ?1 AND id = ?2",
                params![sid, id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(());
        };
        let mut item: SeriesItem = serde_json::from_str(&raw).unwrap_or_else(|_| SeriesItem {
            id: id.to_string(),
            name: String::new(),
            cover: None,
            banner: None,
            plot: None,
            year: None,
            rating: None,
            genre: None,
            imdb_id: None,
            actors: None,
            director: None,
            writer: None,
            runtime: None,
            rated: None,
            awards: None,
            language: None,
            country: None,
            seasons: Vec::new(),
            source_id: Some(source_id),
            category_id: None,
        });
        if year.is_some() {
            item.year = year.map(str::to_string);
        }
        if genre.is_some() {
            item.genre = genre.map(str::to_string);
        }
        if plot.is_some() {
            item.plot = plot.map(str::to_string);
        }
        if cover.is_some() {
            item.cover = cover.map(str::to_string);
        }
        if rating.is_some() {
            item.rating = rating.map(str::to_string);
        }
        if imdb_id.is_some() {
            item.imdb_id = imdb_id.map(str::to_string);
        }
        self.persist_series(&item)
    }

    /// Write full series JSON payload (credits, seasons, IMDb id, …).
    pub fn persist_series(&self, item: &SeriesItem) -> rusqlite::Result<()> {
        let Some(source_id) = item.source_id else {
            return Ok(());
        };
        let sid = source_id.to_string();
        let payload = serde_json::to_string(item).unwrap_or_default();
        let Ok(conn) = self.conn(source_id) else {
            return Ok(());
        };
        conn.execute(
            "UPDATE series SET payload = ?1, year = ?2, genre = ?3, meta_ok = 1
             WHERE source_id = ?4 AND id = ?5",
            params![payload, item.year, item.genre, sid, item.id],
        )?;
        Ok(())
    }

    /// OMDb/TVMaze response cache — avoids re-fetching the same cleaned title.
    /// Hits never expire. Misses retry after 7 days.
    pub fn meta_cache_get(
        &self,
        cache_key: &str,
    ) -> Option<(bool /* is_miss */, crate::metadata::MetaPatch)> {
        for conn in self.conns.values() {
            let row: Result<(i64, String, i64), _> = conn.query_row(
                "SELECT is_miss, payload, fetched_at FROM meta_cache WHERE cache_key = ?1",
                params![cache_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            );
            let Ok((is_miss, payload, fetched_at)) = row else {
                continue;
            };
            let age = now_secs().saturating_sub(fetched_at as u64);
            if is_miss != 0 {
                if age > 7 * 86400 {
                    continue;
                }
                return Some((true, crate::metadata::MetaPatch::default()));
            }
            let patch = serde_json::from_str(&payload).unwrap_or_default();
            return Some((false, patch));
        }
        None
    }

    pub fn meta_cache_put(
        &self,
        cache_key: &str,
        kind: &str,
        query: &crate::metadata::TitleQuery,
        patch: Option<&crate::metadata::MetaPatch>,
    ) {
        let is_miss = patch.is_none();
        let payload = patch
            .and_then(|p| serde_json::to_string(p).ok())
            .unwrap_or_else(|| "{}".into());
        let imdb = patch.and_then(|p| p.imdb_id.clone());
        for conn in self.conns.values() {
            let _ = conn.execute(
                "INSERT INTO meta_cache (cache_key, kind, query_title, query_year, imdb_id, payload, is_miss, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(cache_key) DO UPDATE SET
                   payload=excluded.payload,
                   imdb_id=excluded.imdb_id,
                   is_miss=excluded.is_miss,
                   fetched_at=excluded.fetched_at",
                params![
                    cache_key,
                    kind,
                    query.title,
                    query.year,
                    imdb,
                    payload,
                    if is_miss { 1 } else { 0 },
                    now_secs() as i64,
                ],
            );
        }
    }

    pub fn meta_cache_get_imdb(
        &self,
        imdb_id: &str,
    ) -> Option<crate::metadata::MetaPatch> {
        for conn in self.conns.values() {
            let payload: Result<String, _> = conn.query_row(
                "SELECT payload FROM meta_cache WHERE imdb_id = ?1 AND is_miss = 0 LIMIT 1",
                params![imdb_id],
                |r| r.get(0),
            );
            if let Ok(p) = payload {
                if let Ok(patch) = serde_json::from_str(&p) {
                    return Some(patch);
                }
            }
        }
        None
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let mut ch = 0usize;
        let mut vod = 0usize;
        let mut series = 0usize;
        for conn in self.conns.values() {
            ch += conn
                .query_row("SELECT COUNT(*) FROM channels", [], |r| r.get::<_, i64>(0))
                .unwrap_or(0) as usize;
            vod += conn
                .query_row("SELECT COUNT(*) FROM vod", [], |r| r.get::<_, i64>(0))
                .unwrap_or(0) as usize;
            series += conn
                .query_row("SELECT COUNT(*) FROM series", [], |r| r.get::<_, i64>(0))
                .unwrap_or(0) as usize;
        }
        (ch, vod, series)
    }

    /// Flush WAL into the main DB file before zip/export (avoids incomplete copies).
    pub fn wal_checkpoint_all(&self) {
        for conn in self.conns.values() {
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        }
    }
}

fn migrate_schema(conn: &Connection) -> rusqlite::Result<()> {
    // Performance PRAGMAs: WAL + NORMAL + page cache. mmap/cache sized for Android RAM.
    #[cfg(target_os = "android")]
    let pragmas = r#"
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        PRAGMA temp_store=MEMORY;
        PRAGMA busy_timeout=5000;
        PRAGMA cache_size=-8192;
        PRAGMA mmap_size=33554432;
        PRAGMA foreign_keys=OFF;
        PRAGMA analysis_limit=400;
        "#;
    #[cfg(not(target_os = "android"))]
    let pragmas = r#"
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        PRAGMA temp_store=MEMORY;
        PRAGMA busy_timeout=5000;
        PRAGMA cache_size=-32768;
        PRAGMA mmap_size=268435456;
        PRAGMA foreign_keys=OFF;
        PRAGMA analysis_limit=400;
        "#;
    conn.execute_batch(pragmas)?;

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS categories (
            source_id TEXT NOT NULL,
            id TEXT NOT NULL,
            name TEXT NOT NULL,
            content TEXT NOT NULL,
            PRIMARY KEY (source_id, id, content)
        );
        CREATE TABLE IF NOT EXISTS channels (
            source_id TEXT NOT NULL,
            id TEXT NOT NULL,
            payload TEXT NOT NULL,
            name TEXT NOT NULL,
            PRIMARY KEY (source_id, id)
        );
        CREATE TABLE IF NOT EXISTS vod (
            source_id TEXT NOT NULL,
            id TEXT NOT NULL,
            payload TEXT NOT NULL,
            name TEXT NOT NULL,
            year TEXT,
            genre TEXT,
            category_id TEXT,
            meta_ok INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (source_id, id)
        );
        CREATE TABLE IF NOT EXISTS series (
            source_id TEXT NOT NULL,
            id TEXT NOT NULL,
            payload TEXT NOT NULL,
            name TEXT NOT NULL,
            year TEXT,
            genre TEXT,
            category_id TEXT,
            meta_ok INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (source_id, id)
        );
        CREATE TABLE IF NOT EXISTS epg (
            channel_id TEXT NOT NULL,
            start_ts TEXT NOT NULL,
            title TEXT NOT NULL,
            payload TEXT NOT NULL,
            PRIMARY KEY (channel_id, start_ts, title)
        );
        CREATE TABLE IF NOT EXISTS image_cache (
            url_hash TEXT PRIMARY KEY,
            url TEXT NOT NULL,
            path TEXT NOT NULL,
            bytes INTEGER NOT NULL,
            accessed_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS meta_cache (
            cache_key TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            query_title TEXT NOT NULL,
            query_year TEXT,
            imdb_id TEXT,
            payload TEXT NOT NULL,
            is_miss INTEGER NOT NULL DEFAULT 0,
            fetched_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_vod_cat ON vod(category_id);
        CREATE INDEX IF NOT EXISTS idx_series_cat ON series(category_id);
        CREATE INDEX IF NOT EXISTS idx_vod_meta ON vod(meta_ok);
        CREATE INDEX IF NOT EXISTS idx_series_meta ON series(meta_ok);
        CREATE INDEX IF NOT EXISTS idx_meta_cache_imdb ON meta_cache(imdb_id);
        "#,
    )?;

    // Migrate older DBs that predate meta_cache.
    let _ = conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS meta_cache (
            cache_key TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            query_title TEXT NOT NULL,
            query_year TEXT,
            imdb_id TEXT,
            payload TEXT NOT NULL,
            is_miss INTEGER NOT NULL DEFAULT 0,
            fetched_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_meta_cache_imdb ON meta_cache(imdb_id);
        "#,
    );

    // FTS5 — ignore errors if already present / unsupported.
    let _ = conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS vod_fts USING fts5(
            name, year, genre, content='vod', content_rowid='rowid'
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS series_fts USING fts5(
            name, year, genre, content='series', content_rowid='rowid'
        );
        "#,
    );

    // Columns added in later revisions.
    let _ = conn
        .execute("ALTER TABLE vod ADD COLUMN meta_ok INTEGER NOT NULL DEFAULT 0", []);
    let _ = conn
        .execute("ALTER TABLE series ADD COLUMN meta_ok INTEGER NOT NULL DEFAULT 0", []);

    Ok(())
}



/// Active legacy DB, or archived `.migrated.bak` after a prior split.
fn legacy_catalog_source() -> Option<PathBuf> {
    let live = crate::storage::legacy_catalog_path();
    if live.is_file() {
        return Some(live);
    }
    let bak = live.with_extension("sqlite3.migrated.bak");
    if bak.is_file() {
        Some(bak)
    } else {
        None
    }
}

/// Copy rows with **named columns** (legacy may have extra cols like `group_name`).
fn copy_source_rows(dst: &Connection, legacy_path: &Path, source_id: Uuid) -> rusqlite::Result<()> {
    let legacy_path_str = legacy_path.to_string_lossy().replace('\'', "''");
    let attach = format!("ATTACH DATABASE '{legacy_path_str}' AS legacy");
    dst.execute_batch(&attach)?;
    let sid = source_id.to_string();
    let copies = [
        (
            "categories",
            "INSERT OR IGNORE INTO main.categories (source_id, id, name, content)
             SELECT source_id, id, name, content FROM legacy.categories WHERE source_id = ?1",
        ),
        (
            "channels",
            "INSERT OR IGNORE INTO main.channels (source_id, id, payload, name)
             SELECT source_id, id, payload, name FROM legacy.channels WHERE source_id = ?1",
        ),
        (
            "vod",
            "INSERT OR IGNORE INTO main.vod (source_id, id, payload, name, year, genre, category_id, meta_ok)
             SELECT source_id, id, payload, name, year, genre, category_id,
                    COALESCE(meta_ok, 0) FROM legacy.vod WHERE source_id = ?1",
        ),
        (
            "series",
            "INSERT OR IGNORE INTO main.series (source_id, id, payload, name, year, genre, category_id, meta_ok)
             SELECT source_id, id, payload, name, year, genre, category_id,
                    COALESCE(meta_ok, 0) FROM legacy.series WHERE source_id = ?1",
        ),
    ];
    for (table, sql) in copies {
        if let Err(e) = dst.execute(sql, params![sid]) {
            warn!(%table, error = %e, "migrate named-column copy failed");
        }
    }
    for (table, sql) in [
        (
            "epg",
            "INSERT OR IGNORE INTO main.epg (channel_id, start_ts, title, payload)
             SELECT channel_id, start_ts, title, payload FROM legacy.epg",
        ),
        (
            "meta_cache",
            "INSERT OR IGNORE INTO main.meta_cache SELECT * FROM legacy.meta_cache",
        ),
        (
            "image_cache",
            "INSERT OR IGNORE INTO main.image_cache SELECT * FROM legacy.image_cache",
        ),
        (
            "meta",
            "INSERT OR IGNORE INTO main.meta SELECT * FROM legacy.meta",
        ),
    ] {
        if let Err(e) = dst.execute(sql, []) {
            debug!(%table, error = %e, "migrate shared table copy");
        }
    }
    let _ = dst.execute_batch("DETACH DATABASE legacy");
    Ok(())
}

fn relocate_profile_images(dst: &Connection, source_id: Uuid) {
    let Ok(mut stmt) = dst.prepare("SELECT path FROM image_cache") else {
        return;
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return;
    };
    let dest_root = crate::storage::profile_images_dir(source_id);
    for p in rows.flatten() {
        let from = PathBuf::from(&p);
        if !from.is_file() {
            continue;
        }
        let Some(name) = from.file_name() else {
            continue;
        };
        let to = dest_root.join(name);
        if !to.exists() {
            let _ = std::fs::copy(&from, &to);
        }
        let to_s = to.to_string_lossy().to_string();
        let _ = dst.execute(
            "UPDATE image_cache SET path = ?1 WHERE path = ?2",
            params![to_s, p],
        );
    }
}

/// One-shot: split legacy global catalog.sqlite3 into profiles/{uuid}/catalog.sqlite3.
fn migrate_legacy_catalog(known_ids: &[Uuid]) {
    let legacy = crate::storage::legacy_catalog_path();
    if !legacy.is_file() {
        return;
    }
    info!(path = %legacy.display(), "migrating legacy catalog → per-profile DBs");
    let Ok(src) = Connection::open(&legacy) else {
        warn!("legacy catalog open failed");
        return;
    };
    let _ = migrate_schema(&src);

    let mut ids: Vec<Uuid> = known_ids.to_vec();
    if let Ok(mut stmt) = src.prepare(
        "SELECT DISTINCT source_id FROM channels
         UNION SELECT DISTINCT source_id FROM vod
         UNION SELECT DISTINCT source_id FROM series
         UNION SELECT DISTINCT source_id FROM categories",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for s in rows.flatten() {
                if let Ok(id) = Uuid::parse_str(&s) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
    }

    for id in &ids {
        let path = crate::storage::profile_catalog_path(*id);
        if path.is_file() {
            // May still be incomplete (schema mismatch on first migrate) — repair later.
            continue;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::create_dir_all(crate::storage::profile_images_dir(*id));
        let Ok(dst) = Connection::open(&path) else {
            continue;
        };
        if migrate_schema(&dst).is_err() {
            continue;
        }
        if let Err(e) = copy_source_rows(&dst, &legacy, *id) {
            warn!(%id, error = %e, "migrate profile copy failed");
            continue;
        }
        relocate_profile_images(&dst, *id);
        info!(%id, "migrated profile catalog");
    }

    let bak = legacy.with_extension("sqlite3.migrated.bak");
    match std::fs::rename(&legacy, &bak) {
        Ok(()) => info!(path = %bak.display(), "legacy catalog archived"),
        Err(e) => warn!(error = %e, "legacy catalog rename failed"),
    }
}

/// Fix profiles that were created empty for Live (SELECT * vs schema drift).
fn repair_incomplete_profiles(known_ids: &[Uuid]) {
    let Some(legacy) = legacy_catalog_source() else {
        return;
    };
    let Ok(src) = Connection::open(&legacy) else {
        return;
    };
    let mut ids: Vec<Uuid> = known_ids.to_vec();
    if let Ok(mut stmt) = src.prepare("SELECT DISTINCT source_id FROM channels") {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for s in rows.flatten() {
                if let Ok(id) = Uuid::parse_str(&s) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
    }
    drop(src);

    for id in ids {
        let path = crate::storage::profile_catalog_path(id);
        if !path.is_file() {
            continue;
        }
        let Ok(dst) = Connection::open(&path) else {
            continue;
        };
        let _ = migrate_schema(&dst);
        let ch: i64 = dst
            .query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))
            .unwrap_or(0);
        if ch > 0 {
            continue;
        }
        let legacy_ch: i64 = {
            let Ok(src) = Connection::open(&legacy) else {
                continue;
            };
            src.query_row(
                "SELECT COUNT(*) FROM channels WHERE source_id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .unwrap_or(0)
        };
        if legacy_ch == 0 {
            continue;
        }
        info!(%id, legacy_ch, "repairing empty channels from legacy catalog");
        if let Err(e) = copy_source_rows(&dst, &legacy, id) {
            warn!(%id, error = %e, "repair copy failed");
            continue;
        }
        let after: i64 = dst
            .query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))
            .unwrap_or(0);
        info!(%id, channels = after, "profile channels repaired");
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn checksum_meta_key(source_id: Uuid) -> String {
    format!("source_cksum:{source_id}")
}

/// Stable hash of the portal spine (ids / urls / names) — ignores OMDb fields.
/// Outcome of a blocking multi-source ingest (runs off the iced UI thread).
#[derive(Debug, Clone, Default)]
pub struct IngestBatchReport {
    pub ok: usize,
    pub err: usize,
    pub unchanged: usize,
    pub preserved: usize,
    pub source_ids: Vec<Uuid>,
}

/// Open profile DBs and apply portal dumps on a blocking thread.
/// Checksum meta is written only after each source's full upsert commits.
pub fn ingest_sources_blocking(
    results: Vec<(Uuid, Result<PlaylistBundle, String>)>,
    progress: std::sync::Arc<JobProgress>,
) -> IngestBatchReport {
    let ids: Vec<Uuid> = results.iter().map(|(id, _)| *id).collect();
    let mut report = IngestBatchReport {
        source_ids: ids.clone(),
        ..Default::default()
    };
    let Some(mut db) = CatalogDb::open(&ids) else {
        report.err = results.len();
        return report;
    };
    // Approximate total rows for the status bar (refined per apply).
    let approx: u64 = results
        .iter()
        .filter_map(|(_, r)| r.as_ref().ok())
        .map(|p| {
            (p.categories.len() + p.channels.len() + p.vod.len() + p.series.len()) as u64
        })
        .sum();
    progress.reset(approx.max(1));
    let mut done_acc = 0u64;
    for (source_id, result) in results {
        match result {
            Ok(part) => {
                let rows = (part.categories.len()
                    + part.channels.len()
                    + part.vod.len()
                    + part.series.len()) as u64;
                let epg = part.epg.clone();
                match db.apply_source_bundle_progressive(source_id, part, Some(progress.as_ref()))
                {
                    Ok(res) => {
                        if let Err(e) = db.merge_epg(source_id, &epg) {
                            warn!(error = %e, "catalog epg merge failed");
                        }
                        db.mark_full_sync_now(source_id);
                        match res.kind {
                            BundleApplyKind::Unchanged => {
                                report.unchanged += 1;
                                report.ok += 1;
                                done_acc += rows.max(1);
                                progress.set_done(done_acc.min(approx.max(1)));
                            }
                            BundleApplyKind::Updated { preserved_meta } => {
                                report.preserved += preserved_meta;
                                report.ok += 1;
                                done_acc += rows;
                                progress.set_done(done_acc.min(approx.max(1)));
                            }
                        }
                    }
                    Err(e) => {
                        warn!(%source_id, error = %e, "catalog apply failed");
                        report.err += 1;
                    }
                }
            }
            Err(e) => {
                warn!(%source_id, error = %e, "source load failed");
                report.err += 1;
            }
        }
    }
    let (d, t) = progress.snapshot();
    progress.set_done(t.max(d));
    report
}

/// Single-source ingest on a blocking thread.
pub fn ingest_one_blocking(
    source_id: Uuid,
    part: PlaylistBundle,
    progress: std::sync::Arc<JobProgress>,
) -> Result<BundleApplyKind, String> {
    progress.reset(1);
    let mut db = CatalogDb::open(&[source_id]).ok_or_else(|| "catalog db open failed".to_string())?;
    let epg = part.epg.clone();
    let res = db
        .apply_source_bundle_progressive(source_id, part, Some(progress.as_ref()))
        .map_err(|e| e.to_string())?;
    if let Err(e) = db.merge_epg(source_id, &epg) {
        warn!(error = %e, "catalog epg merge failed");
    }
    db.mark_full_sync_now(source_id);
    progress.set_done(1);
    Ok(res.kind)
}

/// Load merged bundle off the UI thread (read path; WAL-friendly).
pub fn load_bundle_blocking(source_ids: &[Uuid]) -> Result<PlaylistBundle, String> {
    let db = CatalogDb::open(source_ids).ok_or_else(|| "catalog db open failed".to_string())?;
    db.load_bundle().map_err(|e| e.to_string())
}

pub fn portal_checksum(part: &PlaylistBundle) -> String {
    let mut hasher = Sha256::new();
    let mut cats: Vec<_> = part
        .categories
        .iter()
        .map(|c| {
            (
                match c.content {
                    ContentKind::Live => 0u8,
                    ContentKind::Vod => 1,
                    ContentKind::Series => 2,
                },
                c.id.as_str(),
                c.name.as_str(),
            )
        })
        .collect();
    cats.sort_unstable();
    for (k, id, name) in cats {
        hasher.update([k]);
        hasher.update(id.as_bytes());
        hasher.update([0]);
        hasher.update(name.as_bytes());
        hasher.update([0xff]);
    }

    let mut channels: Vec<_> = part
        .channels
        .iter()
        .map(|c| {
            (
                c.id.as_str(),
                c.stream_url.as_str(),
                c.name.as_str(),
                c.logo.as_deref().unwrap_or(""),
                c.group.as_deref().unwrap_or(""),
            )
        })
        .collect();
    channels.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (id, url, name, logo, group) in channels {
        hasher.update(id.as_bytes());
        hasher.update([0]);
        hasher.update(url.as_bytes());
        hasher.update([0]);
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(logo.as_bytes());
        hasher.update([0]);
        hasher.update(group.as_bytes());
        hasher.update([0xff]);
    }

    let mut vod: Vec<_> = part
        .vod
        .iter()
        .map(|v| {
            (
                v.id.as_str(),
                v.stream_url.as_str(),
                v.name.as_str(),
                v.poster.as_deref().unwrap_or(""),
                v.category_id.as_deref().unwrap_or(""),
            )
        })
        .collect();
    vod.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (id, url, name, poster, cat) in vod {
        hasher.update(id.as_bytes());
        hasher.update([0]);
        hasher.update(url.as_bytes());
        hasher.update([0]);
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(poster.as_bytes());
        hasher.update([0]);
        hasher.update(cat.as_bytes());
        hasher.update([0xff]);
    }

    let mut series: Vec<_> = part
        .series
        .iter()
        .map(|s| {
            let ep_count: usize = s.seasons.iter().map(|se| se.episodes.len()).sum();
            (
                s.id.as_str(),
                s.name.as_str(),
                s.cover.as_deref().unwrap_or(""),
                s.banner.as_deref().unwrap_or(""),
                s.category_id.as_deref().unwrap_or(""),
                s.seasons.len(),
                ep_count,
            )
        })
        .collect();
    series.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (id, name, cover, banner, cat, seasons, eps) in series {
        hasher.update(id.as_bytes());
        hasher.update([0]);
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(cover.as_bytes());
        hasher.update([0]);
        hasher.update(banner.as_bytes());
        hasher.update([0]);
        hasher.update(cat.as_bytes());
        hasher.update([0]);
        hasher.update(seasons.to_le_bytes());
        hasher.update(eps.to_le_bytes());
        hasher.update([0xff]);
    }

    hex::encode(hasher.finalize())
}

fn prefer_longer(dst: &mut Option<String>, src: Option<String>) {
    let src_len = src.as_ref().map(|s| s.len()).unwrap_or(0);
    let dst_len = dst.as_ref().map(|s| s.len()).unwrap_or(0);
    if src_len > dst_len {
        *dst = src;
    }
}

fn prefer_if_empty(dst: &mut Option<String>, src: Option<String>) {
    if dst.as_ref().map(|s| s.is_empty()).unwrap_or(true)
        && src.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
            *dst = src;
        }
}

fn merge_vod_keep_meta(portal: &mut VodItem, old: &VodItem) {
    prefer_longer(&mut portal.plot, old.plot.clone());
    prefer_if_empty(&mut portal.actors, old.actors.clone());
    prefer_if_empty(&mut portal.director, old.director.clone());
    prefer_if_empty(&mut portal.writer, old.writer.clone());
    prefer_if_empty(&mut portal.runtime, old.runtime.clone());
    prefer_if_empty(&mut portal.rated, old.rated.clone());
    prefer_if_empty(&mut portal.awards, old.awards.clone());
    prefer_if_empty(&mut portal.language, old.language.clone());
    prefer_if_empty(&mut portal.country, old.country.clone());
    prefer_if_empty(&mut portal.genre, old.genre.clone());
    prefer_if_empty(&mut portal.year, old.year.clone());
    prefer_if_empty(&mut portal.rating, old.rating.clone());
    prefer_if_empty(&mut portal.imdb_id, old.imdb_id.clone());
    prefer_if_empty(&mut portal.poster, old.poster.clone());
}

fn merge_series_keep_meta(portal: &mut SeriesItem, old: &SeriesItem) {
    prefer_longer(&mut portal.plot, old.plot.clone());
    prefer_if_empty(&mut portal.actors, old.actors.clone());
    prefer_if_empty(&mut portal.director, old.director.clone());
    prefer_if_empty(&mut portal.writer, old.writer.clone());
    prefer_if_empty(&mut portal.runtime, old.runtime.clone());
    prefer_if_empty(&mut portal.rated, old.rated.clone());
    prefer_if_empty(&mut portal.awards, old.awards.clone());
    prefer_if_empty(&mut portal.language, old.language.clone());
    prefer_if_empty(&mut portal.country, old.country.clone());
    prefer_if_empty(&mut portal.genre, old.genre.clone());
    prefer_if_empty(&mut portal.year, old.year.clone());
    prefer_if_empty(&mut portal.rating, old.rating.clone());
    prefer_if_empty(&mut portal.imdb_id, old.imdb_id.clone());
    prefer_if_empty(&mut portal.cover, old.cover.clone());
    prefer_if_empty(&mut portal.banner, old.banner.clone());
    if portal.seasons.is_empty() && !old.seasons.is_empty() {
        portal.seasons = old.seasons.clone();
    }
}

fn vod_meta_ok(v: &VodItem) -> bool {
    v.imdb_id.is_some()
        || (v.plot.as_ref().map(|p| p.len() >= 40).unwrap_or(false) && v.actors.is_some())
        || (v.genre.is_some() && v.year.is_some() && v.plot.is_some())
}

fn series_meta_ok(s: &SeriesItem) -> bool {
    s.imdb_id.is_some()
        || (s.plot.as_ref().map(|p| p.len() >= 40).unwrap_or(false) && s.actors.is_some())
        || (s.genre.is_some() && s.year.is_some() && s.plot.is_some())
}
