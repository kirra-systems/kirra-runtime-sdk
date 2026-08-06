//! The canonical hashed form, pinned to exact bytes.
//!
//! # Why this file exists, and why it was written *before* the change it guards
//!
//! [`kirra_world_store::canonical_event_json`] is the input to every chain
//! digest the store has ever computed. Its field order, its separators, its
//! null spelling and its number formatting are all load-bearing: change any of
//! them and every previously written store fails [`WorldStore::verify_chain`]
//! — not because a row was edited, but because the verifier now asks a
//! different question of it. There is no migration for that
//! (`KIRRA-WM2-SCHEMA-001` §4), and the failure surfaces as
//! `StoreError::ChainBroken`, which reads as tampering.
//!
//! The function's own doc comment already says field order is fixed and
//! explicit "because a reordering would silently change every digest ever
//! computed". That was a comment. Nothing checked it.
//!
//! **A test asserting the post-change bytes would prove nothing** — it would be
//! written from whatever the code then produced, and would agree with a
//! regression as readily as with correctness. So this file was committed
//! *first*, pinning the bytes the code produces today, before the identity
//! types landed. From here on, any edit that moves a byte reddens this test.
//!
//! The digest is spelled out too. The literal JSON catches a reordering
//! legibly; the digest catches the case where the literal itself is edited to
//! match a regression, since updating both in agreement is a deliberate act.

use kirra_world_store::{
    canonical_event_json, provenance_json, ClaimStatus, EventId, FrameId, MapId, NewEvent,
    ObservationId, WriterClass,
};

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// The four typed references, owned so a `NewEvent` can borrow them.
struct Refs {
    event: EventId,
    observation: ObservationId,
    frame: FrameId,
    map: MapId,
}

impl Refs {
    fn fixture() -> Self {
        Self {
            event: EventId::new("evt-000000000000000000000001").unwrap(),
            observation: ObservationId::new("obs-000000000000000000000001").unwrap(),
            frame: FrameId::new("base_link").unwrap(),
            map: MapId::new("lab-floor-2").unwrap(),
        }
    }
}

/// Every field populated, including the optional ones, so an accidental change
/// to how `Some`/`None` are spelled cannot hide behind a null.
fn fully_populated<'a>(r: &'a Refs, provenance: &'a [&'a str]) -> NewEvent<'a> {
    NewEvent {
        event_id: &r.event,
        observation_id: &r.observation,
        txn_time_ms: 1_700_000_000_000,
        valid_from_ms: 1_699_999_999_000,
        valid_to_ms: Some(1_700_000_060_000),
        source: "test-sensor",
        source_version: "0.1.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance,
        frame_id: Some(&r.frame),
        map_id: Some(&r.map),
        kind: "spatial",
        subject: "cup-1",
        predicate: Some("located_in"),
        object: Some("room-3"),
        payload: r#"{"x":1.5,"y":-2.0}"#,
        payload_schema: 3,
        retention_class: "raw",
    }
}

#[test]
fn canonical_json_is_byte_for_byte_what_it_has_always_been() {
    let r = Refs::fixture();
    let prov: [&str; 2] = ["obs-a", "obs-b"];
    let got = canonical_event_json(&fully_populated(&r, &prov));

    let expected = concat!(
        r#"{"event_id":"evt-000000000000000000000001","#,
        r#""observation_id":"obs-000000000000000000000001","#,
        r#""txn_time_ms":1700000000000,"valid_from_ms":1699999999000,"#,
        r#""valid_to_ms":1700000060000,"source":"test-sensor","#,
        r#""source_version":"0.1.0","writer_class":"sensor","#,
        r#""claim_status":"confirmed","provenance":["obs-a","obs-b"],"#,
        r#""frame_id":"base_link","map_id":"lab-floor-2","kind":"spatial","#,
        r#""subject":"cup-1","predicate":"located_in","object":"room-3","#,
        r#""payload":"{\"x\":1.5,\"y\":-2.0}","payload_schema":3,"#,
        r#""retention_class":"raw"}"#
    );

    assert_eq!(got, expected, "canonical hashed form changed");
}

#[test]
fn canonical_json_digest_is_pinned() {
    let r = Refs::fixture();
    let prov: [&str; 2] = ["obs-a", "obs-b"];
    let got = sha256_hex(canonical_event_json(&fully_populated(&r, &prov)).as_bytes());

    // Recorded from the implementation as it stood when the identity types were
    // introduced. If this changes, the chain in every existing store changed
    // with it.
    assert_eq!(
        got, "0892728f937f181099b7f4a3221fd295eab7e478d30719d207e6016efc4d81ee",
        "the canonical form's digest changed — every existing store's chain \
         now verifies against different bytes"
    );
}

