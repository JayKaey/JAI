//! Games (2026-11-15), phrase-only behind the kit's door (`kit::command`, tool `games`): "start
//! Batman" (his Steam and Epic libraries), "what games do I have", play time, "remind me after
//! the game to …", per-game routines ("when GTA starts, set the volume to 40"), a game's Steam
//! news (and a notice at its start when there is something new), Epic's free games, a Steam
//! download he waits for, "what's using my VRAM" and the school-night nudge.
//!
//! The VRAM guard says when a game starts and ends (`on_change`) and calls `poll` every few
//! seconds; what is due then waits in two queues the app empties (`due_alerts`, `due_orders`).

use crate::kit::say;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("games regex")
}

static ALERTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ORDERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// What the games kit put on screen itself (its news, nudge, finished downloads): never "came in".
static OWN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Lines Sir must hear now (an after-game reminder, the school-night nudge, a finished download).
pub fn due_alerts() -> Vec<String> {
    std::mem::take(&mut *lock(&ALERTS))
}

/// Orders to carry out now as if he had said them (a game's routine).
pub fn due_orders() -> Vec<String> {
    std::mem::take(&mut *lock(&ORDERS))
}

fn alert(line: String) {
    lock(&OWN).push(line.clone());
    lock(&ALERTS).push(line);
}

// --------------------------------------------------------------------------- the library ----

#[derive(Clone, Debug, PartialEq)]
pub struct Game {
    pub name: String,
    /// What starts it (a `steam://` or `com.epicgames.launcher://` link).
    pub launch: String,
    /// Steam's app id and the manifest that says how far its download is.
    pub appid: Option<String>,
    pub manifest: Option<PathBuf>,
    /// The install folder's name (the VRAM guard's title for the running game).
    pub folder: String,
}

/// The values of `key` in a Valve KeyValues file (manifests, the library list).
fn vdf_values(text: &str, key: &str) -> Vec<String> {
    static PAIR: LazyLock<Regex> = LazyLock::new(|| re(r#""([^"]+)"\s+"((?:[^"\\]|\\.)*)""#));
    PAIR.captures_iter(text).filter(|c| c[1].eq_ignore_ascii_case(key)).map(|c| c[2].replace("\\\\", "\\")).collect()
}

fn vdf_value(text: &str, key: &str) -> Option<String> {
    vdf_values(text, key).into_iter().next()
}

/// Runtimes, tools and wallpapers Steam lists among its apps.
fn not_a_game(name: &str) -> bool {
    let n = name.to_lowercase();
    ["redistributable", "proton", "steam linux runtime", "steamvr", "wallpaper engine", "steamworks", "dedicated server", " sdk", "soundtrack"].iter().any(|w| n.contains(w))
}

#[cfg(windows)]
fn steam_path_from_registry() -> Option<PathBuf> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut buf = [0u16; 520];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: reads one string value into a buffer of the size passed.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Valve\\Steam"), w!("SteamPath"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) }.ok().ok()?;
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

#[cfg(not(windows))]
fn steam_path_from_registry() -> Option<PathBuf> {
    None
}

/// Every Steam library's `steamapps` folder.
fn steam_libraries() -> Vec<PathBuf> {
    let Some(root) = steam_path_from_registry().or_else(|| Some(PathBuf::from(r"C:\Program Files (x86)\Steam"))).filter(|p| p.join("steamapps").is_dir()) else {
        return Vec::new();
    };
    let mut libs = vec![root.join("steamapps")];
    if let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) {
        for path in vdf_values(&text, "path") {
            let lib = PathBuf::from(path).join("steamapps");
            let key = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
            if lib.is_dir() && !libs.iter().any(|l| key(l) == key(&lib)) {
                libs.push(lib);
            }
        }
    }
    libs
}

fn steam_game(manifest: &Path) -> Option<Game> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let (appid, name) = (vdf_value(&text, "appid")?, vdf_value(&text, "name")?);
    (!not_a_game(&name)).then(|| Game {
        launch: format!("steam://rungameid/{appid}"),
        folder: vdf_value(&text, "installdir").unwrap_or_else(|| name.clone()),
        name,
        appid: Some(appid),
        manifest: Some(manifest.to_path_buf()),
    })
}

fn epic_game(item: &Value) -> Option<Game> {
    let name = item["DisplayName"].as_str()?.to_string();
    let (ns, id, app) = (item["CatalogNamespace"].as_str()?, item["CatalogItemId"].as_str()?, item["AppName"].as_str()?);
    let folder = Path::new(item["InstallLocation"].as_str().unwrap_or("")).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone());
    Some(Game { launch: format!("com.epicgames.launcher://apps/{ns}%3A{id}%3A{app}?action=launch&silent=true"), name, appid: None, manifest: None, folder })
}

