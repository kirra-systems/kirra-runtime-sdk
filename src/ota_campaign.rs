//! OTA governor-artifact campaign engine (WS-4 / Track 3 — Fleet Plane).
//!
//! The control-plane state machine for rolling a **signed** governor artifact to a
//! fleet in staged percentages, with automatic **halt-on-regression** bound to
//! fleet posture telemetry. This module is the PURE core: the [`Campaign`] type,
//! its transition logic, and the halt rule. Persistence (the `ota_campaigns`
//! SQLite table) lives in `verifier_store`; the admin routes live in the service
//! binary; every state transition there writes an R156-shaped audit-chain entry.
//!
//! **Fail-closed posture (the safety spine of this engine).** A rollout NEVER
//! proceeds while the fleet is unsafe: [`Campaign::advance`] refuses to advance and
//! instead HALTS the campaign whenever the observed [`FleetPosture`] is not
//! `Nominal`. A halted campaign is TERMINAL — the engine never AUTHORS a resume;
//! continuing requires authoring a NEW campaign. This mirrors the LockedOut
//! human-reset discipline of the governor itself: recovery is a deliberate act, not
//! an automatic one. `now_ms` is always supplied by the caller — no wall-clock read
//! lives here (testability; mirrors the store convention).

use serde::{Deserialize, Serialize};

use crate::verifier::FleetPosture;

/// Lifecycle state of an OTA governor-artifact campaign.
///
/// `Draft → Staged → Rolling → {Completed | Halted}`. `Staged` is the deliberate
/// arm gate between authoring and the first roll; `Halted` and `Completed` are
/// terminal. Only `Staged` and `Rolling` are *active* (halt-on-regression armed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignState {
    /// Authored — artifact digest, cohorts and stage schedule pinned; nothing rolled.
    Draft,
    /// Armed for rollout after operator review; ready to advance but 0% rolled.
    Staged,
    /// Actively rolling — at least one stage has advanced; halt-on-regression armed.
    Rolling,
    /// Auto-halted by a posture regression, or operator-halted. TERMINAL.
    Halted,
    /// Reached the final 100% stage with no regression. TERMINAL.
    Completed,
}

impl CampaignState {
    /// A campaign is *active* (advanceable, halt-on-regression armed) only in
    /// `Staged` or `Rolling`. Terminal states (`Halted`/`Completed`) and the
    /// pre-arm `Draft` are not.
    pub fn is_active(self) -> bool {
        matches!(self, CampaignState::Staged | CampaignState::Rolling)
    }

    /// Terminal — no further transition is possible.
    pub fn is_terminal(self) -> bool {
        matches!(self, CampaignState::Halted | CampaignState::Completed)
    }

    /// The canonical lowercase wire string (matches the SQLite `state` column and
    /// the JSON `serde` rename). Kept explicit so the store round-trip does not
    /// depend on `serde_json` for a single enum.
    pub fn as_str(self) -> &'static str {
        match self {
            CampaignState::Draft => "draft",
            CampaignState::Staged => "staged",
            CampaignState::Rolling => "rolling",
            CampaignState::Halted => "halted",
            CampaignState::Completed => "completed",
        }
    }

    /// Parse the wire string back to a state; `None` for an unknown token
    /// (fail-closed — the store treats an unparseable row as corrupt). Named
    /// `parse` (not `from_str`) to follow the codebase's `parse_*`→`Option`
    /// convention (e.g. `ApiRole::parse_role`) rather than the `FromStr` trait.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(CampaignState::Draft),
            "staged" => Some(CampaignState::Staged),
            "rolling" => Some(CampaignState::Rolling),
            "halted" => Some(CampaignState::Halted),
            "completed" => Some(CampaignState::Completed),
            _ => None,
        }
    }
}

/// Why a campaign halted. Carried on the `Halted` transition and written into the
/// R156 audit entry as the machine-readable reason code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltReason {
    /// Fleet posture regressed to `LockedOut` during an active rollout.
    PostureLockedOut,
    /// Fleet posture regressed to `Degraded` during an active rollout.
    PostureDegraded,
    /// Operator-commanded halt (manual stop, not a regression).
    OperatorHalt,
}

impl HaltReason {
    pub fn as_str(self) -> &'static str {
        match self {
            HaltReason::PostureLockedOut => "posture_locked_out",
            HaltReason::PostureDegraded => "posture_degraded",
            HaltReason::OperatorHalt => "operator_halt",
        }
    }

    /// Parse the wire string back to a reason; `None` for an unknown token.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "posture_locked_out" => Some(HaltReason::PostureLockedOut),
            "posture_degraded" => Some(HaltReason::PostureDegraded),
            "operator_halt" => Some(HaltReason::OperatorHalt),
            _ => None,
        }
    }
}

