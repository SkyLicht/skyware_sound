// SPDX-License-Identifier: GPL-3.0-or-later
//! Clip edits beyond moving and resizing, and the arrangement tools that
//! reach them: trimming a clip's start, slipping what it plays, splitting,
//! muting and erasing, painting new clips, and the fade and gain handles of
//! audio clips.
//!
//! The edits are plain functions from one clip to another, so a drag's
//! preview and the commands it applies come from the same code.

use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, Id, Key, Painter, Pos2, Rect, Sense,
    Stroke, StrokeKind, Ui, pos2, vec2,
};
use sky_core::{ClipId, NoteId, Ticks, TrackId};
use sky_model::{Clip, ClipContent, Command, Note, Project, Track, TrackKind, Transaction};

use crate::actions;
use crate::app::{App, Tab};
use crate::arrange::{self, ArrangeState, CLIP_HEAD, snap_floor, snap_round};
use crate::theme;

/// What a click or drag in the arrangement does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Draw,
    Split,
    Slip,
    Mute,
    Erase,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Select,
        Tool::Draw,
        Tool::Split,
        Tool::Slip,
        Tool::Mute,
        Tool::Erase,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Draw => "Draw",
            Tool::Split => "Split",
            Tool::Slip => "Slip",
            Tool::Mute => "Mute",
            Tool::Erase => "Erase",
        }
    }

    /// Its name in the list of keyboard shortcuts.
    pub fn command(self) -> &'static str {
        match self {
            Tool::Select => "The Select tool",
            Tool::Draw => "The Draw tool",
            Tool::Split => "The Split tool",
            Tool::Slip => "The Slip tool",
            Tool::Mute => "The Mute tool",
            Tool::Erase => "The Erase tool",
        }
    }

    /// The number key that picks it.
    pub fn key(self) -> Key {
        match self {
            Tool::Select => Key::Num1,
            Tool::Draw => Key::Num2,
            Tool::Split => Key::Num3,
            Tool::Slip => Key::Num4,
            Tool::Mute => Key::Num5,
            Tool::Erase => Key::Num6,
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            Tool::Select => {
                "Select, move and resize clips. Drag a clip's left edge to trim it, \
                 and Ctrl+Alt-drag a clip to slip it."
            }
            Tool::Draw => {
                "Click an empty spot of an instrument track to paint a one-bar clip, \
                 or drag to paint one as long as the drag"
            }
            Tool::Split => "Click a clip to cut it in two there",
            Tool::Slip => "Drag a clip to slide the audio or notes inside it",
            Tool::Mute => "Click clips, or drag across them, to mute or unmute them",
            Tool::Erase => "Click clips, or drag across them, to delete them",
        }
    }
}

/// The sample rate of an audio clip's file.
fn source_rate(project: &Project, clip: &Clip) -> f64 {
    let rate = match &clip.content {
        ClipContent::Audio { source, .. } => project
            .pool_entry(*source)
            .map_or(project.sample_rate, |e| e.sample_rate),
        _ => project.sample_rate,
    };
    f64::from(rate.max(1))
}

/// Frames of a file at `rate` between two song positions.
fn frames_between(project: &Project, from: Ticks, to: Ticks, rate: f64) -> i64 {
    let seconds = project.tempo.tick_to_seconds(to) - project.tempo.tick_to_seconds(from);
    (seconds * rate).round() as i64
}

/// The earliest a clip's start can be trimmed back to: the start of the
/// song, or for audio, where its file begins.
pub fn earliest_start(project: &Project, clip: &Clip) -> Ticks {
    match &clip.content {
        ClipContent::Audio { offset, .. } => {
            let rate = source_rate(project, clip);
            let seconds = project.tempo.tick_to_seconds(clip.start) - *offset as f64 / rate;
            (project.tempo.seconds_to_ticks(seconds).ceil() as Ticks).max(0)
        }
        _ => 0,
    }
}

/// A note moved `by` ticks. One moved to before the clip's start is cut to
/// start there, and dropped when nothing of it is left.
fn shifted(n: &Note, by: Ticks) -> Option<Note> {
    let start = n.start + by;
    let end = start + n.length;
    (end > 0).then(|| Note {
        start: start.max(0),
        length: end - start.max(0),
        ..*n
    })
}

/// The clip with its start moved to `start` and its end kept. Audio keeps
/// its place in time and MIDI notes keep theirs; a note the new start cuts
/// into starts there instead. Pattern and step clips start their pattern
/// at the new start.
pub fn trimmed(project: &Project, clip: &Clip, start: Ticks) -> Clip {
    let mut out = clip.clone();
    let start = start.clamp(0, clip.end() - 1);
    out.start = start;
    out.length = clip.end() - start;
    match &mut out.content {
        ClipContent::Audio {
            offset,
            fade_in,
            fade_out,
            ..
        } => {
            let rate = source_rate(project, clip);
            *offset = (*offset + frames_between(project, clip.start, start, rate)).max(0);
            *fade_in = (*fade_in).min(out.length);
            *fade_out = (*fade_out).min(out.length - *fade_in);
        }
        ClipContent::Midi { notes } => {
            let by = clip.start - start;
            *notes = notes.iter().filter_map(|n| shifted(n, by)).collect();
        }
        ClipContent::Pattern { .. } | ClipContent::Steps(_) => {}
    }
    out
}

