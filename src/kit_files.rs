//! Sir's files by voice (he chose them on 2026-11-22), on what is selected in the file manager in
//! front (Explorer through the shell's own windows list; Dolphin through a copy of the selection):
//! - "rename this to Exam notes" (the extension kept; "undo that" brings the old name back),
//! - "zip these files" / "unzip this" (the system's own tar: libarchive on both systems),
//! - "make this picture smaller", "convert this to JPG" (a copy beside the original),
//! - "print this", "print it twice" (the default printer),
//! - "copy the path of this", "make a new folder called Exams" ("undo that" removes it while it
//!   is empty),
//! - "open what I just downloaded" / "show my last download",
//! - "open the document I had open yesterday", "what did I work on this morning?" (Windows' list
//!   of recent files; KDE's recently-used.xbel),
//! - "free up some space": what the old temp files and the Recycle Bin hold, emptied on his yes
//!   (`hud_lines::offer`). On Linux only the Trash: the system clears /tmp itself.

use crate::kit::say;
use crate::tools::ToolContext;
use chrono::{Local, NaiveDateTime};
use regex::Regex;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

/// "this", "these files", "the selected picture": what is selected in the file manager.
const THIS: &str = r"(?:(?:this|that|these|those|the|the selected|my selected)\s+(?:files?|pictures?|images?|photos?|screenshots?|folders?|documents?|pdfs?|archives?|zip\s+files?|zips?)|this|these|that|those|it|them|the\s+selection|my\s+selection)";
const DAS: &str = r"(?:(?:die|diese|das|dieses|den|diesen)\s+(?:datei|dateien|bild|bilder|foto|fotos|ordner|dokument|dokumente|archiv)|das|die|diese|dieses|es|sie)";
/// A picture by its own word ("make this smaller" is as often a window).
const PIC: &str = r"(?:this|that|these|those|the|the selected|my)\s+(?:pictures?|images?|photos?|screenshots?|pics?)";
const BILD: &str = r"(?:das|die|dieses|diese)\s+(?:bild|bilder|foto|fotos|screenshot)";

fn re(pattern: &str) -> Regex {
    Regex::new(&pattern.replace("{THIS}", THIS).replace("{DAS}", DAS).replace("{PIC}", PIC).replace("{BILD}", BILD)).expect("a valid file phrase")
}

static FREE_ORDER: LazyLock<Regex> = LazyLock::new(|| re(r"^empty (?P<w>the temp files and the recycle bin|the temp files|the recycle bin|the trash) for good$"));
static FREE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:free\s+up|free|clear\s+up|make)\s+(?:some\s+|more\s+)?(?:disk\s+|drive\s+|storage\s+)?(?:space|room)(?:\s+on\s+(?:my|the)\s+(?:pc|computer|laptop|drive|disk|c\s+drive))?$|^(?P<bin>(?:empty|clear)\s+(?:my\s+|the\s+)?(?:recycle\s+bin|trash|bin))$|^(?P<temp>(?:clear|delete|clean\s+up|clean|remove|empty)\s+(?:my\s+|the\s+|all\s+(?:the\s+)?)?(?:temp|temporary)\s+files)$|^(?:mach|mache|schaff|schaffe)\s+(?:etwas\s+|mehr\s+)?(?:speicher)?platz(?:\s+frei)?(?:\s+auf\s+(?:meinem|dem)\s+(?:pc|rechner|laufwerk))?$|^(?:räum|räume|raeum)\s+(?:etwas\s+)?speicher(?:platz)?\s+frei$|^(?P<bin2>(?:leere|leer)\s+(?:den\s+|meinen\s+)?papierkorb)$|^(?P<temp2>(?:lösche|lösch|leere)\s+(?:die\s+)?(?:temp|temporären)[\s-]?dateien)$")
});
static RENAME: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^rename\s+{THIS}\s+(?:to|as|into)\s+(?P<n>.+)$|^change\s+the\s+name\s+of\s+{THIS}\s+to\s+(?P<n2>.+)$|^(?:benenne|benenn)\s+{DAS}\s+(?:um\s+)?in\s+(?P<n3>.+?)(?:\s+um)?$")
});
static COMPRESS: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:compress|komprimiere|komprimier)\s+(?P<t>{THIS}|{DAS})$"));
static ZIP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:zip|pack)\s+(?:up\s+)?{THIS}(?:\s+up)?(?:\s+(?:into|in|as)\s+(?:a\s+)?zip(?:\s+file)?)?$|^(?:make|create)\s+a\s+zip(?:\s+file)?\s+(?:of|from|with)\s+{THIS}$|^(?:zippe|zipp|pack|packe)\s+{DAS}(?:\s+(?:ein|zusammen|in\s+eine\s+zip(?:-datei)?))?$")
});
static UNZIP: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:unzip|extract|unpack|decompress)\s+{THIS}(?:\s+here)?$|^(?:entpacke|entpack)\s+{DAS}(?:\s+hier)?$"));
static SHRINK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|get)\s+{PIC}\s+(?:smaller|lighter|tiny)$|^(?:shrink|resize|downsize|scale\s+down)\s+{PIC}(?:\s+down)?$|^(?:mach|mache)\s+{BILD}\s+kleiner$|^(?:verkleinere|verkleiner)\s+{BILD}$")
});
static CONVERT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:convert|turn|change|save|make)\s+{THIS}\s+(?:to|into|as)\s+(?:a\s+|an\s+)?(?P<f>jpe?g|png|webp)(?:\s+(?:file|picture|image))?$|^(?:wandle|wandel|konvertiere|speichere)\s+{DAS}\s+(?:in|als|zu)\s+(?:ein\s+|eine\s+)?(?P<f2>jpe?g|png|webp)(?:\s+um)?$")
});
static PRINT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^print\s+(?:out\s+)?{THIS}(?:\s+out)?(?:\s+(?P<n>once|twice|thrice|(?:\d+|two|three|four|five)\s+times|in\s+(?:\d+|two|three|four|five)\s+copies|(?:\d+|two|three|four|five)\s+copies))?$|^print\s+(?P<n2>\d+|two|three|four|five)\s+copies\s+of\s+{THIS}$|^(?:drucke|druck)\s+{DAS}(?:\s+(?P<n3>einmal|zweimal|dreimal|\d+\s*mal))?(?:\s+aus)?$")
});
static COPY_PATH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^copy\s+(?:the\s+|its\s+)?(?:full\s+)?(?:file\s+|folder\s+)?path(?:\s+(?:of|to|for)\s+{THIS})?$|^(?:kopiere|kopier)\s+(?:mir\s+)?(?:den\s+)?(?:datei|ordner)?pfad(?:\s+(?:davon|von\s+{DAS}))?$")
});
static NEW_FOLDER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|create|add)\s+(?:a\s+)?new\s+folder(?:\s+here)?(?:\s+(?:called|named)\s+(?P<n>.+?))?(?:\s+here)?$|^(?:make|create)\s+a\s+folder\s+(?:here\s+)?(?:called|named)\s+(?P<n2>.+?)(?:\s+here)?$|^(?:erstelle|erstell|mach|mache|leg|lege)\s+(?:hier\s+)?(?:einen\s+)?neuen\s+ordner(?:\s+(?:namens|mit\s+dem\s+namen)\s+(?P<n3>.+?))?(?:\s+an)?$")
});
static LAST_DOWNLOAD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?P<v>open|show|reveal|find|where\s+is|wheres)\s+(?:me\s+)?(?:my\s+|the\s+)?(?:last|latest|newest|most\s+recent)\s+download(?:ed\s+file)?$|^(?P<v2>open|show\s+me|show)\s+what\s+i\s+(?:just\s+|last\s+)?downloaded$|^(?P<v3>öffne|zeig\s+mir|zeige\s+mir|zeig)\s+(?:meinen\s+|meine\s+|den\s+|die\s+)?(?:letzten|neuesten|letzte|neueste)\s+download$|^(?P<v4>öffne|zeig\s+mir)\s+was\s+ich\s+(?:gerade\s+|zuletzt\s+)?(?:heruntergeladen|runtergeladen)\s+habe$")
});
static RECENT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?P<open>open|reopen)\s+(?:the\s+)?(?P<k>document|file|pdf|presentation|spreadsheet|picture|photo)\s+i\s+(?:had\s+open|opened|was\s+working\s+on|worked\s+on|was\s+using|used)(?:\s+(?P<w>.+))?$|^(?P<open2>open|reopen)\s+(?:my\s+|the\s+)?(?:last|latest|most\s+recent)\s+(?P<k2>document|file|pdf|presentation|spreadsheet)$|^what\s+(?:did\s+i\s+work\s+on|(?:files|documents)\s+did\s+i\s+(?:open|have\s+open|use|work\s+on|edit))(?:\s+(?P<w2>.+))?$|^what\s+was\s+i\s+working\s+on\s+(?P<w3>.+)$|^(?:show|list)\s+(?:me\s+)?(?:my\s+)?recent\s+(?:files|documents)(?:\s+(?P<w4>.+))?$|^woran\s+habe?\s+ich\s+(?P<w5>.+?)\s+gearbeitet$|^(?P<open3>öffne)\s+(?:das\s+dokument|die\s+datei)\s+(?:das|die)\s+ich\s+(?P<w6>.+?)\s+offen\s+hatte$|^zeig(?:e)?\s+(?:mir\s+)?(?:meine\s+)?(?:zuletzt\s+geöffneten|letzten)\s+(?:dateien|dokumente)$")
});

