//! The programs J.A.I.OS's shell knows: the menu entries (`.desktop` files) with their names,
//! commands and categories, the search and the groups of the [J] menu, which entry a window
//! belongs to, and where the freedesktop icon themes keep an icon. Text in, values out: the
//! folders are only walked on Linux.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One program of the menu.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct App {
    /// The entry's id ("org.kde.dolphin", "firefox"): its file name without `.desktop`.
    pub id: String,
    pub name: String,
    pub generic: String,
    pub keywords: Vec<String>,
    pub exec: String,
    pub icon: String,
    pub categories: Vec<String>,
    /// The window class its windows carry, where the entry names one.
    pub wm_class: String,
    pub terminal: bool,
}

/// The groups of a key file (a menu entry, an icon theme's index): each group's name and its
/// keys as written.
fn key_file(text: &str) -> Vec<(&str, Vec<(&str, &str)>)> {
    let mut groups: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            groups.push((name, Vec::new()));
        } else if let (Some((key, value)), Some(group), false) = (line.split_once('='), groups.last_mut(), line.starts_with('#')) {
            group.1.push((key.trim(), value.trim()));
        }
    }
    groups
}

/// The keys of the group `name`; a key in the language `lang` ("de": `Name[de]`,
/// `Name[de_DE]`) wins over the plain one.
fn group_of<'a>(text: &'a str, name: &str, lang: &str) -> HashMap<&'a str, &'a str> {
    let mut keys = HashMap::new();
    let Some((_, lines)) = key_file(text).into_iter().find(|(group, _)| *group == name) else { return keys };
    for (key, value) in &lines {
        if !key.contains('[') {
            keys.insert(*key, *value);
        }
    }
    for (key, value) in &lines {
        if let Some((plain, locale)) = key.strip_suffix(']').and_then(|k| k.split_once('[')) {
            if !lang.is_empty() && locale.split(['_', '@']).next() == Some(lang) {
                keys.insert(plain, *value);
            }
        }
    }
    keys
}

fn list(value: &str) -> Vec<String> {
    value.split(';').map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect()
}

/// The program a menu entry describes; None for what a menu does not show (no program, hidden,
/// meant for another desktop than KDE).
pub fn parse(id: &str, text: &str, lang: &str) -> Option<App> {
    let keys = group_of(text, "Desktop Entry", lang);
    let get = |key: &str| keys.get(key).copied().unwrap_or("");
    let shown_in = |key: &str| list(get(key)).iter().any(|d| d == "KDE");
    if get("Type") != "Application" || get("NoDisplay") == "true" || get("Hidden") == "true" || get("Name").is_empty() || get("Exec").is_empty() {
        return None;
    }
    if (!get("OnlyShowIn").is_empty() && !shown_in("OnlyShowIn")) || shown_in("NotShowIn") {
        return None;
    }
    Some(App {
        id: id.to_string(),
        name: get("Name").to_string(),
        generic: get("GenericName").to_string(),
        keywords: list(get("Keywords")),
        exec: get("Exec").to_string(),
        icon: get("Icon").to_string(),
        categories: list(get("Categories")),
        wm_class: get("StartupWMClass").to_string(),
        terminal: get("Terminal") == "true",
    })
}

/// The command of an entry's `Exec` line: its words (quotes kept together), without the field
/// codes (`%u`, `%F`) a file manager would fill in.
pub fn command(exec: &str) -> Vec<String> {
    let (mut words, mut word, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => (quoted, any) = (!quoted, true),
            '\\' if quoted => word.extend(chars.next()),
            '%' => match chars.next() {
                Some('%') => word.push('%'),
                _ => {}
            },
            c if c.is_whitespace() && !quoted => {
                if any || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            c => word.push(c),
        }
    }
    if any || !word.is_empty() {
        words.push(word);
    }
    words
}

/// The menu's groups and the categories that put a program into each; the first that fits
/// wins (Steam is a game before it is a network program).
pub const GROUPS: [(&str, &[&str]); 9] = [
    ("Games", &["Game"]),
    ("Internet", &["Network", "WebBrowser"]),
    ("Office", &["Office"]),
    ("Multimedia", &["AudioVideo", "Audio", "Video"]),
    ("Graphics", &["Graphics"]),
    ("Development", &["Development"]),
    ("Education", &["Education", "Science"]),
    ("System", &["System", "Settings"]),
    ("Utilities", &["Utility", "Accessibility"]),
];

/// The group for what fits none.
pub const OTHER: &str = "Other";

