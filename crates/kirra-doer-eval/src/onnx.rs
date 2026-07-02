//! Minimal ONNX export of the learned scorer (Q-1b; `parko/QUANTIZATION_Q1_SCOPE.md` §2).
//!
//! Emits the two per-model artifacts the parko-side backends load:
//!
//! - [`fp32_model`] — the FP32 reference graph: `features → MatMul → Add → Tanh →
//!   MatMul → Add → scores` (opset-13 core ops, no attributes).
//! - [`int8_qdq_model`] — the **explicit-quantization (QDQ)** graph: the same
//!   topology with `QuantizeLinear`/`DequantizeLinear` nodes carrying the
//!   **in-Rust PTQ calibration** (weight codes + `s_x`/`s_h` activation scales from
//!   [`kirra_planner::LearnedPlanner::quantize_int8`]). TensorRT consumes QDQ
//!   models directly in INT8 mode using these embedded scales — no separate
//!   calibration-table format — which is exactly the design-note §6 requirement
//!   that the calibration artifact is produced ONCE and reused by every backend.
//!
//! ## Why hand-encoded protobuf
//!
//! ONNX is protobuf; the two graphs here are tiny and FIXED-topology, so this
//! module hand-encodes the protobuf wire format (~varint + length-delimited only)
//! rather than pulling a protobuf/onnx dependency into the workspace — the same
//! lean-deps ethos as the planner's hand-rolled RNG. The encoding is verified two
//! ways: `tests/onnx_roundtrip.rs` loads the emitted bytes through the REAL ONNX
//! Runtime (via `parko-onnx`, ORT-gated/self-skipping) and asserts the outputs
//! match the Rust scorer; the artifact drift test pins the bytes.
//!
//! Safety framing: offline artifact generation for the UNTRUSTED doer. Nothing
//! here touches the checker.

use kirra_planner::{
    QuantizedScorerWeights, QuantizedScorerWeightsV2, ScorerWeights, ScorerWeightsV2,
};

// --- ONNX constants -----------------------------------------------------------

/// TensorProto.DataType
const DT_FLOAT: u64 = 1;
const DT_INT8: u64 = 3;
/// ONNX IR version 8 (pairs with opsets 13..=19; accepted by ORT and TensorRT).
const IR_VERSION: u64 = 8;
/// Default-domain opset — 13 covers MatMul/Add/Tanh and per-tensor Q/DQ.
const OPSET: u64 = 13;

/// The graph's input/output tensor names — the contract the loaders use.
pub const INPUT_NAME: &str = "features";
pub const OUTPUT_NAME: &str = "scores";

// --- Protobuf wire-format primitives -------------------------------------------
// Wire types: 0 = varint, 2 = length-delimited. That is all ONNX needs here.

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn tag(out: &mut Vec<u8>, field: u32, wire: u8) {
    varint(out, (u64::from(field) << 3) | u64::from(wire));
}

/// `field: <varint>`
fn put_varint(out: &mut Vec<u8>, field: u32, v: u64) {
    tag(out, field, 0);
    varint(out, v);
}

/// `field: <len-delimited bytes>`
fn put_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    tag(out, field, 2);
    varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_str(out: &mut Vec<u8>, field: u32, s: &str) {
    put_bytes(out, field, s.as_bytes());
}

/// Encode a nested message: build its body, emit as a length-delimited field.
fn put_msg(out: &mut Vec<u8>, field: u32, body: impl FnOnce(&mut Vec<u8>)) {
    let mut inner = Vec::new();
    body(&mut inner);
    put_bytes(out, field, &inner);
}

// --- ONNX message builders ------------------------------------------------------

