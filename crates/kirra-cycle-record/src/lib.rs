//! #1215 — the joined cycle record: one artifact per control cycle, assembled
//! from per-stage events on the #1214 evidence-identity chain.
//!
//! # The premise, corrected by the verification gate
//!
//! The issue's framing was "four separate streams that must be correlated by
//! hand after an incident." The gate showed the correlation KEYS already exist
//! end-to-end: the Taj response carries the `PerceptionFrameId`, the plan
//! response binds it to a `proposal_digest`, and the SIGNED release token
//! (`RosBoundCommandPayload`) carries `scan_sequence`, `evidence_digest`,
//! `proposal_digest`, `sequence` and `nonce` all the way to the motor consumer.
//!
//! **The release token is already the join-key carrier to the wheels.** This
//! crate therefore adds no identity plumbing — it is the recorder that exploits
//! the chain that #1214 built. An observability layer, not an architecture
//! change.
//!
//! # Why per-stage events and a joiner, not a shared accumulator
//!
//! No single process witnesses the whole cycle: occy_doer sees perception and
//! the proposal verdict; the interceptor sees the release frame; the motor
//! consumer sees what was actually written. A shared accumulator would need new
//! runtime coupling between them — exactly the kind of channel a flight
//! recorder must not introduce. So each witness emits one small event carrying
//! its join keys, and [`join_cycles`] assembles them offline, deterministically.
//!
//! # Classified, never guessed
//!
//! Carried over from `kirra-replay`, because it earned its place there: a cycle
//! whose release event never arrived is [`Completeness::Partial`] with the
//! missing stages NAMED. It still joins, still replays what it has, and says
//! exactly what it lacks. A joiner that silently invented or dropped stages
//! would be the flight recorder lying about the flight — worse than no recorder,
//! because it would be believed.
//!
//! # The safety boundary
//!
//! This crate holds no authority. Nothing here can influence a verdict, a
//! release, or a motor command; the emitters are fire-and-forget observers and
//! the joiner runs offline. If recording fails, the cycle proceeds — the
//! recorder degrades, never the loop.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Stage events — what each witness can honestly attest
// ---------------------------------------------------------------------------

/// The perception stage, witnessed by occy_doer (it holds the Taj response).
///
/// Carries BOTH caps — raw and stabilized — because #1212 moved stabilization
/// into the checker and the acceptance criteria ask for the pair explicitly:
/// the difference between them is the stabilizer's contribution, and an
/// incident review needs to see whether the enforced cap was the perception's
/// own or the stabilizer holding a stale-but-trusted anchor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerceptionEvent {
    /// Join key: the node-local scan sequence this perception describes.
    pub scan_sequence: u64,
    /// The #1214 frame identity, verbatim from the Taj response.
    pub camera_sequence: Option<u64>,
    pub tracker_generation: u64,
    /// 64 lowercase hex chars; the digest the proposal will be bound against.
    pub evidence_digest: String,
    /// The cap perception computed before stabilization (m/s).
    pub raw_speed_cap_mps: f64,
    /// The cap the checker enforces after stabilization (m/s).
    pub stabilized_speed_cap_mps: f64,
    /// Object count in the frame (shape, not the objects — the record is a
    /// summary; the full frame is reconstructible from the Taj capture stream).
    pub object_count: usize,
    /// Camera channel health as Taj reported it (None = no camera configured).
    pub camera_healthy: Option<bool>,
    /// Wall-clock ms when the witness recorded this stage.
    pub t_wall_ms: u64,
    /// The Taj round trip as the witness measured it (ms).
    pub stage_duration_ms: u64,
}

/// The proposal + slow-loop verdict stage, witnessed by occy_doer (it holds
/// the plan response).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalEvent {
    /// Join key: the scan the proposal's evidence frame described. Taken from
    /// the plan response's ECHOED frame id, not from the request — the echo is
    /// what the planner actually bound.
    pub scan_sequence: u64,
    /// Join key: the deterministic proposal identity the planner minted.
    pub proposal_digest: String,
    /// The evidence digest the planner echoed (must match perception's).
    pub evidence_digest: String,
    /// The slow-loop checker's verdict token (e.g. `Accept`, `MRCFallback`).
    pub verdict: String,
    /// Whether the checker ADMITTED the proposal (`Accept`/`Clamp`).
    pub admitted: bool,
    /// The stable refusal code when refused (`TRAJECTORY_*`), else None.
    pub reason_code: Option<String>,
    pub t_wall_ms: u64,
    /// The planner round trip as the witness measured it (ms).
    pub stage_duration_ms: u64,
}

