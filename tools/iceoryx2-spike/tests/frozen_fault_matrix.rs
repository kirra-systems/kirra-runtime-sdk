// frozen_fault_matrix.rs — the iceoryx2 carrier driving the FROZEN
// `GovernorContractView` through the PRODUCTION `validate()` over a real
// zero-copy channel (#275 / L2). Mirrors the #273 fault_matrix.rs, but the wire
// type and the checker are now the production contract, not the spike's ad-hoc
// CommandFrame/judge.

use iceoryx2_spike::frozen::{run_frozen_matrix, FaultKind, FrozenChannel, FrozenFault};

/// Every transport-contract fault class published as a frozen `WireView`,
/// received zero-copy, and validated with `kirra_contract_channel::validate`
/// produces exactly its expected `FaultKind`. GATE = verdict correctness over the
/// real iceoryx2 transport.
#[test]
fn frozen_matrix_all_classes_correct() {
    let rows = run_frozen_matrix("kirra-frozen-fault-matrix").expect("matrix runs over iceoryx2");
    assert_eq!(rows.len(), FrozenFault::ALL.len());
    for row in &rows {
        assert!(
            row.correct,
            "class {:?}: expected {:?}, observed {:?}",
            row.class.name(),
            row.expected,
            row.observed,
        );
    }
}

/// A well-formed frozen view round-trips through iceoryx2 and is accepted —
/// proves the 176-byte contract image crosses the zero-copy transport intact and
/// the production validator admits it.
#[test]
fn valid_frozen_view_round_trips_and_accepts() {
    let channel = FrozenChannel::create("kirra-frozen-valid").expect("channel");
    let observed = channel.run_class(FrozenFault::Valid).expect("round trip");
    assert_eq!(observed, FaultKind::Ok);
}

/// The replay class (sequence == last-accepted) is rejected by the production
/// `<=` rule after crossing the transport — the load-bearing anti-replay property.
#[test]
fn replay_is_rejected_over_the_transport() {
    let channel = FrozenChannel::create("kirra-frozen-replay").expect("channel");
    let observed = channel.run_class(FrozenFault::Replay).expect("round trip");
    assert_eq!(observed, FaultKind::SeqRegressOrReplay);
}