pub fn group(app: &App) -> &'static str {
    GROUPS.iter().find(|(_, categories)| app.categories.iter().any(|c| categories.contains(&c.as_str()))).map_or(OTHER, |(name, _)| name)
}

/// The groups that hold a program, in the menu's order, each with its programs by name.
pub fn grouped(apps: &[App]) -> Vec<(&'static str, Vec<&App>)> {
    GROUPS.iter().map(|(name, _)| *name).chain([OTHER]).map(|name| (name, apps.iter().filter(|a| group(a) == name).collect::<Vec<_>>())).filter(|(_, apps)| !apps.is_empty()).collect()
}

/// The programs `query` finds, the best first: a name that begins with it, a word of the name
/// that does, a name that holds it, then the generic name and the keywords.
pub fn search<'a>(apps: &'a [App], query: &str) -> Vec<&'a App> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let rank = |app: &App| {
        let name = app.name.to_lowercase();
        if name.starts_with(&q) {
            Some(0)
        } else if name.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q)) {
            Some(1)
        } else if name.contains(&q) {
            Some(2)
        } else if app.generic.to_lowercase().contains(&q) {
            Some(3)
        } else if app.keywords.iter().any(|k| k.to_lowercase().starts_with(&q)) {
            Some(4)
        } else {
            None
        }
    };
    let mut found: Vec<(u8, &App)> = apps.iter().filter_map(|a| rank(a).map(|r| (r, a))).collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
    found.into_iter().map(|(_, a)| a).collect()
}

/// The program a window belongs to, by the id KWin gives for it ("org.kde.dolphin", an X11
/// program's class) or its resource name.
pub fn of_window<'a>(apps: &'a [App], app_id: &str, resource: &str) -> Option<&'a App> {
    let same = |a: &str, b: &str| !a.is_empty() && a.eq_ignore_ascii_case(b);
    let tail = |id: &str| id.rsplit('.').next().unwrap_or(id).to_string();
    apps.iter()
        .find(|a| same(&a.id, app_id))
        .or_else(|| apps.iter().find(|a| same(&a.wm_class, app_id) || same(&a.wm_class, resource)))
        .or_else(|| apps.iter().find(|a| same(&tail(&a.id), app_id) || same(&tail(&a.id), resource) || same(&a.id, &tail(app_id))))
}

