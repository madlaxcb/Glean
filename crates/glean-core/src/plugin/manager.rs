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
//! 插件全部以磁盘目录形式存在（程序不内嵌插件），由「插件管理」界面安装 /
//! 卸载 / 启停。安装/卸载后调用方（service）重建 manager；启停状态记在
//! `disabled` 集合中，路由时跳过。
//!
//! 功能状态：
//! - 扫描 + manifest 解析 + URL 路由已就绪
//! - 启停（disabled 集合）、安装（文件夹/zip）、卸载已就绪
//! - Tier 1 执行入口（`run_tier1_for_url`）已就绪，调用 `tier1::run`
//! - Tier 2 执行入口（`run_tier2_for_url`）已就绪，构建 Runtime + 加载脚本

use crate::error::{CoreError, Result};
use crate::feed::parse::ParsedFeed;
use crate::feed::HttpClient;
use crate::plugin::credential::CredentialStore;
use crate::plugin::manifest::{Manifest, MatchRule};
use crate::plugin::runtime::Runtime;
use crate::plugin::tier1;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 已加载的插件：manifest + 可选脚本内容。
///
/// `script` 直接持有脚本字符串（避免每次执行重新读盘）。来源：
/// - 磁盘插件：`<dir>/adapter.rhai` 读出的内容
/// - Tier 1：`None`
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
    /// 已停用的插件 id。路由（`find_for_url`）跳过；启停不删除文件。
    disabled: HashSet<String>,
}

impl PluginManager {
    /// 创建空 manager。`plugins_dir` 不存在时返回空列表（不报错）。
    pub fn new(plugins_dir: PathBuf) -> Result<Self> {
        let mut mgr = Self {
            plugins: Vec::new(),
            plugins_dir,
            disabled: HashSet::new(),
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
            disabled: HashSet::new(),
        }
    }

    /// 扫描 `<plugins_dir>/<id>/manifest.toml`。解析失败的插件跳过，不阻塞
    /// 其他插件。所有插件均来自磁盘（无内置插件）。
    fn scan(&mut self) -> Result<()> {
        self.plugins.clear();
        if self.plugins_dir.as_os_str().is_empty() {
            return Ok(());
        }
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
        Ok(())
    }

    pub fn list(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// 插件是否已停用（「插件管理」界面展示开关状态用）。
    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.contains(id)
    }

    /// 全部停用插件 id。
    pub fn disabled_ids(&self) -> &HashSet<String> {
        &self.disabled
    }

    /// 停用/启用插件（存在性校验）。`enabled=false` 时加入 disabled 集合，
    /// 文件不动；路由会跳过停用插件。
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<()> {
        if !self.plugins.iter().any(|p| p.manifest.plugin.id == id) {
            return Err(CoreError::Message(format!("插件不存在: {id}")));
        }
        if enabled {
            self.disabled.remove(id);
        } else {
            self.disabled.insert(id.to_string());
        }
        Ok(())
    }

    /// 整体应用停用集合（service 重建 manager 后，用 AppConfig 的
    /// `disabled_plugins` 同步状态）。不校验 id 存在：容忍集合里残留
    /// 已卸载插件的 id。
    pub fn set_disabled(&mut self, ids: &HashSet<String>) {
        self.disabled = ids.clone();
    }

    /// 按 URL 匹配规则找到对应插件（跳过停用插件）。§11.5.8 `[[match]]` 段。
    pub fn find_for_url(&self, url: &str) -> Option<&LoadedPlugin> {
        self.plugins
            .iter()
            .filter(|p| !self.disabled.contains(&p.manifest.plugin.id))
            .find(|p| p.manifest.r#match.iter().any(|r| matches(url, r)))
    }

    /// 安装插件：把 `src` 目录（含 manifest.toml）复制到 `plugins/<id>/`。
    /// 校验 manifest 可解析；目标 id 已存在则报错。只动磁盘，列表由调用方
    /// 重建（rescan）后生效。
    pub fn install_from_dir(&self, src: &Path) -> Result<String> {
        let manifest_text = std::fs::read_to_string(src.join("manifest.toml"))
            .map_err(|e| CoreError::Message(format!("读取 manifest.toml 失败: {e}")))?;
        let manifest: Manifest = toml::from_str(&manifest_text)
            .map_err(|e| CoreError::Message(format!("manifest 解析失败: {e}")))?;
        let id = manifest.plugin.id.clone();
        if id.is_empty() {
            return Err(CoreError::Message("manifest 缺少 plugin.id".into()));
        }
        let target = self.plugins_dir.join(&id);
        if target.exists() {
            return Err(CoreError::Message(format!("插件已存在: {id}")));
        }
        copy_dir_recursive(src, &target)
            .map_err(|e| CoreError::Message(format!("复制插件目录失败: {e}")))?;
        Ok(id)
    }

    /// 安装插件：解压 zip 后调用 [`install_from_dir`]。zip 顶层或第一层
    /// 子目录含 manifest.toml 均可（兼容常见的目录包裹打包）。
    pub fn install_from_zip(&self, zip_path: &Path) -> Result<String> {
        let file = std::fs::File::open(zip_path)
            .map_err(|e| CoreError::Message(format!("打开 zip 失败: {e}")))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| CoreError::Message(format!("zip 解析失败: {e}")))?;
        // 解压临时目录必须唯一（pid + 时间戳）：并发安装（含并行测试）时
        // 若共用固定目录会互相踩踏，导致读到对方解压出的 manifest.toml。
        let tmp = std::env::temp_dir().join(format!(
            "glean-plugin-import-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        archive
            .extract(&tmp)
            .map_err(|e| CoreError::Message(format!("zip 解压失败: {e}")))?;
        let src = if tmp.join("manifest.toml").is_file() {
            tmp.clone()
        } else {
            let mut found = None;
            if let Ok(entries) = std::fs::read_dir(&tmp) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() && p.join("manifest.toml").is_file() {
                        found = Some(p);
                        break;
                    }
                }
            }
            found.ok_or_else(|| CoreError::Message("zip 内未找到 manifest.toml".into()))?
        };
        let id = self.install_from_dir(&src)?;
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(id)
    }

