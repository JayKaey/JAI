//! NETWORK RADAR ("check the network", "who is on my network?"): the home network swept and
//! drawn as a radar on the board, every device placed by how fast it answers.
//!
//! Everything is native: the active adapter from `GetAdaptersAddresses`, an ICMP echo to every
//! address of the /24 around this PC (`IcmpSendEcho` on a pool of small worker threads), the
//! neighbour table (`GetIpNetTable2`) for MAC addresses and for the devices that ignore pings,
//! reverse DNS (`GetNameInfoW`) in the background. While the radar is up, the router and
//! 1.1.1.1 are pinged once a second (the latency chart); the speed test downloads 25 MB from
//! Cloudflare only when Sir clicks it. Devices seen before are remembered in
//! `config/network_radar_devices.json` (MAC → first and last seen, name, kind), so a newcomer
//! stands out. Under the live self-test and in dry runs nothing touches the network: the radar
//! shows a fixed demo network instead (placeholder addresses only).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

// ------------------------------------------------------------------------------ settings ----

/// The devices seen before (MAC → first and last seen, name, kind).
const STORE_PATH: &str = "config/network_radar_devices.json";
/// The most devices the store remembers (the longest unseen leave first).
const STORE_CAP: usize = 256;
/// The most networks the store remembers.
const NETWORKS_CAP: usize = 16;
/// The most addresses one sweep pings: a bigger network is swept as the /22 around this PC.
const MAX_HOSTS: usize = 1024;
/// How long one echo of the sweep waits for its answer.
const ECHO_TIMEOUT_MS: u32 = 600;
/// Echoes in flight at once.
const SWEEP_WORKERS: usize = 64;
/// Reverse-DNS lookups in flight at once (they run on after the sweep).
const NAME_WORKERS: usize = 6;
/// How long the sweep waits for doubtful neighbours to answer ARP.
const ARP_WAIT: Duration = Duration::from_millis(1200);
/// The most doubtful neighbours checked that way.
const ARP_CHECKS: usize = 32;
/// The stack of the sweep's small worker threads.
const WORKER_STACK: usize = 256 * 1024;
/// A sweep this young answers "show" and "is my network secure?" without a new one.
const FRESH_FOR: Duration = Duration::from_secs(60);
/// The latency chart: one sample a second, the last minute.
pub const LATENCY_SAMPLES: usize = 60;
const MONITOR_EVERY: Duration = Duration::from_secs(1);
const MONITOR_TIMEOUT_MS: u32 = 1000;
/// The internet end of the latency chart (Cloudflare's resolver answers pings everywhere).
pub const INTERNET_HOST: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);
/// The speed test: 25 MB from Cloudflare, only when Sir clicks it.
const SPEED_TEST_URL: &str = "https://speed.cloudflare.com/__down?bytes=25000000";
pub const SPEED_TEST_BYTES: u64 = 25_000_000;
const SPEED_TEST_TIMEOUT: Duration = Duration::from_secs(20);
/// A download cut short still gives a speed from this much on.
const SPEED_TEST_PARTIAL: u64 = 2_000_000;
/// A device first seen this recently (after its network's first sweep) counts as new.
const NEW_FOR_SECS: u64 = 24 * 3600;

// --------------------------------------------------------------------------------- types ----

/// How this PC is connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Ethernet,
    WiFi,
    Other,
}

impl Link {
    /// The adapter card's caption: "ETHERNET", "WI-FI".
    pub fn label(self) -> &'static str {
        match self {
            Self::Ethernet => "ETHERNET",
            Self::WiFi => "WI-FI",
            Self::Other => "NETWORK",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Ethernet => "Ethernet",
            Self::WiFi => "Wi-Fi",
            Self::Other => "network",
        }
    }
}

/// The network adapter the sweep runs on (up, with an IPv4 address and a gateway).
#[derive(Clone, Debug)]
pub struct Adapter {
    pub link: Link,
    /// "Realtek PCIe GbE Family Controller".
    pub description: String,
    /// The link speed in bits per second (0: not reported).
    pub speed_bps: u64,
    pub ip: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Ipv4Addr,
    pub mac: Option<[u8; 6]>,
    if_index: u32,
    metric: u32,
}

impl Adapter {
    /// "192.168.1.0/24".
    pub fn subnet(&self) -> String {
        subnet_text(self.ip, self.prefix)
    }
}

/// What a device most likely is (its glyph and colour on the radar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Router,
    ThisPc,
    Pc,
    Phone,
    Tablet,
    Tv,
    SmartHome,
    Printer,
    Console,
    Unknown,
}

impl Kind {
    /// The list's subline: "Router", "Smart home".
    pub fn label(self) -> &'static str {
        match self {
            Self::Router => "Router",
            Self::ThisPc => "This PC",
            Self::Pc => "Computer",
            Self::Phone => "Phone",
            Self::Tablet => "Tablet",
            Self::Tv => "TV",
            Self::SmartHome => "Smart home",
            Self::Printer => "Printer",
            Self::Console => "Console",
            Self::Unknown => "Device",
        }
    }

    /// In a name or a sentence: "router", "smart home device".
    fn noun(self) -> &'static str {
        match self {
            Self::Router => "router",
            Self::ThisPc | Self::Pc => "computer",
            Self::Phone => "phone",
            Self::Tablet => "tablet",
            Self::Tv => "TV",
            Self::SmartHome => "smart home device",
            Self::Printer => "printer",
            Self::Console => "console",
            Self::Unknown => "device",
        }
    }

    /// The store's word for it.
    fn id(self) -> &'static str {
        match self {
            Self::Router => "router",
            Self::ThisPc => "this_pc",
            Self::Pc => "pc",
            Self::Phone => "phone",
            Self::Tablet => "tablet",
            Self::Tv => "tv",
            Self::SmartHome => "smart_home",
            Self::Printer => "printer",
            Self::Console => "console",
            Self::Unknown => "unknown",
        }
    }
}

/// One device on the network.
#[derive(Clone, Debug)]
pub struct Device {
    pub ip: Ipv4Addr,
    pub mac: Option<[u8; 6]>,
    /// Its own name on the network (reverse DNS, first label); "" when it has none.
    pub host: String,
    /// The maker its MAC address names ("TP-Link"); "" when unknown.
    pub vendor: String,
    pub kind: Kind,
    /// The echo's round trip; None: it ignores pings (found through ARP).
    pub latency_ms: Option<f32>,
    pub this_pc: bool,
    pub gateway: bool,
    /// When the radar first saw it (Unix seconds).
    pub first_seen: Option<u64>,
    /// First seen within the last day, after its network's first sweep.
    pub new: bool,
}

impl Device {
    /// A locally administered MAC: a phone or tablet with Wi-Fi privacy on.
    pub fn private_address(&self) -> bool {
        self.mac.is_some_and(|m| is_private_mac(&m))
    }

    /// "02:11:22:33:44:55" ("" when unknown).
    pub fn mac_text(&self) -> String {
        self.mac.map(|m| fmt_mac(&m)).unwrap_or_default()
    }

    /// "TP-Link router", "Private device ·105", "Tapo smart home".
    pub fn name(&self) -> String {
        let last = self.ip.octets()[3];
        if self.gateway && !self.vendor.is_empty() {
            return format!("{} router", self.vendor);
        }
        if !self.host.is_empty() {
            return self.host.clone();
        }
        if self.this_pc {
            return "This PC".to_string();
        }
        if self.gateway {
            return "Router".to_string();
        }
        if self.private_address() {
            return format!("Private device \u{b7}{last}");
        }
        if !self.vendor.is_empty() {
            return format!("{} {}", self.vendor, self.kind.noun());
        }
        format!("Device \u{b7}{last}")
    }

    /// The list's second line: "Router", "Phone · private address".
    pub fn subline(&self) -> String {
        if self.this_pc {
            return if self.vendor.is_empty() { "This computer".to_string() } else { format!("This computer \u{b7} {}", self.vendor) };
        }
        let kind = self.kind.label();
        if self.private_address() {
            return format!("{kind} \u{b7} private address");
        }
        if !self.vendor.is_empty() && !self.name().starts_with(&self.vendor) {
            return format!("{kind} \u{b7} {}", self.vendor);
        }
        kind.to_string()
    }

    /// The name as JAI says it ("Private device 105").
    fn spoken_name(&self) -> String {
        self.name().replace(" \u{b7}", " ").replace('\u{b7}', " ")
    }
}

/// Where the sweep stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Nothing swept yet (or the sweep stopped because the radar closed).
    Idle,
    Locating,
    Sweeping,
    Done,
    /// No adapter with a gateway.
    Failed,
}

/// The speed test (only on Sir's click).
#[derive(Clone, Debug)]
pub enum Speed {
    Idle,
    Running { bytes: u64, started: Instant },
    Done { mbps: f32, bytes: u64, secs: f32 },
    Failed(String),
}

/// One ping a second to the router or to the internet, the last minute of them.
#[derive(Clone, Debug, Default)]
pub struct Series {
    /// Oldest first; None: no answer.
    pub samples: VecDeque<Option<f32>>,
}

impl Series {
    fn push(&mut self, sample: Option<f32>) {
        while self.samples.len() >= LATENCY_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    /// The latest answer among the last three pings.
    pub fn current(&self) -> Option<f32> {
        self.samples.iter().rev().take(3).flatten().next().copied()
    }

    /// Lost pings in percent.
    pub fn loss(&self) -> Option<f32> {
        loss_percent(self.samples.iter().copied())
    }

    /// The mean change between consecutive answers.
    pub fn jitter(&self) -> Option<f32> {
        jitter_ms(self.samples.iter().copied())
    }
}

/// What the radar knows right now (the GUI draws a copy of it every frame).
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// The opening of the radar this belongs to.
    pub session: u64,
    /// Bumped with every sweep.
    pub generation: u64,
    pub phase: Phase,
    pub adapter: Option<Adapter>,
    /// The swept range: "192.168.1.0/24".
    pub scope: String,
    /// The router first, this PC next, then in the order they answered.
    pub devices: Vec<Device>,
    pub probed: usize,
    pub total: usize,
    /// How long the sweep took, seconds.
    pub took: Option<f32>,
    finished_at: Option<Instant>,
    pub error: Option<String>,
    /// This was the first sweep of this network: every device was noted as known.
    pub baseline: bool,
    pub router: Series,
    pub internet: Series,
    pub speed: Speed,
    demo: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            session: 0,
            generation: 0,
            phase: Phase::Idle,
            adapter: None,
            scope: String::new(),
            devices: Vec::new(),
            probed: 0,
            total: 0,
            took: None,
            finished_at: None,
            error: None,
            baseline: false,
            router: Series::default(),
            internet: Series::default(),
            speed: Speed::Idle,
            demo: false,
        }
    }
}

