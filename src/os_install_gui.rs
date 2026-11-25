//! J.A.I.OS's installer, its window (`jai --os-install`): eight numbered pages in his look,
//! in English or German, from the welcome to the restart. What it finds and does is
//! `os_install_run`'s, what it plans `os_install`'s; this file only asks and shows.

use super::{strip_ink, strip_muted};
use crate::config::{ConfigStore, JaiConfig};
use crate::holo::{self, a, Face};
use crate::os_install::{self, Disk, Plan, Target, MIB};
use crate::os_install_run::{self, Found};
use crate::{holo_kit, holo_ui};
use eframe::egui::{self, pos2, vec2, Align, Align2, Frame, Layout, Margin, Rect, RichText, Sense, Ui, UiBuilder, ViewportCommand};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::runtime::Handle;

const ID: &str = "os_install";
const RAIL: f32 = 196.0;
const PAGES: [(&str, &str); 8] = [("Welcome", "Willkommen"), ("Place and keys", "Ort und Tasten"), ("Disk", "Festplatte"), ("Account", "Konto"), ("User", "Benutzer"), ("Summary", "Übersicht"), ("Install", "Installation"), ("Done", "Fertig")];
const INSTALL: usize = 6;

#[derive(Clone, Copy, PartialEq)]
enum Way {
    Free,
    Erase,
    Replace,
}

/// The account at J.A.I.: made here (a Main PC) or at the Main PC this one joins.
#[derive(Default)]
struct Account {
    register: bool,
    main: String,
    name: String,
    password: String,
    /// The key from the Main PC's screen, once an account was asked for.
    key: String,
    asked: bool,
    busy: bool,
    said: (String, bool),
    /// Settled: the Main PC's address (none: this PC is one itself).
    done: Option<String>,
}

#[derive(Default)]
struct Progress {
    step: usize,
    lines: Vec<String>,
    result: Option<Result<(), String>>,
}

pub(super) struct Wizard {
    found: Found,
    german: bool,
    page: usize,
    zone: String,
    zone_search: String,
    layout: String,
    layout_search: String,
    key_test: String,
    way: Way,
    disk: usize,
    part: String,
    typed: String,
    encrypt: bool,
    passphrase: [String; 2],
    account: Arc<Mutex<Account>>,
    full_name: String,
    user: String,
    password: [String; 2],
    host: String,
    plan: Option<Arc<Plan>>,
    progress: Arc<Mutex<Progress>>,
    /// Why the page does not go on.
    said: String,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the account's files are made before there is a disk to put them on (on the stick's
/// live system, in memory).
fn stage() -> PathBuf {
    PathBuf::from(os_install::WORK).join("stage")
}

/// J.A.I.'s settings over an empty stage: the account's database and the vault's key go there.
fn staged() -> ConfigStore {
    let dir = stage();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    ConfigStore::new(JaiConfig { memory_db_path: dir.join("jai_memory.db").to_string_lossy().into_owned(), ..Default::default() })
}

impl Wizard {
    pub(super) fn new(found: Found) -> Self {
        // Beside Windows where that can be, else the first disk that is not the stick.
        let beside = found.disks.iter().position(Disk::beside_windows);
        let disk = beside.or_else(|| found.disks.iter().position(|d| !d.live)).unwrap_or(0);
        Self {
            german: false,
            page: 0,
            zone: "Europe/Berlin".into(),
            zone_search: String::new(),
            layout: found.layouts.first().map_or_else(|| "de".into(), |(code, _)| code.clone()),
            layout_search: String::new(),
            key_test: String::new(),
            way: if beside.is_some() { Way::Free } else { Way::Erase },
            disk,
            part: String::new(),
            typed: String::new(),
            encrypt: false,
            passphrase: Default::default(),
            account: Default::default(),
            full_name: String::new(),
            user: String::new(),
            password: Default::default(),
            host: "jaios".into(),
            plan: None,
            progress: Default::default(),
            said: String::new(),
            found,
        }
    }

    fn t(&self, en: &'static str, de: &'static str) -> &'static str {
        if self.german {
            de
        } else {
            en
        }
    }

    fn target(&self) -> Target {
        let disk = self.found.disks.get(self.disk).map(|d| d.path.clone()).unwrap_or_default();
        match self.way {
            Way::Free => Target::Free(disk),
            Way::Erase => Target::Erase(disk),
            Way::Replace => Target::Replace(self.part.clone()),
        }
    }

    fn choices(&self) -> os_install::Choices {
        let account = lock(&self.account);
        os_install::Choices {
            german: self.german,
            zone: self.zone.clone(),
            layout: self.layout.clone(),
            target: self.target(),
            passphrase: self.encrypt.then(|| self.passphrase[0].clone()),
            full_name: self.full_name.trim().to_string(),
            user: self.user.trim().to_string(),
            password: self.password[0].clone(),
            host: self.host.trim().to_string(),
            main_pc: account.done.clone().unwrap_or_default(),
            account: account.name.trim().to_string(),
            stage: stage().to_string_lossy().replace('\\', "/"),
        }
    }

