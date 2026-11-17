//! Per-universe destinations: which node(s) each universe of the plan is sent to
//! (docs/DEVICES-SPEC.md §5).
//!
//! Compiled **off the output thread**, alongside the [`OutputPlan`](crate::plan::OutputPlan),
//! whenever the plan or the Show's [`NetworkConfig`] changes. The output thread then does
//! an indexed lookup per universe — no searching, no allocation, no locks (CLAUDE.md §3.3):
//! all destinations live in one flat array and each universe owns a contiguous span of it.
//!
//! Rules:
//! - a universe covered by **no** enabled node is not sent (the UI warns);
//! - a universe covered by **several** nodes is sent to each (mirroring is legitimate);
//! - a node with `unicast: false` is sent to the interface's subnet broadcast;
//! - `artnet_enabled: false` or no nodes at all → every universe to the loopback virtual
//!   sink, exactly the pre-device-manager behaviour.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use prisma_core::NetworkConfig;

/// The Art-Net UDP port.
pub const ARTNET_PORT: u16 = 6454;

/// One place a universe's packet goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Destination {
    pub addr: SocketAddr,
    /// Pad the ArtDmx to 512 channels for this destination.
    pub full_frames: bool,
}

/// For each universe of a plan (in plan order), the destinations it is sent to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DestinationTable {
    /// Every destination of every universe, back to back.
    flat: Vec<Destination>,
    /// `spans[i] = (start, len)` into `flat` for plan universe `i`.
    spans: Vec<(u32, u32)>,
    /// One entry per distinct node address, for the per-frame ArtSync.
    sync_targets: Vec<SocketAddr>,
    /// Plan universes (port addresses) no enabled node receives.
    uncovered: Vec<u16>,
}

impl DestinationTable {
    /// Build the table for a plan's universes (`port_addresses`, in plan order).
    ///
    /// `broadcast` is the chosen interface's directed broadcast, used by nodes with
    /// `unicast: false`; without a resolved interface they fall back to the limited
    /// broadcast `255.255.255.255`.
    ///
    /// `fallback` is where everything goes when the output is not routed (Art-Net
    /// disabled, or no nodes): the virtual sink in the app, a test receiver in tests.
    pub fn compile(
        port_addresses: &[u16],
        net: &NetworkConfig,
        broadcast: Option<Ipv4Addr>,
        fallback: SocketAddr,
    ) -> DestinationTable {
        let routed = net.artnet_enabled && !net.nodes.is_empty();
        if !routed {
            return Self::all_to(fallback, port_addresses.len());
        }
        let mut table = DestinationTable::default();

        let bcast = broadcast.unwrap_or(Ipv4Addr::BROADCAST);
        let addr_of = |ip: Ipv4Addr| SocketAddr::new(IpAddr::V4(ip), ARTNET_PORT);
        for &pa in port_addresses {
            let start = table.flat.len() as u32;
            for node in net.nodes.iter().filter(|n| n.enabled && n.receives(pa)) {
                let addr = addr_of(if node.unicast { node.ip } else { bcast });
                // Two nodes sharing a broadcast (or a duplicated IP) get one packet.
                let span = &table.flat[start as usize..];
                if !span.iter().any(|d| d.addr == addr) {
                    table.flat.push(Destination {
                        addr,
                        full_frames: node.full_frames,
                    });
                } else if node.full_frames {
                    // Shared address: if any node behind it wants full frames, pad.
                    let d = table.flat[start as usize..]
                        .iter_mut()
                        .find(|d| d.addr == addr)
                        .expect("present");
                    d.full_frames = true;
                }
            }
            let len = table.flat.len() as u32 - start;
            if len == 0 {
                table.uncovered.push(pa);
            }
            table.spans.push((start, len));
        }

        for node in net.nodes.iter().filter(|n| n.enabled) {
            let addr = addr_of(if node.unicast { node.ip } else { bcast });
            if !table.sync_targets.contains(&addr) {
                table.sync_targets.push(addr);
            }
        }
        table
    }

    /// Every one of `universe_count` universes to the single `target` (and the ArtSync
    /// there too) — the pre-device-manager behaviour, used for the virtual sink, the
    /// probes and tests.
    pub fn all_to(target: SocketAddr, universe_count: usize) -> DestinationTable {
        DestinationTable {
            flat: vec![
                Destination {
                    addr: target,
                    full_frames: false,
                };
                universe_count
            ],
            spans: (0..universe_count as u32).map(|i| (i, 1)).collect(),
            sync_targets: vec![target],
            uncovered: Vec::new(),
        }
    }

    /// How many plan universes the table covers (one span each, possibly empty).
    pub fn universe_count(&self) -> usize {
        self.spans.len()
    }

    /// The destinations of plan universe `index` (empty if uncovered or out of range).
    #[inline]
    pub fn destinations(&self, index: usize) -> &[Destination] {
        match self.spans.get(index) {
            Some(&(start, len)) => &self.flat[start as usize..(start + len) as usize],
            None => &[],
        }
    }

