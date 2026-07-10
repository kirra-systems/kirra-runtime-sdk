//! Speech I/O for the governed loop — **input transduction and output
//! rendering ONLY.**
//!
//! ```text
//! audio in → STT (external OS process) → text ──▶ POST /intent  (the EXISTING
//!                                                  fail-closed door: mick.rs
//!                                                  handle_text → LlmBrain::
//!                                                  decide_request →
//!                                                  MickIntent::parse_llm_json)
//! …the governed loop, unchanged…
//! verdict + reason (the #893 narration side-channel) ──▶ TTS (external OS
//!                                                          process) → audio out
//! ```
//!
//! 🔴 **The structural no-bypass guarantee**: this module imports NOTHING from
//! `kirra_planner` (no `MickIntent`, no `Goal`, no plan types) and NOTHING
//! from the checker. Its only output toward the loop is a `String` handed to
//! a text publisher — the same `POST /intent` door typed text uses. A
//! misheard command, a garbled transcription, or ambient noise parsed as
//! words therefore dead-ends exactly where typed garbage does: in
//! `MickIntent::parse_llm_json` returning a parse failure → no latched
//! intent → no motion. There is no path from audio to a goal that does not
//! pass through that parser.
//!
//! **TTS is a pure sink.** [`Speaker`] consumes strings — the intent door's
//! ack/refusal line and the #893 narration sentence (the `explain_deny_token`
//! / trajectory-reason table text, spoken VERBATIM via
//! [`narration_sentence`]) — and renders audio. It exposes no route, accepts
//! no input from the loop's consumers, and returns nothing the loop reads.
//!
//! STT/TTS engines are **external OS processes** (whisper.cpp, Piper), like
//! `taj_service` is a sidecar — never crate dependencies of the safety
//! workspace. This module only spawns and pipes them.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// External STT command (env). Convention: the command receives the WAV path
/// APPENDED as its last argument and prints the transcript to stdout — e.g.
/// `whisper-cli -m models/ggml-base.en.bin -np -nt -f` (whisper.cpp). Unset →
/// the speech shell refuses to start (it has no purpose without an ear).
pub const STT_CMD_ENV: &str = "KIRRA_STT_CMD";

/// External TTS command (env). Convention: the command receives the text to
/// speak on STDIN and renders/plays it — e.g. a one-line wrapper around
/// `piper --model en_US-….onnx --output-raw | aplay -r 22050 -f S16_LE`.
/// Unset → narration is PRINTED, not spoken (the sink degrades to stdout;
/// nothing else changes).
pub const TTS_CMD_ENV: &str = "KIRRA_TTS_CMD";

/// External push-to-talk record command (env). Convention: the command
/// receives the output WAV path appended as its last argument and records a
/// bounded clip — e.g. `arecord -d 4 -f S16_LE -r 16000 -c 1` (the `-d`
/// bound is the push-to-talk discipline: no always-on mic). Unset → the
/// shell runs in `--wav <file>` mode only.
pub const RECORD_CMD_ENV: &str = "KIRRA_RECORD_CMD";

/// Parse an external-command env value into (program, args). Fail-closed on
/// a SET-but-blank value (the env_config convention: malformed config is a
/// startup error, never a silent default); `None` in → `None` out (feature
/// off / caller decides).
pub fn parse_cmd(
    env_key: &str,
    raw: Option<&str>,
) -> Result<Option<(String, Vec<String>)>, String> {
    match raw {
        None => Ok(None),
        Some(s) => {
            let mut parts = s.split_whitespace().map(str::to_string);
            match parts.next() {
                Some(program) => Ok(Some((program, parts.collect()))),
                None => Err(format!(
                    "{env_key}: set but blank — refusing to start (unset it or give a command)"
                )),
            }
        }
    }
}

/// Minimal WAV facts, from a fail-closed header parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub data_len: u32,
}

