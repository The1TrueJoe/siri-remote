# Siri Remote (1st gen, A1513) -> buttons, touch and voice on Linux

Goal: read the remote programmatically so its input can drive something else
(here: Control4, via a webhook). Not a uinput/keystroke shim.

Hardware: Raspberry Pi 3 Model B, Debian 13 (trixie), BlueZ 5.82,
onboard BCM43438 (`hci0`, `B8:27:EB:0D:D8:3F`).
Remote: `DC:2B:2A:D6:38:61`, name `C08QR0UNGQQT`, Manufacturer `Apple`.

## Status

**Working end to end on a live remote** (`DC:2B:2A:E0:18:71`, `DL4QTTBKGQQT`)
as of 2026-09-07 15:15 UTC:

| Area | State |
|------|-------|
| Pairing / bonding | working |
| `0xAF` handshake over raw ATT | **ACCEPTED on the air** -- the BlueZ bypass works |
| Buttons | **working**: Menu, AirPlay, Play/Pause, Volume Up/Down, Siri, Touchpad |
| Touchpad | **working**: X/Y with pressure, swipes, click + pressed flag |
| Battery / power state | **working** (percent + charging/plugged-in) |
| Opus decode + STT | validated offline (29/29 frames; Vosk 0.99x realtime) |
| Microphone capture | blocked by ATT MTU -- see below |

The original remote (`DC:2B:2A:D6:38:61`, `C08QR0UNGQQT`) went flat and stopped
advertising entirely; a second unit proved the whole stack. Everything here is
device-generic.

### Sample of real output

```
BUTTON   DOWN MENU
BUTTON   DOWN AIRPLAY
BUTTON   DOWN PLAY_PAUSE
BUTTON   DOWN VOLUME_DOWN
TOUCH    1 finger(s) (16,20,p42)
TOUCH    1 finger(s) (97,17,p0)      <- swipe right
TOUCH    1 finger(s) (56,47,p61) PRESSED
BUTTON   DOWN TOUCHPAD
BATTERY  27%
BATTERY  power: plugged in
```

### The remaining gap: microphone audio needs Bluetooth 4.2+

Buttons, touch, and battery all work. Microphone audio does not, and the cause
is now pinned down precisely.

```
kernel socket receive MTU: 517
MTU exchange: asked 104, peer said 23 (raw 03 17 00), using 23
```

Three separate things were checked, in order:

1. **The L2CAP socket receive MTU** defaults to 23, so the kernel discarded
   101-byte notifications before userspace saw them. Fixed by setting
   `BT_RCVMTU` (13) and `BT_SNDMTU` (12) via `setsockopt(SOL_BLUETOOTH, ...)`
   **before** `connect()`. Confirmed granted: 517. Worth doing regardless --
   this is very likely part of the "bluez somehow can't handle more than 20
   bytes" folklore in the published projects.
2. **Handshake ordering.** The remote answers 23 whether the Exchange MTU is
   sent before or after the `0xAF` unlock. Not an unlock-state issue.
3. **The controller is Bluetooth 4.1 and lacks Data Length Extension.**

```
HCI Version: 4.1 (0x7)   LMP Version: 4.1 (0x7)   Broadcom BCM43438
LE Features: 1F 00 00 08 00 00 00 00
             octet 0 = 0b00011111 -> bit 5 (LE Data Packet Length
             Extension) is CLEAR
LE Read Suggested Default Data Length -> status 0x01 (Unknown HCI Command)
```

DLE was introduced in Bluetooth 4.2. The remote grants an ATT MTU of 23 to
this controller, and at MTU 23 the largest possible notification payload is
exactly 20 bytes. That is consistent with everything observed: the documented
payload sizes are 2, 13, 20 and 101 bytes, and we receive **every size that
fits** and miss only the one that does not.

Audio needs MTU >= 104. **The fix is a Bluetooth 4.2+ (ideally 5.0) USB
adapter** -- e.g. an RTL8761B-based dongle, which is well supported by the
mainline Linux `btusb` driver. That also sidesteps the UART entirely, removing
the frame-reassembly problem described below.

Nothing in the software needs to change for this: the Opus decoding and
speech-to-text paths are already validated end to end (29/29 frames, Vosk at
0.99x realtime). Only the transport is short.

## The device does not speak standard HID

`0x2A4B` (Report Map) reads back as 30 bytes:

```
06 00 ff 09 0b a1 01 09 01 85 fc 75 08 95 14 82 03 01
09 02 85 f0 75 08 95 d0 b2 02 01 c0
```

```
06 00 FF    Usage Page (Vendor-Defined 0xFF00)
09 0B       Usage (0x0B)
A1 01       Collection (Application)
09 01         Usage (0x01)
85 FC         Report ID (252)
75 08         Report Size (8 bits)
95 14         Report Count (20)
82 03 01      Input (Constant, Variable, Buffered Bytes)
09 02         Usage (0x02)
85 F0         Report ID (240)
75 08         Report Size (8 bits)
95 D0         Report Count (208)
B2 02 01      Feature (Data, Variable, Buffered Bytes)
C0          End Collection
```

