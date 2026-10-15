// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in effects.

use std::any::Any;

use serde::{Deserialize, Serialize};

use crate::params::ParamSpec;

mod duck;
mod dynamics;
mod space;
mod tone;

pub use duck::Duck;
pub use dynamics::{Compressor, Limiter};
pub use space::{Delay, Reverb};
pub use tone::{Distortion, Eq, Filter, Gain};

/// An insert effect. `process` works in place on one block of planar stereo.
pub trait Effect: Send + 'static {
    /// Sets a parameter by its spec name. Returns false for unknown names.
    /// Called on the control thread before the effect goes live.
    fn set_param(&mut self, name: &str, value: f32) -> bool;
    /// Tells a tempo-synced effect the project tempo, in quarter notes per
    /// minute. Called on the control thread before the effect goes live;
    /// effects that ignore tempo keep the default, which does nothing.
    fn set_tempo(&mut self, _bpm: f64) {}
    /// A note starts on the track this effect follows, `offset` frames into
    /// the next block passed to `process`. Only effects whose type
    /// [takes a sidechain](EffectType::takes_sidechain) listen. Must not
    /// allocate.
    fn sidechain_note(&mut self, _offset: usize, _pitch: u8) {}
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
    Duck,
}

impl EffectType {
    pub const ALL: [EffectType; 9] = [
        EffectType::Gain,
        EffectType::Filter,
        EffectType::Eq,
        EffectType::Compressor,
        EffectType::Limiter,
        EffectType::Delay,
        EffectType::Reverb,
        EffectType::Distortion,
        EffectType::Duck,
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
            EffectType::Duck => "duck",
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
            EffectType::Duck => "Duck",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            EffectType::Gain => "Volume trim and stereo width",
            EffectType::Filter => "Resonant low-pass, high-pass or band-pass filter",
            EffectType::Eq => "Low shelf, mid peak and high shelf",
            EffectType::Compressor => "Feed-forward compressor with soft knee",
            EffectType::Limiter => "Peak limiter that keeps output under the ceiling",
            EffectType::Delay => {
                "Stereo or ping-pong echo with damped feedback, free or synced to the tempo"
            }
            EffectType::Reverb => "Algorithmic room reverb with pre-delay and an optional gate",
            EffectType::Distortion => "Soft, hard or wavefolding saturation",
            EffectType::Duck => {
                "Sidechain ducking: dips the volume on each note of another track, \
                 such as the kick, in a tempo-synced curve"
            }
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
            EffectType::Duck => duck::DUCK_PARAMS,
        }
    }

    /// Whether the effect follows another track's notes, which the
    /// project names in the effect slot's sidechain.
    pub fn takes_sidechain(self) -> bool {
        self == EffectType::Duck
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
        self.create_at(sample_rate, 120.0, values)
    }

    /// Like [`create_with`](Self::create_with), for a project at `bpm`, so
    /// tempo-synced effects start at the right time.
    pub fn create_at<'a>(
        self,
        sample_rate: f32,
        bpm: f64,
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
            EffectType::Duck => Box::new(Duck::new(sample_rate)),
        };
        for spec in self.params() {
            effect.set_param(spec.name, spec.default);
        }
        for (name, value) in values {
            effect.set_param(name, value);
        }
        effect.set_tempo(bpm);
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

    /// The frame of the first sample louder than `level` at or after `from`.
    fn first_above(buf: &[f32], from: usize, level: f32) -> Option<usize> {
        buf.iter()
            .skip(from)
            .position(|s| s.abs() > level)
            .map(|i| i + from)
    }

    fn impulse_response(fx: &mut dyn Effect, frames: usize) -> Vec<f32> {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        l[0] = 1.0;
        r[0] = 1.0;
        for (lc, rc) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            fx.process(lc, rc);
        }
        l
    }

    #[test]
    fn synced_delay_follows_the_tempo() {
        let params = [("sync", 9.0), ("mix", 1.0), ("feedback", 0.0)];
        // A quarter note: 500 ms at 120 BPM, 24000 frames.
        let mut at_120 = EffectType::Delay.create_at(48_000.0, 120.0, params);
        let out = impulse_response(at_120.as_mut(), 30_000);
        assert_eq!(first_above(&out, 1, 0.1), Some(24_000));
        // The same setting at 150 BPM is 400 ms; changing the tempo moves it.
        at_120.set_tempo(150.0);
        at_120.reset();
        let out = impulse_response(at_120.as_mut(), 30_000);
        assert_eq!(first_above(&out, 1, 0.1), Some(19_200));
        // Off goes back to the time in milliseconds.
        let mut free = EffectType::Delay.create_at(
            48_000.0,
            150.0,
            [
                ("sync", 0.0),
                ("time", 100.0),
                ("mix", 1.0),
                ("feedback", 0.0),
            ],
        );
        let out = impulse_response(free.as_mut(), 10_000);
        assert_eq!(first_above(&out, 1, 0.1), Some(4_800));
    }

    #[test]
    fn reverb_predelay_holds_back_the_tail() {
        let mut fx = EffectType::Reverb.create_with(48_000.0, [("mix", 1.0), ("predelay", 100.0)]);
        let out = impulse_response(fx.as_mut(), 20_000);
        let onset = first_above(&out, 0, 1e-4).expect("a tail");
        // The shortest comb adds about 25 ms on top of the 100 ms pre-delay.
        assert!((4_800..7_000).contains(&onset), "tail starts at {onset}");
    }

    #[test]
    fn gated_reverb_cuts_the_tail() {
        let rms = |buf: &[f32]| (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt();
        let mut open = EffectType::Reverb.create_with(48_000.0, [("mix", 0.5), ("size", 1.0)]);
        let mut gated = EffectType::Reverb
            .create_with(48_000.0, [("mix", 0.5), ("size", 1.0), ("gate", 150.0)]);
        let open_out = impulse_response(open.as_mut(), 48_000);
        let gated_out = impulse_response(gated.as_mut(), 48_000);
        // Inside the hold the two are alike; after it the gated one is
        // silent while the open one still rings.
        let early = 2_000..6_000;
        assert!(rms(&gated_out[early.clone()]) > 0.5 * rms(&open_out[early]));
        let late = 12_000..20_000;
        assert!(rms(&open_out[late.clone()]) > 1e-3);
        assert!(
            rms(&gated_out[late]) < 1e-5,
            "{}",
            rms(&gated_out[12_000..20_000])
        );
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
