//! The HUD board's research agent and place boards.
//!
//! Research ("can you do a state of VR report for me?"): eight web searches (the topic and its
//! facet queries from the Deep Research Engine), up to 48 pages read six at a time, facts
//! extracted with the engine's heuristics, then 8-13 blocks assembled — by the language model
//! from the numbered facts when the GPU may run (every number it uses must appear in a fact),
//! otherwise by heuristics. The live counts feed the `RESEARCH // ACTIVE` block; at the end the
//! report is saved, a crystal chime plays and JAI offers to put it on screen.
//!
//! Places ("show me Tulleys Farm on the map"): Nominatim coordinates, OpenStreetMap roads and
//! water from OpenFreeMap's vector tiles (decoded here: Mapbox Vector Tile protobuf) or, when
//! those fail, the Overpass API, and nearby photos from Wikimedia Commons.

use crate::deep_research_engine::{self as engine, Candidate};
use crate::focus::PendingOffer;
use crate::hud_board::{self, Bar, Block, BlockKind, Note, Report, ResearchStatus, Road};
use crate::jai_voice_dsp::Earcon;
use crate::tools::ToolContext;
use futures_util::stream::{self, StreamExt};
use regex::Regex;
use reqwest::{Client, Url};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const API_AGENT: &str = "JAI-HUD/3.6 (local personal assistant; board research)";
const PAGE_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36";
const MAX_SOURCES: usize = 48;
const SEARCHES: usize = 8;
const MAX_BLOCKS: usize = 13;
/// Pages whose text is behind scripts or logins.
const SKIP_DOMAINS: [&str; 9] = ["youtube.com", "facebook.com", "instagram.com", "tiktok.com", "x.com", "twitter.com", "pinterest.", "linkedin.com", "duckduckgo.com"];

static HTTP: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(12))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::limited(6))
        .build()
        .unwrap_or_default()
});

/// A page the agent read.
#[derive(Debug, Clone)]
struct Page {
    url: String,
    domain: String,
    title: String,
    reliability: f32,
    facts: Vec<Candidate>,
    image: Option<String>,
}

/// A fact with where it came from (numbered for the language model).
#[derive(Debug, Clone)]
struct Fact {
    claim: String,
    category: String,
    score: f32,
    domain: String,
}

/// Starts the research agent in the background.
pub fn start(tools: ToolContext, topic: String) {
    hud_board::set_research(Some(ResearchStatus { topic: topic.clone(), phase: "Starting research agent".into(), started: Instant::now(), blocks: 0, sources: 0, progress: 0.02 }));
    tokio::spawn(async move {
        let t0 = Instant::now();
        match run(&tools, &topic).await {
            Ok(report) => finish(&tools, report, t0),
            Err(e) => {
                warn!("[BOARD] Research on '{topic}' failed: {e}");
                hud_board::set_research(None);
                let _ = tools.stark.events.send(crate::stark::StarkEvent::Alert(format!("I'm afraid the {topic} research came back empty, Sir. {e}")));
            }
        }
    });
}

fn finish(tools: &ToolContext, mut report: Report, t0: Instant) {
    let db = tools.config.read(|c| c.memory_db_path.clone());
    report.id = hud_board::save_report(&db, &report);
    let (topic, blocks, sources) = (report.topic.clone(), report.blocks.len(), report.sources.len());
    info!("[BOARD] Report '{topic}' assembled in {} s: {blocks} blocks, {sources} sources", t0.elapsed().as_secs());
    hud_board::set_pending(report);
    tools.focus.note_offer(PendingOffer { tool: hud_board::TOOL.into(), args: json!({ "action": "present" }), terminate: None });
    if !tools.modality.voice_muted() {
        crate::tts::play_earcon(Earcon::ReportChime);
    }
    let line = hud_board::finished_line(&topic, blocks, sources);
    let events = tools.stark.events.clone();
    tokio::spawn(async move {
        // The chime rings out before he speaks.
        tokio::time::sleep(Duration::from_millis(750)).await;
        let _ = events.send(crate::stark::StarkEvent::Alert(line));
    });
}

async fn run(tools: &ToolContext, topic: &str) -> Result<Report, String> {
    let topic_terms = engine::terms(topic);
    let mut queries = vec![topic.to_string()];
    queries.extend(engine::plan_queries(topic, &[]).into_iter().map(|q| q.search).filter(|q| !q.eq_ignore_ascii_case(topic)).take(SEARCHES - 1));

    // 1. Searches (three at a time).
    hud_board::update_research(|r| r.phase = "Searching the web".into());
    let total = queries.len() as f32;
    let mut done = 0.0;
    let mut links: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut searches = stream::iter(queries.into_iter().map(|q| async move { crate::websearch::duckduckgo(&q).await.unwrap_or_default() })).buffer_unordered(3);
    while let Some(results) = searches.next().await {
        done += 1.0;
        for r in results {
            let norm = engine::normalize_url(&r.url);
            let domain = engine::domain_of(&r.url);
            if r.url.starts_with("http") && !SKIP_DOMAINS.iter().any(|d| domain.contains(d)) && seen.insert(norm) && links.len() < MAX_SOURCES {
                links.push(r.url);
            }
        }
        hud_board::update_research(|r| r.progress = 0.04 + 0.22 * done / total);
    }
    if links.is_empty() {
        return Err("No search results came back.".into());
    }

    // 2. Pages (six at a time).
    hud_board::update_research(|r| r.phase = "Reading sources".into());
    let wanted = links.len() as f32;
    let mut pages: Vec<Page> = Vec::new();
    let mut read = 0.0;
    let mut fetches = stream::iter(links.into_iter().map(|url| {
        let terms = topic_terms.clone();
        async move { read_page(&url, &terms).await }
    }))
    .buffer_unordered(6);
    while let Some(page) = fetches.next().await {
        read += 1.0;
        if let Some(page) = page {
            pages.push(page);
        }
        let facts: usize = pages.iter().map(|p| p.facts.len()).sum();
        let sources = pages.len();
        hud_board::update_research(|r| {
            r.sources = sources;
            r.blocks = (2 + facts / 4).min(MAX_BLOCKS);
            r.progress = 0.26 + 0.56 * read / wanted;
        });
    }
    if pages.is_empty() {
        return Err("None of the sources could be read.".into());
    }

    // 3. Assembly.
    hud_board::update_research(|r| {
        r.phase = "Assembling blocks".into();
        r.progress = 0.84;
    });
    let facts = rank_facts(&pages);
    let llm = if crate::vram_guard::llm_allowed() { compose_with_llm(tools, topic, &facts).await } else { None };
    let mut blocks = llm.unwrap_or_default();
    if blocks.len() < 6 {
        blocks = merge(blocks, compose_heuristic(&facts));
    }
    // Photos from the best-scored pages (and their first image prefetched).
    let mut images: Vec<(String, String, String)> = Vec::new();
    for p in pages.iter().filter(|p| p.image.is_some()) {
        let url = p.image.clone().unwrap_or_default();
        if images.iter().all(|(u, _, _)| *u != url) {
            images.push((url, p.title.clone(), p.domain.clone()));
        }
    }
    if let Some(brief) = wiki_image(topic).await {
        images.insert(0, brief);
    }
    for (url, title, domain) in images.into_iter().take(3) {
        if blocks.len() >= MAX_BLOCKS {
            break;
        }
        let words: String = title.split_whitespace().take(4).collect::<Vec<_>>().join(" ");
        blocks.push(Block::new(format!("PHOTO // {words}"), format!("SOURCE: {}", short_domain(&domain)), format!("SOURCE: {domain}"), BlockKind::MediaCard { images: vec![url], caption: title }));
    }
    // Videos on the topic: wide blocks with a moving preview (a click opens them on YouTube).
    hud_board::update_research(|r| r.phase = "Finding videos".into());
    for (id, title) in youtube_videos(topic).await {
        if blocks.len() >= MAX_BLOCKS {
            break;
        }
        let words: String = hud_board::video_length(&title).0.split_whitespace().take(5).collect::<Vec<_>>().join(" ").to_uppercase();
        blocks.push(Block::new(format!("VIDEO // {words}"), "SOURCE: YOUTUBE", format!("youtube.com/watch?v={id}"), BlockKind::MediaCard { images: hud_board::video_frames(&id), caption: title }));
    }
    blocks.truncate(MAX_BLOCKS);
    number_notes(&mut blocks);
    hud_board::update_research(|r| {
        r.blocks = blocks.len();
        r.progress = 1.0;
    });
    let mut sources: Vec<String> = pages.iter().map(|p| p.url.clone()).collect();
    sources.dedup();
    Ok(Report { topic: topic.to_string(), created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(), blocks, sources, id: None })
}

