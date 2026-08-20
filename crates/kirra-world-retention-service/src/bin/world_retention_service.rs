//! **`world_retention_service`** — the process that makes retention run.
//!
//! Config: `KIRRA_WORLD_DB` (required — the store to apply retention to);
//! `KIRRA_WORLD_RETENTION_SWEEP_MS` (optional, default one hour).
//!
//! ```text
//! schedule ──▶ retention_sweeper ──▶ retention_driver ──▶ kirra_world::retention
//!  (here)        every interval        survey → act            decide
//! ```
//!
//! The process holds the sweeper handle and parks. Dropping the handle stops
//! the thread, so the handle must outlive the process's useful life — which is
//! why `main` blocks rather than returning after `start`.

use std::time::Duration;

use kirra_world_retention_service::{
    resolve_interval, start_retention, StartupRefusal, SWEEP_INTERVAL_ENV, WORLD_DB_ENV,
};

fn fail(msg: &str) -> ! {
    eprintln!("world_retention_service: {msg}");
    std::process::exit(1)
}

fn main() {
    let db = match std::env::var(WORLD_DB_ENV) {
        Ok(p) if !p.trim().is_empty() => p,
        _ => fail(&StartupRefusal::NoDatabasePath.to_string()),
    };
    let interval = match resolve_interval(std::env::var(SWEEP_INTERVAL_ENV).ok().as_deref()) {
        Ok(d) => d,
        Err(e) => fail(&e.to_string()),
    };

    // Held for the life of the process: the sweeper's thread stops the moment
    // this is dropped, so binding it to `_` — or letting `main` return — would
    // start retention and immediately cancel it.
    let sweeper = match start_retention(std::path::Path::new(&db), interval) {
        Ok(s) => s,
        Err(e) => fail(&format!("open {db}: {e:?}")),
    };

    println!(
        "Kirra World retention service — store {db}, sweeping every {} s. \
         Horizons are OQ2's; every decision belongs to kirra_world::retention.",
        interval.as_secs()
    );

    // Report each time the picture changes. Not a log line per pass: the
    // ordinary state is "nothing old enough", hourly, forever, and a process
    // that says so every hour trains an operator to stop reading it.
    let mut last_seen = None;
    loop {
        std::thread::sleep(Duration::from_secs(60));
        let counters = sweeper.counters();
        let now = (
            counters.passes(),
            counters.compacted(),
            counters.pinned(),
            counters.failed(),
        );
        if Some(now) != last_seen {
            last_seen = Some(now);
            let (passes, compacted, pinned, failed) = now;
            // `pinned` climbing while `compacted` stays flat is the failure
            // worth alerting on — the store grows while retention truthfully
            // reports doing nothing — so the report carries WHY, not a count.
            let why = sweeper
                .last_report()
                .map(|r| format!("{:?}", r.decision))
                .unwrap_or_else(|| "no pass yet".to_string());
            println!(
                "retention: passes={passes} compacted={compacted} pinned={pinned} \
                 failed={failed} last={why}"
            );
        }
    }
}
