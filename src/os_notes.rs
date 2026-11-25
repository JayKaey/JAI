//! What programs tell Sir on J.A.I.OS: the notifications (org.freedesktop.Notifications) as a
//! short stack of cards, and the on-screen display for the volume, the brightness and the
//! like (what Plasma's osdService shows). The queue, its timing and the cards' drawing; the
//! bus side is `os_bus`.

use crate::holo::{self, Face};
use crate::os_shell::Icons;
use crate::{holo_frame, holo_kit};
use eframe::egui::{self, pos2, vec2, Align2, Rect, Sense, Vec2};

/// How long a notification stays when its program does not say.
pub const DEFAULT_SECS: f64 = 6.0;
/// How many cards stand at once (the newest; older ones wait behind them).
pub const SHOWN: usize = 4;
pub const CARD_W: f32 = 380.0;
const GAP: f32 = 8.0;

/// Why a notification closed, in the specification's numbers.
pub const EXPIRED: u32 = 1;
pub const DISMISSED: u32 = 2;
pub const CLOSED: u32 = 3;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Note {
    pub id: u32,
    pub app: String,
    /// Names its picture in the icons (empty: none).
    pub icon: String,
    pub summary: String,
    pub body: String,
    /// The keys it offers: each one's name on the bus and its label.
    pub actions: Vec<(String, String)>,
    /// When it leaves, in seconds on the shell's clock; None: when Sir closes it.
    pub until: Option<f64>,
}

/// When a notification asked for `timeout_ms` leaves: after that long (between two seconds
/// and a minute), after the default for -1, never for 0 or when it is critical (urgency 2).
pub fn until(timeout_ms: i32, urgency: u8, now: f64) -> Option<f64> {
    match timeout_ms {
        _ if urgency >= 2 => None,
        0 => None,
        ms if ms < 0 => Some(now + DEFAULT_SECS),
        ms => Some(now + (ms as f64 / 1000.0).clamp(2.0, 60.0)),
    }
}

/// A body as plain text: the markup the specification allows (`<b>`, `<i>`, `<a href>`,
/// `<br>`) is taken out, its entities are read.
pub fn plain(body: &str) -> String {
    let (mut out, mut tag) = (String::new(), None::<String>);
    for c in body.chars() {
        match (&mut tag, c) {
            (None, '<') => tag = Some(String::new()),
            (Some(name), '>') => {
                if name.trim_end_matches('/').trim().eq_ignore_ascii_case("br") {
                    out.push('\n');
                }
                tag = None;
            }
            (Some(name), c) => name.push(c),
            (None, c) => out.push(c),
        }
    }
    [("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&apos;", "'"), ("&amp;", "&")].iter().fold(out, |text, (entity, c)| text.replace(entity, c)).trim().to_string()
}

/// The keys of a notification from the bus's flat list (name, label, name, label, …).
pub fn actions(list: &[String]) -> Vec<(String, String)> {
    list.chunks_exact(2).map(|pair| (pair[0].clone(), pair[1].clone())).collect()
}

/// The notifications that are open, the oldest first.
#[derive(Debug, Default)]
pub struct Queue {
    pub notes: Vec<Note>,
}

impl Queue {
    /// A new notification, or the new words of one that is open (it keeps its place).
    pub fn put(&mut self, note: Note) {
        match self.notes.iter_mut().find(|n| n.id == note.id) {
            Some(open) => *open = note,
            None => self.notes.push(note),
        }
    }

    /// Whether `id` was open.
    pub fn close(&mut self, id: u32) -> bool {
        let before = self.notes.len();
        self.notes.retain(|n| n.id != id);
        self.notes.len() < before
    }

    /// Closes what has run out at `now`: their ids.
    pub fn expire(&mut self, now: f64) -> Vec<u32> {
        let gone: Vec<u32> = self.notes.iter().filter(|n| n.until.is_some_and(|t| t <= now)).map(|n| n.id).collect();
        self.notes.retain(|n| !gone.contains(&n.id));
        gone
    }

