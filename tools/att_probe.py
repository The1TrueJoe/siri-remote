#!/usr/bin/env python3
"""Probe an ATT link to a Siri Remote, with the security level selectable.

Purpose: the dongle's firmware is flagged by the kernel as having broken SMP
("WRITE AUTH PAYLOAD TIMEOUT ... leads to unexpected SMP errors"). Links drop
immediately after connecting, which is when encryption is set up. If an
unencrypted link survives, SMP is the broken part and the dongle is still
usable — the published projects never requested encryption either.

  sudo att_probe.py DC:2B:2A:E0:18:71 --adapter F4:4E:FC:58:72:95 --sec low
"""
import argparse
import errno
import os
import select
import socket
import struct
import sys
import time
from ctypes import CDLL, c_int, c_void_p, get_errno

AF_BLUETOOTH, BTPROTO_L2CAP = 31, 0
SOL_BLUETOOTH, BT_SECURITY = 274, 4
BT_SNDMTU, BT_RCVMTU = 12, 13
ATT_CID = 4
SEC = {"none": 1, "low": 1, "medium": 2, "high": 3}

H_ENABLE, H_INPUT_CCCD, H_INPUT = 0x001D, 0x0024, 0x0023
# Feature reports. The Report Map declares report 240 as 208 bytes of
# "Data, Variable, Buffered Bytes" -- big enough for a 101-byte audio frame,
# and readable via Read Blob regardless of the 23-byte ATT MTU.
H_REPORT_MAP = 0x0019
# Apple vendor service 8341f2b4-c013-4f04-8197-c4cdb42e26dc. Three read-only
# characteristics plus one read/write/notify -- the shape of a challenge and
# response. Nothing has ever been read from or written to these.
H_VENDOR = {
    0x0034: "9fbf120d (read)",
    0x0036: "2bdcaebe..b7 (read)",
    0x0038: "2bdcaebe..b8 (read)",
    0x003A: "30e69638 (read/write/notify)",
}
H_FEATURE_240 = 0x001D
H_FEATURE_241 = 0x0020
ENABLE_BYTE = 0xAF

# Every CCCD we know of. Voice-capable HID remotes commonly gate the audio
# stream on having *all* relevant notifications enabled, not just the report
# characteristic, so subscribe to everything rather than guessing.
CCCDS = {
    0x0024: "input report",
    0x0029: "battery",
    0x002C: "power state",
    0x003B: "apple vendor channel",
}

libc = CDLL("libc.so.6", use_errno=True)
libc.bind.argtypes = [c_int, c_void_p, c_int]
libc.connect.argtypes = [c_int, c_void_p, c_int]