/// Videos per report.
const MAX_VIDEOS: usize = 2;

/// The first videos YouTube's own results page lists for `topic` (no API key): (id, title).
async fn youtube_videos(topic: &str) -> Vec<(String, String)> {
    let url = format!("https://www.youtube.com/results?search_query={}", urlencoding::encode(topic));
    let Ok(resp) = HTTP.get(&url).header("User-Agent", PAGE_AGENT).header("Accept-Language", "en-GB,en;q=0.8").timeout(Duration::from_secs(8)).send().await else { return Vec::new() };
    let Ok(html) = resp.text().await else { return Vec::new() };
    let videos = parse_youtube_results(&html, MAX_VIDEOS);
    info!("[BOARD] {} video(s) for '{topic}'", videos.len());
    videos
}

/// (id, title) of the `limit` first results in a YouTube results page (its embedded
/// `"videoRenderer":{"videoId":…,"title":{"runs":[{"text":…}]}}` JSON).
pub fn parse_youtube_results(html: &str, limit: usize) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for chunk in html.split("\"videoRenderer\":{").skip(1) {
        let Some(id) = chunk.split("\"videoId\":\"").nth(1).and_then(|r| r.get(..11)) else { continue };
        if hud_board::youtube_id(&format!("youtu.be/{id}")).is_none() || out.iter().any(|(i, _)| i == id) {
            continue;
        }
        let title = chunk.split("\"title\":{\"runs\":[{\"text\":\"").nth(1).map(json_string_prefix).unwrap_or_default();
        // Its running time ("12:34") rides at the end of the title, for the tile's corner.
        let length = chunk.split("\"lengthText\":").nth(1).and_then(|l| l.split("\"simpleText\":\"").nth(1)).map(json_string_prefix);
        let title = match length {
            Some(len) if (4..=8).contains(&len.len()) && len.bytes().all(|b| b.is_ascii_digit() || b == b':') => format!("{title} · {len}"),
            _ => title,
        };
        out.push((id.to_string(), title));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// A JSON string body up to its closing quote, unescaped (`\u0026` → `&`).
fn json_string_prefix(raw: &str) -> String {
    let mut end = raw.len();
    let mut escaped = false;
    for (i, c) in raw.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => {
                end = i;
                break;
            }
            _ => escaped = false,
        }
    }
    serde_json::from_str::<String>(&format!("\"{}\"", &raw[..end])).unwrap_or_else(|_| raw[..end].to_string())
}

async fn read_page(url: &str, topic_terms: &[String]) -> Option<Page> {
    let resp = HTTP.get(url).header("User-Agent", PAGE_AGENT).header("Accept", "text/html,application/xhtml+xml").timeout(Duration::from_secs(10)).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    if !ctype.is_empty() && !ctype.contains("html") {
        return None;
    }
    let final_url = resp.url().clone();
    let bytes = resp.bytes().await.ok()?;
    let html = String::from_utf8_lossy(&bytes[..bytes.len().min(3 * 1024 * 1024)]).to_string();
    let image = og_image(&html, &final_url);
    let page = engine::html_to_markdown(&html, Some(&final_url));
    if page.markdown.len() < 400 {
        return None;
    }
    let reliability = engine::reliability(final_url.as_str());
    let facts = engine::extract_facts(&page.markdown, &page.title, topic_terms, reliability);
    Some(Page { url: final_url.to_string(), domain: engine::domain_of(final_url.as_str()), title: page.title, reliability, facts, image })
}

static OG_IMAGE: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r#"(?i)<meta[^>]+(?:property|name)=["'](?:og:image|twitter:image)(?::url)?["'][^>]*content=["']([^"']+)["']"#).expect("regex"),
        Regex::new(r#"(?i)<meta[^>]+content=["']([^"']+)["'][^>]*(?:property|name)=["'](?:og:image|twitter:image)(?::url)?["']"#).expect("regex"),
    ]
});

/// A page's share image (absolute URL), skipping logos and icons.
fn og_image(html: &str, base: &Url) -> Option<String> {
    let head = crate::crash_guard::clip_bytes(html, 200_000);
    let raw = OG_IMAGE.iter().find_map(|re| re.captures(head).map(|c| c[1].replace("&amp;", "&")))?;
    let url = base.join(raw.trim()).ok()?.to_string();
    let lower = url.to_lowercase();
    (url.starts_with("http") && !["logo", "icon", "favicon", "placeholder", "default-share", ".svg"].iter().any(|w| lower.contains(w))).then_some(url)
}

