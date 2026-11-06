//! The Gym & Workout PR Coach window: calendar-detected sessions, the set logger, estimated
//! one-repetition records, the strength progression plot and the rest timer shared with the
//! mobile satellite. Child module of `gui`.

use super::{bold, button, card, item, muted, row, Palette, Tone};
use crate::app::Jarvis;
use crate::workout_coach::WorkoutSnapshot;
use eframe::egui::{self, pos2, vec2, Color32, RichText, Sense, Stroke};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;

const REFRESH_EVERY: Duration = Duration::from_secs(2);

pub(super) struct WorkoutWindow {
    pub open: bool,
    snapshot: Arc<Mutex<Option<Result<WorkoutSnapshot, String>>>>,
    fetched_at: Option<Instant>,
    exercise: String,
    sets: u32,
    reps: u32,
    weight: f32,
    rest_seconds: u32,
    status: Option<(String, bool)>,
    plot_exercise: Option<String>,
}

impl Default for WorkoutWindow {
    fn default() -> Self {
        Self {
            open: false,
            snapshot: Arc::new(Mutex::new(None)),
            fetched_at: None,
            exercise: String::new(),
            sets: 3,
            reps: 10,
            weight: 20.0,
            rest_seconds: 90,
            status: None,
            plot_exercise: None,
        }
    }
}

impl WorkoutWindow {
    fn refresh(&mut self, app: &Jarvis, rt: &Handle) {
        self.fetched_at = Some(Instant::now());
        let tools = app.tools().clone();
        let slot = self.snapshot.clone();
        rt.spawn(async move {
            let result = tokio::task::spawn_blocking(move || tools.workout.snapshot(tools.pim.calendar()))
                .await
                .unwrap_or_else(|e| Err(format!("Workout snapshot task failed: {e}")));
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        });
    }

