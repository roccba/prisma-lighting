//! The [`Patch`] aggregate: the fixed physical/logical wiring of the rig
//! (CLAUDE.md §4.1). Holds no performable state — that lives in the Show.
//!
//! `Patch` is the serialized model. Its runtime derivative (global pixel
//! indices, master-buffer layout, resolved zone membership) is
//! [`PatchIndex`](crate::index::PatchIndex), built by [`Patch::compile`] and
//! never persisted.

use crate::error::PatchError;
use crate::fixture::Fixture;
use crate::index::PatchIndex;
use crate::zone::Zone;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// The physical/logical wiring: where every pixel lives and how pixels group
/// into zones. One Patch backs many Shows (§3.1 principle 6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub id: Uuid,
    pub name: String,
    /// Virtual matrix dimensions. `Z = 1` for the current 2D rig, `> 1` reserved
    /// for future 3D patching (Deferred).
    pub matrix_size: [u32; 3],
    pub fixtures: Vec<Fixture>,
    pub zones: Vec<Zone>,
}

impl Patch {
    /// Validate structural invariants that are checkable from the model alone:
    /// DMX channel ranges, address collisions, and dangling zone references.
    ///
    /// This does **not** assign global indices or build the master-buffer layout
    /// — that is [`compile`](Self::compile). `compile` runs `validate` first, so
    /// callers that only want a validated model can call this directly.
    pub fn validate(&self) -> Result<(), PatchError> {
        self.validate_pixel_indices()?;
        self.validate_addresses()?;
        self.validate_zone_refs()?;
        self.validate_zone_mapping_sizes()?;
        Ok(())
    }

    /// Compile the patch into its runtime [`PatchIndex`]: assign global pixel
    /// indices, build the canonical master-buffer layout, and resolve zone
    /// membership to dense indices. Mints a fresh
    /// [`LayoutGeneration`](crate::index::LayoutGeneration).
    pub fn compile(&self) -> Result<PatchIndex, PatchError> {
        self.validate()?;
        PatchIndex::build(self)
    }

    /// Look up a fixture by id.
    pub fn fixture(&self, id: Uuid) -> Option<&Fixture> {
        self.fixtures.iter().find(|f| f.id == id)
    }

    fn validate_pixel_indices(&self) -> Result<(), PatchError> {
        for fixture in &self.fixtures {
            for (position, pixel) in fixture.pixels.iter().enumerate() {
                if pixel.index as usize != position {
                    return Err(PatchError::NonDensePixelIndex {
                        fixture: fixture.id,
                        position: position as u32,
                        index: pixel.index,
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_addresses(&self) -> Result<(), PatchError> {
        // (universe, channel) -> first fixture that claimed it. A patch is
        // thousands of pixels, so this is a load-time cost, not a hot path.
        let mut claimed: HashMap<(u16, u16), Uuid> = HashMap::new();
        for fixture in &self.fixtures {
            let span = fixture.profile.component_count() as u16;
            for pixel in &fixture.pixels {
                let start = pixel.start_channel;
                // Channels are 1..=512; the whole span must fit.
                let end = start.saturating_add(span);
                if start < 1 || span == 0 || end > super::CHANNELS_PER_UNIVERSE as u16 + 1 {
                    return Err(PatchError::ChannelOutOfRange {
                        fixture: fixture.id,
                        pixel: pixel.index,
                        start,
                        span,
                    });
                }
                for channel in start..end {
                    if let Some(&first) = claimed.get(&(pixel.universe, channel)) {
                        if first != fixture.id {
                            return Err(PatchError::AddressCollision {
                                universe: pixel.universe,
                                channel,
                                first,
                                second: fixture.id,
                            });
                        }
                    }
                    claimed.insert((pixel.universe, channel), fixture.id);
                }
            }
        }
        Ok(())
    }

    /// Every zone's own local mapping matrix (§2.4) must have at least one cell on
    /// every axis. `Z = 1` is the norm for the current 2D rig. This validates the
    /// zone's *own* space.
    ///
    /// Note there is deliberately **no** check that pixels land inside a matrix: a
    /// pixel mapped outside its zone's matrix is a valid placement (M7 spec §2.4) —
    /// it falls on no cell, so the render gather samples nothing and it stays dark
    /// (the gather reads black out of range, defensively). A matrix is a **render
    /// resolution**, not a fence; out-of-matrix is not an error, so `compile` must
    /// not reject it.
    fn validate_zone_mapping_sizes(&self) -> Result<(), PatchError> {
        for zone in &self.zones {
            if zone.mapping_size.contains(&0) {
                return Err(PatchError::ZoneMappingSizeZero {
                    zone: zone.id,
                    mapping_size: zone.mapping_size,
                });
            }
        }
        Ok(())
    }

    fn validate_zone_refs(&self) -> Result<(), PatchError> {
        // fixture id -> pixel count, to bounds-check PixelRefs.
        let counts: HashMap<Uuid, u32> = self
            .fixtures
            .iter()
            .map(|f| (f.id, f.pixels.len() as u32))
            .collect();
        for zone in &self.zones {
            for member in &zone.members {
                match counts.get(&member.fixture) {
                    None => {
                        return Err(PatchError::DanglingFixtureRef {
                            zone: zone.id,
                            fixture: member.fixture,
                        })
                    }
                    Some(&count) if member.pixel >= count => {
                        return Err(PatchError::DanglingPixelRef {
                            zone: zone.id,
                            fixture: member.fixture,
                            pixel: member.pixel,
                        })
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }
}
