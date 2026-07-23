//! Virtual sink: a software Art-Net receiver that stands in for a physical rig
//! (CLAUDE.md §5.1, M1). It lets output be validated end-to-end without
//! hardware, and provides an **independent, receive-side** view of both packet
//! content and timing — a second vantage point on the sender's probe.
//!
//! What it validates:
//!   - **Serialization over a real socket**: packets decode with the same codec
//!     ([`crate::packet::artnet`]), so header/length/channel packing is correct
//!     round-trip, not just in a byte-level unit test.
//!   - **Multi-universe routing**: per-universe latest state, keyed by port
//!     address.
//!   - **Cadence**: receive-side inter-packet intervals, cross-checked against
//!     the sender's probe.
//!   - **Sequence continuity**: Art-Net sequence gaps per universe.
//!
//! Art-Net only for M1 (matching the Art-Net-first output slice); an sACN sink
//! reuses this structure.

use std::collections::BTreeMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::packet::artnet::{opcode_of, parse_artdmx, ArtNetOpcode};

/// Latest received state of one universe, plus timing and sequence bookkeeping.
#[derive(Debug, Default, Clone)]
pub struct UniverseState {
    /// Most recent channel data received for this universe.
    pub data: Vec<u8>,
    /// ArtDmx packets received for this universe.
    pub packets: u64,
    /// Last Art-Net sequence value seen (`0` = sequencing disabled).
    pub last_sequence: Option<u8>,
    /// Number of times the sequence advanced by more than one (lost/reordered).
    pub sequence_gaps: u64,
    /// Inter-packet intervals (ns), receive-side.
    pub intervals_ns: Vec<u64>,
    last_recv: Option<Instant>,
}

impl UniverseState {
    fn record(&mut self, sequence: u8, data: &[u8], now: Instant) {
        // Gap detection: Art-Net sequence is 1..=255 rolling; 0 disables it.
        if let Some(prev) = self.last_sequence {
            if prev != 0 && sequence != 0 {
                let expected = if prev == 255 { 1 } else { prev + 1 };
                if sequence != expected {
                    self.sequence_gaps += 1;
                }
            }
        }
        self.last_sequence = Some(sequence);

        self.data.clear();
        self.data.extend_from_slice(data);
        self.packets += 1;

        if let Some(t) = self.last_recv {
            self.intervals_ns
                .push(now.duration_since(t).as_nanos() as u64);
        }
        self.last_recv = Some(now);
    }

    /// Mean receive-side inter-packet interval, if at least two packets arrived.
    pub fn mean_interval(&self) -> Option<Duration> {
        if self.intervals_ns.is_empty() {
            return None;
        }
        let sum: u128 = self.intervals_ns.iter().map(|&x| x as u128).sum();
        Some(Duration::from_nanos(
            (sum / self.intervals_ns.len() as u128) as u64,
        ))
    }

    /// Largest receive-side inter-packet interval.
    pub fn max_interval(&self) -> Option<Duration> {
        self.intervals_ns
            .iter()
            .copied()
            .max()
            .map(Duration::from_nanos)
    }
}

/// Aggregate state of everything the sink has received.
#[derive(Debug, Default, Clone)]
pub struct SinkState {
    /// Total datagrams received (Art-Net or not).
    pub datagrams: u64,
    /// ArtDmx packets received (well-formed).
    pub artdmx_packets: u64,
    /// ArtSync packets received.
    pub artsync_packets: u64,
    /// Datagrams that were not recognized Art-Net.
    pub non_artnet: u64,
    /// Datagrams that claimed to be ArtDmx but failed to parse.
    pub malformed: u64,
    /// Per-universe state, keyed by 15-bit port address.
    pub universes: BTreeMap<u16, UniverseState>,
}

impl SinkState {
    fn ingest(&mut self, datagram: &[u8], now: Instant) {
        self.datagrams += 1;
        match opcode_of(datagram) {
            Some(ArtNetOpcode::Dmx) => match parse_artdmx(datagram) {
                Some(pkt) => {
                    self.artdmx_packets += 1;
                    self.universes.entry(pkt.port_address).or_default().record(
                        pkt.sequence,
                        pkt.data,
                        now,
                    );
                }
                None => self.malformed += 1,
            },
            Some(ArtNetOpcode::Sync) => self.artsync_packets += 1,
            Some(ArtNetOpcode::Other(_)) => {}
            None => self.non_artnet += 1,
        }
    }
}

/// A bound virtual sink, driven synchronously. Useful for unit tests and for
/// the threaded [`spawn`] wrapper.
pub struct VirtualSink {
    socket: UdpSocket,
    state: SinkState,
}

