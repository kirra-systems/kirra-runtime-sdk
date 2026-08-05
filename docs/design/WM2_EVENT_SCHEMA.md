# WM-2 event schema — proposal

| | |
|---|---|
| **Identifier** | KIRRA-WM2-SCHEMA-001 |
| **Status** | **Proposed.** A recommendation for the World Model owner to rule on. It decides nothing, and no store is implemented against it. |
| **Addresses** | The prerequisite ADR-0041 leaves open — *"column-level schemas are deliberately not fixed here"* — and the `kirra-audit-hash` adoption the harness README requires |
| **Unblocked by** | ADR-0042 **Decision 5**, recorded 2026-08-05 (*safety-related, non-authoritative*) |
| **Date** | 2026-08-05 |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

---

## 1. What this is for, and the thing to read first

Before Kirra World can write a real event, two things must be fixed that
deliberately are not: the **column-level schema**, which ADR-0041 declines to
set, and the **chain primitive**, which the measurement harness deliberately
gets wrong so that nothing can depend on it.

**The finding that matters most is §5**, and it is uncomfortable: the retention
budget ruled on 2026-08-05 (open question 2) is computed from a bytes/event
figure measured on the *stand-in* schema. Every column proposed below makes that
number larger, which makes the event budget smaller. **The OQ2 durations are
therefore provisional on this schema being fixed**, and that coupling was not
visible when OQ2 was ruled.

## 2. The stand-in is not a draft of the real schema

The harness's `SCHEMA_V1` is a deliberate stand-in, and its own header says so:
it measures *"the substrate under a representative load, not the finished World
Model"*. Treating it as a starting point is tempting because it exists and it
works. It should not be, because **four concepts the architecture calls
load-bearing are absent from it entirely.**

| Missing | Where it is required | Why the omission matters |
|---|---|---|
| **Provenance** | ADR-0040 §9 — the most-cited concept, and *"the reason the store is evidence-not-truth"* | Without the chain behind a claim, a claim cannot be re-judged when the evidence under it changes. The store degrades from evidence to assertion |
| **Frame / map reference** | ADR-0042 glossary: an observation is *"immutable, timestamped, sourced, **framed**"* | A spatial claim with no frame cannot later be shown *not* to have become checker geometry — which is exactly the Decision 2 boundary |
| **Observation identity distinct from event identity** | `kirra-world`'s `ObservationId` vs `EntityId`: *"an observation outlives whatever entity it was later attributed to, and re-attribution must not rewrite it"* | The stand-in has one `event_id` and a free-text `subject`. Re-attribution would have to rewrite the subject — destroying the immutability the model rests on |
| **Writer class** | ADR-0040 fixes that an LLM may create *"only a suggestion, a candidate label, a candidate relationship or a candidate query — **never a confirmed fact**"* | The stand-in has `source TEXT` and nothing else. That rule is currently **conventional**, and `kirra-world`'s own comment demands it be *"unforgeable rather than conventional"* |

The last row is the one to argue about. A `source` string cannot make the rule
unforgeable, because any writer can put any string in it. Making it structural
means either a separate column the store validates, or separate tables, or a
claim-status field that only a non-LLM writer may set to `confirmed`. **That is
a design decision, not a column name**, and it is proposed as D-2 below.

## 3. Proposed columns

Keeping the stand-in's shape where it was right — one append-only
hash-chained `world_events` as the only writable table, bitemporal
(`txn_time_ms` / `valid_from_ms` / `valid_to_ms`), `generation` as the total
replay order — and adding what §2 found missing.

