//! Cross-platform application launching and closing.
//!
//! Resolution order: URL / known folder / existing path → alias table (per-OS candidates) →
//! executable lookup (App Paths + PATH) → Start-Menu shortcuts (Windows) or `.desktop`
//! entries (Linux) → UWP Start apps (Windows). Closing is graceful first (WM_CLOSE / SIGTERM),
//! then forceful via `sysinfo`, and never touches system processes or JAI's own process tree.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use strsim::jaro_winkler;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tracing::{debug, info};

pub struct AppSpec {
    pub id: &'static str,
    pub display: &'static str,
    pub aliases: &'static [&'static str],
    /// Launch candidates: `uri:`, `exe:`, `path:` (`|` separates arguments), `aumid:`, `url:`.
    pub windows: &'static [&'static str],
    /// Launch candidates: `bin:`, `flatpak:`, `url:`, `xdg:`.
    pub linux: &'static [&'static str],
    /// Process names (lower-case, without `.exe`) used for closing / window matching.
    pub processes: &'static [&'static str],
}

pub static APPS: &[AppSpec] = &[
    AppSpec {
        id: "spotify",
        display: "Spotify",
        aliases: &["spotify", "spotify music", "spotifiy", "spotty"],
        // The executable first: a bare `spotify:` URI handed over while Spotify is still
        // starting makes it quit again.
        windows: &["path:%APPDATA%\\Spotify\\Spotify.exe", "aumid:SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify", "uri:spotify:"],
        linux: &["bin:spotify", "flatpak:com.spotify.Client", "bin:/snap/bin/spotify"],
        processes: &["spotify"],
    },
    AppSpec {
        id: "discord",
        display: "Discord",
        aliases: &["discord", "discort", "diskord"],
        windows: &["path:%LOCALAPPDATA%\\Discord\\Update.exe|--processStart Discord.exe", "uri:discord://", "exe:discord.exe"],
        linux: &["bin:discord", "flatpak:com.discordapp.Discord", "bin:/snap/bin/discord"],
        processes: &["discord"],
    },
    AppSpec {
        id: "chrome",
        display: "Google Chrome",
        aliases: &["chrome", "google chrome", "chrom"],
        windows: &["exe:chrome.exe"],
        linux: &["bin:google-chrome", "bin:google-chrome-stable", "bin:chromium", "bin:chromium-browser", "flatpak:com.google.Chrome"],
        processes: &["chrome", "google-chrome", "chromium", "chromium-browser"],
    },
    AppSpec {
        id: "firefox",
        display: "Firefox",
        aliases: &["firefox", "mozilla firefox", "mozilla"],
        windows: &["exe:firefox.exe"],
        linux: &["bin:firefox", "flatpak:org.mozilla.firefox"],
        processes: &["firefox", "firefox-bin"],
    },
    AppSpec {
        id: "brave",
        display: "Brave",
        aliases: &["brave", "brave browser"],
        windows: &["exe:brave.exe", "path:%LOCALAPPDATA%\\BraveSoftware\\Brave-Browser\\Application\\brave.exe"],
        linux: &["bin:brave-browser", "bin:brave", "flatpak:com.brave.Browser"],
        processes: &["brave", "brave-browser"],
    },
    AppSpec {
        id: "edge",
        display: "Microsoft Edge",
        aliases: &["edge", "microsoft edge", "ms edge"],
        windows: &["exe:msedge.exe"],
        linux: &["bin:microsoft-edge", "bin:microsoft-edge-stable"],
        processes: &["msedge", "microsoft-edge"],
    },
    AppSpec {
        id: "browser",
        display: "your browser",
        aliases: &[
            "browser", "web browser", "internet", "the internet", "webbrowser", "web", "the web", "default browser",
            "standard browser", "standardbrowser", "internet browser", "browser window", "google", "my browser", "brauser",
            "browsers",
        ],
        // Opened through the OS default-browser handler (`open_default_browser`), never as a
        // literal browser.exe.
        windows: &["url:https://www.google.com"],
        linux: &["url:https://www.google.com"],
        processes: &["chrome", "msedge", "firefox", "brave", "chromium", "opera", "vivaldi"],
    },
    AppSpec {
        id: "vscode",
        display: "Visual Studio Code",
        aliases: &["vscode", "vs code", "visual studio code", "code", "vs-code", "v s code"],
        windows: &[
            "path:%LOCALAPPDATA%\\Programs\\Microsoft VS Code\\Code.exe",
            "path:%ProgramFiles%\\Microsoft VS Code\\Code.exe",
            "exe:code.exe",
        ],
        linux: &["bin:code", "bin:codium", "flatpak:com.visualstudio.code", "bin:/snap/bin/code"],
        processes: &["code", "codium"],
    },
    AppSpec {
        id: "steam",
        display: "Steam",
        aliases: &["steam"],
        windows: &["uri:steam://open/main", "path:%ProgramFiles(x86)%\\Steam\\steam.exe"],
        linux: &["bin:steam", "flatpak:com.valvesoftware.Steam"],
        processes: &["steam", "steamwebhelper"],
    },
    AppSpec {
        id: "explorer",
        display: "File Explorer",
        aliases: &["explorer", "file explorer", "files", "file manager", "dateien", "datei explorer", "dateiexplorer", "explorer fenster"],
        windows: &["exe:explorer.exe"],
        linux: &["xdg:~", "bin:nautilus", "bin:dolphin", "bin:thunar", "bin:pcmanfm", "bin:nemo"],
        processes: &["nautilus", "dolphin", "thunar", "pcmanfm", "nemo"],
    },
    AppSpec {
        id: "terminal",
        display: "the terminal",
        aliases: &["terminal", "windows terminal", "konsole", "console", "kommandozeile", "shell"],
        windows: &["exe:wt.exe", "exe:powershell.exe"],
        linux: &[
            "bin:x-terminal-emulator", "bin:kitty", "bin:alacritty", "bin:foot", "bin:wezterm",
            "bin:gnome-terminal", "bin:konsole", "bin:xfce4-terminal", "bin:lxterminal", "bin:xterm",
        ],
        processes: &["windowsterminal", "wt", "kitty", "alacritty", "foot", "wezterm-gui", "gnome-terminal-server", "konsole", "xfce4-terminal", "lxterminal", "xterm"],
    },
    AppSpec {
        id: "powershell",
        display: "PowerShell",
        aliases: &["powershell", "power shell"],
        windows: &["exe:pwsh.exe", "exe:powershell.exe"],
        linux: &["bin:pwsh"],
        processes: &["powershell", "pwsh"],
    },
    AppSpec {
        id: "cmd",
        display: "the command prompt",
        aliases: &["cmd", "command prompt", "eingabeaufforderung"],
        windows: &["exe:cmd.exe"],
        linux: &[],
        processes: &["cmd"],
    },
    AppSpec {
        id: "calculator",
        display: "the calculator",
        aliases: &["calculator", "calc", "rechner", "taschenrechner"],
        windows: &["exe:calc.exe"],
        linux: &["bin:gnome-calculator", "bin:kcalc", "bin:galculator", "bin:qalculate-gtk", "bin:xcalc"],
        processes: &["calculatorapp", "calculator", "calc", "gnome-calculator", "kcalc", "galculator"],
    },
    AppSpec {
        id: "notepad",
        display: "Notepad",
        aliases: &["notepad", "editor", "text editor", "texteditor"],
        windows: &["exe:notepad.exe"],
        linux: &["bin:gnome-text-editor", "bin:gedit", "bin:kate", "bin:mousepad", "bin:xed"],
        processes: &["notepad", "gnome-text-editor", "gedit", "kate", "mousepad", "xed"],
    },
    AppSpec {
        id: "taskmanager",
        display: "the task manager",
        aliases: &["task manager", "taskmanager", "task-manager", "taskmgr", "system monitor"],
        windows: &["exe:taskmgr.exe"],
        linux: &["bin:gnome-system-monitor", "bin:plasma-systemmonitor", "bin:ksysguard"],
        processes: &["taskmgr", "gnome-system-monitor", "plasma-systemmonitor"],
    },
    AppSpec {
        id: "settings",
        display: "Settings",
        aliases: &["settings", "windows settings", "einstellungen", "system settings"],
        windows: &["uri:ms-settings:"],
        linux: &["bin:gnome-control-center", "bin:systemsettings", "bin:xfce4-settings-manager"],
        processes: &["systemsettings", "gnome-control-center"],
    },
    // Settings' own pages (Sir, 2026-11-22: "open the sound settings" opened Settings' start).
    AppSpec {
        id: "sound_settings",
        display: "the sound settings",
        aliases: &["sound settings", "audio settings", "soundeinstellungen", "audioeinstellungen", "toneinstellungen"],
        windows: &["uri:ms-settings:sound"],
        linux: &["bin:systemsettings|kcm_pulseaudio"],
        processes: &["systemsettings"],
    },
    AppSpec {
        id: "display_settings",
        display: "the display settings",
        aliases: &["display settings", "screen settings", "monitor settings", "anzeigeeinstellungen", "bildschirmeinstellungen"],
        windows: &["uri:ms-settings:display"],
        linux: &["bin:systemsettings|kcm_kscreen"],
        processes: &["systemsettings"],
    },
    AppSpec {
        id: "bluetooth_settings",
        display: "the Bluetooth settings",
        aliases: &["bluetooth settings", "bluetooth einstellungen", "bluetootheinstellungen"],
        windows: &["uri:ms-settings:bluetooth"],
        linux: &["bin:systemsettings|kcm_bluetooth"],
        processes: &["systemsettings"],
    },
    AppSpec {
        id: "network_settings",
        display: "the network settings",
        aliases: &["network settings", "wifi settings", "wi-fi settings", "wlan settings", "internet settings", "netzwerkeinstellungen", "wlan einstellungen"],
        windows: &["uri:ms-settings:network-wifi"],
        linux: &["bin:systemsettings|kcm_networkmanagement"],
        processes: &["systemsettings"],
    },
    AppSpec {
        id: "paint",
        display: "Paint",
        aliases: &["paint", "mspaint", "ms paint"],
        windows: &["exe:mspaint.exe"],
        linux: &["bin:pinta", "bin:kolourpaint"],
        processes: &["mspaint", "pinta", "kolourpaint"],
    },
    AppSpec {
        id: "obs",
        display: "OBS Studio",
        aliases: &["obs", "obs studio"],
        windows: &["path:%ProgramFiles%\\obs-studio\\bin\\64bit\\obs64.exe"],
        linux: &["bin:obs", "flatpak:com.obsproject.Studio"],
        processes: &["obs64", "obs"],
    },
];

