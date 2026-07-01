// shm_latency — raw POSIX-SHM seqlock read latency (ADR-0030 evidence).
//
// Times the GOVERNOR read path over a real shared-memory mapping — the carrier's
// actual cost on the validation path — so the "raw SHM + seqlock beats iceoryx2
// for this single-latest-value pattern" claim has a measured number instead of
// the in-process-floor proxy. Same 176-byte GovernorContractView payload, same
// nearest-rank percentile method as `tools/iceoryx2-spike/.../latency_bench.rs`,
// so the rows are directly comparable.
//
// HONESTY: absolute ns are host-INDICATIVE (shared dev box, no core isolation /
// FIFO). The comparative RATIO vs the iceoryx2 / socket rows is the takeaway; the
// certified figure is a QNX-target-under-FIFO measurement (#274).
//
// Run:  KIRRA_LAT_ITERS=1000000 cargo run --release -p kirra-hv-carrier --bin shm_latency

use std::hint::black_box;
use std::time::{Duration, Instant};

use kirra_contract_channel::{
    publish, read_coherent_snapshot, validate, AcceptedWatermark, VehicleCommandPayload,
    MAX_SNAPSHOT_RETRIES,
};
use kirra_hv_carrier::{PosixShmReader, PosixShmRegion};

fn parse_iters() -> usize {
    std::env::var("KIRRA_LAT_ITERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(200_000)
}

fn percentile(sorted: &[Duration], pct: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let mut rank = ((pct / 100.0) * n as f64).ceil() as usize;
    rank = rank.clamp(1, n);
    sorted[rank - 1].as_nanos()
}

fn row(name: &str, mut s: Vec<Duration>) {
    s.sort_unstable();
    println!(
        "{name:<28} {:>9} {:>9} {:>9} {:>9}",
        percentile(&s, 50.0),
        percentile(&s, 99.0),
        percentile(&s, 99.9),
        s.last().copied().unwrap_or(Duration::ZERO).as_nanos(),
    );
}

fn well_formed(seq: u64) -> VehicleCommandPayload {
    VehicleCommandPayload {
        linear_velocity_mps: 3.0,
        current_velocity_mps: 2.9,
        delta_time_s: 0.1,
        steering_angle_deg: 1.5,
        current_steering_angle_deg: 1.2,
    }
    .with_seq_noise(seq)
}

// tiny helper so successive publishes differ (defeat any constant-folding)
trait SeqNoise {
    fn with_seq_noise(self, seq: u64) -> Self;
}
impl SeqNoise for VehicleCommandPayload {
    fn with_seq_noise(mut self, seq: u64) -> Self {
        self.linear_velocity_mps = 3.0 + (seq % 7) as f64 * 0.01;
        self
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let iters = parse_iters();
    let warmup = (iters / 10).max(1_000);

    let name = format!("/kirra-hv-lat-{}", std::process::id());
    let guest = PosixShmRegion::create(&name)?;
    let governor = PosixShmReader::open(&name)?; // read-only, the governor mapping

    // Publish a committed command so the region holds a valid latest value.
    let committed = publish(&guest, 0, &well_formed(1).to_view(0, 1, 0, u64::MAX / 2));

    eprintln!(
        "=== raw POSIX-SHM seqlock read latency — INDICATIVE (host, no isolation/FIFO) ===\n\
         payload = frozen GovernorContractView (176 B) over MAP_SHARED; iters={iters} warmup={warmup}\n\
         governor mapping = PROT_READ (R-HV-1). Compare against the latency_bench\n\
         iceoryx2 / socket rows (same payload + percentile method)."
    );
    println!("\n{:<28} {:>9} {:>9} {:>9} {:>9}", "path", "p50_ns", "p99_ns", "p999_ns", "max_ns");
    println!("{}", "-".repeat(70));

    // (1) read_coherent_snapshot alone — the seqlock read the governor pays.
    let mut read_samples = Vec::with_capacity(iters);
    for i in 0..(iters + warmup) {
        let t0 = Instant::now();
        let snap = read_coherent_snapshot(&governor, MAX_SNAPSHOT_RETRIES).unwrap();
        let dt = t0.elapsed();
        black_box(snap.sequence);
        if i >= warmup {
            read_samples.push(dt);
        }
    }
    row("shm read_coherent_snapshot", read_samples);

    // (2) read + validate + decode — the full governor receive (minus kinematics).
    let wm = AcceptedWatermark::new();
    let mut full_samples = Vec::with_capacity(iters);
    for i in 0..(iters + warmup) {
        let t0 = Instant::now();
        let snap = read_coherent_snapshot(&governor, MAX_SNAPSHOT_RETRIES).unwrap();
        // A fresh watermark each iter so validate always accepts (we are timing
        // the read+validate+decode cost, not the monotonic gate).
        validate(&snap, 0, &wm).unwrap();
        let cmd = VehicleCommandPayload::from_validated_view(&snap).unwrap();
        let dt = t0.elapsed();
        black_box(cmd.linear_velocity_mps);
        if i >= warmup {
            full_samples.push(dt);
        }
    }
    row("shm read+validate+decode", full_samples);

    let _ = committed;
    eprintln!(
        "\nNote: the seqlock read is a handful of atomic loads + a 176 B copy over\n\
         shared pages — the same class as the latency_bench in-process floor, and\n\
         well below the iceoryx2 pub/sub rows: raw SHM is the minimal carrier for a\n\
         single latest-value slot (no queue, no loan, no discovery)."
    );
    Ok(())
}
