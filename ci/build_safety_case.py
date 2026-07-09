#!/usr/bin/env python3
"""EP-18 — safety-case-as-code bundle generator (and self-verifier).

Assembles, per release, ONE versioned, hash-chained evidence bundle:

  * the reviewed machine-checked evidence MANIFESTS (safety-constant
    provenance/EP-09, SOTIF trigger coverage, SPI registry, KPI thresholds +
    Monte-Carlo config, quality-guardrail ratchet baseline);
  * the safety-case DOCUMENTS (UL 4600 case, RTM/traceability matrices,
    MC/DC + SOTIF evidence, governor integrity evidence, RSS formal spec,
    HARA, assumptions of use);
  * EXECUTED gates — re-run at bundle time, captured stdout, exit 0 required
    (safety-constant manifest match, quality-guardrail ratchet, and the
    frozen kinematics-talisman blob-pin check);
  * REFERENCED CI lanes — evidence produced by CI rather than at bundle time
    (coverage, loom, fuzz smoke + deep weekly, Miri, Kani proofs, Postgres
    conformance, KPI campaign), recorded by workflow/job with the run URL
    when built inside CI.

The bundle is HASH-CHAINED (the audit-ledger philosophy applied to
evidence): elements are ordered canonically, each contributes
`h_i = SHA256(h_{i-1} || sha256_i || id_i)`, and the final head is the
`bundle_digest`. The digest covers evidence CONTENT + identity, not
wall-clock, so rebuilding the same tree reproduces the same digest.

Usage:
    python3 ci/build_safety_case.py --out target/safety-case   # build
    python3 ci/build_safety_case.py --verify target/safety-case # self-verify

`make safety-case` runs BOTH (a bundle that does not self-verify never
ships). The release workflow builds it on every tag and attaches the tarball.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BUNDLE_FORMAT_VERSION = 1
CHAIN_GENESIS = b"KIRRA-SAFETY-CASE-v1"

# The frozen kinematics-contract talisman (git blob pin). The AUTHORITATIVE
# statement of the pin lives in docs/CAPTURE_PIPELINE_SPEC.md; the gate below
# greps it from there so this script can never drift from the reviewed doc.
TALISMAN_PATH = "crates/kirra-core/src/kinematics_contract.rs"
TALISMAN_PIN_DOC = "docs/CAPTURE_PIPELINE_SPEC.md"

# ---------------------------------------------------------------------------
# The evidence inventory. Adding a reviewed evidence element = one row here.
# ---------------------------------------------------------------------------

FILE_ELEMENTS = [
    # (id, repo-relative path, what it evidences)
    ("manifest.safety-constants", "ci/safety_constants_manifest.json",
     "EP-09 RSS/checker safety-constant provenance + sign-off state (gate: check_safety_constants.py)"),
    ("manifest.sotif-coverage", "ci/sotif_trigger_coverage.json",
     "WS-3.3 ISO 21448 trigger-condition -> verification-evidence mapping (gate: kirra_kpi_gate::sotif_coverage)"),
    ("manifest.spi-registry", "ci/spi_registry.json",
     "UL 4600 SS5.2 safety-performance-indicator registry (gate: kirra_verifier::spi_ledger)"),
    ("manifest.kpi-thresholds", "ci/scenario_kpi_thresholds.json",
     "Scenario-KPI statistical gate thresholds"),
    ("manifest.kpi-montecarlo", "ci/scenario_kpi_montecarlo.json",
     "Monte-Carlo KPI campaign configuration"),
    ("manifest.quality-ratchet", "ci/quality_guardrails_baseline.json",
     "Line/panic-budget ratchet baseline over the safety-critical files"),
    ("doc.ul4600-case", "docs/safety/UL4600_SAFETY_CASE.md",
     "The UL 4600 safety case"),
    ("doc.requirements-traceability", "docs/safety/REQUIREMENTS_TRACEABILITY.md",
     "SG -> requirement -> code -> test traceability"),
    ("doc.traceability-matrix", "docs/safety/TRACEABILITY_MATRIX.md",
     "Auto-generated SAFETY-tag matrix (scripts/extract_safety_traceability.sh; staleness gated by src/traceability_gate.rs)"),
    ("doc.rtm-gap-report", "docs/safety/RTM_GAP_REPORT.md",
     "Known RTM gaps, explicitly recorded"),
    ("doc.mcdc-evidence", "docs/safety/OCCY_MCDC_EVIDENCE.md",
     "MC/DC / branch coverage evidence over the SG-critical decision logic"),
    ("doc.sotif", "docs/safety/OCCY_SOTIF.md",
     "ISO 21448 SOTIF analysis (triggering-condition catalogue)"),
    ("doc.governor-integrity", "docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md",
     "Governor integrity evidence plan (incl. the EP-15 machine-checked-proof element)"),
    ("doc.rss-formal-spec", "docs/safety/KIRRA_RSS_FORMAL_SPECIFICATION.md",
     "RSS formal specification"),
    ("doc.hara", "docs/safety/HARA.md",
     "Hazard analysis and risk assessment"),
    ("doc.assumptions-of-use", "docs/safety/ASSUMPTIONS_OF_USE.md",
     "Integrator assumptions of use (AoU catalogue)"),
]

# Executed at bundle time; exit 0 REQUIRED (a red gate aborts the bundle).
# NOTE: check_safety_constants.py runs WITHOUT KIRRA_RELEASE_GATE here — the
# bundle RECORDS the sign-off state (pending entries and all, honestly); the
# release-blocking enforcement of "no pending on release" is that gate's own
# job under KIRRA_RELEASE_GATE=1, deliberately separate from evidence assembly.
GATE_ELEMENTS = [
    ("gate.safety-constants", [sys.executable, "ci/check_safety_constants.py"],
     "Declared checker constants still match the reviewed provenance manifest"),
    ("gate.quality-ratchet", [sys.executable, "ci/check_quality_guardrails.py"],
     "Line/panic budgets within the reviewed ratchet baseline"),
]

# Evidence produced by CI lanes rather than at bundle time. Recorded (and
# resolvable to a concrete run URL when the bundle is built inside CI).
CI_LANE_ELEMENTS = [
    ("lane.coverage", "ci.yml", "coverage", "Branch/MC-DC coverage numbers"),
    ("lane.loom", "ci.yml", "loom", "Exhaustive concurrency-model results"),
    ("lane.fuzz-smoke", "ci.yml", "fuzz-build", "Fuzz targets build + bounded 60s run"),
    ("lane.fuzz-deep", "fuzz-deep-weekly.yml", "deep-fuzz", "Weekly deep-fuzz campaign"),
    ("lane.miri", "ci.yml", "miri", "Miri UB checks over the unsafe boundaries"),
    ("lane.kani-proofs", "ci.yml", "kani-proofs", "EP-15 machine-checked proofs (+ blocking mirror tier)"),
    ("lane.postgres-conformance", "ci.yml", "postgres-conformance", "Live second-backend storage conformance"),
    ("lane.kpi-campaign", "kpi-campaign-nightly.yml", "campaign", "Nightly Monte-Carlo KPI campaign + SOTIF coverage gate (decision-floor check rides the coverage lane)"),
]


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=REPO, check=True, capture_output=True, text=True
    ).stdout.strip()


def talisman_gate() -> tuple[int, str]:
    """The frozen-talisman pin check: the file's git blob hash must equal the
    pin recorded in the reviewed spec doc. Returns (exit_code, transcript)."""
    doc = (REPO / TALISMAN_PIN_DOC).read_text(encoding="utf-8")
    import re

    m = re.search(
        re.escape(TALISMAN_PATH) + r"\s*=\s*([0-9a-f]{40})", doc
    )
    if not m:
        return 1, f"FAIL: no blob pin for {TALISMAN_PATH} found in {TALISMAN_PIN_DOC}"
    pinned = m.group(1)
    actual = git("hash-object", TALISMAN_PATH)
    if actual != pinned:
        return 1, (
            f"FAIL: talisman blob drift!\n  pinned ({TALISMAN_PIN_DOC}): {pinned}\n"
            f"  actual git hash-object:  {actual}\n"
            "The frozen kinematics contract changed without a reviewed re-pin."
        )
    return 0, f"OK: {TALISMAN_PATH} blob {actual} matches the reviewed pin in {TALISMAN_PIN_DOC}"


def identity() -> dict:
    ident = {
        "package_version": None,
        "git_commit": git("rev-parse", "HEAD"),
        "git_describe": None,
        "git_dirty": bool(git("status", "--porcelain")),
        "toolchain_channel": None,
    }
    try:
        ident["git_describe"] = git("describe", "--tags", "--always")
    except subprocess.CalledProcessError:
        pass
    for line in (REPO / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        if line.startswith("version = ") and ident["package_version"] is None:
            ident["package_version"] = line.split('"')[1]
    for line in (REPO / "rust-toolchain.toml").read_text(encoding="utf-8").splitlines():
        if line.strip().startswith("channel"):
            ident["toolchain_channel"] = line.split('"')[1]
    return ident


def build(out_dir: Path) -> int:
    if out_dir.exists():
        shutil.rmtree(out_dir)
    (out_dir / "files").mkdir(parents=True)
    (out_dir / "gates").mkdir(parents=True)

    elements = []

    # 1. Files — copied verbatim, hashed.
    for elem_id, rel, evidences in FILE_ELEMENTS:
        src = REPO / rel
        if not src.is_file():
            print(f"FAIL: evidence file missing: {rel} ({elem_id})", file=sys.stderr)
            return 1
        dest = out_dir / "files" / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dest)
        elements.append({
            "id": elem_id, "kind": "file", "path": f"files/{rel}",
            "sha256": sha256_file(src), "evidences": evidences,
        })

    # 2. Gates — executed now; exit 0 required; transcript captured + hashed.
    gates = list(GATE_ELEMENTS)
    for elem_id, argv, evidences in gates:
        proc = subprocess.run(argv, cwd=REPO, capture_output=True, text=True)
        transcript = (
            f"$ {' '.join(argv)}\nexit: {proc.returncode}\n"
            f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}"
        )
        tpath = out_dir / "gates" / f"{elem_id}.txt"
        tpath.write_text(transcript, encoding="utf-8")
        if proc.returncode != 0:
            print(f"FAIL: gate {elem_id} exited {proc.returncode} — see {tpath}",
                  file=sys.stderr)
            print(transcript, file=sys.stderr)
            return 1
        elements.append({
            "id": elem_id, "kind": "gate", "path": f"gates/{elem_id}.txt",
            "sha256": sha256_bytes(transcript.encode("utf-8")),
            "evidences": evidences, "exit_code": 0,
        })

    # 2b. The talisman blob-pin gate (in-process; same contract).
    code, transcript = talisman_gate()
    tpath = out_dir / "gates" / "gate.talisman-pin.txt"
    tpath.write_text(transcript + "\n", encoding="utf-8")
    if code != 0:
        print(transcript, file=sys.stderr)
        return 1
    elements.append({
        "id": "gate.talisman-pin", "kind": "gate",
        "path": "gates/gate.talisman-pin.txt",
        "sha256": sha256_bytes((transcript + "\n").encode("utf-8")),
        "evidences": "Frozen kinematics-contract talisman matches its reviewed blob pin",
        "exit_code": 0,
    })

    # 3. CI lanes — referenced; a concrete run URL when built inside CI.
    run_url = None
    if os.environ.get("GITHUB_RUN_ID") and os.environ.get("GITHUB_REPOSITORY"):
        run_url = (
            f"https://github.com/{os.environ['GITHUB_REPOSITORY']}"
            f"/actions/runs/{os.environ['GITHUB_RUN_ID']}"
        )
    for elem_id, workflow, job, evidences in CI_LANE_ELEMENTS:
        ref = f"workflow={workflow} job={job}"
        elements.append({
            "id": elem_id, "kind": "ci-lane", "workflow": workflow, "job": job,
            "sha256": sha256_bytes(ref.encode("utf-8")),
            "evidences": evidences, "built_under_run": run_url,
        })

    # 4. The hash chain over the canonical (id-sorted) element order.
    elements.sort(key=lambda e: e["id"])
    head = sha256_bytes(CHAIN_GENESIS)
    chain = []
    for e in elements:
        head = sha256_bytes(bytes.fromhex(head) + bytes.fromhex(e["sha256"]) + e["id"].encode())
        chain.append({"id": e["id"], "chained": head})

    manifest = {
        "bundle_format_version": BUNDLE_FORMAT_VERSION,
        "identity": identity(),
        "elements": elements,
        "chain": chain,
        "bundle_digest": head,
    }
    (out_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )
    print(f"safety-case bundle: {len(elements)} elements")
    print(f"bundle_digest: {head}")
    print(f"wrote {out_dir / 'manifest.json'}")
    return 0


def verify(bundle_dir: Path) -> int:
    manifest = json.loads((bundle_dir / "manifest.json").read_text(encoding="utf-8"))
    failures = []
    for e in manifest["elements"]:
        if e["kind"] in ("file", "gate"):
            p = bundle_dir / e["path"]
            if not p.is_file():
                failures.append(f"{e['id']}: missing {e['path']}")
                continue
            actual = sha256_file(p)
            if actual != e["sha256"]:
                failures.append(f"{e['id']}: sha256 mismatch")
        elif e["kind"] == "ci-lane":
            ref = f"workflow={e['workflow']} job={e['job']}"
            if sha256_bytes(ref.encode()) != e["sha256"]:
                failures.append(f"{e['id']}: lane reference mismatch")
    # Re-walk the chain in the recorded order.
    elements = sorted(manifest["elements"], key=lambda e: e["id"])
    head = sha256_bytes(CHAIN_GENESIS)
    for e, link in zip(elements, manifest["chain"]):
        head = sha256_bytes(bytes.fromhex(head) + bytes.fromhex(e["sha256"]) + e["id"].encode())
        if link["id"] != e["id"] or link["chained"] != head:
            failures.append(f"chain broken at {e['id']}")
            break
    if head != manifest["bundle_digest"]:
        failures.append("bundle_digest mismatch")
    if failures:
        for f in failures:
            print(f"VERIFY FAIL: {f}", file=sys.stderr)
        return 1
    print(f"bundle VERIFIED: {len(manifest['elements'])} elements, digest {head}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", type=Path, help="build the bundle into this directory")
    ap.add_argument("--verify", type=Path, help="self-verify an existing bundle")
    args = ap.parse_args()
    if bool(args.out) == bool(args.verify):
        ap.error("exactly one of --out / --verify")
    return build(args.out) if args.out else verify(args.verify)


if __name__ == "__main__":
    sys.exit(main())
