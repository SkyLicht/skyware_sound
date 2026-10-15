// SPDX-License-Identifier: GPL-3.0-or-later
//! Step sequencer clips: lanes of steps on a grid, as on a drum machine or
//! an acid bass line. Each step can have its own key, velocity, accent,
//! slide, probability, condition, ratchet, micro-timing and parameter locks,
//! and each lane its own length, so lanes of different lengths drift against
//! each other (a 12-step bass line over a 16-step beat).
//!
//! A lane's steps also have a short text form, one token per step, used by
//! the MCP tools and the summary:
//!
//! ```text
//! x . x . X . x/ - c2 d#2! g1*3 x?50 x@1:2 x=80 x>10
//! ```
//!
//! * `.` or `~` is a rest, and `-` a tie that holds the previous note
//!   through the step;
//! * `x` plays the lane's key and `X` the same with an accent; a note name
//!   such as `c2` or `d#2`, or a MIDI number such as `36`, plays that key;
//! * after a note, `!` accents it, `/` slides into the next note, `*3`
//!   ratchets it (2 to 8 hits in the step), `?50` plays it half the time,
//!   `@1:2` plays it on the first of every two passes through the lane
//!   (also `@first`, `@!first`, `@fill` for the lane's last pass in the
//!   clip, and `@!fill`), `=80` sets its velocity, and `>10` or `<10` plays
//!   it 10% of a step late or early.
//!
//! A `|` between steps is ignored, so bars can be marked.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sky_core::music::{note_name, parse_note_name};
use sky_core::{PPQ, Ticks};

use crate::error::ModelError;
use crate::project::Swing;

/// The most lanes in one clip.
pub const MAX_LANES: usize = 16;
/// The most steps in one lane.
pub const MAX_STEPS: usize = 64;
/// The most hits a ratchet plays in one step.
pub const MAX_RATCHET: u8 = 8;
/// Step sizes as steps per whole note: 16 is sixteenths, 12 and 24 are
/// triplets.
pub const RATES: [u8; 6] = [4, 8, 12, 16, 24, 32];
/// How far a step can move early or late, as a percentage of a step.
pub const MAX_NUDGE: i8 = 50;
/// The largest `b` of an `a:b` condition.
pub const MAX_CONDITION: u8 = 8;
/// The velocity an accented step plays at. Instruments with an accent of
/// their own, such as the Acid bass, take a velocity above 100 as one.
pub const ACCENT_VELOCITY: u8 = 127;
/// The most parameters one clip can lock.
pub const MAX_LOCKED_PARAMS: usize = 8;

const DEFAULT_VELOCITY: u8 = 100;
/// How long a lane's notes hold by default, as a percentage of a step.
pub const DEFAULT_GATE: u8 = 50;

/// What a step does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepKind {
    #[default]
    Rest,
    Note,
    /// Holds the previous note through this step.
    Tie,
}

/// When a step plays, counted in passes through its lane within the clip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Condition {
    #[default]
    Always,
    /// Plays on pass `a` of every `b`: `1:2` plays the first time and every
    /// other time after.
    Ratio {
        a: u8,
        b: u8,
    },
    First,
    NotFirst,
    /// The lane's last pass in the clip, where a fill goes.
    Fill,
    NotFill,
}

impl Condition {
    /// Whether a step plays on pass `pass` (from 0) of `passes`.
    pub fn plays(self, pass: usize, passes: usize) -> bool {
        let last = pass + 1 >= passes;
        match self {
            Condition::Always => true,
            Condition::Ratio { a, b } => pass % usize::from(b.max(1)) + 1 == usize::from(a),
            Condition::First => pass == 0,
            Condition::NotFirst => pass != 0,
            Condition::Fill => last,
            Condition::NotFill => !last,
        }
    }

    pub fn is_always(&self) -> bool {
        *self == Condition::Always
    }

    /// The conditions offered in menus, with `a:b` up to 1:4.
    pub fn choices() -> Vec<Condition> {
        let mut list = vec![
            Condition::Always,
            Condition::First,
            Condition::NotFirst,
            Condition::Fill,
            Condition::NotFill,
        ];
        for b in 2..=4 {
            for a in 1..=b {
                list.push(Condition::Ratio { a, b });
            }
        }
        list
    }
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Condition::Always => f.write_str("always"),
            Condition::Ratio { a, b } => write!(f, "{a}:{b}"),
            Condition::First => f.write_str("first"),
            Condition::NotFirst => f.write_str("!first"),
            Condition::Fill => f.write_str("fill"),
            Condition::NotFill => f.write_str("!fill"),
        }
    }
}

