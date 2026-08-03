#!/usr/bin/env python3
"""Kirra World bidirectional fence — the structural boundary, built before the
subsystem it fences.

Two directions, and the pair is the point:

  Fence A — KIRRA WORLD CANNOT ACT.
      Semantic knowledge may be wrong, stale, or later revised; that is what a
      world model IS. So it must be structurally unable to reach an actuator or
      an authorization: no `cmd_vel`, no ROS actuation messages, no motor or
      serial consumer, no release-token mint/verify, no trajectory approval, no
      becoming a planner or a checker. Its only permitted outputs are typed
      knowledge — entities, observations, relationships, provenance, freshness,
      resolution outcomes, projections, queries, historical evidence.

  Fence B — THE SAFETY PATH CANNOT DEPEND ON KIRRA WORLD.
      The transitive dependency closure of the safety decision and authorization
      path must not reach the `kirra-world*` crates, their service APIs, their
      database files, or their semantic projections — including through an
      adapter that disguises Kirra World data as an authoritative checker input.

Fence B is the harder one, because the dangerous edge is not a crate named
`kirra-world`. `CorridorSource` is a TRAIT in `kirra-core`, and the checker
receives `&dyn CorridorSource` — it never fetches a corridor. So

    impl CorridorSource for WorldModelCorridor

would pass any crate-name dependency scan while completely inverting the
dependency direction: the checker's admissibility region would become a
projection of accumulated belief. Check 4 exists for exactly that shape, and the
allowlist deliberately does not pre-authorize a Kirra World-derived corridor.

The fence is language-independent, because the deployed system is: the verifying
consumer is Python (`robot/kirra_motor_consumer.py`), the process graph is
systemd, and service endpoints are environment configuration. A Rust-only scan
would leave three quarters of the real topology unchecked.

Checks
------
  1  Rust safety dependency closure          (Fence B)
  2  Kirra World outbound dependencies       (Fence A)
  3  Forbidden type exposure in public APIs  (Fence B)
  4  Safety-authoritative trait impls        (Fence B)
  5  Python consumers, via AST              (both)
  6  systemd / shell / config topology       (both)
  7  Shared source-artifact allowlist        (Fence B)
  7b Shared source-artifact PAIRING          (Fence B)
  8  Reserved database + endpoint isolation  (both)

LIMITATIONS — read before trusting a green run
----------------------------------------------
This is static analysis of one repository. It CANNOT prove:

  * that deployed processes do not talk to each other over the network;
  * what a runtime-loaded plugin does;
  * what a dynamically generated ROS graph connects;
  * that operators have not edited systemd units or environment files on the
    device;
  * that an external database is not shared out of band;
  * that a shared artifact outside repository configuration is not a hidden
    coupling. Check 7b detects sharing only where BOTH consumers reference the
    artifact in tracked source. A map supplied purely at deployment time,
    through a path in an environment file this repository never names, is
    invisible to it — and no .osm map is committed here, so that is the normal
    case rather than an edge one.

It is a ratchet against the accidental and the convenient edge — the same
honesty `ci/check_mick_actuation_fence.py` states about itself — not a proof of
isolation, and emphatically not a certification of anything. Runtime topology
verification on a real deployment remains required.

Usage
-----
    python3 ci/check_kirra_world_bidirectional_fence.py
    python3 ci/check_kirra_world_bidirectional_fence.py --root /path/to/tree
    python3 ci/check_kirra_world_bidirectional_fence.py --verbose
"""

from __future__ import annotations

import argparse
import ast
import re
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
ALLOWLIST_NAME = "ci/kirra_world_fence_allowlist.toml"

EXIT_OK = 0
EXIT_BREACH = 1
EXIT_USAGE = 2


# ===========================================================================
# CONFIGURATION — policy as data. Every root is listed with why it is a root,
# because an unexplained root is one nobody will dare remove or dare trust.
# ===========================================================================

# Fence B roots: the safety decision and authorization path. The checker walks
# the TRANSITIVE closure of these, so this list names entry points, not the
# whole protected set (the measured closure is ~19 workspace crates).
SAFETY_ROOTS: dict[str, str] = {
    "kirra-core": "the checker's shared types + the CorridorSource trait itself",
    "kirra-trajectory": "validate_trajectory_slow — the safety authority",
    "kirra-release-token": "the Ed25519 authorization seam (ADR-0033)",
    "kirra-ros2-adapter": "the slow/fast checker loops + actuator topics",
    "kirra-inline-governor": "the EP-01 in-line SHM enforcement path",
    "kirra-safety-authority": "the gray/black DAG posture traversal",
    "kirra-actuation-consumer": "the verifying motor consumer (serial chokepoint)",
    "kirra-verifier": "the root verifier service (governor control plane)",
    "kirra-policy-types": "command classification consumed by the posture gate",
    "kirra-persistence": "the store behind posture + attestation decisions",
}

# Reserved Kirra World package names. The prefix rule matters: a future adapter
# must not be able to evade the fence by being called `kirra-world-adapter` or
# `kirra-world-corridor`.
WORLD_PACKAGE_EXACT = {"kirra-world", "kirra-world-store", "kirra-world-service"}
WORLD_PACKAGE_PREFIX = "kirra-world"

# Fence A: what Kirra World must never depend on. Derived from the workspace —
# these are the crates that actuate or authorize.
ACTUATION_PACKAGES: dict[str, str] = {
    "kirra-release-token": "mints/verifies the actuator release token",
    "kirra-actuation-consumer": "the verifying motor consumer (serial seam)",
    "kirra-consumer-ffi": "the C-ABI motor consumer (drives the wheels)",
    "kirra-ros2-adapter": "publishes actuator topics",
    "kirra-inline-governor": "the in-line enforcement path",
    "kirra-hv-carrier": "the hypervisor command carrier",
    "kirra-r2cp": "the R2 control protocol transport",
}

