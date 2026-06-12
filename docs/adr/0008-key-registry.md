# ADR-0008: Unified key registry — one abstraction over four key stores; trust grounded in stored registrations, not caller-supplied strings

| Field | Value |
|---|---|
| Status | **Accepted (Phase A)** — registry abstraction + fleet adoption landed; see *Conditions that reopen this decision* |
| Date | 2026-06-12 |
| Deciders | Project owner |
| Issues | #329 (this ADR + the `KeyRegistry`); #319 (the review register this is gap-2 of); #314 (the multi-tenant operator identity that motivated the keyring note); #296/#317 (the fleet transport whose caller-supplied key this replaces) |
| Doc | `src/key_registry.rs` (the abstraction); `crates/kirra-fleet-transport/README.md` (the adopting crate) |
| Builds on | **ADR-0007 Clause 1** (the untrusted-carrier rule — trust is the Ed25519 payload signature; this ADR answers *which key* verifies it) |

## Context

Trust in this system is grounded in Ed25519 public keys. But those keys live in
**four** different principal stores in **three** different encodings, each with its
own ad-hoc load + decode at every verification site:

| Principal | Table | Column | Encoding |
|---|---|---|---|
| Node (attestation) | `nodes` | `ak_public_pem` | SPKI PEM |
| Federation controller | `trusted_federation_controllers` | `public_key_b64` | raw b64 |
| Operator | `operators` | `pubkey_pem` | SPKI PEM |
| Fleet-grant signer | `trusted_federation_controllers` | `public_key_b64` | raw b64 |

Two problems follow. First, **duplication**: every site that verifies a signature
re-implements the row load and the encoding decode, so a normalization bug (or a
revocation check, like "is this operator still active?") has to be re-gotten-right in
each place. Second, and more serious, the **fleet crate took a caller-supplied
`public_key_b64: &str`** (`accept_report`, `ingest_clearance_grant`): the *caller*
decided which key to trust on each verify call. That is a trust-grounding gap — an
authentic signature over the wrong key is still the wrong principal. The #319 review
register named this as architecture gap 2; #314's operator-identity work and
#296/#317's fleet transport both pointed at "a keyring that maps which key verifies
which source" as the missing abstraction.

## Decision

Add a **`KeyRegistry`** struct in the SDK (`src/key_registry.rs`) that unifies the
lookup across all four principal types behind a single API. It is an **additive
wrapper** over `VerifierStore` — **no table schema changes, no encoding migration, no
breaking API changes, no existing API removal.**

```rust
pub enum KeyRole { NodeAttestation, FederationController, Operator, FleetGrant }

pub struct KeyRegistry<'a> { /* &VerifierStore */ }

impl KeyRegistry<'_> {
    pub fn new(store: &VerifierStore) -> KeyRegistry<'_>;
    pub fn resolve_ed25519_pubkey(&self, principal_id: &str, role: KeyRole)
        -> Result<Option<[u8; 32]>>;
    pub fn verify_for(&self, principal_id: &str, role: KeyRole,
                      payload: &[u8], signature_b64: &str) -> bool;
}
```

Three properties make it load-bearing:

### 1 — one normalized output (raw 32 bytes), normalized at the read boundary
`resolve_ed25519_pubkey` returns the **raw 32-byte** Ed25519 key regardless of how the
backing table stores it. **PEM → bytes** reuses the existing, tested SPKI parser
(`attestation::parse_ed25519_public_pem`) rather than a hand-rolled offset; **b64 →
bytes** decodes and **requires exactly 32 bytes** — a wrong-length or undecodable key
is rejected with a logged warning and `None`, never a panic and never a
truncated/padded key. Four tables, three encodings in, **one encoding out.**

### 2 — fail-closed resolution
Every "cannot resolve" is `Ok(None)`, never a trusted default: unknown principal,
**revoked operator** (checked `revoked_at_ms IS NULL`), absent key, or malformed
stored key. `Err` is reserved for an actual SQL failure. `verify_for` collapses all of
these — plus a malformed/bad signature — to `false`, so an unknown principal can never
be ignored into an accept.

### 3 — the fleet crate consults the registry (the trust-grounding fix)
The fleet crate gains registry variants — `accept_report_from_registry` and
`ingest_clearance_grant_from_registry` — that resolve the verifying key **from a
stored registration** keyed by `principal_id`, instead of trusting a `&str` the caller
passed in. The original `&str` signatures **stay** (backward compatibility, and for
test/spike callers without a store); the registry variants are the **fleet-deployment**
path. After this, a fleet trust claim is grounded in *who is registered*, not *what
string the caller supplied*.

> Note on the ingest variant's signature: it takes `principal_id` (not a
> `&KeyRegistry`) because the registry borrows the store **immutably** while
> `ingest_clearance_grant` needs it **mutably** (it burns the replay nonce + writes the
> row). The key is resolved in a scoped immutable borrow that ends *before* the
> mutating path — the borrow checker forbids the two coexisting on one store.

## Rationale

The abstraction is small and the change is additive precisely because the trust model
was already right (ADR-0007 Clause 1: trust is the signature). What was missing was a
single answer to *which key*. Centralizing the load + the encoding normalization + the
revocation check in one tested place removes a class of "re-gotten-wrong-per-site"
bugs, and routing the fleet verify through it closes the caller-supplied-key gap with
no migration risk: the stores are untouched, only the read path is unified.

## Considered and rejected

- **Migrate all stores to one on-disk encoding now.** Rejected for Phase A: a schema
  + data migration across four tables carries real risk for zero behavioral gain over
  read-boundary normalization. Named as deferred future work below, not silently
  dropped.
- **Put the registry on `AppState` instead of wrapping `VerifierStore`.** Rejected:
  the keys live in the store; wrapping the store keeps the abstraction usable by any
  store holder (the fleet crate has a store, not an `AppState`).
- **Change the fleet `&str` APIs in place.** Rejected: that would break the test/spike
  callers and the backward-compat contract. Additive variants instead.

## Honest limits (named, not built)

- **The audit signing key is resolvable READ-ONLY (Phase A.1, #329 residual).** The
  `AuditSigning` role resolves the chain's verifying key from the store's in-memory
  signer, keyed by the key's `verifying_key_id` fingerprint (the same id the audit
  chain stamps in `key_id`), so a chain row's `key_id` resolves to a key through the
  registry. **Still deferred:** rotation + a persisted key *history* — the registry
  knows only the ONE current in-memory key, so any other fingerprint is `None`, and a
  key rotated away cannot be resolved to verify its historical rows. That wider change
  touches the store schema + the chain trust model (see *Conditions that reopen*).
- **Encoding migration is deferred.** Phase A normalizes at the read boundary; it does
  **not** rewrite the stores to a single on-disk format. One abstraction now; one
  on-disk encoding is named future work.
- **Resolution is point-lookup, not cache.** Each `resolve` is a store read. If a hot
  verify path needs it, a caching layer is an additive future step (the abstraction
  boundary makes it a drop-in).

## Conditions that reopen this decision

- A **fifth principal type** or a key store that is not a single `(id → key)` row
  (e.g. multi-key principals / key history) — the `KeyRole` enum + single-row model
  would need extension.
- A need to **rotate or register the audit signing key** as a first-class principal —
  that pulls the named residual into scope and changes the store, not just the read
  path.
- Adopting **one on-disk encoding** (the deferred migration) — at which point the
  read-boundary normalization simplifies to a single decode.
