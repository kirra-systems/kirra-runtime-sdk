#!/usr/bin/env python3
"""Mick actuation fence — the structural "no path to the motors" gate.

Mick (the LLM intent layer) proposes typed intents; it must be STRUCTURALLY
unable to actuate — not conventionally, structurally. The doer-checker thesis
only holds if the intent side cannot reach a `cmd_vel` publisher, a serial
seam, or the release-token mint by ANY dependency route. This gate makes the
missing edge a CI invariant, so "just publish cmd_vel for a quick test" is a
build failure, not a temptation (the ADR-0014 anti-pattern).

Mechanics (the check_orphan_cores.py precedent: pure-Python, no toolchain):

1. DEPENDENCY-GRAPH FENCE — for every fenced crate, compute the transitive
   closure of its NORMAL `[dependencies]` (+ `[target.*.dependencies]`) over
   workspace path deps, and fail if the closure contains any forbidden
   workspace crate (the actuation seams) or any forbidden external crate
   (ROS/DDS/serial/GPIO stacks). `[dev-dependencies]` are exempt from the
   closure — they cannot reach a shipped binary — but see (2).

2. SYMBOL FENCE — scan every Rust source of the fenced crates (src/, bins,
   examples, AND tests — comment-stripped) for the actuation seam tokens
   (`RosReleaseGate`, `MotorSerial`, `issue_ros_release`, the
   `kirra_ros_release` FFI, `write_twist`, `ReleaseToken`). This catches an
   edge smuggled in through a dev-dependency or a re-export rename that the
   manifest walk cannot see.

Heuristic honesty: the manifest walk is textual (tomllib over Cargo.toml),
and the symbol scan is a token match after comment stripping. A determined
evasion beats both — the gate is a ratchet against the accidental/convenient
edge, not a proof. The e2e evidence artifact remains
crates/kirra-actuation-consumer/tests/ (the chokepoint drills).

Exit 0 = fence intact. Exit 1 = an actuation edge (or a gate self-check
failure — a fenced/forbidden crate going missing must not silently void the
gate).
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Crates that must have NO path to actuation. Every crate that houses Mick
# code or a Mick-facing binary belongs here.
FENCED_CRATES = [
    "crates/kirra-mick",  # the LLM transport (OllamaClient)
]

# Workspace crates that ARE (or carry) the actuation seam. An edge from a
# fenced crate to any of these is the fence breach this gate exists to catch.
FORBIDDEN_WORKSPACE = {
    "kirra-release-token":      "the release-token mint/verify seam (ADR-0033)",
    "kirra-actuation-consumer": "the verifying motor consumer (serial seam)",
    "kirra-inline-governor":    "the EP-01 in-line SHM enforcement path",
    "kirra-ros2-adapter":       "the ROS 2 node (cmd_vel / actuator topics)",
    "kirra-hv-carrier":         "the hypervisor shared-memory command carrier",
}

# External crates that would give a fenced crate transport to an actuator:
# ROS client libs, DDS stacks, serial/CAN/GPIO access.
FORBIDDEN_EXTERNAL = {
    "r2r", "rclrs", "rosrust", "ros2-client",
    "rustdds", "cyclonedds-rs", "cyclonedds-sys", "zenoh", "iceoryx2",
    "serialport", "tokio-serial", "serial", "serial2",
    "socketcan", "rppal", "gpio-cdev", "linux-embedded-hal",
}

# Actuation seam tokens: matched as whole identifiers in comment-stripped
# fenced-crate source (src/ + examples/ + tests/). `ReleaseToken` and
# `write_twist` are the consumer-side vocabulary; `kirra_ros_release` is the
# FFI export family.
FORBIDDEN_SYMBOLS = [
    "kirra_ros_release",
    "RosReleaseGate",
    "MotorSerial",
    "issue_ros_release",
    "write_twist",
    "ReleaseToken",
]

LINE_COMMENT_RE = re.compile(r"//[^\n]*")
BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)


def load_manifest(crate_dir: Path) -> dict:
    with open(crate_dir / "Cargo.toml", "rb") as f:
        return tomllib.load(f)


def normal_deps(manifest: dict) -> dict[str, object]:
    """NORMAL dependency name→spec map: [dependencies] plus every
    [target.*.dependencies] table. dev-dependencies are deliberately
    excluded (they cannot reach a shipped bin; the symbol fence covers
    their source)."""
    deps: dict[str, object] = {}
    deps.update(manifest.get("dependencies", {}))
    for target_tbl in manifest.get("target", {}).values():
        deps.update(target_tbl.get("dependencies", {}))
    # build-dependencies could smuggle codegen, not an actuation transport at
    # runtime — but there is no reason for a fenced crate to have any; include
    # them so a weird edge still surfaces.
    deps.update(manifest.get("build-dependencies", {}))
    return deps


def workspace_members() -> dict[str, Path]:
    """Workspace crate name → directory, from the root manifest's members
    (globs resolved), including the root package itself."""
    with open(REPO / "Cargo.toml", "rb") as f:
        root = tomllib.load(f)
    members: dict[str, Path] = {}
    for member in root.get("workspace", {}).get("members", []):
        dirs = [REPO] if member == "." else sorted(REPO.glob(member))
        for d in dirs:
            manifest_path = d / "Cargo.toml" if d.is_dir() else None
            if manifest_path and manifest_path.exists():
                m = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
                name = m.get("package", {}).get("name")
                if name:
                    members[name] = d
    return members


def dep_name(name: str, spec: object) -> str:
    """The real crate name behind a dependency entry (honor `package =` renames)."""
    if isinstance(spec, dict) and "package" in spec:
        return spec["package"]
    return name


def closure(crate_dir: Path, members: dict[str, Path]) -> tuple[set[str], set[str]]:
    """(workspace crate names, external crate names) transitively reachable
    over NORMAL deps, following path deps through workspace members."""
    ws_seen: set[str] = set()
    ext_seen: set[str] = set()
    queue = [crate_dir]
    visited_dirs = set()
    while queue:
        d = queue.pop()
        if d in visited_dirs:
            continue
        visited_dirs.add(d)
        manifest = load_manifest(d)
        for raw_name, spec in normal_deps(manifest).items():
            name = dep_name(raw_name, spec)
            if name in members:
                if name not in ws_seen:
                    ws_seen.add(name)
                    queue.append(members[name])
            else:
                ext_seen.add(name)
    return ws_seen, ext_seen


def strip_comments(src: str) -> str:
    return LINE_COMMENT_RE.sub("", BLOCK_COMMENT_RE.sub("", src))


def symbol_hits(crate_dir: Path) -> list[tuple[Path, str]]:
    hits = []
    for rs in sorted(crate_dir.rglob("*.rs")):
        code = strip_comments(rs.read_text(encoding="utf-8", errors="replace"))
        for sym in FORBIDDEN_SYMBOLS:
            if re.search(rf"\b{re.escape(sym)}\b", code):
                hits.append((rs.relative_to(REPO), sym))
    return hits


def main() -> int:
    failures: list[str] = []
    members = workspace_members()

    # Gate self-checks: a fenced or forbidden crate going missing must not
    # silently void the fence (renames update this file in the same PR).
    for rel in FENCED_CRATES:
        if not (REPO / rel / "Cargo.toml").exists():
            failures.append(
                f"gate self-check: fenced crate `{rel}` has no Cargo.toml — "
                f"renamed/moved? update FENCED_CRATES in {Path(__file__).name}"
            )
    for name in FORBIDDEN_WORKSPACE:
        if name not in members:
            failures.append(
                f"gate self-check: forbidden workspace crate `{name}` is not a "
                f"workspace member — renamed/moved? update FORBIDDEN_WORKSPACE"
            )
    if failures:
        for f in failures:
            print(f"FAIL {f}")
        return 1

    for rel in FENCED_CRATES:
        crate_dir = REPO / rel
        ws, ext = closure(crate_dir, members)
        for name in sorted(ws & set(FORBIDDEN_WORKSPACE)):
            failures.append(
                f"{rel}: dependency closure reaches `{name}` — "
                f"{FORBIDDEN_WORKSPACE[name]}. Mick publishes intents, never "
                f"commands; remove the edge."
            )
        for name in sorted(ext & FORBIDDEN_EXTERNAL):
            failures.append(
                f"{rel}: dependency closure reaches external crate `{name}` "
                f"(an actuator transport). Remove the edge."
            )
        for path, sym in symbol_hits(crate_dir):
            failures.append(
                f"{path}: actuation seam token `{sym}` in fenced source. "
                f"Mick-side code must not touch the release/serial vocabulary."
            )
        print(
            f"ok   {rel}: closure = {len(ws)} workspace + {len(ext)} external "
            f"crates, 0 actuation edges"
        )

    if failures:
        print()
        for f in failures:
            print(f"FAIL {f}")
        print(
            "\nThe Mick actuation fence is load-bearing (doer-checker thesis): "
            "the intent layer must be STRUCTURALLY unable to reach the motors. "
            "If a fenced crate legitimately needs a new dependency, it must not "
            "be one of the actuation seams."
        )
        return 1
    print("mick actuation fence: INTACT")
    return 0


if __name__ == "__main__":
    sys.exit(main())
