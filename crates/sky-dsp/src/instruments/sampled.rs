// SPDX-License-Identifier: GPL-3.0-or-later
//! The multi-sample engine: zones that each play a sample over a range of
//! keys and velocities, the way an SFZ file lays an instrument out.
//!
//! A [`SampleLibrary`] is built once on the control thread: samples held as
//! 16-bit audio, the zones, and a 128 by 128 table from key and velocity to
//! zones, so the audio thread never searches. [`SampleVoices`] plays it
//! with 4-point cubic interpolation. When every voice is busy, the oldest
//! fades out over a few milliseconds instead of jumping to a new sample.
//!
//! The sustain pedal holds keys after they come up, as a piano's dampers
//! stay off the strings: their release waits for the pedal to come up. A
//! key struck again while it still sounds fades the old note out quickly,
//! and keys from [`SampleVoices::set_undamped_from`] up ring on without
//! dampers at all.

use std::sync::Arc;

use crate::AudioData;
use crate::filters::{Svf, SvfCoeffs};
use crate::instruments::Adsr;
use crate::util::{db_to_gain, pan_gains};

/// Voices that may sound at once.
pub const MAX_VOICES: usize = 96;
/// Voice slots: the sounding voices plus room for stolen ones to fade.
const SLOTS: usize = MAX_VOICES + 32;
/// How long a stolen voice takes to fade out.
const STEAL_MS: f32 = 5.0;
/// How long the note a key still sounds takes to fade when the key is
/// struck again.
const RESTRIKE_MS: f32 = 50.0;
/// How long an undamped key rings after it comes up, at the zone's own
/// release scale.
const UNDAMPED_RING_S: f32 = 3.0;
/// Zero frames kept before and after each sample, so interpolation can
/// read past both ends without checks.
const PAD_FRONT: usize = 1;
const PAD_BACK: usize = 3;
/// Velocities 1 to 127 index the table; 0 is treated as 1.
const KEYS: usize = 128;

/// One sample, as 16-bit interleaved audio of one or two channels.
#[derive(Debug, PartialEq)]
pub struct SampleData {
    pub sample_rate: u32,
    channels: usize,
    frames: usize,
    /// `PAD_FRONT` silent frames, the audio, then `PAD_BACK` silent frames.
    data: Vec<i16>,
}

fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

impl SampleData {
    /// Converts decoded audio, keeping its first two channels.
    pub fn from_audio(audio: &AudioData) -> Self {
        let channels = audio.channel_count().clamp(1, 2);
        let frames = audio.frames();
        let mut data = Vec::with_capacity((frames + PAD_FRONT + PAD_BACK) * channels);
        data.resize(PAD_FRONT * channels, 0);
        if channels == 2 {
            for (l, r) in audio.left().iter().zip(audio.right()) {
                data.push(to_i16(*l));
                data.push(to_i16(*r));
            }
        } else {
            data.extend(audio.left().iter().map(|s| to_i16(*s)));
        }
        data.resize((frames + PAD_FRONT + PAD_BACK) * channels, 0);
        Self {
            sample_rate: audio.sample_rate.max(1),
            channels,
            frames,
            data,
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Memory the audio takes.
    pub fn bytes(&self) -> usize {
        self.data.len() * std::mem::size_of::<i16>()
    }

    /// Frame `i` of the padded data, as left and right.
    #[inline]
    fn frame(&self, i: usize) -> (f32, f32) {
        const SCALE: f32 = 1.0 / 32_768.0;
        if self.channels == 2 {
            let j = i * 2;
            (
                f32::from(self.data[j]) * SCALE,
                f32::from(self.data[j + 1]) * SCALE,
            )
        } else {
            let s = f32::from(self.data[i]) * SCALE;
            (s, s)
        }
    }
}

/// When a zone sounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// On note-on.
    Attack,
    /// On note-off, with the velocity the note started with. While the
    /// sustain pedal is down, on pedal up instead.
    Release,
    /// When the sustain pedal goes down, at the zone's root key.
    PedalDown,
    /// When the sustain pedal comes up, at the zone's root key.
    PedalUp,
}

/// How a zone's sample repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopMode {
    /// Plays once; a note-off releases it.
    NoLoop,
    /// Plays to its end whatever the key does.
    OneShot,
    /// Repeats the loop until the voice has faded out.
    Continuous,
    /// Repeats the loop while the key is held, then plays on to the end.
    Sustain,
}

/// A fixed low-pass filter on a zone, as SFZ's `fil_type` and `cutoff`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoneFilter {
    pub cutoff: f32,
    /// 1 or 2.
    pub poles: u8,
    /// Resonance in dB; 0 is a plain Butterworth response.
    pub resonance_db: f32,
}

/// One sample laid over a range of keys and velocities.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    /// Index into the library's samples.
    pub sample: u32,
    pub lokey: u8,
    pub hikey: u8,
    pub lovel: u8,
    pub hivel: u8,
    /// The key the sample plays at its recorded pitch.
    pub root: u8,
    /// Semitones per key away from the root; 1 is normal.
    pub keytrack: f32,
    /// A shift in semitones on top of the key's pitch.
    pub tune: f32,
    pub gain_db: f32,
    /// -1 (left) to 1 (right).
    pub pan: f32,
    /// Stereo width of a two-channel sample: 1 as recorded, 0 mono.
    pub width: f32,
    pub trigger: Trigger,
    /// For release zones: how much quieter, in dB per second the key was
    /// held, the release sample plays.
    pub rt_decay: f32,
    pub loop_mode: LoopMode,
    /// First and last frame of the loop, inclusive.
    pub loop_start: u32,
    pub loop_end: u32,
    /// Where playing starts.
    pub offset: u32,
    /// The last frame played, inclusive; the end of the sample when unset.
    pub end: Option<u32>,
    /// Round robin: with a length above 1, the zone plays on every
    /// `seq_length`th note of its key, the `seq_position`th (from 1).
    pub seq_length: u8,
    pub seq_position: u8,
    /// Random choice: the zone plays when a number drawn for the note, from
    /// 0 up to 1, falls in `lorand..hirand`.
    pub lorand: f32,
    pub hirand: f32,
    /// Release time in seconds after the key comes up.
    pub release: f32,
    pub filter: Option<ZoneFilter>,
}

