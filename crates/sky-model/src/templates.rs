// SPDX-License-Identifier: GPL-3.0-or-later
//! Starter projects.

use sky_core::manifest::{Category, RandomRange, Selection};
use sky_core::{AssetId, ClipId};
use sky_dsp::{EffectType, InstrumentType};

use crate::notes::parse_note_list;
use crate::project::*;

/// An empty project with one drum track, ready for a pattern.
pub fn empty_project(name: &str) -> Project {
    Project::new(name)
}

/// A four-bar loop in A minor at 120 BPM: drums from a pattern clip, a bass
/// line and pad chords from MIDI clips. Used for first runs and tests.
pub fn demo_project() -> Project {
    let mut p = Project::new("Demo loop");
    let bar = p.ticks_per_bar();
    let bars = 4;

    let mut drums = Track::new_instrument("Drums", InstrumentType::Drums);
    drums.clips.push(Clip {
        id: ClipId::new(),
        name: "Beat".into(),
        start: 0,
        length: bar * bars,
        content: ClipContent::Pattern {
            source: "bd*4, ~ sd ~ sd, hh*8".into(),
            seed: 1,
            cycle: bar,
        },
    });

    let mut bass = Track::new_instrument("Bass", InstrumentType::Synth);
    if let Some(inst) = bass.instrument.as_mut() {
        inst.params.insert("cutoff".into(), 500.0);
        inst.params.insert("env_amount".into(), 3.0);
        inst.params.insert("filter_decay".into(), 180.0);
        inst.params.insert("sustain".into(), 0.5);
        inst.params.insert("release".into(), 80.0);
        inst.params.insert("gain".into(), -8.0);
    }
    let roots = ["A1", "F1", "C2", "G1"];
    let line: Vec<String> = roots
        .iter()
        .flat_map(|r| {
            let up = match *r {
                "A1" => "A2",
                "F1" => "F2",
                "C2" => "C3",
                _ => "G2",
            };
            [*r, up, *r, *r, up, *r, up, *r]
                .map(|n| format!("{n} 1/8 v{}", if n == *r { 110 } else { 85 }))
        })
        .collect();
    bass.clips.push(Clip {
        id: ClipId::new(),
        name: "Bass line".into(),
        start: 0,
        length: bar * bars,
        content: ClipContent::Midi {
            notes: parse_note_list(&line.join(", ")).unwrap_or_default(),
        },
    });

    let mut pad = Track::new_instrument("Pad", InstrumentType::Synth);
    if let Some(inst) = pad.instrument.as_mut() {
        inst.params.insert("wave".into(), 1.0);
        inst.params.insert("attack".into(), 300.0);
        inst.params.insert("release".into(), 900.0);
        inst.params.insert("cutoff".into(), 1_400.0);
        inst.params.insert("env_amount".into(), 0.5);
        inst.params.insert("detune".into(), 14.0);
        inst.params.insert("gain".into(), -16.0);
    }
    pad.channel
        .effects
        .push(EffectSlot::new(EffectType::Reverb).with_params(&[("mix", 0.35), ("size", 0.8)]));
    pad.clips.push(Clip {
        id: ClipId::new(),
        name: "Chords".into(),
        start: 0,
        length: bar * bars,
        content: ClipContent::Midi {
            notes: parse_note_list(
                "A3+C4+E4 1 v80, F3+A3+C4 1 v80, C3+E3+G3+C4 1 v80, G3+B3+D4 1 v80",
            )
            .unwrap_or_default(),
        },
    });

    p.master
        .effects
        .push(EffectSlot::new(EffectType::Limiter).with_params(&[("ceiling", -1.0)]));
    p.tracks = vec![drums, bass, pad];

    // What the game export writes: the whole loop as music, and the four
    // beats of the first bar as a set of hits.
    p.game.assets = vec![
        GameAsset {
            id: AssetId::new(),
            name: "demo_music".into(),
            category: Category::Music,
            tags: Vec::new(),
            kind: GameAssetKind::Loop {
                range: TimeRange {
                    start: 0,
                    end: bar * bars,
                },
                intro_start: None,
            },
        },
        GameAsset {
            id: AssetId::new(),
            name: "demo_hits".into(),
            category: Category::Sfx,
            tags: Vec::new(),
            kind: GameAssetKind::Sfx {
                variations: TimeRange { start: 0, end: bar }.split(4),
                selection: Selection::RandomNoRepeat,
                pitch_semitones: RandomRange::new(-1.0, 1.0),
                gain_db: RandomRange::new(-2.0, 0.0),
                tail_seconds: 0.5,
                mono: false,
            },
        },
    ];
    p
}
