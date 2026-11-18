//! The kit's controls as egui widgets: each takes its room in a `Ui`, answers the pointer and
//! the keyboard, and paints itself with `holo_kit`. Panels are laid out with these; egui's own
//! buttons, checkboxes, sliders and combo boxes are not used.

use crate::holo::{self, a, mix, Face};
use crate::holo_kit as kit;
use eframe::egui::{self, emath::Numeric, pos2, vec2, Align2, Color32, Rect, Response, Sense, Shape, Stroke, Ui, WidgetInfo, WidgetText, WidgetType};
use std::ops::RangeInclusive;

/// A row of a menu or a list.
pub const ROW_H: f32 = 22.0;
const BODY: f32 = 13.5;

/// How far the pointer (or the keyboard's focus) has lit a control, 0-1.
fn hot(ui: &Ui, r: &Response) -> f32 {
    ui.ctx().animate_bool_with_time(r.id.with("holo_hot"), r.hovered() || r.has_focus(), crate::holo_motion::TOUCH)
}

/// A control that cannot be used steps back.
fn alpha(ui: &Ui) -> f32 {
    if ui.is_enabled() {
        1.0
    } else {
        0.38
    }
}

fn plain(text: impl Into<WidgetText>) -> String {
    text.into().text().to_string()
}

/// A key: press it and something happens.
pub fn key(ui: &mut Ui, label: impl Into<WidgetText>) -> Response {
    key_on(ui, label, false)
}

/// A key that shows whether its thing is chosen (a tab, a mode): solid while it is.
pub fn key_on(ui: &mut Ui, label: impl Into<WidgetText>, on: bool) -> Response {
    let label = plain(label);
    let width = kit::key_width(ui.painter(), &label);
    let (rect, response) = ui.allocate_exact_size(vec2(width, kit::KEY_H), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, ui.is_enabled(), on, &label));
    if ui.is_rect_visible(rect) {
        kit::key(ui.painter(), rect, &label, hot(ui, &response), response.is_pointer_button_down_on(), on, alpha(ui));
    }
    response
}

/// A key in the alert colour: it ends, deletes or shuts something down.
pub fn key_alert(ui: &mut Ui, label: impl Into<WidgetText>) -> Response {
    let label = plain(label);
    let (rect, response) = ui.allocate_exact_size(vec2(kit::key_width(ui.painter(), &label), kit::KEY_H), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), &label));
    if ui.is_rect_visible(rect) {
        kit::key_tinted(ui.painter(), rect, &label, hot(ui, &response), response.is_pointer_button_down_on(), false, alpha(ui), Some(holo::theme().alert()));
    }
    response
}

/// One of several values as a key: pressing it chooses `value`.
pub fn choice<T: PartialEq>(ui: &mut Ui, current: &mut T, value: T, label: impl Into<WidgetText>) -> Response {
    let mut response = key_on(ui, label, *current == value);
    if response.clicked() && *current != value {
        *current = value;
        response.mark_changed();
    }
    response
}

/// A switch with what it switches written beside it.
pub fn toggle(ui: &mut Ui, on: &mut bool, text: impl Into<WidgetText>) -> Response {
    let text = plain(text);
    let t = holo::theme();
    let wrap = (ui.available_width() - 44.0).max(80.0);
    let galley = ui.painter().layout(text.clone(), holo::font(Face::Body, BODY), Color32::WHITE, wrap);
    let size = vec2(if text.is_empty() { 34.0 } else { 44.0 + galley.size().x }, galley.size().y.max(kit::KEY_H));
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), *on, &text));
    if ui.is_rect_visible(rect) {
        let slide = ui.ctx().animate_bool_with_time(response.id.with("holo_on"), *on, 0.14);
        let (lit, fade) = (hot(ui, &response), alpha(ui));
        kit::toggle(ui.painter(), Rect::from_min_size(rect.min, vec2(34.0, kit::KEY_H)), slide, lit, fade);
        let ink = a(mix(t.muted(), t.ink(), 0.5 + 0.5 * slide.max(lit)), fade);
        ui.painter().galley(pos2(rect.left() + 44.0, rect.center().y - galley.size().y / 2.0), galley, ink);
    }
    response
}

/// A value on a line: drag the caret, or step it with the arrow keys.
pub struct Slider<'a> {
    get_set: Box<dyn 'a + FnMut(Option<f64>) -> f64>,
    range: RangeInclusive<f64>,
    step: f64,
    integral: bool,
    suffix: String,
    text: String,
    show_value: bool,
}