/// TensorProto: an f32 initializer (`dims` empty ⇒ scalar). raw_data is LE f32.
fn f32_tensor(out: &mut Vec<u8>, field: u32, name: &str, dims: &[u64], data: &[f32]) {
    put_msg(out, field, |t| {
        for &d in dims {
            put_varint(t, 1, d); // dims
        }
        put_varint(t, 2, DT_FLOAT); // data_type
        put_str(t, 8, name); // name
        let mut raw = Vec::with_capacity(data.len() * 4);
        for &v in data {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        put_bytes(t, 9, &raw); // raw_data
    });
}

/// TensorProto: an int8 initializer (raw_data = two's-complement bytes).
fn i8_tensor(out: &mut Vec<u8>, field: u32, name: &str, dims: &[u64], data: &[i8]) {
    put_msg(out, field, |t| {
        for &d in dims {
            put_varint(t, 1, d);
        }
        put_varint(t, 2, DT_INT8);
        put_str(t, 8, name);
        let raw: Vec<u8> = data.iter().map(|&v| v as u8).collect();
        put_bytes(t, 9, &raw);
    });
}

/// ValueInfoProto: a named f32 tensor of fixed shape (graph input/output).
fn f32_value_info(out: &mut Vec<u8>, field: u32, name: &str, dims: &[u64]) {
    put_msg(out, field, |vi| {
        put_str(vi, 1, name);
        put_msg(vi, 2, |ty| {
            // TypeProto.tensor_type
            put_msg(ty, 1, |tt| {
                put_varint(tt, 1, DT_FLOAT); // elem_type
                put_msg(tt, 2, |shape| {
                    // TensorShapeProto
                    for &d in dims {
                        put_msg(shape, 1, |dim| put_varint(dim, 1, d)); // dim_value
                    }
                });
            });
        });
    });
}

/// NodeProto: attribute-free op (all ops used here take only defaults).
fn node(out: &mut Vec<u8>, op_type: &str, name: &str, inputs: &[&str], outputs: &[&str]) {
    put_msg(out, 1, |n| {
        for i in inputs {
            put_str(n, 1, i);
        }
        for o in outputs {
            put_str(n, 2, o);
        }
        put_str(n, 3, name);
        put_str(n, 4, op_type);
    });
}

/// ModelProto shell around a GraphProto body.
fn model(graph_name: &str, graph_body: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut m = Vec::new();
    put_varint(&mut m, 1, IR_VERSION);
    put_str(&mut m, 2, "kirra-doer-eval");
    put_msg(&mut m, 7, |g| {
        // GraphProto: nodes/initializers/io are appended by the caller; name here.
        put_str(g, 2, graph_name);
        graph_body(g);
    });
    put_msg(&mut m, 8, |opset| put_varint(opset, 2, OPSET)); // opset_import
    m
}

/// Transpose a `rows × cols` row-major matrix into `cols × rows` row-major — the
/// Rust scorer stores `w[row][col]` per output row; MatMul wants `input × output`.
fn transpose<T: Copy>(m: &[T], rows: usize, cols: usize) -> Vec<T> {
    let mut out = Vec::with_capacity(m.len());
    for c in 0..cols {
        for r in 0..rows {
            out.push(m[r * cols + c]);
        }
    }
    out
}

fn to_f32(v: &[f64]) -> Vec<f32> {
    v.iter().map(|&x| x as f32).collect()
}

// --- The two exported models ----------------------------------------------------

/// The FP32 reference model: `features[1,in] → MatMul(W1) → Add(b1) → Tanh →
/// MatMul(W2) → Add(b2) → scores[1,out]`.
#[must_use]
pub fn fp32_model(w: &ScorerWeights) -> Vec<u8> {
    let (i, h, o) = (w.input_dim, w.hidden_dim, w.output_dim);
    let w1_t = to_f32(&transpose(&w.w1, h, i)); // [in, hidden]
    let w2_t = to_f32(&transpose(&w.w2, o, h)); // [hidden, out]
    model("kirra_planner_scorer_fp32", |g| {
        node(g, "MatMul", "mm1", &[INPUT_NAME, "W1"], &["mm1_out"]);
        node(g, "Add", "add1", &["mm1_out", "B1"], &["pre1"]);
        node(g, "Tanh", "tanh1", &["pre1"], &["hidden"]);
        node(g, "MatMul", "mm2", &["hidden", "W2"], &["mm2_out"]);
        node(g, "Add", "add2", &["mm2_out", "B2"], &[OUTPUT_NAME]);
        f32_tensor(g, 5, "W1", &[i as u64, h as u64], &w1_t);
        f32_tensor(g, 5, "B1", &[h as u64], &to_f32(&w.b1));
        f32_tensor(g, 5, "W2", &[h as u64, o as u64], &w2_t);
        f32_tensor(g, 5, "B2", &[o as u64], &to_f32(&w.b2));
        f32_value_info(g, 11, INPUT_NAME, &[1, i as u64]);
        f32_value_info(g, 12, OUTPUT_NAME, &[1, o as u64]);
    })
}

/// The explicit-quantization (QDQ) int8 model: the same topology with per-tensor
/// `QuantizeLinear`/`DequantizeLinear` carrying the in-Rust PTQ codes + scales.
/// Zero-points are all 0 (symmetric int8), shared via one initializer.
#[must_use]
pub fn int8_qdq_model(q: &QuantizedScorerWeights) -> Vec<u8> {
    let (i, h, o) = (q.input_dim, q.hidden_dim, q.output_dim);
    let w1_t = transpose(&q.w1_codes, h, i); // [in, hidden] int8 codes
    let w2_t = transpose(&q.w2_codes, o, h); // [hidden, out]
    model("kirra_planner_scorer_int8_qdq", |g| {
        // Activation Q/DQ at the calibrated input scale…
        node(g, "QuantizeLinear", "q_x", &[INPUT_NAME, "x_scale", "zp"], &["x_q"]);
        node(g, "DequantizeLinear", "dq_x", &["x_q", "x_scale", "zp"], &["x_dq"]);
        // …int8 weights dequantized at their per-tensor scale…
        node(g, "DequantizeLinear", "dq_w1", &["W1_q", "w1_scale", "zp"], &["w1_dq"]);
        node(g, "MatMul", "mm1", &["x_dq", "w1_dq"], &["mm1_out"]);
        node(g, "Add", "add1", &["mm1_out", "B1"], &["pre1"]);
        node(g, "Tanh", "tanh1", &["pre1"], &["hidden"]);
        // …hidden activations re-quantized at the calibrated hidden scale…
        node(g, "QuantizeLinear", "q_h", &["hidden", "h_scale", "zp"], &["h_q"]);
        node(g, "DequantizeLinear", "dq_h", &["h_q", "h_scale", "zp"], &["h_dq"]);
        node(g, "DequantizeLinear", "dq_w2", &["W2_q", "w2_scale", "zp"], &["w2_dq"]);
        node(g, "MatMul", "mm2", &["h_dq", "w2_dq"], &["mm2_out"]);
        node(g, "Add", "add2", &["mm2_out", "B2"], &[OUTPUT_NAME]);
        // Initializers: codes, scales (f32 scalars), shared zero-point, f32 biases.
        i8_tensor(g, 5, "W1_q", &[i as u64, h as u64], &w1_t);
        i8_tensor(g, 5, "W2_q", &[h as u64, o as u64], &w2_t);
        f32_tensor(g, 5, "x_scale", &[], &[q.input_scale as f32]);
        f32_tensor(g, 5, "h_scale", &[], &[q.hidden_scale as f32]);
        f32_tensor(g, 5, "w1_scale", &[], &[q.w1_scale as f32]);
        f32_tensor(g, 5, "w2_scale", &[], &[q.w2_scale as f32]);
        i8_tensor(g, 5, "zp", &[], &[0]);
        f32_tensor(g, 5, "B1", &[h as u64], &to_f32(&q.b1));
        f32_tensor(g, 5, "B2", &[o as u64], &to_f32(&q.b2));
        f32_value_info(g, 11, INPUT_NAME, &[1, i as u64]);
        f32_value_info(g, 12, OUTPUT_NAME, &[1, o as u64]);
    })
}

// --- The N-layer chain exporters (M-2; `parko/DOER_MODEL_SCALEUP.md` §2) --------
//
// The v2 scorer is an N-layer chain, so these generalize the two fixed-topology
// exporters above to any depth: per hidden layer `MatMul → Add → Tanh`, then a
// final linear `MatMul → Add`; the QDQ variant wraps every matmul input in a
// per-tensor `QuantizeLinear`/`DequantizeLinear` pair carrying the in-Rust PTQ
// calibration, exactly as v1's. The 2-layer `fp32_model` / `int8_qdq_model`
// above are kept VERBATIM (not rewritten as chain calls): the checked-in v1
// artifacts are the exact bytes measured on the Orin (`Q1B_ORIN.md`), and a
// chain-generic naming scheme ("h1" for v1's "hidden", "a0_scale" for
// "x_scale") would churn those pinned bytes for no numerical difference.

/// The FP32 chain model: `features[1,in] → (MatMul(Wi) → Add(Bi) → Tanh)* →
/// MatMul(Wn) → Add(Bn) → scores[1,out]`. Layer names are 1-based (`mm1`…);
/// hidden activations are `h1`…`h{n-1}`.
#[must_use]
pub fn fp32_model_chain(w: &ScorerWeightsV2) -> Vec<u8> {
    assert!(!w.layers.is_empty(), "a scorer chain has at least one layer");
    let in_dim = w.layers[0].in_dim as u64;
    let out_dim = w.layers.last().expect("non-empty").out_dim as u64;
    let n = w.layers.len();
    model("kirra_planner_scorer_v2_fp32", |g| {
        let mut act = INPUT_NAME.to_string();
        for idx in 0..n {
            let i = idx + 1;
            let last = i == n;
            node(g, "MatMul", &format!("mm{i}"), &[&act, &format!("W{i}")], &[&format!("mm{i}_out")]);
            let add_out = if last { OUTPUT_NAME.to_string() } else { format!("pre{i}") };
            node(g, "Add", &format!("add{i}"), &[&format!("mm{i}_out"), &format!("B{i}")], &[&add_out]);
            if !last {
                act = format!("h{i}");
                node(g, "Tanh", &format!("tanh{i}"), &[&add_out], &[&act]);
            }
        }
        for (idx, l) in w.layers.iter().enumerate() {
            let i = idx + 1;
            let w_t = to_f32(&transpose(&l.w, l.out_dim, l.in_dim)); // [in, out]
            f32_tensor(g, 5, &format!("W{i}"), &[l.in_dim as u64, l.out_dim as u64], &w_t);
            f32_tensor(g, 5, &format!("B{i}"), &[l.out_dim as u64], &to_f32(&l.b));
        }
        f32_value_info(g, 11, INPUT_NAME, &[1, in_dim]);
        f32_value_info(g, 12, OUTPUT_NAME, &[1, out_dim]);
    })
}

/// The explicit-quantization (QDQ) chain model: the same topology with every
/// matmul input passed through `QuantizeLinear`/`DequantizeLinear` at its
/// calibrated scale (`a0_scale` = the input features, `a{j}_scale` = hidden
/// activation `j`), and every weight tensor stored as int8 codes dequantized at
/// its per-tensor scale. Zero-points all 0 (symmetric), shared initializer.
#[must_use]
pub fn int8_qdq_model_chain(q: &QuantizedScorerWeightsV2) -> Vec<u8> {
    assert_eq!(
        q.layers.len(),
        q.act_scales.len(),
        "one activation scale per matmul input"
    );
    assert!(!q.layers.is_empty(), "a scorer chain has at least one layer");
    let in_dim = q.layers[0].in_dim as u64;
    let out_dim = q.layers.last().expect("non-empty").out_dim as u64;
    let n = q.layers.len();
    model("kirra_planner_scorer_v2_int8_qdq", |g| {
        let mut act = INPUT_NAME.to_string();
        for idx in 0..n {
            let i = idx + 1;
            let j = idx; // activation index entering this layer
            let last = i == n;
            let (a_q, a_dq, a_scale) =
                (format!("a{j}_q"), format!("a{j}_dq"), format!("a{j}_scale"));
            node(g, "QuantizeLinear", &format!("q_a{j}"), &[&act, &a_scale, "zp"], &[&a_q]);
            node(g, "DequantizeLinear", &format!("dq_a{j}"), &[&a_q, &a_scale, "zp"], &[&a_dq]);
            node(
                g,
                "DequantizeLinear",
                &format!("dq_w{i}"),
                &[&format!("W{i}_q"), &format!("w{i}_scale"), "zp"],
                &[&format!("w{i}_dq")],
            );
            node(g, "MatMul", &format!("mm{i}"), &[&a_dq, &format!("w{i}_dq")], &[&format!("mm{i}_out")]);
            let add_out = if last { OUTPUT_NAME.to_string() } else { format!("pre{i}") };
            node(g, "Add", &format!("add{i}"), &[&format!("mm{i}_out"), &format!("B{i}")], &[&add_out]);
            if !last {
                act = format!("h{i}");
                node(g, "Tanh", &format!("tanh{i}"), &[&add_out], &[&act]);
            }
        }
        for (idx, l) in q.layers.iter().enumerate() {
            let i = idx + 1;
            let codes_t = transpose(&l.codes, l.out_dim, l.in_dim); // [in, out]
            i8_tensor(g, 5, &format!("W{i}_q"), &[l.in_dim as u64, l.out_dim as u64], &codes_t);
            f32_tensor(g, 5, &format!("w{i}_scale"), &[], &[l.w_scale as f32]);
            f32_tensor(g, 5, &format!("B{i}"), &[l.out_dim as u64], &to_f32(&l.b));
        }
        for (j, &s) in q.act_scales.iter().enumerate() {
            f32_tensor(g, 5, &format!("a{j}_scale"), &[], &[s as f32]);
        }
        i8_tensor(g, 5, "zp", &[], &[0]);
        f32_value_info(g, 11, INPUT_NAME, &[1, in_dim]);
        f32_value_info(g, 12, OUTPUT_NAME, &[1, out_dim]);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_planner::{LearnedPlanner, Teacher};

    #[test]
    fn varint_encoding() {
        let mut v = Vec::new();
        varint(&mut v, 0);
        varint(&mut v, 127);
        varint(&mut v, 128);
        varint(&mut v, 300);
        assert_eq!(v, [0x00, 0x7f, 0x80, 0x01, 0xac, 0x02]);
    }

    #[test]
    fn transpose_round_trips() {
        // 2×3 row-major → 3×2 → back.
        let m = [1, 2, 3, 4, 5, 6];
        let t = transpose(&m, 2, 3);
        assert_eq!(t, [1, 4, 2, 5, 3, 6]);
        assert_eq!(transpose(&t, 3, 2), m);
    }

    /// The export is a pure function of the trained weights — byte-deterministic.
    #[test]
    fn export_is_deterministic() {
        let p = LearnedPlanner::trained(0xC0FFEE, Teacher::SafetyAware);
        let w = p.scorer_weights();
        assert_eq!(fp32_model(&w), fp32_model(&w));
        assert_eq!((w.input_dim, w.hidden_dim, w.output_dim), (4, 8, 4));
        assert_eq!(w.w1.len(), 8 * 4);
        assert_eq!(w.w2.len(), 4 * 8);
    }

    /// Structural smoke: the emitted bytes start with the ModelProto ir_version
    /// field (tag 0x08, value 8) and contain the graph i/o names.
    #[test]
    fn model_bytes_look_like_onnx() {
        let p = LearnedPlanner::trained(0xC0FFEE, Teacher::SafetyAware);
        let bytes = fp32_model(&p.scorer_weights());
        assert_eq!(&bytes[..2], &[0x08, IR_VERSION as u8], "ir_version field first");
        let hay = bytes.windows(INPUT_NAME.len()).any(|w| w == INPUT_NAME.as_bytes());
        assert!(hay, "input name embedded");
    }
}
