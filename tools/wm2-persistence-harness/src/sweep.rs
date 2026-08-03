//! The scale sweep — ADR-0041's own reopening condition, made decidable.
//!
//! ADR-0041 states the condition under which it should be abandoned:
//!
//! > *"if measurement shows entities in the millions or genuinely unbounded
//! > ad-hoc traversal, Option B becomes materially stronger and this ADR should
//! > be reopened."*
//!
//! The drill previously discharged that with a shell `for` loop and the
//! instruction to "look at how `graph_bounded_reach` grows". That is not a
//! gate. Whether a curve has a knee is exactly the kind of judgement that gets
//! made favourably by someone who wants to ship, and re-made unfavourably by an
//! assessor a year later, from the same numbers.
//!
//! So the judgement is a function here, and it is fail-closed: too few points,
//! or points that do not admit a conclusion, produce `Insufficient` — never
//! "no knee found".
//!
//! # Why log-log slope
//!
//! The families being swept are expected to be near-linear in entity count if
//! the substrate is adequate (`graph_bounded_reach` fans out over a bounded
//! depth; `temporal_changes_since` scans a window). A knee is a *change in
//! exponent*, which is a change in slope on a log-log plot and nothing simpler.
//! Comparing raw ratios would flag ordinary linear growth as a problem, and a
//! sweep that cries wolf gets ignored.
//!
//! # What this cannot do
//!
//! It measures the harness's stand-in schema against synthetic load. What
//! entity counts a deployed robot actually reaches is an operational fact no
//! benchmark produces, and the ADR says so. This narrows the question from
//! "does SQLite scale" to "does it scale *here, on this shape, to this point*" —
//! which is the answerable half.

/// One measured point on the ladder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepPoint {
    pub entities: u64,
    /// Median microseconds for the family being classified.
    pub median_us: f64,
}

/// The verdict against ADR-0041's reopening condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalingVerdict {
    /// Cost grows slower than entity count. Supports Option A.
    Sublinear(String),
    /// Cost grows about proportionally. Supports Option A.
    Linear(String),
    /// Uniformly steeper than linear, but with no inflection — expensive and
    /// predictable. A design constraint, not a reopening trigger.
    Superlinear(String),
    /// The exponent *increases* along the ladder. This is the shape the ADR's
    /// reopening condition describes, and it is the one that must not be
    /// explained away.
    Knee(String),
    /// Not enough evidence to say. Fails the run rather than passing quietly.
    Insufficient(String),
}

impl ScalingVerdict {
    pub fn token(&self) -> &'static str {
        match self {
            Self::Sublinear(_) => "SUBLINEAR",
            Self::Linear(_) => "LINEAR",
            Self::Superlinear(_) => "SUPERLINEAR",
            Self::Knee(_) => "KNEE",
            Self::Insufficient(_) => "INSUFFICIENT",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Sublinear(d)
            | Self::Linear(d)
            | Self::Superlinear(d)
            | Self::Knee(d)
            | Self::Insufficient(d) => d,
        }
    }

    /// Whether this verdict should fail the run.
    ///
    /// `Insufficient` counts, for the same reason an inconclusive crash tier
    /// does: a gate that cannot be evaluated has not been passed. `Knee` also
    /// counts — it is a real finding, and a sweep that reports a knee and exits
    /// 0 invites the finding to be skimmed past.
    pub fn fails_the_run(&self) -> bool {
        matches!(self, Self::Insufficient(_) | Self::Knee(_))
    }

    /// Whether this verdict supports keeping Option A (SQLite).
    pub fn supports_option_a(&self) -> bool {
        matches!(self, Self::Sublinear(_) | Self::Linear(_))
    }
}

/// Minimum ladder length. Two points define a slope; detecting a *change* in
/// slope needs at least three.
pub const MIN_SWEEP_POINTS: usize = 3;