# External crates that would hand Kirra World a transport to an actuator.
ACTUATION_EXTERNAL = {
    "r2r", "rclrs", "rosrust", "ros2-client",
    "rustdds", "cyclonedds-rs", "cyclonedds-sys", "iceoryx2",
    "serialport", "tokio-serial", "serial", "serial2",
    "socketcan", "rppal", "gpio-cdev", "linux-embedded-hal",
}

# Traits whose implementations carry safety authority. An implementation feeds
# the checker's decision, so the fence is on the IMPL, not the crate name.
SAFETY_AUTHORITATIVE_TRAITS = {
    "CorridorSource": "supplies the admissibility region to validate_trajectory_slow",
}

# Type/impl name fragments that mark a Kirra World adapter. Name heuristics are
# the WEAKEST signal here and are used only in addition to structural proof
# (imports and dependency edges) — never as the sole basis for a pass.
WORLD_ADAPTER_MARKERS = {"worldmodel", "kirraworld", "semanticworld", "worldcorridor"}

# Reserved runtime names. These are FENCE PATTERNS, not implemented behaviour:
# nothing reads them today and this file does not create them. Exact strings, on
# purpose — `robot/world_model.py` already ships a Rabbit narration projection
# gated by KIRRA_WORLD_MODEL_ENABLED, and a prefix rule would flag it forever.
RESERVED_ENV = {"KIRRA_WORLD_URL", "KIRRA_WORLD_DB", "KIRRA_WORLD_ENDPOINT", "KIRRA_WORLD_DB_PATH"}
RESERVED_UNITS = {"kirra-world.service", "kirra-world-store.service", "kirra-world-service.service"}
RESERVED_PATHS = {"/var/lib/kirra/world/", "/var/lib/kirra/world"}
RESERVED_PY_MODULES = {"kirra_world", "kirra_world_client", "kirra_world_store"}

# Python modules on the actuation/authorization path. Fence B applies to these:
# none may import, call, or read Kirra World.
SAFETY_PYTHON: dict[str, str] = {
    "robot/kirra_motor_consumer.py": "the verifying consumer — the ADR-0033 chokepoint",
    "robot/kirra_ffi.py": "the ctypes bridge to the release-token verifier",
    "robot/r2_drive.py": "the Ackermann last hop to the wheels",
    "robot/motor_authority.py": "motor authority arbitration",
    "robot/serial_exclusivity.py": "the boot sentinel owning the motor port",
    "robot/consumer_config_contract.py": "the consumer's fail-closed config contract",
}

# Python markers for actuation, used for Fence A against future Kirra World
# Python. Matched on AST nodes, not raw text.
PY_ACTUATION_IMPORTS = {"kirra_motor_consumer", "kirra_ffi", "r2_drive", "motor_authority"}
PY_ACTUATION_ROS = {"geometry_msgs", "ackermann_msgs", "geometry_msgs.msg"}
PY_ACTUATION_CALLS = {"set_car_motion", "write_twist", "issue_ros_release", "verify_release_token"}
PY_ACTUATION_TOPICS = {"cmd_vel", "/cmd_vel"}
PY_ACTUATION_DEVICES = {"/dev/myserial", "/dev/ttyUSB0", "/dev/ttyTHS1"}

# Shared source-artifact classes (ADR-0042 Decision 2).
#
# ADR-0042 permits Kirra World and the safety path to read the SAME underlying
# source artifact, provided each validates it independently and no DERIVED
# semantic projection crosses between them. That permission is not
# self-executing: something has to notice when the sharing actually starts.
#
# Each class names the tokens that identify a consumer of the artifact. When a
# `kirra-world*` package and a safety-closure package both reference the same
# class and no allowlist entry records the relationship, the fence fails --
# which is what turns "permitted, with an allowlist entry" from a sentence in a
# document into a condition.
#
# Detection is by source token, so it finds a consumer, not a file path. That is
# a real limit and it is stated in LIMITATIONS: an artifact shared purely at
# deployment time, through a path in an environment file that nothing in this
# repository names, is invisible here.
SHARED_ARTIFACT_CLASSES: dict[str, dict[str, object]] = {
    "lanelet2-osm-map": {
        "tokens": ("lanelet2", "Lanelet2", ".osm"),
        "what": "the Lanelet2 OSM lane-geometry map",
        "why_it_matters": (
            "the safety corridor is derived from it, so a Kirra World consumer of the same "
            "artifact is the single most likely route to an accidental Fence B breach "
            "(ADR-0042 Decision 2, the semantic-map vs safety-corridor boundary)"
        ),
    },
}

# Service-role inventory. A role decides which fence applies to a unit; an
# unclassified unit is reported rather than assumed safe.
SERVICE_ROLES: dict[str, str] = {
    "robot/install/systemd/kirra-consumer.service": "safety",
    "robot/install/systemd/kirra-ros-stack.service": "safety",
    "robot/install/systemd/kirra-lidar.service": "safety",
    "robot/install/systemd/kirra-doctor.service": "product",
    "robot/install/systemd/kirra-doctor.timer": "product",
    "robot/install/systemd/kirra-ota-check.service": "product",
    "robot/install/systemd/kirra-ota-check.timer": "product",
    "robot/install/systemd/kirra-rabbit-voice.service": "product",
    "robot/install/systemd/kirra-rabbit-watch.service": "product",
    "robot/install/systemd/kirra-rabbit-greet.service": "product",
    "robot/install/systemd/user/rabbit-voice.service": "product",
    "robot/install/systemd/user/mick-voice-router.service": "product",
    "robot/install/kirra-jetson-perf.service": "product",
    "deploy/systemd/kirra-verifier.service": "safety",
    "deploy/systemd/kirra-planner.service": "product",
    "deploy/systemd/kirra-taj.service": "product",
    "deploy/systemd/kirra-mick.service": "product",
    "deploy/systemd/kirra-mick-chat.service": "product",
}

