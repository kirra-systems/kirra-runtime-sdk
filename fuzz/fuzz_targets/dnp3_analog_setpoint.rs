#![no_main]
//! Fuzz the DNP3 g41 (Analog Output) setpoint decoder
//! (`kirra_industrial::adapters::dnp3::decode_analog_setpoint`).
//!
//! Decodes a raw IEEE-1815 control-write setpoint (variation-tagged, LE) from an
//! untrusted industrial frame into `Option<f64>`. Fail-closed contract: a short
//! buffer or unknown variation must yield `None` — never a fabricated value,
//! never a panic / out-of-bounds slice. The first byte selects the variation;
//! the remainder is the payload.
//!
//! NOTE: the decoder decodes floats (variations 3/4) FAITHFULLY, so `Some(NaN)`
//! / `Some(inf)` is a CORRECT result for a NaN/Inf-encoding payload — finiteness
//! is enforced downstream by `AnalogOutputEnvelope::admits`, not here. So this
//! target asserts only the decoder's own contract: it never panics.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&variation, payload)) = data.split_first() else {
        return;
    };
    let _ = kirra_industrial::adapters::dnp3::decode_analog_setpoint(variation, payload);
});
