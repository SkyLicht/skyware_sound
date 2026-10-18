// SPDX-License-Identifier: GPL-3.0-or-later
//! Factory presets: named starting points for each instrument and effect.
//!
//! A preset lists only the parameters it changes; the rest keep their
//! defaults, so loading one always gives the same sound.

use crate::{EffectType, InstrumentType};

/// A named set of parameter values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    /// Values that differ from the defaults, by parameter name.
    pub values: &'static [(&'static str, f32)],
}

const fn p(name: &'static str, values: &'static [(&'static str, f32)]) -> Preset {
    Preset { name, values }
}

impl Preset {
    /// The preset's value for a parameter, or `None` where it keeps the default.
    pub fn value(&self, param: &str) -> Option<f32> {
        self.values
            .iter()
            .find(|(n, _)| *n == param)
            .map(|(_, v)| *v)
    }
}

impl EffectType {
    pub fn presets(self) -> &'static [Preset] {
        match self {
            EffectType::Gain => GAIN,
            EffectType::Filter => FILTER,
            EffectType::Eq => EQ,
            EffectType::Compressor => COMPRESSOR,
            EffectType::Limiter => LIMITER,
            EffectType::Delay => DELAY,
            EffectType::Reverb => REVERB,
            EffectType::Distortion => DISTORTION,
            EffectType::Duck => DUCK,
        }
    }
}

impl InstrumentType {
    pub fn presets(self) -> &'static [Preset] {
        match self {
            InstrumentType::Synth => SYNTH,
            InstrumentType::Drums => DRUMS,
            InstrumentType::Sampler => SAMPLER,
            InstrumentType::Bank => BANK,
            InstrumentType::Acid => ACID,
            InstrumentType::Piano => PIANO,
        }
    }
}

const GAIN: &[Preset] = &[
    p("Mono", &[("width", 0.0)]),
    p("Wide", &[("width", 1.6)]),
    p("Trim -6 dB", &[("gain", -6.0)]),
];

const FILTER: &[Preset] = &[
    p("Dark low-pass", &[("cutoff", 800.0)]),
    p(
        "Rumble cut",
        &[("mode", 1.0), ("cutoff", 35.0), ("resonance", 0.1)],
    ),
    p(
        "Thin high-pass",
        &[("mode", 1.0), ("cutoff", 400.0), ("resonance", 0.3)],
    ),
    p(
        "Telephone",
        &[("mode", 2.0), ("cutoff", 1_500.0), ("resonance", 0.5)],
    ),
    p("Squelch", &[("cutoff", 900.0), ("resonance", 0.85)]),
];

const EQ: &[Preset] = &[
    p(
        "Kick punch",
        &[
            ("low_freq", 60.0),
            ("low_gain", 4.0),
            ("mid_freq", 350.0),
            ("mid_gain", -4.0),
            ("mid_q", 1.2),
            ("high_freq", 4_000.0),
            ("high_gain", 2.0),
        ],
    ),
    p(
        "Bass clean-up",
        &[
            ("low_freq", 40.0),
            ("low_gain", -6.0),
            ("mid_freq", 250.0),
            ("mid_gain", -3.0),
            ("mid_q", 1.0),
            ("high_freq", 5_000.0),
            ("high_gain", -4.0),
        ],
    ),
    p(
        "Hat smoothing",
        &[
            ("low_freq", 300.0),
            ("low_gain", -12.0),
            ("high_freq", 9_000.0),
            ("high_gain", -3.0),
        ],
    ),
    p("Bright air", &[("high_freq", 10_000.0), ("high_gain", 4.0)]),
    p(
        "Master polish",
        &[
            ("low_freq", 100.0),
            ("low_gain", 1.5),
            ("mid_freq", 400.0),
            ("mid_gain", -1.5),
            ("high_freq", 10_000.0),
            ("high_gain", 2.0),
        ],
    ),
];

const COMPRESSOR: &[Preset] = &[
    p(
        "Gentle glue",
        &[
            ("threshold", -12.0),
            ("ratio", 2.0),
            ("attack", 30.0),
            ("release", 200.0),
            ("makeup", 2.0),
        ],
    ),
    p(
        "Drum punch",
        &[
            ("threshold", -20.0),
            ("attack", 15.0),
            ("release", 80.0),
            ("knee", 3.0),
            ("makeup", 4.0),
        ],
    ),
    p(
        "Bass control",
        &[("attack", 5.0), ("release", 150.0), ("makeup", 3.0)],
    ),
    p(
        "Squash",
        &[
            ("threshold", -30.0),
            ("ratio", 10.0),
            ("attack", 1.0),
            ("release", 50.0),
            ("knee", 0.0),
            ("makeup", 10.0),
        ],
    ),
];

