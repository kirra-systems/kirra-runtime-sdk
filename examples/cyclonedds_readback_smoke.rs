// examples/cyclonedds_readback_smoke.rs
//
// D1 (item 19) live smoke test — run on an integrator host that ships CycloneDDS
// (`--features cyclonedds`, e.g. a ROS 2 Jazzy box with `ros-jazzy-cyclonedds`):
//
//   RUSTFLAGS="-L /opt/ros/jazzy/lib/x86_64-linux-gnu" \
//     cargo run --example cyclonedds_readback_smoke --features cyclonedds
//
// It builds a REAL CycloneDDS `dds_qos_t` from the frozen actuator profile via
// the FFI setters, reads every policy straight back out via the FFI getters, maps
// it home, and runs the fail-closed `validate_qos_readback`. This exercises the
// part of the FFI most sensitive to a wrong constant / argument order (the QoS
// kind enums and `dds_duration_t` units) against the live library — the runtime
// complement to the link check (`cargo build --features cyclonedds`). No DDS
// domain / network is needed, so it is deterministic and side-effect free.
//
// It is `required-features = ["cyclonedds"]` in Cargo.toml, so a default build
// never compiles it.

use kirra_verifier::dds_bridge::{validate_qos_readback, DdsQosProfile};
use kirra_verifier::dds_cyclonedds::qos_roundtrip_selfcheck;

fn main() {
    let requested = DdsQosProfile::critical_actuator_profile();
    println!("requested actuator QoS : {requested:?}");

    match qos_roundtrip_selfcheck(&requested) {
        Ok(readback) => {
            println!("CycloneDDS read-back   : {readback:?}");
            match validate_qos_readback(&requested, &readback) {
                Ok(()) => {
                    println!(
                        "\nVERDICT: PASS — CycloneDDS round-tripped the actuator QoS faithfully \
                         and the read-back validation admits it."
                    );
                }
                Err(e) => {
                    eprintln!("\nVERDICT: FAIL — read-back validation rejected the round-trip: {e}");
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("\nVERDICT: ERROR — CycloneDDS QoS self-check failed: {e:?}");
            std::process::exit(2);
        }
    }
}
