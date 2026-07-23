//! Art-Net codec: `ArtDmx` (data) and `ArtSync` (synchronization).
//!
//! Reference: Art-Net 4 protocol specification (Artistic Licence). Only the
//! two packet types M1 needs are implemented here; `ArtPoll`/`ArtPollReply`
//! node discovery is deferred with the node-management UI (CLAUDE.md §5.1).
//!
//! Wire-format notes that are easy to get wrong:
//!   - The `OpCode` field is transmitted **low byte first** (little-endian).
//!   - The `ProtVer` field is transmitted **high byte first** (big-endian).
//!   - The `Length` field (channel count) is transmitted **high byte first**,
//!     must be even, and is clamped to the range 2..=512.

/// Art-Net packet identifier: the ASCII string `"Art-Net"` followed by a null.
pub const ID: [u8; 8] = *b"Art-Net\0";

/// Protocol revision reported in every packet (`ProtVerHi`/`ProtVerLo` = 0, 14).
pub const PROTOCOL_VERSION: u16 = 14;

/// `OpOutput` / `OpDmx` — a DMX data packet.
pub const OP_DMX: u16 = 0x5000;
/// `OpSync` — a synchronization packet (flush buffered universes together).
pub const OP_SYNC: u16 = 0x5200;

/// Fixed header length of an `ArtDmx` packet, before the channel data.
pub const ARTDMX_HEADER_LEN: usize = 18;
/// Total length of an `ArtSync` packet.
pub const ARTSYNC_LEN: usize = 14;

/// Maximum DMX channels carried by one `ArtDmx` packet (one universe).
pub const MAX_CHANNELS: usize = 512;

/// A DMX data packet targeting a single 15-bit port address (universe).
///
/// The 15-bit `port_address` packs `Net` (high 7 bits) and `Sub-Net:Universe`
/// (low 8 bits) exactly as they sit on the wire; callers usually build it from
/// a plain universe number, but the full field is exposed for multi-net rigs.
pub struct ArtDmx<'a> {
    /// Rolling sequence counter (1..=255; 0 disables sequencing). Wraps 255→1.
    pub sequence: u8,
    /// Physical input port of origin — informational only, usually 0.
    pub physical: u8,
    /// 15-bit port address: `Net(7) : Sub-Net(4) : Universe(4)`.
    pub port_address: u16,
    /// Channel data, 1..=512 bytes. An odd count is padded to the next even
    /// length with a trailing zero (the spec requires an even `Length`).
    pub data: &'a [u8],
}

impl ArtDmx<'_> {
    /// Even, clamped channel count that will be written in the `Length` field
    /// and emitted as data bytes.
    #[inline]
    pub fn payload_len(&self) -> usize {
        let n = self.data.len().min(MAX_CHANNELS);
        let n = n.max(2); // spec minimum
        n + (n & 1) // round up to even
    }

    /// Total encoded length (header + padded payload).
    #[inline]
    pub fn encoded_len(&self) -> usize {
        ARTDMX_HEADER_LEN + self.payload_len()
    }

    /// Encode into `buf`. The buffer is cleared first, so a caller can reuse a
    /// single `Vec` across frames with no per-frame allocation once it has
    /// grown to size (hot-path rule, CLAUDE.md §3.3).
    pub fn write_to(&self, buf: &mut Vec<u8>) {
        debug_assert!(
            self.data.len() <= MAX_CHANNELS,
            "ArtDmx data exceeds one universe ({} > {MAX_CHANNELS})",
            self.data.len()
        );
        let payload_len = self.payload_len();

        buf.clear();
        buf.reserve(ARTDMX_HEADER_LEN + payload_len);
        buf.extend_from_slice(&ID);
        buf.extend_from_slice(&OP_DMX.to_le_bytes()); // OpCode: low byte first
        buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes()); // ProtVer: high byte first
        buf.push(self.sequence);
        buf.push(self.physical);
        buf.push((self.port_address & 0x00FF) as u8); // SubUni (low byte)
        buf.push(((self.port_address >> 8) & 0x7F) as u8); // Net (high 7 bits)
        buf.extend_from_slice(&(payload_len as u16).to_be_bytes()); // Length: high byte first

        let copied = self.data.len().min(MAX_CHANNELS);
        buf.extend_from_slice(&self.data[..copied]);
        for _ in copied..payload_len {
            buf.push(0); // pad to the even Length
        }
    }
}

/// Encode an `ArtSync` packet into `buf` (cleared first).
///
/// Sent after a frame's `ArtDmx` packets so nodes present all universes on the
/// same output frame — tear-free multi-node output (CLAUDE.md §5.1).
pub fn write_artsync(buf: &mut Vec<u8>) {
    buf.clear();
    buf.reserve(ARTSYNC_LEN);
    buf.extend_from_slice(&ID);
    buf.extend_from_slice(&OP_SYNC.to_le_bytes()); // OpCode: low byte first
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes()); // ProtVer: high byte first
    buf.push(0); // Aux1 (transmit as 0)
    buf.push(0); // Aux2 (transmit as 0)
}

/// A decoded `ArtDmx` packet borrowed from a receive buffer. Used by the
/// virtual sink (step 7) and by round-trip tests.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedArtDmx<'a> {
    pub sequence: u8,
    pub physical: u8,
    pub port_address: u16,
    pub data: &'a [u8],
}

/// The Art-Net opcode of a received packet, if it is a recognized Art-Net frame.
#[derive(Debug, PartialEq, Eq)]
pub enum ArtNetOpcode {
    Dmx,
    Sync,
    /// A valid Art-Net packet whose opcode we do not handle in M1.
    Other(u16),
}

