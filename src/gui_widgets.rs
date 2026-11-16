//! Native rendering of JAI's dynamic widgets (the HUD's widgets panel and his extra windows)
//! and the window spawner: every `NativeWindowSpec` is a ROSE block inside the HUD
//! (`gui::hud_block_window`) until it is detached ("in a new window", a monitor, `[ DETACH ]`);
//! then it becomes a real OS window through an egui deferred viewport. Child module of `gui`, so
//! it shares the HUD's palette and building blocks.

use super::{bold, button, chip, meter, muted, paint_background, row, stroke, ts, Palette, Tone, TEXT_SCALE};
use crate::app::Jai;
use crate::hud::HudCommand;
use crate::widgets::{self, BlockKind, DynamicWidgetSpec, NativeWindowSpec, TimerMode, WidgetKind};
use eframe::egui::{
    self, pos2, text::LayoutJob, vec2, Align, Color32, FontId, Frame, IconData, Layout, Margin, RichText, Rounding, Sense, Shape,
    TextFormat, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId,
};
use std::f32::consts::PI;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::runtime::Handle;

/// The window icon, decoded once (windows are re-registered every HUD frame).
fn icon() -> Option<Arc<IconData>> {
    static ICON: OnceLock<Option<Arc<IconData>>> = OnceLock::new();
    ICON.get_or_init(|| super::window_icon().map(Arc::new)).clone()
}

/// Lets widgets send commands to JAI from any window.
#[derive(Clone)]
pub(super) struct WidgetHost {
    pub app: Jai,
    pub rt: Handle,
}

impl WidgetHost {
    pub fn command(&self, cmd: HudCommand) {
        let app = self.app.clone();
        self.rt.spawn(async move {
            app.handle_command(cmd).await;
        });
    }
}

fn accent_of(spec: &DynamicWidgetSpec, p: &Palette) -> Color32 {
    spec.accent
        .as_deref()
        .and_then(crate::interface::parse_hex)
        .map(|[r, g, b]| Color32::from_rgb(r, g, b))
        .unwrap_or(p.accent)
}

// ------------------------------------------------------------------------ the panel ----

/// One widget with its title bar (pop-out and remove buttons).
pub(super) fn widget_card(ui: &mut Ui, p: &Palette, spec: &DynamicWidgetSpec, host: &WidgetHost, in_window: bool) {
    let accent = accent_of(spec, p);
    Frame::none()
        .fill(p.well)
        .stroke(stroke(1.0, accent.gamma_multiply(0.35)))
        .rounding(Rounding::ZERO)
        .inner_margin(Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(&spec.title).font(bold(12.5)).color(accent)).truncate());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("X").on_hover_text("Remove this widget").clicked() {
                        host.command(HudCommand::WidgetRemove { id: spec.id.clone() });
                    }
                    if !in_window && ui.small_button("POP").on_hover_text("Open in its own window").clicked() {
                        host.command(HudCommand::WidgetPopOut { id: spec.id.clone() });
                    }
                    muted(ui, p, short_type(&spec.kind));
                });
            });
            ui.add_space(2.0);
            widget_body(ui, p, spec, host, accent);
        });
    ui.add_space(6.0);
}

/// A widget inside a block that is its frame and carries its title: its content alone, with
/// its two keys above it on the right. (In the block of the board's left column the card's own
/// rounded frame and second title stood inside the block's: a frame in a frame.)
pub(super) fn widget_bare(ui: &mut Ui, p: &Palette, spec: &DynamicWidgetSpec, host: &WidgetHost) {
    let accent = accent_of(spec, p);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
    let labels = [("[ REMOVE ]", false, "Remove this widget"), ("[ POP OUT ]", false, "Open it in its own window")];
    match crate::hud_style::keys(ui, ui.id().with(("widget_keys", &spec.id)), row.right(), row.center().y, &labels, 0.85).0 {
        Some(0) => host.command(HudCommand::WidgetRemove { id: spec.id.clone() }),
        Some(1) => host.command(HudCommand::WidgetPopOut { id: spec.id.clone() }),
        _ => {}
    }
    ui.add_space(4.0);
    widget_body(ui, p, spec, host, accent);
}

/// The kind as a block's tag.
pub(super) fn short_type_upper(kind: &WidgetKind) -> &'static str {
    match short_type(kind) {
        "gauge" => "GAUGE",
        "progress" => "PROGRESS",
        "chart" => "CHART",
        "sparkline" => "SPARKLINE",
        "timer" => "TIMER",
        "stopwatch" => "STOPWATCH",
        "list" => "LIST",
        "table" => "TABLE",
        "note" => "NOTE",
        "checklist" => "CHECKLIST",
        "actions" => "ACTIONS",
        _ => "LIVE",
    }
}

fn short_type(kind: &WidgetKind) -> &'static str {
    match kind {
        WidgetKind::MetricGauge { .. } => "gauge",
        WidgetKind::ProgressBar { .. } => "progress",
        WidgetKind::LiveLineChart { .. } => "chart",
        WidgetKind::Sparkline { .. } => "sparkline",
        WidgetKind::CountdownOrStopwatch { mode: TimerMode::Countdown, .. } => "timer",
        WidgetKind::CountdownOrStopwatch { .. } => "stopwatch",
        WidgetKind::KeyValueList { .. } => "list",
        WidgetKind::DataTable { .. } => "table",
        WidgetKind::MarkdownNoteCard { .. } => "note",
        WidgetKind::Checklist { .. } => "checklist",
        WidgetKind::QuickActionButtons { .. } => "actions",
        WidgetKind::LiveCommandOutput { .. } => "live output",
    }
}

