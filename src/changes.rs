//! What changed in J.A.I., and when (Sir, 2026-11-13: "a button that shows a very structured
//! and clean list of all changes over time ... new features should have a link that shows an
//! explanation of that feature in a new window").
//!
//! `RELEASES` is the list, newest first: each day's work with what is new, what got better
//! and what was put right. A feature that needs explaining names its `EXPLANATIONS` entry:
//! what it does, what to say, what is good to know. The block (`show`) lists them with a
//! search field and a filter by kind; "HOW IT WORKS" opens the explanation in a window of its
//! own. The `[ CHANGES ]` box of the header strip, "what's new" and "show the changes" open it.
//!
//! Whoever adds a feature adds its line here (and its explanation): a test checks that every
//! link leads somewhere and every explanation is linked.

use crate::kit::say;
use eframe::egui::{self, Color32, FontFamily, FontId, RichText, Sense};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What kind of change an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    New,
    Better,
    Fixed,
}

impl Kind {
    pub fn tag(self) -> &'static str {
        match self {
            Kind::New => "NEW",
            Kind::Better => "BETTER",
            Kind::Fixed => "FIXED",
        }
    }
}

pub struct Entry {
    pub kind: Kind,
    pub title: &'static str,
    /// One line: what it is.
    pub what: &'static str,
    /// The id of its explanation, when it has one.
    pub explain: Option<&'static str>,
}

pub struct Release {
    /// The day ("2026-11-13").
    pub date: &'static str,
    pub name: &'static str,
    pub entries: &'static [Entry],
}

pub struct Explanation {
    pub id: &'static str,
    pub title: &'static str,
    /// What it does, in a few sentences.
    pub what: &'static str,
    /// What to say (or do).
    pub say: &'static [&'static str],
    /// Good to know.
    pub know: &'static str,
}

const fn new(title: &'static str, what: &'static str, explain: &'static str) -> Entry {
    Entry { kind: Kind::New, title, what, explain: Some(explain) }
}

const fn added(title: &'static str, what: &'static str) -> Entry {
    Entry { kind: Kind::New, title, what, explain: None }
}

const fn better(title: &'static str, what: &'static str) -> Entry {
    Entry { kind: Kind::Better, title, what, explain: None }
}

const fn better_see(title: &'static str, what: &'static str, explain: &'static str) -> Entry {
    Entry { kind: Kind::Better, title, what, explain: Some(explain) }
}

const fn fixed(title: &'static str, what: &'static str) -> Entry {
    Entry { kind: Kind::Fixed, title, what, explain: None }
}

