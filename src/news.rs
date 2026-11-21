//! "Give me an overview of the most important news from Germany", "die wichtigsten Nachrichten
//! aus Österreich": a newsroom of that country, headline for headline as it wrote them (never
//! reworded by the model), with the newsroom named. Asked in German, a German-language newsroom
//! answers where the country has one (tagesschau's front page, ORF), else an English one: DW for
//! Germany, the BBC for the UK, NPR for the US, the Guardian's page for every other country.
//! Without a country, his own (by his dialling code). Nothing older than three days is told.

use crate::tools::ToolContext;
use chrono::{DateTime, FixedOffset, Local};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

pub const TOOL: &str = "country_news";
/// What is older is no news.
const FRESH_HOURS: i64 = 72;
/// Headlines on the card, and said aloud.
const SHOWN: usize = 8;
const SAID: usize = 3;

#[derive(Clone, Copy)]
enum Feed {
    /// tagesschau's front page, in the newsroom's own order.
    Tagesschau,
    Rss(&'static str),
    /// The Guardian's page for a country (its path).
    Guardian(&'static str),
}

#[derive(Clone, Copy)]
struct Source {
    outlet: &'static str,
    feed: Feed,
    /// The sections that are the country's own news: told before the rest.
    home: &'static [&'static str],
}

const fn guardian(path: &'static str) -> Source {
    Source { outlet: "the Guardian", feed: Feed::Guardian(path), home: &[] }
}
const TAGESSCHAU: Source = Source { outlet: "tagesschau", feed: Feed::Tagesschau, home: &["inland", "wirtschaft", "investigativ"] };
const ORF: Source = Source { outlet: "ORF", feed: Feed::Rss("https://rss.orf.at/news.xml"), home: &["Inland", "Wirtschaft"] };
const DW: Source = Source { outlet: "DW", feed: Feed::Rss("https://rss.dw.com/rdf/rss-en-ger"), home: &[] };
const BBC: Source = Source { outlet: "the BBC", feed: Feed::Rss("https://feeds.bbci.co.uk/news/uk/rss.xml"), home: &[] };
const NPR: Source = Source { outlet: "NPR", feed: Feed::Rss("https://feeds.npr.org/1003/rss.xml"), home: &[] };

struct Country {
    /// In English, as said ("the United States").
    name: &'static str,
    /// In German, after "aus" ("den USA").
    aus: &'static str,
    dial: &'static str,
    /// How he names it, in English and German: the country, and its adjective (German ones
    /// without their ending: "deutsch" for "deutsche", "deutschen").
    said: &'static [&'static str],
    english: Source,
    german: Option<Source>,
}