    /// When the next one runs out.
    pub fn next_expiry(&self) -> Option<f64> {
        self.notes.iter().filter_map(|n| n.until).min_by(f64::total_cmp)
    }

    /// The cards that stand: the newest ones, the newest on top.
    pub fn shown(&self) -> Vec<&Note> {
        self.notes.iter().rev().take(SHOWN).collect()
    }
}

fn card_height(note: &Note) -> f32 {
    52.0 + if note.body.is_empty() { 0.0 } else { 36.0 } + if note.actions.iter().any(|(name, _)| name != "default") { 28.0 } else { 0.0 }
}

/// The cards' places from the top down, and the size of the surface that holds them.
pub fn stack(notes: &[&Note]) -> (Vec<Rect>, Vec2) {
    let mut y = 0.0;
    let cards: Vec<Rect> = notes
        .iter()
        .map(|note| {
            let r = Rect::from_min_size(pos2(0.0, y), vec2(CARD_W, card_height(note)));
            y = r.bottom() + GAP;
            r
        })
        .collect();
    (cards, vec2(CARD_W, (y - GAP).max(1.0)))
}

/// What Sir did with a card.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteAction {
    Dismiss(u32),
    /// A key of it (or the card itself: its "default" action) was pressed.
    Invoke(u32, String),
}

/// Draws the cards over the whole context and says what was clicked.
pub fn paint_notes(ctx: &egui::Context, notes: &[&Note], icons: &Icons) -> Option<NoteAction> {
    let t = holo::theme();
    let mut action = None;
    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
        let p = ui.painter().clone();
        for (note, r) in notes.iter().zip(stack(notes).0) {
            let r = r.shrink(1.0);
            let card = ui.interact(r, ui.id().with(("note", note.id)), Sense::click());
            holo_frame::card(&p, r, card.hovered() as u8 as f32 * 0.5, 1.0);
            // Who says it, and the key that closes it.
            let mut x = r.left() + 12.0;
            if icons.contains_key(&note.icon) {
                crate::os_shell::paint_icon(&p, icons, &note.icon, &note.app, Rect::from_min_size(pos2(x, r.top() + 9.0), Vec2::splat(16.0)), 1.0);
                x += 22.0;
            }
            let close = Rect::from_min_size(pos2(r.right() - 34.0, r.top() + 7.0), vec2(26.0, holo_kit::KEY_H - 2.0));
            let app = holo::galley_within(&p, Face::Label, &note.app, 8.0, t.muted(), close.left() - 8.0 - x);
            p.galley(pos2(x, r.top() + 12.0), app, t.muted());
            let x_key = ui.interact(close, ui.id().with(("close", note.id)), Sense::click());
            holo_kit::key(&p, close, "X", x_key.hovered() as u8 as f32, x_key.is_pointer_button_down_on(), false, 1.0);
            let summary = holo::galley_within(&p, Face::Strong, &note.summary, 14.0, t.ink(), r.width() - 24.0);
            p.galley(pos2(r.left() + 12.0, r.top() + 28.0), summary, t.ink());
            if !note.body.is_empty() {
                holo::wrapped(&p, pos2(r.left() + 12.0, r.top() + 49.0), r.width() - 24.0, Face::Body, &note.body, 12.5, t.muted(), 2);
            }
            let mut right = r.right() - 20.0;
            for (k, (name, label)) in note.actions.iter().enumerate().filter(|(_, (name, _))| name != "default").take(3) {
                let w = holo_kit::key_width(&p, label).min(190.0);
                let key_rect = Rect::from_min_size(pos2(right - w, r.bottom() - holo_kit::KEY_H - 8.0), vec2(w, holo_kit::KEY_H));
                right = key_rect.left() - 6.0;
                let key = ui.interact(key_rect, ui.id().with(("action", note.id, k)), Sense::click());
                holo_kit::key(&p, key_rect, label, key.hovered() as u8 as f32, key.is_pointer_button_down_on(), false, 1.0);
                if key.clicked() {
                    action = Some(NoteAction::Invoke(note.id, name.clone()));
                }
            }
            if x_key.clicked() {
                action = Some(NoteAction::Dismiss(note.id));
            } else if card.clicked() && action.is_none() {
                // The card itself: what the program calls its default, else it just goes.
                action = Some(if note.actions.iter().any(|(name, _)| name == "default") { NoteAction::Invoke(note.id, "default".into()) } else { NoteAction::Dismiss(note.id) });
            }
        }
    });
    action
}

