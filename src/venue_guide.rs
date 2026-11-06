//! Going out, the way the reference board does it.
//!
//! * "Where can I take my fiancée for a super serious Halloween night?" — Jarvis picks one real
//!   venue himself ("I know just the place"): a web search near where Sir is, then the local
//!   model chooses one place *from the results* (a pick that appears nowhere in them is thrown
//!   away and the first result that names a venue wins instead). The place goes on the board as a
//!   dark map with its photos (`hud_board` `show_map`) and he says why.
//! * "Let's go here" / "take me there" with a map on the board — the route from Sir's position:
//!   driving time and distance from the OSRM router, and the directions open in Google Maps.
//!
//! The phrase detectors, the search query, the pick and the route line are pure and unit-tested;
//! the network steps live in `recommend` and `go_here`.

use crate::tools::{str_arg, ToolContext};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use tracing::{info, warn};

pub const TOOL: &str = "venue_guide";

/// The place Jarvis last recommended (name, lat, lon): "let's go here" goes there.
static LAST_PLACE: Mutex<Option<(String, f64, f64)>> = Mutex::new(None);

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "type": "function",
        "function": {
            "name": TOOL,
            "description": "Going out. action recommend (request: Sir's words, e.g. 'where can I take my fiancée for a scary Halloween night'): YOU pick one real venue near him from a web search and put it on the HUD board as a map with photos. action go_here: the route to the place on the board ('let's go here', 'take me there'): driving time, distance, directions opened in Google Maps.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["recommend", "go_here"]},
                    "request": {"type": "string", "description": "recommend: Sir's request in his words."}
                },
                "required": ["action"]
            }
        }
    })]
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid venue regex")
}

/// "Where can I take my fiancée for …", "where should we go tonight", "any ideas where to take
/// her for dinner", "wo können wir heute Abend hin": the outing, in Sir's words.
static VENUE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:hey\s+)?(?:jarvis[,\s]+)?(?:any\s+ideas?\s+|do you know\s+|can you suggest\s+|suggest\s+)?(?:where|somewhere)\s+(?:(?:can|could|should|shall|do|might)\s+)?(?:i\s+|we\s+|to\s+)(?:take|bring|go|head|go out)\b(.*)$|^(?:hey\s+)?(?:jarvis[,\s]+)?(?:wo|wohin)\s+(?:kann|können|koennen|sollen|soll)\s+(?:ich|wir)\b(.*)$")
});

/// Sir is asking for a place to go: the request (trimmed), or None.
pub fn venue_request(text: &str) -> Option<String> {
    let t = text.trim().trim_end_matches(['?', '.', '!']).trim();
    let c = VENUE.captures(t)?;
    let rest = c.get(1).or(c.get(2)).map(|m| m.as_str().trim()).unwrap_or("");
    // "Where can I go to change the colour" is no outing: it needs an occasion or a person.
    let lower = rest.to_lowercase();
    let outing = ["for", "tonight", "today", "this weekend", "on", "to eat", "for dinner", "out", "date", "my ", "her", "him", "them", "the kids", "heute", "abend", "essen", "mit", "am wochenende"];
    if rest.is_empty() || !outing.iter().any(|w| lower.contains(w)) || lower.contains("setting") || lower.contains("colour") || lower.contains("color") {
        return None;
    }
    Some(t.to_string())
}

/// "Let's go here", "take me there", "navigate there", "how do I get there", "route there".
static GO_HERE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:yeah,?\s+|yes,?\s+|okay,?\s+|ok,?\s+|alright,?\s+)*(?:okay,?\s+|ok,?\s+)?(?:let'?s|lets)\s+go\s+(?:here|there)|^(?:take|get|bring|drive)\s+(?:me|us)\s+there|^(?:navigate|route|directions)\s+(?:me\s+)?(?:there|here)|^how\s+do\s+(?:i|we)\s+get\s+there|^(?:show|give)\s+me\s+(?:the\s+)?(?:route|way|directions)(?:\s+there)?$|^(?:lass uns|lasst uns)\s+(?:da|dahin|hier)\s*(?:hin)?\s*(?:gehen|fahren)|^(?:bring|fahr)\s+(?:mich|uns)\s+(?:da|dahin|hin)")
});

