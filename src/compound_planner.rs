//! Several commands in one breath: "Switch back to Jarvis, set theme to Arc Cyan, move the orb
//! to the top right, and open my WebUntis schedule."
//!
//! One router answering for the whole sentence carries out the first thing it recognises and
//! drops the rest (the Ultron stage took the sentence above and the theme, the orb and the
//! schedule never happened). Here the sentence is cut into its clauses, every clause is resolved
//! on its own by the same stateless parsers a single command goes through, and only when every
//! one of them resolves is the sentence a compound: the dispatcher switches mode and privacy
//! itself, the brain runs the other clauses as tool calls — side by side where they touch
//! different things, in order where "then" says so.
//!
//! A sentence with a clause nothing recognises is not a compound and goes the way it always
//! went ("play rock and roll", "remind me to buy milk, eggs and bread"). A clause that needs
//! Sir's confirmation (power, sending, deleting) never runs from a list.

use crate::intent_engine::{IntentAction, Reflex, ReflexContext};
use crate::intents::{self, Intent};
use crate::listening_lock::LockCommand;
use serde_json::{json, Value};

/// The most clauses one sentence is cut into (more is dictation, not a list of commands).
pub const MAX_CLAUSES: usize = 6;

/// One clause as a tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// Sir's words for it.
    pub clause: String,
    pub tool: &'static str,
    pub args: Value,
    /// Clauses of one stage run side by side; "then" begins the next stage.
    pub stage: usize,
}

/// A sentence of several commands, every one of them resolved.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    /// "Switch back to Jarvis" / "switch to Ultron": the dispatcher's.
    pub mode: Option<bool>,
    /// "Stop listening" / "you can listen again": the dispatcher's.
    pub privacy: Option<LockCommand>,
    /// Everything else, in the order it was said.
    pub calls: Vec<Call>,
}

impl Plan {
    /// The words of the clauses that are tool calls, as one request for the brain.
    pub fn rest(&self) -> String {
        let mut out = String::new();
        let mut stage = 0;
        for (i, call) in self.calls.iter().enumerate() {
            if i > 0 {
                out.push_str(if call.stage != stage { ", then " } else if i + 1 == self.calls.len() { " and " } else { ", " });
            }
            stage = call.stage;
            out.push_str(&call.clause);
        }
        out
    }

    /// How many things the sentence asks for.
    pub fn count(&self) -> usize {
        self.calls.len() + usize::from(self.mode.is_some()) + usize::from(self.privacy.is_some())
    }
}

/// Separators that start a later stage ("…, then …") and ones that only list.
const THEN: &[&str] = &[", and then ", " and then ", ", then ", " then ", " und dann ", ", dann ", " dann ", ", danach ", " danach ", " after that ", ", after that ", ", afterwards ", " afterwards "];
const AND: &[&str] = &["; ", ", and also ", " and also ", ", and ", " and ", ", und ", " und ", ", sowie ", " sowie ", ", "];

/// Words that are a clause to nobody ("Jarvis, …", "…, please").
fn is_courtesy(clause: &str) -> bool {
    let lower = clause.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    words.is_empty() || words.iter().all(|w| matches!(*w, "jarvis" | "hey" | "ok" | "okay" | "please" | "bitte" | "sir" | "now" | "jetzt" | "thanks" | "thank" | "you" | "danke" | "ultron" | "also" | "and" | "und" | "then" | "dann"))
}

/// Cuts `text` at the first of `separators` in every piece (ASCII separators, so byte offsets of
/// the lower-cased copy are the original's whenever the lengths agree).
fn cut(pieces: Vec<(String, bool)>, separators: &[&str], begins_stage: bool) -> Vec<(String, bool)> {
    let mut out = Vec::with_capacity(pieces.len() + 2);
    for (piece, staged) in pieces {
        let lower = piece.to_lowercase();
        if lower.len() != piece.len() {
            out.push((piece, staged));
            continue;
        }
        let mut from = 0;
        let mut first = true;
        loop {
            let next = separators.iter().filter_map(|s| lower[from..].find(s).map(|i| (from + i, s.len()))).min_by_key(|(i, len)| (*i, std::cmp::Reverse(*len)));
            match next {
                Some((at, len)) if piece.is_char_boundary(at) && piece.is_char_boundary(at + len) => {
                    out.push((piece[from..at].to_string(), if first { staged } else { begins_stage }));
                    first = false;
                    from = at + len;
                }
                _ => {
                    out.push((piece[from..].to_string(), if first { staged } else { begins_stage }));
                    break;
                }
            }
        }
    }
    out
}

