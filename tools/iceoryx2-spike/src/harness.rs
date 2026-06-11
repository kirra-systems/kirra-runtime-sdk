// harness.rs — drive the fault classes through a REAL iceoryx2 zero-copy
// pub/sub channel and assert the subscriber-side outcome per class.
//
// Flow per injected frame: publisher.loan → write → send  ⇒  zero-copy shared
// memory  ⇒  subscriber.receive  ⇒  edge_validate (bounds/oversize, CRC)  ⇒
// judge. The verdict is asserted on the RECEIVED frame, proving the data made it
// across the transport intact. PASS/FAIL is on VERDICT CORRECTNESS ONLY; the
// timing is indicative (see the honesty banner in main.rs).

use core::time::Duration;
use std::time::Instant;

use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;

use crate::judge::{JudgeState, RejectReason, Verdict};
use crate::wire::{edge_validate, CommandFrame, EdgeReject, MAX_PAYLOAD_LEN};

/// Logical "now" handed to the judge (monotonic-domain nanoseconds). Fixed so
/// the deadline verdict is deterministic; the real path TIMING is measured
/// separately with `Instant` and never feeds a verdict.
const LOGICAL_NOW_NANOS: u64 = 1_000_000_000;
const FUTURE_DEADLINE_NANOS: u64 = u64::MAX / 2;
const PAST_DEADLINE_NANOS: u64 = 0;
/// Seed sequence for the classes that need a prior accepted command.
const SEED_SEQUENCE: u64 = 1_000;

/// The combined subscriber-side outcome: rejected at the edge, or judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    EdgeReject(EdgeReject),
    Judged(Verdict),
}

/// Subscriber-side processing: edge defense-in-depth THEN the judge.
pub fn process(frame: &CommandFrame, state: &mut JudgeState, now_nanos: u64) -> Outcome {
    match edge_validate(frame) {
        Err(e) => Outcome::EdgeReject(e),
        Ok(()) => Outcome::Judged(state.judge(frame, now_nanos)),
    }
}

/// The fault classes injected through the channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultClass {
    Valid,
    BadMagicStaleHeader,
    SequenceRegress,
    Replay,
    DeadlineMissed,
    IntegrityFlag,
    PayloadCorrupt,
    PayloadOversize,
    KinematicLimit,
}

impl FaultClass {
    pub const ALL: [FaultClass; 9] = [
        FaultClass::Valid,
        FaultClass::BadMagicStaleHeader,
        FaultClass::SequenceRegress,
        FaultClass::Replay,
        FaultClass::DeadlineMissed,
        FaultClass::IntegrityFlag,
        FaultClass::PayloadCorrupt,
        FaultClass::PayloadOversize,
        FaultClass::KinematicLimit,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            FaultClass::Valid => "Valid",
            FaultClass::BadMagicStaleHeader => "BadMagic/StaleHeader",
            FaultClass::SequenceRegress => "SequenceRegress",
            FaultClass::Replay => "Replay (equal seq)",
            FaultClass::DeadlineMissed => "DeadlineMissed",
            FaultClass::IntegrityFlag => "IntegrityFlag",
            FaultClass::PayloadCorrupt => "PayloadCorrupt (CRC)",
            FaultClass::PayloadOversize => "PayloadOversize",
            FaultClass::KinematicLimit => "KinematicLimit",
        }
    }

    /// The outcome a correct subscriber MUST produce for this class.
    pub fn expected(&self) -> Outcome {
        match self {
            FaultClass::Valid => Outcome::Judged(Verdict::Accept),
            FaultClass::BadMagicStaleHeader => Outcome::Judged(Verdict::Reject(RejectReason::BadMagic)),
            FaultClass::SequenceRegress => Outcome::Judged(Verdict::Reject(RejectReason::SequenceRegress)),
            FaultClass::Replay => Outcome::Judged(Verdict::Reject(RejectReason::Replay)),
            FaultClass::DeadlineMissed => Outcome::Judged(Verdict::Reject(RejectReason::DeadlineMissed)),
            FaultClass::IntegrityFlag => Outcome::Judged(Verdict::Reject(RejectReason::IntegrityFlag)),
            FaultClass::PayloadCorrupt => Outcome::EdgeReject(EdgeReject::CrcMismatch),
            FaultClass::PayloadOversize => Outcome::EdgeReject(EdgeReject::Oversize),
            FaultClass::KinematicLimit => Outcome::Judged(Verdict::Reject(RejectReason::KinematicLimit)),
        }
    }

    /// Fresh judge state appropriate for this class. Replay/Regress need a prior
    /// accepted command, so their state is pre-seeded; the rest start empty.
    fn initial_state(&self) -> JudgeState {
        match self {
            FaultClass::SequenceRegress | FaultClass::Replay => JudgeState {
                last_accepted: SEED_SEQUENCE,
                have_accepted: true,
            },
            _ => JudgeState::new(),
        }
    }

    /// Build the (faulted) frame for iteration `i` of this class. `i` lets the
    /// Valid stream advance monotonically; reject classes keep a constant
    /// sequence (rejections never advance the gate).
    fn build_frame(&self, i: u64) -> CommandFrame {
        match self {
            FaultClass::Valid => {
                // Strictly-increasing sequence so each is newer than the last.
                CommandFrame::well_formed(SEED_SEQUENCE + 1 + i, FUTURE_DEADLINE_NANOS, 5.0, 0.2)
            }
            FaultClass::BadMagicStaleHeader => {
                let mut f = CommandFrame::well_formed(1, FUTURE_DEADLINE_NANOS, 5.0, 0.2);
                f.magic = 0xDEAD_BEEF;
                f
            }
            FaultClass::SequenceRegress => {
                // Below the seeded last_accepted ⇒ regress.
                CommandFrame::well_formed(SEED_SEQUENCE - 500, FUTURE_DEADLINE_NANOS, 5.0, 0.2)
            }
            FaultClass::Replay => {
                // Exactly the seeded last_accepted ⇒ replay (the corrected `<=`).
                CommandFrame::well_formed(SEED_SEQUENCE, FUTURE_DEADLINE_NANOS, 5.0, 0.2)
            }
            FaultClass::DeadlineMissed => {
                CommandFrame::well_formed(1, PAST_DEADLINE_NANOS, 5.0, 0.2)
            }
            FaultClass::IntegrityFlag => {
                let mut f = CommandFrame::well_formed(1, FUTURE_DEADLINE_NANOS, 5.0, 0.2);
                f.integrity_flag = 0;
                f
            }
            FaultClass::PayloadCorrupt => {
                let mut f = CommandFrame::well_formed(1, FUTURE_DEADLINE_NANOS, 5.0, 0.2);
                f.payload[0] ^= 0xFF; // mutate bytes WITHOUT recomputing the CRC
                f
            }
            FaultClass::PayloadOversize => {
                let mut f = CommandFrame::well_formed(1, FUTURE_DEADLINE_NANOS, 5.0, 0.2);
                f.declared_len = (MAX_PAYLOAD_LEN + 1) as u16;
                f
            }
            FaultClass::KinematicLimit => {
                // Linear speed far beyond the PROXY envelope.
                CommandFrame::well_formed(1, FUTURE_DEADLINE_NANOS, 999.0, 0.0)
            }
        }
    }
}

