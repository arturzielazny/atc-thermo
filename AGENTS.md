# AGENTS.md

Guidance for AI coding agents (Claude Code, Codex, Cursor, Aider, …) working on this
repository. Humans are welcome too.

## What this is

`thermo` is a terminal dashboard and calibration tool for BLE thermometers running the
[pvvx ATC_MiThermometer](https://github.com/pvvx/ATC_MiThermometer) firmware (tested on
Xiaomi LYWSD03MMC) with BTHome v2 advertising. It listens to adverts, charts them, stores
history as CSV, downloads device logs, syncs clocks, changes settings and writes
calibration coefficients over GATT. Linux only today (BlueZ via the `bluer` crate), behind
a boundary that takes a backend per platform.

## Layout

| Path | What | Platform |
|---|---|---|
| `crates/bthome` | BTHome v2 payload decoder. No dependencies. | portable |
| `crates/calibration` | Consensus calibration: binning, steady-state selection, Huber fit, outliers, anchoring. No dependencies. Algorithm described in `docs/calibration.md` §4. | portable |
| `crates/atc` | The firmware's command protocol on characteristic `0x1F1F`: name, MAC, config, coefficients, clock and clock rate, LCD message, log download. `thermometer.rs` speaks it over the `Transport` trait (`transport.rs`); `clock.rs`, `config.rs`, `coefficients.rs`, `log.rs`, `screen.rs` are byte encodings and maths; `device_id.rs` is `DeviceId` (a MAC, or a CoreBluetooth UUID). No Bluetooth I/O. | portable |
| `crates/ble` | Bluetooth backends behind one API: `Bluetooth::scan` (BTHome adverts → `ScanEvent`), `connect` (→ `Thermometer`), `release`. `bluez.rs` is the Linux backend; each platform's quirks stay inside its backend. | `bluez.rs` uses `bluer` (Linux) |
| `crates/thermo/src/main.rs` | Event loop: keys, scan events, job events, chart frames, 1 s tick. | portable |
| `crates/thermo/src/app.rs` | All state and key handling, no I/O, unit-tested. | portable |
| `crates/thermo/src/jobs.rs` | One-at-a-time device worker: connect, run an `Action`, report an `Outcome`. | portable (uses `ble`) |
| `crates/thermo/src/devices.rs` | Per-device latest readings, history, RSSI median. | portable |
| `crates/thermo/src/store.rs` | `~/.local/share/thermo/`: `YYYY-MM-DD.csv` readings, `calibrations.csv`, `clocks.csv`, `downloads.csv`. | XDG paths |
| `crates/thermo/src/calibrate.rs` | Glue from history to `calibration`, and from fits to coefficients. | portable |
| `crates/thermo/src/level.rs` | Signal levelling: per-device median RSSI and the TX power change towards a common target. | portable |
| `crates/thermo/src/ui/` | ratatui widgets. `chart.rs` axes and layout, `plot.rs` tiny-skia rasterizer, `kitty.rs` kitty graphics protocol via `/dev/shm`, braille fallback in `chart.rs`. | `kitty.rs` assumes `/dev/shm` |
| `android/` | Android app (Kotlin, Compose): ports of `bthome`, `calibration`, the `atc` encodings and `app.rs`, plus an Android GATT client and scanner. See `android/README.md`. | Android |
| `scripts/` | The original Python prototypes (`dbus-python`). Not used by the app. | Linux |
| `site/` | GitHub Pages site, deployed by `.github/workflows/pages.yml`. Static HTML, no build step. | — |

## Commands

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings   # pedantic lints are on (Cargo.toml)
cargo test --workspace                                   # no hardware needed
cargo run --release -p thermo                            # needs Bluetooth
cargo run --release -p thermo -- calibrate 6             # offline: fit the last 6 h of saved history
cargo run -p ble --example probe -- A4:C1:38:xx:xx:xx    # read-only dump of one device
```

Android (`android/`, not part of the Cargo workspace):

```bash
cd android && ./gradlew testDebugUnitTest      # JVM tests, no phone needed
cd android && ./gradlew installDebug           # needs a phone with USB debugging
```

Build dependencies: Rust ≥ 1.90 (`rust-version` in `Cargo.toml`, checked in CI),
`pkg-config` and the libdbus headers (`libdbus-1-dev` on Debian/Ubuntu, `dbus-devel` on
Fedora, `dbus` on Arch).

## Rules

- **Never write to real devices unless the user asks.** Calibration, settings, rename,
  clock sync, rate trim and factory reset all change device flash. Prefer unit tests and
  `thermo calibrate` (offline). Reading adverts is harmless.
- **Don't touch the user's real history** when experimenting. Run with a scratch data dir:
  `env -u XDG_DATA_HOME HOME=/tmp/thermo-test thermo` (the status line then shows `~/.local/share/thermo`
  relative to that home). A second instance on the same machine is fine for listening,
  but it will try to download logs from devices whose history has gaps.
- `unsafe_code = "forbid"` workspace-wide. Keep it.
- Keep `bthome` and `calibration` free of dependencies and I/O, so they stay portable
  and testable. `atc` does no Bluetooth I/O either: only `ble` talks to a BLE stack.
- Platform code lives in one `ble` backend file. The app and `jobs.rs` must not ask
  which platform they run on; if a backend needs a decision from them, add it to the
  `ble` API for every backend.
- `app.rs` must stay I/O-free: it receives events and enqueues `jobs::Action`s.
  Add tests there for new keys and outcomes (see the existing `#[cfg(test)]` module).
