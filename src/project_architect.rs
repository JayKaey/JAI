//! The Project Architect: "new PyTorch project called StarkVision on my second monitor." Creates
//! the folder from a template (PyTorch + CUDA, Rust binary, Python app, web app, Minecraft
//! modpack), initialises git, a Cargo project or a virtual environment, checks the GPU for CUDA
//! work, opens it in VS Code and puts the window on the chosen monitor.

use crate::features::{find_tool, python, run, safe_name, spoken, str_arg};
use crate::tools::ToolContext;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::info;

pub const TEMPLATES: [&str; 5] = ["PyTorch_CUDA", "Rust_Binary", "Python_App", "Web_App", "Minecraft_Modpack"];

pub fn template_of(raw: &str) -> &'static str {
    let t = raw.to_lowercase();
    if t.contains("torch") || t.contains("cuda") || t.contains("ml") {
        "PyTorch_CUDA"
    } else if t.contains("rust") || t.contains("cargo") {
        "Rust_Binary"
    } else if t.contains("web") || t.contains("html") || t.contains("site") {
        "Web_App"
    } else if t.contains("minecraft") || t.contains("modpack") {
        "Minecraft_Modpack"
    } else {
        "Python_App"
    }
}

fn projects_root(tools: &ToolContext) -> PathBuf {
    let configured = tools.config.read(|c| c.features.projects_dir.clone());
    if !configured.trim().is_empty() {
        return PathBuf::from(crate::apps::expand_env(configured.trim()));
    }
    let code = PathBuf::from(r"D:\Code");
    if code.is_dir() {
        return code;
    }
    crate::apps::home_dir().unwrap_or_else(std::env::temp_dir).join("Projects")
}

/// The template's files (relative path, content).
pub fn files(template: &str, name: &str) -> Vec<(String, String)> {
    let crate_name: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let readme = |what: &str, run: &str| format!("# {name}\n\n{what}\n\nCreated by J.A.I. on {}.\n\n## Run\n\n```\n{run}\n```\n", chrono::Local::now().format("%d.%m.%Y"));
    match template {
        "PyTorch_CUDA" => vec![
            ("README.md".into(), readme("A PyTorch project with CUDA.", ".venv\\Scripts\\activate\npip install -r requirements.txt\npython train.py")),
            (".gitignore".into(), ".venv/\n__pycache__/\n*.pt\ncheckpoints/\ndata/\nruns/\n".into()),
            ("requirements.txt".into(), "--extra-index-url https://download.pytorch.org/whl/cu124\ntorch\ntorchvision\nnumpy\ntqdm\nmatplotlib\n".into()),
            (
                "train.py".into(),
                r#""""Training entry point."""
import torch
from torch import nn


def device() -> torch.device:
    if torch.cuda.is_available():
        print(f"CUDA: {torch.cuda.get_device_name(0)} · {torch.cuda.get_device_properties(0).total_memory / 2**30:.1f} GB")
        return torch.device("cuda")
    print("CUDA not available — running on the CPU.")
    return torch.device("cpu")


class Net(nn.Module):
    def __init__(self) -> None:
        super().__init__()
        self.layers = nn.Sequential(nn.Linear(32, 128), nn.ReLU(), nn.Linear(128, 1))

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.layers(x)


def main() -> None:
    dev = device()
    model = Net().to(dev)
    optimiser = torch.optim.AdamW(model.parameters(), lr=1e-3)
    x = torch.randn(4096, 32, device=dev)
    y = x.sum(dim=1, keepdim=True)
    for step in range(500):
        loss = nn.functional.mse_loss(model(x), y)
        optimiser.zero_grad()
        loss.backward()
        optimiser.step()
        if step % 100 == 0:
            print(f"step {step:4d}  loss {loss.item():.4f}")


if __name__ == "__main__":
    main()
"#
                .into(),
            ),
        ],
        "Rust_Binary" => vec![
            ("README.md".into(), readme("A Rust command-line program.", "cargo run --release")),
            (".gitignore".into(), "/target\n".into()),
            ("Cargo.toml".into(), format!("[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n\n[profile.release]\nlto = true\n")),
            ("src/main.rs".into(), format!("fn main() {{\n    println!(\"{name} is online.\");\n}}\n")),
        ],
        "Web_App" => vec![
            ("README.md".into(), readme("A static web app (HTML, CSS, JavaScript).", "python -m http.server 8000")),
            (".gitignore".into(), "node_modules/\ndist/\n".into()),
            (
                "index.html".into(),
                format!("<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>{name}</title>\n  <link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>\n  <main>\n    <h1>{name}</h1>\n    <p id=\"status\">Loading…</p>\n  </main>\n  <script src=\"app.js\"></script>\n</body>\n</html>\n"),
            ),
            ("style.css".into(), ":root { color-scheme: dark; --accent: #4cc3ff; }\nbody { margin: 0; font-family: system-ui, sans-serif; background: #050a12; color: #d8f1ff; }\nmain { max-width: 760px; margin: 10vh auto; padding: 0 16px; }\nh1 { color: var(--accent); letter-spacing: 2px; }\n".into()),
            ("app.js".into(), "document.getElementById(\"status\").textContent = `Ready · ${new Date().toLocaleString()}`;\n".into()),
        ],
        "Minecraft_Modpack" => vec![
            ("README.md".into(), readme("A Minecraft modpack (Fabric). Put mods in mods/, configs in config/; import the folder into Prism Launcher or Modrinth App.", "packwiz refresh   # if you use packwiz")),
            (".gitignore".into(), "*.log\nlogs/\ncrash-reports/\nsaves/\n".into()),
            ("modpack.json".into(), serde_json::to_string_pretty(&json!({ "name": name, "minecraft": "1.21.1", "loader": "fabric", "loader_version": "latest", "author": "Sir", "mods": [] })).unwrap_or_default()),
            ("mods/.keep".into(), String::new()),
            ("config/.keep".into(), String::new()),
            ("resourcepacks/.keep".into(), String::new()),
            ("shaderpacks/.keep".into(), String::new()),
        ],
        _ => vec![
            ("README.md".into(), readme("A Python application.", ".venv\\Scripts\\activate\npip install -r requirements.txt\npython main.py")),
            (".gitignore".into(), ".venv/\n__pycache__/\n*.pyc\n.env\n".into()),
            ("requirements.txt".into(), String::new()),
            ("main.py".into(), format!("\"\"\"{name}.\"\"\"\n\n\ndef main() -> None:\n    print(\"{name} is online.\")\n\n\nif __name__ == \"__main__\":\n    main()\n")),
        ],
    }
}

