# Occy / KIRRA — Safety Traceability Convention

**Doc ID (proposed):** KIRRA-OCCY-TRACE-001.
**Status:** Working convention. Parseable tag format + extraction script + CI
regression gate so every Governor-enforced safety goal stays linked to its
code site + its test. Drives the S3 traceability deliverable (issue #115)
and is consumed by `docs/safety/TRACEABILITY_MATRIX.md`.

---

## 1. Tag format

Every safety-relevant Governor check carries a one-line annotation:

    // SAFETY: <SG-list> | REQ: <kebab-id> | TEST: <test-list>

Fields:

- **`SAFETY:`** — fixed prefix, uppercase. Greppable: `grep -rE '// SAFETY:' src/ parko/`.
- **`<SG-list>`** — one or more Occy safety-goal identifiers (`SG1`..`SG9`),
  space-separated. The Occy scheme (see `docs/safety/OCCY_SAFETY_GOALS.md`)
  is the canonical one; the cross-mapping to AEGIS-SG-001 (`SG-001`..`SG-016`)
  lives in §6.2 of that doc and is **not** repeated in every tag.
  - Example single: `SAFETY: SG9`
  - Example multi: `SAFETY: SG3 SG9`
- **`REQ:`** — short kebab-case requirement identifier. Per-tag unique enough to
  identify what's being enforced (e.g. `fail-closed-nonfinite`,
  `velocity-hard-ceiling`, `unknown-command-denied`).
- **`TEST:`** — comma-separated test-function name(s) that verify the site.
  Names match `cargo test` invocation. May span lib / integration / proptest.

Multi-line context is allowed AFTER the SAFETY line as ordinary `//` comments;
only the first line is parsed.

### Example

```rust
// SAFETY: SG9 | REQ: fail-closed-nonfinite | TEST: test_nan_linear_velocity_is_denied_before_any_arithmetic
//
// IEEE 754 NaN/Inf values poison every subsequent computation silently;
// they must be rejected before any arithmetic on the proposed command.
if !cmd.linear_velocity_mps.is_finite() {
    return EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity);
}
```

---

## 2. Disposition vocabulary (SG status)

| Status | Meaning | Applies to |
|---|---|---|
| **ENFORCED** | At least one Governor code site is tagged and tested, AND the check is wired into the live request path | SG1, SG2, SG3, SG7, SG8 (Governor side), SG9 |
| **PENDING-WIRING** | Tagged + tested in isolation, but the live request path doesn't yet feed the inputs the check needs; live enforcement gated on a wiring follow-up issue | (none currently — SG2 graduated to ENFORCED once the Option-B adapter wiring landed, #128/#131) |
| **DELEGATED** | The safety goal is realized outside the Governor — integrator perception, map prior, control adapter; the residual is carried as an SEooC assumption-of-use | SG4, SG5, SG6, SG8 (standing-MRC half) |
| **PENDING** | Coverage hole with no in-tree check: a real implementation gap, tracked in its own issue | (none currently) |
| **STRUCTURAL** | Enforced by absence of a parameter / by type signature; a test pins the property | SG7 (doer-agnostic property) — coexists with ENFORCED if the SG also has tagged sites |

### Recorded exceptions (the CI gate consults these)

The CI gate at `traceability_gate::ci_gate_tests` recognizes these as
explicit non-violations:

| SG | Status | Tracked by |
|---|---|---|
| SG4 | DELEGATED | OCCY_INDEPENDENT_DETECTOR.md (D1 add-on); #126 (Perception Input Contract) |
| SG5 | DELEGATED | map-anchored, integrator side; #126 |
| SG6 | DELEGATED | impact detection PC1 / #102; #126 / #127 |
| SG8 (standing-MRC half) | DELEGATED | planner + control adapter; #126 |

The Governor halves of SG7 and SG8 ARE tagged in code and gated as ENFORCED.

---

## 3. Extraction

```
$ scripts/extract_safety_traceability.sh
```

Walks `src/` and `parko/`, parses every `// SAFETY: ...` line, groups by SG, and
emits `docs/safety/TRACEABILITY_MATRIX.md`. The CI gate (a Rust `#[test]` in
`src/traceability_gate.rs::ci_gate_tests`) runs the same parse and asserts:

1. Every ENFORCED SG has ≥ 1 tagged code site.
2. Every tagged site has a non-empty `REQ:` and `TEST:` field.
3. The SG ids in every tag are valid (`SG1`..`SG9`).

The DELEGATED / PENDING exceptions in §2 are recognized: their absence from the
ENFORCED set is not a failure.

---

## 4. Adding a new safety check

1. Implement the check in the Governor.
2. Add the `// SAFETY: ...` line directly above it.
3. Add (or identify) the test that verifies it; list it in `TEST:`.
4. Regenerate the matrix doc (or wait for CI to do it).
5. The gate-test catches accidental untagged or untested additions.

---

## 5. Removing or relaxing a check

The tag is the human signal that a piece of code is safety-critical. **Removing
the check requires removing the tag**, which removes the row from the matrix —
which is the loud, version-controlled artifact a reviewer will see.

Never remove a tag silently. If a check is intentionally removed, its absence
must be reflected in the safety goal's disposition (e.g. ENFORCED → DELEGATED
with a corresponding SEooC contract entry).
