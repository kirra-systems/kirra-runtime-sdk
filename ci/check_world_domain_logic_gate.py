#!/usr/bin/env python3
"""No Kirra World domain implementation until ADR-0042's scope ruling is recorded.

THE RULE
--------
    No Kirra World domain implementation may merge until ADR-0042's
    safety-assurance scope ruling (Decision 5) is assigned to a named owner
    and recorded.

That sentence exists in ADR-0042. This file is the reason it is a condition
rather than a preference.

WHY A GATE AND NOT A PARAGRAPH
------------------------------
The `kirra-world*` crates currently hold ten unconstructible placeholder types
and nothing else. Their emptiness is not an unfinished state -- it IS the
guardrail, holding the shape open while the decision that governs it is still
being made.

The risk that creates is organizational, not technical. An empty crate reads as
an invitation. Six weeks from now, with the ruling still unassigned, "just the
EntityId newtype, it is obviously a u128" is an entirely reasonable-sounding
pull request, and the reasoning that makes it reasonable is exactly the
reasoning the ruling was supposed to perform. A rule that lives only in prose
loses that argument, because prose does not run in CI and the person making the
argument has a deadline.

WHAT THIS GATE DOES NOT DO
--------------------------
It does not prevent a determined bypass, and it is not trying to. The ruling
record it reads is a block of text in a document, and anyone may edit that
document. That is correct: the ADR edit IS the ruling, and it belongs in review.

What the gate converts is the FAILURE MODE. Without it, the guardrail erodes
silently -- one small type at a time, each defensible on its own, with nobody
ever deciding to bypass anything. With it, unblocking requires editing a named
field in a named ADR, in a diff a reviewer will see, with an owner's name
attached. It makes the bypass deliberate and visible instead of gradual and
invisible. That is the whole of the claim.

The gate is also SELF-RELEASING. When the ruling is recorded it relaxes on its
own and says so, so nobody has to delete it to proceed -- because a gate that
must be deleted to make progress gets deleted wholesale, taking its checks with
it, rather than satisfied.

WHAT COUNTS AS DOMAIN IMPLEMENTATION
------------------------------------
Deliberately mechanical, so it is falsifiable rather than a judgement call.
While the ruling is unrecorded, a `kirra-world*` crate may contain only
declarations that cannot execute or carry state:

  1. no `fn` -- any function at all, including in test modules
  2. no `impl` block (a `#[derive(...)]` is fine; a hand-written impl is not)
  3. no struct field other than the private unit `()` placeholder
  4. no enum variant carrying data
  5. no `const` or `static` item -- a constant table is data, and data is where
     a schema hides before anyone admits to having chosen one
  6. no `macro_rules!`
  7. no dependencies of any kind, including dev-dependencies

Rule 1 is strict on purpose. Allowing functions inside `#[cfg(test)]` would
leave the obvious hole, and there is no test in these crates today that the
rule costs anything: they hold types nobody can construct, so there is nothing
to exercise. If a genuine need appears, that is a good reason to record the
ruling.

Usage
-----
    python3 ci/check_world_domain_logic_gate.py
    python3 ci/check_world_domain_logic_gate.py --root /path/to/tree --verbose
"""

from __future__ import annotations

import argparse
import importlib.util
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

CI = Path(__file__).resolve().parent
REPO = CI.parent

# The package-naming rule (including the `kirra-world` PREFIX, so an adapter
# cannot evade this by being called `kirra-world-domain`) lives in the fence and
# is imported rather than restated. Two copies of that rule would drift, and the
# copy that drifted would be the one protecting nothing.
_spec = importlib.util.spec_from_file_location(
    "kirra_world_fence", CI / "check_kirra_world_bidirectional_fence.py"
)
fence = importlib.util.module_from_spec(_spec)
sys.modules["kirra_world_fence"] = fence
_spec.loader.exec_module(fence)

ADR_PATH = "docs/adr/0042-world-model-terminology-and-safety-boundary-scope.md"

