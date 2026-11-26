//! J.A.I.OS's installer, its plan (Sir, 2026-11-25: J.A.I. is the operating system, beside
//! Windows first). From the disks as `lsblk` reads them and what was chosen in the window
//! (`os_install_gui`) comes a `Plan`: every command and every file, in order, as data. Nothing
//! here touches a disk; `os_install_run` carries a plan out, on the live system alone.
//!
//! The plan chooses every id itself (partition GUIDs, the file systems' UUIDs), so fstab and
//! the boot menu are known before a disk is touched and need no `genfstab` afterwards.

use serde_json::Value;

pub const MIB: u64 = 1 << 20;
pub const GIB: u64 = 1 << 30;
/// The new boot partition: 4 GiB, as CachyOS makes it, since limine-snapper-sync keeps the
/// kernels of bootable snapshots on it. Windows' own small one is never used.
const ESP_MIB: u64 = 4096;
/// J.A.I.OS needs this much room: the system, Steam, his models.
pub const AT_LEAST: u64 = 40 * GIB;
/// Where the new system is mounted while it is made, and the installer's own files.
pub const ROOT: &str = "/mnt";
pub const WORK: &str = "/run/jaios-install";
const MAPPER: &str = "jaios";
const NAME: &str = "J.A.I.OS";

const ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
const MSR: &str = "e3c9e316-0b5c-4db8-817d-f92df00215ae";
const RECOVERY: &str = "de94bba4-06d1-4d40-a16a-bfd50179d6ac";

/// The subvolumes, as CachyOS lays them out, and where each is mounted.
pub const SUBVOLUMES: [(&str, &str); 7] = [("@", "/"), ("@home", "/home"), ("@root", "/root"), ("@srv", "/srv"), ("@cache", "/var/cache"), ("@tmp", "/var/tmp"), ("@log", "/var/log")];
const MOUNT: &str = "noatime,compress=zstd";

/// A sentence in both of the installer's languages.
#[derive(Clone, Debug, PartialEq)]
pub struct Said {
    pub en: String,
    pub de: String,
}

impl Said {
    pub fn get(&self, german: bool) -> &str {
        if german {
            &self.de
        } else {
            &self.en
        }
    }
}

fn said(en: impl Into<String>, de: impl Into<String>) -> Said {
    Said { en: en.into(), de: de.into() }
}

// ------------------------------------------------------------------------- the disks ----

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    pub path: String,
    pub number: u32,
    /// In bytes.
    pub start: u64,
    pub size: u64,
    pub fstype: String,
    pub label: String,
    pub parttype: String,
    pub partuuid: String,
    pub mounted: bool,
    /// Windows' boot manager lies on it (`os_install_run` looks, read-only).
    pub bootmgr: bool,
}

impl Part {
    pub fn is_esp(&self) -> bool {
        self.parttype.eq_ignore_ascii_case(ESP)
    }

    /// Windows' own: its reserved partition, its recovery, anything NTFS or BitLocker.
    pub fn is_windows(&self) -> bool {
        self.parttype.eq_ignore_ascii_case(MSR) || self.parttype.eq_ignore_ascii_case(RECOVERY) || ["ntfs", "bitlocker"].contains(&self.fstype.to_lowercase().as_str())
    }

    /// What the bar calls it.
    pub fn called(&self) -> &str {
        match () {
            _ if self.is_esp() => "EFI",
            _ if self.parttype.eq_ignore_ascii_case(MSR) => "MSR",
            _ if self.parttype.eq_ignore_ascii_case(RECOVERY) => "RECOVERY",
            _ if self.is_windows() => "WINDOWS",
            _ if !self.label.is_empty() => &self.label,
            _ if !self.fstype.is_empty() => &self.fstype,
            _ => "DATA",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Disk {
    pub path: String,
    pub model: String,
    pub size: u64,
    /// "gpt", "dos" or nothing.
    pub table: String,
    /// The stick J.A.I.OS is installed from (or another installation medium).
    pub live: bool,
    /// The disk itself is mounted (no partitions).
    pub mounted: bool,
    pub parts: Vec<Part>,
}

impl Disk {
    /// "nvme0n1": what Sir types to have it erased.
    pub fn name(&self) -> &str {
        self.path.trim_start_matches("/dev/")
    }

    pub fn has_windows(&self) -> bool {
        self.parts.iter().any(|p| p.bootmgr || p.is_windows())
    }

    /// The partition Windows starts from.
    pub fn windows_esp(&self) -> Option<&Part> {
        self.parts.iter().find(|p| p.bootmgr).or_else(|| self.parts.iter().find(|p| p.is_esp()).filter(|_| self.has_windows()))
    }

    fn in_use(&self) -> bool {
        self.mounted || self.parts.iter().any(|p| p.mounted)
    }

    /// The room no partition takes, in MiB (from, to), each on a MiB: the first MiB is the
    /// table's, the last one its copy's.
    pub fn free(&self) -> Vec<(u64, u64)> {
        if self.table != "gpt" {
            return Vec::new();
        }
        let mut parts: Vec<&Part> = self.parts.iter().collect();
        parts.sort_by_key(|p| p.start);
        let (mut at, mut free) = (1, Vec::new());
        for p in parts {
            if p.start / MIB > at {
                free.push((at, p.start / MIB));
            }
            at = at.max((p.start + p.size).div_ceil(MIB));
        }
        let end = (self.size / MIB).saturating_sub(1);
        if end > at {
            free.push((at, end));
        }
        free
    }

    pub fn largest_free(&self) -> Option<(u64, u64)> {
        self.free().into_iter().max_by_key(|(from, to)| to - from)
    }

    /// Windows is on it and there is room beside it: what the installer chooses by itself.
    pub fn beside_windows(&self) -> bool {
        !self.live && self.has_windows() && self.largest_free().is_some_and(|(from, to)| (to - from) * MIB >= AT_LEAST)
    }
}

fn text(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn number(v: &Value, key: &str) -> u64 {
    text(v, key).parse().unwrap_or(0)
}

fn below(v: &Value) -> impl Iterator<Item = &Value> {
    v["children"].as_array().into_iter().flatten()
}

/// Every place a device or anything on it (a LUKS container, a volume) is mounted at.
fn mounts(v: &Value) -> Vec<String> {
    let own = v["mountpoints"].as_array().into_iter().flatten().chain(std::iter::once(&v["mountpoint"])).filter_map(|m| m.as_str().map(str::to_string));
    own.chain(below(v).flat_map(mounts)).collect()
}

fn installation_medium(v: &Value) -> bool {
    text(v, "fstype") == "iso9660" || mounts(v).iter().any(|m| m.starts_with("/run/archiso")) || below(v).any(installation_medium)
}

/// The disks out of `lsblk --json -b -o` with `os_install_run`'s columns.
pub fn disks(lsblk: &str) -> Vec<Disk> {
    let all: Value = serde_json::from_str(lsblk).unwrap_or_default();
    let disk = |d: &Value| Disk {
        path: text(d, "path"),
        model: text(d, "model"),
        size: number(d, "size"),
        table: text(d, "pttype"),
        live: installation_medium(d),
        mounted: !mounts(d).is_empty() && below(d).all(|p| text(p, "type") != "part"),
        parts: below(d)
            .filter(|p| text(p, "type") == "part")
            .map(|p| Part {
                path: text(p, "path"),
                number: number(p, "partn") as u32,
                // (lsblk's START counts 512-byte sectors, whatever the disk's own are.)
                start: number(p, "start") * 512,
                size: number(p, "size"),
                fstype: text(p, "fstype"),
                label: text(p, "label"),
                parttype: text(p, "parttype"),
                partuuid: text(p, "partuuid"),
                mounted: !mounts(p).is_empty(),
                bootmgr: false,
            })
            .collect(),
    };
    all["blockdevices"].as_array().into_iter().flatten().filter(|d| text(d, "type") == "disk" && number(d, "size") > 0).map(disk).collect()
}

/// A PC as Sir's is: Windows 11 on a 1 TB disk with 200 GiB made free at its end, an empty
/// second disk, and the stick. What the installer shows where it cannot install (a preview on
/// Windows, a Linux that is not the stick), and what the tests plan on.
pub const SAMPLE: &str = r#"{ "blockdevices": [
  { "name": "nvme0n1", "path": "/dev/nvme0n1", "type": "disk", "size": 1000204886016, "model": "Sample NVMe 1TB", "pttype": "gpt", "fstype": null, "mountpoints": [null], "children": [
    { "name": "nvme0n1p1", "path": "/dev/nvme0n1p1", "type": "part", "size": 104857600, "fstype": "vfat", "label": null, "parttype": "c12a7328-f81f-11d2-ba4b-00a0c93ec93b", "partuuid": "11111111-2222-4333-8444-555555555555", "partn": 1, "start": 2048, "mountpoints": [null] },
    { "name": "nvme0n1p2", "path": "/dev/nvme0n1p2", "type": "part", "size": 16777216, "fstype": null, "label": null, "parttype": "e3c9e316-0b5c-4db8-817d-f92df00215ae", "partuuid": "22222222-2222-4333-8444-555555555555", "partn": 2, "start": 206848, "mountpoints": [null] },
    { "name": "nvme0n1p3", "path": "/dev/nvme0n1p3", "type": "part", "size": 784598040576, "fstype": "BitLocker", "label": null, "parttype": "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7", "partuuid": "33333333-2222-4333-8444-555555555555", "partn": 3, "start": 239616, "mountpoints": [null] },
    { "name": "nvme0n1p4", "path": "/dev/nvme0n1p4", "type": "part", "size": 734003200, "fstype": "ntfs", "label": null, "parttype": "de94bba4-06d1-4d40-a16a-bfd50179d6ac", "partuuid": "44444444-2222-4333-8444-555555555555", "partn": 4, "start": 1532657664, "mountpoints": [null] } ] },
  { "name": "sda", "path": "/dev/sda", "type": "disk", "size": 500107862016, "model": "Sample SSD 500GB", "pttype": "gpt", "fstype": null, "mountpoints": [null], "children": [
    { "name": "sda1", "path": "/dev/sda1", "type": "part", "size": 214748364800, "fstype": "ext4", "label": "games", "parttype": "0fc63daf-8483-4772-8e79-3d69d8477de4", "partuuid": "55555555-2222-4333-8444-555555555555", "partn": 1, "start": 2048, "mountpoints": [null] } ] },
  { "name": "sdb", "path": "/dev/sdb", "type": "disk", "size": 15518924800, "model": "Sample Stick", "pttype": "dos", "fstype": "iso9660", "mountpoints": [null], "children": [
    { "name": "sdb1", "path": "/dev/sdb1", "type": "part", "size": 1900000000, "fstype": "iso9660", "label": "JAIOS", "parttype": "0x0", "partuuid": null, "partn": 1, "start": 0, "mountpoints": ["/run/archiso/bootmnt"] } ] }
] }"#;

/// A size as the window says it.
pub fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 10 * GIB => format!("{:.0} GiB", b as f64 / GIB as f64),
        b if b >= GIB => format!("{:.1} GiB", b as f64 / GIB as f64),
        b => format!("{} MiB", b / MIB),
    }
}

