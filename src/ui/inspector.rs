use super::{Selection, VolnaApp};
use crate::model::{C0, GuideBend, Point, Profile, Shape, Signal, Source};
use eframe::egui::{self, RichText};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct TableDraft {
    text: String,
    error: Option<String>,
}

/// Таблица принимается целиком: частичный разбор не меняет последний профиль.
fn parse_pairs(text: &str, ordered: bool) -> Result<Vec<[f64; 2]>, String> {
    if text.len() > 256 * 1024 {
        return Err("Текст таблицы превышает 256 KiB.".into());
    }
    let mut rows = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if rows.len() >= 4096 {
            return Err("Не более 4096 точек в одной таблице.".into());
        }
        let mut words = line.split_whitespace();
        let mut next = || words.next().and_then(|s| s.parse::<f64>().ok());
        let (Some(x), Some(y)) = (next(), next()) else {
            return Err(format!(
                "Строка {}: нужны два числа через пробел.",
                line_number + 1
            ));
        };
        if words.next().is_some() || !x.is_finite() || !y.is_finite() {
            return Err(format!(
                "Строка {}: нужны ровно два конечных числа.",
                line_number + 1
            ));
        }
        if ordered && rows.last().is_some_and(|p: &[f64; 2]| p[0] >= x) {
            return Err(format!(
                "Строка {}: координаты должны строго возрастать.",
                line_number + 1
            ));
        }
        rows.push([x, y]);
    }
    if rows.len() < 2 {
        return Err("Нужно не менее двух точек.".into());
    }
    Ok(rows)
}

fn number(ui: &mut egui::Ui, label: &str, value: &mut f64, scale: f64, speed: f64) -> bool {
    let mut display = *value * scale;
    let changed = ui
        .horizontal(|ui| {
            ui.label(label);
            ui.add(
                egui::DragValue::new(&mut display)
                    .speed(speed)
                    .max_decimals(7),
            )
            .changed()
        })
        .inner;
    if changed {
        *value = display / scale;
    }
    changed
}

fn point_editor(ui: &mut egui::Ui, point: &mut Point) -> bool {
    number(ui, "x, мм", &mut point.x, 1e3, 0.2) | number(ui, "y, мм", &mut point.y, 1e3, 0.2)
}

fn pair_editor(
    ui: &mut egui::Ui,
    drafts: &mut BTreeMap<(u64, u8), TableDraft>,
    key: (u64, u8),
    samples: &mut Vec<[f64; 2]>,
    ordered: bool,
) -> bool {
    let draft = drafts.entry(key).or_insert_with(|| TableDraft {
        text: samples
            .iter()
            .map(|p| format!("{} {}", p[0], p[1]))
            .collect::<Vec<_>>()
            .join("\n"),
        error: None,
    });
    ui.label(
        RichText::new("Две колонки через пробел; десятичная точка. Изменения текста — черновик.")
            .small()
            .weak(),
    );
    ui.add(
        egui::TextEdit::multiline(&mut draft.text)
            .font(egui::TextStyle::Monospace)
            .desired_rows(5)
            .desired_width(f32::INFINITY)
            .char_limit(256 * 1024),
    );
    let mut changed = false;
    if ui.button("Принять точки в черновик сцены").clicked() {
        match parse_pairs(&draft.text, ordered) {
            Ok(parsed) => {
                *samples = parsed;
                draft.error = None;
                changed = true;
            }
            Err(error) => draft.error = Some(error),
        }
    }
    if let Some(error) = &draft.error {
        ui.label(format!("Ошибка таблицы: {error}"));
    }
    changed
}

