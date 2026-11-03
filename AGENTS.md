# AGENTS.md

Guidance for AI coding agents (Claude Code, Codex, Cursor, Aider, …) working on this
repository. Humans are welcome too.

## What this is

`thermo` is a terminal dashboard and calibration tool for BLE thermometers running the
[pvvx ATC_MiThermometer](https://github.com/pvvx/ATC_MiThermometer) firmware (tested on
Xiaomi LYWSD03MMC) with BTHome v2 advertising. It listens to adverts, charts them, stores
history as CSV, downloads device logs, syncs clocks, changes settings and writes
calibration coefficients over GATT. Linux only today (BlueZ via the `bluer` crate).

## Layout

| Path | What | Platform |
|---|---|---|
| `crates/bthome` | BTHome v2 payload decoder. No dependencies. | portable |
| `crates/calibration` | Consensus calibration: binning, steady-state selection, Huber fit, outliers, anchoring. No dependencies. Algorithm described in `docs/calibration.md` §4. | portable |
| `crates/atc` | Async client for the firmware's command characteristic `0x1F1F`: name, config, coefficients, clock and clock rate, LCD message, log download. `thermometer.rs` holds the transport; `clock.rs`, `config.rs`, `coefficients.rs`, `log.rs`, `screen.rs` are byte encodings and maths with no I/O. | `thermometer.rs` uses `bluer` (Linux) |
| `crates/thermo/src/main.rs` | Event loop: keys, scan events, job events, chart frames, 1 s tick. | portable |
| `crates/thermo/src/app.rs` | All state and key handling, no I/O, unit-tested. | portable (uses `bluer::Address`) |
| `crates/thermo/src/scanner.rs` | BlueZ discovery → `ScanEvent::Advert`. | Linux |
| `crates/thermo/src/jobs.rs` | One-at-a-time device worker: connect, run an `Action`, report an `Outcome`. | uses `atc` |
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
cargo run -p atc --example probe -- A4:C1:38:xx:xx:xx    # read-only dump of one device
```

Android (`android/`, not part of the Cargo workspace):

```bash
cd android && ./gradlew testDebugUnitTest      # JVM tests, no phone needed
cd android && ./gradlew installDebug           # needs a phone with USB debugging
```

Build dependencies: Rust ≥ 1.88 (`rust-version` in `Cargo.toml`, checked in CI),
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

## Porting to another OS

The code splits cleanly; port in this order and keep the tests passing at each step.

1. **Own the address type.** `bluer::Address` is used throughout `thermo` as a map key.
   Introduce `DeviceId` (a newtype) and convert at the edges. On macOS CoreBluetooth hides
   MAC addresses and gives per-host UUIDs, so `DeviceId` must not assume 6 bytes, and
   stored CSV keys need a migration or a name-based fallback.
2. **Scanner.** Reimplement `scanner.rs` with
   [`btleplug`](https://crates.io/crates/btleplug) (CoreBluetooth, WinRT, BlueZ): listen
   for service data under UUID `0xFCD2`, pass the bytes to `bthome::parse`, emit the
   same `ScanEvent`s. Keep RSSI.
3. **GATT client.** Put a small trait or a cfg-switched module behind
   `atc::Thermometer`'s transport: connect, find characteristic `0x1F1F`, write, and
   read notifications with a timeout. Everything above that (`config.rs`, `clock.rs`, …)
   is bytes and stays unchanged.
4. **Chart images.** `kitty.rs` passes images through `/dev/shm`, which is Linux-only.
   On macOS use direct transmission (`t=d`, base64 chunks) or a temp file (`t=t`).
   Windows terminals generally lack the protocol, so the braille fallback is used.
5. **Paths.** `Store::default_dir` uses XDG under `HOME`. On Windows use
   `%LOCALAPPDATA%` (e.g. via the `dirs` crate).

## Other common changes

- **Headless exporter** (MQTT, InfluxDB, Prometheus): reuse `scanner.rs` + `bthome` and
  skip the TUI; add a binary under `crates/` rather than flags on `thermo`.
- **Another device model:** check its pvvx config layout (`atc/src/config.rs`) and LCD
  limits (`atc/src/screen.rs`); BTHome decoding is model-independent.
- **°F display:** formatting lives in `ui/table.rs`, `ui/chart.rs` and
  `ui/calibration.rs`; keep storage in °C.
