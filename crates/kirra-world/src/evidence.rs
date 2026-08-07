//! **Chain identity** — the digest of a record, and the link to the one before.
//!
//! The last of Tier 1's typing gaps, moved to Tier 3 by
//! `KIRRA-WM-TIER1-DONE-001` because that is where it is first *required*:
//! Tier 3's rule 1 says every answer carries a `ProvenanceHandle`, and a handle
//! over a bare hex string is the thing that rule exists to prevent.
//!
//! # What these replace
//!
//! `String`, in both positions. The store computes a record's digest with
//! `kirra_audit_hash::compute_record_hash_v2` and stores it as text; the link
//! to the previous record is text too. Nothing distinguished either from a
//! subject, a payload, or each other.
//!
//! # The rule this makes structural: verbatim, and lower-case
//!
//! [`EvidenceDigest`] admits **exactly 64 lower-case hex characters** and
//! stores them **verbatim**. Both halves are load-bearing, and the second is
//! the one that looks like pedantry and is not.
//!
//! `hex::encode` emits lower case. `verify_chain` compares digests by **string
//! equality**, not by decoded value. So `"AB…"` and `"ab…"` are the *same hash*
//! and *different strings* — and a type that helpfully lower-cased on the way in
//! would produce a value that no longer matches what a tampered-with or
//! foreign-cased row actually contains, reporting an intact chain as broken or a
//! broken one as intact depending on which side normalized.
//!
//! Refusing upper case is therefore not a style rule. It is the same
//! "validate, never normalize" discipline `reference.rs` applies for the same
//! reason: **the stored bytes are the evidence, and a constructor that improves
//! them is corrupting them.**
//!
//! `DigestError::UppercaseHex` is separate from `NonHexCharacter` deliberately.
//! *"This is a well-formed digest in the wrong case"* and *"this is not a
//! digest"* send an investigator to different questions — the first suggests a
//! producer that used `HEX::encode` semantics, the second suggests the column
//! holds something else entirely.
//!
//! # Why [`PrevHash`] is an enum and not a digest
//!
//! **The first record has no predecessor.** Its previous-hash position holds
//! the sentinel [`GENESIS_TOKEN`] — `kirra-world:genesis`, which is not a hash
//! of anything and is not hex.
//!
//! A single `EvidenceDigest` in that position would force one of two bad
//! choices: widen the digest type to admit non-hex (destroying the invariant
//! that makes it worth having), or **fabricate a digest for genesis** — a
//! synthetic value in a hash-chained evidence log, which is the worse of the
//! two by a distance.
//!
//! So the position gets a type that says what it is: either the beginning, or a
//! link to a real record. A reader can no longer forget that the first case
//! exists, because the compiler will not let them.
//!
//! # These do not change any stored bytes
//!
//! [`GENESIS_TOKEN`] is inside the hashed bytes of every first record —
//! `compute_record_hash_v2` takes the previous hash as its first field — so its
//! value is frozen exactly as the `kind` tokens are. The store re-exports this
//! constant rather than defining its own, so there is one definition rather
//! than two that could drift.

use core::fmt;

/// Length of a hex-encoded SHA-256 digest.
pub const DIGEST_HEX_LEN: usize = 64;

/// The previous-hash sentinel for the first record in a chain.
///
/// **Frozen.** It is hashed into every first record, so changing it does not
/// migrate a store — it makes existing first records unverifiable.
pub const GENESIS_TOKEN: &str = "kirra-world:genesis";

/// Why a value was refused as an [`EvidenceDigest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestError {
    /// Not [`DIGEST_HEX_LEN`] characters.
    WrongLength {
        /// What was offered, in characters.
        len: usize,
    },
    /// Contained a character that is not a hex digit at all.
    NonHexCharacter,
    /// Well-formed hex, but containing upper-case letters.
    ///
    /// Separate from [`Self::NonHexCharacter`] because the two mean different
    /// things to whoever is diagnosing it, and because this is the case a
    /// normalizing constructor would have silently "fixed" — see the module
    /// docs for why that would corrupt chain verification rather than help it.
    UppercaseHex,
}

