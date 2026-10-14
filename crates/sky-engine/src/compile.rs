// SPDX-License-Identifier: GPL-3.0-or-later
//! Compiling a project into an engine snapshot. Runs on the control thread,
//! where allocation is fine: everything the audio thread needs is built here.

use std::collections::HashMap;
use std::sync::Arc;

use num_rational::Rational64;
use sky_core::{AudioId, EffectId, TempoMap, Ticks, TrackId};
use sky_dsp::util::db_to_gain;
use sky_dsp::{AudioData, Effect, Instrument, MAX_BLOCK};
use sky_model::{Channel, Clip, ClipContent, Project, Track, TrackKind};

/// Decoded audio-pool files, shared between snapshots.
pub type AudioCache = HashMap<AudioId, Arc<AudioData>>;

/// A note with its end, both in samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteEvent {
    pub time: i64,
    pub end: i64,
    pub pitch: u8,
    pub velocity: u8,
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
    },
}

pub struct EffectNode {
    pub id: EffectId,
    pub enabled: bool,
    pub effect: Box<dyn Effect>,
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
}

fn channel_node(channel: &Channel, audible: bool, sample_rate: f32) -> ChannelNode {
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
            effect: slot.kind.create_with(
                sample_rate,
                slot.params.iter().map(|(k, v)| (k.as_str(), *v)),
            ),
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

fn pattern_notes(tempo: &TempoMap, sr: u32, clip: &Clip, out: &mut Vec<NoteEvent>) {
    let ClipContent::Pattern {
        source,
        seed,
        cycle,
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
    for ev in pattern.query(Rational64::from_integer(0), to, seed) {
        let Some(pitch) = ev.value.midi_note() else {
            continue;
        };
        let begin = clip_start + rational_ticks(ev.begin, cycle);
        let end = (clip_start + rational_ticks(ev.end, cycle)).min(clip_start + clip_len);
        if end <= begin {
            continue;
        }
        out.push(NoteEvent {
            time: ticks_to_samples(tempo, begin, sr),
            end: ticks_to_samples(tempo, end, sr),
            pitch,
            velocity: 100,
        });
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
) -> TrackNode {
    let tempo = &project.tempo;
    let srf = sr as f32;
    let source = match track.kind {
        TrackKind::Audio => {
            let mut clips = Vec::new();
            for clip in &track.clips {
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
            let mut notes = Vec::new();
            for clip in &track.clips {
                match &clip.content {
                    ClipContent::Midi { notes: clip_notes } => {
                        for n in clip_notes.iter().filter(|n| n.start < clip.length) {
                            let begin = clip.start + n.start;
                            let end = clip.start + (n.start + n.length).min(clip.length);
                            notes.push(NoteEvent {
                                time: ticks_to_samples(tempo, begin, sr),
                                end: ticks_to_samples(tempo, end, sr),
                                pitch: n.pitch,
                                velocity: n.velocity,
                            });
                        }
                    }
                    ClipContent::Pattern { .. } => pattern_notes(tempo, sr, clip, &mut notes),
                    ClipContent::Audio { .. } => {}
                }
            }
            notes.sort_by_key(|n| (n.time, n.pitch));
            let instrument = track.instrument.as_ref().map(|slot| {
                let sample = slot.sample.and_then(|id| audio.get(&id).cloned());
                slot.kind.create_with(
                    srf,
                    sample,
                    slot.params.iter().map(|(k, v)| (k.as_str(), *v)),
                )
            });
            SourceNode::Instrument { instrument, notes }
        }
    };
    let audible = !any_solo || track.channel.solo;
    TrackNode {
        id: track.id,
        meter: index,
        source,
        channel: channel_node(&track.channel, audible, srf),
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
    let tracks = project
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| track_node(project, t, i, audio, sample_rate, any_solo))
        .collect();
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
        master: channel_node(&project.master, true, sample_rate as f32),
        loop_range,
    }
}

impl EngineSnapshot {
    /// An empty snapshot, for an engine that has no project yet.
    pub fn empty(sample_rate: u32) -> Self {
        compile(&Project::new("Empty"), &AudioCache::new(), sample_rate, 0)
    }
}
