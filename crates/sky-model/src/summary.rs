// SPDX-License-Identifier: GPL-3.0-or-later
//! A compact, readable view of a project for AI clients: positions in bars,
//! levels in dB, only parameters that differ from their defaults.

use serde_json::{Value, json};

use crate::notes::format_note_list;
use crate::picture::Picture;
use crate::project::*;
use crate::steps::{DEFAULT_GATE, StepClip, format_locks};

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

fn channel_json(ch: &Channel) -> Value {
    let effects: Vec<Value> = ch
        .effects
        .iter()
        .map(|e| {
            let mut v = json!({
                "id": e.id.to_string(),
                "type": e.kind.name(),
                "enabled": e.enabled,
                "params": e.params,
            });
            if let Some(source) = e.sidechain {
                v["sidechain"] = json!(source.to_string());
            }
            v
        })
        .collect();
    json!({
        "volumeDb": round(ch.volume_db as f64, 2),
        "pan": round(ch.pan as f64, 2),
        "mute": ch.mute,
        "solo": ch.solo,
        "effects": effects,
    })
}

/// Describes one clip; `detail` includes the notes or pattern text.
pub fn clip_json(project: &Project, clip: &Clip, detail: bool) -> Value {
    let bar = project.ticks_per_bar() as f64;
    let mut v = json!({
        "id": clip.id.to_string(),
        "name": clip.name,
        "type": clip.kind_name(),
        "startBar": round(project.ticks_to_bar(clip.start), 4),
        "lengthBars": round(clip.length as f64 / bar, 4),
    });
    if clip.muted {
        v["muted"] = json!(true);
    }
    match &clip.content {
        ClipContent::Audio {
            source,
            gain_db,
            fade_in,
            fade_out,
            offset,
        } => {
            v["source"] = json!(source.to_string());
            v["gainDb"] = json!(gain_db);
            if detail {
                v["fadeInTicks"] = json!(fade_in);
                v["fadeOutTicks"] = json!(fade_out);
                v["offsetSamples"] = json!(offset);
            }
        }
        ClipContent::Midi { notes } => {
            v["noteCount"] = json!(notes.len());
            if detail {
                v["notes"] = json!(format_note_list(notes));
            }
        }
        ClipContent::Pattern {
            source,
            seed,
            cycle,
            controls,
            swing,
        } => {
            v["pattern"] = json!(source);
            v["seed"] = json!(seed);
            v["cycleBars"] = json!(round(*cycle as f64 / bar, 4));
            if !controls.is_empty() {
                v["controls"] = json!(controls);
            }
            if !swing.is_straight() {
                v["swing"] = json!({ "amount": swing.amount, "grid": swing.grid });
            }
        }
        ClipContent::Steps(steps) => {
            v["rate"] = json!(steps.rate);
            v["laneCount"] = json!(steps.lanes.len());
            if detail {
                v["lanes"] = steps_json(steps);
                v["seed"] = json!(steps.seed);
            }
            if !steps.swing.is_straight() {
                v["swing"] = json!({ "amount": steps.swing.amount, "grid": steps.swing.grid });
            }
        }
    }
    v
}

/// A step clip's lanes in the text form.
pub fn steps_json(steps: &StepClip) -> Value {
    steps
        .lanes
        .iter()
        .map(|lane| {
            let mut v = json!({
                "note": sky_core::music::note_name(lane.note),
                "steps": lane.text(),
            });
            if lane.gate != DEFAULT_GATE {
                v["gate"] = json!(lane.gate);
            }
            if lane.mute {
                v["mute"] = json!(true);
            }
            let locks = format_locks(&lane.steps);
            if !locks.is_empty() {
                v["locks"] = json!(locks);
            }
            v
        })
        .collect()
}

pub fn track_json(project: &Project, track: &Track, detail: bool) -> Value {
    let mut v = json!({
        "id": track.id.to_string(),
        "name": track.name,
        "kind": match track.kind { TrackKind::Audio => "audio", TrackKind::Instrument => "instrument" },
        "channel": channel_json(&track.channel),
        "clips": track.clips.iter().map(|c| clip_json(project, c, detail)).collect::<Vec<_>>(),
    });
    match &track.picture {
        Some(Picture::Symbol { symbol, .. }) => v["picture"] = json!(symbol.label()),
        Some(Picture::Image(image)) => v["picture"] = json!(format!("image {}", image.name).trim()),
        None => {}
    }
    if let Some(inst) = &track.instrument {
        v["instrument"] = json!({
            "type": inst.kind.name(),
            "params": inst.params,
            "sample": inst.sample.map(|s| s.to_string()),
        });
        if let Some(bank) = &inst.bank {
            v["instrument"]["bank"] = bank_json(project, bank, detail);
        }
    }
    v
}

/// Sound names listed in a summary before the rest are only counted.
const SUMMARY_SOUNDS: usize = 48;