/// Slope at or below this is treated as sublinear.
const SUBLINEAR_MAX: f64 = 0.9;
/// Slope above this is steeper than proportional.
const LINEAR_MAX: f64 = 1.2;
/// How much steeper the last segment must be than the first to count as a knee.
const KNEE_RATIO: f64 = 1.5;
/// A knee also requires the late slope to be genuinely superlinear; a curve
/// that goes from 0.2 to 0.4 has "doubled its exponent" and is still cheap.
const KNEE_MIN_LATE_SLOPE: f64 = 1.2;
/// Overall slope below this means cost *fell* substantially as entities rose.
///
/// That is not a scaling result — it is a ladder that diluted. See
/// [`classify_scaling`] for why it must be refused rather than reported as
/// excellent sublinear scaling.
const DILUTION_SLOPE: f64 = -0.5;

/// Log-log slope between two points: d(log cost) / d(log entities).
///
/// Returns `None` when either coordinate is non-positive or the entity counts
/// coincide — cases where the slope is undefined rather than zero. Treating
/// them as zero would report flat scaling from unusable data, which is the
/// wrong direction to be wrong in.
pub fn segment_slope(a: SweepPoint, b: SweepPoint) -> Option<f64> {
    if a.entities == 0 || b.entities == 0 || a.median_us <= 0.0 || b.median_us <= 0.0 {
        return None;
    }
    if a.entities == b.entities {
        return None;
    }
    let dx = (b.entities as f64).ln() - (a.entities as f64).ln();
    let dy = b.median_us.ln() - a.median_us.ln();
    if dx == 0.0 || !dy.is_finite() || !dx.is_finite() {
        return None;
    }
    Some(dy / dx)
}

