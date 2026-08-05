//! How the columns the ratified schema ADDED get populated — and why that is
//! a stated assumption rather than a detail.
//!
//! # The honest problem
//!
//! D-2's stand-in and the ratified schema share seventeen columns. The
//! ratified schema adds six: `observation_id`, `writer_class`, `claim_status`,
//! `provenance`, `frame_id`, `map_id`. Four of the six are variable-width
//! TEXT, and two are nullable. So "bytes per event under the real schema" is
//! not a single number — it is a function of how much of that new width real
//! traffic actually carries, and nothing measured so far constrains that.
//!
//! Picking one filling and reporting one number would be the failure mode
//! this project keeps catching: a figure that looks measured while resting on
//! an unstated guess. So the instrument measures a BAND, with both ends named
//! and both reported.
//!
//! # The two ends
//!
//! [`Fill::Lean`] — every added column at its lightest legal value. A raw
//! sensor observation with no derivation history, no frame, no map:
//! `provenance` is the empty array, `frame_id` and `map_id` are NULL. This is
//! a real configuration, not a floor invented to look good — it is exactly
//! what the R2's lidar-derived observations produce today, and SD-4 permits
//! a NULL frame precisely because non-spatial claims have none.
//!
//! [`Fill::Populated`] — every added column carrying realistic content:
//! provenance citing one upstream observation, a frame, a map. This is what
//! a spatial claim derived from perception looks like, and SD-4 makes the
//! frame MANDATORY for those, so it is not a pessimistic invention either.
//!
//! **The horizon must be taken from `Populated`.** A retention horizon is a
//! statement about when a disk fills; taking it from the lean end would give
//! the longest life and the least margin, which is the wrong direction to be
//! wrong in. `Lean` is reported so the width of the band is visible — an
//! obligation is discharged by bounding the uncertainty, not by hiding it.
//!
//! # What is NOT varied
//!
//! `writer_class` and `claim_status` are closed vocabularies with values a
//! handful of bytes wide; the difference between `"sensor"` and
//! `"llm_candidate"` cannot move a per-event figure meaningfully, and pinning
//! them to the sensor path matches the stream the harness generates.
//! `observation_id` is present in both fills because it is NOT NULL — the
//! schema gives no lean option there.

/// Which end of the band to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    /// Added columns at their lightest legal values.
    Lean,
    /// Added columns carrying realistic content.
    Populated,
}

impl Fill {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lean => "lean",
            Self::Populated => "populated",
        }
    }

    /// One-line description carried into the result record, so a reader of the
    /// JSONL does not have to come back to this file to know what was assumed.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Lean => {
                "added columns at lightest legal values: provenance [], frame_id NULL, map_id NULL"
            }
            Self::Populated => {
                "added columns realistic: provenance cites 1 observation, frame_id and map_id set"
            }
        }
    }
}

/// The per-event values for the six added columns.
pub struct Added {
    pub observation_id: String,
    pub provenance: Vec<String>,
    pub frame_id: Option<String>,
    pub map_id: Option<String>,
}

/// Derive the added columns for one event.
///
/// `observation_id` is deliberately NOT equal to `event_id`. The store's own
/// documentation gives the reason — they are distinct so that re-attributing
/// an observation does not rewrite the observation it re-attributes — and
/// making them equal here would understate the column by letting a reader
/// assume it is free.
pub fn added(fill: Fill, generation: u64, seed: u64) -> Added {
    let observation_id = format!(
        "obs-{:022x}",
        generation as u128 * 0x85eb_ca6b + seed as u128
    );
    match fill {
        Fill::Lean => Added {
            observation_id,
            provenance: Vec::new(),
            frame_id: None,
            map_id: None,
        },
        Fill::Populated => Added {
            // One upstream citation. Not a long chain: SD-3 permits many, but
            // a measured band should be bounded by a defensible case, and
            // "this claim rests on the observation before it" is the common
            // one. A deep-provenance derivation is a separate question.
            provenance: vec![format!(
                "obs-{:022x}",
                generation.saturating_sub(1) as u128 * 0x85eb_ca6b + seed as u128
            )],
            frame_id: Some(format!("map:zone-{:03}", generation % 64)),
            map_id: Some(format!("map-{:04}", generation % 16)),
            observation_id,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lean_is_actually_lighter_than_populated() {
        let l = added(Fill::Lean, 500, 1);
        let p = added(Fill::Populated, 500, 1);
        assert!(l.provenance.is_empty());
        assert!(l.frame_id.is_none() && l.map_id.is_none());
        assert_eq!(p.provenance.len(), 1);
        assert!(p.frame_id.is_some() && p.map_id.is_some());
    }

    #[test]
    fn observation_id_is_not_the_event_id() {
        // Guards the modelling choice above: if these ever collapse to the
        // same value the column stops costing anything and the band lies.
        let a = added(Fill::Lean, 7, 20260803);
        let event_id = format!("{:026x}", 7u128 * 0x9e37_79b9 + 20260803u128);
        assert_ne!(a.observation_id, event_id);
    }

    #[test]
    fn observation_id_is_present_in_both_fills() {
        // NOT NULL in the schema — there is no lean option, and a fill that
        // omitted it would not be measuring the ratified table.
        assert!(!added(Fill::Lean, 1, 1).observation_id.is_empty());
        assert!(!added(Fill::Populated, 1, 1).observation_id.is_empty());
    }
}
