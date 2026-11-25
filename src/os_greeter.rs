//! J.A.I.OS: his login screen. `jai --greeter` is started by greetd as the user `greeter`
//! (`os/settings/etc/greetd/config.toml`) inside KWin, the compositor the session runs on too:
//! what draws the desktop on this card and these screens draws the login screen, and no second
//! compositor has to be installed or kept working (cage, on wlroots, would be one more thing
//! between him and an NVIDIA card). KWin stands every screen black; his window fills the
//! first.
//!
//! The screen: black, his core, the clock and the date, the users of this PC, the password,
//! sleep / restart / shut down (logind). He speaks greetd's own protocol on `$GREETD_SOCK`
//! (a length and a JSON object each way): create_session, the password as the answer to PAM's
//! question, start_session with the J.A.I.OS session; a wrong password cancels the session and
//! says so.
//!
//! The voice: when the screen comes up he greets. He listens (Silero), and a sentence in a
//! voice like one of the prints the root core keeps (`/var/lib/jaios/voices`, the voice lock's
//! model) chooses that user and greets by name: "Good evening, Sir. Your password, please."
//! The password is asked for all the same, always. Without the models or a microphone he is
//! silent and everything else works.

#![cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]

use crate::holo::{self, Face};
use crate::holo_core::{CoreLook, CoreStyle};
use eframe::egui::{self, pos2, vec2, Align, Align2, Layout, Rect};
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------- greetd's protocol ----

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    CreateSession {
        username: String,
    },
    PostAuthMessageResponse {
        #[serde(skip_serializing_if = "Option::is_none")]
        response: Option<String>,
    },
    StartSession {
        cmd: Vec<String>,
        env: Vec<String>,
    },
    CancelSession,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Success,
    Error { error_type: String, description: String },
    AuthMessage { auth_message_type: String, auth_message: String },
}

/// A message as it goes over the socket: its length (32 bits, the machine's byte order), then
/// the JSON.
pub fn encode(request: &Request) -> Vec<u8> {
    let body = serde_json::to_vec(request).expect("a request is plain JSON");
    let mut message = (body.len() as u32).to_ne_bytes().to_vec();
    message.extend(body);
    message
}

/// The first whole message in `bytes` and how many bytes it took; None: not all here yet.
pub fn decode(bytes: &[u8]) -> Option<(Result<Response, String>, usize)> {
    let length = u32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    let body = bytes.get(4..4 + length)?;
    Some((serde_json::from_slice(body).map_err(|e| format!("greetd said something else: {e}")), 4 + length))
}

// ------------------------------------------------------------------------- a login ----

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stage {
    /// Nothing asked of greetd.
    Idle,
    /// The session is being made; PAM's questions are answered.
    Asking,
    /// The password was right: the session is being started.
    Starting,
    /// It went wrong: the session is being taken down.
    Cancelling,
    /// greetd starts the session once this program ends.
    Done,
}

/// One login against greetd, message by message: what comes back says what is sent next.
#[derive(Debug)]
pub struct Login {
    pub stage: Stage,
    password: String,
    /// How often the password was given (PAM asking again and again is no login).
    given: u8,
    cmd: Vec<String>,
    env: Vec<String>,
    /// Why the last login failed.
    pub error: String,
    /// What PAM said besides ("Your password expires tomorrow").
    pub note: String,
}

impl Login {
    pub fn new(cmd: Vec<String>, env: Vec<String>) -> Self {
        Self { stage: Stage::Idle, password: String::new(), given: 0, cmd, env, error: String::new(), note: String::new() }
    }

    /// A user and a password were entered.
    pub fn submit(&mut self, user: &str, password: &str) -> Option<Request> {
        if self.stage != Stage::Idle {
            return None;
        }
        (self.stage, self.password, self.given) = (Stage::Asking, password.to_string(), 0);
        self.error.clear();
        self.note.clear();
        Some(Request::CreateSession { username: user.to_string() })
    }

    fn cancel(&mut self, why: String) -> Option<Request> {
        (self.stage, self.error) = (Stage::Cancelling, why);
        self.password.clear();
        Some(Request::CancelSession)
    }

