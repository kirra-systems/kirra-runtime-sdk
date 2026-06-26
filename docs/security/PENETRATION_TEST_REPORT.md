# Kirra Runtime SDK — Penetration Test & Security Assessment Report

**Target:** `kirra-systems/kirra-runtime-sdk` (root crate `kirra-verifier` + workspace crates + `parko/` workspace)
**Assessment type:** White-box pre-production security assessment / simulated penetration test
**Scope:** Entire repository — Rust safety kernel, HTTP governor service, FFI, industrial protocol adapters, doer-checker/planner stack, ML/inference (`parko`), web consoles/dashboards, infrastructure (Docker/Helm/CI), install tooling.
**Methodology:** Source-code audit across seven parallel domains (auth/authz, cryptography, input-validation/injection, memory/concurrency/DoS, safety-governor integrity, infrastructure/supply-chain, network/web/logging). Every finding is evidence-based with file:line references and verified against the actual code. No dynamic exploitation was performed against a live deployment.

---

## 1. Executive Summary

Kirra is a runtime **safety governor** for autonomous vehicles, robots, and drones whose entire purpose is to enforce fail-closed trust semantics — preventing unsafe or unauthorized commands from reaching actuators regardless of what an AI model, LLM, or upstream planner instructs. The assessment evaluated whether that promise holds against a determined attacker.

**The core application-layer security engineering is excellent and unusually disciplined.** We found **no** SQL injection, no command injection, no path traversal, no authentication bypass, no privilege escalation in the core auth mechanisms, no unsafe-deserialization, and no prompt-injection path. The cryptographic primitives (constant-time comparison, real Ed25519 attestation, CSPRNG nonces, domain-separated signing payloads, hash-chained audit log) are correctly built and fail-closed. The safety gate chain (posture routing → admin auth → kinematic envelope), the gray/black DAG traversal, the NaN/Inf-hardened kinematics contract, and the DDS `Volatile` durability invariant are all sound.

**The material risk lives at three seams that surround that strong core:**

1. **Transport & deployment exposure.** The governor serves its API — including the `KIRRA_ADMIN_TOKEN` bearer token and actuator commands — over **plaintext HTTP on `0.0.0.0`** with a **fully permissive CORS policy** (`Any` origin/method/header) and **no rate limiting**. An on-path attacker can sniff the admin token; a malicious browser origin can drive authenticated cross-origin requests. The constant-time token comparison (a genuine strength) is moot once the token is captured on the wire.

2. **A second, un-hardened deployment path.** Alongside the well-hardened `helm/kirra` chart and `deploy/systemd` units, there is a parallel `charts/kirra-verifier` + `deploy/docker/Dockerfile.native` path that **runs as root, ships a hardcoded default admin token**, and has no `securityContext`. The `install.sh` one-liner (`curl | sudo bash`) verifies binary checksums only conditionally (**fail-open**) and has no signature root of trust.

3. **A small number of genuine safety fail-OPEN gaps.** The RSS collision check — the system's primary safety invariant — can be **silently neutralized by NaN/Inf** in perceived-object/velocity inputs (the per-pose kinematics path is hardened, but the RSS loop is not). A newly-registered-then-silent sensor can go undetected by the watchdog for ~28 s. An empty live-node set derives `Nominal` (absence ⇒ accept).

### Overall Risk Rating: **HIGH**

No **Critical** finding (no unauthenticated remote code execution or authentication bypass was identified). The rating is driven by the combination of (a) cleartext transport of the master credential for a safety-critical control plane, (b) a root-running, hardcoded-credential deployment path, and (c) fail-open gaps in the primary safety invariant — each of which is individually serious in a system whose marketed guarantee is fail-closed safety.

| Severity | Count |
|----------|-------|
| Critical | 0 |
| High | 6 |
| Medium | 13 |
| Low | 15 |
| Informational | 5 |

---

## 2. Attack Surface Overview

| Entry point | Exposure | Auth | Notes |
|-------------|----------|------|-------|
| HTTP API `0.0.0.0:8090` | Network (plaintext) | Bearer + challenge | Primary control plane; no TLS in-process |
| `/attestation/challenge`, `/attestation/verify` | Unauthenticated | Challenge-response | CSPRNG nonce, single-use, TTL — sound |
| `/console/*` | Unauthenticated read + supervisor-key mutation | Partial | Posture-exempt by design (recovery plane) |
| Tier-1 identity-gated routes | Network | Admin token + `x-kirra-client-id` | SSE stream, federation submit, action/industrial eval |
| Tier-2 admin routes | Network | `KIRRA_ADMIN_TOKEN` | Node/dep registration, backup export, key rotation |
| Industrial protocol adapters (Modbus/CANopen/DNP3/CIP) | Via `/industrial/*` (admin+client-id) | Gated | Bounds-checked decoders, fail-closed |
| C FFI (`kirra_filter_move_velocity`, `kirra_reset_state`) | In-process / C ABI | Constant-time reset key | Null/len-checked, fail-closed |
| DDS / ROS2 actuator topics | Fleet network | DDS QoS | `Volatile` durability enforced |
| Federation reports | Network (push-in) | Ed25519 `verify_strict` + nonce burn | No SSRF (push, not fetch) |
| `install.sh` / release artifacts | Supply chain | Conditional checksum | Fail-open integrity |
| CI/CD (GitHub Actions) | Supply chain | `GITHUB_TOKEN` | Unpinned actions, `npm install` |
| Web frontends (`dashboard/`, `console/`, `website/`) | Browser | Varies | `console/` proxies server-side (good); `dashboard/` holds token in browser |

