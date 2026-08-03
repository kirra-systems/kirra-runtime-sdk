//! Build-time capture of **which binary produced a number**.
//!
//! `build_profile` already answers "release or debug", which is the difference
//! between measuring SQLite and measuring rustc. It does not answer *which*
//! release build — and a figure that is going to sit in ADR-0041 for years
//! needs that, because "459 B/event" is only reproducible if a later reader can
//! rebuild the thing that said it.
//!
//! Three facts are captured, all from the environment, adding **no dependency**
//! (the crate's one-dependency rule is load-bearing — see the manifest header):
//!
//! - `rustc -V` — the compiler, since optimisation output moves between
//!   versions and this harness reports microsecond-scale query latencies.
//! - the git commit, with an explicit `-dirty` suffix when the tree has
//!   uncommitted changes. A dirty build is not disqualifying, but it must be
//!   visible: it is the difference between a citable commit and a working copy
//!   nobody else has.
//! - a SHA-256 over the harness sources. The commit identifies what *should*
//!   have been built; the digest identifies what *was*. They differ exactly
//!   when someone edited a file to make a number come out better, which is the
//!   failure mode worth being able to detect after the fact.
//!
//! None of this can prevent a fabricated result. It makes an honest result
//! reconstructible, and a dishonest one leave a trace.

use std::path::Path;
use std::process::Command;

// The harness's own SHA-256, included rather than imported: a build script is
// a separate crate and cannot `use` the crate it builds. Keeping one
// implementation means the source digest and the chain links are the same
// function, so a change to one cannot silently diverge from the other.
// Declared by path rather than `include!`d inline, so the file's own `//!`
// header stays a module doc comment instead of trying to annotate the first
// constant. Only `hex` is used here; the rest is unused by design.
#[path = "src/sha256.rs"]
#[allow(dead_code)]
mod sha256;
use sha256::hex;

fn main() {
    // Re-run when any measured source changes, so the digest can never be
    // stale relative to the binary it describes.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    println!("cargo:rustc-env=WM2_RUSTC_VERSION={}", rustc_version());
    println!("cargo:rustc-env=WM2_GIT_COMMIT={}", git_commit());
    println!("cargo:rustc-env=WM2_SOURCE_DIGEST={}", source_digest());
}

/// `rustc -V`, or `unknown` if it cannot be run. Never fails the build: a
/// harness that will not compile because it could not describe itself is worse
/// than one that reports `unknown` and says so in the artifact.
fn rustc_version() -> String {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    Command::new(rustc)
        .arg("-V")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Short commit hash, suffixed `-dirty` when tracked files differ from HEAD.
///
/// `unknown` when git is absent or this is not a repository — a tarball build
/// is legitimate and should not fail, it should be *labelled*.
fn git_commit() -> String {
    let head = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let Some(head) = head else {
        return "unknown".into();
    };

    // `--quiet` exits non-zero when there are differences. An error running it
    // at all is reported as unknown-dirtiness rather than assumed clean.
    let clean = Command::new("git")
        .args(["diff", "--quiet", "HEAD"])
        .status()
        .ok()
        .map(|s| s.success());

    match clean {
        Some(true) => head,
        Some(false) => format!("{head}-dirty"),
        None => format!("{head}-dirtiness-unknown"),
    }
}

/// SHA-256 over `src/*.rs`, `tests/*.rs`, `build.rs` and `Cargo.toml`, in
/// sorted order, each contribution length-prefixed by its path.
///
/// Sorted so the digest is stable across filesystems that enumerate
/// differently; path-tagged so moving a file's contents to another name
/// changes the digest rather than preserving it.
fn source_digest() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<std::path::PathBuf> = Vec::new();

    for dir in ["src", "tests"] {
        if let Ok(entries) = std::fs::read_dir(root.join(dir)) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "rs") {
                    files.push(p);
                }
            }
        }
    }
    files.push(root.join("build.rs"));
    files.push(root.join("Cargo.toml"));
    files.sort();

    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"wm2-harness-source-v1\x00");
    for f in &files {
        let rel = f.strip_prefix(root).unwrap_or(f);
        // Normalised to `/` so a digest taken on Windows matches one taken on
        // the Jetson; the point is comparing builds, not platforms.
        let name = rel.to_string_lossy().replace('\\', "/");
        let body = std::fs::read(f).unwrap_or_default();
        buf.extend_from_slice(name.as_bytes());
        buf.push(0x00);
        buf.extend_from_slice(&(body.len() as u64).to_le_bytes());
        buf.extend_from_slice(&body);
    }
    hex(&buf)
}
