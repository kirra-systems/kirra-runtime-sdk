// crates/kirra-governor-service/src/main.rs
//
// kirra-governor-service — minimal over-the-wire (UDP) KIRRA governor for the
// two-box governed-car prototype (docs/adr/KIRRA_BRINGUP_RUNBOOK.md, Prompt A).
//
// It wraps the EXISTING verdict core — the FROZEN kinematics-contract talisman —
// which now lives in the lean `kirra-core` crate (de-monolith Stage 3). This binary
// depends on that crate directly; because `kirra-core` imports only `serde` + `std`,
// it pulls in nothing heavy: this binary's entire dependency tree is serde + bincode
// + std — NO tokio, ROS 2, r2r, or DDS, per ADR-0032 (the governor is the minimal,
// async/ROS-free checker; the QNX cert target has none of those anyway). The contract
// logic is the real, unmodified one — the talisman is never forked, only relocated.
//
// PROTOTYPE STAGE (QM, not the cert build): regular Rust over UDP. The
// Ferrocene / `no_std` / ASIL-D factoring and the shared-memory mailbox are a
// later stage and do not block the demo — see ADR-0032 and the bring-up runbook.
//
// AUTHENTICATION (review B2): the UDP command path is a vehicle-control surface,
// so it is NOT trusted by source address. Every datagram (both directions) carries
// a 32-byte HMAC-SHA256 tag over its body, keyed by a pre-shared secret
// (`KIRRA_GOVERNOR_PSK`, REQUIRED — absent/empty → the governor refuses to start,
// fail-closed). An unauthenticated request is dropped SILENTLY (no reply), so a
// forged source address cannot turn the governor into a reflector; replies are
// MAC'd too, so a spoofed verdict (e.g. a forged "Allow") cannot be injected at
// the car. The M6 watchdog now REJECTS (rather than only logging) a replayed /
// non-advancing sequence, and — when `KIRRA_GOVERNOR_FRESHNESS_MS` is set —
// rejects stale / future-dated proposals, emitting a fail-closed safe-state verdict.

// hmac 0.13 / digest 0.11: `Mac` brings update/finalize/verify; `new_from_slice`
// now lives on `KeyInit` (it was reachable via `Mac` in hmac 0.12).
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use kirra_core::kinematics_contract::{
    validate_vehicle_command, DenyCode, EnforceAction, ProposedVehicleCommand,
    VehicleKinematicsContract,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::net::UdpSocket;

type HmacSha256 = Hmac<Sha256>;

// bincode 2.x defaults to varint; `legacy()` is the bincode-1.x-compatible
// config (fixint, little-endian, u32 enum tags), keeping the UDP wire bytes
// byte-identical across the 1.x -> 2.x upgrade. The client mirror
// (`kirra-wire-client`) uses the same config; its `wire_layout` tests pin the
// exact bytes both sides must agree on.
#[inline]
fn wire_cfg() -> impl bincode::config::Config {
    bincode::config::legacy()
}

/// Length of the HMAC-SHA256 authentication tag prepended to every datagram.
const MAC_LEN: usize = 32;

/// Default listen address (override with `KIRRA_GOVERNOR_ADDR`).
const DEFAULT_ADDR: &str = "0.0.0.0:9760";

/// Wire request, car -> governor. Carries ONLY the proposed command: the safety
/// envelope (`VehicleKinematicsContract`) is the GOVERNOR's policy, never the
/// doer's to choose. `ProposedVehicleCommand` is the verdict core's real input
/// type (serde-(de)serializable in the core) — no kinematic fields are invented
/// here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Monotonic request sequence (echoed in the verdict; basis for the M6 watchdog).
    pub seq: u64,
    /// Car-side send timestamp (nanoseconds). Used for staleness once the M6
    /// watchdog is wired; carried now so the schema is stable.
    pub ts_nanos: u128,
    /// The proposed command — the verdict core's real input type, verbatim.
    pub command: ProposedVehicleCommand,
}

/// Wire response, governor -> car. `action` is the verdict core's own output
/// (`EnforceAction`, Serialize-only in the core, which is all a one-way encode
/// needs). `reason_code` is a stable numeric a car can branch on WITHOUT
/// deserializing `EnforceAction`.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub seq: u64,
    pub action: EnforceAction,
    pub reason_code: u32,
}

