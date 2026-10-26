# Calibration plan

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
~14 days. This means data collection does not depend on the laptop being in
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
2. **Reference device**: one thermometer is declared correct; the others
   are fitted to it.
3. **Reference values**: typed in from a trusted instrument at some moments,
   or a humidity salt test: a sealed box over saturated NaCl solution holds
   75.3 %RH (MgCl₂: 33 %RH) at room temperature. This is the cheap gold
   standard for hygrometers and gives absolute humidity calibration.

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

1. Start a calibration session in the app. It records the start time and the
   coefficients on each device, and optionally sets each device clock and its
   logger averaging to 1 minute.
2. **Phase A, inside, ≥ 6 h (overnight is ideal).** Put the devices side by
   side, a few cm apart, same orientation, not touching. Keep them away from
   radiators, sun, laptop exhaust and people.
3. **Phase B, outside, ≥ 3 h.** Shaded and sheltered from rain, still a few cm
   apart, with free airflow: an open box or under a roof is fine. Move them all
   at once. Simultaneity is nice but not required, because transients get
   excluded anyway.
4. **Phase C, inside, ≥ 3 h.** Same spot as phase A.
5. Stop the session. The app shows proposed coefficients, before/after
   charts and validation numbers; the user confirms before anything is written.

If the laptop can't hear the devices outside, the app downloads the gap from
each device's logger after the session.

## 4. Algorithm

All of it is pure computation on time series: no Bluetooth involved, fully unit-testable.

1. **Gather** per-device samples for the session window: live CSV history,
   merged with logger downloads where live data has gaps.
2. **Resample** to a common 1-minute grid (mean per bin; empty bins dropped).
   Devices sample unsynchronized every ~10 s, so raw samples never line up.
3. **Select steady bins.** Compute the group consensus (median across
   devices) and keep a bin only if
   - the consensus temperature changes less than ~0.02 °C/min over a 15-min window, and
   - every device has data in it.

   This automatically discards the minutes to hours after each move while the
   cases equilibrate (the time constant is measured from the data, not assumed),
   plus door-opening spikes and similar. Phases are detected as the large
   transitions between steady stretches. That is used for validation and for
   reporting "range achieved", not for fitting.
4. **Fit temperature.** For each device, fit `ref = a_i · reading_i + b_i` by
   robust regression (Huber loss) against the consensus of the *other*
   devices, on steady bins of phases A+B. Re-compute the consensus from
   corrected data and repeat 2–3 times (converges quickly). Then apply the
   chosen anchor (§2).
5. **Decide gain vs offset only.** If the temperature span of steady data is
   small (< ~5 °C), or `a_i` is within two standard errors of 1, use an
   offset-only fit. A noisy slope does more harm than good.
6. **Fit humidity** the same way, after temperature correction. Exclude bins
   above ~95 %RH: the firmware clamps at 99.99, and SHT4x creep is worst there.
   Refinement: compare via dew point, so that a device sitting 0.1 °C
   warmer is not mis-attributed as a humidity error.
7. **Validate** on phase C: report per-device residuals and the group spread
   before → after. Accept only if the spread improves.
8. **Apply** after confirmation: compute `k', z'` (§1), write with `0x25`,
   read back and verify. Record old and new coefficients and the fit summary in
   `~/.local/share/thermo/calibrations/` for undo.

## 5. Implementation plan

| Step | Crate | Content |
|---|---|---|
| 1 | `crates/atc` | pvvx GATT client on `bluer`: connect, command/notify, read/write config (`0x55`), sensor coefficients (`0x25`/`0x26`), clock (`0x23`), logger download (`0x35`). Port of `atc_config.py`. |
| 2 | `crates/calibration` | Pure maths: resampling, steady-state detection, robust fit, anchoring, coefficient conversion. Tests on synthetic sessions with known injected gains/offsets, noise, lag and transients: the fit must recover them. |
| 3 | `thermo calibrate` CLI | Run the fit offline on existing CSV history for a time range and print proposals. Nothing is written to devices, so the maths can be judged on real data first. |
| 4 | TUI | Calibration screen: start/stop session, live progress (steady minutes per phase, temperature/humidity range covered), result preview with corrected curves, apply with confirmation, undo. |
| 5 | Logger | Download and merge on-device history for phases out of radio range. |

Safety rules for writing to devices: write only after explicit confirmation;
refuse if a device's coefficients changed since the session started; always
read back after writing; keep the previous values for one-key undo.

## 6. Open questions

- **Anchor:** consensus only, or a reference instrument or salt test?
- **Radio range:** do the devices reach the laptop from the planned outdoor
  spot? If yes, step 5 can wait.
- **Humidity gain:** apply it, or offsets only for humidity? Gain is where
  sensor ageing shows, so it is worth fitting but is the least certain part.