    /// NEXT: what the page asked is checked; `Err` stays on it and says why.
    fn next(&mut self) -> Result<(), String> {
        let german = self.german;
        match self.page {
            2 => {
                let (disk, _, _) = os_install::room(&self.found.disks, &self.target()).map_err(|e| e.get(german).to_string())?;
                if self.way == Way::Erase && self.typed.trim() != disk.name() {
                    return Err(format!("{} {}", self.t("To erase the disk, type its name:", "Zum Löschen den Namen der Festplatte eintippen:"), disk.name()));
                }
                if self.encrypt && self.passphrase[0] != self.passphrase[1] {
                    return Err(self.t("The two passphrases differ.", "Die beiden Passphrasen sind verschieden.").into());
                }
                if self.encrypt && self.passphrase[0].chars().count() < 8 {
                    return Err(self.t("The passphrase needs 8 characters or more.", "Die Passphrase braucht mindestens 8 Zeichen.").into());
                }
            }
            3 => {
                let mut account = lock(&self.account);
                if account.register && account.done.is_none() {
                    return Err(self.t("Log in at the Main PC first, or ask it for an account.", "Melde dich zuerst am Haupt-PC an oder bitte ihn um ein Konto.").into());
                }
                if !account.register {
                    // This PC becomes a Main PC: its admin account, made in the stage. Nothing
                    // is contacted. (A preview makes none: on Windows that would go through
                    // Sir's own vault key.)
                    if self.found.real {
                        crate::admin::set_up(&staged(), &account.name, &account.password)?;
                    } else if account.name.trim().chars().count() < 3 {
                        return Err(self.t("A name has 3 to 20 characters.", "Ein Name hat 3 bis 20 Zeichen.").into());
                    } else {
                        crate::accounts::check(&account.password, false)?;
                    }
                    account.done = Some(String::new());
                }
            }
            4 => {
                if self.password[0] != self.password[1] {
                    return Err(self.t("The two passwords differ.", "Die beiden Passwörter sind verschieden.").into());
                }
                self.plan = Some(Arc::new(os_install::plan(&self.found.disks, &self.choices(), &self.found.machine).map_err(|e| e.get(german).to_string())?));
            }
            5 => self.start(),
            _ => {}
        }
        self.page += 1;
        Ok(())
    }

    /// The plan is carried out on a thread of its own; the window hears every line.
    fn start(&mut self) {
        let (Some(plan), progress, real) = (self.plan.clone(), self.progress.clone(), self.found.real) else { return };
        std::thread::spawn(move || {
            let result = os_install_run::carry_out(&plan, real, &|step, line| {
                let mut p = lock(&progress);
                p.step = step;
                p.lines.push(line);
                if p.lines.len() > 400 {
                    p.lines.drain(..200);
                }
                drop(p);
                super::request_repaint();
            });
            lock(&progress).result = Some(result);
            super::request_repaint();
        });
    }

    fn installing(&self) -> bool {
        self.page == INSTALL && lock(&self.progress).result.is_none()
    }
}

fn say(ui: &mut Ui, text: &str, face: Face, size: f32, colour: egui::Color32) {
    ui.label(RichText::new(text).font(holo::font(face, size)).color(colour));
}

fn body_text(ui: &mut Ui, text: &str) {
    say(ui, text, Face::Body, 13.5, strip_muted());
}

/// A page's numbered heading.
fn heading(ui: &mut Ui, page: usize, german: bool) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    let (en, de) = PAGES[page];
    holo_kit::section(ui.painter(), r.left_top(), r.width(), &format!("{:02}", page + 1), &(if german { de } else { en }).to_uppercase(), 1.0);
    ui.add_space(8.0);
}

/// A field to search in and the matches under it (`shown` of them): the chosen one is lit.
fn pick(ui: &mut Ui, title: &str, hint: &str, search: &mut String, all: &[(String, String)], chosen: &mut String, shown: usize) -> bool {
    super::login_view::field(ui, title, search, hint, false);
    let wanted = search.trim().to_lowercase();
    let matches = all.iter().filter(|(code, name)| wanted.is_empty() || code.to_lowercase().contains(&wanted) || name.to_lowercase().contains(&wanted));
    // (What is chosen stays in sight, first.)
    let first = all.iter().filter(|(code, _)| code == chosen && wanted.is_empty());
    let rows: Vec<&(String, String)> = first.chain(matches.filter(|(code, _)| code != chosen || !wanted.is_empty())).take(shown).collect();
    let mut changed = false;
    for (code, name) in rows {
        let label = if name.is_empty() { code.clone() } else { format!("{name}  ·  {code}") };
        if holo_ui::item(ui, label, code == chosen).clicked() && code != chosen {
            *chosen = code.clone();
            changed = true;
        }
    }
    changed
}

