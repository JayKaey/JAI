# J.A.I.

A local voice assistant for Windows (and Linux) in native Rust. Speech recognition (Whisper), voice
activity detection (Silero), speech synthesis (Kokoro), the language model (a local Ollama
engine) and the egui HUD all run on the PC; there is no cloud speech or text-to-speech.

His name is J.A.I., said "Jay". He answers to it said or spelt a little wrong ("Jai", "Jae",
"J A I", "Hey Jay"); what the letters stand for is yours to set (`name_meaning` in
`jai_config.json`). His files carry his name too: `JAI.exe`, `jai_config.json`,
`jai_memory.db`, `jai.log` and his folders (`Documents\JAI Models`, `Documents\JAI_Research`,
...).

## Build

You need a stable Rust toolchain, the Visual Studio C++ build tools with the Windows SDK, CMake,
Ninja, libclang and the Vulkan SDK (`whisper-rs-sys` compiles whisper.cpp with its Vulkan path,
`VULKAN_SDK` set). `build.bat` sets the paths for the PC it was written on (CMake and Ninja from
Visual Studio, `LIBCLANG_PATH`); change them at its top for yours.

```
build.bat
```

That runs `cargo build --release` and then `tools\package.ps1`, which copies
`target\release\jai.exe` to `JAI.exe` together with the DLLs it needs.

## First start

The models and runtimes are too large for git and are not in this repository:

- `models\`: Whisper, Kokoro and Silero. `JAI.exe --download-models` installs what is missing;
  `--check-models` only reports. Whisper hears on the CPU from the first second and moves to the
  graphics card (any make, through Vulkan) with the larger `ggml-large-v3-turbo-q5_0.bin` once
  that has loaded, when the card has room for it beside the brain; a game sends it back to the
  CPU until the game is closed. When the card is full (other programs need it) and he has been
  quiet for two minutes, every model leaves it; his name brings him back, with the brain in RAM
  while the card is still full.
- Instant Replay ("clip that") records the screen only once its rolling buffer is
  switched on in the Integrations window.
- `runtime\ollama` and `runtime\espeak-ng`: `tools\package.ps1` copies them from local installs of
  Ollama and eSpeak NG when they are missing.
- `bin\cloudflared.exe`: downloaded on first use for the phone page's tunnel (SHA-256 verified).

The app writes `jai_config.json` and `jai_memory.db` next to the executable. They hold
personal data (settings, the phone pairing key, conversations, sealed account tokens), so git
ignores them, as it does the logs, the phone page's TLS key and the packaged executables.

The film-voice reference recording is not included either. To calibrate the voice, put your own
clip at `assets\jai_voice_reference.mp3` and run `JAI.exe --voice-calibrate`.

JAI speaks Received Pronunciation through a dry studio chain. This is an approximation built
from stock voices, not a clone of an actor. Besides the similarity of the voice to its clip, the
exhaustive verifier measures that the speech recogniser understands it.

To have him start whenever you log in, say "start with Windows" ("starte mit Windows"), tick
"Start with Windows" in the tray menu, or switch it on in the settings (PRESENCE). That is a `JAI`
entry of your own under Windows' Run key, so Task Manager's Startup page shows it too (on KDE:
`~/.config/autostart/jai.desktop`). "Don't start with Windows any more" takes it out again.

## The look

Everything he shows is light drawn on black glass: white structure, and one colour for what is
live, selected or measured. The canvas is always black; the colour is the theme.

- **Themes.** Eight ready-made ones: Stark Gold (his yellow, the default), Mono (black and
  white), Arc Reactor, Hot Rod, Gamma, Vibranium, Ember and Graphite. "Switch to the arc reactor
  theme", "make the interface red" (a colour that names a theme is that theme, any other colour
  becomes a light of its own), "make it darker" (Graphite) and "a bit lighter" (Mono). "Create a
  new theme" opens the Theme Studio; "save this theme as …" and "load my … theme" keep and bring
  back his own.
- **Two cores.** The simple one is an instrument of rings whose scale is the clock; the complex
  one is a sphere of circuitry whose shells light up with the part of his mind that is working.
  "Switch to the complex core", "use the simple core", "make the orb bigger", "set the orb size
  to 150 percent".
- **Windows.** Everything he opens is a window of Windows itself, never a block on the board
  behind your icons. A new window takes a place about the core: beside
  him, the next on the other side, later ones above and below ("overlap the windows" opens them
  in the middle instead). A line ties each window to the core, and its header wears its number
  among the open ones, its title and a lamp for what it is doing. A window you have had before
  opens where it last stood. Every one of them is sized by hand at its edges and corners.
- **His extra windows** take the HUD's theme unless told otherwise: "make that window red",
  "make that window arc reactor", "make that window transparent", "pin that window".
- **What steps back.** "Calm mode" lets the readouts at the top and bottom step back until the
  mouse comes near; "show everything" brings them back. "Close the chat" and "show the chat"
  take the conversation block above the bottom strip away and bring it back: "the chat" is
  never the chat application. "Hide everything" is about the desktop (it minimises every
  window, as "minimize everything" does), or about the Earth's layers while the Earth is open.
- **Settings.** "Open settings" is one panel with a search field. Every setting has a row there
  with a line that says what it does: the theme, the core and its size, text size, zoom,
  opacity, the window's size, and every switch.

## Screens

He is your desktop's background, always and on every PC, from the first start: the board stands
behind your icons, across every monitor or on the first one alone ("use all monitors", "stop
spanning", "toggle span mode", the SCREENS menu). He has no window of his own to hide, minimise
or move, and says so when asked. Across several monitors the HUD proper (header, console,
emblem) stays on the primary one; the others are room for windows, each of which stands whole
on one monitor. The emblem rests in the middle of that screen; it leaves it for a report on the
board, or when told to "move out of the way" (until "come back"); "move yourself to monitor 2"
and the SCREENS menu's MOVE HERE send it to another monitor. Should the desktop not take him
(Explorer still starting), he lies under every other window and asks it again every half minute.

Spoken to while a window covers his emblem, his orb comes out to a corner of the monitor you are
using and glides back when you are done. "Orb mode" keeps his orb floating in front of your work
at its corner (drag it anywhere; the mouse wheel resizes it), "mini mode" a slim strip along the
top. A double click on an orb, or BOARD on the strip, shows the desktop.

Your own pictures and videos can lie on his black: a picture (PNG, JPEG, WebP), an H.264 MP4
(silent unless you ask for its sound) or a folder of pictures as a slideshow, for every monitor
or for one ("set my background to D:\Pictures\sunset.png", "use my Pictures folder as a
slideshow on monitor 2", "change the slideshow every 2 minutes", "background video with sound",
"remove my background picture"; the same in the Background window, "open the background window",
with FILE, FOLDER or NONE for each monitor, and in the SCREENS settings). Behind the board they are
dimmed so the board reads; as the whole background ("fill the whole background") the board
fades out and comes back when you speak to him or say "show the board". A game in front, a
locked PC or a window covering the whole monitor pauses them.

His black can also let your own wallpaper through, Wallpaper Engine's too: "make your background
transparent" ("mach deinen Hintergrund durchsichtig"), "make your background solid again", or
the Opacity row in the settings (anything under 100). On Windows the black is then gone
altogether, never half, and his core is drawn sharp without its glow, so no dark seam rims it;
on Linux (KDE Plasma) it fades as far as the Opacity says. The display finish's grain stays off
meanwhile. Wallpaper Engine unloads its wallpaper when less than a fifth of the graphics card is
free, so while his black is see-through and he has been quiet for two minutes his models leave
the card for it (the first answer after that takes longer while they load again).

"Make Discord full screen" is about Discord, "full screen" alone about the window in front, and
neither ever about him; "move Discord to another monitor" takes it to the next one. A window he
opens or moves goes where it covers none of the others (on a full monitor, where it covers the
least), and a moved one slides there quickly instead of jumping.

JAI starts with an empty HUD. "Open that widget again next time" keeps one for the next
start; "bring back my windows" reopens what was open last, also after a restart.

## Panels

Three panels open by voice, each in a window of its own, and close with "close the radar",
"close the disk map", "close the globe":

- **Network radar**: "check the network", "who's on my wifi?", "scan my network again" sweep
  the local network and place every device that answers; "is my network secure?" looks over
  what the sweep found and says what stands out.
- **Disk map**: "what is taking up space?" maps the system drive as a treemap, "map my D drive"
  another drive or folder; clicking a box goes into it, a right-click, BACK, the breadcrumb or
  "go up a folder" comes back out, and when the reading is done JAI says in two sentences
  what fills it.
- **Earth**: "show me the earth" opens the globe, "switch to satellite view" and "go back to
  the hologram" change its face, "fly to Tokyo" turns it and goes down until the town is in
  view to its outline (a small town from lower than a large one). On it: "were there any earthquakes
  near Tokyo recently?", "show me the flights over New York", "show me the storms", "show me
  the fires", "where is the space station?", and "hide the flights" takes a layer off. The
  globe fetches public data while it is open (NASA GIBS imagery, USGS and EONET events, the
  ISS position, OpenSky flights). Place names and borders show on both faces (Esri). From a
  town's outline down, the hologram draws its streets by rank, its rails, rivers and lakes,
  and the streets' names along them (OpenFreeMap; the names go with "hide the labels"). Down at
  street level the houses of any town in the world that is on OpenStreetMap rise on the map
  as holograms, traced on the ground and raised in a wave from the middle of the view, with
  their heights (estimated from a house's ground where the map gives none), a line for every
  storey, windows up close, a tower's height scale, hipped roofs on small houses (a guess: the
  map knows no roof shapes), their house numbers and the height of every tower (OpenFreeMap): "show me the houses of Musterstadt in 3D" flies there,
  "show me the buildings" goes down where the map is, "hide the buildings" takes them off.
  The wheel and "zoom in" come down to 30 m, a house filling the window (the satellite photo
  is sharp to about 250 m and drawn larger below; a house reaching over the camera steps
  aside). Near the ground the map leans back into 3D as Google Earth does: the 3D key, "switch to 3D"
  or "tilt the map", or a drag with the right mouse button (up and down tilts it, across turns
  it); "back to 2D" or the key again stands it straight down, north up. While the globe is up,
  "show me the Statue of Liberty", "zoom to the Empire State Building" or "fly me to the Golden
  Gate Bridge" flies to any landmark OpenStreetMap knows by name (its best-known one, before a
  town of the same name): a building or a square close up among the houses in 3D, the one asked
  for brightest, a tower from as far back as its top stands in view (up to ~450 m), a bridge or
  a mountain whole.

"Open a few random windows" leaves the choice to him: he opens two to four of his own views
and says which ("open three random windows" opens three). Asked for his opinion he gives one,
in English unless the sentence itself asks for German.

An unclear "show me" is his to decide, and he says what he chose: "show me the moon" or "zeig
mir ein Herz" opens a 3D model, "show me waves" or "show me how a pendulum swings" a simulation,
"show me a sine wave" a plot, anything else a picture ("show me a volcano"); "draw a cat" gets a
picture too, and he says he can't draw. What is Sir's own (his calendar, a program, a window, a
place on the open globe) keeps its own order. Where no tool of his can do a thing, he says so and
asks "Shall I build a tool for it?"; a yes builds it in the Tool Forge.

An "open …" that finds no program is not the end of it: when one order of his own is close and
all it does is open or show something, he carries it out and says what he took it as ("open the
backgrounds window" opens the Background window). A program is still never started on a
guess.

The Simulation Lab works the physics out itself: a thrown ball, a pendulum and a double pendulum,
collisions, bouncing balls, a mass on a spring, orbits ("simulate the sun, the earth and a
comet", "zeig mir das Sonnensystem"), ripples ("show me ripples with a single slit"), standing
waves, the Doppler effect and a sonic boom, sound waves, light through a double slit ("simuliere
den Doppelspalt"), a formula as a curve or as a surface in 3D ("plot z = sin(x) * cos(y)"),
gradient descent and sorting. Its sliders answer by name ("set the speed to 200"), and "simulate
it again" or "plot it with 3 m" changes the one just shown. Beside a game a running simulation
slows to four frames a second. The orbits, the ripple tank and surfaces turn 3D and back ("can
you make it 3D?", "show the orbits in 3D", "back to 2D", "mach es 3D", or the 3D key in their
window): the orbits' plane leans back and sinks into the wells of their gravity, the tank stands
up as a relief of the water's height, and a surface made 2D is seen from straight above. The run
goes on through that, as through a pause; the other simulations are flat and say so.

Every window of his opens as in the films: a spark leaves his core and runs out along the
window's leader, a line draws out from where it lands, the frame unfolds from it, flickers once
as it locks and the content resolves; it leaves with an exit (it folds into a line), whether it
is opened or closed by voice, by its CLOSE key or by Esc. Asked for again while it is leaving,
it stays. On Windows all his windows wait for the screen's refresh together, once per refresh,
so dragging and scrolling run at the screen's full rate however many of them are open.

A pill at the top of the HUD says what he is doing: LISTENING, THINKING, SPEAKING, MUTED or
STANDBY. While Sir speaks, his voice shows as a ring of ticks round the emblem. Both can be
switched off in the VIEW menu. F1 (or M) mutes the microphone; the key hints at the bottom right
can be clicked.

"Open the hotkeys window" (or "change my mute key", "zeig mir die Tastenkürzel"; the settings
panel's HOTKEYS, the admin window's tile) lists his keys that work in every program and in games:
the microphone on and off (no key at first), his windows to the front (Ctrl+Alt+J), the console
(Ctrl+Alt+K) and waking him from standby (Alt+J). SET, then the new key, changes one (a letter,
digit or Space needs Ctrl or Alt; F1-F24, Pause and Scroll Lock may stand alone; a key another of
his has is refused; Esc gives up); CLEAR takes it away; a key another program holds shows TAKEN.
The microphone key sounds rising on and falling off (with the sound cues on). A scripted session
takes no key.

"Open the protocol window" (or "make a new protocol"; the settings panel's PROTOCOLS, the admin
window's tile) shows everything that starts by itself in one list: your protocols (the built-in
ones too, yours to change), the rules said aloud ("when I clap twice, open Spotify") and his own
reactions to two claps and to knocking (MY OWN: switched there, and replaced by a rule or protocol
of yours on the same sound). A protocol reads in two steps. 01 STARTS WHEN, any one of its lines:
a phrase you SAY ("wake up, daddy's home": a protocol's phrase wins over every other reading of
it), a sound, a program starting or closing, a time of day or a friend's PC ("whenever Max plays
Minecraft", "whenever Max's PC comes online"), each added with its ADD key; "engage the gaming
protocol" always works. 02 THEN I DO, the orders in order, as you would say them ("open the earth
on Max's PC" runs on that PC), or TELL ME. A line he does not understand is refused when you save,
with the reason; RUN NOW runs it; MAKE IT A PROTOCOL turns a rule said aloud into one. Every PC of
the link tells the main PC which game it runs. As a protocol starts he says one line, "Activating
the Gaming Protocol, Sir.", and runs its orders without a word: only an order that asks something
("what's the weather") is answered aloud, and one that failed is told afterwards.

Sounds start rules and protocols too: "when I clap twice, open Spotify", "when someone knocks,
tell me", "if the doorbell rings, pause the music", "wenn es klopft, sag mir Bescheid", "when
someone else talks, tell me". He hears two and three claps (soft ones too, at any pace), a finger
snap, knocking, the doorbell,
a phone, an alarm, a dog, breaking glass and whistling: the radar notices a sound and a small sound
model (CED-mini in `models\sound`, offline, optional) names it, so typing, music and applause are
no claps. Someone else talking is your voice print saying it is not you (told at most once a
minute). "Tell me" as the order makes him say what happened. He listens for sounds only while
something waits for one; the protocol window's I HEAR line shows the last two he heard.

"Give me an overview of the most important news from Germany" (or "what's happening in France?",
"die wichtigsten Nachrichten aus Deutschland", "what's the news?" for your own country, by the
country code of the WhatsApp settings) opens a card with that country's headlines, word for word,
and reads the first three. In English: DW for Germany, the BBC for the UK, NPR for the US, the
Guardian's country page for the others; in German: the tagesschau's front page for Germany (its
own news first) and ORF for Austria, English headlines elsewhere. Nothing older than three days is
told; a country without a newsroom goes to the web search.

"Activate goon mode" opens a private window of your default browser (Edge InPrivate, Chrome,
Brave, Vivaldi or Chromium incognito, a Firefox private window).

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

"Open the mixer" opens a window with a fader for every channel: the master first, then
each app that has a session. "I can't hear anything" (or "there's no sound", "ich höre Discord
nicht") looks for the sound: a muted master, a muted or nearly silent app, and says where it went.
"Is anything muted?" names only what is.

"Turn the game down" finds whatever game has a channel, listed or not; the background agents of
hardware makers (Logitech, Razer, Corsair and the like) are never taken for one. "Turn Firefox
off" closes a program and "turn Firefox back on" opens it again; these two sentences work for
real programs only, never for the PC or the sound.

## Music

Sonora is the player he drives, through its own pipe (`sonora.rs`). A song, an album, an artist
or one of Sir's playlists starts there at once, private and not downloaded ones too: by its link
from the library, else by Sonora's own search. "Shuffle my playlist Gym" plays it with shuffle on.
A playlist may be named loosely: "play my rock playlist" takes the one with that word in its name,
his own before others', then the one he played last.
"What's in the queue?" reads what comes next; "play Highway to Hell next" puts it right after the
song playing, "add Thunderstruck to the queue" at the end. "Turn on shuffle", "repeat this song"
("repeat the playlist", "repeat off"), "skip ahead 30 seconds", "go back 10 seconds", "jump to
1:30", "restart the song", "like this song" and "unlike it" are Sonora's own switches. Sonora
lets nobody clear its queue or change a playlist from outside: "clear the queue" and "add this
song to my playlist Gym" say so. When Sonora is not running he starts it and waits for it; a test
session never reaches it.

"Pause", "play", "mute", "unmute", "continue" or "next song" said alone are carried out at once,
without his name; while music plays, a sung line without his name is the song's, not an order. A
song he starts he names, without the player's notes. The song beside his orb goes with "hide the
now playing" (or its row under SCREENS) and comes back with "show the now playing".

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
The free one: install Tailscale on this PC (tailscale.com/download), sign in, and restart JAI.
With the provider on "auto" he takes Funnel by himself; the first time he shows a link to switch
Funnel on for this PC (or run `tailscale funnel 8444` once). The address is then
`https://<pc>.<tailnet>.ts.net` for good, and so is the pairing link. This PC reaches its own
Funnel address through Tailscale itself, so its check proves nothing for a phone outside your
tailnet: the pairing window then says CHECKED INSIDE YOUR TAILNET ONLY and opens the link for
home. A phone with the Tailscale app switched on opens the address anywhere.

