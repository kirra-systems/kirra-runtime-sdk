// parko/crates/parko-ros2/src/scene_vetoes.rs
//
// WS-0.1 (#G2 closure, occlusion/water/commit-zone axis) — publication-seam
// veto gates for the three scene checks that were built + tested in
// parko-core/parko-kirra but reachable from NO live path: RSS rule iv
// occlusion (`compute_occlusion_cap`), the SG4 WATER_UNTRAVERSABLE veto
// (`water_untraversable_veto`), and the SG5 COMMIT_ZONE_BLOCKED veto
// (`commit_zone_blocked`).
//
// Same seam and same discipline as `taj_objects::apply_object_rss_gate`
// (ADR-0029 Phase 3b): each gate composes ONTO a `TickOutcome` after the tick,
// bounding the exact twist about to be published. The node calls a gate ONLY
// when the corresponding scene channel is CONFIGURED (armed); an armed gate
// with a missing or stale scene fails CLOSED to the check's worst-case scene
// variant (`OcclusionScene::Absent` / `WaterScene::Unknown` /
// `CommitZoneScene::Unknown` — each of which the underlying primitive already
// treats as unsafe/veto). Not-configured → the gate is never called →
// byte-identical prior behaviour. This is the enabled-but-silent → fail-closed
// house rule: "the detector did not look" is never "clear".
//
// An already-stopped twist passes through every gate unchanged (a stop is the
// MRC itself — there is nothing stronger to impose) and preserves any upstream
// `TickError`.

use parko_core::commit_zone::{commit_zone_blocked, CommitZoneCfg, CommitZoneScene};
use parko_core::rss::{OcclusionScene, RssParams};
use parko_core::water::{water_untraversable_veto, WaterScene, WaterVetoConfig};
use parko_kirra::compute_occlusion_cap;

use crate::command_mapping::OutgoingTwist;
use crate::tick_pipeline::{TickError, TickOutcome};

/// A scene sample stamped with its production time, so the gates can apply
/// the same fail-closed freshness rule as the object-RSS gate (a stale scene
/// is a perception gap, never a verdict).
#[derive(Debug, Clone, PartialEq)]
pub struct StampedScene<T> {
    /// The scene the producer emitted.
    pub scene: T,
    /// Producer timestamp, ms (same clock as `now_ms` at the call site).
    pub stamp_ms: u64,
}

/// #770 F4 — tolerated future-stamp skew, ms. `now_ms` comes from a
/// NON-MONOTONIC wall clock (`SystemTime`), so a backward NTP step or a
/// producer whose clock runs ahead can stamp a scene in the future relative to
/// `now_ms`. A producer stamp up to this far ahead is tolerated as ordinary
/// clock jitter; beyond it the stamp is IMPLAUSIBLE and the scene is treated as
/// STALE (fail-closed), never age-0.
pub const SCENE_FUTURE_SKEW_BUDGET_MS: u64 = 50;

impl<T> StampedScene<T> {
    /// Age relative to `now_ms`; saturating (a future-stamped scene reads 0).
    #[must_use]
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.stamp_ms)
    }

    /// Fresh iff the stamp is neither too OLD (`age > max_age_ms`) nor
    /// implausibly in the FUTURE (#770 F4). The naive `age_ms <= max_age_ms`
    /// alone fails OPEN on a non-monotonic clock: `saturating_sub` reads any
    /// future stamp as age 0, so a backward clock step (every cached scene now
    /// "in the future") or a skewed-ahead producer keeps a STALE scene passing
    /// the freshness gate indefinitely — silently disarming the interlock whose
    /// whole purpose is "a stale scene is a perception gap, never a verdict."
    /// Treating a beyond-skew future stamp as stale closes both holes fail-closed.
    /// (A monotonic gate-clock domain is the fuller fix; this bounds the wall-clock
    /// exposure now.)
    #[must_use]
    pub fn is_fresh(&self, now_ms: u64, max_age_ms: u64) -> bool {
        if self.stamp_ms > now_ms.saturating_add(SCENE_FUTURE_SKEW_BUDGET_MS) {
            return false; // implausible future stamp → fail closed (stale)
        }
        self.age_ms(now_ms) <= max_age_ms
    }
}

