//! **Mick conversational sidecar** — talk to Mick WITHOUT touching the motion
//! path, tuned for LOW PERCEIVED LATENCY (time to first audible response).
//!
//! ```text
//!   POST /chat         (:8103) one-shot reply + timings
//!   POST /chat/stream  (:8103) SSE: start / delta / sentence / done — TTS can
//!                              begin on the first sentence
//!   GET  /health               warming | ready | degraded (honest readiness)
//!   POST /intent       (:8102, mick_service) — the governed motion door; NOT here
//! ```
//!
//! Latency levers wired here: model residency (`keep_alive` on every request +
//! a one-shot boot warm-up), a compact system prompt, capped output tokens,
//! streaming deltas with sentence chunking, and deterministic fast routes —
//! all in `kirra_sidecars::chat` (pure, tested). Timings are logged as stage
//! durations only; transcript and reply text are never logged.
//!
//! Concurrency: the serve loop is single-threaded — exactly ONE in-flight
//! model request (`CHAT_MAX_INFLIGHT`), further connections wait in the TCP
//! backlog. A client that hangs up mid-stream cancels the generation (the
//! sink write fails → the transport stops consuming → dropping the response
//! aborts Ollama's decode). Process shutdown closes connections promptly.
//!
//! Config (boot-validated, fail-closed): `KIRRA_MICK_CHAT_ADDR`
//! (default 127.0.0.1:8103, loopback-gated); `KIRRA_OLLAMA_URL` +
//! `KIRRA_MICK_CHAT_MODEL` (default gemma3:4b);
//! `KIRRA_MICK_CHAT_MAX_INPUT_CHARS` (500);
//! `KIRRA_MICK_CHAT_MAX_OUTPUT_TOKENS` (48);
//! `KIRRA_MICK_CHAT_TEMPERATURE` (0.2); `KIRRA_MICK_CHAT_KEEP_ALIVE` (30m);
//! `KIRRA_MICK_CHAT_HISTORY_TURNS` (0 = stateless, hard cap 2);
//! `KIRRA_MICK_CHAT_WARMUP` (default on; `0` = fail-soft, health reports the
//! listener without a warm model); `KIRRA_SIDECAR_ALLOW_NONLOCAL=1` for a
//! routable bind.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

use kirra_mick::{ChatGenOptions, ChatWireMessage, OllamaChatClient};
use kirra_sidecars::chat::{
    handle_request, stream_chat_into, ChatConfig, ChatGenParams, ChatMessage, ChatModelClient,
    ChatService, WarmState,
};
use kirra_sidecars::http::{read_request, respond, respond_error};
use kirra_sidecars::net::{allow_nonlocal_from_env, enforce_bind_policy};

/// The Ollama chat transport, adapted to the core's streaming port. Role tags
/// and deltas pass through verbatim; nothing is added or constrained.
struct OllamaChat(OllamaChatClient);

impl ChatModelClient for OllamaChat {
    fn stream_chat(
        &self,
        messages: &[ChatMessage],
        gen: &ChatGenParams,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<(), &'static str> {
        let wire: Vec<ChatWireMessage> = messages
            .iter()
            .map(|m| ChatWireMessage {
                role: m.role,
                content: m.content.clone(),
            })
            .collect();
        let opts = ChatGenOptions {
            keep_alive: gen.keep_alive.clone(),
            num_predict: gen.num_predict,
            temperature: gen.temperature,
        };
        self.0.stream_chat(&wire, &opts, on_delta)
    }
}

/// Shared warm-up state: 0 warming, 1 ready, 2 degraded; warm-up duration ms.
struct Shared {
    state: AtomicU8,
    warmup_ms: AtomicU64,
}

fn warm_state(shared: &Shared) -> (WarmState, Option<u64>) {
    match shared.state.load(Ordering::Relaxed) {
        1 => (
            WarmState::Ready,
            Some(shared.warmup_ms.load(Ordering::Relaxed)),
        ),
        2 => (WarmState::Degraded, None),
        _ => (WarmState::Warming, None),
    }
}

