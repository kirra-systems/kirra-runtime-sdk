//! #1214 — what a consumer restart does to replay protection.
//!
//! The intended assertion for this test was:
//!
//!     mint a token under epoch A → restart the consumer →
//!     present the epoch-A token → REJECTED
//!
//! **The protocol cannot make that assertion true today**, and this file pins
//! what actually happens instead. Writing the aspirational assertion and marking
//! it ignored would be worse than useless: it would read as a covered case.
//!
//! The gate's three watermarks — `last_released_sequence`, `last_released_nonce`
//! and `latest_perception_frame` — are in-memory. A restarted consumer therefore
//! accepts any sequence, any nonce and any perception frame. The ONLY thing that
//! refuses a pre-restart token is its own wall-clock lifetime, so the exposure
//! window is exactly `maximum_lifetime_ms` wide and closes by expiry rather than
//! by any restart-aware rule.
//!
//! In the shipped configuration that lifetime is 200 ms
//! (`ROS_BOUND_RELEASE_LIFETIME_MS`) and a real process restart takes far
//! longer, so this is not exploitable as configured. That is a **timing
//! accident, not a designed invariant** — it depends on restart duration
//! exceeding a configurable token lifetime, and a backward wall-clock step
//! during the restart widens it further.
//!
//! The structural fix (a boot/session epoch bound into evidence identity and
//! release tokens, so pre-restart tokens are invalid by construction) is
//! tracked separately. See the issue cited in the tests below.

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

/// A restarted consumer is a NEW gate: same key, same pinned profile, no
/// watermarks. This is what "restart" means throughout this file.
fn restarted_consumer() -> RosBoundCommandGate {
    RosBoundCommandGate::new(key().verifying_key(), PROFILE, MAXIMUM_LIFETIME_MS)
}

/// Documents the gap: a pre-restart token IS accepted after a restart while it
/// remains inside its own wall-clock lifetime.
///
/// This test asserts behaviour the project does not want. It exists so the gap
/// is visible in the test suite rather than only in prose, and so that
/// implementing epoch binding forces a deliberate edit here rather than silently
/// leaving a stale expectation.
#[test]
fn a_pre_restart_token_is_still_accepted_after_restart_within_its_lifetime() {
    let payload = payload();
    let token = issue_ros_bound_command_release(&payload, &key());
    let bytes = payload.encode();

    let mut before = restarted_consumer();
    before
        .release(&bytes, Some(&token), 10_050)
        .expect("released normally under epoch A");

    // The consumer restarts. Every watermark is gone.
    let mut after = restarted_consumer();
    let replayed = after.release(&bytes, Some(&token), 10_060);

    assert!(
        replayed.is_ok(),
        "EXPECTED-BUT-UNDESIRED: a pre-restart token is accepted after restart. \
         If this now fails, epoch binding has been implemented — update this test \
         and the restart section of docs/safety/EVIDENCE_BINDING.md."
    );
}

/// The only thing that closes the window: the token's own expiry.
#[test]
fn the_exposure_window_is_closed_by_expiry_not_by_any_restart_rule() {
    let payload = payload();
    let token = issue_ros_bound_command_release(&payload, &key());
    let bytes = payload.encode();

    let mut after = restarted_consumer();
    let refused = after.release(&bytes, Some(&token), payload.expires_at_ms);

    assert_eq!(
        refused,
        Err(RosBoundCommandRefusal::Expired {
            expires_at_ms: payload.expires_at_ms,
            now_ms: payload.expires_at_ms,
        }),
        "expiry is the sole bound on post-restart replay"
    );
}

/// States the exposure numerically: the window is exactly the token lifetime,
/// and it is a function of a CONFIGURABLE value rather than of restart itself.
///
/// This is the assertion that makes the risk legible. A deployment that raises
/// `KIRRA_FRESHNESS_WINDOW_MS` widens the post-restart replay window by the same
/// amount, which is not obvious from either the variable's name or its
/// documentation.
#[test]
fn the_exposure_window_equals_the_token_lifetime() {
    let payload = payload();
    let token = issue_ros_bound_command_release(&payload, &key());
    let bytes = payload.encode();

    // Accepted at the last instant before expiry…
    let mut just_inside = restarted_consumer();
    assert!(just_inside
        .release(&bytes, Some(&token), payload.expires_at_ms - 1)
        .is_ok());

    // …and refused at expiry. The window is [issued_at, expires_at).
    let mut just_outside = restarted_consumer();
    assert!(just_outside
        .release(&bytes, Some(&token), payload.expires_at_ms)
        .is_err());

    assert_eq!(
        payload.expires_at_ms - payload.issued_at_ms,
        MAXIMUM_LIFETIME_MS,
        "the exposure window IS the token lifetime, so raising the configured \
         lifetime widens post-restart replay by the same amount"
    );
}

/// The post-restart epoch does re-establish normally: a fresh token under new
/// evidence is accepted, and the restarted gate then enforces its watermarks
/// from that point.
///
/// Non-vacuity for the tests above — they must not be passing because the
/// restarted gate accepts everything unconditionally.
#[test]
fn a_restarted_consumer_re_establishes_and_then_enforces_normally() {
    let mut after = restarted_consumer();

    let mut fresh = payload();
    fresh.sequence = 500;
    fresh.nonce = 90_000;
    fresh.scan_sequence = 900;
    fresh.tracker_generation = 9;
    fresh.issued_at_ms = 50_000;
    fresh.expires_at_ms = 50_000 + MAXIMUM_LIFETIME_MS;
    let fresh_token = issue_ros_bound_command_release(&fresh, &key());

    after
        .release(&fresh.encode(), Some(&fresh_token), 50_050)
        .expect("the new epoch's first token is accepted");

    // …and the watermark is live again: the same frame replayed is refused.
    assert!(
        after
            .release(&fresh.encode(), Some(&fresh_token), 50_060)
            .is_err(),
        "the restarted gate must enforce replay protection within its own epoch"
    );

    // A pre-restart token from the OLD epoch is now refused too — but by the
    // sequence watermark the new epoch established, not by anything that knows
    // a restart happened. Had it arrived first, it would have been accepted.
    let stale = payload();
    let stale_token = issue_ros_bound_command_release(&stale, &key());
    assert!(after
        .release(&stale.encode(), Some(&stale_token), 50_070)
        .is_err());
}