pub fn go_request(text: &str) -> bool {
    let t = text.trim().trim_end_matches(['?', '.', '!']).trim();
    let t = t.strip_prefix("Jarvis,").or_else(|| t.strip_prefix("Jarvis")).map(str::trim).unwrap_or(t);
    GO_HERE.is_match(t)
}

/// The occasion words of a request for the search: what follows "for" ("a super serious
/// Halloween night" → "serious Halloween night"), else what follows the verb ("tonight"); a
/// lone time word becomes "things to do tonight", a lone noun "dinner places".
fn occasion(request: &str) -> String {
    let request = request.trim().trim_end_matches(['?', '.', '!']);
    let lower = request.to_lowercase();
    let after_for = ["for ", "für "].iter().filter_map(|k| lower.find(k).map(|i| &request[i + k.len()..])).next();
    let after_verb = || [" take ", " go ", " head ", " bring ", " hin"].iter().filter_map(|k| lower.find(k).map(|i| &request[i + k.len()..])).next();
    let base = after_for.or_else(after_verb).unwrap_or(request);
    const DROP: &[&str] = &["a", "an", "the", "my", "super", "really", "very", "some", "please", "jarvis", "ein", "eine", "einen", "her", "him", "them", "us", "out", "to", "with", "and", "go", "hin", "wir", "ich"];
    let words: Vec<&str> = base.split_whitespace().filter(|w| !DROP.contains(&w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()))).collect();
    match words.as_slice() {
        [] => "things to do tonight".to_string(),
        [w] if matches!(w.to_lowercase().as_str(), "tonight" | "today" | "weekend" | "heute" | "abend") => format!("things to do {w}"),
        [w] => format!("{w} places"),
        _ => words.join(" "),
    }
}

/// The web search for a request near `town`.
pub fn search_query(request: &str, town: Option<&str>) -> String {
    let what = occasion(request);
    let what = if what.trim().is_empty() { "things to do tonight".to_string() } else { what };
    match town.filter(|t| !t.trim().is_empty()) {
        Some(t) => format!("best {what} near {t}"),
        None => format!("best {what} near me"),
    }
}

/// A result title cut to the venue's name: "Tulleys Farm - Halloween Shocktober Fest | Crawley"
/// → "Tulleys Farm"; listicles ("10 best …", "Things to do …") name no single venue.
pub fn venue_from_title(title: &str) -> Option<String> {
    let lower = title.to_lowercase();
    let listicle = ["best", "top ", "things to do", "guide", "list of", "events in", "events near", "ideas", "reddit", "tripadvisor", "what's on", "whats on"];
    if listicle.iter().any(|w| lower.contains(w)) || lower.chars().take(3).any(|c| c.is_ascii_digit()) {
        return None;
    }
    let name = title.split([' ', '|']).collect::<Vec<_>>().join(" ");
    let name = name.split(" - ").next().unwrap_or(&name).split(" – ").next().unwrap_or(&name).split(" | ").next().unwrap_or(&name).split(": ").next().unwrap_or(&name).trim();
    let words = name.split_whitespace().count();
    (2..=6).contains(&words).then(|| name.to_string())
}

/// A venue to suggest: its name, why, and how a map search may find it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub venue: String,
    pub why: String,
    /// "Schlachthof, Musterstadt": the model's map query (an event's name alone is rarely on a map).
    pub place: Option<String>,
}

/// The model's pick (`{"venue": …, "place": …, "why": …}`), kept only when the venue appears in
/// the search results (the model must choose from them, not invent a place).
pub fn grounded_pick(raw: &str, results_text: &str) -> Option<Pick> {
    let json = raw.get(raw.find('{')?..=raw.rfind('}')?)?;
    let v: Value = serde_json::from_str(json).ok()?;
    let venue = v["venue"].as_str()?.trim().to_string();
    let why = v["why"].as_str().unwrap_or("").trim().trim_end_matches('.').to_string();
    let place = v["place"].as_str().map(str::trim).filter(|p| !p.is_empty() && p.len() <= 120).map(String::from);
    let key: String = venue.to_lowercase().split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    (!venue.is_empty() && venue.len() <= 80 && results_text.to_lowercase().contains(&key)).then_some(Pick { venue, why, place })
}

