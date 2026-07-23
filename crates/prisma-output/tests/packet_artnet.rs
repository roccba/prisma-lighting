//! Packet-level test of the Art-Net codec against a documented reference frame,
//! exercised through the public API only (CLAUDE.md §7: "every protocol
//! implementation gets a packet-level test against captured reference data").
//!
//! The reference below is a single-universe `ArtDmx` frame carrying a 6-channel
//! payload (two RGB pixels: red, green) on universe 1, sequence 1 — the shape
//! of the very first thing M1 puts on the wire.

use prisma_output::packet::artnet::{self, ArtDmx};

/// Byte-for-byte reference of the frame described above.
const REFERENCE_ARTDMX: &[u8] = &[
    b'A', b'r', b't', b'-', b'N', b'e', b't', 0x00, // "Art-Net\0"
    0x00, 0x50, // OpCode 0x5000 (OpDmx), low byte first
    0x00, 0x0E, // ProtVer 14, high byte first
    0x01, // Sequence
    0x00, // Physical
    0x01, // SubUni  -> universe 1
    0x00, // Net     -> 0
    0x00, 0x06, // Length = 6, high byte first
    0xFF, 0x00, 0x00, // pixel 0: red
    0x00, 0xFF, 0x00, // pixel 1: green
];

#[test]
fn encodes_reference_frame() {
    let data = [0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00];
    let mut buf = Vec::new();
    ArtDmx {
        sequence: 1,
        physical: 0,
        port_address: 1,
        data: &data,
    }
    .write_to(&mut buf);

    assert_eq!(buf, REFERENCE_ARTDMX);
}

#[test]
fn decodes_reference_frame() {
    let parsed = artnet::parse_artdmx(REFERENCE_ARTDMX).expect("valid ArtDmx");
    assert_eq!(parsed.sequence, 1);
    assert_eq!(parsed.port_address, 1);
    assert_eq!(parsed.data, &[0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00]);
}

#[test]
fn buffer_reuse_does_not_leak_previous_frame() {
    // The output thread reuses one buffer across frames; a shorter frame after
    // a longer one must not carry stale tail bytes.
    let mut buf = Vec::new();
    ArtDmx {
        sequence: 1,
        physical: 0,
        port_address: 1,
        data: &[9u8; 512],
    }
    .write_to(&mut buf);
    ArtDmx {
        sequence: 2,
        physical: 0,
        port_address: 1,
        data: &[1, 2],
    }
    .write_to(&mut buf);

    assert_eq!(buf.len(), artnet::ARTDMX_HEADER_LEN + 2);
    assert_eq!(&buf[artnet::ARTDMX_HEADER_LEN..], &[1, 2]);
}
