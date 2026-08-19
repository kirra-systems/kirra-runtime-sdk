//! **The route table is the security argument — so it is what these test.**
//!
//! Tier 4 box 3b. The ruling on this seam is that Mick may name a subject and
//! the server owns everything else. That is a claim about which requests this
//! process will answer, so the tests are about requests, not about internals.
//!
//! Real `WorldStore`, real `explain_current_subject`, real codec. The one
//! socket test exists because the HTTP plumbing is a hand-rolled copy: a route
//! table proven only through `dispatch` would leave the bytes untested, and the
//! bytes are the half that was copied.

use kirra_explain_types::{ExplainOutcome, EXPLAIN_CURRENT_SUBJECT_PATH};
use kirra_world_explain_service::service::{dispatch, EXPLAIN_SERVICE_CONTRACT};
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-explain3b-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

/// A store holding one folded claim about `subject`.
fn store_with(subject: &str, name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut s = WorldStore::open(&path).expect("open");
    let event_id = EventId::new("ev-1".to_string()).expect("event id");
    let observation_id = ObservationId::new("obs-1".to_string()).expect("obs id");
    s.append(&NewEvent {
        event_id: &event_id,
        observation_id: &observation_id,
        txn_time_ms: T0,
        valid_from_ms: T0,
        valid_to_ms: None,
        source: "warehouse-scanner",
        source_version: "1.0.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "mission",
        subject,
        subject_ref: None,
        predicate: Some("last_seen_at"),
        object: Some("dock_a"),
        payload: "{}",
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    })
    .expect("append");
    s.fold().expect("fold");
    (s, path)
}

fn ask(store: &WorldStore, subject: &str) -> (&'static str, ExplainOutcome) {
    let body = format!("{{\"subject_id\":\"{subject}\"}}");
    let res = dispatch(store, "POST", EXPLAIN_CURRENT_SUBJECT_PATH, body.as_bytes());
    let outcome = serde_json::from_str(&res.body)
        .unwrap_or_else(|e| panic!("body must decode as ExplainOutcome ({e}): {}", res.body));
    (res.status, outcome)
}

// ---------------------------------------------------------------------------
// The capability claim
// ---------------------------------------------------------------------------

/// **The load-bearing one.** This process answers ONE domain question, and the
/// near-misses are the shapes a generic query surface would arrive as: someone
/// adds `/ask` "just for debugging", or `/lineage` because the data is right
/// there. Each must 404 — and the list includes the trailing-slash variant,
/// because a router that normalises paths would quietly open a second door.
#[test]
fn the_route_table_answers_exactly_one_domain_question() {
    let (s, _p) = store_with("package_17", "onedoor");
    for path in [
        "/ask",
        "/query",
        "/explain",
        "/explain/",
        "/explain/current-subject/",
        "/explain/subject",
        "/lineage",
        "/provenance",
        "/world",
        "/",
    ] {
        let res = dispatch(&s, "POST", path, b"{\"subject_id\":\"package_17\"}");
        assert_eq!(
            res.status, "404 Not Found",
            "`{path}` must not be a second way in; got {res:?}"
        );
    }
}

