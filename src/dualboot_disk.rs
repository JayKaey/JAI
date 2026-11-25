//! The disks' side of J.A.I.OS beside Windows (`dualboot.rs` holds the orders): what this PC is
//! (UEFI, Secure Boot, GPT, BitLocker, Fast Startup, the room after C:), which drives are USB
//! sticks, and the three steps that need rights J.A.I. himself does not run with:
//! - `--make-room <MB>`: C: shrunk through Windows' own storage API (MSFT_Partition's
//!   GetSupportedSize and Resize over WMI), which works with BitLocker on,
//! - `--write-stick …`: the install image written to a USB stick and read back,
//! - `--fast-startup-off`: Windows' Fast Startup switched off.
//!
//! Each runs in this same program started with rights (`run_elevated`: Windows asks Sir by its
//! own question, on Linux polkit's `pkexec` does), tells how far it is in a progress file and
//! leaves its outcome in a result file J.A.I. reads.
//!
//! Safety: a stick is a USB drive that holds nothing of this system and is no larger than a
//! stick (`stick_refusal`); the step with rights finds the drive again by number, size and
//! serial before it touches it; the image's first block (its partition table) is written last,
//! and only when the image's SHA-256 is the one asked for; then everything is read back. No
//! test, scripted session or dry run reaches any of it (`hands_tied`).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

pub const GIB: u64 = 1 << 30;
/// What the installer needs beside Windows.
pub const NEEDS: u64 = 40 * GIB;
/// What Windows keeps free on C: after giving room.
pub const KEEP_FREE: u64 = 30 * GIB;
/// The room he suggests.
pub const SUGGESTED: u64 = 100 * GIB;
/// A USB drive larger than this is an external disk with someone's files, not a stick.
const LARGEST_STICK: u64 = 300_000_000_000;

/// One object's properties as WMI gives them, each as text.
#[cfg(any(windows, test))]
pub type Row = std::collections::HashMap<String, String>;

#[cfg(any(windows, test))]
fn text<'a>(row: &'a Row, name: &str) -> &'a str {
    row.get(name).map_or("", |v| v.trim())
}

#[cfg(any(windows, test))]
fn num(row: &Row, name: &str) -> u64 {
    text(row, name).parse().unwrap_or(0)
}

#[cfg(any(windows, test))]
fn flag(row: &Row, name: &str) -> bool {
    matches!(text(row, name).to_ascii_lowercase().as_str(), "true" | "-1" | "1")
}

/// A drive letter as WMI gives it: a char16 (as its number) or the letter itself.
#[cfg(any(windows, test))]
fn letter(value: &str) -> Option<char> {
    let c = match value.trim().parse::<u32>() {
        Ok(code) => char::from_u32(code)?,
        Err(_) => value.trim().chars().next().filter(|_| value.trim().chars().count() == 1)?,
    };
    c.is_ascii_alphabetic().then(|| c.to_ascii_uppercase())
}

// --------------------------------------------------------------------------------- drives ----

#[derive(Clone, Debug, PartialEq)]
pub struct Disk {
    /// Windows' disk number ("3"), on Linux the device ("/dev/sdb").
    pub id: String,
    pub name: String,
    pub serial: String,
    pub size: u64,
    pub usb: bool,
    /// It holds the running system or what starts it.
    pub system: bool,
    /// Its drive letters, on Linux where its partitions are mounted.
    pub mounts: Vec<String>,
}

/// A serial as it is passed to the step with rights and compared there.
pub fn plain_serial(serial: &str) -> String {
    serial.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect()
}

/// Why a drive is not written to (None: it is a stick).
pub fn stick_refusal(disk: &Disk) -> Option<&'static str> {
    if disk.system {
        Some("it holds this system")
    } else if !disk.usb {
        Some("it is no USB drive")
    } else if disk.size == 0 {
        Some("there is no medium in it")
    } else if disk.size > LARGEST_STICK {
        Some("it is larger than a stick, an external drive with files on it I take it")
    } else {
        None
    }
}

/// The drives from MSFT_Disk and MSFT_Partition; `windows` is the letter Windows runs from.
#[cfg(any(windows, test))]
pub fn disks_from_wmi(disks: &[Row], parts: &[Row], windows: char) -> Vec<Disk> {
    disks
        .iter()
        .map(|d| {
            let own: Vec<&Row> = parts.iter().filter(|p| text(p, "DiskNumber") == text(d, "Number")).collect();
            let mounts: Vec<String> = own.iter().filter_map(|p| letter(text(p, "DriveLetter"))).map(String::from).collect();
            Disk {
                id: text(d, "Number").to_string(),
                name: text(d, "FriendlyName").to_string(),
                serial: text(d, "SerialNumber").to_string(),
                size: num(d, "Size"),
                usb: text(d, "BusType") == "7",
                system: flag(d, "IsBoot") || flag(d, "IsSystem") || own.iter().any(|p| flag(p, "IsBoot") || flag(p, "IsSystem")) || mounts.iter().any(|m| m.starts_with(windows.to_ascii_uppercase())),
                mounts,
            }
        })
        .collect()
}

/// The drives from `lsblk -J -b -o NAME,PATH,SIZE,TRAN,TYPE,MODEL,SERIAL,MOUNTPOINTS`.
#[cfg(any(not(windows), test))]
pub fn disks_from_lsblk(json: &str) -> Vec<Disk> {
    fn mounts(device: &serde_json::Value, into: &mut Vec<String>) {
        into.extend(device["mountpoints"].as_array().into_iter().flatten().filter_map(|m| m.as_str()).map(String::from));
        for child in device["children"].as_array().into_iter().flatten() {
            mounts(child, into);
        }
    }
    let Ok(all) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    all["blockdevices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["type"] == "disk")
        .map(|d| {
            let mut at = Vec::new();
            mounts(d, &mut at);
            Disk {
                id: d["path"].as_str().unwrap_or("").to_string(),
                name: d["model"].as_str().unwrap_or("").trim().to_string(),
                serial: d["serial"].as_str().unwrap_or("").trim().to_string(),
                size: d["size"].as_u64().or_else(|| d["size"].as_str()?.parse().ok()).unwrap_or(0),
                usb: d["tran"] == "usb",
                // (Anything mounted outside the places a plugged-in drive is mounted at.)
                system: at.iter().any(|m| !["/run/media/", "/media/", "/mnt/"].iter().any(|p| m.starts_with(p))),
                mounts: at,
            }
        })
        .collect()
}

/// Windows' own partition among the storage API's rows.
#[cfg(any(windows, test))]
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub gpt: bool,
    pub size: u64,
    /// Unallocated bytes right after it.
    pub after: u64,
    /// Its object path, for GetSupportedSize and Resize.
    pub path: String,
}

