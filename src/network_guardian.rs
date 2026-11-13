//! The Network Guardian: "why is my internet lagging?" Pings the router, public resolvers and Sir's
//! game servers in parallel (latency, jitter, loss), names the processes moving the most data, and
//! scans the home network (ARP after a quick sweep) for phones, iPads, Raspberry Pis and the rest.

use crate::features::{powershell, run, show_window, spoken, str_arg};
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct PingStats {
    pub host: String,
    pub label: String,
    pub sent: usize,
    pub times: Vec<f64>,
}

impl PingStats {
    pub fn loss(&self) -> f64 {
        if self.sent == 0 {
            1.0
        } else {
            1.0 - self.times.len() as f64 / self.sent as f64
        }
    }

    pub fn avg(&self) -> Option<f64> {
        (!self.times.is_empty()).then(|| self.times.iter().sum::<f64>() / self.times.len() as f64)
    }

    /// Mean absolute difference between consecutive replies (RFC 3550 style, unsmoothed).
    pub fn jitter(&self) -> Option<f64> {
        (self.times.len() >= 2).then(|| self.times.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (self.times.len() - 1) as f64)
    }

    fn line(&self) -> String {
        match self.avg() {
            Some(avg) => format!(
                "| {} | {} | {:.0} ms | {:.0}–{:.0} ms | {:.1} ms | {:.0} % |",
                self.label,
                self.host,
                avg,
                self.times.iter().cloned().fold(f64::INFINITY, f64::min),
                self.times.iter().cloned().fold(0.0, f64::max),
                self.jitter().unwrap_or(0.0),
                self.loss() * 100.0
            ),
            None => format!("| {} | {} | — | — | — | 100 % |", self.label, self.host),
        }
    }
}

static TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:time|zeit|temps|tiempo)\s*[=<]\s*(\d+(?:[.,]\d+)?)\s*ms").unwrap());

/// Reply times from Windows' `ping` output (any display language).
pub fn parse_ping(output: &str) -> Vec<f64> {
    TIME.captures_iter(output).filter_map(|c| c[1].replace(',', ".").parse().ok()).collect()
}

pub(crate) async fn ping(host: &str, label: &str, count: usize) -> PingStats {
    let n = count.to_string();
    let args: Vec<&str> = if cfg!(windows) { vec!["-n", &n, "-w", "1000", host] } else { vec!["-c", &n, "-W", "1", host] };
    let out = run(Path::new("ping"), &args, None, Duration::from_secs(count as u64 * 2 + 5)).await;
    let times = out.map(|o| parse_ping(&o.stdout)).unwrap_or_default();
    PingStats { host: host.to_string(), label: label.to_string(), sent: count, times }
}

pub(crate) async fn gateway() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    powershell("(Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Sort-Object RouteMetric | Select-Object -First 1).NextHop", Duration::from_secs(10))
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "0.0.0.0")
}

async fn ping_report(tools: &ToolContext) -> (String, String) {
    let mut targets: Vec<(String, String)> = Vec::new();
    if let Some(gw) = gateway().await {
        targets.push((gw, "Router".into()));
    }
    targets.push(("1.1.1.1".into(), "Cloudflare".into()));
    targets.push(("8.8.8.8".into(), "Google DNS".into()));
    let settings = tools.config.read(|c| c.features.clone());
    for server in settings.game_servers.iter().chain(std::iter::once(&settings.minecraft_server)).filter(|s| !s.trim().is_empty()) {
        let host = server.split(':').next().unwrap_or(server).trim().to_string();
        targets.push((host.clone(), "Game server".into()));
    }
    let results = futures_util::future::join_all(targets.iter().map(|(h, l)| ping(h, l, 10))).await;
    let mut md = String::from("### Latency\n| Target | Host | Avg | Range | Jitter | Loss |\n|---|---|---|---|---|---|\n");
    for r in &results {
        md.push_str(&r.line());
        md.push('\n');
    }
    let router = results.iter().find(|r| r.label == "Router");
    let internet = results.iter().filter(|r| r.label != "Router" && r.label != "Game server").min_by(|a, b| a.avg().unwrap_or(9e9).total_cmp(&b.avg().unwrap_or(9e9)));
    let verdict = match (router, internet) {
        (Some(r), _) if r.loss() > 0.15 || r.avg().unwrap_or(999.0) > 20.0 => format!(
            "The trouble is inside the house: the router itself answers in {:.0} ms with {:.0} % loss — Wi-Fi interference or distance. A cable or a closer access point will help.",
            r.avg().unwrap_or(0.0),
            r.loss() * 100.0
        ),
        (_, Some(i)) if i.loss() > 0.05 => format!("The connection beyond the router is losing {:.0} % of packets — likely the provider.", i.loss() * 100.0),
        (_, Some(i)) if i.jitter().unwrap_or(0.0) > 15.0 => format!("Latency is fine on average ({:.0} ms) but jitter is {:.0} ms — something is saturating the line.", i.avg().unwrap_or(0.0), i.jitter().unwrap_or(0.0)),
        (_, Some(i)) if i.avg().is_some() => format!("The connection looks healthy: {:.0} ms to {}, jitter {:.1} ms, no meaningful loss.", i.avg().unwrap_or(0.0), i.label, i.jitter().unwrap_or(0.0)),
        _ => "No reply from the internet at all — the connection is down.".into(),
    };
    (md, verdict)
}

