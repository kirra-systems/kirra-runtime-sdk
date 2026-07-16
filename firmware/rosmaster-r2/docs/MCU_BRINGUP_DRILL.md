# MCU first-boot bring-up drill (STM32F103RCT6)

The **standalone bring-up image** (`r2_firmware_bringup.bin`) boots on bare
silicon with no bootloader and no peripheral I/O — it wires the `Application`
against `SafeHal` and holds the platform in the latched-safe, bridge-disabled
state. This drill flashes it and confirms, with a debug probe only, that the
image boots and reaches the safe state. **Nothing actuates.**

## Safety preconditions

- **Wheels off the ground and the motor power bus disconnected.** Even though
  this image commands no motion, treat every on-target session as if it could.
- Power the board from a current-limited bench supply for the first boots.

## What you need

- ST-Link V2 (or clone), or a J-Link.
- OpenOCD ≥ 0.11 and `arm-none-eabi-gdb`.
- This repo, with `gcc-arm-none-eabi` installed.

## 1. Build the image

```bash
cmake -S firmware/rosmaster-r2 -B build-target \
  -DCMAKE_TOOLCHAIN_FILE="$PWD/firmware/rosmaster-r2/cmake/arm-none-eabi-cortex-m3.cmake"
cmake --build build-target --parallel
# Produces build-target/r2_firmware_bringup.elf + .bin (and the slot-A image).
arm-none-eabi-size build-target/r2_firmware_bringup.elf   # ~31 KiB flash, well under 256 KiB
```

## 2. Confirm the part before writing flash

```bash
openocd -f interface/stlink.cfg -f target/stm32f1x.cfg \
  -c "init; mdw 0xE0042000 1; exit"
```

`0xE0042000` is `DBGMCU_IDCODE`; the low 12 bits are the device ID. **0x414 =
high-density F103 (STM32F103xC/D/E)**, which the RCT6 is. If you see a different
ID, stop and reconcile the part against `hal/board_manifest.hpp`
(`kExpectedSharedBoardMcu = "STM32F103RCT6"`) before flashing.

## 3. Flash

```bash
openocd -f interface/stlink.cfg -f target/stm32f1x.cfg \
  -c "program build-target/r2_firmware_bringup.elf verify reset exit"
```

`verify` reads flash back and compares; a mismatch fails the command.

## 4. Confirm boot + safe state with GDB (no I/O)

Terminal A:

```bash
openocd -f interface/stlink.cfg -f target/stm32f1x.cfg
```

Terminal B:

```bash
arm-none-eabi-gdb build-target/r2_firmware_bringup.elf
(gdb) target extended-remote :3333
(gdb) monitor reset halt
# 4a. The reset vector must be reached (not stuck in a fault handler):
(gdb) break Reset_Handler
(gdb) continue          # should hit Reset_Handler
# 4b. The application entry must be reached:
(gdb) break r2_app_main
(gdb) continue          # should hit r2_app_main
# 4c. Let it run a moment, then halt inside the loop and confirm it is NOT in a
#     fault handler and IS ticking the control loop:
(gdb) continue
# ...wait ~1 s, Ctrl-C...
(gdb) backtrace         # expect run_cycle / Application::tick, NOT *_Handler
```

**Expected result:** the image reaches `r2_app_main`, enters the superloop, and
stays there (no `HardFault_Handler`). Because `SafeHal` reports an asserted
e-stop, the `SafetyManager` latches into `fault_latched` and the motor-bridge
`disable()` path runs every cycle. You can confirm the latched-safe state by
reading the live `SafetyManager` (its address is the static `app` inside
`r2_app_main`); the exact offset depends on the build, so the simplest proof is
4c: the loop runs and never actuates.

**If it faults** (PC parks in `HardFault_Handler`/`Default_Handler` right after
reset): most often a stack-pointer or vector-table mismatch. Sanity-check the
first two flash words — `mdw 0x08000000 2` should read the initial SP
(`0x2000C000`, top of the 48 KiB SRAM) then the reset-vector address (odd, Thumb
bit set). Capture the fault and share it.

## 5. Next

Once the safe boot is confirmed, drivers land seam-by-seam (#967), non-actuating
first (clock/PLL, watchdog, UART, ADC, encoders), each replacing its `Safe*`
seam and validated on the board before the motor/steering outputs — which come
last, wheels still off the ground. The production image is the slot-A build
(`r2_firmware_image.bin`, behind the bootloader); this standalone image is a
bring-up aid only.
