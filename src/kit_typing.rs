//! What J.A.I. types and presses for Sir (2026-11-13): dictation ("Jay, dictate": what Sir
//! says is typed into the app in front until "stop dictating"), snippets ("type my address":
//! stored pieces of text) and hands-free keys ("next tab", "scroll down", "reload the page").
//!
//! All three end in key strokes sent to whatever has the keyboard focus (`win32::type_text`,
//! `win32::send_chord`). A test and a scripted session send none (`platform::hands_off`), and
//! nothing is ever typed into J.A.I.'s own window by dictation.

use crate::kit::say;
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid phrase")
}

const HANDS_OFF: &str = "A test session types into nothing.";

// --------------------------------------------------------------------------- dictation ----

static DICTATING: AtomicBool = AtomicBool::new(false);
/// When dictation last heard something (it ends by itself after `DICTATION_RESTS` of silence).
static HEARD_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// How many characters each dictated sentence typed (for "delete that").
static TYPED: Mutex<Vec<usize>> = Mutex::new(Vec::new());
const DICTATION_RESTS: Duration = Duration::from_secs(300);

static DICTATE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:start\s+)?dictat(?:e|ing|ion)(?:\s+(?:on|mode))?$|^take\s+(?:a\s+)?dictation$|^(?:type|write)\s+(?:down\s+)?what\s+i\s+say$|^diktat(?:\s+(?:an|starten|modus))?$|^diktieren?$|^schreib\s+(?:mit|auf\s+was\s+ich\s+sage|was\s+ich\s+sage)$")
});
static DICTATE_STOP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:jai|jay)\s+)?(?:(?:stop|end|finish|quit)\s+(?:the\s+)?dictat(?:ing|ion|e)|dictation\s+(?:off|over|done|finished|end)|(?:thats|that\s+is)\s+all\s+for\s+the\s+dictation|diktat\s+(?:beenden|aus|ende|stopp?|fertig)|stopp?\s+(?:das\s+)?diktat|(?:hör|hoer)\s+auf\s+zu\s+(?:diktieren|schreiben))$")
});
/// What ends a dictation while one is running: the same, and the short ways of saying it. (A
/// bare "stop" ends it too: better one word too few on the page than no way out.)
static DICTATE_END: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:stop|end|finish|quit)(?:\s+(?:the\s+|my\s+)?(?:dictat(?:ing|ion|e)|typing|writing))?|dictation\s+(?:off|over|done|finished|end)|(?:thats|that\s+is)\s+(?:all|it|enough)(?:\s+for\s+the\s+dictation)?|diktat\s+(?:beenden|aus|ende|stopp?|fertig)|stopp?(?:\s+(?:das\s+)?diktat)?|(?:hör|hoer)\s+auf(?:\s+zu\s+(?:diktieren|schreiben|tippen))?|ende|fertig)$")
});

/// Whether the words end the dictation: one of its stop phrases, with or without his name
/// and the small words around an order ("Okay, stop dictating, please.").
fn ends_dictation(said: &str) -> bool {
    const AROUND: &[&str] = &["ok", "okay", "jai", "jay", "and", "so", "now", "please", "thanks", "thank", "you", "bitte", "danke", "jetzt"];
    let words: Vec<&str> = said.split_whitespace().collect();
    let from = words.iter().take_while(|w| AROUND.contains(*w)).count();
    let to = words.len() - words[from..].iter().rev().take_while(|w| AROUND.contains(*w)).count();
    from < to && DICTATE_END.is_match(&words[from..to].join(" "))
}

fn plain(text: &str) -> String {
    crate::ear::words(text).join(" ")
}

/// "Dictate", "start dictating", "Diktat": on. "Stop dictating", "Diktat beenden": off.
pub fn dictation_command(s: &str) -> Option<Value> {
    if DICTATE_STOP.is_match(s) {
        return Some(json!({"action": "stop"}));
    }
    DICTATE.is_match(s).then(|| json!({"action": "start"}))
}

