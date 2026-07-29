//! SQLite persistence. FTS5 uses trigram when available.

use crate::error::{CoreError, Result};
use crate::model::{
    EntryDetail, EntryFilter, EntryId, EntrySummary, Feed, FeedId, Folder, FolderId,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

const SCHEMA_VERSION: i64 = 1;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut s = Self { conn };
        s.migrate()?;
        Ok(s)
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Message(e.to_string()))?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;",
        )?;
        let mut s = Self { conn };
        s.migrate()?;
        Ok(s)
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
            "SELECT id, folder_id, title, site_url, feed_url, last_error FROM feeds ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Feed {
                id: FeedId(r.get(0)?),
                folder_id: r.get::<_, Option<i64>>(1)?.map(FolderId),
                title: r.get(2)?,
                site_url: r.get(3)?,
                feed_url: r.get(4)?,
                last_error: r.get(5)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn list_entries(&self, filter: EntryFilter) -> Result<Vec<EntrySummary>> {
        let sql = match filter {
            EntryFilter::All => {
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred, content_html
                 FROM entries ORDER BY COALESCE(published_at, fetched_at) DESC, id DESC"
            }
            EntryFilter::Unread => {
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred, content_html
                 FROM entries WHERE is_read = 0
                 ORDER BY COALESCE(published_at, fetched_at) DESC, id DESC"
            }
            EntryFilter::Starred => {
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred, content_html
                 FROM entries WHERE is_starred = 1
                 ORDER BY COALESCE(published_at, fetched_at) DESC, id DESC"
            }
            EntryFilter::Feed(_) => {
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred, content_html
                 FROM entries WHERE feed_id = ?1
                 ORDER BY COALESCE(published_at, fetched_at) DESC, id DESC"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<EntrySummary> {
            Ok(EntrySummary {
                id: EntryId(r.get(0)?),
                feed_id: FeedId(r.get(1)?),
                title: r.get(2)?,
                url: r.get(3)?,
                published_at: r.get(4)?,
                is_read: r.get::<_, i64>(5)? != 0,
                is_starred: r.get::<_, i64>(6)? != 0,
                has_content: !r.get::<_, String>(7)?.is_empty(),
            })
        };
        let list = match filter {
            EntryFilter::Feed(FeedId(fid)) => {
                let rows = stmt.query_map(params![fid], map_row)?;
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
        self.conn
            .query_row(
                "SELECT id, feed_id, title, url, published_at, is_read, is_starred, author, content_html
                 FROM entries WHERE id = ?1",
                params![id.0],
                |r| {
                    Ok(EntryDetail {
                        summary: EntrySummary {
                            id: EntryId(r.get(0)?),
                            feed_id: FeedId(r.get(1)?),
                            title: r.get(2)?,
                            url: r.get(3)?,
                            published_at: r.get(4)?,
                            is_read: r.get::<_, i64>(5)? != 0,
                            is_starred: r.get::<_, i64>(6)? != 0,
                            has_content: !r.get::<_, String>(8)?.is_empty(),
                        },
                        author: r.get(7)?,
                        content_html: r.get(8)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound(format!("entry {}", id.0)))
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
            "SELECT e.id, e.feed_id, e.title, e.url, e.published_at, e.is_read, e.is_starred, e.content_html
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
            "SELECT id, feed_id, title, url, published_at, is_read, is_starred, content_html
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
                "UPDATE feeds SET last_error = ?1, last_fetched_at = ?2 WHERE id = ?3",
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
                last_fetched_at = ?5
             WHERE id = ?6",
            params![title, site_url, etag, last_modified, now, id.0],
        )?;
        Ok(())
    }

    /// Insert or ignore by (feed_id, guid). Returns true if a new row was inserted.
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
        Ok(true)
    }

    pub fn list_feed_ids(&self) -> Result<Vec<FeedId>> {
        let mut stmt = self.conn.prepare("SELECT id FROM feeds ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.filter_map(|r| r.ok()).map(FeedId).collect())
    }

    pub fn delete_feed(&mut self, id: FeedId) -> Result<()> {
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
        feed_id: FeedId(r.get(1)?),
        title: r.get(2)?,
        url: r.get(3)?,
        published_at: r.get(4)?,
        is_read: r.get::<_, i64>(5)? != 0,
        is_starred: r.get::<_, i64>(6)? != 0,
        has_content: !r.get::<_, String>(7)?.is_empty(),
    })
}

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
