use super::{DisplaySettings, FieldMode, Selection, Tool, VolnaApp, palette};
use crate::{
    model::{ObjectPose, Point, Reflector, Scene, Shape, Source},
    runtime::Snapshot,
};
use eframe::egui::{
    self, Align2, Color32, FontId, Painter, PointerButton, Pos2, Rect, Sense, Stroke, Vec2,
};

#[derive(Clone, Copy)]
struct Camera {
    center: Point,
    zoom: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            center: Point { x: 0.2, y: 0.15 },
            zoom: 1.0,
        }
    }
}

#[derive(Clone, Copy)]
struct Transform {
    rect: Rect,
    center: Point,
    scale: f64,
}

impl Transform {
    fn screen(self, point: Point) -> Pos2 {
        let c = self.rect.center();
        Pos2::new(
            c.x + ((point.x - self.center.x) * self.scale) as f32,
            c.y - ((point.y - self.center.y) * self.scale) as f32,
        )
    }
    fn world(self, point: Pos2) -> Point {
        let c = self.rect.center();
        Point {
            x: self.center.x + f64::from(point.x - c.x) / self.scale,
            y: self.center.y - f64::from(point.y - c.y) / self.scale,
        }
    }
}

#[derive(Clone, Copy)]
struct Drag {
    object: Selection,
    origin: Point,
    start: Point,
    rotating: bool,
    initial_angle: f64,
    changed: bool,
    generation: u64,
    transform: Transform,
}

#[derive(Clone, Copy, PartialEq)]
struct TextureKey {
    revision: u64,
    epoch: u64,
    run: u64,
    step: u64,
    mode: FieldMode,
    auto: bool,
    range: u64,
    mask: bool,
    dark: bool,
}

#[derive(Default)]
pub(super) struct Canvas {
    camera: Camera,
    fit: bool,
    view_initialized: bool,
    texture: Option<egui::TextureHandle>,
    texture_key: Option<TextureKey>,
    range: f64,
    drag: Option<Drag>,
    polyline: Vec<Point>,
    read_position: Option<Point>,
    keyboard_read: bool,
    #[cfg(test)]
    last_transform: Option<Transform>,
}

impl Canvas {
    pub(super) fn reset_view(&mut self) {
        self.fit = true;
    }
    pub(super) fn cancel_gesture(&mut self) {
        self.drag = None;
        self.polyline.clear();
    }

    fn upload(
        &mut self,
        ctx: &egui::Context,
        snapshot: &Snapshot,
        display: &DisplaySettings,
        dark: bool,
    ) -> Result<bool, String> {
        let mode = display.effective_mode(snapshot);
        let key = TextureKey {
            revision: snapshot.revision,
            epoch: snapshot.epoch,
            run: snapshot.run,
            step: snapshot.step,
            mode,
            auto: display.auto_range,
            range: display.range.to_bits(),
            mask: display.show_mask,
            dark,
        };
        if self.texture_key == Some(key) {
            return Ok(true);
        }
        let values = match mode {
            FieldMode::Ez | FieldMode::Absolute => &snapshot.ez,
            FieldMode::Rms => match &snapshot.rms {
                Some(values) => values,
                None => return Ok(false),
            },
        };
        let g = snapshot.grid;
        if g.nx.checked_mul(g.ny) != Some(values.len())
            || values.len() != snapshot.mask.len()
            || g.is_empty()
        {
            return Err("Снимок имеет несовместимые размеры сетки/маски/поля.".into());
        }
        if values.iter().any(|v| !v.is_finite()) {
            return Err(
                "Снимок содержит NaN/Inf; последнее корректное изображение сохранено.".into(),
            );
        }
        let max = values.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        self.range = if display.auto_range {
            if max > 0.0 { max } else { 1.0 }
        } else {
            display.range
        };
        if !self.range.is_finite() || self.range <= 0.0 {
            return Err("Диапазон шкалы должен быть конечным и положительным.".into());
        }
        let mut pixels = Vec::with_capacity(values.len());
        // Память сетки идёт снизу вверх, текстура egui — сверху вниз.
        for y in (0..g.ny).rev() {
            for x in 0..g.nx {
                let index = g.index(x, y);
                let color = if display.show_mask && snapshot.mask[index] {
                    if dark {
                        Color32::from_gray(215)
                    } else {
                        Color32::from_gray(40)
                    }
                } else if mode == FieldMode::Ez {
                    palette::diverging(values[index] / self.range, dark)
                } else {
                    palette::sequential(values[index].abs() / self.range, dark)
                };
                pixels.push(color);
            }
        }
        let image = egui::ColorImage::new([g.nx, g.ny], pixels);
        if let Some(texture) = &mut self.texture {
            texture.set(image, egui::TextureOptions::NEAREST);
        } else {
            self.texture = Some(ctx.load_texture("field", image, egui::TextureOptions::NEAREST));
        }
        self.texture_key = Some(key);
        Ok(true)
    }
}

fn object_pose(scene: &Scene, object: Selection) -> Option<(Point, f64, f64)> {
    match object {
        Selection::Source(id) => scene.sources.iter().find(|s| s.id == id).map(|s| {
            (
                s.center,
                s.angle_deg,
                s.outline()
                    .iter()
                    .map(|p| (p.x - s.center.x).hypot(p.y - s.center.y))
                    .fold(0.0_f64, f64::max),
            )
        }),
        Selection::Probe(id) => scene
            .probes
            .iter()
            .find(|s| s.id == id)
            .map(|s| (s.position, 0.0, 0.0)),
        Selection::Reflector(id) => scene.reflectors.iter().find(|s| s.id == id).map(|s| {
            let radius = match &s.shape {
                Shape::Circle { radius_m } => *radius_m,
                Shape::Rectangle { width_m, height_m } => width_m.max(*height_m) * 0.5,
                Shape::Segment { length_m, .. } => length_m * 0.5,
                Shape::Polyline { points, .. } => points
                    .iter()
                    .map(|p| p.x.hypot(p.y))
                    .fold(0.0_f64, f64::max),
            };
            (s.center, s.angle_deg, radius)
        }),
    }
}

fn set_pose(scene: &mut Scene, object: Selection, position: Point, angle: f64) {
    match object {
        Selection::Source(id) => {
            if let Some(s) = scene.sources.iter_mut().find(|s| s.id == id) {
                s.center = position;
                s.angle_deg = angle;
            }
        }
        Selection::Reflector(id) => {
            if let Some(s) = scene.reflectors.iter_mut().find(|s| s.id == id) {
                s.center = position;
                s.angle_deg = angle;
            }
        }
        Selection::Probe(id) => {
            if let Some(s) = scene.probes.iter_mut().find(|s| s.id == id) {
                s.position = position;
            }
        }
    }
}

fn pick(scene: &Scene, world: Point, tolerance: f64) -> Option<Selection> {
    if let Some(p) = scene
        .probes
        .iter()
        .rev()
        .find(|p| (p.position.x - world.x).hypot(p.position.y - world.y) <= tolerance)
    {
        return Some(Selection::Probe(p.id));
    }
    if let Some(r) = scene.reflectors.iter().rev().find(|r| {
        r.contains(world) || (r.center.x - world.x).hypot(r.center.y - world.y) <= tolerance
    }) {
        return Some(Selection::Reflector(r.id));
    }
    scene
        .sources
        .iter()
        .rev()
        .find(|s| {
            s.contains_footprint(world)
                || (s.center.x - world.x).hypot(s.center.y - world.y) <= tolerance
        })
        .map(|s| Selection::Source(s.id))
}

fn rotation_handle(transform: Transform, center: Point, angle: f64, radius: f64) -> Pos2 {
    let a = angle.to_radians();
    let r = radius.abs() + 24.0 / transform.scale;
    transform.screen(Point {
        x: center.x + r * a.cos(),
        y: center.y + r * a.sin(),
    })
}

fn path(painter: &Painter, points: Vec<Pos2>, closed: bool, stroke: Stroke) {
    if points.len() < 2
        || points
            .iter()
            .any(|p| !p.x.is_finite() || !p.y.is_finite() || p.x.abs() > 1e7 || p.y.abs() > 1e7)
    {
        return;
    }
    painter.add(if closed {
        egui::Shape::closed_line(points, stroke)
    } else {
        egui::Shape::line(points, stroke)
    });
}

fn dashed_path(painter: &Painter, mut points: Vec<Pos2>, closed: bool, stroke: Stroke) {
    if closed && let Some(&first) = points.first() {
        points.push(first);
    }
    let length: f32 = points.windows(2).map(|p| p[0].distance(p[1])).sum();
    if !length.is_finite()
        || length > 8.0 * (painter.clip_rect().width() + painter.clip_rect().height())
    {
        return;
    }
    painter.extend(egui::Shape::dashed_line(&points, stroke, 6.0, 4.0));
}