/// Whether what Sir says is being typed (and ends it when nothing was said for five minutes).
pub fn dictating() -> bool {
    if !DICTATING.load(Ordering::Acquire) {
        return false;
    }
    if lock(&HEARD_AT).is_some_and(|at| at.elapsed() > DICTATION_RESTS) {
        DICTATING.store(false, Ordering::Release);
        tracing::info!("[DICTATION] Nothing was said for five minutes: dictation has ended by itself");
        return false;
    }
    true
}

fn set_dictating(on: bool) {
    DICTATING.store(on, Ordering::Release);
    *lock(&HEARD_AT) = on.then(Instant::now);
    lock(&TYPED).clear();
}

pub fn dictation(args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("start") {
        "stop" => {
            let was = DICTATING.load(Ordering::Acquire);
            set_dictating(false);
            Ok(if was { say("Dictation ended.", "Dictation ended, Sir.") } else { say("No dictation was running.", "I was not taking dictation, Sir.") })
        }
        _ => {
            set_dictating(true);
            tracing::info!("[DICTATION] On: what Sir says is typed into the app in front until \"stop dictating\"");
            Ok(say(
                "Dictation is on. What you say is typed into the app in front. \"New line\", \"new paragraph\" and \"delete that\" do what they say; \"stop dictating\" ends it.",
                "Go ahead, Sir. I type what you say; \"stop dictating\" ends it.",
            ))
        }
    }
}

/// What a sentence is while Sir dictates.
#[derive(Debug, Clone, PartialEq)]
pub enum Dictated {
    /// Typed as it is.
    Type(String),
    /// The sentence typed last is taken out again.
    DeleteLast,
    Stop,
}

/// What a sentence heard during dictation is: text to type, or one of its few orders.
pub fn dictated(text: &str) -> Dictated {
    let said = plain(text);
    if ends_dictation(&said) {
        return Dictated::Stop;
    }
    match said.as_str() {
        "delete that" | "scratch that" | "delete the last sentence" | "undo that" | "lösch das" | "streich das" | "lösche das" => Dictated::DeleteLast,
        "new line" | "next line" | "neue zeile" | "nächste zeile" => Dictated::Type("\n".to_string()),
        "new paragraph" | "next paragraph" | "neuer absatz" => Dictated::Type("\n\n".to_string()),
        _ => Dictated::Type(format!("{} ", text.trim())),
    }
}

/// A sentence heard while dictation is on, carried out: typed into the app in front, taken
/// out again, or the end of the dictation. What the HUD shows for it; `Err` when nothing
/// could be typed. `own_window`: J.A.I.'s own window has the keyboard.
pub async fn dictate(text: &str, own_window: bool) -> Result<String, String> {
    match dictated(text) {
        Dictated::Stop => {
            set_dictating(false);
            tracing::info!("[DICTATION] Ended on Sir's word");
            Ok("Dictation ended".to_string())
        }
        _ if own_window => Err("My own window has the keyboard, Sir: click into the app I should type into.".to_string()),
        Dictated::DeleteLast => {
            let Some(n) = lock(&TYPED).pop() else { return Ok("Nothing left to delete".to_string()) };
            press(Key::Backspace, n).await?;
            Ok(format!("Deleted {n} characters"))
        }
        Dictated::Type(typed) => {
            let n = type_out(typed.clone()).await?;
            // (The five minutes count from the last sentence that was typed, not from any sound.)
            *lock(&HEARD_AT) = Some(Instant::now());
            lock(&TYPED).push(n);
            tracing::info!("[DICTATION] Typed {n} characters");
            Ok(typed.trim().to_string())
        }
    }
}

// ------------------------------------------------------------------------------ typing ----