#[cfg(any(windows, test))]
pub fn windows_layout(disks: &[Row], parts: &[Row], windows: char) -> Option<Layout> {
    let own = parts.iter().find(|p| letter(text(p, "DriveLetter")) == Some(windows.to_ascii_uppercase()))?;
    let disk = disks.iter().find(|d| text(d, "Number") == text(own, "DiskNumber"))?;
    let end = num(own, "Offset") + num(own, "Size");
    // (The last megabyte of a GPT disk is its second table.)
    let next = parts.iter().filter(|p| text(p, "DiskNumber") == text(own, "DiskNumber")).map(|p| num(p, "Offset")).filter(|at| *at >= end).min().unwrap_or(num(disk, "Size").saturating_sub(1 << 20));
    Some(Layout { gpt: text(disk, "PartitionStyle") == "2", size: num(own, "Size"), after: next.saturating_sub(end), path: text(own, "__PATH").to_string() })
}

/// How much C: gives: the most, and what is taken.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Room {
    pub most: u64,
    pub take: u64,
}

/// The room C: can give, in whole GiB: no more than Windows' own smallest size allows
/// (`size_min`, from GetSupportedSize; unknown without rights) and never leaving C: under
/// `KEEP_FREE`. `asked`: the bytes Sir named (else `SUGGESTED`, or the most there is).
pub fn room_plan(size: u64, free: u64, size_min: Option<u64>, asked: Option<u64>) -> Result<Room, String> {
    let gb = |bytes: u64| bytes / GIB;
    let most = free.saturating_sub(KEEP_FREE).min(size_min.map_or(u64::MAX, |min| size.saturating_sub(min))) / GIB * GIB;
    if most < NEEDS {
        return Err(format!("C: can give up only {} GB right now, Sir, and J.A.I.OS needs {}. Windows keeps {} GB free for itself; clearing files off C: makes more.", gb(most), gb(NEEDS), gb(KEEP_FREE)));
    }
    let take = asked.unwrap_or(SUGGESTED.min(most)) / GIB * GIB;
    if take < NEEDS {
        return Err(format!("J.A.I.OS needs at least {} GB, Sir.", gb(NEEDS)));
    }
    if take > most {
        return Err(format!("C: can give up {} GB at most, Sir.", gb(most)));
    }
    Ok(Room { most, take })
}

/// What this PC is, for the readiness report (None: not known).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub windows: bool,
    pub uefi: Option<bool>,
    pub secure_boot: Option<bool>,
    pub gpt: Option<bool>,
    pub bitlocker: Option<bool>,
    pub fast_startup: Option<bool>,
    pub c_size: Option<u64>,
    pub c_free: Option<u64>,
    pub unallocated: Option<u64>,
    pub size_min: Option<u64>,
}

// ------------------------------------------------------------------------------ the image ----

const CHUNK: usize = 4 << 20;
/// Unbuffered reads and writes are whole sectors from a sector-aligned address.
const ALIGN: usize = 4096;

fn padded(len: usize) -> usize {
    len.div_ceil(ALIGN) * ALIGN
}

/// A block of memory that starts on a sector boundary.
struct Aligned(Vec<u8>, usize);

impl Aligned {
    fn new() -> Self {
        let raw = vec![0u8; CHUNK + ALIGN];
        let at = raw.as_ptr().align_offset(ALIGN);
        Self(raw, at)
    }

    fn bytes(&mut self) -> &mut [u8] {
        &mut self.0[self.1..self.1 + CHUNK]
    }
}

fn hex(hash: ring::digest::Context) -> String {
    hash.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// `len` bytes of `src` onto `dst` in aligned blocks (the last one filled up with zeros), then
/// read back and compared: the image's SHA-256. The first block is written last, and only if
/// the image is the one `expected`: a write that breaks off, or a wrong image, leaves nothing
/// that starts. `told("write" | "verify", percent)`.
pub fn write_image(src: &mut impl Read, dst: &mut (impl Read + Write + Seek), len: u64, expected: Option<&str>, told: &mut dyn FnMut(&str, u8)) -> Result<String, String> {
    let io = |e: std::io::Error| e.to_string();
    let percent = |done: u64| (done * 100 / len.max(1)) as u8;
    let (mut first, mut block) = (Aligned::new(), Aligned::new());
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let first_len = CHUNK.min(len as usize);
    src.read_exact(&mut first.bytes()[..first_len]).map_err(io)?;
    hash.update(&first.bytes()[..first_len]);
    let mut done = first_len as u64;
    dst.seek(SeekFrom::Start(padded(first_len) as u64)).map_err(io)?;
    while done < len {
        let n = CHUNK.min((len - done) as usize);
        let bytes = block.bytes();
        src.read_exact(&mut bytes[..n]).map_err(io)?;
        bytes[n..padded(n)].fill(0);
        hash.update(&bytes[..n]);
        dst.write_all(&bytes[..padded(n)]).map_err(io)?;
        done += n as u64;
        told("write", percent(done));
    }
    let sha = hex(hash);
    if expected.is_some_and(|e| !e.eq_ignore_ascii_case(&sha)) {
        return Err("the image is not the one that was checked (its SHA-256 differs); the stick was left without a start block".into());
    }
    dst.seek(SeekFrom::Start(0)).map_err(io)?;
    dst.write_all(&first.bytes()[..padded(first_len)]).map_err(io)?;
    dst.flush().map_err(io)?;
    told("write", 100);

    dst.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut done = 0u64;
    while done < len {
        let n = CHUNK.min((len - done) as usize);
        let bytes = block.bytes();
        dst.read_exact(&mut bytes[..padded(n)]).map_err(io)?;
        hash.update(&bytes[..n]);
        done += n as u64;
        told("verify", percent(done));
    }
    if hex(hash) != sha {
        return Err("what the stick gives back is not what was written; the stick may be worn out".into());
    }
    Ok(sha)
}

// ------------------------------------------------------------------- the step with rights ----

#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    MakeRoom { mb: u64 },
    WriteStick { disk: String, size: u64, serial: String, iso: PathBuf, sha: String },
    FastStartupOff,
}