/// Every change, newest first.
pub const RELEASES: &[Release] = &[
    Release {
        date: "2026-11-14",
        name: "He in the background",
        entries: &[
            new("He as your desktop's background", "The board behind your icons, his views as real windows, the orb in a corner when you call.", "desktop_background"),
            better("His windows on call", "Ctrl+Alt+J or \"show your windows\": all of them in front, with a list to move or close each."),
            better("Windows open where you work", "A new window of his opens on the monitor you are using; beside a game, on another one."),
            better("Tidy on your word", "\"Tidy the windows\" lines his windows up; nothing lines up by itself any more."),
            better("Across monitors without a jump", "A block in your hand crosses from one monitor to the next; let go on the gap, it glides in."),
            better("The day on the background", "Next lesson, next appointment, running timers and the song, beside the emblem."),
            better("Words and progress on the orb", "Beside the corner orb: what was said. Round it: a ring for a running timer or a search."),
            better("The orb where you want it", "Drag the corner orb anywhere; it comes out there on that monitor. A click opens the console."),
            better("A game is left alone", "Beside a game: no mouse hook, a smaller click-through orb, windows on another monitor."),
            better("Orders with a delay", "\"Play music in one minute\", \"close the board at 18:30\"; \"what is scheduled\", \"cancel that\"."),
            better("He asks before a guess changes anything", "\"Did you mean: create a protocol called 'no think', Sir?\" Yes does it, anything else drops it."),
            better("Did you mean", "For an order he does not know, the closest ones he does: \"the first one\", or a click."),
            better("\"It\" is what you just did", "After the live background: \"throw it on all monitors\". After a switch: \"turn it off again\"."),
            better("Desk layouts and the coach by more names", "\"Save that desk layout as standard\", \"layout standard\", \"open your pronunciation coach\"."),
            better("Quicker when he is sure", "The careful second hearing only runs when the first was unsure. \"Don't listen\" acts at once."),
            better("\"Don't listen\" with an end", "\"Don't listen for ten minutes\" or \"until eight\": he listens again by himself and says so."),
            fixed("Windows that never showed", "\"How it works\" and detached windows stayed hidden. They are shown a surer way now."),
            fixed("Shown while not listening", "Under \"don't listen\" nothing he hears is put on screen, unless you ask for the subtitles."),
            fixed("A lyric taken for an order", "A song line stitched to a noise went to the model without your name. Not any more."),
            fixed("\"Subtitles off\" by voice", "The recogniser's filter threw away every sentence with \"subtitles\" in it."),
            fixed("Alignment help", "Gone, as you asked: no snapping and no guide lines when you move a block."),
        ],
    },
    Release {
        date: "2026-11-13",
        name: "His ear, the kit, and this list",
        entries: &[
            new("Dictation", "He types what you say into the app in front, in German or English.", "dictation"),
            new("Pronunciation coach", "Read a sentence aloud and see word by word what reached him.", "coach"),
            new("Hearing log", "What he heard lately; click a line to tell him what you meant.", "hearing_log"),
            new("Hands-free keys", "\"Next tab\", \"scroll down\", \"reload the page\" for the app in front.", "keys"),
            new("Monitor brightness", "The real brightness of your monitors, by voice.", "brightness"),
            new("Desk layouts", "Save where every window stands and bring it back by name.", "layouts"),
            new("Whenever-rules", "\"Whenever Minecraft starts, set the music to 20 percent.\"", "rules"),
            new("Sleep timer", "\"Stop the music in 30 minutes\", with the volume sinking first.", "sleep_timer"),
            new("Notebook", "Notes by voice, searchable, on the board.", "notebook"),
            new("Read aloud", "He reads the text you selected in any app.", "read_aloud"),
            new("Explain selection", "Select text anywhere and say \"explain this\".", "explain_selection"),
            new("Evening recap", "\"What did I do today?\": the day in a few lines.", "recap"),
            new("Internet doctor", "\"Why is my internet slow?\": where the fault lies, in plain words.", "doctor"),
            new("Privacy board", "Everything he keeps about you, each with a key to delete it.", "privacy"),
            new("Known guests", "Guests he knows by voice and name, and what each may do.", "guests"),
            new("Portrait dashboard", "A dashboard for your upright monitor, behind your windows.", "dashboard"),
            new("Snippets", "Stored pieces of text that he types for you.", "snippets"),
            new("The changes list", "This list, with an explanation window for every feature.", "changes"),
            better_see("The voice lock learns your voice", "It grows with every sentence that is yours and no longer turns you away.", "voice_lock"),
            better_see("His ear for your words", "He learns how your words arrive and puts them right before acting.", "ear"),
            better("Orders in your own words", "One order, many wordings: \"I need the mixer\", \"mach das Diktat an\", \"it's too bright\"."),
            better("A second, careful hearing", "An order that leads nowhere is listened to again, slower and wider."),
            better("Your name for him as it arrives", "\"They open the mixer\" in your own voice is an order for him."),
            better("Guest mode", "\"Let my friend speak for half an hour\": he answers to anyone for a while."),
            better("Talk to others is left alone", "With his window in front he no longer answers what you say to someone else."),
            better("The microphone gate follows your voice", "It opens for a quiet voice and narrows for a strong one, sentence by sentence."),
            better("No false start", "A quoted German title or a German timetable no longer makes him begin again."),
            better("Whose voice he answers to", "The header says VOICE: YOURS, LEARNING, GUESTS or ANYONE; a click switches the lock."),
            better("What did you just hear?", "He says the last thing he heard from you and what he took it for."),
            better("Shorter confirmations", "\"Done, Sir.\" instead of repeating what you just said."),
            better("Errors in plain words", "\"The service is having trouble on its side (503)\" instead of the raw error text."),
            better("Console history", "Arrow up and down bring back what you typed, after a restart too; Escape clears the line."),
            better("Blocks remember their place", "A block opens where you left it, after a restart too."),
            better("Grouped notifications", "Five messages from one chat are one card and one entry in the missed list."),
            better("What did I miss survives a restart", "The notification history is kept; \"clear my notifications\" empties it."),
            better("Subtitles where you want them", "\"Subtitles to the top\", \"subtitles on monitor 2\", \"subtitles follow my window\"."),
            better("Tray switches", "The voice lock, guests, night mode and the standby from the tray icon's menu."),
            better("Phone quick buttons", "Pause, next, louder, quieter, sleep timer, night mode and a note from the phone page."),
            better("German for more phrases", "The switches that only knew English know their German words too."),
            better("Undo for more", "Reminders, the listening mode, the subtitle bar, brightness, notes, rules and snippets."),
            better_see("Nightly backups", "A whole copy of his memory and the settings once a day, seven days kept.", "backups"),
            better("Dated logs", "Every log line carries its day; three old logs are kept instead of one."),
            better("Lighter when you are away", "With no key or pointer for two minutes the board draws a third of the frames."),
            better("Sooner at the window", "The icon and the ear's summary no longer hold the window back; the log times the start."),
            better("The changes box glows", "The CHANGES box is lit until you have opened the list for a new release."),
            better("The privacy board knows more", "Backups, typed lines, window places and notifications, each with its key."),
            better("Voice clips on your word", "\"Stop keeping my voice clips\" and \"keep my voice clips again\"."),
            better("Settings search in German", "\"Nachtmodus\", \"Hintergrund\", \"Stimme\" and \"Lautstärke\" find their rows."),
            better("Settings kept before an update", "The settings file is copied before a new version changes it."),
            better("The tray says who runs", "The menu's first line shows the version and since when he has been running."),
            better("Guest mode says when it ends", "A note on the board when the guests' time is up."),
            better("German everyday orders by sound", "\"Wie spät ist es\", \"nächstes Lied\" and others are in his phrase book from the start."),
            fixed("Live desktop background", "It was in the right place but never drawn on Windows 11's newer desktop."),
            fixed("Detaching a block", "Voice set-up, notifications, settings and text blocks closed instead of detaching."),
            fixed("A window detached a second time", "It could stay hidden for good; it shows itself again now."),
        ],
    },
    Release {
        date: "2026-11-12",
        name: "Look and feel",
        entries: &[
            added("Start-up sequence and idle screen", "He powers up in steps and dims to the clock when you are away."),
            added("Night mode", "Dim and quiet from ten to six, or on your word."),
            added("Mini mode", "The slim strip at the top of the screen instead of the full board."),
            new("Live desktop background", "The emblem, the clock and what he is doing behind your desktop icons.", "desktop_background"),
            added("Music visuals and lyrics", "The emblem moves with your music; the lyrics run along with the song."),
            added("Live subtitles", "What a video or a call says, as a bar over your screen, also in English."),
            new("Voice set-up and voice lock", "Six lines to learn your voice; then he answers to you alone.", "voice_lock"),
            added("Settings in one place", "One searchable panel for everything he can be set to."),
            added("Notifications and edge light", "What he showed while you were not looking, and a light at the screen's edge."),
            better("Blocks", "A plain frame, a lettered title, quiet keys; they snap into place."),
            fixed("Briefing after a long absence", "It was shown and never read out."),
            fixed("\"Close the windows\"", "It ignored the board's own blocks."),
            fixed("Music visuals", "They listened to the wrong output and stopped in quiet passages."),
        ],
    },
    Release {
        date: "2026-11-11",
        name: "J.A.I.",
        entries: &[
            better("A new name", "He is J.A.I. (\"Jay\") everywhere: files, settings, voice."),
            better("His name in the recogniser", "The short name is taken apart from the order it gets glued to."),
        ],
    },
    Release {
        date: "2026-11-09",
        name: "Earth, 3D and pictures",
        entries: &[
            added("The Earth on the graphics card", "Live layers, cameras, street view and satellites on a real globe."),
            added("3D studio and real models", "Thousands of real 3D models, and a model made from a photo."),
            added("Picture blocks", "Up to six pictures side by side, each detachable."),
            added("Arrange his windows by voice", "\"Move it to my second monitor\", \"make it bigger\"."),
            better("His voice", "The speech model gets the sounds it was trained on: clearer words."),
            better("Hearing in noise", "A wider listening window when people talk around you."),
            fixed("Detached windows", "They are owned by the board and no longer hide behind it."),
        ],
    },
    Release {
        date: "2026-11-08",
        name: "Reference panels and Away Mode",
        entries: &[
            added("Radar, disk map and Earth panels", "The reference panels with their entrance and exit."),
            added("Away Mode", "He watches while you are gone and tells you what you missed."),
            added("Tell me when", "\"Tell me when the download is done\", \"when Discord closes\"."),
            added("His own Discord account", "He can sit in a voice channel of your server and answer there."),
            better("Tests beside your work", "Test windows stay on the second monitor and never take the focus."),
        ],
    },
    Release {
        date: "2026-11-07",
        name: "Questions only read",
        entries: &[
            better("A plain question never acts", "\"Are you in orb mode?\" answers; it no longer switches it on."),
            better("Misheard commands", "More of what the recogniser writes for your orders is put right."),
        ],
    },
    Release {
        date: "2026-11-06",
        name: "Reflexes, mixer and the verifier",
        entries: &[
            added("Reflex router", "Everyday orders are carried out in a blink, without the language model."),
            added("Butler cards and the suggestion bar", "He offers what fits the moment; one click carries it out."),
            added("The exhaustive verifier", "Thousands of probes of himself before a build is packaged."),
            better("The mixer", "Rewritten: every app's own volume, on the device it really plays on."),
            better("Home screen across monitors", "The board spans your screens cleanly."),
        ],
    },
    Release {
        date: "2026-11-04",
        name: "Fixes from your screenshots",
        entries: &[
            fixed("\"... to Jarvis\" eaten", "The name stripper took words that belonged to the order."),
            fixed("The board", "Dragging, scrolling and paging of blocks."),
            better("Empty replies", "He asks the model again instead of saying nothing."),
        ],
    },
    Release {
        date: "2026-11-03",
        name: "Themes, Discord and the model's window",
        entries: &[
            added("Theme Studio", "Make your own theme; ready-made ones in neon and OLED."),
            added("Discord bridge", "He hears a call and can speak into it."),
            added("Feature Lab", "Every feature with a button to try it."),
            added("Gears", "Reflex for the quick, deep for the hard: the model thinks only when it must."),
            fixed("Cut-off prompts", "The prompt was longer than the model's window; replies came from half of it."),
        ],
    },
    Release {
        date: "2026-11-02",
        name: "The board",
        entries: &[
            added("The fullscreen board and the ring emblem", "The chat window became a canvas of blocks around the emblem."),
            added("Zero-VRAM standby", "He lets the graphics card go when you play, and is back on Alt+J."),
            added("Deep research", "Missions that read for hours and report on the board."),
            added("Tool Forge", "He builds himself small tools for orders he could not carry out."),
            added("The core view", "Every module he runs on, wired as it runs."),
            better("Grounded answers", "He acts on what you said, not on examples from his own instructions."),
            fixed("Silent hang after standby", "A cold model made him say nothing; he answers within 25 seconds now."),
        ],
    },
    Release {
        date: "2026-10-25",
        name: "Reflexes and volume",
        entries: &[
            added("Typo and mishearing repairs", "Slips of the keyboard and the recogniser are read as you meant them."),
            better("Volume in layers", "His voice, the music and the system, each on its own."),
            better("PC power", "Shutting down asks twice."),
            fixed("Calendar entries at 15h", "An hour written as \"15h\" landed at the wrong time."),
        ],
    },
    Release {
        date: "2026-10-24",
        name: "Voice input",
        entries: &[
            fixed("Voice input", "The name prompt, the beam for sentences and stitching across breath pauses."),
            better("German", "German is only taken for German when the recogniser is sure."),
        ],
    },
    Release {
        date: "2026-10-23",
        name: "A new voice and a quieter machine",
        entries: &[
            added("The film voice", "A new speech model and a voice shaped after the film's."),
            added("Silent commands", "Routine orders are carried out without a word."),
            added("Native replay", "\"Clip that\" saves the last seconds of your screen."),
            added("Pure orb mode", "Only the orb, anywhere on your screens."),
            better("Clean speech", "Nothing that is not meant to be spoken reaches his voice."),
        ],
    },
    Release {
        date: "2026-10-22",
        name: "Field systems",
        entries: &[
            added("Phone link", "Your phone as his satellite: chat, hold to talk, your position."),
            added("Network radar and simulation lab", "Who is on your network; physics and maths you can watch."),
            added("Fifteen field systems", "Price sniper, folder cleanup, oral exam, Minecraft doctor and more."),
        ],
    },
    Release {
        date: "2026-10-21",
        name: "School and mail",
        entries: &[added("Timetable, homework, mail and calendar", "WebUntis, itslearning, your mail and your calendar in one place.")],
    },
];

