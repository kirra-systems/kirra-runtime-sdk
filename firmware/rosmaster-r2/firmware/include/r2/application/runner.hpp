#pragma once

// Portable control-loop runner. run_cycle() drives exactly one Application tick,
// deriving dt from the monotonic clock, so the same cadence logic serves both
// the host superloop and the target timer ISR / superloop. The PLATFORM owns
// the loop and the idle between cycles (e.g. WFI on the MCU); this function is
// the per-cycle body only, allocation-free and noexcept.

#include "r2/application/control_loop.hpp"
#include "r2/hal/interfaces.hpp"

#include <cstdint>

namespace r2::application {

// Per-cycle timing state owned by the caller (statically allocated on target).
struct RunnerState {
    std::uint64_t last_us{0U};
    bool initialized{false};
};

// Drive one control cycle:
//   * read the monotonic clock;
//   * the first cycle (and any non-advancing or backwards clock reading — a
//     glitch or same-microsecond re-entry) uses nominal_dt_s and reseeds;
//   * otherwise dt is the real elapsed time since the previous cycle. A genuine
//     overrun is passed through as a large dt (NOT clamped) so the Application's
//     control-deadline check catches the stall and fails closed, rather than
//     the runner hiding it.
// nominal_dt_s should be the loop's design period; a non-positive value is
// itself guarded downstream by the Application/MotionController.
void run_cycle(Application& app,
               const hal::MonotonicClock& clock,
               RunnerState& state,
               double nominal_dt_s) noexcept;

}  // namespace r2::application
