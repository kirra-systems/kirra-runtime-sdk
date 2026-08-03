//! Platform classification — which runs may be cited, and which may not.
//!
//! This module exists because the most likely way WM-2 goes wrong is not a bad
//! benchmark. It is a *good* benchmark, run on a laptop, pasted into ADR-0041's
//! ratification checklist, and never questioned again. The ADR asks for the log
//! and projection path measured "on target hardware, **not a development
//! host**"; something has to be able to tell the difference and say so in the
//! artifact.
//!
//! The convention is lifted deliberately from `tools/qnx-rtm-harness`, which
//! stamps every timing row `TBD-QNX-TARGET` until a real target run replaces it.
//! Same discipline, same reason: a number without its provenance is not
//! evidence, and the moment it is separated from its provenance nobody can put
//! it back.
//!
//! # Both halves are required
//!
//! A run counts as target evidence only when the machine corroborates it *and*
//! the operator asserts it (`--assert-target`). Either alone is insufficient,
//! and the asymmetry is intentional:
//!
//! - Detection alone would let a Jetson dev-rig run under conditions nobody
//!   inspected (wrong storage, wrong power, thermally throttled) become
//!   evidence silently.
//! - Assertion alone would make the flag a rubber stamp — which is exactly what
//!   it becomes if it can override the facts.
//!
//! # Why the storage medium blocks target status
//!
//! SQLite's durability cost is dominated by `fsync`, and `fsync` on a Jetson
//! spans more than an order of magnitude between microSD, eMMC and NVMe. A run
//! against a `tmpfs` path does not fsync to anything at all — it would produce
//! the *best* numbers the harness can emit while measuring none of the property
//! ADR-0041 is trying to decide (`synchronous` policy per source class, open
//! question 1). That is the single most dangerous false positive available here,
//! so a volatile or stacked filesystem forfeits target status rather than
//! merely warning.

use std::fmt;

/// Filesystems that cannot carry a durability measurement.
///
/// `tmpfs`/`ramfs` have no backing store. `overlay`/`aufs` interpose on
/// `fsync` semantics and are not the deployed layout for
/// `/var/lib/kirra/world`. `squashfs` is read-only. In every case the honest
/// answer is "point `--db` at real storage", not "record it with a caveat".
const NON_DURABLE_FS: &[&str] = &["tmpfs", "ramfs", "overlay", "overlayfs", "aufs", "squashfs"];

/// Raw observations about the machine. Gathering is impure; judging is not.
#[derive(Debug, Clone, Default)]
pub struct PlatformFacts {
    pub arch: String,
    /// `/proc/device-tree/model` — on a Jetson this reads e.g.
    /// "NVIDIA Jetson Orin NX Engineering Reference Developer Kit".
    pub model: Option<String>,
    /// `/etc/nv_tegra_release` — the L4T / Jetson Linux release banner.
    pub tegra_release: Option<String>,
    /// Filesystem type backing the database directory.
    pub db_fs_type: Option<String>,
    /// Backing device for that filesystem (e.g. `/dev/mmcblk0p1`).
    pub db_fs_source: Option<String>,
    /// `"release"` or `"debug"` — a debug build measures rustc, not SQLite.
    pub build_profile: String,
    /// The operator's explicit `--assert-target`.
    pub operator_asserted_target: bool,
}

/// What a run is allowed to be cited as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceStatus {
    /// May be cited in ADR-0041's ratification checklist.
    JetsonTargetMeasured,
    /// Useful for development and regression. **Not** ratification evidence.
    HostIndicativeNotTarget,
}

impl EvidenceStatus {
    pub fn token(self) -> &'static str {
        match self {
            Self::JetsonTargetMeasured => "JETSON-TARGET-MEASURED",
            Self::HostIndicativeNotTarget => "HOST-INDICATIVE-NOT-TARGET",
        }
    }

    pub fn is_citable(self) -> bool {
        matches!(self, Self::JetsonTargetMeasured)
    }
}

impl fmt::Display for EvidenceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// The verdict, plus every reason it is not target evidence.
///
/// Reasons are accumulated rather than short-circuited: an operator who fixes
/// one blocker and re-runs should not discover a second one on the next attempt
/// and a third after that.
#[derive(Debug, Clone)]
pub struct Classification {
    pub status: EvidenceStatus,
    pub blockers: Vec<String>,
}

fn looks_like_tegra(facts: &PlatformFacts) -> bool {
    if facts.tegra_release.is_some() {
        return true;
    }
    match &facts.model {
        Some(m) => {
            let lower = m.to_ascii_lowercase();
            lower.contains("jetson") || lower.contains("tegra")
        }
        None => false,
    }
}

