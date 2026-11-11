//! Checks against the real PC that the ordinary test run leaves alone. Each is `#[ignore]`d and
//! runs only when its environment variable says so, one at a time:
//! `cargo test --release <name> -- --ignored --exact --nocapture --test-threads=1`.
//!
//! - `the_real_mixer_moves_its_own_session_and_nothing_else` (JAI_REAL_AUDIO=1): this test
//!   plays silence, which gives it a session of its own in Windows' volume mixer, and JAI's
//!   mixer sets, steps, mutes, mutes for three seconds, fades out and in, undoes and restores
//!   that session through Core Audio; every other app and the master volume must stay as they
//!   were.
//! - `the_master_volume_moves_and_comes_back` (JAI_REAL_MASTER=1): the master volume a few
//!   points down and up, muted and unmuted, and put back exactly (only while nobody listens).
//! - `shutdown_and_restart_reach_windows_and_are_called_off` (JAI_REAL_POWER=1): shutdown.exe
//!   run exactly as JAI runs it, but ten minutes out, and called off at once; and whether
//!   Windows lets the PC sleep.
//! - `lock_locks_the_pc` (JAI_REAL_LOCK=1): the lock JAI runs, and the lock screen taking
//!   the input desktop.
//! - `a_test_session_gives_way_to_the_jai_sir_starts` (JAI_EXE=<JAI.exe>): a hidden
//!   silent session is started and asked to give way as Sir's JAI asks it.
//! - `his_words_for_the_route_reader` (JAI_HIS_WORDS=<file>): Sir's own lines from the
//!   conversation log (read-only), one per line, for `where_these_sentences_go`.
//! - `voice_samples_for_his_ear` (JAI_SAMPLES=<folder>): both voices as WAV files.

use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

fn wanted(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| v.trim() == "1")
}

fn skipped(var: &str) {
    eprintln!("skipped: set {var} to run this check");
}

/// A stream of silence on the default output: this test's own session in the volume mixer.
fn own_session() -> cpal::Stream {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let device = cpal::default_host().default_output_device().expect("an output device");
    let config = device.default_output_config().expect("its format");
    let format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(&config, |data: &mut [f32], _| data.fill(0.0), |e| eprintln!("stream: {e}"), None),
        cpal::SampleFormat::I16 => device.build_output_stream(&config, |data: &mut [i16], _| data.fill(0), |e| eprintln!("stream: {e}"), None),
        cpal::SampleFormat::I32 => device.build_output_stream(&config, |data: &mut [i32], _| data.fill(0), |e| eprintln!("stream: {e}"), None),
        other => panic!("output sample format {other:?}"),
    }
    .expect("the silent stream");
    stream.play().expect("the silent stream plays");
    stream
}

/// (name, level, muted) of every app in the mixer.
async fn mixer() -> Vec<(String, u32, f32, bool)> {
    crate::audio_mixer::list_audio_apps().await.expect("the mixer").into_iter().map(|s| (s.name, s.pid, s.level, s.muted)).collect()
}

async fn mine(pid: u32) -> (f32, bool) {
    mixer().await.into_iter().find(|(_, p, _, _)| *p == pid).map(|(_, _, level, muted)| (level, muted)).expect("the test's own session")
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 0.011
}

