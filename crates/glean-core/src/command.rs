//! UI → core commands.

use crate::model::{EntryFilter, EntryId, FeedId, FolderId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppCommand {
    /// Ensure schema and optionally load demo rows (no network).
    Bootstrap {
        seed_demo: bool,
    },
    /// List folders + feeds for nav projection.
    RefreshNav,
    /// List entries for the middle pane.
    ListEntries {
        filter: EntryFilter,
    },
    /// Open one entry in the reader (loads HTML from store).
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
    /// Insert a feed row without fetching (M0b / tests). Network fetch is M1.
    AddFeedLocal {
        title: String,
        feed_url: String,
        folder_id: Option<FolderId>,
    },
    /// Insert a local-only entry (tests / demo).
    AddEntryLocal {
        feed_id: FeedId,
        guid: String,
        title: String,
        url: Option<String>,
        content_html: String,
    },
}
