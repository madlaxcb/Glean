//! Command handler: AppCommand → mutate store → AppEvent list.

use crate::command::AppCommand;
use crate::error::{CoreError, Result};
use crate::event::AppEvent;
use crate::extract::{ExtractOutcome, ExtractTask};
use crate::feed::{
    discover_feed_urls, fetch_feed_bytes, parse_feed, FetchResult, HttpClient, RefreshOutcome,
    RefreshTask,
};
use crate::model::{EntryFilter, EntryId, FeedId};
use crate::opml;
use crate::paths;
use crate::plugin::{CredentialStore, PluginManager};
use crate::store::Store;
use std::path::Path;

pub struct GleanService {
    store: Store,
    filter: EntryFilter,
    search_query: String,
    http: HttpClient,
    /// §11.5 插件管理器。`None` 表示 in-memory 模式（测试用），不加载磁盘插件。
    plugin_mgr: Option<PluginManager>,
    /// §11.5.9 凭证存储。`None` 表示 in-memory 模式。
    credentials: Option<CredentialStore>,
}

impl GleanService {
    pub fn open_in_memory() -> Result<Self> {
        Self::open_in_memory_with_proxy(None)
    }

    pub fn open_in_memory_with_proxy(proxy_url: Option<&str>) -> Result<Self> {
        Ok(Self {
            store: Store::open_in_memory()?,
            filter: EntryFilter::All,
            search_query: String::new(),
            http: HttpClient::with_proxy(proxy_url)?,
            plugin_mgr: None,
            credentials: None,
        })
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        Self::open_path_with_proxy(path, None)
    }

    pub fn open_path_with_proxy(path: &Path, proxy_url: Option<&str>) -> Result<Self> {
        // §11.5.8 / §11.5.9 加载插件目录 + 凭证存储。失败不阻塞核心功能：
        // 插件系统是扩展层，DB/HTTP/订阅主线必须能独立工作。
        let plugin_mgr = paths::plugins_dir().and_then(|d| PluginManager::new(d).ok());
        let credentials = paths::credentials_path().and_then(|p| CredentialStore::open(p).ok());
        Ok(Self {
            store: Store::open_path(path)?,
            filter: EntryFilter::All,
            search_query: String::new(),
            http: HttpClient::with_proxy(proxy_url)?,
            plugin_mgr,
            credentials,
        })
    }

    /// 访问插件管理器（§11.5）。M5 仅完成框架加载；Tier 1/2 端到端接入排到 M6。
    pub fn plugins(&self) -> Option<&PluginManager> {
        self.plugin_mgr.as_ref()
    }

    /// 访问凭证存储（§11.5.9）。
    pub fn credentials(&self) -> Option<&CredentialStore> {
        self.credentials.as_ref()
    }

    /// 可变访问凭证存储——设置 UI 输入凭证后调用。
    pub fn credentials_mut(&mut self) -> Option<&mut CredentialStore> {
        self.credentials.as_mut()
    }

    pub fn handle(&mut self, cmd: AppCommand) -> Vec<AppEvent> {
        match self.handle_inner(cmd) {
            Ok(ev) => ev,
            Err(e) => vec![AppEvent::Error {
                message: e.to_string(),
            }],
        }
    }

