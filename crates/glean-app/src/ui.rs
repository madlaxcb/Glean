use crate::tray::TrayAction;
use crate::update;
use crate::SpikeState;
use eframe::egui::{self, Color32, Frame, Margin, RichText, Sense, Stroke, Ui, Vec2};
use glean_core::{
    AccentColor, AppCommand, EnhanceAction, EntryFilter, FeedCategory, FolderId, ImagePolicy,
    ReaderHostMode, ACCENT_COLORS, FEED_CATEGORIES, THUMBNAIL_SIZE_MAX, THUMBNAIL_SIZE_MIN,
};

const SPLIT_HIT: f32 = 6.0;
const NAV_MIN: f32 = 120.0;
const NAV_MAX: f32 = 360.0;
const LIST_MIN: f32 = 180.0;
const LIST_MAX: f32 = 520.0;
const READER_MIN: f32 = 240.0;
const FAVICON_SIZE: f32 = 14.0;

/// 订阅行的交互动作（左键点击选中 / 右键菜单项）。
enum FeedRowAction {
    Click(glean_core::FeedId),
    Delete(glean_core::FeedId),
    Rename(glean_core::FeedId),
    EditUrl(glean_core::FeedId),
    MarkRead(glean_core::FeedId),
    MoveFolder(glean_core::FeedId, Option<FolderId>),
    ToggleMute(glean_core::FeedId),
    SetCategory(glean_core::FeedId, FeedCategory),
    ToggleProxy(glean_core::FeedId),
}

pub struct SpikeApp {
    state: SpikeState,
    primed: bool,
    /// Show OPML import text area.
    show_opml_import: bool,
    /// Show error log popup.
    show_errors: bool,
    /// Show settings popup.
    show_settings: bool,
    /// Show plugin manager popup.
    show_plugins: bool,
    /// 卸载确认：挂起的插件 id（两段式确认，避免误删）。
    confirm_uninstall: Option<String>,
    /// Cached favicon textures keyed by FeedId.
    favicons: std::collections::HashMap<glean_core::FeedId, egui::TextureHandle>,
    /// Cached thumbnail textures keyed by EntryId (列表预览图).
    thumbnails: std::collections::HashMap<glean_core::EntryId, egui::TextureHandle>,
    /// Accumulator for periodic window-geometry persistence (§9 M3).
    geometry_timer: f32,
    /// 已应用到 ctx 的样式（dark, accent）；变化时才重建 style，避免每帧 set_style。
    applied_style: Option<(bool, AccentColor)>,
    /// 上一帧的 Win32 异步按键状态（用于 WebView2 焦点时快捷键边沿检测）。
    #[cfg(windows)]
    prev_async_keys: u16,
}

impl SpikeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        let state = SpikeState::new();
        apply_style(&cc.egui_ctx, state.dark, state.config.accent);
        // Give the tray an egui::Context clone so tray event callbacks can
        // directly drive viewport commands and repaints. This is essential
        // for restoring from tray: ctx.request_repaint() alone is a no-op
        // for hidden windows, so callbacks must also call Win32 ShowWindow.
        state.tray.set_egui_ctx(cc.egui_ctx.clone());
        let applied_style = Some((state.dark, state.config.accent));
        Self {
            state,
            primed: false,
            show_opml_import: false,
            show_errors: false,
            show_settings: false,
            show_plugins: false,
            confirm_uninstall: None,
            favicons: std::collections::HashMap::new(),
            thumbnails: std::collections::HashMap::new(),
            geometry_timer: 0.0,
            applied_style,
            #[cfg(windows)]
            prev_async_keys: 0,
        }
    }
}

