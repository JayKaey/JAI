//! The Exam Coach: a spoken oral exam. JAI reads a source (a worksheet, notes, a PDF) or takes a
//! topic, asks one question at a time, grades each spoken answer 0-10 with a short correction, and
//! keeps a Quiz card window up to date. While a session runs, everything Sir says is his answer
//! (the brain routes it here) until he says "stop the quiz".

use crate::features::{bar, show_window, spoken, str_arg};
use crate::integration_router::{Mode, Route};
use crate::tools::ToolContext;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tracing::info;

const DEFAULT_QUESTIONS: usize = 5;

#[derive(Debug, Clone)]
struct Graded {
    question: String,
    answer: String,
    score: u32,
    feedback: String,
}

#[derive(Debug, Clone)]
struct Session {
    subject: String,
    source: String,
    total: usize,
    current: String,
    done: Vec<Graded>,
}

#[derive(Clone, Default)]
pub struct ExamCoach {
    session: Arc<Mutex<Option<Session>>>,
}

fn is_stop(text: &str) -> bool {
    let t = text.to_lowercase();
    ["stop the quiz", "stop the exam", "end the quiz", "end the exam", "cancel the quiz", "quit the quiz", "i'm done", "that's enough", "stop quizzing", "beende das quiz", "hör auf"].iter().any(|p| t.contains(p))
}

fn is_skip(text: &str) -> bool {
    let t = text.to_lowercase();
    ["skip", "next question", "i don't know", "no idea", "pass", "keine ahnung", "weiß ich nicht"].iter().any(|p| t.trim_end_matches(['.', '!']) == *p || t.starts_with(p))
}

/// While a quiz runs, Sir's words are his answer.
pub fn route_answer(coach: &ExamCoach, text: &str) -> Option<Route> {
    coach.active().then(|| Route { tool: "answer_oral_exam", args: json!({ "answer": text, "stop": is_stop(text) }), mode: Mode::Verbatim })
}

/// "SCORE: 7" / "FEEDBACK: …" / "NEXT: …" from the examiner's reply.
fn field(reply: &str, name: &str) -> Option<String> {
    // ASCII upper-casing keeps the byte offsets of the original.
    let upper = reply.to_ascii_uppercase();
    let start = upper.find(&format!("{name}:"))? + name.len() + 1;
    let rest = &reply[start..];
    let end = ["SCORE:", "FEEDBACK:", "NEXT:", "QUESTION:"].iter().filter_map(|m| rest.to_ascii_uppercase().find(m)).min().unwrap_or(rest.len());
    Some(rest[..end].trim().trim_matches('*').trim().to_string()).filter(|s| !s.is_empty())
}

fn parse_score(raw: &str) -> Option<u32> {
    raw.split(|c: char| !c.is_ascii_digit()).find(|s| !s.is_empty())?.parse::<u32>().ok().map(|s| s.min(10))
}

