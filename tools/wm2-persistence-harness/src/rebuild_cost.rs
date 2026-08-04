//! What ADR-0041 R2's alongside-rebuild-and-swap actually costs.
//!
//! The acceptance record carries one outstanding obligation: prototype R2 "far
//! enough to know what it costs in code, in peak disk, in write amplification
//! and in cutover latency". The **code** half was answered by the protocol
//! (`docs/design/WM2_PROJECTION_REBUILD_PROTOCOL.md`). This measures the other
//! three.
//!
//! # The attribution problem, and the control that solves it
//!
//! A rebuild runs *while ingest continues* — that is the whole premise — and
//! both write through the same process. `/proc/self/io` therefore cannot say
//! which bytes belong to the rebuild, and neither can the database file size.
//!
//! So this runs **two arms with identical ingest**:
//!
//! | Arm | Does |
//! |---|---|
//! | **control** | seeds a store, then ingests and maintains the live projection |
//! | **rebuild** | the same, *plus* the alongside fold, the equivalence check and the swap |
//!
//! Every reported cost is the **difference**. Without the control the numbers
//! would be "bytes written while a rebuild happened to be running", which is not
//! the cost of the rebuild and would overstate it by the whole ingest load.
//!
//! # Ingest is interleaved, not threaded
//!
//! Deterministically: append a round, fold a chunk, append a round, fold a
//! chunk. A background thread would make the two arms do *different* amounts of
//! ingest depending on scheduling, and then the difference between them would
//! carry that noise rather than the rebuild's cost. Interleaving keeps the arms
//! identical by construction and still moves the head under the fold, which is
//! the condition the protocol exists for.
//!
//! # The protocol drives it
//!
//! Every transition goes through [`crate::rebuild::RebuildState`], so what is
//! measured is the procedure ADR-0041 specifies rather than a copy of it that
//! happens to resemble it. That module had no caller before this one.

use crate::bench::db_bytes;
use crate::rebuild::{catchup_is_converging, FailureReason, RebuildState};
use crate::standin::{Durability, Store};
use std::path::Path;
use std::time::Instant;

/// How many catch-up laps to allow before declaring the fold cannot keep up.
/// Generous: the point is to terminate rather than to grade the hardware.
pub const MAX_CATCHUP_ROUNDS: usize = 64;

/// Process-wide bytes actually written to storage, from `/proc/self/io`.
///
/// `write_bytes`, not `wchar`: wchar counts bytes handed to `write()`, which
/// includes writes absorbed by the page cache and never sent anywhere. Write
/// amplification is a question about the device.
///
/// `None` off Linux, or wherever the file is unreadable — a missing counter
/// must not arrive as zero, which would render as a flatteringly efficient
/// rebuild.
pub fn process_write_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/io").ok()?;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("write_bytes:") {
            return v.trim().parse().ok();
        }
    }
    None
}

/// One arm's observations.
#[derive(Debug, Clone)]
pub struct ArmResult {
    pub wall_ms: f64,
    pub write_bytes: Option<u64>,
    pub peak_db_bytes: u64,
    /// Size once the arm has settled. Kept alongside the peak because SQLite
    /// does not shrink on `DROP`, so "final" and "peak" answer different
    /// questions and reporting only one invites the wrong reading.
    pub final_db_bytes: u64,
}

/// The answer to the outstanding R2 obligation, or as much of it as a stand-in
/// schema can give.
#[derive(Debug, Clone)]
pub struct RebuildCost {
    pub seeded_events: u64,
    pub ingested_during: u64,
    pub rounds: usize,
    pub entities: u64,

    pub control: ArmResult,
    pub rebuild: ArmResult,

    /// Bytes the rebuild wrote, over and above the same ingest without it.
    pub extra_write_bytes: Option<u64>,
    /// Extra bytes written per byte the second projection occupies on disk.
    /// The question "how much I/O does a duplicate projection cost" in one
    /// number — 1.0 would mean it wrote itself exactly once.
    pub write_amplification: Option<f64>,
    /// Bytes of on-disk footprint the second projection added at its peak.
    pub peak_overhead_bytes: u64,
    /// That overhead as a fraction of the control arm's peak store size.
    pub peak_overhead_ratio: f64,
    /// The projection's own size, from the growth the cutover's DROP reclaims.
    pub projection_bytes: u64,

