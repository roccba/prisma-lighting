//! Node monitor: the single owner of UDP 6454 for discovery — periodic `ArtPoll`,
//! per-node "responde / sin respuesta / dejó de responder", and the Scan on demand
//! (docs/DEVICES-SPEC.md §7–§8).
//!
//! Replies to an `ArtPoll` arrive on port 6454, and only one socket can usefully
//! listen there. So the monitor and the Scan are one service: a low-priority thread
//! (never the output thread, which sends from its own ephemeral socket) polls every
//! [`POLL_PERIOD`] and records who answered; [`NodeMonitor::scan`] just polls now and
//! collects the replies that arrive in its window.
//!
//! Status (§8): a node that answered within [`ONLINE_WINDOW`] is **online**; one that
//! answered before and then went quiet is **lost** — the on-site alarm (a hung board,
//! an unplugged cable); one that never answered is **silent**, shown neutral because
//! some devices simply do not implement ArtPoll.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

use crate::discovery::{merge, DiscoveredNode, ScanError};
use crate::netif::host_interfaces;
use crate::packet::artpoll::{parse_artpollreply, write_artpoll, PollReply};
use crate::route::ARTNET_PORT;

/// How often the monitor polls.
pub const POLL_PERIOD: Duration = Duration::from_secs(3);
/// A node is online if it answered within this long (three poll periods + slack).
pub const ONLINE_WINDOW: Duration = Duration::from_secs(10);
/// Replies kept for a Scan to collect (longer than any scan window).
const REPLY_HISTORY: Duration = Duration::from_secs(15);

/// A watched node's reachability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeStatus {
    /// Never answered since the monitor started (neutral: some devices do not reply).
    Silent,
    /// Answered within [`ONLINE_WINDOW`].
    Online,
    /// Answered before, quiet for longer than [`ONLINE_WINDOW`] (alarm).
    Lost,
}

/// Called from the monitor thread with every watched node whose status changed.
pub type StatusCallback = Box<dyn Fn(&[(Ipv4Addr, NodeStatus)]) + Send>;

#[derive(Default)]
struct State {
    last_reply: HashMap<Ipv4Addr, Instant>,
    history: Vec<(Instant, PollReply, Vec<u8>)>,
    watched: Vec<Ipv4Addr>,
    reported: HashMap<Ipv4Addr, NodeStatus>,
}

impl State {
    fn status(&self, ip: Ipv4Addr, now: Instant) -> NodeStatus {
        match self.last_reply.get(&ip) {
            None => NodeStatus::Silent,
            Some(&t) if now.duration_since(t) <= ONLINE_WINDOW => NodeStatus::Online,
            Some(_) => NodeStatus::Lost,
        }
    }
}

/// The running monitor. Dropping it stops and joins the thread.
pub struct NodeMonitor {
    state: Arc<Mutex<State>>,
    socket: UdpSocket,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    port: u16,
}

impl NodeMonitor {
    /// Start on the Art-Net port. `PortBusy` if another program holds 6454.
    pub fn start(on_change: StatusCallback) -> Result<NodeMonitor, ScanError> {
        Self::start_on(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), ARTNET_PORT),
            ARTNET_PORT,
            on_change,
        )
    }

    /// Start listening on `listen`, polling targets on `poll_port` (tests use loopback
    /// ports; production is 6454 for both).
    pub fn start_on(
        listen: SocketAddr,
        poll_port: u16,
        on_change: StatusCallback,
    ) -> Result<NodeMonitor, ScanError> {
        let socket = open(listen)?;
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let socket = socket.try_clone().map_err(ScanError::Io)?;
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("prisma-artnet-monitor".into())
                .spawn(move || run(socket, state, stop, poll_port, on_change))
                .map_err(ScanError::Io)?
        };
        Ok(NodeMonitor {
            state,
            socket,
            stop,
            thread: Some(thread),
            port: poll_port,
        })
    }

    /// The node IPs whose status is tracked and reported (the Show's nodes). They are
    /// also polled directly, in case a broadcast does not reach them.
    pub fn set_watched(&self, ips: Vec<Ipv4Addr>) {
        let mut s = self.state.lock().unwrap();
        s.reported.retain(|ip, _| ips.contains(ip));
        s.watched = ips;
    }

    /// Current status of `ip`.
    pub fn status(&self, ip: Ipv4Addr) -> NodeStatus {
        self.state.lock().unwrap().status(ip, Instant::now())
    }

    /// Scan: poll now (and again halfway), wait `window`, return the nodes that
    /// answered during it — replies merged per board, like
    /// [`discovery::scan`](crate::discovery::scan). Blocks the caller for `window`.
    pub fn scan(&self, window: Duration) -> Vec<DiscoveredNode> {
        let start = Instant::now();
        let watched = self.state.lock().unwrap().watched.clone();
        send_polls(&self.socket, &watched, self.port);
        std::thread::sleep(window / 2);
        send_polls(&self.socket, &watched, self.port);
        std::thread::sleep(window - window / 2);
        let s = self.state.lock().unwrap();
        merge(
            s.history
                .iter()
                .filter(|(t, _, _)| *t >= start)
                .map(|(_, r, raw)| (r.clone(), raw.clone()))
                .collect(),
        )
    }
}

impl Drop for NodeMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn open(listen: SocketAddr) -> Result<UdpSocket, ScanError> {
    let socket =
        Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(ScanError::Io)?;
    socket.set_broadcast(true).map_err(ScanError::Io)?;
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(ScanError::Io)?;
    socket.bind(&listen.into()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            ScanError::PortBusy(e)
        } else {
            ScanError::Io(e)
        }
    })?;
    Ok(socket.into())
}

