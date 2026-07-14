// src/verifier_store/audit_appender.rs
//
// ADR-0035 Addendum A, Stage 2.5 step 1 (PROTOTYPE) — the injected audit-append seam.
//
// The hard-tier `verifier_store` families couple to the safety-authority layer by
// appending a signed, hash-chained `AuditChainLinker` event *inside the same
// transaction* as their table write (coupling "C1"). That atomicity is load-bearing
// (the power-loss drill proves the chain never forks), so the append cannot simply
// move up to the authority layer. This trait inverts the dependency WITHOUT losing
// atomicity: the store still owns the transaction and calls `append_within` on an
// injected appender, so a future `kirra-persistence` crate depends only on THIS
// contract — not on `crate::audit_chain` or the Ed25519 signing key.
//
// This is the proof-of-mechanics on the `posture` family (the smallest C1-only
// family). Behaviour is byte-identical: the production `ChainedAuditAppender`
// delegates to the exact same `AuditChainLinker::append_audit_event_tx` with the
// exact same key, so the audit-chain bytes are unchanged (a chained-write test
// re-verifies the chain, and an atomicity test proves a failing appender rolls the
// whole caller-owned transaction back).

/// Appends an audit event INTO a caller-owned transaction. The implementation
/// supplies whatever signing material the chain needs (the production impl below
/// borrows the store's key; the trait imposes no ownership); the persistence layer
/// depends only on this trait, so the append happens atomically with the caller's
/// table write while the store stays ignorant of `audit_chain` internals and the key.
///
/// `append_within` MUST NOT commit or roll back `tx` — it only stages its rows so
/// the CALLER's `tx.commit()` makes the table write and the audit append all-or-
/// nothing (and any error here propagates to abort the caller's transaction).
pub trait AuditAppender {
    fn append_within(
        &self,
        tx: &rusqlite::Transaction<'_>,
        event_type: &str,
        payload: &str,
        created_at_ms: i64,
    ) -> rusqlite::Result<()>;
}

/// The production appender: the hash-chained, Ed25519-signed audit append, delegating
/// to [`crate::audit_chain::AuditChainLinker::append_audit_event_tx`]. Holds a borrow
/// of the store's signing key. This is the ONE type that knows both the chain logic
/// and the key; at the eventual crate split it lives in the safety-authority layer and
/// is injected into persistence, so persistence itself carries neither dependency.
pub struct ChainedAuditAppender<'k> {
    pub signing_key: Option<&'k ed25519_dalek::SigningKey>,
}

impl AuditAppender for ChainedAuditAppender<'_> {
    fn append_within(
        &self,
        tx: &rusqlite::Transaction<'_>,
        event_type: &str,
        payload: &str,
        created_at_ms: i64,
    ) -> rusqlite::Result<()> {
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            tx,
            event_type,
            payload,
            created_at_ms,
            self.signing_key,
        )
    }
}
