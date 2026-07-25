//! Object-goal resolution — "drive to the red cup" → a GOAL POINT.
//!
//! # The separation this module exists to enforce
//!
//! A camera contributes two things, and conflating them would be a safety bug:
//!
//! 1. **Drivability** (`SemanticClass` / [`crate::clip_corridor_to_hazards`]) —
//!    "may the ego drive here?" That fusion is **TIGHTEN-ONLY**: a detection can
//!    shorten the drivable corridor, never extend it. Lidar (Phase A) remains the
//!    sole authority on free space.
//! 2. **Where a NAMED THING is** (this module) — "the red cup is 2.4 m ahead,
//!    0.3 m left." That is a **destination**, not a claim about the ground.
//!
//! 🔴 **A resolved object goal asserts NOTHING about drivability.** It is only a
//! point the operator asked for. The path there is adjudicated exactly as any
//! other goal: Occy plans inside the lidar corridor and the KIRRA checker bounds
//! the trajectory, so a visible cup behind an obstacle produces a robot that
//! **drives as far as is safe and stops short** — never one that drives at the cup
//! because the camera "saw" it. That is why this returns a bare point and the
//! caller emits an ordinary `MickIntent::GoTo`: no new intent variant, no new
//! authority, no change to the fail-closed intent parse, and the single-door
//! invariant (text → `/intent` → checker) is untouched.
//!
//! Fail-closed throughout: an unnamed / unmatched / stale / non-finite / ambiguous
//! target yields **no goal**, and the caller then speaks instead of driving.

/// A camera-detected, LABELLED thing the operator can name — the goal channel's
/// input. Distinct from [`crate::SemanticDetection`] (the drivability channel) on
/// purpose: this carries a free-text `label` and makes no drivability claim.
#[derive(Debug, Clone, PartialEq)]
pub struct LabeledTarget {
    /// Detector label, e.g. `"red cup"`, `"cup"`, `"person"`. Matched
    /// case-insensitively against the operator's requested phrase.
    pub label: String,
    /// Centroid in the ego frame: +X forward, +Y left (metres) — the SAME frame
    /// the lidar scan and corridor use (see the module docs on `crate`).
    pub x_m: f64,
    pub y_m: f64,
    /// Detector confidence in `[0, 1]`.
    pub confidence: f32,
}

impl LabeledTarget {
    fn is_finite(&self) -> bool {
        self.x_m.is_finite() && self.y_m.is_finite() && self.confidence.is_finite()
    }
}

/// Why no goal was produced. Every arm is a REFUSAL to drive — the caller speaks
/// the reason instead of moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalRefusal {
    /// The operator's phrase named nothing we can match.
    NoRequestedLabel,
    /// No detection matched the requested label.
    NotSeen,
    /// Matches exist but none clears `min_confidence`.
    LowConfidence,
    /// Two or more equally-good matches — "which cup?" is ambiguous, so refuse
    /// rather than silently pick one.
    Ambiguous,
    /// The camera frame is older than the freshness budget (or absent): a stale
    /// sighting is not a present location.
    Stale,
    /// A matching target carried non-finite geometry.
    NonFinite,
    /// The match is behind the ego (`x <= 0`): this resolver only produces FORWARD
    /// goals, so a behind-the-robot target refuses rather than commanding a
    /// reverse manoeuvre nothing here is bounded for.
    BehindEgo,
}

/// A resolved goal: a forward ego-frame point the caller turns into
/// `MickIntent::GoTo { x_m, y_m }`. Carries the matched label + confidence purely
/// for narration ("heading for the red cup, 2.4 m ahead").
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectGoal {
    pub label: String,
    pub x_m: f64,
    pub y_m: f64,
    pub confidence: f32,
}

/// Freshness budget for the goal channel, ms. A sighting older than this is not a
/// present location (the robot may have moved, or the object may have).
pub const DEFAULT_GOAL_MAX_AGE_MS: u64 = 750;

/// Minimum detector confidence for a target to be drivable-to as a GOAL.
pub const DEFAULT_MIN_CONFIDENCE: f32 = 0.40;

/// Does `label` satisfy the operator's `requested` phrase?
///
/// Deliberately simple + predictable (no fuzzy matching): every whitespace token
/// of the request must appear as a substring of the label. So `"red cup"` matches
/// `"red cup"` and `"large red cup"`, but not `"blue cup"` (no `red`) and not
/// `"cup"` alone (no `red`) — asking for a *red* cup must not drive to a blue one.
/// A bare `"cup"` request matches any cup.
#[must_use]
pub fn label_matches(label: &str, requested: &str) -> bool {
    let hay = label.to_ascii_lowercase();
    let mut any = false;
    for tok in requested.to_ascii_lowercase().split_whitespace() {
        any = true;
        if !hay.contains(tok) {
            return false;
        }
    }
    any // an empty request matches nothing
}

