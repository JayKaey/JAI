//! Plain words for what went wrong. A tool's own error text is written for the log ("Open-Meteo
//! answered HTTP 503 Service Unavailable", "close_application needs 'app_name'."); what Sir hears
//! says what it means for him and, where there is one, what helps.

use regex::Regex;
use std::sync::LazyLock;

static HTTP_STATUS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\bhttp(?:\s+(?:status|error))?|\bstatus(?:\s+code)?)[\s:]*([1-5]\d\d)\b").expect("valid regex"));

/// The status of a web answer named in an error text ("HTTP 503", "status code: 404"), when it
/// is one that means a refusal or a failure.
fn http_status(lower: &str) -> Option<u16> {
    HTTP_STATUS.captures(lower)?.get(1)?.as_str().parse().ok().filter(|code| *code >= 400)
}

/// The thing an order is about, from the name of the argument that was missing.
fn missing(param: &str) -> &'static str {
    match param {
        "app_name" | "app" | "application" | "process" => "which app you mean",
        "target_window" | "window" => "which window you mean",
        "query" | "search" | "question" => "what to look for",
        "text" | "content" | "message" | "body" => "what to write",
        "path" | "file" | "file_path" | "folder" => "which file you mean",
        "url" | "site" | "address" => "which page you mean",
        "name" | "title" | "id" => "which one you mean",
        "time" | "when" | "due" | "minutes" | "duration" => "for when",
        "to" | "recipient" | "contact" => "who it is for",
        _ => "one part of that",
    }
}

/// The plain sentence for a raw error text, when it is one of the kinds that come up again and
/// again (None: nothing here knows it, and the text stands as it is).
pub fn plain(err: &str) -> Option<String> {
    let lower = err.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if let Some(code) = http_status(&lower) {
        return Some(match code {
            401 | 403 => format!("The service refused me ({code}): it does not accept the sign-in or the key I have."),
            404 => "That address does not exist any more (404).".to_string(),
            408 | 504 => format!("The service took too long to answer ({code}). It usually works again a minute later."),
            429 => "The service says I asked too often (429). It will answer again a little later.".to_string(),
            500..=599 => format!("The service is having trouble on its side ({code}). It usually works again a minute later."),
            _ => format!("The service turned the request down ({code})."),
        });
    }
    if has(&["dns error", "no such host", "failed to lookup address", "name or service not known", "os error 11001", "network is unreachable", "os error 10051", "os error 10065"]) {
        return Some("I could not reach the internet for that. The connection seems to be down.".to_string());
    }
    if has(&["connection refused", "os error 10061", "actively refused"]) {
        return Some("Nothing answered at the other end: the program or service I asked is not running.".to_string());
    }
    if has(&["timed out", "timeout", "deadline has elapsed", "os error 10060"]) {
        return Some("That took too long and I stopped waiting. It may work on a second try.".to_string());
    }
    if has(&["error sending request", "connection reset", "connection closed", "os error 10054", "tls handshake", "invalid certificate", "certificate verify"]) {
        return Some("The connection broke off on the way. It may work on a second try.".to_string());
    }
    if has(&["access is denied", "os error 5)", "permission denied", "zugriff verweigert"]) {
        return Some("Windows refused that: access denied. It may need a program that runs as administrator.".to_string());
    }
    if has(&["being used by another process", "os error 32)", "sharing violation"]) {
        return Some("Another program is using that file right now. Close it there and ask me again.".to_string());
    }
    if has(&["cannot find the file", "cannot find the path", "os error 2)", "os error 3)", "no such file or directory"]) {
        return Some("That file or folder is not where I looked for it.".to_string());
    }
    if has(&["not enough space on the disk", "disk full", "os error 112)"]) {
        return Some("The drive is full.".to_string());
    }
    // "close_application needs 'app_name'.": the order arrived without the thing it is about.
    if let Some(param) = lower.split(" needs '").nth(1).and_then(|rest| rest.split('\'').next()) {
        return Some(format!("I did not catch {}. Say it once more with that in it.", missing(param)));
    }
    if lower.starts_with("unknown tool") || (lower.starts_with("unknown ") && lower.contains(" action")) {
        return Some("That is not something I can do yet.".to_string());
    }
    if has(&["missing field", "invalid type:", "expected value at", "eof while parsing"]) {
        return Some("The order reached me in a form I could not use. Say it once more, a little differently.".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_errors_seen_in_the_log_are_said_in_plain_words() {
        for (raw, holds) in [
            ("Open-Meteo answered HTTP 503 Service Unavailable", "trouble on its side (503)"),
            ("the feed answered with status code: 404", "does not exist any more"),
            ("HTTP status 429 Too Many Requests", "asked too often"),
            ("HTTP 401 Unauthorized", "refused me (401)"),
            ("error sending request for url (https://example.org/): dns error: No such host is known. (os error 11001)", "could not reach the internet"),
            ("tcp connect error: No connection could be made because the target machine actively refused it. (os error 10061)", "is not running"),
            ("operation timed out", "took too long"),
            ("Access is denied. (os error 5)", "access denied"),
            ("The process cannot access the file because it is being used by another process. (os error 32)", "Another program is using that file"),
            ("The system cannot find the file specified. (os error 2)", "not where I looked"),
            ("close_application needs 'app_name'.", "which app you mean"),
            ("create_reminder needs 'text'.", "what to write"),
            ("Unknown watch action 'x'.", "not something I can do yet"),
            ("missing field `action` at line 1 column 2", "a form I could not use"),
        ] {
            let said = plain(raw);
            assert!(said.as_deref().is_some_and(|s| s.contains(holds)), "{raw} -> {said:?}");
        }
    }

    #[test]
    fn what_is_already_in_his_words_stands_as_it_is() {
        for raw in [
            "Up doesn't appear to be running.",
            "I could not find an application called 'Blender' on this machine.",
            "There's nothing on the board, Sir.",
            "No open window matches 'firefox'.",
            // A page count or a year is no web status.
            "The status of 200 files could not be read.",
            "The timetable for 2026 is not out yet.",
        ] {
            assert_eq!(plain(raw), None, "{raw}");
        }
    }
}
