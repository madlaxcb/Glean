//! Glean app — Hybrid shell (egui) + WebView2 reader + core service.
//!
//! See docs/Glean-开发方案.md and docs/spike-ui.md.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod fonts;
mod img_server;
mod reader;
mod tray;
mod ui;
mod update;

use eframe::egui;
use glean_core::{
    default_config_path, default_db_path, run_enhance_task, run_extract_task,
    run_refresh_task_with_ctx, AppCommand, AppConfig, AppEvent, EnhanceAction, EnhanceOutcome,
    EntryDetail, EntryFilter, EntryId, EntrySummary, ExtractOutcome, FaviconCache, Feed,
    FeedCategory, FeedId, Folder, FolderId, GleanService, ImagePolicy, ReaderHostMode, RefreshCtx,
    RefreshOutcome, RefreshTask,
};
use reader::ReaderHost;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use tray::Tray;
use ui::SpikeApp;
use update::{check_for_update, UpdateCheckResult, APPCAST_URL};

fn main() -> eframe::Result<()> {
    // Single-instance lock: exit if another instance is already running.
    let _lock = match single_instance_lock() {
        Some(lock) => lock,
        None => {
            eprintln!("Glean 已在运行。");
            return Ok(());
        }
    };

    // Load persisted config to restore window geometry (dev plan §9 M3).
    let config = load_config(&default_config_path());

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([900.0, 600.0])
        .with_title("Glean / 拾光");
    if let (Some(x), Some(y)) = (config.window_x, config.window_y) {
        viewport = viewport.with_position([x, y]);
    }
    if let (Some(w), Some(h)) = (config.window_w, config.window_h) {
        viewport = viewport.with_inner_size([w, h]);
    }
    if config.window_maximized {
        viewport = viewport.with_maximized(true);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Glean",
        options,
        Box::new(|cc| Ok(Box::new(SpikeApp::new(cc)))),
    )
}

/// Acquire a single-instance lock. Returns None if another instance holds it.
#[cfg(windows)]
fn single_instance_lock() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "GleanSingleInstance\0".encode_utf16().collect();
    unsafe {
        let handle = CreateMutexW(None, false, PCWSTR(name.as_ptr())).ok()?;
        // ERROR_ALREADY_EXISTS = 183
        if GetLastError().0 == 183 {
            None
        } else {
            Some(handle)
        }
    }
}

#[cfg(not(windows))]
fn single_instance_lock() -> Option<std::fs::File> {
    use fs4::fs_std::FileExt;
    let lock_path = std::env::temp_dir().join("glean-single-instance.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    if file.try_lock_exclusive().is_err() {
        return None;
    }
    Some(file)
}

/// Max concurrent HTTP fetches during a refresh batch (dev plan §7.1: 4–8).
/// 调低到 1：Pixiv 等插件 API 对并发限流严格（HTTP 429），2 并发仍会
/// 触发限流；串行刷新（一次只抓一个订阅）最稳妥，对 RSS 抓取影响很小。
const REFRESH_WORKERS: usize = 1;

/// Spawn bounded worker threads that fetch+parse in parallel, sending each
/// `RefreshOutcome` to the shared channel. Sender clones drop per-worker;
/// the receiver sees disconnect only after all workers finish.
///
/// `cancel` 是「停止刷新」标志：worker 在每处理一个订阅前检查，置位则
/// 提前退出（不再发送结果）。正在进行的单个 HTTP 请求不受影响（有超时）。
fn spawn_refresh_workers(
    tasks: Vec<RefreshTask>,
    ctx: RefreshCtx,
    tx: mpsc::Sender<RefreshOutcome>,
    cancel: Arc<AtomicBool>,
) {
    let n = tasks.len();
    if n == 0 {
        return;
    }
    let workers = REFRESH_WORKERS.min(n);
    // Round-robin chunk so each worker gets a balanced slice.
    let mut chunks: Vec<Vec<RefreshTask>> = vec![Vec::new(); workers];
    for (i, task) in tasks.into_iter().enumerate() {
        chunks[i % workers].push(task);
    }
    for chunk in chunks {
        if chunk.is_empty() {
            continue;
        }
        let tx = tx.clone();
        let ctx = ctx.clone();
        let cancel = cancel.clone();
        thread::spawn(move || {
            for task in chunk {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let outcome = run_refresh_task_with_ctx(task, &ctx);
                let _ = tx.send(outcome);
            }
        });
    }
}

/// UI-thread state: projects AppEvent; sends AppCommand to GleanService.
pub struct SpikeState {
    pub service: GleanService,
    pub folders: Vec<Folder>,
    pub feeds: Vec<Feed>,
    pub entries: Vec<EntrySummary>,
    pub unread_total: u64,
    /// Unread count per feed (FeedId -> count).
    pub unread_per_feed: std::collections::HashMap<glean_core::FeedId, u64>,
    pub selected: Option<usize>,
    pub open_detail: Option<EntryDetail>,
    pub filter: EntryFilter,
    pub dark: bool,
    pub host_mode: ReaderHostMode,
    pub nav_width: f32,
    pub list_width: f32,
    pub status: String,
    pub open_count: u64,
    pub search: String,
    pub reader_rect: egui::Rect,
    pub reader: ReaderHost,
    pub splitting: bool,
    pub feed_url_input: String,
    /// 添加订阅时选择的分类（None = 沿用自动分类 categorize）。
    pub feed_add_category: Option<FeedCategory>,
    /// 添加订阅时选择的目标文件夹（None = 无文件夹）。
    pub feed_add_folder: Option<FolderId>,
    /// 添加订阅时新建文件夹名（非空则新建并放入该文件夹）。
    pub feed_add_new_folder: String,
    /// Background refresh state.
    refresh_rx: Option<mpsc::Receiver<RefreshOutcome>>,
    refresh_pending: usize,
    /// 「停止刷新」取消标志：每次启动刷新创建新的 Arc；停止时置 true，
    /// worker 线程在下一订阅前退出。
    refresh_cancel: Arc<AtomicBool>,
    /// Clipboard-captured OPML export text (for copy/paste).
    pub opml_export: Option<String>,
    /// Pasted OPML text for import.
    pub opml_import_input: String,
    /// Feed being renamed (id + current title for editing).
    pub rename_feed: Option<(glean_core::FeedId, String)>,
    /// Feed URL being edited (id + current feed_url).
    pub edit_feed_url: Option<(glean_core::FeedId, String)>,
    /// Recent error messages for notification popup.
    pub errors: Vec<String>,
    /// New folder name input.
    pub new_folder_input: String,
    /// Persistent config.
    pub config: AppConfig,
    /// Config file path.
    config_path: std::path::PathBuf,
    /// Auto-refresh timer: seconds since last check.
    auto_refresh_timer: f32,
    /// Persistent buffer for the refresh-interval TextEdit in settings.
    /// Must outlive the frame so user typing isn't overwritten each frame.
    pub refresh_interval_input: String,
    /// Buffer for font size input in settings.
    pub font_size_input: String,
    /// Buffer for line width input in settings.
    pub line_width_input: String,
    /// Buffer for cache directory input in settings.
    pub cache_dir_input: String,
    /// AI 设置缓冲：Base URL（TextEdit 需跨帧存活）。
    pub ai_base_url_input: String,
    /// AI 设置缓冲：模型名。
    pub ai_model_input: String,
    /// AI 设置缓冲：api_key 明文（password 输入；保存时加密为 cipher）。
    pub ai_key_input: String,
    /// AI 设置缓冲：翻译目标语言。
    pub ai_lang_input: String,
    /// Per-article one-shot override: when true, reader renders with Allow
    /// regardless of config.image_policy. Reset on entry switch.
    pub reader_show_images: bool,
    /// System tray (Windows only; None on Linux stub).
    pub tray: Tray,
    /// Background update-check receiver (once, on startup).
    update_rx: Option<mpsc::Receiver<UpdateCheckResult>>,
    /// Pending update notification (set when remote version > current).
    pub update_available: Option<UpdateCheckResult>,
    /// Background full-text extraction receiver.
    extract_rx: Option<mpsc::Receiver<ExtractOutcome>>,
    /// Entry currently being extracted (to avoid duplicate tasks).
    extract_in_flight: Option<EntryId>,
    /// Background AI enhance receiver.
    enhance_rx: Option<mpsc::Receiver<EnhanceOutcome>>,
    /// (entry_id, kind) currently being enhanced; prevents duplicate concurrent tasks.
    enhance_in_flight: Option<(EntryId, String)>,
    /// Background image-cache receiver: (rewritten_html, dark, image_policy).
    img_cache_rx: Option<mpsc::Receiver<(String, bool, ImagePolicy)>>,
    /// Loopback HTTP origin that serves files from the local image cache
    /// (`http://127.0.0.1:PORT`). Keeps large originals out of data URLs.
    img_serve_base: Option<String>,
    /// Background favicon download receiver: (feed_id, rgba_bytes, width, height).
    favicon_rx: Option<mpsc::Receiver<(FeedId, Vec<u8>, u32, u32)>>,
    /// Set of feed IDs whose favicon download is already in flight.
    favicon_pending: std::collections::HashSet<FeedId>,
    /// Background thumbnail download receiver: (entry_id, rgba_bytes, width, height).
    thumbnail_rx: Option<mpsc::Receiver<(EntryId, Vec<u8>, u32, u32)>>,
    /// Set of entry IDs whose thumbnail download is already in flight.
    thumbnail_pending: std::collections::HashSet<EntryId>,
    thumbnail_failed: std::collections::HashSet<EntryId>,
    thumbnail_loaded: std::collections::HashSet<EntryId>,
    /// 导航栏分类组折叠状态（会话内有效，不持久化）。
    pub collapsed_categories: std::collections::HashSet<glean_core::FeedCategory>,
    /// 导航栏当前选中的分类 tab（None = 显示全部）。
    pub nav_active_category: Option<glean_core::FeedCategory>,
    /// 导航栏文件夹展开状态（会话内有效）。展开的文件夹 id 集合。
    pub expanded_folders: std::collections::HashSet<FolderId>,
    /// 拖拽前保存的文件夹展开状态，拖拽结束后恢复。
    pub expanded_folders_before_drag: Option<std::collections::HashSet<FolderId>>,
    /// 导航区多选开关（会话内有效）。开启时，点击订阅行切换选中状态。
    pub feed_multi_select: bool,
    /// 多选模式下选中的订阅 id 集合。
    pub selected_feeds: std::collections::HashSet<glean_core::FeedId>,
    /// 导航区整理模式（拖拽调整订阅顺序）。与「拖入文件夹」拖拽互斥：
    /// 只在整理模式下拖拽行间插入排序，避免语义冲突。
    pub feed_sort_mode: bool,
    /// OPML 导入是否覆盖（false = 追加）。
    pub opml_import_overwrite: bool,
    /// 插件凭证槽编辑缓冲：key = `plugin_id:slot`，value = (header_name, header_value)。
    /// 跨帧存活，避免输入被每帧重绘覆盖。
    pub plugin_cred_edits: std::collections::HashMap<String, (String, String)>,
}