fn profile_editor(
    ui: &mut egui::Ui,
    label: &str,
    profile: &mut Profile,
    drafts: &mut BTreeMap<(u64, u8), TableDraft>,
    key: (u64, u8),
) -> bool {
    let old_kind = match profile {
        Profile::Uniform => 0,
        Profile::Fundamental => 1,
        Profile::Gaussian { .. } => 2,
        Profile::Formula { .. } => 3,
        Profile::Table { .. } => 4,
    };
    let mut kind = old_kind;
    let labels = [
        "Однородный",
        "Основная мода",
        "Gaussian",
        "Формула",
        "Таблица",
    ];
    egui::ComboBox::from_id_salt(key)
        .selected_text(labels[kind])
        .show_ui(ui, |ui| {
            for (index, title) in labels.iter().enumerate() {
                ui.selectable_value(&mut kind, index, *title);
            }
        });
    let mut changed = kind != old_kind;
    if changed {
        *profile = match kind {
            0 => Profile::Uniform,
            1 => Profile::Fundamental,
            2 => Profile::Gaussian { width: 0.5 },
            3 => Profile::Formula {
                expression: if key.1 == 2 { "0" } else { "cos(pi*s/2)" }.into(),
            },
            _ => Profile::Table {
                samples: vec![[-1.0, 0.0], [0.0, 1.0], [1.0, 0.0]],
            },
        };
        drafts.remove(&key);
    }
    match profile {
        Profile::Gaussian { width } => changed |= number(ui, "Ширина по s", width, 1.0, 0.01),
        Profile::Formula { expression } => {
            changed |= ui
                .add(
                    egui::TextEdit::multiline(expression)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .char_limit(2048),
                )
                .changed();
            ui.label(
                RichText::new("t [с], s ∈ [−1,1], f [Гц], phi [рад], pi; только арифметика.")
                    .small()
                    .weak(),
            );
        }
        Profile::Table { samples } => {
            ui.label(format!("s, {label}; вне таблицы — 0"));
            changed |= pair_editor(ui, drafts, key, samples, true);
        }
        Profile::Uniform | Profile::Fundamental => {}
    }
    changed
}

fn signal_editor(
    ui: &mut egui::Ui,
    source: &mut Source,
    drafts: &mut BTreeMap<(u64, u8), TableDraft>,
) -> bool {
    let old_kind = match source.signal {
        Signal::Continuous { .. } => 0,
        Signal::Gaussian { .. } => 1,
        Signal::Formula { .. } => 2,
        Signal::Table { .. } => 3,
    };
    let mut kind = old_kind;
    let labels = [
        "CW · плавный старт",
        "Gaussian · импульс",
        "Временная формула",
        "Таблица времени",
    ];
    egui::ComboBox::from_id_salt("signal_kind")
        .selected_text(labels[kind])
        .show_ui(ui, |ui| {
            for (index, title) in labels.iter().enumerate() {
                ui.selectable_value(&mut kind, index, *title);
            }
        });
    let mut changed = kind != old_kind;
    if changed {
        source.signal = match kind {
            0 => Signal::Continuous { ramp_periods: 4.0 },
            1 => Signal::Gaussian {
                center_s: 0.5e-9,
                width_s: 0.15e-9,
            },
            2 => Signal::Formula {
                expression: "sin(2*pi*f*t + phi)".into(),
            },
            _ => Signal::Table {
                samples: vec![[0.0, 0.0], [1e-10, 1.0], [2e-10, 0.0]],
                periodic: false,
            },
        };
        drafts.remove(&(source.id, 0));
    }
    match &mut source.signal {
        Signal::Continuous { ramp_periods } => {
            changed |= number(ui, "Плавный старт, периодов", ramp_periods, 1.0, 0.1)
        }
        Signal::Gaussian { center_s, width_s } => {
            changed |= number(ui, "Центр, нс", center_s, 1e9, 0.01);
            changed |= number(ui, "Ширина, нс", width_s, 1e9, 0.01);
        }
        Signal::Formula { expression } => {
            changed |= ui
                .add(
                    egui::TextEdit::multiline(expression)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .char_limit(2048),
                )
                .changed();
            ui.label(RichText::new("Безразмерный сигнал q: t [с], s, f [Гц], phi [рад], pi. f_max — ваша оценка полосы.").small().weak());
        }
        Signal::Table { samples, periodic } => {
            changed |= ui
                .checkbox(periodic, "Периодически (проверять стык)")
                .changed();
            ui.label("t [с], q [безразмерный]; непериодическое продолжение — 0");
            changed |= pair_editor(ui, drafts, (source.id, 0), samples, true);
        }
    }
    changed
}

