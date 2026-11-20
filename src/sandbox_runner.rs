//! The Sandbox Runner: "calculate the orbital period in Python." JAI writes (or takes) a Python
//! or Rust program, runs it in a throw-away folder with a time limit, repairs it from the error up
//! to three times, and shows the code, the output and any chart it drew (chart.png).

use crate::features::{find_tool, python, run, show_window, spoken, str_arg};
use crate::tools::ToolContext;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::info;

const RUN_LIMIT: Duration = Duration::from_secs(45);
const MAX_FIXES: usize = 3;

/// The code inside a Markdown fence (or the whole reply).
pub fn extract_code(reply: &str) -> String {
    if let Some(start) = reply.find("```") {
        let after = &reply[start + 3..];
        let body = after.split_once('\n').map(|(_, b)| b).unwrap_or(after);
        if let Some(end) = body.find("```") {
            return body[..end].trim_end().to_string();
        }
        return body.trim_end().to_string();
    }
    reply.trim().to_string()
}

pub fn looks_like_code(text: &str, rust: bool) -> bool {
    let markers: &[&str] = if rust { &["fn main", "println!", "let "] } else { &["print(", "import ", "def ", " = ", "for "] };
    text.contains('\n') && markers.iter().filter(|m| text.contains(*m)).count() >= 2
}

struct Attempt {
    ok: bool,
    output: String,
}

async fn execute(dir: &Path, code: &str, rust: bool) -> Result<Attempt, String> {
    if rust {
        std::fs::write(dir.join("main.rs"), code).map_err(|e| e.to_string())?;
        let rustc = find_tool("rustc")
            .or_else(|| crate::apps::home_dir().map(|h| h.join(".cargo").join("bin").join("rustc.exe")).filter(|p| p.is_file()))
            .ok_or("Rust is not installed (rustc not found).")?;
        let exe = dir.join(if cfg!(windows) { "main.exe" } else { "main" });
        let build = run(&rustc, &["-O", "--edition", "2021", "main.rs", "-o", &exe.display().to_string()], Some(dir), Duration::from_secs(120)).await?;
        if !build.ok {
            return Ok(Attempt { ok: false, output: build.stderr.chars().take(3000).collect() });
        }
        let out = run(&exe, &[], Some(dir), RUN_LIMIT).await?;
        Ok(Attempt { ok: out.ok, output: format!("{}{}", out.stdout, if out.ok { String::new() } else { format!("\n{}", out.stderr) }) })
    } else {
        std::fs::write(dir.join("main.py"), code).map_err(|e| e.to_string())?;
        let py = python().ok_or("No Python interpreter is installed.")?;
        // Matplotlib without a display: charts are written to chart.png.
        let out = run(&py, &["-X", "utf8", "-c", "import os, runpy; os.environ.setdefault('MPLBACKEND', 'Agg'); runpy.run_path('main.py', run_name='__main__')"], Some(dir), RUN_LIMIT).await?;
        Ok(Attempt { ok: out.ok, output: format!("{}{}", out.stdout, if out.ok { String::new() } else { format!("\n{}", out.stderr) }) })
    }
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let language = str_arg(args, &["language", "lang"]).unwrap_or("python").to_lowercase();
    let rust = language.contains("rust");
    let lang = if rust { "Rust" } else { "Python" };
    let task = str_arg(args, &["code_or_task", "code", "task"]).ok_or("What shall I calculate, Sir?")?.to_string();
    let mut code = if looks_like_code(&task, rust) {
        extract_code(&task)
    } else {
        let instruction = if rust {
            "Write a complete, standalone Rust program (std only, no crates) that solves the task and prints the result clearly with labels and units. Reply with the code only in one ```rust block."
        } else {
            "Write a complete, standalone Python 3 script that solves the task and prints the result clearly with labels and units. Use only the standard library plus numpy/matplotlib if truly useful; if a chart helps, save it with plt.savefig('chart.png') (never plt.show()). Reply with the code only in one ```python block."
        };
        extract_code(&tools.pim.complete(instruction, &task, 900).await.ok_or("The language model is not available to write the program, Sir.")?)
    };
    let dir: PathBuf = std::env::temp_dir().join("jai_sandbox").join(crate::features::stamp());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut attempt = execute(&dir, &code, rust).await?;
    let mut fixes = 0;
    while !attempt.ok && fixes < MAX_FIXES {
        fixes += 1;
        info!("[SANDBOX] Attempt {fixes} failed; repairing");
        let fix = tools
            .pim
            .complete(&format!("The {lang} program failed. Fix it. Reply with the complete corrected program only, in one code block."), &format!("Task: {task}\n\nProgram:\n{code}\n\nError:\n{}", attempt.output.chars().take(2500).collect::<String>()), 900)
            .await;
        let Some(fix) = fix else { break };
        code = extract_code(&fix);
        attempt = execute(&dir, &code, rust).await?;
    }
    let chart = dir.join("chart.png");
    let has_chart = chart.is_file();
    if has_chart {
        let _ = crate::platform::open(&chart.display().to_string());
    }
    let output = attempt.output.trim().chars().take(4000).collect::<String>();
    let status = if attempt.ok { format!("ran successfully{}", if fixes > 0 { format!(" after {fixes} repair(s)") } else { String::new() }) } else { format!("still fails after {fixes} repair(s)") };
    let md = format!(
        "## Sandbox — {lang}\n*{status} · folder {}*\n\n```{}\n{code}\n```\n\n### Output\n```\n{output}\n```{}",
        dir.display(),
        lang.to_lowercase(),
        if has_chart { "\n\nChart: chart.png (opened)" } else { "" }
    );
    show_window(tools, "sandbox_runner", &format!("Sandbox — {lang}"), &md, 720.0, 640.0);
    if !attempt.ok {
        return Ok(spoken(md, format!("I'm afraid the program still fails, Sir: {}", output.lines().last().unwrap_or("unknown error"))));
    }
    let first: String = output.lines().filter(|l| !l.trim().is_empty()).take(3).collect::<Vec<_>>().join(". ");
    Ok(spoken(md, if first.is_empty() { "Done, Sir. The program ran without output.".into() } else { format!("{}{}", first.chars().take(300).collect::<String>(), if has_chart { " The chart is on screen." } else { "" }) }))
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "run_sandbox_calculation_or_script",
        "description": "Sandbox Runner: write (or take) a Python or Rust program for a calculation, simulation or data task, run it in a temporary folder with a time limit, auto-fix errors up to 3 times, and show code + output (+ chart.png). Use for maths/physics/finance calculations that need real computation.",
        "parameters": {"type": "object", "properties": {
            "language": {"type": "string", "enum": ["python", "rust"]},
            "code_or_task": {"type": "string", "description": "The task in words, or complete code"}
        }, "required": ["code_or_task"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_extraction() {
        assert_eq!(extract_code("Here:\n```python\nprint(1)\n```\nDone"), "print(1)");
        assert_eq!(extract_code("print(2)"), "print(2)");
        assert!(looks_like_code("import math\nprint(math.pi)", false));
        assert!(!looks_like_code("calculate the orbit of the moon", false));
    }
}
