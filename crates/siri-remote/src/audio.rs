//! Decoding for the remote's microphone stream.
//!
//! Both hardware revisions send 16 kHz mono in 20 ms frames — exactly the
//! format speech recognisers such as Vosk expect, so nothing needs
//! resampling — but in different codecs:
//!
//! * **A1513** (HID product `0x0266`): Speex wideband. [`SpeexDecoder`],
//!   feature `speex`, links `libspeex`.
//! * **A1962** (HID product `0x026D`): Opus, TOC byte `0xB8` = config 23
//!   (CELT, wideband), mono, one frame per packet. [`OpusDecoder`], feature
//!   `opus`, links `libopus`.
//!
//! Both libraries are called through small `extern "C"` blocks rather than
//! wrapper crates: the C APIs are tiny and stable, and this keeps the
//! dependency tree and build requirements down to the system libraries.

#[cfg(feature = "opus")]
pub use opus::OpusDecoder;
#[cfg(feature = "speex")]
pub use speex::SpeexDecoder;

#[cfg(feature = "opus")]
mod opus {
    use crate::error::{Error, Result};
    use std::os::raw::{c_char, c_int, c_void};

    /// 120 ms at 48 kHz — the maximum any Opus packet can decode to.
    const MAX_FRAME_SAMPLES: usize = 5760;
    /// One 20 ms frame at 16 kHz, the length concealment fills in.
    const FRAME_SAMPLES: usize = 320;

    #[link(name = "opus")]
    extern "C" {
        fn opus_decoder_create(fs: i32, channels: c_int, error: *mut c_int) -> *mut c_void;
        fn opus_decoder_destroy(st: *mut c_void);
        fn opus_decode(
            st: *mut c_void,
            data: *const u8,
            len: i32,
            pcm: *mut i16,
            frame_size: c_int,
            decode_fec: c_int,
        ) -> c_int;
        fn opus_strerror(error: c_int) -> *const c_char;
    }

    fn strerror(code: c_int) -> String {
        unsafe {
            let p = opus_strerror(code);
            if p.is_null() {
                return format!("opus error {code}");
            }
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }

    pub struct OpusDecoder {
        st: *mut c_void,
        channels: usize,
        buf: Vec<i16>,
    }

    // The decoder is owned exclusively and libopus keeps no global state.
    unsafe impl Send for OpusDecoder {}

    impl OpusDecoder {
        pub fn new(sample_rate: u32, channels: u8) -> Result<Self> {
            let mut err: c_int = 0;
            let st =
                unsafe { opus_decoder_create(sample_rate as i32, channels as c_int, &mut err) };
            if st.is_null() || err != 0 {
                return Err(Error::Opus(strerror(err)));
            }
            Ok(OpusDecoder {
                st,
                channels: channels as usize,
                buf: vec![0i16; MAX_FRAME_SAMPLES * channels as usize],
            })
        }

        /// Decode one Opus packet into 16-bit PCM samples.
        pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>> {
            let n = unsafe {
                opus_decode(
                    self.st,
                    packet.as_ptr(),
                    packet.len() as i32,
                    self.buf.as_mut_ptr(),
                    MAX_FRAME_SAMPLES as c_int,
                    0,
                )
            };
            if n < 0 {
                return Err(Error::Opus(strerror(n)));
            }
            Ok(self.buf[..n as usize * self.channels].to_vec())
        }

        /// Synthesise one frame in place of a lost one (packet loss concealment).
        pub fn conceal(&mut self) -> Result<Vec<i16>> {
            let n = unsafe {
                opus_decode(
                    self.st,
                    std::ptr::null(),
                    0,
                    self.buf.as_mut_ptr(),
                    FRAME_SAMPLES as c_int,
                    0,
                )
            };
            if n < 0 {
                return Err(Error::Opus(strerror(n)));
            }
            Ok(self.buf[..n as usize * self.channels].to_vec())
        }
    }

    impl Drop for OpusDecoder {
        fn drop(&mut self) {
            if !self.st.is_null() {
                unsafe { opus_decoder_destroy(self.st) };
                self.st = std::ptr::null_mut();
            }
        }
    }
}

#[cfg(feature = "speex")]
mod speex {
    use crate::error::{Error, Result};
    use std::os::raw::{c_char, c_int, c_void};

