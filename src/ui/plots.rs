use super::{Selection, VolnaApp, palette};
use crate::{
    model::{Profile, Signal, Source},
    sources::CompiledSource,
};
use eframe::egui::{self, RichText};
use egui_plot::{HoverPosition, Legend, Line, LineStyle, Plot, PlotPoint, Text};

pub(super) struct Preview {
    name: String,
    time: Vec<[f64; 2]>,
    aperture: Vec<[f64; 2]>,
    aperture_time_s: f64,
}

impl Preview {
    pub(super) fn compile(source: &Source) -> crate::Result<Self> {
        let expressions = [
            match &source.signal {
                Signal::Formula { expression } => Some(expression),
                _ => None,
            },
            match &source.aperture {
                Profile::Formula { expression } => Some(expression),
                _ => None,
            },
            match &source.phase_profile {
                Profile::Formula { expression } => Some(expression),
                _ => None,
            },
        ];
        if expressions
            .iter()
            .flatten()
            .any(|expression| expression.chars().count() > 2048)
        {
            return Err(crate::Error::Expression(
                "Для предпросмотра формула ограничена 2048 символами.".into(),
            ));
        }
        // Компиляция только по явной кнопке, не в каждом repaint/движении мыши.
        let compiled = CompiledSource::new(source)?;
        let period = 1.0 / source.frequency_hz;
        let (start, end) = match &source.signal {
            Signal::Gaussian { center_s, width_s } => {
                (source.delay_s, source.delay_s + center_s + 4.0 * width_s)
            }
            Signal::Table { samples, .. } => (
                source.delay_s + samples.first().map_or(0.0, |s| s[0]),
                source.delay_s + samples.last().map_or(period, |s| s[0]),
            ),
            Signal::Continuous { ramp_periods } => (
                source.delay_s,
                source.delay_s + (ramp_periods + 4.0) * period,
            ),
            Signal::Formula { .. } => (source.delay_s, source.delay_s + 8.0 * period),
        };
        if !start.is_finite() || !end.is_finite() || end <= start {
            return Err(crate::Error::Invalid(
                "Интервал предпросмотра должен быть конечным и ненулевым.".into(),
            ));
        }
        let mut time = Vec::with_capacity(256);
        let mut aperture_time_s = start;
        let mut peak = -1.0;
        for i in 0..256 {
            let t = start + (end - start) * f64::from(i) / 255.0;
            let current = compiled.value(t, 0.0)?;
            if current.abs() > peak {
                peak = current.abs();
                aperture_time_s = t;
            }
            time.push([t * 1e9, current]);
        }
        let mut aperture = Vec::with_capacity(129);
        for i in 0..129 {
            let s = -1.0 + 2.0 * f64::from(i) / 128.0;
            aperture.push([s, compiled.value(aperture_time_s, s)?]);
        }
        Ok(Self {
            name: source.name.clone(),
            time,
            aperture,
            aperture_time_s,
        })
    }

    pub(super) fn show(&self, ui: &mut egui::Ui, dark: bool) {
        ui.separator();
        ui.label(RichText::new(format!("Предпросмотр · {}", self.name)).strong());
        ui.label(
            RichText::new("Jz(t, s=0), не поле Ez; 256 отсчётов, не спектральная проверка.")
                .small(),
        );
        single_plot(ui, "preview_time", &self.time, "t, нс", dark);
        ui.label(format!("Jz(s) при t={:.4} нс", self.aperture_time_s * 1e9));
        single_plot(ui, "preview_aperture", &self.aperture, "s ∈ [−1,1]", dark);
        ui.collapsing("Таблица предпросмотра", |ui| {
            ui.label("Время [нс] / Jz [А/м²]");
            egui::ScrollArea::vertical()
                .id_salt("preview_time_table")
                .max_height(120.0)
                .show_rows(ui, 17.0, self.time.len(), |ui, rows| {
                    for i in rows {
                        ui.monospace(format!("{:.6}  {:.6e}", self.time[i][0], self.time[i][1]));
                    }
                });
            ui.label("Апертурная координата s / Jz [А/м²]");
            egui::ScrollArea::vertical()
                .id_salt("preview_aperture_table")
                .max_height(120.0)
                .show_rows(ui, 17.0, self.aperture.len(), |ui, rows| {
                    for i in rows {
                        ui.monospace(format!(
                            "{:.6}  {:.6e}",
                            self.aperture[i][0], self.aperture[i][1]
                        ));
                    }
                });
        });
    }
}