/// Stable numeric reason code: `0` = no breach (Accept / Clamp); `1..=11` map
/// 1:1 to `DenyCode`. Kept as an explicit match so adding a `DenyCode` variant
/// upstream forces a compile error here (no silent gap).
fn reason_code(action: &EnforceAction) -> u32 {
    match action {
        EnforceAction::Allow
        | EnforceAction::ClampLinear(_)
        | EnforceAction::ClampSteering(_)
        | EnforceAction::ClampBoth { .. } => 0,
        EnforceAction::DenyBreach(code) => deny_code_num(*code),
    }
}

fn deny_code_num(code: DenyCode) -> u32 {
    match code {
        DenyCode::NanInfLinearVelocity => 1,
        DenyCode::NanInfCurrentVelocity => 2,
        DenyCode::NanInfSteeringAngle => 3,
        DenyCode::NanInfCurrentSteering => 4,
        DenyCode::NanInfDeltaTime => 5,
        DenyCode::InvalidTimeDelta => 6,
        DenyCode::AssetLockedOut => 7,
        DenyCode::DrivableSpaceDeparture => 8,
        DenyCode::DegradedReinitiationDenied => 9,
        DenyCode::DegradedSpeedIncreaseDenied => 10,
        DenyCode::FrameIntegrityUntrusted => 11,
        DenyCode::TrajectoryHorizonExceeded => 12,
    }
}

/// The decision: run the verdict core VERBATIM against the governor's contract.
/// Pure (no I/O) so it is unit-testable without a socket.
fn decide(proposal: &Proposal, contract: &VehicleKinematicsContract) -> Verdict {
    let action = validate_vehicle_command(&proposal.command, contract);
    let reason_code = reason_code(&action);
    Verdict {
        seq: proposal.seq,
        action,
        reason_code,
    }
}

/// Compute the HMAC-SHA256 tag over `body` with the pre-shared key. HMAC accepts
/// a key of any length, so this never fails for a non-empty key.
fn compute_mac(psk: &[u8], body: &[u8]) -> [u8; MAC_LEN] {
    let mut mac = HmacSha256::new_from_slice(psk).expect("HMAC accepts any key length");
    mac.update(body);
    let out = mac.finalize().into_bytes();
    let mut tag = [0u8; MAC_LEN];
    tag.copy_from_slice(&out);
    tag
}

/// Constant-time verification that `tag` authenticates `body` under `psk`.
/// `Mac::verify_slice` is constant-time; an empty key (which should be impossible
/// past the startup check) fails closed.
fn verify_mac(psk: &[u8], body: &[u8], tag: &[u8]) -> bool {
    let mut mac = match HmacSha256::new_from_slice(psk) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    mac.verify_slice(tag).is_ok()
}

/// Frame the wire response: `tag(32) || bincode(verdict)`, so the car can
/// authenticate the verdict and reject a spoofed one.
fn frame_response(psk: &[u8], verdict: &Verdict) -> Result<Vec<u8>, bincode::error::EncodeError> {
    let body = bincode::serde::encode_to_vec(verdict, wire_cfg())?;
    let tag = compute_mac(psk, &body);
    let mut out = Vec::with_capacity(MAC_LEN + body.len());
    out.extend_from_slice(&tag);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Outcome of the M6 freshness/replay watchdog for one proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchdogVerdict {
    /// Fresh and sequence-advancing — safe to evaluate.
    Fresh,
    /// Sequence did not strictly advance — a reordered / replayed proposal.
    Replay,
    /// `ts_nanos` is older than the freshness window — a delayed / held command.
    Stale,
    /// `ts_nanos` is ahead of now beyond the window — clock skew / forgery.
    FutureDated,
}

impl WatchdogVerdict {
    fn as_str(self) -> &'static str {
        match self {
            WatchdogVerdict::Fresh => "FRESH",
            WatchdogVerdict::Replay => "REPLAY",
            WatchdogVerdict::Stale => "STALE",
            WatchdogVerdict::FutureDated => "FUTURE_DATED",
        }
    }
}

/// M6 watchdog state. Sequence-replay rejection is ALWAYS active (it needs no
/// clock). Absolute-time staleness is opt-in via `freshness_window_nanos`
/// (`KIRRA_GOVERNOR_FRESHNESS_MS`) because it requires the car and governor
/// clocks to be roughly synchronized (AOU-TIMESYNC-001); when unset, only the
/// clock-free replay check runs.
struct WatchdogState {
    last_seq: Option<u64>,
    freshness_window_nanos: Option<u128>,
}