    fn handle_inner(&mut self, cmd: AppCommand) -> Result<Vec<AppEvent>> {
        match cmd {
            AppCommand::Bootstrap { seed_demo } => {
                let mut ev = vec![AppEvent::Ready];
                if seed_demo {
                    let seeded = self.store.seed_demo_if_empty()?;
                    if seeded {
                        ev.push(AppEvent::Status {
                            message: "Demo data seeded (offline)".into(),
                        });
                    }
                }
                ev.extend(self.emit_nav()?);
                ev.extend(self.emit_entries()?);
                Ok(ev)
            }
            AppCommand::RefreshNav => Ok(self.emit_nav()?),
            AppCommand::ListEntries { filter } => {
                self.filter = filter;
                self.search_query.clear();
                Ok(self.emit_entries()?)
            }
            AppCommand::SearchEntries { query } => {
                self.search_query = query;
                Ok(self.emit_entries()?)
            }
            AppCommand::OpenEntry { id } => {
                let entry = self.store.get_entry(id)?;
                if !entry.summary.is_read {
                    self.store.set_read(id, true)?;
                }
                let entry = self.store.get_entry(id)?;
                let unread = self.store.unread_count()?;
                Ok(vec![
                    AppEvent::EntryOpened { entry },
                    AppEvent::UnreadChanged { total: unread },
                    AppEvent::EntriesUpdated {
                        entries: self.store.list_entries(self.filter)?,
                    },
                ])
            }
            AppCommand::MarkRead { id, read } => {
                self.store.set_read(id, read)?;
                let unread = self.store.unread_count()?;
                let mut ev = self.emit_entries()?;
                ev.push(AppEvent::UnreadChanged { total: unread });
                Ok(ev)
            }
            AppCommand::ToggleStar { id } => {
                let starred = self.store.toggle_star(id)?;
                let mut ev = self.emit_entries()?;
                ev.push(AppEvent::Status {
                    message: if starred {
                        "已加星标".into()
                    } else {
                        "已取消星标".into()
                    },
                });
                Ok(ev)
            }
            AppCommand::MarkAllRead { feed_id } => {
                match feed_id {
                    Some(fid) => self.store.mark_all_read_in_feed(fid)?,
                    None => self.store.mark_all_read()?,
                }
                let unread = self.store.unread_count()?;
                let mut ev = self.emit_entries()?;
                ev.push(AppEvent::UnreadChanged { total: unread });
                ev.push(AppEvent::Status {
                    message: "已全部标记已读".into(),
                });
                Ok(ev)
            }
            AppCommand::AddFeedLocal { title, feed_url } => {
                let id = self.store.add_feed(&title, &feed_url, None)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: format!("已添加本地源 id={}", id.0),
                });
                Ok(ev)
            }
            AppCommand::AddEntryLocal {
                feed_id,
                guid,
                title,
                url,
                content_html,
            } => {
                let id =
                    self.store
                        .add_entry(feed_id, &guid, &title, url.as_deref(), &content_html)?;
                let mut ev = self.emit_entries()?;
                ev.push(AppEvent::Status {
                    message: format!("Entry added id={}", id.0),
                });
                ev.push(AppEvent::UnreadChanged {
                    total: self.store.unread_count()?,
                });
                Ok(ev)
            }
            AppCommand::AddFeedFromUrl { feed_url } => self.add_feed_from_url(&feed_url),
            AppCommand::DeleteFeed { id } => {
                self.store.delete_feed(id)?;
                let mut ev = self.emit_nav()?;
                ev.extend(self.emit_entries()?);
                ev.push(AppEvent::UnreadChanged {
                    total: self.store.unread_count()?,
                });
                ev.push(AppEvent::Status {
                    message: format!("已删除源 {}", id.0),
                });
                Ok(ev)
            }
            AppCommand::RenameFeed { id, title } => {
                self.store.rename_feed(id, &title)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: format!("已重命名「{title}」"),
                });
                Ok(ev)
            }
            AppCommand::EditFeedUrl { id, feed_url } => {
                self.store.set_feed_url(id, &feed_url)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: "已更新订阅 URL".into(),
                });
                Ok(ev)
            }
            AppCommand::MoveFeedToFolder { feed_id, folder_id } => {
                self.store.move_feed_to_folder(feed_id, folder_id)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: "已移动到文件夹".into(),
                });
                Ok(ev)
            }
            AppCommand::ToggleMuteFeed { id } => {
                let muted = self.store.toggle_mute_feed(id)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: if muted {
                        "已静音".into()
                    } else {
                        "已取消静音".into()
                    },
                });
                Ok(ev)
            }
            AppCommand::SetFeedRefreshInterval { id, secs } => {
                self.store.set_feed_refresh_interval(id, secs)?;
                self.emit_nav()
            }
            AppCommand::AutoRefresh => {
                // Caller (UI) checks interval and dispatches refresh tasks for due feeds.
                Ok(vec![])
            }
            AppCommand::CreateFolder { name } => {
                let id = self.store.add_folder(&name)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: format!("已创建文件夹「{name}」id={}", id.0),
                });
                Ok(ev)
            }
            AppCommand::RefreshFeeds { feed_id } => {
                // Synchronous fallback; UI should prefer prepare/apply for async.
                let ids = match feed_id {
                    Some(id) => vec![id],
                    None => self.store.list_feed_ids()?,
                };
                let mut ev = Vec::new();
                for id in ids {
                    if let Some(outcome) = self.refresh_one_sync(id) {
                        ev.extend(self.apply_refresh_outcome_inner(outcome)?);
                    }
                }
                ev.extend(self.emit_nav()?);
                ev.extend(self.emit_entries()?);
                ev.push(AppEvent::UnreadChanged {
                    total: self.store.unread_count()?,
                });
                ev.push(AppEvent::Status {
                    message: "刷新完成".into(),
                });
                Ok(ev)
            }
            AppCommand::ImportOpml { content } => self.import_opml(&content),
            AppCommand::ExportOpml => {
                let feeds = self.store.list_feeds()?;
                let xml = opml::export_opml(&feeds);
                Ok(vec![
                    AppEvent::OpmlExported { xml },
                    AppEvent::Status {
                        message: format!("已导出 {} 个订阅", feeds.len()),
                    },
                ])
            }
            AppCommand::ExtractEntry { id } => {
                // Manual trigger. UI normally uses prepare_extract_task +
                // apply_extract_outcome for async; this is a sync fallback.
                let detail = self.store.get_entry(id)?;
                let url = match detail.summary.url.as_deref() {
                    Some(u) => u.to_string(),
                    None => {
                        return Err(CoreError::Message(
                            "entry has no URL; cannot extract".into(),
                        ))
                    }
                };
                let task = ExtractTask { entry_id: id, url };
                let outcome = crate::extract::run_extract_task(&self.http.inner, &task);
                Ok(self.apply_extract_outcome_inner(outcome)?)
            }
        }
    }

    // --- Async refresh: UI calls prepare → thread → apply ---

    pub fn prepare_refresh_tasks(&self, feed_id: Option<FeedId>) -> Result<Vec<RefreshTask>> {
        let ids = match feed_id {
            Some(id) => vec![id],
            None => self.store.list_feed_ids()?,
        };
        let mut tasks = Vec::with_capacity(ids.len());
        for id in ids {
            let (url, etag, last_modified) = self.store.get_feed_fetch_meta(id)?;
            tasks.push(RefreshTask {
                feed_id: id,
                url,
                etag,
                last_modified,
            });
        }
        Ok(tasks)
    }

    pub fn prepare_auto_refresh_tasks(
        &self,
        global_interval_secs: i64,
    ) -> Result<Vec<RefreshTask>> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let due = self
            .store
            .feeds_due_for_refresh(global_interval_secs, now)?;
        if due.is_empty() {
            return Ok(vec![]);
        }
        let mut tasks = Vec::with_capacity(due.len());
        for id in due {
            let (url, etag, last_modified) = self.store.get_feed_fetch_meta(id)?;
            tasks.push(RefreshTask {
                feed_id: id,
                url,
                etag,
                last_modified,
            });
        }
        Ok(tasks)
    }

    /// Apply one background refresh result; returns events to emit.
    pub fn apply_refresh_outcome(&mut self, outcome: RefreshOutcome) -> Vec<AppEvent> {
        match self.apply_refresh_outcome_inner(outcome) {
            Ok(ev) => ev,
            Err(e) => vec![AppEvent::Error {
                message: e.to_string(),
            }],
        }
    }

    fn apply_refresh_outcome_inner(&mut self, outcome: RefreshOutcome) -> Result<Vec<AppEvent>> {
        match outcome {
            RefreshOutcome::NotModified { feed_id } => {
                self.store
                    .update_feed_after_fetch(feed_id, None, None, None, None, None)?;
                Ok(vec![])
            }
            RefreshOutcome::Updated {
                feed_id,
                parsed,
                etag,
                last_modified,
            } => {
                self.store.update_feed_after_fetch(
                    feed_id,
                    Some(&parsed.title),
                    parsed.site_url.as_deref(),
                    etag.as_deref(),
                    last_modified.as_deref(),
                    None,
                )?;
                // Persist favicon URL if the feed provides one.
                if parsed.favicon_url.is_some() {
                    self.store
                        .set_favicon_url(feed_id, parsed.favicon_url.as_deref())?;
                }
                let mut new_items = 0u32;
                for e in &parsed.entries {
                    if self.store.upsert_entry(
                        feed_id,
                        &e.guid,
                        &e.title,
                        e.url.as_deref(),
                        e.author.as_deref(),
                        e.published_at,
                        e.summary.as_deref(),
                        &e.content_html,
                    )? {
                        new_items += 1;
                    }
                }
                Ok(vec![AppEvent::Status {
                    message: format!("「{}」+{} 篇", parsed.title, new_items),
                }])
            }
            RefreshOutcome::Error { feed_id, error } => {
                self.store.update_feed_after_fetch(
                    feed_id,
                    None,
                    None,
                    None,
                    None,
                    Some(&error),
                )?;
                Ok(vec![AppEvent::Error {
                    message: format!("源 {} 失败: {error}", feed_id.0),
                }])
            }
        }
    }

    // --- Async extraction: UI calls prepare → thread → apply ---

    /// Build an extraction task for an entry. Returns None if the entry has no
    /// URL, is already long enough (full feed content), or already extracted.
    pub fn prepare_extract_task(&self, id: EntryId) -> Result<Option<ExtractTask>> {
        let entry = self.store.get_entry(id)?;
        // Skip if already extracted (don't re-extract on every open).
        if !entry.extracted_html.is_empty() {
            return Ok(None);
        }
        // Skip if the feed content is already substantial.
        if !crate::extract::should_extract(&entry.content_html, entry.summary.url.as_deref()) {
            return Ok(None);
        }
        match entry.summary.url {
            Some(u) => Ok(Some(ExtractTask {
                entry_id: id,
                url: u,
            })),
            None => Ok(None),
        }
    }

    /// Apply one background extraction result; returns events to emit.
    pub fn apply_extract_outcome(&mut self, outcome: ExtractOutcome) -> Vec<AppEvent> {
        match self.apply_extract_outcome_inner(outcome) {
            Ok(ev) => ev,
            Err(e) => vec![AppEvent::Error {
                message: e.to_string(),
            }],
        }
    }

    fn apply_extract_outcome_inner(&mut self, outcome: ExtractOutcome) -> Result<Vec<AppEvent>> {
        match outcome {
            ExtractOutcome::Extracted { entry_id, html } => {
                self.store.set_extracted_html(entry_id, &html)?;
                Ok(vec![AppEvent::EntryExtracted {
                    id: entry_id,
                    success: true,
                }])
            }
            ExtractOutcome::Failed { entry_id, error } => Ok(vec![
                AppEvent::EntryExtracted {
                    id: entry_id,
                    success: false,
                },
                AppEvent::Status {
                    message: format!("全文抽取失败: {error}"),
                },
            ]),
        }
    }

    /// Query unread count per feed (lightweight, for badge updates).
    pub fn unread_counts_per_feed(&self) -> std::collections::HashMap<FeedId, u64> {
        self.store
            .unread_counts_per_feed()
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    fn refresh_one_sync(&mut self, id: FeedId) -> Option<RefreshOutcome> {
        let (url, etag, last_modified) = self.store.get_feed_fetch_meta(id).ok()?;
        match fetch_feed_bytes(&self.http, &url, etag.as_deref(), last_modified.as_deref()) {
            Ok(FetchResult::NotModified) => Some(RefreshOutcome::NotModified { feed_id: id }),
            Ok(FetchResult::Body {
                bytes,
                etag,
                last_modified,
                ..
            }) => match parse_feed(&bytes) {
                Ok(parsed) => Some(RefreshOutcome::Updated {
                    feed_id: id,
                    parsed,
                    etag,
                    last_modified,
                }),
                Err(e) => Some(RefreshOutcome::Error {
                    feed_id: id,
                    error: e.to_string(),
                }),
            },
            Err(e) => Some(RefreshOutcome::Error {
                feed_id: id,
                error: e.to_string(),
            }),
        }
    }

    fn add_feed_from_url(&mut self, feed_url: &str) -> Result<Vec<AppEvent>> {
        let trimmed = feed_url.trim();
        if trimmed.is_empty() {
            return Err(CoreError::Message("订阅 URL 为空".into()));
        }
        // §11.5.2 Tier 0: 对 GitHub releases / YouTube channel 做 URL 规范化。
        // 规范化后的 URL 才是真正入库的订阅地址。
        let url = crate::feed::tier0::normalize(trimmed);
        if let Some(existing) = self.store.find_feed_by_url(&url)? {
            let mut ev = vec![AppEvent::Status {
                message: format!("源已存在，已刷新 id={}", existing.0),
            }];
            if let Some(outcome) = self.refresh_one_sync(existing) {
                ev.extend(self.apply_refresh_outcome_inner(outcome)?);
            }
            ev.extend(self.emit_nav()?);
            ev.extend(self.emit_entries()?);
            return Ok(ev);
        }

        // Try 1: parse URL directly as a feed.
        match self.try_add_as_feed(&url) {
            Ok(Some(events)) => return Ok(events),
            Ok(None) => {} // Not a feed, fall through to discovery.
            Err(e) => return Err(e),
        }

        // Try 2: fetch as HTML and discover feed links.
        self.try_discover_and_add(&url)
    }

    /// Try to add the URL as a feed. Returns Ok(None) if it's not a feed.
    fn try_add_as_feed(&mut self, url: &str) -> Result<Option<Vec<AppEvent>>> {
        let fetched = match fetch_feed_bytes(&self.http, url, None, None) {
            Ok(r) => r,
            Err(_) => return Ok(None), // Network error — not a feed.
        };
        let (bytes, etag, last_modified, _final_url) = match fetched {
            FetchResult::NotModified => {
                return Err(CoreError::Http("unexpected 304 on first fetch".into()));
            }
            FetchResult::Body {
                bytes,
                etag,
                last_modified,
                final_url,
            } => (bytes, etag, last_modified, final_url),
        };
        let parsed = match parse_feed(&bytes) {
            Ok(p) => p,
            Err(_) => return Ok(None), // Not a feed.
        };
        let id = self.store.add_feed(&parsed.title, url, None)?;
        self.store.update_feed_after_fetch(
            id,
            Some(&parsed.title),
            parsed.site_url.as_deref(),
            etag.as_deref(),
            last_modified.as_deref(),
            None,
        )?;
        if parsed.favicon_url.is_some() {
            self.store
                .set_favicon_url(id, parsed.favicon_url.as_deref())?;
        }
        let mut new_items = 0u32;
        for e in &parsed.entries {
            if self.store.upsert_entry(
                id,
                &e.guid,
                &e.title,
                e.url.as_deref(),
                e.author.as_deref(),
                e.published_at,
                e.summary.as_deref(),
                &e.content_html,
            )? {
                new_items += 1;
            }
        }
        let mut ev = self.emit_nav()?;
        ev.extend(self.emit_entries()?);
        ev.push(AppEvent::UnreadChanged {
            total: self.store.unread_count()?,
        });
        ev.push(AppEvent::Status {
            message: format!("已订阅「{}」· 新文章 {} 篇", parsed.title, new_items),
        });
        Ok(Some(ev))
    }

    /// Fetch the URL as HTML, discover feed links, and subscribe to the first one.
    fn try_discover_and_add(&mut self, url: &str) -> Result<Vec<AppEvent>> {
        let fetched = fetch_feed_bytes(&self.http, url, None, None)?;
        let bytes = match fetched {
            FetchResult::Body { bytes, .. } => bytes,
            FetchResult::NotModified => {
                return Err(CoreError::Http("unexpected 304 on first fetch".into()));
            }
        };
        let html = String::from_utf8_lossy(&bytes);
        let feeds = discover_feed_urls(&html, url);
        if feeds.is_empty() {
            return Err(CoreError::Message(format!(
                "无法识别为 feed，也未在页面中发现 feed 链接: {url}"
            )));
        }

        // Subscribe to the first discovered feed.
        let (feed_url, feed_title) = &feeds[0];
        let title = feed_title.as_deref().unwrap_or_else(|| feed_url.as_str());
        match self.try_add_as_feed(feed_url) {
            Ok(Some(mut events)) => {
                // Patch status message to mention discovery.
                for ev in &mut events {
                    if let AppEvent::Status { message } = ev {
                        *message = format!("自动发现并订阅「{title}」\n{message}");
                    }
                }
                Ok(events)
            }
            Ok(None) => Err(CoreError::Message(format!(
                "发现 feed 链接但订阅失败: {feed_url}"
            ))),
            Err(e) => Err(e),
        }
    }

    fn import_opml(&mut self, content: &str) -> Result<Vec<AppEvent>> {
        let outlines = opml::parse_opml(content);
        let mut added = 0u32;
        for o in &outlines {
            if self.store.find_feed_by_url(&o.feed_url)?.is_none() {
                let title = if o.title.is_empty() {
                    o.feed_url.clone()
                } else {
                    o.title.clone()
                };
                self.store.add_feed(&title, &o.feed_url, None)?;
                added += 1;
            }
        }
        let mut ev = self.emit_nav()?;
        ev.push(AppEvent::Status {
            message: format!("OPML 导入：新增 {added} 个订阅（共 {} 条）", outlines.len()),
        });
        Ok(ev)
    }

    fn emit_nav(&self) -> Result<Vec<AppEvent>> {
        let pairs = self.store.unread_counts_per_feed()?;
        let unread_per_feed: std::collections::HashMap<FeedId, u64> = pairs.into_iter().collect();
        Ok(vec![AppEvent::NavUpdated {
            folders: self.store.list_folders()?,
            feeds: self.store.list_feeds()?,
            unread_total: self.store.unread_count_excluding_muted()?,
            unread_per_feed,
        }])
    }

    fn emit_entries(&self) -> Result<Vec<AppEvent>> {
        let q = self.search_query.trim();
        let entries = if q.is_empty() {
            self.store.list_entries(self.filter)?
        } else {
            self.store.search_entries(q, 500)?
        };
        Ok(vec![AppEvent::EntriesUpdated { entries }])
    }
}
