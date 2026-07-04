# Model-Integrity Allow-List (#G16)

**Status:** LIVE for the source-model path (CPU + TensorRT backends). Compiled-
engine fingerprint verification and the accelerator watchdog are tracked
remainders (§4).

## 1. The gap

A backend loaded whatever model file it was pointed at. `parko-tensorrt`
*computed* a SHA-256 of its cached engine but only **logged** it;
`parko-onnx` computed **nothing**. So a substituted or corrupted ML artifact
loaded and ran undetected — a fail-**open** in an otherwise fail-closed stack
(`INDUSTRY_BENCHMARK_GAP_ANALYSIS.md` G16).

## 2. The primitive

`parko_core::model_integrity` (pure, no logging, no globals — fully unit-tested):

- `sha256_file(path)` — streaming SHA-256 (bounded memory), fail-closed `Io` on
  an unreadable file.
- `ModelAllowList` — the operator policy: a set of allowed lowercase-hex SHA-256
  digests plus a strict flag. `from_env()` / `parse()` / `from_parts()`.
- `verify_model_file(path, &allow) -> Result<VerifiedModel, BackendError>` — the
  gate. Hashes the file and applies the fail-closed table below.

## 3. Fail-closed policy

| `KIRRA_MODEL_ALLOWLIST` | `KIRRA_MODEL_ALLOWLIST_STRICT` | model digest | result |
|---|---|---|---|
| has entries | any | in the list | `Ok { verified: true }` |
| has entries | any | NOT in the list | **`Err(IntegrityRejected)`** |
| empty / unset | `1`/`true`/`yes`/`on` | (nothing allowed) | **`Err(IntegrityRejected)`** — deny-by-default |
| empty / unset | off | — | `Ok { verified: false }` — enforcement OFF; digest computed for audit, acceptance byte-identical to pre-#G16 |
| (either) | — | file unreadable | **`Err(Io)`** — a model that cannot be hashed cannot be proven |

Enforcement is **opt-in**: configuring an allow-list turns it on (matching the
repo's other env-gated safety gates), so absent config never rejects a
previously-accepted model, but a configured operator gets hard substitution
detection and `STRICT` gives the deny-by-default posture.

### Env vars

| Var | Meaning |
|---|---|
| `KIRRA_MODEL_ALLOWLIST` | Comma/space/`;`-separated SHA-256 hex digests. Each entry may be a bare `<64-hex>` or `name=<64-hex>` (only the digest is compared). Case-insensitive; malformed entries are ignored (never match). |
| `KIRRA_MODEL_ALLOWLIST_STRICT` | `1`/`true`/`yes`/`on` → deny any model whose digest is not explicitly listed, even when the list is empty. Off by default. |

## 4. Wiring & coverage

The gate lives in `parko_onnx::session_core::OrtRunCore::load_model` — the
**single-sourced** inference core. Because **both** `parko-onnx` (`OrtBackend`,
CPU) and `parko-tensorrt` (`TrtBackend`, TensorRT EP) route `load_model` through
`OrtRunCore`, the source-model substitution defense covers both backends from one
call site. A rejected model returns `Err` from `load_model` and never yields a
runnable `ModelHandle`.

**Tracked remainders (not in this slice):**
1. **Compiled-engine fingerprint verification** — `parko-tensorrt` computes an
   `engine_sha256` of the *compiled* TRT engine (deployment/driver-specific).
   Verifying it against an engine allow-list is a follow-up; the *source* model
   (what gets substituted upstream) is already gated here.
2. **Accelerator watchdog** — a hung inference call. Partially covered already by
   the scheduler's per-tick inference deadline (`parko_core::scheduler`,
   `with_inference_deadline_ms`); a dedicated per-call accelerator watchdog is a
   separate follow-up.

## 5. Test traceability

| Property | Test (`parko-core::model_integrity`) |
|---|---|
| Known SHA-256 vector ("abc") | `digest_is_stable_and_matches_known_vector` |
| Matching digest verified | `matching_digest_is_verified` |
| Substituted model rejected (fail-closed) | `substituted_model_is_rejected_fail_closed` |
| Enforcement off → accept + unverified | `enforcement_off_accepts_but_reports_unverified` |
| Strict + empty list → deny-by-default | `strict_mode_rejects_when_allowlist_empty` |
| Unreadable file fails closed | `unreadable_file_fails_closed_even_when_enforcement_off` |
| Env parse (forms/case/junk/strict) | `parse_reads_env_forms`, `parse_unset_is_not_enforcing` |
| Real mnist artifact reject/accept (backend seam) | `parko-onnx::model_integrity_allowlist_gates_the_real_mnist_artifact` |
