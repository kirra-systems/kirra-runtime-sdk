# ADR-0042 (WM-6a): Clarify World Model terminology and safety-boundary scope

| Field | Value |
|---|---|
| Status | **Proposed — NOT ratified on merge.** Merging records the clarification; it ratifies nothing and authorizes no implementation. |
| Date | 2026-08-02 |
| Clarifies | [`ADR-0039`](0039-world-model-bidirectional-governor-fence.md) (WM-6) · [`ADR-0040`](0040-world-model-ownership-and-boundary.md) (WM-1) · [`ADR-0041`](0041-world-model-persistence-architecture.md) (WM-2) |
| Blueprint | `KIRRA-WM-ARCH-001` — [`docs/design/WORLD_MODEL_ARCHITECTURE.md`](../design/WORLD_MODEL_ARCHITECTURE.md) |
| Deciders | Architecture owner · World Model owner · **safety-assurance owner** (Decision 5) |
| Cross-refs | [`crates/kirra-trajectory/src/perception_redundancy.rs`](../../crates/kirra-trajectory/src/perception_redundancy.rs) · [`crates/kirra-core/src/corridor.rs`](../../crates/kirra-core/src/corridor.rs) · [`robot/world_model.py`](../../robot/world_model.py) · [`ci/check_mick_actuation_fence.py`](../../ci/check_mick_actuation_fence.py) |

> **This ADR does not ratify WM-1, WM-2 or WM-6.** They remain Proposed. This
> resolves five blockers found during review of #1306 so that WM-6 can later be
> *considered* for acceptance.
>
> **Decision 5's safety-assurance ruling is still `PENDING`**, and when it is
> made it will be an **owner self-assessment, not an independent assurance
> review** — see *Independence posture*. Kirra is designed in alignment with
> ISO 26262 ASIL-D requirements and IEC 61508 SIL 3 requirements; independent
> third-party assessment has not yet been performed.

---

## Context

Review of #1306 exposed five issues that must be settled before the foundational
World Model ADRs can move toward acceptance:

1. **"World model" already means something else** in the safety code.
2. **Kirra World includes maps**, while the checker consumes a map-derived
   `CorridorSource`.
3. **Fence B was stated over named crates**, not the transitive closure.
4. **The fence was Rust-shaped**, while the verifying consumer is Python.
5. **The out-of-scope assurance claim was asserted**, not ruled.

Each is addressed below as a numbered decision.

---

# Decision 1 — Canonical terminology

## The live collision

Measured, not assumed:

| Location | Uses "world model" to mean |
|---|---|
| [`crates/kirra-trajectory/src/perception_redundancy.rs:4`](../../crates/kirra-trajectory/src/perception_redundancy.rs) | *"two INDEPENDENT world models (camera-only vs. …)"* — redundant **perception channels** |
| [`crates/kirra-trajectory/src/perception_redundancy.rs:156`](../../crates/kirra-trajectory/src/perception_redundancy.rs) | *"redundant world model has been untrustworthy…"* — same sense |
| [`crates/kirra-ros2-adapter/src/node.rs:375`](../../crates/kirra-ros2-adapter/src/node.rs) | *"a camera-only world model vs the primary radar+lidar"* — same sense |
| [`robot/world_model.py`](../../robot/world_model.py) | A TTL'd operator-facing **read projection** |
| `KIRRA-WM-ARCH-001` | The proposed **semantic evidence subsystem** |

Two of these are inside the safety closure. The ambiguity is unacceptable in
architecture, safety, incident and assurance discussion, where "the world model
was wrong" must not be able to mean either a perception channel fault or a
semantic knowledge fault.

## Options evaluated

**Semantic subsystem:** `Kirra World` · Kirra Semantic World Model · Kirra
Knowledge Model · Kirra Evidence Model · Kirra World State.

**Perception concept:** `independent perception channel` · perception
hypothesis · perception representation · perception model · sensor-fusion
channel.

*Kirra Knowledge Model* and *Kirra Evidence Model* describe the content
accurately but lose the product framing already established in the blueprint and
in `VISION.md`. *Perception model* was rejected because "model" is the
overloaded word doing the damage.

## Decision

| Canonical term | Use for |
|---|---|
| **Kirra World** | The product / subsystem name for the semantic evidence architecture |
| **semantic world model** | The generic descriptive phrase, when a common noun is needed |
| **independent perception channel** | The existing redundant perception inputs |
| **perception hypothesis** | One channel's interpretation, where channel-specific |

