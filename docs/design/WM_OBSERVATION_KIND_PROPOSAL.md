# `ObservationKind` — RULED

**KIRRA-WM-OBSKIND-001** · drafted 2026-08-07 · **status: ADOPTED (option 2), 2026-08-07**

## RULED — 2026-08-07

> **ADOPTED — 2026-08-07 by Justin Looney, World Model owner. Option 2:** the
> three **attested** variants only — `Attribute` (`observation`), `Spatial`
> (`spatial`), `Relationship` (`relationship`) — plus `Unrecognised` as the
> non-writable degrade target. **`Existence` is DEFERRED**, to be revisited when
> a producer actually needs it.
>
> The three constraints in §"Three constraints" are carried into the ruling
> unchanged: the existing tokens keep their exact spellings, SD-4's
> `kind = 'spatial'` check moves with `Spatial`, and an unrecognised kind
> degrades rather than guesses.
>
> **What this authorizes:** `ObservationKind` may enter `kirra-world` as an enum
> shaped like `EntityKind`'s, and `TypedPayload`'s body becomes definable per
> variant. It does **not** authorize changing `NewEvent::kind` from `&str` to the
> enum in the same slice — that touches the canonically-hashed bytes and needs
> its own compatibility argument, exactly as the trust axes did.
>
> **Revisit trigger for `Existence`:** a perception producer with a
> saw-something-claiming-nothing output. Adding it then is an additive enum
> variant; that asymmetry against removal is the reason it was deferred rather
> than included.
>
> **One approver.** This ruling was recorded by the same person holding every
> role, as with every other World Model ruling. Kirra is designed in alignment
> with ISO 26262 ASIL-D requirements and IEC 61508 SIL 3 requirements;
> independent third-party assessment has not yet been performed.

The proposal as put to the owner follows, unamended.

## What this is, and what it is not

This document **proposes** a variant list for §7.1's `ObservationKind` and asks
for a ruling. It does not adopt one, and nothing in the codebase depends on it
until it is ruled.

The distinction matters here more than usual. Every other Tier 1 gap has been an
*implementation* gap — the design existed and the code did not. This one is the
reverse: the blueprint **names the field and never enumerates its variants**, so
there is nothing to implement against. `crates/kirra-world/src/observation.rs`
records the refusal in those terms:

> **`ObservationKind` is also absent.** §7.1 names the field but the blueprint
> never enumerates its variants, and inventing a taxonomy of observation kinds
> would be a domain decision this module has no basis for.

That refusal was right, and it is why this is a proposal rather than a commit.

## Why it is now on the critical path

It was treated as a parallel nice-to-have. It is not.

§7.1's `TypedPayload` body is **per-kind** and versioned. Per-kind payloads
need kinds. So:

- `TypedPayload` is Tier 1's last implementation item, and it is **blocked** on
  this.
- `WM_SCOPE.md` Tier 1 therefore cannot complete.
- **ADR-0040's Q1 revisit trigger cannot fire**, because it is keyed to *"on
  completion of `WM_SCOPE.md` Tier 1"*.

One unenumerated field is holding the seam decision, the payload model and the
tier boundary. That is the reason to rule on it now rather than later.

## Evidence: what `kind` already carries

Counting unit: one distinct string literal assigned to `NewEvent::kind` in the
`kirra-world-store` tree and the D-20 generator, at `8c1c4fdc`. Independence
unit: the call site. This is what the system writes **today**, not what it
should write.

| Value | Where | What it means in practice |
|---|---|---|
| `observation` | 9 call sites, and the generator's default | A claim about a subject's attribute |
| `spatial` | 4 call sites | A claim with a position, requiring a frame |
| `relationship` | the D-20 generator, every *N*th event | A claim linking two subjects, carrying predicate + object |

Three values, arrived at by use rather than by design. That is a starting point,
not a taxonomy — but it is real usage, and a proposal that cannot express these
three is disqualified.

## Three constraints any ruling has to respect

These are not preferences. Each is already enforced somewhere.

1. **`kind` is inside the chain hash.** `compute_record_hash_v2` takes it as a
   distinct argument, separate from the canonical JSON. Re-spelling an existing
   value — `observation` → `attribute`, say — is not a rename; it changes the
   digest of every row that carried it and breaks `verify_chain` on every
   existing store. **Any ruling must keep the three tokens above spelled exactly
   as they are, or accept that no existing store verifies.** This is the same
   constraint that forced the trust axes to be appended-when-present.

2. **`spatial` is load-bearing in the schema.** SD-4 is
   `CHECK (kind <> 'spatial' OR frame_id IS NOT NULL)`. A vocabulary that
   renames or splits `spatial` has to carry that check with it, and ADR-0042
   Decision 2 is what the check protects.

