use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] io::Error),

    #[error("bad bluetooth address: {0}")]
    BadAddress(String),

    /// An ATT Error Response from the remote.
    #[error("ATT error on request 0x{request:02x}, handle 0x{handle:04x}: {message}")]
    Att {
        request: u8,
        handle: u16,
        code: u8,
        message: &'static str,
    },

    #[error("unexpected ATT opcode 0x{0:02x}")]
    UnexpectedOpcode(u8),

    #[error("link closed")]
    LinkClosed,

    #[error("timed out waiting for the remote to advertise")]
    ConnectTimeout,

    #[error("opus: {0}")]
    Opus(String),

    #[error("bluetooth: {0}")]
    Bluez(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn att_error_message(code: u8) -> &'static str {
    match code {
        0x01 => "Invalid Handle",
        0x02 => "Read Not Permitted",
        0x03 => "Write Not Permitted",
        0x05 => "Insufficient Authentication",
        0x06 => "Request Not Supported",
        0x08 => "Insufficient Authorization",
        0x0D => "Invalid Attribute Value Length",
        0x0E => "Unlikely Error",
        0x0F => "Insufficient Encryption",
        _ => "unknown ATT error",
    }
}