**Bare "world model" is not canonical anywhere.** New documents must qualify it
(*semantic* world model, or an *independent perception channel*) or name the
subsystem (**Kirra World**).

## Safety relevance table

| Term | Meaning | Safety relevance |
|---|---|---|
| **Kirra World** | Semantic evidence subsystem | **Non-authoritative** for safety decisions |
| **Independent perception channel** | Direct safety/perception input channel | **May be authoritative** under its owning safety contract |
| **Derived projection** | Rebuildable semantic view | **Not a live safety input** |
| **Corridor source** | Checker-owned geometric safety input | **Authoritative only under its owning contract** |

## No renames in this PR

Source symbols are unchanged. This ADR establishes the glossary and records the
migration intent; see *Migration checklist*.

---

# Decision 2 — Semantic map vs safety corridor

"Map" means two different things and the two must never be substituted.

## Kirra World may contain (semantic)

place names · semantic regions · aliases · destination references · route labels
· operator annotations · map IDs · frame IDs · historical spatial observations ·
imported semantic metadata.

## The safety path independently owns (authoritative)

corridor geometry · drivable-space bounds · lane/lanelet geometry used by the
checker · localization used by the checker · live object inputs used by the
checker · freshness and watchdog state · configured safety margins ·
authoritative kinematic inputs.

## The rule

> A semantic place or route from **Kirra World** may help an **untrusted doer**
> choose a goal.
>
> It must **not** become the checker's authoritative corridor, localization, or
> live-object input merely because it exists in Kirra World.

## The hidden-adapter prohibition

The concrete failure:

```rust
impl CorridorSource for WorldModelCorridor { … }   // FORBIDDEN
```

**Why a crate-name-only scan would miss it.** `CorridorSource` is a trait
declared in [`crates/kirra-core/src/corridor.rs`](../../crates/kirra-core/src/corridor.rs);
the checker takes `&dyn CorridorSource` and never fetches a corridor. An
implementation can therefore live in *any* crate. Scanning
`crates/kirra-trajectory`'s dependencies for the name `kirra-world` would find
nothing, because the dependency is inverted — the semantic layer would be
*supplying* the checker, not being called by it.

**This is not hypothetical.** A `CorridorSource` implementation already lives in
a doer-side crate today:

```
crates/kirra-sidecars/src/planner.rs:584   impl CorridorSource for ReqCorridor
```

`ReqCorridor` is populated from the plan request's own boundaries, which is
legitimate — but it demonstrates the exact shape: a product-side crate supplying
a value the checker consumes as authoritative geometry.

### The rule

> **Any implementation of a safety-authoritative input trait must prove
> independence from Kirra World, not merely avoid importing a crate with that
> name.**

Known safety-authoritative input traits, from investigation:

| Trait | Declared in | Implementors today |
|---|---|---|
| `CorridorSource` | `kirra-core` | `kirra-core` (mock), `kirra-ros2-adapter` (Lanelet2), `kirra-map`, `kirra-taj`, `kirra-sidecars` (`ReqCorridor`), test doubles |

The list is not closed; new authoritative-input traits inherit the rule.

## Three-way distinction — what is *not* prohibited

Not all sharing is a fence breach. Distinguish:

| Kind | Example | Permitted? |
|---|---|---|
| **Shared source artifact** | Both load the same approved Lanelet2 map file, each validating it under its own contract | ✅ **Permitted** — with an architecture allowlist entry |
| **Shared derived semantic projection** | The checker consumes a corridor that Kirra World computed from its entities | ❌ **Prohibited** |
| **Runtime query dependency** | The checker calls a Kirra World query API during the safety decision | ❌ **Prohibited** |

> A safety consumer **may** independently load and validate an approved map
> artifact under its own contract. It **must not** query a semantic projection
> and treat the answer as authoritative.

The distinction is *who validated it*: independent validation of a common
artifact preserves independence; consuming another subsystem's derived view does
not. Dependent-failure implications of the shared artifact itself are a question
for the Decision 5 ruling (item 3).

---

# Decision 3 — Fence B is transitive

ADR-0039 stated Fence B over named crates. **A direct-import check is
insufficient**: a shared crate one hop away can become a transit path.

