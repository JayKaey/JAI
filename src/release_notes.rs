//! "What's new?": the newest day of the changes list (`changes::RELEASES`), spoken. (It was a
//! list of its own, kept by hand, and a week later still called the network radar new.)

use crate::changes::{Kind, RELEASES};

/// How many of the newest day's new things are read out; the changes list shows the rest.
const READ_OUT: usize = 3;

/// The answer to "what's new?".
pub fn spoken() -> String {
    let Some(latest) = RELEASES.first() else { return "Nothing new in this build, Sir.".to_string() };
    let new: Vec<&str> = latest.entries.iter().filter(|e| e.kind == Kind::New).map(|e| e.what).collect();
    let rest = latest.entries.len() - new.len();
    let mut line = format!("New in this build, Sir: {} new {} and {rest} improvements.", new.len(), if new.len() == 1 { "thing" } else { "things" });
    for what in new.iter().take(READ_OUT) {
        line.push(' ');
        line.push_str(what);
    }
    line.push_str(" \"Show the changes\" lists them all.");
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whats_new_is_the_newest_day_of_the_changes_list() {
        let line = spoken();
        assert!(line.starts_with("New in this build, Sir: "), "{line}");
        // (A day of fixes alone says "0 new things".)
        if let Some(first_new) = RELEASES[0].entries.iter().find(|e| e.kind == Kind::New) {
            assert!(line.contains(first_new.what), "{line}");
        }
        assert!(RELEASES[1].entries.iter().all(|e| !line.contains(e.what)), "an older day's news: {line}");
        assert!(line.split_whitespace().count() < 120, "short enough to hear: {line}");
        assert!(crate::changes::command("show the changes").is_some(), "the line it ends on opens the list");
    }
}
