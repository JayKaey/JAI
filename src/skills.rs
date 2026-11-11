//! Autonomous Skill Forge: reusable PowerShell (Windows) / Bash (Linux) scripts that JAI can
//! create, list, run and delete. `skills/<name>.json` is the canonical store (human-editable);
//! SQLite mirrors it for usage statistics and fast listing.

use regex::Regex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

const SKILL_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_OUTPUT_CHARS: usize = 2400;

static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*\}\}").expect("valid regex"));

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("skill storage error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("skill file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("skill JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no skill named '{0}'")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("skill execution failed: {0}")]
    Execution(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillParameter {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JaiSkill {
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub trigger_phrases: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<SkillParameter>,
    #[serde(default = "default_runtime")]
    pub runtime: String,
    pub script: String,
    #[serde(default)]
    pub usage_count: i64,
    #[serde(default)]
    pub last_output: String,
    #[serde(default)]
    pub updated_at: String,
}

fn default_runtime() -> String {
    if cfg!(windows) { "powershell" } else { "bash" }.to_string()
}

/// "powershell", "bash" or "python" (anything else falls back to the OS shell).
pub fn normalize_runtime(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "python" | "python3" | "py" => "python".into(),
        "bash" | "sh" => "bash".into(),
        "powershell" | "pwsh" | "ps" | "ps1" => "powershell".into(),
        _ => default_runtime(),
    }
}

fn script_extension(runtime: &str) -> &'static str {
    match runtime {
        "powershell" => "ps1",
        "python" => "py",
        _ => "sh",
    }
}

/// The Python interpreter, if one is installed.
fn python() -> Option<String> {
    ["python3", "python", "py"]
        .iter()
        .find(|p| crate::apps::which(p).is_some())
        .map(|p| p.to_string())
}

#[derive(Clone)]
pub struct SkillManager {
    conn: Arc<Mutex<Connection>>,
    dir: PathBuf,
}