impl std::str::FromStr for Condition {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim().to_ascii_lowercase();
        Ok(match s.as_str() {
            "" | "always" => Condition::Always,
            "first" => Condition::First,
            "!first" | "notfirst" | "not first" => Condition::NotFirst,
            "fill" | "last" => Condition::Fill,
            "!fill" | "notfill" | "not fill" | "!last" => Condition::NotFill,
            _ => {
                let parsed = s
                    .split_once(':')
                    .and_then(|(a, b)| Some((a.parse::<u8>().ok()?, b.parse::<u8>().ok()?)));
                match parsed {
                    Some((a, b)) if (1..=b).contains(&a) && b <= MAX_CONDITION => {
                        Condition::Ratio { a, b }
                    }
                    _ => {
                        return Err(format!(
                            "a condition is a:b (such as 1:2, with a <= b <= {MAX_CONDITION}), \
                             first, !first, fill or !fill, got {s:?}"
                        ));
                    }
                }
            }
        })
    }
}

impl Serialize for Condition {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Condition {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn default_velocity() -> u8 {
    DEFAULT_VELOCITY
}

fn is_default_velocity(v: &u8) -> bool {
    *v == DEFAULT_VELOCITY
}

fn default_probability() -> u8 {
    100
}

fn is_certain(p: &u8) -> bool {
    *p == 100
}

fn default_ratchet() -> u8 {
    1
}

fn is_single(r: &u8) -> bool {
    *r == 1
}

fn default_gate() -> u8 {
    DEFAULT_GATE
}

fn is_default_gate(g: &u8) -> bool {
    *g == DEFAULT_GATE
}

fn is_rest(k: &StepKind) -> bool {
    *k == StepKind::Rest
}

fn is_zero(n: &i8) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One step of a lane. Only what differs from a plain step is saved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    #[serde(default, skip_serializing_if = "is_rest")]
    pub kind: StepKind,
    /// The key this step plays; the lane's when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u8>,
    #[serde(
        default = "default_velocity",
        skip_serializing_if = "is_default_velocity"
    )]
    pub velocity: u8,
    /// Plays louder; an instrument with its own accent also plays brighter.
    #[serde(default, skip_serializing_if = "is_false")]
    pub accent: bool,
    /// Holds into the next note, so a mono instrument such as the Acid bass
    /// glides to it without starting again.
    #[serde(default, skip_serializing_if = "is_false")]
    pub slide: bool,
    /// The chance the step plays, in percent.
    #[serde(default = "default_probability", skip_serializing_if = "is_certain")]
    pub probability: u8,
    #[serde(default, skip_serializing_if = "Condition::is_always")]
    pub condition: Condition,
    /// Hits in the step, 1 to 8.
    #[serde(default = "default_ratchet", skip_serializing_if = "is_single")]
    pub ratchet: u8,
    /// Micro-timing: how far the step moves late (positive) or early, as a
    /// percentage of a step.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub nudge: i8,
    /// Instrument parameters set for this step's notes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locks: BTreeMap<String, f32>,
}

impl Default for Step {
    fn default() -> Self {
        Self {
            kind: StepKind::Rest,
            note: None,
            velocity: DEFAULT_VELOCITY,
            accent: false,
            slide: false,
            probability: 100,
            condition: Condition::Always,
            ratchet: 1,
            nudge: 0,
            locks: BTreeMap::new(),
        }
    }
}

impl Step {
    pub fn rest() -> Self {
        Self::default()
    }

    /// A step that plays the lane's key.
    pub fn hit() -> Self {
        Self {
            kind: StepKind::Note,
            ..Self::default()
        }
    }

    /// A step that plays `note`.
    pub fn note(note: u8) -> Self {
        Self {
            kind: StepKind::Note,
            note: Some(note),
            ..Self::default()
        }
    }

    pub fn tie() -> Self {
        Self {
            kind: StepKind::Tie,
            ..Self::default()
        }
    }

    pub fn accented(mut self) -> Self {
        self.accent = true;
        self
    }

