//! The workbench: ten everyday systems on top of the field systems, each a tool of its own
//! with deterministic voice routes in English and German and a row in the Feature Lab.
//!
//! * `manage_flashcards` — spaced-repetition flashcards (`flashcards`);
//! * `lookup_formula` — the maths / physics / chemistry formula sheet (`formula_sheet`);
//! * `chemistry_helper` — molar masses and mole / gram conversion (`chem_helper`);
//! * `show_app_shortcuts` — shortcut sheets for the program in front (`shortcut_overlay`);
//! * `screen_time_report` — screen time and the break coach (`screen_time`);
//! * `suggest_routine` — routines learned from what Sir asks for (`routine_learner`);
//! * `thermal_advisor` — an hour of temperatures, a verdict, a fan curve (`thermal_advisor`);
//! * `clipboard_history` — the last copies, in memory only (`clipboard_history`);
//! * `tile_windows` — program windows arranged on a monitor (`window_tiler`);
//! * `suggest_commands` — the console's predictive completion (`command_palette`).
//!
//! This hub owns their shared state, the tool list, the dispatcher, the voice routes and the
//! four quiet background samplers (foreground program, temperatures, clipboard, routines).

use crate::features::{spoken, str_arg};
use crate::integration_router::{Mode, Route};
use crate::tools::{ToolContext, ToolOutcome};
use regex::Regex;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

/// Shared state of the workbench (cheap to clone).
#[derive(Clone)]
pub struct Workbench {
    pub cards: crate::flashcards::Deck,
    pub screen: crate::screen_time::Tracker,
    pub routines: crate::routine_learner::Routines,
    pub thermals: crate::thermal_advisor::History,
    pub clips: crate::clipboard_history::Ring,
}

impl Workbench {
    pub fn open(db: &Path) -> Self {
        let warn = |what: &str, e: String| tracing::warn!("[WORKBENCH] {what} cannot be persisted ({e}); it lasts for this session only.");
        Self {
            cards: crate::flashcards::Deck::open(db).unwrap_or_else(|e| {
                warn("Flashcards", e);
                crate::flashcards::Deck::in_memory()
            }),
            screen: crate::screen_time::Tracker::open(db).unwrap_or_else(|e| {
                warn("Screen time", e);
                crate::screen_time::Tracker::in_memory()
            }),
            routines: crate::routine_learner::Routines::open(db).unwrap_or_else(|e| {
                warn("Routines", e);
                crate::routine_learner::Routines::in_memory()
            }),
            thermals: crate::thermal_advisor::History::default(),
            clips: crate::clipboard_history::Ring::default(),
        }
    }

    pub fn in_memory() -> Self {
        Self {
            cards: crate::flashcards::Deck::in_memory(),
            screen: crate::screen_time::Tracker::in_memory(),
            routines: crate::routine_learner::Routines::in_memory(),
            thermals: crate::thermal_advisor::History::default(),
            clips: crate::clipboard_history::Ring::default(),
        }
    }

    /// The four samplers: the program in front (15 s), the temperatures (10 s), the clipboard
    /// (when it changes) and the routine offers (every five minutes). All of them only read.
    pub fn start_background(&self, tools: &ToolContext) {
        let (bench, monitor) = (self.clone(), tools.monitor.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(crate::thermal_advisor::SAMPLE_SECS));
            loop {
                tick.tick().await;
                let t = monitor.latest();
                bench.thermals.push(crate::thermal_advisor::Sample {
                    cpu_temp: t.cpu_temp_c,
                    cpu_load: t.cpu_usage_pct,
                    gpu_temp: t.gpu.as_ref().and_then(|g| g.temperature_c),
                    gpu_load: t.gpu.as_ref().and_then(|g| g.utilization_pct),
                });
            }
        });
        #[cfg(windows)]
        {
            let bench = self.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(crate::screen_time::SAMPLE_SECS));
                loop {
                    tick.tick().await;
                    let Some((process, _)) = crate::win32::foreground_info() else { continue };
                    let idle = crate::win32::idle_seconds().unwrap_or(0);
                    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
                    if let Some(stretch) = bench.screen.sample(&day, &process, idle, crate::win32::foreground_is_fullscreen()) {
                        crate::proactive_butler::show(&break_card(stretch));
                    }
                }
            });
            let bench = self.clone();
            tokio::spawn(async move {
                let mut seen = crate::win32::clipboard_sequence();
                let mut tick = tokio::time::interval(Duration::from_millis(1200));
                loop {
                    tick.tick().await;
                    let now = crate::win32::clipboard_sequence();
                    if now == seen || bench.clips.paused() {
                        seen = now;
                        continue;
                    }
                    seen = now;
                    if let Ok(text) = crate::clipboard::read().await {
                        bench.clips.note(&text);
                    }
                }
            });
        }
        let bench = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(300));
            loop {
                tick.tick().await;
                // Never over a game or a film, and not while Sir is not there to see it.
                #[cfg(windows)]
                if crate::win32::foreground_is_fullscreen() || crate::win32::idle_seconds().unwrap_or(0) > 300 {
                    continue;
                }
                if let Some(habit) = bench.routines.offer(chrono::Local::now().naive_local()) {
                    crate::proactive_butler::show(&routine_card(&habit));
                }
            }
        });
    }

    /// A line for the live context while a flashcard quiz runs.
    pub fn live_context(&self) -> Option<String> {
        let card = self.cards.get(self.cards.asking()?)?;
        Some(format!("Flashcard quiz running (card: \"{}\"). Sir's answer goes to manage_flashcards with action answer.", card.front))
    }
}