impl VirtualSink {
    /// Bind a sink to `addr` (use port 0 for an ephemeral port). The read
    /// timeout lets a receive loop check a stop flag between packets.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_read_timeout(Some(Duration::from_millis(20)))?;
        Ok(Self {
            socket,
            state: SinkState::default(),
        })
    }

    /// The address the sink is bound to (resolves an ephemeral port).
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Receive and process one datagram if one is available before the read
    /// timeout. Returns `true` if a datagram was processed.
    pub fn poll_once(&mut self, buf: &mut [u8]) -> bool {
        match self.socket.recv_from(buf) {
            Ok((n, _)) => {
                self.state.ingest(&buf[..n], Instant::now());
                true
            }
            Err(_) => false, // timeout / would-block
        }
    }

    /// Process a raw datagram directly (no socket) — for tests.
    pub fn ingest(&mut self, datagram: &[u8]) {
        self.state.ingest(datagram, Instant::now());
    }

    /// The accumulated state.
    pub fn state(&self) -> &SinkState {
        &self.state
    }
}

/// Handle to a sink running on its own thread.
pub struct SinkHandle {
    stop: Arc<AtomicBool>,
    local_addr: SocketAddr,
    /// Republished by the receive thread after every datagram, so a test can
    /// observe the wire at several points during ONE continuous run instead of
    /// having to tear the sink down to look at it.
    shared: Arc<Mutex<SinkState>>,
    thread: JoinHandle<SinkState>,
}

impl SinkHandle {
    /// The address the sink is receiving on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// What the sink has received so far, without stopping it.
    pub fn snapshot(&self) -> SinkState {
        self.shared.lock().expect("sink state mutex").clone()
    }

    /// Stop the receive loop and return the final accumulated state, draining
    /// any datagrams already buffered by the kernel.
    pub fn stop_and_collect(self) -> SinkState {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().expect("sink thread panicked")
    }
}

/// Bind a sink to `bind` and run its receive loop on a background thread.
pub fn spawn(bind: SocketAddr) -> io::Result<SinkHandle> {
    let mut sink = VirtualSink::bind(bind)?;
    let local_addr = sink.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let shared = Arc::new(Mutex::new(SinkState::default()));
    let shared_thread = Arc::clone(&shared);

    let thread = thread::spawn(move || {
        let mut buf = [0u8; 2048];
        while !stop_thread.load(Ordering::Relaxed) {
            if sink.poll_once(&mut buf) {
                *shared_thread.lock().expect("sink state mutex") = sink.state.clone();
            }
        }
        // Drain whatever the kernel already buffered before we were told to stop
        // so a final burst is not lost from the measurement.
        while sink.poll_once(&mut buf) {}
        *shared_thread.lock().expect("sink state mutex") = sink.state.clone();
        sink.state
    });

    Ok(SinkHandle {
        stop,
        local_addr,
        shared,
        thread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::artnet::{write_artsync, ArtDmx};
    use std::net::{IpAddr, Ipv4Addr};

    fn artdmx(port: u16, sequence: u8, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        ArtDmx {
            sequence,
            physical: 0,
            port_address: port,
            data,
        }
        .write_to(&mut buf);
        buf
    }

    #[test]
    fn ingest_records_universe_data() {
        let mut sink =
            VirtualSink::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
        sink.ingest(&artdmx(1, 1, &[10, 20, 30, 40]));

        let u = sink.state().universes.get(&1).unwrap();
        assert_eq!(u.data, &[10, 20, 30, 40]);
        assert_eq!(u.packets, 1);
        assert_eq!(sink.state().artdmx_packets, 1);
    }

    #[test]
    fn detects_sequence_gaps() {
        let mut sink =
            VirtualSink::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
        sink.ingest(&artdmx(1, 1, &[0, 0]));
        sink.ingest(&artdmx(1, 2, &[0, 0])); // in order
        sink.ingest(&artdmx(1, 5, &[0, 0])); // jumped 2->5: one gap
        assert_eq!(sink.state().universes.get(&1).unwrap().sequence_gaps, 1);
    }

    #[test]
    fn classifies_sync_and_foreign_datagrams() {
        let mut sink =
            VirtualSink::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
        let mut sync = Vec::new();
        write_artsync(&mut sync);
        sink.ingest(&sync);
        sink.ingest(b"totally not art-net");

        assert_eq!(sink.state().artsync_packets, 1);
        assert_eq!(sink.state().non_artnet, 1);
    }

    #[test]
    fn threaded_sink_receives_over_the_socket() {
        let handle = spawn(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
        let tx = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();

        for seq in 1..=3u8 {
            tx.send_to(&artdmx(2, seq, &[seq, seq]), handle.local_addr())
                .unwrap();
        }
        // Give the receive loop a moment to drain (loopback is fast; drain on
        // stop also catches buffered packets).
        thread::sleep(Duration::from_millis(50));

        let state = handle.stop_and_collect();
        let u = state.universes.get(&2).expect("universe 2 received");
        assert_eq!(u.packets, 3);
        assert_eq!(u.data, &[3, 3]);
        assert_eq!(u.sequence_gaps, 0);
    }
}