    pub fn sliding(mut self) -> Self {
        self.slide = true;
        self
    }

    pub fn is_note(&self) -> bool {
        self.kind == StepKind::Note
    }

    /// The key the step plays in a lane on `lane_note`.
    pub fn key(&self, lane_note: u8) -> u8 {
        self.note.unwrap_or(lane_note)
    }

    /// The velocity the step plays at.
    pub fn played_velocity(&self) -> u8 {
        if self.accent {
            ACCENT_VELOCITY
        } else {
            self.velocity
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.note.is_some_and(|n| n > 127) {
            return Err(format!("note {} is above 127", self.note.unwrap_or(0)));
        }
        if !(1..=127).contains(&self.velocity) {
            return Err(format!("velocity must be 1-127, got {}", self.velocity));
        }
        if self.probability > 100 {
            return Err(format!(
                "probability must be 0-100%, got {}",
                self.probability
            ));
        }
        if !(1..=MAX_RATCHET).contains(&self.ratchet) {
            return Err(format!(
                "a ratchet plays 1 to {MAX_RATCHET} hits, got {}",
                self.ratchet
            ));
        }
        if !(-MAX_NUDGE..=MAX_NUDGE).contains(&self.nudge) {
            return Err(format!(
                "micro-timing goes from -{MAX_NUDGE}% to +{MAX_NUDGE}% of a step, got {}",
                self.nudge
            ));
        }
        if let Condition::Ratio { a, b } = self.condition
            && !((1..=b).contains(&a) && b <= MAX_CONDITION)
        {
            return Err(format!(
                "condition {a}:{b} needs 1 <= a <= b <= {MAX_CONDITION}"
            ));
        }
        for (name, value) in &self.locks {
            if !value.is_finite() {
                return Err(format!("lock {name} must be a number, got {value}"));
            }
        }
        Ok(())
    }
}

/// A row of steps playing one key, such as the kick, or a melody whose
/// steps each play their own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepLane {
    /// The key the lane plays: a drum (36 is the kick) or the root of a
    /// melody.
    pub note: u8,
    /// How long notes hold, as a percentage of a step; ties and slides hold
    /// longer.
    #[serde(default = "default_gate", skip_serializing_if = "is_default_gate")]
    pub gate: u8,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mute: bool,
    /// The steps; there are as many as the lane is long.
    pub steps: Vec<Step>,
}

impl StepLane {
    /// A lane of `length` rests on `note`.
    pub fn new(note: u8, length: usize) -> Self {
        Self {
            note,
            gate: DEFAULT_GATE,
            mute: false,
            steps: vec![Step::rest(); length.clamp(1, MAX_STEPS)],
        }
    }

    /// A lane from the text form (see the module docs).
    pub fn parse(note: u8, text: &str) -> Result<Self, ModelError> {
        Ok(Self {
            steps: parse_steps(text)?,
            ..Self::new(note, 1)
        })
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Makes the lane `length` steps long, adding rests at the end or
    /// dropping steps from it.
    pub fn resize(&mut self, length: usize) {
        self.steps.resize(length.clamp(1, MAX_STEPS), Step::rest());
    }

    /// The steps in the text form.
    pub fn text(&self) -> String {
        format_steps(&self.steps, self.note)
    }

    fn validate(&self) -> Result<(), String> {
        if self.note > 127 {
            return Err(format!("lane note {} is above 127", self.note));
        }
        if !(1..=100).contains(&self.gate) {
            return Err(format!("gate must be 1-100% of a step, got {}", self.gate));
        }
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err(format!(
                "a lane has 1 to {MAX_STEPS} steps, got {}",
                self.steps.len()
            ));
        }
        for (i, step) in self.steps.iter().enumerate() {
            step.validate()
                .map_err(|e| format!("step {}: {e}", i + 1))?;
        }
        Ok(())
    }
}

/// A note a step clip plays, in ticks from the clip start.
#[derive(Clone, Debug, PartialEq)]
pub struct StepNote<'a> {
    pub start: Ticks,
    pub end: Ticks,
    pub pitch: u8,
    pub velocity: u8,
    pub lane: usize,
    /// The parameters the step locks.
    pub locks: &'a BTreeMap<String, f32>,
}

