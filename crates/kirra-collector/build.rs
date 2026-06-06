// crates/kirra-collector/build.rs
//
// Records provenance into compile-time env vars for the dataset manifest [M3]:
//   - KIRRA_GIT_COMMIT    : `git rev-parse --short HEAD`, or "unknown"
//   - KIRRA_SCHEMA_VERSION: the kirra-capture-schema crate version, read from the
//     sibling Cargo.toml (the schema crate is off-limits to edit, so we parse its
//     manifest), or "unknown" if the sibling can't be found (standalone/packaged
//     build outside the workspace) — mirrors the git-hash fallback so the build
//     never fails just because a path moved.

use std::process::Command;

fn main() {
    let git = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KIRRA_GIT_COMMIT={git}");

    let schema_version = std::fs::read_to_string("../kirra-capture-schema/Cargo.toml")
        .ok()
        .and_then(|toml| parse_package_version(&toml))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KIRRA_SCHEMA_VERSION={schema_version}");

    // Re-run when provenance sources change.
    println!("cargo:rerun-if-changed=../kirra-capture-schema/Cargo.toml");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=build.rs");
}

/// Extract `version = "..."` from the `[package]` section of a Cargo.toml.
fn parse_package_version(toml: &str) -> Option<String> {
    let mut in_package = false;
    for line in toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = t.strip_prefix("version") {
                let val = rest.trim_start().strip_prefix('=')?.trim();
                return Some(val.trim_matches('"').to_string());
            }
        }
    }
    None
}
