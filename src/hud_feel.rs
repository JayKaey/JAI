//! The look-and-feel features Sir chose one by one on 2026-11-12, as he asks for them: the
//! phrases that switch them (`command`, part of the fast path's `configure_interface`
//! arguments), the ones that are not settings but requests of the moment (`apply`: subtitles,
//! lyrics, tonight's night mode, "what did I miss?", the voice set-up) and what he says to each
//! (`spoken`). The settings themselves live in `InterfaceSettings`; what each feature draws and
//! does lives in its own module.

use chrono::{NaiveDateTime, Timelike};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn flag(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

// ------------------------------------------------------------------------------ phrases ----

/// A switch of the interface as Sir names it: its `configure_interface` key, the words that
/// name it, and the words that mean "off" for it on top of the general ones.
struct Switch {
    key: &'static str,
    names: &'static str,
}

/// (Names are alternations inside one group; the longest first where one contains another.)
const SWITCHES: [Switch; 11] = [
    Switch { key: "calm_hud", names: r"calm\s+mode|calm\s+hud|quiet\s+hud|ruhiger\s+modus|ruhemodus" },
    Switch { key: "live_words", names: r"live\s+words|live\s+captions?|live\s+transcript|my\s+words\s+(?:while|as)\s+i\s+(?:speak|talk)|live\s+mitschrift" },
    Switch { key: "startup_sequence", names: r"start\s*-?\s*up\s+sequence|boot\s+sequence|power\s*-?\s*up\s+sequence|startsequenz" },
    Switch { key: "idle_screen", names: r"idle\s+screen|calm\s+screen|screen\s*saver|ruhebildschirm" },
    Switch { key: "night_mode", names: r"automatic\s+night\s+mode|night\s+mode\s+by\s+the\s+clock|automatischer\s+nachtmodus" },
    Switch { key: "night_now", names: r"night\s+mode|nachtmodus" },
    Switch { key: "edge_light", names: r"edge\s+lights?|edge\s+glow|screen\s+edge\s+lights?|randlicht" },
    Switch { key: "music_visuals", names: r"music\s+visuals?|music\s+visuali[sz]er|visuali[sz]er|musikvisualisierung" },
    Switch { key: "mini_strip", names: r"mini\s+mode|mini\s+strip|mini\s+bar|minimodus" },
    Switch { key: "desktop_background", names: r"live\s+desktop(?:\s+background)?|desktop\s+background|live\s+wallpaper|live\s+hintergrund" },
    Switch { key: "subtitles", names: r"live\s+subtitles|subtitles|untertitel" },
];

/// "turn on night mode", "night mode off", "mini mode", "schalte den nachtmodus aus".
static SWITCH: LazyLock<Regex> = LazyLock::new(|| {
    let names: Vec<String> = SWITCHES.iter().map(|s| format!("(?P<{}>{})", s.key, s.names)).collect();
    Regex::new(&format!(
        r"^(?:(?:please\s+)?(?:turn|switch|put|go\s+(?:in)?to|enter|start|begin|open|show|use|give\s+me|schalte?|mach|starte?)\s+)?(?P<pre>on|off|an|aus|enable|disable|stop|leave|end|exit|close|hide|no|without|skip)?\s*(?:the\s+|your\s+|my\s+|a\s+|den\s+|die\s+|das\s+|deinen?\s+)?(?:{})(?:\s+(?P<post>on|off|an|aus|ein|again|please|now))?$",
        names.join("|")
    ))
    .expect("valid switch regex")
});

/// Phrases that are not "<switch> on/off": each (pattern, arguments).
static PHRASES: LazyLock<Vec<(Regex, Value)>> = LazyLock::new(|| {
    let p = |pattern: &str, args: Value| (Regex::new(pattern).expect("valid phrase regex"), args);
    vec![
        // Calm or dense.
        // ("Show everything" is the older order that brings every panel back: not this one's.)
        p(r"^(?:show\s+(?:me\s+)?(?:all\s+(?:the\s+)?readouts|the\s+readouts)|dense\s+(?:mode|hud)|busy\s+(?:mode|hud)|zeig\s+(?:mir\s+)?(?:alle\s+)?anzeigen)$", json!({"calm_hud": false})),
        p(r"^(?:hide\s+the\s+readouts|calm\s+(?:the\s+hud|down\s+the\s+hud|it\s+down)|less\s+on\s+(?:the\s+)?screen|blende?\s+die\s+anzeigen\s+aus|anzeigen\s+ausblenden|weniger\s+auf\s+dem\s+bildschirm)$", json!({"calm_hud": true})),
        p(r"^(?:stop\s+showing\s+my\s+words|don'?t\s+show\s+my\s+words|zeig\s+meine\s+worte\s+nicht(?:\s+mehr)?(?:\s+an)?|meine\s+worte\s+nicht\s+(?:mehr\s+)?(?:an)?zeigen)$", json!({"live_words": false})),
        p(r"^(?:show\s+(?:me\s+)?my\s+words(?:\s+(?:while|as)\s+i\s+(?:speak|talk))?|zeig\s+(?:mir\s+)?meine\s+worte(?:\s+(?:beim\s+sprechen|während\s+ich\s+(?:spreche|rede)))?)$", json!({"live_words": true})),
        // Night.
        p(r"^(?:day\s+mode|it'?s\s+(?:day|morning)|tagmodus)$", json!({"night_now": false})),
        p(r"^(?:never\s+(?:use|go\s+(?:in)?to)\s+night\s+mode|no\s+night\s+mode\s+by\s+(?:yourself|itself)|nie(?:mals)?\s+(?:von\s+(?:selbst|allein)\s+)?(?:in\s+den\s+)?nachtmodus|kein(?:en)?\s+nachtmodus\s+von\s+(?:selbst|allein))$", json!({"night_mode": false})),
        // Music.
        p(r"^(?:(?:move|dance|pulse)\s+(?:with|to)\s+the\s+music|(?:beweg|bewege|tanz|tanze|pulsiere?)\s+(?:dich\s+)?(?:mit|zu)\s+der\s+musik)$", json!({"music_visuals": true})),
        p(r"^(?:(?:stop|don'?t)\s+(?:moving|dancing|pulsing|move|dance|pulse)\s+(?:with|to)\s+the\s+music|(?:beweg|bewege|tanz|tanze)\s+(?:dich\s+)?nicht\s+(?:mehr\s+)?(?:mit|zu)\s+der\s+musik|h(?:ö|oe)r\s+auf\s+(?:dich\s+)?(?:mit|zu)\s+der\s+musik\s+zu\s+(?:bewegen|tanzen))$", json!({"music_visuals": false})),
        // Mini mode and the desktop.
        p(r"^(?:go\s+mini|shrink\s+to\s+(?:a|the)\s+strip|(?:werde?|mach\s+dich)\s+(?:klein|mini)|schrumpfe?\s+zur\s+leiste)$", json!({"mini_strip": true})),
        // (The background on every monitor or on one comes first: "background on all monitors"
        // is more than "background on".)
        p(
            r"^(?:(?:(?:put|show|throw|have|use|be)\s+)?(?:the\s+|your\s+|my\s+|your\s+live\s+|the\s+live\s+)?(?:desktop\s+)?(?:background|wallpaper)\s+(?:on|across|over|onto)\s+(?:all|every|each|all\s+(?:of\s+)?(?:the|my)|all\s+(?:two|three|four))\s+(?:monitors?|screens?|displays?)|(?:be\s+my\s+)?background\s+everywhere|(?:den\s+)?hintergrund\s+auf\s+(?:allen|alle|jedem)\s+(?:monitoren?|bildschirmen?|monitor|bildschirm))$",
            json!({"desktop_background": true, "background_all_monitors": true}),
        ),
        p(
            r"^(?:(?:(?:put|show|have|use|be)\s+)?(?:the\s+|your\s+|my\s+)?(?:desktop\s+)?(?:background|wallpaper)\s+(?:only\s+)?on\s+(?:one|the\s+main|my\s+main|the\s+first|my\s+first|the\s+primary|just\s+one|only\s+one)\s+(?:monitor|screen|display)(?:\s+only)?|(?:den\s+)?hintergrund\s+nur\s+auf\s+(?:einem|dem\s+ersten|meinem\s+ersten|dem\s+haupt)\s*(?:monitor|bildschirm))$",
            json!({"background_all_monitors": false}),
        ),
        p(
            r"^(?:(?:be|become)\s+(?:only\s+|just\s+)?my\s+(?:desktop\s+|pc\s+)?(?:background|wallpaper)|background\s+mode(?:\s+on)?|(?:go|get|move)\s+(?:back\s+)?(?:in)?to\s+the\s+background|live\s+in\s+the\s+background|(?:sei|werde?)\s+(?:nur\s+)?mein\s+(?:desktop\s*)?hintergrund(?:bild)?|hintergrundmodus(?:\s+an)?|geh\s+(?:wieder\s+)?in\s+den\s+hintergrund)$",
            json!({"desktop_background": true}),
        ),
        p(
            r"^(?:(?:stop\s+being|leave)\s+(?:my\s+|the\s+)(?:desktop\s+)?(?:background|wallpaper)|background\s+mode\s+off|board\s+mode|window\s+mode|be\s+a\s+window\s+again|(?:come\s+)?back\s+(?:to|as)\s+(?:the|a)\s+(?:board\s+)?window|come\s+out\s+of\s+the\s+background|sei\s+nicht\s+mehr\s+mein\s+(?:desktop\s*)?hintergrund(?:bild)?|verlass\s+(?:meinen|den)\s+(?:desktop\s*)?hintergrund|hintergrundmodus\s+aus|komm\s+aus\s+dem\s+hintergrund|sei\s+wieder\s+ein\s+fenster)$",
            json!({"desktop_background": false}),
        ),
        // His windows on call: all of them in front of the other programs' windows.
        p(
            r"^(?:(?:show|bring|get|give)\s+(?:me\s+)?(?:all\s+)?(?:of\s+)?your\s+windows(?:\s+(?:to\s+the\s+front|forward|up|back|here))?|your\s+windows\s+to\s+the\s+front|windows\s+to\s+the\s+front|(?:zeig|hol|bring)\s+(?:mir\s+)?(?:alle\s+)?deine\s+fenster(?:\s+(?:nach\s+vorne?|her|hervor))?|deine\s+fenster\s+nach\s+vorne?)$",
            json!({"windows_on_call": true}),
        ),
        // Subtitles.
        p(r"^(?:(?:caption|subtitle)\s+(?:this|that|it|the\s+video|the\s+stream|the\s+call)|untertitel\s+(?:für\s+)?(?:das|dieses\s+video|das\s+video|den\s+stream|den\s+anruf)|untertitle\s+das)$", json!({"subtitles": true})),
        p(r"^(?:translate\s+(?:the\s+)?subtitles|(?:english\s+subtitles|subtitles\s+in\s+english)|translated\s+subtitles|(?:caption|subtitle)\s+(?:this|that|it)\s+in\s+english|untertitel\s+auf\s+englisch)$", json!({"subtitles": true, "subtitles_translate": true})),
        p(r"^(?:stop\s+translating(?:\s+the\s+subtitles)?|subtitles\s+in\s+the\s+original|original\s+subtitles|h(?:ö|oe)r\s+auf\s+zu\s+(?:ü|ue)bersetzen|nicht\s+mehr\s+(?:ü|ue)bersetzen|untertitel\s+im\s+original|originale?\s+untertitel)$", json!({"subtitles": true, "subtitles_translate": false})),
        // Lyrics.
        p(r"^(?:(?:show|display|give)\s+(?:me\s+)?(?:the\s+)?lyrics|lyrics(?:\s+on|\s+please)?|sing\s+along|zeig\s+(?:mir\s+)?(?:den\s+)?(?:songtext|liedtext))$", json!({"lyrics": true})),
        p(r"^(?:(?:hide|stop|close|remove)\s+(?:the\s+)?lyrics|lyrics\s+off|no\s+(?:more\s+)?lyrics|(?:songtext|liedtext)\s+(?:aus|weg|ausblenden)|kein(?:en)?\s+(?:songtext|liedtext)(?:\s+mehr)?)$", json!({"lyrics": false})),
        // What he showed while Sir was not looking.
        p(r"^(?:what\s+did\s+i\s+miss|did\s+i\s+miss\s+(?:anything|something)|(?:show|open)\s+(?:me\s+)?(?:my\s+|the\s+)?(?:notifications|notification\s+history|missed\s+notifications)|was\s+habe\s+ich\s+verpasst)$", json!({"missed": true})),
        // His voice.
        p(r"^(?:l(?:ea|e|a)r?n(?:e)?\s+(?:to\s+know\s+)?my\s+voice|(?:start\s+|run\s+|open\s+)?(?:the\s+)?voice\s+set\s*-?\s*up|calibrate\s+(?:to\s+)?my\s+voice|tune\s+(?:in\s+)?to\s+my\s+voice|lerne?\s+meine\s+stimme(?:\s+kennen)?)$", json!({"voice_setup": true})),
        p(r"^(?:only\s+(?:obey|listen\s+to|answer(?:\s+to)?)\s+me|(?:obey|listen\s+to)\s+(?:only\s+)?(?:me|my\s+voice)\s+(?:only|alone)|listen\s+only\s+to\s+(?:me|my\s+voice)|voice\s+lock(?:\s+on)?|h(?:ö|oe)r\s+nur\s+auf\s+mich)$", json!({"voice_lock": true})),
        p(r"^(?:obey\s+(?:everyone|everybody|anyone)|listen\s+to\s+(?:everyone|everybody|anyone)(?:\s+again)?|voice\s+lock\s+off|h(?:ö|oe)r\s+auf\s+alle)$", json!({"voice_lock": false})),
        // Everything in one place.
        p(r"^(?:(?:open|show)\s+(?:me\s+)?(?:the\s+|your\s+|all\s+)?settings(?:\s+panel)?|settings|einstellungen(?:\s+(?:öffnen|zeigen))?)$", json!({"settings_panel": true})),
        // What is being said, beside the corner orb or not. (Sir, 2026-11-14.)
        p(
            r"^(?:show\s+(?:me\s+)?(?:the\s+)?(?:words|text|info(?:rmation)?(?:\s+window)?)\s+(?:beside|next\s+to|by|at)\s+(?:the\s+|your\s+)?orb|(?:corner\s+)?orb\s+(?:words|text|info(?:rmation)?)\s+on|(?:the\s+)?info(?:rmation)?\s+window\s+on|zeig\s+(?:mir\s+)?(?:den\s+)?text\s+neben\s+(?:der\s+kugel|dem\s+orb))$",
            json!({"orb_words": true}),
        ),
        p(
            r"^(?:hide\s+(?:the\s+)?(?:words|text|info(?:rmation)?(?:\s+window)?)\s+(?:beside|next\s+to|by|at)\s+(?:the\s+|your\s+)?orb|(?:corner\s+)?orb\s+(?:words|text|info(?:rmation)?)\s+off|(?:the\s+)?info(?:rmation)?\s+window\s+off|(?:the\s+)?(?:corner\s+)?orb\s+without\s+(?:the\s+|its\s+)?(?:words|text)|kein(?:en)?\s+text\s+neben\s+(?:der\s+kugel|dem\s+orb))$",
            json!({"orb_words": false}),
        ),
        // The clock on the desktop: there or not, its seconds, twelve hours or twenty-four, its date.
        p(r"^(?:(?:show|give)\s+(?:me\s+)?(?:a|the|your)\s+clock|(?:show|put)\s+(?:a|the)\s+clock\s+on\s+(?:the|my)\s+(?:desktop|screen|background)|(?:desktop\s+)?clock\s+on|zeig\s+(?:mir\s+)?(?:eine|die)\s+uhr|uhr\s+an)$", json!({"desk_clock": true})),
        p(r"^(?:(?:hide|remove)\s+the\s+clock|(?:desktop\s+)?clock\s+off|no\s+clock|blende?\s+die\s+uhr\s+aus|uhr\s+aus|keine\s+uhr)$", json!({"desk_clock": false})),
        p(r"^(?:(?:the\s+)?clock\s+with\s+(?:the\s+)?seconds|show\s+(?:me\s+)?the\s+seconds|(?:clock\s+)?seconds\s+on|uhr\s+mit\s+sekunden|sekunden\s+an)$", json!({"desk_clock_seconds": true})),
        p(r"^(?:(?:the\s+)?clock\s+without\s+(?:the\s+)?seconds|hide\s+the\s+seconds|no\s+seconds|(?:clock\s+)?seconds\s+off|uhr\s+ohne\s+sekunden|sekunden\s+aus)$", json!({"desk_clock_seconds": false})),
        p(r"^(?:(?:a\s+|the\s+)?(?:12|twelve)\s*-?\s*hour\s+clock|(?:the\s+)?clock\s+in\s+(?:12|twelve)\s+hours|am\s*pm\s+clock|(?:12|zw(?:ö|oe)lf)\s*-?\s*stunden\s*-?\s*uhr)$", json!({"desk_clock_12h": true})),
        p(r"^(?:(?:a\s+|the\s+)?(?:24|twenty\s*-?\s*four)\s*-?\s*hour\s+clock|(?:the\s+)?clock\s+in\s+(?:24|twenty\s*-?\s*four)\s+hours|24\s*-?\s*stunden\s*-?\s*uhr)$", json!({"desk_clock_12h": false})),
        p(r"^(?:(?:show\s+(?:me\s+)?)?the\s+date\s+(?:under|with|on|by)\s+the\s+clock|(?:the\s+)?clock\s+with\s+(?:the\s+)?date|(?:clock\s+)?date\s+on|uhr\s+mit\s+datum|datum\s+an)$", json!({"desk_clock_date": true})),
        p(r"^(?:hide\s+the\s+date|(?:the\s+)?clock\s+without\s+(?:the\s+)?date|no\s+date|(?:clock\s+)?date\s+off|uhr\s+ohne\s+datum|datum\s+aus)$", json!({"desk_clock_date": false})),
        // What comes next stands beside the emblem only when he is asked.
        p(
            r"^(?:what(?:'?s|\s+s|\s+is|\s+comes)\s+(?:up\s+)?next(?:\s+(?:today|for\s+me|on\s+my\s+(?:day|schedule)))?|what\s+do\s+i\s+have\s+next|show\s+(?:me\s+)?what(?:'?s|\s+s|\s+is|\s+comes)\s+next|was\s+(?:kommt|steht)\s+als\s+n(?:ä|ae)chstes(?:\s+an)?|was\s+steht\s+an)$",
            json!({"next_now": true}),
        ),
        // The corner orb's size.
        p(r"^(?:(?:make\s+)?(?:the\s+|your\s+)?corner\s+orb\s+(?:bigger|larger)|(?:a\s+)?(?:bigger|larger)\s+corner\s+orb)$", json!({"corner_orb_size": "bigger"})),
        p(r"^(?:(?:make\s+)?(?:the\s+|your\s+)?corner\s+orb\s+smaller|(?:a\s+)?smaller\s+corner\s+orb)$", json!({"corner_orb_size": "smaller"})),
        p(r"^(?:(?:the\s+|your\s+)?corner\s+orb\s+(?:size\s+)?(?:normal|back\s+to\s+normal)|reset\s+(?:the\s+|your\s+)?corner\s+orb(?:\s+size)?)$", json!({"corner_orb_size": "normal"})),
    ]
});

/// Guests are welcome for this many minutes when Sir names no time.
pub const GUEST_MINUTES: u64 = 10;

/// People he may be told to answer to for a while.
const GUEST: &str = r"friends?|guests?|brother|sister|mother|father|mum|mom|dad|parents|family|girlfriend|boyfriend|wife|husband|mates?|buddy|buddies|kumpel|freund|freundin|bruder|schwester|mama|papa";

/// "Guest mode", "let my friend speak", "let Tom talk to you for half an hour": with the voice
/// lock on, he answers to anyone for a while.
static GUESTS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^(?:guest\s+mode(?:\s+on)?|gastmodus(?:\s+an)?|(?:let|allow)\s+(?P<who>my\s+(?:{GUEST})|[a-zäöüß]+)\s+(?:speak|talk)(?:\s+to\s+you)?(?:\s+too|\s+as\s+well)?|(?:you\s+(?:can|may)\s+)?(?:listen|answer)\s+to\s+(?:him|her|them|my\s+(?:{GUEST}))(?:\s+too|\s+as\s+well)?)(?:\s+for\s+(?P<span>.+))?$"
    ))
    .expect("valid guest regex")
});
static GUESTS_OFF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:guest\s+mode\s+off|end\s+guest\s+mode|no\s+more\s+guests|(?:only|just)\s+me\s+again|gastmodus\s+aus|keine\s+g(?:ä|ae)ste\s+mehr|nur\s+(?:noch\s+)?ich\s+wieder|wieder\s+nur\s+ich)$").expect("valid guest regex"));
/// The same in German: "lass meinen Bruder sprechen", "du darfst auch auf sie hören".
static GUESTS_DE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^(?:(?:lass|lasse)\s+(?P<who>mein(?:e|en)?\s+(?:{GUEST})|[a-zäöüß]+)\s+(?:auch\s+)?(?:mit\s+dir\s+)?(?:sprechen|reden)|(?:du\s+(?:kannst|darfst)\s+)?(?:auch\s+)?auf\s+(?:ihn|sie|mein(?:e|en)?\s+(?:{GUEST}))\s+h(?:ö|oe)ren)$"
    ))
    .expect("valid guest regex")
});