/// The content of a step clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepClip {
    /// The step size as steps per whole note: 16 is sixteenths.
    pub rate: u8,
    /// Seeds the steps' probabilities, so a render always plays the same.
    #[serde(default)]
    pub seed: u64,
    #[serde(default, skip_serializing_if = "Swing::is_straight")]
    pub swing: Swing,
    pub lanes: Vec<StepLane>,
}

impl StepClip {
    pub fn new(rate: u8, lanes: Vec<StepLane>) -> Self {
        Self {
            rate,
            seed: 0,
            swing: Swing::new(Swing::STRAIGHT, rate.min(32)),
            lanes,
        }
    }

    /// The length of a step in ticks.
    pub fn step_ticks(&self) -> Ticks {
        step_ticks(self.rate)
    }

    /// Every parameter name a step locks, sorted, once each.
    pub fn locked_params(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .lanes
            .iter()
            .flat_map(|l| &l.steps)
            .flat_map(|s| s.locks.keys())
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        let invalid = |m: String| Err(ModelError::InvalidValue(m));
        if !RATES.contains(&self.rate) {
            return invalid(format!(
                "a step is a 4th, 8th, 8th triplet, 16th, 16th triplet or 32nd \
                 (rate 4, 8, 12, 16, 24 or 32), got {}",
                self.rate
            ));
        }
        if self.lanes.is_empty() || self.lanes.len() > MAX_LANES {
            return invalid(format!(
                "a step clip has 1 to {MAX_LANES} lanes, got {}",
                self.lanes.len()
            ));
        }
        for (i, lane) in self.lanes.iter().enumerate() {
            lane.validate()
                .map_err(|e| ModelError::InvalidValue(format!("lane {}: {e}", i + 1)))?;
        }
        let locked = self.locked_params();
        if locked.len() > MAX_LOCKED_PARAMS {
            return invalid(format!(
                "a step clip can lock at most {MAX_LOCKED_PARAMS} parameters, got {}",
                locked.len()
            ));
        }
        for name in locked {
            let known = sky_dsp::InstrumentType::ALL
                .iter()
                .any(|t| sky_dsp::params::find(t.params(), name).is_some());
            if !known {
                return Err(ModelError::InvalidParam(format!(
                    "a step can lock an instrument parameter, got {name:?}"
                )));
            }
        }
        crate::command::validate_swing(self.swing)
    }

    /// The notes the clip plays over `length` ticks, sorted by start. Each
    /// lane repeats from the clip start for as long as the clip lasts.
    /// Swing is not applied, since it works on the timeline.
    pub fn notes(&self, length: Ticks) -> Vec<StepNote<'_>> {
        let st = self.step_ticks();
        let mut out = Vec::new();
        if st <= 0 || length <= 0 {
            return out;
        }
        let total = usize::try_from((length + st - 1) / st).unwrap_or(0);
        for (li, lane) in self.lanes.iter().enumerate() {
            if lane.mute || lane.steps.is_empty() {
                continue;
            }
            lane_notes(self.seed, li, lane, st, total, length, &mut out);
        }
        out.retain(|n| n.end > n.start);
        out.sort_by_key(|n| (n.start, n.pitch, n.lane));
        out
    }
}

/// The length of a step at `rate` steps per whole note.
pub fn step_ticks(rate: u8) -> Ticks {
    if rate == 0 {
        0
    } else {
        PPQ * 4 / Ticks::from(rate)
    }
}

