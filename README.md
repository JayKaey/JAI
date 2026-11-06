# J.A.R.V.I.S.

A local voice assistant for Windows in native Rust. Speech recognition (Whisper), voice
activity detection (Silero), speech synthesis (Kokoro), the language model (a local Ollama
engine) and the egui HUD all run on the PC; there is no cloud speech or text-to-speech.

## Build

You need a stable Rust toolchain, the Visual Studio C++ build tools with the Windows SDK, CMake
and libclang (`whisper-rs-sys` compiles whisper.cpp). `build.bat` sets the paths for the PC it
was written on (CMake from Visual Studio, `LIBCLANG_PATH`); change them at its top for yours.

```
build.bat
```

That runs `cargo build --release` and then `tools\package.ps1`, which copies
`target\release\jarvis.exe` to `JARVIS.exe` together with the DLLs it needs.

## First start

The models and runtimes are too large for git and are not in this repository:

- `models\`: Whisper, Kokoro and Silero. `JARVIS.exe --download-models` installs what is missing;
  `--check-models` only reports.
- `runtime\ollama` and `runtime\espeak-ng`: `tools\package.ps1` copies them from local installs of
  Ollama and eSpeak NG when they are missing.
- `bin\cloudflared.exe`: downloaded on first use for the phone page's tunnel (SHA-256 verified).

The app writes `jarvis_config.json` and `jarvis_memory.db` next to the executable. They hold
personal data (settings, the phone pairing key, conversations, sealed account tokens), so git
ignores them, as it does the logs, the phone page's TLS key and the packaged executables.

The film-voice reference recordings are not included either. To calibrate the two voices, put
your own clips at `assets\jarvis_voice_reference.mp3` and `assets\ultron_voice_reference.mp3`
and run `JARVIS.exe --voice-calibrate`.

## Phone page

The mobile page is served over HTTPS and reached through a tunnel. Without any setup that is a
Cloudflare quick tunnel whose address changes on every start. For an address that never
changes, set a permanent address in the Integrations hub (Quick Connections, Worldwide address):
a Cloudflare named tunnel (token plus your own hostname), Tailscale Funnel or your own server.

## Tests

```
cargo test --release
JARVIS.exe --selftest --no-llm     # silent TTS -> VAD -> Whisper check, no language model
JARVIS.exe --live-test             # every probe in the running app; nothing is sent or changed
```

`JARVIS.exe --help` lists all options.