/// His installed games, by name.
pub fn library() -> Vec<Game> {
    if cfg!(test) {
        return test_library();
    }
    // The games kit looks at every sentence first: the disk is read once a minute at most.
    static LIBRARY: Mutex<Option<(std::time::Instant, Vec<Game>)>> = Mutex::new(None);
    if let Some((at, games)) = &*lock(&LIBRARY) {
        if at.elapsed() < std::time::Duration::from_secs(60) {
            return games.clone();
        }
    }
    let files = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect::<Vec<_>>();
    let mut games: Vec<Game> = steam_libraries()
        .iter()
        .flat_map(|lib| files(lib))
        .filter(|p| p.file_name().is_some_and(|f| f.to_string_lossy().starts_with("appmanifest_")))
        .filter_map(|p| steam_game(&p))
        .collect();
    let epic = files(Path::new(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests"));
    games.extend(epic.iter().filter(|p| p.extension().is_some_and(|x| x == "item")).filter_map(|p| epic_game(&serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?)));
    games.sort_by_key(|g| g.name.to_lowercase());
    *lock(&LIBRARY) = Some((std::time::Instant::now(), games.clone()));
    games
}

#[cfg(test)]
fn test_library() -> Vec<Game> {
    let steam = |appid: &str, name: &str| Game { name: name.into(), launch: format!("steam://rungameid/{appid}"), appid: Some(appid.into()), manifest: None, folder: name.into() };
    vec![steam("208650", "Batman™: Arkham Knight"), steam("271590", "Grand Theft Auto V"), steam("12210", "Grand Theft Auto IV: The Complete Edition"), steam("900001", "It Takes Example"), steam("900002", "The Example Game"), steam("900003", "One Example")]
}

#[cfg(not(test))]
fn test_library() -> Vec<Game> {
    Vec::new()
}

fn words(s: &str) -> Vec<String> {
    let numerals = |w: &str| match w {
        "1" => "i".to_string(),
        "2" => "ii".to_string(),
        "3" => "iii".to_string(),
        "4" => "iv".to_string(),
        "5" | "five" => "v".to_string(),
        _ => w.to_string(),
    };
    s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>().split_whitespace().map(numerals).collect()
}

/// Whether Sir's words name this game: each of his words in its name ("batman", "arkham
/// knight") or its initials ("gta", "gta 5" for Grand Theft Auto V).
pub fn names(query: &str, game: &str) -> bool {
    // Filler alone names no game ("start it", "start the game", "start one").
    const FILLER: [&str; 10] = ["it", "this", "that", "the", "a", "an", "my", "game", "games", "one"];
    let (q, g) = (words(query), words(game));
    if q.iter().all(|w| FILLER.contains(&w.as_str())) {
        return false;
    }
    if q.iter().all(|w| g.contains(w)) {
        return true;
    }
    let initials: String = g.iter().filter_map(|w| w.chars().next()).collect();
    let said = q.concat();
    said.len() >= 3 && initials.starts_with(&said)
}

/// The game he means: the shortest name that fits ("gta" is V, not IV: The Complete Edition).
pub fn find(query: &str) -> Option<Game> {
    library().into_iter().filter(|g| names(query, &g.name) || names(query, &g.folder)).min_by_key(|g| g.name.len())
}

// ---------------------------------------------------------------------------- his words ----

static START: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:please\s+)?(?:start|launch|boot up|fire up)\s+(?:the\s+game\s+|my\s+)?(.+?)(?:\s+for me)?(?:\s+please)?$"));
static LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:what|which)\s+games\s+(?:do i have|have i got|are installed|did i install)|^(?:list|show)\s+(?:me\s+)?(?:all\s+)?my\s+games$"));
static PLAYTIME: LazyLock<Regex> = LazyLock::new(|| re(r"^how long (?:have|did) i (?:been )?(?:play(?:ed|ing)?|gam(?:ed|ing))(?: (?P<week>this week)| today)?$|^(?:my )?play ?time(?: today| (?P<week2>this week))?$"));
static AFTER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:remind me|tell me)\s+(?:after|when)\s+(?:the|my|this)\s+game(?:\s+is\s+over|\s+ends|\s+is\s+done)?\s+(?:to\s+|that\s+|about\s+)?(?P<a>.+)$|^after\s+(?:the|my|this)\s+game\s+remind me\s+(?:to\s+|that\s+|about\s+)?(?P<b>.+)$")
});
static ROUTINE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:when|whenever|every time)\s+(?:i\s+(?:start|launch|play|open)\s+(?P<g1>.+?)|(?P<g2>.+?)\s+(?:starts|launches|opens|begins))\s*,?\s+(?:then\s+)?(?P<order>(?:set|open|play|turn|start|mute|put|close|switch|change|launch|lower|raise|make|pause)\b.*)$")
});
static ROUTINE_LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:what are|list|show me)\s+(?:my\s+)?game routines$"));
static ROUTINE_FORGET: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:forget|delete|remove|drop)\s+(?:the\s+)?(?:routine\s+for\s+(?P<a>.+)|(?P<b>.+?)\s+routine)$"));
static NEWS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:any|is there any|are there any|what's the|whats the|read me the|show me the)?\s*(?:news|updates?|patch notes)\s+(?:for|about|on)\s+(?P<a>.+)$|^(?:is there|was there)\s+(?:an?\s+)?(?:new\s+)?(?:update|patch)\s+(?:for|to)\s+(?P<b>.+)$|^(?P<c>.+?)\s+patch notes$")
});
static FREE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:any|are there any|what are the|show me the|which)?\s*free games(?:\s+(?:this week|on epic|today|right now|are there))?$|^what(?:'s| is) free on epic(?: this week)?$"));
/// "Anything on my wishlist on sale?", "ist was auf meiner Wunschliste im Angebot?"
static WISHLIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:is\s+)?(?:anything|something|what(?:'s| is)?|which games?)\s+(?:on|from)\s+my\s+(?:steam\s+)?wishlist\s+(?:is\s+)?(?:on sale|discounted|cheaper|reduced)(?:\s+(?:right now|today|now))?$|^(?:steam\s+)?wishlist\s+(?:sales?|deals)$|^(?:ist\s+)?(?:was|etwas|irgendwas)\s+(?:auf|von)\s+meiner\s+(?:steam\s+)?wunschliste\s+(?:im angebot|reduziert|günstiger)$"));
static VRAM: LazyLock<Regex> =LazyLock::new(|| re(r"^(?:what|who)(?:'s|s| is)\s+(?:using|eating|taking|filling|hogging)\s+(?:up\s+)?(?:all\s+)?(?:of\s+)?(?:my\s+|the\s+)?(?:vram|video memory|gpu memory|graphics memory)$|^vram report$"));
static DOWNLOAD_WATCH: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:tell me|let me know|notify me|say)\s+when\s+(?:the\s+|my\s+)?(?P<g>.+?)\s+download\s+(?:is\s+)?(?:done|finished|finishes|complete|completes|is complete)$"));
static DOWNLOAD_ASK: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:how far is|how's|how is)\s+(?:the|my)\s+(?:(?P<g>.+?)\s+)?download$|^is\s+(?:the|my)\s+(?:(?P<h>.+?)\s+)?download\s+(?:done|finished)$"));
static BEDTIME: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:remind me to |tell me to )?stop (?:gaming|playing) at (?P<t>.+?) on school nights$"));
static UNPLAYED: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:which|what)\s+games\s+(?:(?:havent|haven\s+t|haven't|have\s+not|didnt|didn\s+t|didn't|did\s+not)\s+i|(?:have|did)\s+i\s+not)\s+(?:played|play|touched|opened)(?:\s+(?:in|for)\s+(?:months|a\s+while|ages|a\s+long\s+time|a\s+year))?$|^(?:show|list)\s+(?:me\s+)?(?:my\s+)?(?:unplayed|forgotten|untouched)\s+games$|^welche\s+spiele\s+(?:habe|hab)\s+ich\s+(?:lange|seit\s+monaten|ewig|nie)\s+nicht\s+(?:mehr\s+)?gespielt$")
});
static BEDTIME_OFF: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:turn off|stop|cancel|no more|switch off)\s+the school night (?:reminder|nudge)$"));

/// "22:00", "10", "10 pm", "half past ten" is not understood: a school-night hour (an hour
/// under 12 without "am" is the evening). Only 17:00-23:59 is one: noon, midnight and the
/// small hours are none.
fn evening_time(t: &str) -> Option<String> {
    let c = re(r"^(\d{1,2})(?:[:.](\d{2}))?\s*(am|pm|uhr)?$").captures(t.trim())?;
    let (mut h, m): (u32, u32) = (c[1].parse().ok()?, c.get(2).map_or(Some(0), |m| m.as_str().parse().ok())?);
    if h < 12 && c.get(3).map(|x| x.as_str()) != Some("am") {
        h += 12;
    }
    ((17..24).contains(&h) && m < 60).then(|| format!("{h:02}:{m:02}"))
}

/// An order of the games kit in Sir's words.
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().to_lowercase();
    let s = s.trim_end_matches(['?', '.', '!']);
    let cap = |c: &regex::Captures, names: &[&str]| names.iter().find_map(|n| c.name(n)).map(|m| m.as_str().trim().to_string());
    if LIST.is_match(s) {
        return Some(json!({"action": "list"}));
    }
    if let Some(c) = PLAYTIME.captures(s) {
        let week = c.name("week").or(c.name("week2")).is_some();
        return Some(json!({"action": "playtime", "span": if week { "week" } else { "today" }}));
    }
    if let Some(c) = AFTER.captures(s) {
        return Some(json!({"action": "after", "text": cap(&c, &["a", "b"])?}));
    }
    if ROUTINE_LIST.is_match(s) {
        return Some(json!({"action": "routines"}));
    }
    if let Some(c) = ROUTINE.captures(s) {
        let game = find(&cap(&c, &["g1", "g2"])?)?;
        return Some(json!({"action": "routine", "game": game.folder, "order": cap(&c, &["order"])?}));
    }
    if let Some(c) = ROUTINE_FORGET.captures(s) {
        let game = find(&cap(&c, &["a", "b"])?)?;
        return Some(json!({"action": "forget_routine", "game": game.folder}));
    }
    if let Some(c) = NEWS.captures(s) {
        let game = find(&cap(&c, &["a", "b", "c"])?)?;
        return Some(json!({"action": "news", "game": game.name}));
    }
    if FREE.is_match(s) {
        return Some(json!({"action": "free"}));
    }
    if WISHLIST.is_match(s) {
        return Some(json!({"action": "wishlist"}));
    }
    if VRAM.is_match(s) {
        return Some(json!({"action": "vram"}));
    }
    if let Some(c) = DOWNLOAD_WATCH.captures(s) {
        return Some(json!({"action": "watch_download", "game": cap(&c, &["g"])?}));
    }
    if let Some(c) = DOWNLOAD_ASK.captures(s) {
        return Some(json!({"action": "download", "game": cap(&c, &["g", "h"]).unwrap_or_default()}));
    }
    if let Some(c) = BEDTIME.captures(s) {
        return Some(json!({"action": "bedtime", "at": evening_time(&cap(&c, &["t"])?)?}));
    }
    if BEDTIME_OFF.is_match(s) {
        return Some(json!({"action": "bedtime", "at": ""}));
    }
    if UNPLAYED.is_match(s) {
        return Some(json!({"action": "unplayed"}));
    }
    // Last: "start X" only where X is one of his games ("start the timer" is not).
    let game = find(START.captures(s)?.get(1)?.as_str())?;
    Some(json!({"action": "start", "game": game.name}))
}

// --------------------------------------------------------------------- carrying it out ----

