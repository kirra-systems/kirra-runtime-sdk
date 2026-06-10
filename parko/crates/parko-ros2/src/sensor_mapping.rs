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
    /// Unit-norm tolerance for the orientation quaternion. The orientation
    /// quaternion is REJECTED (fail-closed, never re-normalized) when
    /// `|‖q‖ - 1|` exceeds this. Mirrors `ImuConfig::quat_norm_tolerance` — the
    /// same fail-closed unit-norm convention, as a per-sensor field. Validated
    /// finite and `>= 0` at transform entry (a negative/non-finite value is
    /// itself rejected, mirroring IMU's `InvalidQuatTolerance`).
    pub quat_norm_tolerance: f32,
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

/// Fail-closed rejection reasons for `OdomMapping::to_tensor`. Disjoint from the
/// other sensors' error enums (Imu/Lidar/Radar/Camera), mirroring their style.
/// Before this guard odom was infallible and fed `quat_to_euler` unvalidated,
/// producing silent garbage Euler on a non-unit/near-zero/non-finite quaternion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OdomMappingError {
    /// `quat_norm_tolerance` is negative or non-finite (mirrors IMU).
    InvalidQuatTolerance,
    /// A non-finite (`NaN`/`inf`) orientation quaternion component — rejected so
    /// it can't reach `quat_to_euler`.
    NonFiniteQuaternion,
    /// The orientation quaternion is not unit-norm within `quat_norm_tolerance`.
    /// Rejected, never silently re-normalized (that would mask an upstream
    /// sensor/frame fault); fail-closed surfaces it. Mirrors IMU's guard.
    NonUnitQuaternion,
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

    /// The pure transform. Fail-closed: when an orientation feature is
    /// requested, the orientation quaternion is validated for finiteness and
    /// unit-norm at THIS entry (one shared guard covering both the FullEuler and
    /// Yaw `quat_to_euler` call sites) and REJECTED — never re-normalized, never
    /// fed to `quat_to_euler` to produce silent garbage Euler.
    pub fn to_tensor(&self, sample: &OdomSample) -> Result<TensorBatch<'static>, OdomMappingError> {
        // Config: the tolerance itself must be finite and non-negative (mirrors
        // ImuMapping's InvalidQuatTolerance gate).
        if !self.config.quat_norm_tolerance.is_finite() || self.config.quat_norm_tolerance < 0.0 {
            return Err(OdomMappingError::InvalidQuatTolerance);
        }

        // Orientation quaternion guard — only when an orientation feature is
        // requested (it is the only consumer of the quaternion). REJECT a
        // non-finite or non-unit quaternion before any quat_to_euler call.
        if self.config.include_orientation.is_some() {
            let [qx, qy, qz, qw] = sample.orientation_xyzw;
            if !(qx.is_finite() && qy.is_finite() && qz.is_finite() && qw.is_finite()) {
                return Err(OdomMappingError::NonFiniteQuaternion);
            }
            let norm = (qx * qx + qy * qy + qz * qz + qw * qw).sqrt();
            if (norm - 1.0).abs() > self.config.quat_norm_tolerance as f64 {
                return Err(OdomMappingError::NonUnitQuaternion);
            }
        }

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
        Ok(TensorBatch { named_tensors: named, metadata: HashMap::new() })
    }
}

impl SensorInputMapping for OdomMapping {
    type Sample = OdomSample;

    fn to_frame(&self, frame_id: u64, timestamp_ms: u64, sample: &OdomSample) -> SensorFrame {
        // The trait can't surface errors. On a rejected quaternion (non-unit /
        // non-finite), emit a structured log + a zero tensor; the tick
        // pipeline's staleness/governor MRC path catches the downstream
        // consequence. Mirrors CameraMapping::to_frame — fail-closed surfaces
        // the fault rather than feeding garbage Euler forward.
        match self.to_tensor(sample) {
            Ok(batch) => SensorFrame { frame_id, timestamp_ms, payload: batch },
            Err(err) => {
                tracing::error!(
                    ?err, frame_id, timestamp_ms,
                    "OdomMapping::to_frame received an invalid orientation quaternion; \
                     emitting zero tensor (downstream MRC will fire)"
                );
                let mut named = HashMap::new();
                named.insert(
                    self.config.tensor_name.clone(),
                    TensorStorage::Owned(vec![0.0_f32; self.config.vector_len()]),
                );
                SensorFrame {
                    frame_id, timestamp_ms,
                    payload: TensorBatch { named_tensors: named, metadata: HashMap::new() },
                }
            }
        }
    }
}

/// ROS quaternion `(x, y, z, w)` → Euler `(roll, pitch, yaw)` in radians.
/// Tait–Bryan ZYX intrinsic convention (yaw about Z, then pitch about Y,
/// then roll about X). The same convention `kirra-ros2-adapter::geometry::quat_to_yaw`
/// uses, so adapter + parko-ros2 agree on what "yaw" means.
///
/// PRECONDITION — UNIT NORM. The formulas (`1 - 2(qx²+qy²)`, …) assume a unit
/// quaternion; a non-unit or near-zero input yields silently-wrong (often
/// out-of-range) Euler. This is a pure shared helper and does NOT validate —
/// callers MUST reject non-unit input first. Both consumers do: `ImuMapping`
/// (norm vs `quat_norm_tolerance`) and `OdomMapping` (the entry guard returning
/// `OdomMappingError::NonUnitQuaternion`).
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
// LiDAR mapping  (point cloud → BEV grid tensor)
// ===========================================================================
//
// SAFETY FRAMING. Sensor mapping is UPSTREAM of the governor's guarantee. The
// governor bounds the OUTPUT command but cannot detect a wrong-but-in-bounds
// command produced from mis-mapped input: a LiDAR mapping that mis-places,
// drops, or mis-frames points feeds the model corrupted spatial geometry, and
// the resulting command can be confidently wrong AND within the envelope the
// governor passes. So this transform's contract is correctness —
// deterministic, frame-correct, fail-closed on malformed input — not
// convenience.
//
// REPRESENTATION (FLAGGED — architect's call; see the PR/report). A cloud can
// be mapped to several model-input representations; the right one depends on
// the perception model that consumes it:
//   - BEV grid  — occupancy / max-height / density / intensity channels over
//     an X–Y grid; the PointPillars/CenterPoint family input class.
//   - Range image (spherical projection: azimuth × elevation) — RangeNet /
//     SalsaNext class.
//   - Point list (N×[x,y,z,intensity], sampled/padded) — PointNet class.
// BEV is IMPLEMENTED as a reasonable DEFAULT (the most common AV 3D-detection
// input, fully deterministic) — but treat it as a PLACEHOLDER pending Parko's
// actual model. This is the PARKO path: Parko runs its OWN perception model
// here; this mapping is NOT shared with Occy's Autoware detector, so "Autoware
// uses BEV" is explicitly NOT the rationale. When Parko's model is chosen,
// RE-CONFIRM it wants BEV rather than a range image or point list. The choice
// is the architect's; the config makes it explicit, and the enum + exhaustive
// `to_tensor` match make switching to a sibling representation clean.
// Only `BevGrid` is implemented; `RangeImage` / `PointList` are the documented
// sibling variants + transforms to add then.
//
// COORDINATE FRAME (FLAGGED). This pure transform ASSUMES the input cloud is
// ALREADY in the model's target frame (ego / base_link), in metres. It does
// NOT apply an extrinsic. A wrong sensor→ego transform places obstacles in the
// wrong location undetectably, so the extrinsic is a SEPARATE, explicit
// concern handled upstream (the deferred ROS shim or a dedicated transform
// stage) — never buried in this mapping.
//
// AXIS CONVENTION (EXPLICIT — spatial meaning must not be implicit). The BEV
// grid is row-major, `[rows(H) × cols(W)]`:
//   col = floor((x − x_min) / resolution)   — increases with +x
//   row = floor((y − y_min) / resolution)   — increases with +y
// so cell (row 0, col 0) is the (x_min, y_min) corner. The integrator MUST
// reconcile this with their model's expected BEV axis convention (some models
// flip Y); it is documented, not assumed.
//
// ROS SHIM — DEFERRED. Parsing `sensor_msgs/PointCloud2` (the binary blob, via
// per-field offsets/datatypes) into `Vec<LidarPoint>`, and the
// `SensorInputMapping` trait impl that drives it, are the ros2-gated
// integration layer — exactly like the camera/odom shims. PLANNED, not
// implemented here.

/// A single LiDAR return, in the model's TARGET frame (see the frame
/// assumption above), metres + raw intensity. NOT the ROS message — that is
/// the deferred shim's input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LidarPoint {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub intensity: f32,
}

/// Which model-input representation the cloud is mapped to. Only `BevGrid` is
/// implemented; `RangeImage` / `PointList` are the flagged alternatives (see
/// the module docs), added as sibling variants + transforms when needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LidarRepresentation {
    /// Bird's-eye-view feature grid over the X–Y plane.
    BevGrid,
}

/// One BEV feature channel. The OUTPUT channel order is exactly the order of
/// `LidarConfig.channels` — no implicit reordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BevChannel {
    /// `1.0` if the cell holds ≥1 in-ROI point, else `0.0`.
    Occupancy,
    /// Maximum point height (z) among the cell's points. Empty cell → `0.0`.
    MaxHeight,
    /// Number of in-ROI points in the cell.
    Density,
    /// Mean intensity of the cell's points. Empty cell → `0.0`.
    MeanIntensity,
}

/// How BEV channel values are scaled — explicit so a channel's numeric meaning
/// never changes silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BevNormalization {
    /// Physical units: `MaxHeight` in metres, `Density` a raw count,
    /// `MeanIntensity` in raw intensity units, `Occupancy` 0/1.
    Raw,
    /// Scaled toward `[0,1]` using the configured ranges:
    /// `MaxHeight → clamp((z−z_min)/(z_max−z_min),0,1)`,
    /// `Density → min(count/density_norm,1)`,
    /// `MeanIntensity → min(intensity/intensity_max,1)`, `Occupancy → 0/1`.
    Normalized,
}

/// Policy for points OUTSIDE the configured ROI
/// (`[x_min,x_max) × [y_min,y_max) × [z_min,z_max]`).
///
/// There is deliberately NO "clip to bounds" option: clamping an out-of-ROI
/// point to the nearest edge cell FABRICATES a false obstacle at the ROI
/// boundary — precisely the wrong-but-in-bounds geometry the governor cannot
/// catch. Out-of-ROI points are dropped or rejected, never relocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutOfBoundsPolicy {
    /// Exclude out-of-ROI points. NORMAL for BEV (a 360° scan sees far beyond
    /// the grid). Chosen deliberately via config so the data loss is
    /// intentional, not accidental.
    Drop,
    /// Any out-of-ROI point is a structured error (`OutOfRoiPoint`). Use when
    /// the input is expected pre-cropped to the ROI, so an out-of-ROI point
    /// signals a frame/extrinsic bug rather than a normal far return.
    Error,
}

/// BEV LiDAR mapping configuration. Everything that affects spatial meaning is
/// explicit; no hidden default can silently move, drop, or rescale a point.
#[derive(Debug, Clone)]
pub struct LidarConfig {
    /// Output representation. `BevGrid` is the only implemented value.
    pub representation: LidarRepresentation,
    /// ROI X extent in metres, `[x_min, x_max)`. Maps to grid COLUMNS (width).
    pub x_min: f32,
    pub x_max: f32,
    /// ROI Y extent in metres, `[y_min, y_max)`. Maps to grid ROWS (height).
    pub y_min: f32,
    pub y_max: f32,
    /// ROI Z (height) extent in metres, `[z_min, z_max]` inclusive. Points
    /// outside are out-of-ROI (the height-of-interest filter).
    pub z_min: f32,
    pub z_max: f32,
    /// Square cell size, metres per cell. Must be finite and `> 0`. Each of the
    /// X and Y extents must be an integer multiple of this (validated) so every
    /// in-`[min,max)` coordinate maps to exactly one valid cell.
    pub resolution_m: f32,
    /// Ordered BEV channels → output channel dimension `C`. Must be non-empty.
    pub channels: Vec<BevChannel>,
    /// Channel value scaling. See `BevNormalization`.
    pub normalization: BevNormalization,
    /// Under `Normalized`, the per-cell point count that maps `Density` to
    /// `1.0`. Must be finite and `> 0` when `Normalized` is used.
    pub density_norm: f32,
    /// Under `Normalized`, the intensity that maps `MeanIntensity` to `1.0`.
    /// Must be finite and `> 0` when `Normalized` is used.
    pub intensity_max: f32,
    /// Output tensor layout (NCHW = `[C,H,W]`, NHWC = `[H,W,C]`). Reuses the
    /// camera layout enum.
    pub layout: CameraLayout,
    /// How to handle points outside the ROI. See `OutOfBoundsPolicy`.
    pub out_of_bounds: OutOfBoundsPolicy,
    /// Tensor name inside the produced `TensorBatch`. Must match the model's
    /// input-node name.
    pub tensor_name: String,
}

