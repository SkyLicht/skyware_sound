// SPDX-License-Identifier: GPL-3.0-or-later
//! The MCP tools. Every tool that changes the project applies one
//! transaction, so it is one undo step and shows in the AI activity panel.
//!
//! Positions are in bars (bar 1 is the start), lengths in bars, levels in
//! dB. Tracks, clips and effects can be named by ID or by name.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use sky_core::manifest::{Category, RandomRange, Selection};
use sky_core::{AssetId, AudioId, ClipId, EffectId, Ticks, TrackId};
use sky_dsp::analysis;
use sky_dsp::params::{self, ParamKind, ParamSpec};
use sky_dsp::{EffectType, InstrumentType};
use sky_io::WavFormat;
use sky_library::{PACKS, PackSource, Samples};
use sky_model::command::VELOCITY;
use sky_model::notes::{format_note_list, parse_note_list};
use sky_model::summary::{clip_json, game_asset_json, game_json, project_summary, track_json};
use sky_model::templates::{acid_project, demo_project, empty_project, techno_project};
use sky_model::{
    ChannelRef, Clip, ClipContent, Command, EffectSlot, GameAsset, GameAssetKind, InstrumentSlot,
    License, MAX_SFX_VARIATIONS, Origin, Picture, Project, StepClip, StepLane, Swing, Symbol,
    TimeRange, Track, TrackKind, Transaction,
};
use sky_session::{BankRequest, GameExportOptions, LoopExportOptions, Session, SoundsRequest};

use crate::policy::{Permission, Policy};

type ToolResult = Result<String, String>;

const INSTRUCTIONS: &str = "\
Skyware Sound is a music and game-audio workstation. These tools read and edit \
the project open in it. Call project_summary first.\n\
\n\
Units: positions are bars where 1 is the start (1.5 is half way through bar 1); \
lengths are in bars; volume in dB; pan from -1 (left) to 1 (right). Name tracks, \
clips and effects by ID or by name; the master channel is \"master\".\n\
\n\
Every change is one undo step the user can see and undo. If a result is wrong, \
call undo rather than patching it by hand.\n\
\n\
Notes use a compact list, entries played one after another: \
\"C4 1/4 v90, E4 1/8, r 1/8, C4+E4+G4 1/2\". A pitch is a name (C4, F#3, Bb2) or \
MIDI number; + makes a chord; r is a rest; the duration is a fraction of a whole \
note (1/4, 1/8., 1/8t); v sets velocity 1-127; @beats places the entry that many \
quarter notes from the clip start.\n\
\n\
Pattern clips use mini-notation, one cycle per bar by default: \
\"bd*4, ~ sd ~ sd, hh*8\". Spaces split a cycle into steps, [ ] groups steps \
into one, < > alternates per cycle, *n repeats, /n slows, ~ is a rest, a comma \
stacks layers, bd(3,8) is a Euclidean rhythm, ? drops events at random, and \
note names or numbers (c3 e3 g3) play pitches. Drum names: bd sd hh oh cp rim \
lt mt ht cr rd cb. Use pattern_query to check a pattern before writing it.\n\
\n\
Pattern text can also be Strudel code, which plays as written: \
s(\"bd*2 sd\").fast(2).every(4, rev), note(\"c3 e3 g3\").s(\"sawtooth\").lpf(800), \
n(\"0 2 4\").scale(\"C:minor\"), s(\"hh*8\").gain(sine.range(.4, 1)); lines \
starting with $: play together. The strings are mini-notation. Supported: s, \
note, n, stack, cat, seq, fast, slow, early, late, rev, palindrome, ply, iter, \
every/firstOf, lastOf, chunk, within, off, superimpose, echo, sometimes and the \
like, degradeBy, struct, mask, euclid, add, range, scale, bank, legato, the \
controls gain, velocity, speed, lpf, resonance, attack, decay, sustain, release, \
and signals (sine, saw, tri, square, rand, perlin, irand) with range and \
segment. On a synth, sawtooth, square, triangle and sine pick its wave. Effects \
such as room, delay or pan are left out with a warning: put effects on the \
track's inserts instead. pattern_query and pattern_set report warnings.\n\
\n\
A pattern clip can also have controls: number patterns in the same notation, \
read at the start of each note. {\"velocity\": \"127 60 90 60\"} accents \
steps (0 skips a note); an instrument parameter's name sets it per note, as in \
{\"cutoff\": \"<400 800 1600>\"} or, on drums, {\"hat_decay\": \"0.5 2\"}. A \
rest (~) keeps the usual value. Controls apply to every note of the clip, so \
put a part that needs its own accents on its own track. swing (50 straight to \
75) delays every second step of swing_grid (8 or 16).\n\
\n\
For techno: a duck effect on a bass or pad, with sidechain set to the kick's \
track, dips it on every kick (trigger \"kick only\" ignores the other drums on \
that track; it works while that track is muted). The delay's sync and the \
duck's length follow the tempo. The drums have per-piece tune, decay and level \
(kick_tune, hat_decay, ...), kick drive, and an industrial style.\n\
\n\
Strudel's sample packs: a track with the \"bank\" instrument plays named sounds \
from a pack, as Strudel's s(\"bd sd:2\").bank(\"RolandTR808\") does: sd:2 is the \
third sd. sound_packs lists the packs and their banks; bank_load downloads a \
bank or some sounds into a track. Or write Strudel's own line above the \
pattern given to pattern_set, as samples('github:tidalcycles/dirt-samples'): it \
links that pack to the track's bank, and the sounds the pattern names, then or \
later, download from it. MIDI notes on a bank track play its drums by \
General MIDI number, or its pitched sound (piano) when it has one. Packs keep \
their license: exports list the credits they need and warn about unknown \
licenses.\n\
\n\
The piano (instrument piano) plays a recorded piano with velocity layers and \
release sounds. It starts with a small built-in upright; instrument_libraries \
lists the others and piano_library switches a track to one, downloading it \
once. Libraries live outside the project, which names the one it needs.\n\
\n\
Step clips (steps_write) work like a drum machine or an acid bass: lanes of \
steps, one token per step. On drums give a lane per piece (note bd, sd, cp, hh, \
oh); for a bass line give one lane whose steps play notes, such as \
\"c2 c2/ c3! - . d#2 g1 c2!\": ! accents, / slides into the next note, - ties. \
Lanes may differ in length (a 12-step line over a 16-step beat drifts against \
it), and steps can have a probability (?50), condition (@1:2, @fill for the \
lane's last pass in the clip), ratchet (*3), micro-timing (>10) and locks of \
instrument parameters (locks: {\"cutoff\": \". 800 . 1600\"}). The Acid bass \
(instrument acid) is the classic squelchy mono bass: velocity above 100 (an \
accented step) accents a note, and a slide glides into the next note without \
starting again; turn up resonance and env_mod for more squelch.\n\
\n\
For a game loop: set_loop to the bars, loop_check_seam to confirm it repeats \
cleanly, then export_loop writes the WAV and the manifest the game reads.\n\
\n\
For everything a game plays from this project, keep game assets in the project \
(project_summary lists them under game): game_asset_set adds loops (with an \
optional intro) and SFX sets (variations, such as one per beat of a bar, with \
random pitch and gain); game_export writes them all to the game's folder \
(game_export_settings) with one manifest, and checks them in the game runtime.";

#[derive(Clone)]
pub struct SkywareMcp {
    session: Session,
    policy: Arc<Policy>,
    tool_router: ToolRouter<Self>,
}

/// The permission each tool needs.
fn required(tool: &str) -> Permission {
    match tool {
        "project_summary"
        | "track_get"
        | "clip_get"
        | "catalog"
        | "history"
        | "pattern_query"
        | "analyze"
        | "loop_check_seam"
        | "instrument_libraries" => Permission::ReadOnly,
        "project_open"
        | "project_save"
        | "audio_import"
        | "export_wav"
        | "export_loop"
        | "system_check"
        | "sound_packs"
        | "bank_load"
        | "piano_library"
        | "game_export_settings"
        | "game_export" => Permission::Files,
        _ => Permission::Edit,
    }
}

// ----- helpers -----

fn origin(ctx: &RequestContext<RoleServer>, tool: &str) -> Origin {
    let client = ctx
        .client_info()
        .map(|i| i.title.unwrap_or(i.name))
        .unwrap_or_else(|| "MCP client".into());
    Origin::Mcp {
        client,
        tool: tool.into(),
    }
}

/// The instrument names tracks take, for messages.
fn instrument_names() -> String {
    names(InstrumentType::ALL.iter().map(|t| t.name()))
}

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

fn names<'a>(it: impl Iterator<Item = &'a str>) -> String {
    let v: Vec<&str> = it.collect();
    if v.is_empty() {
        "(none)".into()
    } else {
        v.join(", ")
    }
}

fn find_track(project: &Project, r: &str) -> Result<TrackId, String> {
    let r = r.trim();
    if let Ok(id) = r.parse::<TrackId>()
        && project.track(id).is_ok()
    {
        return Ok(id);
    }
    let found: Vec<&Track> = project
        .tracks
        .iter()
        .filter(|t| t.name.eq_ignore_ascii_case(r))
        .collect();
    match found.as_slice() {
        [t] => Ok(t.id),
        [] => Err(format!(
            "no track is named {r:?}; the tracks are: {}",
            names(project.tracks.iter().map(|t| t.name.as_str()))
        )),
        _ => Err(format!("several tracks are named {r:?}; use the track ID")),
    }
}

fn find_channel(project: &Project, r: &str) -> Result<ChannelRef, String> {
    if r.trim().eq_ignore_ascii_case("master") {
        Ok(ChannelRef::Master)
    } else {
        find_track(project, r).map(ChannelRef::Track)
    }
}

fn channel_effects(project: &Project, channel: ChannelRef) -> &[EffectSlot] {
    match channel {
        ChannelRef::Master => &project.master.effects,
        ChannelRef::Track(id) => project
            .track(id)
            .map(|t| t.channel.effects.as_slice())
            .unwrap_or_default(),
    }
}

fn find_effect(project: &Project, channel: ChannelRef, r: &str) -> Result<EffectId, String> {
    let r = r.trim();
    let effects = channel_effects(project, channel);
    if let Ok(id) = r.parse::<EffectId>()
        && effects.iter().any(|e| e.id == id)
    {
        return Ok(id);
    }
    let found: Vec<&EffectSlot> = effects
        .iter()
        .filter(|e| e.kind.name().eq_ignore_ascii_case(r) || e.kind.label().eq_ignore_ascii_case(r))
        .collect();
    match found.as_slice() {
        [e] => Ok(e.id),
        [] => Err(format!(
            "that channel has no effect {r:?}; it has: {}",
            names(effects.iter().map(|e| e.kind.name()))
        )),
        _ => Err(format!(
            "that channel has several {r} effects; use the effect ID"
        )),
    }
}

fn find_clip(project: &Project, r: &str) -> Result<ClipId, String> {
    let r = r.trim();
    if let Ok(id) = r.parse::<ClipId>()
        && project.find_clip(id).is_ok()
    {
        return Ok(id);
    }
    let found: Vec<&Clip> = project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .filter(|c| c.name.eq_ignore_ascii_case(r))
        .collect();
    match found.as_slice() {
        [c] => Ok(c.id),
        [] => Err(format!("no clip has the ID or name {r:?}")),
        _ => Err(format!("several clips are named {r:?}; use the clip ID")),
    }
}

fn find_audio(project: &Project, r: &str) -> Result<AudioId, String> {
    let r = r.trim();
    if let Ok(id) = r.parse::<AudioId>()
        && project.pool_entry(id).is_ok()
    {
        return Ok(id);
    }
    project
        .audio_pool
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(r))
        .map(|e| e.id)
        .ok_or_else(|| {
            format!(
                "no imported audio is named {r:?}; the audio pool has: {}",
                names(project.audio_pool.iter().map(|e| e.name.as_str()))
            )
        })
}

fn bar_tick(project: &Project, bar: f64) -> Result<Ticks, String> {
    if !(1.0..=10_000.0).contains(&bar) {
        return Err(format!("bar positions start at 1, got {bar}"));
    }
    Ok(project.bar_to_ticks(bar))
}

fn bars_len(project: &Project, bars: f64) -> Result<Ticks, String> {
    if !(bars > 0.0 && bars <= 10_000.0) {
        return Err(format!("a length in bars must be above 0, got {bars}"));
    }
    Ok((bars * project.ticks_per_bar() as f64).round() as Ticks)
}

