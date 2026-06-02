// parko/crates/parko-ros2/src/sensor_mapping.rs
//
// Mapping: incoming ROS 2 sensor message → parko-core `SensorFrame`.
//
// This is one of the two seams the integrator overrides per platform.
// The mapping is intentionally pure (no ROS imports here — the node
// crate provides the ROS-side deserialization and hands the typed
// payload to a `SensorInputMapping`). That keeps this module
// unit-testable on stable and lets the same mapping be used in a
// CARLA harness or a bag-replay test.

use std::collections::HashMap;

use parko_core::backend::{TensorBatch, TensorStorage};
use parko_core::sensor::SensorFrame;

/// Integrator-supplied sensor → tensor mapping. The integrator
/// implements this trait against their concrete sensor message type
/// (a flattened image vector, a lidar batch, fused features, etc.)
/// and hands it to the node.
///
/// Implementations must be `Send + Sync` so the node's drain task can
/// hold an `Arc<dyn SensorInputMapping>`.
pub trait SensorInputMapping: Send + Sync {
    /// The integrator's concrete sensor message type. Project-local —
    /// `r2r::UntypedMessage` deserialised JSON, a hand-rolled struct,
    /// or whatever the sensor publisher emits. The node side
    /// instantiates `Self::Sample` from the r2r untyped subscription
    /// before calling `to_frame`.
    type Sample;

    /// Map one observation to a `SensorFrame`. `frame_id` is a
    /// monotonic counter the caller maintains. `timestamp_ms` is
    /// the wall-clock timestamp of the observation (typically from
    /// `header.stamp` on the ROS side); the staleness check in the
    /// tick pipeline compares this to wall clock at tick time.
    fn to_frame(
        &self,
        frame_id: u64,
        timestamp_ms: u64,
        sample: &Self::Sample,
    ) -> SensorFrame;
}

/// A test-only mapping that wraps a vector of f32 features under a
/// single tensor name. Used by the stable-lane tests in
/// `tick_pipeline_tests` and reusable by integrators as a starting
/// point for a real sensor.
#[derive(Debug, Clone)]
pub struct VectorMapping {
    tensor_name: String,
}

impl VectorMapping {
    #[must_use]
    pub fn new(tensor_name: impl Into<String>) -> Self {
        Self { tensor_name: tensor_name.into() }
    }
}

impl SensorInputMapping for VectorMapping {
    type Sample = Vec<f32>;

    fn to_frame(&self, frame_id: u64, timestamp_ms: u64, sample: &Vec<f32>) -> SensorFrame {
        let mut named_tensors: HashMap<String, TensorStorage<'static>> =
            HashMap::with_capacity(1);
        named_tensors.insert(
            self.tensor_name.clone(),
            TensorStorage::Owned(sample.clone()),
        );
        // `SensorFrame::new` stamps `current_time_ms()` itself; for
        // staleness-correctness we want the timestamp the sensor
        // emitted. Construct the struct directly using its public
        // fields.
        SensorFrame {
            frame_id,
            timestamp_ms,
            payload: TensorBatch {
                named_tensors,
                metadata: HashMap::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_mapping_preserves_payload() {
        let m = VectorMapping::new("obs");
        let frame = m.to_frame(42, 1_000, &vec![1.0, 2.0, 3.0]);
        assert_eq!(frame.frame_id, 42);
        assert_eq!(frame.timestamp_ms, 1_000);
        let tensor = frame.payload.named_tensors.get("obs").expect("tensor present");
        assert_eq!(tensor.as_slice(), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn vector_mapping_is_send_sync() {
        // Compile-time check: the trait object must be `Send + Sync`
        // so the node can pass it across the drain-task boundary.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VectorMapping>();
        let _: Box<dyn SensorInputMapping<Sample = Vec<f32>> + Send + Sync>
            = Box::new(VectorMapping::new("obs"));
    }
}

// ===========================================================================
// Camera mapping
// ===========================================================================
//
// Pure (no-ROS) transform from a raw camera frame to a model-ready
// `TensorBatch`. Splits the bug-prone parts from the integrator's hands:
//
//   - Channel order (`rgb8` vs `bgr8` — the classic bug).
//   - Pixel normalization (`[0,1]`, `[-1,1]`, per-channel mean/std).
//   - Tensor layout (NCHW vs NHWC).
//   - Resize (nearest-neighbour; bilinear is a future option).
//
// The transform takes a `CameraSample` (borrowed bytes + src dimensions)
// and produces a `TensorBatch<'static>` keyed by the configured tensor
// name. The `SensorInputMapping` impl wraps the pure transform so the
// node can drive it through the standard trait dispatch.
//
// ROS adapter: behind the `ros2` feature, `image_msg_to_sample` is a
// thin field-extraction shim that pulls bytes + encoding + dims from a
// `sensor_msgs/Image` and hands them to the pure transform.

/// Source-image pixel encoding. Determines bytes-per-pixel and channel
/// order. Extend cautiously — every new variant needs a tested mapping
/// to the NCHW/NHWC output layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraEncoding {
    /// Three bytes per pixel, channel order R, G, B.
    Rgb8,
    /// Three bytes per pixel, channel order B, G, R.
    /// **The most common integrator bug is treating a `Bgr8` source as
    /// `Rgb8` — pixel values look right but the model sees swapped
    /// channels.** This enum makes the choice explicit.
    Bgr8,
    /// One byte per pixel, single channel (grayscale).
    Mono8,
}

impl CameraEncoding {
    #[must_use]
    pub fn channels(self) -> usize {
        match self {
            Self::Rgb8 | Self::Bgr8 => 3,
            Self::Mono8 => 1,
        }
    }
}

/// How to normalize 8-bit pixel values to floats.
#[derive(Debug, Clone, PartialEq)]
pub enum CameraNormalization {
    /// `value / 255.0` — output in `[0.0, 1.0]`. The most common
    /// starting point.
    Unit01,
    /// `value / 127.5 - 1.0` — output in `[-1.0, 1.0]`. Common for
    /// models trained with `tanh` outputs / certain GAN backbones.
    SignedUnit,
    /// Per-channel `(value/255.0 - mean[c]) / std[c]`. Matches the
    /// ImageNet preprocessing convention.
    ///
    /// Length of `mean` and `std` MUST equal the source encoding's
    /// channel count (3 for Rgb8/Bgr8, 1 for Mono8). A mismatch
    /// produces `CameraMappingError::NormalizationChannelMismatch`
    /// at `to_tensor` time so the operator sees the misconfiguration
    /// rather than a silently-wrong tensor.
    MeanStd {
        mean: Vec<f32>,
        std:  Vec<f32>,
    },
}

/// Output tensor layout. The model's input contract dictates which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraLayout {
    /// `[1, C, H, W]` — PyTorch / ONNX convention.
    Nchw,
    /// `[1, H, W, C]` — TensorFlow / TFLite convention.
    Nhwc,
}

/// Resize algorithm. M1 only ships nearest-neighbour — it's the simplest
/// to test exactly (no interpolation artefacts) and works well as a
/// first-pass mapping. Bilinear is the obvious next addition; gate it
/// behind a feature when added so models that need EXACT nearest results
/// don't drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraResize {
    /// Nearest-neighbour resize. Pixel `(y, x)` in the target comes
    /// from `src[(y * src_h / dst_h, x * src_w / dst_w)]`. Sufficient
    /// for first-pass integration; not appropriate for high-fidelity
    /// vision policies, which need bilinear or area resampling.
    Nearest,
}

/// Configuration for `CameraMapping`. Every field is a choice the
/// integrator must make explicitly; defaults are documented but not
/// silently applied.
#[derive(Debug, Clone)]
pub struct CameraConfig {
    /// Source pixel encoding.
    pub encoding: CameraEncoding,
    /// Target tensor height (rows). Must match the model's input
    /// height for the configured layout.
    pub target_height: u32,
    /// Target tensor width (columns). Must match the model's input
    /// width for the configured layout.
    pub target_width: u32,
    /// Resize algorithm used when `(src_w, src_h) != (target_w,
    /// target_h)`. When the dimensions already match, this field is
    /// ignored (identity copy).
    pub resize: CameraResize,
    /// Pixel-value normalization. See `CameraNormalization`.
    pub normalization: CameraNormalization,
    /// Output tensor layout (NCHW vs NHWC).
    pub layout: CameraLayout,
    /// Tensor name inside the produced `TensorBatch`. Must match the
    /// model's input-node name.
    pub tensor_name: String,
}

/// One raw camera observation, borrowed. The pure transform
/// (`CameraMapping::to_tensor`) accepts this; the owned variant
/// (`OwnedCameraSample`) is what the `SensorInputMapping` trait impl
/// stores and lends to the transform.
#[derive(Debug, Clone, Copy)]
pub struct CameraSample<'a> {
    pub bytes:      &'a [u8],
    pub src_width:  u32,
    pub src_height: u32,
}

/// Owned variant of `CameraSample` used as `SensorInputMapping::Sample`
/// for the camera trait impl.
#[derive(Debug, Clone)]
pub struct OwnedCameraSample {
    pub bytes:      Vec<u8>,
    pub src_width:  u32,
    pub src_height: u32,
}

impl OwnedCameraSample {
    #[must_use]
    pub fn borrowed(&self) -> CameraSample<'_> {
        CameraSample {
            bytes:      &self.bytes,
            src_width:  self.src_width,
            src_height: self.src_height,
        }
    }
}

