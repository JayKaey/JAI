//! System volume (exact percentages) and media transport keys.
//! Windows: Core Audio `IAudioEndpointVolume` + `SendInput` media keys.
//! Linux: `wpctl` (PipeWire) → `pactl` (PulseAudio) → `amixer` (ALSA); `playerctl` for media.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAction {
    SetVolume(u8),
    VolumeUp(u8),
    VolumeDown(u8),
    Mute,
    Unmute,
    ToggleMute,
    PlayPause,
    Next,
    Previous,
    Stop,
    GetVolume,
}

impl MediaAction {
    pub fn parse(action: &str, level: Option<i64>) -> Option<Self> {
        let a = action.trim().to_lowercase().replace([' ', '-'], "_");
        let step = level.map(|l| l.clamp(1, 100) as u8).unwrap_or(10);
        Some(match a.as_str() {
            "set_volume" | "volume" | "set" => Self::SetVolume(level?.clamp(0, 100) as u8),
            "volume_up" | "louder" | "up" | "increase_volume" => Self::VolumeUp(step),
            "volume_down" | "quieter" | "down" | "decrease_volume" => Self::VolumeDown(step),
            "mute" => Self::Mute,
            "unmute" => Self::Unmute,
            "toggle_mute" => Self::ToggleMute,
            "play_pause" | "play" | "pause" | "resume" | "toggle_playback" => Self::PlayPause,
            "next_track" | "next" | "skip" => Self::Next,
            "prev_track" | "previous_track" | "previous" | "prev" | "back" => Self::Previous,
            "stop" | "stop_media" => Self::Stop,
            "get_volume" | "volume_level" | "current_volume" => Self::GetVolume,
            _ => return None,
        })
    }
}

#[cfg(windows)]
pub async fn execute(action: MediaAction) -> Result<String, String> {
    use crate::audio_mixer as mixer;
    use crate::win32;
    let pct = |v: f32| (v * 100.0).round() as i32;
    // The volume goes through the mixer's three layers (Core Audio, the PowerShell endpoint,
    // the volume keys), so it changes even when COM fails inside Jarvis.
    match action {
        MediaAction::SetVolume(level) => mixer::set_master_volume(level as f32 / 100.0).await.map(|_| format!("System volume set to {level}%.")),
        MediaAction::VolumeUp(step) | MediaAction::VolumeDown(step) => {
            let before = mixer::master_state().await.ok().map(|(l, _)| l);
            let delta = step as f32 * if matches!(action, MediaAction::VolumeUp(_)) { 1.0 } else { -1.0 };
            let out = mixer::change_master_volume(delta).await?;
            let after = mixer::master_state().await.ok().map(|(l, _)| l);
            Ok(match (before, after) {
                (Some(b), Some(a)) => format!("System volume changed from {}% to {}%.", pct(b), pct(a)),
                _ => out,
            })
        }
        MediaAction::Mute => mixer::set_master_mute(true).await.map(|_| "System audio muted.".to_string()),
        MediaAction::Unmute => mixer::set_master_mute(false).await.map(|_| "System audio unmuted.".to_string()),
        MediaAction::ToggleMute => {
            let (_, muted) = mixer::master_state().await?;
            mixer::set_master_mute(!muted).await?;
            Ok(if muted { "System audio unmuted." } else { "System audio muted." }.to_string())
        }
        MediaAction::GetVolume => {
            let (level, muted) = mixer::master_state().await?;
            Ok(format!("System volume is {}%{}.", pct(level), if muted { " (muted)" } else { "" }))
        }
        // A media key reaches whatever plays on the PC: never from a test.
        MediaAction::PlayPause | MediaAction::Next | MediaAction::Previous | MediaAction::Stop if crate::platform::hands_off() => Ok("Media key left alone: a test never touches Sir's players.".into()),
        MediaAction::PlayPause | MediaAction::Next | MediaAction::Previous | MediaAction::Stop => win32::call(move || -> Result<String, String> {
            match action {
                MediaAction::PlayPause => win32::tap_key(win32::VK_MEDIA_PLAY_PAUSE).map(|_| "Toggled play/pause.".to_string()),
                MediaAction::Next => win32::tap_key(win32::VK_MEDIA_NEXT_TRACK).map(|_| "Skipped to the next track.".to_string()),
                MediaAction::Previous => win32::tap_key(win32::VK_MEDIA_PREV_TRACK).map(|_| "Went back to the previous track.".to_string()),
                _ => win32::tap_key(win32::VK_MEDIA_STOP).map(|_| "Stopped playback.".to_string()),
            }
        })
        .await?,
    }
}

#[cfg(not(windows))]
pub async fn execute(action: MediaAction) -> Result<String, String> {
    tokio::task::spawn_blocking(move || linux::execute(action))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(not(windows))]
mod linux {
    use super::MediaAction;
    use crate::apps::which;
    use std::process::Command;

