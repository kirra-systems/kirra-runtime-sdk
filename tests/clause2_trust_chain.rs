//! End-to-end Clause 2 trust chain (HVCHAN-001 §3, steps 1-7) composed across the
//! two crates that implement it:
//!   - `kirra-contract-channel` — the frozen layout, the odd/even seqlock, and
//!     validation (steps 1-4); plus the carrier-agnostic `InProcessRegion`
//!     reference (the Ideal-B seam made concrete for host tests).
//!   - `kirra_verifier::governor_release` — the digest, the Ed25519 release token,
//!     and the actuator verify-before-release gate (steps 5-7).
//!
//! Neither crate's own tests exercise the FULL chain; this proves the seam between
//! them and demonstrates the closed claim: "the governor approved exactly the
//! data represented by this digest, and actuator release is bound to that
//! approval." The carrier here is in-process; on the target the SAME chain runs
//! with the reader/writer traits bound to mapped hypervisor SHM (Ideal-B) or a
//! future minimal-footprint iceoryx2 consumer (Ideal-A) — the contract and the
//! trust chain are unchanged.

use ed25519_dalek::SigningKey;

use kirra_contract_channel::reference::InProcessRegion;
use kirra_contract_channel::{
    publish, read_coherent_snapshot, validate, AcceptedWatermark, ContractFault,
    GovernorContractView, MAX_SNAPSHOT_RETRIES,
};
use kirra_verifier::governor_release::{issue_release_token, verify_release, ReleaseDenied};

/// A monotonic stream of commands flows the whole chain end-to-end: published
/// under the seqlock, coherently read, validated, signed into a release token,
/// and released only after the actuator re-derives a matching digest.
#[test]
fn monotonic_stream_publishes_validates_signs_and_releases() {
    let region = InProcessRegion::new();
    let governor = SigningKey::from_bytes(&[11u8; 32]);
    let governor_vk = governor.verifying_key();

    let mut committed = 0u64;
    let mut watermark = AcceptedWatermark::new();
    const NOW_NANOS: u64 = 500_000;

    for s in 1..=6u64 {
        let cmd = std::format!("v={s}");

        // Steps 1-2: the (untrusted) guest publishes under the seqlock.
        let body =
            GovernorContractView::new_command(0, s, s * 10, 1_000_000, cmd.as_bytes()).unwrap();
        committed = publish(&region, committed, &body);

        // Step 3: the governor obtains a coherent owned snapshot.
        let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES)
            .expect("quiescent region yields a coherent snapshot");
        assert_eq!(snap.generation, committed);

        // Step 4: the governor validates against the advancing watermark.
        validate(&snap, NOW_NANOS, &watermark).expect("well-formed, fresh, monotonic");
        watermark.record(&snap);

        // Steps 5-6: the governor signs a release token over the validated bytes.
        let token = issue_release_token(&snap, &governor);

        // Step 7: the actuator releases only after re-deriving a matching digest
        // AND verifying the signature against the trusted governor key.
        assert_eq!(verify_release(&token, &snap, &governor_vk), Ok(()));

        // Negative: that token must NOT release any OTHER command.
        let substitute =
            GovernorContractView::new_command(0, s, s * 10, 1_000_000, b"tamper").unwrap();
        assert_eq!(
            verify_release(&token, &substitute, &governor_vk),
            Err(ReleaseDenied::DigestMismatch),
            "a release token is bound to the exact bytes the governor approved"
        );
    }
}

/// A replayed command (equal sequence) is rejected at validation (step 4) — it
/// never reaches the release stage, so no token is ever minted for it.
#[test]
fn replayed_command_is_rejected_before_release() {
    let region = InProcessRegion::new();
    let mut watermark = AcceptedWatermark::new();

    let first = GovernorContractView::new_command(0, 1, 0, 1_000_000, b"go").unwrap();
    let committed = publish(&region, 0, &first);
    let snap = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
    assert_eq!(snap.generation, committed);
    validate(&snap, 0, &watermark).unwrap();
    watermark.record(&snap);

    // Re-read the same committed region (no new publish) → replay on `sequence`.
    let replay = read_coherent_snapshot(&region, MAX_SNAPSHOT_RETRIES).unwrap();
    assert!(matches!(
        validate(&replay, 0, &watermark),
        Err(ContractFault::SequenceRegressOrReplay { .. })
    ));
}
