//! #1215 consumer wiring — every deployed geometry value is the canonical
//! contract plus a NAMED policy, and the relationship is asserted, not assumed.
//!
//! # The two wiring shapes, and why both exist
//!
//! The rule for this slice: *replace literals with canonical geometry plus
//! explicit policy, not with opaque generated numbers.* The repo's dependency
//! structure forces that rule into two shapes:
//!
//! - **Construct**, where an import edge exists. `kirra-core::regions` is the
//!   constructive path: a consumer that can see the contract builds its region
//!   from [`kirra_core::regions::PhysicalFootprint`] and a [`Margin`].
//!
//! - **Validate**, where one does not. `kirra-trajectory` cannot import
//!   `contract_for` (the profile family lives in this root crate, above it),
//!   and `kirra_params.yaml` is consumed by Python nodes that cannot import
//!   Rust at all. Those carry the repo's existing **cited-copy discipline**
//!   (CONTRACT_PROFILES.md): the copy is deliberate and cited — and, as of
//!   this module, *checked*. The root crate sees both sides of every such
//!   boundary, so the conformance tests live here.
//!
//! A copy that is cited but unchecked is how the four-way footprint divergence
//! happened (#1215's motivating finding: checker 0.165/0.102, doer 0.17/0.11,
//! contract 0.330×0.203, all claiming the same robot).
//!
//! # The margins, declared once
//!
//! The named policies below are THE declarations of each consumer's expansion
//! over the physical body. The YAML and the checker config are mirrors of
//! `contract + policy`; if either drifts, the tests in `tests.rs` name the
//! value and the policy it violated.

use kirra_core::regions::Margin;

/// The slow-loop checker's collision extents (`kirra_trajectory::VehicleConfig::r2`).
///
/// Longitudinal: none — `half_length_m: 0.165` is exactly `0.330 / 2`.
/// Lateral: `half_width_m: 0.102` sits 0.5 mm over the physical `0.1015`; that
/// was an undeclared rounding-up before this constant existed. Keeping it (and
/// naming it) rather than tightening to the physical value: it is conservative,
/// shipped, and the point of this module is to make expansions DECLARED, not to
/// re-tune them.
pub const CHECKER_R2_COLLISION_MARGIN: Margin = Margin {
    reason: "slow-loop checker rounding headroom: half_width carried 0.102 (vs physical 0.1015) \
             since its introduction; retained and declared rather than silently re-tuned",
    longitudinal_m: 0.0,
    lateral_m: 0.0005,
};

/// The doer's proposal envelope (`kirra_params.yaml` `occy_doer`
/// `half_length_m` / `half_width_m`).
///
/// The YAML says "+ margin" and never said how much. This is how much: 5 mm
/// longitudinal, 8.5 mm lateral over the physical half-extents. Conservative
/// direction — the doer proposes within a larger body than the checker demands,
/// so a proposal that fits the doer's envelope has headroom at the checker.
pub const DOER_R2_PROPOSAL_MARGIN: Margin = Margin {
    reason: "doer proposal envelope: propose within a body larger than the checker demands, \
             so admissible proposals carry headroom at the checker (kirra_params.yaml occy_doer)",
    longitudinal_m: 0.005,
    lateral_m: 0.0085,
};

/// The perception governor's required lateral clearance on EACH side of the
/// body (`kirra_params.yaml` `perception_governor` `lateral_clearance_m`),
/// feeding `required_corridor_width = vehicle_width + 2 · clearance` in the Taj
/// sidecar.
pub const PERCEPTION_R2_CLEARANCE: Margin = Margin {
    reason: "required clearance each side of the body before a corridor admits the ego \
             (kirra_params.yaml perception_governor lateral_clearance_m)",
    longitudinal_m: 0.0,
    lateral_m: 0.15,
};

/// The relative path of the deployed ROS parameter file this module's tests
/// validate. One definition so the tests and any future tooling agree on which
/// file is the deployed one.
pub const R2_PARAMS_YAML: &str = "ros2_ws/src/kirra_safety/config/kirra_params.yaml";

/// Read a `key: value` scalar out of a flat ROS params YAML section.
///
/// Deliberately not a YAML library: the file is flat `key: value  # comment`
/// under two-level indentation, the root crate carries no YAML dependency, and
/// a parser that handled more would invite the config to grow shapes this
/// validation cannot see. Fails loudly (None) on absence so a renamed key
/// cannot silently pass.
#[must_use]
pub fn yaml_scalar(source: &str, section: &str, key: &str) -> Option<f64> {
    let mut in_section = false;
    for line in source.lines() {
        let trimmed = line.trim_end();
        // A section header is a top-level `name:` line.
        if !trimmed.starts_with(' ') && trimmed.ends_with(':') {
            in_section = trimmed.trim_end_matches(':') == section;
            continue;
        }
        if !in_section {
            continue;
        }
        let inner = trimmed.trim_start();
        let Some(rest) = inner.strip_prefix(key) else {
            continue;
        };
        let Some(value_part) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        let value_text = value_part.split('#').next().unwrap_or("").trim();
        if let Ok(v) = value_text.parse::<f64>() {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests;