/// Errors the pure LiDAR transform may return. Mirrors `CameraMappingError`'s
/// fail-closed discipline (structured, comparable, returned by the pure
/// transform so direct callers can assert refusal). A dedicated enum rather
/// than overloading the camera type — the variants are LiDAR-specific and the
/// "Camera" name would not fit them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LidarMappingError {
    /// `resolution_m` is non-finite or `<= 0`.
    InvalidResolution,
    /// A bound is non-finite, or an extent is inverted/zero
    /// (`x_max <= x_min`, `y_max <= y_min`, or `z_max <= z_min`).
    InvalidBounds,
    /// The X or Y extent is not an integer multiple of `resolution_m`, so the
    /// grid would not tile the ROI exactly. Rejected to keep cell assignment
    /// exact and total (no fractional edge cell silently dropping points).
    GridExtentNotDivisible,
    /// `channels` is empty — the output would have zero channels.
    EmptyChannelSet,
    /// `Normalized` selected with a non-finite or `<= 0` scale
    /// (`density_norm` / `intensity_max`).
    InvalidNormalizationScale,
    /// The cloud is empty. Returned rather than emitting a silently-zero grid.
    EmptyCloud,
    /// Point `index` has a non-finite (`NaN`/`inf`) coordinate or intensity —
    /// rejected so one bad return can never silently corrupt the grid.
    NonFinitePoint { index: usize },
    /// Point `index` is outside the ROI and `out_of_bounds == Error`.
    OutOfRoiPoint { index: usize },
}

/// Pure LiDAR-cloud → BEV-grid tensor mapping. Cloning is cheap.
#[derive(Debug, Clone)]
pub struct LidarMapping {
    config: LidarConfig,
}

impl LidarMapping {
    #[must_use]
    pub fn new(config: LidarConfig) -> Self {
        Self { config }
    }

    /// Validate the config independently of any cloud and return the derived
    /// grid `(n_cols, n_rows)`. Fail-closed; returns the first structured
    /// violation. Called before any point is processed.
    fn validate_config(&self) -> Result<(usize, usize), LidarMappingError> {
        let c = &self.config;
        if !c.resolution_m.is_finite() || c.resolution_m <= 0.0 {
            return Err(LidarMappingError::InvalidResolution);
        }
        if !c.x_min.is_finite() || !c.x_max.is_finite()
            || !c.y_min.is_finite() || !c.y_max.is_finite()
            || !c.z_min.is_finite() || !c.z_max.is_finite()
            || c.x_max <= c.x_min || c.y_max <= c.y_min || c.z_max <= c.z_min
        {
            return Err(LidarMappingError::InvalidBounds);
        }
        if c.channels.is_empty() {
            return Err(LidarMappingError::EmptyChannelSet);
        }
        if c.normalization == BevNormalization::Normalized
            && (!c.density_norm.is_finite() || c.density_norm <= 0.0
                || !c.intensity_max.is_finite() || c.intensity_max <= 0.0)
        {
            return Err(LidarMappingError::InvalidNormalizationScale);
        }
        // Extents must tile the ROI exactly: (extent / res) must be a whole
        // number, so every in-[min,max) coordinate maps to a valid cell.
        let cols_f = (c.x_max - c.x_min) / c.resolution_m;
        let rows_f = (c.y_max - c.y_min) / c.resolution_m;
        if (cols_f - cols_f.round()).abs() > 1e-4 || (rows_f - rows_f.round()).abs() > 1e-4 {
            return Err(LidarMappingError::GridExtentNotDivisible);
        }
        let (n_cols, n_rows) = (cols_f.round() as usize, rows_f.round() as usize);
        if n_cols == 0 || n_rows == 0 {
            return Err(LidarMappingError::InvalidBounds);
        }
        Ok((n_cols, n_rows))
    }

    /// The pure transform. Same cloud → same tensor, every call, no I/O.
    pub fn to_tensor(
        &self,
        cloud: &[LidarPoint],
    ) -> Result<TensorBatch<'static>, LidarMappingError> {
        let c = &self.config;
        // Exhaustive so a future representation can't compile until its
        // transform is implemented (the config stays explicit, never silent).
        match c.representation {
            LidarRepresentation::BevGrid => {}
        }
        let (n_cols, n_rows) = self.validate_config()?;

        if cloud.is_empty() {
            return Err(LidarMappingError::EmptyCloud);
        }

        let n_cells = n_cols * n_rows;
        // Per-cell accumulators.
        let mut count = vec![0u32; n_cells];
        let mut max_z = vec![f32::NEG_INFINITY; n_cells];
        let mut sum_intensity = vec![0.0_f32; n_cells];

        for (idx, p) in cloud.iter().enumerate() {
            if !p.x.is_finite() || !p.y.is_finite() || !p.z.is_finite() || !p.intensity.is_finite()
            {
                return Err(LidarMappingError::NonFinitePoint { index: idx });
            }
            let in_roi = p.x >= c.x_min && p.x < c.x_max
                && p.y >= c.y_min && p.y < c.y_max
                && p.z >= c.z_min && p.z <= c.z_max;
            // Cell indices for in-ROI points; the divisibility guarantee keeps
            // these in range. The `>= n_*` guard is defensive against float
            // edges so we NEVER write out of bounds — treat as out-of-ROI.
            let col = ((p.x - c.x_min) / c.resolution_m).floor();
            let row = ((p.y - c.y_min) / c.resolution_m).floor();
            let in_grid = in_roi
                && col >= 0.0
                && row >= 0.0
                && (col as usize) < n_cols
                && (row as usize) < n_rows;
            if !in_grid {
                match c.out_of_bounds {
                    OutOfBoundsPolicy::Drop => continue,
                    OutOfBoundsPolicy::Error => {
                        return Err(LidarMappingError::OutOfRoiPoint { index: idx });
                    }
                }
            }
            let cell = (row as usize) * n_cols + (col as usize);
            count[cell] += 1;
            if p.z > max_z[cell] {
                max_z[cell] = p.z;
            }
            sum_intensity[cell] += p.intensity;
        }

        // Assemble the output tensor in the configured layout.
        let n_ch = c.channels.len();
        let mut out = vec![0.0_f32; n_ch * n_cells];
        for (ci, ch) in c.channels.iter().enumerate() {
            for row in 0..n_rows {
                for col in 0..n_cols {
                    let cell = row * n_cols + col;
                    let n = count[cell];
                    let value = match ch {
                        BevChannel::Occupancy => {
                            if n > 0 { 1.0 } else { 0.0 }
                        }
                        BevChannel::MaxHeight => {
                            if n == 0 {
                                0.0
                            } else {
                                match c.normalization {
                                    BevNormalization::Raw => max_z[cell],
                                    BevNormalization::Normalized => {
                                        ((max_z[cell] - c.z_min) / (c.z_max - c.z_min))
                                            .clamp(0.0, 1.0)
                                    }
                                }
                            }
                        }
                        BevChannel::Density => match c.normalization {
                            BevNormalization::Raw => n as f32,
                            BevNormalization::Normalized => (n as f32 / c.density_norm).min(1.0),
                        },
                        BevChannel::MeanIntensity => {
                            if n == 0 {
                                0.0
                            } else {
                                let mean = sum_intensity[cell] / n as f32;
                                match c.normalization {
                                    BevNormalization::Raw => mean,
                                    BevNormalization::Normalized => {
                                        (mean / c.intensity_max).min(1.0)
                                    }
                                }
                            }
                        }
                    };
                    let out_idx = match c.layout {
                        CameraLayout::Nchw => ci * n_cells + row * n_cols + col,
                        CameraLayout::Nhwc => row * n_cols * n_ch + col * n_ch + ci,
                    };
                    out[out_idx] = value;
                }
            }
        }

        let mut named = HashMap::new();
        named.insert(c.tensor_name.clone(), TensorStorage::Owned(out));
        Ok(TensorBatch { named_tensors: named, metadata: HashMap::new() })
    }
}

// ===========================================================================
// Radar mapping  (sparse polar detections → detection-list tensor)
// ===========================================================================
//
// SAFETY FRAMING. Sensor mapping is UPSTREAM of the governor's guarantee. A
// radar mapping that mis-places a detection (bad polar→cartesian), drops or
// corrupts its Doppler velocity, or fabricates a phantom return feeds Parko's
// model corrupted spatial/velocity data — producing a command that is
// confidently wrong AND within the envelope the governor passes. Correctness —
// deterministic, geometrically correct, fail-closed — is the whole point.
//
// HOW RADAR DIFFERS FROM LIDAR (each a correctness surface):
//   1. Sparse DETECTIONS, not a dense cloud — a list, each with range, az,
//      optional elevation, radial (Doppler) velocity, RCS.
//   2. Native POLAR — any cartesian output needs an explicit polar→cartesian
//      conversion, the new place geometry errors hide.
//   3. DOPPLER (radial velocity) — radar's most valuable signal, which LiDAR
//      lacks. It MUST be preserved through the mapping, never discarded.
//   4. Many automotive radars are 2D (azimuth only, no elevation).
//
// REPRESENTATION (FLAGGED — architect's call; reversible pending Parko's
// model, exactly like the LiDAR mapping). This is the PARKO path (Parko's own
// model), NOT shared with Occy.
//   - Detection list (N × features, padded to fixed N) — radar-native;
//     preserves Doppler/RCS per detection with NO discretization loss; the
//     simplest. Features are polar `[range, az, el, velocity, rcs]` or
//     cartesian `[x, y, z, velocity, rcs]`. IMPLEMENTED here (recommended
//     default: lossless on Doppler/RCS, no grid discretization).
//   - BEV grid (occupancy / radial-velocity / RCS / density) — consistent with
//     the LiDAR BEV IF Parko fuses sensors into a common BEV, but sparse radar
//     in a grid is mostly empty and discretizes per-detection precision.
//     Documented sibling `RadarRepresentation` variant to add then.
// Re-confirm against Parko's actual model; the enum + exhaustive `to_tensor`
// match keep switching clean.
//
// POLAR→CARTESIAN + ANGLE CONVENTION (FLAGGED — the radar analog of LiDAR's
// frame concern). For the cartesian feature frame the conversion is
//     x = range · cos(el) · cos(az)
//     y = range · cos(el) · sin(az)
//     z = range · sin(el)
// with these conventions, stated LOUDLY because a wrong one silently
// mis-places EVERY detection:
//   - azimuth: radians, zero-reference along +x, CCW positive (toward +y).
//   - elevation: radians, zero in the x–y plane, positive toward +z.
// In the polar feature frame there is NO conversion — the model then receives
// polar features `[range, az, el, velocity, rcs]` verbatim.
//
// 2D RADAR / MISSING ELEVATION (FLAGGED). A detection with `elevation == None`
// (2D radar) is handled per `ElevationPolicy`: `Assume(angle_rad)` substitutes
// an EXPLICIT configured elevation (e.g. 0.0 = the sensor's horizontal plane —
// never a silent z=0), or `Reject` fails closed. The value is always explicit.
//
// ROS SHIM — DEFERRED. Parsing `radar_msgs/RadarScan` / `RadarTracks` into
// `Vec<RadarDetection>`, and the `SensorInputMapping` trait impl, are the
// ros2-gated integration layer — like the lidar/camera shims. PLANNED, not
// implemented here.

/// A single radar detection in the radar's native polar frame. NOT the ROS
/// message — that is the deferred shim's input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadarDetection {
    /// Range to the detection, metres (`>= 0`).
    pub range: f32,
    /// Azimuth, radians: zero along +x, CCW positive (see module docs).
    pub azimuth: f32,
    /// Elevation, radians: zero in the x–y plane, positive toward +z. `None`
    /// for a 2D radar — resolved via `RadarConfig.elevation_policy`.
    pub elevation: Option<f32>,
    /// Radial (Doppler) velocity, m/s. Radar's key signal — preserved into the
    /// output verbatim, never discarded. Producer's sign convention carried
    /// through unchanged.
    pub velocity: f32,
    /// Radar cross-section / detection amplitude (raw units).
    pub rcs: f32,
}

