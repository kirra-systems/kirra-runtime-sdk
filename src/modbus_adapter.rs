// src/modbus_adapter.rs

use crate::{ProtocolAdapter, AdapterError};

pub struct ModbusTcpAdapter {
    pub target_register_offset: u16,
    pub scale_factor: f64,
}

impl ModbusTcpAdapter {
    pub fn new(register_offset: u16, scale: f64) -> Self {
        let valid_scale = if scale <= 0.0 { 1.0 } else { scale };
        Self { target_register_offset: register_offset, scale_factor: valid_scale }
    }
}

impl ProtocolAdapter for ModbusTcpAdapter {
    fn decode_demand(&self, frame: &[u8]) -> Result<f64, AdapterError> {
        if frame.len() < 12 { return Err(AdapterError::FrameTruncated); }
        let protocol_id = u16::from_be_bytes([frame[2], frame[3]]);
        if protocol_id != 0 { return Err(AdapterError::InvalidProtocolIdentifier); }
        let function_code = frame[7];
        if function_code != 6 { return Err(AdapterError::IllegalFunctionCode); }
        let wire_register_offset = u16::from_be_bytes([frame[8], frame[9]]);
        if wire_register_offset != self.target_register_offset { return Err(AdapterError::UnmonitoredRegisterTarget); }
        let raw_integer_counts = u16::from_be_bytes([frame[10], frame[11]]) as f64;
        Ok(raw_integer_counts / self.scale_factor)
    }

    fn encode_response(&self, sanitized_value: f64, original_frame: &[u8]) -> Vec<u8> {
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