/// Types `text` into whatever has the keyboard focus; how many characters went out.
pub async fn type_out(text: String) -> Result<usize, String> {
    if crate::platform::hands_off() {
        return Err(HANDS_OFF.to_string());
    }
    #[cfg(windows)]
    {
        crate::win32::call(move || crate::win32::type_text(&text)).await.map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        Err("Typing for you is only built for Windows.".to_string())
    }
}

/// A key the kit presses, alone or with Ctrl / Shift / Alt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Tab,
    T,
    A,
    V,
    F,
    Left,
    Right,
    Down,
    Up,
    PageDown,
    PageUp,
    Home,
    End,
    F5,
    Enter,
    Escape,
    Backspace,
}

/// One key stroke: the key with (ctrl, shift, alt), pressed `times` times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stroke {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub times: usize,
}

const fn stroke(key: Key, ctrl: bool, shift: bool, alt: bool, times: usize) -> Stroke {
    Stroke { key, ctrl, shift, alt, times }
}

async fn press(key: Key, times: usize) -> Result<(), String> {
    strike(vec![stroke(key, false, false, false, times)]).await
}

/// Sends key strokes to whatever has the keyboard focus.
async fn strike(strokes: Vec<Stroke>) -> Result<(), String> {
    if crate::platform::hands_off() {
        return Err(HANDS_OFF.to_string());
    }
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse as k;
        crate::win32::call(move || {
            for s in strokes {
                let vk = match s.key {
                    Key::Tab => k::VK_TAB,
                    Key::T => k::VK_T,
                    Key::A => k::VK_A,
                    Key::V => k::VK_V,
                    Key::F => k::VK_F,
                    Key::Left => k::VK_LEFT,
                    Key::Right => k::VK_RIGHT,
                    Key::Down => k::VK_DOWN,
                    Key::Up => k::VK_UP,
                    Key::PageDown => k::VK_NEXT,
                    Key::PageUp => k::VK_PRIOR,
                    Key::Home => k::VK_HOME,
                    Key::End => k::VK_END,
                    Key::F5 => k::VK_F5,
                    Key::Enter => k::VK_RETURN,
                    Key::Escape => k::VK_ESCAPE,
                    Key::Backspace => k::VK_BACK,
                };
                let mut with = Vec::new();
                if s.ctrl {
                    with.push(k::VK_CONTROL);
                }
                if s.shift {
                    with.push(k::VK_SHIFT);
                }
                if s.alt {
                    with.push(k::VK_MENU);
                }
                for n in 0..s.times.clamp(1, 400) {
                    crate::win32::send_chord(&with, vk)?;
                    if n % 16 == 15 {
                        std::thread::sleep(Duration::from_millis(3));
                    }
                }
            }
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        let _ = strokes;
        Err("Pressing keys for you is only built for Windows.".to_string())
    }
}

// --------------------------------------------------------------------- hands-free keys ----

/// The keys Sir can ask for: (the phrase, its name, the strokes, what he says).
struct Keys {
    phrase: &'static str,
    name: &'static str,
    strokes: &'static [Stroke],
    done: &'static str,
}

const KEYS: &[Keys] = &[
    Keys { phrase: r"^(?:next\s+tab|tab\s+right|nächster\s+tab|naechster\s+tab)$", name: "next_tab", strokes: &[stroke(Key::Tab, true, false, false, 1)], done: "Next tab." },
    Keys {
        phrase: r"^(?:previous\s+tab|last\s+tab|tab\s+left|vorheriger\s+tab|letzter\s+tab)$",
        name: "previous_tab",
        strokes: &[stroke(Key::Tab, true, true, false, 1)],
        done: "Previous tab.",
    },
    Keys { phrase: r"^(?:new\s+tab|open\s+a\s+new\s+tab|neuer\s+tab|neuen\s+tab)$", name: "new_tab", strokes: &[stroke(Key::T, true, false, false, 1)], done: "A new tab." },
    Keys {
        phrase: r"^(?:reopen\s+(?:the\s+|that\s+|my\s+)?(?:closed\s+|last\s+)?tab|bring\s+(?:the\s+|that\s+)?tab\s+back|tab\s+wiederherstellen)$",
        name: "reopen_tab",
        strokes: &[stroke(Key::T, true, true, false, 1)],
        done: "The tab is back.",
    },
    Keys {
        phrase: r"^(?:scroll\s+down|page\s+down|nach\s+unten\s+scrollen|runter\s+scrollen|scroll\s+runter|weiter\s+runter)$",
        name: "scroll_down",
        strokes: &[stroke(Key::PageDown, false, false, false, 1)],
        done: "Down.",
    },
    Keys {
        phrase: r"^(?:scroll\s+up|page\s+up|nach\s+oben\s+scrollen|hoch\s+scrollen|scroll\s+hoch|weiter\s+hoch)$",
        name: "scroll_up",
        strokes: &[stroke(Key::PageUp, false, false, false, 1)],
        done: "Up.",
    },
    Keys {
        phrase: r"^(?:scroll\s+down\s+a\s+(?:bit|little)|a\s+(?:bit|little)\s+(?:further\s+)?down|ein\s+(?:bisschen|stück)\s+runter)$",
        name: "scroll_down_a_little",
        strokes: &[stroke(Key::Down, false, false, false, 6)],
        done: "A little down.",
    },
    Keys {
        phrase: r"^(?:scroll\s+up\s+a\s+(?:bit|little)|a\s+(?:bit|little)\s+(?:further\s+)?up|ein\s+(?:bisschen|stück)\s+hoch)$",
        name: "scroll_up_a_little",
        strokes: &[stroke(Key::Up, false, false, false, 6)],
        done: "A little up.",
    },
    Keys {
        phrase: r"^(?:(?:scroll|go)\s+to\s+the\s+(?:very\s+)?top|top\s+of\s+the\s+page|ganz\s+nach\s+oben)$",
        name: "top",
        strokes: &[stroke(Key::Home, true, false, false, 1)],
        done: "The top.",
    },
    Keys {
        phrase: r"^(?:(?:scroll|go)\s+to\s+the\s+(?:very\s+)?(?:bottom|end)|(?:bottom|end)\s+of\s+the\s+page|ganz\s+nach\s+unten)$",
        name: "bottom",
        strokes: &[stroke(Key::End, true, false, false, 1)],
        done: "The bottom.",
    },
    Keys {
        phrase: r"^(?:(?:browser|page)\s+back|go\s+back\s+(?:a|one)\s+page|back\s+in\s+the\s+browser|seite\s+zurück|im\s+browser\s+zurück)$",
        name: "page_back",
        strokes: &[stroke(Key::Left, false, false, true, 1)],
        done: "Back.",
    },
    Keys {
        phrase: r"^(?:(?:browser|page)\s+forward|go\s+forward\s+(?:a|one)\s+page|forward\s+in\s+the\s+browser|seite\s+vor|im\s+browser\s+vor)$",
        name: "page_forward",
        strokes: &[stroke(Key::Right, false, false, true, 1)],
        done: "Forward.",
    },
    Keys {
        // (A bare "reload" is the older order that restarts him.)
        phrase: r"^(?:(?:reload|refresh)\s+(?:the\s+|this\s+)?(?:page|tab|site)|seite\s+(?:neu\s+laden|aktualisieren))$",
        name: "reload",
        strokes: &[stroke(Key::F5, false, false, false, 1)],
        done: "Reloaded.",
    },
    Keys { phrase: r"^(?:select\s+all|select\s+everything|alles\s+(?:markieren|auswählen))$", name: "select_all", strokes: &[stroke(Key::A, true, false, false, 1)], done: "All selected." },
    Keys { phrase: r"^(?:paste\s+(?:it|that)|paste\s+(?:it\s+)?here|füg\s+(?:es|das)\s+ein|einfügen)$", name: "paste", strokes: &[stroke(Key::V, true, false, false, 1)], done: "Pasted." },
    Keys { phrase: r"^(?:press|hit)\s+(?:the\s+)?enter(?:\s+key)?$|^enter\s+drücken$|^drück\s+enter$", name: "enter", strokes: &[stroke(Key::Enter, false, false, false, 1)], done: "Enter." },
    Keys { phrase: r"^(?:press|hit)\s+(?:the\s+)?escape(?:\s+key)?$|^escape\s+drücken$|^drück\s+escape$", name: "escape", strokes: &[stroke(Key::Escape, false, false, false, 1)], done: "Escape." },
];

static KEY_PHRASES: LazyLock<Vec<Regex>> = LazyLock::new(|| KEYS.iter().map(|k| re(k.phrase)).collect());
static FIND_ON_PAGE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:search|look)\s+(?:on\s+)?(?:the|this)\s+page\s+for\s+(.+)$|^find\s+(.+?)\s+on\s+(?:the|this)\s+page$|^such(?:e)?\s+auf\s+der\s+seite\s+nach\s+(.+)$"));

