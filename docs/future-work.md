# Future work

Known gaps and loose ends, mostly found while reviewing #3–#6 (macOS backend, THB2
firmware). Each is small; none blocks normal use.

## Android app lacks THB2

THB2 devices (THB1, THB2, BTH01, TH05, …) show readings in the Android app, but every
command fails with "not a pvvx ATC firmware device": it only looks for the ATC command
characteristic `0x1F1F`. A port needs what `crates/atc/src/firmware.rs` lists: the
`0xFFF4` characteristic, the 12-byte THB `cfg_t` and its TX power table, the 19-byte
coefficient reply with sensor info and per-sensor gain limits (AHT2x ≤ 4095), the
`[0x10, MAC]` reply, no `0x24`, and `0x26` followed by a `0x25` write so a reset lasts.
AGENTS.md asks for byte layouts in both apps, with tests on both sides.

## THB2 on Linux is untested on hardware

`bluez.rs` only changed in finding the command characteristic, and CI builds it, but
nobody has run a THB2 device through BlueZ. With one in range, check it read-only first:
`cargo run -p ble --example probe -- <MAC>`.

## macOS: "not a thermometer" is remembered forever

When identifying a UUID fails with `NotPvvx`, `UnsupportedFirmware`, `ShortReply` or
`InvalidValue` (`can_retry` in `crates/ble/src/macos.rs`), the UUID is saved to
`~/Library/Caches/thermo/macos-ids.csv` as another kind of device and never tried again.
A device reflashed to pvvx firmware later, or one that sent a single malformed reply,
stays unidentified until that file is deleted. Options: give `others` entries a
timestamp and retry after a day or so, or treat only `NotPvvx` as final. At least
mention deleting the file in `docs/macos.md`.

## Chart images on macOS: measure once

The kitty chart goes through a temporary file (`t=t`) on macOS, since `/dev/shm` is
missing and `shm_open` would need `unsafe`. It should cost about as much as shared memory
(page cache, deleted by the terminal before it is flushed), but it hasn't been timed. If
large charts stutter, compare render-to-display time against Linux.

## Small cleanups

- `factory_power` in `crates/thermo/src/ui/leveling.rs` names the factory TX power per
  firmware family, but both are +0.0 dBm. It could be a constant until they differ.