impl eframe::App for SpikeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // First-frame: restore window position/size via ViewportCommand
        // (ViewportBuilder hints are unreliable on Windows).
        if !self.primed {
            if let (Some(x), Some(y)) = (self.state.config.window_x, self.state.config.window_y) {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
            }
            if let (Some(w), Some(h)) = (self.state.config.window_w, self.state.config.window_h) {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
            }
            if self.state.config.window_maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            // Apply system theme for native title bar.
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if self.state.dark {
                egui::viewport::SystemTheme::Dark
            } else {
                egui::viewport::SystemTheme::Light
            }));
            // Force-apply egui visuals (accent color, dark mode) in case
            // eframe's default initialization overrode our SpikeApp::new() call.
            apply_style(ctx, self.state.dark, self.state.config.accent);
            self.applied_style = Some((self.state.dark, self.state.config.accent));
        }

        // Auto-refresh timer.
        let dt = ctx.input(|i| i.stable_dt);
        self.state.tick_auto_refresh(dt);

        // Persist window geometry (§9 M3). Read current viewport info into
        // config every frame (cheap), flush to disk every few seconds.
        let (outer, inner, maximized, minimized) = ctx.input(|i| {
            let vp = i.viewport();
            (vp.outer_rect, vp.inner_rect, vp.maximized, vp.minimized)
        });
        let is_maximized = maximized.unwrap_or(false);
        let is_minimized = minimized.unwrap_or(false);
        if !is_maximized && !is_minimized {
            if let Some(r) = outer {
                self.state.config.window_x = Some(r.left());
                self.state.config.window_y = Some(r.top());
            }
            if let Some(r) = inner {
                self.state.config.window_w = Some(r.width());
                self.state.config.window_h = Some(r.height());
            }
        }
        self.state.config.window_maximized = is_maximized;
        self.geometry_timer += dt;
        if self.geometry_timer > 3.0 {
            self.geometry_timer = 0.0;
            self.state.sync_config();
            self.state.save_config();
        }

        // Poll background refresh every frame.
        self.state.poll_refresh();

        // Poll background full-text extraction.
        self.state.poll_extract();

        // Poll background AI enhance (summary/translate).
        self.state.poll_enhance();

        // Poll background image caching.
        self.state.poll_img_cache();

        // Poll background favicon downloads.
        while let Some((fid, rgba, w, h)) = self.state.poll_favicon_cache() {
            let size = [w as usize, h as usize];
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
            let tex = ctx.load_texture(
                format!("favicon_{}", fid.0),
                color_image,
                egui::TextureOptions::LINEAR,
            );
            self.favicons.insert(fid, tex);
        }

        // Poll background thumbnail downloads (列表预览图).
        for (eid, rgba, w, h) in self.state.poll_thumbnail_cache() {
            if w == 0 || h == 0 {
                continue; // decode failed
            }
            let size = [w as usize, h as usize];
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
            let tex = ctx.load_texture(
                format!("thumb_{}", eid.0),
                color_image,
                egui::TextureOptions::LINEAR,
            );
            self.thumbnails.insert(eid, tex);
        }
        // Spawn thumbnail downloads for entries lacking a texture.
        self.state.maybe_download_thumbnails();

        // Load cached favicons on first frame (after Bootstrap).
        if !self.primed {
            for (fid, rgba, w, h) in self.state.load_cached_favicons() {
                let size = [w as usize, h as usize];
                let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                let tex = ctx.load_texture(
                    format!("favicon_{}", fid.0),
                    color_image,
                    egui::TextureOptions::LINEAR,
                );
                self.favicons.insert(fid, tex);
            }
        }

        // Poll update-check thread.
        self.state.poll_update_check();

        // Poll tray actions (Windows only).
        while let Some(action) = self.state.tray.poll() {
            match action {
                TrayAction::Show => self.state.show_from_tray(ctx),
                TrayAction::Refresh => self.state.refresh_all_feeds_async(),
                TrayAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }

        // 主题（深浅）或主题色变化时重建样式；不变则跳过重设。
        let want_style = (self.state.dark, self.state.config.accent);
        if self.applied_style != Some(want_style) {
            apply_style(ctx, want_style.0, want_style.1);
            self.applied_style = Some(want_style);
        }

        let panel_fill = ctx.style().visuals.panel_fill;
        let extreme = ctx.style().visuals.extreme_bg_color;
        let stroke_color = ctx.style().visuals.window_stroke.color;

        // --- Top toolbar ---
        egui::TopBottomPanel::top("toolbar")
            .frame(
                Frame::new()
                    .fill(panel_fill)
                    .inner_margin(Margin::symmetric(8, 6))
                    .stroke(Stroke::new(1.0_f32, stroke_color)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Glean");
                    if ui.button("刷新全部").clicked() {
                        self.state.refresh_all_feeds_async();
                    }
                    if ui.button("全部已读").clicked() {
                        self.state.mark_all_read(None);
                    }
                    ui.separator();
                    if ui.button("星标").clicked() {
                        self.state.toggle_star_current();
                    }
                    // Per-article "显示图片" only in LoadOnDemand mode and not yet shown.
                    if self.state.config.image_policy == ImagePolicy::LoadOnDemand
                        && !self.state.reader_show_images
                        && self.state.open_detail.is_some()
                    {
                        if ui.button("显示图片").clicked() {
                            self.state.show_reader_images();
                        }
                    }
                    // Manual "抽取全文": enabled when an entry is open and no
                    // extraction is already in flight.
                    if self.state.open_detail.is_some() && self.state.extract_in_flight().is_none()
                    {
                        if ui.button("抽取全文").clicked() {
                            self.state.extract_current();
                        }
                    }
                    // AI 摘要/翻译：需已配置 AI 且有打开的 entry。
                    if self.state.open_detail.is_some() && self.state.ai_configured() {
                        let in_flight = self.state.enhance_in_flight();
                        let entry_id = self
                            .state
                            .open_detail
                            .as_ref()
                            .map(|e| e.summary.id)
                            .unwrap();
                        let summary_busy = in_flight
                            .map(|(id, k)| *id == entry_id && k == "summary")
                            .unwrap_or(false);
                        let translate_busy = in_flight
                            .map(|(id, k)| *id == entry_id && k == "translate")
                            .unwrap_or(false);
                        ui.add_enabled_ui(!summary_busy, |ui| {
                            if ui.button("摘要").clicked() {
                                self.state.enhance_current(EnhanceAction::Summarize);
                            }
                        });
                        ui.add_enabled_ui(!translate_busy, |ui| {
                            if ui.button("翻译").clicked() {
                                self.state.enhance_current(EnhanceAction::Translate {
                                    target_lang: "中文".into(),
                                });
                            }
                        });
                    }
                    ui.separator();
                    if ui.button("导入OPML").clicked() {
                        self.show_opml_import = !self.show_opml_import;
                    }
                    if ui.button("导出OPML").clicked() {
                        self.state.export_opml();
                    }
                    ui.separator();
                    if ui
                        .selectable_label(self.state.host_mode == ReaderHostMode::ChildEmbed, "H1")
                        .clicked()
                        && self.state.host_mode != ReaderHostMode::ChildEmbed
                    {
                        self.state.toggle_host_mode();
                    }
                    if ui
                        .selectable_label(
                            self.state.host_mode == ReaderHostMode::FollowOverlay,
                            "H2",
                        )
                        .clicked()
                        && self.state.host_mode != ReaderHostMode::FollowOverlay
                    {
                        self.state.toggle_host_mode();
                    }
                    ui.separator();
                    if ui.button("Theme").clicked() {
                        self.state.toggle_theme(ctx);
                    }
                    if ui.button("插件").clicked() {
                        self.show_plugins = !self.show_plugins;
                    }
                    if ui.button("设置").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                    if self.state.tray.is_active() && ui.button("最小化到托盘").clicked() {
                        self.state.hide_to_tray(ctx);
                    }
                    ui.separator();
                    ui.label("搜索");
                    let search_id = egui::Id::new("spike_search");
                    let prev_search = self.state.search.clone();
                    let te = egui::TextEdit::singleline(&mut self.state.search)
                        .id(search_id)
                        .desired_width(160.0)
                        .hint_text("标题/正文…");
                    let search_resp = ui.add(te);
                    if search_resp.clicked() || search_resp.gained_focus() {
                        self.state.reader.reclaim_shell_focus();
                        search_resp.request_focus();
                    }
                    // Run search on text change or Enter.
                    if self.state.search != prev_search
                        || (search_resp.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        self.state.run_search();
                    }
                    if ui.button("✕").clicked() {
                        self.state.search.clear();
                        self.state.set_filter(self.state.filter);
                    }
                    // Error badge
                    let err_count = self.state.errors.len();
                    if err_count > 0 {
                        let badge = format!("⚠{}", err_count);
                        if ui.button(&badge).clicked() {
                            self.show_errors = !self.show_errors;
                        }
                    }
                });
            });

        // --- Add-feed bar ---
        egui::TopBottomPanel::top("add_feed")
            .frame(
                Frame::new()
                    .fill(panel_fill)
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.label("分类");
                    let cur_cat = self.state.feed_add_category;
                    let cur_icon = cur_cat.map(|c| c.icon()).unwrap_or("●");
                    let cur_label = cur_cat.map(|c| c.label()).unwrap_or("自动");
                    egui::ComboBox::from_id_salt("feed_add_category")
                        .selected_text(format!("{cur_icon} {cur_label}"))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            // 保证下拉菜单足够宽，避免项目因宽度不足而出现滚动条。
                            ui.set_min_width(140.0);
                            for c in FEED_CATEGORIES {
                                if ui
                                    .selectable_label(
                                        cur_cat == Some(c),
                                        format!("{} {}", c.icon(), c.label()),
                                    )
                                    .clicked()
                                {
                                    self.state.feed_add_category = Some(c);
                                }
                            }
                            ui.separator();
                            if ui.selectable_label(cur_cat.is_none(), "● 自动").clicked() {
                                self.state.feed_add_category = None;
                            }
                        });

                    ui.label("文件夹");
                    let folder_label = match self.state.feed_add_folder {
                        Some(fid) => self
                            .state
                            .folders
                            .iter()
                            .find(|f| f.id == fid)
                            .map(|f| f.name.clone())
                            .unwrap_or_else(|| "无文件夹".into()),
                        None => "无文件夹".into(),
                    };
                    egui::ComboBox::from_id_salt("feed_add_folder")
                        .selected_text(&folder_label)
                        .width(100.0)
                        .show_ui(ui, |ui| {
                            for f in &self.state.folders {
                                if ui
                                    .selectable_label(
                                        self.state.feed_add_folder == Some(f.id),
                                        &f.name,
                                    )
                                    .clicked()
                                {
                                    self.state.feed_add_folder = Some(f.id);
                                    self.state.feed_add_new_folder.clear();
                                }
                            }
                            ui.separator();
                            if ui
                                .selectable_label(
                                    self.state.feed_add_folder.is_none()
                                        && self.state.feed_add_new_folder.trim().is_empty(),
                                    "无文件夹",
                                )
                                .clicked()
                            {
                                self.state.feed_add_folder = None;
                                self.state.feed_add_new_folder.clear();
                            }
                        });

                    ui.label("新建");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.state.feed_add_new_folder)
                            .id(egui::Id::new("feed_add_new_folder"))
                            .hint_text("新建或留空")
                            .desired_width(90.0),
                    );

                    ui.label("订阅 URL");
                    let feed_id = egui::Id::new("feed_url_input");
                    let te = egui::TextEdit::singleline(&mut self.state.feed_url_input)
                        .id(feed_id)
                        .desired_width(240.0)
                        .hint_text("https://…/rss.xml");
                    let resp = ui.add(te);
                    if resp.clicked() || resp.gained_focus() {
                        self.state.reader.reclaim_shell_focus();
                        resp.request_focus();
                    }
                    let enter = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                    if ui.button("添加").clicked() || enter {
                        self.state.add_feed_from_url();
                    }
                    hint(ui, "示例: https://www.reddit.com/r/rust/.rss");
                });
            });

        // --- Bottom status bar: 左=操作状态，右=快捷键说明（左右分隔，互不重叠） ---
        egui::TopBottomPanel::bottom("status")
            .frame(
                Frame::new()
                    .fill(panel_fill)
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let color = ui.visuals().strong_text_color().gamma_multiply(0.85);
                    ui.label(RichText::new(&self.state.status).size(13.0).color(color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        hint(
                            ui,
                            "j/k 换文 · r 刷新 · s 星标 · t 主题 · , 设置 · Esc 关闭弹窗",
                        );
                    });
                });
            });

        // --- Keyboard shortcuts ---
        // When WebView2 (reader) has focus, egui's ctx.input() doesn't receive
        // key events. Use Win32 GetAsyncKeyState as a fallback to detect
        // shortcuts regardless of focus. Edge-detect via prev_async_keys bitmap.
        let search_focused = ctx.memory(|m| m.has_focus(egui::Id::new("spike_search")));
        let feed_focused = ctx.memory(|m| m.has_focus(egui::Id::new("feed_url_input")));
        let feed_add_ff_focused = ctx.memory(|m| m.has_focus(egui::Id::new("feed_add_new_folder")));
        let rename_focused = self.state.rename_feed.is_some() || self.state.edit_feed_url.is_some();
        let text_input_active =
            search_focused || feed_focused || feed_add_ff_focused || rename_focused;

        // 当前窗口是否获得焦点。GetAsyncKeyState 读取的是全局按键状态：即使
        // 用户切换到其他软件，只要该键被按下也返回按下。因此快捷键整体必须以
        // 窗口焦点为门槛，避免在别的软件里输入时误触发 Glean 的动作。
        let app_focused = ctx.input(|i| i.focused);

        // Bit positions in async_keys bitmap: 0=J, 1=K, 2=T, 3=S, 4=R, 5=Comma
        const BIT_J: u16 = 0;
        const BIT_K: u16 = 1;
        const BIT_T: u16 = 2;
        const BIT_S: u16 = 3;
        const BIT_R: u16 = 4;
        const BIT_COMMA: u16 = 5;

        #[cfg(windows)]
        let async_just_pressed = {
            use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
            let vk_j = 0x4A_i32; // VK_J
            let vk_k = 0x4B_i32; // VK_K
            let vk_t = 0x54_i32; // VK_T
            let vk_s = 0x53_i32; // VK_S
            let vk_r = 0x52_i32; // VK_R
            let vk_comma = 0xBC_i32; // VK_OEM_COMMA
            let now = 0u16
                | (((unsafe { GetAsyncKeyState(vk_j) } as u16) >> 15) & 1) << BIT_J
                | (((unsafe { GetAsyncKeyState(vk_k) } as u16) >> 15) & 1) << BIT_K
                | (((unsafe { GetAsyncKeyState(vk_t) } as u16) >> 15) & 1) << BIT_T
                | (((unsafe { GetAsyncKeyState(vk_s) } as u16) >> 15) & 1) << BIT_S
                | (((unsafe { GetAsyncKeyState(vk_r) } as u16) >> 15) & 1) << BIT_R
                | (((unsafe { GetAsyncKeyState(vk_comma) } as u16) >> 15) & 1) << BIT_COMMA;
            let just = now & !self.prev_async_keys;
            self.prev_async_keys = now;
            just
        };

        #[cfg(not(windows))]
        let async_just_pressed = 0u16;

        if app_focused && !text_input_active {
            // Try egui input first; fall back to async detection on Windows.
            let j_pressed = ctx.input(|i| i.key_pressed(egui::Key::J))
                || (async_just_pressed & (1 << BIT_J) != 0);
            let k_pressed = ctx.input(|i| i.key_pressed(egui::Key::K))
                || (async_just_pressed & (1 << BIT_K) != 0);
            let t_pressed = ctx.input(|i| i.key_pressed(egui::Key::T))
                || (async_just_pressed & (1 << BIT_T) != 0);
            let s_pressed = ctx.input(|i| i.key_pressed(egui::Key::S))
                || (async_just_pressed & (1 << BIT_S) != 0);
            let r_pressed = ctx.input(|i| i.key_pressed(egui::Key::R))
                || (async_just_pressed & (1 << BIT_R) != 0);
            let comma_pressed = ctx.input(|i| i.key_pressed(egui::Key::Comma))
                || (async_just_pressed & (1 << BIT_COMMA) != 0);

            if j_pressed {
                self.state.next();
            }
            if k_pressed {
                self.state.prev();
            }
            if t_pressed {
                self.state.toggle_theme(ctx);
            }
            if s_pressed {
                self.state.toggle_star_current();
            }
            if r_pressed {
                self.state.refresh_all_feeds_async();
            }
            if comma_pressed {
                self.show_settings = !self.show_settings;
            }
        }
        // Esc closes topmost popup.
        if app_focused && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.state.rename_feed.is_some() {
                self.state.rename_feed = None;
            } else if self.state.edit_feed_url.is_some() {
                self.state.edit_feed_url = None;
            } else if self.show_errors {
                self.show_errors = false;
            } else if self.show_opml_import {
                self.show_opml_import = false;
            } else if self.show_plugins {
                self.show_plugins = false;
            } else if self.show_settings {
                self.show_settings = false;
            } else if self.state.opml_export.is_some() {
                self.state.opml_export = None;
            }
        }

        self.state.splitting = false;

        if ctx.input(|i| i.pointer.any_click()) {
            if let Some(pos) = ctx.pointer_latest_pos() {
                let in_reader =
                    self.state.reader_rect.is_positive() && self.state.reader_rect.contains(pos);
                if !in_reader {
                    self.state.reader.reclaim_shell_focus();
                }
            }
        }

        // --- Three columns ---
        egui::CentralPanel::default()
            .frame(Frame::new().fill(extreme).inner_margin(Margin::ZERO))
            .show(ctx, |ui| {
                let full = ui.available_rect_before_wrap();
                let h = full.height();
                let total_w = full.width();

                let max_nav =
                    (total_w - LIST_MIN - READER_MIN - 2.0 * SPLIT_HIT).clamp(NAV_MIN, NAV_MAX);
                let nav_w = self.state.nav_width.clamp(NAV_MIN, max_nav);

                let after_nav = (total_w - nav_w - SPLIT_HIT).max(0.0);
                let max_list = (after_nav - READER_MIN - SPLIT_HIT).clamp(LIST_MIN, LIST_MAX);
                let list_w = self.state.list_width.clamp(LIST_MIN, max_list);

                let mut x = full.min.x;

                // --- Nav column ---
                let nav_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(nav_w, h));
                paint_column_bg(ui, nav_rect, panel_fill, stroke_color);
                ui.allocate_new_ui(
                    egui::UiBuilder::new().id_salt("nav_col").max_rect(nav_rect),
                    |ui| {
                        column_contents(ui, "导航", |ui| {
                            self.draw_nav_contents(ui);
                        });
                    },
                );
                x += nav_w;

                let (hit, dragged) = splitter_drag(ui, ctx, x, full, "split_nav", |pos_x| {
                    self.state.nav_width = (pos_x - full.min.x).clamp(NAV_MIN, max_nav);
                });
                if dragged {
                    self.state.splitting = true;
                }
                x += hit;

                // --- List column ---
                let list_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(list_w, h));
                paint_column_bg(ui, list_rect, panel_fill, stroke_color);
                ui.allocate_new_ui(
                    egui::UiBuilder::new()
                        .id_salt("list_col")
                        .max_rect(list_rect),
                    |ui| {
                        column_contents(ui, "列表", |ui| {
                            self.draw_list_contents(ui);
                        });
                    },
                );
                x += list_w;

                let list_left_x = full.min.x + nav_w + SPLIT_HIT;
                let (hit, dragged) = splitter_drag(ui, ctx, x, full, "split_list", |pos_x| {
                    self.state.list_width = (pos_x - list_left_x).clamp(LIST_MIN, max_list);
                });
                if dragged {
                    self.state.splitting = true;
                }
                x += hit;

                // --- Reader column ---
                let reader_w = (full.max.x - x).max(READER_MIN.min(full.width() * 0.2));
                let reader_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(reader_w, h));
                self.state.reader_rect = reader_rect;

                paint_column_bg(
                    ui,
                    reader_rect,
                    Color32::from_gray(if self.state.dark { 30 } else { 245 }),
                    stroke_color,
                );
                #[cfg(not(windows))]
                ui.allocate_new_ui(
                    egui::UiBuilder::new()
                        .id_salt("reader_col")
                        .max_rect(reader_rect),
                    |ui| {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                ui.label(RichText::new("阅读区 (WebView)").strong());
                                ui.colored_label(
                                    Color32::from_rgb(200, 80, 40),
                                    "非 Windows：WebView2 未启用。请下载 CI artifact。",
                                );
                            });
                        });
                    },
                );
            });

        // Hide WebView2 when any popup is open so it doesn't occlude.
        let has_popup = self.state.opml_export.is_some()
            || self.show_opml_import
            || self.state.rename_feed.is_some()
            || self.state.edit_feed_url.is_some()
            || self.show_errors
            || self.show_plugins
            || self.show_settings
            || self.state.update_available.is_some()
            || ctx.memory(|memory| memory.any_popup_open());
        self.state.reader.set_hidden(has_popup);

        // --- Popups (after CentralPanel so they render on top) ---

        // OPML export popup
        if let Some(xml) = self.state.opml_export.clone() {
            let mut close = false;
            let mut copied = false;
            let mut saved = false;
            let mut txt = xml.clone();
            let mut win = egui::Window::new("OPML 导出")
                .resizable(true)
                .default_width(520.0)
                .default_height(340.0)
                .collapsible(false);
            if let Some([x, y, w, h]) = self.state.popup_geom("opml_export") {
                win = win.default_pos(egui::pos2(x, y)).default_size([w, h]);
            }
            let win_resp = win.show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("复制到剪贴板").clicked() {
                        copied = true;
                    }
                    if ui.button("另存为…").clicked() {
                        saved = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("关闭").clicked() {
                            close = true;
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height())
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut txt)
                                .desired_width(f32::INFINITY)
                                .code_editor(),
                        );
                    });
            });
            if copied {
                ctx.copy_text(xml.clone());
                self.state.status = "OPML 已复制到剪贴板".into();
            }
            if saved {
                let xml_clone = xml.clone();
                let path = rfd::FileDialog::new()
                    .set_file_name("glean-subscriptions.opml")
                    .add_filter("OPML", &["opml", "xml"])
                    .save_file();
                if let Some(path) = path {
                    match std::fs::write(&path, &xml_clone) {
                        Ok(()) => self.state.status = format!("已保存到 {}", path.display()),
                        Err(e) => self.state.status = format!("保存失败: {e}"),
                    }
                }
            }
            if let Some(inner) = &win_resp {
                let rect = inner.response.rect.intersect(ctx.screen_rect());
                self.state.save_popup_geom(
                    "opml_export",
                    [rect.min.x, rect.min.y, rect.width(), rect.height()],
                );
            }
            if close {
                self.state.opml_export = None;
            }
        }

        // OPML import popup
        if self.show_opml_import {
            let mut do_import_text = false;
            let mut do_import_file = false;
            let mut close = false;
            let mut win = egui::Window::new("OPML 导入")
                .resizable(true)
                .default_width(520.0)
                .default_height(200.0)
                .collapsible(false);
            if let Some([x, y, w, h]) = self.state.popup_geom("opml_import") {
                win = win.default_pos(egui::pos2(x, y)).default_size([w, h]);
            }
            let win_resp = win.show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("选择文件…").clicked() {
                        do_import_file = true;
                    }
                    if ui.button("导入文本").clicked() {
                        do_import_text = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("关闭").clicked() {
                            close = true;
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("导入方式");
                    ui.radio_value(&mut self.state.opml_import_overwrite, false, "追加")
                        .on_hover_text("保留现有订阅，只添加 OPML 中不存在的新源");
                    ui.radio_value(&mut self.state.opml_import_overwrite, true, "覆盖")
                        .on_hover_text("先清空现有订阅，再按 OPML 导入（星标文章保留）");
                });
                ui.label("或粘贴 OPML XML：");
                let te = egui::TextEdit::multiline(&mut self.state.opml_import_input)
                    .desired_width(f32::INFINITY)
                    .desired_rows(4)
                    .code_editor();
                let resp = ui.add(te);
                if resp.clicked() || resp.gained_focus() {
                    self.state.reader.reclaim_shell_focus();
                    resp.request_focus();
                }
            });
            if do_import_file {
                let path = rfd::FileDialog::new()
                    .add_filter("OPML", &["opml", "xml"])
                    .pick_file();
                if let Some(path) = path {
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            self.state.opml_import_input = content;
                            self.state.import_opml();
                        }
                        Err(e) => self.state.status = format!("读取失败: {e}"),
                    }
                }
            }
            if do_import_text {
                self.state.import_opml();
            }
            if let Some(inner) = &win_resp {
                let rect = inner.response.rect.intersect(ctx.screen_rect());
                self.state.save_popup_geom(
                    "opml_import",
                    [rect.min.x, rect.min.y, rect.width(), rect.height()],
                );
            }
            if close {
                self.show_opml_import = false;
                self.state.opml_import_input.clear();
            }
        }

        // Rename feed popup
        if let Some((feed_id, ref mut title)) = self.state.rename_feed {
            let mut do_rename = false;
            let mut close = false;
            egui::Window::new("重命名订阅")
                .resizable(false)
                .default_width(360.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("新名称");
                        let te = egui::TextEdit::singleline(title)
                            .id(egui::Id::new("rename_feed_input"))
                            .desired_width(f32::INFINITY);
                        let resp = ui.add(te);
                        if resp.clicked() || resp.gained_focus() {
                            self.state.reader.reclaim_shell_focus();
                            resp.request_focus();
                        }
                        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            do_rename = true;
                        }
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("确定").clicked() {
                            do_rename = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });
            if do_rename {
                let t = title.clone();
                self.state.rename_feed(feed_id, t);
                self.state.rename_feed = None;
            }
            if close {
                self.state.rename_feed = None;
            }
        }

        // Edit feed URL popup
        if let Some((feed_id, ref mut url)) = self.state.edit_feed_url {
            let mut do_save = false;
            let mut close = false;
            egui::Window::new("编辑订阅 URL")
                .resizable(false)
                .default_width(420.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("URL");
                        let te = egui::TextEdit::singleline(url)
                            .id(egui::Id::new("edit_feed_url_input"))
                            .desired_width(f32::INFINITY);
                        let resp = ui.add(te);
                        if resp.clicked() || resp.gained_focus() {
                            self.state.reader.reclaim_shell_focus();
                            resp.request_focus();
                        }
                        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            do_save = true;
                        }
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            do_save = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });
            if do_save {
                let u = url.clone();
                self.state.edit_feed_url(feed_id, u);
                self.state.edit_feed_url = None;
            }
            if close {
                self.state.edit_feed_url = None;
            }
        }

        // Error log popup
        if self.show_errors {
            let mut close = false;
            let mut clear = false;
            let mut win = egui::Window::new("错误日志")
                .resizable(true)
                .default_width(480.0)
                .default_height(320.0)
                .collapsible(false);
            if let Some([x, y, w, h]) = self.state.popup_geom("errors") {
                win = win.default_pos(egui::pos2(x, y)).default_size([w, h]);
            }
            let win_resp = win.show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("清空").clicked() {
                        clear = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("关闭").clicked() {
                            close = true;
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height())
                    .show(ui, |ui| {
                        for (i, err) in self.state.errors.iter().enumerate().rev() {
                            ui.horizontal(|ui| {
                                let color = ui.visuals().strong_text_color().gamma_multiply(0.72);
                                ui.label(
                                    RichText::new(format!("#{}", i + 1)).size(12.5).color(color),
                                );
                                ui.label(RichText::new(err).size(13.5));
                            });
                            ui.separator();
                        }
                    });
            });
            if let Some(inner) = &win_resp {
                let rect = inner.response.rect.intersect(ctx.screen_rect());
                self.state.save_popup_geom(
                    "errors",
                    [rect.min.x, rect.min.y, rect.width(), rect.height()],
                );
            }
            if clear {
                self.state.errors.clear();
                self.state.status = "错误日志已清空".into();
            }
            if close {
                self.show_errors = false;
            }
        }

        // Settings popup
        if self.show_settings {
            let mut close = false;
            let mut win = egui::Window::new("设置")
                .resizable(true)
                .default_size([480.0, 560.0])
                .collapsible(false);
            if let Some([x, y, w, h]) = self.state.popup_geom("settings") {
                win = win.default_pos(egui::pos2(x, y)).default_size([w, h]);
            }
            let win_resp = win.show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            settings_heading(ui, "外观");
                            ui.horizontal(|ui| {
                                ui.label("主题");
                                if ui.selectable_label(!self.state.dark, "浅色").clicked()
                                    && self.state.dark
                                {
                                    self.state.toggle_theme(ctx);
                                }
                                if ui.selectable_label(self.state.dark, "深色").clicked()
                                    && !self.state.dark
                                {
                                    self.state.toggle_theme(ctx);
                                }
                            });
                            // 主题色色板：圆形色块，选中者加描边。
                            ui.horizontal(|ui| {
                                ui.label("主题色");
                                for c in ACCENT_COLORS {
                                    let (r, g, b) = c.rgb(self.state.dark);
                                    let color = Color32::from_rgb(r, g, b);
                                    let selected = self.state.config.accent == c;
                                    let (rect, resp) = ui.allocate_exact_size(
                                        Vec2::splat(26.0),
                                        Sense::click(),
                                    );
                                    let border = if selected {
                                        Stroke::new(2.5_f32, ui.visuals().strong_text_color())
                                    } else {
                                        Stroke::new(
                                            1.0_f32,
                                            ui.visuals().widgets.noninteractive.bg_stroke.color,
                                        )
                                    };
                                    ui.painter().circle(rect.center(), 9.0, color, border);
                                    if resp.clicked() && !selected {
                                        self.state.config.accent = c;
                                        self.state.sync_config();
                                        self.state.save_config();
                                    }
                                    resp.on_hover_text(c.label());
                                }
                            });
                            hint(ui, "主题色影响选中背景、悬停高亮与链接颜色");

                            settings_heading(ui, "阅读");
                            ui.horizontal(|ui| {
                                ui.label("远程图片");
                                let policy = self.state.config.image_policy;
                                if ui
                                    .selectable_label(policy == ImagePolicy::Block, "拦截")
                                    .clicked()
                                    && policy != ImagePolicy::Block
                                {
                                    self.state.config.image_policy = ImagePolicy::Block;
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                                if ui
                                    .selectable_label(policy == ImagePolicy::LoadOnDemand, "按需")
                                    .clicked()
                                    && policy != ImagePolicy::LoadOnDemand
                                {
                                    self.state.config.image_policy = ImagePolicy::LoadOnDemand;
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                                if ui
                                    .selectable_label(policy == ImagePolicy::Allow, "允许")
                                    .clicked()
                                    && policy != ImagePolicy::Allow
                                {
                                    self.state.config.image_policy = ImagePolicy::Allow;
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                            });
                            hint(ui, "拦截=去图；按需=每篇可点「显示图片」；允许=始终加载");

                            settings_heading(ui, "布局");
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "导航 {} · 列表 {}",
                                    self.state.nav_width as i32, self.state.list_width as i32
                                ));
                                if ui.button("重置布局").clicked() {
                                    self.state.reset_layout();
                                }
                            });

                            settings_heading(ui, "刷新");
                            ui.horizontal(|ui| {
                                ui.label("全局自动刷新间隔（秒，0=关闭）");
                                let te = egui::TextEdit::singleline(&mut self.state.refresh_interval_input)
                                    .id(egui::Id::new("refresh_interval_input"))
                                    .desired_width(80.0);
                                let resp = ui.add(te);
                                if resp.clicked() || resp.gained_focus() {
                                    self.state.reader.reclaim_shell_focus();
                                    resp.request_focus();
                                }
                                if resp.lost_focus() {
                                    if let Ok(v) = self.state.refresh_interval_input.parse::<i64>() {
                                        self.state.set_global_refresh_interval(v.max(0));
                                    } else {
                                        // Reset to current config on invalid input.
                                        self.state.refresh_interval_input =
                                            self.state.config.refresh_interval_secs.to_string();
                                    }
                                }
                            });
                            hint(ui, "每源可在右键菜单单独设置刷新间隔");

                            settings_heading(ui, "全文与缓存");
                            ui.horizontal(|ui| {
                                ui.label("自动抽取");
                                let cur = self.state.config.auto_extract;
                                if ui.selectable_label(cur, "开").clicked() && !cur {
                                    self.state.config.auto_extract = true;
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                                if ui.selectable_label(!cur, "关").clicked() && cur {
                                    self.state.config.auto_extract = false;
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                            });
                            hint(ui, "打开短摘要文章时后台抓取原文全文（readability）");

                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label("缓存图片");
                                let cur = self.state.config.cache_images;
                                if ui.selectable_label(cur, "开").clicked() && !cur {
                                    self.state.config.cache_images = true;
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                                if ui.selectable_label(!cur, "关").clicked() && cur {
                                    self.state.config.cache_images = false;
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                            });
                            hint(ui, "显示图片时下载到本地，改写 src 指向本地缓存（离线可看）");

                            settings_heading(ui, "网络");
                            ui.horizontal(|ui| {
                                ui.label("HTTP 代理");
                                let te = egui::TextEdit::singleline(&mut self.state.config.proxy_url)
                                    .id(egui::Id::new("proxy_url_input"))
                                    .desired_width(200.0)
                                    .hint_text("http://127.0.0.1:7890");
                                let resp = ui.add(te);
                                if resp.clicked() || resp.gained_focus() {
                                    self.state.reader.reclaim_shell_focus();
                                    resp.request_focus();
                                }
                                if resp.lost_focus() {
                                    // 立即重建带代理的 HTTP 客户端，无需重启。
                                    let proxy = self.state.config.proxy_url.clone();
                                    match self.state.service.set_proxy_url(&proxy) {
                                        Ok(()) => {
                                            self.state.status = if proxy.trim().is_empty() {
                                                "代理已清除".into()
                                            } else {
                                                "代理已生效（开启「使用代理」的插件/订阅会走此代理）".into()
                                            };
                                        }
                                        Err(e) => {
                                            self.state.status = e.to_string();
                                            // 配置里保留原值，用户可看到后修正。
                                        }
                                    }
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                            });
                            hint(ui, "支持 http/socks5 代理；开启「使用代理」的订阅会走此代理");

                            settings_heading(ui, "AI 增强");
                            if self.state.ai_configured() {
                                hint(ui, "已配置：阅读工具栏显示「摘要」「翻译」");
                            } else {
                                hint(ui, "未配置：阅读工具栏不显示 AI 按钮");
                            }
                            ui.horizontal(|ui| {
                                ui.label("Base URL");
                                let te = egui::TextEdit::singleline(&mut self.state.ai_base_url_input)
                                    .id(egui::Id::new("ai_base_url_input"))
                                    .desired_width(200.0)
                                    .hint_text("https://api.openai.com/v1");
                                let resp = ui.add(te);
                                if resp.clicked() || resp.gained_focus() {
                                    self.state.reader.reclaim_shell_focus();
                                    resp.request_focus();
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("模型");
                                let te = egui::TextEdit::singleline(&mut self.state.ai_model_input)
                                    .id(egui::Id::new("ai_model_input"))
                                    .desired_width(200.0)
                                    .hint_text("gpt-4o-mini / deepseek-chat");
                                let resp = ui.add(te);
                                if resp.clicked() || resp.gained_focus() {
                                    self.state.reader.reclaim_shell_focus();
                                    resp.request_focus();
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("API Key");
                                let te = egui::TextEdit::singleline(&mut self.state.ai_key_input)
                                    .id(egui::Id::new("ai_key_input"))
                                    .password(true)
                                    .desired_width(200.0)
                                    .hint_text("sk-…");
                                let resp = ui.add(te);
                                if resp.clicked() || resp.gained_focus() {
                                    self.state.reader.reclaim_shell_focus();
                                    resp.request_focus();
                                }
                            });
                            hint(ui, "OpenAI 兼容协议。api_key 加密存储（Windows DPAPI），不落明文；留空则保留已存 key。");
                            ui.horizontal(|ui| {
                                if ui.button("保存").clicked() {
                                    self.state.save_ai_config();
                                }
                                if self.state.ai_configured() && ui.button("清除配置").clicked() {
                                    self.state.clear_ai_config();
                                }
                            });

                            settings_heading(ui, "排版");
                            ui.horizontal(|ui| {
                                ui.label("字体大小 (px)");
                                let te = egui::TextEdit::singleline(&mut self.state.font_size_input)
                                    .id(egui::Id::new("font_size_input"))
                                    .desired_width(50.0);
                                let resp = ui.add(te);
                                if resp.clicked() || resp.gained_focus() {
                                    self.state.reader.reclaim_shell_focus();
                                    resp.request_focus();
                                }
                                if resp.lost_focus() {
                                    if let Ok(v) = self.state.font_size_input.parse::<u16>() {
                                        if v >= 10 && v <= 32 {
                                            self.state.config.font_size_px = v;
                                            self.state.sync_config();
                                            self.state.save_config();
                                        }
                                    } else {
                                        self.state.font_size_input =
                                            self.state.config.font_size_px.to_string();
                                    }
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("行宽 (rem)");
                                let te = egui::TextEdit::singleline(&mut self.state.line_width_input)
                                    .id(egui::Id::new("line_width_input"))
                                    .desired_width(50.0);
                                let resp = ui.add(te);
                                if resp.clicked() || resp.gained_focus() {
                                    self.state.reader.reclaim_shell_focus();
                                    resp.request_focus();
                                }
                                if resp.lost_focus() {
                                    if let Ok(v) = self.state.line_width_input.parse::<u16>() {
                                        if v >= 20 && v <= 80 {
                                            self.state.config.line_width_rem = v;
                                            self.state.sync_config();
                                            self.state.save_config();
                                        }
                                    } else {
                                        self.state.line_width_input =
                                            self.state.config.line_width_rem.to_string();
                                    }
                                }
                            });
                            hint(ui, "修改后重新打开文章生效");

                            settings_heading(ui, "列表");
                            ui.horizontal(|ui| {
                                ui.label("缩略图大小");
                                let slider = egui::Slider::new(
                                    &mut self.state.config.thumbnail_size,
                                    THUMBNAIL_SIZE_MIN..=THUMBNAIL_SIZE_MAX,
                                )
                                .fixed_decimals(0)
                                .suffix(" px");
                                let resp = ui.add(slider);
                                if resp.changed() {
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                            });
                            hint(ui, "列表条目缩略图边长（同时影响行高）");

                            settings_heading(ui, "存储");
                            if ui.button("清除所有缓存").clicked() {
                                let removed = glean_core::clear_all_cache();
                                // Also clear in-memory favicon textures.
                                self.favicons.clear();
                                self.state.status = format!("已清除 {} 个缓存文件", removed);
                            }
                            hint(ui, "清除正文缓存、图片缓存、Favicon 缓存（数据库不受影响）");
                            ui.add_space(4.0);
                            if ui.button("修复数据库").clicked() {
                                self.state.status = self.state.repair_database();
                            }
                            hint(ui, "订阅删除失败/提示 database disk image is malformed 时使用；原文件备份为 .db.bak");
                            ui.add_space(4.0);
                            if ui.button("强制重建数据库").clicked() {
                                self.state.status = self.state.force_rebuild_database();
                            }
                            hint(ui, "「修复数据库」仍无法解决时使用：不检测直接重建，原文件备份为 .db.bak");
                            ui.horizontal(|ui| {
                                ui.label("缓存位置");
                                let te = egui::TextEdit::singleline(&mut self.state.cache_dir_input)
                                    .id(egui::Id::new("cache_dir_input"))
                                    .desired_width(280.0)
                                    .hint_text("留空使用默认路径");
                                let resp = ui.add(te);
                                if resp.clicked() || resp.gained_focus() {
                                    self.state.reader.reclaim_shell_focus();
                                    resp.request_focus();
                                }
                                if resp.lost_focus() {
                                    let trimmed = self.state.cache_dir_input.trim().to_string();
                                    if trimmed.is_empty() {
                                        self.state.config.cache_dir = None;
                                    } else {
                                        self.state.config.cache_dir = Some(trimmed);
                                    }
                                    self.state.sync_config();
                                    self.state.save_config();
                                }
                                if ui.button("浏览…").clicked() {
                                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                        let s = dir.to_string_lossy().to_string();
                                        self.state.cache_dir_input = s.clone();
                                        self.state.config.cache_dir = Some(s);
                                        self.state.sync_config();
                                        self.state.save_config();
                                    }
                                }
                            });
                            hint(ui, "自定义缓存根目录（需重启生效）；留空使用默认路径");

                            settings_heading(ui, "插件");
                            if ui.button("管理插件…").clicked() {
                                self.show_settings = false;
                                self.show_plugins = true;
                            }
                            hint(ui, "安装 / 卸载 / 启用停用，见「插件管理」窗口");

                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("关闭").clicked() {
                                        close = true;
                                    }
                                });
                            });
                        });
                });
            if let Some(inner) = &win_resp {
                let rect = inner.response.rect.intersect(ctx.screen_rect());
                self.state.save_popup_geom(
                    "settings",
                    [rect.min.x, rect.min.y, rect.width(), rect.height()],
                );
            }
            if close {
                self.show_settings = false;
            }
        }

        // Plugin manager popup.
        if self.show_plugins {
            let mut open = true;
            let mut close = false;
            let mut win = egui::Window::new("插件管理")
                .open(&mut open)
                .default_size([560.0, 400.0])
                .max_height(720.0);
            if let Some([x, y, w, h]) = self.state.popup_geom("plugins") {
                win = win.default_pos(egui::pos2(x, y)).default_size([w, h]);
            }
            let win_resp = win.show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("安装插件（文件夹）…").clicked() {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.state.install_plugin_from_dir(&dir);
                            }
                        }
                        if ui.button("安装插件（zip）…").clicked() {
                            if let Some(file) = rfd::FileDialog::new()
                                .add_filter("zip 压缩包", &["zip"])
                                .pick_file()
                            {
                                self.state.install_plugin_from_zip(&file);
                            }
                        }
                    });
                    hint(ui, "插件目录: <data_dir>/plugins/<id>/（manifest.toml + adapter.rhai），官方插件见仓库 plugins/ 目录");
                    ui.separator();
                    // 克隆列表：循环内要对 self.state 做可变操作。
                    let plugin_list: Vec<glean_core::plugin::LoadedPlugin> = self
                        .state
                        .service
                        .plugins()
                        .map(|m| m.list().to_vec())
                        .unwrap_or_default();
                    if plugin_list.is_empty() {
                        hint(ui, "未安装任何插件。点击上方按钮安装，或从仓库 plugins/ 目录导入。");
                    }
                    egui::ScrollArea::vertical()
                        .auto_shrink([true, true])
                        .show(ui, |ui| {
                            for p in &plugin_list {
                                let id = &p.manifest.plugin.id;
                                let disabled = self
                                    .state
                                    .service
                                    .plugins()
                                    .map(|m| m.is_disabled(id))
                                    .unwrap_or(false);
                                let tier_label = match p.manifest.plugin.tier {
                                    glean_core::Tier::Config => "Tier 1 配置",
                                    glean_core::Tier::Script => "Tier 2 脚本",
                                    glean_core::Tier::Builtin => "内置",
                                };
                                egui::Frame::group(ui.style()).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "{} v{}",
                                                p.manifest.plugin.name, p.manifest.plugin.version
                                            ))
                                            .strong(),
                                        );
                                        let color = ui.visuals().strong_text_color().gamma_multiply(0.72);
                                        ui.label(
                                            RichText::new(format!("({id}) [{tier_label}]"))
                                                .size(12.5)
                                                .color(color),
                                        );
                                    });
                                    let patterns = p
                                        .manifest
                                        .r#match
                                        .iter()
                                        .map(|r| r.url_pattern.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    if !patterns.is_empty() {
                                        hint(ui, format!("匹配: {patterns}"));
                                    }
                                    let caps = &p.manifest.capabilities;
                                    let mut cap_bits = Vec::new();
                                    if !caps.feed_fetch.is_empty() {
                                        cap_bits.push(format!(
                                            "fetch=[{}]",
                                            caps.feed_fetch.join(", ")
                                        ));
                                    }
                                    if !caps.credential_use.is_empty() {
                                        cap_bits.push(format!(
                                            "creds=[{}]",
                                            caps.credential_use.join(", ")
                                        ));
                                    }
                                    if !caps.content_transform.is_empty() {
                                        cap_bits.push(format!(
                                            "transform=[{}]",
                                            caps.content_transform.join(", ")
                                        ));
                                    }
                                    if !caps.external_call.is_empty() {
                                        cap_bits.push(format!(
                                            "external=[{}]",
                                            caps.external_call.join(", ")
                                        ));
                                    }
                                    if !cap_bits.is_empty() {
                                        hint(ui, format!("能力: {}", cap_bits.join(" · ")));
                                    }
                                    if p.manifest.compliance.uses_user_session {
                                        hint(ui, "合规: 使用用户会话（凭证 Host 注入，插件不可见）");
                                    }
                                    // 凭证槽设置（§11.5.9 UI 入口）。
                                    if !caps.credential_use.is_empty() {
                                        let slots = caps.credential_use.clone();
                                        for slot in &slots {
                                            let has_cred = self
                                                .state
                                                .service
                                                .get_credential(id, slot)
                                                .is_some();
                                            let key = format!("{id}:{slot}");
                                            let mut do_save = false;
                                            let mut do_remove = false;
                                            ui.horizontal(|ui| {
                                                let color = ui.visuals().strong_text_color().gamma_multiply(0.85);
                                                ui.label(
                                                    RichText::new(format!("凭证槽: {slot}"))
                                                        .size(13.0)
                                                        .color(color),
                                                );
                                                if has_cred {
                                                    ui.colored_label(
                                                        ui.visuals().hyperlink_color,
                                                        "已设置",
                                                    );
                                                } else {
                                                    hint(ui, "未设置");
                                                }
                                            });
                                            {
                                                // 借用放在独立作用域，避免与下方 self 方法调用冲突。
                                                let entry = self
                                                    .state
                                                    .plugin_cred_edits
                                                    .entry(key.clone())
                                                    .or_default();
                                                ui.horizontal(|ui| {
                                                    ui.label("Header 名");
                                                    ui.add(
                                                        egui::TextEdit::singleline(&mut entry.0)
                                                            .id(egui::Id::new(format!(
                                                                "plugin_cred_name_{key}"
                                                            )))
                                                            .desired_width(100.0)
                                                            .hint_text("可选，如 Cookie"),
                                                    );
                                                    ui.label("凭证值");
                                                    ui.add(
                                                        egui::TextEdit::singleline(&mut entry.1)
                                                            .id(egui::Id::new(format!(
                                                                "plugin_cred_val_{key}"
                                                            )))
                                                            .password(true)
                                                            .desired_width(180.0),
                                                    );
                                                });
                                            }
                                            ui.horizontal(|ui| {
                                                if ui.button("保存凭证").clicked() {
                                                    do_save = true;
                                                }
                                                if has_cred && ui.button("清除").clicked() {
                                                    do_remove = true;
                                                }
                                                hint(ui, "凭证由 Host 注入，插件脚本不可见");
                                            });
                                            hint(ui, "Pixiv 场景：把 Refresh Token 填入「凭证值」，Header 名留空即可");
                                            if do_save {
                                                self.state.save_plugin_credential(id, slot);
                                            }
                                            if do_remove {
                                                self.state.remove_plugin_credential(id, slot);
                                            }
                                        }
                                    }
                                    ui.horizontal(|ui| {
                                        let mut enabled = !disabled;
                                        if ui.checkbox(&mut enabled, "启用").changed() {
                                            self.state.toggle_plugin(id, enabled);
                                        }
                                        // 插件级「使用代理」（§11.5.10）：命中该插件
                                        // 的请求（含添加订阅时）走设置页配置的代理。
                                        let mut proxy = self.state.config.plugin_proxy.contains(id);
                                        if ui.checkbox(&mut proxy, "使用代理").changed() {
                                            self.state.set_plugin_proxy(id, proxy);
                                        }
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if self.confirm_uninstall.as_deref()
                                                    == Some(id.as_str())
                                                {
                                                    if ui.button("确认卸载？").clicked() {
                                                        self.state.uninstall_plugin(id);
                                                        self.confirm_uninstall = None;
                                                    }
                                                    if ui.button("取消").clicked() {
                                                        self.confirm_uninstall = None;
                                                    }
                                                } else if ui.button("卸载").clicked() {
                                                    self.confirm_uninstall = Some(id.clone());
                                                }
                                            },
                                        );
                                    });
                                });
                            }
                        });
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("关闭").clicked() {
                                close = true;
                            }
                        });
                    });
                });
            if let Some(inner) = &win_resp {
                let rect = inner.response.rect.intersect(ctx.screen_rect());
                self.state.save_popup_geom(
                    "plugins",
                    [rect.min.x, rect.min.y, rect.width(), rect.height()],
                );
            }
            if !open || close {
                self.show_plugins = false;
                self.confirm_uninstall = None;
            }
        }

        // Update available popup (V1: prompt only, no auto-install).
        if self.state.update_available.is_some() {
            let mut close = false;
            let mut open_browser = false;
            egui::Window::new("发现新版本")
                .resizable(false)
                .default_width(420.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    let (current, version, url, changelog) = match &self.state.update_available {
                        Some(update::UpdateCheckResult::Available { current, cast }) => (
                            current.clone(),
                            cast.version.clone(),
                            cast.url.clone(),
                            cast.changelog.clone(),
                        ),
                        _ => unreachable!(),
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✨").strong());
                        ui.label(format!("新版本 {} 已发布（当前 {}）", version, current));
                    });
                    ui.add_space(6.0);
                    if let Some(log) = &changelog {
                        let color = ui.visuals().strong_text_color().gamma_multiply(0.8);
                        ui.label(RichText::new("更新日志：").size(13.5).color(color));
                        egui::ScrollArea::vertical()
                            .max_height(160.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new(log).size(13.5));
                            });
                    } else {
                        hint(ui, "无更新日志");
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("前往下载").clicked() {
                            open_browser = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("稍后").clicked() {
                                close = true;
                            }
                        });
                    });
                    let _ = (current, version, url);
                });
            if open_browser {
                if let Some(update::UpdateCheckResult::Available { cast, .. }) =
                    &self.state.update_available
                {
                    update::open_url(&cast.url);
                }
            }
            if close {
                self.state.update_available = None;
            }
        }

        // --- WebView2 attach (Windows only) ---
        #[cfg(windows)]
        {
            let ppp = ctx.pixels_per_point();
            match self.state.reader.ensure_attached(
                self.state.host_mode,
                self.state.reader_rect,
                ppp,
            ) {
                Ok(()) => {
                    self.state.reader.sync_bounds(self.state.reader_rect, ppp);
                    if !self.primed {
                        if !self.state.entries.is_empty() {
                            self.state.select_index(0);
                        }
                        self.primed = true;
                    }
                }
                Err(e) => {
                    let transient =
                        e.contains("not ready") || e.contains("retry") || e.contains("not found");
                    if !transient {
                        self.state.status = format!("WebView error: {e}");
                    }
                }
            }
        }

        #[cfg(not(windows))]
        if !self.primed {
            if !self.state.entries.is_empty() {
                self.state.select_index(0);
            }
            self.primed = true;
        }

        // Repaint: faster during refresh/split for responsiveness.
        if self.state.splitting || self.state.refresh_rx.is_some() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.state.sync_config();
        self.state.save_config();
        self.state.reader.shutdown();
    }
}