def sockaddr_l2(addr, cid, atype):
    raw = bytes(int(b, 16) for b in reversed(addr.split(":")))
    return struct.pack("<HH6sHB", AF_BLUETOOTH, 0, raw, cid, atype) + b"\x00"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("address")
    ap.add_argument("--adapter", default="00:00:00:00:00:00",
                    help="local adapter BD address to bind to")
    ap.add_argument("--sec", choices=SEC, default="medium")
    ap.add_argument("--mtu", type=int, default=517)
    ap.add_argument("--timeout", type=int, default=300)
    ap.add_argument("--mtu-first", action="store_true",
                    help="exchange MTU before the 0xAF unlock")
    ap.add_argument("--never-ask", action="store_true",
                    help="never send our own Exchange MTU Request. The spec "
                         "allows only one per connection, so asking consumes "
                         "it and locks the link at 23 -- which stops the "
                         "remote raising it later when audio needs the room.")
    ap.add_argument("--wait-mtu", type=float, default=0.0,
                    help="seconds to wait for the REMOTE to initiate the MTU "
                         "exchange before asking ourselves. The remote offers "
                         "Client RX MTU 104 when it asks first; asking first "
                         "gets its Server RX MTU of 23 instead.")
    ap.add_argument("--sweep", default="",
                    help="comma-separated MTU values to request, one per "
                         "connection (only one exchange is allowed per "
                         "connection). Stops at the first value the remote "
                         "grants >= 104 and stays connected for a Siri test.")
    ap.add_argument("--info", action="store_true",
                    help="read Device Information before anything else")
    ap.add_argument("--stream-on-siri", action="store_true",
                    help="when Siri goes down, write feature report 0x98=01 "
                         "(EVT_ENABLE_AUDIO_STREAMING). Siri alone only arms "
                         "the mic; the host must then start the stream.")
    ap.add_argument("--stream-handle", type=lambda v: int(v, 0),
                    default=H_ENABLE,
                    help="keyhole handle for the 0x98 write (default 0x001d)")
    args = ap.parse_args()

    print(f"adapter {args.adapter} -> {args.address}, security={args.sec}")
    print("press a button on the remote\n", flush=True)

    sweep = [int(v) for v in args.sweep.split(",") if v.strip()]
    results = []
    if sweep:
        print(f"sweeping MTU requests: {sweep} -- one per connection, so "
              f"press a button each time it asks\n", flush=True)

    deadline = time.time() + args.timeout
    attempt = 0
    while time.time() < deadline:
        attempt += 1
        if sweep:
            args.mtu = sweep[len(results)]
        s = socket.socket(AF_BLUETOOTH, socket.SOCK_SEQPACKET, BTPROTO_L2CAP)
        try:
            sa = sockaddr_l2(args.adapter, ATT_CID, 1)
            if libc.bind(s.fileno(), sa, len(sa)) != 0:
                e = get_errno()
                raise OSError(e, f"bind: {os.strerror(e)}")

            s.setsockopt(SOL_BLUETOOTH, BT_SECURITY,
                         struct.pack("BB", SEC[args.sec], 0))
            for opt in (BT_RCVMTU, BT_SNDMTU):
                try:
                    s.setsockopt(SOL_BLUETOOTH, opt, struct.pack("<H", args.mtu))
                except OSError:
                    pass

            s.setblocking(False)
            da = sockaddr_l2(args.address, ATT_CID, 1)
            if libc.connect(s.fileno(), da, len(da)) != 0:
                e = get_errno()
                if e not in (errno.EINPROGRESS, errno.EALREADY):
                    raise OSError(e, os.strerror(e))
                _, w, _ = select.select([], [s], [], min(45, deadline - time.time()))
                if not w:
                    raise OSError(errno.ETIMEDOUT, "still pending")
                err = s.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)
                if err:
                    raise OSError(err, os.strerror(err))
            s.setblocking(True)
            s.settimeout(15)
            print(f"*** ATT CHANNEL OPEN (attempt {attempt}) ***", flush=True)
            outcome = probe(s, args)
            if sweep and outcome == "next":
                results.append((args.mtu, args.last_mtu))
                print(f"\n=== SWEEP: asked {args.mtu} -> remote granted "
                      f"{args.last_mtu} ===", flush=True)
                s.close()
                if len(results) >= len(sweep):
                    print("\nsweep complete:", flush=True)
                    for asked, got in results:
                        print(f"  asked {asked:>4} -> granted {got}", flush=True)
                    return 0
                print(f"next: {sweep[len(results)]} -- press a button "
                      f"on the remote\n", flush=True)
                time.sleep(3)  # let the link drop before re-arming
                continue
            return 0
        except OSError as e:
            s.close()
            name = errno.errorcode.get(e.args[0], e.args[0])
            if attempt % 4 == 0:
                print(f"  attempt {attempt}: {name} "
                      f"({int(deadline - time.time())}s left)", flush=True)
            time.sleep(1 if e.args[0] == errno.ETIMEDOUT else 2)
    print("never connected")
    return 1


def txn(s, pdu, expect):
    s.send(pdu)
    while True:
        r = s.recv(1024)
        if not r:
            raise OSError("link closed")
        if r[0] == 0x1B:      # notification racing our request
            continue
        if r[0] == 0x01:      # error response
            return None, r
        if r[0] == expect:
            return r, None
        return None, r


def read_info(s):
    """Device Information service, to identify the unit precisely."""
    for handle, label in ((0x000B, "serial"), (0x000D, "hardware rev"),
                          (0x000F, "firmware rev"), (0x0011, "manufacturer"),
                          (0x0013, "model"), (0x0016, "HID info")):
        rsp, err = txn(s, struct.pack("<BH", 0x0A, handle), 0x0B)
        if rsp:
            raw = rsp[1:]
            text = raw.decode("utf-8", "replace").strip("\x00")
            printable = text if text.isprintable() and text else raw.hex(" ")
            print(f"  {label:<14} {printable}")
        else:
            print(f"  {label:<14} (unreadable)")


