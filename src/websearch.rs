//! Live web search from several sources at once, so JAI answers with concrete facts instead
//! of guesses:
//! * DuckDuckGo (HTML endpoint, falling back to Lite when the first serves a bot check): the
//!   top five results (title, URL, snippet), parsed with `scraper`;
//! * Wikipedia: REST summary of the topic, or of the best hit of the search API;
//! * wttr.in for weather questions (live observation + today/tomorrow);
//! * the deep reader (`read_webpage`): the top relevant result is fetched, stripped of scripts,
//!   styles, navigation and footers, and up to 4,500 characters of article text are appended.

use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE};
use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Characters of article text handed to the LLM per page.
pub const PAGE_TEXT_LIMIT: usize = 4500;
const RESULT_COUNT: usize = 5;
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36";

/// Shared browser-like client (also used by the website navigator's channel lookup).
pub(crate) static HTTP: LazyLock<Client> = LazyLock::new(|| {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,application/json;q=0.8,*/*;q=0.7"));
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-GB,en;q=0.9,de;q=0.8"));
    Client::builder()
        .timeout(Duration::from_secs(9))
        .connect_timeout(Duration::from_secs(4))
        .pool_idle_timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::limited(6))
        .tcp_nodelay(true)
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .build()
        .unwrap_or_else(|_| Client::new())
});

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("valid CSS selector")
}

/// Web searches running now (the status pill says so).
static SEARCHING: LazyLock<crate::thinking_guard::ThinkingFlag> = LazyLock::new(Default::default);

pub fn searching() -> bool {
    SEARCHING.is_set()
}

/// What was searched or read in the last 15 minutes: a repeated or narrowed search reuses it
/// instead of fetching it again. shortcut: 32 entries each, oldest out first; more when long
/// research runs re-read pages.
struct Recent(std::sync::Mutex<Vec<(String, std::time::Instant, String)>>);
const KEPT_FOR: Duration = Duration::from_secs(15 * 60);

impl Recent {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(Vec::new()))
    }
    fn get(&self, key: &str) -> Option<String> {
        let mut kept = self.0.lock().unwrap_or_else(|e| e.into_inner());
        kept.retain(|(_, at, _)| at.elapsed() < KEPT_FOR);
        kept.iter().find(|(k, _, _)| k == key).map(|(_, _, v)| v.clone())
    }
    fn put(&self, key: &str, value: &str) {
        let mut kept = self.0.lock().unwrap_or_else(|e| e.into_inner());
        kept.retain(|(k, _, _)| k != key);
        if kept.len() >= 32 {
            kept.remove(0);
        }
        kept.push((key.to_string(), std::time::Instant::now(), value.to_string()));
    }
}

static SEARCHED: Recent = Recent::new();
static PAGES: Recent = Recent::new();

pub async fn search(query: &str) -> Result<String, String> {
    let _searching = SEARCHING.engage();
    let query = query
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '.' | '!' | '?'))
        .trim();
    if query.is_empty() {
        return Err("The search query is empty.".into());
    }
    let key = query.to_lowercase();
    if let Some(kept) = SEARCHED.get(&key) {
        info!("[TOOL] Web search: '{query}' (read in the last 15 minutes)");
        note_sources(&kept);
        return Ok(kept);
    }
    info!("[TOOL] Web search: '{query}'");

    // An encyclopaedia article about the city adds nothing to "what's the weather in Berlin",
    // and the engines get "weather berlin" (live 2026-11-26 the whole question found WhatsApp).
    let place = weather_location(query);
    let wants_weather = place.is_some();
    let asked = place.filter(|place| !place.is_empty()).map_or_else(|| query.to_string(), |place| format!("weather {place}"));
    let wiki_lookup = async {
        if wants_weather {
            None
        } else {
            wikipedia_summary(query).await
        }
    };
    // Two engines at once, their results taken in turn (one blocked or thin leaves the other).
    let (wiki, ddg, bing, weather) = tokio::join!(wiki_lookup, duckduckgo(&asked), bing(&asked), weather(query));
    let results = interleave(
        ddg.unwrap_or_else(|e| {
            warn!("[TOOL] DuckDuckGo search failed: {e}");
            Vec::new()
        }),
        bing.unwrap_or_else(|e| {
            warn!("[TOOL] Bing search failed: {e}");
            Vec::new()
        }),
    );

    let mut out = String::new();
    if let Some(w) = &weather {
        out.push_str(&format!("{w}\n\n"));
    }
    if let Some(w) = &wiki {
        out.push_str(&format!("Wikipedia: {w}\n\n"));
    }
    if !results.is_empty() {
        out.push_str("Web results:\n");
        for (i, r) in results.iter().take(RESULT_COUNT).enumerate() {
            out.push_str(&format!("{}. {} — {} ({})\n", i + 1, r.title, whole_sentences(&r.snippet), r.url));
        }
    }
    // Live weather already has the numbers; otherwise read the best two pages for concrete
    // facts (two sources agree or tell what differs).
    if weather.is_none() {
        for (url, page) in read_best(&results, wiki.is_some()).await {
            out.push_str(&format!("\nPage extract from {url}:\n{page}\n"));
        }
    }
    if out.trim().is_empty() {
        Err(format!("No live results found for '{query}' (offline or blocked)."))
    } else {
        SEARCHED.put(&key, out.trim());
        note_sources(out.trim());
        Ok(out.trim().to_string())
    }
}

/// Two engines' results in turn, each page once (its address without a trailing slash).
fn interleave(a: Vec<SearchResult>, b: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut out: Vec<SearchResult> = Vec::new();
    let (mut a, mut b) = (a.into_iter(), b.into_iter());
    loop {
        let (x, y) = (a.next(), b.next());
        if x.is_none() && y.is_none() {
            return out;
        }
        for r in [x, y].into_iter().flatten() {
            if !out.iter().any(|o| o.url.trim_end_matches('/') == r.url.trim_end_matches('/')) {
                out.push(r);
            }
        }
    }
}

/// Reads the three most relevant readable results at once; the two best-ranked that read well.
async fn read_best(results: &[SearchResult], have_wikipedia: bool) -> Vec<(String, String)> {
    let candidates: Vec<&SearchResult> = results
        .iter()
        .take(RESULT_COUNT)
        .filter(|r| readable_url(&r.url) && !(have_wikipedia && r.url.contains("wikipedia.org")))
        .take(3)
        .collect();
    let reads = futures_util::future::join_all(candidates.iter().map(|r| async move {
        tokio::time::timeout(Duration::from_secs(6), fetch_readable(&r.url)).await.ok().and_then(Result::ok)
    }))
    .await;
    candidates
        .iter()
        .zip(reads)
        .filter_map(|(r, page)| page.filter(|p| p.text.chars().count() >= 200).map(|p| (r.url.clone(), p.text.chars().take(PAGE_TEXT_LIMIT * 3 / 5).collect())))
        .take(2)
        .collect()
}

/// The sources of the last search: (title, address), and when.
static SOURCES: std::sync::Mutex<Option<(std::time::Instant, Vec<(String, String)>)>> = std::sync::Mutex::new(None);

/// What a search's result lists as its sources ("1. Title — snippet (https://…)"; Wikipedia).
pub fn sources_of(result: &str) -> Vec<(String, String)> {
    static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\d+\. (.+?) — .*\((https?://[^\s()]+)\)\s*$").expect("valid source line regex"));
    let mut found: Vec<(String, String)> = LINE.captures_iter(result).map(|c| (c[1].trim().to_string(), c[2].to_string())).collect();
    if result.lines().any(|l| l.starts_with("Wikipedia: ")) {
        found.insert(0, ("Wikipedia".into(), String::new()));
    }
    found
}

fn note_sources(result: &str) {
    *SOURCES.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), sources_of(result)));
}

/// The sources of a search made since `since`, as a list for the chat (Sir, 2026-11-22: "JAI
/// should provide its sources"). None when no search was made since.
pub fn sources_since(since: std::time::Instant) -> Option<String> {
    let kept = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    kept.as_ref().filter(|(at, list)| *at >= since && !list.is_empty()).map(|(_, list)| sources_list(list))
}

fn sources_list(list: &[(String, String)]) -> String {
    let lines: Vec<String> = list.iter().take(5).enumerate().map(|(i, (title, url))| if url.is_empty() { format!("{}. {title}", i + 1) } else { format!("{}. {title} — {url}", i + 1) }).collect();
    format!("Sources:\n{}", lines.join("\n"))
}

/// The host of an address without "www." ("reuters.com").
fn site(url: &str) -> &str {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or_default();
    host.strip_prefix("www.").unwrap_or(host)
}

/// "What are your sources?", "where did you get that from?", "woher weißt du das?": the last
/// search's sources (in the last half hour), shown in full and named aloud by their sites.
pub fn sources_answer(text: &str) -> Option<String> {
    static ASKED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:jai,?\s+)?(?:what\s+(?:are|were|was)\s+(?:your|the|its)\s+sources?|(?:and\s+)?where\s+(?:did|do)\s+you\s+(?:get|find|read|learn|have)\s+(?:that|this|it)(?:\s+from)?|(?:give|show|tell)\s+me\s+(?:your|the)\s+sources?|(?:your\s+)?sources?(?:\s+please)?|which\s+sources?\s+did\s+you\s+use|was\s+(?:sind|waren)\s+(?:deine|die)\s+quellen|woher\s+(?:hast|weißt|weisst)\s+du\s+das|(?:deine\s+)?quellen(?:\s+bitte)?|welche\s+quellen\s+hast\s+du\s+(?:benutzt|genutzt))$")
            .expect("valid sources question regex")
    });
    let asked = text.trim().trim_end_matches(['?', '.', '!']).trim().to_lowercase();
    if !ASKED.is_match(&asked) {
        return None;
    }
    let kept = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    let Some((_, list)) = kept.as_ref().filter(|(at, list)| at.elapsed() < Duration::from_secs(30 * 60) && !list.is_empty()) else {
        return Some("I haven't searched anything recently, Sir: that came from what I know.".into());
    };
    let mut sites: Vec<&str> = Vec::new();
    for (title, url) in list.iter().take(5) {
        let name = if url.is_empty() { title.as_str() } else { site(url) };
        if !sites.contains(&name) {
            sites.push(name);
        }
    }
    let spoken = match sites.as_slice() {
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        [] => String::new(),
    };
    Some(format!("{}\nSpoken: From {spoken}, Sir.", sources_list(list)))
}

fn readable_url(url: &str) -> bool {
    let u = url.to_lowercase();
    u.starts_with("http")
        && ![
            "duckduckgo.com", "youtube.com", "youtu.be", ".pdf", "facebook.com", "instagram.com", "tiktok.com", "twitter.com",
            "//x.com", "reddit.com", "linkedin.com", "pinterest.", "amazon.", "ebay.",
        ]
        .iter()
        .any(|bad| u.contains(bad))
}

pub struct Page {
    pub title: String,
    pub text: String,
}

/// "error decoding response body: … : connection reset" — reqwest's top-level message alone hides
/// the cause.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        out.push_str(&format!(": {s}"));
        source = s.source();
    }
    out
}

/// Downloads a page's HTML, once more after a dropped connection (a reset mid-body otherwise
/// fails the whole answer).
async fn fetch_html(url: &str) -> Result<String, String> {
    if let Some(html) = PAGES.get(url) {
        return Ok(html);
    }
    let mut last = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            debug!("[TOOL] Retrying {url} after: {last}");
        }
        let resp = match HTTP.get(url).send().await {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() || e.is_request() => {
                last = error_chain(&e);
                continue;
            }
            Err(e) => return Err(error_chain(&e)),
        };
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
        if !content_type.is_empty() && !content_type.contains("html") && !content_type.contains("text") {
            return Err(format!("not a web page ({content_type})"));
        }
        match resp.text().await {
            Ok(html) => {
                PAGES.put(url, &html);
                return Ok(html);
            }
            Err(e) => last = error_chain(&e),
        }
    }
    Err(last)
}

async fn fetch_readable(url: &str) -> Result<Page, String> {
    let html = fetch_html(url).await?;
    let page = extract_readable(&html, PAGE_TEXT_LIMIT);
    debug!("[TOOL] Read {url}: {} chars of text", page.text.len());
    Ok(page)
}

/// The `read_webpage` tool: clean article text of one URL.
pub async fn read_webpage(url: &str) -> Result<String, String> {
    let url = url.trim().trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>');
    let url = if url.starts_with("http://") || url.starts_with("https://") { url.to_string() } else { format!("https://{url}") };
    info!("[TOOL] Reading {url}");
    let page = fetch_readable(&url).await?;
    if page.text.trim().is_empty() {
        return Err(format!("{url} has no readable text (it may need JavaScript)."));
    }
    Ok(format!("Title: {}\nURL: {url}\n\n{}", page.title, page.text))
}

/// Elements whose text is never article content.
const BOILERPLATE: &[&str] = &[
    "script", "style", "noscript", "nav", "footer", "header", "aside", "form", "svg", "button", "iframe", "select", "template",
    "dialog", "menu", "figcaption",
];

/// Class names of boilerplate blocks: anywhere in a class for the distinctive ones, as a whole
/// word (`toc`, `share-buttons`) for the short ones that also occur inside unrelated names
/// (`vector-toc-available` on Wikipedia's `<body>`).
fn boilerplate_class(class: &str) -> bool {
    let c = class.to_lowercase();
    const ANYWHERE: &[&str] = &["cookie", "consent", "newsletter", "advert", "breadcrumb", "infobox", "navbox", "reflist", "hatnote", "mw-editsection"];
    const WORDS: &[&str] = &["sidebar", "share", "related", "comment", "comments", "references", "toc", "metadata", "noprint", "footer", "nav", "menu"];
    ANYWHERE.iter().any(|w| c.contains(w))
        || WORDS.iter().any(|w| c == *w || c.starts_with(&format!("{w}-")) || c.starts_with(&format!("{w}_")))
}

fn in_boilerplate(el: &ElementRef<'_>) -> bool {
    std::iter::once(*el).chain(el.ancestors().filter_map(ElementRef::wrap)).any(|a| {
        let name = a.value().name();
        if matches!(name, "html" | "body") {
            return false;
        }
        BOILERPLATE.contains(&name)
            || a.value().attr("role").is_some_and(|r| matches!(r, "navigation" | "banner" | "contentinfo" | "complementary"))
            || a.value().attr("aria-hidden") == Some("true")
            || a.value().classes().any(boilerplate_class)
    })
}

fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text of an element without the text of embedded styles, scripts and footnote markers.
fn element_text(el: &ElementRef<'_>) -> String {
    let mut out = String::new();
    for node in el.descendants() {
        let Some(text) = node.value().as_text() else { continue };
        let hidden = node.ancestors().take_while(|a| a.id() != el.id()).filter_map(ElementRef::wrap).any(|a| {
            matches!(a.value().name(), "script" | "style" | "noscript" | "template" | "svg")
                || a.value().classes().any(|c| c == "reference" || c == "mw-editsection" || c == "sr-only")
        });
        if !hidden {
            out.push_str(text);
        }
    }
    clean(&out)
}

/// Readable text of a page: title + headings, paragraphs, list items and table cells of the main
/// content (`<article>`, `<main>`, …), without scripts, styles, navigation, headers and footers.
pub fn extract_readable(html: &str, max_chars: usize) -> Page {
    let doc = Html::parse_document(html);
    let title = doc.select(&selector("title")).next().map(|t| clean(&t.text().collect::<String>())).unwrap_or_default();
    let blocks = selector("h1, h2, h3, h4, p, li, td, th, pre, blockquote, dd, dt");
    let collect = |root: ElementRef<'_>| -> String {
        let mut out = String::new();
        let mut last = String::new();
        for el in root.select(&blocks) {
            // Nested blocks (a <p> inside an <li>) are read through their outermost block only.
            if el.ancestors().filter_map(ElementRef::wrap).take_while(|a| a.id() != root.id()).any(|a| blocks.matches(&a)) {
                continue;
            }
            if in_boilerplate(&el) {
                continue;
            }
            let line = element_text(&el);
            let heading = matches!(el.value().name(), "h1" | "h2" | "h3" | "h4");
            if line.is_empty() || line == last || (!heading && line.chars().count() < 25 && el.value().name() == "p") {
                continue;
            }
            out.push_str(if heading { "\n## " } else { "" });
            out.push_str(&line);
            out.push('\n');
            last = line;
            if out.chars().count() > max_chars + 200 {
                break;
            }
        }
        out
    };
    let mut text = String::new();
    for css in ["article", "main", "[role=main]", "#content", "#main-content", ".post-content", ".article-body", "body"] {
        if let Some(root) = doc.select(&selector(css)).next() {
            let candidate = collect(root);
            if candidate.chars().count() >= 400 || css == "body" {
                text = candidate;
                break;
            }
        }
    }
    let text = text.trim().to_string();
    let text = if text.chars().count() > max_chars {
        let cut: String = text.chars().take(max_chars).collect();
        // End on a sentence where possible.
        match cut.rfind(['.', '\n']) {
            Some(i) if i > max_chars / 2 => format!("{} …", &cut[..=i]),
            _ => format!("{cut} …"),
        }
    } else {
        text
    };
    Page { title, text }
}

// ------------------------------------------------------------------------- Wikipedia ----

async fn wikipedia_summary(query: &str) -> Option<String> {
    let lower = query.to_lowercase();
    let mut topic = lower.as_str();
    for prefix in [
        "who is ", "who was ", "what is ", "what was ", "what are ", "tell me about ", "wer ist ",
        "wer war ", "was ist ", "was war ", "was sind ", "erzähl mir von ", "erzähle mir über ",
        "erkläre mir ", "erkläre ", "search for ", "suche nach ", "search ", "suche ",
    ] {
        if let Some(rest) = topic.strip_prefix(prefix) {
            topic = rest.trim();
        }
    }
    let topic = topic.trim_matches(|c: char| !c.is_alphanumeric()).trim();
    if topic.is_empty() || topic.split_whitespace().count() > 8 {
        return None;
    }
    let german = lower.chars().any(|c| matches!(c, 'ä' | 'ö' | 'ü' | 'ß'))
        || ["wer ", "was ", "wie ", "der ", "die ", "das "].iter().any(|w| lower.contains(w));
    let langs = if german { ["de", "en"] } else { ["en", "de"] };

    if topic.split_whitespace().count() <= 5 {
        let title: String = topic
            .split_whitespace()
            .map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("_")
            .replace(['?', '#', '/', '%'], "");
        for lang in langs {
            if let Some(summary) = rest_summary(lang, &title).await {
                return Some(summary);
            }
        }
    }
    // Not a page title: ask the search API for the best article.
    for lang in langs {
        let Ok(resp) = HTTP
            .get(format!("https://{lang}.wikipedia.org/w/api.php"))
            .query(&[("action", "query"), ("list", "search"), ("srsearch", topic), ("srlimit", "1"), ("format", "json"), ("utf8", "1")])
            .send()
            .await
        else {
            continue;
        };
        let Ok(json) = resp.json::<Value>().await else { continue };
        if let Some(title) = json["query"]["search"][0]["title"].as_str() {
            if let Some(summary) = rest_summary(lang, &title.replace(' ', "_")).await {
                return Some(summary);
            }
        }
    }
    None
}

async fn rest_summary(lang: &str, title: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(&format!("https://{lang}.wikipedia.org/api/rest_v1/page/summary/")).ok()?;
    url.path_segments_mut().ok()?.pop_if_empty().push(title);
    let resp = HTTP.get(url).header("Accept", "application/json").send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json = resp.json::<Value>().await.ok()?;
    if json.get("type").and_then(Value::as_str) == Some("disambiguation") {
        return None;
    }
    let extract = json.get("extract").and_then(Value::as_str)?;
    if extract.len() <= 40 {
        return None;
    }
    let name = json.get("title").and_then(Value::as_str).unwrap_or(title);
    Some(format!("{name}: {}", truncate(extract, 1100)))
}

// ------------------------------------------------------------------------ DuckDuckGo ----

pub(crate) async fn duckduckgo(query: &str) -> Result<Vec<SearchResult>, String> {
    let html = HTTP
        .post("https://html.duckduckgo.com/html/")
        .form(&[("q", query), ("kl", "wt-wt")])
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let results = parse_ddg_html(&html);
    if !results.is_empty() {
        return Ok(results);
    }
    debug!("[TOOL] DuckDuckGo HTML gave no results; trying Lite");
    let lite = HTTP
        .post("https://lite.duckduckgo.com/lite/")
        .form(&[("q", query)])
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    Ok(parse_ddg_lite(&lite))
}

fn parse_ddg_html(html: &str) -> Vec<SearchResult> {
    let doc = Html::parse_document(html);
    let (link, snippet) = (selector("a.result__a"), selector(".result__snippet"));
    let mut results = Vec::new();
    for block in doc.select(&selector(".result, .web-result")) {
        if block.value().classes().any(|c| c == "result--ad") {
            continue;
        }
        let Some(a) = block.select(&link).next() else { continue };
        let url = a.value().attr("href").map(decode_ddg_link).unwrap_or_default();
        let title = clean(&a.text().collect::<String>());
        let text = block.select(&snippet).next().map(|s| clean(&s.text().collect::<String>())).unwrap_or_default();
        if !title.is_empty() && !url.contains("duckduckgo.com/y.js") && !results.iter().any(|r: &SearchResult| r.url == url) {
            results.push(SearchResult { title, url, snippet: text });
        }
        if results.len() >= RESULT_COUNT {
            break;
        }
    }
    results
}

fn parse_ddg_lite(html: &str) -> Vec<SearchResult> {
    let doc = Html::parse_document(html);
    let links: Vec<ElementRef<'_>> = doc.select(&selector("a.result-link")).collect();
    let snippets: Vec<String> = doc.select(&selector("td.result-snippet")).map(|s| clean(&s.text().collect::<String>())).collect();
    links
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let url = a.value().attr("href").map(decode_ddg_link)?;
            let title = clean(&a.text().collect::<String>());
            (!title.is_empty() && !url.contains("duckduckgo.com/y.js")).then(|| SearchResult {
                title,
                url,
                snippet: snippets.get(i).cloned().unwrap_or_default(),
            })
        })
        .take(RESULT_COUNT)
        .collect()
}

/// Bing's results page: each result's own address is base64 in its redirect ("u=a1…").
async fn bing(query: &str) -> Result<Vec<SearchResult>, String> {
    let html = HTTP.get("https://www.bing.com/search").query(&[("q", query), ("setlang", "en")]).send().await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
    Ok(parse_bing(&html))
}

fn parse_bing(html: &str) -> Vec<SearchResult> {
    let doc = Html::parse_document(html);
    let (link, snippet) = (selector("h2 a"), selector(".b_caption p"));
    doc.select(&selector("li.b_algo"))
        .filter_map(|block| {
            let a = block.select(&link).next()?;
            let url = decode_bing_link(a.value().attr("href")?)?;
            let title = clean(&a.text().collect::<String>());
            let text = block.select(&snippet).next().map(|s| clean(&s.text().collect::<String>())).unwrap_or_default();
            (!title.is_empty()).then_some(SearchResult { title, url, snippet: text })
        })
        .take(RESULT_COUNT)
        .collect()
}

fn decode_bing_link(raw: &str) -> Option<String> {
    use base64::Engine;
    if !raw.contains("bing.com/ck/") {
        return raw.starts_with("http").then(|| raw.to_string());
    }
    let encoded = raw.split(['?', '&']).find_map(|p| p.strip_prefix("u=a1"))?;
    let url = String::from_utf8(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('=')).ok()?).ok()?;
    url.starts_with("http").then_some(url)
}

fn decode_ddg_link(raw: &str) -> String {
    if let Some(i) = raw.find("uddg=") {
        let enc = raw[i + 5..].split('&').next().unwrap_or("");
        return percent_decode(enc);
    }
    if raw.starts_with("//") {
        return format!("https:{raw}");
    }
    raw.to_string()
}

// --------------------------------------------------------------------------- weather ----

static WEATHER_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:weather|wetter|forecast|vorhersage|temperature|temperatur|rain|raining|regen|regnet|snow|schnee|sunny|sonnig|grad draußen|degrees outside)\b")
        .expect("valid weather regex")
});
static WEATHER_PLACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:in|for|at|bei|für|fuer|around|near)\s+([\p{L}0-9][\p{L}0-9 .,'\-]{1,48}?)(?:\s+(?:today|tomorrow|tonight|now|right now|this week|heute|morgen|jetzt|gerade|diese woche|aktuell))*\s*$")
        .expect("valid weather place regex")
});

/// The place a weather question is about; "" means "where Sir is" (wttr.in uses the IP).
fn weather_location(query: &str) -> Option<String> {
    let lower = query.to_lowercase();
    if !WEATHER_WORDS.is_match(&lower) {
        return None;
    }
    let trimmed = lower.trim_end_matches(['?', '.', '!']).trim().to_string();
    let place = WEATHER_PLACE.captures(&trimmed).and_then(|c| c.get(1)).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
    let here = ["my current location", "my location", "current location", "here", "hier", "the moment", "my area", "meiner nähe", "meinem ort"];
    let place = if here.iter().any(|h| place == *h || place.ends_with(h)) || WEATHER_WORDS.is_match(&place) { String::new() } else { place };
    Some(place.replace(" in ", ", "))
}

async fn weather(query: &str) -> Option<String> {
    let mut place = weather_location(query)?;
    // "What's the weather?" with no place: Sir's home (from memory, else the town JAI found him
    // in) beats IP geolocation, whose nearest place can be a district of his town.
    if place.is_empty() {
        place = home_location().or_else(|| crate::location_engine::known_home().and_then(|h| h.town)).unwrap_or_default();
    }
    format_weather(&weather_json(&place).await?)
}

/// wttr.in's JSON for a place ("" = where Sir's IP is).
async fn weather_json(place: &str) -> Option<Value> {
    let mut url = reqwest::Url::parse("https://wttr.in/").ok()?;
    if !place.is_empty() {
        url.path_segments_mut().ok()?.pop_if_empty().push(place);
    }
    url.set_query(Some("format=j1"));
    let resp = HTTP.get(url).header("Accept", "application/json").send().await.ok()?;
    if !resp.status().is_success() {
        warn!("[TOOL] wttr.in returned HTTP {} for '{place}'", resp.status());
        return None;
    }
    resp.json().await.ok()
}

static HOME: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Sir's home town, kept current from long-term memory by the tool layer.
pub fn set_home_location(place: Option<String>) {
    *HOME.write().unwrap_or_else(|e| e.into_inner()) = place;
}

pub fn home_location() -> Option<String> {
    HOME.read().unwrap_or_else(|e| e.into_inner()).clone()
}

static PLACE_IN_FACT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:in|aus|from|near|bei)\s+(\p{Lu}[\p{L}\-]+(?:(?:,\s*|\s+)\p{Lu}[\p{L}\-]+)?)").expect("valid place regex")
});

/// "Sir lives in Musterstadt, Germany." → "Musterstadt, Germany" (memories about home, city or location).
pub fn place_from_memories(memories: &[crate::memory::MemoryEntry]) -> Option<String> {
    let about_home = |m: &&crate::memory::MemoryEntry| {
        let k = m.key.to_lowercase();
        let v = m.value.to_lowercase();
        ["home", "city", "town", "location", "lives", "wohn", "hometown", "address", "stadt", "ort"].iter().any(|w| k.contains(w))
            || v.contains(" lives in ") || v.contains(" wohnt in ") || v.contains(" lives near ")
    };
    memories.iter().filter(about_home).find_map(|m| PLACE_IN_FACT.captures(&m.value).map(|c| c[1].trim().to_string()))
}

/// Top headlines from an RSS feed (titles only).
pub async fn headlines(feed: &str, count: usize) -> Vec<String> {
    static TITLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<item\b.*?<title>\s*(?:<!\[CDATA\[)?(.*?)(?:\]\]>)?\s*</title>").expect("valid rss regex")
    });
    let Ok(resp) = HTTP.get(feed).send().await else { return Vec::new() };
    let Ok(xml) = resp.text().await else { return Vec::new() };
    TITLE
        .captures_iter(&xml)
        .map(|c| c[1].replace("&amp;", "&").replace("&#39;", "'").replace("&quot;", "\"").trim().to_string())
        .filter(|t| !t.is_empty())
        .take(count)
        .collect()
}

fn format_weather(j: &Value) -> Option<String> {
    let now = &j["current_condition"][0];
    let temp = now["temp_C"].as_str()?;
    let s = |v: &Value| v.as_str().unwrap_or("").trim().to_string();
    let area = &j["nearest_area"][0];
    let place = [s(&area["areaName"][0]["value"]), s(&area["region"][0]["value"]), s(&area["country"][0]["value"])]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let observed = s(&now["localObsDateTime"]);
    let mut out = format!(
        "Live weather (wttr.in{}) for {place}: {temp}°C (feels like {}°C), {}, humidity {}%, wind {} km/h.",
        if observed.is_empty() { String::new() } else { format!(", observed {observed}") },
        s(&now["FeelsLikeC"]),
        s(&now["weatherDesc"][0]["value"]),
        s(&now["humidity"]),
        s(&now["windspeedKmph"]),
    );
    for (i, label) in ["Today", "Tomorrow"].iter().enumerate() {
        let day = &j["weather"][i];
        if day.is_null() {
            continue;
        }
        let rain = day["hourly"]
            .as_array()
            .map(|h| h.iter().filter_map(|x| x["chanceofrain"].as_str()?.parse::<u32>().ok()).max().unwrap_or(0))
            .unwrap_or(0);
        out.push_str(&format!(" {label}: {}–{}°C, up to {rain}% chance of rain.", s(&day["mintempC"]), s(&day["maxtempC"])));
    }
    Some(out)
}

// --------------------------------------------------------------------------- helpers ----

pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A search engine's snippet ends wherever the engine cut it ("…which had an energy output"):
/// back to the last whole sentence, else an explicit "…" — never a sentence that stops mid-thought.
pub fn whole_sentences(snippet: &str) -> String {
    let s = snippet.trim();
    if s.is_empty() || s.ends_with(['.', '!', '?', '…', '"', ')']) {
        return s.to_string();
    }
    match [". ", "! ", "? "].iter().filter_map(|p| s.rfind(p)).max() {
        Some(i) if i >= s.len() / 4 => s[..=i].to_string(),
        _ => format!("{s} …"),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        whole_sentences(&s.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_html_and_urls() {
        let page = extract_readable("<p>Hello &amp; welcome to &quot;<b>J.A.I.</b>&quot; &lt;World&gt;, the finest butler!</p>", 500);
        assert_eq!(page.text, "Hello & welcome to \"J.A.I.\" <World>, the finest butler!");
        assert_eq!(
            percent_decode("https%3A%2F%2Fde.wikipedia.org%2Fwiki%2FBundeskanzler"),
            "https://de.wikipedia.org/wiki/Bundeskanzler"
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(
            decode_ddg_link("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&rut=1"),
            "https://example.com/a"
        );
    }

    #[test]
    fn parses_ddg_html_results() {
        let html = r#"<div class="result results_links"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org&amp;rut=x">Rust <b>Programming</b></a>
            <a class="result__snippet" href="x">A language empowering everyone.</a></div>
            <div class="result result--ad"><a class="result__a" href="https://duckduckgo.com/y.js?ad">Ad</a></div>
            <div class="result"><a class="result__a" href="https://example.org">Second</a><a class="result__snippet">Snippet two</a></div>"#;
        let r = parse_ddg_html(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Rust Programming");
        assert_eq!(r[0].url, "https://rust-lang.org");
        assert_eq!(r[0].snippet, "A language empowering everyone.");
        // The screenshot: DuckDuckGo's snippet stopped mid-sentence.
        let cut = "The Arc Reactor was a device initially designed by Howard Stark. This design was miniaturized by Tony to power a life-saving electromagnet in his chest, which had an energy output";
        assert_eq!(whole_sentences(cut), "The Arc Reactor was a device initially designed by Howard Stark.");
        assert_eq!(whole_sentences("Short fragment without an end"), "Short fragment without an end …");
        assert_eq!(whole_sentences("Complete."), "Complete.");
        assert_eq!(r[1].snippet, "Snippet two");
    }

    /// Sir, 2026-11-22: search "much better", with its sources. Bing's page (its shape on that
    /// day: the address base64 in the redirect) joins DuckDuckGo's, each page once, and the
    /// sources of a search are listed and named.
    #[test]
    fn a_second_engine_and_the_sources_of_a_search() {
        let html = r#"<ol><li class="b_algo" data-id><h2><a target="_blank" href="https://www.bing.com/ck/a?!&amp;&amp;p=1e&amp;u=a1aHR0cHM6Ly93d3cuZnJhbmtmdXJ0LWFpcnBvcnQuY29tL2VuLmh0bWw&amp;ntb=1">Frankfurt <b>Airport</b></a></h2>
            <div class="b_caption"><p class="b_lineclamp2">Germany's largest airport.</p></div></li>
            <li class="b_algo"><h2><a href="https://example.org/two/">Two</a></h2></li></ol>"#;
        let bing = parse_bing(html);
        assert_eq!(bing.iter().map(|r| r.url.as_str()).collect::<Vec<_>>(), ["https://www.frankfurt-airport.com/en.html", "https://example.org/two/"]);
        assert_eq!((bing[0].title.as_str(), bing[0].snippet.as_str()), ("Frankfurt Airport", "Germany's largest airport."));
        let r = |url: &str| SearchResult { title: url.into(), url: url.into(), snippet: String::new() };
        let both = interleave(vec![r("https://a.org"), r("https://example.org/two"), r("https://c.org")], bing);
        assert_eq!(both.iter().map(|r| r.url.as_str()).collect::<Vec<_>>(), ["https://a.org", "https://www.frankfurt-airport.com/en.html", "https://example.org/two", "https://c.org"]);

        let result = "Wikipedia: Frankfurt Airport is …\n\nWeb results:\n1. Frankfurt Airport — Germany's largest (and busiest) airport. (https://www.frankfurt-airport.com/en.html)\n2. Two — x (https://example.org/two)\n\nPage extract from https://example.org/two:\ntext";
        assert_eq!(sources_of(result), [("Wikipedia".to_string(), String::new()), ("Frankfurt Airport".into(), "https://www.frankfurt-airport.com/en.html".into()), ("Two".into(), "https://example.org/two".into())]);
        assert_eq!(sources_answer("What's the weather like?"), None);
        let before = std::time::Instant::now();
        note_sources(result);
        let shown = sources_since(before).unwrap();
        assert!(shown.starts_with("Sources:\n1. Wikipedia\n2. Frankfurt Airport — https://www.frankfurt-airport.com/en.html"), "{shown}");
        for asked in ["What are your sources?", "Where did you get that from?", "Woher weißt du das?", "sources please"] {
            let answer = sources_answer(asked).unwrap_or_else(|| panic!("{asked}"));
            assert!(answer.ends_with("\nSpoken: From Wikipedia, frankfurt-airport.com and example.org, Sir."), "{answer}");
        }
    }

    #[test]
    fn parses_ddg_lite_results() {
        let html = r#"<table><tr><td><a rel="nofollow" href="https://example.com/a" class='result-link'>Example A</a></td></tr>
            <tr><td class='result-snippet'>First snippet</td></tr>
            <tr><td><a href="https://example.com/b" class="result-link">Example B</a></td></tr>
            <tr><td class="result-snippet">Second snippet</td></tr></table>"#;
        let r = parse_ddg_lite(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1], SearchResult { title: "Example B".into(), url: "https://example.com/b".into(), snippet: "Second snippet".into() });
    }

    #[test]
    fn extracts_article_text_without_boilerplate() {
        let html = r#"<html><head><title>Mars facts</title><style>p{}</style><script>var x = "tracking";</script></head><body>
            <nav><p>Home | About | Contact us for more information today</p></nav>
            <header><h1>Site header</h1></header>
            <article><h1>Mars</h1>
              <table class="infobox"><tr><td><style>.mw-parser-output{margin:0}</style>Infobox junk cell</td></tr></table>
              <p>Mars is the fourth planet from the Sun and has a mean radius of 3,389.5 kilometres.<sup class="reference">[9]</sup></p>
              <p>A Martian day lasts 24 hours and 37 minutes, and a year lasts 687 Earth days.</p>
              <ul><li>Moons: Phobos and Deimos, both small and irregular in shape.</li></ul>
              <div class="share-buttons"><p>Share this on every social network you know</p></div>
            </article>
            <footer><p>Copyright 2026 Example Media Group, all rights reserved.</p></footer></body></html>"#;
        let page = extract_readable(html, 4500);
        assert_eq!(page.title, "Mars facts");
        assert!(page.text.contains("## Mars"));
        assert!(page.text.contains("3,389.5 kilometres"));
        assert!(page.text.contains("687 Earth days"));
        assert!(page.text.contains("Phobos and Deimos"));
        for junk in ["tracking", "Contact us", "Copyright", "Site header", "Share this", "Infobox", "mw-parser", "[9]"] {
            assert!(!page.text.contains(junk), "{junk} leaked into: {}", page.text);
        }
        let short = extract_readable(html, 60);
        assert!(short.text.chars().count() <= 64);
    }

    #[test]
    fn weather_questions_find_their_place() {
        assert_eq!(weather_location("Wie ist das Wetter gerade in Berlin?").as_deref(), Some("berlin"));
        assert_eq!(weather_location("What's the weather in London tomorrow").as_deref(), Some("london"));
        assert_eq!(weather_location("What's the weather like in Berlin").as_deref(), Some("berlin"));
        assert_eq!(weather_location("what's the weather at my current location").as_deref(), Some(""));
        assert_eq!(weather_location("Weather forecast").as_deref(), Some(""));
        assert_eq!(weather_location("Who is the chancellor of Germany"), None);
    }

    #[test]
    fn formats_wttr_json() {
        let j = serde_json::json!({
            "current_condition": [{"temp_C": "11", "FeelsLikeC": "9", "humidity": "87", "windspeedKmph": "12",
                "localObsDateTime": "2026-10-20 10:45 PM", "weatherDesc": [{"value": "Light rain"}]}],
            "nearest_area": [{"areaName": [{"value": "Musterstadt"}], "region": [{"value": "Baden-Wurttemberg"}], "country": [{"value": "Germany"}]}],
            "weather": [{"mintempC": "8", "maxtempC": "13", "hourly": [{"chanceofrain": "40"}, {"chanceofrain": "80"}]}]
        });
        let text = format_weather(&j).unwrap();
        assert!(text.contains("Musterstadt, Baden-Wurttemberg, Germany: 11°C (feels like 9°C), Light rain"));
        assert!(text.contains("observed 2026-10-20 10:45 PM"));
        assert!(text.contains("Today: 8–13°C, up to 80% chance of rain."));
    }

    #[tokio::test]
    #[ignore = "requires internet access"]
    async fn live_search_english() {
        let text = search("James Webb Space Telescope").await.unwrap();
        println!("{text}");
        assert!(text.to_lowercase().contains("telescope"));
        assert!(text.contains("Web results:") || text.contains("Wikipedia:"));
    }

    #[tokio::test]
    #[ignore = "requires internet access"]
    async fn live_weather_and_reader() {
        let weather = search("What's the weather in Berlin today").await.unwrap();
        println!("{weather}");
        assert!(weather.contains("Live weather"));
        let page = read_webpage("https://en.wikipedia.org/wiki/Rust_(programming_language)").await.unwrap();
        println!("{}", page.chars().take(600).collect::<String>());
        assert!(page.contains("Rust"));
    }
}
