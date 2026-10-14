// SPDX-License-Identifier: GPL-3.0-or-later
//! The `.skyproj` folder format.
//!
//! ```text
//! MySong.skyproj/
//!   project.json    the project, versioned by `schemaVersion`
//!   journal.jsonl   commands applied since the last save (crash recovery)
//!   audio/          the audio pool
//!   renders/        bounces and exports
//!   cache/          rebuildable data such as waveform peaks
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::Command;
use crate::error::ModelError;
use crate::project::{Project, SCHEMA_VERSION};

pub const PROJECT_FILE: &str = "project.json";
pub const JOURNAL_FILE: &str = "journal.jsonl";
pub const AUDIO_DIR: &str = "audio";
pub const RENDERS_DIR: &str = "renders";
pub const CACHE_DIR: &str = "cache";
pub const EXTENSION: &str = "skyproj";

/// One step that turns a file of version `from` into version `from + 1`.
type Migration = fn(Value) -> Result<Value, ModelError>;

/// Migrations indexed by the version they upgrade from. Version 1 is the
/// first released format, so the list starts empty; each future format
/// change appends one step here and bumps [`SCHEMA_VERSION`].
const MIGRATIONS: &[(u32, Migration)] = &[];

/// Upgrades a parsed project file to the current schema.
pub fn migrate(mut value: Value) -> Result<Value, ModelError> {
    let found = value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| ModelError::File("project.json has no schemaVersion".into()))?
        as u32;
    if found > SCHEMA_VERSION {
        return Err(ModelError::NewerSchema {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    let mut version = found;
    while version < SCHEMA_VERSION {
        let step = MIGRATIONS
            .iter()
            .find(|(from, _)| *from == version)
            .map(|(_, f)| *f)
            .ok_or_else(|| ModelError::File(format!("no migration from schema {version}")))?;
        value = step(value)?;
        version += 1;
        value["schemaVersion"] = Value::from(version);
    }
    Ok(value)
}

pub fn project_file(dir: &Path) -> PathBuf {
    dir.join(PROJECT_FILE)
}

/// Parses `project.json` text, migrating older versions.
pub fn project_from_json(text: &str) -> Result<Project, ModelError> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| ModelError::File(format!("invalid JSON: {e}")))?;
    let value = migrate(value)?;
    serde_json::from_value(value).map_err(|e| ModelError::File(format!("invalid project: {e}")))
}

pub fn project_to_json(project: &Project) -> String {
    serde_json::to_string_pretty(project).unwrap_or_default()
}

/// Writes the project folder. `project.json` is written to a temporary file
/// and renamed, so a crash mid-save never leaves a half-written project.
pub fn save_project(project: &Project, dir: &Path) -> Result<(), ModelError> {
    for sub in [AUDIO_DIR, RENDERS_DIR, CACHE_DIR] {
        fs::create_dir_all(dir.join(sub))?;
    }
    let tmp = dir.join(format!("{PROJECT_FILE}.tmp"));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(project_to_json(project).as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    fs::rename(&tmp, project_file(dir))?;
    clear_journal(dir)?;
    Ok(())
}

/// Reads `project.json` from a project folder (without replaying the journal).
pub fn load_project(dir: &Path) -> Result<Project, ModelError> {
    let path = project_file(dir);
    let text = fs::read_to_string(&path)
        .map_err(|e| ModelError::File(format!("cannot read {}: {e}", path.display())))?;
    project_from_json(&text)
}

/// One journal line: the commands of one change, as applied.
#[derive(Debug, Serialize, Deserialize)]
pub struct JournalEntry {
    pub label: String,
    pub commands: Vec<Command>,
}

/// Appends applied commands so a crash loses nothing since the last save.
pub struct Journal {
    file: File,
}

impl Journal {
    pub fn open(dir: &Path) -> Result<Self, ModelError> {
        fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(JOURNAL_FILE))?;
        Ok(Self { file })
    }

    pub fn append(&mut self, label: &str, commands: &[Command]) -> Result<(), ModelError> {
        let entry = JournalEntry {
            label: label.to_string(),
            commands: commands.to_vec(),
        };
        let line = serde_json::to_string(&entry).map_err(|e| ModelError::File(e.to_string()))?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }
}

pub fn clear_journal(dir: &Path) -> Result<(), ModelError> {
    let path = dir.join(JOURNAL_FILE);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Reads the journal; a torn last line (from a crash mid-write) is ignored.
pub fn read_journal(dir: &Path) -> Result<Vec<JournalEntry>, ModelError> {
    let path = dir.join(JOURNAL_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let reader = BufReader::new(File::open(path)?);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalEntry>(&line) {
            Ok(e) => entries.push(e),
            Err(_) => break,
        }
    }
    Ok(entries)
}

/// Loads a project and replays any journal left by a crash. Returns the
/// project and how many journal entries were replayed.
pub fn recover_project(dir: &Path) -> Result<(Project, usize), ModelError> {
    let mut project = load_project(dir)?;
    let entries = read_journal(dir)?;
    let mut replayed = 0;
    for entry in entries {
        if crate::history::apply_all(&mut project, entry.commands).is_err() {
            break;
        }
        replayed += 1;
    }
    Ok((project, replayed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::demo_project;

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Demo.skyproj");
        let project = demo_project();
        save_project(&project, &path).unwrap();
        assert!(path.join("audio").is_dir());
        let loaded = load_project(&path).unwrap();
        assert_eq!(loaded, project);
        let text = fs::read_to_string(path.join(PROJECT_FILE)).unwrap();
        assert!(text.contains("\"schemaVersion\": 1"));
    }

    #[test]
    fn newer_schema_is_refused() {
        let mut v: Value = serde_json::from_str(&project_to_json(&demo_project())).unwrap();
        v["schemaVersion"] = Value::from(SCHEMA_VERSION + 1);
        let err = project_from_json(&v.to_string()).unwrap_err();
        assert!(matches!(err, ModelError::NewerSchema { .. }));
        v.as_object_mut().unwrap().remove("schemaVersion");
        assert!(project_from_json(&v.to_string()).is_err());
    }

    #[test]
    fn journal_replays_after_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Crash.skyproj");
        let mut project = demo_project();
        save_project(&project, &path).unwrap();
        let mut journal = Journal::open(&path).unwrap();
        let cmds = vec![Command::SetTempo { bpm: 140.0 }];
        crate::history::apply_all(&mut project, cmds.clone()).unwrap();
        journal.append("Change tempo", &cmds).unwrap();
        // Simulate a torn write.
        OpenOptions::new()
            .append(true)
            .open(path.join(JOURNAL_FILE))
            .unwrap()
            .write_all(b"{\"label\": \"half")
            .unwrap();
        let (recovered, replayed) = recover_project(&path).unwrap();
        assert_eq!(replayed, 1);
        assert_eq!(recovered, project);
        save_project(&recovered, &path).unwrap();
        assert!(read_journal(&path).unwrap().is_empty());
    }
}