fn source_editor(
    ui: &mut egui::Ui,
    source: &mut Source,
    drafts: &mut BTreeMap<(u64, u8), TableDraft>,
    pose_enabled: bool,
) -> (bool, bool) {
    let mut changed = ui
        .checkbox(&mut source.enabled, "Источник включён")
        .changed();
    ui.label(
        RichText::new("Отключение тока не удаляет PEC-стенки.")
            .small()
            .weak(),
    );
    changed |= ui
        .add(egui::TextEdit::singleline(&mut source.name).char_limit(120))
        .changed();
    let pose_changed = ui
        .add_enabled_ui(pose_enabled, |ui| {
            point_editor(ui, &mut source.center)
                | number(ui, "Поворот, °", &mut source.angle_deg, 1.0, 0.5)
        })
        .inner;
    ui.collapsing(
        "Волновод и область возбуждения",
        |ui| {
            let was_bent = source.bend.is_some();
            let mut bent = was_bent;
            egui::ComboBox::from_id_salt("guide_geometry").selected_text(if bent { "Г-образный · 90°" } else { "Прямой" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut bent, false, "Прямой");
                ui.selectable_value(&mut bent, true, "Г-образный · 90°");
            });
            if bent != was_bent {
                source.bend = bent.then_some(GuideBend { length_m: 0.08 });
                changed = true;
            }
            changed |= number(ui, "Ширина канала, мм", &mut source.width_m, 1e3, 0.2);
            changed |= number(ui, if bent { "Первое плечо, мм" } else { "Длина, мм" }, &mut source.length_m, 1e3, 0.5);
            if let Some(bend) = &mut source.bend {
                changed |= number(ui, "Второе плечо, мм", &mut bend.length_m, 1e3, 0.5);
                ui.label(RichText::new("Поворот канала на +90° в локальных координатах. Тип и длины применяются с холодным сбросом.").small().weak());
            }
            changed |= number(ui, "Стенки, мм", &mut source.wall_m, 1e3, 0.1);
            changed |= number(
                ui,
                "Ток от заднего края, мм",
                &mut source.source_offset_m,
                1e3,
                0.1,
            );
            changed |= number(ui, "Толщина полосы Jz, мм", &mut source.strip_m, 1e3, 0.1);
            changed |= ui
                .checkbox(&mut source.closed_back, "Задняя PEC-стенка")
                .changed();
        },
    );
    ui.separator();
    changed |= number(ui, "J₀, А/м²", &mut source.current_a_m2, 1.0, 0.05);
    changed |= number(ui, "f₀, ГГц", &mut source.frequency_hz, 1e-9, 0.05);
    changed |= number(ui, "f_max, ГГц", &mut source.fmax_hz, 1e-9, 0.05);
    changed |= number(
        ui,
        "Фаза, °",
        &mut source.phase_rad,
        180.0 / std::f64::consts::PI,
        1.0,
    );
    changed |= number(ui, "Задержка, нс", &mut source.delay_s, 1e9, 0.01);
    ui.label(
        RichText::new("J₀ — ток, не амплитуда Ez и не мощность.")
            .small()
            .weak(),
    );
    ui.collapsing("Временной сигнал", |ui| {
        changed |= signal_editor(ui, source, drafts);
    });
    ui.collapsing("Амплитуда по апертуре", |ui| {
        changed |= profile_editor(
            ui,
            "множитель амплитуды",
            &mut source.aperture,
            drafts,
            (source.id, 1),
        );
    });
    ui.collapsing("Фаза по апертуре", |ui| {
        ui.label("Добавка фазы в радианах; нулевая формула — синфазная апертура.");
        changed |= profile_editor(
            ui,
            "фаза [рад]",
            &mut source.phase_profile,
            drafts,
            (source.id, 2),
        );
    });
    (changed, pose_changed)
}

