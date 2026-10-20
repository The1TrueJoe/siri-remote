//! Wire protocol of the Apple TV Siri Remote.
//!
//! Everything the remote sends arrives as notifications on a single
//! characteristic (value handle [`H_INPUT`]) and is told apart by payload
//! length:
//!
//! | Length | Meaning |
//! |--------|---------|
//! | 2      | button bitmask only |
//! | 13     | one finger on the touchpad |
//! | 20     | two fingers |
//! | ~101   | Opus-encoded microphone audio |
//!
//! This module is pure logic so it can be unit tested without hardware.

/// Write [`ENABLE_BYTE`] here to unlock the input stream. Until this happens
/// the remote reports nothing but zeros.
pub const H_ENABLE: u16 = 0x001D;
/// Buttons, touch and audio all arrive here.
pub const H_INPUT: u16 = 0x0023;
pub const H_INPUT_CCCD: u16 = 0x0024;
pub const H_BATTERY: u16 = 0x0028;
pub const H_BATTERY_CCCD: u16 = 0x0029;
pub const H_POWER: u16 = 0x002B;
pub const H_POWER_CCCD: u16 = 0x002C;
/// Apple vendor service characteristic. Purpose undocumented; subscribed so
/// anything routed here is visible rather than silently dropped.
pub const H_VENDOR_CCCD: u16 = 0x003B;

/// The vendor handshake byte.
pub const ENABLE_BYTE: u8 = 0xAF;

/// Microphone frames are 101 bytes, so the ATT MTU must be at least this
/// (101 payload + 3 bytes of ATT header) or audio cannot arrive at all.
pub const MIN_MTU: u16 = 104;

pub const AUDIO_SAMPLE_RATE: u32 = 16_000;
pub const AUDIO_CHANNELS: u8 = 1;
/// Opus TOC 0xB8 = config 23 (CELT, wideband 16 kHz, 20 ms), mono, 1 frame.
pub const OPUS_TOC_CONFIG: u8 = 23;

pub const POWER_CHARGING: u8 = 0xAB;
pub const POWER_DISCHARGING: u8 = 0xAF;
pub const POWER_PLUGGED_IN: u8 = 0xBB;

const TOUCH_EVENT_MARKER: u8 = 50;
const TOUCHPAD_BIT: u8 = 0x80;
const TOUCHPAD_2_BIT: u8 = 0x40;

/// A physical control on the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Button {
    AirPlay,
    VolumeUp,
    VolumeDown,
    PlayPause,
    Siri,
    Menu,
    /// Two-finger click. Synthesised: the remote reports finger count 2 plus
    /// the touchpad bit.
    TouchpadTwoFinger,
    Touchpad,
}

impl Button {
    pub const ALL: [(u8, Button); 8] = [
        (0x01, Button::AirPlay),
        (0x02, Button::VolumeUp),
        (0x04, Button::VolumeDown),
        (0x08, Button::PlayPause),
        (0x10, Button::Siri),
        (0x20, Button::Menu),
        (0x40, Button::TouchpadTwoFinger),
        (0x80, Button::Touchpad),
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Button::AirPlay => "AIRPLAY",
            Button::VolumeUp => "VOLUME_UP",
            Button::VolumeDown => "VOLUME_DOWN",
            Button::PlayPause => "PLAY_PAUSE",
            Button::Siri => "SIRI",
            Button::Menu => "MENU",
            Button::TouchpadTwoFinger => "TOUCHPAD_2",
            Button::Touchpad => "TOUCHPAD",
        }
    }

    pub fn from_str(s: &str) -> Option<Button> {
        Button::ALL
            .iter()
            .map(|(_, b)| *b)
            .find(|b| b.as_str().eq_ignore_ascii_case(s))
    }
}

/// Set of buttons currently held, as a bitmask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons(pub u8);

impl Buttons {
    pub fn contains(&self, b: Button) -> bool {
        Button::ALL
            .iter()
            .find(|(_, x)| *x == b)
            .map(|(bit, _)| self.0 & bit != 0)
            .unwrap_or(false)
    }