/// Never terminated, whatever the request.
const PROTECTED: &[&str] = &[
    "system", "idle", "registry", "smss", "csrss", "wininit", "winlogon", "services", "lsass", "svchost",
    "dwm", "explorer", "fontdrvhost", "sihost", "taskhostw", "ctfmon", "conhost", "runtimebroker",
    "jai", "ollama", "ollama app", "ollama_llama_server", "systemd", "init", "kthreadd", "xorg",
    "xwayland", "gnome-shell", "plasmashell", "kwin_x11", "kwin_wayland", "hyprland", "sway",
    "pipewire", "wireplumber", "pulseaudio", "dbus-daemon", "sshd", "login",
];

/// The part an app plays for him, and the apps that may play it: (process name, said name). The
/// one running now plays it, else the first.
const ROLES: &[(&str, &[(&str, &str)])] = &[
    ("music", &[("sonora", "Sonora"), ("spotify", "Spotify"), ("musicbee", "MusicBee"), ("foobar2000", "foobar2000"), ("vlc", "VLC")]),
    ("chat", &[("discord", "Discord"), ("whatsapp", "WhatsApp"), ("telegram", "Telegram"), ("signal", "Signal"), ("ms-teams", "Teams")]),
    ("editor", &[("code", "VS Code"), ("notepad++", "Notepad++"), ("sublime_text", "Sublime Text"), ("notepad", "Notepad")]),
];

/// "The music app", "die Musik-App", "my chat app", "den Messenger", "my editor", "the code
/// editor". ("The chat" and "the editor" alone may be his own blocks: not these.)
static ROLE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(?:the|my|die|den|der|meine?n?)\s+(?:(?P<music>music|musik)[\s-]?(?:app|player|programm)|(?P<chat>chat[\s-]?(?:app|programm)|messaging\s+app|messenger)|(?P<editor>code\s+editor|editor[\s-]?(?:app|programm))|(?P<my_editor>editor))\b").expect("role pattern")
});