fn http() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().timeout(std::time::Duration::from_secs(8)).build().unwrap_or_default()).clone()
}

pub async fn run(args: &Value) -> Result<String, String> {
    let game = args["game"].as_str().unwrap_or("");
    match args["action"].as_str().unwrap_or("") {
        "start" => {
            let g = find(game).ok_or_else(|| format!("I can't find {game} among your games, Sir."))?;
            if !crate::platform::hands_off() {
                crate::win32::shell_execute(&g.launch, None, None)?;
            }
            Ok(say(format!("Starting {}.", g.name), format!("Starting {}, Sir.", plain(&g.name))))
        }
        "list" => {
            let games = library();
            if games.is_empty() {
                return Ok(say("No games found in Steam or Epic.", "I can't find any installed games, Sir."));
            }
            let names: Vec<String> = games.iter().map(|g| plain(&g.name)).collect();
            crate::kit::show_text("games_library", "GAMES // INSTALLED", &names.join("\n"), [360.0, 420.0]);
            Ok(say(format!("{} games: {}", names.len(), names.join(", ")), format!("You have {} games installed, Sir; they're on the board.", names.len())))
        }
        "playtime" => Ok(playtime_answer(args["span"].as_str() == Some("week"), chrono::Local::now())),
        "unplayed" => {
            let rows: Vec<(String, u64, u64)> = steam_libraries()
                .iter()
                .flat_map(|lib| std::fs::read_dir(lib).into_iter().flatten().flatten().map(|e| e.path()))
                .filter(|p| p.file_name().is_some_and(|f| f.to_string_lossy().starts_with("appmanifest_")))
                .filter_map(|p| std::fs::read_to_string(p).ok())
                .filter_map(|text| {
                    let num = |k: &str| vdf_value(&text, k).and_then(|v| v.parse().ok()).unwrap_or(0u64);
                    let name = vdf_value(&text, "name")?;
                    (!not_a_game(&name)).then(|| (name, num("LastPlayed"), num("SizeOnDisk")))
                })
                .collect();
            let (text, spoken) = unplayed_answer(&rows, chrono::Local::now().timestamp().max(0) as u64);
            if !text.is_empty() {
                crate::kit::show_text("games_unplayed", "GAMES // NOT PLAYED IN 3 MONTHS", &text, [420.0, 380.0]);
            }
            Ok(say(if text.is_empty() { spoken.clone() } else { text }, spoken))
        }
        "after" => {
            let text = args["text"].as_str().unwrap_or("").trim().to_string();
            let mut list: Vec<Value> = crate::kit::get("games", "after").and_then(|v| v.as_array().cloned()).unwrap_or_default();
            list.push(json!(text));
            crate::kit::put("games", "after", &json!(list))?;
            let when = if lock(&SESSION).is_some() { "when this game is over" } else { "after your next game" };
            Ok(say(format!("After the game: {text}"), format!("Very good, Sir; I'll remind you {when}.")))
        }
        "routine" => {
            let order = args["order"].as_str().unwrap_or("").trim();
            crate::kit::put("game_routine", game, &json!(order))?;
            Ok(say(format!("When {game} starts: {order}"), format!("Done, Sir: whenever {} starts, I'll {}.", plain(game), order)))
        }
        "forget_routine" => Ok(if crate::kit::remove("game_routine", game) {
            say(format!("Routine for {game} removed."), format!("The {} routine is gone, Sir.", plain(game)))
        } else {
            say(format!("No routine for {game}."), format!("There was no routine for {}, Sir.", plain(game)))
        }),
        "routines" => {
            let all = crate::kit::list("game_routine");
            if all.is_empty() {
                return Ok(say("No game routines.", "You have no game routines yet, Sir."));
            }
            let lines: Vec<String> = all.iter().map(|(g, order, _)| format!("{}: {}", plain(g), order.as_str().unwrap_or(""))).collect();
            Ok(say(lines.join("\n"), format!("{}, Sir.", lines.join("; "))))
        }
        "news" => news(game).await,
        "free" => free_games().await,
        "wishlist" => wishlist_answer().await,
        "vram" => Ok(vram_report()),
        "watch_download" => {
            let g = find(game).filter(|g| g.manifest.is_some()).ok_or_else(|| format!("I can't find a Steam download of {game}, Sir."))?;
            lock(&WATCHED).retain(|w| w.name != g.name);
            lock(&WATCHED).push(g.clone());
            Ok(say(format!("Watching the download of {}.", g.name), format!("I'll tell you when {} has finished downloading, Sir.", plain(&g.name))))
        }
        "download" => Ok(download_answer(game)),
        "bedtime" => {
            let at = args["at"].as_str().unwrap_or("");
            crate::kit::put("games", "bedtime", &json!(at))?;
            Ok(if at.is_empty() {
                say("School-night reminder off.", "The school-night reminder is off, Sir.")
            } else {
                say(format!("School nights: stop gaming at {at}."), format!("Very good, Sir: on school nights I'll tell you at {at}."))
            })
        }
        other => Err(format!("The games kit has no action '{other}'.")),
    }
}

/// A name as it is said: without ™ and ®.
fn plain(name: &str) -> String {
    name.replace(['™', '®', '©'], "")
}

// -------------------------------------------------------------- when games start and end ----

/// The game running now and when it started (for play time and "welcome back").
static SESSION: Mutex<Option<(String, chrono::DateTime<chrono::Local>)>> = Mutex::new(None);
/// The game that closed and when: its session ends in `poll` 30 s later, unless it starts again
/// (a launcher handing over to its game).
static LEFT: Mutex<Option<(String, chrono::DateTime<chrono::Local>)>> = Mutex::new(None);
/// The day the school-night nudge was last given.
static NUDGED: Mutex<Option<chrono::NaiveDate>> = Mutex::new(None);
/// Steam downloads Sir is waiting for.
static WATCHED: Mutex<Vec<Game>> = Mutex::new(Vec::new());

/// The VRAM guard saw a game start or end.
pub fn on_change(was: Option<String>, now: Option<String>) {
    let at = chrono::Local::now();
    if let Some(game) = was {
        *lock(&LEFT) = Some((game, at));
    }
    if let Some(game) = now {
        let left = lock(&LEFT).take();
        match left {
            // Back within the grace: the same session goes on, its routine has run.
            Some((g, _)) if g == game => return,
            Some((g, t)) => finish(&g, t),
            None => {}
        }
        *lock(&SESSION) = Some((game.clone(), at));
        // His routine for this game, as if he had just said it.
        for (key, order, _) in crate::kit::list("game_routine") {
            if (key == game || names(&key, &game)) && order.as_str().is_some_and(|o| !o.is_empty()) {
                lock(&ORDERS).push(order.as_str().unwrap_or_default().to_string());
            }
        }
        if !crate::platform::hands_off() {
            tokio::spawn(news_at_start(game));
            // (A pending restart could cut the game short: told once, as it starts.)
            if let Some(line) = crate::kit_pc::restart_note_for_game() {
                crate::announcements::say(line);
            }
            // The memory the game wants, held by programs open beside it: named once, the
            // biggest closed after his yes.
            tokio::spawn(async {
                let list = tokio::task::spawn_blocking(crate::diagnostics::top_processes).await.unwrap_or_default();
                if let Some((line, order)) = hogs_line(&hogs(&list)) {
                    crate::hud_lines::offer(&order);
                    crate::announcements::say(line);
                }
            });
        }
    }
}

/// Programs that are safe to close for a game (never his players, Discord, a launcher, the game
/// itself or Windows), with the name to say.
const CLOSABLE: &[(&str, &str)] = &[
    ("chrome", "Chrome"), ("msedge", "Edge"), ("firefox", "Firefox"), ("opera", "Opera"), ("brave", "Brave"),
    ("code", "VS Code"), ("devenv", "Visual Studio"), ("idea64", "IntelliJ"), ("rider64", "Rider"), ("blender", "Blender"),
    ("photoshop", "Photoshop"), ("afterfx", "After Effects"), ("resolve", "DaVinci Resolve"), ("teams", "Teams"), ("ms-teams", "Teams"),
    ("slack", "Slack"), ("obsidian", "Obsidian"), ("notion", "Notion"), ("vmware-vmx", "VMware"), ("virtualboxvm", "VirtualBox"),
];
/// Below this a program is not worth closing for a game.
const HOG_MB: f64 = 800.0;