/// Errors the pure camera transform may return. The
/// `SensorInputMapping::to_frame` trait impl maps these to a fall-back
/// zero tensor (the tick pipeline's MRC path then takes over); the
/// pure transform returns them so direct callers (tests, CARLA
/// fixtures) can assert correctness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraMappingError {
    /// `bytes.len() != src_width * src_height * channels`. The integrator's
    /// source image is malformed.
    ByteCountMismatch  { expected: usize, got: usize },
    /// `src_width == 0` or `src_height == 0` — would divide by zero in
    /// the resize step.
    InvalidDimensions  { width: u32, height: u32 },
    /// `MeanStd` channel count disagrees with the encoding's channel
    /// count.
    NormalizationChannelMismatch { expected: usize, got: usize },
    /// `MeanStd` has a non-finite `mean[channel]`, or a `std[channel]` that
    /// is non-finite or `<= 0`. `(value/255 - mean)/std` would then be
    /// non-finite (`std == 0` → ±inf/NaN), violating the "MeanStd → finite"
    /// invariant. Rejected before any output is produced so the integrator
    /// sees the misconfiguration rather than a silently non-finite tensor.
    MeanStdNonFiniteScale { channel: usize },
}

/// Pure camera-to-tensor mapping. Cloning is cheap.
#[derive(Debug, Clone)]
pub struct CameraMapping {
    config: CameraConfig,
}

impl CameraMapping {
    #[must_use]
    pub fn new(config: CameraConfig) -> Self {
        Self { config }
    }

    /// The pure transform. Same input → same output, no I/O.
    pub fn to_tensor(
        &self,
        sample: &CameraSample<'_>,
    ) -> Result<TensorBatch<'static>, CameraMappingError> {
        let channels = self.config.encoding.channels();

        if sample.src_width == 0 || sample.src_height == 0 {
            return Err(CameraMappingError::InvalidDimensions {
                width: sample.src_width, height: sample.src_height,
            });
        }
        let expected = (sample.src_width as usize)
            * (sample.src_height as usize)
            * channels;
        if sample.bytes.len() != expected {
            return Err(CameraMappingError::ByteCountMismatch {
                expected, got: sample.bytes.len(),
            });
        }
        if let CameraNormalization::MeanStd { mean, std } = &self.config.normalization {
            if mean.len() != channels || std.len() != channels {
                return Err(CameraMappingError::NormalizationChannelMismatch {
                    expected: channels,
                    got: mean.len().min(std.len()),
                });
            }
            // Fail-closed scale validation (quality-hardening finding): the
            // per-channel transform `(value/255 - mean[c]) / std[c]` is finite
            // for every valid byte ONLY if each mean is finite and each std is
            // finite and strictly positive. `std == 0` yields ±inf/NaN; a
            // non-finite mean/std yields non-finite output. Reject here, before
            // producing any output, so "MeanStd → finite" holds at the source
            // instead of relying on the downstream governor's non-finite guard.
            for c in 0..channels {
                if !mean[c].is_finite() || !std[c].is_finite() || std[c] <= 0.0 {
                    return Err(CameraMappingError::MeanStdNonFiniteScale { channel: c });
                }
            }
        }

        let dst_h = self.config.target_height as usize;
        let dst_w = self.config.target_width  as usize;
        let total = dst_h * dst_w * channels;
        let mut out = vec![0.0_f32; total];

