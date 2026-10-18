// SPDX-License-Identifier: GPL-3.0-or-later
//! Editing what is selected: cut, copy, paste, duplicate, delete, nudge,
//! transpose and select all, for clips in the arrangement and for notes in
//! the piano roll. Each edit is one undo step.

use egui::Context;
use serde::{Deserialize, Serialize};
use sky_core::{ClipId, NoteId, PPQ, Ticks, TrackId};
use sky_model::{Clip, ClipContent, Command, Note, Project, Track, Transaction};

use crate::actions;
use crate::app::{App, Focus};
use crate::arrange::{clip_fits, snap_floor, snap_round};
use crate::audition;

/// What Copy took. Paste uses it, and it also goes on the system clipboard
/// as JSON, so another Skyware Sound window can paste it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "skywareClipboard", rename_all = "camelCase")]
pub enum Copied {
    #[serde(rename_all = "camelCase")]
    Clips {
        /// The earliest start; the clips' starts count from here.
        from: Ticks,
        /// From the earliest start to the latest end.
        span: Ticks,
        clips: Vec<CopiedClip>,
    },
    #[serde(rename_all = "camelCase")]
    Notes {
        /// The earliest start in the clip; the notes' starts count from here.
        from: Ticks,
        span: Ticks,
        notes: Vec<Note>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopiedClip {
    /// Tracks below the track of the topmost copied clip.
    pub row: usize,
    /// The track it was copied from.
    pub track: TrackId,
    pub clip: Clip,
}

impl Copied {
    /// Reads what another window put on the system clipboard.
    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str(text.trim()).ok()
    }

    fn describe(&self) -> String {
        match self {
            Copied::Clips { clips, .. } => amount(clips.len(), "clip"),
            Copied::Notes { notes, .. } => amount(notes.len(), "note"),
        }
    }
}

#[derive(Default)]
pub struct EditState {
    pub clipboard: Option<Copied>,
    /// Text the system pasted with Ctrl+V, for the Paste it starts.
    pub pasted: Option<String>,
    last_paste: Option<LastPaste>,
}

/// Where the last paste went, so pasting again without moving the playhead
/// lays the next copy after it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LastPaste {
    playhead: Ticks,
    /// The clip notes went into; `None` for clips.
    clip: Option<ClipId>,
    next: Ticks,
}

/// What the edit commands act on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Clips,
    /// The selected notes of this MIDI clip, open in the piano roll.
    Notes(ClipId),
}

/// "3 clips" or "1 clip".
fn amount(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// An undo label such as "Delete clip" or "Delete 3 clips".
fn label(verb: &str, n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{verb} {noun}")
    } else {
        format!("{verb} {n} {noun}s")
    }
}

fn midi_notes(project: &Project, clip: ClipId) -> Option<(&Track, &Clip, &[Note])> {
    let (track, c) = project.find_clip(clip).ok()?;
    match &c.content {
        ClipContent::Midi { notes, .. } => Some((track, c, notes)),
        _ => None,
    }
}

/// The selected clips, each with its track's row.
fn chosen_clips<'a>(project: &'a Project, ids: &[ClipId]) -> Vec<(usize, &'a Clip)> {
    project
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(row, t)| {
            t.clips
                .iter()
                .filter(|c| ids.contains(&c.id))
                .map(move |c| (row, c))
        })
        .collect()
}

fn chosen_notes(notes: &[Note], ids: &[NoteId]) -> Vec<Note> {
    notes
        .iter()
        .filter(|n| ids.contains(&n.id))
        .copied()
        .collect()
}

/// The notes moved to the nearest grid line, keeping those that start
/// inside a clip `length` long there. With `ends`, each note's end goes to
/// its nearest grid line too, at least one step past its start. Only the
/// notes that change are returned.
pub fn quantized(notes: &[Note], step: Ticks, length: Ticks, ends: bool) -> Vec<Note> {
    let step = step.max(1);
    notes
        .iter()
        .filter_map(|n| {
            let mut start = snap_round(n.start as f64, Some(step));
            if start >= length {
                start = snap_floor(n.start as f64, Some(step));
            }
            let end = if ends {
                let end = snap_round((n.start + n.length) as f64, Some(step));
                end.max(start + step).min(length.max(n.start + n.length))
            } else {
                start + n.length
            };
            let moved = Note {
                start,
                length: (end - start).max(1),
                ..*n
            };
            (moved != *n).then_some(moved)
        })
        .collect()
}

