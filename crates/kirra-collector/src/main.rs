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

use kirra_collector::bag::{BagReader, Db3BagReader, InMemoryBag, McapBagReader};
use kirra_collector::{read_jsonl_with_digests, run, CollectorConfig, CollectorError, Lineage};

struct Args {
    captures: Vec<PathBuf>,
    bag_json: Option<PathBuf>,
    bag_real: Option<PathBuf>,
    bag_topic: Option<String>,
    doer_version: Option<String>,
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
            bag_topic: None,
            doer_version: None,
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
  --bag <path>              real rosbag2 .db3/.mcap backend (C2). Requires --bag-topic + --doer-version
  --bag-topic <topic>       doer trajectory/proposal topic to index in the .db3
  --doer-version <ver>      doer build stamped on joined rows (can't be read from CDR)
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
            "--bag-topic" => args.bag_topic = Some(next("--bag-topic")?),
            "--doer-version" => args.doer_version = Some(next("--doer-version")?),
            "--out" => args.out = Some(PathBuf::from(next("--out")?)),
            "--pass-rate" => {
                args.pass_rate = next("--pass-rate")?
                    .parse()
                    .map_err(|e| format!("--pass-rate: {e}"))?
            }
            "--window-ms" => {
                args.window_ms = next("--window-ms")?
                    .parse()
                    .map_err(|e| format!("--window-ms: {e}"))?
            }
            "--max-orphan-rate" => {
                args.max_orphan_rate = next("--max-orphan-rate")?
                    .parse()
                    .map_err(|e| format!("--max-orphan-rate: {e}"))?
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

    let (bag, bag_backend): (Box<dyn BagReader>, &str) = if let Some(p) = &args.bag_real {
        match p.extension().and_then(|e| e.to_str()) {
            Some("db3") => {
                let Some(topic) = args.bag_topic.as_deref() else {
                    eprintln!("error: --bag <db3> requires --bag-topic <doer trajectory topic>");
                    return ExitCode::FAILURE;
                };
                let Some(ver) = args.doer_version.as_deref() else {
                    eprintln!(
                        "error: --bag <db3> requires --doer-version <run's doer build> \
                         (it can't be read without decoding payloads)"
                    );
                    return ExitCode::FAILURE;
                };
                match Db3BagReader::open(p, topic, ver) {
                    Ok(r) => (Box::new(r), "db3"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            Some("mcap") => {
                let Some(topic) = args.bag_topic.as_deref() else {
                    eprintln!("error: --bag <mcap> requires --bag-topic <doer trajectory topic>");
                    return ExitCode::FAILURE;
                };
                let Some(ver) = args.doer_version.as_deref() else {
                    eprintln!(
                        "error: --bag <mcap> requires --doer-version <run's doer build> \
                         (it can't be read without decoding payloads)"
                    );
                    return ExitCode::FAILURE;
                };
                match McapBagReader::open(p, topic, ver) {
                    Ok(r) => (Box::new(r), "mcap"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            _ => {
                eprintln!(
                    "error: --bag {} — unrecognized bag extension (expected .db3 or .mcap).",
                    p.display()
                );
                return ExitCode::FAILURE;
            }
        }
    } else {
        match InMemoryBag::from_json_file(args.bag_json.as_ref().unwrap()) {
            Ok(b) => (Box::new(b), "bag-json"),
            Err(e) => {
                eprintln!("error: reading --bag-json: {e}");
                return ExitCode::FAILURE;
            }
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
        bag_backend: bag_backend.to_string(),
    };
    let cfg = CollectorConfig {
        pass_rate: args.pass_rate,
        window_ms: args.window_ms,
        max_orphan_rate: args.max_orphan_rate,
        out_dir: args.out.unwrap(),
    };

    match run(records, bag.as_ref(), &lineage, &cfg) {
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
