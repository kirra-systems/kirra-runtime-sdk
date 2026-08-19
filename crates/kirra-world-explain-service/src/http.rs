//! Minimal HTTP/1.1 plumbing for the explanation producer.
//!
//! # This is a deliberate duplicate, and the duplication is the point for now
//!
//! `kirra_sidecars::http` is the same 70 lines. Depending on it would be the
//! obvious move and it is the wrong one: `kirra-sidecars` depends on
//! `kirra-mick`, so a World crate taking that dependency would put the doer's
//! whole tree — the renderer this producer is meant to be a separate PROCESS
//! from — into the producer's build. The seam would still be a seam on the
//! wire and would have stopped being one in the Cargo graph.
//!
//! So the choice is between a shared crate neither side owns and a second copy
//! of a small, finished piece of plumbing. This copy is the smaller commitment,
//! and it is the one that can be undone: extracting later is mechanical,
//! whereas un-picking a dependency that shipped is not. Whether the third
//! caller justifies the extraction is the open question this leaves.
//!
//! Deliberately dependency-free `std::net`: an explanation producer is a local,
//! single-consumer service, and an async runtime here would buy nothing but a
//! larger tree for a crate whose whole claim is that it is boring.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

/// Request-body cap. The only legitimate body on this service is one subject
/// name, so the cap is generous by three orders of magnitude and still refuses
/// a slow-body flood before any store work.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// One parsed request: method, path, body bytes.
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

/// Read one HTTP/1.1 request off the stream. `Err` carries the status line to
/// respond with — fail-closed: an unreadable or over-cap request never reaches
/// the store.
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

/// Refuse a non-loopback bind unless explicitly permitted.
///
/// This service answers questions about what Kirra World knows and carries no
/// authentication of its own, so the default deployment shape is strictly
/// on-box. Fail-closed: an address that cannot be classified as loopback is
/// treated as routable.
///
/// Pure over `(addr, allow_nonlocal)` so the policy is testable without env
/// mutation (INV-13).
pub fn enforce_bind_policy(addr: &str, allow_nonlocal: bool) -> Result<(), String> {
    if allow_nonlocal {
        return Ok(());
    }
    let loopback = addr
        .parse::<std::net::SocketAddr>()
        .map(|sa| sa.ip().is_loopback())
        .unwrap_or_else(|_| addr.starts_with("localhost:"));
    if loopback {
        Ok(())
    } else {
        Err(format!(
            "refusing to bind non-loopback address {addr}: the explanation \
             producer is an unauthenticated on-box service that answers \
             questions about what Kirra World knows. Set \
             KIRRA_WORLD_EXPLAIN_ALLOW_NONLOCAL=1 only behind a trusted \
             network boundary."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_binds_are_admitted_and_everything_else_is_refused() {
        for addr in ["127.0.0.1:8120", "[::1]:8120", "localhost:8120"] {
            assert!(enforce_bind_policy(addr, false).is_ok(), "{addr}");
        }
        for addr in ["0.0.0.0:8120", "10.1.2.3:8120", "example.com:80", "garbage"] {
            assert!(enforce_bind_policy(addr, false).is_err(), "{addr}");
        }
        assert!(
            enforce_bind_policy("0.0.0.0:8120", true).is_ok(),
            "the explicit opt-in admits a routable bind"
        );
    }
}