EXIT_OK = 0
EXIT_BLOCKED = 1
EXIT_USAGE = 2

# Every field the ruling record must carry. A ruling missing any of them is not
# a ruling; it is an intention.
# The owner is tracked separately from the rest: assigning accountability and
# taking the decision are two different events, and a gate that could not tell
# them apart would report "not recorded" identically before and after someone
# accepted responsibility for producing the ruling. That erases a real state
# change, and the whole reason this file exists is that invisible states are the
# ones that drift.
OWNER_FIELD = "Owner"

REQUIRED_FIELDS = (
    "Owner",
    "Date",
    "Scope classification",
    "Rationale",
    "Assumptions",
    "Required evidence",
    "Conditions that reopen the decision",
)

# Values that look filled in but name nobody and decide nothing. "the
# safety-assurance owner" is on the list deliberately: it is a ROLE, and a role
# cannot be held accountable for a decision -- the point of the exercise is a
# person.
PLACEHOLDERS = {
    "",
    "-",
    "--",
    "n/a",
    "na",
    "tbd",
    "tbc",
    "todo",
    "unassigned",
    "pending",
    "none",
    "unknown",
    "?",
    "xxx",
    "the safety-assurance owner",
    "safety-assurance owner",
    "safety owner",
    "owner",
    "to be completed by the owner",
    "to be decided",
}

RULING_HEADER = re.compile(r"^Safety-assurance ruling:\s*(.+?)\s*$", re.MULTILINE)


@dataclass
class Ruling:
    status: str
    fields: dict[str, str] = field(default_factory=dict)
    problems: list[str] = field(default_factory=list)

    @property
    def recorded(self) -> bool:
        return not self.problems

    @property
    def owner(self) -> str | None:
        """The named owner, or None if still a placeholder."""
        value = self.fields.get(OWNER_FIELD, "")
        return value if value.strip().lower() not in PLACEHOLDERS else None

    @property
    def owner_assigned(self) -> bool:
        return self.owner is not None


def parse_ruling(text: str) -> Ruling | None:
    """Extract the ruling record from ADR-0042.

    Scans fenced code blocks rather than the whole document so that prose
    quoting the template -- which this ADR does, at length -- cannot be mistaken
    for the record itself.
    """
    for block in re.findall(r"```[a-z]*\n(.*?)```", text, re.DOTALL):
        header = RULING_HEADER.search(block)
        if not header:
            continue
        ruling = Ruling(status=header.group(1).strip())
        for line in block.splitlines():
            if ":" not in line:
                continue
            key, _, value = line.partition(":")
            key = key.strip()
            if key in REQUIRED_FIELDS:
                ruling.fields[key] = value.strip()

        if ruling.status.strip().lower() in {"pending", "unassigned", "not recorded", "none"}:
            ruling.problems.append(f"status is `{ruling.status}` — no ruling has been made")
        for name in REQUIRED_FIELDS:
            value = ruling.fields.get(name)
            if value is None:
                ruling.problems.append(f"field `{name}` is absent from the record")
            elif value.strip().lower() in PLACEHOLDERS:
                ruling.problems.append(
                    f"field `{name}` is `{value or '(empty)'}` — a placeholder, not a decision"
                )
        return ruling
    return None


# ===========================================================================
# Source scanning
# ===========================================================================

# A placeholder struct: `pub struct Name(());` — a private unit field, so
# nothing outside the crate can construct one.
PLACEHOLDER_STRUCT = re.compile(r"\bstruct\s+\w+\s*\(\s*\(\s*\)\s*\)\s*;")

