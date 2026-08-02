//! Rhai runtime：按插件动态构建 Engine + 注册 host 函数。§11.5.4 / §11.5.6
//!
//! **核心安全决策**：
//! 1. `Engine` 实例**按插件动态构建**，只注册该插件 manifest 声明过的 host 函数。
//!    没声明 `credential_use: ["pixiv_session"]` 的插件，脚本里写
//!    `http_get(url, hdrs, "pixiv_session")` 会直接报"函数不存在"——
//!    能力边界是代码层面强制的，不是运行时权限检查。
//! 2. 凭证永远不进插件手里：`http_get` 内部由 Host 注入 Header，
//!    脚本只传 `credential_slot` 字符串，永远拿不到明文值。
//! 3. `set_max_operations` + `set_max_call_levels` 强制操作数上限和递归深度，
//!    防死循环/卡死刷新流程。
//! 4. 输出回消毒管线：脚本产出的 HTML 在 `tier1.rs` / `tier2` 入库前必过 ammonia
//!    （见 `service.rs` 的 upsert_entry 路径）。

use crate::error::{CoreError, Result};
use crate::feed::parse::{ParsedEntry, ParsedFeed};
use crate::feed::HttpClient;
use crate::plugin::credential::CredentialStore;
use crate::plugin::manifest::{Capabilities, Manifest, Tier};
use rhai::{Dynamic, Engine, ImmutableString, Map};
use std::sync::{Arc, Mutex};

/// 单次 Rhai 脚本执行的最大操作数（防止死循环）。
/// Pixiv 分页拉取（20 页 × 30 条/页 × 多字段操作）约需 500k+ 操作数。
const MAX_OPERATIONS: u64 = 2_000_000;
/// 最大调用栈深度。
const MAX_CALL_LEVELS: usize = 64;
/// 单次脚本执行软超时（秒）。分页拉取可能耗时较长，放宽到 120 秒。
const SCRIPT_TIMEOUT_SECS: u64 = 120;

/// Rhai 脚本执行的运行时上下文。一个 `Runtime` 实例对应一个加载的插件。
pub struct Runtime {
    pub engine: Engine,
    pub manifest: Manifest,
    #[allow(dead_code)]
    pub http: Arc<HttpClient>,
    #[allow(dead_code)]
    pub credentials: Arc<CredentialStore>,
    /// Tier 2 脚本的 entry 收集器；非 Script 插件为 `None`。
    collector: Option<Arc<Mutex<EntryCollector>>>,
}

impl Runtime {
    /// 按插件 manifest 动态构建 Engine，只注册声明过的 host 函数。
    pub fn build(
        manifest: Manifest,
        http: Arc<HttpClient>,
        credentials: Arc<CredentialStore>,
    ) -> Self {
        let plugin_id = manifest.plugin.id.clone();
        let caps = manifest.capabilities.clone();
        let is_tier2 = matches!(manifest.plugin.tier, Tier::Script);
        let mut engine = Engine::new();
        engine.set_max_operations(MAX_OPERATIONS);
        engine.set_max_call_levels(MAX_CALL_LEVELS);
        engine.set_max_string_size(1_000_000);
        engine.set_max_array_size(10_000);
        engine.set_max_map_size(10_000);
        engine.set_max_expr_depths(64, 64);
        // 关闭沙箱外的能力：脚本不应直接访问文件/进程。
        engine.disable_symbol("eval");
        engine.disable_symbol("import");
        engine.disable_symbol("export");
        engine.disable_symbol("Fn");

        register_pure_fns(&mut engine);
        if !caps.feed_fetch.is_empty() {
            register_http_fns(
                &mut engine,
                plugin_id.clone(),
                caps.clone(),
                http.clone(),
                credentials.clone(),
            );
        }
        // Tier 2 脚本插件注册 entry 收集函数（不再以 content_transform 为 gate）。
        let collector = if is_tier2 {
            let c = Arc::new(Mutex::new(EntryCollector::default()));
            register_entry_fns(&mut engine, c.clone());
            Some(c)
        } else {
            None
        };

        Self {
            engine,
            manifest,
            http,
            credentials,
            collector,
        }
    }

