//! Crash containment: no single fault takes J.A.I. down.
//!
//! * `lock` — a poisoned mutex (a thread panicked while holding it) is recovered, never
//!   re-panicked: the data inside is still the last consistent value the code wrote.
//! * `guard` — one unit of work (a transcription, a synthesis, a GUI frame, a viewport) runs
//!   under `catch_unwind`; a panic is logged with its message and the caller carries on.
//! * `supervise` — a worker loop (microphone, VAD, TTS) that panics is restarted after a short
//!   back-off instead of leaving JAI deaf or mute; a loop that keeps panicking (more than
//!   `MAX_RESTARTS` within `RESTART_WINDOW`) is given up on and says so in the log.
//! * `clip` / `floor_char_boundary` — prefixes of user and model text by characters, never by a
//!   byte index that could split "ä", "ß" or "—" (`byte index is not a char boundary`).

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tracing::{error, warn};

/// A worker that panics this often within `RESTART_WINDOW` is not restarted again.
pub const MAX_RESTARTS: usize = 5;
pub const RESTART_WINDOW: Duration = Duration::from_secs(60);

/// The mutex's guard, recovering from poisoning.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A panic payload as text ("index out of bounds: …").
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Runs `f`, turning a panic into `None` (logged as `[CRASH] <what> …`).
pub fn guard<T>(what: &str, f: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(payload) => {
            error!("[CRASH] {what} panicked and was contained: {}", panic_message(payload.as_ref()));
            None
        }
    }
}

/// Whether a worker that just panicked (at `now`, after the earlier panics in `history`) may be
/// restarted. Old panics fall out of the window.
pub fn may_restart(history: &mut Vec<Instant>, now: Instant) -> bool {
    history.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
    history.push(now);
    history.len() <= MAX_RESTARTS
}

/// Runs a worker loop, restarting it when it panics (0.5 s, 1 s, 2 s … back-off). `body`
/// returning normally ends the worker.
pub fn supervise(name: &str, mut body: impl FnMut()) {
    let mut history: Vec<Instant> = Vec::new();
    loop {
        match catch_unwind(AssertUnwindSafe(&mut body)) {
            Ok(()) => return,
            Err(payload) => {
                let msg = panic_message(payload.as_ref());
                if !may_restart(&mut history, Instant::now()) {
                    error!("[CRASH] Worker '{name}' panicked {} times within {} s ({msg}); it stays stopped.", history.len(), RESTART_WINDOW.as_secs());
                    return;
                }
                let backoff = Duration::from_millis(500 << (history.len() - 1).min(4));
                warn!("[CRASH] Worker '{name}' panicked ({msg}); restarting in {} ms.", backoff.as_millis());
                std::thread::sleep(backoff);
            }
        }
    }
}

/// Stack of every async worker (`jai-rt`). Tokio's 2 MB default overflowed on "open my system
/// stats" (a tool that runs another tool: two of the big `ToolContext::execute` poll frames on
/// top of the turn), which aborted the whole process (a stack overflow cannot be caught).
pub const RUNTIME_STACK_BYTES: usize = 16 * 1024 * 1024;

/// The application's multi-threaded runtime: named workers with `RUNTIME_STACK_BYTES` of stack.
pub fn runtime_builder() -> tokio::runtime::Builder {
    let mut b = tokio::runtime::Builder::new_multi_thread();
    b.enable_all().thread_name("jai-rt").thread_stack_size(RUNTIME_STACK_BYTES);
    b
}

/// The largest char boundary of `s` at or below byte `index`.
pub fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    (0..=index).rev().find(|i| s.is_char_boundary(*i)).unwrap_or(0)
}

/// At most `max_bytes` of `s`, cut at a char boundary.
pub fn clip_bytes(s: &str, max_bytes: usize) -> &str {
    &s[..floor_char_boundary(s, max_bytes)]
}

/// The first `max_chars` characters of `s`.
pub fn clip(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn german_and_symbol_text_is_never_split_inside_a_character() {
        let s = "Schließ den Kern — Größe ändern // ü";
        for n in 0..=s.len() + 3 {
            let cut = clip_bytes(s, n);
            assert!(s.starts_with(cut));
            assert!(cut.len() <= n.min(s.len()));
        }
        assert_eq!(clip("Größe", 3), "Grö");
        assert_eq!(clip("ä", 5), "ä");
        assert_eq!(clip("", 2), "");
        // Byte 7 sits inside "ß" (bytes 6-7 of "Schließ").
        assert_eq!(floor_char_boundary("Schließ", 7), 6);
        assert_eq!(floor_char_boundary("—", 1), 0);
    }

    #[test]
    fn a_poisoned_lock_is_recovered() {
        let m = Arc::new(Mutex::new(41));
        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("worker crashed while holding the lock");
        })
        .join();
        assert!(m.is_poisoned());
        *lock(&m) += 1;
        assert_eq!(*lock(&m), 42);
    }

    #[test]
    fn a_panic_is_contained_and_reported() {
        assert_eq!(guard("test", || 7), Some(7));
        let s = String::from("Kern");
        assert_eq!(guard("slice", || s[..9].to_string()), None);
        assert!(panic_message(&"boom" as &(dyn Any + Send)).contains("boom"));
    }

    /// Uses about `depth` × 64 KB of stack.
    #[inline(never)]
    fn deep(depth: usize) -> u64 {
        let pad = std::hint::black_box([depth as u8; 64 * 1024]);
        if depth == 0 {
            return pad[17] as u64;
        }
        deep(depth - 1) + std::hint::black_box(pad[depth % 1024]) as u64
    }

    #[test]
    fn async_workers_have_room_for_nested_tool_calls() {
        // 6 MB of stack inside a task: three times what overflowed tokio's 2 MB default.
        let rt = runtime_builder().worker_threads(1).build().unwrap();
        let sum = rt.block_on(async { tokio::spawn(async { deep(96) }).await.unwrap() });
        assert!(sum > 0);
        const { assert!(RUNTIME_STACK_BYTES >= 8 * 1024 * 1024) };
    }

    #[test]
    fn a_crashing_worker_restarts_until_it_finishes() {
        let mut runs = 0;
        supervise("test", || {
            runs += 1;
            if runs < 3 {
                panic!("audio device disconnected");
            }
        });
        assert_eq!(runs, 3);
    }

    #[test]
    fn a_worker_that_keeps_crashing_is_given_up_on() {
        let mut history = Vec::new();
        let t0 = Instant::now();
        for k in 0..MAX_RESTARTS {
            assert!(may_restart(&mut history, t0 + Duration::from_secs(k as u64)));
        }
        assert!(!may_restart(&mut history, t0 + Duration::from_secs(10)));
        // A minute later the old panics no longer count.
        let mut history = vec![t0];
        assert!(may_restart(&mut history, t0 + RESTART_WINDOW + Duration::from_secs(1)));
        assert_eq!(history.len(), 1);
    }
}
