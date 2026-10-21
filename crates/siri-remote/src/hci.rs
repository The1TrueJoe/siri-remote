//! Just enough raw HCI to ask for a faster connection interval.
//!
//! Linux connects as central with a 30–50 ms interval, and the remote sends
//! roughly one notification per connection event. Microphone frames are
//! produced every 20 ms, so at the default interval they queue on the remote:
//! 7 s of speech took 17 s to arrive. Asking for 7.5–15 ms lets the link keep
//! up. There is no socket option for this, so it goes to the controller as an
//! HCI LE Connection Update — the same command `hcitool lecup` sends — which
//! needs `CAP_NET_RAW`.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

const AF_BLUETOOTH: libc::c_int = 31;
const BTPROTO_HCI: libc::c_int = 1;
const HCI_CHANNEL_RAW: u16 = 0;
const SOL_L2CAP: libc::c_int = 6;
const L2CAP_CONNINFO: libc::c_int = 2;
/// `_IOR('H', 211, int)`.
const HCIGETDEVINFO: u64 = 0x8004_48d3;
const HCI_MAX_DEV: u16 = 16;
const HCI_COMMAND_PKT: u8 = 0x01;
/// OGF 0x08 (LE), OCF 0x0013 (LE Connection Update).
const OP_LE_CONN_UPDATE: u16 = 0x2013;

/// LE connection parameters, in the controller's units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnParams {
    /// Minimum connection interval, in 1.25 ms units (6 = 7.5 ms).
    pub min_interval: u16,
    /// Maximum connection interval, in 1.25 ms units.
    pub max_interval: u16,
    /// Connection events the remote may skip.
    pub latency: u16,
    /// Supervision timeout, in 10 ms units.
    pub supervision_timeout: u16,
}

impl ConnParams {
    /// 7.5–15 ms, no latency, 5 s supervision timeout: fast enough for 50
    /// microphone frames a second with headroom, and as forgiving of a weak
    /// signal as the kernel default.
    pub const VOICE: ConnParams = ConnParams {
        min_interval: 6,
        max_interval: 12,
        latency: 0,
        supervision_timeout: 500,
    };

    /// The HCI command parameters, little-endian.
    fn command(&self, handle: u16) -> [u8; 14] {
        let mut p = [0u8; 14];
        let fields = [
            handle,
            self.min_interval,
            self.max_interval,
            self.latency,
            self.supervision_timeout,
            0, // minimum connection event length
            0, // maximum connection event length
        ];
        for (i, f) in fields.iter().enumerate() {
            p[i * 2..i * 2 + 2].copy_from_slice(&f.to_le_bytes());
        }
        p
    }
}

/// The HCI connection handle behind a connected L2CAP socket.
pub fn conn_handle(l2cap: RawFd) -> io::Result<u16> {
    // struct l2cap_conninfo { __u16 hci_handle; __u8 dev_class[3]; }
    let mut info = [0u8; 6];
    let mut len = info.len() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            l2cap,
            SOL_L2CAP,
            L2CAP_CONNINFO,
            info.as_mut_ptr() as *mut libc::c_void,
            &mut len,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u16::from_le_bytes([info[0], info[1]]))
}

/// The local adapter address a connected L2CAP socket is bound to.
fn local_bdaddr(l2cap: RawFd) -> io::Result<[u8; 6]> {
    // struct sockaddr_l2: family u16, psm u16, bdaddr[6], cid u16, type u8
    let mut sa = [0u8; 14];
    let mut len = sa.len() as libc::socklen_t;
    let rc = unsafe { libc::getsockname(l2cap, sa.as_mut_ptr() as *mut libc::sockaddr, &mut len) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut a = [0u8; 6];
    a.copy_from_slice(&sa[4..10]);
    Ok(a)
}

fn hci_socket() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_RAW, BTPROTO_HCI) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Find the `hciN` index of the adapter with this address.
fn dev_id_for(bdaddr: [u8; 6]) -> io::Result<u16> {
    let sock = hci_socket()?;
    for id in 0..HCI_MAX_DEV {
        // struct hci_dev_info: dev_id u16, name[8], bdaddr[6], ... (92 bytes)
        let mut di = [0u8; 128];
        di[0..2].copy_from_slice(&id.to_le_bytes());
        let rc = unsafe { libc::ioctl(sock.as_raw_fd(), HCIGETDEVINFO as _, di.as_mut_ptr()) };
        if rc < 0 {
            continue;
        }
        if di[10..16] == bdaddr {
            return Ok(id);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no HCI device owns this link",
    ))
}

/// Ask the controller to move the link behind `l2cap` to `params`.
///
/// Fire and forget: the controller answers asynchronously, the remote may
/// counter-propose, and the kernel tracks the outcome. Failure leaves the
/// link exactly as it was.
pub fn request_conn_params(l2cap: RawFd, params: &ConnParams) -> io::Result<u16> {
    let handle = conn_handle(l2cap)?;
    let dev = dev_id_for(local_bdaddr(l2cap)?)?;

    let sock = hci_socket()?;
    // struct sockaddr_hci { sa_family_t (u16), dev u16, channel u16 }
    let mut sa = [0u8; 6];
    sa[0..2].copy_from_slice(&(AF_BLUETOOTH as u16).to_le_bytes());
    sa[2..4].copy_from_slice(&dev.to_le_bytes());
    sa[4..6].copy_from_slice(&HCI_CHANNEL_RAW.to_le_bytes());
    let rc = unsafe {
        libc::bind(
            sock.as_raw_fd(),
            sa.as_ptr() as *const libc::sockaddr,
            mem::size_of_val(&sa) as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }

    let body = params.command(handle);
    let mut pkt = Vec::with_capacity(4 + body.len());
    pkt.push(HCI_COMMAND_PKT);
    pkt.extend_from_slice(&OP_LE_CONN_UPDATE.to_le_bytes());
    pkt.push(body.len() as u8);
    pkt.extend_from_slice(&body);
    let n = unsafe {
        libc::write(
            sock.as_raw_fd(),
            pkt.as_ptr() as *const libc::c_void,
            pkt.len(),
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_conn_update_command_layout() {
        let p = ConnParams::VOICE.command(0x0010);
        assert_eq!(p, [0x10, 0x00, 6, 0, 12, 0, 0, 0, 0xF4, 0x01, 0, 0, 0, 0]);
    }

    #[test]
    fn voice_params_fit_fifty_frames_a_second() {
        let max_ms = ConnParams::VOICE.max_interval as f32 * 1.25;
        assert!(1000.0 / max_ms > 50.0);
        // Supervision timeout must exceed (1 + latency) * interval * 2.
        let sup_ms = ConnParams::VOICE.supervision_timeout as f32 * 10.0;
        assert!(sup_ms > (1.0 + ConnParams::VOICE.latency as f32) * max_ms * 2.0);
    }
}
