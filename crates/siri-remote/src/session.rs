//! A connected remote: handshake, then a stream of decoded events.

use std::time::{Duration, Instant};

use crate::att::{AttSocket, BDADDR_LE_PUBLIC, BT_SECURITY_MEDIUM};
use crate::error::Result;
use crate::protocol::{self as proto, Button, Buttons, Finger, Report};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerState {
    Charging,
    Discharging,
    PluggedIn,
    Unknown(u8),
}

impl PowerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PowerState::Charging => "charging",
            PowerState::Discharging => "discharging",
            PowerState::PluggedIn => "plugged in",
            PowerState::Unknown(_) => "unknown",
        }
    }
}

/// Something the remote did.
#[derive(Debug, Clone)]
pub enum Event {
    Connected {
        /// Negotiated ATT MTU. Below [`proto::MIN_MTU`] the microphone cannot
        /// be carried — see [`Event::AudioUnavailable`].
        mtu: u16,
        kernel_rcv_mtu: u16,
    },
    /// The link is up but the MTU is too small for microphone audio. Buttons
    /// and touch still work.
    AudioUnavailable {
        mtu: u16,
        needed: u16,
    },
    ButtonDown(Button),
    ButtonUp(Button),
    Touch {
        fingers: Vec<Finger>,
        pressed: bool,
    },
    Battery(u8),
    Power(PowerState),
    /// Siri was pressed; microphone frames follow.
    VoiceStart,
    /// One decoded 20 ms chunk, streamed as it arrives so a recogniser can
    /// consume it live rather than waiting for the button to be released.
    VoiceAudio(Vec<i16>),
    VoiceEnd {
        pcm: Vec<i16>,
        frames: usize,
        undecodable: usize,
        held: Duration,
    },
    Disconnected(String),
}

pub struct SessionConfig {
    pub address: String,
    pub addr_type: u8,
    pub sec_level: u8,
    pub want_mtu: u16,
    /// Local adapter to connect through, by BD address. `None` lets the kernel
    /// choose, which is only unambiguous with a single adapter. Microphone
    /// audio needs a Bluetooth 4.2+ controller, so on a machine with both a
    /// built-in radio and a dongle this should name the dongle.
    pub adapter: Option<String>,
    /// How long to let the remote initiate the MTU exchange before asking
    /// ourselves. Asking first caps the link at MTU 23 and silently disables
    /// the microphone, so this wait is what makes audio possible.
    pub peer_mtu_wait: Duration,
    /// How long to keep a connect pending while waiting for a button press.
    pub connect_timeout: Duration,
}

impl SessionConfig {
    pub fn new(address: impl Into<String>) -> Self {
        SessionConfig {
            address: address.into(),
            addr_type: BDADDR_LE_PUBLIC,
            sec_level: BT_SECURITY_MEDIUM,
            want_mtu: 517,
            adapter: None,
            peer_mtu_wait: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(900),
        }
    }
}

pub struct Session;

impl Session {
    /// Connect, unlock, and pump events until the link drops or `on_event`
    /// returns `false`.
    ///
    /// This blocks; run it on its own thread (or `spawn_blocking`) per remote.
    pub fn run(
        cfg: &SessionConfig,
        mut on_event: impl FnMut(Event) -> bool,
    ) -> Result<()> {
        Self::run_with_retry_hook(cfg, |_, _, _| {}, &mut on_event)
    }

    pub fn run_with_retry_hook(
        cfg: &SessionConfig,
        on_retry: impl FnMut(u32, &std::io::Error, Duration),
        on_event: &mut impl FnMut(Event) -> bool,
    ) -> Result<()> {
        let mut sock = AttSocket::connect(
            &cfg.address,
            cfg.addr_type,
            cfg.sec_level,
            cfg.want_mtu,
            cfg.adapter.as_deref(),
            cfg.connect_timeout,
            on_retry,
        )?;

        // Let the remote ask for the MTU first. It offers a Client RX MTU of
        // 104 when it initiates, but answers 23 if we ask — and 101-byte audio
        // frames only fit under the former. See AttSocket::await_peer_mtu.
        let (peer_mtu, buffered) =
            sock.await_peer_mtu(cfg.peer_mtu_wait, cfg.want_mtu);

        sock.write_req(proto::H_ENABLE, &[proto::ENABLE_BYTE])?;

        let mtu = match peer_mtu {
            Some(m) => m,
            // The remote stayed quiet; fall back to asking, which still gets
            // buttons and touch working even though audio will not fit.
            None => sock.exchange_mtu(cfg.want_mtu).unwrap_or(23),
        };

        for cccd in [
            proto::H_INPUT_CCCD,
            proto::H_BATTERY_CCCD,
            proto::H_POWER_CCCD,
            proto::H_VENDOR_CCCD,
        ] {
            // A remote without the vendor service simply rejects that one.
            let _ = sock.write_req(cccd, &[0x01, 0x00]);
        }

        if !on_event(Event::Connected {
            mtu,
            kernel_rcv_mtu: sock.kernel_rcv_mtu,
        }) {
            return Ok(());
        }
        if mtu < proto::MIN_MTU {
            if !on_event(Event::AudioUnavailable {
                mtu,
                needed: proto::MIN_MTU,
            }) {
                return Ok(());
            }
        }

        let mut held = Buttons::default();
        let mut voice = VoiceSession::default();

        // Replay anything that arrived while we waited for the MTU exchange.
        for (handle, data) in buffered {
            if handle == proto::H_INPUT {
                if let Report::Buttons(b) = proto::parse(&data) {
                    if !Self::emit_transitions(b, &mut held, &mut voice, on_event) {
                        return Ok(());
                    }
                }
            }
        }

        loop {
            let (handle, data) = match sock.next_notification(None) {
                Ok(v) => v,
                Err(e) => {
                    if voice.active {
                        if let Some(ev) = voice.finish() {
                            on_event(ev);
                        }
                    }
                    on_event(Event::Disconnected(e.to_string()));
                    return Err(e);
                }
            };

            match handle {
                proto::H_BATTERY if !data.is_empty() => {
                    if !on_event(Event::Battery(data[0])) {
                        return Ok(());
                    }
                }
                proto::H_POWER if !data.is_empty() => {
                    let st = match data[0] {
                        proto::POWER_CHARGING => PowerState::Charging,
                        proto::POWER_DISCHARGING => PowerState::Discharging,
                        proto::POWER_PLUGGED_IN => PowerState::PluggedIn,
                        other => PowerState::Unknown(other),
                    };
                    if !on_event(Event::Power(st)) {
                        return Ok(());
                    }
                }
                proto::H_INPUT => {
                    match proto::parse(&data) {
                        Report::Audio { opus, .. } => {
                            if let Some(pkt) = opus {
                                if let Some(pcm) = voice.push(&pkt) {
                                    if !on_event(Event::VoiceAudio(pcm)) {
                                        return Ok(());
                                    }
                                }
                            }
                        }
                        Report::Touch {
                            buttons,
                            fingers,
                            pressed,
                        } => {
                            if !on_event(Event::Touch { fingers, pressed }) {
                                return Ok(());
                            }
                            if !Self::emit_transitions(buttons, &mut held, &mut voice, on_event) {
                                return Ok(());
                            }
                        }
                        Report::Buttons(buttons) => {
                            if !Self::emit_transitions(buttons, &mut held, &mut voice, on_event) {
                                return Ok(());
                            }
                        }
                        Report::Unknown(_) => {}
                    }
                }
                _ => {}
            }
        }
    }