/// Every program of the menu folders `dirs` (the first folder that holds an id wins, as the
/// user's own entry does over the system's), by name.
#[cfg(target_os = "linux")]
pub fn load(dirs: &[PathBuf], lang: &str) -> Vec<App> {
    let (mut seen, mut apps) = (HashSet::new(), Vec::new());
    for dir in dirs {
        for file in walkdir::WalkDir::new(dir).follow_links(true).into_iter().flatten().filter(|f| f.path().extension().is_some_and(|e| e == "desktop")) {
            let Ok(inside) = file.path().strip_prefix(dir) else { continue };
            let id = inside.with_extension("").to_string_lossy().replace('/', "-");
            if seen.insert(id.clone()) {
                apps.extend(std::fs::read_to_string(file.path()).ok().and_then(|text| parse(&id, &text, lang)));
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

// --------------------------------------------------------------------------------- icons ----

/// One folder of an icon theme, as its index describes it.
#[derive(Debug, Clone, PartialEq)]
struct IconDir {
    path: String,
    size: u32,
    scale: u32,
    min: u32,
    max: u32,
    threshold: u32,
    kind: String,
}

impl IconDir {
    /// How far the folder's icons are from `size` at `scale` (0: made for it), as the icon
    /// theme specification measures it.
    fn distance(&self, size: u32, scale: u32) -> u32 {
        let (want, own) = (size * scale, self.size * self.scale);
        let (min, max) = match self.kind.as_str() {
            "Fixed" => (own, own),
            "Scalable" => (self.min * self.scale, self.max * self.scale),
            _ => (own.saturating_sub(self.threshold * self.scale), own + self.threshold * self.scale),
        };
        if want < min {
            min - want
        } else {
            want.saturating_sub(max)
        }
    }
}

/// An icon theme's index: the themes it inherits from and its folders.
fn theme_index(text: &str) -> (Vec<String>, Vec<IconDir>) {
    let head = group_of(text, "Icon Theme", "");
    let named = |key: &str| head.get(key).map(|v| v.split(',').map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
    let wanted: HashSet<String> = named("Directories").into_iter().chain(named("ScaledDirectories")).collect();
    let dirs = key_file(text)
        .into_iter()
        .filter(|(name, _)| wanted.contains(*name))
        .map(|(name, keys)| {
            let number = |key: &str, or: u32| keys.iter().find(|(k, _)| *k == key).and_then(|(_, v)| v.parse().ok()).unwrap_or(or);
            let size = number("Size", 48);
            IconDir { path: name.to_string(), size, scale: number("Scale", 1), min: number("MinSize", size), max: number("MaxSize", size), threshold: number("Threshold", 2), kind: keys.iter().find(|(k, _)| *k == "Type").map_or("Threshold", |(_, v)| v).to_string() }
        })
        .collect();
    (named("Inherits"), dirs)
}

/// The file of the icon `name` for `size` points at `scale`, looked for as the freedesktop
/// icon theme specification says: in `theme` and what it inherits from, then in hicolor, in
/// each of the icon folders `bases`; then loose in a base (`/usr/share/pixmaps`). An absolute
/// path names its own file.
pub fn icon_file(name: &str, size: u32, scale: u32, theme: &str, bases: &[PathBuf]) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if Path::new(name).is_absolute() {
        return Path::new(name).is_file().then(|| PathBuf::from(name));
    }
    let (mut queue, mut done) = (vec![theme.to_string()], HashSet::new());
    while !queue.is_empty() {
        let theme = queue.remove(0);
        if theme.is_empty() || !done.insert(theme.clone()) {
            continue;
        }
        let mut best: Option<(u32, bool, PathBuf)> = None;
        for base in bases {
            let root = base.join(&theme);
            let Ok(index) = std::fs::read_to_string(root.join("index.theme")) else { continue };
            let (parents, dirs) = theme_index(&index);
            queue.extend(parents);
            for dir in dirs {
                let distance = dir.distance(size, scale);
                // (Of two as near, the drawing wins over the picture: it is sharp at any scale.)
                for (drawn, ext) in [(true, "svg"), (true, "svgz"), (false, "png")] {
                    let file = root.join(&dir.path).join(format!("{name}.{ext}"));
                    if best.as_ref().is_none_or(|(d, vector, _)| (distance, !drawn) < (*d, !*vector)) && file.is_file() {
                        best = Some((distance, drawn, file));
                    }
                }
            }
        }
        if let Some((_, _, file)) = best {
            return Some(file);
        }
        if queue.is_empty() && !done.contains("hicolor") {
            queue.push("hicolor".into());
        }
    }
    bases.iter().flat_map(|base| ["svg", "png"].map(|ext| base.join(format!("{name}.{ext}")))).find(|f| f.is_file())
}

/// The picture in an icon file at `px` pixels square (SVG drawn at that size, PNG scaled to it).
#[cfg(target_os = "linux")]
pub fn icon_image(file: &Path, px: u32) -> Option<eframe::egui::ColorImage> {
    use resvg::{tiny_skia, usvg};
    let bytes = std::fs::read(file).ok()?;
    let px = px.clamp(8, 512);
    if file.extension().is_some_and(|e| e == "png") {
        let picture = image::load_from_memory(&bytes).ok()?.resize_exact(px, px, image::imageops::FilterType::Triangle).to_rgba8();
        return Some(eframe::egui::ColorImage::from_rgba_unmultiplied([px as usize; 2], picture.as_raw()));
    }
    let tree = usvg::Tree::from_data(&bytes, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
    let size = tree.size();
    let fit = (px as f32 / size.width()).min(px as f32 / size.height());
    let shift = tiny_skia::Transform::from_scale(fit, fit).post_translate((px as f32 - size.width() * fit) / 2.0, (px as f32 - size.height() * fit) / 2.0);
    resvg::render(&tree, shift, &mut pixmap.as_mut());
    Some(eframe::egui::ColorImage::from_rgba_premultiplied([px as usize; 2], pixmap.data()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const FIREFOX: &str = "[Desktop Entry]\nVersion=1.0\nName=Firefox\nName[de]=Firefox Browser\nGenericName=Web Browser\nGenericName[de]=Internetbrowser\nKeywords=Internet;WWW;Browser;Web;Explorer;\nExec=/usr/lib/firefox/firefox %u\nIcon=firefox\nTerminal=false\nType=Application\nCategories=Network;WebBrowser;\nStartupWMClass=firefox\n\n[Desktop Action new-window]\nName=New Window\nExec=/usr/lib/firefox/firefox --new-window %u\n";
    const STEAM: &str = "[Desktop Entry]\nName=Steam (Runtime)\nComment=Application for managing and playing games on Steam\nExec=/usr/bin/steam-runtime %U\nIcon=steam\nTerminal=false\nType=Application\nCategories=Network;FileTransfer;Game;\n";
    const DOLPHIN: &str = "[Desktop Entry]\nName=Dolphin\nName[de]=Dolphin\nGenericName=File Manager\nGenericName[de]=Dateiverwaltung\nExec=dolphin %u\nIcon=system-file-manager\nType=Application\nCategories=Qt;KDE;System;FileTools;FileManager;\nKeywords=files;file management;\n";
    const KONSOLE: &str = "[Desktop Entry]\nType=Application\nExec=konsole\nIcon=utilities-terminal\nCategories=Qt;KDE;System;TerminalEmulator;\nName=Konsole\nGenericName=Terminal\nKeywords=terminal;console;shell;command line;\n";
    const KRITA: &str = "[Desktop Entry]\nName=Krita\nExec=krita %F\nGenericName=Digital Painting\nIcon=krita\nType=Application\nCategories=Qt;KDE;Graphics;2DGraphics;RasterGraphics;\nStartupWMClass=krita\n";

    pub(crate) fn sample() -> Vec<App> {
        [("firefox", FIREFOX), ("steam", STEAM), ("org.kde.dolphin", DOLPHIN), ("org.kde.konsole", KONSOLE), ("org.kde.krita", KRITA)].into_iter().filter_map(|(id, text)| parse(id, text, "")).collect()
    }

    #[test]
    fn a_menu_entry_is_read_in_sirs_language_and_hidden_ones_are_left_out() {
        let english = parse("firefox", FIREFOX, "").expect("an entry");
        assert_eq!((english.name.as_str(), english.generic.as_str(), english.icon.as_str(), english.wm_class.as_str()), ("Firefox", "Web Browser", "firefox", "firefox"));
        assert_eq!(english.categories, ["Network", "WebBrowser"]);
        assert_eq!(english.keywords.len(), 5);
        // (The action's Name and Exec, further down the file, are not the program's.)
        assert_eq!(english.exec, "/usr/lib/firefox/firefox %u");
        let german = parse("firefox", FIREFOX, "de").expect("an entry");
        assert_eq!((german.name.as_str(), german.generic.as_str()), ("Firefox Browser", "Internetbrowser"));
        for hidden in ["NoDisplay=true", "Hidden=true", "OnlyShowIn=GNOME;", "NotShowIn=KDE;", "Type=Link"] {
            let text = FIREFOX.replace("Type=Application", hidden).replace("Version=1.0", if hidden.starts_with("Type") { "" } else { "Type=Application" });
            assert_eq!(parse("firefox", &text, ""), None, "{hidden}");
        }
        assert!(parse("firefox", &FIREFOX.replace("Version=1.0", "OnlyShowIn=KDE;"), "").is_some());
    }

    #[test]
    fn a_command_keeps_quoted_words_and_drops_field_codes() {
        assert_eq!(command("/usr/lib/firefox/firefox %u"), ["/usr/lib/firefox/firefox"]);
        assert_eq!(command("env GAME=1 \"/opt/My Game/run.sh\" --title \"A \\\"B\\\"\" %F"), ["env", "GAME=1", "/opt/My Game/run.sh", "--title", "A \"B\""]);
        assert_eq!(command("printf 100%% \"\""), ["printf", "100%", ""]);
        assert_eq!(command("  "), Vec::<String>::new());
    }

    #[test]
    fn programs_fall_into_the_menus_groups() {
        let apps = sample();
        let groups = grouped(&apps);
        let names = |group: &str| groups.iter().find(|(g, _)| *g == group).map(|(_, apps)| apps.iter().map(|a| a.id.as_str()).collect::<Vec<_>>()).unwrap_or_default();
        assert_eq!(names("Games"), ["steam"]);
        assert_eq!(names("Internet"), ["firefox"]);
        assert_eq!(names("System"), ["org.kde.dolphin", "org.kde.konsole"]);
        assert_eq!(names("Graphics"), ["org.kde.krita"]);
        // (Only groups that hold something, in the menu's order.)
        assert_eq!(groups.iter().map(|(g, _)| *g).collect::<Vec<_>>(), ["Games", "Internet", "Graphics", "System"]);
        assert_eq!(group(&App { categories: vec!["Qt".into()], ..App::default() }), OTHER);
    }

    #[test]
    fn the_search_finds_by_name_generic_name_and_keyword() {
        let apps = sample();
        let found = |q: &str| search(&apps, q).iter().map(|a| a.id.as_str()).collect::<Vec<_>>();
        assert_eq!(found("fire"), ["firefox"]);
        assert_eq!(found("  KON"), ["org.kde.konsole"]);
        assert_eq!(found("runtime"), ["steam"]);
        assert_eq!(found("file manager"), ["org.kde.dolphin"]);
        assert_eq!(found("shell"), ["org.kde.konsole"]);
        // A name that begins with it comes before one that only holds it.
        assert_eq!(found("k"), ["org.kde.konsole", "org.kde.krita"]);
        assert_eq!(found("ol"), ["org.kde.dolphin", "org.kde.konsole"]);
        assert_eq!(found("turn the lights off"), Vec::<&str>::new());
        assert_eq!(found(""), Vec::<&str>::new());
    }

    #[test]
    fn a_window_finds_its_program() {
        let apps = sample();
        let of = |id: &str, resource: &str| of_window(&apps, id, resource).map(|a| a.id.as_str());
        assert_eq!(of("org.kde.dolphin", ""), Some("org.kde.dolphin"));
        assert_eq!(of("Firefox", "Navigator"), Some("firefox"));
        assert_eq!(of("konsole", ""), Some("org.kde.konsole"));
        assert_eq!(of("", "krita"), Some("org.kde.krita"));
        assert_eq!(of("steam_app_1245620", ""), None);
        assert_eq!(of("", ""), None);
    }

    #[test]
    fn an_icon_is_found_in_the_theme_its_parents_and_the_loose_pictures() {
        let base = std::env::temp_dir().join(format!("jai-os-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let put = |path: &str, text: &str| {
            let file = base.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        put("icons/dark/index.theme", "[Icon Theme]\nName=Dark\nInherits=light\nDirectories=apps/22,apps/48\n\n[apps/22]\nSize=22\nType=Fixed\n\n[apps/48]\nSize=48\nMinSize=32\nMaxSize=256\nType=Scalable\n");
        put("icons/light/index.theme", "[Icon Theme]\nName=Light\nDirectories=apps/64\n\n[apps/64]\nSize=64\nType=Threshold\n");
        put("icons/hicolor/index.theme", "[Icon Theme]\nName=Hicolor\nDirectories=48x48/apps\n\n[48x48/apps]\nSize=48\n");
        put("icons/dark/apps/22/firefox.png", "");
        put("icons/dark/apps/48/firefox.svg", "");
        put("icons/light/apps/64/krita.png", "");
        put("icons/hicolor/48x48/apps/steam.png", "");
        put("pixmaps/game.png", "");
        let bases = [base.join("icons"), base.join("pixmaps")];
        let find = |name: &str, size: u32, scale: u32| icon_file(name, size, scale, "dark", &bases).map(|f| f.strip_prefix(&base).unwrap().to_string_lossy().replace('\\', "/"));
        // The folder made for the size; at twice the scale the drawing that reaches it.
        assert_eq!(find("firefox", 22, 1).as_deref(), Some("icons/dark/apps/22/firefox.png"));
        assert_eq!(find("firefox", 22, 2).as_deref(), Some("icons/dark/apps/48/firefox.svg"));
        assert_eq!(find("firefox", 64, 1).as_deref(), Some("icons/dark/apps/48/firefox.svg"));
        assert_eq!(find("krita", 22, 1).as_deref(), Some("icons/light/apps/64/krita.png"));
        assert_eq!(find("steam", 22, 1).as_deref(), Some("icons/hicolor/48x48/apps/steam.png"));
        assert_eq!(find("game", 22, 1).as_deref(), Some("pixmaps/game.png"));
        assert_eq!(find("nothing", 22, 1), None);
        assert_eq!(find("", 22, 1), None);
        let own = base.join("pixmaps/game.png");
        assert_eq!(icon_file(&own.to_string_lossy(), 22, 1, "dark", &bases), Some(own));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_drawn_icon_becomes_a_picture_of_the_size_asked_for() {
        let file = std::env::temp_dir().join(format!("jai-os-icon-{}.svg", std::process::id()));
        std::fs::write(&file, "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><rect width=\"16\" height=\"16\" fill=\"#ff0000\"/></svg>").unwrap();
        let picture = icon_image(&file, 44).expect("a picture");
        let _ = std::fs::remove_file(&file);
        assert_eq!(picture.size, [44, 44]);
        assert_eq!(picture.pixels[22 * 44 + 22], eframe::egui::Color32::from_rgb(255, 0, 0));
    }
}
