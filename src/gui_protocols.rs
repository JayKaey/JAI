//! The protocol window ("open the protocol window", the admin window's tile): everything that
//! starts by itself, in one place (Sir, 2026-11-21: "the protocol menu should be way more clearly
//! laid out … all existing protocols should get shown there too"). On the left his protocols and
//! the built-in ones, the rules he said aloud ("whenever I clap twice, …") and J.A.I.'s own
//! reactions to two claps and a knock. A protocol reads as 01 STARTS WHEN (a phrase he says, a
//! sound, a program, a time, a friend's PC: any one of them) → 02 THEN I DO (its steps in order,
//! "tell me" among them). The strip on top says how that works and what he heard last.
//! `protocols` keeps and runs them, `kit_notes` watches the starts, `sound_sense` names sounds.

use super::{strip_ink, strip_muted};
use crate::config::AcousticSettings;
use crate::holo::{self, Face};
use crate::holo_ui;
use crate::kit_notes::{self, When};
use crate::protocols::{self, Protocol, ProtocolStore, Step};
use crate::sound_sense::Sound;
use eframe::egui::{self, vec2, Align, Color32, Frame, Label, Layout, Margin, RichText, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

/// The window's id among his views.
const ID: &str = "protocols";
const TITLE: &str = "Protocols // when it happens, I do";
static OPEN: AtomicBool = AtomicBool::new(false);
/// The list's width.
const LIST_W: f32 = 230.0;

/// What is chosen on the left.
#[derive(Clone, Default, PartialEq, Debug)]
enum Chosen {
    /// A new protocol on the form.
    #[default]
    New,
    Protocol(String),
    /// A rule he said aloud, by its key.
    Rule(String),
    /// J.A.I.'s own reactions.
    Claps,
    Knock,
}

/// The kind of start being added.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum Adding {
    #[default]
    Nothing,
    Say,
    Sound,
    Program,
    Time,
    Pc,
}

/// The protocol on the form, as it is put together.
#[derive(Default)]
struct Form {
    chosen: Chosen,
    name: String,
    /// What starts it: phrases he says, and the rest in the rules' words (`kit_notes::when_of`).
    say: Vec<String>,
    when: Vec<String>,
    steps: Vec<Step>,
    /// The rule this protocol is made from: dropped once it is saved.
    from_rule: Option<String>,
    adding: Adding,
    /// What is typed for the start being added, and for a step.
    typed: String,
    step: String,
    /// The step being changed (EDIT): what is typed takes its place.
    editing: Option<usize>,
    /// What the last key came to, and whether that is a fault.
    said: (String, bool),
}

static FORM: LazyLock<Mutex<Form>> = LazyLock::new(Mutex::default);

fn form() -> MutexGuard<'static, Form> {
    FORM.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the window works on: the app's own in a session, stand-ins in a test.
struct Desk<'a> {
    store: &'a ProtocolStore,
    /// Engages a protocol by its name.
    run: &'a dyn Fn(String),
    /// J.A.I.'s own sound reactions: read (None) or changed (Some).
    acoustic: &'a dyn Fn(Option<AcousticSettings>) -> AcousticSettings,
}

/// The protocol he asked to see, put on the form as the window next draws.
static WANTED: Mutex<Option<String>> = Mutex::new(None);

pub(super) fn open(at: Option<&str>) {
    if let Some(name) = at {
        *WANTED.lock().unwrap_or_else(|e| e.into_inner()) = Some(name.to_string());
    }
    OPEN.store(true, Ordering::Release);
}