// --- Nav column contents (folder-grouped feed list) ---

impl SpikeApp {
    fn draw_nav_contents(&mut self, ui: &mut Ui) {
        // --- 过滤器行：未读总数 + 全部/未读/星标/今日 ---
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("未读 {}", self.state.unread_total))
                    .strong()
                    .size(12.5),
            );
            ui.separator();
            let filters: [(EntryFilter, &str); 4] = [
                (EntryFilter::All, "全部"),
                (EntryFilter::Unread, "未读"),
                (EntryFilter::Starred, "星标"),
                (EntryFilter::Today, "今日"),
            ];
            for (f, label) in filters {
                let active =
                    std::mem::discriminant(&self.state.filter) == std::mem::discriminant(&f);
                if ui.selectable_label(active, label).clicked() {
                    self.state.set_filter(f);
                }
            }
        });

        // --- 分类 Tab 行：水平排列，显示各分类未读计数 ---
        ui.horizontal_wrapped(|ui| {
            let all_active = self.state.nav_active_category.is_none();
            let all_unread: u64 = self.state.unread_per_feed.values().sum();
            let all_label = if all_unread > 0 {
                format!("全部 ({})", all_unread)
            } else {
                "全部".to_string()
            };
            if ui.selectable_label(all_active, all_label).clicked() {
                self.state.nav_active_category = None;
            }
            for category in FEED_CATEGORIES {
                let cat_unread: u64 = self
                    .state
                    .feeds
                    .iter()
                    .filter(|f| f.category == category)
                    .map(|f| self.state.unread_per_feed.get(&f.id).copied().unwrap_or(0))
                    .sum();
                let has_feeds = self.state.feeds.iter().any(|f| f.category == category);
                if !has_feeds && cat_unread == 0 {
                    continue;
                }
                let active = self.state.nav_active_category == Some(category);
                let label = if cat_unread > 0 {
                    format!("{} {}", category.icon(), cat_unread)
                } else {
                    category.icon().to_string()
                };
                let resp = ui.selectable_label(active, label);
                resp.clone().on_hover_text(category.label());
                if resp.clicked() {
                    self.state.nav_active_category = Some(category);
                }
            }
        });
        ui.separator();

        // Collect action requests from the closure.
        let mut action = None;
        let mut do_create_folder = false;

        // Context menu on "订阅" header for creating folders.
        let color = ui.visuals().strong_text_color().gamma_multiply(0.72);
        ui.label(RichText::new("订阅").size(12.5).color(color));
        ui.menu_button("＋ 文件夹", |ui| {
            let te = egui::TextEdit::singleline(&mut self.state.new_folder_input)
                .id(egui::Id::new("new_folder_input"))
                .desired_width(120.0)
                .hint_text("文件夹名");
            let resp = ui.add(te);
            if resp.clicked() || resp.gained_focus() {
                self.state.reader.reclaim_shell_focus();
                resp.request_focus();
            }
            if ui.button("创建").clicked()
                || (resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                do_create_folder = true;
            }
        });

        // 多选工具栏：批量移动 / 删除。
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(self.state.feed_multi_select, "多选")
                .clicked()
            {
                self.state.feed_multi_select = !self.state.feed_multi_select;
                if !self.state.feed_multi_select {
                    self.state.selected_feeds.clear();
                }
            }
            if self.state.feed_multi_select {
                // 当前分类下可见的订阅（与下方列表过滤逻辑一致）。
                let visible_feeds: Vec<glean_core::FeedId> = self
                    .state
                    .feeds
                    .iter()
                    .filter(|f| {
                        self.state
                            .nav_active_category
                            .map_or(true, |c| f.category == c)
                    })
                    .map(|f| f.id)
                    .collect();
                let all_selected = !visible_feeds.is_empty()
                    && visible_feeds
                        .iter()
                        .all(|id| self.state.selected_feeds.contains(id));
                // 全选 / 反选 / 取消选择。
                if ui
                    .button(if all_selected {
                        "取消全选"
                    } else {
                        "全选"
                    })
                    .clicked()
                {
                    if all_selected {
                        for id in &visible_feeds {
                            self.state.selected_feeds.remove(id);
                        }
                    } else {
                        for id in &visible_feeds {
                            self.state.selected_feeds.insert(*id);
                        }
                    }
                }
                if ui.button("反选").clicked() {
                    for id in &visible_feeds {
                        if !self.state.selected_feeds.remove(id) {
                            self.state.selected_feeds.insert(*id);
                        }
                    }
                }
                if ui.button("取消选择").clicked() {
                    self.state.selected_feeds.clear();
                }
                let n = self.state.selected_feeds.len();
                ui.label(RichText::new(format!("已选 {n}")).size(12.0));
                if n > 0 {
                    let sel: Vec<_> = self.state.selected_feeds.iter().copied().collect();
                    let mut move_to: Option<Option<FolderId>> = None;
                    ui.menu_button("移入文件夹", |ui| {
                        for f in &self.state.folders {
                            let fid = f.id;
                            if ui.button(&f.name).clicked() {
                                move_to = Some(Some(fid));
                                ui.close_menu();
                            }
                        }
                        if ui.button("（无文件夹）").clicked() {
                            move_to = Some(None);
                            ui.close_menu();
                        }
                    });
                    if let Some(target) = move_to {
                        self.state.batch_move_feeds(sel.clone(), target);
                    }
                    // 批量代理开关：快速给选中订阅统一开启/关闭代理。
                    if ui.button("开启代理").clicked() {
                        self.state.batch_set_feed_proxy(sel.clone(), true);
                    }
                    if ui.button("关闭代理").clicked() {
                        self.state.batch_set_feed_proxy(sel.clone(), false);
                    }
                    if ui.button(format!("删除选中 ({n})")).clicked() {
                        self.state.batch_delete_feeds(sel);
                    }
                }
            }
        });

        // --- 订阅列表（可滚动） ---
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .max_width(ui.available_width())
            .max_height(ui.available_height())
            .show(ui, |ui| {
                let folders = self.state.folders.clone();
                let active_cat = self.state.nav_active_category;
                let dragging = egui::DragAndDrop::has_any_payload(ui.ctx());

                // 过滤当前分类的订阅
                let feeds: Vec<glean_core::Feed> = self
                    .state
                    .feeds
                    .iter()
                    .filter(|f| active_cat.map_or(true, |c| f.category == c))
                    .cloned()
                    .collect();

                // 拖动中：临时收起所有展开的文件夹，仅显示文件夹以便拖放；
                // 释放后恢复之前的展开状态。
                let was_dragging = self.state.expanded_folders_before_drag.is_some();
                if dragging && !was_dragging {
                    self.state.expanded_folders_before_drag =
                        Some(self.state.expanded_folders.clone());
                    self.state.expanded_folders.clear();
                } else if !dragging && was_dragging {
                    if let Some(saved) = self.state.expanded_folders_before_drag.take() {
                        self.state.expanded_folders = saved;
                    }
                }

                // 无文件夹的订阅（orphans）在前
                if !dragging {
                    for feed in feeds.iter().filter(|f| f.folder_id.is_none()) {
                        if let Some(a) = self.draw_feed_item(ui, feed, &folders, false) {
                            action = Some(a);
                        }
                    }
                }

                // 文件夹分组
                for folder in &folders {
                    let folder_feeds: Vec<glean_core::Feed> = feeds
                        .iter()
                        .filter(|f| f.folder_id == Some(folder.id))
                        .cloned()
                        .collect();
                    if folder_feeds.is_empty() && !dragging {
                        continue;
                    }

                    let expanded = self.state.expanded_folders.contains(&folder.id);
                    let arrow = if expanded { "▾" } else { "▸" };
                    let folder_unread: u64 = folder_feeds
                        .iter()
                        .map(|f| self.state.unread_per_feed.get(&f.id).copied().unwrap_or(0))
                        .sum();
                    let header = if folder_unread > 0 {
                        format!("{} 📁 {} ({})", arrow, folder.name, folder_unread)
                    } else {
                        format!("{} 📁 {}", arrow, folder.name)
                    };

                    let folder_id = folder.id;
                    // 文件夹作为拖放目标
                    let frame = if dragging {
                        Frame::new()
                            .stroke(Stroke::new(1.0_f32, ui.visuals().selection.stroke.color))
                    } else {
                        Frame::new()
                    };
                    let mut inner_action: Option<FeedRowAction> = None;
                    let (resp_inner, dropped) = ui.dnd_drop_zone::<i64, _>(frame, |ui| {
                        ui.set_min_width(ui.available_width());
                        let width = ui.available_width();
                        let r = ui.add_sized(
                            [width, ui.spacing().interact_size.y],
                            egui::Button::new(RichText::new(header.clone()).strong())
                                .truncate()
                                .selected(false),
                        );
                        if expanded {
                            for feed in &folder_feeds {
                                if let Some(a) = self.draw_feed_item(ui, feed, &folders, true) {
                                    inner_action = Some(a);
                                }
                            }
                        }
                        r
                    });
                    if let Some(a) = inner_action {
                        action = Some(a);
                    }
                    if resp_inner.inner.clicked() {
                        if expanded {
                            self.state.expanded_folders.remove(&folder_id);
                        } else {
                            self.state.expanded_folders.insert(folder_id);
                        }
                    }
                    if let Some(payload) = dropped {
                        action = Some(FeedRowAction::MoveFolder(
                            glean_core::FeedId(*payload.as_ref()),
                            Some(folder_id),
                        ));
                    }
                }

                // 拖动中：空白区域作为"移出文件夹"的目标
                if dragging {
                    let frame = Frame::new()
                        .stroke(Stroke::new(1.0_f32, ui.visuals().selection.stroke.color));
                    let (_resp, dropped) = ui.dnd_drop_zone::<i64, _>(frame, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.set_min_height(40.0);
                        ui.vertical_centered(|ui| {
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new("拖到此处移出文件夹")
                                    .size(12.0)
                                    .color(ui.visuals().hyperlink_color),
                            );
                            ui.add_space(10.0);
                        });
                    });
                    if let Some(payload) = dropped {
                        action = Some(FeedRowAction::MoveFolder(
                            glean_core::FeedId(*payload.as_ref()),
                            None,
                        ));
                    }
                }
            });

        // Apply actions after the closure borrows are released.
        match action {
            Some(FeedRowAction::Click(id)) => {
                self.state.set_filter(EntryFilter::Feed(id));
            }
            Some(FeedRowAction::Delete(id)) => {
                self.state.delete_feed(id);
            }
            Some(FeedRowAction::Rename(id)) => {
                if let Some(f) = self.state.feeds.iter().find(|f| f.id == id) {
                    self.state.rename_feed = Some((id, f.title.clone()));
                }
            }
            Some(FeedRowAction::EditUrl(id)) => {
                if let Some(f) = self.state.feeds.iter().find(|f| f.id == id) {
                    self.state.edit_feed_url = Some((id, f.feed_url.clone()));
                }
            }
            Some(FeedRowAction::MoveFolder(feed_id, folder_id)) => {
                self.state.move_feed_to_folder(feed_id, folder_id);
            }
            Some(FeedRowAction::ToggleMute(id)) => {
                self.state.toggle_mute_feed(id);
            }
            Some(FeedRowAction::SetCategory(id, category)) => {
                self.state
                    .dispatch(AppCommand::SetFeedCategory { id, category });
            }
            Some(FeedRowAction::ToggleProxy(id)) => {
                self.state.dispatch(AppCommand::ToggleFeedProxy { id });
            }
            Some(FeedRowAction::MarkRead(id)) => {
                self.state.mark_all_read(Some(id));
            }
            None => {}
        }
        if do_create_folder {
            let name = self.state.new_folder_input.trim().to_string();
            if !name.is_empty() {
                self.state.create_folder(name);
                self.state.new_folder_input.clear();
            }
        }
    }

    /// 渲染单个订阅行（favicon + 标题 + 未读/静音/错误标记），返回用户动作。
    fn draw_feed_item(
        &mut self,
        ui: &mut Ui,
        feed: &glean_core::Feed,
        folders: &[glean_core::Folder],
        indent: bool,
    ) -> Option<FeedRowAction> {
        let multi = self.state.feed_multi_select;
        let selected = if multi {
            self.state.selected_feeds.contains(&feed.id)
        } else {
            matches!(self.state.filter, EntryFilter::Feed(id) if id == feed.id)
        };
        let unread = self
            .state
            .unread_per_feed
            .get(&feed.id)
            .copied()
            .unwrap_or(0);
        let mut title = feed.title.clone();
        if feed.muted {
            title.push_str(" 🔇");
        }
        if feed.last_error.is_some() {
            title.push_str(" ⚠");
        }
        if unread > 0 {
            title.push_str(&format!(" ({unread})"));
        }
        let rich = if unread > 0 {
            RichText::new(title).strong()
        } else {
            RichText::new(title)
        };
        let feed_id_for_drag = feed.id;
        let drag_id = ui.id().with(("feed_drag", feed.id));

        // 多选模式下不使用 dnd_drag_source，因为它的 Sense::drag() 会拦截
        // 鼠标事件，导致内部 button/label 的 clicked() 无法触发。
        // 复选框使用 selectable_label（与文字同高），保证多选/非多选行高一致。
        let row_height = ui.spacing().interact_size.y;
        let (resp, row_clicked) = if multi {
            let width = ui.available_width();
            ui.set_min_width(width);
            ui.set_max_width(width);
            let row = ui.horizontal(|ui| {
                ui.set_min_height(row_height);
                if indent {
                    ui.add_space(12.0);
                }
                if let Some(tex) = self.favicons.get(&feed.id) {
                    ui.add(egui::Image::new(tex).fit_to_exact_size(Vec2::splat(FAVICON_SIZE)));
                } else {
                    ui.label("🌐");
                }
                let check_text = if selected { "☑" } else { "☐" };
                let check = ui.selectable_label(selected, check_text);
                let mut clicked = check.clicked();
                let text = ui.add(
                    egui::Label::new(rich)
                        .truncate()
                        .wrap_mode(egui::TextWrapMode::Truncate)
                        .sense(egui::Sense::click()),
                );
                clicked |= text.clicked();
                clicked
            });
            (row.response, row.inner)
        } else {
            let row = ui.dnd_drag_source(drag_id, feed_id_for_drag.0, |ui| {
                let width = ui.available_width();
                ui.set_min_width(width);
                ui.set_max_width(width);
                ui.horizontal(|ui| {
                    ui.set_min_height(row_height);
                    if indent {
                        ui.add_space(12.0);
                    }
                    if let Some(tex) = self.favicons.get(&feed.id) {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(Vec2::splat(FAVICON_SIZE)));
                    } else {
                        ui.label("🌐");
                    }
                    // 非多选时也渲染勾选框占位：宽度为 0（不可见），
                    // 使用与多选相同的 SelectableLabel，高度一致，行高统一。
                    let check_text = if selected { "☑" } else { "☐" };
                    ui.add_sized(
                        egui::Vec2::new(0.0, row_height),
                        egui::SelectableLabel::new(selected, check_text),
                    );
                    ui.add(
                        egui::Label::new(rich)
                            .truncate()
                            .wrap_mode(egui::TextWrapMode::Truncate)
                            .sense(egui::Sense::click()),
                    )
                })
            });
            // 悬停时恢复默认指针，只在拖拽过程中显示 Grab 手型。
            let resp = row.response.on_hover_cursor(egui::CursorIcon::Default);
            let resp = resp.interact(egui::Sense::click());
            (resp, false)
        };

        let mut action = None;
        if resp.clicked() || row_clicked {
            if multi {
                if selected {
                    self.state.selected_feeds.remove(&feed.id);
                } else {
                    self.state.selected_feeds.insert(feed.id);
                }
            } else {
                action = Some(FeedRowAction::Click(feed.id));
            }
        }
        self.draw_feed_context_menu(&resp, feed, folders, &mut action);
        action
    }

    fn draw_feed_context_menu(
        &mut self,
        resp: &egui::Response,
        feed: &glean_core::Feed,
        folders: &[glean_core::Folder],
        action: &mut Option<FeedRowAction>,
    ) {
        resp.context_menu(|ui| {
            // 分类子菜单（导航栏分组依据）。
            ui.menu_button("分类", |ui| {
                for category in FEED_CATEGORIES {
                    if ui
                        .selectable_label(feed.category == category, category.label())
                        .clicked()
                    {
                        *action = Some(FeedRowAction::SetCategory(feed.id, category));
                        ui.close_menu();
                    }
                }
            });
            // 代理开关：开启时走设置页配置的 HTTP 代理，关闭时直连。
            let mut use_proxy = feed.use_proxy;
            if ui.checkbox(&mut use_proxy, "使用代理").changed() {
                *action = Some(FeedRowAction::ToggleProxy(feed.id));
                ui.close_menu();
            }
            if ui.button("重命名").clicked() {
                *action = Some(FeedRowAction::Rename(feed.id));
                ui.close_menu();
            }
            if ui.button("编辑 URL").clicked() {
                *action = Some(FeedRowAction::EditUrl(feed.id));
                ui.close_menu();
            }
            let mute_label = if feed.muted { "取消静音" } else { "静音" };
            if ui.button(mute_label).clicked() {
                *action = Some(FeedRowAction::ToggleMute(feed.id));
                ui.close_menu();
            }
            if ui.button("标记全部已读").clicked() {
                *action = Some(FeedRowAction::MarkRead(feed.id));
                ui.close_menu();
            }
            if ui.button("删除订阅").clicked() {
                *action = Some(FeedRowAction::Delete(feed.id));
                ui.close_menu();
            }
            ui.separator();
            // Move to folder sub-menu.
            ui.menu_button("移动到文件夹", |ui| {
                // Option to remove from folder.
                if feed.folder_id.is_some() {
                    if ui.button("（无文件夹）").clicked() {
                        *action = Some(FeedRowAction::MoveFolder(feed.id, None));
                        ui.close_menu();
                    }
                }
                for folder in folders {
                    let already = feed.folder_id == Some(folder.id);
                    if ui
                        .add_enabled(!already, egui::Button::new(&folder.name))
                        .clicked()
                    {
                        *action = Some(FeedRowAction::MoveFolder(feed.id, Some(folder.id)));
                        ui.close_menu();
                    }
                }
                ui.separator();
                let te = egui::TextEdit::singleline(&mut self.state.new_folder_input)
                    .id(egui::Id::new("ctx_new_folder"))
                    .desired_width(100.0)
                    .hint_text("新文件夹");
                let r = ui.add(te);
                if r.clicked() || r.gained_focus() {
                    self.state.reader.reclaim_shell_focus();
                    r.request_focus();
                }
                if ui.button("创建并移入").clicked() {
                    // 保持既有占位行为（移入“无文件夹”）；真正的建文件夹走“＋ 新建文件夹”菜单。
                    *action = Some(FeedRowAction::MoveFolder(feed.id, None));
                    ui.close_menu();
                }
            });
            // Refresh interval sub-menu.
            ui.menu_button("刷新间隔", |ui| {
                let current = feed.refresh_interval_secs;
                for &secs in &[0, 300, 900, 1800, 3600, 21600] {
                    let label = if secs == 0 {
                        "默认".into()
                    } else {
                        format!("{}分钟", secs / 60)
                    };
                    if ui.selectable_label(current == secs, label).clicked() {
                        self.state.set_feed_refresh_interval(feed.id, secs);
                        ui.close_menu();
                    }
                }
            });
            // Show feed error details.
            if let Some(err) = &feed.last_error {
                ui.separator();
                ui.colored_label(
                    Color32::from_rgb(220, 80, 60),
                    RichText::new(format!("错误: {err}")).size(13.0),
                );
            }
        });
    }

    fn draw_list_contents(&mut self, ui: &mut Ui) {
        // Virtual scrolling: only render visible rows.
        // row_height must match the actual rendered row height (excluding
        // item_spacing) to avoid phantom blank space at scroll bottom.
        // The row height is dominated by the thumbnail/space allocation.
        let thumb_size = self.state.config.thumbnail_size;
        let row_height = thumb_size;
        let num_rows = self.state.entries.len();
        egui::ScrollArea::vertical()
            .max_width(ui.available_width())
            .max_height(ui.available_height())
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show_rows(ui, row_height, num_rows, |ui, row_range| {
                let current = self.state.selected;
                let mut clicked = None;
                for i in row_range {
                    let entry = &self.state.entries[i];
                    let read_mark = if entry.is_read { "已读" } else { "未读" };
                    let star = if entry.is_starred { "★" } else { "" };
                    let cache = if entry.has_content { "" } else { " ⇊" };
                    let label = format!("[{read_mark}]{star}{cache} {}", entry.title);
                    let rich = if entry.is_read {
                        RichText::new(label).weak()
                    } else {
                        RichText::new(label).strong()
                    };
                    let selected = Some(i) == current;
                    let thumb = self.thumbnails.get(&entry.id).cloned();
                    // 整行：缩略图 + 文字，水平居左，垂直居中。
                    let row_width = ui.available_width();
                    ui.set_min_width(row_width);
                    ui.set_max_width(row_width);
                    let row = Frame::new()
                        .fill(if selected {
                            ui.visuals().selection.bg_fill
                        } else {
                            Color32::TRANSPARENT
                        })
                        .inner_margin(Margin::ZERO)
                        .show(ui, |ui| {
                            ui.set_min_width(row_width);
                            ui.set_max_width(row_width);
                            ui.horizontal_top(|ui| {
                                if let Some(tex) = &thumb {
                                    ui.add(
                                        egui::Image::new(tex)
                                            .fit_to_exact_size(Vec2::splat(thumb_size)),
                                    );
                                } else {
                                    ui.allocate_space(Vec2::splat(thumb_size));
                                }
                                let text_width =
                                    (row_width - thumb_size - ui.spacing().item_spacing.x).max(1.0);
                                ui.allocate_ui_with_layout(
                                    egui::Vec2::new(text_width, thumb_size),
                                    egui::Layout::left_to_right(egui::Align::TOP),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(rich)
                                                .truncate()
                                                .wrap_mode(egui::TextWrapMode::Truncate),
                                        )
                                    },
                                )
                                .inner
                            })
                        });
                    // Frame 的 response 默认无 click sense，显式加上使整行可点。
                    let resp = row
                        .response
                        .on_hover_cursor(egui::CursorIcon::Default)
                        .interact(egui::Sense::click());
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                    // Capture entry data for context menu (avoids borrow conflict).
                    let eid = entry.id;
                    let is_read = entry.is_read;
                    let is_starred = entry.is_starred;
                    let url = entry.url.clone();
                    // Right-click context menu.
                    resp.context_menu(|ui| {
                        if is_read {
                            if ui.button("标记为未读").clicked() {
                                self.state.dispatch(AppCommand::MarkRead {
                                    id: eid,
                                    read: false,
                                });
                                ui.close_menu();
                            }
                        } else {
                            if ui.button("标记为已读").clicked() {
                                self.state.dispatch(AppCommand::MarkRead {
                                    id: eid,
                                    read: true,
                                });
                                ui.close_menu();
                            }
                        }
                        if is_starred {
                            if ui.button("取消星标").clicked() {
                                self.state.dispatch(AppCommand::ToggleStar { id: eid });
                                ui.close_menu();
                            }
                        } else {
                            if ui.button("加星标").clicked() {
                                self.state.dispatch(AppCommand::ToggleStar { id: eid });
                                ui.close_menu();
                            }
                        }
                        if let Some(url) = url.as_deref() {
                            if ui.button("在浏览器中打开").clicked() {
                                let _ = open::that(url);
                                ui.close_menu();
                            }
                        }
                    });
                }
                if let Some(i) = clicked {
                    self.state.select_index(i);
                }
            });
        // Footer: total entry count (fills remaining space, no blank gap).
        let color = ui.visuals().weak_text_color();
        ui.add_space(2.0);
        ui.label(
            RichText::new(format!("共 {} 条", num_rows))
                .size(12.0)
                .color(color),
        );
    }
}

