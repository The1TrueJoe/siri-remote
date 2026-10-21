# Apple TV Siri Remote — BLE protocol

The authoritative wire-protocol description for the 1st-gen Apple TV Siri Remote
(**A1513**, HID product `0x0266`) and its sibling the **A1962** (`0x026D`), as
spoken over Bluetooth LE. This is the shared contract for every implementation
in this repo — the Linux/Rust library under `crates/` and the Zephyr/C module
under `zephyr/` — so they behave identically. It was recovered on real hardware;
the long-form investigation is in
[`docs/siri-remote-gen1-findings.md`](docs/siri-remote-gen1-findings.md).

All multi-byte values here are little-endian unless stated otherwise.

## 1. Connection

- Transport: Bluetooth LE, HID-over-GATT. Apple vendor ID `0x004C`.
  Products: `0x0266` (A1513), `0x026D` (A1962). The remote advertises its
  serial number as its name and the HID service UUID `0x1812`.
- It advertises **only for a second or two after a button press**, so a central
  must keep a connect pending and let a press complete it.
- Pair/bond normally (LE, Just-Works; a NoInputNoOutput agent accepts it). An
  **encrypted** link is required — the remote sends nothing decryptable without
  the LTK. On Linux the raw ATT path relies on BlueZ having loaded the LTK into
  the kernel; a from-scratch host (Zephyr) stores the LTK itself.

## 2. The facade: it does not speak standard HID

The Report Map (`0x2A4B`) is vendor-defined only, with the input item flagged
`Constant` ("ignore"). So a generic HID host builds nothing usable. The real
protocol is unlocked by a vendor write (§3); until then the notify
characteristic emits only zeros.

## 3. Unlock and stream-start (writes to handle `0x001D`)

Handle `0x001D` is a Feature report ("keyhole"); each write is
`[report-id, payload…]`.

| Write | Bytes | Effect |
|-------|-------|--------|
| **Unlock** | `AF` | Enables button/touch/battery/power reports. Report id `0xAF`, no payload. Re-send on every connect. |
| **Stream-start** | `98 01` | Feature report `0x98` = 1 = `EVT_ENABLE_AUDIO_STREAMING`. **Required for microphone audio.** Send on Siri-button-down. |
| **Stream-stop** | `98 00` | Stops streaming immediately (drops the tail). Optional; releasing Siri stops it on its own. |

Holding Siri only **arms** the mic (firmware CONNECTED → CONNECTED_WITH_AUDIO);
`98 01` moves it to STREAMING, which is what an Apple TV sends. Without it the
remote reports the button and stays silent.

Then enable notifications by writing `01 00` to each CCCD below.

## 4. Handle map

| Handle | Purpose |
|--------|---------|
| `0x001D` | unlock / stream-start writes (§3) |
| `0x0023` | input: buttons, touch **and** audio all arrive here |
| `0x0024` | CCCD for `0x0023` |
| `0x0028` / `0x0029` | battery level / its CCCD |
| `0x002B` / `0x002C` | power state / its CCCD |
| `0x003B` | Apple vendor channel CCCD (subscribe so nothing is silently dropped) |

## 5. Input notifications (handle `0x0023`)

One characteristic carries everything; disambiguate by **length** and **byte 0**:

| Len | Byte 0 | Meaning |
|-----|--------|---------|
| 2 | `0x00` | buttons only |
| 2 | `0x10` | end of a voice stream (A1962 only) |
| 13 | finger count | touch, 1 finger |
| 20 | finger count | touch, 2 fingers |
| 82 | `0x10` | **Speex** microphone frame (A1513) |
| 101 | `0x10` | **Opus** microphone frame (A1962) |

**Byte 1 is always the button bitmask**, in every report kind — so a press or
release made while the mic is streaming is visible in the audio frames
themselves (the remote sends no separate button reports while streaming).

### Buttons (bitmask in byte 1)

| Bit | Button | Bit | Button |
|-----|--------|-----|--------|
| `0x01` | AirPlay/TV | `0x10` | Siri |
| `0x02` | Volume Up | `0x20` | Menu |
| `0x04` | Volume Down | `0x80` | Touchpad (click) |
| `0x08` | Play/Pause | `0x00` | all released |

A two-finger click arrives as finger-count 2 + the touchpad bit; promote it to a
synthetic `TOUCHPAD_2`.

### Touch (`data[2] == 0x32` marks a touch report)

Finger records at `[6:13]` and `[13:20]`, 7 bytes each:
```
x = (f[0] + 255*(f[1] & 7) - 230) / 15
y = (f[2] if f[2] & 0x80 else f[2] + 255) - 188   # wraps 188..255 then 0..38
pressure = f[5]
```
`+x` right, `+y` up; Y resolution is coarser than X. Gestures (swipes) are
derived from the touch stream by the implementation.

## 6. Microphone audio

Both models: **16 kHz mono, 20 ms frames** (320 samples). Codec differs:

### A1513 — Speex wideband (82-byte notification)
```
[0x10][buttons][70-byte Speex WB frame][len u16 LE = 70][4 bytes ?][stream-time f32 LE]
```
- Codec frame starts `0x30`..`0x37` (Speex NB-layer header, wideband bit 0,
  submode 6); decode with libspeex `SPEEX_MODEID_WB`.
- The trailing float advances by exactly `0.02` per frame — doubles as a loss
  detector (a bigger jump = lost frames; conceal them).
- No end marker: after Siri release the stream runs a ~1–1.5 s tail then stops.
  End the utterance after a ~600 ms gap.

### A1962 — Opus (101-byte notification)
```
[0x10][buttons][level u16][seq u16][len][Opus frame, TOC 0xB8][filler]
```
- Opus TOC `0xB8` = config 23 (CELT wideband 16 kHz, 20 ms), mono, 1 frame.
- Stream ends with a 2-byte `10 xx` notification.

**For Control4 (`appled`) the audio must be Opus.** A1962 relays its Opus
verbatim; A1513 must be transcoded Speex→Opus (host/Pi: libspeex+libopus; on an
MCU this is the heavy step — see the firmware notes).

## 7. The ATT MTU does not gate audio

The remote answers MTU **23** to every host (macOS included) and never
initiates an exchange, yet it sends whole 82/101-byte notifications anyway,
fragmented into 27-byte link-layer PDUs that the controller reassembles. What
matters is the **host's receive buffer** (Linux: the L2CAP socket `BT_RCVMTU`,
set before connect; Zephyr: `CONFIG_BT_BUF_ACL_RX_SIZE` / L2CAP RX MTU large
enough for the frame). Bluetooth version / Data Length Extension is irrelevant —
a 4.0/4.1 radio works.

## 8. Connection interval

At the host default (~30–50 ms) the remote delivers ~1 notification per
connection event while producing one every 20 ms, so audio lags and keeps
draining long after release. Request a **7.5–15 ms** interval right after
connecting (Linux: raw HCI LE Connection Update, needs `CAP_NET_RAW`; Zephyr:
`bt_conn_le_param_update`). The remote accepts 15 ms; audio then arrives in real
time.

## 9. Event model (what an implementation should emit)

Both implementations present the same logical events to their consumer:
`Connected{mtu, audio_ok}`, `ButtonDown/Up`, `Touch{fingers, pressed}`,
`Swipe{dir}`, `Battery{pct}`, `Power{state}`, `VoiceStart`, `VoiceFrame`
(one 20 ms chunk, codec-tagged), `VoiceEnd{frames, concealed, …}`,
`Disconnected`.
