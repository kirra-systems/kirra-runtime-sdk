//! Object **detector** path: `SensorFrame` → `InferenceBackend` → **decode** →
//! `Detection`s.
//!
//! Distinct from Parko's end-to-end driving *policy* (sensor → `ControlCommand`): a
//! detector emits object boxes for the perception world model that feeds RSS and the
//! perception-redundancy cross-check. The **decode** — confidence threshold + non-max
//! suppression on the raw output tensor — is pure Rust and fully tested here; the model
//! weights live behind the hardware backends and run through the SAME
//! [`InferenceBackend`] seam (the mock in tests, TensorRT on the Orin). So this ships
//! the *detector pipeline* without pinning a model: swap the backend, keep the decode.
//!
//! Out of scope (a calibration concern, not parko-core's): projecting an image-frame
//! [`BBox`] to a world-frame object. That needs camera intrinsics/extrinsics + depth
//! and belongs at the integration boundary, where the result becomes the adapter's
//! `PerceivedObject` the checker consumes.

use crate::backend::{BackendError, InferenceBackend, ModelHandle, TensorBatch};

/// An axis-aligned box in the detector's output frame (center + size). Units are
/// whatever the model emits (normalized or pixels) — the integrator's calibration
/// resolves them downstream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
}

impl BBox {
    fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    /// Intersection-over-union with `other`.
    pub fn iou(&self, other: &BBox) -> f32 {
        let (ax0, ay0) = (self.cx - self.w / 2.0, self.cy - self.h / 2.0);
        let (ax1, ay1) = (self.cx + self.w / 2.0, self.cy + self.h / 2.0);
        let (bx0, by0) = (other.cx - other.w / 2.0, other.cy - other.h / 2.0);
        let (bx1, by1) = (other.cx + other.w / 2.0, other.cy + other.h / 2.0);
        let iw = (ax1.min(bx1) - ax0.max(bx0)).max(0.0);
        let ih = (ay1.min(by1) - ay0.max(by0)).max(0.0);
        let inter = iw * ih;
        let union = self.area() + other.area() - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }
}

/// One detected object: a class, a confidence, and a box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub class_id: u32,
    pub confidence: f32,
    pub bbox: BBox,
}

/// Decode + post-processing parameters for [`run_detector`] / [`decode_detections`].
#[derive(Debug, Clone)]
pub struct DetectorConfig {
    /// Name of the backend output tensor holding the detection rows.
    pub output_tensor: String,
    /// Values per detection row. Must be ≥ 6: `[cx, cy, w, h, confidence, class_id]`.
    pub stride: usize,
    /// Drop detections below this confidence.
    pub conf_threshold: f32,
    /// Suppress a lower-confidence box overlapping a kept same-class box above this IoU.
    pub iou_threshold: f32,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            output_tensor: "detections".to_string(),
            stride: 6,
            conf_threshold: 0.5,
            iou_threshold: 0.45,
        }
    }
}

/// Why a detector run could not produce detections.
#[derive(Debug)]
pub enum DetectError {
    /// The backend's `run()` failed.
    Backend(BackendError),
    /// The configured output tensor was absent from the backend's result.
    MissingOutputTensor(String),
}

impl core::fmt::Display for DetectError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DetectError::Backend(e) => write!(f, "detector backend error: {e}"),
            DetectError::MissingOutputTensor(name) => {
                write!(f, "detector output tensor '{name}' missing from backend result")
            }
        }
    }
}

impl std::error::Error for DetectError {}

/// Decode a flat row-major detector output `[N × stride]` into detections: drop rows
/// below `conf_threshold` (or with non-finite values), then **class-aware greedy NMS**
/// by `iou_threshold`. A `stride < 6` or a ragged tail yields no detections — fail-safe
/// on an undecodable tensor (the safety layer never trusts a frame it cannot read).
#[must_use]
pub fn decode_detections(raw: &[f32], cfg: &DetectorConfig) -> Vec<Detection> {
    if cfg.stride < 6 {
        return Vec::new();
    }
    let mut dets: Vec<Detection> = raw
        .chunks_exact(cfg.stride)
        .filter_map(|row| {
            let conf = row[4];
            if !conf.is_finite() || conf < cfg.conf_threshold {
                return None;
            }
            let bbox = BBox { cx: row[0], cy: row[1], w: row[2], h: row[3] };
            if ![bbox.cx, bbox.cy, bbox.w, bbox.h].iter().all(|v| v.is_finite()) {
                return None;
            }
            Some(Detection { class_id: row[5].max(0.0) as u32, confidence: conf, bbox })
        })
        .collect();
    nms(&mut dets, cfg.iou_threshold)
}

/// Class-aware greedy non-max suppression: keep the highest-confidence box, suppress
/// any same-class box overlapping a kept one above `iou_threshold`.
fn nms(dets: &mut [Detection], iou_threshold: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let mut keep: Vec<Detection> = Vec::new();
    for &d in dets.iter() {
        let suppressed = keep
            .iter()
            .any(|k| k.class_id == d.class_id && k.bbox.iou(&d.bbox) > iou_threshold);
        if !suppressed {
            keep.push(d);
        }
    }
    keep
}

