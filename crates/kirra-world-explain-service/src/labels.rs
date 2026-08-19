//! **Store-backed [`ClaimLabels`] — what an explanation is allowed to SAY.**
//!
//! Tier 4 box 3a. This is the one place in the explanation path where World
//! facts become words, so it is the place where the honesty of an explanation is
//! decided. Everything downstream — the projection, the wire, Mick's renderer —
//! can only preserve or lose what this produces.
//!
//! # The rule these labels are written to
//!
//! > A label describes only facts present in the cited World evidence, and adds
//! > no interpretation.
//!
//! So they are deterministic, evidence-derived and **intentionally boring**. If
//! a phrase would need inference, preference or a stylistic choice to produce,
//! it does not belong here — it belongs to the renderer, which is the layer
//! entitled to make prose out of facts. A label that reads awkwardly is working
//! as designed; a label that reads well because it guessed is a defect.
//!
//! Concretely, these do NOT: rank or summarise claims, resolve which of several
//! claims "matters", describe a claim's significance, soften or emphasise
//! anything, or say ANYTHING about a generation that is not retained.
//!
//! # Why `None` is the load-bearing case
//!
//! [`project_explanation`] defines a `None` label as *"the event is gone"* and
//! renders `DELETED_CLAIM_LABEL` or `UNINDEXED_CLAIM_LABEL` accordingly. So
//! `None` is not "I could not find it" — it is a CLAIM, made to an operator,
//! that evidence was deleted.
//!
//! That is why every method here routes through
//! [`WorldStore::claim_at_generation`], which is a point read on the event log
//! with no subject scope and no fallback: `Some` when an event is retained at
//! exactly that coordinate, `None` only when it genuinely is not. A
//! subject-scoped substitute would have returned `None` for every cross-subject
//! provenance node, and the artifact would have said "deleted" about evidence
//! sitting in the log, with every gate green.
//!
//! [`project_explanation`]: kirra_world_service::explain::project_explanation
//! [`WorldStore::claim_at_generation`]: kirra_world_store::WorldStore::claim_at_generation

use kirra_explain_types::{DisplayLabel, EvidenceDigest};
use kirra_world_service::explain::ClaimLabels;
use kirra_world_store::{projection::ProjectedClaim, StoreError, WorldStore};

/// Labels rendered from the event log of one store.
pub struct StoreLabels<'a> {
    store: &'a WorldStore,
}

impl<'a> StoreLabels<'a> {
    /// Read labels out of `store`.
    #[must_use]
    pub fn new(store: &'a WorldStore) -> Self {
        Self { store }
    }
}

/// Render one claim as a flat statement of what the event recorded.
///
/// Subject, predicate and object, in that order, because that is the order they
/// are stored in and any other order would be a presentation decision. A claim
/// with no predicate or no object says so rather than having the gap papered
/// over: `"package_17 (no predicate recorded)"` is uglier than a fabricated verb
/// and is the point.
fn describe_claim(c: &ProjectedClaim) -> String {
    match (c.predicate.as_deref(), c.object.as_deref()) {
        (Some(p), Some(o)) => format!("{} {} {}", c.subject, p, o),
        (Some(p), None) => format!("{} {} (no object recorded)", c.subject, p),
        (None, Some(o)) => format!("{} (no predicate recorded) {}", c.subject, o),
        (None, None) => format!("{} (no predicate or object recorded)", c.subject),
    }
}

/// Render one claim as the EVIDENCE it is, naming its source and event.
///
/// The source and event id are carried because an auditor's next question is
/// *"recorded by what, and which event?"*, and both are stored facts. Nothing is
/// said about whether the source is reliable — that is an interpretation, and
/// the trust axes exist to carry it separately.
fn describe_evidence(c: &ProjectedClaim) -> String {
    format!(
        "{} — recorded by {} as event {}",
        describe_claim(c),
        c.source,
        c.event_id
    )
}

/// Render `txn_time_ms` as a UTC instant.
///
/// Deliberately mechanical, and deliberately without a date library. The trait
/// asks for *"a rendered instant, not the generation"*, and this is the smallest
/// thing that satisfies it: a fixed ISO-8601-shaped UTC string, computed by
/// plain arithmetic, identical on every machine and in every locale.
///
/// A friendlier rendering — *"just after nine on Tuesday"* — would be a
/// STYLISTIC choice made on the World side about how an operator should hear the
/// time. That belongs to the renderer.
fn utc_stamp(ms: i64) -> String {
    // Days since the Unix epoch, floored so pre-epoch instants render correctly
    // rather than folding onto the wrong day.
    let (days, rem_ms) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (
        rem_ms / 3_600_000,
        (rem_ms % 3_600_000) / 60_000,
        (rem_ms % 60_000) / 1000,
    );
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02} UTC")
}

