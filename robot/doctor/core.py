"""doctor.core — schema, module runner, aggregation, and report rendering.

Design (docs/diagnostics.md):
  * Every module is an independent file in doctor/modules/ exposing
    NAME / DESCRIPTION / DEFAULT / HEAVY / TIMEOUT_S and run(ctx) -> partial
    result. A module raising is an UNKNOWN result for that module — it never
    stops the others (isolation is enforced HERE, not trusted per-module).
  * Statuses: PASS < WARN < UNKNOWN < FAIL (severity order). UNKNOWN means
    "could not verify" — not healthy, so it maps to the WARN exit code.
  * Report schema v1 is STABLE and machine-readable (kirra_doctor --json);
    fields are only ever added, never renamed (schema_version bumps on breaks).
  * Deterministic: modules run sorted by name, results are emitted sorted by
    name; the only environment-independent nondeterminism is the timestamp and
    elapsed timings.
  * stdlib only (urllib, subprocess, concurrent.futures) — same dependency
    discipline as the rabbit_* scripts.

Status policy (matches kirra_voice_doctor.sh): FAIL is reserved for
"configured-but-broken" (an env var points at a missing file, a pinned device
absent). "Optional thing not running" (a service that is staged-not-enabled,
NTP off) is a WARN — diagnostics must not cry wolf.
"""
from __future__ import annotations

import concurrent.futures
import importlib
import json
import os
import pkgutil
import socket
import subprocess
import time
import urllib.request

SCHEMA_VERSION = 1
STATUSES = ("PASS", "WARN", "UNKNOWN", "FAIL")
_RANK = {"PASS": 0, "WARN": 1, "UNKNOWN": 2, "FAIL": 3}
# Exit codes: 0 healthy / 1 warnings-or-unverifiable / 2 failures / 3 internal.
EXIT_FOR = {"PASS": 0, "WARN": 1, "UNKNOWN": 1, "FAIL": 2}
EXIT_INTERNAL = 3
DEFAULT_MODULE_TIMEOUT_S = 20


def worst(statuses):
    """Severity-max of an iterable of statuses ('' -> PASS baseline)."""
    w = "PASS"
    for s in statuses:
        if _RANK.get(s, _RANK["UNKNOWN"]) > _RANK[w]:
            w = s if s in _RANK else "UNKNOWN"
    return w


def detail(check, status, info="", fix=None):
    """One check row inside a module result."""
    d = {"check": check, "status": status, "info": info}
    if fix:
        d["fix"] = fix
    return d


# ---- read-only helpers modules share (never raise) --------------------------

def run_cmd(argv, timeout_s=15, env=None):
    """(rc, stdout, stderr); rc -1 on timeout / not-found. Never raises."""
    try:
        p = subprocess.run(argv, capture_output=True, text=True,
                           timeout=timeout_s, env=env)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return -1, "", f"timeout after {timeout_s}s"
    except Exception as e:  # noqa: BLE001
        return -1, "", str(e)


def http_status(url, timeout_s=2.0):
    """HTTP status code for a GET, or None (unreachable). Never raises."""
    try:
        with urllib.request.urlopen(url, timeout=timeout_s) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code
    except Exception:  # noqa: BLE001
        return None


def port_listening(port, host="127.0.0.1", timeout_s=0.5):
    try:
        with socket.create_connection((host, port), timeout=timeout_s):
            return True
    except Exception:  # noqa: BLE001
        return False


def read_env_file(path):
    """KEY=value pairs from a robot.env-style file ('' on unreadable)."""
    out = {}
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                k, v = line.split("=", 1)
                out[k.strip()] = v.strip().strip('"').strip("'")
    except Exception:  # noqa: BLE001
        pass
    return out


def make_ctx():
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # robot/
    repo = os.path.dirname(here)
    renv_path = os.environ.get("KIRRA_ROBOT_ENV", "/etc/kirra/robot.env")
    return {
        "here": here,                       # the staged/repo robot/ dir
        "repo": repo,                       # repo root when running from a checkout
        "robot_env_path": renv_path,
        "robot_env": read_env_file(renv_path),
        "env": dict(os.environ),
    }


# ---- discovery + execution ---------------------------------------------------

def discover_modules():
    """Import every doctor.modules.* file exposing NAME + run(). Sorted by NAME.
    A module that fails to IMPORT is surfaced as an UNKNOWN placeholder rather
    than silently vanishing (a missing diagnostic must be visible)."""
    from doctor import modules as pkg
    found = []
    for info in pkgutil.iter_modules(pkg.__path__):
        if info.name.startswith("_"):
            continue
        try:
            m = importlib.import_module(f"doctor.modules.{info.name}")
            if hasattr(m, "NAME") and callable(getattr(m, "run", None)):
                found.append(m)
        except Exception as e:  # noqa: BLE001
            found.append(_broken_module(info.name, str(e)))
    return sorted(found, key=lambda m: m.NAME)


def _broken_module(name, err):
    class _B:  # minimal stand-in so the failure is REPORTED, not hidden
        NAME = name
        DESCRIPTION = "module failed to import"
        DEFAULT, HEAVY, TIMEOUT_S = True, False, 5
        _err = err

        @staticmethod
        def run(_ctx):
            return {"details": [detail("import", "UNKNOWN", _B._err)],
                    "recommended_action": "fix the module import error"}
    return _B