fn single_plot(ui: &mut egui::Ui, id: &str, values: &[[f64; 2]], x_label: &str, dark: bool) {
    Plot::new(id)
        .height(135.0)
        .show_background(false)
        .x_axis_label(x_label)
        .y_axis_label("Jz, А/м²")
        .include_y(0.0)
        .show_crosshair(true)
        .show(ui, |plot| {
            plot.line(
                Line::new("Jz", values.to_vec())
                    .width(2.0)
                    .color(palette::series(0, dark)),
            );
        });
}

/// Сохраняем локальные экстремумы, порядок времени и края при ограничении отрисовки.
/// Это только графическое прореживание, не антиалиас-фильтр измерений.
fn display_samples(samples: &[[f64; 2]], limit: usize) -> Vec<[f64; 2]> {
    let scale = |p: [f64; 2]| [p[0] * 1e9, p[1]];
    if samples.len() <= limit {
        return samples.iter().copied().map(scale).collect();
    }
    let bins = (limit.saturating_sub(2) / 2).max(1);
    let chunk = (samples.len() - 2).div_ceil(bins);
    let mut output = Vec::with_capacity(limit);
    output.push(scale(samples[0]));
    for values in samples[1..samples.len() - 1].chunks(chunk) {
        let mut min = 0;
        let mut max = 0;
        for i in 1..values.len() {
            if values[i][1] < values[min][1] {
                min = i;
            }
            if values[i][1] > values[max][1] {
                max = i;
            }
        }
        for index in [min.min(max), min.max(max)] {
            output.push(scale(values[index]));
        }
    }
    output.push(scale(samples[samples.len() - 1]));
    output
}

/// Только диапазон отображения: отсчёты не изменяются и не ограничиваются.
/// Оси расширяются независимо: egui_plot 0.37 иначе заимствует размер другой
/// оси, и tiny Ez не способен раздвинуть единственную ненулевую координату t.
fn probe_plot_bounds(
    data: &[Vec<[f64; 2]>],
    time_ns: f64,
    dt_ns: f64,
    screen_extent: f64,
) -> Result<egui_plot::PlotBounds, &'static str> {
    let error = "Диапазон графика не представим конечными координатами; измерения сохранены, используйте таблицу или CSV.";
    if !time_ns.is_finite()
        || !dt_ns.is_finite()
        || dt_ns <= 0.0
        || !screen_extent.is_finite()
        || screen_extent <= 0.0
    {
        return Err(error);
    }
    let mut x = [f64::INFINITY, f64::NEG_INFINITY];
    let mut y = [0.0_f64, 0.0_f64];
    for point in data.iter().flatten() {
        if point.iter().any(|v| !v.is_finite()) {
            return Err(error);
        }
        x[0] = x[0].min(point[0]);
        x[1] = x[1].max(point[0]);
        y[0] = y[0].min(point[1]);
        y[1] = y[1].max(point[1]);
    }
    if x[0] > x[1] {
        x = [time_ns, time_ns];
    }
    // Нижний предел ширины нужен только для конечного screen/span transform,
    // а не как допуск или порог обнуления физического поля.
    let representable_span = (screen_extent * 4.0) / f64::MAX;
    let padded = |range: [f64; 2], minimum_span: f64| {
        let span = range[1] - range[0];
        let scale = range[0].abs().max(range[1].abs());
        let padding = (span * 0.05)
            .max(scale * (32.0 * f64::EPSILON))
            .max(minimum_span * 0.5);
        let lo = range[0] - padding;
        let hi = range[1] + padding;
        if lo.is_finite() && hi.is_finite() && hi > lo && (hi - lo).is_finite() {
            Ok([lo, hi])
        } else {
            Err(error)
        }
    };
    let x = padded(x, dt_ns.max(representable_span))?;
    let y = padded(
        y,
        if y == [0.0, 0.0] {
            2.0
        } else {
            representable_span
        },
    )?;
    Ok(egui_plot::PlotBounds::from_min_max(
        [x[0], y[0]],
        [x[1], y[1]],
    ))
}

fn nearest_sample(samples: &[[f64; 2]], time: f64) -> Option<[f64; 2]> {
    let right = samples.partition_point(|p| p[0] < time);
    match (
        right.checked_sub(1).and_then(|i| samples.get(i)),
        samples.get(right),
    ) {
        (Some(left), Some(right)) => Some(if time - left[0] <= right[0] - time {
            *left
        } else {
            *right
        }),
        (Some(point), None) | (None, Some(point)) => Some(*point),
        (None, None) => None,
    }
}

