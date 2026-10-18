// SPDX-License-Identifier: GPL-3.0-or-later
//! Sample banks: the Load sounds dialog, which reads a Strudel sample pack
//! and loads a bank or some of its sounds into a track; the bank section of
//! the Instrument tab; and the license notes the export dialogs show.

use std::collections::BTreeSet;
use std::sync::Arc;

use egui::{
    Align, Color32, ComboBox, Context, Grid, Id, Key, Label, Layout, Modal, RichText, ScrollArea,
    Sense, TextEdit, Ui,
};
use sky_core::TrackId;
use sky_dsp::InstrumentType;
use sky_library::{MAX_FILES, MapFile, PACKS, Pack, PackInfo, PackSource, packs};
use sky_model::{
    Command, InstrumentSlot, License, LicenseReport, Origin, Project, SampleBank, Track, TrackKind,
    Transaction,
};
use sky_session::{BankLoaded, BankRequest};

use crate::actions;
use crate::app::{App, Tab};
use crate::audition::PreviewKey;
use crate::jobs::JobOutput;
use crate::theme;

/// The pack the dialog starts with: Strudel's drum machines.
const DEFAULT_PACK: &str = "tidal-drum-machines";
/// The bank it starts with in a pack that has only banks.
const DEFAULT_BANK: &str = "RolandTR808";
const LOAD_TIP: &str = "Load a bank or some sounds from a Strudel sample pack";

/// A pack's sample map, read for the dialog.
pub struct Listing {
    /// The map address it was read for.
    pub map: String,
    pub pack: Result<Arc<Pack>, String>,
    /// Its banks, with how many sounds each has.
    pub banks: Vec<(String, usize)>,
    /// How many sounds belong to no bank.
    pub plain: usize,
}

pub struct LoadSoundsDialog {
    /// The track to load into; `None` adds a sample-bank track.
    pub track: Option<TrackId>,
    /// One of [`PACKS`], or `None` for another sample map or folder.
    pub pack: Option<usize>,
    /// The sample map's address or folder, when `pack` is `None`.
    pub address: String,
    /// What the person loading another pack says its license is.
    pub license: License,
    /// The bank to load from; `None` offers the sounds in no bank.
    pub bank: Option<String>,
    /// The sounds to load; with none chosen, all that are offered load.
    pub chosen: BTreeSet<String>,
    /// Add to the track's sounds instead of replacing them.
    pub add: bool,
    /// Play each sound as it is clicked.
    pub hear: bool,
    pub listing: Option<Listing>,
    /// The map address being read now.
    pub reading: Option<String>,
    pub running: bool,
    pub result: Option<Result<BankLoaded, String>>,
}

impl LoadSoundsDialog {
    /// The pack as chosen now. A typed address that names a known pack
    /// keeps that pack's license.
    pub fn source(&self) -> Option<PackSource> {
        if let Some(p) = self.pack.and_then(|i| PACKS.get(i)) {
            return Some(PackSource::from(p));
        }
        // Strudel's samples('…') line, pasted, names its address.
        let text = sky_library::samples::address_in(&self.address);
        if text.is_empty() {
            return None;
        }
        let known = packs::find(&text).is_some();
        Some(PackSource::parse(&text, (!known).then_some(self.license)))
    }

    /// The listing of the pack as chosen now, once read.
    fn current(&self) -> Option<&Listing> {
        let map = self.source()?.map;
        self.listing.as_ref().filter(|l| l.map == map)
    }

    /// The pack as read, when the chosen one is.
    pub fn listed_pack(&self) -> Option<Arc<Pack>> {
        self.current()?.pack.as_ref().ok().cloned()
    }

    /// The files of a sound on offer.
    pub fn files_of(&self, name: &str) -> Option<Vec<MapFile>> {
        let Some(Ok(pack)) = self.current().map(|l| &l.pack) else {
            return None;
        };
        let sounds = match &self.bank {
            Some(b) => pack.map.bank_sounds(Some(b)),
            None => pack.map.plain_sounds(),
        };
        sounds.get(name).map(|f| f.to_vec())
    }

