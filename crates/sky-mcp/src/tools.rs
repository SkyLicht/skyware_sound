// SPDX-License-Identifier: GPL-3.0-or-later
//! The MCP tools. Every tool that changes the project applies one
//! transaction, so it is one undo step and shows in the AI activity panel.
//!
//! Positions are in bars (bar 1 is the start), lengths in bars, levels in
//! dB. Tracks, clips and effects can be named by ID or by name.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use sky_core::manifest::Category;
use sky_core::{AudioId, ClipId, EffectId, Ticks, TrackId};
use sky_dsp::analysis;
use sky_dsp::params::{self, ParamKind, ParamSpec};
use sky_dsp::{EffectType, InstrumentType};
use sky_io::WavFormat;
use sky_model::notes::{format_note_list, parse_note_list};
use sky_model::summary::{clip_json, project_summary, track_json};
use sky_model::templates::{demo_project, empty_project};
use sky_model::{
    ChannelRef, Clip, ClipContent, Command, EffectSlot, InstrumentSlot, Origin, Project, Track,
    TrackKind, Transaction,
};
use sky_session::{LoopExportOptions, Session};

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
For a game loop: set_loop to the bars, loop_check_seam to confirm it repeats \
cleanly, then export_loop writes the WAV and the manifest the game reads.";

#[derive(Clone)]
pub struct SkywareMcp {
    session: Session,
    policy: Arc<Policy>,
    tool_router: ToolRouter<Self>,
}

