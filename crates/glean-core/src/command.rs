//! UI → core commands.

use crate::model::{EntryFilter, EntryId, FeedId, FolderId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppCommand {
    /// Ensure schema and optionally load offline demo rows.
    Bootstrap {
        seed_demo: bool,
    },
    RefreshNav,
    ListEntries {
        filter: EntryFilter,
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
    /// Insert feed row without network (tests).
    AddFeedLocal {
        title: String,
        feed_url: String,
        folder_id: Option<FolderId>,
    },
    AddEntryLocal {
        feed_id: FeedId,
        guid: String,
        title: String,
        url: Option<String>,
        content_html: String,
    },
    /// Fetch URL, parse feed, insert feed + entries (M1).
    AddFeedFromUrl {
        feed_url: String,
        folder_id: Option<FolderId>,
    },
    /// Refresh one feed or all (None).
    RefreshFeeds {
        feed_id: Option<FeedId>,
    },
}