/// The clauses of a sentence and whether each begins a later stage ("…, then …"). One clause:
/// an ordinary command.
pub fn clauses(text: &str) -> Vec<(String, bool)> {
    let lower = text.to_lowercase();
    if !THEN.iter().chain(AND).any(|s| lower.contains(s)) {
        return vec![(text.trim().to_string(), false)];
    }
    let pieces = cut(vec![(text.to_string(), false)], THEN, true);
    let pieces = cut(pieces, AND, false);
    let mut out: Vec<(String, bool)> = Vec::new();
    // A stage marker on a clause that turns out to be a courtesy moves to the next real clause.
    let mut carried = false;
    for (piece, staged) in pieces {
        let clause = piece.trim().trim_end_matches(['.', '!', '?', ',', ';']).trim().to_string();
        if is_courtesy(&clause) {
            carried |= staged;
            continue;
        }
        out.push((clause, (staged || carried) && !out.is_empty()));
        carried = false;
    }
    out
}

/// Tools that wait for Sir's "yes" or cannot be taken back: never run from a list.
fn needs_confirmation(tool: &str) -> bool {
    ["shutdown", "power", "send", "reply", "delete", "cleanup", "restart", "lifecycle", "overwrite", "shell", "recompile", "forge", "beam", "teleport"].iter().any(|w| tool.contains(w))
}

/// What one clause asks for. A plain question only reads, whichever parser took it for an order:
/// what looks it up answers, or nothing does here.
fn resolve(clause: &str, ctx: &ReflexContext) -> Option<Resolved> {
    match resolve_any(clause, ctx)? {
        Resolved::Call(tool, args) if intents::beyond_the_question(clause, tool, &args) => match intents::parse(clause)? {
            Intent::Tool { name, args } if !intents::beyond_the_question(clause, name, &args) => Some(Resolved::Call(name, args)),
            _ => None,
        },
        found => Some(found),
    }
}

/// What one clause says, from the stateless parsers in the order a single command meets them:
/// the reflex router (mode, privacy, Jarvis' own controls), the deterministic actions, the HUD's
/// views and look, quick commands, the mixer, the deterministic routes, then the exact command
/// grammar.
fn resolve_any(clause: &str, ctx: &ReflexContext) -> Option<Resolved> {
    if let Some(routed) = crate::intent_engine::route(clause, ctx) {
        return Some(match routed.reflex {
            Reflex::Mode(on) => Resolved::Mode(on),
            Reflex::Privacy(cmd) => Resolved::Privacy(cmd),
            Reflex::Silent(action) => Resolved::Call(action.tool, action.args),
        });
    }
    if let Some(action) = crate::intent_engine::deterministic_action(clause) {
        return Some(match action {
            IntentAction::CloseSecondaryWindows => Resolved::Call("close_all_secondary_windows", json!({})),
            IntentAction::ExitJarvisApplication => Resolved::Call("shutdown_jarvis", json!({})),
        });
    }
    let of = |intent: Option<Intent>| match intent {
        Some(Intent::Tool { name, args }) => Some(Resolved::Call(name, args)),
        Some(Intent::Silent(action)) => Some(Resolved::Call(action.tool, action.args)),
        _ => None,
    };
    let call = |tool: &'static str| move |args: Value| Resolved::Call(tool, args);
    of(intents::hud_view_command(clause))
        .or_else(|| crate::oled_minimal_ui::voice_command(clause).map(call("configure_interface")))
        .or_else(|| crate::dynamic_popups::voice_command(clause).map(call("control_popups")))
        .or_else(|| intents::silent_quick_command(clause).map(|a| Resolved::Call(a.tool, a.args)))
        .or_else(|| intents::orb_mode_command(clause).map(call("control_orb_mode")))
        .or_else(|| intents::listening_mode_command(clause).map(call("set_listening_mode")))
        .or_else(|| intents::mixer_command(clause).map(call("control_audio_mixer")))
        .or_else(|| intents::internal_view_command(clause).map(call("open_jarvis_view")))
        .or_else(|| {
            crate::features::route_text(clause)
                .or_else(|| crate::integration_router::route_text(clause))
                .filter(|route| route_allowed(clause, route.tool))
                .map(|route| Resolved::Call(route.tool, route.args))
        })
        .or_else(|| of(intents::parse(clause)))
        .or_else(|| intents::ui_layout_command(clause).map(call("control_jarvis_interface")))
}