/// The break coach's card: silent, with one button that turns the coach off.
pub fn break_card(stretch_secs: u64) -> crate::proactive_butler::Card {
    crate::proactive_butler::Card {
        title: "BREAK COACH".to_string(),
        body: format!("{} at the screen without a break, Sir. Five minutes away would do you good.", crate::screen_time::span(stretch_secs)),
        fixes: vec![crate::proactive_butler::Fix { label: "COACH OFF".to_string(), tool: "screen_time_report", args: json!({ "action": "coach_off" }) }],
        spoken: None,
    }
}

/// A routine's offer: one button that carries it out.
pub fn routine_card(habit: &crate::routine_learner::Habit) -> crate::proactive_butler::Card {
    crate::proactive_butler::Card {
        title: "ROUTINE".to_string(),
        body: format!("Around {} you usually {}, Sir ({} days running).", crate::routine_learner::clock(habit.usual_minute), habit.label, habit.days),
        fixes: vec![crate::proactive_butler::Fix { label: "DO IT".to_string(), tool: "suggest_routine", args: json!({ "action": "run" }) }],
        spoken: None,
    }
}

// --------------------------------------------------------------------------- tools ----

/// Every workbench tool.
pub const TOOLS: &[&str] = &["manage_flashcards", "lookup_formula", "chemistry_helper", "show_app_shortcuts", "screen_time_report", "suggest_routine", "thermal_advisor", "clipboard_history", "tile_windows", "suggest_commands"];

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// The tool name as `'static` (routes and fast-path intents need one).
pub fn static_name(name: &str) -> Option<&'static str> {
    TOOLS.iter().copied().find(|t| *t == name)
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        crate::flashcards::tool_definition(),
        crate::formula_sheet::tool_definition(),
        crate::chem_helper::tool_definition(),
        crate::shortcut_overlay::tool_definition(),
        crate::screen_time::tool_definition(),
        crate::routine_learner::tool_definition(),
        crate::thermal_advisor::tool_definition(),
        crate::clipboard_history::tool_definition(),
        crate::window_tiler::tool_definition(),
        json!({"type": "function", "function": {
            "name": "suggest_commands",
            "description": "Commands Jarvis understands that begin with or contain the given words (the text console's predictive completion): 'what can I say that starts with open'.",
            "parameters": {"type": "object", "properties": {"prefix": {"type": "string", "description": "The first words or letters of a command."}}, "required": ["prefix"]}
        }}),
    ]
}

/// A routine's action as a tool call of its own. Boxed: this hub is itself reached through
/// `ToolContext::execute`, whose future would otherwise have to contain itself.
fn carry_out(tools: ToolContext, tool: String, args: Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send>> {
    Box::pin(async move { tools.execute(&tool, &args).await })
}

pub async fn run_tool(tools: &ToolContext, name: &str, args: &Value) -> Result<String, String> {
    let bench = &tools.stark.features.bench;
    tools.stark.pulse(&["brain", "context"], false);
    match name {
        "manage_flashcards" => bench.cards.tool(tools, args),
        "lookup_formula" => {
            let said = crate::formula_sheet::run_tool(args)?;
            show(tools, "formula_sheet", "Formula sheet", &said, 620.0, 360.0);
            Ok(said)
        }
        "chemistry_helper" => crate::chem_helper::run_tool(args),
        "show_app_shortcuts" => {
            #[cfg(windows)]
            let front = crate::win32::user_window().map(|w| (w.process, w.title));
            #[cfg(not(windows))]
            let front: Option<(String, String)> = None;
            let said = crate::shortcut_overlay::run_tool(args, front)?;
            show(tools, "shortcut_overlay", "Shortcuts", &said, 620.0, 520.0);
            Ok(said)
        }
        "screen_time_report" => {
            let said = bench.screen.tool(args)?;
            if !matches!(str_arg(args, &["action"]).unwrap_or("today"), "coach_on" | "coach_off") {
                show(tools, "screen_time", "Screen time", &said, 560.0, 380.0);
            }
            Ok(said)
        }
        "suggest_routine" => {
            let now = chrono::Local::now().naive_local();
            if str_arg(args, &["action"]).is_some_and(|a| matches!(a.to_lowercase().as_str(), "run" | "do" | "go")) {
                let habit = crate::routine_learner::habits_now(&bench.routines.events(), now).into_iter().next().ok_or("There's nothing you usually do at this hour, Sir.")?;
                let outcome = carry_out(tools.clone(), habit.tool.clone(), habit.args.clone()).await;
                return if outcome.success { Ok(outcome.output) } else { Err(outcome.output) };
            }
            bench.routines.tool(args, now)
        }
        "thermal_advisor" => {
            let said = bench.thermals.tool(args)?;
            show(tools, "thermal_advisor", "Thermal advisor", &said, 640.0, 360.0);
            Ok(said)
        }
        "clipboard_history" => {
            let (said, restore) = bench.clips.tool(args)?;
            match restore {
                Some(text) => crate::clipboard::write(&text).await?,
                None if !matches!(str_arg(args, &["action"]).unwrap_or("list"), "clear" | "pause" | "resume") => show(tools, "clipboard_history", "Clipboard history", &said, 620.0, 460.0),
                None => {}
            }
            Ok(said)
        }
        "tile_windows" => crate::window_tiler::run_tool(args).await,
        "suggest_commands" => {
            let prefix = str_arg(args, &["prefix", "text", "query"]).ok_or("Which words should they begin with, Sir?")?;
            let found = crate::command_palette::complete(prefix, &[], 8);
            if found.is_empty() {
                return Ok(spoken(format!("No command begins with '{prefix}'."), format!("I know no command that begins with {prefix}, Sir.")));
            }
            Ok(spoken(found.join("\n"), format!("For instance: {}.", found.iter().take(3).cloned().collect::<Vec<_>>().join("; "))))
        }
        other => Err(format!("Unknown workbench tool '{other}'.")),
    }
}

/// The display part of a tool result as a block on the board (the spoken line is not shown).
fn show(tools: &ToolContext, id: &str, title: &str, said: &str, width: f32, height: f32) {
    let display = said.split("\nSpoken:").next().unwrap_or(said).trim();
    // Each line its own paragraph, so the block keeps the table's rows.
    let markdown = display.lines().map(|l| if l.trim().is_empty() { String::new() } else { format!("`{}`", l.replace('`', "'")) }).collect::<Vec<_>>().join("\n\n");
    crate::features::show_window(tools, id, title, &markdown, width, height);
}

// ------------------------------------------------------------------------- voice routes ----

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid workbench route")
}

