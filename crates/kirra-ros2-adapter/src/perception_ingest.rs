// crates/kirra-ros2-adapter/src/perception_ingest.rs
//
// KIRRA-OCCY-PMON-003 slice-1 — the perception ingest that flips the Track-C
// kinematic guard from dormant to ENFORCING at the adapter surface (D3a).
//
// THIS MODULE IS NOT ros2-gated. It holds the *safety-relevant* ingest
// transforms as PURE functions over kernel types + the adapter's plain
// `PerceivedObject` (itself non-gated), so they compile and are unit-tested
// under DEFAULT features (CI `Test`). The ROS2 wiring in `node.rs`
// (`ros2`-gated) is a thin extractor that snapshots `PerceivedObject`s and
// calls these — it makes no safety decision.
//
// Pipeline (all pure here):
//   PerceivedObject[]  --perceived_to_tracked-->  TrackedObject[]
//                      --ingest_perception_output--> PerceptionOutput
//                      --PerceptionCapPublisher::on_tick--> SharedPerceptionCap
//   then the slow loop: resolve_perception_cap(enabled, cache, now) -> Option<f64>
//        -> validate_trajectory_slow_capped(.., eff_cap) -> apply_perception_cap
//
// Scope: kinematic reported-velocity CEILING only (D2a; teleport D2b deferred).
// Range guard, parko-kirra, and the verifier HTTP/fabric path (D3b) are staged.

use kirra_core::perception_monitor::{
    ingest_perception_output, tracked_object_from_parts, PerceptionCapPublisher, TrackedObject, Vec2,
};

use crate::state::PerceivedObject;

/// Env var that enables the Track-C perception derate on the adapter surface.
/// **Default OFF** (mirrors the `KIRRA_POSTURE_STREAM_URL` presence-gating in
/// `node.rs`): unset / falsey → disabled → `resolve_perception_cap` returns
/// state-1 `None` → pure no-op. Enabling on a real vehicle is additionally
/// gated on the D4 frame-confirm + a sim/bench validation gate (NOT in this
/// slice).
pub const PERCEPTION_DERATE_ENABLED_ENV: &str = "KIRRA_PERCEPTION_DERATE_ENABLED";

