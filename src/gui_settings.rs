//! Every switch in one place (Sir, 2026-11-12: "one panel with search"): a block with a search
//! field, the groups as keys under it, and the settings in groups, each with one line that says
//! what it does. What used to be spread over the VIEW, SCREENS and THEME menus, the drawer, the
//! Theme Studio, the Feature Lab and the Integrations hub is found here; the deeper tools open
//! from their row. "Open settings", VIEW → ALL SETTINGS.

use super::{strip_font, strip_ink, strip_muted, ts};
use crate::holo::{self, Face};
use crate::interface::{HudLayout, InterfaceSettings};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, FontId, Layout, Rect, RichText, Rounding, Sense, Ui};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) const ID: &str = "settings";
const TITLE: &str = "Settings // everything in one place";

static OPEN: AtomicBool = AtomicBool::new(false);
/// The search field takes the keyboard when the panel opens.
static FOCUS: AtomicBool = AtomicBool::new(false);

pub(super) fn open(open: bool) {
    if open && !OPEN.swap(true, Ordering::AcqRel) {
        FOCUS.store(true, Ordering::Release);
        *GAMES.lock().unwrap_or_else(|e| e.into_inner()) = None;
        crate::hud_dock::cancel_close(ID);
    } else if !open {
        OPEN.store(false, Ordering::Release);
    }
}

pub(super) fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// A deeper tool a row opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    Devices,
    ThemeStudio,
    Screens,
    Hub,
    FeatureLab,
    Mixer,
    VoiceSetup,
    Missed,
}

/// What the panel asks of the HUD this frame.
pub(super) enum Out {
    Set(Box<InterfaceSettings>),
    Open(Tool),
}

#[derive(Clone, Copy)]
enum Control {
    /// A setting that is kept.
    Switch(fn(&InterfaceSettings) -> bool, fn(&mut InterfaceSettings, bool)),
    /// A state of the moment (`hud_feel::apply` with this key).
    Moment(fn(&InterfaceSettings) -> bool, &'static str),
    /// A size, changed a step at a time: (get, set, least, most).
    Size(fn(&InterfaceSettings) -> f32, fn(&mut InterfaceSettings, f32), f32, f32),
    /// One of a few, each a key: (its label, whether it is the one chosen, choosing it).
    Keys(&'static [(&'static str, fn(&InterfaceSettings) -> bool, fn(&mut InterfaceSettings))]),
    /// His name, typed.
    Name,
    /// The HUD's colours: a swatch for each ready-made theme.
    Theme,
    /// A deeper tool, behind a key with this label.
    Open(Tool, &'static str),
    /// Game mode now (the switch), and under it every installed game with its own switch.
    Games,
}

/// The installed games for the list: (name, the folder title detection knows it by); None until
/// read (again each time the panel opens), off the HUD's thread.
pub(super) static GAMES: std::sync::Mutex<Option<Vec<(String, String)>>> = std::sync::Mutex::new(None);

struct Row {
    group: &'static str,
    name: &'static str,
    /// One line: what it does.
    explain: &'static str,
    /// More words the search finds it by.
    words: &'static str,
    control: Control,
}

const fn row(group: &'static str, name: &'static str, explain: &'static str, words: &'static str, control: Control) -> Row {
    Row { group, name, explain, words, control }
}

/// The groups, in the order they stand (each is also a key that shows it alone).
const GROUPS: [&str; 6] = ["LOOK", "SCREENS", "VOICE", "PRESENCE", "GAMES", "TOOLS"];