/// A number from 0 to 99 for one step of one pass, the same every time.
fn roll(seed: u64, lane: usize, step: usize) -> u8 {
    let mut x = seed
        ^ (lane as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (step as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % 100) as u8
}

fn lane_notes<'a>(
    seed: u64,
    li: usize,
    lane: &'a StepLane,
    st: Ticks,
    total: usize,
    length: Ticks,
    out: &mut Vec<StepNote<'a>>,
) {
    let n = lane.steps.len();
    let passes = total.div_ceil(n);
    // Where a slide holds a note into the next one, it overlaps that note
    // by this much, so legato instruments glide instead of starting again.
    let overlap = (st / 8).max(1);
    // The note a tie extends, and a note sliding into the next one.
    let mut open: Option<usize> = None;
    let mut slide: Option<usize> = None;
    for k in 0..total {
        let Some(step) = lane.steps.get(k % n) else {
            continue;
        };
        let t0 = k as Ticks * st;
        let t1 = t0 + st;
        let plays = step.is_note()
            && step.condition.plays(k / n, passes)
            && roll(seed, li, k) < step.probability;
        match step.kind {
            StepKind::Tie => {
                if let Some(note) = open.and_then(|i| out.get_mut(i)) {
                    note.end = note.end.max(t1);
                }
                continue;
            }
            _ if !plays => {
                // A slide into silence holds its note to the end of its step.
                if let Some(note) = slide.take().and_then(|i| out.get_mut(i)) {
                    note.end = note.end.max(t0);
                }
                open = None;
                continue;
            }
            _ => {}
        }
        let pitch = step.key(lane.note);
        let velocity = step.played_velocity();
        let hits = Ticks::from(step.ratchet.max(1));
        let shift = st * Ticks::from(step.nudge) / 100;
        let at = |j: Ticks| t0 + shift + (j * st + hits / 2) / hits;
        if let Some(i) = slide.take() {
            let first = at(0).max(0);
            if let Some(note) = out.get_mut(i) {
                if note.pitch == pitch && hits == 1 {
                    // Sliding into the same key is one long note.
                    let len = (st * Ticks::from(lane.gate) / 100).max(1);
                    note.end = note.end.max(first + len);
                    open = Some(i);
                    slide = step.slide.then_some(i);
                    continue;
                }
                note.end = note.end.max(first + overlap);
            }
        }
        for j in 0..hits {
            let start = at(j).max(0);
            let span = at(j + 1) - at(j);
            let end = start + (span * Ticks::from(lane.gate) / 100).max(1);
            if start >= length {
                break;
            }
            out.push(StepNote {
                start,
                end,
                pitch,
                velocity,
                lane: li,
                locks: &step.locks,
            });
        }
        open = out.len().checked_sub(1);
        slide = if step.slide { open } else { None };
    }
    if let Some(note) = slide.and_then(|i| out.get_mut(i)) {
        note.end = length;
    }
    for note in out.iter_mut() {
        note.end = note.end.min(length);
    }
}

fn parse_key(text: &str) -> Result<u8, String> {
    if let Ok(n) = text.parse::<u8>() {
        return if n <= 127 {
            Ok(n)
        } else {
            Err(format!("MIDI note {n} is above 127"))
        };
    }
    parse_note_name(text).map_err(|e| e.to_string())
}

fn parse_number<T: std::str::FromStr>(text: &str, what: &str) -> Result<T, String> {
    text.parse::<T>()
        .map_err(|_| format!("{what} needs a number, got {text:?}"))
}

/// Parses one step token.
pub fn parse_step(token: &str) -> Result<Step, String> {
    match token {
        "." | "~" => return Ok(Step::rest()),
        "-" | "_" => return Ok(Step::tie()),
        _ => {}
    }
    let split = token
        .char_indices()
        .skip(1)
        .find(|(_, c)| "!/*?@=<>".contains(*c))
        .map_or(token.len(), |(i, _)| i);
    let (head, mut rest) = token.split_at(split);
    let mut step = match head {
        "x" => Step::hit(),
        "X" => Step::hit().accented(),
        _ => Step::note(parse_key(head).map_err(|e| {
            format!("{e}; a step is ., -, x, X or a note such as c2 (in {token:?})")
        })?),
    };
    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        // A condition may start with its own `!`, as in `@!fill`.
        let skip = usize::from(c == '@' && rest.starts_with('!'));
        let end = rest[skip..]
            .find(|c: char| "!/*?@=<>".contains(c))
            .map_or(rest.len(), |i| i + skip);
        let (arg, tail) = rest.split_at(end);
        match c {
            '!' | '/' if !arg.is_empty() => {
                return Err(format!("unexpected {arg:?} after {c} in {token:?}"));
            }
            '!' => step.accent = true,
            '/' => step.slide = true,
            '*' => step.ratchet = parse_number(arg, "a ratchet (*)")?,
            '?' => step.probability = parse_number(arg, "a probability (?)")?,
            '@' => step.condition = arg.parse()?,
            '=' => step.velocity = parse_number(arg, "a velocity (=)")?,
            '>' => step.nudge = parse_number(arg, "micro-timing (>)")?,
            '<' => step.nudge = -parse_number::<i8>(arg, "micro-timing (<)")?,
            _ => unreachable!("split on these characters"),
        }
        rest = tail;
    }
    step.validate().map_err(|e| format!("{e} (in {token:?})"))?;
    Ok(step)
}