/// The topic's Wikipedia lead image: (url, title, domain).
async fn wiki_image(topic: &str) -> Option<(String, String, String)> {
    let brief = engine::wikipedia_brief(topic).await?;
    let title = brief.url.rsplit('/').next()?.to_string();
    let v: Value = HTTP.get(format!("https://en.wikipedia.org/api/rest_v1/page/summary/{title}")).header("User-Agent", API_AGENT).send().await.ok()?.json().await.ok()?;
    let src = v["thumbnail"]["source"].as_str().or(v["originalimage"]["source"].as_str())?;
    Some((src.to_string(), brief.title, "wikipedia.org".into()))
}

/// Every page's facts, strongest first, without near-duplicates.
fn rank_facts(pages: &[Page]) -> Vec<Fact> {
    let mut out: Vec<Fact> = pages
        .iter()
        .flat_map(|p| p.facts.iter().map(move |f| Fact { claim: f.claim.trim().to_string(), category: f.category.clone(), score: f.confidence * (0.5 + p.reliability), domain: p.domain.clone() }))
        .filter(|f| f.claim.len() >= 40 && f.claim.len() <= 420)
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut seen = HashSet::new();
    out.retain(|f| seen.insert(f.claim.to_lowercase().chars().filter(|c| c.is_alphanumeric()).take(70).collect::<String>()));
    out.truncate(80);
    out
}

fn short_domain(domain: &str) -> String {
    let d = domain.trim_start_matches("www.");
    d.split('.').rev().nth(1).unwrap_or(d).to_uppercase()
}

// --------------------------------------------------------------------- composition ----

/// The notes' running numbers ("NOTE // 03 — …") once the blocks are final.
fn number_notes(blocks: &mut [Block]) {
    let mut n = 0;
    for b in blocks.iter_mut() {
        if let BlockKind::NoteStack { .. } = b.kind {
            n += 1;
            let section = b.label.split_once('—').map(|(_, s)| s.trim().to_string()).unwrap_or_else(|| b.label.clone());
            b.label = format!("NOTE // {n:02} — {section}");
        }
    }
}

/// `primary` first, topped up from `extra` with kinds it lacks (one chart at most).
fn merge(primary: Vec<Block>, extra: Vec<Block>) -> Vec<Block> {
    let mut out = primary;
    let has_chart = out.iter().any(|b| matches!(b.kind, BlockKind::BarChart { .. }));
    for b in extra {
        if out.len() >= MAX_BLOCKS - 3 {
            break;
        }
        if has_chart && matches!(b.kind, BlockKind::BarChart { .. }) {
            continue;
        }
        if out.iter().any(|o| o.label == b.label) {
            continue;
        }
        out.push(b);
    }
    out
}

const SECTIONS: [(&str, &str); 7] = [
    ("definition", "OVERVIEW"),
    ("architecture", "HOW IT WORKS"),
    ("statistic", "BY THE NUMBERS"),
    ("benchmark", "PERFORMANCE"),
    ("history", "TIMELINE"),
    ("limitation", "LIMITS & RISKS"),
    ("claim", "KEY POINTS"),
];

static FIGURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:~|≈|about |around |nearly |over |more than )?(\$|€|£)?(\d{1,3}(?:[.,]\d{3})*(?:\.\d+)?|\d+(?:\.\d+)?)\s*(%|percent|per cent|million|billion|trillion|thousand|bn\b|m\b|k\b)").expect("regex")
});
static SHARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([A-Z][A-Za-z0-9\-]+(?:\s+[A-Z0-9][A-Za-z0-9\-]*){0,2})\b[^.%\d]{0,32}?(\d{1,2}(?:\.\d+)?)\s?%").expect("regex")
});
const NOT_LABELS: [&str; 14] = ["The", "In", "According", "This", "That", "It", "As", "With", "Of", "By", "For", "About", "Around", "Over"];

/// "63.7%", "20M", "$1.2B", "100+" from a matched figure.
fn format_figure(c: &regex::Captures<'_>) -> String {
    let currency = c.get(1).map(|m| m.as_str()).unwrap_or("");
    let number = c[2].replace(',', "");
    let unit = c[3].to_lowercase();
    let suffix = match unit.as_str() {
        "%" | "percent" | "per cent" => "%",
        "million" | "m" => "M",
        "billion" | "bn" => "B",
        "trillion" => "T",
        "thousand" | "k" => "K",
        _ => "",
    };
    format!("{currency}{number}{suffix}")
}

/// A headline from a claim: its first clause, at most nine words.
fn headline_of(claim: &str) -> String {
    let first = claim.split([',', ';', ':', '(']).next().unwrap_or(claim);
    let words: Vec<&str> = first.split_whitespace().take(9).collect();
    words.join(" ").trim_end_matches(['.', ' ']).to_string()
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut))
}

/// Blocks from the facts alone: section note stacks, hero figures and a share chart.
fn compose_heuristic(facts: &[Fact]) -> Vec<Block> {
    let year = chrono::Local::now().format("%Y").to_string();
    let mut blocks = Vec::new();
    // A chart of shares quoted for named things ("Quest 2 … 38 %").
    let mut bars: Vec<Bar> = Vec::new();
    let mut chart_domains: Vec<String> = Vec::new();
    for f in facts.iter().filter(|f| f.category == "statistic" || f.claim.contains('%')) {
        for c in SHARE.captures_iter(&f.claim) {
            let label = c[1].trim().to_string();
            let Ok(value) = c[2].parse::<f32>() else { continue };
            if NOT_LABELS.contains(&label.split_whitespace().next().unwrap_or("")) || label.len() < 2 || bars.iter().any(|b| b.label.eq_ignore_ascii_case(&label)) {
                continue;
            }
            bars.push(Bar { label, value });
            if !chart_domains.contains(&f.domain) {
                chart_domains.push(f.domain.clone());
            }
        }
        if bars.len() >= 6 {
            break;
        }
    }
    if bars.len() >= 3 {
        let caption = format!("Percentages quoted in {} — each bar is the figure a source gave for that name.", chart_domains.iter().take(2).cloned().collect::<Vec<_>>().join(" and "));
        blocks.push(Block::new("CHART // SHARES QUOTED", format!("SOURCE: {}", short_domain(&chart_domains[0])), format!("SOURCE: {}", chart_domains[0]), BlockKind::BarChart { unit: "%".into(), bars, caption }));
    }
    // Hero figures.
    let mut figures = 0;
    for f in facts.iter().filter(|f| matches!(f.category.as_str(), "statistic" | "benchmark" | "history")) {
        if figures >= 3 {
            break;
        }
        let Some(c) = FIGURE.captures(&f.claim) else { continue };
        let figure = format_figure(&c);
        let before = &f.claim[..c.get(0).map(|m| m.start()).unwrap_or(0)];
        let header: Vec<&str> = before.split_whitespace().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
        let header = if header.len() >= 2 { header.join(" ") } else { headline_of(&f.claim) };
        blocks.push(Block::new(format!("FIGURE // {year}"), "FACT", format!("SOURCE: {}", f.domain), BlockKind::HeroFigure { header: clip(&header, 48), figure, note: clip(&f.claim, 150) }));
        figures += 1;
    }
    // Note stacks by section (up to four notes each).
    for (category, section) in SECTIONS {
        let notes: Vec<Note> = facts.iter().filter(|f| f.category == category).take(4).map(|f| Note { headline: headline_of(&f.claim), body: clip(&f.claim, 260) }).collect();
        if notes.is_empty() {
            continue;
        }
        let domain = facts.iter().find(|f| f.category == category).map(|f| f.domain.clone()).unwrap_or_default();
        blocks.push(Block::new(format!("NOTE // — {section}"), "FACT", format!("SOURCE: {domain}"), BlockKind::NoteStack { notes }));
    }
    blocks
}

