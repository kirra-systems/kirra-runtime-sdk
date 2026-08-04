# WM-2 projection rebuild-and-swap protocol

| | |
|---|---|
| **Identifier** | KIRRA-WM2-REBUILD-001 |
| **Status** | **Design — proposed.** The protocol and its tests exist; the store-side implementation does not, and is gated by ADR-0042 Decision 5. |
| **Implements** | ADR-0041 **R2** — "Projection changes are rebuilds, not backfills, and they run alongside" |
| **Addresses** | ADR-0041 **open question 7** — partial projections under disk pressure |
| **Prototype** | `tools/wm2-persistence-harness/src/rebuild.rs` (pure state machine, 15 tests) |
| **Date** | 2026-08-04 |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

---

## 1. Scope

**In scope.** The decision protocol governing a projection rebuild: which states
a rebuild attempt passes through, which transitions are legal, and — the only
question a query path actually asks — *when may this projection be believed*.

**Out of scope, deliberately.**

- **Kirra World domain logic, storage, APIs and services.** This is a protocol
  prototype living in the measurement harness, over the harness's stand-in
  schema (`standin_schema_digest 630eb690aaef…`). It implements no world-model
  semantics.
- **The fold itself.** How rows are read and projection rows written is store
  work. This document decides when a fold's *output* may be trusted.
- **Robot runtime behaviour, safety algorithms, checker inputs, release-token
  logic, actuator behaviour.** None are touched, and none are in the dependency
  closure of the prototype.
- **Cost.** ADR-0041's outstanding R2 obligation asks what alongside-rebuild
  costs "in code, in peak disk, in write amplification and in cutover latency".
  This document answers the *code* half — the protocol is 319 lines and its
  decision surface is exhaustively testable — and answers none of the other
  three. See §9.

ADR-0042 Decision 5 still gates implementation. Nothing here asks for that gate
to move.

---

## 2. Why "build alongside and swap" is not yet a protocol

ADR-0041 R2 is one sentence: build the new projection beside the live one, catch
up, and swap. That sentence is correct and it hides three races, each of which
is a silent-wrong-answer defect rather than a crash.

**Ingest does not stop.** The rebuild folds from generation 0 forward while new
events keep landing. So:

1. **Catch-up need not converge.** If ingest outruns the fold, "wait until caught
   up" is not a step — it is a hang that looks like slowness. Without an explicit
   convergence rule, the operator has no signal distinguishing *slow* from
   *never*.

2. **A verification goes stale the instant an event arrives.** Equivalence is
   proven *at a generation*. If the log advances between proving equivalence and
   acting on it, the thing that was verified is not the thing being activated.
   An implementation that proves equivalence, then swaps, has a window in which
   it activates an unverified projection — and reports success.

3. **An interrupted fold leaves partial rows with nothing marking them.** This is
   open question 7 verbatim. A fold writes, so disk pressure refuses it midway;
   what is left behind is a projection that is *wrong* but *present*, and a query
   path that reads projections by name cannot tell.

All three are decisions, not computations. That is why the protocol is a state
machine with no database access: a decision table that cannot touch a store can
be tested exhaustively, and every invariant below is a test rather than a review
comment.

---

## 3. States

| State | Meaning | Authoritative? |
|---|---|---|
| `Building { folded_to }` | Folding forward; the projection is **partial**. | No |
| `CaughtUp { at }` | The fold reached the log head, observed at generation `at`. Nothing has been compared yet, and the head may already have moved. | No |
| `Verified { at }` | Equivalence against the incremental projection was proven **at generation `at`**. Valid only while the head is still `at`. | No |
| `Active` | Serving queries. Terminal. | **Yes** |
| `Failed(reason)` | This attempt is over. The previous projection is untouched. | No |

`is_authoritative()` returns true for `Active` and nothing else. That single
predicate is the whole answer to open question 7: a projection left partial by an
interrupted fold sits at `Building`, which is not authoritative and cannot cut
over. **The marker OQ7 said was missing is the protocol state itself** — it does
not need to be inferred from row counts, a sentinel row, or a checkpoint
heuristic.

`Failed` carries *why*, because the operator response differs by reason:

| Reason | What it means | Response |
|---|---|---|
| `CatchupDiverged` | Ingest is outrunning the fold | Capacity — shed, throttle, or rebuild off-peak |
| `EquivalenceMismatch { at }` | The rebuilt projection ≠ the incremental one | **Defect.** The fold is not deterministic. Do not retry blindly |
| `FoldRefused(_)` | Disk pressure or I/O error | Capacity — reclaim, then retry |
| `Abandoned` | Operator call | None |