impl VolnaApp {
    pub(super) fn inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Сцена");
        ui.label(RichText::new("ИНТЕРАКТИВНАЯ ГЕОМЕТРИЯ").small().weak());
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new("Применить и сбросить").strong())
                .clicked()
            {
                self.apply();
            }
            if ui.small_button("Отменить правки").clicked() {
                self.scene = self.active_scene.clone();
                self.table_drafts.clear();
                self.selection = None;
                self.edited();
                // Новая revision делает восстановление после отклонённой команды явным.
                self.apply();
            }
        });
        ui.label(
            RichText::new(
                "Координаты и угол — live, без запуска на паузе. Размеры, сетка и сигналы — кнопкой с холодным сбросом.",
            )
            .small()
            .weak(),
        );
        ui.separator();
        ui.collapsing("Область, сетка и RMS", |ui| {
            let d = &mut self.scene.domain;
            let mut changed = number(ui, "Ширина, мм", &mut d.width_m, 1e3, 1.0);
            changed |= number(ui, "Высота, мм", &mut d.height_m, 1e3, 1.0);
            changed |= number(ui, "Шаг сетки, мм", &mut d.cell_m, 1e3, 0.05);
            changed |= number(ui, "CFL, S", &mut d.cfl, 1.0, 0.01);
            ui.horizontal(|ui| {
                ui.label("CPML, ячеек");
                changed |= ui.add(egui::DragValue::new(&mut d.pml_cells).range(0..=256)).changed();
            });
            changed |= number(ui, "Окно RMS, нс", &mut self.scene.rms_window_s, 1e9, 0.05);
            ui.label(RichText::new("RMS: неперекрывающиеся физические окна; показана последняя завершённая карта. Изменение сетки требует явного применения.").small().weak());
            if changed { self.edited(); }
        });
        ui.collapsing("Объекты", |ui| {
            for source in &self.scene.sources {
                if ui
                    .selectable_label(
                        self.selection == Some(Selection::Source(source.id)),
                        format!("Волновод · {}", source.name),
                    )
                    .clicked()
                {
                    self.selection = Some(Selection::Source(source.id));
                }
            }
            for reflector in &self.scene.reflectors {
                if ui
                    .selectable_label(
                        self.selection == Some(Selection::Reflector(reflector.id)),
                        format!("PEC · {}", reflector.name),
                    )
                    .clicked()
                {
                    self.selection = Some(Selection::Reflector(reflector.id));
                }
            }
            for probe in &self.scene.probes {
                if ui
                    .selectable_label(
                        self.selection == Some(Selection::Probe(probe.id)),
                        format!("Зонд · {}", probe.name),
                    )
                    .clicked()
                {
                    self.selection = Some(Selection::Probe(probe.id));
                }
            }
            ui.label(
                RichText::new("Добавление — инструмент над полем, затем щелчок в сцене.")
                    .small()
                    .weak(),
            );
        });
        ui.separator();
        ui.heading("Инспектор");
        let mut changed = false;
        let mut pose_changed = false;
        let pose_enabled = self.selection.is_some_and(|s| self.live_available(s.id()));
        if self.selection.is_some() && !pose_enabled {
            ui.label(
                "Live-поза недоступна: дождитесь холодной команды; новый объект сначала примените.",
            );
        }
        let mut preview_source = None;
        match self.selection {
            Some(Selection::Source(id)) => {
                if let Some(source) = self.scene.sources.iter_mut().find(|s| s.id == id) {
                    ui.push_id(id, |ui| {
                        let edits = source_editor(ui, source, &mut self.table_drafts, pose_enabled);
                        changed |= edits.0;
                        pose_changed |= edits.1;
                        if source.frequency_hz > 0.0
                            && source.width_m > 0.0
                            && self.scene.domain.cell_m > 0.0
                        {
                            let lambda = C0 / source.frequency_hz;
                            let cutoff = C0 / (2.0 * source.width_m);
                            ui.separator();
                            ui.monospace(format!(
                                "λ₀ = {:.3} мм · {:.1} яч/λ₀",
                                lambda * 1e3,
                                lambda / self.scene.domain.cell_m
                            ));
                            ui.monospace(format!(
                                "fс₁ = {:.3} ГГц · fс₂ = {:.3} ГГц",
                                cutoff * 1e-9,
                                2.0 * cutoff * 1e-9
                            ));
                            if source.fmax_hz > 0.0 {
                                ui.monospace(format!(
                                    "На f_max: {:.1} яч/λ (нужно ≥20)",
                                    C0 / source.fmax_hz / self.scene.domain.cell_m
                                ));
                            }
                            if source.frequency_hz > cutoff {
                                let lambda_g =
                                    lambda / (1.0 - (cutoff / source.frequency_hz).powi(2)).sqrt();
                                ui.monospace(format!("λg₁ = {:.3} мм", lambda_g * 1e3));
                            } else {
                                ui.label("Ниже отсечки основной моды — затухающее поле.");
                            }
                        }
                        if ui.button("Предпросмотр Jz(t) и апертуры").clicked()
                        {
                            preview_source = Some(source.clone());
                        }
                    });
                }
            }
            Some(Selection::Reflector(id)) => {
                if let Some(object) = self.scene.reflectors.iter_mut().find(|s| s.id == id) {
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut object.name).char_limit(120))
                        .changed();
                    pose_changed |= ui
                        .add_enabled_ui(pose_enabled, |ui| {
                            point_editor(ui, &mut object.center)
                                | number(ui, "Поворот, °", &mut object.angle_deg, 1.0, 0.5)
                        })
                        .inner;
                    match &mut object.shape {
                        Shape::Segment {
                            length_m,
                            thickness_m,
                        } => {
                            changed |= number(ui, "Длина, мм", length_m, 1e3, 0.5);
                            changed |= number(ui, "Толщина, мм", thickness_m, 1e3, 0.1);
                        }
                        Shape::Rectangle { width_m, height_m } => {
                            changed |= number(ui, "Ширина, мм", width_m, 1e3, 0.5);
                            changed |= number(ui, "Высота, мм", height_m, 1e3, 0.5);
                        }
                        Shape::Circle { radius_m } => {
                            changed |= number(ui, "Радиус, мм", radius_m, 1e3, 0.2)
                        }
                        Shape::Polyline {
                            points,
                            thickness_m,
                        } => {
                            changed |= number(ui, "Толщина, мм", thickness_m, 1e3, 0.1);
                            ui.label(
                                "Локальные x y в метрах (до поворота), порядок вдоль ломаной.",
                            );
                            let mut pairs = points.iter().map(|p| [p.x, p.y]).collect();
                            if pair_editor(ui, &mut self.table_drafts, (id, 3), &mut pairs, false) {
                                *points = pairs.into_iter().map(|[x, y]| Point { x, y }).collect();
                                changed = true;
                            }
                        }
                    }
                }
            }
            Some(Selection::Probe(id)) => {
                if let Some(probe) = self.scene.probes.iter_mut().find(|p| p.id == id) {
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut probe.name).char_limit(120))
                        .changed();
                    pose_changed |= ui
                        .add_enabled_ui(pose_enabled, |ui| point_editor(ui, &mut probe.position))
                        .inner;
                    ui.label("Измерение в ближайшем узле Yee для Ez; фактическая координата показана в таблице.");
                }
            }
            None => {
                ui.label("Выберите объект на поле или в списке. Геометрия задаётся в метрах; здесь размеры показаны в мм.");
            }
        }
        if changed || pose_changed {
            self.edited();
        }
        if let Some(selection) = self.selection {
            let now_s = ui.input(|i| i.time);
            if pose_changed {
                self.stage_pose(selection.id(), now_s);
            }
            if ui.input(|i| i.pointer.any_released()) {
                self.flush_poses(now_s, Some(selection.id()));
            }
        }
        if let Some(source) = preview_source {
            match super::plots::Preview::compile(&source) {
                Ok(preview) => {
                    self.preview = Some(preview);
                    self.error = None;
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        if let Some(preview) = &self.preview {
            preview.show(ui, self.dark);
        }
        if self.selection.is_some() {
            ui.horizontal(|ui| {
                if ui.button("Дублировать").clicked() {
                    self.duplicate_selected();
                }
                if ui.button("Удалить").clicked() {
                    self.delete_selected();
                }
            });
        }
        ui.separator();
        ui.collapsing("Модель и ограничения", |ui| {
            ui.label("Самостоятельная 2D-модель Ez, Hx, Hy. Не произвольный срез 3D-устройства; сравнение с экспериментом не заявлено.");
            ui.label("Live-поворот/перенос сохраняет поле и физическое время. Это перестройка PEC, не физика движущегося металла: скорости стенки и Доплера нет. Переходное поле не является стационарным ответом; для статического сравнения используйте явный сброс.");
            ui.label("Лимиты: 16 источников, 256 отражателей / 8192 вершины, 16 зондов, 4096 точек профиля. Формула ≤2048 символов. Бюджет массивов 512 MiB.");
            ui.label("Формулы/таблицы могут быть широкополосными. f_max — декларация пользователя, не измеренная полоса. PEC-маска показывает staircase-сетку.");
            ui.label("JSON — холодный старт, не checkpoint. CSV содержит доступную ограниченную историю worker, а не только точки графика.");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspector_frame(
        app: &mut VolnaApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Rect)> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 1000.0),
            )),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| app.inspector(ui));
        });
        output.textures_delta.clear();
        fn collect(shape: &egui::Shape, labels: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(text) => labels.push((
                    text.galley.job.text.clone(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, labels)),
                _ => {}
            }
        }
        let mut labels = Vec::new();
        for shape in output.shapes {
            collect(&shape.shape, &mut labels);
        }
        labels
    }

    fn enter_number(app: &mut VolnaApp, ctx: &egui::Context, time: f64, label: &str, value: &str) {
        let labels = inspector_frame(app, ctx, time, vec![]);
        let label_rect = labels
            .iter()
            .find(|(text, _)| text == label)
            .expect("подпись числового поля")
            .1;
        let position = labels
            .iter()
            .find(|(text, rect)| {
                text.parse::<f64>().is_ok()
                    && (rect.center().y - label_rect.center().y).abs() < 3.0
                    && rect.center().x > label_rect.right()
            })
            .expect("значение рядом с подписью")
            .1
            .center();
        let button = |pressed| egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        inspector_frame(
            app,
            ctx,
            time + 0.01,
            vec![egui::Event::PointerMoved(position), button(true)],
        );
        inspector_frame(app, ctx, time + 0.02, vec![button(false)]);
        inspector_frame(app, ctx, time + 0.03, vec![egui::Event::Text(value.into())]);
        inspector_frame(
            app,
            ctx,
            time + 0.04,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
    }

    #[test]
    fn typed_inspector_positions_and_angle_are_live_without_applying_other_fields()
    -> crate::Result<()> {
        for selection in [
            Selection::Source(1),
            Selection::Reflector(3),
            Selection::Probe(2),
        ] {
            let mut scene = crate::model::Scene::default();
            scene.reflectors.push(crate::model::Reflector {
                id: 3,
                name: "Отражатель".into(),
                center: Point { x: 0.3, y: 0.2 },
                angle_deg: 0.0,
                shape: Shape::Circle { radius_m: 0.02 },
            });
            let mut app = crate::ui::tests::app_with_scene(scene)?;
            let before = app.snapshot.as_ref().unwrap().clone();
            app.selection = Some(selection);
            app.scene.sources[0].signal = Signal::Formula {
                expression: "bad(".into(),
            };
            app.scene.domain.cell_m = 0.0;
            let ctx = egui::Context::default();
            let x = if selection == Selection::Source(1) {
                "91"
            } else {
                "280"
            };
            enter_number(&mut app, &ctx, 1.0, "x, мм", x);
            if !matches!(selection, Selection::Probe(_)) {
                enter_number(&mut app, &ctx, 2.0, "Поворот, °", "12");
            }
            let id = selection.id();
            let pose = app.scene.pose(id).unwrap();
            let expected_x = if selection == Selection::Source(1) {
                0.091
            } else {
                0.280
            };
            match &pose {
                crate::model::ObjectPose::Source {
                    center, angle_deg, ..
                }
                | crate::model::ObjectPose::Reflector {
                    center, angle_deg, ..
                } => {
                    assert_eq!(center.x, expected_x);
                    assert_eq!(*angle_deg, 12.0);
                }
                crate::model::ObjectPose::Probe { position, .. } => {
                    assert_eq!(position.x, expected_x)
                }
            }
            assert_ne!(
                Some(pose.clone()),
                before.scene.pose(id),
                "ввод должен менять координату"
            );
            crate::ui::tests::wait_until(&mut app, |a| {
                a.active_scene.pose(id) == Some(pose.clone())
            })?;
            let after = app.snapshot.as_ref().unwrap();
            assert_eq!(*after.scene, before.scene.with_pose(&pose)?);
            assert_eq!(
                (after.run, after.step, after.time_s, after.running),
                (before.run, before.step, before.time_s, false)
            );
            assert_eq!(app.scene.domain.cell_m, 0.0);
            assert!(app.cold_pending.is_none());
        }
        Ok(())
    }

    #[test]
    fn table_rejects_partial_and_nonfinite_or_unordered_data() {
        for bad in [
            "0 1\n1",
            "0 1\n1 NaN",
            "0 1\n1 inf",
            "0 1 2\n1 2",
            "0 1\n0 2",
            "1 1\n0 2",
        ] {
            assert!(parse_pairs(bad, true).is_err(), "{bad}");
        }
        assert_eq!(
            parse_pairs("-1 0\n0 1\n1 0", true),
            Ok(vec![[-1.0, 0.0], [0.0, 1.0], [1.0, 0.0]])
        );
    }

    #[test]
    fn polyline_coordinates_need_not_be_monotone() {
        assert!(parse_pairs("0 0\n1 0\n0 1", false).is_ok());
    }
}