impl<'a> Slider<'a> {
    pub fn new<N: Numeric>(value: &'a mut N, range: RangeInclusive<N>) -> Self {
        let range = range.start().to_f64()..=range.end().to_f64();
        Self {
            get_set: Box::new(move |set| {
                if let Some(v) = set {
                    *value = N::from_f64(v);
                }
                value.to_f64()
            }),
            range,
            step: 0.0,
            integral: N::INTEGRAL,
            suffix: String::new(),
            text: String::new(),
            show_value: true,
        }
    }

    pub fn step_by(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    pub fn suffix(mut self, suffix: impl ToString) -> Self {
        self.suffix = suffix.to_string();
        self
    }

    /// What it sets, written after it.
    pub fn text(mut self, text: impl ToString) -> Self {
        self.text = text.to_string();
        self
    }

    pub fn show_value(mut self, show: bool) -> Self {
        self.show_value = show;
        self
    }
}

/// `value` as a slider or a number shows it: whole where it is whole, else as few decimals as
/// its step asks for.
fn figure(value: f64, step: f64, integral: bool) -> String {
    if integral || (step >= 1.0 && step.fract() == 0.0) {
        format!("{value:.0}")
    } else if step >= 0.1 || step == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

impl egui::Widget for Slider<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let t = holo::theme();
        let (lo, hi) = (*self.range.start(), *self.range.end());
        let track_w = ui.spacing().slider_width.max(120.0);
        let value_w = if self.show_value { 58.0 } else { 0.0 };
        let text = (!self.text.is_empty()).then(|| holo::galley(ui.painter(), Face::Body, &self.text, BODY, Color32::WHITE));
        let text_w = text.as_ref().map_or(0.0, |g| g.size().x + 8.0);
        let (rect, mut response) = ui.allocate_exact_size(vec2(track_w + value_w + text_w, kit::KEY_H), Sense::click_and_drag());
        let track = Rect::from_min_size(pos2(rect.left() + 4.0, rect.top()), vec2(track_w - 8.0, rect.height()));
        let mut value = (self.get_set)(None);
        let snap = |v: f64| {
            let v = if self.step > 0.0 { lo + ((v - lo) / self.step).round() * self.step } else { v };
            (if self.integral { v.round() } else { v }).clamp(lo, hi)
        };
        let mut wanted = None;
        if let Some(at) = response.interact_pointer_pos() {
            wanted = Some(snap(lo + (hi - lo) * ((at.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64));
        } else if response.has_focus() {
            let step = if self.step > 0.0 { self.step } else { (hi - lo) / 100.0 };
            let turn = ui.input(|i| i.key_pressed(egui::Key::ArrowRight) as i32 - i.key_pressed(egui::Key::ArrowLeft) as i32);
            if turn != 0 {
                wanted = Some(snap(value + step * turn as f64));
            }
        }
        if let Some(next) = wanted.filter(|next| *next != value) {
            value = (self.get_set)(Some(next));
            response.mark_changed();
        }
        response.widget_info(|| WidgetInfo::slider(ui.is_enabled(), value, &self.text));
        if ui.is_rect_visible(rect) {
            let (lit, fade) = (hot(ui, &response).max(response.dragged() as u8 as f32), alpha(ui));
            let share = if hi > lo { ((value - lo) / (hi - lo)) as f32 } else { 0.0 };
            kit::slider(ui.painter(), track, share, lit, fade);
            let mut x = rect.left() + track_w + 6.0;
            if self.show_value {
                holo::text(ui.painter(), pos2(x, rect.center().y), Align2::LEFT_CENTER, Face::Mono, &format!("{}{}", figure(value, self.step, self.integral), self.suffix), 11.5, a(t.ink(), fade));
                x += value_w;
            }
            if let Some(g) = text {
                ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, a(t.muted(), fade));
            }
        }
        response
    }
}

/// A number: drag it sideways, turn it with the arrow keys, or click it and type.
pub struct Number<'a> {
    get_set: Box<dyn 'a + FnMut(Option<f64>) -> f64>,
    range: RangeInclusive<f64>,
    speed: f64,
    integral: bool,
    decimals: Option<usize>,
    prefix: String,
    suffix: String,
}