impl Zone {
    /// A zone that plays `sample` on every key and velocity, recorded at
    /// `root`.
    pub fn new(sample: u32, root: u8) -> Self {
        Self {
            sample,
            lokey: 0,
            hikey: 127,
            lovel: 1,
            hivel: 127,
            root,
            keytrack: 1.0,
            tune: 0.0,
            gain_db: 0.0,
            pan: 0.0,
            width: 1.0,
            trigger: Trigger::Attack,
            rt_decay: 0.0,
            loop_mode: LoopMode::NoLoop,
            loop_start: 0,
            loop_end: 0,
            offset: 0,
            end: None,
            seq_length: 1,
            seq_position: 1,
            lorand: 0.0,
            hirand: 1.0,
            release: 0.001,
            filter: None,
        }
    }

    fn covers(&self, key: u8, vel: u8) -> bool {
        (self.lokey..=self.hikey).contains(&key) && (self.lovel..=self.hivel).contains(&vel)
    }

    /// Whether the zone plays on note `n` of its key, drawn `rand`.
    fn picked(&self, n: u32, rand: f32) -> bool {
        let len = u32::from(self.seq_length);
        (len <= 1 || n % len + 1 == u32::from(self.seq_position)) && self.drawn(rand)
    }

    /// Whether the zone plays on draw `rand`.
    fn drawn(&self, rand: f32) -> bool {
        (self.lorand..self.hirand).contains(&rand)
    }
}

/// For each key and velocity, the slice of zone indices that sound.
#[derive(Debug, Default, PartialEq)]
struct ZoneTable {
    cells: Vec<(u32, u32)>,
    zones: Vec<u32>,
}

impl ZoneTable {
    fn build(zones: &[Zone], trigger: Trigger) -> Self {
        let mut table = ZoneTable {
            cells: Vec::with_capacity(KEYS * KEYS),
            zones: Vec::new(),
        };
        for key in 0..KEYS as u8 {
            for vel in 0..KEYS as u8 {
                let start = table.zones.len() as u32;
                let v = vel.max(1);
                table.zones.extend(
                    zones
                        .iter()
                        .enumerate()
                        .filter(|(_, z)| z.trigger == trigger && z.covers(key, v))
                        .map(|(i, _)| i as u32),
                );
                table.cells.push((start, table.zones.len() as u32));
            }
        }
        table
    }

    fn get(&self, key: u8, vel: u8) -> &[u32] {
        let i = usize::from(key.min(127)) * KEYS + usize::from(vel.min(127));
        self.cells
            .get(i)
            .and_then(|&(s, e)| self.zones.get(s as usize..e as usize))
            .unwrap_or_default()
    }
}

/// A multi-sampled instrument ready to play: samples, zones and the lookup
/// table. Built once and shared, through an `Arc`, by every track that
/// plays it.
#[derive(Debug, Default, PartialEq)]
pub struct SampleLibrary {
    pub name: String,
    samples: Vec<SampleData>,
    zones: Vec<Zone>,
    attack: ZoneTable,
    release: ZoneTable,
    pedal_down: Vec<u32>,
    pedal_up: Vec<u32>,
}

impl SampleLibrary {
    /// Builds the library. Zones whose sample is missing are dropped, and
    /// offsets, ends and loops are kept inside their samples.
    pub fn new(name: impl Into<String>, samples: Vec<SampleData>, zones: Vec<Zone>) -> Self {
        let zones: Vec<Zone> = zones
            .into_iter()
            .filter_map(|mut z| {
                let frames = samples.get(z.sample as usize)?.frames() as u32;
                if frames < 2 {
                    return None;
                }
                let last = frames - 1;
                z.end = z.end.map(|e| e.min(last));
                let end = z.end.unwrap_or(last);
                z.offset = z.offset.min(end);
                if z.loop_mode == LoopMode::Continuous || z.loop_mode == LoopMode::Sustain {
                    z.loop_end = if z.loop_end == 0 {
                        end
                    } else {
                        z.loop_end.min(end)
                    };
                    if z.loop_start + 4 > z.loop_end {
                        z.loop_mode = LoopMode::NoLoop;
                    }
                }
                z.lovel = z.lovel.max(1);
                z.lorand = z.lorand.clamp(0.0, 1.0);
                z.hirand = z.hirand.clamp(0.0, 1.0);
                z.seq_length = z.seq_length.max(1);
                z.seq_position = z.seq_position.clamp(1, z.seq_length);
                Some(z)
            })
            .collect();
        let with = |trigger| {
            (0..zones.len() as u32)
                .filter(|&i| zones[i as usize].trigger == trigger)
                .collect()
        };
        Self {
            name: name.into(),
            attack: ZoneTable::build(&zones, Trigger::Attack),
            release: ZoneTable::build(&zones, Trigger::Release),
            pedal_down: with(Trigger::PedalDown),
            pedal_up: with(Trigger::PedalUp),
            samples,
            zones,
        }
    }