/// The language model's layout of the facts (None when it is unavailable or invents numbers).
async fn compose_with_llm(tools: &ToolContext, topic: &str, facts: &[Fact]) -> Option<Vec<Block>> {
    if facts.len() < 6 {
        return None;
    }
    let listing: String = facts.iter().take(40).enumerate().map(|(i, f)| format!("#{} [{}] ({}) {}\n", i + 1, f.category, f.domain, f.claim)).collect();
    let instruction = format!(
        "You assemble a HUD research report about \"{topic}\" from the numbered facts. Reply with JSON only, no prose:\n\
         {{\"notes\": [{{\"section\": \"1-3 word uppercase section\", \"headline\": \"at most 9 words\", \"body\": \"one or two sentences, at most 40 words\", \"facts\": [numbers]}}],\n\
         \"figures\": [{{\"header\": \"at most 5 words\", \"figure\": \"the number exactly as a fact gives it, e.g. 63.7% or 20m or 100+\", \"note\": \"at most 18 words\", \"fact\": number}}],\n\
         \"chart\": null or {{\"title\": \"at most 5 words\", \"unit\": \"% or a unit\", \"bars\": [{{\"label\": \"at most 12 characters\", \"value\": number}}], \"caption\": \"one or two sentences: what the numbers measure and where they come from\", \"facts\": [numbers]}}}}\n\
         6 to 9 notes (notes with the same section are stacked together), 2 or 3 figures, a chart only when 3 to 6 facts give comparable numbers for different things. \
         Use only numbers that appear in the facts; never invent or estimate."
    );
    let raw = tools.pim.complete(&instruction, &listing, 1400).await?;
    let json = raw.get(raw.find('{')?..=raw.rfind('}')?)?;
    let v: Value = serde_json::from_str(json).ok()?;
    let fact_text = |ids: &Value| -> String {
        ids.as_array().map(|a| a.iter().filter_map(Value::as_u64).filter_map(|i| facts.get(i as usize - 1).map(|f| f.claim.clone())).collect::<Vec<_>>().join(" ")).unwrap_or_default()
    };
    let all: String = facts.iter().take(40).map(|f| f.claim.as_str()).collect::<Vec<_>>().join(" ");
    let domain_of = |ids: &Value| -> String {
        ids.as_array().and_then(|a| a.iter().filter_map(Value::as_u64).find_map(|i| facts.get(i as usize - 1).map(|f| f.domain.clone()))).unwrap_or_default()
    };
    let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect::<String>().trim_matches('.').to_string();
    let grounded = |number: &str, text: &str| {
        let d = digits(number);
        !d.is_empty() && text.replace(',', "").contains(&d)
    };
    let year = chrono::Local::now().format("%Y").to_string();
    let mut blocks = Vec::new();
    // The chart.
    if let Some(chart) = v.get("chart").filter(|c| c.is_object()) {
        let source = fact_text(&chart["facts"]);
        let bars: Vec<Bar> = chart["bars"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|b| Some(Bar { label: b["label"].as_str()?.chars().take(12).collect(), value: b["value"].as_f64()? as f32 }))
                    .filter(|b| grounded(&format!("{}", b.value), &source) || grounded(&format!("{}", b.value), &all))
                    .collect()
            })
            .unwrap_or_default();
        if (3..=6).contains(&bars.len()) {
            let domain = domain_of(&chart["facts"]);
            let title = chart["title"].as_str().unwrap_or("chart").to_string();
            blocks.push(Block::new(
                format!("CHART // {title}"),
                format!("SOURCE: {}", short_domain(&domain)),
                format!("SOURCE: {domain}"),
                BlockKind::BarChart { unit: chart["unit"].as_str().unwrap_or("").chars().take(4).collect(), bars, caption: clip(chart["caption"].as_str().unwrap_or(""), 180) },
            ));
        }
    }
    // Figures.
    for f in v["figures"].as_array().into_iter().flatten().take(3) {
        let (Some(figure), Some(header)) = (f["figure"].as_str(), f["header"].as_str()) else { continue };
        let ids = json!([f["fact"].clone()]);
        let source = fact_text(&ids);
        if !grounded(figure, if source.is_empty() { &all } else { &source }) {
            info!("[BOARD] Dropped an ungrounded figure '{figure}'");
            continue;
        }
        blocks.push(Block::new(
            format!("FIGURE // {year}"),
            "FACT",
            format!("SOURCE: {}", domain_of(&ids)),
            BlockKind::HeroFigure { header: clip(header, 48), figure: figure.chars().take(10).collect(), note: clip(f["note"].as_str().unwrap_or(""), 150) },
        ));
    }
    // Notes, stacked by section.
    let mut stacks: Vec<(String, Vec<Note>, String)> = Vec::new();
    for n in v["notes"].as_array().into_iter().flatten().take(10) {
        let (Some(headline), Some(body)) = (n["headline"].as_str(), n["body"].as_str()) else { continue };
        let section = n["section"].as_str().unwrap_or("KEY POINTS").to_uppercase();
        let note = Note { headline: clip(headline, 80), body: clip(body, 280) };
        match stacks.iter_mut().find(|(s, notes, _)| *s == section && notes.len() < 4) {
            Some((_, notes, _)) => notes.push(note),
            None => stacks.push((section, vec![note], domain_of(&n["facts"]))),
        }
    }
    for (section, notes, domain) in stacks {
        blocks.push(Block::new(format!("NOTE // — {section}"), "FACT", format!("SOURCE: {domain}"), BlockKind::NoteStack { notes }));
    }
    (!blocks.is_empty()).then_some(blocks)
}

// ----------------------------------------------------------------------- place boards ----

