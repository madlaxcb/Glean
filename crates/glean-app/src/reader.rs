//! WebView2 reader host for M0 spike (Windows).
//!
//! Single-instance webview; bounds track the egui reader rect.
//! Parent HWND is resolved via the main window title (eframe 0.31 does not
//! expose a stable window handle on `Frame` — acceptable for spike only).

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
    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
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
            let hwnd = self.0.0 as isize;
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
    }

    impl ReaderHostInner {
        pub fn new() -> Self {
            Self {
                webview: None,
                last_html: String::new(),
                mode: ReaderHostMode::ChildEmbed,
            }
        }

        pub fn set_mode(&mut self, mode: ReaderHostMode) {
            if self.mode != mode {
                self.mode = mode;
                // Drop and recreate so bounds policy can be re-evaluated.
                self.webview = None;
            }
        }

        pub fn shutdown(&mut self) {
            self.webview = None;
        }

        pub fn show_html(&mut self, html: &str) {
            self.last_html = html.to_string();
            if let Some(wv) = self.webview.as_ref() {
                if let Err(e) = wv.load_html(html) {
                    eprintln!("load_html failed: {e}");
                }
            }
        }

        pub fn ensure_attached(
            &mut self,
            mode: ReaderHostMode,
            reader_rect: Rect,
            _pixels_per_point: f32,
        ) -> Result<(), String> {
            self.mode = mode;
            if self.webview.is_some() {
                return Ok(());
            }

            let parent = find_main_hwnd().ok_or_else(|| {
                "FindWindow failed — is the title still \"Glean / 拾光 — M0 UI Spike\"?".to_string()
            })?;

            let html = if self.last_html.is_empty() {
                "<!DOCTYPE html><html><body style=\"font-family:sans-serif\">Glean reader</body></html>"
                    .to_string()
            } else {
                self.last_html.clone()
            };

            let bounds = rect_to_wry(reader_rect);
            let parent_wrap = ParentHwnd(parent);

            // Both H1 and H2 start as child webviews with tracked bounds.
            // H2 "true top-level popup" can be iterated after first Windows run
            // if child mode fails IME/focus checks — record in docs/spike-ui.md.
            let webview = WebViewBuilder::new()
                .with_html(html)
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
            let _ = mode; // documented: same attach path for H1/H2 in v0 spike
            Ok(())
        }

        pub fn sync_bounds(&mut self, reader_rect: Rect, _pixels_per_point: f32) {
            let Some(wv) = self.webview.as_ref() else {
                return;
            };
            if let Err(e) = wv.set_bounds(rect_to_wry(reader_rect)) {
                eprintln!("set_bounds failed: {e}");
            }
        }
    }

    fn find_main_hwnd() -> Option<HWND> {
        // Must match ViewportBuilder title in main.rs
        unsafe { FindWindowW(None, w!("Glean / 拾光 — M0 UI Spike")).ok() }
    }

    fn rect_to_wry(rect: Rect) -> WryRect {
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

    fn open_external(uri: &str) -> std::io::Result<()> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", uri])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
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
}
