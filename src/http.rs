//! Minimal HTTP/1.1 client for ASR submissions and small JSON APIs.
//!
//! Deliberately small: one request per connection, system trust via
//! `native-tls`, explicit byte limits, no redirects. It never proxies through
//! environment variables, matching Python's `trust_env=False`.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use url::{Position, Url};

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub connect: Duration,
    pub total: Duration,
    pub max_body: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            total: Duration::from_secs(60),
            max_body: 1024 * 1024,
        }
    }
}

/// POST a body and read a bounded response. Redirects are returned as-is.
pub async fn post(
    url: &Url,
    headers: &[(&str, &str)],
    content_type: &str,
    body: &[u8],
    limits: Limits,
) -> Result<Response> {
    validate(url)?;
    let host = url.host_str().context("HTTP URL 缺少主机")?.to_string();
    let host_header = url[Position::BeforeHost..Position::AfterPort].to_string();
    let mut target = url.path().to_string();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }

    let mut head = format!(
        "POST {target} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: Whisper-Input-Next/Rust\r\nAccept: application/json\r\nConnection: close\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");

    let request = dispatch(url, &host, head.as_bytes(), body, limits)
        .await
        .with_context(|| format!("请求 {host} 失败"))?;
    Ok(request)
}

/// GET a bounded response. Used for model catalog refreshes.
pub async fn get(url: &Url, headers: &[(&str, &str)], limits: Limits) -> Result<Response> {
    validate(url)?;
    let host = url.host_str().context("HTTP URL 缺少主机")?.to_string();
    let host_header = url[Position::BeforeHost..Position::AfterPort].to_string();
    let mut target = url.path().to_string();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    let mut head = format!(
        "GET {target} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: Whisper-Input-Next/Rust\r\nAccept: application/json\r\nConnection: close\r\n"
    );
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    dispatch(url, &host, head.as_bytes(), &[], limits).await
}

fn validate(url: &Url) -> Result<()> {
    if url.host().is_none() {
        bail!("HTTP URL 缺少主机");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("HTTP URL 不能包含账号信息");
    }
    match url.scheme() {
        "https" | "http" => Ok(()),
        other => Err(anyhow!("不支持的 URL 协议: {other}")),
    }
}

async fn dispatch(
    url: &Url,
    host: &str,
    head: &[u8],
    body: &[u8],
    limits: Limits,
) -> Result<Response> {
    let exchange = async {
        let mut stream = connect(url, host, limits.connect).await?;
        stream.write_all(head).await?;
        if !body.is_empty() {
            stream.write_all(body).await?;
        }
        stream.flush().await?;
        read_response(stream, limits.max_body).await
    };
    match tokio::time::timeout(limits.total, exchange).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!("请求超时")),
    }
}

async fn connect(url: &Url, host: &str, timeout: Duration) -> Result<Box<dyn AsyncStream>> {
    let port = url
        .port_or_known_default()
        .context("HTTP URL 缺少端口")?;
    let connect = TcpStream::connect((host, port));
    let tcp = match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => bail!("连接超时"),
    };
    let _ = tcp.set_nodelay(true);
    if url.scheme() == "https" {
        let connector = native_tls::TlsConnector::builder()
            .build()
            .context("无法初始化 TLS")?;
        let connector = tokio_native_tls::TlsConnector::from(connector);
        let stream = connector
            .connect(host, tcp)
            .await
            .context("TLS 握手失败")?;
        Ok(Box::new(stream))
    } else {
        Ok(Box::new(tcp))
    }
}

async fn read_response(
    mut stream: Box<dyn AsyncStream>,
    max_body: usize,
) -> Result<Response> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 8192];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return parse_at_eof(&buffer, max_body);
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > max_body + 64 * 1024 {
            bail!("响应内容过大");
        }
        match parse_buffer(&buffer, max_body)? {
            Some(response) => return Ok(response),
            None => continue,
        }
    }
}

struct Headers {
    status: u16,
    headers: Vec<(String, String)>,
    body_start: usize,
}

fn parse_head(buffer: &[u8]) -> Result<Option<Headers>> {
    let Some(end) = find_header_end(buffer) else {
        return Ok(None);
    };
    let head = std::str::from_utf8(&buffer[..end]).context("HTTP 响应头不是 UTF-8")?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.split_whitespace();
    let _version = parts.next();
    let status = parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .context("HTTP 响应缺少状态码")?;
    let mut headers = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok(Some(Headers {
        status,
        headers,
        body_start: end + 4,
    }))
}

