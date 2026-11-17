//! Art-Net node discovery — the Device Manager's **Scan** (docs/DEVICES-SPEC.md §7).
//!
//! Sends `ArtPoll` and collects `ArtPollReply`s for a short window, then merges the
//! replies **per node**: a board with more than 4 ports answers once per group of 4
//! (`BindIndex`), and professional consoles lists each group as its own row — the H802RA shows up as
//! 18 rows. Prisma shows one row per board (Bauti, 2026-11-17), so replies sharing an
//! IP become one [`DiscoveredNode`] with all their universes.
//!
//! Never runs on the output thread: a scan blocks for its window and is called from a
//! command/worker thread.

use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use prisma_core::UniverseRange;
use socket2::{Domain, Protocol, Socket, Type};

use crate::packet::artpoll::{parse_artpollreply, write_artpoll, PollReply};
use crate::route::ARTNET_PORT;

/// One node found by a scan, its replies merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredNode {
    pub ip: Ipv4Addr,
    pub short_name: String,
    pub long_name: String,
    pub mac: Option<[u8; 6]>,
    /// Every output universe (raw port address) across all its replies, sorted, unique.
    pub universes: Vec<u16>,
    /// How many `BindIndex` groups answered (1 for a ≤ 4-port node, 18 for the H802RA).
    pub binds: usize,
    /// The raw reply datagrams, one per bind, kept so a real device's bytes can be
    /// saved as a reference fixture for the parser tests (CLAUDE.md §7).
    pub raw_replies: Vec<Vec<u8>>,
}

impl DiscoveredNode {
    /// The universes as contiguous ranges (what a node in the Show stores).
    pub fn universe_ranges(&self) -> Vec<UniverseRange> {
        to_ranges(&self.universes)
    }

    /// The MAC as `48:E0:C7:4D:96:64`, or empty when the reply had none.
    pub fn mac_string(&self) -> String {
        self.mac
            .map(|m| {
                m.iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(":")
            })
            .unwrap_or_default()
    }
}

/// Why a scan could not run.
#[derive(Debug)]
pub enum ScanError {
    /// UDP 6454 is held by another program (e.g. professional consoles running on this PC): replies
    /// cannot be received. Manual add still works.
    PortBusy(io::Error),
    /// Any other socket error.
    Io(io::Error),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::PortBusy(e) => write!(f, "UDP port 6454 is in use by another program: {e}"),
            ScanError::Io(e) => write!(f, "scan failed: {e}"),
        }
    }
}

impl std::error::Error for ScanError {}

/// Scan the network: `ArtPoll` to each of `broadcasts` (directed subnet broadcasts and
/// usually `255.255.255.255`) on the Art-Net port, replies collected for `window`.
///
/// Listens on `0.0.0.0:6454` — Art-Net 4 nodes unicast their reply to the poller,
/// older ones broadcast it, and both arrive on 6454.
pub fn scan(broadcasts: &[Ipv4Addr], window: Duration) -> Result<Vec<DiscoveredNode>, ScanError> {
    let listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), ARTNET_PORT);
    let targets: Vec<SocketAddr> = broadcasts
        .iter()
        .map(|&b| SocketAddr::new(IpAddr::V4(b), ARTNET_PORT))
        .collect();
    scan_on(listen, &targets, window)
}

/// [`scan`] on an explicit listen address and target list (tests use loopback).
pub fn scan_on(
    listen: SocketAddr,
    targets: &[SocketAddr],
    window: Duration,
) -> Result<Vec<DiscoveredNode>, ScanError> {
    let socket = open(listen)?;
    let mut poll = Vec::new();
    write_artpoll(&mut poll);
    let send_polls = |s: &UdpSocket| {
        for &t in targets {
            // A failed send to one target (no route to that subnet) must not stop the
            // others.
            let _ = s.send_to(&poll, t);
        }
    };
    send_polls(&socket);

    let start = Instant::now();
    let mut repolled = false;
    let mut replies = Vec::new();
    let mut buf = [0u8; 1024];
    while start.elapsed() < window {
        // UDP can drop a poll; ask once more halfway through the window.
        if !repolled && start.elapsed() >= window / 2 {
            send_polls(&socket);
            repolled = true;
        }
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Some(reply) = parse_artpollreply(&buf[..n]) {
                    replies.push((reply, buf[..n].to_vec()));
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(ScanError::Io(e)),
        }
    }
    Ok(merge(replies))
}

fn open(listen: SocketAddr) -> Result<UdpSocket, ScanError> {
    let socket =
        Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(ScanError::Io)?;
    socket.set_broadcast(true).map_err(ScanError::Io)?;
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .map_err(ScanError::Io)?;
    socket.bind(&listen.into()).map_err(|e| {
        if e.kind() == io::ErrorKind::AddrInUse {
            ScanError::PortBusy(e)
        } else {
            ScanError::Io(e)
        }
    })?;
    Ok(socket.into())
}

