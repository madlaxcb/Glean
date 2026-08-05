//! Readability-style full-text extraction.
//!
//! Used when a feed entry only ships a short summary but the original article
//! URL is available. Fetches the HTML page and extracts the main content via
//! a simplified mozilla/readability-style scoring algorithm.
//!
//! Design:
//! - Parse with scraper (html5ever).
//! - Remove obviously-non-content nodes (script, style, nav, footer, header,
//!   aside, form, iframe, noscript).
//! - Score candidate block elements (`article`, `[role=main]`, `main`, `div`,
//!   `section`) by text length, paragraph density, class/id hints ("content",
//!   "article", "post", "entry", "body" → +; "comment", "sidebar", "nav",
//!   "footer", "promo", "ad" → -).
//! - Return the inner HTML of the highest-scoring node, then sanitize via
//!   ammonia before storage.
//!
//! This is intentionally simple. It won't beat a real readability port on every
//! site, but covers the common cases (blogs, news) without a heavy dependency.

use crate::error::{CoreError, Result};
use crate::model::EntryId;
use crate::sanitize::sanitize_html_with_policy;
use scraper::{Html, Selector};

/// Background extraction task: UI prepares this, hands it to a worker thread,
/// and feeds the result back via `apply_extract_outcome`.
#[derive(Debug, Clone)]
pub struct ExtractTask {
    pub entry_id: EntryId,
    pub url: String,
}

/// Result of running an extraction task.
#[derive(Debug, Clone)]
pub enum ExtractOutcome {
    /// Extraction succeeded and produced new HTML (sanitized).
    Extracted { entry_id: EntryId, html: String },
    /// Page was fetched but no readable content found, or HTTP failed.
    Failed { entry_id: EntryId, error: String },
}

/// Run an extraction task on a worker thread. Blocking; the UI should spawn
/// this in a thread. Reuses an existing reqwest client.
pub fn run_extract_task(client: &reqwest::blocking::Client, task: &ExtractTask) -> ExtractOutcome {
    match fetch_article_html(client, &task.url) {
        Ok(raw) => {
            let extracted = extract_content(&raw);
            if extracted.is_empty() {
                ExtractOutcome::Failed {
                    entry_id: task.entry_id,
                    error: "no readable content found".into(),
                }
            } else {
                ExtractOutcome::Extracted {
                    entry_id: task.entry_id,
                    html: extracted,
                }
            }
        }
        Err(e) => ExtractOutcome::Failed {
            entry_id: task.entry_id,
            error: e.to_string(),
        },
    }
}

/// Minimum content length below which we consider a feed entry "summary-only"
/// and worth attempting full-text extraction.
pub const SUMMARY_THRESHOLD: usize = 500;

/// Extract readable content from raw HTML. Returns sanitized HTML fragment
/// (no <html>/<body> wrapper). Returns empty string if no good candidate found.
pub fn extract_content(raw_html: &str) -> String {
    let document = Html::parse_document(raw_html);

    // First, try the obvious semantic containers in priority order.
    for sel in &PREFERRED_SELECTORS {
        if let Some(html) = best_match_for_selector(&document, sel) {
            return sanitize_fragment(&html);
        }
    }

    // Fallback: score all <div>/<section> by content signals.
    let best = score_candidates(&document);
    if let Some(html) = best {
        return sanitize_fragment(&html);
    }

    // Last resort: <body>.
    if let Ok(sel) = Selector::parse("body") {
        if let Some(el) = document.select(&sel).next() {
            return sanitize_fragment(&el.inner_html());
        }
    }
    String::new()
}

/// Decide whether extraction is worthwhile: only if the feed content is short
/// (summary-only) and the URL points to http(s).
pub fn should_extract(feed_content_html: &str, entry_url: Option<&str>) -> bool {
    if feed_content_html.len() >= SUMMARY_THRESHOLD {
        return false;
    }
    match entry_url.and_then(|u| url::Url::parse(u).ok()) {
        Some(url) if matches!(url.scheme(), "http" | "https") => {
            let host = url.host_str().unwrap_or_default();
            !(host == "pixiv.net" || host == "www.pixiv.net")
                || !url.path().starts_with("/artworks/")
        }
        Some(_) => false,
        None => false,
    }
}

