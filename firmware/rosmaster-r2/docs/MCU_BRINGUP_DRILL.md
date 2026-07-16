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

- ST-Link V2 (or clone). The OpenOCD commands below use `interface/stlink.cfg`;
  for a J-Link, substitute `interface/jlink.cfg` (everything else is the same).
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
ID, stop and reconcile the part against `hal/include/r2/hal/board_manifest.hpp`
(`kExpectedSharedBoardMcu = "STM32F103RCT6"`) before flashing.

## 3. Flash

> **⚠ This image is linked at `0x08000000` and spans the whole 256 KiB flash.**
> Flashing it **erases everything**, including any preinstalled bootloader +
> public key (`0x08000000`) and both application slots. Only flash the bring-up
> image to a blank or development part. On a board that already carries the
> bootloader, flash the **slot-A** image (`r2_firmware_image.bin` at
> `0x08006000`) through the bootloader instead, so the boot + key region is
> preserved.

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

## 4b. No debug probe? Confirm via the status-LED heartbeat

If you have no ST-Link/J-Link and are flashing over the STM32 serial bootloader
(below), you can't use GDB — but the image gives a **visible sign of life**: it
drives the status LED (PC13, per `hal/include/r2/hal/board_manifest.hpp`),
toggling it every `half_period` control cycles so it blinks steadily while the
loop runs.

- **LED blinking** → the reset path, runtime init and control loop all ran; the
  image is alive and looping in the latched-safe state. Boot confirmed.
- **LED solid or dark** → it never reached the loop (hardfault at reset, or a
  wrong vector table / SP). Same failure modes as the GDB "faults" note above.

The blink rate is approximate (the loop isn't paced by a real time base yet), so
judge presence, not precision. If the board's LED isn't on PC13, change the pin
in `firmware/src/status_led_stm32.cpp` (the single place it's defined).

## 4c. Flashing over the serial bootloader (no probe)

The STM32F103 has a ROM serial bootloader (USART1) entered with **BOOT0 = 1 at
reset** (jumper/DIP/button on the control board). With `stm32flash`:

```bash
sudo apt-get install -y stm32flash
PORT=/dev/ttyUSB0   # the STM32's UART bridge — confirm which one it is first

# 1. BACK UP the existing firmware BEFORE writing anything (reversible!).
stm32flash -r stock_stm32_backup.bin "$PORT"

# 2. Write the bring-up image and run it.
stm32flash -w build-target/r2_firmware_bringup.bin -v -g 0x08000000 "$PORT"

# To restore the robot's original firmware later:
stm32flash -w stock_stm32_backup.bin -v -g 0x08000000 "$PORT"
```

> **⚠ This erases the factory STM32 firmware** (the bring-up image spans the whole
> flash). Keep `stock_stm32_backup.bin` safe — it's the only way back. Wheels off
> the ground, motor bus disconnected.

Identify the STM32's port (vs. e.g. the lidar's) with
`udevadm info /dev/ttyUSB0 | grep -E "ID_VENDOR|ID_MODEL"`.

## 5. Next

Once the safe boot is confirmed, drivers land seam-by-seam (#967), non-actuating
first (clock/PLL, watchdog, UART, ADC, encoders), each replacing its `Safe*`
seam and validated on the board before the motor/steering outputs — which come
last, wheels still off the ground. The production image is the slot-A build
(`r2_firmware_image.bin`, behind the bootloader); this standalone image is a
bring-up aid only.
