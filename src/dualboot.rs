//! J.A.I.OS beside Windows, asked for in Sir's words (he decided it on 2026-11-25):
//! - "make room for J.A.I.OS": C: gives up room for it. Linux cannot shrink a BitLocker
//!   partition, so J.A.I. does it on Windows, with Windows' own storage API: he says how much C:
//!   can give, suggests a size, asks, and shrinks only on Sir's yes (and Windows' own question).
//! - "make a J.A.I.OS stick": the newest install image from this project's GitHub releases
//!   (`jaios-<version>-x86_64.iso` with its `.sha256`), checked, written to the USB stick he
//!   names and read back. The stick is wiped, so he names it by model and size and asks first.
//! - "is my PC ready for J.A.I.OS?": UEFI, Secure Boot, GPT, BitLocker, Fast Startup, the room
//!   after C: and the newest image, as a block and in one sentence.
//! - "switch off Fast Startup": Windows' Fast Startup, which a dual boot should not have.
//!
//! On Linux the stick is written the same way (polkit asks); room beside Windows is made from
//! Windows only. What touches a disk is `dualboot_disk.rs`.

use crate::dualboot_disk::{self as disk, Disk, Facts, Job, GIB, NEEDS};
use crate::kit::say;
use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub const TOOL: &str = "jaios_setup";
/// How long his question about a disk waits for Sir's yes.
const ANSWER_FOR: Duration = Duration::from_secs(60);
const RELEASES: &str = "https://api.github.com/repos/JayKaey/JAI/releases?per_page=30";
const NO_RELEASE: &str = "There is no J.A.I.OS release on GitHub yet, Sir, so there is no install image to fetch.";

/// The orders his yes carries out (what the offer on the HUD holds).
const STICK_ORDER: &str = "erase the stick and write jai os to it";
const FAST_ORDER: &str = "switch off fast startup";
const ROOM_WORDS: &str = "make room for jai os";

fn room_order(gb: u64) -> String {
    format!("shrink windows by {gb} gigabytes for jai os")
}

/// "J.A.I.OS" as it is typed and as Whisper writes it: J.A.I. OS, JAI OS, Jay OS, Jaios, Jay-OS,
/// J A I O S, Jay's OS.
const OS: &str = r"(?:(?:j\.?\s?a\.?\s?i\.?|jays|jay|jae|jy|j)[\s.\-]*o\.?\s?s|jaios|jayos|jios)\b\.?";
const GB: &str = r"(?:gb|gigs?|gigabytes?|gigabyte)";

fn re(pattern: &str) -> Regex {
    Regex::new(&pattern.replace("OS", OS).replace("GB", GB)).expect("a valid J.A.I.OS phrase")
}

