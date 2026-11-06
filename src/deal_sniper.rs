//! The Deal Sniper: "find me an RTX 3080 under 350 euros." Watches Kleinanzeigen and Geizhals in
//! the background, filters out broken goods ("defekt", "Bastler", "für Teile"), and alerts Sir the
//! moment a new offer at or below his price appears.

use crate::features::{num_arg, show_window, spoken, str_arg};
use crate::stark::StarkEvent;
use crate::tools::ToolContext;
use regex::Regex;
use rusqlite::{params, Connection};
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tracing::{info, warn};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// Words that mark broken or parts-only listings.
const BROKEN: &[&str] = &["defekt", "defect", "broken", "bastler", "für teile", "fuer teile", "ersatzteil", "not working", "kaputt", "ohne funktion", "for parts", "gesucht", "suche "];

#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub id: String,
    pub title: String,
    pub price: f64,
    pub url: String,
    pub platform: &'static str,
    pub place: String,
}

#[derive(Debug, Clone)]
pub struct Sniper {
    pub id: i64,
    pub query: String,
    pub max_price: f64,
    pub platform: String,
}

#[derive(Clone)]
pub struct DealSniper {
    conn: Arc<Mutex<Connection>>,
}

static PRICE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d{1,3}(?:\.\d{3})*|\d+)(?:,(\d{1,2}))?\s*€|€\s*(\d{1,3}(?:\.\d{3})*|\d+)(?:,(\d{1,2}))?").unwrap());

/// "1.234,50 € VB" → 1234.5.
pub fn parse_price(text: &str) -> Option<f64> {
    let c = PRICE.captures(text)?;
    let whole = c.get(1).or_else(|| c.get(3))?.as_str().replace('.', "");
    let cents = c.get(2).or_else(|| c.get(4)).map(|m| m.as_str()).unwrap_or("0");
    format!("{whole}.{cents}").parse().ok()
}

pub fn is_broken(text: &str) -> bool {
    let t = text.to_lowercase();
    BROKEN.iter().any(|w| t.contains(w))
}

fn sel(css: &str) -> Selector {
    Selector::parse(css).expect("valid selector")
}

fn text_of(el: scraper::ElementRef) -> String {
    el.text().collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Kleinanzeigen search results (`article.aditem`).
pub fn parse_kleinanzeigen(html: &str) -> Vec<Offer> {
    let doc = Html::parse_document(html);
    let (item, title, price, desc, place) = (sel("article.aditem"), sel("a.ellipsis, h2 a"), sel("[class*='price']"), sel("[class*='description']"), sel("[class*='top--left']"));
    doc.select(&item)
        .filter_map(|a| {
            let link = a.select(&title).next()?;
            let name = text_of(link);
            let price = a.select(&price).next().map(text_of).and_then(|p| parse_price(&p))?;
            let description = a.select(&desc).next().map(text_of).unwrap_or_default();
            if is_broken(&format!("{name} {description}")) {
                return None;
            }
            let href = link.value().attr("href").unwrap_or("");
            Some(Offer {
                id: a.value().attr("data-adid").map(String::from).unwrap_or_else(|| href.to_string()),
                title: name,
                price,
                url: if href.starts_with("http") { href.to_string() } else { format!("https://www.kleinanzeigen.de{href}") },
                platform: "Kleinanzeigen",
                place: a.select(&place).next().map(text_of).unwrap_or_default(),
            })
        })
        .collect()
}

/// Geizhals product list (cheapest new price per product).
pub fn parse_geizhals(html: &str) -> Vec<Offer> {
    let doc = Html::parse_document(html);
    let item = sel("[class*='productlist__item'], [class*='galleryview__item'], article[class*='product'], div[class*='listview__item']");
    let link = sel("a[href*='-a'], a[class*='name'], a[href]");
    let price = sel("[class*='price']");
    doc.select(&item)
        .filter_map(|el| {
            let a = el.select(&link).find(|a| !text_of(*a).is_empty())?;
            let title = text_of(a);
            let p = el.select(&price).map(text_of).find_map(|t| parse_price(&t))?;
            let href = a.value().attr("href").unwrap_or("");
            Some(Offer {
                id: href.to_string(),
                title,
                price: p,
                url: if href.starts_with("http") { href.to_string() } else { format!("https://geizhals.de/{}", href.trim_start_matches('/')) },
                platform: "Geizhals",
                place: String::new(),
            })
        })
        .filter(|o| !is_broken(&o.title))
        .collect()
}

async fn search(client: &reqwest::Client, query: &str, platform: &str) -> Result<Vec<Offer>, String> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    if platform != "geizhals" {
        let slug: String = query.split_whitespace().collect::<Vec<_>>().join("-");
        let url = format!("https://www.kleinanzeigen.de/s-{}/k0", urlencoding::encode(&slug));
        match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => out.extend(parse_kleinanzeigen(&r.text().await.unwrap_or_default())),
            Ok(r) => errors.push(format!("Kleinanzeigen HTTP {}", r.status().as_u16())),
            Err(e) => errors.push(format!("Kleinanzeigen: {e}")),
        }
    }
    if platform != "kleinanzeigen" {
        let url = format!("https://geizhals.de/?fs={}&hloc=at&hloc=de", urlencoding::encode(query));
        match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => out.extend(parse_geizhals(&r.text().await.unwrap_or_default())),
            Ok(r) => errors.push(format!("Geizhals HTTP {}", r.status().as_u16())),
            Err(e) => errors.push(format!("Geizhals: {e}")),
        }
    }
    // Only listings that are really about the query (every word in the title).
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
    out.retain(|o| {
        let t = o.title.to_lowercase();
        words.iter().all(|w| t.contains(w.as_str()))
    });
    if out.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(out)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().user_agent(UA).timeout(Duration::from_secs(20)).build().unwrap_or_default()
}

