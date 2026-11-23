//! Network output (the device manager): the show's Art-Net nodes and interface, node status and
//! the Art-Net scan.

use super::*;

/// The running show's whole network output config (nodes, interface, enable, ArtSync),
/// for the Device Manager. Universe numbers are raw port addresses (the UI shows +1).
#[tauri::command]
pub fn main_output_get(state: State<AppState>) -> ShowNetwork {
    state.show.lock().unwrap().network.clone()
}

/// Apply the Device Manager's staged config (Aplicar / Aceptar) to the running show and
/// recompile the output onto it. Validated first; a bad config changes nothing.
#[tauri::command]
pub fn main_output_set(app: AppHandle, network: ShowNetwork) -> Result<(), String> {
    network.validate().map_err(|e| e.to_string())?;
    recompile_running_show(&app, |show| {
        show.network = network;
        show.network.initialized = true;
    })
}

/// This machine's host name, shown in the Device Manager like professional consoles ("Nombre del
/// host"). From the OS environment / kernel; empty if unknown.
#[tauri::command]
pub fn main_host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        })
        .map(|h| h.trim().to_string())
        .unwrap_or_default()
}

/// Open a node's web configuration page (the classic "HTTP" button) in the system browser.
/// Only `http://<IPv4>` is accepted — the command never opens arbitrary URLs.
#[tauri::command]
pub fn main_open_node_page(ip: String) -> Result<(), String> {
    let ip: Ipv4Addr = ip
        .trim()
        .parse()
        .map_err(|_| format!("IP inválida: {ip}"))?;
    let url = format!("http://{ip}/");
    #[cfg(target_os = "windows")]
    let spawned = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn();
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open").arg(&url).spawn();
    spawned.map(|_| ()).map_err(|e| e.to_string())
}

/// A host network interface, for the Device Manager's interface dropdown.
#[derive(Serialize)]
pub(crate) struct InterfaceInfo {
    pub(crate) name: String,
    pub(crate) address: String,
    pub(crate) prefix_len: u8,
}

impl From<&HostInterface> for InterfaceInfo {
    fn from(h: &HostInterface) -> Self {
        Self {
            name: h.name.clone(),
            address: h.address.to_string(),
            prefix_len: h.prefix_len,
        }
    }
}

/// This machine's IPv4 interfaces (loopback excluded), for choosing the show's output
/// interface (docs/DEVICES-SPEC.md §6). Setup path only — never the output thread.
#[tauri::command]
pub fn main_network_interfaces() -> Vec<InterfaceInfo> {
    host_interfaces().iter().map(InterfaceInfo::from).collect()
}

/// How the running show's output resolved: the interface (or why none), and the patch
/// universes no enabled node receives — the Device Manager's warnings (§9).
#[derive(Serialize)]
pub(crate) struct OutputStatusInfo {
    /// "unset" (OS routes), "resolved" or "missing" (the show's network is not here).
    pub(crate) interface_state: &'static str,
    /// The resolved host interface, or the show's wanted network when missing.
    pub(crate) interface: Option<InterfaceInfo>,
    /// Uncovered universes, as raw Art-Net port addresses (the UI shows them +1).
    pub(crate) uncovered: Vec<u16>,
}

#[tauri::command]
pub fn main_output_status(state: State<AppState>) -> Result<OutputStatusInfo, String> {
    let guard = state.engine.lock().unwrap();
    let engine = guard.as_ref().ok_or("engine is not running")?;
    let routing = engine.output_routing();
    let (interface_state, interface) = match &routing.interface {
        InterfaceResolution::Unset => ("unset", None),
        InterfaceResolution::Resolved(h) => ("resolved", Some(InterfaceInfo::from(h))),
        InterfaceResolution::Missing(r) => (
            "missing",
            Some(InterfaceInfo {
                name: r.name_hint.clone(),
                address: r.address.to_string(),
                prefix_len: r.prefix_len,
            }),
        ),
    };
    Ok(OutputStatusInfo {
        interface_state,
        interface,
        uncovered: routing.uncovered.clone(),
    })
}

/// One device row found by a Scan (one per `BindIndex` group, like professional consoles).
#[derive(Serialize)]
pub(crate) struct ScannedNodeInfo {
    pub(crate) ip: String,
    pub(crate) short_name: String,
    pub(crate) long_name: String,
    pub(crate) mac: String,
    pub(crate) bind_index: u8,
    /// The universe (raw Art-Net port address) of each output port.
    pub(crate) ports: Vec<u16>,
    pub(crate) esta: u16,
    pub(crate) oem: u16,
    pub(crate) firmware: u16,
}

impl From<&DiscoveredNode> for ScannedNodeInfo {
    fn from(n: &DiscoveredNode) -> Self {
        Self {
            ip: n.ip.to_string(),
            short_name: n.short_name.clone(),
            long_name: n.long_name.clone(),
            mac: n.mac_string(),
            bind_index: n.bind_index,
            ports: n.ports.clone(),
            esta: n.esta,
            oem: n.oem,
            firmware: n.firmware,
        }
    }
}

