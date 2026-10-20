#!/usr/bin/env python3
"""Diagnose why the 0xAF handshake write is refused.

Attempts writes to the two HID feature reports AND to the non-HID Apple vendor
characteristic as a control. Run with btmon capturing to see whether an ATT
Write Request actually reaches the air:

  * ATT request sent, Error Response returned -> the remote is refusing
  * no ATT traffic at all                     -> BlueZ local policy is refusing
"""
import sys
import time
import argparse

import dbus
import dbus.mainloop.glib

BLUEZ = "org.bluez"
OM = "org.freedesktop.DBus.ObjectManager"
PROPS = "org.freedesktop.DBus.Properties"
DEV = "org.bluez.Device1"
CHRC = "org.bluez.GattCharacteristic1"

TARGETS = [
    ("char001c", "HID feature report 240 (handshake target)"),
    ("char001f", "HID feature report 241"),
    ("char0039", "Apple vendor 30e69638 (NON-HID control)"),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("address", nargs="?", default="DC:2B:2A:D6:38:61")
    ap.add_argument("--connect-timeout", type=int, default=600)
    args = ap.parse_args()

    addr = args.address.upper()
    dev_path = "/org/bluez/hci0/dev_" + addr.replace(":", "_")

    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SystemBus()

    dev = dbus.Interface(bus.get_object(BLUEZ, dev_path), DEV)
    devp = dbus.Interface(bus.get_object(BLUEZ, dev_path), PROPS)

    print(f"waiting up to {args.connect_timeout}s -- PRESS A BUTTON to wake the remote")
    deadline = time.time() + args.connect_timeout
    n = 0
    while time.time() < deadline:
        try:
            if bool(devp.Get(DEV, "Connected")):
                break
        except dbus.DBusException:
            pass
        try:
            dev.Connect(timeout=15)
            time.sleep(1)
            if bool(devp.Get(DEV, "Connected")):
                break
        except dbus.DBusException:
            pass
        n += 1
        if n % 10 == 0:
            print(f"  still waiting ({int(deadline - time.time())}s left)")
        time.sleep(2)
    else:
        print("never connected")
        return
    print("connected")

    for _ in range(20):
        try:
            if bool(devp.Get(DEV, "ServicesResolved")):
                break
        except dbus.DBusException:
            pass
        time.sleep(0.5)

    # report the link's security level -- authorization failures often mean
    # the bond is unauthenticated (Just Works) rather than MITM-protected
    for k in ("Paired", "Bonded", "Trusted", "Connected", "ServicesResolved"):
        try:
            print(f"  {k}: {bool(devp.Get(DEV, k))}")
        except dbus.DBusException:
            pass

    objs = dbus.Interface(bus.get_object(BLUEZ, "/"), OM).GetManagedObjects()

    for short, desc in TARGETS:
        path = f"{dev_path}/service{'0015' if short != 'char0039' else '0032'}/{short}"
        # locate properly rather than guessing the service number
        path = next(
            (p for p in objs if p.startswith(dev_path) and p.endswith("/" + short)), None
        )
        if not path:
            print(f"\n{short}: not found")
            continue
        c = objs[path].get(CHRC, {})
        flags = [str(f) for f in c.get("Flags", [])]
        print(f"\n{short}  {desc}")
        print(f"  uuid  {c.get('UUID')}")
        print(f"  flags {flags}")

        ci = dbus.Interface(bus.get_object(BLUEZ, path), CHRC)
        for wtype in ("request", "command"):
            if wtype == "command" and "write-without-response" not in flags:
                continue
            if wtype == "request" and "write" not in flags:
                continue
            try:
                print(f"  >>> ATT write 0xAF ({wtype})")
                ci.WriteValue([dbus.Byte(0xAF)], {"type": dbus.String(wtype)})
                print("      OK")
            except dbus.DBusException as e:
                print(f"      {e.get_dbus_name()}: {e.get_dbus_message()}")
        if "read" in flags:
            try:
                print(f"  <<< read: {bytes(ci.ReadValue({})).hex(' ')}")
            except dbus.DBusException as e:
                print(f"  <<< read failed: {e.get_dbus_message()}")


if __name__ == "__main__":
    main()