/// The processes moving the most data right now (network + disk I/O) and the busiest connections.
async fn bandwidth_report() -> (String, String) {
    if !cfg!(windows) {
        return (String::new(), "Bandwidth analysis needs Windows.".into());
    }
    let script = r#"$nic = Get-CimInstance Win32_PerfFormattedData_Tcpip_NetworkInterface | Measure-Object -Property BytesReceivedPersec,BytesSentPersec -Sum
"NIC|" + [math]::Round(($nic | Where-Object Property -eq 'BytesReceivedPersec').Sum) + "|" + [math]::Round(($nic | Where-Object Property -eq 'BytesSentPersec').Sum)
Get-CimInstance Win32_PerfFormattedData_PerfProc_Process | Where-Object { $_.Name -ne '_Total' -and $_.Name -ne 'Idle' } | Sort-Object IODataBytesPersec -Descending | Select-Object -First 6 | ForEach-Object { "IO|" + $_.Name + "|" + $_.IODataBytesPersec + "|" + $_.IDProcess }
Get-NetTCPConnection -State Established -ErrorAction SilentlyContinue | Where-Object { $_.RemoteAddress -notmatch '^(127\.|::1|0\.)' } | Group-Object OwningProcess | Sort-Object Count -Descending | Select-Object -First 5 | ForEach-Object { $p = Get-Process -Id $_.Name -ErrorAction SilentlyContinue; "TCP|" + ($(if ($p) { $p.ProcessName } else { $_.Name })) + "|" + $_.Count }"#;
    let out = match powershell(script, Duration::from_secs(25)).await {
        Ok(o) => o,
        Err(e) => return (String::new(), format!("I couldn't read the traffic counters: {e}")),
    };
    let mut md = String::from("### Traffic\n");
    let mut hogs: Vec<(String, f64)> = Vec::new();
    for line in out.lines() {
        let parts: Vec<&str> = line.trim().split('|').collect();
        match parts.as_slice() {
            ["NIC", down, up] => {
                let mb = |s: &str| s.parse::<f64>().unwrap_or(0.0) * 8.0 / 1_000_000.0;
                md.push_str(&format!("Network adapters now: **↓ {:.1} Mbit/s · ↑ {:.1} Mbit/s**\n\n| Process | I/O now |\n|---|---|\n", mb(down), mb(up)));
            }
            ["IO", name, bytes, _pid] => {
                let b = bytes.parse::<f64>().unwrap_or(0.0);
                md.push_str(&format!("| {name} | {:.1} MB/s |\n", b / 1_000_000.0));
                hogs.push((name.to_string(), b));
            }
            ["TCP", name, count] => md.push_str(&format!("\n{name}: {count} open internet connections")),
            _ => {}
        }
    }
    let top = hogs.iter().find(|(n, _)| !matches!(n.as_str(), "System" | "svchost" | "MsMpEng" | "jai" | "JAI"));
    let verdict = match top {
        Some((name, b)) if *b > 1_000_000.0 => format!("{name} is the heaviest right now at {:.1} megabytes a second.", b / 1_000_000.0),
        Some((name, _)) => format!("Nothing is hogging the line; the busiest process is {name}, and it is modest."),
        None => "Nothing is moving much data right now.".into(),
    };
    (md, verdict)
}

