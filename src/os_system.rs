//! J.A.I.OS: his root core. `jai --system` runs as root (`jai-system.service`, a D-Bus service
//! on the system bus) and carries out what a user's J.A.I. may not do itself: pacman, the
//! update, snapper's snapshots and the way back to one, Secure Boot's keys, the voice prints the
//! login screen knows Sir by, and a command as root.
//!
//! `org.jaios.System1` at `/org/jaios/System1`:
//!   Install(as) → u, Remove(as) → u, Update() → u, UndoLastUpdate() → u, Rollback(u) → u,
//!   FinishSecureBoot() → u, Run(as) → u     a job each: its number comes back at once;
//!   signals Progress(u job, s line) and Finished(u job, b ok, s summary) follow it
//!   Snapshots() → a(ususs)                   number, type, its pre snapshot (0: none), date, what for
//!   SecureBootStatus() → (bbb)               keys made, firmware in Setup Mode, Secure Boot on
//!   StoreVoice(s json), ForgetVoice()        the caller's own print, never another user's
//!
//! Every method asks the bus for the caller's Unix user and refuses anyone outside the group
//! `jai` (the bus policy in `os/settings` lets no one else send to it either). Whoever is in
//! that group is root by these methods: the questions he asks aloud first (`os_orders`) are
//! J.A.I.'s manners, not a wall. One job runs at a time. A command is always an argument list,
//! never a line for a shell. AUR packages are never built here: paru runs as the user and only
//! its pacman calls come through `Run` (`jai --as-root`).
//!
//! The way back (2026-11-25, read up on CachyOS): its own tool, `limine-snapper-restore`, is a
//! console dialogue with no options, so a service cannot drive it. He uses snapper itself:
//! `snapper undochange <pre>..<post>` takes back exactly what one update changed (snap-pac
//! made the pair), `<n>..0` everything since snapshot n, on the running system; a snapshot of
//! now is made first, and the boot files are rebuilt after (`limine-mkinitcpio`), since the
//! kernel on the ESP is no part of a snapshot. A system that no longer starts is still Limine's:
//! its menu lists the snapshots (limine-snapper-sync).

/// The bus name, and the interface's.
#[cfg(target_os = "linux")]
const BUS: &str = "org.jaios.System1";
#[cfg(target_os = "linux")]
const PATH: &str = "/org/jaios/System1";
/// Who may call the root core.
#[cfg(target_os = "linux")]
const GROUP: &str = "jai";
/// The voice prints the login screen reads: root's, readable by the group `greeter`.
#[cfg(target_os = "linux")]
pub const VOICES: &str = "/var/lib/jaios/voices";
/// What every call answers where there is no root core.
pub const NOT_HERE: &str = "That is J.A.I.OS's own, Sir: this system has no root core of mine.";

/// Whether this is J.A.I.OS (`/etc/os-release`).
pub fn is_jaios() -> bool {
    static IS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS.get_or_init(|| cfg!(target_os = "linux") && std::fs::read_to_string("/etc/os-release").is_ok_and(|text| names_jaios(&text)))
}

fn names_jaios(os_release: &str) -> bool {
    os_release.lines().any(|l| l.strip_prefix("ID=").is_some_and(|id| id.trim().trim_matches(['"', '\'']) == "jaios"))
}

/// A package's name as pacman writes them (never an option, never a path).
pub fn valid_package(name: &str) -> bool {
    !name.is_empty() && name.len() <= 96 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "@._+-".contains(c)) && !name.starts_with(['-', '.'])
}

/// What the root core is asked to do as a job.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub enum Order {
    Install(Vec<String>),
    Remove(Vec<String>),
    Update,
    UndoLastUpdate,
    Rollback(u32),
    FinishSecureBoot,
    Run(Vec<String>),
}

/// One of snapper's snapshots of the root.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub number: u32,
    /// single, pre or post.
    pub kind: String,
    /// A post snapshot's pre (0: none).
    pub pre: u32,
    pub date: String,
    pub about: String,
}