    /// The sounds on offer from the chosen bank, with their file counts.
    pub fn offered(&self) -> Vec<(String, usize)> {
        let Some(Ok(pack)) = self.current().map(|l| &l.pack) else {
            return Vec::new();
        };
        let sounds = match &self.bank {
            Some(b) => pack.map.bank_sounds(Some(b)),
            None => pack.map.plain_sounds(),
        };
        sounds.into_iter().map(|(n, f)| (n, f.len())).collect()
    }

    fn set_listing(&mut self, map: String, pack: Result<Arc<Pack>, String>) {
        let (banks, plain) = match &pack {
            Ok(p) => (
                p.map.banks().into_iter().collect::<Vec<_>>(),
                p.map.plain_sounds().len(),
            ),
            Err(_) => (Vec::new(), 0),
        };
        if let Some(b) = &self.bank {
            match banks.iter().find(|(n, _)| n.eq_ignore_ascii_case(b)) {
                Some((n, _)) => self.bank = Some(n.clone()),
                None => self.bank = None,
            }
        }
        if self.bank.is_none() && plain == 0 {
            self.bank = banks
                .iter()
                .find(|(n, _)| n == DEFAULT_BANK)
                .or(banks.first())
                .map(|(n, _)| n.clone());
        }
        self.listing = Some(Listing {
            map,
            pack,
            banks,
            plain,
        });
        let offered: BTreeSet<String> = self.offered().into_iter().map(|(n, _)| n).collect();
        self.chosen.retain(|n| offered.contains(n));
    }
}

/// The track's sample bank, when it has sounds.
pub fn bank_of(track: &Track) -> Option<&SampleBank> {
    track
        .instrument
        .as_ref()
        .filter(|i| i.kind == InstrumentType::Bank)
        .and_then(|i| i.bank.as_ref())
        .filter(|b| !b.sounds.is_empty())
}

fn is_bank_track(track: &Track) -> bool {
    track
        .instrument
        .as_ref()
        .is_some_and(|i| i.kind == InstrumentType::Bank)
}

/// The packs a bank's sounds came from, with their licenses and credits.
fn packs_of(project: &Project, bank: &SampleBank) -> Vec<(String, License, Option<String>)> {
    let mut out: Vec<(String, License, Option<String>)> = Vec::new();
    // Every variation of a sound comes from the same load.
    for samples in bank.sounds.values() {
        let Some(s) = samples
            .first()
            .and_then(|s| project.pool_entry(s.audio).ok())
            .and_then(|e| e.source.as_ref())
        else {
            continue;
        };
        if !out.iter().any(|(p, l, _)| *p == s.pack && *l == s.license) {
            out.push((s.pack.clone(), s.license, s.credit.clone()));
        }
    }
    out
}

/// A pattern that plays a bank: a beat when it has drums, the pitched
/// sound's notes, or else its first few sounds in turn.
pub fn starter_pattern(bank: &SampleBank) -> String {
    if bank.pitched.is_some() {
        return "c3 e3 g3 e3".into();
    }
    let layers: Vec<&str> = [("bd", "bd*4"), ("sd", "~ sd ~ sd"), ("hh", "hh*8")]
        .into_iter()
        .filter(|(name, _)| bank.resolve(name).is_some())
        .map(|(_, layer)| layer)
        .collect();
    if layers.len() >= 2 {
        return layers.join(", ");
    }
    // Names a pattern reads as sounds, not as notes or rests.
    let names: Vec<&str> = bank
        .sounds
        .keys()
        .map(String::as_str)
        .filter(|n| {
            n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && !n.starts_with('_')
                && sky_pattern::note_number(n).is_none()
        })
        .take(4)
        .collect();
    if names.is_empty() {
        "bd*4".into()
    } else {
        names.join(" ")
    }
}

/// The color a license is shown in: free, credit only, or a condition to check.
pub fn license_color(license: License) -> Color32 {
    match license {
        License::Cc0 | License::PublicDomain => theme::OK,
        License::Unknown | License::CcBySa4 => theme::WARN,
        License::Mit | License::CcBy3 | License::CcBy4 => theme::TEXT_WEAK,
    }
}