/// The regression halt rule — the single point where posture telemetry decides
/// whether a rollout may continue. A non-`Nominal` posture yields the halt reason
/// to apply; `Nominal` yields `None` (proceed). Pure and total: exhaustive over
/// [`FleetPosture`], so a new posture variant is a compile error here.
pub fn posture_regression_halt(posture: FleetPosture) -> Option<HaltReason> {
    match posture {
        FleetPosture::Nominal => None,
        FleetPosture::Degraded => Some(HaltReason::PostureDegraded),
        FleetPosture::LockedOut => Some(HaltReason::PostureLockedOut),
    }
}

/// The outcome of an [`Campaign::advance`] call — lets the caller pick the audit
/// event type without re-inspecting the campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvanceOutcome {
    /// Moved to a new rollout stage; `rollout_percent` is the new stage percentage.
    Advanced { rollout_percent: u8 },
    /// The final 100% stage was reached — campaign is now `Completed`.
    Completed,
    /// The observed posture was not `Nominal`; the campaign was HALTED, not advanced.
    Halted { reason: HaltReason },
}

/// Errors from campaign construction and invalid transitions. Fail-closed: an
/// invalid transition is refused, never silently coerced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignError {
    /// `campaign_id` was empty.
    EmptyId,
    /// `artifact_digest` was not a 64-char lowercase hex SHA-256.
    InvalidArtifactDigest,
    /// `artifact_version` was empty.
    EmptyVersion,
    /// The cohort set was empty — a campaign must target at least one cohort.
    EmptyCohorts,
    /// The stage schedule was empty, not strictly increasing, out of `1..=100`, or
    /// did not end at exactly `100`.
    InvalidStages,
    /// The requested transition is not legal from the current state.
    InvalidTransition {
        from: CampaignState,
        action: &'static str,
    },
}

impl std::fmt::Display for CampaignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CampaignError::EmptyId => write!(f, "campaign id is empty"),
            CampaignError::InvalidArtifactDigest => {
                write!(f, "artifact digest is not a 64-char lowercase hex sha256")
            }
            CampaignError::EmptyVersion => write!(f, "artifact version is empty"),
            CampaignError::EmptyCohorts => write!(f, "campaign targets no cohorts"),
            CampaignError::InvalidStages => write!(
                f,
                "stage schedule must be strictly increasing within 1..=100 and end at 100"
            ),
            CampaignError::InvalidTransition { from, action } => {
                write!(f, "cannot {action} a campaign in state {from:?}")
            }
        }
    }
}

impl std::error::Error for CampaignError {}

