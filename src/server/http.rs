use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName};
use std::io::{self, Read, Write};
use std::net::TcpStream;

pub fn read_http_headers(stream: &mut TcpStream) -> io::Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 1024];

    loop {
        let bytes_read = stream.read(&mut chunk)?;
        if bytes_read == 0 {
            break;
        }

        buffer.extend_from_slice(&chunk[..bytes_read]);

        if buffer.windows(4).any(|window| window == b"\r\n\r\n")
            || buffer.windows(2).any(|window| window == b"\n\n")
        {
            break;
        }

        if buffer.len() > 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP headers exceeded 16 KiB",
            ));
        }
    }

    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

pub fn write_upstream_response(
    stream: &mut TcpStream,
    status: StatusCode,
    headers: &HeaderMap,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    let content_type = header_to_string(headers, CONTENT_TYPE.as_str())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let digest = header_to_string(headers, "docker-content-digest");
    let status_line = status.canonical_reason().map_or_else(
        || format!("{} Error", status.as_u16()),
        |reason| format!("{} {}", status.as_u16(), reason),
    );

    write_registry_response(
        stream,
        &status_line,
        &content_type,
        digest.as_deref(),
        body,
        head_only,
    )
}

pub fn write_registry_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    digest: Option<&str>,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nDocker-Distribution-Api-Version: registry/2.0\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    )?;

    if let Some(digest) = digest {
        write!(stream, "Docker-Content-Digest: {digest}\r\n")?;
    }

    write!(stream, "\r\n")?;

    if !head_only {
        stream.write_all(body)?;
    }

    Ok(())
}

pub fn write_text_response(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub fn write_html_response(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub fn write_redirect_response(stream: &mut TcpStream, location: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 303 See Other\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

pub fn query_value(path: &str, name: &str) -> Option<String> {
    let (_, query) = path.split_once('?')?;

    query.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

pub fn header_to_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(HeaderName::from_bytes(name.as_bytes()).ok()?)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

pub fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn url_encode(value: &str) -> String {
    let mut encoded = String::new();

    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }

    encoded
}

pub fn url_decode(value: &str) -> Option<String> {
    let mut decoded = Vec::new();
    let mut bytes = value.bytes();

    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => decoded.push(b' '),
            b'%' => {
                let high = bytes.next()?;
                let low = bytes.next()?;
                decoded.push(hex_value(high)? * 16 + hex_value(low)?);
            }
            _ => decoded.push(byte),
        }
    }

    String::from_utf8(decoded).ok()
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;

    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
