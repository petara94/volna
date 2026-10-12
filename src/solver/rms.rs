use crate::{Error, Result};

/// LAPACK-style scaled sum of squares per cell; storage is O(N), not O(N*T).
#[cfg_attr(test, derive(Clone, Debug, PartialEq))]
pub(super) struct Rms {
    scale: Vec<f64>,
    squares: Vec<f64>,
    last: Vec<f64>,
    count: u64,
    window_steps: u64,
    completed: bool,
}

impl Rms {
    pub(super) fn new(len: usize, window_s: f64, dt: f64) -> Result<Self> {
        let count = (window_s / dt).ceil();
        if !window_s.is_finite()
            || window_s <= 0.0
            || !dt.is_finite()
            || dt <= 0.0
            || !count.is_finite()
            || !(1.0..=4_503_599_627_370_496.0).contains(&count)
        {
            return Err(Error::Invalid(
                "окно RMS должно содержать от 1 до 2^52 физических шагов".into(),
            ));
        }
        Ok(Self {
            scale: vec![0.0; len],
            squares: vec![0.0; len],
            last: vec![0.0; len],
            count: 0,
            window_steps: count as u64,
            completed: false,
        })
    }

    pub(super) fn push(&mut self, values: &[f64]) -> Result<()> {
        if values.len() != self.scale.len() {
            return Err(Error::Numerical("несовместимый размер RMS".into()));
        }
        for ((scale, squares), &value) in self.scale.iter_mut().zip(&mut self.squares).zip(values) {
            if !value.is_finite() {
                return Err(Error::Numerical(
                    "неконечное поле при накоплении RMS".into(),
                ));
            }
            let x = value.abs();
            if x > *scale {
                *squares = 1.0 + *squares * (*scale / x).powi(2);
                *scale = x;
            } else if x != 0.0 {
                *squares += (x / *scale).powi(2);
            }
        }
        self.count += 1;
        if self.count == self.window_steps {
            for ((result, &scale), &squares) in
                self.last.iter_mut().zip(&self.scale).zip(&self.squares)
            {
                *result = scale * (squares / self.count as f64).sqrt();
                if !result.is_finite() {
                    return Err(Error::Numerical("переполнение RMS".into()));
                }
            }
            self.scale.fill(0.0);
            self.squares.fill(0.0);
            self.count = 0;
            self.completed = true;
        }
        Ok(())
    }

    /// Begin a new measurement segment without allocating field-sized storage.
    pub(super) fn reset(&mut self) {
        self.scale.fill(0.0);
        self.squares.fill(0.0);
        self.last.fill(0.0);
        self.count = 0;
        self.completed = false;
    }

    pub(super) fn last(&self) -> Option<&[f64]> {
        self.completed.then_some(&self.last)
    }
    pub(super) fn progress(&self) -> f64 {
        self.count as f64 / self.window_steps as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_reuses_storage_and_retains_window_length() {
        let mut rms = Rms::new(2, 3.0, 1.0).unwrap();
        for _ in 0..4 {
            rms.push(&[2.0, 4.0]).unwrap();
        }
        assert!(rms.last().is_some());
        assert_eq!(rms.count, 1);
        let pointers = (rms.scale.as_ptr(), rms.squares.as_ptr(), rms.last.as_ptr());
        let capacities = (
            rms.scale.capacity(),
            rms.squares.capacity(),
            rms.last.capacity(),
        );
        rms.reset();
        assert_eq!(
            pointers,
            (rms.scale.as_ptr(), rms.squares.as_ptr(), rms.last.as_ptr())
        );
        assert_eq!(
            capacities,
            (
                rms.scale.capacity(),
                rms.squares.capacity(),
                rms.last.capacity()
            )
        );
        assert_eq!(rms.window_steps, 3);
        assert_eq!(rms.count, 0);
        assert!(rms.last().is_none());
        assert!(
            rms.scale
                .iter()
                .chain(&rms.squares)
                .chain(&rms.last)
                .all(|&x| x == 0.0)
        );
        rms.push(&[3.0, 5.0]).unwrap();
        rms.push(&[3.0, 5.0]).unwrap();
        assert!(rms.last().is_none());
        rms.push(&[3.0, 5.0]).unwrap();
        assert_eq!(rms.last(), Some([3.0, 5.0].as_slice()));
    }
}
