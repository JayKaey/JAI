//! The contextual "teleport" pipeline: content from one place to another in one step — "take my
//! clipboard and send it to Max on WhatsApp", "send my schedule to Lena", "put that in a window",
//! "copy my location", "save that to a file". Sources and destinations are the existing tools,
//! chained here, so every step shows in the tool log and lights the core.

use crate::context_graph::ArtifactKind;
use crate::tools::{bool_arg, str_arg, ToolContext};
use chrono::Local;
use serde_json::{json, Value};

pub fn is_tool(name: &str) -> bool {
    name == "teleport_content"
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "teleport_content",
        "description": "Move content between tools in one step: source clipboard, schedule (today's calendar + timetable), school_timetable, last_reply / that (what was just discussed), location, weather, file (the file just discussed) or text; destination whatsapp (needs recipient), email (opens a draft), clipboard, window, widget, file, or phone (the mobile satellite).",
        "parameters": {"type": "object", "properties": {
            "source_type": {"type": "string", "enum": ["clipboard", "schedule", "school_timetable", "last_reply", "that", "location", "weather", "file", "text"]},
            "destination_type": {"type": "string", "enum": ["whatsapp", "email", "clipboard", "window", "widget", "file", "phone"]},
            "recipient": {"type": "string", "description": "Person for whatsapp / email (\"him\" = the person just discussed)"},
            "content": {"type": "string", "description": "The text itself for source text"},
            "day": {"type": "string", "description": "Day for schedule sources (default today)"},
            "subject": {"type": "string", "description": "E-mail subject (optional)"},
            "send_immediately": {"type": "boolean"}
        }, "required": ["source_type", "destination_type"]}
    }})
}