impl<'a> Number<'a> {
    pub fn new<N: Numeric>(value: &'a mut N) -> Self {
        Self {
            get_set: Box::new(move |set| {
                if let Some(v) = set {
                    *value = N::from_f64(v);
                }
                value.to_f64()
            }),
            range: N::MIN.to_f64()..=N::MAX.to_f64(),
            speed: if N::INTEGRAL { 0.25 } else { 0.1 },
            integral: N::INTEGRAL,
            decimals: None,
            prefix: String::new(),
            suffix: String::new(),
        }
    }

    pub fn range<N: Numeric>(mut self, range: RangeInclusive<N>) -> Self {
        self.range = range.start().to_f64()..=range.end().to_f64();
        self
    }

    /// How much one point of dragging changes it.
    pub fn speed(mut self, speed: impl Into<f64>) -> Self {
        self.speed = speed.into();
        self
    }

    pub fn prefix(mut self, prefix: impl ToString) -> Self {
        self.prefix = prefix.to_string();
        self
    }

    pub fn suffix(mut self, suffix: impl ToString) -> Self {
        self.suffix = suffix.to_string();
        self
    }

    pub fn max_decimals(mut self, decimals: usize) -> Self {
        self.decimals = Some(decimals);
        self
    }

    pub fn fixed_decimals(self, decimals: usize) -> Self {
        self.max_decimals(decimals)
    }

    fn shown(&self, value: f64) -> String {
        match self.decimals {
            _ if self.integral => format!("{value:.0}"),
            Some(n) => format!("{value:.n$}"),
            None => figure(value, self.speed, false),
        }
    }
}

impl egui::Widget for Number<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let t = holo::theme();
        let (lo, hi) = (*self.range.start(), *self.range.end());
        let mut value = (self.get_set)(None);
        let fit = |v: f64, integral: bool| (if integral { v.round() } else { v }).clamp(lo, hi);
        let shown = format!("{}{}{}", self.prefix, self.shown(value), self.suffix);
        let width = (holo::galley(ui.painter(), Face::Mono, &shown, 12.0, Color32::WHITE).size().x + 18.0).max(46.0);
        let (rect, mut response) = ui.allocate_exact_size(vec2(width, kit::KEY_H), Sense::click_and_drag());
        let edit_id = response.id.with("holo_number_edit");
        let drag_id = response.id.with("holo_number_drag");
        // Typing: a click turns it into a line of text until Enter or a click elsewhere.
        if let Some(mut typed) = ui.data(|d| d.get_temp::<String>(edit_id)) {
            let field = ui.put(rect.shrink2(vec2(4.0, 1.0)), egui::TextEdit::singleline(&mut typed).frame(false).font(holo::font(Face::Mono, 12.0)).text_color(t.ink()));
            if !field.has_focus() && !field.lost_focus() {
                field.request_focus();
            }
            if field.lost_focus() {
                if let Ok(v) = typed.trim().replace(',', ".").parse::<f64>() {
                    (self.get_set)(Some(fit(v, self.integral)));
                    response.mark_changed();
                }
                ui.data_mut(|d| d.remove::<String>(edit_id));
            } else {
                ui.data_mut(|d| d.insert_temp(edit_id, typed));
            }
            holo::rule(ui.painter(), rect.left_bottom(), rect.right_bottom(), t.accent());
            return response;
        }
        if response.clicked() {
            ui.data_mut(|d| d.insert_temp(edit_id, self.shown(value)));
        }
        let mut wanted = None;
        if response.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_id, value));
        }
        if response.dragged() {
            // (The exact value rides along: a whole number would swallow slow drags.)
            let exact = ui.data(|d| d.get_temp::<f64>(drag_id)).unwrap_or(value) + response.drag_delta().x as f64 * self.speed;
            ui.data_mut(|d| d.insert_temp(drag_id, exact));
            wanted = Some(fit(exact, self.integral));
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        } else if response.has_focus() {
            let turn = ui.input(|i| i.key_pressed(egui::Key::ArrowUp) as i32 - i.key_pressed(egui::Key::ArrowDown) as i32);
            if turn != 0 {
                wanted = Some(fit(value + turn as f64 * if self.integral { 1.0 } else { self.speed.max(0.0001) }, self.integral));
            }
        }
        if let Some(next) = wanted.filter(|next| *next != value) {
            value = (self.get_set)(Some(next));
            response.mark_changed();
        }
        response.widget_info(|| WidgetInfo::drag_value(ui.is_enabled(), value));
        if ui.is_rect_visible(rect) {
            let (lit, fade) = (hot(ui, &response).max(response.dragged() as u8 as f32), alpha(ui));
            let shown = format!("{}{}{}", self.prefix, self.shown(value), self.suffix);
            holo::rule(ui.painter(), rect.left_bottom(), rect.right_bottom(), a(mix(a(t.ink(), 0.30), t.accent(), lit), fade));
            holo::rule(ui.painter(), rect.left_bottom(), rect.left_bottom() - vec2(0.0, 4.0), a(t.ink(), 0.45 * fade));
            holo::rule(ui.painter(), rect.right_bottom(), rect.right_bottom() - vec2(0.0, 4.0), a(t.ink(), 0.45 * fade));
            holo::text(ui.painter(), rect.center(), Align2::CENTER_CENTER, Face::Mono, &shown, 12.0, a(mix(t.ink(), t.hot(), lit), fade));
        }
        response.on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
    }
}