#[test]
#[ignore = "changes real mixer levels (its own session only): JAI_REAL_AUDIO=1"]
fn the_real_mixer_moves_its_own_session_and_nothing_else() {
    if !wanted("JAI_REAL_AUDIO") {
        return skipped("JAI_REAL_AUDIO=1");
    }
    assert!(crate::audio_mixer::bench().is_none(), "the pretend mixer is set: run this check alone");
    let rt = tokio::runtime::Runtime::new().expect("a runtime");
    rt.block_on(async {
        let _stream = own_session();
        let me = std::env::current_exe().expect("the test's path").file_stem().expect("a name").to_string_lossy().to_lowercase();
        let pid = std::process::id();
        let mut found = false;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if mixer().await.iter().any(|(_, p, _, _)| *p == pid) {
                found = true;
                break;
            }
        }
        assert!(found, "the silent stream has no session in the mixer");
        let others_before: Vec<(String, u32, f32, bool)> = mixer().await.into_iter().filter(|(_, p, _, _)| *p != pid).collect();
        let master_before = crate::audio_mixer::master_state().await.expect("the master volume");
        let start = mine(pid).await;
        eprintln!("own session '{me}' at {:.0} %{}; {} other app(s); master {:.0} %", start.0 * 100.0, if start.1 { " (muted)" } else { "" }, others_before.len(), master_before.0 * 100.0);
        let tool = |args: serde_json::Value| async move { crate::audio_mixer::tool(&args).await };
        let mut failures: Vec<String> = Vec::new();
        let mut check = |what: &str, ok: bool, detail: String| {
            eprintln!("{} {what}: {detail}", if ok { "PASS" } else { "FAIL" });
            if !ok {
                failures.push(format!("{what}: {detail}"));
            }
        };

        let said = tool(json!({"action": "set_app", "app": me, "level": 30})).await;
        let now = mine(pid).await;
        check("set to 30 %", said.as_ref().is_ok_and(|s| s.contains("30 %")) && near(now.0, 0.30) && !now.1, format!("{said:?}, Windows reads {:.1} %", now.0 * 100.0));

        let said = tool(json!({"action": "change_app", "app": me, "delta": -10})).await;
        let now = mine(pid).await;
        check("10 points down", said.as_ref().is_ok_and(|s| s.contains("lowered to 20 %")) && near(now.0, 0.20), format!("{said:?}, Windows reads {:.1} %", now.0 * 100.0));

        let said = tool(json!({"action": "mute_app", "app": me})).await;
        let now = mine(pid).await;
        check("muted", said.is_ok() && now.1 && near(now.0, 0.20), format!("{said:?}, Windows reads muted={}", now.1));
        let said = tool(json!({"action": "unmute_app", "app": me})).await;
        let now = mine(pid).await;
        check("unmuted", said.is_ok() && !now.1 && near(now.0, 0.20), format!("{said:?}, Windows reads muted={}", now.1));

        let said = tool(json!({"action": "mute_app", "app": me, "seconds": 3})).await;
        let now = mine(pid).await;
        check("muted for 3 seconds", said.as_ref().is_ok_and(|s| s.contains("for 3 seconds")) && now.1, format!("{said:?}, Windows reads muted={}", now.1));
        tokio::time::sleep(Duration::from_millis(4200)).await;
        let now = mine(pid).await;
        check("the timed mute ends by itself", !now.1 && near(now.0, 0.20), format!("after 4.2 s Windows reads muted={} at {:.1} %", now.1, now.0 * 100.0));

        let said = tool(json!({"action": "fade", "app": me, "way": "out"})).await;
        let started = Instant::now();
        let mut levels: Vec<f32> = Vec::new();
        while started.elapsed() < Duration::from_millis(6500) {
            levels.push(mine(pid).await.0);
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let between = levels.iter().filter(|l| **l > 0.005 && **l < 0.195).count();
        let falling = levels.windows(2).all(|w| w[1] <= w[0] + 0.005);
        let last = *levels.last().unwrap_or(&1.0);
        check(
            "fade out over five seconds",
            said.as_ref().is_ok_and(|s| s.starts_with("Fading")) && between >= 8 && falling && near(last, 0.0),
            format!("{said:?}; {} readings, {between} on the way, falling={falling}, last {:.1} %: {}", levels.len(), last * 100.0, levels.iter().map(|l| format!("{:.0}", l * 100.0)).collect::<Vec<_>>().join(" ")),
        );

        let said = tool(json!({"action": "fade", "app": me, "way": "in"})).await;
        tokio::time::sleep(Duration::from_millis(6000)).await;
        let now = mine(pid).await;
        check("fade back in to where it was", said.is_ok() && near(now.0, 0.20), format!("{said:?}, Windows reads {:.1} %", now.0 * 100.0));

        let said = tool(json!({"action": "undo_last"})).await;
        let now = mine(pid).await;
        check("undo takes the last change back", said.is_ok() && near(now.0, 0.0), format!("{said:?}, Windows reads {:.1} %", now.0 * 100.0));

        let said = tool(json!({"action": "restore"})).await;
        let now = mine(pid).await;
        check("restore puts the first level back", said.is_ok() && near(now.0, start.0) && now.1 == start.1, format!("{said:?}, Windows reads {:.1} % (it started at {:.1} %)", now.0 * 100.0, start.0 * 100.0));

        // Nothing else moved: every other app as it was, the master volume as it was.
        let others_after = mixer().await;
        let moved: Vec<String> = others_before
            .iter()
            .filter_map(|(name, p, level, muted)| {
                let after = others_after.iter().find(|(n, q, _, _)| q == p && n == name)?;
                (!near(after.2, *level) || after.3 != *muted).then(|| format!("{name} {:.0}%→{:.0}% muted {}→{}", level * 100.0, after.2 * 100.0, muted, after.3))
            })
            .collect();
        check("no other app moved", moved.is_empty(), if moved.is_empty() { format!("{} app(s) unchanged", others_before.len()) } else { moved.join(", ") });
        let master_after = crate::audio_mixer::master_state().await.expect("the master volume");
        check("the master volume did not move", near(master_after.0, master_before.0) && master_after.1 == master_before.1, format!("{:.1} % → {:.1} %", master_before.0 * 100.0, master_after.0 * 100.0));
        assert!(failures.is_empty(), "{} failure(s): {}", failures.len(), failures.join(" | "));
    });
}