/// Judge a set of facts. Pure — this is the part that is unit-tested, because
/// it is the part whose failure mode is a laptop number in a safety-adjacent
/// document.
pub fn classify(facts: &PlatformFacts) -> Classification {
    let mut blockers = Vec::new();

    if !facts.operator_asserted_target {
        blockers.push(
            "operator did not pass --assert-target (the flag is an assertion about \
             the physical setup: real device, real storage, representative thermal \
             and power conditions)"
                .to_string(),
        );
    }
    if facts.arch != "aarch64" {
        blockers.push(format!(
            "architecture is `{}`, not aarch64 — a Jetson Orin is aarch64",
            facts.arch
        ));
    }
    if !looks_like_tegra(facts) {
        blockers.push(
            "no Tegra evidence: neither /etc/nv_tegra_release nor a Jetson/Tegra \
             device-tree model was found"
                .to_string(),
        );
    }
    match facts.db_fs_type.as_deref() {
        Some(fs) if NON_DURABLE_FS.contains(&fs) => blockers.push(format!(
            "database is on `{fs}`, which cannot carry a durability measurement — \
             point --db at real storage (e.g. under /var/lib)"
        )),
        Some(_) => {}
        None => blockers.push(
            "could not determine the filesystem backing the database path; \
             fsync cost is the dominant term and an unidentified medium is not a \
             measurement"
                .to_string(),
        ),
    }
    if facts.build_profile != "release" {
        blockers.push(format!(
            "built in `{}` profile — a debug build measures rustc overhead, not the \
             substrate; rebuild with --release",
            facts.build_profile
        ));
    }

    let status = if blockers.is_empty() {
        EvidenceStatus::JetsonTargetMeasured
    } else {
        EvidenceStatus::HostIndicativeNotTarget
    };
    Classification { status, blockers }
}

// ---------------------------------------------------------------------------
// Fact gathering (impure)
// ---------------------------------------------------------------------------

/// Parse `/proc/self/mountinfo` and return `(fstype, source)` for the mount
/// carrying `path`.
///
/// Longest-prefix wins, which is what makes a nested mount resolve correctly —
/// `/var/lib` mounted separately under `/` must not be reported as `/`'s
/// filesystem, or the harness would cheerfully classify an NVMe-backed run by
/// the properties of the root device.
///
/// Split on the `" - "` separator rather than by field index: the optional
/// fields between the mount point and the separator are variable in number, so
/// a positional read of the right-hand side is wrong on any machine that has
/// shared subtrees (which is every systemd machine).
pub fn resolve_mount(mountinfo: &str, path: &str) -> Option<(String, String)> {
    let mut best: Option<(usize, String, String)> = None;
    for line in mountinfo.lines() {
        let (left, right) = match line.split_once(" - ") {
            Some(parts) => parts,
            None => continue,
        };
        let left_fields: Vec<&str> = left.split_whitespace().collect();
        let right_fields: Vec<&str> = right.split_whitespace().collect();
        if left_fields.len() < 5 || right_fields.len() < 2 {
            continue;
        }
        let mount_point = left_fields[4];
        if !path_has_prefix(path, mount_point) {
            continue;
        }
        let len = mount_point.len();
        if best.as_ref().is_none_or(|(b, _, _)| len > *b) {
            best = Some((
                len,
                right_fields[0].to_string(),
                right_fields[1].to_string(),
            ));
        }
    }
    best.map(|(_, fs, src)| (fs, src))
}

/// Component-wise prefix test. `"/varlib"` is not under `"/var"`, and a plain
/// `str::starts_with` would say it is.
fn path_has_prefix(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return path.starts_with('/');
    }
    if !path.starts_with(prefix) {
        return false;
    }
    matches!(path.as_bytes().get(prefix.len()), None | Some(b'/'))
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| {
        // Device-tree strings are NUL-terminated; the trailing NUL survives a
        // plain trim() and turns an equality check into a mystery.
        s.trim_matches(|c: char| c.is_whitespace() || c == '\0')
            .to_string()
    })
}

/// Collect facts about this machine and the directory `db_path` lives in.
pub fn gather(db_path: &std::path::Path, operator_asserted_target: bool) -> PlatformFacts {
    let dir = db_path.parent().unwrap_or(std::path::Path::new("."));
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let dir_str = canonical.to_string_lossy().to_string();

    let mount = std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .and_then(|info| resolve_mount(&info, &dir_str));

    PlatformFacts {
        arch: std::env::consts::ARCH.to_string(),
        model: read_trimmed("/proc/device-tree/model")
            .or_else(|| read_trimmed("/sys/firmware/devicetree/base/model")),
        tegra_release: read_trimmed("/etc/nv_tegra_release"),
        db_fs_type: mount.as_ref().map(|(fs, _)| fs.clone()),
        db_fs_source: mount.as_ref().map(|(_, src)| src.clone()),
        build_profile: build_profile().to_string(),
        operator_asserted_target,
    }
}

