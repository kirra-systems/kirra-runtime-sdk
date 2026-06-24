//! **Mick decision capture** — the eval side-channel for the LLM brain.
//!
//! Logs the full doer-checker triple for each Mick decision — the **intent** the brain
//! chose, the **grounding** Occy produced for it, and the **verdict** KIRRA returned — as a
//! flat JSONL record, so the brain's choices can be scored OFFLINE against the checker
//! (acceptance rate, how often an intent grounds to a refused plan, speed/clamp profiles,
//! per-intent-kind breakdowns).
//!
//! This is a SEPARATE channel from `kirra_core::capture` (which records the governor's
//! *corrections* for the supervised-learning loop): that one is verdict-shaped and has no
//! notion of an upstream intent. Mick-eval is *brain-quality* data, a distinct dataset, so
//! it gets its own pure record + a tiny synchronous JSONL sink rather than churning the
//! shared safety-capture wire schema. Mick is the slow (~2 Hz) System-2 loop, so a plain
//! append is ample — no async writer, no new dependencies.
//!
//! **Observability only — never on the verdict path.** Building a record is pure; the sink
//! is a best-effort append the caller invokes *after* the verdict is already decided. It
//! cannot alter what Occy proposes or what KIRRA admits.

use std::io::Write;

use crate::{MickIntent, PlanOutput, ProposalKind, TrajectoryVerdict};

/// Env gate (default OFF), mirroring `kirra_core::capture::CAPTURE_ENABLED_ENV`. Truthy =
/// `1` / `true` / `yes` (case-insensitive).
pub const MICK_EVAL_ENABLED_ENV: &str = "KIRRA_MICK_EVAL_ENABLED";
/// Optional JSONL sink path override. Default: [`DEFAULT_MICK_EVAL_PATH`] in the CWD.
pub const MICK_EVAL_PATH_ENV: &str = "KIRRA_MICK_EVAL_PATH";
/// Default sink path when [`MICK_EVAL_PATH_ENV`] is unset.
pub const DEFAULT_MICK_EVAL_PATH: &str = "kirra_mick_eval.jsonl";

/// One Mick decision: intent → grounding → verdict. Flat by design so a JSONL row is
/// directly analyzable (one `serde_json` line per decision). Field groups:
/// - the **intent** (`intent_kind` + the one relevant parameter, if any);
/// - the **grounding** Occy produced (`proposal_kind`, point count, path length, speeds);
/// - the **verdict** KIRRA returned (`verdict` + the boolean `admitted`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MickDecisionRecord {
    /// Monotonic per-run decision counter (caller-supplied).
    pub seq: u64,
    /// Wall-clock milliseconds at the decision (caller-supplied; no clock is read here so
    /// the record stays pure and testable).
    pub t_wall_ms: u64,

    // ----- intent (what the brain chose) -----
    /// Intent tag — the SAME vocabulary as the wire schema / `from_llm_json`
    /// (`go_to` | `lane_change` | `hold` | `cruise` | `overtake` | `pull_over`).
    pub intent_kind: &'static str,
    /// `go_to` target x (ego frame), if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_x_m: Option<f64>,
    /// `go_to` target y (ego frame), if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_y_m: Option<f64>,
    /// `lane_change` target offset, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_offset_m: Option<f64>,
    /// `cruise` requested speed, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_speed_mps: Option<f64>,

    // ----- grounding (what Occy produced) -----
    /// `motion` | `safe_stop`.
    pub proposal_kind: &'static str,
    /// Number of trajectory points.
    pub points: usize,
    /// Path length: summed consecutive XY distances along the trajectory (frame-independent
    /// measure of how far the grounded plan travels).
    pub path_len_m: f64,
    /// Peak commanded speed over the trajectory.
    pub max_speed_mps: f64,
    /// Terminal commanded speed (≈ 0 for a controlled stop / HOLD).
    pub final_speed_mps: f64,

    // ----- verdict (what KIRRA returned) -----
    /// `accept` | `clamp` | `mrc_fallback` | `pending`.
    pub verdict: &'static str,
    /// Whether the verdict admits motion (`Accept` | `Clamp`).
    pub admitted: bool,
}

