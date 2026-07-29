//! Command handler: AppCommand → mutate store → AppEvent list.

use crate::command::AppCommand;
use crate::error::{CoreError, Result};
use crate::event::AppEvent;
use crate::feed::{
    fetch_feed_bytes, parse_feed, FetchResult, HttpClient, RefreshOutcome, RefreshTask,
};
use crate::model::{EntryFilter, FeedId};
use crate::opml;
use crate::store::Store;
use std::path::Path;

pub struct GleanService {
    store: Store,
    filter: EntryFilter,
    search_query: String,
    http: HttpClient,
}

impl GleanService {
    pub fn open_in_memory() -> Result<Self> {
        Ok(Self {
            store: Store::open_in_memory()?,
            filter: EntryFilter::All,
            search_query: String::new(),
            http: HttpClient::new()?,
        })
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        Ok(Self {
            store: Store::open_path(path)?,
            filter: EntryFilter::All,
            search_query: String::new(),
            http: HttpClient::new()?,
        })
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
        let url = feed_url.trim();
        if url.is_empty() {
            return Err(CoreError::Message("订阅 URL 为空".into()));
        }
        if let Some(existing) = self.store.find_feed_by_url(url)? {
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

        let fetched = fetch_feed_bytes(&self.http, url, None, None)?;
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
        let parsed = parse_feed(&bytes)?;
        let id = self.store.add_feed(&parsed.title, url, None)?;
        self.store.update_feed_after_fetch(
            id,
            Some(&parsed.title),
            parsed.site_url.as_deref(),
            etag.as_deref(),
            last_modified.as_deref(),
            None,
        )?;
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
        Ok(ev)
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
        Ok(vec![AppEvent::NavUpdated {
            folders: self.store.list_folders()?,
            feeds: self.store.list_feeds()?,
            unread_total: self.store.unread_count()?,
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
