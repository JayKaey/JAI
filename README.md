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

The film-voice reference recording is not included either. To calibrate the voice, put your own
clip at `assets\jarvis_voice_reference.mp3` and run `JARVIS.exe --voice-calibrate`.

Jarvis speaks Received Pronunciation through a dry studio chain. This is an approximation built
from stock voices, not a clone of an actor. Besides the similarity of the voice to its clip, the
exhaustive verifier measures that the speech recogniser understands it.

## Screens

The HUD is the fullscreen board on one monitor, or one board across all of them ("use all
monitors", "stop spanning", "toggle span mode", the SCREENS menu, F11 to leave). Across several
monitors the HUD proper (header, console, emblem, the board) stays on the primary one; the
others are room for windows, each of which stands whole on one monitor. The emblem rests in the
middle of that screen; it leaves it for a report on the board, for a panel on the centre stage
(below), or when told to "move out of the way" (until "come back").

F11 switches between the fullscreen board and a maximised window. "Make Discord full screen" is
about Discord, "full screen" alone about the window in front, and neither ever about him; "move
Discord to another monitor" takes it to the next one.

Jarvis starts with an empty HUD. "Open that widget again next time" keeps one for the next
start; "bring back my windows" reopens what was open last, also after a restart.

## Panels

Three panels open by voice, as a block on the board or in a window of their own ("… in a new
window"), and close with "close the radar", "close the disk map", "close the globe". On the
board a panel takes the centre stage when nothing else stands there, and the emblem moves to its
corner until the panel closes or is dragged away:

- **Network radar**: "check the network", "who's on my wifi?", "scan my network again" sweep
  the local network and place every device that answers; "is my network secure?" looks over
  what the sweep found and says what stands out.
- **Disk map**: "what is taking up space?" maps the system drive as a treemap, "map my D drive"
  another drive or folder; clicking a box goes into it, a right-click, BACK, the breadcrumb or
  "go up a folder" comes back out, and when the reading is done Jarvis says in two sentences
  what fills it.
- **Earth**: "show me the earth" opens the globe, "switch to satellite view" and "go back to
  the hologram" change its face, "fly to Tokyo" turns it. On it: "were there any earthquakes
  near Tokyo recently?", "show me the flights over New York", "show me the storms", "show me
  the fires", "where is the space station?", and "hide the flights" takes a layer off. The
  globe fetches public data while it is open (NASA GIBS imagery, USGS and EONET events, the
  ISS position, OpenSky flights).

Every window of his opens with a holographic entrance (the frame draws in, brackets close,
a scan line passes) and leaves with an exit (it folds into a line), whether it is opened or
closed by voice, by its CLOSE chip, by Ctrl+W, or as a window of its own. Asked for again while
it is leaving, it stays.

A pill at the top of the HUD says what he is doing: LISTENING, THINKING, SPEAKING, MUTED or
STANDBY. While Sir speaks, his voice shows as a ring of ticks round the emblem. Both can be
switched off in the VIEW menu. F1 (or M) mutes the microphone; the key hints at the bottom right
can be clicked.

"Explain what's on my screen" (and every other look at the screen) leaves his own windows out
of the capture, so he reads what is behind them; they stay on the monitor (Windows 10 2004 or
later). The HUD frosts over for a moment while he looks, and a card under the emblem names what
he saw ("VIDEO DETECTED", "ERROR DETECTED", "CODE DETECTED") with three quick actions.

A file dropped on the HUD becomes the file of the conversation: "summarize this file" or "what
is in this file?" are about it. The console shows which file is loaded.

## Volume

Every volume request is carried out on the Windows mixer without the language model: the
master volume, one app's own ("lower my browser's volume by 50 %", "Discord to 80 %"), "it" for
what was just changed, "mute everything except Discord", and "make everything loud again",
which undoes his own changes. A level with a sign is always a step, never a level.

"Save this mix as gaming" keeps the master volume and every app's own level under that name
(`config/audio_mixes.json`); "load my gaming mix", or just "gaming mix", brings them back in one
go, and "undo that" takes all of it back. "Which mixes do I have?" and "delete the gaming mix"
do what they say.

Right after a change, for 30 seconds and until anything else is asked, short answers go on from
it: "more", "a bit more", "less", "too much", "the other way", "up a bit", "same for Firefox",
"and Discord too". "Make Discord loud again" puts back one app; "solo Discord" mutes the others.
"Make it quiet" and "make it loud" go to a level, never past it the wrong way; "it's very quiet"
is one step up on what was just changed.

"Mute Discord for 10 minutes" ends by itself (a mute or unmute in between takes the timer's job
away). "Fade out the music" takes five seconds in the background, "fade it back in" returns to
where it was, "turn it down slowly" is a step taken the same way, and any volume change he asks
for meanwhile ends a fade. "Make Firefox as loud as Discord" and "make Firefox quieter than
Discord" read the other app's level off the mixer. "Set all apps to 100 %" (or "every volume",
"all the volumes") sets their own levels and leaves the master alone; "my PC's volume" is the
master. Questions ("is the sound on?") only ever read. Which device plays is
not his to change: "switch to my speakers" says so, "which audio device am I using?" tells.

"Open the mixer" puts a block with a fader for every channel on the board: the master first, then
each app that has a session. "I can't hear anything" (or "there's no sound", "ich höre Discord
nicht") looks for the sound: a muted master, a muted or nearly silent app, and says where it went.
"Is anything muted?" names only what is.

"Turn the game down" finds whatever game has a channel, listed or not; the background agents of
hardware makers (Logitech, Razer, Corsair and the like) are never taken for one. "Turn Firefox
off" closes a program and "turn Firefox back on" opens it again; these two sentences work for
real programs only, never for the PC or the sound.

## Widgets by voice

Notes, lists, clocks and charts go up without the language model: "add a note saying call Tom at
5", "make a shopping list with milk and eggs", "add butter to my shopping list", "check off milk",
"start a stopwatch", "pause the timer", "show me a CPU chart". "The note", "the timer", "the list"
mean the latest one of that kind. "… in a new window" gives a note a window of its own, and "close
the note" closes that too. "What's open?" names his board, blocks, windows and widgets.

Questions are looked up, not guessed at: "is Firefox open?", "läuft Discord?", "is the mixer
open?", "how long is left on the timer?", "is my mic muted?", "why is it so quiet?", "are you in
orb mode?", "did you mute Discord?". And a plain question only reads: whatever the language model
reaches for beyond reading ("what happens if I close Firefox?") is left undone and it is told so,
and a question about the PC's power ("should I lock my PC?") never starts it. "How do I mute
Discord?" and "should I close Firefox?" are answered with the words that do it, and do nothing;
"what happens if I close Firefox?" says what closing it costs; "what is orb mode?" and "what does
the mixer do?" are told as they are.
"Can you close Firefox?" and "would you mind opening the mixer?" are orders in good manners and
are carried out. A thing he is told not to do stays undone: "don't close it", "schließ das nicht"
get a plain "I'll leave it".

"Close it" means the last thing of his that came up (a window, a widget, the mixer, the Theme
Studio) and only then the program the conversation is about. "Get rid of that" (or "weg damit")
is about his own things alone: with none up he says so, and no program is touched.

## PC power

"Shut down my PC" (or "put my PC into shutdown mode") and "restart the whole system" ask once and
run on a yes within 30 seconds; anything else said meanwhile calls it off. "Put my PC to sleep"
and "lock my PC" run straight away. A "yes" that comes after the question has lapsed runs
nothing, and for two minutes he says that it lapsed and which words start it again. "What
happens if I restart my PC?" is told what that does (what closes, what is lost), "how do I shut
down my PC?" the words that do it, and none of these questions starts anything.

## Undo

"Undo that" (or "change it back", "put it back", "mach das rückgängig") takes back the last thing
he changed of his own, for ten minutes after it: a volume, a window or widget of his that opened
or closed, the look and layout of the HUD, the music he stopped or skipped, a file he edited. One step, the latest; a second "undo that" finds nothing rather than redoing. Nothing of
anybody else's (another program's window, a sent message) comes back this way. "What did you just
do?" tells his last actions as they went, and "what's new?" what this build does that the last one
did not.

## Phone page

The mobile page is served over HTTPS and reached through a tunnel. Without any setup that is a
Cloudflare quick tunnel whose address changes on every start. For an address that never
changes, set a permanent address in the Integrations hub (Quick Connections, Worldwide address):
a Cloudflare named tunnel (token plus your own hostname), Tailscale Funnel or your own server.

## Jarvis's own Discord account

Jarvis can have a Discord account of his own: a bot you create once.

1. On discord.com/developers, make a New Application. On its Bot page, copy the token (Reset
   Token) and switch on the Message Content Intent. Switch on the Presence Intent too if he
   should tell you who comes online.
2. Under OAuth2 → URL Generator, tick the scope `bot` and the permissions View Channels, Send
   Messages, Read Message History, Connect, Speak and Use Voice Activity. Open the link it gives
   and invite him to your server.
3. Paste the token into the Integrations hub (Quick Connections, Jarvis's Discord account) and
   save. The token is sealed in the vault, never in the settings file.
4. Say "pair my Discord" (or press Pair my account) and send him the six-digit code as a direct
   message. From then on your DMs to him are orders from anywhere ("what's my GPU temperature",
   "lock my PC"), answered in the DM.

Friends who DM or mention him get a short answer in character (never a tool, never your private
things), one every five seconds and twenty an hour per person at most. "Join my call" puts him
into your voice channel as himself, end-to-end encrypted (DAVE): he hears everyone, answers
whoever says his name, and what he says goes into the call. What he hears stays in memory for 30
minutes and is never written down; the first time in a channel he says so in its chat. "What did
they say?", "summarise the call", "who's in the call?", "only listen", "leave the call". "Write in
#general that I'm late" asks first; "what did they write on Discord?" reads it out. Bots can join
server voice channels, not private calls.

## Away Mode

"I'm heading out" (or "ich bin weg", "away mode on") and he minds the PC; it also turns on by
itself after the PC has been locked for ten minutes (Quick Connections, Away Mode). While it is on,
friends' Discord and WhatsApp messages are kept for you (a Discord DM gets the answering machine
once), and every alert (a reminder, the Guardian, an urgent WhatsApp, a finished download, a drive
plugged in, someone using the PC after you said you were leaving) also reaches your phone as a DM
from his account. When you're back (you say so, or unlock the PC after an automatic Away Mode) he
says how long you were gone and what came in; "what did I miss?" goes through it.

"Tell me when my download is done", "tell me when Blender is finished" and "tell me when GitHub is
back up" are watched until they happen (eight at a time, twelve hours at most).

## Tests

```
cargo test --release
JARVIS.exe --selftest --no-llm     # silent TTS -> VAD -> Whisper check, no language model
JARVIS.exe --live-test             # every probe in the running app; nothing is sent or changed
JARVIS.exe --live-test --headless  # the same with the HUD drawn offscreen (no window)
JARVIS.exe --exhaustive 5          # the live test plus the exhaustive matrices, five runs in a row
JARVIS.exe --silent --chat "..."   # ask him for real, unseen and unheard; answers go to jarvis.log
JARVIS.exe --silent --show --monitor 2 --chat "..."   # the same in the real window, on monitor 2
JARVIS.exe --live-test --monitor 2   # the live test with its window on monitor 2
JARVIS.exe --silent --show --window-drill --chat "What time is it?"   # SCREENS buttons and F11 in the real window
```

`--monitor <n>` opens a test session's window on monitor n (1 = the primary) for that session
only, so a test never lands on the monitor you are playing on. A window of its own without a
place on the board opens on that monitor too. A test session never moves the focus: its board
covers the monitor as a borderless window rather than winit's fullscreen (which forces a new
window to the front with an Alt tap), and none of its windows asks to be brought forward.