/// What a license means for a game, in a sentence or two.
pub fn license_note(license: License) -> &'static str {
    match license {
        License::Cc0 | License::PublicDomain => "Free to use in games, with no conditions.",
        License::Unknown => {
            "The pack states no license. Sketch with it, but find out what its authors allow \
             before it ships in a game; exports warn about it."
        }
        License::CcBySa4 => {
            "Free to use with credit, but what you make with it must be shared under the same \
             license. Loop exports write the credit into the manifest and warn."
        }
        License::Mit | License::CcBy3 | License::CcBy4 => {
            "Free to use with credit. Loop exports write the credit into the game's manifest."
        }
    }
}

/// The credits and warnings for the audio an export contains.
pub fn license_report_ui(ui: &mut Ui, report: &LicenseReport, credits_go: &str) {
    for w in &report.warnings {
        ui.label(RichText::new(w).small().color(theme::WARN));
    }
    if !report.credits.is_empty() {
        ui.label(RichText::new(credits_go).small().color(theme::TEXT_WEAK));
        for c in &report.credits {
            ui.label(RichText::new(c).small().color(theme::TEXT));
        }
    }
}

impl App {
    /// Opens the Load sounds dialog for `track`, or for a new track.
    pub fn open_load_sounds(&mut self, track: Option<TrackId>) {
        let project = self.view.project.clone();
        let track = track.and_then(|t| project.track(t).ok());
        let target = track.filter(|t| t.kind == TrackKind::Instrument);
        // Start where the track's sounds came from.
        let bank = target.and_then(bank_of);
        let pack = bank
            .and_then(|b| packs_of(&project, b).into_iter().next())
            .and_then(|(name, _, _)| PACKS.iter().position(|p| p.name == name))
            .or_else(|| PACKS.iter().position(|p| p.id == DEFAULT_PACK));
        self.dialogs.load_sounds = Some(LoadSoundsDialog {
            track: target.map(|t| t.id),
            pack,
            address: String::new(),
            license: License::Unknown,
            bank: bank.map(|b| b.name.clone()),
            chosen: BTreeSet::new(),
            add: false,
            hear: true,
            listing: None,
            reading: None,
            running: false,
            result: None,
        });
    }

    /// Opens Load sounds on one of the known packs, and a bank of it, for
    /// the selected sample bank or a new one.
    pub fn open_load_sounds_on(&mut self, ctx: &Context, info: &PackInfo, bank: Option<&str>) {
        self.run_action(ctx, crate::app::Action::LoadSounds);
        let read = self.browser.listing(info.map).map(|l| l.pack.clone());
        let Some(d) = self.dialogs.load_sounds.as_mut() else {
            return;
        };
        d.pack = PACKS.iter().position(|p| p.id == info.id);
        d.address.clear();
        d.bank = bank.map(str::to_string);
        d.chosen.clear();
        // The browser has read it already: no need to read it again.
        if let Some(pack) = read {
            d.set_listing(info.map.to_string(), Ok(pack));
        }
    }

    /// Reads the chosen pack's sample map in the background.
    pub fn read_pack(&mut self, ctx: &Context) {
        let Some(d) = self.dialogs.load_sounds.as_mut() else {
            return;
        };
        let Some(source) = d.source() else {
            d.result = Some(Err(
                "Type the address of a strudel.json, or choose the folder that has one".into(),
            ));
            return;
        };
        let map = source.map.clone();
        d.reading = Some(map.clone());
        d.result = None;
        let library = self.session.library();
        self.jobs
            .spawn(ctx, Some(format!("Reading {}", source.name)), move || {
                JobOutput::PackListed {
                    result: library
                        .open(&source)
                        .map(Arc::new)
                        .map_err(|e| e.to_string()),
                    map,
                }
            });
    }

    pub fn pack_listed(&mut self, map: String, result: Result<Arc<Pack>, String>) {
        if let Some(d) = self.dialogs.load_sounds.as_mut()
            && d.reading.as_deref() == Some(map.as_str())
        {
            d.reading = None;
            d.set_listing(map, result);
        }
    }

