//! Нативный редактор: позы обновляются интерактивно, остальные настройки — холодным применением.
mod canvas;
mod inspector;
mod palette;
mod plots;

use crate::{
    model::{GuideBend, ObjectPose, Point, Probe, Reflector, Scene, Shape, Source},
    runtime::{Command, Event, Runtime, Snapshot},
};
use canvas::Canvas;
use eframe::egui::{self, RichText};
use inspector::TableDraft;
use plots::Preview;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    Source(u64),
    Reflector(u64),
    Probe(u64),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tool {
    #[default]
    Select,
    Pan,
    Source,
    BentSource,
    Segment,
    Rectangle,
    Circle,
    Polyline,
    Probe,
}

/// Начальный и выбранный в интерфейсе режим отображения поля.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldMode {
    #[default]
    Ez,
    Absolute,
    Rms,
}

impl FieldMode {
    fn title(self) -> &'static str {
        match self {
            Self::Ez => "Ez",
            Self::Absolute => "|Ez|",
            Self::Rms => "RMS(Ez)",
        }
    }
}

/// Настройки запуска; файловый ввод-вывод выполняется до создания окна.
#[derive(Debug)]
pub struct StartupOptions {
    pub scene: Scene,
    pub view: FieldMode,
    pub scene_path: String,
}

impl Default for StartupOptions {
    fn default() -> Self {
        Self {
            scene: Scene::interference_demo(),
            view: FieldMode::Ez,
            scene_path: "presets/interference_10ghz.json".into(),
        }
    }
}

struct DisplaySettings {
    mode: FieldMode,
    auto_range: bool,
    range: f64,
    crop_pml: bool,
    show_grid: bool,
    show_mask: bool,
    texture_sign: bool,
}

impl DisplaySettings {
    fn effective_mode(&self, snapshot: &Snapshot) -> FieldMode {
        if self.mode == FieldMode::Rms && snapshot.rms.is_none() && snapshot.inherited_fields {
            FieldMode::Ez
        } else {
            self.mode
        }
    }
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            mode: FieldMode::Ez,
            auto_range: true,
            range: 1.0,
            crop_pml: true,
            show_grid: true,
            show_mask: false,
            texture_sign: false,
        }
    }
}

#[derive(Clone, Copy)]
enum FileAction {
    Save,
    Export,
}

struct FileConfirmation {
    action: FileAction,
    path: PathBuf,
}