/// A disk as a bar: every partition and every free room in its share of the width (none too
/// thin to see), what belongs to Windows as plain structure, where J.A.I.OS would go lit.
/// A click chooses the disk, and the partition under it.
fn disk_bar(ui: &mut Ui, w: &mut Wizard, index: usize) {
    let t = holo::theme();
    let german = w.german;
    let disk = &w.found.disks[index];
    let chosen = index == w.disk;
    let room = if chosen { os_install::room(&w.found.disks, &w.target()).ok().map(|(_, from, to)| (from, to)) } else { None };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 62.0), Sense::click());
    let p = ui.painter();
    let ink = if disk.live { t.faint() } else if chosen { t.ink() } else { t.muted() };
    let name = holo::text(p, rect.left_top(), Align2::LEFT_TOP, Face::Label, &disk.name().to_uppercase(), 9.0, if chosen { t.accent() } else { ink });
    let about = holo::text(p, pos2(name.right() + 10.0, name.center().y), Align2::LEFT_CENTER, Face::Mono, &format!("{} · {}", disk.model, os_install::size(disk.size)), 11.0, ink);
    let tag = match () {
        _ if disk.live => Some(if german { "DER STICK" } else { "THE STICK" }),
        _ if disk.has_windows() => Some("WINDOWS"),
        _ => None,
    };
    if let Some(tag) = tag {
        holo_kit::chip(p, pos2(about.right() + 10.0, about.center().y - 7.0), tag, ink, false, 1.0);
    }
    // The pieces, by where they start (MiB): partitions, and free rooms worth showing.
    let mut pieces: Vec<(u64, u64, Option<&os_install::Part>)> = disk.parts.iter().map(|part| (part.start / MIB, (part.start + part.size).div_ceil(MIB), Some(part))).collect();
    pieces.extend(disk.free().into_iter().filter(|(from, to)| to - from >= 64).map(|(from, to)| (from, to, None)));
    if pieces.is_empty() {
        pieces.push((0, disk.size / MIB, None));
    }
    pieces.sort_by_key(|piece| piece.0);
    let bar = Rect::from_min_max(pos2(rect.left(), rect.top() + 18.0), pos2(rect.right(), rect.top() + 44.0));
    let total = (disk.size / MIB).max(1) as f32;
    let widths: Vec<f32> = pieces.iter().map(|(from, to, _)| ((to - from) as f32 / total * bar.width()).max(34.0)).collect();
    let scale = bar.width() / widths.iter().sum::<f32>();
    let mut x = bar.left();
    let mut clicked = None;
    for ((from, to, part), width) in pieces.iter().zip(&widths) {
        let piece = Rect::from_min_max(pos2(x + 1.0, bar.top()), pos2(x + width * scale - 1.0, bar.bottom()));
        x += width * scale;
        let takes = room.is_some_and(|(room_from, room_to)| *from < room_to && *to > room_from);
        let outline = holo::plate(piece, [0.0, 0.0, 5.0, 0.0]);
        let (fill, line, words) = match part {
            _ if takes => (a(t.accent(), 0.17), t.accent(), t.ink()),
            Some(part) if part.is_windows() || part.is_esp() => (a(t.ink(), 0.07), a(t.ink(), 0.45), t.muted()),
            Some(_) => (a(t.ink(), 0.03), a(t.ink(), 0.45), t.muted()),
            None => (egui::Color32::TRANSPARENT, a(t.ink(), 0.18), t.faint()),
        };
        holo::fill(p, &outline, fill);
        holo::outline(p, &outline, line);
        let called = match part {
            _ if takes => "J.A.I.OS".to_string(),
            Some(part) => part.called().to_uppercase(),
            None => (if german { "FREI" } else { "FREE" }).to_string(),
        };
        // (Named under the bar where the name fits its piece; a sliver stays unnamed.)
        let label = holo::galley(p, Face::Label, &called, 8.0, words);
        if label.size().x + 6.0 <= piece.width() {
            let figure = holo::galley(p, Face::Mono, &os_install::size((to - from) * MIB), 10.0, words);
            let fits = label.size().x + figure.size().x + 14.0 <= piece.width();
            p.galley(pos2(piece.left() + 2.0, bar.bottom() + 5.0), label.clone(), words);
            if fits {
                p.galley(pos2(piece.left() + label.size().x + 10.0, bar.bottom() + 3.0), figure, words);
            }
        }
        if let (Some(part), true) = (part, response.hover_pos().is_some_and(|at| piece.contains(at))) {
            clicked = Some(part.path.clone());
        }
    }
    if response.clicked() {
        w.disk = index;
        w.typed.clear();
        if let (Way::Replace, Some(part)) = (w.way, clicked) {
            w.part = part;
        }
    }
}

