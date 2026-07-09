// parko/crates/parko-ros2/src/image_shim.rs
//
// sensor_msgs/Image → OwnedCameraSample extraction shim — the deferred ROS half
// of the CAMERA mapping. Same template as the PointCloud2 shim: a PURE DECODE
// CORE (no r2r, always compiled, unit-tested) + a THIN r2r ADAPTER
// (`#[cfg(feature = "ros2")]`, extraction only).
//
// SAFETY FRAMING. The shim sits upstream of the camera transform → model →
// governor. Two silent-corruption surfaces, each yielding a confidently wrong,
// in-bounds command the governor cannot catch:
//   1. ENCODING MISMATCH — feeding a `bgr8` image to a transform configured for
//      `rgb8` swaps R and B with no error (the transform's "classic bug"). The
//      shim RECONCILES the message's declared encoding against the configured
//      one and REJECTS a mismatch — never a silent reinterpretation, because the
//      transform trusts whatever bytes arrive under its configured order.
//   2. ROW STRIDE — the ROS Image `step` (full row byte length) may exceed
//      `width * bytes_per_pixel` (alignment padding). Assuming a packed buffer
//      misaligns every row after the first. The shim DE-STRIDES: it honors
//      `step` and emits the tight, packed buffer the transform requires.
//
// NOTE: the camera types (`OwnedCameraSample`, `CameraEncoding`) live in `main`,
// so this shim references them DIRECTLY — no mirror, no collapse-at-merge
// (unlike the PointCloud2 shim's `LidarPoint`). Encoding / channel order /
// normalization are OWNED by the transform via `CameraConfig.encoding`; the shim
// only reconciles + de-strides, it does not reinterpret pixels.
//
// FLAGGED DECISIONS (surfaced, not silently chosen):
//   - SUPPORTED ENCODINGS: only the 8-bit set `CameraEncoding` actually models —
//     `rgb8` / `bgr8` / `mono8`. Anything the transform can't interpret (yuv422,
//     mono16 / 16UCx, 32FCx, rgba8, ...) is REJECTED (`UnsupportedEncoding`), no
//     lossy guess.
//   - ENDIANNESS: the supported set is 8-bit (one byte per channel), so
//     `is_bigendian` is MOOT — there is no multi-byte value to order. It is
//     accepted for signature parity and ignored; multi-byte encodings are
//     rejected as unsupported rather than carrying a byte-order path the
//     transform cannot use.
//   - RECONCILIATION STRICTNESS: the message encoding must map to EXACTLY the
//     configured `expected` (same channel order AND count). A looser match is
//     deliberately not allowed — a channel-order or channel-count difference is
//     a hard reject.

use crate::sensor_mapping::{CameraEncoding, OwnedCameraSample};

/// Fail-closed decode errors. Sibling type, disjoint from `CameraMappingError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageDecodeError {
    /// `width == 0` or `height == 0`.
    InvalidDimensions { width: u32, height: u32 },
    /// The message `encoding` string is not one the transform models.
    UnsupportedEncoding { encoding: String },
    /// The message encoding is supported but differs from the configured one
    /// (channel order and/or count) — the channel-swap guard.
    EncodingMismatch {
        message: String,
        expected: CameraEncoding,
    },
    /// `step` is smaller than `width * bytes_per_pixel` — a row cannot even hold
    /// its tight pixels.
    InvalidStep { step: usize, tight_row: usize },
    /// The buffer is shorter than `step * height` — never read past the end or
    /// fabricate pixels.
    TruncatedBuffer { needed: usize, got: usize },
}

/// Map a ROS `sensor_msgs/Image` encoding string to the `CameraEncoding` the
/// transform models. Only the 8-bit set is supported; everything else returns
/// `None` (→ `UnsupportedEncoding`).
fn parse_encoding(s: &str) -> Option<CameraEncoding> {
    match s {
        "rgb8" => Some(CameraEncoding::Rgb8),
        "bgr8" => Some(CameraEncoding::Bgr8),
        "mono8" => Some(CameraEncoding::Mono8),
        _ => None,
    }
}

