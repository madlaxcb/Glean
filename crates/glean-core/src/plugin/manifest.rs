//! 插件清单 (manifest.toml) serde 结构。§11.5.8
//!
//! ```toml
//! [plugin]
//! id = "pixiv"
//! name = "Pixiv 订阅适配器"
//! version = "0.1.0"
//! author = "Glean"
//! min_glean_version = "0.5.0"
//! tier = 2  # 0=内置 / 1=配置 / 2=脚本
//!
//! [[match]]
//! url_pattern = "pixiv.net/users/*"
//!
//! [capabilities]
//! feed_fetch = ["app-api.pixiv.net", "i.pximg.net"]
//! credential_use = ["pixiv_session"]
//! content_transform = ["embed_ref"]
//!
//! [compliance]
//! uses_user_session = true
//!
//! [tier1]                      # Tier 1 配置（仅 tier = 1 时有意义）
//! request_url_template = "https://api.example.com/users/{uid}/videos"
//! entries_json_path = "$.data.list"
//! ```

use serde::{Deserialize, Serialize};

/// 插件层级。§11.5.2 / §11.5.8：manifest 中以整数表示（0/1/2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(into = "u8", try_from = "u8")]
#[repr(u8)]
pub enum Tier {
    /// 内置（不通过插件系统加载，仅占位）
    #[default]
    Builtin = 0,
    /// 纯配置驱动
    Config = 1,
    /// Rhai 脚本
    Script = 2,
}

impl Tier {
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

impl From<Tier> for u8 {
    fn from(t: Tier) -> u8 {
        t as u8
    }
}

impl TryFrom<u8> for Tier {
    type Error = String;
    fn try_from(v: u8) -> std::result::Result<Self, Self::Error> {
        match v {
            0 => Ok(Self::Builtin),
            1 => Ok(Self::Config),
            2 => Ok(Self::Script),
            _ => Err(format!("unknown tier: {v} (expected 0/1/2)")),
        }
    }
}

/// 顶层 manifest.toml 结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub plugin: PluginMeta,
    #[serde(default)]
    pub r#match: Vec<MatchRule>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub compliance: Compliance,
    /// Tier 1 配置；仅当 `tier = 1` 时有意义。
    #[serde(default)]
    pub tier1: Option<Tier1Config>,
}

/// `[plugin]` 段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub min_glean_version: String,
    #[serde(default)]
    pub tier: Tier,
}

/// `[[match]]` 段：URL 匹配规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchRule {
    /// 形如 `pixiv.net/users/*`，`*` 匹配任意后缀。
    pub url_pattern: String,
}

/// `[capabilities]` 段：能力原语 + 作用域。§11.5.4
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// `feed.fetch` 域名白名单。
    #[serde(default)]
    pub feed_fetch: Vec<String>,
    /// `credential.use:<slot>` 具名凭证槽。
    #[serde(default)]
    pub credential_use: Vec<String>,
    /// `content.transform`：可写字段白名单。
    #[serde(default)]
    pub content_transform: Vec<String>,
    /// `external.call:<domain>`：可调用的外部服务域名。
    #[serde(default)]
    pub external_call: Vec<String>,
}

impl Capabilities {
    pub fn is_empty(&self) -> bool {
        self.feed_fetch.is_empty()
            && self.credential_use.is_empty()
            && self.content_transform.is_empty()
            && self.external_call.is_empty()
    }

    /// 相对 `old` 新增的能力项（各字段取差集并集）。用于插件更新时的权限
    /// 变更判定（§11.5.4 安装/更新时权限确认：能力扩大须重新确认）。
    pub fn new_items_relative_to(&self, old: &Capabilities) -> Capabilities {
        fn diff(new: &[String], old: &[String]) -> Vec<String> {
            new.iter().filter(|x| !old.contains(x)).cloned().collect()
        }
        Capabilities {
            feed_fetch: diff(&self.feed_fetch, &old.feed_fetch),
            credential_use: diff(&self.credential_use, &old.credential_use),
            content_transform: diff(&self.content_transform, &old.content_transform),
            external_call: diff(&self.external_call, &old.external_call),
        }
    }

    /// 是否比 `old` 扩大了权限（新增了任一能力项）。
    pub fn grows_from(&self, old: &Capabilities) -> bool {
        !self.new_items_relative_to(old).is_empty()
    }
}

/// `[compliance]` 段：合规声明。§11.5.2
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Compliance {
    #[serde(default)]
    pub uses_user_session: bool,
}