- Match the surrounding style: small modules with a `//!` doc line, doc comments on
  items, named constants with a comment saying where the value comes from, `anyhow` in
  `thermo`, typed errors in the library crates.
- The Android app ports the Rust logic rather than calling it. A change to the
  calibration algorithm, a byte layout or a CSV format must be made in both, with tests
  on both sides; `android/.../RealDataTest.kt` compares a fit with the Rust crate on real data.

## Firmware facts worth knowing

- Command/notify characteristic `0x1F1F`; first byte is the command. Replies echo it.
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
- A connected thermometer stops advertising; BlueZ pauses discovery during connects.
  The BlueZ backend restarts discovery after jobs finish (`ScanControl::Resume`) and
  whenever BlueZ ends it by itself.

## Porting to another OS

The protocol (`atc`), the app and the job worker are platform-independent; a port adds
one backend to `crates/ble` and keeps the tests passing.

1. **Backend.** Add `crates/ble/src/<platform>.rs` behind a `cfg` in `ble/src/lib.rs`,
   with the same API as `bluez.rs`: a `Bluetooth` handle (`Default`, `Clone`) with
   `scan`, `connect` and `release`, and a `Link` implementing `atc::Transport` (write to
   characteristic `0x1F1F`, disconnect). Scan for service data under UUID `0xFCD2` and
   keep RSSI. [`btleplug`](https://crates.io/crates/btleplug) covers CoreBluetooth and
   WinRT.
2. **Device ids.** History, `calibrations.csv` and the Android app key devices by MAC,
   so every backend reports MACs whenever it can. CoreBluetooth hides them and hands out
   per-host UUIDs instead; the macOS backend maps each UUID back to a MAC by connecting
   once and reading `Thermometer::mac` (command `0x10`, read-only), then:
   - keeps the UUID → MAC map in a cache file of its own (e.g.
     `$HOME/Library/Caches/thermo/macos-ids.csv`); losing it only costs a re-read;
   - sends adverts from mapped UUIDs as `DeviceId::Mac`, and `connect(DeviceId::Mac)`
     looks the UUID up in the map;
   - holds back adverts from an unmapped UUID until its first identification attempt
     ends; if that fails (weak signal), sends them as `DeviceId::Uuid` so the readings
     still show, and retries now and then;
   - identifies only while scanning is not paused, one device at a time, and shares a
     lock with `connect` so a job never connects alongside an identification.

   All of this stays inside the backend: the app and `jobs.rs` see the same ids as on
   Linux. A device in range of the Mac but never identified keeps its `DeviceId::Uuid`.
3. **Chart images.** `kitty.rs` passes images through `/dev/shm`, which is Linux-only.
   On macOS use direct transmission (`t=d`, base64 chunks) or a temp file (`t=t`).
   Windows terminals generally lack the protocol, so the braille fallback is used.
4. **Paths.** `Store::default_dir` uses XDG under `HOME`. On Windows use
   `%LOCALAPPDATA%` (e.g. via the `dirs` crate).
5. **CI.** Add the platform's runner to `.github/workflows/ci.yml` (fmt, clippy,
   test); tests must not need Bluetooth.
6. **Real devices.** `cargo run -p ble --example probe -- <id>` only reads, and must
   work unchanged on the new backend (on macOS it also shows the identified MAC). Don't
   test with commands that write (see Rules).

## Other common changes

- **Headless exporter** (MQTT, InfluxDB, Prometheus): reuse `ble::Bluetooth::scan` +
  `bthome` and skip the TUI; add a binary under `crates/` rather than flags on `thermo`.
- **Another device model:** check its pvvx config layout (`atc/src/config.rs`) and LCD
  limits (`atc/src/screen.rs`); BTHome decoding is model-independent.
- **°F display:** formatting lives in `ui/table.rs`, `ui/chart.rs` and
  `ui/calibration.rs`; keep storage in °C.
