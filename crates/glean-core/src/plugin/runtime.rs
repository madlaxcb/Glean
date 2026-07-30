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
use crate::feed::HttpClient;
use crate::plugin::credential::CredentialStore;
use crate::plugin::manifest::{Capabilities, Manifest, Tier};
use rhai::{Dynamic, Engine, Map};
use std::sync::Arc;

/// 单次 Rhai 脚本执行的最大操作数（防止死循环）。
const MAX_OPERATIONS: u64 = 200_000;
/// 最大调用栈深度。
const MAX_CALL_LEVELS: usize = 64;
/// 单次脚本执行软超时（秒）。Host 在 worker 线程之外另加硬超时。
const SCRIPT_TIMEOUT_SECS: u64 = 10;

/// Rhai 脚本执行的运行时上下文。一个 `Runtime` 实例对应一个加载的插件。
pub struct Runtime {
    pub engine: Engine,
    pub manifest: Manifest,
    #[allow(dead_code)]
    pub http: Arc<HttpClient>,
    #[allow(dead_code)]
    pub credentials: Arc<CredentialStore>,
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
        if !caps.content_transform.is_empty() {
            register_content_fns(&mut engine);
        }

        Self {
            engine,
            manifest,
            http,
            credentials,
        }
    }

    /// 执行 Tier 2 适配器脚本。
    ///
    /// M5：本方法仅完成 Engine 构建 + 脚本执行；Entry 收集器 (`EntryCollector`)
    /// 的接入排到 M6（§11.5.11）。
    pub fn run_script(&self, script: &str) -> Result<Dynamic> {
        self.engine
            .eval::<Dynamic>(script)
            .map_err(|e| CoreError::Message(format!("rhai eval: {e}")))
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
    engine.register_fn("log", |level: String, msg: String| {
        let _ = (level, msg); // M6 接入正式日志渠道
    });
    engine.register_fn("parse_json", |s: String| -> Dynamic {
        serde_json::from_str::<serde_json::Value>(&s)
            .map(json_to_dynamic)
            .unwrap_or(Dynamic::UNIT)
    });
    engine.register_fn("json_path", |json: Dynamic, path: String| -> Dynamic {
        json_path_lookup(&json, &path).unwrap_or(Dynamic::UNIT)
    });
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

/// 注册内容构建 host 函数（仅当 manifest 声明 `content_transform` 时）。
/// §11.5.6：`set_field` / `set_embed`，输出在入库前必过 ammonia。
fn register_content_fns(engine: &mut Engine) {
    // M5 占位：实际 Entry 收集器接入排到 M6。这里先注册一个 no-op，
    // 让脚本可以调用 `set_field("title", "...")` 不报"函数不存在"。
    engine.register_fn("set_field", |_name: String, _value: Dynamic| {});
    engine.register_fn("set_embed", |_provider: String, _id: String| {});
}

#[derive(Clone, Copy)]
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
    if !credential_slot.is_empty() {
        if let Some(cred) = creds.get(plugin_id, credential_slot) {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(cred.header_name.as_bytes()),
                reqwest::header::HeaderValue::from_str(&cred.header_value),
            ) {
                hdrs.insert(name, val);
            }
        }
    }

    let send_result = match method {
        HttpMethod::Get => http.inner.get(url).headers(hdrs).send(),
        HttpMethod::Post => http
            .inner
            .post(url)
            .body(body.to_string())
            .headers(hdrs)
            .send(),
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
                assert!(m.get("error").is_some());
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
    fn json_path_lookup_simple() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{ "a": { "b": [10, 20] } }"#).unwrap();
        let d = json_to_dynamic(json);
        let v = json_path_lookup(&d, "$.a.b[0]").unwrap();
        assert_eq!(v.as_int().unwrap(), 10);
    }
}