fn parse_buffer(buffer: &[u8], max_body: usize) -> Result<Option<Response>> {
    let Some(head) = parse_head(buffer)? else {
        return Ok(None);
    };
    let body = &buffer[head.body_start..];
    if header(&head.headers, "transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        return match decode_chunked(body, max_body)? {
            Some(body) => Ok(Some(Response {
                status: head.status,
                body,
            })),
            None => Ok(None),
        };
    }
    if let Some(length) = header(&head.headers, "content-length") {
        let length: usize = length.parse().context("HTTP Content-Length 无效")?;
        if length > max_body {
            bail!("响应内容过大");
        }
        if body.len() >= length {
            return Ok(Some(Response {
                status: head.status,
                body: body[..length].to_vec(),
            }));
        }
        return Ok(None);
    }
    if body.len() > max_body {
        bail!("响应内容过大");
    }
    Ok(None)
}

fn parse_at_eof(buffer: &[u8], max_body: usize) -> Result<Response> {
    let head = parse_head(buffer)?.context("HTTP 响应头不完整")?;
    let body = &buffer[head.body_start..];
    if header(&head.headers, "transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        let body = decode_chunked(body, max_body)?.context("HTTP 分块响应不完整")?;
        return Ok(Response {
            status: head.status,
            body,
        });
    }
    if let Some(length) = header(&head.headers, "content-length") {
        let length: usize = length.parse().context("HTTP Content-Length 无效")?;
        if length > max_body {
            bail!("响应内容过大");
        }
        if body.len() < length {
            bail!("HTTP 响应正文不完整");
        }
        return Ok(Response {
            status: head.status,
            body: body[..length].to_vec(),
        });
    }
    if body.len() > max_body {
        bail!("响应内容过大");
    }
    Ok(Response {
        status: head.status,
        body: body.to_vec(),
    })
}

fn decode_chunked(mut body: &[u8], max_body: usize) -> Result<Option<Vec<u8>>> {
    let mut decoded = Vec::new();
    loop {
        let Some(line_end) = find_crlf(body) else {
            return Ok(None);
        };
        let line = std::str::from_utf8(&body[..line_end]).context("分块长度无效")?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).context("分块长度无效")?;
        body = &body[line_end + 2..];
        if size == 0 {
            // Terminal chunk may carry trailers; require the final CRLF.
            if body.len() < 2 {
                return Ok(None);
            }
            return Ok(Some(decoded));
        }
        if decoded.len() + size > max_body {
            bail!("响应内容过大");
        }
        if body.len() < size + 2 {
            return Ok(None);
        }
        decoded.extend_from_slice(&body[..size]);
        if &body[size..size + 2] != b"\r\n" {
            bail!("HTTP 分块格式无效");
        }
        body = &body[size + 2..];
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn find_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\r\n")
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    async fn serve(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await;
            socket.write_all(response).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        format!("http://{address}/v1")
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[tokio::test]
    async fn content_length_response_is_read_exactly() {
        let base = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
        let response = post(
            &url(&format!("{base}/audio/transcriptions")),
            &[],
            "application/json",
            b"{}",
            Limits::default(),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"ok");
    }

    #[tokio::test]
    async fn chunked_response_is_decoded() {
        let base = serve(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
        )
        .await;
        let response = get(
            &url(&format!("{base}/models")),
            &[("Authorization", "Bearer test")],
            Limits::default(),
        )
        .await
        .unwrap();
        assert_eq!(response.body, b"abcde");
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let base = serve(b"HTTP/1.1 302 Found\r\nLocation: https://example.com/\r\nContent-Length: 0\r\n\r\n").await;
        let response = post(
            &url(&format!("{base}/audio/transcriptions")),
            &[],
            "application/json",
            b"{}",
            Limits::default(),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 302);
        assert!(response.body.is_empty());
    }

    #[tokio::test]
    async fn oversized_declared_body_is_rejected() {
        let base = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 9999\r\n\r\n").await;
        let error = get(
            &url(&format!("{base}/models")),
            &[],
            Limits {
                max_body: 10,
                ..Limits::default()
            },
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("响应内容过大"));
    }

    #[tokio::test]
    async fn embedded_credentials_are_rejected_without_connecting() {
        let error = get(
            &url("http://user:pass@127.0.0.1:1/"),
            &[],
            Limits::default(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("账号信息"));
    }
}
