//! Stream Siri Remote events as newline-delimited JSON.
//!
//! This is the seam between "what the remote did" and "what that should mean".
//! It deliberately makes no decisions: it prints events, and writes the
//! microphone stream where you tell it to. Glue it to anything — Control4,
//! Home Assistant, MQTT, a shell script — in whatever language you like.
//!
//! ```sh
//! siri-events scan
//! siri-events pair DC:2B:2A:E0:18:71
//! siri-events listen DC:2B:2A:E0:18:71 --audio-dir /tmp/voice | ./glue.py
//! ```
//!
//! # Event schema
//!
//! One JSON object per line on stdout. Every event carries `event`, `address`
//! and `t` (unix seconds). Diagnostics go to stderr, so stdout stays parseable.
//!
//! ```text
//! {"event":"connected","mtu":23,"kernel_rcv_mtu":517,"audio_available":true}
//! {"event":"button","button":"MENU","state":"down"}
//! {"event":"button","button":"MENU","state":"up"}
//! {"event":"swipe","direction":"SWIPE_LEFT"}
//! {"event":"touch","x":57,"y":43,"pressure":41,"pressed":true}
//! {"event":"battery","percent":27}
//! {"event":"power","state":"plugged in"}
//! {"event":"voice_start"}
//! {"event":"voice_end","frames":52,"concealed":0,"seconds":1.04,"wav":"/tmp/voice/….wav"}
//! {"event":"disconnected","reason":"link closed"}
//! ```

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use siri_remote::{audio, protocol, Event, Session, SessionConfig, SwipeDetector};

