//! deliver_clearance — the money demo for operator-console Phase B.
//!
//! `cargo run -p parko-kirra --example deliver_clearance -- --db <path> --node KIRRA-DEMO-03`
//!
//! Opens the demo store, constructs a `ClearanceLoop` driven into the same
//! immobilized state the seeder's escalation describes (through the REAL state
//! machine — `observe()` with synthetic `ImpactEvidence`, never by poking
//! internals), and runs exactly ONE [`ClearanceDelivery::poll_and_deliver`].
//!
//! Demo flow: the browser shows the operator's grant as `RECORDED · PENDING` →
//! run this in a terminal → refresh → `DELIVERED · CLEARED`. The Phase-B loop,
//! live on a desk.
//!
//! HONESTY: this example STANDS IN for the parko-ros2 node tick (the named deploy
//! step). `poll_and_deliver` below is the EXACT call the node will make — the
//! `ClearanceLoop` is the real one (production wraps it in `RecordedClearanceLoop`,
//! which records the same transitions); only the `ImpactEvidence` that pre-latches
//! it here is synthetic.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
use ed25519_dalek::SigningKey;

use kirra_persistence::VerifierStore;
use kirra_verifier::store_handle::StoreHandle;
use parko_core::{ClearanceLoop, ImpactCfg, ImpactEvidence};
use parko_kirra::clearance_delivery::{ClearanceDelivery, DeliveryOutcome};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn arg(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let db = arg("--db")
        .or_else(|| std::env::var("KIRRA_DB_PATH").ok())
        .unwrap_or_else(|| "kirra_demo.sqlite".to_string());
    let node = arg("--node").unwrap_or_else(|| "KIRRA-DEMO-03".to_string());

    println!("DEMO DELIVERY — parko-kirra clearance delivery, live.");
    println!(
        "  This example STANDS IN for the parko-ros2 node tick (the named deploy step):\n  \
         poll_and_deliver below is the EXACT call the node will make."
    );
    println!("  db={db}  node={node}");

    let mut store = match VerifierStore::new(&db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not open store '{db}': {e}");
            std::process::exit(2);
        }
    };
    // Sign the delivery-outcome event under the demo key (same key the service
    // verifies with) so the console shows a verified ClearanceDelivered row.
    if let Ok(b64) = std::env::var("KIRRA_LOG_SIGNING_KEY") {
        if let Ok(bytes) = b64e.decode(b64.trim()) {
            if let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice()) {
                store.set_signing_key(SigningKey::from_bytes(&seed));
            }
        }
    }
    let store = StoreHandle::new(store);

    // Pre-latch a real ClearanceLoop into EscalationRaised, mirroring the seeded
    // escalation (vanished-object trigger), via the real state machine.
    let mut clearance_loop = ClearanceLoop::new();
    let ev = ImpactEvidence {
        imu_accel_spike_mps2: 0.0,
        contact_sensor: false,
        vanished_object: true,
    };
    let cfg = ImpactCfg::default();
    clearance_loop.observe(&ev, &cfg, now_ms()); // Normal -> Latched
    clearance_loop.observe(&ev, &cfg, now_ms()); // Latched -> EscalationRaised
    println!(
        "  loop state: {:?} (immobilized via the real state machine)",
        clearance_loop.state()
    );

    let delivery = ClearanceDelivery::new(store, &node);
    match delivery.poll_and_deliver(&mut clearance_loop, now_ms()) {
        DeliveryOutcome::Cleared {
            operator_id,
            grant_rowid,
        } => {
            println!(
                "  verdict: DELIVERED · CLEARED  (operator={operator_id}, grant #{grant_rowid})"
            );
            println!(
                "  loop state now: {:?} — refresh /console to see the grant card flip.",
                clearance_loop.state()
            );
        }
        DeliveryOutcome::Rejected {
            reason,
            grant_rowid,
        } => {
            println!("  verdict: DELIVERY REJECTED · {reason}  (grant #{grant_rowid})");
            println!(
                "  the grant is consumed (never retried) — re-issue a FRESH grant in the console."
            );
        }
        DeliveryOutcome::NoGrant => {
            println!("  verdict: NO GRANT — nothing pending for {node}.");
            println!(
                "  record a grant in the console first (the supervisor-key form), then re-run."
            );
        }
        DeliveryOutcome::StoreError => {
            eprintln!("  verdict: STORE ERROR — could not consult '{db}'.");
            std::process::exit(2);
        }
    }
}
