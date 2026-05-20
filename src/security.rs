// src/security.rs

use std::sync::atomic::{AtomicU8, Ordering};

pub struct VolatileZeroizer;

impl VolatileZeroizer {
    #[inline]
    pub fn zeroize(slice: &mut [u8]) {
        for i in 0..slice.len() {
            unsafe { std::ptr::write_volatile(&mut slice[i], 0u8); }
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}

pub fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
    let bitwise_accumulator = AtomicU8::new(0);
    let length_match = a.len() == b.len();
    let length_mask = if length_match { 0u8 } else { 0xFFu8 };

    for i in 0..64 {
        let byte_a = if i < a.len() { unsafe { std::ptr::read_volatile(&a[i]) } } else { 0u8 };
        let byte_b = if i < b.len() { unsafe { std::ptr::read_volatile(&b[i]) } } else { 0u8 };
        bitwise_accumulator.fetch_or(byte_a ^ byte_b, Ordering::SeqCst);
    }

    bitwise_accumulator.fetch_or(length_mask, Ordering::SeqCst);
    bitwise_accumulator.load(Ordering::SeqCst) == 0
}

pub struct AdministrativeKeyContainer {
    private_auth_key: Vec<u8>,
}

impl AdministrativeKeyContainer {
    pub fn new(initial_key: Vec<u8>) -> Self {
        Self { private_auth_key: initial_key }
    }

    pub fn rotate_key(&mut self, new_key: Vec<u8>) {
        let old_key = std::mem::replace(&mut self.private_auth_key, new_key);
        let mut old_to_zeroize = old_key;
        VolatileZeroizer::zeroize(&mut old_to_zeroize);
    }

    #[inline]
    pub fn verify_token_constant_time(&self, raw_token: &[u8]) -> bool {
        constant_time_compare(raw_token, &self.private_auth_key)
    }
}

impl Drop for AdministrativeKeyContainer {
    fn drop(&mut self) {
        VolatileZeroizer::zeroize(&mut self.private_auth_key);
    }
}