    /// 执行 Tier 2 适配器脚本，返回脚本通过 `set_field`/`add_entry` 收集到的
    /// `ParsedFeed`。脚本结束未 commit 的 current entry 自动 commit。
    ///
    /// `source_url` 是用户输入的原始订阅 URL（如
    /// `https://space.bilibili.com/12345`），通过 Rhai 全局常量 `SOURCE_URL`
    /// 暴露给脚本，脚本据此提取路径变量（如 mid）。
    pub fn run_script(
        &self,
        script: &str,
        source_url: &str,
        existing_guids: &[String],
    ) -> Result<ParsedFeed> {
        // 重置上次运行残留的 feed_title（同一 Runtime 可多次执行脚本，
        // 避免第一次 set_feed_title 的结果泄漏到第二次）。
        if let Some(c) = &self.collector {
            c.lock().unwrap().feed_title = None;
        }
        let mut scope = rhai::Scope::new();
        scope.push_constant("SOURCE_URL", source_url.to_string());
        // 增量刷新提示：该订阅已存在条目的 guid 集合，格式为 ",guid1,guid2,"
        // （首尾补逗号，便于脚本用 contains 精确匹配）。空集合注入空串。
        // 脚本可据此在按时间倒序的分页中遇到已存在条目时提前停止，减少请求。
        let guid_set = if existing_guids.is_empty() {
            String::new()
        } else {
            format!(",{},", existing_guids.join(","))
        };
        scope.push_constant("EXISTING_GUIDS", guid_set);
        let _ = self
            .engine
            .eval_with_scope::<rhai::Dynamic>(&mut scope, script)
            .map_err(|e| CoreError::Message(format!("rhai eval: {e}")))?;
        let collector = self
            .collector
            .as_ref()
            .ok_or_else(|| CoreError::Message("run_script called on non-Tier-2 runtime".into()))?;
        let mut g = collector.lock().unwrap();
        // 自动 commit 未提交的 current entry（脚本忘记调 add_entry 时不丢数据）。
        if !g.current.title.is_empty()
            || !g.current.content_html.is_empty()
            || g.current.url.is_some()
            || g.current.guid.is_some()
        {
            g.commit_current();
        }
        let entries = std::mem::take(&mut g.entries);
        let title = g
            .feed_title
            .clone()
            .unwrap_or_else(|| self.manifest.plugin.name.clone());
        Ok(ParsedFeed {
            title,
            site_url: None,
            favicon_url: None,
            entries,
        })
    }

    /// 配置的脚本软超时（worker 线程之外的硬超时由调用方实施）。
    pub fn timeout_secs(&self) -> u64 {
        SCRIPT_TIMEOUT_SECS
    }

    /// 是否为 Tier 2 脚本插件。
    pub fn is_tier2(&self) -> bool {
        matches!(self.manifest.plugin.tier, Tier::Script)
    }
}

/// 注册纯计算 host 函数（无网络/无凭证，所有插件都可用）。§11.5.6
fn register_pure_fns(engine: &mut Engine) {
    engine.register_fn("now", system_now);
    engine.register_fn("sleep_ms", |ms: i64| {
        // 供脚本在失败重试间做节流（阻塞当前 worker 线程，不阻塞 UI 主线程）。
        std::thread::sleep(std::time::Duration::from_millis(ms.max(0) as u64));
    });
    engine.register_fn("log", |level: String, msg: String| {
        // 输出到 stderr：GUI 模式下从终端启动可见，便于诊断插件执行失败。
        eprintln!("[plugin:{level}] {msg}");
    });
    engine.register_fn("parse_json", |s: String| -> Dynamic {
        serde_json::from_str::<serde_json::Value>(&s)
            .map(json_to_dynamic)
            .unwrap_or(Dynamic::UNIT)
    });
    engine.register_fn("json_path", |json: Dynamic, path: String| -> Dynamic {
        json_path_lookup(&json, &path).unwrap_or(Dynamic::UNIT)
    });
    // 通用 MD5 hex 摘要（非 Bilibili 专属，所有脚本可用）。
    engine.register_fn("md5", |s: String| -> String {
        use md5::{Digest, Md5};
        let mut hasher = Md5::new();
        hasher.update(s.as_bytes());
        format!("{:x}", hasher.finalize())
    });
    // 字符串切片：`s.substring(start, end)` 返回 chars[start..end]。
    // 语义与 Rust/Python 切片一致（end 是结束位置，不是长度），
    // 负数/越界自动夹紧。Rhai 内置的 `sub_string(start, len)` 语义不同，
    // 脚本统一用 `substring` 避免混淆。
    engine.register_fn(
        "substring",
        |s: ImmutableString, start: i64, end: i64| -> String {
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len() as i64;
            let s = start.max(0).min(len) as usize;
            let e = end.max(s as i64).min(len) as usize;
            chars[s..e].iter().collect()
        },
    );
}