/// An OTA governor-artifact rollout campaign.
///
/// A campaign pins a signed artifact (`artifact_digest` — the cosign-signed release
/// digest) and rolls it to `cohorts` through a strictly-increasing `stages`
/// schedule of rollout percentages ending at 100. `stage_index` points at the
/// current stage; `rollout_percent` is the percentage actually reached (0 until
/// the first `advance`). The [`CampaignState`] machine and the fail-closed
/// halt-on-regression rule are the safety-relevant core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Campaign {
    pub campaign_id: String,
    /// SHA-256 hex (64 lowercase) of the signed governor artifact being rolled.
    pub artifact_digest: String,
    pub artifact_version: String,
    /// Target cohort labels (node groups). Non-empty.
    pub cohorts: Vec<String>,
    /// Strictly-increasing rollout percentages within `1..=100`, ending at `100`.
    pub stages: Vec<u8>,
    /// Index into `stages` of the CURRENT stage (0 before the first advance).
    pub stage_index: usize,
    /// The rollout percentage actually reached (0 until `Rolling`).
    pub rollout_percent: u8,
    pub state: CampaignState,
    /// Set only in the `Halted` state.
    pub halt_reason: Option<HaltReason>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl Campaign {
    /// Author a new campaign in the `Draft` state. Validates every field
    /// fail-closed; a bad artifact digest / empty cohort set / malformed stage
    /// schedule is refused here, before any persistence.
    pub fn new(
        campaign_id: impl Into<String>,
        artifact_digest: impl Into<String>,
        artifact_version: impl Into<String>,
        cohorts: Vec<String>,
        stages: Vec<u8>,
        now_ms: u64,
    ) -> Result<Self, CampaignError> {
        let campaign_id = campaign_id.into();
        let artifact_digest = artifact_digest.into();
        let artifact_version = artifact_version.into();

        if campaign_id.trim().is_empty() {
            return Err(CampaignError::EmptyId);
        }
        if !is_sha256_hex(&artifact_digest) {
            return Err(CampaignError::InvalidArtifactDigest);
        }
        if artifact_version.trim().is_empty() {
            return Err(CampaignError::EmptyVersion);
        }
        if cohorts.is_empty() || cohorts.iter().any(|c| c.trim().is_empty()) {
            return Err(CampaignError::EmptyCohorts);
        }
        if !stages_valid(&stages) {
            return Err(CampaignError::InvalidStages);
        }

        Ok(Campaign {
            campaign_id,
            artifact_digest,
            artifact_version,
            cohorts,
            stages,
            stage_index: 0,
            rollout_percent: 0,
            state: CampaignState::Draft,
            halt_reason: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        })
    }

    /// Arm the campaign for rollout: `Draft → Staged`. The deliberate operator
    /// review gate between authoring and the first roll. No node is rolled yet.
    pub fn arm(&mut self, now_ms: u64) -> Result<(), CampaignError> {
        match self.state {
            CampaignState::Draft => {
                self.state = CampaignState::Staged;
                self.updated_at_ms = now_ms;
                Ok(())
            }
            from => Err(CampaignError::InvalidTransition {
                from,
                action: "arm",
            }),
        }
    }

    /// Advance to the next rollout stage — the fail-closed heart of the engine.
    ///
    /// FIRST it consults the halt rule on `observed` posture: if the fleet is not
    /// `Nominal`, the campaign is HALTED (not advanced) and [`AdvanceOutcome::Halted`]
    /// is returned — a rollout never proceeds while the fleet is unsafe. On
    /// `Nominal`, it moves `Staged → Rolling` (first stage) or `Rolling → Rolling`
    /// (next stage); reaching the final `100` stage transitions to `Completed`.
    ///
    /// Only legal from an active state (`Staged`/`Rolling`).
    pub fn advance(
        &mut self,
        observed: FleetPosture,
        now_ms: u64,
    ) -> Result<AdvanceOutcome, CampaignError> {
        if !self.state.is_active() {
            return Err(CampaignError::InvalidTransition {
                from: self.state,
                action: "advance",
            });
        }

        // Fail-closed: posture regression halts before any stage moves.
        if let Some(reason) = posture_regression_halt(observed) {
            self.enter_halted(reason, now_ms);
            return Ok(AdvanceOutcome::Halted { reason });
        }

        // Nominal — move to the next stage. From `Staged` the first advance enters
        // `stages[0]`; from `Rolling` it steps `stage_index` forward.
        let next_index = match self.state {
            CampaignState::Staged => 0,
            CampaignState::Rolling => {
                self.stage_index
                    .checked_add(1)
                    .ok_or(CampaignError::InvalidTransition {
                        from: self.state,
                        action: "advance",
                    })?
            }
            // Unreachable: guarded by is_active() above, but stay total.
            other => {
                return Err(CampaignError::InvalidTransition {
                    from: other,
                    action: "advance",
                })
            }
        };

        // Defense-in-depth: a well-formed campaign always has `next_index` in range
        // (reaching the final stage transitions to `Completed`, so a `Rolling`
        // campaign is never parked at the last index), but a corrupt-LOADED
        // `stage_index` inconsistent with `stages` could push it out of bounds.
        // Refuse rather than panic on `stages[next_index]` — this keeps `advance`
        // panic-free for ANY `Campaign` value, not just engine-produced ones.
        if next_index >= self.stages.len() {
            return Err(CampaignError::InvalidTransition {
                from: self.state,
                action: "advance",
            });
        }

        self.stage_index = next_index;
        self.rollout_percent = self.stages[next_index];
        self.updated_at_ms = now_ms;

        if next_index + 1 == self.stages.len() {
            // Final stage (== 100 by construction) reached.
            self.state = CampaignState::Completed;
            Ok(AdvanceOutcome::Completed)
        } else {
            self.state = CampaignState::Rolling;
            Ok(AdvanceOutcome::Advanced {
                rollout_percent: self.rollout_percent,
            })
        }
    }

    /// Operator-commanded halt from any active state. Terminal; the engine never
    /// authors a resume. Refused if the campaign is not active.
    pub fn halt(&mut self, reason: HaltReason, now_ms: u64) -> Result<(), CampaignError> {
        if !self.state.is_active() {
            return Err(CampaignError::InvalidTransition {
                from: self.state,
                action: "halt",
            });
        }
        self.enter_halted(reason, now_ms);
        Ok(())
    }

    /// Posture-monitor entry point: if the campaign is active AND `observed`
    /// posture has regressed, HALT it and return the reason; otherwise `None` (no
    /// state change). Idempotent on an already-terminal campaign. This is what a
    /// periodic posture-telemetry sweep calls to enforce halt-on-regression even
    /// between explicit `advance` calls.
    pub fn check_regression(&mut self, observed: FleetPosture, now_ms: u64) -> Option<HaltReason> {
        if !self.state.is_active() {
            return None;
        }
        let reason = posture_regression_halt(observed)?;
        self.enter_halted(reason, now_ms);
        Some(reason)
    }

    fn enter_halted(&mut self, reason: HaltReason, now_ms: u64) {
        self.state = CampaignState::Halted;
        self.halt_reason = Some(reason);
        self.updated_at_ms = now_ms;
    }
}

