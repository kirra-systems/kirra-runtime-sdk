// kirra-replay — EP-19 deterministic-replay CLI (incident reconstruction).
//
//   kirra-replay --class <courier|delivery-av|robotaxi> <session.jsonl>
//
// Feeds every captured record back through the REAL gateway checker and
// compares verdicts BIT-identically. Exit codes: 0 = deterministic (every
// replayable record identical, every line parsed); 1 = divergence or parse
// error (the alarm — same inputs, same checker, different verdict); 2 = usage.
//
// See docs/REPLAY_INCIDENT_RECONSTRUCTION.md for the operator workflow.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // #1215 joined-cycle mode: chain-consistency replay over the joined
    // artifact (optionally re-joined from its raw stage events). Exit 1 on any
    // consistency finding — same contract as the session mode's divergence.
    if let [_, flag, rest @ ..] = args.as_slice() {
        if flag == "--joined" {
            let (artifact_path, events_path) = match rest {
                [a] => (a.clone(), None),
                [a, e] => (a.clone(), Some(e.clone())),
                _ => {
                    eprintln!("usage: kirra-replay --joined <artifact.json> [events.jsonl]");
                    return ExitCode::from(2);
                }
            };
            let artifact = match std::fs::read_to_string(&artifact_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("kirra-replay: cannot read {artifact_path}: {e}");
                    return ExitCode::from(2);
                }
            };
            let events = match &events_path {
                None => None,
                Some(p) => match std::fs::read_to_string(p) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        eprintln!("kirra-replay: cannot read {p}: {e}");
                        return ExitCode::from(2);
                    }
                },
            };
            return match kirra_replay::replay_joined_artifact(&artifact, events.as_deref()) {
                Err(e) => {
                    eprintln!("kirra-replay: {e}");
                    ExitCode::from(2)
                }
                Ok(summary) => {
                    println!(
                        "joined replay: {} records ({} complete, {} partial), {} with findings",
                        summary.records,
                        summary.complete,
                        summary.partial,
                        summary.findings.len()
                    );
                    for (scan, findings) in &summary.findings {
                        for f in findings {
                            println!("  scan {scan}: {f:?}");
                        }
                    }
                    if summary.is_consistent() {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    }
                }
            };
        }
    }

    let (class_arg, path) = match args.as_slice() {
        [_, flag, class, path] if flag == "--class" => (class.clone(), path.clone()),
        _ => {
            eprintln!(
                "usage: kirra-replay --class <courier|delivery-av|robotaxi> <session.jsonl>\n       kirra-replay --joined <artifact.json> [events.jsonl]"
            );
            return ExitCode::from(2);
        }
    };
    let class = match kirra_replay::parse_class(&class_arg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kirra-replay: {e}");
            return ExitCode::from(2);
        }
    };
    let jsonl = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("kirra-replay: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    let summary = kirra_replay::replay_session_jsonl(&jsonl, class);
    println!(
        "replayed {} records under class {class_arg}: {} identical, {} not-replayable, {} DIVERGENT, {} parse errors",
        summary.total,
        summary.identical,
        summary.not_replayable,
        summary.divergences.len(),
        summary.parse_errors.len(),
    );
    for (seq, reason) in &summary.skipped {
        println!("  skipped decision_seq={seq}: {reason}");
    }
    for (lineno, err) in &summary.parse_errors {
        eprintln!("  PARSE ERROR line {lineno}: {err}");
    }
    for (seq, detail) in &summary.divergences {
        eprintln!("  DIVERGENT decision_seq={seq}: {detail}");
    }

    if summary.is_deterministic() {
        println!("session replays DETERMINISTICALLY (bit-identical verdicts)");
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "REPLAY DIVERGENCE — the recorded session does not reproduce under this build/class"
        );
        ExitCode::from(1)
    }
}