/// Identify a received datagram as Art-Net and return its opcode, or `None` if
/// the `"Art-Net\0"` identifier / minimum length is absent.
pub fn opcode_of(buf: &[u8]) -> Option<ArtNetOpcode> {
    if buf.len() < 12 || buf[..8] != ID {
        return None;
    }
    let op = u16::from_le_bytes([buf[8], buf[9]]);
    Some(match op {
        OP_DMX => ArtNetOpcode::Dmx,
        OP_SYNC => ArtNetOpcode::Sync,
        other => ArtNetOpcode::Other(other),
    })
}

/// Decode an `ArtDmx` packet. Returns `None` if the buffer is not a
/// well-formed `ArtDmx` (bad id/opcode, short header, or `Length` exceeding
/// the available bytes).
pub fn parse_artdmx(buf: &[u8]) -> Option<ParsedArtDmx<'_>> {
    if opcode_of(buf) != Some(ArtNetOpcode::Dmx) || buf.len() < ARTDMX_HEADER_LEN {
        return None;
    }
    let sequence = buf[12];
    let physical = buf[13];
    let sub_uni = buf[14] as u16;
    let net = (buf[15] as u16) & 0x7F;
    let port_address = (net << 8) | sub_uni;
    let length = u16::from_be_bytes([buf[16], buf[17]]) as usize;
    let end = ARTDMX_HEADER_LEN + length;
    if length == 0 || length > MAX_CHANNELS || buf.len() < end {
        return None;
    }
    Some(ParsedArtDmx {
        sequence,
        physical,
        port_address,
        data: &buf[ARTDMX_HEADER_LEN..end],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artdmx_reference_bytes() {
        let data = [10u8, 20, 30, 40];
        let pkt = ArtDmx {
            sequence: 1,
            physical: 0,
            port_address: 0x0001, // Net 0, universe 1
            data: &data,
        };
        let mut buf = Vec::new();
        pkt.write_to(&mut buf);

        let expected: [u8; 22] = [
            b'A', b'r', b't', b'-', b'N', b'e', b't', 0x00, // ID
            0x00, 0x50, // OpCode 0x5000, low byte first
            0x00, 0x0E, // ProtVer 14, high byte first
            0x01, // Sequence
            0x00, // Physical
            0x01, // SubUni (universe 1)
            0x00, // Net
            0x00, 0x04, // Length = 4, high byte first
            10, 20, 30, 40, // data
        ];
        assert_eq!(buf, expected);
        assert_eq!(pkt.encoded_len(), expected.len());
    }

    #[test]
    fn artdmx_pads_odd_length_to_even() {
        let data = [1u8, 2, 3];
        let pkt = ArtDmx {
            sequence: 7,
            physical: 0,
            port_address: 0,
            data: &data,
        };
        let mut buf = Vec::new();
        pkt.write_to(&mut buf);

        // Length field reports 4, and a trailing zero pads the odd payload.
        assert_eq!(u16::from_be_bytes([buf[16], buf[17]]), 4);
        assert_eq!(buf.len(), ARTDMX_HEADER_LEN + 4);
        assert_eq!(&buf[ARTDMX_HEADER_LEN..], &[1, 2, 3, 0]);
    }

    #[test]
    fn artdmx_port_address_splits_net_and_universe() {
        // Net = 0x7F, SubUni = 0xA5  ->  port_address 0x7FA5.
        let pkt = ArtDmx {
            sequence: 1,
            physical: 0,
            port_address: 0x7FA5,
            data: &[0u8; 2],
        };
        let mut buf = Vec::new();
        pkt.write_to(&mut buf);
        assert_eq!(buf[14], 0xA5); // SubUni
        assert_eq!(buf[15], 0x7F); // Net
    }

    #[test]
    fn artsync_reference_bytes() {
        let mut buf = Vec::new();
        write_artsync(&mut buf);
        let expected: [u8; ARTSYNC_LEN] = [
            b'A', b'r', b't', b'-', b'N', b'e', b't', 0x00, // ID
            0x00, 0x52, // OpCode 0x5200, low byte first
            0x00, 0x0E, // ProtVer 14
            0x00, 0x00, // Aux1, Aux2
        ];
        assert_eq!(buf, expected);
    }

    #[test]
    fn write_then_parse_roundtrips() {
        let data: Vec<u8> = (0..48u8).collect();
        let pkt = ArtDmx {
            sequence: 200,
            physical: 3,
            port_address: 0x0102,
            data: &data,
        };
        let mut buf = Vec::new();
        pkt.write_to(&mut buf);

        let parsed = parse_artdmx(&buf).expect("well-formed ArtDmx");
        assert_eq!(
            parsed,
            ParsedArtDmx {
                sequence: 200,
                physical: 3,
                port_address: 0x0102,
                data: &data,
            }
        );
    }

    #[test]
    fn opcode_detection_and_rejection() {
        let mut sync = Vec::new();
        write_artsync(&mut sync);
        assert_eq!(opcode_of(&sync), Some(ArtNetOpcode::Sync));

        // Foreign / short datagrams are not Art-Net.
        assert_eq!(opcode_of(b"not art-net!!"), None);
        assert_eq!(opcode_of(&[]), None);
    }

    #[test]
    fn parse_rejects_length_past_end() {
        let mut buf = Vec::new();
        ArtDmx {
            sequence: 1,
            physical: 0,
            port_address: 0,
            data: &[1, 2, 3, 4],
        }
        .write_to(&mut buf);
        // Corrupt the Length field to claim more data than present.
        buf[16] = 0x02; // 0x0200 = 512 channels claimed, only 4 present
        buf[17] = 0x00;
        assert!(parse_artdmx(&buf).is_none());
    }
}