## The rule

> **No component reachable from the safety decision or authorization path may
> depend on Kirra World, its service API, its database, its semantic
> projections, or adapters that expose those projections as authoritative safety
> inputs.**

## Measured closure

Computed from the workspace manifests at `922a0d0c` — transitive over normal
`[dependencies]` and `[target.*.dependencies]`:

| Crate | Workspace dependencies |
|---|---|
| `kirra-release-token` | `kirra-contract-channel` |
| `kirra-actuation-consumer` | `kirra-release-token` |
| `kirra-inline-governor` | `kirra-contract-channel`, `kirra-core`, `kirra-release-token`, `kirra-hv-carrier` |
| `kirra-trajectory` | `kirra-core`, `parko-core` |
| `kirra-safety-authority` | `kirra-core`, `kirra-audit-hash` |
| `kirra-hv-carrier` | `kirra-contract-channel` |
| `kirra-consumer-ffi` | `kirra-actuation-consumer`, `kirra-release-token`, `kirra-r2cp` |
| `kirra-core` | `kirra-contract-channel`, `kirra-capture-schema` |
| `kirra-contract-channel` | *(leaf)* |
| `kirra-capture-schema` | *(leaf)* |
| `kirra-audit-hash` | *(leaf)* |
| `kirra-r2cp` | *(leaf)* |

**Closure: 12 workspace crates.** All are in Fence B scope, not the 7 roots
alone.

> **Now enforced, from a wider root set.**
> [`ci/check_kirra_world_bidirectional_fence.py`](../../ci/check_kirra_world_bidirectional_fence.py)
> implements this rule and measures the closure at **19 workspace crates from 10
> roots** — it additionally roots at `kirra-verifier`, `kirra-persistence`,
> `kirra-policy-types` and `kirra-ros2-adapter`. The table above remains correct
> for the 7 roots it names; the checker's wider set is the one enforced. The
> enforced closure was cross-checked against `cargo metadata` and agrees exactly.
> Dev-dependency edges are deliberately excluded from the closure — the safety
> roots test-harness against the doer crates, which are precisely the ones that
> may legitimately depend on Kirra World — while a *direct* dev edge from a
> safety root onto `kirra-world*` is still refused. See ADR-0039
> §*Structural enforcement — IMPLEMENTED* for what the checker does and does not
> prove.

## Treatment of `kirra-core`

`kirra-core` is **inside the closure** — reached by `kirra-trajectory`,
`kirra-inline-governor` and `kirra-safety-authority`. It is also the natural
home for shared lean types, which makes it the most likely accidental transit
path in the entire repository.

**Rule adopted: strict no-dependency for shared crates inside the closure.**

`kirra-core` (and every other closure member) **must not** depend on Kirra World
under any feature, including optional and dev-dependencies that a fenced root
would link.

Alternatives considered and rejected:

- *Split neutral primitives into a lower-level crate* — plausible later, but
  adds a crate today to solve a problem that does not yet exist.
- *Feature separation with a CI proof that safety builds exclude semantic
  dependencies* — feature unification across a workspace makes this fragile;
  a single consumer enabling the feature re-links it.

The strict rule is the simplest enforceable one and matches the existing
actuation fence's posture. If a genuine shared-primitive need arises, the split
is the escape hatch — via a superseding ADR.

## Enforcement layers (specified, not implemented)

| # | Layer | Catches |
|---|---|---|
| 1 | Cargo dependency-closure check | Declared crate edges, transitively |
| 2 | Forbidden service endpoint / configuration reference scan | `KIRRA_WORLD_*_URL`-class runtime coupling |
| 3 | **Trait-implementation ownership check** | The hidden adapter (Decision 2) |
| 4 | Database-path / configuration scan | Shared store file access |
| 5 | Architecture allowlist for independently validated shared artifacts | Legitimate shared map files — an explicit, reviewed list |
| 6 | Review gate for newly introduced safety input adapters | Anything the static checks cannot see |

Layers 3 and 5 are the ones a conventional dependency scan lacks, and they are
the reason this ADR exists.

---

# Decision 4 — The fence is language-independent

The architecture rule applies **regardless of implementation language**. The
current fence is Rust-only, and the verifying consumer is Python.

## Non-Rust paths inventoried