/// Parses a lane's steps from the text form.
pub fn parse_steps(text: &str) -> Result<Vec<Step>, ModelError> {
    let steps = text
        .split_whitespace()
        .filter(|t| *t != "|")
        .enumerate()
        .map(|(i, t)| parse_step(t).map_err(|e| format!("step {}: {e}", i + 1)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(ModelError::Steps)?;
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return Err(ModelError::Steps(format!(
            "a lane has 1 to {MAX_STEPS} steps, got {}",
            steps.len()
        )));
    }
    Ok(steps)
}

/// One step in the text form. Locks are left out; see [`format_locks`].
pub fn format_step(step: &Step, lane_note: u8) -> String {
    let mut s = match step.kind {
        StepKind::Rest => return ".".into(),
        StepKind::Tie => return "-".into(),
        StepKind::Note => match step.note.filter(|n| *n != lane_note) {
            None if step.accent => "X".to_string(),
            None => "x".to_string(),
            Some(n) => note_name(n).to_ascii_lowercase(),
        },
    };
    if step.accent && s != "X" {
        s.push('!');
    }
    if step.slide {
        s.push('/');
    }
    if step.ratchet > 1 {
        s.push_str(&format!("*{}", step.ratchet));
    }
    if step.probability < 100 {
        s.push_str(&format!("?{}", step.probability));
    }
    if !step.condition.is_always() {
        s.push_str(&format!("@{}", step.condition));
    }
    if step.velocity != DEFAULT_VELOCITY {
        s.push_str(&format!("={}", step.velocity));
    }
    match step.nudge {
        0 => {}
        n if n > 0 => s.push_str(&format!(">{n}")),
        n => s.push_str(&format!("<{}", n.unsigned_abs())),
    }
    s
}

/// A lane's steps in the text form, with a `|` every 16 steps.
pub fn format_steps(steps: &[Step], lane_note: u8) -> String {
    let mut out = String::new();
    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            out.push_str(if i % 16 == 0 { " | " } else { " " });
        }
        out.push_str(&format_step(step, lane_note));
    }
    out
}

/// The locks of a lane's steps by parameter, one value or `.` per step.
pub fn format_locks(steps: &[Step]) -> BTreeMap<String, String> {
    let mut names: Vec<&String> = steps.iter().flat_map(|s| s.locks.keys()).collect();
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .map(|name| {
            let values: Vec<String> = steps
                .iter()
                .map(|s| {
                    s.locks
                        .get(name)
                        .map_or_else(|| ".".to_string(), |v| format_value(*v))
                })
                .collect();
            (name.clone(), values.join(" "))
        })
        .collect()
}

fn format_value(v: f32) -> String {
    let rounded = (v as f64 * 1000.0).round() / 1000.0;
    format!("{rounded}")
}