        for dy in 0..dst_h {
            // Nearest-neighbour vertical source index.
            let sy = dy * (sample.src_height as usize) / dst_h;
            for dx in 0..dst_w {
                let sx = dx * (sample.src_width as usize) / dst_w;
                let src_idx = (sy * (sample.src_width as usize) + sx) * channels;
                for c in 0..channels {
                    // 1) Read the source byte AT THE SOURCE CHANNEL OFFSET.
                    let src_c = src_idx + c;
                    let raw   = sample.bytes[src_c] as f32;

                    // 2) Map source channel offset c → output channel
                    //    offset. For Rgb8 / Mono8 this is identity; for
                    //    Bgr8 it swaps so output channel 0 is the R
                    //    component (channel 2 in BGR source), channel
                    //    1 is G (1), channel 2 is B (0). The OUTPUT
                    //    is therefore ALWAYS RGB-ordered for 3-channel
                    //    encodings — integrators get consistent NCHW
                    //    semantics regardless of source order.
                    let dst_c = match self.config.encoding {
                        CameraEncoding::Bgr8 => channels - 1 - c,
                        CameraEncoding::Rgb8 | CameraEncoding::Mono8 => c,
                    };

                    // 3) Normalize.
                    let n = match &self.config.normalization {
                        CameraNormalization::Unit01     => raw / 255.0,
                        CameraNormalization::SignedUnit => raw / 127.5 - 1.0,
                        CameraNormalization::MeanStd { mean, std } => {
                            (raw / 255.0 - mean[dst_c]) / std[dst_c]
                        }
                    };

                    // 4) Write to the configured layout. NCHW =
                    //    [1, C, H, W]; NHWC = [1, H, W, C].
                    let out_idx = match self.config.layout {
                        CameraLayout::Nchw => dst_c * dst_h * dst_w + dy * dst_w + dx,
                        CameraLayout::Nhwc => dy * dst_w * channels + dx * channels + dst_c,
                    };
                    out[out_idx] = n;
                }
            }
        }

        let mut named = HashMap::new();
        named.insert(self.config.tensor_name.clone(), TensorStorage::Owned(out));
        Ok(TensorBatch { named_tensors: named, metadata: HashMap::new() })
    }
}

impl SensorInputMapping for CameraMapping {
    type Sample = OwnedCameraSample;

    fn to_frame(&self, frame_id: u64, timestamp_ms: u64, sample: &Self::Sample) -> SensorFrame {
        match self.to_tensor(&sample.borrowed()) {
            Ok(batch) => SensorFrame { frame_id, timestamp_ms, payload: batch },
            Err(err) => {
                // The trait can't surface errors. Emit a structured
                // log + a zero-tensor frame; the tick pipeline's
                // staleness watchdog / governor MRC path catches the
                // downstream consequences.
                tracing::error!(
                    ?err, frame_id, timestamp_ms,
                    "CameraMapping::to_frame received malformed input; emitting zero tensor (downstream MRC will fire)"
                );
                let dst_h = self.config.target_height as usize;
                let dst_w = self.config.target_width  as usize;
                let channels = self.config.encoding.channels();
                let mut named = HashMap::new();
                named.insert(
                    self.config.tensor_name.clone(),
                    TensorStorage::Owned(vec![0.0_f32; dst_h * dst_w * channels]),
                );
                SensorFrame {
                    frame_id, timestamp_ms,
                    payload: TensorBatch { named_tensors: named, metadata: HashMap::new() },
                }
            }
        }
    }
}

// ===========================================================================
// Odometry mapping
// ===========================================================================
//
// Pure transform from a pose + twist observation to a flat state-vector
// tensor. Used by state-based control policies (which see a vector of
// state features rather than a camera image).
//
// Configurable surface:
//   - Field selection: position / orientation / linear vel / angular vel
//     toggles independently.
//   - Orientation representation: quaternion (raw 4 floats), full Euler
//     (roll, pitch, yaw — 3 floats), or yaw-only (1 float, the planar-
//     control default).
//
// Output layout (fixed, in order): pos.x, pos.y, pos.z,
// {orientation block}, vlin.x, vlin.y, vlin.z, vang.x, vang.y, vang.z.
// Each block appears only if the matching `include_*` toggle is on.

/// How to represent the orientation portion of the state vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OdomOrientation {
    /// Raw quaternion `(x, y, z, w)` — 4 floats. Use when the model
    /// was trained on quaternion inputs (rare in robot policy work
    /// but valid).
    Quaternion,
    /// Full Euler `(roll, pitch, yaw)` — 3 floats. Use when the model
    /// expects a non-planar orientation summary.
    FullEuler,
    /// Yaw only — 1 float. **Default for planar control**; matches the
    /// `quat_to_yaw` helper Parko's adapter side uses for 2-D
    /// trajectories.
    Yaw,
}

impl OdomOrientation {
    #[must_use]
    pub fn float_count(self) -> usize {
        match self {
            Self::Quaternion => 4,
            Self::FullEuler  => 3,
            Self::Yaw        => 1,
        }
    }
}

/// Configuration for `OdomMapping`.
#[derive(Debug, Clone)]
pub struct OdomConfig {
    pub include_position:         bool,
    pub include_orientation:      Option<OdomOrientation>,
    pub include_linear_velocity:  bool,
    pub include_angular_velocity: bool,
    pub tensor_name: String,
}

impl OdomConfig {
    /// Total length of the produced state vector.
    #[must_use]
    pub fn vector_len(&self) -> usize {
        (if self.include_position         { 3 } else { 0 })
            + self.include_orientation.map(|o| o.float_count()).unwrap_or(0)
            + (if self.include_linear_velocity  { 3 } else { 0 })
            + (if self.include_angular_velocity { 3 } else { 0 })
    }
}

/// One odometry observation. ROS quaternion convention: `(x, y, z, w)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OdomSample {
    pub position:          [f64; 3],
    pub orientation_xyzw:  [f64; 4],
    pub linear_velocity:   [f64; 3],
    pub angular_velocity:  [f64; 3],
}

/// Pure odom-to-tensor mapping.
#[derive(Debug, Clone)]
pub struct OdomMapping {
    config: OdomConfig,
}

impl OdomMapping {
    #[must_use]
    pub fn new(config: OdomConfig) -> Self {
        Self { config }
    }

    /// The pure transform.
    pub fn to_tensor(&self, sample: &OdomSample) -> TensorBatch<'static> {
        let mut v: Vec<f32> = Vec::with_capacity(self.config.vector_len());