#[test]
#[ignore = "moves the real master volume and puts it back: JAI_REAL_MASTER=1, only while nobody listens"]
fn the_master_volume_moves_and_comes_back() {
    if !wanted("JAI_REAL_MASTER") {
        return skipped("JAI_REAL_MASTER=1");
    }
    let rt = tokio::runtime::Runtime::new().expect("a runtime");
    rt.block_on(async {
        let tool = |args: serde_json::Value| async move { crate::audio_mixer::tool(&args).await };
        let read = || async { crate::audio_mixer::master_state().await.expect("the master volume") };
        let (level, muted) = read().await;
        let pct = (level * 100.0).round();
        // A few points the way there is room for.
        let target = if pct >= 10.0 { pct - 4.0 } else { pct + 4.0 };
        let mut seen: Vec<String> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        let said = tool(json!({"action": "set_master", "level": target})).await;
        let now = read().await;
        seen.push(format!("set {target} %: {said:?} → {:.1} %", now.0 * 100.0));
        if !(said.is_ok() && near(now.0, target / 100.0)) {
            failures.push("set".into());
        }
        let said = tool(json!({"action": "change_master", "delta": 2})).await;
        let now = read().await;
        seen.push(format!("+2: {said:?} → {:.1} %", now.0 * 100.0));
        if !(said.is_ok() && near(now.0, (target + 2.0) / 100.0)) {
            failures.push("step".into());
        }
        let said = tool(json!({"action": "mute_master"})).await;
        let now = read().await;
        seen.push(format!("mute: {said:?} → muted {}", now.1));
        if !(said.is_ok() && now.1) {
            failures.push("mute".into());
        }
        let said = tool(json!({"action": "unmute_master"})).await;
        let now = read().await;
        seen.push(format!("unmute: {said:?} → muted {}", now.1));
        if !(said.is_ok() && !now.1) {
            failures.push("unmute".into());
        }
        // Back exactly as it was, whatever happened above.
        let _ = tool(json!({"action": "set_master", "level": pct})).await;
        if muted {
            let _ = tool(json!({"action": "mute_master"})).await;
        }
        let back = read().await;
        seen.push(format!("back: {:.1} % muted {} (it was {:.1} % muted {muted})", back.0 * 100.0, back.1, level * 100.0));
        for line in &seen {
            eprintln!("{line}");
        }
        assert!(near(back.0, level) && back.1 == muted, "the master volume is not back where it was: {back:?} instead of {:?}", (level, muted));
        assert!(failures.is_empty(), "failed: {}", failures.join(", "));
    });
}

#[cfg(windows)]
#[link(name = "PowrProf")]
extern "system" {
    fn IsPwrSuspendAllowed() -> u8;
}

#[test]
#[ignore = "schedules a real shutdown and restart ten minutes out and calls each off: JAI_REAL_POWER=1"]
fn shutdown_and_restart_reach_windows_and_are_called_off() {
    if !wanted("JAI_REAL_POWER") {
        return skipped("JAI_REAL_POWER=1");
    }
    #[cfg(windows)]
    {
        let rt = tokio::runtime::Runtime::new().expect("a runtime");
        rt.block_on(async {
            for (what, flag) in [("shutdown", "/s"), ("restart", "/r")] {
                let asked = crate::system_power::run_shutdown_exe(&[flag, "/t", "600", "/c", "JAI power check: called off at once."]).await;
                // Whatever happened, nothing may stay pending.
                let called_off = crate::system_power::run_shutdown_exe(&["/a"]).await;
                assert!(called_off.is_ok() || asked.is_err(), "THE {} COULD NOT BE CALLED OFF ({called_off:?}): run `shutdown /a` now", what.to_uppercase());
                assert!(asked.is_ok(), "shutdown.exe {flag} was refused: {asked:?}");
                let again = crate::system_power::run_shutdown_exe(&["/a"]).await;
                assert!(again.is_err(), "a second `shutdown /a` still found a pending {what}");
                eprintln!("PASS {what}: shutdown.exe {flag} accepted (ten minutes out) and called off; nothing pending");
            }
        });
        // SAFETY: parameterless query.
        let sleep = unsafe { IsPwrSuspendAllowed() } != 0;
        eprintln!("{} sleep: Windows {} this PC sleep", if sleep { "PASS" } else { "NOTE" }, if sleep { "lets" } else { "does not let" });
    }
}

