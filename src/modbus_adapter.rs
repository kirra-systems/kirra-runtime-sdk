// src/modbus_adapter.rs

use crate::{AdapterError, ProtocolAdapter};

pub struct ModbusTcpAdapter {
    pub target_register_offset: u16,
    pub scale_factor: f64,
}

impl ModbusTcpAdapter {
    pub fn new(register_offset: u16, scale: f64) -> Self {
        let valid_scale = if scale <= 0.0 { 1.0 } else { scale };
        Self {
            target_register_offset: register_offset,
            scale_factor: valid_scale,
        }
    }
}

impl ProtocolAdapter for ModbusTcpAdapter {
    fn decode_demand(&self, frame: &[u8]) -> Result<f64, AdapterError> {
        if frame.len() < 12 {
            return Err(AdapterError::FrameTruncated);
        }
        let protocol_id = u16::from_be_bytes([frame[2], frame[3]]);
        if protocol_id != 0 {
            return Err(AdapterError::InvalidProtocolIdentifier);
        }
        let function_code = frame[7];
        if function_code != 6 {
            return Err(AdapterError::IllegalFunctionCode);
        }
        let wire_register_offset = u16::from_be_bytes([frame[8], frame[9]]);
        if wire_register_offset != self.target_register_offset {
            return Err(AdapterError::UnmonitoredRegisterTarget);
        }
        let raw_integer_counts = u16::from_be_bytes([frame[10], frame[11]]) as f64;
        Ok(raw_integer_counts / self.scale_factor)
    }

    fn encode_response(&self, sanitized_value: f64, original_frame: &[u8]) -> Vec<u8> {
        // Defense-in-depth: indexing bytes [10]/[11] requires len >= 12.
        // The wired call path goes through `decode_demand` first, which
        // already enforces FrameTruncated on len < 12 — so this guard is
        // unreachable today. Keep it anyway: a safety component cannot
        // rely on caller discipline for memory safety, and a future
        // caller that skips decode must NOT panic the verifier.
        if original_frame.len() < 12 {
            // Runtime guard is the contract: a safety component must NEVER
            // panic on caller error in either debug or release. (No
            // `debug_assert!` here — it would panic under `cargo test` and
            // contradict the no-panic invariant exercised below.)
            tracing::error!(
                len = original_frame.len(),
                "modbus encode_response: under-length frame — returning unmodified (no sanitization applied)"
            );
            return original_frame.to_vec();
        }
        let mut modified_buffer = original_frame.to_vec();
        let raw_counts = (sanitized_value * self.scale_factor).round();
        let safe_u16_bytes = (raw_counts.clamp(0.0, 65535.0) as u16).to_be_bytes();
        modified_buffer[10] = safe_u16_bytes[0];
        modified_buffer[11] = safe_u16_bytes[1];
        modified_buffer
    }

    fn encode_exception(&self, original_frame: &[u8], exception_code: u8) -> Vec<u8> {
        let mut exception_buffer = Vec::with_capacity(10);
        if original_frame.len() < 8 {
            exception_buffer.extend_from_slice(&[0, 0, 0, 0, 0, 3, 1, 0x86, exception_code]);
            return exception_buffer;
        }
        exception_buffer.extend_from_slice(&original_frame[0..2]);
        exception_buffer.extend_from_slice(&[0, 0]);
        exception_buffer.extend_from_slice(&[0, 3]);
        exception_buffer.push(original_frame[6]);
        exception_buffer.push(original_frame[7] | 0x80);
        exception_buffer.push(exception_code);
        exception_buffer
    }
}

#[cfg(test)]
mod modbus_adapter_tests {
    use super::*;

    /// Under-length frames must NOT panic the verifier. The wired call path
    /// gates on `decode_demand`'s len>=12 check, but a safety component
    /// cannot rely on caller discipline for memory safety.
    #[test]
    fn test_encode_response_short_frame_does_not_panic() {
        let adapter = ModbusTcpAdapter::new(0, 1.0);
        let short_frame: [u8; 8] = [0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x01, 0x06];
        let out = adapter.encode_response(42.0, &short_frame);
        assert_eq!(
            out.as_slice(),
            &short_frame[..],
            "under-length frame must be returned unmodified, not panic or partially written"
        );
    }

    /// Empty frame is also handled — exercises the extreme of the guard.
    #[test]
    fn test_encode_response_empty_frame_does_not_panic() {
        let adapter = ModbusTcpAdapter::new(0, 1.0);
        let out = adapter.encode_response(42.0, &[]);
        assert!(
            out.is_empty(),
            "empty-frame input must return empty-frame output without panic"
        );
    }

    /// Sanity: a well-formed 12-byte frame still works end-to-end.
    #[test]
    fn test_encode_response_writes_clamped_value_into_bytes_10_11() {
        let adapter = ModbusTcpAdapter::new(0, 1.0);
        let frame: [u8; 12] = [0, 1, 0, 0, 0, 6, 1, 6, 0, 0, 0xFF, 0xFF];
        let out = adapter.encode_response(1.0, &frame);
        assert_eq!(out.len(), 12);
        assert_eq!(out[10], 0x00);
        assert_eq!(out[11], 0x01);
    }
}