/// A place on the dark vector map with photos of it.
pub async fn place_board(tools: &ToolContext, place: &str) -> Result<Report, String> {
    let near = tools.stark.location.current(tools).await.ok().map(|f| (f.lat, f.lon));
    let (lat, lon, name) = tools.stark.location.geocode(place, near).await?;
    let span = MAP_SPAN;
    // The streets arrive later (`street_data`): the board goes up as soon as the place is found.
    let (photos, brief) = tokio::join!(commons_photos(lat, lon), engine::wikipedia_brief(place));
    let title = name.split(',').take(2).map(str::trim).collect::<Vec<_>>().join(", ");
    let mut blocks = vec![Block::new(
        format!("MAP // {}", name.split(',').next().unwrap_or(place).trim()),
        "SOURCE: OSM",
        "SOURCE: OPENSTREETMAP CONTRIBUTORS",
        BlockKind::DarkVectorMap { place: title.clone(), lat, lon, span, roads: Vec::new(), water: Vec::new(), pending: true },
    )];
    if !photos.is_empty() {
        // The first photo block holds the whole gallery ("maximise that" shows them all).
        blocks.push(Block::new(format!("PHOTOS // {}", title.split(',').next().unwrap_or("")), format!("{} PHOTOS", photos.len()), "SOURCE: WIKIMEDIA COMMONS", BlockKind::MediaCard { images: photos.clone(), caption: title.clone() }));
        for url in photos.iter().skip(1).take(2) {
            blocks.push(Block::new("PHOTO // NEARBY", "SOURCE: COMMONS", "SOURCE: WIKIMEDIA COMMONS", BlockKind::MediaCard { images: vec![url.clone()], caption: title.clone() }));
        }
    }
    let mut notes = vec![Note { headline: title.clone(), body: format!("{name}. {lat:.5}° N, {lon:.5}° E.") }];
    if let Some(b) = brief.filter(|b| !b.extract.is_empty()) {
        notes.insert(0, Note { headline: b.title.clone(), body: clip(&b.extract, 280) });
    }
    blocks.push(Block::new("NOTE // 01 — LOCATION", "FACT", "SOURCE: WIKIPEDIA / NOMINATIM", BlockKind::NoteStack { notes }));
    Ok(Report {
        topic: title,
        created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        blocks,
        sources: vec!["https://www.openstreetmap.org".into(), "https://commons.wikimedia.org".into()],
        id: None,
    })
}

/// Degrees of latitude a place map spans (≈1.5 km).
pub const MAP_SPAN: f64 = 0.014;

/// Public Overpass instances, tried in turn (the main one often answers 502 when it is full).
const OVERPASS: [&str; 3] = [
    "https://overpass-api.de/api/interpreter",
    "https://maps.mail.ru/osm/tools/overpass/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
];

/// Roads (by class) and water around a point: OpenFreeMap's vector tiles (a CDN, fast), then
/// the Overpass API's instances in turn. None when every source failed.
pub async fn street_data(lat: f64, lon: f64) -> Option<(Vec<Road>, Vec<Vec<[f64; 2]>>)> {
    let half = MAP_SPAN * 0.8;
    let dlon = half / lat.to_radians().cos().max(0.2);
    let t0 = Instant::now();
    match vector_tiles(lat, lon, half, dlon).await {
        Some(data) if !data.0.is_empty() => {
            info!("[BOARD] Street data from OpenFreeMap tiles: {} roads, {} water ways in {} ms", data.0.len(), data.1.len(), t0.elapsed().as_millis());
            return Some(data);
        }
        _ => warn!("[BOARD] OpenFreeMap tiles gave no street data; trying Overpass"),
    }
    let bbox = format!("{:.6},{:.6},{:.6},{:.6}", lat - half, lon - dlon, lat + half, lon + dlon);
    let query = format!(
        "[out:json][timeout:20];(way[\"highway\"~\"^(motorway|trunk|primary|secondary|tertiary|unclassified|residential|living_street|service|motorway_link|trunk_link|primary_link)$\"]({bbox});way[\"natural\"=\"water\"]({bbox});way[\"waterway\"~\"^(river|stream|canal)$\"]({bbox}););out geom;"
    );
    for url in OVERPASS {
        let t0 = Instant::now();
        let resp = HTTP.post(url).header("User-Agent", API_AGENT).form(&[("data", query.as_str())]).timeout(Duration::from_secs(22)).send().await;
        let result = match resp {
            Ok(r) if r.status().is_success() => r.json::<Value>().await.map_err(|e| e.to_string()),
            Ok(r) => Err(format!("HTTP {}", r.status())),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(v) => {
                let data = parse_overpass(&v);
                info!("[BOARD] Street data from {url}: {} roads, {} water ways in {} ms", data.0.len(), data.1.len(), t0.elapsed().as_millis());
                return Some(data);
            }
            Err(e) => warn!("[BOARD] Overpass {url} failed: {e}"),
        }
    }
    None
}

/// Zoom of the vector tiles (a tile is ≈1.5 km wide at 51° N).
const TILE_ZOOM: u32 = 14;

/// The current tile URL template of OpenFreeMap (its data version changes weekly).
async fn tile_template() -> Option<String> {
    static TEMPLATE: LazyLock<tokio::sync::OnceCell<String>> = LazyLock::new(tokio::sync::OnceCell::new);
    TEMPLATE
        .get_or_try_init(|| async {
            let v: Value = HTTP.get("https://tiles.openfreemap.org/planet").header("User-Agent", API_AGENT).send().await.map_err(|_| ())?.json().await.map_err(|_| ())?;
            v["tiles"][0].as_str().map(str::to_string).ok_or(())
        })
        .await
        .ok()
        .cloned()
}