fn verbatim(tool: &'static str, args: Value) -> Option<Route> {
    Some(Route { tool, args, mode: Mode::Verbatim })
}

/// The sentence without the wake word, courtesies and closing punctuation, lower case.
fn core(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    let mut s = lower.trim_end_matches(['?', '.', '!', ',']).trim();
    for lead in ["hey jarvis", "jarvis", ",", "please", "bitte", "can you", "could you", "kannst du", "would you", "ok", "okay"] {
        s = s.trim_start_matches(lead).trim_start_matches([',', ' ']);
    }
    s.trim_end_matches(" please").trim_end_matches(" bitte").trim().to_string()
}

static CARD_ADD: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:add|create|make|new|neue|erstelle|füge)\s+(?:a\s+|an\s+|eine\s+)?(?:new\s+|neue\s+)?(?:flash\s?card|karteikarte|card)(?:\s+hinzu)?(?:\s+(?:to|in|for|zu|für)\s+(?:my\s+|the\s+|meinem\s+|dem\s+)?(.+?)(?:\s+deck|\s+stapel)?)?\s*[:,]\s*(.+?)\s+(?:means|is|equals|are|bedeutet|ist|heißt|sind)\s+(.+)$"));
static CARD_QUIZ: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:quiz me on|test me on|study|learn|review|start|lets do|let's do|do)\s+(?:my\s+|the\s+|some\s+)?(?:(.+?)\s+)?(?:flash\s?cards|cards|karteikarten)(?:\s+quiz)?$|^(?:flash\s?card|karteikarten)\s?(?:quiz|abfrage|time)$|^(?:frag|frage)\s+mich\s+(?:meine\s+|die\s+)?karteikarten\s+ab$|^karteikarten\s+(?:lernen|abfragen|üben)$"));
static CARD_DUE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:how many|which|what)\s+(?:flash\s?cards|cards)\s+(?:are|is)\s+due(?:\s+today)?$|^(?:wie viele|welche)\s+karteikarten\s+sind\s+(?:heute\s+)?fällig$|^(?:flash\s?card|karteikarten)\s?(?:stats|status|statistik)$"));
static CARD_ANSWER: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:the answer is|answer is|answer|my answer is|its|it's|it is|that is|that's|thats|die antwort ist|antwort|das ist|es ist)\s*[:,]?\s+(.+)$"));
static FORMULA: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:what is|what's|whats|show me|show|give me|tell me|whats|remind me of)\s+)?(?:the\s+)?(?:formula|equation)\s+(?:for|of|to calculate)\s+(?:the\s+)?(.+)$|^(?:show me|show|give me|what is|what's)\s+(?:the\s+)?(.+?)\s+(?:formula|equation)$|^(?:wie lautet|zeig mir|zeig|nenn mir|was ist)\s+(?:die\s+)?(?:formel|gleichung)\s+(?:für|der|von|zur)\s+(?:die\s+|den\s+|das\s+|der\s+)?(.+)$|^(?:die\s+)?(?:formel|gleichung)\s+(?:für|der|von)\s+(?:die\s+|den\s+|das\s+|der\s+)?(.+)$"));
static MOLAR: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:what is|what's|whats|calculate|give me|tell me|was ist|wie groß ist|berechne)?\s*(?:the\s+|die\s+)?(?:molar mass|molecular weight|molecular mass|molare masse|molmasse)\s+(?:of|von|für)\s+(.+)$"));
static MOLES: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:how many|wie viel|wie viele)\s+(?:moles?|mol)\s+(?:are\s+in|is\s+in|sind\s+in|ist\s+in|are|is|in|sind|ist)\s+(\d+(?:[.,]\d+)?)\s*(?:grams?|g|gramm)\s+(?:of\s+)?(.+)$"));
static GRAMS: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:how many|wie viel|wie viele)\s+(?:grams?|gramm)\s+(?:are\s+in|is\s+in|sind\s+in|ist\s+in|are|is|in|sind|ist|do|weigh)\s+(\d+(?:[.,]\d+)?)\s*(?:moles?|mol)\s+(?:of\s+)?(.+?)(?:\s+weigh)?$"));
static COMPOSITION: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:what is|what's|whats|show me|was ist)?\s*(?:the\s+|die\s+)?(?:mass\s+)?(?:composition|zusammensetzung)\s+(?:of|von)\s+(.+)$"));
static SHORTCUT_SHEET: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show me|show|open|list|give me|zeig mir|zeig)?\s*(?:the\s+|all\s+|die\s+)?(?:keyboard\s+)?(?:shortcuts|shortcut sheet|hotkeys|cheat sheet|tastenkürzel|tastenkombinationen)(?:\s+(?:for|in|of|für)\s+(.+))?$"));
static SHORTCUT_FIND: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:what is|what's|whats|which is|tell me|was ist|wie ist|wie lautet)?\s*(?:the\s+|a\s+|der\s+|die\s+|das\s+)?(?:keyboard\s+)?(?:shortcut|hotkey|key combination|tastenkürzel|tastenkombination)\s+(?:for|to|für|zum|zu)\s+(.+?)(?:\s+(?:in|on)\s+(.+))?$"));
static SCREEN_STRETCH: LazyLock<Regex> = LazyLock::new(|| re(r"^how long (?:have i been|am i) (?:sitting )?(?:at|on|in front of) (?:the|my|this) (?:screen|pc|computer|desk)(?: (?:today|now|already))?$|^how long (?:since|without) (?:my last|a) break$|^wie lange (?:sitze|bin) ich (?:schon |jetzt )*(?:am|vor dem) (?:bildschirm|pc|computer|rechner)$|^wann war meine letzte pause$"));
static SCREEN_TODAY: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:what|which programs|which apps) (?:did|have) i (?:use|used|been using) today$|^(?:show me |show |whats |what's |what is )?(?:my )?screen ?time(?: today| for today)?$|^(?:meine )?bildschirmzeit(?: heute)?$|^was habe ich heute (?:benutzt|verwendet|gemacht am pc)$"));
static SCREEN_WEEK: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show me |show |whats |what's |what is )?(?:my )?screen ?time (?:this|for the|of the|last) week$|^(?:meine )?bildschirmzeit (?:diese|der|letzte) woche$"));
static COACH: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:turn|switch|schalte?)?\s*(on|off|an|aus|ein)?\s*(?:the\s+|den\s+)?(?:break|pausen)[ -]?coach(?:\s+(on|off|an|aus|ein))?$"));
static ROUTINE_NOW: LazyLock<Regex> = LazyLock::new(|| re(r"^what do i (?:usually|normally|typically) do (?:now|at this time|around now|at this hour|about now)$|^(?:whats|what's|what is) my (?:usual )?routine(?: now| right now)?$|^was mache ich (?:normalerweise|sonst|gewöhnlich|üblicherweise)(?: immer)? (?:jetzt|um diese zeit|um die zeit)$"));
static ROUTINE_LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show me|show|list|what are|zeig mir|zeig)\s+(?:all\s+)?(?:my|meine)\s+(?:routines|habits|routinen|gewohnheiten)$|^(?:my routines|meine routinen)$"));
static ROUTINE_RUN: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:do|run|start)\s+(?:my|the)\s+(?:usual|usual thing|routine)$|^the usual$|^(?:das übliche|wie immer|mach das übliche)$"));
static ROUTINE_FORGET: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:forget|reset|clear|vergiss)\s+(?:all\s+)?(?:my|meine)\s+(?:routines|habits|routinen|gewohnheiten)$"));
static THERMAL: LazyLock<Regex> = LazyLock::new(|| re(r"^how are (?:my|the) (?:temperatures|temps|thermals)$|^(?:is|are) (?:my|the) (?:pc|computer|system|cpu|gpu|graphics card)(?: and (?:cpu|gpu))? (?:running |getting |too )*(?:hot|warm|overheating)$|^(?:show me |show |give me )?(?:the |a |my )?(?:thermal|temperature) (?:report|advisor|advice|check|status)$|^(?:check|show)(?: me)? (?:my|the) (?:temperatures|temps|thermals)$|^thermals$|^wie sind (?:meine|die) temperaturen$|^(?:wird|ist|läuft) mein (?:pc|rechner|computer)(?: zu)? (?:heiß|warm)$|^temperatur(?:en)?(?: ?bericht| ?check)$"));
static FAN_CURVE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:give me |show me |what |which |suggest |recommend )?(?:a |the |my )?(?:good |better )?fan curve(?: advice| should i use| do you recommend| for my (?:pc|gpu|cpu))?$|^(?:fan curve|lüfterkurve)(?: advice| empfehlung| vorschlag)?$|^(?:welche|was für eine) lüfterkurve(?: empfiehlst du| soll ich nehmen)?$"));
static CLIP_LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:show me|show|open|list|zeig mir|zeig)?\s*(?:my\s+|the\s+|meinen\s+|den\s+)?(?:clipboard history|clipboard verlauf|zwischenablage[- ]?verlauf|copy history)$|^what did i copy(?: before| earlier| before that| last)?$|^was habe ich (?:vorher |davor |zuletzt )?kopiert$"));
static CLIP_CONTROL: LazyLock<Regex> = LazyLock::new(|| re(r"^(clear|empty|forget|pause|stop|resume|continue|lösche|leere|pausiere)\s+(?:my\s+|the\s+|den\s+|meinen\s+)?(?:clipboard history|zwischenablage[- ]?verlauf|copy history)$"));
static CLIP_RESTORE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:restore|copy|bring back|put back)\s+(?:the\s+)?(?:(first|second|third|fourth|fifth|1st|2nd|3rd|4th|5th|last|previous|one before)\s+)?(?:clipboard\s+)?(?:entry|copy|thing i copied|clipboard)(?:\s+(?:number\s+)?(\d{1,2}))?(?:\s+again)?$|^copy (?:the\s+)?(?:previous|last) (?:thing|one)(?: again)?$"));
static TILE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:tile|arrange|organi[sz]e|put|place|line up|snap|sort)\s+(?:all\s+)?my\s+(?:open\s+)?(?:windows|apps|programs|programme)(?:\s+(.+))?$|^(?:ordne|kachel|sortiere)\s+meine\s+(?:fenster|programme)(?:\s+(.+))?$|^(?:meine\s+)?fenster\s+(nebeneinander|untereinander|im raster|kacheln)(?:\s+(.+))?$"));
static COMPLETE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:what can i say|which commands?|what commands?)\s+(?:that\s+)?(?:starts?|begins?|beginning|starting)\s+with\s+(.+)$|^(?:complete|autocomplete|suggest commands for|commands (?:for|with|starting with))\s+(.+)$|^welche befehle (?:beginnen|fangen) mit\s+(.+?)(?:\s+an)?$"));