impl Selection {
    fn id(self) -> u64 {
        match self {
            Self::Source(id) | Self::Reflector(id) | Self::Probe(id) => id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColdKind {
    Initial,
    Apply,
    Load,
    Reset,
}

#[derive(Clone, Copy, Debug)]
struct ColdPending {
    request_id: u64,
    kind: ColdKind,
    edit_epoch: u64,
    start_epoch: u64,
}

#[derive(Clone, Debug, PartialEq)]
enum PoseState {
    Queued,
    Pending,
    Accepted,
    Rejected(String),
}

struct PoseEdit {
    epoch: u64,
    generation: u64,
    request_id: u64,
    pose: ObjectPose,
    state: PoseState,
}

/// Слот постоянной высоты: длинный статус не меняет геометрию поля.
fn stable_line(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    let text = text.into();
    ui.add_sized(
        [ui.available_width(), 22.0],
        egui::Label::new(text.clone()).truncate(),
    )
    .on_hover_text(text)
}

/// Единственный владелец редактируемой сцены. Снимки worker неизменяемы.
pub struct VolnaApp {
    runtime: Runtime,
    scene: Scene,
    active_scene: Scene,
    epoch: u64,
    next_request_id: u64,
    cold_pending: Option<ColdPending>,
    gesture_generation: u64,
    poses: BTreeMap<u64, PoseEdit>,
    last_pose_send_s: Option<f64>,
    edit_epoch: u64,
    snapshot: Option<Arc<Snapshot>>,
    error: Option<String>,
    notice: Option<String>,
    selection: Option<Selection>,
    tool: Tool,
    canvas: Canvas,
    display: DisplaySettings,
    dark: bool,
    speed: u32,
    path: String,
    csv_path: String,
    confirmation: Option<FileConfirmation>,
    table_drafts: BTreeMap<(u64, u8), TableDraft>,
    preview: Option<Preview>,
    probe_colors: [Option<u64>; 3],
    selected_probe_only: bool,
    show_probe_table: bool,
}

impl VolnaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> crate::Result<Self> {
        Self::new_with_options(cc, StartupOptions::default())
    }

    /// Создаёт UI из подготовленной сцены, не перечитывая файл после открытия окна.
    pub fn new_with_options(
        cc: &eframe::CreationContext<'_>,
        options: StartupOptions,
    ) -> crate::Result<Self> {
        let runtime = Runtime::new(options.scene.clone())?;
        palette::configure(&cc.egui_ctx, true);
        let mut app = Self::from_runtime(runtime, options.scene);
        app.path = options.scene_path;
        app.display.mode = options.view;
        app.selection = app.scene.sources.first().map(|s| Selection::Source(s.id));
        Ok(app)
    }

    fn from_runtime(runtime: Runtime, scene: Scene) -> Self {
        let probe_colors = std::array::from_fn(|i| scene.probes.get(i).map(|p| p.id));
        Self {
            runtime,
            active_scene: scene.clone(),
            scene,
            epoch: 1,
            next_request_id: 2,
            cold_pending: Some(ColdPending {
                request_id: 1,
                kind: ColdKind::Initial,
                edit_epoch: 0,
                start_epoch: 1,
            }),
            gesture_generation: 0,
            poses: BTreeMap::new(),
            last_pose_send_s: None,
            edit_epoch: 0,
            snapshot: None,
            error: None,
            notice: None,
            selection: Some(Selection::Source(1)),
            tool: Tool::Select,
            canvas: Canvas::default(),
            display: DisplaySettings::default(),
            dark: true,
            speed: crate::runtime::DEFAULT_STEPS_PER_BATCH,
            path: "scene.json".into(),
            csv_path: "probes.csv".into(),
            confirmation: None,
            table_drafts: BTreeMap::new(),
            preview: None,
            probe_colors,
            selected_probe_only: false,
            show_probe_table: false,
        }
    }

    fn send(&mut self, command: Command) -> bool {
        match self.runtime.send(command) {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(error.to_string());
                false
            }
        }
    }

    fn reserve_request(&mut self) -> Option<u64> {
        let Some(next) = self.next_request_id.checked_add(1) else {
            self.error = Some("Исчерпан счётчик запросов; перезапустите приложение.".into());
            return None;
        };
        let request_id = self.next_request_id;
        self.next_request_id = next;
        Some(request_id)
    }

    fn cancel_live(&mut self) {
        self.gesture_generation = self.gesture_generation.saturating_add(1);
        self.canvas.cancel_gesture();
        self.poses.clear();
        self.last_pose_send_s = None;
    }

    fn begin_cold(&mut self, request_id: u64, kind: ColdKind) {
        self.cold_pending = Some(ColdPending {
            request_id,
            kind,
            edit_epoch: self.edit_epoch,
            start_epoch: self.epoch,
        });
        self.error = None;
        self.notice = Some("Подготовка холодной сцены; live-перенос временно недоступен…".into());
    }

    fn apply(&mut self) {
        self.cancel_live();
        let Some(request_id) = self.reserve_request() else {
            return;
        };
        if self.send(Command::Apply {
            request_id,
            scene: self.scene.clone(),
        }) {
            self.begin_cold(request_id, ColdKind::Apply);
        }
    }

    fn reset(&mut self) {
        self.cancel_live();
        // У Reset нет request_id в API. Не допускаем два неразличимых Reset одновременно.
        if self.cold_pending.is_some() {
            self.notice = Some("Дождитесь завершения холодной команды перед сбросом.".into());
            return;
        }
        if self.send(Command::Reset) {
            self.begin_cold(0, ColdKind::Reset);
        }
    }

    fn apply_geometry(&mut self) {
        if self.scene.domain != self.active_scene.domain {
            self.notice = Some("Геометрия изменена в черновике. Новая сетка применяется только кнопкой «Применить и сбросить».".into());
        } else {
            self.apply();
        }
    }

    fn can_add(&mut self, tool: Tool) -> bool {
        let full = match tool {
            Tool::Source | Tool::BentSource => self.scene.sources.len() >= 16,
            Tool::Probe => self.scene.probes.len() >= 16,
            Tool::Segment | Tool::Rectangle | Tool::Circle | Tool::Polyline => {
                self.scene.reflectors.len() >= 256
            }
            Tool::Select | Tool::Pan => false,
        };
        if full {
            self.error =
                Some("Достигнут лимит числа объектов этого типа; удалите лишний объект.".into());
        }
        !full
    }

    fn load(&mut self) {
        self.cancel_live();
        if self.path.trim().is_empty() {
            self.error = Some("Введите путь JSON-сцены.".into());
            return;
        }
        let Some(request_id) = self.reserve_request() else {
            return;
        };
        if self.send(Command::Load {
            request_id,
            path: PathBuf::from(self.path.trim()),
        }) {
            self.begin_cold(request_id, ColdKind::Load);
        }
    }

    fn cold_matches(&self, epoch: u64, request_id: u64) -> bool {
        self.cold_pending.is_some_and(|pending| {
            pending.request_id == request_id
                && epoch >= pending.start_epoch
                && (pending.kind != ColdKind::Reset || epoch > pending.start_epoch)
        })
    }

    fn advance_epoch(&mut self, epoch: u64) {
        if epoch > self.epoch {
            self.cancel_live();
            self.epoch = epoch;
        }
    }

    fn accept_event(&mut self, event: Event) {
        match event {
            Event::Applied {
                epoch,
                request_id,
                revision,
                scene,
            } if self.cold_matches(epoch, request_id) => {
                let pending = self.cold_pending.take();
                self.advance_epoch(epoch);
                if pending
                    .is_some_and(|p| p.kind == ColdKind::Load && p.edit_epoch == self.edit_epoch)
                {
                    self.scene = *scene;
                    self.selection = None;
                    self.table_drafts.clear();
                    self.preview = None;
                    self.canvas.reset_view();
                    self.probe_colors =
                        std::array::from_fn(|i| self.scene.probes.get(i).map(|p| p.id));
                }
                // Принятая геометрия меняется только вместе с соответствующим Snapshot.
                self.error = None;
                self.notice = Some(format!(
                    "Холодная сцена r{revision} принята. Поле сброшено."
                ));
            }
            Event::Error {
                epoch,
                request_id,
                message,
            } => {
                let cold = self.cold_matches(epoch, request_id);
                if cold {
                    self.cold_pending = None;
                    self.advance_epoch(epoch);
                }
                if cold || (request_id == 0 && (epoch == 0 || epoch >= self.epoch)) {
                    if epoch == 0 {
                        self.cold_pending = None;
                    }
                    self.error = Some(message);
                    self.notice = None;
                }
            }
            Event::PoseResult {
                epoch,
                request_id,
                object_id,
                result,
            } => {
                if epoch == self.epoch
                    && let Some(edit) = self.poses.get_mut(&object_id)
                    && edit.epoch == epoch
                    && edit.request_id == request_id
                    && edit.generation == self.gesture_generation
                {
                    edit.state = match result {
                        Ok(revision) => {
                            self.notice = Some(format!(
                                "Поза принята: r{revision}. Поле и физическое время не сброшены."
                            ));
                            PoseState::Accepted
                        }
                        Err(message) => PoseState::Rejected(message),
                    };
                }
            }
            Event::FileSaved { path } => {
                self.notice = Some(format!("Записано: {}", path.display()))
            }
            Event::Applied { .. } => {}
        }
    }

    fn accept_snapshot(&mut self, snapshot: Arc<Snapshot>) {
        if snapshot.epoch < self.epoch {
            return;
        }
        if self.snapshot.as_ref().is_some_and(|last| {
            snapshot.revision < last.revision
                || snapshot.run < last.run
                || (snapshot.run == last.run
                    && snapshot.revision == last.revision
                    && snapshot.step < last.step)
        }) {
            return;
        }
        self.advance_epoch(snapshot.epoch);
        if self
            .snapshot
            .as_ref()
            .is_none_or(|last| !Arc::ptr_eq(&last.scene, &snapshot.scene))
        {
            self.active_scene = (*snapshot.scene).clone();
        }
        self.snapshot = Some(snapshot);
    }

    fn poll(&mut self) {
        for _ in 0..64 {
            let Some(event) = self.runtime.event() else {
                break;
            };
            self.accept_event(event);
        }
        match self.runtime.poll() {
            Ok(Some(snapshot)) => self.accept_snapshot(snapshot),
            Ok(None) => {}
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn live_available(&self, id: u64) -> bool {
        self.cold_pending.is_none()
            && self.snapshot.is_some()
            && self.active_scene.pose(id).is_some()
    }

    fn stage_pose(&mut self, id: u64, now_s: f64) {
        if !self.live_available(id) {
            self.error = Some("Live-перенос недоступен: дождитесь холодной команды; новый объект сначала нужно применить.".into());
            return;
        }
        let Some(pose) = self.scene.pose(id) else {
            return;
        };
        if self.active_scene.pose(id).is_none_or(|accepted| {
            std::mem::discriminant(&accepted) != std::mem::discriminant(&pose)
        }) {
            self.error = Some(
                "Тип объекта не совпадает с принятой сценой; требуется холодное применение.".into(),
            );
            return;
        }
        let unchanged = self
            .poses
            .get(&id)
            .is_some_and(|edit| edit.pose == pose && edit.epoch == self.epoch);
        if !unchanged {
            let Some(request_id) = self.reserve_request() else {
                return;
            };
            self.poses.insert(
                id,
                PoseEdit {
                    epoch: self.epoch,
                    generation: self.gesture_generation,
                    request_id,
                    pose,
                    state: PoseState::Queued,
                },
            );
        }
        self.flush_poses(now_s, None);
    }

    fn flush_poses(&mut self, now_s: f64, final_id: Option<u64>) {
        if self.cold_pending.is_some() || !now_s.is_finite() {
            return;
        }
        if final_id.is_none()
            && self
                .last_pose_send_s
                .is_some_and(|last| now_s - last < 1.0 / 30.0)
        {
            return;
        }
        let queued = self
            .poses
            .iter()
            .filter(|(id, edit)| {
                final_id.is_none_or(|wanted| wanted == **id)
                    && edit.state == PoseState::Queued
                    && edit.epoch == self.epoch
                    && edit.generation == self.gesture_generation
            })
            .min_by_key(|(_, edit)| edit.request_id)
            .map(|(&id, edit)| (id, edit.epoch, edit.request_id, edit.pose.clone()));
        let Some((id, epoch, request_id, pose)) = queued else {
            return;
        };
        let result = self.runtime.send(Command::SetPose {
            epoch,
            request_id,
            pose,
        });
        if let Some(edit) = self.poses.get_mut(&id) {
            edit.state = match result {
                Ok(()) => PoseState::Pending,
                Err(error) => PoseState::Rejected(error.to_string()),
            };
        }
        self.last_pose_send_s = Some(now_s);
    }

    fn edited(&mut self) {
        self.edit_epoch = self.edit_epoch.saturating_add(1);
        self.preview = None;
    }

    fn stale(&self) -> bool {
        self.scene != self.active_scene || self.cold_pending.is_some()
    }

    fn display_mode(&self) -> FieldMode {
        self.snapshot
            .as_ref()
            .map_or(self.display.mode, |s| self.display.effective_mode(s))
    }

    fn display_title(&self) -> &'static str {
        if self.display.mode == FieldMode::Rms && self.display_mode() == FieldMode::Ez {
            "Ez; RMS накапливается"
        } else {
            self.display.mode.title()
        }
    }

    fn add_at(&mut self, tool: Tool, point: Point) {
        if !self.can_add(tool) {
            return;
        }
        let id = self.scene.next_id();
        let cell = self.scene.domain.cell_m;
        match tool {
            Tool::Source | Tool::BentSource => {
                self.scene.sources.push(Source {
                    id,
                    name: format!(
                        "{} {id}",
                        if tool == Tool::BentSource {
                            "Г-волновод"
                        } else {
                            "Волновод"
                        }
                    ),
                    center: point,
                    angle_deg: if tool == Tool::BentSource { 180.0 } else { 0.0 },
                    bend: (tool == Tool::BentSource).then_some(GuideBend { length_m: 0.08 }),
                    ..Source::default()
                });
                self.selection = Some(Selection::Source(id));
            }
            Tool::Probe => {
                self.scene.probes.push(Probe {
                    id,
                    name: format!("Зонд {id}"),
                    position: point,
                });
                if let Some(slot) = self.probe_colors.iter_mut().find(|slot| slot.is_none()) {
                    *slot = Some(id);
                }
                self.selection = Some(Selection::Probe(id));
            }
            Tool::Segment | Tool::Rectangle | Tool::Circle => {
                let shape = match tool {
                    Tool::Segment => Shape::Segment {
                        length_m: 0.08,
                        thickness_m: 2.0 * cell,
                    },
                    Tool::Rectangle => Shape::Rectangle {
                        width_m: 0.05,
                        height_m: 0.04,
                    },
                    _ => Shape::Circle { radius_m: 0.025 },
                };
                self.scene.reflectors.push(Reflector {
                    id,
                    name: format!("Отражатель {id}"),
                    center: point,
                    angle_deg: 0.0,
                    shape,
                });
                self.selection = Some(Selection::Reflector(id));
            }
            Tool::Polyline | Tool::Select | Tool::Pan => return,
        }
        self.tool = Tool::Select;
        self.edited();
        self.apply_geometry();
    }

    fn delete_selected(&mut self) {
        let Some(selection) = self.selection.take() else {
            return;
        };
        match selection {
            Selection::Source(id) => self.scene.sources.retain(|s| s.id != id),
            Selection::Reflector(id) => self.scene.reflectors.retain(|s| s.id != id),
            Selection::Probe(id) => self.scene.probes.retain(|s| s.id != id),
        }
        // Освобождаем только исчезнувшие слоты: оставшиеся зонды сохраняют цвет.
        for slot in &mut self.probe_colors {
            if slot.is_some_and(|id| !self.scene.probes.iter().any(|probe| probe.id == id)) {
                *slot = None;
            }
        }
        self.table_drafts.retain(|&(id, kind), _| match kind {
            0..=2 => self.scene.sources.iter().any(|source| source.id == id),
            3 => self.scene.reflectors.iter().any(|reflector| {
                reflector.id == id && matches!(reflector.shape, Shape::Polyline { .. })
            }),
            _ => false,
        });
        self.edited();
        self.apply_geometry();
    }

    fn duplicate_selected(&mut self) {
        let tool = match self.selection {
            Some(Selection::Source(_)) => Tool::Source,
            Some(Selection::Reflector(_)) => Tool::Rectangle,
            Some(Selection::Probe(_)) => Tool::Probe,
            None => return,
        };
        if !self.can_add(tool) {
            return;
        }
        let id = self.scene.next_id();
        let shift = 8.0 * self.scene.domain.cell_m;
        match self.selection {
            Some(Selection::Source(old)) => {
                if let Some(mut object) = self.scene.sources.iter().find(|s| s.id == old).cloned() {
                    object.id = id;
                    object.name.push_str(" (копия)");
                    object.center.y += shift;
                    self.scene.sources.push(object);
                    self.selection = Some(Selection::Source(id));
                }
            }
            Some(Selection::Reflector(old)) => {
                if let Some(mut object) =
                    self.scene.reflectors.iter().find(|s| s.id == old).cloned()
                {
                    object.id = id;
                    object.name.push_str(" (копия)");
                    object.center.y += shift;
                    self.scene.reflectors.push(object);
                    self.selection = Some(Selection::Reflector(id));
                }
            }
            Some(Selection::Probe(old)) => {
                if let Some(mut object) = self.scene.probes.iter().find(|s| s.id == old).cloned() {
                    object.id = id;
                    object.name.push_str(" (копия)");
                    object.position.y += shift;
                    self.scene.probes.push(object);
                    self.selection = Some(Selection::Probe(id));
                }
            }
            None => return,
        }
        self.edited();
        self.apply_geometry();
    }

    fn file_command(&mut self, action: FileAction, path: PathBuf, overwrite: bool) {
        if path.as_os_str().is_empty() {
            self.error = Some("Путь не может быть пустым.".into());
            return;
        }
        let command = match action {
            FileAction::Save => Command::Save {
                path,
                scene: self.scene.clone(),
                overwrite,
            },
            FileAction::Export => Command::Export { path, overwrite },
        };
        self.send(command);
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.add_space(5.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("VOLNA").size(23.0).strong());
            ui.label(RichText::new("2D · TMz · ВАКУУМ / PEC").small().weak());
            ui.separator();
            let running = self.snapshot.as_ref().is_some_and(|s| s.running);
            if ui
                .button(if running {
                    "Пауза"
                } else {
                    "Запуск"
                })
                .clicked()
            {
                self.send(Command::Play(!running));
            }
            if ui
                .add_enabled(!running, egui::Button::new("Один шаг"))
                .clicked()
            {
                self.send(Command::Step);
            }
            if ui
                .add_enabled(self.cold_pending.is_none(), egui::Button::new("Сброс поля"))
                .clicked()
            {
                self.reset();
            }
            ui.label("Шагов/пакет");
            if ui
                .add(egui::DragValue::new(&mut self.speed).range(1..=128))
                .changed()
            {
                self.send(Command::Speed(self.speed));
            }
            ui.separator();
            if ui
                .button(if self.dark {
                    "Светлая тема"
                } else {
                    "Тёмная тема"
                })
                .clicked()
            {
                self.dark = !self.dark;
                palette::configure(ui.ctx(), self.dark);
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("JSON");
            ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(150.0).char_limit(4096));
            if ui.button("Открыть").on_hover_text("Невалидный файл не заменяет рабочую сцену. Корректная загрузка заменит черновик.").clicked() { self.load(); }
            if ui.button("Сохранить новый").clicked() { self.file_command(FileAction::Save, self.path.trim().into(), false); }
            if ui.small_button("Перезаписать…").clicked() {
                self.confirmation = Some(FileConfirmation { action: FileAction::Save, path: self.path.trim().into() });
            }
            ui.separator();
            ui.label("CSV");
            ui.add(egui::TextEdit::singleline(&mut self.csv_path).desired_width(125.0).char_limit(4096));
            if ui.button("Экспорт новый").clicked() { self.file_command(FileAction::Export, self.csv_path.trim().into(), false); }
            if ui.small_button("Заменить CSV…").clicked() {
                self.confirmation = Some(FileConfirmation { action: FileAction::Export, path: self.csv_path.trim().into() });
            }
        });
        let pose_error = self
            .poses
            .iter()
            .filter_map(|(&id, edit)| match &edit.state {
                PoseState::Rejected(message) => Some((edit.request_id, id, message)),
                _ => None,
            })
            .max_by_key(|(request_id, _, _)| *request_id)
            .map(|(_, id, message)| (id, message));
        if let Some(error) = &self.error {
            stable_line(
                ui,
                RichText::new(
                    "ОШИБКА — последняя корректная сцена сохранена · подробности при наведении",
                )
                .strong(),
            )
            .on_hover_text(error);
        } else if let Some((id, message)) = pose_error {
            let name = self
                .active_scene
                .sources
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.name.as_str())
                .or_else(|| {
                    self.active_scene
                        .reflectors
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.name.as_str())
                })
                .or_else(|| {
                    self.active_scene
                        .probes
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| p.name.as_str())
                })
                .unwrap_or("Объект");
            stable_line(ui, RichText::new(format!("Не применено положение «{name}» (ID {id})")).strong())
                .on_hover_text(format!("{message}\nПринятое положение этого объекта сохранено; пунктир показывает черновик."));
        } else {
            stable_line(
                ui,
                RichText::new(
                    self.notice
                        .as_deref()
                        .unwrap_or("Готово. Координаты и угол применяются live."),
                )
                .small()
                .weak(),
            );
        }
        ui.add_space(3.0);
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for (tool, title) in [
                (Tool::Select, "Выбор"),
                (Tool::Pan, "Камера"),
                (Tool::Source, "+ Волновод"),
                (Tool::BentSource, "+ Г-волновод"),
                (Tool::Segment, "+ Отрезок"),
                (Tool::Rectangle, "+ Прямоугольник"),
                (Tool::Circle, "+ Круг"),
                (Tool::Polyline, "+ Ломаная"),
                (Tool::Probe, "+ Зонд"),
            ] {
                if ui.selectable_label(self.tool == tool, title).clicked() {
                    self.tool = tool;
                    self.canvas.cancel_gesture();
                }
            }
            if ui.small_button("Вписать").clicked() {
                self.canvas.reset_view();
            }
        });
        ui.horizontal_wrapped(|ui| {
            for mode in [FieldMode::Ez, FieldMode::Absolute, FieldMode::Rms] {
                ui.selectable_value(&mut self.display.mode, mode, mode.title());
            }
            ui.checkbox(&mut self.display.auto_range, "Автошкала");
            if !self.display.auto_range {
                ui.add(egui::DragValue::new(&mut self.display.range).speed(0.01).range(1e-12..=1e12).suffix(" В/м"));
            }
            ui.checkbox(&mut self.display.crop_pml, "Скрыть CPML");
            ui.checkbox(&mut self.display.show_mask, "PEC-ячейки");
            ui.checkbox(&mut self.display.show_grid, "Сетка осей");
            ui.checkbox(&mut self.display.texture_sign, "Штрихи ±").on_hover_text("Наклон штриха показывает знак Ez: / положительный, \\ отрицательный. Цвет не единственный канал.");
        });
        stable_line(ui, RichText::new(if self.stale() {
            "Поле и сплошные контуры — принятая сцена; пунктир — неприменённый черновик. Остальные настройки требуют применения."
        } else { "Принятая сцена. Координаты и угол — live; размеры, сигналы и сетка — с холодным применением." }).small());
    }

    fn status(&self, ui: &mut egui::Ui) {
        let text = if let Some(s) = &self.snapshot {
            format!(
                "e{} / r{} · run {} · t = {:.5} нс · шаг {} · {:.0} шаг/с · {:.1} кадров/с · {} × {} · Δt = {:.3} пс{}",
                s.epoch,
                s.revision,
                s.run,
                s.time_s * 1e9,
                s.step,
                s.steps_per_second,
                s.frame_rate,
                s.grid.nx,
                s.grid.ny,
                s.grid.dt * 1e12,
                if s.inherited_fields {
                    " · Унаследованное поле: переходный процесс"
                } else {
                    ""
                }
            )
        } else {
            "Подготовка сетки и первого снимка…".into()
        };
        stable_line(ui, RichText::new(text).monospace());
    }

    fn confirm_overwrite(&mut self, ctx: &egui::Context) {
        let Some(confirmation) = &self.confirmation else {
            return;
        };
        let path = confirmation.path.clone();
        let mut accept = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("overwrite_confirmation")).show(ctx, |ui| {
            ui.heading("Подтвердите перезапись");
            ui.label(
                "Содержимое существующего файла будет заменено. Это действие нельзя отменить.",
            );
            ui.monospace(path.display().to_string());
            ui.horizontal(|ui| {
                accept = ui.button("Перезаписать файл").clicked();
                cancel = ui.button("Отмена").clicked();
            });
        });
        if accept {
            if let Some(confirmation) = self.confirmation.take() {
                self.file_command(confirmation.action, confirmation.path, true);
            }
        } else if cancel {
            self.confirmation = None;
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("header").show(ui, |ui| self.header(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status(ui));
        egui::Panel::right("inspector")
            .default_size(310.0)
            .min_size(275.0)
            .max_size(410.0)
            .resizable(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.inspector(ui));
            });
        egui::Panel::bottom("probes")
            .default_size(215.0)
            .min_size(130.0)
            .max_size(380.0)
            .resizable(true)
            .show(ui, |ui| self.probe_plots(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            self.toolbar(ui);
            ui.separator();
            self.canvas_ui(ui);
        });
        self.flush_poses(ui.input(|i| i.time), None);
        self.confirm_overwrite(ui.ctx());
    }
}

