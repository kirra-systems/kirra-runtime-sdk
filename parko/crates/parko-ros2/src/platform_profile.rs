// parko/crates/parko-ros2/src/platform_profile.rs
//
// ADR-0029 Phase 2 — the live differential-drive courier deployment profile.
//
// Phase 1 closed the silently-unbounded angular axis in the SDK slow-loop
// checker (`crates/kirra-ros2-adapter`). Phase 2 is the LIVE half: the parko
// ML node (`parko-ros2`) drives the Rosmaster's `(v, ω)` cmd_vel through
// parko-kirra's diff-drive checker. The angular bound is ALREADY enforced by
// `KirraGovernor` (`nominal_angular_clamp` → `AngularVelocityBound::omega_max`),
// but the stock node builds `KirraGovernor::new()` — the SOTIF
// `PlatformParams::conservative_default()` (ω_max(0) ≈ 0.20 rad/s), a generic
// bound for an UNCHARACTERIZED platform. The courier is characterized: this
// profile parameterizes the live governor with the courier's SOTIF envelope
// (`urban_service_robot_reference`, ω_max(0) ≈ 0.833 rad/s) and exposes the
// `DiffDrivePlatform` checker as the named per-command safety surface
// (the "checker BOUNDS the doer" thesis, S-PK1b), plus the SG2 containment
// seam (`validate_platform_containment`) fed the courier footprint.
//
// Cited-copy correspondence (one robot, two dependency-separated workspaces):
//   - `crates/kirra-ros2-adapter` `VehicleConfig::courier()` holds a CITED COPY
//     of the angular numbers (Phase 1, gated by `courier_angular_bound_matches_
//     parko_record`). HERE in parko we are the MODEL OF RECORD: this profile
//     uses parko's own `PlatformParams::urban_service_robot_reference()` and
//     `DiffDrivePlatform` directly — no copy, no cross-workspace import.
//   - The 0.6 × 0.9 m footprint matches the SDK `courier()` footprint and the
//     `DiffDrivePlatform::centered_footprint(0.6, 0.9)` used in the S-PK1b tests.
//
// Frozen-AV-safe: this is purely additive. `ParkoNodeConfig::platform_profile`
// defaults to `None` → the node keeps building `KirraGovernor::new()`
// (conservative default), byte-identical to today. The profile is opt-in.

use kirra_core::containment::VehicleFootprint;
use parko_kirra::platform::DiffDrivePlatform;
use parko_kirra::{KirraGovernor, PlatformParams};

/// Per-platform deployment profile for the differential-drive courier
/// (Rosmaster R2). Carries the SOTIF angular `PlatformParams` (the model of
/// record), the footprint, and the linear kinematic limits the diff-drive
/// checker reads. Constructed by the node binary and threaded through
/// [`ParkoNodeConfig`](crate::config::ParkoNodeConfig) to build the live
/// [`DiffDrivePlatform`] checker.
#[derive(Debug, Clone, PartialEq)]
pub struct CourierPlatformProfile {
    /// SOTIF angular-velocity params → `ω_max(v) = min(rollover, sweep, ftti)`.
    /// The model of record (issue #136); the SDK courier holds a cited copy.
    pub angular_params: PlatformParams,
    /// Footprint width, m (lateral). The diff-drive center convention is
    /// applied in [`Self::footprint`].
    pub footprint_width_m: f64,
    /// Footprint length, m (longitudinal).
    pub footprint_length_m: f64,
    /// Max linear speed, m/s — the diff-drive checker's `max_speed_mps`.
    pub max_speed_mps: f64,
    /// Max braking deceleration, m/s².
    pub max_brake_mps2: f64,
    /// Linear converge-to-stop floor, m/s (the angular floor is
    /// `STOP_EPSILON_RAD_S`, held inside parko-kirra).
    pub stop_epsilon_mps: f64,
}

impl CourierPlatformProfile {
    /// **The Rosmaster R2 sidewalk courier.** Cited-copy correspondence:
    /// the angular params ARE parko's `urban_service_robot_reference()` (the
    /// SOTIF model of record), and the 0.6 × 0.9 m footprint matches the SDK
    /// `VehicleConfig::courier()` profile — same robot, two workspaces.
    /// ω_max(0) ≈ 0.833 rad/s (sweep binds); MRC halves the contact/heading
    /// budgets (`mrc_posture_factor = 0.5`).
    #[must_use]
    pub fn courier_reference() -> Self {
        Self {
            angular_params: PlatformParams::urban_service_robot_reference(),
            footprint_width_m: 0.6,
            footprint_length_m: 0.9,
            max_speed_mps: 1.5,
            max_brake_mps2: 1.0,
            stop_epsilon_mps: 0.05,
        }
    }