impl SpikeState {
    pub fn new() -> Self {
        let db = default_db_path();
        let config_path = default_config_path();
        let config = load_config(&config_path);
        let proxy = if config.proxy_url.is_empty() {
            None
        } else {
            Some(config.proxy_url.as_str())
        };

        // Apply custom cache directory from config (must be set before
        // GleanService::open, which calls cache_entries_dir() internally).
        if let Some(ref dir) = config.cache_dir {
            let path = std::path::PathBuf::from(dir);
            if let Err(e) = std::fs::create_dir_all(&path) {
                eprintln!("glean: cannot create cache dir {:?}: {e}", path);
            } else {
                glean_core::set_custom_cache_dir(path);
            }
        }

        let service = GleanService::open_path_with_proxy(&db, proxy).unwrap_or_else(|e| {
            eprintln!("glean: open db {:?}: {e}; falling back to memory", db);
            GleanService::open_in_memory_with_proxy(proxy).expect("memory store")
        });

        let mut s = Self {
            service,
            folders: Vec::new(),
            feeds: Vec::new(),
            entries: Vec::new(),
            unread_total: 0,
            unread_per_feed: std::collections::HashMap::new(),
            selected: None,
            open_detail: None,
            filter: EntryFilter::All,
            dark: config.dark,
            host_mode: ReaderHostMode::ChildEmbed,
            nav_width: config.nav_width,
            list_width: config.list_width,
            status: format!("库: {}", db.display()),
            open_count: 0,
            search: String::new(),
            reader_rect: egui::Rect::NOTHING,
            reader: ReaderHost::new(),
            splitting: false,
            feed_url_input: String::new(),
            feed_add_category: None,
            feed_add_folder: None,
            feed_add_new_folder: String::new(),
            refresh_rx: None,
            refresh_pending: 0,
            refresh_cancel: Arc::new(AtomicBool::new(false)),
            opml_export: None,
            opml_import_input: String::new(),
            rename_feed: None,
            edit_feed_url: None,
            errors: Vec::new(),
            new_folder_input: String::new(),
            refresh_interval_input: config.refresh_interval_secs.to_string(),
            font_size_input: config.font_size_px.to_string(),
            line_width_input: config.line_width_rem.to_string(),
            cache_dir_input: config.cache_dir.clone().unwrap_or_default(),
            // AI 设置缓冲：回填已存配置；api_key 解密回填便于查看/修改。
            ai_base_url_input: config
                .ai
                .as_ref()
                .map(|a| a.base_url.clone())
                .unwrap_or_default(),
            ai_model_input: config
                .ai
                .as_ref()
                .map(|a| a.model.clone())
                .unwrap_or_default(),
            ai_key_input: config
                .ai
                .as_ref()
                .and_then(|a| {
                    glean_core::plugin::credential::decrypt_secret(&a.api_key_cipher).ok()
                })
                .unwrap_or_default(),
            ai_lang_input: config.ai_translate_lang.clone(),
            config,
            config_path,
            auto_refresh_timer: 0.0,
            reader_show_images: false,
            tray: Tray::new(),
            update_rx: None,
            update_available: None,
            extract_rx: None,
            extract_in_flight: None,
            enhance_rx: None,
            enhance_in_flight: None,
            img_cache_rx: None,
            img_serve_base: None,
            favicon_rx: None,
            favicon_pending: std::collections::HashSet::new(),
            thumbnail_rx: None,
            thumbnail_pending: std::collections::HashSet::new(),
            thumbnail_failed: std::collections::HashSet::new(),
            thumbnail_loaded: std::collections::HashSet::new(),
            collapsed_categories: std::collections::HashSet::new(),
            nav_active_category: None,
            expanded_folders: std::collections::HashSet::new(),
            expanded_folders_before_drag: None,
            feed_multi_select: false,
            selected_feeds: std::collections::HashSet::new(),
            feed_sort_mode: false,
            opml_import_overwrite: false,
            plugin_cred_edits: std::collections::HashMap::new(),
        };
        // Local loopback server for cached images (full-res Pixiv originals etc.).
        if let Some(dir) = glean_core::cache_images_dir() {
            if let Some(server) = img_server::LocalImageServer::start(dir) {
                s.img_serve_base = Some(server.base_url);
            }
        }
        // Sync the reader's title bar dark state with the loaded config.
        s.reader.set_titlebar_dark(s.dark);
        // 若配置了 AI，把配置注入 service（供同步 fallback 命令使用）。
        if let Some(ai) = s.config.ai.clone() {
            s.service.set_ai_config(ai);
        }
        // 同步插件启停状态与代理开关（AppConfig → PluginManager）。
        let disabled = s.config.disabled_plugins.clone();
        let proxy = s.config.plugin_proxy.clone();
        if let Err(e) = s.service.reload_plugins(&disabled, &proxy) {
            s.status = format!("插件加载失败: {e}");
        }
        s.dispatch(AppCommand::Bootstrap);
        s.spawn_update_check();
        s
    }

    /// Persist current config to disk.
    pub fn save_config(&self) {
        save_config(&self.config_path, &self.config);
    }

    /// 读取某弹窗已保存的几何 (x, y, w, h)；未记录时返回 None。
    pub fn popup_geom(&self, key: &str) -> Option<[f32; 4]> {
        self.config.popup_geometry.get(key).copied()
    }