/// The request for guests in `s`, as arguments: on with its minutes, or off.
fn guest_command(s: &str) -> Option<Value> {
    if GUESTS_OFF.is_match(s) {
        return Some(json!({"voice_guest": false}));
    }
    if let Some(c) = GUESTS_DE.captures(s) {
        // ("Lass mich sprechen", "lass es reden": no guest.)
        if c.name("who").is_some_and(|who| matches!(who.as_str(), "mich" | "uns" | "es" | "das" | "dich" | "ihn" | "sie")) {
            return None;
        }
        return Some(json!({"voice_guest": true, "voice_guest_minutes": GUEST_MINUTES}));
    }
    let c = GUESTS.captures(s)?;
    // ("Let me speak", "let it talk": no guest.)
    if c.name("who").is_some_and(|who| matches!(who.as_str(), "me" | "us" | "it" | "that" | "this" | "you" | "the" | "a")) {
        return None;
    }
    let minutes = match c.name("span") {
        Some(span) => (crate::listening_lock::duration_in(span.as_str())?.as_secs() / 60).clamp(1, 720),
        None => GUEST_MINUTES,
    };
    Some(json!({"voice_guest": true, "voice_guest_minutes": minutes}))
}

/// A request for one of these features as `configure_interface` arguments (`s`: the fast
/// path's normalised phrase).
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim();
    if let Some(args) = guest_command(s) {
        return Some(args);
    }
    if let Some((_, args)) = PHRASES.iter().find(|(pattern, _)| pattern.is_match(s)) {
        // (What comes next is his to say only from a glance at Sir's day that tells it.)
        if args["next_now"] == true && next_now().is_none() {
            return None;
        }
        return Some(args.clone());
    }
    let c = SWITCH.captures(s)?;
    let key = SWITCHES.iter().map(|sw| sw.key).find(|key| c.name(key).is_some())?;
    let word = |name: &str| c.name(name).map(|m| m.as_str());
    let off = |w: &str| matches!(w, "off" | "aus" | "disable" | "stop" | "leave" | "end" | "exit" | "close" | "hide" | "no" | "without" | "skip");
    let on = !(word("pre").is_some_and(off) || word("post").is_some_and(off));
    Some(json!({ key: on }))
}

