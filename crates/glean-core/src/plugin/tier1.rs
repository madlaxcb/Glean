//! Tier 1 配置驱动适配器。§11.5.2
//!
//! 形态：TOML manifest 中的 `[tier1]` 段定义 URL 模板 + JSON 字段映射。
//! Host 拉取 JSON → 按 `entries_json_path` 取条目数组 → 按字段映射构建 ParsedEntry。
//!
//! 与 Tier 2 (Rhai) 的区别：没有脚本执行，纯数据映射，攻击面最小。
//! 仍走 `feed_fetch` 域名白名单（manifest 中声明）。
//!
//! M5 状态：核心结构 + URL 模板替换 + JSON 路径查找已就绪。
//! Bilibili 端到端验证排到 M6（§11.5.11）。

use crate::error::{CoreError, Result};
use crate::feed::parse::{ParsedEntry, ParsedFeed};
use crate::feed::HttpClient;
use crate::plugin::manifest::Manifest;
use serde_json::Value;

/// 执行 Tier 1 适配器：根据 manifest 配置拉取 JSON 并映射成 ParsedFeed。
///
/// `source_url` 是用户输入的原始 URL（如 `https://space.bilibili.com/12345`），
/// 用于从 path 中提取变量替换到 `request_url_template`。
pub fn run(manifest: &Manifest, http: &HttpClient, source_url: &str) -> Result<ParsedFeed> {
    let tier1 = manifest
        .tier1
        .as_ref()
        .ok_or_else(|| CoreError::Message("Tier 1 manifest missing [tier1] config".into()))?;

    let request_url = render_template(&tier1.request_url_template, source_url)?;

    // §11.5.4 域名白名单强制校验。
    enforce_domain_whitelist(&request_url, &manifest.capabilities.feed_fetch)?;

    let body = fetch_json(http, &request_url)?;
    let root: Value =
        serde_json::from_str(&body).map_err(|e| CoreError::Parse(format!("tier1 json: {e}")))?;

    let entries_val = json_path(&root, &tier1.entries_json_path)?;
    let arr = entries_val.as_array().ok_or_else(|| {
        CoreError::Parse("tier1: entries_json_path did not resolve to array".into())
    })?;

    let mut entries = Vec::with_capacity(arr.len());
    for item in arr {
        entries.push(map_entry(item, &tier1.fields));
    }

    Ok(ParsedFeed {
        title: manifest.plugin.name.clone(),
        site_url: Some(source_url.to_string()),
        favicon_url: None,
        entries,
    })
}

/// 把 `https://api.example.com/users/{uid}/videos` 中的 `{uid}` 用
/// source_url 的 path 段替换。当前实现：`{uid}` → 第 1 段 path（数字段）。
/// M6 可扩展为 manifest 显式声明 `path_extract = { uid = "1" }`。
fn render_template(template: &str, source_url: &str) -> Result<String> {
    let parsed = url::Url::parse(source_url)
        .map_err(|e| CoreError::Message(format!("tier1: bad source url: {e}")))?;
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    let mut out = template.to_string();
    for (i, seg) in segments.iter().enumerate() {
        // 占位 `{n}` → 第 n 段（1-based）
        out = out.replace(&format!("{{{}}}", i + 1), seg);
    }
    // 命名占位 `{uid}`：取第一个数字段
    if let Some(num_seg) = segments
        .iter()
        .find(|s| s.chars().all(|c| c.is_ascii_digit()))
    {
        out = out.replace("{uid}", num_seg);
    }
    Ok(out)
}

fn enforce_domain_whitelist(url: &str, allowed: &[String]) -> Result<()> {
    if allowed.is_empty() {
        return Err(CoreError::Message(
            "tier1: manifest declares no feed_fetch domains".into(),
        ));
    }
    let parsed =
        url::Url::parse(url).map_err(|e| CoreError::Message(format!("tier1: bad url: {e}")))?;
    let host = parsed.host_str().unwrap_or("");
    let host = host.trim_start_matches("www.");
    let ok = allowed.iter().any(|d| {
        let d = d.trim_start_matches("www.");
        host == d || host.ends_with(&format!(".{d}"))
    });
    if ok {
        Ok(())
    } else {
        Err(CoreError::Message(format!(
            "tier1: domain {host} not in feed_fetch whitelist {allowed:?}"
        )))
    }
}

