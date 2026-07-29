//! Glean domain core: models, SQLite store, feed fetch, AppCommand/AppEvent.
//! This crate must never depend on egui, wry, or tauri.

mod command;
mod error;
mod event;
pub mod feed;
mod model;
mod opml;
mod paths;
mod reader_html;
mod sanitize;
mod service;
pub mod store;

pub use command::AppCommand;
pub use error::{CoreError, Result};
pub use event::AppEvent;
pub use feed::{run_refresh_task, RefreshOutcome, RefreshTask};
pub use model::{
    AppConfig, EntryDetail, EntryFilter, EntryId, EntrySummary, Feed, FeedId, Folder, FolderId,
    ImagePolicy,
};
pub use opml::{export_opml, parse_opml, OpmlOutline};
pub use paths::{default_config_path, default_db_path};
pub use reader_html::reader_document;
pub use sanitize::{sanitize_html, sanitize_html_with_policy};
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
    use crate::feed::parse::parse_feed;

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
        let doc = reader_document(
            "t",
            None,
            None,
            "<p>hi</p>",
            false,
            true,
            ImagePolicy::Block,
        );
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
        });
        assert!(ev
            .iter()
            .any(|e| matches!(e, AppEvent::NavUpdated { feeds, .. } if !feeds.is_empty())));
    }

    #[test]
    fn parse_and_upsert_without_network() {
        const SAMPLE: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>T</title>
<item><title>A</title><guid>g1</guid><description>&lt;p&gt;x&lt;/p&gt;</description></item>
<item><title>A</title><guid>g1</guid><description>dup</description></item>
</channel></rss>"#;
        let parsed = parse_feed(SAMPLE).unwrap();
        let mut store = Store::open_in_memory().unwrap();
        let fid = store.add_feed("T", "https://ex/feed.xml", None).unwrap();
        let mut n = 0;
        for e in &parsed.entries {
            if store
                .upsert_entry(
                    fid,
                    &e.guid,
                    &e.title,
                    e.url.as_deref(),
                    e.author.as_deref(),
                    e.published_at,
                    e.summary.as_deref(),
                    &e.content_html,
                )
                .unwrap()
            {
                n += 1;
            }
        }
        // two items same guid in sample -> only one insert from loop over unique...
        // sample has two items same guid - second IGNORE
        assert_eq!(n, 1);
        assert_eq!(store.list_entries(EntryFilter::All).unwrap().len(), 1);
    }
}