/// The first three bytes of a MAC address → maker / kind of device.
pub fn vendor(mac: &str) -> &'static str {
    let m = mac.to_uppercase().replace('-', ":");
    let first = u8::from_str_radix(m.get(0..2).unwrap_or("00"), 16).unwrap_or(0);
    if first & 0x02 != 0 {
        return "private address (phone / tablet with Wi-Fi privacy)";
    }
    let oui = m.get(0..8).unwrap_or("");
    const TABLE: &[(&str, &str)] = &[
        ("B8:27:EB", "Raspberry Pi"),
        ("DC:A6:32", "Raspberry Pi"),
        ("E4:5F:01", "Raspberry Pi"),
        ("D8:3A:DD", "Raspberry Pi"),
        ("2C:CF:67", "Raspberry Pi"),
        ("28:CD:C1", "Raspberry Pi"),
        ("2C:91:AB", "AVM FRITZ!"),
        ("3C:A6:2F", "AVM FRITZ!"),
        ("44:4E:6D", "AVM FRITZ!"),
        ("5C:49:79", "AVM FRITZ!"),
        ("7C:FF:4D", "AVM FRITZ!"),
        ("98:9B:CB", "AVM FRITZ!"),
        ("B0:F2:08", "AVM FRITZ!"),
        ("C8:0E:14", "AVM FRITZ!"),
        ("DC:39:6F", "AVM FRITZ!"),
        ("E0:28:6D", "AVM FRITZ!"),
        ("F0:B0:14", "AVM FRITZ!"),
        ("38:10:D5", "AVM FRITZ!"),
        ("00:1A:11", "Google"),
        ("F4:F5:D8", "Google"),
        ("3C:5A:B4", "Google"),
        ("F0:27:2D", "Amazon"),
        ("74:C2:46", "Amazon"),
        ("FC:A6:67", "Amazon"),
        ("00:17:88", "Philips Hue"),
        ("EC:B5:FA", "Philips Hue"),
        ("24:0A:C4", "Espressif (smart-home)"),
        ("30:AE:A4", "Espressif (smart-home)"),
        ("A4:CF:12", "Espressif (smart-home)"),
        ("C4:4F:33", "Espressif (smart-home)"),
        ("00:D9:D1", "Sony PlayStation"),
        ("70:9E:29", "Sony PlayStation"),
        ("BC:60:A7", "Sony PlayStation"),
        ("98:B6:E9", "Nintendo"),
        ("7C:BB:8A", "Nintendo"),
        ("7C:ED:8D", "Microsoft (Xbox / Surface)"),
        ("C8:3F:26", "Microsoft (Xbox / Surface)"),
        ("F0:18:98", "Apple"),
        ("A4:83:E7", "Apple"),
        ("BC:D0:74", "Apple"),
        ("3C:22:FB", "Apple"),
        ("AC:BC:32", "Apple"),
        ("F4:0F:24", "Apple"),
        ("8C:85:90", "Apple"),
        ("D0:81:7A", "Apple"),
        ("98:01:A7", "Apple"),
        ("5C:F9:38", "Apple"),
        ("F8:FF:C2", "Apple"),
        ("00:E0:4C", "Realtek (PC)"),
        ("D8:BB:C1", "Micro-Star (PC)"),
        ("04:7C:16", "Micro-Star (PC)"),
        ("2C:F0:5D", "Micro-Star (PC)"),
        ("10:7B:44", "ASUSTek (PC)"),
        ("04:D9:F5", "ASUSTek (PC)"),
        ("A8:5E:45", "ASUSTek (PC)"),
        ("F8:32:E4", "ASUSTek (PC)"),
        ("50:EB:F6", "ASUSTek (PC)"),
        ("00:1B:21", "Intel (PC)"),
        ("8C:8D:28", "Intel (PC)"),
        ("A4:C3:F0", "Intel (PC)"),
        ("F4:8C:50", "Intel (PC)"),
        ("98:DA:C4", "TP-Link"),
        ("50:C7:BF", "TP-Link"),
        ("E8:48:B8", "TP-Link"),
        ("8C:F5:A3", "Samsung"),
        ("FC:A1:3E", "Samsung"),
        ("5C:0A:5B", "Samsung"),
        ("84:25:19", "Samsung"),
        ("64:B4:73", "Xiaomi"),
        ("28:6C:07", "Xiaomi"),
    ];
    TABLE.iter().find(|(p, _)| *p == oui).map(|(_, v)| *v).unwrap_or("unknown maker")
}