/// The explanations, by id.
pub const EXPLANATIONS: &[Explanation] = &[
    Explanation {
        id: "dictation",
        title: "Dictation",
        what: "He types what you say into whatever app has the keyboard: a chat, a document, a search field. German and English both work, with the commas and full stops the recogniser hears. Each sentence is typed when you finish it.",
        say: &["Jay, dictate.", "new line   ·   new paragraph   ·   delete that", "stop dictating   ·   stop"],
        know: "Click into the app first: nothing is typed into his own window. While dictation is on, what you say is not an order, and the microphone box at the top says MIC: DICTATING (a click on it ends it too). Only your voice is typed. A new line is a soft one (Shift+Enter), so a chat does not send half a message. It ends by itself five minutes after the last typed sentence.",
    },
    Explanation {
        id: "coach",
        title: "Pronunciation coach",
        what: "He shows a sentence with the sounds that are hard for a German mouth in English. You read it aloud; he shows every word in green when it reached him as written, or in amber with what reached him instead.",
        say: &["Jay, practise my pronunciation.", "next   ·   [ HEAR IT ] has him say the sentence", "stop the coach"],
        know: "It shows what a machine heard, not how a person would hear you: a word in amber is a word worth saying once more, nothing worse. The coach only shows: to teach him a word, put its line right in the hearing log or say \"when I say … I mean …\". What is no reading of the sentence (an order, talk in the room) goes on as usual.",
    },
    Explanation {
        id: "hearing_log",
        title: "Hearing log",
        what: "The last forty things he heard from you: what arrived, what he took it for, whether it was an order he knew, and how like your voice it was. Click a line, type what you meant, and he learns it at once: the same words are read right from then on.",
        say: &["Jay, show the hearing log.", "Jay, what did you just hear?   (he says the last thing and what he took it for)"],
        know: "It reads from the clips of your voice he keeps on this PC. \"Delete my voice clips\" empties it.",
    },
    Explanation {
        id: "keys",
        title: "Hands-free keys",
        what: "He presses the keys of the app in front for you: tabs, scrolling, reloading, going back and forward, searching the page.",
        say: &["next tab   ·   previous tab   ·   new tab   ·   close the tab   ·   reopen the closed tab", "scroll down   ·   scroll up a bit   ·   go to the top   ·   go to the bottom", "page back   ·   page forward   ·   reload the page", "search the page for photosynthesis", "select all   ·   paste it   ·   press enter"],
        know: "The keys go to the window you were working in, never to him. Because people say \"press enter\" and \"paste it\" to each other too, these orders need his name (or an open conversation with him).",
    },
    Explanation {
        id: "brightness",
        title: "Monitor brightness",
        what: "He sets the real brightness of your monitors over their own control channel (DDC/CI): the same thing the buttons on the monitor do, not a dark veil over the picture.",
        say: &["brightness 40 percent", "dim the side monitors", "set monitor 2 to 30 percent", "make the screens brighter", "how bright are my monitors?"],
        know: "A monitor that does not answer has its control channel switched off in its own menu (look for DDC/CI). \"Undo that\" puts the brightness back.",
    },
    Explanation {
        id: "layouts",
        title: "Desk layouts",
        what: "He remembers where every window stands, on which monitor and how large, under a name, with his own windows and the music that played. Later the name brings them all back; an app that is closed is started first.",
        say: &["save this layout as study", "study layout   ·   restore the study layout", "list my layouts", "delete the study layout"],
        know: "One window per program is remembered. Minimised windows are left out. A layout is also a protocol: \"engage the study protocol\" does the same.",
    },
    Explanation {
        id: "rules",
        title: "Whenever-rules",
        what: "Your own automatic rules: when a program starts or closes, or at a time of day, he carries out an order as if you had just given it. Anything he can do on your word, a rule can do.",
        say: &["whenever Minecraft starts, set the music to 20 percent", "when Discord closes, unmute the music", "every day at 7:30 open my calendar", "list my rules   ·   delete rule 2"],
        know: "A rule is only kept when its order is one he knows. Rules fire on a change: a program that is already running when he starts does not count as started.",
    },
    Explanation {
        id: "sleep_timer",
        title: "Sleep timer",
        what: "When the time is up the volume sinks for a few seconds, every player is paused, and the volume stands again where it stood.",
        say: &["stop the music in 30 minutes", "sleep timer 20 minutes", "how long is left on the sleep timer?", "cancel the sleep timer"],
        know: "A new timer replaces the one that runs.",
    },
    Explanation {
        id: "notebook",
        title: "Notebook",
        what: "Notes by voice or keyboard, each with its day and time, newest first. He finds them again by their words.",
        say: &["note: maths test on Friday, chapters 3 and 4", "show my notes", "what did I note about the test?", "delete the last note"],
        know: "Notes stay on this PC, in his memory database. The privacy board deletes them all.",
    },
    Explanation {
        id: "read_aloud",
        title: "Read aloud",
        what: "Select text in any app and he reads it to you, about a minute at a time.",
        say: &["read this to me", "read my clipboard", "read on", "stop reading   ·   stop"],
        know: "He copies the selection to read it and puts your clipboard back as it was.",
    },
    Explanation {
        id: "explain_selection",
        title: "Explain selection",
        what: "Select text anywhere (a paragraph of a website, an error message, a task) and ask him about it. He reads the selection and answers about exactly that, from the text alone: nothing in it can make him do anything.",
        say: &["explain this", "translate this", "summarise this", "what does this mean?"],
        know: "With nothing selected he says so instead of guessing. \"Explain that\" and \"what does that mean\" are about what was just said, not about the selection: say \"this\", or \"explain the selection\".",
    },
    Explanation {
        id: "recap",
        title: "Evening recap",
        what: "The day in a few lines: how long you were at the screen and in which apps, how many orders you gave him, what you noted, and what is set for tomorrow.",
        say: &["what did I do today?", "recap my day"],
        know: "Screen time is counted while you are at the PC; two minutes without input count as away.",
    },
    Explanation {
        id: "doctor",
        title: "Internet doctor",
        what: "He tests the way to the internet one stretch at a time: this PC to your router, the router to the internet, the lookup of names, and a website. Then he names the first stretch that fails, in plain words, and what to try.",
        say: &["why is my internet so slow?", "internet doctor", "what is wrong with my connection?"],
        know: "It takes about ten seconds. \"Am I online?\" is the quick check.",
    },
    Explanation {
        id: "privacy",
        title: "Privacy board",
        what: "Everything he keeps about you on this PC in one block: your voice print, the clips of your voice, guests' voices, what he learnt of your words, notes, snippets, rules, the lines you typed, the notification history, where your windows stood, the backups, and how large his memory and his log are. Each has a key that deletes it; the key asks once more before it does. A desk layout is deleted by its name.",
        say: &["privacy board", "what do you keep about me?"],
        know: "None of it is ever sent anywhere.",
    },
    Explanation {
        id: "guests",
        title: "Known guests",
        what: "With the voice lock on he answers to you alone. A guest you introduce says three sentences in his or her own voice (a sentence in another voice is left out), and from then on he knows that voice by name. You decide what a guest may do: everything, or only the music and the volume.",
        say: &["this is my friend Tom, remember his voice", "Tom may only control the music   ·   Tom may do everything", "who do you know?   ·   who is speaking?", "forget Tom's voice"],
        know: "For someone who is only here today, \"guest mode\" is quicker: he answers to anyone for ten minutes, or for as long as you say.",
    },
    Explanation {
        id: "dashboard",
        title: "Portrait dashboard",
        what: "On your upright monitor, behind your windows and your desktop icons: the emblem and the clock, the song that plays, your newest notes, the day's screen time, the next reminders and whom he answers to.",
        say: &["portrait dashboard", "portrait dashboard off"],
        know: "It is your desktop's background there: a window in front covers it.",
    },
    Explanation {
        id: "snippets",
        title: "Snippets",
        what: "Pieces of text you need again and again (an address, a greeting, a link) under a name. He types them into the app in front.",
        say: &["save my clipboard as snippet address", "save the snippet greeting: Kind regards, ...", "type my address", "list my snippets   ·   delete the snippet address"],
        know: "Copy the text and save the clipboard: that keeps every letter as it is. Do not keep passwords here.",
    },
    Explanation {
        id: "changes",
        title: "The changes list",
        what: "Every change to him by day: what is new, what got better, what was put right. Search it, or show one kind only. \"HOW IT WORKS\" opens a feature's explanation in a window of its own.",
        say: &["show the changes", "what has changed about you?", "the [ CHANGES ] box at the top of the board (lit while there is a release you have not looked at)"],
        know: "A feature's explanation can also be asked for by name: \"explain the sleep timer\".",
    },
    Explanation {
        id: "voice_lock",
        title: "Voice lock",
        what: "He answers to your voice alone. The print of your voice comes from the voice set-up, or is gathered from the orders you give him by name, and it grows with every full sentence that is surely yours and like the voice of the set-up: your everyday voice, a cold, another microphone. For half a minute after such a sentence, one that falls a little short still passes (and teaches nothing).",
        say: &["learn my voice", "only obey me", "listen to everyone", "guest mode   ·   let my friend speak for half an hour"],
        know: "The keyboard and push-to-talk are always yours. If the lock ever cannot judge a voice, the order is carried out and the log says so: a fault never locks you out. Three orders by name that fall just short of his bar bring it back down to where the set-up put it. If he turns you away often, say \"learn my voice\" once more: one minute of reading.",
    },
    Explanation {
        id: "ear",
        title: "His ear for your words",
        what: "The recogniser writes accented words as other words, and the same ones every time: \"this card\" for Discord, \"sum out\" for zoom out. Before he decides anything he puts them right: from what you taught him, from what he learnt, by the sound of an app's name where a name belongs, and from the orders you have given before. An order that still leads nowhere is listened to a second time, slower and wider.",
        say: &["when I say this card I mean Discord", "no, I said open the mixer", "that's not what I said", "what have you learnt about my pronunciation?", "forget how I speak"],
        know: "An app's name is read by sound after \"open\", \"start\", \"switch to\" and \"mute\" only, and never where the order would close or delete something: teach him those (\"when I say this card I mean Discord\"). A second hearing only stands when it still sounds like the first. \"That's not what I said\" takes the last rewrite back, and he does not make it again.",
    },
    Explanation {
        id: "backups",
        title: "Nightly backups",
        what: "Once a day, a few minutes after a start or when the date turns, a whole copy of his memory database and of the settings file goes into the folder backups beside the database. The copy is checked before it takes its name. Seven days are kept; the oldest leaves when the eighth comes.",
        say: &["back up your memory", "when was the last backup?"],
        know: "To go back to a day: close him, copy that day's jai_memory file over the memory database, start him again. The backups stay on this PC; the privacy board can delete them.",
    },
    Explanation {
        id: "desktop_background",
        title: "Live desktop background",
        what: "He is your desktop's background: the board itself stands behind your icons, on every monitor or on your first, and looks as it does across all monitors. Every view of his, the console too, opens as a window of its own that you move like any other. Speak to him while a window covers his emblem, and the orb leaves the background for the lower left corner of the monitor you are using: smaller, in front of everything, with what is being said beside it. Five seconds after the last of it, it glides back. His keys on the background take your clicks; everything else on the desktop is yours as before.",
        say: &["background mode", "be my desktop background", "background on all monitors", "background only on the main monitor", "show your windows", "tidy the windows", "board mode"],
        know: "Ctrl+Alt+J brings his windows to the front, Ctrl+Alt+K the console to type to him. Drag the corner orb where you want it (he remembers the spot for each monitor); a click on it opens the console, a double click brings the board back. Beside a game he leaves the mouse alone. It sits in front of a wallpaper program's picture and behind everything else. Should Explorer start again, he opens his background anew by himself.",
    },
];