// --------------------------------------------------------------------- places and keys ----

/// The time zones out of /usr/share/zoneinfo/zone1970.tab.
pub fn zones(tab: &str) -> Vec<String> {
    let mut zones: Vec<String> = tab.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split('\t').nth(2)).map(str::to_string).chain(["UTC".to_string()]).collect();
    zones.sort();
    zones.dedup();
    zones
}

/// The keyboard layouts out of xkb's base.lst (code, name), German and US first.
pub fn layouts(lst: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = lst
        .lines()
        .skip_while(|l| l.trim() != "! layout")
        .skip(1)
        .take_while(|l| !l.starts_with('!'))
        .filter_map(|l| l.trim().split_once(char::is_whitespace))
        .map(|(code, name)| (code.to_string(), name.trim().to_string()))
        .collect();
    for first in ["us", "de"] {
        let at = found.iter().position(|(code, _)| code == first);
        let layout = at.map_or_else(|| (first.to_string(), if first == "de" { "German" } else { "English (US)" }.to_string()), |i| found.remove(i));
        found.insert(0, layout);
    }
    found
}

/// The console's name for an xkb layout.
fn console_keymap(layout: &str) -> &str {
    // shortcut: only the layouts whose console map is named otherwise; a layout without a
    // console map of its name leaves the text console on US keys. Take localectl's table
    // (/usr/share/systemd/kbd-model-map) when that matters.
    match layout {
        "de" | "at" => "de-latin1",
        "ch" => "de_CH-latin1",
        "gb" => "uk",
        other => other,
    }
}

// ----------------------------------------------------------------------- the machine ----

/// Which of CachyOS's repositories a processor takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Level {
    Generic,
    V3,
    V4,
    Znver4,
}

impl Level {
    /// As CachyOS's own script decides: Zen 4 and 5 (AMD's family 19h with AVX-512, and 1Ah)
    /// take znver4, AVX-512 takes v4, AVX2 takes v3.
    pub fn of(amd_family: Option<u32>, v3: bool, v4: bool) -> Level {
        match (amd_family, v3, v4) {
            (Some(family), _, true) if family >= 0x19 => Level::Znver4,
            (_, _, true) => Level::V4,
            (_, true, _) => Level::V3,
            _ => Level::Generic,
        }
    }

    /// This PC's, with AMD's family out of /proc/cpuinfo.
    pub fn of_this_pc(cpuinfo: &str) -> Level {
        let field = |name: &str| cpuinfo.lines().find_map(|l| l.split_once(':').filter(|(key, _)| key.trim() == name).map(|(_, v)| v.trim().to_string()));
        let amd = field("vendor_id").is_some_and(|v| v == "AuthenticAMD").then(|| field("cpu family").and_then(|f| f.parse().ok())).flatten();
        #[cfg(target_arch = "x86_64")]
        let (v3, v4) = {
            use std::arch::is_x86_feature_detected as has;
            let v3 = has!("avx2") && has!("bmi1") && has!("bmi2") && has!("fma") && has!("lzcnt") && has!("movbe") && has!("f16c");
            (v3, v3 && has!("avx512f") && has!("avx512bw") && has!("avx512cd") && has!("avx512dq") && has!("avx512vl"))
        };
        #[cfg(not(target_arch = "x86_64"))]
        let (v3, v4) = (false, false);
        Level::of(amd, v3, v4)
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Generic => "x86-64",
            Level::V3 => "x86-64-v3",
            Level::V4 => "x86-64-v4",
            Level::Znver4 => "znver4",
        }
    }
}

/// The firmware's Secure Boot: in Setup Mode it takes new keys, on it checks what starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SecureBoot {
    Setup,
    On,
    Off,
}

impl SecureBoot {
    /// From the firmware's own two variables, SetupMode and SecureBoot (their value byte).
    pub fn of(setup_mode: Option<u8>, secure_boot: Option<u8>) -> SecureBoot {
        match (setup_mode, secure_boot) {
            (Some(1), _) => SecureBoot::Setup,
            (_, Some(1)) => SecureBoot::On,
            _ => SecureBoot::Off,
        }
    }
}

/// The ids the plan gives what it makes.
#[derive(Clone, Debug)]
pub struct Ids {
    pub esp: String,
    pub root: String,
    pub fs: String,
    pub luks: String,
    /// The FAT volume's id, eight hex digits.
    pub fat: String,
}

impl Ids {
    pub fn fresh() -> Self {
        let id = || uuid::Uuid::new_v4().to_string();
        Self { esp: id(), root: id(), fs: id(), luks: id(), fat: id()[..8].to_uppercase() }
    }
}

#[derive(Clone, Debug)]
pub struct Machine {
    pub uefi: bool,
    pub level: Level,
    pub secure_boot: SecureBoot,
    pub vram_mb: Option<u64>,
    pub ids: Ids,
}

// ----------------------------------------------------------------------- the choices ----

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Beside Windows, into the largest free room of this disk.
    Free(String),
    /// The whole disk.
    Erase(String),
    /// In place of this partition.
    Replace(String),
}

#[derive(Clone, Debug)]
pub struct Choices {
    pub german: bool,
    pub zone: String,
    pub layout: String,
    pub target: Target,
    /// LUKS2 with this passphrase.
    pub passphrase: Option<String>,
    pub full_name: String,
    pub user: String,
    pub password: String,
    pub host: String,
    /// The Main PC this one is a client of; none: it is a Main PC itself.
    pub main_pc: String,
    /// The account's name at J.A.I., and the folder its files were made in (`os_install_gui`).
    pub account: String,
    pub stage: String,
}