const fn land(name: &'static str, aus: &'static str, dial: &'static str, said: &'static [&'static str], english: Source) -> Country {
    Country { name, aus, dial, said, english, german: None }
}

static COUNTRIES: &[Country] = &[
    Country { name: "Germany", aus: "Deutschland", dial: "+49", said: &["germany", "german", "deutschland", "deutsch", "bundesrepublik"], english: DW, german: Some(TAGESSCHAU) },
    Country { name: "Austria", aus: "Österreich", dial: "+43", said: &["austria", "austrian", "österreich", "oesterreich", "österreichisch"], english: guardian("world/austria"), german: Some(ORF) },
    land("Switzerland", "der Schweiz", "+41", &["switzerland", "swiss", "schweiz", "schweizer", "schweizerisch"], guardian("world/switzerland")),
    land("the United Kingdom", "Großbritannien", "+44", &["uk", "u k", "united kingdom", "britain", "great britain", "england", "british", "großbritannien", "grossbritannien", "vereinigten königreich", "vereinigtes königreich", "britisch"], BBC),
    land("the United States", "den USA", "+1", &["us", "u s", "usa", "u s a", "united states", "united states of america", "america", "american", "amerika", "vereinigten staaten", "vereinigte staaten", "amerikanisch", "us amerikanisch"], NPR),
    land("France", "Frankreich", "+33", &["france", "french", "frankreich", "französisch", "franzoesisch"], guardian("world/france")),
    land("Italy", "Italien", "+39", &["italy", "italian", "italien", "italienisch"], guardian("world/italy")),
    land("Spain", "Spanien", "+34", &["spain", "spanish", "spanien", "spanisch"], guardian("world/spain")),
    land("Portugal", "Portugal", "+351", &["portugal", "portuguese", "portugiesisch"], guardian("world/portugal")),
    land("the Netherlands", "den Niederlanden", "+31", &["netherlands", "holland", "dutch", "niederlande", "niederlanden", "niederländisch", "holländisch"], guardian("world/netherlands")),
    land("Belgium", "Belgien", "+32", &["belgium", "belgian", "belgien", "belgisch"], guardian("world/belgium")),
    land("Ireland", "Irland", "+353", &["ireland", "irish", "irland", "irisch"], guardian("world/ireland")),
    land("Denmark", "Dänemark", "+45", &["denmark", "danish", "dänemark", "daenemark", "dänisch"], guardian("world/denmark")),
    land("Norway", "Norwegen", "+47", &["norway", "norwegian", "norwegen", "norwegisch"], guardian("world/norway")),
    land("Sweden", "Schweden", "+46", &["sweden", "swedish", "schweden", "schwedisch"], guardian("world/sweden")),
    land("Finland", "Finnland", "+358", &["finland", "finnish", "finnland", "finnisch"], guardian("world/finland")),
    land("Poland", "Polen", "+48", &["poland", "polish", "polen", "polnisch"], guardian("world/poland")),
    land("the Czech Republic", "Tschechien", "+420", &["czech republic", "czechia", "czech", "tschechien", "tschechisch"], guardian("world/czech-republic")),
    land("Hungary", "Ungarn", "+36", &["hungary", "hungarian", "ungarn", "ungarisch"], guardian("world/hungary")),
    land("Romania", "Rumänien", "+40", &["romania", "romanian", "rumänien", "rumaenien", "rumänisch"], guardian("world/romania")),
    land("Croatia", "Kroatien", "+385", &["croatia", "croatian", "kroatien", "kroatisch"], guardian("world/croatia")),
    land("Serbia", "Serbien", "+381", &["serbia", "serbian", "serbien", "serbisch"], guardian("world/serbia")),
    land("Greece", "Griechenland", "+30", &["greece", "greek", "griechenland", "griechisch"], guardian("world/greece")),
    land("Turkey", "der Türkei", "+90", &["turkey", "turkish", "türkei", "tuerkei", "türkisch", "tuerkisch"], guardian("world/turkey")),
    land("Russia", "Russland", "+7", &["russia", "russian", "russland", "russisch"], guardian("world/russia")),
    land("Ukraine", "der Ukraine", "+380", &["ukraine", "ukrainian", "ukrainisch"], guardian("world/ukraine")),
    land("Israel", "Israel", "+972", &["israel", "israeli", "israelisch"], guardian("world/israel")),
    land("Iran", "dem Iran", "+98", &["iran", "iranian", "iranisch"], guardian("world/iran")),
    land("Iraq", "dem Irak", "+964", &["iraq", "iraqi", "irak", "irakisch"], guardian("world/iraq")),
    land("Syria", "Syrien", "+963", &["syria", "syrian", "syrien", "syrisch"], guardian("world/syria")),
    land("Lebanon", "dem Libanon", "+961", &["lebanon", "lebanese", "libanon", "libanesisch"], guardian("world/lebanon")),
    land("Saudi Arabia", "Saudi-Arabien", "+966", &["saudi arabia", "saudi", "saudi arabien", "saudisch"], guardian("world/saudiarabia")),
    land("Egypt", "Ägypten", "+20", &["egypt", "egyptian", "ägypten", "aegypten", "ägyptisch"], guardian("world/egypt")),
    land("Morocco", "Marokko", "+212", &["morocco", "moroccan", "marokko", "marokkanisch"], guardian("world/morocco")),
    land("Nigeria", "Nigeria", "+234", &["nigeria", "nigerian", "nigerianisch"], guardian("world/nigeria")),
    land("Kenya", "Kenia", "+254", &["kenya", "kenyan", "kenia", "kenianisch"], guardian("world/kenya")),
    land("South Africa", "Südafrika", "+27", &["south africa", "south african", "südafrika", "suedafrika", "südafrikanisch"], guardian("world/southafrica")),
    land("Afghanistan", "Afghanistan", "+93", &["afghanistan", "afghan", "afghanisch"], guardian("world/afghanistan")),
    land("Pakistan", "Pakistan", "+92", &["pakistan", "pakistani", "pakistanisch"], guardian("world/pakistan")),
    land("India", "Indien", "+91", &["india", "indian", "indien", "indisch"], guardian("world/india")),
    land("China", "China", "+86", &["china", "chinese", "chinesisch"], guardian("world/china")),
    land("Taiwan", "Taiwan", "+886", &["taiwan", "taiwanese", "taiwanisch"], guardian("world/taiwan")),
    land("Japan", "Japan", "+81", &["japan", "japanese", "japanisch"], guardian("world/japan")),
    land("South Korea", "Südkorea", "+82", &["south korea", "korea", "korean", "south korean", "südkorea", "suedkorea", "koreanisch", "südkoreanisch"], guardian("world/south-korea")),
    land("North Korea", "Nordkorea", "+850", &["north korea", "north korean", "nordkorea", "nordkoreanisch"], guardian("world/north-korea")),
    land("Thailand", "Thailand", "+66", &["thailand", "thai", "thailändisch"], guardian("world/thailand")),
    land("Vietnam", "Vietnam", "+84", &["vietnam", "vietnamese", "vietnamesisch"], guardian("world/vietnam")),
    land("Indonesia", "Indonesien", "+62", &["indonesia", "indonesian", "indonesien", "indonesisch"], guardian("world/indonesia")),
    land("the Philippines", "den Philippinen", "+63", &["philippines", "filipino", "philippinen", "philippinisch"], guardian("world/philippines")),
    land("Canada", "Kanada", "+1", &["canada", "canadian", "kanada", "kanadisch"], guardian("world/canada")),
    land("Mexico", "Mexiko", "+52", &["mexico", "mexican", "mexiko", "mexikanisch"], guardian("world/mexico")),
    land("Brazil", "Brasilien", "+55", &["brazil", "brazilian", "brasilien", "brasilianisch"], guardian("world/brazil")),
    land("Argentina", "Argentinien", "+54", &["argentina", "argentinian", "argentine", "argentinien", "argentinisch"], guardian("world/argentina")),
    land("Chile", "Chile", "+56", &["chile", "chilean", "chilenisch"], guardian("world/chile")),
    land("Colombia", "Kolumbien", "+57", &["colombia", "colombian", "kolumbien", "kolumbianisch"], guardian("world/colombia")),
    land("Venezuela", "Venezuela", "+58", &["venezuela", "venezuelan", "venezolanisch"], guardian("world/venezuela")),
    land("Australia", "Australien", "+61", &["australia", "australian", "australien", "australisch"], guardian("australia-news")),
];

/// The country he named: "the uk", "der Schweiz", "deutschen".
fn country(place: &str) -> Option<&'static Country> {
    static ARTICLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:the|der|die|den|dem|des)\s+").expect("article pattern"));
    let place = ARTICLE.replace(place.trim(), "");
    let named = |p: &str| COUNTRIES.iter().find(|c| c.said.contains(&p));
    named(&place).or_else(|| named(place.strip_suffix("en").or_else(|| place.strip_suffix("er")).or_else(|| place.strip_suffix('e'))?))
}

