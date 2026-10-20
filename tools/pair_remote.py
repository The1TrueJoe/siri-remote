#!/usr/bin/env python3
"""Wait for a BLE HID remote to enter pairing mode, then bond + connect it.

Unlike sleep-driven bluetoothctl scripts, this reacts the moment the device
advertises, so you can start it and *then* hold the pairing chord.

Usage:  sudo python3 pair_remote.py [BD_ADDR] [--timeout SECONDS]
"""
import sys
import argparse

import dbus
import dbus.service
import dbus.mainloop.glib
from gi.repository import GLib

BLUEZ = "org.bluez"
ADAPTER_IFACE = "org.bluez.Adapter1"
DEVICE_IFACE = "org.bluez.Device1"
PROPS_IFACE = "org.freedesktop.DBus.Properties"
OM_IFACE = "org.freedesktop.DBus.ObjectManager"
AGENT_PATH = "/c4remote/agent"
HID_UUID = "00001812-0000-1000-8000-00805f9b34fb"


class Agent(dbus.service.Object):
    """NoInputNoOutput agent: auto-accepts Just Works pairing."""

    @dbus.service.method("org.bluez.Agent1", in_signature="", out_signature="")
    def Release(self):
        pass

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="")
    def RequestAuthorization(self, device):
        print(f"  agent: authorizing {device}")

    @dbus.service.method("org.bluez.Agent1", in_signature="os", out_signature="")
    def AuthorizeService(self, device, uuid):
        print(f"  agent: authorizing service {uuid}")

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="u")
    def RequestPasskey(self, device):
        return dbus.UInt32(0)

    @dbus.service.method("org.bluez.Agent1", in_signature="ouq", out_signature="")
    def DisplayPasskey(self, device, passkey, entered):
        print(f"  agent: passkey {passkey:06d}")

    @dbus.service.method("org.bluez.Agent1", in_signature="os", out_signature="")
    def DisplayPinCode(self, device, pincode):
        print(f"  agent: pin {pincode}")

    @dbus.service.method("org.bluez.Agent1", in_signature="ou", out_signature="")
    def RequestConfirmation(self, device, passkey):
        print(f"  agent: confirming passkey {passkey:06d}")

    @dbus.service.method("org.bluez.Agent1", in_signature="", out_signature="")
    def Cancel(self):
        print("  agent: cancelled")


def find_adapter(bus):
    objs = dbus.Interface(bus.get_object(BLUEZ, "/"), OM_IFACE).GetManagedObjects()
    for path, ifaces in objs.items():
        if ADAPTER_IFACE in ifaces:
            return path
    raise SystemExit("no bluetooth adapter found")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("address", nargs="?", help="BD_ADDR; omit to take any HID device")
    ap.add_argument("--timeout", type=int, default=120)
    args = ap.parse_args()
    want = args.address.upper() if args.address else None

    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SystemBus()
    loop = GLib.MainLoop()

    agent = Agent(bus, AGENT_PATH)
    mgr = dbus.Interface(bus.get_object(BLUEZ, "/org/bluez"), "org.bluez.AgentManager1")
    mgr.RegisterAgent(AGENT_PATH, "NoInputNoOutput")
    mgr.RequestDefaultAgent(AGENT_PATH)
    print("agent registered (NoInputNoOutput)")

    apath = find_adapter(bus)
    adapter = dbus.Interface(bus.get_object(BLUEZ, apath), ADAPTER_IFACE)
    aprops = dbus.Interface(bus.get_object(BLUEZ, apath), PROPS_IFACE)
    aprops.Set(ADAPTER_IFACE, "Powered", dbus.Boolean(True))

    state = {"done": False, "target": None}

    def matches(path, dev):
        addr = str(dev.get("Address", "")).upper()
        if want:
            return addr == want
        uuids = [str(u).lower() for u in dev.get("UUIDs", [])]
        return HID_UUID in uuids

    def try_pair(path, dev):
        if state["done"]:
            return
        state["done"] = True
        addr = str(dev.get("Address", "?"))
        name = str(dev.get("Name", dev.get("Alias", "")))
        print(f"\nFOUND {addr}  {name}  RSSI={dev.get('RSSI')}")
        try:
            adapter.StopDiscovery()
        except dbus.DBusException:
            pass

        d = dbus.Interface(bus.get_object(BLUEZ, path), DEVICE_IFACE)
        dp = dbus.Interface(bus.get_object(BLUEZ, path), PROPS_IFACE)

        try:
            print("pairing...")
            d.Pair(timeout=60)
            print("  paired OK")
        except dbus.DBusException as e:
            print(f"  pair failed: {e.get_dbus_name()}: {e.get_dbus_message()}")

        try:
            dp.Set(DEVICE_IFACE, "Trusted", dbus.Boolean(True))
            print("  trusted")
        except dbus.DBusException as e:
            print(f"  trust failed: {e.get_dbus_message()}")

        try:
            print("connecting...")
            d.Connect(timeout=60)
            print("  connected OK")
        except dbus.DBusException as e:
            print(f"  connect failed: {e.get_dbus_name()}: {e.get_dbus_message()}")

        for k in ("Paired", "Bonded", "Trusted", "Connected", "ServicesResolved"):
            try:
                print(f"  {k}: {bool(dp.Get(DEVICE_IFACE, k))}")
            except dbus.DBusException:
                pass
        loop.quit()

    def on_added(path, ifaces):
        dev = ifaces.get(DEVICE_IFACE)
        if dev and matches(path, dev):
            try_pair(path, dev)

    def on_changed(iface, changed, invalidated, path=None):
        if iface != DEVICE_IFACE or state["done"] or not path:
            return
        try:
            dp = dbus.Interface(bus.get_object(BLUEZ, path), PROPS_IFACE)
            dev = dp.GetAll(DEVICE_IFACE)
        except dbus.DBusException:
            return
        if matches(path, dev):
            try_pair(path, dev)

    bus.add_signal_receiver(
        on_added, dbus_interface=OM_IFACE, signal_name="InterfacesAdded"
    )
    bus.add_signal_receiver(
        on_changed,
        dbus_interface=PROPS_IFACE,
        signal_name="PropertiesChanged",
        path_keyword="path",
    )

    # Anything already cached from a previous scan?
    objs = dbus.Interface(bus.get_object(BLUEZ, "/"), OM_IFACE).GetManagedObjects()
    for path, ifaces in objs.items():
        dev = ifaces.get(DEVICE_IFACE)
        if dev and matches(path, dev):
            try_pair(path, dev)
            break

    if not state["done"]:
        try:
            adapter.SetDiscoveryFilter({"Transport": "le", "DuplicateData": False})
            adapter.StartDiscovery()
        except dbus.DBusException as e:
            print(f"discovery: {e.get_dbus_message()}")
        target = want or "any HID device"
        print(f"scanning up to {args.timeout}s for {target}")
        print(">>> HOLD THE PAIRING CHORD NOW (Menu + Volume Up, ~5s) <<<\n")

        def bail():
            if not state["done"]:
                print("\ntimed out -- device never advertised")
                loop.quit()
            return False

        GLib.timeout_add_seconds(args.timeout, bail)
        loop.run()

    try:
        mgr.UnregisterAgent(AGENT_PATH)
    except dbus.DBusException:
        pass


if __name__ == "__main__":
    main()
