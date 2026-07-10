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
//!     → 422 {"ok":false,"error":"MICK_JSON_PARSE_ERROR"}   (fail-closed: no intent latched)
//!     → 429 {"ok":false,"error":"MICK_RATE_LIMITED"}
//!   GET  /intent/last     → {"intent":{...},"seq":n,"at_ms":t} | {"intent":null}
//!   GET  /narration/last  → relay of the verifier's #893 GET /system/verdicts/last
//!                           (AUDITOR tier — never the admin token); 503 if unconfigured
//!   GET  /health          → {"status":"ok"}
//!
//! Config (boot-validated, fail-closed on malformed): `KIRRA_MICK_ADDR`
//! (default 127.0.0.1:8102); `KIRRA_OLLAMA_URL` / `KIRRA_MICK_MODEL` (the
//! OllamaClient pair); `KIRRA_MICK_PERSONA` = `chauffeur` (default) |
//! `courier`; `KIRRA_VERIFIER_URL` + `KIRRA_MICK_AUDITOR_TOKEN` (both or
//! neither — half-configured aborts startup);
//! `KIRRA_SIDECAR_ALLOW_NONLOCAL=1` to permit a routable bind.
//!
//! Fail-closed by construction: no Ollama / a hallucinated reply / a
//! non-finite goal → 422 and NO latched intent — the doer sees nothing new,
//! grounds nothing, and the platform does not move on Mick's account.

use std::net::{TcpListener, TcpStream};

use kirra_mick::OllamaClient;
use kirra_planner::LlmBrain;
use kirra_sidecars::http::{read_request, respond, respond_error};
use kirra_sidecars::mick::{IntentRequest, IntentService};
use kirra_sidecars::narrator::{fetch_last_verdict, NarratorConfig};
use kirra_sidecars::net::{allow_nonlocal_from_env, enforce_bind_policy, now_ms};

fn serve(
    mut stream: TcpStream,
    svc: &mut IntentService<OllamaClient>,
    narrator: Option<&NarratorConfig>,
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
                Ok((_, accepted)) => respond(&mut stream, "200 OK", &accepted.to_post_wire()),
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
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("mick_service: bind {addr}: {e}");
        std::process::exit(1);
    });
    println!(
        "Mick intent service on http://{addr}  (POST /intent, GET /intent/last, GET /narration/last, GET /health) — persona {persona_label}, narrator {}",
        if narrator.is_some() { "on" } else { "off" }
    );
    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(s, &mut svc, narrator.as_ref()),
            Err(e) => eprintln!("mick_service: accept error: {e}"),
        }
    }
}