| Path | Evidence |
|---|---|
| **Python verifying consumer** | `robot/kirra_ffi.py`, `robot/bound_consumer_test.py`, `robot/consumer_config_contract_test.py` — ctypes into `libkirra_consumer_ffi.so` |
| **Python read projection** | [`robot/world_model.py`](../../robot/world_model.py) |
| **Shell / install wiring** | `robot/install/install_robot_units.sh` |
| **systemd** | `ExecStart`, `Environment=`, `EnvironmentFile=` across `deploy/systemd/*.service` |
| **Local HTTP clients** | Service URLs are environment-configured: `KIRRA_VERIFIER_URL`, `KIRRA_TAJ_URL`, `KIRRA_MICK_URL`, `KIRRA_MICK_CHAT_URL`, `KIRRA_POSTURE_STREAM_URL`, `KIRRA_DB_URL`, … |
| **ROS / DDS topic bridges** | `kirra-ros2-adapter`, R2 topology (ADR-0033) |
| **Generated adapters** | FFI surface (`kirra-consumer-ffi`) |

The environment-variable surface is the important finding: a safety process could
acquire a Kirra World dependency **through configuration alone**, with no source
change and nothing for a dependency graph to see.

## Planned checks (specified, not implemented)

**Rust** — Cargo metadata dependency graph · AST / source-symbol checks · trait
implementation checks · feature-resolution checks.

**Python** — **AST-based**, not substring: `import` graph · HTTP endpoint
constants · database paths · ROS publisher construction · `cmd_vel` topic
strings · release-token client calls · actuation-consumer calls.

> Naive scans must be avoided: this repository's own documents and tests
> legitimately *describe* the boundary. The existing actuation fence already
> strips comments for exactly this reason, and the chat separation test builds
> its forbidden tokens by concatenation so the test file cannot match itself.
> Any new check inherits that discipline.

**Shell / systemd / configuration** — `ExecStart` · `Environment` ·
`EnvironmentFile` · service URLs · database paths · ROS topic names · binary
wiring.

**Runtime topology** — where static proof is insufficient, deployment
verification showing: no safety service calls Kirra World; no Kirra World
process publishes to actuation topics; no shared database file; no World Model
endpoint present in a safety process's environment.

**None of these is implemented in this PR.** See *Planned enforcement work*.

---

# Decision 5 — The safety-assurance ruling is pending

ADR-0039 stated that Kirra World is out of the safety-assurance scope. **That
was asserted, not ruled.** It is corrected here.

## The proposed argument (not a conclusion)

> **If Fence A and Fence B hold, Kirra World is *intended* to remain outside the
> safety decision and authorization scope.**

**This requires an explicit ruling from the safety-assurance owner.** Until that
ruling is recorded, no document may state the scope determination as settled.

## Questions the ruling must address

1. Is the **absence of a runtime dependency sufficient**, or is more required?
2. Can **semantic goal selection influence a safety goal indirectly** — e.g. by
   systematically steering the doer toward the envelope's edge?
3. Do **common-source artifacts** (a map file loaded by both) create
   dependent-failure concerns?
4. Does Kirra World affect **ODD assumptions**?
5. Are **incorrect semantic goals fully bounded** by the checker, for every
   hazardous outcome — or only for those the checker's inputs can observe?
6. Are the **checker's independent inputs adequate** for all hazardous outcomes?
7. Is Kirra World **QM**, safety-related but non-authoritative, or another
   classification?
8. What **evidence is required** to preserve that classification over time?

Question 5 is the sharpest: the checker bounds *trajectories*, and a semantic
error that produces a legal trajectory to a wrong-but-reachable place is bounded
kinematically while still being operationally wrong. Whether that is a safety
concern or an availability concern is precisely the owner's call.

## The merge-blocking rule

> **No Kirra World domain implementation may merge until this ruling is
> assigned to a named owner and recorded.**

This is enforced, not requested:
[`ci/check_world_domain_logic_gate.py`](../../ci/check_world_domain_logic_gate.py)
reads the record below and, while it is unrecorded, requires the `kirra-world*`
crates to contain declarations only — no function, no hand-written `impl`, no
struct field beyond the unconstructible unit placeholder, no enum variant
carrying data, no `const` or `static`, no macro, and no dependency of any kind.