/// The closable programs holding the most memory: (name to say, process, MB), up to three.
pub fn hogs(list: &[crate::diagnostics::ProcessUsage]) -> Vec<(&'static str, String, f64)> {
    let mut out: Vec<(&'static str, String, f64)> = list
        .iter()
        .filter(|p| p.memory_mb >= HOG_MB)
        .filter_map(|p| CLOSABLE.iter().find(|(stem, _)| p.name.eq_ignore_ascii_case(stem)).map(|(_, said)| (*said, p.name.to_lowercase(), p.memory_mb)))
        .collect();
    out.sort_by(|a, b| b.2.total_cmp(&a.2));
    out.truncate(3);
    out
}

/// "Chrome holds 3.2 GB and VS Code 1.8 GB beside the game, Sir. Shall I close Chrome?" and the
/// order his yes runs.
pub fn hogs_line(hogs: &[(&str, String, f64)]) -> Option<(String, String)> {
    let (said, process, _) = hogs.first()?;
    let parts: Vec<String> = hogs.iter().enumerate().map(|(i, (n, _, mb))| format!("{n}{} {:.1} GB", if i == 0 { " holds" } else { "" }, mb / 1024.0)).collect();
    Some((format!("{} beside the game, Sir. Shall I close {said}?", crate::briefing::join_and(&parts)), format!("close {process}")))
}

fn finish(game: &str, at: chrono::DateTime<chrono::Local>) {
    let Some((title, since)) = lock(&SESSION).take() else { return };
    let minutes = (at - since).num_seconds().max(0) as f64 / 60.0;
    // shortcut: a session past midnight counts on the day it began; split it if he asks for exact days.
    let key = format!("{}|{title}", since.format("%Y-%m-%d"));
    let before = crate::kit::get("playtime", &key).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let _ = crate::kit::put("playtime", &key, &json!(before + minutes));
    tracing::info!("[GAME] {game}: {minutes:.0} min played");
    let after: Vec<String> = crate::kit::get("games", "after").and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    if !after.is_empty() {
        let _ = crate::kit::remove("games", "after");
        alert(format!("The game is over, Sir. You asked me to remind you: {}.", after.join("; and ")));
    }
    // Welcome back: what came in while he played.
    let came = came_in(&crate::hud_notify::recent(at), since..=at, &std::mem::take(&mut *lock(&OWN)));
    if came > 0 {
        alert(format!("Welcome back, Sir. {came} thing{} came in while you played; say \"what did I miss\" to hear {}.", if came == 1 { "" } else { "s" }, if came == 1 { "it" } else { "them" }));
    }
}

/// Notes that came in while he played: not the kit's own lines, not a routine's "As planned: …"
/// (app.rs `run_put_off`).
fn came_in(notes: &[crate::hud_notify::Note], played: std::ops::RangeInclusive<chrono::DateTime<chrono::Local>>, own: &[String]) -> usize {
    // shortcut: J.A.I.'s other notices (a Discord call, a backup) still count; carry their origin into hud_notify::record if he minds.
    notes.iter().filter(|n| played.contains(&n.at) && !own.contains(&n.text) && !n.text.starts_with("As planned: ")).count()
}

/// A game gone 30 s is over: its session ends at the moment it closed.
fn finish_left(now: chrono::DateTime<chrono::Local>) {
    // shortcut: a session open when J.A.I. quits (game running, or closed under 30 s ago) is not counted; finish it on quit if he minds.
    let over = lock(&LEFT).take_if(|(_, t)| now - *t >= chrono::Duration::seconds(30));
    if let Some((game, t)) = over {
        finish(&game, t);
    }
}

/// Every few seconds from the guard: the end of a closed game's session, the school-night nudge
/// and the downloads he waits for.
pub fn poll() {
    let now = chrono::Local::now();
    finish_left(now);
    let free_tomorrow = crate::webuntis_client::known_holiday(now.date_naive() + chrono::Duration::days(1)).is_some();
    if let Some(line) = nudge_due(now, crate::vram_guard::game_now().is_some() && !free_tomorrow, crate::kit::get("games", "bedtime").and_then(|v| v.as_str().map(str::to_string)), &mut lock(&NUDGED)) {
        alert(line);
    }
    let mut watched = lock(&WATCHED);
    watched.retain(|g| {
        let done = g.manifest.as_deref().and_then(download_state).is_some_and(|(have, all, installed)| installed && have >= all);
        if done {
            alert(format!("{} has finished downloading, Sir.", plain(&g.name)));
        }
        !done
    });
}

/// The school-night nudge: once, at or after his time on a Sunday-to-Thursday evening, while a
/// game runs.
fn nudge_due(now: chrono::DateTime<chrono::Local>, playing: bool, at: Option<String>, last: &mut Option<chrono::NaiveDate>) -> Option<String> {
    use chrono::{Datelike, Weekday};
    // Evening times only: an older build kept "12:00" or "01:00" too.
    let at = at.filter(|a| a.as_str() >= "17:00")?;
    let school_night = matches!(now.weekday(), Weekday::Sun | Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu);
    let due = now.format("%H:%M").to_string() >= at;
    if !playing || !school_night || !due || *last == Some(now.date_naive()) {
        return None;
    }
    *last = Some(now.date_naive());
    Some(format!("It's {}, Sir: school tomorrow. Time to wrap up the game.", now.format("%H:%M")))
}

/// Steam games not started for 90 days (or never), biggest first: (the board list, one line
/// to say). `rows`: (name, LastPlayed in Unix seconds, 0 = never; SizeOnDisk in bytes).
fn unplayed_answer(rows: &[(String, u64, u64)], now: u64) -> (String, String) {
    const STALE: u64 = 90 * 86_400;
    let mut stale: Vec<&(String, u64, u64)> = rows.iter().filter(|(_, last, _)| now.saturating_sub(*last) >= STALE).collect();
    stale.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    if stale.is_empty() {
        return (String::new(), "You've played every Steam game of yours in the last three months, Sir.".to_string());
    }
    let gb = |bytes: u64| bytes as f64 / 1e9;
    let when = |last: u64| match last {
        0 => "never".to_string(),
        _ => chrono::DateTime::from_timestamp(last as i64, 0).map(|t| t.with_timezone(&chrono::Local).format("%b %Y").to_string()).unwrap_or_default(),
    };
    let lines: Vec<String> = stale.iter().map(|(name, last, size)| format!("{:>6.1} GB   {:<9} {}", gb(*size), when(*last), plain(name))).collect();
    let total: u64 = stale.iter().map(|r| r.2).sum();
    let spoken = format!(
        "{} game{} you haven't played in three months, Sir, {:.0} gigabytes together. The biggest is {}.",
        stale.len(),
        if stale.len() == 1 { "" } else { "s" },
        gb(total),
        plain(&stale[0].0)
    );
    (lines.join("\n"), spoken)
}

fn playtime_answer(week: bool, now: chrono::DateTime<chrono::Local>) -> String {
    let days = if week { 7 } else { 1 };
    let since = (now - chrono::Duration::days(days - 1)).format("%Y-%m-%d").to_string();
    let mut per: Vec<(String, f64)> = Vec::new();
    for (key, minutes, _) in crate::kit::list("playtime") {
        let Some((day, title)) = key.split_once('|') else { continue };
        if day >= since.as_str() {
            match per.iter_mut().find(|(t, _)| t == title) {
                Some((_, m)) => *m += minutes.as_f64().unwrap_or(0.0),
                None => per.push((title.to_string(), minutes.as_f64().unwrap_or(0.0))),
            }
        }
    }
    // The game running now counts as far as it has got.
    if let Some((title, start)) = lock(&SESSION).clone() {
        let m = (now - start).num_seconds().max(0) as f64 / 60.0;
        match per.iter_mut().find(|(t, _)| *t == title) {
            Some((_, have)) => *have += m,
            None => per.push((title, m)),
        }
    }
    let span = if week { "this week" } else { "today" };
    let total: f64 = per.iter().map(|(_, m)| m).sum();
    if total < 1.0 {
        return say(format!("No play time {span}."), format!("You haven't played {span}, Sir."));
    }
    per.sort_by(|a, b| b.1.total_cmp(&a.1));
    let parts: Vec<String> = per.iter().map(|(t, m)| format!("{} {}", plain(t), spoken_minutes(*m))).collect();
    say(format!("{span}: {}", parts.join(", ")), format!("{} {span}, Sir: {}.", spoken_minutes(total), parts.join(", ")))
}

fn spoken_minutes(m: f64) -> String {
    let m = m.round() as i64;
    match (m / 60, m % 60) {
        (0, m) => format!("{m} minute{}", if m == 1 { "" } else { "s" }),
        (h, 0) => format!("{h} hour{}", if h == 1 { "" } else { "s" }),
        (h, m) => format!("{h} hour{} {m} minute{}", if h == 1 { "" } else { "s" }, if m == 1 { "" } else { "s" }),
    }
}

// ------------------------------------------------------------------------- downloads ----

/// A web question about the game being played, one with a game word (a boss, a mission, a
/// trophy …): (the query with the game's name, the game).
pub fn with_game(query: &str) -> Option<(String, String)> {
    let (game, _) = crate::vram_guard::game_now()?;
    game_query(query, &game).map(|q| (q, plain(&game)))
}

fn game_query(query: &str, game: &str) -> Option<String> {
    static GAME_QUESTION: LazyLock<Regex> = LazyLock::new(|| {
        re(r"(?i)\b(?:walkthrough|boss(?:es)?|quests?|achievements?|troph(?:y|ies)|puzzles?|riddles?|collectibles?|endings?|cheats?|best (?:weapon|build|car|gear|armou?r|skill)s?)\b")
    });
    let said = words(query);
    let named = words(game).iter().filter(|w| w.len() > 2).any(|w| said.contains(w));
    (GAME_QUESTION.is_match(query) && !named).then(|| format!("{query} {}", plain(game)))
}

/// (bytes downloaded, bytes to download, fully installed) from a Steam manifest.
fn download_state(manifest: &Path) -> Option<(u64, u64, bool)> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let flags: u64 = vdf_value(&text, "StateFlags")?.parse().ok()?;
    let num = |k: &str| vdf_value(&text, k).and_then(|v| v.parse().ok()).unwrap_or(0u64);
    // StateFlags bit 4: fully installed (no update pending).
    Some((num("BytesDownloaded"), num("BytesToDownload"), flags & 4 != 0 && flags & 2 == 0))
}

