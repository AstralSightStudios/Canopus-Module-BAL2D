//! HTTP GET over the host's `wasi:http` (p2 `outgoing-handler`, through the `wasip2` bindings).
//!
//! The calls block (a p2 poll inside our async export, which the host allows), so the page is
//! redrawn from the progress callback. Every wait is bounded by our own timer as well as the
//! request options, in case the host ignores the latter: the connection and the first byte
//! within [`FIRST_BYTE`], then no gap longer than [`BETWEEN_BYTES`] in the body. Redirects are
//! followed and only 2xx counts as success. Bodies are returned as received (a gzip body when
//! `gzip` was asked for and the server used it).
//!
//! Outside wasm (host tests) there is no network: [`get`] always fails.

use std::fmt;

/// Why a download failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub enum Error {
    Status(u16),
    Timeout,
    Failed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Status(404) => f.write_str("文件不存在（HTTP 404）"),
            Error::Status(s) => write!(f, "HTTP {s}"),
            Error::Timeout => f.write_str("连接超时"),
            Error::Failed(e) => f.write_str(e),
        }
    }
}

/// Bodies larger than this are refused (the largest file, a dialogue table, is about 15 MB).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub const MAX_BODY: usize = 48 << 20;

/// Downloads `url`. `progress` gets the bytes received so far and the length the server
/// announced (if any). With `gzip`, `Accept-Encoding: gzip` is sent and the body may come back
/// compressed (see `online::decoded`).
#[cfg(target_arch = "wasm32")]
pub fn get(url: &str, gzip: bool, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Vec<u8>, Error> {
    imp::get(url, gzip, progress)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn get(_url: &str, _gzip: bool, _progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Vec<u8>, Error> {
    Err(Error::Failed("没有网络（只在 AstroBox 中可用）".into()))
}

/// Splits an absolute http(s) URL into (https?, authority, path with query).
pub fn split_url(url: &str) -> Option<(bool, &str, String)> {
    let url = url.trim();
    let (https, rest) = if url.len() > 8 && url[..8].eq_ignore_ascii_case("https://") {
        (true, &url[8..])
    } else if url.len() > 7 && url[..7].eq_ignore_ascii_case("http://") {
        (false, &url[7..])
    } else {
        return None;
    };
    let rest = rest.split('#').next().unwrap_or(rest);
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.is_empty() || authority.contains(['@', ' ']) {
        return None;
    }
    let path = if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('?') {
        format!("/{path}")
    } else {
        path.to_string()
    };
    Some((https, authority, path))
}

/// Where a redirect's `Location` points, relative to the URL that answered.
pub fn resolve(base: &str, location: &str) -> String {
    let location = location.trim();
    if split_url(location).is_some() {
        return location.to_string();
    }
    let Some((https, authority, path)) = split_url(base) else { return location.to_string() };
    let scheme = if https { "https" } else { "http" };
    if let Some(rest) = location.strip_prefix("//") {
        format!("{scheme}://{rest}")
    } else if location.starts_with('/') {
        format!("{scheme}://{authority}{location}")
    } else {
        let path = path.split('?').next().unwrap_or("/");
        let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)];
        format!("{scheme}://{authority}{dir}{location}")
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::time::Duration;

    use wasip2::clocks::monotonic_clock;
    use wasip2::http::outgoing_handler;
    use wasip2::http::types::{ErrorCode, Fields, IncomingResponse, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
    use wasip2::io::poll::{self, Pollable};
    use wasip2::io::streams::StreamError;

    use super::{Error, MAX_BODY, resolve, split_url};

    const CONNECT: Duration = Duration::from_secs(12);
    /// From sending the request to the response head (connecting included).
    const FIRST_BYTE: Duration = Duration::from_secs(25);
    const BETWEEN_BYTES: Duration = Duration::from_secs(30);
    const MAX_REDIRECTS: usize = 6;
    const CHUNK: u64 = 64 * 1024;
    const USER_AGENT: &str = concat!("BAL2D-AstroBox-Plugin/", env!("CARGO_PKG_VERSION"));

    fn ns(d: Duration) -> u64 {
        d.as_nanos() as u64
    }

    /// Waits until `p` is ready or `timeout` has passed; `false` on timeout.
    fn wait(p: &Pollable, timeout: Duration) -> bool {
        let timer = monotonic_clock::subscribe_duration(ns(timeout));
        let ready = poll::poll(&[p, &timer]);
        ready.contains(&0)
    }

    fn describe(e: &ErrorCode) -> Error {
        match e {
            ErrorCode::DnsTimeout | ErrorCode::ConnectionTimeout | ErrorCode::ConnectionReadTimeout | ErrorCode::HttpResponseTimeout => Error::Timeout,
            ErrorCode::DnsError(_) | ErrorCode::DestinationNotFound => Error::Failed("无法解析域名".into()),
            ErrorCode::ConnectionRefused | ErrorCode::DestinationUnavailable => Error::Failed("连接被拒绝".into()),
            ErrorCode::ConnectionTerminated => Error::Failed("连接被中断".into()),
            ErrorCode::TlsProtocolError | ErrorCode::TlsCertificateError | ErrorCode::TlsAlertReceived(_) => Error::Failed("TLS 握手失败".into()),
            ErrorCode::InternalError(Some(s)) => Error::Failed(format!("网络错误（{s}）")),
            other => Error::Failed(format!("网络错误（{other:?}）")),
        }
    }

    fn header(response: &IncomingResponse, name: &str) -> Option<String> {
        // The headers are a child of the response: dropped here, before it.
        let headers = response.headers();
        let values = headers.get(name);
        values.first().map(|v| String::from_utf8_lossy(v).into_owned())
    }

    /// Sends one GET and waits for the response head.
    fn send(url: &str, gzip: bool) -> Result<IncomingResponse, Error> {
        let (https, authority, path) = split_url(url).ok_or_else(|| Error::Failed(format!("无效的网址：{url}")))?;
        let headers = Fields::new();
        let _ = headers.append("user-agent", USER_AGENT.as_bytes());
        let _ = headers.append("accept", b"*/*");
        if gzip {
            let _ = headers.append("accept-encoding", b"gzip");
        }
        let request = OutgoingRequest::new(headers);
        let bad = |what: &str| Error::Failed(format!("无法设置请求的{what}"));
        request.set_method(&Method::Get).map_err(|()| bad("方法"))?;
        request.set_scheme(Some(&if https { Scheme::Https } else { Scheme::Http })).map_err(|()| bad("协议"))?;
        request.set_authority(Some(authority)).map_err(|()| bad("主机"))?;
        request.set_path_with_query(Some(&path)).map_err(|()| bad("路径"))?;
        let body = request.body().map_err(|()| bad("正文"))?;
        let options = RequestOptions::new();
        // Hosts may refuse some of these; our own timers below still apply.
        let _ = options.set_connect_timeout(Some(ns(CONNECT)));
        let _ = options.set_first_byte_timeout(Some(ns(FIRST_BYTE)));
        let _ = options.set_between_bytes_timeout(Some(ns(BETWEEN_BYTES)));
        let future = outgoing_handler::handle(request, Some(options)).map_err(|e| describe(&e))?;
        OutgoingBody::finish(body, None).map_err(|e| describe(&e))?;
        let started = std::time::Instant::now();
        loop {
            if let Some(result) = future.get() {
                return match result {
                    Ok(Ok(response)) => Ok(response),
                    Ok(Err(e)) => Err(describe(&e)),
                    Err(()) => Err(Error::Failed("响应已被读取".into())),
                };
            }
            let left = FIRST_BYTE.saturating_sub(started.elapsed());
            // The pollable is a child of the future: it is dropped at the end of this statement.
            if left.is_zero() || !wait(&future.subscribe(), left) {
                return Err(Error::Timeout);
            }
        }
    }

    /// Reads the whole body, reporting progress after every chunk.
    fn read_body(response: &IncomingResponse, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Vec<u8>, Error> {
        let total = header(response, "content-length").and_then(|v| v.trim().parse::<u64>().ok());
        if total.is_some_and(|t| t > MAX_BODY as u64) {
            return Err(Error::Failed("文件过大".into()));
        }
        let body = response.consume().map_err(|()| Error::Failed("无法读取响应正文".into()))?;
        // The stream is a child of the body: declared after it, so dropped before it.
        let stream = body.stream().map_err(|()| Error::Failed("无法读取响应正文".into()))?;
        let mut out = Vec::with_capacity(total.unwrap_or(0) as usize);
        progress(0, total);
        loop {
            match stream.read(CHUNK) {
                Ok(bytes) if bytes.is_empty() => {
                    if !wait(&stream.subscribe(), BETWEEN_BYTES) {
                        return Err(Error::Timeout);
                    }
                }
                Ok(bytes) => {
                    out.extend_from_slice(&bytes);
                    if out.len() > MAX_BODY {
                        return Err(Error::Failed("文件过大".into()));
                    }
                    progress(out.len() as u64, total);
                }
                Err(StreamError::Closed) => break,
                Err(StreamError::LastOperationFailed(e)) => return Err(Error::Failed(format!("读取中断（{}）", e.to_debug_string()))),
            }
        }
        if total.is_some_and(|t| (out.len() as u64) < t) {
            return Err(Error::Failed(format!("下载不完整（{} / {} 字节）", out.len(), total.unwrap_or(0))));
        }
        Ok(out)
    }

    pub fn get(url: &str, gzip: bool, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Vec<u8>, Error> {
        let mut url = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            let response = send(&url, gzip)?;
            let status = response.status();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let Some(location) = header(&response, "location") else { return Err(Error::Status(status)) };
                tracing::debug!("redirect {status}: {url} -> {location}");
                url = resolve(&url, &location);
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(Error::Status(status));
            }
            return read_body(&response, progress);
        }
        Err(Error::Failed("重定向次数过多".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_and_redirects_resolve() {
        let (https, host, path) = split_url("https://cdn.jsdelivr.net/gh/a/b@main/x%20y.png?v=1#frag").unwrap();
        assert!(https);
        assert_eq!(host, "cdn.jsdelivr.net");
        assert_eq!(path, "/gh/a/b@main/x%20y.png?v=1");
        assert_eq!(split_url("HTTP://example.com").unwrap(), (false, "example.com", "/".to_string()));
        assert_eq!(split_url("http://example.com?q").unwrap().2, "/?q");
        assert!(split_url("ftp://example.com/").is_none());
        assert!(split_url("https://").is_none());
        let base = "https://ghfast.top/https://raw.githubusercontent.com/o/r/main/a/b.png";
        assert_eq!(resolve(base, "https://x.org/y"), "https://x.org/y");
        assert_eq!(resolve(base, "//x.org/y"), "https://x.org/y");
        assert_eq!(resolve(base, "/y"), "https://ghfast.top/y");
        assert_eq!(resolve("https://h.org/a/b.png?x=1", "c.png"), "https://h.org/a/c.png");
    }
}