/// `snapper --jsonout -c root list`.
#[cfg(any(target_os = "linux", test))]
fn snapshots_of(json: &str) -> Vec<Snapshot> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let text = |row: &serde_json::Value, key: &str| row[key].as_str().unwrap_or("").to_string();
    v["root"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| Some(Snapshot { number: row["number"].as_u64().filter(|n| *n > 0)? as u32, kind: text(row, "type"), pre: row["pre-number"].as_u64().unwrap_or(0) as u32, date: text(row, "date"), about: text(row, "description") }))
        .collect()
}

/// The last update's pair of snapshots (snap-pac's pre and post).
#[cfg(any(target_os = "linux", test))]
fn last_update(snapshots: &[Snapshot]) -> Option<(u32, u32)> {
    snapshots.iter().rev().find(|s| s.kind == "post" && s.pre > 0 && snapshots.iter().any(|p| p.number == s.pre)).map(|s| (s.pre, s.number))
}

/// `sbctl status --json`: keys made, the firmware in Setup Mode, Secure Boot on.
#[cfg(any(target_os = "linux", test))]
fn secure_boot_of(json: &str) -> (bool, bool, bool) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    (v["installed"].as_bool().unwrap_or(false), v["setup_mode"].as_bool().unwrap_or(false), v["secure_boot"].as_bool().unwrap_or(false))
}

#[cfg(any(target_os = "linux", test))]
fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// Taking back what changed between two snapshots: a snapshot of now first, then snapper, then
/// the boot files (the kernel and its initramfs stand on the ESP, outside every snapshot).
#[cfg(any(target_os = "linux", test))]
fn way_back(range: &str, has: &dyn Fn(&str) -> bool) -> Vec<Vec<String>> {
    vec![
        argv(&["snapper", "-c", "root", "create", "--cleanup-algorithm", "number", "--description", &format!("J.A.I.: before taking back {range}")]),
        argv(&["snapper", "-c", "root", "undochange", range]),
        if has("limine-mkinitcpio") { argv(&["limine-mkinitcpio"]) } else { argv(&["mkinitcpio", "-P"]) },
    ]
}

#[cfg(any(target_os = "linux", test))]
impl Order {
    /// The commands of an order, each an argument list, in turn. `has`: whether a program is
    /// installed; `snapshots` and `setup_mode`: the system as it stands.
    fn steps(&self, has: &dyn Fn(&str) -> bool, snapshots: &[Snapshot], setup_mode: bool) -> Result<Vec<Vec<String>>, String> {
        let packages = |names: &[String], flags: &[&str]| {
            if names.is_empty() || !names.iter().all(|n| valid_package(n)) {
                return Err("That is no package name.".to_string());
            }
            // ("--": whatever follows is a name, never an option.)
            Ok(vec![argv(&["pacman"]).into_iter().chain(argv(flags)).chain(["--".to_string()]).chain(names.iter().cloned()).collect()])
        };
        match self {
            Order::Install(names) => packages(names, &["-S", "--noconfirm", "--needed"]),
            Order::Remove(names) => packages(names, &["-Rs", "--noconfirm"]),
            // (snap-pac's hooks make the pre and post snapshots around the transaction.)
            Order::Update => Ok([argv(&["pacman", "-Syu", "--noconfirm"])].into_iter().chain(has("flatpak").then(|| argv(&["flatpak", "update", "-y", "--noninteractive"]))).collect()),
            Order::UndoLastUpdate => last_update(snapshots).map(|(pre, post)| way_back(&format!("{pre}..{post}"), has)).ok_or_else(|| "No update has a snapshot before and after it.".to_string()),
            Order::Rollback(n) => snapshots.iter().any(|s| s.number == *n).then(|| way_back(&format!("{n}..0"), has)).ok_or_else(|| format!("There is no snapshot {n}.")),
            Order::FinishSecureBoot if !setup_mode => Err("The firmware is not in Setup Mode. Restart into the firmware's setup, clear its Secure Boot keys, start again and ask once more.".to_string()),
            // (CachyOS's own way with Limine: own keys beside Microsoft's, the config's checksum
            // enrolled, then limine-update signs what the firmware starts.)
            Order::FinishSecureBoot => Ok([argv(&["sbctl", "create-keys"]), argv(&["sbctl", "enroll-keys", "--microsoft"])].into_iter().chain(["limine-enroll-config", "limine-update"].into_iter().filter(|tool| has(tool)).map(|tool| argv(&[tool]))).collect()),
            Order::Run(command) if command.first().is_none_or(|program| program.trim().is_empty() || program.starts_with('-')) => Err("That is no command.".to_string()),
            Order::Run(command) => Ok(vec![command.clone()]),
        }
    }
}

