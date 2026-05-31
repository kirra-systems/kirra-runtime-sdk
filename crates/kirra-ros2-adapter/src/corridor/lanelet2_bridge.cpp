// crates/kirra-ros2-adapter/src/corridor/lanelet2_bridge.cpp
//
// Implementation of the two cxx::bridge entry points declared in
// lanelet2_bridge.{rs,h}. Calls into:
//   - boost::serialization (binary_iarchive) to deserialize the
//     LaneletMapBin.data wire format — the same path Autoware's
//     `autoware_lanelet2_extension::utils::conversion::fromBinMsg`
//     takes, decoupled from the Autoware-specific message type.
//   - lanelet2_core's Lanelet::leftBound2d / rightBound2d to extract
//     the boundary polylines.
//
// Error handling: anything that throws on the C++ side is converted to
// a Rust `Err(cxx::Exception)` by cxx — no exceptions propagate up. We
// throw `std::runtime_error` with a human-readable message at every
// failure point (unknown lanelet id, corrupt stream, NaN/Inf geometry).

// Our hand-written header (see build.rs `.include("src/corridor")`).
#include "lanelet2_bridge.h"
// cxx-generated header for the bridge (path produced by
// `cxx_build::bridge("src/corridor/lanelet2_bridge.rs")`).
#include "kirra-ros2-adapter/src/corridor/lanelet2_bridge.rs.h"

#include <boost/archive/binary_iarchive.hpp>
#include <lanelet2_core/primitives/Lanelet.h>
#include <lanelet2_core/primitives/LineString.h>
#include <lanelet2_core/primitives/Point.h>

#include <cmath>
#include <cstring>
#include <memory>
#include <sstream>
#include <stdexcept>
#include <string>

namespace kirra::lanelet2_bridge {

std::unique_ptr<LaneletMap> load_lanelet_map(rust::Slice<const std::uint8_t> data) {
    if (data.empty()) {
        throw std::runtime_error(
            "kirra::lanelet2_bridge::load_lanelet_map: empty input buffer");
    }
    // Wrap the byte slice in a non-owning istringstream. We copy into a
    // std::string so the stream owns its storage (cxx Slices are not
    // guaranteed to outlive the function call after early returns).
    std::string buf(reinterpret_cast<const char*>(data.data()), data.size());
    std::istringstream iss(std::move(buf), std::ios::binary);
    try {
        boost::archive::binary_iarchive ia(iss);
        auto map = std::make_unique<lanelet::LaneletMap>();
        ia >> *map;
        return map;
    } catch (const std::exception& e) {
        throw std::runtime_error(
            std::string("kirra::lanelet2_bridge::load_lanelet_map: ") + e.what());
    }
}

namespace {

// Push a 2D point onto the CorridorPoints side, with NaN/Inf rejection.
// The kernel's containment check itself rejects non-finite vertices via
// `point_is_finite()`, but failing here gives a precise error location.
inline void push_point(rust::Vec<CorridorPoint>& side, double x, double y) {
    if (!std::isfinite(x) || !std::isfinite(y)) {
        throw std::runtime_error(
            "kirra::lanelet2_bridge::extract_corridor: non-finite vertex");
    }
    side.push_back(CorridorPoint{x, y});
}

// Append a ConstLineString2d's vertices to `out`. If the side already
// has a trailing vertex equal to the first vertex of `ls`, skip the
// duplicate so adjacent lanelets join cleanly.
template <typename LineString2d>
inline void append_linestring_dedup(rust::Vec<CorridorPoint>& out, const LineString2d& ls) {
    bool first = true;
    for (const auto& p : ls) {
        const double x = p.x();
        const double y = p.y();
        if (first && !out.empty()) {
            const auto& back = out.back();
            // Tolerance-free comparison: producers (Autoware) emit the
            // exact-same boost-serialized Point ID across adjacent
            // lanelets, so they're bit-equal in practice.
            if (back.x == x && back.y == y) {
                first = false;
                continue;
            }
        }
        first = false;
        push_point(out, x, y);
    }
}

}  // anonymous namespace

CorridorPoints extract_corridor(
        const LaneletMap& map,
        rust::Slice<const std::int64_t> lanelet_ids) {
    CorridorPoints out;
    if (lanelet_ids.empty()) {
        throw std::runtime_error(
            "kirra::lanelet2_bridge::extract_corridor: empty lanelet_ids");
    }
    for (std::size_t i = 0; i < lanelet_ids.size(); ++i) {
        const lanelet::Id id = static_cast<lanelet::Id>(lanelet_ids[i]);
        const auto& layer = map.laneletLayer;
        if (!layer.exists(id)) {
            std::ostringstream oss;
            oss << "kirra::lanelet2_bridge::extract_corridor: lanelet id "
                << id << " not found in map";
            throw std::runtime_error(oss.str());
        }
        const lanelet::ConstLanelet ll = layer.get(id);
        // `leftBound2d()` / `rightBound2d()` return `ConstLineString2d`,
        // iterable as `ConstPoint2d` with `.x()` / `.y()`.
        append_linestring_dedup(out.left,  ll.leftBound2d());
        append_linestring_dedup(out.right, ll.rightBound2d());
    }
    if (out.left.size() < 2 || out.right.size() < 2) {
        throw std::runtime_error(
            "kirra::lanelet2_bridge::extract_corridor: "
            "fewer than 2 vertices per side after dedup");
    }
    return out;
}

}  // namespace kirra::lanelet2_bridge
