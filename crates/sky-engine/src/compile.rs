// SPDX-License-Identifier: GPL-3.0-or-later
//! Compiling a project into an engine snapshot. Runs on the control thread,
//! where allocation is fine: everything the audio thread needs is built here.

use std::collections::HashMap;
use std::sync::Arc;

use num_rational::Rational64;
use sky_core::{AudioId, EffectId, TempoMap, Ticks, TrackId};
use sky_dsp::instruments::{BankMap, KeyZone};
use sky_dsp::util::db_to_gain;
use sky_dsp::{
    AudioData, Effect, Instrument, InstrumentAudio, InstrumentType, MAX_BLOCK, ParamSpec,
};
use sky_model::command::VELOCITY;
use sky_model::{
    Channel, Clip, ClipContent, InstrumentSlot, Project, SampleBank, StepClip, Track, TrackKind,
};
use sky_pattern::{Time, Value};

/// Decoded audio-pool files, shared between snapshots.
pub type AudioCache = HashMap<AudioId, Arc<AudioData>>;

/// A note with its end, both in samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteEvent {
    pub time: i64,
    pub end: i64,
    /// The key, or for a bank sound the General MIDI drum note of its name
    /// (0 when it has none), so a duck can tell kicks apart.
    pub pitch: u8,
    pub velocity: u8,
    /// For a sample bank: the sound to start as a one-shot instead of the
    /// key `pitch`.
    pub sound: Option<u32>,
    /// The note's parameter values: `locks[lock_start..lock_end]` of its
    /// source, set on the instrument right before the note starts.
    pub lock_start: u32,
    pub lock_end: u32,
}

/// An instrument parameter set for one note.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamLock {
    pub param: &'static str,
    pub value: f32,
}

pub struct AudioClipNode {
    pub start: i64,
    pub end: i64,
    pub data: Arc<AudioData>,
    /// Source frame at the clip start.
    pub offset: f64,
    /// Source frames per output frame.
    pub rate: f64,
    pub gain: f32,
    pub fade_in: i64,
    pub fade_out: i64,
}

pub enum SourceNode {
    Audio(Vec<AudioClipNode>),
    Instrument {
        instrument: Option<Box<dyn Instrument>>,
        notes: Vec<NoteEvent>,
        locks: Vec<ParamLock>,
    },
}

pub struct EffectNode {
    pub id: EffectId,
    pub enabled: bool,
    pub effect: Box<dyn Effect>,
    /// The index of the track whose note starts drive the effect.
    pub sidechain: Option<usize>,
}

/// Most note starts a track reports to sidechains in one block.
pub(crate) const MAX_TRIGGERS: usize = 128;

/// The note starts of one track in the block being rendered: the frame
/// offset in the block and the pitch.
pub(crate) struct Triggers {
    notes: [(u32, u8); MAX_TRIGGERS],
    len: usize,
}

impl Default for Triggers {
    fn default() -> Self {
        Self {
            notes: [(0, 0); MAX_TRIGGERS],
            len: 0,
        }
    }
}

impl Triggers {
    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    pub(crate) fn push(&mut self, offset: usize, pitch: u8) {
        if let Some(slot) = self.notes.get_mut(self.len) {
            *slot = (offset.min(u32::MAX as usize) as u32, pitch);
            self.len += 1;
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (usize, u8)> + '_ {
        self.notes
            .get(..self.len)
            .unwrap_or_default()
            .iter()
            .map(|&(offset, pitch)| (offset as usize, pitch))
    }
}

pub struct ChannelNode {
    pub gain_l: f32,
    pub gain_r: f32,
    pub effects: Vec<EffectNode>,
    /// Smoothed gains actually applied; carried across swaps.
    pub(crate) current: [f32; 2],
}

/// Per-track state that must survive a snapshot swap.
pub(crate) struct TrackState {
    /// End sample of each sounding pitch, or -1.
    pub active: [i64; 128],
    /// Index of the next note to start.
    pub cursor: usize,
}

impl Default for TrackState {
    fn default() -> Self {
        Self {
            active: [-1; 128],
            cursor: 0,
        }
    }
}

pub struct TrackNode {
    pub id: TrackId,
    pub meter: usize,
    pub source: SourceNode,
    pub channel: ChannelNode,
    pub(crate) state: TrackState,
    pub(crate) buf: [Vec<f32>; 2],
}

/// Everything the audio thread needs to play the project.
pub struct EngineSnapshot {
    pub version: u64,
    pub sample_rate: u32,
    pub tracks: Vec<TrackNode>,
    pub master: ChannelNode,
    /// Loop region in samples, when looping is on.
    pub loop_range: Option<(i64, i64)>,
    /// Each track's note starts in the current block, by track index.
    pub(crate) triggers: Vec<Triggers>,
}

/// What effects need from the project beyond their own settings.
struct EffectContext<'a> {
    sample_rate: f32,
    /// Tempo-synced effects follow the tempo at the start of the song.
    bpm: f64,
    tracks: &'a HashMap<TrackId, usize>,
}

