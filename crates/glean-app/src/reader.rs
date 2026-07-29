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
        IsWindowVisible, GW_OWNER, SW_SHOWNORMAL,
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
        dark_title_applied: bool,
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
                dark_title_applied: false,
                hidden: false,
                last_rect: None,
                last_ppp: 1.0,
            }
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
            // Don't apply immediately — eframe may reset DWM attributes on the
            // same frame when processing SetTheme.  Let sync_bounds apply it on
            // the next frame instead, which is more reliable.
            self.dark_title_applied = false;
        }

        /// Pull Win32 keyboard focus from WebView2 child back to the main window.
        pub fn reclaim_shell_focus(&mut self) {
            let Some(parent) = self.parent else {
                return;
            };
            unsafe {
                // windows 0.58: GetFocus() -> HWND (not Result).
                let focus = GetFocus();
                if focus == parent {
                    return;
                }
                // Focus is elsewhere (typically a WebView2 child) — reclaim.
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
                    apply_titlebar_dark(h, self.dark_title);
                    h
                }
            };

            let html = if self.last_html.is_empty() {
                "<!DOCTYPE html><html><body style=\"font-family:sans-serif;padding:1rem\">Glean reader</body></html>"
                    .to_string()
            } else {
                self.last_html.clone()
            };

            let bounds = rect_to_wry(reader_rect, pixels_per_point);
            let parent_wrap = ParentHwnd(parent);

            let webview = WebViewBuilder::new()
                .with_html(&html)
                .with_bounds(bounds)
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
            let _ = mode;
            Ok(())
        }

        pub fn sync_bounds(&mut self, reader_rect: Rect, pixels_per_point: f32) {
            // Re-apply dark titlebar if needed (eframe may reset it).
            if !self.dark_title_applied {
                if let Some(hwnd) = self.parent {
                    apply_titlebar_dark(hwnd, self.dark_title);
                    self.dark_title_applied = true;
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
                // Move offscreen so egui popups aren't occluded.
                let hidden_rect = WryRect {
                    position: Position::Logical(LogicalPosition::new(-10000.0, -10000.0)),
                    size: Size::Logical(LogicalSize::new(1.0, 1.0)),
                };
                let _ = wv.set_bounds(hidden_rect);
            } else if let Err(e) = wv.set_bounds(rect_to_wry(reader_rect, pixels_per_point)) {
                eprintln!("set_bounds failed: {e}");
            }
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
                    // Reload HTML so WebView2 renders the latest content
                    // (e.g. after a theme change while hidden).
                    if !self.last_html.is_empty() {
                        let _ = wv.load_html(&self.last_html);
                    }
                }
            }
        }
    }

    fn apply_titlebar_dark(hwnd: HWND, dark: bool) {
        use windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE;
        let value: u32 = if dark { 1 } else { 0 };
        // 20 = DWMWA_USE_IMMERSIVE_DARK_MODE; 19 = legacy pre-release value.
        for code in [20i32, 19i32] {
            let attr = DWMWINDOWATTRIBUTE(code);
            unsafe {
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    attr,
                    &value as *const u32 as *const _,
                    std::mem::size_of::<u32>() as u32,
                );
            }
        }
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

    fn find_glean_main_hwnd() -> Option<HWND> {
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
            if !IsWindowVisible(hwnd).as_bool() {
                return TRUE;
            }
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

        state
            .best_glean
            .or(state.best_any)
            .map(|(h, _)| h)
            .filter(|h| {
                let HWND(p) = *h;
                !p.is_null()
            })
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

    pub fn set_mode(&mut self, mode: ReaderHostMode) {
        self.inner.set_mode(mode);
    }

    pub fn shutdown(&mut self) {
        self.inner.shutdown();
    }

    pub fn show_html(&mut self, html: &str) {
        self.inner.show_html(html);
    }

    pub fn set_titlebar_dark(&mut self, dark: bool) {
        self.inner.set_titlebar_dark(dark);
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
    pub fn set_titlebar_dark(&mut self, _dark: bool) {}
    pub fn reclaim_shell_focus(&mut self) {}
    pub fn set_hidden(&mut self, _hidden: bool) {}
}
