//! Smart website navigation in Sir's default browser, in three modes:
//!
//! * `direct_url` — "open YouTube", "open github.com": 70+ known sites or any domain.
//! * `channel_or_profile` — "open BastiGHG on YouTube", "torvalds on GitHub", "r/rust": straight
//!   to the creator's channel / profile (`youtube.com/@<handle>`, `twitch.tv/<handle>` …). A
//!   YouTube handle is checked in the background; when it does not exist (or Sir said a display
//!   name with spaces), YouTube's channel search supplies the real `/@Handle`.
//! * `site_search` — "open YouTube and search for Minecraft", "RTX 3060 Ti on Kleinanzeigen":
//!   the platform's own URL-encoded search page.
//!
//! Whatever opens, the browser becomes the conversation focus (`focus`), so a following "full
//! screen" or "close it" acts on it.

use crate::tools::str_arg;
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::Duration;
use tracing::{debug, info};

pub struct Site {
    pub id: &'static str,
    pub display: &'static str,
    /// Spoken names besides `id` (lower-case).
    aliases: &'static [&'static str],
    home: &'static str,
    /// `{q}` = URL-encoded query, `{slug}` = hyphenated words, `{lang}` = de/en.
    search: Option<&'static str>,
    /// `{handle}` = the creator / account / repository.
    profile: Option<&'static str>,
}

impl Site {
    pub fn home_url(&self) -> String {
        self.home.to_string()
    }

    pub fn search_url(&self, query: &str, lang: &str) -> Option<String> {
        let slug: String = query.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>().join("-");
        self.search.map(|t| {
            t.replace("{q}", &urlencoding::encode(query.trim()))
                .replace("{slug}", &urlencoding::encode(&slug))
                .replace("{lang}", lang)
        })
    }

    pub fn profile_url(&self, handle: &str) -> Option<String> {
        let h = clean_handle(self.id, handle);
        if h.is_empty() {
            return None;
        }
        // "u/spez" is a Reddit user, everything else a subreddit.
        if self.id == "reddit" && handle.trim().trim_start_matches('/').starts_with("u/") {
            return Some(format!("https://www.reddit.com/user/{h}/"));
        }
        Some(self.profile?.replace("{handle}", &h))
    }
}

macro_rules! site {
    ($id:literal, $display:literal, [$($alias:literal),*], $home:literal, $search:expr, $profile:expr) => {
        Site { id: $id, display: $display, aliases: &[$($alias),*], home: $home, search: $search, profile: $profile }
    };
}

