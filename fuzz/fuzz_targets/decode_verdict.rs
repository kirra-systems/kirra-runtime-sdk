#![no_main]
//! Fuzz the governor→car wire decoder (`kirra_wire_client::decode_verdict`).
//!
//! `decode_verdict` bincode-decodes an UNTRUSTED UDP datagram into a typed
//! `Verdict { seq, action: ClientEnforceAction, reason_code }` with nested enum
//! tags and strict trailing-byte rejection. The safety contract is fail-closed:
//! any malformed / truncated / over-long / bad-enum-tag input must return
//! `Err(DecodeError)` — never panic, never abort. libFuzzer flags a panic as a
//! crash, so this target IS that assertion over the whole byte space.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Result deliberately ignored: the property under test is "never panics",
    // not the decoded value. A returned Err is the correct fail-closed outcome.
    let _ = kirra_wire_client::decode_verdict(data);
});