pub(super) fn close() {
    OPEN.store(false, Ordering::Release);
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

impl Form {
    /// The form for `p` (None: an empty one for a new protocol).
    fn load(&mut self, p: Option<&Protocol>) {
        *self = p.map_or_else(Form::default, |p| Form {
            chosen: Chosen::Protocol(p.name.clone()),
            name: p.name.clone(),
            say: p.trigger_phrases.clone(),
            when: p.when.clone(),
            steps: p.steps.clone(),
            ..Form::default()
        });
    }

    /// Something else is chosen on the left: what was on the form is let go.
    fn choose(&mut self, chosen: Chosen) {
        *self = Form { chosen, ..Form::default() };
    }

    /// A start of the kind being added: kept, or why not.
    fn add_start(&mut self, line: &str) -> Result<(), String> {
        let line = line.trim().to_string();
        if line.is_empty() {
            return Err("Type it first.".into());
        }
        if self.adding == Adding::Say {
            if !self.say.contains(&line) {
                self.say.push(line);
            }
        } else {
            if kit_notes::when_of(&line).is_none() {
                return Err(format!("\"{line}\" is no start I know."));
            }
            if !self.when.contains(&line) {
                self.when.push(line);
            }
        }
        self.typed.clear();
        Ok(())
    }

    /// A step after the others, in his words, or in place of the one being changed (left as it
    /// was when its words are).
    fn add_step(&mut self, words: &str) {
        match (self.editing.take(), Step::of_line(words)) {
            (Some(i), Some(step)) if i < self.steps.len() => {
                if self.steps[i].words() != words.trim() {
                    self.steps[i] = step;
                }
            }
            (_, step) => self.steps.extend(step),
        }
        self.step.clear();
    }
}

/// Keeps the form as a protocol (one named anew replaces the one it was; a rule it was made of
/// is dropped).
fn save(f: &mut Form, store: &ProtocolStore) -> Result<Protocol, String> {
    // (A step typed and not yet added counts.)
    let typed = std::mem::take(&mut f.step);
    f.add_step(&typed);
    if let Some(bad) = f.when.iter().find(|line| kit_notes::when_of(line).is_none()) {
        return Err(format!("\"{bad}\" is no start I know."));
    }
    let chosen = match &f.chosen {
        Chosen::Protocol(name) => name.clone(),
        _ => String::new(),
    };
    if let Some(other) = store.get(&f.name).filter(|p| p.name != chosen) {
        return Err(format!("There is a {} already: choose it on the left to change it.", protocols::display_name(&other.name)));
    }
    let description = store.get(&chosen).map(|p| p.description).unwrap_or_default();
    let saved = store.save(Protocol { name: f.name.clone(), description, trigger_phrases: f.say.clone(), steps: f.steps.clone(), when: f.when.clone() })?;
    if !chosen.is_empty() && chosen != saved.name {
        store.delete(&chosen);
    }
    if let Some(key) = f.from_rule.take() {
        kit_notes::drop_rule(&key);
    }
    f.chosen = Chosen::Protocol(saved.name.clone());
    f.name = saved.name.clone();
    Ok(saved)
}

fn text(ui: &mut Ui, words: impl Into<String>, face: Face, size: f32, colour: Color32) {
    ui.label(RichText::new(words.into()).font(holo::font(face, size)).color(colour));
}

/// Words that are cut to the room left, from the left.
fn cut(ui: &mut Ui, words: &str, face: Face, size: f32, colour: Color32) {
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 20.0), Layout::left_to_right(Align::Center), |ui| {
        ui.add(Label::new(RichText::new(words).font(holo::font(face, size)).color(colour)).truncate());
    });
}

/// How it works, in three lines.
fn how_it_works(ui: &mut Ui) {
    let t = holo::theme();
    for (n, head, words) in [
        ("01", "STARTS WHEN", "something sets it off: a phrase you say, a sound I hear, a program, a time of day or a friend's PC. Any one is enough."),
        ("02", "THEN I DO", "its steps, top to bottom, as if you had said them. A step \"tell me\" has me say what happened."),
        ("03", "BY VOICE", "\"whenever I clap twice, open Spotify\" or \"when you hear knocking, tell me\" makes a rule. Rules are listed here too."),
    ] {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            text(ui, n, Face::Figure, 13.0, t.accent());
            text(ui, head, Face::Label, 8.5, strip_ink());
            text(ui, words, Face::Body, 12.5, strip_muted());
        });
    }
}

/// What he heard last and what he took it for (the radar listens for every sound while this
/// shows).
fn hearing(ui: &mut Ui) {
    let t = holo::theme();
    let heard = crate::sound_sense::recent();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        text(ui, "I HEAR", Face::Label, 8.5, t.accent());
        if heard.is_empty() {
            text(ui, "nothing yet. Clap twice, knock or snap your fingers to see what I make of it.", Face::Body, 12.5, strip_muted());
        }
        for h in heard.iter().take(2) {
            text(ui, &h.at, Face::Mono, 11.0, t.faint());
            text(ui, &h.what, Face::Body, 12.5, if h.sound { strip_ink() } else { strip_muted() });
            if !h.detail.is_empty() {
                text(ui, format!("({})", h.detail), Face::Body, 11.5, t.faint());
            }
        }
        if !crate::sound_sense::available() {
            text(ui, "· without the sound model I know claps and knocks by their shape alone", Face::Body, 11.5, t.faint());
        }
    });
}

/// How a protocol starts, in a line of the list.
fn starts(p: &Protocol) -> String {
    let mut parts: Vec<String> = p.trigger_phrases.iter().take(1).map(|s| format!("\"{s}\"")).collect();
    parts.extend(p.when.iter().take(2).cloned());
    if parts.is_empty() {
        parts.push("by its name".into());
    }
    if protocols::built_in(&p.name) {
        parts.insert(0, "built in".into());
    }
    parts.join(" · ")
}

/// What of his own waits on a sound (a rule's order, a protocol's name): it takes the place of
/// J.A.I.'s own reaction to it.
fn his_own(sound: Sound, rules: &[(String, When, String)], all: &[Protocol]) -> Option<String> {
    let when = When::Hears(sound);
    rules
        .iter()
        .find(|(_, w, _)| *w == when)
        .map(|(_, _, then)| then.clone())
        .or_else(|| all.iter().find(|p| p.when.iter().any(|line| kit_notes::when_of(line).as_ref() == Some(&when))).map(|p| protocols::display_name(&p.name)))
}