# Directories scanned for units and launch scripts.
TOPOLOGY_DIRS = ["robot/install", "deploy", "robot"]
TOPOLOGY_SUFFIXES = {".service", ".timer", ".socket", ".sh", ".env", ".example"}

# Paths never scanned: this checker, its tests, its allowlist, and the ADRs that
# describe the fence. They necessarily contain the forbidden strings.
SELF_EXEMPT = {
    "ci/check_kirra_world_bidirectional_fence.py",
    "ci/test_kirra_world_bidirectional_fence.py",
    ALLOWLIST_NAME,
}
SELF_EXEMPT_PREFIXES = ("docs/",)


# ===========================================================================
# Results
# ===========================================================================


@dataclass(frozen=True)
class Violation:
    fence: str  # "A" or "B"
    check: str
    location: str
    detail: str
    adr: str
    repair: str

    def render(self) -> str:
        return (
            f"  [Fence {self.fence}] {self.check}\n"
            f"      where : {self.location}\n"
            f"      what  : {self.detail}\n"
            f"      ADR   : {self.adr}\n"
            f"      fix   : {self.repair}\n"
        )


@dataclass
class Report:
    violations: list[Violation] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)

    def add(self, **kw: str) -> None:
        self.violations.append(Violation(**kw))

    def note(self, msg: str) -> None:
        self.notes.append(msg)


# ===========================================================================
# Cargo manifest graph
#
# Implemented over tomllib rather than `cargo metadata` so it runs with no
# toolchain, no network, and no lockfile -- which is what lets the test suite
# build throwaway fixture workspaces. `cargo metadata` is the authority on a
# real build; --cargo-metadata cross-checks against it when available.
# ===========================================================================


def find_manifests(root: Path) -> dict[str, tuple[Path, dict]]:
    """Map package name -> (crate dir, parsed Cargo.toml) for every manifest."""
    out: dict[str, tuple[Path, dict]] = {}
    for manifest in root.rglob("Cargo.toml"):
        if "target" in manifest.parts or ".git" in manifest.parts:
            continue
        try:
            data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        except (tomllib.TOMLDecodeError, OSError):
            continue
        name = data.get("package", {}).get("name")
        if name:
            out[name] = (manifest.parent, data)
    return out


def direct_deps(data: dict, include_dev: bool) -> set[str]:
    """Normal + build (+ optionally dev) dependency names for one manifest.

    Transitive dev-dependencies are excluded on purpose: cargo never links a
    dependency's dev-deps into the dependent's build, so counting them would
    manufacture edges that do not exist in any shipped artifact.
    """
    names: set[str] = set()
    sections = ["dependencies", "build-dependencies"]
    if include_dev:
        sections.append("dev-dependencies")
    for section in sections:
        names |= set(data.get(section, {}).keys())
    for target in data.get("target", {}).values():
        for section in sections:
            names |= set(target.get(section, {}).keys())
    return names


def closure(roots: list[str], manifests: dict[str, tuple[Path, dict]]) -> dict[str, list[str]]:
    """Transitive closure over workspace packages. Returns pkg -> path from a root.

    NORMAL + BUILD dependencies only -- exactly what cargo links into a shipped
    artifact. Dev-dependencies are deliberately excluded, and the reason is
    load-bearing rather than a convenience:

    the safety roots dev-depend on the DOER crates (kirra-planner, kirra-taj,
    kirra-sidecars, kirra-mick, parko-kirra) for their test harnesses. Those
    crates are the ones that SHOULD one day depend on Kirra World -- Occy
    consuming semantic knowledge to generate a proposal is the intended design,
    not a breach. Counting dev edges would drag them inside the safety closure
    and make the fence fire on the architecture working as specified.

    A direct dev-dependency from a safety ROOT onto a kirra-world* package is
    still forbidden, and is checked separately (check_1) -- that is a deliberate
    edge, not an inherited one.
    """
    paths: dict[str, list[str]] = {}
    stack: list[tuple[str, list[str]]] = [(r, [r]) for r in roots if r in manifests]
    while stack:
        pkg, path = stack.pop()
        if pkg in paths and len(paths[pkg]) <= len(path):
            continue
        paths[pkg] = path
        entry = manifests.get(pkg)
        if entry is None:
            continue
        for dep in sorted(direct_deps(entry[1], include_dev=False)):
            if dep in manifests and dep not in paths:
                stack.append((dep, path + [dep]))
    return paths


def crate_dirs(manifests: dict[str, tuple[Path, dict]]) -> set[Path]:
    """Every package's manifest directory, for ownership resolution."""
    return {d for d, _ in manifests.values()}


def is_world_package(name: str) -> bool:
    return name in WORLD_PACKAGE_EXACT or name.startswith(WORLD_PACKAGE_PREFIX)


# ===========================================================================
# Rust source helpers
# ===========================================================================

_LINE_COMMENT = re.compile(r"//.*?$", re.MULTILINE)
_BLOCK_COMMENT = re.compile(r"/\*.*?\*/", re.DOTALL)
_STRING_LIT = re.compile(r'"(?:[^"\\]|\\.)*"')


def strip_rust(src: str, drop_strings: bool = True) -> str:
    """Remove comments (and optionally string literals) from Rust source.

    Documentation and prose must never trip this fence -- an ADR quoted in a doc
    comment is a description of the rule, not a breach of it.
    """
    out = _BLOCK_COMMENT.sub(" ", src)
    out = _LINE_COMMENT.sub(" ", out)
    if drop_strings:
        out = _STRING_LIT.sub('""', out)
    return out