impl WatchdogState {
    fn new(freshness_window_nanos: Option<u128>) -> Self {
        Self {
            last_seq: None,
            freshness_window_nanos,
        }
    }

    /// Classify a proposal against the replay and freshness rules. Advances the
    /// stored sequence ONLY on `Fresh`, so a rejected (replayed/stale) proposal
    /// can neither poison the high-water mark nor be laundered into acceptance.
    fn observe(&mut self, proposal: &Proposal, now_nanos: u128) -> WatchdogVerdict {
        // Replay: the sequence must strictly advance. The MAC already prevents an
        // attacker forging a NEW high sequence, so monotonic-seq + MAC together
        // reject both forged and captured-and-replayed datagrams.
        if let Some(prev) = self.last_seq {
            if proposal.seq <= prev {
                return WatchdogVerdict::Replay;
            }
        }

        // Freshness (opt-in; needs car/governor clock sync per AOU-TIMESYNC-001).
        if let Some(window) = self.freshness_window_nanos {
            if proposal.ts_nanos > now_nanos {
                if proposal.ts_nanos - now_nanos > window {
                    return WatchdogVerdict::FutureDated;
                }
            } else if now_nanos - proposal.ts_nanos > window {
                return WatchdogVerdict::Stale;
            }
        }

        self.last_seq = Some(proposal.seq);
        WatchdogVerdict::Fresh
    }
}

/// Current wall-clock time in nanoseconds since the UNIX epoch (read once per
/// request in `main` and passed into the pure watchdog for testability).
fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// The fail-closed safe-state verdict emitted for an authenticated-but-untrustworthy
/// frame (replay / stale / future-dated): a `FrameIntegrityUntrusted` deny, so the
/// car safe-stops rather than acting on the questionable command.
fn safe_state_verdict(seq: u64) -> Verdict {
    Verdict {
        seq,
        action: EnforceAction::DenyBreach(DenyCode::FrameIntegrityUntrusted),
        reason_code: deny_code_num(DenyCode::FrameIntegrityUntrusted),
    }
}

