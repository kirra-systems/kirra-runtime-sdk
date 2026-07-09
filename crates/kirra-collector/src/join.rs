// crates/kirra-collector/src/join.rs
//
// The join (docs/COLLECTOR_DESIGN.md [D2]). For each kept capture record, find
// the bus message that recorded the same decision: within a `±window_ms`
// wall-clock window, cross-checked on `asset_id` / `trajectory_id` / `objects_ms`
// where the record carries them (trajectory records do; gateway records don't).
// A record with no bus match in the window is an ORPHAN — surfaced by the
// reconciliation report, never silently dropped.

use kirra_capture_schema::CaptureRecord;

use crate::bag::{BagReader, BusMatch, BusMessage};

/// Outcome of joining one record against the bag.
#[derive(Debug, Clone, PartialEq)]
pub enum JoinOutcome {
    Joined(BusMatch),
    Orphan,
}

#[inline]
fn time_dist(msg: &BusMessage, rec: &CaptureRecord) -> u64 {
    msg.t_wall_ms.abs_diff(rec.t_wall_ms)
}

/// Do a candidate bus message's cross-check keys agree with this record's
/// trajectory summary? A `None` on the bus side is treated as "not asserted"
/// (does not veto); only a present-and-DIFFERENT key disqualifies. This keeps
/// the join robust to partial bus stamping while still rejecting mismatches.
fn keys_agree(msg: &BusMessage, rec: &CaptureRecord) -> bool {
    let Some(t) = rec.traj.as_ref() else {
        return true; // gateway records have no cross-check keys
    };
    let asset_ok = msg.asset_id.as_deref().is_none_or(|a| a == t.asset_id);
    let traj_ok = msg.trajectory_id.is_none_or(|id| id == t.trajectory_id);
    let objs_ok = msg.objects_ms.is_none_or(|o| o == t.objects_ms);
    asset_ok && traj_ok && objs_ok
}

/// Join one record. Prefers candidates whose cross-check keys agree; among
/// those, the nearest in time. Falls back to nearest-in-time only if NO
/// key-agreeing candidate exists (so a stray bus message with a wrong id never
/// wins over a correct one, but the join still lands when keys are unstamped).
#[must_use]
pub fn join_record(rec: &CaptureRecord, bag: &dyn BagReader, window_ms: u64) -> JoinOutcome {
    let candidates = bag.messages_in_window(rec.t_wall_ms, window_ms);
    if candidates.is_empty() {
        return JoinOutcome::Orphan;
    }
    let best = candidates
        .iter()
        .filter(|m| keys_agree(m, rec))
        .min_by_key(|m| time_dist(m, rec))
        .or_else(|| candidates.iter().min_by_key(|m| time_dist(m, rec)));
    match best {
        Some(m) => JoinOutcome::Joined(BusMatch {
            doer_version: m.doer_version.clone(),
            bulk_ref: m.bulk_ref.clone(),
            matched_t_wall_ms: m.t_wall_ms,
        }),
        None => JoinOutcome::Orphan,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bag::InMemoryBag;
    use kirra_capture_schema::{
        CaptureOutcome, CaptureRecord, CaptureSource, TrajectoryCaptureExt,
    };

    fn gw(seq: u64, t_wall_ms: u64) -> CaptureRecord {
        CaptureRecord {
            decision_seq: seq,
            t_mono_ns: 0,
            t_wall_ms,
            source: CaptureSource::CommandGateway,
            proposed: None,
            traj: None,
            outcome: CaptureOutcome::Allow,
            deny_code: None,
            safe_value: None,
            mrc: false,
            posture: "NOMINAL".to_string(),
            derate_enabled: false,
        }
    }

    fn traj_rec(seq: u64, t_wall_ms: u64, traj_id: u64) -> CaptureRecord {
        CaptureRecord {
            decision_seq: seq,
            t_mono_ns: 0,
            t_wall_ms,
            source: CaptureSource::SlowLoopTrajectory,
            proposed: None,
            traj: Some(TrajectoryCaptureExt {
                asset_id: "ego".to_string(),
                trajectory_id: traj_id,
                objects_ms: 500,
                point_count: 3,
                object_count: 1,
                first_pose: None,
                last_pose: None,
                target_speed_mps: Some(8.0),
            }),
            outcome: CaptureOutcome::Allow,
            deny_code: None,
            safe_value: None,
            mrc: false,
            posture: "NOMINAL".to_string(),
            derate_enabled: false,
        }
    }

    fn msg(t: u64, ver: &str, traj_id: Option<u64>, reff: &str) -> BusMessage {
        BusMessage {
            t_wall_ms: t,
            doer_version: ver.to_string(),
            asset_id: traj_id.map(|_| "ego".to_string()),
            trajectory_id: traj_id,
            objects_ms: traj_id.map(|_| 500),
            bulk_ref: reff.to_string(),
        }
    }

    #[test]
    fn orphan_when_no_message_in_window() {
        let bag = InMemoryBag::new("test", vec![msg(10_000, "v1", None, "a")]);
        assert_eq!(join_record(&gw(0, 1000), &bag, 100), JoinOutcome::Orphan);
    }

    #[test]
    fn nearest_in_time_wins_for_gateway() {
        let bag = InMemoryBag::new(
            "test",
            vec![msg(1050, "v1", None, "far"), msg(1010, "v2", None, "near")],
        );
        let JoinOutcome::Joined(m) = join_record(&gw(0, 1000), &bag, 100) else {
            panic!("expected join");
        };
        assert_eq!(m.bulk_ref, "near");
        assert_eq!(m.doer_version, "v2");
    }

    #[test]
    fn key_agreeing_candidate_beats_a_closer_mismatch() {
        // A closer message with the WRONG trajectory id must lose to a slightly
        // farther one whose keys agree.
        let bag = InMemoryBag::new(
            "test",
            vec![
                msg(1005, "wrong", Some(999), "wrong_keys"),
                msg(1020, "right", Some(42), "right_keys"),
            ],
        );
        let JoinOutcome::Joined(m) = join_record(&traj_rec(0, 1000, 42), &bag, 100) else {
            panic!("expected join");
        };
        assert_eq!(
            m.bulk_ref, "right_keys",
            "key agreement must outrank time proximity"
        );
    }
}