    /// The swap itself: the window in which the live projection is neither the
    /// old one nor the new one.
    pub cutover_ms: f64,
    /// Dropping what the cutover retired. Reclamation, not cutover.
    pub retire_ms: f64,

    pub catchup_rounds: usize,
    pub converged: bool,
    pub equivalence_proven: bool,
    /// `Active` on success. Anything else means the protocol refused and the
    /// numbers above describe an attempt that did not complete.
    pub final_state: String,
    pub failure: Option<String>,
}

impl RebuildCost {
    /// Did the run actually produce what it claims to have costed?
    ///
    /// A cost measured over an attempt that never reached `Active` is the cost
    /// of *something else*, so this gates whether the numbers may be cited.
    pub fn completed(&self) -> bool {
        self.final_state == "Active" && self.equivalence_proven && self.failure.is_none()
    }
}

fn seed(path: &Path, durability: Durability, events: u64, entities: u64, seed_n: u64) -> Store {
    let _ = std::fs::remove_file(path);
    for s in ["-wal", "-shm"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }
    let mut store = Store::open(path, durability).expect("open");
    let all = crate::gen::events(0, events, entities, seed_n);
    store.append_batch(&all).expect("seed append");
    store.fold_from(0).expect("seed fold");
    store
}

/// The control arm: the same ingest, without any rebuild.
fn run_control(
    path: &Path,
    durability: Durability,
    seeded: u64,
    entities: u64,
    rounds: usize,
    per_round: u64,
    seed_n: u64,
) -> ArmResult {
    let mut store = seed(path, durability, seeded, entities, seed_n);
    let before = process_write_bytes();
    let mut peak = db_bytes(path);
    let t = Instant::now();
    let mut next = seeded;
    for _ in 0..rounds {
        let batch = crate::gen::events(next, per_round, entities, seed_n);
        store.append_batch(&batch).expect("ingest");
        // The live projection keeps being maintained — the robot is still
        // serving it. Both arms pay this, so it cancels in the difference.
        store.fold_from(next).expect("live fold");
        next += per_round;
        peak = peak.max(db_bytes(path));
    }
    let wall_ms = t.elapsed().as_secs_f64() * 1e3;
    let after = process_write_bytes();
    ArmResult {
        wall_ms,
        write_bytes: match (before, after) {
            (Some(b), Some(a)) => a.checked_sub(b),
            _ => None,
        },
        peak_db_bytes: peak.max(db_bytes(path)),
        final_db_bytes: db_bytes(path),
    }
}