// ---------------------------------------------------------------------- a game in front ----

/// Whether a game (or another program that fills its screen) is in front: the live words and
/// the subtitles pause then, so they cost the game nothing. Looked up at most every 3 s.
pub fn game_in_front() -> bool {
    // (A scripted session can ask to be looked at as it is with no game in front, while Sir
    // plays on his other monitor: `JAI_DEMO=no-game`.)
    if crate::platform::demo("no-game") {
        return false;
    }
    static SEEN: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);
    let mut seen = lock(&SEEN);
    if let Some((at, gaming)) = *seen {
        if at.elapsed() < std::time::Duration::from_secs(3) {
            return gaming;
        }
    }
    let gaming = game_in_front_now();
    *seen = Some((std::time::Instant::now(), gaming));
    gaming
}

#[cfg(windows)]
fn game_in_front_now() -> bool {
    let (process, own) = crate::win32::foreground_info().unwrap_or_default();
    !own && (is_game(&process) || (crate::win32::foreground_is_fullscreen() && !is_player(&process)))
}

#[cfg(not(windows))]
fn game_in_front_now() -> bool {
    false
}

/// A browser or a media player: full screen, it shows a film, not a game.
pub fn is_player(process: &str) -> bool {
    let p = process.to_lowercase();
    let p = p.trim_end_matches(".exe");
    ["firefox", "chrome", "msedge", "opera", "brave", "vivaldi", "vlc", "mpv", "mpc-hc64", "mpc-hc", "mpc-be64", "wmplayer", "spotify", "sonora", "netflix", "video.ui", "microsoft.media.player", "potplayermini64", "twitch"].contains(&p)
}

