//! The projection rebuild-and-swap protocol (ADR-0041 R2), as a pure state
//! machine.
//!
//! # What this is and is not
//!
//! This is the **protocol**, not the rebuild. It owns the question "given what
//! has happened so far, what is this projection allowed to do?" and nothing
//! else — no SQL, no I/O, no schema. The fold itself lives in
//! [`crate::standin::Store::fold_from`]; this decides when a fold's output may
//! be believed.
//!
//! Separating them is the point. The dangerous transitions are all decisions,
//! not computations: *may this projection serve queries yet*, *is this
//! verification still valid*, *what survives a crash*. A decision table that
//! cannot touch a database can be tested exhaustively, and every one of the
//! invariants below is a test rather than a review comment.
//!
//! **It is not Kirra World domain logic.** It is a protocol prototype in the
//! measurement harness, which is fenced from `kirra-world` and carries a
//! stand-in schema. ADR-0042 Decision 5 still gates domain implementation.
//!
//! # The problem the states exist to solve
//!
//! A rebuild folds from generation 0 forward while **ingest continues**. So the
//! new projection is chasing a head that keeps moving, and two things go wrong
//! if the protocol is just "build, then swap":
//!
//! 1. **Catch-up need not converge.** If ingest outruns the fold, "wait until
//!    caught up" is a hang, not a step. [`catchup_is_converging`] makes
//!    non-convergence a failure rather than a stall.
//! 2. **A verification goes stale the instant an event arrives.** Equivalence
//!    is proven *at a generation*. If the log advances between proving it and
//!    acting on it, the thing that was verified is not the thing being
//!    activated. [`RebuildState::on_append`] demotes `Verified` back to
//!    `Building` for exactly this reason — the single most important transition
//!    here, and the one a hand-rolled implementation would omit. It demotes to
//!    `Building` rather than `CaughtUp` because the projection is now genuinely
//!    behind the head by the appended event: it has to fold again, not merely
//!    be re-proven.
//!
//! # Open question 7
//!
//! ADR-0041 open question 7 asks what marks a projection left partial by an
//! interrupted fold. The answer is this state: a projection is authoritative
//! **only** in [`RebuildState::Active`] ([`RebuildState::is_authoritative`]).
//! A fold refused half-way under disk pressure leaves the state at `Building`,
//! which is not authoritative and cannot cut over — the marker OQ7 said was
//! missing is the protocol state itself.
//!
//! # Why this module has no caller
//!
//! It is exercised by its tests and by nothing else, deliberately. The thing it
//! would drive is a store that implements the durable rebuild-state record and
//! the atomic swap (design §8, S-1/S-2), and that store is gated by ADR-0042
//! Decision 5. Wiring it into the harness's `migrate` command instead — the one
//! nearby thing it would compile against — would produce records that *look*
//! like alongside-rebuild evidence for a run that is a single-pass backfill, and
//! a plausible label on a real measurement is the failure mode this project has
//! had to correct twice. So: `dead_code` is allowed here, and the allowance is
//! the honest statement rather than the shortcut.
#![allow(dead_code)]

/// Why a rebuild attempt was abandoned. Recorded rather than collapsed to a
/// bare "failed", because the operator response differs: a diverging catch-up
/// means ingest outruns the fold (shed or throttle), a failed equivalence check
/// means the fold is not deterministic (a defect, not a capacity problem).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureReason {
    /// Catch-up was not converging — ingest is outrunning the fold.
    CatchupDiverged,
    /// The rebuilt projection did not match the incremental one at the pinned
    /// generation. This is a determinism defect and must not be retried blindly.
    EquivalenceMismatch { at: u64 },
    /// The fold could not proceed — disk pressure, I/O error.
    FoldRefused(String),
    /// The operator abandoned the attempt.
    Abandoned,
}

/// What a rebuild attempt is permitted to do right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuildState {
    /// Folding forward. The projection is **partial** — this is OQ7's marker.
    Building { folded_to: u64 },
    /// The fold reached the log head at generation `at`. Not yet trusted: the
    /// head may already have moved, and nothing has been compared.
    CaughtUp { at: u64 },
    /// Equivalence was proven against the incremental projection **at
    /// generation `at`**. Valid only while the log head is still `at`.
    Verified { at: u64 },
    /// Serving queries. The only authoritative state, and terminal.
    Active,
    /// This attempt is over. The previous projection remains active — failure
    /// never removes the thing that was working.
    Failed(FailureReason),
}