    /// 记录某弹窗的几何并立即落盘（弹窗关闭/拖动时调用）。
    pub fn save_popup_geom(&mut self, key: &str, geom: [f32; 4]) {
        self.config.popup_geometry.insert(key.to_string(), geom);
        self.save_config();
    }

    /// Sync runtime state → config struct (call before save).
    pub fn sync_config(&mut self) {
        self.config.dark = self.dark;
        self.config.nav_width = self.nav_width;
        self.config.list_width = self.list_width;
    }

    pub fn dispatch(&mut self, cmd: AppCommand) {
        let events = self.service.handle(cmd);
        for ev in events {
            self.apply_event(ev);
        }
    }

    /// Poll background refresh results (called every frame from update).
    pub fn poll_refresh(&mut self) {
        let rx = match &self.refresh_rx {
            Some(rx) => rx,
            None => return,
        };
        // Drain all available outcomes first to release the borrow.
        let mut outcomes = Vec::new();
        while let Ok(outcome) = rx.try_recv() {
            outcomes.push(outcome);
        }
        let got = outcomes.len();
        for outcome in outcomes {
            self.refresh_pending = self.refresh_pending.saturating_sub(1);
            let events = self.service.apply_refresh_outcome(outcome);
            for ev in events {
                self.apply_event(ev);
            }
        }
        if got > 0 {
            self.dispatch(AppCommand::RefreshNav);
            self.dispatch(AppCommand::ListEntries {
                filter: self.filter,
            });
        }
        if self.refresh_pending == 0 {
            self.refresh_rx = None;
            self.status = "刷新完成".into();
            // Trigger favicon downloads for feeds with favicon_url.
            self.maybe_download_favicons();
        } else if got > 0 {
            self.status = format!("刷新中… 剩余 {} 个源", self.refresh_pending);
        }
    }

    /// Called every frame with delta time. Triggers auto-refresh if interval > 0.
    pub fn tick_auto_refresh(&mut self, dt: f32) {
        let interval = self.config.refresh_interval_secs;
        if interval <= 0 || self.refresh_rx.is_some() {
            return;
        }
        self.auto_refresh_timer += dt;
        if self.auto_refresh_timer >= interval as f32 {
            self.auto_refresh_timer = 0.0;
            let tasks = match self
                .service
                .prepare_auto_refresh_tasks(self.config.refresh_interval_secs)
            {
                Ok(t) => t,
                Err(_) => return,
            };
            if tasks.is_empty() {
                return;
            }
            let (tx, rx) = mpsc::channel::<RefreshOutcome>();
            self.refresh_rx = Some(rx);
            self.refresh_pending = tasks.len();
            self.refresh_cancel = Arc::new(AtomicBool::new(false));
            let workers = REFRESH_WORKERS.min(tasks.len());
            self.status = format!("自动刷新中… {} 个源（{} 并发）", tasks.len(), workers);
            let ctx = self.service.refresh_ctx();
            spawn_refresh_workers(tasks, ctx, tx, self.refresh_cancel.clone());
        }
    }

    /// Spawn a background thread to fetch and compare appcast.json.
    fn spawn_update_check(&mut self) {
        let (tx, rx) = mpsc::channel::<UpdateCheckResult>();
        self.update_rx = Some(rx);
        thread::spawn(move || {
            let result = check_for_update(APPCAST_URL);
            let _ = tx.send(result);
        });
    }

    /// Poll the update-check thread (called every frame from update).
    pub fn poll_update_check(&mut self) {
        let rx = match &self.update_rx {
            Some(rx) => rx,
            None => return,
        };
        if let Ok(result) = rx.try_recv() {
            match &result {
                UpdateCheckResult::Available { current, cast } => {
                    self.status = format!("发现新版本 {}（当前 {}）", cast.version, current);
                    self.update_available = Some(result);
                }
                UpdateCheckResult::UpToDate { current, remote } => {
                    eprintln!("glean: up to date (current={current}, remote={remote})");
                }
                UpdateCheckResult::Error(e) => {
                    eprintln!("glean: update check failed: {e}");
                }
            }
            self.update_rx = None;
        }
    }

    /// Hide the main window to the system tray.
    pub fn hide_to_tray(&mut self, ctx: &egui::Context) {
        if !self.tray.is_active() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.status = "已最小化到托盘".into();
        // Note: no repaint ticker needed. Tray callbacks directly call
        // Win32 ShowWindow(SW_RESTORE) to wake the event loop, because
        // ctx.request_repaint() is a no-op for hidden windows
        // (RedrawWindow(RDW_INTERNALPAINT) is ignored for invisible windows).
    }