/// Whether slipping changes anything: pattern and step clips always start
/// their pattern at the clip's start.
pub fn can_slip(clip: &Clip) -> bool {
    matches!(
        clip.content,
        ClipContent::Audio { .. } | ClipContent::Midi { .. }
    )
}

/// The clip with what it plays slid `by` ticks later inside it, or earlier
/// when negative, as far as there is audio or notes to slide.
pub fn slipped(project: &Project, clip: &Clip, by: Ticks) -> Clip {
    let rate = source_rate(project, clip);
    let mut out = clip.clone();
    match &mut out.content {
        ClipContent::Audio { source, offset, .. } => {
            let frames = project
                .pool_entry(*source)
                .map_or(i64::MAX, |e| e.frames as i64);
            let moved = frames_between(project, clip.start, clip.start + by, rate);
            *offset = (*offset - moved).clamp(0, (frames - 1).max(0));
        }
        ClipContent::Midi { notes } => {
            let first = notes.iter().map(|n| n.start).min().unwrap_or(0);
            let by = by.max(-first);
            for n in notes.iter_mut() {
                n.start += by;
            }
        }
        ClipContent::Pattern { .. } | ClipContent::Steps(_) => {}
    }
    out
}

/// A clip cut in two at `at`, a song position inside it: the clip itself,
/// shortened, and a new clip for the rest. A note across the cut is cut in
/// two; pattern and step clips start their pattern again at the cut.
pub fn split(project: &Project, clip: &Clip, at: Ticks) -> Option<(Clip, Clip)> {
    if at <= clip.start || at >= clip.end() {
        return None;
    }
    let mut left = clip.clone();
    left.length = at - clip.start;
    match &mut left.content {
        ClipContent::Audio {
            fade_in, fade_out, ..
        } => {
            *fade_in = (*fade_in).min(left.length);
            *fade_out = 0;
        }
        ClipContent::Midi { notes } => notes.retain(|n| n.start < left.length),
        ClipContent::Pattern { .. } | ClipContent::Steps(_) => {}
    }
    let mut right = trimmed(project, clip, at);
    right.id = ClipId::new();
    match &mut right.content {
        ClipContent::Audio { fade_in, .. } => *fade_in = 0,
        ClipContent::Midi { notes } => {
            for n in notes.iter_mut() {
                n.id = NoteId::new();
            }
        }
        ClipContent::Pattern { .. } | ClipContent::Steps(_) => {}
    }
    Some((left, right))
}

/// The commands that turn `old` into `new`, the same clip, on `track`.
pub fn changes(old: &Clip, new: &Clip, track: TrackId) -> Vec<Command> {
    let id = old.id;
    let mut out = Vec::new();
    if new.start != old.start {
        out.push(Command::MoveClip {
            id,
            track,
            start: new.start,
        });
    }
    if new.length != old.length {
        out.push(Command::SetClipLength {
            id,
            length: new.length,
        });
    }
    if new.muted != old.muted {
        out.push(Command::SetClipMuted {
            id,
            muted: new.muted,
        });
    }
    match (&old.content, &new.content) {
        (
            ClipContent::Audio {
                offset: o0,
                gain_db: g0,
                fade_in: i0,
                fade_out: f0,
                ..
            },
            ClipContent::Audio {
                offset: o1,
                gain_db: g1,
                fade_in: i1,
                fade_out: f1,
                ..
            },
        ) => {
            if (o0, g0, i0, f0) != (o1, g1, i1, f1) {
                out.push(Command::SetAudioClip {
                    id,
                    gain_db: (g1 != g0).then_some(*g1),
                    fade_in: (i1 != i0).then_some(*i1),
                    fade_out: (f1 != f0).then_some(*f1),
                    offset: (o1 != o0).then_some(*o1),
                });
            }
        }
        (ClipContent::Midi { notes: a }, ClipContent::Midi { notes: b }) => {
            let gone: Vec<NoteId> = a
                .iter()
                .filter(|n| !b.iter().any(|m| m.id == n.id))
                .map(|n| n.id)
                .collect();
            let moved: Vec<Note> = b
                .iter()
                .filter(|m| a.iter().any(|n| n.id == m.id && n != *m))
                .copied()
                .collect();
            let added: Vec<Note> = b
                .iter()
                .filter(|m| !a.iter().any(|n| n.id == m.id))
                .copied()
                .collect();
            if !gone.is_empty() {
                out.push(Command::RemoveNotes {
                    clip: id,
                    ids: gone,
                });
            }
            if !moved.is_empty() {
                out.push(Command::UpdateNotes {
                    clip: id,
                    notes: moved,
                });
            }
            if !added.is_empty() {
                out.push(Command::AddNotes {
                    clip: id,
                    notes: added,
                });
            }
        }
        _ => {}
    }
    out
}

