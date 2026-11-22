//! Waking Sir's other PCs (he chose it on 2026-11-22): "wake up Max's PC", "turn on Max's
//! computer", "weck Max PC auf". Every other PC of his tells its network card's address with each
//! call (`fleet::client`); Sir's PC keeps it by the account's name (`learn`), sends that card a
//! Wake-on-LAN packet (`remote_guardian::send_wol`) and says so once the PC calls again.
//!
//! Only on the same network, and only a PC whose card and firmware let Wake-on-LAN through: one
//! that never answers was asleep too deeply, or is somewhere else (the README's Linux and
//! Windows notes say so).

use crate::kit::say;
use crate::tools::ToolContext;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const KIND: &str = "pc_macs";
/// How long he waits for a woken PC to call before he lets it be.
const WAITS: Duration = Duration::from_secs(240);

/// This PC's network card: the busiest with an address of its own ("aa:bb:cc:dd:ee:ff").
pub fn this_pc_mac() -> Option<String> {
    static MAC: OnceLock<Option<String>> = OnceLock::new();
    MAC.get_or_init(|| {
        let networks = sysinfo::Networks::new_with_refreshed_list();
        networks
            .iter()
            .filter(|(_, data)| !data.mac_address().is_unspecified())
            .max_by_key(|(_, data)| data.total_received() + data.total_transmitted())
            .map(|(_, data)| data.mac_address().to_string())
            .filter(|mac| valid(mac))
    })
    .clone()
}

/// A card's own address: six bytes, not all nought, not a group's.
fn valid(mac: &str) -> bool {
    let hex: String = mac.chars().filter(char::is_ascii_hexdigit).collect();
    hex.len() == 12 && mac.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '-')) && hex != "000000000000" && u8::from_str_radix(&hex[..2], 16).is_ok_and(|b| b & 1 == 0)
}

/// What another PC told in its call: its card, kept by its account's name.
pub fn learn(name: &str, body: &Value) {
    let Some(mac) = body["mac"].as_str().filter(|m| valid(m)) else { return };
    let key = name.to_lowercase();
    if crate::kit::get(KIND, &key).and_then(|v| v["mac"].as_str().map(str::to_string)).as_deref() != Some(mac) {
        let _ = crate::kit::put(KIND, &key, &json!({ "mac": mac }));
    }
}

/// "Wake up Max's PC": the packet sent, and his word once it calls (None names it).
pub async fn wake(ctx: &ToolContext, names: &[String]) -> Result<String, String> {
    if crate::fleet::is_client(&ctx.config.read(|c| c.main_pc.clone())) {
        return Err("Ask me on your main PC, Sir: that is where your PCs' network cards are known.".to_string());
    }
    let (key, mac) = names
        .iter()
        .find_map(|n| crate::kit::get(KIND, &n.to_lowercase()).and_then(|v| v["mac"].as_str().map(|m| (n.to_lowercase(), m.to_string()))))
        .ok_or_else(|| format!("I don't know {}'s PC's network card yet, Sir: J.A.I. has to run there once while it is on.", crate::kit_voice::capitalised(names.last().map_or("that", String::as_str))))?;
    let name = crate::kit_voice::capitalised(&key);
    let lately = Instant::now().checked_sub(Duration::from_secs(crate::fleet::ONLINE_WITHIN as u64)).unwrap_or_else(Instant::now);
    if crate::fleet::heard_since(&key, lately) {
        return Ok(say(format!("{name}'s PC is on already, Sir."), format!("{name}'s PC is on already, Sir.")));
    }
    if crate::platform::in_test_session() || crate::live_self_tester::dry() {
        return Ok(say(format!("Waking {name}'s PC, Sir (not from a test)."), "Waking it, Sir."));
    }
    let broadcast = ctx.config.read(|c| c.satellite.wake_broadcast.clone());
    crate::remote_guardian::send_wol(&mac, if broadcast.trim().is_empty() { "255.255.255.255" } else { broadcast.trim() }).await?;
    let (sent, events, who) = (Instant::now(), ctx.stark.events.clone(), name.clone());
    tokio::spawn(async move {
        while sent.elapsed() < WAITS {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if crate::fleet::heard_since(&key, sent) {
                let _ = events.send(crate::stark::StarkEvent::Say(format!("{who}'s PC is up, Sir.")));
                return;
            }
        }
        let _ = events.send(crate::stark::StarkEvent::Say(format!("{who}'s PC hasn't woken, Sir: Wake-on-LAN may be off in its settings.")));
    });
    Ok(say(format!("Waking {name}'s PC, Sir: I'll tell you when it's up."), format!("Waking {name}'s PC, Sir.")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sir chose it on 2026-11-22: "wake up Max's PC". The order is the fleet's own; a card is
    /// learned from a call, and only a card's own address is kept.
    #[test]
    fn another_pc_is_woken_by_its_name() {
        let _kit = crate::kit::test_lock();
        for said in ["wake up Max's PC", "turn on maxs computer", "wake max's laptop", "weck Max PC auf", "fahr Max PC hoch"] {
            let order = crate::fleet::command(said).unwrap_or_else(|| panic!("{said}"));
            assert_eq!(order["wake"], true, "{said}: {order}");
            assert!(order["names"].as_array().unwrap().iter().any(|n| n == "max"), "{said}: {order}");
        }
        assert_eq!(crate::fleet::command("open the earth on max's pc").map(|o| o["wake"].clone()), Some(Value::Null));
        for bad in ["", "00:00:00:00:00:00", "01:00:5e:00:00:01", "zz:11:22:33:44:55", "11:22:33:44:55"] {
            assert!(!valid(bad), "{bad}");
        }
        learn("Max", &json!({"mac": "a4:bb:6d:12:34:56"}));
        learn("Chris", &json!({"mac": "01:00:5e:00:00:01"}));
        assert_eq!(crate::kit::get(KIND, "max"), Some(json!({"mac": "a4:bb:6d:12:34:56"})));
        assert_eq!(crate::kit::get(KIND, "chris"), None, "a group's address is no card");
    }
}