/// Fail-closed validation of a WAV byte stream BEFORE it is handed to the
/// external STT process: RIFF/WAVE framing, a PCM (1) or IEEE-float (3)
/// `fmt ` chunk, sane channel/rate fields, and a non-empty `data` chunk.
/// Anything else — a truncated file, a random blob, an empty recording — is
/// refused here, producing NO transcript and therefore no intent-door call.
pub fn validate_wav(bytes: &[u8]) -> Result<WavInfo, &'static str> {
    if bytes.len() < 44 {
        return Err("SPEECH_WAV_TRUNCATED");
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("SPEECH_WAV_NOT_RIFF_WAVE");
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32)> = None; // (audio_format, channels, sample_rate)
    let mut data_len: Option<u32> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body = pos + 8;
        match id {
            b"fmt " => {
                if body + 8 > bytes.len() || len < 16 {
                    return Err("SPEECH_WAV_BAD_FMT");
                }
                let audio_format = u16::from_le_bytes([bytes[body], bytes[body + 1]]);
                let channels = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]);
                let sample_rate = u32::from_le_bytes([
                    bytes[body + 4],
                    bytes[body + 5],
                    bytes[body + 6],
                    bytes[body + 7],
                ]);
                fmt = Some((audio_format, channels, sample_rate));
            }
            b"data" => {
                data_len = Some(len as u32);
            }
            _ => {}
        }
        // Chunks are word-aligned (pad byte on odd lengths).
        pos = body + len + (len & 1);
    }
    let (audio_format, channels, sample_rate) = fmt.ok_or("SPEECH_WAV_NO_FMT")?;
    if audio_format != 1 && audio_format != 3 {
        return Err("SPEECH_WAV_NOT_PCM");
    }
    if channels == 0 || sample_rate == 0 {
        return Err("SPEECH_WAV_BAD_FMT");
    }
    let data_len = data_len.ok_or("SPEECH_WAV_NO_DATA")?;
    if data_len == 0 {
        return Err("SPEECH_WAV_EMPTY_DATA");
    }
    Ok(WavInfo {
        channels,
        sample_rate,
        data_len,
    })
}

/// The STT seam: WAV file → transcript text. The production impl is
/// [`ProcessTranscriber`] (an external whisper.cpp-class OS process); tests
/// use a scripted stand-in — the same pattern as `kirra-mick`'s
/// `MockModel`-vs-live-Ollama split.
pub trait Transcriber {
    fn transcribe(&self, wav_path: &Path) -> Result<String, String>;
}

/// STT as an external OS process: `program args… <wav_path>` → stdout.
pub struct ProcessTranscriber {
    program: String,
    args: Vec<String>,
}

impl ProcessTranscriber {
    #[must_use]
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
        }
    }
}

impl Transcriber for ProcessTranscriber {
    fn transcribe(&self, wav_path: &Path) -> Result<String, String> {
        let out = Command::new(&self.program)
            .args(&self.args)
            .arg(wav_path)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("SPEECH_STT_SPAWN_FAILED: {} ({e})", self.program))?;
        if !out.status.success() {
            return Err(format!("SPEECH_STT_EXIT: {}", out.status));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// The TTS sink: text → audio (or stdout). Write-only by construction —
/// `speak` returns no data the loop could consume.
pub trait Speaker {
    fn speak(&mut self, text: &str) -> Result<(), String>;
}

/// TTS as an external OS process: the text is written to the child's STDIN.
pub struct ProcessSpeaker {
    program: String,
    args: Vec<String>,
}

impl ProcessSpeaker {
    #[must_use]
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
        }
    }
}

impl Speaker for ProcessSpeaker {
    fn speak(&mut self, text: &str) -> Result<(), String> {
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("SPEECH_TTS_SPAWN_FAILED: {} ({e})", self.program))?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(text.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .map_err(|e| format!("SPEECH_TTS_WRITE_FAILED: {e}"))?;
        }
        drop(child.stdin.take());
        let status = child
            .wait()
            .map_err(|e| format!("SPEECH_TTS_WAIT_FAILED: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("SPEECH_TTS_EXIT: {status}"))
        }
    }
}

/// The degraded sink when `KIRRA_TTS_CMD` is unset: narration goes to stdout.
pub struct PrintSpeaker;

impl Speaker for PrintSpeaker {
    fn speak(&mut self, text: &str) -> Result<(), String> {
        println!("🔊 {text}");
        Ok(())
    }
}

/// One speech turn's outcome, for the caller's logging/tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechTurn {
    /// The clip validated but transcribed to nothing (silence / pure noise):
    /// nothing was published — the intent door was never called.
    NothingHeard,
    /// The transcript was handed to the intent door, which ACCEPTED it (the
    /// fail-closed parse produced a typed intent). Carries the door's ack.
    Published { transcript: String, ack: String },
    /// The transcript was handed to the intent door, which REFUSED it (parse
    /// failure / rate limit / bad context). Fail-closed: no intent latched,
    /// no motion — identical to typed garbage. Carries the door's error.
    Refused { transcript: String, error: String },
}