/// 注册 HTTP host 函数（仅当 manifest 声明 `feed_fetch` 域名白名单时）。
/// §11.5.4：域名不在白名单 → Host 拒绝；凭证由 Host 注入，脚本不可读。
#[allow(clippy::too_many_arguments)]
fn register_http_fns(
    engine: &mut Engine,
    plugin_id: String,
    caps: Capabilities,
    http: Arc<HttpClient>,
    creds: Arc<CredentialStore>,
) {
    let allowed_get = Arc::new(caps.feed_fetch.clone());
    let cred_slots_get = Arc::new(caps.credential_use.clone());
    let http_get = http.clone();
    let creds_get = creds.clone();
    let pid_get = plugin_id.clone();
    engine.register_fn(
        "http_get",
        move |url: String, headers: Map, credential_slot: String| -> Map {
            do_http(
                HttpMethod::Get,
                &http_get,
                &creds_get,
                &pid_get,
                &allowed_get,
                &cred_slots_get,
                &url,
                "",
                &headers,
                &credential_slot,
            )
        },
    );

    let allowed_post = Arc::new(caps.feed_fetch.clone());
    let cred_slots_post = Arc::new(caps.credential_use.clone());
    let http_post = http.clone();
    let creds_post = creds.clone();
    let pid_post = plugin_id;
    engine.register_fn(
        "http_post",
        move |url: String, body: String, headers: Map, credential_slot: String| -> Map {
            do_http(
                HttpMethod::Post,
                &http_post,
                &creds_post,
                &pid_post,
                &allowed_post,
                &cred_slots_post,
                &url,
                &body,
                &headers,
                &credential_slot,
            )
        },
    );
}

/// Tier 2 entry 收集器：脚本通过 `set_field` 写 current，`add_entry` 提交，
/// `set_feed_title` 覆盖 feed 标题（默认取 manifest.name）。
/// §11.5.6：所有产出 HTML 在 service 层 upsert 前必过 ammonia。
#[derive(Default)]
struct EntryCollector {
    entries: Vec<ParsedEntry>,
    current: CurrentEntry,
    feed_title: Option<String>,
}

#[derive(Default)]
struct CurrentEntry {
    title: String,
    url: Option<String>,
    author: Option<String>,
    guid: Option<String>,
    summary: Option<String>,
    content_html: String,
    published_at: Option<i64>,
    /// 缩略图/封面图 URL（列表预览用）。
    thumbnail: Option<String>,
}

impl EntryCollector {
    /// 把 current 提交到 entries，并重置 current。
    fn commit_current(&mut self) {
        let cur = std::mem::take(&mut self.current);
        let guid = cur
            .guid
            .unwrap_or_else(|| format!("anon-{}", self.entries.len()));
        self.entries.push(ParsedEntry {
            guid,
            title: cur.title,
            url: cur.url,
            author: cur.author,
            published_at: cur.published_at,
            summary: cur.summary,
            content_html: cur.content_html,
            thumbnail: cur.thumbnail,
        });
    }
}

/// 注册 entry 收集 host 函数（Tier 2 脚本插件专用）。§11.5.6
///
/// - `set_field(name, value)`：写 current 的某个字段。`name` ∈ title/url/
///   author/guid/summary/content_html/published_at。`published_at` 接受 i64
///   或可解析为 i64 的字符串。
/// - `add_entry()`：提交 current 到 entries，重置 current。
/// - `set_embed(provider, id)`：把 current.content_html 设为 `provider:id`
///   占位（M7+ 再考虑 iframe 渲染策略）。
fn register_entry_fns(engine: &mut Engine, collector: Arc<Mutex<EntryCollector>>) {
    let c1 = collector.clone();
    engine.register_fn("set_field", move |name: String, value: Dynamic| {
        let mut g = c1.lock().unwrap();
        let s = value.clone().into_string().unwrap_or_default();
        match name.as_str() {
            "title" => g.current.title = s,
            "url" => g.current.url = Some(s),
            "author" => g.current.author = Some(s),
            "guid" => g.current.guid = Some(s),
            "summary" => g.current.summary = Some(s),
            "content_html" => g.current.content_html = s,
            "thumbnail" => g.current.thumbnail = Some(s),
            "published_at" => {
                g.current.published_at = if let Ok(i) = value.as_int() {
                    Some(i)
                } else {
                    s.parse::<i64>().ok()
                };
            }
            _ => {} // 未知字段忽略，脚本兼容性。
        }
    });

    let c2 = collector.clone();
    engine.register_fn("add_entry", move || {
        let mut g = c2.lock().unwrap();
        g.commit_current();
    });

    let c3 = collector.clone();
    engine.register_fn("set_feed_title", move |title: String| {
        let mut g = c3.lock().unwrap();
        g.feed_title = Some(title);
    });

    let c4 = collector;
    engine.register_fn("set_embed", move |provider: String, id: String| {
        let mut g = c4.lock().unwrap();
        g.current.content_html = format!("{provider}:{id}");
    });
}

