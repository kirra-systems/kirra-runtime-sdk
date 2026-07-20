// src/ros2_adapter.rs

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector3D {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ros2TwistMessage {
    pub linear: Vector3D,
    pub angular: Vector3D,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KinematicInterlockError {
    MessageFrameTruncated,
    MalformedVelocityVector,
    AngularVelocityBreach,
    InvalidAdapterConfiguration,
}

pub struct Ros2Adapter {
    pub angular_velocity_limit: f64,
}

impl Ros2Adapter {
    pub fn new(max_angular: f64) -> Result<Self, KinematicInterlockError> {
        if max_angular <= 0.0 || max_angular.is_nan() || max_angular.is_infinite() {
            return Err(KinematicInterlockError::InvalidAdapterConfiguration);
        }
        Ok(Self {
            angular_velocity_limit: max_angular,
        })
    }

    /// Decode a `geometry_msgs/Twist` from a RAW little-endian body — six `f64`s
    /// at fixed offsets 0/8/…/40, with NO CDR encapsulation.
    ///
    /// M5 (#1050) — NON-WIRE FIXTURE, not a DDS decoder. A real DDS-serialized
    /// sample is CDR: it begins with a 4-byte encapsulation header (2-byte scheme
    /// + 2-byte options) that selects big/little endianness, so the `f64` payload
    /// starts at offset 4, not 0 — this reader would be 4 bytes off and mis-decode
    /// every field. It exists ONLY to exercise the interlock/kinematic-clamp logic
    /// against a hand-built fixed-layout buffer in tests; the production ingress is
    /// the r2r-typed `~/input/*` path (`crates/kirra-ros2-adapter`), which lets the
    /// RMW do CDR (de)serialization. Do NOT feed this a real DDS sample. Making it
    /// wire-correct means parsing the encapsulation header + dispatching on
    /// endianness — deferred until/unless a raw-CDR ingress is actually needed.
    pub fn decode_twist_frame(
        &self,
        raw_buffer: &[u8],
    ) -> Result<Ros2TwistMessage, KinematicInterlockError> {
        if raw_buffer.len() < 48 {
            return Err(KinematicInterlockError::MessageFrameTruncated);
        }

        let read_f64 = |start: usize| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&raw_buffer[start..start + 8]);
            f64::from_le_bytes(bytes)
        };

        let msg = Ros2TwistMessage {
            linear: Vector3D {
                x: read_f64(0),
                y: read_f64(8),
                z: read_f64(16),
            },
            angular: Vector3D {
                x: read_f64(24),
                y: read_f64(32),
                z: read_f64(40),
            },
        };

        let detect_anomalous_float = |v: f64| v.is_nan() || v.is_infinite();
        if detect_anomalous_float(msg.linear.x)
            || detect_anomalous_float(msg.linear.y)
            || detect_anomalous_float(msg.linear.z)
            || detect_anomalous_float(msg.angular.x)
            || detect_anomalous_float(msg.angular.y)
            || detect_anomalous_float(msg.angular.z)
        {
            return Err(KinematicInterlockError::MalformedVelocityVector);
        }
        if msg.angular.z.abs() > self.angular_velocity_limit {
            return Err(KinematicInterlockError::AngularVelocityBreach);
        }

        Ok(msg)
    }

    pub fn encode_twist_frame(&self, sanitized_msg: &Ros2TwistMessage) -> Vec<u8> {
        let mut buffer = vec![0u8; 48];
        let mut write_f64 = |start: usize, val: f64| {
            let bytes = val.to_le_bytes();
            buffer[start..start + 8].copy_from_slice(&bytes);
        };
        write_f64(0, sanitized_msg.linear.x);
        write_f64(8, sanitized_msg.linear.y);
        write_f64(16, sanitized_msg.linear.z);
        write_f64(24, sanitized_msg.angular.x);
        write_f64(32, sanitized_msg.angular.y);
        write_f64(40, sanitized_msg.angular.z);
        buffer
    }
}
