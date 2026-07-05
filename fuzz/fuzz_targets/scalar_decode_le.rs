#![no_main]
//! Fuzz the generic little-endian scalar field decoder
//! (`kirra_verifier::adapters::ScalarType::decode_le`).
//!
//! This backs the CANopen SDO / EtherNet-IP / DNP3 magnitude-bound checks — it
//! slices the leading `width()` bytes of an untrusted protocol payload and
//! reinterprets them as the configured type. Fail-closed contract: a buffer
//! shorter than the type width must yield `None` — never an out-of-bounds slice,
//! never a panic. The first byte selects the type; the rest is the payload.
use kirra_verifier::adapters::ScalarType;
use libfuzzer_sys::fuzz_target;

const TYPES: [ScalarType; 8] = [
    ScalarType::I8,
    ScalarType::U8,
    ScalarType::I16,
    ScalarType::U16,
    ScalarType::I32,
    ScalarType::U32,
    ScalarType::F32,
    ScalarType::F64,
];

fuzz_target!(|data: &[u8]| {
    let Some((&sel, payload)) = data.split_first() else {
        return;
    };
    let ty = TYPES[(sel as usize) % TYPES.len()];
    // Property: a payload at least `width()` long always decodes to `Some`, and a
    // shorter one always to `None` — never a panic / OOB slice either way.
    match ty.decode_le(payload) {
        Some(_) => assert!(payload.len() >= ty.width()),
        None => assert!(payload.len() < ty.width()),
    }
});