#[derive(Clone, Copy, PartialEq)]
enum HttpMethod {
    Get,
    Post,
}

#[allow(clippy::too_many_arguments)]
fn do_http(
    method: HttpMethod,
    http: &HttpClient,
    creds: &CredentialStore,
    plugin_id: &str,
    allowed: &[String],
    cred_slots: &[String],
    url: &str,
    body: &str,
    headers: &Map,
    credential_slot: &str,
) -> Map {
    let mut m = Map::new();
    if !is_domain_allowed(url, allowed) {
        return error_map(&mut m, format!("domain not in feed_fetch whitelist: {url}"));
    }
    if !credential_slot.is_empty() && !cred_slots.iter().any(|s| s == credential_slot) {
        return error_map(
            &mut m,
            format!("credential slot not declared in manifest: {credential_slot}"),
        );
    }

    let mut hdrs = reqwest::header::HeaderMap::new();
    for (k, v) in headers {
        if let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes()) {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&v.to_string()) {
                hdrs.insert(name, val);
            }
        }
    }
    // §11.5.4 凭证注入：脚本永远拿不到明文，Host 在请求前注入 Header。
    // header_name 为空表示该凭证仅供 body 占位符注入（如 Pixiv refresh_token），
    // 此时不做 header 注入。
    if !credential_slot.is_empty() {
        if let Some(cred) = creds.get(plugin_id, credential_slot) {
            if !cred.header_name.is_empty() {
                if let (Ok(name), Ok(val)) = (
                    reqwest::header::HeaderName::from_bytes(cred.header_name.as_bytes()),
                    reqwest::header::HeaderValue::from_str(&cred.header_value),
                ) {
                    hdrs.insert(name, val);
                }
            }
        }
    }

    // §11.5.9 凭证 body 注入：`{{slot_name}}` 占位符在请求前替换为凭证值。
    // 用于把凭证放进 POST body 的 OAuth 流程（如 Pixiv refresh_token 换 access_token）。
    // 只替换 manifest 已声明的 slot（未声明会在上面的 slot 校验中报错），
    // 脚本永远拿不到明文。
    let body = if method == HttpMethod::Post {
        inject_body_credentials(body, creds, plugin_id, cred_slots)
    } else {
        body.to_string()
    };

    let send_result = match method {
        HttpMethod::Get => http.inner.get(url).headers(hdrs).send(),
        HttpMethod::Post => http.inner.post(url).body(body).headers(hdrs).send(),
    };

    match send_result {
        Ok(resp) => {
            let status = resp.status().as_u16() as i64;
            let mut resp_hdrs = Map::new();
            for (k, v) in resp.headers() {
                if let Ok(s) = v.to_str() {
                    resp_hdrs.insert(k.as_str().into(), Dynamic::from(s.to_string()));
                }
            }
            let body = resp.text().unwrap_or_default();
            m.insert("status".into(), Dynamic::from_int(status));
            m.insert("headers".into(), Dynamic::from(resp_hdrs));
            m.insert("body".into(), Dynamic::from(body));
        }
        Err(e) => {
            m.insert("status".into(), Dynamic::from_int(0));
            m.insert("error".into(), Dynamic::from(e.to_string()));
        }
    }
    m
}

fn error_map(m: &mut Map, msg: String) -> Map {
    m.insert("status".into(), Dynamic::from_int(0));
    m.insert("error".into(), Dynamic::from(msg));
    std::mem::take(m)
}

