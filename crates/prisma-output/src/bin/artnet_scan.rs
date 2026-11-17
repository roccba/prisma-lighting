//! `artnet-scan` — the Device Manager's Scan from the terminal (docs/DEVICES-SPEC.md §7).
//!
//! Polls every host interface's subnet broadcast plus 255.255.255.255 and prints each
//! node found (replies merged per board), its universes both raw (Art-Net port
//! address, 0-based) and as professional consoles shows them (+1), and with `--hex` the raw
//! `ArtPollReply` bytes — the reference data for the parser tests (CLAUDE.md §7).
//!
//! Usage: `artnet-scan [seconds] [--hex]`  (default 3 s)

use std::net::Ipv4Addr;
use std::time::Duration;

use prisma_output::discovery::scan;
use prisma_output::netif::host_interfaces;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let hex = args.iter().any(|a| a == "--hex");
    let seconds: u64 = args.iter().find_map(|a| a.parse().ok()).unwrap_or(3);

    let hosts = host_interfaces();
    let mut targets: Vec<Ipv4Addr> = hosts.iter().map(|h| h.broadcast()).collect();
    targets.push(Ipv4Addr::BROADCAST);
    targets.dedup();
    println!("interfaces:");
    for h in &hosts {
        println!("  {:<16} {}/{}", h.name, h.address, h.prefix_len);
    }
    println!("polling {targets:?} for {seconds} s…\n");

    match scan(&targets, Duration::from_secs(seconds)) {
        Err(e) => {
            eprintln!("scan failed: {e}");
            std::process::exit(1);
        }
        Ok(nodes) if nodes.is_empty() => println!("no Art-Net nodes answered"),
        Ok(nodes) => {
            for n in &nodes {
                let raw: Vec<String> = n
                    .universe_ranges()
                    .iter()
                    .map(|r| format!("{}–{}", r.first, r.last))
                    .collect();
                let shown: Vec<String> = n
                    .universe_ranges()
                    .iter()
                    .map(|r| format!("{}–{}", r.first + 1, r.last + 1))
                    .collect();
                println!(
                    "{}  {}  \"{}\" / \"{}\"  MAC {}\n  binds {}  universes raw [{}]  as professional consoles [{}]",
                    n.ip,
                    n.short_name,
                    n.short_name,
                    n.long_name,
                    n.mac_string(),
                    n.binds,
                    raw.join(", "),
                    shown.join(", ")
                );
                if hex {
                    for r in &n.raw_replies {
                        println!(
                            "    {}",
                            r.iter().map(|b| format!("{b:02x}")).collect::<String>()
                        );
                    }
                }
            }
        }
    }
}
