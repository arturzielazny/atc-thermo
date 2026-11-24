# Thermo for Android

The `thermo` dashboard and calibration tool as an Android app, for the same BLE
thermometers: pvvx [ATC_MiThermometer](https://github.com/pvvx/ATC_MiThermometer)
firmware with BTHome v2 advertising (tested on Xiaomi LYWSD03MMC, firmware v5.9, on a
Samsung Galaxy S21 with Android 15), and pvvx [THB2](https://github.com/pvvx/THB2)
firmware (THB1, THB2, BTH01, TH05, …; ported from the terminal app, not yet tried on a
phone). Kotlin and Jetpack Compose (Material 3), no other dependencies. Android 8 and
newer.

**Install:** download the APK from the [Android release](https://github.com/arturzielazny/atc-thermo/releases/tag/android-v0.3.0) and open it on
the phone (allow installing from your browser or file manager when asked).

## What it does

Everything the terminal app does, laid out for touch:

| Terminal | Android |
|---|---|
| chart above the device table | chart card above one card per device; tap the chart and drag for a crosshair with every device's value |
| `t` `h` `r`, `w`, `p` | segmented buttons Temp / Humidity / Signal, span chips 10 min … 48 h, lines/points button |
| `o` | menu: Order by signal (strongest now first, numbered from 1; devices heard later go last) / Order by address |
| `↑` `↓`, `space`, `a`, `Esc` | tap a card for its detail screen; long-press to select, tap to add more, ✕ or Back to clear |
| `i` `s` `d` `e` `n` | Identify, Sync clock, Download log, Settings, Rename: in the selection bar or on the device screen |
| `e` settings form | bottom sheet with a drop-down per field, “Keep” by default; a single device's current values are read and shown |
| `c` calibration, `Tab` `←` `→`, `r` | a guided calibration of the devices you pick, see [Calibration](#calibration) |
| `l` levelling, `←` `→`, `Enter`, `x`, `r` | Signal levelling screen: target slider with ±1 dB buttons, a checkbox per device, Change asks first; menu: factory TX power; ⟳ listens anew |
| status line, Job column | subtitle (“Listening · 2 of 2 in range · connecting”), snackbars for problems, a spinner and the last result with its age on each card (greyed after 10 min) |

It also downloads a device's log the first time it is heard if recent history has a
gap, keeps recording with the app closed (a foreground service with a notification; it
starts again after a reboot or an update and is switched off on the Data screen), restarts
a stuck scan, and follows Bluetooth being switched off and on.

## Calibration

The app is built around calibrating a set of thermometers. The card at the top of the
home screen starts one, and while it runs the app opens on it.

1. **Pick** the thermometers placed side by side, nearest (strongest signal) first; the eye
   shows a device's number on its screen. Choose what they are corrected to (their
   average, or one you trust). If they have read alike for a while already, those
   readings count from the start.
2. **Together.** The chart shows only these devices, with a strip marking the steady
   readings the fit uses. **If you write now** says how precise the corrections would be
   (± two standard errors, °C and %RH), and **Precision or time** how long each level
   takes (Rough ±0.10 °C … Best ±0.02 °C). The level you pick is announced by a
   notification, also with the app closed. Then: write the corrections, or move the devices
   for the gains; the app says where (at least 5 °C warmer or colder than now, and for the
   humidity gain 15 %RH apart).
3. **Second place** (optional): how far it is from the first, which devices are still
   settling there and roughly how much longer, steady time there, and the fitted gains
   with their precision.
4. **Written:** writes that fail can be retried; keeping the devices together shows what
   is left to correct. **Finish** ends it.

The fit is the terminal app's (`crates/calibration`), with 20 minutes of steady data and a
20-minute steadiness window instead of 60 and 30, so a first result comes after about
40 minutes and the precision estimate, rather than a fixed minimum, says if it is enough.
A gain's span leaves out 15 minutes of steady readings at each end rather than 5 %
(`Options.spanTrim`), so a second place counts after half an hour however long the first
lasted; with 5 %, a day in the first place needed more than an hour in the second.

The estimate is Android-only: each correction's standard error from the residuals,
inflated for their correlation over 30 minutes (pooled over devices), times 1.5, plus a
floor that more time in one place doesn't remove (0.009 °C, 0.02 %RH). Both were fitted
with `PrecisionDataTest` on two indoor sessions (8 and 6 devices, 10 and 18 h): windows of
45 min to 3 h compared with the rest of their session at a similar temperature fell within
the stated ± 96–99 % of the time. At other temperatures they agree less (88–94 %), which is
what the second place's gains are for.

The time to each level assumes the statistical error keeps shrinking with the square root
of the steady time. Replaying the same two sessions minute by minute (`SessionReplayTest`),
every time under 2 h came true, mostly within ±50 %; longer ones all came true in one
session and 1 of 140 in the other, where the room didn't stay as steady. So beyond 2 h the
app says only "over 2 h, if at all".

Also Android-only: after a move each device is held back until it has caught up with the
others (`Options.holdUntilSettled`), on top of the hour after a big change. After a change
of 1 °C or 5 %RH within 30 minutes, each device's offset to the group median is fitted as
an exponential decay, and its readings count once less than 0.03 °C / 0.15 %RH of it is
left. On the 2026-10-29 move (8 devices, 21 → 16 °C, 60 → 72 %RH) temperatures had settled
within the hour, but humidity offsets in the second place were still 0.31 %RH RMS off (0.8
at worst) after it, against 0.07 with the hold: some sensors' humidity creeps for hours,
one was still 0.5 %RH behind after 3 h. The two indoor sessions, without a move, lost no
readings to it. `SettlingDataTest` replays a move from real history.

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
./gradlew assembleRelease            # minified; see Releases for the signing key
```

`local.properties` holds `sdk.dir` (Android Studio writes it). If Gradle picks a JDK
that is too new, run it with `JAVA_HOME` pointing at JDK 21.

## Releases

Published APKs are signed with a release key kept outside the repository. Gradle uses it
when `android/keystore.properties` exists (gitignored):

```properties
storeFile=/home/you/.android/thermo-release.jks
storePassword=…
keyAlias=thermo
keyPassword=…
```

Without that file the release build is signed with the debug key. Android only updates
an app with an APK signed by the same key, so keep the keystore and its password backed
up: losing them means users must uninstall (and lose the app's history) to update.

To publish: raise `versionCode` and `versionName` in `app/build.gradle.kts`, then

```sh
./gradlew assembleRelease
cp app/build/outputs/apk/release/app-release.apk thermo-android-X.Y.Z.apk
gh release create android-vX.Y.Z thermo-android-X.Y.Z.apk --title "Android X.Y.Z"
```

## Code

| Path | What |
|---|---|
| `bthome/` | BTHome v2 decoder, port of `crates/bthome` |
| `calibration/` | consensus calibration, port of `crates/calibration` |
| `atc/Firmware.kt` | ATC or THB2, told apart by the command characteristic, and which features a device has; port of `crates/atc/src/firmware.rs` |
| `atc/Encodings.kt` | config, TX power, coefficients, clock, log and LCD layouts of both families, port of `crates/atc` |
| `atc/Thermometer.kt` | GATT client for characteristic `0x1F1F` (ATC) or `0xFFF4` (THB2) on `BluetoothGatt`, with coroutines |
| `model/` | devices, CSV store, levelling, calibration glue and `ThermoModel` (the I/O-free `app.rs`) |
| `ble/` | scanner (filtered on service data `0xFCD2`) and the one-at-a-time job worker |
| `Controller.kt`, `MonitorService.kt` | process-wide wiring and the recording service |
| `ui/` | Compose screens; `Chart.kt` draws with per-pixel min/max reduction |

The JVM unit tests port the Rust tests with the same fixtures. `RealDataTest` fits a
window of real history when `THERMO_DATA_DIR`, `THERMO_FROM` and `THERMO_TO` are set; on
the 2026-10-28/29 eight-device session it matches the Rust crate to four decimals.
`PrecisionDataTest` checks the precision estimate against real sessions when
`THERMO_DATA_DIR` and `THERMO_SESSIONS` are set, `SettlingDataTest` the hold after a move
with `THERMO_MOVE`, and `SessionReplayTest` replays a whole session as the app would have
shown it, with `THERMO_REPLAY` (see the files).

Debug builds only: `adb shell am start -n dev.artur.thermo/.MainActivity --ez demo true`
adds five simulated devices with 14 h of history, the last 2 h after a move (nothing is
saved; `--ez demo_session true` also starts a calibration of them), and `--es screen
calibration|leveling|data|<address>` opens a screen. Debug builds also show over the lock
screen, so they can be driven over adb on a locked phone.