fn download_answer(game: &str) -> String {
    let busy: Vec<(Game, u64, u64)> = library()
        .into_iter()
        .filter(|g| game.is_empty() || names(game, &g.name))
        .filter_map(|g| {
            let (have, all, installed) = download_state(g.manifest.as_deref()?)?;
            (!installed && all > 0).then_some((g, have, all))
        })
        .collect();
    let Some((g, have, all)) = busy.first() else {
        return say("No Steam download running.", "Nothing is downloading in Steam, Sir.");
    };
    let pct = *have as f64 * 100.0 / *all as f64;
    say(format!("{}: {pct:.0} % of {:.1} GB", g.name, *all as f64 / 1e9), format!("{} is at {pct:.0} percent, Sir.", plain(&g.name)))
}

// ------------------------------------------------------------------------------- news ----

async fn steam_news(appid: &str) -> Result<Vec<Value>, String> {
    let url = format!("https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/?appid={appid}&count=3&maxlength=200&format=json");
    let v: Value = http().get(url).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    Ok(v["appnews"]["newsitems"].as_array().cloned().unwrap_or_default())
}

async fn news(game: &str) -> Result<String, String> {
    let g = find(game).ok_or_else(|| format!("I can't find {game} among your games, Sir."))?;
    let appid = g.appid.clone().ok_or_else(|| format!("{} is not a Steam game, so I have no news feed for it, Sir.", plain(&g.name)))?;
    let items = steam_news(&appid).await.map_err(|e| format!("Steam's news didn't answer: {e}"))?;
    let Some(first) = items.first() else {
        return Ok(say(format!("No news for {}.", g.name), format!("There's no news for {}, Sir.", plain(&g.name))));
    };
    let title = first["title"].as_str().unwrap_or("");
    let date = chrono::DateTime::from_timestamp(first["date"].as_i64().unwrap_or(0), 0).map(|d| d.with_timezone(&chrono::Local).format("%-d %B").to_string()).unwrap_or_default();
    let lines: Vec<String> = items.iter().map(|n| format!("{} — {}", n["title"].as_str().unwrap_or(""), n["url"].as_str().unwrap_or(""))).collect();
    crate::kit::show_text("games_news", &format!("NEWS // {}", plain(&g.name).to_uppercase()), &lines.join("\n\n"), [460.0, 300.0]);
    Ok(say(lines.join("\n"), format!("The latest for {}, from {date}: {title}.", plain(&g.name))))
}

/// At a game's start: news newer than what he has seen, said once.
async fn news_at_start(title: String) {
    let Some(appid) = library().into_iter().find(|g| g.folder == title || names(&title, &g.name)).and_then(|g| g.appid) else { return };
    let Ok(items) = steam_news(&appid).await else { return };
    let Some(first) = items.first() else { return };
    let gid = first["gid"].as_str().unwrap_or("").to_string();
    let fresh = chrono::Local::now().timestamp() - first["date"].as_i64().unwrap_or(0) < 3 * 24 * 3600;
    let seen = crate::kit::get("game_news_seen", &appid).and_then(|v| v.as_str().map(str::to_string));
    if fresh && seen.as_deref() != Some(gid.as_str()) {
        let _ = crate::kit::put("game_news_seen", &appid, &json!(gid));
        alert(format!("News for {}, Sir: {}.", plain(&title), first["title"].as_str().unwrap_or("an update")));
    }
}

/// The games Epic gives away right now.
pub async fn epic_free_now() -> Result<Vec<String>, String> {
    let url = "https://store-site-backend-static.ak.epicgames.com/freeGamesPromotions?locale=en-US&country=DE&allowCountries=DE";
    let v: Value = http().get(url).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    Ok(free_titles(&v))
}

/// For the briefing: Epic's free games (listed every time) and a line on those, and on his
/// wishlist's sales, not told of yet.
pub async fn briefing_free_games() -> (Vec<String>, Option<String>) {
    let quick = std::time::Duration::from_secs(6);
    let (titles, sales) = tokio::join!(tokio::time::timeout(quick, epic_free_now()), tokio::time::timeout(quick, wishlist_sales()));
    let titles = titles.ok().and_then(Result::ok).unwrap_or_default();
    let new: Vec<&String> = titles.iter().filter(|t| crate::kit::get("free_games_told", t).is_none()).collect();
    if !crate::platform::in_test_session() {
        for t in &new {
            let _ = crate::kit::put("free_games_told", t, &json!(true));
        }
    }
    let free = (!new.is_empty()).then(|| format!("Free on Epic this week: {}.", new.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(" and ")));
    let on_sale = sales.ok().and_then(Result::ok).and_then(|sales| sale_line(&untold(&sales)));
    let line = [free, on_sale].into_iter().flatten().collect::<Vec<_>>().join(" ");
    (titles, (!line.is_empty()).then_some(line))
}

// ---------------------------------------------------------------------- the wishlist ----

