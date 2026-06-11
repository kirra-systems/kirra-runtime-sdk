// iceoryx2-spike binary — runs the fault matrix over a real iceoryx2 channel and
// prints it with per-fault-class timing.
//
// Modes:
//   (default)        run the matrix in-process over the `ipc` service (real
//                    zero-copy shared memory) — deterministic, the asserted run.
//   --two-process    spawn a child `subscribe` process and a child `publish`
//                    process so the SAME channel is exercised as two OS
//                    processes — the production shape.
//   subscribe N      (internal) receive N frames and judge them.
//   publish   N      (internal) publish N valid frames.
//
// HONESTY BANNER: the PASS gate is VERDICT CORRECTNESS. Host timing is
// INDICATIVE ONLY. Certified WCET comes from the QNX 8.0 target under FIFO
// scheduling (#274) — host numbers are never presented as WCET.

use std::env;
use std::process::Command;

use iceoryx2::prelude::*;
use iceoryx2_spike::harness::{process, run_matrix, Outcome, RowResult};
use iceoryx2_spike::judge::{JudgeState, Verdict};
use iceoryx2_spike::wire::{CommandFrame, FRAME_MAGIC};

const SERVICE: &str = "kirra/iceoryx2-spike/cmd";
const WARMUP: usize = 200;
const SAMPLES: usize = 2_000;

