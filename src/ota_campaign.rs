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

/// The artifact a node should be running, resolved from the active campaigns for
/// the node's cohorts at the current staged rollout percentage. This is the
/// node-facing seam the (future) on-device installer consumes: a node asks "what
/// signed governor artifact am I assigned?" and gets a stable answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAssignment {
    pub node_id: String,
    /// `true` iff the node falls inside a matching campaign's rolled percentage.
    pub rolled: bool,
    /// The campaign that assigned the artifact (`None` when not rolled).
    pub campaign_id: Option<String>,
    /// The signed artifact digest the node should run (`None` when not rolled — the
    /// node stays on its current/baseline artifact).
    pub artifact_digest: Option<String>,
    pub artifact_version: Option<String>,
}

/// Deterministic rollout bucket for a node within a campaign: a stable value in
/// `0..=99` from `SHA-256(campaign_id ":" node_id)`. Pure — no RNG, no clock — so a
/// node always lands in the same bucket for a given campaign across queries and
/// restarts.
fn node_rollout_bucket(campaign_id: &str, node_id: &str) -> u8 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(campaign_id.as_bytes());
    h.update(b":");
    h.update(node_id.as_bytes());
    let digest = h.finalize();
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(first8) % 100) as u8
}

/// Is `node_id` inside `campaign_id`'s rolled cohort at `rollout_percent`?
///
/// `bucket < rollout_percent`, so membership is **monotone** in the percentage —
/// a node rolled at 10% is still rolled at 50% and 100% (a staged rollout only
/// ever ADDS nodes, never un-rolls one), and at 100% every node is rolled. The
/// rolled subset is per-campaign (the bucket is salted by `campaign_id`), so the
/// same node is not always in the early canary of every rollout.
pub fn is_node_rolled(campaign_id: &str, node_id: &str, rollout_percent: u8) -> bool {
    node_rollout_bucket(campaign_id, node_id) < rollout_percent
}

/// Resolve the artifact a node should run from the ACTIVE campaigns.
///
/// A campaign assigns its artifact to `node_id` iff it is `Rolling`, one of its
/// `cohorts` is one of the node's `node_cohorts`, AND the node is inside its rolled
/// percentage ([`is_node_rolled`]). If several campaigns match, the NEWEST (by
/// `created_at_ms`) wins — a later rollout supersedes an earlier one for the same
/// node. No match → not rolled (the node stays on its current/baseline artifact).
/// Pure and deterministic over `active`.
pub fn resolve_node_assignment(
    node_id: &str,
    node_cohorts: &[String],
    active: &[Campaign],
) -> NodeAssignment {
    let mut best: Option<&Campaign> = None;
    for c in active {
        // Only Rolling campaigns have rolled nodes (Staged sits at 0%).
        if c.state != CampaignState::Rolling {
            continue;
        }
        let cohort_match = c
            .cohorts
            .iter()
            .any(|ch| node_cohorts.iter().any(|nc| nc == ch));
        if !cohort_match {
            continue;
        }
        if !is_node_rolled(&c.campaign_id, node_id, c.rollout_percent) {
            continue;
        }
        // Newest matching campaign wins (ties keep the incumbent — deterministic).
        best = match best {
            Some(b) if b.created_at_ms >= c.created_at_ms => Some(b),
            _ => Some(c),
        };
    }
    match best {
        Some(c) => NodeAssignment {
            node_id: node_id.to_string(),
            rolled: true,
            campaign_id: Some(c.campaign_id.clone()),
            artifact_digest: Some(c.artifact_digest.clone()),
            artifact_version: Some(c.artifact_version.clone()),
        },
        None => NodeAssignment {
            node_id: node_id.to_string(),
            rolled: false,
            campaign_id: None,
            artifact_digest: None,
            artifact_version: None,
        },
    }
}

/// A node's self-reported artifact adoption: the digest it is actually RUNNING after
/// an OTA commit. Reported by the node-side agent to
/// `POST /fleet/campaigns/report`; the fleet summary joins it against the active
/// campaigns to show real per-campaign adoption. `node_id` is the primary key in the
/// store (latest report wins — a node runs one governor at a time).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeArtifactStatus {
    pub node_id: String,
    /// The SHA-256 hex digest the node reports currently running.
    pub applied_digest: String,
    /// The campaign the node believes it adopted the digest under (optional — the
    /// join is by digest, so this is context only).
    pub campaign_id: Option<String>,
    pub artifact_version: Option<String>,
    pub reported_at_ms: u64,
}