/// Merge replies into one node per IP. The same bind answering twice (both polls
/// reached it) counts once.
pub fn merge(replies: Vec<(PollReply, Vec<u8>)>) -> Vec<DiscoveredNode> {
    let mut by_ip: BTreeMap<[u8; 4], (DiscoveredNode, Vec<u8>)> = BTreeMap::new();
    for (reply, raw) in replies {
        let (node, seen_binds) = by_ip.entry(reply.ip).or_insert_with(|| {
            (
                DiscoveredNode {
                    ip: Ipv4Addr::from(reply.ip),
                    short_name: reply.short_name.clone(),
                    long_name: reply.long_name.clone(),
                    mac: reply.mac,
                    universes: Vec::new(),
                    binds: 0,
                    raw_replies: Vec::new(),
                },
                Vec::new(),
            )
        });
        if seen_binds.contains(&reply.bind_index) {
            continue;
        }
        seen_binds.push(reply.bind_index);
        node.binds += 1;
        node.universes.extend(&reply.output_universes);
        node.raw_replies.push(raw);
    }
    by_ip
        .into_values()
        .map(|(mut node, _)| {
            node.universes.sort_unstable();
            node.universes.dedup();
            node
        })
        .collect()
}

/// Sorted, unique universes → inclusive contiguous ranges (`[0,1,2,5]` → `0–2, 5`).
pub fn to_ranges(universes: &[u16]) -> Vec<UniverseRange> {
    let mut sorted = universes.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut ranges: Vec<UniverseRange> = Vec::new();
    for u in sorted {
        match ranges.last_mut() {
            Some(r) if r.last + 1 == u => r.last = u,
            _ => ranges.push(UniverseRange { first: u, last: u }),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::artpoll::build_artpollreply;

    const H802RA_MAC: [u8; 6] = [0x48, 0xE0, 0xC7, 0x4D, 0x96, 0x64];

    /// The H802RA as professional consoles scanned it: 18 binds × 4 universes, one IP/MAC. Port
    /// addresses run 0..71 (net 0, sub-nets 0..4, contiguous) — synthetic until the
    /// real bytes are captured by Prisma's own Scan.
    fn h802ra_replies() -> Vec<(PollReply, Vec<u8>)> {
        (0..18u16)
            .map(|b| {
                let first = b * 4;
                let raw = build_artpollreply(
                    [169, 254, 99, 11],
                    "H802RA",
                    "Oem ArtNet-Node",
                    H802RA_MAC,
                    0,
                    (first >> 4) as u8,
                    &[
                        (first & 0xF) as u8,
                        ((first + 1) & 0xF) as u8,
                        ((first + 2) & 0xF) as u8,
                        ((first + 3) & 0xF) as u8,
                    ],
                    (b + 1) as u8,
                );
                (parse_artpollreply(&raw).unwrap(), raw)
            })
            .collect()
    }

    #[test]
    fn eighteen_binds_merge_into_one_board_of_72_universes() {
        let nodes = merge(h802ra_replies());
        assert_eq!(nodes.len(), 1);
        let n = &nodes[0];
        assert_eq!(n.ip, Ipv4Addr::new(169, 254, 99, 11));
        assert_eq!(n.short_name, "H802RA");
        assert_eq!(n.binds, 18);
        assert_eq!(n.universes.len(), 72);
        assert_eq!(n.universe_ranges(), [UniverseRange { first: 0, last: 71 }]);
        assert_eq!(n.mac_string(), "48:E0:C7:4D:96:64");
        assert_eq!(n.raw_replies.len(), 18);
    }

    #[test]
    fn a_bind_answering_both_polls_counts_once_and_nodes_stay_apart() {
        let mut replies = h802ra_replies();
        replies.push(replies[0].clone()); // the re-poll reached bind 1 again
        let other = build_artpollreply([169, 254, 1, 50], "WLED", "WLED", [2; 6], 0, 0, &[0], 1);
        replies.push((parse_artpollreply(&other).unwrap(), other));
        let nodes = merge(replies);
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            nodes
                .iter()
                .find(|n| n.short_name == "H802RA")
                .unwrap()
                .binds,
            18
        );
    }

    #[test]
    fn ranges_collapse_contiguous_universes() {
        assert_eq!(
            to_ranges(&[5, 0, 1, 2, 2, 9, 10]),
            [
                UniverseRange { first: 0, last: 2 },
                UniverseRange { first: 5, last: 5 },
                UniverseRange { first: 9, last: 10 },
            ]
        );
        assert!(to_ranges(&[]).is_empty());
    }

    #[test]
    fn scan_finds_a_node_answering_on_loopback() {
        // A fake node: answers every ArtPoll it receives with 2 binds, to the sender.
        let node = UdpSocket::bind("127.0.0.1:0").unwrap();
        node.set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let node_addr = node.local_addr().unwrap();
        let fake = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            if let Ok((_, from)) = node.recv_from(&mut buf) {
                for bind in 1..=2u8 {
                    let raw = build_artpollreply(
                        [127, 0, 0, 1],
                        "Fake",
                        "Fake node",
                        [9; 6],
                        0,
                        0,
                        &[
                            (bind - 1) * 4,
                            (bind - 1) * 4 + 1,
                            (bind - 1) * 4 + 2,
                            (bind - 1) * 4 + 3,
                        ],
                        bind,
                    );
                    node.send_to(&raw, from).unwrap();
                }
            }
        });
        let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let nodes = scan_on(listen, &[node_addr], Duration::from_millis(400)).unwrap();
        fake.join().unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].universes, (0..8).collect::<Vec<u16>>());
        assert_eq!(nodes[0].binds, 2);
    }

    #[test]
    fn a_held_port_is_reported_as_busy() {
        let holder = UdpSocket::bind("127.0.0.1:0").unwrap();
        let taken = holder.local_addr().unwrap();
        match scan_on(taken, &[], Duration::from_millis(10)) {
            Err(ScanError::PortBusy(_)) => {}
            other => panic!("expected PortBusy, got {other:?}"),
        }
    }
}
