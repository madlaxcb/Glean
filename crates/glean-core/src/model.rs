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
    /// AI 增强结果（摘要/翻译），(kind, content) 列表，按 created_at 升序。
    /// 空表示尚未生成。`#[serde(default)]` 保证旧序列化数据向后兼容。
    #[serde(default)]
    pub enhancements: Vec<(String, String)>,
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
    /// Persisted window outer position (screen coords, egui points).
    /// None = let OS decide (first launch).
    #[serde(default)]
    pub window_x: Option<f32>,
    #[serde(default)]
    pub window_y: Option<f32>,
    /// Persisted window inner size (egui points).
    #[serde(default)]
    pub window_w: Option<f32>,
    #[serde(default)]
    pub window_h: Option<f32>,
    /// Persisted window maximized state.
    #[serde(default)]
    pub window_maximized: bool,
    /// AI 增强配置（摘要/翻译）。`None` = 未配置，UI 隐藏增强按钮。
    /// `api_key_cipher` 是 `encrypt_secret` 输出的 JSON blob，不在内存里留明文。
    #[serde(default)]
    pub ai: Option<AiConfig>,
    /// 已停用的插件 id 列表（「插件管理」界面启停；路由跳过停用插件）。
    #[serde(default)]
    pub disabled_plugins: Vec<String>,
}

/// OpenAI 兼容协议的 AI 配置。§11.5.13 Enhancer。
///
/// `base_url` 形如 `https://api.openai.com/v1` 或 DeepSeek/通义/Kimi 的兼容端点。
/// `api_key_cipher` = `crate::plugin::credential::encrypt_secret(api_key)`。
/// 明文 api_key 只在使用时短暂解密，不持久化、不入 AppConfig 内存常驻。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    pub base_url: String,
    pub model: String,
    pub api_key_cipher: String,
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
            window_x: None,
            window_y: None,
            window_w: None,
            window_h: None,
            window_maximized: false,
            ai: None,
            disabled_plugins: Vec::new(),
        }
    }
}
