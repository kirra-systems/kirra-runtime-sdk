//! **Citation edges — Tier 4 box 4a, the structural half.**
//!
//! `KIRRA-WM-PROVENANCE-GRAPH-001`:
//!
//! > Materialize the citation relation at append, but do not materialize its
//! > resolved target. That distinction is what preserves historical
//! > correctness.
//!
//! This module owns the *derivation*: given what a source event recorded in its
//! `provenance` array, which edge rows exist. It is deliberately tiny, and
//! deliberately shared — see below.
//!
//! # What an edge is, and the one thing it must not become
//!
//! An edge says **"generation G, at position N, cited observation id X"**. It
//! does not say what X is, whether X exists, or which row X names. Those are
//! read-time questions with a different answer at every coordinate, and box 4b
//! answers them against the events visible at the generation being explained.
//!
//! The pressure to answer them HERE is real and worth naming, because it will
//! present itself as an optimisation: resolving at append would let a reader
//! follow one column instead of joining, and every measurement would say it was
//! faster. It would also silently fix, at write time, an answer whose whole
//! purpose is to be different at different coordinates — a citation dangling
//! when it was written and resolvable a week later must still read as dangling
//! when a query is pinned to the week before. `world_events.observation_id` is
//! not unique and nothing requires a cited id to exist, so there is not even a
//! single target to bake in.
//!
//! # Why the derivation is one shared function rather than two call sites
//!
//! [`citation_edges`] is used by BOTH the append path and the backfill. That is
//! the `adjudication_affects` precedent and it is load-bearing for the same
//! reason: a backfilled store and an append-indexed store then agree **by
//! construction** rather than by two pieces of code happening to match. The
//! rebuild-equivalence test proves the property; this function is what makes it
//! true rather than lucky.
//!
//! # Verbatim, including what looks wrong
//!
//! The array is indexed exactly as recorded — order kept, duplicates kept, and
//! an empty or malformed-looking id kept. No validation, for the same reason as
//! no resolution: the hash-covered `provenance` column is the authoritative
//! statement, and an index that dropped an element a caller could see in the
//! evidence would be disagreeing with the thing it indexes. An id that can never
//! resolve is not a corrupt row, it is a citation that will read as
//! `Dangling` — which is a fact about the source, and one 4b is required to
//! report rather than hide.

/// The largest citation page a caller may request.
///
/// Rule 2 — *queries are bounded* — reaching the one place 4a could have leaked
/// it. Nothing bounds how many observations an event may cite: not the schema,
/// not the caller's argument, not the domain. So a whole-array read is
/// structurally unbounded, and box 3d's finding was that such a read looks
/// perfectly bounded at the API while the store method underneath is not.
///
/// It matches [`crate::lineage::MAX_LINEAGE_PAGE`] deliberately. A citation page
/// and a lineage page are both "evidence a caller is walking", and two different
/// ceilings would be two numbers to keep in step for no stated reason.
pub const MAX_CITATIONS_PAGE: usize = 256;

/// One page of a source's citations, and whether it is all of them.
///
/// `truncated` is carried rather than inferred from `edges.len() == limit`,
/// which is ambiguous exactly at the boundary: an array of precisely `limit`
/// citations is complete, and a caller comparing lengths would report it as cut
/// short. Same reason [`crate::lineage::PageBoundary`] is an enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationPage {
    /// The citations, in recorded order.
    pub edges: Vec<CitationEdge>,
    /// Whether more citations follow this page.
    pub truncated: bool,
}

impl CitationPage {
    /// The ordinal to continue after, or `None` when the page is complete.
    ///
    /// Ordinals are dense from 0, so a continuation is the last ordinal served.
    #[must_use]
    pub fn next_after_ordinal(&self) -> Option<i64> {
        if self.truncated {
            self.edges.last().map(|e| e.ordinal)
        } else {
            None
        }
    }
}

/// One recorded citation, positioned.
///
/// `ordinal` is the element's index in the source's `provenance` array, dense
/// from 0. It is part of the identity of the edge rather than decoration: a
/// source that cites the same observation twice has two edges, and collapsing
/// them would report an array the hash does not cover.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CitationEdge {
    /// Position in the source's provenance array, dense from 0.
    pub ordinal: i64,
    /// The observation id the source claimed to cite, **verbatim**.
    pub cited_observation_id: String,
}

/// **The edges a provenance array yields** — the whole derivation, in one place.
///
/// Total: every input produces a result, and an empty array produces no edges.
///
/// That last case is worth stating because it is indistinguishable, in this
/// table alone, from a source event whose edges were deleted by compaction. The
/// two are told apart by whether the SOURCE EVENT is still retained, which is
/// why compaction removes edges with their event and why 4b checks the source
/// rather than inferring from an empty edge set.
#[must_use]
pub fn citation_edges<S: AsRef<str>>(cited: &[S]) -> Vec<CitationEdge> {
    cited
        .iter()
        .enumerate()
        .map(|(i, id)| CitationEdge {
            // `usize` -> `i64`: an array long enough to overflow this cannot fit
            // in memory, let alone in a JSON column.
            ordinal: i as i64,
            cited_observation_id: id.as_ref().to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(edges: &[CitationEdge]) -> Vec<(i64, &str)> {
        edges
            .iter()
            .map(|e| (e.ordinal, e.cited_observation_id.as_str()))
            .collect()
    }

    #[test]
    fn an_empty_array_yields_no_edges() {
        assert!(citation_edges::<&str>(&[]).is_empty());
    }

    #[test]
    fn ordinals_are_dense_from_zero_and_follow_the_recorded_order() {
        let edges = citation_edges(&["obs-c", "obs-a", "obs-b"]);
        assert_eq!(
            ids(&edges),
            vec![(0, "obs-c"), (1, "obs-a"), (2, "obs-b")],
            "the array's order is the source's statement, not something to sort"
        );
    }

    #[test]
    fn a_repeated_citation_is_two_edges_not_one() {
        // The reason the primary key is (generation, ordinal) rather than
        // (generation, cited_observation_id). Deduplicating here would index a
        // provenance array different from the hash-covered one.
        let edges = citation_edges(&["obs-a", "obs-a"]);
        assert_eq!(ids(&edges), vec![(0, "obs-a"), (1, "obs-a")]);
    }

    #[test]
    fn an_id_that_could_never_resolve_is_still_indexed_verbatim() {
        // No validation, deliberately. An empty id is a citation that will read
        // as Dangling at every coordinate — a fact about the source event, and
        // one 4b must report. Dropping it here would make the index disagree
        // with the evidence it indexes.
        let edges = citation_edges(&["", "   ", "obs-a"]);
        assert_eq!(ids(&edges), vec![(0, ""), (1, "   "), (2, "obs-a")]);
    }

    #[test]
    fn the_derivation_is_the_same_for_borrowed_and_owned_inputs() {
        // The append path holds `&[&str]`; the backfill holds `Vec<String>`
        // decoded from JSON. They must be the same derivation, or
        // rebuild-equivalence would be a coincidence of two code paths.
        let borrowed = citation_edges(&["obs-a", "obs-b"]);
        let owned = citation_edges(&["obs-a".to_string(), "obs-b".to_string()]);
        assert_eq!(borrowed, owned);
    }
}