/// The earliest start, and the distance from it to the latest end.
fn extent(items: impl Iterator<Item = (Ticks, Ticks)>) -> (Ticks, Ticks) {
    let (from, end) = items.fold((Ticks::MAX, Ticks::MIN), |(lo, hi), (start, len)| {
        (lo.min(start), hi.max(start + len))
    });
    if from > end {
        (0, 0)
    } else {
        (from, end - from)
    }
}

/// A copy of a clip with new IDs, its notes' too.
fn fresh(clip: &Clip, start: Ticks) -> Clip {
    let mut copy = clip.clone();
    copy.id = ClipId::new();
    copy.start = start;
    if let ClipContent::Midi { notes, .. } = &mut copy.content {
        for n in notes.iter_mut() {
            n.id = NoteId::new();
        }
    }
    copy
}

/// The track each copied clip goes on. The selected track takes the
/// topmost one and the rest follow below it; when they do not fit there,
/// they go back to the tracks they came from.
fn paste_tracks(
    project: &Project,
    clips: &[&CopiedClip],
    selected: Option<TrackId>,
) -> Option<Vec<TrackId>> {
    let base = selected.and_then(|id| project.tracks.iter().position(|t| t.id == id));
    if let Some(base) = base {
        let below: Option<Vec<TrackId>> = clips
            .iter()
            .map(|c| {
                let t = project.tracks.get(base + c.row)?;
                clip_fits(t, &c.clip).then_some(t.id)
            })
            .collect();
        if below.is_some() {
            return below;
        }
    }
    clips
        .iter()
        .map(|c| {
            let t = project.track(c.track).ok()?;
            clip_fits(t, &c.clip).then_some(t.id)
        })
        .collect()
}

impl App {
    fn target(&self) -> Target {
        match self.sel.clip {
            Some(clip)
                if self.edit_focus() == Focus::Editor
                    && midi_notes(&self.view.project, clip).is_some() =>
            {
                Target::Notes(clip)
            }
            _ => Target::Clips,
        }
    }

    /// The playhead, in whole ticks.
    fn playhead(&self) -> Ticks {
        self.transport.position_ticks.max(0.0) as Ticks
    }

    /// Whether Cut, Copy, Duplicate and Delete have something to act on.
    pub fn has_selection(&self) -> bool {
        match self.target() {
            Target::Notes(_) => !self.sel.notes.is_empty(),
            Target::Clips => !self.sel.clips.is_empty(),
        }
    }

    /// Copies the selection; false when nothing is selected.
    pub fn copy(&mut self, ctx: &Context) -> bool {
        let view = self.view.clone();
        let project = &view.project;
        let copied = match self.target() {
            Target::Notes(clip) => {
                let notes = midi_notes(project, clip).map_or(&[][..], |(_, _, n)| n);
                let chosen = chosen_notes(notes, &self.sel.notes);
                if chosen.is_empty() {
                    self.info("Select notes to copy first");
                    return false;
                }
                let (from, span) = extent(chosen.iter().map(|n| (n.start, n.length)));
                Copied::Notes {
                    from,
                    span,
                    notes: chosen
                        .into_iter()
                        .map(|n| Note {
                            start: n.start - from,
                            ..n
                        })
                        .collect(),
                }
            }
            Target::Clips => {
                let chosen = chosen_clips(project, &self.sel.clips);
                if chosen.is_empty() {
                    self.info("Select clips to copy first");
                    return false;
                }
                let top = chosen.iter().map(|(row, _)| *row).min().unwrap_or(0);
                let (from, span) = extent(chosen.iter().map(|(_, c)| (c.start, c.length)));
                Copied::Clips {
                    from,
                    span,
                    clips: chosen
                        .iter()
                        .map(|(row, c)| CopiedClip {
                            row: row - top,
                            track: project.tracks[*row].id,
                            clip: Clip {
                                start: c.start - from,
                                ..(*c).clone()
                            },
                        })
                        .collect(),
                }
            }
        };
        if let Ok(json) = serde_json::to_string(&copied) {
            ctx.copy_text(json);
        }
        self.info(format!("Copied {}", copied.describe()));
        self.edit.clipboard = Some(copied);
        self.edit.last_paste = None;
        true
    }