/// Classify a ladder against ADR-0041's reopening condition.
///
/// Points are expected in ascending entity order; they are not sorted here,
/// because an unsorted ladder means the caller measured something other than
/// what it thinks, and silently sorting would hide that.
pub fn classify_scaling(points: &[SweepPoint]) -> ScalingVerdict {
    if points.len() < MIN_SWEEP_POINTS {
        return ScalingVerdict::Insufficient(format!(
            "only {} point(s); {MIN_SWEEP_POINTS} are needed, because two points define a \
             slope and detecting a CHANGE in slope needs three",
            points.len()
        ));
    }
    if points.windows(2).any(|w| w[1].entities <= w[0].entities) {
        return ScalingVerdict::Insufficient(
            "ladder is not strictly ascending in entity count; the sweep measured something \
             other than a scale ladder"
                .into(),
        );
    }

    let slopes: Vec<f64> = points
        .windows(2)
        .filter_map(|w| segment_slope(w[0], w[1]))
        .collect();
    if slopes.len() + 1 < MIN_SWEEP_POINTS {
        return ScalingVerdict::Insufficient(
            "too many segments had undefined slope (zero or negative timings) to classify".into(),
        );
    }

    let first = slopes[0];
    let last = slopes[slopes.len() - 1];
    let overall =
        segment_slope(points[0], points[points.len() - 1]).expect("endpoints validated above");

    // The knee test runs FIRST. A curve can have a benign overall slope and
    // still bend sharply at the top of the ladder, and the top of the ladder is
    // the part that predicts the next order of magnitude.
    if last >= KNEE_MIN_LATE_SLOPE && first > 0.0 && last >= first * KNEE_RATIO {
        return ScalingVerdict::Knee(format!(
            "exponent INCREASES along the ladder: first segment {first:.2}, last segment \
             {last:.2} ({:.1}x steeper), overall {overall:.2}. This is the shape ADR-0041's \
             reopening condition describes — Option B becomes materially stronger and the \
             ADR should be reopened rather than this being tuned around.",
            last / first
        ));
    }

    // A ladder whose cost FALLS substantially as entities rise did not scale —
    // it diluted. This is what happens when entity count is swept at a fixed
    // total event count: events-per-entity drops, fan-out drops with it, and
    // the query gets cheaper. Measured on a host ladder of 100/1000/10000
    // entities at a fixed 20 000 events: 1687 µs → 73.7 µs → 10.9 µs, slope
    // −1.10.
    //
    // Reporting that as SUBLINEAR would close ADR-0041's scale gate with a
    // result that says nothing about scale — the most dangerous outcome
    // available here, because it looks like unusually good news.
    if overall < DILUTION_SLOPE {
        return ScalingVerdict::Insufficient(format!(
            "cost FELL steeply across the ladder (overall log-log slope {overall:.2}). The \
             ladder diluted rather than scaled: holding total events fixed while raising \
             entity count reduces events-per-entity, so fan-out — and cost — go DOWN. This \
             measures density, not scale, and cannot answer ADR-0041's reopening condition. \
             Re-run holding events-per-entity constant (`--events-per-entity`)."
        ));
    }

    if overall <= SUBLINEAR_MAX {
        ScalingVerdict::Sublinear(format!(
            "overall log-log slope {overall:.2} (segments {first:.2}..{last:.2}) — cost grows \
             slower than entity count; supports Option A"
        ))
    } else if overall <= LINEAR_MAX {
        ScalingVerdict::Linear(format!(
            "overall log-log slope {overall:.2} (segments {first:.2}..{last:.2}) — about \
             proportional to entity count; supports Option A"
        ))
    } else {
        ScalingVerdict::Superlinear(format!(
            "overall log-log slope {overall:.2} (segments {first:.2}..{last:.2}) — steeper \
             than proportional but with no inflection. Expensive and PREDICTABLE: a design \
             constraint on how far this may be scaled, not a reopening trigger on its own"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder(pts: &[(u64, f64)]) -> Vec<SweepPoint> {
        pts.iter()
            .map(|&(entities, median_us)| SweepPoint {
                entities,
                median_us,
            })
            .collect()
    }

    #[test]
    fn two_points_cannot_show_a_knee_and_must_not_pass() {
        let v = classify_scaling(&ladder(&[(1_000, 10.0), (10_000, 100.0)]));
        assert_eq!(v.token(), "INSUFFICIENT");
        assert!(v.fails_the_run());
        assert!(!v.supports_option_a());
    }

    #[test]
    fn an_empty_ladder_is_insufficient_not_sublinear() {
        assert_eq!(classify_scaling(&[]).token(), "INSUFFICIENT");
    }

    #[test]
    fn proportional_growth_is_linear_and_supports_option_a() {
        // 10x entities -> 10x cost at every step.
        let v = classify_scaling(&ladder(&[
            (1_000, 10.0),
            (10_000, 100.0),
            (100_000, 1_000.0),
        ]));
        assert_eq!(v.token(), "LINEAR", "{}", v.detail());
        assert!(v.supports_option_a());
        assert!(!v.fails_the_run());
    }

    #[test]
    fn a_diluting_ladder_is_refused_rather_than_praised_as_sublinear() {
        // The real host measurement that exposed this: sweeping entity count at
        // a FIXED total event count makes each entity thinner, so the graph
        // query gets cheaper. Slope −1.10. Calling that SUBLINEAR would close
        // the scale gate with a number that says nothing about scale.
        let v = classify_scaling(&ladder(&[(100, 1_687.0), (1_000, 73.7), (10_000, 10.9)]));
        assert_eq!(v.token(), "INSUFFICIENT", "{}", v.detail());
        assert!(v.fails_the_run());
        assert!(!v.supports_option_a());
        assert!(v.detail().contains("diluted"), "{}", v.detail());
        assert!(
            v.detail().contains("events-per-entity"),
            "must say how to fix it: {}",
            v.detail()
        );
    }

    #[test]
    fn a_gently_falling_curve_is_still_sublinear_not_dilution() {
        // The guard must not swallow genuine mild improvement — e.g. better
        // index locality — or it becomes a second way to fail honestly.
        let v = classify_scaling(&ladder(&[(1_000, 100.0), (10_000, 80.0), (100_000, 64.0)]));
        assert_eq!(v.token(), "SUBLINEAR", "{}", v.detail());
        assert!(v.supports_option_a());
    }

    #[test]
    fn flat_cost_is_sublinear() {
        let v = classify_scaling(&ladder(&[(1_000, 50.0), (10_000, 55.0), (100_000, 60.0)]));
        assert_eq!(v.token(), "SUBLINEAR", "{}", v.detail());
        assert!(v.supports_option_a());
    }

    #[test]
    fn a_genuine_knee_is_caught_and_fails_the_run() {
        // Flat, flat, then an explosion — the shape that must not be explained
        // away as "it is only the last point".
        let v = classify_scaling(&ladder(&[
            (1_000, 10.0),
            (10_000, 12.0),
            (100_000, 2_000.0),
        ]));
        assert_eq!(v.token(), "KNEE", "{}", v.detail());
        assert!(v.fails_the_run(), "a knee must not exit 0");
        assert!(!v.supports_option_a());
        assert!(v.detail().contains("reopen"), "{}", v.detail());
    }

    #[test]
    fn uniform_superlinear_growth_is_not_called_a_knee() {
        // Quadratic throughout: expensive, but predictable, and the ADR's
        // reopening condition is about an INFLECTION rather than a steep line.
        let v = classify_scaling(&ladder(&[
            (1_000, 10.0),
            (10_000, 1_000.0),
            (100_000, 100_000.0),
        ]));
        assert_eq!(v.token(), "SUPERLINEAR", "{}", v.detail());
        assert!(!v.fails_the_run());
        assert!(!v.supports_option_a());
    }

    #[test]
    fn a_cheap_curve_that_doubles_its_exponent_is_not_a_knee() {
        // 0.2 -> 0.4 doubles the exponent while staying far below linear.
        // Flagging this would make the sweep cry wolf, and a gate that cries
        // wolf gets ignored — which costs more than the false negative.
        let pts = ladder(&[
            (1_000, 10.0),
            (10_000, 10.0 * 10f64.powf(0.2)),
            (100_000, 10.0 * 10f64.powf(0.2) * 10f64.powf(0.4)),
        ]);
        let v = classify_scaling(&pts);
        assert_ne!(v.token(), "KNEE", "{}", v.detail());
        assert!(v.supports_option_a(), "{}", v.detail());
    }

    #[test]
    fn a_descending_ladder_is_refused_rather_than_silently_sorted() {
        let v = classify_scaling(&ladder(&[(100_000, 10.0), (10_000, 20.0), (1_000, 30.0)]));
        assert_eq!(v.token(), "INSUFFICIENT");
        assert!(v.detail().contains("ascending"));
    }

    #[test]
    fn a_repeated_entity_count_is_refused() {
        let v = classify_scaling(&ladder(&[(1_000, 10.0), (1_000, 20.0), (10_000, 30.0)]));
        assert_eq!(v.token(), "INSUFFICIENT");
    }

    #[test]
    fn zero_timings_do_not_read_as_flat_scaling() {
        // A family that matched nothing would time at ~0. Reporting that as
        // SUBLINEAR would be the benchmark equivalent of dividing by zero and
        // calling the answer good news.
        let v = classify_scaling(&ladder(&[(1_000, 0.0), (10_000, 0.0), (100_000, 0.0)]));
        assert_eq!(v.token(), "INSUFFICIENT", "{}", v.detail());
    }

    #[test]
    fn segment_slope_is_undefined_rather_than_zero_on_bad_input() {
        let a = SweepPoint {
            entities: 0,
            median_us: 1.0,
        };
        let b = SweepPoint {
            entities: 10,
            median_us: 1.0,
        };
        assert!(segment_slope(a, b).is_none());
        assert!(segment_slope(b, a).is_none());

        let neg = SweepPoint {
            entities: 100,
            median_us: -1.0,
        };
        assert!(segment_slope(b, neg).is_none());
    }

    #[test]
    fn every_verdict_token_is_distinct() {
        let tokens = [
            ScalingVerdict::Sublinear(String::new()).token(),
            ScalingVerdict::Linear(String::new()).token(),
            ScalingVerdict::Superlinear(String::new()).token(),
            ScalingVerdict::Knee(String::new()).token(),
            ScalingVerdict::Insufficient(String::new()).token(),
        ];
        let unique: std::collections::HashSet<_> = tokens.iter().collect();
        assert_eq!(unique.len(), 5);
    }

    #[test]
    fn the_measured_jetson_point_is_not_by_itself_a_ladder() {
        // The merged run measured ONE entity count (1000). The scale gate is
        // open precisely because a single point cannot be classified, and this
        // pins that so nobody closes the gate from the existing evidence.
        let v = classify_scaling(&ladder(&[(1_000, 11_536.212)]));
        assert_eq!(v.token(), "INSUFFICIENT");
        assert!(v.fails_the_run());
    }
}
