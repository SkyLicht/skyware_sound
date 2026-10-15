// SPDX-License-Identifier: GPL-3.0-or-later
//! The loop check: export a loop for the game, then verify what landed on
//! disk the way a game would use it.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sky_core::manifest::{Asset, Category};
use sky_core::{NoteId, PPQ, Ticks};
use sky_dsp::AudioData;
use sky_dsp::analysis::{self, SeamReport};
use sky_io::WavFormat;
use sky_model::templates::demo_project;
use sky_model::{ClipContent, Project, TimeRange};
use sky_runtime::{PlayParams, Runtime};
use sky_session::{LoopExportOptions, Session};

use crate::{Check, TempFolder};

/// Most bars [`check_project`] builds.
pub const MAX_BARS: u32 = 64;

/// The reference loop: the demo project's four bars repeated to `bars` bars
/// (1 to 64), with the loop region over all of them. The pad has a long
/// release and a reverb, so the export only repeats cleanly if the tails
/// from the end of the loop carry into its start.
pub fn check_project(bars: u32) -> Project {
    let mut p = demo_project();
    let bars = bars.clamp(1, MAX_BARS);
    p.name = format!("{bars}-bar check loop");
    let bar = p.ticks_per_bar();
    let end = bar * bars as Ticks;
    for track in &mut p.tracks {
        let mut clips = Vec::new();
        for clip in std::mem::take(&mut track.clips) {
            if let ClipContent::Pattern { .. } | ClipContent::Steps(_) = clip.content {
                // Patterns and steps repeat, so one long clip covers them.
                clips.push(sky_model::Clip {
                    length: end - clip.start,
                    ..clip
                });
                continue;
            }
            let mut start = clip.start;
            while start < end {
                let mut copy = clip.clone();
                copy.id = sky_core::ClipId::new();
                copy.start = start;
                if let ClipContent::Midi { notes } = &mut copy.content {
                    for n in notes.iter_mut() {
                        n.id = NoteId::new();
                    }
                }
                clips.push(copy);
                start += clip.length.max(bar);
            }
        }
        track.clips = clips;
    }
    p.loop_region = TimeRange { start: 0, end };
    p.loop_enabled = true;
    p
}

/// Options for [`check_loop`].
#[derive(Clone, Debug)]
pub struct LoopOptions {
    /// Export folder. `None` uses a temporary folder that is deleted after
    /// the check.
    pub dir: Option<PathBuf>,
    /// Asset name.
    pub name: String,
    /// Loop range in ticks. `None` uses the project's loop region.
    pub range: Option<(Ticks, Ticks)>,
    pub format: WavFormat,
}

impl Default for LoopOptions {
    fn default() -> Self {
        Self {
            dir: None,
            name: "System check loop".into(),
            range: None,
            format: WavFormat::Pcm24,
        }
    }
}

/// What a loop export should be, worked out from the project rather than
/// from the export, so a conversion mistake in the export shows up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Expected {
    pub bars: f64,
    /// Exact length in samples, before rounding.
    pub exact_frames: f64,
    pub sample_rate: u32,
    pub tempo_bpm: Option<f64>,
}

impl Expected {
    pub fn from_project(project: &Project, start: Ticks, end: Ticks) -> Self {
        let sr = project.sample_rate;
        let tempo = &project.tempo;
        let constant = tempo.points().len() == 1;
        let exact_frames = if constant {
            // Ticks are 960 to the quarter note and the tempo counts
            // quarter notes.
            let quarters = (end - start) as f64 / PPQ as f64;
            quarters * 60.0 / tempo.initial_bpm() * sr as f64
        } else {
            tempo.span_samples_exact(start, end, sr).0
        };
        Self {
            bars: (end - start) as f64 / project.ticks_per_bar() as f64,
            exact_frames,
            sample_rate: sr,
            tempo_bpm: constant.then(|| tempo.initial_bpm()),
        }
    }

    pub fn frames(&self) -> u64 {
        self.exact_frames.round().max(0.0) as u64
    }

    pub fn seconds(&self) -> f64 {
        self.exact_frames / self.sample_rate.max(1) as f64
    }
}

/// The result of [`check_loop`] or [`verify_loop_export`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopCheck {
    pub passed: bool,
    pub asset: String,
    pub bars: f64,
    pub seconds: f64,
    pub frames: u64,
    pub sample_rate: u32,
    pub file: PathBuf,
    pub manifest: PathBuf,
    /// False when the export went to a temporary folder that is gone now.
    pub kept: bool,
    /// The seam of the file as read back from disk.
    pub seam: SeamReport,
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbfs: Option<f32>,
    pub checks: Vec<Check>,
}

