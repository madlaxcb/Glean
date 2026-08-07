//! Glean domain core: models, SQLite store, feed fetch, AppCommand/AppEvent.
//! This crate must never depend on egui, wry, or tauri.

pub mod ai;
mod command;
mod error;
mod event;
pub mod extract;
pub mod favicon_cache;
pub mod feed;
mod image_cache;
mod model;
mod opml;
mod paths;
pub mod plugin;
mod reader_html;
mod sanitize;
mod service;
pub mod store;

pub use ai::{run_enhance_task, EnhanceAction, EnhanceOutcome, EnhanceTask};
pub use command::AppCommand;
pub use error::{CoreError, Result};
pub use event::AppEvent;
pub use extract::{
    extract_content, extract_fantia_post_contents, extract_fantia_post_ids, fetch_article_html,
    run_extract_task, should_extract, ExtractOutcome, ExtractTask,
};
pub use favicon_cache::FaviconCache;
pub use feed::{discover_feed_urls, run_refresh_task, RefreshOutcome, RefreshTask};
pub use image_cache::{ImageCache, CUSTOM_SCHEME as IMAGE_CUSTOM_SCHEME};
pub use model::{
    AccentColor, AiConfig, AppConfig, EntryDetail, EntryFilter, EntryId, EntrySummary, Feed,
    FeedCategory, FeedId, Folder, FolderId, ImagePolicy, ACCENT_COLORS, FEED_CATEGORIES,
    THUMBNAIL_SIZE_DEFAULT, THUMBNAIL_SIZE_MAX, THUMBNAIL_SIZE_MIN,
};
pub use opml::{export_opml, parse_opml, OpmlOutline};
pub use paths::{
    cache_entries_dir, cache_favicons_dir, cache_images_dir, cache_thumbnails_dir, clear_all_cache,
    credentials_path, default_config_path, default_db_path, plugins_dir, set_custom_cache_dir,
};
pub use plugin::{
    Capabilities, Compliance, Credential, CredentialStore, Enhancer, EntryPatch, HostApi,
    InstallPreview, LoadedPlugin, Manifest, MatchRule, PluginManager, PluginMeta, Runtime, Tier,
    Tier1Config, Tier1FieldMap,
};
pub use reader_html::reader_document;
pub use sanitize::{sanitize_html, sanitize_html_with_policy};
pub use service::{run_refresh_task_with_ctx, GleanService, RefreshCtx};
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
        let ev = svc.handle(AppCommand::Bootstrap);
        assert!(ev.iter().any(|e| matches!(e, AppEvent::Ready)));
        assert!(ev.iter().any(|e| matches!(e, AppEvent::NavUpdated { .. })));
        // 空库时显式种 demo 数据（Bootstrap 不再自动 seed）。
        svc.store.seed_demo_if_empty().unwrap();
        let ev2 = svc.handle(AppCommand::ListEntries {
            filter: EntryFilter::All,
        });
        let entries = ev2.iter().find_map(|e| match e {
            AppEvent::EntriesUpdated { entries } => Some(entries.clone()),
            _ => None,
        });
        assert_eq!(entries.expect("entries").len(), 3);
    }

    #[test]
    fn open_entry_marks_read_and_returns_html() {
        let mut svc = GleanService::open_in_memory().unwrap();
        svc.handle(AppCommand::Bootstrap);
        svc.store.seed_demo_if_empty().unwrap();
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
            16,
            42,
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
        svc.handle(AppCommand::Bootstrap);
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
        let fid = store
            .add_feed(
                "T",
                "https://ex/feed.xml",
                None,
                crate::model::FeedCategory::Article,
            )
            .unwrap();
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
                    e.thumbnail.as_deref(),
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

    /// §1.5 conditional requests: etag/last_modified written by
    /// `update_feed_after_fetch` must round-trip through `get_feed_fetch_meta`
    /// so the next refresh sends If-None-Match / If-Modified-Since.
    #[test]
    fn etag_last_modified_roundtrip() {
        let mut store = Store::open_in_memory().unwrap();
        let fid = store
            .add_feed(
                "T",
                "https://ex/feed.xml",
                None,
                crate::model::FeedCategory::Article,
            )
            .unwrap();
        store
            .update_feed_after_fetch(
                fid,
                Some("T"),
                Some("https://ex"),
                Some(r#""abc123""#),
                Some("Wed, 21 Oct 2015 07:28:00 GMT"),
                None,
            )
            .unwrap();
        let (url, etag, lm, use_proxy) = store.get_feed_fetch_meta(fid).unwrap();
        assert_eq!(url, "https://ex/feed.xml");
        assert_eq!(etag.as_deref(), Some(r#""abc123""#));
        assert_eq!(lm.as_deref(), Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        assert!(!use_proxy);
    }
}
