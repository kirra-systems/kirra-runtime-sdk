// crates/kirra-ros2-adapter/build.rs
//
// Two-mode build script:
//   - `ros2` feature OFF (default): no-op. Default builds and the
//     safety-kernel CI don't need any of this.
//   - `ros2` feature ON: compile the C++ shim
//     (src/corridor/lanelet2_bridge.{cpp,h}) via cxx-build, with
//     lanelet2_core + boost-serialization headers discovered from the
//     integrator's sourced ROS environment.
//
// Discovery order (first hit wins):
//   1. $CMAKE_PREFIX_PATH — set by `source /opt/ros/<distro>/setup.bash`.
//      Each entry is an ament prefix; we look under <prefix>/include/.
//   2. $AMENT_PREFIX_PATH — fallback path used by ament alone.
//   3. /opt/ros/<distro>/include — last-ditch hard-coded fallback for
//      the most common Ubuntu install. The integrator should normally
//      have CMAKE_PREFIX_PATH set; this catches the case where they
//      didn't fully source.
//
// On failure to find lanelet2_core/LaneletMap.h the script `panic!`s
// with a precise message — much easier to diagnose than a downstream
// `cc` complaining about missing headers.

fn main() {
    // The `ros2` feature gates ALL of the C++ work. Without it we
    // produce nothing — the default cargo build stays pure-Rust.
    #[cfg(not(feature = "ros2"))]
    {
        // Tell Cargo we don't depend on anything we haven't yet seen.
        // No file changes will re-trigger this script on a default build.
        println!("cargo:rerun-if-changed=build.rs");
    }

    #[cfg(feature = "ros2")]
    ros2_build();
}

#[cfg(feature = "ros2")]
fn ros2_build() {
    use std::env;
    use std::path::{Path, PathBuf};

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/corridor/lanelet2_bridge.cpp");
    println!("cargo:rerun-if-changed=src/corridor/lanelet2_bridge.h");
    println!("cargo:rerun-if-changed=src/corridor/lanelet2_bridge.rs");
    println!("cargo:rerun-if-env-changed=CMAKE_PREFIX_PATH");
    println!("cargo:rerun-if-env-changed=AMENT_PREFIX_PATH");
    println!("cargo:rerun-if-env-changed=ROS_DISTRO");

    // ---- Locate lanelet2_core headers ---------------------------------

    let mut candidate_includes: Vec<PathBuf> = Vec::new();

    if let Ok(prefix_path) = env::var("CMAKE_PREFIX_PATH") {
        for entry in prefix_path.split(':') {
            candidate_includes.push(Path::new(entry).join("include"));
        }
    }
    if let Ok(prefix_path) = env::var("AMENT_PREFIX_PATH") {
        for entry in prefix_path.split(':') {
            candidate_includes.push(Path::new(entry).join("include"));
        }
    }
    if let Ok(distro) = env::var("ROS_DISTRO") {
        candidate_includes.push(PathBuf::from(format!("/opt/ros/{distro}/include")));
    }

    let lanelet_header_relative = Path::new("lanelet2_core/LaneletMap.h");
    let mut lanelet_include_dirs: Vec<PathBuf> = Vec::new();
    for candidate in &candidate_includes {
        // Ament installs nest headers one more level deep under the
        // package name on some distros: <prefix>/include/lanelet2_core/
        // lanelet2_core/LaneletMap.h. We try both layouts.
        let direct = candidate.join(lanelet_header_relative);
        if direct.exists() {
            lanelet_include_dirs.push(candidate.clone());
            continue;
        }
        let ament_nested = candidate.join("lanelet2_core").join(lanelet_header_relative);
        if ament_nested.exists() {
            lanelet_include_dirs.push(candidate.join("lanelet2_core"));
        }
    }

    if lanelet_include_dirs.is_empty() {
        panic!(
            "kirra-ros2-adapter (feature `ros2`): could not locate lanelet2_core \
             headers (lanelet2_core/LaneletMap.h) under any of these include roots:\n\
                {candidate_includes:#?}\n\
             \n\
             To fix:\n\
              - source your ROS environment (e.g. `source /opt/ros/jazzy/setup.bash`), AND\n\
              - install the lanelet2 package (e.g. `apt install ros-jazzy-lanelet2 libboost-serialization-dev`).\n\
             \n\
             The integrator's `package.xml` should list lanelet2_core as a build \
             dependency so colcon / ament sets CMAKE_PREFIX_PATH for us.",
        );
    }

    // ---- Build the C++ shim via cxx-build -----------------------------

    let mut build = cxx_build::bridge("src/corridor/lanelet2_bridge.rs");
    build.file("src/corridor/lanelet2_bridge.cpp");
    // Make the hand-written `lanelet2_bridge.h` resolvable via the bare
    // `#include "lanelet2_bridge.h"` form in both the .cpp shim and the
    // cxx-generated stub (which picks up the `include!()` from the
    // bridge .rs).
    build.include("src/corridor");
    for inc in &lanelet_include_dirs {
        build.include(inc);
    }
    build.flag_if_supported("-std=c++17");
    // boost::serialization headers live under the standard /usr/include
    // path on Ubuntu (`libboost-serialization-dev`) — no extra `.include()`
    // needed if the system include search path is sane. If the integrator
    // uses a custom boost install, they set CXXFLAGS / BOOST_ROOT before
    // invoking `cargo build`.

    build.compile("kirra_lanelet2_bridge");

    // Link the prebuilt lanelet2_core shared library. Same prefix
    // discovery as the include path; we tell the linker about <prefix>/lib
    // and ask it for `-llanelet2_core`.
    let mut link_search_added = false;
    if let Ok(prefix_path) = env::var("CMAKE_PREFIX_PATH") {
        for entry in prefix_path.split(':') {
            let lib_dir = Path::new(entry).join("lib");
            if lib_dir.exists() {
                println!("cargo:rustc-link-search=native={}", lib_dir.display());
                link_search_added = true;
            }
        }
    }
    if let Ok(distro) = env::var("ROS_DISTRO") {
        let lib_dir = format!("/opt/ros/{distro}/lib");
        if Path::new(&lib_dir).exists() {
            println!("cargo:rustc-link-search=native={lib_dir}");
            link_search_added = true;
        }
    }
    if !link_search_added {
        panic!(
            "kirra-ros2-adapter (feature `ros2`): no lib search path produced from \
             CMAKE_PREFIX_PATH / ROS_DISTRO — did `source /opt/ros/<distro>/setup.bash` succeed?"
        );
    }
    println!("cargo:rustc-link-lib=dylib=lanelet2_core");
    // boost-serialization links via the system libraries on Ubuntu; the
    // package is `libboost-serialization-dev`. Linker picks it up by
    // name from the system search path.
    println!("cargo:rustc-link-lib=dylib=boost_serialization");
}