**Why it is a gate and not a paragraph.** The risk at this point is
organizational rather than technical. The ten placeholder types in
`crates/kirra-world` are unconstructible on purpose, and their emptiness is not
an unfinished state — it is holding the shape open while this decision is still
being made. But an empty crate reads as an invitation. With the ruling still
open some weeks from now, *"just the `EntityId` newtype, it is obviously a
`u128`"* is an entirely reasonable-sounding pull request, and the reasoning that
makes it sound reasonable is precisely the reasoning this ruling was supposed to
perform. Prose does not run in CI; the person with the deadline wins that
argument every time.

**What the gate does not claim.** It cannot stop a determined bypass, and it is
not trying to — the record below is text in a document and anyone may edit it.
That is correct, because the edit *is* the ruling and it belongs in review. What
changes is the failure mode: without the gate the guardrail erodes silently, one
defensible type at a time, with nobody ever deciding to bypass anything. With
it, proceeding requires editing a named field in a named ADR, in a diff a
reviewer sees, with an owner's name attached.

The gate is **self-releasing**: recording the ruling relaxes it automatically
and it reports the owner and classification thereafter. Nobody has to delete it
to make progress, because a gate that must be deleted gets deleted wholesale —
taking its checks with it — rather than satisfied.

## Decision record — to be completed by the owner

Machine-read by the gate above. `UNASSIGNED` in any field, or a placeholder such
as `TBD` or the role name `the safety-assurance owner`, counts as unrecorded: a
role cannot be accountable for a decision, which is the point of naming a
person.

**Ownership is assigned; the ruling is not made.** These are separate states and
the gate now reports them separately. An owner exists and is accountable for
producing the decision — that is real progress and is visible in CI — but the
decision itself has not been taken, so the gate continues to hold and
`kirra-world*` stays declaration-only. The remaining `UNASSIGNED` fields are the
ruling, not the ownership.

```
Safety-assurance ruling: PENDING

Owner: Justin Looney
Owner assigned: 2026-08-03
Date: UNASSIGNED
Scope classification: UNASSIGNED
Rationale: UNASSIGNED
Assumptions: UNASSIGNED
Required evidence: UNASSIGNED
Conditions that reopen the decision: UNASSIGNED
```

**Merging this PR does not constitute approval.** The ruling is not invented
here, and no classification is claimed.

## Independence posture — recorded 2026-08-04

The record above says *who* will rule and *that* they have not. It does not say
what kind of assessment the eventual ruling will be, and a reader six months
from now — or an assessor — would be entitled to assume the stronger answer.
Recorded here so they cannot.

> **Owner self-assessment, not independent assurance review.**

Four statements, each load-bearing:

1. **The same person may hold the system-owner and assessor roles.** In this
   project they do. Role separation is a control that has not been applied here.
2. **No independent assessment has occurred** — internal or external. Not
   "pending scheduling"; none.
3. **The ruling is a scope classification, not a safety certification.** It
   answers *where does Kirra World sit relative to the safety scope*. It does
   not certify anything, and no downstream document may cite it as if it did.
4. **The ruling must reopen if Kirra World gains authority over actuation,
   release, safety decisions, or required safety inputs** — any one of the four.

This posture is recorded **now, before the ruling**, rather than as a caveat
attached to it afterwards. A limitation written after a conclusion reads as a
hedge on the conclusion; written before, it is a constraint on what the
conclusion is allowed to be. The distinction matters most to the reader who
was not in the room.

**Statement 4 is a pre-commitment, not the `Conditions that reopen the
decision` field.** That field stays `UNASSIGNED` because the full set of
reopening conditions has not been decided — statement 4 fixes a floor beneath
it. Whatever the eventual ruling says, it says at least this, and a ruling that
recorded a *narrower* reopening condition would contradict a commitment made
before the ruling existed.

Nothing here rules on anything. The gate reads the record above and continues
to hold: status is `PENDING`, seven fields are `UNASSIGNED`, and `kirra-world*`
stays declaration-only.

### What completing the ruling would take

Recorded so "the ruling is pending" does not stay a state with no visible exit.
The eight questions in *Questions the ruling must address* map onto the record's
fields as follows — this mapping is descriptive, and the owner may answer them
in any structure they choose.

