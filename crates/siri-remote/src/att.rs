//! A minimal ATT client over a raw L2CAP socket.
//!
//! BlueZ refuses writes to HID Report characteristics as an anti-keylogging
//! measure — verified with `btmon`, the packet never reaches the air. The
//! remote needs exactly such a write ([`protocol::ENABLE_BYTE`] to
//! [`protocol::H_ENABLE`]) before it emits anything but zeros, so we open our
//! own ATT channel and talk to the device directly.
//!
//! Bonding still comes from BlueZ; the kernel supplies the long-term key when
//! we ask for an encrypted link.

use std::collections::VecDeque;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use crate::error::{att_error_message, Error, Result};

/// Explicitly 16-bit: this describes Linux's `struct sockaddr_l2`, whose
/// family field is always `__u16`. Using the host's `sa_family_t` would be
/// correct on Linux only by coincidence — it is 8-bit on some platforms, which
/// would silently shift every field that follows.
const AF_BLUETOOTH: u16 = 31;
const BTPROTO_L2CAP: libc::c_int = 0;
const SOL_BLUETOOTH: libc::c_int = 274;
const BT_SECURITY: libc::c_int = 4;
const BT_SNDMTU: libc::c_int = 12;
const BT_RCVMTU: libc::c_int = 13;

/// ATT lives on a fixed L2CAP channel.
const ATT_CID: u16 = 4;

pub const BDADDR_LE_PUBLIC: u8 = 1;
pub const BDADDR_LE_RANDOM: u8 = 2;

/// Encrypted link using the bond's long-term key.
pub const BT_SECURITY_MEDIUM: u8 = 2;

const OP_ERROR_RSP: u8 = 0x01;
const OP_MTU_REQ: u8 = 0x02;
const OP_MTU_RSP: u8 = 0x03;
const OP_READ_REQ: u8 = 0x0A;
const OP_READ_RSP: u8 = 0x0B;
const OP_WRITE_REQ: u8 = 0x12;
const OP_WRITE_RSP: u8 = 0x13;
const OP_HANDLE_NOTIFY: u8 = 0x1B;

const DEFAULT_ATT_MTU: u16 = 23;

/// Parse `"DC:2B:2A:E0:18:71"` into the byte-reversed form the kernel wants.
fn parse_addr(addr: &str) -> Result<[u8; 6]> {
    let mut out = [0u8; 6];
    let parts: Vec<&str> = addr.split(':').collect();
    if parts.len() != 6 {
        return Err(Error::BadAddress(addr.to_string()));
    }
    for (i, p) in parts.iter().rev().enumerate() {
        out[i] = u8::from_str_radix(p, 16).map_err(|_| Error::BadAddress(addr.to_string()))?;
    }
    Ok(out)
}

/// `struct sockaddr_l2`, built by hand: Rust's socket types cannot express
/// `l2_cid` or `l2_bdaddr_type`.
fn sockaddr_l2(addr: [u8; 6], cid: u16, addr_type: u8) -> [u8; 14] {
    let mut sa = [0u8; 14];
    sa[0..2].copy_from_slice(&AF_BLUETOOTH.to_le_bytes());
    sa[2..4].copy_from_slice(&0u16.to_le_bytes()); // psm, unused for fixed CID
    sa[4..10].copy_from_slice(&addr);
    sa[10..12].copy_from_slice(&cid.to_le_bytes());
    sa[12] = addr_type;
    sa
}

fn last_os_error() -> io::Error {
    io::Error::last_os_error()
}

pub struct AttSocket {
    fd: OwnedFd,
    /// Negotiated ATT MTU. The remote ignores it for notifications — audio
    /// frames arrive larger than this — so it is informational only.
    pub mtu: u16,
    /// What the kernel will accept per PDU. Audio needs at least
    /// [`protocol::MIN_MTU`](crate::protocol::MIN_MTU).
    pub kernel_rcv_mtu: u16,
    /// Notifications that arrived while a request was waiting for its
    /// response, delivered by [`AttSocket::next_notification`] in order.
    pending: VecDeque<(u16, Vec<u8>)>,
}