/// The content of a source, with a short label.
async fn fetch(tools: &ToolContext, source: &str, args: &Value) -> Result<(String, String), String> {
    let today = Local::now().date_naive();
    match source {
        "clipboard" | "zwischenablage" => {
            let text = crate::clipboard::read().await?;
            if text.trim().is_empty() {
                return Err("Your clipboard is empty, Sir.".into());
            }
            Ok(("your clipboard".into(), text))
        }
        "schedule" | "calendar" | "my_schedule" | "today" | "agenda" => {
            let day = crate::pim::parse_day(str_arg(args, &["day", "date"]).unwrap_or("today"), today).unwrap_or(today);
            let (events, _) = tools.pim.schedule(day, day).await;
            let mut lines: Vec<String> = events.iter().map(|e| if e.all_day { format!("• {} (all day)", e.title) } else { format!("• {} {}", e.start.format("%H:%M"), e.title) }).collect();
            if let Ok(lessons) = tools.pim.lessons(day, day).await {
                let text = crate::webuntis_client::schedule_text(&lessons, day, day, false);
                if !text.trim().is_empty() {
                    lines.push(format!("School:\n{text}"));
                }
            }
            if lines.is_empty() {
                lines.push("Nothing scheduled.".into());
            }
            Ok((format!("your schedule for {}", crate::pim::spoken_day(day, today)), format!("Schedule {}:\n{}", day.format("%a %d.%m."), lines.join("\n"))))
        }
        "school_timetable" | "timetable" | "webuntis" => {
            let day = crate::pim::parse_day(str_arg(args, &["day", "date"]).unwrap_or("today"), today).unwrap_or(today);
            let lessons = tools.pim.lessons(day, day).await?;
            Ok(("your timetable".into(), format!("Timetable {}:\n{}", day.format("%a %d.%m."), crate::webuntis_client::schedule_text(&lessons, day, day, false))))
        }
        "location" | "my_location" | "position" => {
            let fix = tools.stark.location.current(tools).await?;
            let mut text = format!("My location: https://maps.google.com/?q={:.6},{:.6}", fix.lat, fix.lon);
            if let Some(a) = &fix.address {
                text.push_str(&format!(" ({a})"));
            }
            Ok(("your location".into(), text))
        }
        "weather" => {
            let report = crate::location_engine::weather_report(tools, 6, None).await?;
            Ok(("the weather".into(), report.short))
        }
        "file" => {
            let path = tools.focus.last_file().ok_or("I haven't found or opened a file in this conversation yet.")?;
            Ok((path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), path.display().to_string()))
        }
        "text" => {
            let text = str_arg(args, &["content", "text"]).ok_or("What text should I send?")?;
            Ok(("that".into(), text.to_string()))
        }
        _ => {
            // "that", "it", "last_reply": what the conversation is about.
            let g = tools.stark.graph.snapshot();
            if let Some(a) = g.active_artifact {
                let label = match a.kind {
                    ArtifactKind::WhatsAppMessage => "that message".to_string(),
                    ArtifactKind::Email => "that e-mail".to_string(),
                    _ => "that".to_string(),
                };
                return Ok((label, a.shareable()));
            }
            let last = tools.memory.recent_turns(4).into_iter().rev().find(|(_, role, _)| role == "assistant").map(|(_, _, text)| text);
            last.map(|t| ("my last answer".to_string(), t)).ok_or_else(|| "There is nothing in this conversation to send yet.".to_string())
        }
    }
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let source = str_arg(args, &["source_type", "source", "from"]).unwrap_or("that").to_lowercase();
    let destination = str_arg(args, &["destination_type", "destination", "to"]).ok_or("Where should it go?")?.to_lowercase();
    tools.stark.pulse(&["brain", "context", "windows"], false);
    let (label, content) = fetch(tools, &source, args).await?;
    let recipient = str_arg(args, &["recipient", "person", "contact"]).map(str::to_string);
    let now = bool_arg(args, "send_immediately").unwrap_or(false);
    let run = |name: &'static str, a: Value| async move { Box::pin(tools.execute(name, &a)).await };
    let outcome = match destination.as_str() {
        "whatsapp" | "wa" => {
            let who = recipient.ok_or("Who should get it on WhatsApp?")?;
            run("send_whatsapp_message", json!({ "recipient_name_or_phone": who, "message_text": content, "send_immediately": now })).await
        }
        "email" | "e-mail" | "mail" => {
            let who = recipient.ok_or("Who should get the e-mail?")?;
            // An address itself (a reply to the sender), or a contact's.
            let (name, address) = if who.contains('@') {
                (who.clone(), who.clone())
            } else {
                let contact = tools.stark.contacts.resolve(&who)?;
                if contact.email.is_empty() {
                    return Err(format!("I don't have an e-mail address for {}, Sir.", contact.name));
                }
                (contact.name, contact.email)
            };
            let subject = str_arg(args, &["subject"]).map(str::to_string).unwrap_or_else(|| format!("From JAI: {label}"));
            let url = format!("mailto:{address}?subject={}&body={}", urlencoding::encode(&subject), urlencoding::encode(&content));
            crate::apps::open(&url, &[]).await?;
            return Ok(format!("Opened an e-mail draft to {name} with {label}; it is ready to send in your mail app."));
        }
        "clipboard" => run("clipboard", json!({ "action": "copy", "text": content })).await,
        "window" | "native_window" => run("spawn_native_window", json!({ "title": crate::pim::capitalise(&label), "markdown": content })).await,
        "widget" | "note" => run("create_or_update_widget", json!({ "id": format!("teleport_{}", Local::now().format("%H%M%S")), "type": "MarkdownNoteCard", "title": crate::pim::capitalise(&label), "markdown": content })).await,
        "file" => {
            let dir = crate::apps::home_dir().ok_or("I can't find your home folder.")?.join("Documents").join("JAI");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let path = dir.join(format!("teleport_{}.txt", Local::now().format("%Y%m%d_%H%M%S")));
            std::fs::write(&path, &content).map_err(|e| e.to_string())?;
            tools.focus.note_file(&path);
            return Ok(format!("Saved {label} to {}.", path.display()));
        }
        "phone" | "satellite" | "ipad" | "mobile" => {
            tools.stark.push_to_satellite(&format!("{}: {content}", crate::pim::capitalise(&label)));
            return Ok(format!("Sent {label} to your phone's satellite HUD."));
        }
        other => return Err(format!("I can't teleport to '{other}' (whatsapp, email, clipboard, window, widget, file, phone).")),
    };
    if outcome.success {
        Ok(format!("Teleported {label} → {destination}: {}", outcome.output))
    } else {
        Err(outcome.output.trim_start_matches("Error: ").to_string())
    }
}