Collapsing these to a bare "failed" would make a determinism defect
indistinguishable from a full disk, and the second is routinely retried.

---

## 4. Transitions

Rows are the current state; columns are the event. `—` is a refused transition
(returns `TransitionRefused`, carrying which transition was attempted and why —
a caller cannot log "transition failed" without saying which one).

| from \ on | `fold_progress(p)` | `reached_head(h)` | `append(h)` | `equivalence_proven(g)` | `cutover(h)` | `failure(r)` | `restart()` |
|---|---|---|---|---|---|---|---|
| `Building{f}` | `Building{p}`, — if `p < f` | `CaughtUp{h}`, — unless `f == h` | `Building{f}`, — if `h < f` | — | — | `Failed(r)` | `Building{f}` |
| `CaughtUp{at}` | — | — | **`Building{at}`**, — if `h < at` | `Verified{g}` if `g == at`, else — | — | `Failed(r)` | `Building{at}` |
| `Verified{at}` | — | — | **`Building{at}`**, — if `h < at` | — | `Active` if `h == at`, else — | `Failed(r)` | **`Building{at}`** |
| `Active` | — | — | `Active` | — | — | — | `Active` |
| `Failed(_)` | — | — | — | — | — | — | `Failed(_)` |

Six cells carry the argument.

**`Verified` + `append` → `Building`.** The single most important transition
here, and the one a hand-rolled implementation would omit. The proof was taken at
a generation; the log moved; the proof now describes something that is not what
would be served. It **demotes rather than fails** — nothing is wrong, the world
simply moved — and it demotes to `Building` rather than `CaughtUp` because the
projection is now genuinely behind by the appended event. It has to fold again,
not merely be re-proven.

**`reached_head` refused unless the fold position *equals* the head.** Folding
past it is impossible; folding short of it and claiming catch-up anyway is the
dangerous case, and it is not obviously dangerous, which is why it is stated
here. `CaughtUp` is one transition from `Verified` and two from `Active`, so
accepting `reached_head(900)` from a projection folded only to 400 activates an
unfolded projection asserting it is current at 900 — **with every other
invariant in §5 still satisfied**. The state machine does not assume its caller
has folded as far as it says it has; that assumption is the thing a decision
table exists to remove.

**`cutover` refused unless `h == at`.** Belt and braces for the same race: even
if a caller reached `Verified` and the head moved by a path the state machine did
not observe, the head is re-checked at the moment of the swap.

**`equivalence_proven` refused unless `g == at`.** A proof taken at a different
generation is a proof about something else. Accepting it is how "we verified it"
becomes true of a different database than the one being activated.

**`append` refused when `h` is behind the folded position.** A log head that
lands behind work already folded out of the log means the generation source is
not monotonic (§8, S-3). Every staleness comparison in the protocol rests on
that source, so a violation is refused loudly rather than absorbed as "no
progress" — absorbing it would leave the protocol running normally on top of a
broken premise.

**`Active` + `failure` → refused.** An active projection is not a rebuild
attempt. Retiring it is a separate, deliberate act with its own authorisation;
the rebuild protocol has no transition that removes a working projection. This
makes "failure leaves the previous projection active" **structural** rather than
a rule someone has to remember.

---

## 5. The six required invariants

ADR-0041 R2 and open question 7 between them require six properties. Each is a
test, not a review comment.

| # | Invariant | Enforced by | Test |
|---|---|---|---|
| 1 | Partial projections are invalid by default | `is_authoritative()` is true only for `Active` | `partial_projections_are_never_authoritative` |
| 2 | Only a verified projection may cut over | `on_cutover` refuses from every other state | `only_a_verified_projection_may_cut_over` |
| 3 | Cutover is atomic | `Verified → Active` is a single transition with no intermediate state; and no path reaches `Active` except through it | `cutover_is_the_only_path_to_authoritative` |
| 4 | Failure leaves the previous projection active | No transition retires a projection; `Active.on_failure` refused; `Failed` accepts no further operations | `failure_never_touches_the_projection_that_is_working` |
| 5 | Restart/recovery behaviour is explicit | `on_restart` is total — every state has a defined resumption | `restart_resumes_a_fold_but_demotes_a_verification` |
| 6 | Equivalence proof is recorded before activation | `Active` is reachable only from `Verified`, which is reachable only from `on_equivalence_proven` | `equivalence_must_be_proven_at_the_generation_it_claims`, `a_verification_does_not_survive_an_append` |