fn bar_range(
    project: &Project,
    start_bar: Option<f64>,
    end_bar: Option<f64>,
) -> Result<Option<(Ticks, Ticks)>, String> {
    match (start_bar, end_bar) {
        (None, None) => Ok(None),
        (Some(s), Some(e)) => {
            let (s, e) = (bar_tick(project, s)?, bar_tick(project, e)?);
            if e <= s {
                return Err("end_bar must be after start_bar".into());
            }
            Ok(Some((s, e)))
        }
        _ => Err("give both start_bar and end_bar, or neither".into()),
    }
}

fn param_value(
    specs: &[ParamSpec],
    owner: &str,
    name: &str,
    value: &Value,
) -> Result<Option<f32>, String> {
    let spec = params::find(specs, name).ok_or_else(|| {
        format!(
            "{owner} has no parameter {name:?}; it has: {}",
            names(specs.iter().map(|s| s.name))
        )
    })?;
    match value {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(Some(n.as_f64().unwrap_or(0.0) as f32)),
        Value::Bool(b) => Ok(Some(if *b { 1.0 } else { 0.0 })),
        Value::String(s) => {
            if let Some(i) = spec.option_index(s) {
                return Ok(Some(i));
            }
            if let Ok(x) = s.trim().parse::<f32>() {
                return Ok(Some(x));
            }
            match spec.kind {
                ParamKind::Choice { options } => Err(format!(
                    "{name} has no option {s:?}; the options are: {}",
                    options.join(", ")
                )),
                _ => Err(format!("{name} needs a number, got {s:?}")),
            }
        }
        _ => Err(format!("{name} needs a number, got {value}")),
    }
}

fn spec_json(spec: &ParamSpec) -> Value {
    let mut v = json!({
        "name": spec.name,
        "label": spec.label,
        "default": spec.default,
    });
    match spec.kind {
        ParamKind::Choice { options } => {
            v["options"] = json!(options);
        }
        ParamKind::Toggle => {
            v["type"] = json!("toggle");
        }
        ParamKind::Continuous { .. } => {
            v["min"] = json!(spec.min);
            v["max"] = json!(spec.max);
            if !spec.unit.is_empty() {
                v["unit"] = json!(spec.unit);
            }
        }
    }
    v
}

/// Checks control names against the track's instrument, which the project
/// itself does not insist on, so a typo is caught here.
fn check_controls<'a>(
    project: &Project,
    track: TrackId,
    names: impl IntoIterator<Item = &'a str>,
) -> Result<(), String> {
    let slot = project
        .track(track)
        .map_err(|e| e.to_string())?
        .instrument
        .as_ref();
    for name in names {
        if name == VELOCITY {
            continue;
        }
        if let Some(slot) = slot
            && params::find(slot.kind.params(), name).is_none()
        {
            return Err(format!(
                "a control is velocity or a parameter of the track's {}, which has no {name:?}; \
                 it has: {}",
                slot.kind.name(),
                names_of(slot.kind.params())
            ));
        }
    }
    Ok(())
}

fn names_of(specs: &[ParamSpec]) -> String {
    names(specs.iter().map(|s| s.name))
}

/// A lane's key from a tool argument: a drum name, a note name or a number.
fn lane_key(text: &str) -> Result<u8, String> {
    let t = text.trim();
    if let Ok(n) = t.parse::<u8>() {
        return if n <= 127 {
            Ok(n)
        } else {
            Err(format!("MIDI note {n} is above 127"))
        };
    }
    sky_pattern::drum_note(t)
        .or_else(|| sky_core::music::parse_note_name(t).ok())
        .ok_or_else(|| {
            format!("{t:?} is not a drum name (bd, sd, hh, ...), note name (c2) or MIDI number")
        })
}

/// A step lane from a tool argument, with its locks checked against the
/// track's instrument.
fn step_lane(project: &Project, track: TrackId, arg: &StepLaneArg) -> Result<StepLane, String> {
    let mut lane = StepLane::parse(lane_key(&arg.note)?, &arg.steps).map_err(|e| e.to_string())?;
    if let Some(gate) = arg.gate {
        lane.gate = gate;
    }
    lane.mute = arg.mute.unwrap_or(false);
    if let Some(locks) = &arg.locks {
        let slot = project
            .track(track)
            .map_err(|e| e.to_string())?
            .instrument
            .as_ref();
        for (param, values) in locks {
            if let Some(slot) = slot
                && params::find(slot.kind.params(), param).is_none()
            {
                return Err(format!(
                    "a lock is a parameter of the track's {}, which has no {param:?}; it has: {}",
                    slot.kind.name(),
                    names_of(slot.kind.params())
                ));
            }
            sky_model::steps::parse_locks(&mut lane.steps, param, values)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(lane)
}

/// A swing from the tool arguments, on top of `current`.
fn swing_of(current: Swing, amount: Option<u8>, grid: Option<u8>) -> Swing {
    Swing::new(
        amount.unwrap_or(current.amount),
        grid.unwrap_or(current.grid),
    )
}

/// The track a duck follows, from a tool argument; an empty string is none.
fn find_sidechain(project: &Project, r: &str) -> Result<Option<TrackId>, String> {
    if r.trim().is_empty() {
        return Ok(None);
    }
    find_track(project, r).map(Some)
}

fn effect_json(project: &Project, s: &EffectSlot) -> Value {
    let mut v = json!({
        "effect": s.id.to_string(),
        "type": s.kind.name(),
        "enabled": s.enabled,
        "params": s.params,
    });
    if let Some(t) = s.sidechain {
        v["sidechain"] = json!(
            project
                .track(t)
                .map_or("(deleted track)".into(), |t| t.name.clone())
        );
    }
    v
}

fn parse_color(text: &str) -> Result<[u8; 3], String> {
    let hex = text.trim().trim_start_matches('#');
    let bad = || format!("colors look like #3a7bd5, got {text:?}");
    if hex.len() != 6 {
        return Err(bad());
    }
    let byte =
        |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or(""), 16).map_err(|_| bad());
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

fn wav_format(bits: Option<u16>) -> Result<WavFormat, String> {
    WavFormat::from_bits(bits.unwrap_or(24))
        .ok_or_else(|| "bits must be 16, 24 or 32 (32 is float)".to_string())
}

fn category(name: Option<&str>) -> Result<Category, String> {
    let name = name.unwrap_or("music");
    Category::parse(name).ok_or_else(|| {
        format!(
            "category {name:?} is not one of {}",
            names(Category::ALL.iter().map(|c| c.name()))
        )
    })
}

fn selection(name: &str) -> Result<Selection, String> {
    let key: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    Ok(match key.as_str() {
        "random" => Selection::Random,
        "randomnorepeat" => Selection::RandomNoRepeat,
        "roundrobin" => Selection::RoundRobin,
        "shuffle" => Selection::Shuffle,
        _ => {
            return Err(format!(
                "selection {name:?} is not one of random, random_no_repeat, round_robin, shuffle"
            ));
        }
    })
}

/// Resolves `.` and `..` in a path, so a game folder next to the project
/// (`../game/assets/audio`) can be checked against the allowed folders.
/// `..` goes up from where a folder that exists really is, as the file
/// system does when one is a link; folders that do not exist yet are
/// resolved as written.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Ok(real) = std::fs::canonicalize(&out) {
                    out = real;
                }
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn game_asset<'a>(project: &'a Project, name: &str) -> Result<&'a GameAsset, String> {
    project.game.find(name.trim()).ok_or_else(|| {
        format!(
            "no game asset is named {name:?}; the game assets are: {}",
            names(project.game.assets.iter().map(|a| a.name.as_str()))
        )
    })
}

/// Shortens long arrays in a JSON value so results stay small.
fn trim_arrays(v: &mut Value, max: usize) {
    match v {
        Value::Array(a) => {
            a.truncate(max);
            for x in a {
                trim_arrays(x, max);
            }
        }
        Value::Object(o) => {
            for x in o.values_mut() {
                trim_arrays(x, max);
            }
        }
        _ => {}
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("the task failed: {e}"))?
}