fn fmt_value(v: f64) -> String {
    if v.abs() >= 100.0 || v.fract().abs() < 0.05 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

fn widget_body(ui: &mut Ui, p: &Palette, spec: &DynamicWidgetSpec, host: &WidgetHost, accent: Color32) {
    let id = spec.id.clone();
    match &spec.kind {
        WidgetKind::MetricGauge { value, min, max, unit, warn_at, source } => {
            // A live source this machine has no sensor for (CPU temperature without a driver
            // exposing it) says so instead of showing a fake zero.
            if let Some(s) = source.as_deref().filter(|s| widgets::telemetry_value(&host.app.telemetry(), s).is_none()) {
                ui.vertical_centered(|ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new("—").font(bold(26.0)).color(p.muted));
                    muted(ui, p, format!("no {} reading on this PC", s.replace('_', " ")));
                });
                return;
            }
            let hot = warn_at.is_some_and(|w| *value >= w);
            gauge(ui, p, *value, *min, *max, unit, if hot { p.amber } else { accent });
            if let Some(s) = source {
                ui.vertical_centered(|ui| muted(ui, p, format!("live: {s}")));
            }
        }
        WidgetKind::ProgressBar { value, max, unit, .. } => {
            let f = (*value / max.max(f64::EPSILON)) as f32;
            row(ui, p, &format!("{:.0} %", f * 100.0), format!("{} / {} {unit}", fmt_value(*value), fmt_value(*max)));
            meter(ui, p, f, 10.0, if f >= 0.9 { p.amber } else { accent }, &[]);
        }
        WidgetKind::LiveLineChart { series, unit, min, max, .. } => line_chart(ui, p, series, unit, *min, *max, 96.0, true, accent),
        WidgetKind::Sparkline { series, unit, .. } => line_chart(ui, p, series, unit, None, None, 30.0, false, accent),
        WidgetKind::CountdownOrStopwatch { mode, duration_secs, running, alerted, .. } => {
            let (elapsed, remaining) = widgets::timer_state(&spec.kind, widgets::now_ms()).unwrap_or((0, None));
            let shown = remaining.unwrap_or(elapsed);
            let colour = if *alerted { p.green } else if *running { accent } else { p.muted };
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(clock(shown)).font(FontId::monospace(ts(30.0))).color(colour));
            });
            if *mode == TimerMode::Countdown && *duration_secs > 0.0 {
                meter(ui, p, 1.0 - shown as f32 / (*duration_secs as f32 * 1000.0), 5.0, colour, &[]);
            }
            ui.horizontal(|ui| {
                let (label, op) = if *running { ("Pause", "pause") } else { ("Start", "start") };
                if ui.small_button(label).clicked() {
                    host.command(HudCommand::WidgetTimer { id: id.clone(), op: op.into() });
                }
                if ui.small_button("Reset").clicked() {
                    host.command(HudCommand::WidgetTimer { id: id.clone(), op: "reset".into() });
                }
                if *alerted {
                    muted(ui, p, "finished");
                }
            });
        }
        WidgetKind::KeyValueList { items } => {
            if items.is_empty() {
                muted(ui, p, "(empty)");
            }
            for kv in items {
                row(ui, p, &kv.key, &kv.value).on_hover_text(&kv.value);
            }
        }
        WidgetKind::DataTable { columns, rows } => {
            egui::ScrollArea::horizontal().id_salt(("widget_table", &spec.id)).show(ui, |ui| {
                egui::Grid::new(("widget_grid", &spec.id)).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                    for c in columns {
                        ui.label(RichText::new(c).font(bold(11.5)).color(accent));
                    }
                    if !columns.is_empty() {
                        ui.end_row();
                    }
                    for r in rows {
                        for cell in r {
                            ui.label(RichText::new(cell).size(ts(11.5)));
                        }
                        ui.end_row();
                    }
                });
            });
        }
        WidgetKind::MarkdownNoteCard { markdown } => markdown_view(ui, p, markdown, accent),
        WidgetKind::Checklist { items } => {
            if items.is_empty() {
                muted(ui, p, "Nothing on the list.");
            }
            let done = items.iter().filter(|i| i.done).count();
            for (index, item) in items.iter().enumerate() {
                let mut checked = item.done;
                let text = if item.done { RichText::new(&item.text).strikethrough().color(p.muted) } else { RichText::new(&item.text) };
                if ui.checkbox(&mut checked, text).changed() {
                    host.command(HudCommand::WidgetToggleItem { id: id.clone(), index });
                }
            }
            if !items.is_empty() {
                muted(ui, p, format!("{done} of {} done", items.len()));
            }
        }
        WidgetKind::QuickActionButtons { buttons } => {
            ui.horizontal_wrapped(|ui| {
                for (index, b) in buttons.iter().enumerate() {
                    if button(ui, p, &b.label, Tone::Normal).on_hover_text(&b.action).clicked() {
                        host.command(HudCommand::WidgetAction { id: id.clone(), index });
                    }
                }
            });
        }
        WidgetKind::LiveCommandOutput { command, interval_secs, output, last_run_ms, ok } => {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(format!("$ {command}")).monospace().size(ts(10.5)).color(p.muted)).truncate());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("RELOAD").on_hover_text("Run now").clicked() {
                        host.command(HudCommand::WidgetRefresh { id: id.clone() });
                    }
                    let age = if *last_run_ms > 0 { format!("{} s ago", (widgets::now_ms() - last_run_ms).max(0) / 1000) } else { "pending".into() };
                    muted(ui, p, format!("every {interval_secs} s · {age}"));
                });
            });
            Frame::none().fill(p.well_solid).rounding(Rounding::ZERO).inner_margin(Margin::same(6.0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                crate::hud_style::faded(ui, egui::ScrollArea::vertical().id_salt(("widget_output", &spec.id)).max_height(170.0), |ui| {
                    let colour = if *ok { p.text } else { p.red };
                    ui.add(egui::Label::new(RichText::new(if output.is_empty() { "…" } else { output }).monospace().size(ts(10.5)).color(colour)).wrap());
                });
            });
        }
    }
}

fn clock(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// A 270° arc gauge with the value in the middle.
fn gauge(ui: &mut Ui, p: &Palette, value: f64, min: f64, max: f64, unit: &str, colour: Color32) {
    let size = ui.available_width().min(170.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size * 0.78), Sense::hover());
    let painter = ui.painter();
    let c = pos2(rect.center().x, rect.top() + size * 0.46);
    let radius = size * 0.38;
    let (start, sweep) = (0.75 * PI, 1.5 * PI);
    let f = ((value - min) / (max - min).max(f64::EPSILON)).clamp(0.0, 1.0) as f32;
    let arc = |from: f32, to: f32| super::arc_points(c, radius, from, to);
    painter.add(Shape::line(arc(start, start + sweep), stroke(9.0, p.track)));
    if f > 0.0 {
        painter.add(Shape::line(arc(start, start + sweep * f), stroke(9.0, colour)));
    }
    painter.text(c, egui::Align2::CENTER_CENTER, fmt_value(value), FontId::new(size * 0.17, super::bold_family()), p.text);
    painter.text(c + vec2(0.0, size * 0.15), egui::Align2::CENTER_CENTER, unit, FontId::proportional(ts(11.0)), p.muted);
    let label_y = c.y + radius * 0.85;
    painter.text(pos2(c.x - radius * 0.75, label_y), egui::Align2::CENTER_CENTER, fmt_value(min), FontId::proportional(ts(10.0)), p.muted);
    painter.text(pos2(c.x + radius * 0.75, label_y), egui::Align2::CENTER_CENTER, fmt_value(max), FontId::proportional(ts(10.0)), p.muted);
}

