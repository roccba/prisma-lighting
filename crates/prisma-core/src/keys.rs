//! Keyboard shortcuts (docs/LIVE-MODEL-SPEC.md §6, §8.2).
//!
//! The keyboard is one more transport onto the Parameter Tree: a shortcut names a
//! **tree path** and pressing it activates that path (fires a trigger, flips a toggle),
//! exactly like a MIDI pad or a click. Grid navigation and "launch / arm the selected
//! clip" are paths too (`/select/move/*`, `/select/launch`, `/select/arm`), so a
//! controller can learn them as well. The few actions that only change the window (show
//! or hide the parameter panel) are `ui:…` names instead: no tree value behind them, the
//! UI performs them ([`is_ui_action`]).
//!
//! Shortcuts follow the operator, not the show, so they are stored in the
//! [`ControllerMappings`](crate::ControllerMappings) profile.
//!
//! # One decision
//!
//! Whether a key press does anything is decided in **one** function, [`dispatch`], from
//! the key combo and what kind of element has the focus — never scattered across UI
//! handlers. The rule it enforces: **no shortcut fires while the focus is in a text or
//! numeric field** (typing a cell name with a space in it must not fire GO), and the
//! menu's function keys are never taken by a shortcut.

use serde::{Deserialize, Serialize};

/// A key with its modifiers. `key` is the key's name as the UI normalizes it: a single
/// letter upper-cased (`"B"`), a digit by its position on the main row (`"1"`, whatever
/// Shift makes of it), `"Space"`, or the DOM name of a named key (`"Escape"`,
/// `"Enter"`, `"Backspace"`, `"ArrowUp"`, `"F5"`…).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyCombo {
    pub key: String,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

impl KeyCombo {
    /// A key without modifiers.
    pub fn plain(key: &str) -> KeyCombo {
        KeyCombo {
            key: key.into(),
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    pub fn with_ctrl(mut self) -> KeyCombo {
        self.ctrl = true;
        self
    }

    pub fn with_shift(mut self) -> KeyCombo {
        self.shift = true;
        self
    }

    pub fn with_alt(mut self) -> KeyCombo {
        self.alt = true;
        self
    }

    /// The menu keeps these (F3 editor, F4 devices, F7–F10 tools, F11 full screen, and
    /// the Ctrl+N/O/S/M file and learn shortcuts): a shortcut can never take them.
    pub fn is_reserved(&self) -> bool {
        let f = matches!(
            self.key.as_str(),
            "F3" | "F4" | "F7" | "F8" | "F9" | "F10" | "F11"
        );
        let menu = self.ctrl
            && !self.alt
            && !self.shift
            && matches!(self.key.as_str(), "N" | "O" | "S" | "M");
        f || menu
    }
}

/// One shortcut: a combo and the tree path it activates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub combo: KeyCombo,
    /// The tree path pressing the combo activates (a trigger fires, a toggle flips).
    pub path: String,
}

/// What has the keyboard focus when a key is pressed, as the UI classifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusKind {
    /// Nothing editable: the window, a button, a grid cell.
    Other,
    /// A text input, a textarea or anything content-editable.
    Text,
    /// A numeric field (an input the operator types a number into).
    Number,
}

/// Paths of the actions a shortcut can drive that are not plain show paths.
pub mod paths {
    pub const GO: &str = "/arm/go";
    pub const DISARM: &str = "/arm/clear";
    pub const STOP_ALL: &str = "/tracks/stop";
    pub const BLACKOUT: &str = "/master/blackout";
    pub const MOVE_UP: &str = "/select/move/up";
    pub const MOVE_DOWN: &str = "/select/move/down";
    pub const MOVE_LEFT: &str = "/select/move/left";
    pub const MOVE_RIGHT: &str = "/select/move/right";
    pub const LAUNCH_SELECTED: &str = "/select/launch";
    pub const ARM_SELECTED: &str = "/select/arm";
    /// Window action: show / hide the parameter panel.
    pub const TOGGLE_PANEL: &str = "ui:panel";