/// A fleet-wide observability summary of every campaign — the operator's at-a-glance
/// rollout view. Campaign counts/state/progress come PURELY from the campaign records
/// the verifier authoritatively owns; per-node ADOPTION (`applied_nodes`) is joined
/// from the nodes' self-reported [`NodeArtifactStatus`]. The rolled-set DENOMINATOR
/// (how many nodes a stage targets) needs per-node cohort membership the verifier does
/// not persist, so this reports the adoption NUMERATOR only — it never fabricates a
/// completion percentage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignSummary {
    pub total: usize,
    pub draft: usize,
    pub staged: usize,
    pub rolling: usize,
    pub completed: usize,
    pub halted: usize,
    /// Active (`Staged`/`Rolling`) campaigns with their stage progress. Preserves the
    /// input order (the store yields newest-first).
    pub active: Vec<CampaignProgress>,
    /// Halted campaigns and WHY — surfaces fail-closed auto-halts (a posture
    /// regression) distinctly from operator halts. Preserves input order.
    pub halted_campaigns: Vec<HaltedCampaign>,
}

/// Per-active-campaign rollout progress in the [`CampaignSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignProgress {
    pub campaign_id: String,
    pub state: String,
    pub artifact_version: String,
    /// 1-based current stage number (`stage_index + 1`) and the total stage count —
    /// e.g. stage 2 of 4.
    pub stage: usize,
    pub stage_count: usize,
    pub rollout_percent: u8,
    pub cohorts: Vec<String>,
    /// Distinct nodes that have REPORTED running this campaign's `artifact_digest`
    /// (the adoption numerator). `0` until nodes report; needs no cohort data.
    pub applied_nodes: usize,
}

/// A halted campaign and its reason in the [`CampaignSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HaltedCampaign {
    pub campaign_id: String,
    pub artifact_version: String,
    /// The machine-readable halt reason (`posture_locked_out` / `posture_degraded` /
    /// `operator_halt`); `"unknown"` only if a persisted `Halted` row somehow lacks
    /// one (defensive — the engine always sets it).
    pub halt_reason: String,
    /// The percentage the rollout had reached when it halted.
    pub rollout_percent: u8,
}

