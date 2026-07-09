// frozen.rs — the iceoryx2 carrier promoted to the FROZEN cross-partition
// contract (#275 / L2, HVCHAN-001 / ADR-0006).
//
// Where `wire.rs`/`judge.rs` are the original #273 feature-subset spike (their own
// ad-hoc `CommandFrame` + judge), THIS module carries the PRODUCTION frozen
// `GovernorContractView` (from `kirra-contract-channel`, the zero-dep no_std core)
// over a real iceoryx2 zero-copy channel and validates each received sample with
// the PRODUCTION `validate()` pipeline. So the iceoryx2 path now exercises the
// same contract + trust-chain checks the QNX harness and the hypervisor-SHM seam
// use — not a spike-local copy.
//
// ISOLATION (unchanged, #275 gate): this crate is still its own workspace and
// iceoryx2 still never enters the SDK/parko dependency tree. The dependency
// direction is spike → kirra-contract-channel (a path dep on the lean core), never
// the reverse — so the core stays `#![forbid(unsafe_code)]`, zero-dep, no_std.
//
// SCOPE: `validate()` is the TRANSPORT-CONTRACT checker (layout/magic/bounds/CRC/
// sequence/generation/deadline — HVCHAN-001 §4 contract-discipline + judge rows).
// The kinematic envelope is a SEPARATE downstream governor step (the talisman
// `VehicleKinematicsContract`), not part of the transport contract, so it is out
// of scope here — exactly as `kirra_contract_channel::validate` is.

use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;

use kirra_contract_channel::{
    validate, AcceptedWatermark, ContractFault, GovernorContractView, MAX_COMMAND_BYTES,
};

/// A fixed `now` in the **boundary clock domain** (HVCHAN-001 §5 R-HV-3), handed
/// to `validate`. Fixed so the deadline verdict is deterministic; transport
/// timing is measured separately and never feeds a verdict.
pub const LOGICAL_NOW_NANOS: u64 = 1_000_000_000;
const FUTURE_DEADLINE_NANOS: u64 = u64::MAX / 2;
const PAST_DEADLINE_NANOS: u64 = 0;
/// Seed `(generation, sequence)` for the classes that need a prior accepted
/// command (replay / regress). Even generation = committed.
const SEED_GENERATION: u64 = 10;
const SEED_SEQUENCE: u64 = 1_000;

/// The frozen `GovernorContractView`, wrapped so this crate can assert it is
/// `ZeroCopySend` for iceoryx2 transport. The orphan rule forbids implementing
/// the foreign `ZeroCopySend` on the foreign `GovernorContractView` directly, so
/// a `#[repr(transparent)]` newtype carries the assertion — and being
/// `transparent`, its memory layout IS the frozen 176-byte contract image.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct WireView(pub GovernorContractView);

// SAFETY: `GovernorContractView` is the frozen `#[repr(C)]`, fixed-size (176 B),
// **pointer-free** (the command rides by value), `Copy` contract image — its
// freeze assertions pin every offset. It contains no references, no interior
// pointers, no padding-dependent invariants, so its bytes are self-contained and
// safe to share across a process/partition boundary by value. `WireView` is
// `#[repr(transparent)]` over it, so the same holds. This is the single, audited
// `unsafe` the carrier needs (ADR-0006 Clause 3 integration boundary).
unsafe impl ZeroCopySend for WireView {}

/// The transport-contract fault classes the carrier drives through iceoryx2, each
/// mapping to exactly one `validate()` outcome. (Mirrors the §4 taxonomy that
/// `kirra_contract_channel::validate` enforces.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrozenFault {
    Valid,
    LayoutVersionMismatch,
    MagicMismatch,
    CommandLenOversize,
    CrcMismatch,
    SequenceRegress,
    Replay,
    GenerationRegress,
    DeadlineExpired,
}

/// A flattened discriminant of `Result<(), ContractFault>` so the matrix can
/// assert the EXPECTED reject class without depending on each fault's payload
/// fields (found/expected/etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    Ok,
    LayoutVersion,
    Magic,
    Oversize,
    Crc,
    SeqRegressOrReplay,
    GenRegressOrReplay,
    Deadline,
}