/// The absent case pinned separately. `None` spelled as `null` rather than as
/// an omitted key is what keeps the form fixed-arity, and a serializer swap
/// that started omitting nulls would still produce valid JSON — just a
/// different, silently incompatible canonical form.
#[test]
fn absent_optionals_are_spelled_null_not_omitted() {
    let r = Refs::fixture();
    let mut e = fully_populated(&r, &[]);
    e.valid_to_ms = None;
    e.frame_id = None;
    e.map_id = None;
    e.predicate = None;
    e.object = None;
    e.kind = "observation";

    let got = canonical_event_json(&e);

    assert!(
        got.contains(r#""valid_to_ms":null"#),
        "valid_to_ms must be spelled null: {got}"
    );
    assert!(
        got.contains(r#""frame_id":null,"map_id":null"#),
        "frame_id and map_id must be spelled null and stay adjacent: {got}"
    );
    assert!(
        got.contains(r#""predicate":null,"object":null"#),
        "predicate and object must be spelled null and stay adjacent: {got}"
    );
    assert_eq!(
        got.matches("null").count(),
        5,
        "exactly the five optional fields are null: {got}"
    );
}

/// Empty provenance is `[]`, not `null` and not an omitted key. SD-3 stores the
/// chain a claim rests on; "rests on nothing recorded" is a real answer and has
/// to survive a round trip distinguishably.
#[test]
fn empty_provenance_is_an_empty_array() {
    let r = Refs::fixture();
    let e = fully_populated(&r, &[]);
    assert!(canonical_event_json(&e).contains(r#""provenance":[]"#));
    assert_eq!(provenance_json(&[]), "[]");
}

/// The escaping path, pinned. A payload containing a quote or a backslash is
/// ordinary — every JSON payload with a nested string hits this — and an
/// escaping change would alter digests for exactly those rows while leaving
/// simpler ones agreeing, which is the hardest kind of divergence to diagnose.
#[test]
fn embedded_quotes_and_backslashes_escape_stably() {
    let r = Refs::fixture();
    let mut e = fully_populated(&r, &[]);
    e.payload = r#"{"path":"C:\\tmp","q":"\"hi\""}"#;
    e.subject = "cup\"1";

    let got = canonical_event_json(&e);

    assert!(
        got.contains(r#""subject":"cup\"1""#),
        "quote in a bare field must escape as \\\": {got}"
    );
    assert!(
        got.contains(r#"C:\\\\tmp"#),
        "a backslash inside the payload must survive double-encoding: {got}"
    );
    // The whole form still parses — escaping that produced invalid JSON would
    // break every downstream reader, not just the digest.
    let parsed: serde_json::Value =
        serde_json::from_str(&got).expect("canonical form is valid JSON");
    assert_eq!(parsed["subject"], "cup\"1");
    // Recovers the payload TEXT verbatim — the payload is an opaque string to
    // this layer, so the two-character `\\` it contains is content, not an
    // escape this form is entitled to collapse.
    assert_eq!(parsed["payload"], r#"{"path":"C:\\tmp","q":"\"hi\""}"#);
}

/// Field *count* and *order*, asserted independently of the byte pin above.
/// The pin catches any change; this names which property broke, so a failure
/// reads as "a field was added in the middle" rather than "these long strings
/// differ somewhere".
#[test]
fn field_order_is_fixed_and_complete() {
    let r = Refs::fixture();
    let prov: [&str; 1] = ["obs-a"];
    let got = canonical_event_json(&fully_populated(&r, &prov));

    const ORDER: [&str; 19] = [
        "event_id",
        "observation_id",
        "txn_time_ms",
        "valid_from_ms",
        "valid_to_ms",
        "source",
        "source_version",
        "writer_class",
        "claim_status",
        "provenance",
        "frame_id",
        "map_id",
        "kind",
        "subject",
        "predicate",
        "object",
        "payload",
        "payload_schema",
        "retention_class",
    ];

    let mut cursor = 0usize;
    for key in ORDER {
        let needle = format!("\"{key}\":");
        let at = got[cursor..]
            .find(&needle)
            .unwrap_or_else(|| panic!("{key} missing or out of order in: {got}"));
        cursor += at + needle.len();
    }

    let parsed: serde_json::Value = serde_json::from_str(&got).expect("valid JSON");
    assert_eq!(
        parsed.as_object().expect("object").len(),
        ORDER.len(),
        "a field was added to the canonical form without being pinned here"
    );
}