impl fmt::Display for DigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { len } => {
                write!(f, "expected {DIGEST_HEX_LEN} hex characters, got {len}")
            }
            Self::NonHexCharacter => f.write_str("not a hex digest"),
            Self::UppercaseHex => {
                f.write_str("upper-case hex: digests are compared as strings, so case is not free")
            }
        }
    }
}

/// A record's position in the tamper-evident log — its SHA-256, hex-encoded.
///
/// Admitted verbatim. See the module docs on why nothing is normalized.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvidenceDigest(String);

impl EvidenceDigest {
    /// Admit a digest.
    ///
    /// # Errors
    ///
    /// [`DigestError`] if the value is not exactly [`DIGEST_HEX_LEN`]
    /// characters of **lower-case** hex.
    pub fn new(v: impl Into<String>) -> Result<Self, DigestError> {
        let v = v.into();
        if v.chars().count() != DIGEST_HEX_LEN {
            return Err(DigestError::WrongLength {
                len: v.chars().count(),
            });
        }
        // Order matters: report the case problem specifically when the value is
        // otherwise a valid digest, so "wrong case" never hides behind the
        // blunter "not a digest".
        if v.chars().all(|c| c.is_ascii_hexdigit()) {
            if v.chars().any(|c| c.is_ascii_uppercase()) {
                return Err(DigestError::UppercaseHex);
            }
            return Ok(Self(v));
        }
        Err(DigestError::NonHexCharacter)
    }

    /// The digest, exactly as admitted.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this digest, returning the inner string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for EvidenceDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the previous-hash position holds: the beginning, or a real link.
///
/// See the module docs on why this is not an [`EvidenceDigest`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrevHash {
    /// The first record in the chain. Carries [`GENESIS_TOKEN`], which is not
    /// a hash and is not pretending to be one.
    Genesis,
    /// A link to the record before this one.
    Digest(EvidenceDigest),
}

impl PrevHash {
    /// Read a stored previous-hash value.
    ///
    /// # Errors
    ///
    /// [`DigestError`] when the value is neither [`GENESIS_TOKEN`] nor an
    /// admissible digest. Note what this does **not** do: it never falls back
    /// to [`Self::Genesis`] for an unparseable value. A corrupt link that read
    /// as "the beginning" would make a truncated chain verify as a complete
    /// one.
    pub fn parse(v: &str) -> Result<Self, DigestError> {
        if v == GENESIS_TOKEN {
            return Ok(Self::Genesis);
        }
        EvidenceDigest::new(v).map(Self::Digest)
    }

    /// The stored form — the sentinel, or the digest.
    ///
    /// This is what goes into the hashed bytes, so it must round-trip
    /// [`Self::parse`] exactly.
    #[must_use]
    pub fn as_token(&self) -> &str {
        match self {
            Self::Genesis => GENESIS_TOKEN,
            Self::Digest(d) => d.as_str(),
        }
    }

    /// The digest, or `None` at the beginning of the chain.
    ///
    /// `None` is *"there is no previous record"*, never *"the previous record
    /// is unknown"*.
    #[must_use]
    pub fn digest(&self) -> Option<&EvidenceDigest> {
        match self {
            Self::Genesis => None,
            Self::Digest(d) => Some(d),
        }
    }
}