const LIMITER: &[Preset] = &[
    p("Transparent", &[("release", 200.0)]),
    p("Loud", &[("ceiling", -0.3), ("release", 20.0)]),
    p("Game safe -3 dB", &[("ceiling", -3.0), ("release", 80.0)]),
];

const DELAY: &[Preset] = &[
    p(
        "Slapback",
        &[("time", 90.0), ("feedback", 0.1), ("mix", 0.25)],
    ),
    p(
        "Dotted eighth",
        &[("sync", 7.0), ("feedback", 0.4), ("mix", 0.25)],
    ),
    p(
        "Ping-pong eighths",
        &[("sync", 6.0), ("feedback", 0.45), ("ping_pong", 1.0)],
    ),
    p(
        "Dub echo",
        &[
            ("sync", 10.0),
            ("feedback", 0.7),
            ("mix", 0.35),
            ("damping", 2_500.0),
            ("ping_pong", 1.0),
        ],
    ),
];

const REVERB: &[Preset] = &[
    p(
        "Small room",
        &[
            ("size", 0.3),
            ("damping", 0.6),
            ("mix", 0.18),
            ("predelay", 5.0),
        ],
    ),
    p(
        "Large hall",
        &[
            ("size", 0.9),
            ("damping", 0.4),
            ("mix", 0.3),
            ("predelay", 30.0),
        ],
    ),
    p(
        "Gated snare",
        &[("size", 0.7), ("mix", 0.35), ("gate", 250.0)],
    ),
    p(
        "Industrial space",
        &[
            ("size", 1.0),
            ("damping", 0.2),
            ("mix", 0.4),
            ("predelay", 60.0),
        ],
    ),
];

const DISTORTION: &[Preset] = &[
    p("Warm saturation", &[("drive", 6.0), ("output", -2.0)]),
    p(
        "Hard clip",
        &[("mode", 1.0), ("drive", 18.0), ("output", -6.0)],
    ),
    p(
        "Wavefold",
        &[
            ("mode", 2.0),
            ("drive", 24.0),
            ("mix", 0.7),
            ("output", -6.0),
        ],
    ),
    p(
        "Parallel grit",
        &[
            ("mode", 1.0),
            ("drive", 30.0),
            ("mix", 0.3),
            ("output", -3.0),
        ],
    ),
];

const DUCK: &[Preset] = &[
    p("Subtle", &[("depth", 0.4), ("length", 0.25)]),
    p(
        "Hard pump",
        &[
            ("depth", 1.0),
            ("length", 1.0),
            ("curve", 0.8),
            ("attack", 1.0),
        ],
    ),
    p("Kick only", &[("trigger", 1.0)]),
];

const SYNTH: &[Preset] = &[
    p(
        "Warm pad",
        &[
            ("detune", 12.0),
            ("cutoff", 1_200.0),
            ("resonance", 0.15),
            ("env_amount", 0.5),
            ("attack", 600.0),
            ("decay", 800.0),
            ("sustain", 0.8),
            ("release", 1_500.0),
        ],
    ),
    p(
        "Pluck",
        &[
            ("cutoff", 600.0),
            ("resonance", 0.3),
            ("env_amount", 4.0),
            ("filter_decay", 180.0),
            ("attack", 1.0),
            ("decay", 250.0),
            ("sustain", 0.0),
            ("release", 200.0),
        ],
    ),
    p(
        "Sub bass",
        &[
            ("wave", 3.0),
            ("octave", -1.0),
            ("detune", 0.0),
            ("cutoff", 400.0),
            ("env_amount", 0.0),
            ("attack", 2.0),
            ("sustain", 1.0),
            ("release", 80.0),
        ],
    ),
    p(
        "Square lead",
        &[
            ("wave", 1.0),
            ("detune", 10.0),
            ("cutoff", 3_000.0),
            ("resonance", 0.35),
            ("sustain", 0.8),
            ("release", 200.0),
        ],
    ),
    p(
        "Cyber stab",
        &[
            ("detune", 25.0),
            ("cutoff", 900.0),
            ("resonance", 0.6),
            ("env_amount", 5.0),
            ("filter_decay", 120.0),
            ("decay", 150.0),
            ("sustain", 0.0),
            ("release", 150.0),
        ],
    ),
];

