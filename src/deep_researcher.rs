//! The Deep Researcher: "do some deep research on solid-state batteries." Runs in the background:
//! plans sub-questions, searches the web for each, reads the best pages, takes notes, and writes a
//! Stark Research Dossier (summary, findings, details, open questions, sources) — saved to
//! Documents\Jarvis_Research and opened in its own window, with a spoken heads-up when done.

use crate::features::{safe_name, show_window, spoken, str_arg, user_folder};
use crate::stark::StarkEvent;
use crate::tools::ToolContext;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;
use tracing::{info, warn};

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s)\]"'<>]+"#).unwrap());

/// (sub-queries, pages to read) per depth.
fn budget(depth: &str) -> (usize, usize) {
    match depth {
        "quick" => (2, 2),
        "deep" | "thorough" => (5, 7),
        _ => (3, 4),
    }
}

/// Distinct result links, skipping search engines, video and social sites.
pub fn result_links(results: &str, limit: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    LINK.find_iter(results)
        .map(|m| m.as_str().trim_end_matches(['.', ',', ';']).to_string())
        .filter(|u| !["duckduckgo.", "google.", "bing.com", "youtube.", "facebook.", "instagram.", "tiktok.", "pinterest.", "x.com", "twitter."].iter().any(|b| u.contains(b)))
        .filter(|u| seen.insert(u.split('#').next().unwrap_or(u).to_string()))
        .take(limit)
        .collect()
}

async fn research(tools: ToolContext, topic: String, depth: String) {
    let (n_queries, n_pages) = budget(&depth);
    let events = tools.stark.events.clone();
    let notice = |m: String| {
        let _ = events.send(StarkEvent::Notice(m));
    };
    // 1. Sub-questions.
    let plan = tools
        .pim
        .complete(&format!("Break the research topic into {n_queries} distinct web search queries that together cover it (background, current state, numbers, criticism, outlook). One per line, no numbering, no quotes."), &topic, 160)
        .await
        .unwrap_or_default();
    let mut queries: Vec<String> = plan.lines().map(|l| l.trim().trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '-' | '*' | ')')).trim().to_string()).filter(|l| l.len() > 3).take(n_queries).collect();
    if queries.is_empty() {
        queries.push(topic.clone());
    }
    notice(format!("Research on {topic}: searching {} angles…", queries.len()));
    // 2. Search.
    let mut links = Vec::new();
    let mut snippets = String::new();
    for q in &queries {
        match crate::websearch::search(q).await {
            Ok(results) => {
                snippets.push_str(&format!("\n## {q}\n{}\n", results.chars().take(2500).collect::<String>()));
                for l in result_links(&results, 3) {
                    if !links.contains(&l) {
                        links.push(l);
                    }
                }
            }
            Err(e) => warn!("[RESEARCH] search '{q}' failed: {e}"),
        }
    }
    tools.stark.pulse(&["research", "brain"], false);
    // 3. Read and take notes.
    let mut notes = Vec::new();
    let mut sources = Vec::new();
    for url in links.iter().take(n_pages) {
        let Ok(page) = crate::websearch::read_webpage(url).await else { continue };
        let note = tools
            .pim
            .complete(&format!("Take dense research notes on '{topic}' from this page: key facts, numbers with units, dates, claims and who makes them. Bullet points, max 12. If the page is irrelevant reply IRRELEVANT."), &page, 420)
            .await
            .unwrap_or_default();
        if !note.trim().is_empty() && !note.contains("IRRELEVANT") {
            sources.push(url.clone());
            notes.push(format!("### Source [{}] {url}\n{note}", sources.len()));
        }
    }
    if notes.is_empty() && snippets.trim().is_empty() {
        let _ = events.send(StarkEvent::Alert(format!("I'm afraid the research on {topic} found nothing usable, Sir.")));
        return;
    }
    notice(format!("Research on {topic}: writing the dossier from {} sources…", sources.len()));
    // 4. The dossier.
    let material = if notes.is_empty() { snippets.clone() } else { notes.join("\n\n") };
    let dossier = tools
        .pim
        .complete(
            "Write a Stark Research Dossier in Markdown from the notes: '## Executive Summary' (4-6 sentences), '## Key Findings' (bullets with numbers), '## Details' (short sections), '## Risks & Open Questions', '## Bottom Line' (one paragraph). Cite sources as [n] matching the notes. English, precise, no filler.",
            &format!("Topic: {topic}\n\n{material}"),
            1400,
        )
        .await
        .unwrap_or_else(|| format!("## Raw notes\n{material}"));
    let md = format!(
        "# Stark Research Dossier\n**Topic:** {topic}  \n**Compiled:** {}  ·  depth {depth}  ·  {} sources\n\n{dossier}\n\n## Sources\n{}",
        chrono::Local::now().format("%d.%m.%Y %H:%M"),
        sources.len(),
        sources.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect::<Vec<_>>().join("\n")
    );
    let dir = user_folder("Documents").join("Jarvis_Research");
    let file = dir.join(format!("{} {}.md", chrono::Local::now().format("%Y-%m-%d"), safe_name(&topic)));
    let saved = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, &md)).is_ok();
    show_window(&tools, "research_dossier", &format!("Stark Research Dossier — {}", topic.chars().take(40).collect::<String>()), &md, 760.0, 720.0);
    info!("[RESEARCH] Dossier on {topic}: {} sources{}", sources.len(), if saved { format!(", saved to {}", file.display()) } else { String::new() });
    let summary = dossier.lines().skip_while(|l| !l.to_lowercase().contains("summary")).nth(1).unwrap_or("").chars().take(220).collect::<String>();
    let _ = events.send(StarkEvent::Alert(format!("Your research dossier on {topic} is ready, Sir. {summary}")));
}

pub fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let topic = str_arg(args, &["topic", "query", "subject"]).ok_or("What shall I research, Sir?")?.to_string();
    let depth = str_arg(args, &["depth"]).unwrap_or("standard").to_lowercase();
    let (q, p) = budget(&depth);
    tokio::spawn(research(tools.clone(), topic.clone(), depth.clone()));
    Ok(spoken(
        format!("Background research started: \"{topic}\" (depth {depth}: {q} searches, up to {p} pages). The dossier opens in its own window and is saved to Documents\\Jarvis_Research."),
        format!("I'm on it, Sir. I'll research {topic} in the background and let you know when the dossier is ready."),
    ))
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "start_background_research",
        "description": "Deep Researcher: research a topic in the background (several web searches, reads the best pages, takes notes) and deliver a Stark Research Dossier window + Markdown file when done. depth quick, standard or deep.",
        "parameters": {"type": "object", "properties": {
            "topic": {"type": "string"},
            "depth": {"type": "string", "enum": ["quick", "standard", "deep"]}
        }, "required": ["topic"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_distinct_and_useful() {
        let results = "1. Solid-state batteries — https://en.wikipedia.org/wiki/Solid-state_battery\n2. https://www.youtube.com/watch?v=x\n3. (https://www.nature.com/articles/abc). 4. https://en.wikipedia.org/wiki/Solid-state_battery#History";
        assert_eq!(result_links(results, 5), vec!["https://en.wikipedia.org/wiki/Solid-state_battery", "https://www.nature.com/articles/abc"]);
        assert_eq!(budget("deep"), (5, 7));
    }
}
