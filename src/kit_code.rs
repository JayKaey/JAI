//! Sir's coding orders (2026-11-16), for the project open in VS Code:
//! - "how's my project?": branch, changed files, commits not pushed and today's (read only),
//! - "write a commit message": the diff and the last subjects to the model, the message onto
//!   the clipboard (nothing is committed),
//! - "build my project" / "run the tests": cargo in the background, every rustc error on a card
//!   with its own button that opens VS Code at the line.
//!
//! The project is VS Code's last active folder (its own `storage.json`).

use crate::kit::say;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid code phrase")
}

static STATUS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:how(?:'s|s| s| is) my (?:project|code|repo)(?: doing)?|(?:show|give) me (?:my |the )?(?:project|repo|git) (?:status|summary)|what(?:'s|s| s| is| have i) (?:changed|done) in my (?:project|repo)(?: today)?|wie steht (?:mein|das) projekt|was hat sich in meinem projekt geändert)$")
});
static MESSAGE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:write|make|suggest|give me) (?:me )?a commit message(?: for (?:this|my changes|it))?$|^schreib(?:e)? (?:mir )?(?:eine )?commit[- ]?nachricht$")
});
static BUILD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?P<what>build|compile|test|run the tests (?:of|for|in)|run the tests in) (?:my |the )?(?:project|code|repo)$|^(?P<t>run (?:my |the )?tests)$|^(?P<de>bau|kompilier(?:e)?|teste) (?:mein|das) projekt$")
});

/// An order about his code: the tool's arguments.
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    if STATUS.is_match(s) {
        return Some(json!({"action": "status"}));
    }
    if MESSAGE.is_match(s) {
        return Some(json!({"action": "message"}));
    }
    let c = BUILD.captures(s)?;
    let test = c.name("t").is_some() || c.name("what").is_some_and(|m| m.as_str().contains("test")) || c.name("de").is_some_and(|m| m.as_str() == "teste");
    Some(json!({"action": if test { "test" } else { "build" }}))
}

pub fn only_looks(args: &Value) -> bool {
    args["action"].as_str() == Some("status")
}

// ------------------------------------------------------------------------- the project ----

/// "file:///d%3A/Code/jarvis%20V.3" → "d:/Code/jarvis V.3".
fn path_of_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file:///")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// The folder of VS Code's last active window (or of its first open one), from its storage.
fn project_in(storage: &Value) -> Option<PathBuf> {
    let state = &storage["windowsState"];
    std::iter::once(&state["lastActiveWindow"])
        .chain(state["openedWindows"].as_array().into_iter().flatten())
        .find_map(|w| w["folder"].as_str().and_then(path_of_uri))
}

fn open_project() -> Result<PathBuf, String> {
    let file = std::env::var("APPDATA").map(|d| PathBuf::from(d).join("Code\\User\\globalStorage\\storage.json")).map_err(|e| e.to_string())?;
    let storage: Value = serde_json::from_str(&std::fs::read_to_string(&file).map_err(|_| "VS Code has no project open, Sir.".to_string())?).map_err(|e| e.to_string())?;
    project_in(&storage).filter(|p| p.is_dir()).ok_or_else(|| "VS Code has no project folder open, Sir.".to_string())
}

fn name_of(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| dir.display().to_string())
}

async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut all = vec!["-C", dir.to_str()?];
    all.extend_from_slice(args);
    let out = crate::shell::run_helper(Path::new("git"), all, None, &[], Duration::from_secs(20)).await.ok()?;
    (out.code == Some(0)).then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

// ------------------------------------------------------------------------- the orders ----