/// Resolve the operator's requested object to a forward goal point, or refuse.
///
/// `now_ms` / `frame_stamp_ms` gate freshness; `frame_stamp_ms: None` (no camera
/// frame) is [`GoalRefusal::Stale`] — "the detector did not look" is never "it
/// isn't there". Among fresh, confident, forward matches the NEAREST wins; a tie
/// within `tie_epsilon_m` is [`GoalRefusal::Ambiguous`].
///
/// The returned point is a DESTINATION ONLY — see the module docs. It never
/// implies the path there is clear.
pub fn resolve_object_goal(
    requested: &str,
    targets: &[LabeledTarget],
    frame_stamp_ms: Option<u64>,
    now_ms: u64,
    max_age_ms: u64,
    min_confidence: f32,
    tie_epsilon_m: f64,
) -> Result<ObjectGoal, GoalRefusal> {
    if requested.trim().is_empty() {
        return Err(GoalRefusal::NoRequestedLabel);
    }
    // Freshness first: a stale/absent frame refuses regardless of contents.
    let Some(stamp) = frame_stamp_ms else {
        return Err(GoalRefusal::Stale);
    };
    if stamp > now_ms.saturating_add(max_age_ms) || now_ms.saturating_sub(stamp) > max_age_ms {
        return Err(GoalRefusal::Stale); // stale, or implausibly future-stamped
    }

    let matched: Vec<&LabeledTarget> = targets
        .iter()
        .filter(|t| label_matches(&t.label, requested))
        .collect();
    if matched.is_empty() {
        return Err(GoalRefusal::NotSeen);
    }
    // A matching target with garbage geometry is a fault, not something to skip.
    if matched.iter().any(|t| !t.is_finite()) {
        return Err(GoalRefusal::NonFinite);
    }
    let confident: Vec<&&LabeledTarget> = matched
        .iter()
        .filter(|t| t.confidence >= min_confidence)
        .collect();
    if confident.is_empty() {
        return Err(GoalRefusal::LowConfidence);
    }
    let forward: Vec<&&&LabeledTarget> = confident.iter().filter(|t| t.x_m > 0.0).collect();
    if forward.is_empty() {
        return Err(GoalRefusal::BehindEgo);
    }

    let range = |t: &LabeledTarget| (t.x_m * t.x_m + t.y_m * t.y_m).sqrt();
    let best = forward
        .iter()
        .copied()
        .min_by(|a, b| range(a).total_cmp(&range(b)))
        .expect("non-empty");
    // Ambiguity: another DISTINCT match essentially as near as the winner.
    let ties = forward
        .iter()
        .filter(|t| (range(t) - range(best)).abs() <= tie_epsilon_m)
        .count();
    if ties > 1 {
        return Err(GoalRefusal::Ambiguous);
    }
    Ok(ObjectGoal {
        label: best.label.clone(),
        x_m: best.x_m,
        y_m: best.y_m,
        confidence: best.confidence,
    })
}

