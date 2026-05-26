// src/bin/ai_robot_demo.rs
// Demonstrates the Kirra AI robot safety pipeline with CDR-framed DDS output.

use kirra_runtime_sdk::kinematics_contract::KinematicContract;
use kirra_runtime_sdk::robotics_alignment::AlignmentBridge;
use kirra_runtime_sdk::dds_bridge::DdsPublisherBridge;

fn main() {
    let contract = KinematicContract {
        max_linear_velocity: 2.0,
        max_angular_velocity: 1.5,
        max_linear_acceleration: 0.5,
        fallback_linear_speed: 0.0,
    };
    let bridge = AlignmentBridge::new(contract);

    let intents = [
        r#"{"action": "MOVE", "velocity": 1.0}"#,
        r#"{"action": "ROTATE", "angular_velocity": 0.3}"#,
        r#"{"action": "MOVE", "velocity": 5.0}"#,
        r#"{"action": "STOP"}"#,
    ];

    for intent in &intents {
        println!("Intent: {}", intent);
        match bridge.align_and_serialize_intent(intent) {
            Ok((output, frame)) => {
                let dds_payload = DdsPublisherBridge::wrap_cdr_encapsulation(&frame);
                println!("  Resolution: {:?}", output.resolution);
                println!("  Narrative:  {}", output.narrative);
                println!("  DDS Frame:  {}", hex::encode(&dds_payload));
            }
            Err(e) => println!("  Parse error: {}", e),
        }
        println!();
    }
}
