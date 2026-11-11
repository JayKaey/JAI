//! What JAI says unprompted once a job of his own is done (the network radar's sweep, the
//! disk map's scan): any module queues a line here, and the app speaks it as an alert as soon
//! as he is free (`JAI::raise_alert`, which waits while he is talking or thinking).

use std::sync::Mutex;

static QUEUE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Queues one line for JAI to say (blank lines are dropped).
pub fn say(line: impl Into<String>) {
    let line = line.into();
    if line.trim().is_empty() {
        return;
    }
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push(line);
}

/// Everything queued since the last call, oldest first.
pub fn take() -> Vec<String> {
    std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_come_out_once_and_in_order() {
        let _ = take();
        say("The sweep is done, Sir.");
        say("   ");
        say("Six devices are online.");
        assert_eq!(take(), vec!["The sweep is done, Sir.".to_string(), "Six devices are online.".to_string()]);
        assert!(take().is_empty());
    }
}