    /// greetd's answer: what to send next (None: nothing; see `stage`).
    pub fn on(&mut self, response: Response) -> Option<Request> {
        match (self.stage, response) {
            (Stage::Asking, Response::AuthMessage { auth_message_type, auth_message }) => match auth_message_type.as_str() {
                "secret" if self.given < 3 => {
                    self.given += 1;
                    Some(Request::PostAuthMessageResponse { response: Some(self.password.clone()) })
                }
                "secret" => self.cancel("Wrong password.".into()),
                // shortcut: a question PAM asks in the open (a one-time code) gets no answer, so
                // such a login fails; give it a second field when a PC of his asks one.
                "visible" => Some(Request::PostAuthMessageResponse { response: None }),
                _ => {
                    self.note = auth_message;
                    Some(Request::PostAuthMessageResponse { response: None })
                }
            },
            (Stage::Asking, Response::Success) => {
                self.stage = Stage::Starting;
                self.password.clear();
                Some(Request::StartSession { cmd: self.cmd.clone(), env: self.env.clone() })
            }
            (Stage::Starting, Response::Success) => {
                self.stage = Stage::Done;
                None
            }
            (Stage::Asking | Stage::Starting, Response::Error { error_type, description }) => self.cancel(if error_type == "auth_error" { "Wrong password.".into() } else { description }),
            // (Cancelled, or there was nothing left to cancel: either way a new login may begin.)
            (Stage::Cancelling, _) => {
                self.stage = Stage::Idle;
                None
            }
            _ => None,
        }
    }
}

// --------------------------------------------------------------- the users of this PC ----

#[derive(Clone, Debug, PartialEq)]
pub struct User {
    pub name: String,
    /// The name to show: the full name where one is set.
    pub shown: String,
}

/// The people of `/etc/passwd`: a user id from 1000 and a shell to log in with.
pub fn users(passwd: &str) -> Vec<User> {
    passwd
        .lines()
        .filter_map(|line| {
            let c: Vec<&str> = line.split(':').collect();
            let uid: u32 = c.get(2)?.parse().ok()?;
            let shell = c.get(6)?.trim();
            let person = (1000..60000).contains(&uid) && !shell.is_empty() && !shell.ends_with("/nologin") && !shell.ends_with("/false");
            let full = c.get(4)?.split(',').next().unwrap_or("").trim();
            person.then(|| User { name: c[0].to_string(), shown: if full.is_empty() { c[0].to_string() } else { full.to_string() } })
        })
        .collect()
}

/// The session's command out of its `.desktop` file (`Exec=`); Plasma's own without one.
pub fn session_command(desktop: Option<&str>) -> Vec<String> {
    let exec = desktop.and_then(|text| text.lines().find_map(|l| l.strip_prefix("Exec="))).unwrap_or("startplasma-wayland");
    exec.split_whitespace().filter(|w| !w.starts_with('%')).map(str::to_string).collect()
}

// ------------------------------------------------------------------- whose voice it is ----

/// A user's voice as the root core keeps it for this screen.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Voice {
    #[serde(skip)]
    pub user: String,
    /// How he addresses that user ("Sir").
    pub address: String,
    pub print: Vec<f32>,
    pub grown: Vec<f32>,
    /// How like the print a sentence must be.
    pub bar: f32,
}

/// The least a bar may be (a print stored with none is no free pass), and how far the best
/// match must stand above the next user's: two users this alike are not told apart by voice.
const LEAST_BAR: f32 = 0.30;
const CLEAR_BY: f32 = 0.05;

pub fn voices_in(dir: &std::path::Path) -> Vec<Voice> {
    let mut voices: Vec<Voice> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|f| f.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|f| {
            let voice: Voice = serde_json::from_str(&std::fs::read_to_string(f.path()).ok()?).ok()?;
            let user = f.path().file_stem()?.to_str()?.to_string();
            (!voice.print.is_empty() && voice.print.iter().all(|x| x.is_finite())).then_some(Voice { user, ..voice })
        })
        .collect();
    voices.sort_by(|a, b| a.user.cmp(&b.user));
    voices
}

/// Whose voice a sentence's print is: the user it is most like, when it is like enough and
/// clearly more like that one than like anyone else.
pub fn recognise<'a>(voices: &'a [Voice], print: &[f32]) -> Option<&'a Voice> {
    let like = |v: &Voice| crate::speaker_id::likeness(print, &v.print).max(if v.grown.len() == print.len() { crate::speaker_id::likeness(print, &v.grown) } else { f32::MIN });
    let mut scored: Vec<(f32, &Voice)> = voices.iter().filter(|v| v.print.len() == print.len()).map(|v| (like(v), v)).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (best, voice) = *scored.first()?;
    let clear = scored.get(1).is_none_or(|(next, _)| best - next >= CLEAR_BY);
    (best >= voice.bar.max(LEAST_BAR) && clear).then_some(voice)
}

/// What he says: to the room when the screen comes up, by name to a voice he knows.
pub fn welcome(hour: u32, address: Option<&str>) -> String {
    let greeting = crate::briefing::greeting(hour);
    match address.map(str::trim).filter(|a| !a.is_empty()) {
        Some(address) => format!("{} Your password, please.", greeting.replace("Sir", address)),
        None => greeting.replace(", Sir", ""),
    }
}