fn page(ui: &mut Ui, w: &mut Wizard, rt: &Handle) {
    let german = w.german;
    let t = |en: &'static str, de: &'static str| if german { de } else { en };
    heading(ui, w.page, german);
    match w.page {
        0 => {
            say(ui, "J.A.I.OS", Face::Head, 30.0, strip_ink());
            ui.add_space(4.0);
            body_text(ui, t("J.A.I. as the operating system of this PC: his desktop, his login, his voice, on Arch Linux with CachyOS's kernel.", "J.A.I. als Betriebssystem dieses PCs: sein Desktop, seine Anmeldung, seine Stimme, auf Arch Linux mit dem Kernel von CachyOS."));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if holo_ui::key_on(ui, "ENGLISH", !german).clicked() {
                    w.german = false;
                }
                if holo_ui::key_on(ui, "DEUTSCH", german).clicked() {
                    w.german = true;
                }
            });
            ui.add_space(12.0);
            for line in [
                t("You choose the place and the keyboard, where on the disk he goes, your account and a user.", "Du wählst den Ort und die Tastatur, den Platz auf der Festplatte, dein Konto und einen Benutzer."),
                t("Before anything is changed, a summary lists every change. Windows and its partitions are never touched.", "Bevor etwas geändert wird, zeigt eine Übersicht jede Änderung. Windows und seine Partitionen werden nie angerührt."),
                t("Then the system is fetched from the internet and set up. That takes a while; this PC needs to stay online.", "Dann wird das System aus dem Internet geholt und eingerichtet. Das dauert eine Weile; dieser PC muss online bleiben."),
            ] {
                body_text(ui, line);
                ui.add_space(4.0);
            }
            if !w.found.real {
                ui.add_space(10.0);
                say(ui, t("A preview on a sample PC: nothing on this PC is changed. J.A.I.OS is installed from its stick.", "Eine Vorschau an einem Beispiel-PC: An diesem PC wird nichts geändert. J.A.I.OS wird von seinem Stick installiert."), Face::Body, 13.5, holo::theme().warn());
            }
        }
        1 => {
            ui.columns(2, |column| {
                let zones: Vec<(String, String)> = w.found.zones.iter().map(|z| (z.clone(), String::new())).collect();
                pick(&mut column[0], t("Time zone", "Zeitzone"), t("search: Berlin", "suchen: Berlin"), &mut w.zone_search, &zones, &mut w.zone, 9);
                if pick(&mut column[1], t("Keyboard", "Tastatur"), t("search: German", "suchen: German"), &mut w.layout_search, &w.found.layouts, &mut w.layout, 6) {
                    os_install_run::keyboard(&w.layout);
                }
                column[1].add_space(8.0);
                super::login_view::field(&mut column[1], t("Try the keys here", "Tasten hier ausprobieren"), &mut w.key_test, "z y ä ö ü ß @ €", false);
            });
        }
        2 => {
            for index in 0..w.found.disks.len() {
                disk_bar(ui, w, index);
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                for (way, en, de) in [(Way::Free, "BESIDE WINDOWS, INTO FREE SPACE", "NEBEN WINDOWS, IN FREIEN PLATZ"), (Way::Erase, "ERASE THIS DISK", "DIESE FESTPLATTE LÖSCHEN"), (Way::Replace, "REPLACE A PARTITION", "EINE PARTITION ERSETZEN")] {
                    if holo_ui::key_on(ui, if german { de } else { en }, w.way == way).clicked() {
                        w.way = way;
                    }
                }
            });
            ui.add_space(6.0);
            match os_install::room(&w.found.disks, &w.target()) {
                Ok((disk, from, to)) => say(ui, &format!("{} {} · {} · btrfs", t("J.A.I.OS goes on", "J.A.I.OS kommt auf"), disk.path, os_install::size((to - from) * MIB)), Face::Mono, 12.0, strip_ink()),
                Err(_) if w.way == Way::Replace && w.part.is_empty() => body_text(ui, t("Click the partition J.A.I.OS shall take the place of.", "Klicke die Partition an, deren Platz J.A.I.OS einnehmen soll.")),
                Err(why) => say(ui, why.get(german), Face::Body, 13.5, holo::theme().alert()),
            }
            ui.add_space(6.0);
            match w.way {
                Way::Free => body_text(ui, t("The free space is made from Windows beforehand (J.A.I. on Windows: \"make room for J.A.I.OS\"). Nothing of Windows is changed here.", "Den freien Platz schafft man vorher von Windows aus (J.A.I. unter Windows: „mach Platz für J.A.I.OS“). Hier wird nichts an Windows geändert.")),
                Way::Erase => {
                    let name = w.found.disks.get(w.disk).map(|d| d.name().to_string()).unwrap_or_default();
                    super::login_view::field(ui, &format!("{} {name}", t("Everything on it is lost. To erase it, type", "Alles darauf geht verloren. Zum Löschen eintippen:")), &mut w.typed, &name, false);
                }
                Way::Replace => body_text(ui, t("Everything on that partition is lost. Windows' partitions cannot be chosen.", "Alles auf dieser Partition geht verloren. Partitionen von Windows lassen sich nicht wählen.")),
            }
            holo_ui::rule(ui);
            holo_ui::toggle(ui, &mut w.encrypt, t("Encrypt J.A.I.OS (LUKS2): a passphrase at every start", "J.A.I.OS verschlüsseln (LUKS2): eine Passphrase bei jedem Start"));
            if w.encrypt {
                ui.columns(2, |column| {
                    super::login_view::field(&mut column[0], t("Passphrase", "Passphrase"), &mut w.passphrase[0], t("8 characters or more", "mindestens 8 Zeichen"), true);
                    super::login_view::field(&mut column[1], t("Once more", "Noch einmal"), &mut w.passphrase[1], "", true);
                });
            }
        }
        3 => account_page(ui, w, rt),
        4 => {
            ui.set_max_width(460.0);
            super::login_view::field(ui, t("Your name", "Dein Name"), &mut w.full_name, "Max Mustermann", false);
            super::login_view::field(ui, t("User name", "Benutzername"), &mut w.user, t("small letters: max", "Kleinbuchstaben: max"), false);
            ui.columns(2, |column| {
                super::login_view::field(&mut column[0], t("Password", "Passwort"), &mut w.password[0], "", true);
                super::login_view::field(&mut column[1], t("Once more", "Noch einmal"), &mut w.password[1], "", true);
            });
            super::login_view::field(ui, t("This PC's name", "Name dieses PCs"), &mut w.host, "jaios", false);
            body_text(ui, t("The user logs in to J.A.I.OS with this password, by voice once J.A.I. knows it, and has root powers through sudo.", "Mit diesem Passwort meldet sich der Benutzer an J.A.I.OS an, per Stimme, sobald J.A.I. sie kennt, und hat Root-Rechte über sudo."));
        }
        5 => {
            for (index, (line, lost)) in w.plan.iter().flat_map(|plan| &plan.summary).enumerate() {
                ui.horizontal_top(|ui| {
                    let colour = if *lost { holo::theme().alert() } else { strip_ink() };
                    say(ui, &format!("{:02}", index + 1), Face::Mono, 11.0, if *lost { colour } else { holo::theme().accent() });
                    say(ui, line.get(german), Face::Body, 13.5, colour);
                });
                ui.add_space(3.0);
            }
        }
        INSTALL => install_page(ui, w),
        _ => {
            say(ui, t("J.A.I.OS is installed.", "J.A.I.OS ist installiert."), Face::Head, 22.0, strip_ink());
            ui.add_space(6.0);
            body_text(ui, t("Take the stick out when the PC restarts. The start menu shows J.A.I.OS and Windows; it starts what was started last after 3 seconds.", "Zieh den Stick ab, wenn der PC neu startet. Das Startmenü zeigt J.A.I.OS und Windows; nach 3 Sekunden startet, was zuletzt gestartet wurde."));
            ui.add_space(12.0);
            if holo_ui::key(ui, t("RESTART", "NEU STARTEN")).clicked() {
                os_install_run::restart();
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }
}

