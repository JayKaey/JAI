//! Every switch in one place (Sir, 2026-11-12: "one panel with search"): a block with a search
//! field and the settings in groups, each with one line that says what it does. What used to be
//! spread over the VIEW, SCREENS and THEME menus, the drawer, the Theme Studio, the Feature Lab
//! and the Integrations hub is found here; the deeper tools open from their row. "Open
//! settings", VIEW → ALL SETTINGS.

use super::{mix, stroke, strip_font, ts, STRIP_INK, STRIP_MUTED};
use crate::interface::InterfaceSettings;
use eframe::egui::{self, pos2, vec2, Align, Color32, FontId, Layout, Rect, RichText, Rounding, Sense, Ui};
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
    Interface,
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
    /// A size: (get, set, least, most).
    Size(fn(&InterfaceSettings) -> f32, fn(&mut InterfaceSettings, f32), f32, f32),
    /// A deeper tool, behind a key with this label.
    Open(Tool, &'static str),
}

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

static ROWS: [Row; 31] = [
    row("LOOK", "Theme", "The HUD's colours: pick one, or build and save your own.", "colour color palette gold cyan studio", Control::Open(Tool::ThemeStudio, "[ THEME STUDIO ]")),
    row("LOOK", "Colours, opacity and panels", "The accent and background colour, how see-through the HUD is, which panels show.", "interface accent background transparency", Control::Open(Tool::Interface, "[ OPEN ]")),
    row("LOOK", "Display finish", "Scan lines, grain, dust and hairline scratches: the HUD reads as light on glass.", "rough grain scan lines glass scratches", Control::Switch(|s| s.display_finish, |s, v| s.display_finish = v)),
    row("LOOK", "Animations", "Blocks unfold, glide and fold away. Off: everything appears at once.", "motion movement", Control::Switch(|s| s.animations, |s, v| s.animations = v)),
    row("LOOK", "Calm HUD", "The readouts at the top and bottom step back until the mouse comes near.", "quiet dense readouts header telemetry", Control::Switch(|s| s.calm_hud, |s, v| s.calm_hud = v)),
    row("LOOK", "Status pill", "The word at the top that says what he is doing: LISTENING, THINKING, SPEAKING.", "state", Control::Switch(|s| s.status_pill, |s, v| s.status_pill = v)),
    row("LOOK", "Voice ring", "Your voice as a ring of ticks round the emblem while you speak.", "rim microphone level", Control::Switch(|s| s.voice_rim, |s, v| s.voice_rim = v)),
    row("LOOK", "Conversation block", "What you said and what he answers, in a block above the bottom strip.", "comms captions", Control::Switch(|s| s.comms_block, |s, v| s.comms_block = v)),
    row("LOOK", "Edge light", "The screen's edges glow for a moment when something happens.", "glow notification alert", Control::Switch(|s| s.edge_light, |s, v| s.edge_light = v)),
    row("LOOK", "Music visuals", "The emblem moves with the music that plays on the PC.", "visualiser visualizer spectrum song", Control::Switch(|s| s.music_visuals, |s, v| s.music_visuals = v)),
    row("LOOK", "Text size", "How large the text is.", "font bigger smaller", Control::Size(|s| s.font_scale, |s, v| s.font_scale = v, 0.8, 1.6)),
    row("LOOK", "Zoom", "How large everything is.", "scale bigger smaller", Control::Size(|s| s.scale, |s, v| s.scale = v, 0.7, 1.6)),
    row("SCREENS", "Fullscreen board", "The HUD fills its monitor. Off: a window you can move and size.", "full screen window f11", Control::Switch(|s| s.fullscreen, |s, v| s.fullscreen = v)),
    row("SCREENS", "One board across all monitors", "The board spans every screen; the HUD proper stays on the first.", "span monitors", Control::Switch(|s| s.span_all_monitors, |s, v| s.span_all_monitors = v)),
    row("SCREENS", "Always on top", "The HUD stays in front of other programs.", "front", Control::Switch(|s| s.always_on_top, |s, v| s.always_on_top = v)),
    row("SCREENS", "Windows may overlap", "New windows of his may open over each other instead of beside each other.", "stack cascade", Control::Switch(|s| s.allow_window_overlap, |s, v| s.allow_window_overlap = v)),
    row("SCREENS", "Mini mode", "A slim strip at the top of a screen instead of the board, for gaming or work.", "strip bar small compact", Control::Switch(|s| s.mini_strip, |s, v| s.mini_strip = v)),
    row("SCREENS", "Live desktop background", "The emblem, the clock and his state behind your desktop icons.", "wallpaper desktop", Control::Switch(|s| s.desktop_background, |s, v| s.desktop_background = v)),
    row("SCREENS", "Monitors", "Which monitor the board is on.", "screens display", Control::Open(Tool::Screens, "[ SCREENS ]")),
    row("VOICE", "Live words", "Your words appear under the emblem while you speak (paused while a game runs).", "captions transcript", Control::Switch(|s| s.live_words, |s, v| s.live_words = v)),
    row("VOICE", "Learn my voice", "One minute of reading aloud: he tunes his hearing to your voice and microphone.", "set-up setup calibrate wake name jay", Control::Open(Tool::VoiceSetup, "[ START ]")),
    row("VOICE", "Only obey me", "He answers to your voice alone (after the voice set-up).", "lock speaker stranger everyone", Control::Moment(|_| crate::hud_feel::voice_locked(), "voice_lock")),
    row("VOICE", "Sound cues", "The short sounds when a block opens or closes, a report is ready, something fails.", "sounds earcons beep", Control::Switch(|s| s.sound_cues, |s, v| s.sound_cues = v)),
    row("VOICE", "Microphone, devices and memory", "Pick the microphone and speaker, the wake word's strictness, what he remembers.", "audio input output drawer", Control::Open(Tool::Devices, "[ OPEN ]")),
    row("VOICE", "Mixer", "A fader for the master volume and for every program that plays sound.", "volume loud quiet", Control::Open(Tool::Mixer, "[ MIXER ]")),
    row("PRESENCE", "Start-up sequence", "He powers up with a short sequence, a longer one after a long time away.", "boot greeting", Control::Switch(|s| s.startup_sequence, |s, v| s.startup_sequence = v)),
    row("PRESENCE", "Idle screen", "Unused for five minutes, the HUD dims to the clock, your next appointment and the weather.", "screensaver away dim clock", Control::Switch(|s| s.idle_screen, |s, v| s.idle_screen = v)),
    row("PRESENCE", "Night mode by the clock", "From 22:00 to 6:00 he is dimmer, makes no sounds and speaks quietly and briefly.", "evening late dark", Control::Switch(|s| s.night_mode, |s, v| s.night_mode = v)),
    row("PRESENCE", "Night mode tonight", "On or off for this night only; the clock decides again in the morning.", "now", Control::Moment(|s| crate::hud_feel::night(s.night_mode), "night_now")),
    row("PRESENCE", "What did I miss?", "The notifications of the last hours.", "notifications history missed", Control::Open(Tool::Missed, "[ SHOW ]")),
    row("TOOLS", "Accounts and services", "E-mail, calendar, school, Discord, the phone link: the Integrations hub.", "integrations hub webuntis itslearning whatsapp", Control::Open(Tool::Hub, "[ HUB ]")),
];