/// The name of the user `uid` when it may call the root core: root itself, or a member of
/// `group` (as its primary group or in the group's list).
#[cfg(any(target_os = "linux", test))]
fn member(passwd: &str, groups: &str, uid: u32, group: &str) -> Option<String> {
    let row = passwd.lines().map(|l| l.split(':').collect::<Vec<_>>()).find(|c| c.len() >= 4 && c[2].parse() == Ok(uid))?;
    let (name, primary) = (row[0], row[3]);
    let listed = groups.lines().map(|l| l.split(':').collect::<Vec<_>>()).any(|g| g.len() >= 4 && g[0] == group && (g[2] == primary || g[3].split(',').any(|m| m.trim() == name)));
    (uid == 0 || listed).then(|| name.to_string())
}

/// A voice print as it is stored for the login screen: JSON with a `print` of numbers, small.
#[cfg(any(target_os = "linux", test))]
fn voice_is_sound(json: &str) -> bool {
    json.len() <= 64 * 1024 && serde_json::from_str::<serde_json::Value>(json).is_ok_and(|v| v["print"].as_array().is_some_and(|p| (64..=1024).contains(&p.len()) && p.iter().all(|x| x.as_f64().is_some_and(f64::is_finite))))
}

// ------------------------------------------------------------------ the user's side ----

/// Carries an order out through the root core and waits for it: every line it prints goes to
/// `line`, its last word comes back. Blocks for as long as the job runs.
pub fn ask(order: &Order, line: impl FnMut(&str)) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    return client::ask(order, line);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (order, line);
        Err(NOT_HERE.into())
    }
}

/// Snapper's snapshots of the root, oldest first.
pub fn snapshots() -> Result<Vec<Snapshot>, String> {
    #[cfg(target_os = "linux")]
    return client::call::<_, Vec<(u32, String, u32, String, String)>>("Snapshots", &()).map(|rows| rows.into_iter().map(|(number, kind, pre, date, about)| Snapshot { number, kind, pre, date, about }).collect());
    #[cfg(not(target_os = "linux"))]
    {
        Err(NOT_HERE.into())
    }
}

/// Secure Boot as it stands: keys made, the firmware in Setup Mode, Secure Boot on.
pub fn secure_boot() -> Result<(bool, bool, bool), String> {
    #[cfg(target_os = "linux")]
    return client::call("SecureBootStatus", &());
    #[cfg(not(target_os = "linux"))]
    {
        Err(NOT_HERE.into())
    }
}

/// Hands this user's voice print to the root core for the login screen (None: forget it). Only
/// on J.A.I.OS, and never from a test or a scripted session.
pub fn share_voice(print: Option<serde_json::Value>) {
    if crate::platform::hands_off() || !is_jaios() {
        return;
    }
    #[cfg(target_os = "linux")]
    std::thread::spawn(move || {
        let done = match &print {
            Some(print) => client::call::<_, ()>("StoreVoice", &(print.to_string(),)),
            None => client::call::<_, ()>("ForgetVoice", &()),
        };
        match done {
            Ok(()) => tracing::info!("[OS] The login screen's copy of the voice print is {}", if print.is_some() { "up to date" } else { "gone" }),
            Err(why) => tracing::warn!("[OS] The login screen did not get the voice print: {why}"),
        }
    });
    #[cfg(not(target_os = "linux"))]
    drop(print);
}