impl fmt::Display for PrevHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_token())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    // -- EvidenceDigest admission ------------------------------------------

    #[test]
    fn a_real_sha256_hex_digest_is_admitted_verbatim() {
        let d = EvidenceDigest::new(REAL).expect("admissible");
        assert_eq!(d.as_str(), REAL, "stored exactly as offered");
    }

    /// **The load-bearing refusal.** Upper-case hex is the same *hash* and a
    /// different *string*, and `verify_chain` compares strings.
    ///
    /// Revert this to a `to_lowercase()` and the type starts silently rewriting
    /// evidence: a row stored upper-case would compare equal through the type
    /// and unequal in SQL, so the chain would verify differently depending on
    /// which path read it.
    #[test]
    fn uppercase_hex_is_refused_rather_than_lowercased() {
        let err = EvidenceDigest::new(REAL.to_uppercase()).expect_err("refused");
        assert_eq!(err, DigestError::UppercaseHex);
    }

    /// The two failure modes stay distinguishable.
    #[test]
    fn not_hex_and_wrong_case_are_different_diagnoses() {
        let not_hex: String = "z".repeat(DIGEST_HEX_LEN);
        assert_eq!(
            EvidenceDigest::new(not_hex).expect_err("refused"),
            DigestError::NonHexCharacter
        );
        // ...and a mixed-case value that IS hex reports the case, not the
        // blunter "not a digest".
        let mut mixed = REAL.to_string();
        mixed.replace_range(0..1, "E");
        assert_eq!(
            EvidenceDigest::new(mixed).expect_err("refused"),
            DigestError::UppercaseHex
        );
    }

    #[test]
    fn a_digest_of_the_wrong_length_is_refused_with_its_length() {
        assert_eq!(
            EvidenceDigest::new("abc").expect_err("refused"),
            DigestError::WrongLength { len: 3 }
        );
        assert_eq!(
            EvidenceDigest::new(String::new()).expect_err("refused"),
            DigestError::WrongLength { len: 0 }
        );
        let long = format!("{REAL}0");
        assert_eq!(
            EvidenceDigest::new(long).expect_err("refused"),
            DigestError::WrongLength { len: 65 }
        );
    }

    /// Length is counted in **characters**, so a multi-byte value cannot slip
    /// through a byte-length check and then fail somewhere further along.
    #[test]
    fn length_is_counted_in_characters_not_bytes() {
        let multibyte: String = "é".repeat(DIGEST_HEX_LEN);
        assert_eq!(
            EvidenceDigest::new(multibyte).expect_err("refused"),
            DigestError::NonHexCharacter,
            "64 characters, 128 bytes -- reaches the hex check rather than the length one"
        );
    }

    // -- PrevHash ----------------------------------------------------------

    #[test]
    fn the_genesis_token_parses_as_the_beginning_not_as_a_digest() {
        let p = PrevHash::parse(GENESIS_TOKEN).expect("admissible");
        assert_eq!(p, PrevHash::Genesis);
        assert_eq!(p.digest(), None, "the beginning has no previous digest");
        assert_eq!(p.as_token(), GENESIS_TOKEN);
    }

    #[test]
    fn a_real_link_parses_as_a_digest() {
        let p = PrevHash::parse(REAL).expect("admissible");
        assert_eq!(p.digest().expect("a link").as_str(), REAL);
        assert_eq!(p.as_token(), REAL);
    }

    /// **A corrupt link must not read as the beginning.**
    ///
    /// Revert `parse` to fall back to `Genesis` on an unparseable value and this
    /// fails — which is the point. A truncated chain whose first surviving row
    /// claimed to be the beginning would verify as a complete one, and the
    /// missing history would be invisible.
    #[test]
    fn an_unparseable_link_is_refused_rather_than_read_as_genesis() {
        for bad in ["", "kirra-world:GENESIS", "not-a-digest", "deadbeef"] {
            let out = PrevHash::parse(bad);
            assert!(out.is_err(), "{bad:?} must not parse");
            assert_ne!(out.ok(), Some(PrevHash::Genesis));
        }
    }

    /// The stored form round-trips, in both variants.
    ///
    /// `as_token` is what goes into the hashed bytes, so a value that did not
    /// survive the round trip would change a record's digest.
    #[test]
    fn both_variants_round_trip_through_the_stored_form() {
        for original in [PrevHash::Genesis, PrevHash::parse(REAL).unwrap()] {
            let token = original.as_token().to_string();
            assert_eq!(PrevHash::parse(&token).expect("round trips"), original);
        }
    }

    /// The sentinel is pinned by value, not merely by name.
    ///
    /// It is hashed into every first record, so this constant is frozen the way
    /// the `kind` tokens are: changing it does not migrate a store, it makes
    /// existing first records unverifiable.
    #[test]
    fn the_genesis_token_is_frozen() {
        assert_eq!(GENESIS_TOKEN, "kirra-world:genesis");
    }
}
