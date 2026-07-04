# Gap-Closure Status & Plan

**Basis:** `INDUSTRY_BENCHMARK_GAP_ANALYSIS.md` (G1–G20, 2026-07-02), executed per
`docs/roadmap/PRODUCT_EXECUTION_PLAN.md` (WS-0…WS-7, Gates A–E).
**Method:** every status below is verified against the code at the stated date,
not inferred from plans. Update this file when a gap's state changes; each row
names its evidence.
**Last verified:** 2026-07-04.

## 1. Completed (evidence in code / merged PRs)

| Gap | What closed it | Evidence |
|---|---|---|
| G2 parko live-RSS wiring | `apply_object_rss_gate`; `safe:true` default removed | `unfed_governor_tick_holds_at_zero` |
| G4 certified-artifact pipeline | reproducible, provenance-complete, signed QNX judge | #790/#800; release.yml double-build `cmp` gate |
| G8 inference deadline | scheduler deadline-breach accumulator + posture escalation | #773 (Fix-PR F) |
| G9 scenario-KPI gate | 248-scenario corpus, thresholds enforcing in CI | `kirra-kpi-gate` + `scenario_kpi_thresholds.json` |
| G10 persistence integrity | generation persistence live; incident writes `synchronous=FULL` | Fix-PRs C/D (#771/#772 follow-ups) |
| G11 supply chain | SBOM (cyclonedx) + cargo-deny + cosign + SHA-pinned actions | release.yml / ci.yml / `deny.toml` |
| G12 pedestrian RSS | VRU reachable-set bound + D2 wiring + go-live hardening | #788, #799; `PEDESTRIAN_RSS.md` |
| G16 model integrity | SHA-256 allow-list at model load (CPU + TRT via one seam) | #801; `parko/MODEL_INTEGRITY.md` |
| G17 observability | `/metrics` mounted; fleet-safety Prometheus series | WS-0.5 (#774) |
| G20 (partial) | CHANGELOG + semver/MSRV/deprecation policy | WS-0.7 |
| **G7 key/identity (WS-1, core)** | **Unified** per-principal API tokens: DB-backed `api_principals` (SHA-256 at rest, mint/revoke), 4-role × scope RBAC, admin-action attribution into the signed chain, transport-security gate | #802–#805, #807, #808 (unification, merged); `PRINCIPAL_TOKENS.md`, `TRANSPORT_SECURITY.md` |

**Gate A (WS-0, "code tells the truth") is closed.** The parallel-implementation
episode note: WS-1 was built twice (env registry #802/#803 vs the DB-backed
advisor branch); the unification kept the DB-backed engine and retired the env
registry — see the CHANGELOG Unification note.

## 2. Remaining — sequenced plan

### Track 1 — finish WS-1 (Stage 1, blocks Gate B)
1. **TPM-bind the release-token signing key.** Verified: *no production
   provisioning path exists* — the only signers are the l3-e2e demo harness and
   tests; `tss2-esys` is absent in the dev environment. Plan: a fail-closed
   key-provisioning seam (file source with permission checks + zeroization;
   explicit dev-mode; absent → refuse) with a TPM-unseal source deferred until
   tss2 libs + hardware land (named external dependency). Effort M.
2. **In-process TLS termination + mTLS client-cert → principal identity.**
   Mesh-enforcement gate is live (#805); this removes its trusted-proxy AoU.
   Serve-path change + rustls provider selection + live-handshake test. Effort M.

### Track 2 — WS-2 P1 surface remainders (Stage 1, blocks Gate B)
3. **G15 learning-loop capture** — §3 capture-point decision still OPEN in
   `LEARNING_LOOP_ARCHITECTURE.md`; verdict-emit machinery exists. Effort M.
4. **G18 config governance** — `KirraRuntimeConfig` exists; no schema/versioning;
   env sprawl remains. Effort M.
5. **G20 SDK remainder** — 2 Python examples only; `kirra.h` is a 20-line stub;
   no Rust/C examples or published rustdoc. Effort S–M.

### Track 3 — WS-4 fleet plane (Stage 2, blocks Gate C)
6. **G6 secure/measured boot** — TPM feature off-by-default; needs a reference
   platform. Effort L (hardware-gated).
7. **G14 zenoh TLS/QUIC** — deliberately disabled in `kirra-fleet-transport`;
   enabling is a toolchain fight. Effort M.
8. **G3 OTA/A-B/rollback** — verified absent from code. Narrow scope first:
   dual-slot governor artifact + health-gated rollback. Effort L→XL.

### Track 4 — WS-3/WS-5 evidence engine (Stage 3, blocks Gate D)
9. **G5 evidence execution** — RTM test-level at 20.0% (8/40) per
   `RTM_GAP_REPORT.md`; drive 20→60→90%, MC/DC on kernel crates. Effort L,
   incremental.
10. **G19 SOTIF catalog** — two targeted docs exist; no systematic trigger
    catalog. Effort L.
11. **WS-5.2/5.3** — Ferrocene build; certified-hardware WCET + hypervisor fault
    campaign (license/hardware-blocked; named external dependencies).

### Track 5 — WS-6 strategic fork (Stage 4)
12. **G1 perception** — detector still `MockSemanticDetector`; the
    build-vs-partner-AoU fork is a business decision before an engineering one. XL.
13. **G13 map lifecycle** — same fork. XL.

## 3. Out of scope of this plan

The `work/roadmap.md` / `work/backlog.md` PARK-0xx items (parko hardware
backends: TensorRT/QNN/TIDL/OpenVINO/AMD, QNX bring-up, reference robot) are the
separate hardware-gated track; they resume when the named hardware lands.
