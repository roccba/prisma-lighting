//! The real-hardware adapter over `midir` (M9-SPEC §5). Feature `hardware`.
//!
//! Deliberately thin: enumerate ports, open one input by name, and forward each
//! message to [`MidiRuntime::feed`]. Everything that decides anything lives behind
//! that seam and is tested without this module. The feature exists because `midir`
//! links the platform MIDI stack (ALSA on Linux), which the headless workspace build
//! must not require; the desktop app enables it.

use midir::{Ignore, MidiInput, MidiInputConnection};

use crate::ports::find_port_by_keywords;
use crate::runtime::MidiRuntime;

const CLIENT_NAME: &str = "Prisma";

/// Why a port could not be opened.
#[derive(Debug)]
pub enum MidiIoError {
    /// The platform MIDI system could not be initialised (no ALSA sequencer, …).
    Init(String),
    /// No input port with that name (or matching the keywords) is present.
    NotFound,
    Connect(String),
}

impl std::fmt::Display for MidiIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MidiIoError::Init(e) => write!(f, "MIDI system unavailable: {e}"),
            MidiIoError::NotFound => write!(f, "MIDI input port not found"),
            MidiIoError::Connect(e) => write!(f, "could not open MIDI input: {e}"),
        }
    }
}

impl std::error::Error for MidiIoError {}

/// The names of every MIDI input port currently present.
pub fn input_port_names() -> Result<Vec<String>, MidiIoError> {
    let input = MidiInput::new(CLIENT_NAME).map_err(|e| MidiIoError::Init(e.to_string()))?;
    Ok(input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .collect())
}

/// An open input feeding a runtime. Dropping it closes the port.
pub struct MidiInputLink {
    port_name: String,
    _conn: MidiInputConnection<()>,
}

impl MidiInputLink {
    /// Open the input named exactly `port_name`.
    pub fn open(runtime: MidiRuntime, port_name: &str) -> Result<Self, MidiIoError> {
        Self::open_where(runtime, |names| names.iter().position(|n| n == port_name))
    }

    /// Open the first input whose name matches any keyword (makai's auto-detect).
    pub fn open_by_keywords(runtime: MidiRuntime, keywords: &[&str]) -> Result<Self, MidiIoError> {
        Self::open_where(runtime, |names| find_port_by_keywords(names, keywords))
    }

    fn open_where(
        runtime: MidiRuntime,
        pick: impl FnOnce(&[String]) -> Option<usize>,
    ) -> Result<Self, MidiIoError> {
        let mut input =
            MidiInput::new(CLIENT_NAME).map_err(|e| MidiIoError::Init(e.to_string()))?;
        // Clock bytes are kept for Slice 5; sysex and active-sense are never used.
        input.ignore(Ignore::SysexAndActiveSense);
        let ports = input.ports();
        let names: Vec<String> = ports
            .iter()
            .map(|p| input.port_name(p).unwrap_or_default())
            .collect();
        let index = pick(&names).ok_or(MidiIoError::NotFound)?;
        let port_name = names[index].clone();
        let conn = input
            .connect(
                &ports[index],
                "prisma-in",
                move |_stamp, bytes, _| runtime.feed(bytes),
                (),
            )
            .map_err(|e| MidiIoError::Connect(e.to_string()))?;
        Ok(Self {
            port_name,
            _conn: conn,
        })
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

/// The app keeps the open link in `Mutex` managed state, which needs `Send`. Assert
/// it here, where the feature build compiles, not first on the desktop build.
const _: () = {
    fn assert_send<T: Send>() {}
    fn check() {
        assert_send::<MidiInputLink>();
    }
    let _ = check;
};
