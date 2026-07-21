//! Clearance delivery — node-side Phase B (#304, the #267 deferral).
//!
//! Delivers operator clearance grants recorded by the verifier (operator-console
//! Phase A) to this node's [`ClearanceLoop`].
//!
//! **Transport (the decision):** store-level pickup over the shared per-vehicle
//! [`VerifierStore`] — the SAME channel the #247/#263 audit sinks already use;
//! **zero new dependencies**.
//!
//! **Co-location assumption (stated):** the node and the vehicle's verifier share
//! the store (one vehicle, one store); the fleet/federation layer is *above* this.
//! A remote-node wire transport (HTTP / Zenoh) is the future variant — OUT OF SCOPE.
//!
//! **Two-checkpoint validation (the held line):** the verifier validates a grant at
//! RECORD time (Phase A: non-empty operator, registered node). This component
//! re-validates at DELIVERY time via [`ClearanceLoop::try_clear`] with
//! `now = pickup time` — so a grant that sat past `max_grant_age_ms` is REJECTED at
//! the loop even though the verifier accepted it. **Verifier validation is not a
//! substitute for loop validation.**
//!
//! **No retry (the held line):** a rejected-or-stale grant is CONSUMED (one-shot)
//! and never retried — the operator re-issues through the console. Automatic retry
//! of a dead grant would be a replay machine.

use kirra_persistence::VerifierStore;
use kirra_verifier::store_handle::StoreHandle;
use parko_core::{ClearanceLoop, OperatorClearanceGrant, DEFAULT_MAX_GRANT_AGE_MS};

/// The result of one [`ClearanceDelivery::poll_and_deliver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// No pending grant for this node — pickup is idempotent-empty; the loop is
    /// untouched.
    NoGrant,
    /// A grant was delivered and CLEARED the loop (→ `Normal`).
    Cleared {
        operator_id: String,
        grant_rowid: i64,
    },
    /// A grant was picked up but the loop REJECTED it (stale / malformed /
    /// not-immobilized). It is CONSUMED (never retried); the loop stays as it was.
    Rejected {
        reason: &'static str,
        grant_rowid: i64,
    },
    /// The store could not be consulted — fail-closed: nothing is cleared.
    StoreError,
}

/// Node-side clearance delivery. Holds the shared store handle + this node's id —
/// the same shape as the `parko-kirra` audit sinks (the #247 crossing).
pub struct ClearanceDelivery {
    store: StoreHandle,
    node_id: String,
    max_grant_age_ms: u64,
}

impl ClearanceDelivery {
    pub fn new(store: StoreHandle, node_id: impl Into<String>) -> Self {
        Self {
            store,
            node_id: node_id.into(),
            max_grant_age_ms: DEFAULT_MAX_GRANT_AGE_MS,
        }
    }

