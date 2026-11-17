//! Art-Net discovery codec: `ArtPoll` (encode) and `ArtPollReply` (decode).
//!
//! Reference: Art-Net 4 protocol specification (Artistic Licence). Only the fields
//! the Device Manager's Scan needs are decoded (docs/DEVICES-SPEC.md §7): the node's
//! IP, names, MAC, its `BindIndex`, and the port addresses of its **output** ports.
//!
//! An `ArtPollReply` describes at most 4 ports. A node with more (the H802RA answers
//! with 18 of them, 4 universes each) sends one reply per group of 4, each with its
//! own `BindIndex` — merged back into one node by [`crate::discovery`].

use super::artnet::{ID, PROTOCOL_VERSION};

/// `OpPoll`.
pub const OP_POLL: u16 = 0x2000;
/// `OpPollReply`.
pub const OP_POLL_REPLY: u16 = 0x2100;
/// The `ArtPoll` length we send (the Art-Net 4 minimum; the optional targeted-mode
/// fields are not used).
pub const ARTPOLL_LEN: usize = 14;

/// Encode an `ArtPoll` into `buf` (cleared first). Flags 0: replies are sent to the
/// poller (Art-Net 4 nodes unicast them back), no diagnostics requested.
pub fn write_artpoll(buf: &mut Vec<u8>) {
    buf.clear();
    buf.extend_from_slice(&ID);
    buf.extend_from_slice(&OP_POLL.to_le_bytes()); // OpCode: low byte first
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes()); // ProtVer: high byte first
    buf.push(0); // Flags
    buf.push(0x10); // DiagPriority: DpLow (diagnostics are not requested anyway)
}

/// The fields of one `ArtPollReply` the Scan uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollReply {
    pub ip: [u8; 4],
    pub short_name: String,
    pub long_name: String,
    /// `None` when the reply is too short to carry it (pre-Art-Net-3 nodes).
    pub mac: Option<[u8; 6]>,
    /// 1-based group index of a multi-reply node; `1` when absent (single reply).
    pub bind_index: u8,
    /// Port addresses of this reply's OUTPUT ports (DMX from the network to the
    /// node's outputs), in port order.
    pub output_universes: Vec<u16>,
}

// Field offsets (Art-Net 4, ArtPollReply).
const IP: usize = 10;
const NET_SWITCH: usize = 18;
const SUB_SWITCH: usize = 19;
const SHORT_NAME: usize = 26;
const SHORT_NAME_LEN: usize = 18;
const LONG_NAME: usize = 44;
const LONG_NAME_LEN: usize = 64;
const NUM_PORTS_LO: usize = 173;
const PORT_TYPES: usize = 174;
const SW_OUT: usize = 190;
const MAC: usize = 201;
const BIND_INDEX: usize = 211;
/// Shortest reply we accept: through `SwOut` + the fields before `Style`.
const MIN_LEN: usize = 200;

/// `PortTypes` bit 7: the port can output DMX512 from the network.
const PORT_CAN_OUTPUT: u8 = 0x80;

/// Decode an `ArtPollReply`, or `None` if `buf` is not one (wrong ID/opcode, or too
/// short). Tolerant of older/shorter replies: MAC and BindIndex are optional.
pub fn parse_artpollreply(buf: &[u8]) -> Option<PollReply> {
    if buf.len() < MIN_LEN || buf[..8] != ID {
        return None;
    }
    if u16::from_le_bytes([buf[8], buf[9]]) != OP_POLL_REPLY {
        return None;
    }
    let net = (buf[NET_SWITCH] & 0x7F) as u16;
    let sub = (buf[SUB_SWITCH] & 0x0F) as u16;
    let ports = (buf[NUM_PORTS_LO] as usize).min(4);
    let output_universes = (0..ports)
        .filter(|&i| buf[PORT_TYPES + i] & PORT_CAN_OUTPUT != 0)
        .map(|i| (net << 8) | (sub << 4) | (buf[SW_OUT + i] & 0x0F) as u16)
        .collect();
    let mac = (buf.len() >= MAC + 6).then(|| {
        let mut m = [0u8; 6];
        m.copy_from_slice(&buf[MAC..MAC + 6]);
        m
    });
    let bind_index = match buf.get(BIND_INDEX) {
        Some(&b) if b > 0 => b,
        _ => 1,
    };
    Some(PollReply {
        ip: [buf[IP], buf[IP + 1], buf[IP + 2], buf[IP + 3]],
        short_name: c_string(&buf[SHORT_NAME..SHORT_NAME + SHORT_NAME_LEN]),
        long_name: c_string(&buf[LONG_NAME..LONG_NAME + LONG_NAME_LEN]),
        mac,
        bind_index,
        output_universes,
    })
}

