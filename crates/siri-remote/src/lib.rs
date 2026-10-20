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
//! The microphone stream is Opus at 16 kHz mono in 20 ms frames, decoded here
//! to raw PCM. [`Event::VoiceAudio`] delivers each 20 ms chunk as it arrives
//! for live consumers; [`Event::VoiceEnd`] delivers the whole utterance.
//! [`audio::wav_bytes`] wraps PCM as a WAV file.
//!
//! Audio requires an ATT MTU of at least [`protocol::MIN_MTU`], which in
//! practice means a Bluetooth 4.2+ controller — the remote will not grant a
//! large MTU to a 4.1 adapter with no Data Length Extension. When the MTU is
//! too small you get [`Event::AudioUnavailable`] and everything else still
//! works.

pub mod att;
#[cfg(feature = "opus")]
pub mod audio;
#[cfg(feature = "bluez")]
pub mod bluez;
pub mod error;
pub mod gesture;
pub mod protocol;
pub mod session;

pub use error::{Error, Result};
pub use gesture::{Swipe, SwipeDetector};
pub use protocol::{Button, Buttons, Finger, Report};
pub use session::{Event, PowerState, Session, SessionConfig};