    const SPEEX_MODEID_WB: c_int = 1;
    const SPEEX_SET_ENH: c_int = 0;
    const SPEEX_GET_FRAME_SIZE: c_int = 3;

    /// `struct SpeexBits` from `speex/speex_bits.h`.
    #[repr(C)]
    struct SpeexBits {
        chars: *mut c_char,
        nb_bits: c_int,
        char_ptr: c_int,
        bit_ptr: c_int,
        owner: c_int,
        overflow: c_int,
        buf_size: c_int,
        reserved1: c_int,
        reserved2: *mut c_void,
    }

    #[link(name = "speex")]
    extern "C" {
        fn speex_lib_get_mode(mode: c_int) -> *const c_void;
        fn speex_decoder_init(mode: *const c_void) -> *mut c_void;
        fn speex_decoder_destroy(st: *mut c_void);
        fn speex_decoder_ctl(st: *mut c_void, request: c_int, ptr: *mut c_void) -> c_int;
        fn speex_decode_int(st: *mut c_void, bits: *mut SpeexBits, out: *mut i16) -> c_int;
        fn speex_bits_init(bits: *mut SpeexBits);
        fn speex_bits_destroy(bits: *mut SpeexBits);
        fn speex_bits_read_from(bits: *mut SpeexBits, bytes: *const c_char, len: c_int);
    }

    /// Speex wideband (16 kHz) decoder for the A1513's microphone frames.
    pub struct SpeexDecoder {
        st: *mut c_void,
        // Boxed so its address stays put: libspeex keeps pointers into it.
        bits: Box<SpeexBits>,
        frame_size: usize,
        buf: Vec<i16>,
    }

    // The decoder is owned exclusively and libspeex keeps no global state.
    unsafe impl Send for SpeexDecoder {}

    impl SpeexDecoder {
        pub fn new() -> Result<Self> {
            unsafe {
                let mode = speex_lib_get_mode(SPEEX_MODEID_WB);
                if mode.is_null() {
                    return Err(Error::Speex("wideband mode unavailable".into()));
                }
                let st = speex_decoder_init(mode);
                if st.is_null() {
                    return Err(Error::Speex("decoder init failed".into()));
                }
                // Perceptual enhancement: a postfilter that noticeably cleans up
                // speech at this bitrate.
                let mut on: c_int = 1;
                speex_decoder_ctl(st, SPEEX_SET_ENH, &mut on as *mut c_int as *mut c_void);
                let mut fs: c_int = 0;
                speex_decoder_ctl(
                    st,
                    SPEEX_GET_FRAME_SIZE,
                    &mut fs as *mut c_int as *mut c_void,
                );

                let mut bits = Box::new(std::mem::zeroed::<SpeexBits>());
                speex_bits_init(&mut *bits);
                Ok(SpeexDecoder {
                    st,
                    bits,
                    frame_size: fs.max(1) as usize,
                    buf: vec![0i16; fs.max(1) as usize],
                })
            }
        }

        /// Decode one Speex frame into 16-bit PCM samples.
        pub fn decode(&mut self, frame: &[u8]) -> Result<Vec<i16>> {
            let rc = unsafe {
                speex_bits_read_from(
                    &mut *self.bits,
                    frame.as_ptr() as *const c_char,
                    frame.len() as c_int,
                );
                speex_decode_int(self.st, &mut *self.bits, self.buf.as_mut_ptr())
            };
            match rc {
                0 => Ok(self.buf[..self.frame_size].to_vec()),
                -1 => Err(Error::Speex("end of stream".into())),
                _ => Err(Error::Speex("corrupt frame".into())),
            }
        }