static ROWS: [Row; 49] = [
    row("LOOK", "Theme", "The HUD's colours: one click on a swatch changes them.", "colour color palette gold cyan red green farbe farben design", Control::Theme),
    row("LOOK", "Your own theme", "Build a theme from your own colours, name it and save it.", "theme studio custom create", Control::Open(Tool::ThemeStudio, "THEME STUDIO")),
    row(
        "LOOK",
        "Core",
        "The instrument of rings, or the sphere of circuitry.",
        "orb emblem simple complex sphere rings kern kugel",
        Control::Keys(&[("SIMPLE", |s| s.core_style != "complex", |s| s.core_style = "simple".into()), ("COMPLEX", |s| s.core_style == "complex", |s| s.core_style = "complex".into())]),
    ),
    row("LOOK", "Core size", "How large the core stands in the middle of the board.", "orb emblem bigger smaller kugel größe", Control::Size(|s| s.orb_scale, |s, v| s.orb_scale = v, 0.4, 2.5)),
    row(
        "LOOK",
        "Opacity",
        "How solid the black behind the light is. Lower, and the desktop shows through.",
        "transparency transparent see through durchsichtig deckkraft",
        Control::Size(|s| s.opacity as f32 / 100.0, |s, v| s.opacity = (v * 100.0).round() as u8, 0.15, 1.0),
    ),
    row("LOOK", "Name", "The name his window goes by in the taskbar.", "title titel call yourself", Control::Name),
    row("LOOK", "Display finish", "Scan lines, a fine grain and darker edges: the HUD reads as light on glass.", "rough grain scan lines glass scratches", Control::Switch(|s| s.display_finish, |s, v| s.display_finish = v)),
    row("LOOK", "Animations", "Blocks unfold, glide and fold away. Off: everything appears at once.", "motion movement animation bewegung", Control::Switch(|s| s.animations, |s, v| s.animations = v)),
    row("LOOK", "Calm HUD", "The readouts at the top and bottom step back until the mouse comes near.", "quiet dense readouts header telemetry", Control::Switch(|s| s.calm_hud, |s, v| s.calm_hud = v)),
    row("LOOK", "Status pill", "The word at the top that says what he is doing: LISTENING, THINKING, SPEAKING.", "state", Control::Switch(|s| s.status_pill, |s, v| s.status_pill = v)),
    row("LOOK", "Voice ring", "Your voice as a ring of ticks round the emblem while you speak.", "rim microphone level", Control::Switch(|s| s.voice_rim, |s, v| s.voice_rim = v)),
    row("LOOK", "Conversation block", "What you said and what he answers, in a block above the bottom strip.", "comms captions", Control::Switch(|s| s.comms_block, |s, v| s.comms_block = v)),
    row("LOOK", "Edge light", "The screen's edges glow for a moment when something happens.", "glow notification alert", Control::Switch(|s| s.edge_light, |s, v| s.edge_light = v)),
    row("LOOK", "Music visuals", "The emblem moves with the music that plays on the PC.", "visualiser visualizer spectrum song", Control::Switch(|s| s.music_visuals, |s, v| s.music_visuals = v)),
    row("LOOK", "Text size", "How large the text is.", "font bigger smaller schrift größer kleiner", Control::Size(|s| s.font_scale, |s, v| s.font_scale = v, 0.8, 1.6)),
    row("LOOK", "Zoom", "How large everything is.", "scale bigger smaller", Control::Size(|s| s.scale, |s, v| s.scale = v, 0.7, 1.6)),
    row("SCREENS", "Fullscreen board", "The HUD fills its monitor. Off: a window you can move and size.", "full screen window f11 vollbild fenster", Control::Switch(|s| s.fullscreen, |s, v| s.fullscreen = v)),
    row("SCREENS", "One board across all monitors", "The board spans every screen; the HUD proper stays on the first.", "span monitors bildschirme", Control::Switch(|s| s.span_all_monitors, |s, v| s.span_all_monitors = v)),
    row("SCREENS", "Always on top", "The HUD stays in front of other programs.", "front", Control::Switch(|s| s.always_on_top, |s, v| s.always_on_top = v)),
    row(
        "SCREENS",
        "Window size",
        "How large his window is when it is neither fullscreen nor maximised.",
        "layout full compact orb minimal fenster größe",
        Control::Keys(&[
            ("FULL", |s| s.layout == HudLayout::Full, |s| s.layout = HudLayout::Full),
            ("COMPACT", |s| s.layout == HudLayout::Compact, |s| s.layout = HudLayout::Compact),
            ("SMALL", |s| s.layout == HudLayout::Orb, |s| s.layout = HudLayout::Orb),
        ]),
    ),
    row("SCREENS", "Title bar", "The window's own title bar. Off: the top rail moves it.", "frameless frame border rahmen titelleiste", Control::Switch(|s| !s.frameless, |s, v| s.frameless = !v)),
    row("SCREENS", "Floating core", "Only the core, in a small window that floats over your work.", "pure orb mode kugel", Control::Switch(|s| s.orb_mode, |s, v| s.orb_mode = v)),
    row("SCREENS", "Windows may overlap", "New windows of his may open over each other instead of beside each other.", "stack cascade", Control::Switch(|s| s.allow_window_overlap, |s, v| s.allow_window_overlap = v)),
    row("SCREENS", "Mini mode", "A slim strip at the top of a screen instead of the board, for gaming or work.", "strip bar small compact leiste klein", Control::Switch(|s| s.mini_strip, |s, v| s.mini_strip = v)),
    row("SCREENS", "Live desktop background", "He is your desktop's background: the board behind your icons, his views as windows.", "wallpaper desktop hintergrund hintergrundbild background mode", Control::Switch(|s| s.desktop_background, |s, v| s.desktop_background = v)),
    row("SCREENS", "Background on every monitor", "As the background he covers all your monitors; off: your first one only.", "wallpaper all monitors every screen alle monitore hintergrund", Control::Switch(|s| s.background_all_monitors, |s, v| s.background_all_monitors = v)),
    row("SCREENS", "Words beside the corner orb", "What is said stands beside the orb in its corner. Off: the orb alone.", "info information window plate text orb corner kugel", Control::Switch(|s| s.orb_words, |s, v| s.orb_words = v)),
    row("SCREENS", "Corner orb size", "How large the orb is in its corner (the wheel over it does the same).", "orb size bigger smaller kugel größe", Control::Size(|s| s.corner_orb_scale, |s, v| s.corner_orb_scale = v, 0.6, 1.8)),
    row("SCREENS", "Desktop clock", "As the desktop's backdrop: the time under the emblem. Drag it anywhere.", "clock time uhr uhrzeit", Control::Switch(|s| s.desk_clock, |s, v| s.desk_clock = v)),
    row("SCREENS", "Clock: seconds", "The desktop clock shows its seconds.", "clock uhr sekunden", Control::Switch(|s| s.desk_clock_seconds, |s, v| s.desk_clock_seconds = v)),
    row("SCREENS", "Clock: twelve hours", "The desktop clock counts twelve hours with AM and PM, not twenty-four.", "clock uhr 12 24 am pm", Control::Switch(|s| s.desk_clock_12h, |s, v| s.desk_clock_12h = v)),
    row("SCREENS", "Clock: date", "The date stands under the desktop clock.", "clock uhr datum", Control::Switch(|s| s.desk_clock_date, |s, v| s.desk_clock_date = v)),
    row("SCREENS", "Monitors", "Which monitor the board is on.", "screens display bildschirm monitor", Control::Open(Tool::Screens, "SCREENS")),
    row("VOICE", "Live words", "Your words appear under the emblem while you speak (paused while a game runs).", "captions transcript mitschrift worte", Control::Switch(|s| s.live_words, |s, v| s.live_words = v)),
    row("VOICE", "Learn my voice", "One minute of reading aloud: he tunes his hearing to your voice and microphone.", "set-up setup calibrate wake name jay stimme lernen einrichten", Control::Open(Tool::VoiceSetup, "START")),
    row("VOICE", "Only obey me", "He answers to your voice alone, and learns it as you speak.", "lock speaker stranger everyone guest stimme sperre gast gäste", Control::Switch(|s| s.voice_lock, |s, v| s.voice_lock = v)),
    row("VOICE", "Short confirmations", "After an order he says it is done, without repeating what you just said.", "brief acknowledge talk less echo kurz bestätigung", Control::Switch(|s| s.brief_confirmations, |s, v| s.brief_confirmations = v)),
    row("VOICE", "Sound cues", "The short sounds when a block opens or closes, a report is ready, something fails.", "sounds earcons beep töne klänge", Control::Switch(|s| s.sound_cues, |s, v| s.sound_cues = v)),
    row("VOICE", "Microphone, devices and memory", "Pick the microphone and speaker, the wake word's strictness, what he remembers.", "audio input output drawer mikrofon lautsprecher geräte", Control::Open(Tool::Devices, "OPEN")),
    row("VOICE", "Mixer", "A fader for the master volume and for every program that plays sound.", "volume loud quiet lautstärke laut leise", Control::Open(Tool::Mixer, "MIXER")),
    row(
        "PRESENCE",
        if cfg!(windows) { "Start with Windows" } else { "Start at login" },
        "He starts by himself when you log in. Off: only when you start him.",
        "autostart startup boot login sign in automatically system hochfahren anmelden systemstart",
        Control::Moment(|_| crate::kit_pc::starts_with_system(), "start_with_system"),
    ),
    row("PRESENCE", "Start-up sequence", "He powers up with a short sequence, a longer one after a long time away.", "boot greeting start begrüßung", Control::Switch(|s| s.startup_sequence, |s, v| s.startup_sequence = v)),
    row("PRESENCE", "Idle screen", "Unused for five minutes, the HUD dims to the clock, your next appointment and the weather.", "screensaver away dim clock ruhebildschirm uhr", Control::Switch(|s| s.idle_screen, |s, v| s.idle_screen = v)),
    row("PRESENCE", "Night mode by the clock", "From 22:00 to 6:00 he is dimmer, makes no sounds and speaks quietly and briefly.", "evening late dark nacht nachtmodus abend", Control::Switch(|s| s.night_mode, |s, v| s.night_mode = v)),
    row("PRESENCE", "Night mode tonight", "On or off for this night only; the clock decides again in the morning.", "now", Control::Moment(|s| crate::hud_feel::night(s.night_mode), "night_now")),
    row("PRESENCE", "What did I miss?", "The notifications of the last hours.", "notifications history missed benachrichtigungen verpasst", Control::Open(Tool::Missed, "SHOW")),
    row("GAMES", "Game mode", "In a game his mind moves to RAM (0 MB VRAM). Below: what counts as a game.", "gaming spiele spielmodus steam epic vram batman gta minecraft", Control::Games),
    row("TOOLS", "Accounts and services", "E-mail, calendar, school, Discord, the phone link: the Integrations hub.", "integrations hub webuntis itslearning whatsapp", Control::Open(Tool::Hub, "HUB")),
    row("TOOLS", "Everything he can do", "The Feature Lab: every ability, each with a button to try it.", "features lab abilities help funktionen hilfe", Control::Open(Tool::FeatureLab, "FEATURE LAB")),
];