def rust_sources(crate_dir: Path, all_crate_dirs: set[Path] | None = None) -> list[Path]:
    """Rust sources belonging to THIS package.

    The `all_crate_dirs` exclusion is load-bearing, not tidiness: the workspace
    ROOT package (`kirra-verifier`) has the repository root as its manifest
    directory, so a naive rglob sweeps every other crate's sources into it. That
    both double-counts findings and attributes another crate's file to the root
    package, which would make a violation message point at the wrong owner.
    """
    out: list[Path] = []
    for p in crate_dir.rglob("*.rs"):
        if "target" in p.parts or ".git" in p.parts:
            continue
        if all_crate_dirs:
            # Nearest enclosing manifest directory wins.
            owner = max(
                (d for d in all_crate_dirs if d == p.parent or d in p.parents),
                key=lambda d: len(d.parts),
                default=None,
            )
            if owner is not None and owner != crate_dir:
                continue
        out.append(p)
    return out


def is_test_scope(src: str, index: int) -> bool:
    """True if `index` sits inside a `#[cfg(test)]` module.

    Brace-counted from each cfg(test) marker. Crude but adequate: the cost of a
    wrong answer is requiring an allowlist entry for a test impl, not missing a
    production one.
    """
    for m in re.finditer(r"#\[cfg\(test\)\]", src):
        brace = src.find("{", m.end())
        if brace == -1 or brace > index:
            continue
        depth, i = 0, brace
        while i < len(src):
            if src[i] == "{":
                depth += 1
            elif src[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if m.start() < index < i:
            return True
    return False


# ===========================================================================
# Check 1 — Rust safety dependency closure (Fence B)
# ===========================================================================


def check_1_safety_closure(root: Path, manifests: dict, rep: Report) -> dict[str, list[str]]:
    present = [r for r in SAFETY_ROOTS if r in manifests]
    for missing in sorted(set(SAFETY_ROOTS) - set(present)):
        # A configured root that vanished must not silently shrink the fence.
        rep.note(f"configured safety root not found in workspace: {missing}")

    paths = closure(present, manifests)
    for pkg, path in sorted(paths.items()):
        if is_world_package(pkg):
            rep.add(
                fence="B",
                check="Check 1 — safety dependency closure",
                location=f"package {pkg}",
                detail="reached from the safety path via " + " -> ".join(path),
                adr="ADR-0039 (Fence B), ADR-0042 §transitive closure",
                repair=(
                    "Remove the dependency edge. The safety path must read its OWN "
                    "authoritative inputs; if it needs knowledge Kirra World holds, "
                    "that is a superseding-ADR conversation, not a dependency."
                ),
            )

    # A safety root's own DEV-dependency on Kirra World is forbidden too. It
    # does not ship, but it is a deliberate edge and the usual first step toward
    # a real one ("the test already uses it"). Only DIRECT edges: a root
    # dev-depending on a doer crate that legitimately uses Kirra World is fine.
    for r in present:
        for dep in sorted(direct_deps(manifests[r][1], include_dev=True)):
            if is_world_package(dep) and dep not in paths:
                rep.add(
                    fence="B",
                    check="Check 1 — safety dependency closure (dev edge)",
                    location=f"package {r}",
                    detail=f"dev-depends directly on Kirra World package `{dep}`",
                    adr="ADR-0039 (Fence B)",
                    repair=(
                        "Build the test fixture from the safety path's own types. A dev edge "
                        "does not ship, but it is how a normal edge gets argued for later."
                    ),
                )
    return paths


# ===========================================================================
# Check 2 — Kirra World outbound dependency fence (Fence A)
# ===========================================================================


def check_2_world_outbound(root: Path, manifests: dict, rep: Report) -> int:
    world = sorted(n for n in manifests if is_world_package(n))
    if not world:
        rep.note("Kirra World packages not present yet — reserved fence validated.")
        return 0

    for pkg in world:
        paths = closure([pkg], manifests)
        for dep, path in sorted(paths.items()):
            if dep == pkg:
                continue
            if dep in ACTUATION_PACKAGES:
                rep.add(
                    fence="A",
                    check="Check 2 — Kirra World outbound dependencies",
                    location=f"package {pkg}",
                    detail=f"reaches actuation crate {dep} ({ACTUATION_PACKAGES[dep]}) via "
                    + " -> ".join(path),
                    adr="ADR-0039 (Fence A)",
                    repair="Kirra World publishes typed knowledge only. Drop the edge.",
                )
        entry = manifests[pkg]
        for ext in sorted(direct_deps(entry[1], include_dev=True) & ACTUATION_EXTERNAL):
            rep.add(
                fence="A",
                check="Check 2 — Kirra World outbound dependencies",
                location=f"package {pkg}",
                detail=f"depends on actuation transport crate `{ext}`",
                adr="ADR-0039 (Fence A)",
                repair="Remove the transport dependency; knowledge does not need a bus to an actuator.",
            )
    return len(world)


# ===========================================================================
# Check 3 — forbidden type exposure (Fence B)
# ===========================================================================

_PUB_ITEM = re.compile(
    r"\bpub(?:\s*\([^)]*\))?\s+(?:fn|struct|enum|trait|type|const|static|use)\b[^;{]*",
    re.DOTALL,
)


def check_3_type_exposure(root: Path, manifests: dict, closure_pkgs: dict, rep: Report) -> None:
    """Fail if a safety-path crate names a Kirra World type in a public item.

    Token-level, over comment- and string-stripped source. Honest limitation: a
    type reached through an alias defined elsewhere, or through a generic
    parameter bound in another file, is not resolved -- that needs real name
    resolution (rustdoc JSON or a syn pass), which is why Check 1 carries the
    load-bearing proof and this check is corroboration.
    """
    for pkg in sorted(closure_pkgs):
        entry = manifests.get(pkg)
        if entry is None:
            continue
        for path in rust_sources(entry[0], crate_dirs(manifests)):
            rel = str(path.relative_to(root))
            if _exempt(rel):
                continue
            try:
                raw = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            src = strip_rust(raw)
            for m in _PUB_ITEM.finditer(src):
                text = m.group(0)
                if re.search(r"\bkirra_world[a-z_]*\s*::", text):
                    line = src.count("\n", 0, m.start()) + 1
                    rep.add(
                        fence="B",
                        check="Check 3 — forbidden type exposure",
                        location=f"{rel}:{line}",
                        detail="public API of safety-path crate "
                        f"`{pkg}` names a Kirra World type: {_squash(text)}",
                        adr="ADR-0039 (Fence B)",
                        repair=(
                            "Take the value the checker actually needs as a plain type it "
                            "owns. A Kirra World type in a safety signature makes the "
                            "safety path depend on semantic belief."
                        ),
                    )


def _squash(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()[:120]


def _exempt(rel: str) -> bool:
    rel = rel.replace("\\", "/")
    return rel in SELF_EXEMPT or rel.startswith(SELF_EXEMPT_PREFIXES)


# ===========================================================================
# Check 4 — safety-authoritative trait implementations (Fence B)
# ===========================================================================

_IMPL = re.compile(
    r"^(?P<indent>[ \t]*)impl(?:\s*<[^>]*>)?\s+(?P<trait>[A-Za-z_][A-Za-z0-9_]*)\s+for\s+"
    r"(?P<type>[A-Za-z_][A-Za-z0-9_:<>, ]*?)(?:\s*<[^>]*>)?\s*(?:where\b|\{)",
    re.MULTILINE,
)


def check_4_trait_impls(root: Path, manifests: dict, allow: dict, rep: Report) -> list[dict]:
    dirs = crate_dirs(manifests)
    approved = {
        (e["trait"], e["type"], e["file"].replace("\\", "/"))
        for e in allow.get("safety_authoritative_impl", [])
    }
    found: list[dict] = []

    for pkg, (crate_dir, data) in sorted(manifests.items()):
        pkg_deps = closure([pkg], manifests)
        pkg_reaches_world = any(is_world_package(d) for d in pkg_deps)
        for path in rust_sources(crate_dir, dirs):
            rel = str(path.relative_to(root)).replace("\\", "/")
            if _exempt(rel):
                continue
            try:
                raw = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            src = strip_rust(raw, drop_strings=False)
            code = strip_rust(raw, drop_strings=True)
            for m in _IMPL.finditer(src):
                trait = m.group("trait")
                if trait not in SAFETY_AUTHORITATIVE_TRAITS:
                    continue
                ty = m.group("type").strip()
                line = src.count("\n", 0, m.start()) + 1
                in_test = "/tests/" in rel or rel.endswith("_test.rs") or is_test_scope(src, m.start())
                rec = {"trait": trait, "type": ty, "file": rel, "line": line, "test": in_test, "pkg": pkg}
                found.append(rec)

                # --- structural signals (preferred over name heuristics) ---
                imports_world = bool(re.search(r"\buse\s+kirra_world[a-z_]*\b", code)) or bool(
                    re.search(r"\bkirra_world[a-z_]*\s*::", code)
                )
                reads_world_db = any(p in raw for p in RESERVED_PATHS)
                queries_world = any(e in raw for e in RESERVED_ENV)
                name_marked = any(mk in ty.lower().replace("_", "") for mk in WORLD_ADAPTER_MARKERS)

                why = []
                if imports_world:
                    why.append("the file imports a kirra-world* package")
                if pkg_reaches_world:
                    why.append(f"the implementing crate `{pkg}` depends on a kirra-world* package")
                if reads_world_db:
                    why.append("the file references the reserved Kirra World database path")
                if queries_world:
                    why.append("the file references a reserved Kirra World endpoint variable")
                if name_marked:
                    why.append(f"the implementing type `{ty}` carries a Kirra World adapter marker")

                if why:
                    rep.add(
                        fence="B",
                        check=f"Check 4 — safety-authoritative trait impl ({trait})",
                        location=f"{rel}:{line}",
                        detail=f"`impl {trait} for {ty}` is Kirra World-derived: " + "; ".join(why),
                        adr="ADR-0039 (Fence B), ADR-0042 §hidden-adapter prohibition",
                        repair=(
                            f"{trait} carries safety authority: whatever it returns is treated as "
                            "authoritative by the checker. It must be derived from the safety "
                            "path's own inputs, never from accumulated semantic belief. This "
                            "route requires a superseding ADR, not an allowlist entry."
                        ),
                    )
                    continue

                # --- fail closed on anything unreviewed (production scope) ---
                if not in_test and (trait, ty, rel) not in approved:
                    rep.add(
                        fence="B",
                        check=f"Check 4 — safety-authoritative trait impl ({trait})",
                        location=f"{rel}:{line}",
                        detail=(
                            f"`impl {trait} for {ty}` is not in {ALLOWLIST_NAME}. No structural "
                            "Kirra World link was found, but this trait grants safety authority, "
                            "so a new implementation is reviewed before it is trusted."
                        ),
                        adr="ADR-0039 (Fence B)",
                        repair=(
                            f"If the corridor is derived from the safety path's own inputs, add an "
                            f"entry to {ALLOWLIST_NAME} with a rationale and owning ADR. If it is "
                            "derived from Kirra World, it is prohibited."
                        ),
                    )
    return found


# ===========================================================================
# Check 5 — Python fence (both directions), via AST
# ===========================================================================


def _py_imports(tree: ast.AST) -> set[str]:
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names |= {a.name for a in node.names}
        elif isinstance(node, ast.ImportFrom) and node.module:
            names.add(node.module)
    return names


def _py_call_names(tree: ast.AST) -> set[str]:
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            f = node.func
            if isinstance(f, ast.Name):
                names.add(f.id)
            elif isinstance(f, ast.Attribute):
                names.add(f.attr)
    return names


def _py_string_constants(tree: ast.AST) -> set[str]:
    """String constants only -- comments and docstrings are excluded.

    Docstrings ARE Constant nodes, so they are dropped explicitly: a module that
    documents the fence must not breach it.
    """
    docstrings: set[int] = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            body = getattr(node, "body", [])
            if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant):
                docstrings.add(id(body[0].value))
    out: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and id(node) not in docstrings:
            out.add(node.value)
    return out


def _parse_py(path: Path) -> ast.AST | None:
    try:
        return ast.parse(path.read_text(encoding="utf-8", errors="replace"), filename=str(path))
    except (OSError, SyntaxError):
        return None


def _is_world_py(name: str) -> bool:
    base = name.split(".")[0]
    return base in RESERVED_PY_MODULES or base.startswith("kirra_world")


def check_5_python(root: Path, rep: Report) -> None:
    # --- Fence B: safety-path Python must not reach Kirra World ---
    for rel, why in sorted(SAFETY_PYTHON.items()):
        path = root / rel
        if not path.exists():
            rep.note(f"configured safety Python module not found: {rel}")
            continue
        tree = _parse_py(path)
        if tree is None:
            rep.note(f"could not parse {rel} — NOT checked")
            continue
        for imp in sorted(_py_imports(tree)):
            if _is_world_py(imp):
                rep.add(
                    fence="B",
                    check="Check 5 — Python safety consumer",
                    location=rel,
                    detail=f"imports Kirra World module `{imp}` ({why})",
                    adr="ADR-0039 (Fence B), ADR-0042 §language independence",
                    repair="The verifying consumer reads the release token and its own inputs only.",
                )
        for const in sorted(_py_string_constants(tree)):
            hit = next((r for r in (RESERVED_ENV | RESERVED_PATHS) if r in const), None)
            if hit:
                rep.add(
                    fence="B",
                    check="Check 5 — Python safety consumer",
                    location=rel,
                    detail=f"references reserved Kirra World resource `{hit}` ({why})",
                    adr="ADR-0039 (Fence B)",
                    repair="A safety consumer must not read a Kirra World endpoint or database.",
                )

    # --- Fence A: Kirra World Python must not reach actuation ---
    for path in sorted(root.rglob("*.py")):
        rel = str(path.relative_to(root)).replace("\\", "/")
        if _exempt(rel):
            continue
        stem = path.stem
        if not (stem in RESERVED_PY_MODULES or stem.startswith("kirra_world")):
            continue
        tree = _parse_py(path)
        if tree is None:
            continue
        imports, calls, consts = _py_imports(tree), _py_call_names(tree), _py_string_constants(tree)
        for imp in sorted(imports):
            base = imp.split(".")[0]
            if base in PY_ACTUATION_IMPORTS or base in PY_ACTUATION_ROS or imp in PY_ACTUATION_ROS:
                rep.add(
                    fence="A",
                    check="Check 5 — Kirra World Python",
                    location=rel,
                    detail=f"imports actuation module `{imp}`",
                    adr="ADR-0039 (Fence A)",
                    repair="Kirra World publishes typed knowledge; it never drives.",
                )
        for call in sorted(calls & PY_ACTUATION_CALLS):
            rep.add(
                fence="A",
                check="Check 5 — Kirra World Python",
                location=rel,
                detail=f"calls actuation/authorization function `{call}()`",
                adr="ADR-0039 (Fence A)",
                repair="Remove the call. Knowledge cannot mint or consume a release token.",
            )
        for const in sorted(consts):
            if const in PY_ACTUATION_TOPICS or const in PY_ACTUATION_DEVICES:
                rep.add(
                    fence="A",
                    check="Check 5 — Kirra World Python",
                    location=rel,
                    detail=f"references actuation endpoint `{const}`",
                    adr="ADR-0039 (Fence A)",
                    repair="Kirra World must not publish cmd_vel or open an actuator device.",
                )


# ===========================================================================
# Check 6 — systemd / shell / configuration topology
# ===========================================================================

_ENV_LINE = re.compile(r"^\s*(?:Environment|EnvironmentFile)\s*=\s*(.+)$", re.MULTILINE)
_EXEC_LINE = re.compile(r"^\s*(?:ExecStart|ExecStartPre|ExecStop)\s*=\s*(.+)$", re.MULTILINE)
_DEP_LINE = re.compile(r"^\s*(?:Requires|BindsTo|Wants|PartOf)\s*=\s*(.+)$", re.MULTILINE)


def topology_files(root: Path) -> list[Path]:
    seen: list[Path] = []
    for d in TOPOLOGY_DIRS:
        base = root / d
        if not base.exists():
            continue
        for p in sorted(base.rglob("*")):
            if p.is_file() and (p.suffix in TOPOLOGY_SUFFIXES or p.name.endswith(".env.example")):
                seen.append(p)
    return seen


def check_6_topology(root: Path, rep: Report) -> None:
    for path in topology_files(root):
        rel = str(path.relative_to(root)).replace("\\", "/")
        if _exempt(rel):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        role = SERVICE_ROLES.get(rel)
        is_world_unit = "kirra-world" in path.name

        # Strip shell/systemd comments so a documented rule is not a breach.
        code = "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))

        # Fence B: a safety unit must not be wired to Kirra World.
        if role == "safety":
            for pat in sorted(RESERVED_ENV | RESERVED_PATHS | RESERVED_UNITS):
                if pat in code:
                    rep.add(
                        fence="B",
                        check="Check 6 — service topology",
                        location=rel,
                        detail=f"safety unit references reserved Kirra World resource `{pat}`",
                        adr="ADR-0039 (Fence B)",
                        repair="A safety service must not receive a Kirra World endpoint, database, or unit dependency.",
                    )
        elif role is None and path.suffix in {".service", ".timer", ".socket"}:
            rep.note(f"unit not in the service-role inventory (unclassified): {rel}")

        # Fence A: a Kirra World unit must not be wired to actuation.
        if is_world_unit:
            targets = " ".join(_EXEC_LINE.findall(code) + _DEP_LINE.findall(code) + _ENV_LINE.findall(code))
            for marker in sorted(PY_ACTUATION_IMPORTS | PY_ACTUATION_DEVICES | {"kirra-consumer.service"}):
                if marker in targets:
                    rep.add(
                        fence="A",
                        check="Check 6 — service topology",
                        location=rel,
                        detail=f"Kirra World unit is wired to actuation via `{marker}`",
                        adr="ADR-0039 (Fence A)",
                        repair="Remove the wiring. Knowledge services do not start, require, or exec actuation.",
                    )
            for topic in sorted(PY_ACTUATION_TOPICS):
                if topic in targets:
                    rep.add(
                        fence="A",
                        check="Check 6 — service topology",
                        location=rel,
                        detail=f"Kirra World unit publishes `{topic}`",
                        adr="ADR-0039 (Fence A)",
                        repair="Remove the publisher.",
                    )


# ===========================================================================
# Check 7 — shared source-artifact allowlist
# ===========================================================================

_REQUIRED_IMPL_KEYS = {"trait", "type", "file", "rationale", "adr", "owner"}
_REQUIRED_ARTIFACT_KEYS = {
    "artifact", "world_consumer", "safety_consumer",
    "independent_validation", "adr", "owner", "reason",
}


def load_allowlist(root: Path, rep: Report) -> dict:
    path = root / ALLOWLIST_NAME
    if not path.exists():
        rep.add(
            fence="B",
            check="Check 7 — allowlist",
            location=ALLOWLIST_NAME,
            detail="allowlist file is missing",
            adr="ADR-0039, ADR-0042",
            repair="Restore it. Without the allowlist the fence cannot distinguish reviewed from unreviewed.",
        )
        return {}
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as exc:
        rep.add(
            fence="B",
            check="Check 7 — allowlist",
            location=ALLOWLIST_NAME,
            detail=f"allowlist does not parse: {exc}",
            adr="ADR-0039",
            repair="Fix the TOML syntax.",
        )
        return {}


def artifact_consumers(root: Path, manifests: dict, tokens: tuple) -> set[str]:
    """Workspace packages whose CODE references any of `tokens`.

    Comments are stripped first, and that materially changes the answer: before
    stripping, `kirra-core` and `kirra-trajectory` both looked like Lanelet2
    consumers, when in fact each only mentions Lanelet2 in prose explaining that
    the cxx bridge lives in the ADAPTER and deliberately not in them. Counting
    those would have put two crates on the shared-artifact baseline that read
    nothing, and a baseline nobody trusts is one nobody checks.

    String literals are kept -- a map path or filename is a literal, and that is
    exactly the kind of consumption worth catching.
    """
    dirs = crate_dirs(manifests)
    found: set[str] = set()
    for pkg, (crate_dir, _data) in manifests.items():
        for path in rust_sources(crate_dir, dirs):
            rel = str(path.relative_to(root)).replace("\\", "/")
            if _exempt(rel):
                continue
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            if any(tok in strip_rust(text, drop_strings=False) for tok in tokens):
                found.add(pkg)
                break
    return found


def check_7b_shared_artifacts(
    root: Path, manifests: dict, closure_pkgs: dict, allow: dict, rep: Report
) -> list[str]:
    """Require an allowlist entry wherever Kirra World and safety share an artifact.

    Reports the safety-side consumers of every configured class even when no
    Kirra World consumer exists, so the baseline is on the record BEFORE the
    sharing can happen -- a reviewer judging a future allowlist entry needs to
    know what the safety path was already reading independently.
    """
    recorded = {
        (e.get("artifact"), e.get("world_consumer"), e.get("safety_consumer"))
        for e in allow.get("shared_source_artifact", [])
    }
    notes: list[str] = []

    for name, spec in sorted(SHARED_ARTIFACT_CLASSES.items()):
        tokens = tuple(spec["tokens"])  # type: ignore[arg-type]
        consumers = artifact_consumers(root, manifests, tokens)
        safety_side = sorted(c for c in consumers if c in closure_pkgs)
        world_side = sorted(c for c in consumers if is_world_package(c))
        other_side = sorted(c for c in consumers if c not in closure_pkgs and not is_world_package(c))

        notes.append(
            f"shared artifact `{name}`: safety-side consumers {safety_side or '[]'}; "
            f"Kirra World consumers {world_side or '[]'}; other (doer/product) {other_side or '[]'}"
        )

        for w in world_side:
            for s in safety_side:
                if not any(
                    a == name and wc == w and sc == s for (a, wc, sc) in recorded
                ):
                    rep.add(
                        fence="B",
                        check="Check 7b — shared source artifact",
                        location=f"{w} + {s}",
                        detail=(
                            f"both consume `{name}` ({spec['what']}) with no allowlist entry. "
                            f"{spec['why_it_matters']}"
                        ),
                        adr="ADR-0042 Decision 2",
                        repair=(
                            f"If each validates the artifact INDEPENDENTLY and no derived semantic "
                            f"projection crosses between them, record it in {ALLOWLIST_NAME} as a "
                            f"[[shared_source_artifact]] with artifact=\"{name}\", "
                            f"world_consumer=\"{w}\", safety_consumer=\"{s}\", and the independence "
                            "argument. If a derived projection DOES cross, that is a Fence B breach "
                            "and needs a superseding ADR, not an allowlist entry."
                        ),
                    )
    return notes


def check_7_allowlist(root: Path, allow: dict, today: str, rep: Report) -> None:
    def validate(entries: list, kind: str, required: set[str], key_fields: tuple[str, ...]) -> None:
        seen: set[tuple] = set()
        for i, entry in enumerate(entries):
            where = f"{ALLOWLIST_NAME} [[{kind}]] #{i + 1}"
            missing = sorted(required - set(entry))
            for m in missing:
                rep.add(
                    fence="B",
                    check="Check 7 — allowlist",
                    location=where,
                    detail=f"entry is missing required field `{m}`",
                    adr="ADR-0039, ADR-0042",
                    repair=f"Add `{m}`. An exception without it records no decision anyone can audit.",
                )
            for f in ("rationale", "reason", "independent_validation"):
                if f in entry and f in required and not str(entry[f]).strip():
                    rep.add(
                        fence="B",
                        check="Check 7 — allowlist",
                        location=where,
                        detail=f"field `{f}` is empty",
                        adr="ADR-0039",
                        repair="State the actual argument, not a placeholder.",
                    )
            if not missing:
                key = tuple(str(entry[k]) for k in key_fields)
                if key in seen:
                    rep.add(
                        fence="B",
                        check="Check 7 — allowlist",
                        location=where,
                        detail=f"duplicate entry for {key}",
                        adr="ADR-0039",
                        repair="Remove the duplicate; two records of one decision will diverge.",
                    )
                seen.add(key)
            expires = entry.get("expires")
            if expires and str(expires) < today:
                rep.add(
                    fence="B",
                    check="Check 7 — allowlist",
                    location=where,
                    detail=f"entry expired on {expires} (today is {today})",
                    adr="ADR-0039",
                    repair="Re-review the exception and set a new date, or remove it.",
                )

    validate(
        allow.get("safety_authoritative_impl", []),
        "safety_authoritative_impl",
        _REQUIRED_IMPL_KEYS,
        ("trait", "type", "file"),
    )
    validate(
        allow.get("shared_source_artifact", []),
        "shared_source_artifact",
        _REQUIRED_ARTIFACT_KEYS,
        ("artifact", "world_consumer", "safety_consumer"),
    )


# ===========================================================================
# Check 8 — reserved database + endpoint isolation
# ===========================================================================


def check_8_reserved(root: Path, manifests: dict, closure_pkgs: dict, rep: Report) -> None:
    """Fail if safety-path Rust references a reserved Kirra World resource.

    Reserved names are fence patterns only -- nothing implements them. Exact
    strings, so the existing KIRRA_WORLD_MODEL_ENABLED (Rabbit's narration
    projection, outside the safety closure) is not swept up.
    """
    for pkg in sorted(closure_pkgs):
        entry = manifests.get(pkg)
        if entry is None:
            continue
        for path in rust_sources(entry[0], crate_dirs(manifests)):
            rel = str(path.relative_to(root)).replace("\\", "/")
            if _exempt(rel):
                continue
            try:
                raw = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            code = strip_rust(raw, drop_strings=False)  # keep strings: env names live there
            for pat in sorted(RESERVED_ENV | RESERVED_PATHS):
                if pat in code:
                    line = code.count("\n", 0, code.index(pat)) + 1
                    rep.add(
                        fence="B",
                        check="Check 8 — reserved resource isolation",
                        location=f"{rel}:{line}",
                        detail=f"safety-path crate `{pkg}` references reserved Kirra World resource `{pat}`",
                        adr="ADR-0039 (Fence B)",
                        repair="The safety path reads its own inputs. Remove the reference.",
                    )


# ===========================================================================
# Driver
# ===========================================================================


def run(root: Path, today: str) -> Report:
    rep = Report()
    manifests = find_manifests(root)
    allow = load_allowlist(root, rep)

    closure_pkgs = check_1_safety_closure(root, manifests, rep)
    n_world = check_2_world_outbound(root, manifests, rep)
    check_3_type_exposure(root, manifests, closure_pkgs, rep)
    check_4_trait_impls(root, manifests, allow, rep)
    check_5_python(root, rep)
    check_6_topology(root, rep)
    check_7_allowlist(root, allow, today, rep)
    for note in check_7b_shared_artifacts(root, manifests, closure_pkgs, allow, rep):
        rep.note(note)
    check_8_reserved(root, manifests, closure_pkgs, rep)

    rep.note(f"safety closure: {len(closure_pkgs)} workspace packages from {len(SAFETY_ROOTS)} roots")
    rep.note(f"kirra-world* packages present: {n_world}")
    return rep


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--root", default=str(REPO), help="repository root to check")
    ap.add_argument("--verbose", action="store_true", help="print notes and the measured closure")
    ap.add_argument("--today", default=None, help="override today's date (YYYY-MM-DD) for expiry checks")
    args = ap.parse_args(argv)

    root = Path(args.root).resolve()
    if not root.is_dir():
        print(f"not a directory: {root}", file=sys.stderr)
        return EXIT_USAGE

    today = args.today
    if today is None:
        import datetime

        today = datetime.date.today().isoformat()

    rep = run(root, today)

    if args.verbose:
        for n in rep.notes:
            print(f"  note: {n}")

    if rep.violations:
        print("\nKirra World bidirectional fence: BREACHED\n")
        for v in rep.violations:
            print(v.render())
        print(f"{len(rep.violations)} violation(s).\n")
        print(
            "Fence A: Kirra World cannot act.  Fence B: the safety path cannot depend\n"
            "on Kirra World. Neither is a style rule — they are the reason a semantic\n"
            "layer that may be wrong is allowed to exist at all."
        )
        return EXIT_BREACH

    print("Kirra World bidirectional fence: INTACT")
    if not args.verbose:
        for n in rep.notes:
            if n.startswith(("Kirra World packages not present", "safety closure")):
                print(f"  {n}")
    return EXIT_OK


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except SystemExit:
        raise
    except Exception as exc:  # pragma: no cover - defensive
        print(f"internal error: {exc}", file=sys.stderr)
        sys.exit(EXIT_USAGE)