impl MickDecisionRecord {
    /// Build the record from a Mick decision: the chosen `intent`, the `plan` Occy grounded
    /// it to, and the `verdict` KIRRA returned for that plan. Pure — no I/O, no clock.
    #[must_use]
    pub fn new(seq: u64, t_wall_ms: u64, intent: &MickIntent, plan: &PlanOutput, verdict: TrajectoryVerdict) -> Self {
        let (intent_kind, goal_x_m, goal_y_m, lane_offset_m, target_speed_mps) = match *intent {
            MickIntent::GoTo { x_m, y_m } => ("go_to", Some(x_m), Some(y_m), None, None),
            MickIntent::LaneChange { target_offset_m } => ("lane_change", None, None, Some(target_offset_m), None),
            MickIntent::Hold => ("hold", None, None, None, None),
            MickIntent::Cruise { target_speed_mps } => ("cruise", None, None, None, Some(target_speed_mps)),
            MickIntent::Overtake => ("overtake", None, None, None, None),
            MickIntent::PullOver => ("pull_over", None, None, None, None),
        };

        let proposal_kind = match plan.kind {
            ProposalKind::Motion => "motion",
            ProposalKind::SafeStop => "safe_stop",
        };
        let traj = &plan.trajectory;
        let path_len_m = traj
            .windows(2)
            .map(|w| (w[1].pose.x_m - w[0].pose.x_m).hypot(w[1].pose.y_m - w[0].pose.y_m))
            .sum();
        let max_speed_mps = traj.iter().map(|p| p.velocity_mps).fold(0.0, f64::max);
        let final_speed_mps = traj.last().map_or(0.0, |p| p.velocity_mps);

        let (verdict, admitted) = match verdict {
            TrajectoryVerdict::Accept => ("accept", true),
            TrajectoryVerdict::Clamp => ("clamp", true),
            TrajectoryVerdict::MRCFallback => ("mrc_fallback", false),
            TrajectoryVerdict::Pending => ("pending", false),
        };

        Self {
            seq,
            t_wall_ms,
            intent_kind,
            goal_x_m,
            goal_y_m,
            lane_offset_m,
            target_speed_mps,
            proposal_kind,
            points: traj.len(),
            path_len_m,
            max_speed_mps,
            final_speed_mps,
            verdict,
            admitted,
        }
    }
}

/// A best-effort synchronous JSONL sink for [`MickDecisionRecord`]s — one JSON object per
/// line, appended. Default OFF: [`MickEvalLog::from_env`] returns `None` unless
/// [`MICK_EVAL_ENABLED_ENV`] is truthy, mirroring the `kirra_core::capture` env gate.
pub struct MickEvalLog {
    file: std::fs::File,
}

impl MickEvalLog {
    /// True iff Mick-eval capture is enabled ([`MICK_EVAL_ENABLED_ENV`] truthy).
    #[must_use]
    pub fn enabled() -> bool {
        std::env::var(MICK_EVAL_ENABLED_ENV)
            .map(|v| {
                let t = v.trim();
                t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
            })
            .unwrap_or(false)
    }