/// "Split clip" or "Split 3 clips".
fn label(verb: &str, n: usize) -> String {
    if n == 1 {
        format!("{verb} clip")
    } else {
        format!("{verb} {n} clips")
    }
}

/// What a handle on an audio clip changes, or a slip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Slip,
    FadeIn,
    FadeOut,
    Gain,
}

/// A one-clip edit being dragged: a slip, a fade or the gain. Shown as a
/// preview and applied as one undo step when the button comes up.
#[derive(Clone, Debug)]
pub struct ClipEdit {
    pub kind: EditKind,
    pub track: TrackId,
    pub orig: Clip,
    pub preview: Clip,
    /// Where the pointer pressed.
    pub grab: Pos2,
}

/// Mute or Erase being dragged across clips, applied on release.
#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    pub tool: Tool,
    pub clips: Vec<ClipId>,
    /// What Mute sets every clip to: the opposite of the first one's state.
    pub mute: Option<bool>,
}

/// A clip being painted with the Draw tool, from where the drag began.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paint {
    pub track: TrackId,
    pub from: f64,
    pub to: f64,
}

/// The grid of a drag: off while Alt is held.
fn grid(app: &App, ctx: &Context, project: &Project) -> Option<Ticks> {
    if ctx.input(|i| i.modifiers.alt) {
        None
    } else {
        app.arrange.snap.grid(project.time_signature)
    }
}

/// The span a paint covers on the grid. A click paints a bar, as a
/// double-click on an empty lane does; a drag paints what it covers, at
/// least one grid step, or a bar without a grid.
fn paint_span(p: &Paint, grid: Option<Ticks>, bar: Ticks) -> (Ticks, Ticks) {
    let start = snap_floor(p.from.min(p.to), grid).max(0);
    if p.from == p.to {
        return (start, bar);
    }
    let step = grid.unwrap_or(bar).max(1);
    let end = snap_round(p.from.max(p.to), grid).max(start + step);
    (start, end - start)
}

impl App {
    /// Cuts the clips among `ids` that `at` falls inside, as one change,
    /// and returns how many it cut. The new halves join the selection when
    /// the clip they came from was in it.
    pub fn split_clips(&mut self, ids: &[ClipId], at: Ticks) -> usize {
        let project = self.view.project.clone();
        let mut commands = Vec::new();
        let mut halves = Vec::new();
        for &id in ids {
            let Ok((track, clip)) = project.find_clip(id) else {
                continue;
            };
            let Some((left, right)) = split(&project, clip, at) else {
                continue;
            };
            commands.extend(changes(clip, &left, track.id));
            halves.push((id, right.id));
            commands.push(Command::AddClip {
                track: track.id,
                clip: Box::new(right),
            });
        }
        if halves.is_empty() {
            return 0;
        }
        let tx = Transaction::user(label("Split", halves.len()), commands);
        if self
            .report(actions::apply(&self.session, tx, None))
            .is_some()
        {
            for (left, right) in &halves {
                if self.sel.clips.contains(left) {
                    self.sel.clips.push(*right);
                }
            }
            self.refresh();
        }
        halves.len()
    }

    /// Edit > Split at the playhead: the selected clips under it, or else
    /// the clips under it on the selected track.
    pub fn split_at_playhead(&mut self) {
        let at = self.transport.position_ticks.max(0.0).round() as Ticks;
        let project = self.view.project.clone();
        let under = |c: &Clip| c.start < at && at < c.end();
        let mut ids: Vec<ClipId> = project
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .filter(|c| self.sel.clips.contains(&c.id) && under(c))
            .map(|c| c.id)
            .collect();
        if ids.is_empty()
            && let Some(track) = self.sel.track.and_then(|t| project.track(t).ok())
        {
            ids = track
                .clips
                .iter()
                .filter(|c| under(c))
                .map(|c| c.id)
                .collect();
        }
        if self.split_clips(&ids, at) == 0 {
            self.info("Put the playhead over a selected clip, or a clip on the selected track, to split it");
        }
    }

    /// Mutes or unmutes clips as one change.
    pub fn set_muted(&mut self, ids: &[ClipId], muted: bool) {
        let project = self.view.project.clone();
        let commands: Vec<Command> = ids
            .iter()
            .filter(|id| project.find_clip(**id).is_ok_and(|(_, c)| c.muted != muted))
            .map(|&id| Command::SetClipMuted { id, muted })
            .collect();
        if commands.is_empty() {
            return;
        }
        let verb = if muted { "Mute" } else { "Unmute" };
        let tx = Transaction::user(label(verb, commands.len()), commands);
        let r = actions::apply(&self.session, tx, None);
        self.report(r);
        self.refresh();
    }

