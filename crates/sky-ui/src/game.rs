// SPDX-License-Identifier: GPL-3.0-or-later
//! The Game assets window: the loops and SFX sets the project exports for a
//! game, the folder they go to, and the export itself, which also loads the
//! result into `sky-runtime` and checks every asset.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;

use egui::{Align, ComboBox, Context, DragValue, Grid, Id, Layout, RichText, TextEdit, Ui, Window};
use sky_core::manifest::{Asset, Category, MAX_ASSET_NAME, RandomRange, Selection, asset_name};
use sky_core::{AssetId, Ticks};
use sky_model::{
    Command, GameAsset, GameAssetKind, LicenseReport, MAX_SFX_PITCH_SEMITONES,
    MAX_SFX_TAIL_SECONDS, MAX_SFX_VARIATIONS, Origin, Project, SFX_GAIN_DB, TimeRange, Transaction,
};
use sky_session::{GameExportOptions, Session};

use crate::actions::{ActionResult, apply};
use crate::app::App;
use crate::dialogs::{CATEGORIES, PathPurpose, bits_choice};
use crate::jobs::JobOutput;
use crate::sounds::license_report_ui;
use crate::theme;

pub struct GameDialog {
    /// The export folder as typed; it becomes the project's setting when
    /// the export runs.
    pub folder: String,
    pub bits: u16,
    /// Asset names being typed, applied when the field loses focus.
    names: HashMap<AssetId, String>,
    pub running: bool,
    pub result: Option<Result<GameSummary, String>>,
}

/// What an export wrote, and what the runtime check found.
pub struct GameSummary {
    pub manifest: PathBuf,
    /// One line per asset.
    pub assets: Vec<String>,
    pub removed: Vec<String>,
    pub deleted: usize,
    pub licenses: LicenseReport,
    pub passed: bool,
    /// Check lines: passed, name, detail.
    pub checks: Vec<(bool, String, String)>,
}

pub const SELECTIONS: [(Selection, &str); 4] = [
    (Selection::RandomNoRepeat, "Random, no repeats"),
    (Selection::Random, "Random"),
    (Selection::Shuffle, "Shuffle"),
    (Selection::RoundRobin, "In turn"),
];

/// A coalescing key per asset and field, so a drag is one undo step.
fn key(id: AssetId, field: &str) -> u64 {
    let mut h = DefaultHasher::new();
    id.hash(&mut h);
    field.hash(&mut h);
    h.finish()
}

/// `base`, or `base_2`, `base_3`... whichever no game asset uses.
pub fn unique_asset_name(project: &Project, base: &str) -> String {
    let base = asset_name(base);
    // Short enough for the suffix too; asset names are ASCII.
    let fit = |room: usize| {
        let end = base.len().min(MAX_ASSET_NAME - room);
        base[..end].trim_end_matches('_').to_string()
    };
    let taken = |n: &str| project.game.find(n).is_some();
    let first = fit(0);
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|i| {
            let suffix = format!("_{i}");
            fit(suffix.len()) + &suffix
        })
        .find(|n| !taken(n))
        .unwrap_or(first)
}

/// One variation per bar of the loop region, or per beat when it is a bar
/// or shorter.
pub fn default_count(project: &Project, range: TimeRange) -> usize {
    let bar = project.ticks_per_bar().max(1);
    let beat = (bar / Ticks::from(project.time_signature.numerator.max(1))).max(1);
    let unit = if range.length() <= bar { beat } else { bar };
    ((range.length() + unit / 2) / unit).clamp(1, MAX_SFX_VARIATIONS as Ticks) as usize
}

/// Adds a loop over the loop region.
pub fn add_loop(session: &Session) -> ActionResult<AssetId> {
    let project = session.project();
    let asset = GameAsset {
        id: AssetId::new(),
        name: unique_asset_name(&project, &format!("{}_music", project.name)),
        category: Category::Music,
        tags: Vec::new(),
        kind: GameAssetKind::Loop {
            range: project.loop_region,
            intro_start: None,
        },
    };
    add(session, asset)
}

/// Adds an SFX set from the loop region, split per bar or per beat.
pub fn add_sfx(session: &Session) -> ActionResult<AssetId> {
    let project = session.project();
    let range = project.loop_region;
    let asset = GameAsset {
        id: AssetId::new(),
        name: unique_asset_name(&project, &format!("{}_sfx", project.name)),
        category: Category::Sfx,
        tags: Vec::new(),
        kind: GameAssetKind::sfx(range.split(default_count(&project, range))),
    };
    add(session, asset)
}