    /// Loads the chosen sounds in the background, adding a track first
    /// when the dialog has none.
    pub fn start_bank_load(&mut self, ctx: &Context) {
        let project = self.view.project.clone();
        let Some(d) = self.dialogs.load_sounds.as_mut() else {
            return;
        };
        let Some(source) = d.source() else {
            d.result = Some(Err(
                "Choose a pack, or type the address of its strudel.json".into(),
            ));
            return;
        };
        let target = d.track.and_then(|t| project.track(t).ok());
        let request = BankRequest {
            source,
            bank: d.bank.clone(),
            sounds: d.chosen.iter().cloned().collect(),
            add: d.add && target.and_then(bank_of).is_some(),
            pitched: None,
        };
        let offered = d.offered();
        let name = match (&request.bank, request.sounds.as_slice(), offered.as_slice()) {
            (Some(b), _, _) => b.clone(),
            (None, [one], _) | (None, [], [(one, _)]) => one.clone(),
            _ => request.source.name.clone(),
        };
        let track = match target {
            Some(t) => t.id,
            None => {
                let track = Track::new_instrument(
                    actions::unique_track_name(&project, &name),
                    InstrumentType::Bank,
                );
                let id = track.id;
                let index = self
                    .sel
                    .track
                    .and_then(|a| project.track_index(a).ok())
                    .map(|i| i + 1);
                let r = actions::apply(
                    &self.session,
                    Transaction::user(
                        "Add sample bank track",
                        vec![Command::AddTrack {
                            track: Box::new(track),
                            index,
                        }],
                    ),
                    None,
                );
                if self.report(r).is_none() {
                    return;
                }
                self.refresh();
                self.sel.select_track(id);
                id
            }
        };
        if let Some(d) = self.dialogs.load_sounds.as_mut() {
            d.track = Some(track);
            d.running = true;
            d.result = None;
        }
        let session = self.session.clone();
        self.jobs
            .spawn(ctx, Some(format!("Loading {name} sounds")), move || {
                JobOutput::BankLoaded(
                    session
                        .load_bank(track, &request, Origin::User)
                        .map_err(|e| e.to_string()),
                )
            });
    }

    pub fn bank_loaded(&mut self, result: Result<BankLoaded, String>) {
        self.refresh();
        if let Ok(b) = &result
            && self.view.project.track(b.track).is_ok()
        {
            self.sel.select_track(b.track);
            self.workspace.show(Tab::Instrument);
        }
        match self.dialogs.load_sounds.as_mut() {
            Some(d) => {
                d.running = false;
                d.result = Some(result);
            }
            None => match result {
                Ok(b) => self.info(format!("Loaded {} sounds", b.bank)),
                Err(e) => self.error(format!("Loading sounds failed: {e}")),
            },
        }
    }

    /// Adds a four-bar pattern clip that plays the track's sounds, after
    /// its last clip or at the playhead's bar, and opens it.
    pub fn add_bank_pattern(&mut self, track: TrackId) {
        let project = self.view.project.clone();
        let Ok(t) = project.track(track) else {
            return;
        };
        let Some(bank) = bank_of(t) else {
            return;
        };
        let bar = project.ticks_per_bar().max(1);
        let playhead = self.transport.position_ticks.max(0.0) as i64;
        let after = t.clips.iter().map(|c| c.end()).max().unwrap_or(0);
        // Both are at or after zero, so this rounds up to a bar line.
        let start = (playhead.max(after) + bar - 1) / bar * bar;
        let source = starter_pattern(bank);
        let r = actions::add_pattern_clip(&self.session, track, start, bar * 4, &source);
        if let Some(id) = self.report(r) {
            self.refresh();
            self.sel.select_clip(track, id);
            self.workspace.show(Tab::Editor);
        }
    }
}