/// A rule's start in a line: "I clap twice", "Minecraft starts", "Every day at 07:30".
fn start_line(when: &When) -> String {
    kit_notes::capitalised(when.words().trim_start_matches("whenever "))
}

fn list(ui: &mut Ui, f: &mut Form, desk: &Desk, rules: &[(String, When, String)], all: &[Protocol]) {
    let t = holo::theme();
    if holo_ui::key(ui, "NEW PROTOCOL").clicked() {
        f.load(None);
    }
    ui.add_space(4.0);
    let entry = |ui: &mut Ui, name: &str, under: &str, on: bool| -> bool {
        let clicked = holo_ui::item(ui, name, on).clicked();
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            cut(ui, under, Face::Body, 11.5, t.faint());
        });
        clicked
    };
    crate::holo_block::faded(ui, egui::ScrollArea::vertical().id_salt("protocol_list").auto_shrink([false; 2]), |ui| {
        holo_ui::head(ui, "PROTOCOLS");
        for p in all {
            if entry(ui, &protocols::display_name(&p.name), &starts(p), f.chosen == Chosen::Protocol(p.name.clone())) {
                f.load(Some(p));
            }
        }
        ui.add_space(6.0);
        holo_ui::head(ui, "SAID ALOUD");
        if rules.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.add(Label::new(RichText::new("None yet. Say \"whenever I clap twice, open Spotify\".").font(holo::font(Face::Body, 11.5)).color(t.faint())).wrap());
            });
        }
        for (key, when, then) in rules {
            if entry(ui, &start_line(when), &format!("→ {then}"), f.chosen == Chosen::Rule(key.clone())) {
                f.choose(Chosen::Rule(key.clone()));
            }
        }
        ui.add_space(6.0);
        holo_ui::head(ui, "MY OWN");
        let a = (desk.acoustic)(None);
        let claps = match his_own(Sound::Claps(2), rules, all) {
            Some(yours) => format!("yours instead: {yours}"),
            None if !a.double_clap => "off".into(),
            None if a.clap_action == "toggle_music" => "→ music on or off".into(),
            None => "→ I listen".into(),
        };
        if entry(ui, "Two claps", &claps, f.chosen == Chosen::Claps) {
            f.choose(Chosen::Claps);
        }
        let knock = match his_own(Sound::Knock, rules, all) {
            Some(yours) => format!("yours instead: {yours}"),
            None if !a.knock_alert => "off".into(),
            None if a.knock_only_with_music => "→ I tell you, while music plays".into(),
            None => "→ I tell you".into(),
        };
        if entry(ui, "Knocking", &knock, f.chosen == Chosen::Knock) {
            f.choose(Chosen::Knock);
        }
    });
}

/// A start or a step on the form: its tag, its words, and X to take it away (true).
fn row(ui: &mut Ui, tag: &str, words: &str) -> bool {
    let t = holo::theme();
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 22.0], Label::new(RichText::new(tag).font(holo::font(Face::Label, 8.0)).color(t.accent())));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let gone = holo_ui::key(ui, "X").clicked();
            cut(ui, words, Face::Body, 13.0, strip_ink());
            gone
        })
        .inner
    })
    .inner
}

/// The kind of a start, for its tag.
fn kind(line: &str) -> &'static str {
    match kit_notes::when_of(line) {
        Some(When::Hears(_)) => "SOUND",
        Some(When::Starts(_) | When::Closes(_)) => "PROGRAM",
        Some(When::At(..)) => "TIME",
        Some(When::Pc(..)) => "PC",
        None => "?",
    }
}

