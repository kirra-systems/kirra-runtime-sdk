> ⚠️ **HISTORICAL SNAPSHOT (2026-06-01, HEAD `19bcb01`) — substantially superseded.**
> The live issue tracker is authoritative. Do NOT use this document to determine
> whether an issue is open, decided, or implemented; multiple rows below were
> resolved after this date. See the addendum at the end for known resolutions.

# KIRRA Issue Triage — 2026-06-01

**Scope:** all 77 open issues + recent main state (HEAD `19bcb01`).
**Author:** triage pass; close recommendations are NOT executed.
**Source data:** GitHub MCP issue list (open + closed); working-tree
spot checks vs `git log --oneline main -50`,
`docs/safety/SAFETY_CASE_INDEX.md`, `docs/safety/TRACEABILITY_MATRIX.md`.

---

## Summary counts

| Bucket | Count | Notes |
|---|---|---|
| **DONE-CLOSE** | 3 | #71, #119, #128 — verifiable evidence on main. **CLOSE COMMENTS DRAFTED, NOT EXECUTED.** |
| **TRACKER** | 17 | 6 AoU trackers (#122 #123 #124 #125 #126 #127) + 7 EPICs (#94 #95 #96 #97 #101 #106 #110 + standards #121) + #132 (Ferrocene productization) + #66 / #67 (QNX upstream pair). Open-by-design. |
| **REAL-WORK P0** | 4 | #43 #69 #73 #89 — pilot-blocking. |
| **REAL-WORK P1** | 22 | pre-production / first-build. |
| **REAL-WORK P2** | 31 | post-pilot / nice-to-have. |
| **NEEDS-TRIAGE** | 0 | (option to bump #65 here if a human wants the MC/DC story re-reviewed) |
| **Total open** | **77** | matches MCP inventory. |

> **Note on TRACKER count.** Per the brief's "7 known trackers" naming, the strict AoU/AoU-adjacent set is 7 (#122, #123, #124, #125, #126, #127, #132). The 17 above includes EPICs (open-by-design until phase completion) + the QNX upstream pair. Either definition is defensible — adjust the label / dashboard partition to taste.

---

## Per-issue bucket table

### DONE-CLOSE (3)

| # | Title | Bucket | Conf | Evidence | Recommended action |
|---|---|---|---|---|---|
| **71** | fix(gateway): bound body + audit failure logs | DONE-CLOSE | HIGH | commit `a9c4b54` · `src/gateway/policy_layer.rs:33` (`MAX_VEHICLE_COMMAND_BYTES = 16 * 1024`) + `:185-218` loud `tracing::error!` on audit failures | close with comment (drafted §"Ready to close") |
| **119** | [Occy] Governor fault model + degraded-mode availability | DONE-CLOSE | HIGH | commit `3f25043` · `docs/safety/OCCY_FAULT_MODEL.md` (KIRRA-OCCY-FAULT-001) | close with comment (drafted) |
| **128** | [Occy] SG2 drivable-space check missing | DONE-CLOSE | HIGH | commits `73cc7b1` (check built, `src/gateway/containment.rs:148`) + `c03a879` (Option-B adapter wiring via #131) + `ce6fe99` (margin 0.40 m, KIRRA-OCCY-SG2-MARGIN-001); `TRACEABILITY_MATRIX.md` SG2 row = `ENFORCED` | close with comment (drafted) |

### TRACKER (17)

| # | Title | Conf | Action this pass |
|---|---|---|---|
| 66 | QNX upstream socket2 + tokio PRs | HIGH | label `aou-tracker`; recommend close as **duplicate of #67** |
| 67 | PARK-024b QNX socket2/tokio upstream | HIGH | label `aou-tracker` |
| 94 | [Occy Phase 2 EPIC] reactive + RSS | HIGH | label `aou-tracker` (phase EPIC) |
| 95 | [Occy Phase 3 EPIC] hard behaviors + MPC | HIGH | label `aou-tracker` |
| 96 | [Occy Phase 4 EPIC] learned + hardening | HIGH | label `aou-tracker` |
| 97 | [Occy EPIC] flood / standing-water | HIGH | label `aou-tracker` (hazard EPIC) |
| 101 | [Occy EPIC] post-collision | HIGH | label `aou-tracker` |
| 106 | [Occy EPIC] commit-zone | HIGH | label `aou-tracker` |
| 110 | [Occy EPIC] teleop envelope | HIGH | label `aou-tracker` |
| 121 | [Occy EPIC] Standards conformance | HIGH | label `aou-tracker` |
| 122 | [Occy] Occlusion RSS rule (iv) | HIGH | label `aou-tracker` (G1 AoU) |
| 123 | [Occy] Localization-integrity AoU | HIGH | label `aou-tracker`; **status comment posted** (≤0.10 m AoU surfaced via SG2-MARGIN-001) |
| 124 | [Occy] D1 Independent Detection Channel | HIGH | label `aou-tracker` |
| 125 | [Occy] Highway sub-ODD DEFERRED | HIGH | label `aou-tracker` |
| 126 | [Occy] Perception Input Contract | HIGH | label `aou-tracker`; **status comment posted** (3 AoU clauses from SPEED-VAL-001) |
| 127 | [Occy] Actuation safe-stop AoU | HIGH | label `aou-tracker`; **status comment posted** (clause 4 + DR-1 + DR-2) |
| 132 | [Occy] Ferrocene productization | HIGH | label `aou-tracker`; **status comment posted** (scope split vs S3 checkbox) |

### REAL-WORK — P0 (4)

| # | Title | Conf | Evidence | Why P0 |
|---|---|---|---|---|
| **43** | PARK-031 normalize Kirra naming | HIGH | `scripts/build_release.sh:17-92` + `scripts/setup_ros2_fleet.sh:6-97` pervasively `kirra-*` / `KIRRA_*` | Pilot integrator first-build hits these scripts and gets confusing artifacts. |
| **69** | fix(gateway): unknown POST/PUT → WriteState | HIGH | `src/gateway/policy.rs:38-41` falls through to `WriteState` (comment: "All other POST/PUT: treat as WriteState") | SG-006 invariant: Unknown commands must DENY before posture check. This silently widens the WriteState surface. |
| **73** | fix(verifier): attestation HMAC | HIGH | `src/bin/kirra_verifier_service.rs:299-360` still HMAC(`KIRRA_ADMIN_TOKEN`, nonce); `ak_public_pem` + PCR16 unread | Documented impersonation gap. |
| **89** | [Occy 0.A] Scaffold kirra-planner crate | HIGH | no `kirra-planner` workspace member in `/Cargo.toml` or `parko/Cargo.toml`; `crates/` has only `kirra-ros2-adapter` | Blocks all Occy Phase 1 work (#90–#93). |

### REAL-WORK — P1 (22)

| # | Title | Notes |
|---|---|---|
| 36 | PARK-024 QNX deployment spike | TIME-SENSITIVE per body; may be supersedable by #67 |
| 44 | PARK-032 Parko in Kirra Docker image | grep on `/Dockerfile` returned no `parko` reference |
| 45 | PARK-033 backend-aware installer | `install.sh` has no `--backend` flag |
| 46 | PARK-034 systemd watchdog | `kirra-verifier.service:47` has `MemoryMax` but no `WatchdogSec` |
| 49 | PARK-037 Parko + Governor ROS2 cmd_vel | trajectory-level adapter exists; cmd_vel-level glue not landed |
| 70 | docs: reconcile liveness exemption | no liveness/exempt terms in `SAFE_STATE_SPECIFICATION.md` |
| 72 | gateway integration test | no `fn build_app` factored out yet |
| 74 | fix: PRAGMA synchronous=FULL | `verifier_store.rs:52` still `NORMAL` |
| 76 | audit key-rotation verify | `verifier_store.rs:657` comment explicitly defers to this issue |
| 77 | audit tail truncation HWM | no `high_water` / anchor-head plumbing on main |
| 78 | standby hash-v2 anchor on promote | `standby_monitor.rs:338-400` doesn't call the anchor |
| 79 | HA close gate TOCTOU | `policy_layer.rs:285` comment explicitly defers to this issue |
| 82 | C2 RSS ingestion arch decision | subsumed by #92 per body |
| 83 | standby posture-freshness on promote | promote path doesn't spawn engine worker |
| 86 | fabric authoritative clamp | `kirra_verifier_service.rs:1402-1450` doesn't apply clamp |
| 88 | wire verifier→fabric posture | no production writer of `update_asset_posture` from verifier cache |
| 90 | [Occy 1.A] World Model adapter | depends on #89 |
| 91 | [Occy 1.B] Planner core + MRC | depends on #89 |
| 92 | [Occy 1.C] Governor trajectory check | partial via #131 + #128; full RSS pairwise pending |
| 93 | [Occy 1.D] Control adapter + integration test | depends on #89-92 |
| 100 | [Occy] Flood CARLA demo | Phase-1 demo |
| 105 | [Occy] Post-collision CARLA demo | Phase-1 demo |
| 109 | [Occy] Commit-zone CARLA demo | Phase-1 demo |
| 117 | UL 4600 GSN + SPIs | GSN skeleton exists in AEGIS-SC-000 §2; SPI catalog missing |
| 118 | Cybersecurity TARA (21434) | no `OCCY_TARA.md` on main |

### REAL-WORK — P2 (31)

#11, #32, #33, #34, #35, #37, #38, #39, #40, #42, #47, #50, #65, #68, #80, #81, #84, #85, #87, #98, #99, #102, #103, #104, #107, #108, #111, #112 — Parko backend-MVPs, QNX-blocked items, post-pilot tech-debt, Phase-2/3 leaves. Listed in the JSON below; deprioritized while integrator-first-build is the focus.

### NEEDS-TRIAGE (0)

No issue was bucketed `NEEDS-TRIAGE` — all 77 had enough evidence to place. **Optional flag**: #65 (MC/DC restoration) could be re-bucketed by the Test Lead if the manual MC/DC evidence in `OCCY_MCDC_EVIDENCE.md` doesn't substitute long-term.

---

## READY TO CLOSE — drafted comments (for human approval)

### #71 — fix(gateway): bound actuator body + log DenyBreach audit failures

> Closing — both findings landed on main:
> 1. Actuator body read is now bounded (commit `a9c4b54`). `src/gateway/policy_layer.rs:33` defines `MAX_VEHICLE_COMMAND_BYTES = 16 * 1024`; the body is read via `axum::body::to_bytes(body, MAX_VEHICLE_COMMAND_BYTES)` at line 105, and oversized bodies are rejected fail-closed.
> 2. DenyBreach audit-write failures are now logged loudly. `src/gateway/policy_layer.rs:185-218` emits `tracing::error!` on poisoned lock, queue-full, and audit-writer-gone paths, with the explicit message that the sequence gap is detectable in the chain.
>
> External-review finding addressed.

### #119 — [Occy] Governor fault model + degraded-mode availability

> Closing — Governor fault model + degraded-mode availability spec landed on main (commit `3f25043`, `docs/safety/OCCY_FAULT_MODEL.md` = KIRRA-OCCY-FAULT-001). The doc covers: Governor fault detection (watchdog, panic, deadlock), safe-state reachability when the Governor itself faults, fail-operational vs fail-safe per ODD, and the relation to KIRRA HA (durable epoch fence + failover). Loss-of-verdict is classified as MRC-immediately. Cross-references #127 (SEooC output AoU) for the integrator's side of the contract. S7 deliverable.

### #128 — [Occy] SG2 drivable-space check missing in Governor (PO-1 coverage hole)

> Closing — SG2 drivable-space containment is now ENFORCED on main.
> - Containment check built and unit-tested in isolation: commit `73cc7b1`, `src/gateway/containment.rs:148` (`validate_trajectory_containment`).
> - Wired via the Option-B per-trajectory adapter on main (commit `c03a879`, #131): the slow loop in `crates/kirra-ros2-adapter` calls `validate_trajectory_containment` per accepted trajectory; rejects collapse the per-asset slot so the fast loop publishes MRC.
> - Lateral margin = 0.40 m per KIRRA-OCCY-SG2-MARGIN-001 (`docs/safety/OCCY_SG2_MARGIN.md`, commit `ce6fe99`); assumes G2 AoU #123 (≤ 0.10 m 95th-pct localization error).
> - `docs/safety/TRACEABILITY_MATRIX.md` SG2 disposition flipped PENDING-WIRING → ENFORCED.
>
> PO-1 SG2 coverage hole closed.

---

## PRIORITIZED REAL-WORK list

**P0 (pilot-blocking) — 4**:
1. **#89** Scaffold `kirra-planner` crate — blocks Occy Phase 1 entirely.
2. **#43** Normalize kirra-* → kirra-* across `scripts/` + Docker — blocks integrator first-build.
3. **#69** Gateway POST/PUT classification — SG-006 invariant gap (unknown commands silently classified as WriteState).
4. **#73** Per-node attestation key (not shared HMAC) — documented impersonation gap.

**P1 (pre-production / first-build) — 22**: the security-hardening cluster (#74, #76, #77, #78, #79), HA promote-path freshness (#83), fabric authoritative clamp + posture wiring (#86, #88), the Occy Phase-1 dependency chain (#90, #91, #92, #93) once #89 lands, the CARLA pilot demo trio (#100, #105, #109), the standards docs (#117, #118), gateway integration test (#72), liveness reconciliation (#70), Parko packaging (#44, #45, #46), QNX spike (#36), ROS2 cmd_vel wiring (#49).

**P2 (post-pilot) — 31**: Parko backend-MVPs (#32, #33, #34, #35, #37, #38, #39, #40, #42, #11), QNX-blocked items (#47, #50), liveness-only HA improvements (#80, #81), tech-debt (#65, #68, #84, #85, #87), Phase-2/3 leaves (#98, #99, #102, #103, #104, #107, #108, #111, #112).

---

## NEEDS HUMAN DECISION

1. **Duplicate / supersedes pairs** flagged below — each needs an owner's call before closing:
   - **#66 ⊂ #67** (QNX socket2 + tokio upstream PRs) — strict duplicate; recommend closing #66.
   - **#82 → #92** (RSS ingestion arch decision) — explicitly subsumed per #92 body.
   - **#49 ↔ #92** (ROS2 cmd_vel wiring vs Occy Governor trajectory check) — scope overlap.
   - **#119 ↔ #127** (fault model vs actuation safe-stop AoU) — tight coupling, cross-link recommended.
   - **#117 ⊂ #121** (UL 4600 GSN + SPIs is a leaf under the Standards EPIC).
   - **#132 / #124** (Ferrocene productization vs D1 IDC add-on) — both are "premium tier" trackers; cross-link.
   - **#70 + #72** cluster as gateway-wiring external-review fallout.

2. **#65 MC/DC restoration** — bucketed REAL-WORK P2 here on the basis of manual MCDC evidence in `OCCY_MCDC_EVIDENCE.md` providing partial substitute. Test Lead may want to bump it back to P1 or label `needs-triage`.

3. **#85 (legacy industrial token velocities)** — fix the magnitudes vs retire the legacy path. Triage decision needed before scheduling work.

4. **TRACKER vs EPIC partition** — I bucketed Phase / Hazard EPICs as TRACKER. If the project board uses a separate "Epic" board column, drop them from `aou-tracker` to that column.

5. **#11 PARK-006 v0.1.0 release tag** — `parko-core/Cargo.toml` carries `version = "0.1.0"` but no `parko-core-v0.1.0` git tag was ever cut. Decision: cut the tag and close, or rev to v0.1.1 first.

---

## Safe writes — what was applied this pass

- **Status comments posted** on the 4 trackers gaining content this session (commit hashes from S8 close-out):
  - #123 (G2 localization AoU — surfaced ≤ 0.10 m 95th-pct from KIRRA-OCCY-SG2-MARGIN-001)
  - #126 (Perception Input Contract — 3 AoU clauses from KIRRA-OCCY-SPEED-VAL-001)
  - #127 (Actuation safe-stop AoU — clause 4 + DR-1 + DR-2 from SPEED-VAL-001 + KIRRA-OCCY-QUANT-001)
  - #132 (Ferrocene productization — scope split vs S3 safety-case checkbox)
- **Labels** — the brief asked for `aou-tracker`, `done-pending-close`, `needs-triage` to be created if missing. **None exist on the repo today.** The GitHub MCP exposes `get_label` but no `create_label` tool. Two options for the human follow-up:
  - Run `gh label create aou-tracker --color BFD4F2`, `gh label create done-pending-close --color 0E8A16`, `gh label create needs-triage --color FBCA04` (or via web UI).
  - Once created, apply via the per-issue `recommended-action` column above. The MCP `issue_write` can apply labels that already exist, but GitHub's REST API rejects unknown label names — I deliberately did NOT attempt blanket application against the as-yet-uncreated labels.
- **Project board** — the GitHub MCP exposes no Projects v2 tools. If a board exists at `gh project list --owner kirra-systems`, the human can add the REAL-WORK items via `gh project item-add`. If no board exists, recommend creating "KIRRA Occy / Pilot" with columns Todo / In-Progress / Blocked / Done and Priority field {P0, P1, P2}.

## Confirmation

- **Zero issues closed.** Close recommendations in §"READY TO CLOSE" are for human approval only.
- **main untouched.** Triage doc landed on branch `kirra-issue-triage-2026-06-01`; no commits to main.
- **Code untouched.** This triage pass is read-only against the working tree; the only file written is this document.

---

## Resolved after this snapshot (as of 2026-06-11)

Grounded against `main` (each line cites an on-main deliverable). The live tracker
remains authoritative — this is a convenience pointer, not a re-triage. Items that
could not be verified on `main` are omitted rather than asserted.

- **#98** — SG4 water-untraversable veto: `parko/crates/parko-core/src/water.rs` (`water_untraversable_veto`).
- **#99** — flood-condition → FleetPosture coupling: `src/posture_engine.rs:471` (REQ `flood-posture-coupling`; `flood_condition_active`).
- **#102** — richer impact / contact: the SG6 latch `parko/crates/parko-core/src/impact.rs` (`ImpactLatch`); the signed audit bridge **#263** (`parko/crates/parko-kirra/src/audit_sink.rs`, `RecordedImpactLatch`); the derived `vanished_object` (`VanishedObjectDetector`, same `impact.rs`).
- **#103** — clearance loop + reset hardening: `ClearanceLoop` (`impact.rs`, #267); supervisor reset key `KIRRA_SUPERVISOR_RESET_KEY` (#255, `src/`); AoU `AOU-CLEARANCE-AUTH-001` (`docs/safety/ASSUMPTIONS_OF_USE.md`, #268).
- **#107** — SG5 exit-clearance + stop-inside: `parko/crates/parko-core/src/commit_zone.rs` (`exit_clearance_verified`).
- **#108** — SG5 non-yielding-agent clearance: `commit_zone.rs` (`non_yielding_clearance`; re-landed via #269).
- **#117 / #118** — UL4600 safety case / ISO 21434 TARA: `docs/safety/UL4600_SAFETY_CASE.md`; `docs/safety/OCCY_TARA.md`.
- **#122** — occlusion-aware caution (PO-1 G1): `parko/crates/parko-core/src/rss.rs` (`OcclusionScene` / occlusion cap).
- **#123** — localization-integrity gate + AoU: `parko/crates/parko-core/src/localization.rs` (`gate_commit_zone_scene`, #264); `AOU-LOCALIZATION-001` (`ASSUMPTIONS_OF_USE.md`, #265).
- **#126 / #127** — perception / actuation SEooC AoU clauses: `AOU-PERCEPTION-RANGE-001` / `-CLASS-001` / `AOU-ACTUATION-LATENCY-001` in `docs/safety/ASSUMPTIONS_OF_USE.md`.
- **#128** — SG2 drivable-space ENFORCED: see the in-doc closing note above (still accurate); live call at `crates/kirra-ros2-adapter/src/validation.rs:161`. The GitHub issue close is a separate owner action.
- **#136** — angular-velocity SOTIF: `docs/safety/ANGULAR_VELOCITY_SOTIF.md` + the `KIRRA-OCCY-ANGULAR-SOTIF-001` registration in `docs/safety/SAFETY_CASE_INDEX.md` (#266).
- **#146** — runtime-isolation CI guard: `parko/ci/check-runtime-isolation.sh` (wired in `.github/workflows/ci.yml`).
- **#160** — canonical safety-goal ID scheme: closed by-design — `AEGIS-SG-00X` kept; the `src/` cross-reference comments remain (no piecemeal rename).
- **CERT-006** — comparator diversity / divergence audit sink: `parko/crates/parko-kirra/src/audit_sink.rs` (`AuditChainLinkerDivergenceSink`); `docs/safety/COMPARATOR_DIVERSITY.md`.

**New since this snapshot — EPIC #270 (iceoryx2 transport / QNX governor lane):** issues #270–#279; the host spike `tools/iceoryx2-spike/` (#273); the transport decision `docs/adr/0006-governor-transport-iceoryx2.md` (#275); the contract-channel spec `docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md` (#278 design half).