/// The Instrument tab's section for a sample bank: its sounds, where they
/// came from, and which one notes play.
pub fn bank_panel(app: &mut App, ui: &mut Ui, track: TrackId, slot: &InstrumentSlot) {
    let project = app.view.project.clone();
    let Some(bank) = slot.bank.as_ref().filter(|b| !b.sounds.is_empty()) else {
        ui.horizontal(|ui| {
            ui.label(RichText::new("No sounds yet.").color(theme::TEXT_WEAK));
            if ui.button("Load sounds…").on_hover_text(LOAD_TIP).clicked() {
                app.open_load_sounds(Some(track));
            }
        });
        if let Some(bank) = &slot.bank
            && let Some(b) = linked_packs_ui(ui, bank)
        {
            let mut s = slot.clone();
            s.bank = Some(b);
            let r = actions::set_instrument(&app.session, track, s, "Unlink a pack");
            app.report(r);
        }
        return;
    };
    let mut next: Option<(SampleBank, &str)> = None;
    let mut load = false;
    let mut pattern = false;
    let mut hear: Option<(String, usize)> = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(&bank.name).strong().color(theme::TEXT));
        ui.label(
            RichText::new(format!(
                "{} sounds, {} samples",
                bank.sounds.len(),
                bank.sample_count()
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
        if ui.button("Load sounds…").on_hover_text(LOAD_TIP).clicked() {
            load = true;
        }
        if ui
            .button("Add pattern clip")
            .on_hover_text("Add a four-bar pattern clip that plays these sounds")
            .clicked()
        {
            pattern = true;
        }
        ui.separator();
        ui.label("Notes play");
        let mut pitched = bank.pitched.clone();
        ComboBox::from_id_salt(("bank_pitched", track))
            .selected_text(pitched.as_deref().unwrap_or("Drums by note"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut pitched, None, "Drums by note")
                    .on_hover_text(
                        "Each MIDI note plays its General MIDI drum: 36 bd, 38 sd, 42 hh",
                    );
                for name in bank.sounds.keys() {
                    ui.selectable_value(&mut pitched, Some(name.clone()), name)
                        .on_hover_text(format!("Every MIDI note plays {name}, pitched"));
                }
            });
        if pitched != bank.pitched {
            let mut b = bank.clone();
            b.pitched = pitched;
            next = Some((b, "Choose the sound notes play"));
        }
    });
    if let Some(b) = linked_packs_ui(ui, bank) {
        next = Some((b, "Unlink a pack"));
    }
    for (pack, license, credit) in packs_of(&project, bank) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(license.label())
                    .small()
                    .color(license_color(license)),
            )
            .on_hover_text(license_note(license));
            ui.label(RichText::new(pack).small().color(theme::TEXT_WEAK));
            if let Some(c) = credit {
                ui.label(RichText::new(c).small().color(theme::TEXT_DIM));
            }
        });
    }
    ScrollArea::vertical()
        .id_salt(("bank_sounds", track))
        .max_height(60.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                for (name, samples) in &bank.sounds {
                    let text = match samples.len() {
                        1 => name.clone(),
                        n => format!("{name} ×{n}"),
                    };
                    let color = if bank.pitched.as_deref() == Some(name.as_str()) {
                        theme::ACCENT
                    } else {
                        theme::TEXT
                    };
                    let tip = match samples.len() {
                        1 => format!(
                            "{name} in a pattern\nClick to hear it, right-click to remove it"
                        ),
                        n => format!(
                            "{name} or {name}:0 to {name}:{} in a pattern\nClick to hear its \
                             {n} variations in turn, right-click to remove it",
                            n - 1
                        ),
                    };
                    let chip = ui
                        .add(
                            Label::new(RichText::new(text).monospace().color(color))
                                .sense(Sense::click()),
                        )
                        .on_hover_text(tip);
                    if chip.clicked() {
                        hear = Some((name.clone(), samples.len()));
                    }
                    chip.context_menu(|ui| {
                        if ui.button(format!("Remove {name}")).clicked() {
                            let mut b = bank.clone();
                            b.sounds.remove(name);
                            if b.pitched.as_deref() == Some(name.as_str()) {
                                b.pitched = None;
                            }
                            next = Some((b, "Remove a bank sound"));
                            ui.close();
                        }
                    });
                }
            });
        });
    let hint = match &bank.pitched {
        Some(p) => format!(
            "Pattern clips play these by name, as in bd sd:1. Notes and note names play {p} \
             across the keyboard."
        ),
        None => "Pattern clips play these by name, as in bd sd:1 hh. Notes play the drums on \
                 their General MIDI numbers."
            .to_string(),
    };
    ui.label(RichText::new(hint).small().color(theme::TEXT_DIM));
    if let Some((b, label)) = next {
        let mut s = slot.clone();
        s.bank = Some(b);
        let r = actions::set_instrument(&app.session, track, s, label);
        app.report(r);
    }
    if let Some((name, variations)) = hear {
        app.hear_bank_sound(track, &name, variations);
    }
    if load {
        app.open_load_sounds(Some(track));
    } else if pattern {
        app.add_bank_pattern(track);
    }
}

