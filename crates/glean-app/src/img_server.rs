//! Local loopback HTTP server for cached reader images.
//!
//! WebView2 cannot reliably load large Pixiv originals as base64 `data:` URLs,
//! and cannot fetch `i.pximg.net` directly (403 hotlink protection). The app
//! therefore downloads images in Rust (with Referer) and serves the files from
//! `127.0.0.1` so the reader can display full-resolution images in-process.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Background loopback server that only serves files from the image cache dir.
pub struct LocalImageServer {
    pub base_url: String,
}

impl LocalImageServer {
    /// Bind `127.0.0.1:0`, spawn an accept loop, and return the base URL.
    pub fn start(cache_dir: PathBuf) -> Option<Self> {
        let _ = std::fs::create_dir_all(&cache_dir);
        let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
        listener.set_nonblocking(false).ok()?;
        let port = listener.local_addr().ok()?.port();
        let base_url = format!("http://127.0.0.1:{port}");
        let dir = Arc::new(cache_dir);
        thread::Builder::new()
            .name("glean-img-server".into())
            .spawn(move || accept_loop(listener, dir))
            .ok()?;
        Some(Self { base_url })
    }
}

fn accept_loop(listener: TcpListener, dir: Arc<PathBuf>) {
    for stream in listener.incoming().flatten() {
        let dir = Arc::clone(&dir);
        let _ = thread::Builder::new()
            .name("glean-img-req".into())
            .spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
                handle_request(stream, &dir);
            });
    }
}

fn handle_request(mut stream: TcpStream, dir: &Path) {
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let Some(line) = req.lines().next() else {
        return;
    };
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    if method != "GET" && method != "HEAD" {
        let _ = write_simple(&mut stream, 405, "text/plain", b"method not allowed");
        return;
    }
    let Some(filename) = safe_filename(path) else {
        let _ = write_simple(&mut stream, 400, "text/plain", b"bad request");
        return;
    };
    let file_path = dir.join(filename);
    let Ok(bytes) = std::fs::read(&file_path) else {
        let _ = write_simple(&mut stream, 404, "text/plain", b"not found");
        return;
    };
    let mime = glean_core::ImageCache::mime_for(filename);
    if let Some((start, end)) = parse_range(&req, bytes.len()) {
        let body = &bytes[start..=end];
        let range = format!("bytes {start}-{end}/{}", bytes.len());
        if method == "HEAD" {
            let _ =
                write_headers_with_range(&mut stream, 206, mime, body.len(), true, Some(&range));
        } else {
            let _ = write_body_with_range(&mut stream, 206, mime, body, Some(&range));
        }
        return;
    }
    if method == "HEAD" {
        let _ = write_headers(&mut stream, 200, mime, bytes.len(), true);
        return;
    }
    let _ = write_body(&mut stream, 200, mime, &bytes);
}

fn safe_filename(path: &str) -> Option<&str> {
    let path = path.split('?').next().unwrap_or(path);
    let path = path.split('#').next().unwrap_or(path);
    let name = path.trim_start_matches('/');
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.contains('\0')
    {
        return None;
    }
    // Cached names look like `<16 hex>.<ext>`.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return None;
    }
    Some(name)
}

fn parse_range(request: &str, len: usize) -> Option<(usize, usize)> {
    let value = request
        .lines()
        .find_map(|line| line.strip_prefix("Range: "))?;
    let range = value.strip_prefix("bytes=")?.split(',').next()?;
    let mut parts = range.split('-');
    let start = parts.next()?.parse::<usize>().ok()?;
    let end = match parts.next()?.parse::<usize>() {
        Ok(end) => end.min(len.checked_sub(1)?),
        Err(_) => len.checked_sub(1)?,
    };
    (start <= end && start < len).then_some((start, end))
}

fn write_simple(
    stream: &mut TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_body(stream, status, mime, body)
}

fn write_body(stream: &mut TcpStream, status: u16, mime: &str, body: &[u8]) -> std::io::Result<()> {
    write_body_with_range(stream, status, mime, body, None)
}

fn write_body_with_range(
    stream: &mut TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
    range: Option<&str>,
) -> std::io::Result<()> {
    write_headers_with_range(stream, status, mime, body.len(), false, range)?;
    stream.write_all(body)?;
    Ok(())
}

fn write_headers(
    stream: &mut TcpStream,
    status: u16,
    mime: &str,
    len: usize,
    head_only: bool,
) -> std::io::Result<()> {
    write_headers_with_range(stream, status, mime, len, head_only, None)
}

fn write_headers_with_range(
    stream: &mut TcpStream,
    status: u16,
    mime: &str,
    len: usize,
    head_only: bool,
    range: Option<&str>,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    // Keep responses private and cacheable within the local session.
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {mime}\r\n\
         Content-Length: {len}\r\n\
         {content_range}\
         Accept-Ranges: bytes\r\n\
         Cache-Control: private, max-age=86400\r\n\
         Connection: close\r\n\
         \r\n"
        content_range = range
            .map(|value| format!("Content-Range: {value}\r\n"))
            .unwrap_or_default(),
    );
    stream.write_all(headers.as_bytes())?;
    let _ = head_only;
    Ok(())
}