/// Resolve an armed channel's slot to the scene the check runs on:
/// fresh → the producer's scene; missing, stale, OR implausibly-future-stamped
/// → the supplied fail-closed worst-case variant (#770 F4).
fn resolve_scene<T: Clone>(
    slot: Option<&StampedScene<T>>,
    max_age_ms: u64,
    now_ms: u64,
    fail_closed: T,
) -> T {
    match slot {
        Some(stamped) if stamped.is_fresh(now_ms, max_age_ms) => stamped.scene.clone(),
        _ => fail_closed,
    }
}

/// True when this outcome needs no further gating: the twist is already a
/// full stop (the MRC), so every veto is already satisfied — and gating it
/// again would only overwrite the upstream `TickError` provenance.
fn already_stopped(outcome: &TickOutcome) -> bool {
    outcome.twist.linear_x_mps == 0.0 && outcome.twist.angular_z_rads == 0.0
}

/// RSS rule iv — occlusion / assured-clear-distance gate. The ego speed about
/// to be published must not exceed the occlusion speed cap for the sightline
/// scene: `Absent` (or missing/stale when armed) caps at 0.0 (any motion
/// stops), `KnownClear` never binds, `Limited` binds via
/// `occlusion_limited_speed` (fail-closed to 0.0 on invalid inputs).
///
/// # Angular channel (#795 F8 — intended semantics)
///
/// The occlusion cap is an assured-clear-**distance** bound: it limits FORWARD
/// PROGRESS, so it binds only the LINEAR channel (`linear_x_mps`) and leaves
/// `angular_z_rads` untouched. This is DELIBERATE, not an oversight:
/// - A pure in-place rotation (`linear ≈ 0`, `angular ≠ 0`) under an `Absent` /
///   `Limited` scene PASSES this gate unchanged. Turning in place does not
///   advance the ego into the unobserved region — it is the *creep-and-peek*
///   primitive that lets the ego rotate to improve its own sightline before
///   committing to forward motion. Zeroing it would deadlock that maneuver.
/// - Any rotation with a nonzero turn RADIUS carries a nonzero `linear_x` and is
///   therefore still bound by the clamp above (the arc's along-track speed is
///   capped to the sightline).
///
/// CAVEAT (pending safety-owner decision, NOT silently assumed): a *swept*
/// in-place rotation of a long/rectangular footprint can sweep the vehicle's
/// extremities into an occluded conflict zone even at zero `linear_x`. Whether
/// to additionally BOUND that swept-rotation case here — versus the stricter
/// water / commit-zone gates, which zero BOTH channels — is a footprint-geometry
/// safety decision left to the owner. Until decided, the linear-only binding
/// documented above stands (this note exists so the choice is explicit, not
/// implicit). See `docs/safety/PARKO_SCENE_VETO_SEMANTICS.md`.
// SAFETY: SG1 | REQ: parko-ros2-occlusion-gate | TEST: occlusion_known_clear_passes,occlusion_limited_caps_overspeed,occlusion_limited_clamp_preserves_direction,occlusion_absent_scene_full_stops,occlusion_nonfinite_overspeed_full_stops,occlusion_limited_admits_slow_ego,occlusion_missing_scene_fails_closed,occlusion_stale_scene_fails_closed,already_stopped_outcome_passes_all_gates
pub fn apply_occlusion_gate(
    outcome: TickOutcome,
    occlusion: Option<&StampedScene<OcclusionScene>>,
    params: &RssParams,
    max_age_ms: u64,
    now_ms: u64,
) -> TickOutcome {
    if already_stopped(&outcome) {
        return outcome;
    }
    let scene = resolve_scene(occlusion, max_age_ms, now_ms, OcclusionScene::Absent);
    let cap = compute_occlusion_cap(&scene, params);
    let v = outcome.twist.linear_x_mps;
    // Within the assured-clear-distance cap (`<=` is NaN-safe: a non-finite
    // speed fails the comparison → the fail-closed branch below).
    if v.abs() <= cap {
        return outcome;
    }
    // #794 F8/F9: over the cap → CLAMP the ego to the assured-clear-distance
    // speed (creep at ±cap, preserving direction) instead of a bang-bang full
    // STOP that a cap-unaware planner fights every tick (park-at-every-blind-
    // junction / oscillation). The clamped speed IS safe by construction — `cap`
    // is the RSS-Rule-4 max safe speed for the sightline. Full-stop only when no
    // motion is admissible: `cap == 0` (Absent / no visibility) or a non-finite
    // command. The breach tag is preserved either way. (F8: the occlusion gate
    // binds only the LINEAR channel — the in-place-rotation-while-limited
    // decision is a separate track — so the clamp leaves `angular_z` untouched;
    // the full-stop path zeros both via `OutgoingTwist::stopped`.)
    if cap > 0.0 && v.is_finite() {
        TickOutcome {
            twist: OutgoingTwist {
                linear_x_mps: v.clamp(-cap, cap),
                ..outcome.twist
            },
            error: Some(TickError::OcclusionBreach),
            degraded: outcome.degraded,
        }
    } else {
        TickOutcome {
            twist: OutgoingTwist::stopped(outcome.twist.stamp_ms),
            error: Some(TickError::OcclusionBreach),
            degraded: outcome.degraded,
        }
    }
}