fn rows() -> impl Iterator<Item = &'static Row> {
    ROWS.iter()
}

/// Whether `row` answers the search: every word typed is found in its group, name, line or
/// extra words.
fn answers(row: &Row, needle: &str) -> bool {
    let hay = format!("{} {} {} {}", row.group, row.name, row.explain, row.words).to_lowercase();
    needle.to_lowercase().split_whitespace().all(|word| hay.contains(word))
}

/// A size one step on (`up`) or back, kept between `least` and `most`: steps of five percent.
fn stepped(value: f32, up: bool, least: f32, most: f32) -> f32 {
    let step = ((value * 20.0).round() + if up { 1.0 } else { -1.0 }) / 20.0;
    step.clamp(least, most)
}

/// A switch (the kit's): the whole of it is the target. Returns whether it was pressed.
fn switch(ui: &mut Ui, on: bool) -> bool {
    let mut shown = on;
    crate::holo_ui::toggle(ui, &mut shown, "").clicked()
}

/// A key (`lit`: it stands for what is chosen now).
fn key(ui: &mut Ui, label: &str, lit: bool) -> bool {
    crate::holo_ui::key_on(ui, label, lit).clicked()
}

/// The search field: the word SEARCH and a slot that lights while it has the keyboard.
fn search(ui: &mut Ui, needle: &mut String) {
    ui.horizontal(|ui| {
        let (tag, _) = ui.allocate_exact_size(vec2(56.0, 22.0), Sense::hover());
        holo::text(ui.painter(), tag.left_center(), Align2::LEFT_CENTER, Face::Label, "SEARCH", 8.0, strip_muted());
        let edit = egui::TextEdit::singleline(needle).hint_text("a word: night, voice, colour …").desired_width(ui.available_width()).font(FontId::proportional(ts(13.5))).text_color(strip_ink());
        let response = crate::holo_ui::slot(ui, edit);
        if FOCUS.swap(false, Ordering::AcqRel) {
            response.request_focus();
        }
    });
}

