# AGENTS.md

Guidance for AI coding agents (Claude Code, Codex, Cursor, Aider, …) working on this
repository. Humans are welcome too.

## What this is

`thermo` is a terminal dashboard and calibration tool for BLE thermometers running the
[pvvx ATC_MiThermometer](https://github.com/pvvx/ATC_MiThermometer) firmware (tested on
Xiaomi LYWSD03MMC) or the [pvvx THB2](https://github.com/pvvx/THB2) firmware (THB1 etc.)
with BTHome v2 advertising. It listens to adverts, charts them, stores
history as CSV, downloads device logs, syncs clocks, changes settings and writes
calibration coefficients over GATT. Linux and macOS via the `btleplug` crate (BlueZ,
CoreBluetooth; WinRT untested).

## Layout

| Path | What | Platform |
|---|---|---|
| `crates/bthome` | BTHome v2 payload decoder. No dependencies. | portable |
| `crates/calibration` | Consensus calibration: binning, steady-state selection, Huber fit, outliers, anchoring. No dependencies. Algorithm described in `docs/calibration.md` §4. | portable |
| `crates/atc` | Async client for the firmware's command characteristic (`0x1F1F` on ATC, `0xFFF4` on THB2): name, config, coefficients, clock and clock rate, LCD message, log download. `firmware.rs` detects the family from the characteristic and the dev id reply and says which features exist; `device_id.rs` is `DeviceId` (MAC or CoreBluetooth UUID). `thermometer.rs` holds the transport; `clock.rs`, `config.rs`, `coefficients.rs`, `log.rs`, `screen.rs` are byte encodings and maths with no I/O. | `thermometer.rs` uses `btleplug` |
| `crates/thermo/src/main.rs` | Event loop: keys, scan events, job events, chart frames, 1 s tick. | portable |
| `crates/thermo/src/app.rs` | All state and key handling, no I/O, unit-tested. | portable (uses `atc::DeviceId`) |
| `crates/thermo/src/scanner.rs` | btleplug discovery of `0xFCD2` service data → `ScanEvent::Advert`. `ble.rs` caches the one shared adapter (CoreBluetooth makes a new central per `Manager::adapters()` call). | portable |
| `crates/thermo/src/jobs.rs` | One-at-a-time device worker: connect, run an `Action`, report an `Outcome`. | uses `atc` |
| `crates/thermo/src/devices.rs` | Per-device latest readings, history, RSSI median. | portable |
| `crates/thermo/src/store.rs` | `~/.local/share/thermo/` (also on macOS; `XDG_DATA_HOME` wins): `YYYY-MM-DD.csv` readings, `calibrations.csv`, `clocks.csv`, `downloads.csv`. | XDG paths |
| `crates/thermo/src/calibrate.rs` | Glue from history to `calibration`, and from fits to coefficients. | portable |
| `crates/thermo/src/level.rs` | Signal levelling: per-device median RSSI and the TX power change towards a common target. | portable |
| `crates/thermo/src/ui/` | ratatui widgets. `chart.rs` axes and layout, `plot.rs` tiny-skia rasterizer, `kitty.rs` kitty graphics protocol via `/dev/shm` (`t=s`) or, where that is missing (macOS), a temp file (`t=t`), braille fallback in `chart.rs`. | portable |
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
cargo run -p atc --example probe -- A4:C1:38:xx:xx:xx    # read-only dump of one device
```

Android (`android/`, not part of the Cargo workspace):

```bash
cd android && ./gradlew testDebugUnitTest      # JVM tests, no phone needed
cd android && ./gradlew installDebug           # needs a phone with USB debugging
```

Build dependencies: current stable Rust; on Linux also `pkg-config` and the libdbus headers (`libdbus-1-dev`
on Debian/Ubuntu, `dbus-devel` on Fedora, `dbus` on Arch).

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
  and testable.
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
- `0x23` clock (u32 seconds), `0x24` timer ticks per second (16 000 000 nominal; ±32 767
  adjustable, stored in flash), `0x35` log records `{time u32, temp i16, humi u16, vbat u16}`,
  `0x55` config, `0x22` external LCD data.
- Device clocks drift ~200 ppm and start at 0 after flashing. Never trust a device
  timestamp without the measured offset (`clocks.csv`).
- A connected thermometer stops advertising; BlueZ pauses discovery during connects.
  The scanner restarts discovery after jobs finish.
- THB2 firmware (pvvx/THB2): command characteristic `0xFFF4`, write without response.
  Dev id reply `[0x00, ver, device, hw…, services u32]`; `services` bit `0x20` screen,
  `0x10` history, `0x1000` clock rate (not built, so `0x24` is absent). Config is 12
  bytes with its own offsets and TX power codes (`config.rs`). Coefficient reply adds
  mid, vid, I²C address and sensor type; AHT2x gains must stay ≤ 4095. `0x26` resets
  coefficients only in RAM, so `atc` writes the defaults with `0x25` instead.
- On macOS `DeviceId` is a per-host UUID, not the MAC. CSV rows keyed by a MAC (from
  Linux) load as separate devices.

## Porting to another OS

Done for macOS: `DeviceId` (MAC or UUID), a btleplug scanner and GATT transport, the
kitty temp-file medium, and the XDG data dir on every OS. For Windows, check that
btleplug's WinRT backend reports service data and use `%LOCALAPPDATA%` in
`Store::default_dir`; terminals there generally lack the kitty protocol, so the braille
fallback is used.

## Other common changes

- **Headless exporter** (MQTT, InfluxDB, Prometheus): reuse `scanner.rs` + `bthome` and
  skip the TUI; add a binary under `crates/` rather than flags on `thermo`.
- **Another device model:** check its pvvx config layout (`atc/src/config.rs`) and LCD
  limits (`atc/src/screen.rs`); BTHome decoding is model-independent.
- **°F display:** formatting lives in `ui/table.rs`, `ui/chart.rs` and
  `ui/calibration.rs`; keep storage in °C.