    /// Restore the main window from the tray.
    pub fn show_from_tray(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Critical,
        ));
    }

    fn apply_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Ready => {}
            AppEvent::NavUpdated {
                folders,
                feeds,
                unread_total,
                unread_per_feed,
            } => {
                self.folders = folders;
                self.feeds = feeds;
                self.unread_total = unread_total;
                self.unread_per_feed = unread_per_feed;
            }
            AppEvent::EntriesUpdated { entries } => {
                write_debug_log(&format!("[ui-entries-updated] count={}", entries.len()));
                self.entries = entries;
                if let Some(i) = self.selected {
                    if i >= self.entries.len() {
                        self.selected = None;
                    }
                }
            }
            AppEvent::EntryOpened { entry } => {
                let same = self
                    .open_detail
                    .as_ref()
                    .map(|d| d.summary.id == entry.summary.id)
                    .unwrap_or(false);
                if !same {
                    self.open_count += 1;
                    // New article: clear the per-article image override.
                    self.reader_show_images = false;
                }
                let html = render_entry(
                    &entry,
                    self.dark,
                    self.effective_image_policy(),
                    self.config.font_size_px,
                    self.config.line_width_rem,
                );
                write_debug_log(&format!(
                    "[entry-render] id={} enhancements={} html_len={} has_ai_block={} same={}",
                    entry.summary.id.0,
                    entry.enhancements.len(),
                    html.len(),
                    html.contains("ai-enhancement"),
                    same,
                ));
                self.reader.show_html(&html);
                self.open_detail = Some(entry);
                self.refresh_status();
                // Maybe trigger background full-text extraction.
                self.maybe_auto_extract();
                // Maybe trigger background image caching.
                self.maybe_cache_images();
            }
            AppEvent::UnreadChanged { total } => {
                self.unread_total = total;
                self.unread_per_feed = self.service.unread_counts_per_feed();
                self.refresh_status();
            }
            AppEvent::Status { message } => {
                self.status = message;
            }
            AppEvent::Error { message } => {
                self.status = format!("错误: {message}");
                self.errors.push(message);
                // Keep only last 50 errors.
                if self.errors.len() > 50 {
                    self.errors.drain(..self.errors.len() - 50);
                }
            }
            AppEvent::OpmlExported { xml } => {
                self.opml_export = Some(xml);
            }
            AppEvent::EntryExtracted { id, success } => {
                // The poll_extract / dispatch paths already re-open the entry
                // to refresh the reader; nothing to project here beyond status.
                if success {
                    self.status = format!("已抽取全文 (entry {})", id.0);
                }
            }
            AppEvent::EntryEnhanced { id, kind, success } => {
                // 成功时更新状态栏；失败时由并发的 Status 事件报错。
                // 增强结果面板的刷新在 poll_enhance 中处理（重新拉取 enhancements）。
                if success {
                    self.status = format!("AI {} 完成 (entry {})", kind, id.0);
                }
            }
        }
    }

    pub fn select_index(&mut self, index: usize) {
        self.select_index_with(index, false);
    }

    /// `force`: reload reader even if the same entry is already open.
    pub fn select_index_with(&mut self, index: usize, force: bool) {
        if index >= self.entries.len() {
            return;
        }
        let id = self.entries[index].id;
        let already = self
            .open_detail
            .as_ref()
            .map(|d| d.summary.id == id)
            .unwrap_or(false);
        self.selected = Some(index);
        if already && !force {
            self.refresh_status();
            return;
        }
        self.dispatch(AppCommand::OpenEntry { id });
    }

    fn refresh_status(&mut self) {
        let unread = self.unread_total;
        if let Some(e) = &self.open_detail {
            let read_label = if e.summary.is_read {
                "已读"
            } else {
                "未读"
            };
            self.status = format!(
                "「{}」{} · 未读 {} 篇 · 换文计数 {}",
                e.summary.title, read_label, unread, self.open_count
            );
        } else {
            self.status = format!("未读 {} 篇 · 未打开文章", unread);
        }
    }

    pub fn next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = self
            .selected
            .map(|i| (i + 1) % self.entries.len())
            .unwrap_or(0);
        self.select_index(i);
    }

    pub fn prev(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len();
        let i = self.selected.map(|i| (i + n - 1) % n).unwrap_or(0);
        self.select_index(i);
    }

    pub fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.dark = !self.dark;
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if self.dark {
            egui::viewport::SystemTheme::Dark
        } else {
            egui::viewport::SystemTheme::Light
        }));
        self.reader.set_titlebar_dark(self.dark);
        // Reload the reader document with the new theme baked in. With JS
        // disabled (§7.2) there is no live theme-flip; show_html reloads the
        // themed HTML via load_html (NavigateToString — works with JS off).
        if let Some(entry) = self.open_detail.clone() {
            let html = render_entry(
                &entry,
                self.dark,
                self.effective_image_policy(),
                self.config.font_size_px,
                self.config.line_width_rem,
            );
            self.reader.show_html(&html);
            // 主题切换后正文恢复为原始 URL，需重新触发缓存重写（防盗链图片）。
            self.maybe_cache_images();
        } else {
            // No article open: reload the themed placeholder so the empty
            // reader area follows the theme switch.
            self.reader.show_placeholder(self.dark);
        }
        self.reader.apply_theme(self.dark);
        self.sync_config();
        self.save_config();
    }

    pub fn toggle_host_mode(&mut self) {
        self.host_mode = self.host_mode.toggle();
        self.reader.set_mode(self.host_mode);
        self.status = format!("Host mode -> {}", self.host_mode.label());
    }

    pub fn set_filter(&mut self, filter: EntryFilter) {
        self.filter = filter;
        self.selected = None;
        self.search.clear();
        self.dispatch(AppCommand::ListEntries { filter });
    }

    pub fn add_feed_from_url(&mut self) {
        let url = self.feed_url_input.trim().to_string();
        if url.is_empty() {
            self.status = "请输入 RSS/Atom URL".into();
            return;
        }
        self.status = format!("正在抓取 {url} …");
        self.dispatch(AppCommand::AddFeedFromUrl {
            feed_url: url.clone(),
        });
        if self.status.starts_with("错误") {
            return;
        }
        self.feed_url_input.clear();
        // 把订阅栏选择的分类 / 文件夹（含新建文件夹）应用到刚添加的订阅上。
        self.apply_add_options(&url);
    }

    /// 把订阅栏选择的分类 / 文件夹（含新建文件夹）应用到刚添加的订阅上。
    fn apply_add_options(&mut self, url: &str) {
        let Some(feed) = self.feeds.iter().find(|f| f.feed_url == url).cloned() else {
            // URL 规范化后与输入不一致（如 GitHub releases、YouTube），无法
            // 定位到新源，此时跳过个性化设置，保持默认分类 / 无文件夹。
            return;
        };
        if let Some(cat) = self.feed_add_category {
            self.dispatch(AppCommand::SetFeedCategory {
                id: feed.id,
                category: cat,
            });
        }
        let new_name = self.feed_add_new_folder.trim().to_string();
        let folder = if !new_name.is_empty() {
            // 复用同名文件夹，否则新建。
            if let Some(f) = self.folders.iter().find(|f| f.name == new_name) {
                Some(f.id)
            } else {
                self.dispatch(AppCommand::CreateFolder {
                    name: new_name.clone(),
                });
                self.folders
                    .iter()
                    .find(|f| f.name == new_name)
                    .map(|f| f.id)
            }
        } else {
            self.feed_add_folder
        };
        if let Some(fid) = folder {
            self.move_feed_to_folder(feed.id, Some(fid));
        }
        self.feed_add_new_folder.clear();
    }

    /// Launch background refresh: HTTP on threads, DB writes on UI thread.
    pub fn refresh_all_feeds_async(&mut self) {
        if self.refresh_rx.is_some() {
            self.status = "刷新进行中…".into();
            return;
        }
        let tasks: Vec<RefreshTask> = match self.service.prepare_refresh_tasks(None) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("刷新失败: {e}");
                return;
            }
        };
        if tasks.is_empty() {
            self.status = "没有可刷新的订阅".into();
            return;
        }
        let (tx, rx) = mpsc::channel::<RefreshOutcome>();
        self.refresh_rx = Some(rx);
        self.refresh_pending = tasks.len();
        self.refresh_cancel = Arc::new(AtomicBool::new(false));
        let workers = REFRESH_WORKERS.min(tasks.len());
        self.status = format!("刷新中… {} 个源（{} 并发）", tasks.len(), workers);
        let ctx = self.service.refresh_ctx();
        spawn_refresh_workers(tasks, ctx, tx, self.refresh_cancel.clone());
    }

    /// 停止正在进行的刷新：置取消标志（worker 在下一订阅前退出），丢弃
    /// 尚未收到的结果。刷新在后台线程运行，不影响 UI 其他操作。
    pub fn stop_refresh(&mut self) {
        if self.refresh_rx.is_none() {
            return;
        }
        self.refresh_cancel.store(true, Ordering::Relaxed);
        self.refresh_rx = None;
        self.refresh_pending = 0;
        self.status = "刷新已停止".into();
    }

    pub fn delete_feed(&mut self, id: glean_core::FeedId) {
        self.dispatch(AppCommand::DeleteFeed { id });
    }

    /// 批量删除多选的订阅，并清空选择。
    pub fn batch_delete_feeds(&mut self, ids: Vec<glean_core::FeedId>) {
        let n = ids.len();
        for id in ids {
            self.dispatch(AppCommand::DeleteFeed { id });
        }
        self.selected_feeds.clear();
        self.status = format!("已删除 {n} 个订阅");
    }

    /// 批量移动多选的订阅到指定文件夹（None = 移出文件夹）。
    pub fn batch_move_feeds(&mut self, ids: Vec<glean_core::FeedId>, folder_id: Option<FolderId>) {
        for id in ids {
            self.dispatch(AppCommand::MoveFeedToFolder {
                feed_id: id,
                folder_id,
            });
        }
        self.selected_feeds.clear();
        self.status = "已批量移动订阅".into();
    }

    /// 整理模式下调整订阅顺序：把 `feed_id` 移到同组内 `before_id` 之前
    /// （`before_id = None` 表示移到组末尾）。
    pub fn reorder_feed(
        &mut self,
        feed_id: glean_core::FeedId,
        before_id: Option<glean_core::FeedId>,
    ) {
        self.dispatch(AppCommand::ReorderFeed { feed_id, before_id });
    }

    /// 批量设置多选订阅的代理开关，并清空选择。
    pub fn batch_set_feed_proxy(&mut self, ids: Vec<glean_core::FeedId>, use_proxy: bool) {
        let n = ids.len();
        for id in ids {
            self.dispatch(AppCommand::SetFeedProxy { id, use_proxy });
        }
        self.selected_feeds.clear();
        self.status = format!(
            "已批量{} {n} 个订阅的代理",
            if use_proxy { "开启" } else { "关闭" }
        );
    }

    /// 运行时修复数据库（设置页「修复数据库」按钮）。
    /// 返回给 UI 的状态消息。
    pub fn repair_database(&mut self) -> String {
        match self.service.repair_db() {
            Ok(msg) => {
                // 执行了修复/重建时重载数据；仅检查通过时无需重载。
                if msg.contains("修复") || msg.contains("重建") {
                    self.dispatch(AppCommand::Bootstrap);
                }
                msg
            }
            Err(e) => format!("数据库修复失败: {e}"),
        }
    }

    /// 无条件重建数据库（设置页「强制重建」按钮）。
    pub fn force_rebuild_database(&mut self) -> String {
        match self.service.force_rebuild_db() {
            Ok(msg) => {
                self.dispatch(AppCommand::Bootstrap);
                msg
            }
            Err(e) => format!("数据库重建失败: {e}"),
        }
    }

    pub fn toggle_star_current(&mut self) {
        if let Some(e) = &self.open_detail {
            let id = e.summary.id;
            self.dispatch(AppCommand::ToggleStar { id });
            self.dispatch(AppCommand::OpenEntry { id });
        }
    }

    pub fn mark_all_read(&mut self, feed_id: Option<glean_core::FeedId>) {
        self.dispatch(AppCommand::MarkAllRead { feed_id });
    }

    pub fn run_search(&mut self) {
        self.dispatch(AppCommand::SearchEntries {
            query: self.search.clone(),
        });
    }

    pub fn export_opml(&mut self) {
        self.dispatch(AppCommand::ExportOpml);
    }

    pub fn import_opml(&mut self) {
        let content = self.opml_import_input.clone();
        if content.trim().is_empty() {
            self.status = "请粘贴 OPML 内容".into();
            return;
        }
        self.dispatch(AppCommand::ImportOpml {
            content,
            overwrite: self.opml_import_overwrite,
        });
        if !self.status.starts_with("错误") {
            self.opml_import_input.clear();
        }
    }

    pub fn rename_feed(&mut self, id: glean_core::FeedId, title: String) {
        self.dispatch(AppCommand::RenameFeed { id, title });
    }

    pub fn edit_feed_url(&mut self, id: glean_core::FeedId, feed_url: String) {
        self.dispatch(AppCommand::EditFeedUrl { id, feed_url });
    }

    pub fn move_feed_to_folder(
        &mut self,
        feed_id: glean_core::FeedId,
        folder_id: Option<FolderId>,
    ) {
        self.dispatch(AppCommand::MoveFeedToFolder { feed_id, folder_id });
    }

    pub fn create_folder(&mut self, name: String) {
        self.dispatch(AppCommand::CreateFolder { name });
    }

    pub fn toggle_mute_feed(&mut self, id: glean_core::FeedId) {
        self.dispatch(AppCommand::ToggleMuteFeed { id });
    }

    pub fn toggle_image_policy(&mut self) {
        self.config.image_policy = self.config.image_policy.next();
        self.sync_config();
        self.save_config();
        self.status = format!("图片策略: {}", self.config.image_policy.label());
    }

    /// Policy actually used for rendering: per-article override beats config.
    pub fn effective_image_policy(&self) -> ImagePolicy {
        if self.reader_show_images {
            ImagePolicy::Allow
        } else {
            self.config.image_policy
        }
    }

    /// Per-article "显示图片": re-render the current entry with Allow policy.
    pub fn show_reader_images(&mut self) {
        if self.open_detail.is_none() {
            return;
        }
        self.reader_show_images = true;
        if let Some(entry) = self.open_detail.clone() {
            let html = render_entry(
                &entry,
                self.dark,
                ImagePolicy::Allow,
                self.config.font_size_px,
                self.config.line_width_rem,
            );
            self.reader.show_html(&html);
        }
        self.status = "已显示当前文章图片".into();
        // 触发后台图片缓存：i.pximg.net 等防盗链域名需要后端带 Referer 代理下载，
        // 否则 WebView 直接加载会 403。OpenEntry 时若 policy 非 Allow 会跳过缓存，
        // 这里是 LoadOnDemand 模式下唯一触发缓存的入口。
        self.maybe_cache_images();
    }

    /// Maybe spawn a background full-text extraction for the currently open
    /// entry, if it has a short summary and an article URL. No-op if auto-
    /// extract is disabled or an extraction is already in flight.
    pub fn maybe_auto_extract(&mut self) {
        if !self.config.auto_extract || self.extract_rx.is_some() {
            return;
        }
        let entry = match &self.open_detail {
            Some(e) => e.summary.id,
            None => return,
        };
        // force=false：自动抽取，已抽取/正文够长/Pixiv 作品页自动跳过。
        let task = match self.service.prepare_extract_task(entry, false) {
            Ok(Some(t)) => t,
            _ => return,
        };
        // 优先走代理 client（与图片缓存一致），未配置代理时回退直连。
        let client = self
            .service
            .http_proxy()
            .map(|c| c.inner.clone())
            .unwrap_or_else(|| self.service.http().inner.clone());
        let (tx, rx) = mpsc::channel::<ExtractOutcome>();
        self.extract_rx = Some(rx);
        self.extract_in_flight = Some(entry);
        self.status = "正在抽取全文…".into();
        thread::spawn(move || {
            let outcome = run_extract_task(&client, &task);
            let _ = tx.send(outcome);
        });
    }

    /// Poll the extraction thread (called every frame from update).
    pub fn poll_extract(&mut self) {
        let rx = match &self.extract_rx {
            Some(rx) => rx,
            None => return,
        };
        if let Ok(outcome) = rx.try_recv() {
            self.extract_rx = None;
            self.extract_in_flight = None;
            let id = match &outcome {
                ExtractOutcome::Extracted { entry_id, .. } => *entry_id,
                ExtractOutcome::Failed { entry_id, .. } => *entry_id,
            };
            match &outcome {
                ExtractOutcome::Extracted { html, .. } => {
                    write_debug_log(&format!(
                        "[extract] 成功 entry={} html_len={}",
                        id.0,
                        html.len()
                    ));
                }
                ExtractOutcome::Failed { error, .. } => {
                    write_debug_log(&format!("[extract] 失败 entry={} error={}", id.0, error));
                }
            }
            let events = self.service.apply_extract_outcome(outcome);
            for ev in events {
                self.apply_event(ev);
            }
            // If the extracted entry is currently open, re-open to refresh reader.
            if let Some(open) = &self.open_detail {
                if open.summary.id == id {
                    self.dispatch(AppCommand::OpenEntry { id });
                }
            }
        }
    }

    /// Manual "抽取全文" button: force=true, 走代理异步。
    pub fn extract_current(&mut self) {
        if self.extract_rx.is_some() {
            self.status = "全文抽取进行中，请稍候…".into();
            return;
        }
        let entry = match &self.open_detail {
            Some(e) => e.summary.id,
            None => return,
        };
        // force=true：允许重抽已抽取内容；Pixiv 作品页仍跳过（插件已提供图文）。
        let task = match self.service.prepare_extract_task(entry, true) {
            Ok(Some(t)) => t,
            Ok(None) => {
                self.status = "无法抽取：无 URL 或为插件已提供正文的来源（如 Pixiv）".into();
                return;
            }
            Err(e) => {
                self.status = format!("抽取准备失败: {e}");
                return;
            }
        };
        let client = self
            .service
            .http_proxy()
            .map(|c| c.inner.clone())
            .unwrap_or_else(|| self.service.http().inner.clone());
        let (tx, rx) = mpsc::channel::<ExtractOutcome>();
        self.extract_rx = Some(rx);
        self.extract_in_flight = Some(entry);
        self.status = "正在抽取全文…".into();
        write_debug_log(&format!(
            "[extract] 手动触发 entry={} url={}",
            entry.0, task.url
        ));
        thread::spawn(move || {
            let outcome = run_extract_task(&client, &task);
            let _ = tx.send(outcome);
        });
    }

    /// 手动触发 AI 增强（摘要/翻译）。异步：prepare → spawn worker → poll。
    /// 需 `AppConfig.ai` 已配置。同一 (entry, kind) 不重复并发。
    pub fn enhance_current(&mut self, action: EnhanceAction) {
        // 未配置 AI → 静默返回（UI 按钮应已根据 config.ai 禁用）。
        if self.config.ai.is_none() {
            return;
        }
        // 已有增强任务在跑 → 明确提示，不再静默跳过（否则用户以为点了没反应）。
        if self.enhance_rx.is_some() {
            self.status = "AI 任务进行中，请稍候…".into();
            return;
        }
        let entry = match &self.open_detail {
            Some(e) => e.clone(),
            None => return,
        };
        let kind = action.kind_str().to_string();
        let id = entry.summary.id;
        // 同一 kind 已在跑 → 跳过。
        if let Some((inflight_id, inflight_kind)) = &self.enhance_in_flight {
            if *inflight_id == id && inflight_kind == &kind {
                return;
            }
        }
        let cfg = match self.config.ai.clone() {
            Some(c) => c,
            None => return,
        };
        let task = match self.service.prepare_enhance_task(id, action) {
            Ok(Some(t)) => t,
            Ok(None) => {
                self.status = "无内容可增强".into();
                write_debug_log(&format!(
                    "[ai-enhance] entry={} kind={} 无正文可增强（extracted/content 均为空）",
                    id.0, kind
                ));
                return;
            }
            Err(e) => {
                self.status = format!("AI 准备失败: {e}");
                write_debug_log(&format!(
                    "[ai-enhance] entry={} kind={} prepare 失败: {e}",
                    id.0, kind
                ));
                return;
            }
        };
        write_debug_log(&format!(
            "[ai-enhance] 触发 kind={} entry={} title_len={} content_len={} base_url={} model={}",
            kind,
            id.0,
            task.title.len(),
            task.content.len(),
            cfg.base_url,
            cfg.model,
        ));
        let (tx, rx) = mpsc::channel::<EnhanceOutcome>();
        self.enhance_rx = Some(rx);
        self.enhance_in_flight = Some((id, kind.clone()));
        self.status = format!("AI {} 进行中…", kind);
        thread::spawn(move || {
            // 与 extract 一致：独立 blocking client，30s 超时。
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .expect("enhance client");
            let outcome = run_enhance_task(&client, &cfg, &task);
            let _ = tx.send(outcome);
        });
    }

    /// Poll the AI enhance thread (called every frame from update).
    pub fn poll_enhance(&mut self) {
        let rx = match &self.enhance_rx {
            Some(rx) => rx,
            None => return,
        };
        if let Ok(outcome) = rx.try_recv() {
            self.enhance_rx = None;
            self.enhance_in_flight = None;
            let id = match &outcome {
                EnhanceOutcome::Success { entry_id, .. } => *entry_id,
                EnhanceOutcome::Failed { entry_id, .. } => *entry_id,
            };
            match &outcome {
                EnhanceOutcome::Success { kind, result, .. } => {
                    write_debug_log(&format!(
                        "[ai-enhance] 收到成功 kind={} entry={} result_len={} 前120字: {}",
                        kind,
                        id.0,
                        result.len(),
                        &result.chars().take(120).collect::<String>()
                    ));
                }
                EnhanceOutcome::Failed { kind, error, .. } => {
                    write_debug_log(&format!(
                        "[ai-enhance] 收到失败 kind={} entry={} error={}",
                        kind, id.0, error
                    ));
                }
            }
            let events = self.service.apply_enhance_outcome(outcome);
            for ev in events {
                self.apply_event(ev);
            }
            // 刷新打开的 entry：re-open 会重新拉取 enhancements 列表。
            if let Some(open) = &self.open_detail {
                if open.summary.id == id {
                    write_debug_log(&format!(
                        "[ai-enhance] entry={} 正在 re-open 刷新阅读区",
                        id.0
                    ));
                    self.dispatch(AppCommand::OpenEntry { id });
                } else {
                    write_debug_log(&format!(
                        "[ai-enhance] entry={} 已不在阅读区（当前={}），跳过 re-open",
                        id.0, open.summary.id.0
                    ));
                }
            }
        }
    }

    /// Maybe spawn background image caching for the current entry.
    /// Only when cache_images is on and images are being shown (Allow or
    /// LoadOnDemand+override) and no caching task is already in flight.
    pub fn maybe_cache_images(&mut self) {
        if self.img_cache_rx.is_some() {
            return;
        }
        let entry = match &self.open_detail {
            Some(e) => e,
            None => return,
        };
        let showing_images = self.effective_image_policy() == ImagePolicy::Allow;
        if !showing_images {
            return;
        }
        let body = if !entry.extracted_html.is_empty() {
            entry.extracted_html.clone()
        } else {
            entry.content_html.clone()
        };
        if body.is_empty() {
            return;
        }
        // cache_images=false 时，如果 HTML 含防盗链域名（i.pximg.net 等），
        // 仍然触发缓存——这类图片 WebView 直接加载会 403，必须后端代理
        // （带 Referer）下载。其他情况尊重 cache_images 开关。
        let has_hotlink = body.contains("pximg.net/");
        let should_cache = self.config.cache_images || has_hotlink;
        if !should_cache {
            return;
        }
        let dark = self.dark;
        let policy = self.effective_image_policy();
        let font_size_px = self.config.font_size_px;
        let line_width_rem = self.config.line_width_rem;
        // 用 service 的 HTTP client（含代理配置），避免图片下载不走代理。
        // i.pximg.net 等防盗链域名在国内常需代理才能访问；新建无代理 client
        // 会导致 cache_images=true 时图片下载仍然失败。
        let client = self
            .service
            .http_proxy()
            .map(|c| c.inner.clone())
            .unwrap_or_else(|| self.service.http().inner.clone());
        let serve_base = self.img_serve_base.clone();
        let (tx, rx) = mpsc::channel::<(String, bool, ImagePolicy)>();
        self.img_cache_rx = Some(rx);
        thread::spawn(move || {
            let img_dir = glean_core::cache_images_dir();
            let cache = glean_core::ImageCache::new(img_dir).with_serve_base(serve_base);
            let (rewritten, _fetched) = cache.cache_images_in_html(&body, &client);
            // Re-render the full document with the rewritten body.
            let html = glean_core::reader_document(
                "", // title not needed; we only care about the body rewrite
                None,
                None,
                &rewritten,
                dark,
                true,
                policy,
                font_size_px,
                line_width_rem,
            );
            let _ = tx.send((html, dark, policy));
        });
    }

    /// Poll background image-caching thread.
    pub fn poll_img_cache(&mut self) {
        let rx = match &self.img_cache_rx {
            Some(rx) => rx,
            None => return,
        };
        if let Ok((_html, _dark, _policy)) = rx.try_recv() {
            self.img_cache_rx = None;
            // Reload reader with the rewritten HTML (images now point to local cache).
            // We need to re-render the entry with the cached images.
            // Since we don't have the rewritten HTML stored, re-trigger the cache
            // flow by re-rendering — but the images are already on disk, so
            // cache_images_in_html will just rewrite src without downloading.
            if let Some(entry) = &self.open_detail {
                let body = if !entry.extracted_html.is_empty() {
                    entry.extracted_html.clone()
                } else {
                    entry.content_html.clone()
                };
                let dark = self.dark;
                let policy = self.effective_image_policy();
                // Synchronous rewrite (images already cached locally → no network).
                let img_dir = glean_core::cache_images_dir();
                let cache = glean_core::ImageCache::new(img_dir)
                    .with_serve_base(self.img_serve_base.clone());
                // 用带代理的 client：后台下载失败时这里会尝试重下，需要代理才能
                // 访问 i.pximg.net。已缓存的图片不会重新下载，不影响性能。
                let client = self
                    .service
                    .http_proxy()
                    .map(|c| c.inner.clone())
                    .unwrap_or_else(|| self.service.http().inner.clone());
                let (rewritten, _) = cache.cache_images_in_html(&body, &client);
                // 关键：图片缓存重渲染也必须带上 AI 增强区块，否则会覆盖
                // 刚显示在正文顶部的摘要/翻译结果（本 bug 曾导致「点了没反应」）。
                let body_with_enhancements = with_enhancements(&rewritten, &entry.enhancements);
                let html = render_entry_body(
                    &entry.summary.title,
                    entry.summary.url.as_deref(),
                    entry.author.as_deref(),
                    &body_with_enhancements,
                    dark,
                    true,
                    policy,
                    self.config.font_size_px,
                    self.config.line_width_rem,
                );
                self.reader.show_html(&html);
            }
        }
    }

    /// Spawn background favicon download for feeds that have a favicon_url
    /// but no cached icon yet. Called after feed refresh.
    pub fn maybe_download_favicons(&mut self) {
        let favicon_dir = glean_core::cache_favicons_dir();
        let cache = FaviconCache::new(favicon_dir);
        if !cache.enabled() {
            return;
        }
        for feed in &self.feeds {
            let Some(url) = feed.favicon_url.as_deref() else {
                continue;
            };
            if url.is_empty() {
                continue;
            }
            if self.favicon_pending.contains(&feed.id) {
                continue;
            }
            if cache.is_cached(feed.id) {
                continue;
            }
            // Spawn download.
            let fid = feed.id;
            let url = url.to_string();
            let (tx, rx) = mpsc::channel::<(FeedId, Vec<u8>, u32, u32)>();
            self.favicon_rx = Some(rx);
            self.favicon_pending.insert(fid);
            thread::spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .redirect(reqwest::redirect::Policy::limited(3))
                    .build()
                    .expect("favicon client");
                let favicon_dir = glean_core::cache_favicons_dir();
                let fc = FaviconCache::new(favicon_dir);
                // Download to disk.
                if let Err(e) = fc.download(fid, &url, &client) {
                    eprintln!("glean: favicon download failed for {url}: {e}");
                    let _ = tx.send((fid, Vec::new(), 0, 0));
                    return;
                }
                // Read back and decode to RGBA.
                if let Some(bytes) = fc.read(fid) {
                    match image::load_from_memory(&bytes) {
                        Ok(img) => {
                            let rgba = img.to_rgba8();
                            let (w, h) = rgba.dimensions();
                            let _ = tx.send((fid, rgba.into_raw(), w, h));
                        }
                        Err(e) => {
                            eprintln!("glean: favicon decode failed: {e}");
                            let _ = tx.send((fid, Vec::new(), 0, 0));
                        }
                    }
                } else {
                    let _ = tx.send((fid, Vec::new(), 0, 0));
                }
            });
        }
    }

    /// Poll background favicon download thread. Returns decoded favicon data
    /// (feed_id, rgba_pixels, width, height) for the UI to create textures.
    pub fn poll_favicon_cache(&mut self) -> Option<(FeedId, Vec<u8>, u32, u32)> {
        let rx = match &self.favicon_rx {
            Some(rx) => rx,
            None => return None,
        };
        match rx.try_recv() {
            Ok(result) => {
                self.favicon_pending.remove(&result.0);
                // If this was the last pending download, clear the receiver.
                if result.2 == 0 {
                    return None; // decode failed, skip
                }
                Some(result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.favicon_rx = None;
                None
            }
        }
    }

    /// Try to load cached favicons from disk into RGBA data at startup.
    /// Returns (feed_id, rgba_pixels, width, height) pairs.
    pub fn load_cached_favicons(&self) -> Vec<(FeedId, Vec<u8>, u32, u32)> {
        let favicon_dir = glean_core::cache_favicons_dir();
        let cache = FaviconCache::new(favicon_dir);
        if !cache.enabled() {
            return Vec::new();
        }
        let mut result = Vec::new();
        for feed in &self.feeds {
            if let Some(bytes) = cache.read(feed.id) {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    result.push((feed.id, rgba.into_raw(), w, h));
                }
            }
        }
        result
    }

    /// Spawn background thumbnail downloads for entries that have a thumbnail
    /// URL but no texture yet. Pixiv thumbnails on i.pximg.net need a Referer
    /// header and the configured proxy; we reuse the service's HTTP client.
    pub fn maybe_download_thumbnails(&mut self) {
        if self.thumbnail_rx.is_some() {
            return;
        }
        let client = self
            .service
            .http_proxy()
            .map(|c| c.inner.clone())
            .unwrap_or_else(|| self.service.http().inner.clone());
        let mut tasks = Vec::new();
        for e in &self.entries {
            let Some(url) = e.thumbnail_url.as_deref() else {
                continue;
            };
            if url.is_empty()
                || self.thumbnail_pending.contains(&e.id)
                || self.thumbnail_failed.contains(&e.id)
                || self.thumbnail_loaded.contains(&e.id)
            {
                continue;
            }
            if tasks.len() >= 8 {
                break;
            }
            tasks.push((e.id, url.to_string()));
        }
        if tasks.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel::<(EntryId, Vec<u8>, u32, u32)>();
        self.thumbnail_rx = Some(rx);
        for (eid, url) in tasks {
            self.thumbnail_pending.insert(eid);
            let tx = tx.clone();
            let client = client.clone();
            thread::spawn(move || {
                let mut req = client.get(&url);
                if url.contains("pximg.net") {
                    req = req.header(reqwest::header::REFERER, "https://www.pixiv.net/");
                }
                let sent = match req.send().and_then(|r| r.error_for_status()) {
                    Ok(resp) => match resp.bytes() {
                        Ok(bytes) => match image::load_from_memory(&bytes) {
                            Ok(img) => {
                                let rgba = img.to_rgba8();
                                let (w, h) = rgba.dimensions();
                                tx.send((eid, rgba.into_raw(), w, h)).is_ok()
                            }
                            Err(_) => tx.send((eid, Vec::new(), 0, 0)).is_ok(),
                        },
                        Err(_) => tx.send((eid, Vec::new(), 0, 0)).is_ok(),
                    },
                    Err(_) => tx.send((eid, Vec::new(), 0, 0)).is_ok(),
                };
                let _ = sent;
            });
        }
    }

    /// Poll background thumbnail downloads. Returns all completed results.
    pub fn poll_thumbnail_cache(&mut self) -> Vec<(EntryId, Vec<u8>, u32, u32)> {
        let rx = match &self.thumbnail_rx {
            Some(rx) => rx,
            None => return Vec::new(),
        };
        let mut results = Vec::new();
        while let Ok(r) = rx.try_recv() {
            self.thumbnail_pending.remove(&r.0);
            if r.2 == 0 || r.3 == 0 {
                self.thumbnail_failed.insert(r.0);
            } else {
                self.thumbnail_loaded.insert(r.0);
            }
            results.push(r);
        }
        if self.thumbnail_pending.is_empty() {
            self.thumbnail_rx = None;
        }
        results
    }

    /// Entry currently being extracted (for UI button gating).
    pub fn extract_in_flight(&self) -> Option<EntryId> {
        self.extract_in_flight
    }

    /// (entry_id, kind) currently being AI-enhanced (for UI button gating).
    pub fn enhance_in_flight(&self) -> Option<&(EntryId, String)> {
        self.enhance_in_flight.as_ref()
    }

    /// AI 配置是否已就绪（UI 按钮启用/禁用判断）。
    pub fn ai_configured(&self) -> bool {
        self.config.ai.is_some()
    }

    /// 从设置缓冲保存 AI 配置：api_key 加密后入 `config.ai` + 注入 service。
    /// key 输入为空且已有配置时保留原密文（便于只改 URL/模型）。
    pub fn save_ai_config(&mut self) {
        let base_url = self.ai_base_url_input.trim().to_string();
        if base_url.is_empty() {
            self.status = "AI 配置未保存：Base URL 不能为空".into();
            return;
        }
        let model = self.ai_model_input.trim().to_string();
        if model.is_empty() {
            self.status = "AI 配置未保存：模型不能为空".into();
            return;
        }
        let api_key_cipher = if !self.ai_key_input.is_empty() {
            match glean_core::plugin::credential::encrypt_secret(&self.ai_key_input) {
                Ok(c) => c,
                Err(e) => {
                    self.status = format!("AI api_key 加密失败: {e}");
                    return;
                }
            }
        } else {
            self.config
                .ai
                .as_ref()
                .map(|a| a.api_key_cipher.clone())
                .unwrap_or_default()
        };
        let cfg = glean_core::AiConfig {
            base_url,
            model,
            api_key_cipher,
        };
        self.config.ai = Some(cfg.clone());
        self.service.set_ai_config(cfg);
        self.save_config();
        self.status = "AI 配置已保存".into();
    }

    /// 清除 AI 配置（config + service + 输入缓冲）。
    pub fn clear_ai_config(&mut self) {
        self.config.ai = None;
        self.service.clear_ai_config();
        self.ai_base_url_input.clear();
        self.ai_model_input.clear();
        self.ai_key_input.clear();
        self.save_config();
        self.status = "AI 配置已清除".into();
    }

    /// 启用/停用插件（「插件管理」界面开关）。变化写回 `config.disabled_plugins`。
    pub fn toggle_plugin(&mut self, id: &str, enabled: bool) {
        match self.service.set_plugin_enabled(id, enabled) {
            Ok(()) => {
                self.config.disabled_plugins = self.service.disabled_plugins();
                self.save_config();
                if enabled {
                    self.status = format!("插件已启用: {id}");
                } else {
                    self.status = format!("插件已停用: {id}");
                }
            }
            Err(e) => self.status = format!("插件操作失败: {e}"),
        }
    }

    /// 设置插件级「使用代理」开关（§11.5.10）。变化写回
    /// `config.plugin_proxy`。插件请求（含添加订阅时）会走设置页配置的代理。
    pub fn set_plugin_proxy(&mut self, id: &str, use_proxy: bool) {
        match self.service.set_plugin_proxy(id, use_proxy) {
            Ok(()) => {
                self.config.plugin_proxy = self.service.proxy_plugins();
                self.save_config();
                if use_proxy {
                    self.status = format!("插件已开启使用代理: {id}");
                } else {
                    self.status = format!("插件已关闭代理: {id}");
                }
            }
            Err(e) => self.status = format!("插件操作失败: {e}"),
        }
    }

    /// 安装插件（文件夹导入）。
    pub fn install_plugin_from_dir(&mut self, src: &std::path::Path) {
        match self.service.install_plugin_dir(src) {
            Ok(id) => self.status = format!("插件已安装: {id}"),
            Err(e) => self.status = format!("安装失败: {e}"),
        }
    }

    /// 安装插件（zip 导入）。
    pub fn install_plugin_from_zip(&mut self, zip_path: &std::path::Path) {
        match self.service.install_plugin_zip(zip_path) {
            Ok(id) => self.status = format!("插件已安装: {id}"),
            Err(e) => self.status = format!("安装失败: {e}"),
        }
    }

    /// 卸载插件。变化写回 `config.disabled_plugins`。
    pub fn uninstall_plugin(&mut self, id: &str) {
        match self.service.uninstall_plugin(id) {
            Ok(()) => {
                self.config.disabled_plugins = self.service.disabled_plugins();
                self.save_config();
                self.status = format!("插件已卸载: {id}");
            }
            Err(e) => self.status = format!("卸载失败: {e}"),
        }
    }

    /// 保存插件凭证槽（§11.5.9 UI 入口）。落盘加密（Windows DPAPI）。
    /// Header 名可留空（body 占位符注入场景，如 Pixiv refresh_token）；
    /// 凭证值必填。
    pub fn save_plugin_credential(&mut self, plugin_id: &str, slot: &str) {
        let key = format!("{plugin_id}:{slot}");
        let (name, value) = self
            .plugin_cred_edits
            .get(&key)
            .cloned()
            .unwrap_or_default();
        if value.trim().is_empty() {
            self.status = format!("凭证 {plugin_id}:{slot} 未保存：凭证值为空");
            return;
        }
        match self
            .service
            .set_credential(plugin_id, slot, name.trim(), value.trim())
        {
            Ok(()) => self.status = format!("凭证已保存: {plugin_id}:{slot}"),
            Err(e) => self.status = format!("凭证保存失败: {e}"),
        }
    }

    /// 清除插件凭证槽。
    pub fn remove_plugin_credential(&mut self, plugin_id: &str, slot: &str) {
        match self.service.remove_credential(plugin_id, slot) {
            Ok(()) => {
                self.status = format!("凭证已清除: {plugin_id}:{slot}");
                self.plugin_cred_edits
                    .remove(&format!("{plugin_id}:{slot}"));
            }
            Err(e) => self.status = format!("凭证清除失败: {e}"),
        }
    }

    pub fn reset_layout(&mut self) {
        self.nav_width = 200.0;
        self.list_width = 320.0;
        self.sync_config();
        self.save_config();
        self.status = "布局已重置".into();
    }

    pub fn set_feed_refresh_interval(&mut self, id: glean_core::FeedId, secs: i64) {
        self.dispatch(AppCommand::SetFeedRefreshInterval { id, secs });
    }

    pub fn set_global_refresh_interval(&mut self, secs: i64) {
        self.config.refresh_interval_secs = secs;
        self.refresh_interval_input = secs.to_string();
        self.auto_refresh_timer = 0.0;
        self.sync_config();
        self.save_config();
        self.status = if secs > 0 {
            format!("自动刷新间隔: {}秒", secs)
        } else {
            "自动刷新已关闭".into()
        };
    }
}

