//! Three things at his desk (he chose them on 2026-11-22):
//! - "what colour is this?": the colour under the mouse, named, its hex on the clipboard
//!   (Windows reads the pixel; on KDE KWin's own colour picker waits for his click),
//! - "is YouTube down?": whether the site answers from his PC and how fast, and when it doesn't,
//!   whether it is the site or his own connection,
//! - "make a QR code for this": the selected link or text (or the clipboard, or what he names)
//!   as a QR code in a picture block on the board.

use crate::kit::say;
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid desk phrase")
}

static COLOUR: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what|which)\s+colou?r\s+(?:is|s)\s+(?:this|that|it|here|(?:under|at)\s+(?:my|the)\s+(?:mouse|cursor|pointer))$|^whats\s+(?:this|that)\s+colou?r$|^(?:pick|grab|copy|get)\s+(?:this|that|the)\s+colou?r(?:\s+under\s+(?:my|the)\s+(?:mouse|cursor|pointer))?$|^(?:welche|was\s+für\s+eine)\s+farbe\s+(?:ist|hat)\s+(?:das|dies|hier|unter\s+(?:der|meiner)\s+maus)$")
});
static DOWN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^is\s+(?P<s>.+?)\s+(?:down|offline|not\s+working|working|up|online|reachable)(?:\s+(?:for\s+everyone|right\s+now|at\s+the\s+moment|again))?(?:\s+or\s+(?:is\s+it\s+)?(?:just\s+)?me)?$|^check\s+(?:if|whether)\s+(?P<s2>.+?)\s+is\s+(?:down|up|working|online)$|^ist\s+(?P<s3>.+?)\s+(?:gerade\s+|grad\s+)?(?:down|offline|erreichbar|online)$|^(?:geht|funktioniert)\s+(?P<s4>.+?)\s+(?:gerade|grad|im\s+moment)$")
});
static QR: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|create|show|give|generate)\s+(?:me\s+)?(?:a\s+|the\s+)?qr\s*code\s+(?:for|of|from)\s+(?P<t>.+)$|^(?:show|turn|put)\s+(?:this|that|it)\s+(?:as|into)\s+a\s+qr\s*code$|^qr\s*code\s+(?:for|of)\s+(?P<t2>.+)$|^(?:mach|erstelle|zeig)\s+(?:mir\s+)?(?:einen\s+)?qr\s*-?\s*code\s+(?:für|von|aus)\s+(?P<t3>.+)$")
});

/// Sites by the names he gives them: (his word, address, how it is said).
const SITES: &[(&str, &str, &str)] = &[
    ("youtube", "youtube.com", "YouTube"),
    ("google", "google.com", "Google"),
    ("gmail", "mail.google.com", "Gmail"),
    ("discord", "discord.com", "Discord"),
    ("twitch", "twitch.tv", "Twitch"),
    ("reddit", "reddit.com", "Reddit"),
    ("github", "github.com", "GitHub"),
    ("netflix", "netflix.com", "Netflix"),
    ("spotify", "open.spotify.com", "Spotify"),
    ("whatsapp", "web.whatsapp.com", "WhatsApp"),
    ("instagram", "instagram.com", "Instagram"),
    ("tiktok", "tiktok.com", "TikTok"),
    ("chatgpt", "chatgpt.com", "ChatGPT"),
    ("claude", "claude.ai", "Claude"),
    ("steam", "store.steampowered.com", "Steam"),
    ("amazon", "amazon.com", "Amazon"),
    ("twitter", "x.com", "X"),
    ("facebook", "facebook.com", "Facebook"),
    ("wikipedia", "wikipedia.org", "Wikipedia"),
    ("outlook", "outlook.live.com", "Outlook"),
    ("itslearning", "itslearning.com", "itslearning"),
    ("webuntis", "webuntis.com", "WebUntis"),
    ("epic games", "epicgames.com", "Epic Games"),
    ("roblox", "roblox.com", "Roblox"),
    ("minecraft", "minecraft.net", "Minecraft"),
    ("xbox", "xbox.com", "Xbox"),
    ("playstation", "playstation.com", "PlayStation"),
    ("microsoft", "microsoft.com", "Microsoft"),
    ("apple", "apple.com", "Apple"),
    ("icloud", "icloud.com", "iCloud"),
    ("onedrive", "onedrive.live.com", "OneDrive"),
    ("dropbox", "dropbox.com", "Dropbox"),
    ("teams", "teams.microsoft.com", "Teams"),
    ("zoom", "zoom.us", "Zoom"),
    ("ebay", "ebay.com", "eBay"),
    ("paypal", "paypal.com", "PayPal"),
    ("bing", "bing.com", "Bing"),
    ("duckduckgo", "duckduckgo.com", "DuckDuckGo"),
];