/// SG4 — WATER_UNTRAVERSABLE veto gate. A missing/stale scene when armed is
/// `WaterScene::Unknown` → veto (the detector did not look). A bounded-safe
/// puddle is NOT vetoed (no over-stop in rain); the unbounded signature stops.
// SAFETY: SG4 | REQ: parko-ros2-water-gate | TEST: water_clear_passes,water_unknown_scene_stops,water_missing_scene_fails_closed,already_stopped_outcome_passes_all_gates
pub fn apply_water_gate(
    outcome: TickOutcome,
    water: Option<&StampedScene<WaterScene>>,
    cfg: &WaterVetoConfig,
    max_age_ms: u64,
    now_ms: u64,
) -> TickOutcome {
    if already_stopped(&outcome) {
        return outcome;
    }
    let scene = resolve_scene(water, max_age_ms, now_ms, WaterScene::Unknown);
    if water_untraversable_veto(&scene, cfg) {
        TickOutcome {
            twist: OutgoingTwist::stopped(outcome.twist.stamp_ms),
            error: Some(TickError::WaterVeto),
            degraded: outcome.degraded,
        }
    } else {
        outcome
    }
}

/// SG5 — COMMIT_ZONE_BLOCKED veto gate. A missing/stale scene when armed is
/// `CommitZoneScene::Unknown` → veto ("reject fires from MAP ALONE"). A
/// healthy `NoZone` or a confirmed, exit-verified zone passes.
// SAFETY: SG5 | REQ: parko-ros2-commit-zone-gate | TEST: commit_zone_no_zone_passes,commit_zone_unknown_map_stops,commit_zone_missing_scene_fails_closed,already_stopped_outcome_passes_all_gates
pub fn apply_commit_zone_gate(
    outcome: TickOutcome,
    commit_zone: Option<&StampedScene<CommitZoneScene>>,
    cfg: &CommitZoneCfg,
    max_age_ms: u64,
    now_ms: u64,
) -> TickOutcome {
    if already_stopped(&outcome) {
        return outcome;
    }
    let scene = resolve_scene(commit_zone, max_age_ms, now_ms, CommitZoneScene::Unknown);
    if commit_zone_blocked(&scene, cfg) {
        TickOutcome {
            twist: OutgoingTwist::stopped(outcome.twist.stamp_ms),
            error: Some(TickError::CommitZoneVeto),
            degraded: outcome.degraded,
        }
    } else {
        outcome
    }
}

