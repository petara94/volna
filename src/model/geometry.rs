use super::{GridSpec, Point, Reflector, Scene, Shape, Source};

fn local(point: Point, center: Point, angle: f64) -> Point {
    let (s, c) = angle.to_radians().sin_cos();
    let x = point.x - center.x;
    let y = point.y - center.y;
    Point {
        x: c * x + s * y,
        y: -s * x + c * y,
    }
}

fn world(point: Point, center: Point, angle: f64) -> Point {
    let (s, c) = angle.to_radians().sin_cos();
    Point {
        x: center.x + c * point.x - s * point.y,
        y: center.y + s * point.x + c * point.y,
    }
}

fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let length2 = dx * dx + dy * dy;
    if length2 == 0.0 {
        return (p.x - a.x).hypot(p.y - a.y);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / length2).clamp(0.0, 1.0);
    (p.x - a.x - t * dx).hypot(p.y - a.y - t * dy)
}

impl Source {
    pub fn local(&self, point: Point) -> Point {
        local(point, self.center, self.angle_deg)
    }
    pub fn world(&self, point: Point) -> Point {
        world(point, self.center, self.angle_deg)
    }

    fn coordinate_tolerance(&self) -> f64 {
        // Только ошибка преобразования координат, не допуск численного решения.
        32.0 * f64::EPSILON
            * (self.center.x.abs()
                + self.center.y.abs()
                + self.length_m
                + self.width_m
                + self.wall_m
                + self.bend.as_ref().map_or(0.0, |bend| bend.length_m))
    }

    pub fn is_pec(&self, point: Point) -> bool {
        let p = self.local(point);
        let tol = self.coordinate_tolerance();
        if let Some(bend) = &self.bend {
            let corner = self.length_m * 0.5;
            let half = self.width_m * 0.5;
            let outer = half + self.wall_m;
            let in_rect = |x0: f64, x1: f64, y0: f64, y1: f64| {
                p.x >= x0 - tol && p.x <= x1 + tol && p.y >= y0 - tol && p.y <= y1 + tol
            };
            // Four wall rectangles joined at the sharp elbow. There is no
            // transverse cap across the turn or across the +Y outlet.
            return in_rect(-corner, corner + outer, -outer, -half)
                || in_rect(-corner, corner - half, half, outer)
                || in_rect(corner + half, corner + outer, -outer, bend.length_m)
                || in_rect(corner - outer, corner - half, half, bend.length_m)
                || (self.closed_back && in_rect(-corner, -corner + self.wall_m, -outer, outer));
        }
        let wall = p.y.abs() + tol >= self.width_m * 0.5;
        let back = self.closed_back && p.x <= -self.length_m * 0.5 + self.wall_m + tol;
        p.x.abs() <= self.length_m * 0.5 + tol
            && p.y.abs() <= self.width_m * 0.5 + self.wall_m + tol
            && (wall || back)
    }

    pub fn excitation_coordinate(&self, point: Point) -> Option<f64> {
        self.excitation_weight(point).map(|(s, _)| s)
    }

    /// Продольное окно cos² имеет фиксированную физическую толщину и ноль на краях.
    pub fn excitation_weight(&self, point: Point) -> Option<(f64, f64)> {
        let p = self.local(point);
        let source_x = -self.length_m * 0.5 + self.source_offset_m;
        let n = p.x - source_x;
        let tol = self.coordinate_tolerance();
        if n.abs() < self.strip_m * 0.5 - tol && p.y.abs() < self.width_m * 0.5 - tol {
            let weight = (std::f64::consts::PI * n / self.strip_m).cos().powi(2);
            Some((2.0 * p.y / self.width_m, weight))
        } else {
            None
        }
    }

    /// Picking includes the air channel, but excludes the missing corner of
    /// the L-shaped footprint. No phantom bounding-box region is selected.
    pub fn contains_footprint(&self, point: Point) -> bool {
        let p = self.local(point);
        let tol = self.coordinate_tolerance();
        let x = self.length_m * 0.5;
        let y = self.width_m * 0.5 + self.wall_m;
        if let Some(bend) = &self.bend {
            let incoming = p.x >= -x - tol && p.x <= x + y + tol && p.y.abs() <= y + tol;
            let outgoing = p.x >= x - y - tol
                && p.x <= x + y + tol
                && p.y >= y - tol
                && p.y <= bend.length_m + tol;
            incoming || outgoing
        } else {
            p.x.abs() <= x + tol && p.y.abs() <= y + tol
        }
    }

