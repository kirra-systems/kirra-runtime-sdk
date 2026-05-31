// crates/kirra-ros2-adapter/src/corridor/lanelet2_bridge.h
//
// Declarations for the cxx::bridge in lanelet2_bridge.rs. Only the
// `LaneletMap` opaque type alias + the two free functions cross the
// boundary; everything else stays in `.cpp`.
//
// Generated header from cxx (`lanelet2_bridge.rs.h`) is included from
// the .cpp file alone — Rust does not see it.

#pragma once

#include <cstdint>
#include <memory>

// Lanelet2 headers — supplied by the integrator's ROS env. Discovered
// at build time by `build.rs` via `ament` / `pkg-config` / explicit
// CMAKE_PREFIX_PATH.
#include <lanelet2_core/LaneletMap.h>

// cxx-generated Rust types. The path is what cxx-build produces from
// the `#[cxx::bridge]` module path `kirra_ros2_adapter::corridor::lanelet2_bridge`.
// The build.rs `bridge("src/corridor/lanelet2_bridge.rs")` call produces
// this header into the CXX_BRIDGE include path.
#include "rust/cxx.h"

namespace kirra::lanelet2_bridge {

// Aliases so the cxx::bridge opaque `LaneletMap` resolves to
// `lanelet::LaneletMap` on the C++ side. cxx-rs uses C++-side type
// names verbatim once aliased.
using LaneletMap = lanelet::LaneletMap;

// Forward declarations of the two FFI entry points. The full
// definitions live in lanelet2_bridge.cpp.
struct CorridorPoint;
struct CorridorPoints;

std::unique_ptr<LaneletMap> load_lanelet_map(rust::Slice<const std::uint8_t> data);
CorridorPoints extract_corridor(const LaneletMap& map,
                                rust::Slice<const std::int64_t> lanelet_ids);

}  // namespace kirra::lanelet2_bridge