#[allow(clippy::too_many_arguments)]
fn line_chart(ui: &mut Ui, p: &Palette, series: &[f64], unit: &str, min: Option<f64>, max: Option<f64>, height: f32, grid: bool, colour: Color32) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    if grid {
        painter.rect_filled(rect, Rounding::ZERO, p.well_solid);
        for i in 1..4 {
            let y = rect.top() + rect.height() * i as f32 / 4.0;
            painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke(1.0, p.track));
        }
    }
    if series.len() < 2 {
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, "collecting data…", FontId::proportional(ts(10.5)), p.muted);
        return;
    }
    let lo = min.unwrap_or_else(|| series.iter().copied().fold(f64::INFINITY, f64::min));
    let hi = max.unwrap_or_else(|| series.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    let (lo, hi) = if (hi - lo).abs() < 1e-9 { (lo - 1.0, hi + 1.0) } else { (lo, hi) };
    let pad = if min.is_none() && max.is_none() { (hi - lo) * 0.1 } else { 0.0 };
    let (lo, hi) = (lo - pad, hi + pad);
    let inner = rect.shrink2(vec2(2.0, 3.0));
    let n = series.len();
    let points: Vec<egui::Pos2> = series
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = inner.left() + inner.width() * i as f32 / (n - 1) as f32;
            let y = inner.bottom() - inner.height() * ((v - lo) / (hi - lo)) as f32;
            pos2(x, y.clamp(inner.top(), inner.bottom()))
        })
        .collect();
    painter.add(Shape::line(points.clone(), stroke(if grid { 2.0 } else { 1.6 }, colour)));
    if let Some(last) = points.last() {
        painter.circle_filled(*last, 2.5, colour);
    }
    let latest = series[n - 1];
    if grid {
        painter.text(rect.right_top() + vec2(-6.0, 4.0), egui::Align2::RIGHT_TOP, format!("{} {unit}", fmt_value(latest)), FontId::proportional(ts(12.0)), p.text);
        painter.text(rect.left_top() + vec2(6.0, 4.0), egui::Align2::LEFT_TOP, fmt_value(hi), FontId::proportional(ts(9.5)), p.muted);
        painter.text(rect.left_bottom() + vec2(6.0, -4.0), egui::Align2::LEFT_BOTTOM, fmt_value(lo), FontId::proportional(ts(9.5)), p.muted);
    } else {
        response.on_hover_text(format!("{} {unit} (min {}, max {})", fmt_value(latest), fmt_value(lo + pad), fmt_value(hi - pad)));
        ui.horizontal(|ui| muted(ui, p, format!("now {} {unit}", fmt_value(latest))));
    }
}

// ------------------------------------------------------------------------- markdown ----

/// `**bold**` and `` `code` `` inside a line.
fn inline(job: &mut LayoutJob, text: &str, size: f32, base: Color32, accent: Color32, code_bg: Color32, heading: bool) {
    let family = if heading { super::bold_family() } else { egui::FontFamily::Proportional };
    let mut bold_on = heading;
    let mut code_on = false;
    let mut buf = String::new();
    let flush = |job: &mut LayoutJob, buf: &mut String, bold_on: bool, code_on: bool| {
        if buf.is_empty() {
            return;
        }
        let format = if code_on {
            TextFormat { font_id: FontId::monospace(size - 1.0), color: accent, background: code_bg, ..Default::default() }
        } else {
            TextFormat {
                font_id: FontId::new(size, if bold_on { super::bold_family() } else { family.clone() }),
                color: if bold_on && !heading { accent } else { base },
                ..Default::default()
            }
        };
        job.append(&std::mem::take(buf), 0.0, format);
    };
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '`' {
            flush(job, &mut buf, bold_on, code_on);
            code_on = !code_on;
        } else if ch == '*' && chars.peek() == Some(&'*') && !code_on {
            chars.next();
            flush(job, &mut buf, bold_on, code_on);
            bold_on = !bold_on || heading;
        } else {
            buf.push(ch);
        }
    }
    flush(job, &mut buf, bold_on, code_on);
}

/// Headings, bullet and numbered lists, quotes, code blocks and inline emphasis.
pub(super) fn markdown_view(ui: &mut Ui, p: &Palette, markdown: &str, accent: Color32) {
    let mut in_code = false;
    let mut code = String::new();
    for raw in markdown.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            if in_code {
                code_block(ui, p, &code);
                code.clear();
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            ui.add_space(4.0);
            continue;
        }
        let (text, size, heading, prefix, colour) = if let Some(t) = trimmed.strip_prefix("### ") {
            (t, 13.0, true, "", p.text)
        } else if let Some(t) = trimmed.strip_prefix("## ") {
            (t, 14.5, true, "", accent)
        } else if let Some(t) = trimmed.strip_prefix("# ") {
            (t, 16.5, true, "", accent)
        } else if let Some(t) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).or_else(|| trimmed.strip_prefix("• ")) {
            (t, 12.5, false, "•  ", p.text)
        } else if let Some(t) = trimmed.strip_prefix("> ") {
            (t, 12.5, false, "▏ ", p.muted)
        } else {
            (trimmed, 12.5, false, "", p.text)
        };
        let size = ts(size);
        let mut job = LayoutJob::default();
        job.wrap.max_width = ui.available_width();
        // Numbered items keep their number.
        if !prefix.is_empty() {
            job.append(prefix, if prefix.starts_with('•') { (line.len() - trimmed.len()) as f32 * 4.0 } else { 0.0 }, TextFormat { font_id: FontId::proportional(size), color: accent, ..Default::default() });
        }
        inline(&mut job, text, size, colour, accent, p.well_solid, heading);
        ui.label(job);
        if heading {
            ui.add_space(2.0);
        }
    }
    if in_code && !code.is_empty() {
        code_block(ui, p, &code);
    }
}

fn code_block(ui: &mut Ui, p: &Palette, code: &str) {
    Frame::none().fill(p.well_solid).stroke(stroke(1.0, p.line)).rounding(Rounding::ZERO).inner_margin(Margin::same(8.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if ui.small_button("Copy").clicked() {
                    let text = code.trim_end().to_string();
                    ui.output_mut(|o| o.copied_text = text);
                }
            });
        });
        egui::ScrollArea::horizontal().id_salt(code.len()).show(ui, |ui| {
            if looks_like_diff(code) {
                ui.add(egui::Label::new(diff_job(p, code)).extend());
            } else {
                ui.add(egui::Label::new(RichText::new(code.trim_end()).monospace().size(ts(11.5))).extend());
            }
        });
    });
}

/// A unified diff (JAI's file edits): an `@@` hunk header and only ` `/`+`/`-` lines.
fn looks_like_diff(code: &str) -> bool {
    let mut lines = code.lines().filter(|l| !l.is_empty());
    lines.clone().any(|l| l.starts_with("@@")) && lines.all(|l| l.starts_with(['@', '+', '-', ' ', '…']))
}

/// Removed lines red, added lines green, hunk headers in the accent colour.
fn diff_job(p: &Palette, code: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    for line in code.trim_end().lines() {
        let (colour, background) = match line.chars().next() {
            Some('+') => (p.green, p.green.gamma_multiply(0.12)),
            Some('-') => (p.red, p.red.gamma_multiply(0.12)),
            Some('@') => (p.accent, Color32::TRANSPARENT),
            _ => (p.muted, Color32::TRANSPARENT),
        };
        job.append(&format!("{line}\n"), 0.0, TextFormat { font_id: FontId::monospace(ts(11.5)), color: colour, background, ..Default::default() });
    }
    job
}

// ------------------------------------------------------------------------ telemetry ----