fn starts_when(ui: &mut Ui, f: &mut Form) {
    let t = holo::theme();
    let mut gone = None;
    for (i, phrase) in f.say.iter().enumerate() {
        if row(ui, "YOU SAY", &format!("\"{phrase}\"")) {
            gone = Some((true, i));
        }
    }
    for (i, line) in f.when.iter().enumerate() {
        if row(ui, kind(line), line) {
            gone = Some((false, i));
        }
    }
    match gone {
        Some((true, i)) => drop(f.say.remove(i)),
        Some((false, i)) => drop(f.when.remove(i)),
        None => {}
    }
    if !f.name.trim().is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(76.0);
            text(ui, format!("and always: \"engage the {} protocol\"", f.name.trim().trim_end_matches(" protocol")), Face::Body, 11.5, t.faint());
        });
    }
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        text(ui, "ADD", Face::Label, 8.0, strip_muted());
        for (adding, label) in [(Adding::Say, "A PHRASE"), (Adding::Sound, "A SOUND"), (Adding::Program, "A PROGRAM"), (Adding::Time, "A TIME"), (Adding::Pc, "A FRIEND'S PC")] {
            if holo_ui::key_on(ui, label, f.adding == adding).clicked() {
                f.adding = if f.adding == adding { Adding::Nothing } else { adding };
                f.typed.clear();
            }
        }
    });
    /// The line to type the start into: whether Enter was pressed in it.
    fn slot(ui: &mut Ui, typed: &mut String, hint: &str) -> bool {
        let r = holo_ui::slot(ui, egui::TextEdit::singleline(typed).hint_text(hint).desired_width(180.0));
        r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
    }
    let mut line: Option<String> = None;
    match f.adding {
        Adding::Nothing => {}
        Adding::Say => {
            ui.horizontal(|ui| {
                let entered = slot(ui, &mut f.typed, "wake up, daddy's home");
                if holo_ui::key(ui, "ADD").clicked() || entered {
                    line = Some(f.typed.clone());
                }
            });
        }
        Adding::Sound => {
            ui.horizontal_wrapped(|ui| {
                for sound in Sound::ALL {
                    if holo_ui::key(ui, sound.words().to_uppercase()).clicked() {
                        line = Some(kit_notes::capitalised(sound.words()));
                    }
                }
            });
        }
        Adding::Program => {
            ui.horizontal(|ui| {
                slot(ui, &mut f.typed, "Minecraft");
                for (key, verb) in [("STARTS", "starts"), ("CLOSES", "closes")] {
                    if holo_ui::key(ui, key).clicked() {
                        line = Some(format!("{} {verb}", f.typed.trim()));
                    }
                }
            });
        }
        Adding::Time => {
            ui.horizontal(|ui| {
                let entered = slot(ui, &mut f.typed, "7:30");
                if holo_ui::key(ui, "EVERY DAY AT THIS TIME").clicked() || entered {
                    line = Some(format!("every day at {}", f.typed.trim()));
                }
            });
        }
        Adding::Pc => {
            ui.horizontal_wrapped(|ui| {
                slot(ui, &mut f.typed, "Max");
                for (key, verb) in [("COMES ONLINE", "comes online"), ("GOES OFFLINE", "goes offline"), ("STARTS A GAME", "starts a game"), ("STOPS PLAYING", "stops playing")] {
                    if holo_ui::key(ui, key).clicked() {
                        line = Some(format!("{} {verb}", f.typed.trim()));
                    }
                }
            });
        }
    }
    if let Some(line) = line {
        let unnamed = matches!(f.adding, Adding::Program | Adding::Time | Adding::Pc) && f.typed.trim().is_empty();
        f.said = match if unnamed { Err("Type it in the line first.".to_string()) } else { f.add_start(&line) } {
            Ok(()) => (String::new(), false),
            Err(e) => (e, true),
        };
    }
}

fn then_i_do(ui: &mut Ui, f: &mut Form) {
    let t = holo::theme();
    let last = f.steps.len();
    let mut moved: Option<(usize, i32)> = None;
    let mut edit: Option<usize> = None;
    for (i, step) in f.steps.iter().enumerate() {
        let words = if kit_notes::is_tell_me(&step.words()) { "Tell me what happened".to_string() } else { step.words() };
        ui.horizontal(|ui| {
            ui.add_sized([26.0, 22.0], Label::new(RichText::new(format!("{:02}", i + 1)).font(holo::font(Face::Figure, 13.0)).color(t.accent())));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if holo_ui::key(ui, "X").clicked() {
                    moved = Some((i, 0));
                }
                if holo_ui::key_on(ui, "EDIT", f.editing == Some(i)).clicked() {
                    edit = Some(i);
                }
                // (A key a step has no use for leaves its room, so the keys stand in columns.)
                for (shown, label, by) in [(i + 1 < last, "DOWN", 1), (i > 0, "UP", -1)] {
                    if !shown {
                        ui.add_space(crate::holo_kit::key_width(ui.painter(), label) + ui.spacing().item_spacing.x);
                    } else if holo_ui::key(ui, label).clicked() {
                        moved = Some((i, by));
                    }
                }
                cut(ui, &words, Face::Body, 13.0, strip_ink());
            });
        });
    }
    // (Its words go into the line below, to be changed there; a step moved or taken out ends it.)
    if let Some(i) = edit {
        f.step = f.steps[i].words();
        f.editing = Some(i);
    }
    match moved {
        Some((i, 0)) => drop(f.steps.remove(i)),
        Some((i, by)) => f.steps.swap(i, (i as i32 + by) as usize),
        None => {}
    }
    if moved.is_some() {
        f.editing = None;
    }
    if f.steps.is_empty() {
        text(ui, "No steps yet: add what I should do, one at a time.", Face::Body, 12.0, t.faint());
    }
    ui.horizontal(|ui| {
        let typed = holo_ui::slot(ui, egui::TextEdit::singleline(&mut f.step).hint_text("open Spotify").desired_width(240.0));
        let entered = typed.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (holo_ui::key(ui, if f.editing.is_some() { "CHANGE" } else { "ADD STEP" }).clicked() || entered) && !f.step.trim().is_empty() {
            let typed = std::mem::take(&mut f.step);
            f.add_step(&typed);
        }
        if holo_ui::key(ui, "TELL ME").clicked() {
            f.add_step("tell me");
        }
    });
    ui.add(Label::new(RichText::new("A step is anything you would say to me: \"open Spotify\", \"play my rock playlist\", \"set the volume to 30\", \"tell Max's PC to open Minecraft\".").font(holo::font(Face::Body, 11.5)).color(t.faint())).wrap());
}