/// Exports the session's loop (or `opts.range`) for the game and verifies
/// the export.
pub fn check_loop(session: &Session, opts: &LoopOptions) -> Result<LoopCheck, String> {
    let temp = match &opts.dir {
        Some(_) => None,
        None => Some(TempFolder::new("loop")?),
    };
    let dir = match (&opts.dir, &temp) {
        (Some(d), _) => d.clone(),
        (None, Some(t)) => t.path().to_path_buf(),
        (None, None) => unreachable!("a temporary folder exists when no folder is given"),
    };
    let out = session
        .export_loop(&LoopExportOptions {
            name: Some(opts.name.clone()),
            dir: Some(dir.clone()),
            range: opts.range,
            category: Category::Music,
            tags: vec!["system-check".into()],
            format: opts.format,
        })
        .map_err(|e| format!("the loop export failed: {e}"))?;
    let project = session.project();
    let (start, end) = opts
        .range
        .unwrap_or((project.loop_region.start, project.loop_region.end));
    let expected = Expected::from_project(&project, start, end);
    let mut check = verify_loop_export(&dir, out.exported.asset.name(), &expected)?;
    check.kept = temp.is_none();
    Ok(check)
}

/// One second of the loop, or all of it if it is shorter.
fn window(sample_rate: u32, frames: usize) -> usize {
    (sample_rate as usize).min(frames)
}