/// A game's process (the listed games; a voice chat in front is no game).
pub fn is_game(process: &str) -> bool {
    let p = process.to_lowercase();
    let p = p.trim_end_matches(".exe");
    crate::ears::is_game_or_voice_chat(p) && !["discord", "discordptb", "discordcanary", "teamspeak", "ts3client_win64", "ts3client", "mumble", "guilded"].contains(&p)
}

// ------------------------------------------------------------------- requests of the moment ----

/// The arguments that are requests of the moment, not settings (`interface::configure` names
/// them in its summary; `apply` carries them out).
pub const RUNTIME_KEYS: [(&str, &str); 9] = [
    ("next_now", "what comes next"),
    ("night_now", "night mode"),
    ("subtitles", "subtitles"),
    ("subtitles_translate", "subtitle translation"),
    ("lyrics", "lyrics"),
    ("missed", "notification history"),
    ("voice_setup", "voice set-up"),
    ("settings_panel", "settings panel"),
    ("windows_on_call", "his windows in front"),
];

/// Arguments that have a phrase and no line in the tool's schema: the model's prompt is full
/// (`brain::tests::the_whole_prompt_fits_the_context_window`), and these are said in words the
/// fast path knows.
#[cfg(test)]
pub const UNLISTED: [&str; 9] = ["background_all_monitors", "windows_on_call", "orb_words", "desk_clock", "desk_clock_seconds", "desk_clock_12h", "desk_clock_date", "next_now", "corner_orb_size"];

/// How long what comes next stands beside the emblem once Sir asked for it.
const NEXT_SHOWN: std::time::Duration = std::time::Duration::from_secs(10);
/// Until when it stands there. (Sir, 2026-11-14: "I don't want the next thing on my screen".)
static NEXT_UNTIL: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Whether what comes next is on show right now ("what's next?").
pub fn next_shown() -> bool {
    lock(&NEXT_UNTIL).is_some_and(|until| std::time::Instant::now() < until)
}

/// What comes next as he says it: the lesson and the appointment of a glance at Sir's day, all
/// still ahead at `now`. None where the glance cannot tell: nothing in it, or a lesson or an
/// appointment in it has begun since and hides the one after it (the model asks the calendar
/// and the timetable).
pub fn next_spoken(glance: &crate::idle_screen::Glance, now: NaiveDateTime) -> Option<String> {
    let known: Vec<_> = [&glance.lesson, &glance.next].into_iter().flatten().collect();
    if known.is_empty() || known.iter().any(|(_, start, _)| *start <= now) {
        return None;
    }
    let ahead: Vec<String> = known.iter().map(|(title, start, place)| crate::idle_screen::next_line(title, *start, place, now).replace("  ·  ", ", ")).collect();
    Some(format!("Next, Sir: {}.", ahead.join("; then ")))
}

/// What comes next, from the last glance at Sir's day while it is fresh.
fn next_now() -> Option<String> {
    next_spoken(&crate::idle_screen::fresh_glance()?, chrono::Local::now().naive_local())
}

/// Tonight's night mode as Sir set it by hand: (on, the morning it lapses).
static NIGHT_OVERRIDE: Mutex<Option<(bool, NaiveDateTime)>> = Mutex::new(None);
/// Live subtitles: wanted, and whether translated into English.
static SUBTITLES: AtomicBool = AtomicBool::new(false);
static SUBTITLES_TRANSLATE: AtomicBool = AtomicBool::new(false);
static LYRICS: AtomicBool = AtomicBool::new(false);
static MISSED: AtomicBool = AtomicBool::new(false);
static VOICE_SETUP: AtomicBool = AtomicBool::new(false);
static SETTINGS_PANEL: AtomicBool = AtomicBool::new(false);