pub static SITES: &[Site] = &[
    // Video, streaming, social
    site!("youtube", "YouTube", ["yt", "you tube", "youtub", "u tube", "youtube.com"], "https://www.youtube.com", Some("https://www.youtube.com/results?search_query={q}"), Some("https://www.youtube.com/@{handle}")),
    site!("twitch", "Twitch", ["twitch tv", "twitch.tv", "twich"], "https://www.twitch.tv", Some("https://www.twitch.tv/search?term={q}"), Some("https://www.twitch.tv/{handle}")),
    site!("kick", "Kick", ["kick.com"], "https://kick.com", Some("https://kick.com/search?query={q}"), Some("https://kick.com/{handle}")),
    site!("tiktok", "TikTok", ["tik tok", "tiktok.com"], "https://www.tiktok.com", Some("https://www.tiktok.com/search?q={q}"), Some("https://www.tiktok.com/@{handle}")),
    site!("instagram", "Instagram", ["insta", "ig", "instagram.com"], "https://www.instagram.com", Some("https://www.instagram.com/explore/search/keyword/?q={q}"), Some("https://www.instagram.com/{handle}/")),
    site!("x", "X", ["twitter", "x.com", "twitter.com", "x twitter"], "https://x.com", Some("https://x.com/search?q={q}"), Some("https://x.com/{handle}")),
    site!("threads", "Threads", ["threads.net"], "https://www.threads.net", Some("https://www.threads.net/search?q={q}"), Some("https://www.threads.net/@{handle}")),
    site!("bluesky", "Bluesky", ["bsky", "blue sky"], "https://bsky.app", Some("https://bsky.app/search?q={q}"), Some("https://bsky.app/profile/{handle}")),
    site!("facebook", "Facebook", ["fb", "facebook.com"], "https://www.facebook.com", Some("https://www.facebook.com/search/top?q={q}"), Some("https://www.facebook.com/{handle}")),
    site!("reddit", "Reddit", ["reddit.com", "redit", "subreddit"], "https://www.reddit.com", Some("https://www.reddit.com/search/?q={q}"), Some("https://www.reddit.com/r/{handle}/")),
    site!("linkedin", "LinkedIn", ["linked in"], "https://www.linkedin.com", Some("https://www.linkedin.com/search/results/all/?keywords={q}"), Some("https://www.linkedin.com/in/{handle}/")),
    site!("pinterest", "Pinterest", [], "https://www.pinterest.com", Some("https://www.pinterest.com/search/pins/?q={q}"), Some("https://www.pinterest.com/{handle}/")),
    site!("netflix", "Netflix", [], "https://www.netflix.com", Some("https://www.netflix.com/search?q={q}"), None),
    site!("primevideo", "Prime Video", ["prime video", "amazon prime video", "prime"], "https://www.primevideo.com", Some("https://www.primevideo.com/search/?phrase={q}"), None),
    site!("disneyplus", "Disney+", ["disney plus", "disney+"], "https://www.disneyplus.com", None, None),
    site!("soundcloud", "SoundCloud", ["sound cloud"], "https://soundcloud.com", Some("https://soundcloud.com/search?q={q}"), Some("https://soundcloud.com/{handle}")),
    site!("spotifyweb", "Spotify Web", ["spotify web", "spotify web player", "open spotify com"], "https://open.spotify.com", Some("https://open.spotify.com/search/{q}"), None),
    site!("whatsapp", "WhatsApp Web", ["whatsapp web", "whats app", "whatsapp"], "https://web.whatsapp.com", None, None),
    // Search, knowledge, AI
    site!("google", "Google", ["google.com", "google search"], "https://www.google.com", Some("https://www.google.com/search?q={q}"), None),
    site!("bing", "Bing", [], "https://www.bing.com", Some("https://www.bing.com/search?q={q}"), None),
    site!("duckduckgo", "DuckDuckGo", ["duck duck go", "ddg"], "https://duckduckgo.com", Some("https://duckduckgo.com/?q={q}"), None),
    site!("ecosia", "Ecosia", [], "https://www.ecosia.org", Some("https://www.ecosia.org/search?q={q}"), None),
    site!("wikipedia", "Wikipedia", ["wiki", "wikipedia.org", "wikipedia deutsch", "wikipedia english"], "https://www.wikipedia.org", Some("https://{lang}.wikipedia.org/w/index.php?search={q}"), Some("https://{lang}.wikipedia.org/wiki/{handle}")),
    site!("chatgpt", "ChatGPT", ["chat gpt", "openai", "chat gbt"], "https://chatgpt.com", Some("https://chatgpt.com/?q={q}"), None),
    site!("claude", "Claude", ["claude ai", "claude.ai"], "https://claude.ai", None, None),
    site!("gemini", "Gemini", ["google gemini"], "https://gemini.google.com", None, None),
    site!("perplexity", "Perplexity", [], "https://www.perplexity.ai", Some("https://www.perplexity.ai/search?q={q}"), None),
    site!("wolframalpha", "Wolfram Alpha", ["wolfram", "wolfram alpha"], "https://www.wolframalpha.com", Some("https://www.wolframalpha.com/input?i={q}"), None),
    site!("googlemaps", "Google Maps", ["google maps", "google karten"], "https://www.google.com/maps", Some("https://www.google.com/maps/search/{q}"), None),
    site!("translate", "Google Translate", ["google translate", "translator", "übersetzer"], "https://translate.google.com", Some("https://translate.google.com/?sl=auto&tl=en&text={q}"), None),
    site!("deepl", "DeepL", ["deep l"], "https://www.deepl.com/translator", Some("https://www.deepl.com/translator#auto/en/{q}"), None),
    site!("leo", "LEO", ["leo dictionary", "dict leo", "leo wörterbuch"], "https://dict.leo.org", Some("https://dict.leo.org/englisch-deutsch/{q}"), None),
    // Shopping (Sir is in Germany)
    site!("amazon", "Amazon", ["amazon.de", "amazon de", "amazon shop"], "https://www.amazon.de", Some("https://www.amazon.de/s?k={q}"), None),
    site!("ebay", "eBay", ["ebay.de", "e bay"], "https://www.ebay.de", Some("https://www.ebay.de/sch/i.html?_nkw={q}"), None),
    site!("kleinanzeigen", "Kleinanzeigen", ["ebay kleinanzeigen", "klein anzeigen", "kleinanzeigen.de"], "https://www.kleinanzeigen.de", Some("https://www.kleinanzeigen.de/s-{slug}/k0"), None),
    site!("idealo", "idealo", [], "https://www.idealo.de", Some("https://www.idealo.de/preisvergleich/MainSearchProductCategory.html?q={q}"), None),
    site!("geizhals", "Geizhals", [], "https://geizhals.de", Some("https://geizhals.de/?fs={q}"), None),
    site!("mediamarkt", "MediaMarkt", ["media markt"], "https://www.mediamarkt.de", Some("https://www.mediamarkt.de/de/search.html?query={q}"), None),
    site!("aliexpress", "AliExpress", ["ali express"], "https://www.aliexpress.com", Some("https://www.aliexpress.com/w/wholesale-{slug}.html"), None),
    // Gaming
    site!("steamstore", "Steam Store", ["steam store", "steam shop", "steampowered"], "https://store.steampowered.com", Some("https://store.steampowered.com/search/?term={q}"), Some("https://steamcommunity.com/id/{handle}")),
    site!("epicgames", "Epic Games Store", ["epic games", "epic store", "epic games store"], "https://store.epicgames.com", Some("https://store.epicgames.com/browse?q={q}"), None),
    site!("minecraft", "Minecraft.net", ["minecraft.net", "minecraft website"], "https://www.minecraft.net", None, None),
    site!("modrinth", "Modrinth", [], "https://modrinth.com", Some("https://modrinth.com/mods?q={q}"), Some("https://modrinth.com/user/{handle}")),
    site!("curseforge", "CurseForge", ["curse forge"], "https://www.curseforge.com", Some("https://www.curseforge.com/minecraft/search?search={q}"), Some("https://www.curseforge.com/members/{handle}")),
    site!("planetminecraft", "Planet Minecraft", ["planet minecraft"], "https://www.planetminecraft.com", Some("https://www.planetminecraft.com/search/?term={q}"), None),
    site!("namemc", "NameMC", ["name mc"], "https://namemc.com", Some("https://namemc.com/search?q={q}"), Some("https://namemc.com/profile/{handle}")),
    site!("chess", "Chess.com", ["chess.com", "chess com"], "https://www.chess.com", None, Some("https://www.chess.com/member/{handle}")),
    site!("lichess", "Lichess", [], "https://lichess.org", None, Some("https://lichess.org/@/{handle}")),
    // Developer
    site!("github", "GitHub", ["git hub", "github.com"], "https://github.com", Some("https://github.com/search?q={q}"), Some("https://github.com/{handle}")),
    site!("gitlab", "GitLab", ["git lab"], "https://gitlab.com", Some("https://gitlab.com/search?search={q}"), Some("https://gitlab.com/{handle}")),
    site!("stackoverflow", "Stack Overflow", ["stack overflow", "stackoverflow.com"], "https://stackoverflow.com", Some("https://stackoverflow.com/search?q={q}"), None),
    site!("cratesio", "crates.io", ["crates", "crates io", "crates.io"], "https://crates.io", Some("https://crates.io/search?q={q}"), Some("https://crates.io/crates/{handle}")),
    site!("docsrs", "docs.rs", ["docs rs", "docs.rs"], "https://docs.rs", Some("https://docs.rs/releases/search?query={q}"), Some("https://docs.rs/{handle}")),
    site!("rustdocs", "Rust documentation", ["rust docs", "rust documentation", "rust std", "rust doc"], "https://doc.rust-lang.org/std/", Some("https://doc.rust-lang.org/std/index.html?search={q}"), None),
    site!("pythondocs", "Python documentation", ["python docs", "python documentation"], "https://docs.python.org/3/", Some("https://docs.python.org/3/search.html?q={q}"), None),
    site!("pypi", "PyPI", ["py pi", "python package index"], "https://pypi.org", Some("https://pypi.org/search/?q={q}"), Some("https://pypi.org/project/{handle}/")),
    site!("npm", "npm", ["npmjs", "npm js"], "https://www.npmjs.com", Some("https://www.npmjs.com/search?q={q}"), Some("https://www.npmjs.com/package/{handle}")),
    site!("mdn", "MDN", ["mdn web docs", "mozilla developer"], "https://developer.mozilla.org", Some("https://developer.mozilla.org/en-US/search?q={q}"), None),
    site!("hackernews", "Hacker News", ["hacker news", "hn", "ycombinator"], "https://news.ycombinator.com", Some("https://hn.algolia.com/?q={q}"), Some("https://news.ycombinator.com/user?id={handle}")),
    // Google & Microsoft apps
    site!("gmail", "Gmail", ["google mail", "g mail"], "https://mail.google.com", Some("https://mail.google.com/mail/u/0/#search/{q}"), None),
    site!("googledrive", "Google Drive", ["google drive"], "https://drive.google.com", Some("https://drive.google.com/drive/search?q={q}"), None),
    site!("googledocs", "Google Docs", ["google docs"], "https://docs.google.com", None, None),
    site!("googlecalendar", "Google Calendar", ["google calendar", "google kalender"], "https://calendar.google.com", None, None),
    site!("outlookweb", "Outlook", ["outlook web", "outlook.com", "hotmail"], "https://outlook.live.com", None, None),
    site!("notion", "Notion", ["notion.so"], "https://www.notion.so", None, None),
    site!("figma", "Figma", [], "https://www.figma.com", None, None),
    site!("canva", "Canva", [], "https://www.canva.com", Some("https://www.canva.com/search/templates?q={q}"), None),
    // News & tools
    site!("tagesschau", "tagesschau", ["tagesschau.de"], "https://www.tagesschau.de", Some("https://www.tagesschau.de/suche#/all/1/?searchText={q}"), None),
    site!("spiegel", "Der Spiegel", ["spiegel online", "der spiegel"], "https://www.spiegel.de", Some("https://www.spiegel.de/suche/?suchbegriff={q}"), None),
    site!("heise", "heise online", ["heise online", "heise.de"], "https://www.heise.de", Some("https://www.heise.de/suche/?q={q}"), None),
    site!("bbcnews", "BBC News", ["bbc", "bbc news"], "https://www.bbc.com/news", Some("https://www.bbc.co.uk/search?q={q}"), None),
    site!("theverge", "The Verge", ["the verge", "verge"], "https://www.theverge.com", Some("https://www.theverge.com/search?q={q}"), None),
    site!("speedtest", "Speedtest", ["speed test", "internet speed test"], "https://www.speedtest.net", None, None),
    site!("imdb", "IMDb", ["i m d b"], "https://www.imdb.com", Some("https://www.imdb.com/find/?q={q}"), None),
];