/// Read the enable gate from the environment. Truthy = `1`/`true`/`yes`
/// (case-insensitive); anything else (including unset) = disabled.
#[must_use]
pub fn perception_derate_enabled() -> bool {
    std::env::var(PERCEPTION_DERATE_ENABLED_ENV)
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Pure shim: map the adapter's `PerceivedObject`s to kernel `TrackedObject`s
/// (KIRRA-OCCY-PMON-003 §3). Reported-velocity vector is carried through;
/// `prev_pos_m`/`dt_s` are set so the teleport check is inert (D2a). No
/// tracking or association happens here — IDs + velocity come from upstream
/// (ADR-0004).
#[must_use]
pub fn perceived_to_tracked(objects: &[PerceivedObject]) -> Vec<TrackedObject> {
    objects
        .iter()
        .map(|o| {
            tracked_object_from_parts(
                o.id,
                Vec2 { x: o.pos.x_m, y: o.pos.y_m },
                Vec2 { x: o.vel.x_m, y: o.vel.y_m },
            )
        })
        .collect()
}

/// Run one perception tick: remap → assemble → publish the kinematic-guard cap
/// into the publisher's `SharedPerceptionCap`. `tick_ms` should be the
/// **objects' freshness timestamp** (so the cap ages with the object stream and
/// `resolve_perception_cap` fails closed when objects go silent), not a
/// trajectory-cycle clock.
pub fn publish_perception_tick(
    publisher: &PerceptionCapPublisher,
    objects: &[PerceivedObject],
    tick_ms: u64,
) {
    let tracked = perceived_to_tracked(objects);
    let perception = ingest_perception_output(&tracked);
    publisher.on_tick(&perception, tick_ms);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corridor::Point;
    use kirra_core::kinematics_contract::{
        validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
    };
    use kirra_core::perception_monitor::{
        apply_perception_cap, empty_perception_cap, resolve_perception_cap,
        KinematicPlausibilityContract,
    };

    fn obj(id: u64, vx: f64, vy: f64) -> PerceivedObject {
        PerceivedObject {
            id,
            pos: Point { x_m: 0.0, y_m: 0.0 },
            velocity_mps: (vx * vx + vy * vy).sqrt(),
            heading_rad: 0.0,
            vel: Point { x_m: vx, y_m: vy },
        }
    }

    fn publisher(cache: &kirra_core::perception_monitor::SharedPerceptionCap) -> PerceptionCapPublisher {
        PerceptionCapPublisher::new(cache.clone(), KinematicPlausibilityContract::urban_reference(), 500)
    }

    // ---- shim ----

    #[test]
    fn shim_carries_velocity_vector_and_neutralizes_teleport() {
        let tracked = perceived_to_tracked(&[obj(1, 3.0, 4.0)]);
        assert_eq!(tracked.len(), 1);
        assert_eq!(tracked[0].id, 1);
        assert_eq!(tracked[0].vel_mps, Vec2 { x: 3.0, y: 4.0 });
        assert_eq!(tracked[0].prev_pos_m, tracked[0].pos_m); // teleport no-op
        assert!(tracked[0].dt_s > 0.0);
    }

    // ---- publisher tick over synthetic objects ----

    #[test]
    fn tick_publishes_nominal_cap_for_plausible_objects() {
        let cache = empty_perception_cap();
        let p = publisher(&cache);
        publish_perception_tick(&p, &[obj(1, 5.0, 0.0), obj(2, 3.0, 4.0)], 1000);
        // No object over the 60 m/s ceiling → nominal cap published.
        let cap = resolve_perception_cap(true, &cache, 1100);
        assert_eq!(
            cap,
            Some(kirra_core::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS)
        );
    }

    #[test]
    fn tick_publishes_mrc_floor_for_implausible_object() {
        let cache = empty_perception_cap();
        let p = publisher(&cache);
        // |vel| = 70 > 60 → implausible; single object → fraction 1.0 → MRC floor.
        publish_perception_tick(&p, &[obj(1, 70.0, 0.0)], 1000);
        assert_eq!(resolve_perception_cap(true, &cache, 1100), Some(0.0));
    }

    // ---- 3-state resolve (delegated to the kernel resolver, asserted here) ----

    #[test]
    fn resolve_three_states() {
        let cache = empty_perception_cap();
        let p = publisher(&cache);
        publish_perception_tick(&p, &[obj(1, 5.0, 0.0)], 1000);

        // state 1 — disabled → no-op
        assert_eq!(resolve_perception_cap(false, &cache, 1100), None);
        // state 2 — enabled + fresh → the published cap
        assert!(resolve_perception_cap(true, &cache, 1100).is_some());
        assert!(resolve_perception_cap(true, &cache, 1100).unwrap() > 0.0);
        // state 3 — enabled + stale (now - gen = 600 > ttl 500) → MRC floor
        assert_eq!(resolve_perception_cap(true, &cache, 1600), Some(0.0));
    }

    #[test]
    fn resolve_state3_when_never_published() {
        let cache = empty_perception_cap();
        assert_eq!(resolve_perception_cap(true, &cache, 1000), Some(0.0));
    }

    // ---- sweep on a silent stream ----

    #[test]
    fn sweep_publishes_mrc_on_silent_stream() {
        let cache = empty_perception_cap();
        let p = publisher(&cache);
        p.sweep_staleness(2000);
        assert_eq!(cache.read().unwrap().unwrap().cap_mps, 0.0);
    }

    // ---- compose: resolve → apply → per-pose verdict ----

    fn cmd_at(v: f64) -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: v,
            current_velocity_mps: v,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        }
    }

    #[test]
    fn compose_state2_clamps_to_cap() {
        let cache = empty_perception_cap();
        // Publish an implausible snapshot → cap = MRC floor 0.0… instead test a
        // mid cap by writing directly is covered in the kernel; here exercise the
        // adapter path end-to-end with a plausible cap by using a contract whose
        // nominal cap is the published value.
        let p = publisher(&cache);
        publish_perception_tick(&p, &[obj(1, 5.0, 0.0)], 1000); // nominal cap (22.35)
        let eff = resolve_perception_cap(true, &cache, 1100);

        let base = VehicleKinematicsContract::nominal_reference_profile(); // max 35
        let tightened = apply_perception_cap(&base, eff);
        // Command at 30 m/s > 22.35 perception cap → clamp to 22.35.
        let verdict = validate_vehicle_command(&cmd_at(30.0), &tightened);
        assert_eq!(
            verdict,
            EnforceAction::ClampLinear(
                kirra_core::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS
            )
        );
    }

    #[test]
    fn compose_state3_is_controlled_stop() {
        let cache = empty_perception_cap(); // never published → state 3
        let eff = resolve_perception_cap(true, &cache, 1000);
        assert_eq!(eff, Some(0.0));
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let tightened = apply_perception_cap(&base, eff);
        let verdict = validate_vehicle_command(&cmd_at(20.0), &tightened);
        assert_eq!(verdict, EnforceAction::ClampLinear(0.0)); // controlled stop
    }

    #[test]
    fn compose_state1_is_noop() {
        let cache = empty_perception_cap();
        let eff = resolve_perception_cap(false, &cache, 1000); // disabled
        assert_eq!(eff, None);
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let tightened = apply_perception_cap(&base, eff);
        // Identical to baseline: 20 m/s under the 35 m/s vehicle max → Allow.
        assert_eq!(validate_vehicle_command(&cmd_at(20.0), &tightened), EnforceAction::Allow);
        assert_eq!(tightened.effective_max_speed_mps(), base.effective_max_speed_mps());
    }

    // ---- env gate default off ----

    #[test]
    fn env_gate_defaults_off_when_unset() {
        // Do not mutate process env in a multithreaded test runner (INV-13);
        // assert the default-off contract by reading the current value, which
        // is unset in CI → false.
        if std::env::var(PERCEPTION_DERATE_ENABLED_ENV).is_err() {
            assert!(!perception_derate_enabled(), "unset env must be disabled");
        }
    }
}