/// LOG IN: Sir's admin account, on this PC, which becomes a Main PC. REGISTER: this PC joins
/// a Main PC, with an account there or one asked for (the key on that PC's screen).
fn account_page(ui: &mut Ui, w: &mut Wizard, rt: &Handle) {
    let real = w.found.real;
    let mut guard = lock(&w.account);
    let account = &mut *guard;
    ui.set_max_width(460.0);
    ui.add_enabled_ui(!account.busy, |ui| {
        ui.horizontal(|ui| {
            for (register, label) in [(false, w.t("LOG IN", "ANMELDEN")), (true, w.t("REGISTER", "REGISTRIEREN"))] {
                if holo_ui::key_on(ui, label, account.register == register).clicked() && account.register != register {
                    *account = Account { register, name: std::mem::take(&mut account.name), ..Default::default() };
                }
            }
        });
        ui.add_space(6.0);
        if let (true, Some(main)) = (account.register, &account.done) {
            say(ui, &format!("{} {} · {main}", w.t("Logged in as", "Angemeldet als"), account.name.trim()), Face::Mono, 12.0, strip_ink());
            return;
        }
        if account.register {
            body_text(ui, w.t("This PC joins a Main PC: log in with your account there, or ask for one. Then a key appears on the Main PC's screen.", "Dieser PC schließt sich einem Haupt-PC an: Melde dich mit deinem Konto dort an oder bitte um eines. Dann erscheint ein Schlüssel auf dem Bildschirm des Haupt-PCs."));
            ui.add_space(4.0);
            super::login_view::field(ui, w.t("The Main PC's address", "Adresse des Haupt-PCs"), &mut account.main, "https://…", false);
        } else {
            body_text(ui, w.t("This PC becomes a Main PC: the name and the password of your admin account at J.A.I. Nothing is contacted.", "Dieser PC wird ein Haupt-PC: Name und Passwort deines Admin-Kontos bei J.A.I. Es wird nichts kontaktiert."));
            ui.add_space(4.0);
        }
        super::login_view::field(ui, "Name", &mut account.name, w.t("3 to 20 characters", "3 bis 20 Zeichen"), false);
        super::login_view::field(ui, w.t("Password", "Passwort"), &mut account.password, w.t("3 to 20 characters", "3 bis 20 Zeichen"), true);
        if account.asked {
            super::login_view::field(ui, w.t("The key on the Main PC's screen", "Der Schlüssel auf dem Bildschirm des Haupt-PCs"), &mut account.key, w.t("six digits", "sechs Ziffern"), false);
        }
        if account.register {
            ui.horizontal(|ui| {
                let finish = account.asked;
                let log_in = holo_ui::key(ui, if finish { w.t("CREATE ACCOUNT", "KONTO ANLEGEN") } else { w.t("LOG IN THERE", "DORT ANMELDEN") }).clicked();
                let ask = !finish && holo_ui::key(ui, w.t("ASK FOR AN ACCOUNT", "UM EIN KONTO BITTEN")).clicked();
                if (log_in || ask) && !real {
                    // A preview calls no PC and keeps no key: it only goes on.
                    account.said = (w.t("A preview: the Main PC is not called.", "Eine Vorschau: Der Haupt-PC wird nicht angerufen.").into(), false);
                    account.done = Some(account.main.trim().to_string());
                } else if log_in || ask {
                    account.busy = true;
                    account.said = (w.t("One moment…", "Einen Moment…").into(), false);
                    let (shared, main, name, password, key) = (w.account.clone(), account.main.clone(), account.name.trim().to_string(), account.password.clone(), finish.then(|| account.key.clone()));
                    rt.spawn(async move {
                        let result = if ask {
                            crate::fleet::ask_for_account(&main, &name, &password).await.map(|said| (said, None))
                        } else {
                            let config = staged();
                            crate::fleet::log_in(&config, &main, &name, &password, key.as_deref()).await.map(|_| (String::new(), Some(config.read(|c| c.main_pc.clone()))))
                        };
                        let mut account = lock(&shared);
                        account.busy = false;
                        match result {
                            Ok((said, done)) => {
                                account.asked |= ask;
                                account.done = done;
                                account.said = (said, false);
                            }
                            Err(e) => account.said = (e, true),
                        }
                        drop(account);
                        super::request_repaint();
                    });
                }
            });
        }
    });
    let (said, fault) = &account.said;
    if !said.is_empty() {
        say(ui, said, Face::Body, 13.5, if *fault { holo::theme().alert() } else { strip_ink() });
    }
}