fn add(session: &Session, asset: GameAsset) -> ActionResult<AssetId> {
    let id = asset.id;
    let label = format!("Add game asset {}", asset.name);
    apply(
        session,
        Transaction::new(
            label,
            Origin::User,
            vec![Command::AddGameAsset {
                asset: Box::new(asset),
                index: None,
            }],
        ),
        None,
    )?;
    Ok(id)
}

/// Replaces an asset, merging with earlier changes under the same key.
pub fn update(session: &Session, asset: GameAsset, key: Option<u64>) -> ActionResult<u64> {
    let label = format!("Edit game asset {}", asset.name);
    apply(
        session,
        Transaction::new(
            label,
            Origin::User,
            vec![Command::UpdateGameAsset {
                asset: Box::new(asset),
            }],
        ),
        key,
    )
}

pub fn remove(session: &Session, id: AssetId) -> ActionResult<u64> {
    let project = session.project();
    let name = project
        .game
        .asset(id)
        .map_or_else(String::new, |a| a.name.clone());
    apply(
        session,
        Transaction::new(
            format!("Remove game asset {name}"),
            Origin::User,
            vec![Command::RemoveGameAsset { id }],
        ),
        None,
    )
}

impl App {
    pub fn open_game_assets(&mut self) {
        let game = &self.view.project.game;
        self.dialogs.game = Some(GameDialog {
            folder: game.export_dir.clone().unwrap_or_default(),
            bits: game.bits,
            names: HashMap::new(),
            running: false,
            result: None,
        });
    }

    /// Saves the folder and format into the project if they changed, then
    /// exports every game asset on a thread and checks the result in the
    /// runtime.
    pub fn start_game_export(&mut self, ctx: &Context) {
        let game = self.view.project.game.clone();
        let Some(d) = self.dialogs.game.as_mut() else {
            return;
        };
        let folder = Some(d.folder.trim().to_string()).filter(|f| !f.is_empty());
        if folder != game.export_dir || d.bits != game.bits {
            let r = apply(
                &self.session,
                Transaction::single(Command::SetGameExport {
                    export_dir: folder,
                    bits: d.bits,
                }),
                None,
            );
            if let Err(e) = r {
                d.result = Some(Err(e));
                return;
            }
        }
        if game.assets.is_empty() {
            d.result = Some(Err(
                "Add a loop or an SFX set first: set the loop region over the bars, then \
                 use the buttons above"
                    .into(),
            ));
            return;
        }
        if let Err(e) = self.session.game_export_dir() {
            d.result = Some(Err(format!(
                "{e}. Save the project, or type a full folder path"
            )));
            return;
        }
        d.running = true;
        d.result = None;
        let session = self.session.clone();
        self.jobs
            .spawn(ctx, Some("Exporting game assets".into()), move || {
                JobOutput::GameExported(export(&session))
            });
        self.refresh();
    }
}

fn export(session: &Session) -> Result<GameSummary, String> {
    let out = session
        .export_game(&GameExportOptions::default())
        .map_err(|e| e.to_string())?;
    let assets = out
        .assets
        .iter()
        .map(|a| {
            let files = a.asset.files().count();
            let what = match &a.asset {
                Asset::Loop(_) => "loop".to_string(),
                Asset::Sfx(_) => format!("{files} variation(s)"),
            };
            let seam = a
                .seam
                .as_ref()
                .map(|s| format!(", seam {}", s.verdict))
                .unwrap_or_default();
            format!("{}: {what}{seam}", a.asset.name())
        })
        .collect();
    let check = sky_check::check_game(&sky_check::GameOptions {
        dir: out.dir.clone(),
        seconds: 0.0,
        ..Default::default()
    })?;
    Ok(GameSummary {
        manifest: out.manifest,
        assets,
        removed: out.removed,
        deleted: out.deleted.len(),
        licenses: out.licenses,
        passed: check.passed,
        checks: check
            .checks
            .into_iter()
            .map(|c| (c.passed, c.name.to_string(), c.detail))
            .collect(),
    })
}

/// The bars a range covers, as "bar 1 to 5".
fn bars(project: &Project, range: TimeRange) -> String {
    let b = |t: Ticks| {
        let bar = project.ticks_to_bar(t);
        if bar.fract().abs() < 1e-9 {
            format!("{bar:.0}")
        } else {
            format!("{bar:.2}")
        }
    };
    format!("bar {} to {}", b(range.start), b(range.end))
}

/// What the window asked for in one frame.
#[derive(Default)]
struct Wanted {
    update: Option<(GameAsset, Option<u64>)>,
    remove: Option<AssetId>,
    add_loop: bool,
    add_sfx: bool,
    browse: bool,
    export: bool,
}

