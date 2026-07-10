//! **Taj perception sidecar** (shipped binary; formerly the `kirra-mick`
//! `taj_service` example). POST a `LaserScan`, get the geometric corridor +
//! the assured-clear-distance speed cap (RSS Rule 4). Perception PRODUCER,
//! not the safety authority — fail-closed to the MRC floor on unhealthy
//! perception.
//!
//!   POST /perception  → {"healthy":..,"speed_cap_mps":..,"left":..,...}
//!   GET  /health      → {"status":"ok"}
//!
//! Config: `KIRRA_TAJ_ADDR` (default 127.0.0.1:8101);
//! `KIRRA_SIDECAR_ALLOW_NONLOCAL=1` to permit a routable bind.

use std::net::{TcpListener, TcpStream};

use kirra_sidecars::http::{read_request, respond, respond_error};
use kirra_sidecars::net::{allow_nonlocal_from_env, enforce_bind_policy};
use kirra_sidecars::taj::{handle_perception, PerceptionRequest};

fn serve(mut stream: TcpStream) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(status) => return respond_error(&mut stream, status),
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/health") => respond(&mut stream, "200 OK", "{\"status\":\"ok\"}"),
        ("POST", "/perception") => match serde_json::from_slice::<PerceptionRequest>(&req.body) {
            Ok(p) => respond(
                &mut stream,
                "200 OK",
                &serde_json::to_string(&handle_perception(&p)).unwrap_or_else(|_| "{}".into()),
            ),
            Err(e) => respond(
                &mut stream,
                "400 Bad Request",
                &serde_json::json!({"error": format!("{e}")}).to_string(),
            ),
        },
        _ => respond(
            &mut stream,
            "404 Not Found",
            "{\"error\":\"unknown route\"}",
        ),
    }
}

fn main() {
    let addr = std::env::var("KIRRA_TAJ_ADDR").unwrap_or_else(|_| "127.0.0.1:8101".to_string());
    if let Err(e) = enforce_bind_policy(&addr, allow_nonlocal_from_env()) {
        eprintln!("taj_service: {e}");
        std::process::exit(1);
    }
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("taj_service: bind {addr}: {e}");
        std::process::exit(1);
    });
    println!("Taj perception service on http://{addr}  (POST /perception, GET /health)");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(s),
            Err(e) => eprintln!("taj_service: accept error: {e}"),
        }
    }
}
