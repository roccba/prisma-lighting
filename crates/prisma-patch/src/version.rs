//! On-disk envelope, versioning and migration for patch files.
//!
//! Patches serialize to JSON (CLAUDE.md §4.1: human-readable, diffable) wrapped
//! in a small envelope carrying a `schema_version`. The version is what lets
//! users **share patch files across product versions** safely (§3, M2 design):
//!
//! - **Older file, newer reader:** new fields are `#[serde(default)]`, and any
//!   breaking shape change is handled by a migration in [`migrate`]. The file
//!   loads.
//! - **Newer file, older reader:** we refuse with
//!   [`PatchError::NewerThanReader`] rather than deserialize partially and
//!   corrupt a file someone else will open. `written_by` gives the error a
//!   concrete "update Prisma" message.
//!
//! Collections in the model are `Vec`s (never `HashMap`) so serialization order
//! is deterministic and diffs stay small.

use crate::error::PatchError;
use crate::patch::Patch;
use serde::{Deserialize, Serialize};

/// The `format` tag every Prisma patch file carries.
pub const FORMAT_ID: &str = "prisma.patch";

/// The newest schema version this build writes and can read up to.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// The on-disk wrapper around a patch body.
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    /// Must equal [`FORMAT_ID`]; guards against loading an unrelated JSON file.
    format: String,
    /// Schema version of `patch`. Drives migration and the newer-than-reader
    /// refusal.
    schema_version: u32,
    /// Product version string that wrote the file (for diagnostics only).
    #[serde(default)]
    written_by: String,
    /// The patch body, kept generic so migrations can run before we commit to
    /// the current struct shape.
    patch: serde_json::Value,
}

/// Serialize a patch to a JSON string with the current envelope.
///
/// The patch is **not** validated here — call [`Patch::validate`] or
/// [`Patch::compile`] first if you need that guarantee. Saving is deliberately
/// allowed to persist work-in-progress.
pub fn save_patch(patch: &Patch) -> Result<String, PatchError> {
    let envelope = Envelope {
        format: FORMAT_ID.to_string(),
        schema_version: CURRENT_SCHEMA_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_string(),
        patch: serde_json::to_value(patch)?,
    };
    Ok(serde_json::to_string_pretty(&envelope)?)
}

/// Load a patch from a JSON string: check the envelope, migrate the body up to
/// the current schema, then deserialize.
///
/// Does not validate structural invariants (addresses, zone refs) — use
/// [`Patch::validate`] / [`Patch::compile`] on the returned patch.
pub fn load_patch(json: &str) -> Result<Patch, PatchError> {
    let envelope: Envelope = serde_json::from_str(json)?;

    if envelope.format != FORMAT_ID {
        return Err(PatchError::BadFormat {
            found: envelope.format,
        });
    }
    if envelope.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(PatchError::NewerThanReader {
            file: envelope.schema_version,
            reader: CURRENT_SCHEMA_VERSION,
        });
    }

    let body = migrate(envelope.patch, envelope.schema_version)?;
    Ok(serde_json::from_value(body)?)
}

/// One `vN → vN+1` migration over the generic body.
type Migration = fn(serde_json::Value) -> Result<serde_json::Value, PatchError>;

/// The oldest schema version this build can migrate from. Bump when support for
/// an ancient version is dropped.
const OLDEST_SUPPORTED: u32 = 1;

/// The migration chain, one step per version gap. `MIGRATIONS[i]` upgrades
/// `OLDEST_SUPPORTED + i` → `OLDEST_SUPPORTED + i + 1`. There are none yet (v1 is
/// the initial schema); a breaking change adds one entry here and bumps
/// [`CURRENT_SCHEMA_VERSION`]. The `len` invariant is asserted in tests.
static MIGRATIONS: &[Migration] = &[
    // 1 -> 2: migrate_v1_to_v2,
];

/// Migrate a patch body from `from_version` up to [`CURRENT_SCHEMA_VERSION`] by
/// applying each step in [`MIGRATIONS`] in turn. A version below
/// [`OLDEST_SUPPORTED`] is rejected as [`PatchError::UnknownSchema`]. (Newer than
/// current is already refused by the caller.)
fn migrate(body: serde_json::Value, from_version: u32) -> Result<serde_json::Value, PatchError> {
    if from_version < OLDEST_SUPPORTED {
        return Err(PatchError::UnknownSchema {
            version: from_version,
        });
    }
    let mut body = body;
    for step in MIGRATIONS
        .iter()
        .skip((from_version - OLDEST_SUPPORTED) as usize)
    {
        body = step(body)?;
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_chain_covers_every_version_gap() {
        // One migration per version step, or the chain silently skips a version.
        assert_eq!(
            MIGRATIONS.len() as u32,
            CURRENT_SCHEMA_VERSION - OLDEST_SUPPORTED,
            "MIGRATIONS must have exactly CURRENT - OLDEST entries"
        );
    }
}
