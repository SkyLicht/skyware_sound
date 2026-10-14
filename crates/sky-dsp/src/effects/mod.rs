// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in effects.

use std::any::Any;

use serde::{Deserialize, Serialize};

use crate::params::ParamSpec;

mod dynamics;
mod space;
mod tone;

pub use dynamics::{Compressor, Limiter};
pub use space::{Delay, Reverb};
pub use tone::{Distortion, Eq, Filter, Gain};

/// An insert effect. `process` works in place on one block of planar stereo.
pub trait Effect: Send + 'static {
    /// Sets a parameter by its spec name. Returns false for unknown names.
    /// Called on the control thread before the effect goes live.
    fn set_param(&mut self, name: &str, value: f32) -> bool;
    /// Processes a block in place. `left` and `right` have the same length,
    /// at most [`crate::MAX_BLOCK`] frames. Must not allocate.
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);
    /// Clears all internal state (tails, envelopes).
    fn reset(&mut self);
    /// Takes over the running state of `previous` when it is the same kind
    /// of effect, so a graph rebuild does not cut off tails. Swaps buffers,
    /// never allocates. Returns false when the types differ.
    fn adopt(&mut self, previous: &mut dyn Effect) -> bool;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Implements `adopt` and `as_any_mut` by swapping the named state fields.
macro_rules! effect_state {
    ($($field:ident),*) => {
        fn adopt(&mut self, previous: &mut dyn $crate::effects::Effect) -> bool {
            match previous.as_any_mut().downcast_mut::<Self>() {
                Some(_prev) => {
                    $(std::mem::swap(&mut self.$field, &mut _prev.$field);)*
                    true
                }
                None => false,
            }
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    };
}
pub(crate) use effect_state;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EffectType {
    Gain,
    Filter,
    Eq,
    Compressor,
    Limiter,
    Delay,
    Reverb,
    Distortion,
}

impl EffectType {
    pub const ALL: [EffectType; 8] = [
        EffectType::Gain,
        EffectType::Filter,
        EffectType::Eq,
        EffectType::Compressor,
        EffectType::Limiter,
        EffectType::Delay,
        EffectType::Reverb,
        EffectType::Distortion,
    ];

    /// The stable name used in files and MCP calls.
    pub fn name(self) -> &'static str {
        match self {
            EffectType::Gain => "gain",
            EffectType::Filter => "filter",
            EffectType::Eq => "eq",
            EffectType::Compressor => "compressor",
            EffectType::Limiter => "limiter",
            EffectType::Delay => "delay",
            EffectType::Reverb => "reverb",
            EffectType::Distortion => "distortion",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EffectType::Gain => "Gain",
            EffectType::Filter => "Filter",
            EffectType::Eq => "3-band EQ",
            EffectType::Compressor => "Compressor",
            EffectType::Limiter => "Limiter",
            EffectType::Delay => "Delay",
            EffectType::Reverb => "Reverb",
            EffectType::Distortion => "Distortion",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            EffectType::Gain => "Volume trim and stereo width",
            EffectType::Filter => "Resonant low-pass, high-pass or band-pass filter",
            EffectType::Eq => "Low shelf, mid peak and high shelf",
            EffectType::Compressor => "Feed-forward compressor with soft knee",
            EffectType::Limiter => "Peak limiter that keeps output under the ceiling",
            EffectType::Delay => "Stereo or ping-pong echo with damped feedback",
            EffectType::Reverb => "Algorithmic room reverb",
            EffectType::Distortion => "Soft, hard or wavefolding saturation",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(name.trim()))
    }

    pub fn params(self) -> &'static [ParamSpec] {
        match self {
            EffectType::Gain => tone::GAIN_PARAMS,
            EffectType::Filter => tone::FILTER_PARAMS,
            EffectType::Eq => tone::EQ_PARAMS,
            EffectType::Compressor => dynamics::COMPRESSOR_PARAMS,
            EffectType::Limiter => dynamics::LIMITER_PARAMS,
            EffectType::Delay => space::DELAY_PARAMS,
            EffectType::Reverb => space::REVERB_PARAMS,
            EffectType::Distortion => tone::DISTORTION_PARAMS,
        }
    }

    /// A new instance with default parameters.
    pub fn create(self, sample_rate: f32) -> Box<dyn Effect> {
        self.create_with(sample_rate, std::iter::empty())
    }

    /// A new instance with the given parameter values (others default),
    /// with smoothers settled on those values so nothing glides at start.
    pub fn create_with<'a>(
        self,
        sample_rate: f32,
        values: impl IntoIterator<Item = (&'a str, f32)>,
    ) -> Box<dyn Effect> {
        let mut effect: Box<dyn Effect> = match self {
            EffectType::Gain => Box::new(Gain::new(sample_rate)),
            EffectType::Filter => Box::new(Filter::new(sample_rate)),
            EffectType::Eq => Box::new(Eq::new(sample_rate)),
            EffectType::Compressor => Box::new(Compressor::new(sample_rate)),
            EffectType::Limiter => Box::new(Limiter::new(sample_rate)),
            EffectType::Delay => Box::new(Delay::new(sample_rate)),
            EffectType::Reverb => Box::new(Reverb::new(sample_rate)),
            EffectType::Distortion => Box::new(Distortion::new(sample_rate)),
        };
        for spec in self.params() {
            effect.set_param(spec.name, spec.default);
        }
        for (name, value) in values {
            effect.set_param(name, value);
        }
        effect.reset();
        effect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_block(freq: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin() * 0.5)
            .collect()
    }

    #[test]
    fn every_effect_accepts_its_own_params_and_stays_finite() {
        for t in EffectType::ALL {
            let mut fx = t.create(48_000.0);
            for spec in t.params() {
                assert!(
                    fx.set_param(spec.name, spec.max),
                    "{} {}",
                    t.name(),
                    spec.name
                );
                assert!(
                    fx.set_param(spec.name, spec.min),
                    "{} {}",
                    t.name(),
                    spec.name
                );
                assert!(fx.set_param(spec.name, spec.default));
            }
            assert!(!fx.set_param("no_such_param", 1.0));
            let mut l = sine_block(440.0, 512);
            let mut r = l.clone();
            for _ in 0..200 {
                fx.process(&mut l, &mut r);
            }
            assert!(l.iter().chain(&r).all(|s| s.is_finite()), "{}", t.name());
            assert_eq!(EffectType::from_name(t.name()), Some(t));
        }
    }

    #[test]
    fn adopt_moves_state_only_between_same_types() {
        let mut a = EffectType::Delay.create(48_000.0);
        let mut b = EffectType::Delay.create(48_000.0);
        let mut c = EffectType::Reverb.create(48_000.0);
        assert!(b.adopt(a.as_mut()));
        assert!(!c.adopt(b.as_mut()));
    }

    #[test]
    fn delay_tail_survives_adopt() {
        let params = [("time", 10.0), ("mix", 1.0)];
        let mut first = EffectType::Delay.create_with(48_000.0, params);
        let mut l = vec![0.0; 512];
        let mut r = vec![0.0; 512];
        l[0] = 1.0;
        r[0] = 1.0;
        first.process(&mut l, &mut r);
        // The echo of the impulse at 10 ms (480 samples) is inside this block;
        // the second echo lands in the next block, after the swap.
        let mut second = EffectType::Delay.create_with(48_000.0, params);
        assert!(second.adopt(first.as_mut()));
        let mut l2 = vec![0.0; 512];
        let mut r2 = vec![0.0; 512];
        second.process(&mut l2, &mut r2);
        assert!(l2.iter().any(|s| s.abs() > 0.01), "tail lost across adopt");
    }
}