fn by_name(name: &str) -> Option<&'static Country> {
    COUNTRIES.iter().find(|c| c.name == name)
}

/// His own country, by the dialling code he set (Germany when it is none of these).
fn home(dial: &str) -> &'static Country {
    COUNTRIES.iter().find(|c| c.dial == dial.trim()).unwrap_or(&COUNTRIES[0])
}

static ASKED: LazyLock<Regex> = LazyLock::new(|| {
    let ask = r"(?:(?:can|could|would|will) you (?:please )?|please |(?:kannst|könntest|koenntest) du (?:mir )?(?:bitte )?|bitte )?(?:give me|get me|tell me|read me|read|show me|bring me|brief me on|catch me up on|update me on|check|what are|what(?:'s| s|s| is)|any|gib mir|zeig mir|lies mir|lies|sag mir|erzähl mir|erzähle mir|was sind|was gibt(?: es|s| s) für|wie lauten|ich (?:will|möchte|moechte|hätte gerne?))?";
    let article = r"(?:(?:a|an|the|einen|eine|nen|den|die|das) )?";
    let overview = r"(?:(?:quick|short|brief|little|kurzen|kurze|kleinen|kleine|schnellen) )?(?:(?:overview|summary|rundown|round ?up|update|digest|überblick|ueberblick|zusammenfassung|übersicht|uebersicht) (?:of|on|about|über|ueber|zu|von|der|den) )?";
    let qualifier = r"(?:(?:most important|important|main|top|latest|biggest|big|current|today(?:'s| s|s)|wichtigsten|wichtigen|aktuellen|neuesten|heutigen|größten|groessten) )*";
    let news = r"(?:news headlines|news|headlines|top stories|nachrichtenüberblick|nachrichten|schlagzeilen|meldungen)";
    let when = r"(?: (?:today|right now|of the day|please|heute|von heute|des tages|bitte|gerade|jetzt|vor))*";
    let place = r"(?: (?:from|in|for|of|about|out of|on|aus|von|für|fuer|über|ueber) (?P<place>.+?))?";
    let elsewhere = r"what(?:'s| s|s| is) (?:happening|going on|new) in (?P<there>.+?)|was (?:ist|passiert|geht) (?:gerade |heute |so )*in (?P<dort>.+?) (?:los|vor)|was gibt(?: es|s| s) (?:neues|neuigkeiten) (?:aus|in) (?P<drueben>.+?)";
    Regex::new(&format!(r"^(?:{ask}\s*{article}{overview}{article}{qualifier}(?:(?P<adj>[a-zäöüß]+) )?{news}{when}{place}|{elsewhere}|what(?:'s| s|s| is) in the news){when}$")).expect("news pattern")
});
static GERMAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:nachrichten\w*|schlagzeilen|meldungen|neuigkeiten|neues|los|gib|zeig|lies|sag|erzähl|erzähle|überblick|ueberblick|übersicht|zusammenfassung|wichtigsten|aktuellen|neuesten|heutigen|aus|heute|bitte|was)\b").expect("german pattern")
});