/// The permission each tool needs.
fn required(tool: &str) -> Permission {
    match tool {
        "project_summary" | "track_get" | "clip_get" | "catalog" | "history" | "pattern_query"
        | "analyze" | "loop_check_seam" => Permission::ReadOnly,
        "project_open" | "project_save" | "audio_import" | "export_wav" | "export_loop"
        | "system_check" => Permission::Files,
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
    Ok(
        match name.unwrap_or("music").to_ascii_lowercase().as_str() {
            "music" => Category::Music,
            "sfx" => Category::Sfx,
            "ui" => Category::Ui,
            "voice" => Category::Voice,
            "ambience" => Category::Ambience,
            other => {
                return Err(format!(
                    "category {other:?} is not one of music, sfx, ui, voice, ambience"
                ));
            }
        },
    )
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
    /// Mini-notation pattern text.
    pub pattern: String,
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
    /// For instrument tracks: synth (default), drums or sampler.
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstrumentSetArgs {
    /// Track ID or name.
    pub track: String,
    /// Replace the instrument: synth, drums or sampler. Parameters reset.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Parameters to set; null restores a default.
    pub params: Option<BTreeMap<String, Value>>,
    /// For a sampler: the imported audio it plays (ID or name).
    pub sample: Option<String>,
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
    /// For a pattern clip: mini-notation text.
    pub pattern: Option<String>,
    /// Pattern cycle length in bars. Default 1.
    pub cycle_bars: Option<f64>,
    /// Pattern random seed. Default 0.
    pub seed: Option<u64>,
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
pub struct PatternSetArgs {
    /// Pattern clip ID or name.
    pub clip: String,
    pub pattern: String,
    /// Cycle length in bars. Default: unchanged.
    pub cycle_bars: Option<f64>,
    pub seed: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndoArgs {
    /// How many steps, 1 to 50. Default 1.
    pub steps: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectNewArgs {
    pub name: Option<String>,
    /// empty (default) or demo.
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
    /// Let the device check be heard. Default false: the engine runs as
    /// usual but the device gets silence.
    pub audible: Option<bool>,
    /// Bars in the test loop, 1 to 64; 0 skips the loop check. Default 16.
    pub bars: Option<u32>,
    /// Folder to keep the exported test loop in. Default: a temporary
    /// folder that is deleted afterwards.
    pub dir: Option<String>,
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
        description = "Parses a mini-notation pattern and lists the events it plays, without changing the project. Times are in cycles as fractions.",
        annotations(read_only_hint = true)
    )]
    async fn pattern_query(&self, Parameters(a): Parameters<PatternQueryArgs>) -> ToolResult {
        let pattern = sky_pattern::parse(&a.pattern).map_err(|e| format!("pattern error: {e}"))?;
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
                let (sound, index) = match &e.value {
                    sky_pattern::Value::Sound { name, index } => (Some(name.clone()), *index),
                    sky_pattern::Value::Note(_) => (None, 0),
                };
                json!({
                    "begin": e.begin.to_string(),
                    "end": e.end.to_string(),
                    "sound": sound,
                    "index": index,
                    "midi": e.value.midi_note(),
                })
            })
            .collect();
        let silent = events
            .iter()
            .filter(|e| e.value.midi_note().is_none())
            .count();
        let mut v = json!({ "cycles": cycles, "eventCount": events.len(), "events": list });
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
        description = "Adds a track: an instrument track with a synth, drum kit or sampler, or an audio track. Returns its ID."
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
                    format!("instrument {name:?} is not one of synth, drums, sampler")
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
        description = "Changes a track's name, color, position, or its mixer volume, pan, mute and solo. Use track \"master\" for the master volume."
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
                if a.name.is_some() || a.color.is_some() || a.index.is_some() {
                    return Err("the master has no name, color or position".into());
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
                    format!("instrument {name:?} is not one of synth, drums, sampler")
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
        Ok(json!({ "effect": id.to_string(), "type": kind.name() }).to_string())
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
        if cmds.is_empty() {
            return Err("nothing to change: give params or enabled".into());
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
        Ok(json!(slot.map(|s| json!({ "effect": s.id.to_string(), "type": s.kind.name(), "enabled": s.enabled, "params": s.params }))).to_string())
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
        description = "Adds a clip: a MIDI clip (notes in the compact list), a pattern clip (mini-notation) on an instrument track, or an audio clip on an audio track. Returns the clip."
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
            let cycle = bars_len(&project, a.cycle_bars.unwrap_or(1.0))?;
            (
                ClipContent::Pattern {
                    source: p.clone(),
                    seed: a.seed.unwrap_or(0),
                    cycle,
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
        description = "Replaces a pattern clip's mini-notation, and optionally its cycle length or seed."
    )]
    async fn pattern_set(
        &self,
        Parameters(a): Parameters<PatternSetArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolResult {
        let project = self.session.project();
        let id = find_clip(&project, &a.clip)?;
        let (_, clip) = project.find_clip(id).map_err(|e| e.to_string())?;
        let ClipContent::Pattern { seed, cycle, .. } = &clip.content else {
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
        self.apply(
            &ctx,
            "pattern_set",
            format!("Change pattern {}", clip.name),
            vec![Command::SetPattern {
                clip: id,
                source: a.pattern.clone(),
                seed: a.seed.unwrap_or(*seed),
                cycle,
            }],
        )?;
        Ok(format!("Pattern is now {:?}.", a.pattern))
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
        description = "Starts a new project, empty or from the demo template. The open project must be saved first unless discard_unsaved is true.",
        annotations(destructive_hint = true)
    )]
    async fn project_new(&self, Parameters(a): Parameters<ProjectNewArgs>) -> ToolResult {
        self.check_unsaved(a.discard_unsaved)?;
        let name = a.name.unwrap_or_else(|| "Untitled".into());
        let mut project = match a.template.as_deref().unwrap_or("empty") {
            "empty" => empty_project(&name),
            "demo" => demo_project(),
            other => return Err(format!("template {other:?} is not empty or demo")),
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
            Ok(json!({
                "file": path.display().to_string(),
                "seconds": round(audio.duration_seconds(), 3),
                "integratedLufs": stats.integrated_lufs,
                "truePeakDbfs": stats.true_peak_dbfs,
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
            })
            .to_string())
        })
        .await
    }

    #[tool(
        description = "Checks this machine and build for game audio work, without touching the open project. Plays a test loop through the default audio device at a buffer size (silently unless audible) and counts dropouts and overruns, and whether the device really used that buffer size; then exports a 16-bar test loop and verifies the file's length, manifest, seam and seamless playback in the game runtime. Takes device_seconds plus a few seconds."
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