/// The release stage, witnessed by the interceptor (it relays the signed
/// frame and can decode the `RosBoundCommandPayload` it already republishes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseEvent {
    /// Join keys: the identity chain as the SIGNED TOKEN carries it.
    pub scan_sequence: u64,
    pub proposal_digest: String,
    pub evidence_digest: String,
    /// The release token's own identity: strictly-advancing sequence + nonce.
    pub release_sequence: u64,
    pub release_nonce: u64,
    /// The command the token authorizes.
    pub linear_mps: f64,
    pub angular_rad_s: f64,
    /// Token freshness window end (ms wall clock).
    pub expires_at_ms: u64,
    pub t_wall_ms: u64,
    /// Verifier round trip as the witness measured it (ms).
    pub stage_duration_ms: u64,
}

/// The actuation stage, witnessed by the motor consumer — the only process
/// that knows what was ACTUALLY written to the motor port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActuationEvent {
    /// Join keys: from the verified token the consumer accepted (or refused).
    pub release_sequence: u64,
    pub release_nonce: u64,
    /// What happened at the chokepoint.
    pub outcome: ActuationOutcome,
    /// The command actually issued (present only when actuated — a refusal
    /// issues nothing, and recording a would-have-been command as issued is
    /// exactly the kind of guess this crate refuses to make).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub issued_linear_mps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub issued_angular_rad_s: Option<f64>,
    pub t_wall_ms: u64,
}

/// What the verifying consumer did with a release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActuationOutcome {
    /// Verified and written to the motors.
    Actuated,
    /// Refused at the chokepoint; `reason` is the consumer's stable tag.
    Refused { reason: String },
    /// The SS-002 liveness ramp issued a decel step because releases stopped
    /// arriving — an actuation with no corresponding release.
    LivenessRamp,
}

/// One stage event, as emitted (one JSONL line each). The joiner consumes a
/// stream of these in any order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum StageEvent {
    Perception(PerceptionEvent),
    Proposal(ProposalEvent),
    Release(ReleaseEvent),
    Actuation(ActuationEvent),
}

// ---------------------------------------------------------------------------
// The joined record
// ---------------------------------------------------------------------------

/// The stages a cycle can be missing. Named individually so a partial record
/// says exactly what it lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Perception,
    Proposal,
    Release,
    Actuation,
}

/// Whether the cycle assembled completely.
///
/// `Partial` is a CLASSIFICATION, not a failure: a cycle the checker refused at
/// the proposal stage legitimately has no release and no actuation — the
/// missing stages are the finding, not noise. The joiner never fabricates a
/// stage to make a record look whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Partial { missing: Vec<Stage> },
}

/// A cross-stage identity disagreement found while joining.
///
/// Surfaced ON the record rather than as a joiner error, because a mismatch is
/// precisely the kind of evidence an incident review exists to see — a joiner
/// that refused to emit the record would destroy the finding it made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinAnomaly {
    /// Which field disagreed (e.g. `evidence_digest`).
    pub field: String,
    /// The two values, labelled by the stages that carried them.
    pub detail: String,
}

/// One control cycle, joined. THE artifact the issue asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinedCycleRecord {
    /// The cycle's primary key: the scan the whole chain descends from.
    pub scan_sequence: u64,
    pub completeness: Completeness,
    /// Identity disagreements found while joining (empty = chain consistent).
    pub anomalies: Vec<JoinAnomaly>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub perception: Option<PerceptionEvent>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub proposal: Option<ProposalEvent>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub release: Option<ReleaseEvent>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub actuation: Option<ActuationEvent>,
}