/// 应用全局样式：字体（整体调大）、现代化间距/圆角、主题强调色。
/// egui 默认 Body/Button 14px、Small 10px，高分辨率下说明文字过小难读，
/// 这里统一放大并把主题色注入选中/悬停/链接等视觉元素。
fn apply_style(ctx: &egui::Context, dark: bool, accent: AccentColor) {
    use egui::{FontFamily, FontId, TextStyle};
    let (r, g, b) = accent.rgb(dark);
    let accent_c = Color32::from_rgb(r, g, b);

    let mut style = (*ctx.style()).clone();

    // 字体：正文 15.5 / 按钮 15 / 说明文字 12.5（原默认 14/14/10）。
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(22.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(15.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(15.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(12.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.5, FontFamily::Monospace),
        ),
    ]
    .into();

    // 现代化留白与间距。
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style.spacing.window_margin = Margin::same(14);
    style.spacing.menu_margin = Margin::same(8);
    style.spacing.indent = 20.0;

    // 滚动条：让导航/列表的滚动条始终可见、高对比、可拖动。
    // egui 默认是悬浮(floating)式滚动条,dormant 时几乎透明,浅色主题下难以察觉。
    // 改为常驻厚实、前景色(文字色)高对比,配合各 ScrollArea 的 AlwaysVisible。
    {
        let s = &mut style.spacing.scroll;
        s.floating = false;
        s.foreground_color = true;
        s.bar_width = 12.0;
        s.floating_width = 12.0;
        s.handle_min_length = 24.0;
        s.bar_inner_margin = 3.0;
        // 滚动条与外层容器（列分隔条）之间不留空白，使两者贴在一起。
        s.bar_outer_margin = 0.0;
    }

    // 视觉：圆角 + 主题色。
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.hyperlink_color = accent_c;
    visuals.selection.bg_fill = accent_c.linear_multiply(if dark { 0.55 } else { 0.30 });
    visuals.selection.stroke = Stroke::new(1.0_f32, accent_c);
    visuals.widgets.hovered.weak_bg_fill = accent_c.linear_multiply(if dark { 0.25 } else { 0.12 });
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, accent_c.linear_multiply(0.6));
    visuals.widgets.active.weak_bg_fill = accent_c.linear_multiply(if dark { 0.45 } else { 0.25 });
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, accent_c);
    visuals.widgets.open.weak_bg_fill = accent_c.linear_multiply(if dark { 0.30 } else { 0.15 });

    let radius = egui::CornerRadius::same(6);
    visuals.widgets.noninteractive.corner_radius = radius;
    visuals.widgets.inactive.corner_radius = radius;
    visuals.widgets.hovered.corner_radius = radius;
    visuals.widgets.active.corner_radius = radius;
    visuals.widgets.open.corner_radius = radius;
    visuals.window_corner_radius = egui::CornerRadius::same(10);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);

    style.visuals = visuals;
    ctx.set_style(style);
}

