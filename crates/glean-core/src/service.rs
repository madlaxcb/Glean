//! Command handler: AppCommand → mutate store → AppEvent list.

use crate::ai::{run_enhance_task, EnhanceAction, EnhanceOutcome, EnhanceTask};
use crate::command::AppCommand;
use crate::error::{CoreError, Result};
use crate::event::AppEvent;
use crate::extract::{ExtractOutcome, ExtractTask};
use crate::feed::{
    discover_feed_urls, fetch_feed_bytes, parse_feed, FetchResult, HttpClient, ParsedFeed,
    RefreshOutcome, RefreshTask,
};
use crate::model::{AiConfig, EntryFilter, EntryId, FeedId};
use crate::opml;
use crate::paths;
use crate::plugin::{CredentialStore, PluginManager};
use crate::store::Store;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

pub struct GleanService {
    pub store: Store,
    filter: EntryFilter,
    search_query: String,
    /// 直连 HTTP 客户端，共享给刷新 worker 线程。
    http: Arc<HttpClient>,
    /// 带代理的 HTTP 客户端（`proxy_url` 非空时存在），供 `use_proxy = true` 的订阅使用。
    http_proxy: Option<Arc<HttpClient>>,
    /// 当前代理设置（空 = 无代理），与设置页同步。
    proxy_url: String,
    /// §11.5 插件管理器。`None` 表示 in-memory 模式（测试用），不加载磁盘插件。
    /// `Arc` 共享给 worker 线程做 URL→插件路由（加载后只读）。启停/安装/卸载
    /// 通过「重建 manager」生效：UI 调 service 方法，内部写磁盘 + 重建替换 Arc，
    /// 正在执行的 worker 继续用旧快照完成当前刷新。
    plugin_mgr: Option<Arc<PluginManager>>,
    /// 已停用的插件 id（启停状态源）。重建 manager 时应用给它；UI 通过
    /// [`disabled_plugins`](Self::disabled_plugins) 读回写进 `AppConfig`。
    plugin_disabled: HashSet<String>,
    /// 开启「使用代理」的插件 id（§11.5.10）。重建 manager 时应用；
    /// 命中插件后其请求走代理 client，覆盖订阅级开关。
    plugin_proxy: HashSet<String>,
    /// §11.5.9 凭证存储。`None` 表示 in-memory 模式。owned 在此负责可变写入 + 落盘；
    /// worker 线程通过 `Clone` 取快照。
    credentials: Option<CredentialStore>,
    /// AI 增强配置（OpenAI 兼容）。`None` 表示未配置；仅同步 fallback 路径
    /// (`AppCommand::EnhanceEntry`) 会用到。异步路径由 UI 直接把 `AppConfig.ai`
    /// 传给 worker 线程，不经过 service。
    ai_config: Option<Arc<AiConfig>>,
}

impl GleanService {
    pub fn open_in_memory() -> Result<Self> {
        Self::open_in_memory_with_proxy(None)
    }

    pub fn open_in_memory_with_proxy(proxy_url: Option<&str>) -> Result<Self> {
        let (proxy_url, http, http_proxy) = build_http_clients(proxy_url)?;
        Ok(Self {
            store: Store::open_in_memory()?,
            filter: EntryFilter::All,
            search_query: String::new(),
            http,
            http_proxy,
            proxy_url,
            plugin_mgr: None,
            plugin_disabled: HashSet::new(),
            plugin_proxy: HashSet::new(),
            credentials: None,
            ai_config: None,
        })
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        Self::open_path_with_proxy(path, None)
    }

    pub fn open_path_with_proxy(path: &Path, proxy_url: Option<&str>) -> Result<Self> {
        // §11.5.8 / §11.5.9 加载插件目录 + 凭证存储。失败不阻塞核心功能：
        // 插件系统是扩展层，DB/HTTP/订阅主线必须能独立工作。
        let plugin_mgr = paths::plugins_dir()
            .and_then(|d| PluginManager::new(d).ok())
            .map(Arc::new);
        let credentials = paths::credentials_path().and_then(|p| CredentialStore::open(p).ok());
        let (proxy_url, http, http_proxy) = build_http_clients(proxy_url)?;
        Ok(Self {
            store: Store::open_path(path)?,
            filter: EntryFilter::All,
            search_query: String::new(),
            http,
            http_proxy,
            proxy_url,
            plugin_mgr,
            plugin_disabled: HashSet::new(),
            plugin_proxy: HashSet::new(),
            credentials,
            ai_config: None,
        })
    }

    /// 更新代理设置并重建带代理的 HTTP 客户端（设置页保存时调用，立即生效）。
    /// 代理 URL 非法时保留旧客户端并返回错误（UI 提示用户，不再静默失效）。
    /// URL 无 scheme 时自动补 `http://`（见 `HttpClient::with_proxy`）。
    pub fn set_proxy_url(&mut self, proxy_url: &str) -> Result<()> {
        let trimmed = proxy_url.trim().to_string();
        self.proxy_url = trimmed.clone();
        if trimmed.is_empty() {
            self.http_proxy = None;
            return Ok(());
        }
        match HttpClient::with_proxy(Some(&trimmed)) {
            Ok(c) => {
                self.http_proxy = Some(Arc::new(c));
                Ok(())
            }
            Err(e) => {
                eprintln!("glean: invalid proxy {trimmed:?}: {e}");
                Err(CoreError::Message(format!(
                    "代理地址无效：{e}（示例 http://127.0.0.1:7890）"
                )))
            }
        }
    }

    /// 访问插件管理器（§11.5）。
    pub fn plugins(&self) -> Option<&PluginManager> {
        self.plugin_mgr.as_deref()
    }

    /// 用 `AppConfig.disabled_plugins` / `plugin_proxy` 同步插件启停状态与
    /// 代理开关，并重建 manager。UI 启动时调用一次。
    pub fn reload_plugins(&mut self, disabled: &[String], proxy: &[String]) -> Result<()> {
        self.plugin_disabled = disabled.iter().cloned().collect();
        self.plugin_proxy = proxy.iter().cloned().collect();
        self.rebuild_plugins()
    }

