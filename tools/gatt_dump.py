#!/usr/bin/env python3
"""Dump the GATT service/characteristic tree of a connected BLE device."""
import sys
import dbus

BLUEZ = "org.bluez"
OM = "org.freedesktop.DBus.ObjectManager"

NAMES = {
    "00002a4a": "HID Information",
    "00002a4b": "Report Map",
    "00002a4c": "HID Control Point",
    "00002a4d": "Report",
    "00002a4e": "Protocol Mode",
    "00002a22": "Boot Keyboard Input Report",
    "00002a19": "Battery Level",
    "00002a24": "Model Number",
    "00002a25": "Serial Number",
    "00002a26": "Firmware Revision",
    "00002a27": "Hardware Revision",
    "00002a28": "Software Revision",
    "00002a29": "Manufacturer Name",
    "00001812": "Human Interface Device",
    "0000180f": "Battery Service",
    "0000180a": "Device Information",
}


def label(uuid):
    return NAMES.get(uuid.lower()[:8], "")


def main():
    addr = (sys.argv[1] if len(sys.argv) > 1 else "").upper().replace(":", "_")
    bus = dbus.SystemBus()
    objs = dbus.Interface(bus.get_object(BLUEZ, "/"), OM).GetManagedObjects()

    svcs = {}
    for path, ifaces in objs.items():
        if addr and addr not in path:
            continue
        if "org.bluez.GattService1" in ifaces:
            svcs[path] = ifaces["org.bluez.GattService1"]

    for spath in sorted(svcs):
        s = svcs[spath]
        su = str(s["UUID"])
        print(f"\nSERVICE {su}  {label(su)}")
        for path, ifaces in sorted(objs.items()):
            c = ifaces.get("org.bluez.GattCharacteristic1")
            if not c or not path.startswith(spath + "/"):
                continue
            cu = str(c["UUID"])
            flags = ",".join(str(f) for f in c.get("Flags", []))
            print(f"  CHAR {cu}  {label(cu):<28} [{flags}]")
            print(f"       {path}")
            # Try a read for readable, non-report characteristics
            if "read" in flags:
                try:
                    ci = dbus.Interface(
                        bus.get_object(BLUEZ, path), "org.bluez.GattCharacteristic1"
                    )
                    val = bytes(ci.ReadValue({}))
                    printable = val.decode("utf-8", "replace").strip("\x00")
                    print(f"       value: {val.hex(' ')}  ({printable!r})")
                except dbus.DBusException as e:
                    print(f"       read failed: {e.get_dbus_message()}")
            for dpath, difaces in sorted(objs.items()):
                d = difaces.get("org.bluez.GattDescriptor1")
                if d and dpath.startswith(path + "/"):
                    du = str(d["UUID"])
                    extra = ""
                    try:
                        di = dbus.Interface(
                            bus.get_object(BLUEZ, dpath), "org.bluez.GattDescriptor1"
                        )
                        extra = "  " + bytes(di.ReadValue({})).hex(" ")
                    except dbus.DBusException:
                        pass
                    print(f"       DESC {du}{extra}")


if __name__ == "__main__":
    main()
