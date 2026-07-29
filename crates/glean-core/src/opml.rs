//! Minimal OPML 2.0 import/export (no extra deps).

use crate::model::Feed;

pub struct OpmlOutline {
    pub title: String,
    pub feed_url: String,
    pub folder: Option<String>,
}

/// Parse OPML XML, extracting rss outlines (with optional folder via parent).
pub fn parse_opml(xml: &str) -> Vec<OpmlOutline> {
    let mut out = Vec::new();
    let mut current_folder: Option<String> = None;
    let mut depth = 0i32;

    for token in tokenize(xml) {
        match token {
            XmlToken::OpenTag { name, attrs } => {
                if name == "outline" || name.ends_with(":outline") {
                    depth += 1;
                    let _kind = attrs
                        .iter()
                        .find(|(k, _)| k == "type")
                        .map(|(_, v)| v.as_str());
                    let xml_url = attrs
                        .iter()
                        .find(|(k, _)| k == "xmlUrl" || k == "xmlurl")
                        .map(|(_, v)| v.clone());
                    let title = attrs
                        .iter()
                        .find(|(k, _)| k == "title" || k == "text")
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    if let Some(url) = xml_url {
                        out.push(OpmlOutline {
                            title,
                            feed_url: url,
                            folder: current_folder.clone(),
                        });
                    } else {
                        // folder
                        current_folder = Some(title);
                    }
                }
            }
            XmlToken::CloseTag { name } => {
                if name == "outline" || name.ends_with(":outline") {
                    depth -= 1;
                    if depth <= 0 {
                        current_folder = None;
                    }
                }
            }
            XmlToken::Text(_) => {}
        }
    }
    out
}

/// Export feeds to OPML 2.0 XML.
pub fn export_opml(feeds: &[Feed]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
<head><title>Glean subscriptions</title></head>
<body>
"#,
    );
    for f in feeds {
        let title = escape(&f.title);
        let url = escape(&f.feed_url);
        let site = f.site_url.as_deref().map(escape).unwrap_or_default();
        s.push_str(&format!(
            "  <outline type=\"rss\" text=\"{title}\" title=\"{title}\" xmlUrl=\"{url}\" htmlUrl=\"{site}\"/>\n"
        ));
    }
    s.push_str("</body>\n</opml>\n");
    s
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[allow(dead_code)]
enum XmlToken {
    OpenTag {
        name: String,
        attrs: Vec<(String, String)>,
    },
    CloseTag {
        name: String,
    },
    Text(String),
}

fn tokenize(xml: &str) -> Vec<XmlToken> {
    let bytes = xml.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'!' {
                // skip <!-- -->
                if let Some(end) = xml[i..].find("-->") {
                    i += end + 3;
                    continue;
                }
            }
            if i + 1 < bytes.len() && bytes[i + 1] == b'?' {
                if let Some(end) = xml[i..].find("?>") {
                    i += end + 2;
                    continue;
                }
            }
            if i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                if let Some(end) = xml[i..].find('>') {
                    let name = xml[i + 2..i + end].trim().to_string();
                    tokens.push(XmlToken::CloseTag { name });
                    i += end + 1;
                    continue;
                }
            }
            if let Some(end) = xml[i..].find('>') {
                let inner = &xml[i + 1..i + end];
                let self_closing = inner.trim_end().ends_with('/');
                let inner = inner.trim_end().trim_end_matches('/').trim();
                let (name, attrs) = parse_tag(inner);
                tokens.push(XmlToken::OpenTag { name, attrs });
                if self_closing {
                    tokens.push(XmlToken::CloseTag {
                        name: String::new(),
                    });
                }
                i += end + 1;
                continue;
            }
        }
        i += 1;
    }
    tokens
}

fn parse_tag(inner: &str) -> (String, Vec<(String, String)>) {
    let mut chars = inner.chars().peekable();
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            break;
        }
        name.push(c);
        chars.next();
    }
    let mut attrs = Vec::new();
    let rest: String = chars.collect();
    let bytes = rest.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() {
            break;
        }
        let key_start = idx;
        while idx < bytes.len() && bytes[idx] != b'=' && !bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let key = rest[key_start..idx].to_string();
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx < bytes.len() && bytes[idx] == b'=' {
            idx += 1;
            while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
                idx += 1;
            }
            let value = if idx < bytes.len() && (bytes[idx] == b'"' || bytes[idx] == b'\'') {
                let quote = bytes[idx];
                idx += 1;
                let v_start = idx;
                while idx < bytes.len() && bytes[idx] != quote {
                    idx += 1;
                }
                let v = rest[v_start..idx].to_string();
                idx += 1;
                v
            } else {
                let v_start = idx;
                while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
                    idx += 1;
                }
                rest[v_start..idx].to_string()
            };
            attrs.push((key, value));
        }
    }
    let _ = bytes;
    (name, attrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_opml() {
        let xml = r#"<?xml version="1.0"?><opml><body>
        <outline title="Tech">
          <outline type="rss" title="Rust" xmlUrl="https://blog.rust-lang.org/feed.xml"/>
        </outline>
        </body></opml>"#;
        let outlines = parse_opml(xml);
        assert_eq!(outlines.len(), 1);
        assert_eq!(outlines[0].title, "Rust");
        assert_eq!(outlines[0].feed_url, "https://blog.rust-lang.org/feed.xml");
        assert_eq!(outlines[0].folder.as_deref(), Some("Tech"));
    }

    #[test]
    fn export_roundtrip() {
        let feeds = vec![Feed {
            id: crate::model::FeedId(1),
            folder_id: None,
            title: "Test".into(),
            site_url: Some("https://ex.com".into()),
            feed_url: "https://ex.com/rss".into(),
            last_error: None,
            muted: false,
        }];
        let xml = export_opml(&feeds);
        assert!(xml.contains("https://ex.com/rss"));
        assert!(xml.contains("Test"));
        let parsed = parse_opml(&xml);
        assert_eq!(parsed[0].feed_url, "https://ex.com/rss");
    }
}
