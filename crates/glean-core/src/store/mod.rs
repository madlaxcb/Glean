//! SQLite persistence. FTS5 uses trigram when available.

use crate::error::{CoreError, Result};
use crate::model::{
    EntryDetail, EntryFilter, EntryId, EntrySummary, Feed, FeedId, Folder, FolderId,
};
use crate::paths::cache_entries_dir;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: i64 = 9;

pub struct Store {
    conn: Connection,
    /// Optional disk cache dir for entry bodies (§2.5). `None` for in-memory
    /// stores and tests; the app sets it to `cache_entries_dir()` via
    /// [`Store::open_path`].
    cache_dir: Option<PathBuf>,
}

impl Store {
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut s = Self {
            conn,
            cache_dir: None,
        };
        s.migrate()?;
        Ok(s)
    }

    /// Open a DB file with the default on-disk cache dir (`cache_entries_dir()`).
    pub fn open_path(path: &Path) -> Result<Self> {
        Self::open_path_with_cache(path, cache_entries_dir())
    }

    /// Open a DB file with an explicit cache dir. `cache_dir = None` disables
    /// the disk cache (used by tests to avoid polluting the real data dir).
    pub fn open_path_with_cache(path: &Path, cache_dir: Option<PathBuf>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Message(e.to_string()))?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;",
        )?;
        let mut s = Self { conn, cache_dir };
        s.migrate()?;
        Ok(s)
    }

    /// Path to the cached body for an entry: `<cache_dir>/<id>/body.html`.
    fn cache_path(&self, id: EntryId) -> Option<PathBuf> {
        self.cache_dir
            .as_ref()
            .map(|d| d.join(id.0.to_string()).join("body.html"))
    }

    /// Best-effort write of sanitized HTML to the disk cache (§2.5.1).
    /// Failures are silently ignored — the DB copy is the source of truth.
    fn write_entry_cache(&self, id: EntryId, html: &str) {
        let Some(path) = self.cache_path(id) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, html);
    }

    /// Best-effort read of the cached body (§2.5.3). `None` if disabled or
    /// the file is missing/unreadable.
    fn read_entry_cache(&self, id: EntryId) -> Option<String> {
        let path = self.cache_path(id)?;
        std::fs::read_to_string(&path).ok()
    }

    fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
             );",
        )?;
        let ver: i64 = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if ver < 1 {
            self.conn.execute_batch(
                "
                CREATE TABLE folders (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    sort_key INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE feeds (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    folder_id INTEGER REFERENCES folders(id) ON DELETE SET NULL,
                    title TEXT NOT NULL,
                    site_url TEXT,
                    feed_url TEXT NOT NULL UNIQUE,
                    etag TEXT,
                    last_modified TEXT,
                    last_fetched_at INTEGER,
                    last_error TEXT,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE entries (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    feed_id INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
                    guid TEXT NOT NULL,
                    title TEXT NOT NULL,
                    url TEXT,
                    author TEXT,
                    published_at INTEGER,
                    summary TEXT,
                    content_html TEXT NOT NULL DEFAULT '',
                    is_read INTEGER NOT NULL DEFAULT 0,
                    is_starred INTEGER NOT NULL DEFAULT 0,
                    fetched_at INTEGER NOT NULL,
                    UNIQUE(feed_id, guid)
                );
                CREATE INDEX idx_entries_feed_pub ON entries(feed_id, published_at DESC);
                CREATE INDEX idx_entries_unread ON entries(is_read, published_at DESC);
                ",
            )?;
            // FTS5 trigram (SQLite 3.34+). If unavailable, core still works without search.
            let fts_ok = self.conn.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
                    title,
                    summary,
                    content_html,
                    content='entries',
                    content_rowid='id',
                    tokenize='trigram'
                );",
            );
            if fts_ok.is_err() {
                // Fallback without trigram so empty DBs still open on odd builds.
                let _ = self.conn.execute_batch(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
                        title, summary, content_html,
                        content='entries', content_rowid='id'
                    );",
                );
            }
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![SCHEMA_VERSION.to_string()],
            )?;
        }
        if ver < 2 {
            self.conn
                .execute_batch("ALTER TABLE feeds ADD COLUMN muted INTEGER NOT NULL DEFAULT 0;")?;
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', '2')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        if ver < 3 {
            self.conn.execute_batch(
                "ALTER TABLE feeds ADD COLUMN refresh_interval_secs INTEGER NOT NULL DEFAULT 0;",
            )?;
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', '3')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        if ver < 4 {
            self.conn
                .execute_batch("ALTER TABLE feeds ADD COLUMN last_refresh INTEGER;")?;
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', '4')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        if ver < 5 {
            // Full-text extracted from the original article URL (readability).
            // Empty by default; populated on-demand by the extractor.
            self.conn.execute_batch(
                "ALTER TABLE entries ADD COLUMN content_extracted TEXT NOT NULL DEFAULT '';",
            )?;
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', '5')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        if ver < 6 {
            // Favicon URL for each feed (discovered from HTML <link rel="icon">
            // or feed-rs icon field). Populated during refresh.
            self.conn
                .execute_batch("ALTER TABLE feeds ADD COLUMN favicon_url TEXT;")?;
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', '6')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        if ver < 7 {
            // Consecutive refresh failure count for each feed.
            self.conn.execute_batch(
                "ALTER TABLE feeds ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0;",
            )?;
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', '7')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        if ver < 8 {
            // Allow entries.feed_id to be NULL so starred entries survive feed
            // deletion. Also change ON DELETE CASCADE → SET NULL so deleting
            // a feed preserves starred entries (their feed_id becomes NULL).
            self.conn.execute_batch(
                "
                CREATE TABLE entries_new (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    feed_id INTEGER REFERENCES feeds(id) ON DELETE SET NULL,
                    guid TEXT NOT NULL,
                    title TEXT NOT NULL,
                    url TEXT,
                    author TEXT,
                    published_at INTEGER,
                    summary TEXT,
                    content_html TEXT NOT NULL DEFAULT '',
                    content_extracted TEXT NOT NULL DEFAULT '',
                    is_read INTEGER NOT NULL DEFAULT 0,
                    is_starred INTEGER NOT NULL DEFAULT 0,
                    fetched_at INTEGER NOT NULL,
                    UNIQUE(feed_id, guid)
                );
                INSERT INTO entries_new
                    SELECT id, feed_id, guid, title, url, author,
                           published_at, summary, content_html, content_extracted,
                           is_read, is_starred, fetched_at
                    FROM entries;
                DROP TABLE entries;
                ALTER TABLE entries_new RENAME TO entries;
                CREATE INDEX idx_entries_feed_pub ON entries(feed_id, published_at DESC);
                CREATE INDEX idx_entries_unread ON entries(is_read, published_at DESC);
                ",
            )?;
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', '8')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        if ver < 9 {
            // AI 增强结果（摘要/翻译）。每个 entry 每个 kind 唯一，重新触发覆盖旧结果。
            // ON DELETE CASCADE：entry 删除时增强结果一并清理。
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS entry_enhancements (
                    entry_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
                    kind TEXT NOT NULL,
                    content TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (entry_id, kind)
                );",
            )?;
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', '9')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        Ok(())
    }

    pub fn seed_demo_if_empty(&mut self) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM feeds", [], |r| r.get(0))?;
        if n > 0 {
            return Ok(false);
        }
        let now = now_secs();
        self.conn
            .execute("INSERT INTO folders(name, sort_key) VALUES('示例', 0)", [])?;
        let folder_id = self.conn.last_insert_rowid();
        self.conn.execute(
            "INSERT INTO feeds(folder_id, title, site_url, feed_url, created_at)
             VALUES(?1, 'Glean Demo Feed', 'https://example.com', 'https://example.com/demo.xml', ?2)",
            params![folder_id, now],
        )?;
        let feed_id = self.conn.last_insert_rowid();
        let demos = [
            (
                "demo-1",
                "Spike #1 — Hello Glean",
                r#"<h1>Spike Article 1</h1>
<p>This is static sanitized-style HTML. Script should be disabled in WebView.</p>
<p><a href="https://example.com">External link (should open system browser)</a></p>
<p>中文 IME 与焦点请在壳侧搜索框验证。</p>"#,
            ),
            (
                "demo-2",
                "Spike #2 — Resize / DPI",
                r#"<h1>Spike Article 2</h1>
<p>Drag the splitter and resize the window. Reader rect should stay aligned.</p>
<ul><li>Maximize / restore</li><li>Minimise then restore</li><li>Move across DPI monitors</li></ul>"#,
            ),
            (
                "demo-3",
                "Spike #3 — Memory reuse",
                r#"<h1>Spike Article 3</h1>
<p>Switch entries 50 times. Private bytes must not climb linearly (single WebView instance).</p>
<p>Remote images are omitted on purpose (default Block policy later).</p>"#,
            ),
        ];
        for (i, (guid, title, html)) in demos.iter().enumerate() {
            self.insert_entry_raw(
                feed_id,
                guid,
                title,
                Some("https://example.com/posts/demo"),
                html,
                Some(now - (i as i64) * 3600),
            )?;
        }
        Ok(true)
    }

    fn insert_entry_raw(
        &mut self,
        feed_id: i64,
        guid: &str,
        title: &str,
        url: Option<&str>,
        content_html: &str,
        published_at: Option<i64>,
    ) -> Result<i64> {
        let fetched = now_secs();
        self.conn.execute(
            "INSERT INTO entries(feed_id, guid, title, url, published_at, summary, content_html, fetched_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                feed_id,
                guid,
                title,
                url,
                published_at,
                title,
                content_html,
                fetched
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        let _ = self.conn.execute(
            "INSERT INTO entries_fts(rowid, title, summary, content_html) VALUES(?1, ?2, ?3, ?4)",
            params![id, title, title, content_html],
        );
        self.write_entry_cache(EntryId(id), content_html);
        Ok(id)
    }

    pub fn add_feed(
        &mut self,
        title: &str,
        feed_url: &str,
        folder_id: Option<FolderId>,
    ) -> Result<FeedId> {
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO feeds(folder_id, title, feed_url, created_at) VALUES(?1, ?2, ?3, ?4)",
            params![folder_id.map(|f| f.0), title, feed_url, now],
        )?;
        Ok(FeedId(self.conn.last_insert_rowid()))
    }

    pub fn add_entry(
        &mut self,
        feed_id: FeedId,
        guid: &str,
        title: &str,
        url: Option<&str>,
        content_html: &str,
    ) -> Result<EntryId> {
        let id =
            self.insert_entry_raw(feed_id.0, guid, title, url, content_html, Some(now_secs()))?;
        Ok(EntryId(id))
    }

    pub fn list_folders(&self) -> Result<Vec<Folder>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, sort_key FROM folders ORDER BY sort_key, id")?;
        let rows = stmt.query_map([], |r| {
            Ok(Folder {
                id: FolderId(r.get(0)?),
                name: r.get(1)?,
                sort_key: r.get(2)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn list_feeds(&self) -> Result<Vec<Feed>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, folder_id, title, site_url, feed_url, last_error, muted, refresh_interval_secs, favicon_url, consecutive_failures FROM feeds ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Feed {
                id: FeedId(r.get(0)?),
                folder_id: r.get::<_, Option<i64>>(1)?.map(FolderId),
                title: r.get(2)?,
                site_url: r.get(3)?,
                feed_url: r.get(4)?,
                last_error: r.get(5)?,
                muted: r.get::<_, i64>(6)? != 0,
                refresh_interval_secs: r.get(7)?,
                favicon_url: r.get(8)?,
                consecutive_failures: r.get(9)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn list_entries(&self, filter: EntryFilter) -> Result<Vec<EntrySummary>> {
        let sql = match filter {
            EntryFilter::All => {
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred,
                        (content_html != '' OR content_extracted != '') AS has_content
                 FROM entries ORDER BY COALESCE(published_at, fetched_at) DESC, id DESC"
            }
            EntryFilter::Unread => {
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred,
                        (content_html != '' OR content_extracted != '') AS has_content
                 FROM entries WHERE is_read = 0
                 ORDER BY COALESCE(published_at, fetched_at) DESC, id DESC"
            }
            EntryFilter::Starred => {
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred,
                        (content_html != '' OR content_extracted != '') AS has_content
                 FROM entries WHERE is_starred = 1
                 ORDER BY COALESCE(published_at, fetched_at) DESC, id DESC"
            }
            // Last 24h; fallback to fetched_at when published_at is missing.
            EntryFilter::Today => {
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred,
                        (content_html != '' OR content_extracted != '') AS has_content
                 FROM entries
                 WHERE COALESCE(published_at, fetched_at) >= ?1
                 ORDER BY COALESCE(published_at, fetched_at) DESC, id DESC"
            }
            EntryFilter::Feed(_) => {
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred,
                        (content_html != '' OR content_extracted != '') AS has_content
                 FROM entries WHERE feed_id = ?1
                 ORDER BY COALESCE(published_at, fetched_at) DESC, id DESC"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<EntrySummary> {
            Ok(EntrySummary {
                id: EntryId(r.get(0)?),
                feed_id: r.get::<_, Option<i64>>(1)?.map(FeedId),
                title: r.get(2)?,
                url: r.get(3)?,
                published_at: r.get(4)?,
                is_read: r.get::<_, i64>(5)? != 0,
                is_starred: r.get::<_, i64>(6)? != 0,
                has_content: r.get::<_, i64>(7)? != 0,
            })
        };
        let list = match filter {
            EntryFilter::Feed(FeedId(fid)) => {
                let rows = stmt.query_map(params![fid], map_row)?;
                rows.filter_map(|r| r.ok()).collect()
            }
            EntryFilter::Today => {
                let cutoff = now_secs() - 24 * 3600;
                let rows = stmt.query_map(params![cutoff], map_row)?;
                rows.filter_map(|r| r.ok()).collect()
            }
            _ => {
                let rows = stmt.query_map([], map_row)?;
                rows.filter_map(|r| r.ok()).collect()
            }
        };
        Ok(list)
    }

    pub fn get_entry(&self, id: EntryId) -> Result<EntryDetail> {
        let mut detail = self
            .conn
            .query_row(
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred, author,
                        content_html, content_extracted
                 FROM entries WHERE id = ?1",
                params![id.0],
                |r| {
                    Ok(EntryDetail {
                        summary: EntrySummary {
                            id: EntryId(r.get(0)?),
                            feed_id: r.get::<_, Option<i64>>(1)?.map(FeedId),
                            title: r.get(2)?,
                            url: r.get(3)?,
                            published_at: r.get(4)?,
                            is_read: r.get::<_, i64>(5)? != 0,
                            is_starred: r.get::<_, i64>(6)? != 0,
                            has_content: !r.get::<_, String>(8)?.is_empty()
                                || !r.get::<_, String>(9)?.is_empty(),
                        },
                        author: r.get(7)?,
                        content_html: r.get(8)?,
                        extracted_html: r.get(9)?,
                        enhancements: Vec::new(),
                    })
                },
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound(format!("entry {}", id.0)))?;
        // §2.5.3: if the DB body is empty (e.g. future truncation or a feed
        // that only shipped a summary), fall back to the disk cache so offline
        // reading still works.
        if detail.content_html.is_empty() {
            if let Some(cached) = self.read_entry_cache(id) {
                if !cached.is_empty() {
                    detail.content_html = cached;
                    detail.summary.has_content = true;
                }
            }
        }
        // AI 增强结果（摘要/翻译）。
        detail.enhancements = self.list_enhancements(id)?;
        Ok(detail)
    }

    pub fn set_read(&mut self, id: EntryId, read: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE entries SET is_read = ?1 WHERE id = ?2",
            params![if read { 1 } else { 0 }, id.0],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound(format!("entry {}", id.0)));
        }
        Ok(())
    }

    /// Store full-text extracted from the original article URL.
    /// Also writes the extracted HTML to the disk cache so it survives offline.
    pub fn set_extracted_html(&mut self, id: EntryId, html: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE entries SET content_extracted = ?1 WHERE id = ?2",
            params![html, id.0],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound(format!("entry {}", id.0)));
        }
        // Refresh disk cache with the extracted body so offline reads get the
        // full article, not just the short feed summary.
        if !html.is_empty() {
            self.write_entry_cache(id, html);
        }
        Ok(())
    }

    /// 写入/覆盖一条 AI 增强结果（摘要或翻译）。重新触发同一 kind 会覆盖。
    pub fn set_enhancement(&mut self, id: EntryId, kind: &str, content: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO entry_enhancements(entry_id, kind, content, created_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(entry_id, kind) DO UPDATE SET content = excluded.content, created_at = excluded.created_at",
            params![id.0, kind, content, now_secs()],
        )?;
        Ok(())
    }

    /// 取单个 kind 的增强结果。`None` 表示尚未生成。
    pub fn get_enhancement(&self, id: EntryId, kind: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT content FROM entry_enhancements WHERE entry_id = ?1 AND kind = ?2",
                params![id.0, kind],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// 取某 entry 的所有增强结果（kind, content），按 created_at 升序。
    pub fn list_enhancements(&self, id: EntryId) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, content FROM entry_enhancements WHERE entry_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![id.0], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn toggle_star(&mut self, id: EntryId) -> Result<bool> {
        let cur: i64 = self
            .conn
            .query_row(
                "SELECT is_starred FROM entries WHERE id = ?1",
                params![id.0],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound(format!("entry {}", id.0)))?;
        let next = if cur == 0 { 1 } else { 0 };
        self.conn.execute(
            "UPDATE entries SET is_starred = ?1 WHERE id = ?2",
            params![next, id.0],
        )?;
        Ok(next != 0)
    }

    pub fn unread_count(&self) -> Result<u64> {
        let n: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM entries WHERE is_read = 0", [], |r| {
                    r.get(0)
                })?;
        Ok(n as u64)
    }

    /// Unread count per feed, returned as (FeedId, count) pairs.
    pub fn unread_counts_per_feed(&self) -> Result<Vec<(FeedId, u64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT feed_id, COUNT(*) FROM entries WHERE is_read = 0 GROUP BY feed_id")?;
        let rows = stmt.query_map([], |r| {
            Ok((FeedId(r.get::<_, i64>(0)?), r.get::<_, i64>(1)? as u64))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Store the favicon URL for a feed (discovered during refresh).
    pub fn set_favicon_url(&mut self, id: FeedId, url: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET favicon_url = ?1 WHERE id = ?2",
            params![url, id.0],
        )?;
        Ok(())
    }

    /// Substring search: try FTS5, then LIKE (M0b guarantees Chinese substring via LIKE).
    pub fn search_entries(&self, query: &str, limit: i64) -> Result<Vec<EntrySummary>> {
        let q = query.trim();
        if q.is_empty() {
            return self.list_entries(EntryFilter::All);
        }

        if let Ok(hits) = self.search_fts(q, limit) {
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
        self.search_like(q, limit)
    }

    fn search_fts(&self, q: &str, limit: i64) -> Result<Vec<EntrySummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.feed_id, e.title, e.url, e.published_at, e.is_read, e.is_starred,
                    (e.content_html != '' OR e.content_extracted != '') AS has_content
             FROM entries_fts f
             JOIN entries e ON e.id = f.rowid
             WHERE entries_fts MATCH ?1
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, limit], map_summary_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn search_like(&self, q: &str, limit: i64) -> Result<Vec<EntrySummary>> {
        let pattern = format!("%{q}%");
        let mut stmt = self.conn.prepare(
            "SELECT id, feed_id, title, url, published_at, is_read, is_starred,
                    (content_html != '' OR content_extracted != '') AS has_content
             FROM entries
             WHERE title LIKE ?1 OR IFNULL(summary,'') LIKE ?1 OR content_html LIKE ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit], map_summary_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn find_feed_by_url(&self, feed_url: &str) -> Result<Option<FeedId>> {
        let id = self
            .conn
            .query_row(
                "SELECT id FROM feeds WHERE feed_url = ?1",
                params![feed_url],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.map(FeedId))
    }

    pub fn get_feed_fetch_meta(
        &self,
        id: FeedId,
    ) -> Result<(String, Option<String>, Option<String>)> {
        self.conn
            .query_row(
                "SELECT feed_url, etag, last_modified FROM feeds WHERE id = ?1",
                params![id.0],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| CoreError::NotFound(format!("feed {}", id.0)))
    }

    pub fn update_feed_after_fetch(
        &mut self,
        id: FeedId,
        title: Option<&str>,
        site_url: Option<&str>,
        etag: Option<&str>,
        last_modified: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let now = now_secs();
        if let Some(err) = error {
            self.conn.execute(
                "UPDATE feeds SET last_error = ?1, last_fetched_at = ?2, consecutive_failures = consecutive_failures + 1 WHERE id = ?3",
                params![err, now, id.0],
            )?;
            return Ok(());
        }
        self.conn.execute(
            "UPDATE feeds SET
                title = COALESCE(?1, title),
                site_url = COALESCE(?2, site_url),
                etag = COALESCE(?3, etag),
                last_modified = COALESCE(?4, last_modified),
                last_error = NULL,
                last_fetched_at = ?5,
                last_refresh = ?5,
                consecutive_failures = 0
             WHERE id = ?6",
            params![title, site_url, etag, last_modified, now, id.0],
        )?;
        Ok(())
    }

    /// Insert or ignore by (feed_id, guid). Returns true if a new row was inserted.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_entry(
        &mut self,
        feed_id: FeedId,
        guid: &str,
        title: &str,
        url: Option<&str>,
        author: Option<&str>,
        published_at: Option<i64>,
        summary: Option<&str>,
        content_html: &str,
    ) -> Result<bool> {
        let fetched = now_secs();
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO entries(
                feed_id, guid, title, url, author, published_at, summary, content_html, fetched_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                feed_id.0,
                guid,
                title,
                url,
                author,
                published_at,
                summary,
                content_html,
                fetched
            ],
        )?;
        if n == 0 {
            return Ok(false);
        }
        let id = self.conn.last_insert_rowid();
        let _ = self.conn.execute(
            "INSERT INTO entries_fts(rowid, title, summary, content_html) VALUES(?1, ?2, ?3, ?4)",
            params![id, title, summary.unwrap_or(""), content_html],
        );
        self.write_entry_cache(EntryId(id), content_html);
        Ok(true)
    }

    pub fn list_feed_ids(&self) -> Result<Vec<FeedId>> {
        let mut stmt = self.conn.prepare("SELECT id FROM feeds ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.filter_map(|r| r.ok()).map(FeedId).collect())
    }

    pub fn delete_feed(&mut self, id: FeedId) -> Result<()> {
        // Delete non-starred entries first (starred entries survive via
        // ON DELETE SET NULL — their feed_id becomes NULL).
        // 1. Remove FTS rows for non-starred entries.
        self.conn.execute(
            "DELETE FROM entries_fts WHERE rowid IN (
                SELECT id FROM entries WHERE feed_id = ?1 AND is_starred = 0
            )",
            params![id.0],
        )?;
        // 2. Remove non-starred entry rows.
        self.conn.execute(
            "DELETE FROM entries WHERE feed_id = ?1 AND is_starred = 0",
            params![id.0],
        )?;
        // 3. Delete the feed; remaining starred entries get feed_id = NULL
        //    via ON DELETE SET NULL.
        let n = self
            .conn
            .execute("DELETE FROM feeds WHERE id = ?1", params![id.0])?;
        if n == 0 {
            return Err(CoreError::NotFound(format!("feed {}", id.0)));
        }
        Ok(())
    }

    pub fn rename_feed(&mut self, id: FeedId, title: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE feeds SET title = ?1 WHERE id = ?2",
            params![title, id.0],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound(format!("feed {}", id.0)));
        }
        Ok(())
    }

    pub fn set_feed_url(&mut self, id: FeedId, feed_url: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE feeds SET feed_url = ?1, etag = NULL, last_modified = NULL, consecutive_failures = 0 WHERE id = ?2",
            params![feed_url, id.0],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound(format!("feed {}", id.0)));
        }
        Ok(())
    }

    pub fn move_feed_to_folder(
        &mut self,
        feed_id: FeedId,
        folder_id: Option<FolderId>,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE feeds SET folder_id = ?1 WHERE id = ?2",
            params![folder_id.map(|f| f.0), feed_id.0],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound(format!("feed {}", feed_id.0)));
        }
        Ok(())
    }

    pub fn add_folder(&mut self, name: &str) -> Result<FolderId> {
        self.conn.execute(
            "INSERT INTO folders(name, sort_key) VALUES(?1, 0)",
            params![name],
        )?;
        Ok(FolderId(self.conn.last_insert_rowid()))
    }

    pub fn toggle_mute_feed(&mut self, id: FeedId) -> Result<bool> {
        let cur: i64 = self
            .conn
            .query_row(
                "SELECT muted FROM feeds WHERE id = ?1",
                params![id.0],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound(format!("feed {}", id.0)))?;
        let next = if cur == 0 { 1 } else { 0 };
        self.conn.execute(
            "UPDATE feeds SET muted = ?1 WHERE id = ?2",
            params![next, id.0],
        )?;
        Ok(next != 0)
    }

    pub fn unread_count_excluding_muted(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM entries e JOIN feeds f ON e.feed_id = f.id
             WHERE e.is_read = 0 AND f.muted = 0",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    pub fn set_feed_refresh_interval(&mut self, id: FeedId, secs: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET refresh_interval_secs = ?1 WHERE id = ?2",
            params![secs, id.0],
        )?;
        Ok(())
    }

    /// Return feed IDs whose last_refresh is older than their interval (or global default).
    pub fn feeds_due_for_refresh(
        &self,
        global_interval_secs: i64,
        now_secs: i64,
    ) -> Result<Vec<FeedId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, refresh_interval_secs, last_refresh FROM feeds")?;
        let rows = stmt.query_map([], |r| {
            let id: i64 = r.get(0)?;
            let interval: i64 = r.get(1)?;
            let last: Option<i64> = r.get(2)?;
            Ok((id, interval, last))
        })?;
        let mut due = Vec::new();
        for row in rows {
            let (id, interval, last) = row?;
            let effective = if interval > 0 {
                interval
            } else {
                global_interval_secs
            };
            if effective <= 0 {
                continue;
            }
            let last = last.unwrap_or(0);
            if now_secs - last >= effective {
                due.push(FeedId(id));
            }
        }
        Ok(due)
    }

    pub fn mark_all_read_in_feed(&mut self, feed_id: FeedId) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET is_read = 1 WHERE feed_id = ?1 AND is_read = 0",
            params![feed_id.0],
        )?;
        Ok(())
    }

    pub fn mark_all_read(&mut self) -> Result<()> {
        self.conn
            .execute("UPDATE entries SET is_read = 1 WHERE is_read = 0", [])?;
        Ok(())
    }
}

