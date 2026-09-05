//! Local SQLite catalog — offline-first Single Source of Truth.
//! Tuned PRAGMAs (WAL / NORMAL sync / mmap / page cache) + FTS5 search.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fluxplay_core::models::{
    Category, Channel, ContentKind, EpgProgramme, PlaylistBundle, SeriesItem, VodItem,
};
use rusqlite::{params, Connection, OptionalExtension};
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

/// Skip portal re-sync when last full sync is younger than this.
pub const SYNC_FRESH_SECS: u64 = 12 * 3600;

pub struct CatalogDb {
    conn: Connection,
}

impl CatalogDb {
    pub fn open() -> Option<Self> {
        let path = db_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match Connection::open(&path) {
            Ok(conn) => {
                let db = Self { conn };
                if let Err(e) = db.migrate() {
                    warn!(error = %e, "catalog db migrate failed");
                    return None;
                }
                info!(path = %path.display(), "catalog db open");
                Some(db)
            }
            Err(e) => {
                warn!(error = %e, "catalog db open failed");
                None
            }
        }
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        // Performance PRAGMAs (cj.rs / SQLite cheatsheet): WAL + NORMAL + page cache + mmap.
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA temp_store=MEMORY;
            PRAGMA busy_timeout=5000;
            PRAGMA cache_size=-32768;
            PRAGMA mmap_size=268435456;
            PRAGMA foreign_keys=OFF;
            PRAGMA analysis_limit=400;
            "#,
        )?;