    /// Actual outer polygon: four vertices for a straight guide, six for a bend.
    pub fn outline(&self) -> Vec<Point> {
        let x = self.length_m * 0.5;
        let y = self.width_m * 0.5 + self.wall_m;
        let corners = if let Some(bend) = &self.bend {
            vec![
                Point { x: -x, y: -y },
                Point { x: x + y, y: -y },
                Point {
                    x: x + y,
                    y: bend.length_m,
                },
                Point {
                    x: x - y,
                    y: bend.length_m,
                },
                Point { x: x - y, y },
                Point { x: -x, y },
            ]
        } else {
            vec![
                Point { x: -x, y: -y },
                Point { x, y: -y },
                Point { x, y },
                Point { x: -x, y },
            ]
        };
        corners.into_iter().map(|p| self.world(p)).collect()
    }
}

impl Reflector {
    pub fn local(&self, point: Point) -> Point {
        local(point, self.center, self.angle_deg)
    }
    pub fn world(&self, point: Point) -> Point {
        world(point, self.center, self.angle_deg)
    }

    pub fn contains(&self, point: Point) -> bool {
        let p = self.local(point);
        match &self.shape {
            Shape::Segment {
                length_m,
                thickness_m,
            } => {
                segment_distance(
                    p,
                    Point {
                        x: -length_m * 0.5,
                        y: 0.0,
                    },
                    Point {
                        x: length_m * 0.5,
                        y: 0.0,
                    },
                ) <= thickness_m * 0.5
            }
            Shape::Rectangle { width_m, height_m } => {
                p.x.abs() <= width_m * 0.5 && p.y.abs() <= height_m * 0.5
            }
            Shape::Circle { radius_m } => p.x.hypot(p.y) <= *radius_m,
            Shape::Polyline {
                points,
                thickness_m,
            } => points
                .windows(2)
                .any(|w| segment_distance(p, w[0], w[1]) <= thickness_m * 0.5),
        }
    }

    pub(crate) fn local_bounds(&self) -> (Point, Point) {
        let (min_x, max_x, min_y, max_y) = match &self.shape {
            Shape::Segment {
                length_m,
                thickness_m,
            } => {
                let x = (length_m + thickness_m) * 0.5;
                (-x, x, -thickness_m * 0.5, thickness_m * 0.5)
            }
            Shape::Rectangle { width_m, height_m } => (
                -width_m * 0.5,
                width_m * 0.5,
                -height_m * 0.5,
                height_m * 0.5,
            ),
            Shape::Circle { radius_m } => (-radius_m, *radius_m, -radius_m, *radius_m),
            Shape::Polyline {
                points,
                thickness_m,
            } => {
                let r = thickness_m * 0.5;
                let mut bounds = (
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                );
                for p in points {
                    bounds.0 = bounds.0.min(p.x - r);
                    bounds.1 = bounds.1.max(p.x + r);
                    bounds.2 = bounds.2.min(p.y - r);
                    bounds.3 = bounds.3.max(p.y + r);
                }
                bounds
            }
        };
        (Point { x: min_x, y: min_y }, Point { x: max_x, y: max_y })
    }
}

impl Scene {
    /// Маска строится только после `validate`; край сетки замкнут PEC за CPML.
    pub fn pec_mask(&self, grid: GridSpec) -> Vec<bool> {
        let mut mask = vec![false; grid.len()];
        for y in 0..grid.ny {
            for x in 0..grid.nx {
                let point = grid.point(x, y);
                mask[grid.index(x, y)] = x == 0
                    || y == 0
                    || x + 1 == grid.nx
                    || y + 1 == grid.ny
                    || self.sources.iter().any(|s| s.is_pec(point))
                    || self.reflectors.iter().any(|r| r.contains(point));
            }
        }
        mask
    }
}
