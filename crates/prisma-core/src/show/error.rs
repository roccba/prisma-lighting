//! Errors from loading, saving and validating a Show (`.prisma`).

use uuid::Uuid;

/// Everything that can go wrong loading, saving or validating a Show.
#[derive(Debug)]
pub enum ShowError {
    /// The envelope's `format` tag was not [`FORMAT_ID`](super::FORMAT_ID) — most
    /// likely an unrelated JSON file (or a `.prismap` handed to the show loader).
    BadFormat { found: String },
    /// The file's schema is newer than this build can read. Refused rather than
    /// deserialized partially, which would corrupt a file someone else opens.
    NewerThanReader { file: u32, reader: u32 },
    /// The file's schema is older than the oldest this build can migrate from.
    UnknownSchema { version: u32 },
    /// A zone show targets a `zone_id` absent from the embedded patch.
    UnknownZone { zone: Uuid },
    /// Two zone shows target the same patch zone.
    DuplicateZone { zone: Uuid },
    /// Some patch zones have no zone show — the bijection is incomplete.
    MissingZones { expected: usize, found: usize },
    /// A track's `active` index is out of range of its clips.
    ActiveOutOfRange {
        track: Uuid,
        active: usize,
        clips: usize,
    },
    /// Two tracks share an id.
    DuplicateTrack { track: Uuid },
    /// A track's destination names a zone absent from the embedded patch.
    UnknownDestinationZone { track: Uuid, zone: Uuid },
    /// A track's destination names a group absent from the embedded patch.
    UnknownDestinationGroup { track: Uuid, group: Uuid },
    /// A group master or preset names a group absent from the embedded patch.
    UnknownGroup { group: Uuid },
    /// The network/output config is malformed (e.g. a reversed universe range).
    InvalidNetwork(String),
    /// The embedded patch failed its own validation.
    Patch(prisma_patch::PatchError),
    /// JSON (de)serialization failed.
    Json(serde_json::Error),
}

impl std::fmt::Display for ShowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShowError::BadFormat { found } => {
                write!(f, "not a Prisma show file (format tag was {found:?})")
            }
            ShowError::NewerThanReader { file, reader } => write!(
                f,
                "show file schema v{file} is newer than this build reads (v{reader}); update Prisma"
            ),
            ShowError::UnknownSchema { version } => {
                write!(f, "show file schema v{version} is too old to migrate")
            }
            ShowError::UnknownZone { zone } => {
                write!(
                    f,
                    "zone show targets zone {zone}, which is not in the patch"
                )
            }
            ShowError::DuplicateZone { zone } => {
                write!(f, "zone {zone} has more than one zone show")
            }
            ShowError::MissingZones { expected, found } => write!(
                f,
                "show covers {found} of {expected} patch zones (one zone show per zone required)"
            ),
            ShowError::ActiveOutOfRange {
                track,
                active,
                clips,
            } => write!(
                f,
                "track {track} active clip {active} is out of range (track has {clips} clips)"
            ),
            ShowError::DuplicateTrack { track } => {
                write!(f, "track id {track} is used by more than one track")
            }
            ShowError::UnknownDestinationZone { track, zone } => write!(
                f,
                "track {track} targets zone {zone}, which is not in the patch"
            ),
            ShowError::UnknownGroup { group } => {
                write!(f, "group {group} is not in the patch")
            }
            ShowError::UnknownDestinationGroup { track, group } => write!(
                f,
                "track {track} targets group {group}, which is not in the patch"
            ),
            ShowError::InvalidNetwork(msg) => write!(f, "network config invalid: {msg}"),
            ShowError::Patch(e) => write!(f, "embedded patch invalid: {e}"),
            ShowError::Json(e) => write!(f, "show JSON error: {e}"),
        }
    }
}

impl std::error::Error for ShowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ShowError::Patch(e) => Some(e),
            ShowError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for ShowError {
    fn from(e: serde_json::Error) -> Self {
        ShowError::Json(e)
    }
}

impl From<prisma_patch::PatchError> for ShowError {
    fn from(e: prisma_patch::PatchError) -> Self {
        ShowError::Patch(e)
    }
}
