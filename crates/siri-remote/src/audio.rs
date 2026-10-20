//! Opus decoding for the remote's microphone stream.
//!
//! The remote emits Opus at 16 kHz mono in 20 ms frames — TOC byte `0xB8` is
//! config 23 (CELT, wideband), mono, one frame per packet. That is exactly the
//! format speech recognisers such as Vosk expect, so nothing needs resampling.
//!
//! libopus is called through a small `extern "C"` block rather than a wrapper
//! crate: the C API is tiny and stable, and this keeps the dependency tree and
//! build requirements down to `libopus0`.

use crate::error::{Error, Result};
use std::os::raw::{c_char, c_int, c_void};

/// 120 ms at 48 kHz — the maximum any Opus packet can decode to.
const MAX_FRAME_SAMPLES: usize = 5760;

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
        let st = unsafe { opus_decoder_create(sample_rate as i32, channels as c_int, &mut err) };
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
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        if !self.st.is_null() {
            unsafe { opus_decoder_destroy(self.st) };
            self.st = std::ptr::null_mut();
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
}
