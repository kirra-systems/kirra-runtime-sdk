//! The retention driver's acting half — ADR-0040's Tier 1 exit criterion.
//!
//! `kirra_world::retention` decides *what* a retention pass should compact;
//! [`crate::WorldStore::compact_range`] knows *how* to remove it. Until this
//! module, nothing joined them: no code anywhere in the workspace called
//! `compact_range`, which is why the horizons OQ2 ruled had never been enforced
//! and D-20/D-21 measured 15.79 days to fill 8 GiB at 10 Hz.
//!
//! Two pieces live here, and the split is the point:
//!
//! * [`WorldStore::retention_survey`] — asks the log the four questions the pure
//!   policy needs answered, and nothing else. No decision is taken here.
//! * [`WorldStore::run_retention_pass`] — survey → `decide` → act. The only
//!   place in the workspace that calls `compact_range` on a policy's authority.
//!
//! # Retention ages on TRANSACTION time, not valid time
//!
//! The blueprint's records are bitemporal, so the choice has to be made
//! explicitly rather than fall out of whichever column was handy.
//!
//! `txn_time_ms` — when the system *learned* the fact — is the right axis
//! because retention exists to bound **disk**, and disk grows on insertion. It
//! is also monotonic with `generation`, so "older than the cutoff" and "earlier
//! in the log" agree.
//!
//! `valid_from_ms` would be wrong in both directions, and neither failure is
//! hypothetical for a robot: a **backdated** import (a floor plan describing
//! last year) would be eligible for deletion the moment it arrived, and a
//! **future-dated** claim (a scheduled task) would never age out at all. Disk
//! pressure does not care when a fact was true.
//!
//! # What this module does NOT do
//!
//! It does not reclaim space. Deleting rows returns pages to SQLite's free
//! list; the file does not shrink. D-3 measured what `VACUUM` actually costs — a
//! ~1× free-space reserve and a total write blackout — so
//! [`crate::compaction`] deliberately does not call it, and neither does this.
//! A pass that compacts successfully has bounded *growth*, not recovered bytes.

use crate::{StoreError, WorldStore};
use kirra_world::observation::DomainInstant;
use kirra_world::retention::{
    decide, Blocker, CompactablePrefix, Eligibility, RetentionDecision, RetentionPolicy,
    RetentionSurvey,
};
use rusqlite::{params, OptionalExtension};

/// What one retention pass did.
///
/// Carries the [`RetentionDecision`] that produced it, so a caller logging this
/// can say *why* nothing happened — the distinction the four-outcome decision
/// exists to preserve, and which would be thrown away by returning a bare count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPassReport {
    /// What the policy decided, given the survey.
    pub decision: RetentionDecision,
    /// Generations removed, or `None` if the pass compacted nothing.
    pub compacted: Option<(i64, i64)>,
}

impl RetentionPassReport {
    /// Whether retention made no progress **and** something is pinning it.
    ///
    /// The condition worth alerting on, forwarded from
    /// [`RetentionDecision::is_pinned`]: a long-lived protected event can hold a
    /// large raw span, so the store keeps growing while retention truthfully
    /// reports doing nothing.
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.decision.is_pinned()
    }
}

impl WorldStore {
    /// Ask the log what a retention pass has to work with.
    ///
    /// Pure-ish by construction: this only *reads*, and every judgement is left
    /// to [`kirra_world::retention::decide`]. Splitting it this way is what lets
    /// the decision be tested exhaustively without a database — see the survey
    /// tests in `kirra_world::retention`.
    ///
    /// # Errors
    ///
    /// * [`StoreError::Sqlite`] on a query failure.
    /// * [`StoreError::RetentionPolicyRefused`] if the policy refuses the clock
    ///   it was given (a non-wall-clock instant), or if the log's own answers
    ///   contradict each other badly enough that
    ///   [`RetentionSurvey::new`] refuses them — which would mean two of the
    ///   queries below disagree, and acting on that would compact a log nobody
    ///   observed.
    pub fn retention_survey(
        &self,
        policy: &RetentionPolicy,
        now: DomainInstant,
    ) -> Result<RetentionSurvey, StoreError> {
        let cutoff = policy
            .raw_cutoff_ms(now)
            .map_err(StoreError::RetentionPolicyRefused)?;

        let oldest: Option<i64> = self
            .conn
            .query_row("SELECT MIN(generation) FROM world_events", [], |r| r.get(0))
            .optional()?
            .flatten();

        // An empty log is not an error and not "blocked" — there is simply
        // nothing old enough, which is also the correct answer for a store that
        // has never been written to.
        let Some(oldest) = oldest else {
            return RetentionSurvey::new(1, Eligibility::NothingOldEnough)
                .map_err(StoreError::RetentionPolicyRefused);
        };

        // The age question. Cast to i64 saturating: cutoff is a u64 from the
        // pure policy, and txn_time_ms is a signed column. A cutoff beyond
        // i64::MAX would make everything eligible, so it is clamped rather than
        // wrapped — the same fail-closed direction the policy's own saturating
        // subtraction takes.
        let cutoff_i64 = i64::try_from(cutoff).unwrap_or(i64::MAX);
        let newest_past_cutoff: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(generation) FROM world_events WHERE txn_time_ms < ?1",
                params![cutoff_i64],
                |r| r.get(0),
            )
            .optional()?
            .flatten();