fn telemetry_view(ui: &mut Ui, p: &Palette, app: &Jai) {
    let t = app.telemetry();
    let bar = |ui: &mut Ui, pct: f32, hot: bool| meter(ui, p, pct / 100.0, 6.0, if hot { p.amber } else { p.accent }, &[]);
    row(ui, p, "CPU", format!("{:.1}% · {} threads", t.cpu_usage_pct, t.cpu_cores));
    bar(ui, t.cpu_usage_pct, t.cpu_usage_pct > 85.0);
    row(ui, p, "RAM", format!("{:.1} / {:.1} GB", t.ram_used_gb, t.ram_total_gb));
    bar(ui, t.ram_usage_pct, t.ram_usage_pct > 90.0);
    if let Some(g) = &t.gpu {
        let temp = g.temperature_c.map(|c| format!("{c:.0}°C")).unwrap_or_else(|| "—".into());
        row(ui, p, &g.name.replace("NVIDIA GeForce ", ""), format!("{temp} · {:.0}%", g.utilization_pct.unwrap_or(0.0)));
        bar(ui, g.utilization_pct.unwrap_or(0.0), g.temperature_c.unwrap_or(0.0) >= 80.0);
        if let (Some(used), Some(total)) = (g.vram_used_mb, g.vram_total_mb.filter(|t| *t > 0)) {
            row(ui, p, "VRAM", format!("{:.1} / {:.1} GB", used as f32 / 1024.0, total as f32 / 1024.0));
            bar(ui, used as f32 / total as f32 * 100.0, used * 100 / total > 92);
        }
    }
    for d in &t.disks {
        row(ui, p, &format!("Drive {}", d.mount), format!("{:.0} GB free / {:.0} GB", d.free_gb, d.total_gb));
        bar(ui, d.used_pct, d.used_pct > 92.0);
    }
}

// ------------------------------------------------------------------ live data views ----
// Every view sizes itself from the space it gets, so a window reflows when Sir drags its edge
// or JAI resizes it ("make the drive space window smaller").

/// The theme's accent below 75 %, amber to 90 %, red above.
fn capacity_colour(p: &Palette, pct: f32) -> Color32 {
    if pct >= 90.0 {
        p.red
    } else if pct >= 75.0 {
        p.amber
    } else {
        p.accent
    }
}

/// A labelled bar: title left, value right, the bar below.
fn bar_row(ui: &mut Ui, p: &Palette, label: &str, value: String, fraction: f32, colour: Color32, height: f32) {
    row(ui, p, label, value);
    meter(ui, p, fraction.clamp(0.0, 1.0), height, colour, &[]);
}

/// History of a live value per window (kept in egui's memory, two minutes at one sample/s).
fn history(ui: &Ui, key: (&str, &str), value: f32) -> Vec<f64> {
    let id = egui::Id::new(("jai_history", key.0, key.1));
    let now = ui.input(|i| i.time);
    ui.ctx().data_mut(|d| {
        let (last, series) = d.get_temp_mut_or_default::<(f64, Vec<f64>)>(id);
        if now - *last >= 0.95 || series.is_empty() {
            *last = now;
            series.push(value as f64);
            if series.len() > 120 {
                series.drain(..series.len() - 120);
            }
        }
        series.clone()
    })
}

/// A ring gauge sized to `size`, value in the middle.
fn ring(ui: &mut Ui, p: &Palette, size: f32, fraction: f32, colour: Color32, big: &str, small: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter();
    let c = rect.center();
    let radius = size * 0.42;
    let (start, sweep) = (0.75 * PI, 1.5 * PI);
    painter.add(Shape::line(super::arc_points(c, radius, start, start + sweep), stroke(size * 0.07, p.track)));
    if fraction > 0.0 {
        painter.add(Shape::line(super::arc_points(c, radius, start, start + sweep * fraction.clamp(0.0, 1.0)), stroke(size * 0.07, colour)));
    }
    painter.text(c - vec2(0.0, size * 0.04), egui::Align2::CENTER_CENTER, big, FontId::new(size * 0.23, egui::FontFamily::Name("display".into())), p.text);
    painter.text(c + vec2(0.0, size * 0.16), egui::Align2::CENTER_CENTER, small, FontId::proportional((size * 0.085).max(ts(9.5))), p.muted);
}

/// A titled section frame inside a live window.
fn section(ui: &mut Ui, p: &Palette, title: &str, body: impl FnOnce(&mut Ui)) {
    // (Sharp like the block it stands in: a hairline box, its title lettered as the block's is.)
    Frame::none().fill(Color32::from_rgba_unmultiplied(9, 15, 23, 150)).stroke(stroke(1.0, p.line.gamma_multiply(0.7))).rounding(Rounding::ZERO).inner_margin(Margin::same(10.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title.to_uppercase()).font(FontId::new(ts(12.5), egui::FontFamily::Name("display".into()))).color(p.accent).extra_letter_spacing(1.3));
        ui.add_space(4.0);
        body(ui);
    });
    ui.add_space(8.0);
}

