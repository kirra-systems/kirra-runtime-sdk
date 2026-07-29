//! #1230 — what a consumer restart does to replay protection.
//!
//! HISTORY: this file originally pinned the UNDESIRED behaviour — a restarted
//! consumer accepted any pre-restart token still inside its own wall-clock
//! lifetime, because all three gate watermarks are in-memory and expiry was
//! the only refusal left standing. That made the post-restart replay window
//! exactly `maximum_lifetime_ms` wide, i.e. a function of the CONFIGURABLE
//! `KIRRA_FRESHNESS_WINDOW_MS` rather than of restart itself.
//!
//! #1230 Part B closes that coupling: the gate now carries `boot_wall_ms`,
//! and any token ISSUED before this process booted is refused with the
//! distinct `TokenPredatesBoot` — regardless of how much signed lifetime it
//! has left. A restart is a new trust epoch; pre-boot mints belong to the
//! previous one.
//!
//! HONEST SCOPE (why #1230 stays open): this is the INTERIM, not the
//! structural fix. The comparison is same-clock-domain — sound on the
//! ADR-0033 single-host topology — but time-anchored, so a backward wall
//! step DURING the restart re-opens the window; that remainder is pinned
//! below rather than hidden, exactly as this file once pinned the original
//! gap. The structural fix (Part A: a boot epoch bound into the SIGNED
//! payload, wire V3, refusal `TokenEpochMismatch`) is the ADR-0033 revision
//! tracked on #1230.

use ed25519_dalek::SigningKey;
use kirra_release_token::ros_bound_command::{
    issue_ros_bound_command_release, RosBoundCommandGate, RosBoundCommandPayload,
    RosBoundCommandRefusal,
};

const MAXIMUM_LIFETIME_MS: u64 = 200;
const PROFILE: [u8; 32] = [0x11; 32];

fn key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

fn payload() -> RosBoundCommandPayload {
    RosBoundCommandPayload {
        sequence: 10,
        nonce: 9001,
        issued_at_ms: 10_000,
        expires_at_ms: 10_000 + MAXIMUM_LIFETIME_MS,
        linear_mps: 0.75,
        angular_rad_s: -0.25,
        scan_sequence: 77,
        camera_sequence: Some(88),
        tracker_generation: 3,
        profile_digest: PROFILE,
        evidence_digest: [0x22; 32],
        proposal_digest: [0x33; 32],
    }
}

/// A consumer process with a stated boot instant: same key, same pinned
/// profile, no watermarks. This is what "restart" means throughout this file —
/// every in-memory watermark is gone, and only `boot_wall_ms` distinguishes
/// the new epoch from the old.
fn consumer_booted_at(boot_wall_ms: u64) -> RosBoundCommandGate {
    RosBoundCommandGate::new(
        key().verifying_key(),
        PROFILE,
        MAXIMUM_LIFETIME_MS,
        boot_wall_ms,
    )
}

/// THE FLIP (#1230 acceptance): a pre-restart token is refused after restart
/// REGARDLESS of its remaining lifetime, with the distinct predates-boot code
/// — not incidentally by expiry.
///
/// This test previously asserted the opposite, deliberately, so that the fix
/// would force this edit rather than leave a stale expectation.
#[test]
fn a_pre_restart_token_is_refused_after_restart_despite_remaining_lifetime() {
    let payload = payload();
    let token = issue_ros_bound_command_release(&payload, &key());
    let bytes = payload.encode();

    // Epoch A: consumer booted before the mint; releases normally.
    let mut before = consumer_booted_at(9_000);
    before
        .release(&bytes, Some(&token), 10_050)
        .expect("released normally under epoch A");

    // Restart at 10_055 — AFTER the mint. The token still has ~140 ms of
    // signed lifetime at now=10_060; that lifetime no longer matters.
    let mut after = consumer_booted_at(10_055);
    let replayed = after.release(&bytes, Some(&token), 10_060);

    assert_eq!(
        replayed,
        Err(RosBoundCommandRefusal::TokenPredatesBoot {
            issued_at_ms: 10_000,
            boot_wall_ms: 10_055,
        }),
        "a restart is a new trust epoch: pre-boot mints are refused at the \
         boundary, not by waiting out their expiry"
    );
}