    pub fn iter(&self) -> impl Iterator<Item = Button> + '_ {
        Button::ALL
            .iter()
            .filter(move |(bit, _)| self.0 & bit != 0)
            .map(|(_, b)| *b)
    }

    /// Buttons in `self` but not in `other` — i.e. newly pressed.
    pub fn newly_pressed(&self, other: Buttons) -> impl Iterator<Item = Button> + '_ {
        let prev = other.0;
        Button::ALL
            .iter()
            .filter(move |(bit, _)| self.0 & bit != 0 && prev & bit == 0)
            .map(|(_, b)| *b)
    }

    /// Buttons released since `other`.
    pub fn newly_released(&self, other: Buttons) -> impl Iterator<Item = Button> + '_ {
        let now = self.0;
        Button::ALL
            .iter()
            .filter(move |(bit, _)| other.0 & bit != 0 && now & bit == 0)
            .map(|(_, b)| *b)
    }
}

/// One finger on the touchpad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finger {
    pub x: i32,
    pub y: i32,
    pub pressure: u8,
}

/// A decoded notification.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    Buttons(Buttons),
    Touch {
        buttons: Buttons,
        fingers: Vec<Finger>,
        pressed: bool,
    },
    /// A microphone frame. `opus` is the extracted Opus packet, if one was
    /// found in the payload.
    Audio {
        opus: Option<Vec<u8>>,
        raw_len: usize,
    },
    Unknown(usize),
}

/// Decode one 7-byte finger record.
///
/// The X axis spans two bytes but only the low 3 bits of the second are used.
/// Y wraps as a signed byte: values run 188..=255 then 0..=38, and its
/// resolution is noticeably coarser than X.
pub fn decode_finger(f: &[u8]) -> Finger {
    let x = ((f[0] as i32 + 255 * (f[1] as i32 & 7)) - 230) / 15;
    let y = (if f[2] & 128 != 0 { f[2] as i32 } else { f[2] as i32 + 255 }) - 188;
    Finger { x, y, pressure: f[5] }
}

pub fn is_opus_toc(b: u8) -> bool {
    (b >> 3) == OPUS_TOC_CONFIG
}

/// Extract the length-prefixed Opus packet from an audio notification.
///
/// The frame is laid out `[len][TOC ...opus...]`. Rather than hard-coding an
/// offset, locate the TOC byte and validate it against the length prefix that
/// precedes it — a false positive would have to be a byte that both looks like
/// a TOC and is preceded by a byte matching the remaining length.
pub fn extract_opus(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    for i in 1..data.len() {
        if !is_opus_toc(data[i]) {
            continue;
        }
        let len = data[i - 1] as usize;
        if len == 0 {
            continue;
        }
        if i + len <= data.len() {
            return Some((data[i..i + len].to_vec(), i));
        }
    }
    None
}