fn body(ui: &mut Ui, iface: &InterfaceSettings) -> Option<Out> {
    let mut out = None;
    let text_id = egui::Id::new("jai_settings_search");
    let mut needle = ui.data(|d| d.get_temp::<String>(text_id)).unwrap_or_default();
    search(ui, &mut needle);
    ui.add_space(6.0);
    // The groups as keys: one click shows a group alone, ALL shows everything.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if key(ui, "ALL", needle.trim().is_empty()) {
            needle.clear();
        }
        for group in GROUPS {
            if key(ui, &format!("{group}"), needle.trim().eq_ignore_ascii_case(group)) {
                needle = group.to_lowercase();
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(text_id, needle.clone()));
    ui.add_space(4.0);
    let shown: Vec<&Row> = rows().filter(|r| answers(r, &needle)).collect();
    if shown.is_empty() {
        let room = ui.available_rect_before_wrap();
        crate::holo_block::empty(ui, room, "No setting answers to that. Try another word.");
        return None;
    }
    let accent = crate::hud_board::active_colour();
    let kept = crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false, false]), |ui| {
        let mut group = "";
        // Where each group's name stands (for the one kept at the top).
        let mut headers: Vec<(&str, f32)> = Vec::new();
        for (k, row) in shown.iter().enumerate() {
            if row.group != group {
                group = row.group;
                ui.add_space(if k == 0 { 4.0 } else { 14.0 });
                let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
                group_title(ui.painter(), line, group);
                headers.push((group, line.top()));
                ui.add_space(4.0);
            }
            // The row under the pointer stands out a little.
            let band = Rect::from_min_size(ui.cursor().min - vec2(4.0, 2.0), vec2(ui.available_width() + 8.0, 48.0));
            if ui.rect_contains_pointer(band) {
                ui.painter().rect_filled(band, Rounding::ZERO, holo::a(strip_ink(), 0.045));
            }
            ui.horizontal(|ui| {
                ui.set_min_height(44.0);
                let width = ui.available_width();
                let wide = matches!(row.control, Control::Theme | Control::Keys(_));
                ui.allocate_ui_with_layout(vec2((width - if wide { 250.0 } else { 150.0 }).max(120.0), 44.0), Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(lit(row.name, &needle, FontId::new(ts(14.5), super::label_family()), strip_ink(), accent));
                    ui.add(egui::Label::new(lit(row.explain, &needle, FontId::proportional(ts(11.5)), strip_muted(), accent)).wrap());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| match row.control {
                    Control::Switch(get, set) => {
                        let on = get(iface);
                        if switch(ui, on) {
                            let mut next = iface.clone();
                            set(&mut next, !on);
                            out = Some(Out::Set(Box::new(next)));
                        }
                    }
                    Control::Moment(get, key) => {
                        let on = get(iface);
                        if switch(ui, on) {
                            crate::hud_feel::apply(&json!({ key: !on }));
                        }
                    }
                    Control::Size(get, set, least, most) => {
                        // A step at a time: the panel never jumps under a dragged handle.
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let value = get(iface);
                        let up = key(ui, "+", false);
                        ui.label(RichText::new(format!("{:>3.0}%", value * 100.0)).font(strip_font()).color(strip_ink()));
                        let down = key(ui, "-", false);
                        if up != down {
                            let stepped = stepped(value, up, least, most);
                            if (stepped - value).abs() > 0.001 {
                                let mut next = iface.clone();
                                set(&mut next, stepped);
                                next.sanitize();
                                out = Some(Out::Set(Box::new(next)));
                            }
                        }
                    }
                    Control::Keys(keys) => {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        // (Laid from the right: the last key first.)
                        for (label, chosen, choose) in keys.iter().rev() {
                            if key(ui, label, chosen(iface)) && !chosen(iface) {
                                let mut next = iface.clone();
                                choose(&mut next);
                                out = Some(Out::Set(Box::new(next)));
                            }
                        }
                    }
                    Control::Name => {
                        // What is typed is kept until the field is left, then it is his name.
                        let id = egui::Id::new("jai_settings_name");
                        let mut name = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| iface.title.clone());
                        let field = crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut name).desired_width(130.0).font(FontId::proportional(ts(13.5))).text_color(strip_ink()));
                        if field.has_focus() {
                            ui.data_mut(|d| d.insert_temp(id, name));
                        } else if ui.data_mut(|d| d.remove_temp::<String>(id)).is_some() && name.trim() != iface.title {
                            let mut next = iface.clone();
                            next.title = name;
                            next.sanitize();
                            out = Some(Out::Set(Box::new(next)));
                        }
                    }
                    Control::Theme => {
                        // A leaning swatch for each ready-made theme and his six newest own ones:
                        // its accent over its deep tone; the one in use stands full size on a
                        // rule of its own light.
                        ui.spacing_mut().item_spacing.x = 3.0;
                        let own = crate::theme_store::custom_themes();
                        let swatches = own.iter().rev().take(6).map(|t| (t.key(), t.name.clone(), t.theme()));
                        for (name, label, palette) in swatches.chain(crate::holo_theme::PRESETS.iter().rev().map(|(name, _, look)| (name.to_string(), crate::holo_theme::label(name), *look))) {
                            let palette = &palette;
                            let (rect, response) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                            let chosen = iface.hud_palette == name;
                            let p = ui.painter();
                            let swatch = rect.shrink2(vec2(1.0, if response.hovered() || chosen { 2.0 } else { 5.0 }));
                            let (top, foot) = swatch.split_top_bottom_at_fraction(0.7);
                            holo::fill(p, &crate::holo_kit::leaning(top), palette.accent());
                            holo::fill(p, &crate::holo_kit::leaning(foot), palette.deep());
                            if chosen {
                                holo::rule(p, rect.left_bottom(), rect.right_bottom(), palette.hot());
                            }
                            if response.on_hover_text(label).clicked() && !chosen {
                                let mut next = iface.clone();
                                next.hud_palette = name;
                                out = Some(Out::Set(Box::new(next)));
                            }
                        }
                    }
                    Control::Open(tool, label) => {
                        if key(ui, label, false) {
                            out = Some(Out::Open(tool));
                        }
                    }
                    Control::Games => {
                        let on = crate::vram_guard::in_game();
                        if switch(ui, on) {
                            crate::vram_guard::set_game_mode(Some(!on));
                        }
                    }
                });
            });
            if matches!(row.control, Control::Games) {
                games_list(ui);
            }
            ui.add_space(ROW_GAP);
        }
        ui.add_space(8.0);
        // The group of the rows at the top stays named there while they scroll by.
        let top = ui.clip_rect().top();
        headers.iter().rev().find(|(_, y)| *y < top - 1.0).map(|&(group, _)| (group, top))
    });
    // (Painted after the scroll area: over its fade, which would dim it.)
    if let Some((group, top)) = kept.inner {
        let band = Rect::from_min_size(pos2(kept.inner_rect.left(), top), vec2(kept.inner_rect.width(), 22.0));
        // (It takes the clicks too: the half-hidden row under it is not there to be pressed.)
        ui.interact(band, ui.id().with("kept_group_title"), Sense::click());
        ui.painter().rect_filled(band, Rounding::ZERO, super::block_slate());
        group_title(ui.painter(), band, group);
    }
    out
}

