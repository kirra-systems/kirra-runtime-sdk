//! `kirra-r2cp-sim` — run the simulated MCU on a PTY and print its device path.
//!
//! The bench counterpart to the in-process tests: point the real bridge at the
//! printed path and it talks to a peer that enforces the protocol's refusals,
//! with no board attached and nothing that can move.
//!
//! ```text
//! $ kirra-r2cp-sim
//! kirra-r2cp-sim: device /dev/pts/7
//! ```
//!
//! 🔴 This is a TEST PEER. It is not a governor, it is not an authority, and a
//! bridge that satisfies it has proved its framing and sequencing — nothing
//! about the firmware, and nothing about whether motion was authorized. That
//! remains ADR-0033's token-verifying consumer.
//!
//! Flags:
//!   `--physical-ack`  assert the simulated local fault-acknowledge input, so
//!                     an ACKNOWLEDGE_FAULT frame can actually clear a latch.
//!                     Off by default: a network packet alone must not.
//!   `--quiet`         suppress the per-frame log.

use std::time::Instant;

use kirra_r2cp::pty::SimulatedMcuPty;
use kirra_r2cp::sim::SimulatedMcu;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let physical_ack = args.iter().any(|a| a == "--physical-ack");
    let quiet = args.iter().any(|a| a == "--quiet");
    if let Some(bad) = args
        .iter()
        .find(|a| !matches!(a.as_str(), "--physical-ack" | "--quiet"))
    {
        eprintln!("kirra-r2cp-sim: unknown argument {bad}");
        std::process::exit(2);
    }

    let mut pty = SimulatedMcuPty::open()?;
    let mut mcu = SimulatedMcu::new();
    mcu.physical_ack_asserted = physical_ack;

    println!("kirra-r2cp-sim: device {}", pty.device_path().display());
    if physical_ack {
        println!("kirra-r2cp-sim: simulated physical fault-acknowledge input ASSERTED");
    }

    // Monotonic, and zeroed at start so the numbers in the log are readable.
    // Wall time is never used: a jump backwards would make fresh commands look
    // stale and stale ones fresh (AOU-TIMESYNC-001).
    let started = Instant::now();
    let now_us = || u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);

    loop {
        let t = now_us();
        for h in pty.pump(&mut mcu, t, 50)? {
            if !quiet {
                match &h.outcome {
                    Ok(o) => println!("[{t:>10}] {o:?}  state={:?}", mcu.state),
                    Err(e) => println!("[{t:>10}] undecodable: {e:?}"),
                }
            }
        }
        if pty.tick(&mut mcu, now_us())? && !quiet {
            println!(
                "[{:>10}] watchdog: no fresh command -> {:?}",
                now_us(),
                mcu.state
            );
        }
    }
}
