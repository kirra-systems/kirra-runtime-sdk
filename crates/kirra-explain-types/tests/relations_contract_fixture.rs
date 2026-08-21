//! **The canonical relationship-view fixture — the cross-language contract.**
//!
//! `contracts/world_relations_v1.json` is the ONE artifact both sides agree on.
//! This test serializes it from the Rust types;
//! `console/lib/world/relations.test.mjs` decodes that same file with the
//! hand-written TypeScript contract.
//!
//! # Why a fixture and not Rust→TypeScript generation
//!
//! Generation is the attractive answer and was deliberately not taken: it adds
//! a codegen subsystem before anyone knows whether Kirra World will have one JS
//! consumer or twenty. A checked-in fixture plus a conformance test on each
//! side buys the property that matters — **a Rust change that adds, removes,
//! renames or reshapes a field reds a test** — at a fraction of the machinery,
//! and it can be replaced by generation later without either side having built
//! against the generator.
//!
//! # What makes it load-bearing rather than decorative
//!
//! It carries EVERY outcome variant, EVERY field, and ALL FOUR provenance
//! states. A fixture covering only the happy row would let a renamed field on
//! a rare variant reach the console silently — exactly the drift a hand-written
//! contract is accused of, and the reason this file exists to answer the
//! accusation.
//!
//! Covering the refusal variants is not thoroughness for its own sake.
//! `not_an_entity` and `unavailable` are the two answers the console must never
//! render as *related to nothing*, so they are the two whose wire shape it can
//! least afford to get wrong — and the two a fixture built from the happy path
//! would never have pinned.
//!
//! Neither tree owns it: it sits in `contracts/` at the repository root, so the
//! Rust side is not reaching into `console/` and the console is not reaching
//! into `crates/`.

use kirra_explain_types::{
    ProvenanceStanding, RelatedPair, RelationsOutcome, RelationsView, RELATIONS_VIEW_VERSION,
};

fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/world_relations_v1.json")
}

/// **One invocation feeds both the fixture rows and the exhaustiveness check.**
///
/// The two halves have to come from one place. A hand-written list of four
/// rows next to a hand-written match of four arms is two lists that drift, and
/// the drift is silent in the direction that matters: a fifth variant added to
/// `ProvenanceStanding` and left out of the fixture would reach the console at
/// runtime, where the decoder throws, instead of at build time, where somebody
/// can decide how it is presented.
///
/// Expanding this macro produces:
///
/// - `EVERY_STATE` — the fixture's rows, one per state;
/// - `wire_name` — a match over `ProvenanceStanding` that is exhaustive
///   BECAUSE the macro wrote it from the same invocation. A variant missing
///   from the invocation below makes that match non-exhaustive and this file
///   stops compiling.
///
/// So the chain closes: new variant -> compile error here -> author adds a row
/// -> the fixture on disk moves -> `console/lib/world/relations.test.mjs`
/// reds. There is no path that adds a state and leaves both trees green.
macro_rules! provenance_fixture {
    ($( $variant:ident => ($other:literal, $adjudicator:literal, $marker:literal) ),+ $(,)?) => {
        const EVERY_STATE: &[(ProvenanceStanding, &str, &str, &str)] = &[
            $( (ProvenanceStanding::$variant, $other, $adjudicator, $marker) ),+
        ];

        fn wire_name(state: ProvenanceStanding) -> &'static str {
            match state {
                $( ProvenanceStanding::$variant => stringify!($variant) ),+
            }
        }
    };
}

provenance_fixture! {
    Resolved => ("track-b", "yard-supervisor", "d-41"),
    Degraded => ("track-c", "night-shift-lead", "d-57"),
    Dangling => ("track-d", "yard-supervisor", "d-63"),
    Plural   => ("track-e", "depot-manager", "d-70"),
}

/// One pair per provenance state, so the fixture exercises every one of them.
fn canonical_view() -> RelationsView {
    RelationsView {
        subject: "track-a".to_string(),
        related: EVERY_STATE
            .iter()
            .map(|&(provenance, other, adjudicator, marker)| RelatedPair {
                low: "track-a".to_string(),
                high: other.to_string(),
                other: other.to_string(),
                adjudicator: adjudicator.to_string(),
                decision_marker: marker.to_string(),
                provenance,
            })
            .collect(),
        truncated: true,
    }
}

/// **The same one-invocation trick, applied to the outcome variants.**
///
/// `RelationsOutcome` has three arms and the console renders all three. The
/// two it renders most carefully are the refusals — `not_an_entity` (the
/// subject is not askable) and `unavailable` (nobody could answer) — because
/// both must stay distinguishable from `related` with an empty list. A fixture
/// that pinned only `related` would leave exactly those two free to drift.
///
/// Expanding this produces the fixture's document list AND an exhaustive match
/// over the enum, so a fourth variant stops this file compiling.
macro_rules! outcome_fixture {
    ($( $variant:ident { $( $field:ident : $value:expr ),* $(,)? } => $tag:literal ),+ $(,)?) => {
        fn canonical_fixture() -> Vec<RelationsOutcome> {
            vec![ $( RelationsOutcome::$variant { $( $field: $value ),* } ),+ ]
        }

        fn outcome_tag(outcome: &RelationsOutcome) -> &'static str {
            match outcome {
                $( RelationsOutcome::$variant { .. } => $tag ),+
            }
        }
    };
}