// ----------------------------------------------------------------------------------- OSD ----

/// The on-screen display's size in points, and how long it stands.
pub const OSD_SIZE: Vec2 = vec2(300.0, 58.0);
pub const OSD_SECS: f64 = 1.6;

/// What the on-screen display shows: a measure with its reading (0-1), or words.
#[derive(Debug, Clone, PartialEq)]
pub struct Osd {
    pub label: String,
    pub value: Option<f32>,
    pub text: String,
}

/// The display for a call of Plasma's `org.kde.osdService`: the method's name with its first
/// number, switch or (last) words, whichever it carries. None for a call that shows nothing.
pub fn osd_of(method: &str, number: Option<i32>, flag: Option<bool>, words: &str) -> Option<Osd> {
    let measure = |label: &str| number.map(|n| Osd { label: label.into(), value: Some((n as f32 / 100.0).clamp(0.0, 1.5)), text: format!("{n}") });
    let switch = |label: &str| flag.map(|on| Osd { label: label.into(), value: None, text: if on { "ON" } else { "OFF" }.into() });
    let said = |label: &str| (!words.is_empty()).then(|| Osd { label: label.into(), value: None, text: words.to_string() });
    match method {
        "volumeChanged" => measure("VOLUME"),
        "microphoneVolumeChanged" => measure("MICROPHONE"),
        "mediaPlayerVolumeChanged" => measure(if words.is_empty() { "PLAYER" } else { words }),
        "brightnessChanged" | "screenBrightnessChanged" => measure("BRIGHTNESS"),
        "keyboardBrightnessChanged" => measure("KEYBOARD LIGHT"),
        "kbdLayoutChanged" => said("KEYBOARD"),
        "virtualDesktopChanged" => said("DESKTOP"),
        "powerProfileChanged" => said("POWER"),
        "showText" => said(""),
        "touchpadEnabledChanged" => switch("TOUCHPAD"),
        "wifiEnabledChanged" => switch("WI-FI"),
        "bluetoothEnabledChanged" => switch("BLUETOOTH"),
        "wwanEnabledChanged" => switch("MOBILE DATA"),
        "virtualKeyboardEnabledChanged" => switch("SCREEN KEYBOARD"),
        _ => None,
    }
}