fn preview_reflector_path(object: &Reflector, tr: Transform) -> (Vec<Pos2>, bool) {
    let project = |p| tr.screen(object.world(p));
    match &object.shape {
        Shape::Segment {
            length_m,
            thickness_m,
        } => (
            rectangle_points(*length_m, *thickness_m)
                .map(project)
                .to_vec(),
            true,
        ),
        Shape::Rectangle { width_m, height_m } => (
            rectangle_points(*width_m, *height_m).map(project).to_vec(),
            true,
        ),
        Shape::Circle { radius_m } => (
            (0..64)
                .map(|i| {
                    let a = f64::from(i) * std::f64::consts::TAU / 64.0;
                    project(Point {
                        x: radius_m * a.cos(),
                        y: radius_m * a.sin(),
                    })
                })
                .collect(),
            true,
        ),
        Shape::Polyline { points, .. } => (points.iter().copied().map(project).collect(), false),
    }
}

fn rectangle_points(width: f64, height: f64) -> [Point; 4] {
    [
        Point {
            x: -width * 0.5,
            y: -height * 0.5,
        },
        Point {
            x: width * 0.5,
            y: -height * 0.5,
        },
        Point {
            x: width * 0.5,
            y: height * 0.5,
        },
        Point {
            x: -width * 0.5,
            y: height * 0.5,
        },
    ]
}

/// Прямоугольные PEC-стенки без фиктивной поперечной заглушки на повороте/выходе.
fn source_wall_paths(source: &Source) -> Vec<Vec<Point>> {
    let x = source.length_m * 0.5;
    let half = source.width_m * 0.5;
    let outer = half + source.wall_m;
    let mut walls = if let Some(bend) = &source.bend {
        vec![
            [-x, x + outer, -outer, -half],
            [-x, x - half, half, outer],
            [x + half, x + outer, -outer, bend.length_m],
            [x - outer, x - half, half, bend.length_m],
        ]
    } else {
        vec![[-x, x, -outer, -half], [-x, x, half, outer]]
    };
    if source.closed_back {
        walls.push([-x, -x + source.wall_m, -outer, outer]);
    }
    walls
        .into_iter()
        .map(|[x0, x1, y0, y1]| {
            [
                Point { x: x0, y: y0 },
                Point { x: x1, y: y0 },
                Point { x: x1, y: y1 },
                Point { x: x0, y: y1 },
            ]
            .into_iter()
            .map(|p| source.world(p))
            .collect()
        })
        .collect()
}

fn draw_reflector(painter: &Painter, object: &Reflector, tr: Transform, stroke: Stroke) {
    let project = |p| tr.screen(object.world(p));
    match &object.shape {
        Shape::Circle { radius_m } => {
            let center = tr.screen(object.center);
            let radius = (*radius_m * tr.scale) as f32;
            if center.x.is_finite()
                && center.y.is_finite()
                && radius.is_finite()
                && (0.0..1e6).contains(&radius)
            {
                painter.circle_stroke(center, radius, stroke);
            }
        }
        Shape::Rectangle { width_m, height_m } => path(
            painter,
            rectangle_points(*width_m, *height_m).map(project).to_vec(),
            true,
            stroke,
        ),
        Shape::Segment {
            length_m,
            thickness_m,
        } => {
            let half = length_m * 0.5;
            path(
                painter,
                vec![
                    project(Point { x: -half, y: 0.0 }),
                    project(Point { x: half, y: 0.0 }),
                ],
                false,
                Stroke::new(
                    (thickness_m.abs() * tr.scale).clamp(1.0, 1e4) as f32,
                    stroke.color.gamma_multiply(0.25),
                ),
            );
            path(
                painter,
                rectangle_points(*length_m, *thickness_m)
                    .map(project)
                    .to_vec(),
                true,
                stroke,
            );
        }
        Shape::Polyline {
            points,
            thickness_m,
        } => {
            let vertices: Vec<_> = points.iter().copied().map(project).collect();
            path(
                painter,
                vertices.clone(),
                false,
                Stroke::new(
                    (thickness_m.abs() * tr.scale).clamp(1.0, 1e4) as f32,
                    stroke.color.gamma_multiply(0.25),
                ),
            );
            path(painter, vertices, false, stroke);
        }
    }
}

fn nice_tick(span: f64) -> f64 {
    let raw = span / 7.0;
    let power = 10.0_f64.powf(raw.log10().floor());
    let factor = raw / power;
    power
        * if factor > 5.0 {
            10.0
        } else if factor > 2.0 {
            5.0
        } else if factor > 1.0 {
            2.0
        } else {
            1.0
        }
}

fn axes(painter: &Painter, tr: Transform, dark: bool, show_grid: bool) {
    let bottom_left = tr.world(tr.rect.left_bottom());
    let top_right = tr.world(tr.rect.right_top());
    let tick = nice_tick(top_right.x - bottom_left.x);
    let font = FontId::monospace(10.0);
    let ink = palette::ink(dark);
    for axis in [0, 1] {
        let (min, max) = if axis == 0 {
            (bottom_left.x, top_right.x)
        } else {
            (bottom_left.y, top_right.y)
        };
        let start = (min / tick).ceil() * tick;
        for i in 0..100 {
            let value = start + f64::from(i) * tick;
            if value > max {
                break;
            }
            if axis == 0 {
                let x = tr.screen(Point { x: value, y: 0.0 }).x;
                if show_grid {
                    painter.line_segment(
                        [Pos2::new(x, tr.rect.top()), Pos2::new(x, tr.rect.bottom())],
                        Stroke::new(0.5, palette::grid(dark)),
                    );
                }
                painter.text(
                    Pos2::new(x, tr.rect.bottom() - 4.0),
                    Align2::CENTER_BOTTOM,
                    format!("{:.1}", value * 1e3),
                    font.clone(),
                    ink,
                );
            } else {
                let y = tr.screen(Point { x: 0.0, y: value }).y;
                if show_grid {
                    painter.line_segment(
                        [Pos2::new(tr.rect.left(), y), Pos2::new(tr.rect.right(), y)],
                        Stroke::new(0.5, palette::grid(dark)),
                    );
                }
                painter.text(
                    Pos2::new(tr.rect.left() + 5.0, y),
                    Align2::LEFT_CENTER,
                    format!("{:.1}", value * 1e3),
                    font.clone(),
                    ink,
                );
            }
        }
    }
    painter.text(
        tr.rect.right_bottom() - Vec2::new(5.0, 20.0),
        Align2::RIGHT_BOTTOM,
        "x, мм",
        font.clone(),
        ink,
    );
    painter.text(
        tr.rect.left_top() + Vec2::new(5.0, 5.0),
        Align2::LEFT_TOP,
        "y, мм",
        font,
        ink,
    );
}

impl VolnaApp {
    fn preview_source(&self, source: &Source) -> Option<Source> {
        let ObjectPose::Source {
            center, angle_deg, ..
        } = self.scene.pose(source.id)?
        else {
            return None;
        };
        if center == source.center && angle_deg == source.angle_deg {
            return None;
        }
        let mut preview = source.clone();
        preview.center = center;
        preview.angle_deg = angle_deg;
        Some(preview)
    }

    fn preview_reflector(&self, reflector: &Reflector) -> Option<Reflector> {
        let ObjectPose::Reflector {
            center, angle_deg, ..
        } = self.scene.pose(reflector.id)?
        else {
            return None;
        };
        if center == reflector.center && angle_deg == reflector.angle_deg {
            return None;
        }
        let mut preview = reflector.clone();
        preview.center = center;
        preview.angle_deg = angle_deg;
        Some(preview)
    }

    fn interaction_pose(&self, selection: Selection) -> Option<(Point, f64, f64)> {
        let (center, angle, radius) = object_pose(&self.active_scene, selection)?;
        match self.scene.pose(selection.id()) {
            Some(
                ObjectPose::Source {
                    center, angle_deg, ..
                }
                | ObjectPose::Reflector {
                    center, angle_deg, ..
                },
            ) => Some((center, angle_deg, radius)),
            Some(ObjectPose::Probe { position, .. }) => Some((position, 0.0, radius)),
            None => Some((center, angle, radius)),
        }
    }

    fn pick_interaction(&self, point: Point, tolerance: f64) -> Option<Selection> {
        for probe in self.active_scene.probes.iter().rev() {
            if let Some(ObjectPose::Probe { position, .. }) = self.scene.pose(probe.id)
                && (position.x - point.x).hypot(position.y - point.y) <= tolerance
            {
                return Some(Selection::Probe(probe.id));
            }
        }
        for object in self.active_scene.reflectors.iter().rev() {
            if let Some(preview) = self.preview_reflector(object)
                && (preview.contains(point)
                    || (preview.center.x - point.x).hypot(preview.center.y - point.y) <= tolerance)
            {
                return Some(Selection::Reflector(object.id));
            }
        }
        for source in self.active_scene.sources.iter().rev() {
            if let Some(preview) = self.preview_source(source)
                && preview.contains_footprint(point)
            {
                return Some(Selection::Source(source.id));
            }
        }
        pick(&self.active_scene, point, tolerance)
    }

