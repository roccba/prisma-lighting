//! Host network interfaces, and resolving a Show's stored interface on this machine
//! (docs/DEVICES-SPEC.md §6).
//!
//! A Show stores its output interface by **network** ([`InterfaceRef`]: an address +
//! prefix), not by name, because the same NIC is "eth0" on one PC and "Ethernet 2" on
//! another. On load the engine looks for the host interface on that network and sends
//! from it; if this machine has none, output still runs through the OS default route
//! and the UI warns — a show never refuses to play over a missing NIC.
//!
//! Setup path only: listing interfaces is a syscall walk, never done on the output
//! thread.

use std::net::Ipv4Addr;

use prisma_core::InterfaceRef;

/// One IPv4 interface of this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInterface {
    /// OS name ("enp3s0", "Ethernet 2", …), for display.
    pub name: String,
    pub address: Ipv4Addr,
    pub prefix_len: u8,
}

impl HostInterface {
    /// This interface as a Show reference (what the Device Manager stores).
    pub fn to_ref(&self) -> InterfaceRef {
        InterfaceRef {
            address: self.address,
            prefix_len: self.prefix_len,
            name_hint: self.name.clone(),
        }
    }

    /// The interface's directed broadcast address.
    pub fn broadcast(&self) -> Ipv4Addr {
        self.to_ref().broadcast()
    }
}

/// The machine's IPv4 interfaces, loopback excluded, in OS order. An OS error yields an
/// empty list (the Device Manager then offers only "let the OS route").
pub fn host_interfaces() -> Vec<HostInterface> {
    let Ok(all) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    all.into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() => Some(HostInterface {
                name: i.name,
                address: v4.ip,
                prefix_len: v4.prefixlen,
            }),
            _ => None,
        })
        .collect()
}

/// How a Show's stored interface resolved on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterfaceResolution {
    /// The show names no interface: the OS routes (bind `0.0.0.0`).
    Unset,
    /// Found: send from this host interface.
    Resolved(HostInterface),
    /// This machine has no interface on the show's network: the OS routes, and the UI
    /// warns ("La red … del show no está en esta PC").
    Missing(InterfaceRef),
}

impl InterfaceResolution {
    /// The address to bind the output socket to (`0.0.0.0` unless resolved).
    pub fn bind_address(&self) -> Ipv4Addr {
        match self {
            InterfaceResolution::Resolved(h) => h.address,
            _ => Ipv4Addr::UNSPECIFIED,
        }
    }

    /// The subnet broadcast for nodes set to broadcast, when known.
    pub fn broadcast(&self) -> Option<Ipv4Addr> {
        match self {
            InterfaceResolution::Unset => None,
            InterfaceResolution::Resolved(h) => Some(h.broadcast()),
            // Still the show's network — the OS may route there via another path.
            InterfaceResolution::Missing(r) => Some(r.broadcast()),
        }
    }
}

/// Resolve `wanted` against `host` (pure, so it is testable without real NICs).
///
/// A host interface matches when its address is on the wanted network. Several matches
/// (two NICs on the same subnet) prefer the exact address, then the remembered name.
pub fn resolve(wanted: Option<&InterfaceRef>, host: &[HostInterface]) -> InterfaceResolution {
    let Some(wanted) = wanted else {
        return InterfaceResolution::Unset;
    };
    let mut on_network: Vec<&HostInterface> =
        host.iter().filter(|h| wanted.contains(h.address)).collect();
    on_network.sort_by_key(|h| (h.address != wanted.address, h.name != wanted.name_hint));
    match on_network.first() {
        Some(h) => InterfaceResolution::Resolved((*h).clone()),
        None => InterfaceResolution::Missing(wanted.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, ip: [u8; 4], prefix_len: u8) -> HostInterface {
        HostInterface {
            name: name.into(),
            address: Ipv4Addr::from(ip),
            prefix_len,
        }
    }

    fn wanted(ip: [u8; 4], prefix_len: u8, name: &str) -> InterfaceRef {
        InterfaceRef {
            address: Ipv4Addr::from(ip),
            prefix_len,
            name_hint: name.into(),
        }
    }

    #[test]
    fn resolves_by_network_on_another_machine() {
        // Saved on a PC where the NIC was "eth0" 169.254.1.2; this PC has the same
        // link-local network under another name and address.
        let here = [
            host("wlan0", [192, 168, 1, 20], 24),
            host("Ethernet 2", [169, 254, 33, 4], 16),
        ];
        let r = resolve(Some(&wanted([169, 254, 1, 2], 16, "eth0")), &here);
        assert_eq!(r, InterfaceResolution::Resolved(here[1].clone()));
        assert_eq!(r.bind_address(), Ipv4Addr::new(169, 254, 33, 4));
        assert_eq!(r.broadcast(), Some(Ipv4Addr::new(169, 254, 255, 255)));
    }

    #[test]
    fn a_missing_network_warns_but_still_routes() {
        let here = [host("wlan0", [192, 168, 1, 20], 24)];
        let w = wanted([2, 0, 0, 10], 8, "eth0");
        let r = resolve(Some(&w), &here);
        assert_eq!(r, InterfaceResolution::Missing(w));
        assert_eq!(r.bind_address(), Ipv4Addr::UNSPECIFIED);
        assert_eq!(r.broadcast(), Some(Ipv4Addr::new(2, 255, 255, 255)));
    }

    #[test]
    fn several_matches_prefer_exact_address_then_name() {
        let here = [
            host("a", [10, 0, 0, 5], 8),
            host("eth1", [10, 0, 0, 6], 8),
            host("b", [10, 0, 0, 7], 8),
        ];
        let by_ip = resolve(Some(&wanted([10, 0, 0, 7], 8, "zzz")), &here);
        assert_eq!(by_ip, InterfaceResolution::Resolved(here[2].clone()));
        let by_name = resolve(Some(&wanted([10, 0, 0, 99], 8, "eth1")), &here);
        assert_eq!(by_name, InterfaceResolution::Resolved(here[1].clone()));
    }

    #[test]
    fn no_interface_set_lets_the_os_route() {
        let r = resolve(None, &[]);
        assert_eq!(r, InterfaceResolution::Unset);
        assert_eq!(r.bind_address(), Ipv4Addr::UNSPECIFIED);
        assert_eq!(r.broadcast(), None);
    }

    #[test]
    fn host_listing_runs_and_skips_loopback() {
        // Real syscall; content is machine-specific, but loopback must never appear.
        assert!(host_interfaces().iter().all(|h| !h.address.is_loopback()));
    }
}