/// The packs linked to a bank, each with a button that unlinks it. Returns
/// the bank without the pack whose button was clicked.
fn linked_packs_ui(ui: &mut Ui, bank: &SampleBank) -> Option<SampleBank> {
    let mut unlink = None;
    for (i, link) in bank.packs.iter().enumerate() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Linked").small().color(theme::TEXT_WEAK));
            ui.label(RichText::new(&link.name).small().color(theme::TEXT))
                .on_hover_text(format!(
                    "{}\nA sound a pattern names that the bank lacks loads from here, as \
                     samples('…') does in Strudel",
                    link.map
                ));
            ui.label(
                RichText::new(link.license.label())
                    .small()
                    .color(license_color(link.license)),
            )
            .on_hover_text(license_note(link.license));
            if ui
                .small_button("Unlink")
                .on_hover_text("Patterns stop loading sounds from this pack; the sounds stay")
                .clicked()
            {
                unlink = Some(i);
            }
        });
    }
    let i = unlink?;
    let mut b = bank.clone();
    b.packs.remove(i);
    Some(b)
}

fn sound_chip(name: &str, files: usize) -> String {
    if files > 1 {
        format!("{name} ×{files}")
    } else {
        name.to_string()
    }
}

/// The Load sounds dialog.
pub fn load_sounds_dialog(app: &mut App, ctx: &Context) {
    let project = app.view.project.clone();
    let Some(d) = app.dialogs.load_sounds.as_mut() else {
        return;
    };
    let source = d.source();
    let key = source.as_ref().map(|s| s.map.clone());
    // A known pack is read as soon as it is chosen; another one when asked.
    let mut read = d.pack.is_some()
        && key.is_some()
        && d.reading != key
        && d.listing.as_ref().map(|l| &l.map) != key.as_ref();
    let reading = key.is_some() && d.reading == key;
    let (banks, plain, listed_error, offline) = match d.current() {
        Some(l) => (
            l.banks.clone(),
            l.plain,
            l.pack.as_ref().err().cloned(),
            l.pack.as_ref().is_ok_and(|p| p.offline),
        ),
        None => (Vec::new(), 0, None, false),
    };
    let listed = d.current().is_some_and(|l| l.pack.is_ok());
    let offered = d.offered();
    let all_files: usize = offered.iter().map(|(_, n)| n).sum();
    let files: usize = if d.chosen.is_empty() {
        all_files
    } else {
        offered
            .iter()
            .filter(|(n, _)| d.chosen.contains(n))
            .map(|(_, n)| n)
            .sum()
    };
    let instrument_tracks: Vec<(TrackId, String, bool)> = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Instrument)
        .map(|t| (t.id, t.name.clone(), is_bank_track(t)))
        .collect();
    let target_sounds = d
        .track
        .and_then(|t| project.track(t).ok())
        .and_then(bank_of)
        .map(|b| b.sounds.len());

    let mut browse = false;
    let mut load = false;
    let mut close = false;
    let mut add_pattern = None;
    let mut hear: Option<String> = None;
    let modal = Modal::new(Id::new("load_sounds")).show(ctx, |ui| {
        ui.set_width(620.0);
        ui.label(RichText::new("Load sounds").heading());
        ui.label(
            RichText::new(
                "Sounds from a Strudel sample pack play by name in pattern clips, as in \
                 Strudel: bd sd:1 hh. Each file is downloaded once and kept in a cache.",
            )
            .small()
            .color(theme::TEXT_WEAK),
        );
        ui.add_space(4.0);
        ui.add_enabled_ui(!d.running, |ui| {
            Grid::new("load_sounds_grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Pack");
                    ui.vertical(|ui| {
                        let before = d.pack;
                        let current = d
                            .pack
                            .and_then(|i| PACKS.get(i))
                            .map_or("Another pack…", |p| p.name);
                        ComboBox::from_id_salt("sound_pack")
                            .width(320.0)
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for (i, p) in PACKS.iter().enumerate() {
                                    ui.selectable_value(&mut d.pack, Some(i), p.name)
                                        .on_hover_text(format!(
                                            "{}\n{}",
                                            p.about,
                                            p.license.label()
                                        ));
                                }
                                ui.selectable_value(&mut d.pack, None, "Another pack…")
                                    .on_hover_text(
                                        "A strudel.json on the web, a github:user/repo \
                                         shortcut, or a folder on this computer",
                                    );
                            });
                        if d.pack != before {
                            d.bank = None;
                            d.chosen.clear();
                            d.result = None;
                        }
                        match d.pack.and_then(|i| PACKS.get(i)) {
                            Some(p) => {
                                ui.label(RichText::new(p.about).small().color(theme::TEXT_DIM));
                                ui.hyperlink_to(RichText::new(p.homepage).small(), p.homepage);
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    let r = ui.add(
                                        TextEdit::singleline(&mut d.address)
                                            .desired_width(340.0)
                                            .hint_text("https://…/strudel.json, github:user/repo or a folder")
                                            .font(theme::mono(12.0)),
                                    );
                                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                        read = true;
                                    }
                                    if ui.button("Folder…").clicked() {
                                        browse = true;
                                    }
                                    if ui.button("Open").clicked() {
                                        read = true;
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Its license").small());
                                    ComboBox::from_id_salt("sound_pack_license")
                                        .selected_text(d.license.label())
                                        .show_ui(ui, |ui| {
                                            for l in License::ALL {
                                                ui.selectable_value(&mut d.license, l, l.label());
                                            }
                                        });
                                    ui.label(
                                        RichText::new("Only you can say what its authors allow.")
                                            .small()
                                            .color(theme::TEXT_DIM),
                                    );
                                });
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("Into");
                    let into = d
                        .track
                        .and_then(|t| instrument_tracks.iter().find(|x| x.0 == t))
                        .map_or_else(|| "A new track".to_string(), |(_, n, _)| n.clone());
                    ComboBox::from_id_salt("sound_track")
                        .width(320.0)
                        .selected_text(into)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut d.track, None, "A new track");
                            for (id, name, is_bank) in &instrument_tracks {
                                let label = if *is_bank {
                                    name.clone()
                                } else {
                                    format!("{name} (becomes a sample bank)")
                                };
                                ui.selectable_value(&mut d.track, Some(*id), label);
                            }
                        });
                    ui.end_row();

                    if listed && !banks.is_empty() {
                        ui.label("Bank");
                        let before = d.bank.clone();
                        ComboBox::from_id_salt("sound_bank")
                            .width(320.0)
                            .height(320.0)
                            .selected_text(d.bank.as_deref().unwrap_or("Sounds in no bank"))
                            .show_ui(ui, |ui| {
                                if plain > 0 {
                                    ui.selectable_value(
                                        &mut d.bank,
                                        None,
                                        format!("Sounds in no bank ({plain})"),
                                    );
                                }
                                for (b, n) in &banks {
                                    ui.selectable_value(
                                        &mut d.bank,
                                        Some(b.clone()),
                                        format!("{b} ({n})"),
                                    );
                                }
                            });
                        if d.bank != before {
                            d.chosen.clear();
                        }
                        ui.end_row();
                    }
                    if listed {
                        ui.label("Sounds");
                        ui.vertical(|ui| {
                            ScrollArea::vertical()
                                .id_salt("offered_sounds")
                                .max_height(150.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        for (name, n) in &offered {
                                            let on = d.chosen.contains(name);
                                            let chip = RichText::new(sound_chip(name, *n)).monospace();
                                            if ui.add(egui::Button::selectable(on, chip)).clicked() {
                                                if on {
                                                    d.chosen.remove(name);
                                                } else {
                                                    d.chosen.insert(name.clone());
                                                }
                                                if d.hear {
                                                    hear = Some(name.clone());
                                                }
                                            }
                                        }
                                    });
                                });
                            let caption = if d.chosen.is_empty() {
                                format!(
                                    "Click sounds to choose some, or load all {} ({all_files} files).",
                                    offered.len()
                                )
                            } else {
                                format!("{} chosen, {files} files.", d.chosen.len())
                            };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(caption).small().color(theme::TEXT_WEAK));
                                ui.checkbox(&mut d.hear, RichText::new("Hear as you click").small())
                                    .on_hover_text(
                                        "Play a sound's first file when you click it; it is \
                                         downloaded once and kept for the load",
                                    );
                            });
                            if files > MAX_FILES {
                                ui.label(
                                    RichText::new(format!(
                                        "One load fetches at most {MAX_FILES} files; choose fewer sounds."
                                    ))
                                    .small()
                                    .color(theme::WARN),
                                );
                            }
                        });
                        ui.end_row();
                    }
                    if let Some(n) = target_sounds {
                        ui.label("");
                        ui.checkbox(
                            &mut d.add,
                            format!("Add to the track's {n} sounds instead of replacing them"),
                        );
                        ui.end_row();
                    }
                    if let Some(s) = &source {
                        ui.label("License");
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(s.license.label())
                                    .strong()
                                    .color(license_color(s.license)),
                            );
                            ui.label(
                                RichText::new(license_note(s.license))
                                    .small()
                                    .color(theme::TEXT_WEAK),
                            );
                            if let Some(c) = &s.credit {
                                ui.label(RichText::new(c).small().color(theme::TEXT_DIM));
                            }
                        });
                        ui.end_row();
                    }
                });
        });
        ui.add_space(6.0);
        if reading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading the pack's sample map…").color(theme::TEXT_WEAK));
            });
        }
        if let Some(e) = &listed_error {
            ui.label(RichText::new(e).color(theme::ERROR));
            if !d.running && ui.button("Try again").clicked() {
                read = true;
            }
        }
        if offline {
            ui.label(
                RichText::new(
                    "The pack could not be reached, so its saved copy is shown; only files \
                     downloaded before will load.",
                )
                .small()
                .color(theme::WARN),
            );
        }
        match &d.result {
            Some(Ok(b)) => {
                ui.label(RichText::new(format!("Loaded {}", b.bank)).strong().color(theme::OK));
                let mut names: Vec<String> = b
                    .sounds
                    .iter()
                    .take(16)
                    .map(|(n, c)| sound_chip(n, *c))
                    .collect();
                if b.sounds.len() > 16 {
                    names.push(format!("and {} more", b.sounds.len() - 16));
                }
                ui.label(RichText::new(names.join("  ")).monospace().small());
                ui.label(
                    RichText::new(format!(
                        "{} new files, {} already in the project.",
                        b.new_files, b.reused_files
                    ))
                    .small()
                    .color(theme::TEXT_WEAK),
                );
                if b.offline {
                    ui.label(
                        RichText::new("The pack could not be reached, so its saved copy was used.")
                            .small()
                            .color(theme::WARN),
                    );
                }
                if !b.failed.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "{} files could not be loaded, such as:",
                            b.failed.len()
                        ))
                        .small()
                        .color(theme::WARN),
                    );
                    for f in b.failed.iter().take(3) {
                        ui.label(RichText::new(f).small().monospace().color(theme::TEXT_WEAK));
                    }
                }
                ui.horizontal(|ui| {
                    if ui
                        .button("Add pattern clip")
                        .on_hover_text("Add a four-bar pattern clip that plays these sounds")
                        .clicked()
                    {
                        add_pattern = Some(b.track);
                    }
                    ui.label(
                        RichText::new("or type their names in a pattern clip on the track")
                            .small()
                            .color(theme::TEXT_WEAK),
                    );
                });
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(theme::ERROR));
            }
            None => {}
        }
        ui.add_space(6.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Close").clicked() {
                close = true;
            }
            if d.running {
                ui.spinner();
                ui.label(RichText::new("Loading…").color(theme::TEXT_WEAK));
            } else {
                let can = source.is_some() && !reading && files <= MAX_FILES;
                let label = if d.chosen.is_empty() {
                    "Load".to_string()
                } else {
                    format!("Load {}", d.chosen.len())
                };
                if ui
                    .add_enabled(can, egui::Button::new(RichText::new(label).strong()))
                    .clicked()
                {
                    load = true;
                }
            }
        });
    });
    let running = app.dialogs.load_sounds.as_ref().is_some_and(|d| d.running);
    if modal.should_close() && !running {
        close = true;
    }
    if let Some(name) = hear.filter(|_| !close) {
        app.start_preview(ctx, PreviewKey::PackSound(name));
    }
    if close {
        app.dialogs.load_sounds = None;
    } else if let Some(track) = add_pattern {
        app.dialogs.load_sounds = None;
        app.add_bank_pattern(track);
    } else if browse {
        app.request_path(ctx, crate::dialogs::PathPurpose::SoundPack);
    } else if load {
        app.start_bank_load(ctx);
    } else if read {
        app.read_pack(ctx);
    }
}