// --- Config load / save helpers ---

/// 把 AI 增强结果（摘要/翻译）区块放到正文 HTML 上方。正文可能很长，
/// 追加在末尾用户滚动不到；转义文本避免被当成 HTML。
fn with_enhancements(body: &str, enhancements: &[(String, String)]) -> String {
    if enhancements.is_empty() {
        return body.to_string();
    }
    let mut html = String::new();
    for (kind, content) in enhancements {
        let label = match kind.as_str() {
            "summary" => "AI 摘要",
            "translate" => "AI 翻译",
            _ => "AI 增强",
        };
        let escaped = escape_html_text(content);
        // 换行转 <br>，保留段落感。
        let with_br = escaped.replace('\n', "<br>");
        html.push_str(&format!(
            r#"<div class="ai-enhancement"><div class="ai-label">{label}</div><div class="ai-content">{with_br}</div></div>"#
        ));
    }
    html.push_str(body);
    html
}

/// Render an entry to reader HTML. Prefers `extracted_html` (full-text from
/// readability) over `content_html` (feed-provided body) when non-empty.
fn render_entry(
    entry: &EntryDetail,
    dark: bool,
    image_policy: ImagePolicy,
    font_size_px: u16,
    line_width_rem: u16,
) -> String {
    let body = if !entry.extracted_html.is_empty() {
        &entry.extracted_html
    } else {
        &entry.content_html
    };
    let body_with_enhancements = with_enhancements(body, &entry.enhancements);
    let has_content = !body.is_empty();
    render_entry_body(
        &entry.summary.title,
        entry.summary.url.as_deref(),
        entry.author.as_deref(),
        &body_with_enhancements,
        dark,
        has_content,
        image_policy,
        font_size_px,
        line_width_rem,
    )
}

