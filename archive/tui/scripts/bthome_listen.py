#!/usr/bin/env python3
"""Listen for BTHome v2 BLE advertisements via BlueZ D-Bus and print decoded values.

Usage: bthome_listen.py [--duration SECONDS] [--mac AA:BB:CC:DD:EE:FF ...]
"""
import argparse
import datetime
import struct
import sys

import dbus
import dbus.mainloop.glib
from gi.repository import GLib

BTHOME_UUID = "0000fcd2-0000-1000-8000-00805f9b34fb"

# object id -> (name, size, signed, factor, unit)
OBJECTS = {
    0x00: ("packet_id", 1, False, 1, ""),
    0x01: ("battery", 1, False, 1, "%"),
    0x02: ("temperature", 2, True, 0.01, "°C"),
    0x03: ("humidity", 2, False, 0.01, "%"),
    0x04: ("pressure", 3, False, 0.01, "hPa"),
    0x05: ("illuminance", 3, False, 0.01, "lx"),
    0x08: ("dewpoint", 2, True, 0.01, "°C"),
    0x09: ("count", 1, False, 1, ""),
    0x0A: ("energy", 3, False, 0.001, "kWh"),
    0x0B: ("power", 3, False, 0.01, "W"),
    0x0C: ("voltage", 2, False, 0.001, "V"),
    0x0F: ("generic_boolean", 1, False, 1, ""),
    0x10: ("power_on", 1, False, 1, ""),
    0x11: ("opening", 1, False, 1, ""),
    0x12: ("co2", 2, False, 1, "ppm"),
    0x14: ("moisture", 2, False, 0.01, "%"),
    0x2D: ("window", 1, False, 1, ""),
    0x2E: ("humidity", 1, False, 1, "%"),
    0x3A: ("button", 1, False, 1, ""),
    0x3D: ("count", 2, False, 1, ""),
    0x3E: ("count", 4, False, 1, ""),
    0x40: ("distance", 2, False, 1, "mm"),
    0x43: ("current", 2, False, 0.001, "A"),
    0x45: ("temperature", 2, True, 0.1, "°C"),
    0x4A: ("voltage", 2, False, 0.1, "V"),
    0x50: ("timestamp", 4, False, 1, ""),
    0x57: ("temperature", 1, True, 1, "°C"),
    0x58: ("temperature", 1, True, 0.35, "°C"),
    0xF0: ("device_type_id", 2, False, 1, ""),
    0xF1: ("firmware", 4, False, 1, ""),
    0xF2: ("firmware", 3, False, 1, ""),
}


def decode(data):
    """Return (flags dict, list of (name, value, unit)) for a BTHome v2 payload."""
    info = data[0]
    flags = {
        "encrypted": bool(info & 0x01),
        "trigger": bool(info & 0x04),
        "version": info >> 5,
    }
    values = []
    if flags["encrypted"] or flags["version"] != 2:
        return flags, values
    i = 1
    while i < len(data):
        obj_id = data[i]
        spec = OBJECTS.get(obj_id)
        if spec is None:
            values.append((f"unknown_0x{obj_id:02x}", data[i:].hex(), "(raw rest)"))
            break
        name, size, signed, factor, unit = spec
        raw = int.from_bytes(data[i + 1:i + 1 + size], "little", signed=signed)
        value = round(raw * factor, 3) if factor != 1 else raw
        values.append((name, value, unit))
        i += 1 + size
    return flags, values


class Listener:
    def __init__(self, bus, macs):
        self.bus = bus
        self.macs = {m.upper() for m in macs}
        self.names = {}
        self.last = {}

    def handle(self, path, props):
        service_data = props.get("ServiceData", {})
        if BTHOME_UUID not in service_data:
            return
        dev = self.bus.get_object("org.bluez", path)
        dev_props = dbus.Interface(dev, "org.freedesktop.DBus.Properties")
        try:
            mac = str(dev_props.Get("org.bluez.Device1", "Address"))
            name = str(dev_props.Get("org.bluez.Device1", "Alias"))
        except dbus.DBusException:
            return
        if self.macs and mac not in self.macs:
            return
        payload = bytes(service_data[BTHOME_UUID])
        if self.last.get(mac) == payload:
            return
        self.last[mac] = payload
        flags, values = decode(payload)
        ts = datetime.datetime.now().strftime("%H:%M:%S")
        if flags["encrypted"]:
            body = f"encrypted payload {payload.hex()} (bind key needed)"
        else:
            body = "  ".join(f"{n}={v}{u}" for n, v, u in values)
        print(f"{ts}  {name:<12} {mac}  {body}", flush=True)

    def on_interfaces_added(self, path, interfaces):
        if "org.bluez.Device1" in interfaces:
            self.handle(path, interfaces["org.bluez.Device1"])

    def on_properties_changed(self, interface, changed, _invalidated, path=None):
        if interface == "org.bluez.Device1":
            self.handle(path, changed)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--duration", type=int, default=0, help="seconds to listen (0 = forever)")
    parser.add_argument("--mac", nargs="*", default=[], help="only show these MAC addresses")
    parser.add_argument("--adapter", default="hci0")
    args = parser.parse_args()

    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SystemBus()
    listener = Listener(bus, args.mac)
    bus.add_signal_receiver(listener.on_interfaces_added,
                            dbus_interface="org.freedesktop.DBus.ObjectManager",
                            signal_name="InterfacesAdded")
    bus.add_signal_receiver(listener.on_properties_changed,
                            dbus_interface="org.freedesktop.DBus.Properties",
                            signal_name="PropertiesChanged",
                            arg0="org.bluez.Device1",
                            path_keyword="path")

    adapter = dbus.Interface(bus.get_object("org.bluez", f"/org/bluez/{args.adapter}"),
                             "org.bluez.Adapter1")
    adapter.SetDiscoveryFilter({"Transport": "le", "DuplicateData": dbus.Boolean(True)})
    adapter.StartDiscovery()
    print(f"Listening for BTHome v2 on {args.adapter}"
          + (f" for {args.duration}s" if args.duration else "") + " (Ctrl+C to stop)",
          file=sys.stderr, flush=True)

    loop = GLib.MainLoop()
    if args.duration:
        GLib.timeout_add_seconds(args.duration, loop.quit)
    try:
        loop.run()
    except KeyboardInterrupt:
        pass
    finally:
        try:
            adapter.StopDiscovery()
        except dbus.DBusException:
            pass


if __name__ == "__main__":
    main()