/// Summarize a set of campaigns into a [`CampaignSummary`], joining the nodes'
/// self-reported adoption `statuses` to fill each active campaign's `applied_nodes`
/// (distinct nodes reporting that campaign's digest). Pure and total: no clock, no
/// store; preserves the campaigns' input order.
pub fn summarize_campaigns(
    campaigns: &[Campaign],
    statuses: &[NodeArtifactStatus],
) -> CampaignSummary {
    let mut s = CampaignSummary {
        total: campaigns.len(),
        draft: 0,
        staged: 0,
        rolling: 0,
        completed: 0,
        halted: 0,
        active: Vec::new(),
        halted_campaigns: Vec::new(),
    };
    for c in campaigns {
        match c.state {
            CampaignState::Draft => s.draft += 1,
            CampaignState::Staged => s.staged += 1,
            CampaignState::Rolling => s.rolling += 1,
            CampaignState::Completed => s.completed += 1,
            CampaignState::Halted => s.halted += 1,
        }
        if c.state.is_active() {
            // Adoption numerator: distinct nodes reporting this campaign's digest.
            // node_id is the store PK, so each node contributes at most one status.
            let applied_nodes = statuses
                .iter()
                .filter(|st| st.applied_digest == c.artifact_digest)
                .count();
            s.active.push(CampaignProgress {
                campaign_id: c.campaign_id.clone(),
                state: c.state.as_str().to_string(),
                artifact_version: c.artifact_version.clone(),
                stage: c.stage_index.saturating_add(1),
                stage_count: c.stages.len(),
                rollout_percent: c.rollout_percent,
                cohorts: c.cohorts.clone(),
                applied_nodes,
            });
        }
        if c.state == CampaignState::Halted {
            s.halted_campaigns.push(HaltedCampaign {
                campaign_id: c.campaign_id.clone(),
                artifact_version: c.artifact_version.clone(),
                halt_reason: c
                    .halt_reason
                    .map(|r| r.as_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                rollout_percent: c.rollout_percent,
            });
        }
    }
    s
}

/// Escape a string for use as a Prometheus label VALUE (`\` → `\\`, `"` → `\"`,
/// newline → `\n`) — a campaign id is emitted inside `campaign_id="…"`.
fn escape_prom_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Render a [`CampaignSummary`] as Prometheus exposition text (WS-4 fleet-rollout
/// series), for the `/metrics` scrape. Emits campaign counts by state, plus per
/// active-campaign rollout percentage and adoption count. Pure — the caller loads the
/// summary and appends this to the scrape body. Posture-exempt like the rest of
/// `/metrics`, so a rollout stays observable even under LockedOut.
pub fn campaign_metrics_prometheus(summary: &CampaignSummary) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# HELP kirra_ota_campaigns_total OTA governor-artifact campaigns by lifecycle state."
    );
    let _ = writeln!(s, "# TYPE kirra_ota_campaigns_total gauge");
    for (state, n) in [
        ("draft", summary.draft),
        ("staged", summary.staged),
        ("rolling", summary.rolling),
        ("completed", summary.completed),
        ("halted", summary.halted),
    ] {
        let _ = writeln!(s, "kirra_ota_campaigns_total{{state=\"{state}\"}} {n}");
    }
    let _ = writeln!(
        s,
        "# HELP kirra_ota_campaign_rollout_percent Rollout percentage of an active (staged/rolling) campaign."
    );
    let _ = writeln!(s, "# TYPE kirra_ota_campaign_rollout_percent gauge");
    for c in &summary.active {
        let _ = writeln!(
            s,
            "kirra_ota_campaign_rollout_percent{{campaign_id=\"{}\"}} {}",
            escape_prom_label(&c.campaign_id),
            c.rollout_percent
        );
    }
    let _ = writeln!(
        s,
        "# HELP kirra_ota_campaign_applied_nodes Distinct nodes reporting an active campaign's artifact digest (adoption numerator)."
    );
    let _ = writeln!(s, "# TYPE kirra_ota_campaign_applied_nodes gauge");
    for c in &summary.active {
        let _ = writeln!(
            s,
            "kirra_ota_campaign_applied_nodes{{campaign_id=\"{}\"}} {}",
            escape_prom_label(&c.campaign_id),
            c.applied_nodes
        );
    }
    s
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

    // --- node artifact assignment -----------------------------------------

    #[test]
    fn is_node_rolled_is_deterministic_and_monotone() {
        // Same inputs → same answer across calls.
        assert_eq!(
            is_node_rolled("camp-1", "node-7", 50),
            is_node_rolled("camp-1", "node-7", 50)
        );
        // Monotone in percent: once a node is rolled it stays rolled as % grows.
        for node in ["node-a", "node-b", "node-c", "node-xyz", "n42"] {
            let mut was_rolled = false;
            for p in 0..=100u8 {
                let now = is_node_rolled("camp-1", node, p);
                if was_rolled {
                    assert!(
                        now,
                        "{node} un-rolled at {p}% — membership must be monotone"
                    );
                }
                was_rolled = now;
            }
            // Everyone is rolled at 100%.
            assert!(
                is_node_rolled("camp-1", node, 100),
                "{node} must roll at 100%"
            );
        }
    }

    #[test]
    fn rollout_percent_controls_the_rolled_fraction() {
        // Over a population, the rolled count grows with the percentage and hits
        // the whole population at 100% (loose bounds — this checks the mechanism,
        // not an exact distribution).
        let nodes: Vec<String> = (0..500).map(|i| format!("node-{i}")).collect();
        let count = |p: u8| {
            nodes
                .iter()
                .filter(|n| is_node_rolled("camp-x", n, p))
                .count()
        };
        let (c10, c50, c100) = (count(10), count(50), count(100));
        assert_eq!(c100, nodes.len(), "100% rolls the whole population");
        assert!(
            c10 < c50 && c50 < c100,
            "rolled count grows with percent: {c10} < {c50} < {c100}"
        );
        // Roughly proportional (generous tolerance to avoid flakiness).
        assert!((20..=110).contains(&c10), "≈10% of 500 rolled, got {c10}");
        assert!((200..=300).contains(&c50), "≈50% of 500 rolled, got {c50}");
    }

    fn rolling_campaign(id: &str, cohorts: Vec<String>, percent: u8, created: u64) -> Campaign {
        // Build a Rolling campaign parked at `percent` (fields are pub; this mirrors
        // a mid-rollout persisted campaign without threading advances). The stage
        // schedule is irrelevant to assignment resolution (which reads
        // `rollout_percent`), so a fixed valid `[100]` avoids the schedule rules.
        let mut c = Campaign::new(id, DIGEST, "v1", cohorts, vec![100], created).unwrap();
        c.state = CampaignState::Rolling;
        c.rollout_percent = percent;
        c
    }

    #[test]
    fn assignment_requires_cohort_intersection() {
        let c = rolling_campaign("camp-1", vec!["canary".into()], 100, 1_000);
        // Node not in the campaign's cohort → no assignment even at 100%.
        let a = resolve_node_assignment("node-1", &["fleet".into()], std::slice::from_ref(&c));
        assert!(!a.rolled);
        assert!(a.artifact_digest.is_none());
        // Node in the cohort → assigned (100% rolls everyone).
        let a = resolve_node_assignment("node-1", &["canary".into()], std::slice::from_ref(&c));
        assert!(a.rolled);
        assert_eq!(a.artifact_digest.as_deref(), Some(DIGEST));
        assert_eq!(a.campaign_id.as_deref(), Some("camp-1"));
    }

    #[test]
    fn staged_and_unrolled_nodes_get_no_assignment() {
        // A Staged campaign (0%) assigns nothing even to its cohort.
        let mut staged = rolling_campaign("camp-s", vec!["canary".into()], 100, 1_000);
        staged.state = CampaignState::Staged;
        staged.rollout_percent = 0;
        let a =
            resolve_node_assignment("node-1", &["canary".into()], std::slice::from_ref(&staged));
        assert!(!a.rolled);

        // A Rolling campaign at a low percent: a node OUTSIDE the rolled bucket
        // gets nothing. Find such a node deterministically.
        let low = rolling_campaign("camp-low", vec!["canary".into()], 1, 1_000);
        let unrolled = (0..1000)
            .map(|i| format!("node-{i}"))
            .find(|n| !is_node_rolled("camp-low", n, 1))
            .expect("some node is outside the 1% bucket");
        let a = resolve_node_assignment(&unrolled, &["canary".into()], std::slice::from_ref(&low));
        assert!(!a.rolled, "{unrolled} is outside the 1% rolled bucket");
    }

    #[test]
    fn newest_matching_campaign_wins() {
        // Two Rolling campaigns at 100% target the node's cohort; the newer one
        // (larger created_at_ms) supersedes.
        let older = rolling_campaign("camp-old", vec!["fleet".into()], 100, 1_000);
        let mut newer = rolling_campaign("camp-new", vec!["fleet".into()], 100, 2_000);
        newer.artifact_version = "v2".into();
        let active = [older, newer];
        let a = resolve_node_assignment("node-9", &["fleet".into()], &active);
        assert_eq!(a.campaign_id.as_deref(), Some("camp-new"));
        assert_eq!(a.artifact_version.as_deref(), Some("v2"));
    }

    #[test]
    fn no_active_campaigns_means_no_assignment() {
        let a = resolve_node_assignment("node-1", &["fleet".into()], &[]);
        assert!(!a.rolled);
        assert_eq!(a.node_id, "node-1");
        assert!(a.campaign_id.is_none());
    }

    // --- fleet campaign summary (observability) ---------------------------

    #[test]
    fn summary_of_empty_set_is_all_zero() {
        let s = summarize_campaigns(&[], &[]);
        assert_eq!(s.total, 0);
        assert!(s.active.is_empty() && s.halted_campaigns.is_empty());
    }

    fn status(node: &str, digest: &str) -> NodeArtifactStatus {
        NodeArtifactStatus {
            node_id: node.into(),
            applied_digest: digest.into(),
            campaign_id: None,
            artifact_version: None,
            reported_at_ms: 1,
        }
    }

    #[test]
    fn summary_counts_adopted_nodes_by_digest() {
        // A Rolling campaign; three nodes reported — two on its digest, one on another.
        let mut rolling =
            Campaign::new("c-roll", DIGEST, "v2", vec!["fleet".into()], vec![50, 100], 1).unwrap();
        rolling.state = CampaignState::Rolling;
        rolling.rollout_percent = 50;
        let other = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let statuses = [
            status("node-a", DIGEST),
            status("node-b", DIGEST),
            status("node-c", other), // still on the old artifact
        ];
        let s = summarize_campaigns(std::slice::from_ref(&rolling), &statuses);
        assert_eq!(s.active.len(), 1);
        assert_eq!(
            s.active[0].applied_nodes, 2,
            "only the two nodes on the campaign's digest count"
        );
        // No reports → zero adoption, never a fabricated number.
        let s0 = summarize_campaigns(std::slice::from_ref(&rolling), &[]);
        assert_eq!(s0.active[0].applied_nodes, 0);
    }

    #[test]
    fn summary_counts_every_state_and_projects_active_and_halted() {
        let draft = Campaign::new("c-draft", DIGEST, "v1", vec!["fleet".into()], vec![50, 100], 1)
            .unwrap();
        let mut staged = draft.clone();
        staged.campaign_id = "c-staged".into();
        staged.state = CampaignState::Staged;
        let mut rolling = draft.clone();
        rolling.campaign_id = "c-rolling".into();
        rolling.artifact_version = "v2".into();
        rolling.state = CampaignState::Rolling;
        rolling.stage_index = 0; // stage 1 of 2
        rolling.rollout_percent = 50;
        let mut completed = draft.clone();
        completed.campaign_id = "c-done".into();
        completed.state = CampaignState::Completed;
        completed.rollout_percent = 100;
        let mut halted = draft.clone();
        halted.campaign_id = "c-halt".into();
        halted.state = CampaignState::Halted;
        halted.halt_reason = Some(HaltReason::PostureLockedOut);
        halted.rollout_percent = 50;

        let s = summarize_campaigns(&[draft, staged, rolling, completed, halted], &[]);
        assert_eq!(s.total, 5);
        assert_eq!(
            (s.draft, s.staged, s.rolling, s.completed, s.halted),
            (1, 1, 1, 1, 1)
        );

        // Active = Staged + Rolling (2), in input order.
        assert_eq!(s.active.len(), 2);
        assert_eq!(s.active[0].campaign_id, "c-staged");
        let roll = &s.active[1];
        assert_eq!(roll.campaign_id, "c-rolling");
        assert_eq!(roll.state, "rolling");
        assert_eq!(roll.artifact_version, "v2");
        assert_eq!((roll.stage, roll.stage_count), (1, 2)); // 1-based stage of 2
        assert_eq!(roll.rollout_percent, 50);

        // Halted projection carries the reason (auto-halt visibility).
        assert_eq!(s.halted_campaigns.len(), 1);
        assert_eq!(s.halted_campaigns[0].campaign_id, "c-halt");
        assert_eq!(s.halted_campaigns[0].halt_reason, "posture_locked_out");
        assert_eq!(s.halted_campaigns[0].rollout_percent, 50);
    }

    #[test]
    fn campaign_metrics_emit_counts_and_active_series() {
        let mut rolling =
            Campaign::new("gov.11", DIGEST, "v2", vec!["fleet".into()], vec![50, 100], 1).unwrap();
        rolling.state = CampaignState::Rolling;
        rolling.rollout_percent = 50;
        let summary =
            summarize_campaigns(std::slice::from_ref(&rolling), &[status("n1", DIGEST)]);
        let m = campaign_metrics_prometheus(&summary);
        assert!(m.contains("kirra_ota_campaigns_total{state=\"rolling\"} 1"));
        assert!(m.contains("kirra_ota_campaigns_total{state=\"draft\"} 0"));
        assert!(m.contains("kirra_ota_campaign_rollout_percent{campaign_id=\"gov.11\"} 50"));
        assert!(m.contains("kirra_ota_campaign_applied_nodes{campaign_id=\"gov.11\"} 1"));
        // Every series carries a HELP + TYPE header (valid exposition).
        assert!(m.contains("# TYPE kirra_ota_campaigns_total gauge"));
    }

    #[test]
    fn campaign_metrics_escape_label_values() {
        // A quote/backslash in an id must be escaped so the exposition stays valid.
        let mut c =
            Campaign::new("a\"b\\c", DIGEST, "v1", vec!["f".into()], vec![100], 1).unwrap();
        c.state = CampaignState::Rolling;
        c.rollout_percent = 100;
        let m = campaign_metrics_prometheus(&summarize_campaigns(std::slice::from_ref(&c), &[]));
        assert!(m.contains("campaign_id=\"a\\\"b\\\\c\""), "got: {m}");
    }

    #[test]
    fn summary_distinguishes_operator_halt_from_regression() {
        let mut op = Campaign::new("c-op", DIGEST, "v1", vec!["fleet".into()], vec![100], 1).unwrap();
        op.state = CampaignState::Halted;
        op.halt_reason = Some(HaltReason::OperatorHalt);
        let s = summarize_campaigns(std::slice::from_ref(&op), &[]);
        assert_eq!(s.halted_campaigns[0].halt_reason, "operator_halt");
    }
}
