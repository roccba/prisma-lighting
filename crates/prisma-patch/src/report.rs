//! Editor-facing detection of the patch's **physical wire faults** (M7 / S6, replanned).
//!
//! [`Patch::validate`](crate::Patch::validate) is the fail-fast compile gate. The editor
//! needs the *complete* set of a specific, narrow class of problems: the ones that are
//! genuinely wrong on the wire, so it can mark every offending fixture at once rather
//! than surface only the first (M7 spec §1, §4.3).
//!
//! After the S6 replan there are exactly **two** such faults, and nothing else:
//!
//! - an **address collision** — two fixtures on the same universe+channel, so one
//!   mirrors the other or a tube goes dark;
//! - a **channel out of range** — a DMX span past 512, which no universe can carry
//!   (unreachable by dragging, but an *import* from CSV/script (§9) can bring one in).
//!
//! Everything the earlier S6 draft also reported — pixels outside the matrix, several
//! pixels sharing a cell — turned out **not** to be an error at all (§2.4, §4.4): a
//! matrix is a render *resolution*, not a fence, so those are valid placement choices
//! the editor does not flag. This module reports only the two wire faults, each
//! annotated with the pixels it points at so the canvas can mark them discreetly.

use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

use crate::patch::Patch;
use crate::CHANNELS_PER_UNIVERSE;

/// A single pixel a diagnostic points at: the owning fixture and the pixel's index
/// **within that fixture** (the same stable `(fixture, local index)` naming as
/// [`PixelRef`](crate::zone::PixelRef)). The editor resolves it to a canvas position
/// through the patch, so no coordinate is stored here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Mark {
    pub fixture: Uuid,
    pub pixel: u32,
}

/// A physical DMX wire fault the editor surfaces. Serialized as an internally-tagged
/// union (`{ "kind": "AddressCollision", … }`) so the TypeScript side is a discriminated
/// union keyed on `kind`. Both variants are also conditions
/// [`Patch::validate`](crate::Patch::validate) rejects — they block compilation — but
/// the editor shows them *discreetly* (a subtle notice + canvas marks), never as an alarm.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub enum Diagnostic {
    /// Two or more fixtures claim the same DMX channel(s) in one universe. Colliding
    /// channels that share the same set of fixtures are merged into one diagnostic
    /// spanning `channel_start..=channel_end`, so two fully-overlapping tubes read as
    /// one problem, not sixty. `marks` is every pixel involved, for the canvas.
    AddressCollision {
        universe: u16,
        channel_start: u16,
        channel_end: u16,
        fixtures: Vec<Uuid>,
        marks: Vec<Mark>,
    },
    /// A pixel's DMX span (`start .. start + span`) runs past channel 512 or starts
    /// below 1 — it cannot be addressed as written.
    ChannelOutOfRange { mark: Mark, start: u16, span: u16 },
}

impl Diagnostic {
    /// A concise English description. The editor builds its own richer text from
    /// fixture *names* (which it has and this crate does not); this is the fallback
    /// used in logs and tests.
    pub fn message(&self) -> String {
        match self {
            Diagnostic::AddressCollision {
                universe,
                channel_start,
                channel_end,
                fixtures,
                ..
            } => {
                let range = if channel_start == channel_end {
                    format!("channel {channel_start}")
                } else {
                    format!("channels {channel_start}–{channel_end}")
                };
                format!(
                    "universe {universe} {range} claimed by {} fixtures",
                    fixtures.len()
                )
            }
            Diagnostic::ChannelOutOfRange { start, span, .. } => format!(
                "DMX span {start}..{} is outside 1..=512",
                start.saturating_add(*span)
            ),
        }
    }
}

/// Key for merging colliding channels: one universe and the sorted set of fixtures that
/// clash there. Channels with the same fixture set collapse into one diagnostic.
type CollisionKey = (u16, Vec<Uuid>);

/// The accumulated extent of one merged collision: the channel range it spans and every
/// pixel involved (for the canvas overlay).
struct CollisionGroup {
    channel_lo: u16,
    channel_hi: u16,
    marks: Vec<Mark>,
}

impl Patch {
    /// Collect **every** physical wire fault in the patch for the editor (§1, §4.3):
    /// all address collisions and all out-of-range channels, each annotated with the
    /// pixels it points at. Returns them in a deterministic order (channel-range faults
    /// first in fixture order, then collisions by universe/channel) so the editor's
    /// notice and canvas marks are stable across calls.
    ///
    /// This is intentionally **narrow**: out-of-matrix positions and shared cells are
    /// valid choices (§2.4, §4.4) and are not reported here. It is not a general
    /// superset of `validate` — the other invariants `validate` enforces (dense pixel
    /// indices, zone-reference integrity, non-zero mapping sizes) are internal and not
    /// reachable through editor edits, so the editor does not surface them.
    pub fn wire_faults(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        self.collect_address_faults(&mut out);
        out
    }

    fn collect_address_faults(&self, out: &mut Vec<Diagnostic>) {
        // (universe, channel) -> every pixel that touches it. In-range channels only; a
        // pixel whose span is out of range is reported separately and skipped here so a
        // single malformed address does not also masquerade as a collision.
        let mut claimed: HashMap<(u16, u16), Vec<Mark>> = HashMap::new();
        for fixture in &self.fixtures {
            let span = fixture.profile.component_count() as u16;
            for pixel in &fixture.pixels {
                let mark = Mark {
                    fixture: fixture.id,
                    pixel: pixel.index,
                };
                let start = pixel.start_channel;
                let end = start.saturating_add(span); // exclusive
                if start < 1 || span == 0 || end > CHANNELS_PER_UNIVERSE as u16 + 1 {
                    out.push(Diagnostic::ChannelOutOfRange { mark, start, span });
                    continue;
                }
                for channel in start..end {
                    claimed
                        .entry((pixel.universe, channel))
                        .or_default()
                        .push(mark);
                }
            }
        }

        // Merge colliding channels that involve the same set of fixtures into one
        // diagnostic: two tubes overlapping across a whole span is one mistake, not one
        // per channel. Key = (universe, sorted distinct fixtures).
        let mut groups: HashMap<CollisionKey, CollisionGroup> = HashMap::new();
        for ((universe, channel), marks) in &claimed {
            let mut fixtures: Vec<Uuid> = marks.iter().map(|m| m.fixture).collect();
            fixtures.sort_unstable();
            fixtures.dedup();
            if fixtures.len() < 2 {
                continue;
            }
            let group = groups
                .entry((*universe, fixtures))
                .or_insert_with(|| CollisionGroup {
                    channel_lo: *channel,
                    channel_hi: *channel,
                    marks: Vec::new(),
                });
            group.channel_lo = group.channel_lo.min(*channel);
            group.channel_hi = group.channel_hi.max(*channel);
            for m in marks {
                if !group.marks.contains(m) {
                    group.marks.push(*m);
                }
            }
        }

        // Deterministic order: by universe, then first colliding channel, then fixtures.
        let mut groups: Vec<(CollisionKey, CollisionGroup)> = groups.into_iter().collect();
        groups.sort_by(|((ua, fa), ga), ((ub, fb), gb)| {
            ua.cmp(ub)
                .then(ga.channel_lo.cmp(&gb.channel_lo))
                .then(fa.cmp(fb))
        });
        for ((universe, fixtures), mut group) in groups {
            group.marks.sort_by_key(|m| (m.fixture, m.pixel));
            out.push(Diagnostic::AddressCollision {
                universe,
                channel_start: group.channel_lo,
                channel_end: group.channel_hi,
                fixtures,
                marks: group.marks,
            });
        }
    }
}
