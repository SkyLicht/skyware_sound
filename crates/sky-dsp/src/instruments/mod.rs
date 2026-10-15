// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in instruments.

use std::any::Any;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::AudioData;
use crate::params::ParamSpec;

mod acid;
mod bank;
mod drums;
mod envelope;
mod sampler;
mod synth;

pub use acid::{ACCENT_THRESHOLD, Acid};
pub use bank::{Bank, BankMap, KeyZone};
pub use drums::DrumKit;
pub use envelope::Adsr;
pub use sampler::Sampler;
pub use synth::Synth;

/// A note-driven sound source. `render` adds into the buffers.
pub trait Instrument: Send + 'static {
    /// Sets a parameter by its spec name. Returns false for unknown names.
    fn set_param(&mut self, name: &str, value: f32) -> bool;
    fn note_on(&mut self, pitch: u8, velocity: u8);
    fn note_off(&mut self, pitch: u8);
    /// Starts sound number `sound` of a sample bank as a one-shot; it
    /// rings out and takes no note-off. Other instruments ignore it.
    fn sound_on(&mut self, _sound: u32, _velocity: u8) {}
    /// Releases every held note, letting release tails ring out.
    fn all_notes_off(&mut self);
    /// Silences everything immediately.
    fn reset(&mut self);
    /// Adds one block of output into `left` and `right`. Must not allocate.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);
    /// Takes over the voices of `previous` when it is the same kind of
    /// instrument, so a graph rebuild does not cut notes off.
    fn adopt(&mut self, previous: &mut dyn Instrument) -> bool;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

