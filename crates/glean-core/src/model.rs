//! Domain models for Glean (M0b+).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FolderId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FeedId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub name: String,
    pub sort_key: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    pub id: FeedId,
    pub folder_id: Option<FolderId>,
    pub title: String,
    pub site_url: Option<String>,
    pub feed_url: String,
    pub last_error: Option<String>,
    pub muted: bool,
    /// Refresh interval in seconds; 0 = use global default.
    pub refresh_interval_secs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySummary {
    pub id: EntryId,
    pub feed_id: FeedId,
    pub title: String,
    pub url: Option<String>,
    pub published_at: Option<i64>,
    pub is_read: bool,
    pub is_starred: bool,
    /// True if content_html is non-empty (cached / offline-readable).
    pub has_content: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryDetail {
    pub summary: EntrySummary,
    pub author: Option<String>,
    pub content_html: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EntryFilter {
    #[default]
    All,
    Unread,
    Starred,
    Feed(FeedId),
}

// --- App config (persisted as JSON next to the DB) ---

/// Remote image policy for the reader (dev plan §2.4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ImagePolicy {
    /// Strip all remote images (privacy default).
    #[default]
    Block,
    /// Strip at render; a per-article "显示图片" button re-renders with Allow.
    LoadOnDemand,
    /// Keep img tags; WebView loads them.
    Allow,
}

impl ImagePolicy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Block => "拦截远程图片",
            Self::LoadOnDemand => "按需加载图片",
            Self::Allow => "允许远程图片",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Block => Self::LoadOnDemand,
            Self::LoadOnDemand => Self::Allow,
            Self::Allow => Self::Block,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub dark: bool,
    pub nav_width: f32,
    pub list_width: f32,
    pub image_policy: ImagePolicy,
    /// Global default refresh interval in seconds (0 = manual only).
    pub refresh_interval_secs: i64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            dark: false,
            nav_width: 200.0,
            list_width: 320.0,
            image_policy: ImagePolicy::Block,
            refresh_interval_secs: 0,
        }
    }
}
