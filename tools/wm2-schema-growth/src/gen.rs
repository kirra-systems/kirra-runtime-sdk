//! The event stream, reproduced from `wm2-persistence-harness::gen`.
//!
//! # Why this is a copy and not a dependency
//!
//! The measurement this tool exists to produce is a RATIO: bytes/event under
//! the ratified schema against bytes/event under the stand-in that ADR-0041
//! D-2 used. A ratio only isolates the schema if the event stream is held
//! fixed, so the generator has to be the same generator.
//!
//! It cannot be the same *code*, because the harness must not gain a
//! dependency on `kirra-world-store` and this tool must not gain the harness's
//! stand-in schema — that separation is the harness's entire reason for
//! existing, and collapsing it to avoid a hundred lines of duplication would
//! trade a real guarantee for a cosmetic one.
//!
//! So the shared shape is duplicated and PINNED: [`STREAM_SHAPE_DIGEST`] is
//! recorded in every emitted result, and `stream_shape_matches_the_harness`
//! asserts the harness's source still produces the constants this file was
//! copied from. If someone re-tunes the harness's generator, that test reds
//! rather than the two instruments silently diverging into incomparable
//! numbers.

/// SplitMix64, byte-identical to the harness's `gen::Rng`.
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
pub const RELATIONSHIP_EVERY: u64 = 7;
/// One in this many events belongs to a protected retention class.
pub const PROTECTED_EVERY: u64 = 97;
/// The protected classes, in the harness's rotation order.
pub const PROTECTED_CLASSES: [&str; 5] = [
    "safety",
    "incident",
    "calibration",
    "adjudication",
    "operator",
];
/// The harness's default payload width, and D-2's.
pub const DEFAULT_PAYLOAD_BYTES: usize = 96;

/// A stable name for the stream shape, recorded in every result.
///
/// Not a hash of this file — that would change on a comment edit and make
/// every archived result look incomparable. It names the parameters that
/// actually determine the bytes.
pub const STREAM_SHAPE_DIGEST: &str = "splitmix64/rel-7/protected-97/entity-06/sensor-4/place-4";

/// One generated event, in the fields the two schemas share.
///
/// Deliberately NOT `kirra_world_store::NewEvent`: the fields the ratified
/// schema adds are a modelling choice made in `fill.rs`, and mixing them in
/// here would hide which parts of the record came from the shared stream and
/// which came from an assumption.
pub struct Shared {
    pub generation: u64,
    pub event_id: String,
    pub txn_time_ms: i64,
    pub valid_from_ms: i64,
    pub source: String,
    pub kind: &'static str,
    pub subject: String,
    pub predicate: Option<String>,
    pub object: Option<String>,
    pub payload: String,
    pub retention_class: &'static str,
}

/// Generate `count` events, matching the harness's `events_sized`.
pub fn events_sized(
    start_generation: u64,
    count: u64,
    entities: u64,
    seed: u64,
    payload_bytes: usize,
) -> Vec<Shared> {
    let mut rng = Rng::new(seed ^ start_generation.wrapping_mul(0x2545_f491_4f6c_dd1d));
    let entities = entities.max(1);
    let mut out = Vec::with_capacity(count as usize);

    for i in 0..count {
        let generation = start_generation + i;
        let base_ms = 1_700_000_000_000i64 + (generation as i64) * 50;
        let valid_from_ms = base_ms - (rng.below(200) as i64);
        let subject = format!("entity-{:06}", rng.below(entities));

        let (kind, predicate, object) = if generation.is_multiple_of(RELATIONSHIP_EVERY) {
            (
                "relationship",
                Some(["on", "in", "near", "part-of"][rng.below(4) as usize].to_string()),
                Some(format!("entity-{:06}", rng.below(entities))),
            )
        } else {
            ("observation", None, None)
        };

        out.push(Shared {
            generation,
            event_id: format!("{:026x}", generation as u128 * 0x9e37_79b9 + seed as u128),
            txn_time_ms: base_ms,
            valid_from_ms,
            source: format!("sensor-{}", rng.below(4)),
            kind,
            subject,
            predicate,
            object,
            payload: payload(&mut rng, payload_bytes),
            retention_class: if generation.is_multiple_of(PROTECTED_EVERY) {
                PROTECTED_CLASSES[(generation / PROTECTED_EVERY) as usize % PROTECTED_CLASSES.len()]
            } else {
                "raw"
            },
        });
    }
    out
}

fn payload(rng: &mut Rng, bytes: usize) -> String {
    let place = format!("place-{:04}", rng.below(64));
    let mut s = String::with_capacity(bytes.max(place.len()));
    s.push_str(&place);
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
        let a = events_sized(0, 200, 20, 42, 96);
        let b = events_sized(0, 200, 20, 42, 96);
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.event_id, y.event_id);
            assert_eq!(x.payload, y.payload);
            assert_eq!(x.subject, y.subject);
        }
    }

    #[test]
    fn payload_width_is_honoured() {
        for width in [16usize, 96, 512] {
            let e = events_sized(0, 20, 5, 7, width);
            assert!(e.iter().all(|x| x.payload.len() == width));
        }
    }

    /// The load-bearing test for this file's existence. A ratio against the
    /// harness's numbers is only a SCHEMA ratio while both instruments emit
    /// the same stream; if the harness re-tunes its generator, these numbers
    /// stop being comparable and that must be loud rather than silent.
    #[test]
    fn stream_shape_matches_the_harness() {
        let src = match std::fs::read_to_string("../wm2-persistence-harness/src/gen.rs") {
            Ok(s) => s,
            // Vendored/extracted checkouts without the sibling tool: skip
            // rather than fail. A missing file is not evidence of divergence.
            Err(_) => {
                eprintln!("SKIP: harness gen.rs not present next to this tool");
                return;
            }
        };
        for needle in [
            "const RELATIONSHIP_EVERY: u64 = 7;",
            "const PROTECTED_EVERY: u64 = 97;",
            "pub const DEFAULT_PAYLOAD_BYTES: usize = 96;",
            "0x9e37_79b9_7f4a_7c15",
            "0xbf58_476d_1ce4_e5b9",
            "0x94d0_49bb_1331_11eb",
            "0x2545_f491_4f6c_dd1d",
            r#"format!("entity-{:06}", rng.below(entities))"#,
            r#"format!("sensor-{}", rng.below(4))"#,
            r#"format!("place-{:04}", rng.below(64))"#,
            r#"format!("{:026x}", generation as u128 * 0x9e37_79b9 + seed as u128)"#,
        ] {
            assert!(
                src.contains(needle),
                "harness generator diverged from this copy: {needle:?} no longer present. \
                 Growth ratios against D-2 are not comparable until this is reconciled."
            );
        }
    }
}