Invariant 3 needs a caveat stated plainly. **The state machine makes cutover
atomic in the protocol; it does not make it atomic on disk.** `Verified → Active`
has no intermediate state *here*, so no reader can observe a half-swapped
protocol state. Whether the store's swap — renaming a table, flipping a pointer
row, switching an attached file — is itself atomic is a store property this
document does not supply. §8 states what the store must provide for the
invariant to hold end to end. That obligation is real and it is not discharged
by these tests.

Invariant 4 is worth its own note: it is enforced by *absence*. The protocol
contains no operation that removes or retires a projection. There is therefore no
code path — correct, buggy, or maliciously ordered — by which a failed rebuild
attempt takes down the projection that was serving. That is a stronger guarantee
than a rule that says "on failure, keep the old one".

---

## 6. Catch-up convergence

`catchup_is_converging(gaps)` takes the head-minus-folded distance sampled once
per round, oldest first, and answers whether the loop is making progress.

Convergence is **not** "the gap decreases every round". A burst of ingest can
widen the gap for a round without meaning the rebuild will never finish, and a
rule that failed on that would make the guard itself the most common reason
rebuilds fail. The rule is *not stuck*: the gap must beat its best-so-far at
least once every `MAX_NON_CONVERGING_ROUNDS` (3).

Two deliberate choices:

- **No judgement before there is evidence.** With `≤ MAX_NON_CONVERGING_ROUNDS`
  samples the function returns `true`. An attempt that has barely started has not
  demonstrated divergence, and failing it would be the guard inventing the
  failure it exists to detect.
- **Divergence is a failure, not a stall.** It resolves to
  `FailureReason::CatchupDiverged` — a capacity signal with a named operator
  response — rather than an unbounded wait that shows up as a hang.

The sampling interval and the exact round budget are tunables, not invariants. On
target they should be chosen against measured fold throughput; that measurement
is part of the outstanding cost obligation (§9), not of this design.

---

## 7. Restart and recovery

`on_restart` is total. Every state has a defined resumption, so "what happens if
the process dies mid-rebuild" has an answer for every point in the protocol
rather than for the points someone thought of.

| State before | After restart | Why |
|---|---|---|
| `Building{f}` | `Building{f}` | **Resumes.** The fold is deterministic and its position is durable; re-folding from 0 would be correct but wasteful |
| `CaughtUp{at}` | `Building{at}` | The head may have moved while the process was down |
| `Verified{at}` | `Building{at}` | **Demotes.** See below |
| `Active` | `Active` | The live projection survives a restart; that is what durability means |
| `Failed(r)` | `Failed(r)` | An abandoned attempt stays abandoned, with its reason |

The `Verified` demotion is the load-bearing one, and it is the same failure class
as the tier C marker replay corrected in PR #1322: **a proof that survives a
restart unexamined is stale evidence**. The head may have advanced during the
outage, and the process has no way to know from its own memory that it did not.
Re-folding and re-proving costs a lap; activating on a pre-crash proof costs a
wrong answer that nothing detects.

Note the asymmetry, which is deliberate: a *fold position* survives a restart, a
*verification* does not. A fold position is a claim about work done, checkable
against durable state. A verification is a claim about a relationship between two
things at a moment, and the moment is gone.

---

## 8. What the store must provide

The protocol is correct only if the store supplies four things. Naming them here
is the point of writing the protocol first — each is now a requirement on the
implementation rather than something discovered during it.

| # | Requirement | Why the protocol needs it |
|---|---|---|
| S-1 | **A durable rebuild state record**, updated in the same transaction as the fold progress it describes | Otherwise a crash between the fold write and the state write leaves the two disagreeing, and §7's resumption reads a position that is not the position |
| S-2 | **An atomic swap primitive** — the new projection becomes readable and the old becomes not, with no observable intermediate | Invariant 3 end to end (§5). Candidate: a single-row pointer flip inside a transaction |
| S-3 | **A monotonic log head readable at a generation** | `on_append`, `on_reached_head` and `on_cutover` all compare against it; if it is not monotonic, the staleness check is not a check |
| S-4 | **Equivalence comparison at a pinned generation** — the rebuilt and incremental projections compared with the log held still, or compared over a snapshot | `on_equivalence_proven` requires the proof and the generation to refer to the same instant |