FORBIDDEN_ITEMS: tuple[tuple[str, re.Pattern, str], ...] = (
    (
        "function",
        re.compile(r"(?:^|[^\w])(?:pub\s+(?:\([^)]*\)\s*)?)?(?:async\s+|const\s+|unsafe\s+)*fn\s+\w+"),
        "a function is executable behaviour; while the ruling is open these crates hold shape only",
    ),
    (
        "impl block",
        re.compile(r"(?:^|[^\w])impl(?:\s*<[^>]*>)?\s+[\w:]"),
        "a hand-written impl carries behaviour; `#[derive(...)]` is available and is not flagged",
    ),
    (
        "const item",
        re.compile(r"(?:^|[^\w])(?:pub\s+(?:\([^)]*\)\s*)?)?const\s+[A-Z_][A-Z0-9_]*\s*:"),
        "a constant is data, and data is where a schema hides before anyone admits to choosing one",
    ),
    (
        "static item",
        re.compile(r"(?:^|[^\w])(?:pub\s+(?:\([^)]*\)\s*)?)?static\s+[A-Z_][A-Z0-9_]*\s*:"),
        "a static is state; these crates are declarations, not a running component",
    ),
    (
        "macro definition",
        re.compile(r"(?:^|[^\w])macro_rules!\s*\w+"),
        "a macro generates code, which is the same problem one level removed",
    ),
)

# A struct with named fields, or a tuple struct whose payload is anything other
# than the unit placeholder.
STRUCT_WITH_BODY = re.compile(r"\bstruct\s+(\w+)\s*(\{|\()")
ENUM_WITH_DATA = re.compile(r"\benum\s+(\w+)\s*\{([^}]*)\}", re.DOTALL)


@dataclass(frozen=True)
class Finding:
    package: str
    location: str
    kind: str
    detail: str

    def render(self) -> str:
        return (
            f"  {self.kind}\n"
            f"      where : {self.location}\n"
            f"      what  : {self.detail}\n"
        )


def scan_source(pkg: str, path: Path, rel: str) -> list[Finding]:
    raw = path.read_text(encoding="utf-8", errors="replace")
    # Comments and string literals are stripped first: an ADR quoted in a doc
    # comment describes the rule, it does not break it. This is the same
    # discipline the bidirectional fence applies, for the same reason.
    src = fence.strip_rust(raw)
    out: list[Finding] = []

    for kind, pattern, why in FORBIDDEN_ITEMS:
        for m in pattern.finditer(src):
            line = src.count("\n", 0, m.start()) + 1
            out.append(Finding(pkg, f"{rel}:{line}", kind, why))

    for m in STRUCT_WITH_BODY.finditer(src):
        name, opener = m.group(1), m.group(2)
        tail = src[m.start() :]
        if opener == "(" and PLACEHOLDER_STRUCT.match(tail):
            continue  # the sanctioned `Name(());` placeholder
        line = src.count("\n", 0, m.start()) + 1
        out.append(
            Finding(
                pkg,
                f"{rel}:{line}",
                "struct with a body",
                f"`{name}` carries fields; the only permitted form while the ruling is open "
                f"is the unconstructible placeholder `struct {name}(());`",
            )
        )

    for m in ENUM_WITH_DATA.finditer(src):
        name, body = m.group(1), m.group(2)
        if "(" in body or "{" in body:
            line = src.count("\n", 0, m.start()) + 1
            out.append(
                Finding(
                    pkg,
                    f"{rel}:{line}",
                    "enum variant carrying data",
                    f"`{name}` has a variant with a payload, which fixes a representation",
                )
            )
    return out


