//! **Occy planner sidecar** (shipped binary; formerly the `kirra-mick`
//! `planner_service` example). POST a world snapshot — optionally with a
//! typed Mick intent — and get back Occy's proposal plus the KIRRA slow-loop
//! checker's verdict and, on refusal, the #893 narration reason.
//!
//!   POST /plan   {"ego":.., "goal":.., "left":.., "right":.., "objects":..,
//!                 "vehicle":.., "intent": {"intent":"go_to",...}? }
//!     → {"kind":"Motion|SafeStop","verdict":"Accept|Clamp|MRCFallback",
//!        "trajectory":[..], "reason_code"?, "reason"?}
//!   GET /health  → {"status":"ok"}
//!
//! Config (boot-validated, fail-closed on malformed — the env_config
//! convention): `KIRRA_PLANNER_ADDR` (default 127.0.0.1:8100);
//! `KIRRA_SIDECAR_ALLOW_NONLOCAL=1` to permit a routable bind.

use std::net::{TcpListener, TcpStream};

use kirra_sidecars::http::{read_request, respond, respond_error};
use kirra_sidecars::net::{allow_nonlocal_from_env, enforce_bind_policy, now_ms, RateLimiter};
use kirra_sidecars::planner::{handle_plan, PlanRequest};

/// /plan rate bound (burst / per-second). Plumbing: each request runs a full
/// plan + slow-loop check; the doer bridge plans at ≤ 20 Hz.
const PLAN_RATE_BURST: f64 = 40.0;
const PLAN_RATE_PER_S: f64 = 20.0;

fn serve(mut stream: TcpStream, limiter: &mut RateLimiter) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(status) => return respond_error(&mut stream, status),
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/health") => respond(&mut stream, "200 OK", "{\"status\":\"ok\"}"),
        ("POST", "/plan") => {
            if !limiter.admit(now_ms()) {
                return respond(
                    &mut stream,
                    "429 Too Many Requests",
                    "{\"error\":\"RATE_LIMITED\"}",
                );
            }
            match serde_json::from_slice::<PlanRequest>(&req.body) {
                Ok(plan_req) => match handle_plan(&plan_req) {
                    Ok(resp) => respond(
                        &mut stream,
                        "200 OK",
                        &serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into()),
                    ),
                    // Seam rejection: fail-closed to NO MOTION (SafeStop +
                    // empty trajectory on the wire).
                    Err(rej) => respond(&mut stream, "422 Unprocessable Entity", &rej.to_json()),
                },
                Err(e) => respond(
                    &mut stream,
                    "400 Bad Request",
                    &serde_json::json!({"error": format!("{e}")}).to_string(),
                ),
            }
        }
        _ => respond(
            &mut stream,
            "404 Not Found",
            "{\"error\":\"unknown route\"}",
        ),
    }
}

fn main() {
    let addr = std::env::var("KIRRA_PLANNER_ADDR").unwrap_or_else(|_| "127.0.0.1:8100".to_string());
    if let Err(e) = enforce_bind_policy(&addr, allow_nonlocal_from_env()) {
        eprintln!("planner_service: {e}");
        std::process::exit(1);
    }
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("planner_service: bind {addr}: {e}");
        std::process::exit(1);
    });
    println!("Occy planner service on http://{addr}  (POST /plan, GET /health)");
    let mut limiter = RateLimiter::new(PLAN_RATE_BURST, PLAN_RATE_PER_S);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(s, &mut limiter),
            Err(e) => eprintln!("planner_service: accept error: {e}"),
        }
    }
}
