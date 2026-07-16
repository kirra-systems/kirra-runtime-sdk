#pragma once

// Minimal STM32F103 status-LED driver — the first concrete #967 HAL seam.
//
// Drives PC13 (the status LED per hal/board_manifest.hpp, active-low). It is a
// safe GPIO output — an LED, no actuation path — used only as the bring-up
// image's sign of life. Register-level and target-only (compiled into the
// firmware image targets, not the host build). If the board's LED turns out not
// to be on PC13, this is the one place to change.

namespace r2::application {

// Enable the GPIOC clock and configure PC13 as a push-pull output. Call once
// before the control loop.
void status_led_init() noexcept;

// Set the LED state (active-low is handled here: `on` drives PC13 low).
void status_led_write(bool on) noexcept;

}  // namespace r2::application