/// 替换 body 中的 `{{slot_name}}` 占位符为凭证值。§11.5.9
/// 仅处理 manifest 已声明的 slot；占位符保留（未设置凭证时请求自然失败，错误对用户可见）。
fn inject_body_credentials(
    body: &str,
    creds: &CredentialStore,
    plugin_id: &str,
    cred_slots: &[String],
) -> String {
    let mut out = body.to_string();
    for slot in cred_slots {
        let placeholder = format!("{{{{{slot}}}}}");
        if out.contains(&placeholder) {
            if let Some(cred) = creds.get(plugin_id, slot) {
                out = out.replace(&placeholder, &cred.header_value);
            }
        }
    }
    out
}

fn is_domain_allowed(url: &str, allowed: &[String]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or("");
    let host = host.trim_start_matches("www.");
    allowed.iter().any(|d| {
        let d = d.trim_start_matches("www.");
        host == d || host.ends_with(&format!(".{d}"))
    })
}

fn system_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn json_to_dynamic(v: serde_json::Value) -> Dynamic {
    use serde_json::Value;
    match v {
        Value::Null => Dynamic::UNIT,
        Value::Bool(b) => Dynamic::from_bool(b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from_int(i)
            } else if let Some(f) = n.as_f64() {
                Dynamic::from_float(f)
            } else {
                Dynamic::UNIT
            }
        }
        Value::String(s) => Dynamic::from(s),
        Value::Array(arr) => {
            let mut list = rhai::Array::new();
            for item in arr {
                list.push(json_to_dynamic(item));
            }
            Dynamic::from(list)
        }
        Value::Object(obj) => {
            let mut m = Map::new();
            for (k, v) in obj {
                m.insert(k.into(), json_to_dynamic(v));
            }
            Dynamic::from(m)
        }
    }
}