/// Web-Mercator tile coordinates (fractional) of a point.
pub(crate) fn tile_xy(lat: f64, lon: f64, z: u32) -> (f64, f64) {
    let n = f64::from(1u32 << z);
    let x = (lon + 180.0) / 360.0 * n;
    let r = lat.to_radians();
    let y = (1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

/// [lat, lon] of a fractional tile coordinate.
pub(crate) fn tile_point(x: f64, y: f64, z: u32) -> [f64; 2] {
    let n = f64::from(1u32 << z);
    let lon = x / n * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan().to_degrees();
    [lat, lon]
}

/// Roads and water of every zoom-14 tile the box touches.
async fn vector_tiles(lat: f64, lon: f64, half: f64, dlon: f64) -> Option<(Vec<Road>, Vec<Vec<[f64; 2]>>)> {
    let template = tile_template().await?;
    let (x0, y0) = tile_xy(lat + half, lon - dlon, TILE_ZOOM);
    let (x1, y1) = tile_xy(lat - half, lon + dlon, TILE_ZOOM);
    let tiles: Vec<(u32, u32)> = (x0.floor() as u32..=x1.floor() as u32).flat_map(|x| (y0.floor() as u32..=y1.floor() as u32).map(move |y| (x, y))).take(9).collect();
    let fetches = tiles.iter().map(|(x, y)| {
        let url = template.replace("{z}", &TILE_ZOOM.to_string()).replace("{x}", &x.to_string()).replace("{y}", &y.to_string());
        async move {
            let resp = HTTP.get(url).header("User-Agent", API_AGENT).header("Accept-Encoding", "identity").send().await.ok()?;
            if !resp.status().is_success() {
                return None;
            }
            resp.bytes().await.ok().map(|b| b.to_vec())
        }
    });
    let bodies = futures_util::future::join_all(fetches).await;
    let (mut roads, mut water) = (Vec::new(), Vec::new());
    let mut any = false;
    for ((x, y), body) in tiles.iter().zip(bodies) {
        let Some(body) = body else { continue };
        any = true;
        let (r, w) = decode_tile(&body, *x, *y, TILE_ZOOM);
        roads.extend(r);
        water.extend(w);
    }
    roads.sort_by_key(|r| std::cmp::Reverse(r.class));
    any.then_some((roads, water))
}

/// A Mapbox Vector Tile reader: just enough protobuf for layers, features, tags and geometry.
struct Pbf<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Pbf<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.pos)?;
            self.pos += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// (field number, wire type) of the next field.
    fn key(&mut self) -> Option<(u64, u64)> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let k = self.varint()?;
        Some((k >> 3, k & 7))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.varint()? as usize;
        let out = self.buf.get(self.pos..self.pos + len)?;
        self.pos += len;
        Some(out)
    }

    fn skip(&mut self, wire: u64) -> Option<()> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => (self.pos + 8 <= self.buf.len()).then(|| self.pos += 8),
            2 => self.bytes().map(|_| ()),
            5 => (self.pos + 4 <= self.buf.len()).then(|| self.pos += 4),
            _ => None,
        }
    }

    fn packed(buf: &'a [u8]) -> Vec<u64> {
        let mut p = Pbf::new(buf);
        std::iter::from_fn(|| (p.pos < p.buf.len()).then(|| p.varint()).flatten()).collect()
    }
}

fn zigzag(n: u64) -> i64 {
    (n >> 1) as i64 ^ -((n & 1) as i64)
}