#[derive(Parser)]
#[command(name = "siri-events", version, about = "Siri Remote events as NDJSON")]
struct Cli {
    /// Adapter to use, by BD address or kernel name (e.g. `hci1`). Defaults to
    /// BlueZ's default adapter.
    ///
    /// Worth naming on a machine with more than one radio, so the remote is
    /// always paired with and connected through the same one.
    #[arg(long, global = true)]
    adapter: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List every Bluetooth adapter on this machine.
    Adapters,
    /// Look for nearby remotes.
    ///
    /// A remote only advertises for a second or two after a button press, so
    /// press one while this runs — or hold Menu+Volume Up for about five
    /// seconds to force pairing mode.
    Scan {
        #[arg(long, default_value_t = 20)]
        seconds: u64,
        /// Emit NDJSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Bond with a remote so it can be connected to later.
    Pair { address: String },
    /// Forget a remote.
    Unpair { address: String },
    /// Stream events until interrupted.
    Listen {
        address: String,
        /// Write each utterance here as a 16 kHz mono WAV and report the path.
        #[arg(long)]
        audio_dir: Option<PathBuf>,
        /// Also stream raw PCM (16-bit LE, 16 kHz mono) here as it arrives,
        /// for real-time consumers. Create it with `mkfifo` first.
        #[arg(long)]
        pcm_fifo: Option<PathBuf>,
        /// Include every touch sample. Off by default: it is a firehose, and
        /// swipes are usually what you actually want.
        #[arg(long)]
        raw_touch: bool,
        /// Give up if the remote does not appear within this many seconds.
        #[arg(long, default_value_t = 900)]
        timeout: u64,
        /// Peer address type, when BlueZ reports the remote as random.
        #[arg(long, default_value = "public")]
        addr_type: String,
        /// Keep the kernel's default connection interval instead of asking
        /// for a faster one. Asking needs CAP_NET_RAW; without the faster
        /// interval the microphone lags further behind the longer you speak.
        #[arg(long)]
        keep_interval: bool,
    },
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One line of JSON on stdout, flushed so pipes stay responsive.
fn emit(address: &str, event: &str, mut extra: Value) {
    let mut v = json!({ "event": event, "address": address, "t": now() });
    if let (Some(src), Some(dst)) = (extra.as_object_mut(), v.as_object_mut()) {
        for (k, val) in src.iter() {
            dst.insert(k.clone(), val.clone());
        }
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

// A single-purpose CLI: one task, no thread pool needed. Being explicit
// also lets this crate build on its own rather than relying on feature
// unification with the daemon to pull in `rt-multi-thread`.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let adapter = cli.adapter.clone();
    let want = adapter.as_deref();

    match cli.command {
        Command::Adapters => adapters().await,
        Command::Scan { seconds, json } => scan(seconds, json, want).await,
        Command::Pair { address } => {
            let t = siri_remote::bluez::pair(&address, want).await?;
            eprintln!("paired {address} ({t})");
            Ok(())
        }
        Command::Unpair { address } => {
            siri_remote::bluez::remove(&address, want).await?;
            eprintln!("removed {address}");
            Ok(())
        }
        Command::Listen {
            address,
            audio_dir,
            pcm_fifo,
            raw_touch,
            timeout,
            addr_type,
            keep_interval,
        } => listen(
            address,
            audio_dir,
            pcm_fifo,
            raw_touch,
            timeout,
            addr_type,
            adapter,
            keep_interval,
        ),
    }
}

async fn adapters() -> Result<()> {
    let found = siri_remote::bluez::adapters().await?;
    if found.is_empty() {
        eprintln!("no Bluetooth adapters found");
        return Ok(());
    }
    println!(
        "{:<8} {:<20} {:<8} {}",
        "NAME", "ADDRESS", "POWERED", "DISCOVERING"
    );
    for a in found {
        println!(
            "{:<8} {:<20} {:<8} {}",
            a.name, a.address, a.powered, a.discovering
        );
    }
    eprintln!("\nPass --adapter to choose one.");
    Ok(())
}

async fn scan(seconds: u64, as_json: bool, want: Option<&str>) -> Result<()> {
    eprintln!("scanning {seconds}s — press a button on the remote");
    let found = siri_remote::bluez::scan(seconds, want).await?;
    if as_json {
        for d in &found {
            println!("{}", serde_json::to_string(d)?);
        }
        return Ok(());
    }
    if found.is_empty() {
        eprintln!("nothing found. The remote only advertises briefly after a press.");
        return Ok(());
    }
    println!(
        "{:<20} {:>6}  {:<8} {:<7} NAME",
        "ADDRESS", "RSSI", "TYPE", "PAIRED"
    );
    for d in found {
        println!(
            "{:<20} {:>6}  {:<8} {:<7} {}{}",
            d.address,
            d.rssi.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
            d.addr_type,
            d.paired,
            d.name,
            if d.likely_remote {
                "  <-- likely a Siri Remote"
            } else {
                ""
            }
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn listen(
    address: String,
    audio_dir: Option<PathBuf>,
    pcm_fifo: Option<PathBuf>,
    raw_touch: bool,
    timeout: u64,
    addr_type: String,
    adapter: Option<String>,
    keep_interval: bool,
) -> Result<()> {
    if let Some(d) = &audio_dir {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    // Opening a FIFO for writing blocks until a reader attaches, which is the
    // desired behaviour: it means the consumer is ready before audio flows.
    let mut fifo = match &pcm_fifo {
        Some(p) => {
            eprintln!("waiting for a reader on {}", p.display());
            Some(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(p)
                    .with_context(|| format!("opening {}", p.display()))?,
            )
        }
        None => None,
    };

    let mut cfg = SessionConfig::new(&address);
    cfg.connect_timeout = std::time::Duration::from_secs(timeout);
    cfg.addr_type = if addr_type.eq_ignore_ascii_case("random") {
        siri_remote::att::BDADDR_LE_RANDOM
    } else {
        siri_remote::att::BDADDR_LE_PUBLIC
    };
    cfg.adapter = adapter;
    if keep_interval {
        cfg.conn_params = None;
    }

    let mut swipes = SwipeDetector::default();
    eprintln!("waiting for {address} — press a button on the remote");

    let result = Session::run(&cfg, |event| {
        match event {
            Event::Connected {
                mtu,
                kernel_rcv_mtu,
            } => emit(
                &address,
                "connected",
                json!({
                    "mtu": mtu,
                    "kernel_rcv_mtu": kernel_rcv_mtu,
                    "audio_available": kernel_rcv_mtu >= protocol::MIN_MTU,
                }),
            ),
            Event::AudioUnavailable { mtu, needed } => emit(
                &address,
                "audio_unavailable",
                json!({
                    "mtu": mtu,
                    "needed": needed,
                    "reason": "the kernel socket cannot accept whole microphone frames",
                }),
            ),
            Event::ButtonDown(b) => emit(
                &address,
                "button",
                json!({ "button": b.as_str(), "state": "down" }),
            ),
            Event::ButtonUp(b) => emit(
                &address,
                "button",
                json!({ "button": b.as_str(), "state": "up" }),
            ),
            Event::Touch { fingers, pressed } => {
                if let Some(s) = swipes.update(&fingers) {
                    emit(&address, "swipe", json!({ "direction": s.as_str() }));
                }
                if raw_touch {
                    if let Some(f) = fingers.first() {
                        emit(
                            &address,
                            "touch",
                            json!({
                                "x": f.x, "y": f.y, "pressure": f.pressure,
                                "pressed": pressed, "fingers": fingers.len(),
                            }),
                        );
                    }
                }
            }
            Event::Battery(p) => emit(&address, "battery", json!({ "percent": p })),
            Event::Power(p) => emit(&address, "power", json!({ "state": p.as_str() })),
            Event::VoiceStart => emit(&address, "voice_start", json!({})),
            Event::VoiceAudio(pcm) => {
                // Live egress: write PCM through as it arrives so a consumer
                // can transcribe while the button is still held.
                if let Some(f) = fifo.as_mut() {
                    let mut bytes = Vec::with_capacity(pcm.len() * 2);
                    for s in &pcm {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                    if f.write_all(&bytes).is_err() {
                        eprintln!("pcm reader went away; stopping live audio");
                        fifo = None;
                    }
                }
            }
            Event::VoiceEnd {
                pcm,
                frames,
                undecodable,
                concealed,
                held,
            } => {
                let seconds = pcm.len() as f32 / protocol::AUDIO_SAMPLE_RATE as f32;
                let mut extra = json!({
                    "frames": frames,
                    "undecodable": undecodable,
                    "concealed": concealed,
                    "seconds": seconds,
                    "held_ms": held.as_millis() as u64,
                    "sample_rate": protocol::AUDIO_SAMPLE_RATE,
                    "channels": protocol::AUDIO_CHANNELS,
                });
                if let Some(dir) = &audio_dir {
                    let path = dir.join(format!("siri-{}-{}.wav", address.replace(':', ""), now()));
                    let wav = audio::wav_bytes(
                        &pcm,
                        protocol::AUDIO_SAMPLE_RATE,
                        protocol::AUDIO_CHANNELS,
                    );
                    match std::fs::write(&path, wav) {
                        Ok(_) => extra["wav"] = json!(path.display().to_string()),
                        Err(e) => extra["error"] = json!(e.to_string()),
                    }
                }
                emit(&address, "voice_end", extra);
            }
            Event::Disconnected(reason) => {
                emit(&address, "disconnected", json!({ "reason": reason }))
            }
        }
        true
    });

    if let Err(e) = result {
        emit(&address, "error", json!({ "message": e.to_string() }));
        return Err(e.into());
    }
    Ok(())
}