pub async fn run(ctx: &crate::tools::ToolContext, args: &Value) -> Result<String, String> {
    let dir = open_project()?;
    let name = name_of(&dir);
    match args["action"].as_str().unwrap_or("status") {
        "status" => {
            let branch = git(&dir, &["branch", "--show-current"]).await.ok_or_else(|| format!("{name} is no git project, Sir."))?;
            let changed: Vec<String> = git(&dir, &["status", "--porcelain"]).await.unwrap_or_default().lines().map(|l| l.get(3..).unwrap_or(l).to_string()).collect();
            let unpushed = git(&dir, &["rev-list", "--count", "@{u}..HEAD"]).await.and_then(|n| n.trim().parse::<usize>().ok());
            let today: Vec<String> = git(&dir, &["log", "--since=midnight", "--format=%s"]).await.unwrap_or_default().lines().map(str::to_string).collect();
            let (text, line) = status_text(&name, &branch, &changed, unpushed, &today);
            crate::kit::show_text("code_status", &format!("PROJECT // {}", name.to_uppercase()), &text, [440.0, 260.0]);
            Ok(say(&line, &line))
        }
        "message" => {
            let stat = git(&dir, &["diff", "HEAD", "--stat"]).await.unwrap_or_default();
            if stat.trim().is_empty() {
                return Ok(say(&format!("{name} has no changes to describe, Sir."), "There are no changes, Sir."));
            }
            let diff: String = git(&dir, &["diff", "HEAD", "--unified=1"]).await.unwrap_or_default().chars().take(7000).collect();
            let subjects = git(&dir, &["log", "-5", "--format=%s"]).await.unwrap_or_default();
            let text = format!("Recent commit subjects (match their style):\n{subjects}\n\nChanged files:\n{stat}\n\nDiff (trimmed):\n{diff}");
            let message = ctx
                .pim
                .complete("Write the git commit message for these changes: a subject line under 72 characters in the style of the recent subjects, then a blank line and at most four short lines on what changed. Reply with the message only.", &text, 220)
                .await
                .ok_or("The language model is not ready, Sir.")?;
            let message = message.trim().trim_matches('`').trim().to_string();
            crate::clipboard::write(&message).await?;
            let subject = message.lines().next().unwrap_or("").to_string();
            Ok(say(&format!("On your clipboard, Sir (nothing committed):\n\n{message}"), &format!("The message is on your clipboard, Sir: {subject}")))
        }
        action @ ("build" | "test") => {
            if !dir.join("Cargo.toml").is_file() {
                return Err(format!("{name} has no Cargo.toml, Sir: I build Rust projects."));
            }
            let test = action == "test";
            let line = format!("{} {name} in the background, Sir. I'll tell you how it went.", if test { "Testing" } else { "Building" });
            tokio::spawn(async move {
                let args: &[&str] = if test { &["test", "--message-format", "short", "--no-fail-fast"] } else { &["build", "--message-format", "short"] };
                let out = crate::shell::run_helper(Path::new("cargo"), args, Some(&dir), &[], Duration::from_secs(1800)).await;
                let line = match out {
                    Ok(out) => report(&name, &dir, test, out.code == Some(0), &String::from_utf8_lossy(&out.stderr), &String::from_utf8_lossy(&out.stdout)),
                    Err(e) => format!("Cargo did not run for {name}, Sir: {e}"),
                };
                crate::announcements::say(line);
            });
            Ok(say(&line, &line))
        }
        other => Err(format!("The code helper has no action '{other}'.")),
    }
}

/// The block's text and the one spoken line of "how's my project?".
fn status_text(name: &str, branch: &str, changed: &[String], unpushed: Option<usize>, today: &[String]) -> (String, String) {
    let mut text = format!("Branch  {branch}\n");
    text.push_str(&format!("Changed  {} file{}\n", changed.len(), if changed.len() == 1 { "" } else { "s" }));
    for file in changed.iter().take(8) {
        text.push_str(&format!("  {file}\n"));
    }
    if changed.len() > 8 {
        text.push_str(&format!("  … and {} more\n", changed.len() - 8));
    }
    text.push_str(&match unpushed {
        Some(n) => format!("Not pushed  {n}\n"),
        None => "Not pushed  (no upstream)\n".to_string(),
    });
    text.push_str(&format!("Today  {} commit{}\n", today.len(), if today.len() == 1 { "" } else { "s" }));
    for subject in today.iter().take(5) {
        text.push_str(&format!("  {subject}\n"));
    }
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let mut parts = vec![plural(changed.len(), "changed file")];
    if let Some(n) = unpushed.filter(|n| *n > 0) {
        parts.push(format!("{} not pushed", plural(n, "commit")));
    }
    if !today.is_empty() {
        parts.push(format!("{} today", plural(today.len(), "commit")));
    }
    (text.trim_end().to_string(), format!("{name} is on {branch}, Sir: {}.", parts.join(", ")))
}

/// `src\main.rs:12:5: error[E0425]: cannot find value `x` in this scope` (rustc's short form).
static RUSTC_ERROR: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^((?:[A-Za-z]:)?[^:\r\n]+\.rs):(\d+):\d+: error(?:\[E\d+\])?: (.+?)\s*$"));
/// `test kit::tests::grades ... FAILED`
static FAILED_TEST: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^test (\S+) \.\.\. FAILED\s*$"));
/// `test result: FAILED. 1264 passed; 1 failed; …`
static TEST_RESULT: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^test result: \w+\. (\d+) passed; (\d+) failed"));