/// One row of the fault matrix.
#[derive(Clone, Debug)]
pub struct RowResult {
    pub class: FaultClass,
    pub samples: usize,
    pub all_correct: bool,
    pub expected: Outcome,
    pub observed: Outcome,
    pub p50: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl RowResult {
    pub fn pass(&self) -> bool {
        self.all_correct
    }
}

fn percentile(sorted: &[Duration], pct: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((pct / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// A live iceoryx2 publish/subscribe channel for `CommandFrame`, used by the
/// matrix. The publisher and subscriber are real iceoryx2 endpoints over the
/// `ipc` service (genuine zero-copy shared memory); the harness creates them in
/// one process for a deterministic matrix, while the binary's `--two-process`
/// mode demonstrates the production shape.
pub struct Channel {
    publisher: Publisher<ipc::Service, CommandFrame, ()>,
    subscriber: Subscriber<ipc::Service, CommandFrame, ()>,
}

impl Channel {
    pub fn create(service_name: &str) -> Result<Self, Box<dyn core::error::Error>> {
        let node = NodeBuilder::new().create::<ipc::Service>()?;
        let service = node
            .service_builder(&service_name.try_into()?)
            .publish_subscribe::<CommandFrame>()
            .open_or_create()?;
        let publisher = service.publisher_builder().create()?;
        let subscriber = service.subscriber_builder().create()?;
        Ok(Self { publisher, subscriber })
    }

    /// Publish one frame and return the RECEIVED copy (proves the round-trip).
    fn round_trip(&self, frame: CommandFrame) -> Result<CommandFrame, Box<dyn core::error::Error>> {
        let sample = self.publisher.loan_uninit()?;
        let sample = sample.write_payload(frame);
        sample.send()?;
        // Drain to the most recent sample for this 1:1 exchange.
        let mut received = None;
        while let Some(sample) = self.subscriber.receive()? {
            received = Some(*sample);
        }
        received.ok_or_else(|| "no sample received over iceoryx2".into())
    }

    /// Run one fault class `samples` times through the channel, timing the
    /// publish→zero-copy→validate→judge path and asserting verdict correctness.
    pub fn run_class(
        &self,
        class: FaultClass,
        samples: usize,
    ) -> Result<RowResult, Box<dyn core::error::Error>> {
        let mut state = class.initial_state();
        let expected = class.expected();
        let mut all_correct = true;
        let mut observed = expected; // overwritten by the first real outcome
        let mut durations: Vec<Duration> = Vec::with_capacity(samples);

        for i in 0..samples as u64 {
            let frame = class.build_frame(i);
            let start = Instant::now();
            let received = self.round_trip(frame)?;
            let out = process(&received, &mut state, LOGICAL_NOW_NANOS);
            durations.push(start.elapsed());
            observed = out;
            if out != expected {
                all_correct = false;
            }
        }

        durations.sort_unstable();
        Ok(RowResult {
            class,
            samples,
            all_correct,
            expected,
            observed,
            p50: percentile(&durations, 50.0),
            p99: percentile(&durations, 99.0),
            max: durations.last().copied().unwrap_or(Duration::ZERO),
        })
    }
}

/// Run the full fault matrix over a fresh iceoryx2 channel. `warmup` valid
/// round-trips prime the path before the timed `samples` per class.
pub fn run_matrix(
    service_name: &str,
    warmup: usize,
    samples: usize,
) -> Result<Vec<RowResult>, Box<dyn core::error::Error>> {
    let channel = Channel::create(service_name)?;

    // Warmup (not timed, not asserted) — first-touch shared-memory + cache.
    let warm = FaultClass::Valid;
    let _ = channel.run_class(warm, warmup.max(1))?;

    let mut rows = Vec::new();
    for class in FaultClass::ALL {
        rows.push(channel.run_class(class, samples)?);
    }
    Ok(rows)
}
