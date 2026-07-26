//! Errors surfaced by loading, validating and compiling a patch.

use std::fmt;
use uuid::Uuid;

/// Anything that can go wrong turning bytes on disk into a validated,
/// index-ready [`Patch`](crate::Patch).
#[derive(Debug)]
pub enum PatchError {
    /// The JSON envelope or body failed to parse.
    Json(serde_json::Error),
    /// The `format` tag was not the expected `"prisma.patch"`.
    BadFormat { found: String },
    /// The file was written by a newer schema than this build understands. We
    /// refuse rather than load partially and corrupt a shared file (§3, M2).
    NewerThanReader { file: u32, reader: u32 },
    /// A `schema_version` older than any we have a migration path from.
    UnknownSchema { version: u32 },
    /// A zone references a pixel that does not exist in the patch.
    DanglingPixelRef {
        zone: Uuid,
        fixture: Uuid,
        pixel: u32,
    },
    /// A zone references a fixture that does not exist in the patch.
    DanglingFixtureRef { zone: Uuid, fixture: Uuid },
    /// A fixture's pixels are not densely indexed `0..len` in vector order.
    /// The model requires `pixels[i].index == i` so that a `PixelRef` and the
    /// master-buffer ordering have exactly one meaning.
    NonDensePixelIndex {
        fixture: Uuid,
        position: u32,
        index: u32,
    },
    /// A pixel's DMX span (`start_channel .. start_channel + components`) runs
    /// past channel 512 or starts below 1.
    ChannelOutOfRange {
        fixture: Uuid,
        pixel: u32,
        start: u16,
        span: u16,
    },
    /// Two pixels claim an overlapping `(universe, channel)` range.
    AddressCollision {
        universe: u16,
        channel: u16,
        first: Uuid,
        second: Uuid,
    },
    /// A zone's own local mapping matrix (`Zone::mapping_size`, M7 spec §2.4) has
    /// a zero axis. A matrix must have at least one cell on every axis to hold any
    /// content, so a zero is a construction/edit error refused at load.
    ZoneMappingSizeZero { zone: Uuid, mapping_size: [u32; 3] },
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatchError::Json(e) => write!(f, "malformed patch JSON: {e}"),
            PatchError::BadFormat { found } => {
                write!(f, "not a Prisma patch file (format = {found:?})")
            }
            PatchError::NewerThanReader { file, reader } => write!(
                f,
                "this patch was created by a newer version of Prisma \
                 (schema v{file}, this build reads up to v{reader}); please update Prisma"
            ),
            PatchError::UnknownSchema { version } => {
                write!(f, "unknown patch schema version v{version}")
            }
            PatchError::DanglingPixelRef {
                zone,
                fixture,
                pixel,
            } => write!(
                f,
                "zone {zone} references pixel {pixel} of fixture {fixture}, which does not exist"
            ),
            PatchError::DanglingFixtureRef { zone, fixture } => write!(
                f,
                "zone {zone} references fixture {fixture}, which does not exist"
            ),
            PatchError::NonDensePixelIndex {
                fixture,
                position,
                index,
            } => write!(
                f,
                "fixture {fixture} pixel at position {position} declares index {index} \
                 (pixels must be densely indexed 0..len in order)"
            ),
            PatchError::ChannelOutOfRange {
                fixture,
                pixel,
                start,
                span,
            } => write!(
                f,
                "pixel {pixel} of fixture {fixture} spans channels {start}..{} \
                 (must be within 1..=512)",
                start.saturating_add(*span)
            ),
            PatchError::AddressCollision {
                universe,
                channel,
                first,
                second,
            } => write!(
                f,
                "fixtures {first} and {second} both claim universe {universe} channel {channel}"
            ),
            PatchError::ZoneMappingSizeZero { zone, mapping_size } => write!(
                f,
                "zone {zone} has a zero axis in its mapping size {mapping_size:?} \
                 (every axis must be >= 1)"
            ),
        }
    }
}

impl std::error::Error for PatchError {}

impl From<serde_json::Error> for PatchError {
    fn from(e: serde_json::Error) -> Self {
        PatchError::Json(e)
    }
}