/// One thing chosen from a list that drops under it.
pub struct Picker {
    id: egui::Id,
    width: Option<f32>,
    text: String,
}

impl Picker {
    pub fn from_id_salt(salt: impl std::hash::Hash) -> Self {
        Self { id: egui::Id::new(("holo_picker", salt)), width: None, text: String::new() }
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn selected_text(mut self, text: impl Into<WidgetText>) -> Self {
        self.text = plain(text);
        self
    }

    /// Draws it; `list` fills what drops under it (`item`, `option`) while it is open.
    pub fn show_ui<R>(self, ui: &mut Ui, list: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<Option<R>> {
        let t = holo::theme();
        let natural = holo::galley(ui.painter(), Face::Body, &self.text, BODY, Color32::WHITE).size().x + 34.0;
        let width = self.width.unwrap_or(natural.max(110.0)).max(60.0);
        let (rect, response) = ui.allocate_exact_size(vec2(width, kit::KEY_H), Sense::click());
        let popup = ui.make_persistent_id(self.id);
        if response.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup));
        }
        let open = ui.memory(|m| m.is_popup_open(popup));
        response.widget_info(|| WidgetInfo::labeled(WidgetType::ComboBox, ui.is_enabled(), &self.text));
        if ui.is_rect_visible(rect) {
            let (lit, fade) = (hot(ui, &response).max(open as u8 as f32), alpha(ui));
            let p = ui.painter();
            let shape = holo::plate(rect, [0.0, 0.0, 6.0, 0.0]);
            holo::fill(p, &shape, a(t.ink(), 0.05 * lit * fade));
            holo::outline(p, &shape, a(mix(a(t.ink(), 0.34), t.accent(), lit), fade));
            let g = holo::galley_within(p, Face::Body, &self.text, BODY, a(t.ink(), fade), width - 30.0);
            p.galley(pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0), g, t.ink());
            let c = pos2(rect.right() - 13.0, rect.center().y);
            p.add(Shape::convex_polygon(vec![c + vec2(-4.0, -2.5), c + vec2(4.0, -2.5), c + vec2(0.0, 3.0)], a(mix(t.muted(), t.accent(), lit), fade), Stroke::NONE));
        }
        let inner = egui::popup::popup_below_widget(ui, popup, &response, egui::popup::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(width.max(160.0));
            egui::ScrollArea::vertical().max_height(300.0).show(ui, list).inner
        });
        egui::InnerResponse::new(inner, response)
    }
}

/// A row of a menu or of a picker's list: lit under the pointer, marked while it is the one
/// chosen. It takes the whole width in a column, its own width in a row.
pub fn item(ui: &mut Ui, text: impl Into<WidgetText>, on: bool) -> Response {
    let text = plain(text);
    let t = holo::theme();
    let g = holo::galley(ui.painter(), Face::Body, &text, BODY, Color32::WHITE);
    let width = if ui.layout().is_horizontal() { g.size().x + 20.0 } else { ui.available_width().max(g.size().x + 20.0) };
    let (rect, response) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, ui.is_enabled(), on, &text));
    if ui.is_rect_visible(rect) {
        let (lit, fade) = (hot(ui, &response), alpha(ui));
        let p = ui.painter();
        let lift = (lit + on as u8 as f32 * 0.7).min(1.0);
        if lift > 0.01 {
            p.rect_filled(rect, 0.0, a(t.ink(), 0.07 * lift * fade));
        }
        if on {
            p.rect_filled(Rect::from_min_size(rect.min, vec2(2.0, rect.height())), 0.0, a(t.accent(), fade));
        }
        p.galley(pos2(rect.left() + 10.0, rect.center().y - g.size().y / 2.0), g, a(if on { t.accent() } else { mix(t.muted(), t.ink(), 0.6 + 0.4 * lit) }, fade));
    }
    response
}

