//! Snapshot validation (HVCHAN-001 §3 step 4) and the fail-closed fault taxonomy
//! (§4). Runs on the governor's **local owned snapshot**, never the live region.
//!
//! Check order is fixed (§2.2 + §3 step 4): `layout_version` → `magic` → bounds
//! → CRC → `sequence` monotonic → `generation` monotonic → deadline. The first
//! failing check returns its [`ContractFault`]; there is no fall-through accept.
//!
//! This crate owns the **contract-discipline** and **judge** rows of the §4
//! table. The **hypervisor** rows (read-only mapping R-HV-1, clock-skew bound,
//! publisher-silent liveness) are enforced outside this crate; in particular the
//! caller MUST pass a `now_nanos` already in the **boundary clock domain**
//! (R-HV-3) — this path never reads wall/PTP time, and a cross-domain timestamp
//! is the integrator's `AOU-TIMESYNC-001` obligation, not a check here.

use crate::crc::crc32_ieee;
use crate::view::{GovernorContractView, LAYOUT_VERSION, MAGIC, MAX_COMMAND_BYTES};

/// A fail-closed validation failure (HVCHAN-001 §4). Every variant is a reject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContractFault {
    /// `layout_version` is not one this build was certified against. Checked
    /// FIRST; the snapshot is **not** parsed further. (contract-discipline)
    LayoutVersionMismatch { found: u32, expected: u32 },
    /// Channel sentinel wrong — gross corruption / wrong region. (contract-discipline)
    MagicMismatch { found: u32 },
    /// `command_len` exceeds [`MAX_COMMAND_BYTES`]. (contract-discipline)
    CommandLenOversize { found: u32, max: u32 },
    /// CRC over `command[..command_len]` does not match `crc32`. (contract-discipline)
    CrcMismatch { found: u32, computed: u32 },
    /// `sequence <= last_accepted` — replay (equal) or regress (lower). (judge)
    SequenceRegressOrReplay { found: u64, last_accepted: u64 },
    /// `generation <= last_accepted` — replay/regress of the seqlock counter. (judge)
    GenerationRegressOrReplay { found: u64, last_accepted: u64 },
    /// `now_nanos > deadline_nanos` (boundary clock domain). (judge)
    DeadlineExpired { now: u64, deadline: u64 },
}

/// The monotonic high-water mark: the `(generation, sequence)` of the last
/// **accepted** command. Both advance only on a Fresh (accepted) verdict, so a
/// rejected snapshot can never poison it (HVCHAN-001 §3.1; the #273 spike rule).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AcceptedWatermark {
    last_generation: u64,
    last_sequence: u64,
    initialized: bool,
}

impl AcceptedWatermark {
    /// A fresh watermark with nothing accepted yet (the first valid command of
    /// any `(generation, sequence)` is admissible).
    pub const fn new() -> Self {
        Self { last_generation: 0, last_sequence: 0, initialized: false }
    }

    /// Record `view` as the newest accepted command. Call this **only** after
    /// [`validate`] returns `Ok` for the same view.
    pub fn record(&mut self, view: &GovernorContractView) {
        self.last_generation = view.generation;
        self.last_sequence = view.sequence;
        self.initialized = true;
    }

    /// The last accepted `(generation, sequence)`, or `None` if nothing has been
    /// accepted yet.
    pub fn last(&self) -> Option<(u64, u64)> {
        if self.initialized {
            Some((self.last_generation, self.last_sequence))
        } else {
            None
        }
    }
}