/// Space between two rows.
const ROW_GAP: f32 = 6.0;

/// A group's name and a hairline after it, in `line`.
fn group_title(p: &egui::Painter, line: Rect, group: &str) {
    // (Numbered as the groups stand in the row of keys.)
    let index = GROUPS.iter().position(|g| *g == group).map_or(String::new(), |k| format!("{:02}", k + 1));
    crate::holo_kit::section(p, pos2(line.left(), line.center().y - 6.0), line.width(), &index, group, 1.0);
}

/// `text` with each word of the search lit in the accent (the panel's search highlight).
fn lit(text: &str, needle: &str, font: FontId, colour: Color32, accent: Color32) -> egui::text::LayoutJob {
    let lower = text.to_lowercase();
    let mut marks = vec![false; text.len()];
    // (A letter whose small form is longer, rare outside Turkish, leaves the line unlit.)
    if lower.len() == text.len() {
        for word in needle.to_lowercase().split_whitespace() {
            for (at, _) in lower.match_indices(word) {
                marks[at..at + word.len()].fill(true);
            }
        }
    }
    let mut job = egui::text::LayoutJob::default();
    let mut i = 0;
    while i < text.len() {
        let on = marks[i];
        let mut j = i + 1;
        while j < text.len() && (marks[j] == on || !text.is_char_boundary(j)) {
            j += 1;
        }
        let format = egui::TextFormat { font_id: font.clone(), color: if on { accent } else { colour }, background: if on { accent.gamma_multiply(0.16) } else { Color32::TRANSPARENT }, ..Default::default() };
        job.append(&text[i..j], 0.0, format);
        i = j;
    }
    job
}