/// `jai --as-root <command…>`: one command through the root core, its lines printed here. What
/// paru is given for sudo, so that an AUR package is built as the user and only pacman is root.
pub fn run_as_root(command: &[String]) -> std::process::ExitCode {
    // (sudo's own switches, "-v" to keep a ticket alive: there is no ticket here.)
    if command.first().is_none_or(|c| c.starts_with('-')) {
        return std::process::ExitCode::SUCCESS;
    }
    match ask(&Order::Run(command.to_vec()), |line| println!("{line}")) {
        Ok(_) => std::process::ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{why}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
mod client {
    use super::{Order, BUS, PATH};
    use zbus::blocking::{Connection, Proxy};

    fn proxy() -> Result<Proxy<'static>, String> {
        let bus = Connection::system().map_err(|e| format!("no system bus: {e}"))?;
        Proxy::new(&bus, BUS, PATH, BUS).map_err(|e| e.to_string())
    }

    fn plain(e: zbus::Error) -> String {
        match e {
            zbus::Error::MethodError(_, Some(why), _) => why,
            other => other.to_string(),
        }
    }

    pub fn call<B, R>(method: &str, body: &B) -> Result<R, String>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        proxy()?.call(method, body).map_err(plain)
    }

    pub fn ask(order: &Order, mut line: impl FnMut(&str)) -> Result<String, String> {
        let proxy = proxy()?;
        // (Listening before the call: a short job's last word is not missed.)
        let signals = proxy.receive_all_signals().map_err(|e| e.to_string())?;
        let job: u32 = match order {
            Order::Install(names) => proxy.call("Install", &(names,)),
            Order::Remove(names) => proxy.call("Remove", &(names,)),
            Order::Update => proxy.call("Update", &()),
            Order::UndoLastUpdate => proxy.call("UndoLastUpdate", &()),
            Order::Rollback(n) => proxy.call("Rollback", &(n,)),
            Order::FinishSecureBoot => proxy.call("FinishSecureBoot", &()),
            Order::Run(command) => proxy.call("Run", &(command,)),
        }
        .map_err(plain)?;
        for signal in signals {
            let body = signal.body();
            match signal.header().member().map(|m| m.as_str()) {
                Some("Progress") => {
                    if let Ok((of, text)) = body.deserialize::<(u32, String)>() {
                        if of == job {
                            line(&text);
                        }
                    }
                }
                Some("Finished") => {
                    if let Ok((of, ok, summary)) = body.deserialize::<(u32, bool, String)>() {
                        if of == job {
                            return if ok { Ok(summary) } else { Err(summary) };
                        }
                    }
                }
                _ => {}
            }
        }
        Err("The root core went away before the job was done.".into())
    }
}

// ------------------------------------------------------------------- the root's side ----

