// crates/kirra-ros2-adapter/tests/parko_cited_copy_correspondence.rs
//
// Cross-workspace cited-copy DRIFT GUARD (ADR-0029 follow-up).
//
// The SDK courier angular bound (`config::CourierAngularBound::courier_reference`)
// is a CITED COPY of parko's diff-drive model of record
// (`parko/crates/parko-kirra` `PlatformParams::urban_service_robot_reference()`
// + `STOP_EPSILON_RAD_S` + `ROLLOVER_MIN_LINEAR_VELOCITY_MPS` + `GRAVITY_MPS2`).
// The two are DEPENDENCY-SEPARATED diverse-governor workspaces — the SDK cannot
// import parko and vice-versa — so per the `docs/CONTRACT_PROFILES.md`
// single-source rule the numbers travel as a copy, NOT a shared symbol (the
// diversity is deliberate).
//
// The in-crate test `courier_angular_bound_matches_parko_record` pins the SDK
// copy against HARD-CODED literals — but a literal can't notice when PARKO's
// record changes underneath it: edit `urban_service_robot_reference()` and the
// SDK copy silently goes stale while every SDK test still passes. THIS test
// closes that hole: it reads parko's SOURCE from disk at test time and asserts
// the SDK copy still equals it, field by field. Change either side and this
// gate fails until they are reconciled.
//
// FAIL-CLOSED: if parko's source can't be read or parsed, the test PANICS
// (cannot verify correspondence ⇒ the gate fails, forcing a human look) — it
// never passes by default. parko lives in the same repo, so the files are
// always present in a normal checkout.
//
// NOTE: this is a `cargo test` (runs in the existing `Test` CI job) rather than
// a new CI yaml step, mirroring the repo's traceability-gate pattern.

use std::path::PathBuf;

use kirra_ros2_adapter::config::{CourierAngularBound, ROLLOVER_MIN_LINEAR_VELOCITY_MPS};

/// Repo root, derived from this crate's manifest dir
/// (`<root>/crates/kirra-ros2-adapter`).
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // <root>/crates
        .and_then(|p| p.parent()) // <root>
        .expect("manifest dir must have two ancestors (crates/<crate>)")
        .to_path_buf()
}

fn read_parko(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "DRIFT GUARD fail-closed: cannot read parko model-of-record source {}: {e}. \
             The SDK courier bound is a cited copy of parko; correspondence cannot be \
             verified without it.",
            path.display()
        )
    })
}

/// Extract the body of a named `fn` up to the next `pub fn` / `fn ` boundary, so
/// field lookups don't bleed into a sibling constructor with the same field names
/// (`conservative_default` vs `urban_service_robot_reference`).
fn fn_body<'a>(src: &'a str, fn_name: &str) -> &'a str {
    let needle = format!("fn {fn_name}");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("DRIFT GUARD: fn {fn_name} not found in parko source"));
    let rest = &src[start + needle.len()..];
    // End at the next function declaration after this one.
    let end = rest
        .find("fn ")
        .map(|i| start + needle.len() + i)
        .unwrap_or(src.len());
    &src[start..end]
}

/// Parse a `name: <f64>,` struct-init field out of a function body. Tolerates a
/// trailing `// comment` because the value terminates at the comma.
fn field(body: &str, name: &str) -> f64 {
    let key = format!("{name}:");
    let idx = body
        .find(&key)
        .unwrap_or_else(|| panic!("DRIFT GUARD: field `{name}` not found in parko source"));
    let after = &body[idx + key.len()..];
    let end = after
        .find(',')
        .unwrap_or_else(|| panic!("DRIFT GUARD: field `{name}` value must end with a comma"));
    after[..end].trim().parse::<f64>().unwrap_or_else(|_| {
        panic!(
            "DRIFT GUARD: could not parse parko field `{name}` value `{}`",
            after[..end].trim()
        )
    })
}

/// Parse a `NAME: f64 = <f64>;` const out of a source file. The `: f64 =`
/// qualifier matches the DECLARATION, not the symbol's later uses in formulas.
fn konst(src: &str, name: &str) -> f64 {
    let key = format!("{name}: f64 =");
    let idx = src.find(&key).unwrap_or_else(|| {
        panic!("DRIFT GUARD: const `{name}` declaration not found in parko source")
    });
    let after = &src[idx + key.len()..];
    let end = after
        .find(';')
        .unwrap_or_else(|| panic!("DRIFT GUARD: const `{name}` must end with `;`"));
    after[..end].trim().parse::<f64>().unwrap_or_else(|_| {
        panic!(
            "DRIFT GUARD: could not parse parko const `{name}` value `{}`",
            after[..end].trim()
        )
    })
}