| Field | What would fill it | Which questions bear on it |
|---|---|---|
| `Date` | The date the ruling is taken | — |
| `Scope classification` | QM, safety-related but non-authoritative, or another classification, stated as a term of art with its source standard named | Q7 |
| `Rationale` | Why that classification follows, given Fence A and Fence B — including whether absence of a runtime dependency is *sufficient* | Q1, Q2, Q5, Q6 |
| `Assumptions` | What must stay true for the classification to hold — ODD assumptions and common-source artifact handling among them | Q3, Q4 |
| `Required evidence` | What must be produced and re-produced to preserve the classification over time | Q8 |
| `Conditions that reopen the decision` | The full set, of which statement 4 above is the pre-committed floor | Q2, Q5 |

**Q5 remains the sharpest and this PR does not soften it.** The checker bounds
*trajectories*; a semantic error producing a legal trajectory to a
wrong-but-reachable place is bounded kinematically while being operationally
wrong. Whether that is a safety concern or an availability concern is the
owner's call, and it is not made here.

---

## Glossary

| Term | Definition |
|---|---|
| **Kirra World** | The proposed semantic evidence subsystem. Non-authoritative for safety decisions |
| **semantic world model** | Generic descriptive phrase for a knowledge representation of the environment |
| **observation** | An immutable, timestamped, sourced, framed record of a measurement or assertion |
| **evidence** | The body of observations; what the store holds, as opposed to conclusions |
| **derived projection** | A rebuildable view computed from evidence. Never a live safety input |
| **independent perception channel** | A redundant perception input evaluated on its own; may be authoritative under its owning contract |
| **perception hypothesis** | One channel's interpretation of a scene |
| **authoritative safety input** | An input the checker reads and relies on for a verdict |
| **semantic map** | Place names, regions, aliases, route labels, annotations — meaning, not geometry for the checker |
| **safety corridor** | Checker-owned drivable-space geometry, supplied under its owning contract |
| **shared source artifact** | A common underlying file (e.g. an approved map) each consumer validates independently |
| **runtime dependency** | A call, query, or read performed while the depending component is running |
| **transitive dependency closure** | Every component reachable by following dependency edges, not just direct ones |
| **verifying consumer** | The component that verifies a release token before driving hardware |

---

## Contradictions and migration notes

Documented, **not rewritten**.

| # | Collision | Location | Disposition |
|---|---|---|---|
| M1 | "world model" = perception channel | `perception_redundancy.rs:4,156`; `ros2-adapter/node.rs:375` | Prose → "independent perception channel". Inside the safety closure, so change needs safety review |
| M2 | `robot/world_model.py` | Read projection sharing the subsystem name | Retain (ADR-0040 compatibility projection); consider `situation_projection.py` |
| M3 | Ambiguous prose in docs/tests | Various | New documents must qualify; existing cleaned opportunistically |
| M4 | "Map" as semantic category **and** safety artifact | Blueprint §4.1 vs `CorridorSource` | Resolved by Decision 2; blueprint §4.1 to be annotated |

### Migration checklist

- [x] Rename ambiguous **prose** (docs first, lowest risk) — done for LIVE prose; see scope below
- [ ] Rename **source symbols** where justified — safety-closure files need safety review
- [ ] Update **tests** that assert on renamed identifiers
- [ ] Update **diagrams**
- [ ] Update **traceability links** (`// SAFETY:` / `REQ:` tags)
- [ ] **Preserve stable public APIs**; deprecate before removing

#### Prose migration — what was changed, and what was deliberately not

**Changed** (5 live source comments + 3 normative documents):

| Location | Was | Now |
|---|---|---|
| `kirra-trajectory/src/perception_redundancy.rs` ×2 | "two INDEPENDENT world models" / "redundant world model" | *independent perception channels* / *redundant perception channel* |
| `kirra-ros2-adapter/src/node.rs` | "a camera-only world model" | *a camera-only perception channel* |
| `parko-core/src/detector.rs` ×2 | "the perception world model" / "an empty world model" | *the perception hypothesis* / *an empty perception hypothesis* |
| `CONSTITUTION.md` §7, `COMPANION.md` | bare "the world model" | *the semantic world model* |
| `ARCHITECTURE.md` §6 heading | "World model versus conversation" | *World state versus conversation* — matching that section's own body, which already said **World state** |

The two `perception_redundancy.rs` hits are the ones Decision 1 called unacceptable: they sit **inside the safety closure**, where "the world model was wrong" must not be able to mean either a perception fault or a semantic-knowledge fault. Notably that file's *next line* already read "given two independent perception channels" — the canonical term was already the natural one there.

