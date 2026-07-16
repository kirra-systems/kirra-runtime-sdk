#include "r2/application/runner.hpp"

#include "r2/application/control_loop.hpp"
#include "r2/hal/interfaces.hpp"

#include <cstdint>

namespace r2::application {

void run_cycle(Application& app,
               const hal::MonotonicClock& clock,
               RunnerState& state,
               const double nominal_dt_s) noexcept {
    const std::uint64_t now_us = clock.now_us();
    double dt_s = nominal_dt_s;
    if (state.initialized && now_us > state.last_us) {
        dt_s = static_cast<double>(now_us - state.last_us) / 1'000'000.0;
    }
    state.last_us = now_us;
    state.initialized = true;
    app.tick(dt_s);
}

}  // namespace r2::application
