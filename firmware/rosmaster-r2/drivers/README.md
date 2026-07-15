# Driver and BSP closure plan

No STM32 register implementation is committed until the target board revision
and R2 harness are physically verified. Portable code depends only on the HAL
interfaces.

Required drivers:

| Driver | Implementation strategy | Completion evidence |
|---|---|---|
| Clock/timebase | 64-bit extension of free-running hardware timer | drift/overflow/reset tests |
| Motor bridge | TIM1/TIM8 preload, synchronous update, hardware-off default | oscilloscope truth table |
| Encoders | TIM2/3/4/5 quadrature + 10 kHz extension snapshot | 2× max edge-rate campaign |
| Steering | hardware PWM preferred over lesson software PWM | pulse jitter/endpoints |
| IMU | separate ICM20948 SPI and MPU9250 I²C implementations | board option + six-face test |
| Battery ADC | timer-triggered ADC + DMA + calibrated reference | supply sweep |
| E-stop | hardware cut/break plus diagnostic GPIO | electrical <10 ms measurement |
| UART | USART1 circular RX DMA + idle-line spans, DMA TX | BER/latency/flood test |
| Watchdogs | IWDG plus software alive supervision | stuck-task/reset injection |
| Flash | page-aligned A/B records, readback, power-loss safe | reset-at-every-boundary test |
| LED/buzzer/RGB | asynchronous low-priority status patterns | no control jitter change |

The board-support package must export pin selections from one board-revision
manifest. It must fail compilation for unresolved motor, steering, IMU, battery
or E-stop options; “auto-detection” cannot choose safety wiring.
