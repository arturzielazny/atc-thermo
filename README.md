# mi-thermo-flash

Tools for Xiaomi LYWSD03MMC thermometers running the
[pvvx ATC_MiThermometer](https://github.com/pvvx/ATC_MiThermometer) firmware with
BTHome v2 advertising.

## `thermo`: dashboard

```
cargo run --release -p thermo            # or ./target/release/thermo
cargo run --release -p thermo -- calibrate 6   # print a calibration proposal for the last 6 h
```

A chart of every thermometer above a table of current readings. In terminals with the
kitty graphics protocol (Ghostty, kitty) the chart is an image passed through shared
memory (`/dev/shm`); over SSH, inside tmux or elsewhere it is drawn with braille. History is saved
to `~/.local/share/thermo/YYYY-MM-DD.csv` and reloaded on start; gaps in the last
24 h are filled from each device's own log.

| Key | Action |
|---|---|
| `↑` `↓` | select a device · `Esc` clears marks, then the selection |
| `space` / `a` | mark one / all devices: actions apply to marked devices, or the selected one |
| `i` | identify: show the device's `#` from the table on its screen for 10 min (`333` for #3, with a smiley) |
| `n` | rename the selected device |
| `s` | sync device clocks with this computer and trim the drift measured since the last sync |
| `e` | settings: advertising interval, measurement interval, TX power, log interval |
| `d` | download the device log for the last 24 h |
| `c` | calibration screen (see [docs/calibration.md](docs/calibration.md)) |
| `t` `h` `r` `Tab` · `w` · `p` | temperature / humidity / signal strength · chart time span · lines or points |
| `q` | quit |

## Crates

- `crates/bthome`: dependency-free BTHome v2 payload decoder.
- `crates/atc`: async BLE client for the pvvx firmware's command interface (config,
  name, clock and its rate, sensor coefficients, log download).
- `crates/calibration`: consensus calibration of co-located sensors.
- `crates/thermo`: the dashboard.

`bthome_listen.py` and `atc_config.py` are the original Python helpers.
