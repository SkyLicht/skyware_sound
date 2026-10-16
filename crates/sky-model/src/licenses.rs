// SPDX-License-Identifier: GPL-3.0-or-later
//! What an export owes the packs its sounds came from: credit lines the
//! licenses ask for, and warnings about audio whose terms are unknown or
//! carry conditions.

use serde::Serialize;
use sky_core::AudioId;

use crate::project::*;

/// What an export needs to say about the audio in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseReport {
    /// One credit line per pack whose license asks for one.
    pub credits: Vec<String>,
    /// Things to check before the audio ships in a game.
    pub warnings: Vec<String>,
}

impl LicenseReport {
    pub fn is_empty(&self) -> bool {
        self.credits.is_empty() && self.warnings.is_empty()
    }
}

fn push_once(list: &mut Vec<String>, line: String) {
    if !list.contains(&line) {
        list.push(line);
    }
}

/// The pool audio an export of `project` can contain: what the audible
/// tracks with clips play. Muted tracks, and tracks a solo silences, are
/// left out, as they are from the render.
pub fn audio_in_use(project: &Project) -> Vec<AudioId> {
    let any_solo = project.any_solo();
    let mut ids = Vec::new();
    for t in &project.tracks {
        let audible = !t.channel.mute && (!any_solo || t.channel.solo);
        if !audible || t.clips.is_empty() {
            continue;
        }
        for c in &t.clips {
            if let ClipContent::Audio { source, .. } = c.content {
                ids.push(source);
            }
        }
        if let Some(slot) = &t.instrument {
            ids.extend(slot.sample);
            if let Some(bank) = &slot.bank {
                ids.extend(bank.audio_ids());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(*id));
    ids
}

/// Credits and warnings for the pack audio an export of `project` uses.
/// Audio imported from disk has no source and needs nothing.
pub fn license_report(project: &Project) -> LicenseReport {
    let mut report = LicenseReport::default();
    for id in audio_in_use(project) {
        let Some(source) = project.pool_entry(id).ok().and_then(|e| e.source.as_ref()) else {
            continue;
        };
        let pack = &source.pack;
        let license = source.license;
        if license == License::Unknown {
            push_once(
                &mut report.warnings,
                format!(
                    "{pack} does not state a license. Find out what its authors allow before \
                     this audio ships in a game."
                ),
            );
        }
        if license.share_alike() {
            push_once(
                &mut report.warnings,
                format!(
                    "{pack} is {}: audio made with it must also be shared under {}.",
                    license.label(),
                    license.label()
                ),
            );
        }
        if license.needs_credit() {
            let line = source
                .credit
                .clone()
                .unwrap_or_else(|| format!("{pack} ({})", license.label()));
            push_once(&mut report.credits, line);
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use sky_dsp::InstrumentType;

    fn entry(project: &mut Project, pack: &str, license: License) -> AudioId {
        let id = AudioId::new();
        project.audio_pool.push(PoolEntry {
            id,
            name: pack.into(),
            file: format!("audio/{id}.wav"),
            channels: 1,
            sample_rate: 48_000,
            frames: 100,
            source: Some(AudioSource {
                pack: pack.into(),
                url: format!("https://example.com/{pack}.wav"),
                license,
                credit: (license == License::CcBy3).then(|| format!("{pack} by Someone")),
            }),
        });
        id
    }

    fn bank_track(name: &str, ids: &[AudioId]) -> Track {
        let mut t = Track::new_instrument(name, InstrumentType::Bank);
        let sounds = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                (
                    format!("s{i}"),
                    vec![BankSample {
                        audio: *id,
                        root: None,
                    }],
                )
            })
            .collect();
        t.instrument.as_mut().unwrap().bank = Some(SampleBank {
            name: name.into(),
            sounds,
            pitched: None,
        });
        t.clips.push(Clip {
            id: sky_core::ClipId::new(),
            name: "p".into(),
            start: 0,
            length: 960,
            muted: false,
            content: ClipContent::pattern("s0", 0, 960),
        });
        t
    }

    #[test]
    fn credits_and_warnings_follow_the_licenses() {
        let mut p = Project::new("Demo");
        let cc0 = entry(&mut p, "VCSL", License::Cc0);
        let by = entry(&mut p, "Piano", License::CcBy3);
        let unknown = entry(&mut p, "Drums", License::Unknown);
        let sa = entry(&mut p, "Mridangam", License::CcBySa4);
        p.tracks.push(bank_track("A", &[cc0, by, unknown, sa]));
        let r = license_report(&p);
        assert_eq!(
            r.credits,
            ["Piano by Someone", "Mridangam (CC BY-SA 4.0)"],
            "{r:?}"
        );
        assert_eq!(r.warnings.len(), 2, "{r:?}");
        assert!(r.warnings[0].starts_with("Drums does not state a license"));
        assert!(r.warnings[1].contains("must also be shared under CC BY-SA 4.0"));
    }

    #[test]
    fn silent_and_empty_tracks_do_not_count() {
        let mut p = Project::new("Demo");
        let unknown = entry(&mut p, "Drums", License::Unknown);
        let mut muted = bank_track("Muted", &[unknown]);
        muted.channel.mute = true;
        p.tracks.push(muted);
        let mut empty = bank_track("Empty", &[unknown]);
        empty.clips.clear();
        p.tracks.push(empty);
        assert!(license_report(&p).is_empty());
        p.tracks[0].channel.mute = false;
        assert_eq!(license_report(&p).warnings.len(), 1);
        p.tracks[1].clips = p.tracks[0].clips.clone();
        p.tracks[1].channel.solo = true;
        // The solo silences the only track that uses the pack audio.
        p.tracks[1].instrument.as_mut().unwrap().bank = None;
        assert!(license_report(&p).is_empty());
    }

    #[test]
    fn licenses_parse_from_ids_and_labels() {
        for l in License::ALL {
            assert_eq!(License::parse(l.id()), Some(l));
            assert_eq!(License::parse(l.label()), Some(l));
        }
        assert_eq!(License::parse("cc0"), Some(License::Cc0));
        assert_eq!(License::parse("cc-by 3.0"), Some(License::CcBy3));
        assert_eq!(License::parse("GPL"), None);
    }
}
