// SPDX-License-Identifier: GPL-3.0-or-later
//! Starter projects.

use sky_core::{ClipId, TempoMap};
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
        content: ClipContent::pattern("bd*4, ~ sd ~ sd, hh*8", 1, bar),
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
    p
}

/// A pattern clip with controls and swing, `bars` long from the start.
fn pattern_clip(
    name: &str,
    bars: i64,
    bar: i64,
    source: &str,
    controls: &[(&str, &str)],
    swing: Swing,
) -> Clip {
    Clip {
        id: ClipId::new(),
        name: name.into(),
        start: 0,
        length: bar * bars,
        content: ClipContent::Pattern {
            source: source.into(),
            seed: 1,
            cycle: bar,
            controls: controls
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            swing,
        },
    }
}

fn set_params(track: &mut Track, params: &[(&str, f32)]) {
    if let Some(inst) = track.instrument.as_mut() {
        for (k, v) in params {
            inst.params.insert((*k).to_string(), *v);
        }
    }
}

/// A four-bar techno loop at 128 BPM built only from the built-in
/// instruments: a driven kick with claps, accented and swung hats, a
/// rolling bass ducked by the kick, and a chord stab through a dotted
/// delay. It shows off pattern controls, swing and the duck. At 128 BPM
/// the four bars are a whole number of samples at 48 kHz, so the loop
/// never drifts in a game.
pub fn techno_project() -> Project {
    let mut p = Project::new("Techno loop");
    p.tempo = TempoMap::constant(128.0);
    let bar = p.ticks_per_bar();
    let bars = 4;

    let mut kick = Track::new_instrument("Kick", InstrumentType::Drums);
    set_params(
        &mut kick,
        &[
            ("style", 1.0),
            ("kick_tune", -3.0),
            ("kick_decay", 1.6),
            ("kick_drive", 9.0),
            ("clap_level", -6.0),
            ("gain", -4.0),
        ],
    );
    kick.clips.push(pattern_clip(
        "Four on the floor",
        bars,
        bar,
        "bd*4, ~ cp ~ cp",
        &[],
        Swing::default(),
    ));

    let mut hats = Track::new_instrument("Hats", InstrumentType::Drums);
    set_params(&mut hats, &[("style", 1.0), ("gain", -9.0)]);
    hats.clips.push(pattern_clip(
        "Hats",
        bars,
        bar,
        "hh*16",
        &[
            ("velocity", "[70 45 127 45]*4"),
            ("hat_decay", "[0.7 0.5 1.8 0.5]*4"),
        ],
        Swing::new(56, 16),
    ));

    let mut bass = Track::new_instrument("Bass", InstrumentType::Synth);
    set_params(
        &mut bass,
        &[
            ("cutoff", 600.0),
            ("resonance", 0.45),
            ("env_amount", 2.5),
            ("filter_decay", 120.0),
            ("decay", 150.0),
            ("sustain", 0.4),
            ("release", 40.0),
            ("gain", -10.0),
        ],
    );
    bass.clips.push(pattern_clip(
        "Rolling bass",
        bars,
        bar,
        "[~ a1 a1 a1]*4",
        &[("cutoff", "<[500 700 900 700]*4 [500 800 1200 800]*4>")],
        Swing::default(),
    ));
    bass.channel.effects.push(
        EffectSlot::new(EffectType::Duck)
            .with_params(&[("depth", 0.85), ("length", 0.75), ("attack", 1.0)])
            .with_sidechain(kick.id),
    );

    let mut stab = Track::new_instrument("Stab", InstrumentType::Synth);
    set_params(
        &mut stab,
        &[
            ("wave", 1.0),
            ("cutoff", 1_800.0),
            ("env_amount", 1.5),
            ("filter_decay", 200.0),
            ("decay", 180.0),
            ("sustain", 0.0),
            ("release", 120.0),
            ("gain", -18.0),
        ],
    );
    stab.clips.push(pattern_clip(
        "Stab",
        bars,
        bar,
        "<[~ [a3, c4, e4] ~ ~] [~ ~ [a3, c4, e4] ~]>",
        &[],
        Swing::default(),
    ));
    // A dotted-eighth delay and a short room behind it.
    stab.channel
        .effects
        .push(EffectSlot::new(EffectType::Delay).with_params(&[
            ("sync", 7.0),
            ("feedback", 0.45),
            ("mix", 0.3),
            ("damping", 4_000.0),
        ]));
    stab.channel
        .effects
        .push(EffectSlot::new(EffectType::Reverb).with_params(&[
            ("mix", 0.2),
            ("size", 0.5),
            ("predelay", 20.0),
        ]));

    p.master
        .effects
        .push(EffectSlot::new(EffectType::Limiter).with_params(&[("ceiling", -1.0)]));
    p.tracks = vec![kick, hats, bass, stab];
    p.loop_region = TimeRange {
        start: 0,
        end: bar * bars,
    };
    p
}