/// Which app plays `role` given the processes running now (lower case, no ".exe").
fn role_pick(role: &str, running: &[String]) -> Option<&'static str> {
    let apps = ROLES.iter().find(|(r, _)| *r == role)?.1;
    apps.iter().find(|(process, _)| running.iter().any(|p| p == process)).or(apps.first()).map(|(_, said)| *said)
}

/// An order with the part an app plays put as that app's name: "open the music app" → "open
/// Sonora", "schließ den Messenger" → "schließ Discord".
pub fn with_roles(text: &str) -> std::borrow::Cow<'_, str> {
    if !ROLE.is_match(text) {
        return std::borrow::Cow::Borrowed(text);
    }
    #[cfg(windows)]
    let running: Vec<String> = crate::win32::app_windows().iter().map(|w| w.process.to_lowercase().trim_end_matches(".exe").to_string()).collect();
    // Linux: the program ids of Plasma's windows ("spotify", "org.kde.dolphin").
    #[cfg(not(windows))]
    let running: Vec<String> = crate::plasma::windows().iter().flat_map(|w| [w.app_id.to_lowercase(), w.resource_name.to_lowercase()]).collect();
    with_roles_among(text, &running)
}

fn with_roles_among<'a>(text: &'a str, running: &[String]) -> std::borrow::Cow<'a, str> {
    ROLE.replace_all(text, |c: &regex::Captures| {
        let role = ["music", "chat", "editor"].into_iter().find(|r| c.name(r).is_some()).or_else(|| (c.name("my_editor").is_some() && c[0].to_lowercase().starts_with("my")).then_some("editor"));
        role.and_then(|r| role_pick(r, running)).map_or_else(|| c[0].to_string(), str::to_string)
    })
}

pub fn normalize(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let cleaned: String = lower
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '.' { c } else { ' ' })
        .collect();
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    for filler in ["the", "app", "application", "program", "programm", "den", "die", "das", "mein", "my", "please", "bitte"] {
        if words.len() > 1 {
            words.retain(|w| *w != filler);
        }
    }
    words.join(" ").trim_end_matches(".exe").trim_matches('.').to_string()
}

pub fn find_spec(name: &str) -> Option<&'static AppSpec> {
    let q = normalize(name);
    if q.is_empty() {
        return None;
    }
    if let Some(spec) = APPS.iter().find(|s| s.id == q || s.aliases.contains(&q.as_str())) {
        return Some(spec);
    }
    // Mumbled names ("spotfy", "discorde"): fuzzy match against aliases.
    let q = q.as_str();
    APPS.iter()
        .flat_map(|s| s.aliases.iter().map(move |a| (s, jaro_winkler(q, a))))
        .filter(|(_, score)| *score >= 0.92)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, _)| s)
}

pub fn display_name(name: &str) -> String {
    if let Some(spec) = find_spec(name) {
        return spec.display.to_string();
    }
    normalize(name)
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn as_url(q: &str) -> Option<String> {
    let q = q.trim();
    if q.starts_with("http://") || q.starts_with("https://") {
        return Some(q.to_string());
    }
    let host = q.strip_prefix("www.").unwrap_or(q);
    let tlds = [".com", ".de", ".org", ".net", ".io", ".ai", ".dev", ".app", ".co", ".uk", ".tv", ".gg", ".eu", ".me"];
    let looks_like_domain = !host.contains(' ')
        && host.contains('.')
        && tlds.iter().any(|t| host.split('/').next().unwrap_or("").ends_with(t));
    looks_like_domain.then(|| format!("https://{q}"))
}

/// "downloads", "documents folder", "Bilder Ordner", … → the user's folder.
/// Ambiguous words (music, home, …) only count when "folder"/"Ordner" is said explicitly.
fn known_folder(q: &str) -> Option<PathBuf> {
    let markers = ["folder", "ordner", "directory", "verzeichnis"];
    let explicit = markers.iter().any(|m| q.contains(m));
    let word = q
        .split_whitespace()
        .filter(|w| !markers.contains(w) && !matches!(*w, "my" | "mein" | "meine" | "den" | "the"))
        .collect::<Vec<_>>()
        .join(" ");
    let (sub, unambiguous) = match word.as_str() {
        "downloads" | "download" => ("Downloads", true),
        "documents" | "dokumente" | "document" => ("Documents", true),
        "desktop" | "schreibtisch" => ("Desktop", true),
        "pictures" | "bilder" | "photos" | "fotos" => ("Pictures", false),
        "videos" | "video" => ("Videos", false),
        "music" | "musik" => ("Music", false),
        "home" | "user" | "benutzer" => ("", false),
        _ => return None,
    };
    if !explicit && !unambiguous {
        return None;
    }
    let home = home_dir()?;
    let path = if sub.is_empty() { home } else { home.join(sub) };
    path.exists().then_some(path)
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

pub(crate) fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let var = &after[..end];
                out.push_str(&std::env::var(var).unwrap_or_else(|_| format!("%{var}%")));
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    if let Some(stripped) = out.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(stripped).display().to_string();
        }
    }
    out
}