| Column | Change | Note |
|---|---|---|
| `generation`, `txn_time_ms`, `valid_from_ms`, `valid_to_ms` | **keep** | The bitemporal core; unchanged |
| `event_id` | **keep** | Identity of the *record* |
| `observation_id` | **ADD** | Identity of the *observation*, stable across re-attribution |
| `source`, `source_version` | **keep** | Who produced it |
| `writer_class` | **ADD** | `sensor` \| `operator` \| `derivation` \| `llm_candidate`. See D-2 |
| `claim_status` | **ADD** | `candidate` \| `confirmed`. An `llm_candidate` writer may never write `confirmed` |
| `provenance` | **ADD** | The derivation chain: what this rests on. Structure is D-3 |
| `frame_id`, `map_id` | **ADD** | Nullable — non-spatial claims have neither. See D-4 |
| `kind`, `subject`, `predicate`, `object` | **keep** | The claim triple |
| `payload`, `payload_schema`, `payload_digest` | **keep** | Opaque body + its versioned schema |
| `retention_class` | **keep** | The six classes ruled in OQ2; inside the hashed bytes, so immutable |
| `chain_digest` | **keep, recomputed** | See §4 — the value stays, the function changes |
| `redacted` | **keep** | |

## 4. The audit-hash swap

The harness chains with a **local SHA-256** (`crate::sha256`) that is
*deliberately different* from the production primitive, so that nothing can
depend on the harness by accident. The real store must use
**`kirra-audit-hash`**, which already supplies what is needed:

- `compute_record_hash_v2` — the chained record hash
- `canonical_signing_payload_v2` — the canonical byte layout signed over
- `verifying_key_id` — key identity, for the ledger

This is the smaller half of the prerequisite and is close to mechanical, with
one caveat worth stating: **adopting the shared primitive changes every chain
digest**, so a store written under the stand-in chain cannot be verified by the
real one. That is correct and wanted — the harness's databases are measurement
artifacts, not migration sources — but it means there is no upgrade path from a
harness DB, and nobody should try to build one.

## 5. What this costs, and why OQ2 is provisional

D-2 measured **458.51 B/event** log-only, and OQ2's budget of **18 033 812
events** on 8 GiB derives directly from it. Every column in §3 is additive.

`observation_id`, `writer_class`, `claim_status`, `provenance`, `frame_id` and
`map_id` are six new columns, of which `provenance` is the only one plausibly
large. A rough figure is deliberately **not** given here: bytes/event is a
measured quantity in this project, not an estimated one, and the instrument for
measuring it already exists.

**The honest consequence:** OQ2's 30-day `raw` horizon and its ~2× coalescing
factor are computed against a schema that will change. When this schema is
fixed, `wm2-persistence-harness append` should be re-run against it on target,
and if bytes/event moves materially, **the OQ2 durations should be revisited
rather than inherited.** The ruling names the input that drives it — how far
back an incident reconstruction must reach — and that input is unchanged; what
changes is how many events fit.

This is recorded now, before implementation, so that it reads as a known
coupling rather than a discovery.

## 6. The four decisions this asks for

| | Decision | Options |
|---|---|---|
| **D-1** | Is the stand-in's bitemporal core kept as-is? | (a) yes, as §3 (b) revisit `valid_to_ms` nullability (c) other |
| **D-2** | How is *"an LLM may never write a confirmed fact"* made structural? | (a) `writer_class` + `claim_status` with a store-enforced invariant (b) separate tables per writer class (c) enforced above the store — **rejected as conventional, not structural** |
| **D-3** | What shape is `provenance`? | (a) JSON blob, digest-covered (b) a normalized edge table (c) a citation list of `observation_id`s |
| **D-4** | Are `frame_id`/`map_id` nullable columns, or is a spatial claim a distinct `kind`? | (a) nullable columns (b) distinct kind with its own required fields |

## 7. What this does not decide

- **Projection schemas.** `entities_projection` / `relationships_projection` are
  rebuildable views and follow from the fold, not from this table.
- **The grouping budgets** left unset by OQ1's P-2/P-3.
- **OQ2b** — whether policy-supersession records are needed at WM-2.
- **Any implementation.** No store is written against this document, and none
  should be until D-1…D-4 are ruled.