outcome_fixture! {
    Related { view: canonical_view() } => "related",
    NotAnEntity {
        reason: "\"track-\u{fffd}\" is not an askable entity identity".to_string(),
    } => "not_an_entity",
    Unavailable {
        reason: "the relationship projection could not be read".to_string(),
    } => "unavailable",
}

/// **The fixture on disk is what these types serialize to.**
///
/// Pretty-printed so a CI failure diff is readable rather than one enormous
/// line. On mismatch the message prints the expected document — the fix is to
/// update the fixture AND to look at what moved, because the console's
/// conformance test is about to fail too and that is the signal.
#[test]
fn the_checked_in_fixture_matches_what_rust_serializes() {
    let expected = serde_json::to_string_pretty(&canonical_fixture()).expect("serialize");
    let path = fixture_path();
    let found = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

    assert_eq!(
        found.trim(),
        expected.trim(),
        "\n\nthe cross-language fixture is out of date.\n\
         The Rust contract changed, so `console/lib/world/relations.ts` must change with it.\n\
         Write this into {}:\n\n{expected}\n",
        path.display()
    );
}

/// **Every provenance state appears in the fixture, and the two halves agree.**
///
/// `wire_name` is the exhaustive match the macro wrote; `EVERY_STATE` is the
/// row list it wrote from the same invocation. Comparing them here gives the
/// compile-time control a runtime job as well: if the macro ever grew a second
/// source of truth, the two would disagree and this would say so.
///
/// The serialized name is checked against the match arm's identifier, lowered,
/// because the wire form is what the console actually reads. A serde rename on
/// one variant would show up as a mismatch here rather than as a missing
/// presentation in the browser.
#[test]
fn the_fixture_exercises_every_provenance_state() {
    let view = canonical_view();
    assert_eq!(
        view.related.len(),
        EVERY_STATE.len(),
        "one fixture row per provenance state"
    );

    let mut seen: Vec<ProvenanceStanding> = view.related.iter().map(|r| r.provenance).collect();
    seen.sort_by_key(|s| format!("{s:?}"));
    seen.dedup();
    assert_eq!(
        seen.len(),
        EVERY_STATE.len(),
        "the fixture must carry each state exactly once, got {seen:?}"
    );

    for &(state, ..) in EVERY_STATE {
        let serialized = serde_json::to_string(&state).expect("serialize a state");
        assert_eq!(
            serialized.trim_matches('"'),
            wire_name(state).to_ascii_lowercase(),
            "the wire name for {state:?} is not what the match arm names it — \
             a serde rename must be carried into the console contract"
        );
    }

    assert!(
        view.truncated,
        "truncated must be exercised as true — a fixture where it is always \
         false would not catch the field being dropped"
    );
}

/// **Every outcome variant appears in the fixture, exactly once.**
///
/// `outcome_tag` is the exhaustive match the macro wrote from the same
/// invocation as the document list, so this cannot be satisfied by a fixture
/// that has drifted from the enum: a fourth variant fails to compile above,
/// and a variant dropped from the invocation drops its document here.
///
/// The tag is compared against what serde actually emits, because the tag is
/// what the console switches on. A `#[serde(rename)]` on one arm shows up as a
/// mismatch here rather than as an unhandled outcome in the browser.
#[test]
fn the_fixture_carries_every_outcome_variant() {
    let fixture = canonical_fixture();
    let mut tags: Vec<&str> = fixture.iter().map(outcome_tag).collect();
    let distinct = {
        let mut t = tags.clone();
        t.sort_unstable();
        t.dedup();
        t
    };
    assert_eq!(
        tags.len(),
        distinct.len(),
        "each outcome variant appears once, got {tags:?}"
    );
    tags.sort_unstable();
    assert_eq!(
        tags,
        ["not_an_entity", "related", "unavailable"],
        "the console switches on these tags; a change here is a console change"
    );

    for outcome in &fixture {
        let value = serde_json::to_value(outcome).expect("serialize an outcome");
        assert_eq!(
            value.get("outcome").and_then(|v| v.as_str()),
            Some(outcome_tag(outcome)),
            "the serialized tag is not what the match arm names it"
        );
    }

    // The refusals must carry their reason, or the console has nothing to show
    // and would fall back to a generic message — which reads as an empty
    // answer, the one thing these two variants exist to prevent.
    for outcome in &fixture {
        if outcome_tag(outcome) == "related" {
            continue;
        }
        let value = serde_json::to_value(outcome).expect("serialize an outcome");
        let reason = value.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !reason.trim().is_empty(),
            "{} must carry a non-empty reason",
            outcome_tag(outcome)
        );
    }
}

/// The contract version is pinned, so a bump is a deliberate act.
#[test]
fn the_contract_version_is_one() {
    assert_eq!(RELATIONS_VIEW_VERSION, 1);
}
