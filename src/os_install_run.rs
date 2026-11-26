//! J.A.I.OS's installer, its hands: what this PC is (disks, firmware, processor), and the one
//! place a plan of `os_install` is carried out. That happens only as root on the stick's live
//! system (`may_install`); anywhere else, on Windows too, `jai --os-install` is a preview on a
//! sample PC that reads its plan out and changes nothing.

use crate::os_install::{self, Action, Disk, Ids, Level, Machine, Plan, SecureBoot, ROOT, WORK};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

/// Whether this J.A.I. may install: root, on the stick's live system, in no test.
pub fn may_install() -> bool {
    #[cfg(target_os = "linux")]
    let root_on_the_stick = unsafe { libc::geteuid() } == 0 && Path::new("/run/archiso").exists();
    #[cfg(not(target_os = "linux"))]
    let root_on_the_stick = false;
    root_on_the_stick && !crate::platform::hands_off() && !crate::live_self_tester::dry()
}

/// What the installer's window starts from.
pub struct Found {
    pub disks: Vec<Disk>,
    pub machine: Machine,
    pub zones: Vec<String>,
    pub layouts: Vec<(String, String)>,
    /// False: a preview on a sample PC.
    pub real: bool,
}

/// A few zones for the preview (the stick reads the system's own list).
const SAMPLE_ZONES: &str = "DE\t+5230+01322\tEurope/Berlin\nGB\t+513030-0000731\tEurope/London\nAT\t+4813+01620\tEurope/Vienna\nCH\t+4723+00832\tEurope/Zurich\nUS\t+404251-0740023\tAmerica/New_York\nUS\t+340308-1181434\tAmerica/Los_Angeles\nJP\t+353916+1394441\tAsia/Tokyo\n";

fn output(program: &str, args: &[&str]) -> String {
    Command::new(program).args(args).stdin(Stdio::null()).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Whether Windows' boot manager lies on this partition: mounted read-only for the look.
fn holds_bootmgr(part: &str) -> bool {
    let at = format!("{WORK}/esp");
    if std::fs::create_dir_all(&at).is_err() || !Command::new("mount").args(["-o", "ro", part, &at]).status().is_ok_and(|s| s.success()) {
        return false;
    }
    let found = Path::new(&at).join("EFI/Microsoft/Boot/bootmgfw.efi").is_file();
    let _ = Command::new("umount").arg(&at).status();
    found
}

/// This PC on the stick; a sample PC anywhere else.
pub fn look() -> Found {
    let real = may_install();
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let (lsblk, zones, layouts, cpuinfo) = if real {
        (output("lsblk", &["--json", "--bytes", "--output", "NAME,PATH,TYPE,SIZE,MODEL,PTTYPE,FSTYPE,LABEL,PARTTYPE,PARTUUID,PARTN,START,MOUNTPOINTS"]), read("/usr/share/zoneinfo/zone1970.tab"), read("/usr/share/X11/xkb/rules/base.lst"), read("/proc/cpuinfo"))
    } else {
        (os_install::SAMPLE.to_string(), SAMPLE_ZONES.to_string(), String::new(), String::new())
    };
    let mut disks = os_install::disks(&lsblk);
    for part in disks.iter_mut().flat_map(|d| &mut d.parts).filter(|p| p.is_esp()) {
        part.bootmgr = if real { holds_bootmgr(&part.path) } else { true };
    }
    // (efivarfs: four bytes of attributes, then the value.)
    let firmware = |name: &str| std::fs::read(format!("/sys/firmware/efi/efivars/{name}-8be4df61-93ca-11d2-aa0d-00e098032b8c")).ok().and_then(|b| b.get(4).copied());
    let secure_boot = if real { SecureBoot::of(firmware("SetupMode"), firmware("SecureBoot")) } else { SecureBoot::of(Some(1), Some(0)) };
    let machine = Machine { uefi: !real || Path::new("/sys/firmware/efi").exists(), level: Level::of_this_pc(&cpuinfo), secure_boot, vram_mb: crate::installer::vram_mb(), card: card(), ids: Ids::fresh() };
    Found { disks, machine, zones: os_install::zones(&zones), layouts: os_install::layouts(&layouts), real }
}

/// The graphics card's maker, from the PCI bus' display controllers (class 03): NVIDIA before
/// AMD before Intel, for a laptop's two. (None off Linux.)
fn card() -> Option<&'static str> {
    let makers: Vec<String> = std::fs::read_dir("/sys/bus/pci/devices").into_iter().flatten().flatten().filter_map(|d| {
        let read = |f: &str| std::fs::read_to_string(d.path().join(f)).unwrap_or_default().trim().to_string();
        read("class").starts_with("0x03").then(|| read("vendor"))
    }).collect();
    [("0x10de", "nvidia"), ("0x1002", "amd"), ("0x8086", "intel")].into_iter().find(|(id, _)| makers.iter().any(|m| m == id)).map(|(_, maker)| maker)
}

/// The live session's keyboard takes the layout at once, so the test field and the passwords
/// are typed on the keys Sir chose (KWin reads kxkbrc again when told).
pub fn keyboard(layout: &str) {
    if !may_install() {
        return;
    }
    for argv in [vec!["kwriteconfig6", "--file", "kxkbrc", "--group", "Layout", "--key", "Use", "true"], vec!["kwriteconfig6", "--file", "kxkbrc", "--group", "Layout", "--key", "LayoutList", layout], vec!["dbus-send", "--session", "--type=signal", "/Layouts", "org.kde.keyboard.reloadConfig"]] {
        let _ = Command::new(argv[0]).args(&argv[1..]).stdin(Stdio::null()).status();
    }
}

