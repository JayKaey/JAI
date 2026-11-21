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
use crate::holo::{font as face, Face};
use eframe::egui::{self, RichText, Sense};
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
        date: "2026-11-21",
        name: "Keys, ears and control",
        entries: &[
            new("Hotkeys", "\"Open the hotkeys window\": every key of his, in any program or game; SET, then the new key.", "hotkeys"),
            added("A key for the microphone", "Your microphone on and off by a key of your choice, in any program or game, with a sound."),
            better("Straight answers", "Touchy questions, rough words and research get a straight, full answer: no lectures or dodging."),
            added("Corrections and stutters", "\"Open Chrome, uh, I mean Firefox\" opens Firefox; \"Open the… never mind\" is taken back."),
            new("Sonora, at once", "Your songs and playlists, private or not downloaded, start at once; the queue and more by voice.", "sonora"),
            better("He waits for the rest", "A sentence that stops on \"and\" or \"no wait\" gets a person's pause; half an order never runs."),
        ],
    },
    Release {
        date: "2026-11-20",
        name: "Accounts, other PCs and Linux",
        entries: &[
            new("J.A.I. on Linux", "The same program built for Linux: his ears, voice, brain, HUD, music, volume and windows.", "linux"),
            added("Window orders on KDE Plasma", "Focus, minimise, full screen, snap, tile and move windows between monitors work on KDE Plasma."),
            added("The desktop background on KDE", "Background mode and the corner orb work on KDE: below every window, on every virtual desktop."),
            added("The tray icon on KDE", "His icon sits in KDE's system tray with the same menu as on Windows."),
            added("Music and media keys over MPRIS", "Music and media keys on Linux talk to Spotify, VLC, browsers over MPRIS; searches play too."),
            added("Volume of single apps on Linux", "The volume mixer on Linux sets single apps and the microphone through PipeWire."),
            added("Reading the screen on KDE", "Screen reading, OCR and \"copy the text\" work on KDE through KWin screenshots and tesseract."),
            added("Webcam, replay and brightness on KDE", "The webcam, instant replay, monitor brightness and the clipboard work on KDE."),
            added("Typing and keys on KDE", "Typing, hands-free keys and the browser's address work on KDE through Plasma's fake input."),
            added("Disk map and radar on Linux", "The disk map, network radar and guardian read this Linux PC: disks, trash, ARP, pings."),
            added("Subtitles, widgets and more on KDE", "Live subtitles, ghost mode, desk widgets, the clipboard history and shortcuts work on KDE."),
            added("One file installs him", "One file each for Windows and Linux, on GitHub and your phone page: a new PC or an update."),
            added("Start with Windows", "Say \"start with Windows\", or tick it in the settings or the tray: he starts when you log in."),
            better("Installing on another PC", "A new PC downloads nothing from yours: J.A.I. and his engine come from GitHub."),
            better("Power and lock on Linux", "On Linux, shut down, restart, lock and sleep go through logind and KDE's own lock screen."),
            better("PC questions on Linux", "On Linux he finds frozen windows, checks the mic, lists pacman updates, opens Discover."),
            better("Games and the engine on Linux", "On Linux the language engine dies with J.A.I., and Steam games and video memory are found."),
            better("Apps, position and files on KDE", "Apps start from their desktop entries; GeoClue gives your position and Dolphin shows files."),
            better("KDE checked against a real KWin", "Every update is checked against a real KWin on Arch: windows, keys, quick tiles, his background."),
            fixed("Tiling on the monitor you name", "\"Tile on monitor 2\" tiled the third monitor or found none; it tiles the one you name now."),
            fixed("The webcam on Linux", "The webcam never opened on Linux: its picture format was asked with a request of the wrong size."),
            fixed("Your microphone at login", "When your microphone came after J.A.I., he kept another; he switches to yours once it's there."),
            fixed("Building on Arch", "Built on Arch, whisper's bindings lost their fields; the README's command builds him now."),
            fixed("itslearning works again", "Schule@BW sign-in stopped on the wrong page; he reads your tasks and announcements again."),
            new("J.A.I. on other PCs", "Install him on a friend's PC: your PC stays the main one, sends orders and they can't touch it.", "other_pcs"),
            new("Accounts for everyone else", "Only your voice and phone may do everything; anyone else needs an account that only asks.", "accounts"),
            new("The accounts list", "\"Open the accounts list\": passwords, every permission one by one, pauses, topics, PCs.", "accounts"),
            new("The admin window", "\"Open the admin window\": every window he has, yours first; other PCs ask for your sign-in.", "admin"),
            added("Friends may do everything on their PC", "EVERYTHING on their PC: all you can do there, settings too; your PC and others stay yours."),
            better("Your PC has switches of its own", "What a friend may on your PC, by phone or voice, is chosen apart; at first only talking."),
            better("The accounts list, laid out", "Each account: their PC, your PC, topics, PCs, what he did; features in the Lab's groups."),
            new("Always your desktop", "He is your desktop's background on every PC from the first start, never a window of his own.", "desktop_background"),
            better("The floating orb over your desktop", "Orb mode and mini mode float in front of your work while he stays the desktop behind it."),
            fixed("The desktop is asked again", "When the desktop would not take him, he became a window for good; he asks again until it does."),
            fixed("SCREENS as the desktop", "SCREENS moved his background like a window; now it spans your monitors or moves his emblem."),
            fixed("Hiding no longer loses him", "\"Hide\" or the close key on his background left the desktop without him; he stays now."),
            fixed("No freeze as a card shrinks away", "A card squeezed to nothing as a window closed stopped his HUD for seconds; no longer."),
            new("\"Who am I speaking with?\"", "A voice he has never heard is asked its name and learnt; it may ask questions, nothing more.", "unknown_voice"),
            added("Each phone knows its own place", "\"Where am I\" on a friend's phone answers with where that phone is, not where you are."),
            added("The phone page wears a theme", "Your PC's theme, or one picked under LOOK on the phone itself; your saved themes too."),
            better("A permanent address for the phone", "With Tailscale on this PC the phone's address is the same forever, for free."),
            fixed("The permanent address comes back", "When Tailscale Funnel failed at start, he tries it again every five minutes and switches back."),
            better("As many custom themes as you like", "A theme made without a name no longer replaces the last one: it is \"Custom 2\", and on."),
            fixed("The phone page's rough edges", "Its folds, ticks, lists and number fields looked like a browser's; now they wear his look."),
            fixed("Widget windows say CLOSE", "A widget in a window of its own showed POP OUT and REMOVE; it has one CLOSE key now."),
            fixed("No leader into empty space", "A closed or minimised window kept its line from the core; the line goes with the window now."),
            fixed("A crowded city turns smoothly", "With thousands of houses in view the Earth lagged; a frame of them costs about half what it did."),
            better("The Earth comes much closer", "The wheel and \"zoom in\" go down to 30 m, a house filling the window (was 250 m)."),
        ],
    },
    Release {
        date: "2026-11-19",
        name: "Every panel in the new look",
        entries: &[
            new("Landmarks on the Earth", "\"Show me the Statue of Liberty\" flies to it; a building stands among the houses in 3D.", "earth_landmarks"),
            better("The houses rise as holograms", "Traced on the ground, then risen in a wave from the middle, with windows and a tower's scale."),
            new("The Earth in 3D", "Near the ground the camera leans back over the town; the right button tilts and turns it.", "earth_3d"),
            added("Streets and water on the hologram map", "From a town's outline down: its streets by rank, rails, rivers, lakes and the streets' names."),
            fixed("No hidden Edge any more", "Signing in to itslearning started a hidden Edge at every briefing; he starts no browser now."),
            fixed("Windows opened together no longer stack", "Opened at once, they all came up where each last stood alone; a taken spot is skipped now."),
            fixed("No black pane behind a closing window", "As a window folds up or unfolds, what stands behind it shows through instead of a black pane."),
            new("The houses of a town in 3D", "Down at street level the Earth raises every house as a block model of light.", "earth_houses"),
            better("Flying to a town goes down to it", "\"Fly to Berlin\" ends with the town in view to its outline, not as a dot on a continent."),
            better("The hologram map is dark glass up close", "From a town down its land was one flat olive field; it sinks into the glass now."),
            new("He decides, and he has opinions", "\"Open a few random windows\" is his own pick; asked what he thinks, he says so.", "his_own_mind"),
            fixed("English unless you ask for German", "A question about Germany counted as asking for German and switched his language guard off."),
            fixed("Borders and cities on the hologram Earth", "NASA's label tiles come back empty now: names and borders are Esri's in both looks."),
            fixed("The QR window fits its window", "Its long link pushed every line out past both edges; it wraps now, and the view scrolls."),
            better("Every window is a real window", "Nothing he opens stands inside his own window any more, with the live desktop on or off."),
            better("Windows open beside the core", "A new window takes a place about the core: beside him, the next on the other side."),
            better("Leaders run to the real windows", "The board draws a line from the core's circle to each of his windows on its screen."),
            better("Windows wear the panel's header", "Index tab, title in wide capitals and state lamp, as a block on the board had."),
            better("No more dock and detach", "The keys and orders that moved a view on and off the board are gone: nothing docks."),
            better("The block shortcuts are gone", "Ctrl+W, Ctrl+G, Ctrl+P and Ctrl+E only acted on a block inside his own window."),
            fixed("The canvas is black again", "The film grain was four times too strong since 11 November: a grey fog over the black."),
            fixed("The clock stays off the core view", "The desktop clock was drawn over the sphere while the core view was open."),
            fixed("Feature Lab lines end where they should", "A description was cut inside \"e.g.\"; it ends at its first full sentence now."),
            better("Every panel in the new look", "Earth, radar, research, lab, 3D studio, pictures, mixer and the rest: light on black glass."),
            better("The disk map is panes of light", "A folder is faint glass in its file type's colour with a bright edge, not a solid block."),
            better("The Earth is dark glass", "The holographic globe is a dark ball with dots of light and lit coasts, not an olive one."),
            better("Block titles stay whole", "A narrow block lets its keys step back before its title is cut."),
            better("\"Show everything\" ends the calm mode", "The readouts at the top and bottom come back at once."),
            better("One settings panel", "Core, core size, opacity, name and window size are rows of the one panel: \"open settings\"."),
            better("Settings that did nothing are gone", "Panel hiding, zen mode, background colour, corner rounding and the old theme list."),
            better("A window in a theme of its own", "\"Make that window red\": that window alone wears the red theme, frame and content."),
            better("\"Darker\" and \"lighter\" pick a theme", "The canvas is always black: \"make it darker\" is Graphite, \"a bit lighter\" is Mono."),
            better("Old theme words still work", "\"Iron man\", \"stealth\", \"matrix\" and the like choose the nearest of the eight themes."),
            better("Light in every theme", "Windows open with their glow, and the display finish shows, in Mono and Graphite too."),
            fixed("\"Make the orb bigger\" works again", "The core grows and shrinks with a glide, by voice or in the settings panel."),
            fixed("\"The black and white theme\" is one order", "It was cut at its \"and\" into two look changes and answered twice."),
            added("\"Close the chat\", \"show the chat\"", "The conversation block above the bottom strip goes and comes by voice, never Discord."),
            fixed("Wrapped lines on this page", "A description that wraps stays under its own start, not under the tag."),
        ],
    },
    Release {
        date: "2026-11-18",
        name: "The hologram interface",
        entries: &[
            new("An interface drawn from scratch", "Light on black glass: white structure, your yellow for what is live, a real glow.", "hologram_interface"),
            new("Two cores", "Simple is an instrument of rings, complex the sphere of circuitry: \"switch to the complex core\".", "two_cores"),
            added("Black and white, and six more themes", "Mono, Arc Reactor, Hot Rod, Gamma, Vibranium, Ember, Graphite: \"the black and white theme\"."),
            better("Windows open about the core", "The first beside it at eye level, the next on the other side, later ones along its circle."),
            better("Windows are drawn, not shown", "A spark in the corner, a line of light, the frame unfolding from it, then its rows."),
            better("Numbered windows", "Every window carries an index tab in the order it was opened, as on a drawing."),
            better("The core is his clock", "Its scale ring ticks the seconds, an arc shows the minutes, a block the hour."),
            better("His voice and yours", "His own voice lights the core's inner ring in your yellow; yours shows as white ticks round it."),
            better("New lettering", "Wide capitals for labels, thin squared figures for readings, plain sentences for the rest."),
            better("A new icon", "The core in your yellow on a black disc."),
            better("A top rail of words", "The header is words on a hairline: what is open is a solid block of your yellow."),
            better("The rule answers his voice", "The filament under the rail widens and brightens while he speaks, and so does his first circle."),
            better("Windows tied to the core", "A leader runs from his circle to every open window; light runs out along it while he speaks."),
            better("Plates everywhere", "Board blocks, windows, cards and questions are one plate: cut corner, three ticks, an index tab."),
            better("New keys, switches and sliders", "Keys lean, switches slide, a slider is a hairline with a ruler; every panel uses them."),
            better("Charts of light", "Bars are faint panes under a bright cap, on a scale of hairlines; no more olive fills."),
            better("Notifications and the edge light", "Cards are plates with a chip; the screen's edge lights as a filament, not a painted frame."),
            better("The idle dial", "The time in thin figures inside a dial: the seconds as sixty marks, the minutes as an arc."),
            better("He draws himself at start-up", "A spark, the axes, then ring after ring of light, and the parts that are really up."),
            better("The phone page", "The same plates, leaning keys and lettering on your phone."),
            fixed("\"Shut off my PC\" asks first", "It closed an app named \"off my pc\"; now it asks to shut the PC down, also when misheard."),
        ],
    },
    Release {
        date: "2026-11-17",
        name: "The Stark look",
        entries: &[
            new("The Stark look", "Stark's workshop design in your yellow: hairline light, sharp frames, radial readouts.", "stark_look"),
            better("Room between the blocks", "Blocks on the board and in the window float apart instead of touching."),
            better("Finer lines", "Brackets, tabs, rings and chart lines are drawn as hairlines, as Stark draws light."),
            better("White titles, marked frames", "Every block is titled in white; a tab, a dash and a tick ruler mark its top edge."),
            better("Sharp panels everywhere", "Panels are cut glass: two corners cut off, fine scan lines, nothing rounded."),
            better("A workshop table", "Faint rings, a tick dial and a crosshair lie under the board, as on Stark's table."),
            better("Hologram chart bars", "Chart bars are light: a hairline edge, fine scan lines and a bright cap."),
            better("The core as a radial HUD", "The core sphere sits in a ring of ticks and turning arcs; its modules are rings of light."),
            better("A readout round the emblem", "The emblem's counter-rings, a turning outer readout and four bearings."),
            added("Stark Red theme", "The films' red with hologram blue and steel frames: \"change the theme to Stark\"."),
            fixed("No freeze on a click", "Opening a window of his own or a caption froze the whole HUD; both draw normally now."),
            fixed("Questions go on sooner", "Some questions were read twice on the fast path; now once, up to a millisecond sooner."),
        ],
    },
    Release {
        date: "2026-11-16",
        name: "A hundred small things",
        entries: &[
            added("Home, found and confirmed once", "He finds where you live himself and asks you once; your yes keeps it."),
            added("Sunrise and sunset", "The briefing and the desk clock tell when the sun rises or sets, worked out offline."),
            added("Rain against your day", "Rain is set against your school day: an umbrella when it falls as you leave."),
            added("Your schedule in English", "German lessons, appointments and itslearning titles are read out in English."),
            added("Briefing parts by voice", "\"Leave the news out of my briefing\", \"put the weather back in\", \"what's in my briefing?\""),
            added("A service that needs a hand", "A failing WebUntis, itslearning or email is told plainly, with a card and its fix."),
            fixed("The weather in your own town", "The briefing named a district near you; it now finds your town and asks Open-Meteo."),
            fixed("Close all windows closes the cards", "Widget cards close too, but not your lists or a running timer; \"thank you\" first works."),
            fixed("A bare yes closes nothing", "A \"yes\" when he asked nothing no longer closes or opens a program on a guess."),
            fixed("No orb while not listening", "Under \"stop listening\" the corner orb no longer flies out at every noise; it stays put."),
            better("A film look", "Hairline frames on whole pixels with corner brackets, and no halos round the blocks."),
            better("A calmer emblem", "Thinner rings, and a core whose light breathes slowly at rest."),
            better("A quieter canvas", "Less grain and dust on the glass, and no scratches."),
            better("A steady desk clock", "Its digits are even-width, so the time does not shift as the minutes turn."),
            added("Tomorrow at school", "\"What do I need tomorrow?\": the lessons in order, what to pack, and the exam's lesson."),
            added("Your homework, ticked off", "\"What homework is left?\"; \"maths homework done\" ticks it."),
            added("Points and averages", "\"I got 13 points in physics\": kept, with the average per subject and overall."),
            added("A study plan before each exam", "\"Make me a study plan\"; \"add my study plan to the calendar\"."),
            added("The week ahead", "\"What's my week like?\": exams, homework due, appointments and days off, Monday to Sunday."),
            added("School holidays", "In the holidays no school alarm and no bedtime nudge; the briefing says when school starts."),
            added("Is this equation right?", "\"Is x = 5 right for 3x + 5 = 20?\": checked, and solved step by step."),
            added("How do you spell it?", "\"How do you spell Straße?\": the word, letter by letter, in English or German letter names."),
            added("Where you put things", "\"My keys are in the drawer\"; later \"where did I put my keys?\" tells you, and when you said it."),
            added("Birthdays", "\"Max's birthday is on 3 March\": told the evening before and that morning, as are the calendar's."),
            added("What to wear", "\"Do I need a jacket?\": answered from the forecast for the hours you are out."),
            added("Named timers", "\"A 10 minute pizza timer\": each running timer gets its own ring round the orb, with its name."),
            added("Which app is frozen", "\"Is anything frozen?\" names it and ends it after your yes."),
            added("Who holds a port or a file", "\"Who is using port 8080?\", \"who has report.docx open?\"; ended on your yes."),
            added("What starts with Windows", "\"What starts with Windows?\"; \"stop Discord from starting with Windows\" after your yes."),
            added("App updates", "\"Are there updates for my apps?\" lists them; your yes installs them in the background."),
            added("My mic doesn't work", "Privacy switch, default microphone, mute, level and sound: the first thing at fault."),
            added("Your code, by voice", "\"Build my project\", \"what's the state of my project\", \"write me a commit message\"."),
            added("Games you don't play", "\"Which games haven't I played in months?\": biggest first, with the space they take."),
            added("Minecraft worlds backed up", "Once a week and on \"back up my worlds\": each instance's worlds, the last five copies kept."),
            added("Without internet", "The drop and the return are told; searches and the weather answer from their last copy."),
            added("A safe start", "A new build that fails to start twice gives way to the last one that ran well."),
            better("The part an app plays", "\"Open my editor\", \"pause the music app\": your own apps for each part."),
            better("Back at the PC", "After a while away he wakes up for you, not during a game."),
            better("Clashes in the calendar", "A new appointment that clashes with another one is told when it is set."),
            better("Lists, links and paths read aloud", "A pause after each list line; links as \"github dot com slash …\"; folders by name."),
            better("A quieter voice at night", "At night he speaks a little slower."),
            better("A drive running full", "Below a tenth free on a drive, told once per crossing."),
            better("Start-up problems first", "A missing model or a full drive is the first thing he says at the start."),
            better("Searches asked twice", "The same search within a quarter of an hour answers at once from what was found."),
            better("Blocks that open like windows", "Each block's title decodes as it opens, the same entrance his windows have."),
            added("Closed blocks wait in the tray", "A closed block stays dim in the tray for ten minutes; a click brings it back."),
            better("The block you point at", "While the pointer rests on one block, the others step back a little."),
            added("Undo after a change", "[ UNDO ] shows for six seconds after a change to the board."),
            added("Put a misheard order right", "Click the words he heard to correct them; the corrected order runs."),
            better("Figures that glide", "Gauges and meters glide, clock and timer digits roll, start-up lines type on."),
            added("A tour of the screen", "\"Show me around\": each part lights up while he names it."),
            better("Smooth at rest", "60 frames a second while you look, 10 when nobody is."),
            added("Windows like windows", "Double-click a title row for large; Esc closes a window of his."),
            added("Fling a block away", "A fast throw closes a block; pinned ones stay."),
            better("Keys that say their words", "A key's tip tells you what to say instead of clicking it."),
            better("Softer coasts and holograms", "The Earth's coasts glow faintly; holograms take a cool light."),
            better("Times read like a person", "\"Half past nine\", \"a quarter to ten\"; German times and units in German."),
            better("Game and app names", "Titles like \"Final Fantasy XIV\" are read as \"fourteen\"."),
            better("Questions sound like questions", "His voice rises a little at the end of a question."),
            better("Warnings slower, good news brighter", "A warning is said a touch slower, good news a touch quicker."),
            added("\"Go on\"", "Cut him off by mistake? \"Go on\" carries on from the start of that sentence."),
            added("Copy a window's text", "\"Copy the text of this window\": read by Windows' own OCR onto the clipboard."),
            added("Flashcards from a screenshot", "\"Make flashcards from my latest screenshot\": one card per vocabulary line."),
            better("Reading the screen without PowerShell", "Windows' own OCR is called directly: faster, and no window flashes."),
            added("Your Steam wishlist on sale", "A new discount is told once; \"anything on my wishlist on sale?\" lists them."),
            added("Money in other currencies", "\"50 dollars in euros\" at the central bank's daily rate, offline from the last copy."),
            added("Official weather warnings", "The weather service's warnings for your area, each told once; \"turn off the weather warnings\"."),
            added("Your lists on the phone", "Shopping and to-do lists on the phone page; a tick there ticks it on the PC."),
        ],
    },
    Release {
        date: "2026-11-14",
        name: "He in the background",
        entries: &[
            new("He as your desktop's background", "The board behind your icons, his views as real windows, the orb in a corner when you call.", "desktop_background"),
            better("His windows on call", "Ctrl+Alt+J or \"show your windows\": all of them in front, with a list to move or close each."),
            better("Windows open where you work", "A new window of his opens on the monitor you are using; beside a game, on another one."),
            better("Tidy on your word", "\"Tidy the windows\" lines his windows up; nothing lines up by itself any more."),
            better("Across monitors without a jump", "A block in your hand crosses from one monitor to the next; let go on the gap, it glides in."),
            better("The day on the background", "Running timers, planned orders and the song beside the emblem; what is next when you ask."),
            better("A clock on the desktop", "The time under the emblem, with the date and the weather. Drag it anywhere on any monitor."),
            better("The clock your way", "\"Clock with seconds\", \"12 hour clock\", \"hide the clock\"; a double click puts it back."),
            better("What's next, when you ask", "\"What's next?\" shows what comes beside the emblem for ten seconds, and he says it."),
            better("The emblem on any monitor", "Drag the big orb onto another monitor, or say \"move yourself to my second monitor\"."),
            better("Drag his windows by any bare part", "The whole title row and every empty part of a window of his moves it, not just its title."),
            better("His windows remember their place", "A window of his opens where it last stood, at the size it had."),
            better("Keep a window on top", "\"Keep the console on top\", or [ TOP ] in its bar: above other programs until you undo it."),
            better("His windows by voice", "\"Put the console on the left half\", \"move the text console to my second monitor\"."),
            better("The orb follows you", "Out in its corner, it glides to the monitor you go to."),
            better("The orb's own keys and size", "A right click on the corner orb: its keys. The wheel over it: larger or smaller."),
            better("The corner orb without its words", "\"Hide the words beside the orb\": the orb alone. A question for you still shows."),
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
            better("Reports and cards as real windows", "Each card of a briefing or a report is its own Windows window: taskbar, Alt+Tab, any monitor."),
            better("His windows in the taskbar", "His windows are windows of their own now: each has a key in the taskbar and a place in Alt+Tab."),
            fixed("The orb that shook", "The corner orb took a late report of its own flight for your hand and hopped between two spots."),
            fixed("The orb out for nothing", "An overlay nobody sees lay on the emblem and counted as a window: the orb came out for it."),
            fixed("No disc behind the corner orb", "The corner orb floats with nothing behind it; the words beside it keep their dark plate."),
            fixed("Corner orb only while the big one is hidden", "It flies back the moment the big orb is seen again, wherever on the desktop you put that."),
            fixed("The board froze beside a running simulation", "A window of his that draws without a pause took every turn to paint. The board draws on now."),
            fixed("\"Close the feature lab\" in its own window", "Views in windows of their own closed by their key only. They close by word now, all of them."),
            fixed("A closed window that turned black", "A simulation closed by hand stayed open and stood as a dark pane. It closes now."),
            fixed("\"Close all windows\" left the console", "His text console, the history and the camera are windows too, and close with the rest."),
            fixed("Rings that went round in steps", "As your background he drew 12 frames a second at rest and the corner orb 20. Smooth now."),
            fixed("The corner orb's corner", "It comes out bottom right, the words on the side of it that has the room."),
            fixed("Windows that closed as black panes", "The PC's window animations played a flat snapshot of each closing window of his. Not any more."),
            fixed("His own windows hide the emblem too", "A window of his over the big orb counts: called then, the orb comes out to its corner."),
            fixed("Under the taskbar", "His strips and the docked emblem keep to the work area: the taskbar cuts nothing off."),
            fixed("\"On my second monitor\" read the first", "\"What can you see on my second monitor?\" looks at that monitor now."),
            fixed("[ DOCK ] that closed and reopened", "As your background every view of his is a window: the key is gone there."),
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
        id: "other_pcs",
        title: "J.A.I. on other PCs",
        what: "A friend opens your phone page's address in a browser and takes \"J.A.I. for your Windows PC\" (or you give them that file): one file that installs him, or updates him where he is installed. It reads the graphics card (4 GB of memory at the least; under 8 GB he gets the smaller brain, Qwen3.5 2B), logs in at your PC with their account, fetches the program and its engines from your PC, then his voice, ears and brain, and puts him in the Start menu. Their J.A.I. calls yours every few seconds: you see it in the accounts list (online, its version, CPU, memory, graphics card), and what you order for it, it does. Theirs never sends anything to yours.",
        say: &["Jay, open the earth on Max's PC.", "Jay, turn the volume down on Max's computer.", "Jay, mach die Musik leiser auf Max PC.", "Jay, what did you do for Max this week?"],
        know: "Their J.A.I. always runs the version yours runs: when yours is newer, theirs fetches it at once and restarts (yours never updates itself), and a new commit on GitHub is announced to them. Your voice print reaches their J.A.I. and theirs reaches yours, held in memory only: each is stored on its owner's PC alone. Until their J.A.I. is logged in it answers nobody. An order waits ten minutes for a PC that is offline. A Linux PC installs him too (see J.A.I. on Linux).",
    },
    Explanation {
        id: "linux",
        title: "J.A.I. on Linux",
        what: "J.A.I. builds and runs on Linux as well as on Windows, made for KDE Plasma (Arch): listening, Whisper, his brain, his voice, the HUD and its windows, the tray icon, background mode and the corner orb, music and media keys (MPRIS), the volume of every app and the microphone (PipeWire), window orders, tiling and moving windows between monitors (KWin), reading the screen (KWin's screenshots and tesseract), instant replay, the webcam, typing, power and lock (logind), the questions about the PC, screen time and the break coach. Other desktops get what works without KDE (wmctrl and xdotool on X11, hyprctl on Hyprland). On Linux the installer is the program itself: started on its own or with --install, it logs in at your PC, takes your voice lock's model and his voice profile from there, fetches Ollama for Linux and his models, puts him into ~/.local/share/jai and the applications menu, and starts him.",
        say: &["Jay, what's using port 8080?", "Jay, what starts with Linux?", "Jay, do I need to restart?", "Jay, turn the volume down."],
        know: "On Arch install espeak-ng (his voice), zstd (the installer unpacks Ollama with it), libpulse (the volume), tesseract with tesseract-data-eng and tesseract-data-deu (reading the screen), pacman-contrib (updates), yt-dlp and ffmpeg (the media grabber). At his first start he writes his menu entry, which lets him see and move other windows, press keys and take screenshots under KDE: start him once more after that. Only on Windows: WhatsApp Desktop (Linux opens WhatsApp Web) and the microphone's level meter. Your PC runs Windows, so a Linux J.A.I. never takes its new versions from there: it hears of them from GitHub, where the Linux build is made.",
    },
    Explanation {
        id: "accounts",
        title: "Accounts",
        what: "You are the only one who may do everything: by your voice, on your phone, on this PC. Everyone else needs an account. On the phone page anyone can ask for one with a name and a password (3 to 20 characters): a key appears on your screen, and only with that key from you is the account made. A new account may ask questions and nothing more. In the accounts list you see every account with its password and choose what it may do in two places apart, on its own PC and on yours: talk only, what you switch on (tool by tool, down to single options: the Earth, but not its satellites), or everything. Everything on its own PC is all you can do, on that PC alone; on yours it never opens your own windows. You close topics to them (where they are, maths, your own words), pause them for an hour or a day, or remove them; you also see their phones and PCs and what J.A.I. did for them.",
        say: &["Jay, open the accounts list.", "Jay, öffne die Kontenliste.", "Jay, what did you do for Max today?"],
        know: "Logging in and asking for accounts are limited per address (a wrong password five times locks that name for ten minutes; one request a minute, three keys waiting at most), so the page cannot be flooded. Passwords are kept sealed in the vault, never in plain text on disk. Asked from an account, J.A.I. answers in a conversation of its own and never with anything of yours.",
    },
    Explanation {
        id: "admin",
        title: "The admin window",
        what: "\"Open the admin window\" shows every window J.A.I. can open, in groups, yours first, and opens each with a click. Yours are yours alone on every PC: the accounts list and the report, and the settings, the Feature Lab with the Tool Forge and Discord unless that PC's account may do everything there. Your main PC asks once for your admin account (a name and a password) and answers nobody until it is made. On another PC, J.A.I. does what that PC's account may: a new account only talks to him, and a window it may not open asks for your admin sign-in in a pop-up instead, without logging the PC out. Signed in, that window is yours until it closes. The admin window there is admin mode: everything on that PC is yours, your accounts list and your orders to other PCs too, until it closes.",
        say: &["Jay, open the admin window.", "Jay, activate admin mode.", "Jay, öffne das Admin-Fenster.", "Jay, admin mode off."],
        know: "Your admin account logs in nowhere: it only signs you in at another PC, and it is never paused, removed or shown from there. When the last window your sign-in opened closes, the sign-in ends at once, on that PC and at your main PC, which also lets one go that its PC has not asked about for two minutes. In the accounts list, \"On Max's PC\" switches each window on for that PC; an order you send to that PC from your main PC opens its window there all the same.",
    },
    Explanation {
        id: "unknown_voice",
        title: "\"Who am I speaking with?\"",
        what: "When a voice he has never heard calls him, he asks who it is. Its answer (\"I'm Erika\", \"ich bin Erika\", or just the name) starts learning that voice: after a few sentences he knows it, and from then on that voice may ask him questions. What else it may do you set in the accounts list, under its name.",
        say: &["(a new voice) Jay, what time is it?", "(after his question) I'm Erika.", "Jay, open the accounts list."],
        know: "He asks at most every two minutes, and only the voice he asked may answer. A name that already belongs to an account or a known voice is refused: you have to introduce that person. Your own voice is never asked.",
    },
    Explanation {
        id: "earth_landmarks",
        title: "Landmarks on the Earth",
        what: "While the Earth is up, name any place you want to see, a town or a single building, and he flies there: the Statue of Liberty, the Empire State Building, the Eiffel Tower, Times Square, Mount Everest. A building or a square he shows close up among the houses, in 3D, and the one you asked for burns brightest among them; a tower from as far back as its top stands in view; a bridge, a park or a mountain he shows whole from higher up.",
        say: &["Jay, show me the Statue of Liberty.", "Jay, zoom to the Empire State Building.", "Jay, fly me to the Golden Gate Bridge.", "Jay, zeig mir die Freiheitsstatue."],
        know: "He asks OpenStreetMap for the name and takes its best-known hit, so a famous landmark wins over a hamlet that shares its name; a town still comes from the town search, which knows how many live there. A landmark goes by OpenStreetMap's name for it: Big Ben is the Elizabeth Tower there. A tower over about 450 m (the Burj Khalifa) is cut at its top, or the houses would no longer stand. With the Earth closed, say \"on the globe\" at the end.",
    },
    Explanation {
        id: "earth_3d",
        title: "The Earth in 3D",
        what: "Down near the ground the 3D key in the Earth's column leans the camera back over the map, as Google Earth does: the town stands before you in perspective, its houses as blocks you see from the side, its streets running off into the distance, where the ground falls back into the dark. Drag with the right mouse button to tilt it (up and down) and to turn it (left and right); the left button still moves the map. The key again stands it straight down, north up. While it is turned, a mark on the key's ring points north.",
        say: &["Jay, switch to 3D.", "Jay, tilt the map.", "Jay, back to 2D.   (straight down, north up)", "Jay, schalte auf 3D."],
        know: "It leans back only near the ground; from high above the key first takes you down. It leans no further than the map has ground to show, so no sky appears, and the far tiles come coarser so it stays quick. The photo, the hologram, the streets and the houses all lean; place names and markers stay upright.",
    },
    Explanation {
        id: "earth_houses",
        title: "The houses of a town in 3D",
        what: "Zoom the Earth down to street level and the houses rise from the map as Stark's holograms: each traced on the ground first, then raised in a wave from the middle of the view, glass lit from its foot under a white roof line, as tall as OpenStreetMap says it is; where the map gives no height, as in most small towns, he judges it by the house's ground. The walls carry a line for every storey and, up close, their windows; a tower shows its height scale on its nearest corner, a flat roof its parapet, a small house a hipped roof; a roof says its house number once you are close, and a tower says how tall it is. It works in any town of the world whose houses are on OpenStreetMap. It works on the photo map and on the hologram. Straight down you see their roofs and the walls turned to the middle of the view; lean the camera back (the 3D key, or a drag with the right mouse button) and you look at them from the side, and turn round them to see every side.",
        say: &["Jay, show me the houses of Musterstadt in 3D.", "Jay, show me the buildings.   (down to street level where the map is)", "Jay, hide the buildings."],
        know: "These are models from outlines and heights, not photographed ones; a landmark mapped in parts (a tower on a hall) stands in its parts. The map knows no roof shapes, so the hipped roofs of small houses are his guess, and so is a height it does not give. A town's houses come from OpenFreeMap once, when you first look at it, and are kept on this PC. The key with the two blocks in the Earth's column switches them.",
    },
    Explanation {
        id: "his_own_mind",
        title: "He decides, and he has opinions",
        what: "When you leave a choice to him, he makes it and tells you what he chose: \"open a few random windows\" opens two to four of his views, his pick (\"three\" opens three). When you ask what he thinks, he answers with a verdict and a dry reason instead of declining, in the manner of the films' butler: understated, candid, calm and loyal. He says once what he thinks of a risky plan, then does as you decide.",
        say: &["Jay, open a few random windows.", "Jay, open three random windows.", "Jay, what do you think of this plan?", "Jay, which of the two would you take?"],
        know: "He answers in English whatever language you speak, unless the sentence itself asks for German (\"say that in German\"). For news and politics he looks the facts up first. How far his opinions go is up to the small language model on this PC: it may still decline now and then.",
    },
    Explanation {
        id: "hologram_interface",
        title: "The interface, drawn from scratch",
        what: "Everything he shows is built anew, the way the films' designers built Stark's screens. Structure is white: frames, rules and text. Your yellow is kept for what is live, selected or measured, and red for faults alone. Lines of light have a white-hot middle and a real glow that falls off through yellow into amber. Every window is numbered, opens about the core and is tied to it by a line, and nothing appears at once: a window is a spark, then a line, then a frame, then its rows.",
        say: &["Jay, switch to the black and white theme.", "Jay, make the interface yellow.   (your yellow, any time)", "Jay, set theme to arc reactor.   (or hot rod, gamma, vibranium, ember, graphite)"],
        know: "Every mark reads something real: the core's scale ring is the clock, the white ticks round it are your voice, the line under a window says what it is and how old. The themes you saved before still load, and so does a theme named in your settings from before today.",
    },
    Explanation {
        id: "two_cores",
        title: "Two cores",
        what: "He has two cores and wears the one you choose. Simple is an instrument of rings: his voice on the inner ring, a band that opens while he listens and spins up while he thinks, the clock on the scale, two orbits. Complex is the sphere of circuitry from the lab scene in Age of Ultron: four shells of circuit board round a bright nucleus, pulses running the traces, rings of data outside that are his voice. Its shells are the layers of his mind (voice, language, tools, memory), and each lights up while that layer works.",
        say: &["Jay, switch to the complex core.", "Jay, use the simple core.", "Jay, show me your core.   (the complex core, large: drag to turn it, wheel to zoom)"],
        know: "Both react to voice the same way: his own is your yellow, yours is white. In the corner orb the core is drawn as lines only, because that window's black is see-through.",
    },
    Explanation {
        id: "stark_look",
        title: "The Stark look",
        what: "The whole HUD is drawn the way Tony Stark's workshop draws its screens, in your yellow: black glass, hairline frames with long fine corner brackets, white titles with a tab, a dash and a tick ruler on the top edge, room between the blocks, hologram chart bars, a radial readout round the emblem and the core, and crosshair marks on the grid behind it all.",
        say: &["Jay, make the interface yellow.   (your yellow, any time)", "Jay, change the theme to Stark.   (the films' red and blue)", "[ THEME ] at the top left cycles every theme"],
        know: "Your colours stay as they are; every theme wears the same Stark design in its own colours. The minimal OLED design keeps its pure black. Panels are cut sharp; in the Sci-Fi design the corner slider in the settings still rounds them.",
    },
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
        title: "Always your desktop",
        what: "He is your desktop's background, always and on every PC, from the first start: the board itself stands behind your icons, on every monitor or on your first, and looks as it does across all monitors. Every view of his, the console too, opens as a window of its own that you move like any other. Speak to him while a window covers his emblem, and the orb leaves the background for the lower left corner of the monitor you are using: smaller, in front of everything, with what is being said beside it. Five seconds after the last of it, it glides back. Orb mode keeps his orb floating in front of your work the whole time, mini mode a slim strip along the top. His keys on the background take your clicks; everything else on the desktop is yours as before.",
        say: &["background on all monitors", "background only on the main monitor", "orb mode", "mini mode", "show your windows", "tidy the windows"],
        know: "Ctrl+Alt+J brings his windows to the front, Ctrl+Alt+K the console to type to him (the Hotkeys window changes them). Drag an orb where you want it (the corner orb remembers the spot for each monitor); a click on the corner orb opens the console, a double click on either orb, or BOARD on the strip, shows the desktop. Beside a game he leaves the mouse alone. It sits in front of a wallpaper program's picture and behind everything else. Should Explorer start again, or not take him at first, he opens his background anew by himself.",
    },
    Explanation {
        id: "hotkeys",
        title: "Hotkeys",
        what: "His keys work in every program, and in games while they are in front: the microphone on and off (no key until you give it one), his windows to the front (Ctrl+Alt+J), the console to type to him (Ctrl+Alt+K) and waking him from standby (Alt+J). In the Hotkeys window press SET beside one, then the new key; CLEAR takes a key away. A key another program already holds is marked TAKEN: choose another.",
        say: &["open the hotkeys window", "change my mute key", "zeig mir die tastenkürzel"],
        know: "A letter, a digit or Space needs Ctrl or Alt with it; F1 to F24, Pause and Scroll Lock may stand alone. Esc, or another window in front, gives up on a new key and keeps the old one. With the sound cues on, the microphone key sounds rising when he listens again and falling when he stops. On KDE the keys also stand in System Settings under Shortcuts.",
    },
    Explanation {
        id: "sonora",
        title: "Sonora, at once",
        what: "He speaks to Sonora through its own remote control: nothing is typed into its search and none of its windows is touched. A song, an album or any playlist of yours plays at once, downloaded or not, private or public; what is not in your library plays as Sonora's best hit. He reads you the queue, adds to its end or plays something next, turns shuffle and repeat on and off, jumps in the song, and likes or unlikes it. When Sonora isn't running, he starts it first.",
        say: &["what's in the queue", "play Highway to Hell next", "add Thunderstruck to the queue", "turn on shuffle", "repeat this song", "skip ahead 30 seconds", "like this song"],
        know: "Sonora's remote control cannot take songs out of the queue or change playlists: that stays in Sonora itself, and he says so. A Spotify playlist while Sonora is on another provider switches it to Spotify first.",
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
    let features = match news {
        0 => "no new features".to_string(),
        1 => "1 new feature".to_string(),
        n => format!("{n} new features"),
    };
    Ok(say(format!("The changes list is on the board: {} days of work, the latest on {} ({features}).", RELEASES.len(), latest.date), format!("Everything that changed, Sir, by day. The latest brought {features}.")))
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
    let ink = crate::holo::theme().ink();
    let muted = crate::holo::theme().muted();
    if is_open() {
        let (mut search, mut only) = {
            let ui = lock(&UI);
            (ui.search.clone(), ui.only)
        };
        let mut asked: Option<&'static str> = None;
        let closed = crate::gui::block(ctx, ID, "Changes // what is new, better, fixed", [700.0, 640.0], |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("FIND").font(face(Face::Label, 9.0)).color(muted));
                crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut search).desired_width(220.0).hint_text("a word, a date").font(face(Face::Body, 13.5)));
                ui.add_space(10.0);
                for (label, kind) in [("ALL", None), ("NEW", Some(Kind::New)), ("BETTER", Some(Kind::Better)), ("FIXED", Some(Kind::Fixed))] {
                    let lit = only == kind;
                    if ui.add(egui::Label::new(RichText::new(format!("{label}")).font(face(Face::Label, 9.0)).color(if lit { accent } else { muted })).sense(Sense::click())).clicked() {
                        only = kind;
                    }
                }
            });
            ui.add_space(6.0);
            crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), |ui| {
                let mut shown = 0;
                for release in RELEASES {
                    let entries = matching(release, &search, only);
                    if entries.is_empty() {
                        continue;
                    }
                    shown += entries.len();
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(day_words(release.date)).font(face(Face::Mono, 11.0)).color(accent));
                        ui.label(RichText::new(release.name).font(face(Face::Head, 17.0)).color(ink));
                    });
                    let rule = ui.cursor();
                    ui.painter().line_segment([rule.left_top(), egui::pos2(rule.left() + ui.available_width(), rule.top())], egui::Stroke::new(1.0f32, crate::holo::a(crate::holo::theme().ink(), 0.10)));
                    ui.add_space(4.0);
                    for entry in entries {
                        let tag_colour = match entry.kind {
                            Kind::New => accent,
                            Kind::Better => crate::holo::theme().ink(),
                            Kind::Fixed => muted,
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.add_sized([54.0, 16.0], egui::Label::new(RichText::new(entry.kind.tag()).font(face(Face::Label, 8.5)).color(tag_colour)).halign(egui::Align::LEFT));
                            ui.label(RichText::new(entry.title).color(ink).font(face(Face::Strong, 14.0)));
                            if let Some(id) = entry.explain {
                                let link = ui.add(egui::Label::new(RichText::new("HOW IT WORKS").font(face(Face::Label, 8.5)).color(accent).underline()).sense(Sense::click())).on_hover_text("Opens the explanation in a window of its own");
                                if link.clicked() {
                                    asked = Some(id);
                                }
                            }
                        });
                        // (A plain row: a line that wraps stays under its own start, not the tag's.)
                        ui.horizontal(|ui| {
                            ui.add_space(62.0);
                            ui.add(egui::Label::new(RichText::new(entry.what).color(muted).font(face(Face::Body, 12.5))).wrap());
                        });
                        ui.add_space(4.0);
                    }
                }
                if shown == 0 {
                    ui.add_space(20.0);
                    ui.label(RichText::new("Nothing in the list matches that.").color(muted).font(face(Face::Body, 13.5)));
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
            crate::holo_block::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new(e.title).font(face(Face::Head, 24.0)).color(ink));
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(e.what).color(ink).font(face(Face::Body, 14.0))).wrap());
                ui.add_space(14.0);
                ui.label(RichText::new("WHAT TO SAY").font(face(Face::Label, 9.0)).color(accent));
                ui.add_space(4.0);
                for line in e.say {
                    ui.add(egui::Label::new(RichText::new(*line).font(face(Face::Mono, 12.0)).color(ink)).wrap());
                    ui.add_space(2.0);
                }
                ui.add_space(12.0);
                ui.label(RichText::new("GOOD TO KNOW").font(face(Face::Label, 9.0)).color(accent));
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(e.know).color(muted).font(face(Face::Body, 13.0))).wrap());
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
