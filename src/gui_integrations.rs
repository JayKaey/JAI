//! The HUD's "Integrations & Productivity Hub": Sir's own endpoints for e-mail (IMAP), calendar
//! (CalDAV / `.ics`), WebUntis, itslearning and custom web feeds, the background watchers and the
//! study folder. Settings are edited as a draft and saved together; app tokens are typed into
//! password fields and go straight into the encrypted vault (left blank, the stored one stays).
//! "Test connection" runs the real client in the background and shows its answer.

use super::{button, lock, muted, request_repaint, Palette, Tone};
use crate::app::Jai;
use crate::holo::{font as face, Face};

fn bold(size: f32) -> egui::FontId {
    face(Face::Head, super::ts(size))
}
use crate::config::{AcousticSettings, AwaySettings, DiscordBotSettings, FeatureSettings, LocationSettings, SatelliteSettings, WhatsAppSettings};
use crate::pim::Pim;
use crate::pim_config::{self, PimSettings, WebFeed, CALENDAR, EMAIL, FEEDS, LMS, WEBUNTIS};
use eframe::egui::{self, RichText, Ui};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;

/// "Open my productivity settings" from voice: the tab to show (None = the first).
static REQUEST: Mutex<Option<Option<&'static str>>> = Mutex::new(None);

pub(super) fn request(service: Option<&'static str>) {
    *lock(&REQUEST) = Some(service);
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Email,
    Calendar,
    WebUntis,
    Lms,
    Feeds,
    Study,
    /// WhatsApp, the mobile satellite, location, the acoustic radar, remote nodes, contacts.
    Quick,
    /// The fifteen field systems: state, quick actions and settings.
    Field,
}

impl Tab {
    const ALL: [(Tab, &'static str); 8] = [
        (Tab::Email, "Email"),
        (Tab::Calendar, "Calendar"),
        (Tab::WebUntis, "WebUntis"),
        (Tab::Lms, "itslearning"),
        (Tab::Feeds, "Web feeds"),
        (Tab::Study, "Watchers & study"),
        (Tab::Quick, "Quick Connections"),
        (Tab::Field, "Field Systems"),
    ];

    fn of(service: Option<&str>) -> Tab {
        match service {
            Some(CALENDAR) => Tab::Calendar,
            Some(WEBUNTIS) => Tab::WebUntis,
            Some(LMS) => Tab::Lms,
            Some(FEEDS) => Tab::Feeds,
            Some(pim_config::WATCHERS) | Some(pim_config::STUDY) => Tab::Study,
            Some(crate::stark::QUICK_CONNECTIONS) => Tab::Quick,
            _ => Tab::Email,
        }
    }

    fn service(self) -> Option<&'static str> {
        match self {
            Tab::Email => Some(EMAIL),
            Tab::Calendar => Some(CALENDAR),
            Tab::WebUntis => Some(WEBUNTIS),
            Tab::Lms => Some(LMS),
            Tab::Feeds => Some(FEEDS),
            Tab::Study | Tab::Quick | Tab::Field => None,
        }
    }
}

pub(super) struct Hub {
    pub open: bool,
    tab: Tab,
    draft: Option<PimSettings>,
    /// Newly typed tokens per service (blank = keep the stored one).
    tokens: HashMap<&'static str, String>,
    untis_session: String,
    /// Background test results per service.
    results: Arc<Mutex<HashMap<&'static str, (bool, String)>>>,
    message: Option<(String, bool)>,
    /// Watcher intervals being edited (service key → minutes).
    watch_minutes: HashMap<String, u32>,
    /// Quick Connections: the settings being edited, and its small forms.
    quick: Option<QuickDraft>,
    wa_token: String,
    /// A newly typed Cloudflare tunnel token (blank = keep the stored one).
    tunnel_token: String,
    /// A newly typed token of JAI's Discord bot (blank = keep the stored one).
    discord_token: String,
    /// A newly typed Mapillary client token for street view (blank = keep the stored one).
    mapillary_token: String,
    node_form: [String; 4],
    contact_form: [String; 3],
    /// (pairing URL, side, modules) of the satellite QR code.
    qr: Option<(String, usize, Vec<bool>)>,
    /// Field Systems: the settings being edited (game servers as one comma-separated line).
    field: Option<FeatureSettings>,
    field_servers: String,
}

#[derive(Clone, PartialEq)]
struct QuickDraft {
    whatsapp: WhatsAppSettings,
    satellite: SatelliteSettings,
    location: LocationSettings,
    acoustic: AcousticSettings,
    discord_bot: DiscordBotSettings,
    away: AwaySettings,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            open: false,
            tab: Tab::Email,
            draft: None,
            tokens: HashMap::new(),
            untis_session: String::new(),
            results: Arc::new(Mutex::new(HashMap::new())),
            message: None,
            watch_minutes: HashMap::new(),
            quick: None,
            wa_token: String::new(),
            tunnel_token: String::new(),
            discord_token: String::new(),
            mapillary_token: String::new(),
            node_form: Default::default(),
            contact_form: Default::default(),
            qr: None,
            field: None,
            field_servers: String::new(),
        }
    }
}

fn text_field(ui: &mut Ui, value: &mut String, hint: &str) {
    crate::holo_ui::slot(ui, egui::TextEdit::singleline(value).hint_text(hint).desired_width(300.0));
}

fn secret_field(ui: &mut Ui, value: &mut String, stored: bool) {
    let hint = if stored { "•••••• stored encrypted (type to replace)" } else { "app token / password" };
    crate::holo_ui::slot(ui, egui::TextEdit::singleline(value).password(true).hint_text(hint).desired_width(300.0));
}