// ----- parameters -----

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrackArg {
    /// Track ID or name.
    pub track: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipArg {
    /// Clip ID or name.
    pub clip: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PatternQueryArgs {
    /// Mini-notation pattern text, or Strudel code.
    pub pattern: String,
    /// Read the pattern as numbers, the way a control pattern is. Default
    /// false.
    pub numbers: Option<bool>,
    /// How many cycles to list, 1 to 16. Default 1.
    pub cycles: Option<u32>,
    /// Random seed for ? and other random choices. Default 0.
    pub seed: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RangeArgs {
    /// First bar to render. Default: the start of the arrangement.
    pub start_bar: Option<f64>,
    /// Bar where rendering stops. Default: the end of the last clip.
    pub end_bar: Option<f64>,
    /// Seconds rendered after the end so tails ring out. Default 2.
    pub tail_seconds: Option<f64>,
    /// Render the loop region seamlessly instead of a range.
    pub loop_region: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoopRangeArgs {
    /// First bar of the loop. Default: the project's loop region.
    pub start_bar: Option<f64>,
    /// Bar where the loop ends (exclusive).
    pub end_bar: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransportArgs {
    /// play, stop, seek or status.
    pub action: String,
    /// For seek: the bar to move to.
    pub bar: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TempoArgs {
    /// Beats per minute, 10 to 999.
    pub bpm: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TimeSignatureArgs {
    pub numerator: u32,
    /// 1, 2, 4, 8, 16 or 32.
    pub denominator: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoopArgs {
    pub start_bar: f64,
    /// Bar where the loop ends (exclusive): 1 to 5 is four bars.
    pub end_bar: f64,
    /// Turn looping on or off. Default on.
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrackCreateArgs {
    pub name: String,
    /// instrument (default) or audio.
    pub kind: Option<String>,
    /// For instrument tracks: synth (default), piano, acid, drums, sampler
    /// or bank.
    pub instrument: Option<String>,
    /// Instrument parameters, such as {"wave": "square", "cutoff": 800}.
    pub params: Option<BTreeMap<String, Value>>,
    /// For a sampler: the imported audio it plays (ID or name).
    pub sample: Option<String>,
    /// Color such as #3a7bd5.
    pub color: Option<String>,
    /// Position in the track list, 0 is the top. Default: the bottom.
    pub index: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrackSetArgs {
    /// Track ID or name, or "master".
    pub track: String,
    pub name: Option<String>,
    /// -120 to 12 dB.
    pub volume_db: Option<f32>,
    /// -1 (left) to 1 (right).
    pub pan: Option<f32>,
    pub mute: Option<bool>,
    pub solo: Option<bool>,
    /// Color such as #3a7bd5.
    pub color: Option<String>,
    /// New position in the track list, 0 is the top.
    pub index: Option<usize>,
    /// The symbol in the track's picture: synth, acid bass, drums, drum
    /// machine, sampler, sample bank, audio, microphone, piano, strings,
    /// hand drum, record, groovebox, noise, speaker or folder. "auto" goes
    /// back to the one the track shows by itself.
    pub picture: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstrumentSetArgs {
    /// Track ID or name.
    pub track: String,
    /// Replace the instrument: synth, piano, acid, drums, sampler or bank.
    /// Parameters reset.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Parameters to set; null restores a default.
    pub params: Option<BTreeMap<String, Value>>,
    /// For a sampler: the imported audio it plays (ID or name).
    pub sample: Option<String>,
    /// For a sample bank: the sound MIDI notes play, pitched (such as
    /// piano). An empty string plays drums by note number again.
    pub pitched: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SoundPacksArgs {
    /// A pack to look into: a name from the list, a strudel.json URL, a
    /// github:user/repo shortcut, or a folder in the allowed folders.
    /// Without one, lists the known packs.
    pub pack: Option<String>,
    /// List this bank's sounds instead of the pack's banks.
    pub bank: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BankLoadArgs {
    /// Track ID or name. It becomes a sample bank if it is another instrument.
    pub track: String,
    /// A name from sound_packs, a strudel.json URL, a github:user/repo
    /// shortcut, or a folder in the allowed folders.
    pub pack: String,
    /// A bank such as RolandTR808. Without one, the pack's plain sounds.
    pub bank: Option<String>,
    /// Sounds to load, such as ["bd", "sd", "hh"]. Default: all of the bank's.
    pub sounds: Option<Vec<String>>,
    /// Add to the track's sounds instead of replacing them. Default false.
    pub add: Option<bool>,
    /// The sound MIDI notes play, pitched. Default: the sound, when a single
    /// pitched sound is loaded.
    pub pitched: Option<String>,
    /// For a pack that is not in the list: its license, such as CC0-1.0,
    /// CC-BY-4.0 or unknown (the default). Only say what the pack states.
    pub license: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PianoLibraryArgs {
    /// Track ID or name. It becomes a piano if it is another instrument;
    /// a piano keeps its settings.
    pub track: String,
    /// A library from instrument_libraries, by id or name.
    pub library: String,
    /// Download the library when it is not here yet. Default true.
    pub download: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EffectInsertArgs {
    /// Track ID or name, or "master".
    pub channel: String,
    /// Effect type, see catalog.
    #[serde(rename = "type")]
    pub kind: String,
    pub params: Option<BTreeMap<String, Value>>,
    /// Position in the chain, 0 is first. Default: last.
    pub index: Option<usize>,
    /// For a duck: the track whose notes drive it (ID or name). Default:
    /// the first other drum track.
    pub sidechain: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EffectSetArgs {
    /// Track ID or name, or "master".
    pub channel: String,
    /// Effect ID, or its type name if the channel has only one.
    pub effect: String,
    /// Parameters to set; null restores a default.
    pub params: Option<BTreeMap<String, Value>>,
    pub enabled: Option<bool>,
    /// For a duck: the track whose notes drive it (ID or name); an empty
    /// string disconnects it.
    pub sidechain: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EffectRemoveArgs {
    pub channel: String,
    pub effect: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipCreateArgs {
    /// Track ID or name.
    pub track: String,
    pub start_bar: f64,
    pub length_bars: f64,
    pub name: Option<String>,
    /// For a MIDI clip: notes in the compact list form.
    pub notes: Option<String>,
    /// For a pattern clip: mini-notation text, or Strudel code.
    pub pattern: Option<String>,
    /// Pattern cycle length in bars. Default 1.
    pub cycle_bars: Option<f64>,
    /// Pattern random seed. Default 0.
    pub seed: Option<u64>,
    /// For a pattern clip: number patterns read at each note, by parameter:
    /// velocity or one of the instrument's parameters.
    pub controls: Option<BTreeMap<String, String>>,
    /// For a pattern clip: swing from 50 (straight) to 75. Default 50.
    pub swing: Option<u8>,
    /// The step that swings: 4, 8, 16 (default) or 32.
    pub swing_grid: Option<u8>,
    /// For an audio clip on an audio track: the imported audio (ID or name).
    pub audio: Option<String>,
    /// Audio clip gain in dB.
    pub gain_db: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClipMoveArgs {
    /// Clip ID or name.
    pub clip: String,
    pub start_bar: Option<f64>,
    /// Move onto another track (ID or name).
    pub track: Option<String>,
    pub length_bars: Option<f64>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NotesWriteArgs {
    /// MIDI clip ID or name.
    pub clip: String,
    /// Notes in the compact list form.
    pub notes: String,
    /// replace (default) or add.
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StepLaneArg {
    /// The key the lane plays: a drum name (bd, sd, cp, hh, oh, ...), a note
    /// name (c2) or a MIDI number (36). Steps can play other keys.
    pub note: String,
    /// One token per step: . rest, - tie, x the lane's key, X accented, or
    /// a note name; after a note ! accent, / slide, *3 ratchet, ?50
    /// probability, @1:2 or @first, @!first, @fill, @!fill condition, =80
    /// velocity, >10 or <10 micro-timing in % of a step. 1 to 64 steps; the
    /// lane repeats over the clip.
    pub steps: String,
    /// How long notes hold, in % of a step. Default 50.
    pub gate: Option<u8>,
    pub mute: Option<bool>,
    /// Parameter locks by instrument parameter: one value or . per step,
    /// such as {"cutoff": ". 800 . . 1600 . . ."}.
    pub locks: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StepsWriteArgs {
    /// The step clip to change (ID or name). Leave out to add a step clip
    /// to track.
    pub clip: Option<String>,
    /// For a new clip: the track (ID or name).
    pub track: Option<String>,
    /// For a new clip: where it starts. Default 1.
    pub start_bar: Option<f64>,
    /// For a new clip: its length. Default 1.
    pub length_bars: Option<f64>,
    pub name: Option<String>,
    /// The lanes, replacing all of the clip's lanes. Required for a new
    /// clip.
    pub lanes: Option<Vec<StepLaneArg>>,
    /// The step size as steps per whole note: 16 (sixteenths, default), 8,
    /// 4, 32, or 12 and 24 for triplets.
    pub rate: Option<u8>,
    /// Swing from 50 (straight) to 75.
    pub swing: Option<u8>,
    /// The step that swings: 4, 8, 16 or 32.
    pub swing_grid: Option<u8>,
    /// Seeds the probabilities, so a render always plays the same.
    pub seed: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PatternSetArgs {
    /// Pattern clip ID or name.
    pub clip: String,
    /// New mini-notation or Strudel code. Default: unchanged. As in
    /// Strudel, a line
    /// samples('github:user/repo') links that pack to the track's sample
    /// bank (the track becomes one), and sounds the pattern names that the
    /// bank lacks then download from its linked packs; samples({ name:
    /// 'https://…/file.wav' }) loads those files. The line is not kept in
    /// the pattern. Downloading needs the files permission.
    pub pattern: Option<String>,
    /// Cycle length in bars. Default: unchanged.
    pub cycle_bars: Option<f64>,
    pub seed: Option<u64>,
    /// Controls to set, by parameter (velocity or an instrument
    /// parameter); null removes one. Controls not named stay.
    pub controls: Option<BTreeMap<String, Option<String>>>,
    /// Swing from 50 (straight) to 75. Default: unchanged.
    pub swing: Option<u8>,
    /// The step that swings: 4, 8, 16 or 32. Default: unchanged.
    pub swing_grid: Option<u8>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndoArgs {
    /// How many steps, 1 to 50. Default 1.
    pub steps: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectNewArgs {
    pub name: Option<String>,
    /// empty (default), demo, techno (a four-bar techno loop that uses
    /// controls, swing and a duck) or acid (a four-bar acid loop of step
    /// clips with the Acid bass).
    pub template: Option<String>,
    /// Required when the open project has unsaved changes.
    pub discard_unsaved: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectOpenArgs {
    /// A .skyproj folder.
    pub path: String,
    pub discard_unsaved: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectSaveArgs {
    /// Folder to save into (".skyproj" is added). Default: where it was last saved.
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PathArg {
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportWavArgs {
    /// Output .wav path.
    pub path: String,
    pub start_bar: Option<f64>,
    pub end_bar: Option<f64>,
    /// Default 2.
    pub tail_seconds: Option<f64>,
    /// 16, 24 (default) or 32 (float).
    pub bits: Option<u16>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportLoopArgs {
    /// Asset name the game uses. Default: the project name.
    pub name: Option<String>,
    /// Export folder. Default: renders/export inside the project folder.
    pub dir: Option<String>,
    /// Loop bars. Default: the project's loop region.
    pub start_bar: Option<f64>,
    pub end_bar: Option<f64>,
    /// music (default), sfx, ui, voice or ambience.
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    /// 16, 24 (default) or 32 (float).
    pub bits: Option<u16>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SystemCheckArgs {
    /// Seconds to play through the default audio device, 0 to 600; 0 skips
    /// the device check. Default 20.
    pub device_seconds: Option<f64>,
    /// Buffer size to ask the device for, in frames. Default 128.
    pub buffer_frames: Option<u32>,
    /// How to share the device with other programs on Windows: auto
    /// (default; low-latency shared or exclusive mode when shared mode
    /// cannot run the buffer), shared, low-latency or exclusive. Exclusive
    /// mode silences other programs while the check plays.
    pub mode: Option<String>,
    /// Let the device check be heard. Default false: the engine runs as
    /// usual but the device gets silence.
    pub audible: Option<bool>,
    /// Bars in the test loop, 1 to 64; 0 skips the loop check. Default 16.
    pub bars: Option<u32>,
    /// Folder to keep the exported test loop in. Default: a temporary
    /// folder that is deleted afterwards.
    pub dir: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BarSpan {
    pub start_bar: f64,
    pub end_bar: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MinMax {
    pub min: f32,
    pub max: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GameAssetSetArgs {
    /// The name the game plays it by: lowercase letters, digits and
    /// underscores, such as footstep_grass. An asset with this name is
    /// changed; otherwise one is added.
    pub name: String,
    /// loop or sfx. Needed to add an asset.
    pub kind: Option<String>,
    /// A new name for the asset.
    pub rename: Option<String>,
    /// music, sfx, ui, voice or ambience. Default for a new asset: music
    /// for a loop, sfx for an SFX set.
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    /// Loop: the bars of the repeating body (default for a new loop: the
    /// loop region). SFX set: the bars split into `count` variations.
    pub start_bar: Option<f64>,
    pub end_bar: Option<f64>,
    /// Loop: the bar an intro that plays once starts at, before start_bar.
    /// 0 removes the intro.
    pub intro_start_bar: Option<f64>,
    /// SFX set: split start_bar to end_bar into this many equal variations,
    /// such as 4 for one per beat of a 4/4 bar. Default 1. Alone, it
    /// splits the set's current span again.
    pub count: Option<usize>,
    /// SFX set: one bar range per variation, instead of start_bar, end_bar
    /// and count.
    pub variations: Option<Vec<BarSpan>>,
    /// SFX set: how the game picks a variation per trigger: random,
    /// random_no_repeat (default), round_robin or shuffle.
    pub selection: Option<String>,
    /// SFX set: random pitch offset per trigger in semitones, such as
    /// {"min": -1, "max": 1}.
    pub pitch_semitones: Option<MinMax>,
    /// SFX set: random gain offset per trigger in dB.
    pub gain_db: Option<MinMax>,
    /// SFX set: seconds rendered after each variation so it rings out,
    /// 0 to 10; trailing silence is trimmed. Default 1.
    pub tail_seconds: Option<f32>,
    /// SFX set: mix the variations to mono, for sounds the game positions
    /// itself.
    pub mono: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GameAssetArg {
    /// The game asset's name.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GameExportSettingsArgs {
    /// The folder game exports go to, such as the game's assets/audio
    /// folder. A relative path is relative to the project folder. An empty
    /// string resets it to renders/export inside the project folder.
    pub dir: Option<String>,
    /// Bits per sample of the WAV files: 16, 24 or 32 (float).
    pub bits: Option<u16>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GameExportArgs {
    /// Export only these assets. Default: all of them, and (once the
    /// project is saved) assets it no longer has are removed from the folder.
    pub only: Option<Vec<String>>,
    /// Export folder for this export only. Default: the project's game
    /// export folder.
    pub dir: Option<String>,
    /// Load the export into the game runtime afterwards and check that
    /// every loop repeats exactly and every SFX set plays. Default true.
    pub verify: Option<bool>,
}

// ----- the server -----

impl SkywareMcp {
    pub fn new(session: Session, policy: Policy) -> Self {
        let mut router = Self::tool_router();
        let names: Vec<String> = router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for name in names {
            if required(&name) > policy.permission {
                router.remove_route(&name);
            }
        }
        Self {
            session,
            policy: Arc::new(policy),
            tool_router: router,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Names of the tools this server offers under its policy.
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }

    fn apply(
        &self,
        ctx: &RequestContext<RoleServer>,
        tool: &str,
        label: String,
        commands: Vec<Command>,
    ) -> Result<u64, String> {
        self.session
            .apply(Transaction::new(label, origin(ctx, tool), commands))
            .map_err(|e| e.to_string())
    }

    /// Reads a pattern a client wrote: the mini-notation without its
    /// `samples(...)` calls, what to load before it changes, and a note
    /// when sounds it names could not be fetched without the files
    /// permission.
    #[allow(clippy::type_complexity)]
    fn pattern_sounds(
        &self,
        track: &Track,
        text: &str,
    ) -> Result<(String, Option<SoundsRequest>, Option<String>), String> {
        let calls = sky_library::find_samples(text).map_err(|e| format!("pattern error: {e}"))?;
        let source = sky_library::samples::strip_calls(text, &calls);
        sky_pattern::parse(&source).map_err(|e| format!("pattern error: {e}"))?;
        let used = sky_session::pattern_sound_names(&source);
        let bank = track
            .instrument
            .as_ref()
            .filter(|s| s.kind == InstrumentType::Bank)
            .and_then(|s| s.bank.as_ref());
        let missing: Vec<&String> = used
            .iter()
            .filter(|n| bank.is_some_and(|b| !b.packs.is_empty() && b.resolve(n).is_none()))
            .collect();
        if calls.is_empty() && missing.is_empty() {
            return Ok((source, None, None));
        }
        if self.policy.permission < Permission::Files {
            if !calls.is_empty() {
                return Err(
                    "samples(...) downloads sounds, which needs the files permission; write the \
                     pattern without it"
                        .into(),
                );
            }
            let note = format!(
                "the bank has no {}; loading them from its linked packs needs the files permission",
                names(missing.iter().map(|s| s.as_str()))
            );
            return Ok((source, None, Some(note)));
        }
        let mut request = SoundsRequest {
            names: used,
            ..Default::default()
        };
        for call in calls {
            match call.samples {
                Samples::Address(a) => request.link.push(self.pack_source(&a, None)?),
                Samples::Map(map) => {
                    // Files on this computer must be in the allowed folders.
                    for f in map.sounds.values().flatten() {
                        let at = map.locate(f);
                        let lower = at.get(..8).unwrap_or(&at).to_ascii_lowercase();
                        if !lower.starts_with("http://") && !lower.starts_with("https://") {
                            self.policy.resolve(&at)?;
                        }
                    }
                    request.maps.push(map);
                }
            }
        }
        Ok((source, Some(request), None))
    }

    /// The pack a client named: a known pack, an http(s) URL or github:
    /// shortcut, or a folder or file inside the allowed folders.
    fn pack_source(&self, pack: &str, license: Option<&str>) -> Result<PackSource, String> {
        let license = license
            .map(|l| {
                License::parse(l).ok_or_else(|| {
                    format!(
                        "license {l:?} is not one of: {}",
                        names(License::ALL.iter().map(|l| l.id()))
                    )
                })
            })
            .transpose()?;
        let text = pack.trim();
        if let Some(known) = sky_library::packs::find(text) {
            if license.is_some_and(|l| l != known.license) {
                return Err(format!(
                    "{} is {}; its license cannot be changed",
                    known.name,
                    known.license.label()
                ));
            }
            return Ok(PackSource::from(known));
        }
        let lower = text.to_ascii_lowercase();
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("github:")
        {
            return Ok(PackSource::parse(text, license));
        }
        let path = self.policy.resolve(text)?;
        Ok(PackSource::parse(&path.to_string_lossy(), license))
    }

    fn check_unsaved(&self, discard: Option<bool>) -> Result<(), String> {
        if self.session.view().dirty && discard != Some(true) {
            return Err(
                "the open project has unsaved changes; save it first, or pass \
                        discard_unsaved: true"
                    .into(),
            );
        }
        Ok(())
    }
}

#[tool_router]
impl SkywareMcp {
    #[tool(
        description = "The open project: tempo, loop, tracks with their clips, instruments, mixer settings and effects, and the audio pool. Call this first.",
        annotations(read_only_hint = true)
    )]
    async fn project_summary(&self) -> ToolResult {
        let view = self.session.view();
        let mut v = project_summary(&view.project);
        v["path"] = json!(view.path.as_ref().map(|p| p.display().to_string()));
        v["unsavedChanges"] = json!(view.dirty);
        v["undo"] = json!(view.undo_label);
        v["redo"] = json!(view.redo_label);
        Ok(v.to_string())
    }

    #[tool(
        description = "One track in detail, including every clip's notes or pattern.",
        annotations(read_only_hint = true)
    )]
    async fn track_get(&self, Parameters(a): Parameters<TrackArg>) -> ToolResult {
        let project = self.session.project();
        let id = find_track(&project, &a.track)?;
        let track = project.track(id).map_err(|e| e.to_string())?;
        Ok(track_json(&project, track, true).to_string())
    }

    #[tool(
        description = "One clip in detail: its notes (compact list), pattern or audio settings.",
        annotations(read_only_hint = true)
    )]
    async fn clip_get(&self, Parameters(a): Parameters<ClipArg>) -> ToolResult {
        let project = self.session.project();
        let id = find_clip(&project, &a.clip)?;
        let (track, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        let mut v = clip_json(&project, clip, true);
        v["track"] = json!(track.name);
        Ok(v.to_string())
    }

    #[tool(
        description = "The built-in instruments and effects with their parameters, ranges, units and defaults.",
        annotations(read_only_hint = true)
    )]
    async fn catalog(&self) -> ToolResult {
        let instruments: Vec<Value> = InstrumentType::ALL
            .iter()
            .map(|t| {
                json!({
                    "type": t.name(),
                    "description": t.description(),
                    "params": t.params().iter().map(spec_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        let effects: Vec<Value> = EffectType::ALL
            .iter()
            .map(|t| {
                json!({
                    "type": t.name(),
                    "description": t.description(),
                    "params": t.params().iter().map(spec_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({ "instruments": instruments, "effects": effects }).to_string())
    }

    #[tool(
        description = "Recent undo steps (newest last), what redo would do, and the changes AI clients made.",
        annotations(read_only_hint = true)
    )]
    async fn history(&self) -> ToolResult {
        let view = self.session.view();
        let from = view.history.len().saturating_sub(20);
        Ok(json!({
            "undo": view.history.get(from..).unwrap_or_default(),
            "redo": view.redo_label,
            "aiChanges": view.activity.iter().rev().take(20).collect::<Vec<_>>(),
        })
        .to_string())
    }

    #[tool(
        description = "Parses a mini-notation pattern or Strudel code and lists the events it plays, without changing the project. Times are in cycles as fractions; controls such as gain or lpf are listed by their Strudel names.",
        annotations(read_only_hint = true)
    )]
    async fn pattern_query(&self, Parameters(a): Parameters<PatternQueryArgs>) -> ToolResult {
        let numbers = a.numbers.unwrap_or(false);
        // samples(...) lines are not mini-notation; read what is around them.
        let calls =
            sky_library::find_samples(&a.pattern).map_err(|e| format!("pattern error: {e}"))?;
        let text = sky_library::samples::blank_calls(&a.pattern, &calls);
        let pattern = if numbers {
            sky_pattern::parse_numbers(&text)
        } else {
            sky_pattern::parse(&text)
        }
        .map_err(|e| format!("pattern error: {e}"))?;
        let cycles = a.cycles.unwrap_or(1).clamp(1, 16);
        let events = pattern.query(
            sky_pattern::Time::from_integer(0),
            sky_pattern::Time::from_integer(cycles as i64),
            a.seed.unwrap_or(0),
        );
        let list: Vec<Value> = events
            .iter()
            .take(512)
            .map(|e| {
                if let Some(value) = e.value.number() {
                    return json!({
                        "begin": e.begin.to_string(),
                        "end": e.end.to_string(),
                        "value": value,
                    });
                }
                let (sound, index) = match &e.value {
                    sky_pattern::Value::Sound { name, index } => (Some(name.clone()), *index),
                    _ => (None, 0),
                };
                let mut event = json!({
                    "begin": e.begin.to_string(),
                    "end": e.end.to_string(),
                    "sound": sound,
                    "index": index,
                    "midi": e.note(),
                });
                if !e.controls.is_empty() {
                    let controls: serde_json::Map<String, Value> = e
                        .controls
                        .iter()
                        .map(|(c, v)| (c.name().to_string(), json!(v)))
                        .collect();
                    event["controls"] = Value::Object(controls);
                }
                event
            })
            .collect();
        let silent = events
            .iter()
            .filter(|e| e.note().is_none() && e.value.number().is_none())
            .count();
        let mut v = json!({ "cycles": cycles, "eventCount": events.len(), "events": list });
        let warnings: Vec<&str> = pattern
            .warnings()
            .iter()
            .map(|w| w.message.as_str())
            .collect();
        if !warnings.is_empty() {
            v["warnings"] = json!(warnings);
        }
        if silent > 0 {
            v["warning"] = json!(format!(
                "{silent} event(s) name a sound the drum kit does not have; they will be silent"
            ));
        }
        Ok(v.to_string())
    }

    #[tool(
        description = "Renders the project offline and measures it: loudness (LUFS), peak and true peak, clipping, DC offset, silence, onsets, low/mid/high balance and stereo correlation.",
        annotations(read_only_hint = true)
    )]
    async fn analyze(&self, Parameters(a): Parameters<RangeArgs>) -> ToolResult {
        let session = self.session.clone();
        blocking(move || {
            let project = session.project();
            let audio = if a.loop_region == Some(true) {
                session.render_loop(bar_range(&project, a.start_bar, a.end_bar)?)
            } else {
                let (s, e) = match bar_range(&project, a.start_bar, a.end_bar)? {
                    Some(r) => r,
                    None => (0, project.content_end().max(project.ticks_per_bar())),
                };
                session.render_range(s, e, a.tail_seconds.unwrap_or(2.0))
            }
            .map_err(|e| e.to_string())?;
            let mut v =
                serde_json::to_value(analysis::analyze(&audio)).map_err(|e| e.to_string())?;
            trim_arrays(&mut v, 32);
            Ok(v.to_string())
        })
        .await
    }

    #[tool(
        description = "Renders the loop region (or the given bars) as a game loop and checks that its end meets its start without a click or a level jump.",
        annotations(read_only_hint = true)
    )]
    async fn loop_check_seam(&self, Parameters(a): Parameters<LoopRangeArgs>) -> ToolResult {
        let session = self.session.clone();
        blocking(move || {
            let project = session.project();
            let range = bar_range(&project, a.start_bar, a.end_bar)?;
            let audio = session.render_loop(range).map_err(|e| e.to_string())?;
            let seam = analysis::seam_check(&audio, 0, audio.frames());
            let stats = analysis::analyze(&audio);
            Ok(json!({
                "seam": seam,
                "seconds": round(audio.duration_seconds(), 3),
                "integratedLufs": stats.integrated_lufs,
                "truePeakDbfs": stats.true_peak_dbfs,
            })
            .to_string())
        })
        .await
    }

    #[tool(description = "Plays, stops or moves the playhead in the app, or reports where it is.")]
    async fn transport(&self, Parameters(a): Parameters<TransportArgs>) -> ToolResult {
        let s = &self.session;
        match a.action.to_ascii_lowercase().as_str() {
            "play" => s.play(),
            "stop" => s.stop(),
            "seek" => {
                let bar = a.bar.ok_or("seek needs a bar")?;
                let tick = bar_tick(&s.project(), bar)?;
                s.seek(tick)
            }
            "status" => Ok(()),
            other => {
                return Err(format!(
                    "action {other:?} is not play, stop, seek or status"
                ));
            }
        }
        .map_err(|e| e.to_string())?;
        let t = s.transport();
        let project = s.project();
        Ok(json!({
            "deviceRunning": t.running,
            "playing": t.playing,
            "bar": round(project.ticks_to_bar(t.position_ticks.round() as Ticks), 3),
            "dropouts": t.dropouts,
        })
        .to_string())
    }

    #[tool(description = "Sets the project tempo.")]
    async fn set_tempo(
        &self,
        Parameters(a): Parameters<TempoArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        self.apply(
            &ctx,
            "set_tempo",
            format!("Set tempo to {} BPM", a.bpm),
            vec![Command::SetTempo { bpm: a.bpm }],
        )?;
        Ok(format!("Tempo is now {} BPM.", a.bpm))
    }

    #[tool(description = "Sets the time signature, such as 3/4 or 6/8.")]
    async fn set_time_signature(
        &self,
        Parameters(a): Parameters<TimeSignatureArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        self.apply(
            &ctx,
            "set_time_signature",
            format!("Set time signature to {}/{}", a.numerator, a.denominator),
            vec![Command::SetTimeSignature {
                numerator: a.numerator,
                denominator: a.denominator,
            }],
        )?;
        Ok(format!(
            "Time signature is now {}/{}.",
            a.numerator, a.denominator
        ))
    }

    #[tool(description = "Sets the loop region in bars and turns looping on (or off).")]
    async fn set_loop(
        &self,
        Parameters(a): Parameters<LoopArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let (s, e) =
            bar_range(&project, Some(a.start_bar), Some(a.end_bar))?.ok_or("empty range")?;
        let enabled = a.enabled.unwrap_or(true);
        self.apply(
            &ctx,
            "set_loop",
            format!("Loop bars {} to {}", a.start_bar, a.end_bar),
            vec![
                Command::SetLoopRegion { start: s, end: e },
                Command::SetLoopEnabled { enabled },
            ],
        )?;
        Ok(format!(
            "Loop is bars {} to {} ({} bars), {}.",
            a.start_bar,
            a.end_bar,
            round(a.end_bar - a.start_bar, 3),
            if enabled { "on" } else { "off" }
        ))
    }

    #[tool(
        description = "Adds a track: an instrument track with a synth, piano, Acid bass, drum kit, sampler or sample bank, or an audio track. Returns its ID. Load sounds into a bank track with bank_load."
    )]
    async fn track_create(
        &self,
        Parameters(a): Parameters<TrackCreateArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let mut track = match a
            .kind
            .as_deref()
            .unwrap_or("instrument")
            .to_ascii_lowercase()
            .as_str()
        {
            "audio" => Track::new_audio(a.name.clone()),
            "instrument" | "midi" => {
                let name = a.instrument.as_deref().unwrap_or("synth");
                let kind = InstrumentType::from_name(name).ok_or_else(|| {
                    format!("instrument {name:?} is not one of {}", instrument_names())
                })?;
                Track::new_instrument(a.name.clone(), kind)
            }
            other => return Err(format!("kind {other:?} is not instrument or audio")),
        };
        if let Some(slot) = track.instrument.as_mut() {
            for (k, v) in a.params.iter().flatten() {
                if let Some(x) = param_value(slot.kind.params(), slot.kind.name(), k, v)? {
                    slot.params.insert(k.clone(), x);
                }
            }
            if let Some(s) = &a.sample {
                slot.sample = Some(find_audio(&project, s)?);
            }
        } else if a.params.is_some() || a.sample.is_some() {
            return Err("audio tracks have no instrument parameters".into());
        }
        if let Some(c) = &a.color {
            track.color = parse_color(c)?;
        }
        let id = track.id;
        self.apply(
            &ctx,
            "track_create",
            format!("Add track {}", a.name),
            vec![Command::AddTrack {
                track: Box::new(track),
                index: a.index,
            }],
        )?;
        Ok(json!({ "track": id.to_string(), "name": a.name }).to_string())
    }

    #[tool(
        description = "Deletes a track and its clips.",
        annotations(destructive_hint = true)
    )]
    async fn track_delete(
        &self,
        Parameters(a): Parameters<TrackArg>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let id = find_track(&project, &a.track)?;
        let name = project
            .track(id)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        self.apply(
            &ctx,
            "track_delete",
            format!("Delete track {name}"),
            vec![Command::RemoveTrack { id }],
        )?;
        Ok(format!("Deleted track {name}."))
    }

    #[tool(
        description = "Changes a track's name, color, picture symbol, position, or its mixer volume, pan, mute and solo. Use track \"master\" for the master volume."
    )]
    async fn track_set(
        &self,
        Parameters(a): Parameters<TrackSetArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let channel = find_channel(&project, &a.track)?;
        let mut cmds = Vec::new();
        match channel {
            ChannelRef::Master => {
                if a.name.is_some() || a.color.is_some() || a.index.is_some() || a.picture.is_some()
                {
                    return Err("the master has no name, color, picture or position".into());
                }
            }
            ChannelRef::Track(id) => {
                if let Some(name) = &a.name {
                    cmds.push(Command::SetTrackName {
                        id,
                        name: name.clone(),
                    });
                }
                if let Some(c) = &a.color {
                    cmds.push(Command::SetTrackColor {
                        id,
                        color: parse_color(c)?,
                    });
                }
                if let Some(text) = &a.picture {
                    let picture = if text.trim().eq_ignore_ascii_case("auto") {
                        None
                    } else {
                        let symbol = Symbol::parse(text).ok_or_else(|| {
                            let names: Vec<&str> = Symbol::ALL.iter().map(|s| s.label()).collect();
                            format!(
                                "no picture symbol {text:?}; use auto or one of: {}",
                                names.join(", ")
                            )
                        })?;
                        Some(Picture::symbol(symbol))
                    };
                    cmds.push(Command::SetTrackPicture { id, picture });
                }
                if let Some(index) = a.index {
                    cmds.push(Command::MoveTrack { id, index });
                }
            }
        }
        if a.volume_db.is_some() || a.pan.is_some() || a.mute.is_some() || a.solo.is_some() {
            cmds.push(Command::SetChannel {
                channel,
                volume_db: a.volume_db,
                pan: a.pan,
                mute: a.mute,
                solo: a.solo,
            });
        }
        if cmds.is_empty() {
            return Err("nothing to change: give at least one setting".into());
        }
        self.apply(&ctx, "track_set", format!("Change {}", a.track), cmds)?;
        let project = self.session.project();
        Ok(match channel {
            ChannelRef::Master => json!({ "volumeDb": project.master.volume_db }).to_string(),
            ChannelRef::Track(id) => project
                .track(id)
                .map(|t| track_json(&project, t, false).to_string())
                .unwrap_or_default(),
        })
    }

    #[tool(
        description = "Changes a track's instrument: replace it with another type, set parameters (null restores a default), or choose the sampler's audio."
    )]
    async fn instrument_set(
        &self,
        Parameters(a): Parameters<InstrumentSetArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let id = find_track(&project, &a.track)?;
        let track = project.track(id).map_err(|e| e.to_string())?;
        if track.kind != TrackKind::Instrument {
            return Err(format!(
                "{} is an audio track and has no instrument",
                track.name
            ));
        }
        let mut slot = match &a.kind {
            Some(name) => {
                InstrumentSlot::new(InstrumentType::from_name(name).ok_or_else(|| {
                    format!("instrument {name:?} is not one of {}", instrument_names())
                })?)
            }
            None => track
                .instrument
                .clone()
                .unwrap_or_else(|| InstrumentSlot::new(InstrumentType::Synth)),
        };
        for (k, v) in a.params.iter().flatten() {
            match param_value(slot.kind.params(), slot.kind.name(), k, v)? {
                Some(x) => {
                    slot.params.insert(k.clone(), x);
                }
                None => {
                    slot.params.remove(k);
                }
            }
        }
        if let Some(s) = &a.sample {
            slot.sample = Some(find_audio(&project, s)?);
        }
        if let Some(p) = &a.pitched {
            let bank = slot.bank.as_mut().ok_or_else(|| {
                format!("{} has no sample bank; load one with bank_load", track.name)
            })?;
            bank.pitched =
                match p.trim() {
                    "" => None,
                    name => Some(bank.resolve(name).map(|(n, _)| n.to_string()).ok_or_else(
                        || {
                            format!(
                                "the bank has no sound {name:?}; it has: {}",
                                names(bank.sounds.keys().map(String::as_str))
                            )
                        },
                    )?),
                };
        }
        self.apply(
            &ctx,
            "instrument_set",
            format!("Change {} instrument", track.name),
            vec![Command::SetInstrument {
                track: id,
                instrument: Some(slot),
            }],
        )?;
        let project = self.session.project();
        let t = project.track(id).map_err(|e| e.to_string())?;
        Ok(track_json(&project, t, false)["instrument"].to_string())
    }

    #[tool(description = "Inserts an effect on a track or the master. Returns its ID.")]
    async fn effect_insert(
        &self,
        Parameters(a): Parameters<EffectInsertArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let channel = find_channel(&project, &a.channel)?;
        let kind = EffectType::from_name(&a.kind).ok_or_else(|| {
            format!(
                "effect {:?} is not one of: {}",
                a.kind,
                names(EffectType::ALL.iter().map(|t| t.name()))
            )
        })?;
        let mut slot = EffectSlot::new(kind);
        for (k, v) in a.params.iter().flatten() {
            if let Some(x) = param_value(kind.params(), kind.name(), k, v)? {
                slot.params.insert(k.clone(), x);
            }
        }
        let own = match channel {
            ChannelRef::Track(t) => Some(t),
            ChannelRef::Master => None,
        };
        slot.sidechain = match &a.sidechain {
            Some(r) => find_sidechain(&project, r)?,
            None if kind.takes_sidechain() => project.default_sidechain(own),
            None => None,
        };
        let id = slot.id;
        self.apply(
            &ctx,
            "effect_insert",
            format!("Add {} on {}", kind.label(), a.channel),
            vec![Command::AddEffect {
                channel,
                effect: slot,
                index: a.index,
            }],
        )?;
        let project = self.session.project();
        let slot = channel_effects(&project, channel)
            .iter()
            .find(|e| e.id == id)
            .ok_or("effect not found")?;
        let mut v = effect_json(&project, slot);
        if kind.takes_sidechain() && slot.sidechain.is_none() {
            v["warning"] = json!(
                "no instrument track to follow yet; set sidechain with effect_set to make it duck"
            );
        }
        Ok(v.to_string())
    }

    #[tool(
        description = "Sets an effect's parameters (null restores a default) or turns it on or off."
    )]
    async fn effect_set(
        &self,
        Parameters(a): Parameters<EffectSetArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let channel = find_channel(&project, &a.channel)?;
        let effect = find_effect(&project, channel, &a.effect)?;
        let kind = channel_effects(&project, channel)
            .iter()
            .find(|e| e.id == effect)
            .map(|e| e.kind)
            .ok_or("effect not found")?;
        let mut cmds = Vec::new();
        for (k, v) in a.params.iter().flatten() {
            let value = param_value(kind.params(), kind.name(), k, v)?;
            cmds.push(Command::SetEffectParam {
                channel,
                effect,
                param: k.clone(),
                value,
            });
        }
        if let Some(enabled) = a.enabled {
            cmds.push(Command::SetEffectEnabled {
                channel,
                effect,
                enabled,
            });
        }
        if let Some(r) = &a.sidechain {
            cmds.push(Command::SetEffectSidechain {
                channel,
                effect,
                source: find_sidechain(&project, r)?,
            });
        }
        if cmds.is_empty() {
            return Err("nothing to change: give params, enabled or sidechain".into());
        }
        self.apply(
            &ctx,
            "effect_set",
            format!("Change {} on {}", kind.label(), a.channel),
            cmds,
        )?;
        let project = self.session.project();
        let slot = channel_effects(&project, channel)
            .iter()
            .find(|e| e.id == effect)
            .cloned();
        Ok(json!(slot.map(|s| effect_json(&project, &s))).to_string())
    }

    #[tool(description = "Removes an effect from a track or the master.")]
    async fn effect_remove(
        &self,
        Parameters(a): Parameters<EffectRemoveArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let channel = find_channel(&project, &a.channel)?;
        let id = find_effect(&project, channel, &a.effect)?;
        self.apply(
            &ctx,
            "effect_remove",
            format!("Remove effect from {}", a.channel),
            vec![Command::RemoveEffect { channel, id }],
        )?;
        Ok("Removed.".into())
    }

    #[tool(
        description = "Adds a clip: a MIDI clip (notes in the compact list), a pattern clip (mini-notation) on an instrument track, or an audio clip on an audio track. Step clips are added with steps_write. Returns the clip."
    )]
    async fn clip_create(
        &self,
        Parameters(a): Parameters<ClipCreateArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let track = find_track(&project, &a.track)?;
        let start = bar_tick(&project, a.start_bar)?;
        let length = bars_len(&project, a.length_bars)?;
        let given = [a.notes.is_some(), a.pattern.is_some(), a.audio.is_some()]
            .iter()
            .filter(|x| **x)
            .count();
        if given > 1 {
            return Err("give notes, pattern or audio, not more than one".into());
        }
        if a.pattern.is_none()
            && (a.controls.is_some() || a.swing.is_some() || a.swing_grid.is_some())
        {
            return Err("controls and swing are for pattern clips; give a pattern too".into());
        }
        let (content, default_name) = if let Some(audio) = &a.audio {
            let source = find_audio(&project, audio)?;
            let name = project
                .pool_entry(source)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            (
                ClipContent::Audio {
                    source,
                    offset: 0,
                    gain_db: a.gain_db.unwrap_or(0.0),
                    fade_in: 0,
                    fade_out: 0,
                },
                name,
            )
        } else if let Some(p) = &a.pattern {
            if sky_library::find_samples(p).is_ok_and(|c| !c.is_empty()) {
                return Err(
                    "create the clip with the pattern alone, then give pattern_set the \
                     samples(...) line with it to load the sounds"
                        .into(),
                );
            }
            let cycle = bars_len(&project, a.cycle_bars.unwrap_or(1.0))?;
            let controls = a.controls.clone().unwrap_or_default();
            check_controls(&project, track, controls.keys().map(String::as_str))?;
            (
                ClipContent::Pattern {
                    source: p.clone(),
                    seed: a.seed.unwrap_or(0),
                    cycle,
                    controls,
                    swing: swing_of(Swing::default(), a.swing, a.swing_grid),
                },
                "Pattern".to_string(),
            )
        } else {
            let notes =
                parse_note_list(a.notes.as_deref().unwrap_or("")).map_err(|e| e.to_string())?;
            (ClipContent::Midi { notes }, "Clip".to_string())
        };
        let clip = Clip {
            id: ClipId::new(),
            name: a.name.clone().unwrap_or(default_name),
            start,
            length,
            muted: false,
            content,
        };
        let id = clip.id;
        self.apply(
            &ctx,
            "clip_create",
            format!("Add clip {}", clip.name),
            vec![Command::AddClip {
                track,
                clip: Box::new(clip),
            }],
        )?;
        let project = self.session.project();
        let (_, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        Ok(clip_json(&project, clip, true).to_string())
    }

    #[tool(description = "Deletes a clip.", annotations(destructive_hint = true))]
    async fn clip_delete(
        &self,
        Parameters(a): Parameters<ClipArg>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let id = find_clip(&project, &a.clip)?;
        self.apply(
            &ctx,
            "clip_delete",
            "Delete clip".into(),
            vec![Command::RemoveClip { id }],
        )?;
        Ok("Deleted.".into())
    }

    #[tool(
        description = "Moves a clip to another bar or track, changes its length, or renames it."
    )]
    async fn clip_move(
        &self,
        Parameters(a): Parameters<ClipMoveArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let id = find_clip(&project, &a.clip)?;
        let (track, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        let mut cmds = Vec::new();
        if a.start_bar.is_some() || a.track.is_some() {
            let start = match a.start_bar {
                Some(b) => bar_tick(&project, b)?,
                None => clip.start,
            };
            let track = match &a.track {
                Some(t) => find_track(&project, t)?,
                None => track.id,
            };
            cmds.push(Command::MoveClip { id, track, start });
        }
        if let Some(l) = a.length_bars {
            cmds.push(Command::SetClipLength {
                id,
                length: bars_len(&project, l)?,
            });
        }
        if let Some(n) = &a.name {
            cmds.push(Command::SetClipName {
                id,
                name: n.clone(),
            });
        }
        if cmds.is_empty() {
            return Err("nothing to change: give start_bar, track, length_bars or name".into());
        }
        self.apply(
            &ctx,
            "clip_move",
            format!("Change clip {}", clip.name),
            cmds,
        )?;
        let project = self.session.project();
        let (_, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        Ok(clip_json(&project, clip, false).to_string())
    }

    #[tool(
        description = "Writes notes into a MIDI clip from the compact note list, replacing its notes (default) or adding to them. Note positions are relative to the clip start."
    )]
    async fn notes_write(
        &self,
        Parameters(a): Parameters<NotesWriteArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let id = find_clip(&project, &a.clip)?;
        let (_, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        let ClipContent::Midi { notes: old } = &clip.content else {
            return Err(format!(
                "{} is a {} clip, not a MIDI clip",
                clip.name,
                clip.kind_name()
            ));
        };
        let notes = parse_note_list(&a.notes).map_err(|e| e.to_string())?;
        let mut cmds = Vec::new();
        match a.mode.as_deref().unwrap_or("replace") {
            "replace" => {
                if !old.is_empty() {
                    cmds.push(Command::RemoveNotes {
                        clip: id,
                        ids: old.iter().map(|n| n.id).collect(),
                    });
                }
            }
            "add" => {}
            other => return Err(format!("mode {other:?} is not replace or add")),
        }
        let count = notes.len();
        if !notes.is_empty() {
            cmds.push(Command::AddNotes { clip: id, notes });
        }
        if cmds.is_empty() {
            return Err("there are no notes to write".into());
        }
        self.apply(
            &ctx,
            "notes_write",
            format!("Write notes in {}", clip.name),
            cmds,
        )?;
        let project = self.session.project();
        let (_, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        let ClipContent::Midi { notes } = &clip.content else {
            return Err("the clip changed type".into());
        };
        let beyond = notes.iter().filter(|n| n.start >= clip.length).count();
        let mut v =
            json!({ "written": count, "noteCount": notes.len(), "notes": format_note_list(notes) });
        if beyond > 0 {
            v["warning"] = json!(format!(
                "{beyond} note(s) start after the clip ends and will not play; lengthen the clip with clip_move"
            ));
        }
        Ok(v.to_string())
    }

    #[tool(
        description = "Changes a pattern clip: its mini-notation or Strudel code, cycle length, seed, control patterns (velocity or instrument parameters per note) or swing. Returns the clip, with warnings about Strudel functions it leaves out."
    )]
    async fn pattern_set(
        &self,
        Parameters(a): Parameters<PatternSetArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let id = find_clip(&project, &a.clip)?;
        let (track, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        let ClipContent::Pattern {
            source,
            seed,
            cycle,
            controls,
            swing,
        } = &clip.content
        else {
            return Err(format!(
                "{} is a {} clip, not a pattern clip",
                clip.name,
                clip.kind_name()
            ));
        };
        let cycle = match a.cycle_bars {
            Some(b) => bars_len(&project, b)?,
            None => *cycle,
        };
        let mut controls = controls.clone();
        for (name, value) in a.controls.iter().flatten() {
            match value {
                Some(v) => {
                    controls.insert(name.clone(), v.clone());
                }
                None => {
                    controls.remove(name);
                }
            }
        }
        if let Some(changed) = &a.controls {
            check_controls(
                &project,
                track.id,
                changed
                    .iter()
                    .filter(|(_, v)| v.is_some())
                    .map(|(k, _)| k.as_str()),
            )?;
        }
        let (text, request, note) = match &a.pattern {
            Some(p) => self.pattern_sounds(track, p)?,
            None => (source.clone(), None, None),
        };
        let label = format!("Change pattern {}", clip.name);
        let command = Command::SetPattern {
            clip: id,
            source: text,
            seed: a.seed.unwrap_or(*seed),
            cycle,
            controls,
            swing: swing_of(*swing, a.swing, a.swing_grid),
        };
        let mut loaded = None;
        match request {
            Some(request) => {
                let session = self.session.clone();
                let (track, origin) = (track.id, origin(&ctx, "pattern_set"));
                loaded = Some(
                    blocking(move || {
                        session
                            .load_pattern_sounds(track, &request, &label, vec![command], origin)
                            .map_err(|e| e.to_string())
                    })
                    .await?,
                );
            }
            None => {
                self.apply(&ctx, "pattern_set", label, vec![command])?;
            }
        }
        let project = self.session.project();
        let (_, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        let mut v = clip_json(&project, clip, true);
        if let Some(done) = loaded {
            v["sounds"] = serde_json::to_value(&done).map_err(|e| e.to_string())?;
            if done.packs.iter().any(|p| p.license == License::Unknown) {
                v["licenseNote"] = json!(
                    "A linked pack does not state a license; exports will warn until its terms are checked."
                );
            }
        }
        if let Some(note) = note {
            v["note"] = json!(note);
        }
        if let ClipContent::Pattern { source, .. } = &clip.content
            && let Ok(p) = sky_pattern::parse(source)
            && !p.warnings().is_empty()
        {
            let warnings: Vec<&str> = p.warnings().iter().map(|w| w.message.as_str()).collect();
            v["warnings"] = json!(warnings);
        }
        Ok(v.to_string())
    }

    #[tool(
        description = "Writes a step clip, as on a drum machine or an acid bass: lanes of steps, each lane on a key (a drum or a note) and of its own length, with accents, slides, ties, ratchets, probabilities, conditions, micro-timing and parameter locks. Give clip to change one, or track to add one. Returns the clip."
    )]
    async fn steps_write(
        &self,
        Parameters(a): Parameters<StepsWriteArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let existing = match &a.clip {
            Some(c) => {
                let id = find_clip(&project, c)?;
                let (track, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
                let ClipContent::Steps(steps) = &clip.content else {
                    return Err(format!(
                        "{} is a {} clip, not a step clip; leave out clip and give track to add one",
                        clip.name,
                        clip.kind_name()
                    ));
                };
                Some((id, track.id, steps.clone(), clip.name.clone()))
            }
            None => None,
        };
        let track = match (&existing, &a.track) {
            (Some((_, track, _, _)), None) => *track,
            (Some(_), Some(_)) => {
                return Err(
                    "give clip to change a step clip, or track to add one, not both".into(),
                );
            }
            (None, Some(t)) => find_track(&project, t)?,
            (None, None) => {
                return Err("give clip to change a step clip, or track to add one".into());
            }
        };
        let mut steps = match &existing {
            Some((_, _, steps, _)) => steps.clone(),
            None => {
                if a.lanes.is_none() {
                    return Err("a new step clip needs lanes".into());
                }
                StepClip::new(16, Vec::new())
            }
        };
        if let Some(rate) = a.rate {
            steps.rate = rate;
        }
        steps.swing = swing_of(steps.swing, a.swing, a.swing_grid);
        if let Some(seed) = a.seed {
            steps.seed = seed;
        }
        if let Some(lanes) = &a.lanes {
            steps.lanes = lanes
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    step_lane(&project, track, l).map_err(|e| format!("lane {}: {e}", i + 1))
                })
                .collect::<Result<_, _>>()?;
        }
        let (cmd, id, label) = match existing {
            Some((id, _, _, name)) => (
                Command::SetSteps {
                    clip: id,
                    steps: Box::new(steps),
                },
                id,
                format!("Write steps in {name}"),
            ),
            None => {
                let clip = Clip {
                    id: ClipId::new(),
                    name: a.name.clone().unwrap_or_else(|| "Steps".into()),
                    start: bar_tick(&project, a.start_bar.unwrap_or(1.0))?,
                    length: bars_len(&project, a.length_bars.unwrap_or(1.0))?,
                    muted: false,
                    content: ClipContent::Steps(steps),
                };
                let id = clip.id;
                let label = format!("Add step clip {}", clip.name);
                (
                    Command::AddClip {
                        track,
                        clip: Box::new(clip),
                    },
                    id,
                    label,
                )
            }
        };
        let mut cmds = vec![cmd];
        if let (Some(name), Some(_)) = (&a.name, &a.clip) {
            cmds.push(Command::SetClipName {
                id,
                name: name.clone(),
            });
        }
        drop(project);
        self.apply(&ctx, "steps_write", label, cmds)?;
        let project = self.session.project();
        let (_, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        Ok(clip_json(&project, clip, true).to_string())
    }

    #[tool(description = "Undoes the latest change or changes, whoever made them.")]
    async fn undo(&self, Parameters(a): Parameters<UndoArgs>) -> ToolResult {
        let steps = a.steps.unwrap_or(1).clamp(1, 50);
        let mut done = Vec::new();
        for _ in 0..steps {
            match self.session.undo() {
                Ok(label) => done.push(label),
                Err(e) if done.is_empty() => return Err(e.to_string()),
                Err(_) => break,
            }
        }
        Ok(json!({ "undone": done, "nextUndo": self.session.view().undo_label }).to_string())
    }

    #[tool(description = "Redoes the latest undone change.")]
    async fn redo(&self) -> ToolResult {
        let label = self.session.redo().map_err(|e| e.to_string())?;
        Ok(format!("Redid: {label}."))
    }

    #[tool(
        description = "Starts a new project: empty, the demo, or the techno or acid loop template. The open project must be saved first unless discard_unsaved is true.",
        annotations(destructive_hint = true)
    )]
    async fn project_new(&self, Parameters(a): Parameters<ProjectNewArgs>) -> ToolResult {
        self.check_unsaved(a.discard_unsaved)?;
        let name = a.name.unwrap_or_else(|| "Untitled".into());
        let mut project = match a.template.as_deref().unwrap_or("empty") {
            "empty" => empty_project(&name),
            "demo" => demo_project(),
            "techno" => techno_project(),
            "acid" => acid_project(),
            other => {
                return Err(format!(
                    "template {other:?} is not empty, demo, techno or acid"
                ));
            }
        };
        project.name = name;
        self.session.new_project(project);
        Ok(project_summary(&self.session.project()).to_string())
    }

    #[tool(
        description = "Opens a .skyproj project folder. Changes a crash left unsaved are recovered."
    )]
    async fn project_open(&self, Parameters(a): Parameters<ProjectOpenArgs>) -> ToolResult {
        self.check_unsaved(a.discard_unsaved)?;
        let path = self.policy.resolve(&a.path)?;
        let session = self.session.clone();
        let replayed = blocking(move || session.open(&path).map_err(|e| e.to_string())).await?;
        let mut v = project_summary(&self.session.project());
        if replayed > 0 {
            v["recoveredChanges"] = json!(replayed);
        }
        Ok(v.to_string())
    }

    #[tool(description = "Saves the project, into a new folder if a path is given.")]
    async fn project_save(&self, Parameters(a): Parameters<ProjectSaveArgs>) -> ToolResult {
        let path = match &a.path {
            Some(p) => {
                let mut p = PathBuf::from(p);
                if p.extension()
                    .is_none_or(|e| e != sky_model::file::EXTENSION)
                {
                    p.set_extension(sky_model::file::EXTENSION);
                }
                Some(self.policy.resolve(&p.to_string_lossy())?)
            }
            None => {
                if let Some(p) = &self.session.view().path {
                    // Re-check: the folder may predate the policy.
                    self.policy.resolve(&p.to_string_lossy())?;
                }
                None
            }
        };
        let session = self.session.clone();
        let dir =
            blocking(move || session.save(path.as_deref()).map_err(|e| e.to_string())).await?;
        Ok(format!("Saved to {}.", dir.display()))
    }

    #[tool(
        description = "Imports an audio file (WAV, FLAC, OGG, MP3) into the project's audio pool, for audio clips or the sampler. Returns its ID."
    )]
    async fn audio_import(
        &self,
        Parameters(a): Parameters<PathArg>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let path = self.policy.resolve(&a.path)?;
        let session = self.session.clone();
        let origin = origin(&ctx, "audio_import");
        let id = blocking(move || {
            session
                .import_audio(&path, origin)
                .map_err(|e| e.to_string())
        })
        .await?;
        let project = self.session.project();
        let entry = project.pool_entry(id).map_err(|e| e.to_string())?;
        Ok(json!({
            "audio": id.to_string(),
            "name": entry.name,
            "channels": entry.channels,
            "seconds": round(entry.frames as f64 / entry.sample_rate.max(1) as f64, 3),
        })
        .to_string())
    }

    #[tool(
        description = "Lists Strudel's sample packs and their licenses, or looks into one pack: its banks (such as RolandTR808) and plain sounds, or one bank's sounds, each with its number of variations.",
        annotations(read_only_hint = true)
    )]
    async fn sound_packs(&self, Parameters(a): Parameters<SoundPacksArgs>) -> ToolResult {
        let Some(pack) = a.pack else {
            let packs: Vec<Value> = PACKS
                .iter()
                .map(|p| {
                    json!({
                        "pack": p.id,
                        "name": p.name,
                        "license": p.license.id(),
                        "credit": p.credit,
                        "about": p.about,
                        "homepage": p.homepage,
                    })
                })
                .collect();
            return Ok(json!({
                "packs": packs,
                "note": "Only VCSL is CC0. Packs whose license is unknown can be used, but \
                         their terms must be checked before a game ships their sounds.",
            })
            .to_string());
        };
        let source = self.pack_source(&pack, None)?;
        let library = self.session.library();
        blocking(move || {
            let opened = library.open(&source).map_err(|e| e.to_string())?;
            let counts = |sounds: std::collections::BTreeMap<String, &[sky_library::MapFile]>| {
                sounds
                    .into_iter()
                    .take(300)
                    .map(|(name, files)| (name, json!(files.len())))
                    .collect::<serde_json::Map<String, Value>>()
            };
            let banks = opened.map.banks();
            let mut v = json!({
                "pack": source.name,
                "license": source.license.id(),
                "map": opened.location,
                "offline": opened.offline,
            });
            match a.bank.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
                Some(bank) => {
                    let sounds = opened.map.bank_sounds(Some(bank));
                    if sounds.is_empty() {
                        return Err(format!(
                            "{} has no bank {bank:?}; its banks are: {}",
                            source.name,
                            names(banks.keys().map(String::as_str))
                        ));
                    }
                    v["bank"] = json!(bank);
                    v["sounds"] = json!(counts(sounds));
                }
                None => {
                    v["banks"] = json!(banks);
                    v["sounds"] = json!(counts(opened.map.plain_sounds()));
                }
            }
            Ok(v.to_string())
        })
        .await
    }

    #[tool(
        description = "Lists the recorded instrument libraries the piano can play: each one's id, size, license and whether it is downloaded.",
        annotations(read_only_hint = true)
    )]
    async fn instrument_libraries(&self) -> ToolResult {
        let libraries: Vec<Value> = self
            .session
            .instrument_libraries()
            .into_iter()
            .map(|s| {
                let i = s.info;
                json!({
                    "id": i.id,
                    "name": i.name,
                    "instrument": i.kind.name(),
                    "version": i.version,
                    "about": i.description,
                    "license": i.license.id(),
                    "credit": i.credit,
                    "homepage": i.homepage,
                    "megabytes": round(i.size as f64 / 1e6, 1),
                    "builtIn": i.is_bundled(),
                    "downloaded": s.installed,
                })
            })
            .collect();
        Ok(json!({
            "libraries": libraries,
            "folder": self.session.instruments().dir().display().to_string(),
        })
        .to_string())
    }

    #[tool(
        description = "Makes a track a piano that plays a library from instrument_libraries, as one undo step, downloading the library first if needed (tens of MB, checked against its SHA-256). A track that is a piano already keeps its settings."
    )]
    async fn piano_library(
        &self,
        Parameters(a): Parameters<PianoLibraryArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let track = find_track(&project, &a.track)?;
        let session = self.session.clone();
        let origin = origin(&ctx, "piano_library");
        let download = a.download.unwrap_or(true);
        let set = blocking(move || {
            session
                .set_piano_library(track, &a.library, download, origin)
                .map_err(|e| e.to_string())
        })
        .await?;
        let mut v = serde_json::to_value(&set).map_err(|e| e.to_string())?;
        v["track"] = json!(track.to_string());
        v["license"] = json!(set.license.id());
        v["memoryMb"] = json!(round(set.memory_mb, 1));
        Ok(v.to_string())
    }

    #[tool(
        description = "Downloads sounds from a sample pack into a track's sample bank, as one undo step: a whole bank such as RolandTR808, or named sounds. Pattern clips on the track then play them by name (bd sd:1 hh). Downloads are cached, and sounds already in the project are reused."
    )]
    async fn bank_load(
        &self,
        Parameters(a): Parameters<BankLoadArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let track = find_track(&project, &a.track)?;
        let request = BankRequest {
            source: self.pack_source(&a.pack, a.license.as_deref())?,
            bank: a.bank,
            sounds: a.sounds.unwrap_or_default(),
            add: a.add.unwrap_or(false),
            pitched: a.pitched,
        };
        let session = self.session.clone();
        let origin = origin(&ctx, "bank_load");
        let loaded = blocking(move || {
            session
                .load_bank(track, &request, origin)
                .map_err(|e| e.to_string())
        })
        .await?;
        let sounds: serde_json::Map<String, Value> = loaded
            .sounds
            .iter()
            .map(|(name, n)| (name.clone(), json!(n)))
            .collect();
        let example: Vec<&str> = loaded
            .sounds
            .iter()
            .take(3)
            .map(|(n, _)| n.as_str())
            .collect();
        let mut v = json!({
            "track": track.to_string(),
            "bank": loaded.bank,
            "pack": loaded.pack,
            "license": loaded.license.id(),
            "sounds": sounds,
            "pitched": loaded.pitched,
            "newFiles": loaded.new_files,
            "reusedFiles": loaded.reused_files,
            "offline": loaded.offline,
            "pattern": example.join(" "),
        });
        if !loaded.failed.is_empty() {
            v["failedFiles"] = json!(loaded.failed.len());
            v["failed"] = json!(loaded.failed.iter().take(5).collect::<Vec<_>>());
        }
        if loaded.license == License::Unknown {
            v["licenseNote"] = json!(
                "This pack does not state a license; exports will warn until its terms are checked."
            );
        }
        Ok(v.to_string())
    }

    #[tool(
        description = "Renders the arrangement (or the given bars) with a tail and writes a WAV file."
    )]
    async fn export_wav(&self, Parameters(a): Parameters<ExportWavArgs>) -> ToolResult {
        let path = self.policy.resolve(&a.path)?;
        let format = wav_format(a.bits)?;
        let session = self.session.clone();
        blocking(move || {
            let project = session.project();
            let range = bar_range(&project, a.start_bar, a.end_bar)?;
            let audio = session
                .export_wav(&path, range, a.tail_seconds.unwrap_or(2.0), format)
                .map_err(|e| e.to_string())?;
            let stats = analysis::analyze(&audio);
            let licenses = session.license_report();
            Ok(json!({
                "file": path.display().to_string(),
                "seconds": round(audio.duration_seconds(), 3),
                "integratedLufs": stats.integrated_lufs,
                "truePeakDbfs": stats.true_peak_dbfs,
                "credits": licenses.credits,
                "licenseWarnings": licenses.warnings,
            })
            .to_string())
        })
        .await
    }

    #[tool(
        description = "Renders the loop region (or the given bars) as a seamless game loop, writes the WAV into the export folder and records it in skyware-manifest.json for the game runtime. Exporting the same name again replaces it and keeps its ID."
    )]
    async fn export_loop(&self, Parameters(a): Parameters<ExportLoopArgs>) -> ToolResult {
        let dir = match &a.dir {
            Some(d) => Some(self.policy.resolve(d)?),
            None => match &self.session.view().path {
                Some(p) => Some(
                    self.policy
                        .resolve(&p.join("renders").join("export").to_string_lossy())?,
                ),
                None => return Err("save the project first, or give an export dir".into()),
            },
        };
        let format = wav_format(a.bits)?;
        let category = category(a.category.as_deref())?;
        let session = self.session.clone();
        blocking(move || {
            let project = session.project();
            let range = bar_range(&project, a.start_bar, a.end_bar)?;
            let out = session
                .export_loop(&LoopExportOptions {
                    name: a.name,
                    dir,
                    range,
                    category,
                    tags: a.tags.unwrap_or_default(),
                    format,
                })
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "asset": out.exported.asset,
                "file": out.exported.file.display().to_string(),
                "manifest": out.exported.manifest.display().to_string(),
                "bars": out.bars,
                "seam": out.seam,
                "credits": out.licenses.credits,
                "licenseWarnings": out.licenses.warnings,
            })
            .to_string())
        })
        .await
    }

    #[tool(
        description = "Adds or changes a game asset: a loop (a body that repeats seamlessly, with an optional intro that plays once) or an SFX set (variations the game picks one of per trigger, with random pitch and gain). Assets are rendered from the arrangement; game_export writes them all for the game. Give name plus kind to add one; give name and only what changes to edit one."
    )]
    async fn game_asset_set(
        &self,
        Parameters(a): Parameters<GameAssetSetArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let existing = project.game.find(a.name.trim()).cloned();
        let kind = match (a.kind.as_deref(), &existing) {
            (Some(k), _) => match k.trim().to_ascii_lowercase().as_str() {
                "loop" => "loop",
                "sfx" => "sfx",
                other => return Err(format!("kind {other:?} is not loop or sfx")),
            },
            (None, Some(e)) => e.kind.kind_name(),
            (None, None) => {
                return Err(format!(
                    "no game asset is named {:?}; to add one, give kind: loop or sfx. The game \
                     assets are: {}",
                    a.name,
                    names(project.game.assets.iter().map(|a| a.name.as_str()))
                ));
            }
        };
        let is_loop = kind == "loop";
        let sfx_only = [
            ("count", a.count.is_some()),
            ("variations", a.variations.is_some()),
            ("selection", a.selection.is_some()),
            ("pitch_semitones", a.pitch_semitones.is_some()),
            ("gain_db", a.gain_db.is_some()),
            ("tail_seconds", a.tail_seconds.is_some()),
            ("mono", a.mono.is_some()),
        ];
        if is_loop {
            let given: Vec<&str> = sfx_only
                .iter()
                .filter(|(_, g)| *g)
                .map(|(n, _)| *n)
                .collect();
            if !given.is_empty() {
                return Err(format!("{} are for SFX sets, not loops", given.join(", ")));
            }
        } else if a.intro_start_bar.is_some() {
            return Err("intro_start_bar is for loops, not SFX sets".into());
        }
        let mut asset = existing.clone().unwrap_or_else(|| GameAsset {
            id: AssetId::new(),
            name: a.name.trim().to_string(),
            category: if is_loop {
                Category::Music
            } else {
                Category::Sfx
            },
            tags: Vec::new(),
            kind: GameAssetKind::sfx(Vec::new()),
        });
        if let Some(n) = &a.rename {
            asset.name = n.trim().to_string();
        }
        if let Some(c) = &a.category {
            asset.category = category(Some(c))?;
        }
        if let Some(t) = a.tags {
            asset.tags = t;
        }
        let range = bar_range(&project, a.start_bar, a.end_bar)?
            .map(|(start, end)| TimeRange { start, end });
        let same_kind = existing
            .as_ref()
            .is_some_and(|e| e.kind.kind_name() == kind);
        asset.kind = if is_loop {
            let (old_range, old_intro) = match &asset.kind {
                GameAssetKind::Loop { range, intro_start } if same_kind => {
                    (Some(*range), *intro_start)
                }
                _ => (None, None),
            };
            let intro_start = match a.intro_start_bar {
                Some(0.0) => None,
                Some(b) => Some(bar_tick(&project, b)?),
                None => old_intro,
            };
            GameAssetKind::Loop {
                range: range.or(old_range).unwrap_or(project.loop_region),
                intro_start,
            }
        } else {
            let old = match &asset.kind {
                k @ GameAssetKind::Sfx { .. } if same_kind => k.clone(),
                _ => GameAssetKind::sfx(Vec::new()),
            };
            let GameAssetKind::Sfx {
                variations: old_variations,
                selection: old_selection,
                pitch_semitones,
                gain_db,
                tail_seconds,
                mono,
            } = old
            else {
                unreachable!("old is an SFX set")
            };
            if let Some(c) = a.count
                && !(1..=MAX_SFX_VARIATIONS).contains(&c)
            {
                return Err(format!("count must be 1 to {MAX_SFX_VARIATIONS}, got {c}"));
            }
            let variations = match (&a.variations, range) {
                (Some(_), Some(_)) => {
                    return Err("give variations, or start_bar, end_bar and count, not both".into());
                }
                (Some(_), None) if a.count.is_some() => {
                    return Err(
                        "count splits start_bar to end_bar; with variations, leave it out".into(),
                    );
                }
                (Some(spans), None) => spans
                    .iter()
                    .map(|v| {
                        bar_range(&project, Some(v.start_bar), Some(v.end_bar))
                            .map(|r| r.map(|(start, end)| TimeRange { start, end }))
                    })
                    .collect::<Result<Option<Vec<_>>, _>>()?
                    .unwrap_or_default(),
                (None, Some(r)) => r.split(a.count.unwrap_or(1)),
                (None, None) => match (a.count, TimeRange::covering(&old_variations)) {
                    (Some(n), Some(span)) => span.split(n),
                    (_, None) => {
                        return Err(
                            "give start_bar and end_bar (and count), or variations, for \
                                    the bars the SFX set is rendered from"
                                .into(),
                        );
                    }
                    (None, _) => old_variations,
                },
            };
            GameAssetKind::Sfx {
                variations,
                selection: match &a.selection {
                    Some(s) => selection(s)?,
                    None => old_selection,
                },
                pitch_semitones: a
                    .pitch_semitones
                    .map_or(pitch_semitones, |r| RandomRange::new(r.min, r.max)),
                gain_db: a
                    .gain_db
                    .map_or(gain_db, |r| RandomRange::new(r.min, r.max)),
                tail_seconds: a.tail_seconds.unwrap_or(tail_seconds),
                mono: a.mono.unwrap_or(mono),
            }
        };
        let id = asset.id;
        let label = if existing.is_some() {
            format!("Edit game asset {}", asset.name)
        } else {
            format!("Add game {kind} {}", asset.name)
        };
        let cmd = if existing.is_some() {
            Command::UpdateGameAsset {
                asset: Box::new(asset),
            }
        } else {
            Command::AddGameAsset {
                asset: Box::new(asset),
                index: None,
            }
        };
        self.apply(&ctx, "game_asset_set", label, vec![cmd])?;
        let project = self.session.project();
        let saved = project.game.asset(id).ok_or("the asset was not saved")?;
        Ok(json!({
            "asset": game_asset_json(&project, saved),
            "next": "game_export writes it for the game",
        })
        .to_string())
    }

    #[tool(
        description = "Removes a game asset from the project. The next full game_export removes its files from the export folder.",
        annotations(destructive_hint = true)
    )]
    async fn game_asset_remove(
        &self,
        Parameters(a): Parameters<GameAssetArg>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let asset = game_asset(&project, &a.name)?;
        let name = asset.name.clone();
        self.apply(
            &ctx,
            "game_asset_remove",
            format!("Remove game asset {name}"),
            vec![Command::RemoveGameAsset { id: asset.id }],
        )?;
        Ok(format!("Removed the game asset {name}."))
    }

    #[tool(
        description = "Sets where game_export writes (such as the game's assets/audio folder; relative paths are relative to the project folder) and the WAV bit depth. Without arguments, shows the current settings."
    )]
    async fn game_export_settings(
        &self,
        Parameters(a): Parameters<GameExportSettingsArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let view = self.session.view();
        let game = &view.project.game;
        if a.dir.is_some() || a.bits.is_some() {
            let export_dir = match a.dir.as_deref().map(str::trim) {
                None => game.export_dir.clone(),
                Some("") => None,
                Some(d) => {
                    let given = Path::new(d);
                    let full = if given.is_absolute() {
                        given.to_path_buf()
                    } else {
                        match &view.path {
                            Some(p) => normalize(&p.join(given)),
                            None => {
                                return Err("a relative folder is relative to the project \
                                            folder; save the project first, or give an \
                                            absolute folder"
                                    .into());
                            }
                        }
                    };
                    self.policy.resolve(&full.to_string_lossy())?;
                    Some(d.to_string())
                }
            };
            let bits = a.bits.unwrap_or(game.bits);
            self.apply(
                &ctx,
                "game_export_settings",
                "Change game export".into(),
                vec![Command::SetGameExport { export_dir, bits }],
            )?;
        }
        let project = self.session.project();
        let mut v = game_json(&project);
        if let Some(assets) = v.as_object_mut() {
            assets.remove("assets");
        }
        v["folder"] = json!(
            self.session
                .game_export_dir()
                .ok()
                .map(|d| normalize(&d).display().to_string())
        );
        Ok(v.to_string())
    }

    #[tool(
        description = "Renders the project's game assets and writes them for the game: one WAV per loop and per SFX variation, and skyware-manifest.json, which sky-runtime loads. Exporting everything from a saved project also removes assets it no longer has and their old files. Then loads the export into the game runtime and checks every asset."
    )]
    async fn game_export(&self, Parameters(a): Parameters<GameExportArgs>) -> ToolResult {
        let dir = match &a.dir {
            Some(d) => self.policy.resolve(d)?,
            None => {
                let d = self.session.game_export_dir().map_err(|e| e.to_string())?;
                self.policy.resolve(&normalize(&d).to_string_lossy())?
            }
        };
        let session = self.session.clone();
        blocking(move || {
            let out = session
                .export_game(&GameExportOptions {
                    dir: Some(dir.clone()),
                    only: a.only.clone().unwrap_or_default(),
                })
                .map_err(|e| e.to_string())?;
            let assets: Vec<Value> = out
                .assets
                .iter()
                .map(|x| {
                    let mut v = json!({
                        "name": x.asset.name(),
                        "files": x.asset.files().map(|f| f.path.clone()).collect::<Vec<_>>(),
                    });
                    if let Some(seam) = &x.seam {
                        v["seam"] = json!(seam);
                    }
                    v
                })
                .collect();
            let mut v = json!({
                "dir": out.dir.display().to_string(),
                "manifest": out.manifest.display().to_string(),
                "assets": assets,
                "removed": out.removed,
                "deletedFiles": out.deleted.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "credits": out.licenses.credits,
                "licenseWarnings": out.licenses.warnings,
            });
            if a.verify != Some(false) {
                let check = sky_check::check_game(&sky_check::GameOptions {
                    dir,
                    assets: a.only.unwrap_or_default(),
                    seconds: 0.0,
                    ..Default::default()
                })?;
                v["verified"] = json!({
                    "passed": check.passed,
                    "checks": check.checks,
                });
            }
            Ok(v.to_string())
        })
        .await
    }

    #[tool(
        description = "Checks this machine and build for game audio work, without touching the open project. Plays a test loop through the default audio device at a buffer size (silently unless audible) and counts dropouts and overruns, and whether the device really used that buffer size, in the output mode asked for (auto by default: exclusive or low-latency shared mode when shared mode cannot run the buffer); then exports a 16-bar test loop and verifies the file's length, manifest, seam and seamless playback in the game runtime. Takes device_seconds plus a few seconds."
    )]
    async fn system_check(&self, Parameters(a): Parameters<SystemCheckArgs>) -> ToolResult {
        let seconds = a.device_seconds.unwrap_or(20.0);
        if !(0.0..=600.0).contains(&seconds) {
            return Err(format!("device_seconds must be 0 to 600, got {seconds}"));
        }
        let bars = a.bars.unwrap_or(16);
        if bars > sky_check::MAX_BARS {
            return Err(format!(
                "bars must be 0 to {}, got {bars}",
                sky_check::MAX_BARS
            ));
        }
        if seconds == 0.0 && bars == 0 {
            return Err("device_seconds and bars are both 0, so there is nothing to check".into());
        }
        let buffer = a.buffer_frames.unwrap_or(128);
        if !(16..=8_192).contains(&buffer) {
            return Err(format!("buffer_frames must be 16 to 8192, got {buffer}"));
        }
        let mode = match a.mode.as_deref() {
            Some(m) => m.parse::<sky_check::OutputMode>()?,
            None => sky_check::OutputMode::Auto,
        };
        let dir = a
            .dir
            .as_deref()
            .map(|d| self.policy.resolve(d))
            .transpose()?;
        let silent = a.audible != Some(true);
        blocking(move || {
            // A session of its own, so the open project and its undo history
            // are left alone.
            let (session, _) = Session::new(sky_check::check_project(bars.max(1)), None);
            let device = (seconds > 0.0).then(|| {
                let opts = sky_check::DeviceOptions {
                    output: sky_check::OutputConfig {
                        buffer_frames: Some(buffer),
                        mode,
                        silent,
                        ..Default::default()
                    },
                    duration: std::time::Duration::from_secs_f64(seconds),
                    ..Default::default()
                };
                sky_check::check_device(&session, &opts, &mut |_| {})
            });
            let loop_export = (bars > 0).then(|| {
                sky_check::check_loop(
                    &session,
                    &sky_check::LoopOptions {
                        dir,
                        ..Default::default()
                    },
                )
            });
            let report = sky_check::Report::new(device, loop_export);
            serde_json::to_string(&report).map_err(|e| e.to_string())
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SkywareMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("skyware-sound", env!("CARGO_PKG_VERSION"))
                    .with_title("Skyware Sound"),
            )
            .with_instructions(INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_folders_resolve_as_the_file_system_does() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        assert_eq!(
            normalize(&project.join("../game/./audio")),
            root.join("game/audio")
        );
        assert_eq!(
            normalize(&project.join("new/../audio")),
            project.join("audio")
        );
        #[cfg(unix)]
        {
            let elsewhere = root.join("elsewhere/deep");
            std::fs::create_dir_all(&elsewhere).unwrap();
            std::os::unix::fs::symlink(&elsewhere, project.join("link")).unwrap();
            assert_eq!(
                normalize(&project.join("link/../audio")),
                root.join("elsewhere/audio")
            );
            assert_eq!(normalize(&project.join("link/..")), root.join("elsewhere"));
        }
    }
}
