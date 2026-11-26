# Calibration

Goal: make all thermometers agree (and, optionally, be absolutely right) by
co-locating them through several temperature/humidity conditions and letting
the app compute and write per-device correction coefficients.

## 1. What the pvvx firmware offers

Verified in `pvvx/ATC_MiThermometer` `src/sensors.c` and `src/cmd_parser.c` (v5.9).

Each sensor reading is converted from the raw 16-bit SHT4x value with a
**linear** formula, separately for temperature and humidity:

```
temp [0.01 °C] = (raw_t * temp_k) >> 16 + temp_z
humi [0.01 %]  = (raw_h * humi_k) >> 16 + humi_z      (then clamped to 0..99.99)
```

So "shift" is the offset `z` and the "curve" is the gain `k`. There is no
non-linear curve: calibration is one straight line per quantity.

| Command | Bytes | Effect |
|---|---|---|
| `0x25` get | `[0x25]` → `[0x25, temp_k u32, humi_k u32, temp_z i16, humi_z i16, sensor_id u32, i2c_addr, sensor_type]` | read coefficients |
| `0x25` set | `[0x25, temp_k u32, humi_k u32, temp_z i16, humi_z i16]` | write, persisted to flash |
| `0x26` | `[0x26]` | reset to factory coefficients |

Current state of all four devices (read 2026-10-26): sensor type 2 (SHT4x),
`temp_k=17500 temp_z=-4500 humi_k=12500 humi_z=-600`, which are the Sensirion
datasheet formulas, i.e. uncalibrated.

**Applying a correction.** If we want `corrected = a · reported + b` (b in °C or %RH):

```
k' = round(a · k)
z' = round(a · z + 100 · b)
```

Resolution is 1/17500 ≈ 0.006 % for gain and 0.01 for offset, far finer than
needed. Constraint: `raw · k` must fit in an i32, so `k < 32768` (gains up to ~1.8×).
Because corrections compose, re-calibrating later starts from whatever is on
the device; the factory values are always recoverable with `0x26`.

**On-device logger** (`0x35` read, `0x36` clear, `0x23` set clock): ~20 800
records of `{time u32, temp i16, humi u16, vbat u16}`, each an average over
`averaging_measurements × measure_interval`. At 1-minute averaging that is
~14 days. This means data collection does not depend on the phone being in
radio range. The logger stores *calibrated* values, which is fine as long as
coefficients do not change during a session.

## 2. Can it be automatic? Yes, with one caveat

Co-located devices see the same air, so any disagreement between them is
sensor error. From that, the app can fit `a_i, b_i` for every device fully
automatically.

**Caveat: agreement is not accuracy.** If all four read 0.3 °C high, no amount
of comparing them with each other reveals it. Consensus calibration has a
"gauge freedom": the group as a whole can be shifted or scaled freely. It has to
be pinned by one of:

1. **Consensus** (default): constrain the corrections to average to zero
   (mean a = 1, mean b = 0). The devices then agree with their own average.
   With SHT4x (±0.2 °C, ±1.8 %RH typical) the average of four is usually
   better than any single one.
2. **Reference device** ("Correct to…" in the calibration screen's menu): one
   thermometer is declared correct; the others are corrected to read like it. The
   fit is still the consensus one, so every device helps estimate how the others
   differ; only the anchor (§4, step 7) moves to the reference. The reference
   needs an hour of steady data of its own and gets nothing written.
3. **Reference values**: typed in from a trusted instrument at some moments,
   or a humidity salt test: a sealed box over saturated NaCl solution holds
   75.3 %RH (MgCl₂: 33 %RH) at room temperature. This is the cheap gold
   standard for hygrometers and gives absolute humidity calibration. The app's
   **salt box** does this for humidity: once the box has settled, every humidity
   correction is shifted so the group reads NaCl's value (`model/Salt.kt`; see
   `android/README.md` for when a box counts as settled).

**Why your three-phase idea is right:**

- Phase A (inside) alone gives offsets only. Indoor temperature barely moves,
  so slopes can't be estimated.
- Phase B (outside) adds a second operating point: ~10–15 °C colder and much
  more humid this time of year. That range is what makes the gain `a`
  measurable. For humidity, 55 % → 85 % is a very useful span.
- Phase C (back inside) is not needed for fitting. It is valuable as
  **held-out validation**: fit on A+B, check that C agrees. It also exposes
  hysteresis, since humidity sensors can "creep" after long high-humidity exposure.

## 3. Procedure for the user

Offsets need only one place; gains need steady data at two temperatures at least
5 °C apart (15 %RH for humidity). Only live adverts are used, so the phone must stay in
radio range of the devices; the app records with its screen off and when closed.

1. **Together.** Pick the devices for a calibration and lay them side by side, a few cm
   apart, the same way up, not touching, away from radiators, sun and people. The first
   estimate comes after about 40 minutes; then the app shows how precise corrections
   written now would be and how long each better level takes.
