#!/usr/bin/env python3
"""Tests for the Kirra World domain-logic gate.

Every case builds a throwaway fixture and runs the real gate against it. The
negative half carries the weight: a gate that cannot block proves nothing, and a
gate that blocks the shapes the prototype legitimately has would simply be
switched off.

Three properties:

  * while the ruling is unrecorded, each forbidden shape BLOCKS;
  * the shapes WM-1 actually shipped (unconstructible placeholders, intra-family
    dependency edges, doc comments quoting the rule) do NOT block;
  * recording the ruling RELEASES the gate — the self-releasing property, which
    is what stops the gate being deleted rather than satisfied.

Run:  python3 ci/test_world_domain_logic_gate.py
"""

from __future__ import annotations

import importlib.util
import shutil
import sys
import tempfile
import textwrap
from pathlib import Path

CI = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("wdlg", CI / "check_world_domain_logic_gate.py")
gate = importlib.util.module_from_spec(_spec)
sys.modules["wdlg"] = gate
_spec.loader.exec_module(gate)

RESULTS: list[tuple[bool, str, str]] = []


def record(ok: bool, name: str, detail: str = "") -> None:
    RESULTS.append((ok, name, detail))


PENDING_RULING = """
```
Safety-assurance ruling: PENDING

Owner: UNASSIGNED
Date: UNASSIGNED
Scope classification: UNASSIGNED
Rationale: UNASSIGNED
Assumptions: UNASSIGNED
Required evidence: UNASSIGNED
Conditions that reopen the decision: UNASSIGNED
```
"""

RECORDED_RULING = """
```
Safety-assurance ruling: RECORDED

Owner: A. Named Person
Date: 2026-09-01
Scope classification: safety-related, non-authoritative
Rationale: Fence A and Fence B hold and are enforced in CI; the checker's inputs
Assumptions: the checker reads no Kirra World output
Required evidence: the fence stays green; annual re-review
Conditions that reopen the decision: any authoritative use of semantic output
```
"""