    /// The center-referenced [`VehicleFootprint`] for the courier
    /// (`wheelbase_m = 0`, symmetric overhangs) — diff-drive geometry.
    #[must_use]
    pub fn footprint(&self) -> VehicleFootprint {
        DiffDrivePlatform::<KirraGovernor>::centered_footprint(
            self.footprint_width_m,
            self.footprint_length_m,
        )
    }

    /// The courier-parameterized [`KirraGovernor`] — a governor whose angular
    /// bound is the courier's SOTIF envelope (`with_platform_params`), NOT the
    /// `KirraGovernor::new()` conservative default. This is the governor the
    /// live node must attach so the in-place-rotation bound matches the
    /// courier's actual geometry. The linear kinematic contract is unchanged
    /// (the frozen `validate_vehicle_command` talisman).
    #[must_use]
    pub fn angular_governor(&self) -> KirraGovernor {
        KirraGovernor::new().with_platform_params(self.angular_params.clone())
    }

    /// The [`DiffDrivePlatform`] checker — the named per-command safety surface
    /// for ADR-0029 Phase 2. Wraps [`Self::angular_governor`] under the
    /// `PlatformKinematics` abstraction (S-PK1b) so the same generic SG2
    /// containment seam (`validate_platform_containment`) and the per-command
    /// `evaluate` both bind the courier.
    #[must_use]
    pub fn platform(&self) -> DiffDrivePlatform<KirraGovernor> {
        self.platform_with(self.angular_governor())
    }

    /// [`Self::platform`] with a caller-supplied governor — the seam for
    /// choosing the governor's [`parko_kirra::RssFeed`] mode. `platform()`
    /// keeps the fail-closed unfed default (no motion until the integrator
    /// feeds an RSS verdict or explicitly declares external gating on the
    /// governor it passes here).
    #[must_use]
    pub fn platform_with(&self, governor: KirraGovernor) -> DiffDrivePlatform<KirraGovernor> {
        DiffDrivePlatform::new(
            governor,
            self.footprint(),
            self.max_speed_mps,
            self.max_brake_mps2,
            self.stop_epsilon_mps,
        )
    }
}

// SAFETY: SG3 SG8 | REQ: courier-diffdrive-live-checker | TEST: courier_reference_uses_the_sotif_record,courier_envelope_is_wider_than_the_uncharacterized_default,courier_admits_in_place_rotation_within_the_bound,courier_clamps_excessive_in_place_rotation,courier_platform_is_bounded_by_the_generic_containment_seam
#[cfg(test)]
mod tests {
    use super::*;
    use kirra_core::platform_kinematics::{PlatformKinematics, PlatformVerdict};
    use parko_core::commands::ControlCommand;
    use parko_core::safety::{EnforcementAction, SafetyPosture};
    use parko_kirra::platform::DiffDriveState;
    use parko_kirra::AngularVelocityBound;

    fn nominal_state() -> DiffDriveState {
        DiffDriveState {
            previous: None,
            delta_time_s: 0.1,
            posture: SafetyPosture::Nominal,
        }
    }

    /// The profile IS parko's SOTIF record — no drift between the live checker
    /// and the model of record, and the footprint matches the SDK courier.
    #[test]
    fn courier_reference_uses_the_sotif_record() {
        let p = CourierPlatformProfile::courier_reference();
        assert_eq!(
            p.angular_params,
            PlatformParams::urban_service_robot_reference(),
            "the live courier angular params must BE parko's SOTIF model of record"
        );
        let fp = p.footprint();
        assert_eq!(fp.width_m, 0.6, "footprint matches the SDK courier() 0.6 m");
        assert_eq!(
            fp.length_m, 0.9,
            "footprint matches the SDK courier() 0.9 m"
        );
        assert_eq!(
            fp.wheelbase_m, 0.0,
            "diff-drive center convention (no wheelbase)"
        );
    }

    /// The whole point of Phase 2: the live node must bound on the COURIER
    /// envelope, not `KirraGovernor::new()`'s conservative default. At v=0 the
    /// courier sweep binds at ~0.833 rad/s; the uncharacterized default at
    /// ~0.20 rad/s. If the profile weren't applied, in-place yaw would be
    /// clamped ~4× too tight (the courier couldn't corner).
    #[test]
    fn courier_envelope_is_wider_than_the_uncharacterized_default() {
        let courier = AngularVelocityBound::nominal(
            CourierPlatformProfile::courier_reference().angular_params,
        );
        let default_ = AngularVelocityBound::nominal(PlatformParams::conservative_default());
        assert!(
            (courier.omega_max(0.0) - 0.833).abs() < 1e-2,
            "courier ω_max(0) ≈ 0.833 (sweep binds); got {}",
            courier.omega_max(0.0)
        );
        assert!(
            courier.omega_max(0.0) > default_.omega_max(0.0) + 0.5,
            "courier envelope ({}) must exceed the uncharacterized default ({}) — \
             proves the profile is actually applied to the live bound",
            courier.omega_max(0.0),
            default_.omega_max(0.0)
        );
    }

