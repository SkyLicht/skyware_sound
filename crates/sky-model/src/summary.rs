// SPDX-License-Identifier: GPL-3.0-or-later
//! A compact, readable view of a project for AI clients: positions in bars,
//! levels in dB, only parameters that differ from their defaults.

use serde_json::{Value, json};

use crate::notes::format_note_list;
use crate::project::*;

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

fn channel_json(ch: &Channel) -> Value {
    let effects: Vec<Value> = ch
        .effects
        .iter()
        .map(|e| {
            json!({
                "id": e.id.to_string(),
                "type": e.kind.name(),
                "enabled": e.enabled,
                "params": e.params,
            })
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
        } => {
            v["pattern"] = json!(source);
            v["seed"] = json!(seed);
            v["cycleBars"] = json!(round(*cycle as f64 / bar, 4));
        }
    }
    v
}

pub fn track_json(project: &Project, track: &Track, detail: bool) -> Value {
    let mut v = json!({
        "id": track.id.to_string(),
        "name": track.name,
        "kind": match track.kind { TrackKind::Audio => "audio", TrackKind::Instrument => "instrument" },
        "channel": channel_json(&track.channel),
        "clips": track.clips.iter().map(|c| clip_json(project, c, detail)).collect::<Vec<_>>(),
    });
    if let Some(inst) = &track.instrument {
        v["instrument"] = json!({
            "type": inst.kind.name(),
            "params": inst.params,
            "sample": inst.sample.map(|s| s.to_string()),
        });
    }
    v
}

/// The whole project in compact form.
pub fn project_summary(project: &Project) -> Value {
    let ts = project.time_signature;
    json!({
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
        })).collect::<Vec<_>>(),
    })
}