/// Which model-input representation the detections map to. Only `DetectionList`
/// is implemented; `BevGrid` is the documented sibling (added when Parko fuses
/// to a common BEV).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarRepresentation {
    /// `N × F` detection list (radar-native, lossless on Doppler/RCS).
    DetectionList,
}

/// The per-detection feature frame for `DetectionList`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionFeatureFrame {
    /// `[range, azimuth, elevation, velocity, rcs]` — radar-native polar, no
    /// conversion.
    Polar,
    /// `[x, y, z, velocity, rcs]` — cartesian; applies the documented
    /// polar→cartesian conversion.
    Cartesian,
}

/// How a detection with no elevation (2D radar) is resolved. Explicit — never a
/// silent z=0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ElevationPolicy {
    /// Substitute this elevation angle (radians) — e.g. `0.0` = the sensor's
    /// horizontal plane. The assumed value is explicit in config.
    Assume(f32),
    /// A detection with no elevation is a structured error (`MissingElevation`).
    Reject,
}

/// Per-feature value scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarNormalization {
    /// Physical units: metres / radians / m·s⁻¹ / raw RCS. Doppler appears
    /// verbatim in the velocity column.
    Raw,
    /// Divide by reference scales (no clamp, so magnitudes — including Doppler —
    /// are preserved relative to the scale): lengths → `/range_max`, angles →
    /// `/π`, velocity → `/velocity_max`, rcs → `/rcs_max`.
    Normalized,
}

/// Output tensor layout for the `N × F` detection list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarLayout {
    /// `[N, F]` — one detection per row (row-major).
    DetectionMajor,
    /// `[F, N]` — one feature per row.
    FeatureMajor,
}

/// What to do when more than `max_detections` in-FOV detections are present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Keep the first `max_detections` (input order), drop the excess. A
    /// deliberate, documented capacity choice — not a silent accident.
    DropExcess,
    /// More than `max_detections` is a structured error (`TooManyDetections`) —
    /// fail-closed: do not silently discard real returns.
    Error,
}

/// Radar detection-list mapping configuration. Everything that affects spatial
/// or velocity meaning is explicit; no hidden default can move a detection,
/// drop its Doppler, or rescale it.
#[derive(Debug, Clone)]
pub struct RadarConfig {
    /// Output representation. `DetectionList` is the only implemented value.
    pub representation: RadarRepresentation,
    /// Polar vs cartesian per-detection features. See `DetectionFeatureFrame`.
    pub feature_frame: DetectionFeatureFrame,
    /// Range gate, metres, `[range_min, range_max]`. `range_min >= 0`,
    /// `range_max > range_min`. Outside → out-of-FOV.
    pub range_min: f32,
    pub range_max: f32,
    /// Azimuth FOV, radians, `[az_min, az_max]` (`az_max > az_min`). Outside →
    /// out-of-FOV.
    pub az_min: f32,
    pub az_max: f32,
    /// Elevation FOV, radians, `[el_min, el_max]` (`el_max >= el_min`), applied
    /// to the resolved elevation. Outside → out-of-FOV.
    pub el_min: f32,
    pub el_max: f32,
    /// Fixed output detection count `N` (rows). Fewer → zero-padded rows; more →
    /// per `on_overflow`. Must be `> 0`.
    pub max_detections: usize,
    /// Behaviour when in-FOV detections exceed `max_detections`.
    pub on_overflow: OverflowPolicy,
    /// Resolution of `elevation == None` (2D radar).
    pub elevation_policy: ElevationPolicy,
    /// Feature scaling. See `RadarNormalization`.
    pub normalization: RadarNormalization,
    /// Under `Normalized`, the velocity (m/s) mapping to `1.0`. Finite, `> 0`.
    pub velocity_max: f32,
    /// Under `Normalized`, the RCS mapping to `1.0`. Finite, `> 0`.
    pub rcs_max: f32,
    /// Output tensor layout. See `RadarLayout`.
    pub layout: RadarLayout,
    /// Handling of out-of-range / out-of-FOV detections. Reuses the LiDAR
    /// policy: `Drop` (normal — radar sees beyond the gate) or `Error`. There
    /// is deliberately NO clip — clamping a far detection to the boundary
    /// fabricates a phantom obstacle (same hazard the LiDAR mapping excluded).
    pub out_of_bounds: OutOfBoundsPolicy,
    /// Tensor name inside the produced `TensorBatch`.
    pub tensor_name: String,
}

/// Number of feature columns per detection (both feature frames carry 5:
/// 3 position/polar + velocity + rcs).
const RADAR_FEATURES: usize = 5;

/// Errors the pure radar transform may return. A dedicated sibling enum (like
/// `LidarMappingError`), kept disjoint from the camera/lidar errors — the
/// variants are radar-specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadarMappingError {
    /// `range_min` is negative/non-finite, or `range_max <= range_min`.
    InvalidRangeGate,
    /// Azimuth FOV is non-finite or `az_max <= az_min`.
    InvalidAzimuthFov,
    /// Elevation FOV is non-finite, `el_max < el_min`, or the `Assume`
    /// elevation is non-finite.
    InvalidElevationFov,
    /// `max_detections == 0` — the output would have zero rows.
    InvalidMaxDetections,
    /// `Normalized` selected with a non-finite or `<= 0` scale
    /// (`velocity_max` / `rcs_max`).
    InvalidNormalizationScale,
    /// The detection list is empty. Returned rather than a silently-zero tensor.
    EmptyDetectionList,
    /// Detection `index` has a non-finite (`NaN`/`inf`) field — rejected so one
    /// bad return can never silently corrupt the output.
    NonFiniteDetection { index: usize },
    /// Detection `index` has `elevation == None` and `elevation_policy` is
    /// `Reject`.
    MissingElevation { index: usize },
    /// Detection `index` is outside the range/FOV gate and `out_of_bounds` is
    /// `Error`.
    OutOfFovDetection { index: usize },
    /// More than `max_detections` in-FOV detections and `on_overflow` is
    /// `Error`.
    TooManyDetections { found: usize, max: usize },
}

/// Pure radar-detections → detection-list tensor mapping. Cloning is cheap.
#[derive(Debug, Clone)]
pub struct RadarMapping {
    config: RadarConfig,
}

impl RadarMapping {
    #[must_use]
    pub fn new(config: RadarConfig) -> Self {
        Self { config }
    }

    /// Validate the config independently of any detections. Fail-closed;
    /// returns the first structured violation. Called before any detection is
    /// processed.
    fn validate_config(&self) -> Result<(), RadarMappingError> {
        let c = &self.config;
        if !c.range_min.is_finite() || !c.range_max.is_finite()
            || c.range_min < 0.0 || c.range_max <= c.range_min
        {
            return Err(RadarMappingError::InvalidRangeGate);
        }
        if !c.az_min.is_finite() || !c.az_max.is_finite() || c.az_max <= c.az_min {
            return Err(RadarMappingError::InvalidAzimuthFov);
        }
        if !c.el_min.is_finite() || !c.el_max.is_finite() || c.el_max < c.el_min {
            return Err(RadarMappingError::InvalidElevationFov);
        }
        if let ElevationPolicy::Assume(angle) = c.elevation_policy {
            if !angle.is_finite() {
                return Err(RadarMappingError::InvalidElevationFov);
            }
        }
        if c.max_detections == 0 {
            return Err(RadarMappingError::InvalidMaxDetections);
        }
        if c.normalization == RadarNormalization::Normalized
            && (!c.velocity_max.is_finite() || c.velocity_max <= 0.0
                || !c.rcs_max.is_finite() || c.rcs_max <= 0.0)
        {
            return Err(RadarMappingError::InvalidNormalizationScale);
        }
        Ok(())
    }

    /// Resolve a detection's elevation per the 2D policy.
    fn resolve_elevation(&self, det: &RadarDetection, index: usize) -> Result<f32, RadarMappingError> {
        match det.elevation {
            Some(e) => Ok(e),
            None => match self.config.elevation_policy {
                ElevationPolicy::Assume(angle) => Ok(angle),
                ElevationPolicy::Reject => Err(RadarMappingError::MissingElevation { index }),
            },
        }
    }

    /// The five feature columns for one detection, in the configured frame +
    /// normalization. `[range/x, az/y, el/z, velocity, rcs]`.
    fn features(&self, det: &RadarDetection, elevation: f32) -> [f32; RADAR_FEATURES] {
        let c = &self.config;
        let (f0, f1, f2) = match c.feature_frame {
            DetectionFeatureFrame::Polar => (det.range, det.azimuth, elevation),
            DetectionFeatureFrame::Cartesian => {
                // x = r·cos(el)·cos(az), y = r·cos(el)·sin(az), z = r·sin(el).
                let rc = det.range * elevation.cos();
                (rc * det.azimuth.cos(), rc * det.azimuth.sin(), det.range * elevation.sin())
            }
        };
        match c.normalization {
            RadarNormalization::Raw => [f0, f1, f2, det.velocity, det.rcs],
            RadarNormalization::Normalized => {
                // Lengths → /range_max; angles → /π; velocity → /velocity_max;
                // rcs → /rcs_max. No clamp: Doppler magnitude is preserved.
                let pi = std::f32::consts::PI;
                match c.feature_frame {
                    DetectionFeatureFrame::Polar => [
                        f0 / c.range_max, f1 / pi, f2 / pi,
                        det.velocity / c.velocity_max, det.rcs / c.rcs_max,
                    ],
                    DetectionFeatureFrame::Cartesian => [
                        f0 / c.range_max, f1 / c.range_max, f2 / c.range_max,
                        det.velocity / c.velocity_max, det.rcs / c.rcs_max,
                    ],
                }
            }
        }
    }

    /// The pure transform. Same detections → same tensor, every call, no I/O.
    pub fn to_tensor(
        &self,
        detections: &[RadarDetection],
    ) -> Result<TensorBatch<'static>, RadarMappingError> {
        let c = &self.config;
        // Exhaustive so a future representation can't compile until its
        // transform is implemented (the config stays explicit, never silent).
        match c.representation {
            RadarRepresentation::DetectionList => {}
        }
        self.validate_config()?;

        if detections.is_empty() {
            return Err(RadarMappingError::EmptyDetectionList);
        }

        // Collect the in-FOV detections' feature rows, preserving input order.
        let mut rows: Vec<[f32; RADAR_FEATURES]> = Vec::new();
        for (idx, det) in detections.iter().enumerate() {
            if !det.range.is_finite() || !det.azimuth.is_finite() || !det.velocity.is_finite()
                || !det.rcs.is_finite()
                || det.elevation.map(|e| !e.is_finite()).unwrap_or(false)
            {
                return Err(RadarMappingError::NonFiniteDetection { index: idx });
            }
            let elevation = self.resolve_elevation(det, idx)?;
            let in_fov = det.range >= c.range_min && det.range <= c.range_max
                && det.azimuth >= c.az_min && det.azimuth <= c.az_max
                && elevation >= c.el_min && elevation <= c.el_max;
            if !in_fov {
                match c.out_of_bounds {
                    OutOfBoundsPolicy::Drop => continue,
                    OutOfBoundsPolicy::Error => {
                        return Err(RadarMappingError::OutOfFovDetection { index: idx });
                    }
                }
            }
            rows.push(self.features(det, elevation));
        }

        if rows.len() > c.max_detections {
            match c.on_overflow {
                OverflowPolicy::DropExcess => rows.truncate(c.max_detections),
                OverflowPolicy::Error => {
                    return Err(RadarMappingError::TooManyDetections {
                        found: rows.len(),
                        max: c.max_detections,
                    });
                }
            }
        }

        // Assemble [N, F] / [F, N], zero-padding unused rows.
        let n = c.max_detections;
        let f = RADAR_FEATURES;
        let mut out = vec![0.0_f32; n * f];
        for (row_idx, feats) in rows.iter().enumerate() {
            for (fi, &v) in feats.iter().enumerate() {
                let out_idx = match c.layout {
                    RadarLayout::DetectionMajor => row_idx * f + fi,
                    RadarLayout::FeatureMajor => fi * n + row_idx,
                };
                out[out_idx] = v;
            }
        }

        let mut named = HashMap::new();
        named.insert(c.tensor_name.clone(), TensorStorage::Owned(out));
        Ok(TensorBatch { named_tensors: named, metadata: HashMap::new() })
    }
}

