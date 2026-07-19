# Kirra Systems — Website Redesign Deliverables

This document is the complete record of the 2026 website redesign: the repository audit
that grounds every claim, the strategy, the design system, and the rationale for every
decision. The governing rule, inherited from the codebase itself: **if we can't cite it,
we don't claim it.**

---

## 1. Repository audit

Audited at workspace version **1.1.2** (`Cargo.toml:27`), July 2026. Three parallel deep
audits (CI/testing/benchmarks · safety/certification/docs · hardware/runtime/IPC).

### What Kirra is
A distributed **runtime legitimacy engine and safety governor** (`README.md:6`). The
doer–checker split is the load-bearing thesis: doers (Mick LLM intent, Occy planner, Taj
perception) *propose* typed claims; Kirra *bounds* them at a checker that is the sole
safety authority. Doers are swappable and never trusted.

### Architecture
- Root crate `kirra-verifier` + 36 workspace crates + separate `parko/` ML workspace.
- Layered decomposition (ADR-0035): zero-dep leaves (`kirra-policy-types`, `kirra-core`)
  → checker/doer logic (`kirra-trajectory`, `kirra-planner`, `kirra-map`, `kirra-taj`)
  → integration shells (`kirra-ros2-adapter`, `kirra-inline-governor`) → verifier root.
- Deployment decision of record (ADR-0032): governor as QNX Hypervisor 8.0 safety-domain
  payload; ROS 2/Autoware doer as isolated guest VM; `no_std` verdict core as the
  certification-artifact candidate. **Status: cross-builds reproducible in CI; QNX-target
  execution/timing is the recorded remainder.**

### Safety mechanisms
- Postures Nominal/Degraded/LockedOut; stale-cache (>5 s) fails closed; `Unknown`
  denied in all postures (`src/posture_cache.rs`).
