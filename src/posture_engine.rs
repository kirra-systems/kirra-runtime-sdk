// src/posture_engine.rs
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::posture_cache::{now_ms, CachedFleetPosture, SharedPostureCache};
use crate::verifier::{AppState, FleetNodePosture, FleetPosture};

pub use crate::posture_cache::POSTURE_CACHE_TTL_MS;

// ---------------------------------------------------------------------------
// Generation counter — monotonic across process lifetime
// Initialized from persisted value at boot via init_generation_from_store().
// ---------------------------------------------------------------------------

/// Monotonically increasing generation counter for the posture cache.
/// Initialized to 1; first emitted generation is 1.
pub static POSTURE_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Initialize the generation counter from the last persisted value.
/// Call once at service startup after VerifierStore is opened, BEFORE the
/// first recalculation claims a generation — the service binary does this
/// right after the initial node/dependency load (WS-0.2 / #G10: the call was
/// previously missing from the binary entirely, so every restart reset the
/// live counter to 1 while the store held the high-water mark; emitted
/// generations regressed across restarts and `save_last_generation` rejected
/// every persist until the counter caught up).
///
/// Returns the persisted high-water that was loaded (0 = fresh store).
/// FAIL-CLOSED: a store read error is returned as `Err` — the caller must
/// treat an unreadable generation high-water like any other unreadable
/// startup state (refuse to serve), because proceeding would silently
/// re-introduce the cross-restart time-reversal this function exists to
/// prevent. (The prior signature swallowed the error via `unwrap_or(0)`.)
pub fn init_generation_from_store(app: &Arc<AppState>) -> Result<u64, String> {
    let last = app
        .store
        .with(|store| store.load_last_generation())
        .map_err(|e| e.to_string())?;
    if last > 0 {
        // B6: `fetch_max`, not `store`. If any recalc already advanced the counter
        // past `last + 1` before this init runs (e.g. a cold-start recalc), a bare
        // `store` would move the generation BACKWARDS — violating the strict-
        // monotonicity invariant that federation peers rely on for report ordering.
        // `fetch_max` only ever raises it, so the counter is monotone regardless of
        // init/recalc ordering.
        //
        // #771 F4: `checked_add`, not `last + 1`. `load_last_generation` already
        // rejects a stored high-water `>= i64::MAX`, so `last + 1` cannot overflow
        // in practice — but the release profile builds with overflow-checks OFF, so
        // a bare `+ 1` on a `u64::MAX` that slipped past the store guard would wrap
        // to 0 and fail OPEN (fetch_max(0) is a no-op → counter stays 1 → the exact
        // restart time-reversal). Belt-and-suspenders: overflow is corruption, fail
        // closed.
        let next = last.checked_add(1).ok_or_else(|| {
            format!("generation high-water {last} at u64::MAX — refusing to overflow the counter (corruption)")
        })?;
        POSTURE_GENERATION.fetch_max(next, Ordering::SeqCst);
    }
    Ok(last)
}