fn editor(ui: &mut Ui, f: &mut Form, desk: &Desk) {
    let t = holo::theme();
    holo_ui::head(ui, "NAME");
    ui.horizontal(|ui| {
        holo_ui::slot(ui, egui::TextEdit::singleline(&mut f.name).hint_text("door watch").desired_width(240.0));
        if matches!(&f.chosen, Chosen::Protocol(name) if protocols::built_in(name)) {
            text(ui, "BUILT IN · yours to change", Face::Label, 8.0, t.faint());
        }
    });
    ui.add_space(8.0);
    holo_ui::head(ui, "01  STARTS WHEN  ·  ANY ONE OF THESE");
    starts_when(ui, f);
    ui.add_space(8.0);
    holo_ui::head(ui, "02  THEN I DO  ·  IN THIS ORDER");
    then_i_do(ui, f);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if holo_ui::key(ui, "SAVE").clicked() {
            f.said = match save(f, desk.store) {
                Ok(p) => (format!("Saved. Say \"engage the {} protocol\"{}.", p.name, p.trigger_phrases.first().map(|t| format!(" or \"{t}\"")).unwrap_or_default()), false),
                Err(e) => (e, true),
            };
        }
        if holo_ui::key(ui, "RUN NOW").clicked() {
            f.said = match save(f, desk.store) {
                Ok(p) => {
                    (desk.run)(p.name.clone());
                    (format!("The {} is engaged.", protocols::display_name(&p.name)), false)
                }
                Err(e) => (e, true),
            };
        }
        if let Chosen::Protocol(name) = f.chosen.clone() {
            if holo_ui::key_alert(ui, "DELETE").clicked() {
                desk.store.delete(&name);
                f.load(None);
                f.said = (format!("The {} is gone.", protocols::display_name(&name)), false);
            }
        }
    });
}

/// A rule he said aloud: when, then, and making it a protocol (more steps, other starts).
fn rule_view(ui: &mut Ui, f: &mut Form, rules: &[(String, When, String)], key: &str) {
    let Some((_, when, then)) = rules.iter().find(|(k, ..)| k == key) else {
        f.choose(Chosen::New);
        return;
    };
    holo_ui::head(ui, "A RULE YOU SAID ALOUD");
    let what = if kit_notes::is_tell_me(then) { format!("Tell you: \"{}\"", when.news()) } else { then.clone() };
    for (tag, words) in [("WHEN", start_line(when)), ("THEN I", what)] {
        ui.horizontal(|ui| {
            ui.add_sized([70.0, 22.0], Label::new(RichText::new(tag).font(holo::font(Face::Label, 8.0)).color(holo::theme().accent())));
            cut(ui, &words, Face::Body, 13.0, strip_ink());
        });
    }
    ui.add_space(4.0);
    ui.add(Label::new(RichText::new("Made by voice: \"whenever …, …\". As a protocol it can do more than one thing and start in more ways.").font(holo::font(Face::Body, 12.0)).color(strip_muted())).wrap());
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if holo_ui::key(ui, "MAKE IT A PROTOCOL").clicked() {
            let (line, steps) = (start_line(when), Step::of_line(then).into_iter().collect());
            f.load(None);
            f.when = vec![line];
            f.steps = steps;
            f.from_rule = Some(key.to_string());
            f.said = ("Give it a name and SAVE: the rule becomes this protocol.".into(), false);
        }
        if holo_ui::key_alert(ui, "DELETE").clicked() {
            kit_notes::drop_rule(key);
            f.choose(Chosen::New);
            f.said = ("The rule is gone.".into(), false);
        }
    });
}