/// When he means, in his words: "" is the last seven days.
fn when_of(words: &str) -> Option<&'static str> {
    Some(match words.trim() {
        "" => "",
        "today" | "heute" => "today",
        "this morning" | "in the morning" | "heute morgen" | "heute früh" | "heute vormittag" => "morning",
        "this afternoon" | "heute nachmittag" => "afternoon",
        "this evening" | "tonight" | "heute abend" => "evening",
        "yesterday" | "gestern" => "yesterday",
        "this week" | "last week" | "lately" | "recently" | "diese woche" | "letzte woche" => "week",
        _ => return None,
    })
}

fn copies(word: &str) -> u64 {
    let word = word.trim().trim_start_matches("in ").trim_end_matches(" copies").trim_end_matches(" times").trim_end_matches("mal").trim();
    match word {
        "twice" | "two" | "zwei" | "zweimal" => 2,
        "thrice" | "three" | "drei" | "dreimal" => 3,
        "four" => 4,
        "five" => 5,
        other => other.parse().unwrap_or(1),
    }
    .clamp(1, 10)
}

/// An order about his files: the tool's arguments. `original`: what he said, for the names he
/// gives (their capitals).
pub fn command(s: &str, original: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    let pick = |c: &regex::Captures, names: &[&str]| names.iter().find_map(|n| c.name(n)).map(|m| m.as_str().trim().to_string());
    let named = |c: &regex::Captures, names: &[&str]| pick(c, names).map(|n| crate::ways::as_said(original, &n));
    if let Some(c) = FREE_ORDER.captures(s) {
        let what = match &c["w"] {
            "the temp files" => "temp",
            "the recycle bin" | "the trash" => "bin",
            _ => "both",
        };
        return Some(json!({"action": "free_space", "what": what, "confirmed": true}));
    }
    if let Some(c) = FREE.captures(s) {
        let what = if c.name("bin").or(c.name("bin2")).is_some() { "bin" } else if c.name("temp").or(c.name("temp2")).is_some() { "temp" } else { "both" };
        return Some(json!({"action": "free_space", "what": what}));
    }
    if let Some(c) = RENAME.captures(s) {
        return Some(json!({"action": "rename", "name": named(&c, &["n", "n2", "n3"])?}));
    }
    if let Some(c) = COMPRESS.captures(s) {
        let pictures = ["pictur", "imag", "photo", "screenshot", "bild", "foto"].iter().any(|w| c["t"].contains(w));
        return Some(json!({"action": if pictures { "shrink" } else { "zip" }}));
    }
    if ZIP.is_match(s) {
        return Some(json!({"action": "zip"}));
    }
    if UNZIP.is_match(s) {
        return Some(json!({"action": "unzip"}));
    }
    if SHRINK.is_match(s) {
        return Some(json!({"action": "shrink"}));
    }
    if let Some(c) = CONVERT.captures(s) {
        let to = pick(&c, &["f", "f2"])?.replace("jpeg", "jpg");
        return Some(json!({"action": "convert", "to": to}));
    }
    if let Some(c) = PRINT.captures(s) {
        return Some(json!({"action": "print", "copies": pick(&c, &["n", "n2", "n3"]).map_or(1, |n| copies(&n))}));
    }
    if COPY_PATH.is_match(s) {
        return Some(json!({"action": "copy_path"}));
    }
    if let Some(c) = NEW_FOLDER.captures(s) {
        return Some(json!({"action": "new_folder", "name": named(&c, &["n", "n2", "n3"]).unwrap_or_default()}));
    }
    if let Some(c) = LAST_DOWNLOAD.captures(s) {
        let verb = pick(&c, &["v", "v2", "v3", "v4"])?;
        return Some(json!({"action": "last_download", "reveal": !matches!(verb.as_str(), "open" | "öffne")}));
    }
    let c = RECENT.captures(s)?;
    let when = when_of(&pick(&c, &["w", "w2", "w3", "w4", "w5", "w6"]).unwrap_or_default())?;
    let open = c.name("open").or(c.name("open2")).or(c.name("open3")).is_some();
    let kind_word = pick(&c, &["k", "k2"]);
    let kind = match kind_word.as_deref() {
        Some("document") | None if c.name("open3").is_some() => "document",
        Some("file") | None => "",
        Some(kind) => kind,
    };
    Some(json!({"action": "recent", "when": when, "open": open, "kind": kind}))
}

/// Whether the call only reads (a question may ask for it).
pub fn only_looks(args: &Value) -> bool {
    match args["action"].as_str().unwrap_or("") {
        "recent" => !args["open"].as_bool().unwrap_or(false),
        "free_space" => !args["confirmed"].as_bool().unwrap_or(false),
        _ => false,
    }
}

/// A test, a scripted session or a dry run touches none of his files.
fn tied() -> bool {
    crate::platform::hands_off() || crate::live_self_tester::dry()
}

