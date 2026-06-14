# Migration: Aegis → Kirra (Issue #43)

This project was formerly named **Aegis** and is now **Kirra**. The code,
crate, primary binary, and runtime environment variables had already moved to
the `kirra` / `KIRRA_*` namespace, but a set of **build, deploy, and
documentation artifacts still referenced the old `aegis` names**. Because the
running service reads only `KIRRA_*`, those stale references were
non-functional — e.g. an `.env`/compose file setting `AEGIS_ADMIN_TOKEN` left
the service with no token (HTTP 503), and build/install scripts pointed at
binaries, a link name, and unit files that no longer exist. Issue #43 tracked
this integrator-build blocker. This change completes the rename.

## Decision: hard rename, no aliases

These are **breaking** renames with **no backward-compatible aliases**, which
is the correct choice here: this is a pre-release SDK with no production
deployment, and the old `AEGIS_*` names were already inert (the binary never
read them after the code-side rename). Keeping aliases would preserve names
that did not actually work. Update any pinned references as below.

## Public renames (old → new)

### Environment variables (set by integrators / deploy)
The service has read `KIRRA_*` for some time; only the deploy artifacts were
stale. Any integrator still exporting `AEGIS_*` must switch:

| Old | New |
|-----|-----|
| `AEGIS_ADMIN_TOKEN` | `KIRRA_ADMIN_TOKEN` |
| `AEGIS_VERIFIER_ADDR` | `KIRRA_VERIFIER_ADDR` |
| `AEGIS_DB_PATH` | `KIRRA_DB_PATH` |
| `AEGIS_VERIFIER_MODE` | `KIRRA_VERIFIER_MODE` |
| `AEGIS_TRUSTED_INGRESS_MODE` | `KIRRA_TRUSTED_INGRESS_MODE` |
| `AEGIS_CLIENT_ID_HEADER` | `KIRRA_CLIENT_ID_HEADER` |
| `AEGIS_LOG_SIGNING_KEY` | `KIRRA_LOG_SIGNING_KEY` |
| `AEGIS_INSTANCE_ID` / `AEGIS_HEARTBEAT_INTERVAL` / `AEGIS_PROMOTION_TIMEOUT` | `KIRRA_INSTANCE_ID` / `KIRRA_HEARTBEAT_INTERVAL` / `KIRRA_PROMOTION_TIMEOUT` |
| `AEGIS_SUPERVISOR_RESET_KEY` | `KIRRA_SUPERVISOR_RESET_KEY` |
| `AEGIS_PORT` / `AEGIS_STANDBY_PORT` (compose/install helpers) | `KIRRA_PORT` / `KIRRA_STANDBY_PORT` |

### Build / link artifacts
| Old | New |
|-----|-----|
| binary `aegis_verifier_service` | `kirra_verifier_service` |
| binary `aegis_carla_client` | `kirra_carla_client` |
| link flag `-laegis_runtime_sdk` | `-lkirra_runtime_sdk` (lib `kirra_runtime_sdk`) |
| release archive `aegis-<ver>-<target>.tar.gz` | `kirra-<ver>-<target>.tar.gz` |

### Deploy / runtime layout
| Old | New |
|-----|-----|
| HTTP identity header `x-aegis-client-id` | `x-kirra-client-id` |
| systemd unit `aegis-verifier.service` | `kirra-verifier.service` |
| systemd unit `aegis-dashboard.service` | `kirra-dashboard.service` |
| config dir `/etc/aegis`, env file `/etc/aegis/aegis.env` | `/etc/kirra`, `/etc/kirra/kirra.env` |
| data dir `/var/lib/aegis`, db `aegis.db` | `/var/lib/kirra`, `kirra.db` |
| log dir `/var/log/aegis` | `/var/log/kirra` |
| system user/group `aegis` | `kirra` |
| docker-compose services/volumes `aegis-verifier`, `aegis-data` | `kirra-verifier`, `kirra-data` |
| npm package `aegis-dashboard` | `kirra-dashboard` |
| GitHub repo `justinlooney/aegis` | `kirra-systems/kirra-runtime-sdk` |
| ROS 2 package refs `aegis_safety` (scripts) | `kirra_safety` |

## Deliberately NOT renamed (kept on purpose)

To avoid corrupting history and the formal traceability namespace, the
following `Aegis`/`AEGIS` references are **intentional** and were left intact:

1. **The `AEGIS-*` safety-case document-ID scheme** — `AEGIS-SC-000`,
   `AEGIS-HARA-001`, `AEGIS-SG-001`, `AEGIS-CG-001`, `AEGIS-ROAD-001`,
   `AEGIS-RTM-001`, `AEGIS-SA-001`, `AEGIS-F3269-001`, `AEGIS-61508-001`,
   `AEGIS-STD-001`, etc. — and the `≅ AEGIS SG-xxx` cross-map comments in the
   code. These are a controlled predecessor-framework identifier namespace
   that the current `KIRRA-*` / `OCCY-*` documents deliberately cross-reference;
   `docs/releases/v1.5.0.md` records that this scheme's rename is tracked
   separately. They are not build-affecting.
2. **Historical artifacts** — the `docs/releases/v1.5.0.md` release note
   ("published under the previous Aegis branding"), the README v1.1.1
   changelog entry, the `### Foundation (Kirra / Aegis line)` doc-table
   heading, and the `work/` planning/decision/done logs (which deliberately
   reference the old names — e.g. grep patterns hunting for `AegisGovernor` to
   confirm its absence, and the #43 task definition itself). Renaming history
   corrupts it.
3. **`CLAUDE.md`** path-avoidance note (`not /home/user/aegis`) — it
   deliberately names the old path to warn against using it.

> Note for reviewers: `work/github-projects.md` still contains a stale
> `parko-aegis` crate reference and an `aegis-integration` GitHub label name.
> These live in a project-tracking doc (kept verbatim as planning history) and
> are not build-affecting; rename them in a separate project-board pass if the
> label is to be renamed in GitHub itself.
