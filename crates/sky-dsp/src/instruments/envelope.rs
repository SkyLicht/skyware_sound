// SPDX-License-Identifier: GPL-3.0-or-later
//! ADSR envelope.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Linear attack, exponential decay and release.
#[derive(Clone, Copy, Debug)]
pub struct Adsr {
    stage: Stage,
    level: f32,
    attack_step: f32,
    decay_coeff: f32,
    sustain: f32,
    release_coeff: f32,
}

impl Default for Adsr {
    fn default() -> Self {
        Self {
            stage: Stage::Idle,
            level: 0.0,
            attack_step: 1.0,
            decay_coeff: 0.999,
            sustain: 1.0,
            release_coeff: 0.999,
        }
    }
}

fn exp_coeff(ms: f32, sample_rate: f32) -> f32 {
    // Reaches -60 dB of the remaining distance after `ms`.
    let samples = (ms * 0.001 * sample_rate).max(1.0);
    (-6.907_755 / samples).exp()
}

impl Adsr {
    pub fn set(
        &mut self,
        attack_ms: f32,
        decay_ms: f32,
        sustain: f32,
        release_ms: f32,
        sample_rate: f32,
    ) {
        self.attack_step = 1.0 / (attack_ms * 0.001 * sample_rate).max(1.0);
        self.decay_coeff = exp_coeff(decay_ms, sample_rate);
        self.sustain = sustain.clamp(0.0, 1.0);
        self.release_coeff = exp_coeff(release_ms, sample_rate);
    }

    /// Starts the attack from the current level, so retriggers do not click.
    pub fn trigger(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
    }

    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    pub fn is_released(&self) -> bool {
        matches!(self.stage, Stage::Release | Stage::Idle)
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    #[inline]
    pub fn tick(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = self.sustain + (self.level - self.sustain) * self.decay_coeff;
                if (self.level - self.sustain).abs() < 1e-4 {
                    self.level = self.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = self.sustain,
            Stage::Release => {
                self.level *= self.release_coeff;
                if self.level < 1e-5 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        if self.sustain <= 0.0 && self.stage == Stage::Sustain {
            self.reset();
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adsr_runs_through_its_stages() {
        let mut env = Adsr::default();
        env.set(1.0, 10.0, 0.5, 10.0, 48_000.0);
        env.trigger();
        let mut max: f32 = 0.0;
        for _ in 0..48 {
            max = max.max(env.tick());
        }
        assert!((max - 1.0).abs() < 1e-6);
        for _ in 0..4_800 {
            env.tick();
        }
        assert_eq!(env.level(), 0.5);
        env.release();
        for _ in 0..4_800 {
            env.tick();
        }
        assert!(!env.is_active());
    }
}