/// 转义 HTML 特殊字符（`&` `<` `>` `"`），用于把 AI 纯文本输出安全嵌入 reader HTML。
fn escape_html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render a reader document with explicit body content (used for image-cache
/// rewrite where the body has been modified but metadata comes from the entry).
fn render_entry_body(
    title: &str,
    url: Option<&str>,
    author: Option<&str>,
    body: &str,
    dark: bool,
    has_content: bool,
    image_policy: ImagePolicy,
    font_size_px: u16,
    line_width_rem: u16,
) -> String {
    glean_core::reader_document(
        title,
        url,
        author,
        body,
        dark,
        has_content,
        image_policy,
        font_size_px,
        line_width_rem,
    )
}

fn load_config(path: &std::path::Path) -> AppConfig {
    let result = match std::fs::read_to_string(path) {
        Ok(json) => match serde_json::from_str(&json) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("无法读取配置 {}: {e}", path.display());
                AppConfig::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AppConfig::default(),
        Err(e) => {
            eprintln!("无法读取配置 {}: {e}", path.display());
            AppConfig::default()
        }
    };
    // Clamp thumbnail_size to valid range (defensive: config may be hand-edited).
    let mut result = result;
    result.thumbnail_size = result.thumbnail_size.clamp(
        glean_core::THUMBNAIL_SIZE_MIN,
        glean_core::THUMBNAIL_SIZE_MAX,
    );
    write_debug_log(&format!(
        "[config-load] path={} dark={} accent={:?} nav_width={} list_width={} font_size={} line_width={} thumbnail_size={} window=({:?},{:?},{:?},{:?}) maximized={}",
        path.display(),
        result.dark,
        result.accent,
        result.nav_width,
        result.list_width,
        result.font_size_px,
        result.line_width_rem,
        result.thumbnail_size,
        result.window_x,
        result.window_y,
        result.window_w,
        result.window_h,
        result.window_maximized,
    ));
    result
}

fn save_config(path: &std::path::Path, config: &AppConfig) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("无法创建配置目录 {}: {e}", parent.display());
            return;
        }
    }
    match serde_json::to_string_pretty(config) {
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
                eprintln!("无法写入配置 {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("无法序列化配置 {}: {e}", path.display()),
    }
    write_debug_log(&format!(
        "[config-save] path={} dark={} accent={:?} nav_width={} list_width={} font_size={} line_width={} thumbnail_size={} window=({:?},{:?},{:?},{:?}) maximized={}",
        path.display(),
        config.dark,
        config.accent,
        config.nav_width,
        config.list_width,
        config.font_size_px,
        config.line_width_rem,
        config.thumbnail_size,
        config.window_x,
        config.window_y,
        config.window_w,
        config.window_h,
        config.window_maximized,
    ));
}

pub(crate) fn write_debug_log(message: &str) {
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("glean-debug.log")))
        .unwrap_or_else(|| std::path::PathBuf::from("glean-debug.log"));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{message}");
    }
}
