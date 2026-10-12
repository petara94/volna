mod geometry;
mod validation;

use serde::{Deserialize, Serialize};

pub const C0: f64 = 299_792_458.0;
pub const MU0: f64 = 1.256_637_061_27e-6;
pub const EPS0: f64 = 1.0 / (MU0 * C0 * C0);
pub const SCHEMA_VERSION: u32 = 1;
pub const MEMORY_LIMIT: usize = 512 * 1024 * 1024;
pub const HISTORY_LIMIT: usize = 100_000;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Absolute pose only: geometry sizes, signal settings and object membership
/// cannot be changed through this payload.
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectPose {
    Source {
        id: u64,
        center: Point,
        angle_deg: f64,
    },
    Reflector {
        id: u64,
        center: Point,
        angle_deg: f64,
    },
    Probe {
        id: u64,
        position: Point,
    },
}

impl ObjectPose {
    pub fn id(&self) -> u64 {
        match self {
            Self::Source { id, .. } | Self::Reflector { id, .. } | Self::Probe { id, .. } => *id,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Domain {
    pub width_m: f64,
    pub height_m: f64,
    pub cell_m: f64,
    pub cfl: f64,
    pub pml_cells: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Profile {
    Uniform,
    Fundamental,
    Gaussian { width: f64 },
    Formula { expression: String },
    Table { samples: Vec<[f64; 2]> },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Signal {
    Continuous {
        ramp_periods: f64,
    },
    Gaussian {
        center_s: f64,
        width_s: f64,
    },
    Formula {
        expression: String,
    },
    Table {
        samples: Vec<[f64; 2]>,
        periodic: bool,
    },
}

/// A sharp right-angle turn from the incoming local +X arm into local +Y.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuideBend {
    pub length_m: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub center: Point,
    pub angle_deg: f64,
    pub width_m: f64,
    pub length_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend: Option<GuideBend>,
    pub wall_m: f64,
    pub source_offset_m: f64,
    pub strip_m: f64,
    pub closed_back: bool,
    pub current_a_m2: f64,
    pub frequency_hz: f64,
    pub fmax_hz: f64,
    pub phase_rad: f64,
    pub delay_s: f64,
    pub signal: Signal,
    pub aperture: Profile,
    pub phase_profile: Profile,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Shape {
    Segment {
        length_m: f64,
        thickness_m: f64,
    },
    Rectangle {
        width_m: f64,
        height_m: f64,
    },
    Circle {
        radius_m: f64,
    },
    Polyline {
        points: Vec<Point>,
        thickness_m: f64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Reflector {
    pub id: u64,
    pub name: String,
    pub center: Point,
    pub angle_deg: f64,
    pub shape: Shape,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub id: u64,
    pub name: String,
    pub position: Point,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub schema_version: u32,
    pub domain: Domain,
    pub sources: Vec<Source>,
    pub reflectors: Vec<Reflector>,
    pub probes: Vec<Probe>,
    pub rms_window_s: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct GridSpec {
    pub nx: usize,
    pub ny: usize,
    pub pml: usize,
    pub dx: f64,
    pub dt: f64,
}

impl GridSpec {
    pub fn len(self) -> usize {
        self.nx * self.ny
    }
    pub fn is_empty(self) -> bool {
        self.nx == 0 || self.ny == 0
    }
    pub fn index(self, x: usize, y: usize) -> usize {
        y * self.nx + x
    }
    pub fn point(self, x: usize, y: usize) -> Point {
        Point {
            x: (x as f64 - self.pml as f64) * self.dx,
            y: (y as f64 - self.pml as f64) * self.dx,
        }
    }
    pub fn nearest(self, point: Point) -> (usize, usize) {
        let x = (point.x / self.dx + self.pml as f64)
            .round()
            .clamp(0.0, (self.nx - 1) as f64) as usize;
        let y = (point.y / self.dx + self.pml as f64)
            .round()
            .clamp(0.0, (self.ny - 1) as f64) as usize;
        (x, y)
    }
}

impl Default for Source {
    fn default() -> Self {
        Self {
            id: 1,
            name: "Волновод 1".into(),
            enabled: true,
            center: Point { x: 0.080, y: 0.150 },
            angle_deg: 0.0,
            width_m: 0.022,
            length_m: 0.080,
            bend: None,
            wall_m: 0.002,
            source_offset_m: 0.011,
            strip_m: 0.003,
            closed_back: true,
            current_a_m2: 1.0,
            frequency_hz: 10e9,
            fmax_hz: 12e9,
            phase_rad: 0.0,
            delay_s: 0.0,
            signal: Signal::Continuous { ramp_periods: 4.0 },
            aperture: Profile::Fundamental,
            phase_profile: Profile::Formula {
                expression: "0".into(),
            },
        }
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            domain: Domain {
                width_m: 0.4,
                height_m: 0.3,
                cell_m: 0.001,
                cfl: 0.9,
                pml_cells: 20,
            },
            sources: vec![Source::default()],
            reflectors: vec![],
            probes: vec![Probe {
                id: 2,
                name: "Зонд 1".into(),
                position: Point { x: 0.2, y: 0.15 },
            }],
            rms_window_s: 0.5e-9,
        }
    }
}

impl Scene {
    /// An open educational interference layout, separate from the numerical
    /// baseline. Facet angles are approximate geometric-ray orientations toward
    /// (235 mm, 150 mm), not a claim of an ideal wave focus or power gain.
    pub fn interference_demo() -> Self {
        // Coordinates in millimetres, angles in degrees.
        let ribs = [
            (286.0, 62.0, 46.083),
            (318.0, 99.0, 66.993),
            (334.0, 134.0, 83.272),
            (334.0, 166.0, 96.728),
            (318.0, 201.0, 113.007),
            (286.0, 238.0, 133.917),
        ];
        Self {
            reflectors: ribs
                .into_iter()
                .enumerate()
                .map(|(index, (x_mm, y_mm, angle_deg))| Reflector {
                    id: index as u64 + 3,
                    name: format!("Ребро {}", index + 1),
                    center: Point {
                        x: x_mm / 1000.0,
                        y: y_mm / 1000.0,
                    },
                    angle_deg,
                    // Short separated facets leave gaps, with unobstructed escape
                    // paths above and below the shared observation region.
                    shape: Shape::Segment {
                        length_m: 0.024,
                        thickness_m: 0.002,
                    },
                })
                .collect(),
            probes: vec![
                Probe {
                    id: 2,
                    name: "Общая область".into(),
                    position: Point { x: 0.235, y: 0.15 },
                },
                Probe {
                    id: 9,
                    name: "Выше центра".into(),
                    position: Point { x: 0.235, y: 0.17 },
                },
                Probe {
                    id: 10,
                    name: "Ниже центра".into(),
                    position: Point { x: 0.235, y: 0.13 },
                },
            ],
            ..Self::default()
        }
    }

    pub fn pose(&self, id: u64) -> Option<ObjectPose> {
        if let Some(source) = self.sources.iter().find(|s| s.id == id) {
            return Some(ObjectPose::Source {
                id,
                center: source.center,
                angle_deg: source.angle_deg,
            });
        }
        if let Some(reflector) = self.reflectors.iter().find(|r| r.id == id) {
            return Some(ObjectPose::Reflector {
                id,
                center: reflector.center,
                angle_deg: reflector.angle_deg,
            });
        }
        self.probes
            .iter()
            .find(|p| p.id == id)
            .map(|p| ObjectPose::Probe {
                id,
                position: p.position,
            })
    }

    /// Clone the accepted scene and update only the object's pose. Unknown IDs,
    /// mismatched object kinds and invalid resulting geometry are explicit errors.
    pub fn with_pose(&self, pose: &ObjectPose) -> crate::Result<Self> {
        let mut candidate = self.clone();
        let missing = || {
            crate::Error::Invalid(format!(
                "объект {} не найден или имеет другой тип",
                pose.id()
            ))
        };
        match *pose {
            ObjectPose::Source {
                id,
                center,
                angle_deg,
            } => {
                let source = candidate
                    .sources
                    .iter_mut()
                    .find(|s| s.id == id)
                    .ok_or_else(missing)?;
                source.center = center;
                source.angle_deg = angle_deg;
            }
            ObjectPose::Reflector {
                id,
                center,
                angle_deg,
            } => {
                let reflector = candidate
                    .reflectors
                    .iter_mut()
                    .find(|r| r.id == id)
                    .ok_or_else(missing)?;
                reflector.center = center;
                reflector.angle_deg = angle_deg;
            }
            ObjectPose::Probe { id, position } => {
                candidate
                    .probes
                    .iter_mut()
                    .find(|p| p.id == id)
                    .ok_or_else(missing)?
                    .position = position;
            }
        }
        candidate.validate()?;
        Ok(candidate)
    }

    pub fn next_id(&self) -> u64 {
        self.sources
            .iter()
            .map(|x| x.id)
            .chain(self.reflectors.iter().map(|x| x.id))
            .chain(self.probes.iter().map(|x| x.id))
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }
}
