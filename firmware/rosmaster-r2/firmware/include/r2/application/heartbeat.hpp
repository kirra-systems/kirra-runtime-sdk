#pragma once

// Pure blink-phase for the bring-up "sign of life". A steady blink of the status
// LED proves the control loop is running (a solid or dark LED means it never
// reached the loop, or hardfaulted) — the only boot confirmation available when
// flashing over the serial bootloader with no debug probe attached.
//
// This is portable and host-tested; the register-level LED write is the target-
// only status_led_stm32 driver.

#include <cstdint>

namespace r2::application {

// Square wave: true for the first half_period counts, false for the next
// half_period, repeating. `counter` increments once per control cycle and
// `half_period` sets the on/off duration in cycles. A zero half_period yields a
// constant false (disabled). Because the loop is not yet paced by a real time
// base, the blink rate is approximate — its purpose is presence, not precision.
[[nodiscard]] constexpr bool heartbeat_on(std::uint32_t counter,
                                          std::uint32_t half_period) noexcept {
    if (half_period == 0U) {
        return false;
    }
    return ((counter / half_period) & 1U) == 0U;
}

}  // namespace r2::application