/// "Next tab", "scroll down", "reload the page", "search the page for …": keys for the app in
/// front.
pub fn keys_command(s: &str) -> Option<Value> {
    if let Some(at) = KEY_PHRASES.iter().position(|p| p.is_match(s)) {
        return Some(json!({"keys": KEYS[at].name}));
    }
    let c = FIND_ON_PAGE.captures(s)?;
    let what = c.get(1).or(c.get(2)).or(c.get(3))?.as_str().trim();
    (!what.is_empty()).then(|| json!({"keys": "find", "text": what}))
}

/// The window Sir works in gets the keyboard back when J.A.I.'s own window has it (he typed
/// the order into the console): the keys are for his app, never for J.A.I.
async fn to_his_app(ctx: &ToolContext) -> Result<(), String> {
    #[cfg(windows)]
    {
        let own = crate::win32::call(|| crate::win32::foreground_info().is_some_and(|(_, own)| own)).await.unwrap_or(false);
        if own {
            let hwnd = ctx.focus.foreground().map(|w| w.handle).filter(|h| *h != 0).ok_or("I see no app of yours to press that in, Sir.")?;
            crate::win32::call(move || crate::win32::focus(hwnd)).await.map_err(|e| e.to_string())?;
            tokio::time::sleep(Duration::from_millis(140)).await;
        }
    }
    let _ = ctx;
    Ok(())
}

