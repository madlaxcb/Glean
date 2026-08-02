use crate::error::{CoreError, Result};
use crate::model::ImagePolicy;
use crate::sanitize::sanitize_html_with_policy;
use feed_rs::model::{Content, Text};
use feed_rs::parser;

#[derive(Debug, Clone)]
pub struct ParsedFeed {
    pub title: String,
    pub site_url: Option<String>,
    /// Favicon/logo URL from the feed (Atom <icon>, RSS <image>, etc.).
    pub favicon_url: Option<String>,
    pub entries: Vec<ParsedEntry>,
}

#[derive(Debug, Clone)]
pub struct ParsedEntry {
    pub guid: String,
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<i64>,
    pub summary: Option<String>,
    pub content_html: String,
    /// 缩略图/封面图 URL（列表预览用，可能为空）。
    pub thumbnail: Option<String>,
}

pub fn parse_feed(bytes: &[u8]) -> Result<ParsedFeed> {
    let feed = parser::parse(bytes).map_err(|e| CoreError::Parse(e.to_string()))?;
    let title = feed
        .title
        .as_ref()
        .map(|t| t.content.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Untitled feed".into());
    let site_url = feed
        .links
        .iter()
        .find(|l| l.rel == Some("alternate".into()) || l.rel.is_none())
        .map(|l| l.href.clone())
        .or_else(|| feed.links.first().map(|l| l.href.clone()));
    // Favicon: feed-rs exposes it via the `icon` field (Atom <icon>) or
    // `logo` (Atom <logo>). RSS feeds use <image> which feed-rs maps to `logo`.
    let favicon_url = feed
        .icon
        .as_ref()
        .map(|i| i.uri.clone())
        .or_else(|| feed.logo.as_ref().map(|l| l.uri.clone()));

    let mut entries = Vec::with_capacity(feed.entries.len());
    for (i, e) in feed.entries.iter().enumerate() {
        let guid = if e.id.is_empty() {
            e.links
                .first()
                .map(|l| l.href.clone())
                .unwrap_or_else(|| format!("anon-{i}"))
        } else {
            e.id.clone()
        };
        let title = e
            .title
            .as_ref()
            .map(|t| t.content.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(no title)".into());
        let url = e.links.first().map(|l| l.href.clone());
        let author = e.authors.first().map(|a| a.name.clone());
        let published_at = e.published.or(e.updated).map(|t| t.timestamp());
        let raw_html = pick_html(e.content.as_ref(), e.summary.as_ref());
        let content_html = sanitize_html_with_policy(&raw_html, ImagePolicy::Allow);
        let summary = e.summary.as_ref().map(|s| plainish(&s.content));
        entries.push(ParsedEntry {
            guid,
            title,
            url,
            author,
            published_at,
            summary,
            content_html,
            thumbnail: None,
        });
    }

    Ok(ParsedFeed {
        title,
        site_url,
        favicon_url,
        entries,
    })
}

fn pick_html(content: Option<&Content>, summary: Option<&Text>) -> String {
    if let Some(c) = content {
        if let Some(body) = &c.body {
            if !body.is_empty() {
                return body.clone();
            }
        }
    }
    if let Some(s) = summary {
        return s.content.clone();
    }
    String::new()
}

fn plainish(s: &str) -> String {
    // light strip for summary column
    let cleaned = sanitize_html_with_policy(s, ImagePolicy::Block);
    cleaned.chars().take(280).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RSS: &[u8] = br#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Demo Channel</title>
    <link>https://example.com/</link>
    <item>
      <title>Hello World</title>
      <link>https://example.com/1</link>
      <guid>guid-1</guid>
      <description>&lt;p&gt;Body&lt;/p&gt;&lt;script&gt;x&lt;/script&gt;&lt;img src="https://x/a.png"/&gt;</description>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn parses_rss_and_sanitizes() {
        let f = parse_feed(SAMPLE_RSS).unwrap();
        assert_eq!(f.title, "Demo Channel");
        assert_eq!(f.entries.len(), 1);
        assert_eq!(f.entries[0].guid, "guid-1");
        assert!(!f.entries[0].content_html.to_lowercase().contains("script"));
        // img is kept at parse time (Allow policy); removed at render time if ImagePolicy::Block.
        assert!(f.entries[0].content_html.contains("Body"));
    }
}
