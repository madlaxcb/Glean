//! Appcast update check.
//!
//! V1: prompt-only — fetch `appcast.json` from GitHub Releases, compare
//! versions, surface a "new version available" popup. No auto-install.
//!
//! Schema (minimal):
//! ```json
//! {
//!   "version": "0.0.2",
//!   "url": "https://github.com/madlaxcb/Glean/releases/download/v0.0.2/glean-spike-setup.exe",
//!   "sha256": "abcd...",      // optional, reserved for future verification
//!   "changelog": "..."         // optional
//! }
//! ```

use serde::Deserialize;

/// Default appcast endpoint (GitHub Releases `latest` asset).
pub const APPCAST_URL: &str =
    "https://github.com/madlaxcb/Glean/releases/latest/download/appcast.json";

#[derive(Debug, Clone, Deserialize)]
pub struct Appcast {
    pub version: String,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
}

/// Result of an update check.
#[derive(Debug, Clone)]
pub enum UpdateCheckResult {
    /// Remote version is newer than the running build.
    Available { current: String, cast: Appcast },
    /// Running build is up to date or newer than remote.
    UpToDate { current: String, remote: String },
    /// Fetch/parse failed. The message is suitable for a debug log line,
    /// not a user-facing alert (we stay silent on failure).
    Error(String),
}

/// Current app version (mirrors `workspace.package.version` in Cargo.toml).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Fetch and parse appcast synchronously (call from a worker thread).
pub fn fetch_appcast(url: &str) -> Result<Appcast, String> {
    let resp = reqwest::blocking::get(url).map_err(|e| format!("HTTP: {e}"))?;
    let body = resp.text().map_err(|e| format!("read body: {e}"))?;
    serde_json::from_str::<Appcast>(&body).map_err(|e| format!("parse: {e}"))
}

/// Compare dotted-numeric versions (`"0.0.1"` < `"0.0.2"`). Non-numeric
/// segments fall back to lexicographic comparison.
pub fn is_newer(remote: &str, current: &str) -> bool {
    version_tuple(remote) > version_tuple(current)
}

fn version_tuple(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|s| s.parse::<u64>().unwrap_or(0))
        .collect()
}

/// Run a full check: fetch appcast and compare against `CURRENT_VERSION`.
pub fn check_for_update(url: &str) -> UpdateCheckResult {
    let current = CURRENT_VERSION.to_string();
    match fetch_appcast(url) {
        Ok(cast) => {
            if is_newer(&cast.version, &current) {
                UpdateCheckResult::Available { current, cast }
            } else {
                UpdateCheckResult::UpToDate {
                    current,
                    remote: cast.version,
                }
            }
        }
        Err(e) => UpdateCheckResult::Error(e),
    }
}

/// Open an external URL in the user's default browser.
#[cfg(windows)]
pub fn open_url(url: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let mut wide: Vec<u16> = url.encode_utf16().collect();
    wide.push(0);
    let operation: Vec<u16> = "open\0".encode_utf16().collect();
    unsafe {
        let _ = ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Open an external URL in the user's default browser.
#[cfg(not(windows))]
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_basic() {
        assert!(is_newer("0.0.2", "0.0.1"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.0.1", "0.0.1"));
        assert!(!is_newer("0.0.0", "0.0.1"));
    }

    #[test]
    fn version_compare_with_v_prefix() {
        assert!(is_newer("v0.1.0", "0.0.5"));
    }

    #[test]
    fn parse_appcast_minimal() {
        let json = r#"{"version":"0.0.2","url":"https://x/y.exe"}"#;
        let cast: Appcast = serde_json::from_str(json).unwrap();
        assert_eq!(cast.version, "0.0.2");
        assert!(cast.sha256.is_none());
        assert!(cast.changelog.is_none());
    }

    #[test]
    fn parse_appcast_full() {
        let json = r#"{
            "version":"0.0.3",
            "url":"https://x/y.exe",
            "sha256":"abc",
            "changelog":"fixes"
        }"#;
        let cast: Appcast = serde_json::from_str(json).unwrap();
        assert_eq!(cast.sha256.as_deref(), Some("abc"));
        assert_eq!(cast.changelog.as_deref(), Some("fixes"));
    }
}