    pub fn samples(&self) -> &[SampleData] {
        &self.samples
    }

    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    /// The zones `key` at velocity `vel` plays, as indices into
    /// [`zones`](Self::zones). Pedal zones play whatever the key.
    pub fn zones_at(&self, trigger: Trigger, key: u8, vel: u8) -> &[u32] {
        match trigger {
            Trigger::Attack => self.attack.get(key, vel),
            Trigger::Release => self.release.get(key, vel),
            Trigger::PedalDown => &self.pedal_down,
            Trigger::PedalUp => &self.pedal_up,
        }
    }

    /// Memory the sample audio takes.
    pub fn bytes(&self) -> usize {
        self.samples.iter().map(SampleData::bytes).sum()
    }

    /// True when a sample has two channels.
    pub fn is_stereo(&self) -> bool {
        self.samples.iter().any(|s| s.channels == 2)
    }

    /// The lowest and highest keys any zone plays.
    pub fn key_range(&self) -> Option<(u8, u8)> {
        let lo = self.zones.iter().map(|z| z.lokey).min()?;
        let hi = self.zones.iter().map(|z| z.hikey).max()?;
        Some((lo, hi))
    }

    /// How many velocity layers `key` has: the distinct velocity ranges of
    /// its attack zones.
    pub fn layers(&self, key: u8) -> usize {
        let mut ranges: Vec<(u8, u8)> = self
            .zones
            .iter()
            .filter(|z| z.trigger == Trigger::Attack && (z.lokey..=z.hikey).contains(&key))
            .map(|z| (z.lovel, z.hivel))
            .collect();
        ranges.sort_unstable();
        ranges.dedup();
        ranges.len()
    }

    /// True when any zone plays on note-off.
    pub fn has_release_zones(&self) -> bool {
        self.zones.iter().any(|z| z.trigger == Trigger::Release)
    }

    /// True when the library has recordings of the sustain pedal.
    pub fn has_pedal_noise(&self) -> bool {
        !self.pedal_down.is_empty() || !self.pedal_up.is_empty()
    }
}

/// How a note plays, decided by the instrument for each note.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteShape {
    /// Linear gain, on top of each zone's own.
    pub gain: f32,
    /// Semitones on top of each zone's pitch.
    pub tune: f32,
    /// -1 (left) to 1 (right), on top of each zone's pan.
    pub pan: f32,
    /// More pan for mono samples only, such as a key's place on a piano.
    pub mono_pan: f32,
    /// Stereo width of two-channel samples: 1 as recorded, 0 mono, -1 left
    /// and right swapped.
    pub width: f32,
    /// Scales each zone's release time.
    pub release: f32,
}