fn not_in_a_test(what: &str) -> String {
    say(format!("{what} (not from a test)."), "Done, Sir.")
}

pub async fn run(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("") {
        "free_space" => free_space(args).await,
        "recent" => recent(args).await,
        "last_download" => last_download(args["reveal"].as_bool().unwrap_or(false)).await,
        "move_back" => move_back(args),
        "remove_folder" => remove_folder(args),
        "new_folder" => new_folder(ctx, args["name"].as_str().unwrap_or("")).await,
        "copy_path" => copy_path(ctx).await,
        "rename" => rename(ctx, args["name"].as_str().unwrap_or("")).await,
        "zip" => zip(ctx).await,
        "unzip" => unzip(ctx).await,
        "shrink" => pictures(ctx, None).await,
        "convert" => pictures(ctx, args["to"].as_str()).await,
        "print" => print(ctx, args["copies"].as_u64().unwrap_or(1).clamp(1, 10)).await,
        other => Err(format!("The file helper has no action '{other}'.")),
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------- the selection ----

const NOTHING_IN_FRONT: &str = if cfg!(windows) { "I see no Explorer window, Sir: open the folder first." } else { "Nothing is selected in Dolphin, Sir: select it first." };

/// The folder of the file manager in front and what is selected there.
async fn selection(ctx: &ToolContext) -> Result<(PathBuf, Vec<PathBuf>), String> {
    #[cfg(windows)]
    let found = {
        let _ = ctx;
        tokio::task::spawn_blocking(explorer_selection).await.map_err(|e| e.to_string())?
    };
    #[cfg(not(windows))]
    let found = manager_selection(ctx).await?;
    found.ok_or_else(|| NOTHING_IN_FRONT.to_string())
}

async fn selected(ctx: &ToolContext, what: &str) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let (folder, items) = selection(ctx).await?;
    if items.is_empty() {
        return Err(format!("Select {what} first, Sir."));
    }
    Ok((folder, items))
}

/// The Explorer window highest in the z-order: its folder (the tab in front) and its selection.
#[cfg(windows)]
fn explorer_selection() -> Option<(PathBuf, Vec<PathBuf>)> {
    use windows::core::{Interface, VARIANT};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IServiceProvider, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{IFolderView, IShellBrowser, IShellItemArray, IShellWindows, IWebBrowser2, ShellWindows, SID_STopLevelBrowser, SIGDN_FILESYSPATH, SVGIO_SELECTION};
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    let order: Vec<isize> = crate::win32::app_windows().into_iter().filter(|w| w.class == "CabinetWClass").map(|w| w.hwnd).collect();
    if order.is_empty() {
        return None;
    }
    // SAFETY: COM is set up on this blocking thread for the calls below and taken down after;
    // every interface is owned, and each name the shell allocates is copied, then freed with
    // CoTaskMemFree as the API requires.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let found = (|| {
            let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;
            let mut best: Option<(usize, PathBuf, Vec<PathBuf>)> = None;
            for i in 0..windows.Count().ok()? {
                let Ok(browser) = windows.Item(&VARIANT::from(i)).and_then(|d| d.cast::<IWebBrowser2>()) else { continue };
                let Some(rank) = browser.HWND().ok().and_then(|h| order.iter().position(|w| *w == h.0)) else { continue };
                if best.as_ref().is_some_and(|(r, ..)| *r <= rank) {
                    continue;
                }
                let Ok(view) = browser.cast::<IServiceProvider>().and_then(|s| s.QueryService::<IShellBrowser>(&SID_STopLevelBrowser)).and_then(|b| b.QueryActiveShellView()) else { continue };
                // (A window of several tabs lists each: the tab in front is the one whose view shows.)
                if !view.GetWindow().is_ok_and(|h| IsWindowVisible(h).as_bool()) {
                    continue;
                }
                let Some(folder) = browser.LocationURL().ok().and_then(|u| path_of_url(&u.to_string())) else { continue };
                let items = view
                    .cast::<IFolderView>()
                    .and_then(|v| v.Items::<IShellItemArray>(SVGIO_SELECTION))
                    .map(|array| {
                        (0..array.GetCount().unwrap_or(0))
                            .filter_map(|n| {
                                let name = array.GetItemAt(n).and_then(|item| item.GetDisplayName(SIGDN_FILESYSPATH)).ok()?;
                                let path = name.to_string().ok();
                                CoTaskMemFree(Some(name.0 as *const _));
                                path.map(PathBuf::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                best = Some((rank, folder, items));
            }
            best.map(|(_, folder, items)| (folder, items))
        })();
        if init {
            CoUninitialize();
        }
        found
    }
}

/// Dolphin keeps its selection to itself: it is copied (his clipboard put back after) and read
/// as the list of files it is. Its folder is the first item's.
#[cfg(not(windows))]
async fn manager_selection(ctx: &ToolContext) -> Result<Option<(PathBuf, Vec<PathBuf>)>, String> {
    let before = crate::clipboard::read().await.unwrap_or_default();
    crate::kit_typing::copy_in_his_app(ctx).await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let out = crate::shell::run_helper(Path::new("wl-paste"), ["--no-newline", "--type", "text/uri-list"], None, &[], Duration::from_secs(3)).await.map_err(|e| format!("wl-paste (wl-clipboard) did not answer: {e}"))?;
    if !before.is_empty() {
        let _ = crate::clipboard::write(&before).await;
    }
    let items: Vec<PathBuf> = String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.starts_with('#')).filter_map(path_of_url).collect();
    Ok(items.first().and_then(|p| p.parent()).map(|folder| (folder.to_path_buf(), items.clone())))
}

/// A `file://` address as the path it names (Explorer's folders, Dolphin's selection, KDE's
/// recent files).
fn path_of_url(url: &str) -> Option<PathBuf> {
    let rest = url.trim().strip_prefix("file://")?;
    let decoded = urlencoding::decode(rest).ok()?.into_owned();
    #[cfg(windows)]
    let decoded = match decoded.strip_prefix('/') {
        Some(local) => local.replace('/', "\\"),
        None => format!("\\\\{}", decoded.replace('/', "\\")),
    };
    (!decoded.is_empty()).then(|| PathBuf::from(decoded))
}

/// `path`, or "name (2).ext", "name (3).ext" … where it is taken.
fn unique(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..1000).map(|n| path.with_file_name(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap_or(path)
}

/// The name he gave, fit for a file: its own extension kept ("Exam notes" → "Exam notes.pdf").
fn new_name(old: &Path, said: &str) -> Result<String, String> {
    let said = said.trim().trim_matches(['"', '\'', '“', '”']).trim_end_matches('.').trim();
    if said.is_empty() || matches!(said, "." | "..") || said.chars().any(|c| c.is_control() || "<>:\"/\\|?*".contains(c)) {
        return Err("That name won't do for a file, Sir.".to_string());
    }
    Ok(match old.extension().filter(|_| !old.is_dir()).map(|e| e.to_string_lossy().into_owned()) {
        Some(ext) if !said.to_lowercase().ends_with(&format!(".{}", ext.to_lowercase())) => format!("{said}.{ext}"),
        _ => said.to_string(),
    })
}

fn keep_undo(done: &'static str, args: Value) {
    crate::undo::keep(crate::undo::Step { done, interface: None, windows: Vec::new(), widgets: Vec::new(), calls: vec![("file_helper", args)] });
}

// -------------------------------------------------------------------------- the orders ----

async fn rename(ctx: &ToolContext, said: &str) -> Result<String, String> {
    let (_, items) = selected(ctx, "the file to rename").await?;
    let [item] = items.as_slice() else { return Err("Select just the one file to rename, Sir.".to_string()) };
    let name = new_name(item, said)?;
    let to = item.with_file_name(&name);
    if to.exists() {
        return Err(format!("There's a {name} there already, Sir."));
    }
    if tied() {
        return Ok(not_in_a_test(&format!("Would rename {} to {name}", name_of(item))));
    }
    std::fs::rename(item, &to).map_err(|e| format!("I couldn't rename {}, Sir: {e}", name_of(item)))?;
    keep_undo("The old name is back.", json!({"action": "move_back", "from": to, "to": item}));
    Ok(say(format!("Renamed {} to {name}.", name_of(item)), format!("Renamed it to {name}, Sir.")))
}

/// "Undo that" after a rename.
fn move_back(args: &Value) -> Result<String, String> {
    let (from, to) = (PathBuf::from(args["from"].as_str().unwrap_or("")), PathBuf::from(args["to"].as_str().unwrap_or("")));
    if !from.exists() || to.exists() || to.as_os_str().is_empty() {
        return Err("The file has moved on since, Sir: I left it as it is.".to_string());
    }
    std::fs::rename(&from, &to).map_err(|e| format!("I couldn't give it its old name back, Sir: {e}"))?;
    Ok(say(format!("{} is called {} again.", name_of(&from), name_of(&to)), "The old name is back, Sir."))
}

async fn new_folder(ctx: &ToolContext, said: &str) -> Result<String, String> {
    let (folder, _) = selection(ctx).await?;
    let name = if said.trim().is_empty() { "New folder".to_string() } else { new_name(Path::new(""), said)? };
    let path = folder.join(&name);
    if path.exists() {
        return Err(format!("There's a {name} in {} already, Sir.", name_of(&folder)));
    }
    if tied() {
        return Ok(not_in_a_test(&format!("Would make {}", path.display())));
    }
    std::fs::create_dir(&path).map_err(|e| format!("I couldn't make the folder, Sir: {e}"))?;
    keep_undo("The new folder is gone again.", json!({"action": "remove_folder", "path": path}));
    Ok(say(format!("Made the folder {name} in {}.", folder.display()), format!("Made the folder {name} in {}, Sir.", name_of(&folder))))
}

/// "Undo that" after a new folder: only while it is still empty.
fn remove_folder(args: &Value) -> Result<String, String> {
    let path = PathBuf::from(args["path"].as_str().unwrap_or(""));
    if path.read_dir().map_or(true, |mut d| d.next().is_some()) {
        return Err("The folder isn't empty any more, Sir: I left it.".to_string());
    }
    std::fs::remove_dir(&path).map_err(|e| format!("I couldn't remove the folder, Sir: {e}"))?;
    Ok(say(format!("Removed the empty folder {}.", path.display()), "The new folder is gone again, Sir."))
}

async fn copy_path(ctx: &ToolContext) -> Result<String, String> {
    let (folder, items) = selection(ctx).await?;
    let paths = if items.is_empty() { vec![folder] } else { items };
    let text = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
    if tied() {
        return Ok(not_in_a_test(&format!("Would copy {text}")));
    }
    crate::clipboard::write(&text).await?;
    let spoken = match paths.as_slice() {
        [one] => format!("Copied the path of {}, Sir.", name_of(one)),
        many => format!("Copied the paths of {} items, Sir.", many.len()),
    };
    Ok(say(format!("Copied:\n{text}"), spoken))
}

/// The system's own tar (libarchive): Windows' in System32 (Git's GNU tar can't write zips),
/// KDE's bsdtar.
async fn tar(args: Vec<OsString>) -> Result<(), String> {
    #[cfg(windows)]
    let program = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string())).join("System32").join("tar.exe");
    #[cfg(not(windows))]
    let program = PathBuf::from("bsdtar");
    let out = crate::shell::run_helper(&program, args, None, &[], Duration::from_secs(600)).await.map_err(|e| format!("tar didn't run, Sir: {e}"))?;
    if out.code == Some(0) {
        Ok(())
    } else {
        Err(format!("tar failed, Sir: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// tar's arguments for a zip of `items`, each from its own folder.
fn zip_args(out: &Path, items: &[PathBuf]) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-a", "-c", "-f"].map(OsString::from).into();
    args.push(out.into());
    for item in items {
        if let (Some(dir), Some(name)) = (item.parent(), item.file_name()) {
            args.extend([OsString::from("-C"), dir.into(), name.to_os_string()]);
        }
    }
    args
}

async fn zip(ctx: &ToolContext) -> Result<String, String> {
    let (folder, items) = selected(ctx, "what to zip").await?;
    let stem = match items.as_slice() {
        [one] => one.file_stem().map(|s| s.to_string_lossy().into_owned()),
        _ => folder.file_name().map(|s| s.to_string_lossy().into_owned()),
    }
    .unwrap_or_else(|| "Archive".to_string());
    let out = unique(folder.join(format!("{stem}.zip")));
    if tied() {
        return Ok(not_in_a_test(&format!("Would zip {} items into {}", items.len(), out.display())));
    }
    tar(zip_args(&out, &items)).await?;
    let size = std::fs::metadata(&out).map(|m| crate::disk_map::spoken_size(m.len())).unwrap_or_default();
    Ok(say(format!("Zipped {} item{} into {} ({size}).", items.len(), if items.len() == 1 { "" } else { "s" }, name_of(&out)), format!("Zipped into {}, Sir.", name_of(&out))))
}

const ARCHIVES: [&str; 9] = ["zip", "7z", "rar", "tar", "gz", "tgz", "xz", "bz2", "zst"];

async fn unzip(ctx: &ToolContext) -> Result<String, String> {
    let (folder, items) = selected(ctx, "the archive to unpack").await?;
    let [archive] = items.as_slice() else { return Err("Select just the one archive to unpack, Sir.".to_string()) };
    if !archive.extension().is_some_and(|e| ARCHIVES.contains(&e.to_string_lossy().to_lowercase().as_str())) {
        return Err(format!("{} is no archive I can unpack, Sir.", name_of(archive)));
    }
    let stem = name_of(archive).split('.').next().filter(|s| !s.is_empty()).unwrap_or("Unpacked").to_string();
    let dest = unique(folder.join(stem));
    if tied() {
        return Ok(not_in_a_test(&format!("Would unpack {} into {}", name_of(archive), dest.display())));
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("I couldn't make the folder to unpack into, Sir: {e}"))?;
    if let Err(e) = tar(vec!["-x".into(), "-f".into(), archive.into(), "-C".into(), dest.clone().into()]).await {
        let _ = std::fs::remove_dir(&dest);
        return Err(e);
    }
    Ok(say(format!("Unpacked {} into {}.", name_of(archive), dest.display()), format!("Unpacked into the folder {}, Sir.", name_of(&dest))))
}

const PICTURES: [&str; 8] = ["png", "jpg", "jpeg", "webp", "bmp", "gif", "tif", "tiff"];

/// A smaller copy (half the size, at most 1920 pixels across) or one in another format, beside
/// the original: its path.
fn picture_copy(src: &Path, to: Option<&str>) -> Result<PathBuf, String> {
    use image::{DynamicImage, ImageFormat};
    let img = image::open(src).map_err(|e| format!("I can't read {} as a picture, Sir: {e}", name_of(src)))?;
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let alpha = img.color().has_alpha();
    let (img, ext, tail) = match to {
        Some(ext) => (img, ext, ""),
        None => {
            let long = img.width().max(img.height());
            let target = (long / 2).min(1920);
            if target < 16 {
                return Err(format!("{} is tiny already, Sir.", name_of(src)));
            }
            // (A picture with see-through parts stays a PNG: a JPEG would fill them with black.)
            (img.resize(target, target, image::imageops::FilterType::Lanczos3), if alpha { "png" } else { "jpg" }, " (small)")
        }
    };
    let out = unique(src.with_file_name(format!("{stem}{tail}.{ext}")));
    let written = match ext {
        "jpg" => std::fs::File::create(&out).map_err(image::ImageError::IoError).and_then(|file| {
            DynamicImage::ImageRgb8(img.to_rgb8()).write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 85))
        }),
        "png" => img.save_with_format(&out, ImageFormat::Png),
        // (The image crate writes WebP losslessly only.)
        _ => DynamicImage::ImageRgba8(img.to_rgba8()).save_with_format(&out, ImageFormat::WebP),
    };
    written.map_err(|e| format!("I couldn't save {}, Sir: {e}", name_of(&out)))?;
    Ok(out)
}

async fn pictures(ctx: &ToolContext, to: Option<&str>) -> Result<String, String> {
    let (_, items) = selected(ctx, "the picture").await?;
    let pictures: Vec<PathBuf> = items.into_iter().filter(|p| p.extension().is_some_and(|e| PICTURES.contains(&e.to_string_lossy().to_lowercase().as_str()))).collect();
    if pictures.is_empty() {
        return Err("Nothing selected is a picture I can read, Sir.".to_string());
    }
    if tied() {
        return Ok(not_in_a_test(&format!("Would save {} picture{} {}", pictures.len(), if pictures.len() == 1 { "" } else { "s" }, to.map_or("smaller".to_string(), |t| format!("as {}", t.to_uppercase())))));
    }
    let to = to.map(str::to_string);
    let (made, failed) = tokio::task::spawn_blocking(move || {
        let (mut made, mut failed) = (Vec::new(), Vec::new());
        for src in &pictures {
            match picture_copy(src, to.as_deref()) {
                Ok(out) => made.push((std::fs::metadata(src).map_or(0, |m| m.len()), out)),
                Err(e) => failed.push(e),
            }
        }
        (made, failed)
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some((before, first)) = made.first() else { return Err(failed.into_iter().next().unwrap_or_default()) };
    let after = std::fs::metadata(first).map_or(0, |m| m.len());
    let display = made.iter().map(|(_, p)| p.display().to_string()).chain(failed).collect::<Vec<_>>().join("\n");
    let spoken = match made.len() {
        1 if after < *before => format!("Saved {}, Sir: {} instead of {}.", name_of(first), crate::disk_map::spoken_size(after), crate::disk_map::spoken_size(*before)),
        1 => format!("Saved {}, Sir.", name_of(first)),
        n => format!("Saved {n} pictures beside the originals, Sir."),
    };
    Ok(say(display, spoken))
}

async fn print(ctx: &ToolContext, copies: u64) -> Result<String, String> {
    let (_, items) = selected(ctx, "what to print").await?;
    let files: Vec<PathBuf> = items.into_iter().filter(|p| p.is_file()).collect();
    if files.is_empty() {
        return Err("A folder can't be printed, Sir: select the file.".to_string());
    }
    let what = match files.as_slice() {
        [one] => name_of(one),
        many => format!("{} files", many.len()),
    };
    let times = if copies > 1 { format!("{copies} copies of ") } else { String::new() };
    if tied() {
        return Ok(not_in_a_test(&format!("Would print {times}{what}")));
    }
    #[cfg(windows)]
    for file in &files {
        for _ in 0..copies {
            let file = file.display().to_string();
            crate::win32::call(move || crate::win32::shell_verb("print", &file, None, None)).await?.map_err(|e| format!("Windows wouldn't print it, Sir: {e}"))?;
        }
    }
    #[cfg(not(windows))]
    {
        let mut args: Vec<OsString> = vec!["-n".into(), copies.to_string().into()];
        args.extend(files.iter().map(|f| f.as_os_str().to_os_string()));
        let out = crate::shell::run_helper(Path::new("lp"), args, None, &[], Duration::from_secs(30)).await.map_err(|_| "Printing needs CUPS (its lp command), Sir.".to_string())?;
        if out.code != Some(0) {
            return Err(format!("The printer refused it, Sir: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
    }
    Ok(say(format!("Printing {times}{what}."), format!("Printing {times}{what}, Sir.")))
}

// ------------------------------------------------------------------- downloads, recent ----

/// What a browser still writes, or a folder's own settings: never "the last download".
fn finished_download(name: &str) -> bool {
    let lower = name.to_lowercase();
    !lower.starts_with('.') && lower != "desktop.ini" && ![".crdownload", ".part", ".partial", ".tmp", ".download", ".opdownload"].iter().any(|e| lower.ends_with(e))
}

async fn last_download(reveal: bool) -> Result<String, String> {
    let folder = crate::disk_map::known_folder("Downloads").ok_or("I can't find your Downloads folder, Sir.")?;
    let newest = std::fs::read_dir(&folder)
        .map_err(|e| format!("I can't read your Downloads folder, Sir: {e}"))?
        .flatten()
        .filter(|e| finished_download(&e.file_name().to_string_lossy()))
        .filter_map(|e| Some((e.metadata().ok().filter(|m| m.is_file())?.modified().ok()?, e.path())))
        .max_by_key(|(at, _)| *at)
        .map(|(_, path)| path)
        .ok_or("Your Downloads folder is empty, Sir.")?;
    let name = name_of(&newest);
    if tied() {
        return Ok(not_in_a_test(&format!("Would {} {name}", if reveal { "show" } else { "open" })));
    }
    crate::file_ops::open_with(&newest, reveal.then_some("reveal")).await?;
    Ok(if reveal { say(format!("There it is: {}", newest.display()), format!("There it is, Sir: {name}.")) } else { say(format!("Opened {}", newest.display()), format!("Opening {name}, Sir.")) })
}

/// A file he opened: its name, what opens it (Windows' shortcut, or the file), and when.
#[derive(Debug, Clone, PartialEq)]
struct Recent {
    name: String,
    open: PathBuf,
    at: NaiveDateTime,
}

/// What he opened lately, newest first.
fn recent_files() -> Vec<Recent> {
    #[cfg(windows)]
    let found: Vec<Recent> = {
        let folder = PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("Microsoft").join("Windows").join("Recent");
        std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = recent_name(&e.file_name().to_string_lossy())?;
                let at = chrono::DateTime::<Local>::from(e.metadata().ok()?.modified().ok()?).naive_local();
                Some(Recent { name, open: e.path(), at })
            })
            .collect()
    };
    #[cfg(not(windows))]
    let found: Vec<Recent> = crate::apps::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".local/share/recently-used.xbel")).ok())
        .map(|xbel| xbel_files(&xbel).into_iter().filter(|r| r.open.is_file()).collect())
        .unwrap_or_default();
    let mut found = found;
    found.sort_by(|a, b| b.at.cmp(&a.at));
    found
}

/// The file a shortcut in Windows' recent list stands for ("Report.docx.lnk"); None for a
/// folder's ("Downloads.lnk", and "J.A.I.lnk": seen live, its "I" no extension).
#[cfg_attr(not(windows), allow(dead_code))]
fn recent_name(link: &str) -> Option<String> {
    let name = link.strip_suffix(".lnk")?;
    let ext = Path::new(name).extension()?.to_string_lossy().into_owned();
    (ext.len() >= 2 && ext.chars().all(|c| c.is_ascii_alphanumeric())).then(|| name.to_string())
}

/// The files KDE and GTK keep in recently-used.xbel.
#[cfg_attr(windows, allow(dead_code))]
fn xbel_files(xbel: &str) -> Vec<Recent> {
    static BOOKMARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<bookmark\s[^>]*?href="(?P<h>[^"]+)"[^>]*?\smodified="(?P<m>[^"]+)""#).expect("an xbel bookmark"));
    BOOKMARK
        .captures_iter(xbel)
        .filter_map(|c| {
            let open = path_of_url(&c["h"].replace("&amp;", "&"))?;
            let at = chrono::DateTime::parse_from_rfc3339(&c["m"]).ok()?.with_timezone(&Local).naive_local();
            Some(Recent { name: name_of(&open), open, at })
        })
        .collect()
}

/// The span `when` means at `now`: from, until.
fn span(when: &str, now: NaiveDateTime) -> (NaiveDateTime, NaiveDateTime) {
    let midnight = now.date().and_time(chrono::NaiveTime::MIN);
    let at = |hours: i64| midnight + chrono::Duration::hours(hours);
    match when {
        "today" => (midnight, at(24)),
        "morning" => (midnight, at(12)),
        "afternoon" => (at(12), at(18)),
        "evening" => (at(18), at(24)),
        "yesterday" => (at(-24), midnight),
        _ => (now - chrono::Duration::days(7), at(24)),
    }
}

fn of_kind(name: &str, kind: &str) -> bool {
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let list: &[&str] = match kind {
        "document" => &["doc", "docx", "odt", "rtf", "txt", "md", "pdf", "xls", "xlsx", "ods", "csv", "ppt", "pptx", "odp"],
        "pdf" => &["pdf"],
        "presentation" => &["ppt", "pptx", "odp", "key"],
        "spreadsheet" => &["xls", "xlsx", "ods", "csv"],
        "picture" | "photo" => &PICTURES,
        _ => return true,
    };
    list.contains(&ext.as_str())
}

fn opened_in(files: Vec<Recent>, when: &str, kind: &str, now: NaiveDateTime) -> Vec<Recent> {
    let (from, until) = span(when, now);
    files.into_iter().filter(|r| r.at >= from && r.at < until && of_kind(&r.name, kind)).collect()
}

fn when_said(when: &str) -> &'static str {
    match when {
        "today" => "today",
        "morning" => "this morning",
        "afternoon" => "this afternoon",
        "evening" => "this evening",
        "yesterday" => "yesterday",
        _ => "this week",
    }
}

async fn recent(args: &Value) -> Result<String, String> {
    let (when, kind) = (args["when"].as_str().unwrap_or(""), args["kind"].as_str().unwrap_or(""));
    let now = Local::now().naive_local();
    let files = tokio::task::spawn_blocking(recent_files).await.map_err(|e| e.to_string())?;
    let found = opened_in(files, when, kind, now);
    let thing = if kind.is_empty() { "file".to_string() } else { kind.to_string() };
    let Some(newest) = found.first() else { return Err(format!("I see no {thing} you opened {}, Sir.", when_said(when))) };
    if args["open"].as_bool().unwrap_or(false) {
        if tied() {
            return Ok(not_in_a_test(&format!("Would open {}", newest.name)));
        }
        #[cfg(windows)]
        {
            let link = newest.open.display().to_string();
            crate::win32::call(move || crate::win32::shell_execute(&link, None, None)).await??;
        }
        #[cfg(not(windows))]
        crate::file_ops::open_with(&newest.open, None).await?;
        return Ok(say(format!("Opened {} (last opened {}).", newest.name, newest.at.format("%a %H:%M")), format!("Opening {}, Sir.", newest.name)));
    }
    let rows: Vec<String> = found.iter().take(16).map(|r| format!("{}  {}", r.at.format(if r.at.date() == now.date() { "%H:%M" } else { "%a %H:%M" }), r.name)).collect();
    crate::kit::show_text("recent_files", "RECENT FILES", &rows.join("\n"), [420.0, 60.0 + 22.0 * rows.len() as f32]);
    let mut names: Vec<String> = found.iter().take(3).map(|r| r.name.clone()).collect();
    if found.len() > 3 {
        names.push(format!("{} more", found.len() - 3));
    }
    let when = crate::kit_voice::capitalised(when_said(when));
    Ok(say(rows.join("\n"), format!("{when} you had {} open, Sir.", crate::briefing::join_and(&names))))
}

// ------------------------------------------------------------------------- free space ----

/// Entries of the temp folder nothing has touched for a week (by their newest file), with their
/// size. Only from a folder plainly named Temp, and never his own.
fn old_temp() -> Vec<(PathBuf, u64)> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let temp = crate::disk_map::temp_folder();
    if !temp.file_name().is_some_and(|n| n.eq_ignore_ascii_case("temp") || n.eq_ignore_ascii_case("tmp")) {
        return Vec::new();
    }
    let Some(week_ago) = SystemTime::now().checked_sub(Duration::from_secs(7 * 86_400)) else { return Vec::new() };
    let entries: Vec<PathBuf> = std::fs::read_dir(&temp).into_iter().flatten().flatten().filter(|e| !e.file_name().to_string_lossy().to_lowercase().contains("jai")).map(|e| e.path()).collect();
    // (Walked side by side: one after the other, his 1.9 GB took 37 s, seen live.)
    let chunk = entries.len().div_ceil(std::thread::available_parallelism().map_or(4, |n| n.get()).min(8)).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = entries.chunks(chunk).map(|part| s.spawn(move || part.iter().filter_map(|p| old_entry(p, week_ago)).collect::<Vec<_>>())).collect();
        parts.into_iter().flat_map(|part| part.join().unwrap_or_default()).collect()
    })
}