**Trust boundaries:** (1) Untrusted planner/LLM → Governor (the central thesis: doer proposes, checker bounds); (2) Network → HTTP API; (3) Fleet peer controllers → Federation; (4) C/C++ integrator → FFI; (5) Hardware/TPM → Attestation; (6) Operator browser → Console/Dashboard.

**Key assets:** `KIRRA_ADMIN_TOKEN`, `KIRRA_SUPERVISOR_RESET_KEY`, audit-chain signing key, per-node AK keys, fleet trust/posture state, actuator command authority (the crown jewel — issuing unsafe actuator commands is the ultimate attacker goal).

---

## 3. Findings by Severity

### HIGH

| ID | Title | File |
|----|-------|------|
| H-1 | Plaintext HTTP / no TLS; admin token transmitted in cleartext, binds `0.0.0.0` | `src/bin/kirra_verifier_service.rs:2932,2959` |
| H-2 | RSS collision check silently bypassed by NaN/Inf in object/velocity inputs | `crates/kirra-ros2-adapter/src/validation.rs:374-440,577-653` |
| H-3 | Telemetry watchdog ~28 s fail-open window for register-then-silent nodes | `src/telemetry_watchdog.rs:234-264` |
| H-4 | `install.sh` checksum verification fail-open; no signature root of trust | `install.sh:296-313` |
| H-5 | `charts/kirra-verifier` runs container as root (no `securityContext`) | `charts/kirra-verifier/templates/deployment.yaml`; `deploy/docker/Dockerfile.native` |
| H-6 | Hardcoded default admin token shipped in chart values (no `fail` guard) | `charts/kirra-verifier/values.yaml:14`; `templates/secret.yaml` |

### MEDIUM

| ID | Title | File |
|----|-------|------|
| M-1 | Permissive CORS (`Any` origin/method/header) on the entire API | `src/bin/kirra_verifier_service.rs:3850-3853` |
| M-2 | No HTTP request rate limiting (auth brute-force / Ed25519 CPU exhaustion) | router assembly `:3787-3897` |
| M-3 | Admin token held in browser SPA, sent cross-origin over plaintext | `dashboard/src/App.jsx:320-323,509-511` |
| M-4 | `curl \| sudo bash` as primary install method, pinned to mutable `main` | `install.sh:8` |
| M-5 | Admin token via `env_file`/env — exposed in `docker inspect` / `/proc/environ` | `docker-compose.yml:7-8,42-43` |
| M-6 | `npm install` (not `npm ci`) in release build + dashboard image | `release.yml:41`; `dashboard/Dockerfile:4` |
| M-7 | Unpinned third-party GitHub Actions (mutable tags) in write-privileged workflows | `.github/workflows/*.yml` |
| M-8 | `Dockerfile.native` runs as root, bakes empty `KIRRA_SUPERVISOR_RESET_KEY`, stray ports | `deploy/docker/Dockerfile.native:18,20` |
| M-9 | `derive_fleet_posture([])` returns `Nominal` (empty node set ⇒ accept) | `src/posture_engine.rs:45-55` |
| M-10 | Scalar `KirraKernelGovernor` Degraded path is a pure clamp — no decel-to-stop/no-reinit | `src/kirra_core.rs:235-238` |
| M-11 | Corridor polygon winding/self-intersection unvalidated — containment can fail open | `crates/kirra-core/src/containment.rs:121-142` |
| M-12 | DAG traversal recursion depth scales with node count — stack-overflow abort DoS | `src/verifier.rs:451,518` |
| M-13 | HA fenced old-primary keeps acting Active for ~1 heartbeat (~2 s) — bounded split-brain | `src/standby_monitor.rs:233-255` |

### LOW