/// A row that chooses `value`.
pub fn option<T: PartialEq>(ui: &mut Ui, current: &mut T, value: T, text: impl Into<WidgetText>) -> Response {
    let mut response = item(ui, text, *current == value);
    if response.clicked() && *current != value {
        *current = value;
        response.mark_changed();
    }
    response
}

/// A menu row that switches something: its name left, the switch right.
pub fn item_toggle(ui: &mut Ui, text: &str, on: bool) -> Response {
    let t = holo::theme();
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().max(160.0), ROW_H), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), on, text));
    if ui.is_rect_visible(rect) {
        let (lit, fade) = (hot(ui, &response), alpha(ui));
        let p = ui.painter();
        if lit > 0.01 {
            p.rect_filled(rect, 0.0, a(t.ink(), 0.07 * lit * fade));
        }
        holo::text(p, pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, Face::Body, text, BODY, a(mix(t.muted(), t.ink(), 0.6 + 0.4 * lit), fade));
        let slide = ui.ctx().animate_bool_with_time(response.id.with("holo_on"), on, 0.14);
        kit::toggle(p, Rect::from_min_size(pos2(rect.right() - 44.0, rect.top()), vec2(34.0, rect.height())), slide, lit, fade);
    }
    response
}

/// A small heading over a group of rows.
pub fn head(ui: &mut Ui, text: &str) -> Response {
    let t = holo::theme();
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().max(60.0), 18.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let drawn = holo::text(p, pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, Face::Label, text, 8.0, t.muted());
        holo::rule(p, pos2(drawn.right() + 8.0, rect.center().y), pos2(rect.right() - 6.0, rect.center().y), a(t.ink(), 0.13));
    }
    response
}

/// A numbered heading inside a panel (`holo_kit::section`).
pub fn section(ui: &mut Ui, index: &str, title: &str) -> Response {
    ui.add_space(6.0);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().max(80.0), 16.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        kit::section(ui.painter(), rect.min, rect.width(), index, title, 1.0);
    }
    ui.add_space(2.0);
    response
}

/// A hairline between groups.
pub fn rule(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(8.0), 9.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        holo::rule(ui.painter(), rect.left_center(), rect.right_center(), a(holo::theme().ink(), 0.14));
    }
}

/// A level as a row of cells, `width` wide.
pub fn bar(ui: &mut Ui, share: f32, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 10.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let t = holo::theme();
        kit::cells(ui.painter(), rect.shrink2(vec2(0.0, 1.0)), share, (width / 6.0).round().max(4.0) as usize, t.accent(), alpha(ui));
    }
    response
}

/// A line to type into (egui's own editing): a slot on a hairline that lights while it has
/// the keyboard. Pass the `TextEdit` with its options; its frame is ours.
pub fn slot(ui: &mut Ui, edit: egui::TextEdit<'_>) -> Response {
    let t = holo::theme();
    let response = ui.add(edit.frame(false).margin(egui::Margin::symmetric(6.0, 3.0)));
    let rect = response.rect;
    if ui.is_rect_visible(rect) {
        let lit = ui.ctx().animate_bool_with_time(response.id.with("holo_slot"), response.has_focus(), crate::holo_motion::TOUCH).max(hot(ui, &response) * 0.4);
        let p = ui.painter();
        let line = mix(a(t.ink(), 0.30), t.accent(), lit);
        holo::rule(p, rect.left_bottom(), rect.right_bottom(), line);
        holo::rule(p, rect.left_bottom(), rect.left_bottom() - vec2(0.0, 5.0), line);
        holo::rule(p, rect.right_bottom(), rect.right_bottom() - vec2(0.0, 5.0), line);
        if lit > 0.5 {
            let mut light = holo::Light::new(p);
            light.halo(&[rect.left_bottom(), rect.right_bottom()], false, 6.0, holo::lit(t.accent(), 0.22 * lit));
            light.paint(p);
        }
    }
    response
}