3. **An unrecognised kind must degrade, not guess.** §6.2's rule for
   `EntityKind` — *degrade to `Unknown`, not a guessed supertype* — applies with
   more force here, because a kind arriving from a newer writer is the ordinary
   case in a fleet mid-rollout. `EntityKind::from_token` already has the shape to
   copy: an unrecognised token maps to a variant with **no group to read**, so
   the violation is unavailable rather than merely discouraged.

## What `ObservationKind` is not

**It is not `EntityKind` with different names**, and a proposal that derives one
from the other is wrong. I suggested that framing in conversation before looking;
it does not survive looking.

- `EntityKind` answers *what is this thing* — `Robot`, `Door`, `Room`, `Mission`.
- `ObservationKind` answers *what does this claim assert about it* — that it has
  an attribute, that it is somewhere, that it relates to something else.

They are orthogonal. A `Door` can be the subject of a spatial claim, an attribute
claim and a relationship claim; a spatial claim can be about a `Door`, a `Robot`
or a `Package`. Nineteen entity kinds times *k* observation kinds is the space,
not nineteen renamed.

## The proposal

Four variants plus the degrade target. The three existing tokens keep their exact
spellings for constraint 1.

| Variant | Token | Asserts | Notes |
|---|---|---|---|
| `Attribute` | `observation` | a property of one subject | The default. Token kept as-is — it is the most-written value in every store. |
| `Spatial` | `spatial` | where a subject is | Carries SD-4's frame requirement. |
| `Relationship` | `relationship` | that two subjects are related | Uses `predicate` + `object`, which no other kind populates meaningfully. |
| `Existence` | `existence` | that a subject was observed at all, asserting nothing further | **New.** See below. |
| `Unrecognised` | *(any other)* | nothing that may be acted on | The §6.2-shaped degrade target. Not writable; reached only by reading. |

### Why `Existence` is proposed, and why it is the one to argue about

It is the only variant not attested by current usage, so it is the weakest part
of this proposal and the part most worth rejecting if it does not earn itself.

The case for it: the subject-summary projection (#1377) computes
`first_observed` / `last_observed` from *any* confirmed event naming a subject.
Today that means a subject can only be "observed" by asserting something about
it — there is no way to record *"the lidar saw something here and I am claiming
nothing else."* That is a real perception output, and forcing it into
`observation` with an empty payload makes an attribute claim that asserts
nothing, which is exactly the shape §7.3 refuses elsewhere ("the design must not
force producers to invent precision they do not have").

The case against: it may be `spatial` with no attributes, in which case the
vocabulary does not need it and SD-4 already covers the frame.

**I do not have the domain grounds to settle that**, which is the same reason
the module refused to invent the list in the first place.

### What is deliberately not proposed

- **A kind per entity kind** (`door_observation`, `robot_observation`, …). That
  is the `EntityKind` conflation above, and it makes the payload vocabulary grow
  as 19 × *k*.
- **`prediction`.** Blueprint §20: predicted content never appears in the
  evidence store, and `Origin::Predicted` is already refused by both
  `TrustAxes::new` and the schema. A kind for it would reopen that.
- **`correction`.** Already modelled, and better: `PayloadSource::Correction`
  carries what it corrects, on the payload where the provenance belongs. A kind
  would put it on the wrong axis.

## Options

1. **Adopt as proposed** — four writable variants plus the degrade target.
2. **Adopt without `Existence`** — three writable variants, exactly what is
   attested today. Safest against over-fitting; leaves the
   "saw-something-claiming-nothing" case unmodelled.
3. **Adopt with amendments** — a different list. The three constraints above
   bound what is admissible; everything else is open.
4. **Reject and defer** — leave `kind` a free string. Honest, but it does not
   unblock `TypedPayload`, so Tier 1 stays incomplete and the Q1 trigger stays
   unfired indefinitely. If this is chosen, that consequence should be recorded
   rather than rediscovered.

## Recommendation

**Option 2, then revisit `Existence` when a producer needs it.**

Preferring it over option 1 for the reason this document exists: three variants
are *attested*, the fourth is *argued*. A vocabulary is exactly the kind of thing
that is cheap to extend additively and expensive to retract — a kind that ships
and gets written into a hash-chained log cannot be removed without rewriting
rows inside the chain, which is not a migration but a forgery.

The asymmetry is the whole argument. Adding `Existence` later costs an additive
enum variant. Removing it later costs the log's integrity.

## What adoption would license

Only this: `ObservationKind` enters `kirra-world` as an enum with a
`from_token`/`as_str` pair shaped like `EntityKind`'s, and `TypedPayload`'s body
becomes definable per variant. It does **not** license changing `NewEvent::kind`
from `&str` to the enum in the same slice — that touches the hashed bytes and
needs its own compatibility argument, exactly as the trust axes did.

Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
SIL 3 requirements. Independent third-party assessment has not yet been
performed.
