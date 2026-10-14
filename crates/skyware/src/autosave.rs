// SPDX-License-Identifier: GPL-3.0-or-later
//! Autosave: every couple of minutes a project with unsaved changes is
//! copied to the autosave folder, so even a project that was never saved
//! survives a crash. The user's own project folder is never overwritten;
//! for saved projects the crash journal already keeps every change.

use std::path::PathBuf;
use std::time::Duration;

use sky_session::Session;

const INTERVAL: Duration = Duration::from_secs(120);

pub fn autosave_dir() -> Option<PathBuf> {
    crate::config::data_dir().map(|d| d.join("autosave"))
}

/// Starts the autosave thread, which runs until the process exits.
pub fn spawn(session: Session) {
    let Some(root) = autosave_dir() else {
        tracing::warn!("no folder for autosaves; autosave is off");
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("autosave".into())
        .spawn(move || {
            let mut saved_revision = session.view().revision;
            loop {
                std::thread::sleep(INTERVAL);
                let view = session.view();
                if !view.dirty || view.revision == saved_revision {
                    continue;
                }
                let stem = sky_game::file_stem(&view.project.name);
                let dir = root.join(format!(
                    "{stem}-{}.{}",
                    &view.project.id.to_string()[..8],
                    sky_model::file::EXTENSION
                ));
                match session.save_copy(&dir) {
                    Ok(()) => {
                        saved_revision = view.revision;
                        tracing::info!("autosaved to {}", dir.display());
                    }
                    Err(e) => tracing::warn!("autosave failed: {e}"),
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start autosave: {e}");
    }
}