macro_rules! instrument_state {
    ($($field:ident),*) => {
        fn adopt(&mut self, previous: &mut dyn $crate::instruments::Instrument) -> bool {
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
pub(crate) use instrument_state;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstrumentType {
    /// Polyphonic subtractive synth.
    Synth,
    /// Synthesized drum machine in 808 and 909 styles, on General MIDI notes.
    Drums,
    /// Plays one sample from the audio pool across the keyboard.
    Sampler,
    /// Plays named sounds, with variations, from loaded sample packs.
    Bank,
    /// Monophonic acid bass line synth with accent and slide.
    Acid,
}

/// The audio an instrument plays.
#[derive(Clone, Debug, Default)]
pub enum InstrumentAudio {
    #[default]
    None,
    /// The sampler's one sample.
    Sample(Arc<AudioData>),
    /// A sample bank's sounds and key map.
    Bank(Arc<BankMap>),
}

impl InstrumentType {
    pub const ALL: [InstrumentType; 5] = [
        InstrumentType::Synth,
        InstrumentType::Acid,
        InstrumentType::Drums,
        InstrumentType::Sampler,
        InstrumentType::Bank,
    ];

    pub fn name(self) -> &'static str {
        match self {
            InstrumentType::Synth => "synth",
            InstrumentType::Drums => "drums",
            InstrumentType::Sampler => "sampler",
            InstrumentType::Bank => "bank",
            InstrumentType::Acid => "acid",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            InstrumentType::Synth => "Subtractive synth",
            InstrumentType::Drums => "Drum machine",
            InstrumentType::Sampler => "Sampler",
            InstrumentType::Bank => "Sample bank",
            InstrumentType::Acid => "Acid bass",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            InstrumentType::Synth => {
                "16-voice synth: two detuned oscillators, resonant low-pass filter with envelope, ADSR"
            }
            InstrumentType::Drums => {
                "Synthesized kit on General MIDI notes: 36 kick, 37 rim, 38 snare, 39 clap, 42 closed hat, \
                 46 open hat, 41/45/48 toms, 49 crash, 51 ride, 56 cowbell"
            }
            InstrumentType::Sampler => {
                "Plays one audio-pool sample, pitched relative to its root note"
            }
            InstrumentType::Bank => {
                "Plays named sounds from a loaded sample pack, such as bd, sd:2 or piano; \
                 notes play the drums on General MIDI notes, or one pitched sound"
            }
            InstrumentType::Acid => {
                "Mono acid bass line: a saw or square into a squelchy 18 dB resonant low-pass with \
                 env mod and decay, then drive. Velocity above 100 accents a note (louder, with a \
                 brighter sweep that builds over consecutive accents); a note that starts while \
                 another is held slides to it"
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
            InstrumentType::Synth => synth::SYNTH_PARAMS,
            InstrumentType::Drums => drums::DRUM_PARAMS,
            InstrumentType::Sampler => sampler::SAMPLER_PARAMS,
            InstrumentType::Bank => bank::BANK_PARAMS,
            InstrumentType::Acid => acid::ACID_PARAMS,
        }
    }

    /// A new instance with default parameters. `audio` is what the sampler
    /// or sample bank plays; other instruments ignore it, and each ignores
    /// the other's kind of audio.
    pub fn create(self, sample_rate: f32, audio: InstrumentAudio) -> Box<dyn Instrument> {
        self.create_with(sample_rate, audio, std::iter::empty())
    }

    /// A new instance with the given parameter values (others default).
    pub fn create_with<'a>(
        self,
        sample_rate: f32,
        audio: InstrumentAudio,
        values: impl IntoIterator<Item = (&'a str, f32)>,
    ) -> Box<dyn Instrument> {
        let mut inst: Box<dyn Instrument> = match (self, audio) {
            (InstrumentType::Synth, _) => Box::new(Synth::new(sample_rate)),
            (InstrumentType::Acid, _) => Box::new(Acid::new(sample_rate)),
            (InstrumentType::Drums, _) => Box::new(DrumKit::new(sample_rate)),
            (InstrumentType::Sampler, InstrumentAudio::Sample(sample)) => {
                Box::new(Sampler::new(sample_rate, Some(sample)))
            }
            (InstrumentType::Sampler, _) => Box::new(Sampler::new(sample_rate, None)),
            (InstrumentType::Bank, InstrumentAudio::Bank(map)) => {
                Box::new(Bank::new(sample_rate, map))
            }
            (InstrumentType::Bank, _) => Box::new(Bank::new(sample_rate, Arc::default())),
        };
        for spec in self.params() {
            inst.set_param(spec.name, spec.default);
        }
        for (name, value) in values {
            inst.set_param(name, value);
        }
        inst
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_seconds(inst: &mut dyn Instrument, seconds: f32) -> (Vec<f32>, Vec<f32>) {
        let frames = (48_000.0 * seconds) as usize;
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (lc, rc) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            inst.render(lc, rc);
        }
        (l, r)
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    /// Audio that makes each kind of instrument sound: a sine for the
    /// sampler, and the same sine on key 60 and as sound 0 for a bank.
    pub(crate) fn test_audio(kind: InstrumentType) -> InstrumentAudio {
        let sample = Arc::new(AudioData {
            sample_rate: 48_000,
            channels: vec![(0..4_800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect()],
        });
        match kind {
            InstrumentType::Sampler => InstrumentAudio::Sample(sample),
            InstrumentType::Bank => {
                let mut keys = vec![None; 128];
                keys[60] = Some(KeyZone {
                    sample: 0,
                    semitones: 0.0,
                });
                InstrumentAudio::Bank(Arc::new(BankMap {
                    samples: vec![sample],
                    keys,
                }))
            }
            _ => InstrumentAudio::None,
        }
    }

    #[test]
    fn instruments_make_sound_and_stop() {
        for t in InstrumentType::ALL {
            let mut inst = t.create(48_000.0, test_audio(t));
            for spec in t.params() {
                assert!(
                    inst.set_param(spec.name, spec.default),
                    "{} {}",
                    t.name(),
                    spec.name
                );
            }
            let note = if t == InstrumentType::Drums { 38 } else { 60 };
            inst.note_on(note, 100);
            let (l, r) = render_seconds(inst.as_mut(), 0.1);
            assert!(peak(&l) > 0.01 && peak(&r) > 0.01, "{} silent", t.name());
            assert!(l.iter().all(|s| s.is_finite()));
            inst.all_notes_off();
            let _ = render_seconds(inst.as_mut(), 4.0);
            let (l, _) = render_seconds(inst.as_mut(), 0.1);
            assert!(peak(&l) < 1e-3, "{} did not stop: {}", t.name(), peak(&l));
        }
    }

    #[test]
    fn every_drum_note_sounds() {
        for note in [
            35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 56, 57, 59,
        ] {
            let mut kit = InstrumentType::Drums.create(48_000.0, InstrumentAudio::None);
            kit.note_on(note, 127);
            let (l, _) = render_seconds(kit.as_mut(), 0.05);
            assert!(peak(&l) > 0.01, "drum note {note} silent");
        }
    }

    #[test]
    fn every_drum_note_sounds_in_every_style() {
        for style in 0..3 {
            for note in [36, 38, 39, 42, 46, 49] {
                let mut kit = InstrumentType::Drums.create(48_000.0, InstrumentAudio::None);
                assert!(kit.set_param("style", style as f32));
                kit.note_on(note, 127);
                let (l, _) = render_seconds(kit.as_mut(), 0.05);
                assert!(peak(&l) > 0.01, "style {style} note {note} silent");
                assert!(l.iter().all(|s| s.is_finite() && s.abs() < 4.0));
            }
        }
    }

    /// Crossings of zero going up in the first `seconds`, a rough pitch.
    fn rising_crossings(inst: &mut dyn Instrument, seconds: f32) -> usize {
        let (l, _) = render_seconds(inst, seconds);
        l.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn each_drum_piece_has_its_own_controls() {
        let kick = |params: &[(&str, f32)]| {
            let mut kit = InstrumentType::Drums.create(48_000.0, InstrumentAudio::None);
            for (name, value) in params {
                assert!(kit.set_param(name, *value), "{name}");
            }
            kit.note_on(36, 127);
            kit
        };
        // Tuning the kick up raises its pitch.
        let low = rising_crossings(kick(&[("kick_click", 0.0)]).as_mut(), 0.1);
        let high = rising_crossings(
            kick(&[("kick_click", 0.0), ("kick_tune", 12.0)]).as_mut(),
            0.1,
        );
        assert!(high > low + low / 2, "{low} -> {high}");
        // A longer decay rings for longer.
        let tail = |kit: &mut Box<dyn Instrument>| {
            let _ = render_seconds(kit.as_mut(), 0.4);
            peak(&render_seconds(kit.as_mut(), 0.1).0)
        };
        assert!(tail(&mut kick(&[("kick_decay", 3.0)])) > 4.0 * tail(&mut kick(&[])));
        // A piece's level reaches zero without touching the others.
        let mut kit = kick(&[("kick_level", -60.0)]);
        assert!(peak(&render_seconds(kit.as_mut(), 0.05).0) < 1e-6);
        kit.note_on(38, 127);
        assert!(peak(&render_seconds(kit.as_mut(), 0.05).0) > 0.01);
        // Pieces without a pitch refuse a tune.
        assert!(!kit.set_param("clap_tune", 0.0));
        assert!(!kit.set_param("rim_decay", 1.0));
    }

    #[test]
    fn voices_survive_adopt() {
        let mut a = InstrumentType::Synth.create(48_000.0, InstrumentAudio::None);
        a.note_on(60, 100);
        let _ = render_seconds(a.as_mut(), 0.05);
        let mut b = InstrumentType::Synth.create(48_000.0, InstrumentAudio::None);
        assert!(b.adopt(a.as_mut()));
        let (l, _) = render_seconds(b.as_mut(), 0.05);
        assert!(peak(&l) > 0.01, "held note lost across adopt");
        let mut c = InstrumentType::Drums.create(48_000.0, InstrumentAudio::None);
        assert!(!c.adopt(b.as_mut()));
    }
}