fn main() -> std::io::Result<()> {
    let addr = std::env::var("KIRRA_GOVERNOR_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());

    // The UDP command path MUST be authenticated (review B2). No PSK → refuse to
    // start, fail-closed (an unauthenticated vehicle-control surface is the bug).
    let psk = std::env::var("KIRRA_GOVERNOR_PSK").unwrap_or_default();
    if psk.trim().is_empty() {
        eprintln!(
            "FATAL: KIRRA_GOVERNOR_PSK is unset/empty — the UDP command path must be \
             authenticated (HMAC-SHA256). Refusing to start fail-open."
        );
        std::process::exit(1);
    }
    let psk = psk.into_bytes();

    // Optional absolute-time staleness window (AOU-TIMESYNC-001: requires the car
    // and governor clocks to be roughly synchronized). Unset → only the clock-free
    // sequence-replay check runs.
    let freshness_window_nanos = std::env::var("KIRRA_GOVERNOR_FRESHNESS_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|ms| ms as u128 * 1_000_000);

    // The governor enforces its OWN envelope. Nominal reference profile for the
    // prototype; the MRC fallback profile is available for a degraded mode.
    let contract = VehicleKinematicsContract::nominal_reference_profile();

    let socket = UdpSocket::bind(&addr)?;
    eprintln!(
        "kirra-governor-service: listening on {addr} (UDP, HMAC-SHA256 authenticated), \
         contract = nominal_reference_profile, effective_max_speed = {:.2} m/s, \
         freshness_window = {}",
        contract.effective_max_speed_mps(),
        freshness_window_nanos
            .map(|w| format!("{} ms", w / 1_000_000))
            .unwrap_or_else(|| "disabled (seq-replay only)".to_string()),
    );

    let mut watchdog = WatchdogState::new(freshness_window_nanos);
    // One UDP datagram per proposal; 64 KiB is far above the fixed-schema size.
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let (n, peer) = socket.recv_from(&mut buf)?;
        let frame = &buf[..n];

        // Authenticate first: frame = tag(32) || bincode(Proposal). A frame too
        // short to carry a tag, or one whose tag does not verify, is dropped
        // SILENTLY — no reply to a forged source (anti-reflection).
        if frame.len() < MAC_LEN {
            eprintln!("drop: short frame ({} bytes) from {peer}", frame.len());
            continue;
        }
        let (tag, body) = frame.split_at(MAC_LEN);
        if !verify_mac(&psk, body, tag) {
            eprintln!("drop: unauthenticated datagram from {peer} (bad MAC; no reply)");
            continue;
        }

        // Strict framing: the decode MUST consume the whole authenticated body.
        // A prefix that decodes while leaving trailing bytes is a malformed frame
        // and is dropped fail-closed (never evaluated as a clean proposal).
        let proposal: Proposal = match bincode::serde::decode_from_slice(body, wire_cfg()) {
            Ok((p, len)) if len == body.len() => p,
            Ok((_, len)) => {
                eprintln!(
                    "decode error from {peer}: trailing bytes ({} of {} consumed)",
                    len,
                    body.len()
                );
                continue;
            }
            Err(e) => {
                eprintln!("decode error from {peer}: {e}");
                continue;
            }
        };

        // Authenticated peer: classify freshness/replay, then either evaluate or
        // emit the fail-closed safe-state deny.
        let verdict = match watchdog.observe(&proposal, now_nanos()) {
            WatchdogVerdict::Fresh => decide(&proposal, &contract),
            rejected => {
                eprintln!(
                    "watchdog {} seq {} from {peer} -> safe-state deny",
                    rejected.as_str(),
                    proposal.seq
                );
                safe_state_verdict(proposal.seq)
            }
        };

        match frame_response(&psk, &verdict) {
            Ok(bytes) => {
                if let Err(e) = socket.send_to(&bytes, peer) {
                    eprintln!("send error to {peer}: {e}");
                }
            }
            Err(e) => eprintln!("encode error for seq {}: {e}", verdict.seq),
        }
    }
}

#[cfg(test)]
mod service_tests {
    use super::*;

    fn steady(linear: f64, steering: f64) -> Proposal {
        Proposal {
            seq: 1,
            ts_nanos: 0,
            command: ProposedVehicleCommand {
                linear_velocity_mps: linear,
                current_velocity_mps: linear,
                delta_time_s: 0.1,
                steering_angle_deg: steering,
                current_steering_angle_deg: steering,
            },
        }
    }

    #[test]
    fn steady_low_speed_command_is_accepted() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let v = decide(&steady(1.0, 0.0), &contract);
        assert_eq!(
            v.action,
            EnforceAction::Allow,
            "a steady 1 m/s straight command must pass"
        );
        assert_eq!(v.reason_code, 0);
        assert_eq!(v.seq, 1);
    }

    #[test]
    fn nan_linear_velocity_is_denied_with_code_1() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let v = decide(&steady(f64::NAN, 0.0), &contract);
        assert_eq!(
            v.action,
            EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity),
            "NaN linear velocity must be denied by the verdict core verbatim"
        );
        assert_eq!(v.reason_code, 1, "NaN-linear maps to reason_code 1");
    }

    #[test]
    fn verdict_echoes_request_seq() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut p = steady(1.0, 0.0);
        p.seq = 42;
        assert_eq!(decide(&p, &contract).seq, 42);
    }

    #[test]
    fn proposal_round_trips_over_bincode() {
        // The decode side of the wire (Proposal: Deserialize) must round-trip.
        let p = steady(2.5, 3.0);
        let bytes = bincode::serde::encode_to_vec(&p, wire_cfg()).expect("encode");
        let back: Proposal = bincode::serde::decode_from_slice(&bytes, wire_cfg())
            .map(|(v, _)| v)
            .expect("decode");
        assert_eq!(back.seq, p.seq);
        assert_eq!(
            back.command.linear_velocity_mps,
            p.command.linear_velocity_mps
        );
        assert_eq!(
            back.command.steering_angle_deg,
            p.command.steering_angle_deg
        );
    }

    #[test]
    fn verdict_serializes_for_the_wire() {
        // The encode side of the wire (Verdict: Serialize) must succeed for both
        // an accept and a deny.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let accept = decide(&steady(1.0, 0.0), &contract);
        let deny = decide(&steady(f64::INFINITY, 0.0), &contract);
        assert!(bincode::serde::encode_to_vec(&accept, wire_cfg()).is_ok());
        assert!(bincode::serde::encode_to_vec(&deny, wire_cfg()).is_ok());
    }

    #[test]
    fn watchdog_rejects_replayed_seq() {
        // No freshness window → only the clock-free replay check is active.
        let mut wd = WatchdogState::new(None);
        let mut p = steady(1.0, 0.0);
        p.seq = 5;
        assert_eq!(
            wd.observe(&p, 0),
            WatchdogVerdict::Fresh,
            "first observation is fresh"
        );
        p.seq = 4;
        assert_eq!(
            wd.observe(&p, 0),
            WatchdogVerdict::Replay,
            "a lower seq is a replay and must be rejected"
        );
        p.seq = 5;
        assert_eq!(
            wd.observe(&p, 0),
            WatchdogVerdict::Replay,
            "an equal seq (re-sent datagram) is also a replay"
        );
        p.seq = 6;
        assert_eq!(
            wd.observe(&p, 0),
            WatchdogVerdict::Fresh,
            "an advancing seq is fresh again"
        );
    }

    #[test]
    fn watchdog_rejected_proposal_does_not_advance_highwater() {
        // A replayed proposal must not poison the sequence high-water mark.
        let mut wd = WatchdogState::new(None);
        let mut p = steady(1.0, 0.0);
        p.seq = 10;
        assert_eq!(wd.observe(&p, 0), WatchdogVerdict::Fresh);
        p.seq = 3;
        assert_eq!(wd.observe(&p, 0), WatchdogVerdict::Replay);
        // The high-water mark is still 10, so seq 11 is the next acceptable one.
        p.seq = 11;
        assert_eq!(wd.observe(&p, 0), WatchdogVerdict::Fresh);
    }

    #[test]
    fn watchdog_freshness_window_rejects_stale_and_future() {
        // 100 ms window, expressed in nanoseconds.
        let window_ns: u128 = 100 * 1_000_000;
        let now: u128 = 10_000_000_000; // arbitrary "now"
        let mut wd = WatchdogState::new(Some(window_ns));

        // Within window → fresh.
        let mut p = steady(1.0, 0.0);
        p.seq = 1;
        p.ts_nanos = now - 50 * 1_000_000;
        assert_eq!(wd.observe(&p, now), WatchdogVerdict::Fresh);

        // Older than the window → stale (seq still advances so it is not a replay).
        p.seq = 2;
        p.ts_nanos = now - 250 * 1_000_000;
        assert_eq!(wd.observe(&p, now), WatchdogVerdict::Stale);

        // Future-dated beyond the window → forgery/skew rejection.
        p.seq = 3;
        p.ts_nanos = now + 250 * 1_000_000;
        assert_eq!(wd.observe(&p, now), WatchdogVerdict::FutureDated);
    }

    #[test]
    fn watchdog_freshness_disabled_ignores_timestamps() {
        // No window → an ancient timestamp is accepted (replay check still gates seq).
        let mut wd = WatchdogState::new(None);
        let mut p = steady(1.0, 0.0);
        p.seq = 1;
        p.ts_nanos = 0;
        assert_eq!(wd.observe(&p, 10_000_000_000), WatchdogVerdict::Fresh);
    }

    #[test]
    fn mac_round_trips_and_rejects_tamper() {
        let psk = b"shared-bench-secret";
        let body = bincode::serde::encode_to_vec(steady(1.0, 0.0), wire_cfg()).expect("encode");
        let tag = compute_mac(psk, &body);
        assert!(verify_mac(psk, &body, &tag), "a valid tag must verify");

        // Wrong key → reject.
        assert!(
            !verify_mac(b"other-key", &body, &tag),
            "a tag under another key must fail"
        );

        // Tampered body → reject.
        let mut tampered = body.clone();
        tampered[0] ^= 0xFF;
        assert!(
            !verify_mac(psk, &tampered, &tag),
            "a tampered body must fail authentication"
        );

        // Tampered tag → reject.
        let mut bad_tag = tag;
        bad_tag[0] ^= 0xFF;
        assert!(
            !verify_mac(psk, &body, &bad_tag),
            "a tampered tag must fail authentication"
        );
    }

    #[test]
    fn framed_response_is_authenticated_and_decodes() {
        let psk = b"shared-bench-secret";
        let verdict = safe_state_verdict(7);
        let framed = frame_response(psk, &verdict).expect("frame");
        assert!(framed.len() > MAC_LEN, "framed response carries tag + body");
        let (tag, body) = framed.split_at(MAC_LEN);
        assert!(
            verify_mac(psk, body, tag),
            "the car must be able to authenticate the verdict"
        );
        // The body is the serialized verdict; reason_code is FrameIntegrityUntrusted (11).
        assert_eq!(verdict.reason_code, 11);
    }
}
