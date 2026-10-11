//! Wake-on-LAN: build and send the standard "magic packet" a sleeping
//! machine's network card recognizes and wakes for, plus enumerate this
//! machine's own network interfaces (name, MAC, LAN IPv4 addresses) so a
//! pairing payload can hand a future client everything it needs without the
//! user typing a MAC address in by hand.
//!
//! Sending is plain cross-platform code — `std::net::UdpSocket` broadcast —
//! no OS-specific branching needed here. See `wol_status.rs` for the
//! OS-specific "is this even enabled on the host" best-effort check;
//! sending a magic packet to a machine that never asked its NIC to listen
//! for one just does nothing.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// A 6-byte Ethernet MAC address. Serializes as the familiar
/// `"aa:bb:cc:dd:ee:ff"` string (via `#[serde(into/try_from)]`), not a raw
/// byte array — this rides in JSON sent to the frontend
/// (`NetworkInterfaceInfo`) and a byte array would be far less useful there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct MacAddress(pub [u8; 6]);

impl MacAddress {
    /// Parses `aa:bb:cc:dd:ee:ff` or `aa-bb-cc-dd-ee-ff`.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split([':', '-']).collect();
        if parts.len() != 6 {
            return None;
        }
        let mut out = [0u8; 6];
        for (i, p) in parts.iter().enumerate() {
            out[i] = u8::from_str_radix(p, 16).ok()?;
        }
        Some(Self(out))
    }
}

impl From<MacAddress> for String {
    fn from(mac: MacAddress) -> String {
        mac.to_string()
    }
}

impl TryFrom<String> for MacAddress {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        MacAddress::parse(&s).ok_or_else(|| format!("invalid MAC address: {s}"))
    }
}

impl std::fmt::Display for MacAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

/// Build the standard wake-on-LAN "magic packet": 6 bytes of `0xFF`
/// followed by the target MAC address repeated 16 times (102 bytes total).
pub fn build_magic_packet(mac: MacAddress) -> [u8; 102] {
    let mut packet = [0xFFu8; 102];
    for i in 0..16 {
        packet[6 + i * 6..6 + i * 6 + 6].copy_from_slice(&mac.0);
    }
    packet
}

/// Send a magic packet as a UDP broadcast to port 9 (the conventional
/// "discard" port wake-on-LAN traditionally targets — some tools use 7,
/// most listening NICs answer either). `broadcast_addr` is normally the
/// LAN's own broadcast address (e.g. `192.168.1.255`); a client derives
/// this from whatever subnet it's currently on, since it only makes sense
/// to broadcast within the same L2 segment the target's NIC actually
/// listens on — a magic packet does not cross a router on its own.
pub fn send_magic_packet(mac: MacAddress, broadcast_addr: IpAddr) -> std::io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    let packet = build_magic_packet(mac);
    socket.send_to(&packet, SocketAddr::new(broadcast_addr, 9))?;
    Ok(())
}

/// This machine's own network interface, as far as a pairing payload cares:
/// name, MAC, and non-loopback LAN IPv4 addresses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInterfaceInfo {
    pub name: String,
    pub mac: MacAddress,
    pub ipv4_addrs: Vec<Ipv4Addr>,
}

/// Every active interface with a real (non-zero) MAC address. Loopback and
/// virtual interfaces with an all-zero MAC are skipped — nothing a remote
/// client could ever usefully target.
pub fn list_interfaces() -> Vec<NetworkInterfaceInfo> {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    networks
        .list()
        .iter()
        .filter_map(|(name, data)| {
            let mac = data.mac_address();
            if mac.is_unspecified() {
                return None;
            }
            let ipv4_addrs: Vec<Ipv4Addr> = data
                .ip_networks()
                .iter()
                .filter_map(|n| match n.addr {
                    IpAddr::V4(v4) if !v4.is_loopback() => Some(v4),
                    _ => None,
                })
                .collect();
            Some(NetworkInterfaceInfo {
                name: name.clone(),
                mac: MacAddress(mac.0),
                ipv4_addrs,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_address_serializes_as_a_plain_string() {
        let mac = MacAddress([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        let json = serde_json::to_string(&mac).unwrap();
        assert_eq!(json, "\"aa:bb:cc:dd:ee:ff\"");
        let back: MacAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(back, mac);
    }

    #[test]
    fn network_interface_info_uses_camel_case_field_names() {
        let info = NetworkInterfaceInfo {
            name: "en0".to_string(),
            mac: MacAddress([0, 1, 2, 3, 4, 5]),
            ipv4_addrs: vec![Ipv4Addr::new(192, 168, 1, 5)],
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(
            json.get("ipv4Addrs").is_some(),
            "expected camelCase key, got {json}"
        );
    }

    #[test]
    fn magic_packet_is_six_ff_bytes_then_mac_times_sixteen() {
        let mac = MacAddress([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        let packet = build_magic_packet(mac);
        assert_eq!(packet.len(), 102);
        assert_eq!(&packet[0..6], &[0xFF; 6]);
        for i in 0..16 {
            assert_eq!(&packet[6 + i * 6..6 + i * 6 + 6], &mac.0);
        }
    }

    #[test]
    fn mac_address_parses_colon_and_hyphen_forms() {
        let expected = MacAddress([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert_eq!(MacAddress::parse("aa:bb:cc:dd:ee:ff"), Some(expected));
        assert_eq!(MacAddress::parse("AA-BB-CC-DD-EE-FF"), Some(expected));
        assert_eq!(MacAddress::parse("not-a-mac"), None);
    }

    #[test]
    fn mac_address_display_is_lowercase_colon_separated() {
        let mac = MacAddress([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert_eq!(mac.to_string(), "aa:bb:cc:dd:ee:ff");
    }

    #[test]
    fn send_magic_packet_does_not_error_on_localhost_broadcast() {
        // Doesn't verify a real wake (impossible in a test environment) —
        // just that building the socket and sending doesn't itself fail.
        let mac = MacAddress([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        send_magic_packet(mac, IpAddr::V4(Ipv4Addr::new(127, 255, 255, 255))).unwrap();
    }
}