pub fn explanation(id: &str) -> Option<&'static Explanation> {
    EXPLANATIONS.iter().find(|e| e.id == id)
}

// ----------------------------------------------------------------------------- asking ----

static SHOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?:show|open)\s+(?:me\s+)?(?:the\s+|your\s+)?(?:changes|change\s*log|changelog|release\s+notes|list\s+of\s+changes|update\s+history)|what\s+(?:has\s+)?changed\s+(?:in|about|with)\s+you|what\s+is\s+new\s+(?:in|about|with)\s+you|(?:the\s+)?changes\s+list|change\s*log|was\s+hat\s+sich\s+geändert|zeig\s+(?:mir\s+)?die\s+änderungen)$").expect("a valid phrase")
});
static EXPLAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:explain|how\s+does|how\s+do\s+i\s+use|tell\s+me\s+about|erkläre?(?:\s+mir)?)\s+(?:the\s+|your\s+|my\s+|a\s+|den\s+|die\s+|das\s+)?(.+?)(?:\s+(?:work|works|feature|funktion))?$").expect("a valid phrase")
});

/// The explanation Sir names ("the sleep timer", "dictation"), by its title or id.
fn named(what: &str) -> Option<&'static Explanation> {
    let wanted = crate::ear::words(what).join(" ");
    EXPLANATIONS.iter().find(|e| {
        let title = crate::ear::words(e.title).join(" ");
        // (By its title as the list shows it, nothing shorter: "how does the ear work" and
        // "what is a monitor" are questions about the world.)
        wanted == title || wanted == format!("{title}s") || format!("{wanted}s") == title
    })
}