impl Default for NoteShape {
    fn default() -> Self {
        Self {
            gain: 1.0,
            tune: 0.0,
            pan: 0.0,
            mono_pan: 0.0,
            width: 1.0,
            release: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum VoiceFilter {
    None,
    OnePole { a: f32, l: f32, r: f32 },
    TwoPole { c: SvfCoeffs, l: Svf, r: Svf },
}

impl VoiceFilter {
    fn new(filter: Option<ZoneFilter>, sample_rate: f32) -> Self {
        match filter {
            None => VoiceFilter::None,
            Some(f) if f.poles <= 1 => {
                let fc = f.cutoff.clamp(10.0, sample_rate * 0.49);
                VoiceFilter::OnePole {
                    a: 1.0 - (-2.0 * std::f32::consts::PI * fc / sample_rate).exp(),
                    l: 0.0,
                    r: 0.0,
                }
            }
            Some(f) => {
                // Q 0.707 at 0 dB of resonance, as SFZ's lpf_2p.
                let q = std::f32::consts::FRAC_1_SQRT_2 * db_to_gain(f.resonance_db);
                let resonance = ((2.0 - 1.0 / q.max(0.1)) / 1.95).clamp(0.0, 1.0);
                VoiceFilter::TwoPole {
                    c: SvfCoeffs::new(f.cutoff, resonance, sample_rate),
                    l: Svf::default(),
                    r: Svf::default(),
                }
            }
        }
    }

    #[inline]
    fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        match self {
            VoiceFilter::None => (l, r),
            VoiceFilter::OnePole { a, l: zl, r: zr } => {
                *zl += *a * (l - *zl);
                *zr += *a * (r - *zr);
                (*zl, *zr)
            }
            VoiceFilter::TwoPole { c, l: sl, r: sr } => (sl.tick(c, l).low, sr.tick(c, r).low),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    active: bool,
    zone: u32,
    key: u8,
    /// Frame position in the sample, not counting the padding.
    pos: f64,
    rate: f64,
    /// One past the last frame played.
    end: f64,
    looping: bool,
    /// The key is up (or the voice plays on by itself).
    released: bool,
    /// Output matrix: left from sample left and right, right from both.
    mix: [f32; 4],
    env: Adsr,
    /// 1 while playing; above 0 and falling when stolen.
    fade: f32,
    /// How much `fade` falls each frame once stolen.
    fade_step: f32,
    stolen: bool,
    age: u64,
    filter: VoiceFilter,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            active: false,
            zone: 0,
            key: 0,
            pos: 0.0,
            rate: 1.0,
            end: 0.0,
            looping: false,
            released: false,
            mix: [1.0, 0.0, 0.0, 1.0],
            env: Adsr::default(),
            fade: 1.0,
            fade_step: 1.0,
            stolen: false,
            age: 0,
            filter: VoiceFilter::None,
        }
    }
}

/// Hermite 4-point interpolation between `x0` and `x1` at `t`.
#[inline]
fn cubic(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

/// Plays a [`SampleLibrary`]: picks zones for each note, runs the voices
/// and mixes them. The instrument on top decides gains and pitch.
pub struct SampleVoices {
    sample_rate: f32,
    library: Arc<SampleLibrary>,
    voices: Vec<Voice>,
    /// Counts note starts, to find the oldest voice.
    clock: u64,
    /// Frames rendered, to time how long keys are held.
    frames: u64,
    /// Each key's note count, for round robins.
    seq: [u32; KEYS],
    /// Which keys are down, since when and how hard, for release zones.
    down: [bool; KEYS],
    held_at: [u64; KEYS],
    held_vel: [u8; KEYS],
    /// The sustain pedal is down.
    pedal: bool,
    /// Keys that came up while the pedal was down and still sound.
    sustained: [bool; KEYS],
    /// The lowest key without a damper.
    undamped_from: u8,
    /// The state of the random draws for `lorand` and `hirand`.
    rand: u32,
}

impl SampleVoices {
    pub fn new(sample_rate: f32, library: Arc<SampleLibrary>) -> Self {
        Self {
            sample_rate,
            library,
            voices: vec![Voice::default(); SLOTS],
            clock: 0,
            frames: 0,
            seq: [0; KEYS],
            down: [false; KEYS],
            held_at: [0; KEYS],
            held_vel: [0; KEYS],
            pedal: false,
            sustained: [false; KEYS],
            undamped_from: 128,
            rand: 0x9e37_79b9,
        }
    }

    /// Keys from `key` up have no dampers: they ring on after they come
    /// up, as on a piano. 128 (the default) damps every key.
    pub fn set_undamped_from(&mut self, key: u8) {
        self.undamped_from = key.min(128);
    }

    /// A number from 0 up to (not including) 1.
    fn draw(&mut self) -> f32 {
        // xorshift32
        let mut x = self.rand;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rand = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    pub fn library(&self) -> &Arc<SampleLibrary> {
        &self.library
    }

    /// Voices sounding, not counting stolen ones fading out.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active && !v.stolen).count()
    }

    /// A free slot, fading out the oldest voice first when all
    /// [`MAX_VOICES`] are sounding.
    fn alloc(&mut self) -> usize {
        if self.active_voices() >= MAX_VOICES {
            // Released voices go first, the oldest of them.
            let victim = self
                .voices
                .iter()
                .enumerate()
                .filter(|(_, v)| v.active && !v.stolen)
                .min_by_key(|(_, v)| (!v.released, v.age))
                .map(|(i, _)| i);
            if let Some(i) = victim {
                self.voices[i].stolen = true;
            }
        }
        if let Some(i) = self.voices.iter().position(|v| !v.active) {
            return i;
        }
        // Every slot is busy fading: take the quietest.
        self.voices
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.fade.total_cmp(&b.1.fade))
            .map_or(0, |(i, _)| i)
    }

    fn start(&mut self, zone_index: u32, key: u8, gain: f32, shape: &NoteShape) {
        let library = Arc::clone(&self.library);
        let Some(zone) = library.zones.get(zone_index as usize) else {
            return;
        };
        let Some(sample) = library.samples.get(zone.sample as usize) else {
            return;
        };
        let i = self.alloc();
        self.clock += 1;
        let semitones =
            (f32::from(key) - f32::from(zone.root)) * zone.keytrack + zone.tune + shape.tune;
        let rate = 2f64.powf(f64::from(semitones) / 12.0) * f64::from(sample.sample_rate)
            / f64::from(self.sample_rate);
        let gain = gain * db_to_gain(zone.gain_db);
        let stereo = sample.channels == 2;
        let pan = zone.pan + shape.pan + if stereo { 0.0 } else { shape.mono_pan };
        let (pl, pr) = pan_gains(pan);
        let mix = if stereo {
            let w = (zone.width * shape.width).clamp(-1.0, 1.0);
            let (near, far) = ((1.0 + w.abs()) * 0.5, (1.0 - w.abs()) * 0.5);
            let (ll, rl, lr, rr) = if w < 0.0 {
                (far, near, near, far)
            } else {
                (near, far, far, near)
            };
            [
                ll * pl * gain,
                rl * pl * gain,
                lr * pr * gain,
                rr * pr * gain,
            ]
        } else {
            // Both "channels" of a mono sample are the same.
            [
                0.5 * pl * gain,
                0.5 * pl * gain,
                0.5 * pr * gain,
                0.5 * pr * gain,
            ]
        };
        let end = f64::from(zone.end.map_or(sample.frames as u32, |e| e + 1));
        let looping = matches!(zone.loop_mode, LoopMode::Continuous | LoopMode::Sustain);
        let release_ms = (zone.release * shape.release * 1000.0).max(1.0);
        let v = &mut self.voices[i];
        *v = Voice {
            active: true,
            zone: zone_index,
            key,
            pos: f64::from(zone.offset),
            rate,
            end,
            looping,
            released: zone.trigger != Trigger::Attack || zone.loop_mode == LoopMode::OneShot,
            mix,
            env: Adsr::default(),
            fade: 1.0,
            fade_step: 1.0 / (STEAL_MS * 0.001 * self.sample_rate).max(1.0),
            stolen: false,
            age: self.clock,
            filter: VoiceFilter::new(zone.filter, self.sample_rate),
        };
        v.env.set(0.0, 1.0, 1.0, release_ms, self.sample_rate);
        v.env.trigger();
    }

    /// Starts the zones of `key` at velocity `vel` (1-127). `shape.gain`
    /// is the note's gain, from the velocity as the instrument sees it.
    /// The note the key still sounds, held by the pedal or ringing without
    /// a damper, fades out.
    pub fn note_on(&mut self, key: u8, vel: u8, shape: &NoteShape) {
        let key = key.min(127);
        let vel = vel.clamp(1, 127);
        let k = usize::from(key);
        let restrike = 1.0 / (RESTRIKE_MS * 0.001 * self.sample_rate).max(1.0);
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.active && !v.stolen && v.key == key)
        {
            if self.library.zones[v.zone as usize].trigger == Trigger::Attack {
                v.stolen = true;
                v.fade_step = restrike;
            }
        }
        let n = self.seq[k];
        self.seq[k] = n.wrapping_add(1);
        self.down[k] = true;
        self.sustained[k] = false;
        self.held_at[k] = self.frames;
        self.held_vel[k] = vel;
        let rand = self.draw();
        let library = Arc::clone(&self.library);
        for &z in library.zones_at(Trigger::Attack, key, vel) {
            if library.zones[z as usize].picked(n, rand) {
                self.start(z, key, shape.gain, shape);
            }
        }
    }

    /// Releases the voices of `key` and starts its release zones, quieter
    /// the longer the key was held. While the pedal is down the key goes
    /// on sounding until the pedal comes up; a key without a damper rings
    /// on whatever the pedal does.
    pub fn note_off(&mut self, key: u8, shape: &NoteShape) {
        let key = key.min(127);
        let k = usize::from(key);
        if !std::mem::take(&mut self.down[k]) {
            return;
        }
        if self.pedal {
            self.sustained[k] = true;
            return;
        }
        self.damp(key, shape);
    }

    /// The key is up and the pedal too: its dampers fall on the strings.
    fn damp(&mut self, key: u8, shape: &NoteShape) {
        let k = usize::from(key);
        let undamped = key >= self.undamped_from;
        let ring_ms = UNDAMPED_RING_S * 1000.0 * shape.release;
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.active && !v.released && v.key == key)
        {
            v.released = true;
            let zone = &self.library.zones[v.zone as usize];
            if zone.loop_mode == LoopMode::Sustain {
                v.looping = false;
            } else if undamped {
                // The string rings out; a looped sample needs an end.
                if v.looping {
                    v.env.set(0.0, 1.0, 1.0, ring_ms, self.sample_rate);
                    v.env.release();
                }
            } else {
                v.env.release();
            }
        }
        let seconds = (self.frames - self.held_at[k]) as f32 / self.sample_rate;
        let rand = self.draw();
        let library = Arc::clone(&self.library);
        for &z in library.zones_at(Trigger::Release, key, self.held_vel[k]) {
            let zone = &library.zones[z as usize];
            if zone.drawn(rand) {
                let decay = db_to_gain(-zone.rt_decay * seconds);
                self.start(z, key, shape.gain * decay, shape);
            }
        }
    }