    pub fn cut(&mut self, ctx: &Context) {
        if self.copy(ctx) {
            self.remove_selection("Cut");
        }
    }

    pub fn delete_selection(&mut self) {
        self.remove_selection("Delete");
    }

    fn remove_selection(&mut self, verb: &str) {
        match self.target() {
            Target::Notes(clip) => {
                if self.sel.notes.is_empty() {
                    return;
                }
                let ids = std::mem::take(&mut self.sel.notes);
                let tx = Transaction::user(
                    label(verb, ids.len(), "note"),
                    vec![Command::RemoveNotes { clip, ids }],
                );
                let r = actions::apply(&self.session, tx, None);
                self.report(r);
            }
            Target::Clips => {
                let view = self.view.clone();
                let ids: Vec<ClipId> = chosen_clips(&view.project, &self.sel.clips)
                    .iter()
                    .map(|(_, c)| c.id)
                    .collect();
                if ids.is_empty() {
                    return;
                }
                let tx = Transaction::user(
                    label(verb, ids.len(), "clip"),
                    ids.iter().map(|&id| Command::RemoveClip { id }).collect(),
                );
                let r = actions::apply(&self.session, tx, None);
                if self.report(r).is_some() {
                    self.sel.clips.clear();
                    if self.sel.clip.is_some_and(|c| ids.contains(&c)) {
                        self.sel.clip = None;
                        self.sel.notes.clear();
                    }
                }
            }
        }
    }

    /// Pastes what was copied last, here or in another window, at the
    /// playhead.
    pub fn paste(&mut self) {
        let incoming = self
            .edit
            .pasted
            .take()
            .and_then(|text| Copied::from_json(&text));
        let Some(copied) = incoming.or_else(|| self.edit.clipboard.clone()) else {
            self.info("Nothing to paste yet: copy clips or notes first");
            return;
        };
        match copied {
            Copied::Notes { from, span, notes } => self.paste_notes(from, span, &notes),
            Copied::Clips { span, clips, .. } => self.paste_clips(span, &clips),
        }
    }

    /// Notes go into the clip in the editor at the playhead, or right after
    /// where they were copied from when the playhead is outside the clip.
    fn paste_notes(&mut self, from: Ticks, span: Ticks, notes: &[Note]) {
        let view = self.view.clone();
        let project = &view.project;
        let Some((_, clip, _)) = self.sel.clip.and_then(|c| midi_notes(project, c)) else {
            self.info("Open a MIDI clip in the editor to paste notes into it");
            return;
        };
        let playhead = self.playhead();
        let inside = playhead - clip.start;
        let mut at = if (0..clip.length).contains(&inside) {
            snap_floor(inside as f64, Some(self.piano.grid.ticks()))
        } else {
            from + span
        };
        if let Some(last) = self.edit.last_paste
            && last.playhead == playhead
            && last.clip == Some(clip.id)
        {
            at = last.next;
        }
        let new: Vec<Note> = notes
            .iter()
            .map(|n| Note {
                id: NoteId::new(),
                start: n.start + at,
                ..*n
            })
            .filter(|n| n.start < clip.length)
            .collect();
        if new.is_empty() {
            self.info("No room for the notes there: the clip ends first");
            return;
        }
        let ids: Vec<NoteId> = new.iter().map(|n| n.id).collect();
        let tx = Transaction::user(
            label("Paste", new.len(), "note"),
            vec![Command::AddNotes {
                clip: clip.id,
                notes: new,
            }],
        );
        let r = actions::apply(&self.session, tx, None);
        if self.report(r).is_some() {
            self.sel.notes = ids;
            self.sel.focus = Focus::Editor;
            self.edit.last_paste = Some(LastPaste {
                playhead,
                clip: Some(clip.id),
                next: at + span,
            });
        }
    }