/// A map query that only names the town ("Musterstadt, Schwarzwald, Germany" when Sir is in Musterstadt):
/// it finds the town, not the venue.
pub fn is_town_level(place: &str, town: Option<&str>) -> bool {
    let first = place.split(',').next().unwrap_or("").trim().to_lowercase();
    town.is_some_and(|t| !t.trim().is_empty() && first == t.trim().to_lowercase()) && !place.chars().any(|c| c.is_ascii_digit())
}

/// The map searches to try for a pick, best first: the model's place, the venue in Sir's town,
/// the venue alone (no duplicates, never a query that only names the town).
pub fn map_queries(pick: &Pick, town: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |q: String| {
        let q = q.trim().trim_matches(',').trim().to_string();
        if !q.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&q)) {
            out.push(q);
        }
    };
    if let Some(p) = pick.place.as_ref().filter(|p| !is_town_level(p, town)) {
        push(p.clone());
    }
    if let Some(t) = town.filter(|t| !t.trim().is_empty() && !pick.venue.to_lowercase().contains(&t.to_lowercase())) {
        push(format!("{}, {t}", pick.venue));
    }
    push(pick.venue.clone());
    out.retain(|q| !is_town_level(q, town));
    out
}

/// A street address in search snippets ("Turners Hill Road 12", "Schlachthofstraße 11, 12345
/// Musterstadt", "12 Main Street"): the first one, for the map search.
static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b([A-ZÄÖÜ][\wäöüß.\-]*(?:straße|strasse|str\.|weg|platz|allee|gasse|ring|damm|ufer)\s*\d{1,4}[a-z]?(?:\s*,\s*\d{4,5}\s+[A-ZÄÖÜ][\wäöüß\-]+)?)|\b(\d{1,4}\s+[A-Z][\w\-]+(?:\s+[A-Z][\w\-]+)?\s+(?:road|street|lane|avenue|way|drive|rd|st|ave)\b)")
});

/// Where an event takes place, as the results say it next to its name: "… im Schlachthof Musterstadt",
/// "in der Stadthalle", "Veranstaltungsort: Pflugsaal", "held at the Old Fire Station". Proper
/// names only (one to four capitalised words), nearest to the venue's mention first; up to three.
pub fn location_hints(text: &str, venue: &str) -> Vec<String> {
    static HINT: LazyLock<Regex> = LazyLock::new(|| {
        re(r"(?:\b(?:im|in der|in den|auf dem|auf der|am|beim|held at|takes place at|at the|venue:|location:|ort:|veranstaltungsort:)\s+)((?:[A-ZÄÖÜ][\wäöüß\-']+(?:\s+|$)){1,4})")
    });
    let lower = text.to_lowercase();
    let anchor = lower.find(&venue.to_lowercase()).unwrap_or(0);
    const SKIP: &[&str] = &["Halloween", "Oktober", "October", "November", "Samstag", "Freitag", "Saturday", "Friday", "Eintritt", "Tickets", "Ticket", "Abend", "Night"];
    let mut hints: Vec<(usize, String)> = HINT
        .captures_iter(text)
        .filter_map(|c| {
            let m = c.get(1)?;
            let name = m.as_str().trim().trim_end_matches(['.', ',']).to_string();
            let first = name.split_whitespace().next().unwrap_or("");
            (!SKIP.contains(&first) && name.chars().count() >= 4 && !venue.to_lowercase().contains(&name.to_lowercase())).then(|| (m.start().abs_diff(anchor), name))
        })
        .collect();
    hints.sort_by_key(|(d, _)| *d);
    let mut out: Vec<String> = Vec::new();
    for (_, name) in hints {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&name)) {
            out.push(name);
        }
        if out.len() == 3 {
            break;
        }
    }
    out
}

pub fn address_in(text: &str) -> Option<String> {
    let c = ADDRESS.captures(text)?;
    c.get(1).or(c.get(2)).map(|m| m.as_str().trim().to_string())
}

/// Where a pick ended up on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapped {
    /// The venue itself (or its address).
    Venue,
    /// Only its town could be found: the town is on the board.
    Town,
    /// Nothing could be mapped.
    No,
}