/// A schedule route only for words that mean the schedule (never a playlist called "classes").
pub fn route_allowed(text: &str, tool: &str) -> bool {
    match tool {
        "check_school_schedule" => crate::integration_router::explicit_school_request(text),
        "get_calendar_schedule" => !crate::integration_router::has_music_language(text),
        _ => true,
    }
}

enum Resolved {
    Mode(bool),
    Privacy(LockCommand),
    Call(&'static str, Value),
}

/// What one spoken command asks for, as the stateless parsers see it: the tool and its
/// arguments, with the dispatcher's switches as "ultron_mode" `{"on": …}` and "listening_lock"
/// `{"command": …}` (the names a dry run records them under). As in a turn, Jarvis' own words
/// are spelt right first ("close your car" is the core) and a sentence that matches nothing
/// but sounds like a command is taken for it. None: nothing deterministic — the brain's
/// dialogue state or the language model decides.
pub fn resolve_one(text: &str, ctx: &ReflexContext) -> Option<(String, Value)> {
    let said = crate::typos::normalize_user_input_typos(text.trim());
    let said = said.trim().trim_end_matches(['.', '!', '?']);
    let resolved = resolve(said, ctx).or_else(|| crate::intent_engine::phonetic_rescue(said, ctx).and_then(|meant| resolve(meant, ctx)))?;
    Some(match resolved {
        Resolved::Mode(on) => ("ultron_mode".to_string(), json!({ "on": on })),
        Resolved::Privacy(cmd) => ("listening_lock".to_string(), json!({ "command": format!("{cmd:?}") })),
        Resolved::Call(tool, args) => (tool.to_string(), args),
    })
}

/// The plan for a sentence of two to six commands, or None when it is one command, too long a
/// list, or any clause is not a command on its own.
pub fn plan(text: &str, ctx: &ReflexContext) -> Option<Plan> {
    // One command (the usual case) costs a substring scan and nothing more.
    if clauses(text).len() < 2 {
        return None;
    }
    // Jarvis' own words spelt right first, as a single command gets them ("close your car").
    let text = crate::typos::normalize_user_input_typos(text.trim());
    let parts = clauses(&text);
    if !(2..=MAX_CLAUSES).contains(&parts.len()) {
        return None;
    }
    let mut plan = Plan::default();
    let mut stage = 0;
    for (clause, begins_stage) in parts {
        if begins_stage && !plan.calls.is_empty() {
            stage += 1;
        }
        match resolve(&clause, ctx)? {
            // The same switch twice in one sentence is one switch; opposite ones are a slip.
            Resolved::Mode(on) if plan.mode.is_none_or(|known| known == on) => plan.mode = Some(on),
            Resolved::Privacy(cmd) if plan.privacy.is_none_or(|known| known == cmd) => plan.privacy = Some(cmd),
            Resolved::Mode(_) | Resolved::Privacy(_) => return None,
            Resolved::Call(tool, _) if needs_confirmation(tool) => return None,
            Resolved::Call(tool, args) => plan.calls.push(Call { clause, tool, args, stage }),
        }
    }
    (plan.count() >= 2).then_some(plan)
}

/// The order a plan's calls run in, as indices into `calls`: stages one after another; within a
/// stage, lanes side by side; within a lane (calls on the same thing), the order they were said.
pub fn lanes(calls: &[Call]) -> Vec<Vec<Vec<usize>>> {
    let mut out: Vec<Vec<Vec<usize>>> = Vec::new();
    let stages = calls.iter().map(|c| c.stage).max().map_or(0, |last| last + 1);
    for stage in 0..stages {
        let members: Vec<usize> = (0..calls.len()).filter(|i| calls[*i].stage == stage).collect();
        let steps: Vec<crate::protocols::Step> = members.iter().map(|i| crate::protocols::Step { tool: calls[*i].tool.to_string(), args: calls[*i].args.clone() }).collect();
        for planned in crate::protocols::plan(&steps) {
            out.push(planned.into_iter().map(|lane| lane.into_iter().map(|step| members[step]).collect()).collect());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle() -> ReflexContext {
        ReflexContext::default()
    }

    fn ultron() -> ReflexContext {
        ReflexContext { ultron: true, ..ReflexContext::default() }
    }

    #[test]
    fn a_sentence_is_cut_at_commas_and_conjunctions_and_then_starts_a_new_stage() {
        let parts = clauses("Switch back to Jarvis, set theme to Arc Cyan, move the orb to the top right, and open my WebUntis schedule.");
        let words: Vec<&str> = parts.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(words, ["Switch back to Jarvis", "set theme to Arc Cyan", "move the orb to the top right", "open my WebUntis schedule"]);
        assert!(parts.iter().all(|(_, staged)| !staged));
        assert_eq!(clauses("Open the feature lab and then close it"), vec![("Open the feature lab".to_string(), false), ("close it".to_string(), true)]);
        assert_eq!(clauses("Mach das Theme rot und dann zeig mir den Stundenplan"), vec![("Mach das Theme rot".to_string(), false), ("zeig mir den Stundenplan".to_string(), true)]);
        // The wake word and a courtesy are nobody's clause; one command stays one command.
        assert_eq!(clauses("Jarvis, pause the music, please."), vec![("pause the music".to_string(), false)]);
        assert_eq!(clauses("What time is it?"), vec![("What time is it?".to_string(), false)]);
        assert_eq!(clauses("Jarvis, and then, please"), Vec::new());
    }

    #[test]
    fn the_four_command_sentence_resolves_every_clause() {
        let plan = plan("Switch back to Jarvis, set theme to Arc Cyan, move the orb to the top right, and open my WebUntis schedule.", &ultron()).expect("a compound");
        assert_eq!(plan.mode, Some(false), "the mode switch is the dispatcher's");
        assert_eq!(plan.privacy, None);
        let tools: Vec<&str> = plan.calls.iter().map(|c| c.tool).collect();
        assert_eq!(plan.calls.len(), 3, "{tools:?}");
        assert!(!tools.iter().any(|t| t.contains("music")), "never the music player: {tools:?}");
        assert_eq!(plan.calls[0].tool, "configure_interface");
        assert_eq!(plan.calls[0].args["hud_palette"], "arc_cyan", "{}", plan.calls[0].args);
        assert!(plan.calls[1].args.to_string().contains("top"), "the orb goes to the top right: {} {}", plan.calls[1].tool, plan.calls[1].args);
        assert!(plan.calls[2].args.to_string().to_lowercase().contains("schedule") || plan.calls[2].tool.contains("schedule"), "{} {}", plan.calls[2].tool, plan.calls[2].args);
        assert_eq!(plan.rest(), "set theme to Arc Cyan, move the orb to the top right and open my WebUntis schedule");
        // The look, the orb and the schedule view all draw in the HUD: one stage, one lane, in
        // the order they were said.
        assert_eq!(lanes(&plan.calls), vec![vec![vec![0, 1, 2]]]);
        // What touches different things runs side by side: the player beside the HUD.
        let mixed = super::plan("Pause the music, set theme to Arc Cyan and open my system stats", &idle()).expect("a compound");
        let order = lanes(&mixed.calls);
        assert_eq!(order.len(), 1, "one stage: {order:?}");
        assert_eq!(order[0].len(), 2, "two lanes side by side: {order:?}");
        assert!(order[0].contains(&vec![0]) && order[0].contains(&vec![1, 2]), "{order:?}");
    }

    #[test]
    fn german_and_mixed_sentences_plan_the_same_way() {
        let plan = plan("Wechsel zurück zu Jarvis und mach das Theme blau", &ultron()).expect("a compound");
        assert_eq!(plan.mode, Some(false));
        assert_eq!(plan.calls.len(), 1);
        assert_eq!(plan.calls[0].tool, "configure_interface", "{}", plan.calls[0].args);
        let plan = super::plan("Pause the music and close the feature lab", &idle()).expect("a compound");
        assert_eq!(plan.calls.len(), 2);
        assert_eq!((plan.calls[0].tool, plan.calls[1].tool), ("control_music", "hud_board"), "{:?}", plan.calls);
    }

    #[test]
    fn then_runs_in_order_and_a_stranger_clause_means_no_compound() {
        let plan = plan("Open the feature lab, then close the feature lab", &idle()).expect("a compound");
        assert_eq!(plan.calls.iter().map(|c| c.stage).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(lanes(&plan.calls).len(), 2, "two stages, one after the other");
        assert_eq!(plan.rest(), "Open the feature lab, then close the feature lab");
        // One command, a list inside one command, small talk: never a compound.
        for text in ["Play rock and roll", "Remind me to buy milk, eggs and bread", "Switch back to Jarvis and tell me a joke", "What is the weather like today?"] {
            assert_eq!(super::plan(text, &idle()), None, "{text}");
        }
        // Two levels for the mixer are two mixer calls on one lane, in the order they were said.
        let mixer = super::plan("Set Sonora to 30 % and Discord to 100 %", &idle()).expect("two mixer calls");
        assert!(mixer.calls.iter().all(|c| c.tool == "control_audio_mixer"), "{:?}", mixer.calls);
        assert_eq!(lanes(&mixer.calls), vec![vec![vec![0, 1]]]);
        // Nothing that needs a yes runs from a list, and a seventh clause is dictation.
        assert_eq!(super::plan("Pause the music and quit Jarvis", &idle()), None);
        assert_eq!(super::plan("mute, pause, mute, pause, mute, pause, mute", &idle()), None);
        // Opposite switches in one breath are a slip of the tongue, not a plan.
        assert_eq!(super::plan("Switch to Ultron and switch back to Jarvis", &idle()), None);
    }

    #[test]
    fn mode_and_privacy_together_are_both_the_dispatchers() {
        let plan = plan("Switch back to Jarvis and stop listening", &ultron()).expect("a compound");
        assert_eq!((plan.mode, plan.privacy.is_some(), plan.calls.len()), (Some(false), true, 0));
        assert!(plan.rest().is_empty());
        assert_eq!(plan.count(), 2);
    }

    #[test]
    fn planning_a_sentence_stays_far_inside_the_reflex_budget() {
        let ctx = ultron();
        let text = "Switch back to Jarvis, set theme to Arc Cyan, move the orb to the top right, and open my WebUntis schedule.";
        let _ = plan(text, &ctx);
        let started = std::time::Instant::now();
        for _ in 0..50 {
            assert!(plan(text, &ctx).is_some());
        }
        let per_plan = started.elapsed().as_secs_f64() * 1000.0 / 50.0;
        assert!(per_plan < 15.0, "planning four commands took {per_plan:.2} ms");
        // An ordinary sentence costs a substring scan.
        let started = std::time::Instant::now();
        for _ in 0..2000 {
            assert_eq!(plan("What time is it?", &ctx), None);
        }
        assert!(started.elapsed().as_secs_f64() * 1000.0 / 2000.0 < 0.05);
    }
}