    /// 启用/停用插件。「插件管理」界面开关回调；列表变化通过
    /// [`disabled_plugins`](Self::disabled_plugins) 读回写进 `AppConfig`。
    pub fn set_plugin_enabled(&mut self, id: &str, enabled: bool) -> Result<()> {
        let exists = self
            .plugin_mgr
            .as_ref()
            .map(|m| m.list().iter().any(|p| p.manifest.plugin.id == id))
            .unwrap_or(false);
        if !exists {
            return Err(CoreError::Message(format!("插件不存在: {id}")));
        }
        if enabled {
            self.plugin_disabled.remove(id);
        } else {
            self.plugin_disabled.insert(id.to_string());
        }
        self.rebuild_plugins()
    }

    /// 当前停用的插件 id（供 UI 写回 `AppConfig.disabled_plugins`）。
    pub fn disabled_plugins(&self) -> Vec<String> {
        self.plugin_disabled.iter().cloned().collect()
    }

    /// 设置插件级「使用代理」开关（§11.5.10）。变化通过
    /// [`proxy_plugins`](Self::proxy_plugins) 读回写进 `AppConfig.plugin_proxy`。
    pub fn set_plugin_proxy(&mut self, id: &str, use_proxy: bool) -> Result<()> {
        let exists = self
            .plugin_mgr
            .as_ref()
            .map(|m| m.list().iter().any(|p| p.manifest.plugin.id == id))
            .unwrap_or(false);
        if !exists {
            return Err(CoreError::Message(format!("插件不存在: {id}")));
        }
        if use_proxy {
            self.plugin_proxy.insert(id.to_string());
        } else {
            self.plugin_proxy.remove(id);
        }
        self.rebuild_plugins()
    }

    /// 当前开启「使用代理」的插件 id（供 UI 写回 `AppConfig.plugin_proxy`）。
    pub fn proxy_plugins(&self) -> Vec<String> {
        self.plugin_proxy.iter().cloned().collect()
    }

    /// 安装插件（文件夹导入）。返回插件 id；失败时（manifest 无效 /
    /// id 已存在）不改动任何文件。
    pub fn install_plugin_dir(&mut self, src: &Path) -> Result<String> {
        let id = self.require_plugin_mgr()?.install_from_dir(src)?;
        self.rebuild_plugins()?;
        Ok(id)
    }

    /// 安装插件（zip 导入）。zip 顶层或第一层子目录含 manifest.toml 均可。
    pub fn install_plugin_zip(&mut self, zip_path: &Path) -> Result<String> {
        let id = self.require_plugin_mgr()?.install_from_zip(zip_path)?;
        self.rebuild_plugins()?;
        Ok(id)
    }

    /// 卸载插件：删除 `plugins/<id>/` 目录，并从停用集合清除。
    pub fn uninstall_plugin(&mut self, id: &str) -> Result<()> {
        self.require_plugin_mgr()?.uninstall(id)?;
        self.plugin_disabled.remove(id);
        self.rebuild_plugins()
    }

    /// 重建 PluginManager（重扫磁盘）+ 应用停用集合，替换共享 `Arc`。
    /// in-memory 模式无插件目录时静默跳过。
    fn rebuild_plugins(&mut self) -> Result<()> {
        let Some(mgr) = &self.plugin_mgr else {
            return Ok(());
        };
        if mgr.plugins_dir().as_os_str().is_empty() {
            return Ok(());
        }
        let mut rebuilt = PluginManager::new(mgr.plugins_dir().to_path_buf())?;
        rebuilt.set_disabled(&self.plugin_disabled);
        rebuilt.set_proxy_set(&self.plugin_proxy);
        self.plugin_mgr = Some(Arc::new(rebuilt));
        Ok(())
    }

    fn require_plugin_mgr(&self) -> Result<&PluginManager> {
        self.plugin_mgr
            .as_deref()
            .ok_or_else(|| CoreError::Message("插件系统不可用（内存模式）".into()))
    }

    /// 访问凭证存储（§11.5.9）。
    pub fn credentials(&self) -> Option<&CredentialStore> {
        self.credentials.as_ref()
    }

    /// 可变访问凭证存储——设置 UI 输入凭证后调用。
    pub fn credentials_mut(&mut self) -> Option<&mut CredentialStore> {
        self.credentials.as_mut()
    }

    /// 设置插件凭证槽并立即落盘（Windows DPAPI 加密）。§11.5.9 UI 入口。
    pub fn set_credential(
        &mut self,
        plugin_id: &str,
        slot: &str,
        header_name: &str,
        header_value: &str,
    ) -> Result<()> {
        let store = self
            .credentials
            .as_mut()
            .ok_or_else(|| CoreError::Message("凭证存储不可用（内存模式）".into()))?;
        store.set(
            plugin_id,
            slot,
            crate::plugin::Credential {
                header_name: header_name.to_string(),
                header_value: header_value.to_string(),
            },
        );
        store.flush()
    }

    /// 清除插件凭证槽并落盘。
    pub fn remove_credential(&mut self, plugin_id: &str, slot: &str) -> Result<()> {
        let store = self
            .credentials
            .as_mut()
            .ok_or_else(|| CoreError::Message("凭证存储不可用（内存模式）".into()))?;
        store.remove(plugin_id, slot);
        store.flush()
    }

    /// 读取插件凭证槽（明文仅 Host 内部用；UI 用于显示「已设置」状态）。
    pub fn get_credential(
        &self,
        plugin_id: &str,
        slot: &str,
    ) -> Option<&crate::plugin::Credential> {
        self.credentials.as_ref()?.get(plugin_id, slot)
    }

    /// 设置 AI 配置（来自 `AppConfig.ai`）。UI 启动时若有 AI 配置则调用一次。
    /// 仅同步 fallback 路径 (`AppCommand::EnhanceEntry`) 会用到；异步路径由
    /// UI 直接把 `AppConfig.ai` 传给 worker 线程。
    pub fn set_ai_config(&mut self, cfg: AiConfig) {
        self.ai_config = Some(Arc::new(cfg));
    }

    /// 清除 AI 配置（用户在设置里删除时调用），同步 fallback 会回到「AI 未配置」。
    pub fn clear_ai_config(&mut self) {
        self.ai_config = None;
    }