/// The steps, how far it is, and the last lines of what the commands say.
fn install_page(ui: &mut Ui, w: &mut Wizard) {
    let t = holo::theme();
    let Some(plan) = w.plan.clone() else { return };
    let progress = lock(&w.progress);
    let finished = progress.result.is_some();
    for (index, step) in plan.steps.iter().enumerate() {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width().min(420.0), 22.0), Sense::hover());
        let now = index == progress.step && !finished;
        let state = match () {
            _ if index < progress.step || finished && progress.result == Some(Ok(())) => w.t("done", "fertig"),
            _ if now => w.t("working", "läuft"),
            _ => "",
        };
        holo_kit::row(ui.painter(), r, &format!("{:02}", index + 1), step.title.get(w.german), state, if now { 1.0 } else { 0.0 }, 0.0, if index <= progress.step { 1.0 } else { 0.55 });
    }
    ui.add_space(8.0);
    let share = if progress.result == Some(Ok(())) { 1.0 } else { progress.step as f32 / plan.steps.len().max(1) as f32 };
    holo_ui::bar(ui, share, ui.available_width());
    ui.add_space(8.0);
    if let Some(Err(why)) = &progress.result {
        say(ui, &format!("{} {why}", w.t("The installation stopped:", "Die Installation hat angehalten:")), Face::Body, 13.5, t.alert());
        body_text(ui, w.t("The disk was let go of. What was made so far stays; the installer can be started again.", "Die Festplatte wurde wieder freigegeben. Was bis hierher angelegt wurde, bleibt; der Installer kann neu gestartet werden."));
        ui.add_space(6.0);
    }
    // The log: as many of its last lines as fit, each cut to the width.
    let log = ui.available_rect_before_wrap();
    let p = ui.painter();
    holo::fill(p, &holo::plate(log, [0.0; 4]), a(t.ink(), 0.03));
    let rows = ((log.height() - 12.0) / 15.0).floor().max(0.0) as usize;
    let from = progress.lines.len().saturating_sub(rows);
    for (row, line) in progress.lines[from..].iter().enumerate() {
        let colour = if line.starts_with(' ') { t.faint() } else { t.muted() };
        let g = holo::galley_within(p, Face::Mono, line, 10.5, colour, log.width() - 20.0);
        p.galley(pos2(log.left() + 10.0, log.top() + 6.0 + row as f32 * 15.0), g, colour);
    }
    drop(progress);
    if lock(&w.progress).result == Some(Ok(())) {
        w.page = INSTALL + 1;
    }
}