/// No bootstrap deadlock (#1230 acceptance): the first legitimately fresh
/// token after a restart — minted at or after the boot instant — is admitted
/// on the first cycle, and the gate then enforces its watermarks normally
/// within the new epoch (non-vacuity: the refusals above must not be a gate
/// that refuses everything).
#[test]
fn a_fresh_post_restart_token_is_admitted_and_the_epoch_then_enforces_normally() {
    let mut consumer = consumer_booted_at(10_055);

    let mut fresh = payload();
    fresh.sequence = 500;
    fresh.nonce = 90_000;
    fresh.scan_sequence = 900;
    fresh.tracker_generation = 9;
    fresh.issued_at_ms = 50_000;
    fresh.expires_at_ms = 50_000 + MAXIMUM_LIFETIME_MS;
    let fresh_token = issue_ros_bound_command_release(&fresh, &key());

    consumer
        .release(&fresh.encode(), Some(&fresh_token), 50_050)
        .expect("the new epoch's first token is accepted — no deadlock");

    // The watermark is live again: the same frame replayed is refused.
    assert!(
        consumer
            .release(&fresh.encode(), Some(&fresh_token), 50_060)
            .is_err(),
        "the restarted gate must enforce replay protection within its own epoch"
    );
}

/// The boundary is strict `<`: a token minted at EXACTLY the boot instant is
/// admitted. The verifier minting in the same millisecond the consumer comes
/// up is the current epoch, not the previous one.
#[test]
fn a_token_minted_at_the_boot_instant_is_admitted() {
    let mut consumer = consumer_booted_at(10_000);
    let p = payload(); // issued_at_ms == 10_000 == boot
    let token = issue_ros_bound_command_release(&p, &key());
    consumer
        .release(&p.encode(), Some(&token), 10_050)
        .expect("issued_at == boot is the current epoch");
}

/// A token that is BOTH pre-boot and expired is refused on every path.
/// Pinned so a future reordering of the gate's checks cannot silently admit
/// one of the two cases.
#[test]
fn a_pre_boot_and_expired_token_is_refused() {
    let p = payload();
    let token = issue_ros_bound_command_release(&p, &key());
    let mut consumer = consumer_booted_at(10_055);
    let refused = consumer.release(&p.encode(), Some(&token), p.expires_at_ms + 10);
    assert!(refused.is_err(), "no path admits it: {refused:?}");
}

/// The exposure statement, updated: the window no longer scales with the
/// configured token lifetime. Pre-Part-B, "one ms before expiry" was the
/// just-inside acceptance that made the window equal the lifetime; now the
/// boot instant refuses it however much lifetime remains — including under a
/// ten-fold longer configured lifetime.
#[test]
fn the_exposure_window_no_longer_scales_with_the_configured_lifetime() {
    let p = payload();
    let token = issue_ros_bound_command_release(&p, &key());
    let mut just_after_boot = consumer_booted_at(p.issued_at_ms + 1);
    assert!(matches!(
        just_after_boot.release(&p.encode(), Some(&token), p.expires_at_ms - 1),
        Err(RosBoundCommandRefusal::TokenPredatesBoot { .. })
    ));

    let mut long = payload();
    long.expires_at_ms = long.issued_at_ms + 10 * MAXIMUM_LIFETIME_MS;
    let long_token = issue_ros_bound_command_release(&long, &key());
    let mut consumer = RosBoundCommandGate::new(
        key().verifying_key(),
        PROFILE,
        10 * MAXIMUM_LIFETIME_MS,
        long.issued_at_ms + 1,
    );
    assert!(matches!(
        consumer.release(&long.encode(), Some(&long_token), long.issued_at_ms + 500),
        Err(RosBoundCommandRefusal::TokenPredatesBoot { .. })
    ));
}

/// THE HONEST REMAINDER (Part A's justification, kept visible): a backward
/// wall-clock step DURING the restart re-opens the window. If the clock steps
/// back far enough that the consumer's recorded boot instant precedes the
/// token's mint time, the predates-boot rule is blind by construction and
/// only expiry is left — the pre-Part-B behaviour, restored by the clock
/// rather than by any code change.
///
/// This test asserts behaviour the project does not want, exactly as this
/// file's original test did for the un-guarded gate: the gap stays visible in
/// the suite until Part A (the boot epoch in the SIGNED payload, which no
/// clock step can forge) closes it. When `TokenEpochMismatch` exists, this
/// test flips.
#[test]
fn a_backward_clock_step_across_restart_reopens_the_window() {
    let p = payload(); // minted at 10_000
    let token = issue_ros_bound_command_release(&p, &key());

    // The restart happens after the mint, but the wall clock stepped back:
    // the consumer records boot at 9_990 — BEFORE the mint it is trying to
    // fence off.
    let mut consumer = consumer_booted_at(9_990);
    let replayed = consumer.release(&p.encode(), Some(&token), 10_050);

    assert!(
        replayed.is_ok(),
        "EXPECTED-BUT-UNDESIRED: a backward clock step across the restart \
         defeats the time-anchored boot fence. If this now fails, Part A \
         (structural epoch binding) has landed — update this test and \
         docs/safety/EVIDENCE_BINDING.md §4.7."
    );
}