impl Snapshot {
    pub fn sweeping(&self) -> bool {
        matches!(self.phase, Phase::Locating | Phase::Sweeping)
    }

    fn fresh(&self) -> bool {
        self.phase == Phase::Done && self.finished_at.is_some_and(|t| t.elapsed() < FRESH_FOR)
    }

    /// The router's latency: the last pings, else the sweep's echo.
    pub fn router_latency(&self) -> Option<f32> {
        self.router.current().or_else(|| self.devices.iter().find(|d| d.gateway).and_then(|d| d.latency_ms))
    }

    pub fn internet_latency(&self) -> Option<f32> {
        self.internet.current()
    }
}

// ------------------------------------------------------------------------------- state ----

static OPEN: AtomicBool = AtomicBool::new(false);
/// Bumped whenever the radar opens: the sweep, the pings and the speed test of an earlier
/// opening stop.
static SESSION: AtomicU64 = AtomicU64::new(0);
/// The opening the running sweep belongs to.
static SWEEP_SESSION: AtomicU64 = AtomicU64::new(u64::MAX);
/// The opening the latency pings run for.
static MONITOR_SESSION: AtomicU64 = AtomicU64::new(u64::MAX);
static STATE: LazyLock<Mutex<Snapshot>> = LazyLock::new(|| Mutex::new(Snapshot::default()));
/// What JAI says when the running sweep ends: (summary, security verdict).
static ANNOUNCE: Mutex<(bool, bool)> = Mutex::new((false, false));
/// Held while the store is read, changed and written back (the sweep's end and the names that
/// arrive after it), so neither overwrites the other.
static STORE_LOCK: Mutex<()> = Mutex::new(());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn state() -> MutexGuard<'static, Snapshot> {
    lock(&STATE)
}

/// Writes into the state if `generation` is still the current sweep. True when it did.
fn update(generation: u64, f: impl FnOnce(&mut Snapshot)) -> bool {
    let mut s = state();
    if s.generation != generation {
        return false;
    }
    f(&mut s);
    true
}

/// A copy of what the radar knows (drawn every frame).
pub fn snapshot() -> Snapshot {
    let mut s = state().clone();
    s.session = SESSION.load(Ordering::Acquire);
    s
}

/// The live self-test, a dry run and the unit tests never touch the network.
fn gated() -> bool {
    cfg!(test) || crate::live_self_tester::running() || crate::live_self_tester::dry()
}

/// The radar is up (a block on the board, or its own window).
pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// Closes the radar (its `[ CLOSE ]`, a right-click on it, "close the network radar"). The
/// sweep, the pings and a running speed test stop.
pub fn close() {
    OPEN.store(false, Ordering::Release);
}

fn open() {
    if !OPEN.swap(true, Ordering::AcqRel) {
        SESSION.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Show,
    Rescan,
    Security,
}

impl Action {
    fn parse(raw: &str) -> Self {
        let raw = raw.trim().to_lowercase().replace(['-', ' '], "_");
        match raw.as_str() {
            "rescan" | "refresh" | "scan_again" | "sweep_again" | "again" => Self::Rescan,
            "check_security" | "security" | "secure" | "is_secure" | "check" => Self::Security,
            _ => Self::Show,
        }
    }
}

/// `open_jai_view {"view": "network_radar", "action": …}`: `show` opens the radar and sweeps
/// (unless a sweep is under a minute old), `rescan` sweeps again, `check_security` answers
/// whether every device is known (sweeping first when it has to). The result is the display
/// text and a "Spoken:" line; what a sweep finds later is said through `announcements`.
pub async fn run(args: &Value) -> Result<String, String> {
    let action = Action::parse(args.get("action").and_then(Value::as_str).unwrap_or("show"));
    open();
    if gated() {
        show_demo();
        let snap = snapshot();
        let spoken = match action {
            Action::Show => summary_line(&snap),
            Action::Rescan => "Sweeping your network again, Sir.".to_string(),
            Action::Security => security_verdict(&snap.devices, snap.baseline),
        };
        crate::gui::show();
        crate::gui::refresh();
        return Ok(format!("{}\nSpoken: {spoken}", summary_text(&snap)));
    }
    let snap = snapshot();
    let mine = snap.sweeping() && SWEEP_SESSION.load(Ordering::Acquire) == snap.session;
    let fresh = snap.fresh() && !snap.demo;
    let spoken = match action {
        Action::Show if mine => {
            wish(true, false);
            "The sweep is already under way, Sir.".to_string()
        }
        Action::Show if fresh => summary_line(&snap),
        Action::Show => {
            start_sweep(true, false);
            "I'll take a look at your network right away, Sir.".to_string()
        }
        Action::Rescan if mine => "I'm already sweeping your network, Sir.".to_string(),
        Action::Rescan => {
            start_sweep(true, false);
            "Sweeping your network again, Sir.".to_string()
        }
        Action::Security if fresh => security_verdict(&snap.devices, snap.baseline),
        Action::Security => {
            if mine {
                wish(false, true);
            } else {
                start_sweep(false, true);
            }
            "I'm checking every device on your network now, Sir. One moment.".to_string()
        }
    };
    ensure_monitor();
    crate::gui::show();
    crate::gui::refresh();
    Ok(format!("{}\nSpoken: {spoken}", summary_text(&snapshot())))
}

/// The radar's own RESCAN: a new sweep unless one is running (no summary is spoken afterwards,
/// only a device he has not seen before).
pub fn rescan() {
    if gated() {
        show_demo();
        crate::gui::refresh();
        return;
    }
    let snap = snapshot();
    if snap.sweeping() && SWEEP_SESSION.load(Ordering::Acquire) == snap.session {
        return;
    }
    start_sweep(false, false);
}

/// Adds to what JAI says when the running sweep ends.
fn wish(summary: bool, security: bool) {
    let mut a = lock(&ANNOUNCE);
    a.0 |= summary;
    a.1 |= security;
}

fn show_demo() {
    let mut s = state();
    let mut demo = demo_snapshot();
    demo.generation = s.generation + 1;
    *s = demo;
}

fn start_sweep(summary: bool, security: bool) {
    *lock(&ANNOUNCE) = (summary, security);
    let session = SESSION.load(Ordering::Acquire);
    SWEEP_SESSION.store(session, Ordering::Release);
    let generation = {
        let mut s = state();
        let speed = std::mem::replace(&mut s.speed, Speed::Idle);
        let (router, internet) = (std::mem::take(&mut s.router), std::mem::take(&mut s.internet));
        let generation = s.generation + 1;
        *s = Snapshot { generation, phase: Phase::Locating, router, internet, speed, ..Snapshot::default() };
        generation
    };
    if let Err(e) = std::thread::Builder::new().name("radar-sweep".into()).spawn(move || sweep(generation, session)) {
        warn!("[RADAR] Could not start the sweep: {e}");
        update(generation, |s| {
            s.phase = Phase::Failed;
            s.error = Some("The sweep could not start".into());
        });
    }
    crate::gui::refresh();
}

// ------------------------------------------------------------------------------- sweep ----

/// What every discovery needs to know.
struct Net<'a> {
    generation: u64,
    gateway: Ipv4Addr,
    /// MAC → the name it had last time (shown until reverse DNS answers again).
    known: &'a BTreeMap<String, String>,
    names: &'a mpsc::Sender<Ipv4Addr>,
}

fn sweep(generation: u64, session: u64) {
    let started = Instant::now();
    let alive = || is_open() && SESSION.load(Ordering::Acquire) == session && state().generation == generation;
    let Some(adapter) = pick_adapter(native::adapters()) else {
        update(generation, |s| {
            s.phase = Phase::Failed;
            s.error = Some("No active network connection".into());
        });
        crate::gui::refresh();
        announce(generation);
        return;
    };
    let prefix = sweep_prefix(adapter.prefix, MAX_HOSTS);
    let hosts = sweep_hosts(adapter.ip, prefix);
    let known: BTreeMap<String, String> = load_store().devices.into_iter().filter(|(_, k)| !k.host.is_empty()).map(|(mac, k)| (mac, k.host)).collect();
    let range = subnet_text(adapter.ip, prefix);
    info!("[RADAR] Sweeping {range} ({} addresses) over {} ({})", hosts.len(), adapter.link.word(), adapter.description);
    let this_pc = this_pc_device(&adapter);
    let begun = update(generation, |s| {
        s.phase = Phase::Sweeping;
        s.adapter = Some(adapter.clone());
        s.scope = range.clone();
        s.total = hosts.len();
        s.probed = 0;
        s.devices = vec![this_pc];
    });
    if !begun {
        return;
    }
    crate::gui::refresh();

    let (names, wanted) = mpsc::channel::<Ipv4Addr>();
    spawn_resolvers(generation, wanted);
    let net = Net { generation, gateway: adapter.gateway, known: &known, names: &names };
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..SWEEP_WORKERS.min(hosts.len()) {
            let worker = || {
                let Some(icmp) = native::Icmp::open() else { return };
                while alive() {
                    let Some(&ip) = hosts.get(next.fetch_add(1, Ordering::Relaxed)) else { break };
                    if ip != adapter.ip {
                        if let Some(ms) = icmp.echo(ip, ECHO_TIMEOUT_MS) {
                            found(&net, ip, native::neighbour_mac(adapter.if_index, ip), Some(ms));
                        }
                    }
                    update(generation, |s| s.probed = (s.probed + 1).min(s.total));
                }
            };
            if std::thread::Builder::new().name("radar-echo".into()).stack_size(WORKER_STACK).spawn_scoped(scope, worker).is_err() {
                break;
            }
        }
    });
    if !alive() {
        update(generation, |s| s.phase = Phase::Idle);
        return;
    }
    // The devices that ignore pings: they answered the ARP the echoes sent (the neighbour
    // table has them as reachable), or answer one now.
    let in_range = |ip: Ipv4Addr| hosts.binary_search(&ip).is_ok();
    let mut doubtful = Vec::new();
    for (ip, mac, reach) in native::neighbours(adapter.if_index) {
        if ip == adapter.ip || !in_range(ip) {
            continue;
        }
        match reach {
            Reach::Yes => found(&net, ip, Some(mac), None),
            Reach::Maybe => doubtful.push(ip),
        }
    }
    let present: Vec<Ipv4Addr> = state().devices.iter().map(|d| d.ip).collect();
    doubtful.retain(|ip| !present.contains(ip));
    for (ip, mac) in check_arp(adapter.if_index, doubtful) {
        found(&net, ip, Some(mac), None);
    }
    // Echo answers whose MAC the table did not have yet.
    let missing: Vec<Ipv4Addr> = state().devices.iter().filter(|d| d.mac.is_none() && !d.this_pc).map(|d| d.ip).collect();
    for ip in missing {
        if let Some(mac) = native::neighbour_mac(adapter.if_index, ip) {
            found(&net, ip, Some(mac), None);
        }
    }
    drop(names);
    if !alive() {
        update(generation, |s| s.phase = Phase::Idle);
        return;
    }
    let now = unix_now();
    let store_guard = lock(&STORE_LOCK);
    let mut store = load_store();
    let finished = update(generation, |s| {
        let network = network_key(&adapter, &s.devices);
        s.baseline = remember(&mut s.devices, &mut store, &network, now);
        s.phase = Phase::Done;
        s.probed = s.total;
        s.took = Some(started.elapsed().as_secs_f32());
        s.finished_at = Some(Instant::now());
    });
    if !finished {
        return;
    }
    save_store(&store);
    drop(store_guard);
    info!("[RADAR] Sweep of {range} done in {:.1} s: {} devices", started.elapsed().as_secs_f32(), state().devices.len());
    crate::gui::refresh();
    announce(generation);
}

