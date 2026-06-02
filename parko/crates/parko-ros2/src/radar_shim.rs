// parko/crates/parko-ros2/src/radar_shim.rs
//
// radar_msgs/RadarScan → Vec<RadarDetection> extraction shim — the deferred ROS
// half of the radar mapping. PURE field-copy CORE (no r2r, always compiled,
// unit-tested) + a THIN r2r ADAPTER (`#[cfg(feature = "ros2")]`, extraction
// only).
//
// SAFETY FRAMING. Upstream of the radar transform → model → governor. The
// transform already fail-closes on non-finite values, so this shim stays thin.
// What it must get right is the FIELD MAPPING — a wrong column feeds the model
// the wrong quantity (a confidently-wrong, in-bounds corruption). Two names
// change and are flagged:
//   - `doppler_velocity` → `velocity` (the Doppler radial velocity, radar's key
//     signal — must not be dropped or swapped).
//   - `amplitude` → `rcs` (amplitude is the return-strength proxy for radar
//     cross-section; the name change is intentional, documented here).
//
// MESSAGE CHOICE (FLAGGED): target `radar_msgs/RadarScan` — the DETECTION list,
// which matches the radar transform's detection-list input. `radar_msgs/
// RadarTracks` is the object-level alternative and maps differently (tracked
// objects, not raw detections); it is a SEPARATE future shim, not handled here.
//
// 2D RADAR: `RadarScan` always carries an `elevation` field, so the shim passes
// `Some(elevation)`. It does NOT decide 2D-ness — the transform's
// `ElevationPolicy` owns that interpretation; the shim only moves the field.
//
// PIPELINE CONNECTION: `RadarDetection` is the radar mapping's transform input,
// in main (radar mapping landed in sensor_mapping.rs). This shim emits that exact
// type — the prior byte-identical mirror has been collapsed away — so the shim's
// decode output feeds the mapping's `to_tensor` with no conversion: the radar
// path (RadarScan → decode → RadarDetection → mapping → tensor) type-checks end
// to end.
use crate::sensor_mapping::RadarDetection;

/// Plain mirror of one `radar_msgs/RadarReturn`'s relevant fields — r2r-free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadarReturnRaw {
    pub range: f32,
    pub azimuth: f32,
    pub elevation: f32,
    pub doppler_velocity: f32,
    pub amplitude: f32,
}

/// Pure field copy of one return into the transform's `RadarDetection`. The two
/// renamed fields (Doppler→velocity, amplitude→rcs) are the mapping that must
/// not be mis-wired.
#[must_use]
pub fn radar_return_to_detection(r: &RadarReturnRaw) -> RadarDetection {
    RadarDetection {
        range: r.range,
        azimuth: r.azimuth,
        // RadarScan always carries elevation; the transform's ElevationPolicy
        // decides 2D interpretation — the shim just passes Some, never decides.
        elevation: Some(r.elevation),
        velocity: r.doppler_velocity, // Doppler radial velocity
        rcs: r.amplitude,             // amplitude = RCS proxy
    }
}

/// Pure scan → detection list. Preserves COUNT and ORDER (none lost, duplicated,
/// or reordered).
#[must_use]
pub fn radar_scan_to_detections(returns: &[RadarReturnRaw]) -> Vec<RadarDetection> {
    returns.iter().map(radar_return_to_detection).collect()
}

/// THIN r2r ADAPTER — extraction only. Compiles only under `--features ros2`.
#[cfg(feature = "ros2")]
pub fn radar_scan_msg_to_detections(
    msg: &r2r::radar_msgs::msg::RadarScan,
) -> Vec<RadarDetection> {
    let raws: Vec<RadarReturnRaw> = msg
        .returns
        .iter()
        .map(|r| RadarReturnRaw {
            range: r.range,
            azimuth: r.azimuth,
            elevation: r.elevation,
            doppler_velocity: r.doppler_velocity,
            amplitude: r.amplitude,
        })
        .collect();
    radar_scan_to_detections(&raws)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinct values per field so a mis-map (esp. doppler→velocity,
    /// amplitude→rcs) is visible.
    fn ret(range: f32, az: f32, el: f32, dop: f32, amp: f32) -> RadarReturnRaw {
        RadarReturnRaw { range, azimuth: az, elevation: el, doppler_velocity: dop, amplitude: amp }
    }

    #[test]
    fn return_maps_to_correct_detection_fields() {
        let d = radar_return_to_detection(&ret(10.0, 0.5, 0.25, -3.0, 42.0));
        assert_eq!(d.range, 10.0);
        assert_eq!(d.azimuth, 0.5);
        assert_eq!(d.elevation, Some(0.25));
        assert_eq!(d.velocity, -3.0, "doppler_velocity must map to velocity");
        assert_eq!(d.rcs, 42.0, "amplitude must map to rcs");
    }

    #[test]
    fn multi_return_scan_preserves_count_and_order() {
        let scan = vec![
            ret(1.0, 0.1, 0.0, 1.0, 11.0),
            ret(2.0, 0.2, 0.0, 2.0, 22.0),
            ret(3.0, 0.3, 0.0, 3.0, 33.0),
        ];
        let out = radar_scan_to_detections(&scan);
        assert_eq!(out.len(), 3, "no return lost or duplicated");
        // same order: range 1,2,3 with the matching velocity/rcs.
        assert_eq!(out[0].range, 1.0);
        assert_eq!(out[1].velocity, 2.0);
        assert_eq!(out[2].rcs, 33.0);
    }

    #[test]
    fn empty_scan_yields_empty_list() {
        assert!(radar_scan_to_detections(&[]).is_empty());
    }
}