impl Job {
    /// The command line that asks for it; `result` is where its outcome is left.
    pub fn to_args(&self, result: &Path) -> Vec<String> {
        let mut args = match self {
            Job::MakeRoom { mb } => vec!["--make-room".to_string(), mb.to_string()],
            Job::WriteStick { disk, size, serial, iso, sha } => {
                ["--write-stick", "--disk", disk, "--size", &size.to_string(), "--serial", &plain_serial(serial), "--iso", &iso.to_string_lossy(), "--sha", sha].map(String::from).to_vec()
            }
            Job::FastStartupOff => vec!["--fast-startup-off".to_string()],
        };
        args.extend(["--result".to_string(), result.to_string_lossy().into_owned()]);
        args
    }

    pub fn parse(args: &[String]) -> Result<(Job, PathBuf), String> {
        let named = |name: &str| args.iter().position(|a| a == name).and_then(|at| args.get(at + 1)).ok_or_else(|| format!("{name} is missing"));
        let result = PathBuf::from(named("--result")?);
        let job = match args.first().map(String::as_str) {
            Some("--make-room") => Job::MakeRoom { mb: args.get(1).and_then(|mb| mb.parse().ok()).filter(|mb| *mb > 0).ok_or("--make-room takes megabytes")? },
            Some("--write-stick") => {
                let sha = named("--sha")?.to_ascii_lowercase();
                if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("--sha is no SHA-256".into());
                }
                Job::WriteStick { disk: named("--disk")?.clone(), size: named("--size")?.parse().map_err(|_| "--size takes bytes")?, serial: named("--serial")?.clone(), iso: PathBuf::from(named("--iso")?), sha }
            }
            Some("--fast-startup-off") => Job::FastStartupOff,
            other => return Err(format!("no step of J.A.I.OS's: {other:?}")),
        };
        Ok((job, result))
    }
}

/// One argument as Windows' command line takes it (none of ours holds a quote).
#[cfg(any(windows, test))]
fn quoted(arg: &str) -> Result<String, String> {
    if arg.contains('"') || arg.ends_with('\\') {
        return Err(format!("an argument Windows' command line would misread: {arg}"));
    }
    Ok(if arg.is_empty() || arg.contains([' ', '\t']) { format!("\"{arg}\"") } else { arg.to_string() })
}

fn progress_file(result: &Path) -> PathBuf {
    result.with_extension("progress")
}

/// What a finished step left in its result file: its words, as Ok or Err.
pub fn read_result(result: &Path) -> Option<Result<String, String>> {
    let left: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(result).ok()?).ok()?;
    let message = left["message"].as_str()?.to_string();
    Some(if left["ok"].as_bool()? { Ok(message) } else { Err(message) })
}

fn write_result(result: &Path, outcome: &Result<String, String>) {
    let (ok, message) = match outcome {
        Ok(m) => (true, m),
        Err(m) => (false, m),
    };
    let _ = std::fs::write(result, serde_json::json!({ "ok": ok, "message": message }).to_string());
}