        if self.config.include_position {
            v.push(sample.position[0] as f32);
            v.push(sample.position[1] as f32);
            v.push(sample.position[2] as f32);
        }
        if let Some(o) = self.config.include_orientation {
            let [qx, qy, qz, qw] = sample.orientation_xyzw;
            match o {
                OdomOrientation::Quaternion => {
                    v.push(qx as f32); v.push(qy as f32);
                    v.push(qz as f32); v.push(qw as f32);
                }
                OdomOrientation::FullEuler => {
                    let (roll, pitch, yaw) = quat_to_euler(qx, qy, qz, qw);
                    v.push(roll as f32);
                    v.push(pitch as f32);
                    v.push(yaw as f32);
                }
                OdomOrientation::Yaw => {
                    let (_, _, yaw) = quat_to_euler(qx, qy, qz, qw);
                    v.push(yaw as f32);
                }
            }
        }
        if self.config.include_linear_velocity {
            v.push(sample.linear_velocity[0] as f32);
            v.push(sample.linear_velocity[1] as f32);
            v.push(sample.linear_velocity[2] as f32);
        }
        if self.config.include_angular_velocity {
            v.push(sample.angular_velocity[0] as f32);
            v.push(sample.angular_velocity[1] as f32);
            v.push(sample.angular_velocity[2] as f32);
        }

        let mut named = HashMap::new();
        named.insert(self.config.tensor_name.clone(), TensorStorage::Owned(v));
        TensorBatch { named_tensors: named, metadata: HashMap::new() }
    }
}

impl SensorInputMapping for OdomMapping {
    type Sample = OdomSample;

    fn to_frame(&self, frame_id: u64, timestamp_ms: u64, sample: &OdomSample) -> SensorFrame {
        SensorFrame {
            frame_id, timestamp_ms,
            payload: self.to_tensor(sample),
        }
    }
}

/// ROS quaternion `(x, y, z, w)` → Euler `(roll, pitch, yaw)` in radians.
/// Tait–Bryan ZYX intrinsic convention (yaw about Z, then pitch about Y,
/// then roll about X). The same convention `kirra-ros2-adapter::geometry::quat_to_yaw`
/// uses, so adapter + parko-ros2 agree on what "yaw" means.
fn quat_to_euler(qx: f64, qy: f64, qz: f64, qw: f64) -> (f64, f64, f64) {
    // roll (x-axis rotation)
    let sinr_cosp = 2.0 * (qw * qx + qy * qz);
    let cosr_cosp = 1.0 - 2.0 * (qx * qx + qy * qy);
    let roll = sinr_cosp.atan2(cosr_cosp);

    // pitch (y-axis rotation). Clamped to ±π/2 at the gimbal-lock pole.
    let sinp = 2.0 * (qw * qy - qz * qx);
    let pitch = if sinp.abs() >= 1.0 {
        std::f64::consts::FRAC_PI_2.copysign(sinp)
    } else {
        sinp.asin()
    };

    // yaw (z-axis rotation)
    let siny_cosp = 2.0 * (qw * qz + qx * qy);
    let cosy_cosp = 1.0 - 2.0 * (qy * qy + qz * qz);
    let yaw = siny_cosp.atan2(cosy_cosp);

    (roll, pitch, yaw)
}

// ===========================================================================
// Camera + Odom — tests
// ===========================================================================

#[cfg(test)]
mod camera_tests {
    use super::*;

    fn cfg_2x2_unit01_nchw(encoding: CameraEncoding) -> CameraConfig {
        CameraConfig {
            encoding,
            target_height: 2, target_width: 2,
            resize: CameraResize::Nearest,
            normalization: CameraNormalization::Unit01,
            layout: CameraLayout::Nchw,
            tensor_name: "image".to_string(),
        }
    }

    fn get<'a>(batch: &'a TensorBatch<'static>) -> &'a [f32] {
        batch.named_tensors.get("image").expect("tensor present").as_slice()
    }

    /// **Channel-order classic bug.** Same byte sequence interpreted as
    /// `Rgb8` vs `Bgr8` must produce DIFFERENT NCHW outputs — the C
    /// axis swaps so the OUTPUT is always RGB-ordered.
    #[test]
    fn rgb8_vs_bgr8_channel_order_is_correct() {
        // One pixel, R=200 G=100 B=50 in source. As RGB8 the bytes are
        // [200, 100, 50]; as BGR8 the bytes are [50, 100, 200].
        let rgb_bytes = [200u8, 100, 50];
        let bgr_bytes = [50u8,  100, 200];
        // 1×1 source, 1×1 target — no resize needed.
        let cfg_rgb = CameraConfig { target_height: 1, target_width: 1, ..cfg_2x2_unit01_nchw(CameraEncoding::Rgb8) };
        let cfg_bgr = CameraConfig { target_height: 1, target_width: 1, ..cfg_2x2_unit01_nchw(CameraEncoding::Bgr8) };
        let rgb_out = CameraMapping::new(cfg_rgb).to_tensor(&CameraSample {
            bytes: &rgb_bytes, src_width: 1, src_height: 1,
        }).expect("rgb to_tensor");
        let bgr_out = CameraMapping::new(cfg_bgr).to_tensor(&CameraSample {
            bytes: &bgr_bytes, src_width: 1, src_height: 1,
        }).expect("bgr to_tensor");
        // Both outputs should be the SAME normalized values in
        // RGB-channel order: [200/255, 100/255, 50/255].
        let expected = [200.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0];
        for (i, &e) in expected.iter().enumerate() {
            assert!((get(&rgb_out)[i] - e).abs() < 1e-6,
                "Rgb8 channel {i}: expected {e}, got {}", get(&rgb_out)[i]);
            assert!((get(&bgr_out)[i] - e).abs() < 1e-6,
                "Bgr8 channel {i} (after swap to RGB): expected {e}, got {}", get(&bgr_out)[i]);
        }
    }

