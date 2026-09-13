//! The real-hardware adapter over `midir` (M9-SPEC §5). Feature `hardware`.
//!
//! Deliberately thin: enumerate ports, open one input by name and forward each
//! message to [`MidiRuntime::feed`]; open one output and hand the runtime a
//! [`MidiOut`] for LED feedback. Everything that decides anything lives behind
//! that seam and is tested without this module. The feature exists because `midir`
//! links the platform MIDI stack (ALSA on Linux), which the headless workspace build
//! must not require; the desktop app enables it.

use std::sync::{Arc, Weak};

use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use parking_lot::Mutex;

use crate::feedback::MidiOut;

use crate::ports::find_port_by_keywords;
use crate::runtime::MidiRuntime;

const CLIENT_NAME: &str = "Prisma";

/// Why a port could not be opened.
#[derive(Debug)]
pub enum MidiIoError {
    /// The platform MIDI system could not be initialised (no ALSA sequencer, …).
    Init(String),
    /// No port with that name (or matching the keywords) is present.
    NotFound,
    Connect(String),
}

impl std::fmt::Display for MidiIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MidiIoError::Init(e) => write!(f, "MIDI system unavailable: {e}"),
            MidiIoError::NotFound => write!(f, "MIDI port not found"),
            MidiIoError::Connect(e) => write!(f, "could not open MIDI port: {e}"),
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

/// The names of every MIDI output port currently present.
pub fn output_port_names() -> Result<Vec<String>, MidiIoError> {
    let output = MidiOutput::new(CLIENT_NAME).map_err(|e| MidiIoError::Init(e.to_string()))?;
    Ok(output
        .ports()
        .iter()
        .filter_map(|p| output.port_name(p).ok())
        .collect())
}

/// An open output (the controller's LEDs). Dropping it closes the port; the
/// [`MidiOut`] it handed out then silently does nothing, so a runtime still holding
/// one can never keep a closed device open or write to it.
pub struct MidiOutputLink {
    port_name: String,
    conn: Arc<Mutex<MidiOutputConnection>>,
}

impl MidiOutputLink {
    /// Open the output named exactly `port_name`.
    pub fn open(port_name: &str) -> Result<Self, MidiIoError> {
        Self::open_where(|names| names.iter().position(|n| n == port_name))
    }

    /// Open the first output whose name matches any keyword.
    pub fn open_by_keywords(keywords: &[&str]) -> Result<Self, MidiIoError> {
        Self::open_where(|names| find_port_by_keywords(names, keywords))
    }

    fn open_where(pick: impl FnOnce(&[String]) -> Option<usize>) -> Result<Self, MidiIoError> {
        let output = MidiOutput::new(CLIENT_NAME).map_err(|e| MidiIoError::Init(e.to_string()))?;
        let ports = output.ports();
        let names: Vec<String> = ports
            .iter()
            .map(|p| output.port_name(p).unwrap_or_default())
            .collect();
        let index = pick(&names).ok_or(MidiIoError::NotFound)?;
        let port_name = names[index].clone();
        let conn = output
            .connect(&ports[index], "prisma-out")
            .map_err(|e| MidiIoError::Connect(e.to_string()))?;
        Ok(Self {
            port_name,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// The byte sink for [`MidiRuntime::set_output`]. A send error (device unplugged
    /// mid-show) is dropped: feedback is best-effort and must never stall input.
    pub fn sink(&self) -> MidiOut {
        let conn: Weak<Mutex<MidiOutputConnection>> = Arc::downgrade(&self.conn);
        Arc::new(move |bytes: &[u8]| {
            if let Some(conn) = conn.upgrade() {
                let _ = conn.lock().send(bytes);
            }
        })
    }
}

/// The app keeps the open link in `Mutex` managed state, which needs `Send`. Assert
/// it here, where the feature build compiles, not first on the desktop build.
const _: () = {
    fn assert_send<T: Send>() {}
    fn check() {
        assert_send::<MidiInputLink>();
        assert_send::<MidiOutputLink>();
    }
    let _ = check;
};