impl ExamCoach {
    pub fn active(&self) -> bool {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn describe(&self) -> Option<String> {
        let g = self.session.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().map(|s| format!("Oral exam running: {} (question {} of {}). Sir's next words are his answer.", s.subject, s.done.len() + 1, s.total))
    }

    fn card(&self, tools: &ToolContext, s: &Session, finished: bool) {
        let points: u32 = s.done.iter().map(|g| g.score).sum();
        let max = (s.done.len() as u32 * 10).max(1);
        let mut md = format!("## {} — Oral Exam\n\n**Progress** {}  ({} of {})\n\n**Score** {} / {}\n\n", s.subject, bar(s.done.len() as f64 / s.total as f64, 12), s.done.len(), s.total, points, s.done.len() * 10);
        if !finished && !s.current.is_empty() {
            md.push_str(&format!("### Question {}\n{}\n\n", s.done.len() + 1, s.current));
        }
        for (i, g) in s.done.iter().enumerate().rev() {
            md.push_str(&format!("**Q{}** {} — **{}/10**\n> {}\n\n{}\n\n", i + 1, g.question, g.score, g.answer, g.feedback));
        }
        if finished {
            md.push_str(&format!("### Final grade: {}\n", grade(points as f64 / max as f64)));
        }
        show_window(tools, "oral_exam", "Quiz — J.A.I. Exam Coach", &md, 520.0, 620.0);
    }

    /// `start_oral_exam_session`.
    pub async fn start(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let subject_arg = str_arg(args, &["subject", "topic"]).map(String::from);
        let source_arg = str_arg(args, &["source_file_or_topic", "source", "file"]).map(String::from);
        let total = crate::features::num_arg(args, &["questions", "count"]).map(|n| n.clamp(1.0, 20.0) as usize).unwrap_or(DEFAULT_QUESTIONS);
        let (subject, source) = match source_arg.as_deref().and_then(crate::features::resolve_path) {
            Some(path) if path.is_file() => {
                let text = crate::features::read_document(&path).await?;
                let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("the document").to_string();
                (subject_arg.unwrap_or(name), text.chars().take(10_000).collect::<String>())
            }
            _ => {
                let topic = subject_arg.or(source_arg).ok_or("Which subject or file shall I examine you on, Sir?")?;
                (topic.clone(), String::new())
            }
        };
        let question = self.ask(tools, &subject, &source, &[]).await?;
        let session = Session { subject: subject.clone(), source, total, current: question.clone(), done: Vec::new() };
        self.card(tools, &session, false);
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
        info!("[EXAM] Oral exam on {subject} ({total} questions)");
        Ok(spoken(
            format!("Oral exam: {subject} — {total} questions. Say \"stop the quiz\" to end early.\n\nQuestion 1: {question}"),
            format!("Very well, Sir. {total} questions on {subject}. First question: {question}"),
        ))
    }

    /// The next question (never one already asked).
    async fn ask(&self, tools: &ToolContext, subject: &str, source: &str, asked: &[String]) -> Result<String, String> {
        let material = if source.is_empty() { format!("Topic: {subject}") } else { format!("Topic: {subject}\nSource material:\n{source}") };
        let previous = if asked.is_empty() { String::new() } else { format!("\nAlready asked (ask something different):\n- {}", asked.join("\n- ")) };
        let instruction = "You are a strict but fair oral examiner for a German Gymnasium student. Ask exactly ONE clear exam question about the material that can be answered aloud in two or three sentences. Vary between facts, explanations and transfer questions. Reply with the question only, in English.";
        let reply = tools.pim.complete(instruction, &format!("{material}{previous}"), 120).await.ok_or("The language model is not available for the exam, Sir.")?;
        let q = reply.trim().trim_start_matches("Question:").trim().trim_matches('"').to_string();
        if q.is_empty() {
            Err("I could not come up with a question.".into())
        } else {
            Ok(q)
        }
    }

    /// `answer_oral_exam`: grades the answer and asks the next question (or ends the session).
    pub async fn answer(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let Some(mut s) = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return Err("No oral exam is running; say \"quiz me on …\" to start one.".into());
        };
        let answer = str_arg(args, &["answer", "text"]).unwrap_or("").to_string();
        if crate::tools::bool_arg(args, "stop").unwrap_or(false) || is_stop(&answer) {
            return Ok(self.finish(tools, &s));
        }
        let (score, feedback) = if is_skip(&answer) || answer.trim().is_empty() {
            (0, "Skipped.".to_string())
        } else {
            let instruction = "You grade a student's spoken exam answer. Be fair; spoken answers are short. Reply exactly in this format:\nSCORE: <0-10>\nFEEDBACK: <one or two sentences: what was right, what was missing, the correct key point>";
            let material = if s.source.is_empty() { String::new() } else { format!("\nSource material:\n{}", s.source.chars().take(6000).collect::<String>()) };
            let reply = tools
                .pim
                .complete(instruction, &format!("Topic: {}\nQuestion: {}\nStudent's answer: {answer}{material}", s.subject, s.current), 160)
                .await
                .ok_or("The language model is not available to grade that, Sir.")?;
            let score = field(&reply, "SCORE").and_then(|v| parse_score(&v)).unwrap_or(5);
            (score, field(&reply, "FEEDBACK").unwrap_or_else(|| reply.trim().to_string()))
        };
        s.done.push(Graded { question: s.current.clone(), answer: answer.clone(), score, feedback: feedback.clone() });
        if s.done.len() >= s.total {
            let summary = self.finish(tools, &s);
            return Ok(spoken(format!("{score}/10 — {feedback}\n\n{}", summary.split("\nSpoken: ").next().unwrap_or("")), format!("{score} out of 10. {feedback} {}", summary.rsplit("\nSpoken: ").next().unwrap_or(""))));
        }
        let asked: Vec<String> = s.done.iter().map(|g| g.question.clone()).collect();
        let next = self.ask(tools, &s.subject, &s.source, &asked).await?;
        s.current = next.clone();
        let n = s.done.len() + 1;
        self.card(tools, &s, false);
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
        Ok(spoken(format!("{score}/10 — {feedback}\n\nQuestion {n}: {next}"), format!("{score} out of 10. {feedback} Question {n}: {next}")))
    }

