// SPDX-License-Identifier: GPL-3.0-or-later
//! Strudel sample maps (`strudel.json`).
//!
//! A map is a JSON object from sound names to files. A value is one path, a
//! list of paths (the sound's variations, `bd:0`, `bd:1`, ...), or an object
//! from note names to paths for a pitched sound. `_base` is the address the
//! paths are relative to; other keys starting with `_` are ignored:
//!
//! ```json
//! {
//!   "_base": "https://raw.githubusercontent.com/user/samples/main/",
//!   "RolandTR808_bd": ["808/bd0.wav", "808/bd1.wav"],
//!   "piano": { "A0": "piano/A0.mp3", "C1": "piano/C1.mp3" }
//! }
//! ```
//!
//! A bank such as `RolandTR808` is a name prefix: Strudel's
//! `s("bd").bank("RolandTR808")` plays the sound `RolandTR808_bd`.

use std::collections::BTreeMap;

use serde_json::Value;

/// One file of a sound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapFile {
    /// The path as the map gives it, relative to the base.
    pub path: String,
    /// The MIDI note the file plays, for pitched sounds.
    pub root: Option<u8>,
}

/// A parsed sample map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SampleMap {
    /// What relative paths are resolved against: a URL ending in `/`, or a
    /// folder.
    pub base: String,
    pub sounds: BTreeMap<String, Vec<MapFile>>,
}

/// Turns `github:user/repo` (branch `main`) or `github:user/repo/branch`
/// into the address of that repository's `strudel.json`, as Strudel does.
/// Anything else is returned unchanged.
pub fn expand_github(location: &str) -> String {
    let Some(path) = location.strip_prefix("github:") else {
        return location.to_string();
    };
    let path = path.trim_matches('/');
    let path = if path.split('/').count() == 2 {
        format!("{path}/main")
    } else {
        path.to_string()
    };
    format!("https://raw.githubusercontent.com/{path}/strudel.json")
}