class Fixture:
    def __init__(self, ruling: str = PENDING_RULING) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="kirra-wdlg-"))
        adr = self.root / gate.ADR_PATH
        adr.parent.mkdir(parents=True, exist_ok=True)
        adr.write_text(
            "# ADR-0042\n\n## Decision record\n" + textwrap.dedent(ruling),
            encoding="utf-8",
        )
        # The allowlist the imported fence module expects to exist.
        (self.root / "ci").mkdir(parents=True, exist_ok=True)
        (self.root / gate.fence.ALLOWLIST_NAME).write_text("", encoding="utf-8")

    def world_crate(self, name: str, body: str, deps: list[str] | None = None) -> None:
        d = self.root / "crates" / name
        (d / "src").mkdir(parents=True, exist_ok=True)
        lines = [f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n']
        if deps:
            lines.append("[dependencies]")
            for dep in deps:
                lines.append(f'{dep} = {{ path = "../{dep}" }}')
        (d / "Cargo.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
        (d / "src" / "lib.rs").write_text(textwrap.dedent(body), encoding="utf-8")

    def run(self) -> int:
        return gate.check(self.root, verbose=False)[0]

    def close(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)


PLACEHOLDER_BODY = """
    /// A placeholder.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct EntityId(());

    /// Another placeholder.
    #[derive(Debug, Clone)]
    pub struct ObservationId(());
"""


def expect(code: int, fx: Fixture, name: str) -> None:
    got = fx.run()
    record(got == code, name, f"expected exit {code}, got {got}")


# ---------------------------------------------------------------------------
# The shapes WM-1 actually shipped must NOT block
# ---------------------------------------------------------------------------


def t01_placeholders_alone_do_not_block() -> None:
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", PLACEHOLDER_BODY)
        expect(gate.EXIT_OK, fx, "01 unconstructible placeholders alone do not block")
    finally:
        fx.close()


def t02_intra_family_dependency_edges_do_not_block() -> None:
    """`-store` and `-service` depending on the core IS ADR-0040's proposed
    graph — the thing WM-1 merged to demonstrate. Blocking it would make the
    gate fire on the prototype it is meant to protect."""
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", PLACEHOLDER_BODY)
        fx.world_crate("kirra-world-store", "pub use kirra_world::EntityId;\n", deps=["kirra-world"])
        fx.world_crate(
            "kirra-world-service",
            "pub use kirra_world_store::EntityId;\n",
            deps=["kirra-world", "kirra-world-store"],
        )
        expect(gate.EXIT_OK, fx, "02 intra-family dependency edges do not block")
    finally:
        fx.close()


def t03_prose_quoting_the_rule_does_not_block() -> None:
    """The crates' doc comments describe what they may not contain, in the words
    the gate searches for. Describing the rule is not breaking it."""
    fx = Fixture()
    try:
        fx.world_crate(
            "kirra-world",
            '''
            //! No `fn`, no `impl`, no `const FOO: u8`, no `static BAR: u8`,
            //! no `macro_rules! x`, and no `struct Thing { field: u8 }`.
            //! A string mentioning them: "fn impl const static".
            #[derive(Debug)]
            pub struct EntityId(());
            ''',
        )
        expect(gate.EXIT_OK, fx, "03 prose and strings naming the forbidden items do not block")
    finally:
        fx.close()


def t04_derives_do_not_count_as_impls() -> None:
    fx = Fixture()
    try:
        fx.world_crate(
            "kirra-world",
            """
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
            pub struct MapId(());
            """,
        )
        expect(gate.EXIT_OK, fx, "04 #[derive(...)] is not a hand-written impl")
    finally:
        fx.close()


def t05_a_fieldless_enum_does_not_block() -> None:
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", "pub enum Axis { Origin, Corroboration }\n")
        expect(gate.EXIT_OK, fx, "05 a data-free enum is a declaration, not an implementation")
    finally:
        fx.close()


# ---------------------------------------------------------------------------
# Each forbidden shape must BLOCK
# ---------------------------------------------------------------------------


def t06_a_function_blocks() -> None:
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", "pub fn resolve() -> u8 { 1 }\n")
        expect(gate.EXIT_BLOCKED, fx, "06 a function blocks")
    finally:
        fx.close()


def t07_a_function_hidden_in_a_test_module_blocks() -> None:
    """The obvious hole if test modules were exempt."""
    fx = Fixture()
    try:
        fx.world_crate(
            "kirra-world",
            """
            pub struct EntityId(());
            #[cfg(test)]
            mod tests {
                fn resolve_the_whole_domain() -> u8 { 42 }
            }
            """,
        )
        expect(gate.EXIT_BLOCKED, fx, "07 a function inside #[cfg(test)] still blocks")
    finally:
        fx.close()


def t08_a_hand_written_impl_blocks() -> None:
    fx = Fixture()
    try:
        fx.world_crate(
            "kirra-world",
            """
            pub struct EntityId(());
            impl EntityId {}
            """,
        )
        expect(gate.EXIT_BLOCKED, fx, "08 a hand-written impl block blocks")
    finally:
        fx.close()


def t09_a_struct_with_named_fields_blocks() -> None:
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", "pub struct EntityId { id: u128 }\n")
        expect(gate.EXIT_BLOCKED, fx, "09 a struct with named fields blocks")
    finally:
        fx.close()


def t10_the_plausible_newtype_blocks() -> None:
    """The exact pull request this gate exists for: "just the EntityId newtype,
    it is obviously a u128"."""
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", "pub struct EntityId(u128);\n")
        expect(gate.EXIT_BLOCKED, fx, "10 `pub struct EntityId(u128);` blocks")
    finally:
        fx.close()


def t11_an_enum_variant_with_data_blocks() -> None:
    fx = Fixture()
    try:
        fx.world_crate(
            "kirra-world",
            "pub enum ResolutionOutcome { Found(u8), NotFound, Unknown }\n",
        )
        expect(gate.EXIT_BLOCKED, fx, "11 an enum variant carrying data blocks")
    finally:
        fx.close()


def t12_a_const_item_blocks() -> None:
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", "pub const MAX_ENTITIES: usize = 4096;\n")
        expect(gate.EXIT_BLOCKED, fx, "12 a const item blocks")
    finally:
        fx.close()


def t13_a_static_item_blocks() -> None:
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", "static REGISTRY: u8 = 0;\n")
        expect(gate.EXIT_BLOCKED, fx, "13 a static item blocks")
    finally:
        fx.close()


def t14_a_macro_definition_blocks() -> None:
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", "macro_rules! entity { () => {} }\n")
        expect(gate.EXIT_BLOCKED, fx, "14 a macro definition blocks")
    finally:
        fx.close()


def t15_an_external_dependency_blocks() -> None:
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", PLACEHOLDER_BODY, deps=["serde"])
        expect(gate.EXIT_BLOCKED, fx, "15 a dependency outside the kirra-world family blocks")
    finally:
        fx.close()


def t16_the_prefix_rule_covers_a_new_name() -> None:
    """A crate called `kirra-world-domain` must not escape by being new."""
    fx = Fixture()
    try:
        fx.world_crate("kirra-world", PLACEHOLDER_BODY)
        fx.world_crate("kirra-world-domain", "pub fn adjudicate() {}\n")
        expect(gate.EXIT_BLOCKED, fx, "16 the kirra-world* prefix rule covers a newly named crate")
    finally:
        fx.close()


# ---------------------------------------------------------------------------
# Self-release
# ---------------------------------------------------------------------------


def t17_recording_the_ruling_releases_the_gate() -> None:
    fx = Fixture(ruling=RECORDED_RULING)
    try:
        # Real domain logic, which would block under a pending ruling.
        fx.world_crate("kirra-world", "pub struct EntityId(u128);\npub fn resolve() {}\n")
        expect(gate.EXIT_OK, fx, "17 a recorded ruling releases the gate")
    finally:
        fx.close()


def t18_a_placeholder_owner_does_not_release_it() -> None:
    for owner in ("TBD", "the safety-assurance owner", "", "unassigned"):
        fx = Fixture(ruling=RECORDED_RULING.replace("A. Named Person", owner))
        try:
            fx.world_crate("kirra-world", "pub struct EntityId(u128);\n")
            got = fx.run()
            record(
                got == gate.EXIT_BLOCKED,
                f"18 owner `{owner or '(empty)'}` does not release the gate",
                f"expected block, got {got}",
            )
        finally:
            fx.close()


def t19_a_partially_filled_ruling_does_not_release_it() -> None:
    """An owner alone is not a ruling. The classification and the reopening
    conditions are the parts a later reader actually needs."""
    partial = RECORDED_RULING.replace(
        "Scope classification: safety-related, non-authoritative",
        "Scope classification: TBD",
    )
    fx = Fixture(ruling=partial)
    try:
        fx.world_crate("kirra-world", "pub fn resolve() {}\n")
        expect(gate.EXIT_BLOCKED, fx, "19 a partially filled ruling does not release the gate")
    finally:
        fx.close()


def t20_a_missing_ruling_record_fails_closed() -> None:
    fx = Fixture(ruling="\nNo record here at all.\n")
    try:
        fx.world_crate("kirra-world", PLACEHOLDER_BODY)
        expect(gate.EXIT_BLOCKED, fx, "20 an absent ruling record fails closed")
    finally:
        fx.close()


def t21_a_missing_adr_fails_closed() -> None:
    fx = Fixture()
    try:
        (fx.root / gate.ADR_PATH).unlink()
        fx.world_crate("kirra-world", PLACEHOLDER_BODY)
        expect(gate.EXIT_BLOCKED, fx, "21 a deleted ADR fails closed rather than passing")
    finally:
        fx.close()


# ---------------------------------------------------------------------------
# Against the real repository
# ---------------------------------------------------------------------------


def t22_the_real_repository_holds() -> None:
    code, _ = gate.check(CI.parent, verbose=False)
    record(code == gate.EXIT_OK, "22 the real repository passes the gate", f"exit {code}")


def t23_the_real_ruling_is_still_open() -> None:
    """If this ever fails, the ruling was recorded — which is the goal, and the
    moment to re-read what it says rather than to delete this test."""
    text = (CI.parent / gate.ADR_PATH).read_text(encoding="utf-8")
    ruling = gate.parse_ruling(text)
    record(
        ruling is not None and not ruling.recorded,
        "23 ADR-0042's ruling is still unrecorded (the gate is load-bearing today)",
        "the ruling now parses as RECORDED — verify the owner and classification are real",
    )


def t24_the_real_world_crates_are_still_shape_only() -> None:
    """Directly, against the shipped crates, independent of the ruling state —
    so this keeps meaning something after the ruling is recorded and the gate
    itself stops constraining them."""
    repo = CI.parent
    manifests = gate.fence.find_manifests(repo)
    all_dirs = gate.fence.crate_dirs(manifests)
    findings = []
    for pkg in sorted(n for n in manifests if gate.fence.is_world_package(n)):
        crate_dir, _ = manifests[pkg]
        for src in gate.fence.rust_sources(crate_dir, all_dirs):
            findings.extend(gate.scan_source(pkg, src, str(src.relative_to(repo))))
    record(
        not findings,
        "24 the shipped kirra-world* crates contain shape only",
        "\n".join(f.render() for f in findings),
    )


OWNED_PENDING_RULING = """
```
Safety-assurance ruling: PENDING

Owner: A. Named Person
Owner assigned: 2026-08-03
Date: UNASSIGNED
Scope classification: UNASSIGNED
Rationale: UNASSIGNED
Assumptions: UNASSIGNED
Required evidence: UNASSIGNED
Conditions that reopen the decision: UNASSIGNED
```
"""


def t25_an_assigned_owner_does_not_release_the_gate() -> None:
    """Assigning accountability is not taking the decision. The gate must keep
    holding, or naming someone would silently unblock domain work."""
    fx = Fixture(ruling=OWNED_PENDING_RULING)
    try:
        fx.world_crate("kirra-world", "pub struct EntityId(u128);\n")
        expect(gate.EXIT_BLOCKED, fx, "25 an assigned owner alone does not release the gate")
    finally:
        fx.close()


def t26_an_assigned_owner_is_visible_and_distinct_from_unowned() -> None:
    """The state change this models: before, both cases printed the same thing,
    so accepting responsibility was invisible in CI."""
    owned = Fixture(ruling=OWNED_PENDING_RULING)
    unowned = Fixture(ruling=PENDING_RULING)
    try:
        r_owned = gate.parse_ruling((owned.root / gate.ADR_PATH).read_text())
        r_unowned = gate.parse_ruling((unowned.root / gate.ADR_PATH).read_text())
        record(
            r_owned.owner_assigned and r_owned.owner == "A. Named Person",
            "26 an assigned owner is parsed and named",
            f"got owner={r_owned.owner!r}",
        )
        record(
            not r_unowned.owner_assigned,
            "26b an UNASSIGNED owner is not treated as assigned",
            f"got owner={r_unowned.owner!r}",
        )
        # Neither is recorded — the distinction is orthogonal to release.
        record(
            not r_owned.recorded and not r_unowned.recorded,
            "26c neither state counts as a recorded ruling",
        )
    finally:
        owned.close()
        unowned.close()


def t27_a_placeholder_owner_is_not_an_assignment() -> None:
    for owner in ("TBD", "the safety-assurance owner", "UNASSIGNED", ""):
        fx = Fixture(ruling=OWNED_PENDING_RULING.replace("A. Named Person", owner))
        try:
            r = gate.parse_ruling((fx.root / gate.ADR_PATH).read_text())
            record(
                not r.owner_assigned,
                f"27 owner `{owner or '(empty)'}` is not an assignment",
                f"got owner={r.owner!r}",
            )
        finally:
            fx.close()


def t28_the_real_adr_names_an_owner() -> None:
    """Against the shipped ADR. If this fails, the owner was removed."""
    text = (CI.parent / gate.ADR_PATH).read_text(encoding="utf-8")
    r = gate.parse_ruling(text)
    record(
        r is not None and r.owner_assigned,
        f"28 ADR-0042 names a real safety-assurance owner ({r.owner if r else '?'})",
        "the ruling block has no named owner",
    )


def t29_the_real_adr_records_the_independence_posture() -> None:
    """Against the shipped ADR.

    The posture is the statement that stops a reader assuming the eventual
    ruling is an independent assessment. Unlike a `UNASSIGNED` field, deleting
    it makes the document look *stronger*, not weaker — nothing else in the ADR
    would go red, and CI would stay green while the record quietly became a
    claim it has not earned. So it is pinned here.

    Substring checks, deliberately: these are the load-bearing clauses, not the
    prose around them. Rewording the surrounding paragraph is free; removing a
    limitation is not.
    """
    text = (CI.parent / gate.ADR_PATH).read_text(encoding="utf-8")
    required = {
        "the self-assessment framing": "self-assessment, not independent assurance review",
        "the role-separation admission": "system-owner and assessor roles",
        "the no-assessment-occurred statement": "No independent assessment has occurred",
        "the not-a-certification statement": "scope classification, not a safety certification",
        "the reopening pre-commitment": "must reopen if Kirra World gains authority",
    }
    missing = [label for label, needle in required.items() if needle.lower() not in text.lower()]
    record(
        not missing,
        "29 ADR-0042 records the independence posture (self-assessment, limits, reopening floor)",
        "missing from the ADR: " + "; ".join(missing),
    )


def t30_the_independence_posture_does_not_release_the_gate() -> None:
    """Recording *how* the ruling will be made is not making it.

    t23 already asserts the shipped ruling parses as unrecorded. This asserts
    the narrower thing the posture PR could plausibly have broken: that the
    ruling's own fields are all still open, so no single field was quietly
    filled in while adding prose about the ruling's limits.
    """
    text = (CI.parent / gate.ADR_PATH).read_text(encoding="utf-8")
    r = gate.parse_ruling(text)
    if r is None:
        record(False, "30 the independence posture leaves every ruling field open", "no ruling record parsed")
        return
    open_fields = [f for f in gate.REQUIRED_FIELDS if f != gate.OWNER_FIELD and r.fields.get(f, "").strip().lower() in gate.PLACEHOLDERS]
    expected = [f for f in gate.REQUIRED_FIELDS if f != gate.OWNER_FIELD]
    record(
        open_fields == expected and not r.recorded,
        "30 the independence posture leaves every ruling field open and the gate holding",
        f"filled: {sorted(set(expected) - set(open_fields))}; recorded={r.recorded}",
    )


ALL = [v for k, v in sorted(globals().items()) if k.startswith("t") and k[1:3].isdigit()]


def main() -> int:
    for fn in ALL:
        try:
            fn()
        except Exception as exc:  # a crashing test is a failing test
            record(False, fn.__name__, f"raised {type(exc).__name__}: {exc}")

    failed = [r for r in RESULTS if not r[0]]
    for ok, name, detail in RESULTS:
        print(f"  {'PASS' if ok else 'FAIL'}  {name}")
        if not ok and detail:
            print(textwrap.indent(detail, "        "))
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