/// The one row outside the table (it needs no settings): everything he can do.
const FEATURE_LAB: Row = row("TOOLS", "Everything he can do", "The Feature Lab: every ability, each with a button to try it.", "features lab abilities help", Control::Open(Tool::FeatureLab, "[ FEATURE LAB ]"));

fn rows() -> impl Iterator<Item = &'static Row> {
    ROWS.iter().chain(std::iter::once(&FEATURE_LAB))
}

/// Whether `row` answers the search: every word typed is found in its group, name, line or
/// extra words.
fn answers(row: &Row, needle: &str) -> bool {
    let hay = format!("{} {} {} {}", row.group, row.name, row.explain, row.words).to_lowercase();
    needle.to_lowercase().split_whitespace().all(|word| hay.contains(word))
}

/// A switch: a small track with a knob that slides (it lights up under the pointer).
fn switch(ui: &mut Ui, id: egui::Id, on: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(38.0, 18.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(id, on, crate::hud_style::motion::FAST.as_secs_f32());
    let accent = crate::hud_board::active_colour();
    let p = ui.painter();
    p.rect_filled(rect, Rounding::same(2.0), mix(Color32::from_rgb(6, 12, 18), accent.gamma_multiply(0.35), t));
    let edge = if response.hovered() { accent } else { mix(crate::hud_board::border_colour(), accent, t) };
    p.rect_stroke(rect.shrink(0.5), Rounding::same(2.0), stroke(1.0, edge));
    let x = rect.left() + 9.0 + (rect.width() - 18.0) * crate::hud_style::motion::ease_out(t);
    let press = if response.is_pointer_button_down_on() { 1.0 } else { 0.0 };
    p.rect_filled(Rect::from_center_size(pos2(x, rect.center().y), vec2(12.0 - press, 12.0 - press)), Rounding::same(1.0), mix(STRIP_MUTED, Color32::WHITE, t));
    response.clicked()
}

fn key(ui: &mut Ui, label: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(label).font(strip_font()).color(STRIP_INK)).fill(Color32::from_rgba_unmultiplied(3, 7, 12, 230)).stroke(stroke(1.0, crate::hud_board::border_colour())).rounding(Rounding::ZERO)).clicked()
}

