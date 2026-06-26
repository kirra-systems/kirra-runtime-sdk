// src/bin/ai_robot_demo.rs
// Demonstrates the Kirra AI robot safety pipeline with CDR-framed DDS output.

use kirra_verifier::kinematics_contract::KinematicContract;
use kirra_verifier::robotics_alignment::AlignmentBridge;
use kirra_verifier::dds_bridge::{DdsPublisherBridge, DdsQosProfile};

fn main() {
    let contract = KinematicContract {
        max_linear_velocity: 2.0,
        max_angular_velocity: 1.5,
        max_linear_acceleration: 0.5,
        fallback_linear_speed: 0.0,
    };
    let bridge = AlignmentBridge::new(contract);

    // Actuator topics publish under the frozen critical QoS profile; the publish
    // seam enforces Volatile + latest-wins + bounded-lifespan fail-closed.
    let actuator_qos = DdsQosProfile::critical_actuator_profile();

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
                println!("  Resolution: {:?}", output.resolution);
                println!("  Narrative:  {}", output.narrative);
                match DdsPublisherBridge::publish_actuator_command(&frame, &actuator_qos) {
                    Ok(dds_payload) => println!("  DDS Frame:  {}", hex::encode(&dds_payload)),
                    Err(v) => println!("  DDS REFUSED: {v}"),
                }
            }
            Err(e) => println!("  Parse error: {}", e),
        }
        println!();
    }
}