    pub fn show(&mut self, ctx: &egui::Context, p: &Palette, app: &Jarvis, rt: &Handle) {
        if !self.open {
            return;
        }
        if self.fetched_at.is_none_or(|t| t.elapsed() >= REFRESH_EVERY) {
            self.refresh(app, rt);
        }
        let snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut open = self.open;
        egui::Window::new(RichText::new("Gym & Workout PR Coach").font(bold(14.0)).color(p.accent))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(540.0)
            .show(ctx, |ui| {
                muted(ui, p, "Or just say: \"Jarvis, log 3 by 10 bench press at 60 kilos\", \"start a 90 second rest timer\".");
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(620.0).show(ui, |ui| match &snapshot {
                    None => muted(ui, p, "Loading workout records..."),
                    Some(Err(e)) => {
                        ui.colored_label(p.red, e);
                    }
                    Some(Ok(snapshot)) => self.body(ui, p, app, rt, snapshot),
                });
            });
        self.open = open;
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn body(&mut self, ui: &mut egui::Ui, p: &Palette, app: &Jarvis, rt: &Handle, snapshot: &WorkoutSnapshot) {
        self.rest_card(ui, p, app, rt, snapshot);
        self.log_card(ui, p, app, rt, snapshot);
        card(ui, p, "CALENDAR SESSIONS (NEXT 14 DAYS)", |_| {}, |ui| {
            if snapshot.scheduled.is_empty() {
                muted(ui, p, "No workout entries found in the calendar. Entries such as \"Arme\", \"Gym\" or \"Push\" are detected automatically.");
            }
            for event in &snapshot.scheduled {
                item(ui, p, Some(p.green), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&event.title).font(bold(12.0)).color(p.text));
                        ui.label(RichText::new(&event.date).monospace().size(11.0).color(p.muted));
                        if event.all_day {
                            ui.label(RichText::new("all day").size(10.5).color(p.muted));
                        }
                    });
                });
            }
        });
        card(ui, p, "ESTIMATED PERSONAL RECORDS (1RM)", |_| {}, |ui| {
            if snapshot.records.is_empty() {
                muted(ui, p, "Log a set to start tracking records. Estimates use the Epley formula.");
            }
            for record in &snapshot.records {
                let selected = self.plot_exercise.as_deref() == Some(record.exercise.as_str());
                let response = row(ui, p, &record.exercise, format!("{:.1} kg  ({:.1} kg × {} · {})", record.estimated_1rm, record.weight_kg, record.reps, crate::crash_guard::clip(&record.date, 10)));
                if response.interact(Sense::click()).clicked() || (selected && self.plot_exercise.is_none()) {
                    self.plot_exercise = Some(record.exercise.clone());
                }
            }
        });
        self.progression_card(ui, p, snapshot);
        card(ui, p, "RECENT SETS", |_| {}, |ui| {
            if snapshot.recent.is_empty() {
                muted(ui, p, "Nothing logged yet.");
            }
            for log in snapshot.recent.iter().take(25) {
                item(ui, p, None, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} × {}", log.sets, log.reps)).monospace().size(11.5).color(p.accent));
                        ui.label(RichText::new(&log.exercise).size(12.0).color(p.text));
                        ui.label(RichText::new(format!("{:.1} kg", log.weight_kg)).size(11.5).color(p.text));
                        ui.label(RichText::new(format!("1RM ≈ {:.1} kg", log.estimated_1rm)).size(10.5).color(p.muted));
                        ui.label(RichText::new(crate::crash_guard::clip(&log.logged_at, 16)).monospace().size(10.0).color(p.muted));
                    });
                });
            }
        });
    }

    fn rest_card(&mut self, ui: &mut egui::Ui, p: &Palette, app: &Jarvis, rt: &Handle, snapshot: &WorkoutSnapshot) {
        card(ui, p, "REST TIMER (PC + PHONE)", |_| {}, |ui| {
            match &snapshot.rest {
                Some(rest) if !rest.finished => {
                    let colour = if rest.remaining_seconds <= 10 { p.amber } else { p.green };
                    ui.label(RichText::new(format!("{:02}:{:02}", rest.remaining_seconds / 60, rest.remaining_seconds % 60)).font(bold(30.0)).color(colour));
                }
                Some(_) => {
                    ui.label(RichText::new("Rest complete — next set, Sir.").font(bold(14.0)).color(p.green));
                }
                None => muted(ui, p, "No rest timer running."),
            }
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.rest_seconds).range(5..=1800).suffix(" s"));
                for preset in [60u32, 90, 120, 180] {
                    if ui.small_button(format!("{preset}s")).clicked() {
                        self.rest_seconds = preset;
                    }
                }
                if button(ui, p, "Start rest", Tone::On).clicked() {
                    let tools = app.tools().clone();
                    let seconds = u64::from(self.rest_seconds);
                    self.status = Some(match tools.workout.start_rest(seconds) {
                        Ok(timer) => (format!("Rest timer: {} s.", timer.remaining_seconds), true),
                        Err(e) => (e, false),
                    });
                    self.refresh(app, rt);
                }
                if snapshot.rest.is_some() && button(ui, p, "Clear", Tone::Danger).clicked() {
                    self.status = Some(match app.tools().workout.cancel_rest() {
                        Ok(()) => ("Rest timer cleared.".to_string(), true),
                        Err(e) => (e, false),
                    });
                    self.refresh(app, rt);
                }
            });
        });
    }

    fn log_card(&mut self, ui: &mut egui::Ui, p: &Palette, app: &Jarvis, rt: &Handle, snapshot: &WorkoutSnapshot) {
        card(ui, p, "LOG A SET", |_| {}, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Exercise").size(11.5).color(p.muted));
                ui.add(egui::TextEdit::singleline(&mut self.exercise).hint_text("Bench press").desired_width(180.0));
                ui.label(RichText::new("Sets").size(11.5).color(p.muted));
                ui.add(egui::DragValue::new(&mut self.sets).range(1..=100));
                ui.label(RichText::new("Reps").size(11.5).color(p.muted));
                ui.add(egui::DragValue::new(&mut self.reps).range(1..=1000));
                ui.label(RichText::new("kg").size(11.5).color(p.muted));
                ui.add(egui::DragValue::new(&mut self.weight).range(0.0..=2000.0).speed(0.5).fixed_decimals(1));
            });
            ui.horizontal(|ui| {
                if button(ui, p, "Log set", Tone::On).clicked() {
                    let today = chrono::Local::now().date_naive().to_string();
                    let uid = snapshot.scheduled.iter().find(|w| w.date == today).map(|w| w.uid.clone());
                    let outcome = app.tools().workout.log_set(&self.exercise, self.sets, self.reps, f64::from(self.weight), uid.as_deref());
                    self.status = Some(match outcome {
                        Ok(o) => (
                            format!(
                                "Logged {} × {} {} at {:.1} kg (1RM ≈ {:.1} kg){}",
                                o.entry.sets,
                                o.entry.reps,
                                o.entry.exercise,
                                o.entry.weight_kg,
                                o.entry.estimated_1rm,
                                if o.new_personal_record { " — new personal record!" } else { "" }
                            ),
                            true,
                        ),
                        Err(e) => (e, false),
                    });
                    self.plot_exercise = Some(self.exercise.clone());
                    self.refresh(app, rt);
                }
                if let Some((text, ok)) = &self.status {
                    ui.label(RichText::new(text).size(11.5).color(if *ok { p.green } else { p.red }));
                }
            });
        });
    }

    fn progression_card(&mut self, ui: &mut egui::Ui, p: &Palette, snapshot: &WorkoutSnapshot) {
        let mut exercises: Vec<String> = snapshot.records.iter().map(|r| r.exercise.clone()).collect();
        exercises.sort();
        if self.plot_exercise.is_none() {
            self.plot_exercise = exercises.first().cloned();
        }
        let selected = self.plot_exercise.clone().unwrap_or_default();
        let selected_key = selected.to_lowercase();
        let points: Vec<_> = snapshot.progression.iter().filter(|s| s.exercise.to_lowercase() == selected_key).collect();
        card(
            ui,
            p,
            "STRENGTH PROGRESSION",
            |ui| {
                egui::ComboBox::from_id_salt("workout_plot_exercise").selected_text(&selected).show_ui(ui, |ui| {
                    for exercise in &exercises {
                        ui.selectable_value(&mut self.plot_exercise, Some(exercise.clone()), exercise);
                    }
                });
            },
            |ui| {
                if points.len() < 2 {
                    muted(ui, p, "At least two training days are needed to draw a progression line.");
                    return;
                }
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 160.0), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, 6.0, p.well);
                let lo = points.iter().map(|s| s.estimated_1rm).fold(f64::INFINITY, f64::min);
                let hi = points.iter().map(|s| s.estimated_1rm).fold(f64::NEG_INFINITY, f64::max);
                let span = (hi - lo).max(1.0);
                let pad = 14.0;
                let inner = rect.shrink(pad);
                for i in 0..=4 {
                    let y = inner.top() + inner.height() * i as f32 / 4.0;
                    painter.line_segment([pos2(inner.left(), y), pos2(inner.right(), y)], Stroke::new(0.6_f32, p.line));
                    let value = hi - span * f64::from(i) / 4.0;
                    painter.text(pos2(inner.left() + 2.0, y - 1.0), egui::Align2::LEFT_BOTTOM, format!("{value:.0}"), egui::FontId::monospace(8.0), p.muted);
                }
                let step = if points.len() > 1 { inner.width() / (points.len() - 1) as f32 } else { 0.0 };
                let mut previous: Option<egui::Pos2> = None;
                for (i, point) in points.iter().enumerate() {
                    let x = inner.left() + step * i as f32;
                    let y = inner.bottom() - ((point.estimated_1rm - lo) / span) as f32 * inner.height();
                    let at = pos2(x, y);
                    if let Some(prev) = previous {
                        painter.line_segment([prev, at], Stroke::new(1.4_f32, p.accent));
                    }
                    painter.circle_filled(at, 2.6, p.green);
                    if i == 0 || i + 1 == points.len() || points.len() <= 8 {
                        painter.text(pos2(x, inner.bottom() + 2.0), egui::Align2::CENTER_TOP, &point.date[5..point.date.len().min(10)], egui::FontId::monospace(8.0), p.muted);
                    }
                    previous = Some(at);
                }
                let last = points[points.len() - 1];
                let first = points[0];
                let delta = last.estimated_1rm - first.estimated_1rm;
                let colour: Color32 = if delta >= 0.0 { p.green } else { p.red };
                ui.label(RichText::new(format!("{selected}: {:.1} kg → {:.1} kg ({:+.1} kg over {} sessions)", first.estimated_1rm, last.estimated_1rm, delta, points.len())).size(11.5).color(colour));
            },
        );
    }
}