/// A site he names: its address and how it is said (None: not a site, "is my mic working").
fn site_of(words: &str) -> Option<(String, String)> {
    static DOMAIN: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:www\.)?[a-z0-9][a-z0-9-]*(?:\.[a-z0-9-]+)*\.[a-z]{2,}$"));
    let words = words.trim();
    let words = ["the ", "my "].iter().find_map(|a| words.strip_prefix(a)).unwrap_or(words).trim();
    if DOMAIN.is_match(words) {
        return Some((words.trim_start_matches("www.").to_string(), words.trim_start_matches("www.").to_string()));
    }
    let bare = words.trim_end_matches(" website").trim_end_matches(" site").trim_end_matches(" app");
    SITES.iter().find(|(word, ..)| *word == bare || word.replace(' ', "") == bare).map(|(_, site, said)| (site.to_string(), said.to_string()))
}

/// An order at his desk: the tool's arguments. `original`: what he said (a link keeps its case).
pub fn command(s: &str, original: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    if COLOUR.is_match(s) {
        return Some(json!({"action": "colour"}));
    }
    if let Some(c) = DOWN.captures(s) {
        let (site, said) = site_of(["s", "s2", "s3", "s4"].iter().find_map(|n| c.name(n))?.as_str())?;
        return Some(json!({"action": "down", "site": site, "said": said}));
    }
    let c = QR.captures(s)?;
    let Some(t) = ["t", "t2", "t3"].iter().find_map(|n| c.name(n)).map(|m| m.as_str().trim()) else {
        return Some(json!({"action": "qr", "from": "selection"}));
    };
    let from = match t {
        "this" | "that" | "it" | "this link" | "that link" | "this text" | "the link" | "the selection" | "the selected text" | "what i selected" | "das" | "diesen link" | "den link" => "selection",
        "my clipboard" | "the clipboard" | "what i copied" | "die zwischenablage" | "meine zwischenablage" => "clipboard",
        _ => {
            // (What he names keeps its case and its punctuation: a link's path is case-sensitive.)
            let lead = ["for", "of", "from", "für", "von", "aus"];
            let words: Vec<&str> = original.split_whitespace().collect();
            let at = words.iter().position(|w| lead.contains(&w.to_lowercase().as_str()))?;
            let text = words[at + 1..].join(" ").trim_end_matches(['.', '!', '?']).to_string();
            return (!text.is_empty()).then(|| json!({"action": "qr", "text": text}));
        }
    };
    Some(json!({"action": "qr", "from": from}))
}

/// Every desk action only looks, or shows a block of his own.
pub fn only_looks(args: &Value) -> bool {
    matches!(args["action"].as_str().unwrap_or(""), "colour" | "down" | "qr")
}

pub async fn run(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("") {
        "colour" => colour().await,
        "down" => down(args["site"].as_str().unwrap_or(""), args["said"].as_str().unwrap_or("")).await,
        "qr" => qr(ctx, args).await,
        other => Err(format!("The desk helper has no action '{other}'.")),
    }
}

// ----------------------------------------------------------------------------- colour ----