/// Decode a raw ROS Image buffer into the tight `OwnedCameraSample` the camera
/// transform consumes (`bytes.len() == width * height * channels`,
/// `src_width`/`src_height` carried through). Reconciles encoding and de-strides.
pub fn decode_image(
    data: &[u8],
    encoding: &str,
    width: u32,
    height: u32,
    step: u32,
    is_bigendian: bool,
    expected: CameraEncoding,
) -> Result<OwnedCameraSample, ImageDecodeError> {
    // 8-bit-only supported set → endianness is moot (see module docs).
    let _ = is_bigendian;

    if width == 0 || height == 0 {
        return Err(ImageDecodeError::InvalidDimensions { width, height });
    }

    // Reconcile the declared encoding against the configured one.
    let message_encoding =
        parse_encoding(encoding).ok_or_else(|| ImageDecodeError::UnsupportedEncoding {
            encoding: encoding.to_string(),
        })?;
    if message_encoding != expected {
        return Err(ImageDecodeError::EncodingMismatch {
            message: encoding.to_string(),
            expected,
        });
    }

    let channels = expected.channels(); // 8-bit → bytes_per_pixel == channels
    let width_us = width as usize;
    let height_us = height as usize;
    let tight_row = width_us * channels;
    let step_us = step as usize;

    if step_us < tight_row {
        return Err(ImageDecodeError::InvalidStep {
            step: step_us,
            tight_row,
        });
    }

    // `step * height` is the strided buffer length the message must carry.
    // Guard the multiply so an absurd (width,height) can't overflow into a
    // small `needed` that passes the length check.
    let needed = match step_us.checked_mul(height_us) {
        Some(n) => n,
        None => {
            return Err(ImageDecodeError::TruncatedBuffer {
                needed: usize::MAX,
                got: data.len(),
            })
        }
    };
    if data.len() < needed {
        return Err(ImageDecodeError::TruncatedBuffer {
            needed,
            got: data.len(),
        });
    }

    // De-stride: copy each row's tight pixels, dropping per-row padding.
    let mut out = Vec::with_capacity(tight_row * height_us);
    for r in 0..height_us {
        let start = r * step_us;
        out.extend_from_slice(&data[start..start + tight_row]);
    }

    Ok(OwnedCameraSample {
        bytes: out,
        src_width: width,
        src_height: height,
    })
}

// ===========================================================================
// THIN r2r ADAPTER — ros2-gated, extraction only (ZERO decode logic)
// ===========================================================================

/// Map an r2r `sensor_msgs/Image` onto the pure decoder, passing the configured
/// `expected` encoding. All correctness lives in the pure core. Compiles only
/// under `--features ros2` in a sourced ROS environment.
#[cfg(feature = "ros2")]
pub fn decode_r2r_image(
    msg: &r2r::sensor_msgs::msg::Image,
    expected: CameraEncoding,
) -> Result<OwnedCameraSample, ImageDecodeError> {
    decode_image(
        &msg.data,
        &msg.encoding,
        msg.width,
        msg.height,
        msg.step,
        msg.is_bigendian != 0,
        expected,
    )
}