    /// Edit > Mute or unmute: mutes the selected clips, or unmutes them
    /// when every one is muted already.
    pub fn mute_selection(&mut self) {
        let project = self.view.project.clone();
        let chosen: Vec<&Clip> = project
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .filter(|c| self.sel.clips.contains(&c.id))
            .collect();
        if chosen.is_empty() {
            self.info("Select clips to mute first");
            return;
        }
        let mute = chosen.iter().any(|c| !c.muted);
        let ids: Vec<ClipId> = chosen.iter().map(|c| c.id).collect();
        self.set_muted(&ids, mute);
    }

    /// Deletes clips as one change.
    pub fn erase_clips(&mut self, ids: &[ClipId]) {
        let project = self.view.project.clone();
        let ids: Vec<ClipId> = ids
            .iter()
            .copied()
            .filter(|id| project.find_clip(*id).is_ok())
            .collect();
        if ids.is_empty() {
            return;
        }
        let tx = Transaction::user(
            label("Delete", ids.len()),
            ids.iter().map(|&id| Command::RemoveClip { id }).collect(),
        );
        if self
            .report(actions::apply(&self.session, tx, None))
            .is_some()
        {
            self.sel.clips.retain(|c| !ids.contains(c));
            if self.sel.clip.is_some_and(|c| ids.contains(&c)) {
                self.sel.clip = None;
                self.sel.notes.clear();
            }
        }
        self.refresh();
    }

    /// Picks the arrangement tool.
    pub fn set_tool(&mut self, tool: Tool) {
        self.arrange.tool = tool;
        self.arrange.sweep = None;
        self.arrange.paint = None;
    }
}

/// Starts a slip, fade or gain drag on one clip, selecting it.
pub fn begin_edit(app: &mut App, track: TrackId, clip: &Clip, kind: EditKind, grab: Pos2) {
    if app.sel.clips.contains(&clip.id) {
        app.sel.open_clip(track, clip.id);
    } else {
        app.sel.select_clip(track, clip.id);
    }
    app.arrange.clip_edit = Some(ClipEdit {
        kind,
        track,
        orig: clip.clone(),
        preview: clip.clone(),
        grab,
    });
}

/// Follows a slip, fade or gain drag, and applies it on release.
pub fn update_edit(app: &mut App, ctx: &Context, project: &Project) {
    let Some(mut e) = app.arrange.clip_edit.take() else {
        return;
    };
    if project.find_clip(e.orig.id).is_err() {
        return;
    }
    let (pos, down, escape, fine) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.key_pressed(Key::Escape),
            i.modifiers.shift,
        )
    });
    if escape {
        return;
    }
    let grid = grid(app, ctx, project);
    if let Some(p) = pos {
        let st = &app.arrange;
        let clip = &e.orig;
        let tick = st.tick_at(p.x).round() as Ticks;
        let mut preview = clip.clone();
        match e.kind {
            EditKind::Slip => {
                let by = st.tick_at(p.x) - st.tick_at(e.grab.x);
                preview = slipped(project, clip, snap_round(by, grid));
            }
            EditKind::FadeIn | EditKind::FadeOut => {
                if let ClipContent::Audio {
                    fade_in, fade_out, ..
                } = &mut preview.content
                {
                    if e.kind == EditKind::FadeIn {
                        *fade_in = (tick - clip.start).clamp(0, clip.length - *fade_out);
                    } else {
                        *fade_out = (clip.end() - tick).clamp(0, clip.length - *fade_in);
                    }
                }
            }
            EditKind::Gain => {
                if let ClipContent::Audio { gain_db, .. } = &mut preview.content {
                    let per_px = if fine { 0.05 } else { 0.25 };
                    let db = *gain_db + (e.grab.y - p.y) * per_px;
                    *gain_db = ((db * 10.0).round() / 10.0).clamp(-60.0, 24.0);
                }
            }
        }
        e.preview = preview;
        ctx.set_cursor_icon(match e.kind {
            EditKind::Slip => CursorIcon::Grabbing,
            EditKind::FadeIn | EditKind::FadeOut => CursorIcon::ResizeHorizontal,
            EditKind::Gain => CursorIcon::ResizeVertical,
        });
    }
    if down {
        app.arrange.clip_edit = Some(e);
        return;
    }
    let commands = changes(&e.orig, &e.preview, e.track);
    if !commands.is_empty() {
        let label = match e.kind {
            EditKind::Slip => "Slip clip",
            EditKind::FadeIn => "Change fade in",
            EditKind::FadeOut => "Change fade out",
            EditKind::Gain => "Change clip gain",
        };
        let r = actions::apply(&app.session, Transaction::user(label, commands), None);
        app.report(r);
    }
    app.refresh();
}

/// The clip under a point in the lanes drawn last frame.
fn clip_at(app: &App, project: &Project, p: Pos2) -> Option<(TrackId, ClipId)> {
    let (id, lane) = app.arrange.prev_lanes.iter().find(|(_, r)| r.contains(p))?;
    let track = project.track(*id).ok()?;
    track
        .clips
        .iter()
        .rev()
        .find(|c| arrange::clip_rect(&app.arrange, *lane, c.start, c.length).contains(p))
        .map(|c| (track.id, c.id))
}

