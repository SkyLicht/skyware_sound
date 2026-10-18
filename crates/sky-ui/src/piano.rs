// SPDX-License-Identifier: GPL-3.0-or-later
//! The piano section of the Instrument tab: which recorded piano a track
//! plays, downloading one that is not here yet, and what the loaded one
//! holds.

use egui::{ComboBox, RichText, Ui};
use sky_core::TrackId;
use sky_dsp::InstrumentType;
use sky_library::{DEFAULT_PIANO, find_instrument};
use sky_model::{InstrumentSlot, Origin};
use sky_session::LibrarySet;

use crate::app::{App, Tab};
use crate::jobs::JobOutput;
use crate::theme;

/// The status bar label while a track's piano is being changed.
fn job_label(name: &str) -> String {
    format!("Getting the {name} piano")
}

/// The library row: a choice of pianos and a line about the one loaded.
pub fn library_row(app: &mut App, ui: &mut Ui, track: TrackId, slot: &InstrumentSlot) {
    let current = slot
        .library
        .as_ref()
        .map_or(DEFAULT_PIANO, |l| l.id.as_str());
    let current_name = find_instrument(current).map_or_else(
        || {
            slot.library
                .as_ref()
                .map_or("?", |l| l.name.as_str())
                .to_string()
        },
        |i| i.name.to_string(),
    );
    let busy = app
        .jobs
        .busy_labels()
        .any(|l| l.starts_with("Getting the ") && l.ends_with(" piano"));
    let loaded = app.session.track_library(track);
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.label("Piano");
        ui.add_enabled_ui(!busy, |ui| {
            ComboBox::from_id_salt(("piano_library", track))
                .selected_text(&current_name)
                .width(220.0)
                .show_ui(ui, |ui| {
                    for status in app.session.instrument_libraries() {
                        let info = status.info;
                        if info.kind != InstrumentType::Piano {
                            continue;
                        }
                        let text = if status.installed {
                            info.name.to_string()
                        } else {
                            format!("{} (download {:.0} MB)", info.name, info.size as f64 / 1e6)
                        };
                        let hover = format!("{}\n{}", info.description, info.license.label());
                        if ui
                            .selectable_label(info.id == current, text)
                            .on_hover_text(hover)
                            .clicked()
                            // The current one again downloads it when this
                            // computer does not have it yet.
                            && (info.id != current || loaded.is_none())
                        {
                            chosen = Some(info);
                        }
                    }
                });
        });
        let line = match &loaded {
            Some(lib) => {
                let (low, high) = lib.key_range().unwrap_or((0, 0));
                let layers = (low..=high).map(|k| lib.layers(k)).max().unwrap_or(0);
                format!(
                    "{} · {} velocity layer{} · {} · {:.0} MB",
                    slot.library
                        .as_ref()
                        .map_or(sky_model::License::Cc0, |l| l.license)
                        .label(),
                    layers,
                    if layers == 1 { "" } else { "s" },
                    if lib.is_stereo() { "stereo" } else { "mono" },
                    lib.bytes() as f64 / 1e6,
                )
            }
            None => match app.session.track_library_error(track) {
                Some(_)
                    if find_instrument(current)
                        .is_some_and(|i| !app.session.instruments().is_installed(i)) =>
                {
                    "Not on this computer yet: choose it above to download it".to_string()
                }
                Some(e) => format!("Cannot load: {e}"),
                None => "Loading…".to_string(),
            },
        };
        let line = if busy {
            "Getting the new piano…".to_string()
        } else {
            line
        };
        ui.label(RichText::new(line).small().color(theme::TEXT_DIM));
    });
    if let Some(info) = chosen {
        let session = app.session.clone();
        let id = info.id;
        app.jobs
            .spawn(ui.ctx(), Some(job_label(info.name)), move || {
                JobOutput::PianoLibrary(
                    session
                        .set_piano_library(track, id, true, Origin::User)
                        .map_err(|e| e.to_string()),
                )
            });
    }
}

impl App {
    /// A track's piano changed, or could not.
    pub fn piano_library_set(&mut self, result: Result<LibrarySet, String>) {
        self.refresh();
        match result {
            Ok(set) => {
                if self.view.project.track(set.track).is_ok() {
                    self.sel.select_track(set.track);
                    self.workspace.show(Tab::Instrument);
                }
                let how = if set.downloaded {
                    "Downloaded and loaded"
                } else {
                    "Loaded"
                };
                self.info(format!("{how} {}", set.library));
            }
            Err(e) => self.error(format!("Changing the piano failed: {e}")),
        }
    }
}