| ID | Title | File |
|----|-------|------|
| L-1 | Non-strict Ed25519 `verify` (not `verify_strict`) in fleet-transport clearance grant | `crates/kirra-fleet-transport/src/lib.rs:477` |
| L-2 | Non-strict Ed25519 `verify` in audit-chain integrity verifier | `src/verifier_store.rs:274` |
| L-3 | TPM quote `qualifiedSigner` (AK Name) parsed but not validated | `src/tpm_quote.rs:157` |
| L-4 | Two admin handlers omit the `is_active()` passive-standby guard | `src/bin/kirra_verifier_service.rs:1641-1686` |
| L-5 | Missing security response headers (CSP/X-Frame-Options/HSTS/nosniff) | console serving `:2985`; `console/next.config.mjs` |
| L-6 | User-controlled identifiers logged unsanitized (log injection) | `src/bin/kirra_verifier_service.rs:671,700,...` |
| L-7 | Verbose upstream error detail surfaced to client (info disclosure) | `console/app/api/kirra/[...path]/route.ts:61` |
| L-8 | `validate_trajectory_slow` wrapper hardcodes `FrameTrust::Trusted` | `crates/kirra-ros2-adapter/src/validation.rs:135-141` |
| L-9 | Recovery-streak Mutex poison silently skips LockedOut escalation | `src/posture_engine_v2.rs:346` |
| L-10 | `init_generation_from_store` uses non-monotonic `store` (latent rollback) | `src/posture_engine.rs:21-26` |
| L-11 | No `parko/Cargo.lock`; parko CI not `--locked` | `parko/`; `ci.yml:139` |
| L-12 | `deploy-pages.yml` processes untrusted feed in `id-token: write` job | `.github/workflows/deploy-pages.yml:47-48` |
| L-13 | Mutable `:latest` image + GHA cache, no signing/provenance | `.github/workflows/docker.yml` |
| L-14 | `parko`/fleet-transport use `thread_rng` vs the codebase `OsRng` discipline | `crates/kirra-fleet-transport/src/lib.rs:434` |
| L-15 | Trajectory head velocity unbounded; predictive-RSS NaN-`dt` fall-through | `crates/kirra-ros2-adapter/src/validation.rs:258-300,587-590` |

### INFORMATIONAL

| ID | Title |
|----|-------|
| I-1 | `panic = "abort"` means a deterministic panic on attacker input crash-loops the HA cluster |
| I-2 | Audit-chain `key_id` is unsigned metadata (defensible; hardening note for v3 payload) |
| I-3 | No explicit `DefaultBodyLimit` (axum 2 MB default still in force) |
| I-4 | Federation 5 s replay window assumes loose clock-sync (nonce burn is the real guard) |
| I-5 | Clearance-challenge stores entry for active operators only (internal-only timing asymmetry) |

---

## 4. Detailed Technical Findings

### H-1 — Plaintext HTTP / no TLS; admin token transmitted in cleartext

- **Severity:** High · **CWE-319** (Cleartext Transmission), **CWE-311** (Missing Encryption) · **OWASP A02:2021** · **Confidence:** High
- **Files:** `src/bin/kirra_verifier_service.rs:2932-2933, 2959`; default `KIRRA_VERIFIER_ADDR=0.0.0.0:8090`

**Description.** The service binds a plain `tokio::net::TcpListener` and serves with `axum::serve` — there is no `rustls`/`axum-server`/`TlsAcceptor` anywhere in the binary. All traffic, including `Authorization: Bearer <KIRRA_ADMIN_TOKEN>`, federation reports, attestation challenges, and actuator motion commands, travels in cleartext, and the default bind is all-interfaces.

```rust
let listener = tokio::net::TcpListener::bind(&listen_addr).await
    .expect("failed to bind listener");
...
axum::serve(listener, app).with_graceful_shutdown(shutdown).await
```

**Exploitation scenario.** An attacker with any on-path position (fleet VLAN, shared VPC, compromised switch, ARP-spoof) passively captures the admin bearer token from a single admin request, then replays it to register malicious nodes, push dependency-graph edges that force `LockedOut`/`Nominal` flips, export full state via `/system/backup/export`, or issue `/fabric/command/{asset_id}`. An active MITM can rewrite actuator-command responses.

**Business impact.** Complete compromise of the fleet safety control plane — the master credential is recoverable by anyone who can observe traffic. For a safety governor, this can translate to unsafe actuator authority over physical vehicles/robots.

**Remediation.** Terminate TLS in-process (`axum-server` + `rustls`, `RustlsConfig::from_pem_file`) or mandate an enforced mTLS sidecar/ingress; refuse to bind a non-loopback address without TLS; add HSTS once TLS is live.

```rust
let tls = RustlsConfig::from_pem_file(cert, key).await?;
axum_server::bind_rustls(addr, tls).serve(app.into_make_service()).await?;
```

> Note: a deployment may front this with a TLS ingress, but the application default is plaintext on all interfaces — document and enforce the requirement rather than relying on topology.

---

### H-2 — RSS collision check silently bypassed by NaN/Inf in object/velocity inputs

- **Severity:** High · **CWE-697** (Incorrect Comparison), **CWE-754** (Improper Check for Exceptional Conditions) · **OWASP A04:2021** · **Confidence:** High
- **Files:** `crates/kirra-ros2-adapter/src/validation.rs:374-440` (snapshot RSS), `:577-653` (predictive RSS)

**Description.** The per-pose kinematics path (`validate_vehicle_command`) is fully NaN/Inf-hardened with a Priority-0 finiteness guard. The **RSS section — the system's primary collision-avoidance invariant — is not.** It reads `obj.velocity_mps`, `obj.heading_rad`, and `traj_point.velocity_mps` with no finiteness check, and containment validates only `Pose` x/y/heading, never object or velocity fields. A NaN flows straight into the RSS comparisons, which are all `<`-style tests that return `false` under NaN — so the dangerous object is **neither rejected nor skipped**, and the trajectory is accepted.