/// CPU (overall gauge + every core), RAM + swap, GPU load / temperature / VRAM, with history.
fn pc_stats_view(ui: &mut Ui, p: &Palette, app: &Jai, window: &str) {
    let t = app.telemetry();
    let width = ui.available_width();
    let wide = width >= 520.0;
    let gauge = (width / if wide { 4.4 } else { 2.6 }).clamp(90.0, 190.0);
    let cpu_hist = history(ui, (window, "cpu"), t.cpu_usage_pct);
    let gpu = t.gpu.clone();
    let gpu_hist = gpu.as_ref().and_then(|g| g.utilization_pct).map(|u| history(ui, (window, "gpu"), u));
    // Headline gauges, as many per row as fit.
    ui.horizontal_wrapped(|ui| {
        ring(ui, p, gauge, t.cpu_usage_pct / 100.0, capacity_colour(p, t.cpu_usage_pct), &format!("{:.0}%", t.cpu_usage_pct), "CPU");
        ring(ui, p, gauge, t.ram_usage_pct / 100.0, capacity_colour(p, t.ram_usage_pct), &format!("{:.0}%", t.ram_usage_pct), "RAM");
        if let Some(g) = &gpu {
            let load = g.utilization_pct.unwrap_or(0.0);
            ring(ui, p, gauge, load / 100.0, capacity_colour(p, load), &format!("{load:.0}%"), "GPU");
            if let Some(temp) = g.temperature_c {
                let hot = if temp >= 83.0 { p.red } else if temp >= 75.0 { p.amber } else { p.accent };
                ring(ui, p, gauge, (temp / 100.0).clamp(0.0, 1.0), hot, &format!("{temp:.0}°"), "GPU °C");
            }
        }
        if let Some(c) = t.cpu_temp_c {
            let hot = if c >= 90.0 { p.red } else if c >= 80.0 { p.amber } else { p.accent };
            ring(ui, p, gauge, (c / 105.0).clamp(0.0, 1.0), hot, &format!("{c:.0}°"), "CPU °C");
        }
    });
    ui.add_space(6.0);
    section(ui, p, &format!("CPU · {} · {} THREADS{}", t.cpu_name.trim().to_uppercase(), t.cpu_cores, if t.cpu_freq_mhz > 0 { format!(" · {:.2} GHz", t.cpu_freq_mhz as f32 / 1000.0) } else { String::new() }), |ui| {
        line_chart(ui, p, &cpu_hist, "%", Some(0.0), Some(100.0), (ui.available_width() * 0.16).clamp(48.0, 110.0), true, p.accent);
        ui.add_space(6.0);
        // Per-core bars: as many columns as the width allows.
        let cores = &t.cpu_per_core;
        if !cores.is_empty() {
            // Cells share the full width: 4 columns in a 600 px window, 8 when maximised.
            let width = ui.available_width();
            let columns = ((width / 120.0).floor() as usize).clamp(2, 8);
            let gap = 10.0;
            let cell = ((width - gap * (columns as f32 - 1.0)) / columns as f32 - 1.0).max(50.0);
            egui::Grid::new(("cores", window)).num_columns(columns).spacing([gap, 3.0]).min_col_width(cell).max_col_width(cell).show(ui, |ui| {
                for (i, load) in cores.iter().enumerate() {
                    ui.vertical(|ui| {
                        ui.set_width(cell);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("#{i:02}")).monospace().size(ts(9.5)).color(p.muted));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(format!("{load:.0}%")).size(ts(9.5)).color(p.text));
                            });
                        });
                        meter(ui, p, load / 100.0, 5.0, capacity_colour(p, *load), &[]);
                    });
                    if (i + 1) % columns == 0 {
                        ui.end_row();
                    }
                }
            });
        }
    });
    section(ui, p, "MEMORY", |ui| {
        bar_row(ui, p, "RAM", format!("{:.1} / {:.1} GB ({:.0}%)", t.ram_used_gb, t.ram_total_gb, t.ram_usage_pct), t.ram_usage_pct / 100.0, capacity_colour(p, t.ram_usage_pct), 9.0);
        if t.swap_total_gb > 0.05 {
            let pct = t.swap_used_gb / t.swap_total_gb * 100.0;
            bar_row(ui, p, "Swap / page file", format!("{:.1} / {:.1} GB", t.swap_used_gb, t.swap_total_gb), pct / 100.0, capacity_colour(p, pct), 7.0);
        }
    });
    match &gpu {
        Some(g) => section(ui, p, &format!("GPU · {}", g.name.to_uppercase()), |ui| {
            if let Some(hist) = &gpu_hist {
                line_chart(ui, p, hist, "%", Some(0.0), Some(100.0), (ui.available_width() * 0.12).clamp(40.0, 90.0), true, p.violet);
                ui.add_space(4.0);
            }
            let load = g.utilization_pct.unwrap_or(0.0);
            bar_row(ui, p, "Load", format!("{load:.0}%"), load / 100.0, capacity_colour(p, load), 8.0);
            if let Some(temp) = g.temperature_c {
                bar_row(ui, p, "Temperature", format!("{temp:.0} °C"), temp / 100.0, if temp >= 83.0 { p.red } else if temp >= 75.0 { p.amber } else { p.accent }, 8.0);
            }
            if let (Some(used), Some(total)) = (g.vram_used_mb, g.vram_total_mb.filter(|t| *t > 0)) {
                let pct = used as f32 / total as f32 * 100.0;
                bar_row(ui, p, "VRAM", format!("{:.2} / {:.1} GB ({pct:.0}%)", used as f32 / 1024.0, total as f32 / 1024.0), pct / 100.0, capacity_colour(p, pct), 8.0);
            }
            if let Some(w) = g.power_w {
                row(ui, p, "Power draw", format!("{w:.0} W"));
            }
        }),
        None => section(ui, p, "GPU", |ui| muted(ui, p, "No GPU telemetry (NVML / nvidia-smi not available).")),
    }
}

/// Every drive: letter and label, file system, total / used / free (GB and %), capacity bar.
fn drive_space_view(ui: &mut Ui, p: &Palette, app: &Jai) {
    let t = app.telemetry();
    if t.disks.is_empty() {
        muted(ui, p, "Reading the drives…");
        return;
    }
    let total: f32 = t.disks.iter().map(|d| d.total_gb).sum();
    let free: f32 = t.disks.iter().map(|d| d.free_gb).sum();
    ui.horizontal_wrapped(|ui| {
        chip(ui, &format!("{} drives", t.disks.len()), p.accent);
        chip(ui, &format!("{:.0} GB free of {:.0} GB", free, total), p.accent);
        if let Some(low) = t.disks.iter().filter(|d| d.free_gb < 5.0).map(|d| d.letter()).next() {
            chip(ui, &format!("{low} below 5 GB"), p.red);
        }
    });
    ui.add_space(6.0);
    // Cards side by side when the window is wide, stacked when narrow.
    let columns = ((ui.available_width() / 300.0).floor() as usize).clamp(1, 3);
    ui.columns(columns, |cols| {
        for (i, d) in t.disks.iter().enumerate() {
            let ui = &mut cols[i % columns];
            let colour = capacity_colour(p, d.used_pct);
            Frame::none().fill(p.card).stroke(stroke(1.0, colour.gamma_multiply(0.45))).rounding(Rounding::ZERO).inner_margin(Margin::same(10.0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(d.letter()).font(bold(18.0)).color(colour));
                    let mut tags = Vec::new();
                    if !d.label.is_empty() {
                        tags.push(d.label.clone());
                    }
                    if !d.file_system.is_empty() {
                        tags.push(d.file_system.clone());
                    }
                    if !d.kind.is_empty() {
                        tags.push(d.kind.clone());
                    }
                    if d.removable {
                        tags.push("removable".into());
                    }
                    ui.add(egui::Label::new(RichText::new(tags.join(" · ")).size(ts(10.5)).color(p.muted)).truncate());
                });
                ui.add_space(2.0);
                ui.label(RichText::new(format!("{:.1} GB free", d.free_gb)).font(bold(14.0)).color(p.text));
                muted(ui, p, format!("{:.0}% free · {:.1} GB used of {:.1} GB", d.free_pct(), d.used_gb(), d.total_gb));
                meter(ui, p, d.used_pct / 100.0, 10.0, colour, &[(0.75, p.amber.gamma_multiply(0.6)), (0.9, p.red.gamma_multiply(0.6))]);
            });
            ui.add_space(8.0);
        }
    });
}

