# Security Policy

## Reporting a Vulnerability

Kirra is safety-critical infrastructure. Security vulnerabilities
should be reported privately.

**Do not open a public GitHub issue for security vulnerabilities.**

Report vulnerabilities to: justin.looney@kirrasystems.com

Please include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if known)

We will respond within 48 hours and work with you on a coordinated
disclosure timeline.

## Security Invariants

Kirra maintains the following security invariants.
Any bypass of these is a critical vulnerability:

- `KIRRA_ADMIN_TOKEN` compared with `constant_time_compare` only
- `require_admin_token` returns 503 on absent/empty token (never 200)
- `verify_attestation` cryptographically verifies a per-node **Ed25519** signature over the `(node_id, nonce)` challenge against the registered `ak_public_pem` — and, when a node has a registered `expected_pcr16_digest_hex`, BINDS the presented PCR16 digest into the signed payload and ENFORCES it (fail-closed: expected-but-absent or mismatched digest → reject). Never mocked; the prior admin-token HMAC proof was removed — INV-3
- DDS actuator topics use `Volatile` durability (never `TransientLocal`)
- `OperationalCommand::Unknown` denied in all posture states

## Supply-chain hardening

### GitHub Actions are pinned to commit SHAs (M-6)

Every third-party and first-party action referenced from `.github/workflows/`
MUST be pinned to a **full 40-hex commit SHA**, not a mutable tag
(`@v4`, `@stable`, …). A tag can be force-moved by a compromised action
maintainer or a hijacked account to point at malicious code, which would then run
in CI with this repo's `GITHUB_TOKEN` and secrets. A SHA is immutable.

- **Apply the pins:** `scripts/pin-actions.sh` resolves each tag/branch ref to its
  commit SHA (`git ls-remote`) and rewrites it as
  `uses: owner/repo@<sha> # <tag>` (idempotent). Run it in a networked
  environment — egress to `github.com` is required.
- **Enforce:** `scripts/pin-actions.sh --check` exits non-zero if any mutable-tag
  reference remains; wire it into CI once the initial pins are applied.
- **Maintain:** Dependabot (`.github/dependabot.yml`, `github-actions`) bumps the
  pinned SHA + version comment as new releases ship and surfaces new majors for
  review.

### Container base images are pinned by digest (M-7)

Container base images (`Dockerfile`, `dashboard/Dockerfile`,
`deploy/docker/*`) SHOULD be pinned by immutable digest
(`FROM image:tag@sha256:…`), not a floating tag (`alpine:3`, `node:20-alpine`,
`ros:jazzy-ros-base`, …) whose contents can change or be re-pushed under the same
tag. Resolve a digest with
`docker buildx imagetools inspect <image>:<tag> --format '{{.Manifest.Digest}}'`
and refresh it deliberately via Dependabot (`.github/dependabot.yml`, `docker`).
