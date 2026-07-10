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

## 7. Consumer verifying-key enrollment and rotation order (ADR-0033)

The sections above provision the **signing** key on the verifier. The other
half of the pair — the **verifying** key pinned on the ROS/R2 motor consumer
(ADR-0033; `RosReleaseGate::new` admits only a pinned key) — is provisioned by
`kirra-consumer-ctl` (`crates/kirra-actuation-consumer/src/enrollment.rs`):

```
kirra-consumer-ctl enroll --key-hex <64-hex-vk> --path /etc/kirra/governor_vk.pin
kirra-consumer-ctl show   --path /etc/kirra/governor_vk.pin
```

Properties: **idempotent** (re-enrolling the identical key succeeds without a
write); **refuses to overwrite silently** (a different key needs an explicit
`--rotate`, and a corrupt pin is refused as tamper evidence rather than
replaced); writes are **atomic** (temp + rename — no torn pin). The pin is a
PUBLIC key: the requirement is integrity, not secrecy — keep the pin file
inside the same OS-ownership boundary as the serial device (the ADR-0033
layer-2 dedicated user; whoever can write the pin decides which governor the
consumer obeys).

### Rotation order — this order is load-bearing

1. **Re-enroll the consumer** with the NEW verifying key
   (`kirra-consumer-ctl enroll --rotate …`) and restart the consumer. The
   consumer now trusts the new key; the verifier is still signing under the
   old one, so every token is refused `SignatureInvalid` and the platform
   holds its safe stop — **expected and bounded** by how long step 2 takes.
2. **Rotate the verifier's signing key** (`KIRRA_GOVERNOR_SIGNING_KEY_SOURCE`
   → the new seed; restart the verifier). Tokens verify again; motion
   resumes on the next valid release (the key-mismatch alarm self-clears).

Consumer-first is deliberate: the safe-stop window sits between the two
steps either way, but consumer-first bounds it to an operator-controlled
moment with both hands already on the system.

### Done out of order — the failure mode, loudly

If the verifier signs under the new key while any consumer still pins the
old one, that consumer refuses **everything**: `SignatureInvalid` on every
frame → liveness starves → decel-to-stop → output silence. **Permanent safe
stop** until re-enrolled. Fail-closed and safe — and NOT mute: after
`DEFAULT_SIGNATURE_INVALID_ALARM_STREAK` (10) consecutive signature failures
the consumer latches the **key-mismatch alarm**
(`MotorConsumer::health().key_mismatch_alarm` +
`KEY_MISMATCH_ALARM_EXPLANATION`), a diagnostic DISTINCT from staleness or a
dead link, naming the likely cause and this recovery:

**Recovery:** `kirra-consumer-ctl enroll --rotate` with the CURRENT governor
verifying key → restart the consumer → the next valid release clears the
alarm and resumes motion. (A mixed refusal stream does not latch the alarm —
only the uniform signature-failure signature of a key mismatch does.)

### First-provisioning assumption (stated, not assumed away)

`enroll` presumes an operator-authenticated channel to the consumer host
(the same trust `kirra-ota-ctl enroll` presumes): the first pin is only as
trustworthy as the channel that delivered the key hex. Bootstrapping that
first pin from a signed enrollment bundle / attested first boot is a tracked
follow-up, not something this procedure claims to solve.