/// Opens an application, URL, folder or file. Returns a human-readable description.
pub async fn open(name: &str, args: &[String]) -> Result<String, String> {
    let query = normalize(name);
    if query.is_empty() {
        return Err("No application name was given.".into());
    }
    let label = display_name(name);

    if let Some(url) = as_url(name.trim()) {
        let via = open_default_browser(&url).await?;
        return Ok(format!("Opened {url} in the default browser ({via})."));
    }
    if wants_private_window(&query) {
        let browser = open_private_window().await?;
        return Ok(format!("Opened a private window of {browser}."));
    }
    if find_spec(&query).is_some_and(|s| s.id == "browser") {
        let via = open_default_browser(HOME_PAGE).await?;
        return Ok(format!("Opened your default web browser ({via})."));
    }
    if let Some(folder) = known_folder(&query) {
        let path = folder.display().to_string();
        launch_target(&format!("folder:{path}"), args).await?;
        return Ok(format!("Opened the folder {path}."));
    }
    let raw_path = expand_env(name.trim().trim_matches('"'));
    if (raw_path.contains('\\') || raw_path.contains('/')) && Path::new(&raw_path).exists() {
        launch_target(&format!("folder:{raw_path}"), args).await?;
        return Ok(format!("Opened {raw_path}."));
    }

    let mut candidates: Vec<String> = Vec::new();
    if let Some(spec) = find_spec(&query) {
        let list = if cfg!(windows) { spec.windows } else { spec.linux };
        candidates.extend(list.iter().map(|s| s.to_string()));
    }
    let token = query.replace(' ', "");
    if cfg!(windows) {
        candidates.push(format!("exe:{token}.exe"));
    } else {
        candidates.push(format!("bin:{token}"));
        candidates.push(format!("bin:{}", query.replace(' ', "-")));
    }

    let mut errors = Vec::new();
    for c in &candidates {
        match launch_target(c, args).await {
            Ok(()) => {
                info!("[TOOL] Launched '{label}' via {c}");
                return Ok(format!("Opened {label}."));
            }
            Err(e) => {
                debug!("[TOOL] Launch candidate {c} failed: {e}");
                errors.push(e);
            }
        }
    }

    // Installed-application index (Start Menu, App Paths, program folders / .desktop, $PATH).
    let indexed = {
        let q = query.clone();
        tokio::task::spawn_blocking(move || crate::appindex::lookup(&q)).await.ok().flatten()
    };
    if let Some(entry) = indexed {
        launch_entry(&entry, args).await?;
        info!("[TOOL] Launched '{}' from {} ({:?})", entry.name, entry.target.display(), entry.kind);
        return Ok(format!("Opened {}.", entry.name));
    }
    if let Some(entry) = search_installed(&query) {
        launch_installed(&entry).await?;
        info!("[TOOL] Launched '{}' from {}", entry.name, entry.location);
        return Ok(format!("Opened {}.", entry.name));
    }
    // A website by name ("open YouTube", "open Twitch") when no app of that name is installed.
    if let Some(site) = crate::sites::find(name.trim()) {
        let url = site.home_url();
        let via = open_default_browser(&url).await?;
        info!("[TOOL] '{label}' is the website {url}");
        return Ok(format!("Opened {} ({url}) in the default browser ({via}).", site.display));
    }

    #[cfg(windows)]
    if let Some((app_name, aumid)) = find_start_app(&query).await {
        launch_target(&format!("aumid:{aumid}"), args).await?;
        return Ok(format!("Opened {app_name}."));
    }
    // Nothing by that name: the closest installed name is suggested, not started ("open teams" → Steam).
    let q = query.clone();
    let closest = tokio::task::spawn_blocking(move || crate::appindex::nearest(&q)).await.ok().flatten();
    Err(not_found(&label, closest.as_ref().map(|e| e.name.as_str())))
}

/// The answer when no app has that name: the closest one only as a suggestion, never launched;
/// it starts when Sir says its name himself (Sir, 2026-11-15).
pub(crate) fn not_found(label: &str, closest: Option<&str>) -> String {
    match closest {
        Some(name) => format!("No {label} here, Sir. The closest is {name}.\nAsk Sir which app he meant; open one only when he names it himself."),
        None => format!("I could not find an application called '{label}' on this machine."),
    }
}

const HOME_PAGE: &str = "https://www.google.com";