/// The programs using the most CPU and memory (grouped by name), refreshed every two seconds.
fn processes_view(ui: &mut Ui, p: &Palette, host: &WidgetHost) {
    let list = host.app.tools().widgets.processes();
    if list.is_empty() {
        // The first sample is on its way: the outline of the list.
        let room = ui.available_rect_before_wrap();
        crate::hud_style::outline(ui, room.shrink(4.0), crate::hud_style::Outline::List);
        return;
    }
    let rows = ((ui.available_height() / 2.0 / 22.0).floor() as usize).clamp(5, 15);
    let mut by_cpu = list.clone();
    by_cpu.sort_by(|a, b| b.cpu_pct.total_cmp(&a.cpu_pct));
    let mut by_mem = list;
    by_mem.sort_by(|a, b| b.memory_mb.total_cmp(&a.memory_mb));
    let top_mem = by_mem.first().map(|m| m.memory_mb).unwrap_or(1.0).max(1.0);
    let columns = if ui.available_width() >= 640.0 { 2 } else { 1 };
    ui.columns(columns, |cols| {
        let (left, right) = if columns == 2 { let (a, b) = cols.split_at_mut(1); (&mut a[0], Some(&mut b[0])) } else { (&mut cols[0], None) };
        section(left, p, "TOP CPU", |ui| {
            for proc_ in by_cpu.iter().take(rows) {
                bar_row(ui, p, &crate::apps::display_name(&proc_.name), format!("{:.1}%{}", proc_.cpu_pct, if proc_.count > 1 { format!(" · {}×", proc_.count) } else { String::new() }), proc_.cpu_pct / 100.0, capacity_colour(p, proc_.cpu_pct), 5.0);
            }
        });
        let target = match right {
            Some(r) => r,
            None => left,
        };
        section(target, p, "TOP MEMORY", |ui| {
            for proc_ in by_mem.iter().take(rows) {
                bar_row(ui, p, &crate::apps::display_name(&proc_.name), format!("{:.2} GB", proc_.memory_mb / 1024.0), (proc_.memory_mb / top_mem) as f32, p.violet, 5.0);
            }
        });
    });
}

/// Download / upload throughput with two minutes of history.
fn network_view(ui: &mut Ui, p: &Palette, app: &Jai, window: &str) {
    let t = app.telemetry();
    let down = history(ui, (window, "down"), t.net_down_kbps);
    let up = history(ui, (window, "up"), t.net_up_kbps);
    let h = ((ui.available_height() - 80.0) / 2.0).clamp(50.0, 180.0);
    // The first seconds have nothing to plot: the outline of the chart to come.
    if down.len() < 3 {
        let room = ui.available_rect_before_wrap();
        crate::hud_style::outline(ui, egui::Rect::from_min_size(room.min, vec2(room.width(), (2.0 * h + 60.0).min(room.height()))), crate::hud_style::Outline::Chart);
        return;
    }
    section(ui, p, &format!("DOWNLOAD · {}", super::rate(t.net_down_kbps)), |ui| line_chart(ui, p, &down, "KB/s", Some(0.0), None, h, true, p.accent));
    section(ui, p, &format!("UPLOAD · {}", super::rate(t.net_up_kbps)), |ui| line_chart(ui, p, &up, "KB/s", Some(0.0), None, h, true, p.accent));
}

// -------------------------------------------------------------------------- windows ----

fn viewport_id(id: &str) -> ViewportId {
    ViewportId::from_hash_of(("jai_window", id))
}

/// The JAI windows of their own shown in the last frame, as they were (spec and builder): one
/// that was just closed (by voice, by its close button) keeps its viewport while its exit plays,
/// drawn from here.
static SHOWN: std::sync::Mutex<Vec<(NativeWindowSpec, ViewportBuilder)>> = std::sync::Mutex::new(Vec::new());

/// Window `id` as it was while it plays its exit (closed, still on the screen).
fn leaving_spec(id: &str) -> Option<NativeWindowSpec> {
    SHOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(spec, _)| spec.id == id).map(|(spec, _)| spec.clone())
}

/// Logical position of a window: its own x/y, relative to the chosen monitor's work area.
fn position(spec: &NativeWindowSpec, native_ppp: f32) -> Option<egui::Pos2> {
    if spec.monitor.is_none() && spec.x.is_none() && spec.y.is_none() {
        return None;
    }
    #[cfg(windows)]
    let origin = spec
        .monitor
        .and_then(|m| crate::win32::monitor_work_areas().get(m.saturating_sub(1)).copied())
        .map(|[x, y, w, h]| (x as f32 / native_ppp, y as f32 / native_ppp, w as f32 / native_ppp, h as f32 / native_ppp));
    #[cfg(not(windows))]
    let origin: Option<(f32, f32, f32, f32)> = {
        let _ = native_ppp;
        None
    };
    let (ox, oy, ow, oh) = origin.unwrap_or((0.0, 0.0, 0.0, 0.0));
    // Without x/y on a chosen monitor: centred on it.
    let x = spec.x.unwrap_or(if ow > 0.0 { (ow - spec.width).max(0.0) / 2.0 } else { 80.0 });
    let y = spec.y.unwrap_or(if oh > 0.0 { (oh - spec.height).max(0.0) / 2.0 } else { 80.0 });
    Some(pos2(ox + x, oy + y))
}