/// Colours by name, for the nearest.
const NAMES: &[(&str, [u8; 3])] = &[
    ("black", [0, 0, 0]),
    ("white", [255, 255, 255]),
    ("grey", [128, 128, 128]),
    ("silver", [192, 192, 192]),
    ("dark grey", [64, 64, 64]),
    ("charcoal", [54, 69, 79]),
    ("red", [255, 0, 0]),
    ("dark red", [139, 0, 0]),
    ("crimson", [220, 20, 60]),
    ("maroon", [128, 0, 0]),
    ("pink", [255, 170, 200]),
    ("hot pink", [255, 60, 160]),
    ("coral", [255, 127, 80]),
    ("salmon", [250, 128, 114]),
    ("orange", [255, 140, 0]),
    ("amber", [255, 191, 0]),
    ("gold", [255, 215, 0]),
    ("yellow", [255, 255, 0]),
    ("cream", [255, 253, 208]),
    ("beige", [245, 245, 220]),
    ("tan", [210, 180, 140]),
    ("brown", [139, 69, 19]),
    ("olive", [128, 128, 0]),
    ("lime", [50, 205, 50]),
    ("green", [0, 160, 0]),
    ("dark green", [0, 100, 0]),
    ("mint", [152, 255, 152]),
    ("teal", [0, 128, 128]),
    ("turquoise", [64, 224, 208]),
    ("cyan", [0, 255, 255]),
    ("sky blue", [135, 206, 235]),
    ("light blue", [173, 216, 230]),
    ("blue", [0, 80, 255]),
    ("navy", [0, 0, 128]),
    ("indigo", [75, 0, 130]),
    ("purple", [128, 0, 128]),
    ("violet", [238, 130, 238]),
    ("lavender", [200, 180, 255]),
    ("magenta", [255, 0, 255]),
];

/// The nearest named colour (the "redmean" distance: close to how the eye weighs them).
fn colour_name(rgb: [u8; 3]) -> &'static str {
    let distance = |c: [u8; 3]| {
        let (r1, g1, b1, r2, g2, b2) = (rgb[0] as i64, rgb[1] as i64, rgb[2] as i64, c[0] as i64, c[1] as i64, c[2] as i64);
        let mean = (r1 + r2) / 2;
        let (dr, dg, db) = (r1 - r2, g1 - g2, b1 - b2);
        (((512 + mean) * dr * dr) >> 8) + 4 * dg * dg + (((767 - mean) * db * db) >> 8)
    };
    NAMES.iter().min_by_key(|(_, c)| distance(*c)).map_or("grey", |(name, _)| name)
}

fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

#[cfg(windows)]
async fn pixel_under_mouse() -> Result<[u8; 3], String> {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    // SAFETY: plain queries; the screen's DC is released after its one read.
    let colour = unsafe {
        let mut at = POINT::default();
        GetCursorPos(&mut at).map_err(|e| e.to_string())?;
        let dc = GetDC(HWND(0));
        let colour = GetPixel(dc, at.x, at.y);
        ReleaseDC(HWND(0), dc);
        colour.0
    };
    if colour == u32::MAX {
        return Err("Windows wouldn't tell me the colour there, Sir.".to_string());
    }
    Ok([(colour & 0xFF) as u8, ((colour >> 8) & 0xFF) as u8, ((colour >> 16) & 0xFF) as u8])
}

/// Wayland lets no app read the screen's pixels: KWin's own picker waits for his click.
#[cfg(not(windows))]
async fn pixel_under_mouse() -> Result<[u8; 3], String> {
    let pick = crate::kde::blocking(|| crate::kde::call::<_, (u32,)>("org.kde.KWin", "/ColorPicker", "org.kde.kwin.ColorPicker", "pick", &()));
    let (argb,) = tokio::time::timeout(Duration::from_secs(30), pick).await.map_err(|_| "You didn't click a colour, Sir.".to_string())???;
    Ok([(argb >> 16) as u8, (argb >> 8) as u8, argb as u8])
}

async fn colour() -> Result<String, String> {
    if crate::image_viewer::offline() {
        return Ok(say("Would read the colour under the mouse (not from a test).", "Done, Sir."));
    }
    let rgb = pixel_under_mouse().await?;
    let (name, code) = (colour_name(rgb), hex(rgb));
    let copied = !crate::platform::hands_off() && crate::clipboard::write(&code).await.is_ok();
    let spelled: String = code[1..].chars().map(String::from).collect::<Vec<_>>().join(" ");
    Ok(say(
        format!("{code}  {name}  ({}, {}, {}){}", rgb[0], rgb[1], rgb[2], if copied { "\nThe hex is on your clipboard." } else { "" }),
        format!("That's {name}, Sir: hex {spelled}{}.", if copied { ", on your clipboard" } else { "" }),
    ))
}

