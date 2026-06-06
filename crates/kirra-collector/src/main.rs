// crates/kirra-collector/src/main.rs
//
// CLI entry for the offline learning-loop collector. Thin wrapper over the
// library pipeline (`kirra_collector::run`).
//
// Usage:
//   kirra-collector --capture <a.jsonl> [--capture <b.jsonl> ...] \
//                   (--bag-json <bus.json> | --bag <bag.db3|.mcap>) \
//                   --out <dataset_dir> \
//                   [--pass-rate 1.0] [--window-ms 100] [--max-orphan-rate 0.05]
//
// [C2] The real `--bag` backend (rosbag2 db3 / MCAP) is not wired yet (the bench
// isn't up); `--bag-json` reads a JSON array of bus messages so the tool runs
// end-to-end offline today.

use std::path::PathBuf;
use std::process::ExitCode;

use kirra_collector::bag::InMemoryBag;
use kirra_collector::{read_jsonl_with_digests, run, CollectorConfig, CollectorError, Lineage};

struct Args {
    captures: Vec<PathBuf>,
    bag_json: Option<PathBuf>,
    bag_real: Option<PathBuf>,
    out: Option<PathBuf>,
    pass_rate: f64,
    window_ms: u64,
    max_orphan_rate: f64,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            captures: Vec::new(),
            bag_json: None,
            bag_real: None,
            out: None,
            pass_rate: 1.0,
            window_ms: 100,
            max_orphan_rate: 0.05,
        }
    }
}

const USAGE: &str = "\
kirra-collector — offline learning-loop collector

  --capture <path>          capture JSONL (repeatable; at least one required)
  --bag-json <path>         synthetic bus recording (JSON array of bus messages)
  --bag <path>              real rosbag2 db3/MCAP backend (NOT yet wired — C2)
  --out <dir>               output dataset root (required)
  --pass-rate <f64>         ALLOW sampling rate, default 1.0 (keep all)
  --window-ms <u64>         join window ± ms, default 100
  --max-orphan-rate <f64>   fail if exceeded, default 0.05
  -h, --help                this help";

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut next = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--capture" => args.captures.push(PathBuf::from(next("--capture")?)),
            "--bag-json" => args.bag_json = Some(PathBuf::from(next("--bag-json")?)),
            "--bag" => args.bag_real = Some(PathBuf::from(next("--bag")?)),
            "--out" => args.out = Some(PathBuf::from(next("--out")?)),
            "--pass-rate" => {
                args.pass_rate = next("--pass-rate")?.parse().map_err(|e| format!("--pass-rate: {e}"))?
            }
            "--window-ms" => {
                args.window_ms = next("--window-ms")?.parse().map_err(|e| format!("--window-ms: {e}"))?
            }
            "--max-orphan-rate" => {
                args.max_orphan_rate =
                    next("--max-orphan-rate")?.parse().map_err(|e| format!("--max-orphan-rate: {e}"))?
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if args.captures.is_empty() {
        return Err("at least one --capture is required".to_string());
    }
    if args.out.is_none() {
        return Err("--out is required".to_string());
    }
    if args.bag_json.is_none() && args.bag_real.is_none() {
        return Err("one of --bag-json or --bag is required".to_string());
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    // [C2] real bag backend deferred until the bench records its first session.
    if let Some(p) = &args.bag_real {
        eprintln!(
            "error: --bag {} — the rosbag2 db3/MCAP backend is not wired yet (C2, \
             bench not up). Use --bag-json for offline runs.",
            p.display()
        );
        return ExitCode::FAILURE;
    }

    let bag = match InMemoryBag::from_json_file(args.bag_json.as_ref().unwrap()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: reading --bag-json: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (records, input_digests) = match read_jsonl_with_digests(&args.captures) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: reading capture JSONL: {e}");
            return ExitCode::FAILURE;
        }
    };

    let lineage = Lineage {
        inputs: input_digests,
        bag_backend: "bag-json".to_string(),
    };
    let cfg = CollectorConfig {
        pass_rate: args.pass_rate,
        window_ms: args.window_ms,
        max_orphan_rate: args.max_orphan_rate,
        out_dir: args.out.unwrap(),
    };

    match run(records, &bag, &lineage, &cfg) {
        Ok(manifest) => {
            println!("{}", manifest.quality.summary());
            println!("dataset_id = {}", manifest.dataset_id);
            ExitCode::SUCCESS
        }
        Err(CollectorError::OrphanRateExceeded { manifest, max }) => {
            println!("{}", manifest.quality.summary());
            println!("dataset_id = {}", manifest.dataset_id);
            eprintln!(
                "error: orphan rate {:.3} exceeds --max-orphan-rate {:.3} — dataset + manifest \
                 written but flagged for review",
                manifest.quality.orphan_rate, max
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
