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
| Microphone capture | **working** (2026-10-20): host must start the stream -- see below |
| Speex decode + STT | 653/653 live A1513 frames decoded; Vosk transcribed correctly |
| Opus decode (A1962) | validated offline (29/29 frames; Vosk 0.99x realtime) |

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

### Microphone audio: the host has to start it

Holding Siri sends nothing but the button report until the host writes
feature report `0x98` = 1 to the keyhole, i.e. `98 01` to handle `0x001d`.
Then 82-byte audio notifications follow immediately. The full story, including
the theories it took to rule out, is under [Voice](#voice).

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

| Length | First byte | Meaning |
|--------|------------|---------|
| 2 bytes | `0x00` | button bitmask only |
| 2 bytes | `0x10` | end of a voice stream (A1962 only) |
| 13 bytes | finger count | one finger on the touchpad |
| 20 bytes | finger count | two fingers |
| 82 bytes | `0x10` | **Speex microphone frame** (A1513) |
| 101 bytes | `0x10` | **Opus microphone frame** (A1962) |

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

Measured 2026-10-20 on the A1513 (HID product `0x0266`, firmware
`100570220007003300`) through a TP-Link UB500 (RTL8761BU, Bluetooth 5.1),
with `btmon` on the air.

### Holding Siri only arms the microphone

With just the `0xAF` unlock, a Siri hold produces exactly two notifications
-- `00 10` on press, `00 00` on release -- and nothing on the air in between.
The same is true through BlueZ's `hog` + hidraw path, and on macOS, whose
driver logs one input report per edge and no audio.

The firmware's audio state machine (from
[VibeRemote's analysis](https://github.com/mmmmmmarcus/VibeRemote/blob/main/Design/Research/FirstGenerationMicrophoneControl.md)
of Apple's A1962 firmware) explains why:

| State | Event | Next |
|-------|-------|------|
| CONNECTED | Siri down | CONNECTED_WITH_AUDIO (armed, not streaming) |
| CONNECTED_WITH_AUDIO | feature `0x98` != 0 | STREAMING |
| STREAMING | Siri up | STREAMING_TIMEOUT (tail, then stop) |
| STREAMING_TIMEOUT | Siri down | STREAMING again |

An Apple TV sends `0x98` when Siri goes down. Nothing else does. Keyhole writes
are `[report id, payload...]`, so it is `98 01` to `0x001d` -- the same handle
and framing as the `AF` unlock, which is itself report `0xAF` with no payload.
The remote acknowledges with a Write Response and starts streaming within
~100 ms.

The A1962 measured by
[open-appleremote](https://github.com/a-funk/open-appleremote) streams
without it, presumably because its firmware enables the "fast switch" that
skips this step.

### Frame format: Speex wideband, not Opus

```
[0x10][buttons][70-byte Speex frame][46 00 = len 70][4 bytes][f32 LE stream time]
```

- The codec frame starts `0x30`..`0x37`: Speex's narrowband-layer header,
  wideband bit 0 then submode `0110` = 6. Speex wideband at quality 8 is
  narrowband submode 6 plus a high band, 27.8 kbit/s -- 70 bytes per 20 ms.
  All 653 captured frames decode with libspeex (`SPEEX_MODEID_WB`, 320 samples
  each); Vosk transcribed the result correctly. Opus decodes almost none.
- The trailing float advances by exactly 0.02 per frame (`0x3ca3d70a` = 0.02,
  `0x3d23d70a` = 0.04, ...), so it doubles as a loss detector.
- Byte 1 is the live button mask. **While streaming, the remote sends no
  separate button reports** -- a release shows up only as `10 00` in the
  frames, and a re-press as `10 10`.
- There is no end marker on the A1513: after release the stream runs a
  ~1--1.5 s tail and stops.

The A1962's 101-byte layout is different again (`[0x10][buttons][level u16]
[seq u16][len][Opus, TOC 0xB8][filler]`, per open-appleremote) and ends with a
2-byte `10 xx`.

### The ATT MTU is irrelevant

The remote answers MTU 23 to every host -- the Pi, macOS -- and never
initiates an exchange. It sends 85-byte ATT PDUs regardless, split across
27-byte link-layer fragments, and the kernel reassembles them. What matters is
the **socket** receive MTU (`BT_RCVMTU`, set before `connect()`), which must
accept the whole PDU. The earlier conclusions here -- that audio needed a
Bluetooth 4.2 controller with Data Length Extension, or that the remote
grants a larger MTU if it asks first -- were both wrong.

### Connection interval

At Linux's default 30--50 ms interval the remote delivers one notification per
connection event, about every 45 ms, while producing one every 20 ms. It falls
behind: 7.3 s of speech took 16.6 s to arrive, and the stream kept draining
long after release. The library asks for 7.5--15 ms with an HCI LE Connection
Update right after connecting (`crates/siri-remote/src/hci.rs`), which needs
`CAP_NET_RAW`.

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

The `0x98` stream-start report and the audio state machine come from:

- <https://github.com/mmmmmmarcus/VibeRemote>

The A1962's voice layout and the observation that the remote ignores the MTU:

- <https://github.com/a-funk/open-appleremote>