    /// Moves the sustain pedal. Up, the keys it held stop the way they
    /// would have when they came up. `noise` shapes the pedal's own
    /// recordings, when the library has them; `key_shape` gives the shape
    /// of each key it stops.
    pub fn set_pedal(
        &mut self,
        down: bool,
        noise: &NoteShape,
        key_shape: impl Fn(u8) -> NoteShape,
    ) {
        if down == self.pedal {
            return;
        }
        self.pedal = down;
        if !down {
            for key in 0..KEYS as u8 {
                if std::mem::take(&mut self.sustained[usize::from(key)]) {
                    self.damp(key, &key_shape(key));
                }
            }
        }
        let trigger = if down {
            Trigger::PedalDown
        } else {
            Trigger::PedalUp
        };
        let rand = self.draw();
        let library = Arc::clone(&self.library);
        for &z in library.zones_at(trigger, 0, 1) {
            let zone = &library.zones[z as usize];
            if zone.drawn(rand) {
                let root = zone.root;
                self.start(z, root, noise.gain, noise);
            }
        }
    }

    /// Whether the sustain pedal is down.
    pub fn pedal(&self) -> bool {
        self.pedal
    }

    /// Releases every held key, without release zones, and lifts the
    /// pedal without a sound.
    pub fn release_all(&mut self) {
        self.down = [false; KEYS];
        self.sustained = [false; KEYS];
        self.pedal = false;
        for v in self.voices.iter_mut().filter(|v| v.active) {
            v.released = true;
            v.looping = false;
            v.env.release();
        }
    }

    /// Silences everything at once.
    pub fn reset(&mut self) {
        self.down = [false; KEYS];
        self.sustained = [false; KEYS];
        self.pedal = false;
        for v in &mut self.voices {
            v.active = false;
            v.env.reset();
        }
    }

