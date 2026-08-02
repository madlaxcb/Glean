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

/// 订阅的内容类型，用于导航栏分类分组：
/// 文章（纯文本）/ 社交媒体（图文）/ 图片 / 音乐 / 视频。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FeedCategory {
    /// 文章：文本内容。
    #[default]
    Article,
    /// 社交媒体：图文混排。
    Social,
    /// 图片。
    Image,
    /// 音乐 / 播客。
    Music,
    /// 视频。
    Video,
}

impl FeedCategory {
    /// 存储 / 序列化用字符串标识（与 `#[serde(rename_all = "snake_case")]` 一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            FeedCategory::Article => "article",
            FeedCategory::Social => "social",
            FeedCategory::Image => "image",
            FeedCategory::Music => "music",
            FeedCategory::Video => "video",
        }
    }

    /// 从存储字符串解析；未知值回退为 Article（向前兼容）。
    pub fn from_str(s: &str) -> Self {
        match s {
            "social" => FeedCategory::Social,
            "image" => FeedCategory::Image,
            "music" => FeedCategory::Music,
            "video" => FeedCategory::Video,
            _ => FeedCategory::Article,
        }
    }

    /// 导航栏展示名。
    pub fn label(&self) -> &'static str {
        match self {
            FeedCategory::Article => "文章",
            FeedCategory::Social => "社交媒体",
            FeedCategory::Image => "图片",
            FeedCategory::Music => "音乐",
            FeedCategory::Video => "视频",
        }
    }

    /// 导航栏分组图标。
    pub fn icon(&self) -> &'static str {
        match self {
            FeedCategory::Article => "📄",
            FeedCategory::Social => "💬",
            FeedCategory::Image => "🖼",
            FeedCategory::Music => "🎵",
            FeedCategory::Video => "🎬",
        }
    }
}

/// 所有分类的有序遍历（导航栏固定顺序：文章 → 社交媒体 → 图片 → 音乐 → 视频）。
pub const FEED_CATEGORIES: [FeedCategory; 5] = [
    FeedCategory::Article,
    FeedCategory::Social,
    FeedCategory::Image,
    FeedCategory::Music,
    FeedCategory::Video,
];

/// UI 主题强调色（设置页可选）：影响选中背景、悬停高亮、链接颜色。
/// core 不依赖 egui，颜色以 RGB 元组暴露，由 UI 层转成 Color32。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccentColor {
    #[default]
    Blue,
    Purple,
    Green,
    Orange,
    Pink,
    Teal,
}

/// 主题色的有序遍历（设置页色板固定顺序）。
pub const ACCENT_COLORS: [AccentColor; 6] = [
    AccentColor::Blue,
    AccentColor::Purple,
    AccentColor::Green,
    AccentColor::Orange,
    AccentColor::Pink,
    AccentColor::Teal,
];

impl AccentColor {
    /// 设置页展示名。
    pub fn label(&self) -> &'static str {
        match self {
            AccentColor::Blue => "蓝",
            AccentColor::Purple => "紫",
            AccentColor::Green => "绿",
            AccentColor::Orange => "橙",
            AccentColor::Pink => "粉",
            AccentColor::Teal => "青",
        }
    }

    /// 强调色 RGB。dark = true 时返回提亮变体，保证深色背景上的对比度。
    pub fn rgb(&self, dark: bool) -> (u8, u8, u8) {
        if dark {
            match self {
                AccentColor::Blue => (110, 160, 250),
                AccentColor::Purple => (185, 145, 250),
                AccentColor::Green => (95, 200, 130),
                AccentColor::Orange => (250, 165, 90),
                AccentColor::Pink => (245, 135, 175),
                AccentColor::Teal => (90, 200, 195),
            }
        } else {
            match self {
                AccentColor::Blue => (30, 100, 215),
                AccentColor::Purple => (130, 75, 210),
                AccentColor::Green => (25, 140, 75),
                AccentColor::Orange => (215, 110, 20),
                AccentColor::Pink => (205, 55, 110),
                AccentColor::Teal => (15, 135, 130),
            }
        }
    }
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
    /// 内容类型分类（导航栏分组依据）。
    #[serde(default)]
    pub category: FeedCategory,
    /// 是否使用设置页配置的 HTTP 代理抓取该订阅。false = 直连。
    #[serde(default)]
    pub use_proxy: bool,
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
    /// 缩略图/封面图 URL（列表预览用，可能为空）。
    #[serde(default)]
    pub thumbnail_url: Option<String>,
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
#[serde(default)]
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
    /// 开启「使用代理」的插件 id 列表（§11.5.10）。命中插件后其请求走代理，
    /// 覆盖订阅级开关。
    #[serde(default)]
    pub plugin_proxy: Vec<String>,
    /// UI 主题强调色（设置页色板可选）。
    #[serde(default)]
    pub accent: AccentColor,
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
            plugin_proxy: Vec::new(),
            accent: AccentColor::default(),
        }
    }
}
