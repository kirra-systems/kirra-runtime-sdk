//! **Mick typed-intent sidecar** — the shipped "typed text → Mick → intent"
//! binary. No speech, no commands: text goes in, a fail-closed TYPED INTENT
//! comes out, published read-only for the DOER (occy_doer) to consume.
//!
//! A local HTTP endpoint rather than stdin, deliberately: (1) it matches the
//! sidecar convention (`planner_service`/`taj_service` — systemd-supervised,
//! health-checked, one port each); (2) the doer bridge (Python/ROS 2) must be
//! able to POLL the latest intent, which a stdin pipe cannot serve; (3) an
//! operator can still drive it from a shell with one `curl`. The endpoint is
//! loopback-bound by default (`net::enforce_bind_policy`).
//!
//!   POST /intent          {"text":"take me to the loading dock",
//!                          "context"?: {"ego_speed_mps":..,"posture":"NOMINAL",..}}
//!     → 200 {"ok":true,"seq":n,"at_ms":t,"intent":{"intent":"go_to",...}}
//!     → 200 {"ok":true,"intent":null}   (deterministic non-motion: a greeting or
//!                                        read-only question; no model call, no
//!                                        intent, latch and seq UNCHANGED)
//!     → 422 {"ok":false,"error":"MICK_JSON_PARSE_ERROR"}   (fail-closed: no intent latched)
//!     → 429 {"ok":false,"error":"MICK_RATE_LIMITED"}
//!   GET  /intent/last     → {"intent":{...},"seq":n,"at_ms":t} | {"intent":null}
//!   POST /destination     {"destination":{"kind":"named_place","query":"kitchen"},
//!                          "targets"?:[..], "targets_stamp_ms"?, "ego"?}
//!     → 200 {"ok":true,"seq":n,"outcome":"resolved",..}  ADMISSION ONLY — no pose
//!     → 422 {"ok":false,"error":"DEST_NOT_FOUND"|"DEST_AMBIGUOUS"|"DEST_STALE"|
//!            "DEST_UNSUPPORTED_ADDRESS"|…,"detail":"<operator sentence>"}
//!            (fail-closed: NOTHING latched, seq unchanged)
//!     → 429 {"ok":false,"error":"DEST_RATE_LIMITED"}
//!   GET  /destination/last → {"destination":{..,"frame":"map"|"ego",..},"seq":n}
//!                            | {"destination":null}. FRAME-EXPLICIT and its own
//!                            channel: a map-frame registry pose published on
//!                            /intent/last would be misread as ego-relative.
//!   GET  /narration/last  → relay of the verifier's #893 GET /system/verdicts/last
//!                           (AUDITOR tier — never the admin token); 503 if unconfigured
//!   GET  /health          → {"status":"ok"}
//!
//! Config (boot-validated, fail-closed on malformed): `KIRRA_MICK_ADDR`
//! (default 127.0.0.1:8102); `KIRRA_OLLAMA_URL` / `KIRRA_MICK_MODEL` (the
//! OllamaClient pair); `KIRRA_MICK_PERSONA` = `chauffeur` (default) |
//! `courier`; `KIRRA_VERIFIER_URL` + `KIRRA_MICK_AUDITOR_TOKEN` (both or
//! neither — half-configured aborts startup);
//! `KIRRA_SIDECAR_ALLOW_NONLOCAL=1` to permit a routable bind. The typed
//! destination registries/policy come from the SAME `KIRRA_DEST_*` vars
//! `planner_service` reads (`destination::resolver_from_env`) — one config
//! path, so two processes cannot disagree about what "the kitchen" means.
//!
//! Fail-closed by construction: no Ollama / a hallucinated reply / a
//! non-finite goal → 422 and NO latched intent — the doer sees nothing new,
//! grounds nothing, and the platform does not move on Mick's account.

use std::net::{TcpListener, TcpStream};

