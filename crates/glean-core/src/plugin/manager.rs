//! PluginManager：扫描插件目录、加载 manifest、按 URL 路由到适配器。§11.5.8
//!
//! 插件目录布局：
//! ```text
//! <data_dir>/plugins/
//!   bilibili/
//!     manifest.toml
//!     adapter.rhai       # Tier 2 时存在
//!   pixiv/
//!     manifest.toml
//!     adapter.rhai
//! ```
//!
//! M5 状态：
//! - 扫描 + manifest 解析 + URL 路由已就绪
//! - Tier 1 执行入口（`run_tier1_for_url`）已就绪，调用 `tier1::run`
//! - Tier 2 执行入口（`run_tier2_for_url`）已就绪，构建 Runtime + 加载脚本，
//!   Entry 收集器接入排到 M6

use crate::error::{CoreError, Result};
use crate::feed::parse::ParsedFeed;
use crate::feed::HttpClient;
use crate::plugin::builtin;
use crate::plugin::credential::CredentialStore;
use crate::plugin::manifest::{Manifest, MatchRule};
use crate::plugin::runtime::Runtime;
use crate::plugin::tier1;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 已加载的插件：manifest + 可选脚本内容。
///
/// `script` 直接持有脚本字符串（避免每次执行重新读盘）。来源：
/// - 磁盘插件：`<dir>/adapter.rhai` 读出的内容
/// - 内置插件：`include_str!` 嵌入的静态字符串
/// - Tier 1 / 内置 Tier 0：`None`
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub script: Option<String>,
}

/// PluginManager：管理已加载的插件集合并提供 URL 路由。
pub struct PluginManager {
    plugins: Vec<LoadedPlugin>,
    plugins_dir: PathBuf,
}

impl PluginManager {
    /// 创建空 manager。`plugins_dir` 不存在时返回空列表（不报错）。
    pub fn new(plugins_dir: PathBuf) -> Result<Self> {
        let mut mgr = Self {
            plugins: Vec::new(),
            plugins_dir,
        };
        mgr.scan()?;
        Ok(mgr)
    }

    /// 测试用：从给定 manifest 列表构造，不读盘。
    pub fn from_manifests(plugins: Vec<Manifest>) -> Self {
        let plugins = plugins
            .into_iter()
            .map(|m| LoadedPlugin {
                manifest: m,
                dir: PathBuf::new(),
                script: None,
            })
            .collect();
        Self {
            plugins,
            plugins_dir: PathBuf::new(),
        }
    }

    /// 扫描 `<plugins_dir>/<id>/manifest.toml`，再合并内置插件（磁盘优先）。
    fn scan(&mut self) -> Result<()> {
        self.plugins.clear();
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1. 磁盘插件
        if !self.plugins_dir.as_os_str().is_empty() {
            if let Ok(entries) = std::fs::read_dir(&self.plugins_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let manifest_path = path.join("manifest.toml");
                    if !manifest_path.is_file() {
                        continue;
                    }
                    let bytes = match std::fs::read(&manifest_path) {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    let text = match std::str::from_utf8(&bytes) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let manifest: Manifest = match toml::from_str(text) {
                        Ok(m) => m,
                        Err(_) => continue, // 解析失败的插件跳过，不阻塞其他插件
                    };
                    seen_ids.insert(manifest.plugin.id.clone());
                    let script_path = path.join("adapter.rhai");
                    let script = if script_path.is_file() {
                        std::fs::read_to_string(&script_path).ok()
                    } else {
                        None
                    };
                    self.plugins.push(LoadedPlugin {
                        manifest,
                        dir: path,
                        script,
                    });
                }
            }
        }

        // 2. 内置插件（磁盘没有同名 id 时才加入，磁盘优先）
        for b in builtin::all() {
            if seen_ids.contains(b.id) {
                continue;
            }
            let manifest: Manifest = match toml::from_str(b.manifest_toml) {
                Ok(m) => m,
                Err(e) => {
                    // 内置 manifest 解析失败是 bug，开发期直接 panic 提示
                    panic!("builtin plugin '{}' manifest parse error: {e}", b.id);
                }
            };
            self.plugins.push(LoadedPlugin {
                manifest,
                dir: PathBuf::new(),
                script: b.adapter_rhai.map(|s| s.to_string()),
            });
        }

        Ok(())
    }