/// "Give me an overview of the most important news from Germany", "Nachrichten aus Frankreich",
/// "what's happening in Japan", "the news" (his own country): the country (none: his own) and
/// the language he asked in. A place that is no country is not this order's ("news about the
/// Bundesliga", "news from Max").
pub fn command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim().to_lowercase();
    let c = ASKED.captures(&s)?;
    let language = if GERMAN.is_match(&s) { "de" } else { "en" };
    match ["place", "there", "dort", "drueben", "adj"].iter().find_map(|g| c.name(g)) {
        Some(place) => Some(json!({"country": country(place.as_str())?.name, "language": language})),
        None => Some(json!({"language": language})),
    }
}

#[derive(Default)]
struct Story {
    title: String,
    /// tagesschau's line over the title ("Krieg gegen die Ukraine").
    topline: String,
    section: String,
    at: Option<DateTime<FixedOffset>>,
}

/// tagesschau's front page (its app's JSON), without the weather.
fn tagesschau(json: &str) -> Result<Vec<Story>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("tagesschau's page didn't read: {e}"))?;
    let text = |n: &Value, key: &str| n[key].as_str().unwrap_or("").trim().to_string();
    Ok(v["news"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| !n["shareURL"].as_str().unwrap_or("").contains("/wetter/"))
        .map(|n| Story { title: text(n, "title"), topline: text(n, "topline"), section: text(n, "ressort"), at: DateTime::parse_from_rfc3339(&text(n, "date")).ok() })
        .filter(|s| !s.title.is_empty())
        .collect())
}