fn key(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let lower = lower.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.").trim_end_matches('/');
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A known site by name, alias or domain ("YouTube", "yt", "youtube.com", "ebay kleinanzeigen").
pub fn find(name: &str) -> Option<&'static Site> {
    let k = key(name);
    if k.is_empty() {
        return None;
    }
    let compact = k.replace([' ', '-', '.'], "");
    SITES.iter().find(|s| {
        s.id == compact
            || s.display.to_lowercase() == k
            || s.aliases.contains(&k.as_str())
            || s.home.trim_start_matches("https://").trim_start_matches("www.").trim_end_matches('/') == k
    })
}

/// The handle as the platform spells it in URLs: no spaces, no leading `@`, `r/`, `u/`.
fn clean_handle(site: &str, raw: &str) -> String {
    let mut h = raw.trim().trim_matches(|c: char| c == '"' || c == '\'').to_string();
    for prefix in ["https://", "http://", "@", "r/", "/r/", "u/", "/u/", "user/"] {
        if let Some(rest) = h.strip_prefix(prefix) {
            h = rest.to_string();
        }
    }
    let h: String = match site {
        // "torvalds/linux" stays a repository path.
        "github" | "gitlab" => h.split_whitespace().collect::<String>(),
        "wikipedia" => return urlencoding::encode(&h.split_whitespace().collect::<Vec<_>>().join("_")).into_owned(),
        _ => h.chars().filter(|c| !c.is_whitespace()).collect(),
    };
    let h = h.trim_matches('/');
    match site {
        "twitch" | "kick" | "reddit" => h.to_lowercase(),
        _ => h.to_string(),
    }
}

