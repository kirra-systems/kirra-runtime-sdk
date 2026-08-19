//! **`world_explain_service`** — the Kirra World explanation producer.
//!
//!   POST /explain/current-subject  {"subject_id":".."}  → ExplainOutcome
//!   GET  /health                                        → liveness + contract
//!
//! Config: `KIRRA_WORLD_DB` (required — the store to explain from);
//! `KIRRA_WORLD_EXPLAIN_ADDR` (default `127.0.0.1:8120`);
//! `KIRRA_WORLD_EXPLAIN_ALLOW_NONLOCAL=1` to permit a routable bind.
//!
//! Single-threaded and read-only. It holds no state between requests: every
//! response is a pure function of the store as it stands, which is what makes
//! two identical requests against an unchanged store byte-identical answers.

use std::net::{TcpListener, TcpStream};

use kirra_world_explain_service::http::{
    enforce_bind_policy, read_request, respond, respond_error,
};
use kirra_world_explain_service::service::dispatch;
use kirra_world_store::WorldStore;

const DEFAULT_ADDR: &str = "127.0.0.1:8120";

fn serve(mut stream: TcpStream, store: &WorldStore) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(status) => return respond_error(&mut stream, status),
    };
    let res = dispatch(store, &req.method, &req.path, &req.body);
    respond(&mut stream, res.status, &res.body);
}

fn fail(msg: &str) -> ! {
    eprintln!("world_explain_service: {msg}");
    std::process::exit(1)
}

fn main() {
    // Required, with no default. A default path would silently serve an empty
    // store — and an empty store answers "nothing is recorded about that
    // subject", which is a claim about Kirra World rather than a configuration
    // error. Fail-closed at startup instead.
    let db = match std::env::var("KIRRA_WORLD_DB") {
        Ok(p) if !p.trim().is_empty() => p,
        _ => fail("KIRRA_WORLD_DB is required — refusing to start without a store to explain from"),
    };
    let addr =
        std::env::var("KIRRA_WORLD_EXPLAIN_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let allow_nonlocal = std::env::var("KIRRA_WORLD_EXPLAIN_ALLOW_NONLOCAL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if let Err(e) = enforce_bind_policy(&addr, allow_nonlocal) {
        fail(&e);
    }

    let store = match WorldStore::open(std::path::Path::new(&db)) {
        Ok(s) => s,
        Err(e) => fail(&format!("open {db}: {e:?}")),
    };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => fail(&format!("bind {addr}: {e}")),
    };
    println!(
        "Kirra World explanation service on http://{addr}  \
         (POST /explain/current-subject, GET /health) — store {db}, read-only"
    );

    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(s, &store),
            Err(e) => eprintln!("world_explain_service: accept error: {e}"),
        }
    }
}