/// The Steam account last signed in on this PC (`config/loginusers.vdf`: "MostRecent" "1").
fn steam_id_from(loginusers: &str) -> Option<String> {
    static USER: LazyLock<Regex> = LazyLock::new(|| re(r#""(\d{17})"\s*\{([^}]*)\}"#));
    let users: Vec<(String, bool)> = USER.captures_iter(loginusers).map(|c| (c[1].to_string(), vdf_value(&c[2], "MostRecent").as_deref() == Some("1"))).collect();
    users.iter().find(|(_, recent)| *recent).or(users.first()).map(|(id, _)| id.clone())
}

/// A discounted game on his wishlist.
#[derive(Clone, Debug, PartialEq)]
struct Sale {
    appid: u64,
    name: String,
    pct: u64,
    price: String,
    was: String,
    /// When the discount ends (Unix seconds; 0 when Steam does not say).
    ends: i64,
}

/// The discounted ones among `IStoreBrowseService/GetItems`' store items.
fn sales_from(items: &Value) -> Vec<Sale> {
    items["response"]["store_items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let offer = &item["best_purchase_option"];
            Some(Sale {
                appid: item["appid"].as_u64()?,
                name: item["name"].as_str()?.to_string(),
                pct: offer["discount_pct"].as_u64().filter(|p| *p > 0)?,
                price: offer["formatted_final_price"].as_str().unwrap_or("").to_string(),
                was: offer["formatted_original_price"].as_str().unwrap_or("").to_string(),
                ends: offer["active_discounts"][0]["discount_end_date"].as_i64().unwrap_or(0),
            })
        })
        .collect()
}

/// What on his public wishlist is discounted now, the biggest discount first.
async fn wishlist_sales() -> Result<Vec<Sale>, String> {
    let id = tokio::task::spawn_blocking(|| steam_path_from_registry().and_then(|root| std::fs::read_to_string(root.join("config").join("loginusers.vdf")).ok()).and_then(|text| steam_id_from(&text)))
        .await
        .ok()
        .flatten()
        .ok_or("I can't find your Steam account on this PC, Sir.")?;
    let get = |url: String| async move { http().get(url).send().await.map_err(|e| e.to_string())?.json::<Value>().await.map_err(|e| e.to_string()) };
    let list = get(format!("https://api.steampowered.com/IWishlistService/GetWishlist/v1/?steamid={id}")).await.map_err(|e| format!("Steam didn't answer: {e}"))?;
    let ids: Vec<Value> = list["response"]["items"].as_array().into_iter().flatten().filter_map(|i| i["appid"].as_u64()).map(|appid| json!({ "appid": appid })).collect();
    if ids.is_empty() {
        return Err("Your Steam wishlist is empty or private, Sir. Steam only shows a public one.".into());
    }
    let mut sales = Vec::new();
    for chunk in ids.chunks(100) {
        let input = json!({ "ids": chunk, "context": { "language": "english", "country_code": "DE" }, "data_request": { "include_basic_info": false } });
        let items = get(format!("https://api.steampowered.com/IStoreBrowseService/GetItems/v1?input_json={}", urlencoding::encode(&input.to_string()))).await?;
        sales.extend(sales_from(&items));
    }
    sales.sort_by(|a, b| b.pct.cmp(&a.pct));
    Ok(sales)
}

/// The sales not told of yet, marked told (a discount that comes back later is news again).
fn untold(sales: &[Sale]) -> Vec<&Sale> {
    let key = |s: &Sale| format!("{}:{}", s.appid, s.ends);
    let new: Vec<&Sale> = sales.iter().filter(|s| crate::kit::get("wishlist_told", &key(s)).is_none()).collect();
    if !crate::platform::in_test_session() {
        for s in &new {
            let _ = crate::kit::put("wishlist_told", &key(s), &json!(true));
        }
    }
    new
}

fn sale_line(sales: &[&Sale]) -> Option<String> {
    let named: Vec<String> = sales.iter().take(3).map(|s| format!("{} at {} percent off", plain(&s.name), s.pct)).collect();
    let more = sales.len().saturating_sub(3);
    (!named.is_empty()).then(|| format!("On sale from your Steam wishlist: {}{}.", named.join(", "), if more > 0 { format!(", and {more} more") } else { String::new() }))
}

/// "…, and 9 more, Sir.": the line said to him.
fn to_sir(line: &str) -> String {
    format!("{}, Sir.", line.trim_end_matches('.'))
}

/// The watcher's look (every few hours): new sales as an alert.
pub async fn check_wishlist() {
    if let Ok(sales) = wishlist_sales().await {
        if let Some(line) = sale_line(&untold(&sales)) {
            alert(to_sir(&line));
        }
    }
}

/// "Anything on my wishlist on sale?"
async fn wishlist_answer() -> Result<String, String> {
    let sales = wishlist_sales().await?;
    if sales.is_empty() {
        return Ok(say("Nothing on the wishlist is discounted.", "Nothing on your wishlist is on sale right now, Sir."));
    }
    let _ = untold(&sales);
    let rows: Vec<String> = sales.iter().map(|s| format!("{} — {} % off: {} (was {})", s.name, s.pct, s.price, s.was)).collect();
    crate::kit::show_text("games_wishlist", "STEAM // WISHLIST ON SALE", &rows.join("\n"), [460.0, 360.0]);
    let all: Vec<&Sale> = sales.iter().collect();
    Ok(say(rows.join("\n"), sale_line(&all).map(|l| to_sir(&l)).unwrap_or_default()))
}

fn free_titles(v: &Value) -> Vec<String> {
    v["data"]["Catalog"]["searchStore"]["elements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["price"]["totalPrice"]["discountPrice"].as_u64() == Some(0) && e["promotions"]["promotionalOffers"].as_array().is_some_and(|o| !o.is_empty()))
        .filter_map(|e| e["title"].as_str().map(str::to_string))
        .collect()
}

async fn free_games() -> Result<String, String> {
    let titles = epic_free_now().await.map_err(|e| format!("Epic's store didn't answer: {e}"))?;
    Ok(if titles.is_empty() {
        say("Nothing free on Epic right now.", "Nothing is free on Epic right now, Sir.")
    } else {
        say(format!("Free on Epic: {}", titles.join(", ")), format!("Free on Epic this week, Sir: {}.", titles.join(" and ")))
    })
}

// --------------------------------------------------------------------------------- VRAM ----

/// Video memory per program (Windows' "GPU Process Memory" counters), the largest first, in MB.
#[cfg(windows)]
pub fn vram_by_program() -> Vec<(String, u64)> {
    use windows::core::w;
    use windows::Win32::System::Performance::{PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_LARGE, PDH_MORE_DATA};
    let mut per_pid: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    // SAFETY: one query and one counter, the item buffer sized as PDH asked, closed at the end.
    unsafe {
        let (mut query, mut counter) = (0isize, 0isize);
        if PdhOpenQueryW(None, 0, &mut query) != 0 {
            return Vec::new();
        }
        if PdhAddEnglishCounterW(query, w!("\\GPU Process Memory(*)\\Dedicated Usage"), 0, &mut counter) == 0 && PdhCollectQueryData(query) == 0 {
            let (mut size, mut count) = (0u32, 0u32);
            if PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut size, &mut count, None) == PDH_MORE_DATA {
                let mut buf = vec![0u8; size as usize];
                let items = buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
                if PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut size, &mut count, Some(items)) == 0 {
                    for i in 0..count as usize {
                        let item = &*items.add(i);
                        let name = item.szName.to_string().unwrap_or_default();
                        // "pid_1234_luid_0x…_phys_0"
                        let pid = name.strip_prefix("pid_").and_then(|r| r.split('_').next()).and_then(|p| p.parse::<u32>().ok());
                        if let Some(pid) = pid {
                            *per_pid.entry(pid).or_default() += item.FmtValue.Anonymous.largeValue.max(0) as u64;
                        }
                    }
                }
            }
        }
        PdhCloseQuery(query);
    }
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, sysinfo::ProcessRefreshKind::new());
    let mut per_name: Vec<(String, u64)> = Vec::new();
    for (pid, bytes) in per_pid {
        let Some(p) = sys.process(sysinfo::Pid::from_u32(pid)) else { continue };
        let name = p.name().to_string_lossy().trim_end_matches(".exe").to_string();
        match per_name.iter_mut().find(|(n, _)| *n == name) {
            Some((_, b)) => *b += bytes,
            None => per_name.push((name, bytes)),
        }
    }
    let mut out: Vec<(String, u64)> = per_name.into_iter().map(|(n, b)| (n, b / 1_048_576)).filter(|(_, mb)| *mb >= 50).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

#[cfg(not(windows))]
pub fn vram_by_program() -> Vec<(String, u64)> {
    Vec::new()
}