/// The whole window: the pages' rail on the left, the page, BACK and NEXT under it.
pub(super) fn body(ui: &mut Ui, w: &mut Wizard, rt: &Handle) {
    let t = holo::theme();
    let full = ui.available_rect_before_wrap();
    let p = ui.painter();
    for (index, (en, de)) in PAGES.iter().enumerate() {
        let r = Rect::from_min_size(pos2(full.left(), full.top() + index as f32 * 30.0), vec2(RAIL, 28.0));
        let reached = if index <= w.page { 1.0 } else { 0.5 };
        holo_kit::row(p, r, &format!("{:02}", index + 1), if w.german { de } else { en }, "", if index == w.page { 1.0 } else { 0.0 }, 0.0, reached);
    }
    holo::rule(p, pos2(full.left() + RAIL + 9.0, full.top()), pos2(full.left() + RAIL + 9.0, full.bottom()), a(t.ink(), 0.12));
    let keys = Rect::from_min_max(pos2(full.left() + RAIL + 20.0, full.bottom() - 30.0), full.max);
    let sheet = Rect::from_min_max(pos2(keys.left(), full.top()), pos2(full.right(), keys.top() - 34.0));
    ui.allocate_new_ui(UiBuilder::new().max_rect(sheet), |ui| page(ui, w, rt));
    let said = Rect::from_min_max(pos2(keys.left(), keys.top() - 30.0), pos2(full.right(), keys.top() - 4.0));
    if !w.said.is_empty() {
        ui.allocate_new_ui(UiBuilder::new().max_rect(said), |ui| say(ui, &w.said, Face::Body, 13.5, t.alert()));
    }
    ui.allocate_new_ui(UiBuilder::new().max_rect(keys).layout(Layout::right_to_left(Align::Center)), |ui| {
        if w.page >= INSTALL {
            return;
        }
        let forward = if w.page == INSTALL - 1 { w.t("INSTALL", "INSTALLIEREN") } else { w.t("NEXT", "WEITER") };
        let lost = w.page == INSTALL - 1 && w.plan.as_ref().is_some_and(|plan| plan.summary.iter().any(|(_, lost)| *lost));
        let go = if lost { holo_ui::key_alert(ui, forward) } else { holo_ui::key(ui, forward) };
        if go.clicked() {
            w.said = w.next().err().unwrap_or_default();
        }
        if w.page > 0 && holo_ui::key(ui, w.t("BACK", "ZURÜCK")).clicked() {
            w.page -= 1;
            w.said.clear();
        }
    });
}