// ===========================================================================
// IMU mapping  (sensor_msgs/Imu → state-vector tensor)
// ===========================================================================
//
// SAFETY FRAMING. Sensor mapping is UPSTREAM of the governor's guarantee. An
// IMU mapping that adds/omits gravity on the wrong axis, scrambles a
// quaternion, or carries a field at the wrong index feeds Parko's model
// corrupted motion state — a command that is confidently wrong AND within the
// envelope the governor passes. Deterministic, convention-correct, fail-closed
// is the point.
//
// STATE-VECTOR PATTERN — reuses the odom mapping above. Output is a flat,
// fixed-order `Vec<f32>` (there is NO NCHW/NHWC layout for a 1-D state vector,
// so — like odom — there is no layout enum). Orientation reuses odom's
// `OdomOrientation` enum and `quat_to_euler` verbatim, so adapter, odom, and
// IMU agree on quaternion convention + what "yaw" means.
//
// WHAT'S NEW VS ODOM (each a correctness surface): odom carried linear
// VELOCITY; the IMU carries linear ACCELERATION, and a raw accelerometer reads
// the GRAVITY vector (~9.81 m/s² on the up axis) even when stationary. Plus
// IMU orientation is OPTIONAL (sensor_msgs/Imu reports it unavailable via
// covariance[0] = -1). Both are handled explicitly below; odom does NOT
// validate quaternion norm, so the unit-norm check here is newly added.
//
// GRAVITY (FLAGGED — the headline IMU concern). The model expects either raw
// (gravity-included) or gravity-compensated (true motion) acceleration; the
// wrong one is a silent ~9.81 m/s² offset on one axis. `GravityPolicy` makes it
// explicit — never silently picked. `Raw` is the safe lossless default (no
// transform). `Compensated` subtracts the rotated gravity vector and FAILS
// CLOSED if orientation is absent (you cannot remove gravity without attitude;
// never assume level).
//
// UNITS / AXIS CONVENTION (EXPLICIT, like the LiDAR axis convention). Body
// frame REP-103: x-forward, y-left, z-up. `angular_velocity` is rad/s (NOT
// deg/s — a 57× error). `linear_acceleration` is m/s² (NOT g). A stationary
// LEVEL IMU reads `+gravity` on the +z (up) axis. Quaternion is ROS order
// `(x, y, z, w)`, Hamilton convention (same as odom). The quaternion must be
// unit-norm: validated within `quat_norm_tolerance` and REJECTED beyond it —
// never silently re-normalized (that masks a sensor fault).
//
// ROS SHIM — DEFERRED. `sensor_msgs/Imu → ImuSample` and the
// `SensorInputMapping` trait impl are the ros2-gated layer, like the other
// shims. PLANNED, not implemented here. (Magnetometer is a separate message,
// `sensor_msgs/MagneticField` — a possible small follow-on state-vector
// mapping, out of scope here.)

/// A unit quaternion in ROS order `(x, y, z, w)`, Hamilton convention — the
/// same convention odom's `quat_to_euler` consumes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quaternion {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Quaternion {
    /// Euclidean norm `√(x²+y²+z²+w²)`. A valid orientation quaternion is 1.0.
    #[must_use]
    pub fn norm(self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w).sqrt()
    }
}

/// One IMU observation. NOT the ROS message — that is the deferred shim's
/// input. `orientation` is `None` when the sensor reports it unavailable
/// (`sensor_msgs/Imu` covariance[0] = -1); it is NEVER fabricated as identity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImuSample {
    /// Linear acceleration, m/s², body frame. Includes gravity (raw
    /// accelerometer); see `GravityPolicy`.
    pub linear_acceleration: [f32; 3],
    /// Angular velocity, rad/s, body frame.
    pub angular_velocity: [f32; 3],
    /// Orientation, or `None` if unavailable.
    pub orientation: Option<Quaternion>,
}

/// How the gravity component of `linear_acceleration` is handled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GravityPolicy {
    /// Pass the accelerometer reading through unchanged (gravity included).
    /// Lossless, no transform — the model must expect raw acceleration.
    Raw,
    /// Subtract the rotated gravity vector to recover true linear acceleration.
    /// REQUIRES orientation; fails closed (`GravityCompensationNeedsOrientation`)
    /// if it is absent — gravity cannot be removed without knowing attitude.
    Compensated,
}

/// Feature scaling for the accel/gyro blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImuNormalization {
    /// Physical units (m/s², rad/s); orientation is never scaled.
    Raw,
    /// `acceleration / accel_scale`, `angular_velocity / gyro_scale`. NO clamp —
    /// clamping would lose the very transient (a spike, a jerk) the model needs.
    /// Orientation (quaternion already unit, Euler in radians) is not scaled.
    Normalized,
}

/// Optional fault gate: an implausibly large reading is REJECTED, never clipped
/// (clamping an impossible value to a plausible max hides the fault).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImuSanityBound {
    /// Max plausible `|linear_acceleration|` (vector magnitude), m/s².
    pub max_accel_mps2: f32,
    /// Max plausible `|angular_velocity|` (vector magnitude), rad/s.
    pub max_gyro_rad_s: f32,
}

/// IMU state-vector mapping configuration. Everything that changes physical
/// meaning is explicit; no hidden default. Output order (each block present
/// only if selected): `[acceleration(3)] [angular_velocity(3)] [orientation]`.
#[derive(Debug, Clone)]
pub struct ImuConfig {
    /// Include the (policy-applied) linear acceleration block (3 floats).
    pub include_acceleration: bool,
    /// Include the angular-velocity (gyro) block (3 floats).
    pub include_angular_velocity: bool,
    /// Include the orientation block, in the chosen representation. Reuses
    /// odom's `OdomOrientation` (Quaternion=4, FullEuler=3, Yaw=1). `Some`
    /// requires the sample to carry orientation, else fail-closed.
    pub include_orientation: Option<OdomOrientation>,
    /// Raw vs gravity-compensated acceleration. See `GravityPolicy`.
    pub gravity_policy: GravityPolicy,
    /// Gravity magnitude (m/s²) used by `Compensated`. Finite, `> 0`.
    pub gravity_mps2: f32,
    /// Accel/gyro scaling. See `ImuNormalization`.
    pub normalization: ImuNormalization,
    /// Under `Normalized`, the acceleration mapping to 1.0. Finite, `> 0`.
    pub accel_scale: f32,
    /// Under `Normalized`, the angular velocity mapping to 1.0. Finite, `> 0`.
    pub gyro_scale: f32,
    /// Unit-norm tolerance for the orientation quaternion. `>= 0`, finite.
    pub quat_norm_tolerance: f32,
    /// Optional implausible-magnitude fault gate.
    pub sanity: Option<ImuSanityBound>,
    /// Tensor name inside the produced `TensorBatch`.
    pub tensor_name: String,
}

impl ImuConfig {
    /// Total length of the produced state vector.
    #[must_use]
    pub fn vector_len(&self) -> usize {
        (if self.include_acceleration { 3 } else { 0 })
            + (if self.include_angular_velocity { 3 } else { 0 })
            + self.include_orientation.map(|o| o.float_count()).unwrap_or(0)
    }
}

/// Errors the pure IMU transform may return. A dedicated sibling enum (like the
/// other sensor errors), kept disjoint — not folded into Odom/Lidar/Radar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImuMappingError {
    /// No feature block selected — the output would be empty.
    EmptyFeatureSet,
    /// `Normalized` with a non-finite or `<= 0` scale (`accel_scale`/`gyro_scale`).
    InvalidNormalizationScale,
    /// `Compensated` with a non-finite or `<= 0` `gravity_mps2`.
    InvalidGravity,
    /// `quat_norm_tolerance` is negative or non-finite.
    InvalidQuatTolerance,
    /// A sanity bound is non-finite or `<= 0`.
    InvalidSanityBound,
    /// A non-finite (`NaN`/`inf`) acceleration, gyro, or quaternion component —
    /// the whole transform is rejected so one bad field can't corrupt output.
    NonFiniteSample,
    /// `include_orientation` is set but the sample has no orientation. Never
    /// fabricated as identity (that silently asserts "level + facing forward").
    OrientationRequiredButMissing,
    /// `Compensated` but the sample has no orientation — cannot remove gravity
    /// without attitude.
    GravityCompensationNeedsOrientation,
    /// The orientation quaternion is not unit-norm within tolerance. Rejected,
    /// never silently re-normalized (that would mask a sensor fault).
    NonUnitQuaternion,
    /// `|linear_acceleration|` exceeds the configured sanity bound.
    ImplausibleAcceleration,
    /// `|angular_velocity|` exceeds the configured sanity bound.
    ImplausibleAngularVelocity,
}

/// Pure IMU → state-vector tensor mapping. Cloning is cheap.
#[derive(Debug, Clone)]
pub struct ImuMapping {
    config: ImuConfig,
}

impl ImuMapping {
    #[must_use]
    pub fn new(config: ImuConfig) -> Self {
        Self { config }
    }

    fn validate_config(&self) -> Result<(), ImuMappingError> {
        let c = &self.config;
        if c.vector_len() == 0 {
            return Err(ImuMappingError::EmptyFeatureSet);
        }
        if c.normalization == ImuNormalization::Normalized
            && (!c.accel_scale.is_finite() || c.accel_scale <= 0.0
                || !c.gyro_scale.is_finite() || c.gyro_scale <= 0.0)
        {
            return Err(ImuMappingError::InvalidNormalizationScale);
        }
        if c.gravity_policy == GravityPolicy::Compensated
            && (!c.gravity_mps2.is_finite() || c.gravity_mps2 <= 0.0)
        {
            return Err(ImuMappingError::InvalidGravity);
        }
        if !c.quat_norm_tolerance.is_finite() || c.quat_norm_tolerance < 0.0 {
            return Err(ImuMappingError::InvalidQuatTolerance);
        }
        if let Some(s) = c.sanity {
            if !s.max_accel_mps2.is_finite() || s.max_accel_mps2 <= 0.0
                || !s.max_gyro_rad_s.is_finite() || s.max_gyro_rad_s <= 0.0
            {
                return Err(ImuMappingError::InvalidSanityBound);
            }
        }
        Ok(())
    }

    /// World-up unit vector expressed in the body frame for orientation `q`
    /// (= `R(q)ᵀ · ẑ_world`). Used to subtract gravity in the body frame.
    fn world_up_in_body(q: Quaternion) -> [f32; 3] {
        [
            2.0 * (q.x * q.z - q.w * q.y),
            2.0 * (q.y * q.z + q.w * q.x),
            1.0 - 2.0 * (q.x * q.x + q.y * q.y),
        ]
    }

    /// The pure transform. Same sample → same tensor, every call, no I/O.
    pub fn to_tensor(&self, sample: &ImuSample) -> Result<TensorBatch<'static>, ImuMappingError> {
        let c = &self.config;
        self.validate_config()?;

        // Non-finite rejection (accel, gyro, and the quaternion if present).
        let any_nonfinite_vec3 = |v: &[f32; 3]| v.iter().any(|x| !x.is_finite());
        if any_nonfinite_vec3(&sample.linear_acceleration)
            || any_nonfinite_vec3(&sample.angular_velocity)
            || sample
                .orientation
                .map(|q| !(q.x.is_finite() && q.y.is_finite() && q.z.is_finite() && q.w.is_finite()))
                .unwrap_or(false)
        {
            return Err(ImuMappingError::NonFiniteSample);
        }

        // Orientation availability (fail-closed; never fabricate identity).
        if c.include_orientation.is_some() && sample.orientation.is_none() {
            return Err(ImuMappingError::OrientationRequiredButMissing);
        }
        if c.gravity_policy == GravityPolicy::Compensated && sample.orientation.is_none() {
            return Err(ImuMappingError::GravityCompensationNeedsOrientation);
        }

        // Validate the quaternion's unit-norm whenever it will be USED (for an
        // orientation feature OR for gravity compensation). Reject, never
        // re-normalize.
        let quat_used = c.include_orientation.is_some()
            || c.gravity_policy == GravityPolicy::Compensated;
        if quat_used {
            if let Some(q) = sample.orientation {
                if (q.norm() - 1.0).abs() > c.quat_norm_tolerance {
                    return Err(ImuMappingError::NonUnitQuaternion);
                }
            }
        }

        // Sanity fault gate (reject, never clip).
        if let Some(s) = c.sanity {
            let a = sample.linear_acceleration;
            let g = sample.angular_velocity;
            let accel_mag = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
            let gyro_mag = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
            if accel_mag > s.max_accel_mps2 {
                return Err(ImuMappingError::ImplausibleAcceleration);
            }
            if gyro_mag > s.max_gyro_rad_s {
                return Err(ImuMappingError::ImplausibleAngularVelocity);
            }
        }