/// The errors of a cargo run: (file, line, message), each once.
fn rustc_errors(output: &str) -> Vec<(String, u32, String)> {
    let mut out: Vec<(String, u32, String)> = Vec::new();
    for c in RUSTC_ERROR.captures_iter(output) {
        let e = (c[1].trim().to_string(), c[2].parse().unwrap_or(1), c[3].to_string());
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

/// After cargo: the card (errors or failed tests, each error with its own button) and the line
/// he hears.
fn report(name: &str, dir: &Path, test: bool, ok: bool, stderr: &str, stdout: &str) -> String {
    use crate::proactive_butler::{show, Card, Fix};
    use std::sync::atomic::{AtomicU32, Ordering};
    // Red builds in a row (any project): the third earns a dry line.
    static RED: AtomicU32 = AtomicU32::new(0);
    let errors = rustc_errors(stderr);
    let failed: Vec<String> = FAILED_TEST.captures_iter(stdout).map(|c| c[1].to_string()).collect();
    let passed: usize = TEST_RESULT.captures_iter(stdout).filter_map(|c| c[1].parse::<usize>().ok()).sum();
    if ok {
        RED.store(0, Ordering::Release);
        return if test { format!("All {passed} tests of {name} passed, Sir.") } else { format!("{name} built without errors, Sir.") };
    }
    let red = RED.fetch_add(1, Ordering::AcqRel) + 1;
    if red >= 3 {
        crate::kit_wit::on(crate::kit_wit::Event::BuildFailedAgain(red));
    }
    if !errors.is_empty() {
        let shown: Vec<_> = errors.iter().take(6).collect();
        let body = shown.iter().map(|(f, l, m)| format!("{}:{l}  {m}", f.rsplit(['\\', '/']).next().unwrap_or(f))).collect::<Vec<_>>().join("\n");
        let fixes = shown
            .iter()
            .map(|(f, l, _)| {
                let file = dir.join(f).display().to_string();
                Fix { label: format!("OPEN {}:{l}", f.rsplit(['\\', '/']).next().unwrap_or(f)), tool: "open_application", args: json!({"app_name": "vscode", "args": ["-g", format!("{file}:{l}")]}) }
            })
            .collect();
        show(&Card { title: format!("BUILD FAILED // {}", name.to_uppercase()), body, fixes, spoken: None });
        return format!("{name} has {} error{}, Sir; they are on the card.", errors.len(), if errors.len() == 1 { "" } else { "s" });
    }
    if !failed.is_empty() {
        show(&Card { title: format!("TESTS FAILED // {}", name.to_uppercase()), body: failed.iter().take(10).cloned().collect::<Vec<_>>().join("\n"), fixes: Vec::new(), spoken: None });
        return format!("{} of {name}'s tests failed, Sir; {passed} passed.", failed.len());
    }
    format!("Cargo stopped for {name} without a compiler error, Sir.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_orders_are_heard() {
        for (said, want) in [
            ("how's my project", "status"),
            ("what have i changed in my project today", "status"),
            ("wie steht mein projekt", "status"),
            ("write a commit message", "message"),
            ("schreib mir eine commit nachricht", "message"),
            ("build my project", "build"),
            ("run the tests", "test"),
            ("teste mein projekt", "test"),
        ] {
            assert_eq!(command(&crate::intents::normalize_phrase(said)), Some(json!({"action": want})), "{said}");
        }
        for not in ["build a snowman", "how is my day", "run", "write a message to max"] {
            assert_eq!(command(&crate::intents::normalize_phrase(not)), None, "{not}");
        }
    }

    #[test]
    fn the_project_is_vs_codes_last_active_folder() {
        let storage = json!({"windowsState": {"lastActiveWindow": {"folder": "file:///c%3A/Users/max/Code/demo%20app"}, "openedWindows": []}});
        assert_eq!(project_in(&storage), Some(PathBuf::from("c:/Users/max/Code/demo app")));
        let only_opened = json!({"windowsState": {"lastActiveWindow": {"backupPath": "x"}, "openedWindows": [{"folder": "file:///d%3A/Code/demo"}]}});
        assert_eq!(project_in(&only_opened), Some(PathBuf::from("d:/Code/demo")));
        assert_eq!(project_in(&json!({})), None);
        assert_eq!(path_of_uri("file:///d%3A/a%2"), Some(PathBuf::from("d:/a%2")), "a cut-off escape stays as it is");
    }

    #[test]
    fn a_status_is_one_block_and_one_line() {
        let (text, line) = status_text("demo", "main", &["src/main.rs".into(), "Cargo.toml".into()], Some(2), &["Fix the clock".into()]);
        assert_eq!(line, "demo is on main, Sir: 2 changed files, 2 commits not pushed, 1 commit today.");
        assert!(text.contains("Branch  main") && text.contains("  src/main.rs") && text.contains("Not pushed  2") && text.contains("  Fix the clock"), "{text}");
        let (_, clean) = status_text("demo", "dev", &[], None, &[]);
        assert_eq!(clean, "demo is on dev, Sir: 0 changed files.");
    }

    #[test]
    fn rustc_errors_are_read_once_each() {
        let out = "warning: unused\nsrc\\main.rs:12:5: error[E0425]: cannot find value `x` in this scope\nsrc\\main.rs:12:5: error[E0425]: cannot find value `x` in this scope\nsrc/kit.rs:3:1: error: expected item\nerror: could not compile `demo`";
        assert_eq!(
            rustc_errors(out),
            vec![("src\\main.rs".into(), 12, "cannot find value `x` in this scope".into()), ("src/kit.rs".into(), 3, "expected item".into())]
        );
        assert_eq!(FAILED_TEST.captures_iter("test a::b ... FAILED\ntest c ... ok").count(), 1);
    }
}