    /// 共享 HTTP 客户端（含代理配置）给 worker 线程。
    pub fn http(&self) -> Arc<HttpClient> {
        Arc::clone(&self.http)
    }

    /// 带代理的 HTTP 客户端（`proxy_url` 非空时存在）。图片缓存等需要
    /// 访问防盗链域名（如 i.pximg.net）的场景应优先用此 client，否则
    /// 用户开了代理但图片下载走直连会失败。
    pub fn http_proxy(&self) -> Option<&Arc<HttpClient>> {
        self.http_proxy.as_ref()
    }

    /// 构建一份刷新上下文快照供 worker 线程使用：共享 `PluginManager`/`HttpClient`，
    /// 克隆 `CredentialStore`（凭证集很小）。`None` 字段表示该能力不可用，worker 走默认 RSS。
    pub fn refresh_ctx(&self) -> RefreshCtx {
        RefreshCtx {
            plugin_mgr: self.plugin_mgr.clone(),
            http: Arc::clone(&self.http),
            http_proxy: self.http_proxy.clone(),
            credentials: self.credentials.clone(),
        }
    }

    /// §11.5 刷新时的插件路由：URL 命中已加载插件则走 Tier 1/2，否则返回 `None`
    /// 让调用方走默认 RSS。返回 `Some(Err)` 表示插件命中但执行失败。
    /// `client` 由调用方按订阅的 use_proxy 选定（直连或代理）；
    /// 若插件自身开启了「使用代理」（§11.5.10），则覆盖为代理 client。
    fn fetch_via_plugin(
        &self,
        url: &str,
        client: &Arc<HttpClient>,
        existing_guids: &[String],
    ) -> Option<Result<ParsedFeed>> {
        let mgr = self.plugin_mgr.as_deref()?;
        // 命中插件后按插件级代理开关覆盖订阅级选择；未配置代理时回退直连。
        let effective = mgr
            .find_for_url(url)
            .map(|p| {
                if mgr.uses_proxy(&p.manifest.plugin.id) {
                    Arc::clone(pick_client(true, &self.http, &self.http_proxy))
                } else {
                    Arc::clone(client)
                }
            })
            .unwrap_or_else(|| Arc::clone(client));
        // Tier 1：纯配置驱动，无需凭证。
        if let Some(res) = mgr.run_tier1_for_url(url, &effective).transpose() {
            return Some(res);
        }
        // Tier 2：Rhai 脚本，需要凭证快照（如有）。
        let creds = self.credentials.as_ref().map(|c| Arc::new(c.clone()));
        mgr.run_tier2_for_url(url, effective, creds, existing_guids)
            .transpose()
    }

    pub fn handle(&mut self, cmd: AppCommand) -> Vec<AppEvent> {
        match self.handle_inner(cmd) {
            Ok(ev) => ev,
            Err(e) => vec![AppEvent::Error {
                message: e.to_string(),
            }],
        }
    }