/// Registers every JAI window with egui (each root frame); egui creates, patches and closes
/// the native windows to match.
pub(super) fn show_windows(ctx: &egui::Context, host: &WidgetHost) {
    static SEEN_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    /// Raise counters already applied: a window Sir asked for again comes to the front (a resize
    /// or a live update never steals the focus, e.g. from a game).
    static WINDOW_RAISES: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());
    let engine = &host.app.tools().widgets;
    // Widget data changed (a tool call, a live sample): redraw the windows right away.
    let revision = engine.revision();
    let changed = SEEN_REVISION.swap(revision, std::sync::atomic::Ordering::AcqRel) != revision;
    let native_ppp = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0);
    let windows = engine.windows();
    {
        let mut seen = WINDOW_RAISES.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|(id, _)| windows.iter().any(|w| w.id == *id));
        for spec in &windows {
            match seen.iter_mut().find(|(id, _)| *id == spec.id) {
                Some((_, raise)) if *raise != spec.raise => {
                    *raise = spec.raise;
                    // (Never in a test session: Sir may be playing.)
                    if !crate::platform::in_test_session() {
                        ctx.send_viewport_cmd_to(viewport_id(&spec.id), ViewportCommand::Focus);
                    }
                }
                Some(_) => {}
                None => seen.push((spec.id.clone(), spec.raise)),
            }
        }
    }
    let open_ids: Vec<String> = windows.iter().map(|w| w.id.clone()).collect();
    let mut shown_now: Vec<(NativeWindowSpec, ViewportBuilder)> = Vec::new();
    for spec in windows {
        if !crate::hud_dock::adopt(&spec.id) {
            // Inside the HUD: a ROSE block.
            let p = window_palette(host, &spec.style);
            let hud_scale = TEXT_SCALE.with(|s| s.get());
            TEXT_SCALE.with(|s| s.set(spec.style.font_scale));
            let mut fast = false;
            // What the block says, for `Copy Content`, the exports and the search across blocks
            // (stored again only when it changes).
            let text: Vec<&str> = spec.blocks.iter().flat_map(|b| [b.title.as_str(), b.body.as_str()]).filter(|s| !s.trim().is_empty()).collect();
            crate::hud_dock::describe_block(&spec.id, &text.join("\n"));
            // A live window's light: all is well, or (the system's figures) something runs hot.
            if !spec.live_kinds().is_empty() {
                let t = host.app.telemetry();
                let hot = spec.live_kinds().contains(&"pc_stats") && (t.cpu_usage_pct > 85.0 || t.ram_usage_pct > 90.0 || t.gpu.as_ref().and_then(|g| g.temperature_c).is_some_and(|c| c >= 80.0));
                crate::hud_style::set_light(&spec.id, if hot { crate::hud_style::Light::Warn } else { crate::hud_style::Light::Good });
            }
            let action = super::hud_block_window(ctx, &spec.id, &spec.title, [spec.width, spec.height], |ui| {
                fast = window_content(ui, &p, &spec, host);
            });
            TEXT_SCALE.with(|s| s.set(hud_scale));
            if action.close {
                engine.window_closed_by_user(&spec.id);
                crate::hud_dock::forget(&spec.id);
            }
            if action.detach {
                crate::hud_dock::detach(&spec.id);
            }
            ctx.request_repaint_after(Duration::from_millis(if fast { 1000 } else { 700 }));
            continue;
        }
        if changed {
            ctx.request_repaint_of(viewport_id(&spec.id));
        }
        let mut builder = ViewportBuilder::default()
            .with_title(format!("{} — J.A.I.", spec.title))
            .with_app_id("jai")
            .with_inner_size([spec.width, spec.height])
            .with_min_inner_size(spec.min_size)
            .with_resizable(spec.resizable)
            // See-through backgrounds (`opacity`) and the borderless look; egui re-applies a
            // changed builder to the open window.
            .with_transparent(true)
            // Always borderless: Windows never draws its grey frame around a JAI window (the
            // title moves it, the grip resizes it, the theme draws its border).
            .with_decorations(false)
            .with_window_level(if spec.always_on_top { egui::viewport::WindowLevel::AlwaysOnTop } else { egui::viewport::WindowLevel::Normal });
        if let Some(icon) = icon() {
            builder = builder.with_icon(icon);
        }
        if let Some(pos) = position(&spec, native_ppp) {
            builder = builder.with_position(pos);
            super::placed_by_order();
        } else if let Some((at, size)) = super::detached_placement(ctx, &spec.id) {
            builder = builder.with_position(at).with_inner_size(size);
        }
        let builder = super::for_test_session(builder);
        // (Asked for again while it was leaving: it stays.)
        super::detached_keep(ctx, viewport_id(&spec.id), true);
        shown_now.push((spec.clone(), builder.clone()));
        let (host, id) = (host.clone(), spec.id.clone());
        ctx.show_viewport_deferred(viewport_id(&spec.id), builder, move |ctx, class| {
            crate::crash_guard::guard("JAI window", || window_frame(ctx, class, &host, &id));
        });
    }
    // A window of its own that was closed plays its exit before its viewport goes; one that went
    // back into the HUD leaves at once, as the panels' windows do.
    let before = std::mem::take(&mut *SHOWN.lock().unwrap_or_else(|e| e.into_inner()));
    for (spec, builder) in before {
        if shown_now.iter().any(|(s, _)| s.id == spec.id) || open_ids.contains(&spec.id) || !super::detached_keep(ctx, viewport_id(&spec.id), false) {
            continue;
        }
        let (host, id) = (host.clone(), spec.id.clone());
        ctx.show_viewport_deferred(viewport_id(&spec.id), builder.clone(), move |ctx, class| {
            crate::crash_guard::guard("JAI window", || window_frame(ctx, class, &host, &id));
        });
        shown_now.push((spec, builder));
    }
    *SHOWN.lock().unwrap_or_else(|e| e.into_inner()) = shown_now;
}

/// One frame of a JAI window.
fn window_frame(ctx: &egui::Context, class: ViewportClass, host: &WidgetHost, id: &str) {
    let engine = &host.app.tools().widgets;
    // (Closed and playing its exit: drawn as it was.)
    let open = engine.windows().into_iter().find(|w| w.id == id);
    let leaving = open.is_none();
    let Some(spec) = open.or_else(|| leaving_spec(id).filter(|_| super::detached_leaving(ctx))) else {
        // Closed by JAI while the HUD was not painting: hide now, egui drops it next frame.
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        ctx.request_repaint_of(ViewportId::ROOT);
        return;
    };
    if !leaving && ctx.input(|i| i.viewport().close_requested()) {
        engine.window_closed_by_user(id);
        crate::hud_dock::forget(id);
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        ctx.request_repaint_of(ViewportId::ROOT);
        return;
    }
    let p = window_palette(host, &spec.style);
    paint_background(ctx, &p);
    let scale = spec.style.font_scale;
    // The HUD's own text scale comes back once this window is drawn.
    let hud_scale = TEXT_SCALE.with(|s| s.get());
    TEXT_SCALE.with(|s| s.set(scale));
    // The toolbar shows while the pointer is over the window (always when borderless and hovered).
    let hovered = ctx.input(|i| i.pointer.has_pointer());
    let mut fast = false;
    let body = |ui: &mut Ui| {
        // (Borderless, in a window of its own: its bare ground moves it.)
        if class != ViewportClass::Embedded {
            super::window_ground(ui);
        }
        for font in ui.style_mut().text_styles.values_mut() {
            font.size *= scale;
        }
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(&spec.title).font(bold(16.0)).color(p.accent).extra_letter_spacing(1.5)).selectable(false));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if hovered {
                    window_toolbar(ui, ctx, host, &spec);
                } else if spec.always_on_top {
                    muted(ui, &p, "PIN");
                }
            });
        });
        let r = ui.cursor();
        ui.painter().line_segment([pos2(r.left(), r.top()), pos2(r.left() + ui.available_width(), r.top())], stroke(1.0, p.line));
        ui.add_space(6.0);
        fast |= window_content(ui, &p, &spec, host);
    };
    if class == ViewportClass::Embedded {
        // Backends without multi-window support: the window becomes a floating egui window.
        egui::Window::new(&spec.title).id(egui::Id::new(("jai_embedded", id))).default_size([spec.width, spec.height]).show(ctx, body);
    } else {
        egui::CentralPanel::default().frame(Frame::none().inner_margin(Margin::same(14.0))).show(ctx, body);
        resize_grip(ctx, &p, id);
        super::paint_window_border(ctx);
    }
    TEXT_SCALE.with(|s| s.set(hud_scale));
    // Where the window really is (after Sir dragged or resized it): "make it smaller" and
    // "move it to the next monitor" start from here.
    if class != ViewportClass::Embedded {
        let rect = ctx.input(|i| i.viewport().outer_rect.zip(i.viewport().inner_rect));
        if let Some((outer, inner)) = rect {
            engine.note_geometry(id, [outer.min.x, outer.min.y, inner.width(), inner.height()]);
        }
    }
    // Live views refresh every second; repaints while hovered keep the toolbar in step.
    ctx.request_repaint_after(Duration::from_millis(if hovered { 250 } else if fast { 1000 } else { 700 }));
}