fn fetch_json(http: &HttpClient, url: &str) -> Result<String> {
    let resp = http
        .inner
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(
                "Glean/0.3.0 (+https://github.com/madlaxcb/Glean; RSS reader)",
            ),
        )
        .send()
        .map_err(|e| CoreError::Http(format!("tier1 fetch: {e}")))?;
    if !resp.status().is_success() {
        return Err(CoreError::Http(format!("tier1: HTTP {}", resp.status())));
    }
    resp.text()
        .map_err(|e| CoreError::Http(format!("tier1 body: {e}")))
}

fn json_path<'a>(root: &'a Value, path: &str) -> Result<&'a Value> {
    // `$` 或空字符串表示根
    if path == "$" || path.is_empty() {
        return Ok(root);
    }
    let path = path.strip_prefix("$.").unwrap_or(path);
    let mut cur = root;
    for seg in path.split('.') {
        if seg.is_empty() {
            continue;
        }
        // 数组下标
        if let Some(name) = seg.strip_suffix(']') {
            if let Some((field, idx)) = name.split_once('[') {
                if !field.is_empty() {
                    cur = cur.get(field).ok_or_else(|| {
                        CoreError::Parse(format!("tier1: path segment '{field}' not found"))
                    })?;
                }
                let i: usize = idx
                    .parse()
                    .map_err(|_| CoreError::Parse(format!("tier1: bad index '{idx}'")))?;
                cur = cur
                    .get(i)
                    .ok_or_else(|| CoreError::Parse(format!("tier1: index {i} out of range")))?;
                continue;
            }
        }
        cur = cur
            .get(seg)
            .ok_or_else(|| CoreError::Parse(format!("tier1: path segment '{seg}' not found")))?;
    }
    Ok(cur)
}

fn map_entry(item: &Value, fields: &crate::plugin::manifest::Tier1FieldMap) -> ParsedEntry {
    let guid = fields
        .guid
        .as_deref()
        .and_then(|p| json_path(item, p).ok())
        .and_then(value_to_string)
        .unwrap_or_else(|| {
            // 没有显式 guid 字段：用条目序号（item 在数组中的位置由调用方维护）
            // 这里取一个稳定 fallback：item 的 JSON 序列化的简短哈希。
            let s = serde_json::to_string(item).unwrap_or_default();
            format!("tier1-{:x}", fxhash(&s))
        });
    let title = fields
        .title
        .as_deref()
        .and_then(|p| json_path(item, p).ok())
        .and_then(value_to_string)
        .unwrap_or_else(|| "(no title)".into());
    let url = fields
        .url
        .as_deref()
        .and_then(|t| render_entry_template(t, item));
    let author = fields
        .author
        .as_deref()
        .and_then(|p| json_path(item, p).ok())
        .and_then(value_to_string);
    let published_at = fields
        .published_at
        .as_deref()
        .and_then(|p| json_path(item, p).ok())
        .and_then(value_to_i64);
    let summary = fields
        .summary
        .as_deref()
        .and_then(|p| json_path(item, p).ok())
        .and_then(value_to_string);

    // content_html：用模板，{var} 替换为条目字段值
    let content_html = match fields.content_html_template.as_deref() {
        Some(t) => render_entry_template(t, item)
            .unwrap_or_else(|| format!("<p>{}</p>", html_escape(&title))),
        None => format!("<p>{}</p>", html_escape(&title)),
    };

    // §11.5.4 / §11.5.5：Tier 1 产出的 HTML 在入库前必过 ammonia。
    // 此处返回原始 HTML，调用方 (service.rs) 走 upsert_entry 时会消毒。
    ParsedEntry {
        guid,
        title,
        url,
        author,
        published_at,
        summary,
        content_html,
        thumbnail: None,
    }
}

