//! Local SQLite catalog — offline-first Single Source of Truth.
//! Tuned PRAGMAs (WAL / NORMAL sync / mmap / page cache) + FTS5 search.
//!
//! Public helpers include sync-fallback / tooling APIs not always on the hot path.

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
use crate::browse_tiles::BrowseTile;

/// Skip portal re-sync when last full sync is younger than this.
pub const SYNC_FRESH_SECS: u64 = 12 * 3600;

/// Result of applying a portal dump to the local catalog.
#[derive(Debug)]
pub struct BundleApplyResult {
    pub kind: BundleApplyKind,
}

/// Boot spine: categories + EPG + counts. No VOD/series/channel payloads in RAM.
#[derive(Debug, Clone, Default)]
pub struct CatalogSpine {
    pub categories: Vec<Category>,
    pub epg: Vec<EpgProgramme>,
    pub channel_count: usize,
    pub vod_count: usize,
    pub series_count: usize,
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
        // ~8 MiB page cache (was 2) — mosaic OFFSET pages stay warm while scrolling.
        // Still no mmap of the whole catalog file into RSS.
        let _ = conn.pragma_update(None, "cache_size", -8192i64);
        let _ = conn.pragma_update(None, "mmap_size", 0i64);
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

    /// Same as a full portal apply with optional atomic progress (chunked upserts).
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
                "INSERT INTO channels (source_id, id, payload, name, logo)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(source_id, id) DO UPDATE SET
                   payload=excluded.payload, name=excluded.name, logo=excluded.logo",
            )?;
            for chunk in merged.channels.chunks(DEFAULT_CHUNK) {
                for ch in chunk {
                    let payload = serde_json::to_string(ch).unwrap_or_default();
                    let logo = ch
                        .logo
                        .as_deref()
                        .or(ch.tvg_logo.as_deref())
                        .map(str::to_string);
                    keep.execute(params![ch.id])?;
                    stmt.execute(params![sid, ch.id, payload, ch.name, logo])?;
                }
                if let Some(p) = progress {
                    p.add_done(chunk.len() as u64);
                }
            }
        }
        {
            let mut keep = tx.prepare("INSERT OR IGNORE INTO _keep_vod(id) VALUES(?1)")?;
            let mut stmt = tx.prepare(
                "INSERT INTO vod (source_id, id, payload, name, year, genre, category_id, meta_ok, poster, rating)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(source_id, id) DO UPDATE SET
                   payload=excluded.payload,
                   name=excluded.name,
                   year=excluded.year,
                   genre=excluded.genre,
                   category_id=excluded.category_id,
                   meta_ok=excluded.meta_ok,
                   poster=excluded.poster,
                   rating=excluded.rating",
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
                        meta_ok,
                        v.poster,
                        v.rating,
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
                "INSERT INTO series (source_id, id, payload, name, year, genre, category_id, meta_ok, cover, rating)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(source_id, id) DO UPDATE SET
                   payload=excluded.payload,
                   name=excluded.name,
                   year=excluded.year,
                   genre=excluded.genre,
                   category_id=excluded.category_id,
                   meta_ok=excluded.meta_ok,
                   cover=excluded.cover,
                   rating=excluded.rating",
            )?;
            for chunk in merged.series.chunks(DEFAULT_CHUNK) {
                for s in chunk {
                    let payload = serde_json::to_string(s).unwrap_or_default();
                    let meta_ok = if series_meta_ok(s) { 1i64 } else { 0 };
                    let cover = s.cover.clone().or_else(|| s.banner.clone());
                    keep.execute(params![s.id])?;
                    stmt.execute(params![
                        sid,
                        s.id,
                        payload,
                        s.name,
                        s.year,
                        s.genre,
                        s.category_id,
                        meta_ok,
                        cover,
                        s.rating,
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
        })
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
        // Past programmes are never shown again.
        let cutoff = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
        tx.execute("DELETE FROM epg WHERE start_ts < ?1", params![cutoff])?;
        tx.commit()?;
        Ok(())
    }

    /// One catalog row. `table` is `vod`, `series`, or `channels`.
    pub fn payload_by_id(&self, table: &str, id: &str) -> Option<String> {
        let table = match table {
            "vod" | "series" | "channels" => table,
            _ => return None,
        };
        let sql = format!("SELECT payload FROM {table} WHERE id = ?1 LIMIT 1");
        for conn in self.conns.values() {
            let raw: rusqlite::Result<String> =
                conn.query_row(&sql, rusqlite::params![id], |row| row.get(0));
            if let Ok(raw) = raw {
                return Some(raw);
            }
        }
        None
    }

    /// Ids only: the browse index maps them to rows already in RAM, so full
    /// payloads (plot, cast, …) are never deserialized for a search.
    pub fn search_vod_ids(&self, query: &str, category_id: Option<&str>, limit: usize) -> Vec<String> {
        let ids = self.search_fts_or_like("vod", query, category_id, limit);
        trace!(%query, n = ids.len(), "search_vod");
        ids
    }

    pub fn search_series_ids(
        &self,
        query: &str,
        category_id: Option<&str>,
        limit: usize,
    ) -> Vec<String> {
        let ids = self.search_fts_or_like("series", query, category_id, limit);
        trace!(%query, n = ids.len(), "search_series");
        ids
    }

    fn search_fts_or_like(
        &self,
        table: &str,
        query: &str,
        category_id: Option<&str>,
        limit: usize,
    ) -> Vec<String> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        // Prefer FTS5; fall back to LIKE.
        if let Some(items) = self.search_fts(table, q, category_id, limit) {
            if !items.is_empty() {
                return items;
            }
        }
        self.search_like(table, q, category_id, limit)
    }

    fn search_fts(
        &self,
        table: &str,
        query: &str,
        category_id: Option<&str>,
        limit: usize,
    ) -> Option<Vec<String>> {
        let fts = format!("{table}_fts");
        // Every word must match (as a prefix), like the Live filter. Words are cut on
        // anything non-alphanumeric so `-` or `'` never reach the FTS5 syntax.
        let words: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(|w| format!("\"{w}\"*"))
            .collect();
        if words.is_empty() {
            return None;
        }
        let match_q = words.join(" ");
        let sql = if category_id.is_some() {
            format!(
                "SELECT t.id FROM {table} t
                 JOIN {fts} f ON t.rowid = f.rowid
                 WHERE {fts} MATCH ?1 AND t.category_id = ?2
                 LIMIT ?3"
            )
        } else {
            format!(
                "SELECT t.id FROM {table} t
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
            let ids: Vec<String> = if let Some(cid) = category_id {
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
            out.extend(ids);
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn search_like(
        &self,
        table: &str,
        query: &str,
        category_id: Option<&str>,
        limit: usize,
    ) -> Vec<String> {
        let q = format!("%{}%", query.trim().to_ascii_lowercase());
        let mut out = Vec::new();
        for conn in self.conns.values() {
            if out.len() >= limit {
                break;
            }
            let remain = limit - out.len();
            let ids = match category_id {
                Some(cid) => {
                    let sql = format!(
                        "SELECT id FROM {table} WHERE lower(name) LIKE ?1 AND category_id = ?2 LIMIT ?3"
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
                        format!("SELECT id FROM {table} WHERE lower(name) LIKE ?1 LIMIT ?2");
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
            out.extend(ids);
        }
        out
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
            "UPDATE vod SET payload = ?1, year = ?2, genre = ?3, meta_ok = 1,
             poster = ?4, rating = ?5
             WHERE source_id = ?6 AND id = ?7",
            params![
                payload,
                item.year,
                item.genre,
                item.poster,
                item.rating,
                sid,
                item.id
            ],
        )?;
        Ok(())
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
        let cover = item.cover.clone().or_else(|| item.banner.clone());
        conn.execute(
            "UPDATE series SET payload = ?1, year = ?2, genre = ?3, meta_ok = 1,
             cover = ?4, rating = ?5
             WHERE source_id = ?6 AND id = ?7",
            params![
                payload,
                item.year,
                item.genre,
                cover,
                item.rating,
                sid,
                item.id
            ],
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

    /// Cached translation for [`crate::translate::cache_key`].
    pub fn translation_get(&self, cache_key: &str) -> Option<String> {
        self.conns.values().find_map(|conn| {
            conn.query_row(
                "SELECT text FROM translations WHERE cache_key = ?1",
                params![cache_key],
                |r| r.get::<_, String>(0),
            )
            .ok()
        })
    }

    pub fn translation_put(&self, cache_key: &str, text: &str, source_lang: Option<&str>) {
        if let Some(conn) = self.conns.values().next() {
            let _ = conn.execute(
                "INSERT INTO translations (cache_key, text, source_lang, fetched_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(cache_key) DO UPDATE SET
                   text=excluded.text, source_lang=excluded.source_lang, fetched_at=excluded.fetched_at",
                params![cache_key, text, source_lang, now_secs() as i64],
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
        self.counts_of(&self.source_ids())
    }

    pub fn counts_of(&self, ids: &[Uuid]) -> (usize, usize, usize) {
        let mut ch = 0usize;
        let mut vod = 0usize;
        let mut series = 0usize;
        for &id in ids {
            let Ok(conn) = self.conn(id) else {
                continue;
            };
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

    /// Categories + EPG + counts — never deserializes VOD/series/channel payloads.
    pub fn load_spine_of(&self, ids: &[Uuid]) -> rusqlite::Result<CatalogSpine> {
        let mut spine = CatalogSpine::default();
        for &id in ids {
            let Ok(conn) = self.conn(id) else {
                continue;
            };
            Self::load_spine_from_conn(conn, &mut spine)?;
        }
        let (ch, vod, series) = self.counts_of(ids);
        spine.channel_count = ch;
        spine.vod_count = vod;
        spine.series_count = series;
        info!(
            categories = spine.categories.len(),
            channels = spine.channel_count,
            vod = spine.vod_count,
            series = spine.series_count,
            "catalog load_spine"
        );
        Ok(spine)
    }

    fn load_spine_from_conn(conn: &Connection, spine: &mut CatalogSpine) -> rusqlite::Result<()> {
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
                spine.categories.push(r);
            }
        }
        {
            let now = chrono::Utc::now();
            let from = (now - chrono::Duration::hours(6)).to_rfc3339();
            let to = (now + chrono::Duration::hours(48)).to_rfc3339();
            let mut stmt = conn.prepare(
                "SELECT payload FROM epg WHERE start_ts >= ?1 AND start_ts <= ?2 ORDER BY start_ts LIMIT 8000",
            )?;
            let rows = stmt.query_map(params![from, to], |row| row.get::<_, String>(0))?;
            for raw in rows.flatten() {
                if let Ok(p) = serde_json::from_str::<EpgProgramme>(&raw) {
                    spine.epg.push(p);
                }
            }
        }
        Ok(())
    }

    /// Mosaic VOD rows (`ORDER BY rowid` = catalog / identity browse order).
    pub fn browse_tiles_vod(
        &self,
        ids: &[Uuid],
        offset: usize,
        limit: usize,
        category_id: Option<&str>,
    ) -> Vec<BrowseTile> {
        self.query_browse_tiles(ids, "vod", offset, limit, category_id)
    }

    pub fn browse_tiles_series(
        &self,
        ids: &[Uuid],
        offset: usize,
        limit: usize,
        category_id: Option<&str>,
    ) -> Vec<BrowseTile> {
        self.query_browse_tiles(ids, "series", offset, limit, category_id)
    }

    pub fn browse_tiles_channels(
        &self,
        ids: &[Uuid],
        offset: usize,
        limit: usize,
        group: Option<&str>,
    ) -> Vec<BrowseTile> {
        let mut out = Vec::with_capacity(limit.min(512));
        let mut skip = offset;
        let mut need = limit;
        let group_f = group.filter(|g| !g.is_empty() && *g != "Tous" && *g != "*");
        for &source_id in ids {
            if need == 0 {
                break;
            }
            let Ok(conn) = self.conn(source_id) else {
                continue;
            };
            let n = if let Some(g) = group_f {
                conn.query_row(
                    "SELECT COUNT(*) FROM channels WHERE json_extract(payload, '$.group') = ?1",
                    params![g],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0) as usize
            } else {
                conn.query_row("SELECT COUNT(*) FROM channels", [], |r| r.get::<_, i64>(0))
                    .unwrap_or(0) as usize
            };
            if skip >= n {
                skip -= n;
                continue;
            }
            let sql = if group_f.is_some() {
                "SELECT id, name, logo,
                        json_extract(payload, '$.group'),
                        json_extract(payload, '$.tvg_logo')
                 FROM channels
                 WHERE json_extract(payload, '$.group') = ?1
                 ORDER BY rowid LIMIT ?2 OFFSET ?3"
            } else {
                "SELECT id, name, logo,
                        json_extract(payload, '$.group'),
                        json_extract(payload, '$.tvg_logo')
                 FROM channels
                 ORDER BY rowid LIMIT ?1 OFFSET ?2"
            };
            let Ok(mut stmt) = conn.prepare(sql) else {
                continue;
            };
            let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<BrowseTile> {
                let logo: Option<String> = row.get(2)?;
                let group_v: Option<String> = row.get(3)?;
                let tvg: Option<String> = row.get(4)?;
                Ok(BrowseTile {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    art_url: logo.or(tvg),
                    category_id: group_v.clone(),
                    source_id: Some(source_id),
                    year: None,
                    genre: None,
                    rating: None,
                    group: group_v,
                })
            };
            let rows = if let Some(g) = group_f {
                stmt.query_map(params![g, need as i64, skip as i64], map)
            } else {
                stmt.query_map(params![need as i64, skip as i64], map)
            };
            if let Ok(rows) = rows {
                for tile in rows.flatten() {
                    out.push(tile);
                    need -= 1;
                    if need == 0 {
                        break;
                    }
                }
            }
            skip = 0;
        }
        out
    }

    fn query_browse_tiles(
        &self,
        ids: &[Uuid],
        table: &str,
        offset: usize,
        limit: usize,
        category_id: Option<&str>,
    ) -> Vec<BrowseTile> {
        let art_col = if table == "series" { "cover" } else { "poster" };
        let mut out = Vec::with_capacity(limit.min(512));
        let mut skip = offset;
        let mut need = limit;
        let cat = category_id.filter(|c| !c.is_empty() && *c != "*");
        // One profile: skip COUNT(*) (OFFSET is enough). Multi-source still needs it.
        let single = ids.len() == 1;
        for &source_id in ids {
            if need == 0 {
                break;
            }
            let Ok(conn) = self.conn(source_id) else {
                continue;
            };
            if !single {
                let n = if let Some(c) = cat {
                    conn.query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE category_id = ?1"),
                        params![c],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0) as usize
                } else {
                    conn.query_row(
                        &format!("SELECT COUNT(*) FROM {table}"),
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0) as usize
                };
                if skip >= n {
                    skip -= n;
                    continue;
                }
            }
            let sql = if cat.is_some() {
                format!(
                    "SELECT id, name, {art_col}, year, genre, rating, category_id
                     FROM {table} WHERE category_id = ?1 ORDER BY rowid LIMIT ?2 OFFSET ?3"
                )
            } else {
                format!(
                    "SELECT id, name, {art_col}, year, genre, rating, category_id
                     FROM {table} ORDER BY rowid LIMIT ?1 OFFSET ?2"
                )
            };
            let Ok(mut stmt) = conn.prepare(&sql) else {
                continue;
            };
            let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<BrowseTile> {
                Ok(BrowseTile {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    art_url: row.get(2)?,
                    year: row.get(3)?,
                    genre: row.get(4)?,
                    rating: row.get(5)?,
                    category_id: row.get(6)?,
                    source_id: Some(source_id),
                    group: None,
                })
            };
            let rows = if let Some(c) = cat {
                stmt.query_map(params![c, need as i64, skip as i64], map_row)
            } else {
                stmt.query_map(params![need as i64, skip as i64], map_row)
            };
            if let Ok(rows) = rows {
                for tile in rows.flatten() {
                    out.push(tile);
                    need = need.saturating_sub(1);
                    if need == 0 {
                        break;
                    }
                }
            }
            skip = 0;
            if single {
                break;
            }
        }
        out
    }

    /// Count rows matching an optional category (`*` / empty = all).
    pub fn count_vod(&self, ids: &[Uuid], category_id: Option<&str>) -> usize {
        self.count_table(ids, "vod", category_id)
    }

    pub fn count_series(&self, ids: &[Uuid], category_id: Option<&str>) -> usize {
        self.count_table(ids, "series", category_id)
    }

    pub fn count_channels(&self, ids: &[Uuid], group: Option<&str>) -> usize {
        let group_f = group.filter(|g| !g.is_empty() && *g != "Tous" && *g != "*");
        let mut n = 0usize;
        for &id in ids {
            let Ok(conn) = self.conn(id) else {
                continue;
            };
            n += if let Some(g) = group_f {
                conn.query_row(
                    "SELECT COUNT(*) FROM channels WHERE json_extract(payload, '$.group') = ?1",
                    params![g],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0) as usize
            } else {
                conn.query_row("SELECT COUNT(*) FROM channels", [], |r| r.get::<_, i64>(0))
                    .unwrap_or(0) as usize
            };
        }
        n
    }

    pub fn search_channel_ids(
        &self,
        query: &str,
        group: Option<&str>,
        limit: usize,
    ) -> Vec<String> {
        let group_f = group.filter(|g| !g.is_empty() && *g != "Tous" && *g != "*");
        let ids = self.search_fts_or_like("channels", query, None, limit);
        if group_f.is_none() {
            return ids;
        }
        let want = group_f.unwrap();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if out.len() >= limit {
                break;
            }
            if let Some(ch) = self.channel_by_id(&id) {
                if ch.group.as_deref() == Some(want) {
                    out.push(id);
                }
            }
        }
        out
    }

    fn count_table(&self, ids: &[Uuid], table: &str, category_id: Option<&str>) -> usize {
        let cat = category_id.filter(|c| !c.is_empty() && *c != "*");
        let mut n = 0usize;
        for &id in ids {
            let Ok(conn) = self.conn(id) else {
                continue;
            };
            n += if let Some(c) = cat {
                conn.query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE category_id = ?1"),
                    params![c],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0) as usize
            } else {
                conn.query_row(
                    &format!("SELECT COUNT(*) FROM {table}"),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0) as usize
            };
        }
        n
    }

    /// Slim tiles for an ordered id list (search results), keyed later by browse pos.
    pub fn slim_vod_by_ids(&self, ids: &[Uuid], item_ids: &[String]) -> Vec<BrowseTile> {
        self.slim_by_ids(ids, "vod", "poster", item_ids)
    }

    pub fn slim_series_by_ids(&self, ids: &[Uuid], item_ids: &[String]) -> Vec<BrowseTile> {
        self.slim_by_ids(ids, "series", "cover", item_ids)
    }

    pub fn slim_channels_by_ids(&self, ids: &[Uuid], item_ids: &[String]) -> Vec<BrowseTile> {
        let mut out = Vec::with_capacity(item_ids.len());
        for item_id in item_ids {
            for &source_id in ids {
                let Ok(conn) = self.conn(source_id) else {
                    continue;
                };
                let row = conn.query_row(
                    "SELECT id, name, logo,
                            json_extract(payload, '$.group'),
                            json_extract(payload, '$.tvg_logo')
                     FROM channels WHERE id = ?1 LIMIT 1",
                    params![item_id],
                    |row| {
                        let logo: Option<String> = row.get(2)?;
                        let group_v: Option<String> = row.get(3)?;
                        let tvg: Option<String> = row.get(4)?;
                        Ok(BrowseTile {
                            id: row.get(0)?,
                            name: row.get(1)?,
                            art_url: logo.or(tvg),
                            category_id: group_v.clone(),
                            source_id: Some(source_id),
                            year: None,
                            genre: None,
                            rating: None,
                            group: group_v,
                        })
                    },
                );
                if let Ok(tile) = row {
                    out.push(tile);
                    break;
                }
            }
        }
        out
    }

    /// `(global_offset, id, name, category_id)` for browse-index builds off SQLite.
    pub fn collect_vod_browse_rows(
        &self,
        source_ids: &[Uuid],
        category_id: Option<&str>,
    ) -> Vec<(usize, String, String, Option<String>)> {
        self.collect_table_browse_rows(source_ids, "vod", category_id)
    }

    pub fn collect_series_browse_rows(
        &self,
        source_ids: &[Uuid],
        category_id: Option<&str>,
    ) -> Vec<(usize, String, String, Option<String>)> {
        self.collect_table_browse_rows(source_ids, "series", category_id)
    }

    /// `(global_offset, name, group)` for live browse filters.
    pub fn collect_live_browse_rows(
        &self,
        source_ids: &[Uuid],
        group: Option<&str>,
    ) -> Vec<(usize, String, Option<String>)> {
        let group_f = group.filter(|g| !g.is_empty() && *g != "Tous" && *g != "*");
        let mut out = Vec::new();
        let mut global_base = 0usize;
        for &source_id in source_ids {
            let Ok(conn) = self.conn(source_id) else {
                continue;
            };
            let sql = if group_f.is_some() {
                "SELECT name, json_extract(payload, '$.group') FROM channels
                 WHERE json_extract(payload, '$.group') = ?1 ORDER BY rowid"
            } else {
                "SELECT name, json_extract(payload, '$.group') FROM channels ORDER BY rowid"
            };
            let Ok(mut stmt) = conn.prepare(sql) else {
                continue;
            };
            let mut local = 0usize;
            if let Some(g) = group_f {
                if let Ok(rows) = stmt.query_map(params![g], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                }) {
                    for row in rows.flatten() {
                        out.push((global_base + local, row.0, row.1));
                        local += 1;
                    }
                }
            } else if let Ok(rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            }) {
                for row in rows.flatten() {
                    out.push((global_base + local, row.0, row.1));
                    local += 1;
                }
            }
            global_base += local;
        }
        out
    }

    fn collect_table_browse_rows(
        &self,
        source_ids: &[Uuid],
        table: &str,
        category_id: Option<&str>,
    ) -> Vec<(usize, String, String, Option<String>)> {
        let cat = category_id.filter(|c| !c.is_empty() && *c != "*");
        let mut out = Vec::new();
        let mut global_base = 0usize;
        for &source_id in source_ids {
            let Ok(conn) = self.conn(source_id) else {
                continue;
            };
            let sql = if cat.is_some() {
                format!(
                    "SELECT id, name, category_id FROM {table} WHERE category_id = ?1 ORDER BY rowid"
                )
            } else {
                format!("SELECT id, name, category_id FROM {table} ORDER BY rowid")
            };
            let Ok(mut stmt) = conn.prepare(&sql) else {
                continue;
            };
            let mut local = 0usize;
            if let Some(c) = cat {
                if let Ok(rows) = stmt.query_map(params![c], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                }) {
                    for row in rows.flatten() {
                        out.push((global_base + local, row.0, row.1, row.2));
                        local += 1;
                    }
                }
            } else if let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            }) {
                for row in rows.flatten() {
                    out.push((global_base + local, row.0, row.1, row.2));
                    local += 1;
                }
            }
            global_base += local;
        }
        out
    }

    fn slim_by_ids(
        &self,
        source_ids: &[Uuid],
        table: &str,
        art_col: &str,
        item_ids: &[String],
    ) -> Vec<BrowseTile> {
        let mut out = Vec::with_capacity(item_ids.len());
        for item_id in item_ids {
            for &source_id in source_ids {
                let Ok(conn) = self.conn(source_id) else {
                    continue;
                };
                let sql = format!(
                    "SELECT id, name, {art_col}, year, genre, rating, category_id
                     FROM {table} WHERE id = ?1 LIMIT 1"
                );
                let row = conn.query_row(&sql, params![item_id], |row| {
                    Ok(BrowseTile {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        art_url: row.get(2)?,
                        year: row.get(3)?,
                        genre: row.get(4)?,
                        rating: row.get(5)?,
                        category_id: row.get(6)?,
                        source_id: Some(source_id),
                        group: None,
                    })
                });
                if let Ok(tile) = row {
                    out.push(tile);
                    break;
                }
            }
        }
        out
    }

    pub fn vod_item_by_id(&self, id: &str) -> Option<VodItem> {
        let raw = self.payload_by_id("vod", id)?;
        serde_json::from_str(&raw).ok()
    }

    pub fn series_item_by_id(&self, id: &str) -> Option<SeriesItem> {
        let raw = self.payload_by_id("series", id)?;
        serde_json::from_str(&raw).ok()
    }

    pub fn channel_by_id(&self, id: &str) -> Option<Channel> {
        let raw = self.payload_by_id("channels", id)?;
        serde_json::from_str(&raw).ok()
    }

    fn live_group_filter(group: Option<&str>) -> Option<&str> {
        group.filter(|g| !g.is_empty() && *g != "Tous" && *g != "*")
    }

    /// Live channels in catalog order (SQLite SSOT — `bundle.channels` stays empty on spine boot).
    pub fn channels_in_group(
        &self,
        ids: &[Uuid],
        group: Option<&str>,
        search: Option<&str>,
        limit: usize,
    ) -> Vec<Channel> {
        let group_f = Self::live_group_filter(group);
        let q = search
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase());
        let mut out = Vec::with_capacity(limit.min(64));
        'profiles: for &source_id in ids {
            let Ok(conn) = self.conn(source_id) else {
                continue;
            };
            if let Some(g) = group_f {
                let Ok(mut stmt) = conn.prepare(
                    "SELECT payload FROM channels WHERE json_extract(payload, '$.group') = ?1 ORDER BY rowid",
                ) else {
                    continue;
                };
                let Ok(rows) = stmt.query_map(params![g], |r| r.get::<_, String>(0)) else {
                    continue;
                };
                for raw in rows.flatten() {
                    let Ok(mut ch) = serde_json::from_str::<Channel>(&raw) else {
                        continue;
                    };
                    if ch.source_id.is_none() {
                        ch.source_id = Some(source_id);
                    }
                    if let Some(ref needle) = q {
                        let name = ch.name.to_lowercase();
                        let disp = crate::names::display_channel(&ch.name).to_lowercase();
                        if !name.contains(needle) && !disp.contains(needle) {
                            continue;
                        }
                    }
                    out.push(ch);
                    if out.len() >= limit {
                        break 'profiles;
                    }
                }
            } else {
                let Ok(mut stmt) =
                    conn.prepare("SELECT payload FROM channels ORDER BY rowid")
                else {
                    continue;
                };
                let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
                    continue;
                };
                for raw in rows.flatten() {
                    let Ok(mut ch) = serde_json::from_str::<Channel>(&raw) else {
                        continue;
                    };
                    if ch.source_id.is_none() {
                        ch.source_id = Some(source_id);
                    }
                    if let Some(ref needle) = q {
                        let name = ch.name.to_lowercase();
                        let disp = crate::names::display_channel(&ch.name).to_lowercase();
                        if !name.contains(needle) && !disp.contains(needle) {
                            continue;
                        }
                    }
                    out.push(ch);
                    if out.len() >= limit {
                        break 'profiles;
                    }
                }
            }
        }
        out
    }

    /// Previous / next live channel within a group (same order as mosaic / zap).
    pub fn live_channel_neighbor(
        &self,
        ids: &[Uuid],
        group: Option<&str>,
        current_id: &str,
        current_source: Option<Uuid>,
        delta: i32,
    ) -> Option<Channel> {
        let group_f = Self::live_group_filter(group);
        let mut order: Vec<(Uuid, String)> = Vec::new();
        for &source_id in ids {
            let Ok(conn) = self.conn(source_id) else {
                continue;
            };
            if let Some(g) = group_f {
                let Ok(mut stmt) = conn.prepare(
                    "SELECT id FROM channels WHERE json_extract(payload, '$.group') = ?1 ORDER BY rowid",
                ) else {
                    continue;
                };
                let Ok(rows) = stmt.query_map(params![g], |r| r.get::<_, String>(0)) else {
                    continue;
                };
                for id in rows.flatten() {
                    order.push((source_id, id));
                }
            } else {
                let Ok(mut stmt) = conn.prepare("SELECT id FROM channels ORDER BY rowid") else {
                    continue;
                };
                let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
                    continue;
                };
                for id in rows.flatten() {
                    order.push((source_id, id));
                }
            }
        }
        if order.is_empty() {
            return None;
        }
        let idx = order.iter().position(|(sid, id)| {
            id == current_id && (current_source.is_none() || Some(*sid) == current_source)
        })?;
        let len = order.len() as i32;
        if len == 0 {
            return None;
        }
        let next_i = (idx as i32 + delta).rem_euclid(len) as usize;
        let (sid, id) = &order[next_i];
        let mut ch = self.channel_by_id(id)?;
        if ch.source_id.is_none() {
            ch.source_id = Some(*sid);
        }
        Some(ch)
    }

    /// Ids needing metadata enrich (not the full row set).
    pub fn vod_ids_needing_enrich(&self, ids: &[Uuid], limit: usize) -> Vec<(Uuid, String)> {
        self.ids_needing_enrich(ids, "vod", limit)
    }

    pub fn series_ids_needing_enrich(&self, ids: &[Uuid], limit: usize) -> Vec<(Uuid, String)> {
        self.ids_needing_enrich(ids, "series", limit)
    }

    fn ids_needing_enrich(
        &self,
        ids: &[Uuid],
        table: &str,
        limit: usize,
    ) -> Vec<(Uuid, String)> {
        let mut out = Vec::new();
        for &source_id in ids {
            if out.len() >= limit {
                break;
            }
            let Ok(conn) = self.conn(source_id) else {
                continue;
            };
            let sql = format!(
                "SELECT id FROM {table} WHERE meta_ok = 0 ORDER BY rowid LIMIT ?1"
            );
            let Ok(mut stmt) = conn.prepare(&sql) else {
                continue;
            };
            let remain = (limit - out.len()) as i64;
            let ids: Vec<String> = stmt
                .query_map(params![remain], |r| r.get::<_, String>(0))
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .collect();
            for id in ids {
                out.push((source_id, id));
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }

    /// Flush WAL into the main DB file before zip/export (avoids incomplete copies).
    pub fn wal_checkpoint_all(&self) {
        for conn in self.conns.values() {
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        }
    }
}

fn migrate_schema(conn: &Connection) -> rusqlite::Result<()> {
    // WAL + NORMAL sync. Page cache / mmap are set per connection in `ensure_source`
    // (small cache, no mmap): the OS page cache already holds hot pages.
    #[cfg(target_os = "android")]
    let pragmas = r#"
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        PRAGMA temp_store=MEMORY;
        PRAGMA busy_timeout=5000;
        PRAGMA foreign_keys=OFF;
        PRAGMA analysis_limit=400;
        "#;
    #[cfg(not(target_os = "android"))]
    let pragmas = r#"
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        PRAGMA temp_store=MEMORY;
        PRAGMA busy_timeout=5000;
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
        "#,
    )?;

    // Columns added in later revisions — before the indexes that use them, or an
    // old catalog fails to open.
    let _ = conn
        .execute("ALTER TABLE vod ADD COLUMN meta_ok INTEGER NOT NULL DEFAULT 0", []);
    let _ = conn
        .execute("ALTER TABLE series ADD COLUMN meta_ok INTEGER NOT NULL DEFAULT 0", []);
    // Slim mosaic columns — full plot/cast stay in payload JSON only.
    let _ = conn.execute("ALTER TABLE vod ADD COLUMN poster TEXT", []);
    let _ = conn.execute("ALTER TABLE vod ADD COLUMN rating TEXT", []);
    let _ = conn.execute("ALTER TABLE series ADD COLUMN cover TEXT", []);
    let _ = conn.execute("ALTER TABLE series ADD COLUMN rating TEXT", []);
    let _ = conn.execute("ALTER TABLE channels ADD COLUMN logo TEXT", []);
    // One-shot JSON→column backfill. Re-running on every open scanned 200k+ rows
    // (~300–500 ms) and made mosaic scroll unusable when tile fills re-opened the DB.
    if CatalogDb::meta_get_conn(conn, "slim_cols_v1").as_deref() != Some("1") {
        let _ = conn.execute_batch(
            r#"
            UPDATE vod SET poster = json_extract(payload, '$.poster')
              WHERE poster IS NULL AND json_extract(payload, '$.poster') IS NOT NULL;
            UPDATE vod SET rating = json_extract(payload, '$.rating')
              WHERE rating IS NULL AND json_extract(payload, '$.rating') IS NOT NULL;
            UPDATE series SET cover = COALESCE(
                json_extract(payload, '$.cover'),
                json_extract(payload, '$.banner')
              )
              WHERE cover IS NULL;
            UPDATE series SET rating = json_extract(payload, '$.rating')
              WHERE rating IS NULL AND json_extract(payload, '$.rating') IS NOT NULL;
            UPDATE channels SET logo = COALESCE(
                json_extract(payload, '$.logo'),
                json_extract(payload, '$.tvg_logo')
              )
              WHERE logo IS NULL;
            "#,
        );
        let _ = CatalogDb::meta_set_conn(conn, "slim_cols_v1", "1");
    }
    conn.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_vod_cat ON vod(category_id);
        CREATE INDEX IF NOT EXISTS idx_series_cat ON series(category_id);
        CREATE INDEX IF NOT EXISTS idx_vod_meta ON vod(meta_ok);
        CREATE INDEX IF NOT EXISTS idx_series_meta ON series(meta_ok);
        CREATE INDEX IF NOT EXISTS idx_meta_cache_imdb ON meta_cache(imdb_id);
        CREATE INDEX IF NOT EXISTS idx_epg_start ON epg(start_ts);
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

    let _ = conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS translations (
            cache_key TEXT PRIMARY KEY,
            text TEXT NOT NULL,
            source_lang TEXT,
            fetched_at INTEGER NOT NULL
        );
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

/// Load categories + EPG + counts only (no VOD/series/channel payloads).
pub fn load_spine_blocking(source_ids: &[Uuid]) -> Result<CatalogSpine, String> {
    let db = CatalogDb::open(source_ids).ok_or_else(|| "catalog db open failed".to_string())?;
    db.load_spine_of(source_ids).map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_db(source_id: Uuid) -> CatalogDb {
        let conn = Connection::open_in_memory().expect("memory sqlite");
        migrate_schema(&conn).expect("migrate");
        let mut conns = HashMap::new();
        conns.insert(source_id, conn);
        CatalogDb { conns }
    }

    fn sample_vod(id: &str, name: &str, cat: &str) -> VodItem {
        VodItem {
            id: id.into(),
            name: name.into(),
            stream_url: format!("https://example.test/{id}"),
            poster: Some(format!("https://img.test/{id}.jpg")),
            plot: None,
            year: Some("2020".into()),
            rating: Some("7.5".into()),
            genre: Some("Action".into()),
            imdb_id: None,
            actors: None,
            director: None,
            writer: None,
            runtime: None,
            rated: None,
            awards: None,
            language: None,
            country: None,
            category_id: Some(cat.into()),
            source_id: None,
        }
    }

    #[test]
    fn browse_tiles_vod_reads_poster_columns_not_full_payload() {
        let sid = Uuid::nil();
        let mut db = memory_db(sid);
        let mut part = PlaylistBundle::default();
        part.vod = vec![
            sample_vod("1", "Alpha", "movies"),
            sample_vod("2", "Beta", "movies"),
            sample_vod("3", "Gamma", "docs"),
        ];
        db.apply_source_bundle_progressive(sid, part, None)
            .expect("ingest");

        let page = db.browse_tiles_vod(&[sid], 0, 2, None);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, "1");
        assert_eq!(page[0].art_url.as_deref(), Some("https://img.test/1.jpg"));
        assert_eq!(page[0].year.as_deref(), Some("2020"));
        assert_eq!(page[1].id, "2");

        let cat = db.browse_tiles_vod(&[sid], 0, 10, Some("docs"));
        assert_eq!(cat.len(), 1);
        assert_eq!(cat[0].id, "3");
        assert_eq!(db.count_vod(&[sid], Some("movies")), 2);
        assert_eq!(db.count_vod(&[sid], None), 3);

        let spine = db.load_spine_of(&[sid]).expect("spine");
        assert_eq!(spine.vod_count, 3);
        assert_eq!(spine.channel_count, 0);
        assert_eq!(spine.series_count, 0);
    }

    #[test]
    fn browse_tiles_offset_across_pages() {
        let sid = Uuid::nil();
        let mut db = memory_db(sid);
        let mut part = PlaylistBundle::default();
        part.vod = (0..5)
            .map(|i| sample_vod(&format!("{i}"), &format!("V{i}"), "c"))
            .collect();
        db.apply_source_bundle_progressive(sid, part, None)
            .expect("ingest");
        let page = db.browse_tiles_vod(&[sid], 2, 2, None);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, "2");
        assert_eq!(page[1].id, "3");
    }

    fn rss_kib() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines().find_map(|l| {
                    let rest = l.strip_prefix("VmRSS:")?;
                    rest.split_whitespace().next()?.parse().ok()
                })
            })
            .unwrap_or(0)
    }

    /// Optional probe: `FLUXPLAY_RSS_PROBE=1 cargo test -p fluxplay --lib spine_rss -- --nocapture`
    ///
    /// Simulates browse idle: spine + warmed [`BrowseTileCache`] (plan ≤100 Mo navigation).
    #[test]
    fn spine_rss_far_below_full_bundle() {
        if std::env::var_os("FLUXPLAY_RSS_PROBE").is_none() {
            return;
        }
        use crate::browse_tiles::{BrowseTileCache, BROWSE_TILE_CACHE_CAP, BROWSE_TILE_FETCH_LIMIT};

        let ids: Vec<Uuid> = crate::storage::profiles_dir()
            .read_dir()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Uuid::parse_str(e.file_name().to_str()?).ok())
            .collect();
        assert!(!ids.is_empty(), "no profiles under data_dir");
        let before = rss_kib();
        let spine = load_spine_blocking(&ids).expect("spine");
        let after_spine = rss_kib();
        let db = CatalogDb::open(&ids).expect("db");
        let mut cache = BrowseTileCache::default();
        let mut offset = 0usize;
        let mut filled = 0usize;
        while filled < BROWSE_TILE_CACHE_CAP {
            let page = db.browse_tiles_vod(&ids, offset, BROWSE_TILE_FETCH_LIMIT, None);
            if page.is_empty() {
                break;
            }
            let n = page.len();
            cache.store_contiguous(offset, page);
            filled = filled.saturating_add(n);
            offset = offset.saturating_add(n);
            if n < BROWSE_TILE_FETCH_LIMIT {
                break;
            }
        }
        let after_cache = rss_kib();
        let delta_mib = after_cache.saturating_sub(before) as f64 / 1024.0;
        eprintln!(
            "RSS probe (browse idle): before={before} KiB spine={after_spine} KiB (+{}) \
             tile_cache={after_cache} KiB (+{} / {delta_mib:.1} MiB) \
             counts ch={} vod={} series={} tiles_filled≈{}",
            after_spine.saturating_sub(before),
            after_cache.saturating_sub(after_spine),
            spine.channel_count,
            spine.vod_count,
            spine.series_count,
            filled.min(BROWSE_TILE_CACHE_CAP),
        );
        // Spine + warm tile LRU must stay << full PlaylistBundle (~200–450 MiB).
        assert!(
            after_cache.saturating_sub(before) < 80_000,
            "spine+tile_cache grew {} KiB — expected << full bundle",
            after_cache.saturating_sub(before)
        );
        // Navigation budget from the plan: ~40–60 Mo typical; hard fail only if absurd.
        assert!(
            delta_mib < 100.0,
            "browse idle RSS grew {delta_mib:.1} MiB — plan target ≤100 Mo navigation"
        );
        assert!(spine.vod_count + spine.series_count + spine.channel_count > 0);
    }
}