The normative documents take the generic ***semantic* world model** rather than the subsystem name **Kirra World**, deliberately: Kirra World is not built, and writing "Kirra World represents sourced physical-world facts" in a constitutional document would assert a shipped capability. The qualifier removes the ambiguity without making a claim.

**Deliberately NOT changed**, each for a stated reason:

| Left alone | Why |
|---|---|
| Historical ADRs (`0003`, `0004`, `0014`) | An ADR is a dated record of a decision. Retro-editing its prose to match later terminology rewrites the record rather than migrating it. |
| Dated analyses (`docs/analysis/ADAS_BENCHMARK_*`, `docs/COMPETITIVE_*`) | Point-in-time snapshots, including an assessor-style critique that uses "shared world model" as its own finding. |
| Third-party terms of art | "world models" describing Waymo/NVIDIA *generative* foundation models is their vocabulary, not ours; renaming it would misquote them. |
| `robot/world_model.py` | A **source-symbol** rename (module name), which this checklist puts behind safety review. It is also imported by `rabbit_converse.py`, staged by the installer, and gated by the live `KIRRA_WORLD_MODEL_ENABLED` variable — renaming it changes robot deployment, not prose. Its own module docstring already states it is a non-authoritative read projection. |

---

## Planned enforcement work (not implemented here)

| # | Item | Depends on |
|---|---|---|
| E1 | Bidirectional Cargo dependency-closure fence over the 12-crate closure | First `kirra-world` crate |
| E2 | Trait-implementation ownership check (`impl CorridorSource for …`) | E1 |
| E3 | Python AST fence for the verifying consumer and robot layer | — |
| E4 | systemd / shell / environment configuration scan | — |
| E5 | Architecture allowlist file for shared validated artifacts | Decision 2 |
| E6 | Review-gate checklist item for new safety-input adapters | — |
| E7 | Deployment topology verification | Runtime |

---

## Consequences

**Positive.** Terminology is unambiguous in safety discussion. The hidden-adapter
route is named and prohibited. Fence B has a measured scope rather than a list.
The fence covers the Python consumer. The assurance claim is honest about being
unruled.

**Negative / accepted.** The strict no-dependency rule for `kirra-core` may
eventually force a crate split. The migration checklist creates deferred work,
including inside the safety closure where changes are expensive.

---

## Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | Terminology decided but not adopted | New-document rule; checklist |
| R2 | Hidden adapter still slips in via an unlisted trait | Trait list explicitly open; review gate E6 |
| R3 | Configuration-only coupling evades static checks | E4 + E7 |
| R4 | The assurance ruling is never made and the pending state is read as settled | Template requires a named owner and date |
| R5 | Shared-artifact allowlist grows into a loophole | Each entry needs independent-validation evidence |

---

## Assurance impact

This ADR **removes** an unsupported claim (the asserted out-of-scope
determination) and replaces it with a stated, pending question. That is a
reduction in claimed assurance, deliberately.

No existing safety claim, ASIL rating, or standards mapping changes.
Kirra is designed in alignment with ISO 26262 ASIL-D requirements and
IEC 61508 SIL 3 requirements. Independent third-party assessment has not yet
been performed.

---

## Migration impact

**None.** Documentation only. No renames, no code, no configuration.

---

## Open questions

1. Is the strict no-dependency rule for `kirra-core` sustainable, or is the
   lower-level split needed at the first shared-primitive request?
2. Which additional traits are safety-authoritative inputs? The list is open.
3. Does the shared-artifact allowlist need per-entry validation evidence, or
   does the owning contract suffice?
4. Should `robot/world_model.py` be renamed, and when?
5. Who owns the deployment topology verification (E7) — CI or the install
   tooling?

---

## Ratification criteria

**Proposed.** Accepted only when:

- [ ] **Architecture owner** sign-off on the canonical terminology
- [ ] **Safety-assurance owner** confirms M1's prose rename inside the safety
      closure is acceptable, and on what timeline
- [ ] The **Decision 5 ruling template is completed** by the safety-assurance
      owner — this ADR does not require the ruling to be *favourable*, only
      *recorded*
- [ ] Open question 1 dispositioned

Merging this PR satisfies none of the above.
