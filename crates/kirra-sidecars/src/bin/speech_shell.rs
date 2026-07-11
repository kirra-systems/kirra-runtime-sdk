//! **speech_shell** — the voice UX shell around the governed loop. You speak;
//! Mick proposes; Occy plans; KIRRA bounds; the car says why — out loud.
//!
//! A THIN wrapper, by construction (see `speech.rs` module docs): STT and TTS
//! are external OS processes (whisper.cpp / Piper — never crate deps of the
//! safety workspace), and the transcript enters the loop ONLY as text through
//! the EXISTING `mick_service` `POST /intent` door — the same fail-closed
//! `MickIntent::parse_llm_json` path typed text uses. A misheard command
//! fails exactly as typed garbage does: 422, no intent latched, no motion.
//! This binary constructs no intent, no goal, no command — grep it.
//!
//! Push-to-talk, never always-on: in interactive mode each turn records ONE
//! bounded clip via `KIRRA_RECORD_CMD` (e.g. `arecord -d 4 …`) after you
//! press Enter. No wake word, no open mic.
//!
//! Usage:
//!   speech_shell --wav clip.wav      # one turn from a recorded/synthesized WAV
//!   speech_shell                     # interactive push-to-talk (needs KIRRA_RECORD_CMD)
//!
//! Env (fail-closed on malformed; see docs/testing/SPEECH_KITT_DEMO.md):
//!   KIRRA_STT_CMD     required   e.g. "whisper-cli -m models/ggml-base.en.bin -np -nt -f"
//!   KIRRA_TTS_CMD     optional   e.g. "./speak.sh"  (text on stdin; unset → print only)
//!   KIRRA_RECORD_CMD  optional   e.g. "arecord -d 4 -f S16_LE -r 16000 -c 1"
//!   KIRRA_MICK_URL    optional   default http://127.0.0.1:8102 (the mick_service)

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;

use kirra_sidecars::speech::{
    narration_sentence, parse_cmd, speech_turn, utterance_for, PrintSpeaker, ProcessSpeaker,
    ProcessTranscriber, Speaker, SpeechTurn, RECORD_CMD_ENV, STT_CMD_ENV, TTS_CMD_ENV,
};

fn env_cmd(key: &'static str) -> Option<(String, Vec<String>)> {
    match parse_cmd(key, std::env::var(key).ok().as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("speech_shell: {e}");
            std::process::exit(1);
        }
    }
}

/// POST the transcript — as text, nothing else — to the existing intent door.
fn publish_to_mick(
    http: &reqwest::blocking::Client,
    mick_url: &str,
    text: &str,
) -> Result<String, String> {
    let resp = http
        .post(format!("{mick_url}/intent"))
        .json(&serde_json::json!({ "text": text }))
        .send()
        .map_err(|e| format!("mick_service unreachable: {e}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if status.is_success() {
        Ok(body)
    } else {
        Err(body)
    }
}

/// Fetch and render the #893 narration (mick_service's read-only relay of the
/// verifier's auditor-tier `GET /system/verdicts/last`).
fn narrate(http: &reqwest::blocking::Client, mick_url: &str) -> String {
    match http
        .get(format!("{mick_url}/narration/last"))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
    {
        Ok(resp) => match resp.json::<serde_json::Value>() {
            Ok(v) => narration_sentence(&v),
            Err(e) => format!("The narration channel returned unreadable data ({e})."),
        },
        Err(e) => format!("The narration channel is not available ({e})."),
    }
}

fn one_turn(
    stt: &ProcessTranscriber,
    speaker: &mut dyn Speaker,
    http: &reqwest::blocking::Client,
    mick_url: &str,
    wav: &Path,
) {
    let mut publish = |text: &str| publish_to_mick(http, mick_url, text);
    match speech_turn(stt, wav, &mut publish) {
        Ok(turn) => {
            let line = utterance_for(&turn);
            println!("mick: {line}");
            if let Err(e) = speaker.speak(&line) {
                eprintln!("speech_shell: TTS failed ({e}) — continuing print-only");
            }
            // The governor's word, spoken from the EXISTING narration
            // side-channel — only when a proposal was actually published.
            if matches!(turn, SpeechTurn::Published { .. }) {
                let verdict_line = narrate(http, mick_url);
                println!("governor: {verdict_line}");
                if let Err(e) = speaker.speak(&verdict_line) {
                    eprintln!("speech_shell: TTS failed ({e})");
                }
            }
        }
        // A bad clip / dead STT process: nothing was published, say so.
        Err(e) => eprintln!("speech_shell: turn failed fail-closed ({e}) — nothing proposed"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mick_url =
        std::env::var("KIRRA_MICK_URL").unwrap_or_else(|_| "http://127.0.0.1:8102".to_string());

    let Some((stt_prog, stt_args)) = env_cmd(STT_CMD_ENV) else {
        eprintln!("speech_shell: {STT_CMD_ENV} is required (the external STT command — see docs/testing/SPEECH_KITT_DEMO.md)");
        std::process::exit(1);
    };
    let stt = ProcessTranscriber::new(stt_prog, stt_args);

    let mut speaker: Box<dyn Speaker> = match env_cmd(TTS_CMD_ENV) {
        Some((prog, tts_args)) => Box::new(ProcessSpeaker::new(prog, tts_args)),
        None => {
            eprintln!("speech_shell: {TTS_CMD_ENV} unset — narration will be printed, not spoken");
            Box::new(PrintSpeaker)
        }
    };
    let http = reqwest::blocking::Client::new();

    // --wav mode: one turn from a file (also what the CI-adjacent smoke uses).
    if let [flag, path] = args.as_slice() {
        if flag == "--wav" {
            one_turn(&stt, speaker.as_mut(), &http, &mick_url, Path::new(path));
            return;
        }
    }
    if !args.is_empty() {
        eprintln!("usage: speech_shell [--wav clip.wav]");
        std::process::exit(2);
    }

    // Interactive push-to-talk.
    let Some((rec_prog, rec_args)) = env_cmd(RECORD_CMD_ENV) else {
        eprintln!(
            "speech_shell: interactive mode needs {RECORD_CMD_ENV} (a bounded push-to-talk \
             recorder, e.g. \"arecord -d 4 -f S16_LE -r 16000 -c 1\"); or use --wav <file>"
        );
        std::process::exit(1);
    };
    println!("speech_shell: push-to-talk — press Enter to record one clip (Ctrl-D quits).");
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let wav: PathBuf =
            std::env::temp_dir().join(format!("kirra_speech_{}.wav", std::process::id()));
        println!("(recording…)");
        match Command::new(&rec_prog).args(&rec_args).arg(&wav).status() {
            Ok(s) if s.success() => one_turn(&stt, speaker.as_mut(), &http, &mick_url, &wav),
            Ok(s) => eprintln!("speech_shell: recorder exited {s} — nothing proposed"),
            Err(e) => eprintln!("speech_shell: recorder failed ({e}) — nothing proposed"),
        }
        let _ = std::fs::remove_file(&wav);
    }
}
