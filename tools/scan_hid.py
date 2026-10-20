#!/usr/bin/env python3
"""Scan for BLE devices and highlight anything that looks like a HID remote.

Run on the Pi:  sudo python3 scan_hid.py [seconds]
"""
import sys
import subprocess
import dbus

HID_UUID = "00001812-0000-1000-8000-00805f9b34fb"
# Appearance values that mean "some kind of input device" (category 0x00F)
HID_APPEARANCE_CATEGORY = 0x0F


def discovered():
    bus = dbus.SystemBus()
    mgr = dbus.Interface(
        bus.get_object("org.bluez", "/"), "org.freedesktop.DBus.ObjectManager"
    )
    for path, ifaces in mgr.GetManagedObjects().items():
        dev = ifaces.get("org.bluez.Device1")
        if dev:
            yield path, dev


def score(dev):
    """Higher = more likely to be the remote we want."""
    uuids = [str(u).lower() for u in dev.get("UUIDs", [])]
    appearance = int(dev.get("Appearance", 0))
    n = 0
    if HID_UUID in uuids:
        n += 10
    if appearance >> 6 == HID_APPEARANCE_CATEGORY:
        n += 5
    name = str(dev.get("Name", ""))
    if any(w in name.lower() for w in ("remote", "siri", "apple", "atv")):
        n += 3
    if name:
        n += 1
    return n


def main():
    secs = int(sys.argv[1]) if len(sys.argv) > 1 else 20
    print(f"Scanning {secs}s -- put the remote in pairing mode NOW\n")
    subprocess.run(
        ["bluetoothctl", "--timeout", str(secs), "scan", "le"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    rows = sorted(discovered(), key=lambda kv: -score(kv[1]))
    print(f"{'ADDR':<18} {'RSSI':>5}  {'APPEAR':>6}  NAME")
    print("-" * 70)
    for _path, dev in rows:
        if not score(dev):
            continue
        addr = str(dev.get("Address", "?"))
        rssi = dev.get("RSSI")
        appearance = int(dev.get("Appearance", 0))
        name = str(dev.get("Name", dev.get("Alias", "")))
        flag = " <== HID" if HID_UUID in [str(u).lower() for u in dev.get("UUIDs", [])] else ""
        print(
            f"{addr:<18} {rssi if rssi is not None else '-':>5}  "
            f"0x{appearance:04x}  {name}{flag}"
        )


if __name__ == "__main__":
    main()