/// An RSS or RDF feed's items: title, section and time.
fn rss(xml: &str) -> Vec<Story> {
    static ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<item\b.*?</item>").expect("item pattern"));
    static FIELD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<(title|dc:subject|category|pubDate|dc:date)(?:\s[^>]*)?>(.*?)</(?:title|dc:subject|category|pubDate|dc:date)>").expect("field pattern")
    });
    ITEM.find_iter(xml)
        .filter_map(|item| {
            let mut story = Story::default();
            for c in FIELD.captures_iter(item.as_str()) {
                let value = plain(&c[2]);
                match &c[1] {
                    "title" if story.title.is_empty() => story.title = value,
                    "dc:subject" | "category" if story.section.is_empty() => story.section = value,
                    "pubDate" | "dc:date" if story.at.is_none() => story.at = DateTime::parse_from_rfc2822(&value).or_else(|_| DateTime::parse_from_rfc3339(&value)).ok(),
                    _ => {}
                }
            }
            (!story.title.is_empty()).then_some(story)
        })
        .collect()
}

/// A feed's text as it reads: out of its CDATA, its entities decoded, on one line.
fn plain(raw: &str) -> String {
    static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#(x[0-9a-fA-F]+|[0-9]+);").expect("entity pattern"));
    let raw = raw.trim();
    let raw = raw.strip_prefix("<![CDATA[").and_then(|r| r.strip_suffix("]]>")).unwrap_or(raw);
    let raw = NUMBERED.replace_all(raw, |c: &regex::Captures| {
        let n = &c[1];
        n.strip_prefix('x').map_or_else(|| n.parse().ok(), |hex| u32::from_str_radix(hex, 16).ok()).and_then(char::from_u32).map(String::from).unwrap_or_default()
    });
    crate::whatsapp_client::unescape_xml(&raw).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The stories worth telling: fresh, the country's own first (in the newsroom's order), eight.
fn chosen(mut stories: Vec<Story>, source: &Source, now: DateTime<Local>) -> Vec<Story> {
    stories.retain(|s| s.at.is_none_or(|at| (now - at.with_timezone(&Local)).num_hours() < FRESH_HOURS));
    stories.sort_by_key(|s| !source.home.is_empty() && !source.home.iter().any(|h| h.eq_ignore_ascii_case(&s.section)));
    stories.truncate(SHOWN);
    stories
}

/// The card's title and text, and what is said: the headlines word for word, the newsroom named.
fn report(country: &Country, source: &Source, german: bool, stories: &[Story], now: DateTime<Local>) -> (String, String, String) {
    let outlet = source.outlet;
    let title = format!("NEWS // {} · {}", country.name.trim_start_matches("the ").to_uppercase(), outlet.trim_start_matches("the ").to_uppercase());
    let mut card: Vec<String> = stories
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let when = s.at.map(|at| at.with_timezone(&Local)).map(|at| if at.date_naive() == now.date_naive() { at.format("%H:%M").to_string() } else { at.format("%-d %b").to_string() });
            let kicker = if s.topline.is_empty() { String::new() } else { format!("{}: ", s.topline) };
            format!("{:02}  {}{kicker}{}", i + 1, when.map(|w| format!("{w}  ")).unwrap_or_default(), s.title)
        })
        .collect();
    card.push(format!("Source: {outlet} · read at {}", now.format("%H:%M")));
    let said: Vec<String> = stories.iter().take(SAID).map(|s| if s.title.ends_with(['.', '!', '?']) { s.title.clone() } else { format!("{}.", s.title) }).collect();
    let said = said.join(" ");
    let spoken = match (german, stories.is_empty()) {
        (true, true) => format!("{} hat nichts aus den letzten drei Tagen, Sir.", outlet.trim_start_matches("the ")),
        (false, true) => format!("{} has nothing from {} in the last three days, Sir.", capital(outlet), country.name),
        (true, false) if country.german.is_some() => format!("Die Schlagzeilen aus {}, laut {outlet}, Sir: {said}", country.aus),
        (true, false) => format!("Aus {} habe ich nur englische Schlagzeilen, Sir. Laut {}: {said}", country.aus, outlet.trim_start_matches("the ")),
        (false, false) => format!("Here's what {outlet} reports from {}, Sir: {said}", country.name),
    };
    (title, card.join("\n\n"), spoken)
}

