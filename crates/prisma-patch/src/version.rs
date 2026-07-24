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
///
/// - **v1** — initial M2 schema: one global `Patch::matrix_size`; zones carry only
///   overlap membership; fixtures have no ownership field.
/// - **v2** — M7: each zone gains its own `mapping_size` (§2.4), a `display_color`
///   and an optional `world_transform`; each fixture gains `zone` ownership (§2.2).
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

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
/// `OLDEST_SUPPORTED + i` → `OLDEST_SUPPORTED + i + 1`. A breaking change adds one
/// entry here and bumps [`CURRENT_SCHEMA_VERSION`]. The `len` invariant is
/// asserted in tests.
static MIGRATIONS: &[Migration] = &[
    migrate_v1_to_v2, // 1 -> 2
];

/// A deterministic palette of distinct, legible zone display colours, cycled by
/// zone index so a migrated patch opens with visually separable zones. The editor
/// lets the operator recolour; this only seeds sensible defaults.
const ZONE_PALETTE: &[[u8; 3]] = &[
    [0xE6, 0x39, 0x46], // red
    [0x45, 0x7B, 0x9D], // blue
    [0x2A, 0x9D, 0x8F], // teal
    [0xE9, 0xC4, 0x6A], // gold
    [0xF4, 0xA2, 0x61], // orange
    [0x8E, 0x7D, 0xBE], // violet
    [0x6A, 0xA8, 0x4F], // green
    [0xD6, 0x72, 0xA8], // pink
];

/// Id of the single "Default" zone the v1→v2 migration synthesizes **only** when a
/// v1 patch has fixtures but no zones at all (a degenerate but representable state,
/// since the v1 model allowed an empty `zones`). Fixtures with no owning zone are
/// assigned to it so the v2 invariant "every fixture belongs to a zone" holds.
/// A fixed id keeps the migration deterministic (a random id would make golden
/// tests flap).
const SYNTHESIZED_DEFAULT_ZONE_ID: &str = "00000000-0000-0000-0000-0000d3fa17e0";

/// v1 → v2 (M7). Purely **additive** field injection over the generic body — it
/// never restructures existing fields, so an unexpected shape degrades to leaving
/// data untouched rather than corrupting it.
///
/// Per zone: seed `mapping_size` from the old global `matrix_size` (the zone
/// starts as large as the whole v1 matrix — the operator resizes per zone in the
/// editor), a palette `display_color`, and `world_transform = null`.
///
/// Per fixture: derive **ownership** (`zone`). v1 had no ownership; a fixture's
/// pixels could even be split across zones. The rule, documented so a surprised
/// operator can trace it:
///
/// > Owner = the zone (by overlap membership) that covers this fixture with the
/// > **smallest total member count**, ties broken by declaration order. A global
/// > strobe/flash zone spans everything, so it has the largest membership and
/// > never wins ownership; a per-sector zone wins. A fixture split across two
/// > sectors goes to the earlier-declared one — the operator reassigns it in the
/// > editor, where the new one-fixture-one-zone rule (§2.2) is enforced.
///
/// A fixture covered by no zone is assigned to a synthesized "Default" zone
/// ([`SYNTHESIZED_DEFAULT_ZONE_ID`]), created once if and only if it is needed.
fn migrate_v1_to_v2(mut body: serde_json::Value) -> Result<serde_json::Value, PatchError> {
    use serde_json::{json, Value};

    let Some(obj) = body.as_object_mut() else {
        return Ok(body); // not an object we recognise; leave untouched
    };

    // The old global matrix seeds every zone's local mapping space.
    let global_matrix = obj
        .get("matrix_size")
        .cloned()
        .unwrap_or_else(|| json!([1, 1, 1]));

    // Pass 1: index the zones to derive fixture ownership. Record, per zone, its
    // id, its total member count, and the set of fixture ids it covers.
    struct ZoneInfo {
        id: String,
        member_count: usize,
        covers: std::collections::HashSet<String>,
    }
    let mut zone_infos: Vec<ZoneInfo> = Vec::new();
    if let Some(Value::Array(zones)) = obj.get("zones") {
        for zone in zones {
            let id = zone
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let members = zone.get("members").and_then(Value::as_array);
            let member_count = members.map(|m| m.len()).unwrap_or(0);
            let covers = members
                .map(|m| {
                    m.iter()
                        .filter_map(|r| r.get("fixture").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            zone_infos.push(ZoneInfo {
                id,
                member_count,
                covers,
            });
        }
    }

    // Derive each fixture's owner. Returns the owning zone id, or None if no zone
    // covers the fixture (→ the synthesized default).
    let owner_of = |fixture_id: &str| -> Option<String> {
        zone_infos
            .iter()
            .enumerate()
            .filter(|(_, z)| z.covers.contains(fixture_id))
            // smallest membership wins; declaration order (enumerate index) breaks ties
            .min_by_key(|(idx, z)| (z.member_count, *idx))
            .map(|(_, z)| z.id.clone())
    };

    // Pass 2: assign ownership to fixtures, tracking whether any orphan needs the
    // synthesized default zone.
    let mut needs_default_zone = false;
    if let Some(Value::Array(fixtures)) = obj.get_mut("fixtures") {
        for fixture in fixtures.iter_mut() {
            let fixture_id = fixture
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let owner = owner_of(&fixture_id).unwrap_or_else(|| {
                needs_default_zone = true;
                SYNTHESIZED_DEFAULT_ZONE_ID.to_string()
            });
            if let Some(fobj) = fixture.as_object_mut() {
                fobj.insert("zone".to_string(), json!(owner));
            }
        }
    }

    // Pass 3: inject the editor fields into every existing zone.
    if let Some(Value::Array(zones)) = obj.get_mut("zones") {
        for (idx, zone) in zones.iter_mut().enumerate() {
            if let Some(zobj) = zone.as_object_mut() {
                zobj.insert("mapping_size".to_string(), global_matrix.clone());
                zobj.insert(
                    "display_color".to_string(),
                    json!(ZONE_PALETTE[idx % ZONE_PALETTE.len()]),
                );
                zobj.entry("world_transform").or_insert(Value::Null);
            }
        }
    }

    // Append the synthesized default zone only if an orphan fixture needed it.
    if needs_default_zone {
        if let Some(Value::Array(zones)) = obj.get_mut("zones") {
            let idx = zones.len();
            zones.push(json!({
                "id": SYNTHESIZED_DEFAULT_ZONE_ID,
                "name": "Default",
                "members": [],
                "bounds": null,
                "z_order": 0,
                "blend_mode": "Normal",
                "mapping_size": global_matrix,
                "display_color": ZONE_PALETTE[idx % ZONE_PALETTE.len()],
                "world_transform": null,
            }));
        }
    }

    Ok(body)
}

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