def run_module(mod, ctx, timeout_s=None):
    """Execute one module with isolation: an exception or timeout is an UNKNOWN
    result for THIS module only. Expected failures carry no stack traces —
    the exception message is the operator-facing info."""
    t0 = time.monotonic()
    timeout = timeout_s or getattr(mod, "TIMEOUT_S", DEFAULT_MODULE_TIMEOUT_S)
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as ex:
            partial = ex.submit(mod.run, ctx).result(timeout=timeout)
    except concurrent.futures.TimeoutError:
        partial = {"details": [detail("execution", "UNKNOWN",
                                      f"module timed out after {timeout}s")],
                   "recommended_action": "investigate why this check hangs"}
    except Exception as e:  # noqa: BLE001
        partial = {"details": [detail("execution", "UNKNOWN", f"internal error: {e}")],
                   "recommended_action": "run this module alone with --verbose"}
    details = partial.get("details", [])
    status = partial.get("status") or worst(d["status"] for d in details) if details else "UNKNOWN"
    rec = partial.get("recommended_action")
    if rec is None:  # derive from the first non-PASS detail carrying a fix
        for d in details:
            if d["status"] != "PASS" and d.get("fix"):
                rec = d["fix"]
                break
    return {
        "name": mod.NAME,
        "description": getattr(mod, "DESCRIPTION", ""),
        "status": status,
        "elapsed_ms": int((time.monotonic() - t0) * 1000),
        "details": details,
        "recommended_action": rec,
        "metadata": {"heavy": bool(getattr(mod, "HEAVY", False)),
                     "default": bool(getattr(mod, "DEFAULT", True)),
                     "read_only": True,
                     **partial.get("metadata", {})},
    }


def run_all(mods, ctx=None, parallel=True):
    """Run modules (parallel by default — every module is read-only, so there is
    no cross-module state to race) and assemble the schema-v1 report."""
    ctx = ctx or make_ctx()
    t0 = time.monotonic()
    if parallel and len(mods) > 1:
        with concurrent.futures.ThreadPoolExecutor(max_workers=min(8, len(mods))) as ex:
            results = list(ex.map(lambda m: run_module(m, ctx), mods))
    else:
        results = [run_module(m, ctx) for m in mods]
    results.sort(key=lambda r: r["name"])
    counts = {s: sum(1 for r in results if r["status"] == s) for s in STATUSES}
    return {
        "schema_version": SCHEMA_VERSION,
        "status": worst(r["status"] for r in results),
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "host": socket.gethostname(),
        "duration_ms": int((time.monotonic() - t0) * 1000),
        "modules": results,
        "summary": counts,
    }


# ---- rendering ---------------------------------------------------------------

_MARK = {"PASS": "✔", "WARN": "⚠", "UNKNOWN": "?", "FAIL": "❌"}


def human_report(report, verbose=False, timings=False):
    lines = [f"== kirra doctor — {report['status']} "
             f"({report['summary']['PASS']} pass / {report['summary']['WARN']} warn / "
             f"{report['summary']['UNKNOWN']} unknown / {report['summary']['FAIL']} fail, "
             f"{report['duration_ms']} ms) =="]
    for m in report["modules"]:
        t = f"  [{m['elapsed_ms']} ms]" if timings else ""
        lines.append(f" {_MARK[m['status']]} {m['name']:<12} {m['status']:<7}{t}  {m['description']}")
        for d in m["details"]:
            if verbose or d["status"] != "PASS":
                lines.append(f"     {_MARK[d['status']]} {d['check']}: {d['info']}")
                if d.get("fix") and d["status"] != "PASS":
                    lines.append(f"        ↳ fix: {d['fix']}")
        if m["recommended_action"] and m["status"] in ("FAIL", "UNKNOWN"):
            lines.append(f"     ↳ recommended: {m['recommended_action']}")
    return "\n".join(lines)


def issues(report):
    """(module, first non-PASS detail) pairs, FAILs first — the speech source."""
    out = []
    for m in sorted(report["modules"], key=lambda m: -_RANK[m["status"]]):
        if m["status"] == "PASS":
            continue
        first = next((d for d in m["details"] if d["status"] != "PASS"), None)
        out.append((m, first))
    return out


def speech_summary(report, max_issues=3):
    """A short spoken summary — counts + at most max_issues plain sentences.
    Never reads details/paths aloud (the CLI/JSON has those)."""
    probs = issues(report)
    if not probs:
        n = len(report["modules"])
        return f"Diagnostics complete. Everything looks healthy across {n} modules."
    fails = report["summary"]["FAIL"]
    others = len(probs) - fails
    head = "Diagnostics complete. "
    if fails:
        head += f"{fails} problem{'s' if fails != 1 else ''} found"
        head += f" and {others} warning{'s' if others != 1 else ''}." if others else "."
    else:
        head += f"{others} warning{'s' if others != 1 else ''}, nothing broken."
    parts = [head]
    for m, d in probs[:max_issues]:
        what = d["check"] if d else "a check"
        verb = "has a problem" if m["status"] == "FAIL" else "needs a look"
        parts.append(f"The {m['name']} module {verb}: {what}.")
    if len(probs) > max_issues:
        parts.append(f"And {len(probs) - max_issues} more — see the full report.")
    return " ".join(parts)
