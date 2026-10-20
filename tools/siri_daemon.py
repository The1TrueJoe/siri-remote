#!/usr/bin/env python3
"""Persistent Siri Remote daemon.

Runs forever. Waits for the remote to appear, pairs it if needed, takes the
ATT channel away from BlueZ, sends the 0xAF handshake and streams button
events -- reconnecting on its own whenever the remote sleeps or drops.

The point is to decouple everything from human timing: press a button whenever
you like and the log will show what happened, with timestamps. Nothing here
needs a person watching at the right moment.

Log lines are prefixed so they can be grepped:
  SIGHTING  the remote advertised (proof it is alive)
  PAIR      bonding progress
  LINK      ATT channel / handshake progress
  BUTTON    an actual decoded button event
  TOUCH     touchpad coordinates
  VOICE     microphone session start/stop and decoded WAV path
  BATTERY   battery percent / charging state
  STATE     periodic heartbeat so silence is distinguishable from a crash
"""
import os
import subprocess
import sys
import threading
import time
import traceback

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import dbus
import dbus.service
import dbus.mainloop.glib

from siri_att import SiriRemote, ATTError, BT_SECURITY_MEDIUM, BDADDR_LE_PUBLIC
import siri_protocol as proto
from opus_wav import OpusDecoder, write_wav
import transcribe

BLUEZ = "org.bluez"
OM = "org.freedesktop.DBus.ObjectManager"
PROPS = "org.freedesktop.DBus.Properties"
DEV = "org.bluez.Device1"
ADAPTER = "org.bluez.Adapter1"
AGENT_PATH = "/c4remote/daemon_agent"
AUDIO_DIR = os.environ.get("SIRI_AUDIO_DIR", "/home/pi/siri-audio")

HID_UUID = "00001812-0000-1000-8000-00805f9b34fb"
TARGET = os.environ.get("SIRI_ADDR", "").upper()   # optional pin to one address

ADAPTER_PATH = "/org/bluez/hci0"


def log(tag, msg):
    print(f"{time.strftime('%Y-%m-%d %H:%M:%S')}  {tag:<9} {msg}", flush=True)


class Agent(dbus.service.Object):
    """NoInputNoOutput agent -- accepts Just Works pairing."""

    @dbus.service.method("org.bluez.Agent1", in_signature="", out_signature="")
    def Release(self):
        pass

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="")
    def RequestAuthorization(self, device):
        log("PAIR", f"authorizing {device}")

    @dbus.service.method("org.bluez.Agent1", in_signature="os", out_signature="")
    def AuthorizeService(self, device, uuid):
        log("PAIR", f"authorizing service {uuid}")

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="u")
    def RequestPasskey(self, device):
        return dbus.UInt32(0)

    @dbus.service.method("org.bluez.Agent1", in_signature="ouq", out_signature="")
    def DisplayPasskey(self, device, passkey, entered):
        log("PAIR", f"passkey {passkey:06d}")

    @dbus.service.method("org.bluez.Agent1", in_signature="os", out_signature="")
    def DisplayPinCode(self, device, pincode):
        log("PAIR", f"pin {pincode}")

    @dbus.service.method("org.bluez.Agent1", in_signature="ou", out_signature="")
    def RequestConfirmation(self, device, passkey):
        log("PAIR", f"confirming {passkey:06d}")

    @dbus.service.method("org.bluez.Agent1", in_signature="", out_signature="")
    def Cancel(self):
        log("PAIR", "cancelled")


