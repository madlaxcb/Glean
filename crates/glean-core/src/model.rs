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
    /// Favicon URL (discovered from feed or site HTML). None if not yet
    /// resolved; Some("") if resolution attempted but no icon found.
    pub favicon_url: Option<String>,
    /// Consecutive refresh failures. Reset to 0 on success.
    pub consecutive_failures: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySummary {
    pub id: EntryId,
    pub feed_id: Option<FeedId>,
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
    /// Full-text extracted from the original article URL via readability.
    /// Empty if not extracted yet or extraction failed. When non-empty, the
    /// reader prefers this over `content_html`.
    pub extracted_html: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EntryFilter {
    #[default]
    All,
    Unread,
    Starred,
    Today,
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
    /// Auto-extract full text from the original article URL when a feed entry
    /// only ships a short summary. Default on (dev plan §2.4 P2).
    #[serde(default = "default_true")]
    pub auto_extract: bool,
    /// Download and locally cache remote images (dev plan §2.5.2). When on,
    /// img src is rewritten to the glean-img:// custom scheme. Default off
    /// (bandwidth + privacy tradeoff: first load still hits the source).
    #[serde(default)]
    pub cache_images: bool,
    /// Reader font size in pixels (default 16).
    #[serde(default = "default_font_size")]
    pub font_size_px: u16,
    /// Reader line width in rem (default 42).
    #[serde(default = "default_line_width")]
    pub line_width_rem: u16,
    /// HTTP proxy URL (e.g., "http://127.0.0.1:7890", "socks5://…"). Empty = no proxy.
    #[serde(default)]
    pub proxy_url: String,
}

fn default_true() -> bool {
    true
}

fn default_font_size() -> u16 {
    16
}

fn default_line_width() -> u16 {
    42
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            dark: false,
            nav_width: 200.0,
            list_width: 320.0,
            image_policy: ImagePolicy::Block,
            refresh_interval_secs: 0,
            auto_extract: true,
            cache_images: false,
            font_size_px: 16,
            line_width_rem: 42,
            proxy_url: String::new(),
        }
    }
}
