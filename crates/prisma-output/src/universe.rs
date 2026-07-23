//! Universe mapping: which slice of the linear master buffer feeds which DMX
//! universe.
//!
//! This is the buffer→DMX side of the mapping only. Where a universe is *sent*
//! (target address, network interface) and *when* (rate divisor vs the base
//! tick, §1.4) are attached by the transport and scheduler in step 4, kept
//! separate so this stays a pure data description with no I/O.
//!
//! A pixel's physical DMX address comes from the Patch in M2; for M1 a
//! [`Universe`] is configured directly against byte offsets into the master
//! buffer.

/// Maximum DMX channels in one universe.
pub const CHANNELS_PER_UNIVERSE: usize = 512;

/// Maps a contiguous byte range of the master pixel buffer onto one DMX
/// universe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Universe {
    /// Art-Net 15-bit port address (`Net : Sub-Net : Universe`). When sACN/DDP
    /// join, this generalizes to a protocol-tagged address.
    pub port_address: u16,
    /// Byte offset of this universe's first channel in the master buffer.
    pub source_offset: usize,
    /// Number of DMX channels this universe carries, `1..=512`.
    pub channel_count: usize,
}

impl Universe {
    /// Create a universe, clamping `channel_count` to the valid `1..=512` range.
    pub fn new(port_address: u16, source_offset: usize, channel_count: usize) -> Self {
        Self {
            port_address,
            source_offset,
            channel_count: channel_count.clamp(1, CHANNELS_PER_UNIVERSE),
        }
    }
}
