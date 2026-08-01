//! Local image cache (dev plan §2.5.2).
//!
//! When the user opts in (or per-article "显示图片" with cache enabled),
//! remote images in an entry's HTML are downloaded to `cache/images/<hash>`
//! and the `img src` is rewritten to the `glean-img://<hash>` custom scheme.
//! The WebView registers a handler for that scheme to serve the local file.
//!
//! Benefits:
//! - Privacy: subsequent reads don't re-hit the source server.
//! - Offline: cached entries with rewritten img tags remain fully readable.
//! - Privacy against the source: the original request still happens once
//!   on download, but never again.

use crate::error::{CoreError, Result};
use base64::Engine;
use std::collections::HashMap;
use std::path::PathBuf;

/// Custom scheme used by the WebView to load local cached images.
pub const CUSTOM_SCHEME: &str = "glean-img";

/// Hash a URL into a stable filename. Uses a simple FNV-1a 64-bit hash
/// (no extra deps). Extension is derived from the URL path or content type.
pub fn cached_filename(url: &str, content_type: Option<&str>) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in url.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let ext = guess_extension(url, content_type);
    format!("{:016x}.{}", hash, ext)
}

fn guess_extension(url: &str, content_type: Option<&str>) -> &'static str {
    // Prefer content-type.
    if let Some(ct) = content_type {
        let ct = ct.to_lowercase();
        if ct.contains("jpeg") || ct.contains("jpg") {
            return "jpg";
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
        if ct.contains("svg") {
            return "svg";
        }
        if ct.contains("avif") {
            return "avif";
        }
    }
    // Then URL path suffix.
    let lower = url.to_lowercase();
    for ext in &["jpg", "jpeg", "png", "gif", "webp", "svg", "avif", "bmp"] {
        let dot = format!(".{ext}");
        if lower.contains(&dot) {
            // Normalise jpeg → jpg.
            return if *ext == "jpeg" { "jpg" } else { ext };
        }
    }
    // Default: jpg is the safest bet for feed images.
    "jpg"
}

/// Cache for a single entry. Owns the cache dir (None in memory mode).
///
/// When `serve_base` is set (e.g. `http://127.0.0.1:PORT`), rewritten `src`
/// values point at that local HTTP origin instead of embedding base64 data
/// URLs. This keeps large originals (Pixiv img-original) displayable in
/// WebView without blowing up document size.
pub struct ImageCache {
    dir: Option<PathBuf>,
    serve_base: Option<String>,
}

impl ImageCache {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            serve_base: None,
        }
    }

    /// Serve cached files via a local HTTP base URL (`http://127.0.0.1:PORT`).
    pub fn with_serve_base(mut self, base: Option<String>) -> Self {
        self.serve_base = base.filter(|s| !s.is_empty());
        self
    }

    pub fn enabled(&self) -> bool {
        self.dir.is_some()
    }

    /// Download all remote images in `html` and rewrite their `src` to a local
    /// URL (HTTP base when configured, otherwise `data:`). Returns the
    /// rewritten HTML and the list of (filename, bytes) pairs that were
    /// freshly downloaded (already-cached files are not re-downloaded). On
    /// per-image failure the original URL is left in place.
    ///
    /// Uses a blocking reqwest client (call from a worker thread).
    pub fn cache_images_in_html(
        &self,
        html: &str,
        client: &reqwest::blocking::Client,
    ) -> (String, Vec<(String, Vec<u8>)>) {
        if !self.enabled() {
            return (html.to_string(), Vec::new());
        }
        let urls = collect_img_urls(html);
        if urls.is_empty() {
            return (html.to_string(), Vec::new());
        }
        let dir = self.dir.as_ref().unwrap();
        let _ = std::fs::create_dir_all(dir);

        let mut rewritten: HashMap<String, String> = HashMap::new();
        let mut fetched: Vec<(String, Vec<u8>)> = Vec::new();

        for url in &urls {
            // Skip non-http(s) (data:, file:, already-rewritten local URLs).
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                continue;
            }
            // Already pointing at our local image server.
            if self
                .serve_base
                .as_ref()
                .is_some_and(|base| url.starts_with(base))
            {
                continue;
            }
            let filename = cached_filename(url, None);
            let path = dir.join(&filename);
            if !path.exists() {
                // Download.
                match fetch_image(client, url) {
                    Ok((bytes, content_type)) => {
                        // Re-derive filename with the actual content-type.
                        let filename = cached_filename(url, Some(&content_type));
                        let path = dir.join(&filename);
                        if std::fs::write(&path, &bytes).is_ok() {
                            rewritten.insert(url.clone(), self.local_url(&filename, &bytes));
                            fetched.push((filename, bytes));
                        }
                    }
                    Err(e) => {
                        eprintln!("glean: img download failed {url}: {e}");
                        // Leave original URL on failure.
                    }
                }
            } else if let Some(local) = self.cached_local_url(&filename, &path) {
                rewritten.insert(url.clone(), local);
            }
        }

        if rewritten.is_empty() {
            return (html.to_string(), fetched);
        }
        (rewrite_img_src(html, &rewritten), fetched)
    }

    fn local_url(&self, filename: &str, bytes: &[u8]) -> String {
        if let Some(base) = &self.serve_base {
            format!("{}/{filename}", base.trim_end_matches('/'))
        } else {
            data_url(filename, bytes)
        }
    }

    fn cached_local_url(&self, filename: &str, path: &std::path::Path) -> Option<String> {
        if let Some(base) = &self.serve_base {
            return Some(format!("{}/{filename}", base.trim_end_matches('/')));
        }
        let bytes = std::fs::read(path).ok()?;
        Some(data_url(filename, &bytes))
    }

    /// Look up a cached file by filename (used by the WebView scheme handler).
    pub fn read(&self, filename: &str) -> Option<Vec<u8>> {
        let dir = self.dir.as_ref()?;
        // Reject any path traversal.
        if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
            return None;
        }
        std::fs::read(dir.join(filename)).ok()
    }

    /// Guess MIME type from filename extension.
    pub fn mime_for(filename: &str) -> &'static str {
        let lower = filename.to_lowercase();
        if lower.ends_with(".png") {
            "image/png"
        } else if lower.ends_with(".gif") {
            "image/gif"
        } else if lower.ends_with(".webp") {
            "image/webp"
        } else if lower.ends_with(".svg") {
            "image/svg+xml"
        } else if lower.ends_with(".avif") {
            "image/avif"
        } else if lower.ends_with(".bmp") {
            "image/bmp"
        } else {
            "image/jpeg"
        }
    }

    /// Approximate total size of the cache dir in bytes (best-effort).
    pub fn total_size(&self) -> u64 {
        let dir = match &self.dir {
            Some(d) => d,
            None => return 0,
        };
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        total += meta.len();
                    }
                }
            }
        }
        total
    }

    /// Delete all cached files (best-effort). Used by "清空缓存".
    pub fn clear(&self) -> Result<()> {
        let dir = self
            .dir
            .as_ref()
            .ok_or_else(|| CoreError::Message("image cache disabled (in-memory mode)".into()))?;
        if !dir.exists() {
            return Ok(());
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }
}

