//! Core → UI events. UI projects state from these; do not poll full tables on a timer.

use crate::model::{EntryDetail, EntryId, EntrySummary, Feed, FeedId, Folder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppEvent {
    Ready,
    NavUpdated {
        folders: Vec<Folder>,
        feeds: Vec<Feed>,
        unread_total: u64,
        unread_per_feed: HashMap<FeedId, u64>,
    },
    EntriesUpdated {
        entries: Vec<EntrySummary>,
    },
    EntryOpened {
        entry: EntryDetail,
    },
    UnreadChanged {
        total: u64,
    },
    Status {
        message: String,
    },
    Error {
        message: String,
    },
    OpmlExported {
        xml: String,
    },
    /// Emitted when full-text extraction completes. UI should re-open the
    /// entry (or update the reader if currently visible) to show full body.
    EntryExtracted {
        id: EntryId,
        success: bool,
    },
}