    /// Clips go in at the playhead, the topmost on the selected track.
    fn paste_clips(&mut self, span: Ticks, clips: &[CopiedClip]) {
        let view = self.view.clone();
        let project = &view.project;
        let playhead = self.playhead();
        let grid = self.arrange.snap.grid(project.time_signature);
        let mut at = snap_floor(playhead as f64, grid).max(0);
        if let Some(last) = self.edit.last_paste
            && last.playhead == playhead
            && last.clip.is_none()
        {
            at = last.next;
        }
        // Audio from another project's pool cannot come along.
        let usable: Vec<&CopiedClip> = clips
            .iter()
            .filter(|c| match &c.clip.content {
                ClipContent::Audio { source, .. } => project.pool_entry(*source).is_ok(),
                _ => true,
            })
            .collect();
        if usable.is_empty() {
            self.info("The copied audio is not in this project");
            return;
        }
        let Some(tracks) = paste_tracks(project, &usable, self.sel.track) else {
            self.info(
                "Select a track that can take these clips: audio clips go on audio tracks, \
                 the others on instrument tracks",
            );
            return;
        };
        let placed: Vec<(TrackId, Clip)> = usable
            .iter()
            .zip(tracks)
            .map(|(c, track)| (track, fresh(&c.clip, c.clip.start + at)))
            .collect();
        let tx = Transaction::user(
            label("Paste", placed.len(), "clip"),
            placed
                .iter()
                .map(|(track, clip)| Command::AddClip {
                    track: *track,
                    clip: Box::new(clip.clone()),
                })
                .collect(),
        );
        let r = actions::apply(&self.session, tx, None);
        if self.report(r).is_some() {
            self.select_placed(&placed);
            self.edit.last_paste = Some(LastPaste {
                playhead,
                clip: None,
                next: at + span,
            });
        }
    }

    /// Selects new clips, opening the first in the editor.
    fn select_placed(&mut self, placed: &[(TrackId, Clip)]) {
        if let Some((track, clip)) = placed.first() {
            self.sel.open_clip(*track, clip.id);
        }
        self.sel.clips = placed.iter().map(|(_, c)| c.id).collect();
    }

    /// Copies the selection to just after itself.
    pub fn duplicate(&mut self) {
        let view = self.view.clone();
        let project = &view.project;
        match self.target() {
            Target::Notes(clip_id) => {
                let Some((_, clip, notes)) = midi_notes(project, clip_id) else {
                    return;
                };
                let chosen = chosen_notes(notes, &self.sel.notes);
                if chosen.is_empty() {
                    self.info("Select notes to duplicate first");
                    return;
                }
                let (_, span) = extent(chosen.iter().map(|n| (n.start, n.length)));
                let new: Vec<Note> = chosen
                    .iter()
                    .map(|n| Note {
                        id: NoteId::new(),
                        start: n.start + span,
                        ..*n
                    })
                    .filter(|n| n.start < clip.length)
                    .collect();
                if new.is_empty() {
                    self.info("No room after these notes: the clip ends first");
                    return;
                }
                let ids: Vec<NoteId> = new.iter().map(|n| n.id).collect();
                let tx = Transaction::user(
                    label("Duplicate", new.len(), "note"),
                    vec![Command::AddNotes {
                        clip: clip_id,
                        notes: new,
                    }],
                );
                let r = actions::apply(&self.session, tx, None);
                if self.report(r).is_some() {
                    self.sel.notes = ids;
                }
            }
            Target::Clips => {
                let chosen = chosen_clips(project, &self.sel.clips);
                if chosen.is_empty() {
                    self.info("Select clips to duplicate first");
                    return;
                }
                let (_, span) = extent(chosen.iter().map(|(_, c)| (c.start, c.length)));
                let placed: Vec<(TrackId, Clip)> = chosen
                    .iter()
                    .map(|(row, c)| (project.tracks[*row].id, fresh(c, c.start + span)))
                    .collect();
                let tx = Transaction::user(
                    label("Duplicate", placed.len(), "clip"),
                    placed
                        .iter()
                        .map(|(track, clip)| Command::AddClip {
                            track: *track,
                            clip: Box::new(clip.clone()),
                        })
                        .collect(),
                );
                let r = actions::apply(&self.session, tx, None);
                if self.report(r).is_some() {
                    self.select_placed(&placed);
                }
            }
        }
    }