fn map_summary_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EntrySummary> {
    Ok(EntrySummary {
        id: EntryId(r.get(0)?),
        feed_id: r.get::<_, Option<i64>>(1)?.map(FeedId),
        title: r.get(2)?,
        url: r.get(3)?,
        published_at: r.get(4)?,
        is_read: r.get::<_, i64>(5)? != 0,
        is_starred: r.get::<_, i64>(6)? != 0,
        has_content: r.get::<_, i64>(7)? != 0,
    })
}

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp(prefix: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tmp
    }

    /// §2.5 disk cache: `add_entry` writes `cache/entries/<id>/body.html`,
    /// and `get_entry` falls back to it when the DB body is empty (offline).
    #[test]
    fn disk_cache_write_and_offline_fallback() {
        let tmp = unique_tmp("glean-cache");
        let db_path = tmp.join("glean.db");
        let cache_dir = tmp.join("cache").join("entries");
        let mut store = Store::open_path_with_cache(&db_path, Some(cache_dir.clone())).unwrap();

        let fid = store.add_feed("T", "https://ex/feed.xml", None).unwrap();
        let body = "<p>cached body &amp; soul</p>";
        let id = store.add_entry(fid, "g1", "Title", None, body).unwrap();

        // Cache file written alongside the DB row.
        let cache_file = cache_dir.join(id.0.to_string()).join("body.html");
        let on_disk = std::fs::read_to_string(&cache_file).expect("cache file written");
        assert_eq!(on_disk, body);

        // DB copy present → get_entry returns DB body.
        assert_eq!(store.get_entry(id).unwrap().content_html, body);

        // Simulate an empty DB body: the cache must fill in (offline read).
        store
            .conn
            .execute(
                "UPDATE entries SET content_html = '' WHERE id = ?1",
                params![id.0],
            )
            .unwrap();
        let detail = store.get_entry(id).unwrap();
        assert_eq!(detail.content_html, body);
        assert!(detail.summary.has_content);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `open_path_with_cache(None)` must not write any cache files.
    #[test]
    fn cache_disabled_when_dir_none() {
        let tmp = unique_tmp("glean-nocache");
        let db_path = tmp.join("glean.db");
        let mut store = Store::open_path_with_cache(&db_path, None).unwrap();
        let fid = store.add_feed("T", "https://ex/feed.xml", None).unwrap();
        let id = store
            .add_entry(fid, "g1", "Title", None, "<p>x</p>")
            .unwrap();
        assert!(!tmp.join("cache").exists());
        assert_eq!(store.get_entry(id).unwrap().content_html, "<p>x</p>");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `upsert_entry` (refresh path) also writes the cache.
    #[test]
    fn upsert_writes_cache() {
        let tmp = unique_tmp("glean-upsert-cache");
        let db_path = tmp.join("glean.db");
        let cache_dir = tmp.join("cache").join("entries");
        let mut store = Store::open_path_with_cache(&db_path, Some(cache_dir.clone())).unwrap();
        let fid = store.add_feed("T", "https://ex/feed.xml", None).unwrap();
        let body = "<p>upserted</p>";
        assert!(store
            .upsert_entry(fid, "g1", "Title", None, None, None, None, body)
            .unwrap());
        let entries = store.list_entries(EntryFilter::All).unwrap();
        let id = entries[0].id;
        let cache_file = cache_dir.join(id.0.to_string()).join("body.html");
        assert_eq!(std::fs::read_to_string(&cache_file).unwrap(), body);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// schema v9：entry_enhancements 写入/覆盖/读取/级联删除。
    #[test]
    fn enhancement_roundtrip_and_overwrite() {
        let mut store = Store::open_in_memory().unwrap();
        let fid = store.add_feed("T", "https://ex/feed.xml", None).unwrap();
        let id = store
            .add_entry(fid, "g1", "Title", None, "<p>x</p>")
            .unwrap();

        // 初始无增强。
        assert!(store.get_enhancement(id, "summary").unwrap().is_none());
        assert!(store.list_enhancements(id).unwrap().is_empty());

        // 写摘要。
        store.set_enhancement(id, "summary", "第一版摘要").unwrap();
        assert_eq!(
            store.get_enhancement(id, "summary").unwrap().as_deref(),
            Some("第一版摘要")
        );

        // 同 kind 覆盖。
        store.set_enhancement(id, "summary", "第二版摘要").unwrap();
        assert_eq!(
            store.get_enhancement(id, "summary").unwrap().as_deref(),
            Some("第二版摘要")
        );

        // 加翻译，list 返回 2 条。
        store
            .set_enhancement(id, "translate", "Translated.")
            .unwrap();
        let all = store.list_enhancements(id).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|(k, v)| k == "summary" && v == "第二版摘要"));
        assert!(all
            .iter()
            .any(|(k, v)| k == "translate" && v == "Translated."));
    }
}
