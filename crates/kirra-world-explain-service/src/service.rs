//! **The route table — and it is the whole security argument.**
//!
//! Two routes. Not two families, not two verbs on a resource: two routes, one
//! of which is a health probe.
//!
//! ```text
//! POST /explain/current-subject   {"subject_id": "..."}   -> ExplainOutcome
//! GET  /health                                            -> liveness + contract
//! everything else                                         -> 404
//! ```
//!
//! # Why the dispatch is a pure function
//!
//! [`dispatch`] takes a method, a path and a body and returns a status and a
//! body. No socket, no listener, no runtime. That is what lets the route table
//! be tested as what it is — a POLICY about which questions this process will
//! answer — rather than through a network round trip that also exercises the
//! kernel. The socket loop in the binary does one thing: turn bytes into these
//! three arguments.
//!
//! # What "capability-specific by construction" means here
//!
//! The ruling forbids a generic *ask World* endpoint. Enforcement is layered
//! and none of the layers is a comment:
//!
//! 1. The route table has ONE domain entry, and a test walks a list of
//!    plausible near-misses (`/ask`, `/query`, `/explain`, `/lineage`,
//!    `/explain/current-subject/`) asserting each 404s.
//! 2. The request type is [`ExplainCurrentSubject`], which lives in the crate
//!    `check_explain_artifact_neutral.py` guards, so it cannot grow a
//!    generation, a cursor, a depth or any other numeric knob without redding
//!    that gate.
//! 3. `deny_unknown_fields` refuses a request carrying one anyway, so a client
//!    that starts sending steering parameters fails immediately rather than
//!    being served while its extra fields are ignored.
//! 4. Every bound the work actually uses is a constant in
//!    [`kirra_world_service::explain_subject`], chosen server-side.
//!
//! # HTTP status is transport health; the BODY is the answer
//!
//! `NothingRecorded` comes back `200`, not `404`. The request was understood
//! and answered: the answer is that Kirra World retains nothing about that
//! subject. Encoding a domain outcome as a transport failure is how clients
//! learn to treat *"the service is down"* and *"there is no evidence"* as the
//! same thing, which is the one confusion this seam exists to prevent. So the
//! outcome is always the tagged [`ExplainOutcome`] in the body, and a client
//! decodes exactly one type.

use kirra_explain_types::{
    ExplainCurrentSubject, ExplainOutcome, RelationsOutcome, EXPLAIN_CURRENT_SUBJECT_PATH,
    RELATIONS_PATH_PREFIX,
};
use kirra_world_service::explain_subject::{explain_current_subject, ExplainError};
use kirra_world_service::relations::{current_relations, RelationsError};
use kirra_world_store::WorldStore;

/// The contract version this service serves, echoed on `/health`.
///
/// A stale binary is ALIVE, so `{"status":"ok"}` alone can never distinguish a
/// current producer from a legacy one — the lesson `taj_service` learned during
/// R2 bring-up, applied before it can be re-learned here.
pub const EXPLAIN_SERVICE_CONTRACT: u32 = kirra_explain_types::EXPLANATION_ARTIFACT_VERSION;

/// A response: an HTTP status line and a JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: &'static str,
    pub body: String,
}

impl Response {
    fn json(status: &'static str, body: String) -> Self {
        Self { status, body }
    }

    /// Encode an outcome. A serializer failure becomes an `Unavailable` with a
    /// literal body rather than a panic or an empty `{}`: this process must
    /// never answer an explanation request with something a renderer could read
    /// as an explanation.
    fn outcome(status: &'static str, outcome: &ExplainOutcome) -> Self {
        match serde_json::to_string(outcome) {
            Ok(body) => Self::json(status, body),
            Err(_) => Self::json(
                "500 Internal Server Error",
                "{\"outcome\":\"unavailable\",\"reason\":\"the explanation could not be encoded\"}"
                    .to_string(),
            ),
        }
    }
}