```rust
let lon_unsafe = dx_ego < lon_required;            // NaN  => false
if dy_ego.abs() < RSS_LONGITUDINAL_OVERLAP_M && lon_unsafe {   // never taken under NaN
    return TrajectoryVerdict::MRCFallback;
}
```

**Failure / exploitation scenario.** A tracker emits `heading_rad = NaN` (e.g. `atan2(0,0)` for a zero-velocity track) for a real stopped lead vehicle in-path at unsafe distance. NaN poisons `dx_ego`/`dy_ego`; every RSS gate evaluates false; the `continue` skip filters are also false. RSS §4 is bypassed for that object and the trajectory is `Accept`ed → collision risk. A compromised/buggy perception upstream can therefore disable the checker's main guard without ever tripping a fault.

**Business impact.** Defeats the central safety promise (checker bounds the doer) for any object with a non-finite field — potential physical collision / loss of life.

**Remediation.** Add a fail-closed finiteness gate at the top of the object and pose loops (and in `predicted_rss_breach`): any non-finite `obj.pos/velocity/heading` or `traj_point.velocity_mps` → `return TrajectoryVerdict::MRCFallback`. Mirror the `pose_is_finite` discipline already present in `containment.rs`.

```rust
if !obj.velocity_mps.is_finite() || !obj.heading_rad.is_finite()
    || !obj.pos_x_m.is_finite() || !obj.pos_y_m.is_finite()
    || !traj_point.velocity_mps.is_finite() {
    return TrajectoryVerdict::MRCFallback;   // non-finite perception ⇒ fail closed
}
```

---

### H-3 — Watchdog ~28 s fail-open window for register-then-silent nodes

- **Severity:** High · **CWE-636** (Not Failing Securely), **CWE-778** (Insufficient Monitoring) · **OWASP A04:2021** · **Confidence:** High
- **Files:** `src/telemetry_watchdog.rs:234-236, 253-264`

**Description.** The dead-man's-switch sweep only iterates nodes already present in `node_health`. New nodes are added solely on the node-list refresh, which is rate-limited to `AV_WATCHDOG_NODE_REFRESH_MS = 30_000`. A node that registers just after a refresh and then goes silent is invisible to the 2 s timeout sweep until the next 30 s refresh — and a test (`test_node_refresh_interval_is_longer_than_timeout`) actively enforces `refresh ≫ timeout`.

**Failure scenario.** A lidar registers at t≈0 (just after a refresh), reports briefly, then its link dies at t=2 s. No `TELEMETRY_TIMEOUT` fires; the node stays `Trusted` and the DAG keeps deriving `Nominal` until ~t=30 s — a ~28 s window where a dead safety sensor is treated as healthy, despite a 2 s configured fault threshold.

**Business impact.** The governor operates on stale "healthy" sensor state for up to ~28 s after a sensor failure, during which unsafe trajectories that the dead sensor would have flagged are admissible.

**Remediation.** Push newly-registered nodes into `node_health` directly from the registration handler (or signal the watchdog on registration), or drop the refresh interval below `AV_TELEMETRY_TIMEOUT_MS`.

---

### H-4 — `install.sh` checksum verification fail-open; no signature root of trust

- **Severity:** High · **CWE-494** (Download Without Integrity Check), **CWE-347** · **OWASP A08:2021** · **Confidence:** High
- **Files:** `install.sh:296-313` (and the documented `curl … | sudo bash` one-liner at `:8`)

**Description.** The binary checksum is verified only *if* the `SHA256SUMS` file is found and *if* the archive name is grepped from it; otherwise the script `warn`s and installs anyway. There is no detached-signature verification, and `SHA256SUMS` is served from the same origin as the binary (not a tamper-resistant root of trust).

```bash
curl -fsSL "${CHECKSUM_URL}" -o "${CHECKSUMS}" 2>/dev/null || \
    warn "Checksum file unavailable — skipping verification"
if [ -f "${CHECKSUMS}" ]; then
    if grep -q "${ARCHIVE_NAME}" "${CHECKSUMS}"; then ... fi   # else: installs with NO check
fi
```

**Exploitation scenario.** A release-asset compromise, a removed/absent `SHA256SUMS`, or any path where the grep misses results in the trojaned `kirra_verifier_service` being installed and run as a systemd service — silently, with only a warning.

**Business impact.** Supply-chain compromise of the safety kernel binary on every operator host.

**Remediation.** Make checksum verification mandatory and fail-closed, and add a signature (cosign/GPG) whose key is distributed out-of-band:

```bash
[ -n "${CHECKSUM_URL}" ] || fatal "No SHA256SUMS — refusing to install unverified binary"
curl -fsSL "${CHECKSUM_URL}" -o "${CHECKSUMS}" || fatal "Cannot fetch checksums"
grep -q "${ARCHIVE_NAME}" "${CHECKSUMS}" || fatal "Archive not listed in SHA256SUMS"
cosign verify-blob --signature SHA256SUMS.sig SHA256SUMS || fatal "Signature verification failed"
```