/// "Show the changes", "what has changed", "explain the sleep timer". ("What's new" stays the
/// older order that says what this build does; its answer names this list.)
pub fn command(s: &str) -> Option<Value> {
    if SHOW.is_match(s) {
        return Some(json!({"action": "show"}));
    }
    let what = EXPLAIN.captures(s)?.get(1)?.as_str();
    named(what).map(|e| json!({"action": "show", "explain": e.id}))
}

pub fn run(args: &Value) -> Result<String, String> {
    if let Some(e) = args["explain"].as_str().and_then(explanation) {
        open_explanation(e.id);
        return Ok(say(format!("{}\n\n{}\n\nSay: {}\n\n{}", e.title, e.what, e.say.join("\n     "), e.know), format!("{}, Sir. In its own window.", e.title)));
    }
    open(true);
    let latest = &RELEASES[0];
    let news = latest.entries.iter().filter(|e| e.kind == Kind::New).count();
    Ok(say(format!("The changes list is on the board: {} days of work, the latest on {} ({} new features).", RELEASES.len(), latest.date, news), format!("Everything that changed, Sir, by day. The latest brought {news} new features.")))
}

// -------------------------------------------------------------------------- the block ----

pub const ID: &str = "changes";
static OPEN: AtomicBool = AtomicBool::new(false);