/// Drive one turn: WAV file → validate → external STT → transcript →
/// `publish_text` (the caller's binding to the EXISTING `POST /intent`
/// door — nothing else). This function never constructs an intent, a goal,
/// or a command; its only loop-facing act is handing `&str` to the closure.
///
/// `publish_text` contract: `Ok(ack_body)` when the door accepted (200),
/// `Err(error_body)` when it refused (4xx) — transport failures are also
/// `Err` (fail-closed: indistinguishable from refusal, nothing moves).
pub fn speech_turn(
    transcriber: &dyn Transcriber,
    wav_path: &Path,
    publish_text: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<SpeechTurn, String> {
    let bytes = std::fs::read(wav_path).map_err(|e| format!("SPEECH_WAV_READ: {e}"))?;
    validate_wav(&bytes).map_err(str::to_string)?;
    let transcript = transcriber.transcribe(wav_path)?;
    if transcript.trim().is_empty() {
        return Ok(SpeechTurn::NothingHeard);
    }
    match publish_text(transcript.trim()) {
        Ok(ack) => Ok(SpeechTurn::Published {
            transcript: transcript.trim().to_string(),
            ack,
        }),
        Err(error) => Ok(SpeechTurn::Refused {
            transcript: transcript.trim().to_string(),
            error,
        }),
    }
}

/// Mick's own spoken line for a turn outcome — UX strings only, NOT safety
/// narration (the safety sentence is the #893 table text spoken verbatim by
/// [`narration_sentence`]). The refusal line never speculates about what was
/// meant: fail-closed means holding, and saying so.
#[must_use]
pub fn utterance_for(turn: &SpeechTurn) -> String {
    match turn {
        SpeechTurn::NothingHeard => "I didn't catch anything.".to_string(),
        SpeechTurn::Published { transcript, .. } => {
            format!("Heard: \"{transcript}\". Proposing it to the planner — the governor has the final word.")
        }
        SpeechTurn::Refused { transcript, .. } => {
            format!("Heard: \"{transcript}\", but I couldn't turn that into a safe instruction. Holding.")
        }
    }
}

/// Render the #893 narration relay body (`GET /narration/last` →
/// `{"last": null | {at_ms, action, deny_code, explanation}}`) as the spoken
/// sentence. The `explanation` string — the reviewed `explain_deny_token` /
/// trajectory-reason table text — is spoken VERBATIM; this function composes
/// framing around it and generates no new safety-relevant text. A body that
/// is not that shape fails closed to a said-so ("nothing to narrate" is
/// never fabricated from an unrecognized payload).
#[must_use]
pub fn narration_sentence(body: &serde_json::Value) -> String {
    let Some(last) = body.get("last") else {
        return "The narration channel returned something I don't recognize.".to_string();
    };
    if last.is_null() {
        return "No actuator command has been judged since the verifier started.".to_string();
    }
    let action = last.get("action").and_then(|v| v.as_str()).unwrap_or("?");
    match (
        last.get("deny_code").and_then(|v| v.as_str()),
        last.get("explanation").and_then(|v| v.as_str()),
    ) {
        (Some(code), Some(sentence)) => {
            format!("The governor's last verdict was {action} ({code}). {sentence}")
        }
        (Some(code), None) => format!("The governor's last verdict was {action} ({code})."),
        _ => format!("The governor's last command verdict was {action}."),
    }
}

/// Serialize PCM16 mono samples as a minimal valid WAV byte stream. Used to
/// GENERATE the deterministic audio fixtures the CI tests transcribe-from
/// (no binary blob committed; the fixture is byte-reproducible from code) —
/// and handy for any harness that needs a synthetic clip. Pure encoder; it
/// never touches audio hardware.
#[must_use]
pub fn pcm16_wav_bytes(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits/sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_bytes(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        pcm16_wav_bytes(sample_rate, samples)
    }

    #[test]
    fn wav_validation_accepts_a_real_pcm_clip() {
        let bytes = wav_bytes(16_000, &[0, 1000, -1000, 500]);
        let info = validate_wav(&bytes).expect("valid PCM WAV");
        assert_eq!(
            info,
            WavInfo {
                channels: 1,
                sample_rate: 16_000,
                data_len: 8
            }
        );
    }

    /// 🔴 Fail-closed at the first gate: garbage audio input produces NO
    /// transcript and therefore no intent-door call at all.
    #[test]
    fn wav_validation_refuses_garbage_fail_closed() {
        for (bytes, why) in [
            (Vec::new(), "empty file"),
            (vec![0u8; 20], "truncated"),
            (
                b"NOT A WAVE FILE AT ALL......................".to_vec(),
                "not RIFF",
            ),
            (wav_bytes(16_000, &[]), "empty data chunk"),
            (wav_bytes(0, &[1, 2]), "zero sample rate"),
        ] {
            assert!(validate_wav(&bytes).is_err(), "{why} must be refused");
        }
        // A non-PCM format tag is refused too.
        let mut bytes = wav_bytes(16_000, &[1, 2]);
        bytes[20] = 7; // audio_format = 7 (µ-law)
        assert_eq!(validate_wav(&bytes), Err("SPEECH_WAV_NOT_PCM"));
    }

    #[test]
    fn parse_cmd_is_fail_closed_on_blank_and_off_on_unset() {
        assert_eq!(parse_cmd("K", None), Ok(None));
        let (prog, args) = parse_cmd("K", Some("whisper-cli -m model.bin -f"))
            .unwrap()
            .unwrap();
        assert_eq!(prog, "whisper-cli");
        assert_eq!(args, vec!["-m", "model.bin", "-f"]);
        assert!(parse_cmd("K", Some("   ")).is_err(), "blank-but-set aborts");
    }

    struct Scripted(&'static str);
    impl Transcriber for Scripted {
        fn transcribe(&self, _wav: &Path) -> Result<String, String> {
            Ok(self.0.to_string())
        }
    }

    fn temp_wav(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("kirra_speech_test_{name}.wav"));
        std::fs::write(&p, wav_bytes(16_000, &[0, 200, -200, 100])).expect("write wav");
        p
    }

    /// The no-bypass property, at this layer: an empty transcription ends the
    /// turn BEFORE the publish closure — and a non-empty one reaches the loop
    /// ONLY as text through that closure.
    #[test]
    fn silence_publishes_nothing_and_text_goes_only_through_the_door() {
        let wav = temp_wav("silence");
        let mut called = 0u32;
        let mut publish = |_t: &str| -> Result<String, String> {
            called += 1;
            Ok("{\"ok\":true}".into())
        };
        let turn = speech_turn(&Scripted("   "), &wav, &mut publish).unwrap();
        assert_eq!(turn, SpeechTurn::NothingHeard);
        assert_eq!(called, 0, "silence must never call the intent door");

        let mut seen = String::new();
        let mut publish = |t: &str| -> Result<String, String> {
            seen = t.to_string();
            Ok("{\"ok\":true}".into())
        };
        let turn = speech_turn(&Scripted("take me to the dock"), &wav, &mut publish).unwrap();
        assert!(matches!(turn, SpeechTurn::Published { .. }));
        assert_eq!(seen, "take me to the dock");
        let _ = std::fs::remove_file(&wav);
    }

    /// A refused publish (the door's fail-closed parse) is a REFUSED turn —
    /// surfaced, spoken as a hold, never retried into something else.
    #[test]
    fn a_door_refusal_is_a_held_turn() {
        let wav = temp_wav("refusal");
        let mut publish =
            |_t: &str| -> Result<String, String> { Err("MICK_JSON_PARSE_ERROR".into()) };
        let turn = speech_turn(&Scripted("krrrshh mumble"), &wav, &mut publish).unwrap();
        assert!(matches!(turn, SpeechTurn::Refused { .. }));
        let line = utterance_for(&turn);
        assert!(line.contains("Holding"), "{line}");
        let _ = std::fs::remove_file(&wav);
    }

    #[test]
    fn narration_sentence_speaks_the_table_text_verbatim() {
        let body = serde_json::json!({"last": {
            "at_ms": 123, "action": "DenyBreach",
            "deny_code": "INVALID_TIME_DELTA",
            "explanation": "The command reported a zero or negative time step."
        }});
        let line = narration_sentence(&body);
        assert!(
            line.contains("INVALID_TIME_DELTA")
                && line.contains("The command reported a zero or negative time step."),
            "{line}"
        );
        assert_eq!(
            narration_sentence(&serde_json::json!({"last": null})),
            "No actuator command has been judged since the verifier started."
        );
        // Not the endpoint's shape → said so, never fabricated.
        let odd = narration_sentence(&serde_json::json!({"posture": "Nominal"}));
        assert!(odd.contains("don't recognize"), "{odd}");
    }

    /// The process seams, driven by real OS processes (Unix coreutils).
    #[cfg(unix)]
    #[test]
    fn process_transcriber_and_speaker_run_external_commands() {
        let wav = temp_wav("process");
        // `echo hello <wav>` → stdout starts with "hello".
        let t = ProcessTranscriber::new("echo", vec!["hello".into()]);
        let text = t.transcribe(&wav).expect("echo runs");
        assert!(text.starts_with("hello"), "{text}");
        // `cat` consumes stdin and exits 0 — the sink contract.
        let mut s = ProcessSpeaker::new("cat", vec![]);
        assert!(s.speak("narration line").is_ok());
        // A missing binary fails loudly, not silently.
        let missing = ProcessTranscriber::new("definitely-not-a-real-binary-kirra", vec![]);
        assert!(missing.transcribe(&wav).is_err());
        let _ = std::fs::remove_file(&wav);
    }
}