        self.conn.execute_batch(
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
        let _ = self.conn.execute_batch(
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
        let _ = self.conn.execute_batch(
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
        let _ = self
            .conn
            .execute("ALTER TABLE vod ADD COLUMN meta_ok INTEGER NOT NULL DEFAULT 0", []);
        let _ = self
            .conn
            .execute("ALTER TABLE series ADD COLUMN meta_ok INTEGER NOT NULL DEFAULT 0", []);

        Ok(())
    }

    pub fn meta_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn meta_set(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn last_full_sync_secs(&self) -> Option<u64> {
        self.meta_get("last_full_sync")?.parse().ok()
    }

    pub fn mark_full_sync_now(&self) {
        let now = now_secs().to_string();
        let _ = self.meta_set("last_full_sync", &now);
        let _ = self.conn.execute_batch("PRAGMA optimize;");
    }

    /// True when local catalog is warm enough to skip portal re-sync.
    pub fn is_sync_fresh(&self) -> bool {
        let Some(ts) = self.last_full_sync_secs() else {
            debug!("catalog sync not fresh — no last_full_sync");
            return false;
        };
        let (ch, vod, series) = self.counts();
        if ch + vod + series == 0 {
            debug!("catalog sync not fresh — empty");
            return false;
        }
        let fresh = now_secs().saturating_sub(ts) < SYNC_FRESH_SECS;
        debug!(fresh, age_secs = now_secs().saturating_sub(ts), ch, vod, series, "catalog sync freshness");
        fresh
    }

    pub fn delete_source(&self, source_id: Uuid) -> rusqlite::Result<()> {
        let sid = source_id.to_string();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM categories WHERE source_id = ?1", params![sid])?;
        tx.execute("DELETE FROM channels WHERE source_id = ?1", params![sid])?;
        tx.execute("DELETE FROM vod WHERE source_id = ?1", params![sid])?;
        tx.execute("DELETE FROM series WHERE source_id = ?1", params![sid])?;
        tx.commit()?;
        debug!(%source_id, "catalog source deleted");
        Ok(())
    }

    pub fn replace_source_bundle(
        &self,
        source_id: Uuid,
        part: &PlaylistBundle,
    ) -> rusqlite::Result<()> {
        let sid = source_id.to_string();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM categories WHERE source_id = ?1", params![sid])?;
        tx.execute("DELETE FROM channels WHERE source_id = ?1", params![sid])?;
        tx.execute("DELETE FROM vod WHERE source_id = ?1", params![sid])?;
        tx.execute("DELETE FROM series WHERE source_id = ?1", params![sid])?;

        {
            let mut stmt = tx.prepare(
                "INSERT INTO categories (source_id, id, name, content) VALUES (?1,?2,?3,?4)",
            )?;
            for c in &part.categories {
                let content = match c.content {
                    ContentKind::Live => "live",
                    ContentKind::Vod => "vod",
                    ContentKind::Series => "series",
                };
                stmt.execute(params![sid, c.id, c.name, content])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO channels (source_id, id, payload, name) VALUES (?1,?2,?3,?4)",
            )?;
            for ch in &part.channels {
                let payload = serde_json::to_string(ch).unwrap_or_default();
                stmt.execute(params![sid, ch.id, payload, ch.name])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO vod (source_id, id, payload, name, year, genre, category_id, meta_ok)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?;
            for v in &part.vod {
                let payload = serde_json::to_string(v).unwrap_or_default();
                let meta_ok = if v.genre.is_some() && v.year.is_some() {
                    1i64
                } else {
                    0
                };
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
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO series (source_id, id, payload, name, year, genre, category_id, meta_ok)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?;
            for s in &part.series {
                let payload = serde_json::to_string(s).unwrap_or_default();
                let meta_ok = if s.genre.is_some() && s.year.is_some() {
                    1i64
                } else {
                    0
                };
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
        }
        tx.commit()?;

        // Rebuild FTS indexes cheaply (content= external tables).
        let _ = self.conn.execute_batch(
            "INSERT INTO vod_fts(vod_fts) VALUES('rebuild');
             INSERT INTO series_fts(series_fts) VALUES('rebuild');",
        );

        info!(
            %source_id,
            channels = part.channels.len(),
            vod = part.vod.len(),
            series = part.series.len(),
            "catalog db replaced source"
        );
        Ok(())
    }

    pub fn merge_epg(&self, programmes: &[EpgProgramme]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
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

        {
            let mut stmt = self
                .conn
                .prepare("SELECT id, name, content FROM categories")?;
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
            let mut stmt = self.conn.prepare("SELECT payload FROM channels")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for raw in rows.flatten() {
                if let Ok(ch) = serde_json::from_str::<Channel>(&raw) {
                    bundle.channels.push(ch);
                }
            }
        }
        {
            let mut stmt = self.conn.prepare("SELECT payload FROM vod")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for raw in rows.flatten() {
                if let Ok(v) = serde_json::from_str::<VodItem>(&raw) {
                    bundle.vod.push(v);
                }
            }
        }
        {
            let mut stmt = self.conn.prepare("SELECT payload FROM series")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for raw in rows.flatten() {
                if let Ok(s) = serde_json::from_str::<SeriesItem>(&raw) {
                    bundle.series.push(s);
                }
            }
        }
        // EPG: only keep recent-ish rows to bound RAM (last ~2 days of short EPG).
        {
            let mut stmt = self.conn.prepare(
                "SELECT payload FROM epg ORDER BY start_ts DESC LIMIT 8000",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for raw in rows.flatten() {
                if let Ok(p) = serde_json::from_str::<EpgProgramme>(&raw) {
                    bundle.epg.push(p);
                }
            }
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
        let Ok(mut stmt) = self.conn.prepare(&sql) else {
            return None;
        };
        let payloads: Vec<String> = if let Some(cid) = category_id {
            let mut rows = stmt.query(params![match_q, cid, limit as i64]).ok()?;
            let mut collected = Vec::new();
            while let Ok(Some(row)) = rows.next() {
                if let Ok(raw) = row.get::<_, String>(0) {
                    collected.push(raw);
                }
            }
            collected
        } else {
            let mut rows = stmt.query(params![match_q, limit as i64]).ok()?;
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
        Some(out)
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
        let payloads = match category_id {
            Some(cid) => {
                let sql = format!(
                    "SELECT payload FROM {table} WHERE lower(name) LIKE ?1 AND category_id = ?2 LIMIT ?3"
                );
                let Ok(mut stmt) = self.conn.prepare(&sql) else {
                    return out;
                };
                let mut rows = match stmt.query(params![q, cid, limit as i64]) {
                    Ok(r) => r,
                    Err(_) => return out,
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
                let Ok(mut stmt) = self.conn.prepare(&sql) else {
                    return out;
                };
                let mut rows = match stmt.query(params![q, limit as i64]) {
                    Ok(r) => r,
                    Err(_) => return out,
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
        out
    }

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
        let raw: Option<String> = self
            .conn
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
        self.conn.execute(
            "UPDATE vod SET payload = ?1, year = ?2, genre = ?3, meta_ok = 1
             WHERE source_id = ?4 AND id = ?5",
            params![payload, item.year, item.genre, sid, item.id],
        )?;
        Ok(())
    }

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
        let raw: Option<String> = self
            .conn
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
        self.conn.execute(
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
        let row: Result<(i64, String, i64), _> = self.conn.query_row(
            "SELECT is_miss, payload, fetched_at FROM meta_cache WHERE cache_key = ?1",
            params![cache_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        );
        let Ok((is_miss, payload, fetched_at)) = row else {
            return None;
        };
        let age = now_secs().saturating_sub(fetched_at as u64);
        if is_miss != 0 {
            if age > 7 * 86400 {
                return None; // allow retry
            }
            return Some((true, crate::metadata::MetaPatch::default()));
        }
        let patch = serde_json::from_str(&payload).unwrap_or_default();
        Some((false, patch))
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
        let _ = self.conn.execute(
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

    pub fn meta_cache_get_imdb(
        &self,
        imdb_id: &str,
    ) -> Option<crate::metadata::MetaPatch> {
        let payload: Result<String, _> = self.conn.query_row(
            "SELECT payload FROM meta_cache WHERE imdb_id = ?1 AND is_miss = 0 LIMIT 1",
            params![imdb_id],
            |r| r.get(0),
        );
        payload.ok().and_then(|p| serde_json::from_str(&p).ok())
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let ch: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))
            .unwrap_or(0);
        let vod: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM vod", [], |r| r.get(0))
            .unwrap_or(0);
        let series: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM series", [], |r| r.get(0))
            .unwrap_or(0);
        (ch as usize, vod as usize, series as usize)
    }
}

fn db_path() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        if let Some(app) = iced::android::ANDROID_APP.get() {
            if let Some(base) = app.internal_data_path() {
                return base.join("fluxplay").join("catalog.sqlite3");
            }
        }
    }
    dirs::data_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fluxplay")
        .join("catalog.sqlite3")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
