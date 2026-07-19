# KITT Scanner — Jetson ↔ MCU serial protocol (v1)

> **Single source of truth** for the KITT LED scanner wire contract. BOTH halves
> cite this doc:
> - the **MCU firmware** (XIAO/QT Py driving the WS2812B strip) implements the
>   *receiver* side, and
> - the **Jetson-side hooks** (Phase-3: the TTS amplitude streamer + the
>   `/kirra/enforcement_action`→mode bridge) implement the *sender* side.
>
> The two halves are built in different sessions, weeks apart. Freeze this
> contract BEFORE either is written (same discipline as freezing the KIRRA
> trajectory contract) so they cannot diverge. Any change bumps `PROTO_VERSION`
> and updates this doc in the same commit.

## 0. Design goals

- **Unambiguous:** amplitude payload bytes can never be mistaken for a mode
  command (the classic "T then raw bytes vs framed" failure). Every message is a
  framed packet with a start-of-frame anchor and a CRC.
- **Low-latency:** small fixed framing; amplitude streams at 50 Hz, trivial at
  115200 baud (~600 B/s peak).
- **Fail-safe (KIRRA ethos):** if the MCU hears nothing for `LINK_TIMEOUT_MS` it
  reverts to IDLE — a dead Jetson link → the calm idle sweep, never a frozen bar.
- **Trivially implementable** on an Arduino/CircuitPython MCU (~40 lines) and in
  Python on the Jetson.

## 1. Physical layer

