// SPDX-License-Identifier: GPL-3.0-or-later
//! Every processor must run without allocating once constructed.

use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use sky_dsp::instruments::sampled::{LoopMode, Trigger};
use sky_dsp::instruments::{BankMap, KeyZone, SampleData, SampleLibrary, Zone};
use sky_dsp::{AudioData, EffectType, InstrumentAudio, InstrumentType};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn processors_do_not_allocate() {
    let sample = Arc::new(AudioData::silent(2, 9_600, 44_100));
    let mut effects: Vec<_> = EffectType::ALL.iter().map(|t| t.create(48_000.0)).collect();
    let bank = Arc::new(BankMap {
        samples: vec![sample.clone(); 3],
        keys: (0..128u32)
            .map(|k| {
                Some(KeyZone {
                    sample: k % 3,
                    semitones: (k as f32 - 60.0) / 2.0,
                })
            })
            .collect(),
    });
    // A looped layer, a one-shot layer and a release zone on every key,
    // and the pedal's own sounds.
    let piano = Arc::new(SampleLibrary::new(
        "test",
        vec![
            SampleData::from_audio(&sample),
            SampleData::from_audio(&AudioData::silent(1, 4_800, 48_000)),
        ],
        vec![
            Zone {
                hivel: 80,
                loop_mode: LoopMode::Continuous,
                loop_start: 100,
                loop_end: 9_000,
                ..Zone::new(0, 60)
            },
            Zone {
                lovel: 81,
                ..Zone::new(1, 60)
            },
            Zone {
                trigger: Trigger::Release,
                ..Zone::new(1, 60)
            },
            Zone {
                trigger: Trigger::PedalDown,
                hirand: 0.5,
                ..Zone::new(1, 60)
            },
            Zone {
                trigger: Trigger::PedalUp,
                ..Zone::new(1, 60)
            },
        ],
    ));
    let mut instruments: Vec<_> = InstrumentType::ALL
        .iter()
        .map(|t| {
            let audio = match t {
                InstrumentType::Bank => InstrumentAudio::Bank(bank.clone()),
                InstrumentType::Piano => InstrumentAudio::Library(piano.clone()),
                _ => InstrumentAudio::Sample(sample.clone()),
            };
            (t.params(), t.create(48_000.0, audio))
        })
        .collect();
    let mut left = vec![0.0f32; sky_dsp::MAX_BLOCK];
    let mut right = vec![0.0f32; sky_dsp::MAX_BLOCK];
    let mut meter = sky_dsp::loudness::BlockMeter::new(48_000);
    let mut blocks = 0usize;
    assert_no_alloc(|| {
        for block in 0..50 {
            for (params, inst) in instruments.iter_mut() {
                if block % 10 == 0 {
                    // Parameter locks set values right before notes.
                    for spec in params.iter() {
                        inst.set_param(spec.name, spec.max);
                    }
                    inst.controller(sky_dsp::SUSTAIN_PEDAL, 127);
                    for note in [36, 38, 42, 60, 64, 67] {
                        inst.note_on(note, 100);
                    }
                    for sound in 0..4 {
                        inst.sound_on(sound, 90);
                    }
                }
                if block % 10 == 3 {
                    for note in [36, 60] {
                        inst.note_off(note);
                    }
                    // The pedal lets go of the keys that came up.
                    inst.controller(sky_dsp::SUSTAIN_PEDAL, 0);
                    inst.note_on(60, 90);
                    inst.note_on(60, 70);
                }
                if block % 10 == 5 {
                    inst.all_notes_off();
                }
                inst.render(&mut left, &mut right);
            }
            for fx in effects.iter_mut() {
                if block % 10 == 0 {
                    fx.set_tempo(90.0 + block as f64);
                    fx.sidechain_note(17, 36);
                    fx.sidechain_note(300, 42);
                }
                fx.process(&mut left, &mut right);
            }
            meter.process(&left, &right, |_| blocks += 1);
        }
    });
    assert!(blocks > 0);
}