    fn handle_inner(&mut self, cmd: AppCommand) -> Result<Vec<AppEvent>> {
        match cmd {
            AppCommand::Bootstrap => {
                let mut ev = vec![AppEvent::Ready];
                ev.extend(self.emit_nav()?);
                ev.extend(self.emit_entries()?);
                Ok(ev)
            }
            AppCommand::RefreshNav => Ok(self.emit_nav()?),
            AppCommand::ListEntries { filter } => {
                self.filter = filter;
                self.search_query.clear();
                Ok(self.emit_entries()?)
            }
            AppCommand::SearchEntries { query } => {
                self.search_query = query;
                Ok(self.emit_entries()?)
            }
            AppCommand::OpenEntry { id } => {
                let entry = self.store.get_entry(id)?;
                if !entry.summary.is_read {
                    self.store.set_read(id, true)?;
                }
                let entry = self.store.get_entry(id)?;
                let unread = self.store.unread_count()?;
                Ok(vec![
                    AppEvent::EntryOpened { entry },
                    AppEvent::UnreadChanged { total: unread },
                    AppEvent::EntriesUpdated {
                        entries: self.store.list_entries(self.filter)?,
                    },
                ])
            }
            AppCommand::MarkRead { id, read } => {
                self.store.set_read(id, read)?;
                let unread = self.store.unread_count()?;
                let mut ev = self.emit_entries()?;
                ev.push(AppEvent::UnreadChanged { total: unread });
                Ok(ev)
            }
            AppCommand::ToggleStar { id } => {
                let starred = self.store.toggle_star(id)?;
                let mut ev = self.emit_entries()?;
                ev.push(AppEvent::Status {
                    message: if starred {
                        "已加星标".into()
                    } else {
                        "已取消星标".into()
                    },
                });
                Ok(ev)
            }
            AppCommand::MarkAllRead { feed_id } => {
                match feed_id {
                    Some(fid) => self.store.mark_all_read_in_feed(fid)?,
                    None => self.store.mark_all_read()?,
                }
                let unread = self.store.unread_count()?;
                let mut ev = self.emit_entries()?;
                ev.push(AppEvent::UnreadChanged { total: unread });
                ev.push(AppEvent::Status {
                    message: "已全部标记已读".into(),
                });
                Ok(ev)
            }
            AppCommand::AddFeedLocal { title, feed_url } => {
                let id = self.store.add_feed(
                    &title,
                    &feed_url,
                    None,
                    crate::feed::categorize(&feed_url),
                )?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: format!("已添加本地源 id={}", id.0),
                });
                Ok(ev)
            }
            AppCommand::AddEntryLocal {
                feed_id,
                guid,
                title,
                url,
                content_html,
            } => {
                let id =
                    self.store
                        .add_entry(feed_id, &guid, &title, url.as_deref(), &content_html)?;
                let mut ev = self.emit_entries()?;
                ev.push(AppEvent::Status {
                    message: format!("Entry added id={}", id.0),
                });
                ev.push(AppEvent::UnreadChanged {
                    total: self.store.unread_count()?,
                });
                Ok(ev)
            }
            AppCommand::AddFeedFromUrl { feed_url } => self.add_feed_from_url(&feed_url),
            AppCommand::DeleteFeed { id } => {
                self.store.delete_feed(id)?;
                let mut ev = self.emit_nav()?;
                ev.extend(self.emit_entries()?);
                ev.push(AppEvent::UnreadChanged {
                    total: self.store.unread_count()?,
                });
                ev.push(AppEvent::Status {
                    message: format!("已删除源 {}", id.0),
                });
                Ok(ev)
            }
            AppCommand::RenameFeed { id, title } => {
                self.store.rename_feed(id, &title)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: format!("已重命名「{title}」"),
                });
                Ok(ev)
            }
            AppCommand::EditFeedUrl { id, feed_url } => {
                self.store.set_feed_url(id, &feed_url)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: "已更新订阅 URL".into(),
                });
                Ok(ev)
            }
            AppCommand::MoveFeedToFolder { feed_id, folder_id } => {
                self.store.move_feed_to_folder(feed_id, folder_id)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: "已移动到文件夹".into(),
                });
                Ok(ev)
            }
            AppCommand::ToggleMuteFeed { id } => {
                let muted = self.store.toggle_mute_feed(id)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: if muted {
                        "已静音".into()
                    } else {
                        "已取消静音".into()
                    },
                });
                Ok(ev)
            }
            AppCommand::SetFeedRefreshInterval { id, secs } => {
                self.store.set_feed_refresh_interval(id, secs)?;
                self.emit_nav()
            }
            AppCommand::SetFeedCategory { id, category } => {
                self.store.set_feed_category(id, category)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: format!("已更新分类为「{}」", category.label()),
                });
                Ok(ev)
            }
            AppCommand::ToggleFeedProxy { id } => {
                let (_, _, _, use_proxy) = self.store.get_feed_fetch_meta(id)?;
                let next = !use_proxy;
                self.store.set_feed_use_proxy(id, next)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: if next {
                        "已开启代理".into()
                    } else {
                        "已关闭代理（直连）".into()
                    },
                });
                Ok(ev)
            }
            AppCommand::AutoRefresh => {
                // Caller (UI) checks interval and dispatches refresh tasks for due feeds.
                Ok(vec![])
            }
            AppCommand::CreateFolder { name } => {
                let id = self.store.add_folder(&name)?;
                let mut ev = self.emit_nav()?;
                ev.push(AppEvent::Status {
                    message: format!("已创建文件夹「{name}」id={}", id.0),
                });
                Ok(ev)
            }
            AppCommand::RefreshFeeds { feed_id } => {
                // Synchronous fallback; UI should prefer prepare/apply for async.
                let ids = match feed_id {
                    Some(id) => vec![id],
                    None => self.store.list_feed_ids()?,
                };
                let mut ev = Vec::new();
                for id in ids {
                    if let Some(outcome) = self.refresh_one_sync(id) {
                        ev.extend(self.apply_refresh_outcome_inner(outcome)?);
                    }
                }
                ev.extend(self.emit_nav()?);
                ev.extend(self.emit_entries()?);
                ev.push(AppEvent::UnreadChanged {
                    total: self.store.unread_count()?,
                });
                ev.push(AppEvent::Status {
                    message: "刷新完成".into(),
                });
                Ok(ev)
            }
            AppCommand::ImportOpml { content } => self.import_opml(&content),
            AppCommand::ExportOpml => {
                let feeds = self.store.list_feeds()?;
                let xml = opml::export_opml(&feeds);
                Ok(vec![
                    AppEvent::OpmlExported { xml },
                    AppEvent::Status {
                        message: format!("已导出 {} 个订阅", feeds.len()),
                    },
                ])
            }
            AppCommand::ExtractEntry { id } => {
                // Manual trigger. UI normally uses prepare_extract_task +
                // apply_extract_outcome for async; this is a sync fallback.
                let detail = self.store.get_entry(id)?;
                let url = match detail.summary.url.as_deref() {
                    Some(u) => u.to_string(),
                    None => {
                        return Err(CoreError::Message(
                            "entry has no URL; cannot extract".into(),
                        ))
                    }
                };
                let task = ExtractTask { entry_id: id, url };
                let outcome = crate::extract::run_extract_task(&self.http.inner, &task);
                Ok(self.apply_extract_outcome_inner(outcome)?)
            }
            AppCommand::EnhanceEntry { id, action } => {
                // Sync fallback. UI normally uses prepare_enhance_task +
                // apply_enhance_outcome for async (AI calls are slow).
                let cfg = match self.ai_config.as_ref() {
                    Some(c) => Arc::clone(c),
                    None => return Err(CoreError::Message("AI 未配置 (AppConfig.ai 为空)".into())),
                };
                let task = match self.prepare_enhance_task(id, action)? {
                    Some(t) => t,
                    None => {
                        return Err(CoreError::Message(
                            "entry 无内容可增强 (content_html 为空)".into(),
                        ))
                    }
                };
                let outcome = run_enhance_task(&self.http.inner, &cfg, &task);
                Ok(self.apply_enhance_outcome_inner(outcome)?)
            }
        }
    }

    // --- Async refresh: UI calls prepare → thread → apply ---

    pub fn prepare_refresh_tasks(&self, feed_id: Option<FeedId>) -> Result<Vec<RefreshTask>> {
        let ids = match feed_id {
            Some(id) => vec![id],
            None => self.store.list_feed_ids()?,
        };
        let mut tasks = Vec::with_capacity(ids.len());
        for id in ids {
            let (url, etag, last_modified, use_proxy) = self.store.get_feed_fetch_meta(id)?;
            tasks.push(RefreshTask {
                feed_id: id,
                url,
                etag,
                last_modified,
                use_proxy,
                existing_guids: self.store.list_guids_for_feed(id).unwrap_or_default(),
            });
        }
        Ok(tasks)
    }

    pub fn prepare_auto_refresh_tasks(
        &self,
        global_interval_secs: i64,
    ) -> Result<Vec<RefreshTask>> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let due = self
            .store
            .feeds_due_for_refresh(global_interval_secs, now)?;
        if due.is_empty() {
            return Ok(vec![]);
        }
        let mut tasks = Vec::with_capacity(due.len());
        for id in due {
            let (url, etag, last_modified, use_proxy) = self.store.get_feed_fetch_meta(id)?;
            tasks.push(RefreshTask {
                feed_id: id,
                url,
                etag,
                last_modified,
                use_proxy,
                existing_guids: self.store.list_guids_for_feed(id).unwrap_or_default(),
            });
        }
        Ok(tasks)
    }

    /// Apply one background refresh result; returns events to emit.
    pub fn apply_refresh_outcome(&mut self, outcome: RefreshOutcome) -> Vec<AppEvent> {
        match self.apply_refresh_outcome_inner(outcome) {
            Ok(ev) => ev,
            Err(e) => vec![AppEvent::Error {
                message: e.to_string(),
            }],
        }
    }

    fn apply_refresh_outcome_inner(&mut self, outcome: RefreshOutcome) -> Result<Vec<AppEvent>> {
        match outcome {
            RefreshOutcome::NotModified { feed_id } => {
                self.store
                    .update_feed_after_fetch(feed_id, None, None, None, None, None)?;
                Ok(vec![])
            }
            RefreshOutcome::Updated {
                feed_id,
                parsed,
                etag,
                last_modified,
            } => {
                self.store.update_feed_after_fetch(
                    feed_id,
                    Some(&parsed.title),
                    parsed.site_url.as_deref(),
                    etag.as_deref(),
                    last_modified.as_deref(),
                    None,
                )?;
                // Persist favicon URL if the feed provides one.
                if parsed.favicon_url.is_some() {
                    self.store
                        .set_favicon_url(feed_id, parsed.favicon_url.as_deref())?;
                }
                let mut new_items = 0u32;
                for e in &parsed.entries {
                    if self.store.upsert_entry(
                        feed_id,
                        &e.guid,
                        &e.title,
                        e.url.as_deref(),
                        e.author.as_deref(),
                        e.published_at,
                        e.summary.as_deref(),
                        &e.content_html,
                        e.thumbnail.as_deref(),
                    )? {
                        new_items += 1;
                    }
                }
                Ok(vec![AppEvent::Status {
                    message: format!("「{}」+{} 篇", parsed.title, new_items),
                }])
            }
            RefreshOutcome::Error { feed_id, error } => {
                self.store.update_feed_after_fetch(
                    feed_id,
                    None,
                    None,
                    None,
                    None,
                    Some(&error),
                )?;
                Ok(vec![AppEvent::Error {
                    message: format!("源 {} 失败: {error}", feed_id.0),
                }])
            }
        }
    }

    // --- Async extraction: UI calls prepare → thread → apply ---

    /// Build an extraction task for an entry. Returns None if the entry has no
    /// URL, is already long enough (full feed content), or already extracted.
    pub fn prepare_extract_task(&self, id: EntryId) -> Result<Option<ExtractTask>> {
        let entry = self.store.get_entry(id)?;
        // Skip if already extracted (don't re-extract on every open).
        if !entry.extracted_html.is_empty() {
            return Ok(None);
        }
        // Skip if the feed content is already substantial.
        if !crate::extract::should_extract(&entry.content_html, entry.summary.url.as_deref()) {
            return Ok(None);
        }
        match entry.summary.url {
            Some(u) => Ok(Some(ExtractTask {
                entry_id: id,
                url: u,
            })),
            None => Ok(None),
        }
    }

    /// Apply one background extraction result; returns events to emit.
    pub fn apply_extract_outcome(&mut self, outcome: ExtractOutcome) -> Vec<AppEvent> {
        match self.apply_extract_outcome_inner(outcome) {
            Ok(ev) => ev,
            Err(e) => vec![AppEvent::Error {
                message: e.to_string(),
            }],
        }
    }

    fn apply_extract_outcome_inner(&mut self, outcome: ExtractOutcome) -> Result<Vec<AppEvent>> {
        match outcome {
            ExtractOutcome::Extracted { entry_id, html } => {
                self.store.set_extracted_html(entry_id, &html)?;
                Ok(vec![AppEvent::EntryExtracted {
                    id: entry_id,
                    success: true,
                }])
            }
            ExtractOutcome::Failed { entry_id, error } => Ok(vec![
                AppEvent::EntryExtracted {
                    id: entry_id,
                    success: false,
                },
                AppEvent::Status {
                    message: format!("全文抽取失败: {error}"),
                },
            ]),
        }
    }

    // --- Async AI enhance: UI calls prepare → thread → apply ---

    /// 构建一个增强任务。优先用 `extracted_html`（全文抽取结果），否则用
    /// `content_html`（feed 原始正文）。两者皆空返回 `Ok(None)`。
    ///
    /// 不依赖 `ai_config`：UI 在调用前自行检查 `AppConfig.ai` 是否存在。
    /// 不检查是否已有 enhancement——手动触发允许覆盖重新生成。
    pub fn prepare_enhance_task(
        &self,
        id: EntryId,
        action: EnhanceAction,
    ) -> Result<Option<EnhanceTask>> {
        let entry = self.store.get_entry(id)?;
        let content = if !entry.extracted_html.is_empty() {
            entry.extracted_html.clone()
        } else {
            entry.content_html.clone()
        };
        if content.is_empty() {
            return Ok(None);
        }
        Ok(Some(EnhanceTask {
            entry_id: id,
            action,
            title: entry.summary.title.clone(),
            content,
        }))
    }

    /// 应用一个后台增强结果；返回要发出的事件。
    pub fn apply_enhance_outcome(&mut self, outcome: EnhanceOutcome) -> Vec<AppEvent> {
        match self.apply_enhance_outcome_inner(outcome) {
            Ok(ev) => ev,
            Err(e) => vec![AppEvent::Error {
                message: e.to_string(),
            }],
        }
    }

    fn apply_enhance_outcome_inner(&mut self, outcome: EnhanceOutcome) -> Result<Vec<AppEvent>> {
        match outcome {
            EnhanceOutcome::Success {
                entry_id,
                kind,
                result,
            } => {
                self.store.set_enhancement(entry_id, &kind, &result)?;
                Ok(vec![AppEvent::EntryEnhanced {
                    id: entry_id,
                    kind,
                    success: true,
                }])
            }
            EnhanceOutcome::Failed {
                entry_id,
                kind,
                error,
            } => Ok(vec![
                AppEvent::EntryEnhanced {
                    id: entry_id,
                    kind,
                    success: false,
                },
                AppEvent::Status {
                    message: format!("AI 增强失败: {error}"),
                },
            ]),
        }
    }

    /// Query unread count per feed (lightweight, for badge updates).
    pub fn unread_counts_per_feed(&self) -> std::collections::HashMap<FeedId, u64> {
        self.store
            .unread_counts_per_feed()
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    fn refresh_one_sync(&mut self, id: FeedId) -> Option<RefreshOutcome> {
        let (url, etag, last_modified, use_proxy) = self.store.get_feed_fetch_meta(id).ok()?;
        let client = pick_client(use_proxy, &self.http, &self.http_proxy);
        // §11.5 刷新时先做插件路由：URL 命中已加载插件则走 Tier 1/2，
        // 跳过 RSS fetch（插件不做条件请求，每次拉新）。未命中走默认 RSS。
        let existing = self.store.list_guids_for_feed(id).unwrap_or_default();
        if let Some(res) = self.fetch_via_plugin(&url, client, &existing) {
            return Some(match res {
                Ok(parsed) => RefreshOutcome::Updated {
                    feed_id: id,
                    parsed,
                    etag: None,
                    last_modified: None,
                },
                Err(e) => RefreshOutcome::Error {
                    feed_id: id,
                    error: e.to_string(),
                },
            });
        }
        match fetch_feed_bytes(client, &url, etag.as_deref(), last_modified.as_deref()) {
            Ok(FetchResult::NotModified) => Some(RefreshOutcome::NotModified { feed_id: id }),
            Ok(FetchResult::Body {
                bytes,
                etag,
                last_modified,
                ..
            }) => match parse_feed(&bytes) {
                Ok(parsed) => Some(RefreshOutcome::Updated {
                    feed_id: id,
                    parsed,
                    etag,
                    last_modified,
                }),
                Err(e) => Some(RefreshOutcome::Error {
                    feed_id: id,
                    error: e.to_string(),
                }),
            },
            Err(e) => Some(RefreshOutcome::Error {
                feed_id: id,
                error: e.to_string(),
            }),
        }
    }

    fn add_feed_from_url(&mut self, feed_url: &str) -> Result<Vec<AppEvent>> {
        let trimmed = feed_url.trim();
        if trimmed.is_empty() {
            return Err(CoreError::Message("订阅 URL 为空".into()));
        }
        // §11.5.2 Tier 0: 对 GitHub releases / YouTube channel 做 URL 规范化。
        // 规范化后的 URL 才是真正入库的订阅地址。
        let url = crate::feed::tier0::normalize(trimmed);
        if let Some(existing) = self.store.find_feed_by_url(&url)? {
            let mut ev = vec![AppEvent::Status {
                message: format!("源已存在，已刷新 id={}", existing.0),
            }];
            if let Some(outcome) = self.refresh_one_sync(existing) {
                ev.extend(self.apply_refresh_outcome_inner(outcome)?);
            }
            ev.extend(self.emit_nav()?);
            ev.extend(self.emit_entries()?);
            return Ok(ev);
        }

        // Try 0: §11.5 插件路由——URL 命中已加载插件则走 Tier 1/2，跳过 RSS。
        match self.try_add_via_plugin(&url) {
            Ok(Some(events)) => return Ok(events),
            Ok(None) => {} // 未命中插件，继续 RSS 路径。
            Err(e) => return Err(e),
        }

        // Try 1: parse URL directly as a feed.
        match self.try_add_as_feed(&url) {
            Ok(Some(events)) => return Ok(events),
            Ok(None) => {} // Not a feed, fall through to discovery.
            Err(e) => return Err(e),
        }

        // Try 2: fetch as HTML and discover feed links.
        self.try_discover_and_add(&url)
    }

    /// Try to add the URL as a feed. Returns Ok(None) if it's not a feed.
    fn try_add_as_feed(&mut self, url: &str) -> Result<Option<Vec<AppEvent>>> {
        let fetched = match fetch_feed_bytes(&self.http, url, None, None) {
            Ok(r) => r,
            Err(_) => return Ok(None), // Network error — not a feed.
        };
        let (bytes, etag, last_modified, _final_url) = match fetched {
            FetchResult::NotModified => {
                return Err(CoreError::Http("unexpected 304 on first fetch".into()));
            }
            FetchResult::Body {
                bytes,
                etag,
                last_modified,
                final_url,
            } => (bytes, etag, last_modified, final_url),
        };
        let parsed = match parse_feed(&bytes) {
            Ok(p) => p,
            Err(_) => return Ok(None), // Not a feed.
        };
        let ev = self.ingest_new_feed(url, parsed, etag, last_modified)?;
        Ok(Some(ev))
    }

    /// §11.5 订阅时的插件路由：URL 命中已加载插件则用 Tier 1/2 产出的 feed 入库。
    /// 返回 `Ok(None)` 表示未命中插件（调用方走 RSS 发现）；`Err` 表示插件命中但失败。
    fn try_add_via_plugin(&mut self, url: &str) -> Result<Option<Vec<AppEvent>>> {
        // 新订阅默认直连（use_proxy 之后可在右键菜单单独开启）。
        let parsed = match self.fetch_via_plugin(url, &self.http, &[]) {
            None => return Ok(None),
            Some(Err(e)) => return Err(e),
            Some(Ok(p)) => p,
        };
        let ev = self.ingest_new_feed(url, parsed, None, None)?;
        Ok(Some(ev))
    }

    /// 把一个已解析的 feed 入库：建 feed、写 meta、upsert entries、emit 事件。
    /// 供 RSS 路径 (`try_add_as_feed`) 与插件路径 (`try_add_via_plugin`) 共用。
    fn ingest_new_feed(
        &mut self,
        url: &str,
        parsed: ParsedFeed,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Result<Vec<AppEvent>> {
        let id = self
            .store
            .add_feed(&parsed.title, url, None, crate::feed::categorize(url))?;
        self.store.update_feed_after_fetch(
            id,
            Some(&parsed.title),
            parsed.site_url.as_deref(),
            etag.as_deref(),
            last_modified.as_deref(),
            None,
        )?;
        if parsed.favicon_url.is_some() {
            self.store
                .set_favicon_url(id, parsed.favicon_url.as_deref())?;
        }
        let mut new_items = 0u32;
        for e in &parsed.entries {
            if self.store.upsert_entry(
                id,
                &e.guid,
                &e.title,
                e.url.as_deref(),
                e.author.as_deref(),
                e.published_at,
                e.summary.as_deref(),
                &e.content_html,
                e.thumbnail.as_deref(),
            )? {
                new_items += 1;
            }
        }
        let mut ev = self.emit_nav()?;
        ev.extend(self.emit_entries()?);
        ev.push(AppEvent::UnreadChanged {
            total: self.store.unread_count()?,
        });
        ev.push(AppEvent::Status {
            message: format!("已订阅「{}」· 新文章 {} 篇", parsed.title, new_items),
        });
        Ok(ev)
    }

    /// Fetch the URL as HTML, discover feed links, and subscribe to the first one.
    fn try_discover_and_add(&mut self, url: &str) -> Result<Vec<AppEvent>> {
        let fetched = fetch_feed_bytes(&self.http, url, None, None)?;
        let bytes = match fetched {
            FetchResult::Body { bytes, .. } => bytes,
            FetchResult::NotModified => {
                return Err(CoreError::Http("unexpected 304 on first fetch".into()));
            }
        };
        let html = String::from_utf8_lossy(&bytes);
        let feeds = discover_feed_urls(&html, url);
        if feeds.is_empty() {
            return Err(CoreError::Message(format!(
                "无法识别为 feed，也未在页面中发现 feed 链接: {url}"
            )));
        }

        // Subscribe to the first discovered feed.
        let (feed_url, feed_title) = &feeds[0];
        let title = feed_title.as_deref().unwrap_or(feed_url.as_str());
        match self.try_add_as_feed(feed_url) {
            Ok(Some(mut events)) => {
                // Patch status message to mention discovery.
                for ev in &mut events {
                    if let AppEvent::Status { message } = ev {
                        *message = format!("自动发现并订阅「{title}」\n{message}");
                    }
                }
                Ok(events)
            }
            Ok(None) => Err(CoreError::Message(format!(
                "发现 feed 链接但订阅失败: {feed_url}"
            ))),
            Err(e) => Err(e),
        }
    }

    fn import_opml(&mut self, content: &str) -> Result<Vec<AppEvent>> {
        let outlines = opml::parse_opml(content);
        let mut added = 0u32;
        for o in &outlines {
            if self.store.find_feed_by_url(&o.feed_url)?.is_none() {
                let title = if o.title.is_empty() {
                    o.feed_url.clone()
                } else {
                    o.title.clone()
                };
                self.store.add_feed(
                    &title,
                    &o.feed_url,
                    None,
                    crate::feed::categorize(&o.feed_url),
                )?;
                added += 1;
            }
        }
        let mut ev = self.emit_nav()?;
        ev.push(AppEvent::Status {
            message: format!("OPML 导入：新增 {added} 个订阅（共 {} 条）", outlines.len()),
        });
        Ok(ev)
    }

    fn emit_nav(&self) -> Result<Vec<AppEvent>> {
        let pairs = self.store.unread_counts_per_feed()?;
        let unread_per_feed: std::collections::HashMap<FeedId, u64> = pairs.into_iter().collect();
        Ok(vec![AppEvent::NavUpdated {
            folders: self.store.list_folders()?,
            feeds: self.store.list_feeds()?,
            unread_total: self.store.unread_count_excluding_muted()?,
            unread_per_feed,
        }])
    }

    fn emit_entries(&self) -> Result<Vec<AppEvent>> {
        let q = self.search_query.trim();
        let entries = if q.is_empty() {
            self.store.list_entries(self.filter)?
        } else {
            self.store.search_entries(q, 500)?
        };
        Ok(vec![AppEvent::EntriesUpdated { entries }])
    }
}

/// 构建直连 + 带代理两套 HTTP 客户端。代理 URL 为空时 `http_proxy = None`。
/// 代理 URL 非法时不阻塞核心功能：stderr 记录并回退直连（启动时配置可能已损坏）。
fn build_http_clients(
    proxy_url: Option<&str>,
) -> Result<(String, Arc<HttpClient>, Option<Arc<HttpClient>>)> {
    let proxy_url = proxy_url.unwrap_or("").trim().to_string();
    let http = Arc::new(HttpClient::new()?);
    let http_proxy = if proxy_url.is_empty() {
        None
    } else {
        match HttpClient::with_proxy(Some(&proxy_url)) {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                eprintln!("glean: invalid proxy {proxy_url:?}: {e}");
                None
            }
        }
    };
    Ok((proxy_url, http, http_proxy))
}