/// A JAI window's blocks and widgets (in the HUD block or the detached window); true when a
/// live view needs the 1 s refresh.
fn window_content(ui: &mut Ui, p: &Palette, spec: &NativeWindowSpec, host: &WidgetHost) -> bool {
    let engine = &host.app.tools().widgets;
    let id = spec.id.as_str();
    let p = *p;
    let mut fast = false;
    crate::hud_style::faded(ui, egui::ScrollArea::vertical().auto_shrink([false; 2]), |ui| {
        for block in &spec.blocks {
            if !block.title.is_empty() {
                ui.label(RichText::new(&block.title).font(bold(12.0)).color(p.accent));
            }
            match block.kind {
                BlockKind::Text => {
                    ui.add(egui::Label::new(RichText::new(&block.body).size(ts(13.0))).wrap());
                }
                BlockKind::Markdown => markdown_view(ui, &p, &block.body, p.accent),
                BlockKind::Code => {
                    if !block.language.is_empty() {
                        chip(ui, &block.language, p.violet);
                    }
                    code_block(ui, &p, &block.body);
                }
                BlockKind::Telemetry => {
                    fast = true;
                    Frame::none().fill(p.card).stroke(stroke(1.0, p.line)).rounding(Rounding::ZERO).inner_margin(Margin::same(10.0)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        telemetry_view(ui, &p, &host.app);
                    });
                }
                BlockKind::PcStats => {
                    fast = true;
                    pc_stats_view(ui, &p, &host.app, id);
                }
                BlockKind::DriveSpace => {
                    fast = true;
                    drive_space_view(ui, &p, &host.app);
                }
                BlockKind::Processes => {
                    fast = true;
                    processes_view(ui, &p, host);
                }
                BlockKind::Network => {
                    fast = true;
                    network_view(ui, &p, &host.app, id);
                }
            }
            ui.add_space(8.0);
        }
        for wid in &spec.widgets {
            if let Some(w) = engine.widget(wid) {
                fast |= matches!(w.kind, WidgetKind::CountdownOrStopwatch { running: true, .. });
                widget_card(ui, &p, &w, host, true);
            }
        }
        if spec.blocks.is_empty() && spec.widgets.is_empty() {
            muted(ui, &p, "Empty window — ask JAI to put something here.");
        }
    });
    fast
}

/// A JAI window's colours: the HUD's, with the window's own theme, colours and opacity.
fn window_palette(host: &WidgetHost, style: &widgets::WindowStyle) -> Palette {
    let mut iface = host.app.config().read(|c| c.interface.clone());
    if let Some(t) = style.theme.as_deref().and_then(crate::interface::theme) {
        iface.accent = t.accent.to_string();
        iface.background = t.background.to_string();
    }
    if let Some(a) = &style.accent {
        iface.accent = a.clone();
    }
    if let Some(b) = &style.background {
        iface.background = b.clone();
    }
    iface.opacity = (style.opacity * 100.0).round().clamp(20.0, 100.0) as u8;
    let mut p = Palette::new(&iface);
    // A colour Sir gave this one window ("make that window red") beats the theme.
    if style.accent.is_some() || style.theme.is_some() {
        p.accent = super::rgb(iface.accent_rgb());
    }
    p
}

/// The hover toolbar of a JAI window (right to left): close, theme, opacity, text size,
/// borderless and always-on-top. Every change is saved for the window.
fn window_toolbar(ui: &mut Ui, ctx: &egui::Context, host: &WidgetHost, spec: &NativeWindowSpec) {
    let engine = &host.app.tools().widgets;
    let restyle = |args: serde_json::Value| {
        if let Err(e) = engine.style_window(&spec.id, &args) {
            tracing::warn!("[WINDOW] {e}");
        }
        ctx.request_repaint_of(ViewportId::ROOT);
        ctx.request_repaint();
    };
    let small = |text: &str| RichText::new(text).size(ts(11.0));
    if ui.add(egui::Button::new(small("X"))).on_hover_text("Close this window").clicked() {
        // (It plays its exit, then goes: `show_windows`.)
        engine.window_closed_by_user(&spec.id);
        ctx.request_repaint_of(ViewportId::ROOT);
        return;
    }
    let current = spec.style.theme.as_deref().and_then(crate::interface::theme).map(|t| t.display).unwrap_or("HUD theme");
    egui::ComboBox::from_id_salt(("jai_window_theme", &spec.id)).width(ts(92.0)).selected_text(small(current)).show_ui(ui, |ui| {
        if ui.selectable_label(spec.style.theme.is_none() && spec.style.accent.is_none(), "Like the HUD").clicked() {
            restyle(serde_json::json!({ "theme": "hud" }));
        }
        for t in crate::interface::THEMES {
            if ui.selectable_label(spec.style.theme.as_deref() == Some(t.name), t.display).clicked() {
                restyle(serde_json::json!({ "theme": t.name }));
            }
        }
    });
    let mut opacity = spec.style.opacity * 100.0;
    ui.spacing_mut().slider_width = ts(70.0);
    let slider = ui.add(egui::Slider::new(&mut opacity, 20.0..=100.0).step_by(5.0).show_value(false));
    if slider.on_hover_text(format!("Background opacity {opacity:.0} %")).changed() {
        restyle(serde_json::json!({ "opacity": opacity / 100.0 }));
    }
    if ui.add(egui::Button::new(small("A+"))).on_hover_text("Bigger text").clicked() {
        restyle(serde_json::json!({ "font_scale": "bigger" }));
    }
    if ui.add(egui::Button::new(small("A−"))).on_hover_text("Smaller text").clicked() {
        restyle(serde_json::json!({ "font_scale": "smaller" }));
    }
    let framed = spec.style.decorations;
    if ui.add(egui::Button::new(small(if framed { "[ ]" } else { "[X]" })).selected(!framed)).on_hover_text(if framed { "Borderless" } else { "Show the title bar" }).clicked() {
        restyle(serde_json::json!({ "borderless": framed }));
    }
    let pin = if spec.always_on_top { "On top" } else { "PIN" };
    if ui.add(egui::Button::new(small(pin)).selected(spec.always_on_top)).on_hover_text("Keep this window above others").clicked() {
        restyle(serde_json::json!({ "always_on_top": !spec.always_on_top }));
    }
}

/// Bottom-right corner handle that resizes a borderless window.
fn resize_grip(ctx: &egui::Context, p: &Palette, id: &str) {
    egui::Area::new(egui::Id::new(("jai_window_grip", id))).anchor(egui::Align2::RIGHT_BOTTOM, vec2(-2.0, -2.0)).show(ctx, |ui| {
        let (rect, response) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::drag());
        for k in [4.0, 8.0, 12.0] {
            ui.painter().line_segment([pos2(rect.right() - k, rect.bottom()), pos2(rect.right(), rect.bottom() - k)], stroke(1.0, p.muted));
        }
        if response.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast));
        }
        response.on_hover_cursor(egui::CursorIcon::ResizeSouthEast);
    });
}