/// Draws the on-screen display over the whole context.
pub fn paint_osd(ctx: &egui::Context, osd: &Osd) {
    let t = holo::theme();
    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
        let r = ui.max_rect().shrink(1.0);
        let p = ui.painter();
        holo_frame::card(p, r, 0.5, 1.0);
        match osd.value {
            Some(value) => {
                holo::text(p, pos2(r.left() + 14.0, r.top() + 11.0), Align2::LEFT_TOP, Face::Label, &osd.label, 8.5, t.muted());
                holo::text(p, pos2(r.right() - 22.0, r.top() + 7.0), Align2::RIGHT_TOP, Face::Figure, &osd.text, 18.0, t.ink());
                // (Past 100 %, a volume that is raised over its measure, the cells show in the caution.)
                holo_kit::cells(p, Rect::from_min_max(pos2(r.left() + 14.0, r.bottom() - 22.0), pos2(r.right() - 22.0, r.bottom() - 12.0)), value.min(1.0), 24, if value > 1.0 { t.warn() } else { t.accent() }, 1.0);
            }
            None => {
                holo::text(p, pos2(r.left() + 14.0, r.top() + 11.0), Align2::LEFT_TOP, Face::Label, &osd.label, 8.5, t.muted());
                let words = holo::galley_within(p, Face::Head, &osd.text, 17.0, t.ink(), r.width() - 40.0);
                p.galley(pos2(r.left() + 14.0, r.bottom() - 12.0 - words.size().y), words, t.ink());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{clipped_text, text_over_text, Stage};

    fn note(id: u32, until: Option<f64>) -> Note {
        Note { id, app: "Discord".into(), summary: format!("Message {id}"), until, ..Note::default() }
    }

    #[test]
    fn notifications_queue_replace_and_run_out() {
        assert_eq!((until(-1, 1, 10.0), until(0, 1, 10.0), until(500, 0, 10.0), until(9000, 1, 10.0), until(600_000, 1, 10.0), until(3000, 2, 10.0)), (Some(16.0), None, Some(12.0), Some(19.0), Some(70.0), None));
        let mut queue = Queue::default();
        for (id, until) in [(1, Some(16.0)), (2, None), (3, Some(12.0)), (4, Some(30.0)), (5, Some(30.0))] {
            queue.put(note(id, until));
        }
        // The newest four stand, the newest on top; the oldest waits behind them.
        assert_eq!(queue.shown().iter().map(|n| n.id).collect::<Vec<_>>(), [5, 4, 3, 2]);
        assert_eq!(queue.next_expiry(), Some(12.0));
        assert_eq!(queue.expire(11.9), Vec::<u32>::new());
        assert_eq!(queue.expire(12.0), [3]);
        assert_eq!(queue.shown().iter().map(|n| n.id).collect::<Vec<_>>(), [5, 4, 2, 1]);
        // A program writes over its own notification: it keeps its place and gets new time.
        queue.put(Note { summary: "Message 1, again".into(), ..note(1, Some(40.0)) });
        assert_eq!(queue.notes.iter().map(|n| n.id).collect::<Vec<_>>(), [1, 2, 4, 5]);
        assert_eq!(queue.expire(31.0), [4, 5]);
        assert!(queue.close(2) && !queue.close(2));
        assert_eq!(queue.next_expiry(), Some(40.0));
        assert_eq!(queue.expire(1e9), [1]);
        assert_eq!((queue.next_expiry(), queue.shown().len()), (None, 0));
    }

    #[test]
    fn a_notifications_words_and_keys_are_read_from_the_bus_shapes() {
        assert_eq!(plain("<b>Max</b> wrote:<br/>see <a href=\"https://example.com\">this</a> &amp; that &lt;now&gt;"), "Max wrote:\nsee this & that <now>");
        assert_eq!(plain("  plain  "), "plain");
        let list: Vec<String> = ["default", "Open", "reply", "Reply", "odd"].map(String::from).to_vec();
        assert_eq!(actions(&list), [("default".to_string(), "Open".to_string()), ("reply".to_string(), "Reply".to_string())]);
        // The cards stack downwards; one with a body and keys is taller.
        let (bare, full) = (note(1, None), Note { body: "Hello".into(), actions: actions(&list), ..note(2, None) });
        let (cards, size) = stack(&[&full, &bare]);
        assert_eq!((cards[0].height(), cards[1].height()), (116.0, 52.0));
        assert_eq!((cards[1].top(), size), (124.0, vec2(CARD_W, 176.0)));
        assert_eq!(stack(&[]).1, vec2(CARD_W, 1.0));
        // Why a notification closed, as the specification numbers it.
        assert_eq!((EXPIRED, DISMISSED, CLOSED), (1, 2, 3));
    }

    #[test]
    fn plasmas_osd_calls_become_a_measure_a_switch_or_words() {
        let volume = osd_of("volumeChanged", Some(62), None, "").expect("a display");
        assert_eq!((volume.label.as_str(), volume.value, volume.text.as_str()), ("VOLUME", Some(0.62), "62"));
        assert_eq!(osd_of("volumeChanged", Some(130), None, "").and_then(|o| o.value), Some(1.3));
        assert_eq!(osd_of("screenBrightnessChanged", Some(40), None, "DP-1").map(|o| o.label), Some("BRIGHTNESS".to_string()));
        assert_eq!(osd_of("mediaPlayerVolumeChanged", Some(50), None, "Sonora").map(|o| o.label), Some("Sonora".to_string()));
        assert_eq!(osd_of("wifiEnabledChanged", None, Some(false), "").map(|o| o.text), Some("OFF".to_string()));
        assert_eq!(osd_of("kbdLayoutChanged", None, None, "German").map(|o| (o.label, o.text)), Some(("KEYBOARD".to_string(), "German".to_string())));
        assert_eq!(osd_of("showText", None, None, "Do not disturb").map(|o| o.text), Some("Do not disturb".to_string()));
        // A call without what it should carry, or one nobody knows, shows nothing.
        assert_eq!((osd_of("volumeChanged", None, None, ""), osd_of("showText", None, None, ""), osd_of("somethingNew", Some(1), None, "")), (None, None, None));
        // (Long enough to read, gone before it is in the way.)
        assert!((1.0..=3.0).contains(&OSD_SECS));
    }

    #[test]
    fn os_notes_picture_of_notifications_and_the_osd() {
        let _look = crate::holo_theme::look_for_pictures();
        let icons = Icons::new();
        let list: Vec<String> = ["default", "Open", "reply", "Reply", "mute", "Mute for an hour"].map(String::from).to_vec();
        let notes = [
            Note { id: 7, app: "Discord".into(), summary: "Max Mustermann".into(), body: plain("Are you coming to the raid tonight? We start at eight, <b>bring potions</b> and the long sword."), actions: actions(&list), ..Note::default() },
            Note { id: 8, app: "Steam".into(), summary: "Download complete: a game with quite a long name that will not fit".into(), ..Note::default() },
            Note { id: 9, app: "J.A.I.OS".into(), summary: "Updates are ready".into(), body: "Say \"update the system\" when it suits you.".into(), ..Note::default() },
        ];
        let shown: Vec<&Note> = notes.iter().collect();
        let (cards, size) = stack(&shown);
        let mut stage = Stage::new(size);
        let frame = stage.frames(2, &mut |ctx| drop(paint_notes(ctx, &shown, &icons)));
        stage.raster(&frame).save("os_notes_cards");
        assert_eq!(clipped_text(&frame), Vec::<String>::new());
        assert_eq!(text_over_text(&frame), Vec::<String>::new());
        for word in ["Max Mustermann", "REPLY", "MUTE FOR AN HOUR", "Updates are ready", "DISCORD"] {
            assert!(!frame.find_text(word).is_empty(), "{word} is not on the cards:\n{}", frame.all_text());
        }
        // A key, the close key, the card itself (its default action; without one it goes).
        let mut asked = Vec::new();
        let close = |card: Rect| pos2(card.right() - 22.0, card.top() + 16.0);
        for at in [frame.find_exact("REPLY")[0].center(), close(cards[1]), cards[0].center(), cards[2].center()] {
            stage.move_pointer(at);
            stage.frame(&mut |ctx| drop(paint_notes(ctx, &shown, &icons)));
            stage.press(egui::PointerButton::Primary, true);
            stage.frame(&mut |ctx| drop(paint_notes(ctx, &shown, &icons)));
            stage.press(egui::PointerButton::Primary, false);
            stage.frame(&mut |ctx| asked.extend(paint_notes(ctx, &shown, &icons)));
        }
        assert_eq!(asked, [NoteAction::Invoke(7, "reply".into()), NoteAction::Dismiss(8), NoteAction::Invoke(7, "default".into()), NoteAction::Dismiss(9)]);

        for (name, osd) in [("volume", osd_of("volumeChanged", Some(62), None, "")), ("over", osd_of("volumeChanged", Some(120), None, "")), ("words", osd_of("kbdLayoutChanged", None, None, "German (no dead keys)"))] {
            let osd = osd.expect("a display");
            let mut stage = Stage::new(OSD_SIZE);
            let frame = stage.frames(2, &mut |ctx| paint_osd(ctx, &osd));
            stage.raster(&frame).save(&format!("os_notes_osd_{name}"));
            assert_eq!(clipped_text(&frame), Vec::<String>::new(), "{name}");
            assert_eq!(text_over_text(&frame), Vec::<String>::new(), "{name}");
        }
    }
}