impl Choices {
    /// Every word that ends up in a command or a file is of the shape it must have.
    fn check(&self) -> Result<(), Said> {
        let user = self.user.as_str();
        let name_ok = !user.is_empty() && user.len() <= 32 && user.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') && user.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if !name_ok || user == "root" {
            return Err(said("The user name takes small letters, digits, - and _, and starts with a letter.", "Der Benutzername besteht aus Kleinbuchstaben, Ziffern, - und _ und beginnt mit einem Buchstaben."));
        }
        if self.password.is_empty() || self.password.contains(['\n', '\r']) {
            return Err(said("The user needs a password.", "Der Benutzer braucht ein Passwort."));
        }
        let host = self.host.as_str();
        if host.is_empty() || host.len() > 63 || host.starts_with('-') || host.ends_with('-') || !host.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(said("The PC's name takes small letters, digits and -.", "Der Name des PCs besteht aus Kleinbuchstaben, Ziffern und -."));
        }
        if self.zone.is_empty() || self.zone.contains("..") || self.zone.starts_with('/') || !self.zone.chars().all(|c| c.is_ascii_alphanumeric() || "_+-/".contains(c)) {
            return Err(said("Choose a time zone.", "Wähle eine Zeitzone."));
        }
        if self.layout.is_empty() || !self.layout.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(said("Choose a keyboard layout.", "Wähle eine Tastaturbelegung."));
        }
        if self.passphrase.as_ref().is_some_and(|p| p.chars().count() < 8 || p.contains(['\n', '\r'])) {
            return Err(said("The passphrase for the encryption needs 8 characters or more.", "Die Passphrase für die Verschlüsselung braucht mindestens 8 Zeichen."));
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------- the plan ----

/// One thing the installer does: a command (what it is fed on its standard input never shows,
/// a `soft` one may fail), or a file.
pub enum Action {
    Run { argv: Vec<String>, stdin: Option<String>, soft: bool },
    Write { path: String, text: String, mode: u32 },
}

impl Action {
    fn fed(self, with: &str) -> Self {
        match self {
            Action::Run { argv, soft, .. } => Action::Run { argv, stdin: Some(with.to_string()), soft },
            file => file,
        }
    }

    fn soft(self) -> Self {
        match self {
            Action::Run { argv, stdin, .. } => Action::Run { argv, stdin, soft: true },
            file => file,
        }
    }

    /// As the log shows it.
    pub fn shown(&self) -> String {
        match self {
            Action::Run { argv, .. } => argv.join(" "),
            Action::Write { path, .. } => format!("write {path}"),
        }
    }
}

fn run(argv: &[&str]) -> Action {
    Action::Run { argv: argv.iter().map(|a| a.to_string()).collect(), stdin: None, soft: false }
}

/// A command inside the new system.
fn inside(argv: &[&str]) -> Action {
    run(&[&["arch-chroot", ROOT], argv].concat())
}

fn write(path: &str, text: impl Into<String>) -> Action {
    Action::Write { path: path.to_string(), text: text.into(), mode: 0o644 }
}

/// A file of the new system.
fn file(path: &str, text: impl Into<String>) -> Action {
    write(&format!("{ROOT}{path}"), text)
}

pub struct Step {
    pub title: Said,
    pub actions: Vec<Action>,
}

pub struct Plan {
    pub steps: Vec<Step>,
    /// Every change in plain words, the disk's first; true: something is lost by it.
    pub summary: Vec<(Said, bool)>,
    /// What lets go of the disk again, also after a step failed.
    pub release: Vec<Action>,
}

impl Plan {
    /// Every command and file, as the log shows them.
    #[cfg(test)]
    pub fn shown(&self) -> Vec<String> {
        self.steps.iter().flat_map(|s| &s.actions).map(Action::shown).collect()
    }

    /// The text of a file the plan writes.
    #[cfg(test)]
    pub fn file(&self, path: &str) -> Option<&str> {
        self.steps.iter().flat_map(|s| &s.actions).find_map(|a| match a {
            Action::Write { path: p, text, .. } if p == path => Some(text.as_str()),
            _ => None,
        })
    }
}

/// The room the new system goes into.
struct Ground<'a> {
    disk: &'a Disk,
    /// What makes the room first (nothing, for free space).
    clear: Vec<Action>,
    from: u64,
    to: u64,
    /// The partition numbers that stay in use.
    taken: Vec<u32>,
    lost: Vec<Said>,
}

fn ground<'a>(disks: &'a [Disk], target: &Target) -> Result<Ground<'a>, Said> {
    let (path, part) = match target {
        Target::Free(d) | Target::Erase(d) => (d.as_str(), None),
        Target::Replace(p) => (p.as_str(), Some(p.as_str())),
    };
    let disk = disks.iter().find(|d| d.path == path || d.parts.iter().any(|p| Some(p.path.as_str()) == part)).ok_or_else(|| said("Choose a disk.", "Wähle eine Festplatte."))?;
    let d = &disk.path;
    if disk.live {
        return Err(said(format!("{d} is the stick J.A.I.OS is installed from."), format!("{d} ist der Stick, von dem J.A.I.OS installiert wird.")));
    }
    if disk.in_use() {
        return Err(said(format!("{d} is in use: something on it is mounted."), format!("{d} wird benutzt: etwas darauf ist eingehängt.")));
    }
    let mut g = Ground { disk, clear: Vec::new(), from: 1, to: (disk.size / MIB).saturating_sub(1), taken: disk.parts.iter().map(|p| p.number).collect(), lost: Vec::new() };
    match target {
        Target::Free(_) => {
            if disk.table != "gpt" {
                return Err(said(format!("{d} has no GPT partition table: there is no free space J.A.I.OS could take beside what is on it."), format!("{d} hat keine GPT-Partitionstabelle: Daneben ist kein freier Platz, den J.A.I.OS nehmen könnte.")));
            }
            (g.from, g.to) = disk.largest_free().unwrap_or((1, 1));
        }
        Target::Erase(_) => {
            g.clear = vec![run(&["wipefs", "--all", d]), run(&["sgdisk", "--zap-all", d]), run(&["sgdisk", "--clear", d])];
            g.taken.clear();
            g.lost.push(said(format!("Everything on {d} ({}, {}) is erased.", disk.model, size(disk.size)), format!("Alles auf {d} ({}, {}) wird gelöscht.", disk.model, size(disk.size))));
            if disk.has_windows() {
                g.lost.push(said("Windows is on that disk: it is erased too.", "Auf dieser Festplatte liegt Windows: Es wird mitgelöscht."));
            }
        }
        Target::Replace(_) => {
            let p = disk.parts.iter().find(|p| Some(p.path.as_str()) == part).expect("the disk was found by this partition");
            if p.is_windows() || p.is_esp() {
                return Err(said(format!("{} belongs to Windows or starts this PC: J.A.I.OS never takes it.", p.path), format!("{} gehört zu Windows oder startet diesen PC: J.A.I.OS nimmt sie nie.", p.path)));
            }
            if disk.table != "gpt" {
                return Err(said(format!("{d} has no GPT partition table: erase the whole disk instead."), format!("{d} hat keine GPT-Partitionstabelle: Lösche stattdessen die ganze Festplatte.")));
            }
            g.clear = vec![run(&["sgdisk", &format!("--delete={}", p.number), d])];
            g.taken.retain(|n| *n != p.number);
            (g.from, g.to) = (p.start.div_ceil(MIB), ((p.start + p.size) / MIB).min(g.to));
            g.lost.push(said(format!("Everything on {} ({}, {}) is erased.", p.path, p.called(), size(p.size)), format!("Alles auf {} ({}, {}) wird gelöscht.", p.path, p.called(), size(p.size))));
        }
    }
    let room = g.to.saturating_sub(g.from) * MIB;
    if room < AT_LEAST {
        return Err(said(format!("J.A.I.OS needs {} of room; there are {} on {d}.", size(AT_LEAST), size(room)), format!("J.A.I.OS braucht {} Platz; auf {d} sind es {}.", size(AT_LEAST), size(room))));
    }
    Ok(g)
}

/// The disk and the room on it (MiB, from and to) a choice would give J.A.I.OS, or why not.
pub fn room<'a>(disks: &'a [Disk], target: &Target) -> Result<(&'a Disk, u64, u64), Said> {
    ground(disks, target).map(|g| (g.disk, g.from, g.to))
}

/// The packages of the new system (`os/packages/target.txt`).
pub fn packages() -> Vec<&'static str> {
    include_str!("../os/packages/target.txt").lines().map(|l| l.split('#').next().unwrap_or("").trim()).filter(|l| !l.is_empty()).collect()
}

