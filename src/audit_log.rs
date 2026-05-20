// src/audit_log.rs

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SignedAuditRecord {
    pub timestamp_epoch_secs: u64,
    pub transaction_id: u16,
    pub target_register_offset: u16,
    pub original_counts: u16,
    pub sanitized_counts: u16,
    pub policy_narrative: String,
    pub keyed_integrity_tag: Vec<u8>,
}

pub struct AuditSigningEngine { secret_salt_key: [u8; 16] }

impl AuditSigningEngine {
    pub fn new(salt: [u8; 16]) -> Self { Self { secret_salt_key: salt } }

    pub fn compute_record_signature(&self, ts: u64, tx_id: u16, offset: u16, orig: u16, sanitized: u16, narrative: &str) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.secret_salt_key).expect("HMAC key instantiation bounds violated");
        mac.update(&ts.to_be_bytes());
        mac.update(&tx_id.to_be_bytes());
        mac.update(&offset.to_be_bytes());
        mac.update(&orig.to_be_bytes());
        mac.update(&sanitized.to_be_bytes());
        mac.update(narrative.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }

    pub fn generate_signed_record(&self, ts: u64, tx_id: u16, offset: u16, orig: u16, sanitized: u16, narrative: String) -> SignedAuditRecord {
        let keyed_integrity_tag = self.compute_record_signature(ts, tx_id, offset, orig, sanitized, &narrative);
        SignedAuditRecord { timestamp_epoch_secs: ts, transaction_id: tx_id, target_register_offset: offset, original_counts: orig, sanitized_counts: sanitized, policy_narrative: narrative, keyed_integrity_tag }
    }

    pub fn verify_record_integrity(&self, record: &SignedAuditRecord) -> bool {
        let computed = self.compute_record_signature(record.timestamp_epoch_secs, record.transaction_id, record.target_register_offset, record.original_counts, record.sanitized_counts, &record.policy_narrative);
        crate::security::constant_time_compare(&computed, &record.keyed_integrity_tag)
    }
}

impl Drop for AuditSigningEngine {
    fn drop(&mut self) { crate::security::VolatileZeroizer::zeroize(&mut self.secret_salt_key); }
}
