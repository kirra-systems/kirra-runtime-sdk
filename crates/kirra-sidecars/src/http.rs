//! Minimal HTTP/1.1 plumbing shared by the sidecar binaries — the hand-rolled
//! `std::net` server the `planner_service`/`taj_service` examples carried,
//! extracted once. Deliberately dependency-free: the sidecars are doer-side
//! plumbing, not the safety-critical verifier (which runs the real axum
//! stack with backpressure pools).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

/// Request-body cap. A plumbing bound (the biggest legitimate payload is a
/// lidar scan / corridor polyline set, well under 1 MiB); over the cap the
/// request is refused with 413 before any work.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// One parsed request: method, path, body bytes.
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

/// Read one HTTP/1.1 request off the stream. `Err` carries the status line to
/// respond with (fail-closed: an unreadable or over-cap request never reaches
/// a handler).
pub fn read_request(stream: &mut TcpStream) -> Result<Request, &'static str> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|_| "500 Internal Server Error")?,
    );
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|_| "400 Bad Request")?;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Err("413 Payload Too Large");
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|_| "400 Bad Request")?;
    }
    Ok(Request { method, path, body })
}

/// Write a JSON response and close.
pub fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(msg.as_bytes());
}

/// Respond with the error status line from [`read_request`].
pub fn respond_error(stream: &mut TcpStream, status: &'static str) {
    respond(stream, status, "{\"error\":\"bad request\"}");
}
