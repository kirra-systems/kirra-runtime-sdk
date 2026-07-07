# Uptane Four-Role OTA Metadata (WP-13 / MGA G-7)

**Status:** metadata + verification CORE implemented and tested
(`crates/kirra-release-token/src/uptane.rs`); the durable metadata store,
campaign-engine wiring, and on-device client are the recorded follow-up (§4).

## 1. Why, over WP-12's single signature

WP-12 (`kirra_release_token::artifact_release`) gave the OTA path ONE governor
release signature per artifact. It proves *a specific artifact was released*,
but a single key means:

- **No key-compromise recovery.** Stealing that key forges any artifact; the
  only recovery is re-flashing every node's pinned key.
- **No rollback / freeze-attack protection.** Nothing binds "this is the
  *newest authorized* artifact", so an attacker who can serve bytes to a node
  can replay an old (once-valid) signed artifact, or freeze the node on a stale
  one indefinitely.

Uptane — the automotive OTA security standard — closes both by splitting trust
into four independently-keyed roles and a chain of monotonically-versioned,
expiring metadata.

## 2. The roles (as implemented)

| Role | Signs | Security property |
|---|---|---|
| **root** | the verifying key of every role (incl. its own) + version + expiry | trust anchor + **key rotation**: a new, higher-version root signed by BOTH the outgoing and incoming root keys — compromise recovery without touching the device, and a single stolen role key cannot re-delegate the others |
| **targets** | artifact facts (digest, length, version) + version + expiry | authorizes *which* artifacts exist |
| **snapshot** | the exact `targets` version + version + expiry | **consistency**: no mixing an old targets file with a current set |
| **timestamp** | the exact `snapshot` version + version + near-term expiry | **freshness**: a frozen (stale) metadata set is refused |

## 3. The verification pipeline (`uptane::verify_update`)

A node, holding a TRUSTED root and its last-trusted version floor
(`TrustedVersions`), verifies each poll's presented `(timestamp, snapshot,
targets)` metadata + signatures, in order:

1. **Role-separated signatures** — each checked against the key `root` pins for
   *that role* (a targets-key holder cannot forge timestamp — tested:
   `targets_key_cannot_forge_timestamp`).
2. **Freshness / freeze** — `now_ms < expires_at_ms` per role
   (`expired_timestamp_is_refused`).
3. **Rollback** — each version STRICTLY greater than the trusted floor
   (`rollback_below_the_trusted_floor_is_refused`).
4. **Chain consistency** — `timestamp.snapshot_version == snapshot.version`
   and `snapshot.targets_version == targets.version`
   (`snapshot_version_mismatch_is_a_chain_error`,
   `targets_version_mismatch_is_a_chain_error`).

Timestamp is verified first (cheapest freshness gate); each later step is only
reached if the prior pinned it. On success the node persists `new_versions` as
its rollback floor.

Root rotation is a separate entry point (`verify_root_rotation`): strictly
increasing version, signed by both the outgoing and incoming root keys
(`root_rotation_happy_path`, `..._without_old_signature_is_refused`,
`..._downgrade_is_refused`).

Per-role domain tags (`KIRRA-UPTANE-{ROOT,TARGETS,SNAPSHOT,TIMESTAMP}-V1`) are
mixed into every signing image, so a signature made for one role can never be
replayed as another's even at identical version/expiry — crypto-layer role
separation on top of the key-layer separation.

## 4. Follow-up (not in the WP-13 core)

- **Durable metadata store** — persist the trusted root + version floor + the
  latest metadata set (mirrors the `ota_campaigns` table); the WP-12
  `artifact_signature_b64` remains the on-the-wire artifact release signature,
  now understood as the `targets`-role signature over one entry.
- **Campaign-engine wiring** — a campaign references a `targets` entry;
  `resolve_node_assignment` serves the metadata chain; the node runs
  `verify_update` before `decide_pull`/staging (composing with the WP-12
  installer signature check).
- **On-device client** — `kirra-ota-ctl` fetches + verifies the chain; the
  release-pubkey provisioning (WP-12) generalizes to the root trust anchor.
- **Key custody** — HSM/TPM-held root key (deferred; the software rotation
  flow below is complete).

All metadata expiry/version values are operational parameters; the crypto is
the frozen part.


## 5. WP-14 status update (2026-07-07) — rotation operations + durable trust

The ROTATION OPERATIONS and node-side PERSISTENCE are implemented:

- `kirra_release_token::uptane` gained the operator/repository side:
  `SignedRoot` (the wire/storage bundle: root metadata + the outgoing +
  incoming root signatures), `author_initial_root` (self-signed anchor),
  `author_root_rotation` (mints a rotation signed by both root keys — the
  compromise-recovery mint), and `apply_root_rotation` (the node-side ergonomic
  wrapper over `verify_root_rotation`). The metadata types gained OPTIONAL
  serde derives behind a `serde` feature (off by default; the crypto core needs
  no serialization).
- `kirra_ota_installer::uptane_trust::UptaneTrustStore` — a JSON file (sibling
  of the OTA boot record) persisting the adopted `SignedRoot` + `TrustedVersions`
  floor, written with the boot record's atomic temp-fsync-rename discipline and
  loaded FAIL-CLOSED (missing/unparseable → error; the stored root is
  re-verified on load so a tampered file is rejected). `provision` /
  `adopt_rotation` / `record_versions`.

REVOCATION is proven end-to-end and across the restart boundary
(`adopted_rotation_revokes_the_old_key_across_a_reload`): after a node adopts a
root that rotates the `targets` key and persists it, a RELOAD refuses metadata
signed by the revoked old key and accepts the new — a leaked key is dead the
moment the node adopts the new root, no device re-flash.

REMAINING (recorded): HSM/TPM root-key custody; the audit-chained rotation
control-plane route on the verifier (mirrors the audit-signing-key rotation
route); wiring `UptaneTrustStore` into the `kirra-ota-ctl pull` path so a served
root-rotation is adopted before staging.