def await_peer_mtu(s, seconds, our_mtu):
    """Let the remote initiate the MTU exchange, and answer it.

    ATT MTU is asymmetric in practice here: asking the remote yields its
    *Server* RX MTU (23 - it receives almost nothing), while the remote
    announces a *Client* RX MTU of 104 when it initiates. The connection MTU is
    the minimum of the two sides' declared values, so letting it ask first is
    what unlocks large notifications - and audio frames are 101 bytes.
    """
    deadline = time.time() + seconds
    pending = []
    while time.time() < deadline:
        s.settimeout(max(0.2, deadline - time.time()))
        try:
            pkt = s.recv(1024)
        except (socket.timeout, TimeoutError):
            break
        if not pkt:
            break
        if pkt[0] == 0x02 and len(pkt) >= 3:
            theirs = struct.unpack("<H", pkt[1:3])[0]
            s.send(struct.pack("<BH", 0x03, our_mtu))
            mtu = max(23, min(theirs, our_mtu))
            print(f"*** REMOTE initiated MTU exchange: it offered {theirs}, "
                  f"we answered {our_mtu} -> ATT_MTU {mtu} ***", flush=True)
            return mtu, pending
        pending.append(pkt)
    return None, pending


def read_long(s, handle, limit=512):
    """Read a characteristic of any length.

    ATT caps a Read Response at MTU-1, so long values continue with Read Blob
    at successive offsets. This is how a 208-byte feature report can be
    retrieved over a 23-byte MTU -- which is why a small MTU need not prevent
    audio from flowing, if audio is polled rather than notified.
    """
    out = bytearray()
    rsp, err = txn(s, struct.pack("<BH", 0x0A, handle), 0x0B)
    if not rsp:
        return None, err
    out += rsp[1:]

    while len(out) < limit:
        rsp, err = txn(s, struct.pack("<BHH", 0x0C, handle, len(out)), 0x0D)
        if not rsp or len(rsp) <= 1:
            break
        out += rsp[1:]
        # A short blob means we reached the end of the value.
        if len(rsp) - 1 < 22:
            break
    return bytes(out), None


def poll_feature_reports(s, seconds, label):
    """Hammer the feature reports and report anything that is not all zeros."""
    deadline = time.time() + seconds
    seen = {}
    nonzero = 0
    while time.time() < deadline:
        for handle in (H_FEATURE_240, H_FEATURE_241):
            data, err = read_long(s, handle)
            if data is None:
                continue
            seen[handle] = seen.get(handle, 0) + 1
            if any(data):
                nonzero += 1
                print(f"  [{label}] h{handle:04X} {len(data):3d} bytes "
                      f"{data[:32].hex(' ')}", flush=True)
                if len(data) > 40 and any(is_opus_toc(b) for b in data[:8]):
                    print(f"      *** looks like an Opus frame ***", flush=True)
    counts = ", ".join(f"h{h:04X}x{n}" for h, n in seen.items())
    print(f"  [{label}] polled {counts}; {nonzero} non-zero", flush=True)


def is_opus_toc(b):
    return (b >> 3) == 23


def exchange_mtu(s, want):
    rsp, err = txn(s, struct.pack("<BH", 0x02, want), 0x03)
    mtu = struct.unpack("<H", rsp[1:3])[0] if rsp else 23
    print(f"MTU exchange: asked {want}, peer said {mtu}")
    return mtu


def unlock(s):
    rsp, err = txn(s, struct.pack("<BH", 0x12, H_ENABLE) + bytes([ENABLE_BYTE]), 0x13)
    print("0xAF -> 0x001D:", "ACCEPTED" if rsp else f"failed {err.hex(' ')}", flush=True)


