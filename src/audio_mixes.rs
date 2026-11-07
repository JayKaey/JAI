//! Named mixes: "save this mix as gaming", "load my gaming mix". The master volume and every
//! app's own level as they stand, under a name Sir gave them, kept in `config/audio_mixes.json`.
//! A test session reads that file and writes nothing; the mixer (`audio_mixer`) does the rest.

use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, Mutex, MutexGuard};
use tracing::warn;

/// Where the mixes are kept.
pub const MIXES_PATH: &str = "config/audio_mixes.json";

/// One app's volume in a mix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub app: String,
    /// 0.0-1.0.
    pub level: f32,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mix {
    pub name: String,
    /// (level 0.0-1.0, muted)
    pub master: (f32, bool),
    pub apps: Vec<Channel>,
}

static MIXES: LazyLock<Mutex<Vec<Mix>>> = LazyLock::new(|| Mutex::new(if cfg!(test) { Vec::new() } else { read_file() }));

fn read_file() -> Vec<Mix> {
    let Ok(text) = std::fs::read_to_string(MIXES_PATH) else { return Vec::new() };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        warn!("[MIXER] {MIXES_PATH} could not be read: {e}");
        Vec::new()
    })
}

fn write_file(all: &[Mix]) {
    if cfg!(test) || crate::platform::in_test_session() {
        return;
    }
    let saved = serde_json::to_string_pretty(all).map_err(|e| e.to_string()).and_then(|json| {
        let tmp = format!("{MIXES_PATH}.tmp");
        std::fs::create_dir_all("config").and_then(|()| std::fs::write(&tmp, json)).and_then(|()| std::fs::rename(&tmp, MIXES_PATH)).map_err(|e| e.to_string())
    });
    if let Err(e) = saved {
        warn!("[MIXER] Could not save {MIXES_PATH}: {e}");
    }
}

fn mixes() -> MutexGuard<'static, Vec<Mix>> {
    MIXES.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a name is kept as: "My Gaming Mix" is "gaming". None: nothing usable is left of it.
pub fn clean_name(name: &str) -> Option<String> {
    let name = name.trim().to_lowercase();
    let name = name.trim_start_matches("my ").trim_start_matches("the ").trim_end_matches(" mix").trim();
    (!name.is_empty() && name.chars().count() <= 32).then(|| name.to_string())
}

/// The mix of that name.
pub fn find(name: &str) -> Option<Mix> {
    let name = clean_name(name)?;
    mixes().iter().find(|m| m.name == name).cloned()
}

/// The names of the saved mixes.
pub fn names() -> Vec<String> {
    mixes().iter().map(|m| m.name.clone()).collect()
}

/// Keeps a mix (one of the same name is replaced).
pub fn store(mix: Mix) {
    let mut all = mixes();
    all.retain(|m| m.name != mix.name);
    all.push(mix);
    write_file(&all);
}

/// Forgets a mix; false when there was none of that name.
pub fn remove(name: &str) -> bool {
    let Some(name) = clean_name(name) else { return false };
    let mut all = mixes();
    let before = all.len();
    all.retain(|m| m.name != name);
    let found = all.len() != before;
    if found {
        write_file(&all);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mix_is_found_by_the_name_sir_says() {
        assert_eq!(clean_name("My Gaming Mix").as_deref(), Some("gaming"));
        assert_eq!(clean_name("the late night mix").as_deref(), Some("late night"));
        assert_eq!(clean_name("  "), None);
        assert_eq!(clean_name(&"x".repeat(40)), None);
        let mix = |master: f32| Mix { name: "unit test mix".to_string(), master: (master, false), apps: vec![Channel { app: "Discord".to_string(), level: 0.8, muted: false }] };
        store(Mix { name: clean_name("unit test mix").unwrap(), ..mix(0.4) });
        store(Mix { name: clean_name("unit test mix").unwrap(), ..mix(0.5) });
        assert_eq!(find("My Unit Test Mix").map(|m| m.master.0), Some(0.5), "the newer one of that name");
        assert_eq!(names().iter().filter(|n| n.as_str() == "unit test").count(), 1);
        assert!(remove("the unit test mix"));
        assert!(!remove("the unit test mix") && find("unit test").is_none());
    }
}