/// The GPU and driver for CUDA projects ("NVIDIA GeForce RTX 4070, 560.94").
async fn gpu_check() -> String {
    match find_tool("nvidia-smi") {
        Some(smi) => match run(&smi, &["--query-gpu=name,driver_version,memory.total", "--format=csv,noheader"], None, Duration::from_secs(10)).await {
            Ok(o) if o.ok => format!("CUDA GPU: {}", o.stdout.lines().next().unwrap_or("").trim()),
            _ => "nvidia-smi did not answer; CUDA may not be available.".into(),
        },
        None => "No NVIDIA driver found; PyTorch will run on the CPU.".into(),
    }
}

async fn step(log: &mut Vec<String>, label: &str, program: Option<PathBuf>, args: &[&str], dir: &Path, limit: u64) -> bool {
    let Some(program) = program else {
        log.push(format!("{label}: skipped (not installed)"));
        return false;
    };
    match run(&program, args, Some(dir), Duration::from_secs(limit)).await {
        Ok(o) if o.ok => {
            log.push(format!("{label}: OK"));
            true
        }
        Ok(o) => {
            log.push(format!("{label}: failed ({})", o.error_line()));
            false
        }
        Err(e) => {
            log.push(format!("{label}: {e}"));
            false
        }
    }
}