        /// Synthesise one frame in place of a lost one (packet loss concealment).
        pub fn conceal(&mut self) -> Result<Vec<i16>> {
            let rc =
                unsafe { speex_decode_int(self.st, std::ptr::null_mut(), self.buf.as_mut_ptr()) };
            if rc != 0 {
                return Err(Error::Speex("concealment failed".into()));
            }
            Ok(self.buf[..self.frame_size].to_vec())
        }
    }

    impl Drop for SpeexDecoder {
        fn drop(&mut self) {
            unsafe {
                speex_bits_destroy(&mut *self.bits);
                if !self.st.is_null() {
                    speex_decoder_destroy(self.st);
                    self.st = std::ptr::null_mut();
                }
            }
        }
    }
}

/// Serialise PCM as a RIFF/WAVE file.
pub fn wav_bytes(pcm: &[i16], sample_rate: u32, channels: u8) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let byte_rate = sample_rate * channels as u32 * 2;
    let mut w = Vec::with_capacity(44 + pcm.len() * 2);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // format = PCM
    w.extend_from_slice(&(channels as u16).to_le_bytes());
    w.extend_from_slice(&sample_rate.to_le_bytes());
    w.extend_from_slice(&byte_rate.to_le_bytes());
    w.extend_from_slice(&((channels as u16) * 2).to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn wav_header_is_well_formed() {
        let pcm = vec![0i16; 160];
        let w = wav_bytes(&pcm, 16000, 1);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(w.len(), 44 + 320);
        assert_eq!(u32::from_le_bytes([w[24], w[25], w[26], w[27]]), 16000);
        assert_eq!(u16::from_le_bytes([w[22], w[23]]), 1);
        assert_eq!(u32::from_le_bytes([w[40], w[41], w[42], w[43]]), 320);
    }

    #[cfg(feature = "opus")]
    #[test]
    fn decoder_round_trips_a_real_frame() {
        // A genuine 16 kHz mono frame captured from a Siri Remote: TOC 0xB8.
        let frame: Vec<u8> = vec![
            0xB8, 0x4C, 0xDD, 0x3F, 0xC2, 0xF9, 0xB5, 0xBA, 0xCA, 0x94, 0x28, 0x87, 0xE0, 0x77,
            0xAA, 0xA7, 0xFB, 0xDD, 0x14, 0xCC, 0x2A, 0x11, 0xC6, 0xF1, 0xF0, 0xCA, 0x7B, 0xA6,
            0x26, 0xFF, 0x8B, 0x29, 0xD6, 0x6B, 0xF3, 0x1E, 0x97, 0xA4, 0x84, 0x68, 0xF2, 0x60,
            0x3E, 0x66, 0x66, 0x33, 0x01, 0xD8, 0x5F, 0x38, 0x17, 0x90, 0xC3, 0x8B, 0x9D, 0xC6,
            0xB0, 0xB0, 0xD4, 0xAF, 0x48, 0x60, 0xF0, 0x90, 0x19, 0x6A, 0x1C, 0x9D, 0x88, 0x14,
            0x6D, 0x50, 0x8C, 0x1A, 0xC8, 0x9D, 0xA1, 0xB6, 0xB9, 0x62, 0x19, 0xF8, 0x7B, 0x80,
            0x2E, 0xE5,
        ];
        let mut d = OpusDecoder::new(16000, 1).expect("decoder");
        let pcm = d.decode(&frame).expect("decode");
        // 20 ms at 16 kHz
        assert_eq!(pcm.len(), 320);
        assert!(pcm.iter().any(|&s| s != 0), "decoded frame is silent");
    }

    #[cfg(feature = "speex")]
    #[test]
    fn speex_decodes_a_real_a1513_frame() {
        use crate::protocol::extract_speex;
        use crate::protocol::test_frames::{unhex, SPEEX_NOTIFICATION};
        let (frame, _) = extract_speex(&unhex(SPEEX_NOTIFICATION)).expect("speex frame");
        let mut d = SpeexDecoder::new().expect("decoder");
        let pcm = d.decode(&frame).expect("decode");
        assert_eq!(pcm.len(), 320, "20 ms at 16 kHz");
        let concealed = d.conceal().expect("conceal");
        assert_eq!(concealed.len(), 320);
    }
}