/// Returns the next generation number, strictly monotonically increasing.
pub fn next_generation() -> u64 {
    POSTURE_GENERATION.fetch_add(1, Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Posture derivation — pure, independently testable
// ---------------------------------------------------------------------------

/// Derives aggregate fleet posture from per-node posture results.
///
/// Priority order:
///   1. Any LockedOut node → FleetPosture::LockedOut  (early return)
///   2. Any Degraded node  → FleetPosture::Degraded
///   3. All nominal        → FleetPosture::Nominal
///
/// An EMPTY set aggregates to `Nominal` here — correct at this pure layer (no
/// node is Degraded/LockedOut). The fail-closed POLICY for an empty *live* set
/// on an Active verifier ("no nodes" ≠ "healthy") lives one layer up in
/// `recalculate_and_broadcast` (M-9), which overrides this to LockedOut.
///
/// Pure function — no I/O, no side effects.
// ADR-0035 Stage 3 (slice 3a): the pure fleet-fold moved to the lean
// `kirra-safety-authority` crate; re-exported so `crate::posture_engine::derive_fleet_posture`
// (and the `derive_fleet_posture` tests below) resolve unchanged.
pub use kirra_safety_authority::derive_fleet_posture;

// ---------------------------------------------------------------------------
// Core engine — single authoritative write path to SharedPostureCache
// ---------------------------------------------------------------------------

/// Recomputes fleet posture from the live DAG and propagates the result
/// atomically to the posture cache and SSE broadcast channel.
///
/// This is the ONLY function permitted to write to SharedPostureCache.
/// No handler, middleware, or task may write to the cache directly.
///
/// Ordering guarantee: persist → cache replace → broadcast
/// Subscribers never observe a posture transition that hasn't been
/// committed to the audit chain.
///
/// PassiveStandby: DAG is traversed and audit chain is written, but the
/// cache is NOT updated and no broadcast is emitted.
pub fn recalculate_and_broadcast(app: &Arc<AppState>, cache: &SharedPostureCache) {
    let ts = now_ms();

    // Step 1: Traverse the full DAG for every registered node.
    //
    // B1 (deadlock hazard): snapshot the node ids FIRST, releasing the
    // `app.nodes` shard guards, THEN traverse. The previous form held a
    // `nodes.iter()` shard read-guard across each `calculate_posture()` call,
    // which re-locks `app.nodes` / `app.dependency_graph` inside
    // `recursive_calculate`. A re-entrant `get()` on the SAME shard while a writer
    // is queued on it can self-deadlock (DashMap's per-shard RwLock is
    // writer-preferring) and hang the safety engine. Collecting the keys to an
    // owned Vec drops every iterator guard before any traversal begins.
    // SAFETY: SG-RED-2 — snapshot iteration prevents nested DashMap locks.
    // SAFETY: SG-RED-3 — posture DAG recalculation must be deadlock-free.
    let node_ids: Vec<String> = app.nodes.iter().map(|e| e.key().clone()).collect();
    // P3: ONE shared `black` memo across the whole-fleet traversal. Each node's
    // fully-evaluated posture is root-independent, so a node depended on by K
    // others is traversed once and black-hit by the rest — O(N·(N+E)) → ~O(N+E).
    // (The per-call gray cycle-detection set stays fresh inside
    // `calculate_posture_memoized`.)
    let mut black: std::collections::HashMap<
        std::sync::Arc<str>,
        std::sync::Arc<FleetNodePosture>,
    > = std::collections::HashMap::new();
    let node_postures: Vec<FleetNodePosture> = node_ids
        .iter()
        .map(|id| app.calculate_posture_memoized(id, &mut black))
        .collect();

    // Step 2: Derive aggregate posture — pure function, no I/O.
    let dag_posture = derive_fleet_posture(&node_postures);

    // M-9 (fail-closed on an empty live-node set): the pure `derive_fleet_posture`
    // aggregates an empty set to `Nominal` — correct AT THAT LAYER (no node is
    // LockedOut/Degraded), but on an ACTIVE verifier "no nodes" is "no positive
    // trust evidence", not "the fleet is healthy". Caching Nominal here is
    // fail-OPEN: `should_route_command` reads Nominal as "admit everything",
    // so an actuator command would be authorized while the governor is blind.
    //
    // So an empty live set forces LockedOut (blocks all command routing). It is
    // NOT sticky — it is recomputed every recalc, so it auto-recovers to the
    // DAG posture the instant a node is registered (registration routes are not
    // posture-gated), with no human reset. The store cross-check only selects the
    // REASON for observability: a hydration/consistency gap (durable registry
    // non-empty while memory is empty) vs a genuinely empty fleet (cold start /
    // nothing deployed). Queried ONLY on the empty path, so the steady-state hot
    // path takes no extra store lock.
    let empty_live_set = node_ids.is_empty();
    let empty_set_reason = if empty_live_set {
        // SAFETY: SG-HA-3 — durable store reads on runtime paths must be offloaded by callers.
        let registered = app.store.with(|store| store.count_nodes()).unwrap_or(0);
        Some(if registered > 0 {
            "EMPTY_LIVE_SET_HYDRATION_GAP"
        } else {
            "EMPTY_LIVE_SET_NO_NODES"
        })
    } else {
        None
    };

    // RSS / flood escalation: an active RSS violation OR active flood condition
    // elevates Nominal to Degraded. The Nominal-only guard means LockedOut /
    // Degraded (from the DAG) are NEVER downgraded by this check; the two
    // conditions compose (either → Degraded); and recovery is automatic — when
    // both flags clear and the DAG is Nominal, posture returns to Nominal via
    // this same path (no separate recovery logic).
    // SAFETY: SG4 | REQ: flood-posture-coupling | TEST: test_flood_active_nominal_escalates_to_degraded,test_flood_active_locked_out_stays_locked_out,test_flood_active_degraded_stays_degraded,test_flood_and_rss_compose,test_flood_clears_auto_recovers_to_nominal,test_flood_default_false_is_inert
    // S-FI1d frame-integrity coupling: a sub-trusted frame (Degraded OR transient
    // Untrusted) escalates Nominal → Degraded exactly like RSS/flood — the
    // decel-to-stop MRC is the frame-trust-minimal maneuver. Composes with the
    // others (any → Degraded); auto-recovers when the flag clears.
    // SAFETY: SG2 | REQ: frame-integrity-posture-coupling | TEST: test_frame_degraded_active_escalates_nominal,test_frame_degraded_active_locked_out_stays_locked_out,test_frame_and_rss_compose,test_frame_degraded_clears_auto_recovers_to_nominal
    // S-DG1 governor-divergence coupling: a posture-significant disagreement
    // between the two diverse governors escalates Nominal → Degraded exactly
    // like RSS/flood/frame — one of the two safety authorities is wrong and we
    // cannot tell which, so decel-to-stop is the only defensible disposition.
    // Composes with the others (any → Degraded); auto-recovers when agreeing
    // ticks clear the flag.
    // SAFETY: SG9 | REQ: governor-divergence-posture-coupling | TEST: test_divergence_degraded_active_escalates_nominal,test_divergence_lockout_active_forces_locked_out,test_divergence_flags_default_inert
    let escalate = (app
        .rss_active_violation
        .load(std::sync::atomic::Ordering::SeqCst)
        || app
            .flood_condition_active
            .load(std::sync::atomic::Ordering::SeqCst)
        || app
            .frame_degraded_active
            .load(std::sync::atomic::Ordering::SeqCst)
        || app
            .divergence_degraded_active
            .load(std::sync::atomic::Ordering::SeqCst))
        && dag_posture == FleetPosture::Nominal;
    // C2 supervisor escalation has ABSOLUTE priority over the DAG and the
    // operational (rss/flood) escalation: if a critical background safety loop is
    // wedged past its restart budget, `supervisor_tripped` is set and the whole
    // fleet is forced LockedOut here. Because the engine itself honors the flag,
    // the forced LockedOut STICKS across every subsequent recalc (a recovered DAG
    // can never silently downgrade it). Recovery is a human/HA reset, matching
    // LockedOut semantics. SAFETY: SG9 fail-closed on safety-loop death (review C2).
    // A sustained frame-integrity fault (`frame_lockout_active`) shares the
    // absolute LockedOut priority with `supervisor_tripped`: both are sticky
    // human-reset conditions that override the DAG and the operational escalation.
    let new_posture = if app
        .supervisor_tripped
        .load(std::sync::atomic::Ordering::SeqCst)
        || app
            .frame_lockout_active
            .load(std::sync::atomic::Ordering::SeqCst)
        || app
            .divergence_lockout_active
            .load(std::sync::atomic::Ordering::SeqCst)
    {
        FleetPosture::LockedOut
    } else if empty_live_set {
        // M-9: no live nodes → no positive trust evidence → fail closed.
        // Shares LockedOut with the sticky flags but is itself non-sticky
        // (auto-recovers when a node registers — see the comment above).
        FleetPosture::LockedOut
    } else if escalate {
        FleetPosture::Degraded
    } else {
        dag_posture
    };

    // Step 3: Read previous posture for transition deduplication.
    let previous_posture: Option<FleetPosture> = cache
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|c| c.posture));

    let is_transition = previous_posture
        .as_ref()
        .map(|prev| prev != &new_posture)
        .unwrap_or(true);

    let generation = next_generation();

    // Step 4: Persist to audit chain (disk-first, invariant #12).
    //
    // The doc above promises subscribers never observe a transition that
    // hasn't been committed to the audit chain. That requires the cache
    // write AND the broadcast to be gated on a SUCCESSFUL audit commit —
    // a failed/skipped audit must NOT yield an enforced posture change.
    // We capture the outcome here and fail closed (return without
    // touching the cache or broadcast) if the commit did not land.
    //
    // Consuming a generation and then bailing leaves a harmless gap in
    // the generation sequence; monotonicity is preserved.
    let audit_payload = serde_json::json!({
        "new_posture":      format!("{new_posture:?}"),
        "previous_posture": previous_posture.as_ref().map(|p| format!("{p:?}")),
        "is_transition":    is_transition,
        "generation":       generation,
        "node_count":       node_postures.len(),
        "empty_set_reason": empty_set_reason,
        "computed_at_ms":   ts,
    });

    if let Some(reason) = empty_set_reason {
        tracing::warn!(
            reason     = reason,
            generation = generation,
            "M-9 fail-closed: empty live-node set on an Active verifier — forcing LockedOut (no positive trust evidence)"
        );
    }

    let event_type = if is_transition {
        "SYSTEM_POSTURE_TRANSITION"
    } else {
        "POSTURE_CACHE_REFRESHED"
    };

    // SAFETY: SG-HA-3 — durable writes must not execute on Tokio workers.
    // `recalculate_and_broadcast` is run on blocking/offline paths when called from async workers.
    let payload = audit_payload.to_string();
    let audit_committed = app.store.with(|store| {
        // #771 F2: the posture event, its audit-chain link, AND the generation
        // high-water commit in ONE transaction (either variant below) — a failed
        // commit lands none of them and we fail closed, closing the cross-restart
        // window where the chain could hold a duplicate/regressed generation.
        //
        // #772 F2: an incident-class TRANSITION is written DIRECTLY on the FULL
        // (synchronous=FULL) connection, so the commit ITSELF fsyncs the WAL — the
        // row is hard-power-loss durable at write time, atomically, with no
        // separate marker and no cross-connection piggyback inference. The 20 Hz
        // POSTURE_CACHE_REFRESHED traffic stays on the NORMAL connection (INV-12
        // throughput). #772 F3: if the durable write FAILS we do NOT suppress the
        // (possibly escalating) transition over a degraded durability guarantee —
        // we fall back to the NORMAL write so the row still lands (durable to the
        // next checkpoint) and count the degradation for /metrics.
        let write_result = if is_transition {
            match store.save_posture_event_chained_with_generation_durable(
                "posture_engine",
                event_type,
                &payload,
                Some("Fleet posture recomputed from DAG traversal"),
                ts,
                generation,
            ) {
                Ok(advanced) => Ok(advanced),
                Err(e) => {
                    // Relaxed: a monotonic best-effort observability counter that
                    // gates nothing and synchronizes with no other memory — matching
                    // how the /metrics snapshot loads it (see fleet.rs).
                    app.incident_durability_failures
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::error!(
                        error = %e,
                        generation,
                        "INCIDENT-DURABILITY DEGRADED: FULL-connection write of a posture transition failed — falling back to the checkpoint-bounded write; the transition is NOT suppressed (WS-0.3 / #772 F3)"
                    );
                    store.save_posture_event_chained_with_generation(
                        "posture_engine",
                        event_type,
                        &payload,
                        Some("Fleet posture recomputed from DAG traversal"),
                        ts,
                        generation,
                    )
                }
            }
        } else {
            store.save_posture_event_chained_with_generation(
                "posture_engine",
                event_type,
                &payload,
                Some("Fleet posture recomputed from DAG traversal"),
                ts,
                generation,
            )
        };

        match write_result {
            Ok(high_water_advanced) => {
                // #695: a `false` means the in-tx monotonic guard rejected this
                // generation as stale — a benign concurrent-recalc race (another
                // recalc already persisted a higher generation); logged at debug.
                // A PERSISTENT rejection of the latest generation would indicate a
                // regression worth investigating. The posture event still committed
                // in the same tx, so the live transition proceeds.
                if !high_water_advanced {
                    tracing::debug!(
                        generation,
                        "Generation high-water rejected a stale persist in-tx (benign race unless persistent) — #695/#771"
                    );
                }
                true
            }
            // SAFETY: SG-HA-4 — DB errors demote node to safe state (fail-closed).
            // Reached only when BOTH the durable write AND the NORMAL-connection
            // fallback failed (a genuinely wedged store), so the chain has no row —
            // suppress cache/broadcast.
            Err(e) => {
                tracing::error!(
                    error      = %e,
                    generation = generation,
                    "AUDIT-CHAIN WRITE FAILED for posture transition — suppressing cache/broadcast (fail closed)"
                );
                false
            }
        }
    });

    if !audit_committed {
        return;
    }

    // #104: post-incident forensic sequence — OBSERVABILITY ONLY. Runs only
    // after the posture transition is committed to the chain (above); it never
    // perturbs or blocks the cache/broadcast path below. A failed forensic write
    // bumps `post_incident_write_failures` and is dropped, never propagated.
    // Emitted on both Active and PassiveStandby (whichever node wrote the
    // posture audit), before the PassiveStandby early-return.
    crate::post_incident::record_posture_transition(
        app,
        previous_posture.as_ref(),
        &new_posture,
        is_transition,
        generation,
        ts,
    );

    // Step 5: PassiveStandby — audit only, no cache or broadcast mutation.
    if !app.is_active() {
        tracing::debug!(
            posture    = ?new_posture,
            generation = generation,
            "PassiveStandby: posture audited; cache and broadcast suppressed"
        );
        return;
    }

    // Step 6: Generation-monotonic cache replace.
    // Two recalcs can race (promotion path + Step-C worker), and a SLOWER
    // one carrying a LOWER generation must not clobber a newer posture.
    let new_cached = CachedFleetPosture::new_with_generation(new_posture, generation, ts);
    // #688: read the sticky-lockout flags HERE (after the generation grab at
    // `next_generation()` above, just before the write) so a recalc that predates a
    // supervisor/frame trip cannot clobber the forced LockedOut — see
    // `replace_cache_if_newer`. This ordering is LOAD-BEARING and proven so: the
    // loom model `sticky_lockout_never_downgraded_under_recalc_race`
    // (`crates/kirra-loom-models`) fails (finds a downgrade interleaving) if this
    // read is moved BEFORE the generation grab. Do not reorder.
    let sticky_lockout = app
        .supervisor_tripped
        .load(std::sync::atomic::Ordering::SeqCst)
        || app
            .frame_lockout_active
            .load(std::sync::atomic::Ordering::SeqCst)
        || app
            .divergence_lockout_active
            .load(std::sync::atomic::Ordering::SeqCst);
    let cache_written = replace_cache_if_newer(cache, new_cached, sticky_lockout);

    // Step 7: Broadcast ONLY if we actually wrote a newer entry AND it's a
    // transition. A broadcast without a corresponding cache update would
    // mislead subscribers.
    if cache_written && is_transition {
        // WS-0.5 / #774 F4 — count the transition for /metrics HERE, gated on the
        // SAME `cache_written && is_transition` as the broadcast, so
        // `kirra_posture_transitions_total` reconciles exactly with the SSE stream
        // and the cache generation. Counting it earlier (right after the audit
        // commit) over-counted: a PassiveStandby early-returns before this point
        // (audits but never enforces/broadcasts), and an Active recalc whose
        // monotonic CAS lost the race (`cache_written == false`) never reached
        // subscribers — both would tick a transition nobody observed. The audit
        // CHAIN still records every committed transition (the forensic ledger); the
        // METRIC now tracks enforced/broadcast transitions.
        app.fleet_metrics.record_transition(&new_posture);

        let _ = app.posture_tx.send(crate::verifier::PostureStreamEvent {
            event_type: event_type.to_string(),
            node_id: None,
            emitted_at_ms: ts,
            posture: None,
        });

        tracing::info!(
            new_posture      = ?new_posture,
            previous_posture = ?previous_posture,
            generation       = generation,
            "Fleet posture transition"
        );
    }
}