impl AttSocket {
    /// Open an ATT channel, waiting until the remote advertises.
    ///
    /// A BLE peripheral is only connectable while advertising, and the Siri
    /// Remote advertises for a second or two after a button press. The kernel
    /// abandons a pending LE connect after roughly 40s with `ECONNREFUSED`, so
    /// re-arm and stay pending for nearly the whole budget: the press can then
    /// happen at any time.
    pub fn connect(
        addr: &str,
        addr_type: u8,
        sec_level: u8,
        want_mtu: u16,
        adapter: Option<&str>,
        timeout: Duration,
        mut on_retry: impl FnMut(u32, &io::Error, Duration),
    ) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        let mut attempt = 0u32;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::ConnectTimeout);
            }
            attempt += 1;
            let remaining = deadline.saturating_duration_since(Instant::now());
            let started = Instant::now();
            match Self::connect_once(
                addr,
                addr_type,
                sec_level,
                want_mtu,
                adapter,
                remaining.min(Duration::from_secs(60)),
            ) {
                Ok(s) => return Ok(s),
                Err(Error::Io(e)) => {
                    // An immediate failure means the kernel rejected the
                    // attempt outright (EBUSY from a stale link, EHOSTUNREACH
                    // with no keys loaded) rather than waiting for an
                    // advertisement. Back off instead of spinning.
                    if started.elapsed() < Duration::from_secs(1) {
                        std::thread::sleep(Duration::from_secs(3));
                    }
                    on_retry(
                        attempt,
                        &e,
                        deadline.saturating_duration_since(Instant::now()),
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn connect_once(
        addr: &str,
        addr_type: u8,
        sec_level: u8,
        want_mtu: u16,
        adapter: Option<&str>,
        wait: Duration,
    ) -> Result<Self> {
        let raw = parse_addr(addr)?;
        // Binding to all-zeros lets the kernel pick any adapter, which is
        // ambiguous once a USB dongle sits alongside the built-in radio — and
        // they differ in what matters (microphone audio needs the Bluetooth
        // 4.2+ one). Bind explicitly when an adapter is named.
        let local = match adapter {
            Some(a) => parse_addr(a)?,
            None => [0u8; 6],
        };
        unsafe {
            let fd: RawFd = libc::socket(
                AF_BLUETOOTH as libc::c_int,
                libc::SOCK_SEQPACKET,
                BTPROTO_L2CAP,
            );
            if fd < 0 {
                return Err(Error::Io(last_os_error()));
            }
            let fd = OwnedFd::from_raw_fd(fd);

            let local = sockaddr_l2(local, ATT_CID, addr_type);
            if libc::bind(
                fd.as_raw_fd(),
                local.as_ptr() as *const libc::sockaddr,
                local.len() as u32,
            ) < 0
            {
                return Err(Error::Io(last_os_error()));
            }

            // Ask the kernel for an encrypted link; it uses the bond's LTK.
            let sec = [sec_level, 0u8];
            libc::setsockopt(
                fd.as_raw_fd(),
                SOL_BLUETOOTH,
                BT_SECURITY,
                sec.as_ptr() as *const libc::c_void,
                sec.len() as u32,
            );

            // An L2CAP socket defaults to a 23-byte MTU, so the kernel would
            // truncate the remote's 101-byte audio notifications regardless of
            // what ATT negotiates. These must be set BEFORE connect().
            for opt in [BT_RCVMTU, BT_SNDMTU] {
                let v = want_mtu;
                libc::setsockopt(
                    fd.as_raw_fd(),
                    SOL_BLUETOOTH,
                    opt,
                    &v as *const u16 as *const libc::c_void,
                    mem::size_of::<u16>() as u32,
                );
            }

            // A BLE link cannot complete synchronously: connect() returns
            // EINPROGRESS and we wait for writability.
            let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);

            let peer = sockaddr_l2(raw, ATT_CID, addr_type);
            if libc::connect(
                fd.as_raw_fd(),
                peer.as_ptr() as *const libc::sockaddr,
                peer.len() as u32,
            ) < 0
            {
                let e = last_os_error();
                let code = e.raw_os_error().unwrap_or(0);
                if code != libc::EINPROGRESS && code != libc::EALREADY {
                    return Err(Error::Io(e));
                }
                let mut pfd = libc::pollfd {
                    fd: fd.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let ms = wait.as_millis().min(i32::MAX as u128) as libc::c_int;
                let n = libc::poll(&mut pfd, 1, ms);
                if n <= 0 {
                    return Err(Error::Io(io::Error::from_raw_os_error(libc::ETIMEDOUT)));
                }
                let mut err: libc::c_int = 0;
                let mut len = mem::size_of::<libc::c_int>() as u32;
                libc::getsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut err as *mut _ as *mut libc::c_void,
                    &mut len,
                );
                if err != 0 {
                    return Err(Error::Io(io::Error::from_raw_os_error(err)));
                }
            }

            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags);

            let mut granted: u16 = 0;
            let mut glen = mem::size_of::<u16>() as u32;
            libc::getsockopt(
                fd.as_raw_fd(),
                SOL_BLUETOOTH,
                BT_RCVMTU,
                &mut granted as *mut _ as *mut libc::c_void,
                &mut glen,
            );

            Ok(AttSocket {
                fd,
                mtu: DEFAULT_ATT_MTU,
                kernel_rcv_mtu: granted,
                pending: VecDeque::new(),
            })
        }
    }

    fn send_pdu(&self, pdu: &[u8]) -> Result<()> {
        let n = unsafe {
            libc::send(
                self.fd.as_raw_fd(),
                pdu.as_ptr() as *const libc::c_void,
                pdu.len(),
                0,
            )
        };
        if n < 0 {
            return Err(Error::Io(last_os_error()));
        }
        Ok(())
    }

    /// Read one PDU. `timeout` of `None` blocks indefinitely.
    pub fn recv_pdu(&self, timeout: Option<Duration>) -> Result<Vec<u8>> {
        if let Some(t) = timeout {
            let mut pfd = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = t.as_millis().min(i32::MAX as u128) as libc::c_int;
            let n = unsafe { libc::poll(&mut pfd, 1, ms) };
            if n == 0 {
                return Err(Error::Io(io::Error::from_raw_os_error(libc::ETIMEDOUT)));
            }
            if n < 0 {
                return Err(Error::Io(last_os_error()));
            }
        }
        let cap = (self.mtu as usize).max(1024);
        let mut buf = vec![0u8; cap];
        let n = unsafe {
            libc::recv(
                self.fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                cap,
                0,
            )
        };
        if n < 0 {
            return Err(Error::Io(last_os_error()));
        }
        if n == 0 {
            return Err(Error::LinkClosed);
        }
        buf.truncate(n as usize);
        Ok(buf)
    }

    /// Send a request and wait for its matching response. Notifications that
    /// race it are queued, not dropped: a request sent mid-stream (starting
    /// the microphone, say) must not cost an audio frame or a button edge.
    fn transact(&mut self, pdu: &[u8], expect: u8) -> Result<Vec<u8>> {
        self.send_pdu(pdu)?;
        loop {
            let rsp = self.recv_pdu(Some(Duration::from_secs(10)))?;
            match rsp[0] {
                OP_HANDLE_NOTIFY => {
                    if rsp.len() >= 3 {
                        let handle = u16::from_le_bytes([rsp[1], rsp[2]]);
                        self.pending.push_back((handle, rsp[3..].to_vec()));
                    }
                }
                OP_ERROR_RSP if rsp.len() >= 5 => {
                    let handle = u16::from_le_bytes([rsp[2], rsp[3]]);
                    return Err(Error::Att {
                        request: rsp[1],
                        handle,
                        code: rsp[4],
                        message: att_error_message(rsp[4]),
                    });
                }
                op if op == expect => return Ok(rsp),
                op => return Err(Error::UnexpectedOpcode(op)),
            }
        }
    }

    /// Negotiate the ATT MTU.
    ///
    /// The remote answers 23 to every host — macOS included — and then sends
    /// larger audio notifications anyway, so the result does not gate audio.
    /// It is still worth asking: it is what a well-behaved client does.
    pub fn exchange_mtu(&mut self, want: u16) -> Result<u16> {
        let mut pdu = vec![OP_MTU_REQ];
        pdu.extend_from_slice(&want.to_le_bytes());
        let rsp = self.transact(&pdu, OP_MTU_RSP)?;
        let theirs = u16::from_le_bytes([rsp[1], rsp[2]]);
        self.mtu = want.min(theirs).max(DEFAULT_ATT_MTU);
        Ok(self.mtu)
    }

    pub fn write_req(&mut self, handle: u16, value: &[u8]) -> Result<()> {
        let mut pdu = vec![OP_WRITE_REQ];
        pdu.extend_from_slice(&handle.to_le_bytes());
        pdu.extend_from_slice(value);
        self.transact(&pdu, OP_WRITE_RSP)?;
        Ok(())
    }

    pub fn read_req(&mut self, handle: u16) -> Result<Vec<u8>> {
        let mut pdu = vec![OP_READ_REQ];
        pdu.extend_from_slice(&handle.to_le_bytes());
        let rsp = self.transact(&pdu, OP_READ_RSP)?;
        Ok(rsp[1..].to_vec())
    }

    /// Ask for a different connection interval; see [`crate::hci`]. Returns
    /// the HCI connection handle the request was sent for.
    pub fn request_conn_params(&self, params: &crate::hci::ConnParams) -> io::Result<u16> {
        crate::hci::request_conn_params(self.fd.as_raw_fd(), params)
    }

    /// Wait for the next notification, returning `(handle, payload)`.
    /// Non-notification PDUs are skipped.
    pub fn next_notification(&mut self, timeout: Option<Duration>) -> Result<(u16, Vec<u8>)> {
        if let Some(n) = self.pending.pop_front() {
            return Ok(n);
        }
        loop {
            let pkt = self.recv_pdu(timeout)?;
            if pkt.len() < 3 || pkt[0] != OP_HANDLE_NOTIFY {
                continue;
            }
            let handle = u16::from_le_bytes([pkt[1], pkt[2]]);
            return Ok((handle, pkt[3..].to_vec()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_is_byte_reversed() {
        assert_eq!(
            parse_addr("DC:2B:2A:E0:18:71").unwrap(),
            [0x71, 0x18, 0xE0, 0x2A, 0x2B, 0xDC]
        );
    }

    #[test]
    fn rejects_malformed_addresses() {
        assert!(parse_addr("nope").is_err());
        assert!(parse_addr("DC:2B:2A:E0:18").is_err());
        assert!(parse_addr("ZZ:2B:2A:E0:18:71").is_err());
    }

    #[test]
    fn sockaddr_layout_matches_struct_sockaddr_l2() {
        let sa = sockaddr_l2([1, 2, 3, 4, 5, 6], 4, 1);
        assert_eq!(sa.len(), 14);
        assert_eq!(u16::from_le_bytes([sa[0], sa[1]]), 31); // AF_BLUETOOTH
        assert_eq!(&sa[4..10], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(u16::from_le_bytes([sa[10], sa[11]]), 4); // ATT CID
        assert_eq!(sa[12], 1); // public
    }
}