/// Under "Game mode": what it is doing now, and each installed game with its switch ("counts as
/// a game"; off: its start no longer brings game mode).
fn games_list(ui: &mut Ui) {
    let now = match crate::vram_guard::game_now() {
        Some((game, time)) => format!("NOW: {} · {} MIN · {}", game.to_uppercase(), time.as_secs() / 60, if crate::vram_guard::in_game() { "BRAIN IN RAM" } else { "GAME MODE OFF FOR IT" }),
        None if crate::vram_guard::in_game() => "NOW: GAME MODE ON · BRAIN IN RAM".to_string(),
        None => "NOW: NO GAME RUNNING · IT SWITCHES ITSELF ON WHEN ONE STARTS".to_string(),
    };
    ui.label(RichText::new(now).font(FontId::monospace(ts(11.0))).color(strip_ink()));
    let games = {
        let mut games = GAMES.lock().unwrap_or_else(|e| e.into_inner());
        if games.is_none() {
            *games = Some(Vec::new());
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let found = crate::kit_games::library().into_iter().map(|g| (g.name, g.folder)).collect();
                // (Reads the "not a game" list here, off the window thread, before a row asks.)
                let _ = crate::vram_guard::counts_as_game("");
                *GAMES.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
                ctx.request_repaint();
            });
        }
        games.clone().unwrap_or_default()
    };
    for (name, folder) in games {
        ui.horizontal(|ui| {
            ui.set_min_height(24.0);
            ui.label(RichText::new(&name).size(ts(12.5)).color(strip_ink()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let counts = crate::vram_guard::counts_as_game(&folder);
                if switch(ui, counts) {
                    crate::vram_guard::set_counts_as_game(&folder, !counts);
                }
            });
        });
    }
}