/// The spoken line for a pick, true to what the board really shows.
pub fn pick_line(venue: &str, why: &str, mapped: Mapped, town: Option<&str>) -> String {
    let reason = if why.is_empty() { String::new() } else { format!(" — {}", lowercase_first(why)) };
    match (mapped, town) {
        (Mapped::Venue, _) => format!("I know just the place, Sir. {venue}{reason}. It's on the board."),
        (Mapped::Town, Some(t)) => format!("I know just the place, Sir. {venue}{reason}. It's in {t}; I couldn't pin the venue itself, so {t} is on the board."),
        _ => format!("I'd suggest {venue}{reason}. I couldn't pin it on a map, though, Sir."),
    }
}

fn lowercase_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        // Names ("Tulleys …") keep their capital; a lone "It" does not.
        Some(f) if f.is_uppercase() && c.clone().next().is_some_and(char::is_lowercase) && s.split_whitespace().next().is_some_and(|w| matches!(w, "It" | "They" | "The" | "A" | "An" | "Its" | "There")) => f.to_lowercase().chain(c).collect(),
        _ => s.to_string(),
    }
}

/// Google Maps directions from `from` (or "my location") to `to`.
pub fn directions_url(from: Option<(f64, f64)>, to: (f64, f64)) -> String {
    let origin = from.map(|(a, b)| format!("&origin={a:.6},{b:.6}")).unwrap_or_default();
    format!("https://www.google.com/maps/dir/?api=1{origin}&destination={:.6},{:.6}&travelmode=driving", to.0, to.1)
}

/// "Route's up, Sir: Tulleys Farm, 24 minutes by car, 18 kilometres."
pub fn route_line(place: &str, drive: Option<(f64, f64)>) -> String {
    match drive {
        Some((secs, metres)) => format!(
            "Route's up, Sir. {place} is {} by car, {} away. Directions are open in Maps.",
            crate::location_engine::minutes_text(secs),
            if metres >= 10_000.0 { format!("{:.0} kilometres", metres / 1000.0) } else { format!("{:.1} kilometres", metres / 1000.0) }
        ),
        None => format!("Directions to {place} are open in Maps, Sir. The router didn't give me a driving time."),
    }
}

fn spoken(display: impl AsRef<str>, line: impl AsRef<str>) -> String {
    format!("{}\nSpoken: {}", display.as_ref().trim_end(), line.as_ref().trim())
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    match str_arg(args, &["action"]).unwrap_or("recommend") {
        "go_here" | "go" | "route" => go_here(tools).await,
        _ => {
            let request = str_arg(args, &["request", "query", "text"]).ok_or("What should I find for you, Sir?")?.to_string();
            recommend(tools, &request).await
        }
    }
}