    /// Adds the voices into `left` and `right`.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        let library = &*self.library;
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let zone = &library.zones[v.zone as usize];
            let sample = &library.samples[zone.sample as usize];
            let loop_first = f64::from(zone.loop_start);
            let loop_next = f64::from(zone.loop_end) + 1.0;
            let loop_len = loop_next - loop_first;
            // The loop in padded frames: past its last frame comes its first.
            let last = zone.loop_end as usize + PAD_FRONT;
            let span = (zone.loop_end - zone.loop_start + 1) as usize;
            let [ll, rl, lr, rr] = v.mix;
            for (l, r) in left[..n].iter_mut().zip(right[..n].iter_mut()) {
                if v.looping && v.pos >= loop_next {
                    v.pos = loop_first + (v.pos - loop_first) % loop_len;
                }
                if v.pos >= v.end {
                    v.active = false;
                    break;
                }
                let i = v.pos as usize;
                let t = (v.pos - i as f64) as f32;
                let p = i + PAD_FRONT;
                let (a, b, c, d) = if v.looping && p + 2 > last {
                    let at = |j: usize| sample.frame(if j > last { j - span } else { j });
                    (at(p - 1), at(p), at(p + 1), at(p + 2))
                } else {
                    (
                        sample.frame(p - 1),
                        sample.frame(p),
                        sample.frame(p + 1),
                        sample.frame(p + 2),
                    )
                };
                let sl = cubic(a.0, b.0, c.0, d.0, t);
                let sr = cubic(a.1, b.1, c.1, d.1, t);
                let (sl, sr) = v.filter.tick(sl, sr);
                let mut g = v.env.tick();
                if v.stolen {
                    v.fade -= v.fade_step;
                    if v.fade <= 0.0 {
                        v.active = false;
                        break;
                    }
                    g *= v.fade;
                }
                *l += (ll * sl + rl * sr) * g;
                *r += (lr * sl + rr * sr) * g;
                v.pos += v.rate;
            }
            if v.active && !v.env.is_active() {
                v.active = false;
            }
        }
        self.frames += n as u64;
    }

    /// The zone of the voice started last, for tests.
    #[cfg(test)]
    pub(crate) fn newest_zone(&self) -> Option<u32> {
        self.voices
            .iter()
            .filter(|v| v.active)
            .max_by_key(|v| v.age)
            .map(|v| v.zone)
    }

    /// Takes over the voices of `previous` when it plays the same library,
    /// so a graph rebuild does not cut notes off.
    pub fn adopt(&mut self, previous: &mut SampleVoices) -> bool {
        if !Arc::ptr_eq(&self.library, &previous.library) {
            return false;
        }
        std::mem::swap(&mut self.voices, &mut previous.voices);
        self.clock = previous.clock;
        self.frames = previous.frames;
        self.seq = previous.seq;
        self.down = previous.down;
        self.held_at = previous.held_at;
        self.held_vel = previous.held_vel;
        self.pedal = previous.pedal;
        self.sustained = previous.sustained;
        self.rand = previous.rand;
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A sine of `frames` frames at `hz`, recorded at 48 kHz.
    pub(crate) fn sine(frames: usize, hz: f32, channels: usize) -> SampleData {
        let wave: Vec<f32> = (0..frames)
            .map(|i| (i as f32 * hz * std::f32::consts::TAU / 48_000.0).sin() * 0.5)
            .collect();
        SampleData::from_audio(&AudioData {
            sample_rate: 48_000,
            channels: vec![wave; channels],
        })
    }

    /// Two layers on keys 48-72 (soft below 64, loud from 64), and a
    /// release zone.
    pub(crate) fn library() -> Arc<SampleLibrary> {
        let soft = Zone {
            lokey: 48,
            hikey: 72,
            hivel: 63,
            ..Zone::new(0, 60)
        };
        let loud = Zone {
            sample: 1,
            lovel: 64,
            hivel: 127,
            ..soft.clone()
        };
        let release = Zone {
            trigger: Trigger::Release,
            rt_decay: 6.0,
            lovel: 1,
            hivel: 127,
            ..Zone::new(2, 60)
        };
        Arc::new(SampleLibrary::new(
            "test",
            vec![
                sine(48_000, 440.0, 1),
                sine(48_000, 440.0, 2),
                sine(4_800, 1_000.0, 1),
            ],
            vec![soft, loud, release],
        ))
    }

    fn render(v: &mut SampleVoices, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (lc, rc) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            v.render(lc, rc);
        }
        (l, r)
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn the_table_finds_zones_by_key_and_velocity() {
        let lib = library();
        assert_eq!(lib.zones_at(Trigger::Attack, 60, 30), &[0]);
        assert_eq!(lib.zones_at(Trigger::Attack, 60, 100), &[1]);
        assert!(lib.zones_at(Trigger::Attack, 40, 100).is_empty());
        assert_eq!(lib.zones_at(Trigger::Release, 30, 1), &[2]);
        assert_eq!(lib.layers(60), 2);
        assert_eq!(lib.key_range(), Some((0, 127)));
        assert!(lib.is_stereo() && lib.has_release_zones());
    }

    #[test]
    fn notes_play_at_the_right_pitch() {
        let lib = library();
        for (key, expect) in [
            (60, 440.0),
            (72, 880.0),
            (55, 440.0 * 2f32.powf(-5.0 / 12.0)),
        ] {
            let mut v = SampleVoices::new(48_000.0, lib.clone());
            v.note_on(key, 100, &NoteShape::default());
            let (l, _) = render(&mut v, 24_000);
            let ups = l.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() as f32;
            assert!(
                (ups * 2.0 - expect).abs() < 4.0,
                "key {key}: {} Hz",
                ups * 2.0
            );
        }
    }

    #[test]
    fn release_zones_fire_quieter_after_long_notes() {
        let lib = library();
        let tail = |held: usize| {
            let mut v = SampleVoices::new(48_000.0, lib.clone());
            v.note_on(60, 100, &NoteShape::default());
            let _ = render(&mut v, held);
            let shape = NoteShape {
                release: 0.001,
                ..NoteShape::default()
            };
            v.note_off(60, &shape);
            // The attack voice dies within a few ms; the release zone rings.
            let _ = render(&mut v, 480);
            peak(&render(&mut v, 2_400).0)
        };
        let short = tail(480);
        let long = tail(48_000);
        assert!(short > 0.1, "{short}");
        // Held one second longer at 6 dB per second: about half as loud.
        assert!((long / short - 0.5).abs() < 0.08, "{short} -> {long}");
        // A key that was not down starts nothing.
        let mut v = SampleVoices::new(48_000.0, lib.clone());
        v.note_off(60, &NoteShape::default());
        assert_eq!(v.active_voices(), 0);
    }

    #[test]
    fn loops_keep_a_note_ringing_and_stop_on_release() {
        let zone = Zone {
            loop_mode: LoopMode::Continuous,
            loop_start: 1_000,
            loop_end: 1_999,
            release: 0.01,
            ..Zone::new(0, 60)
        };
        let lib = Arc::new(SampleLibrary::new(
            "loop",
            vec![sine(2_400, 480.0, 1)],
            vec![zone],
        ));
        let mut v = SampleVoices::new(48_000.0, lib);
        v.note_on(60, 127, &NoteShape::default());
        let (l, _) = render(&mut v, 48_000);
        assert!(peak(&l[40_000..]) > 0.2, "the loop stopped");
        // 1000 frames of a 480 Hz sine at 48 kHz is ten whole cycles, so the
        // seam is smooth: no step bigger than the wave's own.
        let step = l
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(step < 0.04, "seam step {step}");
        v.note_off(60, &NoteShape::default());
        let _ = render(&mut v, 4_800);
        assert_eq!(v.active_voices(), 0);
    }

    #[test]
    fn round_robins_take_turns() {
        let a = Zone {
            seq_length: 2,
            seq_position: 1,
            ..Zone::new(0, 60)
        };
        let b = Zone {
            seq_position: 2,
            sample: 1,
            ..a.clone()
        };
        let lib = Arc::new(SampleLibrary::new(
            "rr",
            vec![sine(4_800, 440.0, 1), sine(4_800, 440.0, 1)],
            vec![a, b],
        ));
        let mut v = SampleVoices::new(48_000.0, lib);
        let mut played = Vec::new();
        for _ in 0..4 {
            v.note_on(60, 100, &NoteShape::default());
            let newest = v.voices.iter().filter(|x| x.active).max_by_key(|x| x.age);
            played.push(newest.map(|x| x.zone));
        }
        assert_eq!(played, [Some(0), Some(1), Some(0), Some(1)]);
    }

    #[test]
    fn stolen_voices_fade_instead_of_clicking() {
        // One zone across the keyboard, so every voice is a key of its own.
        let lib = Arc::new(SampleLibrary::new(
            "all",
            vec![sine(48_000, 440.0, 1)],
            vec![Zone::new(0, 60)],
        ));
        let mut v = SampleVoices::new(48_000.0, lib);
        for i in 0..MAX_VOICES {
            v.note_on(16 + i as u8, 100, &NoteShape::default());
        }
        let _ = render(&mut v, 256);
        assert_eq!(v.active_voices(), MAX_VOICES);
        let (before, _) = render(&mut v, 64);
        v.note_on(15, 100, &NoteShape::default());
        assert_eq!(v.active_voices(), MAX_VOICES);
        let (after, _) = render(&mut v, 1_024);
        // The output moves on smoothly from where it was.
        let jump = (after[0] - before[63]).abs();
        let typical = before
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(
            jump < typical * 4.0 + 0.05,
            "jump {jump}, typical {typical}"
        );
        // The stolen voice is gone once faded.
        assert!(v.voices.iter().all(|x| !(x.active && x.stolen)));
    }

    #[test]
    fn voices_carry_over_with_the_same_library() {
        let lib = library();
        let mut a = SampleVoices::new(48_000.0, lib.clone());
        a.note_on(60, 100, &NoteShape::default());
        let _ = render(&mut a, 64);
        let mut b = SampleVoices::new(48_000.0, lib);
        assert!(b.adopt(&mut a));
        assert!(peak(&render(&mut b, 64).0) > 0.01);
        let mut c = SampleVoices::new(48_000.0, Arc::new(SampleLibrary::default()));
        assert!(!c.adopt(&mut b));
    }

    #[test]
    fn stereo_width_narrows_and_swaps() {
        let left_only = SampleData::from_audio(&AudioData {
            sample_rate: 48_000,
            channels: vec![vec![0.5; 4_800], vec![0.0; 4_800]],
        });
        let lib = Arc::new(SampleLibrary::new(
            "w",
            vec![left_only],
            vec![Zone::new(0, 60)],
        ));
        let play = |width: f32| {
            let mut v = SampleVoices::new(48_000.0, lib.clone());
            v.note_on(
                60,
                127,
                &NoteShape {
                    width,
                    ..NoteShape::default()
                },
            );
            let (l, r) = render(&mut v, 256);
            (l[200], r[200])
        };
        let (l, r) = play(1.0);
        assert!((l - 0.5).abs() < 1e-3 && r.abs() < 1e-3);
        let (l, r) = play(0.0);
        assert!((l - 0.25).abs() < 1e-3 && (r - 0.25).abs() < 1e-3);
        let (l, r) = play(-1.0);
        assert!(l.abs() < 1e-3 && (r - 0.5).abs() < 1e-3);
    }

    /// Attack voices of `key` held by the key or the pedal, not counting
    /// fading or released ones.
    fn sounding(v: &SampleVoices, key: u8) -> usize {
        v.voices
            .iter()
            .filter(|x| x.active && !x.stolen && !x.released && x.key == key)
            .filter(|x| v.library.zones[x.zone as usize].trigger == Trigger::Attack)
            .count()
    }

    #[test]
    fn the_pedal_holds_keys_until_it_comes_up() {
        let lib = library();
        let quick = NoteShape {
            release: 0.001,
            ..NoteShape::default()
        };
        let mut v = SampleVoices::new(48_000.0, lib);
        v.set_pedal(true, &NoteShape::default(), |_| quick);
        v.note_on(60, 100, &NoteShape::default());
        v.note_on(64, 100, &NoteShape::default());
        let _ = render(&mut v, 480);
        v.note_off(60, &quick);
        v.note_off(64, &quick);
        // The keys are up but still sound, and their release waits.
        let held = peak(&render(&mut v, 4_800).0);
        assert!(held > 0.1, "{held}");
        assert_eq!((sounding(&v, 60), sounding(&v, 64)), (1, 1));
        assert_eq!(v.active_voices(), 2, "no release zone yet");
        // Pedal up: they stop and the release zones play.
        v.set_pedal(false, &NoteShape::default(), |_| quick);
        assert_eq!((sounding(&v, 60), sounding(&v, 64)), (0, 0));
        let _ = render(&mut v, 480);
        assert_eq!(v.active_voices(), 2, "the two release zones");
        // A key still held when the pedal comes up goes on.
        v.set_pedal(true, &NoteShape::default(), |_| quick);
        v.note_on(62, 100, &NoteShape::default());
        v.set_pedal(false, &NoteShape::default(), |_| quick);
        let _ = render(&mut v, 480);
        assert_eq!(sounding(&v, 62), 1);
        v.note_off(62, &quick);
        let _ = render(&mut v, 480);
        assert_eq!(sounding(&v, 62), 0);
        // Stopping lifts the pedal.
        v.set_pedal(true, &NoteShape::default(), |_| quick);
        v.release_all();
        assert!(!v.pedal());
    }

    #[test]
    fn a_key_struck_again_fades_its_old_note() {
        let lib = library();
        let mut v = SampleVoices::new(48_000.0, lib);
        v.set_pedal(true, &NoteShape::default(), |_| NoteShape::default());
        for _ in 0..3 {
            v.note_on(60, 100, &NoteShape::default());
            let _ = render(&mut v, 240);
            v.note_off(60, &NoteShape::default());
        }
        // The old notes fade over about 50 ms instead of piling up.
        assert_eq!(sounding(&v, 60), 1);
        let fading = v.voices.iter().filter(|x| x.active && x.stolen).count();
        assert_eq!(fading, 2);
        let _ = render(&mut v, 2_400 + 64);
        assert!(v.voices.iter().all(|x| !(x.active && x.stolen)));
        // No click: the output moves on smoothly as the old note fades.
        let mut w = SampleVoices::new(48_000.0, library());
        w.note_on(60, 100, &NoteShape::default());
        let (before, _) = render(&mut w, 2_400);
        w.note_on(60, 100, &NoteShape::default());
        let (after, _) = render(&mut w, 256);
        let jump = (after[0] - before[2_399]).abs();
        assert!(jump < 0.1, "jump {jump}");
    }

    #[test]
    fn keys_without_dampers_ring_on() {
        let quick = NoteShape {
            release: 0.001,
            ..NoteShape::default()
        };
        let tail = |key: u8| {
            let mut v = SampleVoices::new(48_000.0, library());
            v.set_undamped_from(70);
            v.note_on(key, 100, &NoteShape::default());
            let _ = render(&mut v, 480);
            v.note_off(key, &quick);
            let _ = render(&mut v, 4_800);
            sounding_any(&v, key)
        };
        assert!(!tail(69), "a damped key stops");
        assert!(tail(70), "an undamped key rings");
    }

    /// Whether any attack voice of `key` still sounds.
    fn sounding_any(v: &SampleVoices, key: u8) -> bool {
        v.voices.iter().any(|x| {
            x.active && x.key == key && v.library.zones[x.zone as usize].trigger == Trigger::Attack
        })
    }

    #[test]
    fn pedal_recordings_play_and_take_turns_by_chance() {
        let down_a = Zone {
            trigger: Trigger::PedalDown,
            hirand: 0.5,
            ..Zone::new(0, 60)
        };
        let down_b = Zone {
            sample: 1,
            lorand: 0.5,
            hirand: 1.0,
            ..down_a.clone()
        };
        let up = Zone {
            trigger: Trigger::PedalUp,
            sample: 2,
            ..Zone::new(2, 60)
        };
        let lib = Arc::new(SampleLibrary::new(
            "pedal",
            vec![
                sine(4_800, 100.0, 1),
                sine(4_800, 200.0, 1),
                sine(4_800, 300.0, 1),
            ],
            vec![down_a, down_b, up],
        ));
        assert!(lib.has_pedal_noise());
        assert!(lib.zones_at(Trigger::Attack, 60, 100).is_empty());
        let mut v = SampleVoices::new(48_000.0, lib);
        let mut downs = [0; 2];
        for _ in 0..40 {
            v.set_pedal(true, &NoteShape::default(), |_| NoteShape::default());
            let zone = v.newest_zone().unwrap();
            downs[zone as usize] += 1;
            v.set_pedal(false, &NoteShape::default(), |_| NoteShape::default());
            assert_eq!(v.newest_zone(), Some(2));
            // Moving it the way it already is does nothing.
            let before = v.clock;
            v.set_pedal(false, &NoteShape::default(), |_| NoteShape::default());
            assert_eq!(v.clock, before);
        }
        assert!(downs[0] > 5 && downs[1] > 5, "{downs:?}");
        // The noise plays at the gain given and ignores the keys.
        let _ = render(&mut v, 4_800);
        let mut v2 = SampleVoices::new(48_000.0, v.library().clone());
        let quiet = NoteShape {
            gain: 0.1,
            ..NoteShape::default()
        };
        v2.set_pedal(true, &quiet, |_| NoteShape::default());
        v2.note_off(60, &NoteShape::default());
        let level = peak(&render(&mut v2, 2_400).0);
        // The sine peaks at 0.5.
        assert!((level - 0.05).abs() < 0.005, "{level}");
    }

    #[test]
    fn the_pedal_carries_over_with_the_voices() {
        let lib = library();
        let mut a = SampleVoices::new(48_000.0, lib.clone());
        a.set_pedal(true, &NoteShape::default(), |_| NoteShape::default());
        a.note_on(60, 100, &NoteShape::default());
        a.note_off(60, &NoteShape::default());
        let mut b = SampleVoices::new(48_000.0, lib);
        assert!(b.adopt(&mut a));
        assert!(b.pedal());
        assert_eq!(sounding(&b, 60), 1);
        b.set_pedal(false, &NoteShape::default(), |_| NoteShape::default());
        assert_eq!(sounding(&b, 60), 0);
    }
}