/// Shows the window if it is open.
pub fn window(app: &mut App, ctx: &Context) {
    if app.dialogs.game.is_none() {
        return;
    }
    let project = app.view.project.clone();
    let has_path = app.view.path.is_some();
    let mut open = true;
    let mut w = Wanted::default();
    Window::new("Game assets")
        .id(Id::new("game_assets"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(620.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            let Some(d) = app.dialogs.game.as_mut() else {
                return;
            };
            ui.label(
                RichText::new(
                    "Loops and sound effects this project renders for a game. Export writes \
                     them with skyware-manifest.json, which sky-runtime loads.",
                )
                .small()
                .color(theme::TEXT_WEAK),
            );
            ui.add_space(4.0);
            ui.add_enabled_ui(!d.running, |ui| {
                Grid::new("game_export_grid")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Folder");
                        ui.horizontal(|ui| {
                            let hint = if has_path {
                                "renders/export in the project"
                            } else {
                                "Choose a folder"
                            };
                            ui.add(
                                TextEdit::singleline(&mut d.folder)
                                    .desired_width(380.0)
                                    .hint_text(hint),
                            )
                            .on_hover_text(
                                "Such as your game's assets/audio folder. A relative path is \
                                 relative to the project folder.",
                            );
                            if ui.button("Browse…").clicked() {
                                w.browse = true;
                            }
                        });
                        ui.end_row();
                        ui.label("Format");
                        bits_choice(ui, &mut d.bits);
                        ui.end_row();
                    });
                ui.separator();
                if project.game.assets.is_empty() {
                    ui.label(
                        RichText::new(
                            "No game assets yet. Set the loop region over the bars, then add \
                             a loop or an SFX set from it.",
                        )
                        .color(theme::TEXT_WEAK),
                    );
                }
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for asset in &project.game.assets {
                            asset_row(ui, &project, asset, &mut d.names, &mut w);
                        }
                    });
                ui.horizontal(|ui| {
                    let region = bars(&project, project.loop_region);
                    if ui
                        .button("Add loop")
                        .on_hover_text(format!("A loop over the loop region ({region})"))
                        .clicked()
                    {
                        w.add_loop = true;
                    }
                    if ui
                        .button("Add SFX set")
                        .on_hover_text(format!(
                            "Variations from the loop region ({region}), one per bar, or one \
                             per beat for a bar or less"
                        ))
                        .clicked()
                    {
                        w.add_sfx = true;
                    }
                });
            });
            ui.add_space(6.0);
            result_ui(ui, d);
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if d.running {
                    ui.spinner();
                    ui.label(RichText::new("Rendering…").color(theme::TEXT_WEAK));
                } else if ui
                    .add_enabled(
                        !project.game.assets.is_empty(),
                        egui::Button::new(RichText::new("Export").strong()),
                    )
                    .clicked()
                {
                    w.export = true;
                }
            });
        });
    if !open && !app.dialogs.game.as_ref().is_some_and(|d| d.running) {
        app.dialogs.game = None;
        return;
    }
    if let Some((asset, key)) = w.update {
        let r = update(&app.session, asset, key);
        app.report(r);
    }
    if let Some(id) = w.remove {
        let r = remove(&app.session, id);
        app.report(r);
    }
    if w.add_loop {
        let r = add_loop(&app.session);
        app.report(r);
    }
    if w.add_sfx {
        let r = add_sfx(&app.session);
        app.report(r);
    }
    if w.browse {
        app.request_path(ctx, PathPurpose::GameFolder);
    }
    if w.export {
        app.start_game_export(ctx);
    }
    app.refresh();
}

fn result_ui(ui: &mut Ui, d: &GameDialog) {
    match &d.result {
        Some(Ok(s)) => {
            license_report_ui(ui, &s.licenses, "These credits went into the manifest:");
            let (text, color) = if s.passed {
                ("Exported and checked in sky-runtime", theme::OK)
            } else {
                ("Exported, but the runtime check failed", theme::ERROR)
            };
            ui.label(RichText::new(text).strong().color(color));
            ui.label(RichText::new(s.manifest.display().to_string()).monospace());
            for line in &s.assets {
                ui.label(RichText::new(line).small());
            }
            if !s.removed.is_empty() {
                ui.label(
                    RichText::new(format!("Removed from the folder: {}", s.removed.join(", ")))
                        .small()
                        .color(theme::TEXT_WEAK),
                );
            }
            if s.deleted > 0 {
                ui.label(
                    RichText::new(format!("Deleted {} old file(s)", s.deleted))
                        .small()
                        .color(theme::TEXT_WEAK),
                );
            }
            for (passed, name, detail) in s.checks.iter().filter(|c| !c.0 || !s.passed) {
                let c = if *passed { theme::OK } else { theme::ERROR };
                ui.label(RichText::new(format!("{name}: {detail}")).small().color(c));
            }
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(theme::ERROR));
        }
        None => {}
    }
}