/// A request carrying a steering parameter is REFUSED, not served with the
/// parameter ignored. Silently ignoring it is worse than either alternative:
/// callers in the field would start depending on a contract the server never
/// honoured, and the day it began honouring it would be the day the seam
/// stopped being capability-specific.
#[test]
fn a_request_that_tries_to_steer_the_query_is_refused() {
    let (s, _p) = store_with("package_17", "steer");
    for body in [
        r#"{"subject_id":"package_17","generation":42}"#,
        r#"{"subject_id":"package_17","at_generation":42}"#,
        r#"{"subject_id":"package_17","cursor":"abc"}"#,
        r#"{"subject_id":"package_17","depth":9}"#,
        r#"{"subject_id":"package_17","max_nodes":250}"#,
        r#"{"subject_id":"package_17","freshness":"stale_ok"}"#,
        r#"{"subject_id":"package_17","answer_ref":"a1"}"#,
    ] {
        let res = dispatch(&s, "POST", EXPLAIN_CURRENT_SUBJECT_PATH, body.as_bytes());
        assert_eq!(res.status, "400 Bad Request", "`{body}` must be refused");
    }

    // The positive control: without the extra field the SAME request is served.
    // Without this, a dispatch that refused everything would pass above.
    let (status, outcome) = ask(&s, "package_17");
    assert_eq!(status, "200 OK");
    assert!(
        outcome.explanation().is_some(),
        "the plain request must be served: {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// The three outcomes stay apart
// ---------------------------------------------------------------------------

/// An explained subject: 200, and a real artifact that decodes on the far side
/// of the codec.
#[test]
fn a_recorded_subject_comes_back_explained() {
    let (s, _p) = store_with("package_17", "explained");
    let (status, outcome) = ask(&s, "package_17");
    assert_eq!(status, "200 OK");
    let artifact = outcome.explanation().expect("explained");
    let root = artifact.root().expect("a root claim");
    assert!(
        root.claim.as_str().contains("package_17"),
        "the artifact must describe the subject asked about: {root:?}"
    );
}

/// **`NothingRecorded` is a 200.** The request was understood and answered; the
/// answer is that Kirra World retains nothing. Serving it as a 404 would teach
/// every client to read "no evidence" and "service down" as one condition,
/// which is the single confusion this seam exists to prevent.
#[test]
fn an_unrecorded_subject_is_an_answer_not_a_transport_failure() {
    let (s, _p) = store_with("package_17", "empty");
    let (status, outcome) = ask(&s, "no_such_subject");
    assert_eq!(status, "200 OK", "an honest empty is not an error");
    assert_eq!(outcome, ExplainOutcome::NothingRecorded);
    assert!(
        outcome.explanation().is_none(),
        "nothing recorded must never carry an artifact"
    );
}

/// The right path with the wrong verb is a different mistake from an unknown
/// path, and the response says which.
#[test]
fn the_wrong_verb_on_the_one_route_is_distinguished_from_an_unknown_route() {
    let (s, _p) = store_with("package_17", "verb");
    for method in ["GET", "PUT", "DELETE", "PATCH"] {
        let res = dispatch(&s, method, EXPLAIN_CURRENT_SUBJECT_PATH, b"");
        assert_eq!(res.status, "405 Method Not Allowed", "{method}");
    }
}

/// Liveness carries the CONTRACT, because a stale binary is alive: `{"status":
/// "ok"}` alone cannot distinguish a current producer from a legacy one.
#[test]
fn health_reports_the_contract_a_renderer_must_agree_with() {
    let (s, _p) = store_with("package_17", "health");
    let res = dispatch(&s, "GET", "/health", b"");
    assert_eq!(res.status, "200 OK");
    assert!(
        res.body
            .contains(&format!("\"contract\":{EXPLAIN_SERVICE_CONTRACT}")),
        "health must name the contract version: {}",
        res.body
    );
}

// ---------------------------------------------------------------------------
// The bytes, once — because the plumbing is a copy
// ---------------------------------------------------------------------------

/// A real client over a real socket, through the hand-rolled `read_request` /
/// `respond`. The route table is proven above without a network; this proves
/// the half that was COPIED from `kirra_sidecars::http` actually parses a
/// request and frames a response.
#[test]
fn a_real_request_over_a_real_socket_is_served() {
    use std::io::{Read, Write};

    let (s, _p) = store_with("package_17", "socket");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let req = kirra_world_explain_service::http::read_request(&mut stream).expect("request");
        let res = dispatch(&s, &req.method, &req.path, &req.body);
        kirra_world_explain_service::http::respond(&mut stream, res.status, &res.body);
    });

    let body = r#"{"subject_id":"package_17"}"#;
    let mut client = std::net::TcpStream::connect(addr).expect("connect");
    client
        .write_all(
            format!(
                "POST {EXPLAIN_CURRENT_SUBJECT_PATH} HTTP/1.1\r\nHost: x\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .expect("write");
    let mut raw = String::new();
    client.read_to_string(&mut raw).expect("read");
    server.join().expect("server thread");

    assert!(raw.starts_with("HTTP/1.1 200 OK"), "{raw}");
    let json = raw.split("\r\n\r\n").nth(1).expect("a body");
    let outcome: ExplainOutcome = serde_json::from_str(json).expect("decodes");
    assert!(
        outcome.explanation().is_some(),
        "the socket path must carry a real explanation: {outcome:?}"
    );
}