/// The PC starts anew, into what was installed (a preview only closes).
pub fn restart() {
    if may_install() {
        let _ = Command::new("systemctl").arg("reboot").status();
    }
}

/// One command or file of a plan, for real.
fn done(action: &Action, tell: &(dyn Fn(String) + Sync)) -> Result<(), String> {
    match action {
        Action::Write { path, text, mode } => {
            let path = Path::new(path);
            std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/"))).and_then(|_| std::fs::write(path, text)).map_err(|e| format!("{} could not be written: {e}", path.display()))?;
            #[cfg(unix)]
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(*mode)).map_err(|e| e.to_string())?;
            #[cfg(not(unix))]
            let _ = mode;
            Ok(())
        }
        Action::Run { argv, stdin, soft } => {
            let name = &argv[0];
            let went_on = || tell(format!("  ({name} did not succeed; the installation goes on without it)"));
            let mut child = match Command::new(name).args(&argv[1..]).stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
                Ok(child) => child,
                Err(_) if *soft => return Ok(went_on()),
                Err(e) => return Err(format!("{name} did not start: {e}")),
            };
            // (Fed and closed at once: a passphrase is short, and the command waits for the end.)
            if let (Some(feed), Some(mut pipe)) = (stdin, child.stdin.take()) {
                let _ = std::io::Write::write_all(&mut pipe, feed.as_bytes());
            }
            fn lines(from: impl std::io::Read, tell: &(dyn Fn(String) + Sync)) {
                for line in std::io::BufRead::lines(std::io::BufReader::new(from)).map_while(Result::ok) {
                    tell(format!("  {line}"));
                }
            }
            let (out, err) = (child.stdout.take(), child.stderr.take());
            std::thread::scope(|s| {
                if let Some(err) = err {
                    s.spawn(move || lines(err, tell));
                }
                if let Some(out) = out {
                    lines(out, tell);
                }
            });
            match child.wait() {
                Ok(s) if s.success() => Ok(()),
                Ok(_) if *soft => Ok(went_on()),
                Ok(s) => Err(format!("{name} failed ({s}).")),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

/// Carries a plan out, step by step; `tell` hears each step's number and every line. When a
/// step fails the rest is left undone, and the disk is let go of either way. Not `real`: the
/// plan is only read out.
pub fn carry_out(plan: &Plan, real: bool, tell: &(dyn Fn(usize, String) + Sync)) -> Result<(), String> {
    let log = std::sync::Mutex::new(Vec::new());
    let perform = |step: usize, action: &Action| {
        let heard = |line: String| {
            log.lock().unwrap_or_else(|e| e.into_inner()).push(line.clone());
            tell(step, line);
        };
        heard(action.shown());
        if real && may_install() {
            return done(action, &heard);
        }
        std::thread::sleep(Duration::from_millis(30));
        Ok(())
    };
    let result = plan.steps.iter().enumerate().try_for_each(|(i, step)| step.actions.iter().try_for_each(|action| perform(i, action)));
    if real && may_install() {
        // (The whole log stays on the new system, for whoever looks into a failed start.)
        let _ = std::fs::write(format!("{ROOT}/var/log/jaios-install.log"), log.lock().unwrap_or_else(|e| e.into_inner()).join("\n"));
    }
    for action in &plan.release {
        let _ = perform(plan.steps.len().saturating_sub(1), action);
    }
    result
}

/// What CI waits for on the serial console: the installer's window has drawn.
pub fn ready_line() -> &'static str {
    "JAIOS-INSTALLER-READY"
}

/// Says it once, on the standard output and in the log.
pub fn announce_ready() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        println!("{}", ready_line());
        tracing::info!("{}", ready_line());
    });
}

/// `jai --os-install`: the installer's window.
pub fn run() -> ExitCode {
    let Ok(rt) = tokio::runtime::Runtime::new() else { return ExitCode::FAILURE };
    match crate::gui::os_install_view::run(rt.handle().clone()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("The installer's window did not open: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test never installs: it gets the sample PC, and a plan carried out there only reads
    /// itself out, the letting go included.
    #[test]
    fn outside_the_stick_a_plan_is_only_read_out() {
        assert!(!may_install());
        let found = look();
        assert!(!found.real && found.disks.len() == 3 && found.disks[0].windows_esp().is_some_and(|p| p.bootmgr));
        assert!(found.zones.contains(&"Europe/Berlin".to_string()) && found.layouts[0].0 == "de");
        let choices = os_install::Choices { german: false, zone: "Europe/Berlin".into(), layout: "de".into(), target: os_install::Target::Erase("/dev/sda".into()), passphrase: Some("correct horse".into()), full_name: "Max Mustermann".into(), user: "max".into(), password: "secret".into(), host: "jaios".into(), main_pc: String::new(), account: "Max".into(), stage: String::new() };
        let plan = os_install::plan(&found.disks, &choices, &found.machine).unwrap();
        let heard = std::sync::Mutex::new(Vec::new());
        // (`real` asked for: still nothing runs, since this is no stick.)
        carry_out(&plan, true, &|step, line| heard.lock().unwrap().push((step, line))).unwrap();
        let heard = heard.into_inner().unwrap();
        assert_eq!(heard.first().unwrap(), &(0, "wipefs --all /dev/sda".to_string()));
        assert_eq!(heard.last().unwrap(), &(plan.steps.len() - 1, "cryptsetup close jaios".to_string()));
        assert_eq!(heard.len(), plan.shown().len() + 2);
        assert!(heard.iter().all(|(_, line)| !line.contains("secret") && !line.contains("correct horse")));
        assert_eq!(ready_line(), "JAIOS-INSTALLER-READY");
    }
}