/// Howard Hinnant's `civil_from_days`, which is exact for the proleptic
/// Gregorian calendar and needs no table and no dependency.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl ClaimLabels for StoreLabels<'_> {
    type Error = StoreError;

    fn claim_label(&self, generation: i64) -> Result<Option<DisplayLabel>, Self::Error> {
        Ok(self
            .store
            .claim_at_generation(generation)?
            .map(|c| DisplayLabel::new(describe_claim(&c))))
    }

    fn evidence(
        &self,
        generation: i64,
    ) -> Result<Option<(DisplayLabel, EvidenceDigest)>, Self::Error> {
        Ok(self.store.claim_at_generation(generation)?.map(|c| {
            (
                DisplayLabel::new(describe_evidence(&c)),
                // The chain digest IS the citable handle: it is what lets an
                // auditor verify the claim against the tamper-evident log
                // rather than trust that the API returned it.
                EvidenceDigest::new(c.chain_digest.clone()),
            )
        }))
    }

    fn pin_label(&self, generation: i64) -> Result<DisplayLabel, Self::Error> {
        // A pin label has no `Option`: the trait requires text for whatever
        // coordinate the artifact was pinned to, including one whose event is
        // gone. Saying so is the honest rendering — inventing a time for a
        // deleted event would put a fact in the artifact that the store does
        // not hold.
        Ok(match self.store.claim_at_generation(generation)? {
            Some(c) => DisplayLabel::new(utc_stamp(c.txn_time_ms)),
            None => DisplayLabel::new("a time no longer recorded"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_utc_stamp_is_fixed_and_locale_free() {
        assert_eq!(utc_stamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_stamp(1_700_000_000_000), "2023-11-14 22:13:20 UTC");
        // A leap day, because the calendar arithmetic is hand-rolled and this
        // is where a hand-rolled calendar goes wrong.
        assert_eq!(utc_stamp(1_709_164_800_000), "2024-02-29 00:00:00 UTC");
        // Pre-epoch: floored division, not truncated, or this lands a day out.
        assert_eq!(utc_stamp(-1), "1969-12-31 23:59:59 UTC");
    }

    fn claim(subject: &str, predicate: Option<&str>, object: Option<&str>) -> ProjectedClaim {
        ProjectedClaim {
            subject: subject.into(),
            predicate: predicate.map(Into::into),
            object: object.map(Into::into),
            kind: "mission".into(),
            payload: "{}".into(),
            frame_id: None,
            map_id: None,
            source: "warehouse-scanner".into(),
            valid_from_ms: 0,
            valid_to_ms: None,
            txn_time_ms: 0,
            generation: 1,
            event_id: "ev-a".into(),
            chain_digest: "9f86d081".into(),
            trust: None,
        }
    }

    #[test]
    fn a_missing_predicate_or_object_is_stated_not_papered_over() {
        assert_eq!(
            describe_claim(&claim("package_17", Some("last_seen_at"), Some("dock_a"))),
            "package_17 last_seen_at dock_a"
        );
        // The gap is NAMED. A renderer that received "package_17 dock_a" could
        // not tell a missing predicate from a two-word one.
        assert!(describe_claim(&claim("package_17", None, Some("dock_a")))
            .contains("no predicate recorded"));
        assert!(
            describe_claim(&claim("package_17", Some("last_seen_at"), None))
                .contains("no object recorded")
        );
        assert!(describe_claim(&claim("package_17", None, None)).contains("no predicate or object"));
    }

    #[test]
    fn evidence_names_its_source_and_event_and_nothing_else() {
        let e = describe_evidence(&claim("package_17", Some("last_seen_at"), Some("dock_a")));
        assert!(
            e.contains("warehouse-scanner"),
            "the source is a stored fact"
        );
        assert!(e.contains("ev-a"), "the event id is a stored fact");
        // No adjectives about reliability: that is what the trust axes are for,
        // and inventing one here would be the interpretation this layer bans.
        for editorial in ["reliable", "trusted", "likely", "probably", "confirmed by"] {
            assert!(
                !e.to_lowercase().contains(editorial),
                "evidence label editorialised with {editorial:?}: {e}"
            );
        }
    }
}