/// `[tier1]` 段：纯配置驱动的适配器规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tier1Config {
    /// 请求 URL 模板，`{var}` 会被 path 提取出的变量替换。
    pub request_url_template: String,
    /// JSON 路径定位条目数组，例如 `$.data.list`。
    pub entries_json_path: String,
    /// 每个条目的字段映射。
    #[serde(default)]
    pub fields: Tier1FieldMap,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tier1FieldMap {
    /// JSON 路径（相对条目根），如 `$.bvid`。
    pub guid: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub author: Option<String>,
    /// Unix 秒。JSON 路径或字面量。
    pub published_at: Option<String>,
    pub summary: Option<String>,
    /// 内容 HTML 模板，`{var}` 会被条目字段替换。
    pub content_html_template: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tier2_manifest() {
        let toml = r#"
[plugin]
id = "pixiv"
name = "Pixiv 订阅适配器"
version = "0.1.0"
author = "Glean"
min_glean_version = "0.5.0"
tier = 2

[[match]]
url_pattern = "pixiv.net/users/*"

[capabilities]
feed_fetch = ["app-api.pixiv.net", "i.pximg.net"]
credential_use = ["pixiv_session"]
content_transform = ["embed_ref"]

[compliance]
uses_user_session = true
"#;
        let m: Manifest = toml::from_str(toml).unwrap();
        assert_eq!(m.plugin.id, "pixiv");
        assert_eq!(m.plugin.tier, Tier::Script);
        assert_eq!(m.r#match.len(), 1);
        assert_eq!(
            m.capabilities.feed_fetch,
            vec!["app-api.pixiv.net", "i.pximg.net"]
        );
        assert!(m.tier1.is_none());
        assert!(m.compliance.uses_user_session);
    }

    #[test]
    fn parse_tier1_manifest() {
        let toml = r#"
[plugin]
id = "bilibili"
name = "Bilibili 用户投稿"
version = "0.1.0"
tier = 1

[[match]]
url_pattern = "space.bilibili.com/*"

[capabilities]
feed_fetch = ["api.bilibili.com"]

[tier1]
request_url_template = "https://api.bilibili.com/x/space/arc/search?mid={uid}"
entries_json_path = "$.data.list.vlist"

[tier1.fields]
guid = "$.bvid"
title = "$.title"
url = "https://www.bilibili.com/video/{bvid}"
published_at = "$.created"
"#;
        let m: Manifest = toml::from_str(toml).unwrap();
        assert_eq!(m.plugin.tier, Tier::Config);
        let t1 = m.tier1.expect("tier1 config");
        assert_eq!(
            t1.request_url_template,
            "https://api.bilibili.com/x/space/arc/search?mid={uid}"
        );
        assert_eq!(t1.fields.guid.as_deref(), Some("$.bvid"));
        assert_eq!(
            t1.fields.url.as_deref(),
            Some("https://www.bilibili.com/video/{bvid}")
        );
    }

    #[test]
    fn empty_capabilities_default() {
        let toml = r#"
[plugin]
id = "x"
name = "X"
version = "0.1"
"#;
        let m: Manifest = toml::from_str(toml).unwrap();
        assert!(m.capabilities.is_empty());
        assert_eq!(m.plugin.tier, Tier::Builtin);
    }

    #[test]
    fn capabilities_growth_detection() {
        // 相同能力 → 不扩大
        let old = Capabilities {
            feed_fetch: vec!["a.com".into()],
            credential_use: vec!["tok".into()],
            content_transform: vec!["embed_ref".into()],
            external_call: vec!["api.example.com".into()],
        };
        let same = old.clone();
        assert!(!same.grows_from(&old));
        assert!(same.new_items_relative_to(&old).is_empty());

        // 新增一个域名 → 扩大，且 diff 只含新增项
        let grown = Capabilities {
            feed_fetch: vec!["a.com".into(), "b.com".into()],
            ..old.clone()
        };
        assert!(grown.grows_from(&old));
        let added = grown.new_items_relative_to(&old);
        assert_eq!(added.feed_fetch, vec!["b.com".to_string()]);
        assert!(added.credential_use.is_empty());
        assert!(added.content_transform.is_empty());
        assert!(added.external_call.is_empty());
    }

    #[test]
    fn capabilities_shrunk_is_not_growth() {
        // 新版去掉了一个域名 → 权限缩小，不算扩大
        let old = Capabilities {
            feed_fetch: vec!["a.com".into(), "b.com".into()],
            ..Default::default()
        };
        let shrunk = Capabilities {
            feed_fetch: vec!["a.com".into()],
            ..Default::default()
        };
        assert!(!shrunk.grows_from(&old));
        assert!(shrunk.new_items_relative_to(&old).is_empty());
    }
}
