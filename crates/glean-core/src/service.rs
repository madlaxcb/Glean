//! Command handler: AppCommand → mutate store → AppEvent list.

use crate::command::AppCommand;
use crate::error::{CoreError, Result};
use crate::event::AppEvent;
use crate::feed::{fetch_feed_bytes, parse_feed, FetchResult, HttpClient};
use crate::model::{EntryFilter, FeedId};
use crate::store::Store;
use std::path::Path;

pub struct GleanService {
    store: Store,
    filter: EntryFilter,
    http: HttpClient,
}

impl GleanService {
    pub fn open_in_memory() -> Result<Self> {
        Ok(Self {
            store: Store::open_in_memory()?,
            filter: EntryFilter::All,
            http: HttpClient::new()?,
        })
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        Ok(Self {
            store: Store::open_path(path)?,
            filter: EntryFilter::All,
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
            AppCommand::AddFeedLocal {
                title,
                feed_url,
                folder_id,
            } => {
                let id = self.store.add_feed(&title, &feed_url, folder_id)?;
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
            AppCommand::AddFeedFromUrl {
                feed_url,
                folder_id,
            } => self.add_feed_from_url(&feed_url, folder_id),
            AppCommand::RefreshFeeds { feed_id } => self.refresh_feeds(feed_id),
        }
    }

    fn add_feed_from_url(
        &mut self,
        feed_url: &str,
        folder_id: Option<crate::model::FolderId>,
    ) -> Result<Vec<AppEvent>> {
        let url = feed_url.trim();
        if url.is_empty() {
            return Err(CoreError::Message("订阅 URL 为空".into()));
        }
        if let Some(existing) = self.store.find_feed_by_url(url)? {
            let mut ev = self.refresh_one(existing)?;
            ev.insert(
                0,
                AppEvent::Status {
                    message: format!("源已存在，已刷新 id={}", existing.0),
                },
            );
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
        let id = self.store.add_feed(&parsed.title, url, folder_id)?;
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

    fn refresh_feeds(&mut self, feed_id: Option<FeedId>) -> Result<Vec<AppEvent>> {
        let ids = match feed_id {
            Some(id) => vec![id],
            None => self.store.list_feed_ids()?,
        };
        if ids.is_empty() {
            return Ok(vec![AppEvent::Status {
                message: "没有可刷新的订阅".into(),
            }]);
        }
        let mut total_new = 0u32;
        let mut errors = 0u32;
        let mut ev = Vec::new();
        for id in ids {
            match self.refresh_one(id) {
                Ok(mut part) => {
                    // Count new from status is hard; re-scan last status — keep simple:
                    total_new += 0;
                    // Extract nothing; accumulate messages
                    for e in part.drain(..) {
                        if let AppEvent::Status { message } = &e {
                            if let Some(rest) = message.strip_prefix("__new__:") {
                                if let Ok(n) = rest.parse::<u32>() {
                                    total_new += n;
                                    continue;
                                }
                            }
                        }
                        // Drop intermediate nav/entries until end
                        match e {
                            AppEvent::Status { .. } | AppEvent::Error { .. } => ev.push(e),
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    errors += 1;
                    let _ = self.store.update_feed_after_fetch(
                        id,
                        None,
                        None,
                        None,
                        None,
                        Some(&e.to_string()),
                    );
                    ev.push(AppEvent::Error {
                        message: format!("源 {} 刷新失败: {e}", id.0),
                    });
                }
            }
        }
        ev.extend(self.emit_nav()?);
        ev.extend(self.emit_entries()?);
        ev.push(AppEvent::UnreadChanged {
            total: self.store.unread_count()?,
        });
        ev.push(AppEvent::Status {
            message: format!("刷新完成 · 新增约 {total_new} 篇 · 失败 {errors} 个源"),
        });
        Ok(ev)
    }

    fn refresh_one(&mut self, id: FeedId) -> Result<Vec<AppEvent>> {
        let (url, etag, last_modified) = self.store.get_feed_fetch_meta(id)?;
        let fetched = fetch_feed_bytes(&self.http, &url, etag.as_deref(), last_modified.as_deref());
        let fetched = match fetched {
            Ok(f) => f,
            Err(e) => {
                self.store.update_feed_after_fetch(
                    id,
                    None,
                    None,
                    None,
                    None,
                    Some(&e.to_string()),
                )?;
                return Err(e);
            }
        };
        match fetched {
            FetchResult::NotModified => {
                self.store
                    .update_feed_after_fetch(id, None, None, None, None, None)?;
                Ok(vec![AppEvent::Status {
                    message: format!("源 {} 无更新 (304)", id.0),
                }])
            }
            FetchResult::Body {
                bytes,
                etag,
                last_modified,
                final_url: _,
            } => {
                let parsed = match parse_feed(&bytes) {
                    Ok(p) => p,
                    Err(e) => {
                        self.store.update_feed_after_fetch(
                            id,
                            None,
                            None,
                            None,
                            None,
                            Some(&e.to_string()),
                        )?;
                        return Err(e);
                    }
                };
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
                Ok(vec![AppEvent::Status {
                    message: format!("__new__:{new_items}"),
                }])
            }
        }
    }

    fn emit_nav(&self) -> Result<Vec<AppEvent>> {
        Ok(vec![AppEvent::NavUpdated {
            folders: self.store.list_folders()?,
            feeds: self.store.list_feeds()?,
            unread_total: self.store.unread_count()?,
        }])
    }

    fn emit_entries(&self) -> Result<Vec<AppEvent>> {
        Ok(vec![AppEvent::EntriesUpdated {
            entries: self.store.list_entries(self.filter)?,
        }])
    }
}