def probe(s, args):
    granted = struct.unpack("<H", s.getsockopt(SOL_BLUETOOTH, BT_RCVMTU, 2))[0]
    print(f"kernel socket receive MTU: {granted}")

    if args.info:
        print("device information:")
        read_info(s)

    mtu = None
    pending = []
    if args.wait_mtu > 0:
        print(f"waiting up to {args.wait_mtu}s for the remote to initiate "
              f"the MTU exchange...", flush=True)
        mtu, pending = await_peer_mtu(s, args.wait_mtu, args.mtu)
        if mtu is None:
            print("  remote did not initiate; asking ourselves", flush=True)

    if args.never_ask:
        unlock(s)
        if mtu is None:
            mtu = 23
            print("not asking for an MTU: leaving the exchange unused so the "
                  "remote can raise it whenever it wants", flush=True)
    elif args.mtu_first and mtu is None:
        mtu = exchange_mtu(s, args.mtu)
        unlock(s)
    else:
        unlock(s)
        if mtu is None:
            mtu = exchange_mtu(s, args.mtu)
    print("  ***", "LARGE ENOUGH FOR AUDIO" if mtu >= 104 else
          "too small for audio (needs >= 104)", "***", flush=True)

    args.last_mtu = mtu
    if args.sweep and mtu < 104:
        return "next"

    for handle, label in CCCDS.items():
        rsp, err = txn(s, struct.pack("<BH", 0x12, handle) + b"\x01\x00", 0x13)
        print(f"  CCCD 0x{handle:04X} {label:<22} "
              f"{'on' if rsp else 'refused'}", flush=True)
    print("", flush=True)
    print("\nnotifications on -- press buttons, hold Siri\n", flush=True)

    s.settimeout(None)
    # Anything received while waiting for the peer's MTU request still counts.
    for pkt in pending:
        if pkt and pkt[0] == 0x1B:
            print(f"  (buffered) {len(pkt) - 3} bytes", flush=True)

    state = {"mtu": mtu, "our_mtu": args.mtu}
    sizes = {}
    siri_held = False
    start = time.time()
    while True:
        pkt = s.recv(max(state["mtu"], args.mtu, 1024))
        if not pkt:
            continue

        # The remote may ask for a bigger MTU only when it actually needs one
        # -- i.e. the moment the microphone starts. Ignoring that request here
        # would leave it waiting forever and no audio would ever arrive, so
        # answer it whenever it turns up, not just during connection setup.
        if pkt[0] == 0x02 and len(pkt) >= 3:
            theirs = struct.unpack("<H", pkt[1:3])[0]
            s.send(struct.pack("<BH", 0x03, state["our_mtu"]))
            state["mtu"] = max(23, min(theirs, state["our_mtu"]))
            print(f"\n*** REMOTE ASKED FOR MTU {theirs} mid-session -> "
                  f"ATT_MTU now {state['mtu']} ***\n", flush=True)
            continue

        if pkt[0] != 0x1B:
            print(f"[{time.time()-start:6.1f}s] other ATT opcode "
                  f"0x{pkt[0]:02x}: {pkt[:8].hex(' ')}", flush=True)
            continue
        handle = struct.unpack("<H", pkt[1:3])[0]
        data = pkt[3:]
        key = (handle, len(data))
        sizes[key] = sizes.get(key, 0) + 1
        # When Siri goes down, poll the feature reports hard: if audio is
        # fetched rather than pushed, this is where it will show up.
        if handle == H_INPUT and len(data) == 2 and data[1] & 0x10:
            print("\n*** SIRI DOWN -- listening for audio ***\n", flush=True)

        # Firmware state machine: Siri down only arms the mic (CONNECTED ->
        # CONNECTED_WITH_AUDIO); report 0x98 != 0 from the host moves it to
        # STREAMING. Fire once per press. Write Request, so the remote's
        # answer (0x13 or an error) shows up below as an "other opcode".
        if handle == H_INPUT and len(data) in (2, 13, 20):
            down = bool(data[1] & 0x10)
            if down and not siri_held and args.stream_on_siri:
                s.send(struct.pack("<BH", 0x12, args.stream_handle) + b"\x98\x01")
                print(f"  -> wrote 98 01 to 0x{args.stream_handle:04X}",
                      flush=True)
            siri_held = down

        if handle != H_INPUT:
            kind = "VENDOR"
        else:
            kind = ("buttons" if len(data) == 2 else
                    "touch" if len(data) in (13, 20) else
                    "AUDIO" if len(data) > 40 else "?")
        print(f"[{time.time()-start:6.1f}s] h{handle:04X} {len(data):3d} bytes  "
              f"{kind:8} {data[:12].hex(' ')}", flush=True)
        if len(data) > 40:
            print(f"           *** AUDIO FRAME *** sizes so far: {sizes}", flush=True)


if __name__ == "__main__":
    sys.exit(main())