/// Run a detector end-to-end: execute `backend` on `inputs`, pull the configured output
/// tensor, and decode it. **Fail-closed:** a backend error or a missing output tensor is
/// returned as `Err` (the caller MRCs rather than driving on an empty world model).
pub fn run_detector(
    backend: &dyn InferenceBackend,
    model: &ModelHandle,
    inputs: &TensorBatch,
    cfg: &DetectorConfig,
) -> Result<Vec<Detection>, DetectError> {
    let out = backend.run(model, inputs).map_err(DetectError::Backend)?;
    let tensor = out
        .named_tensors
        .get(&cfg.output_tensor)
        .ok_or_else(|| DetectError::MissingOutputTensor(cfg.output_tensor.clone()))?;
    Ok(decode_detections(tensor.as_slice(), cfg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendDescriptor;
    use crate::backends::mock::MockBackend;
    use std::collections::HashMap;

    fn cfg() -> DetectorConfig {
        DetectorConfig::default()
    }

    #[test]
    fn iou_is_one_for_identical_boxes_and_zero_for_disjoint() {
        let a = BBox { cx: 0.0, cy: 0.0, w: 2.0, h: 2.0 };
        let b = BBox { cx: 10.0, cy: 10.0, w: 2.0, h: 2.0 };
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
        assert_eq!(a.iou(&b), 0.0);
    }

    #[test]
    fn decode_drops_low_confidence_rows() {
        // Two rows: one above threshold (0.5), one below.
        let raw = vec![
            1.0, 1.0, 2.0, 2.0, 0.9, 0.0, // conf 0.9 → kept (class 0)
            5.0, 5.0, 2.0, 2.0, 0.2, 1.0, // conf 0.2 → dropped
        ];
        let dets = decode_detections(&raw, &cfg());
        assert_eq!(dets.len(), 1);
        assert!((dets[0].confidence - 0.9).abs() < 1e-6);
        assert_eq!(dets[0].class_id, 0);
    }

    #[test]
    fn nms_suppresses_an_overlapping_same_class_box() {
        // Two heavily-overlapping class-0 boxes (IoU≈1) + one distinct class-0 box.
        let raw = vec![
            0.0, 0.0, 2.0, 2.0, 0.9, 0.0, // kept (highest conf)
            0.1, 0.1, 2.0, 2.0, 0.8, 0.0, // suppressed (overlaps the 0.9 box)
            20.0, 20.0, 2.0, 2.0, 0.7, 0.0, // kept (no overlap)
        ];
        let dets = decode_detections(&raw, &cfg());
        assert_eq!(dets.len(), 2, "the overlapping duplicate is suppressed");
        assert!((dets[0].confidence - 0.9).abs() < 1e-6, "NMS keeps the higher-confidence box");
    }

    #[test]
    fn nms_keeps_overlapping_boxes_of_different_classes() {
        // Same geometry as above, but the duplicate is a DIFFERENT class → not suppressed.
        let raw = vec![
            0.0, 0.0, 2.0, 2.0, 0.9, 0.0,
            0.1, 0.1, 2.0, 2.0, 0.8, 1.0, // class 1 → kept despite overlap
        ];
        assert_eq!(decode_detections(&raw, &cfg()).len(), 2);
    }

    #[test]
    fn decode_ignores_non_finite_and_ragged_rows() {
        let raw = vec![
            1.0, 1.0, 2.0, 2.0, f32::NAN, 0.0, // non-finite conf → dropped
            3.0, 3.0, f32::INFINITY, 2.0, 0.9, 0.0, // non-finite box → dropped
            7.0, 7.0, 2.0, 2.0, 0.95, 2.0, // valid → kept
            9.0, 9.0, // ragged tail → ignored by chunks_exact
        ];
        let dets = decode_detections(&raw, &cfg());
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].class_id, 2);
    }

    #[test]
    fn run_detector_decodes_the_named_output_through_the_backend() {
        let mut out = HashMap::new();
        out.insert(
            "detections".to_string(),
            vec![4.0, 4.0, 2.0, 2.0, 0.88, 3.0_f32],
        );
        let backend = MockBackend::new(out, BackendDescriptor::Cpu);
        let model = backend.load_model("yolo.onnx").unwrap();
        let inputs = TensorBatch { named_tensors: HashMap::new(), metadata: HashMap::new() };

        let dets = run_detector(&backend, &model, &inputs, &cfg()).unwrap();
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].class_id, 3);
    }

    #[test]
    fn run_detector_fails_closed_on_a_missing_output_tensor() {
        let mut out = HashMap::new();
        out.insert("something_else".to_string(), vec![0.0_f32; 6]);
        let backend = MockBackend::new(out, BackendDescriptor::Cpu);
        let model = backend.load_model("m").unwrap();
        let inputs = TensorBatch { named_tensors: HashMap::new(), metadata: HashMap::new() };

        let err = run_detector(&backend, &model, &inputs, &cfg()).unwrap_err();
        assert!(matches!(err, DetectError::MissingOutputTensor(_)));
    }
}