/// Refused transitions carry what was attempted, so a caller cannot log
/// "transition failed" without saying which one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionRefused {
    pub from: RebuildState,
    pub attempted: &'static str,
    pub because: String,
}

type Step = Result<RebuildState, TransitionRefused>;

impl RebuildState {
    /// A fresh attempt. Always starts partial — there is no state in which a
    /// projection is trusted before anything has been folded into it.
    pub fn begin() -> Self {
        Self::Building { folded_to: 0 }
    }

    /// **The single question every reader of a projection must ask.**
    ///
    /// Only `Active`. A `Building` projection is partial, a `CaughtUp` one is
    /// unverified, and a `Verified` one has been proven but not yet swapped in
    /// — none of them may answer a query.
    pub fn is_authoritative(&self) -> bool {
        matches!(self, Self::Active)
    }

    fn refuse(&self, attempted: &'static str, because: impl Into<String>) -> TransitionRefused {
        TransitionRefused {
            from: self.clone(),
            attempted,
            because: because.into(),
        }
    }

    /// The fold advanced to `position`.
    pub fn on_fold_progress(&self, position: u64) -> Step {
        match self {
            Self::Building { folded_to } if position < *folded_to => Err(self.refuse(
                "fold_progress",
                format!("fold position went backwards: {folded_to} -> {position}"),
            )),
            Self::Building { .. } => Ok(Self::Building {
                folded_to: position,
            }),
            _ => Err(self.refuse("fold_progress", "only a Building projection folds")),
        }
    }

    /// The fold reached the log head, which was at `head` when observed.
    ///
    /// Requires the fold position to **equal** `head`. Folding *past* it is
    /// impossible; folding *short* of it and calling this anyway would claim a
    /// catch-up that did not happen — and since `CaughtUp` is one
    /// `on_equivalence_proven` away from `Verified` and two from `Active`, that
    /// claim would activate a projection folded to 400 while asserting it is
    /// current at 900. The state machine does not assume its caller has folded
    /// as far as it says.
    pub fn on_reached_head(&self, head: u64) -> Step {
        match self {
            Self::Building { folded_to } if *folded_to != head => Err(self.refuse(
                "reached_head",
                if *folded_to > head {
                    format!("folded past the head it claims to have reached: {folded_to} > {head}")
                } else {
                    format!(
                        "not caught up: folded to {folded_to}, head is {head}. \
                         Keep folding — claiming this would put an unfolded projection \
                         two transitions from serving"
                    )
                },
            )),
            Self::Building { .. } => Ok(Self::CaughtUp { at: head }),
            _ => Err(self.refuse("reached_head", "only a Building projection can catch up")),
        }
    }

    /// An event was appended to the log at generation `head`.
    ///
    /// **This is the transition that makes the protocol correct.** A `Verified`
    /// projection was proven equivalent *at a generation*; once the log moves,
    /// that proof describes a state that is no longer current, so the
    /// projection drops back to `Building` and must be folded and verified
    /// again. Omitting this is how an alongside-rebuild silently activates a
    /// projection that was never checked against the data it is about to serve.
    ///
    /// It demotes to `Building`, not `CaughtUp`: the appended event puts the
    /// projection genuinely behind the head, so there is fold work to do and
    /// not merely a proof to retake.
    pub fn on_append(&self, head: u64) -> Step {
        // The log head cannot land behind work already folded out of it. If it
        // does, the generation source is not monotonic (design §8, S-3) and
        // every staleness comparison below is meaningless — so this is refused
        // rather than absorbed.
        let position = match self {
            Self::Building { folded_to } => Some(*folded_to),
            Self::CaughtUp { at } | Self::Verified { at } => Some(*at),
            Self::Active | Self::Failed(_) => None,
        };
        if let Some(position) = position {
            if head < position {
                return Err(self.refuse(
                    "append",
                    format!("log head went backwards behind folded position: {head} < {position}"),
                ));
            }
        }
        match self {
            // Already partial; the append simply puts it further behind.
            Self::Building { folded_to } => Ok(Self::Building {
                folded_to: *folded_to,
            }),
            // Fell behind again — back to folding.
            Self::CaughtUp { at } => Ok(Self::Building { folded_to: *at }),
            // The proof is stale. Demote, do not fail: nothing is wrong, the
            // world simply moved.
            Self::Verified { at } => Ok(Self::Building { folded_to: *at }),
            // The live projection is updated incrementally; appends are normal.
            Self::Active => Ok(Self::Active),
            Self::Failed(_) => Err(self.refuse("append", "attempt is over")),
        }
    }