fn capital(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

async fn fetch(source: &Source) -> Result<Vec<Story>, String> {
    let url = match source.feed {
        Feed::Tagesschau => "https://www.tagesschau.de/api2u/homepage/".to_string(),
        Feed::Rss(url) => url.to_string(),
        Feed::Guardian(path) => format!("https://www.theguardian.com/{path}/rss"),
    };
    let body = crate::websearch::HTTP.get(&url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
    match source.feed {
        Feed::Tagesschau => tagesschau(&body),
        _ => Ok(rss(&body)),
    }
}

/// Fetches the country's headlines, opens them on a card and says the first three.
pub async fn run(ctx: &ToolContext, args: &Value) -> Result<String, String> {
    let country = match args["country"].as_str() {
        Some(name) => by_name(name).ok_or_else(|| format!("I have no newsroom for {name}, Sir."))?,
        None => home(&ctx.config.read(|c| c.whatsapp.default_country_code.clone())),
    };
    let german = args["language"].as_str() == Some("de");
    let source = if german { country.german.unwrap_or(country.english) } else { country.english };
    let stories = fetch(&source).await.map_err(|e| {
        tracing::warn!("[NEWS] {}: {e}", source.outlet);
        if german { format!("Ich erreiche {} gerade nicht, Sir.", source.outlet.trim_start_matches("the ")) } else { format!("I can't reach {} just now, Sir.", source.outlet) }
    })?;
    let now = Local::now();
    let stories = chosen(stories, &source, now);
    let (title, card, spoken) = report(country, &source, german, &stories, now);
    if !stories.is_empty() {
        crate::kit::show_text("news", &title, &card, [580.0, 440.0]);
    }
    Ok(crate::kit::say(card, spoken))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn news_is_asked_for_in_his_words() {
        let country = |s: &str| command(s).map(|a| (a["country"].as_str().unwrap_or("home").to_string(), a["language"].as_str().unwrap_or("").to_string()));
        for (said, place, language) in [
            ("give me an overview of the most important news from germany", "Germany", "en"),
            ("Give me an overview of the most important news from Germany.", "Germany", "en"),
            ("what are the most important news in germany right now?", "Germany", "en"),
            ("can you give me a quick rundown of the latest headlines from the uk", "the United Kingdom", "en"),
            ("what's the news in the us today", "the United States", "en"),
            ("whats happening in japan", "Japan", "en"),
            ("news from france", "France", "en"),
            ("any news from south korea", "South Korea", "en"),
            ("the latest german news", "Germany", "en"),
            ("top stories in ukraine", "Ukraine", "en"),
            ("gib mir einen überblick über die wichtigsten nachrichten aus deutschland", "Germany", "de"),
            ("die wichtigsten Nachrichten aus Deutschland", "Germany", "de"),
            ("was sind die aktuellen schlagzeilen aus der schweiz", "Switzerland", "de"),
            ("nachrichten aus den usa", "the United States", "de"),
            ("deutsche nachrichten", "Germany", "de"),
            ("die neuesten österreichischen nachrichten", "Austria", "de"),
            ("was ist gerade in frankreich los", "France", "de"),
            ("was gibt es neues aus italien", "Italy", "de"),
            ("what's the news", "home", "en"),
            ("give me the latest headlines", "home", "en"),
            ("what's in the news today", "home", "en"),
            ("news", "home", "en"),
            ("was sind die wichtigsten nachrichten heute", "home", "de"),
            ("die nachrichten bitte", "home", "de"),
        ] {
            assert_eq!(country(said), Some((place.to_string(), language.to_string())), "{said}");
        }
        for not in [
            "any news for gta",
            "news about the bundesliga",
            "news from max",
            "neue nachrichten",
            "meine nachrichten",
            "nachrichten von max",
            "leave the news out of my briefing",
            "what's new",
            "any good news",
            "send a message to max",
            "turn on the news",
            "news in english",
        ] {
            assert_eq!(command(not), None, "{not}");
        }
        assert_eq!(home("+44").name, "the United Kingdom");
        assert_eq!(home(" +49 ").name, "Germany");
        assert_eq!(home("+999").name, "Germany", "an unknown code: Germany");
        assert_eq!(home("+1").name, "the United States", "the US before Canada");
        assert!(COUNTRIES.iter().all(|c| by_name(c.name).is_some_and(|b| std::ptr::eq(b, c))), "every name is one country's");
    }

    #[test]
    fn the_news_order_lands_on_the_newsroom() {
        for (said, country, language) in [
            ("Give me an overview of the most important news from Germany.", "Germany", "en"),
            ("Gib mir einen Überblick über die wichtigsten Nachrichten aus Deutschland.", "Germany", "de"),
            ("What's happening in France?", "France", "en"),
            ("What's happening in Germany right now?", "Germany", "en"),
        ] {
            assert_eq!(crate::phrasings::route(said), Some((TOOL.to_string(), json!({"country": country, "language": language}))), "{said}");
        }
        assert_eq!(crate::phrasings::route("What's the news?"), Some((TOOL.to_string(), json!({"language": "en"}))));
        assert!(crate::kit::only_looks(TOOL, &json!({"language": "en"})), "a question may ask for the news");
    }

    fn at(s: &str) -> DateTime<Local> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Local)
    }

    #[test]
    fn a_newsroom_is_read_word_for_word() {
        let now = at("2026-11-21T15:00:00+01:00");
        // (The card tells the time where the PC is.)
        let clock = |s: &str| at(s).format("%H:%M").to_string();
        // An RDF feed (DW, ORF) and an RSS one with CDATA (the BBC), made up for the test.
        let rdf = r#"<rdf:RDF><channel><title>Channel title</title></channel>
            <item rdf:about="https://example.org/1"><title>Musterstadt&apos;s council votes &amp; agrees</title><link>https://example.org/1</link><dc:subject>Ausland</dc:subject><dc:date>2026-11-21T10:00:00+01:00</dc:date></item>
            <item rdf:about="https://example.org/2"><title>Second headline &#8211; with a dash</title><dc:subject>Inland</dc:subject><dc:date>2026-11-21T09:00:00+01:00</dc:date></item>
            <item rdf:about="https://example.org/3"><title>Old news</title><dc:subject>Inland</dc:subject><dc:date>2026-11-14T09:00:00+01:00</dc:date></item>
        </rdf:RDF>"#;
        let stories = chosen(rss(rdf), &ORF, now);
        let titles: Vec<&str> = stories.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Second headline – with a dash", "Musterstadt's council votes & agrees"], "the country's own first, the stale left out");
        let bbc = r#"<rss><channel><item>
            <title><![CDATA[Minister says "yes" to the plan]]></title><category>UK</category>
            <pubDate>Sat, 21 Nov 2026 13:02:05 GMT</pubDate></item><item><title>No date at all</title></item></channel></rss>"#;
        let stories = chosen(rss(bbc), &BBC, now);
        assert_eq!(stories.len(), 2);
        assert_eq!(stories[0].title, r#"Minister says "yes" to the plan"#);
        assert!(stories[0].at.is_some() && stories[1].at.is_none());
        let (title, card, spoken) = report(by_name("the United Kingdom").unwrap(), &BBC, false, &stories, now);
        assert_eq!(title, "NEWS // UNITED KINGDOM · BBC");
        assert!(card.starts_with(&format!("01  {}  Minister says \"yes\" to the plan\n\n02  No date at all", clock("2026-11-21T13:02:05Z"))), "{card}");
        assert_eq!(spoken, "Here's what the BBC reports from the United Kingdom, Sir: Minister says \"yes\" to the plan. No date at all.");

        // tagesschau's front page: the newsroom's order, Germany's own news first, no weather.
        let page = r#"{"news":[
            {"title":"Erdbeben in Musterland","topline":"Ausland","ressort":"ausland","date":"2026-11-21T10:51:28.903+01:00","shareURL":"https://www.tagesschau.de/ausland/x-100.html"},
            {"title":"Bundestag beschließt Haushalt","topline":"Berlin","ressort":"inland","date":"2026-11-21T12:19:36.173+01:00","shareURL":"https://www.tagesschau.de/inland/y-100.html"},
            {"title":"Wolken, Temperaturen, Wind und Aussichten","topline":"Wetter","ressort":"","date":"2026-11-21T09:44:03.463+01:00","shareURL":"https://www.tagesschau.de/wetter/deutschland/z-100.html"},
            {"title":"Wird die Bahn pünktlicher?","topline":"","ressort":"wirtschaft","date":"2026-11-21T08:00:00+01:00","shareURL":"https://www.tagesschau.de/wirtschaft/w-100.html"}
        ],"regional":[{"title":"Regional story"}]}"#;
        let stories = chosen(tagesschau(page).unwrap(), &TAGESSCHAU, now);
        let titles: Vec<&str> = stories.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Bundestag beschließt Haushalt", "Wird die Bahn pünktlicher?", "Erdbeben in Musterland"]);
        let (title, card, spoken) = report(&COUNTRIES[0], &TAGESSCHAU, true, &stories, now);
        assert_eq!(title, "NEWS // GERMANY · TAGESSCHAU");
        assert!(card.starts_with(&format!("01  {}  Berlin: Bundestag beschließt Haushalt", clock("2026-11-21T12:19:36+01:00"))), "{card}");
        assert_eq!(spoken, "Die Schlagzeilen aus Deutschland, laut tagesschau, Sir: Bundestag beschließt Haushalt. Wird die Bahn pünktlicher? Erdbeben in Musterland.");
        assert!(tagesschau("not json").is_err());

        // Asked in German about a country with only an English newsroom; nothing fresh.
        let (_, _, spoken) = report(by_name("France").unwrap(), &guardian("world/france"), true, &stories[..1], now);
        assert!(spoken.starts_with("Aus Frankreich habe ich nur englische Schlagzeilen, Sir. Laut Guardian: "), "{spoken}");
        let (_, _, spoken) = report(by_name("France").unwrap(), &guardian("world/france"), false, &[], now);
        assert_eq!(spoken, "The Guardian has nothing from France in the last three days, Sir.");
    }

    /// The real newsrooms (network): each answers with fresh headlines.
    /// `cargo test --release the_newsrooms_answer -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn the_newsrooms_answer() {
        for source in [TAGESSCHAU, DW, BBC, NPR, ORF, guardian("world/france"), guardian("australia-news")] {
            let stories = chosen(fetch(&source).await.unwrap_or_else(|e| panic!("{}: {e}", source.outlet)), &source, Local::now());
            println!("{}: {}", source.outlet, stories.iter().take(3).map(|s| s.title.as_str()).collect::<Vec<_>>().join(" | "));
            assert!(stories.len() >= 3, "{}: {} fresh stories", source.outlet, stories.len());
        }
    }
}