/// Starts a Mute or Erase sweep, from a clip or an empty spot.
pub fn begin_sweep(app: &mut App, tool: Tool, first: Option<&Clip>) {
    app.arrange.sweep = Some(Sweep {
        tool,
        clips: first.map(|c| vec![c.id]).unwrap_or_default(),
        mute: first.map(|c| !c.muted),
    });
}

/// Follows a Mute or Erase sweep, and applies it on release.
pub fn update_sweep(app: &mut App, ctx: &Context, project: &Project) {
    let Some(mut s) = app.arrange.sweep.take() else {
        return;
    };
    let (pos, down, escape) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.key_pressed(Key::Escape),
        )
    });
    if escape {
        return;
    }
    if let Some((_, id)) = pos.and_then(|p| clip_at(app, project, p))
        && !s.clips.contains(&id)
        && let Ok((_, clip)) = project.find_clip(id)
    {
        s.clips.push(id);
        s.mute.get_or_insert(!clip.muted);
    }
    if down {
        app.arrange.sweep = Some(s);
        return;
    }
    match (s.tool, s.mute) {
        (Tool::Erase, _) => app.erase_clips(&s.clips),
        (Tool::Mute, Some(mute)) => app.set_muted(&s.clips, mute),
        _ => {}
    }
}

/// Follows a clip being painted with the Draw tool, and adds it on release.
pub fn update_paint(app: &mut App, ctx: &Context, project: &Project) {
    let Some(mut p) = app.arrange.paint.take() else {
        return;
    };
    let (pos, down, escape) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.key_pressed(Key::Escape),
        )
    });
    if escape || project.track(p.track).is_err() {
        return;
    }
    if let Some(pos) = pos {
        p.to = app.arrange.tick_at(pos.x).max(0.0);
    }
    if down {
        app.arrange.paint = Some(p);
        return;
    }
    let (start, length) = paint_span(&p, grid(app, ctx, project), project.ticks_per_bar());
    let r = actions::add_midi_clip(&app.session, p.track, start, length);
    if let Some(id) = app.report(r) {
        app.refresh();
        app.sel.select_clip(p.track, id);
        app.workspace.show(Tab::Editor);
    }
}

/// Draws a clip being painted on its lane.
pub fn draw_paint(
    app: &App,
    ctx: &Context,
    painter: &Painter,
    lane: Rect,
    project: &Project,
    track: &Track,
) {
    let Some(p) = app.arrange.paint.filter(|p| p.track == track.id) else {
        return;
    };
    let (start, length) = paint_span(&p, grid(app, ctx, project), project.ticks_per_bar());
    let r = arrange::clip_rect(&app.arrange, lane, start, length);
    let color = theme::track_color(track.color);
    painter.rect_filled(r, CornerRadius::same(3), color.gamma_multiply(0.35));
    painter.rect_stroke(
        r,
        CornerRadius::same(3),
        Stroke::new(1.5, color),
        StrokeKind::Inside,
    );
}

/// What the tools do on an empty spot of a lane, before the Select tool's
/// marquee and double-click. True when a tool took the gesture.
pub fn lane_gesture(
    app: &mut App,
    resp: &egui::Response,
    origin: Option<Pos2>,
    track: &Track,
) -> bool {
    let tool = app.arrange.tool;
    match tool {
        Tool::Draw => {
            if resp.hovered() && track.kind == TrackKind::Instrument {
                resp.ctx.set_cursor_icon(CursorIcon::Crosshair);
            }
            let start = if resp.drag_started() { origin } else { None };
            let click = if resp.clicked() {
                resp.interact_pointer_pos()
            } else {
                None
            };
            let Some(at) = start.or(click) else {
                return resp.dragged();
            };
            if track.kind != TrackKind::Instrument {
                app.info("Draw paints MIDI clips on instrument tracks; audio comes from the browser or the pool");
                return true;
            }
            let tick = app.arrange.tick_at(at.x).max(0.0);
            app.sel.select_track(track.id);
            app.arrange.paint = Some(Paint {
                track: track.id,
                from: tick,
                to: tick,
            });
            if click.is_some() {
                // A click paints at once: the button is already up.
                let ctx = resp.ctx.clone();
                let project = app.view.project.clone();
                update_paint(app, &ctx, &project);
            }
            true
        }
        Tool::Mute | Tool::Erase => {
            if resp.drag_started() {
                begin_sweep(app, tool, None);
            }
            resp.drag_started() || resp.dragged()
        }
        Tool::Select | Tool::Split | Tool::Slip => false,
    }
}