    /// Equivalence was proven at generation `at`.
    ///
    /// Refused unless the projection is caught up to exactly that generation —
    /// a proof at some other generation is a proof about something else.
    pub fn on_equivalence_proven(&self, at: u64) -> Step {
        match self {
            Self::CaughtUp { at: caught } if *caught == at => Ok(Self::Verified { at }),
            Self::CaughtUp { at: caught } => Err(self.refuse(
                "equivalence_proven",
                format!("proof is at generation {at} but the projection is caught up to {caught}"),
            )),
            _ => Err(self.refuse(
                "equivalence_proven",
                "only a caught-up projection can be verified",
            )),
        }
    }

    /// Swap this projection in as the authoritative one.
    ///
    /// Refused from every state but `Verified`, and refused there unless the
    /// log head is still the generation the proof was taken at.
    pub fn on_cutover(&self, head: u64) -> Step {
        match self {
            Self::Verified { at } if *at == head => Ok(Self::Active),
            Self::Verified { at } => Err(self.refuse(
                "cutover",
                format!("verified at generation {at} but the head is now {head}; re-verify"),
            )),
            _ => Err(self.refuse(
                "cutover",
                "only a verified projection may cut over — this is the rule OQ7 asked for",
            )),
        }
    }

    pub fn on_failure(&self, reason: FailureReason) -> Step {
        match self {
            Self::Active => Err(self.refuse(
                "failure",
                "an active projection is not a rebuild attempt; retire it deliberately",
            )),
            Self::Failed(_) => Err(self.refuse("failure", "already failed")),
            _ => Ok(Self::Failed(reason)),
        }
    }

    /// What this attempt becomes after a process restart.
    ///
    /// The fold is deterministic and its position is durable, so `Building`
    /// resumes rather than restarts. `Verified` **demotes**: the proof was
    /// taken before the crash, the head may have moved during it, and a
    /// verification that survived a restart unexamined is exactly the kind of
    /// stale evidence this protocol exists to prevent.
    pub fn on_restart(&self) -> Self {
        match self {
            Self::Building { folded_to } => Self::Building {
                folded_to: *folded_to,
            },
            Self::CaughtUp { at } | Self::Verified { at } => Self::Building { folded_to: *at },
            Self::Active => Self::Active,
            Self::Failed(r) => Self::Failed(r.clone()),
        }
    }
}

/// How many catch-up rounds may pass without the gap shrinking.
pub const MAX_NON_CONVERGING_ROUNDS: usize = 3;