/// Parse one notification payload from [`H_INPUT`].
pub fn parse(data: &[u8]) -> Report {
    let n = data.len();

    if n >= 40 {
        let opus = extract_opus(data).map(|(p, _)| p);
        return Report::Audio { opus, raw_len: n };
    }
    if n < 2 {
        return Report::Unknown(n);
    }

    let mut mask = data[1];
    // A two-finger click reports as touchpad with a finger count of 2.
    if n >= 3 && data[0] == 2 && mask & TOUCHPAD_BIT != 0 {
        mask = (mask & !TOUCHPAD_BIT) | TOUCHPAD_2_BIT;
    }
    let buttons = Buttons(mask);

    if n >= 3 && data[2] == TOUCH_EVENT_MARKER {
        let mut fingers = Vec::new();
        if n >= 13 {
            fingers.push(decode_finger(&data[6..13]));
        }
        if n >= 20 {
            fingers.push(decode_finger(&data[13..20]));
        }
        if !fingers.is_empty() {
            return Report::Touch {
                buttons,
                fingers,
                pressed: data[1] & TOUCHPAD_BIT != 0,
            };
        }
    }
    Report::Buttons(buttons)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buttons_of(data: &[u8]) -> Vec<Button> {
        match parse(data) {
            Report::Buttons(b) | Report::Touch { buttons: b, .. } => b.iter().collect(),
            _ => vec![],
        }
    }

    #[test]
    fn decodes_single_buttons() {
        assert_eq!(buttons_of(&[0x00, 0x20]), vec![Button::Menu]);
        assert_eq!(buttons_of(&[0x00, 0x10]), vec![Button::Siri]);
        assert!(buttons_of(&[0x00, 0x00]).is_empty());
    }

    #[test]
    fn decodes_chords() {
        assert_eq!(
            buttons_of(&[0x00, 0x03]),
            vec![Button::AirPlay, Button::VolumeUp]
        );
    }

    #[test]
    fn promotes_two_finger_click() {
        assert_eq!(
            buttons_of(&[0x02, 0x80, 0x00]),
            vec![Button::TouchpadTwoFinger]
        );
        assert_eq!(buttons_of(&[0x01, 0x80, 0x00]), vec![Button::Touchpad]);
    }

    #[test]
    fn decodes_one_finger_touch() {
        let mut t = [0u8; 13];
        t[0] = 1;
        t[1] = 0x80;
        t[2] = 50;
        t[6] = 230;
        t[8] = 188;
        t[11] = 42;
        match parse(&t) {
            Report::Touch { fingers, pressed, .. } => {
                assert_eq!(fingers.len(), 1);
                assert_eq!(fingers[0], Finger { x: 0, y: 0, pressure: 42 });
                assert!(pressed);
            }
            other => panic!("expected touch, got {other:?}"),
        }
    }

    #[test]
    fn decodes_two_finger_touch() {
        let mut t = [0u8; 20];
        t[0] = 2;
        t[2] = 50;
        t[6] = 230;
        t[8] = 188;
        // x = 10 needs f[0] + 255*(f[1]&7) == 380
        t[13] = 125;
        t[14] = 1;
        t[15] = 200;
        match parse(&t) {
            Report::Touch { fingers, .. } => {
                assert_eq!(fingers.len(), 2);
                assert_eq!(fingers[1].x, 10);
            }
            other => panic!("expected touch, got {other:?}"),
        }
    }

    #[test]
    fn y_axis_wraps_like_a_signed_byte() {
        assert_eq!(decode_finger(&[230, 0, 188, 0, 0, 0, 0]).y, 0);
        assert_eq!(decode_finger(&[230, 0, 255, 0, 0, 0, 0]).y, 67);
        assert_eq!(decode_finger(&[230, 0, 38, 0, 0, 0, 0]).y, 105);
    }

    #[test]
    fn finds_opus_packet_in_audio_frame() {
        let mut opus = vec![0xB8u8];
        opus.extend(0..85u8);
        let mut pkt = vec![0x00, 0x00, 0x00, 86];
        pkt.extend_from_slice(&opus);
        match parse(&pkt) {
            Report::Audio { opus: Some(p), .. } => assert_eq!(p, opus),
            other => panic!("expected audio, got {other:?}"),
        }
    }

    #[test]
    fn recognises_the_expected_opus_toc() {
        assert!(is_opus_toc(0xB8));
        assert_eq!(0xB8u8 >> 3, OPUS_TOC_CONFIG);
        assert!(!is_opus_toc(0x00));
    }

    #[test]
    fn malformed_payloads_do_not_panic() {
        for p in [
            vec![],
            vec![0u8],
            vec![0x01, 0x80, 50],
            vec![0u8; 101],
            {
                let mut v = vec![0, 0, 0, 200, 0xB8];
                v.extend([0u8; 40]);
                v
            },
        ] {
            let _ = parse(&p);
        }
    }

    #[test]
    fn tracks_press_and_release_transitions() {
        let prev = Buttons(0x20);
        let now = Buttons(0x10);
        assert_eq!(now.newly_pressed(prev).collect::<Vec<_>>(), vec![Button::Siri]);
        assert_eq!(now.newly_released(prev).collect::<Vec<_>>(), vec![Button::Menu]);
    }

    #[test]
    fn button_names_round_trip() {
        for (_, b) in Button::ALL {
            assert_eq!(Button::from_str(b.as_str()), Some(b));
        }
    }
}