fn data_url(filename: &str, bytes: &[u8]) -> String {
    format!(
        "data:{};base64,{}",
        ImageCache::mime_for(filename),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn fetch_image(client: &reqwest::blocking::Client, url: &str) -> Result<(Vec<u8>, String)> {
    let mut req = client.get(url).header(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Glean/0.0.1 (+RSS reader)",
        ),
    );
    // i.pximg.net 要求 Referer 为 pixiv 页面，否则返回 403。
    if let Some(host) = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
    {
        if host == "i.pximg.net" || host.ends_with(".pximg.net") {
            req = req.header(
                reqwest::header::REFERER,
                reqwest::header::HeaderValue::from_static("https://www.pixiv.net/"),
            );
        }
    }
    let resp = req.send().map_err(|e| CoreError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(CoreError::Http(format!(
            "HTTP {} for {}",
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
    Ok((bytes, content_type))
}

/// Collect all unique image URLs from <img src="…"> tags in `html`.
/// Naive regex-free scan; HTML is already ammonia-sanitized so tag shape is
/// predictable. Handles both single and double quotes.
fn collect_img_urls(html: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let lower = html.to_lowercase();
    let mut pos = 0;
    while let Some(idx) = lower[pos..].find("<img ") {
        let abs = pos + idx;
        // Find closing >.
        let tag_end = match lower[abs..].find('>') {
            Some(e) => abs + e + 1, // include '>'
            None => break,
        };
        let tag = &html[abs..tag_end];
        if let Some(url) = extract_attr(tag, "src") {
            if seen.insert(url.clone()) {
                urls.push(url);
            }
        }
        pos = tag_end;
    }
    urls
}

/// Extract an attribute value from an HTML tag. Handles single/double quotes.
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_lowercase();
    // Search for ` attr=` (with leading space to avoid matching e.g. "data-src=").
    let search = format!(" {attr}=");
    let idx = lower.find(&search)?;
    let after_eq = idx + search.len();
    let rest = &tag[after_eq..];
    let trimmed = rest.trim_start();
    let first = trimmed.chars().next()?;
    if first != '"' && first != '\'' {
        return None;
    }
    let inner = &trimmed[1..];
    let end = inner.find(first)?;
    Some(inner[..end].to_string())
}

/// Rewrite every `<img src="original">` to `<img src="rewritten">` per the
/// mapping. Preserves other attributes. Same naive scan as `collect_img_urls`.
fn rewrite_img_src(html: &str, mapping: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_lowercase();
    let mut pos = 0;
    while let Some(idx) = lower[pos..].find("<img ") {
        let abs = pos + idx;
        out.push_str(&html[pos..abs]);
        let tag_end = match lower[abs..].find('>') {
            Some(e) => abs + e,
            None => {
                out.push_str(&html[abs..]);
                return out;
            }
        };
        let tag = &html[abs..tag_end];
        let rewritten = rewrite_one_tag(tag, mapping);
        out.push_str(&rewritten);
        pos = tag_end;
    }
    out.push_str(&html[pos..]);
    out
}

fn rewrite_one_tag(tag: &str, mapping: &HashMap<String, String>) -> String {
    let lower = tag.to_lowercase();
    let search = " src=";
    let Some(idx) = lower.find(search) else {
        return tag.to_string();
    };
    let after_eq = idx + search.len();
    let rest = &tag[after_eq..];
    let trimmed = rest.trim_start();
    let Some(first) = trimmed.chars().next() else {
        return tag.to_string();
    };
    if first != '"' && first != '\'' {
        return tag.to_string();
    }
    let inner = &trimmed[1..];
    let Some(end) = inner.find(first) else {
        return tag.to_string();
    };
    let original = &inner[..end];
    let Some(new_url) = mapping.get(original) else {
        return tag.to_string();
    };
    let mut out = String::with_capacity(tag.len() + new_url.len());
    out.push_str(&tag[..after_eq]);
    out.push(first);
    out.push_str(new_url);
    out.push(first);
    out.push_str(&inner[end + 1..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn filename_stable_and_unique() {
        let a = cached_filename("https://x.com/a.png", None);
        let b = cached_filename("https://x.com/a.png", None);
        let c = cached_filename("https://x.com/b.png", None);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.ends_with(".png"));
    }

    #[test]
    fn content_type_overrides_url_ext() {
        let f = cached_filename("https://x.com/img", Some("image/png"));
        assert!(f.ends_with(".png"));
    }

    #[test]
    fn collect_urls_handles_single_and_double_quotes() {
        let html = r#"<p><img src="https://a.com/1.jpg"> <img src='https://b.com/2.png'></p>"#;
        let urls = collect_img_urls(html);
        assert_eq!(urls.len(), 2);
        assert!(urls.iter().any(|u| u == "https://a.com/1.jpg"));
        assert!(urls.iter().any(|u| u == "https://b.com/2.png"));
    }

    #[test]
    fn rewrite_swaps_src() {
        let html = r#"<img src="https://a.com/x.jpg" alt="x">"#;
        let mut map = HashMap::new();
        map.insert(
            "https://a.com/x.jpg".to_string(),
            "glean-img://abc.jpg".to_string(),
        );
        let out = rewrite_img_src(html, &map);
        assert!(out.contains("glean-img://abc.jpg"));
        assert!(!out.contains("https://a.com/x.jpg"));
        // Other attributes preserved.
        assert!(out.contains(r#"alt="x""#));
    }

    #[test]
    fn data_url_encodes_cached_image() {
        assert_eq!(
            data_url("50ef2c4a4a047187.jpg", b"hello"),
            "data:image/jpeg;base64,aGVsbG8="
        );
    }

    #[test]
    fn serve_base_rewrites_to_local_http() {
        let tmp = std::env::temp_dir().join(format!(
            "glean-img-serve-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let url = "https://i.pximg.net/img-original/img/x.png";
        let filename = cached_filename(url, Some("image/png"));
        std::fs::write(tmp.join(&filename), b"png-bytes").unwrap();
        let cache =
            ImageCache::new(Some(tmp.clone())).with_serve_base(Some("http://127.0.0.1:9".into()));
        let client = reqwest::blocking::Client::new();
        let html = format!(r#"<p><img src="{url}"></p>"#);
        let (out, fetched) = cache.cache_images_in_html(&html, &client);
        assert!(fetched.is_empty());
        assert!(out.contains(&format!("http://127.0.0.1:9/{filename}")));
        assert!(!out.contains("data:image"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn cache_read_roundtrip() {
        let tmp = std::env::temp_dir().join(format!(
            "glean-img-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let cache = ImageCache::new(Some(tmp.clone()));
        let fname = "test.png";
        let path = tmp.join(fname);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"hello").unwrap();
        drop(f);
        assert_eq!(cache.read(fname).unwrap(), b"hello");
        // Path traversal rejected.
        assert!(cache.read("../escape").is_none());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn path_traversal_blocked() {
        let cache = ImageCache::new(Some(PathBuf::from("/tmp")));
        assert!(cache.read("../../etc/passwd").is_none());
        assert!(cache.read("a/b").is_none());
    }

    #[test]
    fn mime_for_common_extensions() {
        assert_eq!(ImageCache::mime_for("x.png"), "image/png");
        assert_eq!(ImageCache::mime_for("x.jpg"), "image/jpeg");
        assert_eq!(ImageCache::mime_for("x.gif"), "image/gif");
        assert_eq!(ImageCache::mime_for("x.webp"), "image/webp");
        assert_eq!(ImageCache::mime_for("x.unknown"), "image/jpeg");
    }

    #[test]
    fn cache_disabled_when_dir_none() {
        let cache = ImageCache::new(None);
        let client = reqwest::blocking::Client::new();
        let (out, fetched) = cache.cache_images_in_html("<img src=x>", &client);
        assert_eq!(out, "<img src=x>");
        assert!(fetched.is_empty());
    }
}