/// How `open_website` was asked to navigate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    DirectUrl,
    ChannelOrProfile,
    SiteSearch,
}

impl Mode {
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
            "direct_url" | "direct" | "url" | "open" | "home" | "website" => Self::DirectUrl,
            "channel_or_profile" | "channel" | "profile" | "creator" | "user" | "account" | "page" | "subreddit" | "repo" => {
                Self::ChannelOrProfile
            }
            "site_search" | "search" | "query" | "find" => Self::SiteSearch,
            _ => return None,
        })
    }
}

/// The URL for a request; `None` when the site cannot do it.
pub fn resolve(site: &Site, mode: Mode, target: Option<&str>, lang: &str) -> Option<String> {
    match (mode, target.filter(|t| !t.trim().is_empty())) {
        (Mode::ChannelOrProfile, Some(t)) => site.profile_url(t).map(|u| u.replace("{lang}", lang)),
        (Mode::SiteSearch, Some(q)) => site.search_url(q, lang),
        _ => Some(site.home_url()),
    }
}

/// YouTube channel URL for a handle or display name: the handle if `/@handle` exists, otherwise
/// the first channel YouTube's own channel search returns, otherwise the plain handle URL.
async fn youtube_channel(name: &str) -> String {
    let compact: String = clean_handle("youtube", name);
    let fallback = format!("https://www.youtube.com/@{compact}");
    let looks_like_handle = !name.trim().contains(' ') && compact.chars().all(|c| c.is_alphanumeric() || "._-".contains(c));
    let get = |url: String| {
        crate::websearch::HTTP
            .get(url)
            // Skips the EU cookie-consent interstitial.
            .header(reqwest::header::COOKIE, "SOCS=CAI; CONSENT=YES+cb")
            .timeout(Duration::from_millis(3500))
            .send()
    };
    if looks_like_handle {
        match get(fallback.clone()).await {
            Ok(r) if r.status().as_u16() != 404 => return fallback,
            Ok(_) => debug!("[WEB] YouTube has no @{compact}; searching channels"),
            Err(e) => {
                debug!("[WEB] YouTube handle check failed ({e}); using @{compact}");
                return fallback;
            }
        }
    }
    // sp=EgIQAg%3D%3D: channels only.
    let search = format!("https://www.youtube.com/results?search_query={}&sp=EgIQAg%253D%253D", urlencoding::encode(name.trim()));
    static CHANNEL: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#""canonicalBaseUrl":"(/@[^"\\]+|/channel/[A-Za-z0-9_-]+)""#).expect("valid channel regex"));
    if let Ok(r) = get(search).await {
        if let Ok(body) = r.text().await {
            if let Some(c) = CHANNEL.captures(&body) {
                let url = format!("https://www.youtube.com{}", &c[1]);
                info!("[WEB] YouTube channel for '{name}' → {url}");
                return url;
            }
        }
    }
    fallback
}