/// Sets the locks of one parameter from one value or `.` per step; `.`
/// removes the step's lock.
pub fn parse_locks(steps: &mut [Step], param: &str, text: &str) -> Result<(), ModelError> {
    let tokens: Vec<&str> = text.split_whitespace().filter(|t| *t != "|").collect();
    if tokens.len() != steps.len() {
        return Err(ModelError::Steps(format!(
            "{param}: give one value or . for each of the lane's {} steps, got {}",
            steps.len(),
            tokens.len()
        )));
    }
    for (i, (step, token)) in steps.iter_mut().zip(tokens).enumerate() {
        if matches!(token, "." | "~") {
            step.locks.remove(param);
            continue;
        }
        let value: f32 = token
            .parse()
            .ok()
            .filter(|v: &f32| v.is_finite())
            .ok_or_else(|| {
                ModelError::Steps(format!(
                    "{param}: step {} needs a number or ., got {token:?}",
                    i + 1
                ))
            })?;
        step.locks.insert(param.to_string(), value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(lanes: Vec<StepLane>) -> StepClip {
        StepClip::new(16, lanes)
    }

    fn starts(notes: &[StepNote]) -> Vec<Ticks> {
        notes.iter().map(|n| n.start).collect()
    }

    const S: Ticks = PPQ / 4;

    #[test]
    fn the_text_form_round_trips() {
        let text = "x . X - c2 d#2! g1*3 x?50 x@1:2 x=80 x>10 x<5 x/ x@!fill 36! .";
        let steps = parse_steps(text).unwrap();
        assert_eq!(steps.len(), 16);
        assert_eq!(steps[2], Step::hit().accented());
        assert_eq!(steps[3], Step::tie());
        assert_eq!(steps[4], Step::note(36));
        assert_eq!(steps[6].ratchet, 3);
        assert_eq!(steps[7].probability, 50);
        assert_eq!(steps[8].condition, Condition::Ratio { a: 1, b: 2 });
        assert_eq!(steps[9].velocity, 80);
        assert_eq!((steps[10].nudge, steps[11].nudge), (10, -5));
        assert!(steps[12].slide);
        assert_eq!(steps[13].condition, Condition::NotFill);
        // On a lane on C2, the C2 steps print as x.
        let formatted = format_steps(&steps, 36);
        assert_eq!(
            formatted,
            "x . X - x d#2! g1*3 x?50 x@1:2 x=80 x>10 x<5 x/ x@!fill X ."
        );
        assert_eq!(parse_steps(&formatted).unwrap()[4], Step::hit());
        // On another lane they keep their key.
        let other = format_steps(&steps, 40);
        let back = parse_steps(&other).unwrap();
        assert_eq!(back[4].key(40), 36);
        assert_eq!(back, steps);
    }

    #[test]
    fn bad_steps_say_where() {
        let err = |t: &str| parse_steps(t).unwrap_err().to_string();
        assert!(err("x . h2").contains("step 3"), "{}", err("x . h2"));
        assert!(err("x*9").contains("1 to 8"));
        assert!(err("x?101").contains("0-100"));
        assert!(err("x@3:2").contains("a:b"));
        assert!(err("x>60").contains("50%"));
        assert!(err("x!3").contains("unexpected"));
        assert!(err("").contains("1 to 64"));
        assert!(parse_steps(&vec!["x"; 65].join(" ")).is_err());
    }

    #[test]
    fn steps_play_on_the_grid_with_the_gate() {
        let c = clip(vec![StepLane::parse(36, "x . x .").unwrap()]);
        let notes = c.notes(PPQ * 4);
        assert_eq!(
            starts(&notes),
            [0, 2 * S, 4 * S, 6 * S, 8 * S, 10 * S, 12 * S, 14 * S]
        );
        assert!(notes.iter().all(|n| n.end - n.start == S / 2));
        assert!(notes.iter().all(|n| n.pitch == 36 && n.velocity == 100));
    }

    #[test]
    fn lanes_of_different_lengths_drift() {
        let c = clip(vec![
            StepLane::parse(36, "x . . .").unwrap(),
            StepLane::parse(48, "x . .").unwrap(),
        ]);
        let notes = c.notes(PPQ * 3);
        let lane = |l: usize| -> Vec<Ticks> {
            notes
                .iter()
                .filter(|n| n.lane == l)
                .map(|n| n.start)
                .collect()
        };
        assert_eq!(lane(0), [0, 4 * S, 8 * S]);
        assert_eq!(lane(1), [0, 3 * S, 6 * S, 9 * S]);
    }

    #[test]
    fn ties_hold_and_slides_overlap_the_next_note() {
        let c = clip(vec![StepLane::parse(36, "c2 - - . c2/ g2 . .").unwrap()]);
        let notes = c.notes(8 * S);
        assert_eq!(notes.len(), 3);
        // The tie holds the first note through two more steps.
        assert_eq!((notes[0].start, notes[0].end), (0, 3 * S));
        // The slide runs past the next note's start.
        assert_eq!(notes[1].start, 4 * S);
        assert!(notes[1].end > notes[2].start, "{notes:?}");
        assert_eq!(notes[2].pitch, 43);
        // Sliding into the same key is one note; into a rest, a full step.
        let c = clip(vec![StepLane::parse(36, "c2/ c2 . x/ .").unwrap()]);
        let notes = c.notes(5 * S);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!((notes[0].start, notes[0].end), (0, S + S / 2));
        assert_eq!((notes[1].start, notes[1].end), (3 * S, 4 * S));
    }

    #[test]
    fn accents_ratchets_and_nudges() {
        let c = clip(vec![StepLane::parse(36, "X x*4 x>20 x<20").unwrap()]);
        let notes = c.notes(4 * S);
        assert_eq!(notes[0].velocity, ACCENT_VELOCITY);
        let ratchet: Vec<Ticks> = notes[1..5].iter().map(|n| n.start).collect();
        assert_eq!(ratchet, [S, S + S / 4, S + S / 2, S + 3 * S / 4]);
        assert_eq!(notes[5].start, 2 * S + S / 5);
        assert_eq!(notes[6].start, 3 * S - S / 5);
        // Early steps never start before the clip.
        let c = clip(vec![StepLane::parse(36, "x<50").unwrap()]);
        assert_eq!(c.notes(S)[0].start, 0);
    }

    #[test]
    fn conditions_count_passes_through_the_lane() {
        let c = clip(vec![
            StepLane::parse(36, "x@1:2 x@first x@fill x@2:3").unwrap(),
        ]);
        // Four steps per pass, six passes.
        let notes = c.notes(24 * S);
        let passes_of = |step: usize| -> Vec<usize> {
            notes
                .iter()
                .filter(|n| (n.start / S) as usize % 4 == step)
                .map(|n| (n.start / S) as usize / 4)
                .collect()
        };
        assert_eq!(passes_of(0), [0, 2, 4]);
        assert_eq!(passes_of(1), [0]);
        assert_eq!(passes_of(2), [5]);
        assert_eq!(passes_of(3), [1, 4]);
    }

    #[test]
    fn probability_is_seeded() {
        let lane = StepLane::parse(42, &vec!["x?50"; 64].join(" ")).unwrap();
        let mut c = clip(vec![lane]);
        let a = starts(&c.notes(64 * S));
        assert_eq!(a, starts(&c.notes(64 * S)));
        assert!((16..48).contains(&a.len()), "{}", a.len());
        c.seed = 7;
        assert_ne!(a, starts(&c.notes(64 * S)));
    }

    #[test]
    fn locks_and_muted_lanes() {
        let mut lane = StepLane::parse(36, "x x x x").unwrap();
        parse_locks(&mut lane.steps, "cutoff", ". 800 . 1600").unwrap();
        assert_eq!(format_locks(&lane.steps)["cutoff"], ". 800 . 1600");
        assert!(parse_locks(&mut lane.steps, "cutoff", "1 2").is_err());
        let mut muted = StepLane::parse(38, "x x x x").unwrap();
        muted.mute = true;
        let c = clip(vec![lane, muted]);
        let notes = c.notes(4 * S);
        assert_eq!(notes.len(), 4);
        assert_eq!(notes[1].locks.get("cutoff"), Some(&800.0));
        assert!(notes[0].locks.is_empty());
        assert_eq!(c.locked_params(), ["cutoff"]);
    }

    #[test]
    fn validation() {
        let ok = clip(vec![StepLane::new(36, 16)]);
        assert!(ok.validate().is_ok());
        let mut bad = ok.clone();
        bad.rate = 10;
        assert!(bad.validate().is_err());
        let mut bad = ok.clone();
        bad.lanes.clear();
        assert!(bad.validate().is_err());
        let mut bad = ok.clone();
        bad.lanes[0].steps[3].locks.insert("nonsense".into(), 1.0);
        assert!(bad.validate().unwrap_err().to_string().contains("nonsense"));
        let mut bad = ok.clone();
        bad.lanes[0].gate = 0;
        assert!(bad.validate().unwrap_err().to_string().contains("lane 1"));
    }

    #[test]
    fn steps_save_compactly() {
        let mut c = clip(vec![StepLane::parse(36, "x . X c2/").unwrap()]);
        c.swing = Swing::new(56, 16);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(
            json,
            r#"{"rate":16,"seed":0,"swing":{"amount":56,"grid":16},"lanes":[{"note":36,"steps":[{"kind":"note"},{},{"kind":"note","accent":true},{"kind":"note","note":36,"slide":true}]}]}"#
        );
        let back: StepClip = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        let cond: Step = serde_json::from_str(r#"{"kind":"note","condition":"1:4"}"#).unwrap();
        assert_eq!(cond.condition, Condition::Ratio { a: 1, b: 4 });
    }
}
