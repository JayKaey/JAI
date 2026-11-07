//! "What's new?": what this build does that the last one did not, the way Sir would ask for it.

/// One sentence each, spoken as they stand.
pub const NOTES: &[&str] = &[
    "\"Leave orb mode\" and \"turn off orb mode\" bring the whole HUD back.",
    "\"Set every volume to 50\" sets each app at once, and \"my PC's volume\" is the master.",
    "\"Make Discord full screen\" is about Discord, and \"move Discord to another monitor\" works.",
    "\"Clear my VRAM\" hands the graphics card back to you right away, and \"go to standby\" works too.",
    "\"Make the interface rose\" switches to the rose theme.",
    "\"Make the opacity 0\" stops at 15 percent and says so, so you can still find me.",
    "\"Kannst du dich selbst schließen?\" closes me, even when it is misheard.",
    "\"Don't make the windows overlap\" keeps them apart instead of being taken as a no.",
    "\"Open the integrations hub\" opens it by voice.",
    "\"Play my playlist called workout\" finds the list by its name, whatever words come around it.",
    "\"Füge einen Termin hinzu\" adds an event, and the word terminal is never taken for one.",
    "A typed slip like \"stop listeting\" still does what you meant.",
    "A late yes to a restart or shutdown question that has lapsed runs nothing, and I say so.",
    "\"What happens if I restart my PC?\" is told what it does, and nothing happens.",
    "\"Put my PC into shutdown mode\" is understood, and I still ask once first.",
    "F11 from a window gives the true fullscreen board again, not a maximised window underneath.",
];

/// The answer to "what's new?".
pub fn spoken() -> String {
    format!("New in this build, Sir. {}", NOTES.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notes_can_be_spoken() {
        assert!(!NOTES.is_empty());
        assert!(NOTES.iter().all(|note| note.ends_with('.') && note.len() < 140), "one short sentence each");
    }
}