static ROOM: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:i\s+(?:need|want)\s+you\s+to\s+)?(?:make|free(?:\s+up)?|clear|create|prepare|get|reserve|give)\s+(?:me\s+|us\s+)?(?:some\s+|a\s+bit\s+of\s+|enough\s+)?(?:(?P<gb>\d+)\s*GB\s+(?:of\s+)?)?(?:room|space|disk\s+space)\s+for\s+(?:installing\s+)?OS(?:\s+(?:on|from)\s+(?:c|windows|my\s+(?:disk|drive|ssd|pc)|the\s+(?:disk|drive|ssd)))?$",
        r"|^(?:shrink|resize)\s+(?:windows|c|drive\s+c|my\s+windows\s+partition|the\s+windows\s+partition|the\s+c\s+drive|my\s+c\s+drive)\s+(?:for|to\s+make\s+room\s+for)\s+OS$",
        r"|^(?:mach|mache|schaff|schaffe|gib)\s+(?:mir\s+)?(?:(?P<gb2>\d+)\s*GB\s+)?(?:platz|speicherplatz|raum)\s+(?:für|fuer|fur)\s+OS(?:\s+frei)?$",
        r"|^(?:verkleinere?|verkleiner)\s+(?:windows|c|laufwerk\s+c|die\s+windows\s*-?\s*partition)\s+(?:für|fuer|fur)\s+OS$",
        r"|^(?:windows|c|laufwerk\s+c)\s+(?:für|fuer|fur)\s+OS\s+verkleinern$",
        r"|^(?:räume?|raeume?)\s+(?:mir\s+)?platz\s+(?:für|fuer|fur)\s+OS\s+(?:frei|ein)$",
    ))
});
static ROOM_YES: LazyLock<Regex> = LazyLock::new(|| re(r"^shrink windows by (?P<gb>\d+) gigabytes for jai os$"));
/// His answer to "how many gigabytes?": only while that question stands.
static SIZE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:make\s+it\s+|take\s+|give\s+it\s+|lets\s+say\s+|nimm\s+|mach\s+)?(?P<gb>\d+)\s*GB(?:\s+then)?$"));
static STICK: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:make|create|build|write|burn|prepare|flash|give)\s+(?:me\s+)?(?:an?\s+|the\s+|my\s+)?(?:bootable\s+)?OS\s*-?\s*(?:usb\s*-?\s*|install(?:er|ation)?\s+|boot\s+|bootable\s+)*(?:stick|drive|medium|media|usb)(?P<n>\s+(?:on|onto|with|from|out\s+of|using)\s+.+)?$",
        r"|^i\s+(?:need|want)\s+an?\s+(?:bootable\s+)?OS\s*-?\s*(?:usb\s*-?\s*|install(?:er|ation)?\s+|boot\s+)*(?:stick|drive)$",
        r"|^(?:put|write|burn|flash|install|copy|load)\s+OS\s+(?:on|onto|on\s+to|to)\s+(?P<n2>.+)$",
        r"|^(?:make|turn)\s+(?P<n5>.+?)\s+(?:into\s+)?an?\s+OS\s*-?\s*(?:usb\s*-?\s*|install(?:er|ation)?\s+|boot\s+)*(?:stick|drive)$",
        r"|^(?:erstelle?|mache?|baue?|schreibe?|brenne?)\s+(?:mir\s+)?(?:einen\s+|den\s+|nen\s+|n\s+|ein\s+)?(?:bootfähigen\s+)?OS\s*-?\s*(?:usb\s*-?\s*|installations\s*-?\s*|boot\s*-?\s*)*stick(?P<n3>\s+(?:auf|mit|aus)\s+.+)?$",
        r"|^(?:packe?|schreibe?|spiele?|tu|mache?|kopiere?|ziehe?|installiere?)\s+OS\s+auf\s+(?P<n4>.+?)(?:\s+drauf)?$",
    ))
});
static STICK_YES: LazyLock<Regex> = LazyLock::new(|| re(&format!("^{STICK_ORDER}$")));
/// What a target must hold to be a stick ("put J.A.I.OS on my laptop" is no order of these).
static STICKY: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:usb|stick|flash\s+drive|thumb\s+drive|pen\s*drive|(?:drive|laufwerk)\s+[a-z])\b"));
static READY: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:is|are)\s+(?:my|this|the)\s+(?:pc|computer|machine|laptop|system|rig)\s+(?:ready|prepared|good|fit|set)\s+for\s+OS$",
        r"|^can\s+(?:my|this|the)\s+(?:pc|computer|machine|laptop)\s+(?:run|take|boot|handle|install)\s+OS$",
        r"|^(?:can|could)\s+i\s+install\s+OS(?:\s+(?:here|on\s+(?:this|my)\s+(?:pc|computer|machine|laptop)))?$",
        r"|^does\s+(?:my|this)\s+(?:pc|computer|machine|laptop)\s+meet\s+(?:the\s+)?requirements\s+(?:for|of)\s+OS$",
        r"|^(?:prüfe?|pruefe?)\s+ob\s+(?:mein|der|dieser)\s+(?:pc|computer|rechner|laptop)\s+(?:für|fuer|fur)\s+OS\s+bereit\s+ist$",
        r"|^(?:am\s+i|are\s+we|is\s+everything)\s+ready\s+for\s+OS$",
        r"|^check\s+(?:if|whether|that)\s+(?:my|this|the)\s+(?:pc|computer|machine|laptop)\s+is\s+ready\s+for\s+OS$",
        r"|^(?:check|show)\s+(?:the\s+|my\s+)?(?:readiness|requirements)\s+for\s+OS$",
        r"|^what\s+does\s+OS\s+need$",
        r"|^ist\s+(?:mein|der|dieser)\s+(?:pc|computer|rechner|laptop)\s+(?:bereit|fertig)\s+(?:für|fuer|fur)\s+OS$",
        r"|^kann\s+(?:mein|der|dieser)\s+(?:pc|computer|rechner|laptop)\s+OS(?:\s+(?:starten|installieren|ausführen))?$",
        r"|^kann\s+ich\s+OS\s+(?:hier\s+)?installieren$",
        r"|^(?:bin\s+ich|sind\s+wir)\s+bereit\s+(?:für|fuer|fur)\s+OS$",
    ))
});
static FAST: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:switch|turn)\s+off\s+(?:windows\s+)?fast\s*-?\s*start\s*-?\s*up$|^(?:switch|turn)\s+(?:windows\s+)?fast\s*-?\s*start\s*-?\s*up\s+off$|^disable\s+(?:windows\s+)?fast\s*-?\s*start\s*-?\s*up$",
        r"|^(?:schalte?|mache?)\s+(?:den\s+|windows\s+)?schnellstart\s+aus$|^deaktiviere?\s+(?:den\s+|windows\s+)?schnellstart$",
    ))
});

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The room he offered to make (GB), until Sir answers.
static ROOM_ASKED: Mutex<Option<(u64, Instant)>> = Mutex::new(None);
/// The stick he asked about and the image for it, until Sir answers.
static STICK_ASKED: Mutex<Option<(Disk, Iso, Instant)>> = Mutex::new(None);
/// One step for J.A.I.OS at a time.
static BUSY: AtomicBool = AtomicBool::new(false);

/// The stick's own words in what he named ("on the SanDisk stick": sandisk; "drive E": e).
fn stick_name(words: &str) -> Option<String> {
    const NO_NAME: &[&str] = &[
        "on", "onto", "to", "with", "from", "out", "of", "using", "the", "my", "a", "an", "this", "that", "usb", "stick", "drive", "flash", "thumb", "pen", "pendrive", "bootable", "called", "named", "auf", "mit", "aus", "den", "dem", "der",
        "meinen", "meinem", "einen", "einem", "diesen", "diesem", "laufwerk", "namens",
    ];
    let name: Vec<&str> = words.split(|c: char| c.is_whitespace() || c == '-').map(|w| w.trim_matches('.')).filter(|w| !w.is_empty() && !NO_NAME.contains(w)).collect();
    (!name.is_empty()).then(|| name.join(" "))
}

/// An order about J.A.I.OS beside Windows in Sir's words (the fast path's normalised phrase).
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    let number = |c: &regex::Captures, names: &[&str]| names.iter().find_map(|n| c.name(n)).and_then(|m| m.as_str().parse::<u64>().ok());
    if let Some(c) = ROOM_YES.captures(s) {
        return Some(json!({"action": "room", "gb": number(&c, &["gb"])?, "confirmed": true}));
    }
    if STICK_YES.is_match(s) {
        return Some(json!({"action": "stick", "confirmed": true}));
    }
    if let Some(c) = ROOM.captures(s) {
        return Some(match number(&c, &["gb", "gb2"]) {
            Some(gb) => json!({"action": "room", "gb": gb}),
            None => json!({"action": "room"}),
        });
    }
    if let Some(c) = STICK.captures(s) {
        let Some((group, words)) = ["n", "n2", "n3", "n4", "n5"].iter().find_map(|n| c.name(n).map(|m| (*n, m.as_str()))) else {
            return Some(json!({"action": "stick"}));
        };
        if matches!(group, "n2" | "n4" | "n5") && !STICKY.is_match(words) {
            return None;
        }
        return Some(match stick_name(words) {
            Some(name) => json!({"action": "stick", "name": name}),
            None => json!({"action": "stick"}),
        });
    }
    if READY.is_match(s) {
        return Some(json!({"action": "ready"}));
    }
    if FAST.is_match(s) {
        return Some(json!({"action": "fast_startup_off"}));
    }
    let c = SIZE.captures(s)?;
    lock(&ROOM_ASKED).filter(|(_, at)| at.elapsed() < ANSWER_FOR)?;
    Some(json!({"action": "room", "gb": number(&c, &["gb"])?}))
}

