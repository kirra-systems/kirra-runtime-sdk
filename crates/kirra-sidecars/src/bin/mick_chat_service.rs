//! **Mick conversational sidecar** — talk to Mick WITHOUT touching the motion
//! path. Plain text in (`POST /chat`), plain conversational text out. No
//! intents, no latch, no seq, no planner, no downstream consumer.
//!
//! The separation this binary exists to make structural:
//!
//! ```text
//!   POST /chat    (this service, :8103) — conversation only, NO motion authority
//!   POST /intent  (mick_service, :8102) — motion-intent classification only,
//!                                          governed downstream
//! ```
//!
//! A live failure showed why the two must not share a door: a greeting posted
//! to the motion-only `/intent` came back as a schema-valid cruise intent.
//! The deterministic fence now blocks that class at `/intent`; this service
//! is where conversation actually belongs. It has no route to intent state
//! (`/intent/last` here is a 404), its model call is a plain `/api/chat`
//! completion with NO schema constraint, and a motion-shaped model reply is
//! replaced by a routing explanation, never passed through
//! (`kirra_sidecars::chat` — the pure, tested core).
//!
//! Chat has NO live robot state: posture, sensors, and diagnostics are only
//! known if the caller includes them in the request text. The prompt forbids
//! inventing nominal status.
//!
//!   POST /chat    {"text":"How are your systems doing?", "session_id"?: "terminal"}
//!     → 200 {"ok":true,"reply":"..."}
//!     → 422 {"ok":false,"error":"MICK_CHAT_EMPTY_REQUEST" | "MICK_CHAT_TEXT_TOO_LONG"
//!                              | "MICK_CHAT_BAD_TEXT" | "MICK_CHAT_BAD_SESSION"}
//!     → 429 {"ok":false,"error":"MICK_CHAT_RATE_LIMITED"}
//!     → 502 {"ok":false,"error":"MICK_CHAT_MODEL_ERROR"}   (Ollama down/failed — fail closed)
//!   GET  /health  → {"status":"ok"}
//!
//! Config (boot-validated, fail-closed on malformed): `KIRRA_MICK_CHAT_ADDR`
//! (default 127.0.0.1:8103, loopback-gated by `net::enforce_bind_policy`);
//! `KIRRA_OLLAMA_URL` (shared — one local Ollama) + `KIRRA_MICK_CHAT_MODEL`
//! (default gemma3:4b); `KIRRA_MICK_CHAT_HISTORY_TURNS` (default 6, 0 = off);
//! `KIRRA_MICK_CHAT_MAX_CHARS` (default 1000);
//! `KIRRA_SIDECAR_ALLOW_NONLOCAL=1` to permit a routable bind.
//!
//! History is in-memory only, bounded per session and across sessions, never
//! persisted, never logged, and cleared on restart.

use std::net::{TcpListener, TcpStream};

use kirra_mick::{ChatWireMessage, OllamaChatClient};
use kirra_sidecars::chat::{handle_request, ChatConfig, ChatMessage, ChatModelClient, ChatService};
use kirra_sidecars::http::{read_request, respond, respond_error};
use kirra_sidecars::net::{allow_nonlocal_from_env, enforce_bind_policy, now_ms};

/// The Ollama chat transport, adapted to the service core's port. Role tags
/// pass through verbatim; nothing is added, constrained, or rewritten.
struct OllamaChat(OllamaChatClient);

impl ChatModelClient for OllamaChat {
    fn chat(&self, messages: &[ChatMessage]) -> Result<String, &'static str> {
        let wire: Vec<ChatWireMessage> = messages
            .iter()
            .map(|m| ChatWireMessage {
                role: m.role,
                content: m.content.clone(),
            })
            .collect();
        self.0.chat_completion(&wire)
    }
}

fn serve(mut stream: TcpStream, svc: &mut ChatService<OllamaChat>) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(status) => return respond_error(&mut stream, status),
    };
    let (status, body) = handle_request(svc, &req.method, &req.path, &req.body, now_ms());
    respond(&mut stream, status, &body);
}

fn main() {
    let addr =
        std::env::var("KIRRA_MICK_CHAT_ADDR").unwrap_or_else(|_| "127.0.0.1:8103".to_string());
    if let Err(e) = enforce_bind_policy(&addr, allow_nonlocal_from_env()) {
        eprintln!("mick_chat_service: {e}");
        std::process::exit(1);
    }
    let cfg = match ChatConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mick_chat_service: {e}");
            std::process::exit(1);
        }
    };
    let client = OllamaChatClient::new();
    let model = client.model().to_string();
    let mut svc = ChatService::new(OllamaChat(client), cfg);
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("mick_chat_service: bind {addr}: {e}");
        std::process::exit(1);
    });
    println!(
        "Mick chat service on http://{addr}  (POST /chat, GET /health) — model {model}, \
         conversation only: NO motion authority, no intent state, history in-memory \
         ({} turns × {} chars)",
        cfg.history_turns, cfg.max_chars
    );
    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(s, &mut svc),
            Err(e) => eprintln!("mick_chat_service: accept error: {e}"),
        }
    }
}
