# siri-remote

Use an Apple TV Siri Remote as a Bluetooth input device on Linux — buttons,
touchpad, swipes, battery, and the microphone stream — and route it wherever
you like.

The remote publishes a deliberately null HID descriptor, so the kernel cannot
drive it and BlueZ refuses the write that unlocks it. This talks to it directly
over a raw ATT channel instead. See
[docs/siri-remote-gen1-findings.md](docs/siri-remote-gen1-findings.md) for how
the protocol was worked out.

## What's here

| Crate / path | What it is |
|---|---|
| `crates/siri-remote` | **The library.** Protocol, transport, gestures, Opus audio. Produces events; decides nothing. |
| `crates/siri-events` | **The seam.** Streams events as newline-delimited JSON so glue can be written in any language. |
| `examples/` | Glue scripts — Python for live audio, shell for MQTT. |
| `tools/` | The Python probes the protocol was reverse-engineered with. |

Nothing here knows what a button press *means*. For a complete consumer, see
[control4-appletv-remote](https://github.com/The1TrueJoe/control4-appletv-remote),
a Control4 bridge built on this library.

**Speech recognition is out of scope.** The remote's microphone is 16 kHz mono
PCM — exactly what offline recognisers want — so this project hands you the
audio and gets out of the way.

## Quick start

```bash
sudo apt install build-essential pkg-config libdbus-1-dev libopus-dev
cargo install --git https://github.com/The1TrueJoe/siri-remote siri-events

siri-events scan                                 # press a button while it runs
siri-events pair DC:2B:2A:E0:18:71
siri-events listen DC:2B:2A:E0:18:71 | examples/python/minimal.py
```

## Event stream

One JSON object per line on stdout; diagnostics go to stderr.

```json
{"event":"connected","address":"DC:…","mtu":104,"audio_available":true}
{"event":"button","button":"MENU","state":"down"}
{"event":"swipe","direction":"SWIPE_LEFT"}
{"event":"battery","percent":27}
{"event":"voice_start"}
{"event":"voice_end","seconds":1.04,"wav":"/tmp/voice/siri-….wav"}
```

Controls: `MENU`, `PLAY_PAUSE`, `VOLUME_UP`, `VOLUME_DOWN`, `AIRPLAY`, `SIRI`,
`TOUCHPAD`, `TOUCHPAD_2`, and `SWIPE_{UP,DOWN,LEFT,RIGHT}`.

## Voice

Holding **Siri** streams the microphone, decoded to 16 kHz mono PCM.

```bash
# whole utterances as WAV files
siri-events listen <addr> --audio-dir /tmp/voice

# live PCM while the button is held, for streaming recognisers
mkfifo /tmp/siri.pcm
siri-events listen <addr> --pcm-fifo /tmp/siri.pcm &
examples/python/voice_stream.py /tmp/siri.pcm
```

**Status: being reworked.** On the A1513 (HID product `0x0266`) holding Siri
only arms the microphone; the remote streams once the host writes `98 01` to
handle `0x001d`, and its frames are Speex wideband rather than Opus. The
library does not do either yet. `tools/att_probe.py --stream-on-siri`
demonstrates the working sequence.

## Using the library

```toml
[dependencies]
siri-remote = { git = "https://github.com/The1TrueJoe/siri-remote", tag = "v0.1.0" }
```

```rust
use siri_remote::{Session, SessionConfig, Event};

Session::run(&SessionConfig::new("DC:2B:2A:E0:18:71"), |event| {
    if let Event::ButtonDown(b) = event {
        println!("pressed {}", b.as_str());
    }
    true
})?;
```

```bash
cargo run -p siri-remote --example print_events -- DC:2B:2A:E0:18:71
```

Features: `opus` (microphone decoding, on by default) and `bluez` (discovery
and pairing, off by default — the rest of the crate is synchronous and has no
D-Bus dependency).

## Building

Plain `cargo build --release`. The only wrinkle is that the crates link C
libraries — `libopus` for microphone decoding and `libdbus` for pairing — which
makes cross-compilation awkward, since it needs those libraries for the target
architecture rather than the host.

**On the Pi itself.** Slowest but simplest, and always correct — it is the
target. A Pi 3 wants `-j2` so the linker does not exhaust memory.

```bash
sudo apt install build-essential pkg-config libdbus-1-dev libopus-dev
cargo build --release -j2
```

**In a container.** Fast on an Apple Silicon Mac, where `linux/arm64` runs
natively rather than emulated:

```bash
docker build --platform linux/arm64 -t siri-build -f scripts/Dockerfile.build scripts/
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w siri-build cargo build --release
```

**Locally, for the library only.** `siri-remote` is portable enough to compile
and test on macOS, which gives a much faster edit-test loop than either of the
above. The `bluez` feature and `siri-events` are Linux-only, so check the
library on its own:

```bash
brew install opus
export LIBRARY_PATH="$(brew --prefix opus)/lib:$LIBRARY_PATH"
cargo test -p siri-remote
```

Note that the raw ATT transport describes Linux kernel structures, so it only
*runs* on Linux — but it type-checks and its unit tests (protocol decoding,
gestures, Opus, sockaddr layout) pass anywhere.

## Credits

The `0xAF` unlock byte, the button bitmask and the touch decoding come from
[Yanndroid/SiriRemote-Linux](https://github.com/Yanndroid/SiriRemote-Linux) and
[wiz78/SiriRemote-HomeAssistant](https://github.com/wiz78/SiriRemote-HomeAssistant).
Opus framing was cross-checked against captures from
[Jack-R1/SiriRemoteVoiceDecoder](https://github.com/Jack-R1/SiriRemoteVoiceDecoder).
The `0x98` stream-start report comes from the firmware analysis in
[mmmmmmarcus/VibeRemote](https://github.com/mmmmmmarcus/VibeRemote).
