//! UDP transport for one output, with link-state tracking and self-healing
//! rebind.
//!
//! Interface loss is the most common real-install failure: an unplugged cable,
//! a DHCP address change, a switch reboot. The requirements this module meets
//! (kept as first-class state so the probe/UI can show it, not just a log):
//!
//!   - **Per-output link state**: `Connected` / `Degraded` / `Down`.
//!   - **Rebind policy**: on repeated send failures the socket is dropped and
//!     re-created on a backoff timer, so output recovers on its own when the
//!     interface returns.
//!   - **Isolation**: a send never blocks (non-blocking socket) and never
//!     panics. A failing output changes only its own state; the scheduler
//!     catches nothing and keeps sending every other universe.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

/// Link state of a single output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// Sending normally.
    Connected,
    /// Recent send failures, but not yet given up — still trying the socket.
    Degraded,
    /// Socket dropped; waiting on the rebind backoff to try re-creating it.
    Down,
}

/// A snapshot of an output's state and counters, for the probe/UI.
#[derive(Debug, Clone, Copy)]
pub struct OutputStatus {
    pub bind: SocketAddr,
    pub target: SocketAddr,
    pub state: LinkState,
    pub sent: u64,
    pub send_errors: u64,
}

/// Tunables for the link-state machine.
#[derive(Debug, Clone, Copy)]
pub struct LinkPolicy {
    /// Consecutive errors before the state drops to `Degraded`.
    pub degraded_after: u32,
    /// Consecutive errors before the socket is torn down and state is `Down`.
    pub down_after: u32,
    /// First rebind delay after going `Down`.
    pub rebind_backoff_start: Duration,
    /// Maximum rebind delay (backoff doubles up to this).
    pub rebind_backoff_max: Duration,
}

impl Default for LinkPolicy {
    fn default() -> Self {
        Self {
            degraded_after: 1,
            down_after: 5,
            rebind_backoff_start: Duration::from_millis(250),
            rebind_backoff_max: Duration::from_secs(5),
        }
    }
}

/// One UDP output: a bound socket sending to a fixed target, plus the link-state
/// machine around it.
pub struct UdpOutput {
    bind: SocketAddr,
    target: SocketAddr,
    policy: LinkPolicy,
    socket: Option<UdpSocket>,
    state: LinkState,
    consecutive_errors: u32,
    next_rebind: Instant,
    rebind_backoff: Duration,
    sent: u64,
    send_errors: u64,
}

impl UdpOutput {
    /// Create an output bound to `bind`, sending to `target`. If the initial
    /// bind fails (interface not present yet), the output starts `Down` and will
    /// rebind on its own — construction never fails on a missing interface.
    pub fn new(bind: SocketAddr, target: SocketAddr, policy: LinkPolicy) -> Self {
        let mut out = Self {
            bind,
            target,
            policy,
            socket: None,
            state: LinkState::Down,
            consecutive_errors: 0,
            next_rebind: Instant::now(),
            rebind_backoff: policy.rebind_backoff_start,
            sent: 0,
            send_errors: 0,
        };
        out.try_bind();
        out
    }

    /// Convenience: bind to an ephemeral local port on all interfaces.
    pub fn to_target(target: SocketAddr) -> Self {
        let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        Self::new(bind, target, LinkPolicy::default())
    }

    fn open_socket(bind: SocketAddr, target: SocketAddr) -> io::Result<UdpSocket> {
        let domain = if bind.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        // Non-blocking is what keeps a full send buffer from ever stalling the
        // output thread (isolation requirement).
        socket.set_nonblocking(true)?;
        // Allow directed/broadcast targets used by some Art-Net setups.
        if target.ip().is_ipv4() {
            let _ = socket.set_broadcast(true);
        }
        socket.bind(&bind.into())?;
        Ok(socket.into())
    }

