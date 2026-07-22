//! # #796 F9 — the parko-side divergence-rate KPI row
//!
//! The root workspace's scenario-KPI gate (`crates/kirra-kpi-gate`) covers the
//! doer/checker/perception axes but nothing on the ML/diverse-governor side —
//! the review flagged that the `GovernorComparator` divergence rate over a
//! scripted corpus is the natural first parko KPI row. This test IS that row:
//! it drives the shipped CERT-006 lockstep pair (`KirraGovernor` primary +
//! `DiverseKirraGovernor` shadow) through a deterministic scripted command
//! corpus and gates the measured divergence rate.
//!
//! The reviewed bounds (the crate has no thresholds-JSON infrastructure; the
//! constants below ARE the committed policy, changed only with review):
//!
//! - **Agreement corpus → divergence rate EXACTLY 0.** The diverse pair must
//!   agree on every scripted tick — ramps, cruise, over-limit bursts (both
//!   clamp identically), angular sweeps, Degraded/LockedOut segments. Any
//!   future change to EITHER governor that opens daylight between them on
//!   this corpus turns the row red.
//! - **Discriminance control → divergence rate ≥ 0.9.** A deliberately
//!   mis-configured shadow (a lower ODD speed cap) over over-cap commands
//!   must be CAUGHT on ~every tick — proving the 0 above is discrimination,
//!   not blindness (the same mutation-testing-of-the-metric discipline as the
//!   root gate's #777 F1 negative controls).
//!
//! Required-check configuration (the F9 documentation half): this test rides
//! the **"Parko Safety Tests (no ROS, no ORT)"** CI job (`cargo test
//! --workspace` in `parko/` with the runtime-dependent excludes) — the lane
//! is runtime-free and `parko-kirra` is never excluded, so the row gates
//! every PR exactly like the root `scenario-KPI gate (WS-3.1)` job. Both jobs
//! must stay in the repository's required-checks set for the KPI gates to be
//! merge-blocking.
//!
//! Determinism: no RNG, no wall clock — the corpus is a closed-form script
//! and `evaluate` is driven with a fixed `DT_S`. A red row is a real change
//! in governor behavior, never flake.

use std::sync::Arc;

use parko_core::commands::ControlCommand;
use parko_core::safety::SafetyPosture;
use parko_kirra::comparator::{GovernorComparator, InMemoryDivergenceSink};
use parko_kirra::diverse::DiverseKirraGovernor;
use parko_kirra::KirraGovernor;

/// Fixed virtual tick, seconds.
const DT_S: f64 = 0.1;

/// The committed floor a discriminance-control divergence rate must clear.
const CONTROL_BREACH_MIN: f64 = 0.9;

/// The deterministic scripted command corpus: (linear m/s, angular rad/s,
/// posture) per tick. Covers ramps, cruise, over-limit bursts (clamped
/// identically by a correct diverse pair), angular sweeps, and the
/// non-Nominal postures.
fn scripted_corpus() -> Vec<(f64, f64, SafetyPosture)> {
    let mut script = Vec::new();

    // Ramp 0 → 3 m/s over 30 ticks, then cruise.
    for i in 0..30 {
        script.push((0.1 * f64::from(i), 0.0, SafetyPosture::Nominal));
    }
    for _ in 0..30 {
        script.push((3.0, 0.0, SafetyPosture::Nominal));
    }
    // Over-limit burst: both governors must clamp IDENTICALLY.
    for _ in 0..20 {
        script.push((50.0, 0.0, SafetyPosture::Nominal));
    }
    // Angular sweep at cruise (±1 rad/s triangle).
    for i in 0..40 {
        let a = if i < 20 {
            0.05 * f64::from(i)
        } else {
            0.05 * f64::from(40 - i)
        };
        script.push((2.0, a, SafetyPosture::Nominal));
    }
    // Degraded segment: decel-to-stop semantics, identically applied.
    for i in 0..20 {
        script.push((2.0 - 0.1 * f64::from(i), 0.0, SafetyPosture::Degraded));
    }
    // LockedOut segment: both must command safe stop.
    for _ in 0..10 {
        script.push((1.0, 0.2, SafetyPosture::LockedOut));
    }
    script
}

