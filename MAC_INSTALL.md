# thermo on macOS

Tested to build for `aarch64-apple-darwin` (Apple silicon, e.g. M4). Bluetooth goes
through CoreBluetooth via `btleplug`; no D-Bus or Homebrew packages are needed.

## 1. Install Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version        # any current stable
```

If `cc` is missing, run `xcode-select --install` once (Command Line Tools).

## 2. Build and install

From the unpacked source folder:

```sh
cd atc-thermo-macos
cargo install --path crates/thermo
```

This puts `thermo` in `~/.cargo/bin`.

## 3. Allow Bluetooth for your terminal

macOS asks the first time `thermo` scans. If you missed the prompt, or the scanner
says Bluetooth is off or not allowed: **System Settings > Privacy & Security >
Bluetooth**, turn on your terminal app (Terminal, iTerm2, Ghostty, kitty…), then quit
and reopen the terminal. Bluetooth itself must be on.

## 4. Run

```sh
thermo                 # dashboard
thermo calibrate 6     # print a proposal for the last 6 h of saved history; no device writes
```

History lives in `~/.local/share/thermo/` (or `$XDG_DATA_HOME/thermo`), the same place
as on Linux.

In Ghostty or kitty the chart is a pixel image (handed over as a temporary file).
Terminal.app and iTerm2 get the braille chart.

If the screen ever shows leftover characters, press `Ctrl-L` to repaint it.

## Debug log

Every run writes `~/.local/share/thermo/thermo.log` (the previous run's is kept as
`thermo.log.1`): each device job with its steps (connect, service discovery,
subscribe, commands), how long they took and why they failed. `thermo` prints the
path when it quits. For more detail, including the Bluetooth library's own messages:

```sh
THERMO_LOG=debug thermo        # or trace for every command byte; off to disable
THERMO_LOG_FILE=/tmp/thermo.log thermo
```

Send that file along with any problem report.

## How device jobs run on macOS

Jobs (log download, identify, sync, settings, calibration) run one device at a time.
Scanning keeps running during jobs. A job starts once its device was heard in the last
60 s (120 s for ATC devices). macOS then keeps one connect request pending for up to
30 s; it goes out on the device's next connectable advert (every 5 s on THB1, 2.5 s
on LYWSD03MMC with default settings), so weak devices can take a while. A device
whose connection fails is put back in the queue (retried after 10 s, then 30 s,
three tries in all) while the other devices go ahead; the table shows
`… queued · <reason>; retry in N s`. A device silent for its whole window counts as
a failed try ("out of range, or connected to another app"). Set
`THERMO_ADVERT_WINDOW=<seconds>` to change the 60 s window (ATC gets twice that).

If a THB1 shows `connecting timed out …; press the device button to connect faster`,
press its button briefly: the THB2 firmware then advertises every 1.5 s for 60 s.
Moving it closer or raising its TX power may help too (in the first real-hardware
log, connects timed out at about −82 and −66 dBm).

## Calibrating in short

1. Put all the thermometers side by side for at least 2 h, away from heat sources.
2. Move them all at once somewhere at least 5 °C different, for at least 3 h.
3. Press `c`. Set the window to cover both steady phases but not the move. Check the
   proposal, press `Enter` and confirm with `y`.

Agreement is not absolute accuracy. For absolute humidity, add a salt test (75.3 %RH
over saturated NaCl). LYWSD03MMC (ATC firmware) and THB1 (THB2 firmware) can be in the
same group; each device gets its own firmware's command set.

## Caveats

- **The ID is not the MAC.** CoreBluetooth hides MAC addresses. Each device shows as a
  UUID that this Mac made up (the table shows its first 16 characters). It differs
  from the MAC, from the ID on another Mac, and from history recorded on Linux:
  copied Linux CSVs load, but as separate devices. Tell devices apart by name
  (`ATC_27C622`, `THB1-26705B`) or with `i` (identify).
- **Home Assistant keeps the devices busy.** If Home Assistant's BTHome integration (or
  a Bluetooth proxy) connects to a THB1, or anything else holds a connection, `thermo`
  can't connect to it. Passive listening is fine; close other connections before
  settings, sync, download or calibration.
- **A connected thermometer stops advertising.** While `thermo` (or another app) is
  connected, its live readings pause. They resume after the job ends.
- **THB1 limits.** Clock-rate trim is not in the THB2 firmware, so sync only sets the
  time. THB2 firmware older than 1.4 is refused. TX power steps differ from ATC
  (−20 to +5 dBm); `thermo` picks the nearest step.
- Not yet run on real macOS hardware: it compiles for `aarch64-apple-darwin`, but
  scanning and GATT on a real Mac are untested. Report problems with the exact error text.