    /// **NCHW vs NHWC layout.** Same input, different output strides.
    #[test]
    fn nchw_vs_nhwc_layout_is_correct() {
        // 1×2 source (one row, two pixels), Rgb8.
        // Pixel 0: R=10, G=20, B=30; Pixel 1: R=40, G=50, B=60.
        let bytes = [10u8, 20, 30,  40, 50, 60];
        let sample = CameraSample { bytes: &bytes, src_width: 2, src_height: 1 };
        let cfg_nchw = CameraConfig { target_height: 1, target_width: 2, ..cfg_2x2_unit01_nchw(CameraEncoding::Rgb8) };
        let cfg_nhwc = CameraConfig { layout: CameraLayout::Nhwc, ..cfg_nchw.clone() };

        let nchw = CameraMapping::new(cfg_nchw).to_tensor(&sample).expect("nchw");
        let nhwc = CameraMapping::new(cfg_nhwc).to_tensor(&sample).expect("nhwc");

        // NCHW [1, C=3, H=1, W=2] — for a single row, layout is:
        //   [R0, R1, G0, G1, B0, B1]
        let n = 1.0 / 255.0;
        let expected_nchw = [10.0*n, 40.0*n,  20.0*n, 50.0*n,  30.0*n, 60.0*n];
        // NHWC [1, H=1, W=2, C=3] — layout:
        //   [R0, G0, B0, R1, G1, B1]
        let expected_nhwc = [10.0*n, 20.0*n, 30.0*n,  40.0*n, 50.0*n, 60.0*n];

        for (i, &e) in expected_nchw.iter().enumerate() {
            assert!((get(&nchw)[i] - e).abs() < 1e-6, "NCHW[{i}]: {e} vs {}", get(&nchw)[i]);
        }
        for (i, &e) in expected_nhwc.iter().enumerate() {
            assert!((get(&nhwc)[i] - e).abs() < 1e-6, "NHWC[{i}]: {e} vs {}", get(&nhwc)[i]);
        }
    }

    /// `[0,1]` normalization arithmetic.
    #[test]
    fn unit01_normalization_is_exact() {
        let bytes = [0u8, 127, 255];
        let cfg = CameraConfig { target_height: 1, target_width: 3, ..cfg_2x2_unit01_nchw(CameraEncoding::Mono8) };
        let out = CameraMapping::new(cfg).to_tensor(&CameraSample { bytes: &bytes, src_width: 3, src_height: 1 }).expect("mono");
        let s = get(&out);
        assert!((s[0] - 0.0          ).abs() < 1e-6);
        assert!((s[1] - 127.0 / 255.0).abs() < 1e-6);
        assert!((s[2] - 1.0          ).abs() < 1e-6);
    }

    /// `[-1, 1]` normalization arithmetic.
    #[test]
    fn signed_unit_normalization_is_exact() {
        let bytes = [0u8, 127, 255];
        let cfg = CameraConfig {
            target_height: 1, target_width: 3,
            normalization: CameraNormalization::SignedUnit,
            ..cfg_2x2_unit01_nchw(CameraEncoding::Mono8)
        };
        let out = CameraMapping::new(cfg).to_tensor(&CameraSample { bytes: &bytes, src_width: 3, src_height: 1 }).expect("mono");
        let s = get(&out);
        assert!((s[0] - (-1.0)       ).abs() < 1e-6);
        assert!((s[1] - (127.0 / 127.5 - 1.0)).abs() < 1e-6);
        assert!((s[2] - (255.0 / 127.5 - 1.0)).abs() < 1e-6);
    }

    /// ImageNet-style `MeanStd` normalization. Per-channel arithmetic.
    #[test]
    fn meanstd_normalization_is_per_channel() {
        // One pixel, R=255 G=0 B=128.
        let bytes = [255u8, 0, 128];
        let cfg = CameraConfig {
            target_height: 1, target_width: 1,
            normalization: CameraNormalization::MeanStd {
                mean: vec![0.5, 0.5, 0.5],
                std:  vec![0.5, 0.5, 0.5],
            },
            ..cfg_2x2_unit01_nchw(CameraEncoding::Rgb8)
        };
        let out = CameraMapping::new(cfg).to_tensor(&CameraSample { bytes: &bytes, src_width: 1, src_height: 1 }).expect("rgb");
        let s = get(&out);
        // (255/255 - 0.5)/0.5 = 1.0
        // (0/255   - 0.5)/0.5 = -1.0
        // (128/255 - 0.5)/0.5 = (0.502 - 0.5)/0.5 ≈ 0.00392
        assert!((s[0] - 1.0          ).abs() < 1e-5);
        assert!((s[1] - (-1.0)       ).abs() < 1e-5);
        assert!((s[2] - ((128.0/255.0 - 0.5)/0.5)).abs() < 1e-5);
    }

    /// Mono8 → single-channel tensor.
    #[test]
    fn mono8_produces_single_channel_tensor() {
        let bytes = [10u8, 20, 30, 40]; // 2×2 mono
        let cfg = cfg_2x2_unit01_nchw(CameraEncoding::Mono8);
        let out = CameraMapping::new(cfg).to_tensor(&CameraSample { bytes: &bytes, src_width: 2, src_height: 2 }).expect("mono");
        assert_eq!(get(&out).len(), 2 * 2 * 1, "mono8 produces H*W*1 floats");
    }

    /// Resize nearest-neighbour to a larger target.
    #[test]
    fn nearest_resize_to_larger_target_replicates_pixels() {
        // 1×1 source, single grey pixel.
        let bytes = [200u8];
        // Upsample to 2×2.
        let cfg = CameraConfig {
            target_height: 2, target_width: 2,
            ..cfg_2x2_unit01_nchw(CameraEncoding::Mono8)
        };
        let out = CameraMapping::new(cfg).to_tensor(&CameraSample { bytes: &bytes, src_width: 1, src_height: 1 }).expect("upsample");
        let s = get(&out);
        let expected = 200.0 / 255.0;
        // All 4 output pixels must read the same source pixel.
        for &v in s {
            assert!((v - expected).abs() < 1e-6, "expected {expected}, got {v}");
        }
    }

    /// Resize nearest-neighbour to a smaller target.
    #[test]
    fn nearest_resize_to_smaller_target_is_correct_dims() {
        // 4×4 mono, increasing values 0..16.
        let bytes: Vec<u8> = (0u8..16).collect();
        let cfg = CameraConfig {
            target_height: 2, target_width: 2,
            ..cfg_2x2_unit01_nchw(CameraEncoding::Mono8)
        };
        let out = CameraMapping::new(cfg).to_tensor(&CameraSample { bytes: &bytes, src_width: 4, src_height: 4 }).expect("downsample");
        assert_eq!(get(&out).len(), 4, "2×2 mono output has 4 floats");
    }