    /// Open the co-located per-vehicle store at `db_path`, install the base64
    /// Ed25519 signing key (the same `KIRRA_LOG_SIGNING_KEY` encoding the verifier
    /// service accepts) so delivered-grant outcomes are SIGNED in the shared
    /// ledger, and build a delivery handle for `node_id`.
    ///
    /// This is the node-side deploy constructor (the #304 deferral): the parko-ros2
    /// node calls it at startup so it never has to name `base64` / `ed25519_dalek`
    /// itself (those deps live here). Fail-closed: an unopenable store or an
    /// undecodable key is an `Err(String)` — never a silent unsigned fallback.
    ///
    /// The signing key is moved into the store and never retained or logged here.
    pub fn open_signed(
        db_path: &str,
        node_id: &str,
        signing_key_b64: &str,
    ) -> Result<Self, String> {
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(signing_key_b64.trim())
            .map_err(|e| format!("KIRRA_LOG_SIGNING_KEY is not valid base64: {e}"))?;
        let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| {
            format!(
                "KIRRA_LOG_SIGNING_KEY must decode to 32 key bytes, got {}",
                raw.len()
            )
        })?;
        let key = ed25519_dalek::SigningKey::from_bytes(&bytes);
        let mut store = VerifierStore::new(db_path)
            .map_err(|e| format!("could not open the co-located store '{db_path}': {e}"))?;
        store.set_signing_key(key);
        Ok(Self::new(StoreHandle::new(store), node_id))
    }

    /// Override the grant-age ceiling (default [`DEFAULT_MAX_GRANT_AGE_MS`]).
    #[must_use]
    pub fn with_max_grant_age_ms(mut self, ms: u64) -> Self {
        self.max_grant_age_ms = ms;
        self
    }

    /// Take at most one pending grant and deliver it to `clearance_loop`.
    ///
    /// CALL SITE: per tick while [`ClearanceLoop::escalation_pending`], or on a
    /// slow cadence — both are correct because pickup is idempotent-empty when no
    /// grant exists (an extra call is a `NoGrant` no-op). The grant is CONSUMED at
    /// pickup, BEFORE the loop verdict, so a rejected grant is never retried.
    /// (parko-ros2 node wiring is the deploy step, consistent with prior deferrals.)
    pub fn poll_and_deliver(
        &self,
        clearance_loop: &mut ClearanceLoop,
        now_ms: u64,
    ) -> DeliveryOutcome {
        // 1. ONE-SHOT CONSUME — the grant is now spent regardless of the verdict.
        //    The closure returns the take result; the outer match maps it to the
        //    early-return outcomes (Rule 4 — `return` cannot cross the closure).
        let row = match self
            .store
            .with(|store| store.take_pending_clearance_grant(&self.node_id, now_ms))
        {
            Ok(Some(r)) => r,
            Ok(None) => return DeliveryOutcome::NoGrant,
            Err(_) => return DeliveryOutcome::StoreError,
        };

        // 2. CHECKPOINT 2 — the loop re-validates at DELIVERY time. `granted_at_ms`
        //    is the verifier's RECORD time (from the row), `now_ms` is pickup time:
        //    a grant older than `max_grant_age_ms` is rejected HERE even though the
        //    verifier accepted it at record time.
        let grant = OperatorClearanceGrant {
            operator_id: row.operator_id.clone(),
            granted_at_ms: row.granted_at_ms,
        };
        let verdict = clearance_loop.try_clear(&grant, now_ms, self.max_grant_age_ms);

        // 3. Record the outcome (signed). Consumed either way — NO retry.
        //    The closure builds and returns the outcome (poison is recovered by
        //    the handle, so the former StoreError-on-poison arm is gone).
        self.store.with(|store| match verdict {
            Ok(()) => {
                let _ = store.record_grant_outcome(row.rowid, "Cleared", None, now_ms);
                DeliveryOutcome::Cleared {
                    operator_id: row.operator_id,
                    grant_rowid: row.rowid,
                }
            }
            Err(rej) => {
                let reason = rej.reason_code();
                let _ = store.record_grant_outcome(row.rowid, reason, Some(reason), now_ms);
                DeliveryOutcome::Rejected {
                    reason,
                    grant_rowid: row.rowid,
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parko_core::{ClearanceState, ImpactCfg, ImpactEvidence};

    fn store() -> StoreHandle {
        StoreHandle::new(VerifierStore::new(":memory:").expect("in-memory store"))
    }

    fn record_grant(s: &StoreHandle, node: &str, op: &str, granted_at_ms: u64) {
        s.with(|store| store.save_clearance_grant_chained(node, op, granted_at_ms))
            .expect("record grant (Phase-A path)");
    }

    fn immobilized_loop() -> ClearanceLoop {
        let mut l = ClearanceLoop::new();
        let ev = ImpactEvidence {
            imu_accel_spike_mps2: 0.0,
            contact_sensor: true,
            vanished_object: false,
        };
        let cfg = ImpactCfg::default();
        l.observe(&ev, &cfg, 0); // Normal -> Latched
        l.observe(&ev, &cfg, 0); // Latched -> EscalationRaised (immobilized)
        assert!(l.is_immobilized() && l.escalation_pending());
        l
    }

    fn audit_has(s: &StoreHandle, event_type: &str) -> bool {
        let page = s
            .with(|store| store.load_audit_chain_page(200, 0, None))
            .unwrap();
        page.entries.iter().any(|e| {
            serde_json::to_value(e)
                .unwrap()
                .get("event_type")
                .and_then(|x| x.as_str())
                == Some(event_type)
        })
    }

    #[test]
    fn record_then_deliver_clears_loop() {
        let s = store();
        record_grant(&s, "robot-01", "alice", 1_000);
        let mut l = immobilized_loop();
        let d = ClearanceDelivery::new(s.clone(), "robot-01");

        let out = d.poll_and_deliver(&mut l, 1_500); // within the age window
        assert!(
            matches!(out, DeliveryOutcome::Cleared { .. }),
            "got {out:?}"
        );
        assert_eq!(l.state(), ClearanceState::Normal, "loop cleared to Normal");
        assert!(
            audit_has(&s, "ClearanceDelivered"),
            "ClearanceDelivered audit present"
        );

        let st = s
            .with(|store| store.latest_clearance_grant("robot-01"))
            .unwrap()
            .unwrap();
        assert_eq!(st.outcome.as_deref(), Some("Cleared"));
        assert!(st.consumed_at_ms.is_some(), "grant consumed");
    }

    #[test]
    fn stale_at_delivery_rejected_consumed_then_fresh_grant_clears() {
        let s = store();
        record_grant(&s, "robot-01", "alice", 1_000);
        let mut l = immobilized_loop();
        let d = ClearanceDelivery::new(s.clone(), "robot-01");

        // pickup far past DEFAULT_MAX_GRANT_AGE_MS → loop rejects (checkpoint 2).
        let stale_now = 1_000 + DEFAULT_MAX_GRANT_AGE_MS + 1;
        let out = d.poll_and_deliver(&mut l, stale_now);
        assert!(
            matches!(
                out,
                DeliveryOutcome::Rejected {
                    reason: "malformed_grant",
                    ..
                }
            ),
            "a grant the verifier accepted is still rejected at delivery if stale; got {out:?}"
        );
        assert!(
            l.is_immobilized(),
            "loop still immobilized after a stale grant"
        );
        assert!(audit_has(&s, "ClearanceDeliveryRejected"));

        // the stale grant is CONSUMED — a second pickup finds nothing (no retry).
        assert_eq!(
            d.poll_and_deliver(&mut l, stale_now + 1),
            DeliveryOutcome::NoGrant
        );

        // the operator RE-ISSUES a fresh grant through the console → clears.
        record_grant(&s, "robot-01", "alice", stale_now + 2);
        let out2 = d.poll_and_deliver(&mut l, stale_now + 3);
        assert!(
            matches!(out2, DeliveryOutcome::Cleared { .. }),
            "fresh grant clears; got {out2:?}"
        );
        assert_eq!(l.state(), ClearanceState::Normal);
    }

    #[test]
    fn one_shot_second_take_gets_none() {
        let s = store();
        record_grant(&s, "robot-01", "alice", 1_000);
        let first = s
            .with(|store| store.take_pending_clearance_grant("robot-01", 1_100))
            .unwrap();
        let second = s
            .with(|store| store.take_pending_clearance_grant("robot-01", 1_101))
            .unwrap();
        assert!(first.is_some(), "first take gets the grant");
        assert!(
            second.is_none(),
            "second take gets None — exactly-once consume"
        );
    }

    #[test]
    fn empty_pickup_is_noop() {
        let s = store();
        let mut l = immobilized_loop();
        let before = l.state();
        let d = ClearanceDelivery::new(s.clone(), "robot-01");

        assert_eq!(d.poll_and_deliver(&mut l, 1_000), DeliveryOutcome::NoGrant);
        assert_eq!(l.state(), before, "loop untouched on empty pickup");
        assert!(!audit_has(&s, "ClearanceDelivered"));
        assert!(!audit_has(&s, "ClearanceDeliveryRejected"));
    }

    #[test]
    fn open_signed_rejects_a_bad_key_and_signs_with_a_good_one() {
        use base64::Engine as _;
        let dir = std::env::temp_dir();
        let db = dir.join(format!("parko_open_signed_{}.sqlite", std::process::id()));
        let db_path = db.to_str().unwrap();
        let _ = std::fs::remove_file(&db);

        // A non-base64 / wrong-length key is a fail-closed Err — never a silent
        // unsigned fallback.
        assert!(ClearanceDelivery::open_signed(db_path, "KIRRA-DEMO-03", "not-base64!!").is_err());

        // A valid 32-byte base64 seed opens a SIGNED store: a delivered grant's
        // outcome lands as a verifiable row.
        let key_b64 = base64::engine::general_purpose::STANDARD.encode([3u8; 32]);
        let d = ClearanceDelivery::open_signed(db_path, "KIRRA-DEMO-03", &key_b64)
            .expect("valid key opens a signed store");

        // Record a grant directly through the same store handle, then deliver.
        d.store
            .with(|store| store.save_clearance_grant_chained("KIRRA-DEMO-03", "alice", 1_000))
            .expect("record grant");
        let mut l = immobilized_loop();
        let out = d.poll_and_deliver(&mut l, 1_500);
        assert!(
            matches!(out, DeliveryOutcome::Cleared { .. }),
            "got {out:?}"
        );
        assert!(audit_has(&d.store, "ClearanceDelivered"));

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(format!("{db_path}-wal"));
        let _ = std::fs::remove_file(format!("{db_path}-shm"));
    }

    #[test]
    fn malformed_row_defense_empty_operator_rejected_consumed() {
        // Defense-in-depth: Phase A validates operator_id, but if a row somehow
        // carried an empty operator_id, the loop rejects it (malformed), the grant
        // is consumed, and it is audited. (We insert the degenerate row directly.)
        let s = store();
        s.with(|store| store.save_clearance_grant_chained("robot-01", "", 1_000))
            .expect("record");
        let mut l = immobilized_loop();
        let d = ClearanceDelivery::new(s.clone(), "robot-01");

        let out = d.poll_and_deliver(&mut l, 1_100);
        assert!(
            matches!(
                out,
                DeliveryOutcome::Rejected {
                    reason: "malformed_grant",
                    ..
                }
            ),
            "got {out:?}"
        );
        assert!(l.is_immobilized());
        assert!(audit_has(&s, "ClearanceDeliveryRejected"));
        assert_eq!(
            d.poll_and_deliver(&mut l, 1_101),
            DeliveryOutcome::NoGrant,
            "consumed, not retried"
        );
    }
}