fn channel_node(channel: &Channel, audible: bool, cx: &EffectContext) -> ChannelNode {
    let gain = if audible && !channel.mute {
        db_to_gain(channel.volume_db)
    } else {
        0.0
    };
    let (pl, pr) = sky_dsp::util::pan_gains(channel.pan);
    let effects = channel
        .effects
        .iter()
        .map(|slot| EffectNode {
            id: slot.id,
            enabled: slot.enabled,
            effect: slot.kind.create_at(
                cx.sample_rate,
                cx.bpm,
                slot.params.iter().map(|(k, v)| (k.as_str(), *v)),
            ),
            sidechain: slot.sidechain.and_then(|t| cx.tracks.get(&t).copied()),
        })
        .collect();
    ChannelNode {
        gain_l: gain * pl,
        gain_r: gain * pr,
        effects,
        current: [gain * pl, gain * pr],
    }
}

fn ticks_to_samples(tempo: &TempoMap, tick: Ticks, sr: u32) -> i64 {
    tempo.tick_to_sample(tick, sr)
}

/// What a pattern value plays on a track: a key, or a bank sound.
type Resolve<'a> = dyn Fn(&Value) -> Option<(u8, Option<u32>)> + 'a;

/// The velocity of a note with none given.
const DEFAULT_VELOCITY: u8 = 100;

/// The instrument parameters a track's patterns and steps set per note.
/// Once any clip sets one, every note on the track carries a value for it,
/// the usual one where nothing else says, so a note always sounds the same
/// whatever played before it.
struct Locks {
    params: Vec<&'static ParamSpec>,
    /// The track's own value of each parameter.
    base: Vec<f32>,
    out: Vec<ParamLock>,
}

impl Locks {
    fn new(track: &Track) -> Self {
        let mut locks = Self {
            params: Vec::new(),
            base: Vec::new(),
            out: Vec::new(),
        };
        let Some(slot) = &track.instrument else {
            return locks;
        };
        let specs = slot.kind.params();
        let mut names: Vec<&str> = Vec::new();
        for clip in &track.clips {
            match &clip.content {
                ClipContent::Pattern { controls, .. } => {
                    names.extend(controls.keys().map(String::as_str));
                }
                ClipContent::Steps(steps) => names.extend(steps.locked_params()),
                _ => {}
            }
        }
        names.sort_unstable();
        names.dedup();
        for spec in names
            .into_iter()
            .filter_map(|n| sky_dsp::params::find(specs, n))
        {
            locks.params.push(spec);
            locks.base.push(base_value(slot, spec));
        }
        locks
    }

    /// Records a note's values, `value(i)` overriding parameter `i`'s
    /// usual one, and returns where they sit.
    fn push(&mut self, value: impl Fn(usize) -> Option<f64>) -> (u32, u32) {
        let start = self.out.len() as u32;
        for (i, (spec, base)) in self.params.iter().zip(&self.base).enumerate() {
            let v = value(i).map_or(*base, |v| spec.clamp(v as f32));
            self.out.push(ParamLock {
                param: spec.name,
                value: v,
            });
        }
        (start, self.out.len() as u32)
    }
}

fn base_value(slot: &InstrumentSlot, spec: &ParamSpec) -> f32 {
    slot.params.get(spec.name).copied().unwrap_or(spec.default)
}

/// What a control pattern sets.
#[derive(Clone, Copy)]
enum Target {
    Velocity,
    /// An index into [`Locks::params`].
    Param(usize),
}

/// A control pattern's events over a clip, ready to be read at any time.
struct Control {
    target: Target,
    /// Begin, end and value, sorted by begin.
    events: Vec<(Time, Time, f64)>,
    longest: Time,
}