    fn finish(&self, tools: &ToolContext, s: &Session) -> String {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let points: u32 = s.done.iter().map(|g| g.score).sum();
        let max = s.done.len() as u32 * 10;
        self.card(tools, s, true);
        if s.done.is_empty() {
            return spoken("Oral exam ended before the first answer.", "Quiz ended, Sir.");
        }
        let fraction = points as f64 / max.max(1) as f64;
        let weakest = s.done.iter().min_by_key(|g| g.score).map(|g| g.question.clone()).unwrap_or_default();
        spoken(
            format!("Oral exam on {} finished: {points}/{max} points ({:.0} %), grade {}.\nWeakest: {weakest}", s.subject, fraction * 100.0, grade(fraction)),
            format!("That's the end of the exam, Sir: {points} of {max} points, a grade {}. Worth revisiting: {weakest}", grade(fraction)),
        )
    }
}

/// German school grade (1 = sehr gut … 6 = ungenügend) for a share of the points.
fn grade(fraction: f64) -> &'static str {
    match (fraction * 100.0).round() as u32 {
        92..=100 => "1 (sehr gut)",
        81..=91 => "2 (gut)",
        67..=80 => "3 (befriedigend)",
        50..=66 => "4 (ausreichend)",
        30..=49 => "5 (mangelhaft)",
        _ => "6 (ungenügend)",
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "start_oral_exam_session",
        "description": "Exam Coach: start a spoken oral exam. JAI asks one question at a time about a subject or a file (worksheet, notes, PDF), grades each spoken answer 0-10 and shows a Quiz card. Use for 'quiz me on …', 'test me on my biology worksheet'.",
        "parameters": {"type": "object", "properties": {
            "subject": {"type": "string"},
            "source_file_or_topic": {"type": "string", "description": "A file path (PDF, .md, .txt) or a topic"},
            "questions": {"type": "integer", "description": "How many questions (default 5)"}
        }, "required": ["subject"]}
    }})
}

pub fn answer_tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "answer_oral_exam",
        "description": "Exam Coach: grade Sir's answer to the current oral-exam question and ask the next one (stop=true ends the exam).",
        "parameters": {"type": "object", "properties": {
            "answer": {"type": "string"},
            "stop": {"type": "boolean"}
        }, "required": ["answer"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examiner_replies_and_routing() {
        let reply = "SCORE: 7/10\nFEEDBACK: Good, but you missed the role of chlorophyll.";
        assert_eq!(field(reply, "SCORE").and_then(|s| parse_score(&s)), Some(7));
        assert_eq!(field(reply, "FEEDBACK").unwrap(), "Good, but you missed the role of chlorophyll.");
        assert_eq!(grade(0.95), "1 (sehr gut)");
        assert_eq!(grade(0.55), "4 (ausreichend)");
        let coach = ExamCoach::default();
        assert!(route_answer(&coach, "photosynthesis makes sugar").is_none());
        *coach.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(Session { subject: "Biology".into(), source: String::new(), total: 3, current: "What is ATP?".into(), done: vec![] });
        let r = route_answer(&coach, "stop the quiz").unwrap();
        assert_eq!((r.tool, r.args["stop"].as_bool()), ("answer_oral_exam", Some(true)));
        assert!(is_skip("I don't know"));
    }
}
