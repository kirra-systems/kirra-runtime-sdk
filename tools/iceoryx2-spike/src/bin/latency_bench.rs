//! latency_bench — host-INDICATIVE latency comparison of the command-handoff
//! transports for the frozen `GovernorContractView`, on the moving-vehicle
//! doer→checker hot path (#275 / L2).
//!
//! The doer (QM planner) ↔ checker (ASIL governor) partition boundary is a
//! MANDATORY safety boundary (freedom-from-interference): you cannot delete it to
//! save latency. So the question is the *lowest-latency way to cross it*. This
//! bench times three ways of handing the 176-byte frozen contract across:
//!
//!   1. IN-PROCESS  — a by-value handoff (no IPC at all). The FLOOR: what you'd
//!      get if doer+checker were co-located (which safety forbids). Reference only.
//!   2. SOCKET+SERDE — canonical_image() -> UDP loopback -> from_canonical_image().
//!      Models a serialized cross-process socket hop (a conservative proxy for a
//!      DDS hop; real DDS adds RTPS/discovery/typed-serialization overhead on top).
//!   3. ICEORYX2     — zero-copy publish/receive of the frozen view over a real
//!      iceoryx2 shared-memory channel (no serialization, no kernel data copy).
//!
//! HONESTY: absolute numbers are INDICATIVE (shared host, no core isolation, no
//! SCHED_FIFO) — the comparative RATIO is the takeaway. The certified figure is a
//! QNX-target-under-FIFO measurement (#274), and the deployment lowest-latency
//! mode is iceoryx2 + busy-wait polling on an isolated core.
//!
//! Run:  KIRRA_LAT_ITERS=100000 cargo run --release --bin latency_bench

use std::hint::black_box;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use iceoryx2_spike::frozen::FrozenChannel;
use kirra_contract_channel::GovernorContractView;