---

### H-5 — `charts/kirra-verifier` runs the container as root

- **Severity:** High · **CWE-250** (Unnecessary Privileges), **CWE-1188** (Insecure Default) · **OWASP A05:2021** · **Confidence:** High
- **Files:** `charts/kirra-verifier/templates/deployment.yaml` (no `securityContext`); `deploy/docker/Dockerfile.native` (no `USER`)

**Description.** Unlike the well-hardened `helm/kirra` chart and `deploy/systemd` units, this second chart defines no pod/container `securityContext`, no `runAsNonRoot`, no `readOnlyRootFilesystem`, no dropped capabilities, no `serviceAccountName`, and references `Dockerfile.native` which has no `USER` directive → the safety governor runs as **root** with the default ServiceAccount token mounted.

**Exploitation scenario.** Any RCE or container breakout in the verifier executes as root inside the pod and can escalate to node/cluster compromise via the mounted SA token and writable root FS.

**Remediation.** Add pod + container `securityContext` (`runAsNonRoot`, `runAsUser: 1000`, `allowPrivilegeEscalation: false`, `readOnlyRootFilesystem: true`, `capabilities: drop: [ALL]`), `automountServiceAccountToken: false`, and a non-root `USER` in `Dockerfile.native`. Mirror `helm/kirra`.

---

### H-6 — Hardcoded default admin token shipped in chart values

- **Severity:** High · **CWE-798** (Hard-coded Credentials), **CWE-1188** · **OWASP A07:2021** · **Confidence:** High
- **Files:** `charts/kirra-verifier/values.yaml:14`; `charts/kirra-verifier/templates/secret.yaml`

**Description.** The chart ships a **non-empty default** `admin.token: "change-me-in-production-secure-profile"`, b64-encoded into a Secret with **no guard**. `helm/kirra` correctly `{{ fail }}`s when the token is empty; this chart silently deploys a globally-known token if the operator forgets `--set admin.token`.

```yaml
# values.yaml
admin: { existingSecret: "", token: "change-me-in-production-secure-profile" }
# secret.yaml — no fail guard:
data: { admin-token: {{ .Values.admin.token | b64enc | quote }} }
```

**Exploitation scenario.** `helm install` with defaults brings up the governor with a publicly-known `KIRRA_ADMIN_TOKEN`. An attacker authenticates all Tier-2 mutation routes — defeating the entire fail-closed trust model.

**Remediation.** Set `token: ""` and add the same `fail` guard as `helm/kirra`:

```yaml
{{- if not .Values.admin.existingSecret }}{{- if not .Values.admin.token }}{{ fail "admin.token or admin.existingSecret is required" }}{{- end }}{{- end }}
```

---

### Medium findings (condensed)

**M-1 — Permissive CORS (`Any/Any/Any`).** `src/bin/kirra_verifier_service.rs:3850-3853`. `CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any)` applied globally, including `Authorization`. Any web origin can read all public observability JSON cross-origin and, where a browser holds the bearer token (see M-3), drive authenticated requests. tower-http forbids `Any`+credentials, which limits cookie CSRF, but Bearer-in-JS is still exposed. **CWE-942/CWE-346, OWASP A05.** Fix: explicit env-driven origin allowlist; restrict methods/headers; or drop CORS and use the server-side proxy pattern `console/` already implements.

**M-2 — No HTTP rate limiting.** Router `:3787-3897`. No `tower_governor`/`ConcurrencyLimit`. Enables admin-token brute force and Ed25519 CPU-exhaustion DoS on `/attestation/verify` and `/federation/reports/submit`. (The `rate_limiting_events` metric is *kinematic*, unrelated.) **CWE-770/CWE-307, OWASP A04.** Fix: per-IP token bucket + global concurrency limit.

**M-3 — Admin token in browser SPA, cross-origin, plaintext.** `dashboard/src/App.jsx:320-323,509-511`. The Vite dashboard sends `Authorization: Bearer ${token}` directly from the browser to `http://localhost:8090`. Works only because of M-1; exposes the privileged token to any XSS/extension/network observer. Contrast the correct server-side proxy in `console/`. **CWE-522/CWE-319, OWASP A02/A07.** Fix: route through a same-origin token-injecting proxy; force HTTPS.

**M-4 — `curl | sudo bash` primary install, mutable `main`.** `install.sh:8`. Entire safety-kernel install trusts the network at runtime and runs as root from a mutable branch. **CWE-494/CWE-829, OWASP A08.** Fix: download → verify signature → pin to release tag → run.

**M-5 — Admin token via `env_file`/env.** `docker-compose.yml:7-8,42-43`; `.env.example`. Secrets as plain env are visible via `docker inspect`/`/proc/<pid>/environ`; copied-but-unedited `.env` ships a placeholder. **CWE-798/CWE-526, OWASP A05.** Fix: Docker `secrets:` file mounts; startup check rejecting placeholder values.

**M-6 — `npm install` not `npm ci`.** `release.yml:41`; `dashboard/Dockerfile:4`. Lockfile not enforced → non-reproducible release artifacts and transitive-dependency drift in the published dashboard image. **CWE-829/CWE-1357, OWASP A08.** Fix: `npm ci` everywhere a lockfile exists.