impl VolnaApp {
    pub(super) fn probe_plots(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Зонды · Ez(t)").strong());
            ui.selectable_value(&mut self.selected_probe_only, false, "До трёх закреплённых");
            ui.selectable_value(&mut self.selected_probe_only, true, "Выбранный зонд");
            ui.checkbox(&mut self.show_probe_table, "Таблица");
        });
        let Some(snapshot) = &self.snapshot else {
            ui.label("Ожидание измерений…");
            return;
        };
        super::stable_line(
            ui,
            RichText::new(format!(
                "Принятый сегмент с шага {} · t₀={:.5} нс · e{} / r{} / run {}",
                snapshot.history_start_step,
                snapshot.history_start_time_s * 1e9,
                snapshot.epoch,
                snapshot.revision,
                snapshot.run
            ))
            .small()
            .weak(),
        );
        let traces: Vec<_> = snapshot
            .probes
            .iter()
            .filter(|trace| {
                if self.selected_probe_only {
                    self.selection == Some(Selection::Probe(trace.id))
                } else {
                    self.probe_colors.contains(&Some(trace.id))
                }
            })
            .take(3)
            .collect();
        if traces.is_empty() {
            ui.label("Добавьте зонд или выберите его в списке. Первые три закреплены за цветами; остальные доступны по выбору.");
            return;
        }
        let data: Vec<_> = traces
            .iter()
            .map(|trace| display_samples(&trace.samples, 4096))
            .collect();
        let height = (ui.available_height() - 44.0 - 2.0 * ui.spacing().item_spacing.y).max(40.0);
        if self.show_probe_table {
            egui::ScrollArea::both()
                .id_salt("probe_table")
                .max_height(height)
                .show(ui, |ui| {
                    for (trace, values) in traces.iter().zip(&data) {
                        ui.label(
                            RichText::new(format!(
                                "{} · фактический узел ({:.3}, {:.3}) мм · RMS {} В/м",
                                trace.name,
                                trace.sample_position.x * 1e3,
                                trace.sample_position.y * 1e3,
                                trace
                                    .rms
                                    .map_or_else(|| "не готов".into(), |v| format!("{v:.6e}"))
                            ))
                            .strong(),
                        );
                        ui.label("t, нс                            Ez, В/м");
                        egui::ScrollArea::vertical()
                            .id_salt(("trace_rows", trace.id))
                            .max_height(110.0)
                            .show_rows(ui, 18.0, values.len(), |ui, rows| {
                                for i in rows {
                                    ui.monospace(format!(
                                        "{:>14.6}  {:>18.8e}",
                                        values[i][0], values[i][1]
                                    ));
                                }
                            });
                    }
                });
        } else {
            let bounds = match probe_plot_bounds(
                &data,
                snapshot.time_s * 1e9,
                snapshot.grid.dt * 1e9,
                f64::from(ui.available_width().max(height)),
            ) {
                Ok(bounds) => bounds,
                Err(message) => {
                    ui.label(message);
                    return;
                }
            };
            let mut chart = Plot::new("probe_ez")
                .height(height)
                .show_background(false)
                .x_axis_label("Физическое время, нс")
                .y_axis_label("Ez, В/м")
                .include_x(bounds.min()[0])
                .include_x(bounds.max()[0])
                .include_y(bounds.min()[1])
                .include_y(bounds.max()[1])
                .set_margin_fraction(egui::Vec2::ZERO)
                .show_crosshair(true)
                .label_formatter(|hover| {
                    let position = match hover {
                        HoverPosition::NearDataPoint { position, .. }
                        | HoverPosition::Elsewhere { position } => position,
                    };
                    let mut text = format!("t ≈ {:.6} нс", position.x);
                    for (trace, values) in traces.iter().zip(&data) {
                        if let Some(point) = nearest_sample(values, position.x) {
                            text.push_str(&format!(
                                "\n{:.6e} В/м · {} (t={:.6})",
                                point[1], trace.name, point[0]
                            ));
                        }
                    }
                    Some(text)
                });
            if traces.len() >= 2 {
                chart = chart.legend(Legend::default());
            }
            chart.show(ui, |plot| {
                let mut end_points: Vec<(f64, f64)> = Vec::new();
                for (trace, values) in traces.iter().zip(&data) {
                    let slot = self
                        .probe_colors
                        .iter()
                        .position(|id| *id == Some(trace.id))
                        .unwrap_or(3);
                    let style = match slot {
                        1 => LineStyle::dashed_loose(),
                        2 => LineStyle::dotted_dense(),
                        _ => LineStyle::Solid,
                    };
                    plot.line(
                        Line::new(&trace.name, values.clone())
                            .width(2.0)
                            .color(palette::series(slot, self.dark))
                            .style(style),
                    );
                    if let Some(last) = values.last() {
                        let position = PlotPoint::new(last[0], last[1]);
                        let screen = plot.screen_from_plot(position);
                        if end_points.iter().all(|&(x, y)| {
                            (x - f64::from(screen.x)).abs() > 90.0
                                || (y - f64::from(screen.y)).abs() > 20.0
                        }) {
                            plot.text(
                                Text::new("", position, &trace.name)
                                    .id(egui::Id::new(("probe_endpoint", trace.id)))
                                    .color(palette::ink(self.dark))
                                    .anchor(egui::Align2::RIGHT_BOTTOM),
                            );
                            end_points.push((f64::from(screen.x), f64::from(screen.y)));
                        }
                    }
                }
            });
        }
        let rms_labels = traces
            .iter()
            .map(|trace| {
                format!(
                    "{}: RMS {} В/м",
                    trace.name,
                    trace.rms.map_or_else(|| "—".into(), |v| format!("{v:.3e}"))
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        super::stable_line(ui, rms_labels);
        super::stable_line(ui, RichText::new("≤4096 точек/кривая; экспорт — история worker. Прореживание не гарантирует отсутствие aliasing.").small().weak());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_ui_probe_legend_excludes_annotation_ids_but_keeps_real_series() -> crate::Result<()> {
        let mut app = crate::ui::tests::app_with_scene(crate::model::Scene::interference_demo())?;
        let mut snapshot = (**app.snapshot.as_ref().unwrap()).clone();
        snapshot.revision += 1;
        for trace in &mut snapshot.probes {
            // Совпадающие конечные точки оставляют только одну direct annotation.
            trace.samples = vec![[0.0, 0.0], [1e-9, 1.0]];
        }
        let names: Vec<_> = snapshot
            .probes
            .iter()
            .map(|trace| trace.name.clone())
            .collect();
        assert_eq!(names.len(), 3);
        app.accept_snapshot(std::sync::Arc::new(snapshot));
        let ctx = egui::Context::default();
        for dark in [true, false] {
            app.dark = dark;
            super::super::palette::configure(&ctx, dark);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| app.show(ui));
            output.textures_delta.clear();
            fn collect(shape: &egui::Shape, labels: &mut Vec<String>) {
                match shape {
                    egui::Shape::Text(text) => labels.push(text.galley.job.text.clone()),
                    egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, labels)),
                    _ => {}
                }
            }
            let mut labels = Vec::new();
            for shape in &output.shapes {
                collect(&shape.shape, &mut labels);
            }
            assert!(
                !labels.iter().any(|text| text.ends_with("_label")),
                "служебный item ID не должен становиться серией легенды"
            );
            for name in &names {
                assert!(
                    labels.iter().filter(|text| *text == name).count() >= 2,
                    "настоящий зонд остаётся на поле и в легенде: {name}"
                );
            }
            assert!(
                labels.iter().filter(|text| **text == names[0]).count() >= 3,
                "direct annotation первого зонда должна остаться"
            );
        }
        Ok(())
    }

    #[test]
    fn live_history_single_tiny_sample_renders_without_degenerate_bounds() -> crate::Result<()> {
        let mut app = crate::ui::tests::app_with_scene(crate::model::Scene::default())?;
        let ctx = egui::Context::default();
        for samples in [
            vec![],
            vec![[0.403_329_891_584_196e-9, -2.337_905_516_805_777e-49]],
        ] {
            let mut snapshot = (**app.snapshot.as_ref().unwrap()).clone();
            snapshot.revision += 1;
            snapshot.probes[0].samples = samples;
            app.accept_snapshot(std::sync::Arc::new(snapshot));
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| app.show(ui));
            output.textures_delta.clear();
            assert!(!output.shapes.is_empty());
        }
        Ok(())
    }

    #[test]
    fn plot_bounds_are_independent_finite_and_contain_unchanged_measurements() {
        let cases = [
            vec![],
            vec![[0.403_329_891_584_196, -2.337_905_516_805_777e-49]],
            vec![[0.4, -1.0], [0.4, 2.0]],
            vec![[0.4, 0.0], [0.5, 0.0]],
            vec![[0.4, 1e-300], [0.5, 1e-300]],
            vec![[0.4, f64::from_bits(1)]],
            vec![[0.4, -1e300], [0.5, 1e300]],
            vec![[1e299, 1.0]],
        ];
        for values in cases {
            let data = vec![values.clone()];
            let bounds = probe_plot_bounds(&data, 0.4, 0.002, 1400.0).unwrap();
            assert!(bounds.is_valid());
            for axis in 0..2 {
                let span = bounds.max()[axis] - bounds.min()[axis];
                assert!(span.is_finite() && span > 0.0);
                for point in &values {
                    assert!(bounds.min()[axis] <= point[axis] && point[axis] <= bounds.max()[axis]);
                }
            }
            assert_eq!(data[0], values);
        }
        for values in [
            vec![[0.4, -f64::MAX], [0.5, f64::MAX]],
            vec![[f64::MAX, 1.0]],
            vec![[f64::INFINITY, 0.0]],
            vec![[0.4, f64::NAN]],
        ] {
            assert!(probe_plot_bounds(&[values], 0.4, 0.002, 1400.0).is_err());
        }
    }

    #[test]
    fn full_ui_handles_degenerate_and_extreme_histories_across_repeated_resets() -> crate::Result<()>
    {
        let mut app = crate::ui::tests::app_with_scene(crate::model::Scene::default())?;
        let ctx = egui::Context::default();
        let cases = [
            vec![],
            vec![[0.403_329_891_584_196e-9, -2.337_905_516_805_777e-49]],
            vec![[0.4e-9, -1.0], [0.4e-9, 2.0]],
            vec![[0.4e-9, 0.0], [0.5e-9, 0.0]],
            vec![[0.4e-9, 1e-300], [0.5e-9, 1e-300]],
            vec![[0.4e-9, f64::from_bits(1)]],
            vec![[0.4e-9, -1e300], [0.5e-9, 1e300]],
            vec![[1e290, 1.0]],
            vec![[0.4e-9, -f64::MAX], [0.5e-9, f64::MAX]],
        ];
        for cycle in 0..3 {
            for (index, samples) in cases.iter().enumerate() {
                let mut snapshot = (**app.snapshot.as_ref().unwrap()).clone();
                snapshot.revision += 1;
                snapshot.history_start_step += 1;
                snapshot.probes[0].samples = samples.clone();
                app.accept_snapshot(std::sync::Arc::new(snapshot));
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 900.0),
                    )),
                    time: Some((cycle * cases.len() + index) as f64 * 0.04),
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |ui| app.show(ui));
                output.textures_delta.clear();
                assert!(!output.shapes.is_empty());
                assert_eq!(app.snapshot.as_ref().unwrap().probes[0].samples, *samples);
            }
        }
        Ok(())
    }

    #[test]
    fn plot_reduction_preserves_endpoints_extrema_and_budget() {
        let mut samples: Vec<_> = (0..20_000).map(|i| [f64::from(i) * 1e-12, 0.0]).collect();
        samples[999][1] = 7.0;
        samples[7777][1] = -8.0;
        let display = display_samples(&samples, 4096);
        assert!(display.len() <= 4096);
        assert_eq!(display[0][0], 0.0);
        assert_eq!(display.last().unwrap()[0], samples.last().unwrap()[0] * 1e9);
        assert!(display.iter().any(|p| p[1] == 7.0));
        assert!(display.iter().any(|p| p[1] == -8.0));
        assert!(display.windows(2).all(|w| w[0][0] <= w[1][0]));
    }

    #[test]
    fn cursor_nearest_sample_handles_empty_edges_and_ties() {
        assert_eq!(nearest_sample(&[], 1.0), None);
        let samples = [[0.0, 2.0], [2.0, 4.0]];
        assert_eq!(nearest_sample(&samples, 1.0), Some(samples[0]));
        assert_eq!(nearest_sample(&samples, 99.0), Some(samples[1]));
    }

    #[test]
    fn source_preview_is_bounded_and_finite() -> crate::Result<()> {
        let preview = Preview::compile(&Source::default())?;
        assert_eq!(preview.time.len(), 256);
        assert_eq!(preview.aperture.len(), 129);
        assert!(
            preview
                .time
                .iter()
                .chain(&preview.aperture)
                .flatten()
                .all(|v| v.is_finite())
        );
        Ok(())
    }
}