/// A 64-char lowercase hex SHA-256 digest (the cosign-signed artifact identity).
fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A stage schedule is valid iff non-empty, every entry in `1..=100`, strictly
/// increasing, and the final entry is exactly `100` (a rollout must reach the
/// whole fleet to `Complete`).
fn stages_valid(stages: &[u8]) -> bool {
    if stages.is_empty() {
        return false;
    }
    if *stages.last().unwrap() != 100 {
        return false;
    }
    let mut prev = 0u8;
    for &s in stages {
        if s == 0 || s > 100 || s <= prev {
            return false;
        }
        prev = s;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn campaign() -> Campaign {
        Campaign::new(
            "camp-1",
            DIGEST,
            "v1.2.3",
            vec!["canary".into(), "fleet".into()],
            vec![10, 50, 100],
            1_000,
        )
        .expect("valid campaign")
    }

    #[test]
    fn construction_validates_fields() {
        assert_eq!(
            Campaign::new("", DIGEST, "v1", vec!["a".into()], vec![100], 0).unwrap_err(),
            CampaignError::EmptyId
        );
        assert_eq!(
            Campaign::new("c", "nothex", "v1", vec!["a".into()], vec![100], 0).unwrap_err(),
            CampaignError::InvalidArtifactDigest
        );
        assert_eq!(
            Campaign::new("c", DIGEST, "", vec!["a".into()], vec![100], 0).unwrap_err(),
            CampaignError::EmptyVersion
        );
        assert_eq!(
            Campaign::new("c", DIGEST, "v1", vec![], vec![100], 0).unwrap_err(),
            CampaignError::EmptyCohorts
        );
        // A blank cohort label is also rejected.
        assert_eq!(
            Campaign::new("c", DIGEST, "v1", vec![" ".into()], vec![100], 0).unwrap_err(),
            CampaignError::EmptyCohorts
        );
    }

    #[test]
    fn stage_schedule_validation() {
        // Not ending at 100.
        assert!(Campaign::new("c", DIGEST, "v1", vec!["a".into()], vec![10, 50], 0).is_err());
        // Not strictly increasing.
        assert!(Campaign::new("c", DIGEST, "v1", vec!["a".into()], vec![50, 50, 100], 0).is_err());
        // Contains 0.
        assert!(Campaign::new("c", DIGEST, "v1", vec!["a".into()], vec![0, 100], 0).is_err());
        // Empty.
        assert!(Campaign::new("c", DIGEST, "v1", vec!["a".into()], vec![], 0).is_err());
        // Valid single-stage 100.
        assert!(Campaign::new("c", DIGEST, "v1", vec!["a".into()], vec![100], 0).is_ok());
    }

    #[test]
    fn advance_refuses_out_of_range_stage_index_without_panicking() {
        // A campaign that could only arise from a corrupt LOAD: state `Rolling`
        // but `stage_index` already at (indeed past) the last stage. `advance`
        // must refuse, not panic on `stages[next_index]`.
        let mut corrupt = campaign();
        corrupt.state = CampaignState::Rolling;
        corrupt.stage_index = corrupt.stages.len() - 1; // last valid index → next is OOB
        assert!(matches!(
            corrupt.advance(FleetPosture::Nominal, 2_000),
            Err(CampaignError::InvalidTransition { .. })
        ));

        // Even a wildly out-of-range index refuses rather than indexing/overflowing.
        let mut corrupt2 = campaign();
        corrupt2.state = CampaignState::Rolling;
        corrupt2.stage_index = usize::MAX;
        assert!(matches!(
            corrupt2.advance(FleetPosture::Nominal, 2_000),
            Err(CampaignError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn happy_path_rolls_to_completion() {
        let mut c = campaign();
        assert_eq!(c.state, CampaignState::Draft);
        c.arm(1_100).unwrap();
        assert_eq!(c.state, CampaignState::Staged);
        assert_eq!(c.rollout_percent, 0);

        assert_eq!(
            c.advance(FleetPosture::Nominal, 1_200).unwrap(),
            AdvanceOutcome::Advanced {
                rollout_percent: 10
            }
        );
        assert_eq!(c.state, CampaignState::Rolling);

        assert_eq!(
            c.advance(FleetPosture::Nominal, 1_300).unwrap(),
            AdvanceOutcome::Advanced {
                rollout_percent: 50
            }
        );
        assert_eq!(
            c.advance(FleetPosture::Nominal, 1_400).unwrap(),
            AdvanceOutcome::Completed
        );
        assert_eq!(c.state, CampaignState::Completed);
        assert_eq!(c.rollout_percent, 100);
        assert!(c.state.is_terminal());
    }

    #[test]
    fn advance_halts_fail_closed_on_degraded() {
        let mut c = campaign();
        c.arm(1_100).unwrap();
        c.advance(FleetPosture::Nominal, 1_200).unwrap(); // reach 10%
                                                          // A regression to Degraded during the rollout halts instead of advancing.
        assert_eq!(
            c.advance(FleetPosture::Degraded, 1_300).unwrap(),
            AdvanceOutcome::Halted {
                reason: HaltReason::PostureDegraded
            }
        );
        assert_eq!(c.state, CampaignState::Halted);
        assert_eq!(c.halt_reason, Some(HaltReason::PostureDegraded));
        // Rollout percentage did NOT move past the last safe stage.
        assert_eq!(c.rollout_percent, 10);
    }

    #[test]
    fn advance_halts_fail_closed_on_lockout() {
        let mut c = campaign();
        c.arm(1_100).unwrap();
        assert_eq!(
            c.advance(FleetPosture::LockedOut, 1_200).unwrap(),
            AdvanceOutcome::Halted {
                reason: HaltReason::PostureLockedOut
            }
        );
        assert_eq!(c.state, CampaignState::Halted);
        // Never even reached the first stage.
        assert_eq!(c.rollout_percent, 0);
    }

    #[test]
    fn halted_is_terminal_no_resume() {
        let mut c = campaign();
        c.arm(1_100).unwrap();
        c.halt(HaltReason::OperatorHalt, 1_200).unwrap();
        assert_eq!(c.state, CampaignState::Halted);
        // The engine authors no resume: advance/arm/halt all refuse.
        assert!(matches!(
            c.advance(FleetPosture::Nominal, 1_300),
            Err(CampaignError::InvalidTransition { .. })
        ));
        assert!(matches!(
            c.halt(HaltReason::OperatorHalt, 1_300),
            Err(CampaignError::InvalidTransition { .. })
        ));
        assert!(c.check_regression(FleetPosture::LockedOut, 1_300).is_none());
    }

    #[test]
    fn cannot_advance_before_arming() {
        let mut c = campaign();
        assert!(matches!(
            c.advance(FleetPosture::Nominal, 1_200),
            Err(CampaignError::InvalidTransition {
                from: CampaignState::Draft,
                ..
            })
        ));
    }

    #[test]
    fn check_regression_halts_active_campaign() {
        let mut c = campaign();
        c.arm(1_100).unwrap();
        c.advance(FleetPosture::Nominal, 1_200).unwrap();
        // A monitor sweep observing Nominal is a no-op.
        assert!(c.check_regression(FleetPosture::Nominal, 1_250).is_none());
        assert_eq!(c.state, CampaignState::Rolling);
        // A sweep observing a regression halts.
        assert_eq!(
            c.check_regression(FleetPosture::LockedOut, 1_300),
            Some(HaltReason::PostureLockedOut)
        );
        assert_eq!(c.state, CampaignState::Halted);
    }

    #[test]
    fn state_wire_roundtrip() {
        for st in [
            CampaignState::Draft,
            CampaignState::Staged,
            CampaignState::Rolling,
            CampaignState::Halted,
            CampaignState::Completed,
        ] {
            assert_eq!(CampaignState::parse(st.as_str()), Some(st));
        }
        assert_eq!(CampaignState::parse("bogus"), None);
    }

    #[test]
    fn digest_validation_is_strict() {
        assert!(is_sha256_hex(DIGEST));
        assert!(!is_sha256_hex(&DIGEST[..63])); // too short
        assert!(!is_sha256_hex(&format!("{DIGEST}0"))); // too long
        assert!(!is_sha256_hex(&DIGEST.to_uppercase())); // uppercase rejected
    }
}