/// The report only reads.
pub fn only_looks(args: &Value) -> bool {
    args["action"] == "ready"
}

pub async fn run(args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("") {
        "room" => room(args).await,
        "stick" => stick(args).await,
        "ready" => ready().await,
        "fast_startup_off" => fast_startup_off().await,
        other => Err(format!("J.A.I.OS's setup has no action '{other}'.")),
    }
}

async fn off<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(work).await.map_err(|e| e.to_string())
}

fn one(line: impl AsRef<str>) -> Result<String, String> {
    Ok(say(line.as_ref(), line.as_ref()))
}

/// A step with rights, in the background: what he says when it is done. None: a test session
/// (nothing is started), or another step still runs.
fn start(job: Job, done: impl FnOnce(Result<String, String>) -> String + Send + 'static) -> Result<(), String> {
    if disk::hands_tied() {
        return Err("A test session changes no disk, Sir.".into());
    }
    if BUSY.swap(true, Ordering::AcqRel) {
        return Err("I am still at the last step for J.A.I.OS, Sir.".into());
    }
    tokio::task::spawn_blocking(move || {
        let outcome = disk::run_elevated(&job, &mut |_, _| {});
        BUSY.store(false, Ordering::Release);
        crate::announcements::say(done(outcome));
    });
    Ok(())
}

// ----------------------------------------------------------------------------------- room ----

const ROOM_IS_WINDOWS: &str = "Not from Linux, Sir: Linux cannot shrink a BitLocker partition. Ask me on Windows, and I make the room there.";

async fn room(args: &Value) -> Result<String, String> {
    if !cfg!(windows) {
        return one(ROOM_IS_WINDOWS);
    }
    let facts = off(disk::facts).await?;
    if facts.uefi == Some(false) {
        return one("This PC starts the old BIOS way, Sir, and J.A.I.OS needs UEFI. I leave C: as it is.");
    }
    if facts.gpt == Some(false) {
        return one("Windows' disk is an MBR disk, Sir, and J.A.I.OS needs GPT. I leave C: as it is.");
    }
    let asked = args["gb"].as_u64();
    if let Some(there) = facts.unallocated.filter(|there| *there >= NEEDS && asked.is_none()) {
        return one(format!("There are {} GB free after C: already, Sir: enough. The installer on the stick takes them.", there / GIB));
    }
    let (Some(size), Some(free)) = (facts.c_size, facts.c_free) else { return Err("Windows does not tell me about its disk, Sir.".into()) };
    let plan = disk::room_plan(size, free, facts.size_min, asked.map(|gb| gb * GIB))?;
    let (gb, left) = (plan.take / GIB, (free - plan.take) / GIB);
    let agreed = args["confirmed"] == true && lock(&ROOM_ASKED).take().is_some_and(|(offered, at)| offered == gb && at.elapsed() < ANSWER_FOR);
    if !agreed {
        *lock(&ROOM_ASKED) = Some((gb, Instant::now()));
        crate::hud_lines::offer_for(&room_order(gb), ANSWER_FOR);
        let lock_note = if facts.bitlocker == Some(true) { " BitLocker stays on." } else { "" };
        return one(match asked {
            Some(_) => format!("I take {gb} GB off C: for J.A.I.OS, which leaves Windows {left} GB free.{lock_note} Shall I, Sir?"),
            None => format!("C: has {} GB free and can give up to {}. I would take {gb} for J.A.I.OS, which leaves Windows {left} GB free.{lock_note} Shall I, Sir, or how many gigabytes?", free / GIB, plan.most / GIB),
        });
    }
    let fast = facts.fast_startup == Some(true);
    start(Job::MakeRoom { mb: plan.take >> 20 }, move |outcome| match outcome {
        Ok(_) if fast => {
            crate::hud_lines::offer_for(FAST_ORDER, ANSWER_FOR);
            format!("Done, Sir: {gb} GB beside Windows are free for J.A.I.OS. Windows' Fast Startup is still on, and for a dual boot it should be off. Shall I switch it off?")
        }
        Ok(_) => format!("Done, Sir: {gb} GB beside Windows are free for J.A.I.OS."),
        Err(e) => format!("C: is as it was, Sir: {e}."),
    })?;
    one(format!("Windows asks for your permission now, Sir. Then I shrink C: by {gb} GB; I'll tell you when it is done."))
}

async fn fast_startup_off() -> Result<String, String> {
    if !cfg!(windows) {
        return one("Fast Startup is Windows' own, Sir: Linux has none to switch off.");
    }
    if off(disk::facts).await?.fast_startup != Some(true) {
        return one("Fast Startup is off already, Sir.");
    }
    start(Job::FastStartupOff, |outcome| match outcome {
        Ok(_) => "Fast Startup is off, Sir: Windows shuts down fully from now on.".to_string(),
        Err(e) => format!("Fast Startup is still on, Sir: {e}."),
    })?;
    one("Windows asks for your permission now, Sir. Then I switch Fast Startup off.")
}

// ---------------------------------------------------------------------------------- image ----

/// An install image among the releases.
#[derive(Clone, Debug, PartialEq)]
pub struct Iso {
    pub version: String,
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha_url: String,
}