fn is_url(s: &str) -> bool {
    let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The folder part of a map's address, ending in `/`.
fn parent(location: &str) -> String {
    match location.rfind(['/', '\\']) {
        Some(i) => location[..=i].to_string(),
        None => String::new(),
    }
}

/// Percent-encodes what may not appear in a URL path, leaving existing
/// `%XX` escapes alone: map paths are sometimes encoded and sometimes not.
fn encode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(path.len());
    for (i, &b) in bytes.iter().enumerate() {
        let escape = b == b'%'
            && bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
            && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit);
        let keep = b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b) || escape;
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `%XX` escapes, for paths inside a local folder.
fn decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |j: usize| {
            bytes
                .get(j)
                .and_then(|&b| (b as char).to_digit(16))
                .map(|d| d as u8)
        };
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (hex(i + 1), hex(i + 2))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl SampleMap {
    /// Parses a map. `location` is where it was read from, which is the base
    /// when the map has no `_base`.
    pub fn parse(json: &str, location: &str) -> Result<SampleMap, String> {
        let value: Value =
            serde_json::from_str(json).map_err(|e| format!("not a sample map: {e}"))?;
        Self::from_value(&value, location)
    }

    /// Reads a map already parsed as JSON, such as one written in a
    /// `samples({ ... })` call.
    pub fn from_value(value: &Value, location: &str) -> Result<SampleMap, String> {
        let Value::Object(object) = value else {
            return Err("not a sample map: expected a JSON object of sound names".into());
        };
        let base = match object.get("_base") {
            Some(Value::String(b)) => {
                let b = expand_github(b)
                    .trim_end_matches("strudel.json")
                    .to_string();
                if b.ends_with('/') || b.ends_with('\\') || b.is_empty() {
                    b
                } else {
                    format!("{b}/")
                }
            }
            _ => parent(location),
        };
        let mut sounds = BTreeMap::new();
        for (name, v) in object {
            if name.starts_with('_') {
                continue;
            }
            let files = files_of(v);
            if !files.is_empty() {
                sounds.insert(name.clone(), files);
            }
        }
        if sounds.is_empty() {
            return Err("the sample map lists no sounds".into());
        }
        Ok(SampleMap { base, sounds })
    }

    /// Where a file of this map is: a URL, or a path in a local folder.
    pub fn locate(&self, file: &MapFile) -> String {
        if is_url(&file.path) {
            return encode_path(&file.path);
        }
        if is_url(&self.base) {
            format!(
                "{}{}",
                self.base,
                encode_path(file.path.trim_start_matches('/'))
            )
        } else {
            let base = self.base.strip_prefix("file://").unwrap_or(&self.base);
            std::path::Path::new(base)
                .join(decode_path(&file.path))
                .to_string_lossy()
                .into_owned()
        }
    }

    /// The sound a pattern's name plays, by its full name: the name itself,
    /// the name in another case, then another name for the same drum, so
    /// `sd` finds a map's `sn`.
    pub fn find_sound(&self, name: &str) -> Option<(&str, &[MapFile])> {
        let found = |key: &str| {
            self.sounds
                .get_key_value(key)
                .or_else(|| {
                    self.sounds
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(key))
                })
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| (k.as_str(), v.as_slice()))
        };
        found(name).or_else(|| {
            let note = sky_pattern::drum_note(name)?;
            sky_pattern::drum_names(note).find_map(found)
        })
    }

    /// The banks in the map: name prefixes before `_` that two or more
    /// sounds share, with how many sounds each has.
    pub fn banks(&self) -> BTreeMap<String, usize> {
        let mut banks = BTreeMap::new();
        for name in self.sounds.keys() {
            if let Some((prefix, rest)) = name.split_once('_')
                && !prefix.is_empty()
                && !rest.is_empty()
            {
                *banks.entry(prefix.to_string()).or_insert(0) += 1;
            }
        }
        banks.retain(|_, n| *n >= 2);
        banks
    }

    /// The sounds of `bank`, named without its prefix (`RolandTR808_bd` is
    /// `bd`), or every sound without a bank. The bank name ignores case.
    pub fn bank_sounds(&self, bank: Option<&str>) -> BTreeMap<String, &[MapFile]> {
        self.sounds
            .iter()
            .filter_map(|(name, files)| {
                let short = match bank {
                    None => name.as_str(),
                    Some(bank) => {
                        let (prefix, rest) = name.split_once('_')?;
                        if !prefix.eq_ignore_ascii_case(bank) || rest.is_empty() {
                            return None;
                        }
                        rest
                    }
                };
                Some((short.to_string(), files.as_slice()))
            })
            .collect()
    }

    /// The sounds that belong to no bank, by their full names.
    pub fn plain_sounds(&self) -> BTreeMap<String, &[MapFile]> {
        let banks = self.banks();
        let mut sounds = self.bank_sounds(None);
        sounds.retain(|name, _| {
            name.split_once('_')
                .is_none_or(|(prefix, _)| !banks.contains_key(prefix))
        });
        sounds
    }
}

