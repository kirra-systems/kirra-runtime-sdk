//! Chain-consistency replay over joined cycle records (option 1 of the #1215
//! replay design).
//!
//! # The replay boundary, stated explicitly
//!
//! ```text
//! joined-cycle replay (THIS module)
//!     verifies cross-process chain consistency and incident chronology
//! gateway/per-stream replay (kirra-replay's existing path)
//!     recomputes verifier decisions from complete captured inputs
//! combined review
//!     correlates the two through the release-token identifiers
//! ```
//!
//! The joined record proves the AUTHORITY CHAIN held — it does not reproduce
//! the checker's internal computation, and does not need to in order to
//! justify its existence. The questions it answers are exactly the ones that
//! previously required hand correlation across four streams.
//!
//! # The acceptance property
//!
//! > For any joined record, replay deterministically reproduces its
//! > completeness classification and every recorded chain-consistency finding,
//! > independent of event arrival order, without inventing missing stage data.
//!
//! Realized by [`verify_record`]: it RE-DERIVES completeness and the chain
//! anomalies from the record's own embedded stages and compares them with what
//! the artifact claims — so a hand-edited or truncated artifact is detected,
//! not trusted — then runs the cross-stage findings the chain makes checkable.

use serde::{Deserialize, Serialize};

use crate::{ActuationOutcome, Completeness, JoinedCycleRecord, Stage, RESERVED_UNBOUND_SCAN};

/// One cross-stage consistency finding. Every variant names the values it
/// compared, so a finding is actionable without re-opening the raw streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "finding", rename_all = "snake_case")]
pub enum ConsistencyFinding {
    /// The release authorized a command faster than the stabilized cap the
    /// perception stage reported. The single highest-value cross-stage check:
    /// it spans three processes and no single stream can see it.
    ReleasedCommandExceedsStabilizedCap {
        released_linear_mps: f64,
        stabilized_cap_mps: f64,
    },
    /// A release exists for a proposal the checker did NOT admit. The chain's
    /// core promise is that admission gates release; this is its violation.
    ReleaseWithoutAdmission { verdict: String },
    /// The consumer reports actuating a different command than the release
    /// authorized. Compared bit-for-bit — "actuated what was authorized"
    /// admits no epsilon.
    ActuationDiffersFromRelease {
        authorized_linear_mps: f64,
        issued_linear_mps: f64,
        authorized_angular_rad_s: f64,
        issued_angular_rad_s: f64,
    },
    /// A refused actuation carries issued_* values. A refusal issues nothing;
    /// a record claiming otherwise has invented data.
    RefusalClaimsIssuedCommand,
    /// The artifact's recorded completeness disagrees with what its own
    /// embedded stages imply — the artifact has been edited or corrupted.
    CompletenessMisstated {
        recorded: Completeness,
        derived: Completeness,
    },
    /// A chain anomaly derivable from the embedded stages is absent from the
    /// artifact's recorded anomalies (or vice versa) — same tamper class.
    AnomalySetMisstated {
        recorded_count: usize,
        derived_count: usize,
    },
    /// A genuine (non-sentinel) record is missing an upstream stage that a
    /// present downstream stage requires: a release with no proposal, an
    /// actuation with no release. Downstream evidence without its upstream
    /// cause is the chronology violation the chain exists to expose.
    DownstreamWithoutUpstream { present: Stage, missing: Stage },
    /// The gateway capture reference's digest does not match the release's own
    /// proposal digest — the ref points at another cycle's capture.
    CaptureRefDigestMismatch {
        ref_digest: String,
        release_digest: String,
    },
}

/// Derive completeness from the stages actually present (the same rule the
/// joiner applies — duplicated here deliberately, since replay must not trust
/// the artifact's own claim to verify the artifact's own claim).
fn derive_completeness(rec: &JoinedCycleRecord) -> Completeness {
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
    if missing.is_empty() {
        Completeness::Complete
    } else {
        Completeness::Partial { missing }
    }
}

/// Count the chain anomalies derivable from the embedded stages alone
/// (digest disagreements — the joiner's `check_chain` findings).
fn derive_chain_anomaly_count(rec: &JoinedCycleRecord) -> usize {
    let mut n = 0;
    if let (Some(p), Some(pr)) = (&rec.perception, &rec.proposal) {
        if p.evidence_digest != pr.evidence_digest {
            n += 1;
        }
    }
    if let (Some(pr), Some(r)) = (&rec.proposal, &rec.release) {
        if pr.proposal_digest != r.proposal_digest {
            n += 1;
        }
        if pr.evidence_digest != r.evidence_digest {
            n += 1;
        }
    }
    n
}

