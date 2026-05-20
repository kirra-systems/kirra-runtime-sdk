// src/ros2_adapter.rs

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector3D { pub x: f64, pub y: f64, pub z: f64 }

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

pub struct Ros2CmdVelInterlockAdapter {
    pub angular_velocity_limit: f64,
}

impl Ros2CmdVelInterlockAdapter {
    pub fn new(max_angular: f64) -> Result<Self, KinematicInterlockError> {
        if max_angular <= 0.0 || max_angular.is_nan() || max_angular.is_infinite() {
            return Err(KinematicInterlockError::InvalidAdapterConfiguration);
        }
        Ok(Self { angular_velocity_limit: max_angular })
    }

    pub fn decode_twist_frame(&self, raw_buffer: &[u8]) -> Result<Ros2TwistMessage, KinematicInterlockError> {
        if raw_buffer.len() < 48 { return Err(KinematicInterlockError::MessageFrameTruncated); }

        let read_f64 = |start: usize| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&raw_buffer[start..start + 8]);
            f64::from_le_bytes(bytes)
        };

        let msg = Ros2TwistMessage {
            linear: Vector3D { x: read_f64(0), y: read_f64(8), z: read_f64(16) },
            angular: Vector3D { x: read_f64(24), y: read_f64(32), z: read_f64(40) },
        };

        let detect_anomalous_float = |v: f64| v.is_nan() || v.is_infinite();
        if detect_anomalous_float(msg.linear.x) || detect_anomalous_float(msg.linear.y) || detect_anomalous_float(msg.linear.z) ||
           detect_anomalous_float(msg.angular.x) || detect_anomalous_float(msg.angular.y) || detect_anomalous_float(msg.angular.z) {
            return Err(KinematicInterlockError::MalformedVelocityVector);
        }
        if msg.angular.z.abs() > self.angular_velocity_limit { return Err(KinematicInterlockError::AngularVelocityBreach); }

        Ok(msg)
    }

    pub fn encode_twist_frame(&self, sanitized_msg: &Ros2TwistMessage) -> Vec<u8> {
        let mut buffer = vec![0u8; 48];
        let mut write_f64 = |start: usize, val: f64| {
            let bytes = val.to_le_bytes();
            buffer[start..start + 8].copy_from_slice(&bytes);
        };
        write_f64(0, sanitized_msg.linear.x); write_f64(8, sanitized_msg.linear.y); write_f64(16, sanitized_msg.linear.z);
        write_f64(24, sanitized_msg.angular.x); write_f64(32, sanitized_msg.angular.y); write_f64(40, sanitized_msg.angular.z);
        buffer
    }
}