impl Control {
    fn new(target: Target, source: &str, to: Time, seed: u64) -> Option<Self> {
        let pattern = sky_pattern::parse_numbers(source).ok()?;
        let mut events: Vec<(Time, Time, f64)> = pattern
            .query(Time::from_integer(0), to, seed)
            .into_iter()
            .filter_map(|e| Some((e.begin, e.end, e.value.number()?)))
            .collect();
        events.sort_by_key(|e| e.0);
        let longest = events.iter().map(|e| e.1 - e.0).max().unwrap_or_default();
        Some(Self {
            target,
            events,
            longest,
        })
    }

    /// The value of the step playing at `t`, or `None` on a rest.
    fn at(&self, t: Time) -> Option<f64> {
        let upto = self.events.partition_point(|e| e.0 <= t);
        self.events
            .get(..upto)
            .unwrap_or_default()
            .iter()
            .rev()
            .take_while(|e| e.0 > t - self.longest)
            .find(|e| e.1 > t)
            .map(|e| e.2)
    }
}

/// A seed of its own for each control, so a `?` in one does not follow the
/// notes' or another control's.
fn control_seed(seed: u64, name: &str) -> u64 {
    name.bytes().fold(seed ^ 0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn pattern_notes(
    tempo: &TempoMap,
    sr: u32,
    clip: &Clip,
    resolve: &Resolve,
    locks: &mut Locks,
    out: &mut Vec<NoteEvent>,
) {
    let ClipContent::Pattern {
        source,
        seed,
        cycle,
        controls,
        swing,
    } = &clip.content
    else {
        return;
    };
    let (clip_start, clip_len, seed, cycle) = (clip.start, clip.length, *seed, *cycle);
    let Ok(pattern) = sky_pattern::parse(source) else {
        return;
    };
    if cycle <= 0 {
        return;
    }
    let to = Rational64::new(clip_len, cycle);
    let controls: Vec<Control> = controls
        .iter()
        .filter_map(|(name, src)| {
            let target = if name == VELOCITY {
                Target::Velocity
            } else {
                Target::Param(locks.params.iter().position(|s| s.name == name)?)
            };
            Control::new(target, src, to, control_seed(seed, name))
        })
        .collect();
    let clip_end = clip_start + clip_len;
    for ev in pattern.query(Rational64::from_integer(0), to, seed) {
        let Some((pitch, sound)) = resolve(&ev.value) else {
            continue;
        };
        let mut velocity = DEFAULT_VELOCITY;
        for c in &controls {
            if let (Target::Velocity, Some(v)) = (c.target, c.at(ev.begin)) {
                // Like MIDI, velocity 0 is no note at all.
                velocity = v.round().clamp(0.0, 127.0) as u8;
            }
        }
        if velocity == 0 {
            continue;
        }
        let begin = swing.apply(clip_start + rational_ticks(ev.begin, cycle));
        let end = swing
            .apply(clip_start + rational_ticks(ev.end, cycle))
            .min(clip_end);
        if end <= begin {
            continue;
        }
        let (lock_start, lock_end) = locks.push(|i| {
            controls.iter().find_map(|c| match c.target {
                Target::Param(p) if p == i => c.at(ev.begin),
                _ => None,
            })
        });
        out.push(NoteEvent {
            time: ticks_to_samples(tempo, begin, sr),
            end: ticks_to_samples(tempo, end, sr),
            pitch,
            velocity,
            sound,
            lock_start,
            lock_end,
        });
    }
}

fn step_notes(
    tempo: &TempoMap,
    sr: u32,
    clip: &Clip,
    steps: &StepClip,
    locks: &mut Locks,
    out: &mut Vec<NoteEvent>,
) {
    let clip_end = clip.start + clip.length;
    let names: Vec<&'static str> = locks.params.iter().map(|s| s.name).collect();
    for n in steps.notes(clip.length) {
        let begin = steps.swing.apply(clip.start + n.start);
        let end = steps.swing.apply(clip.start + n.end).min(clip_end);
        if end <= begin {
            continue;
        }
        let (lock_start, lock_end) = locks.push(|i| {
            names
                .get(i)
                .and_then(|name| n.locks.get(*name))
                .map(|v| f64::from(*v))
        });
        out.push(NoteEvent {
            time: ticks_to_samples(tempo, begin, sr),
            end: ticks_to_samples(tempo, end, sr),
            pitch: n.pitch,
            velocity: n.velocity,
            sound: None,
            lock_start,
            lock_end,
        });
    }
}

/// A sample bank ready to play: the instrument's map, and where each of the
/// bank's sounds sits in the map's sample list.
pub struct CompiledBank<'a> {
    pub map: BankMap,
    bank: &'a SampleBank,
    /// Each sound's loaded variations: the sample index and recorded note.
    sounds: HashMap<&'a str, Vec<(u32, Option<u8>)>>,
}

impl<'a> CompiledBank<'a> {
    /// Collects the bank's decoded samples and lays out its keys. Samples
    /// whose audio is not loaded are left out.
    pub fn new(bank: &'a SampleBank, audio: &AudioCache) -> Self {
        let mut map = BankMap::default();
        let mut sounds = HashMap::new();
        for (name, samples) in &bank.sounds {
            let mut loaded = Vec::new();
            for s in samples {
                if let Some(data) = audio.get(&s.audio) {
                    loaded.push((map.samples.len() as u32, s.root));
                    map.samples.push(data.clone());
                }
            }
            if !loaded.is_empty() {
                sounds.insert(name.as_str(), loaded);
            }
        }
        let mut compiled = Self { map, bank, sounds };
        compiled.map.keys = (0..128u8).map(|key| compiled.key_zone(key)).collect();
        compiled
    }

    fn loaded(&self, name: &str) -> Option<&[(u32, Option<u8>)]> {
        let (key, _) = self.bank.resolve(name)?;
        self.sounds.get(key).map(Vec::as_slice)
    }

    /// The sample for a pattern sound: `bd:3` is the fourth `bd`, wrapping
    /// around when the sound has fewer variations.
    pub fn sound(&self, name: &str, index: u32) -> Option<u32> {
        let list = self.loaded(name)?;
        list.get(index as usize % list.len())
            .map(|&(sample, _)| sample)
    }

    fn key_zone(&self, key: u8) -> Option<KeyZone> {
        if let Some(pitched) = &self.bank.pitched {
            // The pitched sound across the keyboard, each key played by the
            // nearest recorded note. Samples without one count as C4.
            let (sample, root) = self
                .loaded(pitched)?
                .iter()
                .map(|&(sample, root)| (sample, root.unwrap_or(60)))
                .min_by_key(|&(_, root)| (i32::from(root) - i32::from(key)).abs())?;
            return Some(KeyZone {
                sample,
                semitones: f32::from(key) - f32::from(root),
            });
        }
        // Drum sounds on their General MIDI notes.
        sky_pattern::drum_names(key).find_map(|drum| {
            self.sound(drum, 0).map(|sample| KeyZone {
                sample,
                semitones: 0.0,
            })
        })
    }
}

fn rational_ticks(cycles: Rational64, cycle: Ticks) -> Ticks {
    let r = cycles * Rational64::from_integer(cycle);
    // Round to the nearest tick.
    let n = *r.numer();
    let d = *r.denom();
    if d == 0 {
        0
    } else {
        (2 * n + d).div_euclid(2 * d)
    }
}

fn track_node(
    project: &Project,
    track: &Track,
    index: usize,
    audio: &AudioCache,
    sr: u32,
    any_solo: bool,
    cx: &EffectContext,
) -> TrackNode {
    let tempo = &project.tempo;
    let srf = sr as f32;
    let source = match track.kind {
        TrackKind::Audio => {
            let mut clips = Vec::new();
            for clip in track.clips.iter().filter(|c| !c.muted) {
                let ClipContent::Audio {
                    source,
                    offset,
                    gain_db,
                    fade_in,
                    fade_out,
                } = &clip.content
                else {
                    continue;
                };
                let Some(data) = audio.get(source) else {
                    continue;
                };
                let start = ticks_to_samples(tempo, clip.start, sr);
                let end = ticks_to_samples(tempo, clip.end(), sr);
                clips.push(AudioClipNode {
                    start,
                    end,
                    data: data.clone(),
                    offset: *offset as f64,
                    rate: data.sample_rate as f64 / sr as f64,
                    gain: db_to_gain(*gain_db),
                    fade_in: ticks_to_samples(tempo, clip.start + fade_in, sr) - start,
                    fade_out: end - ticks_to_samples(tempo, clip.end() - fade_out, sr),
                });
            }
            SourceNode::Audio(clips)
        }
        TrackKind::Instrument => {
            let slot = track.instrument.as_ref();
            let bank = slot
                .filter(|s| s.kind == InstrumentType::Bank)
                .and_then(|s| s.bank.as_ref())
                .map(|b| CompiledBank::new(b, audio));
            let resolve: Box<Resolve> = match &bank {
                Some(bank) => Box::new(|v: &Value| match v {
                    Value::Note(n) => Some((*n, None)),
                    Value::Sound { name, index } => bank
                        .sound(name, *index)
                        .map(|s| (sky_pattern::drum_note(name).unwrap_or(0), Some(s))),
                    Value::Number(_) => None,
                }),
                None => Box::new(|v: &Value| v.midi_note().map(|n| (n, None))),
            };
            let mut notes = Vec::new();
            let mut locks = Locks::new(track);
            for clip in track.clips.iter().filter(|c| !c.muted) {
                match &clip.content {
                    ClipContent::Midi { notes: clip_notes } => {
                        for n in clip_notes.iter().filter(|n| n.start < clip.length) {
                            let begin = clip.start + n.start;
                            let end = clip.start + (n.start + n.length).min(clip.length);
                            let (lock_start, lock_end) = locks.push(|_| None);
                            notes.push(NoteEvent {
                                time: ticks_to_samples(tempo, begin, sr),
                                end: ticks_to_samples(tempo, end, sr),
                                pitch: n.pitch,
                                velocity: n.velocity,
                                sound: None,
                                lock_start,
                                lock_end,
                            });
                        }
                    }
                    ClipContent::Pattern { .. } => {
                        pattern_notes(tempo, sr, clip, &resolve, &mut locks, &mut notes);
                    }
                    ClipContent::Steps(steps) => {
                        step_notes(tempo, sr, clip, steps, &mut locks, &mut notes);
                    }
                    ClipContent::Audio { .. } => {}
                }
            }
            notes.sort_by_key(|n| (n.time, n.pitch, n.sound));
            drop(resolve);
            let instrument = slot.map(|slot| {
                let input = match (slot.kind, bank) {
                    (InstrumentType::Bank, Some(bank)) => InstrumentAudio::Bank(Arc::new(bank.map)),
                    _ => slot
                        .sample
                        .and_then(|id| audio.get(&id).cloned())
                        .map_or(InstrumentAudio::None, InstrumentAudio::Sample),
                };
                slot.kind.create_with(
                    srf,
                    input,
                    slot.params.iter().map(|(k, v)| (k.as_str(), *v)),
                )
            });
            SourceNode::Instrument {
                instrument,
                notes,
                locks: locks.out,
            }
        }
    };
    let audible = !any_solo || track.channel.solo;
    TrackNode {
        id: track.id,
        meter: index,
        source,
        channel: channel_node(&track.channel, audible, cx),
        state: TrackState::default(),
        buf: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
    }
}

/// Builds a snapshot of `project` for an engine running at `sample_rate`.
pub fn compile(
    project: &Project,
    audio: &AudioCache,
    sample_rate: u32,
    version: u64,
) -> EngineSnapshot {
    let any_solo = project.any_solo();
    let indices: HashMap<TrackId, usize> = project
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id, i))
        .collect();
    let cx = EffectContext {
        sample_rate: sample_rate as f32,
        bpm: project.tempo.initial_bpm(),
        tracks: &indices,
    };
    let tracks: Vec<TrackNode> = project
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| track_node(project, t, i, audio, sample_rate, any_solo, &cx))
        .collect();
    let triggers = tracks.iter().map(|_| Triggers::default()).collect();
    let loop_range = project.loop_enabled.then(|| {
        (
            ticks_to_samples(&project.tempo, project.loop_region.start, sample_rate),
            ticks_to_samples(&project.tempo, project.loop_region.end, sample_rate),
        )
    });
    EngineSnapshot {
        version,
        sample_rate,
        tracks,
        master: channel_node(&project.master, true, &cx),
        loop_range,
        triggers,
    }
}

impl EngineSnapshot {
    /// An empty snapshot, for an engine that has no project yet.
    pub fn empty(sample_rate: u32) -> Self {
        compile(&Project::new("Empty"), &AudioCache::new(), sample_rate, 0)
    }
}