/// Quick Connections: JAI's own Discord account and Away Mode. Needs nothing of the running
/// app, so the offscreen test draws exactly what the hub shows.
fn account_and_away(ui: &mut Ui, p: &Palette, bot: &mut DiscordBotSettings, away: &mut AwaySettings, token: &mut String, token_stored: bool, results: &Mutex<HashMap<&'static str, (bool, String)>>) {
    let section = |ui: &mut Ui, title: &str| {
        ui.add_space(6.0);
        ui.label(RichText::new(title).font(bold(12.5)).color(p.text));
    };
    section(ui, "JAI's Discord account (a bot of his own)");
    crate::holo_ui::toggle(ui, &mut bot.enabled, "Enabled");
    egui::Grid::new("hub_discord_bot").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
        ui.label("Bot token");
        secret_field(ui, token, token_stored);
        ui.end_row();
        ui.label("Your Discord user id");
        text_field(ui, &mut bot.owner_id, "set by pairing (or Copy User ID)");
        ui.end_row();
        ui.label("Status");
        text_field(ui, &mut bot.status_text, "Watching … (empty: none)");
        ui.end_row();
    });
    crate::holo_ui::toggle(ui, &mut bot.answer_others, "Friends who DM or mention him get a short answer");
    crate::holo_ui::toggle(ui, &mut bot.notify_owner, "Alerts reach my phone as DMs while I'm away");
    crate::holo_ui::toggle(ui, &mut bot.follow_owner_voice, "Join my voice channel whenever I join one");
    crate::holo_ui::toggle(ui, &mut bot.announce_listening, "Say once in each call's chat that he transcribes it");
    crate::holo_ui::toggle(ui, &mut bot.presence_intent, "Who's online (switch on the Presence Intent in the Developer Portal too)");
    let status = crate::discord_bot::hud_status();
    ui.horizontal_wrapped(|ui| {
        let online = crate::discord_bot::online();
        if ui.add_enabled_ui(online, |ui| crate::holo_ui::key(ui, "Pair my account")).inner.on_hover_text("Shows a code: send it to the bot as a direct message within ten minutes").clicked() {
            if let Some(bot) = crate::discord_bot::bot() {
                let code = bot.start_pairing();
                lock(results).insert("discord_bot", (true, format!("Send the bot this code as a DM: {code}")));
            }
        }
        match &status {
            Some(s) => {
                let colour = if s.link == "online" { p.nominal } else if s.link.starts_with("stopped") { p.alert } else { p.muted };
                let who = if s.name.is_empty() { String::new() } else { format!(" as {}", s.name) };
                let paired = if s.owner_paired { " · your account paired" } else { " · not paired yet" };
                ui.label(RichText::new(format!("{}{who}{paired}", s.link.to_uppercase())).font(face(Face::Label, 9.0)).color(colour));
                if let Some(code) = &s.pairing_code {
                    ui.label(RichText::new(format!("CODE {code}")).font(face(Face::Mono, 12.0)).color(p.accent));
                }
            }
            None => {
                ui.label(RichText::new("Starts with JAI (not in test sessions)").font(face(Face::Body, 11.0)).color(p.muted));
            }
        }
    });
    if let Some((ok, text)) = lock(results).get("discord_bot").cloned() {
        ui.label(RichText::new(text).font(face(Face::Body, 11.5)).color(if ok { p.accent } else { p.alert }));
    }
    if let Some(note) = status.and_then(|s| s.note) {
        ui.add(egui::Label::new(RichText::new(note).font(face(Face::Body, 11.0)).color(p.warn)).wrap());
    }
    muted(ui, "Create it at discord.com/developers → New Application → Bot: copy the token here and switch on the Message Content Intent. Invite it with OAuth2 → URL Generator (scope bot; View Channels, Send Messages, Read Message History, Connect, Speak, Use Voice Activity). Bots join server voice channels, not private calls.");

    section(ui, "Away Mode");
    ui.horizontal_wrapped(|ui| {
        ui.label("On by itself once the PC has been locked for");
        ui.add(crate::holo_ui::Number::new(&mut away.auto_after_locked_mins).range(0..=240).suffix(" min"));
    });
    muted(ui, "0: only when you tell him you're leaving (\"I'm heading out\").");
    crate::holo_ui::toggle(ui, &mut away.answering_machine, "Answer friends' Discord messages while I'm away");
    crate::holo_ui::toggle(ui, &mut away.alert_on_unlock, "Tell my phone when someone uses the PC after I said I was leaving");
    crate::holo_ui::toggle(ui, &mut away.alert_on_usb, "Tell me when a drive is plugged in while I'm away");
}

