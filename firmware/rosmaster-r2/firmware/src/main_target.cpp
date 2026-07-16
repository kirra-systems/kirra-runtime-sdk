// Target application entry — ROSMASTER R2 safe-boot image (STM32F103RCT6).
//
// This is the first flashable image: it wires the Application against SafeHal
// (the fail-closed seams) and runs the control loop, so a freshly-flashed board
// powers on into a latched-safe, bridge-disabled state. As the concrete STM32
// drivers land (#967), each Safe* seam is replaced by its real driver one at a
// time — the loop body here does not change.
//
// Build/link + size verification only. What remains before this drives motors:
// the concrete HAL drivers (#967), a real time base (SysTick/timer feeding the
// clock and the loop cadence — SafeClock is frozen at 0 here), the 72 MHz PLL
// clock tree, and the per-link HMAC key provisioned in place of the zero
// placeholder below. None of these are exercised in the safe boot.

#include "r2/application/configuration.hpp"
#include "r2/application/control_loop.hpp"
#include "r2/application/heartbeat.hpp"
#include "r2/application/runner.hpp"
#include "r2/application/safe_hal.hpp"
#include "r2/application/status_led_stm32.hpp"

#include <cstdint>

namespace {

// Design control-loop period. The safe boot spins the superloop (SafeClock is
// frozen, so run_cycle uses this nominal dt); a real image paces the loop from
// a hardware timer and derives dt from a live monotonic clock.
constexpr double kNominalDtSeconds = 0.001;  // 1 kHz

// Status-LED blink half-period, in control cycles. Without a real time base the
// loop free-runs, so this is an approximate rate — the point is a visible blink
// (loop alive) versus a solid/dark LED (never reached the loop, or hardfaulted).
constexpr std::uint32_t kHeartbeatHalfPeriodCycles = 250U;

}  // namespace

// Called from Reset_Handler (startup_stm32f103.cpp). Named r2_app_main rather
// than main so the freestanding startup can call it without tripping the ISO
// C++ rule against a program calling ::main. Never returns.
extern "C" int r2_app_main() {
    // Statically allocated: no heap on the target.
    static r2::application::SafeHal safe_hal;

    static r2::application::ApplicationConfig config;
    config.platform = r2::application::factory_defaults();
    config.thresholds = r2::application::default_thresholds();
    // config.link_key stays zero here: a production image provisions the per-link
    // HMAC key. With SafeHal the platform never actuates regardless, so the
    // placeholder key cannot authorise motion.

    static r2::application::Application app{safe_hal.bundle(), config};
    app.initialize();

    // Sign of life: a blinking status LED proves the control loop is running.
    // This is the only boot confirmation available when flashing over the serial
    // bootloader with no debug probe attached. The LED is an observability output
    // only — it does not gate or influence the safety loop.
    r2::application::status_led_init();

    r2::application::RunnerState runner;
    std::uint32_t heartbeat_counter = 0U;
    for (;;) {
        r2::application::run_cycle(app, safe_hal.clock, runner, kNominalDtSeconds);
        r2::application::status_led_write(
            r2::application::heartbeat_on(heartbeat_counter, kHeartbeatHalfPeriodCycles));
        ++heartbeat_counter;
    }
}