/// A part of a panel that folds: its name on a rule, a marker that turns, its body under it.
/// `salt` tells it from the others (it remembers whether it is open).
pub fn fold<R>(ui: &mut Ui, salt: impl std::hash::Hash, title: &str, open_at_first: bool, body: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let t = holo::theme();
    let id = ui.make_persistent_id(("holo_fold", salt));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open_at_first);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().max(80.0), ROW_H), Sense::click());
    if response.clicked() {
        state.toggle(ui);
    }
    response.widget_info(|| WidgetInfo::labeled(WidgetType::CollapsingHeader, ui.is_enabled(), title));
    if ui.is_rect_visible(rect) {
        let (lit, open) = (hot(ui, &response), state.openness(ui.ctx()));
        let p = ui.painter();
        // The marker: a bar that is a plus while shut (its upright arm folds away as it opens).
        let c = pos2(rect.left() + 7.0, rect.center().y);
        let ink = mix(t.muted(), t.accent(), lit.max(open));
        holo::rule(p, c - vec2(4.0, 0.0), c + vec2(4.0, 0.0), ink);
        if open < 0.99 {
            let arm = 4.0 * (1.0 - open);
            holo::rule(p, c - vec2(0.0, arm), c + vec2(0.0, arm), ink);
        }
        let drawn = holo::text(p, pos2(rect.left() + 20.0, rect.center().y), Align2::LEFT_CENTER, Face::Label, title, 8.5, mix(t.muted(), t.ink(), 0.55 + 0.45 * lit.max(open)));
        holo::rule(p, pos2(drawn.right() + 10.0, rect.center().y), pos2(rect.right(), rect.center().y), a(t.ink(), 0.13));
    }
    state.show_body_unindented(ui, body).map(|r| r.inner)
}

/// A word that is a link: underlined in the accent, the hand over it.
pub fn link(ui: &mut Ui, text: impl Into<WidgetText>) -> Response {
    let text = plain(text);
    let t = holo::theme();
    let g = holo::galley(ui.painter(), Face::Body, &text, BODY, Color32::WHITE);
    let (rect, response) = ui.allocate_exact_size(g.size(), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Link, ui.is_enabled(), &text));
    if ui.is_rect_visible(rect) {
        let lit = hot(ui, &response);
        let ink = mix(t.accent(), t.hot(), lit);
        ui.painter().galley(rect.min, g, ink);
        holo::rule(ui.painter(), rect.left_bottom(), rect.right_bottom(), a(ink, 0.5 + 0.5 * lit));
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A plate round what `body` lays out, as tall as that turns out: glass behind it, then the
/// plate's hairline, ticks and cut corner (`holo_frame::dress`). `focus` 0-1 lights its frame.
pub fn plate<R>(ui: &mut Ui, margin: f32, focus: f32, body: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let shown = egui::Frame::none().fill(holo::theme().glass()).inner_margin(egui::Margin::same(margin)).show(ui, body);
    crate::holo_frame::dress(ui.painter(), shown.response.rect, focus, 1.0);
    shown
}

/// A plate with its name in wide capitals on a rule and a tag at the rule's end (what kind of
/// thing it is), then its body.
pub fn titled_plate<R>(ui: &mut Ui, title: &str, tag: &str, body: impl FnOnce(&mut Ui) -> R) -> R {
    let t = holo::theme();
    let shown = plate(ui, 12.0, 0.0, |ui| {
        ui.set_width(ui.available_width());
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
        let p = ui.painter();
        let tag = holo::text(p, row.right_center(), Align2::RIGHT_CENTER, Face::Mono, tag, 10.0, t.faint());
        let name = holo::galley_within(p, Face::Label, title, 9.5, t.ink(), (tag.left() - row.left() - 12.0).max(20.0));
        p.galley(pos2(row.left(), holo::snap(row.center().y - name.size().y / 2.0, holo::ppp(p))), name, t.ink());
        holo::rule(p, pos2(row.left(), row.bottom() + 3.0), pos2(row.right(), row.bottom() + 3.0), a(t.ink(), 0.15));
        holo::rule(p, pos2(row.left(), row.bottom() + 3.0), pos2(row.left() + 24.0, row.bottom() + 3.0), a(t.accent(), 0.9));
        ui.add_space(8.0);
        body(ui)
    });
    ui.add_space(10.0);
    shown.inner
}

/// A link that opens `url` in the browser.
pub fn link_to(ui: &mut Ui, text: impl Into<WidgetText>, url: impl ToString) -> Response {
    let url = url.to_string();
    let response = link(ui, text);
    if response.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
    }
    response.on_hover_text(url)
}