/// pacman's configuration: [jaios] first, CachyOS's repositories for the processor as
/// CachyOS's own script orders them, then Arch's.
pub fn pacman_conf(level: Level) -> String {
    let (architecture, tuned, mirrors) = match level {
        Level::Generic => ("x86_64", None, ""),
        Level::V3 => ("x86_64 x86_64_v3", Some("v3"), "cachyos-v3-mirrorlist"),
        Level::V4 => ("x86_64 x86_64_v4 x86_64_v3", Some("v4"), "cachyos-v4-mirrorlist"),
        Level::Znver4 => ("x86_64 x86_64_v4 x86_64_v3", Some("znver4"), "cachyos-v4-mirrorlist"),
    };
    let mut conf = format!(
        "# J.A.I.OS: CachyOS's repositories for {}, Arch's, and J.A.I.'s own.\n[options]\nHoldPkg = pacman glibc\nArchitecture = {architecture}\nColor\nCheckSpace\nParallelDownloads = 8\nSigLevel = Required DatabaseOptional\nLocalFileSigLevel = Optional\n\n\
         [jaios]\nSigLevel = Optional TrustAll\nServer = https://github.com/JayKaey/JAI/releases/download/jaios-repo\n",
        level.name()
    );
    for repo in tuned.iter().flat_map(|t| [format!("cachyos-{t}"), format!("cachyos-core-{t}"), format!("cachyos-extra-{t}")]) {
        conf += &format!("\n[{repo}]\nInclude = /etc/pacman.d/{mirrors}\n");
    }
    conf += "\n[cachyos]\nInclude = /etc/pacman.d/cachyos-mirrorlist\n";
    for repo in ["core", "extra", "multilib"] {
        conf += &format!("\n[{repo}]\nInclude = /etc/pacman.d/mirrorlist\n");
    }
    conf
}

/// Limine's menu in J.A.I.'s look (`os_look`'s lines, the same as the package's): it starts
/// what was started last, after 3 s. J.A.I.OS's entry stands first; limine-entry-tool puts
/// every kernel under it (with its checksum) and limine-snapper-sync the snapshots, so entry 2
/// is the newest kernel. Windows is started from its own partition, found by that partition's GUID.
fn limine_conf(windows_esp: Option<&Part>) -> String {
    let mut conf = crate::os_look::limine_conf(&crate::holo::theme());
    conf += &format!("default_entry: 2\n\n/+{NAME}\n    comment: J.A.I.\n");
    if let Some(esp) = windows_esp {
        conf += &format!("\n/Windows\n    comment: Windows, as it was\n    protocol: efi\n    path: guid({}):/EFI/Microsoft/Boot/bootmgfw.efi\n", esp.partuuid);
    }
    conf
}