/// One asset: its name, kind, category and settings.
fn asset_row(
    ui: &mut Ui,
    project: &Project,
    asset: &GameAsset,
    names: &mut HashMap<AssetId, String>,
    w: &mut Wanted,
) {
    let id = asset.id;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let name = names.entry(id).or_insert_with(|| asset.name.clone());
            let field = ui.add(
                TextEdit::singleline(name)
                    .desired_width(200.0)
                    .id(Id::new(("game_asset_name", id))),
            );
            if field.lost_focus() {
                let typed = name.trim().to_string();
                if typed != asset.name {
                    let mut a = asset.clone();
                    a.name = typed;
                    w.update = Some((a, None));
                }
                names.remove(&id);
            } else if !field.has_focus() && *name != asset.name {
                // Changed elsewhere (undo, MCP): show the project's name.
                *name = asset.name.clone();
            }
            ui.label(
                RichText::new(match asset.kind {
                    GameAssetKind::Loop { .. } => "Loop",
                    GameAssetKind::Sfx { .. } => "SFX set",
                })
                .color(theme::TEXT_WEAK),
            );
            let mut category = asset.category;
            let current = CATEGORIES
                .iter()
                .find(|(c, _)| *c == category)
                .map_or("Music", |(_, l)| *l);
            ComboBox::from_id_salt(("game_asset_category", id))
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (c, label) in CATEGORIES {
                        ui.selectable_value(&mut category, c, label);
                    }
                });
            if category != asset.category {
                let mut a = asset.clone();
                a.category = category;
                w.update = Some((a, None));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Remove").clicked() {
                    w.remove = Some(id);
                }
            });
        });
        match &asset.kind {
            GameAssetKind::Loop { range, intro_start } => {
                loop_settings(ui, project, asset, *range, *intro_start, w);
            }
            GameAssetKind::Sfx { .. } => sfx_settings(ui, project, asset, w),
        }
    });
}

fn loop_settings(
    ui: &mut Ui,
    project: &Project,
    asset: &GameAsset,
    range: TimeRange,
    intro_start: Option<Ticks>,
    w: &mut Wanted,
) {
    let bar = project.ticks_per_bar().max(1) as f64;
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("Body: {}", bars(project, range)));
        if range != project.loop_region
            && ui
                .small_button("Use loop region")
                .on_hover_text(bars(project, project.loop_region))
                .clicked()
        {
            let mut a = asset.clone();
            let intro = intro_start.filter(|i| *i < project.loop_region.start);
            a.kind = GameAssetKind::Loop {
                range: project.loop_region,
                intro_start: intro,
            };
            w.update = Some((a, None));
        }
        ui.separator();
        ui.label("Intro bars");
        let max = range.start as f64 / bar;
        let mut intro = intro_start.map_or(0.0, |i| (range.start - i) as f64 / bar);
        let r = ui
            .add_enabled(
                max > 0.0,
                DragValue::new(&mut intro)
                    .range(0.0..=max)
                    .speed(0.05)
                    .max_decimals(2),
            )
            .on_hover_text(
                "Bars before the body that play once when the loop starts. The loop must \
                 start after bar 1 to have an intro.",
            );
        if r.changed() {
            let ticks = (intro * bar).round() as Ticks;
            let mut a = asset.clone();
            a.kind = GameAssetKind::Loop {
                range,
                intro_start: (ticks > 0).then(|| (range.start - ticks).max(0)),
            };
            w.update = Some((a, Some(key(asset.id, "intro"))));
        }
    });
}