No Keyboard (0x07) or Consumer Control (0x0C) usage page, and the input item
is flagged `Constant` -- "ignore this field" per the HID spec. So BlueZ's `hog`
plugin cannot build a `uhid` node and logs `Attribute can't be read`.

It is a facade, not a wall. Real data flows once the device is unlocked.

## The unlock: write 0xAF to handle 0x001d

Without it the notify characteristic emits only zeros, which is exactly what we
first observed:

```
[  78.93s] report id=252 Input      00 00
[ 118.32s] report id=252 Input      00 00 00 00 00 00 00 00
```

### Handle map (ours matches the reference implementations exactly)

| Handle | Purpose |
|--------|---------|
| `0x001d` | write `0xAF` here to unlock input (Report id 240, Feature) |
| `0x0023` | notifications: buttons, touch **and audio** (Report id 252, Input) |
| `0x0024` | CCCD for the above |
| `0x0028` / `0x0029` | battery level / its CCCD |
| `0x002b` / `0x002c` | power state / its CCCD (`0xAB` charging, `0xAF` discharging, `0xBB` plugged in) |

## One characteristic carries everything; length disambiguates

| Length | Meaning |
|--------|---------|
| 2 bytes | button bitmask only |
| 13 bytes | one finger on the touchpad |
| 20 bytes | two fingers |
| ~101 bytes | **Opus-encoded microphone audio** |

**Buttons** -- bitmask in byte 1, so chords work:

| Bit | Button | Bit | Button |
|-----|--------|-----|--------|
| `0x01` | AirPlay | `0x10` | Siri |
| `0x02` | Volume Up | `0x20` | Menu |
| `0x04` | Volume Down | `0x80` | Touchpad |
| `0x08` | Play/Pause | `0x00` | all released |

A two-finger click arrives as finger-count 2 plus the touchpad bit; we promote
it to a synthetic `0x40 TOUCHPAD_2`.

**Touch** -- `data[2] == 50` marks a touch report; finger records live at
`[6:13]` and `[13:20]`:

```
x = int((f[0] + 255 * (f[1] & 7) - 230) / 15)
y = (f[2] if f[2] & 128 else f[2] + 255) - 188      # wraps: 188..255 then 0..38
p = f[5]
```

Y resolution is noticeably coarser than X.

## Voice

### Format, derived from the Opus TOC byte

Captured frames begin `[len][0xB8 ...]`. `0xB8` is the Opus TOC:

```
0xB8 = 10111 0 00
        |    | +-- c  = 0  -> 1 frame per packet
        |    +---- s  = 0  -> mono
        +--------- config 23 -> CELT-only, wideband (16 kHz), 20 ms frames
```

So: **Opus, 16 kHz, mono, 20 ms frames (320 samples)**, each packet
length-prefixed by the byte before the TOC. Confirmed against the reference
project's WAV output (PCM 16-bit, 1 channel, 16 kHz).

### Why nobody had captured voice before

Both published implementations note "bluez somehow can't handle more than 20
bytes" and never tested audio. That is an **ATT MTU** problem: the default is
23 bytes (20 usable), so 101-byte audio notifications cannot fit. The fix is a
single Exchange MTU at connect time -- the reference uses exactly `104`
(101 payload + 3 ATT header). `tools/siri_att.py` negotiates it before
enabling notifications, which is the ordering that matters.

The Pi 3's BCM43438 is Bluetooth 4.1 with no Data Length Extension, so a
101-byte notification is fragmented across L2CAP and reassembled by the
kernel. That is fine and invisible to us; only the ATT MTU matters.

### Validated without hardware

`tools/test_audio_decode.py` decodes real captured Siri Remote frames:

```
29 captured frame(s)
  frame 1: len=86 toc=0xB8 config=23 -> 320 samples (20 ms)
           extractor found offset=1, 86 bytes -- AGREES
decoded 29/29 frames, 0 failed
total 9280 samples = 0.58s at 16000 Hz
peak amplitude 12233, RMS 1305.7 -- REAL AUDIO
```

The frame extractor locates the Opus TOC and validates it against the
preceding length byte rather than hard-coding an offset, so it is
self-checking against live data.

### Speech-to-text

Vosk, on-device, no network. Its required input is PCM 16 kHz 16-bit mono --
exactly what the remote emits, so there is no resampling anywhere in the path.

Measured on this Pi 3 with `vosk-model-small-en-us-0.15` (68 MB):

```
model load:  5.4s   (preloaded at daemon start, off the hot path)
recognition: 0.99x realtime
transcript:  'one zero zero zero one nine oh two i no zero one eight zero three'  (correct)
```

Frames are fed to the recogniser as they arrive (one per 20 ms), so the
transcript is ready when the Siri button is released rather than after.

## Why BlueZ cannot be used for the data path