/// The 1-based clipboard entry an ordinal means ("second" → 2, "last" → 1, "previous" → 2).
fn ordinal(word: &str) -> usize {
    match word {
        "first" | "1st" | "last" => 1,
        "third" | "3rd" => 3,
        "fourth" | "4th" => 4,
        "fifth" | "5th" => 5,
        // "second", "previous", "the one before".
        _ => 2,
    }
}

/// "on monitor 2", "on the left screen", "auf dem zweiten bildschirm": the monitor named.
fn monitor_in(lower: &str) -> Option<String> {
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let i = words.iter().position(|w| matches!(*w, "monitor" | "screen" | "display" | "bildschirm"))?;
    let near = &words[i.saturating_sub(2)..(i + 2).min(words.len())];
    [("left", "left"), ("linken", "left"), ("right", "right"), ("rechten", "right"), ("primary", "primary"), ("main", "primary"), ("haupt", "primary"), ("first", "1"), ("ersten", "1"), ("second", "2"), ("zweiten", "2"), ("third", "3"), ("dritten", "3"), ("1", "1"), ("2", "2"), ("3", "3")]
        .iter()
        .find(|(word, _)| near.contains(word))
        .map(|(_, value)| value.to_string())
}

/// Deterministic routes for the workbench (asked before the field systems' and the integration
/// routes: "quiz me on my flashcards" is the deck, never an oral exam about flashcards).
pub fn route_text(text: &str) -> Option<Route> {
    let s = core(text);
    if s.is_empty() || s.split_whitespace().count() > 40 {
        return None;
    }
    let group = |c: &regex::Captures<'_>, from: usize| -> Option<String> { (from..c.len()).find_map(|i| c.get(i)).map(|m| m.as_str().trim().to_string()).filter(|g| !g.is_empty()) };

    // ---- A running flashcard quiz hears its answers first ----
    if crate::flashcards::quiz_running() {
        if matches!(s.as_str(), "i dont know" | "i don't know" | "no idea" | "show me the answer" | "show the answer" | "reveal" | "reveal it" | "flip the card" | "flip it" | "keine ahnung" | "weiß ich nicht" | "zeig die antwort" | "zeig mir die antwort" | "auflösen") {
            return verbatim("manage_flashcards", json!({ "action": "reveal" }));
        }
        if matches!(s.as_str(), "i got it right" | "i knew it" | "i had it" | "correct" | "right" | "richtig" | "wusste ich" | "hatte ich" | "yes i had it") {
            return verbatim("manage_flashcards", json!({ "action": "answer", "correct": true }));
        }
        if matches!(s.as_str(), "i got it wrong" | "wrong" | "i didnt know" | "i didn't know" | "falsch" | "wusste ich nicht" | "hatte ich nicht" | "no i didnt" | "no i didn't") {
            return verbatim("manage_flashcards", json!({ "action": "answer", "correct": false }));
        }
        if matches!(s.as_str(), "next card" | "skip" | "skip it" | "skip this one" | "next one" | "nächste karte" | "überspringen") {
            return verbatim("manage_flashcards", json!({ "action": "next" }));
        }
        if matches!(s.as_str(), "stop the quiz" | "end the quiz" | "stop quizzing me" | "thats enough" | "that's enough" | "enough for now" | "quiz beenden" | "genug" | "das reicht" | "hör auf mit dem quiz") {
            return verbatim("manage_flashcards", json!({ "action": "stop" }));
        }
        if let Some(c) = CARD_ANSWER.captures(&s) {
            return verbatim("manage_flashcards", json!({ "action": "answer", "answer": c[1].trim() }));
        }
    }

    // ---- Flashcards ----
    if let Some(c) = CARD_ADD.captures(&s) {
        // Sir's own capitals for the card ("Mitosis", "ATP").
        let original = |part: &str| text.to_lowercase().find(part).and_then(|i| text.get(i..i + part.len())).unwrap_or(part).trim().trim_end_matches(['.', '!']).to_string();
        let deck = c.get(1).map(|m| original(m.as_str()));
        return verbatim("manage_flashcards", json!({ "action": "add", "front": original(&c[2]), "back": original(&c[3]), "deck": deck }));
    }
    if let Some(c) = CARD_QUIZ.captures(&s) {
        let deck = c.get(1).map(|m| m.as_str().trim()).filter(|d| !matches!(*d, "my" | "the" | "all" | "some"));
        return verbatim("manage_flashcards", json!({ "action": "quiz", "deck": deck }));
    }
    if CARD_DUE.is_match(&s) {
        return verbatim("manage_flashcards", json!({ "action": "due" }));
    }

    // ---- Chemistry (before the formula sheet: "molar mass of" is arithmetic, not a formula) ----
    if let Some(c) = MOLES.captures(&s) {
        if crate::chem_helper::formula_from(c[2].trim()).is_some() {
            return verbatim("chemistry_helper", json!({ "action": "moles", "formula": c[2].trim(), "grams": c[1].replace(',', ".").parse::<f64>().ok() }));
        }
    }
    if let Some(c) = GRAMS.captures(&s) {
        if crate::chem_helper::formula_from(c[2].trim()).is_some() {
            return verbatim("chemistry_helper", json!({ "action": "grams", "formula": c[2].trim(), "moles": c[1].replace(',', ".").parse::<f64>().ok() }));
        }
    }
    for (pattern, action) in [(&*MOLAR, "molar_mass"), (&*COMPOSITION, "composition")] {
        if let Some(c) = pattern.captures(&s) {
            if crate::chem_helper::formula_from(c[1].trim()).is_some() {
                return verbatim("chemistry_helper", json!({ "action": action, "formula": c[1].trim() }));
            }
        }
    }

    // ---- The formula sheet (only for what is on it) ----
    if let Some(c) = FORMULA.captures(&s) {
        if let Some(asked) = group(&c, 1).filter(|q| !crate::formula_sheet::lookup(q, None).is_empty()) {
            return verbatim("lookup_formula", json!({ "query": asked }));
        }
    }

    // ---- Shortcuts ----
    if let Some(c) = SHORTCUT_FIND.captures(&s) {
        let what = c[1].trim().to_string();
        let app = c.get(2).map(|m| m.as_str().trim().to_string());
        // "The shortcut for VS Code" is that program's sheet.
        if app.is_none() && crate::shortcut_overlay::named(&what).is_some() {
            return verbatim("show_app_shortcuts", json!({ "app": what }));
        }
        // Only what the sheets hold, in a program they know ("the shortcut to happiness in
        // life" is a question for the model).
        let sheet = app.as_deref().and_then(crate::shortcut_overlay::named);
        let stranger = app.is_some() && sheet.is_none();
        if !stranger && !crate::shortcut_overlay::find(&what, sheet).is_empty() {
            return verbatim("show_app_shortcuts", json!({ "query": what, "app": app }));
        }
    }
    if let Some(c) = SHORTCUT_SHEET.captures(&s) {
        let app = c.get(1).map(|m| m.as_str().trim()).filter(|a| !matches!(*a, "this app" | "this program" | "this" | "here" | "this window" | "dieses programm" | "hier"));
        return verbatim("show_app_shortcuts", json!({ "app": app }));
    }

    // ---- Screen time and the break coach ----
    if SCREEN_STRETCH.is_match(&s) {
        return verbatim("screen_time_report", json!({ "action": "break" }));
    }
    if SCREEN_WEEK.is_match(&s) {
        return verbatim("screen_time_report", json!({ "action": "week" }));
    }
    if SCREEN_TODAY.is_match(&s) {
        return verbatim("screen_time_report", json!({ "action": "today" }));
    }
    if let Some(c) = COACH.captures(&s) {
        if let Some(word) = c.get(1).or(c.get(2)) {
            let on = matches!(word.as_str(), "on" | "an" | "ein");
            return verbatim("screen_time_report", json!({ "action": if on { "coach_on" } else { "coach_off" } }));
        }
    }

    // ---- Routines ----
    if ROUTINE_NOW.is_match(&s) {
        return verbatim("suggest_routine", json!({ "action": "now" }));
    }
    if ROUTINE_LIST.is_match(&s) {
        return verbatim("suggest_routine", json!({ "action": "list" }));
    }
    if ROUTINE_RUN.is_match(&s) {
        return verbatim("suggest_routine", json!({ "action": "run" }));
    }
    if ROUTINE_FORGET.is_match(&s) {
        return verbatim("suggest_routine", json!({ "action": "forget" }));
    }

    // ---- Thermals ----
    if FAN_CURVE.is_match(&s) {
        return verbatim("thermal_advisor", json!({ "action": "advice" }));
    }
    if THERMAL.is_match(&s) {
        return verbatim("thermal_advisor", json!({ "action": "status" }));
    }

    // ---- Clipboard history ----
    if CLIP_LIST.is_match(&s) {
        return verbatim("clipboard_history", json!({ "action": "list" }));
    }
    if let Some(c) = CLIP_CONTROL.captures(&s) {
        let action = match &c[1] {
            "pause" | "stop" | "pausiere" => "pause",
            "resume" | "continue" => "resume",
            _ => "clear",
        };
        return verbatim("clipboard_history", json!({ "action": action }));
    }
    if let Some(c) = CLIP_RESTORE.captures(&s) {
        let index = c.get(2).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or_else(|| ordinal(c.get(1).map_or("previous", |m| m.as_str())));
        return verbatim("clipboard_history", json!({ "action": "restore", "index": index }));
    }

    // ---- The window tiler ----
    if let Some(c) = TILE.captures(&s) {
        let how = group(&c, 1).unwrap_or_default();
        let layout = crate::window_tiler::parse_layout(&how).map(|l| match l {
            crate::window_tiler::Layout::SideBySide => "side_by_side",
            crate::window_tiler::Layout::Rows => "rows",
            crate::window_tiler::Layout::Grid => "grid",
            crate::window_tiler::Layout::MainAndStack => "main_and_stack",
        });
        return verbatim("tile_windows", json!({ "layout": layout, "monitor": monitor_in(&how) }));
    }

    // ---- The command palette ----
    if let Some(c) = COMPLETE.captures(&s) {
        if let Some(prefix) = group(&c, 1) {
            return verbatim("suggest_commands", json!({ "prefix": prefix }));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(text: &str) -> Option<(&'static str, Value)> {
        route_text(text).map(|r| (r.tool, r.args))
    }

    /// The route for `text` must be `tool` with at least `wanted`'s fields.
    fn expect(text: &str, tool: &str, wanted: Value) {
        let (got_tool, got) = route(text).unwrap_or_else(|| panic!("no route for '{text}'"));
        assert_eq!(got_tool, tool, "{text}: {got}");
        for (key, value) in wanted.as_object().into_iter().flatten() {
            assert_eq!(&got[key], value, "{text}: {key} in {got}");
        }
    }

    #[test]
    fn every_system_answers_in_english() {
        expect("Add a flashcard: Mitosis means cell division into two identical cells", "manage_flashcards", json!({"action": "add", "front": "Mitosis", "back": "cell division into two identical cells"}));
        expect("Jarvis, new card to my Biology deck: ATP is the energy currency of the cell.", "manage_flashcards", json!({"action": "add", "front": "ATP", "back": "the energy currency of the cell", "deck": "Biology"}));
        expect("Quiz me on my flashcards", "manage_flashcards", json!({"action": "quiz"}));
        expect("study my biology flashcards", "manage_flashcards", json!({"action": "quiz", "deck": "biology"}));
        expect("How many flashcards are due?", "manage_flashcards", json!({"action": "due"}));
        expect("Show me the formula for kinetic energy", "lookup_formula", json!({"query": "kinetic energy"}));
        expect("What is the formula for the pq formula?", "lookup_formula", json!({}));
        expect("What is the molar mass of H2SO4?", "chemistry_helper", json!({"action": "molar_mass", "formula": "h2so4"}));
        expect("How many moles are 36 grams of water?", "chemistry_helper", json!({"action": "moles", "formula": "water", "grams": 36.0}));
        expect("how many grams are 2 moles of NaCl", "chemistry_helper", json!({"action": "grams", "moles": 2.0}));
        expect("Show shortcuts for this app", "show_app_shortcuts", json!({"app": null}));
        expect("show me the shortcuts for VS Code", "show_app_shortcuts", json!({"app": "vs code"}));
        expect("What is the shortcut for split screen?", "show_app_shortcuts", json!({"query": "split screen"}));
        expect("what's the shortcut to mute the microphone in discord", "show_app_shortcuts", json!({"query": "mute the microphone", "app": "discord"}));
        expect("How long have I been at the screen?", "screen_time_report", json!({"action": "break"}));
        expect("What did I use today?", "screen_time_report", json!({"action": "today"}));
        expect("screen time this week", "screen_time_report", json!({"action": "week"}));
        expect("turn off the break coach", "screen_time_report", json!({"action": "coach_off"}));
        expect("break coach on", "screen_time_report", json!({"action": "coach_on"}));
        expect("What do I usually do now?", "suggest_routine", json!({"action": "now"}));
        expect("show me my routines", "suggest_routine", json!({"action": "list"}));
        expect("do my usual", "suggest_routine", json!({"action": "run"}));
        expect("How are my temperatures?", "thermal_advisor", json!({"action": "status"}));
        expect("Is my PC running hot?", "thermal_advisor", json!({"action": "status"}));
        expect("fan curve advice", "thermal_advisor", json!({"action": "advice"}));
        expect("Show my clipboard history", "clipboard_history", json!({"action": "list"}));
        expect("what did I copy before that", "clipboard_history", json!({"action": "list"}));
        expect("clear the clipboard history", "clipboard_history", json!({"action": "clear"}));
        expect("pause the clipboard history", "clipboard_history", json!({"action": "pause"}));
        expect("restore the third clipboard entry", "clipboard_history", json!({"action": "restore", "index": 3}));
        expect("Tile my windows side by side", "tile_windows", json!({"layout": "side_by_side"}));
        expect("put my windows in a grid on monitor 2", "tile_windows", json!({"layout": "grid", "monitor": "2"}));
        expect("arrange my windows", "tile_windows", json!({"layout": null}));
        expect("what can I say that starts with open", "suggest_commands", json!({"prefix": "open"}));
    }

    #[test]
    fn every_system_answers_in_german() {
        expect("Neue Karteikarte: Mitose bedeutet Zellteilung in zwei identische Zellen", "manage_flashcards", json!({"action": "add", "front": "Mitose", "back": "Zellteilung in zwei identische Zellen"}));
        expect("Frag mich meine Karteikarten ab", "manage_flashcards", json!({"action": "quiz"}));
        expect("Wie viele Karteikarten sind fällig?", "manage_flashcards", json!({"action": "due"}));
        expect("Zeig mir die Formel für die kinetische Energie", "lookup_formula", json!({}));
        expect("Wie lautet die Formel für die Kettenregel?", "lookup_formula", json!({"query": "kettenregel"}));
        expect("Molare Masse von Kochsalz", "chemistry_helper", json!({"action": "molar_mass", "formula": "kochsalz"}));
        expect("Wie viel Mol sind 36 Gramm Wasser?", "chemistry_helper", json!({"action": "moles", "grams": 36.0}));
        expect("Zeig mir die Tastenkürzel", "show_app_shortcuts", json!({"app": null}));
        expect("Tastenkürzel für Screenshot", "show_app_shortcuts", json!({"query": "screenshot"}));
        expect("Wie lange sitze ich schon am Bildschirm?", "screen_time_report", json!({"action": "break"}));
        expect("Bildschirmzeit diese Woche", "screen_time_report", json!({"action": "week"}));
        expect("Pausen-Coach aus", "screen_time_report", json!({"action": "coach_off"}));
        expect("Was mache ich normalerweise um diese Zeit?", "suggest_routine", json!({"action": "now"}));
        expect("Wie immer", "suggest_routine", json!({"action": "run"}));
        expect("Wie sind meine Temperaturen?", "thermal_advisor", json!({"action": "status"}));
        expect("Lüfterkurve Empfehlung", "thermal_advisor", json!({"action": "advice"}));
        expect("Zeig meinen Zwischenablage-Verlauf", "clipboard_history", json!({"action": "list"}));
        expect("Was habe ich vorher kopiert?", "clipboard_history", json!({"action": "list"}));
        expect("Ordne meine Fenster nebeneinander", "tile_windows", json!({"layout": "side_by_side"}));
        expect("Welche Befehle beginnen mit Theme?", "suggest_commands", json!({"prefix": "theme"}));
    }

    #[test]
    fn other_peoples_sentences_are_left_alone() {
        for text in [
            "Quiz me on photosynthesis",
            "What is the formula for a good life?",
            "What is the molar mass of my patience?",
            "Tidy the windows",
            "Arrange the windows",
            "Close the windows",
            "Play some music",
            "What time is it?",
            "What's the shortcut to happiness in life and love and everything else that matters",
            "Open my WebUntis schedule",
            "How hot is the sun?",
            "the answer is 42",
            "correct",
            "",
        ] {
            assert_eq!(route(text), None, "{text}");
        }
    }

    #[test]
    fn a_running_quiz_hears_its_answers() {
        let bench = Workbench::in_memory();
        bench.cards.run(&json!({"action": "add", "front": "Mitosis", "back": "cell division"}), 1_800_000_000).unwrap();
        bench.cards.run(&json!({"action": "quiz"}), 1_800_000_000).unwrap();
        assert!(crate::flashcards::quiz_running());
        expect("The answer is cell division", "manage_flashcards", json!({"action": "answer", "answer": "cell division"}));
        expect("It's cell division.", "manage_flashcards", json!({"action": "answer", "answer": "cell division"}));
        expect("I don't know", "manage_flashcards", json!({"action": "reveal"}));
        expect("I got it right", "manage_flashcards", json!({"action": "answer", "correct": true}));
        expect("falsch", "manage_flashcards", json!({"action": "answer", "correct": false}));
        expect("next card", "manage_flashcards", json!({"action": "next"}));
        expect("That's enough", "manage_flashcards", json!({"action": "stop"}));
        assert!(bench.live_context().is_some_and(|l| l.contains("Mitosis")));
        bench.cards.run(&json!({"action": "stop"}), 1_800_000_000).unwrap();
        assert!(!crate::flashcards::quiz_running() && bench.live_context().is_none());
        assert_eq!(route("the answer is cell division"), None, "no quiz, no answer");
    }

    #[test]
    fn every_tool_is_defined_once_and_the_cards_carry_valid_calls() {
        let defs = tool_definitions();
        let names: Vec<&str> = defs.iter().filter_map(|d| d["function"]["name"].as_str()).collect();
        assert_eq!(names, TOOLS, "the definitions and the tool list agree, in order");
        for def in &defs {
            assert!(def["function"]["description"].as_str().is_some_and(|d| d.len() > 30), "{def}");
            assert_eq!(def["function"]["parameters"]["type"], "object");
        }
        assert!(is_tool("thermal_advisor") && !is_tool("web_search"));
        assert_eq!(static_name("tile_windows"), Some("tile_windows"));
        for card in [break_card(3000), routine_card(&crate::routine_learner::Habit { tool: "open_jarvis_view".into(), args: json!({"view": "schedule"}), label: "open the schedule view".into(), days: 3, usual_minute: 430 })] {
            assert!(!card.body.is_empty() && card.spoken.is_none(), "the cards are silent");
            for fix in &card.fixes {
                assert!(is_tool(fix.tool), "{}", fix.tool);
                crate::live_self_tester::check_call(&defs, fix.tool, &fix.args).unwrap_or_else(|e| panic!("{}: {e}", fix.label));
            }
        }
        assert!(routine_card(&crate::routine_learner::Habit { tool: "x".into(), args: json!({}), label: "open the schedule view".into(), days: 3, usual_minute: 430 }).body.contains("Around 07:10 you usually open the schedule view"));
    }
}
