// judge.rs — the runtime command judge (contract checks), as PLAIN SAFE RUST.
//
// FFI-ELIMINATION DEMONSTRATION: in the C/C++ classic-iceoryx shim the judge was
// reached across an `extern "C"` boundary (raw pointer + `unsafe`). Over a Rust
// iceoryx2 subscriber edge the judge is just an ORDINARY FUNCTION CALL on a typed
// `&CommandFrame` — no `extern "C"`, no `unsafe`, no FFI on the command path.
// That is the architectural point of this spike (see README).
//
// PROXY CONSTANTS: the envelope numbers below are clearly-labelled PROXIES for
// the spike. The CERTIFIED kinematic envelope lives in the untouched talisman
// `src/gateway/kinematics_contract.rs` (`VehicleKinematicsContract`); this spike
// imports NOTHING from it and must never be read as the certified bound.

use crate::wire::{CommandFrame, FRAME_MAGIC};

/// PROXY nominal linear-speed ceiling (m/s). NOT certified — see module note.
/// (Cf. the talisman `VehicleKinematicsContract::max_speed_mps`.)
pub const PROXY_MAX_LINEAR_MPS: f64 = 22.35;
/// PROXY angular-rate ceiling (rad/s). NOT certified — see module note.
/// (Cf. the talisman / parko angular-bound work, KIRRA-OCCY-ANGULAR-SOTIF-001.)
pub const PROXY_MAX_ANGULAR_RADPS: f64 = 1.5;

/// The judge's verdict for a single frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// All contract checks passed — the command is admissible.
    Accept,
    /// Rejected, with the reason.
    Reject(RejectReason),
}

/// Why the judge rejected a frame. Distinct codes so the fault matrix can assert
/// exactly what each injected fault produces (e.g. Replay vs SequenceRegress).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// Header magic mismatch (BadMagic / StaleHeader).
    BadMagic,
    /// `sequence < last_accepted` — an out-of-order / regressed command.
    SequenceRegress,
    /// `sequence == last_accepted` — a REPLAYED command. (The corrected check:
    /// equal sequence rejects. The strict-`<` form was a known replay hole and
    /// is NOT reintroduced here.)
    Replay,
    /// `now > deadline` — the command arrived too late to be actuated.
    DeadlineMissed,
    /// The upstream integrity assertion was not set.
    IntegrityFlag,
    /// The decoded command exceeds the (PROXY) kinematic envelope, or is
    /// non-finite.
    KinematicLimit,
}

/// Per-stream judge state. `last_accepted` is the highest accepted sequence; it
/// gates monotonicity and replay. The `Default` (no command accepted yet) is the
/// correct fresh state.
#[derive(Clone, Copy, Debug, Default)]
pub struct JudgeState {
    pub last_accepted: u64,
    pub have_accepted: bool,
}

impl JudgeState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Judge one frame at time `now_nanos` (monotonic domain). On `Accept`,
    /// advances `last_accepted`. Pure aside from that state update; no unsafe.
    ///
    /// Check order: magic → sequence (monotonic, replay-rejecting) → deadline →
    /// integrity flag → kinematic envelope. (The subscriber EDGE has already done
    /// bounds/oversize + CRC; see `wire::edge_validate`.)
    pub fn judge(&mut self, frame: &CommandFrame, now_nanos: u64) -> Verdict {
        // 1. Header magic.
        if frame.magic != FRAME_MAGIC {
            return Verdict::Reject(RejectReason::BadMagic);
        }

        // 2. Sequence monotonicity + replay. The corrected rule: reject when
        //    `sequence <= last_accepted`. Equal == replay, lower == regress.
        if self.have_accepted && frame.sequence <= self.last_accepted {
            let reason = if frame.sequence == self.last_accepted {
                RejectReason::Replay
            } else {
                RejectReason::SequenceRegress
            };
            return Verdict::Reject(reason);
        }

        // 3. Deadline freshness.
        if now_nanos > frame.deadline_nanos {
            return Verdict::Reject(RejectReason::DeadlineMissed);
        }

        // 4. Upstream integrity assertion.
        if frame.integrity_flag != 1 {
            return Verdict::Reject(RejectReason::IntegrityFlag);
        }

        // 5. Kinematic envelope (PROXY bounds; NaN/Inf fail closed).
        match frame.decode_command() {
            Some((lin, ang)) => {
                if !lin.is_finite()
                    || !ang.is_finite()
                    || lin.abs() > PROXY_MAX_LINEAR_MPS
                    || ang.abs() > PROXY_MAX_ANGULAR_RADPS
                {
                    return Verdict::Reject(RejectReason::KinematicLimit);
                }
            }
            None => return Verdict::Reject(RejectReason::KinematicLimit),
        }

        // All checks passed — accept and advance the sequence gate.
        self.last_accepted = frame.sequence;
        self.have_accepted = true;
        Verdict::Accept
    }
}
