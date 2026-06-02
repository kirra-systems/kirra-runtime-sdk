// parko/crates/parko-ros2/src/pointcloud2_shim.rs
//
// PointCloud2 → Vec<LidarPoint> extraction shim — the deferred ROS half of the
// LiDAR mapping, and the FIRST of the parko-ros2 sensor shims. The later shims
// (Image, Odometry, Imu, Radar) follow this template:
//
//   PURE DECODE CORE  (this file, NON-gated, always compiled, unit-tested)
//     + THIN r2r ADAPTER (`#[cfg(feature = "ros2")]`, extraction only).
//
// SAFETY FRAMING. This shim sits UPSTREAM of the LiDAR pure transform, which
// sits upstream of the governor. A PointCloud2 decode that reads the wrong
// field offset, datatype, or endianness silently misreads EVERY point's
// coordinates — feeding the model corrupted geometry that yields a confidently
// wrong, in-bounds command the governor cannot catch. The byte-layout decode is
// THE correctness surface; it must be exactly right and it must be the TESTED
// artifact. That is why the decoder is pure and r2r-free: r2r needs a sourced
// ROS environment to build, so a decoder buried in the r2r adapter could never
// be unit-tested and its risk would be unverifiable.
//
// FLAGGED DECISIONS (surfaced, not silently chosen):
//   - REQUIRED vs OPTIONAL fields: x, y, z are REQUIRED (a missing one is a
//     fail-closed reject); intensity is OPTIONAL (absent → 0.0, matching the
//     LiDAR transform's `LidarPoint.intensity`). Flagged: the 0.0-when-absent
//     default is documented, not silent.
//   - COORDINATE DATATYPES: x/y/z accept FLOAT32 and FLOAT64 only. Integer-typed
//     coordinates are REJECTED (no lossy guess) per the recommendation — a
//     fixed-point integer coordinate needs an explicit scale the message does
//     not carry, so decoding it as a raw integer would silently mis-place every
//     point. Intensity is more permissive (it is a magnitude, not geometry):
//     FLOAT32/FLOAT64 plus the integer types, widened to f32.
//   - ROW_STEP: honored (padding between rows is legal). `row_step` must be
//     >= width*point_step; a smaller value (rows would overlap) is rejected.
//     `row_step == 0` is treated as the packed `width*point_step`.

// The decoder outputs the LiDAR transform's EXACT input point. LiDAR mapping
// landed in main (#150), so this references `sensor_mapping::LidarPoint`
// directly — the prior byte-identical mirror has been collapsed away.
use crate::sensor_mapping::LidarPoint;

// sensor_msgs/PointField datatype codes.
pub const INT8: u8 = 1;
pub const UINT8: u8 = 2;
pub const INT16: u8 = 3;
pub const UINT16: u8 = 4;
pub const INT32: u8 = 5;
pub const UINT32: u8 = 6;
pub const FLOAT32: u8 = 7;
pub const FLOAT64: u8 = 8;

/// Plain mirror of a ROS `PointField` — NOT the r2r type, so the core stays
/// r2r-free and unit-testable without a ROS environment.
#[derive(Debug, Clone, PartialEq)]
pub struct PointFieldDesc {
    pub name: String,
    pub offset: u32,
    pub datatype: u8,
    pub count: u32,
}

