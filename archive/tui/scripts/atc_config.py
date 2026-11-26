#!/usr/bin/env python3
"""Read or change pvvx ATC_MiThermometer config over BLE (BlueZ D-Bus).

Usage:
  atc_config.py MAC                          show device info and config
  atc_config.py MAC set key=value [...]      change config fields
  atc_config.py MAC name NEWNAME             set the BLE device name

Config layout follows cfg_t in pvvx/ATC_MiThermometer src/app.h (firmware v4.7+).
"""
import argparse
import sys

import dbus
import dbus.mainloop.glib
from gi.repository import GLib

CMD_UUID = "00001f1f-0000-1000-8000-00805f9b34fb"
CMD_DEV_ID = 0x00
CMD_DNAME = 0x01
CMD_CFG = 0x55
CFG_LEN = 11
DEBUG = False

ADV_TYPES = ["atc", "pvvx", "mi", "bthome"]
HW_VERSIONS = {0: "B1.4", 3: "B1.9", 4: "B1.6", 5: "B1.7", 10: "B1.5", 14: "B1.6 (new)"}
SMILEY = ["off", " ^_^ ", " -^- ", " ooo ", "(   )", "(^_^)", "(-^-)", "(ooo)"]

# name -> (byte index, bit offset, bit width, help)
BITFIELDS = {
    "adv_type":       (0, 0, 2, "atc | pvvx | mi | bthome"),
    "comfort_smiley": (0, 2, 1, "show comfort smiley (0/1)"),
    "show_clock":     (0, 3, 1, "show clock instead of smiley (0/1)"),
    "fahrenheit":     (0, 4, 1, "display °F (0/1)"),
    "show_battery":   (0, 5, 1, "show battery % on screen (0/1)"),
    "tx_measures":    (0, 6, 1, "notify all measurements while connected (0/1)"),
    "lp_measures":    (0, 7, 1, "low-power sensor measurements (0/1)"),
    "smiley":         (1, 0, 3, "fixed smiley 0..7 when comfort_smiley=0"),
    "encryption":     (1, 3, 1, "encrypted advertising, needs bind key (0/1)"),
    "adv_flags":      (1, 4, 1, "add flags to advertising (0/1)"),
    "bt5phy":         (1, 5, 1, "BT5 PHY support (0/1, reboots)"),
    "longrange":      (1, 6, 1, "LE Long Range advertising (0/1, reboots)"),
    "screen_off":     (1, 7, 1, "turn the LCD off (0/1)"),
    "adv_delay":      (2, 0, 4, "random adv delay 0..15 x0.625 ms"),
}
# name -> (byte index, help)
BYTEFIELDS = {
    "event_adv_cnt":     (3, "adverts sent after an event (min 6)"),
    "adv_interval":      (4, "advertising interval x62.5 ms (1..160)"),
    "measure_interval":  (5, "measure every N adverts (2..25)"),
    "tx_power":          (6, "RF TX power code (see flasher page)"),
    "connect_latency":   (7, "connection latency (x20 ms, +1)"),
    "lcd_update":        (8, "min LCD update step x0.05 s"),
    "averaging":         (10, "history averaging, x measure period"),
}
TX_POWER = {63: "+10.46", 191: "+3.01", 185: "+2.39", 180: "+1.73", 176: "+1.17",
            172: "+0.58", 169: "+0.04", 164: "-0.97", 160: "-1.89", 156: "-3.03",
            150: "-5.03", 140: "-9.89", 130: "-25.18"}


def get_field(cfg, name):
    if name in BITFIELDS:
        idx, off, width, _ = BITFIELDS[name]
        return (cfg[idx] >> off) & ((1 << width) - 1)
    return cfg[BYTEFIELDS[name][0]]


def set_field(cfg, name, value):
    if name == "adv_type" and value in ADV_TYPES:
        value = ADV_TYPES.index(value)
    value = int(value, 0) if isinstance(value, str) else value
    if name in BITFIELDS:
        idx, off, width, _ = BITFIELDS[name]
        mask = ((1 << width) - 1) << off
        if value >> width:
            raise ValueError(f"{name}: {value} does not fit in {width} bit(s)")
        cfg[idx] = (cfg[idx] & ~mask) | (value << off)
    elif name in BYTEFIELDS:
        if not 0 <= value <= 255:
            raise ValueError(f"{name}: {value} out of range 0..255")
        cfg[BYTEFIELDS[name][0]] = value
    else:
        raise KeyError(f"unknown field {name!r}; known: {', '.join([*BITFIELDS, *BYTEFIELDS])}")


def describe(cfg):
    adv_ms = cfg[4] * 62.5
    rows = []
    for name in [*BITFIELDS, *BYTEFIELDS]:
        v = get_field(cfg, name)
        note = ""
        if name == "adv_type":
            note = ADV_TYPES[v]
        elif name == "smiley":
            note = repr(SMILEY[v])
        elif name == "adv_interval":
            note = f"{adv_ms / 1000:g} s"
        elif name == "measure_interval":
            note = f"measure every {adv_ms * v / 1000:g} s"
        elif name == "tx_power":
            note = f"{TX_POWER.get(v, '?')} dBm"
        elif name == "averaging":
            note = f"history point every {adv_ms * cfg[5] * v / 60000:g} min" if v else "history off"
        rows.append(f"  {name:<17} {v:<4} {note}")
    rows.append(f"  {'hw_ver':<17} {cfg[9]:<4} {HW_VERSIONS.get(cfg[9], '?')}")
    return "\n".join(rows)


