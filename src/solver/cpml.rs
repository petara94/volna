//! Unsplit recursive convolution, Schneider §11.6, (11.126–11.132).
//! `sigma_rate = sigma_e/eps0 = sigma_h/mu0`, `alpha_rate` likewise,
//! both in s^-1. `kappa`, `a`, `b` are dimensionless. Memory has the
//! same units as the spatial derivative, NOT the corresponding field.

use crate::model::{C0, GridSpec};
use crate::{Error, Result};

#[derive(Clone, Copy, Debug)]
pub(super) struct Coefficient {
    pub(super) inverse_kappa: f64,
    pub(super) a: f64,
    pub(super) b: f64,
}

impl Coefficient {
    const VACUUM: Self = Self {
        inverse_kappa: 1.0,
        a: 0.0,
        b: 1.0,
    };

    fn from_rates(sigma_rate: f64, alpha_rate: f64, kappa: f64, dt: f64) -> Result<Self> {
        let rate = sigma_rate / kappa + alpha_rate;
        let exponent = -rate * dt;
        let coefficient = Self {
            inverse_kappa: 1.0 / kappa,
            // exp_m1 retains precision as sigma and dt tend to zero.
            a: if sigma_rate == 0.0 {
                0.0
            } else {
                sigma_rate / (kappa * kappa * rate) * exponent.exp_m1()
            },
            b: exponent.exp(),
        };
        if ![
            sigma_rate,
            alpha_rate,
            kappa,
            dt,
            rate,
            exponent,
            coefficient.a,
            coefficient.b,
        ]
        .iter()
        .all(|v| v.is_finite())
            || sigma_rate < 0.0
            || alpha_rate < 0.0
            || kappa < 1.0
            || dt <= 0.0
        {
            return Err(Error::Numerical(
                "неконечный или недопустимый коэффициент CPML".into(),
            ));
        }
        Ok(coefficient)
    }

    #[inline]
    pub(super) fn apply(self, derivative: f64, memory: &mut f64) -> f64 {
        *memory = self.b * *memory + self.a * derivative;
        self.inverse_kappa * derivative + *memory
    }
}

pub(super) struct Axis {
    pub(super) electric: Vec<Coefficient>,
    pub(super) magnetic: Vec<Coefficient>,
}

impl Axis {
    pub(super) fn new(n: usize, grid: GridSpec) -> Result<Self> {
        let profile = |position: f64| -> Result<Coefficient> {
            if grid.pml == 0 {
                return Ok(Coefficient::VACUUM);
            }
            let left = grid.pml as f64;
            let right = (n - 1 - grid.pml) as f64;
            let depth = ((left - position).max(position - right) / grid.pml as f64).max(0.0);
            if depth == 0.0 {
                return Ok(Coefficient::VACUUM);
            }
            // Integral of cubic sigma gives the normal-incidence design
            // round-trip amplitude R=1e-10; this is NOT measured reflection.
            let sigma_max = -4.0 * C0 * 1e-10_f64.ln() / (2.0 * grid.pml as f64 * grid.dx);
            let sigma_rate = sigma_max * depth.powi(3);
            let alpha_rate = 0.05 * C0 / grid.dx * (1.0 - depth);
            Coefficient::from_rates(sigma_rate, alpha_rate, 1.0, grid.dt)
        };
        let electric = (0..n).map(|i| profile(i as f64)).collect::<Result<_>>()?;
        // The last H element is deliberately unused; avoid sampling beyond
        // the outer conductor (which would make alpha negative).
        let magnetic = (0..n)
            .map(|i| {
                if i + 1 < n {
                    profile(i as f64 + 0.5)
                } else {
                    Ok(Coefficient::VACUUM)
                }
            })
            .collect::<Result<_>>()?;
        Ok(Self { electric, magnetic })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_derivative_matches_closed_form_convolution() {
        let c = Coefficient::from_rates(2.0e10, 1.0e9, 2.0, 1e-12).unwrap();
        let mut memory = 0.0;
        for n in 1..=100 {
            let result = c.apply(3.0, &mut memory);
            let exact = 1.5 + 3.0 * c.a * (1.0 - c.b.powi(n)) / (1.0 - c.b);
            assert!((result - exact).abs() < 1e-13);
        }
    }

    #[test]
    fn yee_locations_are_symmetric_and_vacuum_is_identity() {
        let g = GridSpec {
            nx: 81,
            ny: 71,
            pml: 20,
            dx: 0.001,
            dt: 2e-12,
        };
        let axis = Axis::new(g.nx, g).unwrap();
        for i in 0..g.nx {
            assert_eq!(axis.electric[i].a, axis.electric[g.nx - 1 - i].a);
        }
        for i in 0..g.nx - 1 {
            assert_eq!(axis.magnetic[i].a, axis.magnetic[g.nx - 2 - i].a);
        }
        let mut memory = 0.0;
        assert_eq!(axis.electric[20].apply(1.234, &mut memory), 1.234);
        assert_eq!(memory, 0.0);
        assert!(axis.magnetic[19].a < 0.0);
    }
}
