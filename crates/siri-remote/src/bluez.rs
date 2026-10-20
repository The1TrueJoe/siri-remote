//! Discovery and pairing via BlueZ.
//!
//! Feature-gated (`bluez`): the rest of this crate is synchronous and
//! dependency-light, while pairing needs an async D-Bus client.
//!
//! Only pairing and bonding go through BlueZ. The data path deliberately does
//! not: BlueZ refuses writes to HID Report characteristics, and the remote
//! needs exactly such a write to unlock. See [`crate::att`].

use std::collections::HashMap;
use std::time::Duration;

use crate::error::{Error, Result};
use bluer::{Adapter, AdapterEvent, Address, Session};
use futures_util::StreamExt;
use serde::Serialize;

/// The Human Interface Device service; the remote advertises it even though
/// its HID descriptor carries no usable data.
const HID_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x00001812_0000_1000_8000_00805f9b34fb);

#[derive(Debug, Clone, Serialize)]
pub struct Discovered {
    pub address: String,
    pub name: String,
    pub rssi: Option<i16>,
    pub paired: bool,
    pub addr_type: String,
    /// True when this looks like a Siri Remote rather than any old HID device.
    pub likely_remote: bool,
}

fn bz(e: impl std::fmt::Display) -> Error {
    Error::Bluez(e.to_string())
}

/// Pick an adapter by BD address (`F4:4E:FC:58:72:95`) or kernel name
/// (`hci1`), falling back to BlueZ's default when nothing is named.
///
/// Naming one matters on a machine with both a built-in radio and a dongle:
/// microphone audio needs a Bluetooth 4.2+ controller, and the built-in radio
/// on a Raspberry Pi 3 is 4.1.
pub async fn adapter_for(want: Option<&str>) -> Result<Adapter> {
    let session = Session::new().await.map_err(bz)?;

    let adapter = match want.map(str::trim).filter(|w| !w.is_empty()) {
        Some(w) => {
            let mut found = None;
            for name in session.adapter_names().await.map_err(bz)? {
                let candidate = session.adapter(&name).map_err(bz)?;
                let address = candidate
                    .address()
                    .await
                    .map(|a| a.to_string())
                    .unwrap_or_default();
                if name.eq_ignore_ascii_case(w) || address.eq_ignore_ascii_case(w) {
                    found = Some(candidate);
                    break;
                }
            }
            found.ok_or_else(|| Error::Bluez(format!("no adapter matching {w:?}")))?
        }
        None => session.default_adapter().await.map_err(bz)?,
    };

    adapter.set_powered(true).await.map_err(bz)?;
    Ok(adapter)
}

/// Every adapter the system has, so a caller can offer a choice.
pub async fn adapters() -> Result<Vec<AdapterInfo>> {
    let session = Session::new().await.map_err(bz)?;
    let mut out = Vec::new();
    for name in session.adapter_names().await.map_err(bz)? {
        let Ok(a) = session.adapter(&name) else { continue };
        out.push(AdapterInfo {
            address: a.address().await.map(|x| x.to_string()).unwrap_or_default(),
            name,
            powered: a.is_powered().await.unwrap_or(false),
            discovering: a.is_discovering().await.unwrap_or(false),
        });
    }
    Ok(out)
}

fn addr_type_str(t: bluer::AddressType) -> String {
    match t {
        bluer::AddressType::LeRandom => "random".into(),
        _ => "public".into(),
    }
}

/// Apple's OUI on these remotes. Used only to rank results, never to exclude:
/// a HID device is still shown, just not marked "likely".
fn is_apple_oui(addr: &Address) -> bool {
    addr.0[0] == 0xDC && addr.0[1] == 0x2B && addr.0[2] == 0x2A
}

/// Scan for HID devices for `secs`.
///
/// The remote only advertises for a second or two after a button press, so the
/// caller should tell the user to press one (or hold Menu+Volume Up to enter
/// pairing mode) while this runs.
pub async fn scan(secs: u64, want: Option<&str>) -> Result<Vec<Discovered>> {
    let adapter = adapter_for(want).await?;
    let mut events = adapter.discover_devices().await.map_err(bz)?;
    let mut found: HashMap<Address, Discovered> = HashMap::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let evt = match tokio::time::timeout(remaining, events.next()).await {
            Ok(Some(e)) => e,
            _ => break,
        };
        let addr = match evt {
            AdapterEvent::DeviceAdded(a) => a,
            AdapterEvent::PropertyChanged(_) => continue,
            AdapterEvent::DeviceRemoved(_) => continue,
        };
        let Ok(dev) = adapter.device(addr) else { continue };

        let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();
        let name = dev.name().await.ok().flatten().unwrap_or_default();
        let is_hid = uuids.contains(&HID_UUID);
        if !is_hid && !is_apple_oui(&addr) {
            continue;
        }

        found.insert(
            addr,
            Discovered {
                address: addr.to_string(),
                name,
                rssi: dev.rssi().await.ok().flatten(),
                paired: dev.is_paired().await.unwrap_or(false),
                addr_type: dev
                    .address_type()
                    .await
                    .map(addr_type_str)
                    .unwrap_or_else(|_| "public".into()),
                likely_remote: is_hid && is_apple_oui(&addr),
            },
        );
    }

    let mut out: Vec<_> = found.into_values().collect();
    // Most likely first, then strongest signal.
    out.sort_by(|a, b| {
        b.likely_remote
            .cmp(&a.likely_remote)
            .then(b.rssi.unwrap_or(-127).cmp(&a.rssi.unwrap_or(-127)))
    });
    Ok(out)
}

/// Bond with a remote so the kernel holds its long-term key.
///
/// The raw ATT data path needs the bond to exist but does not use BlueZ to
/// carry traffic, so we untrust the device afterwards: a trusted device gets
/// auto-connected by bluetoothd, which would steal the ATT channel from us.
pub async fn pair(address: &str, want: Option<&str>) -> Result<String> {
    let addr: Address = address
        .parse()
        .map_err(|_| Error::BadAddress(address.to_string()))?;
    let adapter = adapter_for(want).await?;
    let dev = adapter.device(addr).map_err(bz)?;

    if !dev.is_paired().await.unwrap_or(false) {
        dev.pair()
            .await
            .map_err(|e| Error::Bluez(format!("pairing failed: {e}")))?;
    }
    let addr_type = dev
        .address_type()
        .await
        .map(addr_type_str)
        .unwrap_or_else(|_| "public".into());

    // Hand the link back so our own ATT channel can own it.
    let _ = dev.set_trusted(false).await;
    let _ = dev.disconnect().await;
    Ok(addr_type)
}

pub async fn remove(address: &str, want: Option<&str>) -> Result<()> {
    let addr: Address = address
        .parse()
        .map_err(|_| Error::BadAddress(address.to_string()))?;
    let adapter = adapter_for(want).await?;
    let _ = adapter.remove_device(addr).await;
    Ok(())
}

/// Adapter facts the UI shows, including whether voice is even possible here.
#[derive(Debug, Clone, Serialize)]
pub struct AdapterInfo {
    pub address: String,
    pub name: String,
    pub powered: bool,
    pub discovering: bool,
}

pub async fn info(want: Option<&str>) -> Result<AdapterInfo> {
    let adapter = adapter_for(want).await?;
    Ok(AdapterInfo {
        address: adapter.address().await.map_err(bz)?.to_string(),
        name: adapter.name().to_string(),
        powered: adapter.is_powered().await.unwrap_or(false),
        discovering: adapter.is_discovering().await.unwrap_or(false),
    })
}