def check(root: Path, verbose: bool) -> tuple[int, list[str]]:
    notes: list[str] = []
    adr = root / ADR_PATH
    if not adr.is_file():
        print(f"FAIL — {ADR_PATH} not found; the ruling record cannot be read.")
        return EXIT_BLOCKED, notes

    ruling = parse_ruling(adr.read_text(encoding="utf-8"))
    if ruling is None:
        print(f"FAIL — no `Safety-assurance ruling:` record found in {ADR_PATH}.")
        print("       The gate cannot confirm the ruling is open OR closed, so it fails closed.")
        return EXIT_BLOCKED, notes

    manifests = fence.find_manifests(root)
    world = sorted(n for n in manifests if fence.is_world_package(n))
    all_dirs = fence.crate_dirs(manifests)
    notes.append(f"kirra-world* packages: {len(world)}")
    notes.append(f"ruling status: {ruling.status}")
    notes.append(f"ruling owner: {ruling.owner or '(unassigned)'}")

    if ruling.recorded:
        owner = ruling.fields.get("Owner", "?")
        date = ruling.fields.get("Date", "?")
        classification = ruling.fields.get("Scope classification", "?")
        print("Kirra World domain-logic gate: OPEN")
        print(f"  ADR-0042 Decision 5 ruling is RECORDED — {owner}, {date}")
        print(f"  scope classification: {classification}")
        print("  Domain implementation is unblocked. This gate no longer constrains "
              "the kirra-world* crates;")
        print("  ADR-0039's Fence A and Fence B continue to apply and are checked separately.")
        return EXIT_OK, notes

    findings: list[Finding] = []
    for pkg in world:
        crate_dir, data = manifests[pkg]
        for section in ("dependencies", "build-dependencies", "dev-dependencies"):
            for dep in sorted(data.get(section, {})):
                # Edges INSIDE the kirra-world family are the three-node graph
                # ADR-0040 proposed and WM-1 built to prove -- `-store` and
                # `-service` depending on the core is the shape under
                # evaluation, not an implementation decision. Only an edge out
                # of the family commits to something the ruling has not
                # authorized. (Fence A separately forbids the actuation ones,
                # ruling or no ruling.)
                if fence.is_world_package(dep):
                    continue
                findings.append(
                    Finding(
                        pkg,
                        f"{crate_dir.relative_to(root)}/Cargo.toml",
                        f"{section} entry",
                        f"`{dep}` — a placeholder crate links nothing outside its own graph; "
                        f"an external dependency is a design decision the pending ruling "
                        f"has not authorized",
                    )
                )
        for src in fence.rust_sources(crate_dir, all_dirs):
            findings.extend(scan_source(pkg, src, str(src.relative_to(root))))

    if not findings:
        if ruling.owner_assigned:
            print("Kirra World domain-logic gate: HOLDING (owner assigned, ruling pending)")
            print(f"  ADR-0042 Decision 5 is OWNED by: {ruling.owner}")
            print(f"  The ruling itself is not yet made ({len(ruling.problems)} field(s) outstanding):")
        else:
            print("Kirra World domain-logic gate: HOLDING (UNOWNED)")
            print("  ADR-0042 Decision 5 has no named owner and no ruling.")
            print(f"  {len(ruling.problems)} gap(s):")
        for p in ruling.problems:
            print(f"    - {p}")
        print(f"  {len(world)} kirra-world* package(s) contain shape only. Nothing to block.")
        print()
        if ruling.owner_assigned:
            print("  Accountability is assigned; the decision is not taken. Domain")
            print("  implementation stays blocked until the ruling is recorded in")
            print(f"  {ADR_PATH}, at which point this gate releases itself.")
        else:
            print("  No Kirra World domain implementation may merge until that ruling is")
            print("  assigned to a named owner and recorded. Record it in")
            print(f"  {ADR_PATH} and this gate releases itself.")
        return EXIT_OK, notes

    print("Kirra World domain-logic gate: BLOCKED\n")
    print("ADR-0042 Decision 5's safety-assurance scope ruling is NOT recorded:")
    for p in ruling.problems:
        print(f"    - {p}")
    print()
    print("…but the Kirra World crates now contain domain implementation:\n")
    for f in findings:
        print(f"  [{f.package}]")
        print(f.render(), end="")
    print(f"{len(findings)} item(s) blocked.\n")
    print("THE RULE: no Kirra World domain implementation may merge until ADR-0042's")
    print("scope ruling is assigned to a named owner and recorded.")
    print()
    print("The emptiness of these crates is not an unfinished state — it is holding the")
    print("shape open while the decision that governs it is still being made. Fill the")
    print("ruling in, not the crates.")
    return EXIT_BLOCKED, notes


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--root", default=str(REPO))
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args(argv)

    code, notes = check(Path(args.root).resolve(), args.verbose)
    if args.verbose:
        for n in notes:
            print(f"  note: {n}")
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
