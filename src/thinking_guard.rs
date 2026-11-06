//! `is_thinking` as an RAII guard: a turn holds a `ThinkingStateGuard` while the brain works on
//! it, and dropping the guard — on completion, timeout, cancellation, an early return or a panic
//! unwinding the task — always clears it. The HUD's emblem reads `is_thinking()` to enter the 3D
//! gyroscope, so it can never be left spinning on a turn that is gone.
//!
//! The flag counts guards: an old, cancelled turn that unwinds after a new one started does not
//! clear the new turn's state.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

/// A thinking flag shared by its guards.
#[derive(Clone, Default)]
pub struct ThinkingFlag(Arc<AtomicUsize>);

impl ThinkingFlag {
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire) > 0
    }

    /// Raises the flag until the returned guard is dropped.
    pub fn engage(&self) -> ThinkingStateGuard {
        self.0.fetch_add(1, Ordering::AcqRel);
        ThinkingStateGuard { flag: self.clone(), on_release: None }
    }
}

/// Holds the flag up; released exactly once, when dropped.
#[must_use = "the flag drops again as soon as the guard is dropped"]
pub struct ThinkingStateGuard {
    flag: ThinkingFlag,
    on_release: Option<Box<dyn FnOnce() + Send>>,
}

impl ThinkingStateGuard {
    /// Runs `f` right after the flag is cleared (the HUD repaints, a watchdog starts).
    pub fn on_release(mut self, f: impl FnOnce() + Send + 'static) -> Self {
        self.on_release = Some(Box::new(f));
        self
    }
}

impl Drop for ThinkingStateGuard {
    fn drop(&mut self) {
        self.flag.0.fetch_sub(1, Ordering::AcqRel);
        if let Some(f) = self.on_release.take() {
            f();
        }
    }
}

static THINKING: LazyLock<ThinkingFlag> = LazyLock::new(ThinkingFlag::default);

/// Whether the brain is working on a turn right now.
pub fn is_thinking() -> bool {
    THINKING.is_set()
}

/// Marks a turn as thinking until the guard is dropped (the HUD repaints at both ends).
pub fn engage() -> ThinkingStateGuard {
    let guard = THINKING.engage();
    crate::gui::refresh();
    guard.on_release(crate::gui::refresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn the_guard_always_clears_the_flag() {
        let flag = ThinkingFlag::default();
        assert!(!flag.is_set());
        {
            let _g = flag.engage();
            assert!(flag.is_set());
        }
        assert!(!flag.is_set(), "cleared on drop");
        // An early return from a turn.
        let run = |f: &ThinkingFlag| -> Result<(), &'static str> {
            let _g = f.engage();
            Err("the engine timed out")?;
            Ok(())
        };
        assert!(run(&flag).is_err());
        assert!(!flag.is_set(), "cleared on an early return");
        // A panic unwinding the turn.
        let f2 = flag.clone();
        let result = std::panic::catch_unwind(move || {
            let _g = f2.engage();
            panic!("turn crashed");
        });
        assert!(result.is_err());
        assert!(!flag.is_set(), "cleared while unwinding");
    }

    #[test]
    fn overlapping_turns_keep_the_newer_one_thinking() {
        let flag = ThinkingFlag::default();
        let old = flag.engage();
        let new = flag.engage();
        drop(old);
        assert!(flag.is_set(), "the cancelled turn unwinding does not clear the new one");
        drop(new);
        assert!(!flag.is_set());
    }

    #[test]
    fn release_hook_runs_after_the_flag_is_down() {
        let flag = ThinkingFlag::default();
        let seen = Arc::new(AtomicBool::new(true));
        let (f, s) = (flag.clone(), Arc::clone(&seen));
        drop(flag.engage().on_release(move || s.store(f.is_set(), Ordering::Release)));
        assert!(!seen.load(Ordering::Acquire), "the hook sees the flag already cleared");
    }

    #[tokio::test]
    async fn a_timed_out_turn_task_clears_the_flag() {
        let flag = ThinkingFlag::default();
        let f = flag.clone();
        let turn = async move {
            let _g = f.engage();
            // A language engine that never answers.
            std::future::pending::<()>().await;
        };
        let outcome = tokio::time::timeout(std::time::Duration::from_millis(30), turn).await;
        assert!(outcome.is_err());
        assert!(!flag.is_set(), "dropping the timed-out future drops the guard");
    }
}