/// J.A.I.'s own reaction to two claps or a knock: on or off, what it does, how keen his ears are.
fn reaction(ui: &mut Ui, f: &Form, desk: &Desk, rules: &[(String, When, String)], all: &[Protocol]) {
    let mut a = (desk.acoustic)(None);
    let before = a.clone();
    let sound = if f.chosen == Chosen::Claps { Sound::Claps(2) } else { Sound::Knock };
    if sound == Sound::Claps(2) {
        holo_ui::head(ui, "TWO CLAPS  ·  MY OWN");
        holo_ui::toggle(ui, &mut a.double_clap, "When you clap twice, with quiet before and after, I react");
        ui.horizontal(|ui| {
            text(ui, "THEN I", Face::Label, 8.0, strip_muted());
            if holo_ui::key_on(ui, "LISTEN", a.clap_action != "toggle_music").clicked() {
                a.clap_action = "wake".into();
            }
            if holo_ui::key_on(ui, "PLAY OR PAUSE THE MUSIC", a.clap_action == "toggle_music").clicked() {
                a.clap_action = "toggle_music".into();
            }
        });
    } else {
        holo_ui::head(ui, "KNOCKING  ·  MY OWN");
        holo_ui::toggle(ui, &mut a.knock_alert, "When someone knocks, I lower the music and tell you");
        holo_ui::toggle(ui, &mut a.knock_only_with_music, "Only while music plays (you might not hear it then)");
    }
    ui.add_space(6.0);
    ui.add(holo_ui::Slider::new(&mut a.sensitivity, 0.5..=2.0).text("how keen my ears are"));
    ui.add_space(6.0);
    let yours = match his_own(sound, rules, all) {
        Some(yours) => format!("Yours takes its place now: {yours}."),
        None => format!("A rule or a protocol of yours for \"{}\" takes its place.", sound.words()),
    };
    let model = if crate::sound_sense::available() { " I tell typing, a tap on the desk and music apart from it." } else { "" };
    ui.add(Label::new(RichText::new(format!("{yours}{model}")).font(holo::font(Face::Body, 12.0)).color(strip_muted())).wrap());
    if a != before {
        (desk.acoustic)(Some(a));
    }
}

/// The window's inside: how it works and what he hears on top, everything that starts by itself
/// on the left, the one chosen on the right.
fn body(ui: &mut Ui, desk: &Desk) {
    crate::sound_sense::watch();
    let mut f = form();
    // ("Edit the gaming protocol"; one he has not got yet comes up new, by that name.)
    if let Some(name) = WANTED.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let found = desk.store.get(&name);
        f.load(found.as_ref());
        if found.is_none() {
            f.name = name;
        }
    }
    let (rules, all) = (kit_notes::rules(), desk.store.list());
    how_it_works(ui);
    ui.add_space(2.0);
    hearing(ui);
    holo_ui::rule(ui);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LIST_W);
            list(ui, &mut f, desk, &rules, &all);
        });
        ui.add_space(14.0);
        // (Its own scroll, top to bottom like the list: a long THEN I DO runs past the window's foot.)
        ui.vertical(|ui| {
            crate::holo_block::faded(ui, egui::ScrollArea::vertical().id_salt("protocol_side").auto_shrink([false; 2]), |ui| {
                match f.chosen.clone() {
                    Chosen::Rule(key) => rule_view(ui, &mut f, &rules, &key),
                    Chosen::Claps | Chosen::Knock => reaction(ui, &f, desk, &rules, &all),
                    Chosen::New | Chosen::Protocol(_) => editor(ui, &mut f, desk),
                }
                if !f.said.0.is_empty() {
                    ui.add_space(4.0);
                    ui.add(Label::new(RichText::new(&f.said.0).font(holo::font(Face::Body, 13.0)).color(if f.said.1 { holo::theme().alert() } else { strip_ink() })).wrap());
                }
            });
        });
    });
}