/// Handle one request.
///
/// Pure over the store: same store state and same arguments give the same
/// response, which is what makes the route table testable as policy.
pub fn dispatch(store: &WorldStore, method: &str, path: &str, body: &[u8]) -> Response {
    match (method, path) {
        ("GET", "/health") => Response::json(
            "200 OK",
            format!(
                "{{\"status\":\"ok\",\"service\":\"world-explain\",\"contract\":{EXPLAIN_SERVICE_CONTRACT}}}"
            ),
        ),
        ("POST", EXPLAIN_CURRENT_SUBJECT_PATH) => match serde_json::from_slice::<ExplainCurrentSubject>(body) {
            Ok(req) => explain(store, &req.subject_id),
            // A rejected field is the contract working, so the message says
            // which shape is accepted rather than echoing serde's error alone.
            Err(e) => Response::json(
                "400 Bad Request",
                serde_json::json!({
                    "error": format!("{e}"),
                    "expected": {"subject_id": "<subject>"},
                    "note": "this endpoint accepts a subject and nothing else — \
                             generation, cursor, depth and freshness are chosen by the server",
                })
                .to_string(),
            ),
        },
        // The relationship view. A GET with the subject in the PATH, which is
        // the one place this service accepts a caller-supplied value in a URL
        // -- see `subject_from_path` for why the parse is fail-closed rather
        // than forgiving.
        ("GET", p) if p.starts_with(RELATIONS_PATH_PREFIX) => match subject_from_path(p) {
            Some(subject) => relations(store, subject),
            None => Response::relations(
                "400 Bad Request",
                &RelationsOutcome::NotAnEntity {
                    reason: "the path segment after /relations/ must be one non-empty \
                             subject with no encoding and no further segments"
                        .to_string(),
                },
            ),
        },
        // A known prefix with the wrong verb. READ-ONLY is the whole contract,
        // so a POST or DELETE here is worth naming rather than 404-ing as an
        // unknown route -- a client that thinks it can write should be told it
        // cannot, not told the road does not exist.
        (_, p) if p.starts_with(RELATIONS_PATH_PREFIX) => Response::json(
            "405 Method Not Allowed",
            format!("{{\"error\":\"use GET {RELATIONS_PATH_PREFIX}{{subject}} — this service is read-only\"}}"),
        ),
        // A known path with the wrong verb is a distinct mistake from an
        // unknown one, and saying so costs nothing.
        (_, EXPLAIN_CURRENT_SUBJECT_PATH) => Response::json(
            "405 Method Not Allowed",
            format!("{{\"error\":\"use POST {EXPLAIN_CURRENT_SUBJECT_PATH}\"}}"),
        ),
        _ => Response::json("404 Not Found", "{\"error\":\"unknown route\"}".to_string()),
    }
}

/// Encode a relationship outcome, with [`Response::outcome`]'s failure rule: a
/// serializer error becomes a literal `unavailable` body rather than a panic or
/// an empty `{}`, because this process must never answer with something a
/// consumer could read as an answer.
impl Response {
    fn relations(status: &'static str, outcome: &RelationsOutcome) -> Self {
        match serde_json::to_string(outcome) {
            Ok(body) => Self::json(status, body),
            Err(_) => Self::json(
                "500 Internal Server Error",
                "{\"outcome\":\"unavailable\",\"reason\":\"the relationship view could not be encoded\"}"
                    .to_string(),
            ),
        }
    }
}

/// The subject from `/relations/{subject}`, or `None` if the path is not that.
///
/// **Fail-closed, and deliberately not a URL decoder.** A percent-escape or a
/// further path segment is REFUSED rather than half-interpreted: this service
/// has no encoding contract, and guessing one means two spellings of a subject
/// could reach the store as different strings — or worse, the same one. A
/// caller with an exotic subject id gets a clear refusal instead of a silently
/// wrong answer, and adding an encoding later is a contract decision rather
/// than a bug fix.
fn subject_from_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(RELATIONS_PATH_PREFIX)?;
    if rest.is_empty() || rest.contains('/') || rest.contains('%') || rest.contains('?') {
        return None;
    }
    Some(rest)
}

/// The relationship view, with every failure mapped to a case a caller matches.
fn relations(store: &WorldStore, subject: &str) -> Response {
    match current_relations(store, subject) {
        // A subject related to nothing is an ANSWER, and 200. Encoding it as a
        // 404 is how clients learn to read "the service is down" and "no
        // relations" as the same thing.
        Ok(view) => Response::relations("200 OK", &RelationsOutcome::Related { view }),
        Err(RelationsError::NotAnEntity { detail }) => Response::relations(
            "400 Bad Request",
            &RelationsOutcome::NotAnEntity { reason: detail },
        ),
        Err(e) => Response::relations(
            "503 Service Unavailable",
            &RelationsOutcome::Unavailable {
                reason: e.to_string(),
            },
        ),
    }
}

/// The one operation, with every failure mapped to a case a caller must match.
fn explain(store: &WorldStore, subject_id: &str) -> Response {
    match explain_current_subject(store, subject_id) {
        Ok(explanation) => Response::outcome("200 OK", &ExplainOutcome::Explained { explanation }),
        // Answered, and the answer is "nothing". 200, deliberately — see the
        // module docs.
        Err(ExplainError::NothingRecorded) => {
            Response::outcome("200 OK", &ExplainOutcome::NothingRecorded)
        }
        // Everything else is a failure to produce an explanation, and the
        // renderer must not receive anything it could narrate as one. The
        // Display impls on ExplainError already keep "refused" apart from
        // "unreadable", so the reason string carries the distinction without
        // this layer re-deriving it.
        Err(e) => Response::outcome("503 Service Unavailable", &ExplainOutcome::unavailable(e)),
    }
}