/// Verify one record against itself: re-derive what the artifact claims, then
/// run the cross-stage consistency checks. Pure and deterministic.
#[must_use]
pub fn verify_record(rec: &JoinedCycleRecord) -> Vec<ConsistencyFinding> {
    let mut findings = Vec::new();

    // --- the artifact's own claims, re-derived ---
    let derived = derive_completeness(rec);
    if derived != rec.completeness {
        findings.push(ConsistencyFinding::CompletenessMisstated {
            recorded: rec.completeness.clone(),
            derived,
        });
    }
    let derived_anoms = derive_chain_anomaly_count(rec);
    // Recorded anomalies include join-time-only classes (duplicates, sentinel
    // violations, orphaned actuations) that are NOT derivable from the joined
    // record, so the recorded set may legitimately be LARGER — but never
    // smaller than what the embedded stages themselves prove.
    if rec.anomalies.len() < derived_anoms {
        findings.push(ConsistencyFinding::AnomalySetMisstated {
            recorded_count: rec.anomalies.len(),
            derived_count: derived_anoms,
        });
    }

    // --- cross-stage chronology ---
    if rec.scan_sequence != RESERVED_UNBOUND_SCAN {
        if rec.release.is_some() && rec.proposal.is_none() {
            findings.push(ConsistencyFinding::DownstreamWithoutUpstream {
                present: Stage::Release,
                missing: Stage::Proposal,
            });
        }
        if rec.actuation.is_some() && rec.release.is_none() {
            findings.push(ConsistencyFinding::DownstreamWithoutUpstream {
                present: Stage::Actuation,
                missing: Stage::Release,
            });
        }
    }

    // --- admission gates release ---
    if let (Some(pr), Some(_)) = (&rec.proposal, &rec.release) {
        if !pr.admitted {
            findings.push(ConsistencyFinding::ReleaseWithoutAdmission {
                verdict: pr.verdict.clone(),
            });
        }
    }

    // --- the released command respects the stabilized cap ---
    if let (Some(p), Some(r)) = (&rec.perception, &rec.release) {
        if r.linear_mps.abs() > p.stabilized_speed_cap_mps {
            findings.push(ConsistencyFinding::ReleasedCommandExceedsStabilizedCap {
                released_linear_mps: r.linear_mps,
                stabilized_cap_mps: p.stabilized_speed_cap_mps,
            });
        }
    }

    // --- the consumer actuated what was authorized ---
    if let (Some(r), Some(a)) = (&rec.release, &rec.actuation) {
        match &a.outcome {
            ActuationOutcome::Actuated => {
                let issued_l = a.issued_linear_mps.unwrap_or(f64::NAN);
                let issued_a = a.issued_angular_rad_s.unwrap_or(f64::NAN);
                if issued_l.to_bits() != r.linear_mps.to_bits()
                    || issued_a.to_bits() != r.angular_rad_s.to_bits()
                {
                    findings.push(ConsistencyFinding::ActuationDiffersFromRelease {
                        authorized_linear_mps: r.linear_mps,
                        issued_linear_mps: issued_l,
                        authorized_angular_rad_s: r.angular_rad_s,
                        issued_angular_rad_s: issued_a,
                    });
                }
            }
            ActuationOutcome::Refused { .. } => {
                if a.issued_linear_mps.is_some() || a.issued_angular_rad_s.is_some() {
                    findings.push(ConsistencyFinding::RefusalClaimsIssuedCommand);
                }
            }
            ActuationOutcome::LivenessRamp => {}
        }
    }

    // --- the option-3 reference, when present, binds THIS cycle ---
    if let Some(r) = &rec.release {
        if let Some(cap_ref) = &r.gateway_capture_ref {
            if cap_ref.proposal_digest != r.proposal_digest {
                findings.push(ConsistencyFinding::CaptureRefDigestMismatch {
                    ref_digest: cap_ref.proposal_digest.clone(),
                    release_digest: r.proposal_digest.clone(),
                });
            }
        }
    }

    findings
}

/// The replay summary over a whole artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinedReplaySummary {
    pub records: usize,
    pub complete: usize,
    pub partial: usize,
    /// (scan_sequence, findings) for every record with at least one finding.
    pub findings: Vec<(u64, Vec<ConsistencyFinding>)>,
}

impl JoinedReplaySummary {
    /// The exit-code question: did every record verify clean?
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Verify a whole joined artifact.
#[must_use]
pub fn verify_artifact(records: &[JoinedCycleRecord]) -> JoinedReplaySummary {
    let mut summary = JoinedReplaySummary {
        records: records.len(),
        complete: 0,
        partial: 0,
        findings: Vec::new(),
    };
    for rec in records {
        match rec.completeness {
            Completeness::Complete => summary.complete += 1,
            Completeness::Partial { .. } => summary.partial += 1,
        }
        let findings = verify_record(rec);
        if !findings.is_empty() {
            summary.findings.push((rec.scan_sequence, findings));
        }
    }
    summary
}