/// What a click or drag on a clip does with a tool other than Select and
/// Draw. True when the tool handled it.
pub fn clip_gesture(
    app: &mut App,
    ui: &Ui,
    body: &egui::Response,
    painter: &Painter,
    r: Rect,
    track: &Track,
    clip: &Clip,
) -> bool {
    let tool = app.arrange.tool;
    match tool {
        Tool::Select | Tool::Draw => false,
        Tool::Split => {
            let project = app.view.project.clone();
            let grid = grid(app, ui.ctx(), &project);
            let at = body
                .hover_pos()
                .map(|p| snap_round(app.arrange.tick_at(p.x), grid))
                .filter(|t| clip.start < *t && *t < clip.end());
            if let Some(at) = at {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                let x = app.arrange.x_of(at as f64);
                painter.vline(x, r.y_range(), Stroke::new(1.5, Color32::WHITE));
            }
            if body.clicked()
                && let Some(at) = at
            {
                app.sel.select_track(track.id);
                app.split_clips(&[clip.id], at);
            }
            true
        }
        Tool::Slip => {
            if body.hovered() {
                ui.ctx().set_cursor_icon(if can_slip(clip) {
                    CursorIcon::Grab
                } else {
                    CursorIcon::NotAllowed
                });
            }
            if body.clicked() {
                app.sel.select_clip(track.id, clip.id);
            }
            if body.drag_started() {
                if can_slip(clip) {
                    let grab = ui.input(|i| i.pointer.press_origin()).unwrap_or(r.center());
                    begin_edit(app, track.id, clip, EditKind::Slip, grab);
                } else {
                    app.info("Pattern and step clips always start their pattern at the clip's start, so there is nothing to slip; trim the clip instead");
                }
            }
            true
        }
        Tool::Mute | Tool::Erase => {
            if body.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if body.drag_started() {
                begin_sweep(app, tool, Some(clip));
            } else if body.clicked() {
                if tool == Tool::Mute {
                    app.set_muted(&[clip.id], !clip.muted);
                } else {
                    app.erase_clips(&[clip.id]);
                }
            }
            true
        }
    }
}

/// How a sweep in progress shows a clip: muted, or about to go.
pub fn swept(app: &App, clip: &Clip) -> Option<(Tool, bool)> {
    let s = app.arrange.sweep.as_ref()?;
    s.clips
        .contains(&clip.id)
        .then(|| (s.tool, s.mute.unwrap_or(clip.muted)))
}

const HANDLE: f32 = 8.0;

/// The fade-in, fade-out and gain handles of an audio clip.
fn handle_rects(st: &ArrangeState, r: Rect, clip: &Clip) -> Option<[(EditKind, Rect); 3]> {
    let ClipContent::Audio {
        fade_in, fade_out, ..
    } = &clip.content
    else {
        return None;
    };
    if r.width() < 40.0 || r.height() < CLIP_HEAD + HANDLE + 8.0 {
        return None;
    }
    let y = r.top() + CLIP_HEAD + 2.0 + HANDLE * 0.5;
    let inside = |x: f32| {
        x.clamp(
            r.left() + HANDLE * 0.5 + 1.0,
            r.right() - HANDLE * 0.5 - 1.0,
        )
    };
    let at = |x: f32| Rect::from_center_size(pos2(x, y), vec2(HANDLE, HANDLE));
    let fi = inside(st.x_of((clip.start + fade_in) as f64));
    let fo = inside(st.x_of((clip.end() - fade_out) as f64));
    Some([
        (EditKind::FadeIn, at(fi)),
        (EditKind::FadeOut, at(fo)),
        (EditKind::Gain, at(r.center().x)),
    ])
}