/// #795 F7 — the resolved inputs to the per-tick scene-veto chain. `Some(params)`
/// / `*_armed = true` ARMS a gate; the borrowed scene is that gate's latest
/// slot (already locked out of its mutex by the caller). Grouped into a struct
/// so [`apply_scene_gates`] has a small signature and the drain loop just fills
/// this in.
pub struct SceneGateChain<'a> {
    /// `Some` = occlusion gate armed; the RSS bound for the occlusion cap.
    pub occlusion_params: Option<&'a RssParams>,
    pub occlusion: Option<&'a StampedScene<OcclusionScene>>,
    pub water_armed: bool,
    pub water: Option<&'a StampedScene<WaterScene>>,
    pub water_cfg: &'a WaterVetoConfig,
    pub commit_zone_armed: bool,
    pub commit_zone: Option<&'a StampedScene<CommitZoneScene>>,
    pub commit_zone_cfg: &'a CommitZoneCfg,
    pub max_age_ms: u64,
    pub now_ms: u64,
}

/// #795 F7 — the per-tick scene-veto CHAIN, composed in the fixed order
/// occlusion → water → commit-zone. Each gate runs ONLY when armed; a DISARMED
/// gate is skipped (never stops), an ARMED gate with a missing/stale scene
/// fails CLOSED (stop) INSIDE the gate (the enabled-but-silent rule). Extracted
/// out of the `ros2`-gated drain loop (which was build-only in CI) so the
/// arming + ordering logic is unit-testable — the loop now just locks the slots
/// and calls this.
// SAFETY: SG1 SG4 SG5 | REQ: parko-ros2-scene-gate-chain | TEST: scene_gate_chain_arming_truth_table,scene_gate_chain_disarmed_gates_never_stop
#[must_use]
pub fn apply_scene_gates(outcome: TickOutcome, chain: &SceneGateChain<'_>) -> TickOutcome {
    let outcome = match chain.occlusion_params {
        Some(params) => apply_occlusion_gate(
            outcome,
            chain.occlusion,
            params,
            chain.max_age_ms,
            chain.now_ms,
        ),
        None => outcome,
    };
    let outcome = if chain.water_armed {
        apply_water_gate(
            outcome,
            chain.water,
            chain.water_cfg,
            chain.max_age_ms,
            chain.now_ms,
        )
    } else {
        outcome
    };
    if chain.commit_zone_armed {
        apply_commit_zone_gate(
            outcome,
            chain.commit_zone,
            chain.commit_zone_cfg,
            chain.max_age_ms,
            chain.now_ms,
        )
    } else {
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(linear: f64) -> TickOutcome {
        TickOutcome {
            twist: OutgoingTwist {
                linear_x_mps: linear,
                angular_z_rads: 0.0,
                stamp_ms: 7,
            },
            error: None,
            degraded: false,
        }
    }

    fn params() -> RssParams {
        // Courier-class numbers (mirrors taj_objects::courier_rss_params
        // magnitudes; exact values are irrelevant to the gate logic).
        RssParams {
            reaction_time: 0.5,
            accel_max: 1.0,
            brake_min: 1.0,
            brake_max: 4.0,
            lat_accel_max: 0.5,
            lat_brake_min: 0.35, // 0.7 × lat_accel_max (WP-07 split)
            mu_lateral_m: 0.2,
        }
    }

    fn stamped<T>(scene: T, stamp_ms: u64) -> StampedScene<T> {
        StampedScene { scene, stamp_ms }
    }

    // ---- occlusion ---------------------------------------------------------

    #[test]
    fn occlusion_known_clear_passes() {
        let s = stamped(OcclusionScene::KnownClear, 100);
        let out = apply_occlusion_gate(outcome(1.0), Some(&s), &params(), 500, 100);
        assert!(out.error.is_none());
        assert!((out.twist.linear_x_mps - 1.0).abs() < 1e-9);
    }

    #[test]
    fn occlusion_limited_caps_overspeed() {
        // #794 F8/F9: a bounded sightline yields a positive cap; an overspeed ego
        // is CLAMPED to that cap (creep), not bang-bang stopped. The breach tag is
        // still raised so provenance/telemetry see the correction.
        let scene = OcclusionScene::Limited {
            d_sight_m: 30.0,
            v_emerge_max_mps: 1.0,
        };
        let cap = compute_occlusion_cap(&scene, &params());
        assert!(
            cap > 0.0,
            "this sightline must admit creep for the clamp branch to be exercised; got {cap}"
        );
        let s = stamped(scene, 100);
        let out = apply_occlusion_gate(outcome(20.0), Some(&s), &params(), 500, 100);
        assert_eq!(
            out.twist.linear_x_mps, cap,
            "overspeed past a positive occlusion cap clamps to the cap (creep), not to 0"
        );
        assert_eq!(out.error, Some(TickError::OcclusionBreach));
    }

    #[test]
    fn occlusion_limited_clamp_preserves_direction() {
        // #794 F8/F9: a reversing ego over the cap clamps to -cap, not +cap — the
        // clamp bounds MAGNITUDE and keeps sign, it never flips travel direction.
        let scene = OcclusionScene::Limited {
            d_sight_m: 30.0,
            v_emerge_max_mps: 1.0,
        };
        let cap = compute_occlusion_cap(&scene, &params());
        assert!(cap > 0.0, "sightline must admit creep; got {cap}");
        let s = stamped(scene, 100);
        let out = apply_occlusion_gate(outcome(-20.0), Some(&s), &params(), 500, 100);
        assert_eq!(
            out.twist.linear_x_mps, -cap,
            "reverse overspeed clamps to -cap (magnitude bound, sign preserved)"
        );
        assert_eq!(out.error, Some(TickError::OcclusionBreach));
    }

    #[test]
    fn occlusion_absent_scene_full_stops() {
        // #794 F8/F9: cap == 0 (Absent / no admissible motion) is the only branch
        // that still bang-bang STOPS — there is no non-zero speed to clamp to.
        let s = stamped(OcclusionScene::Absent, 100);
        let out = apply_occlusion_gate(outcome(5.0), Some(&s), &params(), 500, 100);
        assert_eq!(
            out.twist.linear_x_mps, 0.0,
            "a zero cap admits no motion → full stop"
        );
        assert_eq!(out.error, Some(TickError::OcclusionBreach));
    }

    #[test]
    fn occlusion_nonfinite_overspeed_full_stops() {
        // A non-finite command can never be clamped to a finite cap safely → the
        // fail-closed full-stop branch, even under a positive cap.
        let s = stamped(
            OcclusionScene::Limited {
                d_sight_m: 30.0,
                v_emerge_max_mps: 1.0,
            },
            100,
        );
        let out = apply_occlusion_gate(outcome(f64::INFINITY), Some(&s), &params(), 500, 100);
        assert_eq!(
            out.twist.linear_x_mps, 0.0,
            "a non-finite command fails closed to a full stop, not a clamp"
        );
        assert_eq!(out.error, Some(TickError::OcclusionBreach));
    }

    #[test]
    fn occlusion_limited_admits_slow_ego() {
        // A generous sightline admits a slow ego (the creep-into-blind-junction
        // behaviour: the cap binds speed, it does not forbid motion).
        let s = stamped(
            OcclusionScene::Limited {
                d_sight_m: 50.0,
                v_emerge_max_mps: 1.0,
            },
            100,
        );
        let out = apply_occlusion_gate(outcome(0.3), Some(&s), &params(), 500, 100);
        assert!(
            out.error.is_none(),
            "slow ego under a long sightline passes; got {:?}",
            out.error
        );
    }

    #[test]
    fn occlusion_missing_scene_fails_closed() {
        // Armed gate, no scene → Absent → cap 0.0 → any motion stops.
        let out = apply_occlusion_gate(outcome(0.2), None, &params(), 500, 100);
        assert_eq!(out.twist.linear_x_mps, 0.0);
        assert_eq!(out.error, Some(TickError::OcclusionBreach));
    }

    #[test]
    fn occlusion_stale_scene_fails_closed() {
        // A KnownClear scene older than the budget is a gap, not a verdict.
        let s = stamped(OcclusionScene::KnownClear, 100);
        let out = apply_occlusion_gate(outcome(0.2), Some(&s), &params(), 500, 2_000);
        assert_eq!(
            out.twist.linear_x_mps, 0.0,
            "stale sightline must fail closed"
        );
        assert_eq!(out.error, Some(TickError::OcclusionBreach));
    }

    #[test]
    fn occlusion_future_stamped_scene_fails_closed() {
        // #770 F4 — a KnownClear scene stamped implausibly in the FUTURE
        // (backward clock step / skewed-ahead producer) must NOT read as age-0
        // fresh: beyond the skew budget it is treated as stale → fail closed.
        let s = stamped(OcclusionScene::KnownClear, 100_000);
        let out = apply_occlusion_gate(outcome(0.2), Some(&s), &params(), 500, 100);
        assert_eq!(
            out.twist.linear_x_mps, 0.0,
            "future-stamped sightline must fail closed, not read fresh"
        );
        assert_eq!(out.error, Some(TickError::OcclusionBreach));
    }

    #[test]
    fn scene_within_skew_budget_is_still_fresh() {
        // #770 F4 — a stamp a few ms ahead (ordinary jitter, within the skew
        // budget) is tolerated as fresh, so the fix doesn't over-reject.
        let s = stamped(OcclusionScene::KnownClear, 110);
        assert!(
            s.is_fresh(100, 500),
            "a stamp within the skew budget must stay fresh"
        );
        let out = apply_occlusion_gate(outcome(0.2), Some(&s), &params(), 500, 100);
        assert!(
            out.error.is_none(),
            "within-skew-budget KnownClear must pass"
        );
    }

    // ---- water --------------------------------------------------------------

    #[test]
    fn water_clear_passes() {
        let s = stamped(WaterScene::Clear, 100);
        let out = apply_water_gate(
            outcome(1.0),
            Some(&s),
            &WaterVetoConfig::default(),
            500,
            100,
        );
        assert!(out.error.is_none());
        assert!((out.twist.linear_x_mps - 1.0).abs() < 1e-9);
    }

    #[test]
    fn water_unknown_scene_stops() {
        let s = stamped(WaterScene::Unknown, 100);
        let out = apply_water_gate(
            outcome(1.0),
            Some(&s),
            &WaterVetoConfig::default(),
            500,
            100,
        );
        assert_eq!(
            out.twist.linear_x_mps, 0.0,
            "Unknown water must veto (stop short of water)"
        );
        assert_eq!(out.error, Some(TickError::WaterVeto));
    }

    #[test]
    fn water_missing_scene_fails_closed() {
        let out = apply_water_gate(outcome(1.0), None, &WaterVetoConfig::default(), 500, 100);
        assert_eq!(
            out.twist.linear_x_mps, 0.0,
            "armed-but-silent water channel must veto"
        );
        assert_eq!(out.error, Some(TickError::WaterVeto));
    }

    // ---- commit zone ---------------------------------------------------------

    #[test]
    fn commit_zone_no_zone_passes() {
        let s = stamped(CommitZoneScene::NoZone, 100);
        let out =
            apply_commit_zone_gate(outcome(1.0), Some(&s), &CommitZoneCfg::default(), 500, 100);
        assert!(out.error.is_none());
        assert!((out.twist.linear_x_mps - 1.0).abs() < 1e-9);
    }

    #[test]
    fn commit_zone_unknown_map_stops() {
        let s = stamped(CommitZoneScene::Unknown, 100);
        let out =
            apply_commit_zone_gate(outcome(1.0), Some(&s), &CommitZoneCfg::default(), 500, 100);
        assert_eq!(
            out.twist.linear_x_mps, 0.0,
            "an Unknown map must veto (reject from map alone)"
        );
        assert_eq!(out.error, Some(TickError::CommitZoneVeto));
    }

    #[test]
    fn commit_zone_missing_scene_fails_closed() {
        let out = apply_commit_zone_gate(outcome(1.0), None, &CommitZoneCfg::default(), 500, 100);
        assert_eq!(out.twist.linear_x_mps, 0.0);
        assert_eq!(out.error, Some(TickError::CommitZoneVeto));
    }

    // ---- composition ---------------------------------------------------------

    #[test]
    fn already_stopped_outcome_passes_all_gates() {
        // A stop is the MRC — gating it again must not run the checks or
        // overwrite the upstream error provenance.
        let stopped = TickOutcome {
            twist: OutgoingTwist::stopped(7),
            error: Some(TickError::ObjectRssBreach),
            degraded: false,
        };
        let out = apply_occlusion_gate(stopped.clone(), None, &params(), 500, 100);
        let out = apply_water_gate(out, None, &WaterVetoConfig::default(), 500, 100);
        let out = apply_commit_zone_gate(out, None, &CommitZoneCfg::default(), 500, 100);
        assert_eq!(
            out.error,
            Some(TickError::ObjectRssBreach),
            "upstream provenance preserved"
        );
        assert_eq!(out.twist, OutgoingTwist::stopped(7));
    }

    // ---- #795 F7: the composed chain's arming truth table ------------------

    /// Over the FULL {occlusion} × {water} × {commit-zone} arming table, a
    /// MOVING command with NO scenes fed stops IFF at least one gate is armed
    /// (an armed gate with a silent slot fails closed inside the gate); with no
    /// gate armed the command passes through unchanged. This pins the extracted
    /// composition's arming + ordering logic in a non-ros2 unit test.
    #[test]
    fn scene_gate_chain_arming_truth_table() {
        let p = params();
        let wcfg = WaterVetoConfig::default();
        let czcfg = CommitZoneCfg::default();
        for occ in [false, true] {
            for wat in [false, true] {
                for cz in [false, true] {
                    let chain = SceneGateChain {
                        occlusion_params: occ.then_some(&p),
                        occlusion: None,
                        water_armed: wat,
                        water: None,
                        water_cfg: &wcfg,
                        commit_zone_armed: cz,
                        commit_zone: None,
                        commit_zone_cfg: &czcfg,
                        max_age_ms: 100,
                        now_ms: 0,
                    };
                    let out = apply_scene_gates(outcome(2.0), &chain);
                    let any_armed = occ || wat || cz;
                    assert_eq!(
                        out.twist.linear_x_mps == 0.0,
                        any_armed,
                        "occ={occ} water={wat} commit_zone={cz}: armed+silent must fail closed"
                    );
                }
            }
        }
    }

    /// A DISARMED gate is skipped entirely: even a scene that WOULD stop
    /// (`OcclusionScene::Absent` / `WaterScene::Unknown` /
    /// `CommitZoneScene::Unknown`) present in a disarmed slot never stops the
    /// command — arming is the sole authority for whether a gate runs.
    #[test]
    fn scene_gate_chain_disarmed_gates_never_stop() {
        let wcfg = WaterVetoConfig::default();
        let czcfg = CommitZoneCfg::default();
        let occ = stamped(OcclusionScene::Absent, 0);
        let wat = stamped(WaterScene::Unknown, 0);
        let cz = stamped(CommitZoneScene::Unknown, 0);
        let chain = SceneGateChain {
            occlusion_params: None, // disarmed despite a would-stop scene
            occlusion: Some(&occ),
            water_armed: false,
            water: Some(&wat),
            water_cfg: &wcfg,
            commit_zone_armed: false,
            commit_zone: Some(&cz),
            commit_zone_cfg: &czcfg,
            max_age_ms: 100,
            now_ms: 0,
        };
        let out = apply_scene_gates(outcome(2.0), &chain);
        assert!(
            (out.twist.linear_x_mps - 2.0).abs() < 1e-9 && out.error.is_none(),
            "disarmed gates must not run — command passes unchanged, got {out:?}"
        );
    }
}