/// One operator-facing sentence for a refusal — Channel-A narration, so the robot
/// SAYS why it isn't driving instead of silently holding.
#[must_use]
pub fn refusal_sentence(requested: &str, why: GoalRefusal) -> String {
    match why {
        GoalRefusal::NoRequestedLabel => "I didn't catch what I should drive to.".to_string(),
        GoalRefusal::NotSeen => format!("I can't see {requested} from here, so I'm staying put."),
        GoalRefusal::LowConfidence => {
            format!("I might be seeing {requested}, but not clearly enough to drive to it.")
        }
        GoalRefusal::Ambiguous => {
            format!("I can see more than one {requested} — which one did you mean?")
        }
        GoalRefusal::Stale => "My camera view is stale, so I won't drive on it.".to_string(),
        GoalRefusal::NonFinite => {
            format!("My reading of {requested} doesn't make sense, so I'm not moving.")
        }
        GoalRefusal::BehindEgo => {
            format!("{requested} is behind me, and I only drive forward to a target.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(label: &str, x: f64, y: f64, c: f32) -> LabeledTarget {
        LabeledTarget {
            label: label.into(),
            x_m: x,
            y_m: y,
            confidence: c,
        }
    }

    fn resolve(req: &str, targets: &[LabeledTarget]) -> Result<ObjectGoal, GoalRefusal> {
        resolve_object_goal(
            req,
            targets,
            Some(1_000),
            1_000,
            DEFAULT_GOAL_MAX_AGE_MS,
            DEFAULT_MIN_CONFIDENCE,
            0.25,
        )
    }

    // ---- label matching -----------------------------------------------------

    #[test]
    fn label_matching_requires_every_requested_token() {
        assert!(label_matches("red cup", "red cup"));
        assert!(label_matches("large red cup", "red cup")); // extra words fine
        assert!(label_matches("Red Cup", "red cup")); // case-insensitive
        assert!(label_matches("red cup", "cup")); // generic request
                                                  // The load-bearing negative: asking for a RED cup must not match a blue one.
        assert!(!label_matches("blue cup", "red cup"));
        assert!(!label_matches("cup", "red cup"));
        assert!(!label_matches("red cup", "")); // empty request matches nothing
    }

    // ---- the happy path ("drive to the red cup") ---------------------------

    #[test]
    fn resolves_the_named_target_to_a_forward_goal() {
        let g = resolve("red cup", &[t("red cup", 2.4, 0.3, 0.9)]).expect("goal");
        assert_eq!(g.label, "red cup");
        assert!((g.x_m - 2.4).abs() < 1e-9 && (g.y_m - 0.3).abs() < 1e-9);
    }

    #[test]
    fn nearest_match_wins_when_unambiguous() {
        // Two cups, clearly different ranges → the near one, no ambiguity.
        let g = resolve("cup", &[t("cup", 5.0, 0.0, 0.9), t("cup", 1.5, 0.0, 0.9)]).expect("goal");
        assert!((g.x_m - 1.5).abs() < 1e-9);
    }

    #[test]
    fn ignores_a_different_coloured_cup() {
        // A blue cup present, a red one requested and present → picks red even
        // though blue is nearer.
        let g = resolve(
            "red cup",
            &[t("blue cup", 1.0, 0.0, 0.9), t("red cup", 3.0, 0.2, 0.9)],
        )
        .expect("goal");
        assert_eq!(g.label, "red cup");
    }

    // ---- every refusal is fail-closed --------------------------------------

    #[test]
    fn refuses_when_not_seen() {
        assert_eq!(
            resolve("red cup", &[t("chair", 2.0, 0.0, 0.9)]),
            Err(GoalRefusal::NotSeen)
        );
        assert_eq!(resolve("red cup", &[]), Err(GoalRefusal::NotSeen));
    }

    #[test]
    fn refuses_a_low_confidence_sighting() {
        assert_eq!(
            resolve("red cup", &[t("red cup", 2.0, 0.0, 0.1)]),
            Err(GoalRefusal::LowConfidence)
        );
    }

    #[test]
    fn refuses_two_equally_near_matches() {
        assert_eq!(
            resolve("cup", &[t("cup", 2.0, 0.0, 0.9), t("cup", 2.05, 0.1, 0.9)]),
            Err(GoalRefusal::Ambiguous)
        );
    }

    #[test]
    fn refuses_a_target_behind_the_ego() {
        assert_eq!(
            resolve("red cup", &[t("red cup", -2.0, 0.0, 0.9)]),
            Err(GoalRefusal::BehindEgo)
        );
    }

    #[test]
    fn refuses_non_finite_geometry() {
        assert_eq!(
            resolve("red cup", &[t("red cup", f64::NAN, 0.0, 0.9)]),
            Err(GoalRefusal::NonFinite)
        );
        assert_eq!(
            resolve("red cup", &[t("red cup", f64::INFINITY, 0.0, 0.9)]),
            Err(GoalRefusal::NonFinite)
        );
    }

    #[test]
    fn refuses_an_absent_or_stale_camera_frame() {
        let good = [t("red cup", 2.0, 0.0, 0.9)];
        // No frame at all — "the detector did not look" is never "it isn't there".
        assert_eq!(
            resolve_object_goal("red cup", &good, None, 1_000, 750, 0.4, 0.25),
            Err(GoalRefusal::Stale)
        );
        // Frame far older than the budget.
        assert_eq!(
            resolve_object_goal("red cup", &good, Some(100), 5_000, 750, 0.4, 0.25),
            Err(GoalRefusal::Stale)
        );
        // Implausibly FUTURE-stamped (non-monotonic clock) is stale too.
        assert_eq!(
            resolve_object_goal("red cup", &good, Some(90_000), 1_000, 750, 0.4, 0.25),
            Err(GoalRefusal::Stale)
        );
    }

    #[test]
    fn refuses_an_empty_request() {
        assert_eq!(
            resolve("   ", &[t("red cup", 2.0, 0.0, 0.9)]),
            Err(GoalRefusal::NoRequestedLabel)
        );
    }

    // ---- narration ----------------------------------------------------------

    #[test]
    fn every_refusal_has_an_operator_sentence() {
        for why in [
            GoalRefusal::NoRequestedLabel,
            GoalRefusal::NotSeen,
            GoalRefusal::LowConfidence,
            GoalRefusal::Ambiguous,
            GoalRefusal::Stale,
            GoalRefusal::NonFinite,
            GoalRefusal::BehindEgo,
        ] {
            let s = refusal_sentence("the red cup", why);
            assert!(!s.is_empty() && s.ends_with(['.', '?']), "{why:?} -> {s:?}");
        }
    }
}