fn vram_report() -> String {
    let top = vram_by_program();
    if top.is_empty() {
        return say("No program holds more than 50 MB of video memory.", "Nothing is holding much video memory, Sir.");
    }
    let size = |mb: u64| if mb >= 1024 { format!("{:.1} GB", mb as f64 / 1024.0) } else { format!("{mb} MB") };
    let lines: Vec<String> = top.iter().take(6).map(|(n, mb)| format!("{n} {}", size(*mb))).collect();
    let (big, mb) = &top[0];
    say(lines.join("\n"), format!("{big} holds the most, {}. Say \"close {big}\" to free it, Sir.", size(*mb)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_are_named_the_way_he_says_them() {
        assert!(names("gta", "Grand Theft Auto V") && names("gta 5", "Grand Theft Auto V") && names("batman", "Batman™: Arkham Knight"));
        assert!(names("arkham knight", "Batman™: Arkham Knight") && !names("minecraft", "Grand Theft Auto V") && !names("ga", "Grand Theft Auto V"));
        assert_eq!(find("gta").map(|g| g.name).as_deref(), Some("Grand Theft Auto V"), "the shortest that fits");
        assert_eq!(find("timer"), None);
    }

    #[test]
    fn his_words_for_the_games_kit() {
        let action = |s: &str| command(s).map(|v| v["action"].as_str().unwrap_or("").to_string());
        assert_eq!(command("start batman"), Some(json!({"action": "start", "game": "Batman™: Arkham Knight"})));
        assert_eq!(command("Launch GTA 5 please"), Some(json!({"action": "start", "game": "Grand Theft Auto V"})));
        assert_eq!(command("start the timer"), None, "only his games are started");
        assert_eq!(action("what games do I have?").as_deref(), Some("list"));
        assert_eq!(command("how long have I played this week"), Some(json!({"action": "playtime", "span": "week"})));
        assert_eq!(command("how long have i been playing"), Some(json!({"action": "playtime", "span": "today"})));
        assert_eq!(command("remind me after the game to do my homework"), Some(json!({"action": "after", "text": "do my homework"})));
        assert_eq!(command("when GTA starts, set the volume to 40 and open discord"), Some(json!({"action": "routine", "game": "Grand Theft Auto V", "order": "set the volume to 40 and open discord"})));
        assert_eq!(command("whenever i start batman open discord"), Some(json!({"action": "routine", "game": "Batman™: Arkham Knight", "order": "open discord"})));
        assert_eq!(command("whenever it rains open the weather"), None, "not a game: the rules' own words");
        assert_eq!(command("any news for gta"), Some(json!({"action": "news", "game": "Grand Theft Auto V"})));
        assert_eq!(action("are there any free games this week").as_deref(), Some("free"));
        assert_eq!(action("what's using my vram").as_deref(), Some("vram"));
        assert_eq!(command("tell me when the gta download is done"), Some(json!({"action": "watch_download", "game": "gta"})));
        assert_eq!(command("stop gaming at 10 on school nights"), Some(json!({"action": "bedtime", "at": "22:00"})));
        assert_eq!(command("remind me to stop playing at 21:30 on school nights"), Some(json!({"action": "bedtime", "at": "21:30"})));
    }

    #[test]
    fn a_filler_word_starts_no_game() {
        for s in ["start it", "launch it", "start the game", "start one"] {
            assert_eq!(command(s), None, "{s}");
        }
        assert_eq!(command("start it takes example"), Some(json!({"action": "start", "game": "It Takes Example"})));
        assert_eq!(command("start the example game"), Some(json!({"action": "start", "game": "The Example Game"})), "a filler word still tells his games apart");
    }

    #[test]
    fn a_game_question_searches_with_the_game() {
        let game = "Batman™: Arkham Knight";
        assert_eq!(game_query("how do I beat the first boss", game).as_deref(), Some("how do I beat the first boss Batman: Arkham Knight"));
        assert_eq!(game_query("where is the riddler trophy in arkham knight", game), None, "named already");
        assert_eq!(game_query("who won the match yesterday", game), None, "no game question");
        for q in [
            "how do I reset my router", "where is the nearest pharmacy", "how to get the bus to school", "a guide to my tax return",
            "what's my battery level", "how do I unlock my phone", "pause the research mission", "which chapter is the test about",
        ] {
            assert_eq!(game_query(q, game), None, "not about the game: {q}");
        }
    }

    #[test]
    fn a_school_night_time_is_an_evening_time() {
        use chrono::TimeZone;
        for (said, at) in [("10", "22:00"), ("22:30", "22:30"), ("9 pm", "21:00")] {
            assert_eq!(evening_time(said).as_deref(), Some(at), "{said}");
        }
        for said in ["12", "0:30", "1 am"] {
            assert_eq!(evening_time(said), None, "{said}");
        }
        assert_eq!(command("stop gaming at 1 am on school nights"), None);
        // A time kept before (noon, one at night) never nudges on a Sunday afternoon.
        let sunday = chrono::Local.with_ymd_and_hms(2026, 11, 15, 14, 0, 0).single().unwrap();
        for at in ["12:00", "01:00"] {
            assert!(nudge_due(sunday, true, Some(at.into()), &mut None).is_none(), "{at}");
        }
    }

    #[test]
    fn a_launcher_handing_over_to_its_game_is_one_session() {
        let _one = crate::kit::test_lock();
        let game = "Example Game";
        crate::kit::put("game_routine", game, &json!("set the volume to 40")).unwrap();
        crate::kit::put("games", "after", &json!(["do my homework"])).unwrap();
        on_change(None, Some(game.into()));
        // The launcher closes; the game itself starts a few seconds later.
        on_change(Some(game.into()), None);
        on_change(None, Some(game.into()));
        let (orders, alerts, kept) = (due_orders(), due_alerts(), crate::kit::get("games", "after"));
        // Another game: this one is over at once.
        on_change(Some(game.into()), Some("Another Game".into()));
        let (ended, _) = (due_alerts(), due_orders());
        *lock(&SESSION) = None;
        assert_eq!(orders, ["set the volume to 40"], "its routine runs once");
        assert!(alerts.is_empty() && kept.is_some(), "the game is not over yet: {alerts:?}");
        assert!(ended.iter().any(|a| a.contains("do my homework")), "{ended:?}");
    }

    #[test]
    fn a_game_gone_30_s_is_over() {
        use chrono::TimeZone;
        let _one = crate::kit::test_lock();
        let at = |m: u32, s: u32| chrono::Local.with_ymd_and_hms(2026, 11, 15, 20, m, s).single().unwrap();
        *lock(&SESSION) = Some(("Example Game".into(), at(0, 0)));
        // It closed at 20:10.
        *lock(&LEFT) = Some(("Example Game".into(), at(10, 0)));
        finish_left(at(10, 29));
        let kept = lock(&SESSION).is_some() && lock(&LEFT).is_some();
        finish_left(at(10, 30));
        let over = lock(&SESSION).is_none() && lock(&LEFT).is_none();
        let played = crate::kit::get("playtime", "2026-11-15|Example Game").and_then(|v| v.as_f64());
        let _ = due_alerts();
        *lock(&SESSION) = None;
        *lock(&LEFT) = None;
        assert!(kept, "gone 29 s: the game may yet come back");
        assert!(over, "gone 30 s: the session is over");
        assert_eq!(played, Some(10.0), "played until the game closed, not until its session ended");
    }

    #[test]
    fn welcome_back_counts_only_what_came_in() {
        use chrono::TimeZone;
        let at = |m: u32| chrono::Local.with_ymd_and_hms(2026, 11, 15, 20, m, 0).single().unwrap();
        let note = |text: &str, m: u32| crate::hud_notify::Note { text: text.into(), alert: false, at: at(m) };
        let news = "News for Example Game, Sir: Patch 1.2.".to_string();
        let notes = [
            note("Discord: Max Mustermann wrote to JAI: later", 50),
            note("Discord: Max Mustermann wrote to JAI: hi", 30),
            note(&news, 1),
            note("As planned: set the volume to 40", 1),
            note("Discord: Max Mustermann wrote to JAI: earlier", 0),
        ];
        // Played from 20:01 to 20:40: one message came in; the news and the routine are the kit's own.
        assert_eq!(came_in(&notes, at(1)..=at(40), &[news]), 1);
    }

    #[test]
    fn the_school_night_nudge_comes_once_on_school_nights() {
        use chrono::TimeZone;
        let at = |d: u32, h: u32, m: u32| chrono::Local.with_ymd_and_hms(2026, 11, d, h, m, 0).single().unwrap();
        let mut last = None;
        // 2026-11-15 is a Sunday (school tomorrow), the 13th a Friday.
        assert!(nudge_due(at(15, 21, 59), true, Some("22:00".into()), &mut last).is_none(), "not yet");
        assert!(nudge_due(at(15, 22, 0), true, Some("22:00".into()), &mut last).is_some());
        assert!(nudge_due(at(15, 22, 5), true, Some("22:00".into()), &mut last).is_none(), "once a night");
        assert!(nudge_due(at(13, 23, 0), true, Some("22:00".into()), &mut None).is_none(), "Friday is no school night");
        assert!(nudge_due(at(16, 22, 30), false, Some("22:00".into()), &mut None).is_none(), "only while he plays");
    }

    #[test]
    fn memory_hogs_beside_a_game_are_named_and_only_closable_ones() {
        let p = |name: &str, mb: f64| crate::diagnostics::ProcessUsage { name: name.into(), cpu_pct: 1.0, memory_mb: mb, count: 1 };
        let list = [p("chrome", 3276.8), p("javaw", 6100.0), p("Discord", 1500.0), p("Code", 1843.2), p("Sonora", 900.0), p("msedge", 700.0), p("slack", 1000.0), p("blender", 900.0), p("jai", 2500.0), p("svchost", 2000.0)];
        let found = hogs(&list);
        assert_eq!(found.iter().map(|h| h.0).collect::<Vec<_>>(), ["Chrome", "VS Code", "Slack"], "biggest first, three at most; never the game, Discord, his player, JAI or Windows; Edge under the floor");
        let (line, order) = hogs_line(&found).expect("a line");
        assert_eq!(line, "Chrome holds 3.2 GB, VS Code 1.8 GB and Slack 1.0 GB beside the game, Sir. Shall I close Chrome?");
        assert_eq!(order, "close chrome");
        assert_eq!(hogs_line(&hogs(&[p("javaw", 6100.0), p("msedge", 500.0)])), None, "nothing worth closing: nothing said");
    }

    /// His account from Steam's sign-in list, the sales from the store's answer (its real shape,
    /// 2026-11-16), and the line said about them.
    #[test]
    fn the_wishlist_s_sales_are_told_biggest_first() {
        let users = r#""users" { "76561190000000001" { "AccountName" "max" "MostRecent" "0" } "76561190000000002" { "AccountName" "maxi" "MostRecent" "1" } }"#;
        assert_eq!(steam_id_from(users).as_deref(), Some("76561190000000002"));
        assert_eq!(steam_id_from(r#""users" { }"#), None);
        let items = json!({"response": {"store_items": [
            {"appid": 620, "name": "Portal 2", "best_purchase_option": {"formatted_final_price": "9,69€"}},
            {"appid": 489830, "name": "The Elder Scrolls V: Skyrim Special Edition", "best_purchase_option": {"formatted_final_price": "7,99€", "formatted_original_price": "39,99€", "discount_pct": 80, "active_discounts": [{"discount_end_date": 1795024800}]}},
            {"appid": 570, "name": "Dota 2", "is_free": true}
        ]}});
        let sales = sales_from(&items);
        assert_eq!(sales, vec![Sale { appid: 489830, name: "The Elder Scrolls V: Skyrim Special Edition".into(), pct: 80, price: "7,99€".into(), was: "39,99€".into(), ends: 1795024800 }]);
        let many: Vec<&Sale> = std::iter::repeat_n(&sales[0], 5).collect();
        assert_eq!(sale_line(&many).unwrap(), format!("On sale from your Steam wishlist: {0}, {0}, {0}, and 2 more.", format!("{} at 80 percent off", plain(&sales[0].name))));
        assert_eq!(sale_line(&[]), None);
        assert_eq!(to_sir("On sale from your Steam wishlist: Portal 2 at 80 percent off, and 9 more."), "On sale from your Steam wishlist: Portal 2 at 80 percent off, and 9 more, Sir.");
        for said in ["Anything on my wishlist on sale?", "is something on my steam wishlist discounted", "Ist was auf meiner Wunschliste im Angebot?"] {
            assert_eq!(command(&crate::intents::normalize_phrase(said)), Some(json!({"action": "wishlist"})), "{said}");
        }
        let road = crate::phrasings::route("Anything on my wishlist on sale?");
        assert_eq!(road.as_ref().map(|(tool, args)| (tool.as_str(), args["action"].as_str())), Some(("games", Some("wishlist"))), "{road:?}");
    }

    #[test]
    fn games_he_has_not_played_for_months_come_biggest_first() {
        for said in ["Which games haven't I played in months?", "what games have i not played in a while", "show my unplayed games", "Welche Spiele habe ich lange nicht mehr gespielt?"] {
            assert_eq!(command(&crate::intents::normalize_phrase(said)), Some(json!({"action": "unplayed"})), "{said}");
        }
        assert_eq!(command("which games have i played"), None);
        let now = 1_800_000_000;
        let day = 86_400;
        let rows = vec![
            ("Small Old".to_string(), now - 200 * day, 2_000_000_000),
            ("Fresh".to_string(), now - 10 * day, 90_000_000_000),
            ("Never Started".to_string(), 0, 5_500_000_000),
            ("Big Old".to_string(), now - 95 * day, 120_000_000_000),
        ];
        let (text, spoken) = unplayed_answer(&rows, now);
        let names: Vec<&str> = text.lines().map(|l| l.rsplit("   ").next().unwrap_or("").trim()).collect();
        assert!(text.lines().count() == 3 && !text.contains("Fresh"), "{text}");
        assert!(text.lines().next().unwrap().contains("Big Old") && text.lines().last().unwrap().contains("Small Old"), "biggest first: {names:?}");
        assert!(text.contains("never") && text.contains("120.0 GB"), "{text}");
        assert_eq!(spoken, "3 games you haven't played in three months, Sir, 128 gigabytes together. The biggest is Big Old.");
        assert!(unplayed_answer(&rows[1..2], now).0.is_empty());
    }

    #[test]
    fn steam_manifests_and_epic_offers_are_read() {
        let manifest = "\"AppState\"\n{\n\t\"appid\"\t\t\"271590\"\n\t\"name\"\t\t\"Grand Theft Auto V\"\n\t\"StateFlags\"\t\t\"1026\"\n\t\"installdir\"\t\t\"Grand Theft Auto V\"\n\t\"BytesToDownload\"\t\t\"1000\"\n\t\"BytesDownloaded\"\t\t\"250\"\n}";
        assert_eq!(vdf_value(manifest, "appid").as_deref(), Some("271590"));
        assert_eq!(vdf_values("\"path\"\t\t\"D:\\\\SteamLibrary\"", "path"), vec![r"D:\SteamLibrary".to_string()]);
        let dir = std::env::temp_dir().join(format!("jai_manifest_{}", std::process::id()));
        std::fs::write(&dir, manifest).unwrap();
        assert_eq!(download_state(&dir), Some((250, 1000, false)), "1026: an update downloading");
        let _ = std::fs::remove_file(&dir);
        let epic = json!({"data": {"Catalog": {"searchStore": {"elements": [
            {"title": "Free One", "price": {"totalPrice": {"discountPrice": 0}}, "promotions": {"promotionalOffers": [{"promotionalOffers": [{}]}]}},
            {"title": "Next Week", "price": {"totalPrice": {"discountPrice": 1999}}, "promotions": {"promotionalOffers": [], "upcomingPromotionalOffers": [{}]}}
        ]}}}});
        assert_eq!(free_titles(&epic), vec!["Free One".to_string()]);
        assert!(not_a_game("Steamworks Common Redistributables") && not_a_game("Wallpaper Engine") && !not_a_game("Grand Theft Auto V"));
    }

    #[test]
    fn play_time_adds_up_the_days() {
        let _one = crate::kit::test_lock();
        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        crate::kit::put("playtime", &format!("{today}|Grand Theft Auto V"), &json!(95.0)).unwrap();
        crate::kit::put("playtime", "2001-01-01|Old Game", &json!(500.0)).unwrap();
        let said = playtime_answer(false, now);
        assert!(said.contains("1 hour 35 minutes today, Sir: Grand Theft Auto V 1 hour 35 minutes."), "{said}");
    }
}