/// Night runs from this hour to that one.
pub const NIGHT_FROM: u32 = 22;
pub const NIGHT_UNTIL: u32 = 6;

/// The next time the clock shows `NIGHT_UNTIL` after `now`.
fn next_morning(now: NaiveDateTime) -> NaiveDateTime {
    let morning = now.date().and_hms_opt(NIGHT_UNTIL, 0, 0).unwrap_or(now);
    if now < morning { morning } else { morning + chrono::Duration::days(1) }
}

/// Whether it is night for him at `now`: by the clock (22:00 to 6:00) when `by_clock` is on,
/// unless Sir said otherwise for tonight ("night mode off" holds until the morning, "night mode"
/// at noon holds until the next one).
pub fn night_at(now: NaiveDateTime, by_clock: bool) -> bool {
    let mut held = lock(&NIGHT_OVERRIDE);
    if held.is_some_and(|(_, until)| now >= until) {
        *held = None;
    }
    match *held {
        Some((on, _)) => on,
        None => by_clock && (now.hour() >= NIGHT_FROM || now.hour() < NIGHT_UNTIL),
    }
}

/// Whether it is night for him now.
pub fn night(by_clock: bool) -> bool {
    night_at(chrono::Local::now().naive_local(), by_clock)
}

/// Whether night mode goes by the clock (the setting, as the HUD last saw it), and whether it
/// is night right now as of the last look (`night_active`): the sound thread reads the latter.
static NIGHT_BY_CLOCK: AtomicBool = AtomicBool::new(true);
static NIGHT_NOW: AtomicBool = AtomicBool::new(false);

pub fn set_night_by_clock(on: bool) {
    NIGHT_BY_CLOCK.store(on, Ordering::Release);
}

/// Whether it is night for him right now (looked up, and noted for `night_gain`). Never in a
/// test session: what a scripted session checks does not depend on the hour it runs at.
pub fn night_active() -> bool {
    let night = !crate::platform::in_test_session() && night(NIGHT_BY_CLOCK.load(Ordering::Acquire));
    NIGHT_NOW.store(night, Ordering::Release);
    night
}

/// How loud he is at night against the day (his voice and nothing else: the cues are off).
pub const NIGHT_GAIN: f32 = 0.6;

/// The share of his voice's volume right now (as of the last `night_active`).
pub fn night_gain() -> f32 {
    if NIGHT_NOW.load(Ordering::Relaxed) { NIGHT_GAIN } else { 1.0 }
}

/// What the language model is told at night.
pub fn live_context() -> String {
    if night_active() { "NIGHT MODE: it is late and Sir wants quiet. Answer in one short sentence.\n".to_string() } else { String::new() }
}

pub fn set_night(on: bool, now: NaiveDateTime) {
    *lock(&NIGHT_OVERRIDE) = Some((on, next_morning(now)));
}

/// Live subtitles wanted: Some(translate into English).
pub fn subtitles() -> Option<bool> {
    SUBTITLES.load(Ordering::Acquire).then(|| SUBTITLES_TRANSLATE.load(Ordering::Acquire))
}

pub fn set_subtitles(on: bool) {
    SUBTITLES.store(on, Ordering::Release);
}

pub fn lyrics() -> bool {
    LYRICS.load(Ordering::Acquire)
}

pub fn set_lyrics(on: bool) {
    LYRICS.store(on, Ordering::Release);
}

/// The playing song's current line, while the lyrics are on (published by the lyrics module).
static LYRIC_LINE: Mutex<Option<String>> = Mutex::new(None);

pub fn lyric_line() -> Option<String> {
    lyrics().then(|| lock(&LYRIC_LINE).clone()).flatten()
}

pub fn publish_lyric_line(line: Option<String>) {
    *lock(&LYRIC_LINE) = line;
}

/// "What did I miss?" was asked (once).
pub fn take_missed() -> bool {
    MISSED.swap(false, Ordering::AcqRel)
}

/// "Learn my voice" was asked (once).
pub fn take_voice_setup() -> bool {
    VOICE_SETUP.swap(false, Ordering::AcqRel)
}

/// The settings panel was asked for (once).
pub fn take_settings_panel() -> bool {
    SETTINGS_PANEL.swap(false, Ordering::AcqRel)
}

pub fn open_settings_panel() {
    SETTINGS_PANEL.store(true, Ordering::Release);
}

/// Whether Sir's voice print exists (the voice set-up has been done): without it the voice
/// lock cannot be switched on.
static VOICE_KNOWN: AtomicBool = AtomicBool::new(false);

pub fn voice_known() -> bool {
    VOICE_KNOWN.load(Ordering::Acquire)
}

pub fn set_voice_known(known: bool) {
    VOICE_KNOWN.store(known, Ordering::Release);
}

/// Carries out the requests of the moment in `configure_interface`'s arguments.
pub fn apply(args: &Value) {
    if let Some(on) = flag(args, "night_now") {
        set_night(on, chrono::Local::now().naive_local());
    }
    if let Some(on) = flag(args, "subtitles") {
        set_subtitles(on);
    }
    if let Some(on) = flag(args, "subtitles_translate") {
        SUBTITLES_TRANSLATE.store(on, Ordering::Release);
    }
    if let Some(on) = flag(args, "lyrics") {
        set_lyrics(on);
    }
    if flag(args, "missed") == Some(true) {
        MISSED.store(true, Ordering::Release);
    }
    if flag(args, "voice_setup") == Some(true) {
        VOICE_SETUP.store(true, Ordering::Release);
    }
    if flag(args, "settings_panel") == Some(true) {
        open_settings_panel();
    }
    if flag(args, "windows_on_call") == Some(true) {
        crate::gui::windows_on_call();
    }
    if flag(args, "next_now") == Some(true) {
        *lock(&NEXT_UNTIL) = Some(std::time::Instant::now() + NEXT_SHOWN);
        crate::gui::refresh();
    }
    if let Some(on) = flag(args, "voice_guest") {
        if on {
            let minutes = args.get("voice_guest_minutes").and_then(Value::as_u64).unwrap_or(GUEST_MINUTES);
            crate::speaker_id::welcome_guests(std::time::Duration::from_secs(minutes * 60));
        } else {
            crate::speaker_id::end_guests();
        }
    }
    // ("Only obey me" ends a guest's welcome with it.)
    if flag(args, "voice_lock") == Some(true) {
        crate::speaker_id::end_guests();
    }
}

// ------------------------------------------------------------------------- what he says ----