fn body(ui: &mut Ui, iface: &InterfaceSettings) -> Option<Out> {
    let mut out = None;
    let text_id = egui::Id::new("jai_settings_search");
    let mut needle = ui.data(|d| d.get_temp::<String>(text_id)).unwrap_or_default();
    let field = ui.add(egui::TextEdit::singleline(&mut needle).hint_text("Search the settings…").desired_width(f32::INFINITY).font(FontId::proportional(ts(13.5))));
    if FOCUS.swap(false, Ordering::AcqRel) {
        field.request_focus();
    }
    ui.data_mut(|d| d.insert_temp(text_id, needle.clone()));
    ui.add_space(6.0);
    let shown: Vec<&Row> = rows().filter(|r| answers(r, &needle)).collect();
    if shown.is_empty() {
        let room = ui.available_rect_before_wrap();
        crate::hud_style::empty(ui, room, "No setting answers to that. Try another word.");
        return None;
    }
    let accent = crate::hud_board::active_colour();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut group = "";
        for (k, row) in shown.iter().enumerate() {
            if row.group != group {
                group = row.group;
                ui.add_space(if k == 0 { 2.0 } else { 10.0 });
                ui.label(RichText::new(group).font(crate::hud_style::title_font(ts(12.5))).color(accent));
                let r = ui.cursor();
                ui.painter().line_segment([pos2(r.left(), r.top()), pos2(r.left() + ui.available_width(), r.top())], stroke(1.0, crate::hud_board::border_colour().gamma_multiply(0.6)));
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                ui.set_min_height(38.0);
                let width = ui.available_width();
                ui.allocate_ui_with_layout(vec2((width - 150.0).max(120.0), 38.0), Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(row.name).font(FontId::new(ts(14.0), super::label_family())).color(Color32::from_rgb(0xE8, 0xF2, 0xFC)));
                    ui.add(egui::Label::new(RichText::new(row.explain).size(ts(11.0)).color(STRIP_MUTED)).wrap());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| match row.control {
                    Control::Switch(get, set) => {
                        let on = get(iface);
                        if switch(ui, egui::Id::new(("jai_setting", row.name)), on) {
                            let mut next = iface.clone();
                            set(&mut next, !on);
                            out = Some(Out::Set(Box::new(next)));
                        }
                    }
                    Control::Moment(get, key) => {
                        let on = get(iface);
                        if switch(ui, egui::Id::new(("jai_setting", row.name)), on) {
                            crate::hud_feel::apply(&json!({ key: !on }));
                        }
                    }
                    Control::Size(get, set, least, most) => {
                        // Applied when the handle is let go, so the panel holds still while it
                        // is dragged.
                        let id = egui::Id::new(("jai_setting_size", row.name));
                        let mut value = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or_else(|| get(iface));
                        let response = ui.add(egui::Slider::new(&mut value, least..=most).show_value(false));
                        ui.label(RichText::new(format!("{:.0}%", value * 100.0)).font(strip_font()).color(STRIP_INK));
                        if response.dragged() {
                            ui.data_mut(|d| d.insert_temp(id, value));
                        } else {
                            ui.data_mut(|d| d.remove::<f32>(id));
                            if (value - get(iface)).abs() > 0.004 {
                                let mut next = iface.clone();
                                set(&mut next, value);
                                next.sanitize();
                                out = Some(Out::Set(Box::new(next)));
                            }
                        }
                    }
                    Control::Open(tool, label) => {
                        if key(ui, label) {
                            out = Some(Out::Open(tool));
                        }
                    }
                });
            });
        }
        ui.add_space(8.0);
    });
    out
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
    let action = super::hud_block_window(ctx, ID, TITLE, [600.0, 660.0], |ui| out = body(ui, iface));
    if action.close {
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
        assert_eq!(found("").len(), rows().count(), "an empty search shows everything");
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
                Control::Moment(..) | Control::Open(..) => {}
            }
        }
        // The features of 2026-11-12 are all here.
        for name in ["Calm HUD", "Live words", "Start-up sequence", "Idle screen", "Night mode by the clock", "Edge light", "Music visuals", "Mini mode", "Live desktop background"] {
            assert!(names.contains(&name), "{name}");
        }
    }
}