/// Fail-closed decode errors. Sibling type, disjoint from the mapping error
/// enums (`CameraMappingError` / `LidarMappingError` / …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointCloud2DecodeError {
    /// `point_step == 0` — a degenerate stride.
    InvalidPointStep,
    /// A required coordinate field (`x`/`y`/`z`) is absent from `fields`.
    MissingRequiredField { axis: &'static str },
    /// A field name appears more than once — ambiguous layout.
    DuplicateField { name: String },
    /// A coordinate field uses a non-float datatype (integer coordinates are
    /// rejected — no lossy guess).
    UnsupportedCoordinateDatatype { axis: &'static str, datatype: u8 },
    /// A read field uses a datatype with no known size.
    UnsupportedFieldDatatype { name: String, datatype: u8 },
    /// A field's `offset + size` exceeds `point_step` — it cannot fit a point.
    FieldExceedsPointStep { name: String },
    /// `row_step` is smaller than `width * point_step` (rows would overlap).
    InvalidRowStep,
    /// The buffer is shorter than the layout requires — never read past the end
    /// or fabricate points.
    TruncatedBuffer { needed: usize, got: usize },
    /// A decoded coordinate or intensity is NaN/Inf — a non-finite point must
    /// never reach the transform.
    NonFinitePoint { index: usize },
    /// `width * height` (or a derived size) overflowed `usize`.
    DimensionOverflow,
}

/// Byte width of a `PointField` datatype, or `None` if unknown.
fn datatype_size(datatype: u8) -> Option<usize> {
    match datatype {
        INT8 | UINT8 => Some(1),
        INT16 | UINT16 => Some(2),
        INT32 | UINT32 | FLOAT32 => Some(4),
        FLOAT64 => Some(8),
        _ => None,
    }
}

fn is_float_datatype(datatype: u8) -> bool {
    datatype == FLOAT32 || datatype == FLOAT64
}

/// Read one numeric field at `at` as `f32`, honoring datatype + endianness.
/// Caller guarantees `at + datatype_size(datatype) <= data.len()`.
fn read_value_f32(data: &[u8], at: usize, datatype: u8, be: bool) -> f32 {
    match datatype {
        FLOAT32 => {
            let mut b = [0u8; 4];
            b.copy_from_slice(&data[at..at + 4]);
            if be { f32::from_be_bytes(b) } else { f32::from_le_bytes(b) }
        }
        FLOAT64 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[at..at + 8]);
            (if be { f64::from_be_bytes(b) } else { f64::from_le_bytes(b) }) as f32
        }
        UINT8 => data[at] as f32,
        INT8 => (data[at] as i8) as f32,
        UINT16 => {
            let mut b = [0u8; 2];
            b.copy_from_slice(&data[at..at + 2]);
            (if be { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }) as f32
        }
        INT16 => {
            let mut b = [0u8; 2];
            b.copy_from_slice(&data[at..at + 2]);
            (if be { i16::from_be_bytes(b) } else { i16::from_le_bytes(b) }) as f32
        }
        UINT32 => {
            let mut b = [0u8; 4];
            b.copy_from_slice(&data[at..at + 4]);
            (if be { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }) as f32
        }
        INT32 => {
            let mut b = [0u8; 4];
            b.copy_from_slice(&data[at..at + 4]);
            (if be { i32::from_be_bytes(b) } else { i32::from_le_bytes(b) }) as f32
        }
        // Unreachable: every field's datatype is validated before any read.
        _ => f32::NAN,
    }
}

/// Find a field by name, rejecting duplicates.
fn find_unique<'a>(
    fields: &'a [PointFieldDesc],
    name: &str,
) -> Result<Option<&'a PointFieldDesc>, PointCloud2DecodeError> {
    let mut found: Option<&PointFieldDesc> = None;
    for f in fields {
        if f.name == name {
            if found.is_some() {
                return Err(PointCloud2DecodeError::DuplicateField { name: name.to_string() });
            }
            found = Some(f);
        }
    }
    Ok(found)
}

fn ensure_fits(f: &PointFieldDesc, point_step: usize) -> Result<(), PointCloud2DecodeError> {
    let size = datatype_size(f.datatype).ok_or_else(|| {
        PointCloud2DecodeError::UnsupportedFieldDatatype { name: f.name.clone(), datatype: f.datatype }
    })?;
    if (f.offset as usize) + size > point_step {
        return Err(PointCloud2DecodeError::FieldExceedsPointStep { name: f.name.clone() });
    }
    Ok(())
}