    /// Open (creating / appending) the JSONL sink at `path`.
    ///
    /// # Errors
    /// Returns the underlying [`std::io::Error`] if the file cannot be opened for append.
    pub fn open(path: &str) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file })
    }

    /// Construct from the environment: `None` if disabled (default), else open the sink at
    /// [`MICK_EVAL_PATH_ENV`] (default [`DEFAULT_MICK_EVAL_PATH`]). A path that cannot be
    /// opened also yields `None` (capture is best-effort and must never break the loop).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        if !Self::enabled() {
            return None;
        }
        let path = std::env::var(MICK_EVAL_PATH_ENV).unwrap_or_else(|_| DEFAULT_MICK_EVAL_PATH.to_string());
        Self::open(&path).ok()
    }

    /// Append one record as a JSONL line.
    ///
    /// # Errors
    /// Returns the underlying [`std::io::Error`] on a serialization or write failure.
    pub fn append(&mut self, rec: &MickDecisionRecord) -> std::io::Result<()> {
        let line = serde_json::to_string(rec).map_err(std::io::Error::other)?;
        writeln!(self.file, "{line}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Pose, TrajectoryPoint};

    fn motion_plan() -> PlanOutput {
        let trajectory = (0..5)
            .map(|i| TrajectoryPoint {
                pose: Pose { x_m: i as f64, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: if i == 4 { 0.0 } else { 3.0 },
                time_from_start_s: i as f64 * 0.1,
            })
            .collect();
        PlanOutput { trajectory, kind: ProposalKind::Motion }
    }

    #[test]
    fn record_captures_intent_grounding_and_verdict() {
        let plan = motion_plan();
        let rec = MickDecisionRecord::new(7, 1234, &MickIntent::Overtake, &plan, TrajectoryVerdict::Accept);

        assert_eq!(rec.seq, 7);
        assert_eq!(rec.t_wall_ms, 1234);
        assert_eq!(rec.intent_kind, "overtake");
        assert_eq!(rec.proposal_kind, "motion");
        assert_eq!(rec.points, 5);
        assert!((rec.path_len_m - 4.0).abs() < 1e-9, "5 unit-spaced points → 4 m, got {}", rec.path_len_m);
        assert_eq!(rec.max_speed_mps, 3.0);
        assert_eq!(rec.final_speed_mps, 0.0);
        assert_eq!(rec.verdict, "accept");
        assert!(rec.admitted);
    }

    #[test]
    fn intent_kind_and_params_map_per_variant() {
        let plan = motion_plan();
        let cases: [(MickIntent, &str); 6] = [
            (MickIntent::GoTo { x_m: 20.0, y_m: -4.0 }, "go_to"),
            (MickIntent::LaneChange { target_offset_m: 3.5 }, "lane_change"),
            (MickIntent::Hold, "hold"),
            (MickIntent::Cruise { target_speed_mps: 5.0 }, "cruise"),
            (MickIntent::Overtake, "overtake"),
            (MickIntent::PullOver, "pull_over"),
        ];
        for (intent, kind) in cases {
            let r = MickDecisionRecord::new(0, 0, &intent, &plan, TrajectoryVerdict::Accept);
            assert_eq!(r.intent_kind, kind);
        }
        // The one relevant parameter is carried; the others stay None.
        let goto = MickDecisionRecord::new(0, 0, &MickIntent::GoTo { x_m: 20.0, y_m: -4.0 }, &plan, TrajectoryVerdict::Accept);
        assert_eq!((goto.goal_x_m, goto.goal_y_m, goto.lane_offset_m, goto.target_speed_mps), (Some(20.0), Some(-4.0), None, None));
        let cruise = MickDecisionRecord::new(0, 0, &MickIntent::Cruise { target_speed_mps: 5.0 }, &plan, TrajectoryVerdict::Accept);
        assert_eq!((cruise.target_speed_mps, cruise.goal_x_m), (Some(5.0), None));
    }

    #[test]
    fn verdict_maps_to_token_and_admitted() {
        let plan = motion_plan();
        let v = |verdict| {
            let r = MickDecisionRecord::new(0, 0, &MickIntent::Hold, &plan, verdict);
            (r.verdict, r.admitted)
        };
        assert_eq!(v(TrajectoryVerdict::Accept), ("accept", true));
        assert_eq!(v(TrajectoryVerdict::Clamp), ("clamp", true));
        assert_eq!(v(TrajectoryVerdict::MRCFallback), ("mrc_fallback", false));
        assert_eq!(v(TrajectoryVerdict::Pending), ("pending", false));
    }

    #[test]
    fn record_serializes_as_one_flat_json_object_skipping_absent_params() {
        let plan = motion_plan();
        let rec = MickDecisionRecord::new(1, 2, &MickIntent::Hold, &plan, TrajectoryVerdict::MRCFallback);
        let v: serde_json::Value = serde_json::to_value(&rec).unwrap();
        assert_eq!(v["intent_kind"], "hold");
        assert_eq!(v["verdict"], "mrc_fallback");
        assert_eq!(v["admitted"], false);
        // Absent intent params are omitted (skip_serializing_if), not serialized as null.
        assert!(v.get("goal_x_m").is_none(), "absent params are omitted from the JSON");
    }

    #[test]
    fn eval_log_appends_jsonl_lines() {
        let mut path = std::env::temp_dir();
        path.push(format!("kirra_mick_eval_test_{}.jsonl", std::process::id()));
        let path_str = path.to_str().unwrap();
        let _ = std::fs::remove_file(&path); // clean any stale file

        let plan = motion_plan();
        {
            let mut log = MickEvalLog::open(path_str).expect("open sink");
            log.append(&MickDecisionRecord::new(0, 0, &MickIntent::Overtake, &plan, TrajectoryVerdict::Accept)).unwrap();
            log.append(&MickDecisionRecord::new(1, 100, &MickIntent::PullOver, &plan, TrajectoryVerdict::MRCFallback)).unwrap();
        }

        let body = std::fs::read_to_string(&path).expect("read sink");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "one JSONL row per record");
        let first: serde_json::Value = serde_json::from_str(lines[0]).expect("row 0 parses");
        let second: serde_json::Value = serde_json::from_str(lines[1]).expect("row 1 parses");
        assert_eq!(first["intent_kind"], "overtake");
        assert_eq!(first["admitted"], true);
        assert_eq!(second["intent_kind"], "pull_over");
        assert_eq!(second["admitted"], false);

        let _ = std::fs::remove_file(&path);
    }
}