/// Force the posture cache to `LockedOut` immediately (C2 supervisor escalation).
///
/// Writes a `LockedOut` entry with a freshly-bumped generation so it wins the
/// monotonic compare-and-swap, WITHOUT going through `recalculate_and_broadcast`
/// (which may be the very task that died). Callers MUST also set
/// `AppState::supervisor_tripped` so any *surviving* recalc keeps producing
/// LockedOut — this function only makes the lockout instantaneous; the sticky flag
/// makes it durable. Fail-closed by construction: a poisoned cache lock is the
/// gate's own LockedOut signal, so a failed write here still denies.
pub fn force_lockout(cache: &SharedPostureCache, ts_ms: u64) {
    let candidate =
        CachedFleetPosture::new_with_generation(FleetPosture::LockedOut, next_generation(), ts_ms);
    // sticky_lockout = true: this IS the supervisor escalation. The guard only
    // blocks NON-LockedOut candidates, so a LockedOut candidate is unaffected;
    // passing true documents intent and is correct under the gen CAS.
    let wrote = replace_cache_if_newer(cache, candidate, true);
    tracing::error!(
        wrote_cache = wrote,
        "C2 escalation: posture cache forced to LockedOut (critical safety loop wedged)"
    );
}

/// Replaces the cached posture ONLY if `candidate.generation` is strictly
/// greater than the currently cached generation. Returns `true` if a write
/// landed. Prevents a slow / out-of-order recalc (lower generation) from
/// clobbering a newer posture already in the cache. Pure w.r.t. callers —
/// holds the cache write lock for the duration of the compare-and-swap.
fn replace_cache_if_newer(
    cache: &SharedPostureCache,
    candidate: CachedFleetPosture,
    sticky_lockout: bool,
) -> bool {
    match cache.write() {
        Ok(mut guard) => {
            // #688: a supervisor / frame-integrity sticky LockedOut has ABSOLUTE
            // priority. Without this guard, a recalc that computed a non-LockedOut
            // posture BEFORE the trip — but grabbed a HIGHER generation (the trip
            // landing between its flag read and its generation grab) — could win the
            // generation CAS and clobber `force_lockout`'s LockedOut, so the
            // supervisor escalation was not guaranteed to be IMMEDIATE (only
            // eventually re-forced by the next recalc). While the sticky flag is set
            // we refuse any non-LockedOut candidate, so a stale-posture recalc can
            // never downgrade the forced lockout. (`sticky_lockout` is read by the
            // caller from `supervisor_tripped || frame_lockout_active`; the residual
            // read-vs-trip micro-window is closed by the generation CAS below, since
            // `force_lockout`'s generation is always grabbed after the racing
            // recalc's.) Recovery from a sticky lockout is a human/HA reset that
            // clears the flag, after which a normal recalc resumes writing.
            if sticky_lockout && candidate.posture != FleetPosture::LockedOut {
                tracing::warn!(
                    candidate_posture = ?candidate.posture,
                    candidate_gen = candidate.generation,
                    "Refusing to downgrade a supervisor/frame sticky LockedOut (#688)"
                );
                return false;
            }
            let cur_gen = guard.as_ref().map(|c| c.generation).unwrap_or(0);
            if candidate.generation > cur_gen {
                *guard = Some(candidate);
                true
            } else {
                tracing::debug!(
                    candidate_gen = candidate.generation,
                    current_gen = cur_gen,
                    "Skipping cache replace — a newer or equal generation is already cached"
                );
                false
            }
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "Posture cache RwLock poisoned — cache not updated"
            );
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod posture_engine_tests {
    use super::*;
    use crate::verifier::{FleetNodePosture, FleetPosture, NodeTrustState};

    fn nominal(id: &str) -> FleetNodePosture {
        FleetNodePosture {
            node_id: Arc::from(id),
            local_status: NodeTrustState::Trusted,
            propagated_status: FleetPosture::Nominal,
            blocked_by: vec![],
        }
    }

    fn degraded(id: &str, blocked_by: &str) -> FleetNodePosture {
        FleetNodePosture {
            node_id: Arc::from(id),
            local_status: NodeTrustState::Untrusted("test".to_string()),
            propagated_status: FleetPosture::Degraded,
            blocked_by: vec![Arc::from(blocked_by)],
        }
    }

    fn locked(id: &str, blocked_by: &str) -> FleetNodePosture {
        FleetNodePosture {
            node_id: Arc::from(id),
            local_status: NodeTrustState::Untrusted("test".to_string()),
            propagated_status: FleetPosture::LockedOut,
            blocked_by: vec![Arc::from(blocked_by)],
        }
    }

    #[test]
    fn test_all_nominal_produces_nominal() {
        let nodes = vec![nominal("a"), nominal("b"), nominal("c")];
        assert_eq!(derive_fleet_posture(&nodes), FleetPosture::Nominal);
    }

    #[test]
    fn test_empty_fleet_produces_nominal() {
        assert_eq!(derive_fleet_posture(&[]), FleetPosture::Nominal);
    }

    #[test]
    fn test_single_degraded_produces_degraded() {
        let nodes = vec![nominal("a"), degraded("b", "sensor"), nominal("c")];
        assert_eq!(derive_fleet_posture(&nodes), FleetPosture::Degraded);
    }

    #[test]
    fn test_single_locked_out_produces_locked_out() {
        let nodes = vec![nominal("a"), locked("b", "dep"), nominal("c")];
        assert_eq!(derive_fleet_posture(&nodes), FleetPosture::LockedOut);
    }

    #[test]
    fn test_locked_out_dominates_degraded() {
        let nodes = vec![degraded("a", "x"), locked("b", "y"), nominal("c")];
        assert_eq!(derive_fleet_posture(&nodes), FleetPosture::LockedOut);
    }

    #[test]
    fn test_locked_out_early_return_on_first_occurrence() {
        let nodes = vec![locked("a", "x"), degraded("b", "y"), nominal("c")];
        assert_eq!(derive_fleet_posture(&nodes), FleetPosture::LockedOut);
    }

    #[test]
    fn test_multiple_degraded_does_not_escalate_to_locked_out() {
        let nodes = vec![degraded("a", "x"), degraded("b", "y"), degraded("c", "z")];
        assert_eq!(derive_fleet_posture(&nodes), FleetPosture::Degraded);
    }

    #[test]
    fn test_generation_counter_is_strictly_increasing() {
        let g1 = next_generation();
        let g2 = next_generation();
        let g3 = next_generation();
        assert!(g2 > g1);
        assert!(g3 > g2);
    }

    #[test]
    fn test_transition_detection_none_previous_is_always_transition() {
        let previous: Option<FleetPosture> = None;
        let is_transition = previous
            .as_ref()
            .map(|p| p != &FleetPosture::Nominal)
            .unwrap_or(true);
        assert!(is_transition);
    }

    #[test]
    fn test_transition_detection_same_posture_is_not_transition() {
        let previous = Some(FleetPosture::Nominal);
        let is_transition = previous
            .as_ref()
            .map(|p| p != &FleetPosture::Nominal)
            .unwrap_or(true);
        assert!(!is_transition);
    }

    #[test]
    fn test_transition_detection_different_posture_is_transition() {
        let previous = Some(FleetPosture::Nominal);
        let is_transition = previous
            .as_ref()
            .map(|p| p != &FleetPosture::Degraded)
            .unwrap_or(true);
        assert!(is_transition);
    }

    fn registered_trusted_node(id: &str) -> crate::verifier::RegisteredNode {
        crate::verifier::RegisteredNode {
            node_id: id.to_string(),
            status: NodeTrustState::Trusted,
            registered_at_ms: 1,
            last_trust_update_ms: 1,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: None,
            firmware_version: None,
        }
    }

    #[test]
    fn test_recalculate_and_broadcast_writes_to_cache() {
        use crate::verifier::{AppState, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use std::sync::Arc;

        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        // A live, Trusted node — so the happy path genuinely derives Nominal
        // (M-9: an EMPTY live set now fails closed to LockedOut, see the
        // dedicated tests below).
        app.persist_and_insert_node(registered_trusted_node("node-1"))
            .unwrap();

        recalculate_and_broadcast(&app, &cache);

        // Happy path: audit committed → cache + broadcast may proceed.
        let guard = cache.read().unwrap();
        assert!(guard.is_some(), "cache must be populated after recalculate");
        let entry = guard.as_ref().unwrap();
        assert_eq!(entry.posture, FleetPosture::Nominal);
        assert!(entry.generation > 0);
    }

    // M-9 fail-closed: an empty live-node set on an Active verifier is "no
    // positive trust evidence", NOT "healthy". The pure `derive_fleet_posture`
    // still aggregates `[]` → Nominal (test above), but the engine overrides it.

    #[test]
    fn test_empty_live_set_fails_closed_to_locked_out() {
        use crate::verifier::{AppState, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use std::sync::Arc;

        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        // Genuinely empty fleet (durable registry is empty too) — still must NOT
        // certify Nominal; an Active governor with nothing to govern fails closed.
        recalculate_and_broadcast(&app, &cache);

        let guard = cache.read().unwrap();
        assert_eq!(
            guard.as_ref().unwrap().posture,
            FleetPosture::LockedOut,
            "an empty live-node set must fail closed to LockedOut, never Nominal"
        );
    }

    #[test]
    fn test_empty_live_set_with_orphaned_store_nodes_is_locked_out() {
        use crate::verifier::{AppState, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use std::sync::Arc;

        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        // Hydration/consistency gap: the durable registry holds a node, but the
        // in-memory live set was never populated with it (write to the store
        // ONLY — bypassing `persist_and_insert_node`'s memory insert). This is
        // the dangerous fail-open the cross-check targets; it must fail closed.
        app.store
            .with(|s| s.save_node(&registered_trusted_node("orphan")))
            .unwrap();
        assert!(
            app.nodes.is_empty(),
            "in-memory live set must be empty for this case"
        );

        recalculate_and_broadcast(&app, &cache);

        let guard = cache.read().unwrap();
        assert_eq!(
            guard.as_ref().unwrap().posture,
            FleetPosture::LockedOut,
            "an empty live set while the store holds nodes (hydration gap) must fail closed"
        );
    }

    #[test]
    fn test_empty_live_set_lockout_auto_recovers_on_registration() {
        use crate::verifier::{AppState, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use std::sync::Arc;

        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        // Empty → LockedOut.
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache.read().unwrap().as_ref().unwrap().posture,
            FleetPosture::LockedOut
        );

        // The empty-set LockedOut is NOT sticky (unlike supervisor_tripped):
        // registering a Trusted node and recalculating recovers to Nominal with
        // no human reset.
        app.persist_and_insert_node(registered_trusted_node("node-1"))
            .unwrap();
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache.read().unwrap().as_ref().unwrap().posture,
            FleetPosture::Nominal,
            "empty-set LockedOut must auto-recover once a Trusted node registers"
        );
    }

    #[test]
    fn test_supervisor_tripped_forces_locked_out_over_healthy_dag() {
        use crate::verifier::{AppState, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        // Healthy empty DAG would normally be Nominal (see the test above).
        // Trip the C2 supervisor flag: recalc must force LockedOut regardless.
        app.supervisor_tripped.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        {
            let guard = cache.read().unwrap();
            assert_eq!(
                guard.as_ref().unwrap().posture,
                FleetPosture::LockedOut,
                "supervisor_tripped must force LockedOut over a healthy DAG"
            );
        }

        // Sticky: a subsequent recalc (e.g. DAG still healthy) must NOT downgrade.
        recalculate_and_broadcast(&app, &cache);
        {
            let guard = cache.read().unwrap();
            assert_eq!(
                guard.as_ref().unwrap().posture,
                FleetPosture::LockedOut,
                "forced LockedOut must STICK across recalcs while the flag is set"
            );
        }
    }

    #[test]
    fn test_force_lockout_writes_locked_out_with_bumped_generation() {
        use crate::posture_cache::CachedFleetPosture;
        use std::sync::Arc;

        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
            CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 1, 1_000),
        )));

        force_lockout(&cache, 2_000);

        let guard = cache.read().unwrap();
        let entry = guard.as_ref().unwrap();
        assert_eq!(entry.posture, FleetPosture::LockedOut);
        assert!(
            entry.generation > 1,
            "force_lockout must bump the generation to win the CAS"
        );
    }

    #[test]
    fn test_passive_standby_audits_but_does_not_write_cache() {
        use crate::verifier::{AppState, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use std::sync::Arc;

        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::PassiveStandby));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        recalculate_and_broadcast(&app, &cache);

        let guard = cache.read().unwrap();
        assert!(
            guard.is_none(),
            "PassiveStandby must NOT populate the cache even after a successful audit commit"
        );
    }

    // FIX 1: generation-monotonic replace.
    //
    // NOTE: each `cache.read()` is scoped in its own block. std::sync::RwLock
    // does not guarantee write-prefer scheduling, and holding a read guard
    // across a subsequent `replace_cache_if_newer` (which acquires the
    // write lock) deadlocks.
    #[test]
    fn test_replace_cache_if_newer_rejects_lower_generation() {
        use crate::posture_cache::CachedFleetPosture;
        use std::sync::Arc;

        fn snapshot(cache: &SharedPostureCache) -> (u64, FleetPosture) {
            let g = cache.read().unwrap();
            let entry = g.as_ref().expect("cache populated");
            (entry.generation, entry.posture)
        }

        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        // Seed with generation 10.
        let g10 = CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 10, 1_000);
        assert!(replace_cache_if_newer(&cache, g10, false));
        assert_eq!(snapshot(&cache), (10, FleetPosture::Nominal));

        // Lower generation 9 must be rejected, cache unchanged.
        let g9 = CachedFleetPosture::new_with_generation(FleetPosture::Degraded, 9, 2_000);
        assert!(
            !replace_cache_if_newer(&cache, g9, false),
            "lower generation must NOT replace the cache"
        );
        assert_eq!(
            snapshot(&cache),
            (10, FleetPosture::Nominal),
            "older recalc must NOT have clobbered the newer posture"
        );

        // Equal generation 10 must also be rejected (strictly greater).
        let g10_eq = CachedFleetPosture::new_with_generation(FleetPosture::LockedOut, 10, 3_000);
        assert!(
            !replace_cache_if_newer(&cache, g10_eq, false),
            "equal generation must NOT replace (strict > required)"
        );
        assert_eq!(snapshot(&cache), (10, FleetPosture::Nominal));

        // Strictly greater generation 11 wins.
        let g11 = CachedFleetPosture::new_with_generation(FleetPosture::Degraded, 11, 4_000);
        assert!(replace_cache_if_newer(&cache, g11, false));
        assert_eq!(snapshot(&cache), (11, FleetPosture::Degraded));
    }

    // FIX 1: an empty cache always accepts (current_gen treated as 0).
    #[test]
    fn test_replace_cache_if_newer_accepts_into_empty_cache() {
        use crate::posture_cache::CachedFleetPosture;
        use std::sync::Arc;

        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        let g1 = CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 1, 0);
        assert!(
            replace_cache_if_newer(&cache, g1, false),
            "generation > 0 must populate an empty cache"
        );
        let snap_gen = cache.read().unwrap().as_ref().unwrap().generation;
        assert_eq!(snap_gen, 1);
    }

    /// #688: while a sticky lockout is in effect, `replace_cache_if_newer` must
    /// REFUSE a non-LockedOut candidate even with a STRICTLY HIGHER generation —
    /// the race where an in-flight recalc that predates the supervisor trip (but
    /// grabbed a later generation) would otherwise win the CAS and clobber the
    /// forced LockedOut. A LockedOut candidate is still accepted under the normal
    /// generation CAS, and once the flag clears, recovery recalcs resume writing.
    #[test]
    fn test_sticky_lockout_refuses_higher_gen_downgrade_688() {
        use crate::posture_cache::CachedFleetPosture;
        use std::sync::Arc;

        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
        // force_lockout writes LockedOut at gen 10.
        let locked = CachedFleetPosture::new_with_generation(FleetPosture::LockedOut, 10, 1_000);
        assert!(replace_cache_if_newer(&cache, locked, true));

        // A racing recalc: HIGHER generation (11) but non-LockedOut. Without #688 it
        // wins the generation CAS and downgrades the lockout. With the guard it is
        // refused while the sticky flag holds.
        let stale_nominal =
            CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 11, 2_000);
        assert!(
            !replace_cache_if_newer(&cache, stale_nominal, true),
            "a higher-gen non-LockedOut recalc must NOT downgrade a sticky LockedOut"
        );
        {
            let g = cache.read().unwrap();
            let e = g.as_ref().unwrap();
            assert_eq!(e.posture, FleetPosture::LockedOut, "lockout preserved");
            assert_eq!(e.generation, 10);
        }

        // A LockedOut candidate with a higher generation is still accepted (a later
        // recalc that honored supervisor_tripped and re-emitted LockedOut).
        let locked_newer =
            CachedFleetPosture::new_with_generation(FleetPosture::LockedOut, 12, 3_000);
        assert!(
            replace_cache_if_newer(&cache, locked_newer, true),
            "a newer LockedOut is still accepted"
        );

        // Once the sticky flag clears (human/HA reset → sticky_lockout=false), a
        // higher-gen recovery recalc is admitted again.
        let recovery = CachedFleetPosture::new_with_generation(FleetPosture::Nominal, 13, 4_000);
        assert!(
            replace_cache_if_newer(&cache, recovery, false),
            "after the sticky flag clears, a normal recalc resumes writing"
        );
        assert_eq!(
            cache.read().unwrap().as_ref().unwrap().posture,
            FleetPosture::Nominal
        );
    }

    // ── #99 flood-condition → FleetPosture coupling ──────────────────────────
    // Driven through the real authoritative write path (`recalculate_and_broadcast`,
    // audit-commit-gated), reading the resulting cache posture. DAG postures are
    // forced by inserting nodes: Untrusted → LockedOut, Unknown → Degraded,
    // Trusted → Nominal (per `recursive_calculate`).
    //
    // These tests exercise the operational ESCALATION layer (flood / frame / RSS)
    // composing ON TOP OF a healthy fleet, so `active_app` seeds one Trusted
    // baseline node — i.e. a real, non-empty Nominal DAG. (Without it the M-9
    // empty-live-set guard would fail the whole fleet closed to LockedOut before
    // any escalation is considered — that guard has its own dedicated tests.)

    fn active_app() -> std::sync::Arc<AppState> {
        use crate::verifier::VerifierOperationMode;
        use crate::verifier_store::VerifierStore;
        let store = VerifierStore::new(":memory:").unwrap();
        let app = std::sync::Arc::new(AppState::new(store, VerifierOperationMode::Active));
        insert_node(&app, "baseline", NodeTrustState::Trusted);
        app
    }

    fn insert_node(app: &AppState, id: &str, status: NodeTrustState) {
        use crate::verifier::RegisteredNode;
        app.nodes.insert(
            id.to_string(),
            RegisteredNode {
                node_id: id.to_string(),
                status,
                registered_at_ms: 0,
                last_trust_update_ms: 0,
                ak_public_pem: None,
                expected_pcr16_digest_hex: None,
                site: None,
                firmware_version: None,
            },
        );
    }

    fn cache_posture(cache: &SharedPostureCache) -> Option<FleetPosture> {
        cache
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.posture))
    }

    /// #774 F4 — a PassiveStandby recalc AUDITS a transition (it lands in the
    /// tamper-evident chain) but must NOT tick `kirra_posture_transitions_total`:
    /// the standby never enforces or broadcasts, so counting it would be a phantom
    /// transition nobody observed. Pins that the metric moved inside the
    /// `cache_written && is_transition` (broadcast) gate.
    #[test]
    fn test_standby_transition_is_audited_but_not_counted() {
        use crate::verifier::VerifierOperationMode;
        use crate::verifier_store::VerifierStore;
        let store = VerifierStore::new(":memory:").unwrap();
        let app = std::sync::Arc::new(AppState::new(store, VerifierOperationMode::PassiveStandby));
        insert_node(&app, "baseline", NodeTrustState::Trusted);
        let cache = empty_cache();

        // None → (computed) is a transition on the first recalc; the standby audits
        // it but early-returns before the cache CAS / broadcast.
        recalculate_and_broadcast(&app, &cache);

        // NOT counted on any posture bucket.
        for p in [
            FleetPosture::Nominal,
            FleetPosture::Degraded,
            FleetPosture::LockedOut,
        ] {
            assert_eq!(
                app.fleet_metrics.transition_count(&p),
                0,
                "a PassiveStandby transition must not tick the enforced-transition metric ({p:?})"
            );
        }
        // The standby suppresses the cache write (it is not Active).
        assert!(
            cache.read().unwrap().is_none(),
            "a PassiveStandby must not populate the cache"
        );
        // But the transition WAS audited to the tamper-evident chain.
        let audited = app.store.with(|s| {
            s.load_all_posture_events()
                .unwrap()
                .iter()
                .any(|e| e["event_type"] == "SYSTEM_POSTURE_TRANSITION")
        });
        assert!(
            audited,
            "the standby must still audit the transition to the chain"
        );
    }

    fn empty_cache() -> SharedPostureCache {
        std::sync::Arc::new(std::sync::RwLock::new(None))
    }

    #[test]
    fn test_flood_active_nominal_escalates_to_degraded() {
        let app = active_app();
        let cache = empty_cache();
        app.flood_condition_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Degraded),
            "flood + DAG Nominal must escalate to Degraded"
        );
    }

    /// THE KEY SAFETY ASSERTION: flood never downgrades a DAG LockedOut.
    #[test]
    fn test_flood_active_locked_out_stays_locked_out() {
        let app = active_app();
        let cache = empty_cache();
        insert_node(&app, "n", NodeTrustState::Untrusted("test".to_string())); // DAG → LockedOut
        app.flood_condition_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::LockedOut),
            "flood must NEVER downgrade a DAG LockedOut"
        );
    }

    #[test]
    fn test_flood_active_degraded_stays_degraded() {
        let app = active_app();
        let cache = empty_cache();
        insert_node(&app, "n", NodeTrustState::Unknown); // DAG → Degraded
        app.flood_condition_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Degraded),
            "flood does not alter an already-Degraded DAG posture"
        );
    }

    // --- S-FI1d: frame-integrity posture coupling --------------------------

    // ---- S-DG1 governor-divergence composition (mirror the frame coupling) ----

    /// divergence_degraded_active + DAG Nominal → Degraded (same clause as
    /// RSS/flood/frame).
    #[test]
    fn test_divergence_degraded_active_escalates_nominal() {
        let app = active_app();
        let cache = empty_cache();
        app.divergence_degraded_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Degraded),
            "divergence_degraded_active + DAG Nominal must escalate to Degraded"
        );
    }

    /// divergence_lockout_active shares the absolute LockedOut priority with
    /// supervisor_tripped / frame_lockout_active and rides the same sticky
    /// downgrade guard.
    #[test]
    fn test_divergence_lockout_active_forces_locked_out() {
        let app = active_app();
        let cache = empty_cache();
        app.divergence_lockout_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::LockedOut),
            "divergence_lockout_active must force LockedOut over a healthy DAG"
        );
        // Sticky: a subsequent healthy recalc must NOT downgrade it.
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(cache_posture(&cache), Some(FleetPosture::LockedOut));
    }

    /// Default-false flags are inert — byte-identical prior behavior until the
    /// comparator sink is wired (the S-DG1 asymmetry requirement).
    #[test]
    fn test_divergence_flags_default_inert() {
        let app = active_app();
        let cache = empty_cache();
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Nominal),
            "unwired divergence flags must not perturb a healthy fleet"
        );
    }

    #[test]
    fn test_frame_degraded_active_escalates_nominal() {
        let app = active_app();
        let cache = empty_cache();
        app.frame_degraded_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Degraded),
            "frame_degraded_active + DAG Nominal must escalate to Degraded"
        );
    }

    /// frame_lockout_active shares the absolute LockedOut priority with the
    /// supervisor trip: it forces LockedOut over an otherwise-healthy DAG.
    #[test]
    fn test_frame_degraded_active_locked_out_stays_locked_out() {
        let app = active_app();
        let cache = empty_cache();
        app.frame_lockout_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::LockedOut),
            "frame_lockout_active must force LockedOut over a healthy DAG"
        );
    }

    /// frame and RSS compose: either active (with Nominal DAG) → Degraded.
    #[test]
    fn test_frame_and_rss_compose() {
        let app = active_app();
        let cache = empty_cache();
        app.frame_degraded_active.store(true, Ordering::SeqCst);
        app.rss_active_violation.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Degraded),
            "frame OR rss escalates Nominal → Degraded"
        );
    }

    /// Clearing the frame-degraded flag auto-recovers to Nominal via the same path.
    #[test]
    fn test_frame_degraded_clears_auto_recovers_to_nominal() {
        let app = active_app();
        let cache = empty_cache();
        app.frame_degraded_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(cache_posture(&cache), Some(FleetPosture::Degraded));

        app.frame_degraded_active.store(false, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Nominal),
            "clearing frame_degraded_active returns posture to Nominal (auto-recovery)"
        );
    }

    /// flood and RSS compose: either active (with Nominal DAG) → Degraded.
    #[test]
    fn test_flood_and_rss_compose() {
        let app = active_app();
        let cache = empty_cache();
        app.flood_condition_active.store(true, Ordering::SeqCst);
        app.rss_active_violation.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Degraded),
            "flood OR rss escalates Nominal → Degraded"
        );
    }

    /// Clearing the flag auto-recovers to Nominal via the existing path — no new
    /// recovery logic.
    #[test]
    fn test_flood_clears_auto_recovers_to_nominal() {
        let app = active_app();
        let cache = empty_cache();
        app.flood_condition_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(cache_posture(&cache), Some(FleetPosture::Degraded));

        app.flood_condition_active.store(false, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Nominal),
            "clearing the flood flag returns posture to Nominal (auto-recovery)"
        );
    }

    /// Default-false flag is inert (no setter exists in this PR).
    #[test]
    fn test_flood_default_false_is_inert() {
        let app = active_app();
        let cache = empty_cache();
        assert!(
            !app.flood_condition_active.load(Ordering::SeqCst),
            "the flag defaults false"
        );
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            cache_posture(&cache),
            Some(FleetPosture::Nominal),
            "no flood (default) → no escalation"
        );
    }

    /// The flood escalation flows through the EXISTING audit-commit-gated path:
    /// the cache being written to Degraded proves the audit committed, and the
    /// existing posture-transition event is emitted (no new audit plumbing).
    #[test]
    fn test_flood_transition_flows_through_audit_gated_path() {
        let app = active_app();
        let cache = empty_cache();
        app.flood_condition_active.store(true, Ordering::SeqCst);
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(cache_posture(&cache), Some(FleetPosture::Degraded));

        let events = app
            .store
            .with(|store| store.load_all_posture_events().expect("load events"));
        assert!(
            events
                .iter()
                .any(|e| e["event_type"] == "SYSTEM_POSTURE_TRANSITION"),
            "the flood escalation must emit the existing posture-transition audit event"
        );
    }

    #[test]
    fn test_init_generation_never_moves_counter_backwards() {
        use crate::verifier::{AppState, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use std::sync::Arc;

        // B6 regression: simulate a recalc having already advanced the live counter
        // well past any persisted value. (`fetch_max` here so this test is robust to
        // the shared global being concurrently bumped by other tests in the binary.)
        let high = POSTURE_GENERATION.load(Ordering::SeqCst) + 1_000;
        POSTURE_GENERATION.fetch_max(high, Ordering::SeqCst);
        let before = POSTURE_GENERATION.load(Ordering::SeqCst);

        // Persist a LOWER last-generation, then init from it.
        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        app.store.with(|s| s.save_last_generation(5).unwrap());

        let loaded = init_generation_from_store(&app).expect("readable store");
        assert_eq!(loaded, 5, "init reports the persisted high-water it loaded");

        // With the old `store(last + 1)` this would have dropped the counter to 6;
        // `fetch_max(6)` cannot lower a counter already at/above `before`.
        assert!(
            POSTURE_GENERATION.load(Ordering::SeqCst) >= before,
            "init_generation_from_store must never move the generation counter backwards"
        );
    }

    /// WS-0.2 / #G10 — the RAISE case: when the persisted high-water is AHEAD
    /// of the live counter (the restart scenario: a prior process advanced the
    /// store, this process booted fresh), init must lift the counter above it
    /// so every generation emitted after a restart is strictly greater than
    /// every generation emitted before it (the ordering federation peers and
    /// SSE consumers rely on).
    #[test]
    fn test_init_generation_raises_counter_above_persisted_high_water() {
        use crate::verifier::{AppState, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use std::sync::Arc;

        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));

        // Simulate a prior process having persisted a high-water far ahead of
        // this process's live counter (offset keeps the test robust to the
        // shared global being bumped concurrently by other tests).
        let high_water = POSTURE_GENERATION.load(Ordering::SeqCst) + 10_000;
        app.store
            .with(|s| s.save_last_generation(high_water).unwrap());

        let loaded = init_generation_from_store(&app).expect("readable store");
        assert_eq!(loaded, high_water);

        let next = next_generation();
        assert!(
            next > high_water,
            "the first generation after init must exceed the persisted high-water \
             (got {next}, high-water {high_water}) — otherwise restarts time-reverse"
        );
    }

    /// WS-0.5 — a COMMITTED posture transition increments the /metrics
    /// transition counter for its TARGET posture; the periodic no-change
    /// refresh does not count (it is not a transition).
    #[test]
    fn test_transition_counts_for_metrics_but_refresh_does_not() {
        let app = active_app();
        let cache = empty_cache();

        // First recalc: None → Nominal is a transition.
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            app.fleet_metrics.transition_count(&FleetPosture::Nominal),
            1,
            "the None→Nominal transition must be counted"
        );

        // No-change refresh: nothing moves.
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            app.fleet_metrics.transition_count(&FleetPosture::Nominal),
            1,
            "a no-change refresh is not a transition and must not count"
        );

        // Trust the DAG down: the new posture's counter increments once.
        insert_node(&app, "faulty", NodeTrustState::Untrusted("test".into()));
        recalculate_and_broadcast(&app, &cache);
        let new_posture = cache.read().unwrap().expect("cache populated").posture;
        assert_ne!(
            new_posture,
            FleetPosture::Nominal,
            "precondition: the fault degrades the fleet"
        );
        assert_eq!(
            app.fleet_metrics.transition_count(&new_posture),
            1,
            "the degradation transition must be counted under its target posture"
        );
        assert_eq!(
            app.fleet_metrics.transition_count(&FleetPosture::Nominal),
            1,
            "the earlier Nominal count is untouched"
        );
    }

    /// WS-0.3 / #772 F2+F6 — the incident-class DURABLE write is gated on
    /// TRANSITIONS: a posture change is written on the FULL (fsync-per-commit)
    /// connection via `save_posture_event_chained_with_generation_durable`, while
    /// the periodic POSTURE_CACHE_REFRESHED traffic stays on the NORMAL connection
    /// (INV-12 throughput rationale — no 20 Hz per-row fsync re-introduced through
    /// the back door). On this in-memory store `durable_conn` falls back to `conn`,
    /// so the gate is observed via the test-only durable-write COUNTER rather than
    /// any OS-level fsync (which a `:memory:` store cannot exercise).
    #[test]
    fn test_incident_durable_write_fires_on_transition_not_on_refresh() {
        let app = active_app();
        let cache = empty_cache();

        // First recalc: None → Nominal is a transition → at least one durable
        // write (the posture-engine transition itself; a LockedOut transition
        // would add post-incident durable rows too — hence `>=`, not `==`).
        recalculate_and_broadcast(&app, &cache);
        let after_transition = app.store.with(|s| s.durable_posture_write_count());
        assert!(
            after_transition >= 1,
            "a posture transition must take the FULL-connection durable write path"
        );

        // Recalc with NO posture change: the refresh path must add ZERO durable
        // writes — the load-bearing INV-12 gate (no 20 Hz per-row fsync).
        recalculate_and_broadcast(&app, &cache);
        assert_eq!(
            app.store.with(|s| s.durable_posture_write_count()),
            after_transition,
            "a no-change refresh must NOT durable-write (INV-12: transitions only)"
        );

        // Force a real transition (trust the DAG down): the counter advances again.
        insert_node(&app, "faulty", NodeTrustState::Untrusted("test".into()));
        recalculate_and_broadcast(&app, &cache);
        assert!(
            app.store.with(|s| s.durable_posture_write_count()) > after_transition,
            "a real transition must take the durable write path again"
        );
    }

    /// #772 F3 — a FAILED durable (FULL-connection) write of a posture transition
    /// must NOT suppress the (possibly escalating) transition. The recalc counts
    /// the degradation in `incident_durability_failures` and FALLS BACK to the
    /// NORMAL write, so the row still lands and the cache is still populated (the
    /// transition is broadcast). Exercised via the store's durable-write fault seam.
    #[test]
    fn test_durable_write_failure_counts_and_falls_back_not_suppressed() {
        let app = active_app();
        let cache = empty_cache();
        app.store.with(|s| s.set_fail_durable_posture_writes(true));

        // None → Nominal is a transition; its durable write is forced to fail.
        recalculate_and_broadcast(&app, &cache);

        assert_eq!(
            app.incident_durability_failures
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "a failed durable transition write must be counted (#772 F3)"
        );
        assert!(
            cache.read().unwrap().is_some(),
            "the transition must NOT be suppressed — it falls back to the NORMAL write and \
             populates the cache"
        );
        let has_row = app.store.with(|s| {
            s.load_all_posture_events()
                .unwrap()
                .iter()
                .any(|e| e["event_type"] == "SYSTEM_POSTURE_TRANSITION")
        });
        assert!(
            has_row,
            "the fallback NORMAL write must have committed the transition row"
        );
    }

    #[test]
    fn test_recalc_over_shared_dependency_dag_completes() {
        use crate::verifier::{AppState, NodeTrustState, RegisteredNode, VerifierOperationMode};
        use crate::verifier_store::VerifierStore;
        use std::sync::Arc;

        let store = VerifierStore::new(":memory:").unwrap();
        let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
        let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

        // Diamond DAG: d -> {b, c} -> a. `a` is a SHARED dependency of b and c —
        // the case the B1 snapshot must traverse without holding a `nodes` shard
        // guard across the re-entrant `calculate_posture` gets. All trusted.
        for id in ["a", "b", "c", "d"] {
            app.persist_and_insert_node(RegisteredNode {
                node_id: id.to_string(),
                status: NodeTrustState::Trusted,
                registered_at_ms: 1,
                last_trust_update_ms: 1,
                ak_public_pem: None,
                expected_pcr16_digest_hex: None,
                site: None,
                firmware_version: None,
            })
            .unwrap();
        }
        app.persist_and_insert_deps("b", vec!["a".to_string()])
            .unwrap();
        app.persist_and_insert_deps("c", vec!["a".to_string()])
            .unwrap();
        app.persist_and_insert_deps("d", vec!["b".to_string(), "c".to_string()])
            .unwrap();

        // The recalc must COMPLETE (the snapshot path holds no shard guard across
        // the re-entrant gets) and, all-trusted, derive Nominal.
        recalculate_and_broadcast(&app, &cache);
        let guard = cache.read().unwrap();
        assert_eq!(guard.as_ref().unwrap().posture, FleetPosture::Nominal);
    }
}
