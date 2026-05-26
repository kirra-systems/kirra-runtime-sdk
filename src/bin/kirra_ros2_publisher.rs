// src/bin/kirra_ros2_publisher.rs
// ROS2 publisher for Kirra-filtered twist commands.
// Build with: cargo build --features ros2

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "ros2")]
    {
        use kirra_runtime_sdk::kinematics_contract::KinematicContract;
        use kirra_runtime_sdk::robotics_alignment::AlignmentBridge;
        use kirra_runtime_sdk::dds_bridge::DdsPublisherBridge;

        let contract = KinematicContract {
            max_linear_velocity: 2.0,
            max_angular_velocity: 1.5,
            max_linear_acceleration: 0.5,
            fallback_linear_speed: 0.0,
        };

        let context = rclrs::Context::new(std::env::args())?;
        let node = rclrs::create_node(&context, "kirra_twist_publisher")?;

        let qos_policy = rclrs::QoSProfile {
            durability: rclrs::DurabilityPolicy::Volatile,
            reliability: rclrs::ReliabilityPolicy::Reliable,
            history: rclrs::HistoryPolicy::KeepLast { depth: 1 },
            ..rclrs::QoSProfile::default()
        };

        let publisher = node.create_publisher::<geometry_msgs::msg::Twist>("/cmd_vel_safe", qos_policy)?;

        let bridge = AlignmentBridge::new(contract);
        let test_intent = r#"{"action": "MOVE", "velocity": 1.0}"#;

        let (output, frame) = bridge.align_and_serialize_intent(test_intent)?;
        let dds_payload = DdsPublisherBridge::wrap_cdr_encapsulation(&frame);

        println!("Publishing filtered command | Resolution: {:?}", output.resolution);
        println!("DDS payload ({} bytes): {:?}", dds_payload.len(), &dds_payload[..4]);

        let twist_msg = geometry_msgs::msg::Twist::default();
        publisher.publish(twist_msg)?;
    }
    #[cfg(not(feature = "ros2"))]
    {
        eprintln!("ros2 feature not enabled. Build with: cargo build --features ros2");
        std::process::exit(1);
    }
    Ok(())
}