/// Picks one venue for `request` near Sir and puts it on the board.
pub async fn recommend(tools: &ToolContext, request: &str) -> Result<String, String> {
    let engine = &tools.stark.location;
    let fix = engine.current(tools).await.ok();
    let town = match &fix {
        Some(f) => match &f.address {
            Some(a) => Some(a.clone()),
            None => engine.reverse_geocode(f.lat, f.lon).await,
        },
        None => None,
    };
    // The town is the part of the address before the postcode / country.
    let town = town.map(|a| a.split(',').rev().nth(2).or(a.split(',').next()).unwrap_or("").trim().to_string());
    let query = search_query(request, town.as_deref());
    let results = crate::websearch::duckduckgo(&query).await.unwrap_or_default();
    if results.is_empty() {
        return Err("I couldn't reach the search engines just now, Sir. Ask me again in a moment.".into());
    }
    let listing: String = results.iter().take(10).enumerate().map(|(i, r)| format!("#{} {} — {}\n", i + 1, r.title, r.snippet)).collect();
    let results_text = results.iter().map(|r| format!("{} {}", r.title, r.snippet)).collect::<Vec<_>>().join(" ");
    let instruction = format!(
        "Sir asked: \"{request}\". From the search results pick ONE real, specific venue or event location he can go to (never a list, article or website). \
         Reply with JSON only: {{\"venue\": \"the venue's name as the results write it\", \"place\": \"where it is, as a map search would find it: the building or address plus the town\", \"why\": \"one short reason from the results, at most 14 words\"}}"
    );
    let llm = if crate::vram_guard::llm_allowed() { tools.pim.complete(&instruction, &listing, 200).await } else { None };
    // The model's pick first, then every result title that names a venue.
    let mut picks: Vec<Pick> = llm.as_deref().and_then(|raw| grounded_pick(raw, &results_text)).into_iter().collect();
    picks.extend(results.iter().filter_map(|r| venue_from_title(&r.title)).map(|venue| Pick { venue, why: String::new(), place: None }).take(3));
    let Some(first) = picks.first().cloned() else {
        return Err("The search came back with lists rather than a place, Sir. Give me a town or a kind of place and I'll try again.".into());
    };
    // The first pick a map search finds goes on the board.
    let near = fix.as_ref().map(|f| (f.lat, f.lon));
    let mut mapped: Option<(Pick, String)> = None;
    'picks: for pick in &picks {
        for place in map_queries(pick, town.as_deref()) {
            let found = engine.geocode(&place, near).await.is_ok();
            info!("[VENUE] Map search '{place}': {}", if found { "found" } else { "not found" });
            if found {
                mapped = Some((pick.clone(), place));
                break 'picks;
            }
        }
    }
    // An event's name is rarely on a map: its address usually is ("… im Schlachthof,
    // Schlachthofstraße 11, Musterstadt").
    if mapped.is_none() {
        let lookup = format!("{} {} address", first.venue, town.as_deref().unwrap_or(""));
        let hits = crate::websearch::duckduckgo(lookup.trim()).await.unwrap_or_default();
        let text = std::iter::once(results_text.clone()).chain(hits.iter().map(|h| format!("{} {}", h.title, h.snippet))).collect::<Vec<_>>().join(" ");
        if let Some(address) = address_in(&text) {
            let place = match town.as_deref() {
                Some(t) if !address.to_lowercase().contains(&t.to_lowercase()) => format!("{address}, {t}"),
                _ => address,
            };
            let found = engine.geocode(&place, near).await.is_ok();
            info!("[VENUE] Address '{place}' for {}: {}", first.venue, if found { "found" } else { "not found" });
            if found {
                mapped = Some((first.clone(), place));
            }
        }
        // No street address: the building the results name beside it ("im Schlachthof").
        if mapped.is_none() {
            for hint in location_hints(&text, &first.venue) {
                let place = match town.as_deref() {
                    Some(t) if !hint.to_lowercase().contains(&t.to_lowercase()) => format!("{hint}, {t}"),
                    _ => hint,
                };
                let found = engine.geocode(&place, near).await.is_ok();
                info!("[VENUE] Location '{place}' for {}: {}", first.venue, if found { "found" } else { "not found" });
                if found {
                    mapped = Some((first.clone(), place));
                    break;
                }
            }
        }
    }
    // Last resort: the town the model placed it in (said as such).
    let mut town_only = false;
    if mapped.is_none() {
        if let (Some(place), Some(t)) = (first.place.clone().filter(|p| is_town_level(p, town.as_deref())), town.as_deref()) {
            if engine.geocode(&place, near).await.is_ok() {
                info!("[VENUE] Only the town of {} could be mapped: {t}", first.venue);
                mapped = Some((first.clone(), place));
                town_only = true;
            }
        }
    }
    let (pick, place) = match mapped {
        Some(m) => m,
        None => {
            info!("[VENUE] '{request}' → {} (not on any map)", first.venue);
            return Ok(spoken(format!("Suggested: {} (no map match). Search: {query}", first.venue), pick_line(&first.venue, &first.why, Mapped::No, town.as_deref())));
        }
    };
    info!("[VENUE] '{request}' → {} at '{place}' ({})", pick.venue, pick.why);
    let shown = Box::pin(tools.execute(crate::hud_board::TOOL, &json!({ "action": "show_map", "place": place }))).await;
    if !shown.success {
        warn!("[VENUE] The board could not show {place}: {}", shown.output);
    }
    if let Some((_, lat, lon)) = crate::hud_board::current_place() {
        *crate::crash_guard::lock(&LAST_PLACE) = Some((pick.venue.clone(), lat, lon));
    }
    Ok(spoken(
        format!("Recommended: {} at {place}{}. Search: {query}", pick.venue, if pick.why.is_empty() { String::new() } else { format!(" ({})", pick.why) }),
        pick_line(&pick.venue, &pick.why, if !shown.success { Mapped::No } else if town_only { Mapped::Town } else { Mapped::Venue }, town.as_deref()),
    ))
}