    /// `/scene/{row}/launch`.
    pub fn scene(row: usize) -> String {
        format!("/scene/{row}/launch")
    }

    /// Window action: show the bottom panel's tab `n` (1-based; LIVE-MODEL-SPEC §8.5
    /// step 4). A number with no tab does nothing.
    pub fn panel_tab(n: usize) -> String {
        format!("ui:tab/{n}")
    }
}

/// Whether a shortcut's action is a window action the UI performs (not a tree path).
pub fn is_ui_action(path: &str) -> bool {
    path.starts_with("ui:")
}

/// How many bottom-panel tabs get a default key (Alt+1…Alt+5).
pub const PANEL_TAB_KEYS: usize = 5;

/// How many scenes get a default number key (1…9).
pub const DEFAULT_SCENE_KEYS: usize = 9;

/// The default shortcuts (LIVE-MODEL-SPEC §8.2): Space GO, Esc disarm, Ctrl+Backspace
/// stop all, Shift+B blackout, 1…9 scenes, arrows move the selection, Enter launches
/// and Shift+Enter arms the selected clip, Tab shows / hides the parameter panel, Alt+1…5
/// pick the bottom panel's tab.
pub fn default_bindings() -> Vec<KeyBinding> {
    let bind = |combo: KeyCombo, path: &str| KeyBinding {
        combo,
        path: path.into(),
    };
    let mut keys = vec![
        bind(KeyCombo::plain("Space"), paths::GO),
        bind(KeyCombo::plain("Escape"), paths::DISARM),
        bind(KeyCombo::plain("Backspace").with_ctrl(), paths::STOP_ALL),
        bind(KeyCombo::plain("B").with_shift(), paths::BLACKOUT),
    ];
    for row in 0..DEFAULT_SCENE_KEYS {
        keys.push(bind(
            KeyCombo::plain(&(row + 1).to_string()),
            &paths::scene(row),
        ));
    }
    keys.extend([
        bind(KeyCombo::plain("ArrowUp"), paths::MOVE_UP),
        bind(KeyCombo::plain("ArrowDown"), paths::MOVE_DOWN),
        bind(KeyCombo::plain("ArrowLeft"), paths::MOVE_LEFT),
        bind(KeyCombo::plain("ArrowRight"), paths::MOVE_RIGHT),
        bind(KeyCombo::plain("Enter"), paths::LAUNCH_SELECTED),
        bind(KeyCombo::plain("Enter").with_shift(), paths::ARM_SELECTED),
        bind(KeyCombo::plain("Tab"), paths::TOGGLE_PANEL),
    ]);
    for n in 1..=PANEL_TAB_KEYS {
        keys.push(bind(
            KeyCombo::plain(&n.to_string()).with_alt(),
            &paths::panel_tab(n),
        ));
    }
    keys
}

/// Give the shortcut actions this build added since the profile was written their
/// default key — Tab for the panel, say — without touching anything the operator chose.
///
/// `known` lists every action the profile's shortcuts have ever covered (bound, rebound
/// or deliberately cleared). A default whose action is not in it is new: it is added if
/// its combo is free (a combo the operator already uses is never taken over). Then every
/// default action becomes known, so clearing one later sticks. Returns how many were
/// added.
pub fn merge_new_defaults(bindings: &mut Vec<KeyBinding>, known: &mut Vec<String>) -> usize {
    let mut added = 0;
    for default in default_bindings() {
        let new =
            !known.contains(&default.path) && !bindings.iter().any(|b| b.path == default.path);
        let free = !bindings.iter().any(|b| b.combo == default.combo);
        if new && free {
            bindings.push(default.clone());
            added += 1;
        }
        if !known.contains(&default.path) {
            known.push(default.path);
        }
    }
    for b in bindings.iter() {
        if !known.contains(&b.path) {
            known.push(b.path.clone());
        }
    }
    added
}

/// Every default action's path (what a fresh profile already "knows").
pub fn default_actions() -> Vec<String> {
    default_bindings().into_iter().map(|b| b.path).collect()
}