impl eframe::App for VolnaApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        ctx.request_repaint_after(Duration::from_millis(33));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn wait_until(
        app: &mut VolnaApp,
        predicate: impl Fn(&VolnaApp) -> bool,
    ) -> crate::Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            app.poll();
            if predicate(app) {
                return Ok(());
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker не завершил ожидаемое действие: {:?}",
                app.error
            );
            if let Some(snapshot) = app.runtime.wait_snapshot(Duration::from_millis(20))? {
                app.accept_snapshot(snapshot);
            }
        }
    }

    pub(super) fn app_with_scene(scene: Scene) -> crate::Result<VolnaApp> {
        let runtime = Runtime::new(scene.clone())?;
        runtime.send(Command::Play(false))?;
        let mut app = VolnaApp::from_runtime(runtime, scene);
        wait_until(&mut app, |a| {
            a.cold_pending.is_none() && a.snapshot.as_ref().is_some_and(|s| !s.running)
        })?;
        Ok(app)
    }

    fn app() -> crate::Result<VolnaApp> {
        app_with_scene(Scene::default())
    }

    #[test]
    fn launch_existing_constructor_keeps_user_visible_defaults() -> crate::Result<()> {
        let cc = eframe::CreationContext::_new_kittest(egui::Context::default());
        let app = VolnaApp::new(&cc)?;
        assert_eq!(app.scene, Scene::interference_demo());
        assert_eq!(app.active_scene, Scene::interference_demo());
        assert_eq!(app.display_title(), "Ez");
        assert_eq!(app.path, "presets/interference_10ghz.json");
        assert_eq!(app.selection, Some(Selection::Source(1)));
        assert_eq!(app.speed, crate::runtime::DEFAULT_STEPS_PER_BATCH);
        assert!(app.dark);
        assert!(app.display.auto_range && app.display.crop_pml && app.display.show_grid);
        assert!(!app.display.show_mask && !app.display.texture_sign);
        Ok(())
    }

    #[test]
    fn launch_options_reach_first_snapshot_and_display() -> crate::Result<()> {
        let cc = eframe::CreationContext::_new_kittest(egui::Context::default());
        for (view, title) in [
            (FieldMode::Ez, "Ez"),
            (FieldMode::Absolute, "|Ez|"),
            (FieldMode::Rms, "RMS(Ez)"),
        ] {
            let mut scene = Scene::default();
            scene.sources[0].id = 42;
            let mut app = VolnaApp::new_with_options(
                &cc,
                StartupOptions {
                    scene: scene.clone(),
                    view,
                    scene_path: "моя сцена.json".into(),
                },
            )?;
            assert_eq!(app.scene, scene);
            assert_eq!(app.active_scene, scene);
            assert_eq!(app.path, "моя сцена.json");
            assert_eq!(app.selection, Some(Selection::Source(42)));
            assert_eq!(app.display_title(), title);
            wait_until(&mut app, |a| {
                a.cold_pending.is_none() && a.snapshot.is_some()
            })?;
            assert_eq!(*app.snapshot.as_ref().unwrap().scene, scene);
            assert_eq!(app.display_title(), title);
            assert!(app.error.is_none());
        }
        Ok(())
    }

    #[test]
    fn launch_scene_without_sources_has_no_phantom_selection() -> crate::Result<()> {
        let cc = eframe::CreationContext::_new_kittest(egui::Context::default());
        let mut options = StartupOptions::default();
        options.scene.sources.clear();
        let app = VolnaApp::new_with_options(&cc, options)?;
        assert_eq!(app.selection, None);
        Ok(())
    }

    #[test]
    fn initial_ui_speed_matches_worker_default() -> crate::Result<()> {
        let app = app_with_scene(Scene::interference_demo())?;
        assert_eq!(app.speed, crate::runtime::DEFAULT_STEPS_PER_BATCH);
        assert_eq!(app.active_scene, Scene::interference_demo());
        Ok(())
    }

    #[test]
    fn deleting_probe_reuses_only_its_color_slot_for_new_probe() -> crate::Result<()> {
        let mut app = app()?;
        app.add_at(Tool::Probe, Point { x: 0.26, y: 0.1 });
        app.add_at(Tool::Probe, Point { x: 0.3, y: 0.1 });
        let before = app.probe_colors;
        let removed = app.scene.probes[1].id;
        app.selection = Some(Selection::Probe(removed));
        app.delete_selected();
        assert_eq!(app.probe_colors, [before[0], None, before[2]]);
        let added = app.scene.next_id();
        app.add_at(Tool::Probe, Point { x: 0.28, y: 0.2 });
        assert_eq!(app.probe_colors, [before[0], Some(added), before[2]]);
        assert!(app.scene.probes.iter().any(|probe| probe.id == added));
        Ok(())
    }

    #[test]
    fn deleting_objects_prunes_their_drafts_but_keeps_live_source_and_polyline_drafts()
    -> crate::Result<()> {
        let mut app = app()?;
        app.scene.sources.push(Source {
            id: 3,
            ..Source::default()
        });
        app.scene.reflectors.push(Reflector {
            id: 4,
            name: "Ломаная".into(),
            center: Point { x: 0.3, y: 0.2 },
            angle_deg: 0.0,
            shape: Shape::Polyline {
                points: vec![Point { x: 0.0, y: 0.0 }, Point { x: 0.02, y: 0.0 }],
                thickness_m: 0.002,
            },
        });
        for key in [(1, 0), (1, 1), (1, 2), (3, 1), (4, 3), (99, 0)] {
            app.table_drafts.insert(key, TableDraft::default());
        }
        app.selection = Some(Selection::Source(1));
        app.delete_selected();
        assert_eq!(
            app.table_drafts.keys().copied().collect::<Vec<_>>(),
            vec![(3, 1), (4, 3)]
        );
        app.selection = Some(Selection::Reflector(4));
        app.delete_selected();
        assert_eq!(
            app.table_drafts.keys().copied().collect::<Vec<_>>(),
            vec![(3, 1)]
        );
        Ok(())
    }

    #[test]
    fn stale_applied_events_never_replace_active_or_editable_scene() -> crate::Result<()> {
        let mut app = app()?;
        let original = app.scene.clone();
        app.cold_pending = Some(ColdPending {
            request_id: 4,
            kind: ColdKind::Apply,
            edit_epoch: 0,
            start_epoch: 1,
        });
        let mut obsolete = original.clone();
        obsolete.sources.clear();
        app.accept_event(Event::Applied {
            epoch: 2,
            request_id: 3,
            revision: 2,
            scene: Box::new(obsolete),
        });
        assert_eq!(app.active_scene, original);
        assert_eq!(app.scene, original);
        app.accept_event(Event::Error {
            epoch: 2,
            request_id: 3,
            message: "устаревшая ошибка".into(),
        });
        assert!(app.error.is_none());
        Ok(())
    }

    #[test]
    fn load_response_cannot_erase_edits_made_while_loading() -> crate::Result<()> {
        let mut app = app()?;
        app.cold_pending = Some(ColdPending {
            request_id: 2,
            kind: ColdKind::Load,
            edit_epoch: app.edit_epoch,
            start_epoch: 1,
        });
        app.scene.sources[0].name = "Новая правка".into();
        app.edited();
        let loaded = Scene::default();
        app.accept_event(Event::Applied {
            epoch: 2,
            request_id: 2,
            revision: 2,
            scene: Box::new(loaded.clone()),
        });
        assert_eq!(app.active_scene, loaded);
        assert_eq!(app.scene.sources[0].name, "Новая правка");
        Ok(())
    }

    #[test]
    fn rejected_apply_preserves_last_good_scene_and_editable_draft() -> crate::Result<()> {
        let mut app = app()?;
        let active = app.active_scene.clone();
        app.scene.domain.cell_m = 0.0;
        app.cold_pending = Some(ColdPending {
            request_id: 9,
            kind: ColdKind::Apply,
            edit_epoch: 0,
            start_epoch: 1,
        });
        app.accept_event(Event::Error {
            epoch: 2,
            request_id: 9,
            message: "недопустимая сетка".into(),
        });
        assert_eq!(app.active_scene, active);
        assert_eq!(app.scene.domain.cell_m, 0.0);
        assert!(app.cold_pending.is_none());
        assert!(app.error.is_some());
        Ok(())
    }

    #[test]
    fn adding_geometry_does_not_implicitly_apply_pending_domain_edits() -> crate::Result<()> {
        let mut app = app()?;
        app.scene.domain.width_m = 0.5;
        let request = app.next_request_id;
        app.apply_geometry();
        assert_eq!(app.next_request_id, request);
        assert!(app.cold_pending.is_none());
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|n| n.contains("только кнопкой"))
        );
        Ok(())
    }

    #[test]
    fn live_pose_matrix_preserves_paused_run_clock_and_draft_only_settings() -> crate::Result<()> {
        let mut scenes = vec![Scene::default()];
        let mut bent = Scene::default();
        bent.sources[0].bend = Some(GuideBend { length_m: 0.08 });
        scenes.push(bent);
        let shapes = [
            Shape::Segment {
                length_m: 0.04,
                thickness_m: 0.002,
            },
            Shape::Rectangle {
                width_m: 0.04,
                height_m: 0.03,
            },
            Shape::Circle { radius_m: 0.02 },
            Shape::Polyline {
                points: vec![Point { x: -0.02, y: 0.0 }, Point { x: 0.02, y: 0.01 }],
                thickness_m: 0.002,
            },
        ];
        for shape in shapes {
            let mut scene = Scene::default();
            scene.reflectors.push(Reflector {
                id: 3,
                name: "Проверка".into(),
                center: Point { x: 0.3, y: 0.2 },
                angle_deg: 0.0,
                shape,
            });
            scenes.push(scene);
        }
        scenes.push(Scene::default());
        for (case, scene) in scenes.into_iter().enumerate() {
            let mut app = app_with_scene(scene)?;
            // Ненулевое время не позволяет принять скрытый reset за сохранение паузы.
            let step = app.snapshot.as_ref().unwrap().step;
            app.runtime.send(Command::Step)?;
            wait_until(&mut app, |a| a.snapshot.as_ref().unwrap().step > step)?;
            let before = app.snapshot.as_ref().unwrap().clone();
            let id = if case < 2 {
                1
            } else if case < 6 {
                3
            } else {
                2
            };
            match id {
                1 => {
                    app.scene.sources[0].center.x += 0.007;
                    app.scene.sources[0].angle_deg = 12.0;
                }
                3 => {
                    app.scene.reflectors[0].center.x += 0.007;
                    app.scene.reflectors[0].angle_deg = 12.0;
                }
                _ => app.scene.probes[0].position.y += 0.007,
            }
            let pose = app.scene.pose(id).unwrap();
            let expected = before.scene.with_pose(&pose)?;
            // Ни плохая формула, ни неприменённые размеры/сетка не входят в ObjectPose.
            app.scene.domain.cell_m = 0.0;
            app.scene.sources[0].width_m = 0.07;
            app.scene.sources[0].bend = Some(GuideBend { length_m: 0.12 });
            app.scene.sources[0].signal = crate::model::Signal::Formula {
                expression: "bad(".into(),
            };
            let draft = app.scene.clone();
            app.edited();
            app.stage_pose(id, 1.0);
            wait_until(&mut app, |a| {
                a.active_scene.pose(id) == Some(pose.clone())
                    && a.poses[&id].state == PoseState::Accepted
            })?;
            let after = app.snapshot.as_ref().unwrap();
            assert_eq!(*after.scene, expected, "case={case}");
            assert_eq!(app.scene, draft, "ack не должен переписывать черновик");
            assert_eq!(
                (after.run, after.step, after.time_s, after.running),
                (before.run, before.step, before.time_s, false)
            );
            assert!(after.revision > before.revision);
            assert_eq!(after.epoch, before.epoch);
            if id == 2 {
                assert_eq!(after.ez, before.ez);
                assert_eq!(after.rms, before.rms);
                assert_eq!(after.mask, before.mask);
            } else {
                assert!(after.inherited_fields);
                assert!(after.rms.is_none());
                assert_eq!(after.history_start_step, before.step);
            }
        }
        Ok(())
    }

    #[test]
    fn old_ack_cannot_clear_latest_pose_or_replace_draft() -> crate::Result<()> {
        let mut app = app()?;
        app.scene.sources[0].center.x += 0.005;
        app.stage_pose(1, 1.0);
        let old_request = app.poses[&1].request_id;
        app.scene.sources[0].center.x += 0.005;
        app.stage_pose(1, 1.001);
        let latest_request = app.poses[&1].request_id;
        let draft = app.scene.clone();
        assert_eq!(app.poses[&1].state, PoseState::Queued);
        for (epoch, request_id, object_id) in [
            (app.epoch, old_request, 1),
            (app.epoch + 1, latest_request, 1),
            (app.epoch, latest_request, 2),
        ] {
            app.accept_event(Event::PoseResult {
                epoch,
                request_id,
                object_id,
                result: Ok(99),
            });
            assert_eq!(app.poses[&1].state, PoseState::Queued);
        }
        app.accept_event(Event::PoseResult {
            epoch: app.epoch,
            request_id: old_request,
            object_id: 1,
            result: Err("старый отказ".into()),
        });
        assert_eq!(app.poses[&1].state, PoseState::Queued);
        assert_eq!(app.scene, draft);
        app.flush_poses(1.002, Some(1));
        assert_eq!(
            app.poses[&1].state,
            PoseState::Pending,
            "release обходит throttle"
        );
        wait_until(&mut app, |a| {
            a.active_scene.sources[0].center == draft.sources[0].center
                && a.poses[&1].state == PoseState::Accepted
        })?;
        assert_eq!(app.scene, draft);
        Ok(())
    }

    #[test]
    fn invalid_pose_then_valid_pose_recovers_without_reset() -> crate::Result<()> {
        let mut app = app()?;
        let before = app.snapshot.as_ref().unwrap().clone();
        app.scene.sources[0].center.x = -1.0;
        app.stage_pose(1, 1.0);
        wait_until(&mut app, |a| {
            matches!(a.poses[&1].state, PoseState::Rejected(_))
        })?;
        assert_eq!(app.active_scene, *before.scene);
        assert_eq!(app.scene.sources[0].center.x, -1.0);
        app.scene.sources[0].center.x = 0.09;
        app.stage_pose(1, 2.0);
        wait_until(&mut app, |a| {
            a.active_scene.sources[0].center.x == 0.09 && a.poses[&1].state == PoseState::Accepted
        })?;
        let after = app.snapshot.as_ref().unwrap();
        assert_eq!(
            (after.run, after.step, after.time_s, after.running),
            (before.run, before.step, before.time_s, false)
        );
        Ok(())
    }

    #[test]
    fn rejected_pose_banner_names_a_after_successful_pose_b() -> crate::Result<()> {
        let mut app = app()?;
        let name_a = app.active_scene.sources[0].name.clone();
        app.scene.sources[0].center.x = -1.0;
        app.stage_pose(1, 1.0);
        wait_until(&mut app, |a| {
            matches!(a.poses[&1].state, PoseState::Rejected(_))
        })?;
        app.scene.probes[0].position.y += 0.01;
        let pose_b = app.scene.pose(2);
        app.stage_pose(2, 2.0);
        wait_until(&mut app, |a| {
            a.poses[&2].state == PoseState::Accepted && a.active_scene.pose(2) == pose_b
        })?;
        assert!(matches!(app.poses[&1].state, PoseState::Rejected(_)));
        assert_eq!(app.scene.sources[0].center.x, -1.0);
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| app.header(ui));
        output.textures_delta.clear();
        fn text_contains(shape: &egui::Shape, needle: &str) -> bool {
            match shape {
                egui::Shape::Text(text) => text.galley.job.text.contains(needle),
                egui::Shape::Vec(shapes) => shapes.iter().any(|s| text_contains(s, needle)),
                _ => false,
            }
        }
        let title = format!("Не применено положение «{name_a}» (ID 1)");
        assert!(
            output
                .shapes
                .iter()
                .any(|s| text_contains(&s.shape, &title)),
            "видимый баннер должен указывать отклонённый A, не успешно перемещённый B"
        );
        assert!(
            !output
                .shapes
                .iter()
                .any(|s| text_contains(&s.shape, "ОШИБКА — последняя корректная сцена сохранена"))
        );
        Ok(())
    }

    #[test]
    fn rejected_cold_advances_epoch_but_old_scene_frames_and_live_edits_continue()
    -> crate::Result<()> {
        let mut app = app()?;
        let before = app.snapshot.as_ref().unwrap().clone();
        app.scene.domain.cell_m = 0.0;
        app.apply();
        wait_until(&mut app, |a| {
            a.cold_pending.is_none() && a.snapshot.as_ref().unwrap().epoch > before.epoch
        })?;
        assert!(app.error.is_some());
        assert_eq!(app.active_scene, *before.scene);
        assert_eq!(app.snapshot.as_ref().unwrap().revision, before.revision);
        assert_eq!(app.snapshot.as_ref().unwrap().run, before.run);
        app.runtime.send(Command::Play(true))?;
        wait_until(&mut app, |a| {
            a.snapshot.as_ref().unwrap().step > before.step
        })?;
        assert_eq!(app.active_scene, *before.scene);
        app.runtime.send(Command::Play(false))?;
        wait_until(&mut app, |a| !a.snapshot.as_ref().unwrap().running)?;
        app.scene.sources[0].center.x += 0.006;
        let pose = app.scene.pose(1).unwrap();
        app.stage_pose(1, 2.0);
        wait_until(&mut app, |a| a.active_scene.pose(1) == Some(pose.clone()))?;
        assert_eq!(app.scene.domain.cell_m, 0.0);
        assert_eq!(app.active_scene.domain, before.scene.domain);
        Ok(())
    }

    #[test]
    fn snapshots_follow_accepted_revision_not_latest_request_and_do_not_rewrite_draft()
    -> crate::Result<()> {
        let mut app = app()?;
        app.next_request_id = 500;
        app.scene.sources[0].center.x = 0.1;
        let draft = app.scene.clone();
        let original = app.snapshot.as_ref().unwrap().clone();
        let mut accepted = (*original).clone();
        accepted.revision += 1;
        accepted.scene = Arc::new(original.scene.with_pose(&ObjectPose::Source {
            id: 1,
            center: Point { x: 0.09, y: 0.15 },
            angle_deg: 0.0,
        })?);
        app.accept_snapshot(Arc::new(accepted.clone()));
        assert_eq!(app.active_scene.sources[0].center.x, 0.09);
        assert_eq!(app.scene, draft);
        app.accept_snapshot(original.clone());
        assert_eq!(app.snapshot.as_ref().unwrap().revision, accepted.revision);
        app.advance_epoch(accepted.epoch + 1);
        accepted.step += 1;
        app.accept_snapshot(Arc::new(accepted));
        assert_eq!(
            app.snapshot.as_ref().unwrap().step,
            original.step,
            "старый epoch игнорируется"
        );
        Ok(())
    }

    #[test]
    fn unknown_unapplied_object_is_not_sent_as_live_pose() -> crate::Result<()> {
        let mut app = app()?;
        app.scene.probes.push(Probe {
            id: 90,
            name: "Новый".into(),
            position: Point { x: 0.3, y: 0.1 },
        });
        let request = app.next_request_id;
        app.stage_pose(90, 1.0);
        assert!(
            app.error
                .as_ref()
                .is_some_and(|message| message.contains("сначала"))
        );
        assert_eq!(app.next_request_id, request);
        assert!(app.poses.is_empty());
        Ok(())
    }

    #[test]
    fn headless_ui_renders_both_themes_with_inspector_and_real_snapshot() -> crate::Result<()> {
        let mut app = app()?;
        assert!(app.snapshot.is_some(), "worker не подготовил первый снимок");
        let ctx = egui::Context::default();
        for dark in [true, false] {
            app.dark = dark;
            palette::configure(&ctx, dark);
            for size in [[1400.0, 900.0], [1000.0, 700.0]] {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(size[0], size[1]),
                    )),
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |ui| app.show(ui));
                assert!(!output.shapes.is_empty());
                // Headless-тест не имеет GPU backend: дельты намеренно не загружаются.
                output.textures_delta.clear();
            }
        }
        Ok(())
    }
}
