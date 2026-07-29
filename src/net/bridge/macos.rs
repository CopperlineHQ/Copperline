// SPDX-License-Identifier: GPL-3.0-or-later

use super::{mac_filter, AdapterIo, HostInterface};
use anyhow::{bail, Context, Result};
use pcap::{Active, Capture, Device, Error as PcapError, Linktype};

pub(super) fn list_interfaces() -> Result<Vec<HostInterface>> {
    Ok(Device::list()?
        .into_iter()
        .map(|device| HostInterface {
            name: device.name,
            description: device.desc,
            up: device.flags.is_up(),
            running: device.flags.is_running(),
            loopback: device.flags.is_loopback(),
            wireless: device.flags.is_wireless(),
        })
        .collect())
}

pub(super) fn open(interface: &str, _guest_mac: Option<[u8; 6]>) -> Result<MacAdapter> {
    let capture = Capture::from_device(interface)
        .with_context(|| format!("libpcap did not find adapter {interface:?}"))?
        .promisc(true)
        .immediate_mode(true)
        .snaplen(65_535)
        .timeout(1)
        .open()
        .with_context(|| {
            format!(
                "libpcap could not open {interface:?}; on macOS the user must \
                 have access to /dev/bpf (normally through the access_bpf group)"
            )
        })?;
    if capture.get_datalink() != Linktype::ETHERNET {
        bail!(
            "adapter {interface:?} does not expose Ethernet frames through libpcap \
             (link type {:?})",
            capture.get_datalink()
        );
    }
    Ok(MacAdapter {
        capture: capture.setnonblock()?,
    })
}

pub(super) struct MacAdapter {
    capture: Capture<Active>,
}

impl AdapterIo for MacAdapter {
    fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.capture.sendpacket(frame).map_err(Into::into)
    }

    fn receive(&mut self) -> Result<Option<Vec<u8>>> {
        match self.capture.next_packet() {
            Ok(packet) => Ok(Some(packet.data.to_vec())),
            Err(PcapError::NoMorePackets | PcapError::TimeoutExpired) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set_guest_mac(&mut self, mac: [u8; 6]) -> Result<()> {
        self.capture.filter(&mac_filter(mac), true)?;
        Ok(())
    }
}