/// Verifies a loop export in `dir` as the game will see it: the manifest
/// entry, the length of the file, its seam once written to disk, and what
/// `sky-runtime` plays across the loop point.
pub fn verify_loop_export(
    dir: &Path,
    asset_name: &str,
    expected: &Expected,
) -> Result<LoopCheck, String> {
    let manifest_path = sky_game::manifest_path(dir);
    let manifest = sky_game::read_manifest(dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{} was not written", manifest_path.display()))?;
    let Some(Asset::Loop(asset)) = manifest.find(asset_name) else {
        return Err(format!(
            "the manifest has no loop named {asset_name:?}; it has: {}",
            manifest
                .assets
                .iter()
                .map(Asset::name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let file = dir.join(&asset.file.path);
    let audio = sky_io::read_audio(&file).map_err(|e| e.to_string())?;
    let frames = audio.frames() as u64;
    let mut checks = Vec::new();

    // Length: exactly the bars asked for, to the sample.
    let want = expected.frames();
    let remainder = (expected.exact_frames - expected.exact_frames.round()).abs();
    let mut detail = format!(
        "{frames} frames; {} bars{} at {} Hz is {:.3} frames ({:.3} s)",
        trim(expected.bars),
        expected
            .tempo_bpm
            .map(|b| format!(" at {} BPM", trim(b)))
            .unwrap_or_default(),
        expected.sample_rate,
        expected.exact_frames,
        expected.seconds()
    );
    if remainder > 1e-6 {
        detail.push_str(
            "; the loop is not a whole number of samples long, so it drifts by a \
             fraction of a sample per repeat",
        );
    }
    checks.push(Check::new(
        "length",
        frames == want && audio.sample_rate == expected.sample_rate,
        detail,
    ));

    // Manifest: loop points cover the file, and the musical facts match.
    let mut problems = Vec::new();
    if asset.loop_start != 0 || asset.loop_end != frames {
        problems.push(format!(
            "loop points {}..{} should be 0..{frames}",
            asset.loop_start, asset.loop_end
        ));
    }
    if asset.file.length_samples != frames {
        problems.push(format!(
            "it says the file has {} samples, the file has {frames}",
            asset.file.length_samples
        ));
    }
    if asset.file.sample_rate != audio.sample_rate
        || asset.file.channels as usize != audio.channel_count()
    {
        problems.push(format!(
            "it says {} channel(s) at {} Hz, the file has {} at {} Hz",
            asset.file.channels,
            asset.file.sample_rate,
            audio.channel_count(),
            audio.sample_rate
        ));
    }
    if asset.bars.is_none_or(|b| (b - expected.bars).abs() > 1e-9) {
        problems.push(format!(
            "bars is {:?}, should be {}",
            asset.bars,
            trim(expected.bars)
        ));
    }
    if expected.tempo_bpm.is_some() && asset.tempo_bpm != expected.tempo_bpm {
        problems.push(format!(
            "tempo is {:?}, should be {:?}",
            asset.tempo_bpm, expected.tempo_bpm
        ));
    }
    checks.push(Check::new(
        "manifest",
        problems.is_empty(),
        if problems.is_empty() {
            format!(
                "\"{}\" loops 0..{frames} in {}",
                asset.name, asset.file.path
            )
        } else {
            problems.join("; ")
        },
    ));

    // Seam: the file as written (quantized and dithered) still meets itself.
    let seam = analysis::seam_check(&audio, 0, audio.frames());
    checks.push(Check::new(
        "seam",
        seam.verdict == "seamless",
        format!(
            "{}: the jump from the last sample to the first is {}, the largest \
             step next to it {}; level {} dB before, {} dB after{}",
            seam.verdict,
            seam.sample_jump,
            seam.typical_step,
            seam.level_before_db,
            seam.level_after_db,
            seam.note.map(|n| format!(" ({n})")).unwrap_or_default()
        ),
    ));

    // Clipping would be baked into the game asset.
    let stats = analysis::analyze(&audio);
    checks.push(Check::new(
        "clipping",
        stats.clipped_samples == 0,
        format!(
            "{} clipped sample(s); peak {} dBFS, true peak {}",
            stats.clipped_samples,
            stats.peak_dbfs,
            stats
                .true_peak_dbfs
                .map_or("unknown".to_string(), |t| format!("{t} dBFS"))
        ),
    ));

    checks.push(runtime_check(dir, &asset.name, &audio));

    Ok(LoopCheck {
        passed: checks.iter().all(|c| c.passed),
        asset: asset.name.clone(),
        bars: expected.bars,
        seconds: frames as f64 / audio.sample_rate.max(1) as f64,
        frames,
        sample_rate: audio.sample_rate,
        file,
        manifest: manifest_path,
        kept: true,
        seam,
        integrated_lufs: stats.integrated_lufs,
        true_peak_dbfs: stats.true_peak_dbfs,
        checks,
    })
}

/// Plays the export in the game runtime past the loop point and compares
/// the output with the file repeated: at the file's own rate, unity gain
/// and center pan the runtime should reproduce it sample for sample, so any
/// gap, overlap or click at the wrap shows up as a difference.
fn runtime_check(dir: &Path, name: &str, file: &AudioData) -> Check {
    let fail = |detail: String| Check::new("runtime", false, detail);
    let frames = file.frames();
    if frames < 2 {
        return fail("the file is empty".into());
    }
    let sr = file.sample_rate;
    let mut rt = Runtime::new(sr);
    if let Err(e) = rt.load_manifest(dir) {
        return fail(format!("sky-runtime cannot load the manifest: {e}"));
    }
    let voice = match rt.play_by_name(name, PlayParams::default()) {
        Ok(v) => v,
        Err(e) => return fail(format!("sky-runtime cannot play it: {e}")),
    };
    // Through the first repeat and one more second.
    let total = frames + window(sr, frames);
    let mut out = vec![0.0f32; total * 2];
    for chunk in out.chunks_mut(1024) {
        rt.render(chunk);
    }
    if !rt.is_playing(voice) {
        return fail("the voice stopped instead of looping".into());
    }
    let (left, right) = (file.left(), file.right());
    let mut worst = 0.0f32;
    let mut worst_at = 0usize;
    for (i, frame) in out.as_chunks::<2>().0.iter().enumerate() {
        let j = i % frames;
        let d = (frame[0] - left[j]).abs().max((frame[1] - right[j]).abs());
        if d > worst {
            worst = d;
            worst_at = i;
        }
    }
    // The runtime mixes in f32 at unity gain; anything above rounding noise
    // is a real difference.
    let passed = worst <= 1e-5;
    let seconds = total as f64 / sr as f64;
    Check::new(
        "runtime",
        passed,
        if passed {
            format!(
                "sky-runtime played {seconds:.2} s, through the loop point at {:.2} s, \
                 identical to the file repeated (largest difference {worst:e})",
                frames as f64 / sr as f64
            )
        } else {
            format!(
                "sky-runtime output differs from the file repeated by {worst} at {:.4} s \
                 (the loop point is at {:.4} s)",
                worst_at as f64 / sr as f64,
                frames as f64 / sr as f64
            )
        },
    )
}

/// Formats a number without a trailing ".0".
fn trim(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{x:.0}")
    } else {
        format!("{x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_project_repeats_the_demo() {
        let p = check_project(16);
        let bar = p.ticks_per_bar();
        assert_eq!(p.loop_region.end, bar * 16);
        for t in &p.tracks {
            let end = t.clips.iter().map(|c| c.end()).max().unwrap();
            assert!(end >= bar * 16, "{} ends at bar {}", t.name, end / bar);
        }
        let pad = p.tracks.iter().find(|t| t.name == "Pad").unwrap();
        assert_eq!(pad.clips.len(), 4);
        let ids: std::collections::HashSet<_> = pad.clips.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), 4);
        // Six bars: the last copy runs past the loop, which is fine.
        let p = check_project(6);
        assert_eq!(p.loop_region.end, p.ticks_per_bar() * 6);
    }

    #[test]
    fn expected_length_comes_from_the_tempo() {
        let p = check_project(16);
        let e = Expected::from_project(&p, 0, p.loop_region.end);
        // 16 bars of 4/4 at 120 BPM is 32 seconds.
        assert_eq!(e.frames(), 32 * 48_000);
        assert_eq!(e.bars, 16.0);
        assert_eq!(e.tempo_bpm, Some(120.0));
    }
}