**M-7 — Unpinned third-party Actions.** `.github/workflows/*` (`dtolnay/rust-toolchain@stable`, `codecov/codecov-action@v4`, `softprops/action-gh-release@v2`, `docker/*`). Mutable refs in `contents: write` / `packages: write` workflows — real precedent (Codecov 2021, tj-actions 2025). **CWE-829/CWE-494, OWASP A08.** Fix: pin to full commit SHA; enable Dependabot for `github-actions`.

**M-8 — `Dockerfile.native` root + baked empty secret + stray ports.** `deploy/docker/Dockerfile.native:18,20`. No `USER` (root), `ENV KIRRA_SUPERVISOR_RESET_KEY=""` baked in (violates INV-7 intent; surfaces in `docker history`), `EXPOSE 5502 5503 5504 8080` mismatching the documented 8090. **CWE-250/CWE-668, OWASP A05.** Fix: non-root `USER`, remove baked secret, correct `EXPOSE`.

**M-9 — `derive_fleet_posture([])` returns `Nominal`.** `src/posture_engine.rs:45-55`. An empty live node set (cold start before SQLite hydration, partial hydration, or a map-clearing bug) resolves a *fresh* `Nominal`, so `should_route_command` admits everything — and the staleness TTL doesn't catch it because the entry is freshly computed. **CWE-636, OWASP A04.** Fix: on an Active node, treat empty live-node set as `LockedOut`, or cross-check against the SQLite registered count.

**M-10 — Scalar kernel Degraded path is a pure clamp.** `src/kirra_core.rs:235-238`. The `ApplyVelocityCap` arm does `clamp(constraint_cap_min, constraint_cap_max)` with no non-increasing-speed and no no-re-initiation-from-stop check — diverging from the four documented `enforce_degraded_decel_to_stop` enforcement points and SS-002 (the Cruise Oct-2023 re-initiation lesson). Exposed via the C ABI `kirra_filter_move_velocity`. Also `constraint_cap_min/max` are never validated against the contract envelope at construction. **CWE-696/CWE-693, OWASP A04.** Fix: apply the converge-to-zero / no-reinit rule on this arm, or gate the kernel out of degraded actuator authority.

**M-11 — Corridor polygon winding unvalidated.** `crates/kirra-core/src/containment.rs:121-142`. The code itself documents that polygon simplicity/winding is not validated and "is NOT guaranteed to fail in the safe direction." A transposed left/right boundary yields a self-intersecting polygon where even-odd PNPoly can report an outside pose as INSIDE → `Allow`. **CWE-1287, OWASP A04.** Fix: enforce monotone-advancing sides / winding-sign consistency; conservative-reject otherwise.

**M-12 — DAG recursion depth scales with node count.** `src/verifier.rs:451,518`. `max_depth = nodes.len().max(MAX_DEPENDENCY_DEPTH)`; a long linear chain recurses N frames before the backstop fires → stack overflow → `SIGABRT` under `panic=abort` → governor death. In HA the promoted standby recalculates the same persisted graph and dies identically (crash-loop). Admin-gated. **CWE-674/CWE-400, OWASP API4.** Fix: iterative explicit-stack DFS, or an absolute frame cap independent of fleet size.

**M-13 — HA fenced old-primary acts Active for ~1 heartbeat.** `src/standby_monitor.rs:233-255`. Promotion is correctly serialized by a durable SQL CAS, but the fenced old primary self-demotes only on its next heartbeat tick (~2 s). **Mitigated** for mutations by the request-time epoch fence in `enforce_posture_routing` (self-demote + 503 on epoch divergence) and in-transaction `assert_epoch_held`; the residual is reads/non-fenced paths in that window. **CWE-362, OWASP A04.** Likely downgradeable to informational once per-request epoch checks are confirmed on all actuator paths.

---

### Low & Informational findings

See the tables in §3. Highlights worth a ticket:

- **L-1 / L-2 — `verify` vs `verify_strict`.** Two production Ed25519 sites (`fleet-transport/src/lib.rs:477`, `verifier_store.rs:274`) use the malleable `verify` while the rest of the codebase deliberately uses `verify_strict`. Exploitability is bounded (nonce burn / private-key requirement), but fix for one-line crate-wide consistency. **CWE-347.**
- **L-4 — Two admin handlers missing `is_active()` guard.** `register_federation_controller` / `register_node_identity` (`:1641-1686`) lack the standby self-check every sibling mutation has; defense-in-depth only (epoch fence still applies). **CWE-696.**
- **L-5 / L-6 / L-7 — Web hardening:** missing CSP/X-Frame-Options/HSTS/nosniff; unsanitized IDs into `tracing` (log injection, CWE-117); upstream error `detail` leaked to browser (CWE-209).
- **L-8 — `validate_trajectory_slow` wrapper hardcodes `FrameTrust::Trusted`** (`:135-141`) — insecure default bypassing the frame-integrity gate if a production path uses the non-`_capped` form. **CWE-1188.**
- **L-9 / L-10 — Concurrency hygiene:** recover the poisoned recovery-streak guard (`into_inner`, matching the rest of the codebase); use `fetch_max` for the generation seed.
- **L-11 — Commit `parko/Cargo.lock` and add `--locked`** to parko CI to match the root convention.
- **I-1 — `panic = "abort"` crash-loop:** a *deterministic* panic on attacker input is not survived by HA failover (every promoted instance re-processes the same input and dies). M-12 is the one place this is currently reachable; keeping attacker-reachable paths panic-free (this audit largely confirms they are) preserves the property.