/// His answer to a request for one of these features (None: not one of them).
pub fn spoken(args: &Value) -> Option<String> {
    let on = |key: &str| flag(args, key);
    let say = |yes: &str, no: &str, state: bool| Some(if state { yes } else { no }.to_string());
    if let Some(v) = on("calm_hud") {
        return say("Calm it is, Sir. The readouts step back until you reach for them.", "Everything on screen, Sir.", v);
    }
    if let Some(v) = on("live_words") {
        return say("You'll see your words as you say them, Sir.", "Very well, Sir. Your words appear once you have finished.", v);
    }
    if let Some(v) = on("startup_sequence") {
        return say("The start-up sequence is back, Sir.", "Straight to work from now on, Sir. No start-up sequence.", v);
    }
    if let Some(v) = on("idle_screen") {
        return say("I'll dim down to the clock when you are away, Sir.", "The board stays as it is when you are away, Sir.", v);
    }
    if let Some(v) = on("night_mode") {
        return say("Night mode from ten to six, Sir, by the clock.", "I'll leave night mode to you, Sir.", v);
    }
    if let Some(v) = on("night_now") {
        return say("Night mode, Sir. Dim and quiet until the morning.", "Night mode off for tonight, Sir.", v);
    }
    if let Some(v) = on("edge_light") {
        return say("The edge light is on, Sir.", "Edge light off, Sir.", v);
    }
    if let Some(v) = on("music_visuals") {
        return say("I'll move with the music, Sir.", "I'll hold still, Sir.", v);
    }
    if let Some(v) = on("mini_strip") {
        return say("Mini mode, Sir. I'm the strip at the top of your screen.", "Back to the full board, Sir.", v);
    }
    if let (Some(all), None | Some(true)) = (on("background_all_monitors"), on("desktop_background")) {
        return say("On every monitor, Sir.", "On your first monitor only, Sir.", all);
    }
    if let Some(v) = on("desktop_background") {
        return say("I'm your desktop background now, Sir. My windows are yours to move, and I come out when you call.", "Back on the board, Sir.", v);
    }
    if on("windows_on_call") == Some(true) {
        return Some("My windows, Sir, in front of the rest.".to_string());
    }
    if let Some(v) = on("subtitles_translate").filter(|_| on("subtitles") != Some(false)) {
        return say("Subtitles in English, Sir.", "Subtitles in the language spoken, Sir.", v);
    }
    if let Some(v) = on("subtitles") {
        return say("Subtitles on, Sir.", "Subtitles off, Sir.", v);
    }
    if let Some(v) = on("lyrics") {
        return say("The lyrics, Sir, as the song goes.", "Lyrics off, Sir.", v);
    }
    if on("missed") == Some(true) {
        return Some("Here is what came up, Sir.".to_string());
    }
    if on("voice_setup") == Some(true) {
        return Some("Let us get acquainted, Sir. Read me the lines on the screen.".to_string());
    }
    if let Some(v) = on("voice_guest") {
        let minutes = args.get("voice_guest_minutes").and_then(Value::as_u64).unwrap_or(GUEST_MINUTES);
        let span = match minutes {
            1 => "minute".to_string(),
            60 => "hour".to_string(),
            m if m % 60 == 0 => format!("{} hours", m / 60),
            m => format!("{m} minutes"),
        };
        return Some(if v { format!("For the next {span} I'll answer to anyone, Sir. After that to you alone again.") } else { "Only you again, Sir.".to_string() });
    }
    if let Some(v) = on("voice_lock") {
        if v && !voice_known() {
            return Some("I'll answer to your voice alone, Sir, as soon as I know it. A few more orders by name will do, or say \"learn my voice\" and read me six lines.".to_string());
        }
        return say("From now on I answer to your voice alone, Sir.", "I'll answer to anyone again, Sir.", v);
    }
    if on("settings_panel") == Some(true) {
        return Some("Everything I can be set to, Sir, in one place.".to_string());
    }
    if let Some(v) = on("orb_words") {
        return say("What is said stands beside the orb again, Sir.", "Just the orb from now on, Sir.", v);
    }
    if let Some(v) = on("desk_clock_seconds") {
        return say("With its seconds, Sir.", "Without the seconds, Sir.", v);
    }
    if let Some(v) = on("desk_clock_12h") {
        return say("Twelve hours it is, Sir.", "Twenty-four hours it is, Sir.", v);
    }
    if let Some(v) = on("desk_clock_date") {
        return say("The date stands under the clock, Sir.", "The clock alone, Sir.", v);
    }
    if let Some(v) = on("desk_clock") {
        return say("The clock is on your desktop, Sir. Drag it wherever you like.", "The clock is gone, Sir.", v);
    }
    if on("next_now") == Some(true) {
        return Some(next_now().unwrap_or_else(|| "I can't tell from here what comes next, Sir.".to_string()));
    }
    if let Some(size) = args.get("corner_orb_size").and_then(Value::as_str) {
        return Some(match size {
            "normal" => "The corner orb is its usual size, Sir.",
            "smaller" => "Smaller, Sir.",
            _ => "Larger, Sir.",
        }
        .to_string());
    }
    None
}