impl JoinedCycleRecord {
    /// Which trigger conditions this cycle satisfies (empty = unremarkable).
    #[must_use]
    pub fn triggers(&self) -> Vec<CaptureTrigger> {
        let mut out = Vec::new();
        if let Some(p) = &self.proposal {
            if !p.admitted {
                out.push(CaptureTrigger::DenyVerdict);
            }
            if p.verdict == "MRCFallback" {
                out.push(CaptureTrigger::MrcEntry);
            }
        }
        if let Some(a) = &self.actuation {
            match &a.outcome {
                ActuationOutcome::Refused { .. } => out.push(CaptureTrigger::TokenRejection),
                ActuationOutcome::LivenessRamp => out.push(CaptureTrigger::LivenessRamp),
                ActuationOutcome::Actuated => {}
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The joiner
// ---------------------------------------------------------------------------

/// Join a stream of stage events (any order) into per-cycle records, keyed on
/// the scan sequence, then linked stage-to-stage on the digests and the
/// release identity.
///
/// Deterministic and pure: same events in, same records out, regardless of
/// arrival order. Later duplicates of a stage for the same key are refused
/// into `anomalies` rather than silently overwriting — first-wins, because the
/// first event is the one closest in time to the stage it describes.
#[must_use]
pub fn join_cycles(events: &[StageEvent]) -> Vec<JoinedCycleRecord> {
    // Pass 1: index by primary key. Actuation events carry no scan_sequence —
    // they join through the release identity — so index releases both ways.
    let mut by_scan: BTreeMap<u64, JoinedCycleRecord> = BTreeMap::new();
    let mut release_to_scan: BTreeMap<(u64, u64), u64> = BTreeMap::new();
    let mut orphan_actuations: Vec<ActuationEvent> = Vec::new();

    let entry = |map: &mut BTreeMap<u64, JoinedCycleRecord>, scan: u64| {
        map.entry(scan).or_insert_with(|| JoinedCycleRecord {
            scan_sequence: scan,
            completeness: Completeness::Partial {
                missing: Vec::new(),
            },
            anomalies: Vec::new(),
            perception: None,
            proposal: None,
            release: None,
            actuation: None,
        });
    };

    for ev in events {
        match ev {
            StageEvent::Perception(p) => {
                entry(&mut by_scan, p.scan_sequence);
                let rec = by_scan.get_mut(&p.scan_sequence).expect("inserted");
                if rec.perception.is_some() {
                    rec.anomalies.push(JoinAnomaly {
                        field: "perception".to_string(),
                        detail: format!(
                            "duplicate perception event for scan {}; first kept",
                            p.scan_sequence
                        ),
                    });
                } else {
                    rec.perception = Some(p.clone());
                }
            }
            StageEvent::Proposal(pr) => {
                entry(&mut by_scan, pr.scan_sequence);
                let rec = by_scan.get_mut(&pr.scan_sequence).expect("inserted");
                if rec.proposal.is_some() {
                    rec.anomalies.push(JoinAnomaly {
                        field: "proposal".to_string(),
                        detail: format!(
                            "duplicate proposal event for scan {}; first kept",
                            pr.scan_sequence
                        ),
                    });
                } else {
                    rec.proposal = Some(pr.clone());
                }
            }
            StageEvent::Release(r) => {
                entry(&mut by_scan, r.scan_sequence);
                release_to_scan.insert((r.release_sequence, r.release_nonce), r.scan_sequence);
                let rec = by_scan.get_mut(&r.scan_sequence).expect("inserted");
                if rec.release.is_some() {
                    rec.anomalies.push(JoinAnomaly {
                        field: "release".to_string(),
                        detail: format!(
                            "duplicate release event for scan {}; first kept",
                            r.scan_sequence
                        ),
                    });
                } else {
                    rec.release = Some(r.clone());
                }
            }
            StageEvent::Actuation(a) => orphan_actuations.push(a.clone()),
        }
    }

    // Pass 2: attach actuations through the release identity.
    for a in orphan_actuations {
        match release_to_scan.get(&(a.release_sequence, a.release_nonce)) {
            Some(&scan) => {
                let rec = by_scan.get_mut(&scan).expect("release indexed");
                if rec.actuation.is_some() {
                    rec.anomalies.push(JoinAnomaly {
                        field: "actuation".to_string(),
                        detail: format!(
                            "duplicate actuation for release ({}, {}); first kept",
                            a.release_sequence, a.release_nonce
                        ),
                    });
                } else {
                    rec.actuation = Some(a);
                }
            }
            None => {
                // An actuation with no witnessed release. It still becomes a
                // record — keyed scan 0 would LIE, so it gets its own record
                // keyed on nothing and classified as missing everything above
                // it. The liveness ramp is the legitimate instance of this
                // shape: the consumer acted (a decel step) precisely BECAUSE
                // no release arrived.
                let mut rec = JoinedCycleRecord {
                    scan_sequence: 0,
                    completeness: Completeness::Partial {
                        missing: Vec::new(),
                    },
                    anomalies: vec![JoinAnomaly {
                        field: "release".to_string(),
                        detail: format!(
                            "actuation for release ({}, {}) has no witnessed release event",
                            a.release_sequence, a.release_nonce
                        ),
                    }],
                    perception: None,
                    proposal: None,
                    release: None,
                    actuation: Some(a),
                };
                finalize(&mut rec);
                // Keyed negatively below the real scans via a sentinel entry:
                // BTreeMap needs a key; use the release sequence's complement
                // to keep orphans distinct and ordered after nothing real.
                // (scan_sequence stays 0 on the record itself — honest.)
                let k = u64::MAX - rec.actuation.as_ref().map_or(0, |x| x.release_sequence);
                by_scan.insert(k, rec);
            }
        }
    }

    // Pass 3: verify the chain agreement and classify completeness.
    let mut out: Vec<JoinedCycleRecord> = by_scan.into_values().collect();
    for rec in &mut out {
        check_chain(rec);
        finalize(rec);
    }
    out
}

/// Cross-stage identity agreement. Disagreements are recorded, never repaired.
fn check_chain(rec: &mut JoinedCycleRecord) {
    if let (Some(p), Some(pr)) = (&rec.perception, &rec.proposal) {
        if p.evidence_digest != pr.evidence_digest {
            rec.anomalies.push(JoinAnomaly {
                field: "evidence_digest".to_string(),
                detail: format!(
                    "perception carries {} but the proposal was bound to {}",
                    p.evidence_digest, pr.evidence_digest
                ),
            });
        }
    }
    if let (Some(pr), Some(r)) = (&rec.proposal, &rec.release) {
        if pr.proposal_digest != r.proposal_digest {
            rec.anomalies.push(JoinAnomaly {
                field: "proposal_digest".to_string(),
                detail: format!(
                    "proposal minted {} but the release token carries {}",
                    pr.proposal_digest, r.proposal_digest
                ),
            });
        }
        if pr.evidence_digest != r.evidence_digest {
            rec.anomalies.push(JoinAnomaly {
                field: "evidence_digest".to_string(),
                detail: format!(
                    "proposal bound {} but the release token carries {}",
                    pr.evidence_digest, r.evidence_digest
                ),
            });
        }
    }
}

fn finalize(rec: &mut JoinedCycleRecord) {
    let mut missing = Vec::new();
    if rec.perception.is_none() {
        missing.push(Stage::Perception);
    }
    if rec.proposal.is_none() {
        missing.push(Stage::Proposal);
    }
    if rec.release.is_none() {
        missing.push(Stage::Release);
    }
    if rec.actuation.is_none() {
        missing.push(Stage::Actuation);
    }
    rec.completeness = if missing.is_empty() {
        Completeness::Complete
    } else {
        Completeness::Partial { missing }
    };
}

// ---------------------------------------------------------------------------
// Triggers + the ring buffer
// ---------------------------------------------------------------------------

/// The conditions that promote a ring snapshot to a persisted incident capture.
///
/// An explicit enum (the `FLOOR_STATES` idiom): adding a trigger is a
/// deliberate decision about what counts as an incident, not a string that
/// appears one day in a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CaptureTrigger {
    /// The slow-loop checker refused the proposal.
    DenyVerdict,
    /// The verifying consumer refused a release token.
    TokenRejection,
    /// A stage witnessed a stale perception frame.
    StaleFrame,
    /// The checker fell back to the MRC envelope.
    MrcEntry,
    /// A cycle stage missed its deadline (the #1218 wall-clock budget).
    DeadlineMiss,
    /// The consumer's SS-002 ramp engaged because releases stopped arriving.
    LivenessRamp,
}

/// The ring capacity, in EVENTS (not cycles).
///
/// THE BOUND, STATED (the acceptance criterion asks for it explicitly): at the
/// deployed 10 Hz plan rate with 4 stages per healthy cycle, 4096 events is
/// ~102 s of history — comfortably spanning the pre-event window an incident
/// review needs, while bounding worst-case memory at
/// `RING_CAPACITY × sizeof(largest event ≈ 300 B) ≈ 1.2 MiB`.
pub const RING_CAPACITY: usize = 4096;

/// How many events after a trigger are still captured before the snapshot is
/// sealed. Post-event context shows what the SYSTEM DID about the incident
/// (the MRC hold, the recovery), which is half of what a review needs.
pub const POST_TRIGGER_EVENTS: usize = 256;

/// A bounded pre/post-trigger event ring.
///
/// Pure and clock-free: the caller pushes events and reports triggers; the
/// ring decides what to retain. No I/O — persistence is the caller's concern,
/// so this stays testable without a filesystem and reusable across the Python
/// and Rust witnesses' collector.
#[derive(Debug)]
pub struct CycleRing {
    buf: Vec<StageEvent>,
    /// Total events ever pushed (so eviction is observable, not silent).
    pushed: u64,
    evicted: u64,
    /// When Some, a trigger fired and we are counting down post-event capture.
    armed: Option<PendingCapture>,
}

#[derive(Debug)]
struct PendingCapture {
    trigger: CaptureTrigger,
    remaining_post_events: usize,
}

/// A sealed incident snapshot: everything the ring held when the post-trigger
/// window closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncidentCapture {
    pub trigger: CaptureTrigger,
    /// Events, oldest first, joined into cycles for the artifact.
    pub cycles: Vec<JoinedCycleRecord>,
    /// How many events had been evicted from the ring before sealing — nonzero
    /// means the pre-event window was longer than the ring; stated rather than
    /// silent, per the no-silent-caps rule.
    pub events_evicted_before_capture: u64,
}

impl Default for CycleRing {
    fn default() -> Self {
        Self::new()
    }
}

impl CycleRing {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(RING_CAPACITY),
            pushed: 0,
            evicted: 0,
            armed: None,
        }
    }