        // Gravity policy → the acceleration that enters the feature vector.
        let accel = match c.gravity_policy {
            GravityPolicy::Raw => sample.linear_acceleration,
            GravityPolicy::Compensated => {
                // orientation guaranteed Some by the check above.
                let q = sample.orientation.expect("checked present");
                let up = Self::world_up_in_body(q);
                // a_true = measured − gravity · (world-up in body).
                [
                    sample.linear_acceleration[0] - c.gravity_mps2 * up[0],
                    sample.linear_acceleration[1] - c.gravity_mps2 * up[1],
                    sample.linear_acceleration[2] - c.gravity_mps2 * up[2],
                ]
            }
        };

        // Build the state vector in the fixed documented order.
        let mut out: Vec<f32> = Vec::with_capacity(c.vector_len());
        let scale_accel = |x: f32| match c.normalization {
            ImuNormalization::Raw => x,
            ImuNormalization::Normalized => x / c.accel_scale,
        };
        let scale_gyro = |x: f32| match c.normalization {
            ImuNormalization::Raw => x,
            ImuNormalization::Normalized => x / c.gyro_scale,
        };

        if c.include_acceleration {
            out.push(scale_accel(accel[0]));
            out.push(scale_accel(accel[1]));
            out.push(scale_accel(accel[2]));
        }
        if c.include_angular_velocity {
            out.push(scale_gyro(sample.angular_velocity[0]));
            out.push(scale_gyro(sample.angular_velocity[1]));
            out.push(scale_gyro(sample.angular_velocity[2]));
        }
        if let Some(repr) = c.include_orientation {
            // orientation guaranteed Some by the availability check above.
            let q = sample.orientation.expect("checked present");
            match repr {
                OdomOrientation::Quaternion => {
                    out.push(q.x);
                    out.push(q.y);
                    out.push(q.z);
                    out.push(q.w);
                }
                OdomOrientation::FullEuler => {
                    // Reuse odom's quaternion→Euler (ROS x,y,z,w; ZYX).
                    let (roll, pitch, yaw) =
                        quat_to_euler(q.x as f64, q.y as f64, q.z as f64, q.w as f64);
                    out.push(roll as f32);
                    out.push(pitch as f32);
                    out.push(yaw as f32);
                }
                OdomOrientation::Yaw => {
                    let (_, _, yaw) =
                        quat_to_euler(q.x as f64, q.y as f64, q.z as f64, q.w as f64);
                    out.push(yaw as f32);
                }
            }
        }