/// While the lock screen is up its desktop has the input, and this session cannot open it.
#[cfg(windows)]
fn input_desktop_locked() -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> isize;
        fn CloseDesktop(desktop: isize) -> i32;
    }
    const DESKTOP_SWITCHDESKTOP: u32 = 0x0100;
    // SAFETY: the handle is checked and closed.
    unsafe {
        let desktop = OpenInputDesktop(0, 0, DESKTOP_SWITCHDESKTOP);
        if desktop == 0 {
            return true;
        }
        CloseDesktop(desktop);
        false
    }
}

#[test]
#[ignore = "really locks the PC: JAI_REAL_LOCK=1"]
fn lock_locks_the_pc() {
    if !wanted("JAI_REAL_LOCK") {
        return skipped("JAI_REAL_LOCK=1");
    }
    #[cfg(windows)]
    {
        assert!(!input_desktop_locked(), "the PC is locked already");
        let rt = tokio::runtime::Runtime::new().expect("a runtime");
        let out = rt.block_on(crate::system_power::execute(crate::system_power::PowerAction::Lock));
        assert_eq!(out, Ok("The PC is locked.".to_string()));
        let locked = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(200));
            input_desktop_locked()
        });
        assert!(locked, "LockWorkStation succeeded, but the lock screen never took the input desktop");
        eprintln!("PASS lock: the lock screen has the input desktop");
    }
}