struct App {
    rt: Handle,
    wizard: Wizard,
    frames: u8,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().frame(Frame::none().fill(super::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
            // (No leaving while the disk is being worked on.)
            if super::detached_title_bar(ui, ctx, ID, if self.wizard.german { "Installieren // J.A.I.OS" } else { "Install // J.A.I.OS" }) && !self.wizard.installing() {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            ui.add_space(8.0);
            body(ui, &mut self.wizard, &self.rt);
        });
        super::paint_window_border(ctx);
        // The first frame is on the screen when the second is asked for.
        self.frames = self.frames.saturating_add(1);
        if self.frames == 2 {
            os_install_run::announce_ready();
        } else if self.frames < 2 {
            ctx.request_repaint();
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// The installer's window (`os_install_run::run`).
pub fn run(rt: Handle) -> Result<(), String> {
    let viewport = egui::ViewportBuilder::default().with_title("Install J.A.I.OS").with_app_id("jai").with_inner_size([1040.0, 700.0]).with_min_inner_size([940.0, 640.0]).with_decorations(false).with_icon(Arc::new(super::window_icon().unwrap_or_default()));
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        centered: true,
        persist_window: false,
        #[cfg(not(windows))]
        event_loop_builder: super::on_x11(),
        ..Default::default()
    };
    eframe::run_native(
        "J.A.I.OS setup",
        options,
        Box::new(move |cc| {
            holo::install_fonts(&cc.egui_ctx);
            super::configure_style(&cc.egui_ctx, 1.0);
            super::apply_hud_visuals(&cc.egui_ctx);
            *super::lock(&super::SHARED.ctx) = Some(cc.egui_ctx.clone());
            Ok(Box::new(App { rt, wizard: Wizard::new(os_install_run::look()), frames: 0 }))
        }),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage};

    fn filled() -> Wizard {
        let mut w = Wizard::new(os_install_run::look());
        (w.full_name, w.user, w.password, w.zone_search) = ("Max Mustermann".into(), "max".into(), ["secret word".into(), "secret word".into()], "eu".into());
        *lock(&w.account) = Account { name: "Max".into(), password: "hidden".into(), ..Default::default() };
        w
    }

    /// Every page, in English and in German: nothing cut off, nothing written over, no
    /// password shown; and the way through them checks what each page asked.
    #[test]
    fn every_page_of_the_os_installer_in_both_languages() {
        let _look = crate::holo_theme::look_for_pictures();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let mut stage = Stage::new(vec2(1040.0, 700.0));
        for german in [false, true] {
            let mut w = filled();
            w.german = german;
            let mut names = Vec::new();
            for page in 0..PAGES.len() {
                w.page = page;
                let mut variants = vec![""];
                match page {
                    2 => variants = vec!["", "_erase", "_replace"],
                    3 => variants = vec!["", "_register"],
                    5 => w.plan = Some(Arc::new(os_install::plan(&w.found.disks, &w.choices(), &w.found.machine).unwrap())),
                    INSTALL => *lock(&w.progress) = Progress { step: 2, lines: vec!["pacstrap -C /run/jaios-install/pacman.conf /mnt base base-devel linux-cachyos linux-cachyos-headers linux-firmware amd-ucode intel-ucode mkinitcpio sudo".into(), "  (41/212) installing linux-cachyos".into()], result: None },
                    _ => {}
                }
                for variant in variants {
                    match variant {
                        "_erase" => (w.way, w.disk, w.encrypt) = (Way::Erase, 1, true),
                        "_replace" => (w.way, w.part) = (Way::Replace, "/dev/sda1".into()),
                        "_register" => *lock(&w.account) = Account { register: true, asked: true, main: "https://jai.example.ts.net".into(), name: "Max".into(), password: "hidden".into(), said: ("A key is on the Main PC's screen now.".into(), false), ..Default::default() },
                        _ => {}
                    }
                    let frame = stage.frames(4, &mut |ctx| {
                        egui::CentralPanel::default().frame(Frame::none().fill(crate::gui::block_slate()).inner_margin(Margin::same(14.0))).show(ctx, |ui| body(ui, &mut w, rt.handle()));
                    });
                    let name = format!("os_install_{}_{}{variant}_{}", page + 1, PAGES[page].0.to_lowercase().replace(' ', "_"), if german { "de" } else { "en" });
                    stage.raster(&frame).save(&name);
                    assert_eq!(visual::clipped_text(&frame), Vec::<String>::new(), "{name}");
                    assert_eq!(visual::text_over_text(&frame), Vec::<String>::new(), "{name}");
                    for secret in ["secret word", "hidden"] {
                        assert!(frame.find_text(secret).is_empty(), "a password shows in {name}");
                    }
                    assert!(!frame.find_text(&if german { PAGES[page].1 } else { PAGES[page].0 }.to_uppercase()).is_empty(), "{name} has no heading");
                    names.push(name);
                }
                if page == 2 {
                    (w.way, w.disk, w.encrypt) = (Way::Free, 0, false);
                }
                if page == 3 {
                    *lock(&w.account) = Account { name: "Max".into(), password: "hidden".into(), done: Some(String::new()), ..Default::default() };
                }
            }
            assert_eq!(names.len(), 11);
        }
    }

    #[test]
    fn a_page_goes_on_only_with_what_it_asked() {
        let mut w = filled();
        // Sir's PC: beside Windows is chosen by itself, on the disk that has the room.
        assert!(w.way == Way::Free && w.disk == 0 && w.target() == Target::Free("/dev/nvme0n1".into()));
        w.page = 2;
        // Erasing wants the disk's name typed; the stick is never taken; a partition of
        // Windows neither.
        (w.way, w.disk) = (Way::Erase, 1);
        assert!(w.next().unwrap_err().ends_with(" sda") && w.page == 2);
        w.disk = 2;
        assert!(w.next().unwrap_err().contains("the stick"));
        (w.way, w.part) = (Way::Replace, "/dev/nvme0n1p3".into());
        assert!(w.next().unwrap_err().contains("never takes it"));
        (w.way, w.disk, w.typed) = (Way::Erase, 1, "sda".into());
        (w.encrypt, w.passphrase) = (true, ["correct horse".into(), "correct house".into()]);
        w.german = true;
        assert!(w.next().unwrap_err().contains("verschieden"));
        w.passphrase[1] = "correct horse".into();
        assert_eq!((w.next(), w.page), (Ok(()), 3));
        // Joining a Main PC needs the login there first.
        lock(&w.account).register = true;
        assert!(w.next().is_err() && w.page == 3);
        lock(&w.account).done = Some("https://jai.example.ts.net".into());
        assert_eq!((w.next(), w.page), (Ok(()), 4));
        // The user: both passwords alike, a name Linux takes; then the plan stands.
        w.password[1] = "another".into();
        assert!(w.next().is_err() && w.plan.is_none());
        (w.password[1], w.user) = ("secret word".into(), "Max M".into());
        assert!(w.next().unwrap_err().contains("Benutzername"));
        w.user = "max".into();
        assert_eq!((w.next(), w.page), (Ok(()), 5));
        let plan = w.plan.clone().unwrap();
        assert!(plan.summary[0].1 && plan.summary.iter().any(|(s, _)| s.de.contains("https://jai.example.ts.net")));
        assert!(plan.shown().iter().any(|l| l.starts_with("cryptsetup luksFormat")));
    }
}