/// The fade and gain handles of an audio clip, shown while the pointer is
/// over it or it is selected. Call after the clip's other interactions, so
/// the handles are on top.
#[allow(clippy::too_many_arguments)]
pub fn handles_ui(
    app: &mut App,
    ui: &mut Ui,
    painter: &Painter,
    lane: Rect,
    r: Rect,
    track: &Track,
    clip: &Clip,
    selected: bool,
) {
    let editing = app
        .arrange
        .clip_edit
        .as_ref()
        .filter(|e| e.orig.id == clip.id && e.kind != EditKind::Slip)
        .map(|e| e.kind);
    let near = ui.rect_contains_pointer(r.intersect(lane));
    if !(selected || near || editing.is_some()) {
        return;
    }
    let Some(handles) = handle_rects(&app.arrange, r, clip) else {
        return;
    };
    let ClipContent::Audio {
        gain_db,
        fade_in,
        fade_out,
        ..
    } = &clip.content
    else {
        return;
    };
    let project = app.view.project.clone();
    let seconds = |from: Ticks, to: Ticks| {
        project.tempo.tick_to_seconds(to) - project.tempo.tick_to_seconds(from)
    };
    let mut readout = None;
    for (kind, h) in handles {
        if !lane.contains_rect(h) {
            continue;
        }
        let id = Id::new(("clip_handle", clip.id, kind as u8));
        let resp = ui.interact(h.expand(2.0), id, Sense::click_and_drag());
        let active = resp.hovered() || editing == Some(kind);
        let fill = if active {
            theme::ACCENT
        } else {
            Color32::from_gray(232)
        };
        painter.rect_filled(h, CornerRadius::same(2), fill);
        painter.rect_stroke(
            h,
            CornerRadius::same(2),
            Stroke::new(1.0, Color32::from_gray(20)),
            StrokeKind::Outside,
        );
        if active {
            let text = match kind {
                EditKind::FadeIn => {
                    format!("Fade in {:.2} s", seconds(clip.start, clip.start + fade_in))
                }
                EditKind::FadeOut => format!(
                    "Fade out {:.2} s",
                    seconds(clip.end() - fade_out, clip.end())
                ),
                _ => format!("{gain_db:+.1} dB"),
            };
            let anchor = match kind {
                EditKind::FadeOut => (pos2(h.left() - 4.0, h.center().y), Align2::RIGHT_CENTER),
                _ => (pos2(h.right() + 4.0, h.center().y), Align2::LEFT_CENTER),
            };
            readout = Some((anchor, text));
            ui.ctx().set_cursor_icon(match kind {
                EditKind::Gain => CursorIcon::ResizeVertical,
                _ => CursorIcon::ResizeHorizontal,
            });
        }
        let resp = resp.on_hover_text(match kind {
            EditKind::FadeIn => "Fade in: drag sideways",
            EditKind::FadeOut => "Fade out: drag sideways",
            _ => "Clip gain: drag up or down, Shift for fine steps, double-click for 0 dB",
        });
        if resp.drag_started() {
            let grab = ui.input(|i| i.pointer.press_origin()).unwrap_or(h.center());
            begin_edit(app, track.id, clip, kind, grab);
        }
        if kind == EditKind::Gain && resp.double_clicked() && *gain_db != 0.0 {
            let tx = Transaction::user(
                "Change clip gain",
                vec![Command::SetAudioClip {
                    id: clip.id,
                    gain_db: Some(0.0),
                    fade_in: None,
                    fade_out: None,
                    offset: None,
                }],
            );
            let r = actions::apply(&app.session, tx, None);
            app.report(r);
            app.refresh();
        }
    }
    // Drawn after every handle, on a chip, so no other handle covers it.
    if let Some(((at, align), text)) = readout {
        let galley = painter.layout_no_wrap(text, theme::small(), Color32::WHITE);
        let rect = align.anchor_size(at, galley.size()).expand2(vec2(3.0, 1.0));
        painter.rect_filled(rect, CornerRadius::same(2), Color32::from_black_alpha(200));
        painter.galley(rect.min + vec2(3.0, 1.0), galley, Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sky_core::{AudioId, PPQ};
    use sky_model::PoolEntry;

    const SR: u32 = 48_000;

    /// A project at 120 BPM, where a quarter note is 24 000 frames.
    fn project() -> (Project, AudioId) {
        let mut p = Project::new("t");
        let id = AudioId::new();
        p.audio_pool.push(PoolEntry {
            id,
            name: "a".into(),
            file: "audio/a.wav".into(),
            channels: 1,
            sample_rate: SR,
            frames: 480_000,
            source: None,
        });
        (p, id)
    }

    fn audio(source: AudioId, offset: i64) -> Clip {
        Clip {
            id: ClipId::new(),
            name: "a".into(),
            start: PPQ * 4,
            length: PPQ * 8,
            muted: false,
            content: ClipContent::Audio {
                source,
                offset,
                gain_db: 0.0,
                fade_in: PPQ,
                fade_out: PPQ,
            },
        }
    }

    fn midi(notes: Vec<Note>) -> Clip {
        Clip {
            id: ClipId::new(),
            name: "m".into(),
            start: PPQ * 4,
            length: PPQ * 8,
            muted: false,
            content: ClipContent::Midi { notes },
        }
    }

    fn offset(c: &Clip) -> i64 {
        match c.content {
            ClipContent::Audio { offset, .. } => offset,
            _ => unreachable!(),
        }
    }

    fn notes(c: &Clip) -> Vec<(Ticks, Ticks)> {
        match &c.content {
            ClipContent::Midi { notes } => notes.iter().map(|n| (n.start, n.length)).collect(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn trimming_keeps_audio_and_notes_in_place() {
        let (p, src) = project();
        let a = audio(src, 1_000);
        // A beat later: a beat of audio further into the file.
        let t = trimmed(&p, &a, PPQ * 5);
        assert_eq!((t.start, t.end(), offset(&t)), (PPQ * 5, a.end(), 25_000));
        // Back to where the file starts, and no further.
        let e = earliest_start(&p, &a);
        assert_eq!(offset(&trimmed(&p, &a, e)), 0);
        assert!(e < a.start && trimmed(&p, &a, e - 1).start == e - 1);
        assert_eq!(offset(&trimmed(&p, &a, e - 1)), 0);

        let m = midi(vec![
            Note::new(0, PPQ * 2, 60, 100),
            Note::new(PPQ * 3, PPQ, 62, 100),
        ]);
        let t = trimmed(&p, &m, PPQ * 5);
        // The first note is cut to start at the new start; the second stays put in time.
        assert_eq!(notes(&t), [(0, PPQ), (PPQ * 2, PPQ)]);
        let t = trimmed(&p, &m, PPQ * 7);
        assert_eq!(notes(&t), [(0, PPQ)]);
        // Extending the start back leaves room before the notes.
        assert_eq!(notes(&trimmed(&p, &m, PPQ * 3))[0], (PPQ, PPQ * 2));
    }

    #[test]
    fn slipping_moves_the_content_only() {
        let (p, src) = project();
        let a = audio(src, 30_000);
        let s = slipped(&p, &a, PPQ);
        assert_eq!((s.start, s.length, offset(&s)), (a.start, a.length, 6_000));
        // Not before the start of the file.
        assert_eq!(offset(&slipped(&p, &a, PPQ * 4)), 0);
        assert_eq!(offset(&slipped(&p, &a, -PPQ)), 54_000);

        let m = midi(vec![
            Note::new(PPQ, PPQ, 60, 100),
            Note::new(PPQ * 2, PPQ, 62, 100),
        ]);
        assert_eq!(
            notes(&slipped(&p, &m, PPQ)),
            [(PPQ * 2, PPQ), (PPQ * 3, PPQ)]
        );
        // No note goes before the clip's start.
        assert_eq!(notes(&slipped(&p, &m, -PPQ * 3)), [(0, PPQ), (PPQ, PPQ)]);
    }

    #[test]
    fn splits_play_as_before() {
        let (p, src) = project();
        let a = audio(src, 0);
        assert!(split(&p, &a, a.start).is_none());
        assert!(split(&p, &a, a.end()).is_none());
        let (l, r) = split(&p, &a, PPQ * 6).unwrap();
        assert_eq!((l.id, l.start, l.length), (a.id, a.start, PPQ * 2));
        assert_ne!(r.id, a.id);
        assert_eq!((r.start, r.end(), offset(&r)), (PPQ * 6, a.end(), 48_000));
        // The fades stay at the clip's outer ends.
        let fades = |c: &Clip| match c.content {
            ClipContent::Audio {
                fade_in, fade_out, ..
            } => (fade_in, fade_out),
            _ => unreachable!(),
        };
        assert_eq!(fades(&l), (PPQ, 0));
        assert_eq!(fades(&r), (0, PPQ));

        let m = midi(vec![
            Note::new(0, PPQ, 60, 100),
            Note::new(PPQ, PPQ * 2, 62, 100),
        ]);
        let (l, r) = split(&p, &m, PPQ * 6).unwrap();
        assert_eq!(notes(&l), [(0, PPQ), (PPQ, PPQ * 2)]);
        // The note across the cut goes on in the right half.
        assert_eq!(notes(&r), [(0, PPQ)]);
        let ids = |c: &Clip| match &c.content {
            ClipContent::Midi { notes } => notes.iter().map(|n| n.id).collect::<Vec<_>>(),
            _ => unreachable!(),
        };
        assert!(ids(&r).iter().all(|id| !ids(&l).contains(id)));
    }

    #[test]
    fn changes_name_only_what_differs() {
        let (p, src) = project();
        let a = audio(src, 0);
        let track = TrackId::new();
        assert!(changes(&a, &a, track).is_empty());
        let t = trimmed(&p, &a, PPQ * 5);
        let c = changes(&a, &t, track);
        assert!(matches!(c[0], Command::MoveClip { start, .. } if start == PPQ * 5));
        assert!(matches!(c[1], Command::SetClipLength { length, .. } if length == PPQ * 7));
        assert!(matches!(
            c[2],
            Command::SetAudioClip {
                offset: Some(24_000),
                gain_db: None,
                fade_in: None,
                fade_out: None,
                ..
            }
        ));
        let m = midi(vec![
            Note::new(0, PPQ, 60, 100),
            Note::new(PPQ * 2, PPQ, 62, 100),
        ]);
        let c = changes(&m, &trimmed(&p, &m, PPQ * 5), track);
        assert!(matches!(&c[2], Command::RemoveNotes { ids, .. } if ids.len() == 1));
        assert!(matches!(&c[3], Command::UpdateNotes { notes, .. } if notes[0].start == PPQ));
    }

    #[test]
    fn paints_cover_the_grid() {
        let track = TrackId::new();
        let p = |from: f64, to: f64| Paint { track, from, to };
        let bar = PPQ * 4;
        // A click paints a bar from the grid line before it.
        assert_eq!(paint_span(&p(1000.0, 1000.0), Some(PPQ), bar), (PPQ, bar));
        // A short drag paints at least one grid step.
        assert_eq!(paint_span(&p(1000.0, 1100.0), Some(PPQ), bar), (PPQ, PPQ));
        // A drag back to the left covers the span either way.
        assert_eq!(
            paint_span(&p(5000.0, 1000.0), Some(PPQ), bar),
            (PPQ, PPQ * 4)
        );
        assert_eq!(paint_span(&p(10.0, 10.0), None, bar), (10, bar));
    }
}