---

## 5. Threat Model & Attack Tree

**Goal: issue an unsafe actuator command / subvert fleet safety state.**

```
Compromise Kirra safety governor
├── (A) Steal the admin token
│   ├── A1 Sniff plaintext HTTP on the fleet network         [H-1]  ← lowest-effort, on-path
│   ├── A2 Read it from the browser dashboard SPA            [M-3 + M-1]
│   ├── A3 Read it from docker inspect / /proc/environ       [M-5]
│   ├── A4 Deploy with the public default chart token        [H-6]
│   └── A5 Brute force (no rate limiting)                    [M-2]  ← only if token is weak
│       └── then: register malicious nodes, force posture flips, export state, /fabric/command
├── (B) Defeat the safety checker without credentials
│   ├── B1 Feed NaN/Inf perception → RSS bypass → Accept     [H-2]  ← physical-safety critical
│   ├── B2 Kill a sensor right after registration (~28s)     [H-3]
│   ├── B3 Drive the system to an empty-node-set Nominal      [M-9]
│   ├── B4 Transpose corridor boundaries → containment open  [M-11]
│   └── B5 Re-initiate from stop via the scalar FFI kernel   [M-10]
├── (C) Supply-chain / persistence
│   ├── C1 Trojan the install binary (fail-open checksum)    [H-4]
│   ├── C2 Compromise an unpinned CI action → poison release [M-7]
│   ├── C3 Inject via npm transitive drift                   [M-6]
│   └── C4 root container → node/cluster takeover            [H-5, M-8]
├── (D) Availability / DoS
│   ├── D1 Deep dependency chain → stack overflow → abort    [M-12]
│   └── D2 Flood Ed25519 verify endpoints                    [M-2]
└── (E) Lateral movement
    └── E1 root pod + mounted SA token → cluster             [H-5]
```

**Entry points:** HTTP API (plaintext), industrial adapters, FFI, federation push, install/CI supply chain, browser frontends.
**Lateral movement:** root container → Kubernetes node/cluster (H-5); captured admin token → full fleet trust state.
**Persistence:** trojaned binary via install/CI (H-4, M-7); registered malicious node identities / federation controllers.
**Exfiltration:** `/system/backup/export` with a captured/default token; cross-origin reads of observability JSON (M-1).

---

## 6. Penetration Test Simulation (mental)

| Attack | Result | Evidence |
|--------|--------|----------|
| Authentication bypass | **Not found** — `require_admin_token` on every mutation route, constant-time compare, fail-closed 503 | auth audit VERIFIED SECURE |
| Privilege escalation (app) | **Not found** in core auth; **yes** at infra layer (root container) | H-5 |
| Token theft | **Yes** — plaintext sniff / browser SPA / docker inspect / default chart token | H-1, M-3, M-5, H-6 |
| Injection (SQLi/cmd/path/LLM) | **Not found** — parameterized SQL, bounds-checked decoders, closed-enum LLM parsing | injection audit |
| Safety bypass | **Yes** — NaN RSS bypass; watchdog window; empty-node Nominal | H-2, H-3, M-9 |
| RCE | **Not found** directly; reachable only via supply-chain (trojaned binary/CI) | H-4, M-7 |
| DoS | **Yes** — recursion stack overflow; no rate limiting | M-12, M-2 |
| Replay | **Not found** — nonce single-use burn + TTL + Ed25519 `verify_strict` | crypto/auth audits |
| Supply chain | **Yes** — fail-open install, unpinned actions, `npm install` | H-4, M-7, M-6 |
| SSRF | **Not found** — governor makes no request-controlled outbound calls | network audit |

---

## 7. Architectural Weaknesses

1. **Two divergent deployment paths.** The hardened path (`helm/kirra`, `deploy/systemd`, root `Dockerfile`) coexists with an un-hardened path (`charts/kirra-verifier`, `Dockerfile.native`) that runs as root with a default token. Consolidate or delete the weaker path.
2. **Security boundary assumed external.** TLS, rate limiting, and CORS are effectively delegated to an unspecified ingress, but the application defaults are wide open (plaintext, `Any`, unthrottled) — defense-in-depth requires the app to be safe even if the ingress is misconfigured or bypassed.
3. **Fail-closed discipline is near-total but not uniform.** The strong Priority-0 finiteness guard on the kinematics path is *not* mirrored on the RSS path (H-2) or some predictive/containment paths (M-11, L-15) — the safety invariant should be enforced identically at every checker seam.
4. **`panic = abort` + deterministic-input panics.** HA failover does not survive a deterministic crash-input (I-1); the recursion DoS (M-12) is the live instance of this.
5. **Credential delivery to browsers.** The `console/` server-side proxy is the correct pattern; the `dashboard/` SPA is the anti-pattern. Standardize on the proxy.

