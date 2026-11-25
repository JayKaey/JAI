//! The line between the assistant and J.A.I.OS's shell, on the session bus. The assistant
//! holds `org.jaios.Jai` with the object `/Jai`: the property `State` (idle, listening,
//! thinking, speaking; its change is signalled as any property's is), `Order(text)`, which he
//! takes as if it had been typed into the HUD, and `SetFavourites(ids)`, the [J] menu's pins,
//! which live in his settings. The shell (`os_bus`) shows the state as its lamp and core and
//! sends what is typed into its menu. While the shell runs, it draws the desktop: the
//! assistant then is no desktop background of his own.

use crate::kde;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};
use zbus::zvariant::Value;

pub const JAI: &str = "org.jaios.Jai";
pub const JAI_PATH: &str = "/Jai";

static STATE: Mutex<String> = Mutex::new(String::new());
static SERVED: AtomicBool = AtomicBool::new(false);
static SHELL_UP: AtomicBool = AtomicBool::new(false);

enum Ask {
    Order(String),
    Favourites(Vec<String>),
}

struct Link(UnboundedSender<Ask>);

#[zbus::interface(name = "org.jaios.Jai")]
impl Link {
    #[zbus(property)]
    fn state(&self) -> String {
        state()
    }

    fn order(&self, text: String) {
        let _ = self.0.send(Ask::Order(text));
    }

    fn set_favourites(&self, ids: Vec<String>) {
        let _ = self.0.send(Ask::Favourites(ids));
    }
}

fn state() -> String {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.is_empty() { "idle".into() } else { state.clone() }
}

/// The assistant's state changed (`app::Jai::set_state`): the shell's lamp follows.
pub fn state_changed(now: &str) {
    {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if *state == now {
            return;
        }
        *state = now.to_string();
    }
    if SERVED.load(Ordering::Acquire) {
        if let Ok(bus) = kde::bus() {
            let changed = HashMap::from([("State", Value::from(now))]);
            let _ = bus.emit_signal(None::<&str>, JAI_PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged", &(JAI, changed, Vec::<&str>::new()));
        }
    }
}

/// Whether J.A.I.OS's shell runs: it draws the desktop, with his core on it.
pub fn shell_has_desktop() -> bool {
    SHELL_UP.load(Ordering::Acquire)
}

/// The assistant takes his name on the bus and answers the shell. Not in a scripted session:
/// the name belongs to the J.A.I. Sir runs.
pub fn serve(app: crate::app::Jai) {
    if crate::platform::in_test_session() {
        return;
    }
    let (asks, mut asked) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(ask) = asked.recv().await {
            match ask {
                Ask::Order(text) if !text.trim().is_empty() => {
                    info!("[SHELL] Typed into the menu: {text}");
                    app.handle_command(crate::hud::HudCommand::Chat { text }).await;
                }
                Ask::Order(_) => {}
                Ask::Favourites(ids) => app.config().update(|c| c.os_favourites = ids),
            }
        }
    });
    let started = std::thread::Builder::new().name("jai-os-link".into()).spawn(move || {
        let Ok(bus) = kde::bus() else { return };
        match bus.object_server().at(JAI_PATH, Link(asks)).and_then(|_| bus.request_name(JAI)) {
            Ok(()) => SERVED.store(true, Ordering::Release),
            Err(e) => warn!("[SHELL] The line to the desktop shell is not there: {e}"),
        }
        loop {
            let up = kde::call::<_, bool>("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner", &(crate::os_bus::SHELL,)).unwrap_or(false);
            if SHELL_UP.swap(up, Ordering::AcqRel) != up {
                info!("[SHELL] {}", if up { "J.A.I.OS's shell draws the desktop" } else { "J.A.I.OS's shell is gone" });
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
    if let Err(e) = started {
        warn!("[SHELL] No thread for the line to the desktop shell: {e}");
    }
}
