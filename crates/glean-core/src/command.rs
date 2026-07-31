//! UI → core commands.

use crate::ai::EnhanceAction;
use crate::model::{EntryFilter, EntryId, FeedId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppCommand {
    Bootstrap {
        seed_demo: bool,
    },
    RefreshNav,
    ListEntries {
        filter: EntryFilter,
    },
    SearchEntries {
        query: String,
    },
    OpenEntry {
        id: EntryId,
    },
    MarkRead {
        id: EntryId,
        read: bool,
    },
    ToggleStar {
        id: EntryId,
    },
    MarkAllRead {
        feed_id: Option<FeedId>,
    },
    AddFeedLocal {
        title: String,
        feed_url: String,
    },
    AddEntryLocal {
        feed_id: FeedId,
        guid: String,
        title: String,
        url: Option<String>,
        content_html: String,
    },
    AddFeedFromUrl {
        feed_url: String,
    },
    DeleteFeed {
        id: FeedId,
    },
    RenameFeed {
        id: FeedId,
        title: String,
    },
    EditFeedUrl {
        id: FeedId,
        feed_url: String,
    },
    RefreshFeeds {
        feed_id: Option<FeedId>,
    },
    MoveFeedToFolder {
        feed_id: FeedId,
        folder_id: Option<crate::model::FolderId>,
    },
    ToggleMuteFeed {
        id: FeedId,
    },
    SetFeedRefreshInterval {
        id: FeedId,
        secs: i64,
    },
    SetFeedCategory {
        id: FeedId,
        category: crate::model::FeedCategory,
    },
    ToggleFeedProxy {
        id: FeedId,
    },
    AutoRefresh,
    CreateFolder {
        name: String,
    },
    ImportOpml {
        content: String,
    },
    ExportOpml,
    /// Manually trigger full-text extraction for an entry (auto-extract can
    /// be disabled in config). UI may also call prepare/apply for async flow.
    ExtractEntry {
        id: EntryId,
    },
    /// Manually trigger AI enhance (summary/translate) for an entry.
    /// Sync fallback; UI normally uses prepare_enhance_task + apply_enhance_outcome
    /// for async (AI calls are slow, must not block UI).
    EnhanceEntry {
        id: EntryId,
        action: EnhanceAction,
    },
}