    /// Where to send the per-frame ArtSync: each distinct node address once.
    pub fn sync_targets(&self) -> &[SocketAddr] {
        &self.sync_targets
    }

    /// Plan universes (port addresses) that no enabled node receives.
    pub fn uncovered(&self) -> &[u16] {
        &self.uncovered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prisma_core::{ArtNetNode, NodeOrigin, UniverseRange};
    use uuid::Uuid;

    fn node(ip: [u8; 4], first: u16, last: u16) -> ArtNetNode {
        ArtNetNode {
            id: Uuid::new_v4(),
            name: format!("{ip:?}"),
            ip: Ipv4Addr::from(ip),
            universes: vec![UniverseRange { first, last }],
            enabled: true,
            unicast: true,
            full_frames: false,
            origin: NodeOrigin::Manual,
        }
    }

    fn sink() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), ARTNET_PORT)
    }

    fn net(nodes: Vec<ArtNetNode>) -> NetworkConfig {
        NetworkConfig {
            initialized: true,
            artnet_enabled: true,
            nodes,
            ..Default::default()
        }
    }

    fn ip_of(d: &[Destination]) -> Vec<Ipv4Addr> {
        d.iter()
            .map(|d| match d.addr.ip() {
                IpAddr::V4(v4) => v4,
                IpAddr::V6(_) => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn each_universe_goes_to_its_node() {
        let n = net(vec![node([10, 0, 0, 1], 0, 5), node([10, 0, 0, 2], 6, 11)]);
        let t = DestinationTable::compile(&[0, 5, 6, 11], &n, None, sink());
        assert_eq!(ip_of(t.destinations(0)), [Ipv4Addr::new(10, 0, 0, 1)]);
        assert_eq!(ip_of(t.destinations(1)), [Ipv4Addr::new(10, 0, 0, 1)]);
        assert_eq!(ip_of(t.destinations(2)), [Ipv4Addr::new(10, 0, 0, 2)]);
        assert_eq!(ip_of(t.destinations(3)), [Ipv4Addr::new(10, 0, 0, 2)]);
        assert!(t.uncovered().is_empty());
        assert_eq!(t.sync_targets().len(), 2);
        assert_eq!(t.destinations(0)[0].addr.port(), ARTNET_PORT);
    }

    #[test]
    fn overlapping_nodes_both_receive_and_uncovered_is_reported() {
        let n = net(vec![node([10, 0, 0, 1], 0, 3), node([10, 0, 0, 2], 2, 3)]);
        let t = DestinationTable::compile(&[0, 2, 9], &n, None, sink());
        assert_eq!(t.destinations(0).len(), 1);
        assert_eq!(
            t.destinations(1).len(),
            2,
            "mirrored universe goes to both nodes"
        );
        assert!(t.destinations(2).is_empty());
        assert_eq!(t.uncovered(), [9]);
    }

    #[test]
    fn disabled_nodes_are_skipped() {
        let mut off = node([10, 0, 0, 2], 0, 3);
        off.enabled = false;
        let n = net(vec![node([10, 0, 0, 1], 0, 1), off]);
        let t = DestinationTable::compile(&[0, 2], &n, None, sink());
        assert_eq!(t.destinations(0).len(), 1);
        assert_eq!(t.uncovered(), [2]);
        assert_eq!(t.sync_targets().len(), 1, "no ArtSync to a disabled node");
    }

    #[test]
    fn broadcast_nodes_use_the_interface_broadcast_once() {
        let mut a = node([2, 0, 0, 1], 0, 3);
        a.unicast = false;
        let mut b = node([2, 0, 0, 2], 0, 3);
        b.unicast = false;
        b.full_frames = true;
        let bc = Ipv4Addr::new(2, 255, 255, 255);
        let t = DestinationTable::compile(&[1], &net(vec![a, b]), Some(bc), sink());
        assert_eq!(
            ip_of(t.destinations(0)),
            [bc],
            "one packet per shared address"
        );
        assert!(
            t.destinations(0)[0].full_frames,
            "padded if any node behind it wants it"
        );
        assert_eq!(t.sync_targets().len(), 1);
    }

    #[test]
    fn disabled_output_or_no_nodes_goes_to_the_virtual_sink() {
        let sink_ip = Ipv4Addr::LOCALHOST;
        let mut n = net(vec![node([10, 0, 0, 1], 0, 3)]);
        n.artnet_enabled = false;
        let t = DestinationTable::compile(&[0, 1], &n, None, sink());
        assert_eq!(ip_of(t.destinations(1)), [sink_ip]);
        let t = DestinationTable::compile(&[0, 1], &net(vec![]), None, sink());
        assert_eq!(ip_of(t.destinations(0)), [sink_ip]);
        assert!(t.uncovered().is_empty());
    }
}