    /// In-place rotation (v=0) within the courier bound → admitted UNCLAMPED.
    /// This is the maneuver Phase 1 found was silently dropped by the SDK
    /// bicycle model; here the diff-drive checker bounds it natively and lets
    /// a sane rate through.
    #[test]
    fn courier_admits_in_place_rotation_within_the_bound() {
        // Angular-envelope test; RSS is out of scope → externally-gated governor.
        let profile = CourierPlatformProfile::courier_reference();
        let platform = profile.platform_with(profile.angular_governor().with_external_rss_gate());
        let cmd = ControlCommand {
            linear_velocity: 0.0,
            angular_velocity: 0.5,
            timestamp_ms: 0,
        };
        let verdict = platform.evaluate(&cmd, &nominal_state());
        assert!(
            verdict.is_admitted(),
            "in-place yaw within the bound must be admitted"
        );
        assert!(
            matches!(verdict.0, EnforcementAction::Allow),
            "a sane in-place yaw (0.5 < 0.833 rad/s) passes UNCLAMPED; got {:?}",
            verdict.0
        );
    }

    /// In-place rotation above the courier bound → the angular axis is clamped
    /// to ω_max(0) ≈ 0.833 rad/s. The doer proposes a hard spin; the checker
    /// bounds it.
    #[test]
    fn courier_clamps_excessive_in_place_rotation() {
        let profile = CourierPlatformProfile::courier_reference();
        let platform = profile.platform_with(profile.angular_governor().with_external_rss_gate());
        let cmd = ControlCommand {
            linear_velocity: 0.0,
            angular_velocity: 1.5,
            timestamp_ms: 0,
        };
        let verdict = platform.evaluate(&cmd, &nominal_state());
        match verdict.0 {
            EnforcementAction::ClampAngularVelocity(w) => assert!(
                (w.abs() - 0.833).abs() < 1e-2,
                "excessive in-place yaw must clamp to the courier ω_max(0) ≈ 0.833; got {w}"
            ),
            other => panic!("excessive in-place yaw must clamp the angular axis; got {other:?}"),
        }
    }

    /// The courier `DiffDrivePlatform` is bounded by the SAME generic SG2
    /// containment seam that bounds the Ackermann AV — drive-agnostic by
    /// footprint (S-PK1c). Mirrors the parko-kirra proof, here on the live
    /// deployment profile.
    #[test]
    fn courier_platform_is_bounded_by_the_generic_containment_seam() {
        use kirra_core::containment::{Corridor, Point, Pose};
        use kirra_core::frame_integrity::FrameTrust;
        use kirra_core::kinematics_contract::{DenyCode, EnforceAction};
        use kirra_core::platform_kinematics::validate_platform_containment;

        let platform = CourierPlatformProfile::courier_reference().platform();
        let n = 8;
        let dx = 100.0 / (n as f64 - 1.0);
        let left: Vec<Point> = (0..n)
            .map(|i| Point {
                x_m: i as f64 * dx,
                y_m: 1.0,
            })
            .collect();
        let right: Vec<Point> = (0..n)
            .map(|i| Point {
                x_m: i as f64 * dx,
                y_m: -1.0,
            })
            .collect();
        let corridor = Corridor {
            left: &left,
            right: &right,
            confidence: 0.95,
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        };

        let centered = vec![Pose {
            x_m: 50.0,
            y_m: 0.0,
            heading_rad: 0.0,
        }];
        assert!(
            matches!(
                validate_platform_containment(&platform, &centered, &corridor, FrameTrust::Trusted),
                EnforceAction::Allow
            ),
            "a 0.6 m courier fits a 2 m corridor centered"
        );

        let off = vec![Pose {
            x_m: 50.0,
            y_m: 0.9,
            heading_rad: 0.0,
        }];
        assert!(
            matches!(
                validate_platform_containment(&platform, &off, &corridor, FrameTrust::Trusted),
                EnforceAction::DenyBreach(DenyCode::DrivableSpaceDeparture)
            ),
            "the same courier departs when shoved to the corridor edge — same SG2 seam"
        );
    }
}
