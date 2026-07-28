//! Command handler: AppCommand → mutate store → AppEvent list.

use crate::command::AppCommand;
use crate::error::Result;
use crate::event::AppEvent;
use crate::model::EntryFilter;
use crate::store::Store;
use std::path::Path;

pub struct GleanService {
    store: Store,
    filter: EntryFilter,
}

impl GleanService {
    pub fn open_in_memory() -> Result<Self> {
        Ok(Self {
            store: Store::open_in_memory()?,
            filter: EntryFilter::All,
        })
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        Ok(Self {
            store: Store::open_path(path)?,
            filter: EntryFilter::All,
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
                            message: "Demo data seeded (local only, no network)".into(),
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
                // Opening marks read (common reader UX); emits UnreadChanged.
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
                        "Starred".into()
                    } else {
                        "Unstarred".into()
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
                    message: format!("Feed added id={}", id.0),
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