/// What a plan is made from, checked: `Err` says in both languages why J.A.I.OS is not
/// installed that way.
pub fn plan(disks: &[Disk], c: &Choices, m: &Machine) -> Result<Plan, Said> {
    if !m.uefi {
        return Err(said("J.A.I.OS starts by UEFI, and the stick was started the old BIOS way. Start it again in UEFI mode.", "J.A.I.OS startet per UEFI, und der Stick wurde auf die alte BIOS-Art gestartet. Starte ihn im UEFI-Modus neu."));
    }
    let Ground { disk, clear, from, to, taken, lost } = ground(disks, &c.target)?;
    c.check()?;
    let (disk, ids) = (disk.path.as_str(), &m.ids);
    let erased = matches!(c.target, Target::Erase(_)).then_some(disk);
    let windows_esp = disks.iter().filter(|d| Some(d.path.as_str()) != erased).find_map(Disk::windows_esp);
    let mut numbers = (1..=128u32).filter(|n| !taken.contains(n));
    let (n_esp, n_root) = (numbers.next().unwrap_or(127), numbers.next().unwrap_or(128));
    let root_mib = to - from - ESP_MIB;
    let (esp_dev, part_dev) = (format!("/dev/disk/by-partuuid/{}", ids.esp), format!("/dev/disk/by-partuuid/{}", ids.root));
    let root_dev = if c.passphrase.is_some() { format!("/dev/mapper/{MAPPER}") } else { part_dev.clone() };
    let mut steps = Vec::new();
    let mut step = |en: &str, de: &str, actions: Vec<Action>| steps.push(Step { title: said(en, de), actions });

    // The partitions, each on a MiB: the boot partition, then J.A.I.OS on the rest.
    let mut parts = clear;
    let new = |n: u32, from: u64, mib: u64, kind: &str, name: &str, guid: &str| [format!("--new={n}:{from}M:+{mib}M"), format!("--typecode={n}:{kind}"), format!("--change-name={n}:{name}"), format!("--partition-guid={n}:{guid}")];
    let made: Vec<String> = [new(n_esp, from, ESP_MIB, "ef00", "JAIOS-ESP", &ids.esp), new(n_root, from + ESP_MIB, root_mib, if c.passphrase.is_some() { "8309" } else { "8304" }, "JAIOS", &ids.root)].concat();
    parts.push(Action::Run { argv: std::iter::once("sgdisk".to_string()).chain(made).chain([disk.to_string()]).collect(), stdin: None, soft: false });
    parts.extend([run(&["partprobe", disk]), run(&["udevadm", "settle"])]);
    step("Dividing the disk", "Festplatte einteilen", parts);

    let mut format = vec![run(&["wipefs", "--all", &esp_dev]), run(&["wipefs", "--all", &part_dev]), run(&["mkfs.fat", "-F", "32", "-n", "JAIOS-ESP", "-i", &ids.fat, &esp_dev])];
    if let Some(passphrase) = &c.passphrase {
        format.push(run(&["cryptsetup", "luksFormat", "--type", "luks2", "--batch-mode", "--uuid", &ids.luks, "--key-file", "-", &part_dev]).fed(passphrase));
        format.push(run(&["cryptsetup", "open", "--key-file", "-", &part_dev, MAPPER]).fed(passphrase));
    }
    format.extend([run(&["mkfs.btrfs", "--force", "--label", "JAIOS", "--uuid", &ids.fs, &root_dev]), run(&["mount", &root_dev, ROOT])]);
    format.extend(SUBVOLUMES.iter().map(|(volume, _)| run(&["btrfs", "subvolume", "create", &format!("{ROOT}/{volume}")])));
    format.push(run(&["umount", ROOT]));
    format.extend(SUBVOLUMES.iter().map(|(volume, at)| run(&["mount", "--mkdir", "-o", &format!("{MOUNT},subvol=/{volume}"), &root_dev, &format!("{ROOT}{}", at.trim_end_matches('/'))])));
    format.push(run(&["mount", "--mkdir", "-o", "umask=0077", &esp_dev, &format!("{ROOT}/boot")]));
    step("Making the file systems", "Dateisysteme anlegen", format);

    // The live system's pacman installs into the new root, with the new system's own
    // configuration (it copies the stick's keyring and Arch's mirror list along).
    let conf = format!("{WORK}/pacman.conf");
    let pacstrap: Vec<&str> = ["pacstrap", "-C", &conf, ROOT].into_iter().chain(packages()).collect();
    step("Installing the system", "System installieren", vec![write(&conf, pacman_conf(m.level)), run(&pacstrap)]);

    let fstab: String = SUBVOLUMES.iter().map(|(volume, at)| format!("UUID={}  {at}  btrfs  subvol=/{volume},{MOUNT}  0 0\n", ids.fs)).collect();
    let fstab = format!("# J.A.I.OS: one btrfs with CachyOS's subvolumes, the boot partition for Limine and the kernels.\n{fstab}UUID={}-{}  /boot  vfat  umask=0077  0 2\n", &ids.fat[..4], &ids.fat[4..]);
    let locale = if c.german { "de_DE.UTF-8" } else { "en_US.UTF-8" };
    let encrypt = if c.passphrase.is_some() { " sd-encrypt" } else { "" };
    let (user, host) = (c.user.as_str(), c.host.as_str());
    let full_name: String = c.full_name.chars().filter(|ch| !ch.is_control() && !":,".contains(*ch)).collect();
    step(
        "Setting it up",
        "Einrichten",
        vec![
            file("/etc/pacman.conf", pacman_conf(m.level)),
            file("/etc/fstab", fstab),
            file("/etc/locale.gen", "en_US.UTF-8 UTF-8\nde_DE.UTF-8 UTF-8\n"),
            file("/etc/locale.conf", format!("LANG={locale}\n")),
            inside(&["locale-gen"]),
            inside(&["ln", "-sf", &format!("/usr/share/zoneinfo/{}", c.zone), "/etc/localtime"]),
            inside(&["hwclock", "--systohc"]).soft(),
            file("/etc/vconsole.conf", format!("KEYMAP={}\n", console_keymap(&c.layout))),
            file("/etc/X11/xorg.conf.d/00-keyboard.conf", format!("Section \"InputClass\"\n    Identifier \"system-keyboard\"\n    MatchIsKeyboard \"on\"\n    Option \"XkbLayout\" \"{}\"\nEndSection\n", c.layout)),
            file("/etc/hostname", format!("{host}\n")),
            file("/etc/hosts", format!("127.0.0.1  localhost\n::1        localhost\n127.0.1.1  {host}\n")),
            file("/etc/mkinitcpio.conf.d/jaios.conf", format!("# Plymouth shows J.A.I.'s core while the PC starts{}.\nHOOKS=(base systemd plymouth autodetect microcode modconf kms keyboard sd-vconsole block{encrypt} filesystems fsck)\n", if encrypt.is_empty() { "" } else { "; sd-encrypt asks for the passphrase" })),
            inside(&["groupadd", "--force", "jai"]),
            inside(&["useradd", "--create-home", "--comment", &full_name, "--groups", "wheel,jai,input,video", "--shell", "/bin/bash", user]),
            // (The password goes in on chpasswd's standard input, never on a command line.)
            inside(&["chpasswd"]).fed(&format!("{user}:{}\n", c.password)),
            inside(&["passwd", "--lock", "root"]),
            Action::Write { path: format!("{ROOT}/etc/sudoers.d/10-wheel"), text: "%wheel ALL=(ALL:ALL) ALL\n".into(), mode: 0o440 },
            inside(&["chwd", "-a"]).soft(),
        ],
    );

    // Limine. Its kernels' entries are limine-entry-tool's (CachyOS's limine-mkinitcpio-hook):
    // every kernel update copies the kernel to the boot partition and writes its entry with
    // its BLAKE2 checksum, and limine-snapper-sync adds the snapshots.
    let unlock = if c.passphrase.is_some() { format!(" rd.luks.name={}={MAPPER}", ids.luks) } else { String::new() };
    let own_keys = m.secure_boot == SecureBoot::Setup;
    let defaults = format!(
        "# J.A.I.OS (limine-entry-tool and limine-snapper-sync read this).\nTARGET_OS_NAME=\"{NAME}\"\nESP_PATH=\"/boot\"\nKERNEL_CMDLINE[default]=\"root=UUID={} rw rootflags=subvol=/@ quiet splash{unlock}\"\nENABLE_VERIFICATION=yes\nENABLE_ENROLL_LIMINE_CONFIG={}\nENABLE_LIMINE_FALLBACK=no\nFIND_BOOTLOADERS=no\nMAX_SNAPSHOT_ENTRIES=8\nROOT_SUBVOLUME_PATH=\"/@\"\nROOT_SNAPSHOTS_PATH=\"/@/.snapshots\"\n",
        ids.fs,
        if own_keys { "yes" } else { "no" }
    );
    let [loader, fallback] = crate::os_system::LIMINE_LOADERS;
    let mut boot = vec![
        file("/etc/default/limine", defaults),
        file("/boot/limine.conf", limine_conf(windows_esp)),
        run(&["install", "-D", &format!("{ROOT}/usr/share/limine/BOOTX64.EFI"), &format!("{ROOT}{loader}")]),
        run(&["install", "-D", &format!("{ROOT}/usr/share/limine/BOOTX64.EFI"), &format!("{ROOT}{fallback}")]),
        run(&["install", "-D", &format!("{ROOT}/usr/share/jaios/limine/wallpaper.png"), &format!("{ROOT}{}", crate::os_look::LIMINE_WALLPAPER.replace("boot():", "/boot"))]).soft(),
        // jaios-settings' template: ten snapshots, five important ones, none by the clock.
        inside(&["snapper", "--no-dbus", "-c", "root", "create-config", "-t", "jaios", "/"]),
    ];
    if own_keys {
        // The firmware takes keys now: his own and Microsoft's (-m: Windows and the cards'
        // firmware go on starting). The keys are made before the kernels' entries, so that
        // limine-entry-tool finds them.
        boot.extend([inside(&["sbctl", "create-keys"]), inside(&["sbctl", "enroll-keys", "--microsoft"])]);
    }
    boot.push(inside(&["limine-update"]));
    if own_keys {
        // Limine itself is what the firmware checks: limine.conf's checksum is put into it
        // first (from then on it starts only that menu, and only kernels whose checksum the
        // menu carries), then it is signed, for a signature covers the checksum too. The
        // kernels are NOT signed: Limine holds them to their checksums, which signing would
        // change.
        // shortcut: the fallback loader (EFI/BOOT) is signed without the menu's checksum in
        // it, since that needs the checksum computed here; the firmware starts EFI/limine by
        // its entry. Enroll it too once the root core keeps the keys.
        boot.extend([inside(&["limine-enroll-config"]).soft(), inside(&["sbctl", "sign", "--save", loader]), inside(&["sbctl", "sign", "--save", fallback])]);
    }
    // (With the maker's keys, or Secure Boot off, his root core finishes this later: he tells
    // how at the first start, from `sbctl status`, and "finish Secure Boot" does it.)
    boot.push(run(&["efibootmgr", "--create", "--disk", disk, "--part", &n_esp.to_string(), "--label", NAME, "--loader", r"\EFI\limine\limine_x64.efi"]));
    boot.push(inside(&["systemctl", "enable", "greetd.service", "NetworkManager.service", "bluetooth.service", "fstrim.timer", "limine-snapper-sync.service", "jai-system.service"]));
    step("Making it start", "Startfähig machen", boot);

    // J.A.I. for the new user: his settings, the account from the installer, the shared models.
    let brain = crate::installer::brain_for(m.vram_mb).unwrap_or(crate::installer::SMALL_BRAIN);
    let mut settings = crate::config::JaiConfig::default();
    settings.primary_model = brain.into();
    settings.fallback_model = brain.into();
    settings.main_pc = c.main_pc.clone();
    let home = format!("/home/{user}");
    let data = format!("{home}/.local/share/jai");
    let mut jai = vec![file(&format!("{data}/{}", crate::config::CONFIG_FILE), serde_json::to_string_pretty(&settings).unwrap_or_default())];
    if !c.stage.is_empty() {
        jai.push(run(&["cp", "-a", &format!("{}/.", c.stage), &format!("{ROOT}{data}/")]));
    }
    jai.extend([
        inside(&["install", "-d", "-g", "jai", "-m", "2775", "/var/lib/jai", "/var/lib/jai/models"]),
        inside(&["ln", "-sfn", "/var/lib/jai/models", &format!("{data}/models")]),
        inside(&["ln", "-sfn", "/usr/lib/jai/runtime", &format!("{data}/runtime")]),
        // The new system's own J.A.I. fetches his voice, his ears and his brain, from his
        // folder as he will find them: the version that will run decides what it needs.
        // (What does not come now he fetches at his first start.)
        inside(&["env", "-C", &data, &format!("HOME={home}"), "/usr/lib/jai/jai", "--download-models"]).soft(),
        inside(&["chown", "-R", &format!("{user}:"), &format!("{home}/.local")]),
        inside(&["chgrp", "-R", "jai", "/var/lib/jai/models"]),
        inside(&["chmod", "-R", "g+rwX", "/var/lib/jai/models"]),
    ]);
    step("J.A.I. moves in", "J.A.I. zieht ein", jai);

    let mut release = vec![run(&["umount", "--recursive", ROOT]).soft()];
    if c.passphrase.is_some() {
        release.push(run(&["cryptsetup", "close", MAPPER]).soft());
    }

    let mut summary: Vec<(Said, bool)> = lost.into_iter().map(|line| (line, true)).collect();
    let coded = if c.passphrase.is_some() { (", encrypted (LUKS2)", ", verschlüsselt (LUKS2)") } else { ("", "") };
    summary.push((said(format!("New on {disk}: a boot partition of {} (FAT32) and J.A.I.OS on {} (btrfs{}).", size(ESP_MIB * MIB), size(root_mib * MIB), coded.0), format!("Neu auf {disk}: eine Startpartition mit {} (FAT32) und J.A.I.OS auf {} (btrfs{}).", size(ESP_MIB * MIB), size(root_mib * MIB), coded.1)), false));
    if windows_esp.is_some() {
        summary.push((said("Windows stays as it is. It gets an entry in J.A.I.'s start menu.", "Windows bleibt, wie es ist. Es bekommt einen Eintrag in J.A.I.s Startmenü."), false));
    }
    summary.push((
        match m.secure_boot {
            SecureBoot::Setup => said("Secure Boot: J.A.I. enrolls his own keys with Microsoft's and signs his start.", "Secure Boot: J.A.I. trägt eigene Schlüssel neben Microsofts ein und signiert seinen Start."),
            SecureBoot::On => said("Secure Boot is on with the maker's keys: J.A.I.OS starts only once the firmware is put in Setup Mode (he then enrolls his keys) or Secure Boot is turned off.", "Secure Boot ist mit den Schlüsseln des Herstellers an: J.A.I.OS startet erst, wenn die Firmware im Setup-Modus ist (dann trägt er seine Schlüssel ein) oder Secure Boot aus ist."),
            SecureBoot::Off => said("Secure Boot is off. J.A.I. says at the first start how to turn it on with his keys.", "Secure Boot ist aus. J.A.I. sagt beim ersten Start, wie es mit seinen Schlüsseln angeht."),
        },
        m.secure_boot == SecureBoot::On,
    ));
    summary.push((said(format!("Time zone {}, keyboard {}, this PC is called {host}.", c.zone, c.layout), format!("Zeitzone {}, Tastatur {}, dieser PC heißt {host}.", c.zone, c.layout)), false));
    summary.push((said(format!("User {user} ({full_name}), with root powers through sudo."), format!("Benutzer {user} ({full_name}), mit Root-Rechten über sudo.")), false));
    summary.push((
        if c.main_pc.is_empty() {
            said(format!("This PC becomes a Main PC; {} is its admin account at J.A.I.", c.account), format!("Dieser PC wird ein Haupt-PC; {} ist sein Admin-Konto bei J.A.I.", c.account))
        } else {
            said(format!("This PC belongs to the Main PC {}, as {}.", c.main_pc, c.account), format!("Dieser PC gehört zum Haupt-PC {}, als {}.", c.main_pc, c.account))
        },
        false,
    ));
    summary.push((said(format!("{} packages from CachyOS ({}), Arch and J.A.I.'s repository; his brain: {brain}.", packages().len(), m.level.name()), format!("{} Pakete von CachyOS ({}), Arch und aus J.A.I.s Paketquelle; sein Gehirn: {brain}.", packages().len(), m.level.name())), false));
    Ok(Plan { steps, summary, release })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> Ids {
        Ids { esp: "aaaaaaaa-0000-4000-8000-000000000001".into(), root: "aaaaaaaa-0000-4000-8000-000000000002".into(), fs: "aaaaaaaa-0000-4000-8000-000000000003".into(), luks: "aaaaaaaa-0000-4000-8000-000000000004".into(), fat: "1A2B3C4D".into() }
    }

    fn machine() -> Machine {
        Machine { uefi: true, level: Level::V3, secure_boot: SecureBoot::Setup, vram_mb: Some(12 * 1024), ids: ids() }
    }

    fn choices(target: Target) -> Choices {
        Choices { german: true, zone: "Europe/Berlin".into(), layout: "de".into(), target, passphrase: None, full_name: "Max Mustermann".into(), user: "max".into(), password: "pass word".into(), host: "jaios".into(), main_pc: String::new(), account: "Max".into(), stage: format!("{WORK}/stage") }
    }

    fn free() -> Target {
        Target::Free("/dev/nvme0n1".into())
    }

    /// Sir's PC: Windows 11 (EFI 100 MiB, MSR 16 MiB, C:, recovery 700 MiB), then 200 GiB free.
    #[test]
    fn beside_windows_the_free_space_is_taken_and_nothing_else() {
        let disks = disks(SAMPLE);
        let windows = &disks[0];
        assert_eq!((windows.parts.len(), windows.table.as_str(), windows.has_windows(), windows.beside_windows()), (4, "gpt", true, true));
        assert_eq!(windows.parts.iter().map(Part::called).collect::<Vec<_>>(), ["EFI", "MSR", "WINDOWS", "RECOVERY"]);
        assert_eq!(windows.largest_free(), Some((749068, 953868)), "200 GiB after the recovery partition");
        assert!(!disks[1].beside_windows() && disks[2].live && !disks[0].live);
        let plan = plan(&disks, &choices(free()), &machine()).unwrap();
        let shown = plan.shown();
        // One sgdisk: partitions 5 and 6, on a MiB, 4 GiB to start from and the rest for him.
        assert_eq!(
            shown[0],
            "sgdisk --new=5:749068M:+4096M --typecode=5:ef00 --change-name=5:JAIOS-ESP --partition-guid=5:aaaaaaaa-0000-4000-8000-000000000001 \
             --new=6:753164M:+200704M --typecode=6:8304 --change-name=6:JAIOS --partition-guid=6:aaaaaaaa-0000-4000-8000-000000000002 /dev/nvme0n1"
        );
        // Nothing names a partition of Windows, deletes or wipes the table.
        for line in &shown {
            assert!(!["nvme0n1p", "--zap", "--clear", "--delete", "ntfs"].iter().any(|bad| line.contains(bad)), "{line}");
            assert!(!line.contains("pass word"), "a password in a command: {line}");
        }
        assert!(shown.contains(&"mkfs.fat -F 32 -n JAIOS-ESP -i 1A2B3C4D /dev/disk/by-partuuid/aaaaaaaa-0000-4000-8000-000000000001".to_string()));
        assert!(shown.contains(&"mkfs.btrfs --force --label JAIOS --uuid aaaaaaaa-0000-4000-8000-000000000003 /dev/disk/by-partuuid/aaaaaaaa-0000-4000-8000-000000000002".to_string()));
        assert!(shown.contains(&r"efibootmgr --create --disk /dev/nvme0n1 --part 5 --label J.A.I.OS --loader \EFI\limine\limine_x64.efi".to_string()));
        assert!(shown.iter().any(|l| l.starts_with("pacstrap -C /run/jaios-install/pacman.conf /mnt base ") && l.contains(" linux-cachyos ") && l.ends_with(" jaios")));
        assert!(shown.contains(&"arch-chroot /mnt systemctl enable greetd.service NetworkManager.service bluetooth.service fstrim.timer limine-snapper-sync.service jai-system.service".to_string()));
        assert!(plan.summary.iter().all(|(_, lost)| !lost), "nothing is lost beside Windows");
        assert!(plan.summary.iter().any(|(s, _)| s.de.contains("Windows bleibt")) && plan.summary[0].0.en.contains("196 GiB"), "{:?}", plan.summary[0]);
        // The user: his password on chpasswd's input alone; root is locked, wheel may sudo.
        let fed: Vec<&Action> = plan.steps.iter().flat_map(|s| &s.actions).filter(|a| matches!(a, Action::Run { stdin: Some(_), .. })).collect();
        assert_eq!(fed.len(), 1);
        assert!(matches!(fed[0], Action::Run { argv, stdin: Some(s), .. } if argv.last().unwrap() == "chpasswd" && s == "max:pass word\n"));
        assert!(shown.contains(&"arch-chroot /mnt useradd --create-home --comment Max Mustermann --groups wheel,jai,input,video --shell /bin/bash max".to_string()));
        assert_eq!(plan.file("/mnt/etc/sudoers.d/10-wheel"), Some("%wheel ALL=(ALL:ALL) ALL\n"));
        assert_eq!(plan.file("/mnt/etc/locale.conf"), Some("LANG=de_DE.UTF-8\n"));
        assert_eq!(plan.file("/mnt/etc/vconsole.conf"), Some("KEYMAP=de-latin1\n"));
        assert!(shown.contains(&"arch-chroot /mnt ln -sf /usr/share/zoneinfo/Europe/Berlin /etc/localtime".to_string()));
        // J.A.I. for him: the brain his card takes, the account's files, the shared models.
        let settings = plan.file("/mnt/home/max/.local/share/jai/jai_config.json").unwrap();
        let settings: Value = serde_json::from_str(settings).unwrap();
        assert_eq!((settings["primary_model"].as_str(), settings["main_pc"].as_str()), (Some(crate::config::PRIMARY_MODEL), Some("")));
        assert!(shown.contains(&"cp -a /run/jaios-install/stage/. /mnt/home/max/.local/share/jai/".to_string()));
        assert!(shown.contains(&"arch-chroot /mnt ln -sfn /var/lib/jai/models /home/max/.local/share/jai/models".to_string()));
        assert!(shown.contains(&"arch-chroot /mnt env -C /home/max/.local/share/jai HOME=/home/max /usr/lib/jai/jai --download-models".to_string()));
        assert_eq!(plan.release.iter().map(Action::shown).collect::<Vec<_>>(), ["umount --recursive /mnt"]);
    }

    /// Windows' recovery partition at the very end, the free space before it: the new
    /// partitions end where it starts.
    #[test]
    fn free_space_before_the_recovery_partition_is_taken_up_to_it() {
        let mut disks = disks(SAMPLE);
        let end = (disks[0].size / MIB - 1) * MIB;
        disks[0].parts[2].size -= 200 * GIB;
        disks[0].parts[3].start = end - 700 * MIB;
        let c_ends = (disks[0].parts[2].start + disks[0].parts[2].size).div_ceil(MIB);
        assert_eq!(disks[0].largest_free(), Some((c_ends, 953168)));
        let plan = plan(&disks, &choices(free()), &machine()).unwrap();
        let sgdisk = &plan.shown()[0];
        let root_from = c_ends + 4096;
        assert!(sgdisk.contains(&format!("--new=5:{c_ends}M:+4096M")) && sgdisk.contains(&format!("--new=6:{root_from}M:+{}M", 953168 - root_from)), "{sgdisk}");
        assert!(root_from + (953168 - root_from) <= disks[0].parts[3].start / MIB);
    }

    #[test]
    fn a_whole_disk_is_erased_and_divided_anew() {
        let disks = disks(SAMPLE);
        let plan = plan(&disks, &choices(Target::Erase("/dev/sda".into())), &machine()).unwrap();
        let shown = plan.shown();
        assert_eq!(&shown[..3], ["wipefs --all /dev/sda", "sgdisk --zap-all /dev/sda", "sgdisk --clear /dev/sda"]);
        // 500107862016 bytes are 476940 MiB: from MiB 1 to one MiB before the end.
        assert!(shown[3].starts_with("sgdisk --new=1:1M:+4096M --typecode=1:ef00") && shown[3].contains("--new=2:4097M:+472842M") && shown[3].ends_with(" /dev/sda"), "{}", shown[3]);
        assert!(plan.summary[0].1 && plan.summary[0].0.en.contains("Everything on /dev/sda"));
        // Windows on the other disk keeps its entry; erased with its disk, it has none.
        assert!(plan.file("/mnt/boot/limine.conf").unwrap().contains("/Windows"));
        let gone = super::plan(&disks, &choices(Target::Erase("/dev/nvme0n1".into())), &machine()).unwrap();
        assert!(!gone.file("/mnt/boot/limine.conf").unwrap().contains("/Windows"));
        assert!(gone.summary[1].1 && gone.summary[1].0.de.contains("Windows"));
        // In place of a partition: only that one goes, and its number is used again.
        let replaced = super::plan(&disks, &choices(Target::Replace("/dev/sda1".into())), &machine()).unwrap().shown();
        assert_eq!(replaced[0], "sgdisk --delete=1 /dev/sda");
        assert!(replaced[1].contains("--new=1:1M:+4096M") && replaced[1].contains("--new=2:4097M:+200704M"), "{}", replaced[1]);
    }

    #[test]
    fn encryption_wraps_the_root_and_is_asked_for_at_the_start() {
        let disks = disks(SAMPLE);
        let open = plan(&disks, &choices(free()), &machine()).unwrap();
        assert!(!open.shown().iter().any(|l| l.starts_with("cryptsetup") || l.contains("/dev/mapper")));
        assert!(!open.file("/mnt/etc/mkinitcpio.conf.d/jaios.conf").unwrap().contains("encrypt") && !open.file("/mnt/etc/default/limine").unwrap().contains("rd.luks"));
        let sealed = plan(&disks, &Choices { passphrase: Some("correct horse".into()), ..choices(free()) }, &machine()).unwrap();
        let shown = sealed.shown();
        let part = "/dev/disk/by-partuuid/aaaaaaaa-0000-4000-8000-000000000002";
        assert!(shown[0].contains("--typecode=6:8309"));
        assert!(shown.contains(&format!("cryptsetup luksFormat --type luks2 --batch-mode --uuid aaaaaaaa-0000-4000-8000-000000000004 --key-file - {part}")));
        assert!(shown.contains(&format!("cryptsetup open --key-file - {part} jaios")));
        assert!(shown.contains(&"mkfs.btrfs --force --label JAIOS --uuid aaaaaaaa-0000-4000-8000-000000000003 /dev/mapper/jaios".to_string()));
        assert!(shown.iter().all(|l| !l.contains("correct horse")), "the passphrase is only ever fed in");
        let fed = sealed.steps.iter().flat_map(|s| &s.actions).filter(|a| matches!(a, Action::Run { stdin: Some(s), .. } if s == "correct horse")).count();
        assert_eq!(fed, 2);
        assert!(sealed.file("/mnt/etc/mkinitcpio.conf.d/jaios.conf").unwrap().contains("block sd-encrypt filesystems"));
        assert!(sealed.file("/mnt/etc/default/limine").unwrap().contains("quiet splash rd.luks.name=aaaaaaaa-0000-4000-8000-000000000004=jaios\""));
        assert_eq!(sealed.release.iter().map(Action::shown).collect::<Vec<_>>(), ["umount --recursive /mnt", "cryptsetup close jaios"]);
        assert!(plan(&disks, &Choices { passphrase: Some("short".into()), ..choices(free()) }, &machine()).is_err());
    }

    #[test]
    fn limine_remembers_the_last_entry_and_starts_windows_from_its_own_partition() {
        let _look = crate::holo_theme::look_for_pictures();
        let disks = disks(SAMPLE);
        let plan = plan(&disks, &choices(free()), &machine()).unwrap();
        let conf = plan.file("/mnt/boot/limine.conf").unwrap();
        for line in ["timeout: 3", "remember_last_entry: yes", "default_entry: 2", "interface_branding: J.A.I.OS", "/+J.A.I.OS", "/Windows", "    protocol: efi", "    path: guid(11111111-2222-4333-8444-555555555555):/EFI/Microsoft/Boot/bootmgfw.efi"] {
            assert!(conf.lines().any(|l| l == line), "'{line}' is not in\n{conf}");
        }
        // Global options stand before the first entry, his entry before Windows'.
        assert!(conf.find("timeout").unwrap() < conf.find("/+J.A.I.OS").unwrap() && conf.find("/+J.A.I.OS").unwrap() < conf.find("/Windows").unwrap());
        let defaults = plan.file("/mnt/etc/default/limine").unwrap();
        assert!(defaults.contains("KERNEL_CMDLINE[default]=\"root=UUID=aaaaaaaa-0000-4000-8000-000000000003 rw rootflags=subvol=/@ quiet splash\"\n"), "{defaults}");
        assert!(defaults.contains("TARGET_OS_NAME=\"J.A.I.OS\"\nESP_PATH=\"/boot\"\n") && defaults.contains("ENABLE_ENROLL_LIMINE_CONFIG=yes"));
        // Setup Mode: keys before the kernels' entries, the menu's checksum into Limine, then
        // the signature; no kernel is signed.
        let shown = plan.shown();
        let at = |what: &str| shown.iter().position(|l| l == what).unwrap_or_else(|| panic!("{what} is not planned"));
        let order = [at("arch-chroot /mnt sbctl create-keys"), at("arch-chroot /mnt sbctl enroll-keys --microsoft"), at("arch-chroot /mnt limine-update"), at("arch-chroot /mnt limine-enroll-config"), at("arch-chroot /mnt sbctl sign --save /boot/EFI/limine/limine_x64.efi")];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        assert!(!shown.iter().any(|l| l.contains("sbctl sign") && l.contains("vmlinuz")));
        // The maker's keys stay: nothing is enrolled (his root core finishes it later), and the
        // summary warns.
        let kept = super::plan(&disks, &choices(free()), &Machine { secure_boot: SecureBoot::On, ..machine() }).unwrap();
        assert!(!kept.shown().iter().any(|l| l.contains("/mnt sbctl") || l.contains("enroll-config")));
        assert!(kept.file("/mnt/etc/default/limine").unwrap().contains("ENABLE_ENROLL_LIMINE_CONFIG=no") && kept.summary.iter().any(|(s, warn)| *warn && s.en.contains("Setup Mode")));
    }

    #[test]
    fn fstab_mounts_every_subvolume_and_the_boot_partition() {
        let plan = plan(&disks(SAMPLE), &choices(free()), &machine()).unwrap();
        let fstab = plan.file("/mnt/etc/fstab").unwrap();
        let lines: Vec<Vec<&str>> = fstab.lines().filter(|l| !l.starts_with('#')).map(|l| l.split_whitespace().collect()).collect();
        assert_eq!(lines.len(), 8);
        assert_eq!(lines[0], ["UUID=aaaaaaaa-0000-4000-8000-000000000003", "/", "btrfs", "subvol=/@,noatime,compress=zstd", "0", "0"]);
        assert_eq!(lines[5], ["UUID=aaaaaaaa-0000-4000-8000-000000000003", "/var/tmp", "btrfs", "subvol=/@tmp,noatime,compress=zstd", "0", "0"]);
        assert_eq!(lines[7], ["UUID=1A2B-3C4D", "/boot", "vfat", "umask=0077", "0", "2"]);
        let shown = plan.shown();
        let dev = "/dev/disk/by-partuuid/aaaaaaaa-0000-4000-8000-000000000002";
        for (volume, at) in SUBVOLUMES {
            assert!(shown.contains(&format!("btrfs subvolume create /mnt/{volume}")));
            assert!(shown.contains(&format!("mount --mkdir -o noatime,compress=zstd,subvol=/{volume} {dev} /mnt{}", at.trim_end_matches('/'))), "{volume}");
        }
        // The root is mounted before what lies in it, the boot partition last.
        let at = |what: &str| shown.iter().position(|l| l.ends_with(what)).unwrap();
        assert!(at(&format!("subvol=/@ {dev} /mnt")) < at(" /mnt/home") && at(" /mnt/var/log") < at(" /mnt/boot") && at(" /mnt/boot") < shown.iter().position(|l| l.starts_with("pacstrap")).unwrap());
    }

    #[test]
    fn pacman_takes_cachyos_repositories_of_the_processors_level() {
        let sections = |level| pacman_conf(level).lines().filter(|l| l.starts_with('[')).map(str::to_string).collect::<Vec<_>>();
        assert_eq!(sections(Level::Generic), ["[options]", "[jaios]", "[cachyos]", "[core]", "[extra]", "[multilib]"]);
        assert_eq!(sections(Level::V3), ["[options]", "[jaios]", "[cachyos-v3]", "[cachyos-core-v3]", "[cachyos-extra-v3]", "[cachyos]", "[core]", "[extra]", "[multilib]"]);
        assert_eq!(sections(Level::V4)[2..5], ["[cachyos-v4]", "[cachyos-core-v4]", "[cachyos-extra-v4]"]);
        assert_eq!(sections(Level::Znver4)[2..5], ["[cachyos-znver4]", "[cachyos-core-znver4]", "[cachyos-extra-znver4]"]);
        assert!(pacman_conf(Level::V3).contains("[cachyos-v3]\nInclude = /etc/pacman.d/cachyos-v3-mirrorlist\n") && pacman_conf(Level::V3).contains("Architecture = x86_64 x86_64_v3\n"));
        assert!(pacman_conf(Level::Znver4).contains("[cachyos-znver4]\nInclude = /etc/pacman.d/cachyos-v4-mirrorlist\n"));
        assert!(pacman_conf(Level::V4).contains("[jaios]\nSigLevel = Optional TrustAll\nServer = https://github.com/JayKaey/JAI/releases/download/jaios-repo\n"));
        // Zen 4 and 5 take znver4, Zen 3 (no AVX-512) v3, Intel's AVX-512 v4.
        assert_eq!([Level::of(Some(0x19), true, true), Level::of(Some(0x1A), true, true), Level::of(Some(0x19), true, false), Level::of(None, true, true), Level::of(Some(0x17), true, false), Level::of(None, false, false)], [Level::Znver4, Level::Znver4, Level::V3, Level::V4, Level::V3, Level::Generic]);
        assert_ne!(Level::of_this_pc("vendor_id\t: GenuineIntel\ncpu family\t: 6\n"), Level::Znver4);
        // The new system and the installation read the same configuration.
        let plan = plan(&disks(SAMPLE), &choices(free()), &machine()).unwrap();
        assert_eq!(plan.file("/run/jaios-install/pacman.conf"), plan.file("/mnt/etc/pacman.conf"));
        // The list: one name a line, each once, what the owner asked for among them.
        let packages = packages();
        for wanted in ["base", "linux-cachyos", "linux-cachyos-headers", "limine", "limine-snapper-sync", "snapper", "snap-pac", "sbctl", "plymouth", "greetd", "kwin", "steam", "proton-cachyos", "firefox", "discover", "flatpak", "paru", "chwd", "jai", "jaios-settings", "jaios"] {
            assert_eq!(packages.iter().filter(|p| **p == wanted).count(), 1, "{wanted}");
        }
        assert!(packages.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-+._".contains(c))), "{packages:?}");
    }

    #[test]
    fn what_must_never_be_done_is_refused() {
        let disks = disks(SAMPLE);
        let refused = |target: Target| plan(&disks, &choices(target), &machine()).err().expect("refused");
        // The stick itself, either way.
        assert!(refused(Target::Erase("/dev/sdb".into())).en.contains("the stick"));
        assert!(refused(Target::Replace("/dev/sdb1".into())).de.contains("Stick"));
        // Every partition of Windows, and the one this PC starts from.
        for part in ["/dev/nvme0n1p1", "/dev/nvme0n1p2", "/dev/nvme0n1p3", "/dev/nvme0n1p4"] {
            assert!(refused(Target::Replace(part.into())).en.contains("never takes it"), "{part}");
        }
        // Too little room: free space of 20 GiB, a partition of 20 GiB, a disk of 14 GiB.
        let mut small = disks.clone();
        small[0].parts[2].size += 180 * GIB;
        small[0].parts[3].start += 180 * GIB;
        small[1].parts[0].size = 20 * GIB;
        small[2].live = false;
        small[2].parts.clear();
        let refused_small = |target: Target| plan(&small, &choices(target), &machine()).err().expect("refused");
        assert!(refused_small(free()).en.contains("needs 40 GiB of room; there are 20 GiB"));
        assert!(refused_small(Target::Replace("/dev/sda1".into())).de.contains("braucht 40 GiB"));
        assert!(refused_small(Target::Erase("/dev/sdb".into())).en.contains("14 GiB"));
        // A disk in use, a disk that is not there, a stick started the BIOS way.
        let mut used = disks.clone();
        used[1].parts[0].mounted = true;
        assert!(plan(&used, &choices(Target::Erase("/dev/sda".into())), &machine()).err().unwrap().en.contains("in use"));
        assert!(refused(Target::Erase("/dev/sdz".into())).en.contains("Choose a disk"));
        assert!(plan(&disks, &choices(free()), &Machine { uefi: false, ..machine() }).err().unwrap().en.contains("UEFI"));
        // Words that would end up in a command: a path as a time zone, a user called root.
        for bad in [Choices { zone: "../../etc/shadow".into(), ..choices(free()) }, Choices { user: "root".into(), ..choices(free()) }, Choices { user: "Max M".into(), ..choices(free()) }, Choices { host: "my pc".into(), ..choices(free()) }, Choices { password: String::new(), ..choices(free()) }, Choices { layout: "de;reboot".into(), ..choices(free()) }] {
            assert!(plan(&disks, &bad, &machine()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn zones_and_layouts_are_read_from_the_systems_own_lists() {
        assert_eq!(zones("# comment\nDE\t+5230+01322\tEurope/Berlin\tmost of Germany\nUS\t+404251-0740023\tAmerica/New_York\tEastern\n"), ["America/New_York", "Europe/Berlin", "UTC"]);
        let lst = "! model\n  pc105  Generic 105-key PC\n\n! layout\n  al              Albanian\n  us              English (US)\n  de              German\n\n! variant\n  nodeadkeys  de: German (no dead keys)\n";
        assert_eq!(layouts(lst), [("de".to_string(), "German".to_string()), ("us".into(), "English (US)".into()), ("al".into(), "Albanian".into())]);
        assert_eq!(layouts("")[0].0, "de");
        assert_eq!([size(200 * GIB), size(4 * GIB), size(700 * MIB)], ["200 GiB", "4.0 GiB", "700 MiB"]);
    }
}
