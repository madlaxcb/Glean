//! WebView2 reader host for M0 spike (Windows).
//!
//! Single-instance webview; bounds track the egui reader rect.
//! Parent HWND: prefer a visible top-level window whose title contains "Glean".

use glean_core::ReaderHostMode;

#[cfg(windows)]
mod win {
    use super::ReaderHostMode;
    use egui::Rect;
    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
        RawWindowHandle, Win32WindowHandle, WindowHandle, WindowsDisplayHandle,
    };
    use std::num::NonZeroIsize;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
    use windows::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClientRect, GetWindow, GetWindowTextW, GetWindowThreadProcessId,
        SetWindowPos, GW_OWNER, HWND_TOP, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SW_SHOWNORMAL,
    };
    use wry::{
        dpi::{LogicalPosition, LogicalSize, Position, Size},
        Rect as WryRect, WebView, WebViewBuilder,
    };

    struct ParentHwnd(HWND);

    // Spike: HWND is owned by the eframe window for process lifetime.
    unsafe impl Send for ParentHwnd {}
    unsafe impl Sync for ParentHwnd {}

    impl HasWindowHandle for ParentHwnd {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let HWND(ptr) = self.0;
            let hwnd = ptr as isize;
            let nz = NonZeroIsize::new(hwnd).ok_or(HandleError::NotSupported)?;
            let win32 = Win32WindowHandle::new(nz);
            let raw = RawWindowHandle::Win32(win32);
            // SAFETY: parent window lives as long as the spike app.
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }

    impl HasDisplayHandle for ParentHwnd {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            let raw = RawDisplayHandle::Windows(WindowsDisplayHandle::new());
            Ok(unsafe { DisplayHandle::borrow_raw(raw) })
        }
    }

    pub struct ReaderHostInner {
        webview: Option<WebView>,
        last_html: String,
        mode: ReaderHostMode,
        parent: Option<HWND>,
        dark_title: bool,
        pending_theme: Option<bool>,
        /// Deferred NC repaint: SetWindowPos(SWP_FRAMECHANGED) on next sync_bounds.
        needs_nc_repaint: bool,
        hidden: bool,
        last_rect: Option<Rect>,
        last_ppp: f32,
    }

    impl ReaderHostInner {
        pub fn new() -> Self {
            Self {
                webview: None,
                last_html: String::new(),
                mode: ReaderHostMode::ChildEmbed,
                parent: None,
                dark_title: false,
                pending_theme: None,
                needs_nc_repaint: false,
                hidden: false,
                last_rect: None,
                last_ppp: 1.0,
            }
        }

        pub fn set_dark_title(&mut self, dark: bool) {
            self.dark_title = dark;
        }

        pub fn set_mode(&mut self, mode: ReaderHostMode) {
            if self.mode != mode {
                self.mode = mode;
                self.webview = None;
            }
        }

        pub fn shutdown(&mut self) {
            self.webview = None;
            self.parent = None;
        }

        pub fn show_html(&mut self, html: &str) {
            self.last_html = html.to_string();
            if let Some(wv) = self.webview.as_ref() {
                if let Err(e) = wv.load_html(html) {
                    eprintln!("load_html failed: {e}");
                }
            }
        }

        pub fn set_titlebar_dark(&mut self, dark: bool) {
            self.dark_title = dark;
            // Apply DWM attribute now.  Defer NC repaint to next sync_bounds
            // so it runs AFTER eframe processes the async SetTheme command.
            if let Some(hwnd) = self.parent {
                apply_dwm_dark(hwnd, dark);
            }
            self.needs_nc_repaint = true;
        }

        /// Record the desired reader theme. With JavaScript disabled (§7.2),
        /// the theme cannot be flipped live via `evaluate_script`; the caller
        /// regenerates the document with `reader_document(dark=…)` and calls
        /// `show_html` to reload it. This stores the value so that a later
        /// `ensure_attached` knows the intended theme (the rebuilt WebView
        /// already loads `last_html`, which carries the correct `data-theme`).
        pub fn apply_theme(&mut self, dark: bool) {
            self.pending_theme = Some(dark);
        }

        /// Pull Win32 keyboard focus from WebView2 child back to the main window.
        pub fn reclaim_shell_focus(&mut self) {
            let Some(parent) = self.parent else {
                return;
            };
            unsafe {
                let focus = GetFocus();
                if focus == parent {
                    return;
                }
                let _ = SetFocus(parent);
            }
        }

        pub fn ensure_attached(
            &mut self,
            mode: ReaderHostMode,
            reader_rect: Rect,
            pixels_per_point: f32,
        ) -> Result<(), String> {
            self.mode = mode;
            if self.webview.is_some() {
                return Ok(());
            }

            if reader_rect.width() < 2.0 || reader_rect.height() < 2.0 {
                return Err("reader rect not ready".into());
            }

            let parent = match self.parent {
                Some(h) => h,
                None => {
                    let h = find_glean_main_hwnd().ok_or_else(|| {
                        "Glean main HWND not found yet (retry next frame)".to_string()
                    })?;
                    self.parent = Some(h);
                    apply_dwm_dark(h, self.dark_title);
                    h
                }
            };

            let html = if self.last_html.is_empty() {
                themed_placeholder(self.dark_title)
            } else {
                self.last_html.clone()
            };

            let bounds = if self.hidden {
                WryRect {
                    position: Position::Logical(LogicalPosition::new(-10000.0, -10000.0)),
                    size: Size::Logical(LogicalSize::new(1.0, 1.0)),
                }
            } else {
                rect_to_wry(reader_rect, pixels_per_point)
            };
            let parent_wrap = ParentHwnd(parent);

            let webview = WebViewBuilder::new()
                .with_html(&html)
                .with_bounds(bounds)
                // §7.2 / §8.6 critical security default: no JavaScript in the
                // reader WebView. Reader content is sanitized HTML only; theme
                // is baked into the document via `reader_document(dark=…)` and
                // applied by `load_html`, so no scripting is needed.
                .with_javascript_disabled()
                .with_navigation_handler(|uri: String| {
                    if uri.starts_with("http://") || uri.starts_with("https://") {
                        let _ = open_external(&uri);
                        false
                    } else if uri.starts_with("data:")
                        || uri.starts_with("about:")
                        || uri.starts_with("file:")
                    {
                        true
                    } else {
                        false
                    }
                })
                .build_as_child(&parent_wrap)
                .map_err(|e| format!("WebView build_as_child: {e}"))?;

            self.webview = Some(webview);
            // Re-record any pending theme so it stays consistent; with JS off
            // the webview already loaded `last_html` which carries the theme.
            if let Some(dark) = self.pending_theme.take() {
                self.apply_theme(dark);
            }
            let _ = mode;
            Ok(())
        }

        pub fn sync_bounds(&mut self, reader_rect: Rect, pixels_per_point: f32) {
            // Re-apply DWM dark attribute every frame as a safety net.
            // eframe/winit may reset it when processing theme changes.
            if let Some(hwnd) = self.parent {
                apply_dwm_dark(hwnd, self.dark_title);
                if self.needs_nc_repaint {
                    trigger_nc_repaint(hwnd);
                    self.needs_nc_repaint = false;
                }
            }
            let Some(wv) = self.webview.as_ref() else {
                return;
            };
            if reader_rect.width() < 2.0 || reader_rect.height() < 2.0 {
                return;
            }
            self.last_rect = Some(reader_rect);
            self.last_ppp = pixels_per_point;
            if self.hidden {
                let hidden_rect = WryRect {
                    position: Position::Logical(LogicalPosition::new(-10000.0, -10000.0)),
                    size: Size::Logical(LogicalSize::new(1.0, 1.0)),
                };
                let _ = wv.set_bounds(hidden_rect);
            } else if let Err(e) = wv.set_bounds(rect_to_wry(reader_rect, pixels_per_point)) {
                eprintln!("set_bounds failed: {e}");
            }
            // With JavaScript disabled, the live theme-flip via evaluate_script
            // is gone; the caller reloads the themed document via show_html.
            // Just drop any stale pending theme so it doesn't accumulate.
            let _ = self.pending_theme.take();
        }

        /// Hide/show the WebView2 so egui popups are not occluded.
        pub fn set_hidden(&mut self, hidden: bool) {
            if self.hidden == hidden {
                return;
            }
            self.hidden = hidden;
            if let (Some(wv), Some(rect)) = (self.webview.as_ref(), self.last_rect) {
                if hidden {
                    let hidden_rect = WryRect {
                        position: Position::Logical(LogicalPosition::new(-10000.0, -10000.0)),
                        size: Size::Logical(LogicalSize::new(1.0, 1.0)),
                    };
                    let _ = wv.set_bounds(hidden_rect);
                } else {
                    let _ = wv.set_bounds(rect_to_wry(rect, self.last_ppp));
                }
            }
        }
    }

    /// Write a diagnostic line to %TEMP%\glean_dwm.log.
    fn dwm_log(msg: &str) {
        use std::io::Write;
        let path = std::env::temp_dir().join("glean_dwm.log");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
        {
            let _ = writeln!(f, "{msg}");
        }
    }

    /// Set ONLY the DWM attribute (cheap, safe to call every frame).
    fn apply_dwm_dark(hwnd: HWND, dark: bool) {
        use windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE;
        let value: u32 = if dark { 1 } else { 0 };
        for code in [20i32, 19i32] {
            let attr = DWMWINDOWATTRIBUTE(code);
            let result = unsafe {
                DwmSetWindowAttribute(
                    hwnd,
                    attr,
                    &value as *const u32 as *const _,
                    std::mem::size_of::<u32>() as u32,
                )
            };
            if code == 20 {
                let status = match &result {
                    Ok(()) => "OK".to_string(),
                    Err(e) => format!("ERR: {e}"),
                };
                let title = window_title(hwnd);
                dwm_log(&format!(
                    "apply_dwm_dark hwnd={:?} title=\"{}\" dark={} -> {}",
                    hwnd.0, title, dark, status,
                ));
            }
        }
    }

    /// Force non-client area repaint so the DWM title bar picks up the new
    /// dark/light attribute immediately. `SetWindowPos(SWP_FRAMECHANGED)` alone
    /// is insufficient — it sends `WM_NCCALCSIZE` but doesn't always trigger a
    /// DWM title-bar redraw. `RedrawWindow(RDW_FRAME | RDW_INVALIDATE | RDW_UPDATENOW)`
    /// invalidates the window frame and repaints immediately.
    fn trigger_nc_repaint(hwnd: HWND) {
        use windows::Win32::Graphics::Gdi::{
            RedrawWindow, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW,
        };
        let rdw_result =
            unsafe { RedrawWindow(hwnd, None, None, RDW_FRAME | RDW_INVALIDATE | RDW_UPDATENOW) };
        // Also send SWP_FRAMECHANGED as belt-and-suspenders.
        let swp_result = unsafe {
            SetWindowPos(
                hwnd,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )
        };
        let title = window_title(hwnd);
        dwm_log(&format!(
            "trigger_nc_repaint hwnd={:?} title=\"{}\" rdw={:?} swp={:?}",
            hwnd.0, title, rdw_result, swp_result,
        ));
    }

    fn window_title(hwnd: HWND) -> String {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }

    fn client_area(hwnd: HWND) -> i32 {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let w = (rc.right - rc.left).max(0);
        let h = (rc.bottom - rc.top).max(0);
        w.saturating_mul(h)
    }

    pub fn find_glean_main_hwnd() -> Option<HWND> {
        struct State {
            pid: u32,
            console: HWND,
            best_glean: Option<(HWND, i32)>,
            best_any: Option<(HWND, i32)>,
        }

        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let state = &mut *(lparam.0 as *mut State);

            if hwnd == state.console {
                return TRUE;
            }

            let mut wnd_pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut wnd_pid));
            if wnd_pid != state.pid {
                return TRUE;
            }
            // Note: intentionally NOT skipping hidden windows here.
            // When the main window is hidden to the tray (ShowWindow(SW_HIDE)),
            // IsWindowVisible returns false. The tray callback needs to find the
            // HWND to restore it. Other filters (no owner, large client area,
            // title match) are sufficient to identify the main window.
            if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
                let HWND(p) = owner;
                if !p.is_null() {
                    return TRUE;
                }
            }

            let area = client_area(hwnd);
            if area < 10_000 {
                return TRUE;
            }

            let title = window_title(hwnd);
            let is_glean = title.contains("Glean") || title.contains("拾光");
            if is_glean {
                match state.best_glean {
                    Some((_, a)) if a >= area => {}
                    _ => state.best_glean = Some((hwnd, area)),
                }
            } else {
                match state.best_any {
                    Some((_, a)) if a >= area => {}
                    _ => state.best_any = Some((hwnd, area)),
                }
            }
            TRUE
        }

        let console = unsafe { GetConsoleWindow() };
        let mut state = State {
            pid: std::process::id(),
            console,
            best_glean: None,
            best_any: None,
        };
        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut State as isize));
        }

        let result = state
            .best_glean
            .or(state.best_any)
            .map(|(h, _)| h)
            .filter(|h| {
                let HWND(p) = *h;
                !p.is_null()
            });

        if let Some(h) = result {
            let title = window_title(h);
            dwm_log(&format!(
                "find_glean_main_hwnd -> hwnd={:?} title=\"{}\"",
                h.0, title,
            ));
        } else {
            dwm_log("find_glean_main_hwnd -> None");
        }

        result
    }

    fn rect_to_wry(rect: Rect, _ppp: f32) -> WryRect {
        WryRect {
            position: Position::Logical(LogicalPosition::new(
                f64::from(rect.min.x),
                f64::from(rect.min.y),
            )),
            size: Size::Logical(LogicalSize::new(
                f64::from(rect.width()).max(1.0),
                f64::from(rect.height()).max(1.0),
            )),
        }
    }

    /// Minimal themed placeholder shown before any article is opened.
    /// Uses the same `data-theme` + CSS variable approach as `reader_document`.
    pub fn themed_placeholder(dark: bool) -> String {
        let theme_attr = if dark { "dark" } else { "light" };
        format!(
            r#"<!DOCTYPE html>
<html lang="zh-CN" data-theme="{theme_attr}">
<head><meta charset="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1"/>
<style>
  html[data-theme="light"] {{ --bg: #F7F7F5; --fg: #1C1C1E; --muted: #6C6C70; }}
  html[data-theme="dark"]  {{ --bg: #1C1C1E; --fg: #F2F2F7; --muted: #8E8E93; }}
  html, body {{ margin: 0; padding: 0; background: var(--bg); color: var(--fg);
    font-family: "Segoe UI", "Microsoft YaHei UI", sans-serif; }}
  main {{ max-width: 42rem; margin: 0 auto; padding: 1.25rem 1.5rem; }}
  .hint {{ color: var(--muted); font-size: 0.92rem; }}
</style></head>
<body><main><p class="hint">Glean · 选择一篇文章开始阅读</p></main></body>
</html>"#,
            theme_attr = theme_attr,
        )
    }

    /// Open https links without spawning a visible cmd.exe (unlike `cmd /C start`).
    fn open_external(uri: &str) -> windows::core::Result<()> {
        let mut wide: Vec<u16> = uri.encode_utf16().collect();
        wide.push(0);
        let operation: Vec<u16> = "open\0".encode_utf16().collect();
        unsafe {
            let h = ShellExecuteW(
                None,
                PCWSTR(operation.as_ptr()),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
            // ShellExecute returns > 32 on success (as HINSTANCE value).
            if (h.0 as isize) <= 32 {
                return Err(windows::core::Error::from_win32());
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
pub struct ReaderHost {
    inner: win::ReaderHostInner,
}

#[cfg(windows)]
impl ReaderHost {
    pub fn new() -> Self {
        Self {
            inner: win::ReaderHostInner::new(),
        }
    }

    pub fn set_dark_title(&mut self, dark: bool) {
        self.inner.set_dark_title(dark);
    }

    pub fn set_mode(&mut self, mode: ReaderHostMode) {
        self.inner.set_mode(mode);
    }

    pub fn shutdown(&mut self) {
        self.inner.shutdown();
    }

    pub fn show_html(&mut self, html: &str) {
        self.inner.show_html(html);
    }

    /// Reload the reader with a themed placeholder (used when no article is
    /// open but the theme changed, so the empty reader area follows the theme).
    pub fn show_placeholder(&mut self, dark: bool) {
        let html = win::themed_placeholder(dark);
        self.inner.show_html(&html);
    }

    pub fn set_titlebar_dark(&mut self, dark: bool) {
        self.inner.set_titlebar_dark(dark);
    }

    /// Switch reader content theme instantly via JS (no document reload).
    pub fn apply_theme(&mut self, dark: bool) {
        self.inner.apply_theme(dark);
    }

    pub fn reclaim_shell_focus(&mut self) {
        self.inner.reclaim_shell_focus();
    }

    pub fn set_hidden(&mut self, hidden: bool) {
        self.inner.set_hidden(hidden);
    }

    pub fn ensure_attached(
        &mut self,
        mode: ReaderHostMode,
        reader_rect: egui::Rect,
        pixels_per_point: f32,
    ) -> Result<(), String> {
        self.inner
            .ensure_attached(mode, reader_rect, pixels_per_point)
    }

    pub fn sync_bounds(&mut self, reader_rect: egui::Rect, pixels_per_point: f32) {
        self.inner.sync_bounds(reader_rect, pixels_per_point);
    }
}

/// Find the main Glean window's HWND (Windows only).
/// Used by the tray to restore the window directly via Win32 API,
/// bypassing the egui event loop (which doesn't wake for hidden windows
/// because `RedrawWindow(RDW_INTERNALPAINT)` is ignored for invisible windows).
#[cfg(windows)]
pub fn find_main_hwnd() -> Option<windows::Win32::Foundation::HWND> {
    win::find_glean_main_hwnd()
}

#[cfg(not(windows))]
pub struct ReaderHost;

#[cfg(not(windows))]
impl ReaderHost {
    pub fn new() -> Self {
        Self
    }
    pub fn set_mode(&mut self, _mode: ReaderHostMode) {}
    pub fn shutdown(&mut self) {}
    pub fn show_html(&mut self, _html: &str) {}
    pub fn show_placeholder(&mut self, _dark: bool) {}
    pub fn set_titlebar_dark(&mut self, _dark: bool) {}
    pub fn set_dark_title(&mut self, _dark: bool) {}
    pub fn apply_theme(&mut self, _dark: bool) {}
    pub fn reclaim_shell_focus(&mut self) {}
    pub fn set_hidden(&mut self, _hidden: bool) {}
}