/// The route to the place on the board (or the last recommendation).
pub async fn go_here(tools: &ToolContext) -> Result<String, String> {
    let engine = &tools.stark.location;
    let fix = engine.current(tools).await.ok();
    let near = fix.as_ref().map(|f| (f.lat, f.lon));
    // The map on the board first, else the last pick.
    let (place, lat, lon) = crate::hud_board::current_place()
        .or_else(|| crate::crash_guard::lock(&LAST_PLACE).clone())
        .ok_or("There's no place on the board to go to yet, Sir. Ask me where to go first.")?;
    let drive = match near {
        Some(from) => crate::location_engine::driving(engine.http(), from, (lat, lon)).await,
        None => None,
    };
    let url = directions_url(near, (lat, lon));
    if let Err(e) = crate::apps::open_default_browser(&url).await {
        warn!("[VENUE] Could not open the directions: {e}");
    }
    info!("[VENUE] Route to {place}: {drive:?}");
    let name = place.split(',').next().unwrap_or(&place).trim().to_string();
    Ok(spoken(format!("Route to {place}: {url}"), route_line(&name, drive)))
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_event_is_pinned_to_the_building_the_results_name() {
        let text = "Musterstadts größte Halloween Party am 31. Oktober im Schlachthof Musterstadt — Tickets ab 12 Euro. Party im Freien? Nein.";
        assert_eq!(location_hints(text, "Musterstadts größte Halloween Party"), vec!["Schlachthof Musterstadt".to_string()]);
        let en = "The Haunted Ball is held at the Old Fire Station on Saturday night.";
        assert_eq!(location_hints(en, "The Haunted Ball"), vec!["Old Fire Station".to_string()]);
        assert!(location_hints("Halloween night in Musterstadt, no venue named.", "Halloween Party").is_empty());
    }

    use super::*;

    #[test]
    fn going_out_questions_are_recognised() {
        // The reference clip.
        assert!(venue_request("Hey Jarvis, where can I take my fiancée for a super serious Halloween night?").is_some());
        for yes in ["Where should we go tonight?", "Any ideas where to take her for dinner", "where can we go this weekend with the kids", "Wo können wir heute Abend hin?", "Where can I take my girlfriend on a date"] {
            assert!(venue_request(yes).is_some(), "{yes}");
        }
        for no in ["Where can I change the colour of the interface", "Where is my calendar", "Open Spotify", "Where can I go", "where do I go to change the settings"] {
            assert!(venue_request(no).is_none(), "{no}");
        }
    }

    #[test]
    fn lets_go_here_is_recognised() {
        for yes in ["Yeah, okay let's go here.", "Let's go there", "take me there", "Jarvis, navigate there", "How do I get there?", "show me the route", "Lass uns da hingehen", "bring mich dahin"] {
            assert!(go_request(yes), "{yes}");
        }
        for no in ["let's go to bed", "go to the sim lab", "take me to the board", "here we go"] {
            assert!(!go_request(no), "{no}");
        }
    }

    #[test]
    fn the_search_looks_for_the_occasion_near_sir() {
        assert_eq!(search_query("Where can I take my fiancée for a super serious Halloween night?", Some("Crawley")), "best serious Halloween night near Crawley");
        assert_eq!(search_query("where should we go tonight", None), "best things to do tonight near me");
        assert_eq!(search_query("Any ideas where to take her for dinner", Some("Freiburg")), "best dinner places near Freiburg");
    }

    #[test]
    fn the_pick_must_come_from_the_results() {
        let results = "Tulleys Farm Shocktober Fest — the UK's scariest Halloween event near Crawley. Hangar Hall escape rooms.";
        assert_eq!(
            grounded_pick(r#"{"venue": "Tulleys Farm", "place": "Tulleys Farm, Turners Hill Road, Crawley", "why": "It runs the scariest Halloween event nearby"}"#, results),
            Some(Pick { venue: "Tulleys Farm".into(), why: "It runs the scariest Halloween event nearby".into(), place: Some("Tulleys Farm, Turners Hill Road, Crawley".into()) })
        );
        assert_eq!(grounded_pick(r#"{"venue": "Castle Dracula", "why": "spooky"}"#, results), None, "invented: not in the results");
        // The live run: an event's name alone is not on a map; the model's place and the town are tried first.
        let party = Pick { venue: "Musterstadts größte Halloween Party".into(), why: String::new(), place: Some("Schlachthof, Musterstadt".into()) };
        assert_eq!(map_queries(&party, Some("Musterstadt")), ["Schlachthof, Musterstadt", "Musterstadts größte Halloween Party"]);
        // The second live run: the model's "place" was only the town; it is not a venue query.
        let town_only = Pick { place: Some("Musterstadt, Schwarzwald, Germany".into()), ..party.clone() };
        assert!(is_town_level("Musterstadt, Schwarzwald, Germany", Some("Musterstadt")));
        assert!(!is_town_level("Schlachthofstraße 11, Musterstadt", Some("Musterstadt")));
        assert_eq!(map_queries(&town_only, Some("Musterstadt")), ["Musterstadts größte Halloween Party"]);
        let farm = Pick { venue: "Tulleys Farm".into(), why: String::new(), place: None };
        assert_eq!(map_queries(&farm, Some("Crawley")), ["Tulleys Farm, Crawley", "Tulleys Farm"]);
        assert_eq!(grounded_pick("no json here", results), None);
        assert_eq!(venue_from_title("Tulleys Farm - Halloween Shocktober Fest | Crawley"), Some("Tulleys Farm".into()));
        assert_eq!(venue_from_title("10 Best Halloween Events Near Crawley (2026)"), None);
        assert_eq!(venue_from_title("Things to do in Sussex this Halloween"), None);
        assert_eq!(pick_line("Tulleys Farm", "It runs the scariest Halloween event nearby", Mapped::Venue, Some("Crawley")), "I know just the place, Sir. Tulleys Farm — it runs the scariest Halloween event nearby. It's on the board.");
        assert_eq!(pick_line("Tulleys Farm", "", Mapped::Venue, None), "I know just the place, Sir. Tulleys Farm. It's on the board.");
        // Never claim more than the board shows.
        let honest = pick_line("Musterstadts größte Halloween Party", "", Mapped::No, Some("Musterstadt"));
        assert!(!honest.contains("on the board") && honest.contains("couldn't pin it"), "{honest}");
        let town = pick_line("Musterstadts größte Halloween Party", "", Mapped::Town, Some("Musterstadt"));
        assert!(town.contains("It's in Musterstadt") && town.contains("Musterstadt is on the board"), "{town}");
    }

    #[test]
    fn street_addresses_are_read_from_snippets() {
        assert_eq!(address_in("Halloween im Schlachthof, Schlachthofstraße 11, 12345 Musterstadt. Tickets ab 15 €"), Some("Schlachthofstraße 11, 12345 Musterstadt".into()));
        assert_eq!(address_in("Party in der Tiergartenstr. 4a heute"), Some("Tiergartenstr. 4a".into()));
        assert_eq!(address_in("Find us at 12 Turners Hill Road, Crawley"), Some("12 Turners Hill Road".into()));
        assert_eq!(address_in("The scariest night of the year, 2025 tickets"), None);
    }

    #[test]
    fn the_route_opens_maps_and_says_how_long() {
        assert_eq!(directions_url(Some((51.1, -0.2)), (51.12, -0.15)), "https://www.google.com/maps/dir/?api=1&origin=51.100000,-0.200000&destination=51.120000,-0.150000&travelmode=driving");
        assert!(!directions_url(None, (1.0, 2.0)).contains("origin"));
        assert_eq!(route_line("Tulleys Farm", Some((24.0 * 60.0, 18_300.0))), "Route's up, Sir. Tulleys Farm is 24 minutes by car, 18 kilometres away. Directions are open in Maps.");
        assert!(route_line("Tulleys Farm", Some((300.0, 2_400.0))).contains("2.4 kilometres"));
        assert!(route_line("Tulleys Farm", None).contains("open in Maps"));
    }
}