pub async fn keys(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let name = args["keys"].as_str().unwrap_or("");
    if crate::platform::hands_off() {
        return Err(HANDS_OFF.to_string());
    }
    if name == "find" {
        let text = args["text"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            return Err("What shall I look for on the page, Sir?".to_string());
        }
        to_his_app(ctx).await?;
        strike(vec![stroke(Key::F, true, false, false, 1)]).await?;
        tokio::time::sleep(Duration::from_millis(180)).await;
        type_out(text.clone()).await?;
        press(Key::Enter, 1).await?;
        return Ok(say(format!("Searched the page for \"{text}\"."), "Searching the page, Sir."));
    }
    let keys = KEYS.iter().find(|k| k.name == name).ok_or_else(|| format!("I know no keys called '{name}'."))?;
    to_his_app(ctx).await?;
    strike(keys.strokes.to_vec()).await?;
    Ok(say(keys.done, keys.done))
}

// ----------------------------------------------------------------------------- snippets ----

const SNIPPET: &str = "snippet";

static SNIPPET_SAVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^\s*(?:(?:jai|jay)[,.]?\s+)?(?:please\s+)?(?:save|store|add|new|create|remember|speichere?|merk\s+dir)\s+(?:a\s+|the\s+|my\s+|den\s+|einen\s+)?(?:snippet|textbaustein)\s+(?:called\s+|named\s+|namens\s+)?(?P<name>[\w äöüß-]{1,30}?)\s*(?::|=|\s+is\s+|\s+as\s+|\s+with\s+|\s+ist\s+|\s+mit\s+)\s*(?P<text>.+?)\s*$")
});
static SNIPPET_CLIPBOARD: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:save|store|keep)\s+(?:my\s+|the\s+)?clipboard\s+as\s+(?:a\s+|the\s+|my\s+)?snippet\s+(?:called\s+|named\s+)?(.+)$|^speichere?\s+die\s+zwischenablage\s+als\s+(?:textbaustein|snippet)\s+(.+)$"));
static SNIPPET_TYPE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:type|paste|insert|enter|write)\s+(?:in\s+)?(?:my\s+|the\s+)?(?:snippet\s+)?(.+?)(?:\s+snippet)?$|^(?:tippe?|schreib|füge)\s+(?:meine?n?\s+|den\s+)?(?:textbaustein\s+)?(.+?)(?:\s+ein)?$"));
static SNIPPET_LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:list|show)\s+(?:me\s+)?(?:my\s+|the\s+|all\s+)?snippets$|^what\s+snippets\s+do\s+i\s+have$|^(?:zeig\s+(?:mir\s+)?)?meine\s+textbausteine$"));
static SNIPPET_DELETE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:delete|remove|forget)\s+(?:the\s+|my\s+)?snippet\s+(.+)$|^lösche?\s+den\s+textbaustein\s+(.+)$"));