    pub(super) fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        super::stable_line(ui, egui::RichText::new(match self.tool {
            Tool::Select => "Перетащите объект · круглая ручка — поворот · колесо — масштаб · правая кнопка — камера",
            Tool::Pan => "Перетащите поле для панорамирования · колесо — масштаб",
            Tool::Polyline => "Щёлкайте вершины ломаной · двойной щелчок или Enter — завершить · Esc — отменить",
            _ => "Выберите инструмент → щёлкните в поле; однократное добавление. Пунктир виден сразу, до принятия геометрии.",
        }).small().weak());
        let size = Vec2::new(
            ui.available_width().max(50.0),
            (ui.available_height() - 67.0).max(60.0),
        );
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let plot_rect = rect.shrink(2.0);
        let width = self.active_scene.domain.width_m;
        let height = self.active_scene.domain.height_m;
        if self.canvas.fit || !self.canvas.view_initialized {
            self.canvas.camera = Camera {
                center: Point {
                    x: width * 0.5,
                    y: height * 0.5,
                },
                zoom: 1.0,
            };
            self.canvas.fit = false;
            self.canvas.view_initialized = true;
        }
        let pml = if self.display.crop_pml {
            0.0
        } else {
            self.active_scene.domain.cell_m * self.active_scene.domain.pml_cells as f64
        };
        let base = (f64::from(plot_rect.width() - 70.0).max(1.0) / (width + 2.0 * pml))
            .min(f64::from(plot_rect.height() - 55.0).max(1.0) / (height + 2.0 * pml));
        let mut tr = Transform {
            rect: plot_rect,
            center: self.canvas.camera.center,
            scale: base * self.canvas.camera.zoom,
        };
        if response.hovered()
            && let Some(mouse) = response.hover_pos()
        {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let anchor = tr.world(mouse);
                self.canvas.camera.zoom =
                    (self.canvas.camera.zoom * (f64::from(scroll) * 0.002).exp()).clamp(0.25, 80.0);
                tr.scale = base * self.canvas.camera.zoom;
                let after = tr.world(mouse);
                self.canvas.camera.center.x += anchor.x - after.x;
                self.canvas.camera.center.y += anchor.y - after.y;
            }
        }
        if response.dragged_by(PointerButton::Secondary)
            || response.dragged_by(PointerButton::Middle)
            || (self.tool == Tool::Pan && response.dragged_by(PointerButton::Primary))
        {
            let delta = ui.input(|i| i.pointer.delta());
            self.canvas.camera.center.x -= f64::from(delta.x) / tr.scale;
            self.canvas.camera.center.y += f64::from(delta.y) / tr.scale;
        }
        tr.center = self.canvas.camera.center;
        #[cfg(test)]
        {
            self.canvas.last_transform = Some(tr);
        }
        let mut commit = false;
        let mut changed = false;
        let mut live_edit = None;
        let mut final_pose = None;
        let now_s = ui.input(|i| i.time);
        if self.canvas.drag.is_some_and(|drag| {
            drag.generation != self.gesture_generation || self.cold_pending.is_some()
        }) {
            self.canvas.cancel_gesture();
        }
        if self.tool == Tool::Select
            && self.cold_pending.is_none()
            && response.drag_started_by(PointerButton::Primary)
            && let Some(pointer) = ui.input(|i| i.pointer.press_origin())
        {
            let point = tr.world(pointer);
            let handle = self
                .selection
                .and_then(|object| self.interaction_pose(object).map(|pose| (object, pose)));
            let rotating = handle.is_some_and(|(object, (center, angle, radius))| {
                !matches!(object, Selection::Probe(_))
                    && rotation_handle(tr, center, angle, radius).distance(pointer) < 12.0
            });
            if !rotating {
                self.selection = self.pick_interaction(point, 12.0 / tr.scale);
            }
            if let Some(object) = self.selection {
                if self.live_available(object.id()) {
                    if let Some((origin, initial_angle, _)) = self.interaction_pose(object) {
                        self.canvas.drag = Some(Drag {
                            object,
                            origin,
                            start: point,
                            rotating,
                            initial_angle,
                            changed: false,
                            generation: self.gesture_generation,
                            transform: tr,
                        });
                    }
                } else {
                    self.error = Some("Новый объект сначала нужно применить; live-перенос доступен только принятой геометрии.".into());
                }
            }
        }
        if (response.dragged_by(PointerButton::Primary)
            || response.drag_stopped_by(PointerButton::Primary))
            && let Some(pointer) = response.interact_pointer_pos()
            && let Some(drag) = &mut self.canvas.drag
        {
            // Resize и изменение панелей не создают движение в физических координатах.
            let point = drag.transform.world(pointer);
            let position = if drag.rotating {
                drag.origin
            } else {
                Point {
                    x: drag.origin.x + point.x - drag.start.x,
                    y: drag.origin.y + point.y - drag.start.y,
                }
            };
            let angle = if drag.rotating {
                let before = (drag.start.y - drag.origin.y).atan2(drag.start.x - drag.origin.x);
                let after = (point.y - drag.origin.y).atan2(point.x - drag.origin.x);
                drag.initial_angle + (after - before).to_degrees()
            } else {
                drag.initial_angle
            };
            set_pose(&mut self.scene, drag.object, position, angle);
            drag.changed = true;
            changed = true;
            live_edit = Some(drag.object.id());
        }
        if response.drag_stopped_by(PointerButton::Primary)
            && let Some(drag) = self.canvas.drag.take()
            && drag.changed
            && drag.generation == self.gesture_generation
        {
            final_pose = Some(drag.object.id());
        }
        if response.clicked_by(PointerButton::Primary)
            && let Some(pointer) = response.interact_pointer_pos()
        {
            response.request_focus();
            let point = tr.world(pointer);
            self.canvas.read_position = Some(point);
            match self.tool {
                Tool::Select => self.selection = self.pick_interaction(point, 12.0 / tr.scale),
                Tool::Pan => {}
                Tool::Polyline => {
                    if self
                        .canvas
                        .polyline
                        .last()
                        .is_none_or(|p| (p.x - point.x).hypot(p.y - point.y) > 2.0 / tr.scale)
                    {
                        let existing: usize = self
                            .scene
                            .reflectors
                            .iter()
                            .map(|r| match &r.shape {
                                Shape::Polyline { points, .. } => points.len(),
                                _ => 0,
                            })
                            .sum();
                        if existing + self.canvas.polyline.len() < 8192 {
                            self.canvas.polyline.push(point);
                        } else {
                            self.error = Some(
                                "Лимит 8192 вершин ломаных; завершите или отмените текущую линию."
                                    .into(),
                            );
                        }
                    }
                }
                tool => self.add_at(tool, point),
            }
        }
        if self.tool == Tool::Polyline {
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.canvas.polyline.clear();
                self.tool = Tool::Select;
            }
            if (response.double_clicked()
                || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                && self.canvas.polyline.len() >= 2
                && self.can_add(Tool::Polyline)
            {
                let points = std::mem::take(&mut self.canvas.polyline);
                let center = points[0];
                let points = points
                    .into_iter()
                    .map(|p| Point {
                        x: p.x - center.x,
                        y: p.y - center.y,
                    })
                    .collect();
                let id = self.scene.next_id();
                self.scene.reflectors.push(Reflector {
                    id,
                    name: format!("Ломаная {id}"),
                    center,
                    angle_deg: 0.0,
                    shape: Shape::Polyline {
                        points,
                        thickness_m: 2.0 * self.scene.domain.cell_m,
                    },
                });
                self.selection = Some(Selection::Reflector(id));
                self.tool = Tool::Select;
                changed = true;
                commit = true;
            }
        }
        if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            self.delete_selected();
        }
        if response.has_focus() && self.tool != Tool::Polyline {
            let (dx, dy) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowRight) as i32
                        - i.key_pressed(egui::Key::ArrowLeft) as i32,
                    i.key_pressed(egui::Key::ArrowUp) as i32
                        - i.key_pressed(egui::Key::ArrowDown) as i32,
                )
            });
            if dx != 0 || dy != 0 {
                let step = self
                    .snapshot
                    .as_ref()
                    .map_or(self.active_scene.domain.cell_m, |s| s.grid.dx);
                let p = self
                    .canvas
                    .read_position
                    .get_or_insert(self.canvas.camera.center);
                p.x += f64::from(dx) * step;
                p.y += f64::from(dy) * step;
                self.canvas.keyboard_read = true;
            }
        }
        if response.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
            self.canvas.keyboard_read = false;
        }
        if changed {
            self.edited();
        }
        if commit {
            self.apply_geometry();
        } else {
            if let Some(id) = live_edit {
                self.stage_pose(id, now_s);
            }
            self.flush_poses(now_s, final_pose);
        }
        let painter = ui.painter_at(plot_rect);
        painter.rect_filled(plot_rect, 4.0, palette::surface(self.dark));
        let mut ready = false;
        if let Some(snapshot) = &self.snapshot {
            match self
                .canvas
                .upload(ui.ctx(), snapshot, &self.display, self.dark)
            {
                Ok(available) => ready = available,
                Err(error) => self.error = Some(error),
            }
            if ready && let Some(texture) = &self.canvas.texture {
                let g = snapshot.grid;
                let low = g.point(0, 0);
                let high = g.point(g.nx - 1, g.ny - 1);
                let target = Rect::from_two_pos(
                    tr.screen(Point {
                        x: low.x - g.dx * 0.5,
                        y: high.y + g.dx * 0.5,
                    }),
                    tr.screen(Point {
                        x: high.x + g.dx * 0.5,
                        y: low.y - g.dx * 0.5,
                    }),
                );
                let useful = Rect::from_two_pos(
                    tr.screen(Point { x: 0.0, y: height }),
                    tr.screen(Point { x: width, y: 0.0 }),
                );
                let image_painter = if self.display.crop_pml {
                    painter.with_clip_rect(plot_rect.intersect(useful))
                } else {
                    painter.clone()
                };
                image_painter.image(
                    texture.id(),
                    target,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
        if !ready {
            painter.text(
                plot_rect.center(),
                Align2::CENTER_CENTER,
                if self.display.mode == FieldMode::Rms {
                    "Ожидание первого завершённого окна RMS"
                } else {
                    "Подготовка поля…"
                },
                FontId::proportional(16.0),
                palette::ink(self.dark),
            );
        }
        axes(&painter, tr, self.dark, self.display.show_grid);
        let boundary = rectangle_points(width, height).map(|p| {
            tr.screen(Point {
                x: p.x + width * 0.5,
                y: p.y + height * 0.5,
            })
        });
        path(
            &painter,
            boundary.to_vec(),
            true,
            Stroke::new(1.2, palette::ink(self.dark).gamma_multiply(0.65)),
        );
        if !self.display.crop_pml {
            painter.text(
                tr.screen(Point {
                    x: width,
                    y: height,
                }) + Vec2::new(-5.0, -5.0),
                Align2::RIGHT_BOTTOM,
                format!("CPML · {} ячеек", self.active_scene.domain.pml_cells),
                FontId::proportional(10.0),
                palette::ink(self.dark),
            );
        }
        self.draw_objects(&painter, tr);
        if self.display.texture_sign
            && self.display_mode() == FieldMode::Ez
            && let Some(snapshot) = &self.snapshot
        {
            // Редкие штрихи дают второй канал знака; длина кодирует относительный модуль.
            let useful = Rect::from_two_pos(
                tr.screen(Point { x: 0.0, y: height }),
                tr.screen(Point { x: width, y: 0.0 }),
            )
            .intersect(plot_rect);
            for ix in 0..(useful.width().max(0.0) / 18.0) as usize {
                for iy in 0..(useful.height().max(0.0) / 18.0) as usize {
                    let p = useful.min + Vec2::new(ix as f32 * 18.0 + 9.0, iy as f32 * 18.0 + 9.0);
                    let (x, y) = snapshot.grid.nearest(tr.world(p));
                    if let Some(&value) = snapshot.ez.get(snapshot.grid.index(x, y)) {
                        let relative = (value.abs() / self.canvas.range).clamp(0.0, 1.0);
                        if relative > 0.02 {
                            let len = (1.0 + 4.0 * relative) as f32;
                            let delta = Vec2::new(len, if value > 0.0 { -len } else { len });
                            painter.line_segment(
                                [p - delta, p + delta],
                                Stroke::new(1.0, palette::ink(self.dark).gamma_multiply(0.5)),
                            );
                        }
                    }
                }
            }
        }
        let cursor = if self.canvas.keyboard_read {
            self.canvas.read_position.map(|p| tr.screen(p))
        } else {
            response.hover_pos()
        };
        if let Some(pointer) = cursor.filter(|p| plot_rect.contains(*p)) {
            let world = tr.world(pointer);
            self.canvas.read_position = Some(world);
            let stroke = Stroke::new(0.75, palette::ink(self.dark).gamma_multiply(0.7));
            painter.line_segment(
                [
                    Pos2::new(pointer.x, plot_rect.top()),
                    Pos2::new(pointer.x, plot_rect.bottom()),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(plot_rect.left(), pointer.y),
                    Pos2::new(plot_rect.right(), pointer.y),
                ],
                stroke,
            );
            response.on_hover_text(self.field_readout(world));
        }
        if let Some(position) = self.canvas.read_position {
            ui.monospace(self.field_readout(position));
        }
        self.color_legend(ui, ready);
    }

    fn draw_objects(&self, painter: &Painter, tr: Transform) {
        let outline = palette::ink(self.dark);
        for s in &self.active_scene.sources {
            let selected = self.selection == Some(Selection::Source(s.id));
            let stroke = Stroke::new(if selected { 2.5 } else { 1.3 }, outline);
            for wall in source_wall_paths(s) {
                path(
                    painter,
                    wall.into_iter().map(|p| tr.screen(p)).collect(),
                    true,
                    stroke,
                );
            }
            let x = -s.length_m * 0.5 + s.source_offset_m;
            let points = rectangle_points(s.strip_m, s.width_m)
                .map(|p| tr.screen(s.world(Point { x: p.x + x, y: p.y })));
            path(
                painter,
                points.to_vec(),
                true,
                Stroke::new(1.5, palette::series(0, self.dark)),
            );
            let p = tr.screen(s.center);
            if p.x.is_finite() && p.y.is_finite() {
                painter.text(
                    p + Vec2::new(0.0, -18.0),
                    Align2::CENTER_BOTTOM,
                    format!("{}{}", s.name, if s.enabled { "" } else { " · выкл" }),
                    FontId::proportional(11.0),
                    outline,
                );
            }
        }
        for object in &self.active_scene.reflectors {
            let selected = self.selection == Some(Selection::Reflector(object.id));
            draw_reflector(
                painter,
                object,
                tr,
                Stroke::new(if selected { 2.5 } else { 1.5 }, outline),
            );
        }
        for probe in &self.active_scene.probes {
            let position = tr.screen(probe.position);
            if !position.x.is_finite() || !position.y.is_finite() {
                continue;
            }
            let slot = self
                .probe_colors
                .iter()
                .position(|id| *id == Some(probe.id))
                .unwrap_or(3);
            painter.circle_filled(position, 6.0, palette::surface(self.dark));
            painter.circle_filled(position, 4.0, palette::series(slot, self.dark));
            if self.selection == Some(Selection::Probe(probe.id)) {
                painter.circle_stroke(position, 9.0, Stroke::new(1.5, outline));
            }
            painter.text(
                position + Vec2::new(10.0, -8.0),
                Align2::LEFT_BOTTOM,
                &probe.name,
                FontId::proportional(11.0),
                outline,
            );
        }
        let preview_stroke = Stroke::new(1.5, outline.gamma_multiply(0.65));
        let label = |point: Point| {
            let position = tr.screen(point);
            if position.x.is_finite() && position.y.is_finite() {
                painter.text(
                    position + Vec2::new(8.0, 10.0),
                    Align2::LEFT_TOP,
                    "Черновик позы",
                    FontId::proportional(10.0),
                    outline,
                );
            }
        };
        for source in &self.active_scene.sources {
            if let Some(preview) = self.preview_source(source) {
                dashed_path(
                    painter,
                    preview
                        .outline()
                        .into_iter()
                        .map(|p| tr.screen(p))
                        .collect(),
                    true,
                    preview_stroke,
                );
                label(preview.center);
            }
        }
        for reflector in &self.active_scene.reflectors {
            if let Some(preview) = self.preview_reflector(reflector) {
                let (points, closed) = preview_reflector_path(&preview, tr);
                dashed_path(painter, points, closed, preview_stroke);
                label(preview.center);
            }
        }
        for probe in &self.active_scene.probes {
            if let Some(ObjectPose::Probe { position, .. }) = self.scene.pose(probe.id)
                && position != probe.position
            {
                let p = tr.screen(position);
                let vertices = vec![
                    p + Vec2::new(-7.0, -7.0),
                    p + Vec2::new(7.0, -7.0),
                    p + Vec2::new(7.0, 7.0),
                    p + Vec2::new(-7.0, 7.0),
                ];
                dashed_path(painter, vertices, true, preview_stroke);
                label(position);
            }
        }
        // Новые ID ещё отсутствуют в принятом Snapshot. Это только пунктирный
        // черновик, не PEC-маска и не обещание успешного применения.
        let new_status = if self.cold_pending.is_some() {
            "Ожидает применения"
        } else {
            "Не применён"
        };
        let new_label = |point: Point, name: &str, id: u64| {
            let position = tr.screen(point);
            if position.x.is_finite() && position.y.is_finite() {
                painter.text(
                    position + Vec2::new(8.0, 10.0),
                    Align2::LEFT_TOP,
                    format!("{new_status} · {name} (ID {id})"),
                    FontId::proportional(11.0),
                    outline,
                );
            }
        };
        for source in self
            .scene
            .sources
            .iter()
            .filter(|s| self.active_scene.pose(s.id).is_none())
        {
            dashed_path(
                painter,
                source.outline().into_iter().map(|p| tr.screen(p)).collect(),
                true,
                preview_stroke,
            );
            new_label(source.center, &source.name, source.id);
        }
        for reflector in self
            .scene
            .reflectors
            .iter()
            .filter(|r| self.active_scene.pose(r.id).is_none())
        {
            let (points, closed) = preview_reflector_path(reflector, tr);
            dashed_path(painter, points, closed, preview_stroke);
            new_label(reflector.center, &reflector.name, reflector.id);
        }
        for probe in self
            .scene
            .probes
            .iter()
            .filter(|p| self.active_scene.pose(p.id).is_none())
        {
            let p = tr.screen(probe.position);
            dashed_path(
                painter,
                vec![
                    p + Vec2::new(-7.0, -7.0),
                    p + Vec2::new(7.0, -7.0),
                    p + Vec2::new(7.0, 7.0),
                    p + Vec2::new(-7.0, 7.0),
                ],
                true,
                preview_stroke,
            );
            new_label(probe.position, &probe.name, probe.id);
        }
        if let Some(selection) = self.selection
            && let Some((center, angle, radius)) = self.interaction_pose(selection)
        {
            let c = tr.screen(center);
            if c.x.is_finite() && c.y.is_finite() {
                painter.circle_stroke(c, 5.0, Stroke::new(1.5, outline));
                if !matches!(selection, Selection::Probe(_)) {
                    let handle = rotation_handle(tr, center, angle, radius);
                    if handle.x.is_finite() && handle.y.is_finite() {
                        painter.line_segment([c, handle], Stroke::new(1.0, outline));
                        painter.circle_filled(handle, 6.0, palette::surface(self.dark));
                        painter.circle_stroke(handle, 6.0, Stroke::new(2.0, outline));
                    }
                }
            }
        }
        path(
            painter,
            self.canvas
                .polyline
                .iter()
                .copied()
                .map(|p| tr.screen(p))
                .collect(),
            false,
            Stroke::new(2.0, outline),
        );
    }

    fn field_readout(&self, point: Point) -> String {
        let coordinate = format!("x={:.3} мм · y={:.3} мм", point.x * 1e3, point.y * 1e3);
        let Some(s) = &self.snapshot else {
            return coordinate;
        };
        let (x, y) = s.grid.nearest(point);
        let node = s.grid.point(x, y);
        if (node.x - point.x).abs() > s.grid.dx || (node.y - point.y).abs() > s.grid.dx {
            return format!("{coordinate} · вне сетки");
        }
        let index = s.grid.index(x, y);
        let value = match self.display_mode() {
            FieldMode::Ez => s.ez.get(index).copied(),
            FieldMode::Absolute => s.ez.get(index).map(|v| v.abs()),
            FieldMode::Rms => s.rms.as_ref().and_then(|v| v.get(index)).copied(),
        };
        match value {
            Some(value) => format!(
                "{coordinate} · {}={value:.4e} В/м · узел ({:.2}, {:.2}) мм{}",
                self.display_title(),
                node.x * 1e3,
                node.y * 1e3,
                if s.mask.get(index) == Some(&true) {
                    " · PEC"
                } else {
                    ""
                }
            ),
            None => format!("{coordinate} · RMS ещё не готов"),
        }
    }

    fn color_legend(&self, ui: &mut egui::Ui, ready: bool) {
        let range = self.canvas.range;
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} · {}",
                self.display_title(),
                if self.display.auto_range {
                    "авто"
                } else {
                    "ручная"
                }
            ));
            let (rect, _) = ui.allocate_exact_size(Vec2::new(125.0, 12.0), Sense::hover());
            for i in 0..100 {
                let fraction = f64::from(i) / 99.0;
                let color = if self.display_mode() == FieldMode::Ez {
                    palette::diverging(2.0 * fraction - 1.0, self.dark)
                } else {
                    palette::sequential(fraction, self.dark)
                };
                let min = rect.left_top() + Vec2::new(i as f32 * rect.width() / 100.0, 0.0);
                ui.painter().rect_filled(
                    Rect::from_min_size(min, Vec2::new(rect.width() / 100.0 + 0.5, rect.height())),
                    0.0,
                    color,
                );
            }
            if ready {
                ui.label(format!(
                    "{} … {range:.3e} В/м",
                    if self.display_mode() == FieldMode::Ez {
                        format!("{:.3e}", -range)
                    } else {
                        "0".into()
                    }
                ));
            }
            if self.display.mode == FieldMode::Rms
                && let Some(s) = &self.snapshot
            {
                ui.label(format!(
                    "Окно {:.3} нс · заполнено {:.0}%",
                    self.active_scene.rms_window_s * 1e9,
                    s.rms_progress * 100.0
                ));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas_frame(app: &mut VolnaApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0))),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| app.canvas_ui(ui));
        });
        output.textures_delta.clear();
    }

    fn full_frame(
        app: &mut VolnaApp,
        ctx: &egui::Context,
        time: f64,
        size: Vec2,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| app.show(ui));
        output.textures_delta.clear();
        output
    }

    fn text_position(output: &egui::FullOutput, wanted: &str) -> Pos2 {
        fn find(shape: &egui::Shape, wanted: &str) -> Option<Pos2> {
            match shape {
                egui::Shape::Text(text) if text.galley.job.text == wanted => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| find(s, wanted)),
                _ => None,
            }
        }
        output
            .shapes
            .iter()
            .find_map(|s| find(&s.shape, wanted))
            .expect("видимый элемент интерфейса")
    }

    fn has_text(output: &egui::FullOutput, fragment: &str) -> bool {
        let mut labels = Vec::new();
        for shape in &output.shapes {
            shape_text(&shape.shape, &mut labels);
        }
        labels.iter().any(|label| label.contains(fragment))
    }

    #[test]
    fn full_ui_drag_keeps_canvas_rect_and_pose_stable_across_live_status_changes()
    -> crate::Result<()> {
        for size in [Vec2::new(1400.0, 900.0), Vec2::new(1000.0, 700.0)] {
            let mut scene = Scene::default();
            scene.sources[0].bend = Some(crate::model::GuideBend { length_m: 0.08 });
            let mut app = crate::ui::tests::app_with_scene(scene)?;
            let ctx = egui::Context::default();
            for i in 0..3 {
                full_frame(&mut app, &ctx, f64::from(i) * 0.01, size, vec![]);
            }
            let tr = app.canvas.last_transform.unwrap();
            let origin = tr.screen(app.scene.sources[0].world(Point { x: 0.04, y: 0.06 }));
            full_frame(
                &mut app,
                &ctx,
                0.03,
                size,
                vec![
                    egui::Event::PointerMoved(origin),
                    pointer_button(origin, true),
                ],
            );
            let moved = origin + Vec2::new(18.0, 8.0);
            full_frame(
                &mut app,
                &ctx,
                0.1,
                size,
                vec![egui::Event::PointerMoved(moved)],
            );
            let pose = app.scene.pose(1);
            full_frame(&mut app, &ctx, 0.11, size, vec![]);
            assert_eq!(
                app.canvas.last_transform.unwrap().rect,
                tr.rect,
                "pending не должен менять размер поля"
            );
            assert_eq!(
                app.scene.pose(1),
                pose,
                "неподвижная мышь не должна перемещать объект"
            );
            assert_eq!(
                app.canvas.last_transform.unwrap().world(moved),
                tr.world(moved)
            );
            assert!(tr.screen(tr.world(moved)).distance(moved) < 0.001);
            crate::ui::tests::wait_until(&mut app, |a| a.active_scene.pose(1) == pose)?;
            app.display.mode = FieldMode::Rms;
            full_frame(&mut app, &ctx, 0.2, size, vec![]);
            assert_eq!(
                app.canvas.last_transform.unwrap().rect,
                tr.rect,
                "accepted/RMS не должны менять размер поля"
            );
            assert_eq!(app.scene.pose(1), pose);
            let invalid = origin + Vec2::new(-400.0, 8.0);
            full_frame(
                &mut app,
                &ctx,
                0.3,
                size,
                vec![egui::Event::PointerMoved(invalid)],
            );
            crate::ui::tests::wait_until(&mut app, |a| {
                matches!(a.poses[&1].state, crate::ui::PoseState::Rejected(_))
            })?;
            let rejected_pose = app.scene.pose(1);
            let mut snapshot = (**app.snapshot.as_ref().unwrap()).clone();
            snapshot.probes[0].samples =
                vec![[0.403_329_891_584_196e-9, -2.337_905_516_805_777e-49]];
            snapshot.history_start_step += 1;
            app.accept_snapshot(std::sync::Arc::new(snapshot));
            full_frame(&mut app, &ctx, 0.4, size, vec![]);
            assert_eq!(
                app.canvas.last_transform.unwrap().rect,
                tr.rect,
                "ошибка/сегмент истории не должны менять размер поля"
            );
            assert_eq!(app.scene.pose(1), rejected_pose);
            full_frame(
                &mut app,
                &ctx,
                0.5,
                size,
                vec![pointer_button(invalid, false)],
            );
            assert!(app.canvas.drag.is_none());
        }
        Ok(())
    }

    #[test]
    fn full_ui_tool_click_draws_new_ghost_immediately_then_accepts_or_keeps_rejection()
    -> crate::Result<()> {
        for (tool, title, invalid) in [
            (Tool::Source, "+ Волновод", false),
            (Tool::BentSource, "+ Г-волновод", false),
            (Tool::BentSource, "+ Г-волновод", true),
            (Tool::Segment, "+ Отрезок", false),
            (Tool::Rectangle, "+ Прямоугольник", false),
            (Tool::Circle, "+ Круг", false),
            (Tool::Probe, "+ Зонд", false),
        ] {
            let mut app = crate::ui::tests::app_with_scene(Scene::default())?;
            let ctx = egui::Context::default();
            let size = Vec2::new(1400.0, 900.0);
            let mut output = full_frame(&mut app, &ctx, 0.0, size, vec![]);
            for i in 1..3 {
                output = full_frame(&mut app, &ctx, f64::from(i) * 0.01, size, vec![]);
            }
            let button = text_position(&output, title);
            full_frame(
                &mut app,
                &ctx,
                0.03,
                size,
                vec![
                    egui::Event::PointerMoved(button),
                    pointer_button(button, true),
                ],
            );
            full_frame(
                &mut app,
                &ctx,
                0.04,
                size,
                vec![pointer_button(button, false)],
            );
            assert_eq!(app.tool, tool);
            let point = Point {
                x: if invalid { 0.015 } else { 0.28 },
                y: 0.2,
            };
            let position = app.canvas.last_transform.unwrap().screen(point);
            let id = app.scene.next_id();
            let mask = app.snapshot.as_ref().unwrap().mask.clone();
            full_frame(
                &mut app,
                &ctx,
                0.1,
                size,
                vec![
                    egui::Event::PointerMoved(position),
                    pointer_button(position, true),
                ],
            );
            let output = full_frame(
                &mut app,
                &ctx,
                0.11,
                size,
                vec![pointer_button(position, false)],
            );
            assert_eq!(app.tool, Tool::Select, "одноразовое размещение");
            assert!(app.scene.pose(id).is_some());
            assert!(app.active_scene.pose(id).is_none());
            assert_eq!(app.snapshot.as_ref().unwrap().mask, mask);
            assert!(
                has_text(&output, "Ожидает применения"),
                "новый объект виден в том же кадре, до ack"
            );
            crate::ui::tests::wait_until(&mut app, |a| {
                a.cold_pending.is_none() && (invalid || a.active_scene.pose(id).is_some())
            })?;
            let output = full_frame(&mut app, &ctx, 0.2, size, vec![]);
            if invalid {
                assert!(app.error.is_some());
                assert!(app.active_scene.pose(id).is_none());
                assert!(
                    has_text(&output, "Не применён"),
                    "отклонённый объект остаётся видимым черновиком"
                );
                assert_eq!(app.snapshot.as_ref().unwrap().mask, mask);
            } else {
                assert!(app.active_scene.pose(id).is_some());
                assert!(!has_text(&output, "Ожидает применения"));
            }
        }
        Ok(())
    }

    #[test]
    fn full_ui_resize_during_drag_does_not_move_pose_without_pointer_motion() -> crate::Result<()> {
        let mut app = crate::ui::tests::app_with_scene(Scene::default())?;
        let ctx = egui::Context::default();
        let large = Vec2::new(1400.0, 900.0);
        for i in 0..3 {
            full_frame(&mut app, &ctx, f64::from(i) * 0.01, large, vec![]);
        }
        let tr = app.canvas.last_transform.unwrap();
        let origin = tr.screen(app.scene.sources[0].center);
        full_frame(
            &mut app,
            &ctx,
            0.03,
            large,
            vec![
                egui::Event::PointerMoved(origin),
                pointer_button(origin, true),
            ],
        );
        let pointer = origin + Vec2::new(16.0, 9.0);
        full_frame(
            &mut app,
            &ctx,
            0.1,
            large,
            vec![egui::Event::PointerMoved(pointer)],
        );
        assert!(app.canvas.drag.is_some());
        let pose = app.scene.pose(1);
        let small = Vec2::new(1000.0, 700.0);
        full_frame(&mut app, &ctx, 0.2, small, vec![]);
        assert_ne!(app.canvas.last_transform.unwrap().rect, tr.rect);
        assert_eq!(
            app.scene.pose(1),
            pose,
            "resize не является физическим переносом"
        );
        full_frame(
            &mut app,
            &ctx,
            0.3,
            small,
            vec![pointer_button(pointer, false)],
        );
        assert_eq!(app.scene.pose(1), pose);
        assert!(app.canvas.drag.is_none());
        Ok(())
    }

    fn pointer_button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    #[test]
    fn pointer_motion_and_final_flush_preserve_paused_clock_for_straight_and_bent_guides()
    -> crate::Result<()> {
        for bent in [false, true] {
            let mut scene = Scene::default();
            if bent {
                scene.sources[0].bend = Some(crate::model::GuideBend { length_m: 0.08 });
            }
            let mut app = crate::ui::tests::app_with_scene(scene)?;
            let step = app.snapshot.as_ref().unwrap().step;
            app.runtime.send(crate::runtime::Command::Step)?;
            crate::ui::tests::wait_until(&mut app, |a| a.snapshot.as_ref().unwrap().step > step)?;
            let before = app.snapshot.as_ref().unwrap().clone();
            let ctx = egui::Context::default();
            canvas_frame(&mut app, &ctx, 0.0, vec![]);
            let old_key = app.canvas.texture_key.unwrap();
            let tr = app.canvas.last_transform.expect("поле отрисовано");
            // Для Г-волновода захватываем именно второе плечо, не центр первого.
            let grab = if bent {
                app.scene.sources[0].world(Point { x: 0.04, y: 0.06 })
            } else {
                app.scene.sources[0].center
            };
            let origin = tr.screen(grab);
            canvas_frame(
                &mut app,
                &ctx,
                0.01,
                vec![
                    egui::Event::PointerMoved(origin),
                    pointer_button(origin, true),
                ],
            );
            canvas_frame(
                &mut app,
                &ctx,
                0.1,
                vec![egui::Event::PointerMoved(origin + Vec2::new(24.0, 12.0))],
            );
            canvas_frame(
                &mut app,
                &ctx,
                0.2,
                vec![egui::Event::PointerMoved(origin + Vec2::new(40.0, 16.0))],
            );
            let desired = app.scene.pose(1);
            assert!(app.canvas.drag.is_some(), "мышь ещё удерживается");
            assert_ne!(desired, before.scene.pose(1));
            crate::ui::tests::wait_until(&mut app, |a| a.active_scene.pose(1) == desired)?;
            assert!(app.canvas.drag.is_some(), "поза принята ДО release");
            canvas_frame(&mut app, &ctx, 0.201, vec![]);
            let key = app.canvas.texture_key.unwrap();
            assert_eq!((key.step, key.run), (old_key.step, old_key.run));
            assert!(
                key.revision > old_key.revision,
                "текстура обновилась на том же физическом шаге"
            );
            // Эта поза попадает под 30 Hz throttle. Release обязан отправить её без ожидания.
            let end = origin + Vec2::new(46.0, 19.0);
            canvas_frame(&mut app, &ctx, 0.202, vec![egui::Event::PointerMoved(end)]);
            assert_eq!(app.poses[&1].state, crate::ui::PoseState::Queued);
            canvas_frame(&mut app, &ctx, 0.203, vec![pointer_button(end, false)]);
            assert!(app.canvas.drag.is_none());
            assert_eq!(app.poses[&1].state, crate::ui::PoseState::Pending);
            let final_pose = app.scene.pose(1);
            crate::ui::tests::wait_until(&mut app, |a| a.active_scene.pose(1) == final_pose)?;
            let after = app.snapshot.as_ref().unwrap();
            assert_eq!(
                (after.run, after.step, after.time_s, after.running),
                (before.run, before.step, before.time_s, false)
            );
            assert_eq!(after.epoch, before.epoch);
            assert!(app.cold_pending.is_none());
        }
        Ok(())
    }

    #[test]
    fn cold_barriers_cancel_capture_and_stale_release_even_after_errors() -> crate::Result<()> {
        for action in ["apply", "load", "reset", "empty_load"] {
            let mut app = crate::ui::tests::app_with_scene(Scene::default())?;
            let ctx = egui::Context::default();
            canvas_frame(&mut app, &ctx, 0.0, vec![]);
            let origin = app
                .canvas
                .last_transform
                .unwrap()
                .screen(app.scene.sources[0].center);
            canvas_frame(
                &mut app,
                &ctx,
                0.01,
                vec![
                    egui::Event::PointerMoved(origin),
                    pointer_button(origin, true),
                ],
            );
            let end = origin + Vec2::new(24.0, 10.0);
            canvas_frame(&mut app, &ctx, 0.1, vec![egui::Event::PointerMoved(end)]);
            assert!(app.canvas.drag.is_some());
            let generation = app.gesture_generation;
            match action {
                "apply" => {
                    app.scene.domain.cell_m = 0.0;
                    app.apply();
                }
                "load" => {
                    app.path = "/volna-nonexistent-live-regression/scene.json".into();
                    app.load();
                }
                "reset" => app.reset(),
                _ => {
                    app.path.clear();
                    app.load();
                }
            }
            assert!(app.canvas.drag.is_none());
            assert!(app.poses.is_empty());
            assert!(app.gesture_generation > generation);
            let request = app.next_request_id;
            canvas_frame(
                &mut app,
                &ctx,
                0.101,
                vec![pointer_button(end + Vec2::new(20.0, 0.0), false)],
            );
            assert_eq!(
                app.next_request_id, request,
                "release после {action} не возрождает pose"
            );
            assert!(app.poses.is_empty());
            crate::ui::tests::wait_until(&mut app, |a| a.cold_pending.is_none())?;
            if action != "reset" {
                assert!(app.error.is_some());
            }
        }
        Ok(())
    }

    #[test]
    fn pointer_drag_while_running_keeps_advancing_same_run_before_release() -> crate::Result<()> {
        let mut app = crate::ui::tests::app_with_scene(Scene::default())?;
        app.runtime.send(crate::runtime::Command::Play(true))?;
        crate::ui::tests::wait_until(&mut app, |a| a.snapshot.as_ref().unwrap().running)?;
        let before = app.snapshot.as_ref().unwrap().clone();
        let ctx = egui::Context::default();
        canvas_frame(&mut app, &ctx, 0.0, vec![]);
        let origin = app
            .canvas
            .last_transform
            .unwrap()
            .screen(app.scene.sources[0].center);
        canvas_frame(
            &mut app,
            &ctx,
            0.01,
            vec![
                egui::Event::PointerMoved(origin),
                pointer_button(origin, true),
            ],
        );
        canvas_frame(
            &mut app,
            &ctx,
            0.1,
            vec![egui::Event::PointerMoved(origin + Vec2::new(24.0, 12.0))],
        );
        canvas_frame(
            &mut app,
            &ctx,
            0.2,
            vec![egui::Event::PointerMoved(origin + Vec2::new(34.0, 14.0))],
        );
        let desired = app.scene.pose(1);
        crate::ui::tests::wait_until(&mut app, |a| {
            a.active_scene.pose(1) == desired && a.snapshot.as_ref().unwrap().step > before.step
        })?;
        let after = app.snapshot.as_ref().unwrap();
        assert!(app.canvas.drag.is_some());
        assert_eq!(after.run, before.run);
        assert!(after.time_s > before.time_s && after.running);
        canvas_frame(
            &mut app,
            &ctx,
            0.21,
            vec![pointer_button(origin + Vec2::new(34.0, 14.0), false)],
        );
        app.runtime.send(crate::runtime::Command::Play(false))?;
        Ok(())
    }

    #[test]
    fn field_texture_flips_y_and_never_transposes_nodes() {
        let snapshot = Snapshot {
            revision: 1,
            scene: std::sync::Arc::new(Scene::default()),
            epoch: 1,
            run: 1,
            inherited_fields: false,
            history_start_step: 0,
            history_start_time_s: 0.0,
            grid: crate::model::GridSpec {
                nx: 2,
                ny: 2,
                pml: 0,
                dx: 0.001,
                dt: 1e-12,
            },
            ez: vec![-1.0, 0.0, 0.5, 1.0],
            rms: None,
            mask: std::sync::Arc::new(vec![false; 4]),
            time_s: 0.0,
            step: 0,
            rms_progress: 0.0,
            probes: vec![],
            steps_per_second: 0.0,
            frame_rate: 0.0,
            running: false,
        };
        let ctx = egui::Context::default();
        let mut canvas = Canvas::default();
        let mut result = Ok(false);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            result = canvas.upload(ui.ctx(), &snapshot, &DisplaySettings::default(), true);
        });
        let image = canvas
            .texture
            .as_ref()
            .and_then(|texture| output.textures_delta.set.get(&texture.id()))
            .and_then(|deltas| deltas.first())
            .map(|delta| delta.image.clone());
        output.textures_delta.clear();
        assert_eq!(result, Ok(true));
        let Some(egui::ImageData::Color(image)) = image else {
            panic!("Текстура не опубликована");
        };
        assert_eq!(image.size, [2, 2]);
        assert_eq!(
            image.pixels,
            vec![
                palette::diverging(0.5, true),
                palette::diverging(1.0, true),
                palette::diverging(-1.0, true),
                palette::diverging(0.0, true)
            ]
        );
        let mut moved = snapshot.clone();
        moved.revision += 1;
        moved.ez[0] = -0.3;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            result = canvas.upload(ui.ctx(), &moved, &DisplaySettings::default(), true);
        });
        let updated_image = output
            .textures_delta
            .set
            .get(&canvas.texture.as_ref().unwrap().id())
            .and_then(|deltas| deltas.first())
            .map(|delta| delta.image.clone());
        output.textures_delta.clear();
        assert_eq!(result, Ok(true));
        let Some(egui::ImageData::Color(updated_image)) = updated_image else {
            panic!("новая revision на том же шаге обязана обновить texture");
        };
        assert_eq!(updated_image.pixels[2], palette::diverging(-0.3, true));
        assert_ne!(updated_image.pixels[2], image.pixels[2]);
        assert_eq!(canvas.texture_key.unwrap().step, snapshot.step);
        let mut invalid = moved;
        invalid.step = 1;
        invalid.ez[0] = f64::NAN;
        assert!(
            canvas
                .upload(&ctx, &invalid, &DisplaySettings::default(), true)
                .is_err()
        );
        assert_eq!(canvas.texture_key.map(|key| key.step), Some(0));
    }

    fn shape_text(shape: &egui::Shape, text: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => text.push(t.galley.job.text.clone()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| shape_text(s, text)),
            _ => {}
        }
    }

    #[test]
    fn rms_fallback_matches_texture_legend_cursor_and_returns_to_rms() -> crate::Result<()> {
        let mut app = crate::ui::tests::app_with_scene(Scene::default())?;
        app.display.mode = FieldMode::Rms;
        let mut snapshot = (**app.snapshot.as_ref().unwrap()).clone();
        snapshot.revision += 1;
        snapshot.grid = crate::model::GridSpec {
            nx: 2,
            ny: 2,
            pml: 0,
            dx: 0.001,
            dt: 1e-12,
        };
        snapshot.ez = vec![-2.0, 0.0, 0.5, 1.0];
        snapshot.mask = std::sync::Arc::new(vec![false; 4]);
        snapshot.rms = None;
        snapshot.inherited_fields = true;
        let ctx = egui::Context::default();
        for dark in [true, false] {
            app.dark = dark;
            app.accept_snapshot(std::sync::Arc::new(snapshot.clone()));
            assert_eq!(app.display.mode, FieldMode::Rms);
            assert_eq!(app.display_mode(), FieldMode::Ez);
            assert_eq!(app.display_title(), "Ez; RMS накапливается");
            let mut uploaded = Ok(false);
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                uploaded =
                    app.canvas
                        .upload(ui.ctx(), app.snapshot.as_ref().unwrap(), &app.display, dark);
                app.color_legend(ui, true);
            });
            let image = output
                .textures_delta
                .set
                .get(&app.canvas.texture.as_ref().unwrap().id())
                .and_then(|deltas| deltas.first())
                .map(|delta| delta.image.clone());
            output.textures_delta.clear();
            assert_eq!(uploaded, Ok(true));
            let Some(egui::ImageData::Color(image)) = image else {
                panic!("нет текстуры fallback");
            };
            assert_eq!(
                image.pixels,
                vec![
                    palette::diverging(0.25, dark),
                    palette::diverging(0.5, dark),
                    palette::diverging(-1.0, dark),
                    palette::diverging(0.0, dark)
                ]
            );
            assert_eq!(app.canvas.texture_key.unwrap().mode, FieldMode::Ez);
            let mut labels = Vec::new();
            for shape in &output.shapes {
                shape_text(&shape.shape, &mut labels);
            }
            assert!(labels.iter().any(|s| s.contains("Ez; RMS накапливается")));
            assert!(labels.contains(&format!("{:.3e} … {:.3e} В/м", -2.0, 2.0)));
            for endpoint in [-1.0, 1.0] {
                assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(rect) if rect.fill == palette::diverging(endpoint, dark))), "легенда должна иметь diverging-концы");
            }
            let readout = app.field_readout(Point { x: 0.0, y: 0.0 });
            assert!(
                readout.contains("Ez; RMS накапливается=-2.0000e0 В/м"),
                "{readout}"
            );
        }
        let fallback_key = app.canvas.texture_key.unwrap();
        snapshot.rms = Some(vec![0.75; 4]);
        app.accept_snapshot(std::sync::Arc::new(snapshot.clone()));
        let mut uploaded = Ok(false);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            uploaded = app.canvas.upload(
                ui.ctx(),
                app.snapshot.as_ref().unwrap(),
                &app.display,
                app.dark,
            );
            app.color_legend(ui, true);
        });
        let image = output
            .textures_delta
            .set
            .get(&app.canvas.texture.as_ref().unwrap().id())
            .and_then(|deltas| deltas.first())
            .map(|delta| delta.image.clone());
        output.textures_delta.clear();
        assert_eq!(uploaded, Ok(true));
        assert_eq!(app.display.mode, FieldMode::Rms);
        assert_eq!(app.display_mode(), FieldMode::Rms);
        assert_eq!(app.display_title(), "RMS(Ez)");
        let rms_key = app.canvas.texture_key.unwrap();
        assert_eq!(
            (rms_key.step, rms_key.revision),
            (fallback_key.step, fallback_key.revision)
        );
        assert_eq!(rms_key.mode, FieldMode::Rms);
        let Some(egui::ImageData::Color(image)) = image else {
            panic!("нет текстуры RMS");
        };
        assert_eq!(image.pixels, vec![palette::sequential(1.0, app.dark); 4]);
        let mut labels = Vec::new();
        for shape in &output.shapes {
            shape_text(&shape.shape, &mut labels);
        }
        assert!(labels.contains(&format!("0 … {:.3e} В/м", 0.75)));
        assert!(
            app.field_readout(Point { x: 0.0, y: 0.0 })
                .contains("RMS(Ez)=7.5000e-1 В/м")
        );
        // При холодном старте RMS не подменяется Ez без inherited_fields.
        snapshot.revision += 1;
        snapshot.inherited_fields = false;
        snapshot.rms = None;
        app.accept_snapshot(std::sync::Arc::new(snapshot));
        assert_eq!(app.display_mode(), FieldMode::Rms);
        assert_eq!(
            app.canvas
                .upload(&ctx, app.snapshot.as_ref().unwrap(), &app.display, app.dark),
            Ok(false)
        );
        assert!(
            app.field_readout(Point { x: 0.0, y: 0.0 })
                .contains("RMS ещё не готов")
        );
        Ok(())
    }

    #[test]
    fn draft_pose_preview_uses_only_accepted_shape_and_handle_radius() -> crate::Result<()> {
        let mut scene = Scene::default();
        scene.sources[0].bend = Some(crate::model::GuideBend { length_m: 0.08 });
        scene.reflectors.push(Reflector {
            id: 3,
            name: "Отражатель".into(),
            center: Point { x: 0.3, y: 0.2 },
            angle_deg: 0.0,
            shape: Shape::Rectangle {
                width_m: 0.04,
                height_m: 0.03,
            },
        });
        let mut app = crate::ui::tests::app_with_scene(scene)?;
        let accepted_source = app.active_scene.sources[0].clone();
        let accepted_reflector = app.active_scene.reflectors[0].clone();
        let radius = object_pose(&app.active_scene, Selection::Source(1))
            .unwrap()
            .2;
        app.scene.sources[0].center.x += 0.01;
        app.scene.sources[0].angle_deg = 13.0;
        app.scene.sources[0].width_m = 0.1;
        app.scene.sources[0].bend = None;
        app.scene.reflectors[0].center.x += 0.01;
        app.scene.reflectors[0].shape = Shape::Circle { radius_m: 0.05 };
        let preview = app.preview_source(&accepted_source).unwrap();
        let mut expected = accepted_source;
        expected.center = app.scene.sources[0].center;
        expected.angle_deg = 13.0;
        assert_eq!(preview, expected);
        assert_eq!(preview.outline().len(), 6);
        assert_eq!(
            app.interaction_pose(Selection::Source(1)).unwrap().2,
            radius
        );
        let preview = app.preview_reflector(&accepted_reflector).unwrap();
        assert_eq!(preview.shape, accepted_reflector.shape);
        assert_eq!(preview.center, app.scene.reflectors[0].center);
        assert_eq!(app.active_scene, *app.snapshot.as_ref().unwrap().scene);
        Ok(())
    }

    #[test]
    fn bent_guide_picks_second_arm_not_missing_corner_and_has_open_pec_outlet() {
        let mut scene = Scene::default();
        scene.sources[0].bend = Some(crate::model::GuideBend { length_m: 0.08 });
        for angle in [0.0, 37.0, 180.0] {
            scene.sources[0].angle_deg = angle;
            let source = &scene.sources[0];
            assert_eq!(
                pick(&scene, source.world(Point { x: 0.04, y: 0.06 }), 0.002),
                Some(Selection::Source(1))
            );
            assert_eq!(
                pick(&scene, source.world(Point { x: -0.02, y: 0.06 }), 0.002),
                None
            );
            assert!(object_pose(&scene, Selection::Source(1)).unwrap().2 >= 0.08);
            let walls = source_wall_paths(source);
            assert_eq!(walls.len(), 5);
            // Проверяем отрисованные прямоугольники на независимых точках канала/стенок.
            let drawn_pec = |local: Point| {
                walls.iter().any(|wall| {
                    let points: Vec<_> = wall.iter().map(|p| source.local(*p)).collect();
                    let min_x = points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
                    let max_x = points.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
                    let min_y = points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
                    let max_y = points.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
                    local.x >= min_x && local.x <= max_x && local.y >= min_y && local.y <= max_y
                })
            };
            for (local, expected) in [
                (Point { x: 0.04, y: 0.0 }, false),
                (Point { x: 0.04, y: 0.079 }, false),
                (Point { x: -0.02, y: 0.06 }, false),
                (Point { x: 0.052, y: 0.06 }, true),
                (Point { x: 0.028, y: 0.06 }, true),
                (Point { x: 0.0, y: -0.012 }, true),
                (Point { x: -0.039, y: 0.0 }, true),
            ] {
                assert_eq!(drawn_pec(local), expected, "angle={angle}, point={local:?}");
                assert_eq!(source.is_pec(source.world(local)), expected);
            }
        }
    }

    #[test]
    fn world_screen_roundtrip_and_upward_y() {
        let tr = Transform {
            rect: Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(600.0, 400.0)),
            center: Point { x: 0.2, y: 0.15 },
            scale: 1500.0,
        };
        for point in [
            Point { x: 0.0, y: 0.0 },
            Point { x: 0.4, y: 0.3 },
            Point { x: 0.17, y: 0.21 },
        ] {
            let roundtrip = tr.world(tr.screen(point));
            assert!((roundtrip.x - point.x).abs() < 1e-7);
            assert!((roundtrip.y - point.y).abs() < 1e-7);
        }
        assert!(tr.screen(Point { x: 0.0, y: 0.3 }).y < tr.screen(Point { x: 0.0, y: 0.0 }).y);
    }

    #[test]
    fn picking_probes_has_priority_and_rotated_source_is_selectable() {
        let mut scene = Scene::default();
        let p = scene.probes[0].position;
        assert_eq!(
            pick(&scene, p, 0.002),
            Some(Selection::Probe(scene.probes[0].id))
        );
        scene.sources[0].angle_deg = 90.0;
        let point = scene.sources[0].world(Point { x: 0.025, y: 0.0 });
        assert_eq!(
            pick(&scene, point, 0.002),
            Some(Selection::Source(scene.sources[0].id))
        );
        assert_eq!(pick(&scene, Point { x: 0.38, y: 0.28 }, 0.002), None);
    }
}