/// What jai.log gained since `from` bytes.
fn log_since(log: &Path, from: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut text = String::new();
    if let Ok(mut f) = std::fs::File::open(log) {
        if f.seek(SeekFrom::Start(from)).is_ok() {
            let mut bytes = Vec::new();
            let _ = f.read_to_end(&mut bytes);
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    text
}

#[test]
#[ignore = "starts a hidden silent JAI and asks it to give way: JAI_EXE=<path to JAI.exe>"]
fn a_test_session_gives_way_to_the_jai_sir_starts() {
    let Ok(exe) = std::env::var("JAI_EXE") else {
        return skipped("JAI_EXE=<path to JAI.exe>");
    };
    #[cfg(windows)]
    {
        assert!(!crate::platform::test_session_running(), "a test session is running already");
        assert!(crate::platform::SingleInstance::acquire().is_some(), "a JAI is running: this check needs his place free");
        let log = Path::new("jai.log");
        let from = std::fs::metadata(log).map(|m| m.len()).unwrap_or(0);
        let mut child = std::process::Command::new(&exe).args(["--silent", "--chat", "What time is it?", "--chat-delay", "120"]).spawn().expect("the test session starts");
        let started = Instant::now();
        let ready = loop {
            if log_since(log, from).contains("[TRAY] Running in the notification area") && crate::platform::test_session_running() {
                break true;
            }
            if started.elapsed() > Duration::from_secs(150) || child.try_wait().ok().flatten().is_some() {
                break false;
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        if !ready {
            let _ = child.kill();
            panic!("the test session never got its tray icon up");
        }
        eprintln!("the test session is up after {:.1} s", started.elapsed().as_secs_f32());
        std::thread::sleep(Duration::from_secs(3));
        let asked = Instant::now();
        assert!(crate::tray::ask_test_session_to_give_way(), "the test session's tray window could not be reached");
        let status = loop {
            if let Ok(Some(status)) = child.try_wait() {
                break Some(status);
            }
            if asked.elapsed() > Duration::from_secs(30) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let Some(status) = status else {
            let _ = child.kill();
            panic!("the test session did not give way within the 30 s Sir's JAI waits for it");
        };
        eprintln!("PASS it gave way {:.1} s after it was asked ({status})", asked.elapsed().as_secs_f32());
        assert!(crate::platform::SingleInstance::acquire().is_some(), "its single-instance place is still held");
        assert!(!crate::platform::test_session_running(), "its test-session marker is still there");
        assert!(log_since(log, from).contains("this test session gives way"), "jai.log has no give-way line");
        eprintln!("PASS the place is free for the JAI Sir started, and the log says why it went");
    }
}

/// 2026-11-11: a rename changed the name of the vault key's entry, the next start made a new
/// key and could read no secret. This installation's vault, opened as the app opens it: every
/// stored secret decrypts. Counts and slot names only; no secret is printed.
#[test]
#[ignore = "opens this installation's vault with its real key: JAI_VAULT_DB=<path to the memory database>"]
fn the_installations_vault_reads_every_secret() {
    let Ok(db) = std::env::var("JAI_VAULT_DB") else {
        return skipped("JAI_VAULT_DB=<path to the memory database>");
    };
    let slots: Vec<(String, String)> = {
        let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("the memory database");
        let mut stmt = conn.prepare("SELECT service, name FROM vault_secrets").expect("the vault's table");
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).expect("its rows").flatten().collect();
        rows
    };
    let vault = crate::vault::Vault::open(&db).expect("the vault opens");
    let unread: Vec<&(String, String)> = slots.iter().filter(|(service, name)| vault.secret(service, name).is_none()).collect();
    eprintln!("vault: {} of {} secrets read ({})", slots.len() - unread.len(), slots.len(), vault.key_store());
    assert!(unread.is_empty(), "sealed with another key: {unread:?}");
}

#[test]
#[ignore = "reads Sir's own lines from jai_memory.db (read-only) into a file: JAI_HIS_WORDS=<file>"]
fn his_words_for_the_route_reader() {
    let Ok(out) = std::env::var("JAI_HIS_WORDS") else {
        return skipped("JAI_HIS_WORDS=<file>");
    };
    let conn = rusqlite::Connection::open_with_flags("jai_memory.db", rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("the memory database");
    let mut roles = conn.prepare("SELECT role, COUNT(*) FROM conversation_logs GROUP BY role").expect("roles");
    let counts: Vec<(String, i64)> = roles.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).expect("roles").flatten().collect();
    eprintln!("conversation_logs: {counts:?}");
    let mut stmt = conn.prepare("SELECT content FROM conversation_logs WHERE role = 'user' ORDER BY id").expect("his lines");
    let rows: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0)).expect("his lines").flatten().collect();
    let mut seen = std::collections::HashSet::new();
    let lines: Vec<String> = rows
        .iter()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty() && l.chars().count() <= 240)
        .filter(|l| seen.insert(l.to_lowercase()))
        .collect();
    std::fs::write(&out, lines.join("\n")).expect("the file");
    eprintln!("{} distinct line(s) of his from {} row(s)", lines.len(), rows.len());
}

/// 16-bit mono PCM.
fn write_wav(path: &Path, audio: &[f32], rate: u32) -> std::io::Result<()> {
    let data_len = (audio.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in audio {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    std::fs::write(path, out)
}

#[test]
#[ignore = "renders the voice to a WAV file for Sir's ear: JAI_SAMPLES=<folder>"]
fn voice_samples_for_his_ear() {
    let Ok(dir) = std::env::var("JAI_SAMPLES") else {
        return skipped("JAI_SAMPLES=<folder>");
    };
    // His settings as they are (read, never written).
    crate::config::set_read_only(true);
    let cfg = crate::config::JaiConfig::load();
    let rate = crate::jai_voice_dsp::MASTER_RATE;
    for (who, rendered) in [("jai", crate::fish_zero_shot_tts::render_sentences(&cfg))] {
        let sentences = rendered.unwrap_or_else(|e| panic!("{who}'s voice: {e}"));
        let gap = vec![0.0f32; (rate as f32 * 0.6) as usize];
        let audio: Vec<f32> = sentences.iter().flat_map(|(_, _, a)| a.iter().copied().chain(gap.iter().copied())).collect();
        let path = Path::new(&dir).join(format!("{who}_voice_sample.wav"));
        write_wav(&path, &audio, rate).expect("the WAV file");
        let peak = audio.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        eprintln!("{who}: {} sentence(s), {:.1} s, peak {:.2} → {}", sentences.len(), audio.len() as f32 / rate as f32, peak, path.display());
    }
}