    pub fn list(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// 按 URL 匹配规则找到对应插件。§11.5.8 `[[match]]` 段。
    pub fn find_for_url(&self, url: &str) -> Option<&LoadedPlugin> {
        self.plugins
            .iter()
            .find(|p| p.manifest.r#match.iter().any(|r| matches(url, r)))
    }

    /// 执行 Tier 1 适配器（如果 URL 命中的是 Tier 1 插件）。
    /// 返回 `Ok(None)` 表示没有命中任何插件，调用方走默认 RSS 流程。
    pub fn run_tier1_for_url(&self, url: &str, http: &HttpClient) -> Result<Option<ParsedFeed>> {
        let Some(plugin) = self.find_for_url(url) else {
            return Ok(None);
        };
        if !matches!(
            plugin.manifest.plugin.tier,
            crate::plugin::manifest::Tier::Config
        ) {
            return Ok(None);
        }
        let parsed = tier1::run(&plugin.manifest, http, url)?;
        Ok(Some(parsed))
    }

    /// 执行 Tier 2 适配器（如果 URL 命中的是 Tier 2 插件）。
    ///
    /// M6：构建 Runtime + 加载脚本 + EntryCollector 接入。`run_script` 直接
    /// 返回 `ParsedFeed`。`credentials = None` 表示无凭证存储（in-memory
    /// 模式），Tier 2 插件得到空 CredentialStore——声明了 credential_use 的
    /// 插件会因 slot 找不到而拿到 status=0 响应。
    pub fn run_tier2_for_url(
        &self,
        url: &str,
        http: Arc<HttpClient>,
        credentials: Option<Arc<CredentialStore>>,
    ) -> Result<Option<ParsedFeed>> {
        let Some(plugin) = self.find_for_url(url) else {
            return Ok(None);
        };
        if !matches!(
            plugin.manifest.plugin.tier,
            crate::plugin::manifest::Tier::Script
        ) {
            return Ok(None);
        }
        let Some(script) = &plugin.script else {
            return Err(CoreError::Message(format!(
                "tier2 plugin '{}' missing adapter.rhai",
                plugin.manifest.plugin.id
            )));
        };
        let creds = credentials.unwrap_or_else(|| Arc::new(CredentialStore::in_memory()));
        let rt = Runtime::build(plugin.manifest.clone(), http, creds);
        let parsed = rt.run_script(script, url)?;
        Ok(Some(parsed))
    }
}

/// 匹配 `url_pattern` 与 `url`。
///
/// 规则：`*` 匹配任意后缀（含路径分隔符）；其余字符精确匹配。
/// 形如 `pixiv.net/users/*` 匹配 `https://www.pixiv.net/users/12345`。
fn matches(url: &str, rule: &MatchRule) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or("");
    let path = parsed.path().trim_start_matches('/');
    let target = if path.is_empty() {
        host.to_string()
    } else {
        format!("{host}/{path}")
    };
    // 去掉 www. 前缀做匹配
    let target = target.trim_start_matches("www.");

    let pattern = rule.url_pattern.trim_start_matches("www.");
    glob_match(pattern, target)
}

/// 极简 glob：`*` 匹配任意字符序列。
fn glob_match(pattern: &str, target: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = target.chars().collect();
    glob_match_inner(&p, &t)
}

fn glob_match_inner(p: &[char], t: &[char]) -> bool {
    match (p.first(), t.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            // 尝试匹配 0 个或多个字符
            if glob_match_inner(&p[1..], t) {
                return true;
            }
            if !t.is_empty() {
                return glob_match_inner(p, &t[1..]);
            }
            false
        }
        (Some(pc), Some(tc)) if pc == tc => glob_match_inner(&p[1..], &t[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::{Capabilities, Compliance, PluginMeta, Tier};

    fn make_manifest(id: &str, pattern: &str, tier: Tier) -> Manifest {
        Manifest {
            plugin: PluginMeta {
                id: id.into(),
                name: id.into(),
                version: "0.1".into(),
                author: "".into(),
                min_glean_version: "".into(),
                tier,
            },
            r#match: vec![MatchRule {
                url_pattern: pattern.into(),
            }],
            capabilities: Capabilities::default(),
            compliance: Compliance::default(),
            tier1: None,
        }
    }

    #[test]
    fn glob_match_basic() {
        assert!(glob_match("pixiv.net/users/*", "pixiv.net/users/12345"));
        assert!(glob_match(
            "space.bilibili.com/*",
            "space.bilibili.com/12345/video"
        ));
        assert!(!glob_match("pixiv.net/users/*", "pixiv.net/artworks/123"));
        assert!(glob_match("example.com", "example.com"));
        assert!(glob_match("example.com/*", "example.com/"));
    }

    #[test]
    fn find_for_url_returns_matching_plugin() {
        let mut m = make_manifest("pixiv", "pixiv.net/users/*", Tier::Script);
        m.capabilities.feed_fetch = vec!["app-api.pixiv.net".into()];
        let mgr = PluginManager::from_manifests(vec![m]);
        let hit = mgr
            .find_for_url("https://www.pixiv.net/users/12345")
            .expect("matched");
        assert_eq!(hit.manifest.plugin.id, "pixiv");
    }

    #[test]
    fn find_for_url_returns_none_when_no_match() {
        let m = make_manifest("pixiv", "pixiv.net/users/*", Tier::Script);
        let mgr = PluginManager::from_manifests(vec![m]);
        assert!(mgr.find_for_url("https://example.com/feed.xml").is_none());
    }

    #[test]
    fn run_tier1_returns_none_for_non_tier1_plugin() {
        let m = make_manifest("pixiv", "pixiv.net/users/*", Tier::Script);
        let mgr = PluginManager::from_manifests(vec![m]);
        let http = HttpClient::default();
        let result = mgr
            .run_tier1_for_url("https://www.pixiv.net/users/123", &http)
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn run_tier1_returns_none_when_no_plugin_matches() {
        let mgr = PluginManager::from_manifests(vec![]);
        let http = HttpClient::default();
        let result = mgr
            .run_tier1_for_url("https://example.com/feed.xml", &http)
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn new_with_missing_dir_loads_builtins() {
        // 即使磁盘插件目录不存在，内置插件（bilibili）也应被加载。
        let tmp = std::env::temp_dir().join(format!("glean-plugin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mgr = PluginManager::new(tmp.clone()).expect("open");
        assert!(
            mgr.list()
                .iter()
                .any(|p| p.manifest.plugin.id == "bilibili"),
            "bilibili builtin should be loaded"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn scan_loads_manifest_toml() {
        let tmp = std::env::temp_dir().join(format!("glean-plugin-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugin_dir = tmp.join("test-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
[plugin]
id = "test-plugin"
name = "Test"
version = "0.1"
tier = 1

[[match]]
url_pattern = "example.com/*"

[capabilities]
feed_fetch = ["api.example.com"]

[tier1]
request_url_template = "https://api.example.com/items"
entries_json_path = "$.items"

[tier1.fields]
guid = "$.id"
title = "$.name"
"#,
        )
        .unwrap();

        let mgr = PluginManager::new(tmp.clone()).expect("open");
        // 磁盘 test-plugin + 内置 bilibili
        let ids: Vec<&str> = mgr
            .list()
            .iter()
            .map(|p| p.manifest.plugin.id.as_str())
            .collect();
        assert!(ids.contains(&"test-plugin"), "test-plugin should be loaded");
        assert!(
            ids.contains(&"bilibili"),
            "bilibili builtin should be loaded"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 内置 bilibili 插件的 manifest.toml 能正确解析，且能力声明符合预期。
    #[test]
    fn builtin_bilibili_manifest_parses() {
        let toml_text = include_str!("builtin/bilibili/manifest.toml");
        let m: Manifest = toml::from_str(toml_text).expect("bilibili manifest parse");
        assert_eq!(m.plugin.id, "bilibili");
        assert_eq!(m.plugin.tier, crate::plugin::manifest::Tier::Script);
        assert!(m
            .capabilities
            .feed_fetch
            .contains(&"api.bilibili.com".to_string()));
        assert!(!m.compliance.uses_user_session);
    }

    /// 端到端：用内置 bilibili 插件订阅 `space.bilibili.com/2`（碧诗）。
    /// 验证 wbi 签名 + buvid3 流程能拿到真实视频列表。
    /// 手动跑：`cargo test -p glean-core -- --ignored bilibili_end_to_end`
    ///
    /// 注意：在数据中心 IP 环境下可能被风控（-352 / -799）；用户住宅 IP 通常正常。
    #[test]
    #[ignore = "需联网访问 api.bilibili.com（匿名，可能被风控）"]
    fn bilibili_end_to_end() {
        let tmp = std::env::temp_dir().join(format!("glean-bili-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mgr = PluginManager::new(tmp.clone()).expect("open");
        let _ = std::fs::remove_dir_all(&tmp);

        let http = Arc::new(HttpClient::default());
        let parsed = mgr
            .run_tier2_for_url("https://space.bilibili.com/2", http, None)
            .expect("run_tier2")
            .expect("matched plugin");

        assert_eq!(parsed.title, "Bilibili 用户投稿");
        assert!(!parsed.entries.is_empty(), "应至少拿到 1 个视频");
        let first = &parsed.entries[0];
        assert!(
            first.guid.starts_with("BV"),
            "guid 应是 bvid，got: {}",
            first.guid
        );
        assert!(!first.title.is_empty());
        assert!(
            first
                .url
                .as_deref()
                .unwrap_or("")
                .starts_with("https://www.bilibili.com/video/"),
            "url 应指向视频页"
        );
        assert!(first.author.is_some(), "author 不应为空");
        assert!(first.published_at.is_some(), "published_at 不应为空");
        assert!(
            first.content_html.contains("<img"),
            "content_html 应含封面图"
        );
    }
}
