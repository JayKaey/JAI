//! The Stress Tester: "stress test this plan / this file." A ruthless review that names the top
//! three bottlenecks or failure points of an idea or a piece of code, each with a concrete fix,
//! backed by a few hard numbers measured on the code itself.

use crate::features::{read_document, resolve_path, show_window, spoken, str_arg};
use crate::tools::ToolContext;
use serde_json::{json, Value};

/// Cheap facts about source code that make the review concrete.
pub fn code_metrics(text: &str) -> Vec<String> {
    let lines = text.lines().count();
    let count = |needle: &str| text.matches(needle).count();
    let mut m = vec![format!("{lines} lines")];
    let functions = count("fn ") + count("def ") + count("function ");
    if functions > 0 {
        m.push(format!("{functions} functions"));
    }
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    if longest > 140 {
        m.push(format!("longest line {longest} chars"));
    }
    for (needle, label) in [(".unwrap()", "unwrap() calls"), (".clone()", "clone() calls"), ("unsafe ", "unsafe blocks"), ("TODO", "TODOs"), ("FIXME", "FIXMEs"), ("except:", "bare excepts"), ("global ", "globals"), ("time.sleep(", "sleep calls"), ("thread::sleep", "blocking sleeps")] {
        let n = count(needle);
        if n > 0 {
            m.push(format!("{n} {label}"));
        }
    }
    let depth = text.lines().map(|l| l.len() - l.trim_start().len()).max().unwrap_or(0) / 4;
    if depth >= 6 {
        m.push(format!("nesting up to {depth} levels"));
    }
    m
}

fn looks_like_code(text: &str) -> bool {
    let markers = ["fn ", "def ", "class ", "import ", "#include", "function ", "=>", "};", "let ", "pub "];
    text.lines().count() > 3 && markers.iter().filter(|m| text.contains(*m)).count() >= 2
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let target = str_arg(args, &["target_plan_or_file", "target", "plan", "file"]).unwrap_or("clipboard").to_string();
    let (name, text) = match target.to_lowercase().as_str() {
        "clipboard" | "this" | "that" | "it" => ("the clipboard".to_string(), crate::clipboard::read().await.map_err(|e| format!("I couldn't read the clipboard: {e}"))?),
        _ => match resolve_path(&target).filter(|p| p.is_file()) {
            Some(path) => (path.file_name().and_then(|n| n.to_str()).unwrap_or("the file").to_string(), read_document(&path).await?),
            None => ("the plan".to_string(), target.clone()),
        },
    };
    if text.trim().len() < 15 {
        return Err("There's nothing substantial to stress test, Sir — copy the plan or code, or name the file.".into());
    }
    let code = looks_like_code(&text);
    let metrics = if code { code_metrics(&text) } else { Vec::new() };
    let instruction = if code {
        "You are a ruthless principal engineer reviewing code. Find the TOP 3 bottlenecks or failure points (performance, correctness under load, concurrency, error handling, security, maintainability), most severe first. For each give: a bold title, why it breaks (concrete), and the fix (concrete, with a short code hint if useful). Then one line 'Verdict:' with an overall judgement. Markdown, English, no preamble."
    } else {
        "You are a ruthless red-team strategist. Stress-test the plan or idea: find the TOP 3 bottlenecks, risks or failure points (cost, time, scale, dependencies, assumptions, people), most severe first. For each give: a bold title, how it fails (concrete numbers when possible), and the fix. Then one line 'Verdict:' with an overall judgement. Markdown, English, no preamble."
    };
    let facts = if metrics.is_empty() { String::new() } else { format!("Measured: {}\n\n", metrics.join(", ")) };
    let review = tools.pim.complete(instruction, &format!("{facts}{text}"), 900).await.ok_or("The language model is not available for the review, Sir.")?;
    let titles: Vec<String> = review
        .lines()
        .filter_map(|l| {
            let start = l.find("**")? + 2;
            let end = l[start..].find("**")? + start;
            Some(l[start..end].trim().trim_end_matches(':').to_string())
        })
        .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("verdict"))
        .take(3)
        .collect();
    let verdict = review.lines().find(|l| l.to_lowercase().contains("verdict")).map(|l| l.replace("**", "").replace("Verdict:", "").trim().to_string()).unwrap_or_default();
    let md = format!("## Stress Test — {name}\n{}{review}", if metrics.is_empty() { String::new() } else { format!("*{}*\n\n", metrics.join(" · ")) });
    show_window(tools, "stress_test", &format!("Stress Test — {name}"), &md, 680.0, 640.0);
    let said = if titles.is_empty() {
        format!("The review of {name} is on screen, Sir. {verdict}")
    } else {
        format!("Three weak points in {name}: {}. {verdict}", titles.join("; "))
    };
    Ok(spoken(md, said))
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "stress_test_idea_or_code",
        "description": "Stress Tester: a ruthless review of a plan, an idea or a code file — the top 3 bottlenecks / failure points with concrete fixes, shown in a window. target_plan_or_file: the plan text, a file path, or \"clipboard\".",
        "parameters": {"type": "object", "properties": {
            "target_plan_or_file": {"type": "string"}
        }, "required": ["target_plan_or_file"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_and_code_detection() {
        let code = "use std::x;\nfn main() {\n    let a = b.unwrap();\n    let c = a.clone();\n    // TODO speed\n}\n";
        assert!(looks_like_code(code));
        let m = code_metrics(code);
        assert!(m.contains(&"1 unwrap() calls".to_string()) && m.contains(&"1 TODOs".to_string()));
        assert!(!looks_like_code("Open a café in Berlin next spring with two employees."));
    }
}