class Daemon:
    def __init__(self):
        dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
        self.bus = dbus.SystemBus()
        self.adapter = dbus.Interface(
            self.bus.get_object(BLUEZ, ADAPTER_PATH), ADAPTER)
        self.adapter_props = dbus.Interface(
            self.bus.get_object(BLUEZ, ADAPTER_PATH), PROPS)
        self.agent = None
        self.last_seen = {}
        self.deaf = 0
        self.failures = {}
        self.failed_at = {}
        self.bonded_addr = None
        self.bonded_type = None

    # --- helpers ---------------------------------------------------------
    def objects(self):
        return dbus.Interface(self.bus.get_object(BLUEZ, "/"), OM).GetManagedObjects()

    def register_agent(self):
        try:
            self.agent = Agent(self.bus, AGENT_PATH)
            mgr = dbus.Interface(self.bus.get_object(BLUEZ, "/org/bluez"),
                                 "org.bluez.AgentManager1")
            mgr.RegisterAgent(AGENT_PATH, "NoInputNoOutput")
            mgr.RequestDefaultAgent(AGENT_PATH)
            log("PAIR", "agent registered")
        except dbus.DBusException as e:
            log("PAIR", f"agent: {e.get_dbus_message()}")

    def power_on(self):
        try:
            if not bool(self.adapter_props.Get(ADAPTER, "Powered")):
                self.adapter_props.Set(ADAPTER, "Powered", dbus.Boolean(True))
                log("STATE", "adapter powered on")
        except dbus.DBusException as e:
            log("STATE", f"power: {e.get_dbus_message()}")

    def reset_adapter(self, why):
        """Recover a wedged controller.

        The BCM43438 can lock up refusing scan-state changes -- the kernel logs
        "Opcode 0x200c failed: -16" and "Unable to disable scanning" while
        BlueZ still reports Discovering=yes. Nothing is heard from that point
        on, so this must be detected and cleared automatically.

        We bounce the adapter and then exit; systemd restarts us with fresh
        D-Bus proxies and a re-registered agent, which is far more reliable
        than trying to re-attach to a bluetoothd that just restarted.
        """
        log("STATE", f"ADAPTER WEDGED ({why}) -- resetting hci0")
        try:
            self.stop_discovery()
        except Exception:
            pass
        for cmd in (["hciconfig", "hci0", "down"],
                    ["hciconfig", "hci0", "up"],
                    ["hciconfig", "hci0", "reset"],
                    ["systemctl", "restart", "bluetooth"]):
            try:
                subprocess.run(cmd, capture_output=True, timeout=30)
                time.sleep(2)
            except Exception as e:
                log("STATE", f"  {' '.join(cmd)} failed: {e}")
        log("STATE", "adapter reset done -- exiting so systemd restarts us clean")
        sys.exit(1)

    def start_discovery(self):
        try:
            # DuplicateData=False on purpose: reporting every repeat
            # advertisement floods the HCI UART on a Pi 3 (the BCM43438 is
            # UART-attached) and shows up as "Frame reassembly failed (-84)".
            # We only need to know the remote appeared, not every beacon, and
            # a corrupted UART would wreck the 101-byte audio frames.
            self.adapter.SetDiscoveryFilter(
                {"Transport": "le", "DuplicateData": dbus.Boolean(False)})
            self.adapter.StartDiscovery()
        except dbus.DBusException as e:
            if "InProgress" not in e.get_dbus_name():
                log("STATE", f"discovery: {e.get_dbus_message()}")

    def stop_discovery(self):
        try:
            self.adapter.StopDiscovery()
        except dbus.DBusException:
            pass

    def candidates(self):
        """Devices that look like our remote, most recently seen first."""
        out = []
        for path, ifaces in self.objects().items():
            d = ifaces.get(DEV)
            if not d:
                continue
            addr = str(d.get("Address", "")).upper()
            uuids = [str(u).lower() for u in d.get("UUIDs", [])]
            name = str(d.get("Name", d.get("Alias", "")))
            is_target = (
                (TARGET and addr == TARGET)
                or HID_UUID in uuids
                or name.startswith("C08")          # this unit's serial-style name
            )
            if is_target:
                out.append((path, addr, name, d))
        return out

    @staticmethod
    def addr_type_of(d):
        """BlueZ reports 'public' or 'random'; the ATT socket needs the right
        one or connect() fails with ENOSYS."""
        return 2 if str(d.get("AddressType", "public")).lower() == "random" else 1

    # --- phases ----------------------------------------------------------
    def hearing_count(self):
        """How many devices we can hear right now -- proof the radio is live."""
        n = 0
        try:
            for _p, i in self.objects().items():
                d = i.get(DEV)
                if d and d.get("RSSI") is not None:
                    n += 1
        except dbus.DBusException:
            return -1
        return n

    def wait_for_remote(self):
        """Block until the remote is visible.

        Emits a heartbeat that includes how many other devices the radio can
        currently hear, so 'nothing is happening' is always distinguishable
        from 'the daemon or the adapter is broken'.
        """
        self.start_discovery()
        log("STATE", "waiting for the remote to advertise "
                     "(press any button, or hold Menu+Volume Up ~5s)")
        last_beat = 0.0
        while True:
            self.power_on()
            for path, addr, name, d in self.candidates():
                # Do not hammer a device that has repeatedly failed -- it may
                # be somebody else's HID device that merely looks like ours.
                if self.failures.get(addr, 0) >= 4:
                    if time.time() - self.failed_at.get(addr, 0) < 900:
                        continue
                    self.failures[addr] = 0
                rssi = d.get("RSSI")
                paired = bool(d.get("Paired", False))
                log("SIGHTING",
                    f"{addr} name={name!r} rssi={rssi} paired={paired} "
                    f"addrtype={d.get('AddressType', '?')} "
                    f"appearance={d.get('Appearance', '?')}")
                self.last_seen[addr] = time.time()
                return path, addr, d

            if time.time() - last_beat > 60:
                last_beat = time.time()
                heard = self.hearing_count()
                try:
                    scanning = bool(self.adapter_props.Get(ADAPTER, "Discovering"))
                except dbus.DBusException:
                    scanning = False
                log("STATE",
                    f"alive: scanning={scanning}, hearing {heard} other "
                    f"device(s), remote still silent")
                if not scanning:
                    log("STATE", "discovery had stopped -- restarting it")
                    self.start_discovery()

                # Hearing nothing at all while supposedly scanning means the
                # controller is wedged: in a normal home there are always some
                # BLE advertisers. Three consecutive silent minutes is well
                # past plausible.
                if heard == 0:
                    self.deaf += 1
                    if self.deaf >= 3:
                        self.reset_adapter(
                            f"heard nothing for {self.deaf} minutes while "
                            f"scanning={scanning}")
                else:
                    self.deaf = 0
            time.sleep(3)

    def ensure_paired(self, path, addr, d):
        if bool(d.get("Paired", False)):
            return True
        log("PAIR", f"pairing {addr}")
        dev = dbus.Interface(self.bus.get_object(BLUEZ, path), DEV)
        props = dbus.Interface(self.bus.get_object(BLUEZ, path), PROPS)
        try:
            dev.Pair(timeout=60)
            log("PAIR", "paired")
        except dbus.DBusException as e:
            if "AlreadyExists" in e.get_dbus_name():
                log("PAIR", "already bonded")
            else:
                log("PAIR", f"pair failed: {e.get_dbus_message()}")
                return False
        try:
            props.Set(DEV, "Trusted", dbus.Boolean(True))
        except dbus.DBusException:
            pass
        return True

    def release_from_bluez(self, path, addr):
        """BlueZ must not hold the ATT bearer, and must not auto-reconnect."""
        try:
            props = dbus.Interface(self.bus.get_object(BLUEZ, path), PROPS)
            props.Set(DEV, "Trusted", dbus.Boolean(False))
        except dbus.DBusException:
            pass
        try:
            dbus.Interface(self.bus.get_object(BLUEZ, path), DEV).Disconnect()
        except dbus.DBusException:
            pass
        self.stop_discovery()
        time.sleep(2)

    def note_failure(self, addr):
        self.failures[addr] = self.failures.get(addr, 0) + 1
        self.failed_at[addr] = time.time()
        if self.failures[addr] >= 4:
            log("STATE", f"{addr} failed {self.failures[addr]}x -- "
                         f"ignoring it for 15 minutes")

    def run_link(self, addr, addr_type=BDADDR_LE_PUBLIC, timeout=60):
        """Raw ATT: handshake, then stream every event until the link drops."""
        remote = SiriRemote(addr, addr_type=addr_type,
                            sec_level=BT_SECURITY_MEDIUM)
        log("LINK", "opening raw ATT channel (bypassing BlueZ)")

        def on_retry(attempt, err, left):
            if attempt % 10 == 0:
                log("LINK", f"still armed and waiting ({err}), {left}s left "
                            f"-- press a button on the remote")

        if not remote.connect(timeout=timeout, on_retry=on_retry):
            log("LINK", f"no ATT channel as "
                        f"{'random' if addr_type == 2 else 'public'}: "
                        f"{remote.last_error}")
            return False
        log("LINK", "ATT channel open")

        try:
            remote.handshake(log=lambda m: log("LINK", m))
            log("LINK", "HANDSHAKE ACCEPTED -- press buttons or hold Siri")
        except ATTError as e:
            log("LINK", f"handshake FAILED: {e}")
            remote.close()
            return False

        held = set()
        self.voice_reset()
        audio_logged = 0

        try:
            for handle, data in remote.notifications():
                if handle == proto.H_BATTERY:
                    log("BATTERY", f"{data[0]}%")
                    continue
                if handle == proto.H_POWER:
                    state = {proto.POWER_CHARGING: "charging",
                             proto.POWER_DISCHARGING: "discharging",
                             proto.POWER_PLUGGED_IN: "plugged in"
                             }.get(data[0], f"0x{data[0]:02X}")
                    log("BATTERY", f"power: {state}")
                    continue
                if handle != proto.H_INPUT:
                    continue

                ev = proto.parse(data)

                if ev["kind"] == "audio":
                    # Log the first few raw frames so live framing can be
                    # checked against the captures we validated against.
                    if audio_logged < 3:
                        audio_logged += 1
                        log("VOICE", f"frame {self.voice_frames + 1}: "
                                     f"{ev['length']} bytes, opus offset="
                                     f"{ev['opus_offset']} len="
                                     f"{len(ev['opus']) if ev['opus'] else 0} "
                                     f"raw={ev['raw'][:16].hex(' ')}...")
                    self.voice_feed(ev["opus"])
                    continue

                if ev["kind"] == "touch":
                    pts = " ".join(f"({x},{y},p{p})" for x, y, p in ev["fingers"])
                    log("TOUCH", f"{len(ev['fingers'])} finger(s) {pts}"
                                 f"{' PRESSED' if ev['pressed'] else ''}")

                now = ev.get("buttons", set())
                for b in sorted(now - held):
                    log("BUTTON", f"DOWN {b}")
                    if b == "SIRI":
                        self.voice_start()
                        audio_logged = 0
                for b in sorted(held - now):
                    log("BUTTON", f"UP   {b}")
                    if b == "SIRI":
                        self.voice_stop()
                held = now
        except (ATTError, OSError) as e:
            log("LINK", f"link lost: {e}")
            if self.voice_frames:
                self.voice_stop()
        finally:
            remote.close()
        return True

    # --- voice session ----------------------------------------------------
    def voice_reset(self):
        self.voice_pcm = bytearray()
        self.voice_frames = 0
        self.voice_bad = 0
        self.voice_started = None
        self.voice_dec = None
        self.voice_stt = None

    def voice_start(self):
        self.voice_reset()
        self.voice_started = time.time()
        try:
            self.voice_dec = OpusDecoder(proto.AUDIO_SAMPLE_RATE,
                                         proto.AUDIO_CHANNELS)
        except Exception as e:
            log("VOICE", f"opus decoder unavailable: {e}")
        if transcribe.available():
            try:
                self.voice_stt = transcribe.StreamingTranscriber(
                    proto.AUDIO_SAMPLE_RATE)
            except Exception as e:
                log("VOICE", f"transcriber unavailable: {e}")
        log("VOICE", "microphone session started"
                     + (" (streaming STT on)" if self.voice_stt else ""))

    def voice_feed(self, opus):
        """Decode one frame and stream it into the recogniser immediately."""
        if not opus or not self.voice_dec:
            return
        self.voice_frames += 1
        try:
            pcm, _ = self.voice_dec.decode(opus)
        except Exception:
            self.voice_bad += 1
            return
        self.voice_pcm += pcm
        if self.voice_stt:
            try:
                done = self.voice_stt.feed(pcm)
                if done:
                    log("VOICE", f"utterance: {done!r}")
            except Exception as e:
                log("VOICE", f"stt feed failed: {e}")
                self.voice_stt = None

    def voice_stop(self):
        held = time.time() - self.voice_started if self.voice_started else 0
        secs = len(self.voice_pcm) / 2 / proto.AUDIO_SAMPLE_RATE

        transcript = ""
        if self.voice_stt:
            try:
                transcript = self.voice_stt.final()
            except Exception as e:
                log("VOICE", f"stt final failed: {e}")

        path = ""
        if self.voice_pcm:
            try:
                os.makedirs(AUDIO_DIR, exist_ok=True)
                path = os.path.join(AUDIO_DIR,
                                    time.strftime("siri-%Y%m%d-%H%M%S.wav"))
                write_wav(path, bytes(self.voice_pcm),
                          proto.AUDIO_SAMPLE_RATE, proto.AUDIO_CHANNELS)
            except Exception as e:
                log("VOICE", f"wav write failed: {e}")

        log("VOICE", f"session ended: {self.voice_frames} frames "
                     f"({self.voice_bad} undecodable), {secs:.2f}s audio, "
                     f"held {held:.1f}s"
                     + (f" -> {path}" if path else ""))
        if transcript:
            log("VOICE", f"TRANSCRIPT: {transcript!r}")
        elif self.voice_frames:
            log("VOICE", "no speech recognised")

        if self.voice_dec:
            self.voice_dec.close()
        self.voice_reset()

    def run(self):
        log("STATE", "daemon started")
        self.voice_reset()
        if transcribe.available():
            log("STATE", "vosk model found -- preloading in background")

            def preload():
                try:
                    t0 = time.time()
                    transcribe.StreamingTranscriber(proto.AUDIO_SAMPLE_RATE)
                    log("STATE", f"vosk model ready ({time.time() - t0:.1f}s)")
                except Exception as e:
                    log("STATE", f"vosk preload failed: {e}")

            threading.Thread(target=preload, daemon=True).start()
        else:
            log("STATE", "vosk not available -- audio will be saved as WAV only")
        self.power_on()
        self.register_agent()
        beat = 0
        while True:
            try:
                if self.bonded_addr:
                    # Already bonded: do NOT go back through discovery. The
                    # remote only advertises for a second or two after a button
                    # press, and pairing/disconnecting burns exactly that
                    # window. Instead keep a raw ATT connect pending so the
                    # kernel completes it the moment the remote wakes.
                    log("LINK", f"waiting for {self.bonded_addr} to wake "
                                f"(press any button -- connect stays armed)")
                    self.stop_discovery()
                    self.run_link(self.bonded_addr, self.bonded_type,
                                  timeout=900)
                    time.sleep(2)
                    continue

                path, addr, d = self.wait_for_remote()
                atype = self.addr_type_of(d)
                if not self.ensure_paired(path, addr, d):
                    self.note_failure(addr)
                    time.sleep(5)
                    continue
                self.bonded_addr, self.bonded_type = addr, atype
                log("PAIR", f"bonded to {addr}; from now on we wait on a "
                            f"pending ATT connect instead of re-discovering")
                self.release_from_bluez(path, addr)
                if self.run_link(addr, atype, timeout=900):
                    self.failures[addr] = 0
                else:
                    self.note_failure(addr)
                log("STATE", "cycling -- will wait for the remote again")
                time.sleep(3)
            except dbus.DBusException as e:
                log("STATE", f"dbus error: {e.get_dbus_message()}")
                time.sleep(5)
            except Exception:
                log("STATE", "unexpected error:\n" + traceback.format_exc())
                time.sleep(5)
            beat += 1
            if beat % 20 == 0:
                log("STATE", "still alive, still waiting")


if __name__ == "__main__":
    Daemon().run()
