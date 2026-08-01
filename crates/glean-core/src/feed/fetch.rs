use crate::error::{CoreError, Result};
use reqwest::blocking::Client;
use reqwest::header::{
    HeaderMap, HeaderValue, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, USER_AGENT,
};
use std::time::Duration;

pub struct HttpClient {
    pub inner: Client,
}

impl HttpClient {
    pub fn new() -> Result<Self> {
        Self::with_proxy(None)
    }

    /// Create an HTTP client with an optional proxy URL.
    /// Proxy format: "http://host:port" or "socks5://host:port".
    /// 无 scheme 时自动补 `http://`（如 `127.0.0.1:7890`），
    /// 避免 `Proxy::all` 因缺 scheme 报错导致代理静默失效。
    pub fn with_proxy(proxy_url: Option<&str>) -> Result<Self> {
        let mut builder = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5));
        if let Some(proxy) = proxy_url {
            let p = proxy.trim();
            if !p.is_empty() {
                let normalized = if p.contains("://") {
                    p.to_string()
                } else {
                    format!("http://{p}")
                };
                let parsed = reqwest::Proxy::all(normalized)
                    .map_err(|e| CoreError::Http(format!("invalid proxy: {e}")))?;
                builder = builder.proxy(parsed);
            }
        }
        let inner = builder
            .build()
            .map_err(|e| CoreError::Http(e.to_string()))?;
        Ok(Self { inner })
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new().expect("http client")
    }
}

#[derive(Debug)]
pub enum FetchResult {
    NotModified,
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
        final_url: String,
    },
}

pub fn fetch_feed_bytes(
    client: &HttpClient,
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<FetchResult> {
    let mut req = client.inner.get(url);
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("Glean/0.0.1 (+https://github.com/madlaxcb/Glean; RSS reader)"),
    );
    if let Some(e) = etag {
        if let Ok(v) = HeaderValue::from_str(e) {
            headers.insert(IF_NONE_MATCH, v);
        }
    }
    if let Some(lm) = last_modified {
        if let Ok(v) = HeaderValue::from_str(lm) {
            headers.insert(IF_MODIFIED_SINCE, v);
        }
    }
    req = req.headers(headers);

    let resp = req.send().map_err(|e| CoreError::Http(e.to_string()))?;
    let status = resp.status();
    if status.as_u16() == 304 {
        return Ok(FetchResult::NotModified);
    }
    if !status.is_success() {
        return Err(CoreError::Http(format!("HTTP {status} for {url}")));
    }

    let etag = resp
        .headers()
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let last_modified = resp
        .headers()
        .get(LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let final_url = resp.url().to_string();
    let bytes = resp
        .bytes()
        .map_err(|e| CoreError::Http(e.to_string()))?
        .to_vec();
    if bytes.is_empty() {
        return Err(CoreError::Http("empty body".into()));
    }
    Ok(FetchResult::Body {
        bytes,
        etag,
        last_modified,
        final_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_proxy_accepts_scheme_less_url() {
        // 无 scheme 的地址自动补 http://，否则 Proxy::all 会报错
        // 导致代理静默失效（§11.5.10 常见问题）。
        HttpClient::with_proxy(Some("127.0.0.1:7890")).expect("scheme auto-added");
        HttpClient::with_proxy(Some(" 127.0.0.1:7890 ")).expect("trimmed");
        HttpClient::with_proxy(Some("http://127.0.0.1:7890")).expect("explicit scheme");
        HttpClient::with_proxy(Some("socks5://127.0.0.1:1080")).expect("socks5");
        // 明显非法的地址仍报错。
        assert!(HttpClient::with_proxy(Some("::not a proxy::")).is_err());
        assert!(HttpClient::with_proxy(Some("")).is_ok());
        assert!(HttpClient::with_proxy(None).is_ok());
    }
}