const DRUMS: &[Preset] = &[
    p(
        "909 techno",
        &[
            ("style", 1.0),
            ("kick_tune", -3.0),
            ("kick_decay", 1.6),
            ("kick_drive", 9.0),
            ("clap_level", -6.0),
        ],
    ),
    p(
        "808 boom",
        &[
            ("kick_tune", -2.0),
            ("kick_decay", 3.0),
            ("kick_sweep", 1.2),
        ],
    ),
    p(
        "Industrial",
        &[
            ("style", 2.0),
            ("kick_drive", 18.0),
            ("kick_click", 0.6),
            ("snare_snap", 1.6),
            ("tone", 0.7),
        ],
    ),
    p(
        "Tight kit",
        &[
            ("decay", 0.6),
            ("kick_decay", 0.6),
            ("hat_decay", 0.5),
            ("open_decay", 0.6),
        ],
    ),
];

const SAMPLER: &[Preset] = &[
    p("Held notes", &[("one_shot", 0.0), ("release", 300.0)]),
    p(
        "Soft pad",
        &[("one_shot", 0.0), ("attack", 300.0), ("release", 1_500.0)],
    ),
    p("Short chop", &[("release", 30.0), ("end", 0.25)]),
];

const BANK: &[Preset] = &[
    p("Held notes", &[("one_shot", 0.0), ("release", 300.0)]),
    p("Soft", &[("attack", 15.0), ("release", 300.0)]),
    p("Pitched down", &[("tune", -5.0)]),
];

const PIANO: &[Preset] = &[
    p("Concert", &[("dynamics", 36.0), ("vel_curve", -0.2)]),
    p(
        "Soft",
        &[("tone", -4.0), ("dynamics", 20.0), ("release", 140.0)],
    ),
    p(
        "Bright pop",
        &[("tone", 5.0), ("vel_curve", 0.3), ("dynamics", 18.0)],
    ),
    p(
        "Felt",
        &[("tone", -9.0), ("dynamics", 24.0), ("release", 60.0)],
    ),
    p(
        "Lo-fi",
        &[
            ("tone", -6.0),
            ("tune", -18.0),
            ("width", 0.0),
            ("dynamics", 14.0),
        ],
    ),
];

const ACID: &[Preset] = &[
    p(
        "Squelch",
        &[
            ("cutoff", 300.0),
            ("resonance", 0.9),
            ("env_mod", 0.8),
            ("decay", 250.0),
            ("accent", 0.8),
            ("drive", 9.0),
        ],
    ),
    p(
        "Square growl",
        &[
            ("wave", 1.0),
            ("cutoff", 400.0),
            ("resonance", 0.75),
            ("env_mod", 0.6),
            ("drive", 15.0),
        ],
    ),
    p(
        "Deep",
        &[
            ("cutoff", 180.0),
            ("resonance", 0.5),
            ("env_mod", 0.3),
            ("decay", 800.0),
            ("drive", 3.0),
        ],
    ),
    p(
        "Screamer",
        &[
            ("cutoff", 900.0),
            ("resonance", 0.95),
            ("env_mod", 1.0),
            ("decay", 150.0),
            ("accent", 1.0),
            ("drive", 20.0),
            ("gain", -9.0),
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ParamSpec;
    use crate::params::find;

    fn check(owner: &str, specs: &[ParamSpec], presets: &[Preset]) {
        assert!(!presets.is_empty(), "{owner} has presets");
        for (i, preset) in presets.iter().enumerate() {
            assert!(
                presets[..i].iter().all(|o| o.name != preset.name),
                "{owner}: {} is named twice",
                preset.name
            );
            let mut changes = false;
            for (j, (name, value)) in preset.values.iter().enumerate() {
                let spec = find(specs, name)
                    .unwrap_or_else(|| panic!("{owner} {}: no parameter {name}", preset.name));
                assert_eq!(
                    spec.validate(*value),
                    Ok(*value),
                    "{owner} {}: {name}",
                    preset.name
                );
                assert!(
                    preset.values[..j].iter().all(|(n, _)| n != name),
                    "{owner} {}: {name} twice",
                    preset.name
                );
                changes |= *value != spec.default;
            }
            assert!(changes, "{owner} {} changes something", preset.name);
            assert!(
                presets[..i].iter().all(|o| {
                    specs.iter().any(|s| {
                        o.value(s.name).unwrap_or(s.default)
                            != preset.value(s.name).unwrap_or(s.default)
                    })
                }),
                "{owner} {} sounds like another preset",
                preset.name
            );
        }
    }

    #[test]
    fn factory_presets_set_real_parameters_in_range() {
        for kind in EffectType::ALL {
            check(kind.name(), kind.params(), kind.presets());
        }
        for kind in InstrumentType::ALL {
            check(kind.name(), kind.params(), kind.presets());
        }
    }
}