/// Decode a raw PointCloud2 buffer into `Vec<LidarPoint>`. FIELD-DESCRIPTOR
/// DRIVEN: `x`/`y`/`z` (+ optional `intensity`) are located BY NAME and read at
/// their own offset with their own datatype — no positional/offset assumptions,
/// so a different sensor's layout decodes correctly rather than being misread.
pub fn decode_pointcloud2(
    data: &[u8],
    fields: &[PointFieldDesc],
    point_step: u32,
    width: u32,
    height: u32,
    row_step: u32,
    is_bigendian: bool,
) -> Result<Vec<LidarPoint>, PointCloud2DecodeError> {
    if point_step == 0 {
        return Err(PointCloud2DecodeError::InvalidPointStep);
    }
    let point_step = point_step as usize;

    // Locate required coords + optional intensity by name (dup-checked).
    let fx = find_unique(fields, "x")?
        .ok_or(PointCloud2DecodeError::MissingRequiredField { axis: "x" })?;
    let fy = find_unique(fields, "y")?
        .ok_or(PointCloud2DecodeError::MissingRequiredField { axis: "y" })?;
    let fz = find_unique(fields, "z")?
        .ok_or(PointCloud2DecodeError::MissingRequiredField { axis: "z" })?;
    let fi = find_unique(fields, "intensity")?;

    // Coordinates must be float (no lossy integer-coordinate guess).
    for (axis, f) in [("x", fx), ("y", fy), ("z", fz)] {
        if !is_float_datatype(f.datatype) {
            return Err(PointCloud2DecodeError::UnsupportedCoordinateDatatype {
                axis,
                datatype: f.datatype,
            });
        }
    }
    // Intensity: any datatype with a known size (widened to f32).
    if let Some(f) = fi {
        if datatype_size(f.datatype).is_none() {
            return Err(PointCloud2DecodeError::UnsupportedFieldDatatype {
                name: "intensity".to_string(),
                datatype: f.datatype,
            });
        }
    }

    // Every read field must fit inside one point's stride.
    ensure_fits(fx, point_step)?;
    ensure_fits(fy, point_step)?;
    ensure_fits(fz, point_step)?;
    if let Some(f) = fi {
        ensure_fits(f, point_step)?;
    }

    let width = width as usize;
    let height = height as usize;
    let point_count = width.checked_mul(height).ok_or(PointCloud2DecodeError::DimensionOverflow)?;

    // Row stride: honor padding between rows; reject overlap.
    let packed_row = width
        .checked_mul(point_step)
        .ok_or(PointCloud2DecodeError::DimensionOverflow)?;
    let eff_row = if row_step == 0 { packed_row } else { row_step as usize };
    if eff_row < packed_row {
        return Err(PointCloud2DecodeError::InvalidRowStep);
    }

    // Buffer must hold every point (row_step * height) — never read past end.
    let needed = height.checked_mul(eff_row).ok_or(PointCloud2DecodeError::DimensionOverflow)?;
    if data.len() < needed {
        return Err(PointCloud2DecodeError::TruncatedBuffer { needed, got: data.len() });
    }

    let mut out = Vec::with_capacity(point_count);
    for row in 0..height {
        let row_base = row * eff_row;
        for col in 0..width {
            let base = row_base + col * point_step;
            let x = read_value_f32(data, base + fx.offset as usize, fx.datatype, is_bigendian);
            let y = read_value_f32(data, base + fy.offset as usize, fy.datatype, is_bigendian);
            let z = read_value_f32(data, base + fz.offset as usize, fz.datatype, is_bigendian);
            let intensity = match fi {
                Some(f) => read_value_f32(data, base + f.offset as usize, f.datatype, is_bigendian),
                None => 0.0,
            };
            // Fail closed: a non-finite point must never reach the transform.
            if !x.is_finite() || !y.is_finite() || !z.is_finite() || !intensity.is_finite() {
                return Err(PointCloud2DecodeError::NonFinitePoint { index: row * width + col });
            }
            out.push(LidarPoint { x, y, z, intensity });
        }
    }
    Ok(out)
}

// ===========================================================================
// THIN r2r ADAPTER — ros2-gated, extraction only (ZERO decode logic)
// ===========================================================================

