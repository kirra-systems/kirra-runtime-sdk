# WM-2 event schema

| | |
|---|---|
| **Identifier** | KIRRA-WM2-SCHEMA-001 |
| **Status** | **RULED 2026-08-05** — SD-1 through SD-4 decided by the World Model owner (§6). The schema is fixed; **no store is implemented against it yet**. |
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
a design decision, not a column name**, and it is proposed as SD-2 below.

## 3. The columns (as ruled)

Keeping the stand-in's shape where it was right — one append-only
hash-chained `world_events` as the only writable table, bitemporal
(`txn_time_ms` / `valid_from_ms` / `valid_to_ms`), `generation` as the total
replay order — and adding what §2 found missing. The §6 rulings are folded in.

| Column | Change | Note |
|---|---|---|
| `generation`, `txn_time_ms`, `valid_from_ms` | **keep** | The bitemporal core; unchanged |
| `valid_to_ms` | **keep, WRITE-ONCE** | SD-1. Set at insert for an inherently bounded observation, else NULL and the end derived from supersession. Never updated — there is no `UPDATE` in an append-only log |
| `event_id` | **keep** | Identity of the *record* |
| `observation_id` | **ADD** | Identity of the *observation*, stable across re-attribution |
| `source`, `source_version` | **keep** | Who produced it; also carries the derivation *method* (SD-3) |
| `writer_class` | **ADD** | SD-2. `sensor` \| `operator` \| `derivation` \| `llm_candidate`. Inside the hashed bytes |
| `claim_status` | **ADD** | SD-2. `candidate` \| `confirmed`. Inside the hashed bytes, so relabelling breaks the chain |
| `provenance` | **ADD** | SD-3. A JSON **array of `observation_id`s**, digest-covered — traversable with `json_each`, no second writable table |
| `frame_id`, `map_id` | **ADD** | SD-4. Nullable, with `CHECK (kind <> 'spatial' OR frame_id IS NOT NULL)` |
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

ADR-0041 **D-2** measured **458.51 B/event** log-only, and OQ2's budget of **18 033 812
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

### DISCHARGED 2026-08-05 — and it did move materially

Measured: ADR-0041 **D-20**, evidence
**`docs/evidence/wm2-schema-growth-target-20260805/`** (`JETSON-TARGET-MEASURED`).
An earlier host bundle, `wm2-schema-growth-20260805/`, is **superseded** — its
figures were 32 768 B/arm high, from a counted SQLite `-shm` file. Cite the
target bundle.

The figure moved **1.2349×** (`lean`) to **1.3345×** (`populated`) — 458.51 →
**566.23 / 611.86 B/event**. The six columns above were "plausibly additive";
they are, and the addition is large enough to matter.

**OQ2's allocation no longer closes.** Its 15 448 320 allocated events exceed
the corrected budget at both ends of the band — headroom **+14 % → −1.8 % /
−10.0 %** — so the durations were not merely revisable in principle, they are
arithmetically unaffordable as ruled. ADR-0041's OQ2 section carries the
reopening, the three levers and their numbers; the choice among them is a
decision about incident reconstruction and is not made in this document.

One caveat worth carrying: the instrument had to be a **new** one
(`tools/wm2-schema-growth`), not `wm2-persistence-harness append` as
anticipated above. The harness must not depend on `kirra-world-store` — that
separation is what keeps its stand-in numbers from ever being re-read as being
about the real store — so measuring the real schema needed a second instrument
that shares the harness's event generator and nothing else. The shared
generator is pinned by a test that reds if the harness's own copy drifts,
because a ratio between the two is only a *schema* ratio while the stream is
identical.

## 6. The four decisions — RULED 2026-08-05

> **On the labels.** These are **SD-n** — *schema decisions* — and not `D-n`.
> ADR-0041's `D-1…D-19` are **measurement records**, and an earlier draft of
> this document reused `D-2` and `D-4` for decisions while also citing
> ADR-0041's `D-2` (bytes/event) in §5. Review caught the collision. The
> prefixes are distinct so a future reader never has to work out which sense is
> meant, and `SD-` is also distinct from the `S-n` *store obligations* in
> `WM2_PROJECTION_REBUILD_PROTOCOL.md` §8.

### SD-1 — the bitemporal core is kept, and `valid_to_ms` is WRITE-ONCE