// ===========================================================================
// Tests — pure, no ROS (the load-bearing artifact)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// DE-STRIDE with per-row padding: mono8 2×2, step 4 (2 bytes padding/row).
    /// The padding bytes are 0xFF so a stride bug (reading packed) would surface
    /// them; the SECOND row landing correctly is the test that catches it.
    #[test]
    fn destrides_padded_rows_second_row_correct() {
        // row0: 10,11 | FF,FF   row1: 20,21 | FF,FF
        let data = vec![10, 11, 0xFF, 0xFF, 20, 21, 0xFF, 0xFF];
        let s = decode_image(&data, "mono8", 2, 2, 4, false, CameraEncoding::Mono8).unwrap();
        assert_eq!(s.bytes, vec![10, 11, 20, 21]);
        assert_eq!((s.src_width, s.src_height), (2, 2));
    }

    /// No padding (step == width*bpp) decodes to the identical buffer.
    #[test]
    fn no_padding_is_identity() {
        let data = vec![1, 2, 3, 4, 5, 6]; // 2x3 mono8, tight
        let s = decode_image(&data, "mono8", 3, 2, 3, false, CameraEncoding::Mono8).unwrap();
        assert_eq!(s.bytes, data);
    }

    /// rgb8 with padding: 2×2, tight_row 6, step 8. Second row must start at the
    /// 7th OUTPUT byte (de-strided), i.e. the row-1 pixels, not padding.
    #[test]
    fn destrides_rgb8_with_padding() {
        let mut data = Vec::new();
        data.extend_from_slice(&[1, 2, 3, 4, 5, 6]); // row0 pixels
        data.extend_from_slice(&[0xFF, 0xFF]); // row0 padding
        data.extend_from_slice(&[7, 8, 9, 10, 11, 12]); // row1 pixels
        data.extend_from_slice(&[0xFF, 0xFF]); // row1 padding
        let s = decode_image(&data, "rgb8", 2, 2, 8, false, CameraEncoding::Rgb8).unwrap();
        assert_eq!(s.bytes.len(), 2 * 2 * 3);
        assert_eq!(s.bytes[6], 7, "second row must start at output byte 6");
        assert_eq!(&s.bytes[..], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    // -- Encoding reconcile ---------------------------------------------

    #[test]
    fn matching_encoding_ok() {
        let data = vec![0u8; 12];
        assert!(decode_image(&data, "rgb8", 2, 2, 6, false, CameraEncoding::Rgb8).is_ok());
    }

    #[test]
    fn bgr8_message_when_expected_rgb8_is_mismatch() {
        let data = vec![0u8; 12];
        let err = decode_image(&data, "bgr8", 2, 2, 6, false, CameraEncoding::Rgb8).unwrap_err();
        assert_eq!(
            err,
            ImageDecodeError::EncodingMismatch {
                message: "bgr8".to_string(),
                expected: CameraEncoding::Rgb8,
            }
        );
    }

    #[test]
    fn mono8_message_when_expected_rgb8_is_mismatch() {
        // channel-count mismatch is also a hard reject.
        let data = vec![0u8; 4];
        let err = decode_image(&data, "mono8", 2, 2, 2, false, CameraEncoding::Rgb8).unwrap_err();
        assert!(matches!(err, ImageDecodeError::EncodingMismatch { .. }));
    }

    #[test]
    fn unsupported_encoding_rejected() {
        let data = vec![0u8; 64];
        for enc in ["yuv422", "rgb16", "rgba8", "32FC1", "16UC1", "bayer_rggb8"] {
            let err = decode_image(&data, enc, 2, 2, 8, false, CameraEncoding::Rgb8).unwrap_err();
            assert_eq!(
                err,
                ImageDecodeError::UnsupportedEncoding {
                    encoding: enc.to_string()
                },
                "encoding {enc} must be unsupported"
            );
        }
    }

    // -- Fail-closed -----------------------------------------------------

    #[test]
    fn truncated_buffer_rejected() {
        let data = vec![0u8; 11]; // needs step*height = 6*2 = 12
        let err = decode_image(&data, "rgb8", 2, 2, 6, false, CameraEncoding::Rgb8).unwrap_err();
        assert!(matches!(err, ImageDecodeError::TruncatedBuffer { .. }));
    }

    #[test]
    fn step_too_small_rejected() {
        // rgb8 width 2 → tight_row 6, but step 4 < 6.
        let data = vec![0u8; 8];
        let err = decode_image(&data, "rgb8", 2, 2, 4, false, CameraEncoding::Rgb8).unwrap_err();
        assert_eq!(
            err,
            ImageDecodeError::InvalidStep {
                step: 4,
                tight_row: 6
            }
        );
    }

    #[test]
    fn zero_dimensions_rejected() {
        let data = vec![0u8; 4];
        assert_eq!(
            decode_image(&data, "mono8", 0, 2, 0, false, CameraEncoding::Mono8).unwrap_err(),
            ImageDecodeError::InvalidDimensions {
                width: 0,
                height: 2
            }
        );
        assert_eq!(
            decode_image(&data, "mono8", 2, 0, 2, false, CameraEncoding::Mono8).unwrap_err(),
            ImageDecodeError::InvalidDimensions {
                width: 2,
                height: 0
            }
        );
    }

    /// TIGHT-BUFFER INVARIANT: output length == width*height*channels, dims
    /// carried, for each supported encoding.
    #[test]
    fn tight_buffer_invariant() {
        for (enc, ce, ch) in [
            ("rgb8", CameraEncoding::Rgb8, 3usize),
            ("bgr8", CameraEncoding::Bgr8, 3),
            ("mono8", CameraEncoding::Mono8, 1),
        ] {
            let (w, h) = (4u32, 3u32);
            let tight = (w as usize) * ch;
            let step = tight + 5; // padded
            let data = vec![7u8; step * h as usize];
            let s = decode_image(&data, enc, w, h, step as u32, false, ce).unwrap();
            assert_eq!(s.bytes.len(), (w as usize) * (h as usize) * ch);
            assert_eq!((s.src_width, s.src_height), (w, h));
        }
    }
}
