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
use crate::plugin::manifest::{Capabilities, Manifest, MatchRule};
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

/// 安装/更新预览结果（§11.5.4 安装时权限确认）。`preview_install_*` 只读
/// 校验、不动磁盘；用户确认后调用 `install_from_*` 提交落盘。
#[derive(Debug, Clone)]
pub struct InstallPreview {
    /// 将安装的 manifest（含全部能力声明）。
    pub manifest: Manifest,
    /// true = 覆盖已存在的同名插件（更新）。
    pub is_update: bool,
    /// 相对已安装版本新增的能力（首次安装时为全部能力）。
    pub added_capabilities: Capabilities,
    /// 权限是否扩大：首次安装恒为 true（需展示全部能力并确认）；
    /// 更新时为 `added_capabilities` 非空。
    pub capabilities_grown: bool,
}

/// PluginManager：管理已加载的插件集合并提供 URL 路由。
pub struct PluginManager {
    plugins: Vec<LoadedPlugin>,
    plugins_dir: PathBuf,
    /// 已停用的插件 id。路由（`find_for_url`）跳过；启停不删除文件。
    disabled: HashSet<String>,
    /// 显式开启「使用代理」的插件 id。命中后插件请求走代理 client，
    /// 覆盖订阅级开关（见 service::fetch_via_plugin）。
    proxy: HashSet<String>,
}

