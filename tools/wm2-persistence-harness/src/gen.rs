//! Deterministic synthetic load.
//!
//! Seeded and dependency-free (splitmix64), so a measurement is reproducible
//! from its recorded seed. That matters more than it might look: a benchmark
//! whose input cannot be regenerated cannot be re-run when someone disputes the
//! number, and "we cannot reproduce our own evidence" is the failure mode
//! ADR-0041's measurement gate exists to avoid.
//!
//! # What the load is, and what it is not
//!
//! It is a *shape*: entities re-observed over time at a plausible ratio of
//! observations to relationships, with monotonically advancing valid time and a
//! payload of controllable width. It is deliberately not a recording of real
//! perception output, because none exists yet — and generating one from a guess
//! about sensor behaviour would dress an assumption up as a trace.
//!
//! The consequence for citation: the scale parameters are inputs to be reported
//! next to any result, not defaults to be forgotten. ADR-0041's provisional
//! assumptions (hundreds-to-low-thousands of entities, thousands-to-millions of
//! observations) are the point of comparison, and the harness sweeps past them
//! precisely so the "if entities are in the millions, Option B becomes
//! materially stronger" reopening condition is decided by measurement.

use crate::standin::{Event, EventKind, RetentionClass};

/// splitmix64 — small, well-distributed, and reproducible across platforms
/// because every operation is defined on wrapping u64.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
}

/// One in this many events is a relationship rather than an observation.
///
/// Relationships change far more slowly than positions do — a robot re-observes
/// where things are constantly and re-learns that a box is *on* a shelf rarely.
const RELATIONSHIP_EVERY: u64 = 7;

/// One in this many events belongs to a protected retention class.
///
/// Protected traffic is rare and must survive compaction forever, so the
/// generator has to produce some: a load made entirely of raw observations
/// would let the compaction experiment pass without ever exercising the refusal
/// that makes ADR-0041's retention policy real.
const PROTECTED_EVERY: u64 = 97;

const PROTECTED_CLASSES: [RetentionClass; 5] = [
    RetentionClass::Safety,
    RetentionClass::Incident,
    RetentionClass::Calibration,
    RetentionClass::Adjudication,
    RetentionClass::Operator,
];

pub const DEFAULT_PAYLOAD_BYTES: usize = 96;

/// Generate `count` events starting at `start_generation`.
pub fn events(start_generation: u64, count: u64, entities: u64, seed: u64) -> Vec<Event> {
    events_sized(
        start_generation,
        count,
        entities,
        seed,
        DEFAULT_PAYLOAD_BYTES,
    )
}

/// As [`events`], with an explicit payload width.
///
/// Width is a parameter because it is the dominant term in storage growth and
/// the one the real schema will change most: the four trust axes, a provenance
/// handle and a per-source payload all land here. Sweeping it is how the growth
/// projection stays useful after the payload is actually designed.
pub fn events_sized(
    start_generation: u64,
    count: u64,
    entities: u64,
    seed: u64,
    payload_bytes: usize,
) -> Vec<Event> {
    let mut rng = Rng::new(seed ^ start_generation.wrapping_mul(0x2545_f491_4f6c_dd1d));
    let entities = entities.max(1);
    let mut out = Vec::with_capacity(count as usize);

    for i in 0..count {
        let generation = start_generation + i;
        // Valid time advances with the log but jitters, so events do not arrive
        // in perfect valid-time order. A fold that only works on sorted input
        // is not the fold the real system will need.
        let base_ms = 1_700_000_000_000i64 + (generation as i64) * 50;
        let valid_from_ms = base_ms - (rng.below(200) as i64);
        let subject = format!("entity-{:06}", rng.below(entities));

        let (kind, predicate, object) = if generation.is_multiple_of(RELATIONSHIP_EVERY) {
            (
                EventKind::Relationship,
                Some(["on", "in", "near", "part-of"][rng.below(4) as usize].to_string()),
                Some(format!("entity-{:06}", rng.below(entities))),
            )
        } else {
            (EventKind::Observation, None, None)
        };

        out.push(Event {
            generation,
            event_id: format!("{:026x}", generation as u128 * 0x9e37_79b9 + seed as u128),
            txn_time_ms: base_ms,
            valid_from_ms,
            valid_to_ms: None,
            source: format!("sensor-{}", rng.below(4)),
            source_version: "0.1.0".to_string(),
            kind,
            subject,
            predicate,
            object,
            payload: payload(&mut rng, payload_bytes),
            retention: if generation.is_multiple_of(PROTECTED_EVERY) {
                PROTECTED_CLASSES[(generation / PROTECTED_EVERY) as usize % PROTECTED_CLASSES.len()]
            } else {
                RetentionClass::Raw
            },
        });
    }
    out
}