/// The newest release that holds `jaios-<version>-x86_64.iso` with its `.sha256` (GitHub lists
/// the newest first; drafts are not for him).
pub fn newest_iso(releases: &Value) -> Option<Iso> {
    static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^jaios-(.+)-x86_64\.iso$").expect("the image's name"));
    releases.as_array()?.iter().filter(|r| r["draft"] != true).find_map(|release| {
        let assets = release["assets"].as_array()?;
        let named = |name: &str| assets.iter().find(|a| a["name"] == name);
        let (iso, name) = assets.iter().find_map(|a| a["name"].as_str().filter(|n| NAME.is_match(n)).map(|n| (a, n)))?;
        Some(Iso {
            version: NAME.captures(name)?[1].to_string(),
            name: name.to_string(),
            url: iso["browser_download_url"].as_str()?.to_string(),
            size: iso["size"].as_u64().filter(|size| *size > 0)?,
            sha_url: named(&format!("{name}.sha256"))?["browser_download_url"].as_str()?.to_string(),
        })
    })
}

/// The SHA-256 a `.sha256` file names ("<hex>  <file>").
fn sha_of(text: &str) -> Option<String> {
    let sha = text.split_whitespace().next()?.to_ascii_lowercase();
    (sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())).then_some(sha)
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().user_agent("JAI-client").connect_timeout(Duration::from_secs(15)).build().map_err(|e| e.to_string())
}

async fn newest() -> Result<Option<Iso>, String> {
    let asked = http()?.get(RELEASES).timeout(Duration::from_secs(30)).send().await.and_then(|a| a.error_for_status()).map_err(|_| "GitHub did not answer, Sir.")?;
    Ok(newest_iso(&asked.json::<Value>().await.map_err(|_| "GitHub's answer made no sense, Sir.")?))
}

// ---------------------------------------------------------------------------------- stick ----

/// A drive as he names it: "SanDisk Ultra, 31 GB, drive E".
fn describe(d: &Disk) -> String {
    let name = if d.name.is_empty() { "a USB drive" } else { &d.name };
    let at = match d.mounts.first() {
        Some(letter) if cfg!(windows) => format!(", drive {letter}"),
        _ if cfg!(windows) => String::new(),
        _ => format!(", {}", d.id),
    };
    format!("{name}, {:.0} GB{at}", d.size as f64 / 1e9)
}

/// The stick he means: the one he named (by its model, or by its drive letter), else the only
/// one there is.
fn pick_stick(all: &[Disk], named: Option<&str>) -> Result<Disk, String> {
    let sticks: Vec<&Disk> = all.iter().filter(|d| disk::stick_refusal(d).is_none()).collect();
    let list = || sticks.iter().map(|d| describe(d)).collect::<Vec<_>>().join("; ");
    if sticks.is_empty() {
        return Err(match all.iter().find(|d| d.usb) {
            Some(other) => format!("The only USB drive I see is {}, Sir, and I leave it alone: {}.", describe(other), disk::stick_refusal(other).unwrap_or_default()),
            None => "I see no USB stick, Sir. Plug one in and ask again.".into(),
        });
    }
    if let Some(name) = named.map(str::to_lowercase) {
        let hits: Vec<&Disk> = sticks
            .iter()
            .copied()
            .filter(|d| if name.len() == 1 { d.mounts.iter().any(|m| m.eq_ignore_ascii_case(&name)) } else { d.name.to_lowercase().contains(&name) || d.id.to_lowercase() == name })
            .collect();
        match hits.as_slice() {
            [one] => return Ok((*one).clone()),
            [] => return Err(format!("No stick here is called {name}, Sir. I see {}.", list())),
            _ => {}
        }
    }
    match sticks.as_slice() {
        [one] => Ok((*one).clone()),
        _ => Err(format!("I see {} sticks, Sir: {}. Which one? Say it with its name, like: make a J.A.I.OS stick on the {}.", sticks.len(), list(), sticks[0].name.split_whitespace().next().unwrap_or("first"))),
    }
}

async fn stick(args: &Value) -> Result<String, String> {
    if args["confirmed"] == true {
        if let Some((stick, iso, _)) = lock(&STICK_ASKED).take().filter(|(.., at)| at.elapsed() < ANSWER_FOR) {
            return write_stick(stick, iso);
        }
    }
    let iso = newest().await?.ok_or(NO_RELEASE)?;
    let all = off(disk::disks).await??;
    let stick = pick_stick(&all, args["name"].as_str())?;
    if iso.size + 4096 > stick.size {
        return Err(format!("{} is too small for J.A.I.OS {}, Sir: the image is {:.1} GB.", describe(&stick), iso.version, iso.size as f64 / 1e9));
    }
    let line = format!("That is {}. Everything on it will be erased, Sir. Shall I put J.A.I.OS {} on it?", describe(&stick), iso.version);
    *lock(&STICK_ASKED) = Some((stick, iso, Instant::now()));
    crate::hud_lines::offer_for(STICK_ORDER, ANSWER_FOR);
    one(line)
}

/// The stick's block on the board.
fn board(stick: &Disk, iso: &Iso, step: &str) {
    crate::kit::show_text("jaios_stick", "J.A.I.OS // STICK", &format!("STICK   {}\nIMAGE   {}\nSTEP    {step}", describe(stick), iso.name), [460.0, 130.0]);
}

fn write_stick(stick: Disk, iso: Iso) -> Result<String, String> {
    if disk::hands_tied() {
        return one("A test session writes no stick, Sir.");
    }
    if BUSY.swap(true, Ordering::AcqRel) {
        return Err("I am still at the last step for J.A.I.OS, Sir.".into());
    }
    let line = format!("Fetching J.A.I.OS {}, {:.1} GB, Sir. Then I write it to {} and tell you as it goes.", iso.version, iso.size as f64 / 1e9, if stick.name.is_empty() { "the stick" } else { &stick.name });
    tokio::spawn(async move {
        let outcome = stick_job(&stick, &iso).await;
        BUSY.store(false, Ordering::Release);
        board(&stick, &iso, if outcome.is_ok() { "done: read back and the same" } else { "not made" });
        crate::announcements::say(match outcome {
            Ok(()) => format!("J.A.I.OS {} is on the stick, Sir, read back and the same as the image. Restart with it plugged in and pick it in the boot menu.", iso.version),
            Err(e) => format!("The stick was not made, Sir: {}.", e.trim_end_matches('.')),
        });
    });
    one(line)
}