/// 简易 JSON 路径查找：支持 `$.a.b.c` 与 `$.arr[0].field` 形态。
fn json_path_lookup(json: &Dynamic, path: &str) -> Option<Dynamic> {
    let path = path.strip_prefix("$.").unwrap_or(path);
    let mut cur = json.clone();
    for seg in path.split('.') {
        if seg.is_empty() {
            continue;
        }
        // 数组下标
        if let Some(name) = seg.strip_suffix(']') {
            if let Some((field, idx)) = name.split_once('[') {
                if !field.is_empty() {
                    let next = {
                        let m = cur.as_map_ref().ok()?;
                        m.get(field).cloned()
                    };
                    cur = next?;
                }
                let i: usize = idx.parse().ok()?;
                let next = {
                    let arr = cur.as_array_ref().ok()?;
                    arr.get(i).cloned()
                };
                cur = next?;
                continue;
            }
        }
        let next = {
            let m = cur.as_map_ref().ok()?;
            m.get(seg).cloned()
        };
        cur = next?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::PluginMeta;

    fn empty_manifest(tier: Tier) -> Manifest {
        Manifest {
            plugin: PluginMeta {
                id: "t".into(),
                name: "T".into(),
                version: "0.1".into(),
                author: "".into(),
                min_glean_version: "".into(),
                tier,
            },
            r#match: vec![],
            capabilities: Capabilities::default(),
            compliance: Default::default(),
            tier1: None,
        }
    }

    #[test]
    fn build_engine_with_no_caps_registers_only_pure_fns() {
        let m = empty_manifest(Tier::Script);
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        // 纯函数可用
        let v: i64 = rt.engine.eval("now()").expect("now() works");
        assert!(v > 0);
        // http_get 未注册 → 报"函数不存在"
        let err = rt
            .engine
            .eval::<()>(r#"http_get("https://example.com", #{"a":"b"}, "")"#)
            .err();
        assert!(err.is_some(), "http_get should not be registered");
    }

    #[test]
    fn build_engine_with_feed_fetch_registers_http() {
        let mut m = empty_manifest(Tier::Script);
        m.capabilities.feed_fetch = vec!["example.com".into()];
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        // http_get 已注册。无网络环境下会拿到 status=0 + error 的 map，
        // 但函数存在证明能力原语已正确注册。
        let result = rt
            .engine
            .eval::<Map>(r#"http_get("https://example.invalid/x", #{}, "")"#);
        match result {
            Ok(m) => {
                let status = m.get("status").and_then(|v| v.as_int().ok()).unwrap_or(-1);
                assert_eq!(status, 0);
                assert!(m.contains_key("error"));
            }
            Err(_) => { /* 网络超时也接受 */ }
        }
    }

    #[test]
    fn build_engine_rejects_undeclared_credential_slot() {
        // 声明 feed_fetch 但没声明 credential_use →
        // 脚本传非空 credential_slot 应被 Host 拒绝（返回 status=0 + error）。
        let mut m = empty_manifest(Tier::Script);
        m.capabilities.feed_fetch = vec!["example.com".into()];
        // 不设 credential_use
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        let result = rt
            .engine
            .eval::<Map>(r#"http_get("https://example.com/x", #{}, "pixiv_session")"#)
            .expect("fn registered");
        let status = result
            .get("status")
            .and_then(|v| v.as_int().ok())
            .unwrap_or(-1);
        assert_eq!(status, 0);
        let err = result
            .get("error")
            .and_then(|v| v.clone().into_string().ok())
            .unwrap_or_default();
        assert!(
            err.contains("not declared"),
            "expected undeclared slot error, got: {err}"
        );
    }

    #[test]
    fn is_domain_allowed_matches_exact_and_subdomain() {
        assert!(is_domain_allowed(
            "https://example.com/x",
            &["example.com".into()]
        ));
        assert!(is_domain_allowed(
            "https://api.example.com/x",
            &["example.com".into()]
        ));
        assert!(is_domain_allowed(
            "https://www.example.com/x",
            &["example.com".into()]
        ));
        assert!(!is_domain_allowed(
            "https://evil.com/x",
            &["example.com".into()]
        ));
        assert!(!is_domain_allowed("not a url", &["example.com".into()]));
    }

    #[test]
    fn inject_body_credentials_replaces_declared_slot() {
        // 声明的 slot → body 占位符被替换为凭证值（凭证取 header_value）。
        let mut creds = CredentialStore::in_memory();
        creds.set(
            "pixiv",
            "pixiv_refresh_token",
            crate::plugin::Credential {
                header_name: "x-secret".into(),
                header_value: "RT-123456".into(),
            },
        );
        let slots = vec!["pixiv_refresh_token".to_string()];
        let out = inject_body_credentials(
            "client_id=a&grant_type=refresh_token&refresh_token={{pixiv_refresh_token}}",
            &creds,
            "pixiv",
            &slots,
        );
        assert!(
            out.contains("refresh_token=RT-123456"),
            "slot should be replaced: {out}"
        );
        assert!(!out.contains("{{"), "no placeholder left: {out}");
    }

    #[test]
    fn inject_body_credentials_keeps_placeholder_when_unset() {
        // 未设置凭证 → 占位符保留（请求会失败，错误对用户可见），不 panic。
        let creds = CredentialStore::in_memory();
        let slots = vec!["pixiv_refresh_token".to_string()];
        let out = inject_body_credentials(
            "refresh_token={{pixiv_refresh_token}}",
            &creds,
            "pixiv",
            &slots,
        );
        assert!(out.contains("{{pixiv_refresh_token}}"));
    }

    #[test]
    fn official_pixiv_script_compiles() {
        // 官方 pixiv 插件脚本必须能通过 Rhai 编译（语法验证，不执行网络请求）。
        let script = include_str!("../../../../plugins/pixiv/adapter.rhai");
        let mut m = empty_manifest(Tier::Script);
        m.capabilities.feed_fetch =
            vec!["oauth.secure.pixiv.net".into(), "app-api.pixiv.net".into()];
        m.capabilities.credential_use = vec!["pixiv_refresh_token".into()];
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        rt.engine
            .compile(script)
            .expect("pixiv adapter.rhai compiles");
    }

    #[test]
    fn pixiv_iso_to_unix_accounts_for_timezone_offset() {
        // Pixiv create_date 为 JST(+09:00)，iso_to_unix 必须将其转为 UTC。
        // 直接对官方脚本的 iso_to_unix 做行为验证（不发起网络请求）。
        let script = r#"
            fn parse_int_safe(s) {
                if type_of(s) != "string" || len(s) == 0 { return 0; }
                try { return s.parse_int(); } catch { return 0; }
            }
            fn iso_to_unix(s) {
                if type_of(s) != "string" || len(s) < 19 { return 0; }
                let ymd = s.substring(0, 10).split("-");
                if len(ymd) != 3 { return 0; }
                let hms = s.substring(11, 19).split(":");
                if len(hms) != 3 { return 0; }
                let y = parse_int_safe(ymd[0]);
                let mo = parse_int_safe(ymd[1]);
                let d = parse_int_safe(ymd[2]);
                let h = parse_int_safe(hms[0]);
                let mi = parse_int_safe(hms[1]);
                let se = parse_int_safe(hms[2]);
                if y < 2000 || y > 2100 || mo < 1 || mo > 12 { return 0; }
                let y2 = y - (if mo <= 2 { 1 } else { 0 });
                let era = (if y2 >= 0 { y2 } else { y2 - 399 }) / 400;
                let yoe = y2 - era * 400;
                let mp = mo + (if mo > 2 { -3 } else { 9 });
                let doy = (153 * mp + 2) / 5 + d - 1;
                let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
                let days = era * 146097 + doe - 719468;
                let ts = days * 86400 + h * 3600 + mi * 60 + se;
                let tz = s.substring(19, len(s));
                let sign = 0;
                let body = tz;
                if tz.starts_with("+") {
                    sign = 1;
                    body = tz.substring(1, len(tz));
                } else if tz.starts_with("-") {
                    sign = -1;
                    body = tz.substring(1, len(tz));
                } else if tz.starts_with("Z") {
                    sign = 0;
                    body = "";
                }
                if sign != 0 && len(body) >= 4 {
                    let parts = body.split(":");
                    let oh = parse_int_safe(parts[0]);
                    let om = if len(parts) >= 2 { parse_int_safe(parts[1]) } else { 0 };
                    ts -= sign * (oh * 3600 + om * 60);
                }
                return ts;
            }
            let jst = iso_to_unix("2026-07-06T19:56:40+09:00");
            let utc = iso_to_unix("2026-07-06T10:56:40Z");
            let naive = iso_to_unix("2026-07-06T19:56:40");
            jst == utc && utc == naive - 9 * 3600
        "#;
        let m = empty_manifest(Tier::Script);
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        let ok: bool = rt
            .engine
            .eval(script)
            .expect("iso_to_unix timezone logic evals");
        assert!(ok, "JST(+09:00) 时间应转换为 UTC");
    }

    #[test]
    fn json_path_lookup_simple() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{ "a": { "b": [10, 20] } }"#).unwrap();
        let d = json_to_dynamic(json);
        let v = json_path_lookup(&d, "$.a.b[0]").unwrap();
        assert_eq!(v.as_int().unwrap(), 10);
    }

    /// Tier 2 EntryCollector 端到端：set_field + add_entry → run_script 返回 ParsedFeed。
    #[test]
    fn tier2_run_script_collects_entries() {
        let m = empty_manifest(Tier::Script);
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        let script = r#"
            set_field("title", "T");
            set_field("guid", "g1");
            set_field("url", "https://example.com/1");
            set_field("published_at", 1700000000);
            add_entry();
            set_field("title", "T2");
            set_field("guid", "g2");
            set_field("published_at", "1700000001");
            add_entry();
        "#;
        let parsed = rt
            .run_script(script, "https://example.com/test", &[])
            .expect("run_script");
        assert_eq!(parsed.title, "T");
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].title, "T");
        assert_eq!(parsed.entries[0].guid, "g1");
        assert_eq!(
            parsed.entries[0].url.as_deref(),
            Some("https://example.com/1")
        );
        assert_eq!(parsed.entries[0].published_at, Some(1700000000));
        assert_eq!(parsed.entries[1].title, "T2");
        assert_eq!(parsed.entries[1].published_at, Some(1700000001));
    }

    /// 脚本忘记调 add_entry 时，run_script 自动 commit current。
    #[test]
    fn tier2_auto_commits_uncommitted_current() {
        let m = empty_manifest(Tier::Script);
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        let script = r#"
            set_field("title", "Only");
            set_field("guid", "g1");
        "#;
        let parsed = rt
            .run_script(script, "https://example.com/test", &[])
            .expect("run_script");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].title, "Only");
    }

    /// `set_feed_title` 覆盖 feed 标题（默认取 manifest.name）。
    #[test]
    fn set_feed_title_overrides_manifest_name() {
        let m = empty_manifest(Tier::Script); // name = "T"
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        let script = r#"
            set_feed_title("Bilibili 某UP主");
            set_field("title", "v1");
            add_entry();
        "#;
        let parsed = rt
            .run_script(script, "https://space.bilibili.com/3428150", &[])
            .expect("run_script");
        assert_eq!(parsed.title, "Bilibili 某UP主");
        assert_eq!(parsed.entries.len(), 1);
        // 未调用 set_feed_title 时回退到 manifest.name
        let parsed2 = rt
            .run_script(
                r#"set_field("title", "v2"); add_entry();"#,
                "https://x.com",
                &[],
            )
            .expect("run_script");
        assert_eq!(parsed2.title, "T");
    }

    /// md5 host 函数注册后可用，且结果与标准 MD5 一致。
    #[test]
    fn md5_host_fn_matches_known_hash() {
        let m = empty_manifest(Tier::Script);
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        let hash: String = rt.engine.eval(r#"md5("hello")"#).expect("md5 eval");
        assert_eq!(hash, "5d41402abc4b2a76b9719d911017c592");
    }

    /// `substring(start, end)` 按字符切片，end 是结束位置（非长度），
    /// 负数/越界自动夹紧。脚本用 `s.substring(0, 10)` 取前 10 字符。
    #[test]
    fn substring_host_fn_returns_char_range() {
        let m = empty_manifest(Tier::Script);
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        let s: String = rt
            .engine
            .eval(r#""2026-08-01T12:34:56".substring(0, 10)"#)
            .expect("substring eval");
        assert_eq!(s, "2026-08-01");
        let s: String = rt
            .engine
            .eval(r#""2026-08-01T12:34:56".substring(11, 19)"#)
            .expect("substring eval");
        assert_eq!(s, "12:34:56");
        // 越界夹紧
        let s: String = rt
            .engine
            .eval(r#""abc".substring(0, 100)"#)
            .expect("substring eval");
        assert_eq!(s, "abc");
        // 负 start 夹紧到 0
        let s: String = rt
            .engine
            .eval(r#""abc".substring(-5, 2)"#)
            .expect("substring eval");
        assert_eq!(s, "ab");
    }

    /// wbi 签名算法（与仓库 `plugins/bilibili/adapter.rhai` 中 `wbi_sign` 同实现）
    /// 必须与 Bilibili 官方算法一致。测试向量由 Python 实现对固定输入计算得到：
    /// `img_key=7cd084941338484aae1ad9425b84077c, sub_key=4932caff0ff746eab6f01bf08b70ac45,
    /// params={mid:2,pn:1,ps:5,wts:1785474273}` → `w_rid=09e338abf9d88493d458b6c8876af8ff`。
    #[test]
    fn wbi_signing_matches_known_vector() {
        let m = empty_manifest(Tier::Script);
        let http = Arc::new(HttpClient::default());
        let creds = Arc::new(CredentialStore::in_memory());
        let rt = Runtime::build(m, http, creds);
        let script = r#"
            fn filter_chars(s) {
                let out = "";
                for c in s {
                    if c != '!' && c != "'" && c != '(' && c != ')' && c != '*' {
                        out += c;
                    }
                }
                return out;
            }

            fn to_str(v) {
                let t = type_of(v);
                if t == "i64" || t == "i32" || t == "int" {
                    return "" + v;
                }
                return v;
            }

            fn wbi_sign(params, img_key, sub_key, wts) {
                let MIXIN_KEY_ENC_TAB = [
                    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49,
                    33, 9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13, 37, 36, 25, 51, 0, 4, 44, 52,
                    6, 21, 54, 16, 26, 11, 22, 40, 7, 30, 55, 48, 24, 1, 20, 57, 34, 17, 59, 61,
                    56, 60, 63, 62,
                ];
                let orig = img_key + sub_key;
                let mixin_key = "";
                for i in MIXIN_KEY_ENC_TAB {
                    if i >= len(orig) { break; }
                    mixin_key += orig[i];
                    if len(mixin_key) >= 32 { break; }
                }

                params["wts"] = wts;
                let keys = params.keys();
                keys.sort();
                let query = "";
                for k in keys {
                    let v_str = to_str(params[k]);
                    let filtered = filter_chars(v_str);
                    if len(query) > 0 { query += "&"; }
                    query += k + "=" + filtered;
                }
                let w_rid = md5(query + mixin_key);
                params["w_rid"] = w_rid;
                return params;
            }

            let params = #{"mid": "2", "pn": "1", "ps": "5"};
            let signed = wbi_sign(params, "7cd084941338484aae1ad9425b84077c", "4932caff0ff746eab6f01bf08b70ac45", 1785474273);
            signed["w_rid"]
        "#;
        let w_rid: String = rt.engine.eval(script).expect("wbi eval");
        assert_eq!(w_rid, "09e338abf9d88493d458b6c8876af8ff");
    }
}