/// A device answered (or the neighbour table knows it): added, or its MAC / latency filled in.
fn found(net: &Net, ip: Ipv4Addr, mac: Option<[u8; 6]>, latency: Option<f32>) {
    let mac = mac.filter(usable_mac);
    let mut added = false;
    let changed = update(net.generation, |s| {
        if let Some(d) = s.devices.iter_mut().find(|d| d.ip == ip) {
            if d.latency_ms.is_none() {
                d.latency_ms = latency;
            }
            if d.mac.is_none() && mac.is_some() {
                d.mac = mac;
                d.vendor = clean_vendor(raw_vendor(mac));
                if d.host.is_empty() {
                    d.host = known_host(net.known, mac);
                }
                d.kind = kind_of(d);
            }
            return;
        }
        let mut device = Device {
            ip,
            mac,
            host: known_host(net.known, mac),
            vendor: clean_vendor(raw_vendor(mac)),
            kind: Kind::Unknown,
            latency_ms: latency,
            this_pc: false,
            gateway: ip == net.gateway,
            first_seen: None,
            new: false,
        };
        device.kind = kind_of(&device);
        place(&mut s.devices, device);
        added = true;
    });
    if added {
        let _ = net.names.send(ip);
    }
    if changed {
        crate::gui::refresh();
    }
}

/// The router first, this PC next, everything else in the order it was found.
fn place(devices: &mut Vec<Device>, device: Device) {
    let at = if device.gateway {
        0
    } else if device.this_pc {
        devices.iter().take_while(|d| d.gateway).count()
    } else {
        devices.len()
    };
    devices.insert(at, device);
}

fn known_host(known: &BTreeMap<String, String>, mac: Option<[u8; 6]>) -> String {
    mac.and_then(|m| known.get(&fmt_mac(&m)).cloned()).unwrap_or_default()
}

fn raw_vendor(mac: Option<[u8; 6]>) -> &'static str {
    mac.map_or("", |m| crate::network_guardian::vendor(&fmt_mac(&m)))
}

fn kind_of(d: &Device) -> Kind {
    guess_kind(&d.host, raw_vendor(d.mac), d.gateway, d.this_pc, d.private_address())
}

fn this_pc_device(adapter: &Adapter) -> Device {
    let host = std::env::var("COMPUTERNAME").map(|n| clean_host(&n)).unwrap_or_default();
    Device {
        ip: adapter.ip,
        mac: adapter.mac,
        host,
        vendor: clean_vendor(raw_vendor(adapter.mac)),
        kind: Kind::ThisPc,
        latency_ms: Some(0.0),
        this_pc: true,
        gateway: false,
        first_seen: None,
        new: false,
    }
}

/// Reverse DNS on a few threads; they end when the sweep drops its sender. The last one out
/// writes the names into the store.
fn spawn_resolvers(generation: u64, wanted: mpsc::Receiver<Ipv4Addr>) {
    let wanted = Arc::new(Mutex::new(wanted));
    let live = Arc::new(AtomicUsize::new(NAME_WORKERS));
    for _ in 0..NAME_WORKERS {
        let (queue, running) = (Arc::clone(&wanted), Arc::clone(&live));
        let worker = move || {
            loop {
                let next = lock(&queue).recv();
                let Ok(ip) = next else { break };
                if state().generation != generation {
                    continue;
                }
                let Some(host) = native::reverse_name(ip).map(|n| clean_host(&n)).filter(|h| !h.is_empty()) else { continue };
                let named = update(generation, |s| {
                    if let Some(d) = s.devices.iter_mut().find(|d| d.ip == ip && !d.this_pc) {
                        d.host = host;
                        d.kind = kind_of(d);
                    }
                });
                if named {
                    crate::gui::refresh();
                }
            }
            if running.fetch_sub(1, Ordering::AcqRel) == 1 {
                store_hosts(generation);
            }
        };
        if std::thread::Builder::new().name("radar-names".into()).stack_size(WORKER_STACK).spawn(worker).is_err() {
            live.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// The names that arrived after the sweep ended, into the store.
fn store_hosts(generation: u64) {
    let _store = lock(&STORE_LOCK);
    let snap = snapshot();
    if snap.generation != generation || snap.phase != Phase::Done || gated() {
        return;
    }
    let mut store = load_store();
    let mut changed = false;
    for d in snap.devices.iter().filter(|d| !d.host.is_empty() && !d.this_pc) {
        if let Some(known) = d.mac.and_then(|m| store.devices.get_mut(&fmt_mac(&m))) {
            if known.host != d.host {
                known.host = d.host.clone();
                known.name = d.name();
                known.kind = d.kind.id().to_string();
                changed = true;
            }
        }
    }
    if changed {
        save_store(&store);
    }
}

/// Whether a neighbour entry stands for a live device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    /// Reachable (it just answered ARP) or a permanent entry.
    Yes,
    /// Stale or being probed: it may have gone.
    Maybe,
}

/// Neighbours that may have gone, asked once more by ARP (in parallel, `ARP_WAIT` at most).
fn check_arp(if_index: u32, ips: Vec<Ipv4Addr>) -> Vec<(Ipv4Addr, [u8; 6])> {
    if ips.is_empty() {
        return Vec::new();
    }
    let (tx, rx) = mpsc::channel();
    for ip in ips.into_iter().take(ARP_CHECKS) {
        let tx = tx.clone();
        let asked = std::thread::Builder::new().name("radar-arp".into()).stack_size(WORKER_STACK).spawn(move || {
            if let Some(mac) = native::resolve(if_index, ip) {
                let _ = tx.send((ip, mac));
            }
        });
        if asked.is_err() {
            break;
        }
    }
    drop(tx);
    let deadline = Instant::now() + ARP_WAIT;
    let mut answered = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(hit) => answered.push(hit),
            Err(_) => break,
        }
    }
    answered
}

/// Says the summary or the security verdict the request asked for (and a device he has not
/// seen before in any case).
fn announce(generation: u64) {
    let (summary, security) = std::mem::take(&mut *lock(&ANNOUNCE));
    if gated() {
        return;
    }
    let snap = snapshot();
    if snap.generation != generation {
        return;
    }
    let line = match snap.phase {
        Phase::Failed if summary || security => Some("I couldn't find an active network connection to sweep, Sir.".to_string()),
        Phase::Done if security => Some(security_verdict(&snap.devices, snap.baseline)),
        Phase::Done if summary => Some(summary_line(&snap)),
        Phase::Done if snap.devices.iter().any(|d| d.new) => Some(security_verdict(&snap.devices, snap.baseline)),
        _ => None,
    };
    if let Some(line) = line {
        crate::announcements::say(line);
    }
}

// ----------------------------------------------------------------------- latency, speed ----

/// The once-a-second pings to the router and to 1.1.1.1 while the radar is open.
pub fn ensure_monitor() {
    if gated() || !is_open() {
        return;
    }
    let session = SESSION.load(Ordering::Acquire);
    if MONITOR_SESSION.swap(session, Ordering::AcqRel) == session {
        return;
    }
    {
        let mut s = state();
        s.router = Series::default();
        s.internet = Series::default();
    }
    for router in [true, false] {
        if let Err(e) = std::thread::Builder::new().name("radar-latency".into()).stack_size(WORKER_STACK).spawn(move || watch(router, session)) {
            warn!("[RADAR] Could not start the latency pings: {e}");
        }
    }
}

fn watch(router: bool, session: u64) {
    let Some(icmp) = native::Icmp::open() else { return };
    let live = || is_open() && SESSION.load(Ordering::Acquire) == session;
    while live() {
        let tick = Instant::now();
        let target = if router { state().adapter.as_ref().map(|a| a.gateway) } else { Some(INTERNET_HOST) };
        if let Some(ip) = target {
            let sample = icmp.echo(ip, MONITOR_TIMEOUT_MS);
            if !live() {
                break;
            }
            let mut s = state();
            if router {
                s.router.push(sample);
            } else {
                s.internet.push(sample);
            }
        }
        std::thread::sleep(MONITOR_EVERY.saturating_sub(tick.elapsed()).max(Duration::from_millis(20)));
    }
}

/// The SPEED TEST tab's button: 25 MB from Cloudflare (never on its own, never in a test).
pub fn start_speed_test(rt: &tokio::runtime::Handle) {
    if gated() {
        state().speed = Speed::Done { mbps: 942.0, bytes: SPEED_TEST_BYTES, secs: 0.21 };
        return;
    }
    let started = Instant::now();
    {
        let mut s = state();
        if matches!(s.speed, Speed::Running { .. }) {
            return;
        }
        s.speed = Speed::Running { bytes: 0, started };
    }
    let session = SESSION.load(Ordering::Acquire);
    rt.spawn(async move {
        let result = speed_test(session, started).await;
        let speed = match result {
            Ok((bytes, secs)) => {
                info!("[RADAR] Speed test: {bytes} bytes in {secs:.2} s");
                Speed::Done { mbps: mbps(bytes, secs), bytes, secs }
            }
            Err(e) if e == "stopped" => Speed::Idle,
            Err(e) => {
                warn!("[RADAR] Speed test failed: {e}");
                Speed::Failed(e)
            }
        };
        {
            let mut s = state();
            if own_run(&s.speed, started) {
                s.speed = speed;
            }
        }
        crate::gui::refresh();
    });
}

/// Whether the speed test shown is the run that began at `started`.
fn own_run(speed: &Speed, started: Instant) -> bool {
    matches!(speed, Speed::Running { started: at, .. } if *at == started)
}