/// Start the Art-Net node monitor if it is not running, and return it. Its status
/// changes reach the UI as `artnet-node-status` events (`[{ip, status}]`).
pub(crate) fn ensure_artnet_monitor(app: &AppHandle) -> Result<Arc<NodeMonitor>, String> {
    let state = app.state::<AppState>();
    let mut slot = state.artnet_monitor.lock().unwrap();
    if let Some(m) = slot.as_ref() {
        return Ok(Arc::clone(m));
    }
    let emitter = app.clone();
    let monitor = NodeMonitor::start(Box::new(move |changed| {
        let payload: Vec<NodeStatusInfo> = changed
            .iter()
            .map(|&(ip, st)| NodeStatusInfo::new(ip, st))
            .collect();
        let _ = emitter.emit("artnet-node-status", payload);
    }))
    .map_err(|e| match e {
        ScanError::PortBusy(_) => "El puerto UDP 6454 está ocupado por otro programa \
             (¿otro programa de iluminación abierto?). Podés agregar los nodos a mano."
            .to_string(),
        other => other.to_string(),
    })?;
    let monitor = Arc::new(monitor);
    *slot = Some(Arc::clone(&monitor));
    drop(slot);
    watch_show_nodes(&state);
    Ok(monitor)
}

/// Point the monitor at the running show's nodes (their status is what the UI shows).
pub(crate) fn watch_show_nodes(state: &AppState) {
    let ips: Vec<Ipv4Addr> = state
        .show
        .lock()
        .unwrap()
        .network
        .nodes
        .iter()
        .map(|n| n.ip)
        .collect();
    if let Some(m) = state.artnet_monitor.lock().unwrap().as_ref() {
        m.set_watched(ips);
    }
}

/// A node's reachability for the UI: "online" (responde), "silent" (sin respuesta —
/// neutral, some devices never answer ArtPoll) or "lost" (dejó de responder — alarm).
#[derive(Serialize, Clone)]
pub(crate) struct NodeStatusInfo {
    pub(crate) ip: String,
    pub(crate) status: &'static str,
}

impl NodeStatusInfo {
    pub(crate) fn new(ip: Ipv4Addr, status: NodeStatus) -> Self {
        Self {
            ip: ip.to_string(),
            status: match status {
                NodeStatus::Online => "online",
                NodeStatus::Silent => "silent",
                NodeStatus::Lost => "lost",
            },
        }
    }
}

/// Current status of every node of the running show (initial fill for the Device
/// Manager; later changes arrive as `artnet-node-status` events). Empty if the monitor
/// is not running (port held).
#[tauri::command]
pub fn main_node_statuses(state: State<AppState>) -> Vec<NodeStatusInfo> {
    let Some(monitor) = state.artnet_monitor.lock().unwrap().clone() else {
        return Vec::new();
    };
    let ips: Vec<Ipv4Addr> = state
        .show
        .lock()
        .unwrap()
        .network
        .nodes
        .iter()
        .map(|n| n.ip)
        .collect();
    ips.into_iter()
        .map(|ip| NodeStatusInfo::new(ip, monitor.status(ip)))
        .collect()
}

/// How long a Scan listens for replies (spec §7: ~3 s, with a re-poll halfway).
pub(crate) const SCAN_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// Scan the network for Art-Net nodes (ArtPoll → ArtPollReply), like the classic Scan,
/// through the node monitor (the one owner of UDP 6454). It polls every host
/// interface's subnet broadcast plus 255.255.255.255, so a board on a link-local
/// network (the H802RA at 169.254.x) is found without picking an interface first. Runs off the UI/event thread. The raw replies are also written to
/// `artnet-scan.json` in the app-config dir — real device bytes for the parser tests.
#[tauri::command]
pub async fn main_artnet_scan(app: AppHandle) -> Result<Vec<ScannedNodeInfo>, String> {
    let monitor = ensure_artnet_monitor(&app)?;
    let nodes = tauri::async_runtime::spawn_blocking(move || monitor.scan(SCAN_WINDOW))
        .await
        .map_err(|e| e.to_string())?;
    if let Ok(dir) = app.path().app_config_dir() {
        let dump: Vec<serde_json::Value> = nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "ip": n.ip.to_string(),
                    "short_name": n.short_name,
                    "bind_index": n.bind_index,
                    "reply_hex": n.raw_reply.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                })
            })
            .collect();
        let _ = persist_text(
            &dir.join("artnet-scan.json"),
            &serde_json::to_string_pretty(&dump).unwrap_or_default(),
        );
    }
    Ok(nodes.iter().map(ScannedNodeInfo::from).collect())
}