/// `ArtPoll` to every host subnet broadcast, the limited broadcast, and each watched
/// node directly. Host interfaces are re-read every time, so a cable plugged in after
/// start-up is picked up on the next poll.
fn send_polls(socket: &UdpSocket, watched: &[Ipv4Addr], port: u16) {
    let mut poll = Vec::new();
    write_artpoll(&mut poll);
    let mut targets: Vec<Ipv4Addr> = host_interfaces().iter().map(|h| h.broadcast()).collect();
    targets.push(Ipv4Addr::BROADCAST);
    targets.extend_from_slice(watched);
    targets.sort_unstable();
    targets.dedup();
    for t in targets {
        let _ = socket.send_to(&poll, SocketAddr::new(IpAddr::V4(t), port));
    }
}

fn run(
    socket: UdpSocket,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    port: u16,
    on_change: StatusCallback,
) {
    let mut next_poll = Instant::now();
    // Status is re-evaluated at most this often. The socket also receives non-reply
    // Art-Net (when output goes to the loopback virtual sink, its ArtDmx lands on this
    // 0.0.0.0:6454 socket too — thousands of packets/s at scale), so per-packet work
    // must stay a header check.
    const EVAL_EVERY: Duration = Duration::from_millis(250);
    let mut next_eval = Instant::now();
    let mut buf = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_poll {
            let watched = state.lock().unwrap().watched.clone();
            send_polls(&socket, &watched, port);
            next_poll = now + POLL_PERIOD;
        }
        // Blocks ≤ the read timeout, which also paces the status re-evaluation.
        if let Ok((n, _)) = socket.recv_from(&mut buf) {
            if let Some(reply) = parse_artpollreply(&buf[..n]) {
                let at = Instant::now();
                let mut s = state.lock().unwrap();
                s.last_reply.insert(Ipv4Addr::from(reply.ip), at);
                s.history.push((at, reply, buf[..n].to_vec()));
            }
        }
        if Instant::now() < next_eval {
            continue;
        }
        next_eval = Instant::now() + EVAL_EVERY;
        let changed: Vec<(Ipv4Addr, NodeStatus)> = {
            let mut s = state.lock().unwrap();
            let now = Instant::now();
            s.history
                .retain(|(t, _, _)| now.duration_since(*t) <= REPLY_HISTORY);
            let current: BTreeMap<Ipv4Addr, NodeStatus> = s
                .watched
                .iter()
                .map(|&ip| (ip, s.status(ip, now)))
                .collect();
            let mut changed = Vec::new();
            for (ip, st) in current {
                if s.reported.get(&ip) != Some(&st) {
                    s.reported.insert(ip, st);
                    changed.push((ip, st));
                }
            }
            changed
        };
        if !changed.is_empty() {
            on_change(&changed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::artpoll::build_artpollreply;
    use std::sync::mpsc;

    /// A fake board on loopback answering every ArtPoll with one reply naming `ip`.
    fn fake_node(ip: [u8; 4], alive: Arc<AtomicBool>) -> (SocketAddr, JoinHandle<()>) {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let addr = sock.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if let Ok((_, from)) = sock.recv_from(&mut buf) {
                    if alive.load(Ordering::Relaxed) {
                        let raw = build_artpollreply(ip, "Fake", "Fake", [1; 6], 0, 0, &[0, 1], 1);
                        let _ = sock.send_to(&raw, from);
                    }
                }
            }
        });
        (addr, h)
    }

    #[test]
    fn a_watched_node_goes_silent_then_online_and_scan_sees_it() {
        let alive = Arc::new(AtomicBool::new(true));
        let (node_addr, _h) = fake_node([127, 0, 0, 1], Arc::clone(&alive));
        let (tx, rx) = mpsc::channel();
        let monitor = NodeMonitor::start_on(
            "127.0.0.1:0".parse().unwrap(),
            node_addr.port(),
            Box::new(move |c| {
                let _ = tx.send(c.to_vec());
            }),
        )
        .unwrap();
        let ip = Ipv4Addr::LOCALHOST;
        monitor.set_watched(vec![ip]);

        // First report: whatever it is at watch time; then it must reach Online.
        let mut saw_online = false;
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline && !saw_online {
            if let Ok(c) = rx.recv_timeout(Duration::from_millis(200)) {
                saw_online = c.contains(&(ip, NodeStatus::Online));
            }
        }
        assert!(saw_online, "watched node never reported online");
        assert_eq!(monitor.status(ip), NodeStatus::Online);

        let found = monitor.scan(Duration::from_millis(400));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].universes, [0, 1]);
    }

    #[test]
    fn status_classification() {
        let mut s = State::default();
        let now = Instant::now();
        let ip = Ipv4Addr::new(10, 0, 0, 1);
        assert_eq!(s.status(ip, now), NodeStatus::Silent);
        s.last_reply.insert(ip, now);
        assert_eq!(
            s.status(ip, now + Duration::from_secs(2)),
            NodeStatus::Online
        );
        assert_eq!(
            s.status(ip, now + ONLINE_WINDOW + Duration::from_secs(1)),
            NodeStatus::Lost
        );
    }

    #[test]
    fn a_held_port_is_busy() {
        let holder = UdpSocket::bind("127.0.0.1:0").unwrap();
        let r = NodeMonitor::start_on(holder.local_addr().unwrap(), 1, Box::new(|_| {}));
        assert!(matches!(r, Err(ScanError::PortBusy(_))));
    }
}