    /// 卸载插件：删除 `plugins/<id>/` 整个目录。无磁盘目录的插件
    /// （测试构造）拒绝卸载。
    pub fn uninstall(&self, id: &str) -> Result<()> {
        let plugin = self
            .plugins
            .iter()
            .find(|p| p.manifest.plugin.id == id)
            .ok_or_else(|| CoreError::Message(format!("插件不存在: {id}")))?;
        if plugin.dir.as_os_str().is_empty() {
            return Err(CoreError::Message(format!(
                "插件 {id} 不可卸载（无磁盘目录）"
            )));
        }
        std::fs::remove_dir_all(&plugin.dir)
            .map_err(|e| CoreError::Message(format!("卸载失败: {e}")))?;
        Ok(())
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

/// 递归复制目录（std 无内置递归复制）。
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
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
    fn new_with_missing_dir_loads_empty() {
        // 插件目录不存在 → 空列表（无内置插件，插件一律来自磁盘）。
        let tmp = std::env::temp_dir().join(format!("glean-plugin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mgr = PluginManager::new(tmp.clone()).expect("open");
        assert!(mgr.list().is_empty(), "missing dir should yield no plugins");
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
        let ids: Vec<&str> = mgr
            .list()
            .iter()
            .map(|p| p.manifest.plugin.id.as_str())
            .collect();
        assert_eq!(ids, vec!["test-plugin"], "only disk plugin loaded");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 官方插件目录（仓库 `plugins/bilibili/`）的 manifest.toml 能正确解析，
    /// 且能力声明符合预期。所有插件都不再内嵌进程序。
    #[test]
    fn official_bilibili_manifest_parses() {
        let toml_text = include_str!("../../../../plugins/bilibili/manifest.toml");
        let m: Manifest = toml::from_str(toml_text).expect("bilibili manifest parse");
        assert_eq!(m.plugin.id, "bilibili");
        assert_eq!(m.plugin.tier, crate::plugin::manifest::Tier::Script);
        assert!(m
            .capabilities
            .feed_fetch
            .contains(&"api.bilibili.com".to_string()));
        assert!(!m.compliance.uses_user_session);
    }

    #[test]
    fn disabled_plugin_skipped_in_routing() {
        let mut m = make_manifest("pixiv", "pixiv.net/users/*", Tier::Script);
        m.capabilities.feed_fetch = vec!["app-api.pixiv.net".into()];
        let mut mgr = PluginManager::from_manifests(vec![m]);
        let url = "https://www.pixiv.net/users/12345";
        assert!(mgr.find_for_url(url).is_some(), "enabled: routed");
        mgr.set_enabled("pixiv", false).expect("disable");
        assert!(mgr.is_disabled("pixiv"));
        assert!(mgr.find_for_url(url).is_none(), "disabled: not routed");
        mgr.set_enabled("pixiv", true).expect("enable");
        assert!(mgr.find_for_url(url).is_some(), "re-enabled: routed");
    }

    #[test]
    fn set_enabled_validates_existence() {
        let mut mgr = PluginManager::from_manifests(vec![make_manifest(
            "pixiv",
            "pixiv.net/*",
            Tier::Script,
        )]);
        assert!(mgr.set_enabled("ghost", false).is_err());
        assert!(mgr.set_enabled("pixiv", false).is_ok());
    }

    #[test]
    fn install_from_dir_copies_and_loads() {
        let tmp = std::env::temp_dir().join(format!("glean-plugin-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // 源插件目录
        let src = tmp.join("src-plugin");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("manifest.toml"),
            r#"
[plugin]
id = "installed-plugin"
name = "Installed"
version = "0.1"
tier = 1

[[match]]
url_pattern = "inst.example.com/*"

[tier1]
request_url_template = "https://inst.example.com/feed"
entries_json_path = "$.items"
"#,
        )
        .unwrap();
        std::fs::write(src.join("note.txt"), "extra file").unwrap();
        // 目标插件目录（mgr 指向）
        let plugins_dir = tmp.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let mgr = PluginManager::new(plugins_dir.clone()).expect("open");

        let id = mgr.install_from_dir(&src).expect("install");
        assert_eq!(id, "installed-plugin");
        // 文件已复制
        assert!(plugins_dir.join("installed-plugin/manifest.toml").is_file());
        assert!(plugins_dir.join("installed-plugin/note.txt").is_file());
        // 重复安装报错
        assert!(mgr.install_from_dir(&src).is_err());

        // 重建后可见
        let mgr2 = PluginManager::new(plugins_dir.clone()).expect("rescan");
        assert!(mgr2
            .list()
            .iter()
            .any(|p| p.manifest.plugin.id == "installed-plugin"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_from_zip_extracts_and_loads() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("glean-plugin-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let mgr = PluginManager::new(plugins_dir.clone()).expect("open");

        // 构造 zip：顶层 manifest.toml + 脚本
        let zip_path = tmp.join("pkg.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zw.start_file("manifest.toml", opts).unwrap();
        zw.write_all(
            b"[plugin]\nid = \"zip-plugin\"\nname = \"Z\"\nversion = \"0.1\"\ntier = 1\n\n[[match]]\nurl_pattern = \"zip.example.com/*\"\n\n[tier1]\nrequest_url_template = \"https://zip.example.com/feed\"\nentries_json_path = \"$.items\"\n",
        )
        .unwrap();
        zw.start_file("adapter.rhai", opts).unwrap();
        zw.write_all(b"// unused for tier1").unwrap();
        zw.finish().unwrap();

        let id = mgr.install_from_zip(&zip_path).expect("install zip");
        assert_eq!(id, "zip-plugin");
        assert!(plugins_dir.join("zip-plugin/manifest.toml").is_file());
        assert!(plugins_dir.join("zip-plugin/adapter.rhai").is_file());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_from_zip_with_wrapping_dir() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("glean-plugin-zipw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let mgr = PluginManager::new(plugins_dir.clone()).expect("open");

        // 常见分发格式：zip 顶层是插件 id 目录
        let zip_path = tmp.join("pkg.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zw.start_file("my-plugin/manifest.toml", opts).unwrap();
        zw.write_all(
            b"[plugin]\nid = \"my-plugin\"\nname = \"M\"\nversion = \"0.1\"\ntier = 1\n\n[[match]]\nurl_pattern = \"my.example.com/*\"\n\n[tier1]\nrequest_url_template = \"https://my.example.com/feed\"\nentries_json_path = \"$.items\"\n",
        )
        .unwrap();
        zw.finish().unwrap();

        let id = mgr.install_from_zip(&zip_path).expect("install zip");
        assert_eq!(id, "my-plugin");
        assert!(plugins_dir.join("my-plugin/manifest.toml").is_file());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn uninstall_removes_dir() {
        let tmp = std::env::temp_dir().join(format!("glean-plugin-uninst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join("plugins");
        let plugin_dir = plugins_dir.join("victim");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"
[plugin]
id = "victim"
name = "V"
version = "0.1"
tier = 1

[[match]]
url_pattern = "victim.example.com/*"

[tier1]
request_url_template = "https://victim.example.com/feed"
entries_json_path = "$.items"
"#,
        )
        .unwrap();
        let mgr = PluginManager::new(plugins_dir.clone()).expect("open");
        assert!(mgr.list().iter().any(|p| p.manifest.plugin.id == "victim"));

        mgr.uninstall("victim").expect("uninstall");
        assert!(!plugin_dir.exists(), "dir should be removed");
        assert!(mgr.uninstall("victim").is_err(), "unknown id errors");
        assert!(mgr.uninstall("ghost").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn from_manifests_plugin_is_not_uninstallable() {
        let mgr = PluginManager::from_manifests(vec![make_manifest(
            "pixiv",
            "pixiv.net/*",
            Tier::Script,
        )]);
        assert!(mgr.uninstall("pixiv").is_err(), "no dir → cannot uninstall");
    }

    /// 端到端：用官方 bilibili 插件（仓库 `plugins/bilibili/`）订阅
    /// `space.bilibili.com/2`（碧诗）。验证 wbi 签名 + buvid3 流程能拿到真实视频列表。
    /// 手动跑：`cargo test -p glean-core -- --ignored bilibili_end_to_end`
    ///
    /// 注意：在数据中心 IP 环境下可能被风控（-352 / -799）；用户住宅 IP 通常正常。
    #[test]
    #[ignore = "需联网访问 api.bilibili.com（匿名，可能被风控）"]
    fn bilibili_end_to_end() {
        let tmp = std::env::temp_dir().join(format!("glean-bili-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugin_dir = tmp.join("bilibili");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            include_str!("../../../../plugins/bilibili/manifest.toml"),
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("adapter.rhai"),
            include_str!("../../../../plugins/bilibili/adapter.rhai"),
        )
        .unwrap();
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