/// The protocol window while it is open (a window of its own; a block on the board in a session
/// without windows).
pub(super) fn show(ctx: &egui::Context, app: &crate::app::Jai, rt: &tokio::runtime::Handle) {
    let viewport = ViewportId::from_hash_of("jai_protocols");
    // (Closed while it was its own window: it plays its exit first.)
    if !super::detached_keep(ctx, viewport, is_open()) {
        crate::hud_dock::forget(ID);
        return;
    }
    let store = app.tools().protocols.clone();
    let (tools, rt) = (app.tools().clone(), rt.clone());
    let run = move |name: String| {
        let tools = tools.clone();
        rt.spawn(async move {
            // (No turn of his says its line: the tool does.)
            let _ = tools.execute("manage_stark_protocol", &serde_json::json!({ "action": "execute_protocol", "protocol_name": name, "announce": true })).await;
        });
    };
    let app = app.clone();
    let acoustic = move |set: Option<AcousticSettings>| {
        if let Some(a) = set {
            app.config().update(|c| c.acoustic = a.clone());
            app.set_acoustic(&a);
        }
        app.config().read(|c| c.acoustic.clone())
    };
    if !crate::hud_dock::adopt(ID) {
        if super::hud_block_window(ctx, ID, TITLE, [860.0, 640.0], |ui| body(ui, &Desk { store: &store, run: &run, acoustic: &acoustic })).close {
            close();
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("Protocols — J.A.I.").with_app_id("jai").with_inner_size([900.0, 680.0]).with_min_inner_size([720.0, 520.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    let builder = super::for_test_session(builder);
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard("protocols", || {
            if ctx.input(|i| i.viewport().close_requested()) || (!is_open() && !super::detached_leaving(ctx)) {
                close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            // (What he hears is shown as it comes.)
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
            let desk = Desk { store: &store, run: &run, acoustic: &acoustic };
            if class == ViewportClass::Embedded {
                egui::Window::new("Protocols").show(ctx, |ui| body(ui, &desk));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, ID, TITLE) {
                        close();
                    }
                    body(ui, &desk);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage};

    fn desk_with<'a>(store: &'a ProtocolStore, acoustic: &'a dyn Fn(Option<AcousticSettings>) -> AcousticSettings) -> Desk<'a> {
        Desk { store, run: &|_| {}, acoustic }
    }

    /// SAVE keeps what he put together (a start it does not know is refused), a step typed and
    /// not added counts, a new name replaces the old one, another protocol's name is not taken
    /// over, and a sound is a start.
    #[test]
    fn the_form_saves_a_protocol_as_he_put_it_together() {
        let store = ProtocolStore::in_memory();
        let mut f = Form::default();
        f.name = "Wake up".into();
        f.adding = Adding::Say;
        // (A phrase no other test says: the table of phrases is the whole process's.)
        f.add_start("lights up max is here").unwrap();
        f.adding = Adding::Pc;
        f.add_start("Max comes online").unwrap();
        assert!(f.add_start("banana").unwrap_err().contains("\"banana\""));
        f.adding = Adding::Sound;
        f.add_start(&kit_notes::capitalised(Sound::Knock.words())).unwrap();
        f.add_step("open Spotify");
        f.add_step("tell me");
        f.step = "play my rock playlist".into();
        let p = save(&mut f, &store).unwrap();
        assert_eq!((p.name.as_str(), p.trigger_phrases.as_slice(), p.when.as_slice(), p.steps.len()), ("wake up", ["lights up max is here".to_string()].as_slice(), ["Max comes online".to_string(), "Someone knocks".to_string()].as_slice(), 3));
        assert!(p.steps.iter().all(|s| s.tool == protocols::ORDER));
        assert!(f.step.is_empty(), "the typed step was added");
        f.name = "welcome".into();
        save(&mut f, &store).unwrap();
        assert!(store.get("wake up").is_none() && store.get("welcome").is_some(), "renamed, not copied");
        f.name = "gaming".into();
        assert!(save(&mut f, &store).unwrap_err().contains("Gaming Protocol already"));
        let mut loaded = Form::default();
        loaded.load(store.get("welcome").as_ref());
        assert_eq!((loaded.say.len(), loaded.when.len(), loaded.steps.iter().map(Step::words).collect::<Vec<_>>()), (1, 2, vec!["open Spotify".to_string(), "tell me".into(), "play my rock playlist".into()]));
        assert!(store.delete("welcome"));
    }

    /// A built-in step reads as what it does, never as a tool and its arguments.
    #[test]
    fn a_built_in_step_reads_in_plain_words() {
        let store = ProtocolStore::in_memory();
        let gaming = store.get("gaming").expect("the built-in gaming protocol");
        let words: Vec<String> = gaming.steps.iter().map(Step::words).collect();
        assert!(words.iter().all(|w| !w.contains('{') && !w.contains('_')), "{words:?}");
        assert!(words.iter().any(|w| w == "Set the volume to 40 %"), "{words:?}");
        assert!(protocols::built_in("gaming") && !protocols::built_in("door watch"));
    }

    /// The window whole: how it works, what he heard, every protocol, the rules said aloud and
    /// his own reactions on the left, a protocol with a sound among its starts on the right.
    #[test]
    fn the_protocol_window_shows_everything_that_starts_by_itself() {
        let _look = crate::holo_theme::look_for_pictures();
        let _kit = crate::kit::test_lock();
        let _heard = crate::sound_sense::test_lock();
        let store = ProtocolStore::in_memory();
        let settings = Mutex::new(AcousticSettings::default());
        let acoustic = |set: Option<AcousticSettings>| {
            if let Some(a) = set {
                *settings.lock().unwrap() = a;
            }
            settings.lock().unwrap().clone()
        };
        kit_notes::rule(&serde_json::json!({"action": "add", "when": {"kind": "hears", "sound": "clap2"}, "then": "open Spotify"})).unwrap();
        crate::sound_sense::note(crate::acoustic_radar::AcousticEvent::Knock { count: 3 }, Some(Sound::Knock), None);
        form().load(Some(&Protocol {
            name: "door watch".into(),
            description: String::new(),
            trigger_phrases: vec!["who is at the door".into()],
            when: vec!["Someone knocks".into(), "Max comes online".into()],
            steps: ["tell me", "pause the music", "tell Max's PC to open Minecraft"].iter().filter_map(|l| Step::of_line(l)).collect(),
        }));
        form().said = ("Saved. Say \"engage the door watch protocol\" or \"who is at the door\".".into(), false);
        let mut stage = Stage::new(egui::vec2(940.0, 700.0));
        let draw = |stage: &mut Stage, name: &str| {
            let frame = stage.frames(4, &mut |ctx| {
                egui::Window::new("protocols").title_bar(false).fixed_pos(egui::pos2(20.0, 20.0)).fixed_size(egui::vec2(900.0, 660.0)).show(ctx, |ui| body(ui, &desk_with(&store, &acoustic)));
            });
            stage.raster(&frame).save(name);
            assert_eq!(visual::clipped_text(&frame), Vec::<String>::new(), "{name}");
            assert_eq!(visual::text_over_text(&frame), Vec::<String>::new(), "{name}");
            frame
        };
        let frame = draw(&mut stage, "panel_protocols");
        for text in [
            "STARTS WHEN", "THEN I DO", "I HEAR", "knocking", "NEW PROTOCOL", "PROTOCOLS", "Gaming Protocol", "SAID ALOUD", "I clap twice", "MY OWN", "Two claps", "Knocking", "YOU SAY", "SOUND", "Someone knocks", "PC", "Tell me what happened", "pause the music", "A SOUND", "SAVE", "RUN NOW", "DELETE",
        ] {
            assert!(!frame.find_text(text).is_empty(), "{text} is not in the protocol window");
        }
        // His own reaction to two claps: his rule for them takes its place.
        form().choose(Chosen::Claps);
        let frame = draw(&mut stage, "panel_protocols_claps");
        for text in ["TWO CLAPS", "LISTEN", "PLAY OR PAUSE THE MUSIC", "Yours takes its place now: open Spotify."] {
            assert!(!frame.find_text(text).is_empty(), "{text} is not on the reaction");
        }
        // A rule said aloud, and the sounds to add as a start.
        let key = kit_notes::rules().into_iter().find(|(_, w, _)| *w == When::Hears(Sound::Claps(2))).map(|(k, ..)| k).unwrap();
        form().choose(Chosen::Rule(key));
        let frame = draw(&mut stage, "panel_protocols_rule");
        for text in ["A RULE YOU SAID ALOUD", "MAKE IT A PROTOCOL", "open Spotify"] {
            assert!(!frame.find_text(text).is_empty(), "{text} is not on the rule");
        }
        form().load(None);
        form().adding = Adding::Sound;
        let frame = draw(&mut stage, "panel_protocols_sounds");
        for text in ["I CLAP TWICE", "SOMEONE KNOCKS", "THE DOORBELL RINGS", "SOMEONE ELSE TALKS"] {
            assert!(!frame.find_text(text).is_empty(), "{text} is not among the sounds");
        }
        form().load(None);
        assert!(kit_notes::rule(&serde_json::json!({"action": "remove", "all": true})).is_ok());
    }

    /// A long protocol's steps scroll into view, down to the last, in the window at its smallest
    /// (Sir, 2026-11-21: "in the protocol window you cant scroll down in the then i do part").
    #[test]
    fn a_long_protocol_scrolls_to_its_last_step() {
        let _look = crate::holo_theme::look_for_pictures();
        let _kit = crate::kit::test_lock();
        let _heard = crate::sound_sense::test_lock();
        let store = ProtocolStore::in_memory();
        let acoustic = |_: Option<AcousticSettings>| AcousticSettings::default();
        let mut lines = vec!["pause the music"; 11];
        lines.push("set the volume to 30");
        form().load(Some(&Protocol {
            name: "long watch".into(),
            description: String::new(),
            trigger_phrases: vec!["long watch".into()],
            when: vec!["Someone knocks".into()],
            steps: lines.iter().filter_map(|l| Step::of_line(l)).collect(),
        }));
        let window = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(720.0, 520.0));
        let mut stage = Stage::new(egui::vec2(740.0, 540.0));
        let mut draw = |ctx: &egui::Context| {
            egui::Window::new("protocols").title_bar(false).fixed_pos(window.min).fixed_size(window.size()).show(ctx, |ui| body(ui, &desk_with(&store, &acoustic)));
        };
        let seen = |frame: &visual::Frame| frame.find_text("Set the volume to 30").iter().any(|r| window.contains_rect(*r));
        assert!(!seen(&stage.frames(4, &mut draw)), "twelve steps fit the smallest window: this proves nothing");
        for _ in 0..6 {
            stage.scroll(egui::pos2(window.left() + LIST_W + 140.0, window.center().y), egui::vec2(0.0, -300.0));
            stage.frames(20, &mut draw);
        }
        let frame = stage.frames(4, &mut draw);
        form().load(None);
        assert!(seen(&frame), "the last step cannot be scrolled into view");
        assert_eq!(visual::clipped_text(&frame), Vec::<String>::new());
    }
}