impl FaultKind {
    fn of(r: &Result<(), ContractFault>) -> Self {
        match r {
            Ok(()) => FaultKind::Ok,
            Err(ContractFault::LayoutVersionMismatch { .. }) => FaultKind::LayoutVersion,
            Err(ContractFault::MagicMismatch { .. }) => FaultKind::Magic,
            Err(ContractFault::CommandLenOversize { .. }) => FaultKind::Oversize,
            Err(ContractFault::CrcMismatch { .. }) => FaultKind::Crc,
            Err(ContractFault::SequenceRegressOrReplay { .. }) => FaultKind::SeqRegressOrReplay,
            Err(ContractFault::GenerationRegressOrReplay { .. }) => FaultKind::GenRegressOrReplay,
            Err(ContractFault::DeadlineExpired { .. }) => FaultKind::Deadline,
        }
    }
}

impl FrozenFault {
    pub const ALL: [FrozenFault; 9] = [
        FrozenFault::Valid,
        FrozenFault::LayoutVersionMismatch,
        FrozenFault::MagicMismatch,
        FrozenFault::CommandLenOversize,
        FrozenFault::CrcMismatch,
        FrozenFault::SequenceRegress,
        FrozenFault::Replay,
        FrozenFault::GenerationRegress,
        FrozenFault::DeadlineExpired,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FrozenFault::Valid => "Valid",
            FrozenFault::LayoutVersionMismatch => "LayoutVersionMismatch",
            FrozenFault::MagicMismatch => "MagicMismatch",
            FrozenFault::CommandLenOversize => "CommandLenOversize",
            FrozenFault::CrcMismatch => "CrcMismatch",
            FrozenFault::SequenceRegress => "SequenceRegress",
            FrozenFault::Replay => "Replay (seq == last)",
            FrozenFault::GenerationRegress => "GenerationRegress",
            FrozenFault::DeadlineExpired => "DeadlineExpired",
        }
    }

    /// The `FaultKind` a correct governor MUST produce for this class.
    pub fn expected(self) -> FaultKind {
        match self {
            FrozenFault::Valid => FaultKind::Ok,
            FrozenFault::LayoutVersionMismatch => FaultKind::LayoutVersion,
            FrozenFault::MagicMismatch => FaultKind::Magic,
            FrozenFault::CommandLenOversize => FaultKind::Oversize,
            FrozenFault::CrcMismatch => FaultKind::Crc,
            FrozenFault::SequenceRegress | FrozenFault::Replay => FaultKind::SeqRegressOrReplay,
            FrozenFault::GenerationRegress => FaultKind::GenRegressOrReplay,
            FrozenFault::DeadlineExpired => FaultKind::Deadline,
        }
    }

    /// The watermark appropriate for this class. Replay/regress classes need a
    /// prior accepted `(generation, sequence)`; the rest start fresh.
    fn watermark(self) -> AcceptedWatermark {
        match self {
            FrozenFault::SequenceRegress | FrozenFault::Replay | FrozenFault::GenerationRegress => {
                let mut wm = AcceptedWatermark::new();
                // Only the seed's generation+sequence are used by the watermark,
                // but `record`'s documented precondition is that `validate()` would
                // pass for the recorded view — so seed with a fully-valid command
                // (future deadline), not a deadline-0 view that would fail at
                // LOGICAL_NOW. Keeps the test aligned with the production contract.
                let seed = GovernorContractView::new_command(
                    SEED_GENERATION,
                    SEED_SEQUENCE,
                    0,
                    FUTURE_DEADLINE_NANOS,
                    b"seed",
                )
                .unwrap();
                wm.record(&seed);
                wm
            }
            _ => AcceptedWatermark::new(),
        }
    }

    /// Build the (possibly faulted) frozen view for this class. A well-formed
    /// `new_command` view is mutated in place to inject exactly one fault, mirror-
    /// ing the `validate` unit tests.
    fn build_view(self) -> GovernorContractView {
        // A well-formed, strictly-newer-than-seed admissible base.
        let base = || {
            GovernorContractView::new_command(
                SEED_GENERATION + 2,
                SEED_SEQUENCE + 1,
                1,
                FUTURE_DEADLINE_NANOS,
                b"steer:1.5",
            )
            .unwrap()
        };
        match self {
            FrozenFault::Valid => base(),
            FrozenFault::LayoutVersionMismatch => {
                let mut v = base();
                v.layout_version = 999;
                v
            }
            FrozenFault::MagicMismatch => {
                let mut v = base();
                v.magic = 0xDEAD_BEEF;
                v
            }
            FrozenFault::CommandLenOversize => {
                let mut v = base();
                v.command_len = (MAX_COMMAND_BYTES + 1) as u32;
                v
            }
            FrozenFault::CrcMismatch => {
                let mut v = base();
                v.command[0] ^= 0xFF; // flip a payload byte; the crc32 field is now stale
                v
            }
            FrozenFault::SequenceRegress => {
                // Below the seeded last_accepted sequence ⇒ regress.
                GovernorContractView::new_command(
                    SEED_GENERATION + 2,
                    SEED_SEQUENCE - 500,
                    1,
                    FUTURE_DEADLINE_NANOS,
                    b"steer:1.5",
                )
                .unwrap()
            }
            FrozenFault::Replay => {
                // Exactly the seeded last_accepted sequence ⇒ replay (the `<=` rule).
                GovernorContractView::new_command(
                    SEED_GENERATION + 2,
                    SEED_SEQUENCE,
                    1,
                    FUTURE_DEADLINE_NANOS,
                    b"steer:1.5",
                )
                .unwrap()
            }
            FrozenFault::GenerationRegress => {
                // Newer sequence (passes its check) but an equal generation ⇒
                // the generation monotonicity rejects.
                GovernorContractView::new_command(
                    SEED_GENERATION,
                    SEED_SEQUENCE + 1,
                    1,
                    FUTURE_DEADLINE_NANOS,
                    b"steer:1.5",
                )
                .unwrap()
            }
            FrozenFault::DeadlineExpired => {
                let mut v = base();
                v.deadline_nanos = PAST_DEADLINE_NANOS; // now (LOGICAL_NOW) > deadline
                v
            }
        }
    }
}

