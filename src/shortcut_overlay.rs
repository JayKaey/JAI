//! The shortcut overlay: a cheat sheet of keyboard shortcuts for the program Sir is in ("show
//! shortcuts for this app") and a lookup by what he wants to do ("what is the shortcut for split
//! screen", "Tastenkürzel für Screenshot").
//!
//! The sheets are written out by hand for the programs on this desk — Windows itself, the
//! browsers, VS Code, Discord, Minecraft, the Office three, the file explorer, OBS — and
//! matched against the foreground window's process and title. A question is answered from the
//! sheet of the program in front first, then from Windows' own, then from all of them.

use serde_json::{json, Value};

/// One program's sheet: the names it answers to (process names and words of a window title)
/// and its shortcuts as (keys, what it does, extra search words in English and German).
pub struct Sheet {
    pub app: &'static str,
    pub matches: &'static [&'static str],
    pub keys: &'static [(&'static str, &'static str, &'static str)],
}

pub const SHEETS: &[Sheet] = &[
    Sheet {
        app: "Windows",
        matches: &["windows", "explorer", "desktop"],
        keys: &[
            ("Win + Left / Right", "Snap the window to half the screen", "split screen side by side teilen halb"),
            ("Win + Up / Down", "Maximize / restore or minimize the window", "maximieren minimieren full"),
            ("Win + Shift + Left / Right", "Move the window to the other monitor", "monitor bildschirm verschieben display"),
            ("Win + Shift + S", "Screenshot of a region to the clipboard", "screenshot snip bildschirmfoto ausschnitt"),
            ("Win + V", "Clipboard history", "clipboard zwischenablage verlauf paste"),
            ("Win + D", "Show the desktop", "desktop hide all anzeigen"),
            ("Win + Tab", "Task view and virtual desktops", "task view virtual desktops übersicht"),
            ("Win + Ctrl + D", "New virtual desktop", "virtual desktop neu new"),
            ("Win + Ctrl + Left / Right", "Switch virtual desktop", "virtual desktop wechseln switch"),
            ("Alt + Tab", "Switch between windows", "switch window wechseln fenster"),
            ("Alt + F4", "Close the window", "close schließen quit beenden"),
            ("Ctrl + Shift + Esc", "Task Manager", "task manager prozesse processes"),
            ("Win + L", "Lock the PC", "lock sperren"),
            ("Win + E", "File Explorer", "explorer files dateien"),
            ("Win + .", "Emoji and symbol picker", "symbols sonderzeichen picker"),
            ("Win + P", "Projection mode (duplicate, extend)", "project second screen beamer erweitern duplizieren"),
        ],
    },
    Sheet {
        app: "Browser",
        matches: &["chrome", "msedge", "edge", "firefox", "brave", "opera", "vivaldi", "browser"],
        keys: &[
            ("Ctrl + T", "New tab", "tab neu new"),
            ("Ctrl + W", "Close the tab", "tab close schließen"),
            ("Ctrl + Shift + T", "Reopen the closed tab", "reopen restore wiederherstellen geschlossen"),
            ("Ctrl + L", "Address bar", "address url adresse adressleiste"),
            ("Ctrl + Tab / Ctrl + Shift + Tab", "Next / previous tab", "next previous tab wechseln"),
            ("Ctrl + 1 … 8", "Jump to that tab", "tab jump springen"),
            ("Ctrl + F", "Find on the page", "find search suchen seite"),
            ("Ctrl + Shift + N", "Private window", "incognito private inkognito"),
            ("Ctrl + H", "History", "history verlauf"),
            ("Ctrl + J", "Downloads", "downloads"),
            ("F12", "Developer tools", "developer tools devtools entwickler"),
            ("Ctrl + Plus / Minus / 0", "Zoom in / out / reset", "zoom größer kleiner"),
            ("F5 / Ctrl + Shift + R", "Reload / reload without cache", "reload refresh neu laden cache"),
        ],
    },
    Sheet {
        app: "Visual Studio Code",
        matches: &["code", "vscode", "visual studio code", "cursor", "vscodium"],
        keys: &[
            ("Ctrl + P", "Open a file by name", "file open quick datei öffnen"),
            ("Ctrl + Shift + P", "Command palette", "command palette befehl"),
            ("Ctrl + `", "Toggle the terminal", "terminal console konsole"),
            ("Ctrl + B", "Toggle the sidebar", "sidebar seitenleiste"),
            ("Ctrl + /", "Comment the line", "comment kommentar"),
            ("Alt + Up / Down", "Move the line", "move line zeile verschieben"),
            ("Shift + Alt + Down", "Duplicate the line", "duplicate line zeile kopieren duplizieren"),
            ("Ctrl + D", "Select the next occurrence", "multi cursor select next occurrence auswahl"),
            ("Ctrl + Shift + K", "Delete the line", "delete line zeile löschen"),
            ("F2", "Rename the symbol", "rename umbenennen refactor"),
            ("F12", "Go to definition", "definition go to gehe zu"),
            ("Ctrl + Shift + F", "Search in all files", "search files global suche"),
            ("Shift + Alt + F", "Format the document", "format formatieren"),
            ("Ctrl + \\", "Split the editor", "split editor teilen side by side"),
        ],
    },
    Sheet {
        app: "Discord",
        matches: &["discord"],
        keys: &[
            ("Ctrl + Shift + M", "Mute the microphone", "mute microphone mikrofon stumm"),
            ("Ctrl + Shift + D", "Deafen", "deafen taub"),
            ("Ctrl + K", "Quick switcher", "switcher find channel suchen server"),
            ("Alt + Up / Down", "Previous / next channel", "channel kanal wechseln"),
            ("Ctrl + Alt + Up / Down", "Previous / next server", "server wechseln"),
            ("Esc", "Mark the channel as read", "read gelesen"),
            ("Ctrl + E", "Emoji picker", "picker"),
            ("Up", "Edit your last message", "edit message bearbeiten nachricht"),
            ("Ctrl + Enter", "Answer an incoming call", "call answer anruf annehmen"),
        ],
    },
    Sheet {
        app: "Minecraft",
        matches: &["minecraft", "javaw", "lunar", "prism", "modrinth"],
        keys: &[
            ("F3", "Debug screen (coordinates, FPS)", "coordinates koordinaten fps debug"),
            ("F3 + G", "Show chunk borders", "chunk borders grenzen"),
            ("F3 + B", "Show hitboxes", "hitbox hitboxes"),
            ("F3 + T", "Reload resource packs", "reload resource pack texture neu laden"),
            ("F3 + A", "Reload chunks", "reload chunks neu laden"),
            ("F2", "Screenshot", "screenshot bildschirmfoto"),
            ("F5", "Change the camera view", "third person camera perspektive kamera"),
            ("F1", "Hide the HUD", "hide hud ausblenden"),
            ("F11", "Fullscreen", "fullscreen vollbild"),
            ("Ctrl + Q", "Drop the whole stack", "drop stack wegwerfen"),
            ("Shift + Click", "Move the stack between inventories", "move stack inventory inventar verschieben"),
            ("T / /", "Chat / command", "chat command befehl"),
        ],
    },
    Sheet {
        app: "Excel",
        matches: &["excel"],
        keys: &[
            ("Ctrl + Shift + L", "Toggle filters", "filter"),
            ("Ctrl + T", "Make a table", "table tabelle"),
            ("Alt + =", "Sum the column", "sum summe autosum"),
            ("F4", "Repeat / toggle absolute reference", "absolute reference dollar bezug wiederholen"),
            ("Ctrl + Arrow", "Jump to the edge of the data", "jump edge springen ende"),
            ("Ctrl + ;", "Insert today's date", "date datum heute"),
            ("Ctrl + 1", "Format cells", "format cells zellen formatieren"),
            ("F2", "Edit the cell", "edit cell zelle bearbeiten"),
        ],
    },
    Sheet {
        app: "Word",
        matches: &["winword", "word"],
        keys: &[
            ("Ctrl + Alt + 1 / 2 / 3", "Heading 1 / 2 / 3", "heading überschrift"),
            ("Ctrl + Enter", "Page break", "page break seitenumbruch"),
            ("Ctrl + Shift + C / V", "Copy / paste formatting", "format painter formatierung übertragen"),
            ("Ctrl + K", "Insert a link", "link hyperlink"),
            ("Alt + Shift + D", "Insert the date", "date datum"),
            ("Ctrl + Shift + E", "Track changes", "track changes änderungen nachverfolgen"),
            ("F7", "Spelling and grammar", "spelling rechtschreibung grammar"),
        ],
    },
    Sheet {
        app: "PowerPoint",
        matches: &["powerpnt", "powerpoint"],
        keys: &[
            ("F5", "Start the show from the beginning", "present start präsentation starten"),
            ("Shift + F5", "Start from this slide", "present current slide folie"),
            ("Ctrl + M", "New slide", "new slide neue folie"),
            ("Ctrl + D", "Duplicate the slide or object", "duplicate duplizieren"),
            ("B", "Black screen during the show", "black screen schwarz pause"),
            ("Ctrl + G", "Group the objects", "group gruppieren"),
        ],
    },
    Sheet {
        app: "OBS",
        matches: &["obs64", "obs"],
        keys: &[
            ("Ctrl + E", "Edit the source's transform", "transform edit bearbeiten"),
            ("Ctrl + F", "Fit the source to the screen", "fit screen anpassen"),
            ("Ctrl + D", "Centre the source", "center zentrieren"),
            ("Alt + drag", "Crop the source", "crop zuschneiden"),
        ],
    },
];

