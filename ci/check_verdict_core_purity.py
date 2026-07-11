#!/usr/bin/env python3
"""Verdict-core purity fence — the structural "the verdict is computed before the
crypto, and the kernel never blocks" gate (ADR-0031 §crypto-separation, 5.3).

The safety verdict MUST be a pure, bounded, crypto-free computation. Signing
(Ed25519, ~91 µs sign+verify) rides the actuation path AFTER the verdict, never
inside the verdict WCET — that separation is what lets the FTTI budget decompose
as `verdict_WCET + actuation_latency < control_cycle` (ADR-0031). If a verdict-
core crate ever acquires a crypto dependency edge, that separation silently
collapses and the WCET argument is void. This gate makes the missing edge a CI
invariant, so "just sign it in the checker for convenience" is a build failure,
not a temptation.

Broadened (§7 item #1) beyond crypto to the other properties the kernel's timing
argument rests on — async, filesystem I/O, and (for the no_std kernel) heap
allocation — scoped HONESTLY to where each property is actually true:

  UNIT                     crypto  async  fs   alloc/std   how
  ---------------------------------------------------------------------------
  kirra-core               ✓       —      —    —           dep-closure + symbol
  kirra-trajectory         ✓       ✓      ✓    —           dep-closure + symbol
  kirra_judge.rs (QNX)     ✓       ✓      ✓    ✓           symbol (no cargo)

Scoping rationale (deliberate, not laziness):
  * `kirra-core` carries a LEGITIMATE optional `tokio` + `Vec` capture writer
    behind the non-default `capture` feature (crates/kirra-core/Cargo.toml). It
    is NOT an async-free or alloc-free kernel, so claiming so would be false. Its
    verdict-core content (the frozen kinematics talisman + SG2 containment) is
    crypto-free, and THAT is the load-bearing 5.3 invariant — so kirra-core is
    fenced against crypto only.
  * `kirra-trajectory` declares itself "ROS-free and async-free" in its own
    manifest header; the fence makes that promise a CI invariant (crypto + async
    + fs). It is a std crate that uses `Vec` legitimately, so alloc is NOT fenced.
  * `kirra_judge.rs` is the QNX partition judge: `#![no_std]`, `panic = abort`,
    zero-alloc, integer-only, built by rustc directly (no cargo, no Cargo.toml).
    It gets the full fence including alloc + std, because there it IS true.

Note on hashing: `sha2` / `sha3` / `hex` / `blake3` are NOT forbidden. They are
pure, bounded, keyless digest primitives (parko-core legitimately pulls `sha2`
for hashing in kirra-trajectory's closure). The 5.3 line is SIGNING — keyed
crypto that would fold key material and a signature op into the verdict — not
hashing. The forbidden set below is the ed25519 / release-token / MAC / TLS /
cipher family, exactly.

Mechanics mirror ci/check_mick_actuation_fence.py (pure-Python, tomllib, no
toolchain): a dependency-closure walk over workspace path deps + a comment-
stripped symbol scan. Heuristic honesty: both are textual; a determined evasion
beats them. The gate is a ratchet against the accidental/convenient edge, not a
proof. The end-to-end evidence remains the ADR-0031 separation test + the WCET
gate (src/wcet_gate.rs).

Run `--self-test` to prove non-vacuity: it feeds synthetic breaches through the
same check functions and asserts each dimension fires (and that clean inputs
pass). CI runs the self-test first, then the real fence.

Exit 0 = fence intact. Exit 1 = an impurity edge/token (or a gate self-check
failure — a fenced unit going missing must not silently void the gate).
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# ---------------------------------------------------------------------------
# Fenced units
# ---------------------------------------------------------------------------

# Cargo crates whose dependency CLOSURE must never reach a crypto (signing)
# crate. Both hold verdict-core logic: kirra-core the frozen kinematics talisman
# + SG2 containment, kirra-trajectory the trajectory checker (validate_* +
# check_command_conforms).
CRYPTO_DEP_FENCED_CRATES = [
    "crates/kirra-core",
    "crates/kirra-trajectory",
]

# The QNX partition judge — no Cargo.toml (built by rustc directly), so it is
# symbol-scanned only, not dep-walked.
QNX_JUDGE = "tools/qnx-rtm-harness/kirra_judge.rs"

# External / workspace crates that would fold SIGNING (keyed crypto) into a
# verdict-core closure. Hashing (sha2/sha3/hex/blake3) is deliberately absent —
# see the module docstring. Value = why it is forbidden.
FORBIDDEN_CRYPTO_DEPS = {
    # Ed25519 (the release-token / attestation signing primitive)
    "ed25519-dalek": "Ed25519 signing/verification primitive",
    "ed25519": "Ed25519 signature type",
    "curve25519-dalek": "Curve25519 scalar/point arithmetic (signing backend)",
    "kirra-release-token": "the ADR-0033 release-token mint/verify seam",
    # General signing / MAC / asymmetric
    "hmac": "keyed-hash message authentication (signing-adjacent)",
    "rsa": "RSA signing",
    "ecdsa": "ECDSA signing",
    "p256": "NIST P-256 signing",
    "k256": "secp256k1 signing",
    "ring": "aggregate crypto backend (signing + AEAD)",
    "aws-lc-rs": "aggregate crypto backend (signing + AEAD)",
    # TLS / AEAD (transport crypto has no place in a verdict core)
    "rustls": "TLS stack",
    "native-tls": "TLS stack",
    "openssl": "OpenSSL bindings",
    "aes-gcm": "AEAD cipher",
    "chacha20poly1305": "AEAD cipher",
}

# ---------------------------------------------------------------------------
# Symbol dimensions — (compiled-regex, human label). Matched in comment-stripped
# source. Each fenced unit applies a subset (see UNIT_SYMBOL_DIMENSIONS).
# ---------------------------------------------------------------------------

CRYPTO_SYMBOLS = [
    (re.compile(r"\bed25519\b"), "ed25519 signing"),
    (re.compile(r"\bSigningKey\b"), "SigningKey (Ed25519 secret)"),
    (re.compile(r"\bVerifyingKey\b"), "VerifyingKey (Ed25519 public)"),
    (re.compile(r"\bSignature\b"), "Signature type"),
    (re.compile(r"\bReleaseToken\b"), "ReleaseToken (release-token mint)"),
    (re.compile(r"\bissue_release_token\b"), "release-token mint call"),
    (re.compile(r"\brelease_token\b"), "release-token module"),
    (re.compile(r"\bHmac\b"), "HMAC (keyed MAC)"),
]

ASYNC_SYMBOLS = [
    (re.compile(r"\basync\s+fn\b"), "async fn (the kernel must be synchronous)"),
    (re.compile(r"\.await\b"), ".await (async suspension point)"),
    (re.compile(r"\btokio\b"), "tokio (async runtime)"),
    (re.compile(r"\basync_std\b"), "async-std (async runtime)"),
    (re.compile(r"\bsmol\b"), "smol (async runtime)"),
]

FS_SYMBOLS = [
    (re.compile(r"\bstd\s*::\s*fs\b"), "std::fs (filesystem I/O)"),
    (re.compile(r"\bstd\s*::\s*net\b"), "std::net (network I/O)"),
    (re.compile(r"\bOpenOptions\b"), "OpenOptions (file I/O)"),
    (re.compile(r"\bFile\s*::\s*(open|create)\b"), "File::open/create (file I/O)"),
]

# QNX judge only — the no_std zero-alloc kernel. `core::` is fine and never
# matches these (the `\b` before `std`/`alloc` fails inside `no_std`/`core`).
ALLOC_SYMBOLS = [
    (re.compile(r"\balloc\s*::"), "alloc:: (heap allocation)"),
    (re.compile(r"\bBox\s*::"), "Box:: (heap allocation)"),
    (re.compile(r"\bVec\s*[:<]"), "Vec (heap allocation)"),
    (re.compile(r"\bString\b"), "String (heap allocation)"),
    (re.compile(r"\bvec!"), "vec! (heap allocation)"),
    (re.compile(r"\bformat!"), "format! (heap allocation)"),
]

STD_SYMBOLS = [
    # The judge is `#![no_std]`; ANY `std::` reference is a breach (subsumes fs).
    (re.compile(r"\bstd\s*::"), "std:: (the judge is no_std)"),
]

# Per-unit symbol dimension assignment (see the table in the module docstring).
CRYPTO_ONLY = CRYPTO_SYMBOLS
CHECKER_DIMS = CRYPTO_SYMBOLS + ASYNC_SYMBOLS + FS_SYMBOLS
JUDGE_DIMS = CRYPTO_SYMBOLS + ASYNC_SYMBOLS + FS_SYMBOLS + ALLOC_SYMBOLS + STD_SYMBOLS

# (relative path, is_cargo_crate, symbol dimensions to apply)
SYMBOL_UNITS = [
    ("crates/kirra-core", True, CRYPTO_ONLY),
    ("crates/kirra-trajectory", True, CHECKER_DIMS),
    (QNX_JUDGE, False, JUDGE_DIMS),
]

LINE_COMMENT_RE = re.compile(r"//[^\n]*")
BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)


# ---------------------------------------------------------------------------
# Pure check helpers (fed synthetic inputs by --self-test)
# ---------------------------------------------------------------------------

def strip_comments(src: str) -> str:
    return LINE_COMMENT_RE.sub("", BLOCK_COMMENT_RE.sub("", src))


def scan_text(src: str, dimensions) -> list[str]:
    """Return the human labels of every forbidden pattern present in
    comment-stripped `src`."""
    code = strip_comments(src)
    hits = []
    for rx, label in dimensions:
        if rx.search(code):
            hits.append(label)
    return hits


def load_manifest(crate_dir: Path) -> dict:
    with open(crate_dir / "Cargo.toml", "rb") as f:
        return tomllib.load(f)


def linked_deps(manifest: dict, include_dev: bool) -> dict[str, object]:
    """Dependency name→spec for the edges cargo LINKS from this manifest:
    [dependencies] (incl. optional — an optional crypto edge is still a crypto
    edge), every [target.*.dependencies], build-dependencies, and — at a fenced
    ROOT only — its own [dev-dependencies] (tests/examples link those; a crypto
    op smuggled into a checker's own test is exactly the shortcut this forbids).
    Transitive dev-deps are never linked by cargo and are never included."""
    deps: dict[str, object] = {}
    deps.update(manifest.get("dependencies", {}))
    for target_tbl in manifest.get("target", {}).values():
        deps.update(target_tbl.get("dependencies", {}))
        if include_dev:
            deps.update(target_tbl.get("dev-dependencies", {}))
    deps.update(manifest.get("build-dependencies", {}))
    if include_dev:
        deps.update(manifest.get("dev-dependencies", {}))
    return deps


def dep_name(name: str, spec: object) -> str:
    """The real crate name behind a dependency entry (honor `package =`)."""
    if isinstance(spec, dict) and "package" in spec:
        return spec["package"]
    return name


def workspace_members() -> dict[str, Path]:
    """Workspace crate name → directory (globs resolved), including the root and
    the sibling `parko/` workspace members that appear in verdict-core closures
    (parko-core is a kirra-trajectory dependency)."""
    members: dict[str, Path] = {}

    def ingest(root_manifest_dir: Path):
        with open(root_manifest_dir / "Cargo.toml", "rb") as f:
            root = tomllib.load(f)
        for member in root.get("workspace", {}).get("members", []):
            dirs = [root_manifest_dir] if member == "." else sorted(root_manifest_dir.glob(member))
            for d in dirs:
                mp = d / "Cargo.toml" if d.is_dir() else None
                if mp and mp.exists():
                    m = tomllib.loads(mp.read_text(encoding="utf-8"))
                    name = m.get("package", {}).get("name")
                    if name:
                        members[name] = d

    ingest(REPO)
    # parko/ is a SEPARATE workspace; parko-core is reachable from kirra-trajectory.
    if (REPO / "parko" / "Cargo.toml").exists():
        ingest(REPO / "parko")
    return members


def crypto_closure_breaches(crate_dir: Path, members: dict[str, Path]) -> list[str]:
    """Walk the dependency closure of a fenced crate over workspace path deps and
    return every forbidden crypto crate it reaches (workspace or external), with
    the path collapsed to the offending crate name."""
    breaches: list[str] = []
    visited_dirs: set[Path] = set()
    seen_names: set[str] = set()
    queue: list[tuple[Path, bool]] = [(crate_dir, True)]
    while queue:
        d, is_root = queue.pop()
        if d in visited_dirs:
            continue
        visited_dirs.add(d)
        try:
            manifest = load_manifest(d)
        except FileNotFoundError:
            continue
        for raw_name, spec in linked_deps(manifest, include_dev=is_root).items():
            name = dep_name(raw_name, spec)
            if name in FORBIDDEN_CRYPTO_DEPS and name not in seen_names:
                seen_names.add(name)
                breaches.append(name)
            if name in members:
                queue.append((members[name], False))
    return breaches


def rust_sources(crate_dir: Path) -> list[Path]:
    """Every .rs under a fenced crate (src/ + tests/ + examples/ + benches/)."""
    return sorted(crate_dir.rglob("*.rs"))


# ---------------------------------------------------------------------------
# Self-test (non-vacuity proof)
# ---------------------------------------------------------------------------

def self_test() -> int:
    failures: list[str] = []

    def expect(cond: bool, msg: str):
        if not cond:
            failures.append(msg)

    # Crypto symbol scan fires on a signing token, passes on clean checker code.
    expect(scan_text("let sk = SigningKey::generate();", CRYPTO_SYMBOLS),
           "self-test: crypto scan missed SigningKey")
    expect(scan_text("use ed25519_dalek::Signature;", CRYPTO_SYMBOLS),
           "self-test: crypto scan missed Signature")
    expect(not scan_text("let v = validate_trajectory_slow(&t);", CRYPTO_SYMBOLS),
           "self-test: crypto scan false-positived on clean checker code")
    # A signing token buried in a comment must NOT fire (comment stripping).
    expect(not scan_text("// SigningKey is minted elsewhere\nlet x = 1;", CRYPTO_SYMBOLS),
           "self-test: crypto scan fired on a commented-out token")

    # Async fires on .await / async fn / tokio.
    expect(scan_text("async fn tick() { store.read().await; }", ASYNC_SYMBOLS),
           "self-test: async scan missed async fn / .await")
    expect(scan_text("tokio::spawn(f);", ASYNC_SYMBOLS),
           "self-test: async scan missed tokio")
    expect(not scan_text("fn tick() { let x = a + b; }", ASYNC_SYMBOLS),
           "self-test: async scan false-positived on sync code")

    # Filesystem / network fires.
    expect(scan_text("std::fs::read(p)", FS_SYMBOLS),
           "self-test: fs scan missed std::fs")
    expect(scan_text("File::open(p)", FS_SYMBOLS),
           "self-test: fs scan missed File::open")

    # Alloc fires on heap types; core:: and no_std do NOT false-positive.
    expect(scan_text("let v: Vec<u8> = Vec::new();", ALLOC_SYMBOLS),
           "self-test: alloc scan missed Vec")
    expect(scan_text("let s = format!(\"{x}\");", ALLOC_SYMBOLS),
           "self-test: alloc scan missed format!")
    expect(not scan_text("#![no_std]\nuse core::mem::size_of;", ALLOC_SYMBOLS + STD_SYMBOLS),
           "self-test: alloc/std scan false-positived on no_std + core::")
    expect(scan_text("use std::process::abort;", STD_SYMBOLS),
           "self-test: std scan missed std::")

    # Dependency crypto set: ed25519-dalek is forbidden, sha2 is NOT (hashing).
    expect("ed25519-dalek" in FORBIDDEN_CRYPTO_DEPS,
           "self-test: ed25519-dalek must be a forbidden crypto dep")
    expect("kirra-release-token" in FORBIDDEN_CRYPTO_DEPS,
           "self-test: kirra-release-token must be a forbidden crypto dep")
    expect("sha2" not in FORBIDDEN_CRYPTO_DEPS,
           "self-test: sha2 (hashing) must NOT be forbidden (5.3 finding)")
    expect("hex" not in FORBIDDEN_CRYPTO_DEPS,
           "self-test: hex must NOT be forbidden")

    if failures:
        for f in failures:
            print(f"FAIL {f}")
        print("\nverdict-core purity self-test FAILED — the fence is vacuous; do not trust a green run.")
        return 1
    print("verdict-core purity self-test: OK (crypto/async/fs/alloc/std scans all fire; clean inputs pass)")
    return 0


# ---------------------------------------------------------------------------
# Main fence
# ---------------------------------------------------------------------------

def main() -> int:
    if "--self-test" in sys.argv:
        return self_test()

    failures: list[str] = []
    members = workspace_members()

    # Gate self-checks: a fenced unit going missing must fail the gate, not
    # silently void it (renames update this file in the same PR).
    for rel in CRYPTO_DEP_FENCED_CRATES:
        if not (REPO / rel / "Cargo.toml").exists():
            failures.append(
                f"gate self-check: fenced crate `{rel}` has no Cargo.toml — "
                f"renamed/moved? update CRYPTO_DEP_FENCED_CRATES in {Path(__file__).name}"
            )
    for rel, is_crate, _dims in SYMBOL_UNITS:
        target = REPO / rel
        if is_crate and not (target / "Cargo.toml").exists():
            failures.append(f"gate self-check: fenced crate `{rel}` missing — update SYMBOL_UNITS")
        if not is_crate and not target.exists():
            failures.append(
                f"gate self-check: fenced source `{rel}` missing — the QNX judge "
                f"moved/renamed? update QNX_JUDGE in {Path(__file__).name}"
            )
    if failures:
        for f in failures:
            print(f"FAIL {f}")
        return 1

    # 1. Dependency-closure crypto fence.
    for rel in CRYPTO_DEP_FENCED_CRATES:
        breaches = crypto_closure_breaches(REPO / rel, members)
        for name in breaches:
            failures.append(
                f"{rel}: dependency closure reaches crypto crate `{name}` — "
                f"{FORBIDDEN_CRYPTO_DEPS[name]}. The verdict is computed BEFORE "
                f"signing (ADR-0031); a crypto edge in the verdict core folds the "
                f"signing op into the verdict WCET and voids the FTTI decomposition."
            )
        # Only claim "clean" when it actually is — a breach is reported in the
        # failures block below, not contradicted by a premature ok line (Copilot).
        if not breaches:
            print(f"ok   {rel}: crypto dependency closure clean (0 signing edges)")

    # 2. Symbol fence (per-unit dimensions).
    for rel, is_crate, dims in SYMBOL_UNITS:
        target = REPO / rel
        sources = rust_sources(target) if is_crate else [target]
        unit_hits = 0
        for rs in sources:
            code = rs.read_text(encoding="utf-8", errors="replace")
            for label in scan_text(code, dims):
                unit_hits += 1
                failures.append(
                    f"{rs.relative_to(REPO)}: forbidden token — {label}. This is "
                    f"verdict-core / kernel source; it must stay pure (crypto rides "
                    f"the actuation path AFTER the verdict, never inside it)."
                )
        if unit_hits == 0:
            dim_names = "crypto"
            if dims is CHECKER_DIMS:
                dim_names = "crypto+async+fs"
            elif dims is JUDGE_DIMS:
                dim_names = "crypto+async+fs+alloc+std"
            print(f"ok   {rel}: symbol scan clean ({dim_names})")

    if failures:
        print()
        for f in failures:
            print(f"FAIL {f}")
        print(
            "\nThe verdict-core purity fence is load-bearing (ADR-0031 crypto "
            "separation + the FTTI WCET decomposition): the safety verdict must be "
            "a pure, bounded, crypto-free computation, and the QNX judge must stay "
            "no_std / zero-alloc. Signing belongs on the actuation path AFTER the "
            "verdict (kirra-inline-governor / kirra-release-token), never in the core."
        )
        return 1
    print("verdict-core purity fence: INTACT")
    return 0


if __name__ == "__main__":
    sys.exit(main())