/// What `open_website` opened.
pub struct Opened {
    pub url: String,
    pub summary: String,
}

/// The `open_website` tool.
pub async fn open(args: &Value) -> Result<Opened, String> {
    let platform = str_arg(args, &["platform_or_url", "platform", "site", "url", "website"]).ok_or("open_website needs 'platform_or_url'.")?;
    let target = str_arg(args, &["target_or_query", "target", "query", "handle", "channel", "search"]);
    let lang = str_arg(args, &["lang", "language"]).map(|l| if l.to_lowercase().starts_with("de") { "de" } else { "en" }).unwrap_or("en");
    let mode = str_arg(args, &["action_type", "mode", "action"]).and_then(Mode::parse).unwrap_or(if target.is_some() { Mode::SiteSearch } else { Mode::DirectUrl });

    let (url, what) = match find(platform) {
        Some(site) => {
            let url = match (mode, site.id, target) {
                (Mode::ChannelOrProfile, "youtube", Some(t)) => Some(youtube_channel(t).await),
                _ => resolve(site, mode, target, lang),
            };
            let url = match url {
                Some(u) => u,
                None if mode == Mode::ChannelOrProfile => {
                    return Err(format!("{} has no channel or profile pages I know; try a search instead.", site.display));
                }
                None => {
                    // No search page: search the web for it on that site.
                    let q = format!("site:{} {}", site.home.trim_start_matches("https://").trim_start_matches("www."), target.unwrap_or(""));
                    format!("https://www.google.com/search?q={}", urlencoding::encode(q.trim()))
                }
            };
            let what = match (mode, target) {
                (Mode::ChannelOrProfile, Some(t)) => format!("{t} on {}", site.display),
                (Mode::SiteSearch, Some(q)) => format!("a {} search for \"{q}\"", site.display),
                _ => site.display.to_string(),
            };
            (url, what)
        }
        None => match crate::apps::as_url(platform) {
            Some(url) => (url.clone(), url),
            None if mode == Mode::SiteSearch || target.is_some() => {
                let q = format!("{platform} {}", target.unwrap_or("")).trim().to_string();
                (format!("https://www.google.com/search?q={}", urlencoding::encode(&q)), format!("a web search for \"{q}\""))
            }
            None => {
                // "open bastighg" with no platform: guess the .com, which is right for most brands.
                let guess: String = platform.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == '-').collect();
                if guess.is_empty() {
                    return Err(format!("I don't know a website called '{platform}'."));
                }
                (format!("https://www.{guess}.com"), format!("{guess}.com"))
            }
        },
    };
    let via = crate::apps::open_default_browser(&url).await?;
    info!("[WEB] Opened {what}: {url}");
    Ok(Opened { summary: format!("Opened {what} in the default browser ({via}): {url}"), url })
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "open_website",
        "description": "Open a website in Sir's default browser. Modes: direct_url (\"open YouTube\", \"open github.com\"), channel_or_profile (a creator's channel/profile: \"open BastiGHG on YouTube\" → youtube.com/@BastiGHG, Twitch, GitHub user or owner/repo, subreddit, TikTok, Instagram, X …), site_search (\"open YouTube and search for Minecraft\", \"RTX 3060 Ti on Kleinanzeigen\", Google, Amazon, Wikipedia, Reddit, Twitch, GitHub …). Use it instead of open_application for websites.",
        "parameters": {"type": "object", "properties": {
            "platform_or_url": {"type": "string", "description": "Site name (youtube, twitch, github, reddit, google, amazon, kleinanzeigen, wikipedia …) or a domain/URL"},
            "action_type": {"type": "string", "enum": ["direct_url", "channel_or_profile", "site_search"]},
            "target_or_query": {"type": "string", "description": "Channel/profile handle, or the search query"},
            "lang": {"type": "string", "description": "de or en (Wikipedia)"}
        }, "required": ["platform_or_url"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_dozens_of_sites_by_name_alias_and_domain() {
        assert!(SITES.len() >= 40, "{} sites", SITES.len());
        for (name, id) in [("YouTube", "youtube"), ("yt", "youtube"), ("youtube.com", "youtube"), ("Twitch", "twitch"), ("eBay Kleinanzeigen", "kleinanzeigen"), ("GitHub", "github"), ("twitter", "x"), ("Wikipedia", "wikipedia"), ("crates.io", "cratesio")] {
            assert_eq!(find(name).map(|s| s.id), Some(id), "{name}");
        }
        assert!(find("blender").is_none());
        let mut ids: Vec<&str> = SITES.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), SITES.len(), "site ids are unique");
    }

    #[test]
    fn channels_and_profiles() {
        let url = |site: &str, handle: &str| resolve(find(site).unwrap(), Mode::ChannelOrProfile, Some(handle), "en").unwrap();
        assert_eq!(url("youtube", "BastiGHG"), "https://www.youtube.com/@BastiGHG");
        assert_eq!(url("youtube", "@Basti GHG"), "https://www.youtube.com/@BastiGHG", "spaces are stripped");
        assert_eq!(url("twitch", "BastiGHG"), "https://www.twitch.tv/bastighg");
        assert_eq!(url("github", "torvalds"), "https://github.com/torvalds");
        assert_eq!(url("github", "torvalds/linux"), "https://github.com/torvalds/linux");
        assert_eq!(url("reddit", "r/Rust"), "https://www.reddit.com/r/rust/");
        assert_eq!(url("reddit", "u/spez"), "https://www.reddit.com/user/spez/");
        assert_eq!(url("tiktok", "khaby.lame"), "https://www.tiktok.com/@khaby.lame");
    }

    #[test]
    fn site_searches_are_url_encoded() {
        let url = |site: &str, q: &str, lang: &str| resolve(find(site).unwrap(), Mode::SiteSearch, Some(q), lang).unwrap();
        assert_eq!(url("youtube", "Minecraft", "en"), "https://www.youtube.com/results?search_query=Minecraft");
        assert_eq!(url("google", "rust & tokio", "en"), "https://www.google.com/search?q=rust%20%26%20tokio");
        assert_eq!(url("github", "egui viewport", "en"), "https://github.com/search?q=egui%20viewport");
        assert_eq!(url("reddit", "rtx 3060", "en"), "https://www.reddit.com/search/?q=rtx%203060");
        assert_eq!(url("twitch", "minecraft", "en"), "https://www.twitch.tv/search?term=minecraft");
        assert_eq!(url("amazon", "USB-C Kabel", "en"), "https://www.amazon.de/s?k=USB-C%20Kabel");
        assert_eq!(url("kleinanzeigen", "RTX 3060 Ti", "de"), "https://www.kleinanzeigen.de/s-rtx-3060-ti/k0");
        assert_eq!(url("wikipedia", "Albert Einstein", "de"), "https://de.wikipedia.org/w/index.php?search=Albert%20Einstein");
        assert_eq!(resolve(find("youtube").unwrap(), Mode::DirectUrl, None, "en").as_deref(), Some("https://www.youtube.com"));
        assert_eq!(Mode::parse("channel"), Some(Mode::ChannelOrProfile));
        assert_eq!(Mode::parse("site search"), Some(Mode::SiteSearch));
    }
}
