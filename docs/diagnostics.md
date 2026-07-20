# Robot diagnostics — the `kirra_doctor` framework

The R2's self-diagnostics subsystem: deterministic, modular, **read-only**
health checks the robot can run itself — from the CLI, by voice ("Rabbit, run
diagnostics"), at boot, and on a systemd timer — with one stable machine-
readable report schema.

## Security model (the non-negotiable)

**Diagnostics are observability, never a safety authority.**

- Every module is **read-only**: no writes to robot state, no GPIO actuation,
  no firmware, no motion, no `/intent`. The one artifact the subsystem writes
  is its **own report file** (the timer's JSON archive) — never robot state.
- No diagnostic verdict gates the planner, the governor, RSS, the checker, or
  the fence. Those **fail closed on their own, independently** — a green
  doctor is not a permission, and a red doctor is not a stop. Safety stays
  exclusively with the verifier/checker/fence architecture.
- The voice trigger is **deterministic** (regex, the `rabbit_ota` pattern) —
  the LLM can neither decide to run a self-check nor fabricate one; the reply
  the operator hears comes from the real runner, not from model inference.
- Reports carry env **key names**, statuses, and device paths — never secrets
  (tokens, keys, or env *values* beyond non-sensitive config).

## Architecture

```
 robot/kirra_doctor.py        CLI + programmatic entry (collect())
 robot/doctor/core.py         schema v1, runner (parallel + isolation), rendering
 robot/doctor/modules/*.py    one INDEPENDENT file per diagnostic
 robot/rabbit_diag.py         deterministic voice trigger → speech summary
 rabbit_boot.py               boot self-check (logs always; speaks only on FAIL)
 kirra-doctor.service/.timer  periodic run → journal + JSON archive
```

Composition, not replacement: the pre-existing per-concern checks stay
independently runnable and are wrapped as modules —
`kirra_voice_doctor.sh` (→ `voice`), `lint_robot_env.sh` (→ `config`),
`preflight_autostart.sh` (→ `autostart`, opt-in), `cold_boot_drill.sh`
(→ `cold_boot`, opt-in), `capture_from_robot.sh` (→ `snapshot`, status-only —
capture WRITES, so the module reports presence/age and never runs it).
The verifier's own config self-audit (`env_config.rs` unknown-var sweep +
`EffectiveConfig` digest, `startup_sentinel.rs`) runs verifier-side at its
boot; the `governor`/`telemetry` modules observe it from outside via
`/health` `/ready` `/metrics`.

## Module lifecycle

A module is one file in `robot/doctor/modules/` exposing:

```python
NAME = "devices"          # stable id (addressable: --module devices)
DESCRIPTION = "..."
DEFAULT = True            # False → opt-in only (--module NAME or --all)
HEAVY = False             # advisory metadata
TIMEOUT_S = 10            # isolation timeout
def run(ctx) -> {"details": [core.detail(check, status, info, fix)],
                 "recommended_action": str | None, "metadata": {...}}
```

The runner enforces what modules must not be trusted to do themselves:
- **Isolation** — a raising or hanging module becomes an `UNKNOWN` result for
  *that module only*; the others always complete.
- **Status derivation** — module status = worst of its detail statuses;
  `recommended_action` defaults to the first non-PASS detail's `fix`.
- **Determinism** — modules run and report sorted by name; the only
  nondeterminism is the timestamp/timings and the machine's actual state.
- **Parallelism** — read-only modules share no state, so the default set runs
  concurrently (~70 ms wall for 9 modules); `--serial` disables it.

**Statuses** (severity order): `PASS < WARN < UNKNOWN < FAIL`. Policy: `FAIL`
is *configured-but-broken* (a pinned device absent, a placeholder secret);
"optional thing not running" (staged-not-enabled service, NTP off, offline
DNS) is `WARN` — the doctor must not cry wolf. `UNKNOWN` = could not verify
(not healthy: it shares the warning exit code).

## Modules

| Module | Default | Checks |
|---|---|---|
| `config` | ✔ | robot.env readable/structured, placeholders, `lint_robot_env.sh` |
| `devices` | ✔ | motor/lidar serial, camera, `/dev/snd`, gpiochip, group memberships |
| `governor` | ✔ | verify key pinned, consumer FFI present, verifier `/health` (implies `KIRRA_VEHICLE_CLASS` — startup aborts without one) |
| `network` | ✔ | interfaces up, DNS (WARN offline), NTP sync |
| `services` | ✔ | verifier/mick/ollama ports; systemd states (inactive = staged-by-design → PASS; `failed`/restart-looping → FAIL/WARN) |
| `snapshot` | ✔ | per-robot config capture present + fresh (status only — never captures) |
| `storage` | ✔ | disk headroom, `/etc/kirra`, root not remounted read-only |
| `telemetry` | ✔ | `/health` `/ready` `/metrics`, max thermal zone, memory, load |
| `voice` | ✔ | wraps `kirra_voice_doctor.sh` (engines, models, ALSA plughw drift) |
| `autostart` | opt-in | wraps `preflight_autostart.sh` (sudo reads can prompt → never in the timer path) |
| `cold_boot` | opt-in | wraps `cold_boot_drill.sh` (slow, needs ROS + the stack up) |

## CLI

```bash
python3 robot/kirra_doctor.py               # human ✔/⚠/?/❌ report + fixes
python3 robot/kirra_doctor.py --json        # full schema-v1 JSON
python3 robot/kirra_doctor.py --summary     # the short spoken/logged summary
python3 robot/kirra_doctor.py --verbose --timings
python3 robot/kirra_doctor.py --module voice --module devices
python3 robot/kirra_doctor.py --all         # + opt-in heavy modules
python3 robot/kirra_doctor.py --list
python3 robot/kirra_doctor.py --output /var/log/kirra/doctor-last.json
```

**Exit codes:** `0` healthy · `1` warnings or unverifiable · `2` failures ·
`3` internal orchestrator error (including an unknown `--module` name).

## JSON schema (v1 — stable; fields are added, never renamed)

```json
{
  "schema_version": 1,
  "status": "PASS|WARN|UNKNOWN|FAIL",
  "timestamp": "2026-07-20T12:45:07Z",
  "host": "yahboom",
  "duration_ms": 69,
  "summary": {"PASS": 8, "WARN": 1, "UNKNOWN": 0, "FAIL": 0},
  "modules": [{
    "name": "devices", "description": "…", "status": "PASS", "elapsed_ms": 19,
    "details": [{"check": "motor serial", "status": "PASS", "info": "/dev/myserial",
                 "fix": "…(only on non-PASS)"}],
    "recommended_action": null,
    "metadata": {"default": true, "heavy": false, "read_only": true}
  }]
}
```

A future wire format (protobuf) would be generated from this same shape;
`schema_version` bumps on any breaking change.

## Voice ("Rabbit, run diagnostics")

`rabbit_diag.py` — deterministic regex (no LLM), matched in
`rabbit_converse.handle_turn` after the OTA matcher and **before** the LLM
router. Phrases: "run diagnostics", "run a self check/self-test",
"run self diagnostics", "check yourself", "diagnose yourself", "how healthy
are you". Reply = `speech_summary`: counts + at most three plain issue
sentences (module + failing check), never paths/details — voice lines G1–G3
in `RABBIT_VOICE_LINES.md`.

## Boot

`rabbit_boot._maybe_warn_misconfigured` runs the default set after the
greeting: the summary is **logged every boot**; Rabbit **speaks only on a
FAIL** (voice line A6 + the single top issue) — WARNs are routine and must not
become boot nag. Falls back to the standalone `kirra_voice_doctor.sh` when the
framework isn't staged; never breaks boot.

## systemd

`kirra-doctor.service` (oneshot; human report → journal, JSON archive →
`/var/log/kirra/doctor-last.json`; exit 1 = SuccessExitStatus so warnings
don't mark the unit failed) + `kirra-doctor.timer` (5 min after boot, then
hourly; interval via drop-in). Staged NOT enabled by
`install_robot_units.sh`; enable deliberately:
`sudo systemctl enable --now kirra-doctor.timer`.

## Remote exposure (follow-up, designed)

The verifier already serves the posture-exempt observability GETs
(`/health` `/ready` `/metrics`). A `/diagnostics` GET serving the timer's
archived JSON report (path via a registered `KIRRA_*` env key, summary-only by
default, 404 when no report exists) is the planned follow-up — it touches the
verifier binary + route matrix, so it ships as its own reviewed PR rather
than riding this one.

## Adding a module

1. New file `robot/doctor/modules/<name>.py` with the interface above.
   Read-only; expected failures become details, never exceptions.
2. Choose `DEFAULT` honestly: anything slow, sudo-prompting, or
   environment-dependent is opt-in.
3. Add a host test in `robot/kirra_doctor_test.py` (stub external commands —
   the `voice` wrapper test is the pattern).
4. Nothing else: discovery is automatic (`--list` shows it), the installer
   copies the package tree, CI runs the test file.

## Testing

`robot/kirra_doctor_test.py` — host-only, CI-run (`Test` lane), no hardware /
sudo / network: fake-module runners prove schema shape + ordering,
worst-of aggregation, exit-code mapping, isolation (raise + timeout),
recommended-action derivation, selection (default/`--all`/explicit/unknown),
speech capping, the voice-wrapper exit mapping against a stub script, and the
diagnostic-phrase matcher (including OTA/drive non-overlap).