use kirra_mick::OllamaClient;
use kirra_planner::LlmBrain;
use kirra_sidecars::destination::resolver_from_env;
use kirra_sidecars::destination_service::{DestinationRequest, DestinationService};
use kirra_sidecars::explainer::{explain_reply, not_configured_reply, ExplainRequest};
use kirra_sidecars::http::{read_request, respond, respond_error};
use kirra_sidecars::mick::{IntentOutcome, IntentRequest, IntentService};
use kirra_sidecars::narrator::{fetch_last_verdict, NarratorConfig};
use kirra_sidecars::net::{allow_nonlocal_from_env, enforce_bind_policy, now_ms};

fn serve(
    mut stream: TcpStream,
    svc: &mut IntentService<OllamaClient>,
    dest: &mut DestinationService,
    narrator: Option<&NarratorConfig>,
    explainer: Option<&kirra_mick::explain_client::ExplainClient>,
) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(status) => return respond_error(&mut stream, status),
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/health") => respond(&mut stream, "200 OK", "{\"status\":\"ok\"}"),
        ("POST", "/intent") => match serde_json::from_slice::<IntentRequest>(&req.body) {
            Ok(r) => match svc.handle_text(&r, now_ms()) {
                // The accepted slice embeds verbatim — validated as a
                // standalone JSON object at acceptance, so there is no
                // re-parse and no silent-null fallback here.
                Ok(IntentOutcome::Accepted(_, accepted)) => {
                    respond(&mut stream, "200 OK", &accepted.to_post_wire())
                }
                // Deterministic non-motion: a SUCCESS that carries no intent.
                // `{"intent":null}` is the shape `GET /intent/last` already
                // publishes for "nothing latched", so consumers parse one null
                // form, not two. The latch and seq are untouched.
                Ok(IntentOutcome::NonMotion(_)) => {
                    respond(&mut stream, "200 OK", "{\"ok\":true,\"intent\":null}")
                }
                Err("MICK_RATE_LIMITED") => respond(
                    &mut stream,
                    "429 Too Many Requests",
                    "{\"ok\":false,\"error\":\"MICK_RATE_LIMITED\"}",
                ),
                // Fail-closed: no intent latched, no motion downstream.
                Err(code) => respond(
                    &mut stream,
                    "422 Unprocessable Entity",
                    &serde_json::json!({"ok": false, "error": code}).to_string(),
                ),
            },
            Err(e) => respond(
                &mut stream,
                "400 Bad Request",
                &serde_json::json!({"ok": false, "error": format!("{e}")}).to_string(),
            ),
        },
        // The TYPED DESTINATION door — "drive to the kitchen". The caller
        // sends a kind + the operator's words; the TRUSTED resolver supplies
        // the coordinates. Every non-resolved outcome is a 422 carrying the
        // stable DEST_* code, with the latch and seq UNCHANGED: no grounded
        // destination, no goal, no plan.
        ("POST", "/destination") => match serde_json::from_slice::<DestinationRequest>(&req.body) {
            Ok(r) => match dest.handle(&r, now_ms()) {
                // Admission only — the success body carries NO pose.
                Ok(accepted) => respond(&mut stream, "200 OK", &accepted.to_post_wire()),
                Err(refusal) if refusal.code == "DEST_RATE_LIMITED" => {
                    respond(&mut stream, "429 Too Many Requests", &refusal.to_json())
                }
                Err(refusal) => {
                    respond(&mut stream, "422 Unprocessable Entity", &refusal.to_json())
                }
            },
            Err(e) => respond(
                &mut stream,
                "400 Bad Request",
                &serde_json::json!({"ok": false, "error": format!("{e}")}).to_string(),
            ),
        },
        // The doer-facing grounded destination, FRAME-EXPLICIT. Deliberately
        // its own channel: `/intent/last` is consumed as ego-frame, and a
        // map-frame registry pose published there would be silently misread.
        ("GET", "/destination/last") => match dest.last() {
            Some(d) => respond(&mut stream, "200 OK", &d.to_wire()),
            None => respond(&mut stream, "200 OK", "{\"destination\":null}"),
        },
        ("GET", "/intent/last") => match svc.last() {
            Some(a) => respond(&mut stream, "200 OK", &a.to_wire()),
            None => respond(&mut stream, "200 OK", "{\"intent\":null}"),
        },
        ("GET", "/narration/last") => match narrator {
            Some(cfg) => match fetch_last_verdict(cfg) {
                Ok(v) => respond(&mut stream, "200 OK", &v.to_string()),
                Err(e) => respond(
                    &mut stream,
                    "502 Bad Gateway",
                    &serde_json::json!({"error": e}).to_string(),
                ),
            },
            None => respond(
                &mut stream,
                "503 Service Unavailable",
                "{\"error\":\"NARRATOR_NOT_CONFIGURED\"}",
            ),
        },
        // Tier 4 box 3b — the spoken end of the Kirra World explanation seam.
        // Mick names a SUBJECT and nothing else: the pin, the freshness policy,
        // the lineage depth and the graph bounds are all chosen by the producer,
        // which is what keeps this a capability rather than a query surface.
        ("POST", "/explain") => match explainer {
            Some(client) => match serde_json::from_slice::<ExplainRequest>(&req.body) {
                Ok(r) => respond(&mut stream, "200 OK", &explain_reply(client, &r.subject_id)),
                Err(e) => respond(
                    &mut stream,
                    "400 Bad Request",
                    &serde_json::json!({"error": format!("{e}")}).to_string(),
                ),
            },
            // Unconfigured is NOT "there is nothing to explain": that would be
            // a claim about Kirra World's contents from a process that never
            // asked it anything.
            None => respond(
                &mut stream,
                "503 Service Unavailable",
                &not_configured_reply(),
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
    let addr = std::env::var("KIRRA_MICK_ADDR").unwrap_or_else(|_| "127.0.0.1:8102".to_string());
    if let Err(e) = enforce_bind_policy(&addr, allow_nonlocal_from_env()) {
        eprintln!("mick_service: {e}");
        std::process::exit(1);
    }
    // Persona selects the prompt + the constrained-decode schema. Unknown
    // value → startup abort (fail-closed config, the env_config convention).
    let persona = std::env::var("KIRRA_MICK_PERSONA").unwrap_or_else(|_| "chauffeur".to_string());
    let (client, persona_label): (OllamaClient, _) = match persona.as_str() {
        "chauffeur" => {
            let c = OllamaClient::new();
            let model = c.model().to_string();
            (c, format!("chauffeur ({model})"))
        }
        "courier" => {
            let c = OllamaClient::courier();
            let model = c.model().to_string();
            (c, format!("courier ({model})"))
        }
        other => {
            eprintln!("mick_service: unknown KIRRA_MICK_PERSONA `{other}` (chauffeur | courier)");
            std::process::exit(1);
        }
    };
    let mut svc = IntentService::new(match persona.as_str() {
        "courier" => LlmBrain::courier(client),
        _ => LlmBrain::new(client),
    });
    // Narrator: both vars, neither, or ABORT (half-configured must not run
    // with a silently dead narration surface).
    let narrator = match NarratorConfig::from_env() {
        None => None,
        Some(Ok(cfg)) => Some(cfg),
        Some(Err(e)) => {
            eprintln!("mick_service: {e}");
            std::process::exit(1);
        }
    };
    // Explainer: a base URL or nothing. No half-configured case to abort on —
    // unlike the narrator, the producer serves presentation-only text and
    // carries no credential to pair with the URL.
    let explainer = kirra_sidecars::explainer::explainer_from_env();
    // The trusted destination resolver, from the SAME env config
    // planner_service uses — one config path, so two processes can never
    // disagree about what "the kitchen" means. Malformed → startup abort.
    let mut dest = DestinationService::new(resolver_from_env().unwrap_or_else(|e| {
        eprintln!("mick_service: destination config: {e}");
        std::process::exit(1);
    }));
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("mick_service: bind {addr}: {e}");
        std::process::exit(1);
    });
    println!(
        "Mick intent service on http://{addr}  (POST /intent, GET /intent/last, POST /destination, GET /destination/last, GET /narration/last, POST /explain, GET /health) — persona {persona_label}, narrator {}, explainer {}",
        if narrator.is_some() { "on" } else { "off" },
        if explainer.is_some() { "on" } else { "off" }
    );
    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(
                s,
                &mut svc,
                &mut dest,
                narrator.as_ref(),
                explainer.as_ref(),
            ),
            Err(e) => eprintln!("mick_service: accept error: {e}"),
        }
    }
}
