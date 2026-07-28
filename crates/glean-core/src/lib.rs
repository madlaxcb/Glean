//! Glean domain core: models, SQLite store, AppCommand/AppEvent service.
//! This crate must never depend on egui, wry, or tauri.

mod command;
mod error;
mod event;
mod model;
mod reader_html;
mod service;
pub mod store;

pub use command::AppCommand;
pub use error::{CoreError, Result};
pub use event::AppEvent;
pub use model::{EntryDetail, EntryFilter, EntryId, EntrySummary, Feed, FeedId, Folder, FolderId};
pub use reader_html::reader_document;
pub use service::GleanService;
pub use store::Store;

/// Host mode for the WebView reader (spike / hybrid path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum ReaderHostMode {
    #[default]
    ChildEmbed,
    FollowOverlay,
}

impl ReaderHostMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ChildEmbed => "H1 Child embed",
            Self::FollowOverlay => "H2 Follow overlay",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::ChildEmbed => Self::FollowOverlay,
            Self::FollowOverlay => Self::ChildEmbed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_demo_emits_nav_and_entries() {
        let mut svc = GleanService::open_in_memory().expect("mem db");
        let ev = svc.handle(AppCommand::Bootstrap { seed_demo: true });
        assert!(ev.iter().any(|e| matches!(e, AppEvent::Ready)));
        assert!(ev.iter().any(|e| matches!(e, AppEvent::NavUpdated { .. })));
        let entries = ev.iter().find_map(|e| match e {
            AppEvent::EntriesUpdated { entries } => Some(entries.clone()),
            _ => None,
        });
        assert_eq!(entries.expect("entries").len(), 3);
    }

    #[test]
    fn open_entry_marks_read_and_returns_html() {
        let mut svc = GleanService::open_in_memory().unwrap();
        svc.handle(AppCommand::Bootstrap { seed_demo: true });
        let list = svc.handle(AppCommand::ListEntries {
            filter: EntryFilter::All,
        });
        let id = list
            .iter()
            .find_map(|e| match e {
                AppEvent::EntriesUpdated { entries } => entries.first().map(|x| x.id),
                _ => None,
            })
            .expect("id");
        let ev = svc.handle(AppCommand::OpenEntry { id });
        let opened = ev
            .iter()
            .find_map(|e| match e {
                AppEvent::EntryOpened { entry } => Some(entry.clone()),
                _ => None,
            })
            .expect("opened");
        assert!(opened.summary.is_read);
        assert!(!opened.content_html.is_empty());
        assert!(ev.iter().any(|e| matches!(
            e,
            AppEvent::UnreadChanged { total } if *total == 2
        )));
    }

    #[test]
    fn document_has_no_script_tags() {
        let doc = reader_document("t", "<p>hi</p>", false);
        assert!(!doc.to_lowercase().contains("<script"));
    }

    #[test]
    fn host_mode_toggle() {
        assert_eq!(
            ReaderHostMode::ChildEmbed.toggle(),
            ReaderHostMode::FollowOverlay
        );
    }

    #[test]
    fn search_chinese_substring() {
        let mut store = Store::open_in_memory().unwrap();
        store.seed_demo_if_empty().unwrap();
        let feeds = store.list_feeds().unwrap();
        let fid = feeds[0].id;
        store
            .add_entry(fid, "cjk", "你好世界", None, "<p>拾光</p>")
            .unwrap();
        let hits = store.search_entries("世界", 10).unwrap();
        assert!(
            hits.iter()
                .any(|e| e.title.contains("你好") || e.title.contains("世界")),
            "expected substring hit for 世界, got {hits:?}"
        );
    }

    #[test]
    fn add_feed_local_via_service() {
        let mut svc = GleanService::open_in_memory().unwrap();
        svc.handle(AppCommand::Bootstrap { seed_demo: false });
        let ev = svc.handle(AppCommand::AddFeedLocal {
            title: "Local".into(),
            feed_url: "https://example.org/rss.xml".into(),
            folder_id: None,
        });
        assert!(ev
            .iter()
            .any(|e| matches!(e, AppEvent::NavUpdated { feeds, .. } if !feeds.is_empty())));
    }
}