/// A temp entry's size, when nothing in it is younger than `week_ago` (given up at its first
/// young file, or at anything it can't read: what can't be seen is never cleared).
fn old_entry(path: &Path, week_ago: SystemTime) -> Option<(PathBuf, u64)> {
    let mut size = 0;
    for entry in walkdir::WalkDir::new(path) {
        let meta = entry.ok()?.metadata().ok()?;
        if meta.modified().ok()? >= week_ago {
            return None;
        }
        if meta.is_file() {
            size += meta.len();
        }
    }
    Some((path.to_path_buf(), size))
}

/// What "free up some space" measured, for his yes (measuring again kept him waiting twice).
static MEASURED: std::sync::Mutex<Option<(std::time::Instant, Vec<(PathBuf, u64)>)>> = std::sync::Mutex::new(None);

/// What the Recycle Bin (KDE's Trash) holds.
fn bin_size() -> u64 {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
        let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, i64Size: 0, i64NumItems: 0 };
        // SAFETY: no root (every drive's bin) and a struct carrying its own size, as the API requires.
        unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.map_or(0, |_| u64::try_from(info.i64Size).unwrap_or(0))
    }
    #[cfg(not(windows))]
    crate::disk_map::recycle_bin('/').unwrap_or(0)
}

fn empty_bin() -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND};
        // SAFETY: no window, no root (every drive's bin), plain flags.
        unsafe { SHEmptyRecycleBinW(HWND(0), PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND) }.map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let trash = crate::apps::home_dir().ok_or("no home folder")?.join(".local/share/Trash");
        for part in ["files", "info"] {
            for entry in std::fs::read_dir(trash.join(part)).into_iter().flatten().flatten() {
                let path = entry.path();
                let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            }
        }
        let _ = std::fs::remove_file(trash.join("directorysizes"));
        Ok(())
    }
}

