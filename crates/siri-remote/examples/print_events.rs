//! Minimal consumer: connect to a remote and print what it does.
//!
//! ```sh
//! cargo run --example print_events -- DC:2B:2A:E0:18:71
//! ```
//!
//! The remote must already be bonded (`siri-events pair <address>`), and only
//! advertises for a second or two after a button press — so press one once
//! this is running.

use siri_remote::{Event, Session, SessionConfig, SwipeDetector};

fn main() -> siri_remote::Result<()> {
    let address = std::env::args()
        .nth(1)
        .expect("usage: print_events <BD_ADDR>");

    let cfg = SessionConfig::new(address);
    let mut swipes = SwipeDetector::default();

    println!("waiting for the remote — press a button");
    Session::run(&cfg, |event| {
        match event {
            Event::Connected { mtu, .. } => println!("connected, MTU {mtu}"),
            Event::AudioUnavailable { mtu, needed } => {
                println!("no microphone: MTU {mtu} < {needed} (needs a Bluetooth 4.2+ adapter)")
            }
            Event::ButtonDown(b) => println!("down  {}", b.as_str()),
            Event::ButtonUp(b) => println!("up    {}", b.as_str()),
            Event::Touch { fingers, .. } => {
                if let Some(s) = swipes.update(&fingers) {
                    println!("swipe {}", s.as_str());
                }
            }
            Event::Battery(p) => println!("battery {p}%"),
            Event::Power(p) => println!("power {}", p.as_str()),
            Event::VoiceStart => println!("listening…"),
            Event::VoiceEnd { pcm, frames, .. } => {
                let secs = pcm.len() as f32 / 16000.0;
                println!("voice: {frames} frames, {secs:.2}s of 16 kHz mono PCM");
            }
            Event::Disconnected(why) => println!("disconnected: {why}"),
            Event::VoiceAudio(_) => {}
        }
        true
    })
}
