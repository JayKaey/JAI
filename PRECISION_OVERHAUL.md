# J.A.I. precision overhaul

The desktop remains a native Rust/egui application. The existing mobile satellite is served by Rust and uses a browser on the phone.

## Commands

- `What can you do?` reads the capability manifest and the live tool registry.
- `Can you play a specific playlist?` explains music capabilities without starting a timetable request.
- `Play my playlist JaysRock`, `Play Back in Black in JaysRock`, and `Play Highway to Hell` use the music library/player routing.
- `Wake up, Daddy's home` invokes the welcome sequence and requests Highway to Hell. Playback results distinguish a successful player handoff from unconfirmed playback.
- `Add Mathe lernen to my calendar tomorrow at 4 PM` or `Trag morgen um 16 Uhr Mathe lernen in meinen Kalender ein` creates a locally persisted calendar event and queues remote synchronization.
- `How can I use you on my phone from anywhere?` opens the pairing window with local and worldwide links.

## Worldwide phone access

When the satellite is enabled, JAI starts a separate loopback-only HTTP origin, then a managed HTTPS tunnel. The LAN listener retains its own TLS certificate. Every control and data request requires the saved pairing token. Scan the Worldwide QR code in the desktop pairing window; the phone remembers its token in local storage. Treat the QR code as a password.

New fields within `satellite` in `jarvis_config.json`:

| Field | Default | Meaning |
| --- | --- | --- |
| `worldwide_enabled` | `true` | Enable managed public access. |
| `tunnel_provider` | `auto` | `auto`/`cloudflare`, `tailscale`, or `ssh`. |
| `tunnel_public_url` | empty | HTTPS origin configured on an SSH/VPS reverse proxy. |
| `tunnel_ssh_target` | empty | Existing SSH target such as `user@host`. |
| `tunnel_ssh_key` | empty | Optional identity file. |
| `tunnel_ssh_remote_port` | `18443` | Loopback port on the VPS used by its HTTPS reverse proxy. |
| `wake_mac` | empty | MAC address of a Wake-on-LAN target. |
| `wake_broadcast` | `255.255.255.255` | IPv4 broadcast destination for Wake-on-LAN. |
| `wake_relay_url` | empty | Independent HTTPS wake service, opened by the phone when this PC is offline. |

Cloudflare mode finds an installed executable or downloads the official standalone release into `bin/`, verifying the release SHA-256 digest before execution. The child is terminated when JAI exits. Connection failures are reported in the GUI and retried. Quick Tunnel addresses are temporary, so rescan after an address change. Tailscale mode requires Tailscale installation, login and Funnel permission. SSH mode requires a configured VPS, trusted host key, key-based login and an HTTPS reverse proxy; JAI does not invent credentials or provision a VPS.

The PC must be awake and connected to the Internet. A sleeping PC cannot run its own web server or tunnel. Waking this same PC remotely requires a separate always-on relay on the home network and hardware/firmware Wake-on-LAN support. The built-in wake action sends packets to a configured LAN target while JAI is online; sending a packet does not confirm that the target woke.

## Calendar and workouts

New calendar events commit to `calendar_events` before network work. Pending events remain visible without iCloud credentials or during network failure and retry during synchronization. An iCloud read-only subscription cannot accept writes; use the CalDAV account with an app-specific password and a writable calendar. Remote HTTP failures are logged without credentials. The calendar read path merges cached remote events and pending local events.

Workout sets, estimated one-repetition maximum records, calendar associations and rest deadlines persist in SQLite. The desktop and mobile satellite show the same timer. Estimated records are calculations, not measured maximum lifts. Completion notifications for observed external processes report their exit without asserting success when no exit status is available.

## Project graph and speech

The core inventories actual source files, Rust syntax trees, Cargo dependencies, database tables, skills, models and endpoint references. Fine-line projected geometry and telemetry labels represent that inventory; execution traces are instrumented transitions. The graph does not pad its node count with fabricated dependencies.

Speech uses complete-clause coalescing, cubic Hermite resampling, a held 20 ms tail for raised-cosine joins, subtle warmth/presence equalization and peak limiting. Voice identity and naturalness still depend on the selected installed speech model.

## Service references

- [Cloudflare Quick Tunnels](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/)
- [Official cloudflared downloads](https://developers.cloudflare.com/tunnel/downloads/)
- [Tailscale Funnel CLI](https://tailscale.com/docs/reference/tailscale-cli/funnel)
- [Windows LockWorkStation](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-lockworkstation)
- [Windows SetSuspendState](https://learn.microsoft.com/en-us/windows/win32/api/powrprof/nf-powrprof-setsuspendstate)