/// `JAI --make-room …`, `--write-stick …`, `--fast-startup-off`: the step itself, run with
/// rights. Its outcome goes into the result file and the exit code.
pub fn helper(args: &[String]) -> ExitCode {
    let (job, result) = match Job::parse(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let progress = progress_file(&result);
    let mut last = (String::new(), 0u8);
    let outcome = carry_out(&job, &mut |phase, percent| {
        if last != (phase.to_string(), percent) {
            last = (phase.to_string(), percent);
            let _ = std::fs::write(&progress, format!("{phase} {percent}"));
        }
    });
    write_result(&result, &outcome);
    if outcome.is_ok() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn carry_out(job: &Job, told: &mut dyn FnMut(&str, u8)) -> Result<String, String> {
    match job {
        Job::MakeRoom { mb } => os::make_room(*mb),
        Job::FastStartupOff => os::fast_startup_off(),
        Job::WriteStick { disk, size, serial, iso, sha } => {
            // The drive again, as it is now: the same number, size and serial, and still a stick.
            let all = disks()?;
            let stick = all.iter().find(|d| d.id == *disk).ok_or("the stick is gone")?;
            if stick.size != *size || plain_serial(&stick.serial) != *serial {
                return Err("the drive under that number is not the stick that was asked about".into());
            }
            if let Some(why) = stick_refusal(stick) {
                return Err(format!("{} is not written to: {why}", stick.name));
            }
            let len = std::fs::metadata(iso).map_err(|e| format!("the image is not there: {e}"))?.len();
            if len == 0 || len.div_ceil(ALIGN as u64) * ALIGN as u64 > stick.size {
                return Err("the image does not fit on the stick".into());
            }
            let mut src = std::fs::File::open(iso).map_err(|e| e.to_string())?;
            let mut dst = os::open_for_writing(stick)?;
            write_image(&mut src, &mut dst, len, Some(sha), told)?;
            // (Written through and read back already: a drive that knows no flush is no fault.)
            let _ = dst.sync_all();
            Ok(format!("written to {} and read back the same", stick.name))
        }
    }
}

/// No test, scripted session or dry run changes a disk or asks for rights.
pub fn hands_tied() -> bool {
    cfg!(test) || crate::platform::in_test_session() || crate::platform::hands_off() || crate::live_self_tester::dry()
}

/// Who asks Sir for the rights.
pub const ASKS: &str = if cfg!(windows) { "Windows" } else { "The system" };

/// Runs a step with rights and waits for it: its words. `told(phase, percent)` hears how far
/// the stick is, twice a second.
pub fn run_elevated(job: &Job, told: &mut dyn FnMut(&str, u8)) -> Result<String, String> {
    if hands_tied() {
        return Err("a test session changes no disk".into());
    }
    let result = std::env::temp_dir().join(format!("jai-os-{}.json", uuid::Uuid::new_v4()));
    let progress = progress_file(&result);
    let mut running = os::start(&job.to_args(&result))?;
    let code = loop {
        if let Some(code) = running.finished(Duration::from_millis(500)) {
            break code;
        }
        if let Some((phase, percent)) = std::fs::read_to_string(&progress).ok().as_deref().and_then(|p| p.split_once(' ')) {
            told(phase, percent.trim().parse().unwrap_or(0));
        }
    };
    let left = read_result(&result);
    let _ = std::fs::remove_file(&result);
    let _ = std::fs::remove_file(&progress);
    // (pkexec: 126 when the question was dismissed, 127 when the rights were not given.)
    left.unwrap_or_else(|| Err(if cfg!(not(windows)) && matches!(code, 126 | 127) { DECLINED.to_string() } else { format!("the step ended with code {code} and left no word") }))
}

const DECLINED: &str = "the question for the rights was declined, so nothing was changed";

pub use os::{disks, facts};

// -------------------------------------------------------------------------------- Windows ----

#[cfg(windows)]
mod os {
    use super::{disks_from_wmi, num, room_plan, text, windows_layout, Disk, Facts, Row, DECLINED, GIB};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::time::Duration;
    use windows::core::{w, BSTR, HRESULT, HSTRING, PCWSTR, VARIANT};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE};
    use windows::Win32::System::Registry::{RegGetValueW, RegSetKeyValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
    use windows::Win32::System::Wmi::{IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_GENERIC_FLAG_TYPE, WBEM_INFINITE};

    fn told(e: windows::core::Error) -> String {
        e.message().to_string()
    }

    /// Windows' storage management API (root\Microsoft\Windows\Storage).
    struct Storage(IWbemServices);

    const DISK_FIELDS: &[&str] = &["Number", "FriendlyName", "SerialNumber", "Size", "BusType", "IsBoot", "IsSystem", "PartitionStyle", "__PATH"];
    const PART_FIELDS: &[&str] = &["DiskNumber", "DriveLetter", "Size", "Offset", "IsBoot", "IsSystem", "__PATH"];

    fn read(object: &IWbemClassObject, fields: &[&str]) -> Row {
        fields
            .iter()
            .filter_map(|field| {
                let mut value = VARIANT::default();
                // SAFETY: a named property read into a local VARIANT.
                unsafe { object.Get(&HSTRING::from(*field), 0, &mut value, None, None) }.ok()?;
                Some((field.to_string(), value.to_string()))
            })
            .collect()
    }

    impl Storage {
        fn open() -> Result<Self, String> {
            // SAFETY: COM on this thread, a local connection, the caller's own identity.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).map_err(told)?;
                let services = locator.ConnectServer(&BSTR::from(r"ROOT\Microsoft\Windows\Storage"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None).map_err(told)?;
                // (RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE.)
                CoSetProxyBlanket(&services, 10, 0, PCWSTR::null(), RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE).map_err(told)?;
                Ok(Self(services))
            }
        }

        fn all(&self, class: &str, fields: &[&str]) -> Result<Vec<Row>, String> {
            // SAFETY: a forward-only query, each object read as it comes.
            unsafe {
                // (WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY.)
                let found = self.0.ExecQuery(&BSTR::from("WQL"), &BSTR::from(format!("SELECT * FROM {class}")), WBEM_GENERIC_FLAG_TYPE(48), None).map_err(told)?;
                let mut rows = Vec::new();
                loop {
                    let mut one = [None];
                    let mut got = 0u32;
                    let _ = found.Next(WBEM_INFINITE, &mut one, &mut got);
                    let Some(object) = one[0].take().filter(|_| got == 1) else { break };
                    rows.push(read(&object, fields));
                }
                Ok(rows)
            }
        }

        fn rows(&self) -> Result<(Vec<Row>, Vec<Row>), String> {
            Ok((self.all("MSFT_Disk", DISK_FIELDS)?, self.all("MSFT_Partition", PART_FIELDS)?))
        }

        /// A method of the object at `path`: what it gives back, by name. (WMI takes a uint64 as
        /// text.)
        fn call(&self, path: &str, class: &str, method: &str, args: &[(&str, VARIANT)], outs: &[&str]) -> Result<Row, String> {
            // SAFETY: the method's own input object, filled by name; every out pointer is local.
            unsafe {
                let mut of_class = None;
                self.0.GetObject(&BSTR::from(class), WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut of_class), None).map_err(told)?;
                let of_class = of_class.ok_or("the storage API has no such class")?;
                let mut signature = None;
                of_class.GetMethod(&HSTRING::from(method), 0, &mut signature, std::ptr::null_mut()).map_err(told)?;
                let input = match signature {
                    Some(signature) => Some(signature.SpawnInstance(0).map_err(told)?),
                    None => None,
                };
                if let Some(input) = &input {
                    for (name, value) in args {
                        input.Put(&HSTRING::from(*name), 0, value, 0).map_err(told)?;
                    }
                }
                let mut out = None;
                self.0.ExecMethod(&BSTR::from(path), &BSTR::from(method), WBEM_GENERIC_FLAG_TYPE(0), None, input.as_ref(), Some(&mut out), None).map_err(told)?;
                Ok(out.map(|o| read(&o, outs)).unwrap_or_default())
            }
        }

        /// The smallest size Windows would shrink the partition to (rights needed).
        fn size_min(&self, path: &str) -> Result<u64, String> {
            let supported = self.call(path, "MSFT_Partition", "GetSupportedSize", &[], &["ReturnValue", "SizeMin"])?;
            match text(&supported, "ReturnValue") {
                "0" => Ok(num(&supported, "SizeMin")),
                code => Err(format!("Windows does not say how far C: shrinks (storage error {code})")),
            }
        }
    }

    fn windows_letter() -> char {
        std::env::var("SystemDrive").ok().and_then(|d| d.chars().next()).filter(char::is_ascii_alphabetic).unwrap_or('C')
    }

    const CONTROL: &str = r"SYSTEM\CurrentControlSet\Control";
    const POWER: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Power";

    fn reg_dword(path: &str, name: &str) -> Option<u32> {
        let (mut value, mut size) = (0u32, 4u32);
        // SAFETY: a sized read into a local.
        unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(path), &HSTRING::from(name), RRF_RT_REG_DWORD, None, Some((&mut value as *mut u32).cast()), Some(&mut size)) }.ok().ok()?;
        Some(value)
    }

    const SECURE_BOOT: &str = r"SYSTEM\CurrentControlSet\Control\SecureBoot\State";

    /// (Not every Windows keeps the firmware's kind; the Secure Boot state is kept by UEFI PCs
    /// only.)
    fn uefi() -> Option<bool> {
        reg_dword(CONTROL, "PEFirmwareType").map(|kind| kind == 2).or_else(|| reg_dword(SECURE_BOOT, "UEFISecureBootEnabled").map(|_| true))
    }

    fn free_on(letter: char) -> Option<u64> {
        let mut free = 0u64;
        // SAFETY: one out value.
        unsafe { windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(&HSTRING::from(format!("{letter}:\\")), None, None, Some(&mut free)) }.ok()?;
        Some(free)
    }

    /// Explorer's own word on the drive (no rights needed): protected, being encrypted or
    /// suspended is "on".
    fn bitlocker(letter: char) -> Option<bool> {
        use windows::Win32::UI::Shell::PropertiesSystem::{PSGetPropertyKeyFromName, PROPERTYKEY};
        use windows::Win32::UI::Shell::{IShellItem2, SHCreateItemFromParsingName};
        // SAFETY: a property of a shell item, read into a local.
        unsafe {
            let mut key = PROPERTYKEY::default();
            PSGetPropertyKeyFromName(w!("System.Volume.BitLockerProtection"), &mut key).ok()?;
            let drive: IShellItem2 = SHCreateItemFromParsingName(&HSTRING::from(format!("{letter}:\\")), None).ok()?;
            let state = i32::try_from(&drive.GetProperty(&key).ok()?).ok()?;
            Some(matches!(state, 1 | 3 | 5))
        }
    }

    pub fn disks() -> Result<Vec<Disk>, String> {
        let (disks, parts) = Storage::open()?.rows()?;
        Ok(disks_from_wmi(&disks, &parts, windows_letter()))
    }

    pub fn facts() -> Facts {
        let letter = windows_letter();
        let uefi = uefi();
        let mut facts = Facts {
            windows: true,
            uefi,
            secure_boot: reg_dword(SECURE_BOOT, "UEFISecureBootEnabled").map(|on| on == 1).or(uefi.filter(|u| !u)),
            fast_startup: Some(reg_dword(POWER, "HiberbootEnabled") == Some(1)),
            c_free: free_on(letter),
            ..Facts::default()
        };
        if let Ok(storage) = Storage::open() {
            if let Some(own) = storage.rows().ok().and_then(|(disks, parts)| windows_layout(&disks, &parts, letter)) {
                (facts.gpt, facts.c_size, facts.unallocated) = (Some(own.gpt), Some(own.size), Some(own.after));
                facts.size_min = storage.size_min(&own.path).ok();
            }
        }
        facts.bitlocker = bitlocker(letter);
        facts
    }

    pub fn make_room(mb: u64) -> Result<String, String> {
        if uefi() == Some(false) {
            return Err("this PC does not start by UEFI".into());
        }
        let letter = windows_letter();
        let storage = Storage::open()?;
        let (disks, parts) = storage.rows()?;
        let own = windows_layout(&disks, &parts, letter).ok_or("Windows' own partition was not found")?;
        if !own.gpt {
            return Err("Windows' disk is not a GPT disk".into());
        }
        let free = free_on(letter).ok_or("C: does not say how much is free")?;
        let room = room_plan(own.size, free, Some(storage.size_min(&own.path)?), Some(mb << 20))?;
        let shrunk = storage.call(&own.path, "MSFT_Partition", "Resize", &[("Size", VARIANT::from((own.size - room.take).to_string().as_str()))], &["ReturnValue"])?;
        match text(&shrunk, "ReturnValue") {
            "0" => Ok(format!("{} GB", room.take / GIB)),
            code => Err(format!("Windows refused to shrink C: (storage error {code}); nothing was changed")),
        }
    }

    pub fn fast_startup_off() -> Result<String, String> {
        let off = 0u32;
        // SAFETY: one DWORD written by name. (REG_DWORD = 4.)
        unsafe { RegSetKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(POWER), w!("HiberbootEnabled"), 4, Some((&off as *const u32).cast()), 4) }.ok().map_err(told)?;
        Ok("off".into())
    }

    fn control(handle: &std::fs::File, code: u32) -> Result<(), String> {
        let mut got = 0u32;
        // SAFETY: a control code without buffers on an open volume.
        unsafe { windows::Win32::System::IO::DeviceIoControl(HANDLE(handle.as_raw_handle() as isize), code, None, 0, None, 0, Some(&mut got), None) }.map_err(told)
    }

    /// The stick, its volumes taken out of use and its old partitions gone, opened whole:
    /// unbuffered and written through.
    pub fn open_for_writing(stick: &Disk) -> Result<std::fs::File, String> {
        const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
        const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
        for letter in &stick.mounts {
            let volume = std::fs::OpenOptions::new().read(true).write(true).share_mode(3).open(format!(r"\\.\{letter}:")).map_err(|e| format!("drive {letter}: does not open: {e}"))?;
            // (Locked when nothing holds a file on it; taken out of use either way: Sir said
            // it may be wiped.)
            let _ = control(&volume, FSCTL_LOCK_VOLUME);
            control(&volume, FSCTL_DISMOUNT_VOLUME).map_err(|e| format!("drive {letter}: is in use: {e}"))?;
        }
        // Its old partitions go (no volume of Windows' lies where the image is written); a
        // stick without a partition table has none.
        let storage = Storage::open()?;
        let own = storage.all("MSFT_Disk", DISK_FIELDS)?.into_iter().find(|d| text(d, "Number") == stick.id).ok_or("the stick is gone")?;
        if text(&own, "PartitionStyle") != "0" {
            let cleared = storage.call(text(&own, "__PATH"), "MSFT_Disk", "Clear", &[("RemoveData", VARIANT::from(true)), ("RemoveOEM", VARIANT::from(true)), ("ZeroOutEntireDisk", VARIANT::from(false))], &["ReturnValue"])?;
            if text(&cleared, "ReturnValue") != "0" {
                return Err(format!("Windows did not clear the stick (storage error {})", text(&cleared, "ReturnValue")));
            }
        }
        std::fs::OpenOptions::new().read(true).write(true).share_mode(3).custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH).open(format!(r"\\.\PhysicalDrive{}", stick.id)).map_err(|e| format!("the stick does not open for writing: {e}"))
    }

    pub struct Running(HANDLE);

    impl Running {
        /// Its exit code once it has ended (waits up to `wait`).
        pub fn finished(&mut self, wait: Duration) -> Option<i32> {
            let mut code = 0u32;
            // SAFETY: the handle ShellExecuteEx gave, closed here once.
            unsafe {
                if windows::Win32::System::Threading::WaitForSingleObject(self.0, wait.as_millis() as u32) != WAIT_OBJECT_0 {
                    return None;
                }
                let _ = windows::Win32::System::Threading::GetExitCodeProcess(self.0, &mut code);
                let _ = CloseHandle(self.0);
            }
            Some(code as i32)
        }
    }

    /// This program again, with rights: Windows asks Sir by its own question.
    pub fn start(args: &[String]) -> Result<Running, String> {
        use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let params = args.iter().map(|a| super::quoted(a)).collect::<Result<Vec<_>, _>>()?.join(" ");
        let (verb, file, params) = (HSTRING::from("runas"), HSTRING::from(exe.as_os_str()), HSTRING::from(params));
        let dir = HSTRING::from(exe.parent().map(|d| d.as_os_str()).unwrap_or_default());
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            lpDirectory: PCWSTR(dir.as_ptr()),
            nShow: windows::Win32::UI::WindowsAndMessaging::SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: every string outlives the call; `info` is sized. (1223: Sir said no.)
        unsafe { ShellExecuteExW(&mut info) }.map_err(|e| if e.code() == HRESULT::from_win32(1223) { DECLINED.to_string() } else { told(e) })?;
        if info.hProcess.is_invalid() {
            return Err("Windows started nothing".into());
        }
        Ok(Running(info.hProcess))
    }
}