impl DealSniper {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().expect("in-memory SQLite")).expect("deal schema")
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS price_snipers (id INTEGER PRIMARY KEY, query TEXT NOT NULL, max_price REAL NOT NULL, platform TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1, created TEXT NOT NULL, last_check TEXT);
             CREATE TABLE IF NOT EXISTS price_seen (sniper INTEGER NOT NULL, offer TEXT NOT NULL, title TEXT, price REAL, url TEXT, seen TEXT NOT NULL, PRIMARY KEY (sniper, offer));",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<Sniper> {
        let db = self.db();
        let Ok(mut st) = db.prepare("SELECT id, query, max_price, platform FROM price_snipers WHERE active = 1 ORDER BY id") else { return Vec::new() };
        st.query_map([], |r| Ok(Sniper { id: r.get(0)?, query: r.get(1)?, max_price: r.get(2)?, platform: r.get(3)? })).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn active_count(&self) -> usize {
        self.list().len()
    }

    fn add(&self, query: &str, max: f64, platform: &str) -> Result<i64, String> {
        let db = self.db();
        db.execute("INSERT INTO price_snipers (query, max_price, platform, created) VALUES (?1, ?2, ?3, ?4)", params![query, max, platform, chrono::Local::now().to_rfc3339()]).map_err(|e| e.to_string())?;
        Ok(db.last_insert_rowid())
    }

    fn remove(&self, target: &str) -> usize {
        let db = self.db();
        let id: Option<i64> = target.parse().ok();
        db.execute("UPDATE price_snipers SET active = 0 WHERE active = 1 AND (id = ?1 OR lower(query) LIKE ?2)", params![id.unwrap_or(-1), format!("%{}%", target.to_lowercase())]).unwrap_or(0)
    }

    /// Offers not seen before (and remembers them).
    fn fresh(&self, sniper: i64, offers: &[Offer]) -> Vec<Offer> {
        let db = self.db();
        let now = chrono::Local::now().to_rfc3339();
        let _ = db.execute("UPDATE price_snipers SET last_check = ?1 WHERE id = ?2", params![now, sniper]);
        offers
            .iter()
            .filter(|o| db.execute("INSERT OR IGNORE INTO price_seen (sniper, offer, title, price, url, seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![sniper, o.id, o.title, o.price, o.url, now]).unwrap_or(0) > 0)
            .cloned()
            .collect()
    }

    /// Checks one sniper; new hits under the limit.
    async fn check(&self, client: &reqwest::Client, s: &Sniper) -> Result<(Vec<Offer>, Vec<Offer>), String> {
        let mut offers = search(client, &s.query, &s.platform).await?;
        offers.sort_by(|a, b| a.price.total_cmp(&b.price));
        let under: Vec<Offer> = offers.iter().filter(|o| o.price <= s.max_price && o.price >= s.max_price * 0.15).cloned().collect();
        let new = self.fresh(s.id, &under);
        Ok((under, new))
    }

    /// The background watch: every `deal_interval_min` minutes.
    pub fn start(&self, tools: ToolContext) {
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(90)).await;
            let client = client();
            loop {
                for s in me.list() {
                    match me.check(&client, &s).await {
                        Ok((_, new)) if !new.is_empty() => {
                            let best = &new[0];
                            let msg = format!(
                                "Deal alert, Sir: {} for {:.0} euros on {}{}.",
                                best.title.chars().take(70).collect::<String>(),
                                best.price,
                                best.platform,
                                if new.len() > 1 { format!(", and {} more under your limit", new.len() - 1) } else { String::new() }
                            );
                            info!("[DEALS] {msg} {}", best.url);
                            tools.stark.push_to_satellite(&format!("{msg}\n{}", best.url));
                            tools.stark.pulse(&["deals", "brain", "voice"], false);
                            let _ = tools.stark.events.send(StarkEvent::Alert(msg));
                        }
                        Ok(_) => {}
                        Err(e) => warn!("[DEALS] '{}' check failed: {e}", s.query),
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                let minutes = tools.config.read(|c| c.features.deal_interval_min).clamp(5, 24 * 60);
                tokio::time::sleep(Duration::from_secs(minutes as u64 * 60)).await;
            }
        });
    }

    /// `manage_price_sniper`.
    pub async fn tool(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let action = str_arg(args, &["action"]).unwrap_or(if str_arg(args, &["search_query", "query"]).is_some() { "add" } else { "list" }).to_lowercase();
        let platform = match str_arg(args, &["platform"]).unwrap_or("both").to_lowercase().as_str() {
            p if p.contains("klein") || p.contains("ebay") => "kleinanzeigen",
            p if p.contains("geiz") => "geizhals",
            _ => "both",
        };
        match action.as_str() {
            "add" | "watch" | "create" | "snipe" => {
                let query = str_arg(args, &["search_query", "query"]).ok_or("What shall I watch for, Sir?")?.to_string();
                let max = num_arg(args, &["max_price_eur", "max_price", "price"]).ok_or("Up to what price, Sir?")?;
                let id = self.add(&query, max, platform)?;
                // A first look right away (these are not alerted later).
                let s = Sniper { id, query: query.clone(), max_price: max, platform: platform.to_string() };
                let now = self.check(&client(), &s).await;
                let current = match &now {
                    Ok((under, _)) if !under.is_empty() => {
                        let md = offers_md(under);
                        show_window(tools, "deal_sniper", "Deal Sniper", &format!("## {query} ≤ {max:.0} €\n{md}"), 640.0, 480.0);
                        format!("{} offers are already under {max:.0} €; the cheapest: {} for {:.0} €.", under.len(), under[0].title.chars().take(60).collect::<String>(), under[0].price)
                    }
                    Ok(_) => format!("Nothing under {max:.0} € right now."),
                    Err(e) => format!("The first check failed ({e}); I'll keep trying."),
                };
                let every = tools.config.read(|c| c.features.deal_interval_min);
                Ok(spoken(
                    format!("Price sniper #{id}: \"{query}\" ≤ {max:.0} € on {platform}, checked every {every} min, broken/parts listings ignored.\n{current}"),
                    format!("Sniper set, Sir: {query} under {max:.0} euros. {current}"),
                ))
            }
            "remove" | "delete" | "stop" | "cancel" => {
                let target = str_arg(args, &["search_query", "query", "id"]).ok_or("Which sniper shall I stop?")?;
                match self.remove(target) {
                    0 => Err(format!("No active sniper matches '{target}'.")),
                    n => Ok(spoken(format!("Stopped {n} sniper(s) for '{target}'."), format!("Stopped watching {target}, Sir."))),
                }
            }
            "check" | "check_now" | "search" => {
                let list = self.list();
                if list.is_empty() {
                    return Err("No price snipers are active.".into());
                }
                let c = client();
                let mut md = String::new();
                let mut best: Option<(String, Offer)> = None;
                for s in &list {
                    match self.check(&c, s).await {
                        Ok((under, _)) => {
                            md.push_str(&format!("## {} ≤ {:.0} €\n{}\n", s.query, s.max_price, if under.is_empty() { "Nothing under the limit.\n".into() } else { offers_md(&under) }));
                            if let Some(o) = under.first() {
                                if best.as_ref().is_none_or(|(_, b)| o.price < b.price) {
                                    best = Some((s.query.clone(), o.clone()));
                                }
                            }
                        }
                        Err(e) => md.push_str(&format!("## {}\nCheck failed: {e}\n", s.query)),
                    }
                }
                show_window(tools, "deal_sniper", "Deal Sniper", &md, 640.0, 520.0);
                let said = match best {
                    Some((q, o)) => format!("Best find for {q}: {} at {:.0} euros on {}.", o.title.chars().take(60).collect::<String>(), o.price, o.platform),
                    None => "Nothing under your limits at the moment, Sir.".into(),
                };
                Ok(spoken(md, said))
            }
            _ => {
                let list = self.list();
                if list.is_empty() {
                    return Ok(spoken("No price snipers are active.", "You have no price snipers running, Sir."));
                }
                let lines: Vec<String> = list.iter().map(|s| format!("#{} {} ≤ {:.0} € ({})", s.id, s.query, s.max_price, s.platform)).collect();
                Ok(spoken(lines.join("\n"), format!("{} snipers are watching: {}.", list.len(), list.iter().map(|s| s.query.as_str()).collect::<Vec<_>>().join(", "))))
            }
        }
    }
}

