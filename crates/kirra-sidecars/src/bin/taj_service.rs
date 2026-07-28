//! **Taj perception sidecar** (shipped binary; formerly the `kirra-mick`
//! `taj_service` example). POST a `LaserScan`, get the geometric corridor +
//! the assured-clear-distance speed cap (RSS Rule 4). Perception PRODUCER,
//! not the safety authority — fail-closed to the MRC floor on unhealthy
//! perception.
//!
//!   POST /perception  → {"healthy":..,"speed_cap_mps":..,"left":..,...}
//!   GET  /health      → {"status":"ok","service":"taj","contract":N}
//!   GET  /capabilities → {"service":"taj","contract":N,"capabilities":[…]}
//!
//! Config: `KIRRA_TAJ_ADDR` (default 127.0.0.1:8101);
//! `KIRRA_SIDECAR_ALLOW_NONLOCAL=1` to permit a routable bind.

use std::net::{TcpListener, TcpStream};

use kirra_sidecars::capabilities::Capabilities;
use kirra_sidecars::http::{read_request, respond, respond_error};
use kirra_sidecars::net::{allow_nonlocal_from_env, enforce_bind_policy};
use kirra_sidecars::taj::{
    handle_perception_tracked_at, watchdog_armed_from_env, PerceptionRequest, SensorWatchdogConfig,
    TrackedPerceptionState,
};

/// Wall clock in ms, for the watchdog's ARRIVAL observation.
///
/// Read here in the binary rather than inside the perception core so the core
/// stays a pure function of its inputs and every test can inject a clock instead
/// of sleeping.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn serve(mut stream: TcpStream, perception_state: &mut TrackedPerceptionState) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(status) => return respond_error(&mut stream, status),
    };
    match (req.method.as_str(), req.path.as_str()) {
        // Liveness AND capability in one place: a stale binary is ALIVE, so
        // {"status":"ok"} alone can never distinguish current from legacy —
        // which is exactly how an installed pre-frame_id taj_service read as
        // healthy during R2 bring-up.
        ("GET", "/health") => {
            let caps = Capabilities::taj();
            respond(
                &mut stream,
                "200 OK",
                &format!(
                    "{{\"status\":\"ok\",\"service\":\"taj\",\"contract\":{}}}",
                    caps.contract
                ),
            )
        }
        ("GET", "/capabilities") => respond(&mut stream, "200 OK", &Capabilities::taj().to_json()),
        ("POST", "/perception") => match serde_json::from_slice::<PerceptionRequest>(&req.body) {
            Ok(p) => respond(
                &mut stream,
                "200 OK",
                &serde_json::to_string(&handle_perception_tracked_at(
                    &p,
                    perception_state,
                    now_ms(),
                ))
                .unwrap_or_else(|_| "{}".into()),
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
    // Single-threaded service: one deterministic tracker state persists
    // across consecutive POST /perception requests.
    let armed = watchdog_armed_from_env();
    if armed {
        eprintln!(
            "taj_service: scan-liveness watchdog ARMED (#1211) — a frozen, starved \
             or blinded /scan floors the speed cap. Set \
             KIRRA_SENSOR_WATCHDOG_ENABLED=0 to opt out."
        );
    } else {
        eprintln!(
            "taj_service: scan-liveness watchdog DISARMED — a frozen /scan will \
             NOT be detected. This is the pre-#1211 behaviour."
        );
    }
    let mut perception_state =
        TrackedPerceptionState::with_watchdog(SensorWatchdogConfig::r2_lidar(), armed);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(s, &mut perception_state),
            Err(e) => eprintln!("taj_service: accept error: {e}"),
        }
    }
}