pub async fn run_tool(tools: &ToolContext, args: &Value) -> Result<String, String> {
    let template = template_of(str_arg(args, &["template_type", "template", "type"]).unwrap_or("Python_App"));
    let name = safe_name(str_arg(args, &["project_name", "name"]).unwrap_or(&format!("{}_{}", template, chrono::Local::now().format("%m%d_%H%M"))));
    let root = projects_root(tools);
    let dir = root.join(&name);
    if dir.exists() && std::fs::read_dir(&dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Err(format!("{} already exists and is not empty, Sir. Choose another name.", dir.display()));
    }
    for (rel, content) in files(template, &name) {
        let path = dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{} could not be created: {e}", parent.display()))?;
        }
        std::fs::write(&path, content).map_err(|e| format!("{} could not be written: {e}", path.display()))?;
    }
    let mut log = vec![format!("{} files from the {template} template", files(template, &name).len())];
    let git = find_tool("git");
    if step(&mut log, "git init", git.clone(), &["init", "-q"], &dir, 30).await {
        step(&mut log, "git add", git.clone(), &["add", "-A"], &dir, 30).await;
        // Only with Sir's own git identity; otherwise the repository waits for his first commit.
        step(&mut log, "first commit", git, &["commit", "-q", "-m", "Initial commit (J.A.I. Project Architect)"], &dir, 30).await;
    }
    match template {
        "PyTorch_CUDA" | "Python_App" => {
            step(&mut log, "virtual environment (.venv)", python(), &["-m", "venv", ".venv"], &dir, 180).await;
        }
        "Rust_Binary" => {
            let cargo = find_tool("cargo").or_else(|| crate::apps::home_dir().map(|h| h.join(".cargo").join("bin").join("cargo.exe")).filter(|p| p.is_file()));
            step(&mut log, "cargo check", cargo, &["check", "-q"], &dir, 300).await;
        }
        _ => {}
    }
    if template == "PyTorch_CUDA" {
        log.push(gpu_check().await);
        log.push("Install PyTorch: .venv\\Scripts\\pip install -r requirements.txt".into());
    }
    // VS Code, then onto the chosen monitor.
    let code = find_tool("code").or_else(|| std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Programs").join("Microsoft VS Code").join("bin").join("code.cmd")).filter(|p| p.is_file()));
    let opened = match &code {
        Some(c) => run(c, &[&dir.display().to_string()], None, Duration::from_secs(20)).await.is_ok_and(|o| o.ok),
        None => false,
    };
    log.push(if opened { "VS Code: opened".into() } else { "VS Code: not found (folder opened instead)".into() });
    if !opened {
        let _ = crate::win32::shell_execute(&dir.display().to_string(), None, None);
    }
    let monitor = str_arg(args, &["target_monitor", "monitor"]).map(String::from);
    if let (Some(m), true) = (&monitor, opened) {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let moved = Box::pin(tools.execute("move_window_to_monitor", &json!({ "app_or_window_name": "Visual Studio Code", "target_monitor": m }))).await;
        log.push(format!("Monitor {m}: {}", if moved.success { "OK" } else { moved.output.as_str() }));
    }
    tools.focus.note_file(&dir);
    info!("[ARCHITECT] {} ({template}) at {}", name, dir.display());
    Ok(spoken(
        format!("Project {name} ({template}) created at {}\n- {}", dir.display(), log.join("\n- ")),
        format!("{name} is ready, Sir: a {} project{}{}.", template.replace('_', " "), if opened { ", open in VS Code" } else { "" }, monitor.map(|m| format!(" on monitor {m}")).unwrap_or_default()),
    ))
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "spawn_coding_project",
        "description": "Project Architect: create a new coding project from a template (PyTorch_CUDA with GPU check + venv, Rust_Binary with Cargo, Python_App with venv, Web_App, Minecraft_Modpack), git init, open it in VS Code and move VS Code to target_monitor.",
        "parameters": {"type": "object", "properties": {
            "project_name": {"type": "string"},
            "template_type": {"type": "string", "enum": TEMPLATES},
            "target_monitor": {"type": "string", "description": "1, 2, left, right, primary (optional)"}
        }, "required": ["project_name", "template_type"]}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_have_their_files() {
        assert_eq!(template_of("rust"), "Rust_Binary");
        assert_eq!(template_of("PyTorch_CUDA"), "PyTorch_CUDA");
        let rust = files("Rust_Binary", "Stark Net");
        assert!(rust.iter().any(|(p, c)| p == "Cargo.toml" && c.contains("name = \"stark_net\"")));
        assert!(files("Minecraft_Modpack", "Pack").iter().any(|(p, _)| p == "modpack.json"));
        for t in TEMPLATES {
            assert!(files(t, "x").iter().any(|(p, _)| p == "README.md"), "{t}");
        }
    }
}