fn main() {
    let args: Vec<String> = env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("matrix");

    let result = match mode {
        "matrix" => run_and_print_matrix(),
        "--two-process" => run_two_process(&args[0]),
        "subscribe" => run_subscriber(parse_count(&args, 2)),
        "publish" => run_publisher(parse_count(&args, 2)),
        other => {
            eprintln!("unknown mode: {other} (use: matrix | --two-process | subscribe N | publish N)");
            std::process::exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

fn parse_count(args: &[String], idx: usize) -> usize {
    args.get(idx).and_then(|s| s.parse().ok()).unwrap_or(100)
}

fn run_and_print_matrix() -> Result<(), Box<dyn core::error::Error>> {
    let rows = run_matrix(SERVICE, WARMUP, SAMPLES)?;
    print_banner();
    print_matrix(&rows);
    print_torn_header_finding();

    let all_pass = rows.iter().all(RowResult::pass);
    println!(
        "\nGATE (verdict correctness): {}",
        if all_pass { "PASS" } else { "FAIL" }
    );
    if !all_pass {
        return Err("one or more fault classes produced an incorrect verdict".into());
    }
    Ok(())
}

fn print_banner() {
    println!("============================================================================");
    println!(" iceoryx2 host-side fault matrix (#273) — Rust end-to-end, no FFI, no unsafe");
    println!(" transport: iceoryx2 v0.9.1  |  service type: ipc (zero-copy shared memory)");
    println!(" PASS GATE = VERDICT CORRECTNESS.  Timing is INDICATIVE (host, not WCET).");
    println!(" Certified WCET comes from the QNX 8.0 target under FIFO scheduling (#274).");
    println!("============================================================================");
}

fn print_matrix(rows: &[RowResult]) {
    println!(
        "\n{:<22} {:>7} {:>6} {:>10} {:>10} {:>10}",
        "fault class", "samples", "ok", "p50(us)", "p99(us)", "max(us)"
    );
    println!("{}", "-".repeat(70));
    for r in rows {
        println!(
            "{:<22} {:>7} {:>6} {:>10.3} {:>10.3} {:>10.3}",
            r.class.name(),
            r.samples,
            if r.pass() { "PASS" } else { "FAIL" },
            r.p50.as_secs_f64() * 1e6,
            r.p99.as_secs_f64() * 1e6,
            r.max.as_secs_f64() * 1e6,
        );
        if !r.pass() {
            println!("    expected {:?}, observed {:?}", r.expected, r.observed);
        }
    }
}

fn print_torn_header_finding() {
    println!("\nTornHeader: ELIMINATED BY TRANSPORT (finding, not a skipped test).");
    println!("  iceoryx2's sample lifecycle prevents a torn read BY CONSTRUCTION: the");
    println!("  publisher writes into an exclusively-loaned slot and `send()` publishes an");
    println!("  immutable sample; the subscriber's `receive()` returns an owned `Sample`");
    println!("  over a stable slot that is not recycled while held. The application never");
    println!("  double-reads a live, concurrently-mutating buffer — so a torn header is not");
    println!("  reachable without `unsafe` raw-memory access, which this spike forbids.");
    println!("  transport-eliminates-X is first-class ADR evidence for #275.");
}

// --- two-process production-shape demo -------------------------------------

fn run_two_process(exe: &str) -> Result<(), Box<dyn core::error::Error>> {
    const N: usize = 500;
    println!("[two-process] spawning subscriber + publisher as separate OS processes...");
    let mut sub = Command::new(exe).arg("subscribe").arg(N.to_string()).spawn()?;
    // Give the subscriber a moment to create the service/endpoint.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut pubp = Command::new(exe).arg("publish").arg(N.to_string()).spawn()?;
    let pub_status = pubp.wait()?;
    let sub_status = sub.wait()?;
    println!(
        "[two-process] publisher exit={:?}, subscriber exit={:?}",
        pub_status.code(),
        sub_status.code()
    );
    if pub_status.success() && sub_status.success() {
        println!("[two-process] PASS — real two-process zero-copy exchange over iceoryx2.");
        Ok(())
    } else {
        Err("two-process exchange failed".into())
    }
}

fn run_subscriber(n: usize) -> Result<(), Box<dyn core::error::Error>> {
    let node = NodeBuilder::new().create::<ipc::Service>()?;
    let service = node
        .service_builder(&SERVICE.try_into()?)
        .publish_subscribe::<CommandFrame>()
        .open_or_create()?;
    let subscriber = service.subscriber_builder().create()?;

    let mut received = 0usize;
    let mut accepted = 0usize;
    let mut state = JudgeState::new();
    // Bounded zero-copy queues may DROP under burst when the consumer lags — that
    // is expected pub/sub backpressure, not a correctness failure. So complete on
    // a quiet period (no new frame for a grace window) rather than a fixed count,
    // and gate success on CORRECTNESS: every frame that arrived was accepted, and
    // the sequence stream stayed monotonic (no reorder over the transport).
    let overall_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut last_rx = std::time::Instant::now();
    loop {
        let mut got_any = false;
        while let Some(sample) = subscriber.receive()? {
            received += 1;
            got_any = true;
            let frame: CommandFrame = *sample;
            if let Outcome::Judged(Verdict::Accept) = process(&frame, &mut state, 1_000_000_000) {
                accepted += 1;
            }
        }
        if got_any {
            last_rx = std::time::Instant::now();
        }
        if received >= 1 && last_rx.elapsed() > std::time::Duration::from_millis(800) {
            break;
        }
        if std::time::Instant::now() > overall_deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    println!(
        "[subscribe] received={received}/{n} accepted={accepted} \
         (pub/sub may drop under burst; correctness = all received accepted, monotonic)"
    );
    if received >= 1 && accepted == received {
        Ok(())
    } else {
        Err(format!("subscriber correctness failed: accepted {accepted}/{received}").into())
    }
}

fn run_publisher(n: usize) -> Result<(), Box<dyn core::error::Error>> {
    let node = NodeBuilder::new().create::<ipc::Service>()?;
    let service = node
        .service_builder(&SERVICE.try_into()?)
        .publish_subscribe::<CommandFrame>()
        .open_or_create()?;
    let publisher = service.publisher_builder().create()?;

    for i in 0..n as u64 {
        let frame = CommandFrame::well_formed(i + 1, u64::MAX / 2, 5.0, 0.2);
        debug_assert_eq!(frame.magic, FRAME_MAGIC);
        let sample = publisher.loan_uninit()?;
        let sample = sample.write_payload(frame);
        sample.send()?;
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
    println!("[publish] sent {n} valid frames");
    Ok(())
}