/// One-shot warm-up: a tiny capped completion whose output is discarded. On
/// success the model is resident (keep_alive) and /health flips to ready;
/// on failure → degraded (requests still fail closed individually).
fn warm_up(cfg: &ChatConfig, shared: &Shared) {
    let t0 = Instant::now();
    let client = OllamaChatClient::new();
    let messages = [ChatWireMessage {
        role: "user",
        content: "Say the single word: ready".to_string(),
    }];
    let opts = ChatGenOptions {
        keep_alive: cfg.keep_alive.clone(),
        num_predict: 8,
        temperature: 0.0,
    };
    let mut discard = |_: &str| true;
    match client.stream_chat(&messages, &opts, &mut discard) {
        Ok(()) => {
            let ms = t0.elapsed().as_millis() as u64;
            shared.warmup_ms.store(ms, Ordering::Relaxed);
            shared.state.store(1, Ordering::Relaxed);
            eprintln!("mick_chat_service: warm-up complete (warmup_ms={ms})");
        }
        Err(e) => {
            shared.state.store(2, Ordering::Relaxed);
            eprintln!("mick_chat_service: warm-up failed ({e}) — health degraded; requests fail closed until Ollama is reachable");
        }
    }
}

fn serve(
    mut stream: TcpStream,
    svc: &mut ChatService<OllamaChat>,
    shared: &Shared,
    model: &str,
    epoch: Instant,
) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(status) => return respond_error(&mut stream, status),
    };
    // Process-monotonic millisecond clock (shared across requests so the rate
    // limiter refills correctly; per-request stages are differences on it).
    let mut clock = move || epoch.elapsed().as_millis() as u64;
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/chat/stream") => {
            if let Err(code) = stream_chat_into(svc, &req.body, &mut clock, &mut stream) {
                // Nothing streamed yet on admission errors — plain JSON error.
                let status = match code {
                    "MICK_CHAT_RATE_LIMITED" => "429 Too Many Requests",
                    "MICK_CHAT_MODEL_ERROR" => "502 Bad Gateway",
                    "MICK_CHAT_CANCELLED" => return, // client already gone
                    "MICK_CHAT_BAD_REQUEST" => "400 Bad Request",
                    _ => "422 Unprocessable Entity",
                };
                respond(
                    &mut stream,
                    status,
                    &format!("{{\"ok\":false,\"error\":\"{code}\"}}"),
                );
            }
        }
        (method, path) => {
            let (status, body) = handle_request(svc, method, path, &req.body, &mut clock, {
                let (s, w) = warm_state(shared);
                (s, model, w)
            });
            respond(&mut stream, status, &body);
        }
    }
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
    let mut svc = ChatService::new(OllamaChat(client), cfg.clone());
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("mick_chat_service: bind {addr}: {e}");
        std::process::exit(1);
    });
    let shared = Arc::new(Shared {
        state: AtomicU8::new(0),
        warmup_ms: AtomicU64::new(0),
    });
    let warmup_enabled = std::env::var("KIRRA_MICK_CHAT_WARMUP")
        .map(|v| v != "0")
        .unwrap_or(true);
    if warmup_enabled {
        let cfg2 = cfg.clone();
        let shared2 = Arc::clone(&shared);
        std::thread::spawn(move || warm_up(&cfg2, &shared2));
    } else {
        // Explicit fail-soft: the operator opted out of readiness gating.
        shared.state.store(1, Ordering::Relaxed);
        eprintln!("mick_chat_service: warm-up disabled (KIRRA_MICK_CHAT_WARMUP=0) — health reports ready without a warm model");
    }
    println!(
        "Mick chat service on http://{addr}  (POST /chat, POST /chat/stream, GET /health) — \
         model {model}, conversation only: NO motion authority. \
         stateless={} keep_alive={} num_predict={} max_input={} inflight=1",
        cfg.history_turns == 0,
        cfg.keep_alive,
        cfg.max_output_tokens,
        cfg.max_input_chars
    );
    let epoch = Instant::now();
    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(s, &mut svc, &shared, &model, epoch),
            Err(e) => eprintln!("mick_chat_service: accept error: {e}"),
        }
    }
}
