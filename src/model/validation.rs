use std::collections::HashSet;

use super::{C0, GridSpec, HISTORY_LIMIT, MEMORY_LIMIT, Point, SCHEMA_VERSION, Scene, Shape};
use crate::{Error, Result};

fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(message.into()))
    }
}

fn positive(x: f64, name: &str) -> Result<()> {
    require(
        x.is_finite() && x > 0.0,
        format!("{name}: требуется конечное положительное число"),
    )
}

impl Scene {
    pub fn validate(&self) -> Result<GridSpec> {
        require(
            self.schema_version == SCHEMA_VERSION,
            "неподдерживаемая версия схемы сцены",
        )?;
        let d = &self.domain;
        positive(d.width_m, "Ширина области")?;
        positive(d.height_m, "Высота области")?;
        require(
            d.cell_m.is_finite() && (1e-12..=1e6).contains(&d.cell_m),
            "Шаг сетки должен быть от 10⁻¹² до 10⁶ м",
        )?;
        require(
            d.cfl.is_finite() && d.cfl > 0.0 && d.cfl < 1.0,
            "CFL должен быть строго между 0 и 1",
        )?;
        require(
            d.pml_cells == 0 || (8..=64).contains(&d.pml_cells),
            "CPML: 8–64 ячейки; 0 означает замкнутую PEC-область",
        )?;
        let dimension = |length: f64| -> Result<usize> {
            let n = length / d.cell_m;
            require(
                n.is_finite() && (16.0..=8192.0).contains(&n),
                "Размер стороны сетки должен быть 16–8192 ячейки",
            )?;
            require(
                (n - n.round()).abs() <= 1e-9 * n,
                "Размеры области должны быть кратны шагу сетки",
            )?;
            (n.round() as usize)
                .checked_add(1 + 2 * d.pml_cells)
                .ok_or_else(|| Error::Invalid("переполнение размера сетки".into()))
        };
        let nx = dimension(d.width_m)?;
        let ny = dimension(d.height_m)?;
        let n = nx
            .checked_mul(ny)
            .ok_or_else(|| Error::Invalid("переполнение площади сетки".into()))?;
        let dt = d.cfl * d.cell_m / (C0 * 2.0_f64.sqrt());
        require(
            dt.is_finite() && dt > 0.0,
            "шаг времени не представим в f64",
        )?;
        require(
            self.rms_window_s.is_finite()
                && self.rms_window_s >= dt
                && self.rms_window_s / dt <= 10_000_000.0,
            "Окно RMS должно содержать от 1 до 10 000 000 физических шагов",
        )?;
        require(
            self.sources.len() <= 16 && self.reflectors.len() <= 256 && self.probes.len() <= 16,
            "Лимиты: 16 источников, 256 отражателей, 16 зондов",
        )?;
        let mut ids = HashSet::new();
        for (id, name) in self
            .sources
            .iter()
            .map(|s| (s.id, &s.name))
            .chain(self.reflectors.iter().map(|s| (s.id, &s.name)))
            .chain(self.probes.iter().map(|s| (s.id, &s.name)))
        {
            require(
                id != 0 && id != u64::MAX && ids.insert(id),
                "Идентификаторы объектов должны быть уникальны и ненулевые",
            )?;
            require(
                !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
                "Имя объекта: 1–256 байт, без управляющих символов",
            )?;
        }
        let inside = |p: Point, margin: f64| -> bool {
            p.x.is_finite()
                && p.y.is_finite()
                && p.x >= margin
                && p.y >= margin
                && p.x <= d.width_m - margin
                && p.y <= d.height_m - margin
        };
        let mut support_cells = 0_usize;
        for s in &self.sources {
            for (x, name) in [
                (s.width_m, "Ширина канала"),
                (s.length_m, "Длина волновода"),
                (s.wall_m, "Толщина стенки"),
                (s.strip_m, "Толщина полосы возбуждения"),
                (s.frequency_hz, "Частота"),
                (s.fmax_hz, "Верхняя значимая частота"),
            ] {
                positive(x, name)?;
            }
            require(
                s.width_m >= 6.0 * d.cell_m
                    && s.wall_m >= 2.0 * d.cell_m
                    && s.strip_m >= 2.0 * d.cell_m,
                "Канал должен быть не уже 6 ячеек; стенка и полоса возбуждения — не тоньше 2 ячеек",
            )?;
            require(
                s.angle_deg.is_finite()
                    && s.angle_deg.abs() <= 360_000.0
                    && s.current_a_m2.is_finite()
                    && s.phase_rad.is_finite()
                    && s.delay_s.is_finite()
                    && s.delay_s >= 0.0,
                "Некорректный угол, амплитуда, фаза или задержка источника",
            )?;
            require(
                s.fmax_hz >= s.frequency_hz && C0 / s.fmax_hz / d.cell_m >= 20.0,
                format!(
                    "{}: нужно ≥20 ячеек на длину волны f_max; уменьшите шаг сетки",
                    s.name
                ),
            )?;
            require(
                s.source_offset_m.is_finite()
                    && s.source_offset_m >= s.wall_m + s.strip_m * 0.5 + d.cell_m
                    && s.source_offset_m + s.strip_m * 0.5 + d.cell_m <= s.length_m,
                format!(
                    "{}: полоса возбуждения должна целиком лежать внутри волновода",
                    s.name
                ),
            )?;
            if let Some(bend) = &s.bend {
                positive(bend.length_m, "Длина второго плеча Г-волновода")?;
                let min_arm = s.width_m * 0.5 + s.wall_m + 2.0 * d.cell_m;
                require(
                    s.length_m >= min_arm && bend.length_m >= min_arm,
                    "Плечи Г-волновода должны быть не короче половины ширины канала + стенка + 2 ячейки",
                )?;
                require(
                    s.source_offset_m + s.strip_m * 0.5 <= s.length_m - s.width_m * 0.5 - d.cell_m,
                    "Полоса возбуждения должна находиться в прямой части до поворота Г-волновода",
                )?;
            }
            require(
                s.outline().iter().all(|p| inside(*p, d.cell_m)),
                format!("{} выходит за полезную область", s.name),
            )?;
            let (sn, cs) = s.angle_deg.to_radians().sin_cos();
            let bx = (s.strip_m * cs.abs() + s.width_m * sn.abs()) / d.cell_m + 3.0;
            let by = (s.strip_m * sn.abs() + s.width_m * cs.abs()) / d.cell_m + 3.0;
            let estimate = (bx.ceil() * by.ceil()).min(n as f64) as usize;
            support_cells = support_cells
                .checked_add(estimate)
                .ok_or_else(|| Error::Invalid("переполнение размера источников".into()))?;
        }
        let mut vertices = 0_usize;
        // A bend performs up to four wall-box checks plus the back cap.
        let mut geometry_work: usize = self
            .sources
            .iter()
            .map(|s| if s.bend.is_some() { 5 } else { 1 })
            .sum();
        for r in &self.reflectors {
            require(
                r.angle_deg.is_finite() && r.angle_deg.abs() <= 360_000.0,
                "Некорректный угол отражателя",
            )?;
            let min_size = 2.0 * d.cell_m;
            match &r.shape {
                Shape::Segment {
                    length_m,
                    thickness_m,
                } => {
                    require(
                        length_m.is_finite()
                            && *length_m >= min_size
                            && thickness_m.is_finite()
                            && *thickness_m >= min_size,
                        "Длина и толщина отражателя должны быть ≥2 ячеек",
                    )?;
                    geometry_work += 1;
                }
                Shape::Rectangle { width_m, height_m } => {
                    require(
                        width_m.is_finite()
                            && *width_m >= min_size
                            && height_m.is_finite()
                            && *height_m >= min_size,
                        "Размеры прямоугольника должны быть ≥2 ячеек",
                    )?;
                    geometry_work += 1;
                }
                Shape::Circle { radius_m } => {
                    require(
                        radius_m.is_finite() && *radius_m >= min_size,
                        "Радиус должен быть ≥2 ячеек",
                    )?;
                    geometry_work += 1;
                }
                Shape::Polyline {
                    points,
                    thickness_m,
                } => {
                    require(
                        (2..=8192).contains(&points.len())
                            && thickness_m.is_finite()
                            && *thickness_m >= min_size,
                        "Ломаная: 2–8192 точки, толщина ≥2 ячеек",
                    )?;
                    vertices += points.len();
                    require(
                        points.iter().all(|p| p.x.is_finite() && p.y.is_finite())
                            && points
                                .windows(2)
                                .all(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y) >= min_size),
                        "Точки ломаной должны быть конечными, соседние — разнесены хотя бы на 2 ячейки",
                    )?;
                    geometry_work += points.len() - 1;
                }
            }
            let (a, b) = r.local_bounds();
            let corners = [a, b, Point { x: a.x, y: b.y }, Point { x: b.x, y: a.y }];
            require(
                corners.iter().all(|p| inside(r.world(*p), d.cell_m)),
                format!("{} выходит за полезную область", r.name),
            )?;
        }
        require(vertices <= 8192, "Суммарный лимит вершин ломаных — 8192")?;
        require(
            n.checked_mul(geometry_work)
                .is_some_and(|work| work <= 50_000_000),
            "Растеризация слишком велика: уменьшите число сегментов или размер сетки (лимит 50 млн проверок)",
        )?;
        for p in &self.probes {
            require(
                inside(p.position, 0.0),
                format!("{} находится вне полезной области", p.name),
            )?;
        }
        // Консервативный пик: обе сцены при замене, поля, CPML, RMS, два снимка,
        // разреженные источники и копия истории при экспорте.
        let estimated = n
            .checked_mul(256)
            .and_then(|v| {
                support_cells
                    .checked_mul(128)
                    .and_then(|s| v.checked_add(s))
            })
            .and_then(|v| {
                self.probes
                    .len()
                    .checked_mul(HISTORY_LIMIT * 64)
                    .and_then(|s| v.checked_add(s))
            })
            .ok_or_else(|| Error::Invalid("переполнение оценки памяти".into()))?;
        require(
            estimated <= MEMORY_LIMIT,
            format!(
                "Оценка памяти {:.1} MiB превышает лимит 512 MiB",
                estimated as f64 / 1048576.0
            ),
        )?;
        Ok(GridSpec {
            nx,
            ny,
            pml: d.pml_cells,
            dx: d.cell_m,
            dt,
        })
    }
}