static ARP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})\s+([0-9a-fA-F]{2}(?:[-:][0-9a-fA-F]{2}){5})\s+(\w+)").unwrap());

/// (ip, mac) of the dynamic ARP entries (`arp -a`, any language).
pub fn parse_arp(output: &str) -> Vec<(String, String)> {
    ARP.captures_iter(output)
        .filter(|c| {
            let mac = c[2].to_lowercase();
            !mac.starts_with("ff-ff") && !mac.starts_with("01-00-5e") && !mac.starts_with("33-33") && !c[1].ends_with(".255")
        })
        .map(|c| (c[1].to_string(), c[2].to_uppercase().replace('-', ":")))
        .collect()
}

async fn lan_report() -> (String, String) {
    let ip = crate::satellite::lan_ip();
    let std::net::IpAddr::V4(v4) = ip else {
        return (String::new(), "The PC has no IPv4 network address.".into());
    };
    let o = v4.octets();
    // Touch every address of the /24 so the ARP table fills (a datagram each; nothing listens).
    if let Ok(sock) = tokio::net::UdpSocket::bind("0.0.0.0:0").await {
        for host in 1..=254u8 {
            if host != o[3] {
                let _ = sock.send_to(&[0u8], (std::net::Ipv4Addr::new(o[0], o[1], o[2], host), 9)).await;
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(1800)).await;
    let arp = run(Path::new("arp"), &["-a"], None, Duration::from_secs(10)).await.map(|o| o.stdout).unwrap_or_default();
    let prefix = format!("{}.{}.{}.", o[0], o[1], o[2]);
    let devices: Vec<(String, String)> = parse_arp(&arp).into_iter().filter(|(ip, _)| ip.starts_with(&prefix)).collect();
    // Names from the router's DNS (FRITZ!Box and most routers know them).
    let names: std::collections::HashMap<String, String> = if cfg!(windows) && !devices.is_empty() {
        let list = devices.iter().map(|(ip, _)| format!("'{ip}'")).collect::<Vec<_>>().join(",");
        let script = format!("foreach ($ip in @({list})) {{ try {{ $n = (Resolve-DnsName -Name $ip -Type PTR -QuickTimeout -ErrorAction Stop | Select-Object -First 1).NameHost; if ($n) {{ \"$ip|$n\" }} }} catch {{ }} }}");
        powershell(&script, Duration::from_secs(25))
            .await
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('|').map(|(a, b)| (a.trim().to_string(), b.trim().trim_end_matches('.').to_string())))
            .collect()
    } else {
        Default::default()
    };
    let mut md = format!("### Devices on {prefix}0/24\n| IP | Name | Maker / kind | MAC |\n|---|---|---|---|\n| {ip} | this PC | — | — |\n");
    let mut kinds: std::collections::BTreeMap<&str, usize> = Default::default();
    for (ip, mac) in &devices {
        let v = vendor(mac);
        let name = names.get(ip).cloned().unwrap_or_default();
        let guess = guess_kind(&name, v);
        *kinds.entry(guess).or_default() += 1;
        md.push_str(&format!("| {ip} | {} | {v} | {mac} |\n", if name.is_empty() { "—" } else { &name }));
    }
    let summary = kinds.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ");
    (md, format!("{} devices besides this PC are on the network{}.", devices.len(), if summary.is_empty() { String::new() } else { format!(": {summary}") }))
}

/// A short noun for the spoken summary.
fn guess_kind(name: &str, vendor: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("iphone") || n.contains("android") || n.contains("galaxy") || n.contains("pixel") || vendor.starts_with("private") {
        "phone or tablet"
    } else if n.contains("ipad") || n.contains("tablet") {
        "tablet"
    } else if n.contains("raspberry") || vendor == "Raspberry Pi" {
        "Raspberry Pi"
    } else if n.contains("fritz") || vendor.starts_with("AVM") || vendor == "TP-Link" {
        "router or repeater"
    } else if vendor.contains("PlayStation") || vendor == "Nintendo" || vendor.contains("Xbox") {
        "console"
    } else if vendor.contains("(PC)") || n.contains("desktop") || n.contains("laptop") {
        "computer"
    } else if vendor.contains("smart-home") || vendor == "Philips Hue" || vendor == "Amazon" || vendor == "Google" {
        "smart-home device"
    } else {
        "other device"
    }
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let action = str_arg(args, &["action", "mode"]).unwrap_or("full").to_lowercase();
    let want = |k: &str| action == "full" || action == "all" || action.contains(k);
    let ping_part = async {
        if want("ping") || action.contains("lag") || action.contains("jitter") {
            Some(ping_report(tools).await)
        } else {
            None
        }
    };
    let bandwidth_part = async {
        if want("bandwidth") || action.contains("hog") || action.contains("traffic") {
            Some(bandwidth_report().await)
        } else {
            None
        }
    };
    let lan_part = async {
        if want("scan") || action.contains("lan") || action.contains("devices") {
            Some(lan_report().await)
        } else {
            None
        }
    };
    let (p, b, l) = tokio::join!(ping_part, bandwidth_part, lan_part);
    let parts: Vec<(String, String)> = [p, b, l].into_iter().flatten().collect();
    if parts.is_empty() {
        return Err(format!("Unknown action '{action}' (ping, bandwidth, scan_lan or full)."));
    }
    let md = parts.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>().join("\n\n");
    let said = parts.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>().join(" ");
    show_window(tools, "network_guardian", "Network Guardian", &format!("{md}\n\n**Verdict:** {said}"), 720.0, 560.0);
    Ok(spoken(format!("{md}\n\nVerdict: {said}"), said))
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "diagnose_network_and_ping",
        "description": "Network Guardian: action ping (latency, jitter and packet loss to the router, the internet and Sir's game servers — 'why am I lagging'), bandwidth (which process is eating the connection), scan_lan (devices on the home network: phones, iPads, Raspberry Pi, consoles) or full.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["ping", "bandwidth", "scan_lan", "full"]}
        }, "required": ["action"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_arp_and_vendors() {
        let german = "Antwort von 1.1.1.1: Bytes=32 Zeit=12ms TTL=57\nAntwort von 1.1.1.1: Bytes=32 Zeit=18ms TTL=57\nZeitüberschreitung der Anforderung.\nAntwort von 1.1.1.1: Bytes=32 Zeit<1ms TTL=57";
        assert_eq!(parse_ping(german), vec![12.0, 18.0, 1.0]);
        let s = PingStats { host: "1.1.1.1".into(), label: "x".into(), sent: 4, times: vec![12.0, 18.0, 15.0] };
        assert_eq!(s.loss(), 0.25);
        assert_eq!(s.jitter(), Some(4.5));
        let arp = "Schnittstelle: 192.168.178.20 --- 0x7\n  Internetadresse      Physische Adresse     Typ\n  192.168.178.1         2c-91-ab-11-22-33     dynamisch\n  192.168.178.40        b8-27-eb-aa-bb-cc     dynamisch\n  192.168.178.255       ff-ff-ff-ff-ff-ff     statisch\n  224.0.0.22            01-00-5e-00-00-16     statisch";
        let devices = parse_arp(arp);
        assert_eq!(devices.len(), 2);
        assert_eq!(vendor(&devices[1].1), "Raspberry Pi");
        assert_eq!(vendor("DA:11:22:33:44:55"), "private address (phone / tablet with Wi-Fi privacy)");
        assert_eq!(guess_kind("iPhone-von-Max", "Apple"), "phone or tablet");
    }
}