- Degraded = controlled decel-to-stop-and-hold at 4 enforcement points (ADR-0011,
  issue #70); machine-checked by Kani K4/K5.
- Recovery hysteresis: 5 healthy reports / 10 s window; LockedOut exits via operator
  clearance only.
- Gray/black DAG posture traversal with cycle → LockedOut (`kirra_safety_authority::dag`).
- Watchdogs: telemetry 1 s warn / 2 s fault, 100 ms sweeps (`src/telemetry_watchdog.rs`).
- Safe-state spec KIRRA-SSS-001 (Active status), MRC ceiling 5.0 m/s.
- Per-class contract profiles (courier/delivery-AV/robotaxi), no default class — wrong
  class aborts startup (`docs/CONTRACT_PROFILES.md`, normative).

### Middleware / IPC / zero-copy
- Enforced path: POSIX-SHM seqlock over frozen `#[repr(C)]` `GovernorContractView`
  (`kirra-contract-channel`, forbid-unsafe; `kirra-hv-carrier` is the single
  enumerated-unsafe crate). Ed25519 release token over the enforced bytes;
  verify-before-release with strictly-advancing sequence (`kirra-inline-governor`,
  12-row FDIT matrix, cross-process tests). **No HTTP on the enforced path.**
- DDS: fail-closed QoS — Volatile-only actuator topics, QoS readback validation
  (`src/dds_bridge.rs`, `src/dds_cyclonedds.rs` feature-gated).
- iceoryx2: isolated spike (pinned =0.9.2), 3 tiers up to the assembled EP-01 loop over
  zero-copy; 9-class fault matrix passes on full and **empty** feature sets. Explicitly
  not an adoption.
- Zenoh fleet lane (ADR-0007): untrusted carrier, Ed25519 verify-before-use, QM-only.

### Determinism
- `kirra-replay`: bit-identical verdict replay (`f64::to_bits`, no epsilon);
  incomplete-context records classified, never guessed; serde `float_roundtrip`
  load-bearing.
- `VirtualClock`/`ScenarioRunner` injected time; no `SystemTime::now()` in decision paths.
- Loom (4 models), Miri (multi-seed on the seqlock), Kani (12 proofs over `#[path]`-included
  shipped source; talisman blob `ed00f4da…` pinned and format-checked in CI).

### RSS
Primitives in `parko-core/src/rss.rs` (`longitudinal_safe_distance:317`,
`lateral_safe_distance_split:196`, `occlusion_limited_speed:454`,
`opposite_direction_safe_distance:549`); consumed by
`kirra-trajectory/src/validation.rs` (§4 conjunction; `predictive_rss_breach:999`
multi-modal ADR-0017; per-mode fail-closed #824). Occlusion Rule 4 at junctions
(ADR-0016). VRU/pedestrian bound with armed-but-silent → MRC cap.

### Planning / perception / prediction
- Occy geometric planner + Lanelet2-lite `LaneGraph` (Dijkstra, corridors, right-of-way,
  sight distance); `MickIntent` 7-verb vocabulary, one fail-closed parse; local Gemma via
  Ollama; CI actuation fence (`ci/check_mick_actuation_fence.py`).
- Learned planner: ES-fit MLP over trajectory vocabulary (Hydra-MDP-shaped), deterministic
  from seed; teacher-misalignment demonstrable; admissibility floor 0.9032 gated.
- Taj: Phase-A geometric corridor, Phase-B hazard fusion (tighten-only; ForbiddenLoosen
  hard zero); true-redundancy `cross_check:224` → 0.0 m/s cap; kinematic plausibility
  guard WCET-gated.

### Benchmarks / testing / CI (measured)
- **3,305 test functions** (3,075 `#[test]` + 230 `#[tokio::test]`) in **330 files**;
  32 proptest suites; 12 Kani proofs; 4 Loom models; 4 fuzz targets.
- **27 blocking CI lanes** (ci.yml) incl. Miri, Loom, Kani, fuzz, mutation (PR diffs),
  MSRV 1.88, postgres conformance, QNX cross-build with byte-compare reproducibility,
  cargo-deny/audit, clippy -D warnings, ShellCheck, helm lint. Weekly deep fuzz
  (90 min/target) and deep Kani (R2); nightly 40k-sample KPI Monte-Carlo.
- Coverage: gated decision floors kirra-trajectory 78.0 / kirra-core 79.0 / parko-core
  77.0 (measured 80.08/81.15/79.82); Codecov 0.5% anti-regression ratchet. True MC/DC
  toolchain-blocked upstream (#65) — branch coverage gates meanwhile.
- KPI gate: 248 doer scenarios + 71 perception frames; floors incl. unsafe_miss ≤ 0,
  hazard_recall ≥ 1.0, differential hard zeroes; Wilson-bound nightly campaign.
- WCET: target 100 µs, CI ceiling 1,000 µs, p99.9-gated over 100k iters; assembled
  in-line loop ≈65 µs p50 / ≈116 µs p99.9 host-indicative; SHM seqlock read ≈60 ns;
  iceoryx2 309 ns vs UDP+serde 976 ns; Orin NX isolated-core iox2 RTT 1.4 µs p50 with
  a documented ~47–51 ms Linux scheduling tail (the argument for QNX-under-FIFO).
  **No certified WCET numbers exist — repository is explicit.**

### Security
Constant-time comparisons; fail-closed admin gate (absent token → 503); per-node Ed25519
challenge-response attestation + TPM2 quote w/ PCR16 binding; scoped RBAC (4 roles);
mTLS cert principals with expiry fail-close; SHA-256 hash-chained audit surviving SIGKILL
(CI drill) + WORM shipping + independent re-verification; Ed25519 federation with nonce
burn; explainable verdict artifacts (EP-17); release-token key provisioning fail-closed;
supply chain: SHA-pinned actions, digest-pinned images, cosign keyless signing,
cargo-deny/audit gates.

### Certification readiness (honest)
The claim of record (`README.md:117`): *designed in alignment with ISO 26262 ASIL-D and
IEC 61508 SIL 3; independent third-party assessment has not yet been performed.*
65 artifacts in `docs/safety/` — HARA, UL 4600 GSN, SOTIF (ODD §1 signed off), TARA,
SIL3/TR-5469/ASTM F3269 mappings — **all explicitly Draft/pre-assessment** except the
Active safe-state spec and the CI-gated RTM. PMHF: single-supply 17.7 FIT (fail) →
dual-supply 8.7 FIT (pass) is a deployment requirement, in the open.

### Hardware / OS
- **Real:** Yahboom ROSMASTER R2 (Ackermann) + Jetson Orin NX 16 GB; first governed
  motion validated 2026-07; TG30 lidar live. Clean-room STM32F103 firmware (CI-built;
  explicitly not a flashable-image claim). App-level OTA A/B live; `nvbootctrl` rootfs
  controller code real, drill documented. Known limitation recorded: drive+steer together
  blocked on vendor R2 image (`HARDWARE_FINDINGS_R2X3.md`).
- **OS:** Linux (Ubuntu/JetPack) primary; Docker/Helm/systemd deploys; QNX 8.0 =
  certification target (x86_64 VM harness with committed results; Jetson never runs the
  cert target — AOU-HW-QNX-TARGET-001).

---

## 2. Competitive analysis

Kirra is a **governed layer**, not an end-to-end driving stack. Comparisons are made only
where the repository provides evidence; no superiority is fabricated.

| vs | Evidence-supported position |
|---|---|
| **Autoware** | ADR-0036 keeps Autoware as the doer, isolated in its own container; Kirra does **not** reimplement localization/closed-loop control (gap table is explicit: 🔴 major gaps). Kirra-unique per the same analysis: formal fail-closed command bounding (RSS conjunction, occlusion caps, decel-to-stop) — "Autoware has no equivalent." |
| **Mobileye (RSS)** | RSS is Mobileye-published theory; Kirra implements it in open Rust with the §4 conjunction proof note, property tests, and Kani proofs R1–R3. Differentiator is *auditability*, not algorithmic novelty. |
| **NVIDIA DRIVE** | Complementary: the certified-WCET target is DRIVE-class hardware running QNX for Safety. Jetson is the doer/dev rig only. No benchmark claims against DRIVE middleware. |
| **Waymo / Wayve / Apollo** | Closed or end-to-end learned stacks; different problem (driving vs governing). Kirra's doer-invariant safety case (ADR-0020) is the honest contrast: the safety argument survives swapping the doer. No measurable comparison exists, so none is published. |
| **ROS-centric architectures** | Concrete, citable differences: enforced path avoids middleware entirely (seqlock SHM + signature over exact bytes); DDS QoS fail-closed (Volatile-only actuator topics; readback validation); LLM crates fenced from actuation at compile-graph level. |

Unique, evidenced advantages: (1) fail-closed posture routing with typed reasons;
(2) bit-identical incident replay; (3) proofs run against shipped source with a frozen,
hash-pinned checker core; (4) hash-chained kill-tested audit; (5) hard-zero safety KPIs
gating merges; (6) the honesty discipline itself (draft means draft) — rare in this
industry and verifiable in-repo.

---

## 3. Website information architecture

```
/                     Home — thesis, interactive posture pipeline, life-of-a-command,
                      enforced path, assurance dashboard, platform grid, hardware,
                      certification band, field notes
/architecture         Topology (ADR-0032), crate layers, transport lanes, positioning
/runtime              Posture engine, HA + epoch fencing, persistence, OTA, exec model
/safety               Posture state machine, Degraded contract, watchdogs, class
                      envelopes, invariants
/planning /perception /prediction     The doer-side + checker bounds per domain
/determinism          Replay, injected clocks, Loom/Miri, Kani inventory
/performance          Budgets vs gates vs measurements; transport latencies; tuning
/security             Attestation, RBAC, audit, federation, supply chain
/certification        Artifact status table (honest), traceability, evidence program
/benchmarks           Every number with provenance (KPI, coverage, latency, tests)
/open-source          Quickstart, deployment paths, contribution gates
/documentation        Curated map into the repo docs
/research             Labeled research threads (diverse governors, Rabbit, parko, iox2)
/field-notes          Substack feed (client-side, 3-tier fallback preserved)
/careers              Culture-as-evidence; "the interview is a pull request"
+ 404, sitemap.xml, robots.txt
```
Primary nav: 5 links + GitHub CTA + theme toggle. A sticky sub-nav chip strip provides
lateral movement across all 14 technology pages. The footer is the full sitemap.

## 4. UX strategy

- **Evidence chips** are the signature interaction: every claim carries `▸ file:line`
  chips deep-linking to GitHub. The audit trail *is* the brand.
- **Show the system behaving:** the posture switch renders the real
  `should_route_command` outcomes; the timeline walks one command through 10 real
  functions; diagrams carry flow animation with deny paths visible.
- **Honesty as UI:** status pills (LIVE / HARDWARE-GATED / SPIKE / DRAFT) appear wherever
  maturity varies; host-indicative numbers are labeled at the point of display.
- Progressive enhancement: fully readable with JS off; interactions are ARIA-complete
  (tabs, switches, live regions); `prefers-reduced-motion` collapses all animation.

## 5. Visual design system

- **Tokens:** dark default (`#05070d` bg, `#3fd9c9` signal cyan, slate inks) and a
  first-class light theme (`#f6f8fb` paper, `#0a7466` accent). All text tokens verified
  ≥ 4.5:1 (measured 5.3–11.5:1).
- **Type:** Space Grotesk (display) / Inter (body) / JetBrains Mono (data & evidence) —
  self-hosted variable fonts, latin subset, 102 KB total, preloaded.
- **Charts:** validated categorical palettes (six-check CVD validator, both modes):
  dark `#16A99A #8A76F0 #C1832A #3E86DE`, light `#0B8D7D #6D5BD0 #B0731A #2569C3`;
  thin marks, direct value labels, single axis, no dual-scale charts.
- **Motion:** WebGL hero (hand-written shader — node lattice + verification scanline +
  absorbed deny pulses), IntersectionObserver reveals with staggering, animated SVG flow
  edges, count-up stat tiles. DPR-capped, offscreen-paused, reduced-motion static.
- **Diagrams:** one SVG vocabulary (`dg-*` classes) for boxes/edges/deny paths so every
  page's diagrams read as one system, theme-aware via CSS variables.

## 6. Brand language

Voice: **confident, technical, minimal; the humility is load-bearing.** Patterns used:
short declarative headlines ("Fail closed. Recover deliberately."), claims followed by
their falsifier ("Clone it… flip one bit — watch the replay tool refuse it"), and the
explicit refusal to overclaim ("The strongest claim we make is the one we don't").
Banned: unbacked superlatives, "certified" in any unqualified form, comparisons without
measurements, buzzwords without implementation behind them.

## 7. Implementation plan (as executed)

1. Audit (3 parallel research agents) → this document's §1.
2. Design system: `css/kirra.css` tokens/components; palettes validated.
3. Behavior layer: `js/kirra.js` (theme, nav, reveal, counters, tabs, posture demo,
   copy, notes feed) + `js/hero.js` (WebGL) — zero dependencies.
4. Authoring pipeline: `_src/template.mjs` + `_src/pages/*.mjs` + `_src/generate.mjs`;
   committed static output (GitHub Pages needs no build).
5. 17 pages + 404 + sitemap + robots + OG image + adaptive favicon.
6. Headless verification: all pages rendered in Chromium, both themes, interactions
   exercised, zero broken internal links, zero non-external console errors.

## 8. Production-ready code

`website/` in this repository: 17 static HTML pages, `css/kirra.css`, `js/kirra.js`,
`js/hero.js`, self-hosted fonts, generator sources under `_src/`. Editing workflow:
change a page module → `node website/_src/generate.mjs` → commit both.

## 9. Migration steps

1. Merge this branch; the existing `deploy-pages.yml` publishes `website/` unchanged
   (paths and Substack `posts.json` bake-step already match).
2. `CNAME` (kirrasystems.com) preserved; old single-page anchors map to the new pages
   (Pages has no server redirects; the 404 routes users home).
3. Removed legacy CDN dependencies (unpkg Three.js, cloudflare GSAP, Google Fonts) —
   nothing external loads except the optional Substack feed fallbacks.
4. Rollback: `git revert` of this branch restores the previous site exactly.

## 10. SEO strategy

Unique title/meta description per page; canonical URLs; Open Graph + Twitter cards with
a generated 1200×630 brand image; JSON-LD (`Organization`, `WebSite`,
`SoftwareSourceCode` on home; `WebPage` elsewhere); `sitemap.xml` + `robots.txt`;
semantic landmarks and one `h1` per page; descriptive internal anchor text; fast static
HTML (no client-side rendering of content) so every claim is crawlable.

## 11. Performance report

- No external render-blocking resources; fonts self-hosted (102 KB woff2 total,
  preloaded, `font-display: swap` with metric-compatible fallbacks).
- Page weight: HTML 10–53 KB; CSS ~24 KB; JS ~19 KB total, `defer`-loaded; single-request
  WebGL hero with DPR cap 1.5, `low-power` context, offscreen/hidden pause.
- Zero CLS by construction: no client-rendered layout content, explicit dimensions on
  images/canvas, theme set before first paint (no flash), reveal animations are
  transform/opacity-only.
- Static files on GitHub Pages' CDN edge; long-cacheable immutable assets.
- The only third-party requests are the Field Notes feed fallbacks, which degrade to a
  static CTA.

## 12. Accessibility report (AA+)

- Contrast: all text tokens ≥ 4.5:1 in both themes (measured; most ≥ 5.3:1); chart
  palettes pass CVD separation validation; status/verdicts never color-alone (shape +
  text always).
- Structure: skip link, landmarks, single h1, ordered heading levels, `aria-current`
  nav state, table captions, figure alt/`<desc>` on every SVG diagram.
- Interaction: real `<button>`s, full keyboard tab/arrow support on tabs, visible
  `:focus-visible` everywhere, `aria-pressed`/`aria-live` on the posture demo,
  44px-class touch targets in mobile nav.
- Motion: `prefers-reduced-motion` disables the shader loop (static frame), reveals,
  flow animations, and counters.
- Theme: honors `prefers-color-scheme`, persists explicit choice, `color-scheme` set for
  correct UA chrome.

## 13. Design decisions ↔ repository evidence

| Decision | Tied to |
|---|---|
| Build-free static site (no Next.js/server components) | `deploy-pages.yml` uploads `website/` verbatim on GitHub Pages — a runtime framework would add a build step, a supply chain, and zero benefit for fully-static content. "Server components where appropriate" resolved to: none are appropriate on this host; the authoring-time generator provides the componentization instead. Minimal attack surface also mirrors the project's own supply-chain posture (SHA-pinned actions, digest-pinned images). |
| Evidence chips on every claim | The repo's own culture: RTM stub policy ("never falsify coverage"), README's assessment disclaimer. The site adopts the same epistemics. |
| "The AI proposes. Kirra bounds it." | `README.md:16-28` doer/checker thesis, verbatim concept. |
| Interactive posture table | Real `should_route_command` semantics incl. Unknown-always-denied (INVARIANT #9) and posture-exempt reads (Bug 2 rationale). |
| Deny-styled 404 | `classify_http_command` fail-closed allowlist — the brand behavior applied to the site itself. |
| Latency ladder + budget bars labeled "host-indicative" | `src/wcet_gate.rs` "HOST-SIDE, INDICATIVE… NOT certified WCET"; `kirra-timing`'s refusal to mint qnx-target numbers from a host clock. |
| Status pills (LIVE/SPIKE/DRAFT/GATED) | Direct mirror of repo labels: iceoryx2 "spike, not adoption", parko "not yet production", safety docs "Draft — pending review", QNX "recorded remainder". |
| Certification page leads with the disclaimer | `README.md:117` — the strongest trust move available is repeating it prominently. |
| Dark-first with signal cyan | Existing brand mark (cyan node-graph K) and `theme-color #04060c` from the prior site — continuity, not reinvention. |
| Careers page has no job listings | No roles exist in the repository; inventing them would violate the no-fabrication rule. Culture claims cite commit-visible practices instead. |
| Field Notes keeps the 3-tier feed fallback | Preserved from the prior implementation because its resilience design (Substack 403s from datacenter IPs) is documented in the old `notes.js` and the deploy workflow. |
| Preloader ported from the old site | The "VERIFYING FLEET POSTURE" boot sequence was the prior site's most on-brand moment. Rebuilt dependency-free: homepage only, once per session, ~1.3 s, skipped under `prefers-reduced-motion`, armed only when JS runs (can never block a no-JS visit), 3 s hard failsafe. |