/// The image fetched (or the one already here), checked against its SHA-256, written and read
/// back.
async fn stick_job(stick: &Disk, iso: &Iso) -> Result<(), String> {
    let http = http()?;
    let sha = http.get(&iso.sha_url).send().await.and_then(|a| a.error_for_status()).map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
    let sha = sha_of(&sha).ok_or("the release's checksum file holds no SHA-256")?;
    let image = std::env::current_dir().map_err(|e| e.to_string())?.join("jaios").join(&iso.name);
    let checked = |image: PathBuf| off(move || crate::installer::sha256(&image));
    let here = std::fs::metadata(&image).is_ok_and(|m| m.len() == iso.size) && checked(image.clone()).await?.is_ok_and(|have| have == sha);
    if !here {
        board(stick, iso, "fetching the image");
        crate::models::download_to(&http, &iso.url, &image, Some(iso.size), &|got, _| board(stick, iso, &format!("fetching the image: {} of {} MB", got / 1_000_000, iso.size / 1_000_000))).await?;
        board(stick, iso, "checking the image");
        if checked(image.clone()).await?? != sha {
            let _ = std::fs::remove_file(&image);
            return Err("the image that came is not the one the release names (its SHA-256 differs), so I threw it away".into());
        }
    }
    crate::announcements::say(format!("The image is here and its checksum is right, Sir. {} asks for your permission now; then the stick is erased and written.", disk::ASKS));
    board(stick, iso, &format!("waiting for {}' question", disk::ASKS));
    let job = Job::WriteStick { disk: stick.id.clone(), size: stick.size, serial: disk::plain_serial(&stick.serial), iso: image, sha };
    let (stick, iso) = (stick.clone(), iso.clone());
    off(move || {
        let mut last = (String::new(), 0u8);
        disk::run_elevated(&job, &mut |phase, percent| {
            if last == (phase.to_string(), percent) {
                return;
            }
            // (Said at the turns: half written, and when reading back starts.)
            if phase == "write" && last.1 < 50 && percent >= 50 && last.0 != "verify" {
                crate::announcements::say("Half of J.A.I.OS is on the stick, Sir.");
            }
            if phase == "verify" && last.0 != "verify" {
                crate::announcements::say("It is written, Sir. I am reading it back to compare.");
            }
            last = (phase.to_string(), percent);
            board(&stick, &iso, &format!("{}: {percent} %", if phase == "verify" { "reading back" } else { "writing" }));
        })
    })
    .await?
    .map(|_| ())
}

// --------------------------------------------------------------------------------- report ----

/// Whether this PC is ready: the block's rows and the sentence.
fn report(facts: &Facts, iso: &Result<Option<Iso>, String>) -> (String, String) {
    let known = |what: Option<bool>, yes: &str, no: &str| what.map_or("not known", |is| if is { yes } else { no }).to_string();
    let image = match iso {
        Ok(Some(iso)) => format!("J.A.I.OS {}, {:.1} GB", iso.version, iso.size as f64 / 1e9),
        Ok(None) => "none released yet".to_string(),
        Err(_) => "GitHub did not answer".to_string(),
    };
    let mut rows = vec![
        format!("SYSTEM          {}", if facts.windows { "Windows" } else { "Linux" }),
        format!("FIRMWARE        {}", known(facts.uefi, "UEFI", "old BIOS: J.A.I.OS needs UEFI")),
        format!("SECURE BOOT     {}", known(facts.secure_boot, "on, and it stays on", "off")),
    ];
    let room = facts.unallocated.unwrap_or(0);
    if facts.windows {
        rows.push(format!("DISK            {}", known(facts.gpt, "GPT", "MBR: J.A.I.OS needs GPT")));
        rows.push(format!("BITLOCKER       {}", known(facts.bitlocker, "on: keep its recovery key at hand", "off")));
        rows.push(format!("FAST STARTUP    {}", known(facts.fast_startup, "on: should be off for a dual boot", "off")));
        rows.push(format!("ROOM AFTER C:   {} GB{}", room / GIB, if room >= NEEDS { ", enough" } else { " of the 40 it needs" }));
    }
    rows.push(format!("INSTALL IMAGE   {image}"));

    let image_said = match iso {
        Ok(Some(iso)) => format!("The newest install image is J.A.I.OS {}.", iso.version),
        Ok(None) => "There is no J.A.I.OS release on GitHub yet.".to_string(),
        Err(_) => "GitHub did not tell me the newest image.".to_string(),
    };
    if !facts.windows {
        let start = match (facts.uefi, facts.secure_boot) {
            (Some(false), _) => "started the old BIOS way, and J.A.I.OS needs UEFI",
            (_, Some(true)) => "started by UEFI with Secure Boot on",
            _ => "started by UEFI",
        };
        return (rows.join("\n"), format!("This PC runs Linux already, Sir, {start}. {image_said} Room beside Windows is made from Windows."));
    }
    let mut stops = Vec::new();
    if facts.uefi == Some(false) {
        stops.push("it starts the old BIOS way and J.A.I.OS needs UEFI".to_string());
    }
    if facts.gpt == Some(false) {
        stops.push("Windows' disk is MBR and J.A.I.OS needs GPT".to_string());
    }
    let mut to_do = Vec::new();
    if room < NEEDS {
        to_do.push(format!("there are {} GB free after C: of the 40 it needs, so say: make room for J.A.I.OS", room / GIB));
    }
    if facts.fast_startup == Some(true) {
        to_do.push("Fast Startup is on and should be off".to_string());
    }
    let lock_note = if facts.bitlocker == Some(true) { " BitLocker is on, which is fine: keep its recovery key at hand." } else { "" };
    let verdict = if !stops.is_empty() {
        format!("Not as it is, Sir: {}.", stops.join(", and "))
    } else if to_do.is_empty() {
        format!("Yes, Sir: UEFI, a GPT disk, Secure Boot {}, and {} GB free after C:.", known(facts.secure_boot, "on", "off"), room / GIB)
    } else {
        format!("Nearly, Sir: {}.", to_do.join("; and "))
    };
    (rows.join("\n"), format!("{verdict}{lock_note} {image_said}"))
}

