//! The fixture library (M7 spec §3.2): the operator's reusable set of pixel-LED
//! fixture types, kept in **its own file** so it carries across events — the operator
//! defines a type once ("60px WS2811 tube") and instantiates it in many patches.
//!
//! # Separate file + snapshot
//!
//! The library is never referenced by a `.prismap`. Placement ([`crate::plan`])
//! **snapshots** the chosen type's resolved profile into each fixture (its
//! `FixtureProfile` and per-pixel colour order), so a saved patch is self-contained and
//! opens unchanged even if the library is absent, edited, or on another machine. The
//! library only feeds the placement dialog and the manager UI.
//!
//! # File format
//!
//! Same envelope convention as patches (§9): a `format` tag, a `schema_version` driving
//! newer-than-reader refusal, and `written_by` for diagnostics. New type fields are
//! `#[serde(default)]` so an older library loads; a breaking change adds a migration
//! step here and bumps [`LIBRARY_SCHEMA_VERSION`].

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::placement::{builtin_fixture_types, FixtureType};

/// The `format` tag every Prisma fixture-library file carries.
pub const LIBRARY_FORMAT_ID: &str = "prisma.library";

/// The newest library schema this build writes and reads up to. `1` — initial M7
/// schema: a named collection of pixel-LED [`FixtureType`]s.
pub const LIBRARY_SCHEMA_VERSION: u32 = 1;

/// A named, reusable collection of pixel-LED fixture types (§3.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixtureLibrary {
    pub id: Uuid,
    pub name: String,
    pub types: Vec<FixtureType>,
}

impl FixtureLibrary {
    /// A fresh library seeded with a few generic pixel-LED types, written the first
    /// time the app runs so the placement dialog is never empty (§3.2). The operator
    /// edits it from there.
    pub fn seed() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "My fixtures".into(),
            types: builtin_fixture_types(),
        }
    }

    /// Append a type (manager "add").
    pub fn add_type(&mut self, ty: FixtureType) {
        self.types.push(ty);
    }

    /// Replace the type at `index` (manager "edit").
    pub fn update_type(&mut self, index: usize, ty: FixtureType) -> Result<(), LibraryError> {
        let slot = self
            .types
            .get_mut(index)
            .ok_or(LibraryError::NoSuchType { index })?;
        *slot = ty;
        Ok(())
    }

    /// Remove the type at `index` (manager "delete").
    pub fn remove_type(&mut self, index: usize) -> Result<(), LibraryError> {
        if index >= self.types.len() {
            return Err(LibraryError::NoSuchType { index });
        }
        self.types.remove(index);
        Ok(())
    }
}

/// Anything that can go wrong loading, saving or editing a library.
#[derive(Debug)]
pub enum LibraryError {
    Json(serde_json::Error),
    BadFormat { found: String },
    NewerThanReader { file: u32, reader: u32 },
    NoSuchType { index: usize },
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LibraryError::Json(e) => write!(f, "malformed library JSON: {e}"),
            LibraryError::BadFormat { found } => {
                write!(f, "not a Prisma fixture library (format = {found:?})")
            }
            LibraryError::NewerThanReader { file, reader } => write!(
                f,
                "this library was created by a newer version of Prisma \
                 (schema v{file}, this build reads up to v{reader}); please update Prisma"
            ),
            LibraryError::NoSuchType { index } => {
                write!(f, "no fixture type at index {index}")
            }
        }
    }
}

impl std::error::Error for LibraryError {}

impl From<serde_json::Error> for LibraryError {
    fn from(e: serde_json::Error) -> Self {
        LibraryError::Json(e)
    }
}

/// The on-disk wrapper around a library body. The body is kept generic
/// (`serde_json::Value`) so the version can be checked before committing to the current
/// struct shape — the same pattern as the patch envelope.
#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    schema_version: u32,
    #[serde(default)]
    written_by: String,
    library: serde_json::Value,
}

