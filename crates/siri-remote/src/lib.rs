//! Apple TV Siri Remote over Bluetooth LE, as an event stream.
//!
//! The remote publishes a deliberately null HID descriptor (vendor usage page
//! `0xFF00`, input flagged `Constant`), so the kernel cannot drive it as an
//! input device. The real protocol is unlocked by writing one byte, after
//! which buttons, touchpad, battery and microphone audio all arrive on a
//! single characteristic.
//!
//! **This crate produces events and audio. It does not decide what they mean.**
//! Mapping a button to an action, or a voice recording to a command, is glue
//! that belongs in your application — see the `examples/` directory and the
//! `siri-events` binary, which emits newline-delimited JSON so the glue can be
//! written in any language.
//!
//! # Getting events
//!
//! ```no_run
//! use siri_remote::{Session, SessionConfig, Event};
//!
//! let cfg = SessionConfig::new("DC:2B:2A:E0:18:71");
//! Session::run(&cfg, |event| {
//!     match event {
//!         Event::ButtonDown(b) => println!("pressed {}", b.as_str()),
//!         Event::VoiceEnd { pcm, .. } => {
//!             // 16 kHz mono PCM, ready for whatever you use downstream.
//!             println!("{} samples of speech", pcm.len());
//!         }
//!         _ => {}
//!     }
//!     true // keep going
//! }).unwrap();
//! ```
//!
//! # Audio
//!
//! The microphone streams 16 kHz mono in 20 ms frames — Speex on the A1513,
//! Opus on the A1962 — decoded here to raw PCM. [`Event::VoiceAudio`] delivers
//! each 20 ms chunk as it arrives for live consumers; [`Event::VoiceEnd`]
//! delivers the whole utterance. [`audio::wav_bytes`] wraps PCM as a WAV file.
//!
//! Holding Siri only arms the microphone: the session starts the stream by
//! writing [`protocol::STREAM_START`] the moment Siri goes down, and asks for
//! a short connection interval so frames arrive as fast as they are produced.
//! Lost frames are concealed rather than skipped, so the audio keeps its
//! timing.

pub mod att;
#[cfg(any(feature = "opus", feature = "speex"))]
pub mod audio;
#[cfg(feature = "bluez")]
pub mod bluez;
pub mod error;
pub mod gesture;
pub mod hci;
pub mod protocol;
pub mod session;

pub use error::{Error, Result};
pub use gesture::{Swipe, SwipeDetector};
pub use hci::ConnParams;
pub use protocol::{AudioFrame, Button, Buttons, Codec, Finger, Report};
pub use session::{Event, PowerState, Session, SessionConfig};
