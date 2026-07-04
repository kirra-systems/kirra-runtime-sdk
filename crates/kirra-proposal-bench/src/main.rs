// crates/kirra-proposal-bench/src/main.rs
//
// kirra-proposal-bench — DEV/TEST tool. Sends a sweep of proposals to a running
// `kirra-governor-service` over UDP, decodes each verdict via the shared
// `kirra-wire-client` mirror, and prints a CASE / REASON / VERDICT table.
//
// NOT a shipped or safety artifact: it neither contains nor re-implements any
// verdict logic — it just talks to the governor and reports what comes back.
//
//   cargo run -p kirra-proposal-bench
//   KIRRA_GOVERNOR_ADDR=192.168.1.50:9760 cargo run -p kirra-proposal-bench

use kirra_wire_client::{
    decode_verdict, encode_proposal, ClientEnforceAction, Proposal, ProposedCommand,
};
use std::net::UdpSocket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_ADDR: &str = "127.0.0.1:9760";
const RECV_TIMEOUT: Duration = Duration::from_millis(1000);

/// One sweep case: a human-readable intent plus the command to send. The intent
/// is what we EXPECT to provoke; the printed verdict is whatever the governor
/// actually returns (the bench reports, it does not predict).
struct Case {
    name: &'static str,
    cmd: ProposedCommand,
}

fn cmd(linear: f64, current: f64, dt: f64, steer: f64, cur_steer: f64) -> ProposedCommand {
    ProposedCommand {
        linear_velocity_mps: linear,
        current_velocity_mps: current,
        delta_time_s: dt,
        steering_angle_deg: steer,
        current_steering_angle_deg: cur_steer,
    }
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// (REASON token, VERDICT rendering) for the table.
fn render(action: &ClientEnforceAction, reason_code: u32) -> (String, String) {
    match action {
        ClientEnforceAction::Allow => ("ACCEPT".into(), "Allow".into()),
        ClientEnforceAction::ClampLinear(v) => {
            ("CLAMP_LINEAR".into(), format!("ClampLinear({v:.3} m/s)"))
        }
        ClientEnforceAction::ClampSteering(v) => {
            ("CLAMP_STEER".into(), format!("ClampSteering({v:.3} deg)"))
        }
        ClientEnforceAction::ClampBoth { linear, steering } => (
            "CLAMP_BOTH".into(),
            format!("ClampBoth({linear:.3} m/s, {steering:.3} deg)"),
        ),
        ClientEnforceAction::DenyBreach(code) => {
            (format!("{code:?}"), format!("DenyBreach (reason_code={reason_code})"))
        }
    }
}

fn main() -> std::io::Result<()> {
    let addr = std::env::var("KIRRA_GOVERNOR_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());

    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.connect(&addr)?;
    sock.set_read_timeout(Some(RECV_TIMEOUT))?;

    let cases = [
        Case { name: "nominal accept",      cmd: cmd(1.0, 1.0, 0.1, 0.0, 0.0) },
        Case { name: "over-speed clamp",    cmd: cmd(50.0, 50.0, 1.0, 0.0, 0.0) },
        Case { name: "reverse over-speed",  cmd: cmd(-50.0, -50.0, 1.0, 0.0, 0.0) },
        Case { name: "hard steering step",  cmd: cmd(2.0, 2.0, 0.1, 40.0, 0.0) },
        Case { name: "NaN deny",            cmd: cmd(f64::NAN, 1.0, 0.1, 0.0, 0.0) },
        Case { name: "zero-dt deny",        cmd: cmd(1.0, 1.0, 0.0, 0.0, 0.0) },
    ];

    eprintln!("kirra-proposal-bench → {addr} ({} cases)\n", cases.len());
    println!("{:<20} | {:<26} | VERDICT", "CASE", "REASON");
    println!("{}", "-".repeat(78));

    let mut buf = [0u8; 4096];
    let mut sent = 0usize;
    let mut answered = 0usize;

    for (i, c) in cases.iter().enumerate() {
        let proposal = Proposal {
            seq: i as u64 + 1,
            ts_nanos: now_nanos(),
            command: c.cmd.clone(),
        };
        let bytes = encode_proposal(&proposal).expect("encode proposal");
        sock.send(&bytes)?;
        sent += 1;

        match sock.recv(&mut buf) {
            Ok(n) => match decode_verdict(&buf[..n]) {
                Ok(v) => {
                    answered += 1;
                    let (reason, verdict) = render(&v.action, v.reason_code);
                    println!("{:<20} | {:<26} | {}", c.name, reason, verdict);
                }
                Err(e) => println!("{:<20} | {:<26} | DECODE ERROR: {e}", c.name, "ERR"),
            },
            Err(e) => println!("{:<20} | {:<26} | NO REPLY ({e})", c.name, "TIMEOUT"),
        }
    }

    println!("\n{answered}/{sent} proposals answered by {addr}");
    if answered == 0 {
        eprintln!(
            "no verdicts received — is kirra-governor-service running? \
             (cargo run -p kirra-governor-service)"
        );
        std::process::exit(1);
    }
    Ok(())
}