// --------------------------------------------------------------------------- the screen ----

/// What the screen shows.
#[derive(Default)]
pub struct Screen {
    pub users: Vec<User>,
    pub chosen: usize,
    pub password: String,
    /// Why the last login failed.
    pub error: String,
    /// What PAM said besides.
    pub note: String,
    /// His greeting to the voice he knew.
    pub greeting: String,
    /// The password is with greetd.
    pub busy: bool,
    /// The room's sound and his own voice, 0-1, and a fault's flash.
    pub heard: f32,
    pub voice: f32,
    pub fault: f32,
    pub clock: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Asked {
    Login,
    Sleep,
    Restart,
    ShutDown,
}

/// A row of keys about `centre`; which was pressed.
fn keys(ui: &mut egui::Ui, centre: egui::Pos2, labels: &[&str], chosen: Option<usize>) -> Option<usize> {
    const GAP: f32 = 8.0;
    let width: f32 = labels.iter().map(|l| crate::holo_kit::key_width(ui.painter(), l)).sum::<f32>() + GAP * labels.len().saturating_sub(1) as f32;
    let row = Rect::from_center_size(centre, vec2(width, crate::holo_kit::KEY_H));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
    child.spacing_mut().item_spacing.x = GAP;
    let mut pressed = None;
    for (i, label) in labels.iter().enumerate() {
        if crate::holo_ui::key_on(&mut child, *label, chosen == Some(i)).clicked() {
            pressed = Some(i);
        }
    }
    pressed
}

/// Draws the login screen; what was asked for this frame.
pub fn draw(ctx: &egui::Context, s: &mut Screen, now: chrono::NaiveDateTime) -> Option<Asked> {
    use chrono::Timelike;
    let t = holo::theme();
    let mut asked = None;
    egui::CentralPanel::default().frame(egui::Frame::none().fill(t.void())).show(ctx, |ui| {
        let screen = ui.max_rect();
        let p = ui.painter().clone();
        let x = screen.center().x;
        // The clock and the date, top left; what he is, bottom left.
        holo::text(&p, screen.left_top() + vec2(48.0, 40.0), Align2::LEFT_TOP, Face::Figure, &now.format("%H:%M").to_string(), 56.0, t.ink());
        holo::text(&p, screen.left_top() + vec2(50.0, 108.0), Align2::LEFT_TOP, Face::Label, &now.format("%A %-d %B %Y").to_string(), 10.0, t.muted());
        // His core: the scale ring is the clock, the ticks the room's sound, the ring his voice.
        let radius = (screen.height() * 0.19).clamp(80.0, 230.0);
        let centre = pos2(x, screen.top() + screen.height() * 0.36);
        let word = if s.busy {
            "CHECKING"
        } else if s.greeting.is_empty() {
            "LOCKED"
        } else {
            "RECOGNISED"
        };
        let look = CoreLook { clock: s.clock, day_secs: now.time().num_seconds_from_midnight() as f32, heard: s.heard, voice: s.voice, busy: if s.busy { 1.0 } else { 0.0 }, fault: s.fault, word, ..Default::default() };
        crate::holo_core::paint(&p, egui::Id::new("greeter_core"), CoreStyle::Simple, centre, radius, &look);
        // Under it: his greeting, the users, the password, what went wrong.
        let mut y = centre.y + radius * 1.5 + 30.0;
        if !s.greeting.is_empty() {
            holo::text(&p, pos2(x, y), Align2::CENTER_CENTER, Face::Head, &s.greeting, 21.0, t.ink());
        }
        y += 42.0;
        let names: Vec<&str> = s.users.iter().map(|u| u.shown.as_str()).collect();
        if let Some(i) = keys(ui, pos2(x, y), &names, Some(s.chosen)).filter(|_| !s.busy) {
            s.chosen = i;
            s.error.clear();
        }
        y += 44.0;
        let field = Rect::from_center_size(pos2(x, y), vec2(300.0, 28.0));
        let hint = egui::RichText::new("PASSWORD").font(holo::font(Face::Label, 9.0)).color(t.faint());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field).layout(Layout::left_to_right(Align::Center)));
        if s.busy {
            child.disable();
        }
        let typed = crate::holo_ui::slot(&mut child, egui::TextEdit::singleline(&mut s.password).password(true).hint_text(hint).font(holo::font(Face::Mono, 16.0)).text_color(t.ink()).desired_width(field.width()));
        // (The keyboard is the password's whenever nothing else holds it.)
        let free = ctx.memory(|m| m.focused().is_none());
        if free && !s.busy {
            typed.request_focus();
        }
        let entered = typed.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        y += 40.0;
        if (keys(ui, pos2(x, y), &["LOG IN"], None).is_some() || entered) && !s.busy && !s.password.is_empty() && !s.users.is_empty() {
            asked = Some(Asked::Login);
        }
        y += 36.0;
        let (line, colour) = if s.error.is_empty() { (&s.note, t.muted()) } else { (&s.error, t.alert()) };
        if !line.is_empty() {
            holo::text(&p, pos2(x, y), Align2::CENTER_CENTER, Face::Body, line, 14.0, colour);
        }
        // The PC's power, bottom right.
        let power = ["SLEEP", "RESTART", "SHUT DOWN"];
        let width: f32 = power.iter().map(|l| crate::holo_kit::key_width(&p, l) + 8.0).sum();
        if let Some(i) = keys(ui, pos2(screen.right() - 48.0 - width / 2.0, screen.bottom() - 52.0), &power, None) {
            asked = Some([Asked::Sleep, Asked::Restart, Asked::ShutDown][i]);
        }
        holo::text(&p, pos2(screen.left() + 50.0, screen.bottom() - 52.0), Align2::LEFT_CENTER, Face::Label, "J.A.I.OS", 9.0, t.faint());
    });
    asked
}

