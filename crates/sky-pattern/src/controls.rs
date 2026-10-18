// SPDX-License-Identifier: GPL-3.0-or-later
//! Values an event can carry besides what it plays, set in Strudel code
//! with functions such as `.gain(0.8)`, `.lpf(800)` or `.note("c3")`.

/// A value an event can carry. The meanings and units are Strudel's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Control {
    /// The pitch to play a named sound at, as a MIDI note, from
    /// `note("c3").s("piano")`. A sample with no recorded note plays at its
    /// own pitch on note 36 (C2), as in Strudel.
    Note,
    /// Loudness as a linear factor. Strudel's default is 0.8.
    Gain,
    /// Loudness from 0 to 1, multiplied with the gain. The default is 1.
    Velocity,
    /// Sample playback speed: 2 is an octave up.
    Speed,
    /// Low-pass filter cutoff in Hz (`lpf`, `cutoff`).
    Cutoff,
    /// Low-pass filter resonance as a Q from 0 to about 50 (`resonance`,
    /// `lpq`).
    Resonance,
    /// Envelope attack in seconds.
    Attack,
    /// Envelope decay in seconds.
    Decay,
    /// Envelope sustain level, from 0 to 1.
    Sustain,
    /// Envelope release in seconds.
    Release,
}

impl Control {
    pub const ALL: [Control; 10] = [
        Control::Note,
        Control::Gain,
        Control::Velocity,
        Control::Speed,
        Control::Cutoff,
        Control::Resonance,
        Control::Attack,
        Control::Decay,
        Control::Sustain,
        Control::Release,
    ];

    /// The Strudel function that sets it, such as `lpf`.
    pub fn name(self) -> &'static str {
        match self {
            Control::Note => "note",
            Control::Gain => "gain",
            Control::Velocity => "velocity",
            Control::Speed => "speed",
            Control::Cutoff => "lpf",
            Control::Resonance => "resonance",
            Control::Attack => "attack",
            Control::Decay => "decay",
            Control::Sustain => "sustain",
            Control::Release => "release",
        }
    }

    fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// The controls one event sets. Values are always finite.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Controls {
    set: u16,
    values: [f64; Control::ALL.len()],
}

// Values are never NaN, so equality is reflexive.
impl Eq for Controls {}

impl Controls {
    pub fn get(&self, control: Control) -> Option<f64> {
        (self.set & control.bit() != 0).then(|| self.values[control as usize])
    }

    /// Sets `control`, unless `value` is not finite.
    pub fn set(&mut self, control: Control, value: f64) {
        if value.is_finite() {
            self.set |= control.bit();
            self.values[control as usize] = value;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.set == 0
    }

    /// The controls that are set, in [`Control::ALL`] order.
    pub fn iter(&self) -> impl Iterator<Item = (Control, f64)> + '_ {
        Control::ALL
            .into_iter()
            .filter_map(|c| self.get(c).map(|v| (c, v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let mut c = Controls::default();
        assert!(c.is_empty());
        c.set(Control::Cutoff, 800.0);
        c.set(Control::Gain, f64::NAN);
        assert_eq!(c.get(Control::Cutoff), Some(800.0));
        assert_eq!(c.get(Control::Gain), None);
        c.set(Control::Note, 48.0);
        assert_eq!(
            c.iter().collect::<Vec<_>>(),
            [(Control::Note, 48.0), (Control::Cutoff, 800.0)]
        );
    }
}