/// A bank's sounds with their variation counts, and the packs they came from.
pub fn bank_json(project: &Project, bank: &SampleBank, detail: bool) -> Value {
    let limit = if detail { usize::MAX } else { SUMMARY_SOUNDS };
    let sounds: serde_json::Map<String, Value> = bank
        .sounds
        .iter()
        .take(limit)
        .map(|(name, samples)| (name.clone(), json!(samples.len())))
        .collect();
    let mut packs: Vec<Value> = Vec::new();
    for id in bank.audio_ids() {
        let Some(src) = project.pool_entry(id).ok().and_then(|e| e.source.as_ref()) else {
            continue;
        };
        let pack = json!({ "pack": src.pack, "license": src.license.id() });
        if !packs.contains(&pack) {
            packs.push(pack);
        }
    }
    let mut v = json!({
        "name": bank.name,
        "sounds": sounds,
        "samples": bank.sample_count(),
        "packs": packs,
    });
    if bank.sounds.len() > limit {
        v["moreSounds"] = json!(bank.sounds.len() - limit);
    }
    if let Some(p) = &bank.pitched {
        v["pitched"] = json!(p);
    }
    v
}

fn bar_span(project: &Project, range: &TimeRange) -> Value {
    json!({
        "startBar": round(project.ticks_to_bar(range.start), 4),
        "endBar": round(project.ticks_to_bar(range.end), 4),
    })
}

/// One game asset, with positions in bars.
pub fn game_asset_json(project: &Project, asset: &GameAsset) -> Value {
    let mut v = json!({
        "id": asset.id.to_string(),
        "name": asset.name,
        "kind": asset.kind.kind_name(),
        "category": asset.category.name(),
    });
    if !asset.tags.is_empty() {
        v["tags"] = json!(asset.tags);
    }
    match &asset.kind {
        GameAssetKind::Loop { range, intro_start } => {
            v["loop"] = bar_span(project, range);
            if let Some(intro) = intro_start {
                v["introStartBar"] = json!(round(project.ticks_to_bar(*intro), 4));
            }
        }
        GameAssetKind::Sfx {
            variations,
            selection,
            pitch_semitones,
            gain_db,
            tail_seconds,
            mono,
        } => {
            v["variations"] = json!(
                variations
                    .iter()
                    .map(|r| bar_span(project, r))
                    .collect::<Vec<_>>()
            );
            v["selection"] = json!(selection);
            v["pitchSemitones"] = json!([pitch_semitones.min, pitch_semitones.max]);
            v["gainDb"] = json!([gain_db.min, gain_db.max]);
            v["tailSeconds"] = json!(tail_seconds);
            v["mono"] = json!(mono);
        }
    }
    v
}

/// The project's game export settings and assets.
pub fn game_json(project: &Project) -> Value {
    json!({
        "exportDir": project.game.export_dir,
        "bits": project.game.bits,
        "assets": project
            .game
            .assets
            .iter()
            .map(|a| game_asset_json(project, a))
            .collect::<Vec<_>>(),
    })
}

/// The whole project in compact form.
pub fn project_summary(project: &Project) -> Value {
    let ts = project.time_signature;
    let mut v = json!({
        "name": project.name,
        "tempoBpm": project.tempo.initial_bpm(),
        "timeSignature": format!("{}/{}", ts.numerator, ts.denominator),
        "sampleRate": project.sample_rate,
        "loop": {
            "enabled": project.loop_enabled,
            "startBar": round(project.ticks_to_bar(project.loop_region.start), 4),
            "endBar": round(project.ticks_to_bar(project.loop_region.end), 4),
        },
        "lengthBars": round(project.content_end() as f64 / project.ticks_per_bar() as f64, 2),
        "tracks": project.tracks.iter().map(|t| track_json(project, t, false)).collect::<Vec<_>>(),
        "master": channel_json(&project.master),
        "audioPool": project.audio_pool.iter().map(|e| json!({
            "id": e.id.to_string(),
            "name": e.name,
            "channels": e.channels,
            "seconds": round(e.frames as f64 / e.sample_rate.max(1) as f64, 3),
            "pack": e.source.as_ref().map(|s| s.pack.clone()),
            "license": e.source.as_ref().map(|s| s.license.id()),
        })).collect::<Vec<_>>(),
        "game": game_json(project),
    });
    if let Some(scale) = project.scale {
        v["scale"] = json!(scale.label());
    }
    let changes = project.tempo.points().get(1..).unwrap_or_default();
    if !changes.is_empty() {
        v["tempoChanges"] = json!(
            changes
                .iter()
                .map(|p| json!({ "bar": round(project.ticks_to_bar(p.tick), 4), "bpm": p.bpm }))
                .collect::<Vec<_>>()
        );
    }
    if !project.markers.is_empty() {
        v["markers"] = json!(
            project
                .markers
                .iter()
                .map(|m| json!({ "bar": round(project.ticks_to_bar(m.tick), 4), "name": m.name }))
                .collect::<Vec<_>>()
        );
    }
    v
}