// ---------------------------------------------------------------------------------- Linux ----

#[cfg(not(windows))]
mod os {
    use super::{disks_from_lsblk, Disk, Facts};
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::Duration;

    pub fn disks() -> Result<Vec<Disk>, String> {
        let listed = std::process::Command::new("lsblk").args(["-J", "-b", "-o", "NAME,PATH,SIZE,TRAN,TYPE,MODEL,SERIAL,MOUNTPOINTS"]).output().map_err(|e| format!("lsblk does not run: {e}"))?;
        Ok(disks_from_lsblk(&String::from_utf8_lossy(&listed.stdout)))
    }

    pub fn facts() -> Facts {
        let uefi = std::path::Path::new("/sys/firmware/efi").exists();
        // (The variable's four attribute bytes, then its one byte.)
        let secure = std::fs::read("/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c").ok().and_then(|v| v.get(4).map(|on| *on == 1));
        Facts { uefi: Some(uefi), secure_boot: secure.or((!uefi).then_some(false)), ..Facts::default() }
    }

    pub fn make_room(_mb: u64) -> Result<String, String> {
        Err("room beside Windows is made from Windows".into())
    }

    pub fn fast_startup_off() -> Result<String, String> {
        Err("Fast Startup is Windows' own".into())
    }

    /// The stick, unmounted, opened whole for no one else: direct and synchronous.
    pub fn open_for_writing(stick: &Disk) -> Result<std::fs::File, String> {
        for at in &stick.mounts {
            if !std::process::Command::new("umount").arg(at).status().is_ok_and(|s| s.success()) {
                return Err(format!("{at} is in use and does not unmount"));
            }
        }
        std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_DIRECT | libc::O_SYNC | libc::O_EXCL).open(&stick.id).map_err(|e| format!("the stick does not open for writing: {e}"))
    }

    pub struct Running(std::process::Child);

    impl Running {
        pub fn finished(&mut self, wait: Duration) -> Option<i32> {
            match self.0.try_wait() {
                Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
                Ok(None) => {
                    std::thread::sleep(wait);
                    None
                }
                Err(_) => Some(-1),
            }
        }
    }

    /// This program again, as root: polkit asks Sir.
    // shortcut: always pkexec; J.A.I.OS's own root core (org.jaios.System1) is not asked yet, wire
    // its run-as-root in here once it is merged.
    pub fn start(args: &[String]) -> Result<Running, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::process::Command::new("pkexec").arg(exe).args(args).spawn().map(Running).map_err(|e| format!("pkexec does not start (polkit): {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn row(pairs: &[(&str, &str)]) -> Row {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A PC with Windows on an NVMe disk, a data disk, a USB stick and a USB hard disk.
    fn sample() -> (Vec<Row>, Vec<Row>) {
        let disks = vec![
            row(&[("Number", "0"), ("FriendlyName", "Samsung SSD 990"), ("SerialNumber", "S1"), ("Size", "1000204886016"), ("BusType", "17"), ("IsBoot", "True"), ("IsSystem", "True"), ("PartitionStyle", "2")]),
            row(&[("Number", "1"), ("FriendlyName", "WD Blue"), ("SerialNumber", "W1"), ("Size", "2000398934016"), ("BusType", "11"), ("IsBoot", "False"), ("IsSystem", "False"), ("PartitionStyle", "2")]),
            row(&[("Number", "2"), ("FriendlyName", "SanDisk Ultra"), ("SerialNumber", " 4C53 0001 "), ("Size", "30752636928"), ("BusType", "7"), ("IsBoot", "False"), ("IsSystem", "False"), ("PartitionStyle", "1")]),
            row(&[("Number", "3"), ("FriendlyName", "Seagate Expansion"), ("SerialNumber", "E1"), ("Size", "2000398934016"), ("BusType", "7"), ("IsBoot", "False"), ("IsSystem", "False"), ("PartitionStyle", "2")]),
        ];
        let parts = vec![
            row(&[("DiskNumber", "0"), ("DriveLetter", "0"), ("Size", "104857600"), ("Offset", "1048576"), ("IsSystem", "True"), ("__PATH", "efi")]),
            row(&[("DiskNumber", "0"), ("DriveLetter", "67"), ("Size", "900000000000"), ("Offset", "122683392"), ("IsBoot", "True"), ("__PATH", "c")]),
            row(&[("DiskNumber", "0"), ("DriveLetter", "0"), ("Size", "800000000"), ("Offset", "999400000000"), ("__PATH", "recovery")]),
            row(&[("DiskNumber", "1"), ("DriveLetter", "68"), ("Size", "2000000000000"), ("Offset", "1048576")]),
            row(&[("DiskNumber", "2"), ("DriveLetter", "E"), ("Size", "30751588352"), ("Offset", "1048576")]),
            row(&[("DiskNumber", "3"), ("DriveLetter", "70"), ("Size", "2000000000000"), ("Offset", "1048576")]),
        ];
        (disks, parts)
    }

    #[test]
    fn only_a_usb_stick_that_holds_nothing_of_the_system_is_written_to() {
        let (disks, parts) = sample();
        let all = disks_from_wmi(&disks, &parts, 'c');
        let refused: Vec<Option<&str>> = all.iter().map(stick_refusal).collect();
        assert_eq!(refused[0], Some("it holds this system"));
        assert_eq!(refused[1], Some("it is no USB drive"));
        assert_eq!(refused[2], None);
        assert!(refused[3].is_some_and(|why| why.starts_with("it is larger than a stick")));
        assert_eq!((all[2].mounts.clone(), all[2].serial.as_str(), plain_serial(&all[2].serial)), (vec!["E".to_string()], "4C53 0001", "4C530001".to_string()));
        // A USB drive Windows runs from, one that holds C: without the flags, and an empty
        // card reader are refused too.
        let mut to_go = disks[2].clone();
        to_go.insert("IsBoot".into(), "-1".into());
        assert_eq!(stick_refusal(&disks_from_wmi(&[to_go], &[], 'C')[0]), Some("it holds this system"));
        assert_eq!(stick_refusal(&disks_from_wmi(&disks[2..3], &[row(&[("DiskNumber", "2"), ("DriveLetter", "67")])], 'C')[0]), Some("it holds this system"));
        let mut empty = disks[2].clone();
        empty.insert("Size".into(), "0".into());
        assert_eq!(stick_refusal(&disks_from_wmi(&[empty], &[], 'C')[0]), Some("there is no medium in it"));
    }

    #[test]
    fn linux_tells_a_stick_from_the_disk_it_runs_from() {
        let listed = r#"{"blockdevices":[
            {"name":"nvme0n1","path":"/dev/nvme0n1","size":1000204886016,"tran":"nvme","type":"disk","model":"Samsung SSD 990","serial":"S1","mountpoints":[null],
             "children":[{"name":"nvme0n1p1","path":"/dev/nvme0n1p1","size":1,"type":"part","mountpoints":["/boot"]},
                         {"name":"nvme0n1p2","path":"/dev/nvme0n1p2","size":1,"type":"part","mountpoints":[null],"children":[{"name":"root","type":"crypt","mountpoints":["/"]}]}]},
            {"name":"sda","path":"/dev/sda","size":"30752636928","tran":"usb","type":"disk","model":"Ultra ","serial":"4C530001","mountpoints":[null],
             "children":[{"name":"sda1","path":"/dev/sda1","size":1,"type":"part","mountpoints":["/run/media/max/STICK"]}]},
            {"name":"sdb","path":"/dev/sdb","size":30752636928,"tran":"usb","type":"disk","model":"Live","serial":"L1","mountpoints":[null],
             "children":[{"name":"sdb1","path":"/dev/sdb1","size":1,"type":"part","mountpoints":["/run/archiso/bootmnt"]}]},
            {"name":"loop0","path":"/dev/loop0","size":1,"tran":null,"type":"loop","mountpoints":["/snap"]}]}"#;
        let all = disks_from_lsblk(listed);
        assert_eq!(all.len(), 3);
        assert_eq!(stick_refusal(&all[0]), Some("it holds this system"));
        assert_eq!((stick_refusal(&all[1]), all[1].id.as_str(), all[1].name.as_str(), all[1].size, all[1].mounts.clone()), (None, "/dev/sda", "Ultra", 30752636928, vec!["/run/media/max/STICK".to_string()]));
        // (The stick a live system was started from.)
        assert_eq!(stick_refusal(&all[2]), Some("it holds this system"));
        assert!(disks_from_lsblk("no json").is_empty());
    }

    #[test]
    fn the_room_after_c_is_read_from_the_partition_table() {
        let (disks, mut parts) = sample();
        let own = windows_layout(&disks, &parts, 'C').expect("C: is there");
        // C: ends at 900 122 683 392; the recovery partition starts at 999.4 GB.
        assert_eq!(own, Layout { gpt: true, size: 900_000_000_000, after: 999_400_000_000 - 900_122_683_392, path: "c".into() });
        // Nothing behind it: up to the disk's end, less the megabyte of the second table.
        parts.remove(2);
        assert_eq!(windows_layout(&disks, &parts, 'C').map(|l| l.after), Some(1_000_204_886_016 - (1 << 20) - 900_122_683_392));
        assert_eq!(windows_layout(&disks, &parts, 'Q'), None);
    }

    #[test]
    fn the_room_c_gives_keeps_windows_its_margin() {
        // 900 GB with 400 free; Windows would shrink it down to 520 GB (380 off).
        let (size, free, min) = (900 * GIB, 400 * GIB, Some(520 * GIB));
        // His suggestion: 100 GB; the most: 400 - 30 kept free = 370.
        assert_eq!(room_plan(size, free, min, None), Ok(Room { most: 370 * GIB, take: 100 * GIB }));
        // Unmovable files hold the end of C:: Windows' own limit is the lower one.
        assert_eq!(room_plan(size, free, Some(700 * GIB), None).map(|r| r.most), Ok(200 * GIB));
        // Without rights the limit is not known: the free space less the margin.
        assert_eq!(room_plan(size, free, None, Some(150 * GIB)), Ok(Room { most: 370 * GIB, take: 150 * GIB }));
        // What he names is cut to whole gigabytes; the most there is when it is under 100.
        assert_eq!(room_plan(size, 95 * GIB, min, None), Ok(Room { most: 65 * GIB, take: 65 * GIB }));
        assert_eq!(room_plan(size, free, min, Some(50 * GIB + 5)).map(|r| r.take), Ok(50 * GIB));
        assert_eq!(room_plan(size, free, min, Some(370 * GIB)).map(|r| r.take), Ok(370 * GIB));
        // Too much, too little, and a C: that has none to give.
        assert_eq!(room_plan(size, free, min, Some(371 * GIB)), Err("C: can give up 370 GB at most, Sir.".into()));
        assert_eq!(room_plan(size, free, min, Some(20 * GIB)), Err("J.A.I.OS needs at least 40 GB, Sir.".into()));
        assert!(room_plan(size, 69 * GIB, min, None).is_err_and(|e| e.starts_with("C: can give up only 39 GB")));
        assert!(room_plan(size, free, Some(880 * GIB), Some(40 * GIB)).is_err_and(|e| e.starts_with("C: can give up only 20 GB")));
        assert!(room_plan(0, 0, None, None).is_err());
    }

    fn image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn sha(bytes: &[u8]) -> String {
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        hash.update(bytes);
        hex(hash)
    }

    #[test]
    fn an_image_is_written_in_aligned_blocks_and_read_back() {
        // Two whole blocks and a ragged end, onto a file that stands for the stick.
        let bytes = image(2 * CHUNK + 5000);
        let path = std::env::temp_dir().join(format!("jai-os-test-{}.img", uuid::Uuid::new_v4()));
        let mut stick = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).expect("a temp file");
        let mut steps = Vec::new();
        let got = write_image(&mut Cursor::new(&bytes), &mut stick, bytes.len() as u64, Some(&sha(&bytes).to_uppercase()), &mut |phase, percent| steps.push((phase.to_string(), percent)));
        drop(stick);
        let written = std::fs::read(&path).expect("the stick's bytes");
        let _ = std::fs::remove_file(&path);
        assert_eq!(got, Ok(sha(&bytes)));
        // Whole sectors: the image, then zeros up to the next 4096.
        assert_eq!(written.len(), padded(bytes.len()));
        assert!(written[..bytes.len()] == bytes[..] && written[bytes.len()..].iter().all(|b| *b == 0));
        assert_eq!(steps.first(), Some(&("write".to_string(), 99)));
        assert_eq!(steps.last(), Some(&("verify".to_string(), 100)));
        assert!(steps.contains(&("write".to_string(), 100)));
        // An image shorter than one block.
        let small = image(700);
        let mut stick = Cursor::new(Vec::new());
        assert_eq!(write_image(&mut Cursor::new(&small), &mut stick, 700, None, &mut |_, _| {}), Ok(sha(&small)));
        assert_eq!(stick.get_ref().len(), ALIGN);
        // The buffers start on a sector boundary, as an unbuffered write needs.
        assert_eq!(Aligned::new().bytes().as_ptr() as usize % ALIGN, 0);
    }

    #[test]
    fn a_wrong_image_leaves_the_stick_without_a_start_block() {
        let bytes = image(CHUNK + 4096);
        let mut stick = Cursor::new(vec![0xEEu8; CHUNK + 4096]);
        let refused = write_image(&mut Cursor::new(&bytes), &mut stick, bytes.len() as u64, Some(&"0".repeat(64)), &mut |_, _| {});
        assert!(refused.is_err_and(|e| e.contains("SHA-256 differs")));
        assert!(stick.get_ref()[..CHUNK].iter().all(|b| *b == 0xEE), "the first block was written");
        // An image that ends early is an error, not a short stick.
        assert!(write_image(&mut Cursor::new(&bytes[..100]), &mut Cursor::new(Vec::new()), bytes.len() as u64, None, &mut |_, _| {}).is_err());
    }

    /// A stick that gives back something else than was written.
    struct Worn(Cursor<Vec<u8>>);

    impl Read for Worn {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.read(buf)?;
            buf[..n].iter_mut().for_each(|b| *b ^= 1);
            Ok(n)
        }
    }

    impl Write for Worn {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }

    impl Seek for Worn {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(to)
        }
    }

    #[test]
    fn a_stick_that_reads_back_differently_is_told() {
        let bytes = image(9000);
        let told = write_image(&mut Cursor::new(&bytes), &mut Worn(Cursor::new(Vec::new())), 9000, None, &mut |_, _| {});
        assert!(told.is_err_and(|e| e.contains("not what was written")));
    }

    #[test]
    fn a_step_with_rights_keeps_its_arguments_and_tells_its_outcome() {
        let result = std::env::temp_dir().join(format!("jai-os-test-{}.json", uuid::Uuid::new_v4()));
        let stick = Job::WriteStick { disk: "2".into(), size: 30752636928, serial: "4C530001".into(), iso: PathBuf::from(r"D:\My Files\jaios\jaios-1.0-x86_64.iso"), sha: "ab".repeat(32) };
        for job in [Job::MakeRoom { mb: 102400 }, stick, Job::FastStartupOff, Job::WriteStick { disk: "/dev/sda".into(), size: 1, serial: String::new(), iso: PathBuf::from("/home/max/jaios.iso"), sha: "0".repeat(64) }] {
            let args = job.to_args(&result);
            assert_eq!(Job::parse(&args), Ok((job, result.clone())), "{args:?}");
            // Each argument passes Windows' command line: quoted where it holds a space.
            assert!(args.iter().all(|a| quoted(a).is_ok()), "{args:?}");
        }
        assert_eq!(quoted(r"D:\My Files\jaios.iso"), Ok(r#""D:\My Files\jaios.iso""#.to_string()));
        assert_eq!((quoted(""), quoted("--disk")), (Ok(r#""""#.to_string()), Ok("--disk".to_string())));
        assert!(quoted(r#"a" --disk 0"#).is_err() && quoted(r"D:\folder\").is_err());
        // A serial with spaces is passed, and compared, without them.
        assert!(Job::WriteStick { disk: "2".into(), size: 1, serial: " 4C53 0001 ".into(), iso: PathBuf::from("i"), sha: "0".repeat(64) }.to_args(&result).contains(&"4C530001".to_string()));
        // What is no step of his, or lacks a part, is refused.
        for bad in [vec!["--make-room", "0", "--result", "r"], vec!["--make-room", "100"], vec!["--write-stick", "--disk", "2", "--result", "r"], vec!["--write-stick", "--disk", "2", "--size", "1", "--serial", "", "--iso", "i", "--sha", "xyz", "--result", "r"], vec!["--format", "--result", "r"], vec![]] {
            assert!(Job::parse(&bad.iter().map(|a| a.to_string()).collect::<Vec<_>>()).is_err(), "{bad:?}");
        }
        // The result file: the step's words, as done or as failed; none while it is not there.
        assert_eq!(read_result(&result), None);
        write_result(&result, &Ok("100 GB".into()));
        assert_eq!(read_result(&result), Some(Ok("100 GB".into())));
        write_result(&result, &Err("the stick is gone".into()));
        assert_eq!(read_result(&result), Some(Err("the stick is gone".into())));
        let _ = std::fs::remove_file(&result);
        assert_eq!(progress_file(&result).extension().and_then(|e| e.to_str()), Some("progress"));
        // The helper refuses a bad command line with code 2 and writes nothing.
        assert_eq!(format!("{:?}", helper(&["--format".to_string()])), format!("{:?}", ExitCode::from(2)));
        // No test asks for rights.
        assert!(hands_tied());
        assert_eq!(run_elevated(&Job::FastStartupOff, &mut |_, _| {}), Err("a test session changes no disk".into()));
    }

    /// What this PC's storage API says, read only (`JAI_REAL_DISKS=1`): nothing is changed.
    #[test]
    #[ignore]
    fn this_pcs_disks_are_read() {
        if std::env::var_os("JAI_REAL_DISKS").is_none() {
            return;
        }
        let all = disks().expect("the drives");
        for disk in &all {
            println!("{disk:?} -> {:?}", stick_refusal(disk));
        }
        println!("{:?}", facts());
        assert!(all.iter().any(|d| d.system), "one drive holds this system");
        assert!(all.iter().filter(|d| d.system).all(|d| stick_refusal(d).is_some()));
    }
}