2. **Second place (optional), for the gains.** Move them all at once somewhere at least
   5 °C warmer or colder, e.g. outside: shaded, sheltered from rain, still in range, and
   tap "I've moved them". Each device's readings there count once it has caught up with
   the others, and readings above 95 %RH are dropped, so avoid fog and rain. Half an
   hour of steady readings there gives a usable gain, longer a better one.
3. **Write** when the precision is enough. The window starts no earlier than the last
   calibration of the devices taking part. Keep them together a while afterwards: what
   is left to correct should stay small.

## 4. Algorithm (as implemented in the app's `calibration/`)

A port of the archived Rust crate (`archive/tui/crates/calibration`), which this section
first described. The app's sessions use it with shorter windows (20 minutes), hold each
device back after a move until it has caught up, and trim the gain's span by 15 minutes
at each end instead of percentiles; `android/README.md` has the numbers and the tests on
real data behind them.

All of it is pure computation on time series: no Bluetooth involved, fully unit-testable.
Temperature and humidity are handled independently.

1. **Gather** each device's *live* samples for the window (downloaded log records
   are 30-minute averages and are not used). The window starts no earlier than the
   last coefficient change of any participating device (`calibrations.csv`).
2. **Resample** to a common 5-minute grid. Reception is lossy (on a laptop only
   every second or third measurement arrives), so 1-minute bins were mostly empty.
3. **Select usable readings.** A device's bin is used only if, over a 30-minute
   window, both the group median and the device itself changed by at most
   0.75 °C / 3.5 %RH, and the group median did not change by 2 °C / 10 %RH or more
   in the preceding hour. That allows ordinary drifts (a room cooling in the
   evening) but drops the settling hour after a move. The 30-minute window is centered on
   the bin and must lie within the data, so the first and last 15 minutes never
   count: with at least an hour of usable data required, the shortest possible
   window is 1.5 h (the calibration screen offers 2 h and up).
4. **Fit** each device against the median of the group, itself included (robust
   Huber regression, four rounds, each against the readings as the previous one
   corrected them, then a last round against the others only):
   `reference ≈ gain · reading + offset + k · rate`, where `rate` is the device's own
   rate of change. The `k · rate` term absorbs case *lag*: in real data the devices
   responded up to ~6 minutes apart, which during a 1 °C/h drift looked like a
   0.1 °C offset. Lag is modelled only when rates vary enough to tell it from an
   offset (a constant drift makes the two indistinguishable); it is reported but
   cannot be written to the firmware. The device itself counts in the median:
   compared with the others only, an even number of devices swings from round to
   round, and two devices swap their errors every round and are never corrected.
   Both settle on the same corrections, but with itself in the median a device's
   lag looks ~150 s smaller (corrections don't include lag, so the median shares
   it); the last round, from settled corrections, measures lag against the others,
   as do the residuals reported afterwards.
5. **Gain or offset only.** A gain is fitted only if the steady data spans ≥ 5 °C /
   15 %RH and differs from 1 by more than two standard errors.
6. **Outliers.** With four or more devices, a device whose correction at the typical
   operating point has a robust z-score above 3.5 is excluded from the consensus
   and the anchor (its own fit still pulls it to the group). Its corrections are not
   written, neither temperature nor humidity: both are measured in the same spot,
   so whatever set the device apart skews both fits.
7. **Anchor** so that gains average 1 and offsets 0 over devices that are not
   outliers and have at least an hour of usable data, or, with a reference device,
   so that its correction is the identity (`gain_i / gain_ref`,
   `(offset_i − offset_ref) / gain_ref`).
8. **Apply** after confirmation: compose onto the device's current coefficients
   (`k' = a·k`, `z' = a·z + 100·b`), write with `0x25`, read back and verify, log old
   and new values to `calibrations.csv`. "Restore factory calibration" in the
   calibration screen's menu brings back the factory coefficients.

## 5. Status

Done in the Android app: the BLE client, the fit, a guided calibration session with a
precision estimate and the time to each level, the hold after a move, the salt box,
clock sync with rate trimming, and log download with drift-corrected time mapping. The
archived terminal app had all but the session, the hold and the salt box.

Findings from the first real run (5 devices on a table, 2026-10-26, with the terminal
app):

- None of the device clocks had ever been set; they counted seconds since flashing.
  Log records are mapped to host time via the clock offset (and linear drift since
  the last set, if the clock was set to real time). Syncing a clock first downloads
  the history newer than the last download, since older records can't be dated
  reliably afterwards.
- Synced clocks gained about 30 s in a day and a half (~200 ppm), all five alike:
  the chip keeps time through sleep with its 32 kHz RC oscillator. When the last
  sync is at least 12 h old, a sync also lengthens or shortens the device's second
  (`0x24`, in 16 MHz timer ticks, ±2000 ppm, kept in flash) by the measured drift.
  The rate still depends on temperature.
- Connecting made BlueZ pause discovery; the terminal app's scanner restarted it 5 s
  after the last device job. A connection completed after giving up is disconnected
  explicitly, because a connected thermometer stops advertising.

Not done: dew-point-based humidity comparison, validation fit on a held-out last
phase.
