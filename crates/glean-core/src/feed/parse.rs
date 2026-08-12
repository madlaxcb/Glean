use crate::error::{CoreError, Result};
use crate::model::ImagePolicy;
use crate::sanitize::sanitize_html_with_policy;
use feed_rs::model::{Content, Text};
use feed_rs::parser;
use scraper::{Html, Selector};

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
        // 从 Media RSS 扩展中提取缩略图（YouTube、Vimeo 等视频 feed 使用
        // <media:thumbnail> 提供封面图）。取第一个 media object 的第一个
        // thumbnail；如果 media object 有 content 字段标记为 image，也尝试
        // 从中提取。
        let thumbnail = e
            .media
            .iter()
            .find_map(|mo| {
                mo.thumbnails
                    .first()
                    .map(|t| t.image.uri.clone())
                    .or_else(|| {
                        mo.content
                            .iter()
                            .find(|c| {
                                c.content_type
                                    .as_ref()
                                    .is_some_and(|kind| kind.to_string().starts_with("image/"))
                            })
                            .and_then(|c| c.url.as_ref().map(ToString::to_string))
                    })
            })
            .or_else(|| {
                // 回退：从 content_html 中的 <img> 标签提取第一个图片 URL
                extract_first_img_url(&content_html)
            });
        entries.push(ParsedEntry {
            guid,
            title,
            url,
            author,
            published_at,
            summary,
            content_html,
            thumbnail,
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

/// 从 HTML 中提取第一个 <img> 标签的 src 属性值。
fn extract_first_img_url(html: &str) -> Option<String> {
    let selector = Selector::parse("img[src]").ok()?;
    let document = Html::parse_fragment(html);
    let url = document
        .select(&selector)
        .find_map(|image| image.value().attr("src"))?;
    if url.starts_with("http") {
        Some(url.to_string())
    } else {
        None
    }
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

    #[test]
    fn extracts_youtube_media_thumbnail() {
        let sample = br#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:media="http://search.yahoo.com/mrss/">
  <title>Test Channel</title>
  <link rel="alternate" href="https://www.youtube.com/channel/UCxxxx"/>
  <entry>
    <id>yt:video:abc123</id>
    <title>Test Video</title>
    <link rel="alternate" href="https://www.youtube.com/watch?v=abc123"/>
    <published>2026-01-01T00:00:00+00:00</published>
    <author><name>Test Author</name></author>
    <media:group>
      <media:title>Test Video</media:title>
      <media:content url="https://www.youtube.com/v/abc123" type="application/x-shockwave-flash"/>
      <media:thumbnail url="https://i1.ytimg.com/vi/abc123/hqdefault.jpg" width="480" height="360"/>
      <media:description>Video description here</media:description>
    </media:group>
  </entry>
</feed>"#;
        let f = parse_feed(sample).unwrap();
        assert_eq!(f.entries.len(), 1);
        assert_eq!(
            f.entries[0].thumbnail.as_deref(),
            Some("https://i1.ytimg.com/vi/abc123/hqdefault.jpg")
        );
    }

    #[test]
    fn falls_back_to_img_tag_for_thumbnail() {
        let sample = br#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Img Test</title>
    <item>
      <title>Post with image</title>
      <link>https://example.com/2</link>
      <guid>guid-2</guid>
      <description>&lt;p&gt;&lt;img src="https://example.com/photo.jpg"/&gt;Some text&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#;
        let f = parse_feed(sample).unwrap();
        assert_eq!(f.entries.len(), 1);
        assert_eq!(
            f.entries[0].thumbnail.as_deref(),
            Some("https://example.com/photo.jpg")
        );
    }

    /// ── 21 平台全链路验证 ──────────────────────────────────────
    ///
    /// 对每个支持的平台构造最小 RSS/Atom fixture，验证：
    /// 1. tier0::normalize 将用户输入 URL 转为正确的 feed URL
    /// 2. parse_feed 能解析该平台典型 feed 格式
    /// 3. categorize 将 feed URL 归入正确分类
    ///
    /// 插件平台（Pixiv/Bilibili/Civitai/Fanbox/Fantia）不走 RSS 解析，
    /// 只验证 normalize + categorize。

    /// 单个平台的测试描述。
    struct PlatformCase {
        name: &'static str,
        /// 用户输入的 URL（添加订阅时粘贴的地址）
        input_url: &'static str,
        /// tier0 规范化后的 feed URL
        normalized_url: &'static str,
        /// 该平台典型 feed 的 XML 文本
        feed_xml: &'static str,
        /// 期望的分类
        expected_category: crate::model::FeedCategory,
        /// 期望解析出的条目数
        expected_entry_count: usize,
        /// 期望首条 entry 的 title 包含此子串
        expected_title_contains: &'static str,
    }

    #[test]
    fn all_21_platforms_full_pipeline() {
        use crate::feed::categorize::categorize;
        use crate::feed::tier0::normalize;

        let cases: Vec<PlatformCase> = vec![
            // ── 海外 RSS/Atom 平台 ──
            PlatformCase {
                name: "GitHub",
                input_url: "https://github.com/rust-lang/rust",
                normalized_url: "https://github.com/rust-lang/rust/releases.atom",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>rust-lang/rust releases</title>
  <link rel="alternate" href="https://github.com/rust-lang/rust"/>
  <entry>
    <id>tag:github.com,2008:Repository/rust-lang/rust/v1.75.0</id>
    <title>1.75.0</title>
    <link rel="alternate" href="https://github.com/rust-lang/rust/releases/tag/v1.75.0"/>
    <published>2026-01-15T00:00:00Z</published>
    <content type="html">&lt;p&gt;Rust 1.75.0 release&lt;/p&gt;</content>
  </entry>
</feed>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "1.75.0",
            },
            PlatformCase {
                name: "GitLab",
                input_url: "https://gitlab.com/gitlab-org/gitlab",
                normalized_url: "https://gitlab.com/gitlab-org/gitlab/-/releases.atom",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>GitLab Releases</title>
  <link rel="alternate" href="https://gitlab.com/gitlab-org/gitlab"/>
  <entry>
    <id>https://gitlab.com/gitlab-org/gitlab/-/releases/v16.0</id>
    <title>v16.0</title>
    <link rel="alternate" href="https://gitlab.com/gitlab-org/gitlab/-/releases/v16.0"/>
    <published>2026-02-01T00:00:00Z</published>
    <summary>GitLab 16.0 release</summary>
  </entry>
</feed>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "v16.0",
            },
            PlatformCase {
                name: "YouTube",
                input_url: "https://www.youtube.com/channel/UCxxxx",
                normalized_url: "https://www.youtube.com/feeds/videos.xml?channel_id=UCxxxx",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:media="http://search.yahoo.com/mrss/">
  <title>Test YT Channel</title>
  <link rel="alternate" href="https://www.youtube.com/channel/UCxxxx"/>
  <entry>
    <id>yt:video:vid1</id>
    <title>My Video</title>
    <link rel="alternate" href="https://www.youtube.com/watch?v=vid1"/>
    <published>2026-03-01T00:00:00+00:00</published>
    <author><name>TestAuthor</name></author>
    <media:group>
      <media:thumbnail url="https://i1.ytimg.com/vi/vid1/hqdefault.jpg" width="480" height="360"/>
      <media:description>Video desc</media:description>
    </media:group>
  </entry>
</feed>"#,
                expected_category: crate::model::FeedCategory::Video,
                expected_entry_count: 1,
                expected_title_contains: "My Video",
            },
            PlatformCase {
                name: "Medium",
                input_url: "https://medium.com/@testuser",
                normalized_url: "https://medium.com/feed/@testuser",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>Test User on Medium</title>
    <link>https://medium.com/@testuser</link>
    <item>
      <title>My Article</title>
      <link>https://medium.com/@testuser/my-article-abc123</link>
      <guid>https://medium.com/p/abc123</guid>
      <dc:creator>Test User</dc:creator>
      <pubDate>Sat, 01 Mar 2026 00:00:00 GMT</pubDate>
      <description>&lt;p&gt;Article content&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "My Article",
            },
            PlatformCase {
                name: "Reddit",
                input_url: "https://www.reddit.com/r/rust",
                normalized_url: "https://www.reddit.com/r/rust/.rss",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>rust subreddit</title>
  <link rel="alternate" href="https://www.reddit.com/r/rust/"/>
  <entry>
    <id>t3_abc123</id>
    <title>New Rust crate released</title>
    <link rel="alternate" href="https://www.reddit.com/r/rust/comments/abc123/"/>
    <published>2026-04-01T00:00:00+00:00</published>
    <author><name>u/testuser</name></author>
    <content type="html">&lt;p&gt;Check out this crate&lt;/p&gt;</content>
  </entry>
</feed>"#,
                expected_category: crate::model::FeedCategory::Social,
                expected_entry_count: 1,
                expected_title_contains: "Rust crate",
            },
            PlatformCase {
                name: "Steam",
                input_url: "https://store.steampowered.com/app/570",
                normalized_url: "https://store.steampowered.com/feeds/news/app/570/",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Dota 2 News</title>
    <link>https://store.steampowered.com/app/570</link>
    <item>
      <title>Dota 2 Update</title>
      <link>https://store.steampowered.com/news/12345</link>
      <guid>12345</guid>
      <pubDate>Mon, 01 Apr 2026 00:00:00 GMT</pubDate>
      <description>&lt;p&gt;Patch notes&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "Dota 2 Update",
            },
            PlatformCase {
                name: "Mastodon",
                input_url: "https://mastodon.social/@testuser",
                normalized_url: "https://mastodon.social/@testuser.rss",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>testuser@mastodon.social</title>
    <link>https://mastodon.social/@testuser</link>
    <item>
      <title>Hello from Mastodon</title>
      <link>https://mastodon.social/@testuser/123456</link>
      <guid>https://mastodon.social/@testuser/123456</guid>
      <pubDate>Tue, 01 Apr 2026 12:00:00 GMT</pubDate>
      <description>&lt;p&gt;Toot content&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Social,
                expected_entry_count: 1,
                expected_title_contains: "Mastodon",
            },
            PlatformCase {
                name: "Substack",
                input_url: "https://testpub.substack.com",
                normalized_url: "https://testpub.substack.com/feed",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>Test Publication</title>
    <link>https://testpub.substack.com</link>
    <item>
      <title>Weekly Digest</title>
      <link>https://testpub.substack.com/p/weekly-digest</link>
      <guid>https://testpub.substack.com/p/weekly-digest</guid>
      <dc:creator>Author</dc:creator>
      <pubDate>Wed, 01 Jan 2026 00:00:00 GMT</pubDate>
      <description>&lt;p&gt;Newsletter content&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "Weekly Digest",
            },
            // ── 国内 RSS/Atom 平台 ──
            PlatformCase {
                name: "知乎",
                input_url: "https://www.zhihu.com",
                normalized_url: "https://www.zhihu.com/rss",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
  <channel>
    <title>知乎每日精选</title>
    <link>http://www.zhihu.com</link>
    <item>
      <title>有哪些令人叹为观止的细节？</title>
      <link>http://www.zhihu.com/question/63537524/answer/3364481763</link>
      <guid>http://www.zhihu.com/question/63537524/answer/3364481763</guid>
      <pubDate>Wed, 31 Jan 2026 11:59:49 +0800</pubDate>
      <description>知乎回答摘要</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "细节",
            },
            PlatformCase {
                name: "IT之家",
                input_url: "https://www.ithome.com",
                normalized_url: "https://www.ithome.com/rss/",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>IT之家</title>
    <link>https://www.ithome.com</link>
    <item>
      <title>Windows 12 正式发布</title>
      <link>https://www.ithome.com/0/800/123.htm</link>
      <guid>https://www.ithome.com/0/800/123.htm</guid>
      <pubDate>Mon, 10 Mar 2026 08:00:00 +0800</pubDate>
      <description>&lt;p&gt;微软今日发布 Windows 12&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "Windows",
            },
            PlatformCase {
                name: "爱范儿",
                input_url: "https://www.ifanr.com",
                normalized_url: "https://www.ifanr.com/feed",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>爱范儿</title>
    <link>https://www.ifanr.com</link>
    <item>
      <title>苹果春季发布会汇总</title>
      <link>https://www.ifanr.com/1234567</link>
      <guid>https://www.ifanr.com/1234567</guid>
      <pubDate>Tue, 11 Mar 2026 10:00:00 +0800</pubDate>
      <description>&lt;p&gt;苹果新品速览&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "苹果",
            },
            PlatformCase {
                name: "机核",
                input_url: "https://www.gcores.com",
                normalized_url: "https://www.gcores.com/rss",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>机核</title>
    <link>https://www.gcores.com</link>
    <item>
      <title>游戏早报</title>
      <link>https://www.gcores.com/articles/123456</link>
      <guid>https://www.gcores.com/articles/123456</guid>
      <pubDate>Wed, 12 Mar 2026 07:00:00 +0800</pubDate>
      <description>&lt;p&gt;今日游戏资讯&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "游戏",
            },
            PlatformCase {
                name: "V2EX",
                input_url: "https://www.v2ex.com",
                normalized_url: "https://www.v2ex.com/index.xml",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>V2EX</title>
  <link rel="alternate" href="https://www.v2ex.com/"/>
  <entry>
    <id>https://www.v2ex.com/t/123456</id>
    <title>Rust 学习资源推荐</title>
    <link rel="alternate" href="https://www.v2ex.com/t/123456"/>
    <published>2026-03-13T00:00:00+08:00</published>
    <author><name>v2exer</name></author>
    <summary>推荐几个 Rust 入门资源</summary>
  </entry>
</feed>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "Rust",
            },
            PlatformCase {
                name: "LinuxDo",
                input_url: "https://linux.do",
                normalized_url: "https://linux.do/latest.rss",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
  <channel>
    <title>LinuxDo</title>
    <link>https://linux.do</link>
    <item>
      <title>Linux 内核 7.0 发布</title>
      <link>https://linux.do/t/topic/123456</link>
      <guid>https://linux.do/t/topic/123456</guid>
      <pubDate>Thu, 13 Mar 2026 09:00:00 +0800</pubDate>
      <description>&lt;p&gt;内核更新内容&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "Linux",
            },
            PlatformCase {
                name: "美团技术",
                input_url: "https://tech.meituan.com",
                normalized_url: "https://tech.meituan.com/feed",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>美团技术团队</title>
    <link>https://tech.meituan.com</link>
    <item>
      <title>美团分布式架构实践</title>
      <link>https://tech.meituan.com/2026/03/arch-practice.html</link>
      <guid>https://tech.meituan.com/2026/03/arch-practice.html</guid>
      <pubDate>Fri, 14 Mar 2026 00:00:00 +0800</pubDate>
      <description>&lt;p&gt;架构演进总结&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "美团",
            },
            PlatformCase {
                name: "酷壳",
                input_url: "https://coolshell.cn",
                normalized_url: "https://coolshell.cn/feed",
                feed_xml: r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>酷壳 CoolShell</title>
    <link>https://coolshell.cn</link>
    <item>
      <title>程序员练级攻略 2026</title>
      <link>https://coolshell.cn/articles/12345.html</link>
      <guid>https://coolshell.cn/articles/12345.html</guid>
      <pubDate>Sat, 15 Mar 2026 00:00:00 +0800</pubDate>
      <description>&lt;p&gt;技术成长路线&lt;/p&gt;</description>
    </item>
  </channel>
</rss>"#,
                expected_category: crate::model::FeedCategory::Article,
                expected_entry_count: 1,
                expected_title_contains: "程序员",
            },
        ];

        // ── 验证 16 个 RSS/Atom 平台 ──
        for case in &cases {
            // 1. URL 规范化
            let norm = normalize(case.input_url);
            assert_eq!(
                norm, case.normalized_url,
                "[{}] normalize: expected {}, got {}",
                case.name, case.normalized_url, norm
            );

            // 2. Feed 解析
            let parsed = parse_feed(case.feed_xml.as_bytes())
                .unwrap_or_else(|e| panic!("[{}] parse_feed failed: {e}", case.name));
            assert_eq!(
                parsed.entries.len(),
                case.expected_entry_count,
                "[{}] entry count",
                case.name
            );
            assert!(
                parsed.entries[0]
                    .title
                    .contains(case.expected_title_contains),
                "[{}] title '{}' should contain '{}'",
                case.name,
                parsed.entries[0].title,
                case.expected_title_contains
            );

            // 3. 分类
            let cat = categorize(case.normalized_url);
            assert_eq!(
                cat, case.expected_category,
                "[{}] categorize: expected {:?}, got {:?}",
                case.name, case.expected_category, cat
            );
        }

        // ── 验证 5 个插件平台（normalize + categorize，无 RSS 解析）──
        let plugin_cases: Vec<(&str, &str, &str, crate::model::FeedCategory)> = vec![
            // Pixiv: /user/{id} → /users/{id}
            (
                "Pixiv",
                "https://www.pixiv.net/user/123456",
                "https://www.pixiv.net/users/123456",
                crate::model::FeedCategory::Image,
            ),
            // Bilibili: 不做 Tier 0 改写，原样走插件
            (
                "Bilibili",
                "https://space.bilibili.com/3428150",
                "https://space.bilibili.com/3428150",
                crate::model::FeedCategory::Video,
            ),
            // Civitai: 不做 Tier 0 改写，原样走插件
            (
                "Civitai",
                "https://civitai.com/user/madlaxcb",
                "https://civitai.com/user/madlaxcb",
                crate::model::FeedCategory::Article,
            ),
            // Fanbox: 不做 Tier 0 改写，原样走插件
            (
                "Fanbox",
                "https://madlaxcb.fanbox.cc",
                "https://madlaxcb.fanbox.cc",
                crate::model::FeedCategory::Image,
            ),
            // Fantia: 不做 Tier 0 改写，原样走插件
            (
                "Fantia",
                "https://fantia.jp/fanclubs/509981",
                "https://fantia.jp/fanclubs/509981",
                crate::model::FeedCategory::Article,
            ),
        ];

        for (name, input, expected_norm, expected_cat) in &plugin_cases {
            let norm = normalize(input);
            assert_eq!(
                norm, *expected_norm,
                "[{}] normalize: expected {}, got {}",
                name, expected_norm, norm
            );
            let cat = categorize(norm.as_str());
            assert_eq!(
                cat, *expected_cat,
                "[{}] categorize: expected {:?}, got {:?}",
                name, expected_cat, cat
            );
        }
    }
}