const BIN: &str = if cfg!(windows) { "the Recycle Bin" } else { "the Trash" };

async fn free_space(args: &Value) -> Result<String, String> {
    let what = args["what"].as_str().unwrap_or("both").to_string();
    let (temp_on, bin_on) = (what != "bin" && cfg!(windows), what != "temp");
    if !temp_on && !bin_on {
        return Ok(say("Linux clears its temp files itself.", "Linux clears its temp files at each restart, Sir: there's nothing for me to do."));
    }
    let confirmed = args["confirmed"].as_bool().unwrap_or(false);
    let measured = confirmed.then(|| MEASURED.lock().unwrap_or_else(|e| e.into_inner()).take()).flatten().filter(|(at, _)| at.elapsed() < Duration::from_secs(60)).map(|(_, old)| old);
    let (old, bin) = tokio::task::spawn_blocking(move || (if temp_on { measured.unwrap_or_else(old_temp) } else { Vec::new() }, if bin_on { bin_size() } else { 0 })).await.map_err(|e| e.to_string())?;
    let temp: u64 = old.iter().map(|(_, size)| size).sum();
    if temp + bin == 0 {
        return Ok(say("Nothing to clear.", format!("There's nothing to clear, Sir: {} empty already.", if temp_on && bin_on { format!("the old temp files are gone and {BIN} is") } else if bin_on { format!("{BIN} is") } else { "the old temp files are".to_string() })));
    }
    let size = crate::disk_map::spoken_size;
    if !confirmed {
        if temp_on {
            *MEASURED.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), old));
        }
        let mut parts = Vec::new();
        if temp > 0 {
            parts.push(format!("your old temp files hold {}", size(temp)));
        }
        if bin > 0 {
            parts.push(if temp > 0 { format!("{BIN} {}", size(bin)) } else { format!("{BIN} holds {}", size(bin)) });
        }
        let order = match (temp > 0, bin > 0) {
            (true, true) => "the temp files and the recycle bin",
            (true, false) => "the temp files",
            _ if cfg!(windows) => "the recycle bin",
            _ => "the trash",
        };
        crate::hud_lines::offer(&format!("empty {order} for good"));
        let line = format!("{}, Sir. Shall I empty {} for good?", crate::kit_voice::capitalised(&crate::briefing::join_and(&parts)), if temp > 0 && bin > 0 { "them" } else { "it" });
        return Ok(say(&line, &line));
    }
    if tied() {
        return Ok(not_in_a_test(&format!("Would free {}", size(temp + bin))));
    }
    let freed = tokio::task::spawn_blocking(move || {
        let mut freed = 0;
        for (path, bytes) in old {
            if if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) }.is_ok() {
                freed += bytes;
            }
        }
        if bin > 0 && empty_bin().is_ok() {
            freed += bin;
        }
        freed
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(say(format!("Freed {}.", size(freed)), format!("Done, Sir: {} freed.", size(freed))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(said: &str) -> Value {
        let s = crate::intents::normalize_phrase(said);
        command(&s, said).unwrap_or_else(|| panic!("{said} → {s}"))
    }

    /// Sir chose them on 2026-11-22: each order in his words (English and German), and what is
    /// not one of them stays elsewhere.
    #[test]
    fn his_file_orders_are_known_by_their_words() {
        assert_eq!(order("Rename this to Exam notes."), json!({"action": "rename", "name": "Exam notes"}));
        assert_eq!(order("JAI, rename the selected file to Max's Report"), json!({"action": "rename", "name": "Max's Report"}));
        assert_eq!(order("Benenne das in Prüfungsnotizen um"), json!({"action": "rename", "name": "Prüfungsnotizen"}));
        for said in ["Zip these files", "zip it up", "compress these files", "make a zip of these", "Pack die Dateien zusammen"] {
            assert_eq!(order(said)["action"], "zip", "{said}");
        }
        for said in ["Unzip this", "extract it here", "Entpacke das"] {
            assert_eq!(order(said)["action"], "unzip", "{said}");
        }
        for said in ["Make this picture smaller", "shrink these photos", "compress this image", "Mach das Bild kleiner"] {
            assert_eq!(order(said)["action"], "shrink", "{said}");
        }
        assert_eq!(order("Convert this to JPG"), json!({"action": "convert", "to": "jpg"}));
        assert_eq!(order("save these pictures as webp"), json!({"action": "convert", "to": "webp"}));
        assert_eq!(order("Wandle das in ein PNG um"), json!({"action": "convert", "to": "png"}));
        assert_eq!(order("Print this"), json!({"action": "print", "copies": 1}));
        assert_eq!(order("print it twice"), json!({"action": "print", "copies": 2}));
        assert_eq!(order("print 3 copies of this"), json!({"action": "print", "copies": 3}));
        assert_eq!(order("Drucke das zweimal aus"), json!({"action": "print", "copies": 2}));
        for said in ["Copy the path of this", "copy the path", "copy the full path of these files", "Kopiere den Pfad"] {
            assert_eq!(order(said), json!({"action": "copy_path"}), "{said}");
        }
        assert_eq!(order("Make a new folder called Exams"), json!({"action": "new_folder", "name": "Exams"}));
        assert_eq!(order("create a new folder here"), json!({"action": "new_folder", "name": ""}));
        assert_eq!(order("Erstelle einen neuen Ordner namens Ferien"), json!({"action": "new_folder", "name": "Ferien"}));
        assert_eq!(order("Open what I just downloaded"), json!({"action": "last_download", "reveal": false}));
        assert_eq!(order("Show my last download"), json!({"action": "last_download", "reveal": true}));
        assert_eq!(order("öffne meinen letzten Download"), json!({"action": "last_download", "reveal": false}));
        assert_eq!(order("Open the document I had open yesterday"), json!({"action": "recent", "when": "yesterday", "open": true, "kind": "document"}));
        assert_eq!(order("What did I work on this morning?"), json!({"action": "recent", "when": "morning", "open": false, "kind": ""}));
        assert_eq!(order("show my recent files"), json!({"action": "recent", "when": "", "open": false, "kind": ""}));
        assert_eq!(order("Woran habe ich gestern gearbeitet?"), json!({"action": "recent", "when": "yesterday", "open": false, "kind": ""}));
        assert_eq!(order("Free up some space"), json!({"action": "free_space", "what": "both"}));
        assert_eq!(order("empty the recycle bin"), json!({"action": "free_space", "what": "bin"}));
        assert_eq!(order("clear my temp files"), json!({"action": "free_space", "what": "temp"}));
        assert_eq!(order("Leere den Papierkorb"), json!({"action": "free_space", "what": "bin"}));
        // His yes to the offer: the order it holds, confirmed.
        assert_eq!(order("empty the temp files and the recycle bin for good"), json!({"action": "free_space", "what": "both", "confirmed": true}));
        assert!(only_looks(&order("what did I work on yesterday")) && only_looks(&order("free up some space")));
        assert!(!only_looks(&order("open the file I had open today")) && !only_looks(&order("empty the recycle bin for good")));
        for other in ["make it smaller", "make this window smaller", "print screen", "show the QR code", "what was I working on", "where was I", "rename the protocol to morning", "open the file I had open with Max", "how can I free up some space", "copy that"] {
            let s = crate::intents::normalize_phrase(other);
            assert_eq!(command(&s, other), None, "{other}");
        }
    }

    /// The pieces that need no file manager: addresses, names, spans and the recent files' list.
    #[test]
    fn names_places_and_times_are_worked_out() {
        #[cfg(windows)]
        {
            assert_eq!(path_of_url("file:///C:/Users/Max%20Mustermann/Downloads"), Some(PathBuf::from("C:\\Users\\Max Mustermann\\Downloads")));
            assert_eq!(path_of_url("file://server/share/Exams"), Some(PathBuf::from("\\\\server\\share\\Exams")));
        }
        assert_eq!(path_of_url("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"), None);
        assert_eq!(new_name(Path::new("Report.pdf"), "Exam notes"), Ok("Exam notes.pdf".to_string()));
        assert_eq!(new_name(Path::new("Report.pdf"), "exam.PDF"), Ok("exam.PDF".to_string()));
        assert_eq!(new_name(Path::new("Makefile"), "Rules"), Ok("Rules".to_string()));
        assert!(new_name(Path::new("a.txt"), "a/b").is_err() && new_name(Path::new("a.txt"), "..").is_err() && new_name(Path::new("a.txt"), " ").is_err());
        assert_eq!(copies("twice"), 2);
        assert_eq!(copies("in 4 copies"), 4);
        assert_eq!(copies("99 times"), 10);
        assert_eq!(recent_name("Report.docx.lnk").as_deref(), Some("Report.docx"));
        assert_eq!(recent_name("JAI Plans.txt.lnk").as_deref(), Some("JAI Plans.txt"));
        for folder in ["Downloads.lnk", "J.A.I.lnk", "Notes.txt"] {
            assert_eq!(recent_name(folder), None, "{folder}");
        }
        assert!(finished_download("Setup.exe") && !finished_download("Setup.exe.crdownload") && !finished_download("desktop.ini"));
        let zip = zip_args(Path::new("out.zip"), &[PathBuf::from("dir/a.txt"), PathBuf::from("other/b")]);
        assert_eq!(zip, ["-a", "-c", "-f", "out.zip", "-C", "dir", "a.txt", "-C", "other", "b"].map(OsString::from));

        let xbel = r#"<xbel><bookmark href="file:///home/max/Exam%20notes.odt" added="2026-11-21T08:00:00Z" modified="2026-11-22T08:30:00Z" visited="2026-11-22T08:30:00Z"></bookmark><bookmark href="file:///home/max/photo.png" added="2026-11-20T08:00:00Z" modified="2026-11-21T15:00:00Z" visited="x"></bookmark></xbel>"#;
        let files = xbel_files(xbel);
        assert_eq!(files.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["Exam notes.odt", "photo.png"]);
        let at = |d: u32, h: u32| chrono::NaiveDate::from_ymd_opt(2026, 11, d).unwrap().and_hms_opt(h, 0, 0).unwrap();
        let mine = |name: &str, at: NaiveDateTime| Recent { name: name.to_string(), open: PathBuf::from(name), at };
        let list = vec![mine("Exam.docx", at(22, 9)), mine("Holiday.png", at(22, 14)), mine("Old.pdf", at(21, 20)), mine("Ancient.txt", at(10, 9))];
        let now = at(22, 16);
        let names = |when: &str, kind: &str| opened_in(list.clone(), when, kind, now).into_iter().map(|r| r.name).collect::<Vec<_>>();
        assert_eq!(names("morning", ""), ["Exam.docx"]);
        assert_eq!(names("today", ""), ["Exam.docx", "Holiday.png"]);
        assert_eq!(names("yesterday", "document"), ["Old.pdf"]);
        assert_eq!(names("", "picture"), ["Holiday.png"]);
        assert_eq!(names("", ""), ["Exam.docx", "Holiday.png", "Old.pdf"]);
    }

    /// A smaller copy and a converted one land beside the original; a picture with see-through
    /// parts stays a PNG.
    #[test]
    fn a_picture_is_shrunk_and_converted_beside_itself() {
        let dir = std::env::temp_dir().join(format!("kit_files_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("Photo.png");
        image::RgbImage::from_fn(400, 200, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])).save(&photo).unwrap();
        let small = picture_copy(&photo, None).unwrap();
        assert_eq!(name_of(&small), "Photo (small).jpg");
        assert_eq!(image::image_dimensions(&small).unwrap(), (200, 100));
        assert_eq!(name_of(&picture_copy(&photo, None).unwrap()), "Photo (small) (2).jpg", "never over an earlier copy");
        let webp = picture_copy(&photo, Some("webp")).unwrap();
        assert_eq!((name_of(&webp), image::image_dimensions(&webp).unwrap()), ("Photo.webp".to_string(), (400, 200)));
        let clear = dir.join("Logo.png");
        image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 212, 0, 0])).save(&clear).unwrap();
        assert_eq!(name_of(&picture_copy(&clear, None).unwrap()), "Logo (small).png");
        assert!(picture_copy(&dir.join("missing.png"), None).is_err());
        // A temp entry counts as old only when nothing in it is younger than the line.
        let later = SystemTime::now() + Duration::from_secs(3600);
        let total: u64 = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.metadata().unwrap().len()).sum();
        assert_eq!(old_entry(&dir, later), Some((dir.clone(), total)));
        assert_eq!(old_entry(&dir, SystemTime::UNIX_EPOCH + Duration::from_secs(1)), None, "a young file keeps the whole folder");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
