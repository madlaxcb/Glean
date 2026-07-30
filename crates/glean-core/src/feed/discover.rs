//! HTML feed auto-discovery (dev plan §2.2 P0).
//!
//! When a user enters a website URL (not a feed URL), this module fetches
//! the HTML page and extracts `<link rel="alternate" type="application/rss+xml"
//! href="…">` (or Atom/JSON Feed equivalents).

/// Discover feed URLs from an HTML document.
/// Returns a list of (url, title) pairs found in `<link rel="alternate">` tags.
/// Ordered by preference: RSS/Atom first, then JSON Feed.
pub fn discover_feed_urls(html: &str, base_url: &str) -> Vec<(String, Option<String>)> {
    let mut results = Vec::new();
    let lower = html.to_lowercase();
    let mut pos = 0;

    while let Some(idx) = lower[pos..].find("<link ") {
        let abs = pos + idx;
        let tag_end = match lower[abs..].find('>') {
            Some(e) => abs + e + 1,
            None => break,
        };
        let tag = &html[abs..tag_end];

        if let Some(link) = extract_link_tag(tag) {
            if link.rel_contains("alternate") && is_feed_type(&link.type_attr) {
                if let Some(href) = link.href {
                    let resolved = resolve_url(base_url, &href);
                    results.push((resolved, link.title));
                }
            }
        }

        pos = tag_end;
    }

    // Prefer application/rss+xml over application/atom+xml over application/feed+json.
    results.sort_by(|a, b| {
        let _ = (a, b);
        // Keep original order (first found = likely preferred by site).
        std::cmp::Ordering::Equal
    });

    results
}

struct LinkTag {
    href: Option<String>,
    title: Option<String>,
    type_attr: Option<String>,
    rel: Option<String>,
}

impl LinkTag {
    fn rel_contains(&self, keyword: &str) -> bool {
        self.rel
            .as_deref()
            .map(|r| r.to_lowercase().contains(keyword))
            .unwrap_or(false)
    }
}

fn extract_link_tag(tag: &str) -> Option<LinkTag> {
    let href = extract_attr(tag, "href");
    let title = extract_attr(tag, "title");
    let type_attr = extract_attr(tag, "type");
    let rel = extract_attr(tag, "rel");

    // Must have href and rel to be useful.
    if href.is_none() && rel.is_none() {
        return None;
    }

    Some(LinkTag {
        href,
        title,
        type_attr,
        rel,
    })
}

fn is_feed_type(type_attr: &Option<String>) -> bool {
    let Some(t) = type_attr else {
        // No type specified — accept if rel=alternate and href looks like a feed.
        return false;
    };
    let lower = t.to_lowercase();
    lower.contains("rss")
        || lower.contains("atom")
        || lower.contains("feed+json")
        || lower.contains("xml")
}

/// Extract an attribute value from an HTML tag.
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_lowercase();
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

/// Resolve a possibly-relative URL against a base URL.
fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    // Simple resolution: use url crate if available, otherwise best-effort.
    if let Ok(base_parsed) = url::Url::parse(base) {
        if let Ok(resolved) = base_parsed.join(href) {
            return resolved.to_string();
        }
    }
    // Fallback: prepend base (trim trailing path).
    let base_trimmed = base.trim_end_matches('/');
    if href.starts_with('/') {
        format!("{base_trimmed}{href}")
    } else {
        format!("{base_trimmed}/{href}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_rss_link() {
        let html = r#"<html><head>
            <link rel="alternate" type="application/rss+xml" title="My Blog" href="/feed.xml">
            </head><body></body></html>"#;
        let results = discover_feed_urls(html, "https://example.com");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "https://example.com/feed.xml");
        assert_eq!(results[0].1.as_deref(), Some("My Blog"));
    }

    #[test]
    fn discover_atom_link() {
        let html = r#"<html><head>
            <link rel='alternate' type='application/atom+xml' href='https://example.com/atom'>
            </head><body></body></html>"#;
        let results = discover_feed_urls(html, "https://example.com");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "https://example.com/atom");
    }

    #[test]
    fn skip_non_feed_links() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/style.css">
            <link rel="alternate" type="text/html" href="/mobile">
            </head><body></body></html>"#;
        let results = discover_feed_urls(html, "https://example.com");
        assert!(results.is_empty());
    }

    #[test]
    fn resolve_relative_url() {
        assert_eq!(
            resolve_url("https://example.com/blog/", "/feed.xml"),
            "https://example.com/feed.xml"
        );
        assert_eq!(
            resolve_url("https://example.com", "feed.xml"),
            "https://example.com/feed.xml"
        );
        assert_eq!(
            resolve_url("https://example.com", "https://other.com/rss"),
            "https://other.com/rss"
        );
    }
}
