//! Preview transports: where encoded bytes actually go (CLAUDE.md M6, P4).
//!
//! Two [`PreviewSink`](crate::preview_loop::PreviewSink) implementations:
//!
//! - [`ChannelSink`] — in-process, forwards messages over a channel. For tests and
//!   for any headless consumer (a PPM dumper, a measurement harness).
//! - [`WebSocketSink`] — serves the browser viewer over WebSocket.
//!
//! # The transport never blocks the preview loop
//!
//! Both honour the loop's contract: a slow or gone consumer is **dropped**, never
//! waited on. [`ChannelSink`] reports itself disconnected once its receiver is
//! gone. [`WebSocketSink`] gives each client its own thread and a small bounded
//! queue; if that queue is full the frame is dropped for that client (backpressure
//! = drop), and the loop thread never touches a socket.

use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tungstenite::{Message, WebSocket};

use crate::preview_loop::PreviewSink;

/// One message as a transport sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewMessage {
    /// The static descriptor as JSON text.
    Descriptor(String),
    /// One encoded binary frame.
    Frame(Vec<u8>),
}

/// In-process sink: forwards every message to an [`mpsc`](std::sync::mpsc)
/// receiver. `has_consumers` is true while the receiver is alive, so a preview
/// loop feeding a dropped receiver correctly falls back to doing nothing.
pub struct ChannelSink {
    tx: Sender<PreviewMessage>,
    connected: bool,
}

impl ChannelSink {
    /// Create the sink and its receiver. The caller owns the receiver; when it is
    /// dropped, the sink reports no consumers on its next send.
    pub fn new() -> (ChannelSink, Receiver<PreviewMessage>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            ChannelSink {
                tx,
                connected: true,
            },
            rx,
        )
    }

    fn forward(&mut self, msg: PreviewMessage) {
        if self.tx.send(msg).is_err() {
            self.connected = false;
        }
    }
}

impl PreviewSink for ChannelSink {
    fn has_consumers(&self) -> bool {
        self.connected
    }
    fn send_descriptor(&mut self, descriptor_json: &str) {
        self.forward(PreviewMessage::Descriptor(descriptor_json.to_owned()));
    }
    fn send_frame(&mut self, frame: &[u8]) {
        self.forward(PreviewMessage::Frame(frame.to_vec()));
    }
}

// ---------------------------------------------------------------------------
// WebSocket transport
// ---------------------------------------------------------------------------

/// One connected browser client: a bounded queue to its writer thread. A full
/// queue means the client is not keeping up; the frame is dropped for it.
struct Client {
    tx: SyncSender<Message>,
}

struct Shared {
    clients: Mutex<Vec<Client>>,
    /// The last descriptor, replayed to each client that connects afterwards.
    descriptor: Mutex<Option<String>>,
    stop: AtomicBool,
}

/// Serves the preview to browser viewers over WebSocket. Descriptor is sent as a
/// text message, frames as binary. Each client runs on its own thread so the
/// preview loop never blocks on a socket.
pub struct WebSocketSink {
    shared: Arc<Shared>,
    addr: SocketAddr,
    accept: Option<JoinHandle<()>>,
}

impl WebSocketSink {
    /// Bind and start accepting connections. Pass a port of `0` to let the OS pick
    /// one (read it back with [`local_addr`](Self::local_addr)); a real deployment
    /// passes a fixed port the viewer knows.
    pub fn bind(addr: SocketAddr) -> std::io::Result<WebSocketSink> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;

        let shared = Arc::new(Shared {
            clients: Mutex::new(Vec::new()),
            descriptor: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let accept = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || accept_loop(listener, shared))
        };
        Ok(WebSocketSink {
            shared,
            addr,
            accept: Some(accept),
        })
    }

    /// The address actually bound — the one the viewer connects to.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Number of clients currently connected. A one-tick lag is possible (a dead
    /// client is pruned on the next frame), which is harmless for the preview.
    pub fn client_count(&self) -> usize {
        self.shared.clients.lock().unwrap().len()
    }
}

impl Drop for WebSocketSink {
    fn drop(&mut self) {
        // Signal the accept loop and every client writer to stop, then join the
        // acceptor. Client threads observe the flag within their recv timeout.
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

impl PreviewSink for WebSocketSink {
    fn has_consumers(&self) -> bool {
        !self.shared.clients.lock().unwrap().is_empty()
    }

    fn send_descriptor(&mut self, descriptor_json: &str) {
        *self.shared.descriptor.lock().unwrap() = Some(descriptor_json.to_owned());
        let msg = Message::text(descriptor_json.to_owned());
        push_to_all(&self.shared, msg);
    }

    fn send_frame(&mut self, frame: &[u8]) {
        let msg = Message::binary(frame.to_vec());
        push_to_all(&self.shared, msg);
    }
}

/// Try to hand `msg` to every client. Full queue → drop for that client (keep it);
/// disconnected → prune it.
fn push_to_all(shared: &Arc<Shared>, msg: Message) {
    let mut clients = shared.clients.lock().unwrap();
    clients.retain(|c| match c.tx.try_send(msg.clone()) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Disconnected(_)) => false,
    });
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                // The handshake itself is blocking; put the stream back into
                // blocking mode for it, then hand the socket to a writer thread.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                match tungstenite::accept(stream) {
                    Ok(ws) => spawn_client(ws, &shared),
                    Err(_) => continue, // a failed handshake is just a lost client
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn spawn_client(mut ws: WebSocket<TcpStream>, shared: &Arc<Shared>) {
    // A small bounded queue: a couple of frames of slack, then drop. The preview
    // is not on the latency budget, so a lagging viewer loses frames rather than
    // ever stalling the loop.
    let (tx, rx) = sync_channel::<Message>(4);

    // Serve this client the current descriptor first, so it can interpret the
    // frames that follow even if it connected after the loop already announced it.
    if let Some(descriptor) = shared.descriptor.lock().unwrap().clone() {
        let _ = tx.try_send(Message::text(descriptor));
    }

    shared.clients.lock().unwrap().push(Client { tx });

    let shared = Arc::clone(shared);
    thread::spawn(move || {
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(msg) => {
                    // Blocking write on this client's OWN thread — never the loop's.
                    if ws.send(msg).is_err() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue, // re-check stop
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = ws.close(None);
    });
}