/// Fetch raw HTML for an article URL. Reuses a caller-provided reqwest client.
pub fn fetch_article_html(client: &reqwest::blocking::Client, url: &str) -> Result<String> {
    let resp = client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Glean/0.0.1 (+RSS reader)",
            ),
        )
        .send()
        .map_err(|e| CoreError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(CoreError::Http(format!(
            "HTTP {} for {}",
            resp.status(),
            url
        )));
    }
    // reqwest with default features doesn't decompress; text() assumes UTF-8
    // or detected charset. For non-UTF-8 pages this may produce mojibake — an
    // acceptable V1 limitation.
    resp.text()
        .map_err(|e| CoreError::Http(format!("read body: {e}")))
}

const PREFERRED_SELECTORS: [&str; 6] = [
    "article",
    "[role=main]",
    "main",
    ".post-content",
    ".entry-content",
    ".article-content",
];

fn best_match_for_selector(document: &Html, selector_str: &str) -> Option<String> {
    let sel = Selector::parse(selector_str).ok()?;
    let mut best: Option<(usize, String)> = None;
    for el in document.select(&sel) {
        let text = text_length(&el);
        if text < 200 {
            continue;
        }
        match &best {
            Some((t, _)) if *t >= text => {}
            _ => best = Some((text, el.inner_html())),
        }
    }
    best.map(|(_, h)| h)
}

fn score_candidates(document: &Html) -> Option<String> {
    let sel = Selector::parse("div, section").ok()?;
    let mut best: Option<(i64, String)> = None;
    for el in document.select(&sel) {
        // Skip tiny nodes early.
        let p_count = el.select(&Selector::parse("p").unwrap()).count();
        if p_count == 0 && text_length(&el) < 400 {
            continue;
        }
        let mut score: i64 = 0;
        score += (text_length(&el) as i64) / 40;
        score += (p_count as i64) * 10;

        // Class / id hints.
        let class_id = match el.value().attr("class") {
            Some(c) => format!("{} ", c),
            None => String::new(),
        };
        let id = el.value().attr("id").unwrap_or("");
        let combined = format!("{}{}", class_id, id).to_lowercase();
        for good in &[
            "article", "content", "post", "entry", "body", "story", "main",
        ] {
            if combined.contains(good) {
                score += 30;
            }
        }
        for bad in &[
            "comment", "sidebar", "footer", "header", "nav", "menu", "promo", "advert", "share",
            "related", "popup",
        ] {
            if combined.contains(bad) {
                score -= 40;
            }
        }

        match &best {
            Some((s, _)) if *s >= score => {}
            _ => best = Some((score, el.inner_html())),
        }
    }
    best.map(|(_, h)| h)
}

fn text_length(el: &scraper::ElementRef) -> usize {
    el.text().collect::<String>().trim().len()
}

fn sanitize_fragment(html: &str) -> String {
    // Allow remote images; the per-render ImagePolicy is applied later in
    // reader_document. Extraction preserves the original tags.
    sanitize_html_with_policy(html, crate::model::ImagePolicy::Allow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_from_article_tag() {
        let html = r#"<!DOCTYPE html><html><body>
            <nav>home about</nav>
            <article><h1>Title</h1><p>Body text. </p>
            <p>More body text here with enough length to pass thresholds.</p>
            <p>Even more body text to ensure we are above the 200-char minimum
            that the extractor uses to accept a candidate node.</p>
            </article>
            <footer>(c) 2026</footer>
        </body></html>"#;
        let out = extract_content(html);
        assert!(out.to_lowercase().contains("body text"));
        assert!(!out.to_lowercase().contains("home about"));
        assert!(!out.to_lowercase().contains("(c) 2026"));
    }

    #[test]
    fn extract_prefers_content_div_over_nav() {
        let html = r#"<html><body>
            <div class="nav">menu menu menu menu menu menu</div>
            <div class="post-content">
                <p>Real article body content with substantial length to pass
                   the extractor's text-length threshold checks for scoring.</p>
                <p>Second paragraph adds more weight to the content div.</p>
            </div>
        </body></html>"#;
        let out = extract_content(html);
        assert!(out.contains("Real article body"));
        assert!(!out.contains("menu menu"));
    }

    #[test]
    fn should_extract_respects_threshold() {
        assert!(should_extract("short", Some("https://x.com/a")));
        assert!(!should_extract(
            &"x".repeat(SUMMARY_THRESHOLD),
            Some("https://x.com/a")
        ));
        assert!(!should_extract("short", Some("ftp://x")));
        assert!(!should_extract("short", None));
        assert!(!should_extract(
            "short",
            Some("https://www.pixiv.net/artworks/147652038")
        ));
    }

    #[test]
    fn empty_html_returns_empty() {
        assert_eq!(extract_content(""), "");
    }
}