fn sfx_settings(ui: &mut Ui, project: &Project, asset: &GameAsset, w: &mut Wanted) {
    let GameAssetKind::Sfx {
        variations,
        selection,
        pitch_semitones,
        gain_db,
        tail_seconds,
        mono,
    } = &asset.kind
    else {
        return;
    };
    let span = TimeRange::covering(variations).unwrap_or(project.loop_region);
    let with = |kind: GameAssetKind| {
        let mut a = asset.clone();
        a.kind = kind;
        a
    };
    let rebuild = |variations: Vec<TimeRange>,
                   selection: Selection,
                   pitch: RandomRange,
                   gain: RandomRange,
                   tail: f32,
                   mono: bool| GameAssetKind::Sfx {
        variations,
        selection,
        pitch_semitones: pitch,
        gain_db: gain,
        tail_seconds: tail,
        mono,
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("From {}", bars(project, span)));
        if span != project.loop_region
            && ui
                .small_button("Use loop region")
                .on_hover_text(bars(project, project.loop_region))
                .clicked()
        {
            let region = project.loop_region;
            let kind = rebuild(
                region.split(variations.len()),
                *selection,
                *pitch_semitones,
                *gain_db,
                *tail_seconds,
                *mono,
            );
            w.update = Some((with(kind), None));
        }
        ui.separator();
        ui.label("Variations");
        let mut count = variations.len();
        if ui
            .add(DragValue::new(&mut count).range(1..=MAX_SFX_VARIATIONS))
            .on_hover_text("The range is split into this many equal parts, one file each")
            .changed()
        {
            let kind = rebuild(
                span.split(count),
                *selection,
                *pitch_semitones,
                *gain_db,
                *tail_seconds,
                *mono,
            );
            w.update = Some((with(kind), Some(key(asset.id, "count"))));
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Pick");
        let mut sel = *selection;
        let current = SELECTIONS
            .iter()
            .find(|(s, _)| *s == sel)
            .map_or("", |(_, l)| *l);
        ComboBox::from_id_salt(("game_asset_selection", asset.id))
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (s, label) in SELECTIONS {
                    ui.selectable_value(&mut sel, s, label);
                }
            });
        if sel != *selection {
            let kind = rebuild(
                variations.clone(),
                sel,
                *pitch_semitones,
                *gain_db,
                *tail_seconds,
                *mono,
            );
            w.update = Some((with(kind), None));
        }
        ui.label("Pitch ±");
        // The model allows uneven ranges, which these show as their widest
        // side; only a change made here writes the even range back.
        let shown = pitch_semitones
            .max
            .abs()
            .max(pitch_semitones.min.abs())
            .min(MAX_SFX_PITCH_SEMITONES);
        let mut pitch = shown;
        if ui
            .add(
                DragValue::new(&mut pitch)
                    .range(0.0..=MAX_SFX_PITCH_SEMITONES)
                    .speed(0.05)
                    .suffix(" st")
                    .max_decimals(2),
            )
            .on_hover_text("A random pitch offset per trigger, up to this many semitones")
            .changed()
            && pitch != shown
        {
            let kind = rebuild(
                variations.clone(),
                *selection,
                RandomRange::new(-pitch, pitch),
                *gain_db,
                *tail_seconds,
                *mono,
            );
            w.update = Some((with(kind), Some(key(asset.id, "pitch"))));
        }
        ui.label("Gain −");
        let shown = (-gain_db.min).clamp(0.0, -SFX_GAIN_DB.0);
        let mut gain = shown;
        if ui
            .add(
                DragValue::new(&mut gain)
                    .range(0.0..=-SFX_GAIN_DB.0)
                    .speed(0.1)
                    .suffix(" dB")
                    .max_decimals(1),
            )
            .on_hover_text("A random cut per trigger, up to this many dB")
            .changed()
            && gain != shown
        {
            let kind = rebuild(
                variations.clone(),
                *selection,
                *pitch_semitones,
                RandomRange::new(-gain, 0.0),
                *tail_seconds,
                *mono,
            );
            w.update = Some((with(kind), Some(key(asset.id, "gain"))));
        }
        ui.label("Tail");
        let mut tail = *tail_seconds;
        if ui
            .add(
                DragValue::new(&mut tail)
                    .range(0.0..=MAX_SFX_TAIL_SECONDS)
                    .speed(0.02)
                    .suffix(" s")
                    .max_decimals(2),
            )
            .on_hover_text("Rendered after each variation so it rings out; silence is trimmed")
            .changed()
        {
            let kind = rebuild(
                variations.clone(),
                *selection,
                *pitch_semitones,
                *gain_db,
                tail,
                *mono,
            );
            w.update = Some((with(kind), Some(key(asset.id, "tail"))));
        }
        let mut m = *mono;
        if ui
            .checkbox(&mut m, "Mono")
            .on_hover_text("For sounds the game positions itself")
            .changed()
        {
            let kind = rebuild(
                variations.clone(),
                *selection,
                *pitch_semitones,
                *gain_db,
                *tail_seconds,
                m,
            );
            w.update = Some((with(kind), None));
        }
    });
}