/// The SDK courier angular bound must still equal parko's diff-drive model of
/// record, read from parko's SOURCE (not a hard-coded mirror). If parko's
/// `urban_service_robot_reference()` / `STOP_EPSILON_RAD_S` /
/// `ROLLOVER_MIN_LINEAR_VELOCITY_MPS` / `GRAVITY_MPS2` change, reconcile the SDK
/// `CourierAngularBound::courier_reference()` (and the SDK consts) — or revert
/// parko — until this gate passes.
//
// SAFETY: SG3 SG8 | REQ: courier-cited-copy-drift-guard | TEST: sdk_courier_bound_matches_parko_source,drift_guard_parser_extracts_the_right_constructor
#[test]
fn sdk_courier_bound_matches_parko_source() {
    let angular_src = read_parko("parko/crates/parko-kirra/src/angular_bound.rs");
    let lib_src = read_parko("parko/crates/parko-kirra/src/lib.rs");

    let body = fn_body(&angular_src, "urban_service_robot_reference");
    // Sanity: we sliced the RIGHT constructor (guards an empty/mis-scoped slice
    // that would make every `field()` panic — but be explicit).
    assert!(
        body.contains("track_width_m"),
        "DRIFT GUARD: parsed parko constructor body looks wrong (no track_width_m)"
    );

    let sdk = CourierAngularBound::courier_reference();

    // PlatformParams fields (parko: urban_service_robot_reference).
    assert_eq!(
        sdk.track_width_m,
        field(body, "track_width_m"),
        "track_width_m drifted from parko's urban_service_robot_reference()"
    );
    assert_eq!(
        sdk.cog_height_m,
        field(body, "cog_height_m"),
        "cog_height_m drifted from parko"
    );
    assert_eq!(
        sdk.robot_extent_m,
        field(body, "robot_extent_m"),
        "robot_extent_m drifted from parko"
    );
    assert_eq!(
        sdk.v_edge_safe_mps,
        field(body, "v_edge_safe_mps"),
        "v_edge_safe_mps drifted from parko"
    );
    assert_eq!(
        sdk.theta_max_rad,
        field(body, "theta_max_rad"),
        "theta_max_rad drifted from parko"
    );
    assert_eq!(
        sdk.ftti_s,
        field(body, "ftti_s"),
        "ftti_s drifted from parko"
    );
    assert_eq!(
        sdk.mrc_posture_factor,
        field(body, "mrc_posture_factor"),
        "mrc_posture_factor drifted from parko"
    );
    assert_eq!(
        sdk.k_roll,
        field(body, "k_roll"),
        "k_roll (rollover safety factor) drifted from parko's urban_service_robot_reference()"
    );

    // Consts cited from parko.
    assert_eq!(
        sdk.stop_epsilon_rad_s,
        konst(&lib_src, "STOP_EPSILON_RAD_S"),
        "stop_epsilon_rad_s drifted from parko's STOP_EPSILON_RAD_S"
    );
    assert_eq!(
        ROLLOVER_MIN_LINEAR_VELOCITY_MPS,
        konst(&angular_src, "ROLLOVER_MIN_LINEAR_VELOCITY_MPS"),
        "ROLLOVER_MIN_LINEAR_VELOCITY_MPS drifted from parko"
    );
    assert_eq!(
        9.81_f64,
        konst(&angular_src, "GRAVITY_MPS2"),
        "the SDK rollover gravity constant drifted from parko's GRAVITY_MPS2"
    );
}

/// Meta-test: prove the parser actually discriminates between the two
/// same-shaped constructors in `angular_bound.rs`. If `fn_body` mis-scoped and
/// returned the whole file (or the wrong fn), `conservative_default`'s 0.20
/// track width would leak in and this would catch it. This keeps the drift
/// guard from passing vacuously on a broken parser.
#[test]
fn drift_guard_parser_extracts_the_right_constructor() {
    let angular_src = read_parko("parko/crates/parko-kirra/src/angular_bound.rs");
    let reference = fn_body(&angular_src, "urban_service_robot_reference");
    let conservative = fn_body(&angular_src, "conservative_default");

    // The reference platform's track is 0.50; the conservative default's is 0.20.
    // If the parser returned the wrong/whole slice these would not differ as expected.
    assert_eq!(
        field(reference, "track_width_m"),
        0.50,
        "reference constructor must yield track 0.50"
    );
    assert_eq!(
        field(conservative, "track_width_m"),
        0.20,
        "conservative_default must yield track 0.20 — proves fn-body scoping works"
    );
    assert_ne!(
        field(reference, "v_edge_safe_mps"),
        field(conservative, "v_edge_safe_mps"),
        "the two constructors must be distinguished by the parser"
    );
}
