// SPDX-License-Identifier: GPL-3.0-or-later
//! Every processor must run without allocating once constructed.

use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use sky_dsp::{AudioData, EffectType, InstrumentType};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn processors_do_not_allocate() {
    let sample = Arc::new(AudioData::silent(2, 9_600, 44_100));
    let mut effects: Vec<_> = EffectType::ALL.iter().map(|t| t.create(48_000.0)).collect();
    let mut instruments: Vec<_> = InstrumentType::ALL
        .iter()
        .map(|t| t.create(48_000.0, Some(sample.clone())))
        .collect();
    let mut left = vec![0.0f32; sky_dsp::MAX_BLOCK];
    let mut right = vec![0.0f32; sky_dsp::MAX_BLOCK];
    assert_no_alloc(|| {
        for block in 0..50 {
            for inst in instruments.iter_mut() {
                if block % 10 == 0 {
                    for note in [36, 38, 42, 60, 64, 67] {
                        inst.note_on(note, 100);
                    }
                }
                if block % 10 == 5 {
                    inst.all_notes_off();
                }
                inst.render(&mut left, &mut right);
            }
            for fx in effects.iter_mut() {
                fx.process(&mut left, &mut right);
            }
        }
    });
}
