# ROSMASTER R2 clean-room hardware reference

> **Evidence status:** repository research complete; target-unit electrical
> verification incomplete. No row enables actuation until the R2 harness
> manifest is continuity-verified.

## Provenance and confidence

Research is pinned to:

- Yahboom `ROSMASTER-R2` commit
  [`3d0919af47f4dae9c77ffeea3b594795cd4f482c`](https://github.com/YahboomTechnology/ROSMASTER-R2/tree/3d0919af47f4dae9c77ffeea3b594795cd4f482c);
- Yahboom `ROS-robot-expansion-board` commit
  [`856d96585d8eaf240c63271b34102a5bc46cdd7c`](https://github.com/YahboomTechnology/ROS-robot-expansion-board/tree/856d96585d8eaf240c63271b34102a5bc46cdd7c);
- Kirra's physical R2 bench record in
  `docs/hardware/HARDWARE_FINDINGS_R2X3.md` and
  `docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md`.

`Official R2` means the first repository directly identifies the interface.
`Official board` means the shared expansion board documentation identifies
board capability but not necessarily the R2 harness. `Corroborated` means a
non-official reconstruction agrees and must still be checked. `Unknown` means
the firmware must not enable the function.

### Source registry

Manifest `source_id` values resolve here:

| ID | Immutable or official source | Applicability |
|---|---|---|
| `YB-GPIO` | [`ROS-robot-expansion-board@856d965` README](https://github.com/YahboomTechnology/ROS-robot-expansion-board/blob/856d96585d8eaf240c63271b34102a5bc46cdd7c/README.md) plus R2 hardware-course GPIO lessons | Shared board; unit revision unverified |
| `YB-BUZZER` | [official key/buzzer lesson](https://www.yahboom.net/public/upload/upload-html/1689906389/3.%20Key%20control%20buzzer%20sounding.html) | Shared board |
| `YB-UART` | [`myserial.rules@856d965`](https://github.com/YahboomTechnology/ROS-robot-expansion-board/blob/856d96585d8eaf240c63271b34102a5bc46cdd7c/2.Python%20basic%20control/0.%20Bind%20PCB%20port%20devices/myserial.rules) and R2 expansion-board PDF blob `65884ccdc81b01821137d029976452cdc46f9577` | CH340 identity official; PA9/PA10 requires unit inspection |
| `YB-MOTOR` | [official motor lesson](https://www.yahboom.net/public/upload/upload-html/1689907246/12.%20Control%20motor%20forward%20and%20reverse.html), R2 PDF blob `80941c1…` | Shared four-channel capability |
| `CORR-MOTOR` | [`Tchaikovsky66/Rosmaster@9452f7b` motor map](https://github.com/Tchaikovsky66/Rosmaster/blob/9452f7b28c3e943f72d182b6a200fdbb3fe8cefd/BSP/MOTOR/motor.h#L15-L28) | Corroboration only |
| `YB-ENCODER` | [official encoder lesson](https://www.yahboom.net/public/upload/upload-html/1689907268/13.%20Timer%20Capture%20Encoder%20Data.html), R2 PDF blob `e45111f…` | Shared timer capability |
| `YB-SERVO` | [official PWM-servo lesson](https://www.yahboom.net/public/upload/upload-html/1689906624/9.%20Timer%20interrupt%20control%20PWM%20servos.html) | Shared outputs; R2 connector unknown |
| `YB-R2-IMU` | [official ICM20948 lesson](http://www.yahboom.net/public/upload/upload-html/1703065902/11.%20Nine-axis%20attitude%20sensor%20to%20obtain%20data-ICM20948.html), R2 PDF blob `8eb4e05…`; conflicting shared-board README above says MPU9250 | Mutually exclusive candidates; disabled until inspection |
| `KIRRA-BENCH-R2` | `docs/hardware/HARDWARE_FINDINGS_R2X3.md` and `docs/hardware/R2_PATH_B_ACKERMANN_DRIVE.md` | Existing physical unit, not a lot-wide netlist |

Ellipsized PDF blob prefixes are discovery aids; the pinned repository path and
full commit are normative until page-level extracts are captured in Phase 1.

## Board and transport

| Item | Finding | Evidence |
|---|---|---|
| MCU | STM32F103RCT6, Cortex-M3, 72 MHz, 256 KiB flash, 48 KiB SRAM, LQFP64 | Official board |
| SBC UART | USART1: PA9 TX, PA10 RX; 115200 8-N-1 in current material | Official board |
| USB bridge | CH340, VID:PID `1a86:7523`; udev alias `/dev/myserial` | Official board |
| Debug | PA13 SWDIO, PA14 SWCLK | Corroborated; inspect header |
| Update | STM32 ROM UART boot: BOOT0 + RESET, CH340, MCUISP/FlyMCU, `.hex` | Official R2 |
| In-application bootloader | No public evidence | Unknown |

The factory procedure is not authenticated OTA. BOOT0/RESET strongly indicates
the STM32 ROM loader. A clean-room bootloader must preserve a recoverable ROM
loader path.

The v1 motion command is about 68 decoded bytes and at most 70 encoded bytes
including delimiter. At 115200 baud that is about 6.1 ms on the wire (10 UART
bits per byte), before CH340 and Linux delay. The required sub-2 ms end-to-end
command latency is therefore impossible at the factory rate. Production needs
a successfully validated high-rate UART candidate (initially ≥921600 baud),
direct TTL UART, or a revised CAN-FD interface.

## GPIO and peripheral map

The machine-readable copy is `hal/include/r2/hal/board_manifest.hpp`.

| Function | Pin / peripheral | Evidence | R2 action |
|---|---|---|---|
| Status LED | PC13, active-low GPIO | Official board | Verify color/polarity |
| Active buzzer | PC5 GPIO | Official board | Verify polarity |
| User key | PD2 GPIO | Official board | Verify pull-up/debounce |
| UART | PA9/PA10 USART1 | Official board | Scope voltage and maximum baud |
| Servo S1–S4 | PC3/PC2/PC1/PC0, TIM7 software PWM in lessons | Official board | Identify R2 steering connector |
| RGB | PB5, SPI3 MOSI/DMA, WS2812 encoding | Official board revision-dependent | Inspect revision |
| SWD | PA13/PA14 | Corroborated | Locate pads and recovery policy |

## Motor bridge

The shared board exposes four AM2857 H-bridge modules:

| Channel | PWM pair | Evidence |
|---|---|---|
| M1 | PC6 TIM8_CH1 / PC7 TIM8_CH2 | Official board |
| M2 | PC8 TIM8_CH3 / PC9 TIM8_CH4 | Official board |
| M3 | PA11 TIM1_CH4 / PA8 TIM1_CH1 | Corroborated pair mapping |
| M4 | PB0 TIM1_CH2N / PB1 TIM1_CH3N | Corroborated pair mapping |

R2 has two 520 propulsion motors and one steering gear, not four driven wheels.
Kirra bench evidence maps R2 rear-left to M1 and rear-right to M4, both positive
for forward; M2/M3 were unpopulated. This mapping is unit-specific bench
evidence and must be confirmed by continuity before the BSP enables PWM.

Required electrical inspection:

1. establish H-bridge coast/brake truth table and reset pin state;
2. locate any common enable, fault, current or thermal signals;
3. prove both inputs remain inactive from reset through boot;
4. identify whether a timer break input can disable all bridges in hardware;
5. measure PWM frequency constraints and dead-time needs.

No public evidence establishes current or thermal sensing. Those safety goals
require an external sensor/driver-board revision if the PCB lacks them.

## Encoders

| Channel | Quadrature timer pins | Evidence |
|---|---|---|
| M1 | TIM2 CH1/CH2: PA15/PB3 | Official board |
| M2 | TIM4 CH1/CH2: PB6/PB7 | Official board |
| M3 | TIM5 CH1/CH2: PA0/PA1 | Official board |
| M4 | TIM3 CH1/CH2: PA6/PA7 | Official board |

Each timer runs TI1+TI2 quadrature mode. The architecture never services each
edge in an ISR: hardware counts continuously and a 5–10 kHz timer service
extends 16-bit counters and timestamps snapshots. R2 channel signs, electrical
levels and right encoder counts/revolution remain unverified. Existing bench
data measured approximately 834.5 ticks/revolution on the left and found a
material left/right mismatch; it is calibration evidence, not a factory default.

## Steering

The R2 uses one metal steering gear and an Ackermann linkage. Shared-board
lessons expose S1–S4 PWM servo outputs and a selectable 5 V/6.8 V rail, but no
public R2 source identifies the exact steering connector. Existing host-side
bench work found:

- steering responds only in vendor car-type 5 mode;
- host command units are `[-45, +45]`, negative meaning left;
- the default-angle command accepts `[60, 120]`;
- the getter returns `-1` and cannot establish center;
- road-wheel angle, pulse width, linkage ratio and physical endpoints remain
  unmeasured.

The clean-room driver therefore accepts calibrated pulse widths and road-wheel
angles only. It does not embed vendor command units.

## IMU

Two official board variants conflict:

| Variant | Interface | Pins / addresses |
|---|---|---|
| ICM20948 + AK09916 | SPI2 | PB12 CS, PB13 SCK, PB14 MISO, PB15 MOSI; internal magnetometer `0x0C` |
| MPU9250 + AK8963 | software I²C | PB13 SCL, PB15 SDA, PB14 AD0; main `0x68`, magnetometer `0x0C` |

The board port must probe an explicit manufacturing option or read a board
identity record; silently trying both in production hides assembly errors.
Inspect the chip marking and PB12 route before selecting a driver.

## Battery, LEDs and buzzer

An official Yahboom F103 tutorial uses PA5/ADC_IN5 and a nominal 10 kΩ/3.3 kΩ
divider (`4.03` ratio), but it is not R2-specific evidence. PA5, resistor values,
ADC reference, filtering and calibration must be verified by continuity and
multimeter. Do not commit `4.03` as an R2 default.

PC13 status LED and PC5 buzzer are official shared-board assignments. The RGB
output is revision-sensitive. All visible/audible devices remain lower priority
than motor shutdown.

## Existing SBC protocol: compatibility record only

The official public repositories do not contain a complete production parser.
Kirra bench analysis established partial legacy behavior:

- motor function `0x10`, four signed channels, open-loop, with `127` meaning
  “hold previous” rather than zero;
- Ackermann steering function `0x31`;
- automatic reports arrive in four rotating groups at 10 ms packet intervals,
  so each group updates about every 40 ms;
- vendor UART is 115200 baud.

Header, length, checksum, payload scaling, complete command identifiers, retry,
resynchronization and boot protocol are not publicly established. The new
protocol is intentionally not byte-compatible; compatibility belongs in an
isolated Linux adapter, never in the real-time MCU core.

## R2-specific exclusions

Never import X3 assumptions:

- four driven mecanum wheels or a `Vx/Vy/Vz` mixer;
- lateral body translation;
- zero-radius rotation;
- X3 encoder ordering/signs or wheel constants;
- car-type 1 semantics;
- a generic S1 steering selection;
- vendor odometry or PID calibration.

## Bring-up closure matrix

| Unknown | Method | Motion gate |
|---|---|---|
| PCB revision/MCU marking | Photograph and BOM record | Blocks flash |
| R2 motor channels/polarity | Continuity, then elevated low-duty pulse | Blocks PWM |
| Bridge safe state/enable | Scope reset and fault paths | Blocks floor test |
| Encoder channels/sign/PPR | Hand rotation and counter capture | Blocks closed loop |
| Steering connector/pulse/endpoints | Continuity and protractor fixture | Blocks steering |
| IMU variant/orientation | Chip marking, WHO_AM_I, six-face test | Blocks fused odometry |
| Battery divider/reference | Continuity and calibrated supply sweep | Blocks battery policy |
| E-stop electrical authority | Schematic/continuity and oscilloscope | Blocks any motion |
| High-speed UART | BER and latency under load | Blocks <2 ms claim |
| Oscillator/watchdogs/brownout | Clock measurement and reset-cause injection | Blocks production |