impl Hub {
    /// A voice request opens the hub on the right tab.
    pub fn poll_request(&mut self) {
        if let Some(service) = lock(&REQUEST).take() {
            self.open = true;
            self.tab = Tab::of(service);
            self.draft = None;
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, p: &Palette, app: &Jai, rt: &Handle) {
        let pim = &app.tools().pim.clone();
        if self.quick.is_none() {
            self.quick = Some(app.config().read(|c| QuickDraft {
                whatsapp: c.whatsapp.clone(),
                satellite: c.satellite.clone(),
                location: c.location.clone(),
                acoustic: c.acoustic.clone(),
                discord_bot: c.discord_bot.clone(),
                away: c.away.clone(),
            }));
        }
        if self.field.is_none() {
            let f = app.config().read(|c| c.features.clone());
            self.field_servers = f.game_servers.join(", ");
            self.field = Some(f);
        }
        if self.draft.is_none() {
            self.draft = Some(pim.config().settings());
            self.tokens.clear();
            self.untis_session.clear();
        }
        let mut open = self.open;
        super::board_window(ctx, "integrations_hub", "Hub // integrations and productivity", [600.0, 700.0], &mut open, |ui| {
                muted(ui, "Or just ask: “JAI, check my school schedule”, “any new emails?”, “give me my morning briefing”.");
                // One button that verifies every connection, with status codes and previews.
                let registry = app.tools().stark.integrations.clone();
                ui.horizontal(|ui| {
                    let label = if registry.running() { "Diagnosing…" } else { "Test & Diagnose All Connections" };
                    if button(ui, label, Tone::On).clicked() && !registry.running() {
                        let tools = app.tools().clone();
                        rt.spawn(async move {
                            let _ = crate::integration_registry::run_tool(&tools).await;
                            request_repaint();
                        });
                    }
                    if crate::holo_ui::key(ui, "Phone pairing").on_hover_text("Show the QR code for the mobile satellite").clicked() {
                        super::show_pairing();
                    }
                });
                let reports = registry.last();
                if !reports.is_empty() {
                    crate::holo_ui::fold(ui, "hub_diagnosis", "Last diagnosis", true, |ui| {
                        for r in &reports {
                            let colour = match r.state.as_str() {
                                "ok" => p.nominal,
                                "off" => p.muted,
                                _ => p.alert,
                            };
                            ui.add(egui::Label::new(RichText::new(format!("{} {} — {} · {}", if r.ok() { "OK" } else if r.state == "off" { "OFF" } else { "ERR" }, r.name, r.status, r.detail)).font(face(Face::Body, 11.0)).color(colour)).wrap());
                        }
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    for (tab, label) in Tab::ALL {
                        crate::holo_ui::option(ui, &mut self.tab, tab, label);
                    }
                });
                crate::holo_ui::rule(ui);
                crate::holo_block::faded(ui, egui::ScrollArea::vertical().max_height(470.0).auto_shrink([false, true]), |ui| match self.tab {
                    Tab::Email => self.email_tab(ui, pim),
                    Tab::Calendar => self.calendar_tab(ui, pim),
                    Tab::WebUntis => self.webuntis_tab(ui, pim),
                    Tab::Lms => self.lms_tab(ui, pim),
                    Tab::Feeds => self.feeds_tab(ui),
                    Tab::Study => self.study_tab(ui, p, pim),
                    Tab::Quick => self.quick_tab(ui, p, app, rt),
                    Tab::Field => self.field_tab(ui, p, app, rt),
                });
                crate::holo_ui::rule(ui);
                if let Some(service) = self.tab.service() {
                    self.status_line(ui, p, pim, service);
                }
                ui.horizontal(|ui| {
                    if button(ui, "Save", Tone::On).clicked() {
                        if self.tab == Tab::Quick {
                            self.save_quick(app);
                        } else if self.tab == Tab::Field {
                            self.save_field(app);
                        } else {
                            self.save(pim);
                        }
                    }
                    if let Some(service) = self.tab.service() {
                        if crate::holo_ui::key(ui, "Save & test connection").clicked() && self.save(pim) {
                            self.test(pim, rt, service);
                        }
                    }
                    if crate::holo_ui::key(ui, "Revert").clicked() {
                        self.draft = None;
                        self.quick = None;
                        self.field = None;
                        self.message = None;
                    }
                });
                if let Some((text, error)) = &self.message {
                    ui.label(RichText::new(text).font(face(Face::Body, 11.0)).color(if *error { p.alert } else { p.nominal }));
                }
                muted(ui, format!("Tokens are sealed with AES-256-GCM; master key in {}.", pim.config().vault().key_store()));
            });
        self.open = open;
        if !self.open {
            self.draft = None;
            self.quick = None;
            self.field = None;
        }
    }

    /// Writes the field-system settings (the replay buffer restarts with JAI).
    fn save_field(&mut self, app: &Jai) {
        let Some(mut f) = self.field.clone() else { return };
        f.game_servers = self.field_servers.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        app.config().update(|c| c.features = f.clone());
        self.field = Some(f);
        self.message = Some(("Saved. Instant Replay changes apply after a restart.".into(), false));
    }

    /// The fifteen field systems: live state, one-click actions, settings.
    fn field_tab(&mut self, ui: &mut Ui, p: &Palette, app: &Jai, rt: &Handle) {
        let tools = app.tools().clone();
        let f = tools.stark.features.clone();
        ui.label(RichText::new("FIELD SYSTEMS").font(bold(12.0)).color(p.text));
        muted(ui, "Say “clip that”, “quiz me on …”, “why am I lagging”, “clean up my downloads”, “wake me at 6:30 with JaysRock”, “find me an RTX 3080 under 350 euros”.");
        let rows = [
            ("Instant Replay", if f.replay.is_running() { "recording the rolling buffer".to_string() } else { "off".to_string() }),
            ("Smart Alarm", f.alarm.describe().unwrap_or_else(|| "no alarm set".into())),
            ("Deal Sniper", format!("{} sniper(s) watching", f.deals.active_count())),
            ("Exam Coach", f.exam.describe().unwrap_or_else(|| "idle".into())),
            ("Media Grabber", f.media.describe().unwrap_or_else(|| "idle".into())),
        ];
        for (name, state) in rows {
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).font(face(Face::Strong, 11.5)).color(p.text));
                ui.label(RichText::new(state).font(face(Face::Body, 11.5)).color(p.muted));
            });
        }
        ui.add_space(6.0);
        // One click runs the tool; its result window opens and its short line shows as a notice.
        let run = |name: &'static str, args: serde_json::Value| {
            let tools = tools.clone();
            rt.spawn(async move {
                let out = tools.execute(name, &args).await;
                let line = out.output.rsplit("\nSpoken: ").next().unwrap_or(&out.output).chars().take(200).collect::<String>();
                let _ = tools.stark.events.send(crate::stark::StarkEvent::Notice(line));
                request_repaint();
            });
        };
        ui.horizontal_wrapped(|ui| {
            if crate::holo_ui::key(ui, "Save replay").clicked() {
                run("save_instant_replay", json!({}));
            }
            if crate::holo_ui::key(ui, "Preview Downloads cleanup").clicked() {
                run("smart_folder_cleanup", json!({ "target_folder": "downloads", "dry_run_first": true }));
            }
            if crate::holo_ui::key(ui, "Undo cleanup").clicked() {
                run("undo_last_cleanup", json!({}));
            }
            if crate::holo_ui::key(ui, "Network check").clicked() {
                run("diagnose_network_and_ping", json!({ "action": "full" }));
            }
            if crate::holo_ui::key(ui, "Finances").clicked() {
                run("manage_finances_and_goals", json!({ "action": "summary" }));
            }
            if crate::holo_ui::key(ui, "Check deals").clicked() {
                run("manage_price_sniper", json!({ "action": "check_now" }));
            }
            if crate::holo_ui::key(ui, "Minecraft doctor").clicked() {
                run("diagnose_minecraft_or_server", json!({ "action": "diagnose" }));
            }
            if crate::holo_ui::key(ui, "Install yt-dlp + FFmpeg").on_hover_text("Through winget, for the Media Grabber").clicked() {
                run("download_or_extract_media", json!({ "action": "install_tools", "url_or_active_browser": "" }));
            }
        });
        crate::holo_ui::rule(ui);
        let Some(s) = self.field.as_mut() else { return };
        egui::Grid::new("field_settings").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Instant Replay");
            ui.horizontal(|ui| {
                crate::holo_ui::toggle(ui, &mut s.replay_enabled, "rolling buffer");
                ui.add(crate::holo_ui::Number::new(&mut s.replay_seconds).range(10..=600).suffix(" s"));
                ui.add(crate::holo_ui::Number::new(&mut s.replay_fps).range(0.2..=5.0).speed(0.1).suffix(" fps"));
            });
            ui.end_row();
            ui.label("Projects folder");
            text_field(ui, &mut s.projects_dir, "D:\\Code (default) or another folder");
            ui.end_row();
            ui.label("Game servers to ping");
            text_field(ui, &mut self.field_servers, "euw.leagueoflegends.com, 1.2.3.4");
            ui.end_row();
            ui.label("Deal checks every");
            ui.add(crate::holo_ui::Number::new(&mut s.deal_interval_min).range(5..=1440).suffix(" min"));
            ui.end_row();
            ui.label("Minecraft folder");
            text_field(ui, &mut s.minecraft_dir, "%APPDATA%\\.minecraft (auto)");
            ui.end_row();
            ui.label("Minecraft server");
            text_field(ui, &mut s.minecraft_server, "host:25565");
            ui.end_row();
            ui.label("RCON");
            ui.horizontal(|ui| {
                crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut s.rcon_address).hint_text("host:25575").desired_width(140.0));
                crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut s.rcon_password).password(true).hint_text("password").desired_width(140.0));
            });
            ui.end_row();
            ui.label("SSH target");
            text_field(ui, &mut s.ssh_target, "user@host (key login)");
            ui.end_row();
            ui.label("Alarm fade-in");
            ui.horizontal(|ui| {
                ui.add(crate::holo_ui::Number::new(&mut s.alarm_fade_secs).range(10..=900).suffix(" s"));
                ui.add(crate::holo_ui::Number::new(&mut s.alarm_volume).range(5..=100).suffix(" % volume"));
            });
            ui.end_row();
        });
    }

    /// Writes the Quick Connections settings (and a new bridge token) and applies them live.
    fn save_quick(&mut self, app: &Jai) {
        let Some(q) = self.quick.clone() else { return };
        app.config().update(|c| {
            c.whatsapp = q.whatsapp.clone();
            c.satellite = q.satellite.clone();
            c.location = q.location.clone();
            c.acoustic = q.acoustic.clone();
            c.discord_bot = q.discord_bot.clone();
            c.away = q.away.clone();
        });
        app.set_acoustic(&q.acoustic);
        let mut note = "Saved. Satellite port or HTTPS changes apply after a restart.".to_string();
        // JAI's Discord token: sealed in the vault, never in the settings file.
        if !self.discord_token.trim().is_empty() {
            match crate::discord_bot::clean_token(&self.discord_token) {
                Some(token) => match app.tools().pim.config().vault().set_secret(crate::discord_bot::VAULT_SERVICE, crate::vault::TOKEN, &token) {
                    Ok(()) => self.discord_token.clear(),
                    Err(e) => note = format!("Saved, but the Discord token could not be stored: {e}"),
                },
                None => note = "Saved, but that is not a bot token: copy it from the Developer Portal's Bot page (Reset Token).".to_string(),
            }
        }
        // The account reconnects with the new token or settings.
        crate::discord_bot::kick();
        // Street view's Mapillary token: sealed in the vault too (never asked for in chat).
        if !self.mapillary_token.trim().is_empty() {
            match crate::street_view::clean_token(&self.mapillary_token) {
                Some(token) => match app.tools().pim.config().vault().set_secret(crate::street_view::VAULT_SERVICE, crate::vault::TOKEN, &token) {
                    Ok(()) => self.mapillary_token.clear(),
                    Err(e) => note = format!("Saved, but the Mapillary token could not be stored: {e}"),
                },
                None => note = "Saved, but that is not a Mapillary client token: copy the one starting with MLY| from your app at mapillary.com/dashboard/developers.".to_string(),
            }
        }
        if !self.wa_token.trim().is_empty() {
            match app.tools().pim.config().vault().set_secret(crate::whatsapp_client::VAULT_SERVICE, crate::vault::TOKEN, self.wa_token.trim()) {
                Ok(()) => self.wa_token.clear(),
                Err(e) => note = format!("Saved, but the bridge token could not be stored: {e}"),
            }
        }
        // The token of the permanent address: sealed in the vault, never in the settings file.
        if !self.tunnel_token.trim().is_empty() {
            match crate::worldwide_tunnel::clean_token(&self.tunnel_token) {
                Some(token) => match app.tools().pim.config().vault().set_secret(crate::worldwide_tunnel::TOKEN_SERVICE, crate::vault::TOKEN, &token) {
                    Ok(()) => self.tunnel_token.clear(),
                    Err(e) => note = format!("Saved, but the tunnel token could not be stored: {e}"),
                },
                None => note = "Saved, but that is not a Cloudflare tunnel token: copy the long text starting with eyJ from the tunnel's page.".to_string(),
            }
        }
        // The tunnel takes a new address or token up at once.
        crate::worldwide_tunnel::kick();
        self.quick = None;
        self.qr = None;
        self.message = Some((note, false));
    }

    fn quick_tab(&mut self, ui: &mut Ui, p: &Palette, app: &Jai, rt: &Handle) {
        let Some(mut q) = self.quick.take() else { return };
        let tools = app.tools();
        let section = |ui: &mut Ui, title: &str| {
            ui.add_space(6.0);
            ui.label(RichText::new(title).font(bold(12.5)).color(p.text));
        };

        // ---- WhatsApp -------------------------------------------------------------------
        section(ui, "WhatsApp");
        crate::holo_ui::toggle(ui, &mut q.whatsapp.notification_listener, "Read incoming messages from WhatsApp Desktop's notifications");
        crate::holo_ui::toggle(ui, &mut q.whatsapp.desktop_auto_send, "\"Send it now\": press Enter in WhatsApp Desktop once the chat is open");
        egui::Grid::new("hub_whatsapp").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
            ui.label("Local bridge URL");
            text_field(ui, &mut q.whatsapp.bridge_url, "http://127.0.0.1:3000 (optional)");
            ui.end_row();
            ui.label("Bridge type");
            crate::holo_ui::Picker::from_id_salt("hub_wa_kind").selected_text(q.whatsapp.bridge_kind.clone()).show_ui(ui, |ui| {
                crate::holo_ui::option(ui, &mut q.whatsapp.bridge_kind, "generic".to_string(), "generic (GET /messages, POST /send)");
                crate::holo_ui::option(ui, &mut q.whatsapp.bridge_kind, "waha".to_string(), "WAHA (POST /api/sendText)");
            });
            ui.end_row();
            ui.label("WAHA session");
            text_field(ui, &mut q.whatsapp.bridge_session, "default");
            ui.end_row();
            ui.label("Bridge token");
            let stored = tools.pim.config().vault().has_secret(crate::whatsapp_client::VAULT_SERVICE, crate::vault::TOKEN);
            secret_field(ui, &mut self.wa_token, stored);
            ui.end_row();
            ui.label("Country code");
            crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut q.whatsapp.default_country_code).desired_width(70.0));
            ui.end_row();
            ui.label("Check every");
            ui.add(crate::holo_ui::Number::new(&mut q.whatsapp.poll_seconds).range(5..=600).suffix(" s"));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if crate::holo_ui::key(ui, "Test bridge").clicked() {
                lock(&self.results).insert("whatsapp_bridge", (true, "Testing the bridge…".into()));
                let (wa, settings, token, results) = (tools.stark.whatsapp.clone(), q.whatsapp.clone(), tools.pim.config().vault().secret(crate::whatsapp_client::VAULT_SERVICE, crate::vault::TOKEN), Arc::clone(&self.results));
                rt.spawn(async move {
                    let out = match wa.test_bridge(&settings, token).await {
                        Ok(t) => (true, format!("OK: {t}")),
                        Err(e) => (false, format!("FAIL: {e}")),
                    };
                    lock(&results).insert("whatsapp_bridge", out);
                    request_repaint();
                });
            }
            if let Some((ok, text)) = lock(&self.results).get("whatsapp_bridge").cloned() {
                ui.label(RichText::new(text).font(face(Face::Body, 11.0)).color(if ok { p.nominal } else { p.alert }));
            }
        });
        muted(ui, format!(
            "{} unread on record. Bridges can also push messages to the satellite: POST {}/api/whatsapp/incoming?k=<key>.",
            tools.stark.whatsapp.unread_count(),
            crate::satellite::pairing_url(app.config()).split("/satellite").next().unwrap_or("")
        ));

        // ---- JAI's own Discord account and Away Mode -------------------------------------
        let bot_stored = tools.pim.config().vault().has_secret(crate::discord_bot::VAULT_SERVICE, crate::vault::TOKEN);
        account_and_away(ui, p, &mut q.discord_bot, &mut q.away, &mut self.discord_token, bot_stored, &self.results);

        // ---- Street view (Mapillary, optional) ------------------------------------------
        section(ui, "Street view (Mapillary, optional)");
        let mapillary_stored = tools.pim.config().vault().has_secret(crate::street_view::VAULT_SERVICE, crate::vault::TOKEN);
        egui::Grid::new("hub_mapillary").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
            ui.label("Client token");
            secret_field(ui, &mut self.mapillary_token, mapillary_stored);
            ui.end_row();
        });
        if mapillary_stored && crate::holo_ui::key(ui, "Forget the Mapillary token").on_hover_text("Street view then uses Panoramax and KartaView only").clicked() {
            tools.pim.config().vault().clear_secret(crate::street_view::VAULT_SERVICE, crate::vault::TOKEN);
        }
        muted(ui, "Without a token street view uses Panoramax and KartaView (open, no account). With a free Mapillary client token (MLY|…, from mapillary.com/dashboard/developers) Mapillary's photos come first. Stored encrypted, never in the settings file.");
        // ---- Sketchfab (the 3D studio's full models) ----------------------------------------
        super::model3d_view::sketchfab_section(ui, p, tools.pim.config().vault(), rt);

        // ---- The mobile satellite -------------------------------------------------------
        section(ui, "Mobile satellite (phone / iPad with live GPS)");
        ui.horizontal(|ui| {
            crate::holo_ui::toggle(ui, &mut q.satellite.enabled, "Enabled");
            crate::holo_ui::toggle(ui, &mut q.satellite.https, "HTTPS (needed for GPS)");
            ui.label("Port");
            ui.add(crate::holo_ui::Number::new(&mut q.satellite.port).range(1024..=65535));
        });
        let url = crate::satellite::pairing_url(app.config());
        if self.qr.as_ref().is_none_or(|(u, _, _)| *u != url) {
            self.qr = crate::satellite::qr_matrix(&url).map(|(side, m)| (url.clone(), side, m));
        }
        ui.horizontal(|ui| {
            if let Some((_, side, modules)) = &self.qr {
                let cell = 4.0;
                let quiet = 4.0 * cell;
                let size = *side as f32 * cell + 2.0 * quiet;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
                ui.painter().rect_filled(rect, egui::Rounding::ZERO, egui::Color32::WHITE);
                for (i, dark) in modules.iter().enumerate() {
                    if *dark {
                        let (x, y) = ((i % side) as f32, (i / side) as f32);
                        let min = rect.min + egui::vec2(quiet + x * cell, quiet + y * cell);
                        ui.painter().rect_filled(egui::Rect::from_min_size(min, egui::vec2(cell, cell)), egui::Rounding::ZERO, egui::Color32::BLACK);
                    }
                }
            }
            ui.vertical(|ui| {
                ui.label(RichText::new("Scan with your phone's camera:").font(face(Face::Body, 11.5)).color(p.text));
                ui.add(egui::Label::new(RichText::new(&url).font(face(Face::Mono, 11.0)).color(p.accent)).wrap());
                if crate::holo_ui::key(ui, "Copy link").clicked() {
                    ui.output_mut(|o| o.copied_text = url.clone());
                }
                muted(ui, "The certificate is self-signed: accept the browser's warning once (or install it from /satellite/cert.der), then allow location access.");
                if !crate::satellite::has_certificate() && q.satellite.https {
                    muted(ui, "(The certificate is created when the satellite starts.)");
                }
                if crate::holo_ui::key(ui, "New pairing key").on_hover_text("Unpairs every device that used the old QR code").clicked() {
                    q.satellite.token.clear();
                }
                let fix = tools.stark.location.phone_fix();
                let (colour, text) = match fix {
                    Some(f) => (p.nominal, format!("GPS LOCK ACTIVE · {:.5}, {:.5} ±{:.0} m", f.lat, f.lon, f.accuracy_m)),
                    None => (p.muted, "No phone connected".to_string()),
                };
                ui.label(RichText::new(text).font(face(Face::Body, 11.0)).color(colour));
            });
        });

        // ---- The worldwide address ------------------------------------------------------
        section(ui, "Worldwide address (the phone page away from home)");
        crate::holo_ui::toggle(ui, &mut q.satellite.worldwide_enabled, "Reachable from anywhere through an HTTPS tunnel");
        let provider = match q.satellite.tunnel_provider.to_ascii_lowercase().as_str() {
            "tailscale" => "tailscale",
            "ssh" => "ssh",
            _ => "cloudflare",
        };
        let vault = tools.pim.config().vault();
        let token_stored = vault.has_secret(crate::worldwide_tunnel::TOKEN_SERVICE, crate::vault::TOKEN);
        egui::Grid::new("hub_tunnel").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
            ui.label("Tunnel");
            let shown = match provider {
                "tailscale" => "Tailscale Funnel",
                "ssh" => "Own server (SSH)",
                _ => "Cloudflare",
            };
            crate::holo_ui::Picker::from_id_salt("hub_tunnel_provider").selected_text(shown).show_ui(ui, |ui| {
                crate::holo_ui::option(ui, &mut q.satellite.tunnel_provider, "cloudflare".to_string(), "Cloudflare");
                crate::holo_ui::option(ui, &mut q.satellite.tunnel_provider, "tailscale".to_string(), "Tailscale Funnel");
                crate::holo_ui::option(ui, &mut q.satellite.tunnel_provider, "ssh".to_string(), "Own server (SSH)");
            });
            ui.end_row();
            if provider != "tailscale" {
                ui.label("Permanent address");
                text_field(ui, &mut q.satellite.tunnel_public_url, "https://jai.your-domain (empty: new every start)");
                ui.end_row();
            }
            if provider == "cloudflare" {
                ui.label("Tunnel token");
                secret_field(ui, &mut self.tunnel_token, token_stored);
                ui.end_row();
            }
            if provider == "ssh" {
                ui.label("SSH target");
                text_field(ui, &mut q.satellite.tunnel_ssh_target, "user@your-server");
                ui.end_row();
                ui.label("SSH key file");
                text_field(ui, &mut q.satellite.tunnel_ssh_key, "optional: path to the private key");
                ui.end_row();
            }
        });
        let address = q.satellite.tunnel_public_url.trim().to_string();
        if provider != "tailscale" && !address.is_empty() && crate::worldwide_tunnel::https_origin(&address).is_none() {
            ui.label(RichText::new("The address must look like https://jai.your-domain, without a path.").font(face(Face::Body, 11.0)).color(p.alert));
        }
        let tunnel = crate::worldwide_tunnel::snapshot();
        let (colour, line) = match (&tunnel.public_url, tunnel.permanent) {
            (Some(url), _) if tunnel.tailnet_only => (p.warn, format!("ONLINE INSIDE YOUR TAILNET // {url} // {}", tunnel.detail)),
            (Some(url), true) => (p.nominal, format!("PERMANENT ADDRESS ONLINE // {url}")),
            (Some(url), false) => (p.warn, format!("ONLINE // {url} // this address is new whenever the tunnel restarts")),
            (None, _) => match crate::worldwide_tunnel::configured_address(&q.satellite, token_stored) {
                Some(fixed) => (p.muted, format!("{} // {fixed} // {}", tunnel.state.to_uppercase(), tunnel.detail)),
                None => (p.muted, format!("{} // {}", tunnel.state.to_uppercase(), tunnel.detail)),
            },
        };
        ui.add(egui::Label::new(RichText::new(line).font(face(Face::Body, 11.0)).color(colour)).wrap());
        match provider {
            "cloudflare" => {
                muted(ui, format!(
                    "One address for good needs a domain of your own at Cloudflare (the free plan will do): in Zero Trust open Networks, Tunnels, create a tunnel (Cloudflared), paste its token here, and give it a public hostname with the service HTTP 127.0.0.1:{}. Without a token a quick tunnel is used, whose address is new on every start.",
                    q.satellite.tunnel_origin_port
                ));
                if token_stored && crate::holo_ui::key(ui, "Forget the tunnel token").on_hover_text("Back to a quick tunnel with a changing address").clicked() {
                    vault.clear_secret(crate::worldwide_tunnel::TOKEN_SERVICE, crate::vault::TOKEN);
                    crate::worldwide_tunnel::kick();
                }
            }
            "tailscale" => muted(ui, "Tailscale Funnel gives this PC one address for good (https://pc-name.your-tailnet.ts.net) without a domain: install Tailscale on this PC, sign in and allow Funnel once. JAI starts it himself."),
            _ => muted(ui, "Your own server: an HTTPS reverse proxy on it forwards to the SSH port; the permanent address is the proxy's."),
        }

        // ---- Location, geofence & commute -----------------------------------------------
        section(ui, "Location, home geofence & commute");
        let coord = |ui: &mut Ui, lat: &mut Option<f64>, lon: &mut Option<f64>| {
            let (mut a, mut b) = (lat.unwrap_or(0.0), lon.unwrap_or(0.0));
            let ra = ui.add(crate::holo_ui::Number::new(&mut a).speed(0.0001).range(-90.0..=90.0).max_decimals(6).prefix("lat "));
            let rb = ui.add(crate::holo_ui::Number::new(&mut b).speed(0.0001).range(-180.0..=180.0).max_decimals(6).prefix("lon "));
            if ra.changed() || rb.changed() {
                *lat = Some(a);
                *lon = Some(b);
            }
        };
        egui::Grid::new("hub_location").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
            ui.label("Home");
            ui.horizontal(|ui| {
                coord(ui, &mut q.location.home_lat, &mut q.location.home_lon);
                if crate::holo_ui::key(ui, "From phone").on_hover_text("Use the phone's current GPS position").clicked() {
                    if let Some(f) = tools.stark.location.phone_fix() {
                        q.location.home_lat = Some(f.lat);
                        q.location.home_lon = Some(f.lon);
                        q.location.home_label = f.address.unwrap_or_default();
                    }
                }
            });
            ui.end_row();
            ui.label("School");
            ui.horizontal(|ui| {
                coord(ui, &mut q.location.school_lat, &mut q.location.school_lon);
                if crate::holo_ui::key(ui, "From phone").clicked() {
                    if let Some(f) = tools.stark.location.phone_fix() {
                        q.location.school_lat = Some(f.lat);
                        q.location.school_lon = Some(f.lon);
                        q.location.school_label = f.address.unwrap_or_default();
                    }
                }
            });
            ui.end_row();
            ui.label("Arrival playlist");
            text_field(ui, &mut q.location.arrival_playlist, "e.g. JaysRock (empty = none)");
            ui.end_row();
            ui.label("Arrival protocol");
            text_field(ui, &mut q.location.arrival_protocol, "a Stark protocol (empty = none)");
            ui.end_row();
            ui.label("Travel by");
            crate::holo_ui::Picker::from_id_salt("hub_travel").selected_text(q.location.default_travel_mode.clone()).show_ui(ui, |ui| {
                for m in ["motorcycle", "car", "bike", "walk", "transit"] {
                    crate::holo_ui::option(ui, &mut q.location.default_travel_mode, m.to_string(), m);
                }
            });
            ui.end_row();
            ui.label("Commute buffer");
            ui.add(crate::holo_ui::Number::new(&mut q.location.commute_buffer_min).range(0..=120).suffix(" min"));
            ui.end_row();
        });
        ui.horizontal_wrapped(|ui| {
            crate::holo_ui::toggle(ui, &mut q.location.geofence_enabled, "Home geofence");
            crate::holo_ui::toggle(ui, &mut q.location.arrival_wake_hud, "Wake the HUD on arrival");
            crate::holo_ui::toggle(ui, &mut q.location.arrival_greeting, "Greet me");
            crate::holo_ui::toggle(ui, &mut q.location.ip_fallback, "IP location fallback");
        });
        muted(ui, format!("Arrival fires when the phone comes back within {:.0} m after being farther than {:.0} m away.", q.location.geofence_arrive_m, q.location.geofence_leave_m));

        // ---- Acoustic radar ----------------------------------------------------------------
        section(ui, "Acoustic radar");
        ui.horizontal_wrapped(|ui| {
            crate::holo_ui::toggle(ui, &mut q.acoustic.double_clap, "Double clap");
            crate::holo_ui::Picker::from_id_salt("hub_clap").selected_text(if q.acoustic.clap_action == "toggle_music" { "toggles the music" } else { "wakes JAI" }).show_ui(ui, |ui| {
                crate::holo_ui::option(ui, &mut q.acoustic.clap_action, "wake".to_string(), "wakes JAI");
                crate::holo_ui::option(ui, &mut q.acoustic.clap_action, "toggle_music".to_string(), "toggles the music");
            });
            crate::holo_ui::toggle(ui, &mut q.acoustic.knock_alert, "Door-knock alert");
            crate::holo_ui::toggle(ui, &mut q.acoustic.knock_only_with_music, "only while music plays");
        });
        ui.add(crate::holo_ui::Slider::new(&mut q.acoustic.sensitivity, 0.5..=2.0).text("sensitivity"));

        // ---- Remote nodes ------------------------------------------------------------------
        section(ui, "Hivemind nodes (SSH)");
        for n in tools.stark.hivemind.list() {
            muted(ui, format!("• {}", n.describe()));
        }
        ui.horizontal(|ui| {
            for (i, hint) in ["name", "host / IP", "ssh user", "kind (pi, vps, minecraft)"].iter().enumerate() {
                crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut self.node_form[i]).hint_text(*hint).desired_width(if i == 3 { 150.0 } else { 100.0 }));
            }
            if crate::holo_ui::key(ui, "+ Add").clicked() && !self.node_form[0].trim().is_empty() && !self.node_form[1].trim().is_empty() {
                let mut node = crate::hivemind_mesh::RemoteNode {
                    name: self.node_form[0].trim().to_string(),
                    host: self.node_form[1].trim().to_string(),
                    ssh_user: self.node_form[2].trim().to_string(),
                    kind: self.node_form[3].trim().to_lowercase(),
                    ssh_port: 22,
                    ..Default::default()
                };
                if node.kind == "minecraft" {
                    node.game_port = 25565;
                }
                self.message = Some(match tools.stark.hivemind.save(&node) {
                    Ok(()) => {
                        self.node_form = Default::default();
                        (format!("Added {}.", node.name), false)
                    }
                    Err(e) => (e, true),
                });
            }
        });
        ui.horizontal(|ui| {
            if crate::holo_ui::key(ui, "Check all nodes").clicked() {
                lock(&self.results).insert("nodes", (true, "Checking…".into()));
                let (tools, results) = (tools.clone(), Arc::clone(&self.results));
                rt.spawn(async move {
                    let out = match crate::hivemind_mesh::run_tool(&tools, &json!({ "action": "status" })).await {
                        Ok(t) => (true, t),
                        Err(e) => (false, e),
                    };
                    lock(&results).insert("nodes", out);
                    request_repaint();
                });
            }
            if let Some((ok, text)) = lock(&self.results).get("nodes").cloned() {
                ui.add(egui::Label::new(RichText::new(text).font(face(Face::Body, 11.0)).color(if ok { p.text } else { p.alert })).wrap());
            }
        });

        // ---- Contacts ----------------------------------------------------------------------
        section(ui, "Contacts");
        let contacts = tools.stark.contacts.list();
        muted(ui, format!("{} contacts{}", contacts.len(), if contacts.is_empty() { String::new() } else { format!(": {}", contacts.iter().take(12).map(|c| c.name.clone()).collect::<Vec<_>>().join(", ")) }));
        ui.horizontal(|ui| {
            for (i, hint) in ["name", "phone (+49 …)", "WhatsApp name"].iter().enumerate() {
                crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut self.contact_form[i]).hint_text(*hint).desired_width(120.0));
            }
            if crate::holo_ui::key(ui, "+ Save contact").clicked() && !self.contact_form[0].trim().is_empty() {
                let c = crate::contacts::Contact {
                    name: self.contact_form[0].trim().to_string(),
                    phone_number: self.contact_form[1].trim().to_string(),
                    whatsapp_name: self.contact_form[2].trim().to_string(),
                    ..Default::default()
                };
                self.message = Some(match tools.stark.contacts.upsert(&c, &q.whatsapp.default_country_code) {
                    Ok(saved) => {
                        self.contact_form = Default::default();
                        (format!("Saved {}.", saved.describe()), false)
                    }
                    Err(e) => (e, true),
                });
            }
        });
        self.quick = Some(q);
    }

    fn status_line(&self, ui: &mut Ui, p: &Palette, pim: &Pim, service: &'static str) {
        let running = lock(&self.results).get(service).cloned();
        let (colour, text) = match running {
            Some((ok, text)) => (if ok { p.nominal } else { p.alert }, text),
            None => {
                let st = pim.config().status(service);
                match st.state.as_str() {
                    "connected" => (p.nominal, format!("Connected · {} · {}", st.last_checked, st.detail)),
                    "error" => (p.alert, format!("Error · {} · {}", st.last_checked, st.detail)),
                    _ => (p.muted, pim.config().missing(service).unwrap_or_else(|| "Not tested yet".into())),
                }
            }
        };
        ui.add(egui::Label::new(RichText::new(text).font(face(Face::Body, 11.0)).color(colour)).wrap());
    }

    /// Writes the draft and any new tokens. False when nothing could be saved.
    fn save(&mut self, pim: &Pim) -> bool {
        let Some(mut draft) = self.draft.clone() else { return false };
        // Watchers are started and stopped live (here or by voice); the draft never overrides them.
        draft.watchers = pim.config().read(|s| s.watchers.clone());
        let mut errors = Vec::new();
        if let Err(e) = pim.config().save(&draft) {
            errors.push(e);
        }
        for (service, token) in self.tokens.drain() {
            if !token.trim().is_empty() {
                if let Err(e) = pim.config().set_token(service, &token) {
                    errors.push(e);
                }
            }
        }
        if !self.untis_session.trim().is_empty() {
            if let Err(e) = pim.config().set_session(WEBUNTIS, &self.untis_session) {
                errors.push(e);
            }
            self.untis_session.clear();
        }
        self.message = Some(if errors.is_empty() { ("Saved.".into(), false) } else { (errors.join("; "), true) });
        errors.is_empty()
    }

    fn test(&self, pim: &Pim, rt: &Handle, service: &'static str) {
        lock(&self.results).insert(service, (true, "Testing the connection…".into()));
        let (pim, results) = (pim.clone(), Arc::clone(&self.results));
        rt.spawn(async move {
            let outcome = match pim.test_service(service).await {
                Ok(text) => (true, format!("OK: {text}")),
                Err(e) => (false, format!("FAIL: {e}")),
            };
            lock(&results).insert(service, outcome);
            request_repaint();
        });
    }

    fn token_row(&mut self, ui: &mut Ui, pim: &Pim, service: &'static str, label: &str) {
        ui.label(label);
        ui.horizontal(|ui| {
            let stored = pim.config().has_token(service);
            secret_field(ui, self.tokens.entry(service).or_default(), stored);
            if stored && crate::holo_ui::key(ui, "Forget").on_hover_text("Remove the stored token from the vault").clicked() {
                let _ = pim.config().set_token(service, "");
                self.message = Some((format!("Removed the {} token.", pim_config::display(service)), false));
            }
            if !stored {
                muted(ui, "none stored");
            }
        });
        ui.end_row();
    }

    fn email_tab(&mut self, ui: &mut Ui, pim: &Pim) {
        let Some(mut d) = self.draft.take() else { return };
        crate::holo_ui::toggle(ui, &mut d.email.enabled, "Use my e-mail (IMAP over TLS)");
        egui::Grid::new("hub_email").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
            ui.label("IMAP host");
            text_field(ui, &mut d.email.imap_host, "imap.gmail.com");
            ui.end_row();
            ui.label("Port");
            ui.add(crate::holo_ui::Number::new(&mut d.email.imap_port).range(1..=65535));
            ui.end_row();
            ui.label("E-mail address");
            text_field(ui, &mut d.email.address, "sir@gmail.com");
            ui.end_row();
            ui.label("Mailbox");
            text_field(ui, &mut d.email.mailbox, "INBOX");
            ui.end_row();
            self.token_row(ui, pim, EMAIL, "App token");
        });
        muted(ui, "Gmail: turn on 2-step verification, create an app password (Google Account → Security → App passwords) and paste it here. JAI only reads (EXAMINE + BODY.PEEK); nothing is marked as read.");
        self.draft = Some(d);
    }

    fn calendar_tab(&mut self, ui: &mut Ui, pim: &Pim) {
        let Some(mut d) = self.draft.take() else { return };
        crate::holo_ui::toggle(ui, &mut d.calendar.enabled, "Sync my calendar (events are always kept offline too)");
        egui::Grid::new("hub_calendar").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
            ui.label("CalDAV server");
            text_field(ui, &mut d.calendar.caldav_url, "https://caldav.icloud.com");
            ui.end_row();
            ui.label("Username");
            text_field(ui, &mut d.calendar.username, "Apple ID e-mail");
            ui.end_row();
            self.token_row(ui, pim, CALENDAR, "App-specific password");
            ui.label("New events go to");
            text_field(ui, &mut d.calendar.calendar_name, "calendar name (empty = the first)");
            ui.end_row();
            ui.label("iCal feed (.ics)");
            text_field(ui, &mut d.calendar.ics_url, "webcal://… (optional, read-only)");
            ui.end_row();
        });
        muted(ui, "iCloud: appleid.apple.com → Sign-In and Security → App-Specific Passwords. Any CalDAV server (Nextcloud, Fastmail …) works with its server or calendar URL.");
        self.draft = Some(d);
    }

    fn webuntis_tab(&mut self, ui: &mut Ui, pim: &Pim) {
        let Some(mut d) = self.draft.take() else { return };
        crate::holo_ui::toggle(ui, &mut d.webuntis.enabled, "Read my school timetable");
        egui::Grid::new("hub_untis").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
            ui.label("School server");
            text_field(ui, &mut d.webuntis.server, "hebe.webuntis.com");
            ui.end_row();
            ui.label("School name");
            text_field(ui, &mut d.webuntis.school, "as in the WebUntis URL (?school=…)");
            ui.end_row();
            ui.label("Username");
            text_field(ui, &mut d.webuntis.username, "WebUntis login");
            ui.end_row();
            self.token_row(ui, pim, WEBUNTIS, "Password");
            ui.label("Class");
            text_field(ui, &mut d.webuntis.class_name, "e.g. 10b (optional)");
            ui.end_row();
            ui.label("Student id");
            text_field(ui, &mut d.webuntis.student_id, "element id (optional)");
            ui.end_row();
            ui.label("Client name");
            text_field(ui, &mut d.webuntis.client_name, "JAI");
            ui.end_row();
            ui.label("Session id (SSO)");
            ui.horizontal(|ui| {
                crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut self.untis_session).password(true).hint_text("JSESSIONID from the browser (optional)").desired_width(300.0));
                if pim.config().session(WEBUNTIS).is_some() && crate::holo_ui::key(ui, "Forget").on_hover_text("Remove the stored session id").clicked() {
                    let _ = pim.config().set_session(WEBUNTIS, "");
                }
            });
            ui.end_row();
        });
        muted(ui, "Cancelled periods show as Entfall / free period, substitutions and room changes as Vertretung. Sessions are renewed after 8 idle minutes.");
        self.draft = Some(d);
    }

    fn lms_tab(&mut self, ui: &mut Ui, pim: &Pim) {
        let Some(mut d) = self.draft.take() else { return };
        crate::holo_ui::toggle(ui, &mut d.lms.enabled, "Track my itslearning courses");
        egui::Grid::new("hub_lms").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
            ui.label("Portal URL");
            text_field(ui, &mut d.lms.base_url, "https://myschool.itslearning.com");
            ui.end_row();
            ui.label("Username");
            text_field(ui, &mut d.lms.username, "itslearning login (REST)");
            ui.end_row();
            self.token_row(ui, pim, LMS, "Password");
            ui.label("Feed URL");
            text_field(ui, &mut d.lms.feed_url, "RSS / notification feed (optional)");
            ui.end_row();
            ui.label("REST endpoint");
            text_field(ui, &mut d.lms.rest_endpoint, "custom path or URL (optional)");
            ui.end_row();
            ui.label("Keep-alive");
            ui.add(crate::holo_ui::Number::new(&mut d.lms.keepalive_minutes).range(1..=60).suffix(" min"));
            ui.end_row();
        });
        muted(ui, "The session is refreshed whenever it has been idle longer than the keep-alive interval (itslearning logs out after ~10 minutes). Schools with Feide / Microsoft single sign-on: use the feed URL.");
        self.draft = Some(d);
    }

    fn feeds_tab(&mut self, ui: &mut Ui) {
        let Some(mut d) = self.draft.take() else { return };
        muted(ui, "Any page or JSON endpoint: give it a name, a CSS selector (.status, #news li) or JSON path ($.data.items[0].title) and how often a watcher may check it.");
        let mut remove = None;
        for (i, f) in d.feeds.iter_mut().enumerate() {
            crate::holo_ui::plate(ui, 8.0, 0.0, |ui| {
                egui::Grid::new(("hub_feed", i)).num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
                    ui.label("Name");
                    text_field(ui, &mut f.name, "Mensa plan");
                    ui.end_row();
                    ui.label("URL");
                    text_field(ui, &mut f.url, "https://…");
                    ui.end_row();
                    ui.label("Selector / path");
                    text_field(ui, &mut f.selector, ".menu-today (optional)");
                    ui.end_row();
                    ui.label("Refresh");
                    ui.horizontal(|ui| {
                        ui.add(crate::holo_ui::Number::new(&mut f.interval_minutes).range(0..=1440).suffix(" min"));
                        if button(ui, "Remove", Tone::Danger).clicked() {
                            remove = Some(i);
                        }
                    });
                    ui.end_row();
                });
            });
            ui.add_space(4.0);
        }
        if let Some(i) = remove {
            d.feeds.remove(i);
        }
        if crate::holo_ui::key(ui, "+ Add web feed").clicked() {
            d.feeds.push(WebFeed { interval_minutes: 30, ..Default::default() });
        }
        d.feeds.retain(|f| !(f.name.trim().is_empty() && f.url.trim().is_empty() && f.selector.trim().is_empty() && f.interval_minutes == 0));
        self.draft = Some(d);
    }

    fn study_tab(&mut self, ui: &mut Ui, p: &Palette, pim: &Pim) {
        let Some(mut d) = self.draft.take() else { return };
        ui.label(RichText::new("Background change watchers").font(bold(12.0)).color(p.text));
        muted(ui, "JAI checks the service on a timer, lights its node in the core and tells you the moment a class is cancelled or something new is posted.");
        let running: HashMap<String, u32> = pim.watchers().list().into_iter().collect();
        let mut targets: Vec<(String, String)> = vec![(WEBUNTIS.into(), "WebUntis (cancellations, substitutions)".into()), (LMS.into(), "itslearning (new posts)".into())];
        targets.extend(d.feeds.iter().filter(|f| !f.name.trim().is_empty()).map(|f| (format!("feed:{}", f.name), format!("Web feed: {}", f.name))));
        egui::Grid::new("hub_watchers").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
            for (key, label) in &targets {
                let active = running.get(key).copied();
                ui.label(label);
                let default = active.unwrap_or(if key == WEBUNTIS { 15 } else if key == LMS { 10 } else { 30 });
                let minutes = self.watch_minutes.entry(key.clone()).or_insert(default);
                ui.add(crate::holo_ui::Number::new(minutes).range(2..=1440).suffix(" min"));
                let service = key.strip_prefix("feed:").unwrap_or(key).to_string();
                let result = if active.is_some() {
                    ui.horizontal(|ui| {
                        let stop = button(ui, "Stop", Tone::Danger).clicked();
                        let update = crate::holo_ui::key(ui, "Update").clicked();
                        (stop, update)
                    })
                    .inner
                } else {
                    (false, button(ui, "Watch", Tone::On).clicked())
                };
                let args = match result {
                    (true, _) => Some(json!({ "service_name": service, "interval_minutes": 0 })),
                    (false, true) => Some(json!({ "service_name": service, "interval_minutes": *minutes })),
                    _ => None,
                };
                if let Some(args) = args {
                    self.message = Some(match crate::change_notifier::tool(pim, &args) {
                        Ok(text) => (text, false),
                        Err(e) => (e, true),
                    });
                    // The watcher list lives in the settings too (saving the draft keeps it).
                    d.watchers = pim.config().read(|s| s.watchers.clone());
                }
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Study folder").font(bold(12.0)).color(p.text));
        egui::Grid::new("hub_study").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
            ui.label("School folder");
            text_field(ui, &mut d.study.school_dir, &crate::study_assistant::school_dir(pim).display().to_string());
            ui.end_row();
            ui.label("Downloads");
            crate::holo_ui::toggle(ui, &mut d.study.sort_downloads, "Also sort study files from Downloads (last 14 days)");
            ui.end_row();
        });
        muted(ui, "“Sync my study materials” sorts files into School/<Subject>/ and adds exams and homework to the calendar; “summarise my new physics worksheet” reads the newest one.");
        self.draft = Some(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_ui_verifier::{self as visual, Stage};

    /// JAI's Discord account and Away Mode in the hub, drawn offscreen at its width and at a
    /// narrow one: nothing cut off, nothing drawn over anything else, every control there.
    #[test]
    fn the_discord_account_and_away_mode_settings_are_shown_whole() {
        let _look = crate::holo_theme::look_for_pictures();
        let p = Palette::new(&crate::interface::InterfaceSettings::default());
        for width in [540.0, 400.0] {
            let (mut bot, mut away, mut token) = (DiscordBotSettings::default(), AwaySettings::default(), String::new());
            let results = Mutex::new(HashMap::new());
            lock(&results).insert("discord_bot", (true, "Send the bot this code as a DM: 123456".to_string()));
            let mut stage = Stage::new(egui::vec2(width + 80.0, 980.0));
            let frame = stage.frames(4, &mut |ctx| {
                egui::Window::new("hub").title_bar(false).fixed_pos(egui::pos2(20.0, 20.0)).fixed_size(egui::vec2(width, 920.0)).show(ctx, |ui| {
                    account_and_away(ui, &p, &mut bot, &mut away, &mut token, false, &results);
                });
            });
            stage.raster(&frame).save(&format!("hub_discord_account_{width}"));
            assert_eq!(visual::clipped_text(&frame), Vec::<String>::new(), "at {width} px");
            assert_eq!(visual::text_over_text(&frame), Vec::<String>::new(), "at {width} px");
            for shown in ["JAI's Discord account", "Bot token", "Your Discord user id", "Pair my account", "Starts with JAI", "123456", "Presence Intent", "Away Mode", "locked for", "a drive is plugged in"] {
                assert!(!frame.find_text(shown).is_empty(), "{shown} is not in the hub at {width} px");
            }
        }
    }
}