    /// Malformed input → structured error, no panic.
    #[test]
    fn byte_count_mismatch_returns_structured_error() {
        // claim 2×2 rgb8 (12 bytes) but supply 11.
        let bytes = [0u8; 11];
        let cfg = cfg_2x2_unit01_nchw(CameraEncoding::Rgb8);
        let err = CameraMapping::new(cfg).to_tensor(&CameraSample {
            bytes: &bytes, src_width: 2, src_height: 2,
        }).expect_err("must error");
        assert_eq!(err, CameraMappingError::ByteCountMismatch { expected: 12, got: 11 });
    }

    /// Zero dims → guard against div-by-zero.
    #[test]
    fn zero_source_dims_return_structured_error() {
        let bytes = [];
        let cfg = cfg_2x2_unit01_nchw(CameraEncoding::Rgb8);
        let err = CameraMapping::new(cfg).to_tensor(&CameraSample {
            bytes: &bytes, src_width: 0, src_height: 0,
        }).expect_err("must error");
        assert_eq!(err, CameraMappingError::InvalidDimensions { width: 0, height: 0 });
    }

    /// MeanStd channel-count mismatch surfaces explicitly.
    #[test]
    fn meanstd_channel_mismatch_returns_structured_error() {
        let bytes = [0u8, 0, 0]; // 1×1 rgb8
        let cfg = CameraConfig {
            target_height: 1, target_width: 1,
            normalization: CameraNormalization::MeanStd { mean: vec![0.5, 0.5], std: vec![0.5, 0.5] },
            ..cfg_2x2_unit01_nchw(CameraEncoding::Rgb8)
        };
        let err = CameraMapping::new(cfg).to_tensor(&CameraSample {
            bytes: &bytes, src_width: 1, src_height: 1,
        }).expect_err("must error");
        assert!(matches!(err, CameraMappingError::NormalizationChannelMismatch { .. }));
    }

    /// `std == 0` fails closed with a structured error (the offending channel),
    /// never a non-finite tensor. Hardening finding.
    #[test]
    fn meanstd_zero_std_returns_structured_error() {
        let bytes = [0u8, 0, 0]; // 1×1 rgb8
        let cfg = CameraConfig {
            target_height: 1, target_width: 1,
            normalization: CameraNormalization::MeanStd { mean: vec![0.5; 3], std: vec![0.5, 0.0, 0.5] },
            ..cfg_2x2_unit01_nchw(CameraEncoding::Rgb8)
        };
        let err = CameraMapping::new(cfg).to_tensor(&CameraSample {
            bytes: &bytes, src_width: 1, src_height: 1,
        }).expect_err("std==0 must error");
        assert_eq!(err, CameraMappingError::MeanStdNonFiniteScale { channel: 1 });
    }

    /// Non-finite mean, non-finite std, and negative std each fail closed at
    /// the offending channel.
    #[test]
    fn meanstd_non_finite_or_negative_scale_returns_structured_error() {
        for (mean, std, ch) in [
            (vec![0.5, f32::NAN, 0.5], vec![0.5_f32; 3],            1usize),
            (vec![0.5_f32; 3],        vec![f32::INFINITY, 0.5, 0.5], 0usize),
            (vec![0.5_f32; 3],        vec![0.5, 0.5, -1.0],          2usize),
        ] {
            let bytes = [0u8, 0, 0];
            let cfg = CameraConfig {
                target_height: 1, target_width: 1,
                normalization: CameraNormalization::MeanStd { mean, std },
                ..cfg_2x2_unit01_nchw(CameraEncoding::Rgb8)
            };
            let err = CameraMapping::new(cfg).to_tensor(&CameraSample {
                bytes: &bytes, src_width: 1, src_height: 1,
            }).expect_err("non-finite/negative scale must error");
            assert_eq!(err, CameraMappingError::MeanStdNonFiniteScale { channel: ch });
        }
    }
}

#[cfg(test)]
mod odom_tests {
    use super::*;

    fn all_on(orientation: OdomOrientation) -> OdomConfig {
        OdomConfig {
            include_position: true,
            include_orientation: Some(orientation),
            include_linear_velocity: true,
            include_angular_velocity: true,
            tensor_name: "odom".to_string(),
        }
    }

    fn s_get<'a>(batch: &'a TensorBatch<'static>) -> &'a [f32] {
        batch.named_tensors.get("odom").unwrap().as_slice()
    }

    /// **Quaternion→yaw correctness.** A pure-yaw quaternion of θ rad
    /// around Z is `(0, 0, sin(θ/2), cos(θ/2))`. The mapping must
    /// recover θ.
    #[test]
    fn quaternion_to_yaw_recovers_known_angle() {
        let theta = std::f64::consts::FRAC_PI_4; // 45°
        let half  = theta / 2.0;
        let sample = OdomSample {
            position:         [0.0; 3],
            orientation_xyzw: [0.0, 0.0, half.sin(), half.cos()],
            linear_velocity:  [0.0; 3],
            angular_velocity: [0.0; 3],
        };
        let cfg = OdomConfig {
            include_position: false,
            include_orientation: Some(OdomOrientation::Yaw),
            include_linear_velocity: false,
            include_angular_velocity: false,
            tensor_name: "odom".to_string(),
        };
        let out = OdomMapping::new(cfg).to_tensor(&sample);
        let v = s_get(&out);
        assert_eq!(v.len(), 1);
        assert!((v[0] - theta as f32).abs() < 1e-5,
            "yaw: expected {theta}, got {}", v[0]);
    }

    /// Quaternion→yaw recovers a NEGATIVE angle correctly.
    #[test]
    fn quaternion_to_yaw_handles_negative_angle() {
        let theta = -std::f64::consts::FRAC_PI_3;
        let half  = theta / 2.0;
        let sample = OdomSample {
            position:         [0.0; 3],
            orientation_xyzw: [0.0, 0.0, half.sin(), half.cos()],
            linear_velocity:  [0.0; 3],
            angular_velocity: [0.0; 3],
        };
        let cfg = OdomConfig {
            include_position: false,
            include_orientation: Some(OdomOrientation::Yaw),
            include_linear_velocity: false,
            include_angular_velocity: false,
            tensor_name: "odom".to_string(),
        };
        let out = OdomMapping::new(cfg).to_tensor(&sample);
        let v = s_get(&out);
        assert!((v[0] - theta as f32).abs() < 1e-5);
    }