struct Ui {
    search: String,
    /// The kind shown (None: all).
    only: Option<Kind>,
    /// The explanations that are open, each in a window of its own.
    explaining: Vec<&'static str>,
}

static UI: Mutex<Ui> = Mutex::new(Ui { search: String::new(), only: None, explaining: Vec::new() });

pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// The newest release Sir has opened this list for ("2026-11-13"), read from the kit's store
/// once. None: not read yet.
static SEEN: Mutex<Option<String>> = Mutex::new(None);

/// Whether there is a release Sir has not opened the list for yet: the header's box glows
/// until he does.
pub fn unseen() -> bool {
    let newest = RELEASES.first().map_or("", |r| r.date);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let seen = seen.get_or_insert_with(|| crate::kit::get("changes", "seen").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default());
    seen.as_str() < newest
}

fn mark_seen() {
    let newest = RELEASES.first().map_or("", |r| r.date).to_string();
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.as_deref() != Some(newest.as_str()) {
        *seen = Some(newest.clone());
        let _ = crate::kit::put("changes", "seen", &serde_json::json!(newest));
    }
}

pub fn open(on: bool) {
    OPEN.store(on, Ordering::Release);
    if on {
        mark_seen();
        crate::hud_dock::cancel_close(ID);
    } else {
        crate::hud_dock::forget(ID);
    }
    crate::gui::refresh();
}

