//! UI → core commands.

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
    AutoRefresh,
    CreateFolder {
        name: String,
    },
    ImportOpml {
        content: String,
    },
    ExportOpml,
}