/// Drive a comparator through the script; return (divergent ticks, total).
fn divergence_over<S>(
    comparator: &GovernorComparator<S>,
    sink: &InMemoryDivergenceSink,
    script: &[(f64, f64, SafetyPosture)],
) -> (usize, usize)
where
    S: parko_core::safety::SafetyGovernor,
{
    let mut previous: Option<ControlCommand> = None;
    for (i, &(lin, ang, posture)) in script.iter().enumerate() {
        let cmd = ControlCommand {
            linear_velocity: lin,
            angular_velocity: ang,
            timestamp_ms: (i as u64) * 100,
        };
        let _ = comparator.evaluate(&cmd, previous.as_ref(), DT_S, posture);
        previous = Some(cmd);
    }
    (sink.len(), script.len())
}

/// THE F9 row (green half): the shipped diverse pair agrees on EVERY tick of
/// the scripted corpus — divergence rate exactly 0.
#[test]
fn diverse_pair_divergence_rate_is_zero_over_scripted_corpus() {
    let sink = Arc::new(InMemoryDivergenceSink::new());
    // RSS is declared externally gated (the publication-seam arrangement):
    // without a live RSS feed both governors fail closed into MRC semantics,
    // which would test the Degraded arm three times and the NOMINAL ceiling
    // arms never.
    let comparator = GovernorComparator::with_sink(
        KirraGovernor::new(),
        DiverseKirraGovernor::new(),
        sink.clone(),
    )
    .with_external_rss_gate();
    let script = scripted_corpus();
    let (divergent, total) = divergence_over(&comparator, &sink, &script);
    assert!(total >= 150, "corpus stays non-trivial ({total} ticks)");
    assert_eq!(
        divergent, 0,
        "the shipped diverse pair must agree on the whole scripted corpus \
         (divergence rate {divergent}/{total}); a red row here is a REAL \
         behavioral split between KirraGovernor and DiverseKirraGovernor — \
         fix the regression, never widen this bound"
    );
}

/// THE F9 row (red half / discriminance control): a shadow with a
/// deliberately lower ODD speed cap must be caught on ≥ 90 % of over-cap
/// ticks — the zero above is discrimination, not blindness.
#[test]
fn misconfigured_shadow_breaches_the_divergence_floor() {
    let sink = Arc::new(InMemoryDivergenceSink::new());
    // Shadow's ODD cap is 1.0 m/s; every scripted command below is ABOVE it
    // and below the primary's ceiling, so the shadow's re-derived
    // `effective_ceiling` arm clamps where the primary allows → the pair
    // must split on every tick. (The DIVERSE shadow carries the cap: its
    // ceiling derivation is the independently-implemented arm this control
    // exercises.)
    let comparator = GovernorComparator::with_sink(
        KirraGovernor::new(),
        DiverseKirraGovernor::new().with_odd_speed_cap(1.0),
        sink.clone(),
    )
    .with_external_rss_gate();
    let script: Vec<(f64, f64, SafetyPosture)> = (0..50)
        .map(|_| (2.0, 0.0, SafetyPosture::Nominal))
        .collect();
    let (divergent, total) = divergence_over(&comparator, &sink, &script);
    let rate = divergent as f64 / total as f64;
    assert!(
        rate >= CONTROL_BREACH_MIN,
        "a mis-capped shadow must drive the divergence rate over \
         {CONTROL_BREACH_MIN} (got {divergent}/{total} = {rate}); if this \
         drops, the comparator has been blinded"
    );
}

/// The scripted corpus is deterministic: two fresh comparator runs produce
/// identical divergence counts (no RNG, no wall-clock dependence in the
/// gated path).
#[test]
fn divergence_row_is_deterministic() {
    let run = || {
        let sink = Arc::new(InMemoryDivergenceSink::new());
        let comparator = GovernorComparator::with_sink(
            KirraGovernor::new(),
            DiverseKirraGovernor::new(),
            sink.clone(),
        )
        .with_external_rss_gate();
        divergence_over(&comparator, &sink, &scripted_corpus())
    };
    assert_eq!(run(), run());
}