// -------------------------------------------------------------------------- is it down ----

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) JAI/3")
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default()
});

async fn down(site: &str, said: &str) -> Result<String, String> {
    if site.is_empty() {
        return Err("Which site, Sir?".to_string());
    }
    if crate::image_viewer::offline() {
        return Ok(say(format!("Would check {site} (no network in a test)."), "Done, Sir."));
    }
    let started = Instant::now();
    match HTTP.get(format!("https://{site}")).send().await {
        Ok(reply) => {
            let (ms, status) = (started.elapsed().as_millis(), reply.status());
            // (Any answer is "up": a site that turns a script away with 403 still answered.)
            if status.is_server_error() {
                let line = format!("{said} answers, but with an error ({}): it's having trouble, Sir.", status.as_u16());
                Ok(say(format!("{site}: HTTP {} in {ms} ms", status.as_u16()), line))
            } else {
                Ok(say(format!("{site}: HTTP {} in {ms} ms", status.as_u16()), format!("{said} is up, Sir: it answered in {ms} milliseconds.")))
            }
        }
        Err(e) => {
            // Is it the site, or his own line?
            let own_line = HTTP.get("https://www.cloudflare.com/cdn-cgi/trace").send().await.is_ok();
            let line = if !own_line {
                format!("It's your own connection, Sir: nothing answers, not just {said}.")
            } else if e.is_timeout() {
                format!("{said} doesn't answer from here, Sir: it looks down.")
            } else {
                format!("{said} can't be reached from here, Sir: it looks down. Your internet is fine.")
            };
            Ok(say(format!("{site}: {e}"), line))
        }
    }
}

// ----------------------------------------------------------------------------- QR code ----

/// The QR code of `text` as a black-on-white picture with its quiet border (what every phone
/// reads), saved where JAI keeps such files.
fn qr_png(text: &str) -> Result<PathBuf, String> {
    use std::hash::{Hash, Hasher};
    let (side, modules) = crate::satellite::qr_matrix(text).ok_or("That's too long for a QR code, Sir.")?;
    let (quiet, cell) = (4usize, 10usize);
    let full = ((side + 2 * quiet) * cell) as u32;
    let picture = image::GrayImage::from_fn(full, full, |x, y| {
        let (mx, my) = (x as usize / cell, y as usize / cell);
        let dark = (quiet..quiet + side).contains(&mx) && (quiet..quiet + side).contains(&my) && modules[(my - quiet) * side + mx - quiet];
        image::Luma([if dark { 0 } else { 255 }])
    });
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let dir = std::env::temp_dir().join("jai_qr");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("qr_{:016x}.png", hasher.finish()));
    picture.save(&path).map_err(|e| format!("I couldn't save the QR code, Sir: {e}"))?;
    Ok(path)
}

