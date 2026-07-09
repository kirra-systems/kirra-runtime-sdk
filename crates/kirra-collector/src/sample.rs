// crates/kirra-collector/src/sample.rs
//
// Stratified pass-sampling (docs/COLLECTOR_DESIGN.md [D5]). Selection bias is the
// trap: training only on corrections teaches the wrong thing. So the collector
// keeps EVERY intervention (clamp / steering-clamp / deny / MRC) unconditionally,
// and samples the abundant ALLOW ("pass") records at a configurable `pass_rate`
// (bench default 1.0 = keep everything).
//
// The sampling is DETERMINISTIC — a stable hash of `(source, decision_seq)` maps
// each record to a fixed value in [0, 1), kept iff that value < `pass_rate`. This
// makes a dataset reproducible (same inputs + rate → same rows) and lets the
// reconciliation counts be asserted exactly in tests, rather than relying on a
// random seed.

use kirra_capture_schema::{CaptureOutcome, CaptureRecord};

use crate::source_token;

/// A record is an "intervention" iff Kirra did anything other than allow it.
/// These are NEVER sampled out — they are the rare, valuable corrective signal.
#[must_use]
pub fn is_intervention(rec: &CaptureRecord) -> bool {
    rec.outcome != CaptureOutcome::Allow
}

/// Deterministic per-record sample value in `[0, 1)`. FNV-1a over the join key
/// `(source, decision_seq)`, folded to a 53-bit mantissa for a uniform double.
#[must_use]
pub fn sample_value(rec: &CaptureRecord) -> f64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in source_token(rec.source).as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    for b in rec.decision_seq.to_le_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    // Top 53 bits → uniform in [0, 1). Strictly < 1.0, so pass_rate = 1.0 keeps
    // every pass and pass_rate = 0.0 keeps none.
    ((h >> 11) as f64) / ((1u64 << 53) as f64)
}

/// The stratified keep decision: all interventions, plus passes whose
/// deterministic sample value is below `pass_rate`.
#[must_use]
pub fn keep(rec: &CaptureRecord, pass_rate: f64) -> bool {
    is_intervention(rec) || sample_value(rec) < pass_rate
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_capture_schema::{CaptureRecord, CaptureSource};

    fn rec(seq: u64, outcome: CaptureOutcome) -> CaptureRecord {
        CaptureRecord {
            decision_seq: seq,
            t_mono_ns: 0,
            t_wall_ms: 0,
            source: CaptureSource::CommandGateway,
            proposed: None,
            traj: None,
            outcome,
            deny_code: None,
            safe_value: None,
            mrc: false,
            posture: "NOMINAL".to_string(),
            derate_enabled: false,
        }
    }

    #[test]
    fn sample_value_is_in_unit_interval_and_deterministic() {
        for seq in 0..1000 {
            let v = sample_value(&rec(seq, CaptureOutcome::Allow));
            assert!((0.0..1.0).contains(&v), "value {v} out of [0,1)");
            // stable across calls
            assert_eq!(v, sample_value(&rec(seq, CaptureOutcome::Allow)));
        }
    }

    #[test]
    fn rate_one_keeps_all_passes_rate_zero_keeps_none() {
        for seq in 0..1000 {
            let r = rec(seq, CaptureOutcome::Allow);
            assert!(keep(&r, 1.0), "pass_rate 1.0 must keep every pass");
            assert!(!keep(&r, 0.0), "pass_rate 0.0 must drop every pass");
        }
    }

    #[test]
    fn interventions_are_never_sampled_out() {
        for outcome in [
            CaptureOutcome::ClampLinear,
            CaptureOutcome::ClampSteering,
            CaptureOutcome::Deny,
        ] {
            let r = rec(7, outcome);
            assert!(
                keep(&r, 0.0),
                "intervention must survive even at pass_rate 0.0"
            );
        }
    }
}