        let Some(newest) = newest_past_cutoff else {
            return RetentionSurvey::new(oldest_u64(oldest), Eligibility::NothingOldEnough)
                .map_err(StoreError::RetentionPolicyRefused);
        };

        let prefix = self.compactable_prefix_with_blocker(oldest, newest)?;
        RetentionSurvey::new(
            oldest_u64(oldest),
            Eligibility::Aged {
                newest: oldest_u64(newest),
                prefix,
            },
        )
        .map_err(StoreError::RetentionPolicyRefused)
    }

    /// [`WorldStore::largest_compactable_prefix`], but reporting **which**
    /// refusal stopped it.
    ///
    /// The existing helper returns only the limit, which is all a retry needs.
    /// The policy needs more: `ProtectedClass` and `ProjectionHead` are
    /// different situations — one is a requirement, the other carries §11.3's
    /// pre-agreed escalation — and a driver that cannot tell them apart cannot
    /// act on that difference. See [`Blocker::may_compact_around`].
    fn compactable_prefix_with_blocker(
        &self,
        lo: i64,
        hi: i64,
    ) -> Result<CompactablePrefix, StoreError> {
        let protected: Option<i64> = self
            .conn
            .query_row(
                "SELECT MIN(generation) FROM world_events
                 WHERE generation BETWEEN ?1 AND ?2 AND retention_class <> 'raw'",
                params![lo, hi],
                |r| r.get(0),
            )
            .optional()?
            .flatten();

        let head: Option<i64> = if self.has_projections()? {
            self.conn
                .query_row(
                    "SELECT MIN(e.generation) FROM world_events e
                     JOIN world_current c ON c.generation = e.generation
                     WHERE e.generation BETWEEN ?1 AND ?2",
                    params![lo, hi],
                    |r| r.get(0),
                )
                .optional()?
                .flatten()
        } else {
            None
        };

        // The BINDING blocker is the earliest one — it is what actually stops
        // the prefix. On a tie the protected class wins: it is the stronger
        // refusal (a requirement rather than a conservative simplification), so
        // reporting it never over-promises what the escalation may do.
        let binding = match (protected, head) {
            (None, None) => None,
            (Some(p), None) => Some((p, Blocker::ProtectedClass)),
            (None, Some(h)) => Some((h, Blocker::ProjectionHead)),
            (Some(p), Some(h)) => Some(if p <= h {
                (p, Blocker::ProtectedClass)
            } else {
                (h, Blocker::ProjectionHead)
            }),
        };

        Ok(match binding {
            None => CompactablePrefix::Through {
                through: oldest_u64(hi),
                blocker: None,
            },
            Some((at, blocker)) if at <= lo => CompactablePrefix::Nothing { blocker },
            Some((at, blocker)) => CompactablePrefix::Through {
                through: oldest_u64(at - 1),
                blocker: Some(blocker),
            },
        })
    }

    /// Run one retention pass: survey, decide, act.
    ///
    /// **This is the only call to [`WorldStore::compact_range`] made on a
    /// policy's authority anywhere in the workspace.** Everything else that
    /// compacts does so because a test or an operator asked for a specific
    /// range.
    ///
    /// Idempotent in the sense that matters: running it twice in a row on an
    /// unchanged log compacts once and then reports `NothingOldEnough` or a
    /// blocked outcome, because the first pass removed what was eligible.
    ///
    /// # Errors
    ///
    /// * [`StoreError::RetentionPolicyRefused`] — see
    ///   [`WorldStore::retention_survey`].
    /// * Whatever [`WorldStore::compact_range`] returns. Note that a refusal
    ///   there is **not** expected: the survey already excluded protected
    ///   classes and projection heads from the range, so a
    ///   `ProtectedClassInRange` or `ProjectionHeadInRange` error here means the
    ///   log changed between the survey and the compaction, and is propagated
    ///   rather than swallowed.
    pub fn run_retention_pass(
        &mut self,
        policy: &RetentionPolicy,
        now: DomainInstant,
    ) -> Result<RetentionPassReport, StoreError> {
        let survey = self.retention_survey(policy, now)?;
        let decision = decide(&survey);

        let Some((lo, hi)) = decision.range() else {
            return Ok(RetentionPassReport {
                decision,
                compacted: None,
            });
        };

        let lo = i64::try_from(lo).unwrap_or(i64::MAX);
        let hi = i64::try_from(hi).unwrap_or(i64::MAX);
        let now_ms = i64::try_from(now.ms).unwrap_or(i64::MAX);
        self.compact_range(lo, hi, now_ms)?;

        Ok(RetentionPassReport {
            decision,
            compacted: Some((lo, hi)),
        })
    }
}