async fn qr(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let text = match args["from"].as_str() {
        Some("clipboard") => crate::clipboard::read().await?,
        Some(_) => match crate::kit_voice::selected(ctx).await? {
            Some(text) => text,
            None => crate::clipboard::read().await.unwrap_or_default(),
        },
        None => args["text"].as_str().unwrap_or("").to_string(),
    };
    let text = text.trim();
    if text.is_empty() {
        return Err("Select the link first, Sir, or copy it.".to_string());
    }
    let path = qr_png(text)?;
    crate::image_viewer::run_tool(ctx, &json!({"path": path, "query": "QR code"})).await?;
    let shown: String = text.chars().take(120).collect();
    Ok(say(format!("QR code for: {shown}"), "There's the QR code, Sir."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(said: &str) -> Option<Value> {
        command(&crate::intents::normalize_phrase(said), said)
    }

    /// Sir chose them on 2026-11-22: each in his words; a question about something that is no
    /// website stays elsewhere.
    #[test]
    fn his_desk_orders_are_known_by_their_words() {
        for said in ["What colour is this?", "what color is under my mouse", "What's this colour?", "pick this colour", "Welche Farbe ist das?"] {
            assert_eq!(order(said), Some(json!({"action": "colour"})), "{said}");
        }
        assert_eq!(order("Is YouTube down?"), Some(json!({"action": "down", "site": "youtube.com", "said": "YouTube"})));
        assert_eq!(order("is discord down for everyone or just me"), Some(json!({"action": "down", "site": "discord.com", "said": "Discord"})));
        assert_eq!(order("Is myschool.webuntis.com down?"), Some(json!({"action": "down", "site": "myschool.webuntis.com", "said": "myschool.webuntis.com"})));
        assert_eq!(order("Ist Twitch gerade down?"), Some(json!({"action": "down", "site": "twitch.tv", "said": "Twitch"})));
        assert_eq!(order("check if epic games is up"), Some(json!({"action": "down", "site": "epicgames.com", "said": "Epic Games"})));
        assert_eq!(order("Make a QR code for this"), Some(json!({"action": "qr", "from": "selection"})));
        assert_eq!(order("QR code for my clipboard"), Some(json!({"action": "qr", "from": "clipboard"})));
        assert_eq!(order("turn this into a QR code"), Some(json!({"action": "qr", "from": "selection"})));
        assert_eq!(order("Make a QR code for https://example.com/Sign-Up?id=12345."), Some(json!({"action": "qr", "text": "https://example.com/Sign-Up?id=12345"})));
        for other in ["is my mic working", "is it working", "is the music up", "is the internet down", "show the QR code", "what colour is the sky", "what is this"] {
            assert_eq!(order(other), None, "{other}");
        }
        assert!(only_looks(&json!({"action": "down"})) && !only_looks(&json!({"action": "nothing"})));
    }

    /// What the Features tab offers to try for the features of 2026-11-22 lands, said as it is
    /// written there, on the feature itself (the whole fast path, his name in front).
    #[test]
    fn every_new_feature_is_tried_as_it_is_written() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        for (id, tools) in [
            ("where_was_i", &["where_was_i"][..]),
            ("wake_pc", &["other_pc"]),
            ("countdowns", &["countdowns"]),
            ("free_space", &["file_helper"]),
            ("colour_picker", &["desk_helper"]),
            ("is_it_down", &["desk_helper"]),
            ("last_download", &["file_helper"]),
            ("recent_files", &["file_helper"]),
            ("file_orders", &["file_helper"]),
            ("picture_copies", &["file_helper"]),
            ("qr_code", &["desk_helper"]),
            ("path_and_folder", &["file_helper"]),
        ] {
            let feature = crate::changes::EXPLANATIONS.iter().find(|e| e.id == id).expect(id);
            // ("How many days until my exam?" is the calculator's answer: `countdowns::` tests it.)
            for said in feature.say.iter().filter(|s| !s.contains("how many days")) {
                let routed = crate::phrasings::route(&crate::wake::canon(said)).map(|(name, _)| name).unwrap_or_default();
                assert!(tools.contains(&routed.as_str()), "{id}: \"{said}\" went to '{routed}'");
            }
        }
    }

    #[test]
    fn a_colour_is_named_and_a_qr_code_drawn() {
        assert_eq!(colour_name([255, 212, 0]), "gold");
        assert_eq!(colour_name([250, 5, 5]), "red");
        assert_eq!(colour_name([10, 10, 12]), "black");
        assert_eq!(colour_name([0, 0, 120]), "navy");
        assert_eq!(colour_name([30, 170, 30]), "green");
        assert_eq!(hex([255, 212, 0]), "#FFD400");
        let path = qr_png("https://example.com").unwrap();
        let picture = image::open(&path).unwrap().to_luma8();
        let side = crate::satellite::qr_matrix("https://example.com").unwrap().0;
        assert_eq!(picture.width(), ((side + 8) * 10) as u32);
        // The quiet border is white, the finder pattern's corner black.
        assert_eq!(picture.get_pixel(5, 5).0, [255]);
        assert_eq!(picture.get_pixel(45, 45).0, [0]);
        let _ = std::fs::remove_file(path);
    }
}
