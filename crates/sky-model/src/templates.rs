// SPDX-License-Identifier: GPL-3.0-or-later
//! Starter projects.

use sky_core::manifest::{Category, RandomRange, Selection};
use sky_core::{AssetId, ClipId, TempoMap};
use sky_dsp::{EffectType, InstrumentType};

use crate::notes::parse_note_list;
use crate::project::*;
use crate::steps::{StepClip, StepLane};

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
        muted: false,
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
        muted: false,
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
        muted: false,
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
        muted: false,
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

/// A step clip `bars` long from the start.
fn step_clip(name: &str, bars: i64, bar: i64, steps: StepClip) -> Clip {
    Clip {
        id: ClipId::new(),
        name: name.into(),
        start: 0,
        length: bar * bars,
        muted: false,
        content: ClipContent::Steps(steps),
    }
}

fn lane(note: u8, steps: &str) -> StepLane {
    StepLane::parse(note, steps).expect("template steps parse")
}

/// A four-bar acid techno loop at 128 BPM made of step clips: a 909-style
/// beat whose hats use probability, ratchets and a fill on the last bar,
/// and a 12-step Acid bass line with accents and slides that drifts
/// against the 16-step beat, ducked lightly by the kick and echoed by a
/// dotted delay.
pub fn acid_project() -> Project {
    let mut p = Project::new("Acid loop");
    p.tempo = TempoMap::constant(128.0);
    let bar = p.ticks_per_bar();
    let bars = 4;

    let mut drums = Track::new_instrument("Drums", InstrumentType::Drums);
    set_params(
        &mut drums,
        &[
            ("style", 1.0),
            ("kick_tune", -2.0),
            ("kick_drive", 6.0),
            ("hat_decay", 0.8),
            ("clap_level", -4.0),
            ("gain", -5.0),
        ],
    );
    let beat = StepClip::new(
        16,
        vec![
            lane(36, "X . . . x . . . x . . . x . . ."),
            lane(39, ". . . . x . . . . . . . x . . x@fill"),
            lane(46, ". . x . . . x . . . x . . . x ."),
            lane(
                42,
                "x=70 x?50=50 . x=70 x=70 x?50=50 . x=70 x=70 x?50=50 . x=70 x=70 x?50=50 . x*2@fill",
            ),
        ],
    );
    drums.clips.push(step_clip("Beat", bars, bar, beat));

    let mut acid = Track::new_instrument("Acid", InstrumentType::Acid);
    set_params(
        &mut acid,
        &[
            ("cutoff", 320.0),
            ("resonance", 0.82),
            ("env_mod", 0.6),
            ("decay", 300.0),
            ("accent", 0.8),
            ("drive", 9.0),
            ("gain", -9.0),
        ],
    );
    let mut line = StepClip::new(16, vec![lane(36, "x x/ c3! x . d#2/ f2 x! - g1 x a#1/")]);
    line.seed = 1;
    acid.clips.push(step_clip("Acid line", bars, bar, line));
    acid.channel.effects.push(
        EffectSlot::new(EffectType::Duck)
            .with_params(&[("depth", 0.5), ("length", 0.5), ("attack", 1.0)])
            .with_sidechain(drums.id),
    );
    acid.channel
        .effects
        .push(EffectSlot::new(EffectType::Delay).with_params(&[
            ("sync", 7.0),
            ("feedback", 0.35),
            ("mix", 0.2),
            ("damping", 3_000.0),
        ]));

    p.master
        .effects
        .push(EffectSlot::new(EffectType::Limiter).with_params(&[("ceiling", -1.0)]));
    p.tracks = vec![drums, acid];
    p.loop_region = TimeRange {
        start: 0,
        end: bar * bars,
    };
    p
}