class AtcDevice:
    def __init__(self, bus, mac, adapter="hci0"):
        self.bus = bus
        self.loop = GLib.MainLoop()
        self.adapter = dbus.Interface(bus.get_object("org.bluez", f"/org/bluez/{adapter}"),
                                      "org.bluez.Adapter1")
        self.path = f"/org/bluez/{adapter}/dev_{mac.upper().replace(':', '_')}"
        self.char = None
        self.replies = {}

    def _wait(self, predicate, timeout, what, every=None):
        """Run the main loop until predicate() is true; call every() once a second while waiting."""
        if predicate():
            return
        deadline = GLib.get_monotonic_time() + timeout * 1_000_000
        ticks = [0]

        def poll():
            if predicate() or GLib.get_monotonic_time() > deadline:
                self.loop.quit()
                return False
            ticks[0] += 1
            if every and ticks[0] % 20 == 0:
                every()
            return True

        GLib.timeout_add(50, poll)
        self.loop.run()
        if not predicate():
            raise TimeoutError(f"timed out waiting for {what}")

    def _device_known(self):
        om = dbus.Interface(self.bus.get_object("org.bluez", "/"), "org.freedesktop.DBus.ObjectManager")
        return self.path in om.GetManagedObjects()

    def connect(self):
        if not self._device_known():
            self.adapter.SetDiscoveryFilter({"Transport": "le"})
            self.adapter.StartDiscovery()
            try:
                self._wait(self._device_known, 30, "device to show up in scan")
            finally:
                self.adapter.StopDiscovery()
        self.dev = self.bus.get_object("org.bluez", self.path)
        self.dev_props = dbus.Interface(self.dev, "org.freedesktop.DBus.Properties")
        for attempt in range(3):
            try:
                dbus.Interface(self.dev, "org.bluez.Device1").Connect(timeout=30)
                break
            except dbus.DBusException:
                if attempt == 2:
                    raise
        self._wait(lambda: bool(self.dev_props.Get("org.bluez.Device1", "ServicesResolved")),
                   20, "GATT services")
        om = dbus.Interface(self.bus.get_object("org.bluez", "/"), "org.freedesktop.DBus.ObjectManager")
        for path, ifaces in om.GetManagedObjects().items():
            ch = ifaces.get("org.bluez.GattCharacteristic1")
            if ch and path.startswith(self.path) and str(ch["UUID"]) == CMD_UUID:
                self.char_path = path
                self.char = dbus.Interface(self.bus.get_object("org.bluez", path),
                                           "org.bluez.GattCharacteristic1")
                break
        if self.char is None:
            raise RuntimeError("command characteristic 0x1F1F not found (not pvvx firmware?)")
        self.bus.add_signal_receiver(self._on_notify, dbus_interface="org.freedesktop.DBus.Properties",
                                     signal_name="PropertiesChanged", path=self.char_path)
        self.char.StartNotify()

    def _on_notify(self, interface, changed, _invalidated):
        if interface == "org.bluez.GattCharacteristic1" and "Value" in changed:
            data = bytes(changed["Value"])
            if DEBUG:
                print(f"notify: {data.hex()}", file=sys.stderr)
            if data:
                self.replies[data[0]] = data

    def command(self, payload, timeout=8):
        cmd = payload[0]
        self.replies.pop(cmd, None)
        def send():
            self.char.WriteValue(dbus.Array(payload, signature="y"), {})

        # write-without-response can be dropped right after connecting; commands are idempotent
        send()
        self._wait(lambda: cmd in self.replies, timeout, f"reply to command 0x{cmd:02x}", every=send)
        return self.replies[cmd]

    def disconnect(self):
        try:
            dbus.Interface(self.dev, "org.bluez.Device1").Disconnect()
        except dbus.DBusException:
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter,
                                     epilog="fields:\n" + "\n".join(
                                         f"  {k:<17} {v[-1]}" for k, v in {**BITFIELDS, **BYTEFIELDS}.items()))
    parser.add_argument("mac")
    parser.add_argument("action", nargs="?", choices=["show", "set", "name"], default="show")
    parser.add_argument("args", nargs="*")
    args = parser.parse_args()

    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    dev = AtcDevice(dbus.SystemBus(), args.mac)
    try:
        dev.connect()
        info = dev.command([CMD_DEV_ID])
        hw, sw = int.from_bytes(info[2:4], "little"), int.from_bytes(info[4:6], "little")
        name = dev.command([CMD_DNAME])[1:].decode(errors="replace").rstrip("\0")
        print(f"{args.mac}  name={name or '(default)'}  firmware=v{sw >> 4}.{sw & 0xf}  hw_id={hw}")
        if sw < 0x47:
            sys.exit("firmware older than v4.7 uses a different config layout; not supported")

        cfg = bytearray(dev.command([CMD_CFG])[2:2 + CFG_LEN])
        if len(cfg) != CFG_LEN:
            sys.exit(f"unexpected config length {len(cfg)}")

        if args.action == "set":
            new = bytearray(cfg)
            for item in args.args:
                key, _, value = item.partition("=")
                set_field(new, key, value)
            if new == cfg:
                print("nothing to change")
            else:
                cfg = bytearray(dev.command([CMD_CFG, *new])[2:2 + CFG_LEN])
                for key in {item.partition("=")[0] for item in args.args}:
                    if get_field(cfg, key) != get_field(new, key):
                        print(f"warning: device adjusted {key} to {get_field(cfg, key)}")
                print("config written")
        elif args.action == "name":
            new_name = " ".join(args.args).encode()
            if not 0 < len(new_name) <= 20:
                sys.exit("name must be 1..20 bytes")
            reply = dev.command([CMD_DNAME, *new_name])
            print(f"name set to {reply[1:].decode(errors='replace').rstrip(chr(0))} (applies after reconnect)")
        print(describe(cfg))
    finally:
        dev.disconnect()


if __name__ == "__main__":
    main()