/// The path a key press activates, if any — **the** keyboard decision.
///
/// `None` when the focus is in a text or numeric field (whatever the combo), when the
/// combo belongs to the menu, or when nothing is bound to it.
pub fn dispatch<'a>(
    bindings: &'a [KeyBinding],
    combo: &KeyCombo,
    focus: FocusKind,
) -> Option<&'a str> {
    if matches!(focus, FocusKind::Text | FocusKind::Number) || combo.is_reserved() {
        return None;
    }
    bindings
        .iter()
        .find(|b| &b.combo == combo)
        .map(|b| b.path.as_str())
}

/// Bind `combo` to `path`, replacing whatever used that combo before; an empty combo
/// list entry for `path` is removed when `combo` is `None` (the action has no key).
/// Refuses a reserved combo (returns `false`).
pub fn rebind(bindings: &mut Vec<KeyBinding>, path: &str, combo: Option<KeyCombo>) -> bool {
    if combo.as_ref().is_some_and(KeyCombo::is_reserved) {
        return false;
    }
    bindings.retain(|b| b.path != path && Some(&b.combo) != combo.as_ref());
    if let Some(combo) = combo {
        bindings.push(KeyBinding {
            combo,
            path: path.into(),
        });
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_the_spec_table() {
        let keys = default_bindings();
        let at = |c: KeyCombo| dispatch(&keys, &c, FocusKind::Other);
        assert_eq!(at(KeyCombo::plain("Space")), Some(paths::GO));
        assert_eq!(at(KeyCombo::plain("Escape")), Some(paths::DISARM));
        assert_eq!(
            at(KeyCombo::plain("Backspace").with_ctrl()),
            Some(paths::STOP_ALL)
        );
        assert_eq!(at(KeyCombo::plain("B").with_shift()), Some(paths::BLACKOUT));
        // Plain B is not blackout: Shift is part of the combo.
        assert_eq!(at(KeyCombo::plain("B")), None);
        assert_eq!(at(KeyCombo::plain("1")), Some("/scene/0/launch"));
        assert_eq!(at(KeyCombo::plain("9")), Some("/scene/8/launch"));
        // Ctrl+1 is not a scene: scenes have no modifier.
        assert_eq!(at(KeyCombo::plain("1").with_ctrl()), None);
        assert_eq!(at(KeyCombo::plain("ArrowLeft")), Some(paths::MOVE_LEFT));
        assert_eq!(at(KeyCombo::plain("Enter")), Some(paths::LAUNCH_SELECTED));
        assert_eq!(
            at(KeyCombo::plain("Enter").with_shift()),
            Some(paths::ARM_SELECTED)
        );
        assert_eq!(at(KeyCombo::plain("Tab")), Some(paths::TOGGLE_PANEL));
        assert!(is_ui_action(paths::TOGGLE_PANEL));
        assert!(!is_ui_action(paths::GO));
    }

    #[test]
    fn no_shortcut_fires_with_focus_in_a_text_or_numeric_field() {
        let keys = default_bindings();
        for combo in keys.iter().map(|b| b.combo.clone()) {
            assert!(dispatch(&keys, &combo, FocusKind::Other).is_some());
            assert_eq!(dispatch(&keys, &combo, FocusKind::Text), None, "{combo:?}");
            assert_eq!(
                dispatch(&keys, &combo, FocusKind::Number),
                None,
                "{combo:?}"
            );
        }
    }

    #[test]
    fn menu_keys_are_never_shortcuts() {
        // Even if a file binds them, the menu keeps them.
        let keys = vec![
            KeyBinding {
                combo: KeyCombo::plain("F3"),
                path: paths::GO.into(),
            },
            KeyBinding {
                combo: KeyCombo::plain("S").with_ctrl(),
                path: paths::GO.into(),
            },
        ];
        assert_eq!(
            dispatch(&keys, &KeyCombo::plain("F3"), FocusKind::Other),
            None
        );
        assert_eq!(
            dispatch(&keys, &KeyCombo::plain("S").with_ctrl(), FocusKind::Other),
            None
        );
        let mut keys = default_bindings();
        assert!(!rebind(&mut keys, paths::GO, Some(KeyCombo::plain("F11"))));
        assert_eq!(keys, default_bindings());
    }

    #[test]
    fn rebind_moves_a_combo_and_clears_an_action() {
        let mut keys = default_bindings();
        // GO onto G: Space no longer fires anything.
        assert!(rebind(&mut keys, paths::GO, Some(KeyCombo::plain("G"))));
        assert_eq!(
            dispatch(&keys, &KeyCombo::plain("G"), FocusKind::Other),
            Some(paths::GO)
        );
        assert_eq!(
            dispatch(&keys, &KeyCombo::plain("Space"), FocusKind::Other),
            None
        );
        // Give "1" to stop all: scene 1 loses it.
        assert!(rebind(
            &mut keys,
            paths::STOP_ALL,
            Some(KeyCombo::plain("1"))
        ));
        assert_eq!(
            dispatch(&keys, &KeyCombo::plain("1"), FocusKind::Other),
            Some(paths::STOP_ALL)
        );
        assert!(!keys.iter().any(|b| b.path == "/scene/0/launch"));
        // No key for blackout.
        assert!(rebind(&mut keys, paths::BLACKOUT, None));
        assert!(!keys.iter().any(|b| b.path == paths::BLACKOUT));
    }

    #[test]
    fn new_default_actions_get_their_key_without_touching_the_operators() {
        // A profile written before the panel shortcut existed: the operator moved GO to G
        // and cleared blackout (both known actions), and uses Tab for nothing.
        let mut keys = default_bindings();
        keys.retain(|b| b.path != paths::TOGGLE_PANEL);
        let mut known: Vec<String> = keys.iter().map(|b| b.path.clone()).collect();
        assert!(rebind(&mut keys, paths::GO, Some(KeyCombo::plain("G"))));
        assert!(rebind(&mut keys, paths::BLACKOUT, None));
        known.push(paths::BLACKOUT.into());

        assert_eq!(merge_new_defaults(&mut keys, &mut known), 1);
        let at = |c: KeyCombo, keys: &[KeyBinding]| {
            dispatch(keys, &c, FocusKind::Other).map(str::to_owned)
        };
        assert_eq!(
            at(KeyCombo::plain("Tab"), &keys).as_deref(),
            Some(paths::TOGGLE_PANEL)
        );
        // The operator's choices stand: GO stays on G, Space is not given back, blackout
        // stays without a key.
        assert_eq!(at(KeyCombo::plain("G"), &keys).as_deref(), Some(paths::GO));
        assert_eq!(at(KeyCombo::plain("Space"), &keys), None);
        assert!(!keys.iter().any(|b| b.path == paths::BLACKOUT));
        // Nothing more on the next load; clearing the new action now sticks too.
        assert_eq!(merge_new_defaults(&mut keys, &mut known), 0);
        assert!(rebind(&mut keys, paths::TOGGLE_PANEL, None));
        assert_eq!(merge_new_defaults(&mut keys, &mut known), 0);
        assert!(!keys.iter().any(|b| b.path == paths::TOGGLE_PANEL));
    }

    #[test]
    fn a_new_default_never_takes_a_combo_in_use() {
        let mut keys = vec![KeyBinding {
            combo: KeyCombo::plain("Tab"),
            path: paths::GO.into(),
        }];
        let mut known = vec![paths::GO.to_string()];
        merge_new_defaults(&mut keys, &mut known);
        assert_eq!(
            dispatch(&keys, &KeyCombo::plain("Tab"), FocusKind::Other),
            Some(paths::GO)
        );
        assert!(!keys.iter().any(|b| b.path == paths::TOGGLE_PANEL));
    }

    #[test]
    fn bindings_roundtrip_as_json() {
        let keys = default_bindings();
        let json = serde_json::to_string(&keys).unwrap();
        let back: Vec<KeyBinding> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, keys);
    }
}