/// Opens `url` with the OS default browser, falling back through the other OS handlers and
/// finally any installed browser. Returns how it was opened.
pub async fn open_default_browser(url: &str) -> Result<String, String> {
    let url = url.to_string();
    #[cfg(windows)]
    {
        let u = url.clone();
        let shell = crate::win32::call(move || crate::win32::shell_execute(&u, None, None)).await;
        match shell {
            Ok(Ok(())) => return Ok("default browser".into()),
            Ok(Err(e)) | Err(e) => debug!("[TOOL] ShellExecute({url}) failed: {e}"),
        }
        {
            use std::os::windows::process::CommandExt;
            let mut rundll = std::process::Command::new("rundll32");
            rundll.args(["url.dll,FileProtocolHandler", &url]);
            // `start` needs its empty title argument quoted exactly like this.
            let mut start = std::process::Command::new("cmd");
            start.raw_arg(format!("/C start \"\" \"{}\"", url.replace('"', "%22")));
            for (label, mut cmd) in [("url.dll handler", rundll), ("cmd start", start)] {
                crate::shell::hide_console_std(&mut cmd);
                if cmd.spawn().is_ok() {
                    return Ok(label.to_string());
                }
            }
        }
        for exe in ["chrome.exe", "msedge.exe", "brave.exe", "firefox.exe", "opera.exe"] {
            let (e, u) = (exe.to_string(), url.clone());
            if let Ok(Ok(())) = crate::win32::call(move || crate::win32::shell_execute(&e, Some(&u), None)).await {
                return Ok(exe.trim_end_matches(".exe").to_string());
            }
        }
        Err("No web browser could be started (no default browser is registered).".into())
    }
    #[cfg(not(windows))]
    {
        tokio::task::spawn_blocking(move || {
            for program in ["xdg-open", "gio", "sensible-browser", "x-www-browser", "firefox", "chromium", "chromium-browser", "google-chrome", "brave-browser"] {
                let Some(bin) = which(program) else { continue };
                let args = if program == "gio" { vec!["open".to_string(), url.clone()] } else { vec![url.clone()] };
                if spawn_detached(&bin.display().to_string(), &args).is_ok() {
                    return Ok(program.to_string());
                }
            }
            Err("No web browser could be started (install xdg-utils or a browser).".to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

/// What "activate goon mode" opens (`intents`): a private window of Sir's browser.
pub const PRIVATE_WINDOW: &str = "private window";

/// "A private window", "an incognito tab", "inprivate", "privates Fenster": no app of that name
/// ("open private internet access" is one).
fn wants_private_window(query: &str) -> bool {
    let words: Vec<&str> = query.split_whitespace().collect();
    let private = words.iter().any(|w| ["privat", "incognito", "inprivate", "inkognito"].iter().any(|p| w.starts_with(p)));
    private && (words.len() == 1 || words.iter().any(|w| ["window", "tab", "browser", "fenster", "mode", "modus", "session", "browsing"].contains(w)))
}

/// The browser a ProgId (Windows: "ChromeHTML", "MSEdgeHTM", "FirefoxURL-…") or a desktop file
/// (Linux: "firefox.desktop") names, with its switch for a private window: (program, switch,
/// name). Edge on Windows and Firefox on Linux where none is known.
fn private_browser(id: &str) -> (&'static str, &'static str, &'static str) {
    let id = id.to_lowercase();
    let program = |windows: &'static str, linux: &'static str| if cfg!(windows) { windows } else { linux };
    if id.contains("firefox") || (!cfg!(windows) && !["brave", "chrome", "chromium", "opera", "vivaldi", "edge"].iter().any(|b| id.contains(b))) {
        (program("firefox.exe", "firefox"), "-private-window", "Firefox")
    } else if id.contains("brave") {
        (program("brave.exe", "brave-browser"), "--incognito", "Brave")
    } else if id.contains("chromium") {
        (program("chrome.exe", "chromium"), "--incognito", "Chromium")
    } else if id.contains("chrome") {
        (program("chrome.exe", "google-chrome"), "--incognito", "Chrome")
    } else if id.contains("opera") {
        (program("opera.exe", "opera"), "--private", "Opera")
    } else if id.contains("vivaldi") {
        (program("vivaldi.exe", "vivaldi"), "--incognito", "Vivaldi")
    } else {
        (program("msedge.exe", "microsoft-edge"), "--inprivate", "Edge")
    }
}

/// Opens a private window of Sir's default browser (Sir, 2026-11-21: "when you say activate
/// goon mode than he should open a private browser window"). Which browser it was.
pub async fn open_private_window() -> Result<String, String> {
    #[cfg(windows)]
    let id = crate::kit_pc::reg_text(windows::Win32::System::Registry::HKEY_CURRENT_USER, r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice", "ProgId").unwrap_or_default();
    #[cfg(not(windows))]
    let id = std::process::Command::new("xdg-settings").args(["get", "default-web-browser"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let (program, switch, name) = private_browser(&id);
    #[cfg(windows)]
    match crate::win32::call(move || crate::win32::shell_execute(program, Some(switch), None)).await {
        Ok(Ok(())) => Ok(name.to_string()),
        Ok(Err(e)) | Err(e) => Err(format!("{name} did not open a private window: {e}")),
    }
    #[cfg(not(windows))]
    {
        let bin = which(program).ok_or_else(|| format!("{name} is not installed."))?;
        spawn_detached(&bin.display().to_string(), &[switch.to_string()]).map(|()| name.to_string())
    }
}

/// Launches an entry of the installed-application index.
async fn launch_entry(entry: &crate::appindex::AppEntry, args: &[String]) -> Result<(), String> {
    use crate::appindex::EntryKind;
    let target = entry.target.display().to_string();
    match entry.kind {
        #[cfg(windows)]
        EntryKind::Shortcut | EntryKind::AppPath | EntryKind::Program => {
            let params = windows_command_line(args);
            let dir = matches!(entry.kind, EntryKind::Program | EntryKind::AppPath)
                .then(|| entry.target.parent().map(|p| p.display().to_string()))
                .flatten();
            crate::win32::call(move || crate::win32::shell_execute(&target, Some(&params), dir.as_deref())).await?
        }
        EntryKind::Desktop => launch_installed(&InstalledApp { name: entry.name.clone(), location: target }).await,
        #[cfg(not(windows))]
        EntryKind::PathBin | EntryKind::Program | EntryKind::AppPath | EntryKind::Shortcut => {
            let args = args.to_vec();
            tokio::task::spawn_blocking(move || spawn_detached(&target, &args)).await.map_err(|e| e.to_string())?
        }
        #[cfg(windows)]
        EntryKind::PathBin => crate::win32::call(move || crate::win32::shell_execute(&target, None, None)).await?,
    }
}

async fn launch_target(target: &str, args: &[String]) -> Result<(), String> {
    let (kind, value) = target.split_once(':').ok_or_else(|| format!("bad target {target}"))?;
    let kind = kind.to_string();
    let value = value.to_string();

    #[cfg(windows)]
    {
        use crate::win32;
        let extra = windows_command_line(args);
        let (file, params, dir): (String, String, Option<String>) = match kind.as_str() {
            "uri" | "url" => (value, String::new(), None),
            "aumid" => (format!("shell:AppsFolder\\{value}"), String::new(), None),
            "exe" | "folder" => (value, extra, None),
            "path" => {
                let (path, path_args) = value.split_once('|').unwrap_or((value.as_str(), ""));
                let path = expand_env(path);
                if !Path::new(&path).exists() {
                    return Err(format!("{path} does not exist"));
                }
                let dir = Path::new(&path).parent().map(|p| p.display().to_string());
                (path, format!("{path_args} {extra}").trim().to_string(), dir)
            }
            other => return Err(format!("candidate type {other} is not supported on Windows")),
        };
        win32::call(move || win32::shell_execute(&file, Some(&params), dir.as_deref())).await?
    }

    #[cfg(not(windows))]
    {
        let extra_args: Vec<String> = args.to_vec();
        tokio::task::spawn_blocking(move || match kind.as_str() {
            "url" | "folder" | "xdg" => spawn_detached("xdg-open", &[expand_env(&value)]),
            "bin" => {
                // ("bin:systemsettings|kcm_pulseaudio": the program, then its own arguments.)
                let (bin, own) = value.split_once('|').unwrap_or((value.as_str(), ""));
                let bin = expand_env(bin);
                let resolved = which(&bin).ok_or_else(|| format!("{bin} is not installed"))?;
                let args: Vec<String> = own.split_whitespace().map(str::to_string).chain(extra_args).collect();
                spawn_detached(&resolved.display().to_string(), &args)
            }
            "flatpak" => {
                which("flatpak").ok_or("flatpak is not installed")?;
                let ok = std::process::Command::new("flatpak")
                    .args(["info", &value])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !ok {
                    return Err(format!("flatpak {value} is not installed"));
                }
                let mut a = vec!["run".to_string(), value.clone()];
                a.extend(extra_args);
                spawn_detached("flatpak", &a)
            }
            other => Err(format!("candidate type {other} is not supported on Linux")),
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

/// Arguments joined for ShellExecute: ones with spaces (paths like `D:\Code\my project\x.rs`)
/// are quoted, already-quoted ones are kept as they are.
#[cfg(windows)]
fn windows_command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty() || (a.contains([' ', '\t']) && !a.starts_with('"')) {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// PATH lookup (absolute paths are checked directly).
pub fn which(bin: &str) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.is_absolute() {
        return p.exists().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            ["exe", "cmd", "bat"]
                .iter()
                .map(|ext| dir.join(format!("{bin}.{ext}")))
                .find(|c| c.is_file())
        } else {
            None
        }
    })
}

#[cfg(not(windows))]
pub(crate) fn spawn_detached(program: &str, args: &[String]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    // Under Hyprland let the compositor own the process (correct workspace + environment).
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() && which("hyprctl").is_some() {
        let cmdline = std::iter::once(program.to_string())
            .chain(args.iter().map(|a| shell_quote(a)))
            .collect::<Vec<_>>()
            .join(" ");
        let status = Command::new("hyprctl")
            .args(["dispatch", "exec", &cmdline])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        return if status.success() { Ok(()) } else { Err("hyprctl exec failed".into()) };
    }

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    // A launcher that exits immediately with an error means "not really there".
    std::thread::sleep(Duration::from_millis(150));
    match child.try_wait() {
        Ok(Some(status)) if !status.success() => Err(format!("{program} exited with {status}")),
        _ => Ok(()),
    }
}

#[cfg(not(windows))]
fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || "-_./:=".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[derive(Debug, Clone)]
pub struct InstalledApp {
    pub name: String,
    /// Shortcut / .desktop file path.
    pub location: String,
}

fn score_name(query: &str, candidate: &str) -> f64 {
    let c = normalize(candidate);
    if c.is_empty() {
        return 0.0;
    }
    if c == query {
        return 1.0;
    }
    if c.starts_with(&format!("{query} ")) || c.split_whitespace().any(|w| w == query) {
        return 0.95;
    }
    jaro_winkler(query, &c)
}

/// Start-Menu shortcuts (Windows) or `.desktop` entries (Linux) whose name matches `query`.
pub fn search_installed(query: &str) -> Option<InstalledApp> {
    let mut best: Option<(f64, InstalledApp)> = None;
    let mut consider = |score: f64, app: InstalledApp| {
        if score >= 0.88 && best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, app));
        }
    };
    let skip = ["uninstall", "deinstall", "readme", "help", "website", "documentation", "release notes", "license"];

    #[cfg(windows)]
    {
        let roots: Vec<PathBuf> = ["APPDATA", "ProgramData"]
            .iter()
            .filter_map(std::env::var_os)
            .map(|base| PathBuf::from(base).join("Microsoft\\Windows\\Start Menu\\Programs"))
            .collect();
        for root in roots {
            walk(&root, 4, &mut |path| {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                if ext != "lnk" && ext != "url" {
                    return;
                }
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let lower = stem.to_lowercase();
                if skip.iter().any(|s| lower.contains(s)) {
                    return;
                }
                consider(
                    score_name(query, stem),
                    InstalledApp { name: stem.to_string(), location: path.display().to_string() },
                );
            });
        }
    }

    #[cfg(not(windows))]
    {
        let mut roots = vec![
            PathBuf::from("/usr/share/applications"),
            PathBuf::from("/usr/local/share/applications"),
            PathBuf::from("/var/lib/flatpak/exports/share/applications"),
            PathBuf::from("/var/lib/snapd/desktop/applications"),
        ];
        if let Some(home) = home_dir() {
            roots.push(home.join(".local/share/applications"));
            roots.push(home.join(".local/share/flatpak/exports/share/applications"));
        }
        for root in roots {
            walk(&root, 2, &mut |path| {
                if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                    return;
                }
                let Ok(text) = std::fs::read_to_string(path) else { return };
                if text.lines().any(|l| matches!(l.trim(), "NoDisplay=true" | "Hidden=true")) {
                    return;
                }
                let name = text
                    .lines()
                    .find_map(|l| l.strip_prefix("Name="))
                    .unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or(""))
                    .to_string();
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let score = score_name(query, &name).max(score_name(query, &stem));
                if skip.iter().any(|s| name.to_lowercase().contains(s)) {
                    return;
                }
                consider(score, InstalledApp { name, location: path.display().to_string() });
            });
        }
    }

    best.map(|(_, app)| app)
}

fn walk(dir: &Path, depth: usize, visit: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                walk(&path, depth - 1, visit);
            }
        } else {
            visit(&path);
        }
    }
}

async fn launch_installed(app: &InstalledApp) -> Result<(), String> {
    #[cfg(windows)]
    {
        let location = app.location.clone();
        crate::win32::call(move || crate::win32::shell_execute(&location, None, None)).await?
    }
    #[cfg(not(windows))]
    {
        let location = app.location.clone();
        tokio::task::spawn_blocking(move || {
            let id = Path::new(&location)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if which("gtk-launch").is_some() {
                return spawn_detached("gtk-launch", &[id]);
            }
            let text = std::fs::read_to_string(&location).map_err(|e| e.to_string())?;
            let exec = text
                .lines()
                .find_map(|l| l.strip_prefix("Exec="))
                .ok_or("desktop entry has no Exec line")?;
            let parts = exec_args(exec);
            let (program, rest) = parts.split_first().ok_or("empty Exec line")?;
            spawn_detached(program, rest)
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

/// The words of a `.desktop` Exec line: quotes and `\` escapes undone, field codes (`%u`, `%F` …) dropped, `%%` a percent sign.
#[cfg(not(windows))]
fn exec_args(exec: &str) -> Vec<String> {
    let (mut out, mut word, mut quoted, mut started) = (Vec::new(), String::new(), false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => (quoted, started) = (!quoted, true),
            '\\' if quoted => word.extend(chars.next()),
            ' ' | '\t' if !quoted => {
                if started {
                    out.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(word);
    }
    out.iter().filter_map(|w| if w.len() == 2 && w.starts_with('%') && w != "%%" { None } else { Some(w.replace("%%", "%")) }).collect()
}

/// UWP / Store apps via `Get-StartApps` (slow, last resort).
#[cfg(windows)]
async fn find_start_app(query: &str) -> Option<(String, String)> {
    let out = crate::shell::run_helper(
        Path::new("powershell"),
        [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; Get-StartApps | ForEach-Object { \"$($_.Name)`t$($_.AppID)\" }",
        ],
        None,
        &[],
        Duration::from_secs(8),
    )
    .await
    .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(name, id)| (score_name(query, name), name.trim().to_string(), id.trim().to_string()))
        .filter(|(s, _, _)| *s >= 0.88)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, name, id)| (name, id))
}

fn own_process_tree(sys: &System) -> HashSet<Pid> {
    let mut set = HashSet::new();
    let mut current = sysinfo::get_current_pid().ok();
    while let Some(pid) = current {
        if !set.insert(pid) {
            break;
        }
        current = sys.process(pid).and_then(|p| p.parent());
    }
    set
}

fn proc_stem(name: &std::ffi::OsStr) -> String {
    name.to_string_lossy().to_lowercase().trim_end_matches(".exe").to_string()
}

/// The process names a program goes by: its spec's, or what it was called and what is installed
/// under that name.
async fn process_names(query: &str) -> Vec<String> {
    let mut wanted: Vec<String> = find_spec(query).map(|s| s.processes.iter().map(|p| p.to_string()).collect()).unwrap_or_default();
    if wanted.is_empty() {
        wanted.push(query.replace(' ', ""));
        wanted.push(query.replace(' ', "-"));
        // "Close Visual Studio Code" → the installed app's executable ("code").
        let q = query.to_string();
        if let Some(entry) = tokio::task::spawn_blocking(move || crate::appindex::lookup(&q)).await.ok().flatten() {
            if entry.target.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) || cfg!(not(windows)) {
                if let Some(stem) = entry.target.file_stem().map(|s| s.to_string_lossy().to_lowercase()) {
                    if !wanted.contains(&stem) {
                        wanted.push(stem);
                    }
                }
            }
        }
    }
    wanted
}

/// "Is Firefox open?": a process of that name, whatever its windows do. JAI's own tree (his
/// runtime, his tunnels) does not count.
pub async fn is_running(name: &str) -> bool {
    let query = normalize(name);
    if query.is_empty() {
        return false;
    }
    let mut wanted = process_names(&query).await;
    // (Minecraft runs as Java, as the mixer and the screen time know it too.)
    if query == "minecraft" {
        wanted.push("javaw".to_string());
    }
    tokio::task::spawn_blocking(move || {
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new());
        let own = own_process_tree(&sys);
        sys.processes().iter().filter(|(pid, _)| !own.contains(pid)).any(|(_, process)| {
            let stem = proc_stem(process.name());
            wanted.iter().any(|w| stem == *w || (w.len() >= 4 && stem.starts_with(w.as_str())))
        })
    })
    .await
    .unwrap_or(false)
}

/// Closes an application: graceful first, then (if `force`) terminate survivors.
pub async fn close(name: &str, force: bool) -> Result<String, String> {
    let query = normalize(name);
    if query.is_empty() {
        return Err("No application name was given.".into());
    }
    let display = display_name(name);
    #[cfg(windows)]
    let spec = find_spec(&query);

    #[cfg(windows)]
    if spec.map(|s| s.id) == Some("explorer") {
        let closed = crate::win32::call(|| {
            crate::win32::app_windows()
                .into_iter()
                .filter(|w| w.class == "CabinetWClass")
                .filter(|w| crate::win32::close_window(w.hwnd))
                .count()
        })
        .await?;
        return if closed > 0 {
            Ok(format!("Closed {closed} File Explorer window(s)."))
        } else {
            Err("No File Explorer windows are open.".into())
        };
    }

    let wanted = process_names(&query).await;
    if wanted.iter().any(|w| PROTECTED.contains(&w.as_str())) {
        return Err(format!("{display} is a protected system process; I will not terminate it."));
    }

    let wanted_clone = wanted.clone();
    let targets = tokio::task::spawn_blocking(move || {
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new());
        let protected_tree = own_process_tree(&sys);
        let candidates: Vec<(Pid, String)> = sys
            .processes()
            .iter()
            .map(|(pid, p)| (*pid, proc_stem(p.name())))
            .filter(|(pid, stem)| !protected_tree.contains(pid) && !PROTECTED.contains(&stem.as_str()))
            .collect();
        let exact: Vec<Pid> = candidates
            .iter()
            .filter(|(_, stem)| wanted_clone.iter().any(|w| stem == w))
            .map(|(pid, _)| *pid)
            .collect();
        if !exact.is_empty() {
            return exact;
        }
        candidates
            .iter()
            .filter(|(_, stem)| wanted_clone.iter().any(|w| w.len() >= 4 && stem.starts_with(w.as_str())))
            .map(|(pid, _)| *pid)
            .collect()
    })
    .await
    .map_err(|e| e.to_string())?;

    if targets.is_empty() {
        return Err(format!("{display} doesn't appear to be running."));
    }
    let total = targets.len();

    // 1) Graceful: WM_CLOSE to the app's windows (Windows) / SIGTERM (Unix).
    #[cfg(windows)]
    {
        let pids: HashSet<u32> = targets.iter().map(|p| p.as_u32()).collect();
        let _ = crate::win32::call(move || {
            for w in crate::win32::app_windows().into_iter().filter(|w| pids.contains(&w.pid)) {
                crate::win32::close_window(w.hwnd);
            }
        })
        .await;
    }
    #[cfg(not(windows))]
    {
        let t = targets.clone();
        let _ = tokio::task::spawn_blocking(move || {
            // The windows are asked as their close button would (a save dialog may open); what has none gets SIGTERM.
            let windows = crate::plasma::windows();
            let mut sys = System::new();
            sys.refresh_processes_specifics(ProcessesToUpdate::Some(&t), ProcessRefreshKind::new());
            for pid in &t {
                let mine: Vec<_> = windows.iter().filter(|w| w.pid == pid.as_u32() && !w.is(crate::plasma::state::SKIP_TASKBAR)).collect();
                if !mine.is_empty() {
                    mine.iter().for_each(|w| drop(crate::plasma::close(&w.uuid)));
                } else if let Some(p) = sys.process(*pid) {
                    let _ = p.kill_with(sysinfo::Signal::Term);
                }
            }
        })
        .await;
    }

    // 2) Wait up to 2.5 s, then terminate survivors (many apps just hide to the tray).
    let started = Instant::now();
    let survivors = loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let t = targets.clone();
        let alive: Vec<Pid> = tokio::task::spawn_blocking(move || {
            let mut sys = System::new();
            sys.refresh_processes_specifics(ProcessesToUpdate::Some(&t), ProcessRefreshKind::new());
            t.into_iter().filter(|pid| sys.process(*pid).is_some()).collect()
        })
        .await
        .unwrap_or_default();
        if alive.is_empty() || started.elapsed() > Duration::from_millis(2500) {
            break alive;
        }
    };

    if survivors.is_empty() {
        return Ok(format!("Closed {display}."));
    }
    if !force {
        return Ok(format!(
            "Asked {display} to close; {} process(es) are still running (it may have minimised to the tray).",
            survivors.len()
        ));
    }
    let killed = tokio::task::spawn_blocking(move || {
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&survivors), ProcessRefreshKind::new());
        survivors
            .iter()
            .filter(|pid| sys.process(**pid).map(|p| p.kill()).unwrap_or(false))
            .count()
    })
    .await
    .unwrap_or(0);
    Ok(format!("Closed {display} ({total} process(es), {killed} terminated forcefully)."))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goon mode (Sir, 2026-11-21): a private window of whatever browser is his default, by that
    /// browser's own switch; "Private Internet Access" stays an app.
    #[test]
    fn a_private_window_opens_in_the_default_browser() {
        for query in [PRIVATE_WINDOW, "incognito tab", "inprivate", "privates fenster", "private browser window", "incognito mode"] {
            assert!(wants_private_window(query), "{query}");
        }
        for query in ["private internet access", "browser", "private", "my private notes"] {
            assert_eq!(wants_private_window(query), query == "private", "{query}");
        }
        if cfg!(windows) {
            assert_eq!(private_browser("MSEdgeHTM"), ("msedge.exe", "--inprivate", "Edge"));
            assert_eq!(private_browser("ChromeHTML"), ("chrome.exe", "--incognito", "Chrome"));
            assert_eq!(private_browser("FirefoxURL-308046B0AF4A39CB"), ("firefox.exe", "-private-window", "Firefox"));
            assert_eq!(private_browser("BraveHTML"), ("brave.exe", "--incognito", "Brave"));
            assert_eq!(private_browser("OperaGXStable").1, "--private");
            assert_eq!(private_browser("").2, "Edge", "none registered: Edge, which Windows has");
        } else {
            assert_eq!(private_browser("firefox.desktop"), ("firefox", "-private-window", "Firefox"));
            assert_eq!(private_browser("google-chrome.desktop"), ("google-chrome", "--incognito", "Chrome"));
            assert_eq!(private_browser("").2, "Firefox");
        }
    }

    #[test]
    fn the_part_an_app_plays_is_that_app() {
        let none: Vec<String> = Vec::new();
        let spotify_on = vec!["spotify".to_string(), "explorer".to_string()];
        assert_eq!(with_roles_among("open the music app", &none), "open Sonora");
        assert_eq!(with_roles_among("open the music app", &spotify_on), "open Spotify", "the one running plays it");
        assert_eq!(with_roles_among("Schließ die Musik-App", &none), "Schließ Sonora");
        assert_eq!(with_roles_among("close my chat app", &none), "close Discord");
        assert_eq!(with_roles_among("öffne den Messenger", &["whatsapp".to_string()]), "öffne WhatsApp");
        assert_eq!(with_roles_among("move my editor to the left", &none), "move VS Code to the left");
        assert_eq!(with_roles_among("focus the code editor", &none), "focus VS Code");
        // His own blocks and plain words stay.
        for said in ["close the editor", "open the chat", "play some music", "the music is too loud"] {
            assert_eq!(with_roles_among(said, &none), said);
        }
    }

    #[test]
    fn resolves_aliases_fuzzy_names_and_fillers() {
        assert_eq!(find_spec("Spotify").map(|s| s.id), Some("spotify"));
        assert_eq!(find_spec("the spotify app").map(|s| s.id), Some("spotify"));
        assert_eq!(find_spec("VS Code").map(|s| s.id), Some("vscode"));
        assert_eq!(find_spec("Taschenrechner").map(|s| s.id), Some("calculator"));
        assert_eq!(find_spec("spotfy").map(|s| s.id), Some("spotify"));
        assert_eq!(find_spec("Datei Explorer").map(|s| s.id), Some("explorer"));
        assert!(find_spec("blender").is_none());
        for browser in ["my browser", "browser", "web", "the internet", "default browser", "Browser."] {
            assert_eq!(find_spec(browser).map(|s| s.id), Some("browser"), "{browser}");
        }
        assert_eq!(display_name("discord"), "Discord");
        assert_eq!(display_name("blender"), "Blender");
    }

    #[test]
    fn detects_urls_and_expands_env() {
        assert_eq!(as_url("youtube.com").as_deref(), Some("https://youtube.com"));
        assert_eq!(as_url("https://github.com/x").as_deref(), Some("https://github.com/x"));
        assert!(as_url("visual studio code").is_none());
        std::env::set_var("JAI_TEST_DIR", "C:\\Tools");
        assert_eq!(expand_env("%JAI_TEST_DIR%\\app.exe"), "C:\\Tools\\app.exe");
        assert_eq!(expand_env("%NOPE_NOT_SET%\\x"), "%NOPE_NOT_SET%\\x");
    }

    #[cfg(not(windows))]
    #[test]
    fn desktop_exec_lines_lose_their_field_codes() {
        assert_eq!(exec_args(r#"env GDK=1 "/opt/My App/run" --new-window %U"#), ["env", "GDK=1", "/opt/My App/run", "--new-window"]);
        assert_eq!(exec_args("firefox %u"), ["firefox"]);
        assert_eq!(exec_args("echo 100%%"), ["echo", "100%"]);
    }

    #[test]
    fn protected_processes_are_refused() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let err = rt.block_on(close("ollama", true)).unwrap_err();
        assert!(err.contains("protected"));
    }
}