The columns are unchanged. What was missing was a rule, and the rule is the
decision: **`valid_to_ms` is set at insert or never.**

In an append-only log there is no `UPDATE` that closes a fact's validity, so the
column has exactly two honest uses — set at insert when the observation is
*inherently* bounded ("seen between 10:00 and 10:05"), or left NULL with the end
derived from a superseding event. Leaving it nullable and silent invites the
third thing, an `UPDATE` that breaks append-only. Stating write-once removes the
temptation rather than relying on nobody taking it.

### SD-2 — `writer_class` + `claim_status`, both inside the hashed bytes

**Separate tables per writer class (option b) is rejected**, and it was already
rejected once: ADR-0041 open question 1, ruled the same day, enumerates what
splitting the log costs — one chain becomes several, the total replay order is
lost, compaction citations fragment. Those costs are not cheaper when the split
is by writer instead of by durability.

**Enforcement above the store (option c) is rejected as conventional.** "The
caller promises not to" is the thing `kirra-world`'s own comment rules out when
it demands the distinction be *unforgeable rather than conventional*.

The adopted option buys more than validation. Because both columns sit inside
the canonically-hashed event bytes — the property `retention_class` already has
— relabelling an LLM-written event as `confirmed` **does not fail a check, it
breaks the chain**. The tamper evidence does the enforcement, not the code path.
A code path can be bypassed; a hash cannot.

> **Invariant.** `writer_class = 'llm_candidate'` ⇒ `claim_status = 'candidate'`.
> Enforced at the write path *and* structurally evident in the chain.

### SD-3 — provenance is a citation list of `observation_id`s, digest-covered

**A normalized edge table (option b) is rejected on architecture:** ADR-0041
fixes `world_events` as *"the append-only evidence log (the only writable
table)"*, and an edge table is a second one.

**A free-form JSON blob (option a) is rejected on purpose:** provenance exists so
a claim can be re-judged when the evidence under it changes, which means walking
it. Unstructured, it cannot be walked.

Adopted: a **JSON array of `observation_id`s** in one digest-covered column —
structured enough to traverse with `json_each`, with no second writable table.
The derivation *method* needs no new field; `kind`, `source` and
`source_version` already carry it. If provenance later needs to be queryable at
speed, that is a **projection** — rebuildable, not writable — which is the
escape hatch the architecture already provides.

### SD-4 — nullable `frame_id`/`map_id`, with a CHECK that excludes the bad state

```sql
CHECK (kind <> 'spatial' OR frame_id IS NOT NULL)
```

Plain nullable columns would permit a spatial claim with a NULL frame, which is
precisely what ADR-0042 Decision 2 needs excluded. A distinct `kind` with its own
required fields (option b) gets the guarantee at the cost of a kind taxonomy that
forks every query. The `CHECK` gets option b's guarantee inside option a's shape,
at the storage layer rather than by convention.

### The through-line

SD-2 and SD-4 are the same decision twice: **structural, not conventional.** Both
are answered by putting the constraint somewhere a caller cannot route around —
the hashed bytes in one case, a storage-layer `CHECK` in the other. SD-1 is the
same instinct applied to a rule that had simply gone unstated.

## 7. What this does not decide

- **Projection schemas.** `entities_projection` / `relationships_projection` are
  rebuildable views and follow from the fold, not from this table.
- **The grouping budgets** left unset by OQ1's P-2/P-3.
- **OQ2b** — whether policy-supersession records are needed at WM-2.
- **The implementation.** SD-1…SD-4 are ruled, so the schema is no longer the
  blocker — but **no store is written against this document**, and
  `kirra-world*` remains declaration-only at the time of this ruling.

## 8. What the first implementation must carry

Recorded here so the rulings above are not re-derived from prose when someone
writes the migration:

1. **The `CHECK` from SD-4 is part of the schema**, not a validation the writer
   performs. A spatial claim with a NULL frame must be rejected by the storage
   layer.
2. **The SD-2 invariant is enforced at the write path AND evident in the chain.**
   Enforcing it only in code would satisfy the letter and lose the property that
   made option (a) win.
3. **`kirra-audit-hash` from the first commit**, not retrofitted — §4 explains
   why there is no upgrade path from a store written under the harness chain.
4. **Re-measure bytes/event before trusting OQ2's horizons** (§5). The
   instrument exists; the digest gate will confirm it is unchanged.