/// A payload of approximately `bytes` width, carrying a place reference the
/// projection actually reads plus filler standing in for the fields the real
/// schema has not fixed yet.
fn payload(rng: &mut Rng, bytes: usize) -> String {
    let place = format!("place-{:04}", rng.below(64));
    let mut s = String::with_capacity(bytes.max(place.len()));
    s.push_str(&place);
    // Deterministic, incompressible-ish filler: constant bytes would let
    // SQLite's page layout behave unrepresentatively well and understate growth.
    while s.len() < bytes {
        let v = rng.next_u64();
        s.push(char::from(b'a' + (v % 26) as u8));
    }
    s.truncate(bytes.max(place.len()));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_produces_the_same_events() {
        let a = events(0, 200, 20, 42);
        let b = events(0, 200, 20, 42);
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.canonical_bytes(), y.canonical_bytes());
        }
    }

    #[test]
    fn a_different_seed_produces_different_events() {
        let a = events(0, 100, 20, 1);
        let b = events(0, 100, 20, 2);
        assert_ne!(a[10].canonical_bytes(), b[10].canonical_bytes());
    }

    #[test]
    fn generations_are_contiguous_from_the_start_point() {
        let e = events(500, 50, 10, 3);
        assert_eq!(e.first().unwrap().generation, 500);
        assert_eq!(e.last().unwrap().generation, 549);
        for (i, ev) in e.iter().enumerate() {
            assert_eq!(ev.generation, 500 + i as u64);
        }
    }

    #[test]
    fn appending_a_second_run_continues_the_log_without_collision() {
        // The growth and replay benchmarks generate in chunks; overlapping
        // event_ids would violate the UNIQUE constraint and the failure would
        // look like a storage bug rather than a generator one.
        let a = events(0, 100, 10, 9);
        let b = events(100, 100, 10, 9);
        let ids: std::collections::HashSet<_> =
            a.iter().chain(&b).map(|e| e.event_id.clone()).collect();
        assert_eq!(ids.len(), 200);
    }

    #[test]
    fn valid_time_is_not_perfectly_ordered() {
        // Deliberate: a fold that silently relies on sorted arrival passes on
        // ordered input and fails in the field.
        let e = events(0, 500, 30, 4);
        assert!(
            e.windows(2)
                .any(|w| w[1].valid_from_ms < w[0].valid_from_ms),
            "generator produced monotonic valid time — the fold would not be exercised"
        );
    }

    #[test]
    fn relationships_are_a_minority_but_present() {
        let e = events(0, 700, 30, 6);
        let rels = e
            .iter()
            .filter(|x| x.kind == EventKind::Relationship)
            .count();
        assert!(
            rels > 0,
            "no relationship events — the graph query family is untested"
        );
        assert!(
            rels * 3 < e.len(),
            "relationships should not dominate: {rels}/{}",
            e.len()
        );
    }

    #[test]
    fn entity_count_is_respected_and_reused() {
        let e = events(0, 1000, 8, 5);
        let distinct: std::collections::HashSet<_> = e.iter().map(|x| &x.subject).collect();
        assert!(
            distinct.len() <= 8,
            "generated more entities than requested"
        );
        assert!(distinct.len() > 1, "entities are not being re-observed");
    }

    #[test]
    fn zero_entities_does_not_divide_by_zero() {
        assert_eq!(events(0, 5, 0, 1).len(), 5);
    }

    #[test]
    fn payload_width_is_honoured() {
        for width in [16usize, 96, 512] {
            let e = events_sized(0, 20, 5, 7, width);
            assert!(
                e.iter().all(|x| x.payload.len() == width),
                "width {width} not produced"
            );
        }
    }

    #[test]
    fn a_tiny_payload_width_still_carries_the_place_reference() {
        // The projection reads the place out of the payload; truncating below
        // it would make the set-query family measure nothing.
        let e = events_sized(0, 5, 3, 1, 4);
        assert!(e[0].payload.starts_with("place-"));
    }
}