fn offers_md(offers: &[Offer]) -> String {
    let mut md = String::from("| Price | Offer | Where |\n|---|---|---|\n");
    for o in offers.iter().take(12) {
        md.push_str(&format!("| {:.0} € | [{}]({}) | {} {} |\n", o.price, o.title.replace('|', "/").chars().take(70).collect::<String>(), o.url, o.platform, o.place));
    }
    md
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "manage_price_sniper",
        "description": "Deal Sniper: watch Kleinanzeigen and Geizhals in the background for an item at or below a price (broken / 'Bastler' listings ignored) and alert Sir when one appears. action add (search_query + max_price_eur), list, remove, check_now.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["add", "list", "remove", "check_now"]},
            "search_query": {"type": "string"},
            "max_price_eur": {"type": "number"},
            "platform": {"type": "string", "enum": ["kleinanzeigen", "geizhals", "both"]}
        }, "required": ["action"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_prices_and_filters() {
        assert_eq!(parse_price("1.234,50 € VB"), Some(1234.5));
        assert_eq!(parse_price("€ 299,00"), Some(299.0));
        assert_eq!(parse_price("350 €"), Some(350.0));
        assert!(is_broken("RTX 3080 DEFEKT für Bastler"));
        let html = r#"<article class="aditem" data-adid="42"><div class="aditem-main--top--left">10115 Berlin</div><h2><a class="ellipsis" href="/s-anzeige/rtx-3080/42">Gigabyte RTX 3080 Gaming OC</a></h2><p class="aditem-main--middle--description">Top Zustand</p><p class="aditem-main--middle--price-shipping--price">320 € VB</p></article>
        <article class="aditem" data-adid="43"><h2><a class="ellipsis" href="/x/43">RTX 3080 defekt</a></h2><p class="aditem-main--middle--price-shipping--price">90 €</p></article>"#;
        let offers = parse_kleinanzeigen(html);
        assert_eq!(offers.len(), 1);
        assert_eq!((offers[0].id.as_str(), offers[0].price), ("42", 320.0));
        assert_eq!(offers[0].url, "https://www.kleinanzeigen.de/s-anzeige/rtx-3080/42");
        let s = DealSniper::in_memory();
        let id = s.add("rtx 3080", 350.0, "both").unwrap();
        assert_eq!(s.fresh(id, &offers).len(), 1);
        assert!(s.fresh(id, &offers).is_empty(), "seen offers are not alerted twice");
        assert_eq!(s.remove("3080"), 1);
        assert_eq!(s.active_count(), 0);
    }
}