/// Is the catch-up loop actually converging?
///
/// `gaps` is the head-minus-folded distance sampled once per round, oldest
/// first. Convergence is not "always decreasing" — a burst of ingest can widen
/// the gap for a round without meaning the rebuild will never finish. It is
/// "not stuck": the gap must beat its best-so-far at least once every
/// [`MAX_NON_CONVERGING_ROUNDS`].
///
/// Without this, "wait until caught up" is an unbounded loop that looks like a
/// hang, and the operator has no signal distinguishing *slow* from *never*.
pub fn catchup_is_converging(gaps: &[u64]) -> bool {
    if gaps.len() <= MAX_NON_CONVERGING_ROUNDS {
        return true;
    }
    let mut best = u64::MAX;
    let mut stale = 0usize;
    for &g in gaps {
        if g < best {
            best = g;
            stale = 0;
        } else {
            stale += 1;
            if stale > MAX_NON_CONVERGING_ROUNDS {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built_to(n: u64) -> RebuildState {
        RebuildState::Building { folded_to: n }
    }

    // -----------------------------------------------------------------------
    // The six invariants ADR-0041 R2 requires
    // -----------------------------------------------------------------------

    #[test]
    fn partial_projections_are_never_authoritative() {
        // OQ7: what marks a projection left partial by an interrupted fold?
        // Its state does, and nothing but Active may answer a query.
        assert!(!RebuildState::begin().is_authoritative());
        assert!(!built_to(500).is_authoritative());
        assert!(!RebuildState::CaughtUp { at: 500 }.is_authoritative());
        assert!(!RebuildState::Verified { at: 500 }.is_authoritative());
        assert!(!RebuildState::Failed(FailureReason::Abandoned).is_authoritative());
        assert!(RebuildState::Active.is_authoritative());
    }

    #[test]
    fn only_a_verified_projection_may_cut_over() {
        for s in [
            RebuildState::begin(),
            built_to(500),
            RebuildState::CaughtUp { at: 500 },
            RebuildState::Failed(FailureReason::Abandoned),
        ] {
            let r = s.on_cutover(500);
            assert!(r.is_err(), "{s:?} was allowed to cut over");
        }
        assert_eq!(
            RebuildState::Verified { at: 500 }.on_cutover(500),
            Ok(RebuildState::Active)
        );
    }

    #[test]
    fn a_verification_does_not_survive_an_append() {
        // The transition the whole protocol turns on. Proven at 500, one event
        // arrives, and the proof no longer describes the log being served.
        let v = RebuildState::Verified { at: 500 };
        assert_eq!(v.on_append(501).unwrap(), built_to(500));
        // And the stale proof cannot be spent directly either.
        assert!(
            v.on_cutover(501).is_err(),
            "cut over on a verification taken at an older generation"
        );
    }

    #[test]
    fn equivalence_must_be_proven_at_the_generation_it_claims() {
        let c = RebuildState::CaughtUp { at: 500 };
        assert_eq!(
            c.on_equivalence_proven(500).unwrap(),
            RebuildState::Verified { at: 500 }
        );
        assert!(
            c.on_equivalence_proven(499).is_err(),
            "accepted a proof taken at a different generation"
        );
        assert!(c.on_equivalence_proven(501).is_err());
    }

    #[test]
    fn failure_never_touches_the_projection_that_is_working() {
        // Failure is terminal for the ATTEMPT. The protocol has no transition
        // that retires the previous projection except a successful cutover, so
        // "the previous projection remains active" is structural rather than a
        // rule someone has to remember.
        let f = built_to(300)
            .on_failure(FailureReason::FoldRefused("disk full".into()))
            .unwrap();
        assert!(matches!(f, RebuildState::Failed(_)));
        assert!(!f.is_authoritative());
        for attempt in [
            f.on_fold_progress(400),
            f.on_reached_head(400),
            f.on_equivalence_proven(400),
            f.on_cutover(400),
            f.on_append(400),
        ] {
            assert!(attempt.is_err(), "a failed attempt kept operating");
        }
        // An active projection is not an attempt and cannot be "failed" into
        // nothing — retiring it is a separate, deliberate act.
        assert!(RebuildState::Active
            .on_failure(FailureReason::Abandoned)
            .is_err());
    }

    #[test]
    fn restart_resumes_a_fold_but_demotes_a_verification() {
        // Resuming is safe: the fold is deterministic and its position durable.
        assert_eq!(built_to(700).on_restart(), built_to(700));
        // Demoting is mandatory: the head may have moved while the process was
        // down, and a proof that survives a restart unexamined is stale
        // evidence — the same failure class as the tier C marker replay.
        assert_eq!(
            RebuildState::Verified { at: 700 }.on_restart(),
            built_to(700)
        );
        assert_eq!(
            RebuildState::CaughtUp { at: 700 }.on_restart(),
            built_to(700)
        );
        assert_eq!(RebuildState::Active.on_restart(), RebuildState::Active);
    }

    #[test]
    fn cutover_is_the_only_path_to_authoritative() {
        // Exhaustive: no sequence of legal transitions reaches Active except
        // through Verified -> cutover. Proven by construction over the whole
        // transition surface rather than by inspection.
        let states = [
            RebuildState::begin(),
            built_to(10),
            RebuildState::CaughtUp { at: 10 },
            RebuildState::Verified { at: 10 },
            RebuildState::Failed(FailureReason::Abandoned),
        ];
        for s in &states {
            let reached: Vec<RebuildState> = [
                s.on_fold_progress(11),
                s.on_reached_head(11),
                s.on_equivalence_proven(10),
                s.on_append(11),
                s.on_failure(FailureReason::Abandoned),
            ]
            .into_iter()
            .flatten()
            .collect();
            for r in reached {
                assert!(
                    !r.is_authoritative(),
                    "{s:?} reached Active without a cutover"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Fold-position sanity
    // -----------------------------------------------------------------------

    #[test]
    fn a_fold_position_may_not_go_backwards() {
        assert!(built_to(500).on_fold_progress(499).is_err());
        assert_eq!(built_to(500).on_fold_progress(500).unwrap(), built_to(500));
        assert_eq!(built_to(500).on_fold_progress(600).unwrap(), built_to(600));
    }

    #[test]
    fn an_append_behind_the_folded_position_is_refused_not_absorbed() {
        // A head that lands behind work already folded out of the log means the
        // generation source is not monotonic (design §8, S-3). Every staleness
        // comparison in this module rests on that, so it is refused loudly
        // rather than silently treated as "no progress".
        assert!(built_to(500).on_append(499).is_err());
        assert!(RebuildState::CaughtUp { at: 500 }.on_append(499).is_err());
        assert!(RebuildState::Verified { at: 500 }.on_append(499).is_err());
        // Equal is fine: an append AT the folded position is the boundary, not
        // a reversal.
        assert_eq!(built_to(500).on_append(500).unwrap(), built_to(500));
    }

    #[test]
    fn a_fold_cannot_claim_to_have_reached_a_head_behind_itself() {
        assert!(built_to(600).on_reached_head(500).is_err());
        assert_eq!(
            built_to(500).on_reached_head(500).unwrap(),
            RebuildState::CaughtUp { at: 500 }
        );
    }

    #[test]
    fn claiming_catch_up_short_of_the_head_cannot_smuggle_a_partial_fold_through() {
        // The hole this closes: CaughtUp is one transition from Verified and
        // two from Active, so accepting `reached_head(900)` from a projection
        // folded only to 400 would activate an unfolded projection asserting it
        // is current at 900 — with every other invariant still satisfied.
        assert!(built_to(400).on_reached_head(900).is_err());
        // And the whole path is closed, not just the first step.
        let smuggled = built_to(400)
            .on_reached_head(900)
            .and_then(|s| s.on_equivalence_proven(900))
            .and_then(|s| s.on_cutover(900));
        assert!(
            smuggled.is_err(),
            "a projection folded to 400 reached Active claiming generation 900"
        );
    }

    // -----------------------------------------------------------------------
    // Catch-up convergence
    // -----------------------------------------------------------------------

    #[test]
    fn a_shrinking_gap_converges_and_a_stuck_one_does_not() {
        assert!(catchup_is_converging(&[900, 700, 500, 300, 100]));
        // A burst widens the gap briefly; that is not divergence.
        assert!(catchup_is_converging(&[900, 700, 800, 500, 300, 100]));
        // Never beating the best again is.
        assert!(!catchup_is_converging(&[500, 600, 700, 800, 900]));
        assert!(!catchup_is_converging(&[500, 500, 500, 500, 500]));
    }

    #[test]
    fn convergence_is_not_judged_before_there_is_evidence() {
        // Too few rounds to tell — must not fail an attempt that has barely
        // started, which would make the guard itself the reason rebuilds fail.
        for n in 0..=MAX_NON_CONVERGING_ROUNDS {
            let gaps = vec![500u64; n];
            assert!(
                catchup_is_converging(&gaps),
                "declared divergence after {n} round(s)"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The happy path, end to end
    // -----------------------------------------------------------------------

    #[test]
    fn the_full_protocol_runs_and_only_then_serves() {
        let s = RebuildState::begin();
        assert!(!s.is_authoritative());
        let s = s.on_fold_progress(400).unwrap();
        let s = s.on_append(900).unwrap(); // ingest continues
        let s = s.on_fold_progress(900).unwrap();
        let s = s.on_reached_head(900).unwrap();
        assert_eq!(s, RebuildState::CaughtUp { at: 900 });
        let s = s.on_equivalence_proven(900).unwrap();
        assert!(!s.is_authoritative(), "verified is not yet serving");
        let s = s.on_cutover(900).unwrap();
        assert!(s.is_authoritative());
    }

    #[test]
    fn a_late_append_forces_another_lap_rather_than_a_stale_cutover() {
        // The realistic race: verified, then one more event lands before the
        // swap. The protocol must go round again, not activate.
        let s = RebuildState::CaughtUp { at: 900 }
            .on_equivalence_proven(900)
            .unwrap()
            .on_append(901)
            .unwrap();
        assert_eq!(s, built_to(900));
        assert!(s.on_cutover(901).is_err());
        let s = s
            .on_fold_progress(901)
            .unwrap()
            .on_reached_head(901)
            .unwrap()
            .on_equivalence_proven(901)
            .unwrap()
            .on_cutover(901)
            .unwrap();
        assert!(s.is_authoritative());
    }
}