async fn speed_test(session: u64, started_at: Instant) -> Result<(u64, f32), String> {
    let client = reqwest::Client::builder().timeout(SPEED_TEST_TIMEOUT).build().map_err(|e| e.to_string())?;
    let mut response = client.get(SPEED_TEST_URL).send().await.map_err(|e| short_error(&e))?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let started = Instant::now();
    let mut bytes = 0u64;
    loop {
        if !is_open() || SESSION.load(Ordering::Acquire) != session {
            return Err("stopped".into());
        }
        match response.chunk().await {
            Ok(Some(chunk)) => {
                bytes += chunk.len() as u64;
                let mut s = state();
                if own_run(&s.speed, started_at) {
                    s.speed = Speed::Running { bytes, started: started_at };
                }
            }
            Ok(None) => break,
            Err(e) if bytes >= SPEED_TEST_PARTIAL => {
                warn!("[RADAR] Speed test cut short after {bytes} bytes: {}", short_error(&e));
                break;
            }
            Err(e) => return Err(short_error(&e)),
        }
    }
    Ok((bytes, started.elapsed().as_secs_f32().max(0.001)))
}

fn short_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "no connection".into()
    } else {
        "download failed".into()
    }
}

// ------------------------------------------------------------------------------- store ----

#[derive(Clone, Default, Serialize, Deserialize)]
struct Known {
    #[serde(default)]
    first_seen: u64,
    #[serde(default)]
    last_seen: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    host: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    kind: String,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Seen {
    #[serde(default)]
    first_seen: u64,
    #[serde(default)]
    last_seen: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    /// The networks swept so far (by the router's MAC).
    #[serde(default)]
    networks: BTreeMap<String, Seen>,
    /// MAC → when it was first and last seen.
    #[serde(default)]
    devices: BTreeMap<String, Known>,
}

fn load_store() -> Store {
    std::fs::read_to_string(STORE_PATH).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn save_store(store: &Store) {
    if cfg!(test) || gated() || crate::platform::in_test_session() {
        return;
    }
    let Ok(text) = serde_json::to_string_pretty(store) else { return };
    let path = std::path::Path::new(STORE_PATH);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temp = path.with_extension("json.tmp");
    let written = std::fs::write(&temp, text).and_then(|_| std::fs::rename(&temp, path));
    if let Err(e) = written {
        warn!("[RADAR] Could not save {STORE_PATH}: {e}");
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The network a sweep ran on: its router's MAC (its address if the MAC is unknown).
fn network_key(adapter: &Adapter, devices: &[Device]) -> String {
    devices.iter().find(|d| d.gateway).and_then(|d| d.mac).map(|m| fmt_mac(&m)).unwrap_or_else(|| format!("gateway {}", adapter.gateway))
}

/// Notes every device in the store and marks the newcomers. True when this was the network's
/// first sweep (everything on it counts as known from now on).
fn remember(devices: &mut [Device], store: &mut Store, network: &str, now: u64) -> bool {
    let baseline = !store.networks.contains_key(network);
    let since = {
        let seen = store.networks.entry(network.to_string()).or_insert(Seen { first_seen: now, last_seen: now });
        seen.last_seen = now;
        seen.first_seen
    };
    for d in devices.iter_mut() {
        let Some(mac) = d.mac else { continue };
        let known = store.devices.entry(fmt_mac(&mac)).or_insert_with(|| Known { first_seen: now, last_seen: now, ..Known::default() });
        known.last_seen = now;
        if !d.host.is_empty() && !d.this_pc {
            known.host = d.host.clone();
        }
        known.name = d.name();
        known.kind = d.kind.id().to_string();
        d.first_seen = Some(known.first_seen);
        d.new = !baseline && !d.this_pc && known.first_seen > since && now.saturating_sub(known.first_seen) < NEW_FOR_SECS;
    }
    prune(&mut store.devices, STORE_CAP, |k| k.last_seen);
    prune(&mut store.networks, NETWORKS_CAP, |s| s.last_seen);
    baseline
}

fn prune<T>(map: &mut BTreeMap<String, T>, cap: usize, last_seen: impl Fn(&T) -> u64) {
    while map.len() > cap {
        let Some(oldest) = map.iter().min_by_key(|(_, v)| last_seen(v)).map(|(k, _)| k.clone()) else { break };
        map.remove(&oldest);
    }
}

// ------------------------------------------------------------------------- pure helpers ----

fn mask(prefix: u8) -> u32 {
    match prefix.min(32) {
        0 => 0,
        p => u32::MAX << (32 - u32::from(p)),
    }
}

/// "192.168.1.0/24".
pub fn subnet_text(ip: Ipv4Addr, prefix: u8) -> String {
    format!("{}/{}", Ipv4Addr::from(u32::from(ip) & mask(prefix)), prefix.min(32))
}

/// The prefix the sweep uses: the network's own, or the smallest one around this PC that holds
/// at most `cap` hosts.
fn sweep_prefix(prefix: u8, cap: usize) -> u8 {
    let mut p = prefix.min(32);
    while p < 30 && (1usize << (32 - p)) - 2 > cap {
        p += 1;
    }
    p
}

/// Every host address of the network around `ip` (this PC included), ascending.
fn sweep_hosts(ip: Ipv4Addr, prefix: u8) -> Vec<Ipv4Addr> {
    let me = u32::from(ip);
    let m = mask(prefix);
    let (first, last) = (me & m, me | !m);
    if prefix >= 31 {
        // A point-to-point /31 has two usable addresses, a /32 only itself.
        return (first..=last).map(Ipv4Addr::from).collect();
    }
    (first + 1..last).map(Ipv4Addr::from).collect()
}

/// A locally administered unicast MAC (Wi-Fi privacy: a random address per network).
pub fn is_private_mac(mac: &[u8; 6]) -> bool {
    mac[0] & 0x02 != 0 && mac[0] & 0x01 == 0
}

/// A MAC a device can have: not all zeros, not multicast or broadcast.
fn usable_mac(mac: &[u8; 6]) -> bool {
    mac.iter().any(|b| *b != 0) && mac[0] & 0x01 == 0
}

/// "02:11:22:33:44:55".
pub fn fmt_mac(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

fn fnv(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(*b)).wrapping_mul(0x0100_0193))
}

/// Where a device stands on the scope, degrees clockwise from north: the router at the top,
/// every other device at an angle of its own that stays the same from sweep to sweep.
pub fn device_angle(d: &Device) -> f32 {
    if d.gateway {
        return 0.0;
    }
    let hash = match d.mac {
        Some(mac) => fnv(&mac),
        None => fnv(&d.ip.octets()),
    };
    (hash % 3600) as f32 / 10.0
}

/// How far out a device stands, as a share of the scope's radius: the faster it answers, the
/// closer to the centre (the rings stand at 1, 3, 10 and 30 ms). No answer: the outer edge.
pub fn radius_fraction(ms: Option<f32>) -> f32 {
    match ms {
        Some(ms) if ms.is_finite() && ms < 1.0 => 0.12 + 0.08 * ms.max(0.0),
        Some(ms) if ms.is_finite() => (0.2 + 0.4 * ms.log10()).min(0.95),
        _ => 0.95,
    }
}

/// "<1 ms", "4.0 ms", "18 ms", "--".
pub fn fmt_ms(ms: Option<f32>) -> String {
    match ms {
        Some(ms) if ms.is_finite() && ms < 1.0 => "<1 ms".to_string(),
        Some(ms) if ms.is_finite() && ms < 10.0 => format!("{ms:.1} ms"),
        Some(ms) if ms.is_finite() => format!("{ms:.0} ms"),
        _ => "--".to_string(),
    }
}

/// A link speed: "1 Gbps", "2.5 Gbps", "867 Mbps" ("" when unknown).
pub fn fmt_speed(bps: u64) -> String {
    if bps == 0 {
        return String::new();
    }
    if bps >= 1_000_000_000 {
        let gbps = bps as f64 / 1e9;
        return if (gbps - gbps.round()).abs() < 0.05 { format!("{:.0} Gbps", gbps.round()) } else { format!("{gbps:.1} Gbps") };
    }
    format!("{:.0} Mbps", bps as f64 / 1e6)
}

/// A measured speed: "942 Mbps", "87.5 Mbps".
pub fn fmt_mbps(mbps: f32) -> String {
    if mbps >= 100.0 {
        format!("{mbps:.0} Mbps")
    } else {
        format!("{mbps:.1} Mbps")
    }
}

fn mbps(bytes: u64, secs: f32) -> f32 {
    bytes as f32 * 8.0 / secs.max(0.001) / 1e6
}

/// Lost pings in percent (None without any).
fn loss_percent(samples: impl IntoIterator<Item = Option<f32>>) -> Option<f32> {
    let (mut total, mut lost) = (0usize, 0usize);
    for s in samples {
        total += 1;
        lost += usize::from(s.is_none());
    }
    (total > 0).then(|| lost as f32 * 100.0 / total as f32)
}

/// Jitter: the mean absolute change between consecutive answers (None without two in a row).
fn jitter_ms(samples: impl IntoIterator<Item = Option<f32>>) -> Option<f32> {
    let (mut sum, mut pairs, mut previous) = (0.0f32, 0usize, None::<f32>);
    for s in samples {
        if let (Some(a), Some(b)) = (previous, s) {
            sum += (b - a).abs();
            pairs += 1;
        }
        previous = s;
    }
    (pairs > 0).then(|| sum / pairs as f32)
}

fn number_word(n: usize) -> String {
    const WORDS: [&str; 21] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen",
        "nineteen", "twenty",
    ];
    WORDS.get(n).map_or_else(|| n.to_string(), |w| (*w).to_string())
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn devices_phrase(n: usize) -> String {
    if n == 1 {
        "one device".to_string()
    } else {
        format!("{} devices", number_word(n))
    }
}

fn with_article(noun: &str) -> String {
    let vowel = noun.chars().next().is_some_and(|c| "AEIOUaeiou".contains(c));
    format!("{} {noun}", if vowel { "an" } else { "a" })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Quality {
    Excellent,
    Good,
    Fair,
    Poor,
}

fn quality(router_ms: Option<f32>, internet_ms: Option<f32>, loss: f32) -> Quality {
    let (router, internet) = (router_ms.unwrap_or(1.0), internet_ms.unwrap_or(20.0));
    if loss < 1.0 && router <= 2.0 && internet <= 30.0 {
        Quality::Excellent
    } else if loss < 3.0 && router <= 10.0 && internet <= 60.0 {
        Quality::Good
    } else if loss < 10.0 && router <= 40.0 && internet <= 150.0 {
        Quality::Fair
    } else {
        Quality::Poor
    }
}

/// What JAI says when a sweep is done: "Your network is performing excellently, Sir: six
/// devices, connected by a fast Ethernet cable with very low latency."
fn spoken_summary(link: Link, speed_bps: u64, devices: usize, router_ms: Option<f32>, internet_ms: Option<f32>, loss: f32, new: &[String]) -> String {
    let state = match quality(router_ms, internet_ms, loss) {
        Quality::Excellent => "performing excellently",
        Quality::Good => "in good shape",
        Quality::Fair => "running a little slowly",
        Quality::Poor => "struggling",
    };
    let connection = match link {
        Link::Ethernet if speed_bps >= 1_000_000_000 => "connected by a fast Ethernet cable".to_string(),
        Link::Ethernet if speed_bps > 0 => format!("connected by an Ethernet cable at {} megabits", speed_bps / 1_000_000),
        Link::Ethernet => "connected by an Ethernet cable".to_string(),
        Link::WiFi => "connected over Wi-Fi".to_string(),
        Link::Other => "connected to the network".to_string(),
    };
    let latency = match router_ms {
        Some(ms) if ms < 2.0 => " with very low latency",
        Some(ms) if ms < 10.0 => " with low latency",
        Some(ms) if ms < 40.0 => " with noticeable latency",
        Some(_) => " with high latency",
        None => "",
    };
    let mut line = format!("Your network is {state}, Sir: {}, {connection}{latency}.", devices_phrase(devices));
    if loss >= 3.0 {
        line.push_str(&format!(" I'm seeing {} percent packet loss to the internet.", loss.round() as u32));
    }
    match new {
        [] => {}
        [one] => line.push_str(&format!(" One of them is new: {one}.")),
        many => line.push_str(&format!(" {} of them are new to me.", capitalised(&number_word(many.len())))),
    }
    line
}

/// The spoken summary of a snapshot.
fn summary_line(snap: &Snapshot) -> String {
    let Some(adapter) = &snap.adapter else {
        return if snap.phase == Phase::Failed { "I couldn't find an active network connection, Sir.".to_string() } else { "I haven't found your network yet, Sir.".to_string() };
    };
    let new: Vec<String> = snap.devices.iter().filter(|d| d.new).map(Device::spoken_name).collect();
    let mut line = spoken_summary(adapter.link, adapter.speed_bps, snap.devices.len(), snap.router_latency(), snap.internet_latency(), snap.internet.loss().unwrap_or(0.0), &new);
    if let Some(hint) = held_back(adapter) {
        line.push(' ');
        line.push_str(&hint);
    }
    line
}

/// A gigabit network card linked at 100 megabits or less: a cable or a switch port holds it
/// back (worth saying: the fix is cheap). Only when its name says it can do more.
fn held_back(adapter: &Adapter) -> Option<String> {
    let name = adapter.description.to_ascii_lowercase();
    let gigabit = ["gbe", "gigabit", "1000", "2.5g", "10g"].iter().any(|w| name.contains(w));
    (adapter.link == Link::Ethernet && gigabit && adapter.speed_bps > 0 && adapter.speed_bps <= 100_000_000)
        .then(|| format!("Your network card can do a full gigabit, though; a cable or a switch port is holding it at {} megabits.", adapter.speed_bps / 1_000_000))
}

/// "Is my network secure?": every device known, or the newcomers by name.
fn security_verdict(devices: &[Device], baseline: bool) -> String {
    let n = devices.len();
    if baseline {
        let all = match n {
            0 | 1 => "the one device on it".to_string(),
            2 => "both devices on it".to_string(),
            _ => format!("all {} devices on it", number_word(n)),
        };
        return format!("This is my first sweep of this network, Sir, so I've noted {all} as known. From now on I'll point out any newcomer.");
    }
    let new: Vec<&Device> = devices.iter().filter(|d| d.new).collect();
    match new.as_slice() {
        [] => match n {
            0 | 1 => "Everything is secure, Sir. There's nothing on the network but this PC.".to_string(),
            2 => "Everything is secure, Sir. Both devices are recognised and accounted for.".to_string(),
            _ => format!("Everything is secure, Sir. All {} devices are recognised and accounted for.", number_word(n)),
        },
        [one] => {
            let what = if one.private_address() {
                format!("{} using a private address", with_article(one.kind.noun()))
            } else if !one.vendor.is_empty() {
                with_article(&format!("{} {}", one.vendor, one.kind.noun()))
            } else {
                with_article(one.kind.noun())
            };
            let guest = if one.private_address() && one.kind == Kind::Phone { " Most likely a guest's phone." } else { "" };
            format!("One device I haven't seen before, Sir: {}, at {}, {what}.{guest} If you don't recognise it, change your Wi-Fi password.", one.spoken_name(), one.ip)
        }
        many => {
            let mut names: Vec<String> = many.iter().take(3).map(|d| d.spoken_name()).collect();
            let more = many.len().saturating_sub(3);
            let list = if more > 0 {
                format!("{} and {} more", names.join(", "), number_word(more))
            } else {
                let last = names.pop().unwrap_or_default();
                format!("{} and {last}", names.join(", "))
            };
            format!("{} devices I haven't seen before, Sir: {list}. If you don't recognise them, change your Wi-Fi password.", capitalised(&number_word(many.len())))
        }
    }
}

/// The radar as text: the tool's result and what the block's search and Copy read.
pub fn summary_text(snap: &Snapshot) -> String {
    let mut out = String::from("Network radar");
    match (&snap.adapter, snap.phase) {
        (_, Phase::Failed) => {
            out.push_str(": no active network connection.");
            return out;
        }
        (None, _) => {
            out.push_str(": locating the network.");
            return out;
        }
        (Some(a), _) => {
            let speed = fmt_speed(a.speed_bps);
            out.push_str(&format!(" \u{b7} {} \u{b7} gateway {} \u{b7} {}", a.subnet(), a.gateway, a.link.word()));
            if !speed.is_empty() {
                out.push_str(&format!(" {speed}"));
            }
            if !a.description.is_empty() {
                out.push_str(&format!(" ({})", a.description));
            }
        }
    }
    match snap.phase {
        Phase::Done => out.push_str(&format!("\nSwept in {:.1} s: {} online.", snap.took.unwrap_or(0.0), devices_phrase(snap.devices.len()))),
        Phase::Sweeping | Phase::Locating => out.push_str(&format!("\nSweeping: {} of {} addresses, {} so far.", snap.probed, snap.total, devices_phrase(snap.devices.len()))),
        _ => {}
    }
    for d in snap.devices.iter().take(30) {
        let mut row = format!("\n- {} \u{b7} {}", d.name(), d.ip);
        if d.this_pc {
            row.push_str(" \u{b7} this PC");
        } else {
            row.push_str(&format!(" \u{b7} {} \u{b7} {}", fmt_ms(d.latency_ms), d.subline()));
        }
        if d.new {
            row.push_str(" \u{b7} new");
        }
        out.push_str(&row);
    }
    if snap.devices.len() > 30 {
        out.push_str(&format!("\n- and {} more", snap.devices.len() - 30));
    }
    let mut tail = format!("\nRouter {} \u{b7} internet {}", fmt_ms(snap.router_latency()), fmt_ms(snap.internet_latency()));
    if let Some(loss) = snap.internet.loss() {
        tail.push_str(&format!(" \u{b7} loss {loss:.0}%"));
    }
    if let Some(jitter) = snap.internet.jitter() {
        tail.push_str(&format!(" \u{b7} jitter {jitter:.1} ms"));
    }
    if let Speed::Done { mbps, .. } = &snap.speed {
        tail.push_str(&format!(" \u{b7} speed test {}", fmt_mbps(*mbps)));
    }
    out.push_str(&tail);
    out
}

fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// What a device most likely is, from its name and its maker.
fn guess_kind(host: &str, raw_vendor: &str, gateway: bool, this_pc: bool, private: bool) -> Kind {
    if this_pc {
        return Kind::ThisPc;
    }
    if gateway {
        return Kind::Router;
    }
    let name = host.to_lowercase();
    let words = tokens(host);
    let word = |list: &[&str]| words.iter().any(|w| list.iter().any(|k| w == k || (k.len() >= 4 && w.starts_with(k))));
    let part = |list: &[&str]| list.iter().any(|k| name.contains(k));
    if part(&["iphone", "android", "galaxy", "pixel", "redmi", "oneplus", "huawei", "xperia", "motorola", "nokia"]) || word(&["phone", "smartphone", "handy", "mobile"]) {
        return Kind::Phone;
    }
    if part(&["ipad", "tablet", "kindle"]) || word(&["tab"]) {
        return Kind::Tablet;
    }
    if part(&["bravia", "roku", "chromecast", "firetv", "appletv", "webos", "tizen", "androidtv", "googletv"]) || word(&["tv", "television", "fernseher", "shield"]) {
        return Kind::Tv;
    }
    if part(&["printer", "laserjet", "officejet", "deskjet", "epson", "kyocera", "lexmark", "xerox", "brother"]) || word(&["hp", "canon", "drucker", "mfp"]) {
        return Kind::Printer;
    }
    if part(&["playstation", "xbox", "nintendo"]) || word(&["ps4", "ps5", "switch", "steamdeck"]) {
        return Kind::Console;
    }
    if part(&["desktop", "laptop", "notebook", "macbook", "imac", "thinkpad", "workstation", "synology", "qnap", "raspberry"]) || word(&["pc", "nas", "server", "mac", "pi"]) {
        return Kind::Pc;
    }
    if part(&["tapo", "tasmota", "shelly", "espressif", "sonoff", "tuya", "nanoleaf", "meross", "homematic", "homepod", "alexa", "smarthome", "esphome"])
        || word(&["esp", "plug", "bulb", "lamp", "light", "camera", "cam", "smart", "echo", "nest", "hue", "kasa", "wled", "ring", "wiz"])
    {
        return Kind::SmartHome;
    }
    if private {
        return Kind::Phone;
    }
    let maker = raw_vendor.to_lowercase();
    if maker.contains("playstation") || maker.contains("nintendo") {
        Kind::Console
    } else if maker.contains("smart-home") || maker.contains("hue") || maker.contains("tp-link") || maker.contains("amazon") || maker.contains("google") {
        Kind::SmartHome
    } else if maker.contains("(pc)") || maker.contains("raspberry") {
        Kind::Pc
    } else if maker.contains("avm") {
        Kind::Router
    } else {
        Kind::Unknown
    }
}

/// "Realtek (PC)" → "Realtek"; "" for an unknown maker or a private address.
fn clean_vendor(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if raw.is_empty() || lower.starts_with("unknown") || lower.starts_with("private") {
        return String::new();
    }
    raw.split(" (").next().unwrap_or(raw).trim().to_string()
}

/// A reverse-DNS answer as a name: its first label ("" for an address or a DHCP placeholder).
fn clean_host(raw: &str) -> String {
    let raw = raw.trim().trim_end_matches('.');
    if raw.is_empty() || raw.parse::<Ipv4Addr>().is_ok() {
        return String::new();
    }
    let first = raw.split('.').next().unwrap_or("").trim();
    let lower = first.to_lowercase();
    if first.is_empty() || first.chars().all(|c| c.is_ascii_digit() || c == '-') || lower.starts_with("dhcp") || lower.starts_with("unknown") || lower == "localhost" {
        return String::new();
    }
    first.chars().take(32).collect()
}

fn virtual_adapter(description: &str) -> bool {
    let d = description.to_lowercase();
    ["virtual", "vpn", "hyper-v", "vmware", "virtualbox", "tap-", "tunnel", "wireguard", "loopback", "pseudo"].iter().any(|w| d.contains(w))
}

/// The adapter Windows routes through: a real one first, then the lowest metric, the fastest.
fn pick_adapter(adapters: Vec<Adapter>) -> Option<Adapter> {
    adapters.into_iter().min_by_key(|a| (virtual_adapter(&a.description), a.link == Link::Other, a.metric, u64::MAX - a.speed_bps))
}

/// The fixed network the radar shows under the live self-test and in dry runs: a router and
/// five devices on placeholder addresses.
pub fn demo_snapshot() -> Snapshot {
    const SEEN: u64 = 1_790_000_000;
    let ip = |last: u8| Ipv4Addr::new(192, 168, 1, last);
    let mac = |last: u8| [0x00, 0x11, 0x22, 0x33, 0x44, last];
    let adapter = Adapter {
        link: Link::Ethernet,
        description: "Realtek PCIe GbE Family Controller".into(),
        speed_bps: 1_000_000_000,
        ip: ip(20),
        prefix: 24,
        gateway: ip(1),
        mac: Some(mac(0x20)),
        if_index: 0,
        metric: 0,
    };
    let device = |last: u8, mac: [u8; 6], host: &str, vendor: &str, kind: Kind, ms: f32| Device {
        ip: ip(last),
        mac: Some(mac),
        host: host.to_string(),
        vendor: vendor.to_string(),
        kind,
        latency_ms: Some(ms),
        this_pc: kind == Kind::ThisPc,
        gateway: last == 1,
        first_seen: Some(SEEN),
        new: false,
    };
    let devices = vec![
        device(1, mac(0x01), "", "TP-Link", Kind::Router, 0.6),
        device(20, mac(0x20), "MAX-DESKTOP", "Realtek", Kind::ThisPc, 0.0),
        device(105, [0x02, 0x11, 0x22, 0x33, 0x44, 0x55], "", "", Kind::Phone, 3.2),
        device(50, mac(0x50), "Tapo smart home", "TP-Link", Kind::SmartHome, 18.0),
        device(103, [0x06, 0x11, 0x22, 0x33, 0x44, 0x03], "", "", Kind::Phone, 5.5),
        device(104, [0x0A, 0x11, 0x22, 0x33, 0x44, 0x04], "", "", Kind::Phone, 42.0),
    ];
    const ROUTER: [f32; 6] = [0.4, 0.5, 0.3, 0.6, 0.4, 0.5];
    const INTERNET: [f32; 10] = [3.2, 5.8, 4.1, 2.9, 6.3, 3.6, 4.9, 2.7, 5.4, 4.0];
    let mut router = Series::default();
    let mut internet = Series::default();
    for i in 0..LATENCY_SAMPLES {
        router.push(Some(ROUTER[i % ROUTER.len()]));
        internet.push(Some(if i == 41 { 14.5 } else { INTERNET[i % INTERNET.len()] }));
    }
    Snapshot {
        session: 0,
        generation: 1,
        phase: Phase::Done,
        scope: adapter.subnet(),
        adapter: Some(adapter),
        devices,
        probed: 254,
        total: 254,
        took: Some(3.0),
        finished_at: Some(Instant::now()),
        error: None,
        baseline: false,
        router,
        internet,
        speed: Speed::Idle,
        demo: true,
    }
}

// ------------------------------------------------------------------------------ native ----

/// The Win32 calls: IP Helper and WinSock, no external programs.
#[cfg(windows)]
mod native {
    use super::{Adapter, Link, Reach};
    use std::net::Ipv4Addr;
    use std::sync::Once;
    use std::time::Instant;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetAdaptersAddresses, GetIpNetEntry2, GetIpNetTable2, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ResolveIpNetEntry2, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST,
        GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, ICMP_ECHO_REPLY, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
        IP_ADAPTER_GATEWAY_ADDRESS_LH, IP_ADAPTER_UNICAST_ADDRESS_LH, IP_SUCCESS, MIB_IPNET_ROW2, MIB_IPNET_TABLE2,
    };
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::{
        socklen_t, GetNameInfoW, NlnsDelay, NlnsPermanent, NlnsProbe, NlnsReachable, NlnsStale, WSAStartup, AF_INET, IN_ADDR, IN_ADDR_0, NI_NAMEREQD, SOCKADDR, SOCKADDR_IN, WSADATA,
    };

    const ERROR_BUFFER_OVERFLOW: u32 = 111;

    /// The adapters that are up with an IPv4 address and a default gateway.
    pub fn adapters() -> Vec<Adapter> {
        let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size: u32 = 32 * 1024;
        for _ in 0..4 {
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            // SAFETY: the buffer holds `size` bytes, is 8-aligned and outlives the walk.
            match unsafe { GetAdaptersAddresses(u32::from(AF_INET.0), flags, None, Some(first), &mut size) } {
                0 => return unsafe { walk(first) },
                ERROR_BUFFER_OVERFLOW => continue,
                _ => return Vec::new(),
            }
        }
        Vec::new()
    }

    unsafe fn walk(mut node: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<Adapter> {
        let mut list = Vec::new();
        while let Some(a) = node.as_ref() {
            node = a.Next;
            if a.OperStatus != IfOperStatusUp || a.IfType == IF_TYPE_SOFTWARE_LOOPBACK {
                continue;
            }
            let Some((ip, prefix)) = unicast(a.FirstUnicastAddress) else { continue };
            let Some(gateway) = gateway(a.FirstGatewayAddress) else { continue };
            let link = match a.IfType {
                IF_TYPE_ETHERNET_CSMACD => Link::Ethernet,
                IF_TYPE_IEEE80211 => Link::WiFi,
                _ => Link::Other,
            };
            let description = if a.Description.is_null() { String::new() } else { a.Description.to_string().unwrap_or_default() };
            let p = a.PhysicalAddress;
            let mac = (a.PhysicalAddressLength == 6).then_some([p[0], p[1], p[2], p[3], p[4], p[5]]);
            let speed_bps = if a.TransmitLinkSpeed == u64::MAX { 0 } else { a.TransmitLinkSpeed };
            list.push(Adapter { link, description, speed_bps, ip, prefix, gateway, mac, if_index: a.Anonymous1.Anonymous.IfIndex, metric: a.Ipv4Metric });
        }
        list
    }

    unsafe fn unicast(mut node: *const IP_ADAPTER_UNICAST_ADDRESS_LH) -> Option<(Ipv4Addr, u8)> {
        while let Some(u) = node.as_ref() {
            node = u.Next;
            if let Some(ip) = ipv4(u.Address.lpSockaddr) {
                if !ip.is_link_local() {
                    return Some((ip, u.OnLinkPrefixLength));
                }
            }
        }
        None
    }

    unsafe fn gateway(mut node: *const IP_ADAPTER_GATEWAY_ADDRESS_LH) -> Option<Ipv4Addr> {
        while let Some(g) = node.as_ref() {
            node = g.Next;
            if let Some(ip) = ipv4(g.Address.lpSockaddr) {
                return Some(ip);
            }
        }
        None
    }

    unsafe fn ipv4(address: *const SOCKADDR) -> Option<Ipv4Addr> {
        let sa = address.as_ref()?;
        if sa.sa_family != AF_INET {
            return None;
        }
        let sin = &*address.cast::<SOCKADDR_IN>();
        let ip = Ipv4Addr::from(sin.sin_addr.S_un.S_addr.to_ne_bytes());
        (!ip.is_unspecified()).then_some(ip)
    }

    fn sockaddr(ip: Ipv4Addr) -> SOCKADDR_IN {
        SOCKADDR_IN { sin_family: AF_INET, sin_port: 0, sin_addr: IN_ADDR { S_un: IN_ADDR_0 { S_addr: u32::from_ne_bytes(ip.octets()) } }, sin_zero: [0; 8] }
    }

    /// An ICMP handle of one worker thread.
    pub struct Icmp(HANDLE);

    impl Icmp {
        pub fn open() -> Option<Self> {
            unsafe { IcmpCreateFile() }.ok().map(Self)
        }

        /// One echo: the round trip in ms (below 1 ms measured here), None without an answer.
        pub fn echo(&self, ip: Ipv4Addr, timeout_ms: u32) -> Option<f32> {
            const PAYLOAD: &[u8; 32] = b"abcdefghijklmnopqrstuvwabcdefghi";
            let mut reply = [0u64; 32];
            let target = u32::from_ne_bytes(ip.octets());
            let started = Instant::now();
            // SAFETY: the payload and the 8-aligned reply buffer outlive the call; the sizes
            // are theirs.
            let answers = unsafe {
                IcmpSendEcho(self.0, target, PAYLOAD.as_ptr().cast(), PAYLOAD.len() as u16, None, reply.as_mut_ptr().cast(), std::mem::size_of_val(&reply) as u32, timeout_ms)
            };
            let took = started.elapsed().as_secs_f32() * 1000.0;
            if answers == 0 {
                return None;
            }
            // SAFETY: an answer leaves an ICMP_ECHO_REPLY at the start of the buffer.
            let echo = unsafe { reply.as_ptr().cast::<ICMP_ECHO_REPLY>().read() };
            if echo.Status != IP_SUCCESS || echo.Address != target {
                return None;
            }
            let rtt = echo.RoundTripTime as f32;
            Some(if took >= rtt && took < rtt + 1.0 { took } else { rtt })
        }
    }

    impl Drop for Icmp {
        fn drop(&mut self) {
            let _ = unsafe { IcmpCloseHandle(self.0) };
        }
    }

    fn mac_of(row: &MIB_IPNET_ROW2) -> Option<[u8; 6]> {
        if row.PhysicalAddressLength != 6 {
            return None;
        }
        let p = &row.PhysicalAddress;
        let mac = [p[0], p[1], p[2], p[3], p[4], p[5]];
        super::usable_mac(&mac).then_some(mac)
    }

    /// The IPv4 neighbour table of one interface: address, MAC, whether it is alive.
    pub fn neighbours(if_index: u32) -> Vec<(Ipv4Addr, [u8; 6], Reach)> {
        let mut table: *mut MIB_IPNET_TABLE2 = std::ptr::null_mut();
        if unsafe { GetIpNetTable2(AF_INET, &mut table) }.0 != 0 || table.is_null() {
            return Vec::new();
        }
        let mut list = Vec::new();
        // SAFETY: the table holds `NumEntries` rows from `Table` on; it is freed once, below.
        unsafe {
            let count = (*table).NumEntries as usize;
            let rows = std::ptr::addr_of!((*table).Table).cast::<MIB_IPNET_ROW2>();
            for i in 0..count {
                let row = &*rows.add(i);
                if row.InterfaceIndex != if_index || row.Address.si_family != AF_INET {
                    continue;
                }
                let Some(mac) = mac_of(row) else { continue };
                let reach = match row.State {
                    s if s == NlnsReachable || s == NlnsPermanent => Reach::Yes,
                    s if s == NlnsStale || s == NlnsDelay || s == NlnsProbe => Reach::Maybe,
                    _ => continue,
                };
                list.push((Ipv4Addr::from(row.Address.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()), mac, reach));
            }
            FreeMibTable(table.cast_const().cast());
        }
        list
    }

    fn row_for(if_index: u32, ip: Ipv4Addr) -> MIB_IPNET_ROW2 {
        let mut row = MIB_IPNET_ROW2::default();
        row.Address.Ipv4 = sockaddr(ip);
        row.InterfaceIndex = if_index;
        row
    }

    /// The MAC the neighbour table has for `ip` (an echo answer just put it there).
    pub fn neighbour_mac(if_index: u32, ip: Ipv4Addr) -> Option<[u8; 6]> {
        let mut row = row_for(if_index, ip);
        (unsafe { GetIpNetEntry2(&mut row) }.0 == 0).then(|| mac_of(&row)).flatten()
    }

    /// Asks `ip` by ARP once more (the entry is flushed and resolved again): its MAC if it is
    /// there. Blocks until it answers or gives up.
    pub fn resolve(if_index: u32, ip: Ipv4Addr) -> Option<[u8; 6]> {
        let mut row = row_for(if_index, ip);
        (unsafe { ResolveIpNetEntry2(&mut row, None) }.0 == 0).then(|| mac_of(&row)).flatten()
    }

    /// The name `ip` has on the network (reverse DNS, LLMNR, NetBIOS: whatever Windows asks).
    pub fn reverse_name(ip: Ipv4Addr) -> Option<String> {
        static WINSOCK: Once = Once::new();
        WINSOCK.call_once(|| {
            let mut data = WSADATA::default();
            let _ = unsafe { WSAStartup(0x0202, &mut data) };
        });
        let address = sockaddr(ip);
        let mut host = [0u16; 1025];
        // SAFETY: `address` is a whole SOCKADDR_IN; the buffer's length goes with it.
        let code = unsafe {
            GetNameInfoW(std::ptr::addr_of!(address).cast::<SOCKADDR>(), socklen_t(std::mem::size_of::<SOCKADDR_IN>() as i32), Some(&mut host[..]), None, NI_NAMEREQD as i32)
        };
        if code != 0 {
            return None;
        }
        let len = host.iter().position(|c| *c == 0).unwrap_or(host.len());
        let name = String::from_utf16_lossy(&host[..len]);
        (!name.is_empty()).then_some(name)
    }
}

#[cfg(not(windows))]
mod native {
    use super::{Adapter, Reach};
    use std::net::Ipv4Addr;

    pub fn adapters() -> Vec<Adapter> {
        Vec::new()
    }

    pub struct Icmp;

    impl Icmp {
        pub fn open() -> Option<Self> {
            None
        }

        pub fn echo(&self, _ip: Ipv4Addr, _timeout_ms: u32) -> Option<f32> {
            None
        }
    }

    pub fn neighbours(_if_index: u32) -> Vec<(Ipv4Addr, [u8; 6], Reach)> {
        Vec::new()
    }

    pub fn neighbour_mac(_if_index: u32, _ip: Ipv4Addr) -> Option<[u8; 6]> {
        None
    }

    pub fn resolve(_if_index: u32, _ip: Ipv4Addr) -> Option<[u8; 6]> {
        None
    }

    pub fn reverse_name(_ip: Ipv4Addr) -> Option<String> {
        None
    }
}

// ------------------------------------------------------------------------------- tests ----

#[cfg(test)]
mod tests {
    use super::*;

    fn device(last: u8, mac: [u8; 6]) -> Device {
        Device {
            ip: Ipv4Addr::new(192, 168, 1, last),
            mac: Some(mac),
            host: String::new(),
            vendor: String::new(),
            kind: Kind::Unknown,
            latency_ms: Some(2.0),
            this_pc: false,
            gateway: false,
            first_seen: None,
            new: false,
        }
    }

    fn adapter(description: &str, link: Link, metric: u32) -> Adapter {
        Adapter {
            link,
            description: description.to_string(),
            speed_bps: 1_000_000_000,
            ip: Ipv4Addr::new(192, 168, 1, 20),
            prefix: 24,
            gateway: Ipv4Addr::new(192, 168, 1, 1),
            mac: None,
            if_index: 1,
            metric,
        }
    }

    #[test]
    fn the_sweep_covers_the_network_around_this_pc() {
        let me = Ipv4Addr::new(192, 168, 1, 20);
        assert_eq!(subnet_text(me, 24), "192.168.1.0/24");
        assert_eq!(sweep_prefix(24, MAX_HOSTS), 24);
        let hosts = sweep_hosts(me, 24);
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts.first(), Some(&Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(hosts.last(), Some(&Ipv4Addr::new(192, 168, 1, 254)));
        assert!(hosts.contains(&me));
        assert!(hosts.windows(2).all(|w| w[0] < w[1]), "ascending, for the binary search");
        // A /16 is swept as the /22 around this PC: never more than the cap.
        let far = Ipv4Addr::new(10, 0, 5, 7);
        let prefix = sweep_prefix(16, MAX_HOSTS);
        assert_eq!(prefix, 22);
        let hosts = sweep_hosts(far, prefix);
        assert_eq!(hosts.len(), 1022);
        assert!(hosts.len() <= MAX_HOSTS);
        assert!(hosts.contains(&far));
        assert_eq!(hosts.first(), Some(&Ipv4Addr::new(10, 0, 4, 1)));
        assert_eq!(subnet_text(far, prefix), "10.0.4.0/22");
        // The small ends.
        assert_eq!(sweep_hosts(Ipv4Addr::new(10, 0, 0, 1), 30).len(), 2);
        assert_eq!(sweep_hosts(Ipv4Addr::new(10, 0, 0, 1), 31).len(), 2);
        assert_eq!(sweep_hosts(Ipv4Addr::new(10, 0, 0, 1), 32), vec![Ipv4Addr::new(10, 0, 0, 1)]);
        assert_eq!(mask(0), 0);
    }

    #[test]
    fn a_private_address_is_a_phone_named_by_its_last_octet() {
        let mut phone = device(105, [0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
        assert!(phone.private_address());
        phone.kind = kind_of(&phone);
        assert_eq!(phone.kind, Kind::Phone);
        assert_eq!(phone.name(), "Private device \u{b7}105");
        assert_eq!(phone.subline(), "Phone \u{b7} private address");
        assert_eq!(phone.mac_text(), "02:11:22:33:44:55");
        assert_eq!(phone.spoken_name(), "Private device 105");
        assert!(!is_private_mac(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]));
        // The multicast bit makes it no device address at all.
        assert!(!is_private_mac(&[0x03, 0x11, 0x22, 0x33, 0x44, 0x55]));
        assert!(!usable_mac(&[0xFF; 6]));
        assert!(!usable_mac(&[0; 6]));
        assert!(usable_mac(&[0x02, 0x11, 0x22, 0x33, 0x44, 0x55]));
    }

    #[test]
    fn devices_are_named_from_what_is_known_about_them() {
        let mut router = device(1, [0x00, 0x11, 0x22, 0x33, 0x44, 0x01]);
        router.gateway = true;
        router.vendor = "TP-Link".into();
        router.kind = kind_of(&router);
        assert_eq!((router.name().as_str(), router.subline().as_str(), router.kind), ("TP-Link router", "Router", Kind::Router));
        let mut plug = device(50, [0x00, 0x11, 0x22, 0x33, 0x44, 0x50]);
        plug.host = "Tapo smart home".into();
        assert_eq!(plug.name(), "Tapo smart home");
        let unknown = device(77, [0x00, 0x11, 0x22, 0x33, 0x44, 0x77]);
        assert_eq!(unknown.name(), "Device \u{b7}77");
        let mut maker = unknown.clone();
        maker.vendor = "Apple".into();
        assert_eq!(maker.name(), "Apple device");
    }

    #[test]
    fn the_kind_comes_from_the_name_then_the_maker() {
        assert_eq!(guess_kind("MAX-DESKTOP", "", false, false, false), Kind::Pc);
        assert_eq!(guess_kind("Max-iPhone", "", false, false, false), Kind::Phone);
        assert_eq!(guess_kind("tapo-plug", "", false, false, false), Kind::SmartHome);
        assert_eq!(guess_kind("LG-webOS-TV", "", false, false, false), Kind::Tv);
        assert_eq!(guess_kind("HP-LaserJet-Pro", "", false, false, false), Kind::Printer);
        assert_eq!(guess_kind("Max-iPad", "", false, false, false), Kind::Tablet);
        assert_eq!(guess_kind("", "Espressif (smart-home)", false, false, false), Kind::SmartHome);
        assert_eq!(guess_kind("", "Sony PlayStation", false, false, false), Kind::Console);
        assert_eq!(guess_kind("", "Realtek (PC)", false, false, false), Kind::Pc);
        assert_eq!(guess_kind("", "unknown maker", false, false, false), Kind::Unknown);
        assert_eq!(guess_kind("", "", false, false, true), Kind::Phone);
        assert_eq!(guess_kind("anything", "", true, false, false), Kind::Router);
        assert_eq!(guess_kind("anything", "", false, true, false), Kind::ThisPc);
        // "home" alone is no smart-home word ("home-pc" is a computer).
        assert_eq!(guess_kind("home-pc", "", false, false, false), Kind::Pc);
        assert_eq!(clean_vendor("Realtek (PC)"), "Realtek");
        assert_eq!(clean_vendor("AVM FRITZ!"), "AVM FRITZ!");
        assert_eq!(clean_vendor("unknown maker"), "");
        assert_eq!(clean_vendor("private address (phone / tablet with Wi-Fi privacy)"), "");
        assert_eq!(clean_host("MAX-DESKTOP.fritz.box."), "MAX-DESKTOP");
        assert_eq!(clean_host("192.168.1.5"), "");
        assert_eq!(clean_host("dhcp-192-168-1-5"), "");
        assert_eq!(clean_host(""), "");
    }

    #[test]
    fn faster_devices_stand_closer_to_the_centre() {
        let samples = [0.0, 0.3, 0.9, 1.0, 1.5, 3.0, 7.0, 10.0, 25.0, 30.0, 80.0];
        let fractions: Vec<f32> = samples.iter().map(|ms| radius_fraction(Some(*ms))).collect();
        assert!(fractions.windows(2).all(|w| w[0] <= w[1]), "{fractions:?}");
        assert!((radius_fraction(Some(0.999)) - radius_fraction(Some(1.0))).abs() < 0.01, "no jump at 1 ms");
        assert!((radius_fraction(Some(1.0)) - 0.2).abs() < 1e-4);
        assert!((radius_fraction(Some(10.0)) - 0.6).abs() < 1e-4);
        assert_eq!(radius_fraction(None), 0.95);
        assert_eq!(radius_fraction(Some(f32::NAN)), 0.95);
        assert!(radius_fraction(Some(10_000.0)) <= 0.95);
        assert!(radius_fraction(Some(0.0)) > 0.0);
    }

    #[test]
    fn every_device_keeps_its_angle() {
        let a = device(105, [0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
        let b = device(103, [0x06, 0x11, 0x22, 0x33, 0x44, 0x03]);
        assert_eq!(device_angle(&a), device_angle(&a.clone()));
        // The angle goes with the MAC, not with the address DHCP gave it this time.
        let moved = Device { ip: Ipv4Addr::new(192, 168, 1, 150), ..a.clone() };
        assert_eq!(device_angle(&a), device_angle(&moved));
        assert_ne!(device_angle(&a), device_angle(&b));
        for d in [&a, &b] {
            assert!((0.0..360.0).contains(&device_angle(d)));
        }
        let router = Device { gateway: true, ..b };
        assert_eq!(device_angle(&router), 0.0);
    }

    #[test]
    fn loss_and_jitter_come_from_the_pings() {
        assert_eq!(loss_percent([Some(1.0), None, Some(3.0), Some(2.0)]), Some(25.0));
        assert_eq!(loss_percent(std::iter::empty()), None);
        assert_eq!(jitter_ms([Some(1.0), Some(3.0), Some(2.0)]), Some(1.5));
        // A lost ping breaks the pair.
        assert_eq!(jitter_ms([Some(1.0), None, Some(5.0)]), None);
        let mut series = Series::default();
        for i in 0..(LATENCY_SAMPLES + 10) {
            series.push(Some(i as f32));
        }
        assert_eq!(series.samples.len(), LATENCY_SAMPLES);
        series.push(None);
        series.push(None);
        assert_eq!(series.current(), Some((LATENCY_SAMPLES + 9) as f32), "the last answer of the last three");
        series.push(None);
        assert_eq!(series.current(), None);
    }

    #[test]
    fn numbers_read_the_way_the_radar_shows_them() {
        assert_eq!(fmt_ms(None), "--");
        assert_eq!(fmt_ms(Some(0.4)), "<1 ms");
        assert_eq!(fmt_ms(Some(4.04)), "4.0 ms");
        assert_eq!(fmt_ms(Some(18.4)), "18 ms");
        assert_eq!(fmt_speed(1_000_000_000), "1 Gbps");
        assert_eq!(fmt_speed(2_500_000_000), "2.5 Gbps");
        assert_eq!(fmt_speed(866_700_000), "867 Mbps");
        assert_eq!(fmt_speed(0), "");
        assert_eq!(fmt_mbps(942.3), "942 Mbps");
        assert_eq!(fmt_mbps(87.46), "87.5 Mbps");
        assert!((mbps(25_000_000, 2.0) - 100.0).abs() < 0.01);
        assert_eq!(number_word(6), "six");
        assert_eq!(number_word(21), "21");
    }

    #[test]
    fn the_summary_says_how_the_network_is_doing() {
        let video = spoken_summary(Link::Ethernet, 1_000_000_000, 6, Some(0.6), Some(4.0), 0.0, &[]);
        assert_eq!(video, "Your network is performing excellently, Sir: six devices, connected by a fast Ethernet cable with very low latency.");
        let wifi = spoken_summary(Link::WiFi, 866_700_000, 9, Some(3.0), Some(20.0), 0.0, &[]);
        assert!(wifi.contains("connected over Wi-Fi with low latency"), "{wifi}");
        assert!(wifi.contains("nine devices"), "{wifi}");
        assert!(spoken_summary(Link::Ethernet, 1_000_000_000, 1, Some(0.5), None, 0.0, &[]).contains("one device,"));
        let slow = spoken_summary(Link::WiFi, 0, 4, Some(80.0), Some(300.0), 12.0, &[]);
        assert!(slow.contains("struggling") && slow.contains("12 percent packet loss"), "{slow}");
        let newcomer = spoken_summary(Link::Ethernet, 100_000_000, 7, Some(0.5), Some(4.0), 0.0, &["Private device 105".to_string()]);
        assert!(newcomer.contains("an Ethernet cable at 100 megabits"), "{newcomer}");
        assert!(newcomer.ends_with("One of them is new: Private device 105."), "{newcomer}");
        let demo = demo_snapshot();
        assert_eq!(summary_line(&demo), video);
    }

    #[test]
    fn a_gigabit_card_held_at_100_megabits_is_pointed_out() {
        let mut adapter = demo_snapshot().adapter.expect("the demo has an adapter");
        adapter.description = "Realtek Gaming GbE Family Controller".to_string();
        adapter.speed_bps = 100_000_000;
        assert_eq!(held_back(&adapter).as_deref(), Some("Your network card can do a full gigabit, though; a cable or a switch port is holding it at 100 megabits."));
        adapter.speed_bps = 1_000_000_000;
        assert_eq!(held_back(&adapter), None);
        adapter.speed_bps = 0;
        assert_eq!(held_back(&adapter), None, "no speed reported: nothing to say");
        adapter.speed_bps = 100_000_000;
        adapter.description = "USB 10/100 LAN adapter".to_string();
        assert_eq!(held_back(&adapter), None, "a 100-megabit card is at its best");
        adapter.description = "Intel(R) PRO/1000 MT".to_string();
        assert!(held_back(&adapter).is_some());
        adapter.link = Link::WiFi;
        assert_eq!(held_back(&adapter), None, "Wi-Fi rates say nothing about a cable");
    }

    #[test]
    fn the_security_verdict_names_newcomers_only() {
        let demo = demo_snapshot();
        assert_eq!(security_verdict(&demo.devices, false), "Everything is secure, Sir. All six devices are recognised and accounted for.");
        assert!(security_verdict(&demo.devices, true).starts_with("This is my first sweep of this network, Sir, so I've noted all six devices on it as known."));
        let mut devices = demo.devices.clone();
        devices[2].new = true;
        let one = security_verdict(&devices, false);
        assert!(one.starts_with("One device I haven't seen before, Sir: Private device 105, at 192.168.1.105, a phone using a private address."), "{one}");
        assert!(one.contains("guest's phone"), "{one}");
        devices[4].new = true;
        let two = security_verdict(&devices, false);
        assert!(two.starts_with("Two devices I haven't seen before, Sir: Private device 105 and Private device 103."), "{two}");
        assert_eq!(security_verdict(&devices[..2], false), "Everything is secure, Sir. Both devices are recognised and accounted for.");
    }

    #[test]
    fn the_store_marks_newcomers_after_the_first_sweep() {
        let demo = demo_snapshot();
        let mut store = Store::default();
        let mut devices = demo.devices.clone();
        let network = network_key(demo.adapter.as_ref().expect("an adapter"), &devices);
        assert_eq!(network, "00:11:22:33:44:01");
        assert!(remember(&mut devices, &mut store, &network, 1_000));
        assert!(devices.iter().all(|d| !d.new && d.first_seen == Some(1_000)));
        // A minute later a new phone joins.
        let mut later = devices.clone();
        let mut guest = device(150, [0x0E, 0x11, 0x22, 0x33, 0x44, 0x0F]);
        guest.kind = Kind::Phone;
        later.push(guest);
        assert!(!remember(&mut later, &mut store, &network, 1_060));
        assert_eq!(later.iter().filter(|d| d.new).map(|d| d.ip.octets()[3]).collect::<Vec<_>>(), vec![150]);
        assert_eq!(later[0].first_seen, Some(1_000), "first seen stays");
        // A day later it is one of the regulars.
        assert!(!remember(&mut later, &mut store, &network, 1_060 + NEW_FOR_SECS));
        assert!(later.iter().all(|d| !d.new));
        // The store keeps its size.
        let mut crowd: Vec<Device> = (0..300u16).map(|i| device((i % 250) as u8 + 2, [0x00, 0x22, 0x33, 0x44, (i >> 8) as u8, i as u8])).collect();
        remember(&mut crowd, &mut store, &network, 2_000_000);
        assert!(store.devices.len() <= STORE_CAP);
    }

    #[test]
    fn the_real_adapter_wins_over_a_virtual_one() {
        let picked = pick_adapter(vec![adapter("Hyper-V Virtual Ethernet Adapter", Link::Ethernet, 5), adapter("Realtek PCIe GbE Family Controller", Link::Ethernet, 25)]);
        assert_eq!(picked.map(|a| a.description), Some("Realtek PCIe GbE Family Controller".to_string()));
        let picked = pick_adapter(vec![adapter("Wi-Fi card", Link::WiFi, 50), adapter("Ethernet card", Link::Ethernet, 25)]);
        assert_eq!(picked.map(|a| a.description), Some("Ethernet card".to_string()));
        assert!(pick_adapter(Vec::new()).is_none());
    }

    #[test]
    fn the_demo_network_is_whole_and_reads_as_text() {
        let demo = demo_snapshot();
        assert_eq!(demo.devices.len(), 6);
        assert_eq!(demo.devices.iter().filter(|d| d.gateway).count(), 1);
        assert_eq!(demo.devices.iter().filter(|d| d.this_pc).count(), 1);
        assert!(demo.devices[0].gateway && demo.devices[1].this_pc);
        assert_eq!(demo.router.samples.len(), LATENCY_SAMPLES);
        assert_eq!(demo.internet_latency(), Some(4.0));
        assert_eq!(demo.internet.loss(), Some(0.0));
        let text = summary_text(&demo);
        assert!(text.starts_with("Network radar \u{b7} 192.168.1.0/24 \u{b7} gateway 192.168.1.1 \u{b7} Ethernet 1 Gbps"), "{text}");
        assert!(text.contains("Swept in 3.0 s: six devices online."), "{text}");
        assert!(text.contains("- TP-Link router \u{b7} 192.168.1.1 \u{b7} <1 ms \u{b7} Router"), "{text}");
        assert!(text.contains("- MAX-DESKTOP \u{b7} 192.168.1.20 \u{b7} this PC"), "{text}");
        assert!(text.contains("internet 4.0 ms"), "{text}");
        assert_eq!(Action::parse("check_security"), Action::Security);
        assert_eq!(Action::parse("Rescan"), Action::Rescan);
        assert_eq!(Action::parse("whatever"), Action::Show);
    }
}
