// store_offload_guard — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// P1 CI GUARD — the per-command audit-chain write must never run sync on a worker
// ---------------------------------------------------------------------------

//! Locks in the review-P1 offload: `save_posture_event_chained` is the hottest
//! durable (fsync) write in the service. Running it synchronously on a tokio
//! worker via `store.with(..)` head-of-line-blocks every other request handler
//! and the fast loop behind one fsync. Every site must instead go through
//! `StoreHandle::call(..)` (spawn_blocking). This guard scans the binary source
//! and fails if any production `save_posture_event_chained` is reached through a
//! synchronous `.store.with(` rather than `.store.call(` — so a future edit
//! cannot silently reintroduce a worker-pinning audit write.

#[test]
fn audit_chain_write_is_never_on_a_sync_worker() {
    // Embeds a compile-time snapshot of this very file (path is relative to it).
    let sources: [&str; 11] = [
        include_str!("../kirra_verifier_service.rs"),
        include_str!("attestation.rs"),
        include_str!("fleet.rs"),
        include_str!("audit.rs"),
        include_str!("action_filter.rs"),
        include_str!("industrial.rs"),
        include_str!("federation.rs"),
        include_str!("actuator.rs"),
        include_str!("fabric.rs"),
        include_str!("console.rs"),
        include_str!("operators.rs"),
    ];
    let mut violations: Vec<usize> = Vec::new();

    for src in sources {
        let mut nearest_access = ""; // "with" (sync) | "call" (off-worker)
        let mut in_test = false; // tests live only in the root file; submodules are all production
        for (idx, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!")
            {
                continue;
            }
            if trimmed.starts_with("#[cfg(test)]") {
                in_test = true;
            }
            // Track the nearest ENCLOSING store access (last one seen wins; the
            // production sites place the access immediately above the write).
            if line.contains(".store.call(")
                || line.contains(".store.call_read(")
                || trimmed.starts_with(".call(")
                || trimmed.starts_with(".call_read(")
            {
                nearest_access = "call";
            } else if line.contains(".store.with(")
                || line.contains(".store.with_read(")
                || trimmed.starts_with(".with(")
                || trimmed.starts_with(".with_read(")
            {
                nearest_access = "with";
            }
            if !in_test && line.contains("save_posture_event_chained") && nearest_access == "with" {
                violations.push(idx + 1);
            }
        }
    }

    assert!(
        violations.is_empty(),
        "P1 VIOLATION: `save_posture_event_chained` (the per-command audit-chain fsync) \
         reached via a SYNCHRONOUS `store.with(` at line(s) {violations:?} — a durable write \
         on a tokio worker head-of-line-blocks the whole service. Offload it via \
         `svc.app.store.call(move |store| ...).await` instead.",
    );
}
