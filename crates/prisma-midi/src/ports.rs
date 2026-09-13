//! Port selection by keyword (M9-SPEC §5), ported from makai's
//! `findPortIndexByKeywords`: a device is found by a case-insensitive substring of
//! its port name, so it is located across OSes without hardcoding indices.

/// The APC40's default keywords (makai `apcKeywords`).
pub const APC40_KEYWORDS: &[&str] = &["apc", "akai"];

/// The index of the first port whose name contains any keyword (case-insensitive).
pub fn find_port_by_keywords<S: AsRef<str>>(names: &[S], keywords: &[&str]) -> Option<usize> {
    names.iter().position(|name| {
        let lowered = name.as_ref().to_lowercase();
        keywords.iter().any(|k| lowered.contains(&k.to_lowercase()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_apc_across_os_naming() {
        let linux = [
            "Midi Through:Midi Through Port-0 14:0",
            "APC40 mkII:APC40 mkII MIDI 1 24:0",
        ];
        assert_eq!(find_port_by_keywords(&linux, APC40_KEYWORDS), Some(1));
        let windows = ["Microsoft GS Wavetable Synth", "APC40 mkII"];
        assert_eq!(find_port_by_keywords(&windows, APC40_KEYWORDS), Some(1));
        let none = ["Midi Through Port-0"];
        assert_eq!(find_port_by_keywords(&none, APC40_KEYWORDS), None);
    }
}