fn files_of(v: &Value) -> Vec<MapFile> {
    let file = |path: &str, root| MapFile {
        path: path.to_string(),
        root,
    };
    match v {
        Value::String(p) => vec![file(p, None)],
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|p| file(p, None))
            .collect(),
        Value::Object(notes) => {
            let mut out: Vec<MapFile> = notes
                .iter()
                .filter_map(|(note, paths)| Some((sky_pattern::note_number(note)?, paths)))
                .flat_map(|(root, paths)| match paths {
                    Value::String(p) => vec![file(p, Some(root))],
                    Value::Array(items) => items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|p| file(p, Some(root)))
                        .collect(),
                    _ => Vec::new(),
                })
                .collect();
            out.sort_by_key(|f| f.root);
            out
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = r#"{
        "_base": "https://example.com/samples",
        "RolandTR808_bd": ["808/bd 0.wav", "808/bd%201.wav"],
        "RolandTR808_sd": "808/sd.wav",
        "snare_modern": ["s.wav"],
        "casio": ["casio/high.wav"],
        "piano": { "C4": "p/C4.mp3", "A0": "p/A0.mp3", "Ds1": ["p/Ds1.mp3"], "zz": "x.mp3" },
        "_comment": "ignored",
        "empty": []
    }"#;

    #[test]
    fn parses_every_value_shape() {
        let map = SampleMap::parse(MAP, "https://elsewhere/strudel.json").unwrap();
        assert_eq!(map.base, "https://example.com/samples/");
        assert_eq!(
            map.sounds.keys().collect::<Vec<_>>(),
            [
                "RolandTR808_bd",
                "RolandTR808_sd",
                "casio",
                "piano",
                "snare_modern"
            ]
        );
        assert_eq!(map.sounds["RolandTR808_bd"].len(), 2);
        let piano: Vec<_> = map.sounds["piano"].iter().map(|f| f.root).collect();
        assert_eq!(piano, [Some(21), Some(27), Some(60)], "sorted by pitch");
        assert_eq!(
            map.locate(&map.sounds["RolandTR808_bd"][0]),
            "https://example.com/samples/808/bd%200.wav"
        );
        // An escape that is already there stays as it is.
        assert_eq!(
            map.locate(&map.sounds["RolandTR808_bd"][1]),
            "https://example.com/samples/808/bd%201.wav"
        );
    }

    #[test]
    fn banks_are_shared_prefixes() {
        let map = SampleMap::parse(MAP, "x").unwrap();
        assert_eq!(
            map.banks().into_iter().collect::<Vec<_>>(),
            [("RolandTR808".to_string(), 2)]
        );
        let tr808 = map.bank_sounds(Some("rolandtr808"));
        assert_eq!(tr808.keys().collect::<Vec<_>>(), ["bd", "sd"]);
        assert_eq!(map.bank_sounds(None).len(), 5);
        assert_eq!(
            map.plain_sounds().keys().collect::<Vec<_>>(),
            ["casio", "piano", "snare_modern"]
        );
        assert!(map.bank_sounds(Some("Nope")).is_empty());
    }

    #[test]
    fn base_defaults_to_the_maps_folder() {
        let map =
            SampleMap::parse(r#"{"bd": "bd/0.wav"}"#, "https://a.org/x/strudel.json").unwrap();
        assert_eq!(map.locate(&map.sounds["bd"][0]), "https://a.org/x/bd/0.wav");
        let local =
            SampleMap::parse(r#"{"bd": "my%20bd.wav"}"#, "/packs/kit/strudel.json").unwrap();
        let path = local.locate(&local.sounds["bd"][0]);
        assert!(
            path.ends_with("my bd.wav") && path.contains("kit"),
            "{path}"
        );
        let github = SampleMap::parse(r#"{"_base": "github:a/b", "bd": "x.wav"}"#, "").unwrap();
        assert_eq!(github.base, "https://raw.githubusercontent.com/a/b/main/");
    }

    #[test]
    fn github_shortcuts_expand_like_strudel() {
        assert_eq!(
            expand_github("github:yaxu/clean-breaks"),
            "https://raw.githubusercontent.com/yaxu/clean-breaks/main/strudel.json"
        );
        assert_eq!(
            expand_github("github:tidalcycles/dirt-samples/master/"),
            "https://raw.githubusercontent.com/tidalcycles/dirt-samples/master/strudel.json"
        );
        assert_eq!(
            expand_github("C:/packs/strudel.json"),
            "C:/packs/strudel.json"
        );
    }

    #[test]
    fn bad_maps_say_why() {
        assert!(
            SampleMap::parse("[1, 2]", "x")
                .unwrap_err()
                .contains("JSON object")
        );
        assert!(
            SampleMap::parse("{\"_base\": \"x\"}", "x")
                .unwrap_err()
                .contains("no sounds")
        );
        assert!(
            SampleMap::parse("{", "x")
                .unwrap_err()
                .contains("not a sample map")
        );
    }

    #[test]
    fn patterns_find_sounds_by_other_names() {
        let map =
            SampleMap::parse(r#"{"BD": "bd.wav", "sn": ["a.wav", "b.wav"], "e": []}"#, "").unwrap();
        assert_eq!(map.find_sound("bd").map(|(k, _)| k), Some("BD"));
        assert_eq!(
            map.find_sound("sd").map(|(k, f)| (k, f.len())),
            Some(("sn", 2))
        );
        assert!(
            map.find_sound("e").is_none(),
            "a sound with no files is not there"
        );
        assert!(map.find_sound("hh").is_none());
    }
}