fn parse_iters() -> usize {
    std::env::var("KIRRA_LAT_ITERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(50_000)
}

fn percentile(sorted: &[Duration], pct: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    // Nearest-rank: rank = ceil(n*pct/100), 1-based, clamped.
    let n = sorted.len();
    let mut rank = ((pct / 100.0) * n as f64).ceil() as usize;
    rank = rank.clamp(1, n);
    sorted[rank - 1]
}

struct Stats {
    p50: Duration,
    p99: Duration,
    p999: Duration,
    max: Duration,
}

fn summarize(mut samples: Vec<Duration>) -> Stats {
    samples.sort_unstable();
    Stats {
        p50: percentile(&samples, 50.0),
        p99: percentile(&samples, 99.0),
        p999: percentile(&samples, 99.9),
        max: samples.last().copied().unwrap_or(Duration::ZERO),
    }
}

fn well_formed(seq: u64) -> GovernorContractView {
    GovernorContractView::new_command(seq + 2, seq, 1, u64::MAX / 2, b"steer:1.5,0.2").unwrap()
}

/// 1. In-process by-value handoff — the floor (no IPC; safety forbids this for
///    doer→checker, shown only as the reference latency a co-located call costs).
fn bench_in_process(iters: usize, warmup: usize) -> Stats {
    let mut samples = Vec::with_capacity(iters);
    for i in 0..(iters + warmup) {
        let v = well_formed(i as u64);
        let t0 = Instant::now();
        // Hand the 176 B view across by value, defeat DCE.
        let received: GovernorContractView = black_box(v);
        let dt = t0.elapsed();
        black_box(received.sequence);
        if i >= warmup {
            samples.push(dt);
        }
    }
    summarize(samples)
}

/// 2. Serialize + UDP loopback — a conservative proxy for a serialized
///    cross-process socket/DDS hop (canonical_image -> kernel -> parse back).
fn bench_socket_serde(iters: usize, warmup: usize) -> std::io::Result<Stats> {
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    let local = sock.local_addr()?;
    sock.connect(local)?; // send to self: a full loopback round-trip
    let mut buf = [0u8; kirra_contract_channel::CANONICAL_IMAGE_LEN];

    let mut samples = Vec::with_capacity(iters);
    for i in 0..(iters + warmup) {
        let v = well_formed(i as u64);
        let t0 = Instant::now();
        let img = v.canonical_image(); // serialize (LE, 176 B)
        sock.send(&img)?;
        let n = sock.recv(&mut buf)?;
        let received =
            GovernorContractView::from_canonical_image(&buf[..n]).expect("parse"); // deserialize
        let dt = t0.elapsed();
        black_box(received.sequence);
        if i >= warmup {
            samples.push(dt);
        }
    }
    Ok(summarize(samples))
}

/// 3. iceoryx2 zero-copy — publish/receive the frozen view over real shared
///    memory (no serialization, no kernel data copy).
fn bench_iceoryx2(iters: usize, warmup: usize) -> Result<Stats, Box<dyn core::error::Error>> {
    let channel = FrozenChannel::create("kirra-latency-bench")?;
    let mut samples = Vec::with_capacity(iters);
    for i in 0..(iters + warmup) {
        let v = well_formed(i as u64);
        let t0 = Instant::now();
        let received = channel.round_trip(v)?;
        let dt = t0.elapsed();
        black_box(received.sequence);
        if i >= warmup {
            samples.push(dt);
        }
    }
    Ok(summarize(samples))
}

fn ns(d: Duration) -> u128 {
    d.as_nanos()
}

fn print_row(name: &str, s: &Stats, floor: Option<&Stats>) {
    let ratio = floor
        .map(|f| {
            if f.p50.as_nanos() == 0 {
                String::from("    n/a")
            } else {
                format!("{:6.1}x", s.p50.as_nanos() as f64 / f.p50.as_nanos() as f64)
            }
        })
        .unwrap_or_else(|| String::from("  (floor)"));
    println!(
        "{name:<22} {:>10} {:>10} {:>10} {:>10}   {ratio}",
        ns(s.p50),
        ns(s.p99),
        ns(s.p999),
        ns(s.max),
    );
}

fn main() {
    let iters = parse_iters();
    let warmup = (iters / 10).max(1_000);

    eprintln!(
        "=== command-handoff latency — INDICATIVE (host, no core isolation / FIFO) ===\n\
         payload = frozen GovernorContractView (176 B, #[repr(C)], by value)\n\
         iters={iters} warmup={warmup}\n\
         The doer->checker partition boundary is MANDATORY (safety); these are the\n\
         costs of CROSSING it. Ratio = p50 vs the in-process floor. Absolute ns are\n\
         host-indicative; the comparative ratio + 'crosses an isolation boundary?'\n\
         column are the takeaways. Certified numbers come from QNX/FIFO (#274)."
    );

    let in_proc = bench_in_process(iters, warmup);
    let socket = bench_socket_serde(iters, warmup).expect("udp loopback bench");
    let iox = bench_iceoryx2(iters, warmup).expect("iceoryx2 bench");

    println!(
        "\n{:<22} {:>10} {:>10} {:>10} {:>10}   p50 vs floor",
        "transport", "p50_ns", "p99_ns", "p999_ns", "max_ns"
    );
    println!("{}", "-".repeat(86));
    print_row("in-process (floor)", &in_proc, None);
    print_row("socket+serde (proxy)", &socket, Some(&in_proc));
    print_row("iceoryx2 (zero-copy)", &iox, Some(&in_proc));

    println!(
        "\nNotes:\n\
         * in-process is the FLOOR — doer+checker co-located. Safety FORBIDS it for\n\
           the QM-planner -> ASIL-governor hop (freedom-from-interference), so it is\n\
           a reference, not an option.\n\
         * socket+serde is a CONSERVATIVE proxy for a cross-process DDS hop: it pays\n\
           serialize + 2 syscalls + a kernel copy. A real DDS hop adds RTPS /\n\
           discovery / typed-CDR overhead on top (tens of us..ms; see README refs).\n\
         * iceoryx2 crosses the SAME isolation boundary as the socket, but with NO\n\
           serialization and NO kernel data copy (only an 8-byte offset moves). The\n\
           deployment lowest-latency mode pairs it with busy-wait polling on an\n\
           isolated core under SCHED_FIFO -> deterministic sub-microsecond on target."
    );
}
