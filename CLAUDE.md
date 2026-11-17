# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

J.A.I. ("Jay") is a local, offline voice assistant for Windows. It is a single Rust binary crate (`jai`, ~250 modules flat in `src/`, no workspace). Whisper does speech-to-text, Silero does voice detection and Kokoro does speech. A bundled Ollama engine runs the language model, and the HUD is native egui. `README.md` describes what each spoken order must do, so treat it as the behaviour spec.

## Working rules (every task)

The owner states only the task. Follow these rules on every task without being told again.

1. **Pure Rust.** Everything is native, compiled Rust in `src/` and `build.rs`, built through `Cargo.toml`. That covers UI, animation, 3D, voice, VAD/STT, audio routing and tests. Use no Python, JavaScript/Node, HTML/Electron wrappers or cloud APIs, and no throwaway Python patch scripts either. Everything runs locally and offline. The phone page served by `satellite.rs` is the only browser-side code.
2. **Assumptions first.** Inspect surgically: `graphify query` or `Grep` first, then `Read` with `offset`/`limit` of 40–80 lines. Never read a large `.rs` file top to bottom. Before changing code, post a short bulleted list of **Technical Assumptions**. If any part of the task is unclear, ask before writing code. Otherwise, go straight on.
3. **Minimum code.** Make the smallest `Edit` diffs and reuse existing helpers. Remove dead, unused or duplicate code you find in the modules you touch. Write no chatter between tool calls; when the app asks for a status, give one short line.
4. **Reproduce before fixing.** For every reported bug, first write a Rust test or probe that reproduces it and shows why it happens. Then fix it minimally. Also fix adjacent visual or logic glitches you spot in the same modules.
5. **Verify in several scenarios.** Test each fix and feature across edge cases. Where they apply, include visual layout and geometry checks (offscreen HUD, `ui_snapshot`) and phonetic or STT variations (misheard, German, compound sentences). Repeat `cargo test --release` and `cargo build --release` until both pass with 0 errors and 0 warnings.

Finish every task with this checklist:
1. Update JAI's changes page in `src/changes.rs`. Add a line to `RELEASES` under today's date, newest day first, one line per entry, using `added` / `better` / `fixed`. A feature that needs explaining uses `new(…)` plus an `EXPLANATIONS` entry. Then run `cargo test --release changes`, which checks the links.
2. Run `graphify update .` once, at the very end, not after every edit.
3. Give a four-sentence summary.
4. Commit to the current branch and push it to `origin`; the owner has given standing permission. Before you push:
   - Run `git grep --cached -n -i -E -f .claude/private-words.txt` (the untracked word list) and check new fixtures for real names, places, logins, IDs or tokens. Read each hit, and don't push personal data. Two known hits are harmless: a crate name in `Cargo.lock` and a video file extension in `disk_map.rs`.
   - Push with `GCM_INTERACTIVE=never GIT_TERMINAL_PROMPT=0`, so a missing login fails instead of opening a sign-in window.
   - Never force-push, and never push `claude/*`, `worktree-agent-*` or `refs/original/` refs.

## Build and test

- `build.bat` runs `cargo build --release`, then `tools\package.ps1`. The script copies `target\release\jai.exe` to `JAI.exe` with its DLLs. It refuses while `JAI.exe` is running, and it fails if the exe is a console-subsystem build.
- Plain `cargo` outside `build.bat` fails inside `whisper-rs`, because bindgen needs libclang. In Git Bash, from the repo root, run this first:
  `export PATH="/c/Program Files/Microsoft Visual Studio/18/Community/Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin:$PATH" LIBCLANG_PATH="$(pwd -W)/tools/libclang"`