        let mut named = HashMap::new();
        named.insert(c.tensor_name.clone(), TensorStorage::Owned(out));
        Ok(TensorBatch { named_tensors: named, metadata: HashMap::new() })
    }
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
        assert_eq!(get(&out).len(), (2 * 2), "mono8 produces H*W*1 floats");
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
            quat_norm_tolerance: 1e-3,
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
            quat_norm_tolerance: 1e-3,
        };
        let out = OdomMapping::new(cfg).to_tensor(&sample).expect("valid odom sample");
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
            quat_norm_tolerance: 1e-3,
        };
        let out = OdomMapping::new(cfg).to_tensor(&sample).expect("valid odom sample");
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
        let out = OdomMapping::new(all_on(OdomOrientation::Yaw)).to_tensor(&sample).expect("valid odom sample");
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
            quat_norm_tolerance: 1e-3,
        };
        assert_eq!(cfg.vector_len(), 6);
        let out = OdomMapping::new(cfg).to_tensor(&sample).expect("valid odom sample");
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
            quat_norm_tolerance: 1e-3,
        };
        assert_eq!(cfg.vector_len(), 3);
        let out = OdomMapping::new(cfg).to_tensor(&sample).expect("valid odom sample");
        let v = s_get(&out);
        assert_eq!(v.len(), 3);
        assert_eq!(v, &[0.0, 0.0, 0.0]); // identity quaternion
    }

    /// Quaternion representation — 4 floats, raw.
    #[test]
    fn raw_quaternion_passthrough() {
        let sample = OdomSample {
            position:         [0.0; 3],
            orientation_xyzw: [0.182_574_2, 0.365_148_4, 0.547_722_6, 0.730_296_8], // unit norm
            linear_velocity:  [0.0; 3],
            angular_velocity: [0.0; 3],
        };
        let cfg = OdomConfig {
            include_position: false,
            include_orientation: Some(OdomOrientation::Quaternion),
            include_linear_velocity: false,
            include_angular_velocity: false,
            tensor_name: "odom".to_string(),
            quat_norm_tolerance: 1e-3,
        };
        let out = OdomMapping::new(cfg).to_tensor(&sample).expect("valid odom sample");
        let v = s_get(&out);
        assert_eq!(v, &[0.182_574_2_f32, 0.365_148_4, 0.547_722_6, 0.730_296_8]);
    }

    // -- Quaternion-norm fail-closed guard (mirrors IMU; the odom gap) --------

    fn odom_with_quat(q: [f64; 4], repr: OdomOrientation) -> (OdomMapping, OdomSample) {
        let cfg = OdomConfig {
            include_position: false,
            include_orientation: Some(repr),
            include_linear_velocity: false,
            include_angular_velocity: false,
            tensor_name: "odom".to_string(),
            quat_norm_tolerance: 1e-3,
        };
        let sample = OdomSample {
            position: [0.0; 3],
            orientation_xyzw: q,
            linear_velocity: [0.0; 3],
            angular_velocity: [0.0; 3],
        };
        (OdomMapping::new(cfg), sample)
    }

    /// A NON-UNIT quaternion (norm 2.0) is REJECTED across the full, yaw, and
    /// raw-quaternion orientation reprs — never fed to quat_to_euler.
    #[test]
    fn non_unit_quaternion_is_rejected_all_reprs() {
        let non_unit = [0.0, 0.0, 0.0, 2.0]; // norm 2.0
        for repr in [OdomOrientation::FullEuler, OdomOrientation::Yaw, OdomOrientation::Quaternion] {
            let (m, s) = odom_with_quat(non_unit, repr);
            assert_eq!(m.to_tensor(&s).unwrap_err(), OdomMappingError::NonUnitQuaternion, "repr {repr:?}");
        }
    }

    /// A near-zero / all-zero quaternion (norm ~0) is REJECTED (it would make
    /// quat_to_euler produce garbage).
    #[test]
    fn near_zero_quaternion_is_rejected() {
        let (m, s) = odom_with_quat([0.0, 0.0, 0.0, 0.0], OdomOrientation::Yaw);
        assert_eq!(m.to_tensor(&s).unwrap_err(), OdomMappingError::NonUnitQuaternion);
    }

    /// A non-finite (NaN) quaternion component is REJECTED before quat_to_euler.
    #[test]
    fn non_finite_quaternion_is_rejected() {
        let (m, s) = odom_with_quat([f64::NAN, 0.0, 0.0, 1.0], OdomOrientation::FullEuler);
        assert_eq!(m.to_tensor(&s).unwrap_err(), OdomMappingError::NonFiniteQuaternion);
    }

    /// A unit quaternion, and one slightly off but WITHIN tolerance, are ACCEPTED
    /// and produce the correct yaw.
    #[test]
    fn unit_and_within_tolerance_quaternion_accepted() {
        let theta = std::f64::consts::FRAC_PI_4;
        let half = theta / 2.0;
        // Exact unit.
        let (m, s) = odom_with_quat([0.0, 0.0, half.sin(), half.cos()], OdomOrientation::Yaw);
        let out = m.to_tensor(&s).expect("unit quaternion accepted");
        assert!((s_get(&out)[0] - theta as f32).abs() < 1e-5);

        // Slightly off but within 1e-3 tolerance (scale by 1.0005 → norm 1.0005).
        let off = [0.0, 0.0, half.sin() * 1.0005, half.cos() * 1.0005];
        let (m2, s2) = odom_with_quat(off, OdomOrientation::Yaw);
        assert!(m2.to_tensor(&s2).is_ok(), "within-tolerance quaternion must be accepted");
    }

    /// NEGATIVE CONTROL — the UNGUARDED computation WOULD have produced
    /// wrong Euler for the non-unit input the guard now rejects. The delta
    /// (guard rejects vs unguarded garbage) is the evidence the guard changed the
    /// outcome.
    #[test]
    fn negative_control_unguarded_quat_to_euler_would_be_garbage() {
        // A pure-yaw rotation of 45° is (0, 0, sin(22.5°), cos(22.5°)); SCALED by
        // 2 it has norm 2.0 but the same intended attitude. quat_to_euler is NOT
        // scale-invariant — the atan2 denominator (1 - 2(qy²+qz²)) is wrong for
        // the scaled input, so the UNGUARDED path (what odom used to do) yields a
        // grossly WRONG yaw; the unit form recovers 45°.
        let theta = std::f64::consts::FRAC_PI_4; // 45°
        let half = theta / 2.0;
        let q_unit = [0.0, 0.0, half.sin(), half.cos()];
        let q_nonunit = [0.0, 0.0, 2.0 * half.sin(), 2.0 * half.cos()]; // norm 2.0
        let (_, _, correct_yaw) = quat_to_euler(q_unit[0], q_unit[1], q_unit[2], q_unit[3]);
        let (_, _, garbage_yaw) =
            quat_to_euler(q_nonunit[0], q_nonunit[1], q_nonunit[2], q_nonunit[3]);
        assert!((correct_yaw - theta).abs() < 1e-5, "unit form recovers 45°, got {correct_yaw}");
        assert!(
            (garbage_yaw - correct_yaw).abs() > 1e-2,
            "unguarded non-unit quaternion must produce a WRONG yaw (garbage {garbage_yaw} vs correct {correct_yaw})"
        );
        // And the guard now REJECTS exactly that input instead of emitting garbage.
        let (m, s) = odom_with_quat(q_nonunit, OdomOrientation::Yaw);
        assert_eq!(m.to_tensor(&s).unwrap_err(), OdomMappingError::NonUnitQuaternion);
    }

    /// Config: a negative or non-finite tolerance is itself REJECTED (mirrors
    /// ImuMappingError::InvalidQuatTolerance).
    #[test]
    fn invalid_quat_tolerance_is_rejected() {
        for bad in [-1.0_f32, f32::NAN, f32::INFINITY] {
            let cfg = OdomConfig {
                include_position: false,
                include_orientation: Some(OdomOrientation::Yaw),
                include_linear_velocity: false,
                include_angular_velocity: false,
                tensor_name: "odom".to_string(),
                quat_norm_tolerance: bad,
            };
            let sample = OdomSample {
                position: [0.0; 3],
                orientation_xyzw: [0.0, 0.0, 0.0, 1.0],
                linear_velocity: [0.0; 3],
                angular_velocity: [0.0; 3],
            };
            assert_eq!(
                OdomMapping::new(cfg).to_tensor(&sample).unwrap_err(),
                OdomMappingError::InvalidQuatTolerance,
                "tolerance {bad} must be rejected"
            );
        }
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

// ===========================================================================
// LiDAR — tests
// ===========================================================================

#[cfg(test)]
mod lidar_tests {
    use super::*;
    use proptest::prelude::*;

    /// A 2×2-cell BEV over `[0,2)×[0,2)`, 1 m cells, z∈[0,4], Raw, NCHW.
    fn cfg_2x2(channels: Vec<BevChannel>, oob: OutOfBoundsPolicy) -> LidarConfig {
        LidarConfig {
            representation: LidarRepresentation::BevGrid,
            x_min: 0.0, x_max: 2.0,
            y_min: 0.0, y_max: 2.0,
            z_min: 0.0, z_max: 4.0,
            resolution_m: 1.0,
            channels,
            normalization: BevNormalization::Raw,
            density_norm: 1.0,
            intensity_max: 1.0,
            layout: CameraLayout::Nchw,
            out_of_bounds: oob,
            tensor_name: "lidar_bev".to_string(),
        }
    }

    fn pt(x: f32, y: f32, z: f32, intensity: f32) -> LidarPoint {
        LidarPoint { x, y, z, intensity }
    }

    fn t<'a>(b: &'a TensorBatch<'static>) -> &'a [f32] {
        b.named_tensors.get("lidar_bev").unwrap().as_slice()
    }

    /// DETERMINISTIC CORRECTNESS: cell assignment + occupancy/max-height
    /// verified EXACTLY on a 2×2 grid (cell assignment is checked, not assumed).
    /// (x,y) → col=floor(x), row=floor(y); NCHW order is cell = row*2 + col.
    #[test]
    fn bev_hand_computed_2x2_occupancy_and_maxheight() {
        let cloud = vec![
            pt(0.5, 0.5, 1.0, 10.0), // r0c0
            pt(1.5, 0.5, 2.0, 20.0), // r0c1
            pt(0.5, 1.5, 3.0, 30.0), // r1c0
            pt(1.7, 1.2, 2.5, 40.0), // r1c1
            pt(1.2, 1.9, 0.5, 50.0), // r1c1 (max z stays 2.5)
        ];
        let cfg = cfg_2x2(
            vec![BevChannel::Occupancy, BevChannel::MaxHeight],
            OutOfBoundsPolicy::Error,
        );
        let out = LidarMapping::new(cfg).to_tensor(&cloud).expect("all in-ROI");
        // [C=2, H=2, W=2]: ch0 occupancy {r0c0,r0c1,r1c0,r1c1}, ch1 max-height.
        assert_eq!(
            t(&out),
            &[
                1.0, 1.0, 1.0, 1.0, // occupancy: every cell occupied
                1.0, 2.0, 3.0, 2.5, // max height per cell
            ]
        );
    }

    /// Output length == C × H × W.
    #[test]
    fn bev_output_dims_match_config() {
        let cfg = cfg_2x2(
            vec![BevChannel::Occupancy, BevChannel::Density, BevChannel::MaxHeight],
            OutOfBoundsPolicy::Drop,
        );
        let out = LidarMapping::new(cfg).to_tensor(&[pt(0.5, 0.5, 1.0, 1.0)]).expect("valid");
        assert_eq!(t(&out).len(), 3 * 2 * 2);
    }

    /// Identical cloud → identical tensor, every call.
    #[test]
    fn bev_is_deterministic() {
        let cfg = cfg_2x2(
            vec![BevChannel::Occupancy, BevChannel::MaxHeight, BevChannel::Density],
            OutOfBoundsPolicy::Drop,
        );
        let cloud = vec![pt(0.3, 1.2, 2.0, 5.0), pt(1.9, 0.1, 0.4, 7.0)];
        let a = LidarMapping::new(cfg.clone()).to_tensor(&cloud).expect("valid");
        let b = LidarMapping::new(cfg).to_tensor(&cloud).expect("valid");
        assert_eq!(t(&a), t(&b));
    }

    /// NHWC interleaves channels per cell; verify the layout index is honored.
    #[test]
    fn bev_nhwc_layout_interleaves_channels() {
        let mut cfg = cfg_2x2(
            vec![BevChannel::Occupancy, BevChannel::Density],
            OutOfBoundsPolicy::Drop,
        );
        cfg.layout = CameraLayout::Nhwc;
        // one point in r0c0 → occ=1, density=1 there; all other cells zero.
        let out = LidarMapping::new(cfg).to_tensor(&[pt(0.5, 0.5, 1.0, 1.0)]).expect("valid");
        // NHWC [H,W,C]: cell order (r0c0,r0c1,r1c0,r1c1), 2 channels each.
        assert_eq!(t(&out), &[1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    }

    // -- Fail-closed -----------------------------------------------------

    #[test]
    fn empty_cloud_fails_closed() {
        let err = LidarMapping::new(cfg_2x2(vec![BevChannel::Occupancy], OutOfBoundsPolicy::Drop))
            .to_tensor(&[])
            .unwrap_err();
        assert_eq!(err, LidarMappingError::EmptyCloud);
    }

    #[test]
    fn non_finite_point_fails_closed_at_index() {
        for bad in [
            pt(f32::NAN, 0.5, 1.0, 1.0),
            pt(0.5, f32::INFINITY, 1.0, 1.0),
            pt(0.5, 0.5, f32::NAN, 1.0),
            pt(0.5, 0.5, 1.0, f32::NAN),
        ] {
            let cloud = vec![pt(0.5, 0.5, 1.0, 1.0), bad];
            let err = LidarMapping::new(cfg_2x2(vec![BevChannel::Occupancy], OutOfBoundsPolicy::Drop))
                .to_tensor(&cloud)
                .unwrap_err();
            assert_eq!(err, LidarMappingError::NonFinitePoint { index: 1 });
        }
    }

    #[test]
    fn out_of_roi_point_errors_under_error_policy() {
        let cloud = vec![pt(0.5, 0.5, 1.0, 1.0), pt(5.0, 0.5, 1.0, 1.0)]; // 2nd beyond x_max
        let err = LidarMapping::new(cfg_2x2(vec![BevChannel::Occupancy], OutOfBoundsPolicy::Error))
            .to_tensor(&cloud)
            .unwrap_err();
        assert_eq!(err, LidarMappingError::OutOfRoiPoint { index: 1 });
    }

    #[test]
    fn out_of_roi_point_dropped_under_drop_policy() {
        // far point dropped; the two near points both land in r0c0.
        let cloud = vec![
            pt(0.5, 0.5, 1.0, 1.0),
            pt(50.0, 50.0, 1.0, 1.0),
            pt(0.5, 0.5, 1.0, 1.0),
        ];
        let out = LidarMapping::new(cfg_2x2(vec![BevChannel::Density], OutOfBoundsPolicy::Drop))
            .to_tensor(&cloud)
            .expect("valid");
        assert_eq!(t(&out), &[2.0, 0.0, 0.0, 0.0]);
    }

    /// A point outside the Z band is out-of-ROI too (height filter).
    #[test]
    fn out_of_z_band_is_out_of_roi() {
        let cloud = vec![pt(0.5, 0.5, 9.0, 1.0)]; // z above z_max=4
        let err = LidarMapping::new(cfg_2x2(vec![BevChannel::Occupancy], OutOfBoundsPolicy::Error))
            .to_tensor(&cloud)
            .unwrap_err();
        assert_eq!(err, LidarMappingError::OutOfRoiPoint { index: 0 });
    }

    #[test]
    fn malformed_config_fails_closed() {
        let base = cfg_2x2(vec![BevChannel::Occupancy], OutOfBoundsPolicy::Drop);
        let cloud = [pt(0.5, 0.5, 1.0, 1.0)];

        let mut c = base.clone();
        c.resolution_m = 0.0;
        assert_eq!(
            LidarMapping::new(c).to_tensor(&cloud).unwrap_err(),
            LidarMappingError::InvalidResolution
        );

        let mut c = base.clone();
        c.x_max = -1.0; // inverted
        assert_eq!(
            LidarMapping::new(c).to_tensor(&cloud).unwrap_err(),
            LidarMappingError::InvalidBounds
        );

        let mut c = base.clone();
        c.channels = vec![];
        assert_eq!(
            LidarMapping::new(c).to_tensor(&cloud).unwrap_err(),
            LidarMappingError::EmptyChannelSet
        );

        let mut c = base.clone();
        c.x_max = 2.5; // 2.5 / 1.0 not integer
        assert_eq!(
            LidarMapping::new(c).to_tensor(&cloud).unwrap_err(),
            LidarMappingError::GridExtentNotDivisible
        );

        let mut c = base.clone();
        c.normalization = BevNormalization::Normalized;
        c.density_norm = 0.0;
        assert_eq!(
            LidarMapping::new(c).to_tensor(&cloud).unwrap_err(),
            LidarMappingError::InvalidNormalizationScale
        );
    }

    // -- Safety invariant (property) -------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// SAFETY INVARIANT — the property the governor CANNOT protect: every
        /// in-ROI point lands in EXACTLY ONE cell, none silently lost or
        /// duplicated. With a Raw `Density` channel the grid sum must equal the
        /// point count, and each point's floor-computed cell must be counted.
        /// `out_of_bounds = Error` makes any stray out-of-ROI point fail loudly,
        /// so a green run also proves every generated point was in-ROI.
        #[test]
        fn prop_every_in_roi_point_counted_exactly_once(
            pts in proptest::collection::vec(
                (0.0_f32..10.0, 0.0_f32..10.0, 0.0_f32..3.0, 0.0_f32..100.0),
                1..50,
            ),
        ) {
            // ROI [0,10)×[0,10), 2 m cells → 5×5; z∈[0,3]. Generated points are
            // in-ROI by construction (half-open x,y; z below z_max).
            let cfg = LidarConfig {
                representation: LidarRepresentation::BevGrid,
                x_min: 0.0, x_max: 10.0,
                y_min: 0.0, y_max: 10.0,
                z_min: 0.0, z_max: 3.0,
                resolution_m: 2.0,
                channels: vec![BevChannel::Density],
                normalization: BevNormalization::Raw,
                density_norm: 1.0,
                intensity_max: 1.0,
                layout: CameraLayout::Nchw,
                out_of_bounds: OutOfBoundsPolicy::Error,
                tensor_name: "lidar_bev".to_string(),
            };
            let cloud: Vec<LidarPoint> =
                pts.iter().map(|&(x, y, z, i)| pt(x, y, z, i)).collect();
            let out = LidarMapping::new(cfg).to_tensor(&cloud).expect("all in-ROI");
            let grid = out.named_tensors.get("lidar_bev").unwrap().as_slice();

            // No point lost or duplicated: total count preserved.
            let total: f32 = grid.iter().sum();
            prop_assert_eq!(total as usize, cloud.len());

            // Each point counted in its own floor-computed cell.
            let n_cols = 5usize;
            for p in &cloud {
                let col = (p.x / 2.0).floor() as usize;
                let row = (p.y / 2.0).floor() as usize;
                prop_assert!(grid[row * n_cols + col] >= 1.0);
            }
        }
    }
}
// ===========================================================================
// Radar — tests
// ===========================================================================

#[cfg(test)]
mod radar_tests {
    use super::*;
    use proptest::prelude::*;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

    /// Range gate [1,50], az ±π/2, el ±π/4, N=8, Raw, DetectionMajor, Drop.
    fn cfg(
        frame: DetectionFeatureFrame,
        elevation_policy: ElevationPolicy,
        oob: OutOfBoundsPolicy,
    ) -> RadarConfig {
        RadarConfig {
            representation: RadarRepresentation::DetectionList,
            feature_frame: frame,
            range_min: 1.0, range_max: 50.0,
            az_min: -FRAC_PI_2, az_max: FRAC_PI_2,
            el_min: -FRAC_PI_4, el_max: FRAC_PI_4,
            max_detections: 8,
            on_overflow: OverflowPolicy::Error,
            elevation_policy,
            normalization: RadarNormalization::Raw,
            velocity_max: 30.0,
            rcs_max: 100.0,
            layout: RadarLayout::DetectionMajor,
            out_of_bounds: oob,
            tensor_name: "radar".to_string(),
        }
    }

    fn det(range: f32, az: f32, el: Option<f32>, v: f32, rcs: f32) -> RadarDetection {
        RadarDetection { range, azimuth: az, elevation: el, velocity: v, rcs }
    }

    fn t<'a>(b: &'a TensorBatch<'static>) -> &'a [f32] {
        b.named_tensors.get("radar").unwrap().as_slice()
    }

    /// GEOMETRIC CORRECTNESS: polar→cartesian verified at known angles, not
    /// assumed. r=5,az=0,el=0 → (5,0,0); az=π/2 → (0,5,0); el=π/4 → z=r·sin(π/4).
    #[test]
    fn cartesian_conversion_is_geometrically_correct() {
        let dets = vec![
            det(5.0, 0.0, Some(0.0), 3.0, 10.0),        // → (5,0,0)
            det(5.0, FRAC_PI_2, Some(0.0), -4.0, 20.0), // → (0,5,0)
            det(10.0, 0.0, Some(FRAC_PI_4), 1.0, 30.0), // → (10·cos45,0,10·sin45)
        ];
        let out = cart_tensor(&dets);
        let r = t(&out);
        let eps = 1e-4;
        // row 0: (5,0,0,3,10)
        assert!((r[0] - 5.0).abs() < eps && r[1].abs() < eps && r[2].abs() < eps);
        assert_eq!(r[3], 3.0); // Doppler preserved
        assert_eq!(r[4], 10.0);
        // row 1 (F=5): (0,5,0,-4,20)
        assert!(r[5].abs() < eps && (r[6] - 5.0).abs() < eps && r[7].abs() < eps);
        assert_eq!(r[8], -4.0);
        // row 2: (10·cos45, 0, 10·sin45, 1, 30)
        let c45 = (FRAC_PI_4).cos() * 10.0;
        let s45 = (FRAC_PI_4).sin() * 10.0;
        assert!((r[10] - c45).abs() < eps && r[11].abs() < eps && (r[12] - s45).abs() < eps);
        assert_eq!(r[13], 1.0);
    }

    // Cartesian-frame transform, so the geometry test reads cleanly.
    fn cart_tensor(dets: &[RadarDetection]) -> TensorBatch<'static> {
        RadarMapping::new(cfg(
            DetectionFeatureFrame::Cartesian,
            ElevationPolicy::Assume(0.0),
            OutOfBoundsPolicy::Error,
        ))
        .to_tensor(dets)
        .expect("valid")
    }

    /// DOPPLER PRESERVED verbatim in the velocity column (index 3), both frames.
    #[test]
    fn doppler_velocity_is_preserved() {
        for frame in [DetectionFeatureFrame::Polar, DetectionFeatureFrame::Cartesian] {
            let out = RadarMapping::new(cfg(frame, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Error))
                .to_tensor(&[det(10.0, 0.2, Some(0.0), 7.5, 12.0)])
                .expect("valid");
            assert_eq!(t(&out)[3], 7.5, "Doppler must pass through unchanged");
        }
    }

    /// Polar frame passes [range, az, el, v, rcs] through verbatim (Raw).
    #[test]
    fn polar_frame_is_verbatim() {
        let out = RadarMapping::new(cfg(
            DetectionFeatureFrame::Polar,
            ElevationPolicy::Reject,
            OutOfBoundsPolicy::Error,
        ))
        .to_tensor(&[det(12.0, 0.3, Some(0.1), 5.0, 9.0)])
        .expect("valid");
        assert_eq!(&t(&out)[0..5], &[12.0, 0.3, 0.1, 5.0, 9.0]);
    }

    /// Output length == N × F; unused rows zero-padded.
    #[test]
    fn output_dims_and_padding() {
        let out = RadarMapping::new(cfg(
            DetectionFeatureFrame::Polar,
            ElevationPolicy::Assume(0.0),
            OutOfBoundsPolicy::Drop,
        ))
        .to_tensor(&[det(10.0, 0.0, Some(0.0), 1.0, 1.0)])
        .expect("valid");
        assert_eq!(t(&out).len(), 8 * 5);
        // only the first row populated; the rest are zero padding.
        assert!(t(&out)[5..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn is_deterministic() {
        let c = cfg(DetectionFeatureFrame::Cartesian, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Drop);
        let dets = vec![det(7.0, 0.4, Some(0.1), 2.0, 5.0), det(20.0, -0.5, None, -3.0, 8.0)];
        let a = RadarMapping::new(c.clone()).to_tensor(&dets).expect("valid");
        let b = RadarMapping::new(c).to_tensor(&dets).expect("valid");
        assert_eq!(t(&a), t(&b));
    }

    /// FeatureMajor lays features down columns: [F, N].
    #[test]
    fn feature_major_layout() {
        let mut c = cfg(DetectionFeatureFrame::Polar, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Drop);
        c.layout = RadarLayout::FeatureMajor;
        c.max_detections = 2;
        let out = RadarMapping::new(c).to_tensor(&[det(10.0, 0.2, Some(0.0), 4.0, 6.0)]).expect("valid");
        // [F=5, N=2]: feature f of detection 0 is at index f*2 + 0.
        let r = t(&out);
        assert_eq!(r.len(), 5 * 2);
        assert_eq!(r[0 * 2], 10.0); // range
        assert_eq!(r[2], 0.2);  // az
        assert_eq!(r[3 * 2], 4.0);  // velocity (Doppler)
        // detection-1 slots (odd indices) are padding.
        assert!((0..5).all(|f| r[f * 2 + 1] == 0.0));
    }

    // -- 2D radar / elevation policy ------------------------------------

    #[test]
    fn missing_elevation_assume_substitutes_value() {
        // 2D detection, Assume(0) → cartesian z == 0.
        let out = RadarMapping::new(cfg(
            DetectionFeatureFrame::Cartesian,
            ElevationPolicy::Assume(0.0),
            OutOfBoundsPolicy::Error,
        ))
        .to_tensor(&[det(10.0, 0.0, None, 1.0, 1.0)])
        .expect("valid");
        assert!(t(&out)[2].abs() < 1e-4, "z must be the assumed-ground 0");
    }

    #[test]
    fn missing_elevation_reject_fails_closed() {
        let err = RadarMapping::new(cfg(
            DetectionFeatureFrame::Cartesian,
            ElevationPolicy::Reject,
            OutOfBoundsPolicy::Error,
        ))
        .to_tensor(&[det(10.0, 0.0, None, 1.0, 1.0)])
        .unwrap_err();
        assert_eq!(err, RadarMappingError::MissingElevation { index: 0 });
    }

    // -- Fail-closed -----------------------------------------------------

    #[test]
    fn empty_list_fails_closed() {
        let err = RadarMapping::new(cfg(DetectionFeatureFrame::Polar, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Drop))
            .to_tensor(&[])
            .unwrap_err();
        assert_eq!(err, RadarMappingError::EmptyDetectionList);
    }

    #[test]
    fn non_finite_detection_fails_closed_at_index() {
        let bads = [
            det(f32::NAN, 0.0, Some(0.0), 1.0, 1.0),
            det(10.0, f32::INFINITY, Some(0.0), 1.0, 1.0),
            det(10.0, 0.0, Some(f32::NAN), 1.0, 1.0),
            det(10.0, 0.0, Some(0.0), f32::NAN, 1.0),
            det(10.0, 0.0, Some(0.0), 1.0, f32::INFINITY),
        ];
        for bad in bads {
            let dets = vec![det(10.0, 0.0, Some(0.0), 1.0, 1.0), bad];
            let err = RadarMapping::new(cfg(DetectionFeatureFrame::Polar, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Drop))
                .to_tensor(&dets)
                .unwrap_err();
            assert_eq!(err, RadarMappingError::NonFiniteDetection { index: 1 });
        }
    }

    #[test]
    fn out_of_fov_detection_errors_under_error_policy() {
        // 2nd detection beyond range_max.
        let dets = vec![det(10.0, 0.0, Some(0.0), 1.0, 1.0), det(500.0, 0.0, Some(0.0), 1.0, 1.0)];
        let err = RadarMapping::new(cfg(DetectionFeatureFrame::Polar, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Error))
            .to_tensor(&dets)
            .unwrap_err();
        assert_eq!(err, RadarMappingError::OutOfFovDetection { index: 1 });
    }

    #[test]
    fn out_of_fov_detection_dropped_under_drop_policy() {
        let dets = vec![
            det(10.0, 0.0, Some(0.0), 1.0, 1.0),
            det(500.0, 3.0, Some(0.0), 9.0, 9.0), // far + out of az FOV → dropped
            det(11.0, 0.1, Some(0.0), 2.0, 2.0),
        ];
        let out = RadarMapping::new(cfg(DetectionFeatureFrame::Polar, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Drop))
            .to_tensor(&dets)
            .expect("valid");
        // two kept rows in order; row 0 range 10, row 1 range 11; rest padding.
        assert_eq!(t(&out)[0], 10.0);
        assert_eq!(t(&out)[5], 11.0);
        assert!(t(&out)[10..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn overflow_errors_under_error_policy() {
        let mut c = cfg(DetectionFeatureFrame::Polar, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Error);
        c.max_detections = 2;
        let dets: Vec<RadarDetection> = (0..3).map(|i| det(10.0 + i as f32, 0.0, Some(0.0), 1.0, 1.0)).collect();
        let err = RadarMapping::new(c).to_tensor(&dets).unwrap_err();
        assert_eq!(err, RadarMappingError::TooManyDetections { found: 3, max: 2 });
    }

    #[test]
    fn overflow_drops_excess_under_drop_policy() {
        let mut c = cfg(DetectionFeatureFrame::Polar, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Drop);
        c.max_detections = 2;
        c.on_overflow = OverflowPolicy::DropExcess;
        let dets: Vec<RadarDetection> = (0..3).map(|i| det(10.0 + i as f32, 0.0, Some(0.0), 1.0, 1.0)).collect();
        let out = RadarMapping::new(c).to_tensor(&dets).expect("valid");
        // first two kept (range 10, 11); third dropped.
        assert_eq!(t(&out).len(), 2 * 5);
        assert_eq!(t(&out)[0], 10.0);
        assert_eq!(t(&out)[5], 11.0);
    }

    #[test]
    fn malformed_config_fails_closed() {
        let base = cfg(DetectionFeatureFrame::Polar, ElevationPolicy::Assume(0.0), OutOfBoundsPolicy::Drop);
        let dets = [det(10.0, 0.0, Some(0.0), 1.0, 1.0)];

        let mut c = base.clone();
        c.range_min = -1.0;
        assert_eq!(RadarMapping::new(c).to_tensor(&dets).unwrap_err(), RadarMappingError::InvalidRangeGate);

        let mut c = base.clone();
        c.az_max = c.az_min; // inverted/empty az FOV
        assert_eq!(RadarMapping::new(c).to_tensor(&dets).unwrap_err(), RadarMappingError::InvalidAzimuthFov);

        let mut c = base.clone();
        c.el_max = c.el_min - 1.0;
        assert_eq!(RadarMapping::new(c).to_tensor(&dets).unwrap_err(), RadarMappingError::InvalidElevationFov);

        let mut c = base.clone();
        c.max_detections = 0;
        assert_eq!(RadarMapping::new(c).to_tensor(&dets).unwrap_err(), RadarMappingError::InvalidMaxDetections);

        let mut c = base.clone();
        c.normalization = RadarNormalization::Normalized;
        c.velocity_max = 0.0;
        assert_eq!(RadarMapping::new(c).to_tensor(&dets).unwrap_err(), RadarMappingError::InvalidNormalizationScale);
    }

    // -- Safety invariant (property) -------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// SAFETY INVARIANT — the property the governor CANNOT protect: every
        /// in-FOV detection is represented EXACTLY ONCE, in order, none lost,
        /// duplicated, or mis-placed, AND its Doppler is preserved. Polar Raw
        /// makes each row equal its detection's values verbatim, so we assert
        /// the 1:1 mapping directly; `out_of_bounds = Error` + `on_overflow =
        /// Error` make any stray/overflow detection fail loudly.
        #[test]
        fn prop_every_in_fov_detection_represented_once(
            raw in proptest::collection::vec(
                (1.0_f32..50.0, -FRAC_PI_2..FRAC_PI_2, -FRAC_PI_4..FRAC_PI_4, -30.0_f32..30.0, 0.0_f32..100.0),
                1..8,
            ),
        ) {
            let c = RadarConfig {
                representation: RadarRepresentation::DetectionList,
                feature_frame: DetectionFeatureFrame::Polar,
                range_min: 1.0, range_max: 50.0,
                az_min: -FRAC_PI_2, az_max: FRAC_PI_2,
                el_min: -FRAC_PI_4, el_max: FRAC_PI_4,
                max_detections: 8,
                on_overflow: OverflowPolicy::Error,
                elevation_policy: ElevationPolicy::Reject,
                normalization: RadarNormalization::Raw,
                velocity_max: 30.0, rcs_max: 100.0,
                layout: RadarLayout::DetectionMajor,
                out_of_bounds: OutOfBoundsPolicy::Error,
                tensor_name: "radar".to_string(),
            };
            let dets: Vec<RadarDetection> = raw.iter()
                .map(|&(r, az, el, v, rcs)| det(r, az, Some(el), v, rcs))
                .collect();
            let k = dets.len();
            let out = RadarMapping::new(c).to_tensor(&dets).expect("all in-FOV");
            let grid = out.named_tensors.get("radar").unwrap().as_slice();

            // Each in-FOV detection at its own row, verbatim (Doppler col 3).
            for (i, d) in dets.iter().enumerate() {
                let base = i * RADAR_FEATURES;
                prop_assert_eq!(grid[base], d.range);
                prop_assert_eq!(grid[base + 1], d.azimuth);
                prop_assert_eq!(grid[base + 2], d.elevation.unwrap());
                prop_assert_eq!(grid[base + 3], d.velocity); // Doppler preserved
                prop_assert_eq!(grid[base + 4], d.rcs);
            }
            // Remaining rows are zero padding — no phantom detections.
            for &cell in &grid[(k * RADAR_FEATURES)..(8 * RADAR_FEATURES)] {
                prop_assert_eq!(cell, 0.0);
            }
        }
    }
}
// ===========================================================================
// IMU — tests
// ===========================================================================

#[cfg(test)]
mod imu_tests {
    use super::*;
    use proptest::prelude::*;
    use std::f32::consts::FRAC_PI_4;

    const G: f32 = 9.80665;

    fn quat(x: f32, y: f32, z: f32, w: f32) -> Quaternion {
        Quaternion { x, y, z, w }
    }
    fn identity() -> Quaternion {
        quat(0.0, 0.0, 0.0, 1.0)
    }

    /// All blocks on, Raw, no sanity. Tests clone and tweak.
    fn base_cfg() -> ImuConfig {
        ImuConfig {
            include_acceleration: true,
            include_angular_velocity: true,
            include_orientation: Some(OdomOrientation::Quaternion),
            gravity_policy: GravityPolicy::Raw,
            gravity_mps2: G,
            normalization: ImuNormalization::Raw,
            accel_scale: 1.0,
            gyro_scale: 1.0,
            quat_norm_tolerance: 1e-3,
            sanity: None,
            tensor_name: "imu".to_string(),
        }
    }

    fn sample(accel: [f32; 3], gyro: [f32; 3], q: Option<Quaternion>) -> ImuSample {
        ImuSample { linear_acceleration: accel, angular_velocity: gyro, orientation: q }
    }

    fn out(cfg: ImuConfig, s: &ImuSample) -> Vec<f32> {
        ImuMapping::new(cfg)
            .to_tensor(s)
            .expect("valid")
            .named_tensors
            .get("imu")
            .unwrap()
            .as_slice()
            .to_vec()
    }

    // -- Gravity (the headline) -----------------------------------------

    /// GRAVITY VERIFIED, not assumed: a stationary LEVEL IMU under `Raw` shows
    /// `+G` on the +z (up) axis, passed through verbatim.
    #[test]
    fn gravity_raw_includes_gravity_on_up_axis() {
        let cfg = ImuConfig {
            include_angular_velocity: false,
            include_orientation: None,
            ..base_cfg()
        };
        let s = sample([0.0, 0.0, G], [0.0, 0.0, 0.0], None);
        assert_eq!(out(cfg, &s), vec![0.0, 0.0, G]);
    }

    /// `Compensated` with a LEVEL orientation removes gravity → ~0 on every axis.
    #[test]
    fn gravity_compensated_level_removes_gravity() {
        let cfg = ImuConfig {
            include_angular_velocity: false,
            include_orientation: None,
            gravity_policy: GravityPolicy::Compensated,
            ..base_cfg()
        };
        let s = sample([0.0, 0.0, G], [0.0, 0.0, 0.0], Some(identity()));
        let o = out(cfg, &s);
        assert!(o.iter().all(|x| x.abs() < 1e-4), "expected ~0, got {o:?}");
    }

    /// `Compensated` USES the orientation: rolled 90° about x, the stationary
    /// reading is gravity on body +y, and compensation still yields ~0 —
    /// verifying the rotation, not just the identity case.
    #[test]
    fn gravity_compensated_rolled_uses_orientation() {
        // roll +90° about x: q = (sin45, 0, 0, cos45).
        let q = quat(FRAC_PI_4.sin(), 0.0, 0.0, FRAC_PI_4.cos());
        let cfg = ImuConfig {
            include_angular_velocity: false,
            include_orientation: None,
            gravity_policy: GravityPolicy::Compensated,
            ..base_cfg()
        };
        // gravity now reads on body +y for a rolled-90° stationary IMU.
        let s = sample([0.0, G, 0.0], [0.0, 0.0, 0.0], Some(q));
        let o = out(cfg, &s);
        assert!(o.iter().all(|x| x.abs() < 1e-4), "rolled compensation expected ~0, got {o:?}");
    }

    // -- Orientation / quaternion ---------------------------------------

    #[test]
    fn quaternion_orientation_passes_through() {
        let cfg = ImuConfig {
            include_acceleration: false,
            include_angular_velocity: false,
            ..base_cfg()
        };
        let q = quat(0.0, 0.0, FRAC_PI_4.sin(), FRAC_PI_4.cos()); // yaw 90°, unit
        let s = sample([0.0; 3], [0.0; 3], Some(q));
        assert_eq!(out(cfg, &s), vec![q.x, q.y, q.z, q.w]);
    }

    /// Yaw representation reuses odom's `quat_to_euler` — a yaw-90° quaternion
    /// yields ~π/2.
    #[test]
    fn yaw_orientation_matches_quat_to_euler() {
        let cfg = ImuConfig {
            include_acceleration: false,
            include_angular_velocity: false,
            include_orientation: Some(OdomOrientation::Yaw),
            ..base_cfg()
        };
        let q = quat(0.0, 0.0, FRAC_PI_4.sin(), FRAC_PI_4.cos());
        let o = out(cfg, &s_with(q));
        assert!((o[0] - std::f32::consts::FRAC_PI_2).abs() < 1e-5, "yaw got {}", o[0]);
        assert_eq!(o.len(), 1);
    }
    fn s_with(q: Quaternion) -> ImuSample {
        sample([0.0; 3], [0.0; 3], Some(q))
    }

    #[test]
    fn non_unit_quaternion_rejected() {
        let cfg = ImuConfig { include_acceleration: false, include_angular_velocity: false, ..base_cfg() };
        let s = sample([0.0; 3], [0.0; 3], Some(quat(0.0, 0.0, 0.0, 2.0))); // norm 2
        let err = ImuMapping::new(cfg).to_tensor(&s).unwrap_err();
        assert_eq!(err, ImuMappingError::NonUnitQuaternion);
    }

    #[test]
    fn orientation_required_but_missing_rejected() {
        let cfg = ImuConfig { include_acceleration: false, include_angular_velocity: false, ..base_cfg() };
        let s = sample([0.0; 3], [0.0; 3], None);
        let err = ImuMapping::new(cfg).to_tensor(&s).unwrap_err();
        assert_eq!(err, ImuMappingError::OrientationRequiredButMissing);
    }

    #[test]
    fn compensated_without_orientation_rejected() {
        let cfg = ImuConfig {
            include_angular_velocity: false,
            include_orientation: None, // so the orientation-required check doesn't fire first
            gravity_policy: GravityPolicy::Compensated,
            ..base_cfg()
        };
        let s = sample([0.0, 0.0, G], [0.0; 3], None);
        let err = ImuMapping::new(cfg).to_tensor(&s).unwrap_err();
        assert_eq!(err, ImuMappingError::GravityCompensationNeedsOrientation);
    }

    // -- Dims / normalization / determinism -----------------------------

    #[test]
    fn output_dims_match_config() {
        // accel(3) + gyro(3) + quaternion(4) = 10.
        let s = sample([1.0, 2.0, 3.0], [0.1, 0.2, 0.3], Some(identity()));
        assert_eq!(out(base_cfg(), &s).len(), 10);
        // yaw-only orientation → accel(3)+gyro(3)+1 = 7.
        let cfg = ImuConfig { include_orientation: Some(OdomOrientation::Yaw), ..base_cfg() };
        assert_eq!(out(cfg, &s).len(), 7);
    }

    #[test]
    fn normalized_scales_accel_and_gyro_not_orientation() {
        let cfg = ImuConfig {
            normalization: ImuNormalization::Normalized,
            accel_scale: 10.0,
            gyro_scale: 2.0,
            ..base_cfg()
        };
        let s = sample([10.0, 20.0, 30.0], [2.0, 4.0, 6.0], Some(identity()));
        // accel/10, gyro/2, quaternion unchanged.
        assert_eq!(out(cfg, &s), vec![1.0, 2.0, 3.0, 1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn is_deterministic() {
        let s = sample([0.5, -1.0, G], [0.1, -0.2, 0.3], Some(identity()));
        assert_eq!(out(base_cfg(), &s), out(base_cfg(), &s));
    }

    // -- Fail-closed -----------------------------------------------------

    #[test]
    fn non_finite_sample_rejected() {
        let cfgs_samples = [
            sample([f32::NAN, 0.0, 0.0], [0.0; 3], Some(identity())),
            sample([0.0; 3], [0.0, f32::INFINITY, 0.0], Some(identity())),
            sample([0.0; 3], [0.0; 3], Some(quat(f32::NAN, 0.0, 0.0, 1.0))),
        ];
        for s in cfgs_samples {
            let err = ImuMapping::new(base_cfg()).to_tensor(&s).unwrap_err();
            assert_eq!(err, ImuMappingError::NonFiniteSample);
        }
    }

    #[test]
    fn malformed_config_rejected() {
        let s = sample([0.0, 0.0, G], [0.0; 3], Some(identity()));

        let c = ImuConfig {
            include_acceleration: false, include_angular_velocity: false, include_orientation: None,
            ..base_cfg()
        };
        assert_eq!(ImuMapping::new(c).to_tensor(&s).unwrap_err(), ImuMappingError::EmptyFeatureSet);

        let c = ImuConfig { normalization: ImuNormalization::Normalized, accel_scale: 0.0, ..base_cfg() };
        assert_eq!(ImuMapping::new(c).to_tensor(&s).unwrap_err(), ImuMappingError::InvalidNormalizationScale);

        let c = ImuConfig { gravity_policy: GravityPolicy::Compensated, gravity_mps2: 0.0, ..base_cfg() };
        assert_eq!(ImuMapping::new(c).to_tensor(&s).unwrap_err(), ImuMappingError::InvalidGravity);

        let c = ImuConfig { quat_norm_tolerance: -1.0, ..base_cfg() };
        assert_eq!(ImuMapping::new(c).to_tensor(&s).unwrap_err(), ImuMappingError::InvalidQuatTolerance);

        let c = ImuConfig { sanity: Some(ImuSanityBound { max_accel_mps2: 0.0, max_gyro_rad_s: 1.0 }), ..base_cfg() };
        assert_eq!(ImuMapping::new(c).to_tensor(&s).unwrap_err(), ImuMappingError::InvalidSanityBound);
    }

    #[test]
    fn sanity_bound_rejects_implausible() {
        let cfg = ImuConfig {
            sanity: Some(ImuSanityBound { max_accel_mps2: 50.0, max_gyro_rad_s: 10.0 }),
            ..base_cfg()
        };
        // 200 m/s² accel magnitude >> 50 → rejected, NOT clipped.
        let s = sample([200.0, 0.0, 0.0], [0.0; 3], Some(identity()));
        assert_eq!(ImuMapping::new(cfg.clone()).to_tensor(&s).unwrap_err(), ImuMappingError::ImplausibleAcceleration);
        // huge gyro.
        let s2 = sample([0.0, 0.0, G], [100.0, 0.0, 0.0], Some(identity()));
        assert_eq!(ImuMapping::new(cfg).to_tensor(&s2).unwrap_err(), ImuMappingError::ImplausibleAngularVelocity);
    }

    // -- Field-fidelity invariant (property) -----------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// LOAD-BEARING INVARIANT (state-vector analog of the spatial in-bounds
        /// invariant): every selected input component appears at its correct
        /// output index, carried through faithfully — nothing lost, swapped, or
        /// corrupted. Raw + no compensation + quaternion repr makes the output
        /// exactly [accel(3), gyro(3), quat(4)] verbatim.
        #[test]
        fn prop_field_fidelity(
            ax in -50.0_f32..50.0, ay in -50.0_f32..50.0, az in -50.0_f32..50.0,
            gx in -20.0_f32..20.0, gy in -20.0_f32..20.0, gz in -20.0_f32..20.0,
            qx in -1.0_f32..1.0, qy in -1.0_f32..1.0, qz in -1.0_f32..1.0, qw in 0.5_f32..1.5,
        ) {
            // Normalize to a unit quaternion (norm > 0.5 by construction).
            let n = (qx*qx + qy*qy + qz*qz + qw*qw).sqrt();
            let q = quat(qx/n, qy/n, qz/n, qw/n);
            let s = sample([ax, ay, az], [gx, gy, gz], Some(q));
            let o = out(base_cfg(), &s);
            prop_assert_eq!(o.len(), 10);
            // accel verbatim at 0..3
            prop_assert_eq!(o[0], ax); prop_assert_eq!(o[1], ay); prop_assert_eq!(o[2], az);
            // gyro verbatim at 3..6
            prop_assert_eq!(o[3], gx); prop_assert_eq!(o[4], gy); prop_assert_eq!(o[5], gz);
            // quaternion verbatim at 6..10 (unit, passes validation)
            prop_assert_eq!(o[6], q.x); prop_assert_eq!(o[7], q.y);
            prop_assert_eq!(o[8], q.z); prop_assert_eq!(o[9], q.w);
        }
    }
}