/// Serialize a library to a JSON string with the current envelope.
pub fn save_library(library: &FixtureLibrary) -> Result<String, LibraryError> {
    let envelope = Envelope {
        format: LIBRARY_FORMAT_ID.to_string(),
        schema_version: LIBRARY_SCHEMA_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_string(),
        library: serde_json::to_value(library)?,
    };
    Ok(serde_json::to_string_pretty(&envelope)?)
}

/// Load a library from a JSON string: check the envelope, refuse a newer schema, then
/// deserialize. (No migrations yet — v1 is the only schema; new fields use
/// `#[serde(default)]`.)
pub fn load_library(json: &str) -> Result<FixtureLibrary, LibraryError> {
    let envelope: Envelope = serde_json::from_str(json)?;
    if envelope.format != LIBRARY_FORMAT_ID {
        return Err(LibraryError::BadFormat {
            found: envelope.format,
        });
    }
    if envelope.schema_version > LIBRARY_SCHEMA_VERSION {
        return Err(LibraryError::NewerThanReader {
            file: envelope.schema_version,
            reader: LIBRARY_SCHEMA_VERSION,
        });
    }
    Ok(serde_json::from_value(envelope.library)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::FixtureArrangement;

    #[test]
    fn seed_is_non_empty_and_round_trips() {
        let lib = FixtureLibrary::seed();
        assert!(
            !lib.types.is_empty(),
            "seed must not leave the dialog empty"
        );
        let json = save_library(&lib).unwrap();
        assert_eq!(
            load_library(&json).unwrap(),
            lib,
            "library survives save/load"
        );
    }

    #[test]
    fn envelope_carries_format_and_version() {
        let json = save_library(&FixtureLibrary::seed()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["format"], LIBRARY_FORMAT_ID);
        assert_eq!(value["schema_version"], LIBRARY_SCHEMA_VERSION);
    }

    #[test]
    fn refuses_a_newer_schema() {
        let json = save_library(&FixtureLibrary::seed()).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["schema_version"] = serde_json::json!(LIBRARY_SCHEMA_VERSION + 1);
        let forged = serde_json::to_string(&value).unwrap();
        assert!(matches!(
            load_library(&forged),
            Err(LibraryError::NewerThanReader { .. })
        ));
    }

    #[test]
    fn rejects_a_foreign_format() {
        let json = save_library(&FixtureLibrary::seed()).unwrap();
        let broken = json.replace("prisma.library", "prisma.patch");
        assert!(matches!(
            load_library(&broken),
            Err(LibraryError::BadFormat { .. })
        ));
    }

    #[test]
    fn crud_add_update_remove() {
        let mut lib = FixtureLibrary::seed();
        let n = lib.types.len();
        let mut ty = lib.types[0].clone();
        ty.name = "New Tube".into();

        lib.add_type(ty.clone());
        assert_eq!(lib.types.len(), n + 1);
        assert_eq!(lib.types[n].name, "New Tube");

        let mut edited = ty.clone();
        edited.pixel_count = 144;
        lib.update_type(n, edited).unwrap();
        assert_eq!(lib.types[n].pixel_count, 144);

        lib.remove_type(n).unwrap();
        assert_eq!(lib.types.len(), n);
        assert!(lib.update_type(999, ty.clone()).is_err());
        assert!(lib.remove_type(999).is_err());
    }

    #[test]
    fn an_older_library_missing_new_fields_still_loads() {
        // A hand-written / pre-arrangement library: a type with only the base fields.
        let json = r#"{
            "format": "prisma.library",
            "schema_version": 1,
            "library": {
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "old",
                "types": [
                    { "name": "Tube", "pixel_count": 30, "components": "Rgb", "color_order": "Grb" }
                ]
            }
        }"#;
        let lib = load_library(json).expect("older library loads via serde defaults");
        assert_eq!(lib.types[0].arrangement, FixtureArrangement::Strip);
        assert!(lib.types[0].roles.is_empty());
    }
}