/// A colour: a leaning swatch that opens its three sliders (hue, saturation, light) and shows
/// its hex. Returns the swatch's response, marked changed when a slider moved.
pub fn colour(ui: &mut Ui, rgb: &mut [u8; 3]) -> Response {
    let t = holo::theme();
    let (rect, mut response) = ui.allocate_exact_size(vec2(34.0, kit::KEY_H), Sense::click());
    let popup = response.id.with("holo_colour");
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    if ui.is_rect_visible(rect) {
        let lit = hot(ui, &response);
        let shape = kit::leaning(rect.shrink(2.0));
        holo::fill(ui.painter(), &shape, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
        holo::outline(ui.painter(), &kit::leaning(rect), a(mix(a(t.ink(), 0.34), t.ink(), lit), alpha(ui)));
    }
    let mut changed = false;
    egui::popup::popup_below_widget(ui, popup, &response, egui::popup::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_width(250.0);
        let (h, s, v) = crate::holo_theme::to_hsv(*rgb);
        let (mut hue, mut sat, mut val) = (h, s * 100.0, v * 100.0);
        ui.spacing_mut().slider_width = 150.0;
        for (name, value, most, unit) in [("Hue", &mut hue, 360.0, "°"), ("Saturation", &mut sat, 100.0, " %"), ("Light", &mut val, 100.0, " %")] {
            changed |= ui.add(Slider::new(value, 0.0..=most).step_by(1.0).suffix(unit).text(name)).changed();
        }
        if changed {
            *rgb = crate::holo_theme::from_hsv(hue, sat / 100.0, val / 100.0);
        }
        holo::text(ui.painter(), ui.min_rect().right_top() + vec2(-4.0, 2.0), Align2::RIGHT_TOP, Face::Mono, &format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]), 10.5, t.muted());
    });
    if changed {
        response.mark_changed();
    }
    response
}

/// How many of a header's keys leave a title `title` wide its room in a row `row` wide: all of
/// them when it fits, else the last ones go first (a narrow block keeps its title whole), and
/// the first always stays.
pub fn keys_that_fit(p: &egui::Painter, labels: &[(&str, bool, &str)], row: f32, title: f32) -> usize {
    let mut used = 0.0;
    let mut fit = 0;
    for (k, (label, _, _)) in labels.iter().enumerate() {
        used += kit::key_width(p, label) + 2.0;
        if k > 0 && row - used < title {
            break;
        }
        fit = k + 1;
    }
    fit
}

/// A row of keys at a place of the caller's choosing (a header, a plate's corner), right to
/// left from `right` and centred on `mid`: each (label, on, hint). Returns which was clicked
/// this frame and where the leftmost key begins.
pub fn keys_at(ui: &mut Ui, id: egui::Id, right: f32, mid: f32, labels: &[(&str, bool, &str)], alpha: f32) -> (Option<usize>, f32) {
    let mut clicked = None;
    let mut x = right;
    for (k, (label, on, hint)) in labels.iter().enumerate() {
        let width = kit::key_width(ui.painter(), label);
        let rect = Rect::from_min_size(pos2(x - width, mid - kit::KEY_H / 2.0), vec2(width, kit::KEY_H));
        let response = ui.interact(rect, id.with(("key", k)), Sense::click());
        kit::key(ui.painter(), rect, label, hot(ui, &response), response.is_pointer_button_down_on(), *on, alpha);
        if response.clicked() {
            clicked = Some(k);
        }
        let tip = tip_with_sentence(label, hint);
        if !tip.is_empty() {
            response.on_hover_text(tip);
        }
        x -= width + 2.0;
    }
    (clicked, x + 2.0)
}

/// What Sir can say instead of pressing a key: its tip names it (`tip_with_sentence`).
pub const SAY_INSTEAD: &[(&str, &str)] = &[("DETACH", "detach it"), ("POP OUT", "detach it"), ("UNDO", "undo that"), ("YES", "yes"), ("NO", "no"), ("NONE", "none of them")];

