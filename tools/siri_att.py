#!/usr/bin/env python3
"""Raw ATT transport for the Apple TV Siri Remote (1st gen, A1513).

Why raw ATT rather than BlueZ's D-Bus GATT API: the remote needs a vendor
handshake (write 0xAF to handle 0x001d) before it emits anything but zeros,
and BlueZ refuses writes to HID Report characteristics locally -- verified
with btmon, the packet never reaches the air. So we open our own L2CAP/ATT
channel. Bonding still comes from BlueZ; the kernel supplies the LTK when we
ask for an encrypted link.

This module is transport only. Payload parsing lives in siri_protocol.py.
"""
import errno
import os
import select
import socket
import struct
import time
from ctypes import CDLL, c_int, c_void_p, get_errno

from siri_protocol import (
    H_ENABLE, H_INPUT_CCCD, H_BATTERY_CCCD, H_POWER_CCCD, H_VENDOR_CCCD,
    ENABLE_BYTE, MIN_MTU,
)

# --- L2CAP / ATT constants ------------------------------------------------
AF_BLUETOOTH = 31
BTPROTO_L2CAP = 0
SOL_BLUETOOTH = 274
BT_SECURITY = 4
BT_SNDMTU = 12
BT_RCVMTU = 13
BT_SECURITY_LOW = 1
BT_SECURITY_MEDIUM = 2

ATT_CID = 4
BDADDR_LE_PUBLIC = 1
BDADDR_LE_RANDOM = 2

OP_ERROR_RSP = 0x01
OP_MTU_REQ = 0x02
OP_MTU_RSP = 0x03
OP_READ_REQ = 0x0A
OP_READ_RSP = 0x0B
OP_WRITE_REQ = 0x12
OP_WRITE_RSP = 0x13
OP_HANDLE_NOTIFY = 0x1B

ATT_ERRORS = {
    0x01: "Invalid Handle",
    0x02: "Read Not Permitted",
    0x03: "Write Not Permitted",
    0x05: "Insufficient Authentication",
    0x08: "Insufficient Authorization",
    0x0D: "Invalid Attribute Value Length",
    0x0E: "Unlikely Error",
    0x0F: "Insufficient Encryption",
}

libc = CDLL("libc.so.6", use_errno=True)
libc.bind.argtypes = [c_int, c_void_p, c_int]
libc.connect.argtypes = [c_int, c_void_p, c_int]


def sockaddr_l2(addr_str, cid, addr_type):
    """Pack struct sockaddr_l2 (BD_ADDR is stored byte-reversed)."""
    raw = bytes(int(b, 16) for b in reversed(addr_str.split(":")))
    return struct.pack("<HH6sHB", AF_BLUETOOTH, 0, raw, cid, addr_type) + b"\x00"


class ATTError(Exception):
    pass