    fn try_bind(&mut self) {
        match Self::open_socket(self.bind, self.target) {
            Ok(sock) => {
                self.socket = Some(sock);
                self.state = LinkState::Connected;
                self.consecutive_errors = 0;
                self.rebind_backoff = self.policy.rebind_backoff_start;
            }
            Err(_) => {
                self.socket = None;
                self.state = LinkState::Down;
                self.schedule_rebind();
            }
        }
    }

    fn schedule_rebind(&mut self) {
        self.next_rebind = Instant::now() + self.rebind_backoff;
        self.rebind_backoff = (self.rebind_backoff * 2).min(self.policy.rebind_backoff_max);
    }

    fn on_error(&mut self) {
        self.send_errors += 1;
        self.consecutive_errors += 1;
        if self.consecutive_errors >= self.policy.down_after {
            // Tear the socket down and let rebind re-create it. A dead interface
            // often keeps failing on the same fd; a fresh bind is how DHCP/IP
            // changes and switch reboots recover.
            self.socket = None;
            self.state = LinkState::Down;
            self.schedule_rebind();
        } else if self.consecutive_errors >= self.policy.degraded_after {
            self.state = LinkState::Degraded;
        }
    }

    /// Send one datagram. Never blocks, never panics. Returns `true` if the
    /// bytes were handed to the kernel, `false` if the frame was dropped (link
    /// down, buffer full, or a send error) — the caller keeps going regardless.
    pub fn send(&mut self, bytes: &[u8]) -> bool {
        // If we are down, try to recover on the backoff timer, but do not block.
        if self.socket.is_none() {
            if Instant::now() >= self.next_rebind {
                self.try_bind();
            }
            if self.socket.is_none() {
                return false;
            }
        }

        let target = self.target;
        let socket = self.socket.as_ref().expect("socket present");
        match socket.send_to(bytes, target) {
            Ok(_) => {
                self.sent += 1;
                self.consecutive_errors = 0;
                self.state = LinkState::Connected;
                true
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Transient buffer pressure, not a link fault: drop this frame,
                // do not escalate link state.
                false
            }
            Err(_) => {
                self.on_error();
                false
            }
        }
    }

    /// Current link state.
    #[inline]
    pub fn state(&self) -> LinkState {
        self.state
    }

    /// A snapshot for the probe/UI.
    pub fn status(&self) -> OutputStatus {
        OutputStatus {
            bind: self.bind,
            target: self.target,
            state: self.state,
            sent: self.sent,
            send_errors: self.send_errors,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn binds_and_sends_to_loopback() {
        // A real receiver on loopback; the send must land.
        let rx = UdpSocket::bind(loopback(0)).unwrap();
        let target = rx.local_addr().unwrap();

        let mut out = UdpOutput::to_target(target);
        assert_eq!(out.state(), LinkState::Connected);
        assert!(out.send(&[1, 2, 3, 4]));

        let mut buf = [0u8; 16];
        rx.set_nonblocking(true).unwrap();
        // Give the loopback a moment; nonblocking recv may need a couple tries.
        let mut got = None;
        for _ in 0..1000 {
            if let Ok((n, _)) = rx.recv_from(&mut buf) {
                got = Some(n);
                break;
            }
            std::hint::spin_loop();
        }
        assert_eq!(got, Some(4));
        assert_eq!(&buf[..4], &[1, 2, 3, 4]);
    }

    #[test]
    fn send_failures_never_panic_and_output_stays_usable() {
        // 240.0.0.0/4 is reserved/unroutable; sends may error depending on the
        // OS, but the output must never panic or block and must stay usable.
        let policy = LinkPolicy {
            degraded_after: 1,
            down_after: 3,
            ..LinkPolicy::default()
        };
        let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let unroutable = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1)), 6454);
        let mut out = UdpOutput::new(bind, unroutable, policy);

        for _ in 0..10 {
            // Return value is intentionally ignored: a dropped frame is fine,
            // a panic or block is not.
            let _ = out.send(&[0u8; 4]);
        }
        // Still queryable, error count is bounded by attempts.
        assert!(out.status().send_errors <= 10);
        let _ = out.state();
    }
}
