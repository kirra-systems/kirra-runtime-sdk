//! Lane-line crossing rules — the LATERAL behavioral constraint (de-monolith Stage 6b:
//! relocated verbatim from the planner's `behavior` module).
//!
//! Lane markings govern *when you may cross laterally* (the planner's behavioral layer
//! governs *when you stop / how fast*). Like signs, these are LEGAL, not physical — you
//! *can* drive across a solid line; doing so is unlawful, and its collision shadow
//! (oncoming traffic) is still KIRRA's RSS. So the rule gates the lateral-avoidance
//! maneuver (route-around / lane offset).
//!
//! Boundaries are given as lateral offsets from the path centerline (+y left).
//! Full typed boundaries with real positions ride on a lane map ([`crate::lanemap`]);
//! this is the rule logic + the route-around gate.

/// A lane-marking type and its crossing permission (per the rules of the road).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineType {
    /// Broken / dashed — crossing permitted either direction (when safe).
    Broken,
    /// Single solid — no crossing either direction.
    Solid,
    /// Double solid — no crossing either direction.
    DoubleSolid,
    /// Combined (solid + broken) with the **broken** marking facing the +y (left)
    /// side: a vehicle on the +y side may cross; the -y (solid) side may not.
    BrokenOnLeft,
    /// Combined with the broken marking facing the -y (right) side: the -y side
    /// may cross; the +y (solid) side may not.
    BrokenOnRight,
    /// **Unmarked** — no painted line at all (an undivided road / dirt road
    /// centerline). Crossing is *permitted* either direction (like [`Broken`]),
    /// because the law does not forbid using the other half to pass when clear.
    /// The absence of paint does NOT remove the rule of the road, though: the
    /// **keep-right positional default** is a separate concern, modeled
    /// structurally by placing the ego's lane on the right half of the road (see
    /// `lanemap::LaneGraph::from_undivided_corridor`), not by this crossing flag.
    ///
    /// [`Broken`]: LineType::Broken
    Unmarked,
}

/// A lane boundary at lateral offset `y_m` from the path centerline (+y left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneBoundary {
    pub y_m: f64,
    pub line: LineType,
}

impl LaneBoundary {
    /// May the ego cross this boundary moving laterally from `from_y` to `to_y`?
    /// A move that does not cross `y_m` is unconstrained (returns `true`).
    #[must_use]
    pub fn may_cross(&self, from_y: f64, to_y: f64) -> bool {
        let from_side = from_y - self.y_m;
        let to_side = to_y - self.y_m;
        // Not crossing this line (same side, or starting exactly on it) → allowed.
        if from_side.abs() <= 1e-9 || from_side.signum() == to_side.signum() {
            return true;
        }
        match self.line {
            // Permissive: dashed paint, or no paint at all (undivided road).
            LineType::Broken | LineType::Unmarked => true,
            LineType::Solid | LineType::DoubleSolid => false,
            // Combined: only the side the broken marking faces may cross.
            LineType::BrokenOnLeft => from_side > 0.0,
            LineType::BrokenOnRight => from_side < 0.0,
        }
    }
}

/// Is a lateral move from `from_y` to `to_y` permitted by ALL lane boundaries?
#[must_use]
pub fn lateral_move_permitted(boundaries: &[LaneBoundary], from_y: f64, to_y: f64) -> bool {
    boundaries.iter().all(|b| b.may_cross(from_y, to_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_line_allows_crossing_solid_forbids() {
        let broken = LaneBoundary { y_m: -0.5, line: LineType::Broken };
        assert!(broken.may_cross(0.0, -1.5), "broken: crossing OK");
        let solid = LaneBoundary { y_m: -0.5, line: LineType::Solid };
        assert!(!solid.may_cross(0.0, -1.5), "solid: no crossing");
        let double = LaneBoundary { y_m: 0.5, line: LineType::DoubleSolid };
        assert!(!double.may_cross(0.0, 2.0), "double solid: no crossing");
    }

    #[test]
    fn unmarked_line_allows_crossing_like_broken() {
        // No paint (undivided / dirt road centerline) → crossing permitted either
        // way, same as a broken line. (Keep-right is a positional default handled
        // elsewhere, not a crossing prohibition.)
        let unmarked = LaneBoundary { y_m: 0.0, line: LineType::Unmarked };
        assert!(unmarked.may_cross(-1.0, 1.0), "unmarked: cross left OK");
        assert!(unmarked.may_cross(1.0, -1.0), "unmarked: cross right OK");
    }

    #[test]
    fn not_crossing_a_line_is_unconstrained() {
        // Move stays on one side of the line → allowed even for a solid line.
        let solid = LaneBoundary { y_m: -3.0, line: LineType::Solid };
        assert!(solid.may_cross(0.0, -1.5), "did not reach the line → OK");
    }

    #[test]
    fn combined_line_crosses_from_the_broken_side_only() {
        // Broken faces +y: a vehicle on the +y side may cross down; the -y side may not.
        let bl = LaneBoundary { y_m: 0.0, line: LineType::BrokenOnLeft };
        assert!(bl.may_cross(1.0, -1.0), "+y (broken) side may cross");
        assert!(!bl.may_cross(-1.0, 1.0), "-y (solid) side may NOT cross");
        // Mirror image.
        let br = LaneBoundary { y_m: 0.0, line: LineType::BrokenOnRight };
        assert!(br.may_cross(-1.0, 1.0), "-y (broken) side may cross");
        assert!(!br.may_cross(1.0, -1.0), "+y (solid) side may NOT cross");
    }

    #[test]
    fn lateral_move_permitted_requires_all_boundaries() {
        let bounds = [
            LaneBoundary { y_m: -0.5, line: LineType::Broken },
            LaneBoundary { y_m: -1.0, line: LineType::Solid },
        ];
        // Crossing to -1.5 crosses BOTH; the solid one forbids it.
        assert!(!lateral_move_permitted(&bounds, 0.0, -1.5));
        // Crossing only to -0.7 crosses just the broken one → allowed.
        assert!(lateral_move_permitted(&bounds, 0.0, -0.7));
    }
}