class SiriRemote:
    def __init__(self, address, adapter="00:00:00:00:00:00",
                 addr_type=BDADDR_LE_PUBLIC, sec_level=BT_SECURITY_MEDIUM):
        self.address = address.upper()
        self.adapter = adapter
        self.addr_type = addr_type
        self.sec_level = sec_level
        self.sock = None
        self.mtu = 23
        self.want_mtu = 517
        self.last_error = None
        self.last_mtu_error = None
        self.mtu_sockopt_error = None

    # --- link ------------------------------------------------------------
    def _connect_once(self, wait):
        s = socket.socket(AF_BLUETOOTH, socket.SOCK_SEQPACKET, BTPROTO_L2CAP)
        try:
            sa = sockaddr_l2(self.adapter, ATT_CID, self.addr_type)
            if libc.bind(s.fileno(), sa, len(sa)) != 0:
                err = get_errno()
                raise OSError(err, f"bind failed: {os.strerror(err)}")

            s.setsockopt(SOL_BLUETOOTH, BT_SECURITY,
                         struct.pack("BB", self.sec_level, 0))

            # An L2CAP socket defaults to a 23-byte MTU, so the kernel would
            # truncate the remote's 101-byte audio notifications no matter what
            # ATT negotiates. These must be set BEFORE connect(). This is very
            # likely the real reason the published projects could never capture
            # microphone audio and blamed "bluez can't handle more than 20
            # bytes".
            for opt, name in ((BT_RCVMTU, "BT_RCVMTU"), (BT_SNDMTU, "BT_SNDMTU")):
                try:
                    s.setsockopt(SOL_BLUETOOTH, opt,
                                 struct.pack("<H", self.want_mtu))
                except OSError as e:
                    self.mtu_sockopt_error = f"{name}: {e}"

            # A BLE link cannot complete synchronously, so connect() returns
            # EINPROGRESS and we wait for writability.
            s.setblocking(False)
            da = sockaddr_l2(self.address, ATT_CID, self.addr_type)
            if libc.connect(s.fileno(), da, len(da)) != 0:
                err = get_errno()
                if err not in (errno.EINPROGRESS, errno.EALREADY):
                    raise OSError(err, os.strerror(err))
                _, writable, _ = select.select([], [s], [], wait)
                if not writable:
                    raise OSError(errno.ETIMEDOUT, "still pending")
                so_err = s.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)
                if so_err != 0:
                    raise OSError(so_err, os.strerror(so_err))
            s.setblocking(True)
            s.settimeout(20)
            self.sock = s
            return True
        except OSError as e:
            s.close()
            self.last_error = e
            return False

    def connect(self, timeout=600, on_retry=None):
        """Keep a connect pending until the remote advertises.

        The remote is only connectable for a second or two after a button
        press, and the kernel abandons a pending LE connect after roughly 40s
        with ECONNREFUSED -- so re-arm and stay pending nearly all the time.
        """
        deadline = time.time() + timeout
        attempt = 0
        while time.time() < deadline:
            attempt += 1
            started = time.time()
            if self._connect_once(min(60, deadline - time.time())):
                return True
            # Instant failures (EBUSY from a stale link, EHOSTUNREACH with no
            # keys) mean the kernel rejected outright; back off rather than spin.
            if time.time() - started < 1.0:
                time.sleep(3)
            if on_retry:
                on_retry(attempt, self.last_error, int(deadline - time.time()))
        return False

    def close(self):
        if self.sock:
            try:
                self.sock.close()
            except OSError:
                pass
            self.sock = None

    # --- ATT primitives ---------------------------------------------------
    def _recv(self):
        return self.sock.recv(max(self.mtu, 512))

    def _txn(self, pdu, expect):
        self.sock.send(pdu)
        while True:
            rsp = self._recv()
            if not rsp:
                raise ATTError("link closed")
            op = rsp[0]
            if op == OP_HANDLE_NOTIFY:
                continue          # notification racing our request
            if op == OP_ERROR_RSP:
                req, handle, err = rsp[1], struct.unpack("<H", rsp[2:4])[0], rsp[4]
                raise ATTError(
                    f"op 0x{req:02x} handle 0x{handle:04x}: "
                    f"{ATT_ERRORS.get(err, hex(err))}")
            if op == expect:
                return rsp
            raise ATTError(f"unexpected opcode 0x{op:02x}")

    def exchange_mtu(self, mtu=517, log=None):
        """Negotiate a larger ATT MTU.

        This must succeed for microphone audio: frames are 101 bytes and the
        default 23-byte MTU truncates them. The reference implementation asks
        for exactly 104. Some peripherals reject an unusually large request,
        so fall back through smaller values rather than silently staying at 23.
        """
        self.last_mtu_error = None
        # Ask for exactly MIN_MTU (104) first: that is the value the known
        # working implementation uses, and some stacks clamp oddly on large
        # requests. Only one Exchange MTU is permitted per connection, so the
        # first answer is the one we live with.
        for want in (MIN_MTU, mtu, 185):
            try:
                rsp = self._txn(struct.pack("<BH", OP_MTU_REQ, want), OP_MTU_RSP)
                theirs = struct.unpack("<H", rsp[1:3])[0]
                self.mtu = max(23, min(want, theirs))
                if log:
                    log(f"MTU exchange: asked {want}, peer said {theirs} "
                        f"(raw {rsp[:3].hex(' ')}), using {self.mtu}")
                return self.mtu
            except (ATTError, OSError) as e:
                self.last_mtu_error = e
                if log:
                    log(f"MTU exchange at {want} failed: {e}")
                # An MTU exchange may only be issued once per connection; if
                # the peer errored on the request itself, retrying is futile.
                if isinstance(e, ATTError) and "Request Not Supported" in str(e):
                    break
        self.mtu = 23
        return self.mtu

    def write_req(self, handle, value):
        self._txn(struct.pack("<BH", OP_WRITE_REQ, handle) + value, OP_WRITE_RSP)

    def read_req(self, handle):
        return self._txn(struct.pack("<BH", OP_READ_REQ, handle), OP_READ_RSP)[1:]

    # --- protocol ---------------------------------------------------------
    def handshake(self, log=print):
        """Unlock the input stream and subscribe to everything.

        Order matters: the MTU must be raised before audio arrives, or the
        101-byte microphone frames cannot fit and get dropped. That is exactly
        why the published implementations never managed to capture voice.
        """
        if self.mtu_sockopt_error:
            log(f"socket MTU option failed: {self.mtu_sockopt_error}")
        try:
            granted = struct.unpack(
                "<H", self.sock.getsockopt(SOL_BLUETOOTH, BT_RCVMTU, 2))[0]
            log(f"kernel socket receive MTU: {granted}")
        except OSError as e:
            log(f"could not read BT_RCVMTU: {e}")

        # Unlock FIRST, then negotiate the MTU. The remote answers 23 to an
        # Exchange MTU sent before the 0xAF handshake; it may be in a
        # restricted mode until unlocked. Only one Exchange MTU is allowed per
        # connection, so it is worth spending it after the unlock.
        self.write_req(H_ENABLE, bytes([ENABLE_BYTE]))
        log(f"0x{ENABLE_BYTE:02X} -> 0x{H_ENABLE:04X} accepted")

        mtu = self.exchange_mtu(log=log)
        if mtu >= MIN_MTU:
            log(f"MTU {mtu} -- large enough for microphone audio")
        else:
            log(f"MTU {mtu}: buttons and touch work, but microphone audio "
                f"needs >= {MIN_MTU}. This controller is Bluetooth 4.1 with no "
                f"Data Length Extension; the remote will not grant more. A "
                f"BT 4.2+/5.0 USB adapter is required for voice.")

        for cccd, what in ((H_INPUT_CCCD, "input"),
                           (H_BATTERY_CCCD, "battery"),
                           (H_POWER_CCCD, "power"),
                           (H_VENDOR_CCCD, "apple vendor channel")):
            try:
                self.write_req(cccd, b"\x01\x00")
                log(f"notifications on for {what} (CCCD 0x{cccd:04X})")
            except ATTError as e:
                log(f"could not subscribe {what}: {e}")
        return mtu

    def notifications(self):
        """Yield (handle, payload) for every notification until the link drops."""
        self.sock.settimeout(None)
        while True:
            pkt = self._recv()
            if not pkt:
                raise ATTError("link closed")
            if pkt[0] == OP_MTU_REQ and len(pkt) >= 3:
                theirs = struct.unpack("<H", pkt[1:3])[0]
                try:
                    self.sock.send(struct.pack("<BH", OP_MTU_RSP, self.want_mtu))
                    self.mtu = max(23, min(self.want_mtu, theirs))
                except OSError:
                    pass
                continue
            if pkt[0] != OP_HANDLE_NOTIFY or len(pkt) < 3:
                continue
            yield struct.unpack("<H", pkt[1:3])[0], pkt[3:]
