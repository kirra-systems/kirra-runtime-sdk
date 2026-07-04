# Governor Release-Signing Key Provisioning (ADR-0031 Clause E)

**Status:** the file / dev-fixed sources are **LIVE**; the TPM-unseal source is
**deferred** (Phase-II, named external dependency: tss2 libs + hardware). Gap
**G7 — key/identity lifecycle** (`INDUSTRY_BENCHMARK_GAP_ANALYSIS.md`), workstream
**WS-1** (`docs/roadmap/PRODUCT_EXECUTION_PLAN.md`), Track 1.1.

## 1. The gap

The governor→actuator release bridge (`kirra-release-token`,
`docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md` §3.5–7) signs the digest of the exact
validated `GovernorContractView` with an **Ed25519** key so the actuator can verify
*"the governor approved exactly these bytes"* before it releases a command.
`issue_release_token(view, signing_key)` takes that key **by argument** — the crate
never decided *where the key comes from*. In the tree, the only signers were tests
and the `kirra-l3-e2e` measurement harness (a fixed `[7u8; 32]` key). There was **no
production provisioning path at all**, and nothing prevented a future caller from
minting a fresh random key that would verify against nothing.

## 2. What this adds

A single fail-closed provisioning seam: `kirra_release_token::provisioning`. It is the
ONE place that resolves the signing key, and it **refuses** rather than fabricating one.

Refusing is the safe outcome: the actuator and the verifier key registry (ADR-0008)
admit only a **pinned** governor verifying key, so an on-the-fly key would silently
break the trust chain (every release denied downstream, late and opaquely). The seam
fails loud and early instead.

### Env vars

| Var | Default | Meaning |
|---|---|---|
| `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` | — | `file:<path>` \| `dev-fixed` \| `tpm:<handle>`. **Unset/empty → refuse** (fail-closed). |
| `KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV` | `false` | `1`/`true` → admit the `dev-fixed` source. Absent → `dev-fixed` is refused. |

### Sources (`SigningKeySource`)

| Source | Behavior |
|---|---|
| `file:<path>` | Read exactly **32 raw bytes** (the Ed25519 seed). On Unix the file must not be group/other-accessible (`mode & 0o077 == 0`) — a leaky key file is refused (SSH private-key hygiene). Every intermediate buffer is **zeroized**. |
| `dev-fixed` | The well-known deterministic `[7u8; 32]` key the l3-e2e harness / release-flow tests use. **Never a production identity** — admitted ONLY under `KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV`. |
| `tpm:<handle>` | **Deferred** (Clause E, Phase-II). Always returns `TpmUnsealUnsupported` until tss2 libs + hardware land — the wiring exists so a deployment can *name* the source, but it never silently degrades to a weaker one. |

## 3. Fail-closed decision table

| Condition | Result |
|---|---|
| `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` unset/empty | `SourceUnset` (refuse) |
| unrecognized spec / `file:` with no path | `UnknownSource` |
| `dev-fixed` without `ALLOW_DEV` | `DevKeyNotAllowed` |
| `file:` path missing/unreadable | `FileUnreadable` |
| `file:` not exactly 32 bytes | `SeedLength` (buffer zeroized first) |
| `file:` group/other-readable (Unix) | `InsecurePermissions` |
| `tpm:<handle>` | `TpmUnsealUnsupported` (deferred) |
| `file:` 32-byte, `0600` seed | **provision** |
| `dev-fixed` + `ALLOW_DEV` | **provision** (non-production key) |

No path generates a random key; there is no success-by-default.

## 4. Testability (INVARIANT #13)

`parse_source` and `provision_signing_key` are **pure** — they take their inputs by
argument and read no env — so the full truth table is exercised without
`std::env::set_var`. The thin `source_from_env` / `dev_key_allowed_from_env` /
`provision_from_env` wrappers are the only env readers and are deliberately not
`set_var`-tested. The file-source tests (`#[cfg(unix)]`) write a per-test seed file
under the system temp dir (no `tempfile` dev-dep, no env mutation) and assert the
provisioned key actually signs a token that `verify_release` accepts.

## 5. Zeroization under `#![forbid(unsafe_code)]`

The crate forbids unsafe, so a hand-rolled volatile wipe is impossible. The seed
buffers are wiped via the vetted `zeroize` crate (already in the tree transitively via
`ed25519-dalek`/`curve25519-dalek`); `SigningKey::from_bytes` copies the seed into the
key's own zeroize-on-drop storage.

## 6. Tracked remainder

**TPM-unseal source (ADR-0031 Clause E, Phase-II).** Unseal the seed from a TPM
(sealed against a PCR policy) instead of a file. Named external dependency: tss2 libs
(`tss-esapi`, the `tpm` feature) + hardware. The seam already routes `tpm:<handle>` to
a single refusal point (`provision_signing_key` → `TpmUnsealUnsupported`), so landing
it is an additive change at one call site — no consumer rewiring.