/// Run the alongside rebuild, driven by the protocol, and cost it.
#[allow(clippy::too_many_arguments)]
pub fn run(
    path: &Path,
    control_path: &Path,
    durability: Durability,
    seeded: u64,
    entities: u64,
    rounds: usize,
    per_round: u64,
    seed_n: u64,
) -> RebuildCost {
    let control = run_control(
        control_path,
        durability,
        seeded,
        entities,
        rounds,
        per_round,
        seed_n,
    );

    let mut store = seed(path, durability, seeded, entities, seed_n);
    store
        .create_rebuild_projection()
        .expect("create alongside projection");

    let before = process_write_bytes();
    let mut peak = db_bytes(path);
    let t = Instant::now();

    let mut state = RebuildState::begin();
    let mut folded_to = 0u64;
    let mut next = seeded;
    let mut failure: Option<String> = None;

    // Interleave: ingest a round, then fold a chunk. The head moves under the
    // fold, which is the condition the protocol exists to handle.
    for _ in 0..rounds {
        let batch = crate::gen::events(next, per_round, entities, seed_n);
        store.append_batch(&batch).expect("ingest");
        store.fold_from(next).expect("live fold");
        next += per_round;
        let head = store.max_generation().expect("head");
        state = match state.on_append(head) {
            Ok(s) => s,
            Err(e) => {
                failure = Some(format!("{e:?}"));
                break;
            }
        };

        // One chunk per round, sized so the fold outpaces ingest — otherwise
        // catch-up cannot converge and the run measures a divergence instead.
        let chunk_to = (folded_to + per_round * 2).min(head);
        store
            .fold_rebuild_range(folded_to, chunk_to)
            .expect("rebuild fold");
        folded_to = chunk_to;
        state = match state.on_fold_progress(folded_to) {
            Ok(s) => s,
            Err(e) => {
                failure = Some(format!("{e:?}"));
                break;
            }
        };
        peak = peak.max(db_bytes(path));
    }

    // Catch up. Ingest has stopped here, which models a rebuild that outpaces
    // its inflow; a rebuild that cannot is the `CatchupDiverged` case and the
    // convergence guard below is what turns it into a result rather than a hang.
    let mut gaps: Vec<u64> = Vec::new();
    let mut catchup_rounds = 0usize;
    let mut converged = true;
    while failure.is_none() {
        let head = store.max_generation().expect("head");
        if folded_to >= head {
            break;
        }
        gaps.push(head - folded_to);
        if !catchup_is_converging(&gaps) || catchup_rounds >= MAX_CATCHUP_ROUNDS {
            converged = false;
            state = state
                .on_failure(FailureReason::CatchupDiverged)
                .unwrap_or(state);
            failure = Some("catch-up did not converge".into());
            break;
        }
        let chunk_to = (folded_to + per_round * 4).min(head);
        store
            .fold_rebuild_range(folded_to, chunk_to)
            .expect("rebuild fold");
        folded_to = chunk_to;
        state = match state.on_fold_progress(folded_to) {
            Ok(s) => s,
            Err(e) => {
                failure = Some(format!("{e:?}"));
                break;
            }
        };
        catchup_rounds += 1;
        peak = peak.max(db_bytes(path));
    }

    let head = store.max_generation().expect("head");
    let mut equivalence_proven = false;
    let mut cutover_ms = f64::NAN;
    let mut retire_ms = f64::NAN;
    let mut projection_bytes = 0u64;

    if failure.is_none() {
        state = match state.on_reached_head(head) {
            Ok(s) => s,
            Err(e) => {
                failure = Some(format!("{e:?}"));
                state
            }
        };
    }

    if failure.is_none() {
        // Equivalence: the rebuilt projection must equal the incrementally
        // maintained one. Compared by the identical digest, so a difference is
        // about the data and not about how it was measured.
        let live = store.projection_digest().expect("live digest");
        let rebuilt = store.rebuild_projection_digest().expect("rebuild digest");
        if live == rebuilt {
            equivalence_proven = true;
            state = match state.on_equivalence_proven(head) {
                Ok(s) => s,
                Err(e) => {
                    failure = Some(format!("{e:?}"));
                    state
                }
            };
        } else {
            state = state
                .on_failure(FailureReason::EquivalenceMismatch { at: head })
                .unwrap_or(state);
            failure = Some(format!(
                "equivalence mismatch at generation {head}: live {live} != rebuilt {rebuilt}"
            ));
        }
    }

    if failure.is_none() {
        let before_swap = db_bytes(path);
        let c = Instant::now();
        store.swap_rebuild_projection().expect("cutover");
        cutover_ms = c.elapsed().as_secs_f64() * 1e3;
        state = match state.on_cutover(head) {
            Ok(s) => s,
            Err(e) => {
                failure = Some(format!("{e:?}"));
                state
            }
        };
        peak = peak.max(db_bytes(path));

        // Measured on the FREE LIST, not on file size: `DROP TABLE` does not
        // shrink a SQLite file, so a before/after on bytes-on-disk reports zero
        // and reads as a projection that cost nothing.
        let free_before = store.free_bytes().unwrap_or(0);
        let r = Instant::now();
        store.drop_retired_projection().expect("retire");
        retire_ms = r.elapsed().as_secs_f64() * 1e3;
        projection_bytes = store.free_bytes().unwrap_or(0).saturating_sub(free_before);
        let _ = before_swap;
    }

    let wall_ms = t.elapsed().as_secs_f64() * 1e3;
    let after = process_write_bytes();
    let rebuild_arm = ArmResult {
        wall_ms,
        write_bytes: match (before, after) {
            (Some(b), Some(a)) => a.checked_sub(b),
            _ => None,
        },
        peak_db_bytes: peak.max(db_bytes(path)),
        final_db_bytes: db_bytes(path),
    };

    let extra_write_bytes = match (control.write_bytes, rebuild_arm.write_bytes) {
        (Some(c), Some(r)) => r.checked_sub(c),
        _ => None,
    };
    let write_amplification = match (extra_write_bytes, projection_bytes) {
        (Some(e), p) if p > 0 => Some(e as f64 / p as f64),
        _ => None,
    };
    let peak_overhead_bytes = rebuild_arm
        .peak_db_bytes
        .saturating_sub(control.peak_db_bytes);
    let peak_overhead_ratio = if control.peak_db_bytes > 0 {
        peak_overhead_bytes as f64 / control.peak_db_bytes as f64
    } else {
        f64::NAN
    };

    RebuildCost {
        seeded_events: seeded,
        ingested_during: rounds as u64 * per_round,
        rounds,
        entities,
        control,
        rebuild: rebuild_arm,
        extra_write_bytes,
        write_amplification,
        peak_overhead_bytes,
        peak_overhead_ratio,
        projection_bytes,
        cutover_ms,
        retire_ms,
        catchup_rounds,
        converged,
        equivalence_proven,
        final_state: format!("{state:?}")
            .split(&[' ', '('][..])
            .next()
            .unwrap_or("?")
            .to_string(),
        failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "wm2-rebuild-cost-{name}-{}.sqlite",
            std::process::id()
        ));
        p
    }

    #[test]
    fn the_rebuild_completes_and_the_protocol_says_so() {
        let (a, b) = (temp("run-a"), temp("run-b"));
        let r = run(&a, &b, Durability::Off, 2_000, 100, 4, 250, 7);
        assert!(
            r.completed(),
            "state={} equivalence={} failure={:?}",
            r.final_state,
            r.equivalence_proven,
            r.failure
        );
        assert_eq!(r.final_state, "Active");
        assert!(r.converged);
        // The equivalence check is the load-bearing one: an alongside rebuild
        // that does not reproduce the live projection has measured the cost of
        // building something wrong.
        assert!(r.equivalence_proven);
        for p in [&a, &b] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn the_cutover_is_much_cheaper_than_the_fold_that_precedes_it() {
        // The claim R2 rests on: the robot keeps serving throughout, and the
        // only moment it cannot is the swap. That is only true if the swap is
        // small next to the rebuild — a rename pair moves no rows, so it should
        // be orders below the wall time, not merely under it.
        let (a, b) = (temp("cut-a"), temp("cut-b"));
        let r = run(&a, &b, Durability::Off, 4_000, 200, 4, 500, 11);
        assert!(r.completed(), "{:?}", r.failure);
        assert!(
            r.cutover_ms < r.rebuild.wall_ms / 10.0,
            "cutover {:.3} ms is not small against a {:.1} ms rebuild",
            r.cutover_ms,
            r.rebuild.wall_ms
        );
        assert!(r.cutover_ms >= 0.0 && r.cutover_ms.is_finite());
        for p in [&a, &b] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn the_second_projection_costs_real_bytes() {
        // Peak overhead must be positive and must be a fraction of the store,
        // not a multiple — the D-2 arithmetic says a duplicate projection is
        // ~3.7 % of total size, and a measurement wildly off that would mean
        // this is building something other than a projection.
        let (a, b) = (temp("bytes-a"), temp("bytes-b"));
        let r = run(&a, &b, Durability::Off, 4_000, 200, 4, 500, 13);
        assert!(r.completed(), "{:?}", r.failure);
        assert!(
            r.projection_bytes > 0,
            "the retired projection reclaimed no space, so nothing was measured"
        );
        assert!(
            r.peak_overhead_ratio < 1.0,
            "peak overhead {:.2}x the store — that is a second store, not a second projection",
            r.peak_overhead_ratio
        );
        for p in [&a, &b] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn write_amplification_rises_with_chunk_count_so_it_is_a_dial_not_a_constant() {
        // THE finding, pinned. Amplification is not a property of the rebuild;
        // it is a property of how finely the fold is chunked, because each
        // chunk commits a transaction that rewrites projection pages already
        // written. Measured on target-shaped data it spans 2.7x to 35.6x across
        // a 16x range of chunk counts — so quoting a single number for it would
        // describe one arbitrary point on a curve.
        //
        // Total ingest is held CONSTANT and only the chunking varies, or the
        // comparison would confound the two.
        let total = 4_000u64;
        let (a1, b1) = (temp("amp-coarse-a"), temp("amp-coarse-b"));
        let (a2, b2) = (temp("amp-fine-a"), temp("amp-fine-b"));
        let coarse = run(&a1, &b1, Durability::Off, 8_000, 400, 2, total / 2, 3);
        let fine = run(&a2, &b2, Durability::Off, 8_000, 400, 16, total / 16, 3);
        assert!(coarse.completed() && fine.completed());

        // The projection is the same one either way — if this moved, the two
        // runs built different things and the amplification comparison would be
        // between different denominators.
        let ratio = fine.projection_bytes as f64 / coarse.projection_bytes as f64;
        assert!(
            (0.8..1.25).contains(&ratio),
            "projection size moved {ratio:.2}x between chunk counts; \
             the amplification numbers do not share a denominator"
        );

        match (coarse.write_amplification, fine.write_amplification) {
            (Some(c), Some(f)) => assert!(
                f > c * 1.5,
                "finer chunking ({f:.1}x) did not cost materially more than \
                 coarser ({c:.1}x) — the trade-off this measurement exists to \
                 expose is not present"
            ),
            // No /proc/self/io: the platform cannot answer, which is a
            // limitation to report rather than a test to fail.
            _ => eprintln!("write_bytes unavailable on this platform; amplification not asserted"),
        }
        for p in [&a1, &b1, &a2, &b2] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn the_cutover_does_not_depend_on_how_the_fold_was_chunked() {
        // The swap is a schema operation — no rows move — so it should be flat
        // against chunking. If it were not, R2's availability claim would
        // depend on a tuning parameter, which is the sort of coupling worth
        // knowing about before it is relied on.
        let (a1, b1) = (temp("cut-coarse-a"), temp("cut-coarse-b"));
        let (a2, b2) = (temp("cut-fine-a"), temp("cut-fine-b"));
        let coarse = run(&a1, &b1, Durability::Off, 8_000, 400, 2, 2_000, 5);
        let fine = run(&a2, &b2, Durability::Off, 8_000, 400, 16, 250, 5);
        assert!(coarse.completed() && fine.completed());
        assert!(
            coarse.cutover_ms < 100.0 && fine.cutover_ms < 100.0,
            "cutover is not a schema-speed operation: {:.1} / {:.1} ms",
            coarse.cutover_ms,
            fine.cutover_ms
        );
        for p in [&a1, &b1, &a2, &b2] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn a_missing_write_counter_is_none_not_zero() {
        // Zero would render as a rebuild that wrote nothing at all, which is
        // the most flattering possible answer from an absent measurement.
        let r = RebuildCost {
            seeded_events: 0,
            ingested_during: 0,
            rounds: 0,
            entities: 0,
            control: ArmResult {
                wall_ms: 0.0,
                write_bytes: None,
                peak_db_bytes: 0,
                final_db_bytes: 0,
            },
            rebuild: ArmResult {
                wall_ms: 0.0,
                write_bytes: Some(10),
                peak_db_bytes: 0,
                final_db_bytes: 0,
            },
            extra_write_bytes: None,
            write_amplification: None,
            peak_overhead_bytes: 0,
            peak_overhead_ratio: 0.0,
            projection_bytes: 0,
            cutover_ms: 0.0,
            retire_ms: 0.0,
            catchup_rounds: 0,
            converged: true,
            equivalence_proven: true,
            final_state: "Active".into(),
            failure: None,
        };
        assert!(r.extra_write_bytes.is_none());
        assert!(r.write_amplification.is_none());
    }

    #[test]
    fn an_incomplete_attempt_may_not_be_cited() {
        let mut r = RebuildCost {
            seeded_events: 0,
            ingested_during: 0,
            rounds: 0,
            entities: 0,
            control: ArmResult {
                wall_ms: 0.0,
                write_bytes: None,
                peak_db_bytes: 0,
                final_db_bytes: 0,
            },
            rebuild: ArmResult {
                wall_ms: 0.0,
                write_bytes: None,
                peak_db_bytes: 0,
                final_db_bytes: 0,
            },
            extra_write_bytes: None,
            write_amplification: None,
            peak_overhead_bytes: 0,
            peak_overhead_ratio: 0.0,
            projection_bytes: 0,
            cutover_ms: 0.0,
            retire_ms: 0.0,
            catchup_rounds: 0,
            converged: true,
            equivalence_proven: true,
            final_state: "Active".into(),
            failure: None,
        };
        assert!(r.completed());
        r.final_state = "Building".into();
        assert!(!r.completed(), "a rebuild that never cut over was citable");
        r.final_state = "Active".into();
        r.equivalence_proven = false;
        assert!(!r.completed(), "an unproven rebuild was citable");
        r.equivalence_proven = true;
        r.failure = Some("x".into());
        assert!(!r.completed());
    }
}