/// `debug_assertions` is the only profile signal available without a build
/// script, and it is the one that matters: it is on exactly when the
/// optimiser is not doing the work the benchmark is meant to measure.
pub const fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jetson() -> PlatformFacts {
        PlatformFacts {
            arch: "aarch64".into(),
            model: Some("NVIDIA Jetson Orin NX Developer Kit".into()),
            tegra_release: Some("# R36 (release), REVISION: 3.0".into()),
            db_fs_type: Some("ext4".into()),
            db_fs_source: Some("/dev/mmcblk0p1".into()),
            build_profile: "release".into(),
            operator_asserted_target: true,
        }
    }

    #[test]
    fn a_real_asserted_jetson_run_is_citable() {
        let c = classify(&jetson());
        assert_eq!(c.status, EvidenceStatus::JetsonTargetMeasured);
        assert!(c.blockers.is_empty(), "{:?}", c.blockers);
        assert!(c.status.is_citable());
    }

    #[test]
    fn detection_without_assertion_is_not_citable() {
        let mut f = jetson();
        f.operator_asserted_target = false;
        let c = classify(&f);
        assert_eq!(c.status, EvidenceStatus::HostIndicativeNotTarget);
        assert!(c.blockers.iter().any(|b| b.contains("--assert-target")));
    }

    #[test]
    fn assertion_cannot_override_the_facts() {
        // The whole point of requiring both halves: an operator who passes the
        // flag on a laptop still gets an uncitable run.
        let f = PlatformFacts {
            arch: "x86_64".into(),
            build_profile: "release".into(),
            db_fs_type: Some("ext4".into()),
            operator_asserted_target: true,
            ..Default::default()
        };
        let c = classify(&f);
        assert_eq!(c.status, EvidenceStatus::HostIndicativeNotTarget);
        assert!(c.blockers.iter().any(|b| b.contains("aarch64")));
        assert!(c.blockers.iter().any(|b| b.contains("Tegra")));
    }

    #[test]
    fn tmpfs_forfeits_target_status_on_a_real_jetson() {
        // The dangerous case: everything else is genuinely correct, so nothing
        // else would flag it, and the numbers come out BETTER than the truth.
        let mut f = jetson();
        f.db_fs_type = Some("tmpfs".into());
        let c = classify(&f);
        assert_eq!(c.status, EvidenceStatus::HostIndicativeNotTarget);
        assert_eq!(
            c.blockers.len(),
            1,
            "only the filesystem should block: {:?}",
            c.blockers
        );
        assert!(c.blockers[0].contains("tmpfs"));
    }

    #[test]
    fn overlayfs_also_forfeits() {
        let mut f = jetson();
        f.db_fs_type = Some("overlay".into());
        assert!(!classify(&f).status.is_citable());
    }

    #[test]
    fn a_debug_build_is_never_citable() {
        let mut f = jetson();
        f.build_profile = "debug".into();
        let c = classify(&f);
        assert!(!c.status.is_citable());
        assert!(c.blockers.iter().any(|b| b.contains("--release")));
    }

    #[test]
    fn an_unknown_filesystem_blocks_rather_than_passes() {
        let mut f = jetson();
        f.db_fs_type = None;
        assert!(!classify(&f).status.is_citable());
    }

    #[test]
    fn tegra_model_alone_is_enough_evidence() {
        // A Jetson image without /etc/nv_tegra_release still identifies itself
        // in the device tree.
        let mut f = jetson();
        f.tegra_release = None;
        assert!(classify(&f).status.is_citable());
    }

    #[test]
    fn every_blocker_is_reported_at_once() {
        let f = PlatformFacts {
            arch: "x86_64".into(),
            build_profile: "debug".into(),
            ..Default::default()
        };
        let c = classify(&f);
        // assert-target, arch, tegra, filesystem, profile.
        assert_eq!(c.blockers.len(), 5, "{:?}", c.blockers);
    }

    #[test]
    fn mountinfo_longest_prefix_wins() {
        let info = "\
25 0 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
31 25 259:1 / /var/lib rw,relatime shared:2 - ext4 /dev/mmcblk0p1 rw
";
        assert_eq!(
            resolve_mount(info, "/var/lib/kirra/world"),
            Some(("ext4".into(), "/dev/mmcblk0p1".into())),
            "a nested mount must not be reported as the root filesystem"
        );
        assert_eq!(
            resolve_mount(info, "/home/x"),
            Some(("ext4".into(), "/dev/nvme0n1p2".into()))
        );
    }

    #[test]
    fn mountinfo_handles_variable_optional_fields() {
        // `shared:` / `master:` tags vary in number, which is why the parser
        // splits on " - " instead of indexing from the left.
        let info = "\
25 0 259:2 / / rw shared:1 master:2 propagate_from:3 - xfs /dev/sda1 rw
";
        assert_eq!(
            resolve_mount(info, "/tmp"),
            Some(("xfs".into(), "/dev/sda1".into()))
        );
    }

    #[test]
    fn mountinfo_prefix_is_component_wise() {
        let info = "\
25 0 259:2 / / rw - ext4 /dev/root rw
31 25 259:1 / /var rw - tmpfs tmpfs rw
";
        // "/varlib" is NOT under "/var"; a naive starts_with would misclassify
        // it as tmpfs and forfeit target status on a perfectly good run.
        assert_eq!(
            resolve_mount(info, "/varlib/x"),
            Some(("ext4".into(), "/dev/root".into()))
        );
        assert_eq!(
            resolve_mount(info, "/var/x"),
            Some(("tmpfs".into(), "tmpfs".into()))
        );
    }

    #[test]
    fn mountinfo_without_a_separator_is_skipped_not_panicked() {
        assert_eq!(resolve_mount("garbage line\n", "/x"), None);
    }
}