fn snippet_key(name: &str) -> String {
    plain(name)
}

/// "Save the snippet address: …", "save my clipboard as snippet address", "type my address",
/// "list my snippets", "delete the snippet address".
pub fn snippet_command(s: &str, original: &str) -> Option<Value> {
    if SNIPPET_LIST.is_match(s) {
        return Some(json!({"action": "list"}));
    }
    if let Some(c) = SNIPPET_DELETE.captures(s) {
        return Some(json!({"action": "delete", "name": c.get(1).or(c.get(2))?.as_str().trim()}));
    }
    if let Some(c) = SNIPPET_CLIPBOARD.captures(s) {
        return Some(json!({"action": "save", "name": c.get(1).or(c.get(2))?.as_str().trim(), "from_clipboard": true}));
    }
    if let Some(c) = SNIPPET_SAVE.captures(original) {
        return Some(json!({"action": "save", "name": c.name("name")?.as_str().trim(), "text": c.name("text")?.as_str()}));
    }
    // "Type my address": only what is a snippet of his (anything else is not this order).
    let c = SNIPPET_TYPE.captures(s)?;
    let name = c.get(1).or(c.get(2))?.as_str().trim();
    crate::kit::get(SNIPPET, &snippet_key(name)).map(|_| json!({"action": "type", "name": name}))
}