/// The sheet for a foreground window: by its process name first, then by a word of its title.
pub fn for_app(process: &str, title: &str) -> Option<&'static Sheet> {
    let process = process.to_lowercase();
    let process = process.trim_end_matches(".exe");
    let title = title.to_lowercase();
    SHEETS.iter().find(|s| s.matches.contains(&process)).or_else(|| SHEETS.iter().skip(1).find(|s| s.matches.iter().any(|m| m.len() >= 4 && title.contains(m))))
}

/// The sheet a spoken program name means ("VS Code", "chrome", "this app" is the caller's).
pub fn named(app: &str) -> Option<&'static Sheet> {
    let asked = app.to_lowercase();
    let asked = asked.trim().trim_start_matches("the ").trim();
    if asked.is_empty() {
        return None;
    }
    SHEETS.iter().find(|s| s.app.to_lowercase() == asked || s.matches.contains(&asked)).or_else(|| SHEETS.iter().find(|s| s.app.to_lowercase().contains(asked) || s.matches.iter().any(|m| asked.contains(m) && m.len() >= 4)))
}

/// The shortcuts that do what `query` asks, best first, as (program, keys, what it does).
/// `front`: the program in front, whose sheet is asked first.
pub fn find(query: &str, front: Option<&'static Sheet>) -> Vec<(&'static str, &'static str, &'static str)> {
    const SMALL: &[&str] = &["the", "a", "an", "to", "for", "of", "in", "on", "my", "what", "is", "whats", "how", "do", "i", "shortcut", "shortcuts", "hotkey", "key", "keys", "keyboard", "tastenkürzel", "tastenkombination", "kürzel", "für", "wie", "was", "ist", "der", "die", "das", "den", "ein", "eine", "show", "me", "tell", "please", "this", "app", "program", "window"];
    let lower = query.to_lowercase();
    let asked: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1 && !SMALL.contains(w)).collect();
    if asked.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(u32, usize, (&'static str, &'static str, &'static str))> = Vec::new();
    let mut order = 0;
    for sheet in SHEETS {
        // The program in front outranks Windows, which outranks the rest.
        let rank = if front.is_some_and(|f| std::ptr::eq(f, sheet)) { 3 } else if sheet.app == "Windows" { 2 } else { 1 };
        for (keys, what, extra) in sheet.keys {
            order += 1;
            let hay = format!("{} {}", what.to_lowercase(), extra);
            let words: Vec<&str> = hay.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
            let hits = asked.iter().filter(|a| words.iter().any(|w| w == *a || (a.len() >= 4 && w.starts_with(**a)))).count() as u32;
            // Every word asked for, or at least the larger part of the question.
            if hits > 0 && hits * 2 > asked.len() as u32 {
                scored.push((hits * 10 + rank, order, (sheet.app, *keys, *what)));
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(5).map(|(_, _, found)| found).collect()
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "show_app_shortcuts",
        "description": "Keyboard shortcuts: the cheat sheet of the program Sir is in (or app: 'VS Code', 'Chrome', 'Discord', 'Minecraft', 'Excel', 'Windows' …) as a block, or with query the shortcut that does something ('split screen', 'screenshot', 'mute the microphone').",
        "parameters": {"type": "object", "properties": {
            "app": {"type": "string", "description": "The program (default: the one in front)."},
            "query": {"type": "string", "description": "What the shortcut should do."}
        }, "required": []}
    }})
}

/// The `show_app_shortcuts` tool. `front`: the (process, title) of the window Sir is in.
pub fn run_tool(args: &Value, front: Option<(String, String)>) -> Result<String, String> {
    let asked_app = crate::features::str_arg(args, &["app", "program", "application"]).filter(|a| !matches!(a.to_lowercase().as_str(), "this" | "this app" | "this program" | "here" | "current" | ""));
    let sheet = match asked_app {
        Some(name) => Some(named(name).ok_or_else(|| format!("I have no shortcut sheet for {name}, Sir. I know {}.", SHEETS.iter().map(|s| s.app).collect::<Vec<_>>().join(", ")))?),
        None => front.as_ref().and_then(|(process, title)| for_app(process, title)),
    };
    if let Some(query) = crate::features::str_arg(args, &["query", "action", "what"]).filter(|q| !q.trim().is_empty()) {
        let found = find(query, sheet);
        let Some((app, keys, what)) = found.first() else {
            return Ok(crate::features::spoken(format!("No shortcut for '{query}' on my sheets."), format!("I have no shortcut for {query} on my sheets, Sir.")));
        };
        let lines: Vec<String> = found.iter().map(|(app, keys, what)| format!("{keys}  —  {what} ({app})")).collect();
        return Ok(crate::features::spoken(lines.join("\n"), format!("{}: {what}, in {app}, Sir.", spoken_keys(keys))));
    }
    let sheet = sheet.unwrap_or(&SHEETS[0]);
    let lines: Vec<String> = sheet.keys.iter().map(|(keys, what, _)| format!("{keys}  —  {what}")).collect();
    Ok(crate::features::spoken(format!("{} shortcuts\n{}", sheet.app, lines.join("\n")), format!("The {} shortcuts are on the board, Sir.", sheet.app)))
}

/// Keys as they are said: "Win + Shift + S" → "Windows, Shift, S".
fn spoken_keys(keys: &str) -> String {
    keys.split('/').next().unwrap_or(keys).split('+').map(|k| match k.trim() {
        "Win" => "Windows",
        "Ctrl" => "Control",
        "Esc" => "Escape",
        "`" => "backtick",
        "\\" => "backslash",
        "." => "period",
        ";" => "semicolon",
        "=" => "equals",
        other => other,
    }).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_in_front_gets_its_own_sheet() {
        assert_eq!(for_app("Code.exe", "main.rs - jarvis - Visual Studio Code").map(|s| s.app), Some("Visual Studio Code"));
        assert_eq!(for_app("chrome.exe", "YouTube - Google Chrome").map(|s| s.app), Some("Browser"));
        assert_eq!(for_app("javaw.exe", "Minecraft 1.21").map(|s| s.app), Some("Minecraft"));
        assert_eq!(for_app("ApplicationFrameHost.exe", "Discord | #general").map(|s| s.app), Some("Discord"), "by the title when the process says nothing");
        assert_eq!(for_app("notepad.exe", "Untitled - Notepad").map(|s| s.app), None);
        assert_eq!(named("VS Code").map(|s| s.app), Some("Visual Studio Code"));
        assert_eq!(named("the browser").map(|s| s.app), Some("Browser"));
        assert_eq!(named("Photoshop").map(|s| s.app), None);
    }

    #[test]
    fn a_question_finds_the_shortcut_in_english_and_german() {
        let first = |q: &str, front: Option<&'static Sheet>| find(q, front).first().map(|f| (f.0, f.1));
        assert_eq!(first("what is the shortcut for split screen", None), Some(("Windows", "Win + Left / Right")));
        assert_eq!(first("Tastenkürzel für Screenshot", None), Some(("Windows", "Win + Shift + S")));
        assert_eq!(first("mute the microphone", None), Some(("Discord", "Ctrl + Shift + M")));
        assert_eq!(first("Zwischenablage Verlauf", None), Some(("Windows", "Win + V")));
        // The program in front answers first: a screenshot in Minecraft is F2, not Win+Shift+S.
        let minecraft = for_app("javaw.exe", "");
        assert_eq!(first("screenshot", minecraft), Some(("Minecraft", "F2")));
        assert_eq!(first("screenshot", None), Some(("Windows", "Win + Shift + S")));
        // "Split" in VS Code is the editor's split.
        assert_eq!(first("split the editor", for_app("code.exe", "")), Some(("Visual Studio Code", "Ctrl + \\")));
        assert!(find("teleport to mars", None).is_empty() && find("", None).is_empty() && find("shortcut for the", None).is_empty());
    }

    #[test]
    fn the_tool_shows_a_sheet_or_answers_a_question() {
        let front = Some(("Code.exe".to_string(), "main.rs - Visual Studio Code".to_string()));
        let sheet = run_tool(&json!({}), front.clone()).unwrap();
        assert!(sheet.starts_with("Visual Studio Code shortcuts\n") && sheet.contains("Ctrl + Shift + P  —  Command palette"), "{sheet}");
        assert!(sheet.ends_with("Spoken: The Visual Studio Code shortcuts are on the board, Sir."));
        let answer = run_tool(&json!({"query": "split screen"}), None).unwrap();
        assert!(answer.contains("Win + Left / Right") && answer.contains("Spoken: Windows, Left: Snap the window to half the screen, in Windows, Sir."), "{answer}");
        // No window in front and no program named: Windows' own sheet.
        assert!(run_tool(&json!({}), None).unwrap().starts_with("Windows shortcuts\n"));
        assert!(run_tool(&json!({"app": "this app"}), front).unwrap().starts_with("Visual Studio Code shortcuts"));
        assert!(run_tool(&json!({"app": "Photoshop"}), None).is_err());
        assert!(run_tool(&json!({"query": "teleport to mars"}), None).unwrap().contains("No shortcut for"));
    }

    #[test]
    fn every_sheet_is_plain_text_with_search_words() {
        for sheet in SHEETS {
            assert!(!sheet.keys.is_empty() && !sheet.matches.is_empty(), "{}", sheet.app);
            for (keys, what, extra) in sheet.keys {
                assert!(!keys.is_empty() && !what.is_empty() && *extra == extra.to_lowercase(), "{} {keys}", sheet.app);
                assert!(!what.chars().chain(keys.chars()).any(crate::live_self_tester::is_pictograph), "{} {keys}", sheet.app);
            }
        }
    }
}
