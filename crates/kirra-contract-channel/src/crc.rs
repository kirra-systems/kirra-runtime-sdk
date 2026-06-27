//! CRC-32/IEEE (reflected, polynomial `0xEDB88320`) — the integrity check over
//! the command payload (HVCHAN-001 §3 step 4).
//!
//! Implemented in-crate (no dependency) to keep the boundary-contract TCB a
//! struct definition plus this arithmetic. Standard zlib/Ethernet CRC-32: init
//! `0xFFFFFFFF`, reflected input/output, final XOR `0xFFFFFFFF`.

/// CRC-32/IEEE over `data`. Matches zlib `crc32` / Ethernet FCS.
pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    let mut i = 0;
    while i < data.len() {
        crc ^= data[i] as u32;
        let mut bit = 0;
        while bit < 8 {
            // Reflected: shift right, conditionally xor the reflected polynomial.
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            bit += 1;
        }
        i += 1;
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_answer_vectors() {
        // Canonical CRC-32/IEEE check values.
        assert_eq!(crc32_ieee(b""), 0x0000_0000);
        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32_ieee(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    #[test]
    fn single_bit_flip_changes_crc() {
        let a = crc32_ieee(b"steer:1.5");
        let b = crc32_ieee(b"steer:1.6");
        assert_ne!(a, b);
    }
}