/// A live iceoryx2 publish/subscribe channel carrying the frozen `WireView`.
/// Real iceoryx2 endpoints over the `ipc` service (genuine zero-copy shared
/// memory); created in one process for a deterministic matrix.
pub struct FrozenChannel {
    publisher: Publisher<ipc::Service, WireView, ()>,
    subscriber: Subscriber<ipc::Service, WireView, ()>,
}

impl FrozenChannel {
    pub fn create(service_name: &str) -> Result<Self, Box<dyn core::error::Error>> {
        let node = NodeBuilder::new().create::<ipc::Service>()?;
        let service = node
            .service_builder(&service_name.try_into()?)
            .publish_subscribe::<WireView>()
            .open_or_create()?;
        let publisher = service.publisher_builder().create()?;
        let subscriber = service.subscriber_builder().create()?;
        Ok(Self {
            publisher,
            subscriber,
        })
    }

    /// Publish one frozen view and return the RECEIVED owned copy (proves the
    /// zero-copy round-trip — the bytes crossed the transport intact). Public so
    /// the latency benchmark can time the bare transport hop.
    pub fn round_trip(
        &self,
        view: GovernorContractView,
    ) -> Result<GovernorContractView, Box<dyn core::error::Error>> {
        let sample = self.publisher.loan_uninit()?;
        let sample = sample.write_payload(WireView(view));
        sample.send()?;
        let mut received = None;
        while let Some(sample) = self.subscriber.receive()? {
            // `sample` derefs to the `WireView` payload; destructure to the view.
            let WireView(view) = *sample;
            received = Some(view);
        }
        received.ok_or_else(|| "no sample received over iceoryx2".into())
    }

    /// Drive one fault class through the channel and validate the RECEIVED owned
    /// snapshot with the production `validate()`. Returns the observed `FaultKind`.
    pub fn run_class(&self, class: FrozenFault) -> Result<FaultKind, Box<dyn core::error::Error>> {
        let received = self.round_trip(class.build_view())?;
        let verdict = validate(&received, LOGICAL_NOW_NANOS, &class.watermark());
        Ok(FaultKind::of(&verdict))
    }
}

/// One row of the frozen-contract fault matrix.
#[derive(Clone, Copy, Debug)]
pub struct FrozenRow {
    pub class: FrozenFault,
    pub expected: FaultKind,
    pub observed: FaultKind,
    pub correct: bool,
}

/// Run the full frozen-contract fault matrix over a fresh iceoryx2 channel: each
/// class is published as a `WireView`, received zero-copy, and checked with the
/// production `validate()`. Every row's observed verdict must equal its expected
/// `FaultKind` — the GATE is verdict correctness over the real transport.
pub fn run_frozen_matrix(
    service_name: &str,
) -> Result<Vec<FrozenRow>, Box<dyn core::error::Error>> {
    let channel = FrozenChannel::create(service_name)?;
    let mut rows = Vec::new();
    for class in FrozenFault::ALL {
        let expected = class.expected();
        let observed = channel.run_class(class)?;
        rows.push(FrozenRow {
            class,
            expected,
            observed,
            correct: observed == expected,
        });
    }
    Ok(rows)
}