pub async fn snippet(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let name = args["name"].as_str().unwrap_or("").trim();
    match args["action"].as_str().unwrap_or("") {
        "list" => {
            let all = crate::kit::list(SNIPPET);
            if all.is_empty() {
                return Ok(say("No snippets yet. Say \"save my clipboard as snippet address\", then \"type my address\".", "You have no snippets yet, Sir."));
            }
            let lines: Vec<String> = all.iter().map(|(_, v, _)| format!("{}  ·  {} characters", v["name"].as_str().unwrap_or("?"), v["text"].as_str().unwrap_or("").chars().count())).collect();
            crate::kit::show_text("kit_snippets", "Snippets // text I type for you", &format!("{}\n\nSay \"type my <name>\" to have one typed.", lines.join("\n")), [420.0, 320.0]);
            Ok(say(lines.join("\n"), format!("{} snippet{}, Sir. On the board.", all.len(), if all.len() == 1 { "" } else { "s" })))
        }
        "delete" => {
            if crate::kit::remove(SNIPPET, &snippet_key(name)) {
                Ok(say(format!("Snippet \"{name}\" deleted."), "Deleted, Sir."))
            } else {
                Err(format!("I have no snippet called \"{name}\", Sir."))
            }
        }
        "save" => {
            let text = if args["from_clipboard"] == true { crate::clipboard::read().await? } else { args["text"].as_str().unwrap_or("").to_string() };
            let text = text.trim_end_matches(['\r', '\n']).to_string();
            if name.is_empty() || text.trim().is_empty() {
                return Err(if text.trim().is_empty() { "There is nothing to keep, Sir: the text is empty.".to_string() } else { "What shall the snippet be called, Sir?".to_string() });
            }
            let replaced = crate::kit::get(SNIPPET, &snippet_key(name)).is_some();
            crate::kit::put(SNIPPET, &snippet_key(name), &json!({"name": name, "text": text}))?;
            Ok(say(format!("Snippet \"{name}\" {} ({} characters). Say \"type my {name}\".", if replaced { "replaced" } else { "kept" }, text.chars().count()), format!("Kept, Sir. Say \"type my {name}\" whenever you need it.")))
        }
        "type" => {
            let kept = crate::kit::get(SNIPPET, &snippet_key(name)).ok_or_else(|| format!("I have no snippet called \"{name}\", Sir."))?;
            let text = kept["text"].as_str().unwrap_or("").to_string();
            to_his_app(ctx).await?;
            let n = type_out(text).await?;
            Ok(say(format!("Typed \"{name}\" ({n} characters)."), "Typed, Sir."))
        }
        other => Err(format!("I do not know what to do with a snippet: '{other}'.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_is_asked_for_and_ended_in_his_words() {
        for text in ["dictate", "start dictating", "take a dictation", "type what i say", "diktat", "diktieren", "schreib mit"] {
            assert_eq!(dictation_command(text), Some(json!({"action": "start"})), "{text}");
        }
        for text in ["stop dictating", "end the dictation", "dictation off", "diktat beenden", "stopp das diktat"] {
            assert_eq!(dictation_command(text), Some(json!({"action": "stop"})), "{text}");
        }
        for text in ["dictate me a letter to my teacher", "what is a dictation", "stop", "stop the music", "quit"] {
            assert_eq!(dictation_command(text), None, "{text}");
        }
        // While one runs, the short ways end it too, with the small words around an order.
        for text in ["Stop dictating, please.", "Okay, stop dictating.", "Stop the dictation now", "Stop.", "Stop typing", "That's all.", "Jay, stop dictating.", "Diktat beenden", "Fertig."] {
            assert_eq!(dictated(text), Dictated::Stop, "{text}");
        }
        for text in ["Stop the car at the corner.", "The end of the story is sad.", "I am finished with school at two."] {
            assert!(matches!(dictated(text), Dictated::Type(_)), "{text}");
        }
    }

    #[test]
    fn a_dictated_sentence_is_typed_and_its_few_orders_are_orders() {
        assert_eq!(dictated("Dear Mr Example, thank you for your letter."), Dictated::Type("Dear Mr Example, thank you for your letter. ".to_string()));
        assert_eq!(dictated("New line."), Dictated::Type("\n".to_string()));
        assert_eq!(dictated("neuer Absatz"), Dictated::Type("\n\n".to_string()));
        assert_eq!(dictated("Delete that."), Dictated::DeleteLast);
        assert_eq!(dictated("Jay, stop dictating."), Dictated::Stop);
        assert_eq!(dictated("Diktat beenden"), Dictated::Stop);
        // A sentence that only holds such words is typed.
        assert!(matches!(dictated("Please open a new line of credit."), Dictated::Type(_)));
    }

    #[test]
    fn dictation_types_nothing_in_a_test_and_ends_on_his_word() {
        let _one = crate::kit::test_lock();
        set_dictating(false);
        assert!(!dictating());
        assert!(dictation(&json!({"action": "start"})).unwrap().contains("Spoken:"));
        assert!(dictating());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // (A test has its hands off the keyboard: the sentence is refused, dictation stays on.)
        assert_eq!(rt.block_on(dictate("Hello there.", false)), Err(HANDS_OFF.to_string()));
        assert!(rt.block_on(dictate("Hello there.", true)).unwrap_err().contains("own window"));
        assert_eq!(rt.block_on(dictate("Stop dictating.", false)), Ok("Dictation ended".to_string()));
        assert!(!dictating());
        assert!(dictation(&json!({"action": "stop"})).unwrap().contains("not taking dictation"));
        // Five minutes without a word end it.
        set_dictating(true);
        *lock(&HEARD_AT) = Instant::now().checked_sub(DICTATION_RESTS + Duration::from_secs(1));
        assert!(lock(&HEARD_AT).is_none() || !dictating());
        set_dictating(false);
    }

    #[test]
    fn the_keys_answer_to_their_names_in_english_and_german() {
        for (text, keys) in [
            ("next tab", "next_tab"),
            ("previous tab", "previous_tab"),
            ("new tab", "new_tab"),
            ("reopen the closed tab", "reopen_tab"),
            ("scroll down", "scroll_down"),
            ("scroll up a bit", "scroll_up_a_little"),
            ("go to the top", "top"),
            ("ganz nach unten", "bottom"),
            ("page back", "page_back"),
            ("reload the page", "reload"),
            ("seite neu laden", "reload"),
            ("select all", "select_all"),
            ("paste it", "paste"),
            ("press enter", "enter"),
        ] {
            assert_eq!(keys_command(text), Some(json!({"keys": keys})), "{text}");
        }
        assert_eq!(keys_command("search the page for photosynthesis"), Some(json!({"keys": "find", "text": "photosynthesis"})));
        assert_eq!(keys_command("find the answer on this page"), Some(json!({"keys": "find", "text": "the answer"})));
        // His own orders with the same words are not keys.
        // ("Close the tab" is the window tool's, and stays there.)
        for text in ["go back", "close it", "close the window", "close the tab", "next", "next song", "scroll", "reload yourself", "open a new window", "paste"] {
            assert_eq!(keys_command(text), None, "{text}");
        }
        // Every phrase names keys that exist once.
        let mut names: Vec<&str> = KEYS.iter().map(|k| k.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), KEYS.len());
    }

    #[test]
    fn snippets_are_kept_typed_listed_and_deleted_by_name() {
        let _one = crate::kit::test_lock();
        let said = "Jay, save the snippet address: 12 Example Road, 12345 Sampletown";
        let args = snippet_command(&plain(said), said).expect("a snippet to save");
        assert_eq!(args, json!({"action": "save", "name": "address", "text": "12 Example Road, 12345 Sampletown"}), "the text keeps its capitals and commas");
        assert_eq!(snippet_command("save my clipboard as snippet mail", "x"), Some(json!({"action": "save", "name": "mail", "from_clipboard": true})));
        // "Type my address" is an order only once there is such a snippet.
        assert_eq!(snippet_command("type my address", "type my address"), None);
        crate::kit::put(SNIPPET, "address", &json!({"name": "address", "text": "12 Example Road"})).unwrap();
        assert_eq!(snippet_command("type my address", "x"), Some(json!({"action": "type", "name": "address"})));
        assert_eq!(snippet_command("tippe meine address ein", "x"), Some(json!({"action": "type", "name": "address"})));
        assert_eq!(snippet_command("type hello world", "x"), None, "no snippet of that name");
        assert_eq!(snippet_command("list my snippets", "x"), Some(json!({"action": "list"})));
        assert_eq!(snippet_command("delete the snippet address", "x"), Some(json!({"action": "delete", "name": "address"})));
    }
}