    /// All-fields-on layout: position(3) + yaw(1) + linvel(3) + angvel(3) = 10.
    #[test]
    fn yaw_default_all_fields_layout_is_documented_order() {
        let sample = OdomSample {
            position:         [1.0, 2.0, 3.0],
            orientation_xyzw: [0.0, 0.0, 0.0, 1.0], // identity → yaw=0
            linear_velocity:  [4.0, 5.0, 6.0],
            angular_velocity: [7.0, 8.0, 9.0],
        };
        let out = OdomMapping::new(all_on(OdomOrientation::Yaw)).to_tensor(&sample);
        let v = s_get(&out);
        assert_eq!(v.len(), 10);
        assert_eq!(v, &[1.0, 2.0, 3.0, /*yaw*/ 0.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    }

    /// Field selection — disabling toggles shortens the vector.
    #[test]
    fn field_selection_changes_vector_length() {
        let sample = OdomSample {
            position:         [1.0, 2.0, 3.0],
            orientation_xyzw: [0.0, 0.0, 0.0, 1.0],
            linear_velocity:  [4.0, 5.0, 6.0],
            angular_velocity: [7.0, 8.0, 9.0],
        };
        // Position + linear velocity ONLY.
        let cfg = OdomConfig {
            include_position: true,
            include_orientation: None,
            include_linear_velocity: true,
            include_angular_velocity: false,
            tensor_name: "odom".to_string(),
        };
        assert_eq!(cfg.vector_len(), 6);
        let out = OdomMapping::new(cfg).to_tensor(&sample);
        let v = s_get(&out);
        assert_eq!(v.len(), 6);
        assert_eq!(v, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    /// FullEuler representation — 3 floats.
    #[test]
    fn full_euler_produces_three_floats() {
        let sample = OdomSample {
            position:         [0.0; 3],
            orientation_xyzw: [0.0, 0.0, 0.0, 1.0],
            linear_velocity:  [0.0; 3],
            angular_velocity: [0.0; 3],
        };
        let cfg = OdomConfig {
            include_position: false,
            include_orientation: Some(OdomOrientation::FullEuler),
            include_linear_velocity: false,
            include_angular_velocity: false,
            tensor_name: "odom".to_string(),
        };
        assert_eq!(cfg.vector_len(), 3);
        let out = OdomMapping::new(cfg).to_tensor(&sample);
        let v = s_get(&out);
        assert_eq!(v.len(), 3);
        assert_eq!(v, &[0.0, 0.0, 0.0]); // identity quaternion
    }

    /// Quaternion representation — 4 floats, raw.
    #[test]
    fn raw_quaternion_passthrough() {
        let sample = OdomSample {
            position:         [0.0; 3],
            orientation_xyzw: [0.1, 0.2, 0.3, 0.4],
            linear_velocity:  [0.0; 3],
            angular_velocity: [0.0; 3],
        };
        let cfg = OdomConfig {
            include_position: false,
            include_orientation: Some(OdomOrientation::Quaternion),
            include_linear_velocity: false,
            include_angular_velocity: false,
            tensor_name: "odom".to_string(),
        };
        let out = OdomMapping::new(cfg).to_tensor(&sample);
        let v = s_get(&out);
        assert_eq!(v, &[0.1_f32, 0.2, 0.3, 0.4]);
    }
}

// ===========================================================================
// PROPERTY TESTS — sensor mapping invariants (quality-hardening pass)
//
// Each property states the invariant + its source. The mappings are the
// untrusted-input boundary (raw sensor bytes → model tensor); a violated
// range/shape invariant is a real safety risk (an out-of-contract tensor fed
// to the policy). These are written to assert REAL behavior, not to chase a
// mutation score.
// ===========================================================================
#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    fn encoding_strategy() -> impl Strategy<Value = CameraEncoding> {
        prop_oneof![
            Just(CameraEncoding::Rgb8),
            Just(CameraEncoding::Bgr8),
            Just(CameraEncoding::Mono8),
        ]
    }

    /// Build a config + a correctly-sized random byte buffer for the chosen
    /// encoding and source dims. Keeps dims small so the property runs fast.
    fn camera_case() -> impl Strategy<Value = (CameraConfig, Vec<u8>, u32, u32)> {
        (encoding_strategy(), 1u32..6, 1u32..6, 1u32..6, 1u32..6).prop_flat_map(
            |(enc, sw, sh, tw, th)| {
                let ch = enc.channels();
                let nbytes = (sw as usize) * (sh as usize) * ch;
                proptest::collection::vec(any::<u8>(), nbytes).prop_map(move |bytes| {
                    let cfg = CameraConfig {
                        encoding: enc,
                        target_height: th,
                        target_width: tw,
                        resize: CameraResize::Nearest,
                        normalization: CameraNormalization::Unit01,
                        layout: CameraLayout::Nchw,
                        tensor_name: "image".to_string(),
                    };
                    (cfg, bytes, sw, sh)
                })
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1500))]

        /// INVARIANT: Unit01 normalization output is ALWAYS in [0, 1].
        /// SOURCE: sensor_mapping.rs — `raw / 255.0`, raw ∈ [0, 255].
        #[test]
        fn prop_unit01_output_in_0_1((cfg, bytes, sw, sh) in camera_case()) {
            let cfg = CameraConfig { normalization: CameraNormalization::Unit01, ..cfg };
            let out = CameraMapping::new(cfg)
                .to_tensor(&CameraSample { bytes: &bytes, src_width: sw, src_height: sh })
                .expect("valid sized input must map");
            for &x in out.named_tensors.get("image").unwrap().as_slice() {
                prop_assert!((0.0..=1.0).contains(&x), "Unit01 out-of-range: {x}");
            }
        }

        /// INVARIANT: SignedUnit normalization output is ALWAYS in [-1, 1].
        /// SOURCE: `raw / 127.5 - 1.0`; raw ∈ [0,255] ⇒ [-1.0, 1.0].
        #[test]
        fn prop_signedunit_output_in_pm1((cfg, bytes, sw, sh) in camera_case()) {
            let cfg = CameraConfig { normalization: CameraNormalization::SignedUnit, ..cfg };
            let out = CameraMapping::new(cfg)
                .to_tensor(&CameraSample { bytes: &bytes, src_width: sw, src_height: sh })
                .expect("valid sized input must map");
            for &x in out.named_tensors.get("image").unwrap().as_slice() {
                prop_assert!((-1.0..=1.0).contains(&x), "SignedUnit out-of-range: {x}");
            }
        }

        /// INVARIANT: MeanStd output is finite for finite mean and std > 0.
        /// SOURCE: `(raw/255 - mean)/std`; finite ÷ nonzero-finite is finite.
        /// (std = 0 is an integrator misconfiguration — see the finding note
        /// in the hardening report; this property is the valid-domain claim.)
        #[test]
        fn prop_meanstd_output_finite_for_positive_std(
            (cfg, bytes, sw, sh) in camera_case(),
            mean in -5.0_f32..5.0,
            std in 0.05_f32..5.0,
        ) {
            let ch = cfg.encoding.channels();
            let cfg = CameraConfig {
                normalization: CameraNormalization::MeanStd {
                    mean: vec![mean; ch], std: vec![std; ch],
                },
                ..cfg
            };
            let out = CameraMapping::new(cfg)
                .to_tensor(&CameraSample { bytes: &bytes, src_width: sw, src_height: sh })
                .expect("valid sized input must map");
            for &x in out.named_tensors.get("image").unwrap().as_slice() {
                prop_assert!(x.is_finite(), "MeanStd produced non-finite {x} (mean={mean}, std={std})");
            }
        }

        /// INVARIANT (complement of the above + the std-guard finding fix):
        /// for ARBITRARY mean/std — including std ∈ {0, negative} and
        /// non-finite mean/std — `to_tensor` NEVER returns a non-finite
        /// tensor. It either rejects fail-closed with `MeanStdNonFiniteScale`
        /// or returns all-finite output. This locks in the up-front scale
        /// guard so the "MeanStd → finite" invariant holds on the FULL domain,
        /// not just std > 0.
        #[test]
        fn prop_meanstd_never_emits_non_finite(
            (cfg, bytes, sw, sh) in camera_case(),
            mean in prop_oneof![Just(f32::NAN), Just(f32::INFINITY), Just(f32::NEG_INFINITY), -5.0_f32..5.0],
            std  in prop_oneof![Just(0.0_f32), Just(-1.0_f32), Just(f32::NAN), Just(f32::INFINITY), 0.05_f32..5.0],
        ) {
            let ch = cfg.encoding.channels();
            let cfg = CameraConfig {
                normalization: CameraNormalization::MeanStd { mean: vec![mean; ch], std: vec![std; ch] },
                ..cfg
            };
            match CameraMapping::new(cfg)
                .to_tensor(&CameraSample { bytes: &bytes, src_width: sw, src_height: sh })
            {
                Ok(out) => {
                    for &x in out.named_tensors.get("image").unwrap().as_slice() {
                        prop_assert!(x.is_finite(), "emitted non-finite {x} for mean={mean} std={std}");
                    }
                }
                Err(e) => prop_assert!(
                    matches!(e, CameraMappingError::MeanStdNonFiniteScale { .. }),
                    "unexpected error variant for mean={mean} std={std}: {e:?}",
                ),
            }
        }

        /// INVARIANT: resize yields EXACTLY target_height × target_width ×
        /// channels elements. SOURCE: `out = vec![0.0; dst_h*dst_w*channels]`.
        /// A wrong-length tensor breaks the model input contract.
        #[test]
        fn prop_output_length_matches_target_dims((cfg, bytes, sw, sh) in camera_case()) {
            let expect = (cfg.target_height as usize)
                * (cfg.target_width as usize)
                * cfg.encoding.channels();
            let out = CameraMapping::new(cfg)
                .to_tensor(&CameraSample { bytes: &bytes, src_width: sw, src_height: sh })
                .expect("valid sized input must map");
            prop_assert_eq!(out.named_tensors.get("image").unwrap().as_slice().len(), expect);
        }

        /// INVARIANT: the rgb8↔bgr8 reorder is a TRUE PERMUTATION — the same
        /// physical pixel (R,G,B) maps to the SAME RGB-ordered output whether
        /// the source is declared Rgb8 (bytes R,G,B) or Bgr8 (bytes B,G,R).
        /// No channel is lost or duplicated. SOURCE: `dst_c = channels-1-c`
        /// for Bgr8 is the reverse permutation on 3 channels.
        #[test]
        fn prop_rgb8_bgr8_is_permutation(r in any::<u8>(), g in any::<u8>(), b in any::<u8>()) {
            let base = CameraConfig {
                encoding: CameraEncoding::Rgb8,
                target_height: 1, target_width: 1,
                resize: CameraResize::Nearest,
                normalization: CameraNormalization::Unit01,
                layout: CameraLayout::Nchw,
                tensor_name: "image".to_string(),
            };
            let rgb = CameraMapping::new(base.clone())
                .to_tensor(&CameraSample { bytes: &[r, g, b], src_width: 1, src_height: 1 })
                .unwrap();
            let bgr = CameraMapping::new(CameraConfig { encoding: CameraEncoding::Bgr8, ..base })
                .to_tensor(&CameraSample { bytes: &[b, g, r], src_width: 1, src_height: 1 })
                .unwrap();
            // Same RGB-ordered output for both source orderings.
            prop_assert_eq!(
                rgb.named_tensors.get("image").unwrap().as_slice(),
                bgr.named_tensors.get("image").unwrap().as_slice()
            );
            // And it is exactly [R,G,B]/255 — no channel dropped/duplicated.
            let got = rgb.named_tensors.get("image").unwrap().as_slice();
            prop_assert_eq!(got, &[r as f32/255.0, g as f32/255.0, b as f32/255.0]);
        }

        /// INVARIANT: quaternion→yaw is finite and within atan2's range
        /// [-π, π]. SOURCE: `yaw = siny_cosp.atan2(cosy_cosp)` (Tait–Bryan ZYX).
        /// NOTE: atan2's range is [-π, π]; the spec's "(-π, π]" is the typical
        /// non-boundary case — the boundary value -π is a legal atan2 output,
        /// so the asserted invariant is the mathematically-exact closed range.
        #[test]
        fn prop_quat_to_yaw_in_range(
            qx in -1.0_f64..1.0, qy in -1.0_f64..1.0,
            qz in -1.0_f64..1.0, qw in -1.0_f64..1.0,
        ) {
            // Skip the degenerate zero quaternion (not a rotation).
            let norm = (qx*qx + qy*qy + qz*qz + qw*qw).sqrt();
            prop_assume!(norm > 1e-6);
            let (nx, ny, nz, nw) = (qx/norm, qy/norm, qz/norm, qw/norm);
            let (_roll, _pitch, yaw) = quat_to_euler(nx, ny, nz, nw);
            prop_assert!(yaw.is_finite(), "yaw must be finite, got {yaw}");
            prop_assert!(
                (-std::f64::consts::PI..=std::f64::consts::PI).contains(&yaw),
                "yaw {yaw} outside [-π, π]"
            );
        }
    }
}
