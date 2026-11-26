# AGENTS.md

Guidance for AI coding agents (Claude Code, Codex, Cursor, Aider, …) working on this
repository. Humans are welcome too.

## What this is

`thermo` is an Android app for BLE thermometers running the
[pvvx ATC_MiThermometer](https://github.com/pvvx/ATC_MiThermometer) firmware (tested on
Xiaomi LYWSD03MMC) or the [pvvx THB2](https://github.com/pvvx/THB2) firmware (THB1, THB2,
BTH01, TH05, …) with BTHome v2 advertising. It listens to adverts, charts them, stores
history as CSV, downloads device logs, syncs clocks, changes settings, levels the
devices' signal and calibrates them against each other in a guided session, writing the
coefficients over GATT. Kotlin and Jetpack Compose, no other dependencies.

**Only the Android app is maintained.** It began as a port of a Rust terminal app for
Linux and macOS, which was archived on 2026-11-26 in [`archive/tui`](archive/tui). Don't
change the archive unless asked. If asked to revive it, bring it up to the Android app's
features with the Android code as the reference; `archive/tui/AGENTS.md` describes that
code base.

## Layout

Code under `android/app/src/main/java/dev/artur/thermo/`:

| Path | What |
|---|---|
| `bthome/` | BTHome v2 payload decoder. No Android dependencies. |
| `calibration/` | Consensus calibration: binning, steady-state selection, Huber fit, outliers, anchoring, plus the precision of each correction and the hold after a move. No Android dependencies. Algorithm described in `docs/calibration.md` §4. |
| `atc/` | The firmwares' command protocol on characteristic `0x1F1F` (ATC) or `0xFFF4` (THB2): `Firmware.kt` names the family by its characteristic and says which features a device has, `Encodings.kt` the byte layouts (config, TX power, coefficients, clock, log, LCD), `Thermometer.kt` the GATT client on `BluetoothGatt`. |
| `ble/` | The scanner (filtered on service data `0xFCD2`) and the one-at-a-time job worker. The only code that talks to Android Bluetooth. |
| `model/` | All state, I/O-free and unit-tested: `ThermoModel` (receives adverts, job events and a 1 s tick, enqueues jobs), `Devices`, `Store` (CSV formats), `Level` (signal levelling), `Calibrate` (glue from history to the fit), `Session` (the calibration session: precision, time to each level, second place), `Salt` (salt box reference), `Speech` (what is spoken and when). |
| `ui/` | Compose screens. `Chart.kt` draws with per-pixel min/max reduction. |
| `Controller.kt`, `MonitorService.kt` | Process-wide wiring, background fits, and the foreground service that records with the app closed. |
| `Speaker.kt`, `ThermoWidget.kt`, `BootReceiver.kt` | Text-to-speech, the home-screen widget, restart after boot. |

Elsewhere:

| Path | What |
|---|---|
| `android/app/src/test/` | JVM unit tests, no phone needed. Some replay real history when environment variables point at it (`RealDataTest`, `PrecisionDataTest`, `SettlingDataTest`, `SessionReplayTest`). |
| `android/app/src/debug/` | `DebugHooks`: simulated demo devices and sessions (`--ez demo true`, see `android/README.md`), showing over the lock screen. |
| `docs/calibration.md` | The firmware's calibration interface, the procedure and the algorithm. |
| `site/` | GitHub Pages site, deployed by `.github/workflows/pages.yml`. Static HTML, no build step. |
| `archive/tui/` | The archived Rust terminal app (Linux, macOS). Not built by CI. |

## Commands

```bash
cd android && ./gradlew testDebugUnitTest      # JVM tests, no phone needed
cd android && ./gradlew lintDebug
cd android && ./gradlew installDebug           # needs a phone with USB debugging
```

Android SDK platform 36 and JDK 17–21; if Gradle picks a newer JDK, run it with
`JAVA_HOME` pointing at JDK 21. Releases are signed with a key outside the repository;
see `android/README.md`.

## Rules

- **Never write to real devices unless the user asks.** Calibration, settings, rename,
  clock sync, rate trim, TX power and factory reset all change device flash. Prefer unit
  tests, and the debug build's demo devices on a phone. Reading adverts is harmless.
- **Don't touch the user's real history** when experimenting: the phone's files under
  `Android/data/dev.artur.thermo/files/thermo/`. Copy them with `adb pull` and work on the
  copy. The demo's history is never saved.
- `model/` must stay I/O-free: it receives events and enqueues jobs. Add tests there for
  new behaviour. `bthome/`, `calibration/` and `atc/Encodings.kt` stay free of Android
  APIs, so they run in JVM tests.
- Only `ble/` and `atc/Thermometer.kt` talk to Bluetooth.
- Match the surrounding style: a comment at the top of each file saying what it is, KDoc
  on declarations, named constants with a comment saying where the value comes from
  (often a test on real data), plain words in the UI and spoken texts.
- Texts that show numbers follow the phone's language for decimals (`"%.1f".format`),
  except spoken ones, which use a decimal point. Don't put a formatted string holding `%`
  into another format string.

## Firmware facts worth knowing

- Command/notify characteristic `0x1F1F`; first byte is the command. Replies echo it.
- THB2 firmware (pvvx/THB2): command characteristic `0xFFF4`, write without response.
  Dev id reply `[0x00, ver, device, hw…, services u32]`; `services` bit `0x20` screen,
  `0x10` history, `0x1000` clock rate (not built, so `0x24` is absent). Config is
  `[0x55, cfg_t]`, 12 bytes with its own offsets and TX power codes. Coefficient reply
  adds mid, vid, I²C address and sensor type; AHT2x gains must stay ≤ 4095. `0x26`
  resets coefficients only in RAM, so the app writes the defaults with `0x25` instead.
  `0x10` answers `[0x10, MAC]`, without ATC's length and random bytes. `0x23` has no
  last-set time.
- `0x25` coefficients: `temp = raw·k_T/65536 + z_T` in 0.01 °C, same for humidity. Write
  composes as `k' = a·k`, `z' = a·z + 100·b`. `0x26` restores factory values.
- `0x10` alone returns the device's MAC: `[0x10, 8, MAC least significant byte first,
  2 random bytes]`. With parameters the same command writes a new MAC to flash (one
  `0x00` byte resets it to the factory MAC): never send it with any.
- `0x23` clock (u32 seconds), `0x24` timer ticks per second (16 000 000 nominal; ±32 767
  adjustable, stored in flash), `0x35` log records `{time u32, temp i16, humi u16, vbat u16}`,
  `0x55` config, `0x22` external LCD data.
- Device clocks drift ~200 ppm and start at 0 after flashing. Never trust a device
  timestamp without the measured offset (`clocks.csv`).
- A connected thermometer stops advertising, so jobs run one at a time and a device is
  released as soon as its job ends.

## Common changes

- **Another device model:** check its pvvx config layout and LCD limits against
  `atc/Encodings.kt`; BTHome decoding is model-independent.
- **°F display:** formatting lives in `ui/`; keep storage and calibration in °C.
- **A change to the calibration fit:** test it on real history too (`RealDataTest`,
  `PrecisionDataTest`, `SessionReplayTest`), and keep `docs/calibration.md` in step.
- **A change to a CSV format:** the archived terminal app reads the same files; say so
  in the change, but don't update the archive unless asked.