## Accounts

Only Sir may do everything: his voice, his paired phone, this PC. Anyone else needs an account.
On the phone page anyone can ask for one with a name and a password (3 to 20 characters each); a
six-digit key appears on Sir's screen only, and the account is made when it is typed in. A new
account may ask questions and nothing else: it gets its own conversation and nothing of Sir's
(not his place, his messages, his PC). "Open the accounts list" ("öffne die Kontenliste") shows
every account with its password and what it may do in two places apart, on its own PC and on
Sir's: talk only, what Sir switches on (tools by the Feature Lab's groups, down to single options:
the Earth, but not its satellites), or everything. Everything on its own PC is all Sir can do, on
that PC alone (its settings, Feature Lab and Discord included); on Sir's PC (his phone page, or a
known voice at his PC) it never opens his own windows, and those switches all start off. Sir
closes topics (where they are, maths, words of his own), pauses an
account ("time out Max for two hours") or removes it. "What did you do for Max today?" (or this
week, or for everyone) reports what JAI did for others. Logging in and asking for accounts are
limited per address, so the page cannot be flooded.

A voice JAI has never heard that calls him is asked "Who am I speaking with?". Its answer ("I'm
Erika", "ich bin Erika", the name alone) starts learning it; after a few sentences it may ask
questions. A name that belongs to an account or a known voice already is refused: Sir introduces
that person himself.

## J.A.I. on other PCs

Every PC logged into Sir's admin account is a Main PC of his; any other PC is registered at one of
them. The one-file setup below (`JAI-setup.exe` from GitHub's Releases page; the phone page's
"J.A.I. for your Windows PC" leads there too) opens on that choice. LOG IN takes Sir's admin
account, the same name and password as on his other Main PCs, and asks no other PC: this PC
becomes one of his Main PCs, its admin account made with them (on a second try they must match).
REGISTER is a friend's: the address of Sir's Main PC, then log in there with an account, or ask
for one (a key appears on Sir's screen); that is all it takes from Sir's PC. Either way it needs a
graphics card with 4 GB of memory (under 8 GB it takes the smaller brain, Qwen3.5 2B).
Everything else is downloaded from GitHub: JAI and eSpeak NG come inside the setup,
his engine from Ollama's release (the version Sir's PC runs, checked by its SHA-256), the voice
lock's model from sherpa-onnx's. Then come the voice, ears and brain, from where every JAI fetches
them; the setup adds JAI to the Start menu and starts him. A download the connection cuts off
goes on where it stopped, by itself or when the setup is started again. Until it is logged in,
such a JAI answers nobody.

"Open the earth on Max's PC", "turn the volume down on Max's computer", "shut down Max's PC": the
order goes to Max's PC (it waits ten minutes if that PC is offline) and never acts on Sir's. A JAI
on another PC sends no orders anywhere but in Sir's admin mode (see Admin): asked to act on Sir's
PC, it says it can't. It always runs
the version Sir's PC runs (a newer one is fetched at once and started; Sir's own PC never updates
itself). Every half hour it also looks at GitHub's newest release and takes it by itself when it is
newer, even while Sir's PC is off, as long as its account's key was good when Sir's PC last
answered (the release's `JAI.exe` or `jai-linux`); it never goes back to an older version. It shows
in the accounts list with its version and its
PC's load. Voice prints are shared both ways and held in memory only: each stays on its owner's PC.
A Linux PC installs him as well (see Linux).

GitHub's Releases page has one file for each: `JAI-setup.exe` (Windows) and `JAI-setup-linux`
(`chmod +x` first). Each carries the installer with everything it starts with (on Windows the
Visual C++ runtime, DirectML and eSpeak NG, so nothing has to be installed first; on Arch it
installs the packages below with pacman, asking for the password). On a PC where JAI is installed
already, the same file updates him: the running JAI quits, UPDATE puts in the version the file
carries with the key that PC holds, or on a Main PC with its admin account (no login either way),
and he starts again (and then takes the version
Sir's PC runs, if that is another); called off, he starts as he was. It never
touches a JAI that runs from another folder, such as Sir's own. The files are built from `setup/`
(a crate of its own that packs the built JAI) by the `Release` workflow, which publishes them when
a tag `v…` is pushed; `--check` unpacks the installer and starts it with `--help` only.

## Admin

"Open the admin window" ("activate admin mode", "öffne das Admin-Fenster") shows every window JAI
can open, in groups, Sir's own first; a click on one opens it. His own are his alone on every PC:
the accounts list and the report, and the settings, the Feature Lab (the Tool Forge is in it) and
Discord unless that PC's account may do everything there.
"Close the admin window" ("admin mode off") closes it.

A Main PC of Sir's (one with no main PC of its own) installed without LOG IN asks once for his
admin account, a name and a password typed twice, and answers nobody until it is made; from then
on it is his. That account
logs in nowhere: it only signs Sir in at another PC, and it is never paused, removed or shown from
there.

On another PC, JAI does what that PC's account may: Sir's switches in the accounts list under "On
Max's PC", each window among them. A new account only talks to him. A window it may not open
does not open: a pop-up asks for Sir's admin sign-in instead, and the PC stays logged in with its
own account. Signed in, that window is his until it closes; opened again, it asks again. The admin
window there is admin mode: everything on that PC is his until it closes, his accounts list (the
main PC's, changed there) and his orders to other PCs included. When the last window his sign-in
opened closes, the sign-in ends at once, there and at his main PC (which also lets one go that its
PC has not asked about for two minutes). An order Sir sends to that PC from his main PC opens its
window there all the same.

## Linux

On Linux J.A.I. is made for KDE Plasma 6 (Wayland) on Arch. The same crate builds on Linux; the
`Linux` GitHub workflow builds it on Ubuntu, runs the tests of the Linux parts and keeps
`target/release/jai` as the run's artifact, and on Arch checks the KDE parts against a real KWin
without a screen (`live_checks::kde`). To build it yourself on Arch:

```
sudo pacman -S --needed base-devel rust cmake clang alsa-lib opus
WHISPER_DONT_GENERATE_BINDINGS=1 cargo build --release
```

(The variable keeps whisper's own bindings: generated with Arch's newer clang, they lose the
fields of `whisper_full_params`. After a build without it, run `cargo clean -p whisper-rs-sys
--release` once, as whisper's build script does not run again for the variable.)

At run time he needs `espeak-ng` (his voice), `zstd` (the installer unpacks Ollama with it),
`libpulse` (`pactl`: the volume of every app and the microphone), `tesseract` with its English and
German data (reading the screen), `pacman-contrib` and `discover` (app updates), `geoclue` (where
the PC is), `xdg-utils`, `yt-dlp` and `ffmpeg` (the media grabber), `wl-clipboard` (the
selection in Dolphin) and `polkit` (`pkexec`: the rights to write a J.A.I.OS stick); the NVIDIA driver's `nvidia-smi` tells him the video memory:

```
sudo pacman -S --needed espeak-ng zstd libpulse tesseract tesseract-data-eng tesseract-data-deu pacman-contrib discover geoclue xdg-utils yt-dlp ffmpeg wl-clipboard polkit
```

The file orders ("rename this", "zip these", "print it twice", "copy the path of this") read
Dolphin's selection by copying it, so "make a new folder here" needs something selected in that
folder. Zips are made and unpacked by `bsdtar` (libarchive, which pacman itself needs); printing
needs CUPS (`lp`). "Free up some space" empties only the Trash: Linux clears its temp files
itself. "What colour is this?" brings up KWin's own colour picker, since Wayland lets no program
read the screen: click the colour. Waking another PC works on the same network only, as on
Windows.

His own windows are X11 windows (XWayland), each waiting for the screen's refresh by the driver's
own vsync (the shared wait of Windows is not used there). Other programs' windows, the keys he presses for Sir
and screenshots go through KWin, which allows them only to a program its menu entry names: at
every start he writes `~/.local/share/applications/jai.desktop` for the program he is, so start
him once more after the first start (or after moving him). Music and media keys go over MPRIS,
power and sleep over logind, the lock to KDE's own lock screen, and his tray icon is a
StatusNotifierItem. Other desktops get what works without KDE (`wmctrl` and `xdotool` on X11,
`hyprctl` on Hyprland, `xprintidle` for the idle time).

Started on its own, `jai` is the installer, with the same LOG IN and REGISTER as on Windows: it
fetches Ollama for Linux and the models, and installs into `~/.local/share/jai` with an entry in
the applications menu. Only on Windows: WhatsApp Desktop (Linux opens WhatsApp
Web) and the microphone's level meter.

"Make a J.A.I.OS stick" works on Linux too: polkit (`pkexec`) asks for the rights, the stick's
partitions are unmounted and the device is written directly and read back; drives are read with
`lsblk`. "Is my PC ready for J.A.I.OS?" tells UEFI, Secure Boot and the newest image. Making room
for J.A.I.OS is not offered on Linux, as Linux cannot shrink a BitLocker partition: ask J.A.I. on
Windows.

## J.A.I.OS

J.A.I.OS is an operating system made of J.A.I.: Arch Linux with CachyOS's repositories and kernel
(`linux-cachyos`), KWin as the compositor, and J.A.I. in place of the Plasma shell, the login
screen and the system's control. Everything for it lives in `os/` in this repository; the program
is the same `jai`, started in other modes. Nothing of it has run on a real PC yet.

**From Windows.** "Is my PC ready for J.A.I.OS?" shows UEFI, Secure Boot, GPT, BitLocker, Fast
Startup, the room after C: (40 GB needed) and the newest install image. "Make room for J.A.I.OS"
(or "make 150 GB of room for J.A.I.OS") shrinks C: with Windows' own storage API, which works
with BitLocker on: he says how much C: can give, suggests 100 GB, never leaves C: under 30 GB
free, and shrinks only after your yes and Windows' own permission question. Keep the BitLocker
recovery key at hand. "Switch off Fast Startup" does that (a dual boot needs it off). "Make a
J.A.I.OS stick" (optionally "on the SanDisk" or "to drive E") fetches the newest
`jaios-<version>-x86_64.iso` from the GitHub releases, checks its SHA-256, asks before erasing the
stick, writes it and reads it back. Only USB sticks up to 300 GB that hold nothing of the system
are written to. Rufus and Ventoy work with the same ISO. The steps that need rights are flags of
the same program, started by him with Windows' permission question: `--make-room`,
`--write-stick`, `--fast-startup-off`.

**Installing J.A.I.OS.** Start the stick in UEFI mode; the installer (`jai --os-install`) opens by
itself. It asks for the language (English or German), the time zone and keyboard, where on the
disk J.A.I.OS goes, your J.A.I. account, and a user. Three ways onto the disk: *beside Windows,
into free space* (chosen by itself when a disk with Windows has 40 GiB or more unallocated; make
that room from Windows first, the installer never shrinks Windows), *erase this disk* (type the
disk's name), or *replace a partition*. Windows' partitions, its EFI partition and the stick are
never taken. A summary lists every change before anything is touched. J.A.I.OS gets its own
4 GiB boot partition and a btrfs root (@, @home, @root, @srv, @cache, @tmp, @log), encrypted with
LUKS2 if you switch that on. Packages come from CachyOS's repositories for your processor
(x86-64-v3, v4, znver4), Arch's and `[jaios]`; the list is `os/packages/target.txt`. Limine
starts what was started last after 3 seconds and has an entry for Windows. Secure Boot: with the
firmware in Setup Mode J.A.I. enrolls his own keys next to Microsoft's; otherwise he tells you at
the first start how to finish. With Secure Boot on and the maker's keys, J.A.I.OS does not start
until you do that or turn it off. Account: LOG IN makes this PC a Main PC with your admin
account; REGISTER joins a Main PC. Anywhere but on the stick, `jai --os-install` is a preview on a
sample PC and changes nothing.

**The desktop.** On J.A.I.OS, J.A.I. draws the desktop himself, on KWin: `jai --shell` is a small
process beside the assistant (no models, no voice), started with the session by
`jai-shell.service`. Every monitor gets a taskbar: the [J] key, a key per open program (a click
brings its window to the front or steps it back, another click goes round a program's windows,
a middle click opens another), the tray, his lamp with what he is doing, and the clock. The [J]
key or the Meta key opens the menu: type to search programs; whatever matches none is an order
for J.A.I. on Enter; a right click pins a program to the favourites; shut down, restart and log
out ask for a second press. The desktop shows his core and the icons of your Desktop folder.
Notifications from every program appear as cards at the top right; the volume and brightness
keys show their reading at the bottom. If the shell crashes it starts again; after three crashes
in a minute KDE's own desktop opens as safe mode (`systemctl --user start jai-shell.service`
brings his back). The shell is drawn on the CPU into shared memory, so it comes up on any
graphics driver.

**Root core.** `jai-system.service` runs `jai --system` as root: the D-Bus service
`org.jaios.System1` on the system bus. Only root and members of the group `jai` may call it; it
checks each caller's Unix user itself. It runs pacman, the update (`pacman -Syu`, then Flatpak;
snap-pac takes snapshots before and after), snapper's way back, Secure Boot's enrolment and single
commands as root, always as argument lists, never as a shell line. Whoever is in `jai` is root
through it.

**By voice.** "Install Firefox", "uninstall VLC", "update yourself", "are there system updates",
"undo the last update", "show the snapshots", "go back to snapshot 12", "finish Secure Boot", "run …
as root". Installing from the repositories and updating run at once (never during a game).
Removing, going back, Secure Boot's keys, any command as root and a package from the AUR are asked
aloud first. AUR packages are built by paru as you, never as root. Once a day he tells what updates
wait.

**Undoing an update.** He takes back exactly what the last update changed (`snapper undochange`),
makes a snapshot of now first and rebuilds the boot files; restart afterwards. If the system no
longer starts, pick a snapshot in Limine's menu.

**Secure Boot.** If the firmware was not in Setup Mode at installation he tells you on the first
start: clear the Secure Boot keys in the firmware's setup, start again, say "finish Secure Boot".

**Login.** greetd starts `jai --greeter` inside KWin. He greets when the screen is up; if he knows
your voice he greets you by name and chooses your user. The password is always required. Voice
prints for the login screen live in `/var/lib/jaios/voices` (root's, readable by the login screen
only).

**Accounts.** Root is the switch "System" in an account's list, off by default and not part of
"everything".

**The look.** `jai --os-look <folder>` writes what the OS shows outside J.A.I.'s windows, from his
theme: Plymouth's theme `jaios` (a light runs round his reference ring while the PC starts, then
his core draws itself and settles), Limine's wallpaper and colour lines, KDE's colour scheme `JAI`
and the system's kdeglobals, Konsole's colours, the install stick's boot picture and the logo.
`jaios-settings` runs it when it is built.

**The repository.** `[jaios]` is the rolling GitHub release `jaios-repo`: `Server =
https://github.com/JayKaey/JAI/releases/download/jaios-repo`. Every push to `main` replaces its
packages; the release number of a package is the commit count. The packages are not signed yet.

**The release.** A tag `v…` attaches `jaios-<version>-x86_64.iso` and its `.sha256` to that
version's release (under 2 GiB, GitHub's limit for one file). The stick's own boot loaders are not
signed: switch Secure Boot off for the install; the installed system signs its own with sbctl.

**Building it.** J.A.I.OS's files are in `os/`: `os/settings/` (the tree of the package
`jaios-settings`), `os/pkg/` (the PKGBUILDs of `jai`, `jaios-settings` and `jaios`) and `os/iso/`
(the install stick's archiso profile). The `J.A.I.OS` workflow builds all of it on Arch, packs the
repository, builds the stick and starts it in QEMU until the installer's window is up. By hand, on
Arch:

```
WHISPER_DONT_GENERATE_BINDINGS=1 cargo build --release
for p in jai jaios-settings jaios; do (cd os/pkg/$p && PKGDEST=$PWD/../../../repo makepkg -d -f); done
(cd repo && repo-add jaios.db.tar.zst *.pkg.tar.zst)
sed -i "s|%JAIOS_REPO%|$PWD/repo|" os/iso/pacman.conf
bsdtar -xf repo/jaios-settings-*.pkg.tar.zst -C /tmp usr/share/jaios/iso && cp /tmp/usr/share/jaios/iso/* os/iso/syslinux/
sudo mkarchiso -v -w /tmp/archiso -o out os/iso
```

(The builder's pacman must trust CachyOS's key first: `pacman-key --recv-keys F3B607488DB35A47
--keyserver keyserver.ubuntu.com && pacman-key --lsign-key F3B607488DB35A47`, then install
`cachyos-keyring`.)

## JAI's own Discord account

JAI can have a Discord account of his own: a bot you create once.

1. On discord.com/developers, make a New Application. On its Bot page, copy the token (Reset
   Token) and switch on the Message Content Intent. Switch on the Presence Intent too if he
   should tell you who comes online.
2. Under OAuth2 → URL Generator, tick the scope `bot` and the permissions View Channels, Send
   Messages, Read Message History, Connect, Speak and Use Voice Activity. Open the link it gives
   and invite him to your server.
3. Paste the token into the Integrations hub (Quick Connections, JAI's Discord account) and
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
JAI.exe --selftest --no-llm     # silent TTS -> VAD -> Whisper check, no language model
JAI.exe --live-test             # every probe in the running app; nothing is sent or changed
JAI.exe --live-test --headless  # the same with the HUD drawn offscreen (no window)
JAI.exe --exhaustive 5          # the live test plus the exhaustive matrices, five runs in a row
JAI.exe --silent --chat "..."   # ask him for real, unseen and unheard; answers go to jai.log
JAI.exe --silent --show --monitor 2 --chat "..."   # the same in the real window, on monitor 2
JAI.exe --live-test --monitor 2   # the live test with its window on monitor 2
JAI.exe --silent --show --window-drill --chat "What time is it?"   # SCREENS buttons in the real window
```

`--monitor <n>` puts a test session on monitor n (1 = the primary) for that session only: its
background covers that monitor alone, so a test never lands on the monitor you are playing on.
His other windows open on that monitor too. A test session never moves the focus: none of its
windows asks to be brought forward, and without `--monitor` its board is a borderless window
rather than winit's fullscreen (which forces a new window to the front with an Alt tap).

`--window-drill` (without `--monitor`, where a test session's board is a window) clicks every
SCREENS button (each monitor, the span and back, CLOSE) in the real window, one step at a time,
and checks where Windows really put the window after each (`[DRILL]` lines in `jai.log`).
The clicks go into the window's own input, so the cursor and keyboard are never touched. The
checks that change real things on the PC (the real mixer, a
shutdown that is scheduled and cancelled, the lock screen, voice samples) are ignored tests in
`src/live_checks.rs`, each behind its own environment switch.

`--silent` (with one or more `--chat` requests) is a scripted session through the real
pipeline, language model included, that nobody sees or hears and that leaves no trace in the
conversation log or the settings. It acts inside JAI only: reads and his own HUD are carried
out, the volume works on a copy of the mixer, and anything that would reach the PC around him
(an application, the music, another window, a message) is logged and left undone. `--show` runs
it in the real window.

`--exhaustive` needs no window and makes no sound: the real HUD is drawn into a private egui
context and inspected as geometry (text against clip rectangles, blocks against each other and
the orb, stroke colours against the theme, animations frame by frame) at four screen sizes.
Every command family goes through the real dispatcher in five passes (English, German, misheard,
four commands spoken over JAI, compound sentences), both voices are measured against their
clips and against each other, and every Feature Lab button is pressed five times. What Sir says
to the screens, his windows and the volume is also said aloud as samples: two stock voices at
speech level over room hiss, through Whisper, the wake word and the dispatcher. They stand in
for a person; they do not show how his own accent and microphone fare. The microphone is deaf for
the whole run, whatever its switch says. Requests are dry runs: tool calls are recorded, not
carried out, except reads and what only draws inside the HUD. The settings file and the
conversation log stay untouched. The report is `exhaustive_report.json`.

A test session runs where nobody sees it, so it gives way: starting JAI.exe while one is
running ends it and starts him normally. Its tray tip says so, and a run cut short exits with a
failure code.

Time budgets are for the running app, not for its start: once per process the pipelines, the
routes and the read-only tools are gone through first without being counted (the first time
anything runs costs two to three times what it costs afterwards), and the time is taken from
the second pass on. Only the time is forgiven: a wrong result in that first pass fails the run
that follows, and the probes that were late in it are named in the log and in the report
(`first_pass_late`).

`JAI.exe --help` lists all options.

## Workbench

Ten everyday tools with voice routes in English and German: spaced-repetition flashcards, a
maths/physics/chemistry formula sheet, molar masses and mole conversion, keyboard-shortcut
sheets for the program in front, screen time with a break coach, routines learned from what is
asked at the same time on several days, a thermal advisor with a fan-curve suggestion,
clipboard history (in memory only, secrets never kept), tiling program windows on a monitor,
and predictive command completion in the text console (Ctrl+K).

Every block of a report on the board has a right-click menu (copy, pin, maximize, export to
Markdown or JSON, close), and new windows are placed so that they never open on each other or
on the core.