/// Map an r2r `sensor_msgs/PointCloud2` onto the pure decoder. Pulls the fields
/// off the message, mirrors each `PointField` into a `PointFieldDesc`, and calls
/// `decode_pointcloud2`. All correctness lives in the pure core. Compiles only
/// under `--features ros2` in a sourced ROS environment.
#[cfg(feature = "ros2")]
pub fn decode_r2r_pointcloud2(
    msg: &r2r::sensor_msgs::msg::PointCloud2,
) -> Result<Vec<LidarPoint>, PointCloud2DecodeError> {
    let fields: Vec<PointFieldDesc> = msg
        .fields
        .iter()
        .map(|f| PointFieldDesc {
            name: f.name.clone(),
            offset: f.offset,
            datatype: f.datatype,
            count: f.count,
        })
        .collect();
    decode_pointcloud2(
        &msg.data,
        &fields,
        msg.point_step,
        msg.width,
        msg.height,
        msg.row_step,
        msg.is_bigendian,
    )
}

// ===========================================================================
// Tests — pure, no ROS (the load-bearing artifact)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fd(name: &str, offset: u32, datatype: u8) -> PointFieldDesc {
        PointFieldDesc { name: name.to_string(), offset, datatype, count: 1 }
    }

    /// Append a value as little-endian f32 to `buf`.
    fn push_f32(buf: &mut Vec<u8>, v: f32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Canonical layout: FLOAT32 x,y,z at 0/4/8, intensity at 12, point_step 16.
    #[test]
    fn decodes_canonical_float32_layout() {
        let mut data = Vec::new();
        for &(x, y, z, i) in &[(1.0_f32, 2.0, 3.0, 10.0), (-4.5, 5.5, 6.5, 20.0)] {
            push_f32(&mut data, x);
            push_f32(&mut data, y);
            push_f32(&mut data, z);
            push_f32(&mut data, i);
        }
        let fields = vec![
            fd("x", 0, FLOAT32),
            fd("y", 4, FLOAT32),
            fd("z", 8, FLOAT32),
            fd("intensity", 12, FLOAT32),
        ];
        let pts = decode_pointcloud2(&data, &fields, 16, 2, 1, 0, false).unwrap();
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0], LidarPoint { x: 1.0, y: 2.0, z: 3.0, intensity: 10.0 });
        assert_eq!(pts[1], LidarPoint { x: -4.5, y: 5.5, z: 6.5, intensity: 20.0 });
    }

    /// `point_step` larger than the packed fields (trailing padding) must be
    /// honored — points start every `point_step` bytes, padding ignored.
    #[test]
    fn honors_point_step_padding() {
        let point_step = 32; // 16 bytes of fields + 16 bytes padding
        let mut data = Vec::new();
        for &(x, y, z, i) in &[(1.0_f32, 2.0, 3.0, 7.0), (8.0, 9.0, 10.0, 11.0)] {
            push_f32(&mut data, x);
            push_f32(&mut data, y);
            push_f32(&mut data, z);
            push_f32(&mut data, i);
            data.extend_from_slice(&[0u8; 16]); // padding
        }
        let fields = vec![
            fd("x", 0, FLOAT32),
            fd("y", 4, FLOAT32),
            fd("z", 8, FLOAT32),
            fd("intensity", 12, FLOAT32),
        ];
        let pts = decode_pointcloud2(&data, &fields, point_step, 2, 1, 0, false).unwrap();
        assert_eq!(pts[1], LidarPoint { x: 8.0, y: 9.0, z: 10.0, intensity: 11.0 });
    }

    /// Fields declared OUT OF OFFSET ORDER (and not positionally) — proves the
    /// decoder uses name+offset lookup, not a positional assumption. Layout:
    /// y@0, z@4, x@8.
    #[test]
    fn uses_name_offset_lookup_not_position() {
        // data point0: y=8.0 @0, z=9.0 @4, x=7.0 @8
        let mut data = Vec::new();
        push_f32(&mut data, 8.0); // y at 0
        push_f32(&mut data, 9.0); // z at 4
        push_f32(&mut data, 7.0); // x at 8
        let fields = vec![
            fd("z", 4, FLOAT32), // declared out of order too
            fd("x", 8, FLOAT32),
            fd("y", 0, FLOAT32),
        ];
        let pts = decode_pointcloud2(&data, &fields, 12, 1, 1, 0, false).unwrap();
        assert_eq!(pts[0], LidarPoint { x: 7.0, y: 8.0, z: 9.0, intensity: 0.0 });
    }

    /// BIG-ENDIAN multi-byte reads.
    #[test]
    fn decodes_big_endian() {
        let mut data = Vec::new();
        data.extend_from_slice(&1.25_f32.to_be_bytes());
        data.extend_from_slice(&(-2.5_f32).to_be_bytes());
        data.extend_from_slice(&3.75_f32.to_be_bytes());
        let fields = vec![fd("x", 0, FLOAT32), fd("y", 4, FLOAT32), fd("z", 8, FLOAT32)];
        let pts = decode_pointcloud2(&data, &fields, 12, 1, 1, 0, true).unwrap();
        assert_eq!(pts[0], LidarPoint { x: 1.25, y: -2.5, z: 3.75, intensity: 0.0 });
    }

    /// FLOAT64 coordinates (cast to f32).
    #[test]
    fn decodes_float64_coordinates() {
        let mut data = Vec::new();
        data.extend_from_slice(&1.5_f64.to_le_bytes());
        data.extend_from_slice(&2.5_f64.to_le_bytes());
        data.extend_from_slice(&(-3.5_f64).to_le_bytes());
        let fields = vec![fd("x", 0, FLOAT64), fd("y", 8, FLOAT64), fd("z", 16, FLOAT64)];
        let pts = decode_pointcloud2(&data, &fields, 24, 1, 1, 0, false).unwrap();
        assert_eq!(pts[0], LidarPoint { x: 1.5, y: 2.5, z: -3.5, intensity: 0.0 });
    }

    /// Integer intensity (UINT8) is accepted and widened; coordinates stay float.
    #[test]
    fn accepts_integer_intensity_widened() {
        let mut data = Vec::new();
        push_f32(&mut data, 1.0);
        push_f32(&mut data, 2.0);
        push_f32(&mut data, 3.0);
        data.push(200u8); // intensity @12, UINT8
        let fields = vec![
            fd("x", 0, FLOAT32),
            fd("y", 4, FLOAT32),
            fd("z", 8, FLOAT32),
            fd("intensity", 12, UINT8),
        ];
        let pts = decode_pointcloud2(&data, &fields, 13, 1, 1, 0, false).unwrap();
        assert_eq!(pts[0].intensity, 200.0);
    }

    /// row_step PADDING between rows is honored.
    #[test]
    fn honors_row_step_padding() {
        let point_step = 12u32; // x,y,z float32
        let width = 2u32;
        let row_step = 32u32; // 24 bytes of points + 8 bytes row padding
        let height = 2u32;
        let mut data = vec![0u8; (row_step * height) as usize];
        // row 0: p(0,0)@0, p(0,1)@12 ; row 1 base @32
        let write = |buf: &mut Vec<u8>, at: usize, x: f32, y: f32, z: f32| {
            buf[at..at + 4].copy_from_slice(&x.to_le_bytes());
            buf[at + 4..at + 8].copy_from_slice(&y.to_le_bytes());
            buf[at + 8..at + 12].copy_from_slice(&z.to_le_bytes());
        };
        write(&mut data, 0, 1.0, 1.1, 1.2);
        write(&mut data, 12, 2.0, 2.1, 2.2);
        write(&mut data, 32, 3.0, 3.1, 3.2);
        write(&mut data, 44, 4.0, 4.1, 4.2);
        let fields = vec![fd("x", 0, FLOAT32), fd("y", 4, FLOAT32), fd("z", 8, FLOAT32)];
        let pts = decode_pointcloud2(&data, &fields, point_step, width, height, row_step, false).unwrap();
        assert_eq!(pts.len(), 4);
        assert_eq!(pts[2].x, 3.0); // first point of row 1, after the padding
        assert_eq!(pts[3].x, 4.0);
    }

    /// LOAD-BEARING INVARIANT: width*height in-buffer points decode to exactly
    /// that many output points, each at its decoded coordinate.
    #[test]
    fn point_count_invariant() {
        let (w, h) = (3u32, 2u32);
        let mut data = Vec::new();
        for i in 0..(w * h) {
            push_f32(&mut data, i as f32);
            push_f32(&mut data, (i as f32) + 0.5);
            push_f32(&mut data, (i as f32) + 0.25);
        }
        let fields = vec![fd("x", 0, FLOAT32), fd("y", 4, FLOAT32), fd("z", 8, FLOAT32)];
        let pts = decode_pointcloud2(&data, &fields, 12, w, h, 0, false).unwrap();
        assert_eq!(pts.len(), (w * h) as usize);
        for (i, p) in pts.iter().enumerate() {
            assert_eq!(p.x, i as f32);
            assert_eq!(p.y, i as f32 + 0.5);
        }
    }

    // -- Fail-closed -----------------------------------------------------

    fn xyz_fields() -> Vec<PointFieldDesc> {
        vec![fd("x", 0, FLOAT32), fd("y", 4, FLOAT32), fd("z", 8, FLOAT32)]
    }

    #[test]
    fn rejects_truncated_buffer() {
        let data = vec![0u8; 11]; // one point needs 12
        let err = decode_pointcloud2(&data, &xyz_fields(), 12, 1, 1, 0, false).unwrap_err();
        assert!(matches!(err, PointCloud2DecodeError::TruncatedBuffer { .. }));
    }

    #[test]
    fn rejects_missing_required_field() {
        let data = vec![0u8; 12];
        let fields = vec![fd("x", 0, FLOAT32), fd("y", 4, FLOAT32)]; // no z
        assert_eq!(
            decode_pointcloud2(&data, &fields, 12, 1, 1, 0, false).unwrap_err(),
            PointCloud2DecodeError::MissingRequiredField { axis: "z" }
        );
    }

    #[test]
    fn rejects_duplicate_field() {
        let data = vec![0u8; 16];
        let mut fields = xyz_fields();
        fields.push(fd("x", 12, FLOAT32)); // x twice
        assert_eq!(
            decode_pointcloud2(&data, &fields, 16, 1, 1, 0, false).unwrap_err(),
            PointCloud2DecodeError::DuplicateField { name: "x".to_string() }
        );
    }

    #[test]
    fn rejects_integer_coordinate_datatype() {
        let data = vec![0u8; 12];
        let fields = vec![fd("x", 0, UINT32), fd("y", 4, FLOAT32), fd("z", 8, FLOAT32)];
        assert_eq!(
            decode_pointcloud2(&data, &fields, 12, 1, 1, 0, false).unwrap_err(),
            PointCloud2DecodeError::UnsupportedCoordinateDatatype { axis: "x", datatype: UINT32 }
        );
    }

    #[test]
    fn rejects_nan_coordinate() {
        let mut data = Vec::new();
        push_f32(&mut data, f32::NAN);
        push_f32(&mut data, 1.0);
        push_f32(&mut data, 2.0);
        assert_eq!(
            decode_pointcloud2(&data, &xyz_fields(), 12, 1, 1, 0, false).unwrap_err(),
            PointCloud2DecodeError::NonFinitePoint { index: 0 }
        );
    }

    #[test]
    fn rejects_field_exceeding_point_step() {
        // x is FLOAT32 (4 bytes) at offset 0, but point_step is only 2.
        let data = vec![0u8; 8];
        assert_eq!(
            decode_pointcloud2(&data, &xyz_fields(), 2, 1, 1, 0, false).unwrap_err(),
            PointCloud2DecodeError::FieldExceedsPointStep { name: "x".to_string() }
        );
    }

    #[test]
    fn rejects_zero_point_step() {
        let data = vec![0u8; 12];
        assert_eq!(
            decode_pointcloud2(&data, &xyz_fields(), 0, 1, 1, 0, false).unwrap_err(),
            PointCloud2DecodeError::InvalidPointStep
        );
    }

    #[test]
    fn rejects_row_step_smaller_than_packed_row() {
        let data = vec![0u8; 48];
        // width 2 * point_step 12 = 24 packed, but row_step 20 < 24 → overlap.
        assert_eq!(
            decode_pointcloud2(&data, &xyz_fields(), 12, 2, 1, 20, false).unwrap_err(),
            PointCloud2DecodeError::InvalidRowStep
        );
    }
}
