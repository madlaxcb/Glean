//! Favicon download and disk cache (dev plan §2.2 P1).
//!
//! Favicons are downloaded to `<data_dir>/cache/favicons/<feed_id>.<ext>`
//! on a background thread. The UI thread then decodes them into textures.

use crate::error::{CoreError, Result};
use crate::model::FeedId;
use std::path::PathBuf;

/// Manages favicon files in `<data_dir>/cache/favicons/`.
pub struct FaviconCache {
    dir: Option<PathBuf>,
}

impl FaviconCache {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    pub fn enabled(&self) -> bool {
        self.dir.is_some()
    }

    /// Download a favicon from `url` and save to the cache dir.
    /// Returns the filename (e.g., "42.png") on success.
    /// Uses a blocking reqwest client (call from a worker thread).
    pub fn download(
        &self,
        feed_id: FeedId,
        url: &str,
        client: &reqwest::blocking::Client,
    ) -> Result<String> {
        let dir = self
            .dir
            .as_ref()
            .ok_or_else(|| CoreError::Message("favicon cache disabled (in-memory mode)".into()))?;
        let _ = std::fs::create_dir_all(dir);

        // Skip if already cached.
        if let Some(name) = self.cached_filename(feed_id) {
            return Ok(name);
        }

        let resp = client
            .get(url)
            .header(
                reqwest::header::USER_AGENT,
                reqwest::header::HeaderValue::from_static(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Glean/0.0.1 (+RSS reader)",
                ),
            )
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .map_err(|e| CoreError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(CoreError::Http(format!(
                "HTTP {} for favicon {}",
                resp.status(),
                url
            )));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = resp
            .bytes()
            .map_err(|e| CoreError::Http(e.to_string()))?
            .to_vec();

        if bytes.is_empty() {
            return Err(CoreError::Http("empty favicon response".into()));
        }

        let ext = guess_favicon_ext(&content_type, url);
        let filename = format!("{}.{}", feed_id.0, ext);
        let path = dir.join(&filename);
        std::fs::write(&path, &bytes).map_err(|e| CoreError::Message(e.to_string()))?;

        Ok(filename)
    }

    /// Find the cached filename for a feed (e.g., "42.png").
    /// Scans the cache dir for `<feed_id>.*`.
    pub fn cached_filename(&self, feed_id: FeedId) -> Option<String> {
        let dir = self.dir.as_ref()?;
        if !dir.exists() {
            return None;
        }
        let prefix = format!("{}.", feed_id.0);
        for entry in std::fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) {
                return Some(name);
            }
        }
        None
    }

    /// Read the cached favicon bytes for a feed.
    pub fn read(&self, feed_id: FeedId) -> Option<Vec<u8>> {
        let dir = self.dir.as_ref()?;
        let filename = self.cached_filename(feed_id)?;
        std::fs::read(dir.join(filename)).ok()
    }

    /// Is a favicon already cached for this feed?
    pub fn is_cached(&self, feed_id: FeedId) -> bool {
        self.cached_filename(feed_id).is_some()
    }
}

fn guess_favicon_ext(content_type: &str, url: &str) -> &'static str {
    let ct = content_type.to_lowercase();
    if ct.contains("svg") {
        return "svg";
    }
    if ct.contains("png") {
        return "png";
    }
    if ct.contains("gif") {
        return "gif";
    }
    if ct.contains("webp") {
        return "webp";
    }
    if ct.contains("jpeg") || ct.contains("jpg") {
        return "jpg";
    }
    if ct.contains("ico") || ct.contains("icon") {
        return "ico";
    }
    // Fallback: check URL.
    let lower = url.to_lowercase();
    if lower.contains(".svg") {
        return "svg";
    }
    if lower.contains(".png") {
        return "png";
    }
    if lower.contains(".gif") {
        return "gif";
    }
    if lower.contains(".webp") {
        return "webp";
    }
    if lower.contains(".jpg") || lower.contains(".jpeg") {
        return "jpg";
    }
    if lower.contains(".ico") {
        return "ico";
    }
    // Default: ico (most common favicon format).
    "ico"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guess_ext_from_content_type() {
        assert_eq!(guess_favicon_ext("image/png", ""), "png");
        assert_eq!(guess_favicon_ext("image/x-icon", ""), "ico");
        assert_eq!(guess_favicon_ext("image/svg+xml", ""), "svg");
        assert_eq!(guess_favicon_ext("image/jpeg", ""), "jpg");
    }

    #[test]
    fn guess_ext_from_url_fallback() {
        assert_eq!(guess_favicon_ext("", "https://x.com/favicon.png"), "png");
        assert_eq!(
            guess_favicon_ext("text/html", "https://x.com/favicon.ico"),
            "ico"
        );
    }

    #[test]
    fn cache_disabled_returns_none() {
        let cache = FaviconCache::new(None);
        assert!(!cache.enabled());
        assert!(cache.cached_filename(FeedId(1)).is_none());
        assert!(cache.read(FeedId(1)).is_none());
    }
}