S-2 is the one most likely to be underestimated. A rebuild that swaps by dropping
and renaming has a window; a rebuild that swaps by pointer has one row to make
durable. The choice interacts with open question 3 (projections in the same file
or a rebuildable sidecar), which remains open.

---

## 9. What this resolves, and what it does not

**Resolves — open question 7, in design.** OQ7 asked for "either a
fold-in-progress marker, a transactional whole-fold, or an explicit rule that
projections are invalid until a checkpoint confirms them". The protocol supplies
the first and third together and shows they are the same thing: the rebuild state
*is* the fold-in-progress marker, and `is_authoritative()` *is* the
invalid-until-confirmed rule. A fold refused midway under disk pressure leaves
`Building`, which cannot serve and cannot cut over.

**Does not resolve — the same question in the live store.** OQ7 is about
behaviour under disk pressure in a real store. The protocol says what the state
must be; §8 says the state record must be durable and transactional with the
fold; none of it is implemented, because implementing it means store work that
ADR-0042 Decision 5 gates.

**The ruling, 2026-08-04**, recorded in ADR-0041's OQ7 entry:

> OQ7 is resolved at the protocol level. Full closure is conditional on the
> production store implementing and testing durable rebuild state,
> pinned-generation equivalence verification, and atomic cutover with restart
> recovery.

Which properties fall on which side:

| Property | Protocol | Store |
|---|---|---|
| Restart/recovery | **Answered** — `on_restart` is total (§7) | S-1: the durable state record it resumes from |
| Old-active preservation on failure | **Guaranteed** — no transition retires a projection (§5, invariant 4) | — |
| Equivalence proof before activation | **Required** — `Active` only from `Verified` (§5, invariant 6) | **S-4: the comparison itself** |
| Cutover atomicity | **State transition** is atomic (§5, invariant 3) | **S-2: the database swap, for readers and across restart** |

The two bold store entries are the load-bearing gap. Recorded this way so the
state machine does not stand in for unbuilt persistence behaviour — the same
posture as OQ8, where the strategy was adopted and the R2 prototype obligation
carried explicitly rather than quietly.

**Does not discharge — the R2 cost obligation.** ADR-0041's acceptance record
carries one outstanding obligation: prototype R2's alongside-rebuild-and-swap far
enough to know what it costs in code, peak disk, write amplification and cutover
latency. This document and its prototype answer **code** — the protocol is small
and its decision surface is exhaustively testable. The other three are
*unmeasured*, and this PR must not be read as evidence about them:

| Cost | Status |
|---|---|
| Code | Answered. 319 lines of protocol (about a third of it documentation) plus 266 of tests; 15 tests; no store dependency |
| Peak disk | **Unmeasured.** Bounded above by the D-2 arithmetic — projections are 3.74 % of store size, so a duplicate is ≈306 MiB (321 MB) at the 8 GiB ceiling — but *bounded* is not *measured*, and that figure is an arithmetic consequence of a measurement, not itself one |
| Write amplification | **Unmeasured.** The fold writes the whole projection while ingest writes it incrementally; the sum has not been observed |
| Cutover latency | **Unmeasured.** Depends entirely on S-2, which is not chosen |

**Measurement semantics of the one number quoted above**, per the rule in
`docs/hardware/JETSON_WM2_PERSISTENCE_DRILL.md` §4: the 3.74 % figure counts
**bytes of projection tables against bytes of total store**, its independence
unit is **one store built by one harness run**, it holds **schema, entity count
and event mix** fixed at the D-2 configuration, and it supports the claim *"a
duplicate projection is a small fraction of the store"* — and no claim about
rebuild throughput, peak disk during a rebuild, or the cost of the swap.

---

## 10. Prototype status

`tools/wm2-persistence-harness/src/rebuild.rs` — pure state machine, no SQL, no
I/O, no schema. 15 tests covering the six invariants, fold-position and log-head
sanity, convergence, and the two end-to-end paths (clean run; late append forcing
another lap rather than a stale cutover).

Run:

```
cargo test --manifest-path tools/wm2-persistence-harness/Cargo.toml rebuild::
```

The prototype is fenced from `kirra-world` by construction — it is in the
measurement harness, it depends on nothing in the SDK's runtime crates, and it
carries the stand-in schema. It is a design artifact that happens to compile and
be tested, which is the only form of design document that cannot drift from what
it describes.