    pub fn select_all(&mut self) {
        let view = self.view.clone();
        let project = &view.project;
        match self.target() {
            Target::Notes(clip) => {
                if let Some((_, _, notes)) = midi_notes(project, clip) {
                    self.sel.notes = notes.iter().map(|n| n.id).collect();
                }
            }
            Target::Clips => {
                let all: Vec<(TrackId, ClipId)> = project
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter().map(|c| (t.id, c.id)))
                    .collect();
                let open = self.sel.clip.filter(|c| all.iter().any(|(_, id)| id == c));
                if let (None, Some((track, clip))) = (open, all.first()) {
                    self.sel.open_clip(*track, *clip);
                }
                self.sel.clips = all.into_iter().map(|(_, c)| c).collect();
            }
        }
    }

    pub fn select_none(&mut self) {
        match self.target() {
            Target::Notes(_) => self.sel.notes.clear(),
            Target::Clips => {
                self.sel.clips.clear();
                self.arrange.asset = None;
            }
        }
    }

    /// Moves the selection along by grid steps, or by bars.
    pub fn nudge(&mut self, steps: i32, bars: bool) {
        let view = self.view.clone();
        let project = &view.project;
        let bar = project.ticks_per_bar();
        match self.target() {
            Target::Notes(clip_id) => {
                let Some((_, clip, notes)) = midi_notes(project, clip_id) else {
                    return;
                };
                let chosen = chosen_notes(notes, &self.sel.notes);
                let (Some(first), Some(last)) = (
                    chosen.iter().map(|n| n.start).min(),
                    chosen.iter().map(|n| n.start).max(),
                ) else {
                    return;
                };
                let unit = if bars { bar } else { self.piano.grid.ticks() };
                let dt = (unit * Ticks::from(steps)).max(-first);
                // Notes that start past the end would not play.
                if dt == 0 || last + dt >= clip.length {
                    return;
                }
                let moved = chosen
                    .iter()
                    .map(|n| Note {
                        start: n.start + dt,
                        ..*n
                    })
                    .collect();
                let r = actions::update_notes(&self.session, clip_id, moved, "Nudge notes", None);
                self.report(r);
            }
            Target::Clips => {
                let chosen = chosen_clips(project, &self.sel.clips);
                let Some(first) = chosen.iter().map(|(_, c)| c.start).min() else {
                    return;
                };
                let unit = if bars {
                    bar
                } else {
                    self.arrange
                        .snap
                        .grid(project.time_signature)
                        .unwrap_or(PPQ / 4)
                };
                let dt = (unit * Ticks::from(steps)).max(-first);
                if dt == 0 {
                    return;
                }
                let tx = Transaction::user(
                    label("Nudge", chosen.len(), "clip"),
                    chosen
                        .iter()
                        .map(|(row, c)| Command::MoveClip {
                            id: c.id,
                            track: project.tracks[*row].id,
                            start: c.start + dt,
                        })
                        .collect(),
                );
                let r = actions::apply(&self.session, tx, None);
                self.report(r);
            }
        }
    }

    /// Moves the selected notes up or down by semitones, and plays them.
    pub fn transpose(&mut self, semitones: i32) {
        let view = self.view.clone();
        let project = &view.project;
        let Target::Notes(clip_id) = self.target() else {
            self.info("Select notes in the piano roll to transpose them");
            return;
        };
        let Some((track, clip, notes)) = midi_notes(project, clip_id) else {
            return;
        };
        let chosen = chosen_notes(notes, &self.sel.notes);
        let (Some(lo), Some(hi)) = (
            chosen.iter().map(|n| i32::from(n.pitch)).min(),
            chosen.iter().map(|n| i32::from(n.pitch)).max(),
        ) else {
            return;
        };
        let by = semitones.clamp(-lo, 127 - hi);
        if by == 0 {
            return;
        }
        let moved: Vec<Note> = chosen
            .iter()
            .map(|n| Note {
                pitch: (i32::from(n.pitch) + by) as u8,
                ..*n
            })
            .collect();
        let heard: Vec<(u8, u8)> = moved.iter().map(|n| (n.pitch, n.velocity)).collect();
        let first = moved.iter().min_by_key(|n| n.start).copied();
        let label = if by.abs() == 12 {
            "Move notes by an octave"
        } else {
            "Transpose notes"
        };
        let r = actions::update_notes(&self.session, clip_id, moved, label, None);
        if self.report(r).is_some()
            && let Some(n) = first
        {
            let seconds = audition::note_seconds(project, clip.start + n.start, n.length);
            self.hear_notes(track.id, &heard, seconds);
        }
    }

    /// Whether the piano roll has a MIDI clip open to quantize.
    pub fn can_quantize(&self) -> bool {
        self.sel
            .clip
            .is_some_and(|id| midi_notes(&self.view.project, id).is_some())
    }

    /// Moves the piano roll's selected notes, or all its notes when none are
    /// selected, to the grid. With `ends`, their ends move to it too.
    pub fn quantize_notes(&mut self, ends: bool) {
        let view = self.view.clone();
        let Some((_, clip, notes)) = self.sel.clip.and_then(|id| midi_notes(&view.project, id))
        else {
            self.info("Open a MIDI clip in the piano roll to quantize its notes");
            return;
        };
        let chosen = chosen_notes(notes, &self.sel.notes);
        let chosen = if chosen.is_empty() {
            notes.to_vec()
        } else {
            chosen
        };
        let moved = quantized(&chosen, self.piano.grid.ticks(), clip.length, ends);
        if moved.is_empty() {
            self.info("Those notes are on the grid already");
            return;
        }
        let label = label("Quantize", moved.len(), "note");
        let r = actions::update_notes(&self.session, clip.id, moved, &label, None);
        self.report(r);
    }

    /// Moves the selected clips to the track `rows` below, or above when
    /// negative.
    pub fn move_to_track(&mut self, rows: i32) {
        let view = self.view.clone();
        let project = &view.project;
        let chosen = chosen_clips(project, &self.sel.clips);
        if chosen.is_empty() {
            self.info("Select clips to move first");
            return;
        }
        let mut commands = Vec::new();
        for (row, clip) in &chosen {
            let target = row
                .checked_add_signed(rows as isize)
                .and_then(|r| project.tracks.get(r));
            let Some(track) = target else {
                self.info(if rows < 0 {
                    "There is no track above"
                } else {
                    "There is no track below"
                });
                return;
            };
            if !clip_fits(track, clip) {
                let what = match clip.content {
                    ClipContent::Audio { .. } => "audio clips",
                    _ => "this kind of clip",
                };
                self.info(format!("{} cannot hold {what}", track.name));
                return;
            }
            commands.push(Command::MoveClip {
                id: clip.id,
                track: track.id,
                start: clip.start,
            });
        }
        let tx = Transaction::user(label("Move", chosen.len(), "clip"), commands);
        let r = actions::apply(&self.session, tx, None);
        self.report(r);
    }

    /// Zooms the arrangement, or the piano roll while it has the focus.
    pub fn timeline_zoom(&mut self, direction: i32) {
        let factor = if direction > 0 { 1.4 } else { 1.0 / 1.4 };
        match self.edit_focus() {
            Focus::Editor => self.piano.zoom(factor),
            Focus::Arrange => {
                let center = self.arrange.lane_left + self.arrange.lane_width * 0.5;
                self.arrange.zoom_at(factor, center);
            }
        }
    }

    /// Shows the whole song, or the whole clip in the piano roll.
    pub fn timeline_fit(&mut self) {
        match self.edit_focus() {
            // The piano roll fits the clip again when it next draws.
            Focus::Editor => self.piano.clip = None,
            Focus::Arrange => self.arrange.fit(self.view.project.content_end()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_moves_notes_to_the_grid() {
        let step = PPQ / 4;
        let notes = [
            Note::new(10, 100, 60, 90),
            Note::new(step * 2, step, 62, 90),
            Note::new(step - 10, step + 30, 64, 90),
            // Rounding up would start it at the clip's end.
            Note::new(step * 8 - 20, 15, 65, 90),
        ];
        let length = step * 8;
        let moved = quantized(&notes, step, length, false);
        // The note already on the grid is left out.
        assert_eq!(moved.len(), 3);
        assert_eq!((moved[0].start, moved[0].length), (0, 100));
        assert_eq!((moved[1].start, moved[1].length), (step, step + 30));
        assert_eq!(moved[2].start, step * 7);
        assert!(moved.iter().all(|m| notes.iter().any(|n| n.id == m.id)));

        let moved = quantized(&notes, step, length, true);
        // A note shorter than half a step still lasts one step.
        assert_eq!((moved[0].start, moved[0].length), (0, step));
        assert_eq!((moved[1].start, moved[1].length), (step, step));
        assert_eq!((moved[2].start, moved[2].length), (step * 7, step));
    }
}
