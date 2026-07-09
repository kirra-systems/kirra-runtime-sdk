//! **SG2 containment — property tests.** The unit tests pin specific cases; these assert
//! the *invariant* across thousands of random trajectories. The load-bearing one is the
//! core safety property stated as a differential check against a **closed-form reference**:
//! for a straight, axis-aligned corridor with a heading-0 footprint, a trajectory is
//! Allowed *iff* every footprint corner is at least `margin` inside the lateral boundaries
//! — pure algebra, no re-implementation of the production geometry. So a mismatch means the
//! production check admitted something it shouldn't (or rejected something it should admit).

use kirra_core::containment::{
    validate_trajectory_containment, Corridor, Point, Pose, VehicleFootprint,
    MAX_TRAJECTORY_HORIZON,
};
use kirra_core::frame_integrity::{containment_margin_m, FrameTrust};
use kirra_core::kinematics_contract::{DenyCode, EnforceAction, VehicleKinematicsContract};
use proptest::prelude::*;

/// A long, straight, axis-aligned corridor: left edge at `y = +HALF`, right at `y = -HALF`,
/// spanning `x ∈ [0, LEN]`. Long enough that the end-cap margin never binds.
const HALF: f64 = 5.0;
const LEN: f64 = 200.0;
/// Keep poses in a longitudinal middle band so only the lateral edges gate (the closed-form
/// case); the caps at x=0 / x=LEN stay tens of metres away — far beyond any margin.
const X_LO: f64 = 50.0;
const X_HI: f64 = 150.0;

fn footprint() -> VehicleFootprint {
    VehicleFootprint::from(&VehicleKinematicsContract::nominal_reference_profile())
}

fn straight_corridor<'a>(left: &'a [Point], right: &'a [Point]) -> Corridor<'a> {
    Corridor {
        left,
        right,
        confidence: 1.0,
        age_ms: 0,
        min_confidence: 0.5,
        max_age_ms: 5_000,
    }
}

proptest! {
    /// **The SG2 core invariant, differential vs closed form.** For the straight
    /// axis-aligned corridor + heading-0 footprint, the worst corner's lateral reach is
    /// `|y| + width/2`, so the verdict must be `Allow` iff `|y| + width/2 ≤ HALF − margin`
    /// for *every* pose. We assert the production check agrees with that algebra.
    #[test]
    fn admitted_iff_every_corner_is_within_lateral_margin(
        ys in prop::collection::vec(-(HALF + 2.0)..(HALF + 2.0), 1..=MAX_TRAJECTORY_HORIZON),
        x0 in X_LO..X_HI,
    ) {
        let fp = footprint();
        let half_w = fp.width_m * 0.5;
        let margin = containment_margin_m(FrameTrust::Trusted).expect("Trusted has a margin");

        // March x forward inside the safe band (so the cap margin can never bind), heading 0.
        let dx = (X_HI - x0) / (MAX_TRAJECTORY_HORIZON as f64);
        let traj: Vec<Pose> = ys.iter().enumerate()
            .map(|(i, &y)| Pose { x_m: x0 + i as f64 * dx, y_m: y, heading_rad: 0.0 })
            .collect();

        let left  = [Point { x_m: 0.0, y_m: HALF },  Point { x_m: LEN, y_m: HALF }];
        let right = [Point { x_m: 0.0, y_m: -HALF }, Point { x_m: LEN, y_m: -HALF }];
        let corridor = straight_corridor(&left, &right);

        // Closed-form reference: slack of the WORST pose's worst corner to the nearer edge,
        // minus the margin. Admit iff that's ≥ 0.
        let worst_slack = traj.iter()
            .map(|p| HALF - margin - (p.y_m.abs() + half_w))
            .fold(f64::INFINITY, f64::min);

        // Dead-band: production uses squared distances; skip cases within 1e-6 of the exact
        // boundary, where linear-vs-squared FP rounding could legitimately disagree.
        prop_assume!(worst_slack.abs() > 1e-6);

        let verdict = validate_trajectory_containment(&traj, &corridor, &fp, FrameTrust::Trusted);
        if worst_slack > 0.0 {
            prop_assert_eq!(verdict, EnforceAction::Allow,
                "every corner ≥ margin inside ⇒ Allow (worst_slack={})", worst_slack);
        } else {
            prop_assert_eq!(verdict, EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture),
                "a corner within margin of / past the boundary ⇒ Deny (worst_slack={})", worst_slack);
        }
    }

    /// **Translation invariance.** Containment is frame-relative: translating the whole
    /// trajectory AND the corridor by the same vector must not change the verdict. Catches
    /// any absolute-coordinate leakage.
    #[test]
    fn verdict_is_translation_invariant(
        ys in prop::collection::vec(-8.0_f64..8.0, 1..=20usize),
        tx in -500.0_f64..500.0,
        ty in -500.0_f64..500.0,
    ) {
        let fp = footprint();
        let verdict_at = |ox: f64, oy: f64| {
            let traj: Vec<Pose> = ys.iter().enumerate()
                .map(|(i, &y)| Pose { x_m: ox + 50.0 + i as f64 * 2.0, y_m: oy + y, heading_rad: 0.0 })
                .collect();
            let left  = [Point { x_m: ox, y_m: oy + HALF },  Point { x_m: ox + LEN, y_m: oy + HALF }];
            let right = [Point { x_m: ox, y_m: oy - HALF }, Point { x_m: ox + LEN, y_m: oy - HALF }];
            // Corridor borrows the local arrays; evaluate before they drop.
            let corridor = straight_corridor(&left, &right);
            validate_trajectory_containment(&traj, &corridor, &fp, FrameTrust::Trusted)
        };
        prop_assert_eq!(verdict_at(0.0, 0.0), verdict_at(tx, ty),
            "containment verdict must be invariant under a common translation");
    }
}