/// Geometry commands → parts in tile units (MoveTo starts a part, ClosePath closes a ring).
pub(crate) fn decode_geometry(cmds: &[u64]) -> Vec<Vec<(i64, i64)>> {
    let mut parts: Vec<Vec<(i64, i64)>> = Vec::new();
    let mut cur: Vec<(i64, i64)> = Vec::new();
    let (mut x, mut y) = (0i64, 0i64);
    let mut i = 0;
    while i < cmds.len() {
        let (cmd, count) = (cmds[i] & 7, (cmds[i] >> 3) as usize);
        i += 1;
        match cmd {
            1 | 2 => {
                for _ in 0..count {
                    let (Some(dx), Some(dy)) = (cmds.get(i), cmds.get(i + 1)) else { return parts };
                    i += 2;
                    x += zigzag(*dx);
                    y += zigzag(*dy);
                    if cmd == 1 && !cur.is_empty() {
                        parts.push(std::mem::take(&mut cur));
                    }
                    cur.push((x, y));
                }
            }
            7 => {
                if let Some(first) = cur.first().copied() {
                    cur.push(first);
                }
            }
            _ => break,
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// The roads ("transportation") and water ("water", "waterway") of one tile.
pub(crate) fn decode_tile(tile: &[u8], tx: u32, ty: u32, z: u32) -> (Vec<Road>, Vec<Vec<[f64; 2]>>) {
    let (mut roads, mut water) = (Vec::new(), Vec::new());
    let mut t = Pbf::new(tile);
    while let Some((field, wire)) = t.key() {
        if field != 3 || wire != 2 {
            if t.skip(wire).is_none() {
                break;
            }
            continue;
        }
        let Some(layer) = t.bytes() else { break };
        let mut l = Pbf::new(layer);
        let (mut name, mut extent) = ("", 4096u64);
        let (mut keys, mut values, mut features): (Vec<&str>, Vec<String>, Vec<&[u8]>) = (Vec::new(), Vec::new(), Vec::new());
        while let Some((f, w)) = l.key() {
            match (f, w) {
                (1, 2) => name = l.bytes().and_then(|b| std::str::from_utf8(b).ok()).unwrap_or(""),
                (2, 2) => features.extend(l.bytes()),
                (3, 2) => keys.push(l.bytes().and_then(|b| std::str::from_utf8(b).ok()).unwrap_or("")),
                (4, 2) => {
                    let mut v = Pbf::new(l.bytes().unwrap_or_default());
                    let mut text = String::new();
                    while let Some((vf, vw)) = v.key() {
                        if vf == 1 && vw == 2 {
                            text = v.bytes().and_then(|b| std::str::from_utf8(b).ok()).unwrap_or("").to_string();
                        } else if v.skip(vw).is_none() {
                            break;
                        }
                    }
                    values.push(text);
                }
                (5, 0) => extent = l.varint().unwrap_or(4096).max(1),
                _ => {
                    if l.skip(w).is_none() {
                        break;
                    }
                }
            }
        }
        if !matches!(name, "transportation" | "water" | "waterway") {
            continue;
        }
        let to_geo = |(gx, gy): (i64, i64)| tile_point(f64::from(tx) + gx as f64 / extent as f64, f64::from(ty) + gy as f64 / extent as f64, z);
        for feature in features {
            let mut f = Pbf::new(feature);
            let (mut tags, mut geometry) = (Vec::new(), Vec::new());
            while let Some((ff, fw)) = f.key() {
                match (ff, fw) {
                    (2, 2) => tags = Pbf::packed(f.bytes().unwrap_or_default()),
                    (4, 2) => geometry = Pbf::packed(f.bytes().unwrap_or_default()),
                    _ => {
                        if f.skip(fw).is_none() {
                            break;
                        }
                    }
                }
            }
            let class = tags.chunks(2).find_map(|kv| {
                let (k, v) = (*kv.first()? as usize, *kv.get(1)? as usize);
                (keys.get(k) == Some(&"class")).then(|| values.get(v).cloned()).flatten()
            });
            let parts = decode_geometry(&geometry);
            if name == "transportation" {
                let rank = match class.as_deref() {
                    Some("motorway" | "trunk") => 0,
                    Some("primary" | "secondary") => 1,
                    Some("tertiary") => 2,
                    Some("minor" | "service" | "living_street") => 3,
                    _ => continue,
                };
                roads.extend(parts.into_iter().filter(|p| p.len() >= 2).map(|p| Road { class: rank, points: p.into_iter().map(to_geo).collect() }));
            } else {
                water.extend(parts.into_iter().filter(|p| p.len() >= 2).map(|p| p.into_iter().map(to_geo).collect()));
            }
        }
    }
    (roads, water)
}

pub(crate) fn parse_overpass(v: &Value) -> (Vec<Road>, Vec<Vec<[f64; 2]>>) {
    let mut roads = Vec::new();
    let mut water = Vec::new();
    for el in v["elements"].as_array().into_iter().flatten() {
        let points: Vec<[f64; 2]> = el["geometry"].as_array().into_iter().flatten().filter_map(|g| Some([g["lat"].as_f64()?, g["lon"].as_f64()?])).collect();
        if points.len() < 2 {
            continue;
        }
        let tags = &el["tags"];
        if let Some(h) = tags["highway"].as_str() {
            let class = match h {
                "motorway" | "trunk" | "motorway_link" | "trunk_link" => 0,
                "primary" | "secondary" | "primary_link" => 1,
                "tertiary" | "unclassified" => 2,
                _ => 3,
            };
            roads.push(Road { class, points });
        } else if tags["natural"] == "water" || tags["waterway"].is_string() {
            water.push(points);
        }
    }
    // Small roads first, so the main roads draw on top.
    roads.sort_by_key(|r| std::cmp::Reverse(r.class));
    (roads, water)
}

/// Photos taken near a point (Wikimedia Commons geosearch), as 800 px thumbnails.
async fn commons_photos(lat: f64, lon: f64) -> Vec<String> {
    let url = format!(
        "https://commons.wikimedia.org/w/api.php?action=query&format=json&generator=geosearch&ggscoord={lat:.6}%7C{lon:.6}&ggsradius=900&ggslimit=24&ggsnamespace=6&prop=imageinfo&iiprop=url%7Cmime&iiurlwidth=800"
    );
    let Ok(resp) = HTTP.get(url).header("User-Agent", API_AGENT).send().await else { return Vec::new() };
    let Ok(v) = resp.json::<Value>().await else { return Vec::new() };
    let mut out: Vec<String> = v["query"]["pages"]
        .as_object()
        .into_iter()
        .flat_map(|pages| pages.values())
        .filter_map(|p| {
            let info = p["imageinfo"].get(0)?;
            let mime = info["mime"].as_str().unwrap_or("");
            (mime == "image/jpeg" || mime == "image/png").then(|| info["thumburl"].as_str().or(info["url"].as_str()).map(str::to_string)).flatten()
        })
        .collect();
    out.truncate(12);
    out
}

/// A web image decoded and scaled to at most 960 px. Three downloads at a time; a 429 / 503
/// (Wikimedia rate-limits bursts) is retried after its `Retry-After`.
pub async fn fetch_image(url: &str) -> Option<eframe::egui::ColorImage> {
    static SLOTS: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(3));
    let _slot = SLOTS.acquire().await.ok()?;
    for attempt in 0..3u64 {
        let resp = HTTP.get(url).header("User-Agent", API_AGENT).header("Accept", "image/avif,image/webp,image/png,image/jpeg,*/*").send().await.ok()?;
        let status = resp.status().as_u16();
        if status == 429 || status == 503 {
            let wait = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(1 + attempt * 2).min(8);
            tokio::time::sleep(Duration::from_secs(wait)).await;
            continue;
        }
        if !resp.status().is_success() || resp.content_length().is_some_and(|n| n > 15 * 1024 * 1024) {
            return None;
        }
        let bytes = resp.bytes().await.ok()?;
        return tokio::task::spawn_blocking(move || decode_image(&bytes)).await.ok()?;
    }
    None
}

pub(crate) fn decode_image(bytes: &[u8]) -> Option<eframe::egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width().max(img.height()) > 960 { img.resize(960, 960, image::imageops::FilterType::Triangle) } else { img };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(eframe::egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn youtube_results_give_ids_and_titles() {
        let html = r#"…"videoRenderer":{"videoId":"dQw4w9WgXcQ","thumbnail":{},"title":{"runs":[{"text":"Quest 3 \u0026 Vision Pro: \"hands-on\""}]},"lengthText":{"accessibility":{"accessibilityData":{"label":"12 minutes, 34 seconds"}},"simpleText":"12:34"}}…"videoRenderer":{"videoId":"dQw4w9WgXcQ","title":{"runs":[{"text":"dup"}]}}…"videoRenderer":{"videoId":"abcdefghijk","title":{"runs":[{"text":"Second"}]}}…"videoRenderer":{"videoId":"zzzzzzzzzzz","title":{"runs":[{"text":"Third"}]}}"#;
        let videos = parse_youtube_results(html, 2);
        assert_eq!(videos, vec![("dQw4w9WgXcQ".to_string(), "Quest 3 & Vision Pro: \"hands-on\" · 12:34".to_string()), ("abcdefghijk".to_string(), "Second".to_string())]);
        assert_eq!(crate::hud_board::video_length(&videos[0].1), ("Quest 3 & Vision Pro: \"hands-on\"", Some("12:34")));
        assert_eq!(crate::hud_board::video_length("Second"), ("Second", None));
        assert_eq!(crate::hud_board::video_length("Rock · Paper"), ("Rock · Paper", None), "only a running time is taken off");
        assert_eq!(crate::hud_board::video_length("F1 Highlights · 2024"), ("F1 Highlights · 2024", None), "a year is no running time");
        assert_eq!(crate::hud_board::video_length("Live · 1:02:03"), ("Live", Some("1:02:03")));
        assert!(parse_youtube_results("<html>no results</html>", 2).is_empty());
    }

    use super::*;

    fn fact(claim: &str, category: &str) -> Fact {
        Fact { claim: claim.into(), category: category.into(), score: 1.0, domain: "example.com".into() }
    }

    #[test]
    fn heuristics_build_charts_figures_and_stacks_from_facts() {
        let facts = vec![
            fact("On Steam, Quest 2 holds 38.2% of headsets, Quest 3 has 24.1% and Index sits at 11%.", "statistic"),
            fact("Pico 4 accounts for 2.4% of headsets in the latest hardware survey.", "statistic"),
            fact("Around 20 million Quest headsets have been sold since 2019 according to analysts.", "statistic"),
            fact("Virtual reality is a simulated experience that uses head-mounted displays.", "definition"),
            fact("Inside-out tracking uses cameras on the headset instead of external base stations.", "architecture"),
            fact("However, motion sickness remains a problem for a significant share of users.", "limitation"),
        ];
        let blocks = compose_heuristic(&facts);
        let chart = blocks.iter().find_map(|b| if let BlockKind::BarChart { bars, .. } = &b.kind { Some(bars.clone()) } else { None }).expect("a chart");
        assert!(chart.iter().any(|b| b.label == "Quest 2" && (b.value - 38.2).abs() < 1e-4), "{chart:?}");
        assert!(chart.len() >= 3);
        let figures: Vec<String> = blocks.iter().filter_map(|b| if let BlockKind::HeroFigure { figure, .. } = &b.kind { Some(figure.clone()) } else { None }).collect();
        assert!(figures.contains(&"20M".to_string()), "{figures:?}");
        assert!(blocks.iter().any(|b| b.label.contains("OVERVIEW")));
        assert!(blocks.iter().any(|b| b.label.contains("LIMITS & RISKS")));
    }

    #[test]
    fn note_labels_are_numbered_and_merging_keeps_one_chart() {
        let chart = || Block::new("CHART // A", "", "", BlockKind::BarChart { unit: "%".into(), bars: vec![], caption: String::new() });
        let note = |s: &str| Block::new(format!("NOTE // — {s}"), "FACT", "", BlockKind::NoteStack { notes: vec![Note { headline: "h".into(), body: "b".into() }] });
        let mut merged = merge(vec![chart(), note("OVERVIEW")], vec![chart(), note("TIMELINE")]);
        assert_eq!(merged.iter().filter(|b| matches!(b.kind, BlockKind::BarChart { .. })).count(), 1);
        number_notes(&mut merged);
        assert_eq!(merged[1].label, "NOTE // 01 — OVERVIEW");
        assert_eq!(merged[2].label, "NOTE // 02 — TIMELINE");
    }

    #[test]
    fn share_images_skip_logos_and_resolve_relative_urls() {
        let base = Url::parse("https://www.example.com/news/vr").unwrap();
        let html = r#"<head><meta property="og:image" content="/img/headset.jpg"></head>"#;
        assert_eq!(og_image(html, &base).as_deref(), Some("https://www.example.com/img/headset.jpg"));
        let logo = r#"<meta content="https://cdn.example.com/logo.png" property="og:image">"#;
        assert_eq!(og_image(logo, &base), None);
    }

    #[test]
    fn overpass_ways_become_roads_and_water() {
        let v = json!({"elements": [
            {"type": "way", "tags": {"highway": "residential"}, "geometry": [{"lat": 51.0, "lon": 0.1}, {"lat": 51.001, "lon": 0.101}]},
            {"type": "way", "tags": {"highway": "primary"}, "geometry": [{"lat": 51.0, "lon": 0.1}, {"lat": 51.002, "lon": 0.1}]},
            {"type": "way", "tags": {"waterway": "river"}, "geometry": [{"lat": 51.0, "lon": 0.2}, {"lat": 51.01, "lon": 0.2}]},
            {"type": "way", "tags": {"highway": "path"}, "geometry": [{"lat": 51.0, "lon": 0.1}]}
        ]});
        let (roads, water) = parse_overpass(&v);
        assert_eq!(roads.len(), 2);
        assert_eq!(roads.last().unwrap().class, 1, "main roads draw last");
        assert_eq!(water.len(), 1);
    }

    /// Protobuf helpers for hand-built test tiles.
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn field_bytes(field: u64, bytes: &[u8], out: &mut Vec<u8>) {
        varint(field << 3 | 2, out);
        varint(bytes.len() as u64, out);
        out.extend_from_slice(bytes);
    }

    fn packed(values: &[u64]) -> Vec<u8> {
        let mut out = Vec::new();
        values.iter().for_each(|v| varint(*v, &mut out));
        out
    }

    fn zz(n: i64) -> u64 {
        ((n << 1) ^ (n >> 63)) as u64
    }

    #[test]
    fn vector_tiles_decode_roads_by_class_and_water() {
        // One "transportation" layer: a primary road from (0,0) to (4096,4096), and a path that
        // is skipped; one "water" layer with a closed triangle.
        let road = |class_value: u64, geometry: &[u64]| {
            let mut f = Vec::new();
            field_bytes(2, &packed(&[0, class_value]), &mut f);
            varint(3 << 3, &mut f);
            varint(2, &mut f);
            field_bytes(4, &packed(geometry), &mut f);
            f
        };
        let line = [9, zz(0), zz(0), 10, zz(4096), zz(4096)];
        let mut layer = Vec::new();
        field_bytes(1, b"transportation", &mut layer);
        field_bytes(2, &road(0, &line), &mut layer);
        field_bytes(2, &road(1, &line), &mut layer);
        field_bytes(3, b"class", &mut layer);
        let mut v = Vec::new();
        field_bytes(1, b"primary", &mut v);
        field_bytes(4, &v, &mut layer);
        let mut v = Vec::new();
        field_bytes(1, b"path", &mut v);
        field_bytes(4, &v, &mut layer);
        varint(5 << 3, &mut layer);
        varint(4096, &mut layer);
        let mut water = Vec::new();
        field_bytes(1, b"water", &mut water);
        let mut f = Vec::new();
        field_bytes(4, &packed(&[9, zz(0), zz(0), 18, zz(100), zz(0), zz(0), zz(100), 15]), &mut f);
        field_bytes(2, &f, &mut water);
        let mut tile = Vec::new();
        field_bytes(3, &layer, &mut tile);
        field_bytes(3, &water, &mut tile);
        let (roads, lakes) = decode_tile(&tile, 8187, 5478, 14);
        assert_eq!(roads.len(), 1, "the path is not drawn");
        assert_eq!(roads[0].class, 1);
        let start = tile_point(8187.0, 5478.0, 14);
        let end = tile_point(8188.0, 5479.0, 14);
        assert!((roads[0].points[0][0] - start[0]).abs() < 1e-9 && (roads[0].points[1][1] - end[1]).abs() < 1e-9);
        assert_eq!(lakes.len(), 1);
        assert_eq!(lakes[0].len(), 4, "the ring is closed");
        assert_eq!(lakes[0].first(), lakes[0].last());
    }

    #[test]
    fn tile_coordinates_round_trip() {
        let (x, y) = tile_xy(51.1077, -0.1093, 14);
        assert_eq!((x.floor() as u32, y.floor() as u32), (8187, 5477));
        let back = tile_point(x, y, 14);
        assert!((back[0] - 51.1077).abs() < 1e-9 && (back[1] + 0.1093).abs() < 1e-9);
    }

    #[test]
    fn png_bytes_decode_to_a_texture_image() {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([255, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let decoded = decode_image(&png).unwrap();
        assert_eq!(decoded.size, [4, 3]);
        assert_eq!(decoded.pixels[0], eframe::egui::Color32::from_rgb(255, 0, 0));
    }
}
