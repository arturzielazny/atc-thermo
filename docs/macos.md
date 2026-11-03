# thermo on macOS

Bluetooth goes through CoreBluetooth via [`btleplug`](https://crates.io/crates/btleplug),
so no D-Bus or Homebrew packages are needed. Tested on Apple silicon; Intel Macs build
the same code.

## 1. Install Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version        # 1.90 or newer
```

If `cc` is missing, run `xcode-select --install` once (Command Line Tools).

## 2. Build and install

```sh
cargo install --git https://github.com/arturzielazny/atc-thermo thermo
```

This puts `thermo` in `~/.cargo/bin`.

## 3. Allow Bluetooth for your terminal

macOS asks the first time `thermo` scans. If you missed the prompt, or the status line
says Bluetooth is not available: **System Settings › Privacy & Security › Bluetooth**,
turn on your terminal app (Terminal, iTerm2, Ghostty, kitty…), then quit and reopen
it. Each terminal app needs its own permission. Bluetooth itself must be on.

## 4. Run

```sh
thermo                 # dashboard
thermo calibrate 6     # print a proposal for the last 6 h of saved history; no device writes
```

History lives in `~/.local/share/thermo/` (or `$XDG_DATA_HOME/thermo`), as on Linux.

In Ghostty or kitty the chart is a pixel image, handed over as a temporary file.
Terminal.app and iTerm2 get the braille chart.

## Device ids

CoreBluetooth hides MAC addresses and names each device by a UUID that differs from
Mac to Mac. So that history and `calibrations.csv` match those from Linux and Android,
`thermo` connects once to each new thermometer, reads its MAC (a read-only command)
and shows the MAC from then on. The UUID → MAC pairs are kept in
`~/Library/Caches/thermo/macos-ids.csv`; deleting that file only means each device is
asked again.

A new device's readings appear once it has been identified, usually within a minute.
A device that can't be reached keeps its UUID until a later attempt works (after 1, 5,
then every 30 minutes). Devices whose firmware `thermo` can't talk to keep their UUID.

## How connections work

macOS keeps a connect request pending until the device's next connectable advert, so
scanning goes on while `thermo` talks to a device. A connection waits for the device to
have been heard in the last minute, then gives each of two requests 30 s. Weak devices
can take a while; moving them closer or raising their TX power helps.

## Caveats

- **Other apps keep devices busy.** If Home Assistant, a Bluetooth proxy or a phone app
  is connected to a thermometer, `thermo` can't connect to it. Passive listening is
  fine.
- **A connected thermometer stops advertising.** Its live readings pause while
  `thermo` (or another app) is connected, and resume after.
