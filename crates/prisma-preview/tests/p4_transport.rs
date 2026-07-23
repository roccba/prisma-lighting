//! P4 acceptance: the preview transports (CLAUDE.md M6).
//!
//! The WebSocket test stands in for the browser viewer: a real WebSocket client
//! connects to the sink, and we assert it receives the descriptor as text and a
//! frame as binary, that the sink reports consumers only while one is connected,
//! and that a gone client is pruned (so the loop's zero-cost gate flips back).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use prisma_preview::preview_loop::PreviewSink;
use prisma_preview::sink::{ChannelSink, PreviewMessage, WebSocketSink};
use tungstenite::Message;

fn loopback0() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

/// Poll `cond` until true or the deadline, so the test does not race the sink's
/// accept thread.
fn wait_until(mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "condition never became true");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn channel_sink_forwards_and_reports_disconnect() {
    let (mut sink, rx) = ChannelSink::new();
    assert!(sink.has_consumers());

    sink.send_descriptor("{\"generation\":1}");
    sink.send_frame(&[1, 2, 3]);

    assert_eq!(
        rx.recv().unwrap(),
        PreviewMessage::Descriptor("{\"generation\":1}".into())
    );
    assert_eq!(rx.recv().unwrap(), PreviewMessage::Frame(vec![1, 2, 3]));

    // Receiver gone -> the sink reports no consumers on its next send, so the loop
    // will stop encoding.
    drop(rx);
    sink.send_frame(&[9]);
    assert!(!sink.has_consumers());
}

#[test]
fn websocket_sink_delivers_descriptor_then_frame_to_a_real_client() {
    let mut sink = WebSocketSink::bind(loopback0()).unwrap();
    let port = sink.local_addr().port();
    assert!(!sink.has_consumers(), "nobody connected yet");

    let (mut client, _resp) =
        tungstenite::connect(format!("ws://127.0.0.1:{port}")).expect("client connects");

    // The accept thread registers the client slightly after connect returns.
    wait_until(|| sink.client_count() == 1);
    assert!(sink.has_consumers());

    let descriptor = "{\"generation\":7,\"pixels\":[]}";
    sink.send_descriptor(descriptor);
    sink.send_frame(&[0xAA, 0xBB, 0xCC]);

    match client.read().unwrap() {
        Message::Text(t) => assert_eq!(t.as_str(), descriptor),
        other => panic!("expected descriptor text, got {other:?}"),
    }
    match client.read().unwrap() {
        Message::Binary(b) => assert_eq!(&b[..], &[0xAA, 0xBB, 0xCC]),
        other => panic!("expected binary frame, got {other:?}"),
    }
}

#[test]
fn websocket_sink_serves_a_late_client_the_cached_descriptor() {
    // Descriptor announced BEFORE any client connects: a client that connects
    // afterwards must still receive it (the sink caches and replays it).
    let mut sink = WebSocketSink::bind(loopback0()).unwrap();
    let port = sink.local_addr().port();

    let descriptor = "{\"generation\":42}";
    sink.send_descriptor(descriptor); // no clients yet; cached only

    let (mut client, _resp) = tungstenite::connect(format!("ws://127.0.0.1:{port}")).unwrap();
    wait_until(|| sink.client_count() == 1);

    match client.read().unwrap() {
        Message::Text(t) => assert_eq!(t.as_str(), descriptor, "late client got the cache"),
        other => panic!("expected cached descriptor, got {other:?}"),
    }
}

#[test]
fn a_gone_client_is_pruned() {
    let mut sink = WebSocketSink::bind(loopback0()).unwrap();
    let port = sink.local_addr().port();

    let (client, _resp) = tungstenite::connect(format!("ws://127.0.0.1:{port}")).unwrap();
    wait_until(|| sink.client_count() == 1);

    drop(client); // the viewer closes its tab

    // A couple of sends let the server observe the dead socket and prune it; then
    // has_consumers flips back to false so the loop returns to zero cost.
    wait_until(|| {
        sink.send_frame(&[0]);
        std::thread::sleep(Duration::from_millis(20));
        sink.client_count() == 0
    });
    assert!(!sink.has_consumers());
}