/// `jai --system`: the root core, until the system stops it.
#[cfg(target_os = "linux")]
pub fn serve() -> std::process::ExitCode {
    // SAFETY: geteuid has no arguments and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("jai --system is the root core of J.A.I.OS: it is started by jai-system.service, as root.");
        return std::process::ExitCode::from(2);
    }
    let served = zbus::blocking::connection::Builder::system().and_then(|b| b.name(BUS)).and_then(|b| b.serve_at(PATH, server::Core)).and_then(|b| b.build());
    match served {
        Ok(_bus) => {
            eprintln!("[OS] The root core is up as {BUS}");
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            eprintln!("[OS] The root core could not take {BUS} on the system bus: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn serve() -> std::process::ExitCode {
    eprintln!("{NOT_HERE}");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod server {
    use super::{member, secure_boot_of, snapshots_of, voice_is_sound, Order, Snapshot, BUS, GROUP, PATH, VOICES};
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use zbus::fdo::Error;

    pub struct Core;

    static BUSY: AtomicBool = AtomicBool::new(false);
    static NEXT: AtomicU32 = AtomicU32::new(1);

    /// The caller's user name, or the refusal: the bus says which Unix user sent the call.
    async fn caller(header: &zbus::message::Header<'_>, bus: &zbus::Connection) -> Result<String, Error> {
        let denied = |why: &str| Error::AccessDenied(why.to_string());
        let sender = header.sender().ok_or_else(|| denied("a call without a sender"))?.to_owned();
        let uid = zbus::fdo::DBusProxy::new(bus).await.map_err(|e| denied(&e.to_string()))?.get_connection_unix_user(sender.into()).await?;
        let read = |file: &str| std::fs::read_to_string(file).unwrap_or_default();
        member(&read("/etc/passwd"), &read("/etc/group"), uid, GROUP).ok_or_else(|| denied("Only J.A.I.'s own users (the group jai) may ask the root core."))
    }

    fn has(program: &str) -> bool {
        std::env::split_paths("/usr/local/sbin:/usr/local/bin:/usr/bin").any(|dir| dir.join(program).is_file())
    }

    fn output(command: &[&str]) -> Result<String, Error> {
        let out = Command::new(command[0]).args(&command[1..]).env("LC_ALL", "C").stdin(Stdio::null()).output().map_err(|e| Error::Failed(format!("{}: {e}", command[0])))?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    fn snapshots() -> Result<Vec<Snapshot>, Error> {
        output(&["snapper", "--jsonout", "-c", "root", "list"]).map(|json| snapshots_of(&json))
    }

    fn secure_boot() -> Result<(bool, bool, bool), Error> {
        output(&["sbctl", "status", "--json"]).map(|json| secure_boot_of(&json))
    }

    /// Runs the commands in turn, every line of theirs to `line`. Whether all went well, and
    /// the last line said.
    fn run(steps: &[Vec<String>], mut line: impl FnMut(&str)) -> (bool, String) {
        let mut last = String::new();
        for step in steps {
            line(&format!("» {}", step.join(" ")));
            let mut child = match Command::new(&step[0]).args(&step[1..]).env("LC_ALL", "C").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
                Ok(child) => child,
                Err(e) => return (false, format!("{} did not start: {e}", step[0])),
            };
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            let errors = child.stderr.take().map(|pipe| {
                let tx = tx.clone();
                std::thread::spawn(move || BufReader::new(pipe).lines().map_while(Result::ok).for_each(|l| drop(tx.send(l))))
            });
            let out = child.stdout.take().map(|pipe| std::thread::spawn(move || BufReader::new(pipe).lines().map_while(Result::ok).for_each(|l| drop(tx.send(l)))));
            for text in rx {
                if !text.trim().is_empty() {
                    line(&text);
                    last = text;
                }
            }
            for reader in [errors, out].into_iter().flatten() {
                let _ = reader.join();
            }
            match child.wait() {
                Ok(status) if status.success() => {}
                Ok(status) => return (false, format!("{} failed ({status}): {last}", step[0])),
                Err(e) => return (false, format!("{}: {e}", step[0])),
            }
        }
        (true, last)
    }

    /// Starts an order as a job on a thread of its own; its number at once.
    fn start(bus: &zbus::Connection, who: &str, order: Order) -> Result<u32, Error> {
        let setup_mode = matches!(order, Order::FinishSecureBoot) && secure_boot()?.1;
        let known = if matches!(order, Order::UndoLastUpdate | Order::Rollback(_)) { snapshots()? } else { Vec::new() };
        let steps = order.steps(&has, &known, setup_mode).map_err(Error::Failed)?;
        if BUSY.swap(true, Ordering::AcqRel) {
            return Err(Error::Failed("Another job of mine is still running.".into()));
        }
        let job = NEXT.fetch_add(1, Ordering::AcqRel);
        eprintln!("[OS] Job {job} for {who}: {order:?}");
        let bus = zbus::blocking::Connection::from(bus.clone());
        std::thread::spawn(move || {
            let (ok, summary) = run(&steps, |text| drop(bus.emit_signal(None::<&str>, PATH, BUS, "Progress", &(job, text))));
            BUSY.store(false, Ordering::Release);
            eprintln!("[OS] Job {job} {}: {summary}", if ok { "done" } else { "failed" });
            let _ = bus.emit_signal(None::<&str>, PATH, BUS, "Finished", &(job, ok, summary));
        });
        Ok(job)
    }

    /// The group's id (`/etc/group`).
    fn gid(group: &str) -> Option<u32> {
        std::fs::read_to_string("/etc/group").ok()?.lines().map(|l| l.split(':').collect::<Vec<_>>()).find(|g| g.len() >= 3 && g[0] == group)?[2].parse().ok()
    }

    /// Root's, and readable by the login screen's user alone.
    fn for_the_greeter(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).map_err(std::io::Error::other)?;
        // SAFETY: `c` is a NUL-terminated path that lives through the call.
        if unsafe { libc::chown(c.as_ptr(), 0, gid("greeter").unwrap_or(0)) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn voice_file(user: &str) -> std::path::PathBuf {
        // (A user name out of /etc/passwd holds no slash.)
        std::path::Path::new(VOICES).join(format!("{user}.json"))
    }

    #[zbus::interface(name = "org.jaios.System1")]
    impl Core {
        async fn install(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection, packages: Vec<String>) -> Result<u32, Error> {
            start(bus, &caller(&header, bus).await?, Order::Install(packages))
        }

        async fn remove(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection, packages: Vec<String>) -> Result<u32, Error> {
            start(bus, &caller(&header, bus).await?, Order::Remove(packages))
        }

        async fn update(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection) -> Result<u32, Error> {
            start(bus, &caller(&header, bus).await?, Order::Update)
        }

        async fn undo_last_update(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection) -> Result<u32, Error> {
            start(bus, &caller(&header, bus).await?, Order::UndoLastUpdate)
        }

        async fn rollback(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection, snapshot: u32) -> Result<u32, Error> {
            start(bus, &caller(&header, bus).await?, Order::Rollback(snapshot))
        }

        async fn finish_secure_boot(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection) -> Result<u32, Error> {
            start(bus, &caller(&header, bus).await?, Order::FinishSecureBoot)
        }

        async fn run(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection, command: Vec<String>) -> Result<u32, Error> {
            start(bus, &caller(&header, bus).await?, Order::Run(command))
        }

        async fn snapshots(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection) -> Result<Vec<(u32, String, u32, String, String)>, Error> {
            caller(&header, bus).await?;
            Ok(snapshots()?.into_iter().map(|s| (s.number, s.kind, s.pre, s.date, s.about)).collect())
        }

        async fn secure_boot_status(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection) -> Result<(bool, bool, bool), Error> {
            caller(&header, bus).await?;
            secure_boot()
        }

        /// The caller's own voice print (the user is the bus's word, never the caller's).
        async fn store_voice(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection, print: String) -> Result<(), Error> {
            let user = caller(&header, bus).await?;
            if !voice_is_sound(&print) {
                return Err(Error::InvalidArgs("That is no voice print.".into()));
            }
            let failed = |e: std::io::Error| Error::Failed(format!("The voice print could not be stored: {e}"));
            std::fs::create_dir_all(VOICES).map_err(failed)?;
            for_the_greeter(std::path::Path::new(VOICES), 0o750).map_err(failed)?;
            let (file, new) = (voice_file(&user), voice_file(&format!(".{user}")));
            std::fs::write(&new, print).map_err(failed)?;
            for_the_greeter(&new, 0o640).map_err(failed)?;
            std::fs::rename(&new, &file).map_err(failed)
        }

        async fn forget_voice(&self, #[zbus(header)] header: zbus::message::Header<'_>, #[zbus(connection)] bus: &zbus::Connection) -> Result<(), Error> {
            let _ = std::fs::remove_file(voice_file(&caller(&header, bus).await?));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(number: u32, kind: &str, pre: u32) -> Snapshot {
        Snapshot { number, kind: kind.into(), pre, date: String::new(), about: String::new() }
    }

    #[test]
    fn jaios_is_told_by_its_os_release() {
        assert!(names_jaios("NAME=\"J.A.I.OS\"\nID=jaios\nID_LIKE=\"cachyos arch\"\n"));
        assert!(names_jaios("ID=\"jaios\"\n"));
        assert!(!names_jaios("NAME=CachyOS\nID=cachyos\nID_LIKE=arch\n") && !names_jaios("ID_LIKE=jaios\n") && !names_jaios(""));
        assert!(!is_jaios() || cfg!(target_os = "linux"));
    }

    /// The commands exist here as data only: nothing is run.
    #[test]
    fn an_order_becomes_argument_lists_never_a_shell_line() {
        let all = |_: &str| true;
        let none = |_: &str| false;
        let steps = |order: Order| order.steps(&all, &[snapshot(4, "single", 0), snapshot(7, "pre", 0), snapshot(8, "post", 7)], true);
        assert_eq!(steps(Order::Install(vec!["firefox".into(), "obs-studio".into()])).unwrap(), [argv(&["pacman", "-S", "--noconfirm", "--needed", "--", "firefox", "obs-studio"])]);
        assert_eq!(steps(Order::Remove(vec!["vlc".into()])).unwrap(), [argv(&["pacman", "-Rs", "--noconfirm", "--", "vlc"])]);
        // A name is a name: no option, no path, no second command.
        for bad in ["--overwrite=*", "-Rdd", "../../etc/passwd", "vlc; reboot", "vlc firefox", "", "Firefox"] {
            assert!(steps(Order::Install(vec![bad.into()])).is_err() && steps(Order::Remove(vec!["vlc".into(), bad.into()])).is_err(), "{bad}");
        }
        assert!(steps(Order::Install(Vec::new())).is_err());
        assert_eq!(steps(Order::Update).unwrap(), [argv(&["pacman", "-Syu", "--noconfirm"]), argv(&["flatpak", "update", "-y", "--noninteractive"])]);
        assert_eq!(Order::Update.steps(&none, &[], false).unwrap().len(), 1, "without Flatpak, pacman alone");
        // The last update: its own pair, a snapshot of now first, the boot files after.
        let back = steps(Order::UndoLastUpdate).unwrap();
        assert_eq!(back[1], argv(&["snapper", "-c", "root", "undochange", "7..8"]));
        assert_eq!((back[0][..4].to_vec(), back[2].clone()), (argv(&["snapper", "-c", "root", "create"]), argv(&["limine-mkinitcpio"])));
        assert_eq!(Order::Rollback(4).steps(&none, &[snapshot(4, "single", 0)], false).unwrap()[1..], [argv(&["snapper", "-c", "root", "undochange", "4..0"]), argv(&["mkinitcpio", "-P"])]);
        assert!(steps(Order::Rollback(5)).is_err(), "a snapshot that is not there");
        assert!(Order::UndoLastUpdate.steps(&all, &[snapshot(4, "single", 0), snapshot(9, "post", 3)], false).is_err(), "no pair, nothing to take back");
        // Secure Boot only while the firmware takes keys.
        assert!(Order::FinishSecureBoot.steps(&all, &[], false).unwrap_err().contains("Setup Mode"));
        assert_eq!(steps(Order::FinishSecureBoot).unwrap(), [argv(&["sbctl", "create-keys"]), argv(&["sbctl", "enroll-keys", "--microsoft"]), argv(&["limine-enroll-config"]), argv(&["limine-update"])]);
        // A command as root is its own words, one argument each.
        assert_eq!(steps(Order::Run(argv(&["systemctl", "restart", "bluetooth"]))).unwrap(), [argv(&["systemctl", "restart", "bluetooth"])]);
        assert!(steps(Order::Run(Vec::new())).is_err() && steps(Order::Run(argv(&["-rf", "/"]))).is_err() && steps(Order::Run(argv(&[" "]))).is_err());
    }

    #[test]
    fn only_the_group_jai_and_root_may_call() {
        let passwd = "root:x:0:0::/root:/usr/bin/bash\nmax:x:1000:1000:Max Mustermann:/home/max:/usr/bin/bash\nerika:x:1001:1001::/home/erika:/usr/bin/bash\ngast:x:1002:984::/home/gast:/usr/bin/bash\ngreeter:x:970:970::/:/usr/bin/nologin\n";
        let groups = "root:x:0:\nwheel:x:998:max,erika\njai:x:984:max\nmax:x:1000:\nerika:x:1001:\ngreeter:x:970:\n";
        assert_eq!(member(passwd, groups, 1000, "jai").as_deref(), Some("max"), "in the group's list");
        assert_eq!(member(passwd, groups, 1002, "jai").as_deref(), Some("gast"), "the group as the primary one");
        assert_eq!(member(passwd, groups, 0, "jai").as_deref(), Some("root"));
        assert_eq!(member(passwd, groups, 1001, "jai"), None, "wheel alone is not enough");
        assert_eq!(member(passwd, groups, 970, "jai"), None, "the login screen's user");
        assert_eq!(member(passwd, groups, 4242, "jai"), None, "a user nobody knows");
        assert_eq!(member(passwd, "jai:x:984:maximilian\n", 1000, "jai"), None, "a name is a whole name");
    }

    #[test]
    fn snappers_list_and_sbctls_status_are_read() {
        let list = r#"{"root":[{"number":0,"type":"single","pre-number":null,"date":"","description":"current"},{"number":12,"type":"pre","pre-number":null,"date":"2026-11-24 18:02:11","description":"pacman -Syu"},{"number":13,"type":"post","pre-number":12,"date":"2026-11-24 18:04:40","description":"firefox linux-cachyos"}]}"#;
        let snaps = snapshots_of(list);
        assert_eq!(snaps.len(), 2, "the running system (0) is no snapshot");
        assert_eq!(snaps[1], Snapshot { number: 13, kind: "post".into(), pre: 12, date: "2026-11-24 18:04:40".into(), about: "firefox linux-cachyos".into() });
        assert_eq!(last_update(&snaps), Some((12, 13)));
        assert!(snapshots_of("no json").is_empty() && last_update(&[]).is_none());
        assert_eq!(secure_boot_of(r#"{"installed":true,"guid":"x","setup_mode":true,"secure_boot":false,"vendors":["microsoft"]}"#), (true, true, false));
        assert_eq!(secure_boot_of(""), (false, false, false));
    }

    #[test]
    fn a_voice_print_for_the_login_screen_is_checked() {
        let print = serde_json::json!({ "address": "Sir", "bar": 0.42, "print": vec![0.0625f32; 256] }).to_string();
        assert!(voice_is_sound(&print));
        assert!(!voice_is_sound("{}") && !voice_is_sound("not json") && !voice_is_sound(r#"{"print":[1,2,3]}"#) && !voice_is_sound(r#"{"print":"/etc/shadow"}"#));
        assert!(!voice_is_sound(&serde_json::json!({ "print": vec![0.5f32; 256], "pad": "x".repeat(70_000) }).to_string()), "nothing large");
        // Off J.A.I.OS (and in every test) nothing is handed anywhere.
        share_voice(Some(serde_json::json!({ "print": [] })));
    }
}
