//! A minimal JSON object writer.
//!
//! Hand-rolled to keep the harness at exactly one external dependency — see the
//! manifest for why that constraint is part of the Fence A argument rather than
//! minimalism for its own sake.
//!
//! Results are emitted as JSON Lines so a drill can be appended to across runs
//! and diffed, and so a partial run still yields readable records: a harness
//! killed during the crash tier must not take its earlier measurements with it.

use std::fmt::Write as _;

/// Escape a string for a JSON double-quoted context, including the control
/// characters below 0x20 that a naive escaper misses. A device-tree model
/// string or a mount source can contain almost anything, and emitting a raw
/// control byte produces a file that a downstream parser rejects wholesale —
/// losing every record in it, not just the bad field.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// A JSON number, or `null` for a non-finite value.
///
/// NaN and infinity are not representable in JSON. Emitting them raw produces a
/// file that fails to parse; emitting `0` would launder a defect into a
/// plausible measurement. `null` is the honest third option: the consumer sees
/// a missing value and the record survives.
pub fn number(v: f64) -> String {
    if v.is_finite() {
        // Enough precision to round-trip a duration in milliseconds without
        // exponent notation for the ranges this harness produces.
        format!("{v:.6}")
    } else {
        "null".to_string()
    }
}

#[derive(Default)]
pub struct Obj {
    parts: Vec<String>,
}

impl Obj {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn str(mut self, key: &str, value: &str) -> Self {
        self.parts
            .push(format!("\"{}\":\"{}\"", escape(key), escape(value)));
        self
    }

    pub fn int(mut self, key: &str, value: u64) -> Self {
        self.parts.push(format!("\"{}\":{}", escape(key), value));
        self
    }

    pub fn float(mut self, key: &str, value: f64) -> Self {
        self.parts
            .push(format!("\"{}\":{}", escape(key), number(value)));
        self
    }

    pub fn bool(mut self, key: &str, value: bool) -> Self {
        self.parts.push(format!("\"{}\":{}", escape(key), value));
        self
    }

    pub fn opt_str(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(v) => self.str(key, v),
            None => self.null(key),
        }
    }

    pub fn null(mut self, key: &str) -> Self {
        self.parts.push(format!("\"{}\":null", escape(key)));
        self
    }

    pub fn str_array(mut self, key: &str, values: &[String]) -> Self {
        let items: Vec<String> = values
            .iter()
            .map(|v| format!("\"{}\"", escape(v)))
            .collect();
        self.parts
            .push(format!("\"{}\":[{}]", escape(key), items.join(",")));
        self
    }

    pub fn obj(mut self, key: &str, value: Obj) -> Self {
        self.parts
            .push(format!("\"{}\":{}", escape(key), value.render()));
        self
    }

    pub fn render(&self) -> String {
        format!("{{{}}}", self.parts.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_the_structural_characters() {
        assert_eq!(escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape("a\nb\tc\rd"), "a\\nb\\tc\\rd");
    }

    #[test]
    fn escapes_control_characters_a_naive_escaper_misses() {
        // A NUL is exactly what a device-tree model string carries, and it is
        // the one most likely to reach this code path.
        assert_eq!(escape("a\u{0}b"), "a\\u0000b");
        assert_eq!(escape("\u{1b}"), "\\u001b");
    }

    #[test]
    fn leaves_non_ascii_alone() {
        // Valid UTF-8 needs no escaping in JSON, and mangling it would corrupt
        // a mount source or a model string for no benefit.
        assert_eq!(escape("µs — Ω"), "µs — Ω");
    }

    #[test]
    fn non_finite_numbers_become_null_not_invalid_json() {
        assert_eq!(number(f64::NAN), "null");
        assert_eq!(number(f64::INFINITY), "null");
        assert_eq!(number(f64::NEG_INFINITY), "null");
        assert_eq!(number(1.5), "1.500000");
    }

    #[test]
    fn renders_a_nested_object() {
        let inner = Obj::new().int("n", 3).bool("ok", true);
        let outer = Obj::new()
            .str("name", "x")
            .obj("inner", inner)
            .null("missing");
        assert_eq!(
            outer.render(),
            r#"{"name":"x","inner":{"n":3,"ok":true},"missing":null}"#
        );
    }

    #[test]
    fn renders_an_empty_object_and_array() {
        assert_eq!(Obj::new().render(), "{}");
        assert_eq!(Obj::new().str_array("k", &[]).render(), r#"{"k":[]}"#);
    }

    #[test]
    fn keys_are_escaped_too() {
        assert_eq!(Obj::new().int("a\"b", 1).render(), r#"{"a\"b":1}"#);
    }
}
