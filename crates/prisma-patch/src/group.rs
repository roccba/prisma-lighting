//! Groups (LIVE-MODEL-SPEC §3.1): ordered sets of fixtures, saved in the patch next to
//! zones. A group can span zones. It is wiring, not performance: a track can use it as a
//! destination, a group master dims it at the output stage, and (phase 6) its member
//! order is the element order of chasers.

use crate::zone::{default_display_color, DisplayColor};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An ordered set of fixtures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    /// Fixture ids, in element order: the order they were selected in when the group
    /// was created, editable afterwards. No repeats.
    pub members: Vec<Uuid>,
    /// Colour identifying the group in the editor and on its master.
    #[serde(default = "default_display_color")]
    pub display_color: DisplayColor,
}

impl Group {
    /// Whether `fixture` is a member.
    pub fn contains(&self, fixture: Uuid) -> bool {
        self.members.contains(&fixture)
    }
}