    /// Push one event. Returns a sealed capture if a pending trigger's
    /// post-event window just closed.
    pub fn push(&mut self, event: StageEvent) -> Option<IncidentCapture> {
        if self.buf.len() == RING_CAPACITY {
            self.buf.remove(0);
            self.evicted += 1;
        }
        self.buf.push(event);
        self.pushed += 1;

        if let Some(pending) = &mut self.armed {
            if pending.remaining_post_events == 0 {
                return self.seal();
            }
            pending.remaining_post_events -= 1;
            if pending.remaining_post_events == 0 {
                return self.seal();
            }
        }
        None
    }

    /// Report a trigger. If a capture is already pending, the earlier trigger
    /// wins (its pre-event window is the one closest to the cause); the later
    /// trigger's cycles are inside the same window anyway.
    pub fn trigger(&mut self, trigger: CaptureTrigger) {
        if self.armed.is_none() {
            self.armed = Some(PendingCapture {
                trigger,
                remaining_post_events: POST_TRIGGER_EVENTS,
            });
        }
    }

    /// Seal immediately (shutdown path): whatever post-window remains is cut
    /// short rather than lost.
    pub fn seal_now(&mut self) -> Option<IncidentCapture> {
        if self.armed.is_some() {
            self.seal()
        } else {
            None
        }
    }

    fn seal(&mut self) -> Option<IncidentCapture> {
        let pending = self.armed.take()?;
        Some(IncidentCapture {
            trigger: pending.trigger,
            cycles: join_cycles(&self.buf),
            events_evicted_before_capture: self.evicted,
        })
    }

    /// Observability: how many events have been evicted so far.
    #[must_use]
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

// ---------------------------------------------------------------------------
// JSONL — the emitter/joiner wire format
// ---------------------------------------------------------------------------

/// Parse a JSONL stream of stage events. Malformed lines are returned with
/// their line numbers rather than dropped — a recorder that silently skips
/// evidence is the failure mode this crate exists to prevent.
pub fn parse_events_jsonl(jsonl: &str) -> (Vec<StageEvent>, Vec<(usize, String)>) {
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for (idx, line) in jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<StageEvent>(line) {
            Ok(ev) => events.push(ev),
            Err(e) => errors.push((idx + 1, e.to_string())),
        }
    }
    (events, errors)
}

#[cfg(test)]
mod tests;