impl SkillManager {
    pub fn open<P: AsRef<Path>, D: AsRef<Path>>(db_path: P, skills_dir: D) -> Result<Self, SkillError> {
        let dir = skills_dir.as_ref().to_path_buf();
        let first_run = !dir.exists();
        fs::create_dir_all(&dir)?;

        let conn = Connection::open(db_path.as_ref())?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS skills (
                 name TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 description TEXT NOT NULL,
                 trigger_phrases_json TEXT NOT NULL DEFAULT '[]',
                 parameters_json TEXT NOT NULL DEFAULT '[]',
                 runtime TEXT NOT NULL DEFAULT 'powershell',
                 script TEXT NOT NULL,
                 usage_count INTEGER NOT NULL DEFAULT 0,
                 last_output TEXT NOT NULL DEFAULT '',
                 updated_at TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )?;

        let manager = Self {
            conn: Arc::new(Mutex::new(conn)),
            dir,
        };
        if first_run {
            manager.seed_starter_skills()?;
        }
        manager.sync_from_disk();
        info!("[SKILL] {} skills loaded from {:?}", manager.list()?.len(), manager.dir);
        Ok(manager)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn normalize_name(raw: &str) -> String {
        let mut out = String::new();
        let mut last_us = true;
        for ch in raw.trim().to_lowercase().chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
                last_us = false;
            } else if !last_us {
                out.push('_');
                last_us = true;
            }
        }
        let trimmed = out.trim_matches('_');
        let name = trimmed.strip_prefix("skill_").unwrap_or(trimmed);
        if name.is_empty() {
            "custom_skill".to_string()
        } else {
            name.chars().take(48).collect()
        }
    }

    /// Names copied from a template instead of chosen ("snake_case_name", "skill_name").
    pub fn is_placeholder_name(name: &str) -> bool {
        matches!(name, "snake_case_name" | "snake_case" | "skill_name" | "name" | "my_skill" | "new_skill" | "example" | "example_skill" | "your_skill_name" | "tool_name")
            || name.contains("snake_case")
    }

    /// Disk is canonical: upsert every `skills/*.json`, drop DB rows whose file is gone.
    fn sync_from_disk(&self) {
        let mut on_disk = HashSet::new();
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for path in entries.flatten().map(|e| e.path()) {
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                match fs::read_to_string(&path)
                    .map_err(SkillError::from)
                    .and_then(|raw| serde_json::from_str::<JaiSkill>(&raw).map_err(SkillError::from))
                {
                    Ok(mut skill) => {
                        skill.name = Self::normalize_name(&skill.name);
                        // A skill saved under the forge template's placeholder ("snake_case_name")
                        // gets a real name from its title, on disk too.
                        if Self::is_placeholder_name(&skill.name) {
                            let renamed = Self::normalize_name(&skill.title);
                            if !Self::is_placeholder_name(&renamed) && !self.dir.join(format!("{renamed}.json")).exists() {
                                info!("[SKILL] Renaming the placeholder skill '{}' to '{renamed}'", skill.name);
                                let old = skill.name.clone();
                                skill.name = renamed;
                                if self.save(&skill).is_ok() {
                                    let _ = self.lock().execute("DELETE FROM skills WHERE name = ?1", params![old]);
                                    for ext in ["json", "ps1", "sh", "py"] {
                                        let _ = fs::remove_file(self.dir.join(format!("{old}.{ext}")));
                                    }
                                }
                            }
                        }
                        Self::declare_placeholders(&mut skill);
                        if let Err(e) = self.upsert_row(&skill) {
                            warn!("[SKILL] Could not index {:?}: {e}", path);
                        }
                        on_disk.insert(skill.name);
                    }
                    Err(e) => warn!("[SKILL] Ignoring invalid skill file {:?}: {e}", path),
                }
            }
        }
        let names: Vec<String> = self.list().map(|l| l.into_iter().map(|s| s.name).collect()).unwrap_or_default();
        for name in names.into_iter().filter(|n| !on_disk.contains(n)) {
            info!("[SKILL] '{name}' no longer exists in {:?}; removing it from the index.", self.dir);
            let _ = self.lock().execute("DELETE FROM skills WHERE name = ?1", params![name]);
        }
    }

    /// `{{param}}` placeholders used by the script but missing from `parameters` get declared,
    /// so the LLM knows to supply them.
    fn declare_placeholders(skill: &mut JaiSkill) {
        for cap in PLACEHOLDER.captures_iter(&skill.script) {
            let name = cap[1].to_string();
            if !skill.parameters.iter().any(|p| p.name == name) {
                skill.parameters.push(SkillParameter {
                    description: format!("Value for {{{{{name}}}}}"),
                    name,
                    required: true,
                });
            }
        }
    }

    fn upsert_row(&self, skill: &JaiSkill) -> Result<(), SkillError> {
        let triggers = serde_json::to_string(&skill.trigger_phrases)?;
        let parameters = serde_json::to_string(&skill.parameters)?;
        self.lock().execute(
            "INSERT INTO skills (name, title, description, trigger_phrases_json, parameters_json, runtime, script, usage_count, last_output, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'))
             ON CONFLICT(name) DO UPDATE SET
                 title = excluded.title, description = excluded.description,
                 trigger_phrases_json = excluded.trigger_phrases_json,
                 parameters_json = excluded.parameters_json,
                 runtime = excluded.runtime, script = excluded.script",
            params![
                skill.name, skill.title, skill.description, triggers, parameters,
                skill.runtime, skill.script, skill.usage_count, skill.last_output
            ],
        )?;
        Ok(())
    }

    /// Saves to `skills/<name>.json` (+ script file for editing) and indexes it.
    pub fn save(&self, skill: &JaiSkill) -> Result<JaiSkill, SkillError> {
        let mut s = skill.clone();
        s.name = Self::normalize_name(&s.name);
        if Self::is_placeholder_name(&s.name) {
            s.name = Self::normalize_name(&s.title);
            if Self::is_placeholder_name(&s.name) || s.name == "custom_skill" {
                return Err(SkillError::Invalid("A skill needs a real name (not the template's placeholder).".into()));
            }
        }
        if s.script.trim().is_empty() {
            return Err(SkillError::Invalid("A skill needs a non-empty script.".into()));
        }
        if s.title.trim().is_empty() {
            s.title = s.name.replace('_', " ");
        }
        s.runtime = normalize_runtime(&s.runtime);
        s.trigger_phrases.retain(|t| !t.trim().is_empty());
        Self::declare_placeholders(&mut s);
        s.updated_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        fs::write(self.dir.join(format!("{}.json", s.name)), serde_json::to_string_pretty(&s)?)?;
        // The JSON is canonical; the plain script is a copy for hand-editing.
        let script_file = self.dir.join(format!("{}.{}", s.name, script_extension(&s.runtime)));
        if let Err(e) = fs::write(&script_file, &s.script) {
            warn!("[SKILL] Could not write the editable copy {}: {e}", script_file.display());
        }
        self.upsert_row(&s)?;
        info!("[SKILL] Saved skill '{}' ({})", s.name, s.title);
        Ok(s)
    }

    pub fn list(&self) -> Result<Vec<JaiSkill>, SkillError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT name, title, description, trigger_phrases_json, parameters_json, runtime, script, usage_count, last_output, updated_at
             FROM skills ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let triggers: String = row.get(3)?;
            let parameters: String = row.get(4)?;
            Ok(JaiSkill {
                name: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                trigger_phrases: serde_json::from_str(&triggers).unwrap_or_default(),
                parameters: serde_json::from_str(&parameters).unwrap_or_default(),
                runtime: row.get(5)?,
                script: row.get(6)?,
                usage_count: row.get(7)?,
                last_output: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get(&self, name: &str) -> Result<Option<JaiSkill>, SkillError> {
        let norm = Self::normalize_name(name);
        Ok(self.list()?.into_iter().find(|s| s.name == norm))
    }

    pub fn delete(&self, name: &str) -> Result<usize, SkillError> {
        let norm = Self::normalize_name(name);
        let count = self.lock().execute("DELETE FROM skills WHERE name = ?1", params![norm])?;
        for ext in ["json", "ps1", "sh", "py"] {
            let _ = fs::remove_file(self.dir.join(format!("{norm}.{ext}")));
        }
        Ok(count)
    }

    /// Longest trigger phrase that appears in the utterance on word boundaries.
    /// The text after the phrase becomes the first parameter (e.g. a search query).
    pub fn match_utterance(&self, utterance: &str) -> Option<(JaiSkill, Value)> {
        let text = utterance
            .trim()
            .trim_matches(|c: char| matches!(c, '.' | '!' | '?'))
            .to_lowercase();
        let padded = format!(" {} ", text);
        let mut best: Option<(usize, JaiSkill, usize)> = None;
        for skill in self.list().ok()? {
            for phrase in &skill.trigger_phrases {
                let p = phrase.trim().to_lowercase();
                if p.len() < 3 {
                    continue;
                }
                if let Some(pos) = padded.find(&format!(" {p} ")).or_else(|| {
                    // Allow punctuation right after the phrase ("take a screenshot, please").
                    padded.find(&format!(" {p}")).filter(|&i| {
                        padded[i + 1 + p.len()..].starts_with(|c: char| !c.is_alphanumeric())
                    })
                }) {
                    let end = pos + p.len(); // index into `text` (padded has one leading space)
                    if best.as_ref().map(|(len, _, _)| p.len() > *len).unwrap_or(true) {
                        best = Some((p.len(), skill.clone(), end));
                    }
                }
            }
        }
        let (_, skill, end) = best?;
        let mut args = serde_json::Map::new();
        if let Some(param) = skill.parameters.first() {
            let rest = text
                .get(end..)
                .unwrap_or("")
                .trim()
                .trim_matches(|c: char| matches!(c, ',' | ':' | '.' | '!' | '?'))
                .trim();
            if !rest.is_empty() {
                args.insert(param.name.clone(), Value::String(rest.to_string()));
            }
        }
        Some((skill, Value::Object(args)))
    }

    /// Runs a skill with `args` exposed as `$env:JARVIS_PARAM_<KEY>` and `{{key}}` substitutions.
    pub async fn execute(&self, name: &str, args: &Value) -> Result<String, SkillError> {
        let skill = self
            .get(name)?
            .ok_or_else(|| SkillError::NotFound(Self::normalize_name(name)))?;
        self.run_skill(&skill, args, true).await
    }

    /// Runs a not-yet-saved skill once (the test step of `create_persistent_skill`).
    pub async fn test_run(&self, skill: &JaiSkill, args: &Value) -> Result<String, SkillError> {
        let mut s = skill.clone();
        s.runtime = normalize_runtime(&s.runtime);
        Self::declare_placeholders(&mut s);
        self.run_skill(&s, args, false).await
    }

    async fn run_skill(&self, skill: &JaiSkill, args: &Value, record: bool) -> Result<String, SkillError> {
        let mut env_pairs = Vec::new();
        let mut script = skill.script.clone();
        if let Some(obj) = args.as_object() {
            for (k, v) in obj {
                let val = match v {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                script = PLACEHOLDER
                    .replace_all(&script, |c: &regex::Captures| {
                        if &c[1] == k { val.clone() } else { c[0].to_string() }
                    })
                    .into_owned();
                env_pairs.push((format!("JARVIS_PARAM_{}", k.to_uppercase()), val));
            }
        }
        let missing: Vec<String> = PLACEHOLDER.captures_iter(&script).map(|c| c[1].to_string()).collect();
        if !missing.is_empty() {
            return Err(SkillError::Invalid(format!(
                "skill '{}' needs parameter(s): {}",
                skill.name,
                missing.join(", ")
            )));
        }

        info!("[SKILL] Running '{}' with {} parameter(s)", skill.name, env_pairs.len());
        let (program, args): (PathBuf, Vec<String>) = if skill.runtime == "python" {
            let interpreter = python().ok_or_else(|| SkillError::Execution("Python is not installed".into()))?;
            (PathBuf::from(interpreter), vec!["-X".into(), "utf8".into(), "-c".into(), script.clone()])
        } else if skill.runtime == "bash" || !cfg!(windows) {
            (PathBuf::from("bash"), vec!["-c".into(), script.clone()])
        } else {
            (
                PathBuf::from("powershell"),
                vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-Command".into(),
                    format!("[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; $ProgressPreference='SilentlyContinue'; {script}"),
                ],
            )
        };

        let output = match crate::shell::run_helper(&program, &args, None, &env_pairs, SKILL_TIMEOUT).await {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(SkillError::Execution(format!("timed out after {}s", SKILL_TIMEOUT.as_secs()))),
            Err(e) => return Err(SkillError::Execution(format!("could not start the {} runtime: {e}", skill.runtime))),
        };
        let mut text = crate::shell::combine_output(&output.stdout, &output.stderr, MAX_OUTPUT_CHARS);
        if text.is_empty() {
            text = format!("Skill '{}' finished (exit code {}).", skill.title, output.code.unwrap_or(-1));
        }
        if record {
            if let Err(e) = self.lock().execute(
                "UPDATE skills SET usage_count = usage_count + 1, last_output = ?1 WHERE name = ?2",
                params![text, skill.name],
            ) {
                warn!("[SKILL] Could not record the run of '{}': {e}", skill.name);
            }
        }
        if output.success() {
            Ok(text)
        } else {
            Err(SkillError::Execution(format!(
                "exit code {}: {text}",
                output.code.unwrap_or(-1)
            )))
        }
    }

    /// Generic forge/run/list/delete tools plus one first-class tool per learned skill.
    pub fn tool_definitions(&self) -> Vec<Value> {
        let os_language = if cfg!(windows) { "powershell" } else { "bash" };
        let mut defs = vec![
            json!({"type": "function", "function": {
                "name": "create_persistent_skill",
                "description": "Permanently learn a reusable automation skill: write the script, test-run it, and save it to ./skills (it becomes one of your tools). Use when Sir asks you to learn, automate or create a skill, or to improve yourself with a new ability.",
                "parameters": {"type": "object", "properties": {
                    "name": {"type": "string", "description": "snake_case id, e.g. 'battery_report'"},
                    "title": {"type": "string"},
                    "description": {"type": "string", "description": "What the skill does"},
                    "language": {"type": "string", "enum": ["powershell", "bash", "python"], "description": format!("Script language (default {os_language})")},
                    "trigger_phrases": {"type": "array", "items": {"type": "string"}, "description": "English and German phrases that should run it"},
                    "script": {"type": "string", "description": "Complete script printing a short result. Parameters: $env:JARVIS_PARAM_<NAME> (PowerShell), $JARVIS_PARAM_<NAME> (Bash), os.environ['JARVIS_PARAM_<NAME>'] (Python) or {{name}}."},
                    "test_args": {"type": "object", "description": "Arguments for the test run"}
                }, "required": ["name", "description", "script"]}
            }}),
            json!({"type": "function", "function": {
                "name": "run_custom_skill",
                "description": "Run one of your learned skills by name.",
                "parameters": {"type": "object", "properties": {
                    "name": {"type": "string"},
                    "arguments": {"type": "object"}
                }, "required": ["name"]}
            }}),
            json!({"type": "function", "function": {
                "name": "list_skills",
                "description": "List all learned skills.",
                "parameters": {"type": "object", "properties": {}, "required": []}
            }}),
            json!({"type": "function", "function": {
                "name": "delete_skill",
                "description": "Delete a learned skill by name.",
                "parameters": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}
            }}),
        ];
        for skill in self.list().unwrap_or_default().into_iter().take(25) {
            let mut props = serde_json::Map::new();
            let mut required = Vec::new();
            for p in &skill.parameters {
                props.insert(p.name.clone(), json!({"type": "string", "description": p.description}));
                if p.required {
                    required.push(p.name.clone());
                }
            }
            defs.push(json!({"type": "function", "function": {
                "name": format!("skill_{}", skill.name),
                "description": format!("Learned skill '{}': {}", skill.title, skill.description),
                "parameters": {"type": "object", "properties": props, "required": required}
            }}));
        }
        defs
    }

    fn seed_starter_skills(&self) -> Result<(), SkillError> {
        let windows = cfg!(windows);
        let starters = [
            (
                "take_screenshot",
                "Take Desktop Screenshot",
                "Captures the primary monitor and saves a PNG to the Desktop.",
                vec!["take a screenshot", "capture the screen", "mach einen screenshot", "bildschirmfoto"],
                if windows {
                    "Add-Type -AssemblyName System.Windows.Forms,System.Drawing\n$b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds\n$bmp=New-Object System.Drawing.Bitmap($b.Width,$b.Height)\n$g=[System.Drawing.Graphics]::FromImage($bmp)\n$g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size)\n$f=Join-Path ([Environment]::GetFolderPath('Desktop')) (\"jarvis_screenshot_\" + (Get-Date -Format 'yyyyMMdd_HHmmss') + \".png\")\n$bmp.Save($f,[System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()\nWrite-Output \"Saved screenshot to $f\""
                } else {
                    "f=~/Desktop/jarvis_screenshot_$(date +%Y%m%d_%H%M%S).png; mkdir -p ~/Desktop; (grim \"$f\" || scrot \"$f\" || gnome-screenshot -f \"$f\") 2>/dev/null && echo \"Saved screenshot to $f\""
                },
            ),
            (
                "top_processes_report",
                "Top Resource Processes",
                "Lists the six processes using the most memory.",
                vec!["top processes", "what is using my ram", "welche prozesse", "was verbraucht ram"],
                if windows {
                    "Get-Process | Sort-Object WorkingSet64 -Descending | Select-Object -First 6 | ForEach-Object { \"$($_.ProcessName): $([math]::Round($_.WorkingSet64/1MB)) MB\" }"
                } else {
                    "ps -eo comm,%mem,%cpu --sort=-%mem | head -n 7"
                },
            ),
            (
                "network_diagnostics",
                "Network Diagnostics",
                "Reports local IPv4 addresses and ping latency to 1.1.1.1.",
                vec!["network diagnostics", "check my internet", "what is my ip", "wie ist mein ping"],
                if windows {
                    "$ip=(Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254*' }).IPAddress -join ', '\n$p=Test-Connection 1.1.1.1 -Count 1 -ErrorAction SilentlyContinue\n$ms= if ($p) { \"$($p.ResponseTime) ms\" } else { 'offline' }\nWrite-Output \"LAN: $ip | Ping 1.1.1.1: $ms\""
                } else {
                    "echo \"LAN: $(hostname -I)\"; ping -c 1 -W 2 1.1.1.1 | tail -n 1"
                },
            ),
        ];
        for (name, title, description, triggers, script) in starters {
            self.save(&JaiSkill {
                name: name.to_string(),
                title: title.to_string(),
                description: description.to_string(),
                trigger_phrases: triggers.into_iter().map(String::from).collect(),
                parameters: Vec::new(),
                runtime: default_runtime(),
                script: script.to_string(),
                usage_count: 0,
                last_output: String::new(),
                updated_at: String::new(),
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_manager() -> (SkillManager, PathBuf) {
        let root = std::env::temp_dir().join(format!("jarvis_skills_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let mgr = SkillManager::open(root.join("db.sqlite"), root.join("skills")).unwrap();
        (mgr, root)
    }

    #[test]
    fn trigger_matching_respects_word_boundaries_and_extracts_argument() {
        let (mgr, root) = temp_manager();
        mgr.save(&JaiSkill {
            name: "Web Search".into(),
            title: String::new(),
            description: "search".into(),
            trigger_phrases: vec!["search youtube for".into()],
            parameters: vec![SkillParameter { name: "query".into(), description: String::new(), required: true }],
            runtime: String::new(),
            script: "echo {{query}}".into(),
            usage_count: 0,
            last_output: String::new(),
            updated_at: String::new(),
        })
        .unwrap();
        let (skill, args) = mgr.match_utterance("Please search YouTube for Rust tutorials!").unwrap();
        assert_eq!(skill.name, "web_search");
        assert_eq!(args["query"], "rust tutorials");
        assert!(mgr.match_utterance("researchyoutube for x").is_none());
        // Umlauts before the phrase must not break slicing.
        assert!(mgr.match_utterance("Größe: search youtube for Öl").is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn undeclared_placeholders_become_parameters_and_disk_is_canonical() {
        let (mgr, root) = temp_manager();
        let saved = mgr
            .save(&JaiSkill {
                name: "set_level".into(),
                title: String::new(),
                description: "d".into(),
                trigger_phrases: vec![],
                parameters: vec![],
                runtime: String::new(),
                script: "echo {{volume_level}}".into(),
                usage_count: 0,
                last_output: String::new(),
                updated_at: String::new(),
            })
            .unwrap();
        assert_eq!(saved.parameters[0].name, "volume_level");
        fs::remove_file(root.join("skills").join("set_level.json")).unwrap();
        mgr.sync_from_disk();
        assert!(mgr.get("set_level").unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn placeholder_names_are_replaced_by_the_title() {
        let (mgr, root) = temp_manager();
        let skill = JaiSkill {
            name: "snake_case_name".into(),
            title: "Greet User".into(),
            description: "greets".into(),
            trigger_phrases: vec!["hello".into()],
            parameters: vec![],
            runtime: String::new(),
            script: "Write-Output 'Hello'".into(),
            usage_count: 0,
            last_output: String::new(),
            updated_at: String::new(),
        };
        assert_eq!(mgr.save(&skill).unwrap().name, "greet_user");
        // An old placeholder file on disk is renamed when the skills are loaded.
        fs::write(root.join("skills").join("snake_case_name.json"), serde_json::to_string(&JaiSkill { title: "Disk Report".into(), ..skill }).unwrap()).unwrap();
        mgr.sync_from_disk();
        assert!(mgr.get("disk_report").unwrap().is_some());
        assert!(mgr.get("snake_case_name").unwrap().is_none());
        assert!(!root.join("skills").join("snake_case_name.json").exists());
        let _ = fs::remove_dir_all(root);
    }
}