- `cargo test --release` runs the whole suite (~1,300 `#[test]`s inline in the `src/` modules; `tests/fixtures/` only holds data). To run one test: `cargo test --release <name_substring> -- --nocapture`.
- `src/live_checks.rs` holds `#[ignore]`d tests that act on the real PC (mixer, master volume, shutdown, lock screen, vault). Each needs its own env var (`JAI_REAL_AUDIO`, `JAI_REAL_MASTER`, `JAI_REAL_POWER`, `JAI_REAL_LOCK`, `JAI_EXE`, `JAI_VAULT_DB`). Run one with `cargo test --release <name> -- --ignored`.
- `.cargo/config.toml` forces `-O2` for whisper.cpp; without it, speech recognition is about 20× slower. After you change those flags, run `cargo clean -p whisper-rs-sys --release`.
- Worktrees under `.claude/worktrees/` share this `target/`, so cargo can call your checkout "Fresh" from another checkout's build. While others build, pass `--config profile.release.package.jai.codegen-units=<a number of your own>`, then confirm the `Compiling jai v… (<your path>)` line.

The packaged exe has test modes too (`JAI.exe --help` lists every flag; the README's "Tests" section explains them):
- `--selftest --no-llm` checks TTS → VAD → Whisper with no language model.
- `--silent --chat "…"` (repeatable) runs the real pipeline with the LLM. It has no window, sound or microphone, and it leaves the settings file and the conversation log alone. Answers go to `jai.log` as `[CLI]` lines. `--silent --show --monitor 2 --chat "…"` does the same in the real window.
- `--live-test [--headless]` and `--exhaustive [n]` run every probe in the running app as dry runs. The exhaustive report goes to `exhaustive_report.json`.

The owner uses the PC while tests run. Put test windows on `--monitor 2` and never take the focus. Anything that acts outside JAI (other apps, media players, power, the real mixer) goes behind the guards listed below.

## Architecture

**Voice in.** `ears.rs` runs two threads. `jai-mic` is a cpal callback that pushes into a lock-free ring. `jai-vad` resamples to 16 kHz, runs Silero and the `VadMachine`, and emits utterances. The same file decides whether speech is meant for JAI at all (addressee detection, the background-speech filter). `wake.rs` matches the name; `wake::canon` rewrites "…, Jay" to "JAI, …" for the fixed-phrase parsers. `stt.rs` runs Whisper on its own priority worker thread.

**Orchestration.** `app.rs` (`Jai`) wires audio → STT → wake gating → brain → TTS. It also runs the idle / listening / thinking / speaking state machine, stitches a turn that was split by a pause, and handles `HudCommand`s from the window (`process_utterance`, `dispatch_turn`, `handle_command`).

**Routing.** `brain.rs` `Brain::run_turn` hands a request to the LLM only if no deterministic route claims it. The order, roughly:
1. Instant reflexes (stop, mute, next).
2. `typos` normalisation.
3. `ear` and `did_you_mean` corrections.
4. `ways` rewording to an order's canonical wording (`brain::another_way`).
5. `later`.
6. Tool Forge triggers.
7. `features::route_text` and `integration_router::route_text`.
8. `intents::parse`, `parse_sentences`, `parse_misheard` and `resolve_fuzzy`.
9. `llm_loop` with native tool calling, streamed.

`intent_engine::route` is the reflex router for JAI's own controls. `hyper_cortex` picks the tier (reflex, tactical or deep), starts reads early on a speculative transcript, and runs read-only tools in parallel. `phrasings::route(text)` tells you where a sentence lands without the LLM. Use it, and the phrasings corpus test over `tests/fixtures/phrasings_*.json`, when you add or change an order.

**Tools and the prompt budget.** `tools.rs` is the one registry: JSON schemas plus `ToolContext::execute`, shared by the LLM, the fast path and the UI. The system prompt and the tool schemas must fit `num_ctx`. The tests `the_whole_prompt_fits_the_context_window` and `rare_tools_wait_behind_the_door` in `brain.rs` enforce this. Rare tools sit in `tools::BEHIND_DOOR`, and the `kit.rs` / `features.rs` features are fast-path only on purpose. Give a new capability a fast-path route rather than a new LLM tool schema.

**LLM requests.** The brain keeps a byte-stable prompt prefix so Ollama reuses its KV cache. Per-turn live context rides on the newest user message and never enters the system prompt or the history. `runtime.rs` runs the bundled `runtime/ollama` as a hidden child in a job object, with a private port and store (`models/llm`). `vram_guard.rs` takes every model off the GPU while a game runs or after idling (Alt+J wakes JAI), and game mode moves the brain to the CPU.

**Voice out.** Streamed tokens pass output hygiene in `brain.rs`: the meta filter, the language lock, then the repetition gate. `tts.rs` takes it from there: clause splitter → eSpeak-NG phonemes (`runtime/espeak-ng`) → Kokoro ONNX → `jai_voice_dsp` → cpal. Barge-in bumps a generation counter.

**HUD.** `gui.rs` (`Hud`, `HudModel`) runs egui/glow on the main thread. A background task applies backend events to the shared `HudModel`, so nothing is lost while the window is hidden. `hud_board.rs` holds the board blocks, `hud_dock.rs` the in-app blocks, and `interface.rs` the look settings (the `configure_interface` tool); there is one `gui_*.rs` per panel or window. `gui_offscreen.rs` and `ui_snapshot.rs` draw the real HUD into a private egui context for tests.

**State.**
- `config.rs` reads and writes `jai_config.json`. Every field has a serde default. To change a tuned default for existing installs, bump `CONFIG_VERSION` and add a step to `migrate()`.
- `memory.rs` is `jai_memory.db` (SQLite with WAL and FTS5). The feature modules keep their tables there too.
- `vault.rs` keeps AES-GCM sealed secrets, with the key in the Windows Credential Manager.

**Test-session guards.**
- `platform::in_test_session()`: a scripted session is running.
- `platform::hands_off()`: true under `cfg(test)` as well. Code that touches the owner's media players obeys it.
- `live_self_tester::dry()`: tool calls are recorded, not carried out.

Code that acts outside JAI, writes the settings file, or writes the conversation or routine logs must check these guards.

## Conventions and traps

- Line endings: `.gitattributes` is `* -text`, and files mix CRLF and LF. The Edit tool keeps each file as it is, but `sed -i` in Git Bash strips every CR. After any scripted edit, check `git diff --numstat` for whole-file churn.
- Bash heredocs mangle backslashes (a regex `\b` became 0x08). Write patch content with the Write tool.
- Values fed into a cipher, hash or stored id must never change in a rename sweep. That covers `vault.rs` `KEYRING_SERVICE` and `Vault::TAG` (sealed secrets), the `STOCK_*` fingerprints in `config.rs` (`config::fingerprint`) and the zlib test bytes in `mesh_io.rs`. Renaming them once made the real vault unreadable.
- The assistant is "J.A.I." / "JAI" in UI text, prompts and identifiers. His former name must not reappear in tracked files.
- Keep no personal data in tracked files, even though the remote is private. Use placeholder names in fixtures and tests: `Max Mustermann`, `Musterstadt`, `12345`, `myschool.webuntis.com`. Personal values live only in the untracked `jai_config.json` and `jai_memory.db`. Invent test data rather than copying it from the owner's screenshots or logs.

## graphify
This project has a knowledge graph at `graphify-out/` with god nodes, community structure, and cross-file relationships.

Rules:
- For codebase questions, first run `graphify query "<question>"` when `graphify-out/graph.json` exists. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than `GRAPH_REPORT.md` or raw grep output.
- If `graphify-out/wiki/index.md` exists, use it for broad navigation instead of raw source browsing.
- Read `graphify-out/GRAPH_REPORT.md` only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost): once, at the end of the task.

## Token & Sub-Agent Rules
- Do NOT spawn sub-agents (`Task` tool) for simple edits, single-file lookups, or routine tasks—handle them directly in the main thread.
- When sub-agents are strictly necessary for parallel research, always pass `model: "sonnet"` or `model: "haiku"` instead of inheriting Opus/Fable.