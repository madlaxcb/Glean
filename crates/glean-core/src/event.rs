//! Core → UI events. UI projects state from these; do not poll full tables on a timer.

use crate::model::{EntryDetail, EntrySummary, Feed, Folder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppEvent {
    Ready,
    NavUpdated {
        folders: Vec<Folder>,
        feeds: Vec<Feed>,
        unread_total: u64,
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
}