/// A NUL-terminated ASCII field, trimmed (non-ASCII bytes replaced).
fn c_string(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).trim().to_string()
}

/// Build an `ArtPollReply` (for tests and the virtual node): `outputs` are the
/// `SwOut` nibbles of up to 4 output ports.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_artpollreply(
    ip: [u8; 4],
    short_name: &str,
    long_name: &str,
    mac: [u8; 6],
    net: u8,
    sub: u8,
    outputs: &[u8],
    bind_index: u8,
) -> Vec<u8> {
    let mut b = vec![0u8; 239];
    b[..8].copy_from_slice(&ID);
    b[8..10].copy_from_slice(&OP_POLL_REPLY.to_le_bytes());
    b[IP..IP + 4].copy_from_slice(&ip);
    b[14..16].copy_from_slice(&6454u16.to_le_bytes());
    b[NET_SWITCH] = net;
    b[SUB_SWITCH] = sub;
    b[SHORT_NAME..SHORT_NAME + short_name.len()].copy_from_slice(short_name.as_bytes());
    b[LONG_NAME..LONG_NAME + long_name.len()].copy_from_slice(long_name.as_bytes());
    b[NUM_PORTS_LO] = outputs.len() as u8;
    for (i, &sw) in outputs.iter().enumerate() {
        b[PORT_TYPES + i] = PORT_CAN_OUTPUT;
        b[SW_OUT + i] = sw;
    }
    b[MAC..MAC + 6].copy_from_slice(&mac);
    b[BIND_INDEX] = bind_index;
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artpoll_is_a_valid_minimal_poll() {
        let mut b = Vec::new();
        write_artpoll(&mut b);
        assert_eq!(b.len(), ARTPOLL_LEN);
        assert_eq!(&b[..8], b"Art-Net\0");
        assert_eq!(u16::from_le_bytes([b[8], b[9]]), OP_POLL);
        assert_eq!(u16::from_be_bytes([b[10], b[11]]), 14);
    }

    #[test]
    fn parses_names_mac_bind_and_output_universes() {
        let raw = build_artpollreply(
            [169, 254, 99, 11],
            "H802RA",
            "Oem ArtNet-Node",
            [0x48, 0xE0, 0xC7, 0x4D, 0x96, 0x64],
            0,
            1,
            &[0, 1, 2, 3],
            2,
        );
        let r = parse_artpollreply(&raw).unwrap();
        assert_eq!(r.ip, [169, 254, 99, 11]);
        assert_eq!(r.short_name, "H802RA");
        assert_eq!(r.long_name, "Oem ArtNet-Node");
        assert_eq!(r.mac, Some([0x48, 0xE0, 0xC7, 0x4D, 0x96, 0x64]));
        assert_eq!(r.bind_index, 2);
        // Sub-Net 1 → port addresses 16..19.
        assert_eq!(r.output_universes, [16, 17, 18, 19]);
    }

    #[test]
    fn input_only_ports_are_not_outputs() {
        let mut raw = build_artpollreply([10, 0, 0, 1], "n", "n", [0; 6], 0, 0, &[0, 1], 1);
        raw[PORT_TYPES + 1] = 0x40; // input only
        assert_eq!(parse_artpollreply(&raw).unwrap().output_universes, [0]);
    }

    #[test]
    fn short_and_foreign_packets_are_rejected_or_tolerated() {
        let raw = build_artpollreply([10, 0, 0, 1], "n", "n", [1; 6], 0, 0, &[5], 0);
        // An Art-Net 2 sized reply (no MAC / BindIndex) still parses, bind defaults to 1.
        let old = parse_artpollreply(&raw[..MIN_LEN]).unwrap();
        assert_eq!(
            (old.mac, old.bind_index, old.output_universes),
            (None, 1, vec![5])
        );
        assert!(parse_artpollreply(&raw[..MIN_LEN - 1]).is_none());
        let mut poll = Vec::new();
        write_artpoll(&mut poll);
        assert!(
            parse_artpollreply(&poll).is_none(),
            "an ArtPoll is not a reply"
        );
    }
}