    fn run(program: &str, args: &[&str]) -> Result<String, String> {
        let out = Command::new(program).args(args).output().map_err(|e| format!("{program}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(format!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    enum Mixer {
        Wpctl,
        Pactl,
        Amixer,
    }

    fn mixer() -> Result<Mixer, String> {
        if which("wpctl").is_some() {
            Ok(Mixer::Wpctl)
        } else if which("pactl").is_some() {
            Ok(Mixer::Pactl)
        } else if which("amixer").is_some() {
            Ok(Mixer::Amixer)
        } else {
            Err("No audio mixer found (install wireplumber, pulseaudio-utils or alsa-utils).".into())
        }
    }

    /// Current volume (0-100) and mute state.
    fn get() -> Result<(u8, bool), String> {
        match mixer()? {
            Mixer::Wpctl => {
                // "Volume: 0.40 [MUTED]"
                let out = run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
                let v: f32 = out
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .ok_or("could not parse wpctl output")?;
                Ok(((v * 100.0).round().clamp(0.0, 150.0) as u8, out.contains("MUTED")))
            }
            Mixer::Pactl => {
                let vol = run("pactl", &["get-sink-volume", "@DEFAULT_SINK@"])?;
                let pct = vol
                    .split('/')
                    .nth(1)
                    .and_then(|s| s.trim().trim_end_matches('%').parse::<u8>().ok())
                    .ok_or("could not parse pactl output")?;
                let mute = run("pactl", &["get-sink-mute", "@DEFAULT_SINK@"])?.contains("yes");
                Ok((pct, mute))
            }
            Mixer::Amixer => {
                let out = run("amixer", &["sget", "Master"])?;
                let pct = out
                    .split('[')
                    .find_map(|s| s.split(']').next().and_then(|v| v.strip_suffix('%')).and_then(|v| v.parse().ok()))
                    .ok_or("could not parse amixer output")?;
                Ok((pct, out.contains("[off]")))
            }
        }
    }

    fn set(level: u8) -> Result<(), String> {
        match mixer()? {
            Mixer::Wpctl => run("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{:.2}", level as f32 / 100.0)]).map(|_| ()),
            Mixer::Pactl => run("pactl", &["set-sink-volume", "@DEFAULT_SINK@", &format!("{level}%")]).map(|_| ()),
            Mixer::Amixer => run("amixer", &["-q", "sset", "Master", &format!("{level}%")]).map(|_| ()),
        }
    }

    fn mute(on: Option<bool>) -> Result<(), String> {
        let arg = match on {
            Some(true) => "1",
            Some(false) => "0",
            None => "toggle",
        };
        match mixer()? {
            Mixer::Wpctl => run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", arg]).map(|_| ()),
            Mixer::Pactl => run("pactl", &["set-sink-mute", "@DEFAULT_SINK@", arg]).map(|_| ()),
            Mixer::Amixer => {
                let a = match on {
                    Some(true) => "mute",
                    Some(false) => "unmute",
                    None => "toggle",
                };
                run("amixer", &["-q", "sset", "Master", a]).map(|_| ())
            }
        }
    }

    fn player(cmd: &str) -> Result<(), String> {
        if which("playerctl").is_none() {
            return Err("playerctl is not installed (sudo apt install playerctl).".into());
        }
        run("playerctl", &[cmd]).map(|_| ())
    }

    pub fn execute(action: MediaAction) -> Result<String, String> {
        if matches!(action, MediaAction::PlayPause | MediaAction::Next | MediaAction::Previous | MediaAction::Stop) && crate::platform::hands_off() {
            return Ok("Media key left alone: a test never touches Sir's players.".into());
        }
        match action {
            MediaAction::SetVolume(level) => {
                set(level)?;
                mute(Some(false))?;
                Ok(format!("System volume set to {level}%."))
            }
            MediaAction::VolumeUp(step) | MediaAction::VolumeDown(step) => {
                let (current, _) = get()?;
                let next = if matches!(action, MediaAction::VolumeUp(_)) {
                    current.saturating_add(step).min(100)
                } else {
                    current.saturating_sub(step)
                };
                set(next)?;
                Ok(format!("System volume changed from {current}% to {next}%."))
            }
            MediaAction::Mute => mute(Some(true)).map(|_| "System audio muted.".into()),
            MediaAction::Unmute => mute(Some(false)).map(|_| "System audio unmuted.".into()),
            MediaAction::ToggleMute => mute(None).map(|_| "Toggled mute.".into()),
            MediaAction::GetVolume => {
                let (level, muted) = get()?;
                Ok(format!("System volume is {level}%{}.", if muted { " (muted)" } else { "" }))
            }
            MediaAction::PlayPause => player("play-pause").map(|_| "Toggled play/pause.".into()),
            MediaAction::Next => player("next").map(|_| "Skipped to the next track.".into()),
            MediaAction::Previous => player("previous").map(|_| "Went back to the previous track.".into()),
            MediaAction::Stop => player("stop").map(|_| "Stopped playback.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_actions() {
        assert_eq!(MediaAction::parse("set_volume", Some(55)), Some(MediaAction::SetVolume(55)));
        assert_eq!(MediaAction::parse("set_volume", Some(400)), Some(MediaAction::SetVolume(100)));
        assert_eq!(MediaAction::parse("set_volume", None), None);
        assert_eq!(MediaAction::parse("volume_up", None), Some(MediaAction::VolumeUp(10)));
        assert_eq!(MediaAction::parse("play pause", None), Some(MediaAction::PlayPause));
        assert_eq!(MediaAction::parse("prev_track", None), Some(MediaAction::Previous));
    }

    /// A test never presses a media key: it would pause or skip whatever Sir is playing.
    #[tokio::test]
    async fn a_test_never_presses_a_media_key() {
        for action in [MediaAction::PlayPause, MediaAction::Next, MediaAction::Previous, MediaAction::Stop] {
            assert_eq!(execute(action).await, Ok("Media key left alone: a test never touches Sir's players.".to_string()), "{action:?}");
        }
    }
}