/// §11.5 worker 线程的刷新上下文快照：共享 `PluginManager`/`HttpClient`，
/// 克隆 `CredentialStore`。`None` 字段表示该能力不可用，worker 走默认 RSS。
///
/// 由 `GleanService::refresh_ctx()` 构造，传给 `run_refresh_task_with_ctx`。
#[derive(Clone)]
pub struct RefreshCtx {
    pub plugin_mgr: Option<Arc<PluginManager>>,
    pub http: Arc<HttpClient>,
    /// 带代理的客户端（`use_proxy = true` 的订阅使用）；`None` = 未配置代理，回退直连。
    pub http_proxy: Option<Arc<HttpClient>>,
    pub credentials: Option<CredentialStore>,
}

/// 按订阅的 use_proxy 选择 HTTP 客户端；未配置代理时回退直连。
fn pick_client<'a>(
    use_proxy: bool,
    http: &'a Arc<HttpClient>,
    http_proxy: &'a Option<Arc<HttpClient>>,
) -> &'a Arc<HttpClient> {
    if use_proxy {
        http_proxy.as_ref().unwrap_or(http)
    } else {
        http
    }
}

/// 在 worker 线程执行一次刷新。逻辑与 `GleanService::refresh_one_sync` 对齐：
/// 先做插件路由（Tier 1/2），未命中走默认 RSS（fetch + parse）。
/// 插件不做条件请求，命中即拉新；RSS 路径保留 etag/last_modified。
pub fn run_refresh_task_with_ctx(task: RefreshTask, ctx: &RefreshCtx) -> RefreshOutcome {
    let client = pick_client(task.use_proxy, &ctx.http, &ctx.http_proxy);
    if let Some(mgr) = ctx.plugin_mgr.as_deref() {
        if let Some(res) = mgr.run_tier1_for_url(&task.url, client).transpose() {
            return match res {
                Ok(parsed) => RefreshOutcome::Updated {
                    feed_id: task.feed_id,
                    parsed,
                    etag: None,
                    last_modified: None,
                },
                Err(e) => RefreshOutcome::Error {
                    feed_id: task.feed_id,
                    error: e.to_string(),
                },
            };
        }
        let creds = ctx.credentials.as_ref().map(|c| Arc::new(c.clone()));
        if let Some(res) = mgr
            .run_tier2_for_url(&task.url, Arc::clone(client), creds, &task.existing_guids)
            .transpose()
        {
            return match res {
                Ok(parsed) => RefreshOutcome::Updated {
                    feed_id: task.feed_id,
                    parsed,
                    etag: None,
                    last_modified: None,
                },
                Err(e) => RefreshOutcome::Error {
                    feed_id: task.feed_id,
                    error: e.to_string(),
                },
            };
        }
    }
    // 默认 RSS 路径
    match fetch_feed_bytes(
        client,
        &task.url,
        task.etag.as_deref(),
        task.last_modified.as_deref(),
    ) {
        Ok(FetchResult::NotModified) => RefreshOutcome::NotModified {
            feed_id: task.feed_id,
        },
        Ok(FetchResult::Body {
            bytes,
            etag,
            last_modified,
            ..
        }) => match parse_feed(&bytes) {
            Ok(parsed) => RefreshOutcome::Updated {
                feed_id: task.feed_id,
                parsed,
                etag,
                last_modified,
            },
            Err(e) => RefreshOutcome::Error {
                feed_id: task.feed_id,
                error: e.to_string(),
            },
        },
        Err(e) => RefreshOutcome::Error {
            feed_id: task.feed_id,
            error: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::EnhanceAction;
    use crate::model::EntryFilter;

    /// Demo entry 有 content_html，prepare 应返回 Some(task)，title/content 已填充。
    #[test]
    fn prepare_enhance_task_for_entry_with_content() {
        let mut svc = GleanService::open_in_memory().unwrap();
        bootstrap_with_demo(&mut svc);
        let id = first_entry_id(&svc);
        let task = svc
            .prepare_enhance_task(id, EnhanceAction::Summarize)
            .expect("prepare")
            .expect("Some(task)");
        assert_eq!(task.entry_id, id);
        assert!(!task.title.is_empty());
        assert!(!task.content.is_empty());
        assert_eq!(task.action.kind_str(), "summary");
    }

    /// 空内容的 entry，prepare 返回 Ok(None)。
    #[test]
    fn prepare_enhance_task_returns_none_for_empty_content() {
        let mut svc = GleanService::open_in_memory().unwrap();
        bootstrap_with_demo(&mut svc);
        let fid = svc.store.list_feeds().unwrap()[0].id;
        let id = svc
            .store
            .add_entry(fid, "g-empty", "空条目", None, "")
            .unwrap();
        let task = svc
            .prepare_enhance_task(id, EnhanceAction::Summarize)
            .expect("prepare");
        assert!(task.is_none());
    }

    /// apply Success 落库 + 发 EntryEnhanced{success:true}。
    #[test]
    fn apply_enhance_outcome_success_persists_and_emits() {
        let mut svc = GleanService::open_in_memory().unwrap();
        bootstrap_with_demo(&mut svc);
        let id = first_entry_id(&svc);
        let outcome = EnhanceOutcome::Success {
            entry_id: id,
            kind: "summary".into(),
            result: "这是摘要。".into(),
        };
        let ev = svc.apply_enhance_outcome(outcome);
        assert!(ev.iter().any(|e| matches!(
            e,
            AppEvent::EntryEnhanced { id: eid, kind, success } if *eid == id && kind == "summary" && *success
        )));
        let stored = svc.store.get_enhancement(id, "summary").unwrap();
        assert_eq!(stored.as_deref(), Some("这是摘要。"));
    }

    /// apply Failed 不落库 + 发 EntryEnhanced{success:false} + Status。
    #[test]
    fn apply_enhance_outcome_failed_emits_error() {
        let mut svc = GleanService::open_in_memory().unwrap();
        bootstrap_with_demo(&mut svc);
        let id = first_entry_id(&svc);
        let outcome = EnhanceOutcome::Failed {
            entry_id: id,
            kind: "translate".into(),
            error: "boom".into(),
        };
        let ev = svc.apply_enhance_outcome(outcome);
        assert!(ev.iter().any(|e| matches!(
            e,
            AppEvent::EntryEnhanced { id: eid, kind, success } if *eid == id && kind == "translate" && !*success
        )));
        assert!(svc
            .store
            .get_enhancement(id, "translate")
            .unwrap()
            .is_none());
    }

    /// 同步 `EnhanceEntry` 在未配置 AI 时返回 Error 事件，不发网络请求。
    #[test]
    fn enhance_entry_sync_errors_without_ai_config() {
        let mut svc = GleanService::open_in_memory().unwrap();
        bootstrap_with_demo(&mut svc);
        let id = first_entry_id(&svc);
        let ev = svc.handle(AppCommand::EnhanceEntry {
            id,
            action: EnhanceAction::Summarize,
        });
        assert!(ev.iter().any(|e| matches!(
            e,
            AppEvent::Error { message } if message.contains("AI 未配置")
        )));
    }

    fn first_entry_id(svc: &GleanService) -> EntryId {
        svc.store
            .list_entries(EntryFilter::All)
            .unwrap()
            .first()
            .expect("at least one demo entry")
            .id
    }

    /// Bootstrap 不再自动种 demo 数据，测试需显式造数据。
    fn bootstrap_with_demo(svc: &mut GleanService) {
        svc.handle(AppCommand::Bootstrap);
        svc.store.seed_demo_if_empty().unwrap();
    }
}