/// Generations are `INTEGER PRIMARY KEY` and start at 1, so this is total in
/// practice; a negative generation would mean the schema's own invariant is
/// broken, and clamping to 0 keeps that from becoming a panic in a maintenance
/// path.
fn oldest_u64(g: i64) -> u64 {
    u64::try_from(g).unwrap_or(0)
}

/// Re-exported so a caller driving retention needs one import, not two.
pub use kirra_world::retention::{
    RetentionDecision as Decision, RetentionError as PolicyError, RetentionPolicy as Policy,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClaimStatus, EventId, NewEvent, ObservationId, WriterClass};
    use kirra_world::observation::ClockDomain;
    use kirra_world::retention::RetentionError;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    fn now(ms: i64) -> DomainInstant {
        DomainInstant {
            ms: u64::try_from(ms).unwrap(),
            domain: ClockDomain::System,
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "kirra-world-retention-{}-{}.sqlite",
            name,
            std::process::id()
        ));
        for s in ["", "-wal", "-shm"] {
            let mut q = p.as_os_str().to_os_string();
            q.push(s);
            let _ = std::fs::remove_file(std::path::PathBuf::from(q));
        }
        p
    }

    fn append(
        s: &mut WorldStore,
        id: &str,
        subject: &str,
        txn_time_ms: i64,
        valid_from_ms: i64,
        retention_class: &str,
    ) {
        // One string, two types. The fixture conflates them, as fixtures may —
        // but it now has to say so, rather than the conflation being invisible
        // in a pair of adjacent `&str`s.
        let event_id = EventId::new(id).expect("admissible event id");
        let observation_id = ObservationId::new(id).expect("admissible observation id");
        s.append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms,
            valid_from_ms,
            valid_to_ms: None,
            source: "sensor-a",
            source_version: "0.1.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject,
            predicate: Some("at"),
            object: Some("bench"),
            payload: r#"{"n":1}"#,
            payload_schema: 1,
            retention_class,
            trust: None,
        })
        .expect("append");
    }

    #[test]
    fn an_empty_log_has_nothing_old_enough_rather_than_erroring() {
        // Also the correct answer for a store that has never been written to,
        // which is why this is not an error case.
        let s = WorldStore::open(&tmp("empty")).unwrap();
        let survey = s
            .retention_survey(&RetentionPolicy::oq2(), now(400 * DAY_MS))
            .unwrap();
        assert_eq!(decide(&survey), RetentionDecision::NothingOldEnough);
    }

    #[test]
    fn a_young_log_is_not_eligible() {
        let mut s = WorldStore::open(&tmp("young")).unwrap();
        append(&mut s, "e-1", "a", 10 * DAY_MS, 10 * DAY_MS, "raw");
        let survey = s
            .retention_survey(&RetentionPolicy::oq2(), now(20 * DAY_MS))
            .unwrap();
        assert_eq!(decide(&survey), RetentionDecision::NothingOldEnough);
    }

    #[test]
    fn retention_ages_on_transaction_time_not_valid_time() {
        // THE choice this module documents. A BACKDATED import: recorded now,
        // describing a year ago. Ageing on valid_from_ms would make it eligible
        // for deletion on arrival.
        let mut s = WorldStore::open(&tmp("backdated")).unwrap();
        append(&mut s, "e-1", "a", 400 * DAY_MS, 0, "raw");

        let survey = s
            .retention_survey(&RetentionPolicy::oq2(), now(401 * DAY_MS))
            .unwrap();
        assert_eq!(
            decide(&survey),
            RetentionDecision::NothingOldEnough,
            "a just-recorded event must not be eligible because it describes the past"
        );
    }

    #[test]
    fn a_protected_class_pins_the_prefix_and_is_reported_as_such() {
        // The failure mode the four-outcome decision exists to surface: a
        // long-lived protected event in front of raw traffic. The store keeps
        // growing while retention truthfully reports doing nothing.
        let mut s = WorldStore::open(&tmp("pinned")).unwrap();
        append(&mut s, "e-1", "a", DAY_MS, DAY_MS, "incident");
        append(&mut s, "e-2", "b", 2 * DAY_MS, 2 * DAY_MS, "raw");
        append(&mut s, "e-3", "c", 3 * DAY_MS, 3 * DAY_MS, "raw");

        let survey = s
            .retention_survey(&RetentionPolicy::oq2(), now(100 * DAY_MS))
            .unwrap();
        let d = decide(&survey);
        assert_eq!(
            d,
            RetentionDecision::FullyBlocked {
                blocker: Blocker::ProtectedClass,
                at_generation: 1,
            }
        );
        assert!(d.is_pinned(), "this is the condition worth alerting on");
        // And the escalation §11.3 pre-agreed does NOT apply to this refusal.
        assert!(!Blocker::ProtectedClass.may_compact_around());
    }

    #[test]
    fn a_pass_with_no_projections_compacts_the_whole_eligible_range() {
        // Projections are opt-in (ADR-0041 D-20 keeps a fresh store from
        // materialising them), so with none built there is no head to refuse
        // and the entire aged range is compactable.
        let mut s = WorldStore::open(&tmp("compacts")).unwrap();
        for i in 1..=4 {
            append(
                &mut s,
                &format!("e-{i}"),
                "subj",
                i * DAY_MS,
                i * DAY_MS,
                "raw",
            );
        }
        assert!(!s.has_projections().unwrap());

        let report = s
            .run_retention_pass(&RetentionPolicy::oq2(), now(100 * DAY_MS))
            .expect("pass");

        assert_eq!(report.compacted, Some((1, 4)));
        assert_eq!(report.decision, RetentionDecision::Compact { lo: 1, hi: 4 });
        assert!(!report.is_pinned());
        // The chain still verifies ACROSS the hole — what compaction-with-
        // citation buys, and the reason this is not a plain DELETE.
        s.verify_chain().expect("chain intact across the citation");
    }

    #[test]
    fn a_projection_head_stops_the_prefix_at_the_head() {
        // With projections built, the newest event for a subject IS its current
        // claim, so compaction must stop before it — otherwise
        // rebuild_projections would no longer reproduce the incremental fold.
        let mut s = WorldStore::open(&tmp("head")).unwrap();
        for i in 1..=4 {
            append(
                &mut s,
                &format!("e-{i}"),
                "subj",
                i * DAY_MS,
                i * DAY_MS,
                "raw",
            );
        }
        s.rebuild_projections().expect("build projections");
        assert!(s.has_projections().unwrap());

        let report = s
            .run_retention_pass(&RetentionPolicy::oq2(), now(100 * DAY_MS))
            .expect("pass");

        assert_eq!(report.compacted, Some((1, 3)), "generation 4 is the head");
        assert_eq!(
            report.decision,
            RetentionDecision::CompactPartial {
                lo: 1,
                hi: 3,
                blocker: Blocker::ProjectionHead,
            }
        );
        // Progress, so not pinned — and this refusal is the one §11.3 pre-agreed
        // an escalation for, unlike a protected class.
        assert!(!report.is_pinned());
        assert!(Blocker::ProjectionHead.may_compact_around());
        s.verify_chain().expect("chain intact across the citation");
    }

    #[test]
    fn a_second_pass_on_an_unchanged_log_compacts_nothing() {
        // Idempotent in the sense that matters: the first pass removed what was
        // eligible, so the second has nothing to do and says so.
        let mut s = WorldStore::open(&tmp("idempotent")).unwrap();
        for i in 1..=4 {
            append(
                &mut s,
                &format!("e-{i}"),
                "same-subject",
                i * DAY_MS,
                i * DAY_MS,
                "raw",
            );
        }
        let policy = RetentionPolicy::oq2();
        let first = s.run_retention_pass(&policy, now(100 * DAY_MS)).unwrap();
        assert!(first.compacted.is_some());

        let second = s.run_retention_pass(&policy, now(100 * DAY_MS)).unwrap();
        assert_eq!(second.compacted, None);
    }

    #[test]
    fn a_non_wall_clock_is_refused_rather_than_used() {
        // Retention is the one irreversible maintenance operation here, so the
        // clock-domain rule is enforced at this boundary too, not just in the
        // pure policy.
        let s = WorldStore::open(&tmp("boundary-clock")).unwrap();
        let boundary = DomainInstant {
            ms: 400 * 24 * 60 * 60 * 1000,
            domain: ClockDomain::Boundary,
        };
        assert!(matches!(
            s.retention_survey(&RetentionPolicy::oq2(), boundary),
            Err(StoreError::RetentionPolicyRefused(
                RetentionError::NotWallClock(ClockDomain::Boundary)
            ))
        ));
    }
}
