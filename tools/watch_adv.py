#!/usr/bin/env python3
"""Watch whether a bonded device is advertising, using BlueZ's own resolution.

A bonded peripheral that rotates through resolvable private addresses still
shows up under its identity address in BlueZ, because bluetoothd resolves the
RPA with the stored IRK. So an RSSI update on the device object is proof the
device advertised -- no hand-rolled crypto required.

Usage:  sudo python3 watch_adv.py [BD_ADDR] [--seconds N]
"""
import argparse
import time

import dbus
import dbus.mainloop.glib
from gi.repository import GLib

BLUEZ = "org.bluez"
OM = "org.freedesktop.DBus.ObjectManager"
PROPS = "org.freedesktop.DBus.Properties"
DEV = "org.bluez.Device1"
ADAPTER = "org.bluez.Adapter1"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("address", nargs="?", default="DC:2B:2A:D6:38:61")
    ap.add_argument("--seconds", type=int, default=120)
    args = ap.parse_args()

    dev_path = "/org/bluez/hci0/dev_" + args.address.upper().replace(":", "_")

    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SystemBus()
    loop = GLib.MainLoop()

    adapter = dbus.Interface(bus.get_object(BLUEZ, "/org/bluez/hci0"), ADAPTER)
    try:
        adapter.SetDiscoveryFilter({"Transport": "le", "DuplicateData": True})
        adapter.StartDiscovery()
    except dbus.DBusException as e:
        print(f"discovery: {e.get_dbus_message()}")

    print(f"watching {args.address} for {args.seconds}s")
    print("press buttons / hold the pairing chord -- any RSSI line below means "
          "it IS advertising\n")

    seen = {"n": 0}
    start = time.time()

    def on_props(iface, changed, invalidated, path=None):
        if iface != DEV or path != dev_path:
            return
        interesting = {k: v for k, v in changed.items()
                       if k in ("RSSI", "Connected", "ServicesResolved", "TxPower")}
        if not interesting:
            return
        seen["n"] += 1
        t = time.time() - start
        pretty = ", ".join(f"{k}={v}" for k, v in interesting.items())
        print(f"[{t:6.1f}s] {pretty}")

    bus.add_signal_receiver(on_props, dbus_interface=PROPS,
                            signal_name="PropertiesChanged", path_keyword="path")

    def done():
        print(f"\n{seen['n']} advertising/state update(s) seen")
        if not seen["n"]:
            print("the remote did NOT advertise in this window")
        else:
            print("the remote IS advertising and reachable")
        try:
            adapter.StopDiscovery()
        except dbus.DBusException:
            pass
        loop.quit()
        return False

    GLib.timeout_add_seconds(args.seconds, done)
    loop.run()


if __name__ == "__main__":
    main()