---

## 8. Dependency & Supply-Chain Risks

- Root crate dependency versions are **current** (tokio 1.52.3, axum 0.8.9, ring 0.17.14, ed25519-dalek 2.2.0, rusqlite 0.31, time 0.3.48) — **no known-vulnerable pins identified**.
- **No external git/registry dependencies** — all `path =` deps are internal workspace crates (no dependency-confusion vector).
- **No `parko/Cargo.lock`** and parko CI builds without `--locked` (L-11) — the only Rust reproducibility gap.
- Supply-chain exposure is concentrated in **tooling**: install integrity (H-4), unpinned Actions (M-7), `npm install` (M-6), unsigned mutable `:latest` images (L-13).

---

## 9. Remediation Roadmap

### Quick wins (hours–days, high value)
1. **Fix H-2** — add the finiteness gate to the RSS loops (a few lines; closes the most safety-critical bypass).
2. **Fix H-6 / M-8** — set chart token to `""` + `fail` guard; add non-root `USER` to `Dockerfile.native`; remove baked empty supervisor key.
3. **Fix H-4** — make `install.sh` checksum verification mandatory/fail-closed.
4. **Fix M-1** — replace `Any` CORS with an env-driven origin allowlist.
5. **Fix L-1 / L-2** — swap the two production `verify` calls to `verify_strict`.
6. **Fix M-9** — empty live-node set ⇒ `LockedOut` on an Active node.
7. **Fix L-9 / L-10** — recover the poisoned streak guard; `fetch_max` the generation seed.

### Medium term (weeks)
8. **H-1** — terminate TLS in-process (or hard-require a verified mTLS sidecar) and refuse non-loopback plaintext binds.
9. **H-3** — register nodes into the watchdog at registration time (close the ~28 s window).
10. **H-5** — full `securityContext` on `charts/kirra-verifier`; consolidate the two deployment paths.
11. **M-2** — add per-IP rate limiting + global concurrency limit.
12. **M-12** — convert DAG traversal to iterative DFS / absolute frame cap.
13. **M-3** — move the dashboard to a server-side token-injecting proxy.
14. **M-7 / M-6 / L-11** — pin Actions to SHAs, `npm ci`, commit `parko/Cargo.lock` + `--locked`.
15. **M-10 / M-11** — extend decel-to-stop/no-reinit to the scalar kernel; enforce corridor winding validation.

### Long term (strategic)
16. **Uniform fail-closed finiteness contract** at every checker seam, enforced by a shared helper + proptest (prevents H-2-class regressions).
17. **Signed release artifacts** (cosign/SLSA provenance) for binaries and images; image digest pinning in Helm.
18. **Secrets management** — move all tokens/keys to a secret manager / file-mounted secrets; placeholder-rejection at startup; key rotation runbook.
19. **Security headers + CSP** across all served HTML; HSTS once TLS is live.
20. **Structured JSON logging** with input normalization to close log-injection and standardize forensic fields.

---

## 10. Scope & Coverage Statement

**Analyzed (source-read):** the root `kirra-verifier` crate (service binary, verifier/store, attestation, federation, audit chain, posture engine, gateway, adapters, FFI, supervisor, kinematics), `crates/*` (core, ros2-adapter validation/prediction/redundancy, planner, map, taj, fleet-transport, capture-schema), `parko/` (parko-core scheduler/comparator, parko-kirra), web frontends (`console/`, `dashboard/`, `website/`, `static/`), infra (`Dockerfile`(s), `docker-compose.yml`, `helm/`, `charts/`, `deploy/`, `.github/workflows/`), `install.sh` and `scripts/`, and `Cargo.lock`.

**Not exhaustively analyzed (and why):**
- **`#[cfg(feature = "ros2")]` `node.rs`** wiring — requires a sourced ROS 2 (`r2r`) toolchain; the safety-relevant logic it calls (`validation.rs`) *was* reviewed. The env-gate fail-closed defaults were verified by reading the gates, not by building the node.
- **TPM `tpm` feature paths** beyond the quote parser — `tss-esapi` hardware interaction was reviewed at the parsing/verification layer only.
- **Inference backends** (`parko-onnx`/`-openvino`/`-tensorrt`) — hardware/CI-gated native code; not built. These are doer-side inference, not checker authority.
- **Dynamic/runtime testing** — this was a static white-box assessment; no live instance was attacked, no fuzzing was run. The DoS/panic findings are reasoned from code, not crashed in situ. A follow-up fuzzing campaign on the protocol decoders and the RSS/containment inputs is recommended to confirm H-2 and probe for additional panics.

---

*Report generated from a seven-domain parallel source audit. All findings include file:line evidence and were verified against the actual code; no speculative findings are included beyond those explicitly marked Informational/defense-in-depth.*
