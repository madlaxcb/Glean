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
use crate::plugin::credential::CredentialStore;
use crate::plugin::manifest::{Manifest, MatchRule};
use crate::plugin::runtime::Runtime;
use crate::plugin::tier1;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 已加载的插件：manifest + 可选脚本路径。
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub script_path: Option<PathBuf>,
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
                script_path: None,
            })
            .collect();
        Self {
            plugins,
            plugins_dir: PathBuf::new(),
        }
    }

    /// 扫描 `<plugins_dir>/<id>/manifest.toml`。
    fn scan(&mut self) -> Result<()> {
        self.plugins.clear();
        if self.plugins_dir.as_os_str().is_empty() {
            return Ok(());
        }
        let Ok(entries) = std::fs::read_dir(&self.plugins_dir) else {
            return Ok(()); // 目录不存在视为空
        };
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
            let script_path = path.join("adapter.rhai");
            let script_path = if script_path.is_file() {
                Some(script_path)
            } else {
                None
            };
            self.plugins.push(LoadedPlugin {
                manifest,
                dir: path,
                script_path,
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
    /// M5：构建 Runtime + 加载脚本并执行；Entry 收集器接入排到 M6。
    /// 当前返回 `Ok(None)` 表示"框架已就绪但 Entry 收集尚未接入"。
    pub fn run_tier2_for_url(
        &self,
        url: &str,
        http: Arc<HttpClient>,
        credentials: Arc<CredentialStore>,
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
        let Some(script_path) = &plugin.script_path else {
            return Err(CoreError::Message(format!(
                "tier2 plugin '{}' missing adapter.rhai",
                plugin.manifest.plugin.id
            )));
        };
        let script = std::fs::read_to_string(script_path)
            .map_err(|e| CoreError::Message(format!("read adapter.rhai: {e}")))?;
        let rt = Runtime::build(plugin.manifest.clone(), http, credentials);
        // M5：执行脚本但不接入 Entry 收集器；M6 会用 EntryCollector 替换。
        let _ = rt.run_script(&script)?;
        Ok(None)
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
    fn new_with_missing_dir_is_empty() {
        let tmp = std::env::temp_dir().join(format!("glean-plugin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mgr = PluginManager::new(tmp.clone()).expect("open");
        assert!(mgr.list().is_empty());
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
        assert_eq!(mgr.list().len(), 1);
        assert_eq!(mgr.list()[0].manifest.plugin.id, "test-plugin");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
