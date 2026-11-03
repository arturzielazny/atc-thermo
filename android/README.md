# Thermo for Android

The `thermo` dashboard and calibration tool as an Android app, for the same BLE
thermometers: pvvx [ATC_MiThermometer](https://github.com/pvvx/ATC_MiThermometer)
firmware with BTHome v2 advertising (tested on Xiaomi LYWSD03MMC, firmware v5.9, on a
Samsung Galaxy S21 with Android 15). Kotlin and Jetpack Compose (Material 3), no other
dependencies. Android 8 and newer.

## What it does

Everything the terminal app does, laid out for touch:

| Terminal | Android |
|---|---|
| chart above the device table | chart card above one card per device; tap the chart and drag for a crosshair with every device's value |
| `t` `h` `r`, `w`, `p` | segmented buttons Temp / Humidity / Signal, span chips 10 min … 48 h, lines/points button |
| `↑` `↓`, `space`, `a`, `Esc` | tap a card for its detail screen; long-press to select, tap to add more, ✕ or Back to clear |
| `i` `s` `d` `e` `n` | Identify, Sync clock, Download log, Settings, Rename: in the selection bar or on the device screen |
| `e` settings form | bottom sheet with a drop-down per field, “Keep” by default; a single device's current values are read and shown |
| `c` calibration, `Tab` `←` `→` | Calibration screen: device chips, a range slider over the shaded chart (snapped to 15 min), summary, corrections with bars; Write asks first; menu: restore factory calibration, sync clocks |
| `l` levelling, `←` `→`, `Enter`, `x`, `r` | Signal levelling screen: target slider with ±1 dB buttons, a checkbox per device, Change asks first; menu: factory TX power; ⟳ listens anew |
| status line | subtitle (“Listening · 2 of 2 in range · connecting”), snackbars for problems, a spinner and the last result on each card |

It also downloads a device's log the first time it is heard if recent history has a
gap, keeps recording with the app closed (a foreground service with a notification and a
Stop action), restarts a stuck scan, and follows Bluetooth being switched off and on.

## History files

`Android/data/dev.artur.thermo/files/thermo/` on the phone's shared storage, in exactly the
desktop app's format (`YYYY-MM-DD.csv`, `calibrations.csv`, `clocks.csv`, `downloads.csv`).
**Data and recording** (main menu) shares them, and imports files from
`~/.local/share/thermo`: lines already present are skipped. Over USB:

```sh
adb pull /sdcard/Android/data/dev.artur.thermo/files/thermo/ phone-thermo/
```

## Build

Android SDK (platform 36) and JDK 17 or newer, e.g. Android Studio's.

```sh
cd android
./gradlew testDebugUnitTest          # the ported unit tests, no phone needed
./gradlew installDebug               # build and install on a connected phone
./gradlew assembleRelease            # minified, signed with the debug key for sideloading
```

`local.properties` holds `sdk.dir` (Android Studio writes it). If Gradle picks a JDK
that is too new, run it with `JAVA_HOME` pointing at JDK 21.

## Code

| Path | What |
|---|---|
| `bthome/` | BTHome v2 decoder, port of `crates/bthome` |
| `calibration/` | consensus calibration, port of `crates/calibration` |
| `atc/Encodings.kt` | config, TX power, coefficients, clock, log and LCD layouts, port of `crates/atc` |
| `atc/Thermometer.kt` | GATT client for characteristic `0x1F1F` on `BluetoothGatt`, with coroutines |
| `model/` | devices, CSV store, levelling, calibration glue and `ThermoModel` (the I/O-free `app.rs`) |
| `ble/` | scanner (filtered on service data `0xFCD2`) and the one-at-a-time job worker |
| `Controller.kt`, `MonitorService.kt` | process-wide wiring and the recording service |
| `ui/` | Compose screens; `Chart.kt` draws with per-pixel min/max reduction |

The JVM unit tests port the Rust tests with the same fixtures. `RealDataTest` fits a
window of real history when `THERMO_DATA_DIR`, `THERMO_FROM` and `THERMO_TO` are set; on
the 2026-10-28/29 eight-device session it matches the Rust crate to four decimals.

Debug builds only: `adb shell am start -n dev.artur.thermo/.MainActivity --ez demo true`
adds five simulated devices with a 14 h session (nothing is saved), and `--es screen
calibration|leveling|data|<address>` opens a screen. Debug builds also show over the lock
screen, so they can be driven over adb on a locked phone.
