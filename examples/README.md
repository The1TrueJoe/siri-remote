# Glue examples

`siri-events` reports what the remote did. These decide what it means.

| Example | What it shows |
|---|---|
| `python/minimal.py` | The smallest useful consumer — react to presses and swipes. |
| `python/voice_stream.py` | Consuming live microphone PCM while the Siri button is held. |
| `shell/mqtt_glue.sh` | Glue needs no particular language: `jq` plus a CLI publisher. |

```bash
siri-events listen DC:2B:2A:E0:18:71 | python/minimal.py
```

A complete Control4 bridge in ~100 lines, including hold-to-ramp volume, lives
in [control4-appletv-remote](https://github.com/The1TrueJoe/control4-appletv-remote)
as `examples/python/control4_glue.py`.

## Writing your own

Read stdin, parse each line as JSON, switch on `event`. The events that matter
are `button` (with `state` of `down`/`up`), `swipe`, `battery`, `voice_start`
and `voice_end`.

Two things worth copying:

- **Use both edges for hold-style actions.** Ramp volume by sending a start
  command on press and a stop command on release, rather than a stream of
  discrete steps.
- **Ignore what you don't map.** New event kinds get added; a `switch` that
  falls through harmlessly keeps your glue working across upgrades.