/// Validate a coherent local snapshot against the contract and the monotonic
/// watermark. `now_nanos` MUST be in the boundary clock domain (R-HV-3).
///
/// Returns `Ok(())` iff every check passes; otherwise the first [`ContractFault`].
/// On `Ok`, the caller advances the watermark via [`AcceptedWatermark::record`]
/// and proceeds to digest + sign (HVCHAN-001 §3 steps 5-6).
pub fn validate(
    view: &GovernorContractView,
    now_nanos: u64,
    watermark: &AcceptedWatermark,
) -> Result<(), ContractFault> {
    // 1. Version prefix — checked before any other field is interpreted (§2.2).
    if view.layout_version != LAYOUT_VERSION {
        return Err(ContractFault::LayoutVersionMismatch {
            found: view.layout_version,
            expected: LAYOUT_VERSION,
        });
    }
    // 2. Sentinel.
    if view.magic != MAGIC {
        return Err(ContractFault::MagicMismatch { found: view.magic });
    }
    // 3. Bounds — before the CRC reads `command[..len]`.
    let len = view.command_len as usize;
    if len > MAX_COMMAND_BYTES {
        return Err(ContractFault::CommandLenOversize {
            found: view.command_len,
            max: MAX_COMMAND_BYTES as u32,
        });
    }
    // 4. Integrity.
    let computed = crc32_ieee(&view.command[..len]);
    if view.crc32 != computed {
        return Err(ContractFault::CrcMismatch { found: view.crc32, computed });
    }
    // 5/6. Monotonic generation + sequence (`<= last_accepted ⇒ reject`; §3.1).
    if let Some((last_gen, last_seq)) = watermark.last() {
        if view.sequence <= last_seq {
            return Err(ContractFault::SequenceRegressOrReplay {
                found: view.sequence,
                last_accepted: last_seq,
            });
        }
        if view.generation <= last_gen {
            return Err(ContractFault::GenerationRegressOrReplay {
                found: view.generation,
                last_accepted: last_gen,
            });
        }
    }
    // 7. Freshness against the absolute deadline (boundary domain).
    if now_nanos > view.deadline_nanos {
        return Err(ContractFault::DeadlineExpired { now: now_nanos, deadline: view.deadline_nanos });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed view: generation 2 (even), sequence 5, deadline far ahead.
    fn good() -> GovernorContractView {
        GovernorContractView::new_command(2, 5, 100, 10_000, b"steer:1.5").unwrap()
    }

    #[test]
    fn well_formed_view_with_fresh_watermark_is_accepted() {
        assert_eq!(validate(&good(), 1_000, &AcceptedWatermark::new()), Ok(()));
    }

    #[test]
    fn layout_version_is_checked_first() {
        let mut v = good();
        v.layout_version = 999;
        // Even with everything else also wrong, the version is the reported fault.
        v.magic = 0;
        assert_eq!(
            validate(&v, 1_000, &AcceptedWatermark::new()),
            Err(ContractFault::LayoutVersionMismatch { found: 999, expected: LAYOUT_VERSION })
        );
    }

    #[test]
    fn wrong_magic_rejects() {
        let mut v = good();
        v.magic = 0xDEAD_BEEF;
        assert_eq!(
            validate(&v, 1_000, &AcceptedWatermark::new()),
            Err(ContractFault::MagicMismatch { found: 0xDEAD_BEEF })
        );
    }

    #[test]
    fn oversize_command_len_rejects_before_crc() {
        let mut v = good();
        v.command_len = (MAX_COMMAND_BYTES + 1) as u32;
        assert_eq!(
            validate(&v, 1_000, &AcceptedWatermark::new()),
            Err(ContractFault::CommandLenOversize {
                found: (MAX_COMMAND_BYTES + 1) as u32,
                max: MAX_COMMAND_BYTES as u32,
            })
        );
    }

    #[test]
    fn corrupt_command_fails_crc() {
        let mut v = good();
        v.command[0] ^= 0xFF; // flip a payload byte; crc32 field now stale
        match validate(&v, 1_000, &AcceptedWatermark::new()) {
            Err(ContractFault::CrcMismatch { .. }) => {}
            other => panic!("expected CrcMismatch, got {other:?}"),
        }
    }

    #[test]
    fn equal_sequence_is_replay_reject() {
        let mut wm = AcceptedWatermark::new();
        wm.record(&good()); // last = (gen 2, seq 5)
        // Same sequence, higher generation → still a replay on sequence.
        let mut v = good();
        v.generation = 4;
        v.crc32 = crate::crc::crc32_ieee(v.validated_command().unwrap());
        assert_eq!(
            validate(&v, 1_000, &wm),
            Err(ContractFault::SequenceRegressOrReplay { found: 5, last_accepted: 5 })
        );
    }

    #[test]
    fn equal_generation_with_newer_sequence_is_generation_replay() {
        let mut wm = AcceptedWatermark::new();
        wm.record(&good()); // last = (gen 2, seq 5)
        let mut v = good();
        v.sequence = 6; // newer sequence passes its check…
        v.generation = 2; // …but the generation regressed/equalled
        v.crc32 = crate::crc::crc32_ieee(v.validated_command().unwrap());
        assert_eq!(
            validate(&v, 1_000, &wm),
            Err(ContractFault::GenerationRegressOrReplay { found: 2, last_accepted: 2 })
        );
    }

    #[test]
    fn strictly_newer_generation_and_sequence_is_accepted() {
        let mut wm = AcceptedWatermark::new();
        wm.record(&good());
        let mut v = good();
        v.sequence = 6;
        v.generation = 4;
        v.crc32 = crate::crc::crc32_ieee(v.validated_command().unwrap());
        assert_eq!(validate(&v, 1_000, &wm), Ok(()));
    }

    #[test]
    fn expired_deadline_rejects_now_strictly_after() {
        let v = good(); // deadline 10_000
        assert_eq!(validate(&v, 10_000, &AcceptedWatermark::new()), Ok(()), "now == deadline is fresh");
        assert_eq!(
            validate(&v, 10_001, &AcceptedWatermark::new()),
            Err(ContractFault::DeadlineExpired { now: 10_001, deadline: 10_000 })
        );
    }

    #[test]
    fn rejected_snapshot_does_not_advance_the_watermark() {
        let mut wm = AcceptedWatermark::new();
        wm.record(&good());
        let before = wm;
        let mut replay = good(); // seq 5 == last → rejected
        replay.generation = 8;
        replay.crc32 = crate::crc::crc32_ieee(replay.validated_command().unwrap());
        let _ = validate(&replay, 1_000, &wm);
        // validate() does not mutate the watermark; the caller only records on Ok.
        assert_eq!(wm, before);
    }
}
