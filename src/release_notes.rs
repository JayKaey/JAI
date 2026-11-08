//! "What's new?": what this build does that the last one did not, the way Sir would ask for it.

/// One sentence each, spoken as they stand.
pub const NOTES: &[&str] = &[
    "\"Check the network\" opens the network radar and sweeps it, and \"who is on my wifi?\" names the devices.",
    "\"Is my network secure?\" looks over the devices on the radar and tells you what stands out.",
    "\"What is taking up space?\" maps your drive as a treemap, and \"map my D drive\" maps D.",
    "\"Show me the earth\" opens the globe, \"switch to satellite view\" changes it, and \"fly to Tokyo\" goes there.",
    "Earthquakes near a place, the flights over a city, storms, fires and the space station show on the globe.",
    "\"Close the globe\" closes it, also when it has a window of its own.",
    "My windows open with a holographic entrance and leave with an exit, by voice or by hand.",
    "A status pill at the top shows whether I am listening, thinking or speaking.",
    "While you speak, your voice shows as a ring of ticks round my emblem; the VIEW menu turns it off.",
    "F1 mutes the microphone, and the key hints at the bottom right can be clicked.",
    "\"Explain what's on my screen\" reads what is behind my own windows, and a card offers quick actions.",
    "Drop a file on the HUD and ask about it, for example \"summarize this file\".",
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