/// The settings panel while it is open: a block on the board. Returns what it asks of the HUD.
pub(super) fn show(ctx: &egui::Context, iface: &InterfaceSettings) -> Option<Out> {
    if crate::hud_feel::take_settings_panel() {
        open(true);
    }
    if !is_open() {
        crate::hud_dock::forget(ID);
        return None;
    }
    let mut out = None;
    // (`block`: a window of its own.)
    if super::block(ctx, ID, TITLE, [620.0, 680.0], |ui| out = body(ui, iface)) {
        open(false);
        crate::hud_dock::forget(ID);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_finds_a_setting_by_any_of_its_words() {
        let found = |needle: &str| -> Vec<&'static str> { rows().filter(|r| answers(r, needle)).map(|r| r.name).collect() };
        assert_eq!(found("night").len(), 2);
        assert!(found("wallpaper").contains(&"Live desktop background"));
        assert!(found("GRAIN").contains(&"Display finish"));
        assert_eq!(found("only obey"), vec!["Only obey me"]);
        assert!(found("voice").len() >= 4, "the whole group answers to its name");
        assert!(found("quantum flux").is_empty());
        // The search understands German words for the same things.
        assert_eq!(found("nachtmodus"), vec!["Night mode by the clock"]);
        assert_eq!(found("hintergrund"), vec!["Live desktop background", "Background on every monitor"]);
        assert!(found("stimme").contains(&"Only obey me") && found("stimme").contains(&"Learn my voice"));
        assert_eq!(found("lautstärke"), vec!["Mixer"]);
        assert_eq!(found("").len(), rows().count(), "an empty search shows everything");
        // A group's key shows that group (a setting of another group that mentions the word
        // may come with it, never fewer than the group itself).
        for group in GROUPS {
            let own = rows().filter(|r| r.group == group).count();
            assert!(own > 0 && found(&group.to_lowercase()).len() >= own, "{group}");
        }
        assert!(rows().all(|r| GROUPS.contains(&r.group)), "every row stands in a group that has a key");
    }

    /// Every switch is found under one name, says in a line what it does, and really sets what
    /// it shows.
    #[test]
    fn every_row_has_its_line_and_every_switch_sets_what_it_shows() {
        let mut names: Vec<&str> = rows().map(|r| r.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), rows().count(), "a name twice");
        let defaults = InterfaceSettings::default();
        for row in rows() {
            assert!(row.explain.ends_with('.') && row.explain.len() < 100, "{}: one line", row.name);
            match row.control {
                Control::Switch(get, set) => {
                    crate::hud_feel::set_voice_known(true);
                    let mut s = defaults.clone();
                    let was = get(&s);
                    set(&mut s, !was);
                    assert_eq!(get(&s), !was, "{}", row.name);
                    set(&mut s, was);
                    assert_eq!(s, defaults, "{} changes nothing else", row.name);
                }
                Control::Size(get, set, least, most) => {
                    let mut s = defaults.clone();
                    assert!((least..=most).contains(&get(&s)), "{}", row.name);
                    set(&mut s, most);
                    assert_eq!(get(&s), most);
                }
                Control::Keys(keys) => {
                    // Choosing a key shows that key as the one chosen, and no other.
                    for (k, (label, _, choose)) in keys.iter().enumerate() {
                        let mut s = defaults.clone();
                        choose(&mut s);
                        let chosen: Vec<usize> = keys.iter().enumerate().filter(|(_, (_, chosen, _))| chosen(&s)).map(|(i, _)| i).collect();
                        assert_eq!(chosen, [k], "{}: {label}", row.name);
                    }
                }
                Control::Moment(..) | Control::Open(..) | Control::Theme | Control::Games | Control::Name => {}
            }
        }
        // The features of 2026-11-12 are all here.
        for name in ["Calm HUD", "Live words", "Start-up sequence", "Idle screen", "Night mode by the clock", "Edge light", "Music visuals", "Mini mode", "Live desktop background", "Only obey me"] {
            assert!(names.contains(&name), "{name}");
        }
    }

    #[test]
    fn the_search_lights_the_words_it_found() {
        let job = lit("Night mode by the clock", "night clock", FontId::monospace(11.0), Color32::WHITE, Color32::RED);
        let lit: Vec<&str> = job.sections.iter().filter(|s| s.format.color == Color32::RED).map(|s| &job.text[s.byte_range.clone()]).collect();
        assert_eq!(lit, vec!["Night", "clock"]);
        assert_eq!(job.text, "Night mode by the clock", "the words stay as they were");
        let german = lit_text("Größer: die Schrift", "schrift");
        assert_eq!(german, vec!["Schrift"]);
    }

    fn lit_text(text: &str, needle: &str) -> Vec<String> {
        let job = lit(text, needle, FontId::monospace(11.0), Color32::WHITE, Color32::RED);
        job.sections.iter().filter(|s| s.format.color == Color32::RED).map(|s| job.text[s.byte_range.clone()].to_string()).collect()
    }

    #[test]
    fn a_size_goes_a_step_at_a_time_and_stays_in_its_range() {
        assert!((stepped(1.0, true, 0.8, 1.6) - 1.05).abs() < 1e-5);
        assert!((stepped(1.0, false, 0.8, 1.6) - 0.95).abs() < 1e-5);
        assert_eq!(stepped(1.6, true, 0.8, 1.6), 1.6);
        assert_eq!(stepped(0.8, false, 0.8, 1.6), 0.8);
        // From an odd value (Ctrl + Plus zooms by tenths of itself) it lands on a whole step.
        assert!((stepped(1.1 * 1.1, true, 0.7, 1.6) - 1.25).abs() < 1e-5);
    }
}