async fn ready() -> Result<String, String> {
    let facts = off(disk::facts).await?;
    let iso = newest().await;
    let (rows, line) = report(&facts, &iso);
    crate::kit::show_text("jaios_ready", "J.A.I.OS // READY?", &rows, [460.0, 60.0 + 22.0 * rows.lines().count() as f32]);
    // What is to do next is on offer: the room first, then Fast Startup.
    if facts.windows && facts.uefi != Some(false) && facts.gpt != Some(false) {
        if facts.unallocated.unwrap_or(0) < NEEDS {
            crate::hud_lines::offer(ROOM_WORDS);
        } else if facts.fast_startup == Some(true) {
            crate::hud_lines::offer(FAST_ORDER);
        }
    }
    Ok(say(rows, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(text: &str) -> Option<Value> {
        command(&crate::intents::normalize_phrase(text))
    }

    #[test]
    fn the_orders_are_understood_however_the_name_is_written() {
        let _kit = crate::kit::test_lock();
        *lock(&ROOM_ASKED) = None;
        for name in ["J.A.I.OS", "J.A.I. OS", "JAI OS", "Jay OS", "jaios", "Jay-OS", "J A I O S", "JayOS", "Jay's OS", "J.A.I.O.S."] {
            assert_eq!(heard(&format!("Make room for {name}")), Some(json!({"action": "room"})), "{name}");
            assert_eq!(heard(&format!("make a {name} stick")), Some(json!({"action": "stick"})), "{name}");
            assert_eq!(heard(&format!("Is my PC ready for {name}?")), Some(json!({"action": "ready"})), "{name}");
            assert_eq!(heard(&format!("Mach Platz für {name}")), Some(json!({"action": "room"})), "{name}");
        }
        for (said, gb) in [("make 150 GB of room for J.A.I.OS", 150), ("free up 80 gigabytes of space for Jay OS", 80), ("mach 120 GB Platz für J.A.I.OS", 120), ("schaff mir 60 Gigabyte Platz für Jay OS", 60)] {
            assert_eq!(heard(said), Some(json!({"action": "room", "gb": gb})), "{said}");
        }
        for said in ["could you make some room for J.A.I.OS please", "shrink Windows for J.A.I.OS", "shrink drive C to make room for Jay OS", "free up space for JAI OS", "verkleinere Windows für J.A.I.OS", "schaffe Platz für J.A.I.OS", "räum Platz für Jay OS frei"] {
            assert_eq!(heard(said), Some(json!({"action": "room"})), "{said}");
        }
        for said in [
            "put J.A.I.OS on the USB stick",
            "create a bootable J.A.I.OS USB stick",
            "make me a J.A.I.OS install stick",
            "write Jay OS to my USB drive",
            "burn J.A.I.OS onto a stick",
            "erstell einen J.A.I.OS-Stick",
            "mach mir einen Jay OS USB-Stick",
            "pack J.A.I.OS auf den USB-Stick",
            "schreib J.A.I.OS auf den Stick",
            "turn my USB stick into a J.A.I.OS stick",
        ] {
            assert_eq!(heard(said), Some(json!({"action": "stick"})), "{said}");
        }
        // The stick he names: by its model, or by its letter.
        for (said, name) in [("make a J.A.I.OS stick on the SanDisk", "sandisk"), ("put J.A.I.OS on the Kingston stick", "kingston"), ("write J.A.I.OS to drive E", "e"), ("erstell einen J.A.I.OS-Stick auf dem SanDisk", "sandisk"), ("pack J.A.I.OS auf Laufwerk F", "f")] {
            assert_eq!(heard(said), Some(json!({"action": "stick", "name": name})), "{said}");
        }
        for said in ["can my computer run J.A.I.OS?", "am I ready for Jay OS", "check if this PC is ready for J.A.I.OS", "ist mein PC bereit für J.A.I.OS?", "kann mein Rechner J.A.I.OS?", "can I install J.A.I.OS on this PC"] {
            assert_eq!(heard(said), Some(json!({"action": "ready"})), "{said}");
        }
        for said in ["switch off Fast Startup", "turn Windows fast startup off", "disable fast startup", "schalte den Schnellstart aus"] {
            assert_eq!(heard(said), Some(json!({"action": "fast_startup_off"})), "{said}");
        }
        // What his offers hold comes back as the confirmed order.
        assert_eq!(heard(&room_order(100)), Some(json!({"action": "room", "gb": 100, "confirmed": true})));
        assert_eq!(heard(STICK_ORDER), Some(json!({"action": "stick", "confirmed": true})));
        assert_eq!(heard(FAST_ORDER), Some(json!({"action": "fast_startup_off"})));
        assert_eq!(heard(ROOM_WORDS), Some(json!({"action": "room"})));
        // No orders of these: another target, another system, talk about it.
        for said in ["put J.A.I.OS on my laptop", "install J.A.I.OS on the second SSD", "make room for the new sofa", "make a Windows stick", "what is J.A.I.OS", "is my PC ready for Windows 12", "I like J.A.I.OS", "turn on fast startup", "100 gigabytes"] {
            assert_eq!(heard(said), None, "{said}");
        }
        assert!(only_looks(&json!({"action": "ready"})) && !only_looks(&json!({"action": "room"})) && !only_looks(&json!({"action": "stick"})) && !only_looks(&json!({"action": "fast_startup_off"})));
    }

    #[test]
    fn a_bare_size_answers_only_his_question() {
        let _kit = crate::kit::test_lock();
        *lock(&ROOM_ASKED) = None;
        assert_eq!(heard("150 gigabytes"), None);
        *lock(&ROOM_ASKED) = Some((100, Instant::now()));
        assert_eq!(heard("150 gigabytes"), Some(json!({"action": "room", "gb": 150})));
        assert_eq!(heard("make it 80 GB"), Some(json!({"action": "room", "gb": 80})));
        assert_eq!(heard("nimm 60 Gigabyte"), Some(json!({"action": "room", "gb": 60})));
        *lock(&ROOM_ASKED) = None;
    }

    #[test]
    fn the_orders_take_the_fast_path() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        for (said, args) in [
            ("make room for J.A.I.OS", json!({"action": "room"})),
            ("JAI, make room for Jay OS.", json!({"action": "room"})),
            ("Mach Platz für J.A.I.OS", json!({"action": "room"})),
            ("make a J.A.I.OS stick", json!({"action": "stick"})),
            ("Erstell einen J.A.I.OS-Stick", json!({"action": "stick"})),
            ("put J.A.I.OS on the USB stick", json!({"action": "stick"})),
            ("is my PC ready for J.A.I.OS?", json!({"action": "ready"})),
            ("switch off fast startup", json!({"action": "fast_startup_off"})),
        ] {
            assert_eq!(crate::phrasings::route(said), Some((TOOL.to_string(), args)), "{said}");
        }
        assert!(crate::kit::is_tool(TOOL));
        assert!(crate::kit::only_looks(TOOL, &json!({"action": "ready"})) && !crate::kit::stays_inside(TOOL, &json!({"action": "stick", "confirmed": true})) && !crate::kit::stays_inside(TOOL, &json!({"action": "room", "gb": 100, "confirmed": true})));
    }

    fn stick_disk(id: &str, name: &str, letter: &str) -> Disk {
        Disk { id: id.into(), name: name.into(), serial: "S".into(), size: 30_752_636_928, usb: true, system: false, mounts: vec![letter.into()] }
    }

    #[test]
    fn the_stick_is_the_one_he_names_and_never_a_disk_of_the_system() {
        let system = Disk { id: "0".into(), name: "Samsung SSD 990".into(), serial: "A".into(), size: 1_000_204_886_016, usb: false, system: true, mounts: vec!["C".into()] };
        let external = Disk { id: "3".into(), name: "Seagate Expansion".into(), serial: "E".into(), size: 2_000_398_934_016, usb: true, system: false, mounts: vec!["F".into()] };
        let (sandisk, kingston) = (stick_disk("2", "SanDisk Ultra", "E"), stick_disk("4", "Kingston DataTraveler", "G"));
        // The only stick is the one, named or not.
        let all = [system.clone(), external.clone(), sandisk.clone()];
        assert_eq!(pick_stick(&all, None), Ok(sandisk.clone()));
        assert_eq!(pick_stick(&all, Some("sandisk")), Ok(sandisk.clone()));
        assert_eq!(pick_stick(&all, Some("e")), Ok(sandisk.clone()));
        // The system's disk and the external drive are no sticks, by name or by letter.
        for name in ["c", "samsung", "f", "seagate"] {
            assert!(pick_stick(&all, Some(name)).is_err_and(|e| e.starts_with(&format!("No stick here is called {name}"))), "{name}");
        }
        // Two sticks: he asks which, and takes the one named.
        let two = [system.clone(), sandisk.clone(), kingston.clone()];
        let which = pick_stick(&two, None).expect_err("two sticks");
        assert!(which.starts_with("I see 2 sticks, Sir: SanDisk Ultra, 31 GB") && which.contains("Kingston DataTraveler") && which.ends_with("make a J.A.I.OS stick on the SanDisk."), "{which}");
        assert_eq!(pick_stick(&two, Some("kingston")), Ok(kingston));
        assert_eq!(pick_stick(&two, Some("g")).map(|d| d.id), Ok("4".to_string()));
        // None at all, and only a drive that is no stick.
        assert_eq!(pick_stick(&[system.clone()], None), Err("I see no USB stick, Sir. Plug one in and ask again.".into()));
        let only = pick_stick(&[system, external], None).expect_err("no stick");
        assert!(only.starts_with("The only USB drive I see is Seagate Expansion, 2000 GB") && only.contains("larger than a stick"), "{only}");
        assert_eq!(stick_name(" on the usb stick"), None);
        assert_eq!(stick_name("my sandisk ultra usb-stick").as_deref(), Some("sandisk ultra"));
    }

    fn release(tag: &str, draft: bool, assets: &[(&str, u64)]) -> Value {
        json!({"tag_name": tag, "draft": draft, "assets": assets.iter().map(|(name, size)| json!({"name": name, "size": size, "browser_download_url": format!("https://github.com/JayKaey/JAI/releases/download/{tag}/{name}")})).collect::<Vec<_>>()})
    }

    #[test]
    fn the_newest_image_is_the_newest_release_that_holds_one_with_its_checksum() {
        let listed = json!([
            release("v3.6.0", false, &[("JAI.exe", 90_000_000), ("JAI-setup.exe", 80_000_000)]),
            release("jaios-1.2", true, &[("jaios-1.2-x86_64.iso", 1_900_000_000), ("jaios-1.2-x86_64.iso.sha256", 90)]),
            release("jaios-1.1", false, &[("jaios-1.1-x86_64.iso", 1_850_000_000)]),
            release("jaios-1.0", false, &[("jaios-1.0-x86_64.iso.sha256", 90), ("jaios-1.0-x86_64.iso", 1_800_000_000)]),
            release("jaios-0.9", false, &[("jaios-0.9-x86_64.iso", 1_700_000_000), ("jaios-0.9-x86_64.iso.sha256", 90)]),
        ]);
        // (1.2 is a draft, 1.1 has no checksum beside it.)
        assert_eq!(
            newest_iso(&listed),
            Some(Iso {
                version: "1.0".into(),
                name: "jaios-1.0-x86_64.iso".into(),
                url: "https://github.com/JayKaey/JAI/releases/download/jaios-1.0/jaios-1.0-x86_64.iso".into(),
                size: 1_800_000_000,
                sha_url: "https://github.com/JayKaey/JAI/releases/download/jaios-1.0/jaios-1.0-x86_64.iso.sha256".into(),
            })
        );
        // Until the first J.A.I.OS release there is none, and he says so.
        assert_eq!(newest_iso(&json!([release("v3.6.0", false, &[("JAI.exe", 1)])])), None);
        assert_eq!(newest_iso(&json!({"message": "rate limit"})), None);
        assert!(NO_RELEASE.contains("no J.A.I.OS release on GitHub yet"));
        let sha = "ab".repeat(32);
        assert_eq!(sha_of(&format!("{}  jaios-1.0-x86_64.iso\n", sha.to_uppercase())), Some(sha));
        assert_eq!((sha_of("not a checksum"), sha_of("")), (None, None));
    }

    #[test]
    fn the_readiness_report_names_what_is_missing() {
        let iso = Ok(Some(Iso { version: "1.0".into(), name: "jaios-1.0-x86_64.iso".into(), url: String::new(), size: 1_800_000_000, sha_url: String::new() }));
        let ready = Facts { windows: true, uefi: Some(true), secure_boot: Some(true), gpt: Some(true), bitlocker: Some(true), fast_startup: Some(false), c_size: Some(800 * GIB), c_free: Some(300 * GIB), unallocated: Some(100 * GIB), size_min: None };
        let (rows, line) = report(&ready, &iso);
        assert_eq!(
            rows,
            "SYSTEM          Windows\nFIRMWARE        UEFI\nSECURE BOOT     on, and it stays on\nDISK            GPT\nBITLOCKER       on: keep its recovery key at hand\nFAST STARTUP    off\nROOM AFTER C:   100 GB, enough\nINSTALL IMAGE   J.A.I.OS 1.0, 1.8 GB"
        );
        assert_eq!(line, "Yes, Sir: UEFI, a GPT disk, Secure Boot on, and 100 GB free after C:. BitLocker is on, which is fine: keep its recovery key at hand. The newest install image is J.A.I.OS 1.0.");
        // His own PC before anything is done: no room yet, Fast Startup on, no release yet.
        let before = Facts { unallocated: Some(GIB / 2), fast_startup: Some(true), bitlocker: Some(false), ..ready.clone() };
        let (rows, line) = report(&before, &Ok(None));
        assert!(rows.contains("ROOM AFTER C:   0 GB of the 40 it needs") && rows.contains("FAST STARTUP    on: should be off for a dual boot") && rows.contains("INSTALL IMAGE   none released yet"), "{rows}");
        assert_eq!(line, "Nearly, Sir: there are 0 GB free after C: of the 40 it needs, so say: make room for J.A.I.OS; and Fast Startup is on and should be off. There is no J.A.I.OS release on GitHub yet.");
        // An old PC: what cannot be helped comes first.
        let old = Facts { uefi: Some(false), secure_boot: Some(false), gpt: Some(false), ..before };
        let (rows, line) = report(&old, &Err("offline".into()));
        assert!(rows.contains("FIRMWARE        old BIOS: J.A.I.OS needs UEFI") && rows.contains("DISK            MBR: J.A.I.OS needs GPT") && rows.contains("INSTALL IMAGE   GitHub did not answer"), "{rows}");
        assert_eq!(line, "Not as it is, Sir: it starts the old BIOS way and J.A.I.OS needs UEFI, and Windows' disk is MBR and J.A.I.OS needs GPT. GitHub did not tell me the newest image.");
        // What Windows did not say is "not known", and counts as no room.
        let (rows, line) = report(&Facts { windows: true, ..Facts::default() }, &iso);
        assert!(rows.contains("FIRMWARE        not known") && rows.contains("BITLOCKER       not known") && line.starts_with("Nearly, Sir: there are 0 GB free"), "{rows} {line}");
        // On Linux: no Windows rows, and room is Windows' to make.
        let (rows, line) = report(&Facts { uefi: Some(true), secure_boot: Some(true), ..Facts::default() }, &iso);
        assert_eq!(rows, "SYSTEM          Linux\nFIRMWARE        UEFI\nSECURE BOOT     on, and it stays on\nINSTALL IMAGE   J.A.I.OS 1.0, 1.8 GB");
        assert_eq!(line, "This PC runs Linux already, Sir, started by UEFI with Secure Boot on. The newest install image is J.A.I.OS 1.0. Room beside Windows is made from Windows.");
    }

    #[tokio::test]
    async fn no_test_reaches_a_disk() {
        let _kit = crate::kit::test_lock();
        // A yes with nothing asked asks again instead of writing; a stick that was asked about
        // is not written by a test either.
        let stick = stick_disk("2", "SanDisk Ultra", "E");
        let iso = Iso { version: "1.0".into(), name: "jaios-1.0-x86_64.iso".into(), url: String::new(), size: 1, sha_url: String::new() };
        assert_eq!(write_stick(stick, iso), one("A test session writes no stick, Sir."));
        assert_eq!(start(Job::FastStartupOff, |_| String::new()), Err("A test session changes no disk, Sir.".into()));
        assert!(!BUSY.load(Ordering::Acquire));
        if !cfg!(windows) {
            assert_eq!(room(&json!({"action": "room"})).await, one(ROOM_IS_WINDOWS));
        }
    }
}
