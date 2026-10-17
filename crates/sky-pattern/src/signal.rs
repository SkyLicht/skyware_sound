// SPDX-License-Identifier: GPL-3.0-or-later
//! Continuous signals such as Strudel's `sine` and `rand`: values that change
//! smoothly over time rather than in steps. A signal has no events of its
//! own; it is read at the start of each event that uses it, as in
//! `.lpf(sine.range(200, 2000).slow(4))`, or cut into steps with
//! `.segment(n)`.

use crate::query::{hash, unit};

/// The shape of a signal over one cycle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Wave {
    /// `0.5 + 0.5 sin(2πt)`.
    Sine,
    /// `0.5 + 0.5 cos(2πt)`.
    Cosine,
    /// Rises from 0 to 1 over each cycle.
    Saw,
    /// Falls from 1 to 0 over each cycle.
    Isaw,
    /// Rises over the first half of each cycle and falls over the second.
    Tri,
    /// 0 for the first half of each cycle, 1 for the second.
    Square,
    /// A random value for each moment, the same for the same seed.
    Rand,
    /// Smooth random values: random at each whole cycle, eased between.
    Perlin,
    /// Random whole numbers from 0 to `n - 1`.
    Irand(u32),
}

/// What happens to a signal's value after its wave.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Step {
    /// `v * mul + add`, as `range`, `add` and `mul` do.
    Affine {
        mul: f64,
        add: f64,
    },
    /// From 0..1 onto `lo..hi` along an exponential curve (`rangex`).
    Exp {
        lo: f64,
        hi: f64,
    },
    Round,
    Floor,
}

/// The most steps one signal can have.
pub(crate) const MAX_STEPS: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Signal {
    pub wave: Wave,
    /// The wave is read at `t * scale + offset` for local time `t`.
    pub scale: f64,
    pub offset: f64,
    pub steps: Vec<Step>,
    /// Keeps two random signals in one pattern apart.
    pub salt: u64,
}

impl Signal {
    pub fn new(wave: Wave, salt: u64) -> Self {
        Signal {
            wave,
            scale: 1.0,
            offset: 0.0,
            steps: Vec::new(),
            salt,
        }
    }

    /// The signal from -1 to 1 instead of 0 to 1, such as `sine2`.
    pub fn bipolar(mut self) -> Self {
        self.steps.push(Step::Affine {
            mul: 2.0,
            add: -1.0,
        });
        self
    }

    /// The signal `factor` times faster.
    pub fn fast(mut self, factor: f64) -> Self {
        self.scale *= factor;
        self
    }

    /// The signal shifted earlier by `cycles`.
    pub fn early(mut self, cycles: f64) -> Self {
        self.offset += cycles * self.scale;
        self
    }

    /// The value at local time `t`.
    pub fn at(&self, t: f64, seed: u64) -> f64 {
        let x = t * self.scale + self.offset;
        let phase = x - x.floor();
        let rand_at = |x: f64| {
            // Times on a 2^-20 grid, so nearby reads agree.
            let tick = (x * f64::from(1 << 20)).floor() as i64;
            unit(hash(seed, &[self.salt, tick.cast_unsigned()]))
        };
        let mut v = match self.wave {
            Wave::Sine => 0.5 + 0.5 * (std::f64::consts::TAU * x).sin(),
            Wave::Cosine => 0.5 + 0.5 * (std::f64::consts::TAU * x).cos(),
            Wave::Saw => phase,
            Wave::Isaw => 1.0 - phase,
            Wave::Tri => {
                if phase < 0.5 {
                    phase * 2.0
                } else {
                    2.0 - phase * 2.0
                }
            }
            Wave::Square => {
                if phase < 0.5 {
                    0.0
                } else {
                    1.0
                }
            }
            Wave::Rand => rand_at(x),
            Wave::Irand(n) => (rand_at(x) * f64::from(n)).floor(),
            Wave::Perlin => {
                let a = rand_at(x.floor());
                let b = rand_at(x.floor() + 1.0);
                // Smootherstep easing.
                let s = phase * phase * phase * (phase * (phase * 6.0 - 15.0) + 10.0);
                a + (b - a) * s
            }
        };
        for step in &self.steps {
            v = match *step {
                Step::Affine { mul, add } => v * mul + add,
                Step::Exp { lo, hi } => lo * (hi / lo).powf(v),
                Step::Round => v.round(),
                Step::Floor => v.floor(),
            };
        }
        if v.is_finite() { v } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waves() {
        let at = |w, t| Signal::new(w, 0).at(t, 0);
        assert!((at(Wave::Sine, 0.0) - 0.5).abs() < 1e-12);
        assert!((at(Wave::Sine, 0.25) - 1.0).abs() < 1e-12);
        assert!((at(Wave::Cosine, 0.0) - 1.0).abs() < 1e-12);
        assert_eq!(at(Wave::Saw, 1.25), 0.25);
        assert_eq!(at(Wave::Isaw, 0.25), 0.75);
        assert_eq!(at(Wave::Tri, 0.25), 0.5);
        assert_eq!(at(Wave::Tri, 0.75), 0.5);
        assert_eq!(at(Wave::Square, 0.25), 0.0);
        assert_eq!(at(Wave::Square, 0.75), 1.0);
        for i in 0..100 {
            let t = f64::from(i) / 7.0;
            let r = at(Wave::Rand, t);
            assert!((0.0..1.0).contains(&r));
            assert_eq!(r, at(Wave::Rand, t));
            let n = at(Wave::Irand(4), t);
            assert!(n == n.floor() && (0.0..4.0).contains(&n));
            assert!((0.0..=1.0).contains(&at(Wave::Perlin, t)));
        }
    }

    #[test]
    fn time_and_steps() {
        let s = Signal::new(Wave::Saw, 0).fast(0.5);
        assert_eq!(s.at(1.0, 0), 0.5);
        let s = Signal::new(Wave::Saw, 0).early(0.25);
        assert_eq!(s.at(0.0, 0), 0.25);
        let mut s = Signal::new(Wave::Saw, 0);
        s.steps.push(Step::Affine {
            mul: 1800.0,
            add: 200.0,
        });
        assert_eq!(s.at(0.5, 0), 1100.0);
        let s = Signal::new(Wave::Square, 0).bipolar();
        assert_eq!(s.at(0.0, 0), -1.0);
        let mut s = Signal::new(Wave::Saw, 0);
        s.steps.push(Step::Exp {
            lo: 100.0,
            hi: 10_000.0,
        });
        assert!((s.at(0.5, 0) - 1000.0).abs() < 1e-9);
    }
}