pub fn toggle() {
    open(!is_open());
}

fn explain_id(id: &str) -> String {
    format!("explain:{id}")
}

/// Opens a feature's explanation in a window of its own.
pub fn open_explanation(id: &'static str) {
    let mut ui = lock(&UI);
    if !ui.explaining.contains(&id) {
        ui.explaining.push(id);
        // (Born as a window of its own: "in a new window".)
        crate::hud_dock::detach_next(&explain_id(id));
    }
    drop(ui);
    crate::gui::refresh();
}

/// The entries of a release that match the search words and the kind.
pub fn matching<'a>(release: &'a Release, search: &str, only: Option<Kind>) -> Vec<&'a Entry> {
    let words = crate::ear::words(search);
    release
        .entries
        .iter()
        .filter(|e| only.is_none_or(|k| e.kind == k))
        .filter(|e| {
            let text = format!("{} {} {} {}", e.title, e.what, release.name, release.date).to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        })
        .collect()
}

fn day_words(date: &str) -> String {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map(|d| d.format("%-d %B %Y").to_string().to_uppercase()).unwrap_or_else(|_| date.to_string())
}

/// Draws the changes block and the explanation windows that are open.
pub fn show(ctx: &egui::Context) {
    let accent = crate::hud_board::active_colour();
    let ink = Color32::from_rgb(0xE8, 0xF2, 0xFC);
    let muted = Color32::from_rgb(0x8E, 0xA4, 0xB8);
    if is_open() {
        let (mut search, mut only) = {
            let ui = lock(&UI);
            (ui.search.clone(), ui.only)
        };
        let mut asked: Option<&'static str> = None;
        let closed = crate::gui::block(ctx, ID, "Changes // what is new, better, fixed", [700.0, 640.0], |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("FIND").font(FontId::monospace(10.0)).color(muted));
                ui.add(egui::TextEdit::singleline(&mut search).desired_width(220.0).hint_text("a word, a date").font(FontId::proportional(13.5)));
                ui.add_space(10.0);
                for (label, kind) in [("ALL", None), ("NEW", Some(Kind::New)), ("BETTER", Some(Kind::Better)), ("FIXED", Some(Kind::Fixed))] {
                    let lit = only == kind;
                    if ui.add(egui::Label::new(RichText::new(format!("[ {label} ]")).font(FontId::monospace(10.5)).color(if lit { accent } else { muted })).sense(Sense::click())).clicked() {
                        only = kind;
                    }
                }
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                let mut shown = 0;
                for release in RELEASES {
                    let entries = matching(release, &search, only);
                    if entries.is_empty() {
                        continue;
                    }
                    shown += entries.len();
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(day_words(release.date)).font(FontId::monospace(10.5)).color(accent));
                        ui.label(RichText::new(release.name).font(FontId::new(17.0, FontFamily::Name("display".into()))).color(ink));
                    });
                    let rule = ui.cursor();
                    ui.painter().line_segment([rule.left_top(), egui::pos2(rule.left() + ui.available_width(), rule.top())], egui::Stroke::new(1.0f32, crate::hud_style::white(0.10)));
                    ui.add_space(5.0);
                    for entry in entries {
                        let tag_colour = match entry.kind {
                            Kind::New => accent,
                            Kind::Better => crate::hud_style::GOOD,
                            Kind::Fixed => muted,
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.add_sized([54.0, 16.0], egui::Label::new(RichText::new(entry.kind.tag()).font(FontId::monospace(9.5)).color(tag_colour)).halign(egui::Align::LEFT));
                            ui.label(RichText::new(entry.title).color(ink).size(14.0).family(FontFamily::Name("label".into())));
                            if let Some(id) = entry.explain {
                                let link = ui.add(egui::Label::new(RichText::new("HOW IT WORKS").font(FontId::monospace(9.5)).color(accent).underline()).sense(Sense::click())).on_hover_text("Opens the explanation in a window of its own");
                                if link.clicked() {
                                    asked = Some(id);
                                }
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(62.0);
                            ui.label(RichText::new(entry.what).color(muted).size(12.5));
                        });
                        ui.add_space(3.0);
                    }
                }
                if shown == 0 {
                    ui.add_space(20.0);
                    ui.label(RichText::new("Nothing in the list matches that.").color(muted).size(13.5));
                }
                ui.add_space(10.0);
            });
        });
        {
            let mut ui = lock(&UI);
            (ui.search, ui.only) = (search, only);
        }
        if let Some(id) = asked {
            open_explanation(id);
        }
        if closed {
            open(false);
        }
    }
    // The explanations: each a block born as a window of its own.
    let explaining = lock(&UI).explaining.clone();
    for id in explaining {
        let Some(e) = explanation(id) else { continue };
        let block = explain_id(id);
        let closed = crate::gui::block(ctx, &block, &format!("{} // how it works", e.title), [520.0, 460.0], |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new(e.title).font(FontId::new(24.0, FontFamily::Name("display".into()))).color(ink));
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(e.what).color(ink).size(14.0)).wrap());
                ui.add_space(14.0);
                ui.label(RichText::new("WHAT TO SAY").font(FontId::monospace(10.0)).color(accent));
                ui.add_space(4.0);
                for line in e.say {
                    ui.add(egui::Label::new(RichText::new(*line).font(FontId::monospace(12.5)).color(ink)).wrap());
                    ui.add_space(2.0);
                }
                ui.add_space(12.0);
                ui.label(RichText::new("GOOD TO KNOW").font(FontId::monospace(10.0)).color(accent));
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(e.know).color(muted).size(13.0)).wrap());
                ui.add_space(8.0);
            });
        });
        if closed {
            lock(&UI).explaining.retain(|open| *open != id);
            crate::hud_dock::forget(&block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_link_leads_to_an_explanation_and_every_explanation_is_linked() {
        let linked: Vec<&str> = RELEASES.iter().flat_map(|r| r.entries.iter().filter_map(|e| e.explain)).collect();
        for id in &linked {
            assert!(explanation(id).is_some(), "'{id}' is linked and not explained");
        }
        for e in EXPLANATIONS {
            assert!(linked.contains(&e.id), "'{}' is explained and nowhere linked", e.id);
            assert!(!e.what.is_empty() && !e.say.is_empty() && !e.know.is_empty() && e.what.ends_with('.') && e.know.ends_with('.'), "{}", e.id);
        }
        let mut ids: Vec<&str> = EXPLANATIONS.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), EXPLANATIONS.len(), "an explanation twice");
    }

    #[test]
    fn the_list_is_by_day_newest_first_and_every_line_is_one_line() {
        assert!(RELEASES.windows(2).all(|w| w[0].date > w[1].date), "newest first, no day twice");
        for r in RELEASES {
            assert!(chrono::NaiveDate::parse_from_str(r.date, "%Y-%m-%d").is_ok(), "{}", r.date);
            assert!(!r.entries.is_empty() && !r.name.is_empty());
            for e in r.entries {
                assert!(e.what.ends_with('.') || e.what.ends_with('"') || e.what.ends_with("\".\""), "{}: {}", e.title, e.what);
                assert!(e.what.chars().count() <= 96 && e.title.chars().count() <= 44, "{}: one line", e.title);
            }
        }
        assert_eq!(day_words("2026-11-13"), "13 NOVEMBER 2026");
    }

    #[test]
    fn the_list_is_searched_and_filtered_and_asked_for_by_name() {
        let latest = RELEASES.iter().find(|r| r.date == "2026-11-13").expect("that day's release");
        assert!(matching(latest, "", None).len() == latest.entries.len());
        assert_eq!(matching(latest, "sleep timer", None).iter().map(|e| e.title).collect::<Vec<_>>(), ["Sleep timer", "Phone quick buttons"]);
        assert!(matching(latest, "", Some(Kind::Fixed)).iter().all(|e| e.kind == Kind::Fixed) && !matching(latest, "", Some(Kind::Fixed)).is_empty());
        assert!(matching(latest, "no such thing anywhere", None).is_empty());
        assert!(matching(latest, "2026-11-13 dictation", None).len() == 1, "a date finds its day");
        assert_eq!(command("show the changes"), Some(json!({"action": "show"})));
        assert_eq!(command("what has changed about you"), Some(json!({"action": "show"})));
        // (Questions about the world and about what just happened are not about this list.)
        for said in ["what has changed", "what changed", "what is a monitor", "how does the ear work", "what is the internet", "explain the rules of chess"] {
            assert_eq!(command(said), None, "{said}");
        }
        assert_eq!(command("explain the sleep timer"), Some(json!({"action": "show", "explain": "sleep_timer"})));
        assert_eq!(command("how does dictation work"), Some(json!({"action": "show", "explain": "dictation"})));
        assert_eq!(command("tell me about the privacy board"), Some(json!({"action": "show", "explain": "privacy"})));
        for said in ["explain photosynthesis", "what is the time", "how does a car work", "show the weather"] {
            assert_eq!(command(said), None, "{said}");
        }
    }
}