impl PluginManager {
    /// 创建空 manager。`plugins_dir` 不存在时返回空列表（不报错）。
    pub fn new(plugins_dir: PathBuf) -> Result<Self> {
        let mut mgr = Self {
            plugins: Vec::new(),
            plugins_dir,
            disabled: HashSet::new(),
            proxy: HashSet::new(),
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
            proxy: HashSet::new(),
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
    /// 的 id（对应插件可能已被卸载）。
    pub fn set_disabled(&mut self, ids: &HashSet<String>) {
        self.disabled = ids.clone();
    }

    /// 插件级「使用代理」开关是否开启（§11.5.10）。
    pub fn uses_proxy(&self, id: &str) -> bool {
        self.proxy.contains(id)
    }

    /// 当前开启「使用代理」的插件 id（供 UI 写回 `AppConfig.plugin_proxy`）。
    pub fn proxy_ids(&self) -> &HashSet<String> {
        &self.proxy
    }

    /// 设置插件级「使用代理」开关（存在性校验）。
    pub fn set_proxy(&mut self, id: &str, use_proxy: bool) -> Result<()> {
        if !self.plugins.iter().any(|p| p.manifest.plugin.id == id) {
            return Err(CoreError::Message(format!("插件不存在: {id}")));
        }
        if use_proxy {
            self.proxy.insert(id.to_string());
        } else {
            self.proxy.remove(id);
        }
        Ok(())
    }

    /// 用 `AppConfig.plugin_proxy` 同步插件代理开关集合（不校验 id 存在）。
    pub fn set_proxy_set(&mut self, ids: &HashSet<String>) {
        self.proxy = ids.clone();
    }

    /// 按 URL 匹配规则找到对应插件（跳过停用插件）。§11.5.8 `[[match]]` 段。
    pub fn find_for_url(&self, url: &str) -> Option<&LoadedPlugin> {
        self.plugins
            .iter()
            .filter(|p| !self.disabled.contains(&p.manifest.plugin.id))
            .find(|p| p.manifest.r#match.iter().any(|r| matches(url, r)))
    }

    /// 预览安装/更新（§11.5.4）：校验 manifest 可解析、id 非空，并报告
    /// 是安装还是更新、能力是否扩大。只读磁盘，不写任何文件。
    pub fn preview_install_dir(&self, src: &Path) -> Result<InstallPreview> {
        self.build_preview(read_manifest(src)?)
    }

    /// 预览 zip 安装/更新：直接从压缩包读取 manifest.toml（顶层或第一层
    /// 子目录），不落盘、不写临时文件。
    pub fn preview_install_zip(&self, zip_path: &Path) -> Result<InstallPreview> {
        self.build_preview(read_manifest_from_zip(zip_path)?)
    }

    /// 由新 manifest 生成预览：与已安装版本对比能力差异。
    fn build_preview(&self, manifest: Manifest) -> Result<InstallPreview> {
        let id = manifest.plugin.id.clone();
        if id.is_empty() {
            return Err(CoreError::Message("manifest 缺少 plugin.id".into()));
        }
        let target = self.plugins_dir.join(&id);
        let is_update = target.exists();
        let old = if is_update {
            read_manifest(&target).ok()
        } else {
            None
        };
        let added_capabilities = match &old {
            Some(old) => manifest
                .capabilities
                .new_items_relative_to(&old.capabilities),
            None => manifest.capabilities.clone(),
        };
        let capabilities_grown = !is_update || !added_capabilities.is_empty();
        Ok(InstallPreview {
            manifest,
            is_update,
            added_capabilities,
            capabilities_grown,
        })
    }

    /// 安装/更新插件：把 `src` 目录（含 manifest.toml）复制到 `plugins/<id>/`。
    /// 校验 manifest 可解析；目标 id 已存在时按「更新」覆盖（§11.5.4）。
    /// 先复制到同目录 staging 再原子替换：更新失败时旧插件不受影响。
    /// 列表由调用方重建（rescan）后生效。
    pub fn install_from_dir(&self, src: &Path) -> Result<String> {
        let manifest = read_manifest(src)?;
        let id = manifest.plugin.id.clone();
        if id.is_empty() {
            return Err(CoreError::Message("manifest 缺少 plugin.id".into()));
        }
        let target = self.plugins_dir.join(&id);
        let staging = self.plugins_dir.join(format!(
            ".staging-{id}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&staging);
        if let Err(e) = copy_dir_recursive(src, &staging) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(CoreError::Message(format!("复制插件目录失败: {e}")));
        }
        if target.exists() {
            std::fs::remove_dir_all(&target)
                .map_err(|e| CoreError::Message(format!("移除旧插件目录失败: {e}")))?;
        }
        if let Err(e) = std::fs::rename(&staging, &target) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(CoreError::Message(format!("替换插件目录失败: {e}")));
        }
        Ok(id)
    }

    /// 安装/更新插件：解压 zip 后调用 [`install_from_dir`]。zip 顶层或第一层
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
        existing_guids: &[String],
        shallow: bool,
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
        let mut rt = Runtime::build(plugin.manifest.clone(), http, creds);
        if shallow {
            rt = rt.with_shallow();
        }
        let parsed = rt.run_script(script, url, existing_guids)?;
        Ok(Some(parsed))
    }
}

/// 读取目录内的 manifest.toml 并解析（预览与安装共用）。
fn read_manifest(src: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(src.join("manifest.toml"))
        .map_err(|e| CoreError::Message(format!("读取 manifest.toml 失败: {e}")))?;
    toml::from_str(&text).map_err(|e| CoreError::Message(format!("manifest 解析失败: {e}")))
}

/// 从 zip 读取 manifest.toml（顶层或第一层子目录均可），不落盘。
fn read_manifest_from_zip(zip_path: &Path) -> Result<Manifest> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| CoreError::Message(format!("打开 zip 失败: {e}")))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| CoreError::Message(format!("zip 解析失败: {e}")))?;
    let name = archive
        .file_names()
        .find(|n| {
            let n = n.replace('\\', "/");
            let n = n.trim_end_matches('/');
            n == "manifest.toml" || (n.split('/').count() == 2 && n.ends_with("manifest.toml"))
        })
        .map(str::to_string)
        .ok_or_else(|| CoreError::Message("zip 内未找到 manifest.toml".into()))?;
    let mut f = archive
        .by_name(&name)
        .map_err(|e| CoreError::Message(format!("读取 manifest.toml 失败: {e}")))?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut f, &mut text)
        .map_err(|e| CoreError::Message(format!("读取 manifest.toml 失败: {e}")))?;
    toml::from_str(&text).map_err(|e| CoreError::Message(format!("manifest 解析失败: {e}")))
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
    // 兼容存量坏数据：用户曾粘贴 markdown 反引号链接（`` `https://…` ``），
    // strip 后再解析，否则 Url::parse 失败导致插件永远 miss。
    let url = url.trim_matches(|c: char| c == '`' || c.is_whitespace());
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
    fn find_for_url_matches_backtick_wrapped_url() {
        // 存量坏数据：feed_url 曾被粘贴为 markdown 反引号链接。matches()
        // 必须剥掉反引号再解析，否则插件永远 miss（曾导致「刷新该贴」
        // 走到 RSS 抓到登录页）。
        let m = make_manifest("pixiv", "pixiv.net/user/*", Tier::Script);
        let mgr = PluginManager::from_manifests(vec![m]);
        assert!(
            mgr.find_for_url("`https://www.pixiv.net/user/8252709`")
                .is_some(),
            "backtick-wrapped URL should still match the plugin"
        );
    }

    #[test]
    fn find_for_url_matches_singular_pixiv_user_rule() {
        let m = make_manifest("pixiv", "pixiv.net/user/*", Tier::Script);
        let mgr = PluginManager::from_manifests(vec![m]);
        assert!(mgr
            .find_for_url("https://www.pixiv.net/user/8252709")
            .is_some());
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

    /// 官方 pixiv 插件 manifest 能解析，且声明了 OAuth 端点白名单 + 凭证槽。
    #[test]
    fn official_pixiv_manifest_parses() {
        let toml_text = include_str!("../../../../plugins/pixiv/manifest.toml");
        let m: Manifest = toml::from_str(toml_text).expect("pixiv manifest parse");
        assert_eq!(m.plugin.id, "pixiv");
        assert_eq!(m.plugin.tier, crate::plugin::manifest::Tier::Script);
        assert!(m
            .capabilities
            .feed_fetch
            .contains(&"app-api.pixiv.net".to_string()));
        assert!(m
            .capabilities
            .credential_use
            .contains(&"pixiv_refresh_token".to_string()));
        assert!(m.compliance.uses_user_session);
        // URL 路由要命中用户主页。
        assert!(m
            .r#match
            .iter()
            .any(|r| r.url_pattern == "pixiv.net/users/*"));
    }

    #[test]
    fn official_fanbox_manifest_parses() {
        let toml_text = include_str!("../../../../plugins/fanbox/manifest.toml");
        let m: Manifest = toml::from_str(toml_text).expect("fanbox manifest parse");
        assert_eq!(m.plugin.id, "fanbox");
        assert_eq!(m.plugin.tier, crate::plugin::manifest::Tier::Script);
        assert!(m
            .capabilities
            .feed_fetch
            .contains(&"api.fanbox.cc".to_string()));
        assert!(m
            .capabilities
            .credential_use
            .contains(&"fanbox_session".to_string()));
        assert!(m.compliance.uses_user_session);
        assert!(m.r#match.iter().any(|r| r.url_pattern == "fanbox.cc/@*"));
        assert!(m.r#match.iter().any(|r| r.url_pattern == "*.fanbox.cc"));
    }

    /// fanbox 子域名形式（https://creator.fanbox.cc/）应能命中插件。
    #[test]
    fn find_for_url_matches_fanbox_subdomain() {
        let tmp = std::env::temp_dir().join(format!("glean-fanbox-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugin_dir = tmp.join("fanbox");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            include_str!("../../../../plugins/fanbox/manifest.toml"),
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("adapter.rhai"),
            include_str!("../../../../plugins/fanbox/adapter.rhai"),
        )
        .unwrap();
        let mgr = PluginManager::new(tmp.clone()).expect("open");
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(
            mgr.find_for_url("https://mana.fanbox.cc/").is_some(),
            "裸子域名应命中"
        );
        assert!(
            mgr.find_for_url("https://mana.fanbox.cc/posts/123")
                .is_some(),
            "子域名带路径应命中"
        );
        assert!(
            mgr.find_for_url("https://www.fanbox.cc/@mana").is_some(),
            "@创作者 形式应命中"
        );
    }

    #[test]
    fn plugin_proxy_switch_roundtrip() {
        // 插件级「使用代理」开关：set → 查询 → 外部同步（set_proxy_set）。
        let mut mgr = PluginManager::from_manifests(vec![make_manifest(
            "my-plugin",
            "my.example.com/*",
            Tier::Config,
        )]);
        assert!(!mgr.uses_proxy("my-plugin"));
        mgr.set_proxy("my-plugin", true).unwrap();
        assert!(mgr.uses_proxy("my-plugin"));
        assert!(mgr.proxy_ids().contains("my-plugin"));
        // 未知 id 报错。
        assert!(mgr.set_proxy("no-such", true).is_err());
        // 外部同步（AppConfig 启动加载路径）。
        mgr.set_proxy_set(&HashSet::from(["my-plugin".to_string()]));
        assert!(mgr.uses_proxy("my-plugin"));
        mgr.set_proxy("my-plugin", false).unwrap();
        assert!(!mgr.uses_proxy("my-plugin"));
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
        // 重复安装 = 更新覆盖（§11.5.4），不报错
        assert!(mgr.install_from_dir(&src).is_ok());

        // 重建后可见
        let mgr2 = PluginManager::new(plugins_dir.clone()).expect("rescan");
        assert!(mgr2
            .list()
            .iter()
            .any(|p| p.manifest.plugin.id == "installed-plugin"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn preview_install_dir_new_plugin() {
        let tmp = std::env::temp_dir().join(format!("glean-plugin-prev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src = tmp.join("src-plugin");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("manifest.toml"),
            r#"
[plugin]
id = "fresh"
name = "Fresh"
version = "0.1"
tier = 1

[[match]]
url_pattern = "fresh.example.com/*"

[capabilities]
feed_fetch = ["api.fresh.example.com"]

[tier1]
request_url_template = "https://api.fresh.example.com/feed"
entries_json_path = "$.items"
"#,
        )
        .unwrap();
        let plugins_dir = tmp.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let mgr = PluginManager::new(plugins_dir.clone()).expect("open");

        let preview = mgr.preview_install_dir(&src).expect("preview");
        assert_eq!(preview.manifest.plugin.id, "fresh");
        assert!(!preview.is_update, "首次安装不是更新");
        assert!(preview.capabilities_grown, "首次安装需确认全部能力");
        assert_eq!(
            preview.added_capabilities.feed_fetch,
            vec!["api.fresh.example.com"]
        );
        // 预览不写磁盘
        assert!(!plugins_dir.join("fresh").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn preview_install_dir_update_detects_growth() {
        let tmp = std::env::temp_dir().join(format!("glean-plugin-prevup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join("plugins");
        let v1_dir = plugins_dir.join("up-plugin");
        std::fs::create_dir_all(&v1_dir).unwrap();
        std::fs::write(
            v1_dir.join("manifest.toml"),
            r#"
[plugin]
id = "up-plugin"
name = "Up"
version = "0.1"
tier = 1

[[match]]
url_pattern = "up.example.com/*"

[capabilities]
feed_fetch = ["api.up.example.com"]

[tier1]
request_url_template = "https://api.up.example.com/feed"
entries_json_path = "$.items"
"#,
        )
        .unwrap();
        let mgr = PluginManager::new(plugins_dir.clone()).expect("open");

        // v2：域名扩大到两个 → 更新 + 权限扩大
        let v2_dir = tmp.join("v2");
        std::fs::create_dir_all(&v2_dir).unwrap();
        std::fs::write(
            v2_dir.join("manifest.toml"),
            r#"
[plugin]
id = "up-plugin"
name = "Up"
version = "0.2"
tier = 1

[[match]]
url_pattern = "up.example.com/*"

[capabilities]
feed_fetch = ["api.up.example.com", "cdn.up.example.com"]

[tier1]
request_url_template = "https://api.up.example.com/feed"
entries_json_path = "$.items"
"#,
        )
        .unwrap();
        let preview = mgr.preview_install_dir(&v2_dir).expect("preview v2");
        assert!(preview.is_update, "同 id 已安装 → 更新");
        assert!(preview.capabilities_grown, "新增域名 → 权限扩大");
        assert_eq!(
            preview.added_capabilities.feed_fetch,
            vec!["cdn.up.example.com"],
            "diff 只含新增域名"
        );
        assert_eq!(preview.manifest.plugin.version, "0.2");
        // 预览不落盘：磁盘仍是 v0.1
        let on_disk: Manifest =
            toml::from_str(&std::fs::read_to_string(v1_dir.join("manifest.toml")).unwrap())
                .unwrap();
        assert_eq!(on_disk.plugin.version, "0.1");

        // v3：能力与 v2 相同 → 相对已安装版本（先提交 v2 后）不扩大
        mgr.install_from_dir(&v2_dir).expect("commit v2");
        let v3_dir = tmp.join("v3");
        std::fs::create_dir_all(&v3_dir).unwrap();
        std::fs::write(
            v3_dir.join("manifest.toml"),
            r#"
[plugin]
id = "up-plugin"
name = "Up"
version = "0.3"
tier = 1

[[match]]
url_pattern = "up.example.com/*"

[capabilities]
feed_fetch = ["api.up.example.com", "cdn.up.example.com"]

[tier1]
request_url_template = "https://api.up.example.com/feed"
entries_json_path = "$.items"
"#,
        )
        .unwrap();
        let preview3 = mgr.preview_install_dir(&v3_dir).expect("preview v3");
        assert!(preview3.is_update);
        assert!(!preview3.capabilities_grown, "能力未变 → 不扩大");
        assert!(preview3.added_capabilities.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_from_dir_updates_existing_plugin() {
        let tmp = std::env::temp_dir().join(format!("glean-plugin-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        // 先装 v0.1（含旧脚本文件 old.rhai）
        let v1 = tmp.join("v1");
        std::fs::create_dir_all(&v1).unwrap();
        std::fs::write(
            v1.join("manifest.toml"),
            r#"
[plugin]
id = "upd-plugin"
name = "Upd"
version = "0.1"
tier = 2

[[match]]
url_pattern = "upd.example.com/*"
"#,
        )
        .unwrap();
        std::fs::write(v1.join("old.rhai"), "old").unwrap();
        let mgr = PluginManager::new(plugins_dir.clone()).expect("open");
        mgr.install_from_dir(&v1).expect("install v1");
        assert!(plugins_dir.join("upd-plugin/old.rhai").is_file());

        // 再装 v0.2（无 old.rhai，新增 adapter.rhai）→ 覆盖
        let v2 = tmp.join("v2");
        std::fs::create_dir_all(&v2).unwrap();
        std::fs::write(
            v2.join("manifest.toml"),
            r#"
[plugin]
id = "upd-plugin"
name = "Upd"
version = "0.2"
tier = 2

[[match]]
url_pattern = "upd.example.com/*"
"#,
        )
        .unwrap();
        std::fs::write(v2.join("adapter.rhai"), "new").unwrap();
        let id = mgr.install_from_dir(&v2).expect("update v2");
        assert_eq!(id, "upd-plugin");
        // 目录内容已替换：manifest 版本更新，旧文件消失
        let on_disk =
            std::fs::read_to_string(plugins_dir.join("upd-plugin/manifest.toml")).unwrap();
        assert!(on_disk.contains("version = \"0.2\""));
        assert!(!plugins_dir.join("upd-plugin/old.rhai").exists());
        assert!(plugins_dir.join("upd-plugin/adapter.rhai").is_file());
        // 重建后版本可见
        let mgr2 = PluginManager::new(plugins_dir.clone()).expect("rescan");
        let p = mgr2
            .list()
            .iter()
            .find(|p| p.manifest.plugin.id == "upd-plugin")
            .expect("found");
        assert_eq!(p.manifest.plugin.version, "0.2");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn preview_install_zip_reads_manifest_without_extract() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("glean-plugin-prevzip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let mgr = PluginManager::new(plugins_dir.clone()).expect("open");

        // 构造 zip：第一层子目录包裹（常见分发格式）
        let zip_path = tmp.join("pkg.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zw.start_file("pkg/manifest.toml", opts).unwrap();
        zw.write_all(
            b"[plugin]\nid = \"preview-zip\"\nname = \"PZ\"\nversion = \"0.1\"\ntier = 1\n\n[[match]]\nurl_pattern = \"pz.example.com/*\"\n\n[capabilities]\ncredential_use = [\"pz_session\"]\n\n[tier1]\nrequest_url_template = \"https://pz.example.com/feed\"\nentries_json_path = \"$.items\"\n",
        )
        .unwrap();
        zw.finish().unwrap();

        let preview = mgr.preview_install_zip(&zip_path).expect("preview zip");
        assert_eq!(preview.manifest.plugin.id, "preview-zip");
        assert!(!preview.is_update);
        assert_eq!(
            preview.manifest.capabilities.credential_use,
            vec!["pz_session".to_string()]
        );
        // 预览不落盘
        assert!(!plugins_dir.join("preview-zip").exists());
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
    /// `space.bilibili.com/<mid>`。验证 wbi 签名 + buvid3 流程能拿到真实视频列表。
    /// 默认 mid=2（碧诗）；可用环境变量 `GLEAN_BILI_MID` 指定任意 UP 主：
    /// `GLEAN_BILI_MID=3428150 cargo test -p glean-core -- --ignored bilibili_end_to_end`
    ///
    /// 注意：在数据中心 IP 环境下可能被风控（-352 / -799）；用户住宅 IP 通常正常。
    #[test]
    #[ignore = "需联网访问 api.bilibili.com（匿名，可能被风控）"]
    fn bilibili_end_to_end() {
        let mid = std::env::var("GLEAN_BILI_MID").unwrap_or_else(|_| "2".to_string());
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
        let url = format!("https://space.bilibili.com/{mid}");
        let parsed = mgr
            .run_tier2_for_url(&url, http, None, &[], false)
            .expect("run_tier2")
            .expect("matched plugin");

        assert!(
            parsed.title.starts_with("Bilibili "),
            "订阅标题应含 UP 主名（set_feed_title），got: {}",
            parsed.title
        );
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

    /// 端到端：用官方 fanbox 插件（仓库 `plugins/fanbox/`）订阅创作者主页。
    /// 需要用户会话凭证（fanbox_session），因此默认忽略，必须显式提供环境变量：
    /// `GLEAN_FANBOX_URL='https://fanbox.cc/@creator' GLEAN_FANBOX_SESSION='<本地配置的会话>'`
    /// `cargo test -p glean-core -- --ignored fanbox_end_to_end`
    ///
    /// 凭证只通过环境变量注入 Host，绝不写入源码、fixture 或断言。
    #[test]
    #[ignore = "需联网 + 用户会话凭证（环境变量 GLEAN_FANBOX_URL / GLEAN_FANBOX_SESSION）"]
    fn fanbox_end_to_end() {
        let url = std::env::var("GLEAN_FANBOX_URL")
            .expect("设置 GLEAN_FANBOX_URL，例如 https://fanbox.cc/@creator");
        let session = std::env::var("GLEAN_FANBOX_SESSION")
            .expect("设置 GLEAN_FANBOX_SESSION（本地配置的会话凭证）");
        let tmp = std::env::temp_dir().join(format!("glean-fanbox-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugin_dir = tmp.join("fanbox");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            include_str!("../../../../plugins/fanbox/manifest.toml"),
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("adapter.rhai"),
            include_str!("../../../../plugins/fanbox/adapter.rhai"),
        )
        .unwrap();
        let mgr = PluginManager::new(tmp.clone()).expect("open");
        let _ = std::fs::remove_dir_all(&tmp);

        let mut creds = CredentialStore::in_memory();
        creds.set(
            "fanbox",
            "fanbox_session",
            crate::plugin::Credential {
                header_name: "Cookie".into(),
                header_value: session,
            },
        );
        let http = Arc::new(HttpClient::default());
        let parsed = mgr
            .run_tier2_for_url(&url, http, Some(Arc::new(creds)), &[], false)
            .expect("run_tier2")
            .expect("matched plugin");

        assert!(
            parsed.title.starts_with("Fanbox "),
            "订阅标题应含创作者名（set_feed_title），got: {}",
            parsed.title
        );
        assert!(!parsed.entries.is_empty(), "应至少拿到 1 条投稿");
        let first = &parsed.entries[0];
        assert!(
            first.guid.starts_with("fanbox-"),
            "guid 应是 fanbox-<id>，got: {}",
            first.guid
        );
        assert!(!first.title.is_empty());
        assert!(
            first
                .url
                .as_deref()
                .unwrap_or("")
                .starts_with("https://fanbox.cc/"),
            "url 应指向投稿页"
        );
        assert!(first.published_at.is_some(), "published_at 不应为空");
    }
}