Over the BlueZ D-Bus GATT API the handshake write fails:

```
char001c (id=240 type=3) failed: Operation Not Authorized
```

`btmon` proves this is local policy, not the remote refusing. In one capture,
other ATT traffic flows and the remote answers for itself, but the write never
leaves the Pi:

```
ATT: Read Request  handle 0x001d -> Error: Read Not Permitted (0x02)  [remote replied]
ATT: Write Request handle 0x003a -> Error: Invalid Attribute Value Length (0x0d)
ATT: Write Request handle 0x001d -> NEVER APPEARS ON THE AIR
```

BlueZ refuses writes to HID Report characteristics as an anti-keylogging
measure. `--noplugin=input` does **not** lift it (tried; the drop-in is still
in place at `/etc/systemd/system/bluetooth.service.d/noplugin-input.conf`).
The restriction is in gatt-client, not the plugin.

The published projects sidestep this with `bluepy`, which opens its own
L2CAP/ATT socket. `tools/siri_att.py` does the same thing directly on a socket
so there is no unmaintained dependency.

## Traps worth remembering

- **Do not hand-roll RPA resolution.** An earlier conclusion here came from a
  homegrown `ah()` that failed the Bluetooth spec's own test vector
  (`k=ec0234a3...`, `r=708194`, expected `ah=0d94f1`). Use BlueZ's resolution
  (`tools/watch_adv.py`), which watches for RSSI updates on the bonded device.
- **Do not stop `bluetoothd` to get raw access.** It loads the LTK/IRK into the
  kernel; stopping it and cycling the adapter leaves the kernel unable to
  connect at all (`EHOSTUNREACH` instantly). Keep it running and just own the
  ATT channel.
- Calling `bluetoothctl` D-Bus-activates `bluetoothd` right back, silently
  undoing `systemctl stop bluetooth`. Hence `--no-bluez`.
- An abandoned pending LE connect leaves a link stuck in `BT_CONNECT`
  (`hcitool con` shows `state 5`); the next attempt then fails `EBUSY`.
  `systemctl restart bluetooth` clears it.
- **Always back off when a connect fails instantly.** An early version spun
  64,000 attempts in seconds and produced a 2.8 MB log -- and probably helped
  flatten the remote's battery.
- **`pkill -f <pattern>` over SSH matches the invoking `bash -c` command line
  and kills your own session.** This bit three times. All process management
  now lives in `*ctl.sh` files on the Pi so the pattern is never in the caller.
- Address-filtered scanning is unreliable; matching on the HID service UUID
  `0x1812` found the remote immediately.
- A completely drained A1513 **cannot pair at all**. Apple's guidance: 30
  minutes minimum on a charger before testing, ~3 hours for a full charge.

## Running it

The daemon is installed and enabled as `siri-remote.service`, so nothing needs
starting by hand and it survives reboots:

```bash
sudo journalctl -u siri-remote -n 40 --no-pager -o cat   # recent
sudo journalctl -u siri-remote -f -o cat                 # live
```

Log tags: `SIGHTING` (remote advertised), `PAIR`, `LINK` (watch for
`HANDSHAKE ACCEPTED`), `BUTTON`, `TOUCH`, `VOICE` (incl. `TRANSCRIPT:`),
`BATTERY`, `STATE` (heartbeat with a live count of other devices heard, so
silence is never ambiguous).

Recordings land in `/home/pi/siri-audio/siri-YYYYmmdd-HHMMSS.wav`.

## Tooling

Sources in `tools/`, deployed to `~` on the Pi.

| File | Purpose |
|------|---------|
| `siri_daemon.py` | **the service**: waits, pairs, handshakes, decodes, transcribes |
| `siri_att.py` | raw L2CAP/ATT transport + handshake (bypasses BlueZ) |
| `siri_protocol.py` | payload parsing: buttons, touch, audio framing |
| `opus_wav.py` | libopus via ctypes + WAV writer |
| `transcribe.py` | streaming Vosk speech-to-text |
| `test_protocol.py` | 26 parser unit tests, no hardware needed |
| `test_audio_decode.py` | decodes real captured frames -> WAV |
| `test_stt.py` | end-to-end frames -> PCM -> transcript |
| `watch_adv.py` | is the remote advertising? (BlueZ RPA resolution) |
| `pair_remote.py` / `pairctl.sh` | standalone pairer that waits for advertising |
| `gatt_dump.py` | dump GATT tree with values and report references |
| `diag_write.py` / `diagctl.sh` | write-authorisation diagnostic with btmon |
| `scan_hid.py` | rank nearby BLE devices by how HID-like they are |

## Credits

The `0xAF` handshake, the button bitmask and the touch decoding come from:

- <https://github.com/Yanndroid/SiriRemote-Linux>
- <https://github.com/wiz78/SiriRemote-HomeAssistant>

The Opus framing was cross-checked against captured frames from:

- <https://github.com/Jack-R1/SiriRemoteVoiceDecoder>
