//! A connected remote: handshake, then a stream of decoded events.

use std::time::{Duration, Instant};

use crate::att::{AttSocket, BDADDR_LE_PUBLIC, BT_SECURITY_MEDIUM};
use crate::error::{Error, Result};
use crate::hci::ConnParams;
use crate::protocol::{self as proto, AudioFrame, Button, Buttons, Codec, Finger, Report};

/// After Siri is released the remote keeps streaming a short tail, then simply
/// stops — the A1513 sends no end marker. A gap this long ends the utterance.
const TAIL_TIMEOUT: Duration = Duration::from_millis(600);

/// Conceal at most this many consecutive lost frames (0.5 s). A bigger jump in
/// stream time means a new stream rather than a hole in this one.
const MAX_CONCEALED_RUN: usize = 25;

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
        /// Negotiated ATT MTU. Informational: the remote answers 23 to every
        /// host and sends larger audio notifications regardless.
        mtu: u16,
        /// Largest PDU the kernel will accept on this link. Below
        /// [`proto::MIN_MTU`] audio frames would be truncated — see
        /// [`Event::AudioUnavailable`].
        kernel_rcv_mtu: u16,
    },
    /// The kernel socket cannot accept whole microphone frames. Buttons and
    /// touch still work.
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
    /// consume it live rather than waiting for the utterance to end. Lost
    /// frames are filled with concealment, so the chunks keep real time.
    VoiceAudio(Vec<i16>),
    /// The utterance is over: Siri was released and the remote's tail has
    /// finished streaming.
    VoiceEnd {
        pcm: Vec<i16>,
        /// Frames received.
        frames: usize,
        /// Frames received but not decodable.
        undecodable: usize,
        /// Frames lost in transit and filled in by the decoder.
        concealed: usize,
        /// How long Siri was held.
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
    /// choose, which is only unambiguous with a single adapter.
    pub adapter: Option<String>,
    /// Connection parameters to request once connected. The kernel's default
    /// interval is too slow for the microphone, which then lags further behind
    /// the longer you speak. Needs `CAP_NET_RAW`; without it the link keeps
    /// the default and everything else still works. `None` leaves it alone.
    pub conn_params: Option<ConnParams>,
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
            conn_params: Some(ConnParams::VOICE),
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

        sock.write_req(proto::H_ENABLE, &[proto::ENABLE_BYTE])?;
        let mtu = sock.exchange_mtu(cfg.want_mtu).unwrap_or(23);

        for cccd in [
            proto::H_INPUT_CCCD,
            proto::H_BATTERY_CCCD,
            proto::H_POWER_CCCD,
            proto::H_VENDOR_CCCD,
        ] {
            // A remote without the vendor service simply rejects that one.
            let _ = sock.write_req(cccd, &[0x01, 0x00]);
        }

        if let Some(params) = &cfg.conn_params {
            match sock.request_conn_params(params) {
                Ok(handle) => tracing::debug!(handle, ?params, "requested connection parameters"),
                Err(e) => tracing::warn!(
                    "could not request a faster connection interval ({e}); \
                     microphone audio will lag on long utterances"
                ),
            }
        }

        if !on_event(Event::Connected {
            mtu,
            kernel_rcv_mtu: sock.kernel_rcv_mtu,
        }) {
            return Ok(());
        }
        if sock.kernel_rcv_mtu < proto::MIN_MTU {
            if !on_event(Event::AudioUnavailable {
                mtu: sock.kernel_rcv_mtu,
                needed: proto::MIN_MTU,
            }) {
                return Ok(());
            }
        }

        let mut held = Buttons::default();
        let mut voice = VoiceSession::default();

        loop {
            let timeout = voice.awaiting_tail().then_some(TAIL_TIMEOUT);
            let (handle, data) = match sock.next_notification(timeout) {
                Ok(v) => v,
                Err(Error::Io(e))
                    if timeout.is_some() && e.raw_os_error() == Some(libc::ETIMEDOUT) =>
                {
                    // The tail has run out: the utterance is complete.
                    if let Some(ev) = voice.finish() {
                        if !on_event(ev) {
                            return Ok(());
                        }
                    }
                    continue;
                }
                Err(e) => {
                    if let Some(ev) = voice.finish() {
                        on_event(ev);
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
                    let report = proto::parse(&data);

                    // The remote sends nothing but audio while it streams, so
                    // any other report after a release means the stream is
                    // over and the tail has been delivered.
                    if voice.awaiting_tail() && !matches!(report, Report::Audio { .. }) {
                        if let Some(ev) = voice.finish() {
                            if !on_event(ev) {
                                return Ok(());
                            }
                        }
                    }

                    match report {
                        Report::Audio { buttons, frame, .. } => {
                            // Presses and releases made mid-stream only show
                            // up here, in the frame's button byte.
                            if !Self::emit_transitions(buttons, &mut held, &mut voice, &mut sock, on_event) {
                                return Ok(());
                            }
                            if let Some(frame) = frame {
                                for pcm in voice.push(&frame) {
                                    if !on_event(Event::VoiceAudio(pcm)) {
                                        return Ok(());
                                    }
                                }
                            }
                        }
                        Report::AudioEnd(buttons) => {
                            if !Self::emit_transitions(buttons, &mut held, &mut voice, &mut sock, on_event) {
                                return Ok(());
                            }
                            if let Some(ev) = voice.finish() {
                                if !on_event(ev) {
                                    return Ok(());
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
                            if !Self::emit_transitions(buttons, &mut held, &mut voice, &mut sock, on_event) {
                                return Ok(());
                            }
                        }
                        Report::Buttons(buttons) => {
                            if !Self::emit_transitions(buttons, &mut held, &mut voice, &mut sock, on_event) {
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
        sock: &mut AttSocket,
        on_event: &mut impl FnMut(Event) -> bool,
    ) -> bool {
        for b in now.newly_pressed(*held) {
            if b == Button::Siri {
                // A press during the previous utterance's tail starts a new
                // one; the remote just keeps streaming.
                if let Some(ev) = voice.finish() {
                    if !on_event(ev) {
                        return false;
                    }
                }
                voice.start();
                // Siri only arms the microphone; this starts the stream.
                if let Err(e) = sock.write_req(proto::H_ENABLE, &proto::STREAM_START) {
                    tracing::warn!("could not start the microphone stream: {e}");
                }
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
                voice.release();
            }
        }
        *held = now;
        true
    }
}

/// A frame decoder for whichever codec this remote uses.
enum Decoder {
    #[cfg(feature = "opus")]
    Opus(crate::audio::OpusDecoder),
    #[cfg(feature = "speex")]
    Speex(crate::audio::SpeexDecoder),
}

impl Decoder {
    fn new(codec: Codec) -> Option<Decoder> {
        match codec {
            #[cfg(feature = "opus")]
            Codec::Opus => {
                crate::audio::OpusDecoder::new(proto::AUDIO_SAMPLE_RATE, proto::AUDIO_CHANNELS)
                    .ok()
                    .map(Decoder::Opus)
            }
            #[cfg(feature = "speex")]
            Codec::Speex => crate::audio::SpeexDecoder::new().ok().map(Decoder::Speex),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    // `match *self` so this still compiles with both codecs disabled, when
    // `Decoder` has no variants at all.
    fn codec(&self) -> Codec {
        match *self {
            #[cfg(feature = "opus")]
            Decoder::Opus(_) => Codec::Opus,
            #[cfg(feature = "speex")]
            Decoder::Speex(_) => Codec::Speex,
        }
    }

    #[cfg_attr(not(any(feature = "opus", feature = "speex")), allow(unused_variables))]
    fn decode(&mut self, payload: &[u8]) -> Option<Vec<i16>> {
        match *self {
            #[cfg(feature = "opus")]
            Decoder::Opus(ref mut d) => d.decode(payload).ok(),
            #[cfg(feature = "speex")]
            Decoder::Speex(ref mut d) => d.decode(payload).ok(),
        }
    }

    fn conceal(&mut self) -> Option<Vec<i16>> {
        match *self {
            #[cfg(feature = "opus")]
            Decoder::Opus(ref mut d) => d.conceal().ok(),
            #[cfg(feature = "speex")]
            Decoder::Speex(ref mut d) => d.conceal().ok(),
        }
    }
}

/// Accumulates one utterance: decodes each frame as it arrives so the audio
/// can be streamed onward live.
#[derive(Default)]
struct VoiceSession {
    active: bool,
    started: Option<Instant>,
    released: Option<Instant>,
    frames: usize,
    undecodable: usize,
    concealed: usize,
    last_time: Option<f32>,
    pcm: Vec<i16>,
    decoder: Option<Decoder>,
}

impl VoiceSession {
    fn start(&mut self) {
        *self = VoiceSession {
            active: true,
            started: Some(Instant::now()),
            ..Default::default()
        };
    }

    fn release(&mut self) {
        if self.active && self.released.is_none() {
            self.released = Some(Instant::now());
        }
    }

    /// Siri is up and the tail is still arriving.
    fn awaiting_tail(&self) -> bool {
        self.active && self.released.is_some()
    }

    /// Lost frames before `frame`, from the jump in stream time.
    fn missing_before(&self, frame: &AudioFrame) -> usize {
        let (Some(prev), Some(now)) = (self.last_time, frame.time) else {
            return 0;
        };
        let steps = ((now - prev) / proto::AUDIO_FRAME_SECONDS).round() as i64;
        if steps > 1 && (steps - 1) as usize <= MAX_CONCEALED_RUN {
            (steps - 1) as usize
        } else {
            0
        }
    }

    /// Decode one frame, preceded by concealment for any frames lost before
    /// it. Returns the PCM chunks in order.
    fn push(&mut self, frame: &AudioFrame) -> Vec<Vec<i16>> {
        if !self.active {
            // Audio without a Siri press: still collect it rather than drop it
            // silently, since that would hide a protocol surprise.
            self.start();
        }
        self.frames += 1;

        if self.decoder.as_ref().map(Decoder::codec) != Some(frame.codec) {
            self.decoder = Decoder::new(frame.codec);
        }
        let missing = self.missing_before(frame);
        if frame.time.is_some() {
            self.last_time = frame.time;
        }

        let mut out = Vec::new();
        let Some(dec) = self.decoder.as_mut() else {
            self.undecodable += 1;
            return out;
        };
        for _ in 0..missing {
            if let Some(pcm) = dec.conceal() {
                self.concealed += 1;
                self.pcm.extend_from_slice(&pcm);
                out.push(pcm);
            }
        }
        match dec.decode(&frame.payload) {
            Some(pcm) => {
                self.pcm.extend_from_slice(&pcm);
                out.push(pcm);
            }
            None => self.undecodable += 1,
        }
        out
    }

    fn finish(&mut self) -> Option<Event> {
        if !self.active {
            return None;
        }
        let held = match (self.started, self.released) {
            (Some(s), Some(r)) => r.duration_since(s),
            (Some(s), None) => s.elapsed(),
            _ => Duration::ZERO,
        };
        let ev = Event::VoiceEnd {
            pcm: std::mem::take(&mut self.pcm),
            frames: self.frames,
            undecodable: self.undecodable,
            concealed: self.concealed,
            held,
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

    #[test]
    fn utterance_stays_open_through_the_tail() {
        let mut v = VoiceSession::default();
        v.start();
        assert!(!v.awaiting_tail());
        v.release();
        assert!(v.awaiting_tail(), "release waits for the tail, not ends");
        assert!(v.finish().is_some());
        assert!(!v.awaiting_tail());
    }

    fn speex_at(time: f32) -> AudioFrame {
        AudioFrame {
            codec: Codec::Speex,
            payload: vec![],
            time: Some(time),
        }
    }

    #[test]
    fn gaps_in_stream_time_count_as_lost_frames() {
        let mut v = VoiceSession::default();
        v.last_time = Some(0.10);
        assert_eq!(v.missing_before(&speex_at(0.12)), 0);
        assert_eq!(v.missing_before(&speex_at(0.16)), 2);
        // A jump this large is a new stream, not a hole to fill.
        assert_eq!(v.missing_before(&speex_at(5.0)), 0);
        // Time going backwards is a restart, not loss.
        assert_eq!(v.missing_before(&speex_at(0.02)), 0);
    }

    #[cfg(feature = "speex")]
    #[test]
    fn concealment_fills_lost_frames_with_audio() {
        use crate::protocol::test_frames::{unhex, SPEEX_NOTIFICATION};
        let Report::Audio { frame: Some(first), .. } = proto::parse(&unhex(SPEEX_NOTIFICATION)) else {
            panic!("fixture is not an audio frame");
        };
        let mut v = VoiceSession::default();
        v.start();
        assert_eq!(v.push(&first).len(), 1);
        // Same frame, three steps later: two frames went missing.
        let mut later = first.clone();
        later.time = first.time.map(|t| t + 3.0 * proto::AUDIO_FRAME_SECONDS);
        let chunks = v.push(&later);
        assert_eq!(chunks.len(), 3, "two concealed, one decoded");
        assert!(chunks.iter().all(|c| c.len() == 320));
        match v.finish() {
            Some(Event::VoiceEnd { frames, concealed, pcm, .. }) => {
                assert_eq!(frames, 2);
                assert_eq!(concealed, 2);
                assert_eq!(pcm.len(), 4 * 320);
            }
            other => panic!("expected VoiceEnd, got {other:?}"),
        }
    }
}