    fn emit_transitions(
        now: Buttons,
        held: &mut Buttons,
        voice: &mut VoiceSession,
        on_event: &mut impl FnMut(Event) -> bool,
    ) -> bool {
        for b in now.newly_pressed(*held) {
            if b == Button::Siri {
                voice.start();
                if !on_event(Event::VoiceStart) {
                    return false;
                }
            }
            if !on_event(Event::ButtonDown(b)) {
                return false;
            }
        }
        for b in now.newly_released(*held) {
            if !on_event(Event::ButtonUp(b)) {
                return false;
            }
            if b == Button::Siri {
                if let Some(ev) = voice.finish() {
                    if !on_event(ev) {
                        return false;
                    }
                }
            }
        }
        *held = now;
        true
    }
}

/// Accumulates a Siri press: decodes each frame as it arrives so the audio can
/// be streamed onward live.
struct VoiceSession {
    active: bool,
    started: Option<Instant>,
    frames: usize,
    undecodable: usize,
    pcm: Vec<i16>,
    #[cfg(feature = "opus")]
    decoder: Option<crate::audio::OpusDecoder>,
}

impl Default for VoiceSession {
    fn default() -> Self {
        VoiceSession {
            active: false,
            started: None,
            frames: 0,
            undecodable: 0,
            pcm: Vec::new(),
            #[cfg(feature = "opus")]
            decoder: None,
        }
    }
}

impl VoiceSession {
    fn start(&mut self) {
        *self = VoiceSession {
            active: true,
            started: Some(Instant::now()),
            ..Default::default()
        };
        #[cfg(feature = "opus")]
        {
            self.decoder =
                crate::audio::OpusDecoder::new(proto::AUDIO_SAMPLE_RATE, proto::AUDIO_CHANNELS)
                    .ok();
        }
    }

    fn push(&mut self, packet: &[u8]) -> Option<Vec<i16>> {
        if !self.active {
            // Audio without a Siri press: still count it rather than drop it
            // silently, since that would hide a protocol surprise.
            self.start();
        }
        self.frames += 1;
        #[cfg(feature = "opus")]
        {
            let dec = self.decoder.as_mut()?;
            match dec.decode(packet) {
                Ok(pcm) => {
                    self.pcm.extend_from_slice(&pcm);
                    return Some(pcm);
                }
                Err(_) => {
                    self.undecodable += 1;
                    return None;
                }
            }
        }
        #[cfg(not(feature = "opus"))]
        {
            let _ = packet;
            None
        }
    }

    fn finish(&mut self) -> Option<Event> {
        if !self.active {
            return None;
        }
        let ev = Event::VoiceEnd {
            pcm: std::mem::take(&mut self.pcm),
            frames: self.frames,
            undecodable: self.undecodable,
            held: self.started.map(|s| s.elapsed()).unwrap_or_default(),
        };
        *self = VoiceSession::default();
        Some(ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_states_are_named() {
        assert_eq!(PowerState::Charging.as_str(), "charging");
        assert_eq!(PowerState::PluggedIn.as_str(), "plugged in");
    }

    #[test]
    fn voice_session_reports_nothing_when_idle() {
        let mut v = VoiceSession::default();
        assert!(v.finish().is_none());
    }

    #[test]
    fn voice_session_tracks_frames() {
        let mut v = VoiceSession::default();
        v.start();
        assert!(v.active);
        match v.finish() {
            Some(Event::VoiceEnd { frames, .. }) => assert_eq!(frames, 0),
            other => panic!("expected VoiceEnd, got {other:?}"),
        }
        assert!(!v.active);
    }
}