// ---------------------------------------------------------------------- the program ----

#[cfg(not(target_os = "linux"))]
pub fn run() -> std::process::ExitCode {
    eprintln!("jai --greeter is the login screen of J.A.I.OS.");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
pub use live::run;

#[cfg(target_os = "linux")]
mod live {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::{mpsc, Arc, Mutex};

    /// Where the greeter's user finds `models` and `runtime` (tmpfiles.d links them).
    const HOME: &str = "/var/lib/jaios/greeter";
    const SESSION: &str = "/usr/share/wayland-sessions/jaios.desktop";

    /// What the other threads tell the screen.
    #[derive(Default)]
    struct Shared {
        error: String,
        note: String,
        busy: bool,
        done: bool,
        /// The user a voice was known as, and his greeting.
        known: Option<(usize, String)>,
        heard: f32,
    }

    fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
        shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn exchange(socket: &mut std::os::unix::net::UnixStream, request: &Request) -> Result<Response, String> {
        socket.write_all(&encode(request)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some((response, _)) = decode(&bytes) {
                return response;
            }
            match socket.read(&mut chunk).map_err(|e| e.to_string())? {
                0 => return Err("greetd closed the connection.".into()),
                n => bytes.extend_from_slice(&chunk[..n]),
            }
        }
    }

    /// One login from the first message to the last. Whether the session starts.
    fn log_in(socket_path: &str, login: &mut Login, user: &str, password: &str) -> bool {
        let mut socket = match std::os::unix::net::UnixStream::connect(socket_path) {
            Ok(socket) => socket,
            Err(e) => {
                login.error = format!("greetd does not answer: {e}");
                return false;
            }
        };
        let mut next = login.submit(user, password);
        while let Some(request) = next {
            next = match exchange(&mut socket, &request) {
                Ok(response) => login.on(response),
                Err(why) => {
                    (login.stage, login.error) = (Stage::Idle, why);
                    None
                }
            };
        }
        login.stage == Stage::Done
    }

    /// His voice, and his ear for the voices the root core keeps. Silent without the models.
    fn voice(shared: Arc<Mutex<Shared>>, users: Vec<User>) -> Option<crate::tts::TtsController> {
        use chrono::Timelike;
        let cfg = crate::config::JaiConfig::default();
        let there = |file: &str| std::path::Path::new(file).is_file();
        let controls = Arc::new(crate::ears::AudioControls::new(&cfg));
        let tts = there(&cfg.kokoro_model_path).then(|| crate::tts::TtsController::start(&cfg, Arc::clone(&controls), tokio::sync::mpsc::unbounded_channel().0));
        let say = |tts: &Option<crate::tts::TtsController>, text: &str| {
            if let Some(tts) = tts {
                let turn = tts.generation();
                tts.speak(text, turn);
                tts.finish(turn);
            }
        };
        let hour = || chrono::Local::now().hour();
        say(&tts, &welcome(hour(), None));
        crate::speaker_id::set_models_dir(std::path::Path::new(&cfg.silero_vad_model_path).parent().unwrap_or(std::path::Path::new("models")));
        let voices = voices_in(std::path::Path::new(crate::os_system::VOICES));
        if voices.is_empty() || !there(&cfg.silero_vad_model_path) || !crate::speaker_id::available() {
            eprintln!("[GREETER] No voice is listened for ({} prints; the hearing models are {})", voices.len(), if crate::speaker_id::available() { "there" } else { "missing" });
            return tts;
        }
        let (events, mut heard) = tokio::sync::mpsc::unbounded_channel();
        crate::ears::spawn(&cfg, controls, events);
        let speaker = tts.clone();
        std::thread::spawn(move || {
            while let Some(event) = heard.blocking_recv() {
                match event {
                    crate::ears::AudioEvent::Level(level) => lock(&shared).heard = (level.rms * 6.0).clamp(0.0, 1.0),
                    crate::ears::AudioEvent::Utterance(said) => {
                        let Ok(print) = crate::speaker_id::print_of(&said.pcm) else { continue };
                        let Some(voice) = recognise(&voices, &print) else { continue };
                        let Some(user) = users.iter().position(|u| u.name == voice.user) else { continue };
                        // (Greeted once: he does not say it again to every further sentence.)
                        if lock(&shared).known.as_ref().is_some_and(|(known, _)| *known == user) {
                            continue;
                        }
                        let line = welcome(hour(), Some(if voice.address.is_empty() { "Sir" } else { &voice.address }));
                        eprintln!("[GREETER] A voice is known as {}'s", voice.user);
                        say(&speaker, &line);
                        lock(&shared).known = Some((user, line));
                    }
                    _ => {}
                }
            }
        });
        tts
    }

    struct Greeter {
        screen: Screen,
        shared: Arc<Mutex<Shared>>,
        logins: mpsc::Sender<(String, String)>,
        tts: Option<crate::tts::TtsController>,
        /// The user his greeting already chose.
        greeted: Option<usize>,
    }

    impl eframe::App for Greeter {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            let dt = ctx.input(|i| i.stable_dt).min(0.1);
            {
                let mut shared = lock(&self.shared);
                if shared.done {
                    drop(shared);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                if !shared.error.is_empty() && shared.error != self.screen.error {
                    self.screen.fault = 1.0;
                }
                if self.screen.busy && !shared.busy {
                    (self.screen.error, self.screen.note) = (std::mem::take(&mut shared.error), std::mem::take(&mut shared.note));
                }
                self.screen.busy = shared.busy;
                self.screen.heard = shared.heard;
                if let Some((user, line)) = shared.known.clone().filter(|(user, _)| self.greeted != Some(*user)) {
                    (self.greeted, self.screen.chosen, self.screen.greeting) = (Some(user), user, line);
                }
            }
            self.screen.clock += dt;
            self.screen.fault = (self.screen.fault - dt * 0.8).max(0.0);
            self.screen.voice = if self.tts.as_ref().is_some_and(|t| t.is_speaking()) { crate::tts::output_level().clamp(0.0, 1.0) } else { 0.0 };
            let power = |method: &'static str| drop(std::thread::spawn(move || drop(crate::system_power::logind(method))));
            match draw(ctx, &mut self.screen, chrono::Local::now().naive_local()) {
                Some(Asked::Login) => {
                    let user = self.screen.users[self.screen.chosen.min(self.screen.users.len() - 1)].name.clone();
                    lock(&self.shared).busy = true;
                    self.screen.busy = true;
                    let _ = self.logins.send((user, std::mem::take(&mut self.screen.password)));
                }
                Some(Asked::Sleep) => power("Suspend"),
                Some(Asked::Restart) => power("Reboot"),
                Some(Asked::ShutDown) => power("PowerOff"),
                None => {}
            }
            // (The clock ring steps and the core breathes: thirty frames a second are plenty.)
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    /// `jai --greeter`: the login screen, until greetd has a session to start.
    pub fn run() -> std::process::ExitCode {
        let Some(socket) = std::env::var("GREETD_SOCK").ok().filter(|s| !s.is_empty()) else {
            eprintln!("jai --greeter is started by greetd (there is no GREETD_SOCK here).");
            return std::process::ExitCode::from(2);
        };
        let _ = std::env::set_current_dir(HOME);
        let users = users(&std::fs::read_to_string("/etc/passwd").unwrap_or_default());
        let shared = Arc::new(Mutex::new(Shared::default()));
        // The logins: one at a time, on a thread of their own (PAM may take its time).
        let (logins, asked) = mpsc::channel::<(String, String)>();
        let told = Arc::clone(&shared);
        std::thread::spawn(move || {
            let env = ["XDG_SESSION_TYPE=wayland", "XDG_CURRENT_DESKTOP=KDE", "XDG_SESSION_DESKTOP=KDE"].map(String::from).to_vec();
            let mut login = Login::new(session_command(std::fs::read_to_string(SESSION).ok().as_deref()), env);
            for (user, password) in asked {
                let started = log_in(&socket, &mut login, &user, &password);
                let mut shared = lock(&told);
                (shared.busy, shared.done) = (false, started);
                (shared.error, shared.note) = (login.error.clone(), login.note.clone());
                if !started {
                    eprintln!("[GREETER] {user} was not logged in: {}", login.error);
                }
            }
        });
        let tts = voice(Arc::clone(&shared), users.clone());
        let viewport = egui::ViewportBuilder::default().with_title("J.A.I.OS").with_app_id("jai-greeter").with_decorations(false).with_fullscreen(true);
        let options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Glow, persist_window: false, event_loop_builder: crate::gui::on_x11(), ..Default::default() };
        let greeter = Greeter { screen: Screen { users, ..Default::default() }, shared, logins, tts, greeted: None };
        let shown = eframe::run_native(
            "J.A.I.OS",
            options,
            Box::new(move |cc| {
                holo::install_fonts(&cc.egui_ctx);
                crate::gui::configure_style(&cc.egui_ctx, 1.0);
                crate::gui::apply_hud_visuals(&cc.egui_ctx);
                Ok(Box::new(greeter))
            }),
        );
        match shown {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("[GREETER] The login screen could not be shown: {e}");
                std::process::ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage as Canvas};

    fn secret(text: &str) -> Response {
        Response::AuthMessage { auth_message_type: "secret".into(), auth_message: text.into() }
    }

    fn login() -> Login {
        Login::new(vec!["startplasma-wayland".into()], vec!["XDG_SESSION_TYPE=wayland".into()])
    }

    #[test]
    fn greetds_messages_are_a_length_and_json() {
        let message = encode(&Request::CreateSession { username: "max".into() });
        let body = br#"{"type":"create_session","username":"max"}"#;
        assert_eq!(&message[..4], (body.len() as u32).to_ne_bytes());
        assert_eq!(&message[4..], body);
        let json = |request: &Request| String::from_utf8(encode(request)[4..].to_vec()).unwrap();
        assert_eq!(json(&Request::PostAuthMessageResponse { response: Some("geheim123".into()) }), r#"{"type":"post_auth_message_response","response":"geheim123"}"#);
        assert_eq!(json(&Request::PostAuthMessageResponse { response: None }), r#"{"type":"post_auth_message_response"}"#);
        assert_eq!(json(&Request::StartSession { cmd: vec!["startplasma-wayland".into()], env: vec!["A=b".into()] }), r#"{"type":"start_session","cmd":["startplasma-wayland"],"env":["A=b"]}"#);
        assert_eq!(json(&Request::CancelSession), r#"{"type":"cancel_session"}"#);
        // The way back: whole messages only, one at a time, whatever else follows.
        let framed = |json: &str| [(json.len() as u32).to_ne_bytes().to_vec(), json.as_bytes().to_vec()].concat();
        let mut bytes = framed(r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#);
        let first = bytes.len();
        bytes.extend(framed(r#"{"type":"error","error_type":"auth_error","description":"pam_authenticate: AUTH_ERR"}"#));
        assert_eq!(decode(&bytes), Some((Ok(secret("Password: ")), first)));
        assert_eq!(decode(&bytes[first..]).unwrap().0, Ok(Response::Error { error_type: "auth_error".into(), description: "pam_authenticate: AUTH_ERR".into() }));
        assert_eq!(decode(&framed(r#"{"type":"success"}"#)).unwrap().0, Ok(Response::Success));
        assert_eq!(decode(&bytes[..first - 1]), None, "a message still on its way");
        assert_eq!(decode(&bytes[..3]), None);
        assert!(decode(&framed("{}")).unwrap().0.is_err());
    }

    #[test]
    fn a_login_goes_message_by_message() {
        // The right password: the session is made, the question answered, the session started.
        let mut ok = login();
        assert_eq!(ok.submit("max", "geheim123"), Some(Request::CreateSession { username: "max".into() }));
        assert_eq!(ok.submit("erika", "x"), None, "one login at a time");
        assert_eq!(ok.on(secret("Password: ")), Some(Request::PostAuthMessageResponse { response: Some("geheim123".into()) }));
        assert_eq!(ok.on(Response::Success), Some(Request::StartSession { cmd: vec!["startplasma-wayland".into()], env: vec!["XDG_SESSION_TYPE=wayland".into()] }));
        assert_eq!((ok.on(Response::Success), ok.stage), (None, Stage::Done));
        assert!(ok.password.is_empty() && ok.error.is_empty(), "the password is not kept");
        // A wrong one: said so, the session cancelled, and the next try starts afresh.
        let mut wrong = login();
        wrong.submit("max", "falsch");
        wrong.on(secret("Password: "));
        assert_eq!(wrong.on(Response::Error { error_type: "auth_error".into(), description: "pam_authenticate: AUTH_ERR".into() }), Some(Request::CancelSession));
        assert_eq!((wrong.stage, wrong.error.as_str()), (Stage::Cancelling, "Wrong password."));
        assert_eq!((wrong.on(Response::Success), wrong.stage), (None, Stage::Idle));
        assert_eq!(wrong.submit("max", "geheim123"), Some(Request::CreateSession { username: "max".into() }));
        assert!(wrong.error.is_empty());
        // PAM tells something on the way: noted, acknowledged, and the login goes on.
        assert_eq!(wrong.on(Response::AuthMessage { auth_message_type: "info".into(), auth_message: "Your password expires tomorrow.".into() }), Some(Request::PostAuthMessageResponse { response: None }));
        assert_eq!(wrong.note, "Your password expires tomorrow.");
        assert!(matches!(wrong.on(secret("Password: ")), Some(Request::PostAuthMessageResponse { response: Some(_) })));
        // Asked again and again: no login.
        wrong.on(secret("Password: "));
        wrong.on(secret("Password: "));
        assert_eq!(wrong.on(secret("Password: ")), Some(Request::CancelSession));
        // Any other failure: greetd's own words; and one while the session starts.
        let mut other = login();
        other.submit("max", "geheim123");
        assert_eq!(other.on(Response::Error { error_type: "error".into(), description: "a session is already being configured".into() }), Some(Request::CancelSession));
        assert_eq!(other.error, "a session is already being configured");
        assert_eq!((other.on(Response::Error { error_type: "error".into(), description: "no session".into() }), other.stage), (None, Stage::Idle));
        other.submit("max", "geheim123");
        other.on(Response::Success);
        assert_eq!(other.on(Response::Error { error_type: "error".into(), description: "the session's command was not found".into() }), Some(Request::CancelSession));
        // Nothing asked: nothing answered.
        assert_eq!(login().on(Response::Success), None);
    }

    #[test]
    fn the_people_of_this_pc_are_listed_and_its_session_found() {
        let passwd = "root:x:0:0::/root:/usr/bin/bash\nbin:x:1:1::/:/usr/bin/nologin\ngreeter:x:970:970:greetd greeter:/var/lib/greeter:/usr/bin/nologin\nmax:x:1000:1000:Max Mustermann,,,:/home/max:/usr/bin/fish\nerika:x:1001:1001::/home/erika:/bin/bash\ndienst:x:1002:1002:Ein Dienst:/srv/dienst:/usr/bin/nologin\naus:x:1003:1003::/home/aus:/bin/false\nnobody:x:65534:65534:Nobody:/:/usr/bin/nologin\nkaputt:x:abc\n";
        assert_eq!(users(passwd), [User { name: "max".into(), shown: "Max Mustermann".into() }, User { name: "erika".into(), shown: "erika".into() }]);
        assert!(users("").is_empty());
        assert_eq!(session_command(Some("[Desktop Entry]\nName=J.A.I.OS\nExec=/usr/lib/jai/jaios-session --shell %U\nDesktopNames=KDE\n")), ["/usr/lib/jai/jaios-session", "--shell"]);
        assert_eq!(session_command(None), ["startplasma-wayland"]);
    }

    /// A unit vector along axis `i`, leaning `lean` towards axis `j`.
    fn print(i: usize, j: usize, lean: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; 8];
        (v[i], v[j]) = ((1.0 - lean * lean).sqrt(), lean);
        v
    }

    #[test]
    fn a_voice_is_known_only_when_it_is_like_enough_and_clearly_that_one() {
        let max = Voice { user: "max".into(), address: "Sir".into(), print: print(0, 1, 0.0), grown: Vec::new(), bar: 0.42 };
        let erika = Voice { user: "erika".into(), address: "Erika".into(), print: print(2, 1, 0.0), grown: print(3, 1, 0.0), bar: 0.42 };
        let both = [max.clone(), erika.clone()];
        let who = |voices: &[Voice], clip: &[f32]| recognise(voices, clip).map(|v| v.user.clone());
        assert_eq!(who(&both, &print(0, 1, 0.3)).as_deref(), Some("max"), "0.95 like his print");
        assert_eq!(who(&both, &print(3, 1, 0.3)).as_deref(), Some("erika"), "like her voice as it has grown");
        assert_eq!(who(&both, &print(0, 1, 0.95)), None, "0.31: under the bar");
        assert_eq!(who(&both, &print(5, 6, 0.5)), None, "a stranger");
        // At the bar exactly it passes; a breath under it does not.
        let at = |likeness: f32| print(0, 1, (1.0 - likeness * likeness).sqrt());
        assert_eq!(who(&both, &at(0.43)).as_deref(), Some("max"));
        assert_eq!(who(&both, &at(0.41)), None);
        // Two users whose prints the sentence is almost equally like: no guess.
        let twin = Voice { user: "zwilling".into(), print: print(0, 1, 0.1), ..max.clone() };
        assert_eq!(who(&[max.clone(), twin], &print(0, 1, 0.05)), None);
        // A print with no bar is no free pass, one of another size is not compared, and no prints, nobody.
        assert_eq!(who(&[Voice { bar: 0.0, ..max.clone() }], &at(0.2)), None);
        assert_eq!(who(&[Voice { print: vec![1.0; 4], ..max }], &print(0, 1, 0.0)), None);
        assert_eq!(who(&[], &print(0, 1, 0.0)), None);
        // What he says.
        assert_eq!(welcome(20, Some("Sir")), "Good evening, Sir. Your password, please.");
        assert_eq!(welcome(8, Some("Erika")), "Good morning, Erika. Your password, please.");
        assert_eq!(welcome(8, None), "Good morning.");
        assert_eq!(welcome(14, Some(" ")), "Good afternoon.");
    }

    #[test]
    fn stored_prints_are_read_by_their_users_names() {
        let dir = std::env::temp_dir().join(format!("jai_greeter_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("max.json"), serde_json::json!({ "address": "Sir", "print": print(0, 1, 0.0), "grown": [], "bar": 0.42 }).to_string()).unwrap();
        std::fs::write(dir.join("erika.json"), "not json").unwrap();
        std::fs::write(dir.join("leer.json"), r#"{"print":[],"bar":0.4}"#).unwrap();
        std::fs::write(dir.join("notiz.txt"), "x").unwrap();
        let voices = voices_in(&dir);
        assert_eq!(voices.len(), 1);
        assert_eq!((voices[0].user.as_str(), voices[0].address.as_str(), voices[0].bar), ("max", "Sir", 0.42));
        assert!(voices_in(&dir.join("fehlt")).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The screen at rest, with a voice known, and after a wrong password: every part there,
    /// nothing cut off, and the password never in the clear.
    #[test]
    fn panel_greeter_pictures() {
        let _look = crate::holo_theme::look_for_pictures();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 25).unwrap().and_hms_opt(20, 41, 7).unwrap();
        let users = vec![User { name: "max".into(), shown: "Max Mustermann".into() }, User { name: "erika".into(), shown: "Erika".into() }];
        let idle = Screen { users: users.clone(), ..Default::default() };
        let known = Screen { users: users.clone(), greeting: welcome(20, Some("Sir")), heard: 0.5, voice: 0.6, password: "geh".into(), ..Default::default() };
        let wrong = Screen { users, chosen: 1, error: "Wrong password.".into(), fault: 0.8, ..Default::default() };
        for (picture, mut screen, shown) in [("greeter_idle", idle, "PASSWORD"), ("greeter_recognised", known, "Good evening, Sir. Your password, please."), ("greeter_wrong_password", wrong, "Wrong password.")] {
            let mut stage = Canvas::new(vec2(1280.0, 720.0));
            let mut asked = None;
            let frame = stage.frames(4, &mut |ctx| asked = draw(ctx, &mut screen, now));
            stage.raster(&frame).save(picture);
            assert_eq!(asked, None, "{picture}: nothing is asked for by itself");
            assert_eq!(visual::clipped_text(&frame), Vec::<String>::new(), "{picture}");
            assert_eq!(visual::text_over_text(&frame), Vec::<String>::new(), "{picture}");
            for text in [shown, "20:41", "WEDNESDAY 25 NOVEMBER 2026", "MAX MUSTERMANN", "ERIKA", "LOG IN", "SLEEP", "RESTART", "SHUT DOWN", "J.A.I."] {
                assert!(!frame.find_text(text).is_empty(), "'{text}' is not in {picture}");
            }
            assert!(frame.find_text("geh").is_empty(), "{picture}: the password is never shown");
        }
        // LOG IN asks for the login; with no password typed it asks for nothing.
        for (password, wanted) in [("geheim123", Some(Asked::Login)), ("", None)] {
            let mut screen = Screen { users: vec![User { name: "max".into(), shown: "Max".into() }], password: password.into(), ..Default::default() };
            let mut stage = Canvas::new(vec2(1280.0, 720.0));
            let frame = stage.frames(3, &mut |ctx| assert_eq!(draw(ctx, &mut screen, now), None));
            let key = frame.find_text("LOG IN")[0].center();
            let mut asked = None;
            stage.click(key, egui::PointerButton::Primary, &mut |ctx| asked = asked.or(draw(ctx, &mut screen, now)));
            assert_eq!(asked, wanted, "with the password '{password}'");
        }
    }
}