/// The tool schema's lines for these arguments (appended to `configure_interface`'s).
pub fn schema() -> Vec<(&'static str, Value)> {
    // Each has its spoken phrase on the fast path; the model needs the names, and a word only
    // where a name does not say it. (Every character here is in every prompt: with a line of
    // description each, the prompt no longer fitted the fallback's window.)
    let plain = || json!({"type": "boolean"});
    let b = |description: &str| json!({"type": "boolean", "description": description});
    vec![
        ("calm_hud", plain()),
        ("live_words", plain()),
        ("startup_sequence", plain()),
        ("idle_screen", plain()),
        ("night_mode", b("Night mode by the clock.")),
        ("night_now", b("Night mode now.")),
        ("edge_light", plain()),
        ("music_visuals", plain()),
        ("mini_strip", plain()),
        ("desktop_background", plain()),
        ("subtitles", plain()),
        ("subtitles_translate", plain()),
        ("lyrics", plain()),
        ("missed", b("What Sir missed.")),
        ("voice_setup", plain()),
        ("voice_lock", b("Obey only Sir's voice.")),
        ("settings_panel", plain()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn says(text: &str) -> Option<Value> {
        command(&text.to_lowercase())
    }

    #[test]
    fn the_features_answer_to_their_names() {
        // ("What's next" with a fresh glance at Sir's day that tells it.)
        let _turn = crate::idle_screen::GLANCE_TURN.lock().unwrap_or_else(|e| e.into_inner());
        crate::idle_screen::show_glance(crate::idle_screen::Glance { lesson: Some(("Physics".into(), chrono::Local::now().naive_local() + chrono::Duration::minutes(25), "204".into())), ..Default::default() });
        for (text, want) in [
            ("night mode", json!({"night_now": true})),
            ("turn on night mode", json!({"night_now": true})),
            ("night mode off", json!({"night_now": false})),
            ("turn off night mode", json!({"night_now": false})),
            ("schalte den nachtmodus aus", json!({"night_now": false})),
            ("day mode", json!({"night_now": false})),
            ("automatic night mode off", json!({"night_mode": false})),
            ("never use night mode", json!({"night_mode": false})),
            ("night mode by the clock", json!({"night_mode": true})),
            ("calm mode", json!({"calm_hud": true})),
            ("calm mode off", json!({"calm_hud": false})),
            ("show the readouts", json!({"calm_hud": false})),
            ("hide the readouts", json!({"calm_hud": true})),
            ("live words off", json!({"live_words": false})),
            ("show my words while i speak", json!({"live_words": true})),
            ("skip the start-up sequence", json!({"startup_sequence": false})),
            ("startup sequence on", json!({"startup_sequence": true})),
            ("idle screen off", json!({"idle_screen": false})),
            ("edge light on", json!({"edge_light": true})),
            ("turn off the edge light", json!({"edge_light": false})),
            ("move with the music", json!({"music_visuals": true})),
            ("stop moving with the music", json!({"music_visuals": false})),
            ("music visuals off", json!({"music_visuals": false})),
            ("mini mode", json!({"mini_strip": true})),
            ("go mini", json!({"mini_strip": true})),
            ("leave mini mode", json!({"mini_strip": false})),
            ("mini mode off", json!({"mini_strip": false})),
            ("live desktop on", json!({"desktop_background": true})),
            ("be my desktop background", json!({"desktop_background": true})),
            ("be only my pc background", json!({"desktop_background": true})),
            ("background mode", json!({"desktop_background": true})),
            ("go back to the background", json!({"desktop_background": true})),
            ("geh in den hintergrund", json!({"desktop_background": true})),
            ("desktop background off", json!({"desktop_background": false})),
            ("board mode", json!({"desktop_background": false})),
            ("be a window again", json!({"desktop_background": false})),
            ("komm aus dem hintergrund", json!({"desktop_background": false})),
            // (Sir, 2026-11-13, as Whisper heard him: "… enables the background on all three monitors".)
            ("the background on all three monitors", json!({"desktop_background": true, "background_all_monitors": true})),
            ("put your background on every monitor", json!({"desktop_background": true, "background_all_monitors": true})),
            ("hintergrund auf allen monitoren", json!({"desktop_background": true, "background_all_monitors": true})),
            ("background only on the main monitor", json!({"background_all_monitors": false})),
            ("hintergrund nur auf einem monitor", json!({"background_all_monitors": false})),
            ("show your windows", json!({"windows_on_call": true})),
            ("bring all your windows to the front", json!({"windows_on_call": true})),
            ("zeig mir deine fenster", json!({"windows_on_call": true})),
            ("subtitles on", json!({"subtitles": true})),
            ("turn on subtitles", json!({"subtitles": true})),
            ("subtitles off", json!({"subtitles": false})),
            ("untertitel aus", json!({"subtitles": false})),
            ("caption this", json!({"subtitles": true})),
            ("translate the subtitles", json!({"subtitles": true, "subtitles_translate": true})),
            ("subtitles in english", json!({"subtitles": true, "subtitles_translate": true})),
            ("show the lyrics", json!({"lyrics": true})),
            ("show me the lyrics", json!({"lyrics": true})),
            ("hide the lyrics", json!({"lyrics": false})),
            ("lyrics off", json!({"lyrics": false})),
            ("what did i miss", json!({"missed": true})),
            ("show my notifications", json!({"missed": true})),
            ("learn my voice", json!({"voice_setup": true})),
            // (Typed in a hurry: seen live 2026-11-12, where it went to the model and came back as another tool.)
            ("lern my voice", json!({"voice_setup": true})),
            ("lerne meine stimme", json!({"voice_setup": true})),
            ("voice setup", json!({"voice_setup": true})),
            ("only obey me", json!({"voice_lock": true})),
            ("listen only to my voice", json!({"voice_lock": true})),
            ("obey everyone", json!({"voice_lock": false})),
            ("open settings", json!({"settings_panel": true})),
            ("show me all settings", json!({"settings_panel": true})),
            // 2026-11-14: the plate beside the corner orb, the desktop clock, what comes next
            // only when asked, the corner orb's size.
            ("hide the words beside the orb", json!({"orb_words": false})),
            ("the orb without its words", json!({"orb_words": false})),
            ("info window off", json!({"orb_words": false})),
            ("show the words next to the orb", json!({"orb_words": true})),
            ("the information window on", json!({"orb_words": true})),
            ("show me a clock", json!({"desk_clock": true})),
            ("clock on", json!({"desk_clock": true})),
            ("hide the clock", json!({"desk_clock": false})),
            ("uhr aus", json!({"desk_clock": false})),
            ("clock with seconds", json!({"desk_clock_seconds": true})),
            ("seconds off", json!({"desk_clock_seconds": false})),
            ("12 hour clock", json!({"desk_clock_12h": true})),
            ("twenty four hour clock", json!({"desk_clock_12h": false})),
            ("clock with the date", json!({"desk_clock_date": true})),
            ("clock without the date", json!({"desk_clock_date": false})),
            ("what's next", json!({"next_now": true})),
            ("whats next", json!({"next_now": true})),
            ("what comes next today", json!({"next_now": true})),
            ("was steht an", json!({"next_now": true})),
            ("make the corner orb smaller", json!({"corner_orb_size": "smaller"})),
            ("bigger corner orb", json!({"corner_orb_size": "bigger"})),
            ("corner orb size normal", json!({"corner_orb_size": "normal"})),
        ] {
            assert_eq!(says(text), Some(want), "{text}");
        }
    }

    /// What comes next is said from the last glance at Sir's day, and only what is still ahead.
    #[test]
    fn what_comes_next_is_what_is_still_ahead() {
        let at = |h: u32, m: u32| chrono::NaiveDate::from_ymd_opt(2026, 11, 12).unwrap().and_hms_opt(h, m, 0).unwrap();
        let glance = crate::idle_screen::Glance { lesson: Some(("Physics".into(), at(8, 35), "204".into())), next: Some(("Dentist".into(), at(14, 30), String::new())), weather: None };
        assert_eq!(next_spoken(&glance, at(8, 10)).as_deref(), Some("Next, Sir: Physics in 25 min, 204; then Dentist at 14:30."));
        let appointment = crate::idle_screen::Glance { lesson: None, ..glance.clone() };
        assert_eq!(next_spoken(&appointment, at(9, 0)).as_deref(), Some("Next, Sir: Dentist at 14:30."));
        // Physics has begun, and the lesson after it is not in the glance: no answer from it.
        assert_eq!(next_spoken(&glance, at(9, 0)), None);
        assert_eq!(next_spoken(&glance, at(15, 0)), None);
        assert_eq!(next_spoken(&crate::idle_screen::Glance::default(), at(8, 0)), None);
        // Asked where the last glance cannot tell (a lesson in it has begun, and the one after
        // it is not in it), it is not his to say: the model asks the calendar and the timetable.
        let _turn = crate::idle_screen::GLANCE_TURN.lock().unwrap_or_else(|e| e.into_inner());
        let now = chrono::Local::now().naive_local();
        crate::idle_screen::show_glance(crate::idle_screen::Glance { lesson: Some(("Physics".into(), now - chrono::Duration::minutes(5), "204".into())), ..Default::default() });
        assert_eq!(says("what's next"), None);
        crate::idle_screen::show_glance(crate::idle_screen::Glance { lesson: Some(("Physics".into(), now + chrono::Duration::minutes(25), "204".into())), ..Default::default() });
        assert_eq!(says("what's next"), Some(json!({"next_now": true})));
        // The next thing on a list is the list's.
        assert_ne!(says("what's next on my list"), Some(json!({"next_now": true})));
        // Asked for, it stands for a few seconds; nobody asked: it is not on the screen.
        let _one = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *lock(&NEXT_UNTIL) = None;
        assert!(!next_shown());
        apply(&json!({"next_now": true}));
        assert!(next_shown());
        *lock(&NEXT_UNTIL) = Some(std::time::Instant::now());
        assert!(!next_shown(), "its time is up");
    }

    /// Talk about these things is not an order for them.
    #[test]
    fn the_same_orders_in_german() {
        for (said, want) in [
            ("blende die anzeigen aus", json!({"calm_hud": true})),
            ("zeig meine worte nicht mehr", json!({"live_words": false})),
            ("zeig mir meine worte beim sprechen", json!({"live_words": true})),
            ("nie von selbst in den nachtmodus", json!({"night_mode": false})),
            ("beweg dich mit der musik", json!({"music_visuals": true})),
            ("tanz nicht mehr zu der musik", json!({"music_visuals": false})),
            ("mach dich klein", json!({"mini_strip": true})),
            ("sei mein hintergrund", json!({"desktop_background": true})),
            ("werde mein desktophintergrund", json!({"desktop_background": true})),
            ("sei nicht mehr mein hintergrund", json!({"desktop_background": false})),
            ("untertitel für das video", json!({"subtitles": true})),
            ("hör auf zu übersetzen", json!({"subtitles": true, "subtitles_translate": false})),
            ("songtext aus", json!({"lyrics": false})),
            ("gastmodus", json!({"voice_guest": true, "voice_guest_minutes": GUEST_MINUTES})),
            ("lass meinen bruder sprechen", json!({"voice_guest": true, "voice_guest_minutes": GUEST_MINUTES})),
            ("du darfst auch auf sie hören", json!({"voice_guest": true, "voice_guest_minutes": GUEST_MINUTES})),
            ("keine gäste mehr", json!({"voice_guest": false})),
        ] {
            assert_eq!(command(said), Some(want), "{said}");
        }
        // No guests: himself, and things.
        for said in ["lass mich sprechen", "lass es reden", "lass das", "hör auf", "sei still"] {
            assert_eq!(command(said), None, "{said}");
        }
    }

    #[test]
    fn talk_about_them_is_no_order() {
        for text in [
            "is night mode on",
            "what is night mode",
            "i like the night",
            "the subtitles of this film are bad",
            "who wrote the lyrics",
            "what are the lyrics about",
            "open the settings of firefox",
            "the music is too loud",
            "miss you",
            "obey",
            "my voice is hoarse",
            "mini golf",
            "calm down",
            "what time is it",
            "the clock is wrong",
            "what is the next song",
            "next",
            "set an alarm clock",
            "make the orb smaller",
            "what is an orb",
            // ("Just the orb" is the older order for the floating orb: not this one's.)
            "just the orb",
            "orb only",
        ] {
            assert_eq!(says(text), None, "{text}");
        }
    }

    #[test]
    fn every_request_has_an_answer_and_a_schema_line() {
        let keys: Vec<&str> = schema().iter().map(|(key, _)| *key).collect();
        for sw in &SWITCHES {
            assert!(keys.contains(&sw.key), "{} has no schema line", sw.key);
            assert!(spoken(&json!({ sw.key: true })).is_some() && spoken(&json!({ sw.key: false })).is_some(), "{}", sw.key);
        }
        for (_, args) in PHRASES.iter() {
            assert!(spoken(args).is_some(), "{args}");
            assert!(args.as_object().unwrap().keys().all(|k| keys.contains(&k.as_str()) || UNLISTED.contains(&k.as_str())), "{args}");
        }
        for (key, _) in RUNTIME_KEYS {
            assert!(keys.contains(&key) || UNLISTED.contains(&key), "{key}");
        }
        assert_eq!(spoken(&json!({"accent_color": "red"})), None);
    }

    /// Night by the clock runs from ten to six; "night mode off" holds until the morning and
    /// "night mode" at noon until the next one; the automatic can be switched off.
    #[test]
    fn night_follows_the_clock_until_sir_says_otherwise() {
        let _one = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *lock(&NIGHT_OVERRIDE) = None;
        let at = |day: u32, h: u32, m: u32| chrono::NaiveDate::from_ymd_opt(2026, 11, day).unwrap().and_hms_opt(h, m, 0).unwrap();
        assert!(!night_at(at(12, 21, 59), true));
        assert!(night_at(at(12, 22, 0), true));
        assert!(night_at(at(13, 5, 59), true));
        assert!(!night_at(at(13, 6, 0), true));
        assert!(!night_at(at(12, 23, 0), false), "not by himself when the automatic is off");
        // "Night mode off" at eleven: off until six, then the clock again.
        set_night(false, at(12, 23, 0));
        assert!(!night_at(at(13, 2, 0), true));
        assert!(!night_at(at(13, 6, 1), true));
        assert!(night_at(at(13, 22, 30), true), "the next night is night again");
        // "Night mode" at noon: on until the next morning.
        set_night(true, at(14, 12, 0));
        assert!(night_at(at(14, 15, 0), true) && night_at(at(15, 5, 0), false));
        assert!(!night_at(at(15, 6, 0), true));
        assert!(lock(&NIGHT_OVERRIDE).is_none(), "the morning clears what was said");
    }

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn a_voice_chat_in_front_is_no_game() {
        assert!(is_game("javaw.exe") && is_game("cs2") && is_game("Cyberpunk2077.exe"));
        assert!(!is_game("Discord.exe") && !is_game("firefox.exe") && !is_game("Code.exe"));
        assert!(is_player("Firefox.exe") && is_player("vlc") && !is_player("javaw.exe"));
    }

    #[test]
    fn requests_of_the_moment_are_taken_once() {
        let _one = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        apply(&json!({"subtitles": true, "subtitles_translate": true}));
        assert_eq!(subtitles(), Some(true));
        apply(&json!({"subtitles": false}));
        assert_eq!(subtitles(), None);
        apply(&json!({"subtitles_translate": false, "lyrics": true, "missed": true, "voice_setup": true, "settings_panel": true}));
        assert!(lyrics() && take_missed() && !take_missed() && take_voice_setup() && !take_voice_setup());
        assert!(take_settings_panel() && !take_settings_panel());
        // The lock needs a voice to lock to.
        set_voice_known(false);
        assert!(spoken(&json!({"voice_lock": true})).unwrap().contains("learn my voice"));
        // Guests: for a while, with the time Sir names or ten minutes.
        for (text, minutes) in [("guest mode", 10), ("let my friend speak", 10), ("let tom talk to you for half an hour", 30), ("you can listen to my brother too for 2 hours", 120), ("guest mode for 5 minutes", 5)] {
            assert_eq!(command(text), Some(json!({"voice_guest": true, "voice_guest_minutes": minutes})), "{text}");
        }
        for text in ["guest mode off", "only me again", "no more guests"] {
            assert_eq!(command(text), Some(json!({"voice_guest": false})), "{text}");
        }
        for text in ["let me speak", "let it talk", "listen to my music", "let my friend know", "answer to my email"] {
            assert_eq!(command(text), None, "{text}");
        }
        assert!(spoken(&json!({"voice_guest": true, "voice_guest_minutes": 30})).unwrap().contains("next 30 minutes"));
        assert!(spoken(&json!({"voice_guest": true, "voice_guest_minutes": 60})).unwrap().contains("next hour"));
        assert!(!crate::speaker_id::guests_welcome());
        apply(&json!({"voice_guest": true, "voice_guest_minutes": 10}));
        assert!(crate::speaker_id::guests_welcome());
        apply(&json!({"voice_lock": true}));
        assert!(!crate::speaker_id::guests_welcome(), "only obey me ends the welcome");
        set_voice_known(true);
        assert!(spoken(&json!({"voice_lock": true})).unwrap().contains("your voice alone"));
        set_voice_known(false);
        apply(&json!({"lyrics": false}));
        assert!(!lyrics());
    }
}