/// 说明/提示文字：12.5px + 正文色的 72% 透明度。
/// 替代旧的 `.small().weak()`（10px 极淡灰），深浅主题下都清晰可读。
fn hint(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    let color = ui.visuals().strong_text_color().gamma_multiply(0.72);
    ui.label(RichText::new(text).size(12.5).color(color))
}

/// 设置窗体的分组标题（16px 加粗 + 适度间距）。
fn settings_heading(ui: &mut Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).size(16.0).strong());
    ui.add_space(4.0);
}

fn paint_column_bg(ui: &Ui, rect: egui::Rect, fill: Color32, stroke: Color32) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, fill);
    painter.line_segment(
        [rect.right_top(), rect.right_bottom()],
        Stroke::new(1.0_f32, stroke),
    );
}

fn column_contents(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(title).strong());
    });
    ui.separator();
    ui.add_space(2.0);
    // Give the body the full remaining height.
    add(ui);
}

fn splitter_drag(
    ui: &mut Ui,
    ctx: &egui::Context,
    x: f32,
    full: egui::Rect,
    id: &'static str,
    mut on_drag: impl FnMut(f32),
) -> (f32, bool) {
    let hit = SPLIT_HIT;
    let rect = egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(hit, full.height()));
    let resp = ui.interact(rect, ui.id().with(id), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        ui.painter().rect_filled(
            rect,
            0.0,
            Color32::from_rgba_unmultiplied(100, 140, 200, 120),
        );
    } else {
        ui.painter().rect_filled(
            rect,
            0.0,
            Color32::from_rgba_unmultiplied(120, 120, 120, 40),
        );
    }
    let dragging = resp.dragged();
    if dragging {
        if let Some(pos) = ctx.pointer_latest_pos() {
            on_drag(pos.x);
        }
    }
    (hit, dragging)
}