fn render_entry_template(template: &str, item: &Value) -> Option<String> {
    // 支持 `{json.path}` 形式：用点路径从 item 取值
    let mut out = template.to_string();
    // 简单循环替换所有 `{xxx.yyy}` 形式的占位
    while let Some(start) = out.find('{') {
        let rest = &out[start + 1..];
        let Some(end) = rest.find('}') else { break };
        let key = &rest[..end];
        let val = json_path(item, key)
            .ok()
            .and_then(value_to_string)
            .unwrap_or_default();
        out = format!("{}{}{}", &out[..start], val, &out[start + end + 2..]);
    }
    Some(out)
}

fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn value_to_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 极简 FNV-1a 64bit 哈希，用于生成 guid fallback。无需引入额外 crate。
fn fxhash(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::HttpClient;
    use crate::plugin::manifest::{
        Capabilities, Compliance, MatchRule, PluginMeta, Tier, Tier1Config, Tier1FieldMap,
    };

    fn bilibili_manifest() -> Manifest {
        Manifest {
            plugin: PluginMeta {
                id: "bilibili".into(),
                name: "Bilibili 用户投稿".into(),
                version: "0.1".into(),
                author: "".into(),
                min_glean_version: "".into(),
                tier: Tier::Config,
            },
            r#match: vec![MatchRule {
                url_pattern: "space.bilibili.com/*".into(),
            }],
            capabilities: Capabilities {
                feed_fetch: vec!["api.bilibili.com".into()],
                ..Default::default()
            },
            compliance: Compliance::default(),
            tier1: Some(Tier1Config {
                request_url_template: "https://api.bilibili.com/x/space/arc/search?mid={1}".into(),
                entries_json_path: "$.data.list.vlist".into(),
                fields: Tier1FieldMap {
                    guid: Some("$.bvid".into()),
                    title: Some("$.title".into()),
                    url: Some("https://www.bilibili.com/video/{bvid}".into()),
                    author: None,
                    published_at: Some("$.created".into()),
                    summary: Some("$.description".into()),
                    content_html_template: None,
                },
            }),
        }
    }

    #[test]
    fn render_template_replaces_numeric_placeholder() {
        let url = render_template(
            "https://api.example.com/users/{1}/videos",
            "https://space.example.com/12345",
        )
        .unwrap();
        assert_eq!(url, "https://api.example.com/users/12345/videos");
    }

    #[test]
    fn render_template_replaces_uid_named_placeholder() {
        let url = render_template(
            "https://api.example.com/users/{uid}/videos",
            "https://space.example.com/12345",
        )
        .unwrap();
        assert_eq!(url, "https://api.example.com/users/12345/videos");
    }

    #[test]
    fn enforce_domain_whitelist_rejects_unlisted() {
        let err = enforce_domain_whitelist("https://evil.com/x", &["api.bilibili.com".into()])
            .unwrap_err();
        assert!(matches!(err, CoreError::Message(_)));
    }

    #[test]
    fn enforce_domain_whitelist_allows_subdomain() {
        enforce_domain_whitelist("https://api.bilibili.com/x", &["bilibili.com".into()])
            .expect("subdomain allowed");
    }

    #[test]
    fn json_path_resolves_nested_array() {
        let v: Value = serde_json::from_str(
            r#"{"data": {"list": {"vlist": [{"bvid": "BV1xx", "title": "t"}]}}}"#,
        )
        .unwrap();
        let arr = json_path(&v, "$.data.list.vlist").unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 1);
    }

    #[test]
    fn map_entry_extracts_fields() {
        let item: Value = serde_json::from_str(
            r#"{"bvid": "BV1abc", "title": "hello", "created": 1700000000, "description": "d"}"#,
        )
        .unwrap();
        let fields = Tier1FieldMap {
            guid: Some("$.bvid".into()),
            title: Some("$.title".into()),
            url: Some("https://www.bilibili.com/video/{bvid}".into()),
            author: None,
            published_at: Some("$.created".into()),
            summary: Some("$.description".into()),
            content_html_template: None,
        };
        let entry = map_entry(&item, &fields);
        assert_eq!(entry.guid, "BV1abc");
        assert_eq!(entry.title, "hello");
        assert_eq!(
            entry.url.as_deref(),
            Some("https://www.bilibili.com/video/BV1abc")
        );
        assert_eq!(entry.published_at, Some(1700000000));
        assert_eq!(entry.summary.as_deref(), Some("d"));
        assert!(entry.content_html.contains("hello"));
    }

    #[test]
    fn map_entry_template_renders_html() {
        let item: Value =
            serde_json::from_str(r#"{"bvid": "BV1", "title": "T", "pic": "https://img/x.jpg"}"#)
                .unwrap();
        let fields = Tier1FieldMap {
            guid: Some("$.bvid".into()),
            title: Some("$.title".into()),
            url: Some("https://www.bilibili.com/video/{bvid}".into()),
            author: None,
            published_at: None,
            summary: None,
            content_html_template: Some(r#"<p>{title}</p><img src="{pic}">"#.into()),
        };
        let entry = map_entry(&item, &fields);
        assert_eq!(
            entry.content_html,
            r#"<p>T</p><img src="https://img/x.jpg">"#
        );
    }

    #[test]
    fn manifest_round_trip() {
        // 验证 bilibili manifest 能正常构造
        let m = bilibili_manifest();
        assert_eq!(m.plugin.tier, Tier::Config);
        assert!(m.tier1.is_some());
    }

    /// 端到端验证：真实 GitHub releases JSON API（匿名，60/h 限速）。
    /// 手动跑：`cargo test -p glean-core -- --ignored tier1_github_releases_end_to_end`
    ///
    /// 选 GitHub releases 是因为：匿名稳定、字段清晰、能测试 URL 模板多段替换
    /// （`{1}`/`{2}`）+ 域名白名单 + json_path 根数组（`$`）+ 字段映射全链路。
    #[test]
    #[ignore = "需联网访问 api.github.com（匿名限速 60/h）"]
    fn tier1_github_releases_end_to_end() {
        let manifest = Manifest {
            plugin: PluginMeta {
                id: "github-releases".into(),
                name: "GitHub Releases".into(),
                version: "0.1".into(),
                author: "".into(),
                min_glean_version: "".into(),
                tier: Tier::Config,
            },
            r#match: vec![MatchRule {
                url_pattern: "github.com/*/*".into(),
            }],
            capabilities: Capabilities {
                feed_fetch: vec!["api.github.com".into()],
                ..Default::default()
            },
            compliance: Compliance::default(),
            tier1: Some(Tier1Config {
                request_url_template: "https://api.github.com/repos/{1}/{2}/releases".into(),
                entries_json_path: "$".into(),
                fields: Tier1FieldMap {
                    guid: Some("$.id".into()),
                    title: Some("$.tag_name".into()),
                    url: Some("{html_url}".into()),
                    author: Some("$.author.login".into()),
                    // GitHub 返回 ISO8601 字符串；当前 value_to_i64 只解析整数/整数字符串，
                    // 映射失败 → published_at = None。已知限制，后续评估是否扩展。
                    published_at: Some("$.published_at".into()),
                    summary: Some("$.body".into()),
                    content_html_template: None,
                },
            }),
        };
        let http = HttpClient::default();
        let feed = run(&manifest, &http, "https://github.com/rust-lang/rust")
            .expect("tier1 github releases 应成功");
        assert!(!feed.entries.is_empty(), "应至少拿到 1 个 release");
        let first = &feed.entries[0];
        assert!(!first.title.is_empty(), "tag_name 不应为空");
        assert!(first.title != "(no title)", "title 不应是 fallback");
        assert!(
            matches!(first.url.as_deref(), Some(u) if u.starts_with("https://")),
            "url 应是 https"
        );
        assert!(!first.guid.is_empty(), "release id 不应为空");
    }
}
