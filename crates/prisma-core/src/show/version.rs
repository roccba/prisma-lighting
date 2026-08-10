//! On-disk envelope, versioning and migration for show files (`.prisma`).
//!
//! Mirrors `prisma_patch::version` exactly (M8-SPEC §5: "same conventions as the
//! patch"). Shows serialize to JSON wrapped in a small envelope carrying a
//! `schema_version`:
//!
//! - **Older file, newer reader:** new fields are `#[serde(default)]`, and any
//!   breaking shape change is handled by a migration in [`migrate`]. The file
//!   loads.
//! - **Newer file, older reader:** refused with [`ShowError::NewerThanReader`]
//!   rather than deserialized partially and corrupt a file someone else opens.
//!
//! Loading does **not** validate structural invariants (zone bijection, active
//! range, the embedded patch) — call [`Show::validate`] on the result, as the
//! patch pipeline does. Saving is deliberately allowed to persist
//! work-in-progress.

use serde::{Deserialize, Serialize};

use crate::show::{Show, ShowError};

/// The `format` tag every Prisma show file carries. Distinct from the patch's
/// `prisma.patch`, so a `.prismap` handed to the show loader is rejected as
/// [`ShowError::BadFormat`] rather than mis-parsed.
pub const FORMAT_ID: &str = "prisma.show";

/// The newest schema version this build writes and can read up to.
///
/// - **v1** — initial M8 schema: embedded patch, per-zone grids (`ZoneShow`), cells
///   holding one layer, master state, reserved network config.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// The on-disk wrapper around a show body.
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    /// Must equal [`FORMAT_ID`]; guards against loading an unrelated JSON file.
    format: String,
    /// Schema version of `show`. Drives migration and the newer-than-reader refusal.
    schema_version: u32,
    /// Product version string that wrote the file (diagnostics only).
    #[serde(default)]
    written_by: String,
    /// The show body, kept generic so migrations can run before we commit to the
    /// current struct shape. `#[serde(default)]` so a wrong file type (e.g. a
    /// `.prismap`, whose body field is `patch`, not `show`) is caught by the
    /// `format` check below with a clear [`ShowError::BadFormat`] rather than a
    /// cryptic "missing field `show`".
    #[serde(default)]
    show: serde_json::Value,
}

/// Serialize a show to a JSON string with the current envelope.
///
/// The show is **not** validated here — call [`Show::validate`] first if you need
/// that guarantee. Saving work-in-progress is deliberately allowed.
pub fn save_show(show: &Show) -> Result<String, ShowError> {
    let envelope = Envelope {
        format: FORMAT_ID.to_string(),
        schema_version: CURRENT_SCHEMA_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_string(),
        show: serde_json::to_value(show)?,
    };
    Ok(serde_json::to_string_pretty(&envelope)?)
}

/// Load a show from a JSON string: check the envelope, migrate the body up to the
/// current schema, then deserialize. Does not validate — use [`Show::validate`].
pub fn load_show(json: &str) -> Result<Show, ShowError> {
    let envelope: Envelope = serde_json::from_str(json)?;

    if envelope.format != FORMAT_ID {
        return Err(ShowError::BadFormat {
            found: envelope.format,
        });
    }
    if envelope.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(ShowError::NewerThanReader {
            file: envelope.schema_version,
            reader: CURRENT_SCHEMA_VERSION,
        });
    }

    let body = migrate(envelope.show, envelope.schema_version)?;
    Ok(serde_json::from_value(body)?)
}

/// One `vN → vN+1` migration over the generic body.
type Migration = fn(serde_json::Value) -> Result<serde_json::Value, ShowError>;

/// The oldest schema version this build can migrate from.
const OLDEST_SUPPORTED: u32 = 1;

/// The migration chain, one step per version gap. `MIGRATIONS[i]` upgrades
/// `OLDEST_SUPPORTED + i` → `OLDEST_SUPPORTED + i + 1`. A breaking change adds one
/// entry here and bumps [`CURRENT_SCHEMA_VERSION`]. The `len` invariant is asserted
/// in tests. Empty at v1 — there is nothing older to migrate from yet.
static MIGRATIONS: &[Migration] = &[];

/// Migrate a show body from `from_version` up to [`CURRENT_SCHEMA_VERSION`] by
/// applying each step in [`MIGRATIONS`] in turn. A version below
/// [`OLDEST_SUPPORTED`] is rejected as [`ShowError::UnknownSchema`]. (Newer than
/// current is already refused by the caller.)
fn migrate(body: serde_json::Value, from_version: u32) -> Result<serde_json::Value, ShowError> {
    if from_version < OLDEST_SUPPORTED {
        return Err(ShowError::UnknownSchema {
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