| Parameter | Value |
|---|---|
| Transport | USB CDC serial (the MCU's native USB) |
| Baud | **115200**, 8N1, no flow control |
| Direction | Jetson → MCU (commands) primary; MCU → Jetson (HELLO only) optional |
| Byte order | little-endian for multi-byte fields |

The MCU port appears on the Jetson as `/dev/ttyACM*` (or a udev symlink, e.g.
`/dev/kitt_scanner` — recommended, add a rule so it's stable).

## 2. Frame format

Every message — command or amplitude — is ONE frame:

```
+------+------+------+---------------+-------+
| SOF  | CMD  | LEN  | PAYLOAD[LEN]  | CRC8  |
| 0xAA | 1 B  | 1 B  | LEN bytes     | 1 B   |
+------+------+------+---------------+-------+
```

- `SOF = 0xAA` — start-of-frame / resync anchor. If the receiver is mid-garbage
  it scans for `0xAA` and restarts.
- `CMD` — an ASCII command letter (table below).
- `LEN` — payload length, 0–255.
- `PAYLOAD` — `LEN` bytes (may be 0).
- `CRC8` — over `CMD | LEN | PAYLOAD` (NOT the SOF). Polynomial **0x07** (CRC-8/SMBUS),
  init 0x00, no reflection, no final xor. A frame with a bad CRC is DROPPED
  silently (the sender will send another within 20 ms in TALK mode).

Max frame = 1 + 1 + 1 + 255 + 1 = 259 bytes. In practice payloads are 0–2 bytes.

## 3. Commands

| CMD | Name | Payload | Meaning |
|---|---|---|---|
| `'I'` (0x49) | IDLE | none (LEN 0) | Slow Larson sweep. **Default power-on state.** Persistent. |
| `'S'` (0x53) | DRIVE | none (LEN 0) | Fast Larson sweep — robot is actively driving. Persistent. |
| `'T'` (0x54) | TALK | none (LEN 0) | Enter amplitude-reactive mode. Persistent until another mode. |
| `'V'` (0x56) | AMPL | 1 byte, `0–255` | One amplitude sample. **Rendered only while the persistent mode is TALK**; ignored otherwise. |
| `'A'` (0x41) | ALERT | 2 bytes, `uint16 LE` `duration_ms` | Momentary red full-bar flash OVERLAY for `duration_ms`, then auto-revert to the current persistent mode. Non-persistent (see §5). |
| `'H'` (0x48) | HELLO | 1 byte, `PROTO_VERSION` | Handshake / version check (§6). Bi-directional. |
| `'C'` (0x43) | CONFIG | see §7 | Optional runtime config (brightness, speeds). |

"Persistent mode" = the last of `IDLE`/`DRIVE`/`TALK` received. `AMPL` and `ALERT`
do not change the persistent mode.

## 4. Amplitude semantics (the TALK sync)

- The Jetson computes a **rolling RMS envelope** of the audio it is *generating*
  (not a mic) over a ~20 ms window, normalizes to the speech's dynamic range, and
  maps to a byte `0–255`.
- **Stream rate: 50 Hz** (one `'V'` frame every 20 ms) while KITT speaks. This is
  smooth to the eye and ~300 B/s.
- MCU mapping (firmware-owned, tune to taste): amplitude → the lit bar's
  **width AND brightness** — louder syllables = wider/brighter, gaps = a dim
  narrow core. A reasonable default: `width = 1 + round(ampl/255 * (N-1))`,
  `brightness = 40 + round(ampl/255 * 215)`.
- **End of speech:** the Jetson sends `'I'` (or `'S'` if driving) to leave TALK.
  It does NOT need to send a final `'V' 0` — the mode change ends the pulse.
- If TALK is set but no `'V'` arrives for `TALK_SILENCE_MS` (default 300 ms), the
  MCU shows a dim static core (speech paused) — it does NOT revert mode (the
  Jetson owns mode).

## 5. ALERT semantics (the KIRRA-state hook)

The high-value tie-in: the scanner reflects the *actual* safety-governor verdict.

- The Jetson-side bridge subscribes to `/kirra/enforcement_action` (already
  published every command cycle: `PASS:v=…` / `BLOCKED:<reason>` / `SafeStop`).
- On a **rising edge into `BLOCKED`/`SafeStop`** (a refusal the checker just made),
  the bridge sends `'A'` with `duration_ms` (e.g. 400).
- The MCU **overlays** a red full-bar flash for that duration, then returns to
  whatever persistent mode was active (IDLE/DRIVE/TALK) — so a momentary refusal
  produces a visible flash without the Jetson tracking a clear.
- Repeated refusals re-trigger `'A'` (the flash restarts). The bridge should
  debounce so a sustained refusal doesn't strobe (e.g. min 250 ms between `'A'`).

This makes the cosmetic and the thesis the same event: **KITT refuses an unsafe
command → the scanner flashes red at that instant.**

## 6. Version handshake

- `PROTO_VERSION = 1` (this doc).
- On boot the MCU MAY send `H | 0x01 | PROTO_VERSION | CRC` to the Jetson.
- The Jetson SHOULD send `'H'` with its version once on connect; if the MCU's
  reply (or its unsolicited HELLO) is a different major version, the Jetson logs a
  mismatch and refuses to stream (fail-loud, don't drive a bar with the wrong
  contract). Absent a HELLO, assume v1 (back-compat for a bare firmware).

## 7. CONFIG (optional, forward-compatible)

`'C'` payload is a list of `[key, value]` byte pairs (LEN even). Unknown keys are
ignored (forward-compat). Reserved keys:

| key | value | meaning |
|---|---|---|
| 0x01 | 0–255 | master brightness |
| 0x02 | 0–255 | idle sweep period (×10 ms) |
| 0x03 | 0–255 | drive sweep period (×10 ms) |
| 0x04 | 0–255 | base hue (0=red; KITT is red — default 0) |

CONFIG is a nice-to-have; v1 firmware may ignore it entirely.

## 8. Reference: CRC8 (both sides use this exact function)

```python
def crc8(data: bytes) -> int:          # CRC-8/SMBUS, poly 0x07
    c = 0
    for b in data:
        c ^= b
        for _ in range(8):
            c = ((c << 1) ^ 0x07) & 0xFF if (c & 0x80) else (c << 1) & 0xFF
    return c
```

## 9. Reference: Jetson sender (Python, Phase-3)

```python
import struct, serial
SOF = 0xAA
def _frame(cmd: str, payload: bytes = b"") -> bytes:
    body = bytes([ord(cmd), len(payload)]) + payload
    return bytes([SOF]) + body + bytes([crc8(body)])

class Scanner:
    def __init__(self, port="/dev/kitt_scanner", baud=115200):
        self.s = serial.Serial(port, baud, timeout=0)
        self.s.write(_frame("H", bytes([1])))          # announce v1
    def idle(self):   self.s.write(_frame("I"))
    def drive(self):  self.s.write(_frame("S"))
    def talk(self):   self.s.write(_frame("T"))
    def ampl(self, a: int):  self.s.write(_frame("V", bytes([a & 0xFF])))   # 50 Hz in TALK
    def alert(self, ms=400): self.s.write(_frame("A", struct.pack("<H", ms)))
```

## 10. Reference: MCU receiver (pseudocode)

```
state = { mode: IDLE, ampl: 0, alert_until: 0, last_rx_ms: now }
loop forever:
    # --- parse (non-blocking) ---
    while serial.available():
        b = serial.read()
        feed(b) into a small state machine: [SOF][CMD][LEN][PAYLOAD..][CRC]
        on a COMPLETE, CRC-valid frame:
            last_rx_ms = now
            switch CMD:
              'I','S','T': mode = that
              'V': if mode == TALK: ampl = payload[0]
              'A': alert_until = now + u16le(payload)
              'H': (optionally reply H|version); check version
              'C': apply config pairs
    # --- render (fixed tick, e.g. every 10–20 ms) ---
    if now - last_rx_ms > LINK_TIMEOUT_MS: mode = IDLE      # fail-safe
    if now < alert_until: render_alert_red_flash()          # overlay wins
    elif mode == IDLE:  render_larson(period_idle)
    elif mode == DRIVE: render_larson(period_drive)
    elif mode == TALK:  render_pulse(ampl)                  # width+brightness ~ ampl
    show()
```

## 11. Constants (pin these)

| Name | Value | Owner |
|---|---|---|
| `PROTO_VERSION` | 1 | this doc |
| Baud | 115200 | this doc |
| `SOF` | 0xAA | this doc |
| CRC | CRC-8/SMBUS (poly 0x07) | this doc |
| Amplitude range | 0–255 (RMS-mapped) | this doc |
| Amplitude rate | 50 Hz | this doc |
| `LINK_TIMEOUT_MS` | 1000 | this doc |
| `TALK_SILENCE_MS` | 300 | this doc |
| ALERT default duration | 400 ms | Jetson bridge |
| LED count / data pin / strip type | firmware-owned | MCU firmware |
| RMS window, normalization curve | Jetson-owned | Jetson streamer |

## 12. Build/test order (contract-first)

1. **MCU firmware now** (other session) — implement §2/§3/§10 + the Larson sweep,
   testable with a serial terminal (`printf '\xAAI\x00…'`) and fake `'V'`
   amplitude (a sine) before the strip or Jetson exist.
2. **Phase 3 — Jetson hooks** (this repo, later) — the §9 sender wired to (a) the
   TTS RMS envelope → `ampl()` at 50 Hz, and (b) `/kirra/enforcement_action` →
   `alert()`/mode. Built against THIS doc, so they meet the firmware exactly.

## Status
- ⬜ MCU firmware (other session) — write against this contract.
- ⬜ Jetson TTS amplitude streamer — Phase 3 (needs the voice stack).
- ⬜ Jetson `/kirra/enforcement_action`→mode bridge — Phase 3.
- ✅ Protocol frozen at v1 (this doc).