/// A key's tip with the sentence that does the same, unless the tip names it already.
pub fn tip_with_sentence(label: &str, hint: &str) -> String {
    match SAY_INSTEAD.iter().find(|(key, said)| *key == label && !hint.contains(said)) {
        Some((_, said)) if hint.is_empty() => format!("Say “{said}”."),
        Some((_, said)) => format!("{}. Or say “{said}”.", hint.trim_end_matches('.')),
        None => hint.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keys_tip_names_the_sentence_that_does_the_same() {
        assert_eq!(tip_with_sentence("DETACH", ""), "Say “detach it”.");
        assert_eq!(tip_with_sentence("DETACH", "A window of its own"), "A window of its own. Or say “detach it”.");
        assert_eq!(tip_with_sentence("CLOSE", "Close it"), "Close it");
    }

    /// One pass of a headless context with `body` in its central panel.
    fn pass(ctx: &egui::Context, events: Vec<egui::Event>, mut body: impl FnMut(&mut Ui)) {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0))), events, ..Default::default() };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| body(ui));
        });
    }

    fn click(at: egui::Pos2) -> Vec<egui::Event> {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        vec![egui::Event::PointerMoved(at), button(true), button(false)]
    }

    #[test]
    fn a_switch_flips_and_a_key_answers_under_the_pointer() {
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        let (mut on, mut pressed) = (false, 0);
        let mut rects = (Rect::NOTHING, Rect::NOTHING);
        let draw = |ui: &mut Ui, on: &mut bool, pressed: &mut i32| {
            let a = toggle(ui, on, "Sound cues");
            let b = key(ui, "RUN");
            *pressed += b.clicked() as i32;
            (a.rect, b.rect)
        };
        pass(&ctx, vec![], |ui| rects = draw(ui, &mut on, &mut pressed));
        pass(&ctx, click(rects.0.center()), |ui| rects = draw(ui, &mut on, &mut pressed));
        pass(&ctx, vec![], |ui| rects = draw(ui, &mut on, &mut pressed));
        assert!(on, "a click on the switch turns it on");
        assert_eq!(pressed, 0);
        pass(&ctx, click(rects.1.center()), |ui| rects = draw(ui, &mut on, &mut pressed));
        pass(&ctx, vec![], |ui| rects = draw(ui, &mut on, &mut pressed));
        assert_eq!(pressed, 1, "and one on the key presses it once");
        assert_eq!(rects.1.height(), kit::KEY_H);
    }

    #[test]
    fn a_slider_follows_the_pointer_in_its_steps_and_stays_in_range() {
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        let mut value = 40.0f32;
        let mut rect = Rect::NOTHING;
        let draw = |ui: &mut Ui, value: &mut f32| ui.add(Slider::new(value, 0.0..=100.0).step_by(5.0).suffix(" %")).rect;
        pass(&ctx, vec![], |ui| rect = draw(ui, &mut value));
        // Three quarters along the track (which starts 4 px in and is the slider width less 8).
        let track = ctx.style().spacing.slider_width.max(120.0) - 8.0;
        let at = pos2(rect.left() + 4.0 + track * 0.74, rect.center().y);
        pass(&ctx, vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }], |ui| rect = draw(ui, &mut value));
        pass(&ctx, vec![], |ui| rect = draw(ui, &mut value));
        assert_eq!(value, 75.0, "the nearest step");
        let far = pos2(rect.right() + 400.0, rect.center().y);
        pass(&ctx, vec![egui::Event::PointerMoved(far)], |ui| rect = draw(ui, &mut value));
        assert_eq!(value, 100.0, "never past its range");
        assert_eq!(figure(12.0, 5.0, false), "12");
        assert_eq!(figure(0.25, 0.01, false), "0.25");
        assert_eq!(figure(1.5, 0.1, false), "1.5");
    }

    #[test]
    fn a_number_is_dragged_whole_and_kept_in_its_range() {
        let ctx = egui::Context::default();
        holo::install_fonts(&ctx);
        let mut value = 10i32;
        let mut rect = Rect::NOTHING;
        let draw = |ui: &mut Ui, value: &mut i32| ui.add(Number::new(value).range(5..=12).speed(1.0).suffix(" s")).rect;
        pass(&ctx, vec![], |ui| rect = draw(ui, &mut value));
        let from = rect.center();
        let press = egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() };
        pass(&ctx, vec![egui::Event::PointerMoved(from), press], |ui| rect = draw(ui, &mut value));
        for step in 1..=4 {
            pass(&ctx, vec![egui::Event::PointerMoved(from + vec2(step as f32 * 10.0, 0.0))], |ui| rect = draw(ui, &mut value));
        }
        assert_eq!(value, 12, "dragged right as far as its range goes");
        let n = Number::new(&mut value).max_decimals(3);
        assert_eq!(n.shown(7.0), "7", "a whole number shows no decimals");
    }
}