`--window-drill` clicks every SCREENS button (each monitor, the span and back, CLOSE) and
presses F11 out of and back into the fullscreen board, in the real window, one step at a time,
and checks where Windows really put the window after each (`[DRILL]` lines in `jarvis.log`).
The clicks go into the window's own input and F11 is a key message to the window, so the cursor
and keyboard are never touched. The checks that change real things on the PC (the real mixer, a
shutdown that is scheduled and cancelled, the lock screen, voice samples) are ignored tests in
`src/live_checks.rs`, each behind its own environment switch.

`--silent` (with one or more `--chat` requests) is a scripted session through the real
pipeline, language model included, that nobody sees or hears and that leaves no trace in the
conversation log or the settings. It acts inside Jarvis only: reads and his own HUD are carried
out, the volume works on a copy of the mixer, and anything that would reach the PC around him
(an application, the music, another window, a message) is logged and left undone. `--show` runs
it in the real window.

`--exhaustive` needs no window and makes no sound: the real HUD is drawn into a private egui
context and inspected as geometry (text against clip rectangles, blocks against each other and
the orb, stroke colours against the theme, animations frame by frame) at four screen sizes.
Every command family goes through the real dispatcher in five passes (English, German, misheard,
four commands spoken over Jarvis, compound sentences), both voices are measured against their
clips and against each other, and every Feature Lab button is pressed five times. What Sir says
to the screens, his windows and the volume is also said aloud as samples: two stock voices at
speech level over room hiss, through Whisper, the wake word and the dispatcher. They stand in
for a person; they do not show how his own accent and microphone fare. The microphone is deaf for
the whole run, whatever its switch says. Requests are dry runs: tool calls are recorded, not
carried out, except reads and what only draws inside the HUD. The settings file and the
conversation log stay untouched. The report is `exhaustive_report.json`.

A test session runs where nobody sees it, so it gives way: starting JARVIS.exe while one is
running ends it and starts him normally. Its tray tip says so, and a run cut short exits with a
failure code.

Time budgets are for the running app, not for its start: once per process the pipelines, the
routes and the read-only tools are gone through first without being counted (the first time
anything runs costs two to three times what it costs afterwards), and the time is taken from
the second pass on. Only the time is forgiven: a wrong result in that first pass fails the run
that follows, and the probes that were late in it are named in the log and in the report
(`first_pass_late`).

`JARVIS.exe --help` lists all options.

## Workbench

Ten everyday tools with voice routes in English and German: spaced-repetition flashcards, a
maths/physics/chemistry formula sheet, molar masses and mole conversion, keyboard-shortcut
sheets for the program in front, screen time with a break coach, routines learned from what is
asked at the same time on several days, a thermal advisor with a fan-curve suggestion,
clipboard history (in memory only, secrets never kept), tiling program windows on a monitor,
and predictive command completion in the text console (Ctrl+K).

Every block on the board has a right-click menu (copy, pin, maximize, export to Markdown or
JSON, close), Ctrl+F searches across all open blocks, and new blocks are placed so that they
never overlap each other or the orb.
