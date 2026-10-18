// SPDX-License-Identifier: GPL-3.0-or-later
//! Reads the part of the SFZ format that sampled instruments such as pianos
//! use, into zones for the multi-sample engine.
//!
//! Supported: the `<control>`, `<global>`, `<master>`, `<group>` and
//! `<region>` headers, `#define` and `#include`, note names, and the
//! opcodes that place samples (`sample`, `lokey`, `hikey`, `key`,
//! `pitch_keycenter`, `lovel`, `hivel`), tune them (`tune`, `transpose`,
//! `pitch_keytrack`), set their level and place (`volume`, `amplitude`,
//! `pan`, `width`), loop them (`loop_mode`, `loop_start`, `loop_end`,
//! `offset`, `end`), take turns (`seq_length`, `seq_position`), play on
//! note-off (`trigger=release`, `rt_decay`), release (`ampeg_release`) and
//! filter them (`fil_type` low-pass, `cutoff`, `resonance`). Regions that
//! need a pedal or controller to be pressed are left out, as the instrument
//! starts with every controller at 0. Anything else is skipped and listed,
//! such as ARIA's `set_hdcc`.

use std::collections::{BTreeMap, BTreeSet};

use sky_dsp::instruments::Zone;
use sky_dsp::instruments::sampled::{LoopMode, Trigger, ZoneFilter};

/// The most `#include`s one instrument may use, nested or not.
const MAX_INCLUDES: usize = 256;
const MAX_DEPTH: usize = 8;

/// An instrument read from an SFZ file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SfzInstrument {
    /// Sample files, relative to the instrument's folder, with `/` between
    /// folders. Each zone's `sample` indexes this list.
    pub samples: Vec<String>,
    pub zones: Vec<Zone>,
    /// Opcodes that were not understood or not supported.
    pub skipped: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Control,
    Global,
    Master,
    Group,
    Region,
    /// Headers whose opcodes do not shape regions, such as `<curve>`.
    Other,
}

type Opcodes = BTreeMap<String, String>;

/// Removes `//` and `/* */` comments.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let line = rest.find("//");
        let block = rest.find("/*");
        match (line, block) {
            (Some(l), b) if b.is_none_or(|b| l < b) => {
                out.push_str(&rest[..l]);
                rest = rest[l..].find('\n').map_or("", |e| &rest[l + e..]);
            }
            (_, Some(b)) => {
                out.push_str(&rest[..b]);
                rest = rest[b + 2..]
                    .find("*/")
                    .map_or("", |e| &rest[b + 2 + e + 2..]);
                out.push(' ');
            }
            _ => {
                out.push_str(rest);
                rest = "";
            }
        }
    }
    out
}

/// A note number from a number or a name such as `c4`, `F#3` or `eb2`
/// (middle C is `c4`, 60).
fn note(value: &str) -> Option<i32> {
    let v = value.trim();
    if let Ok(n) = v.parse::<i32>() {
        return Some(n);
    }
    let mut chars = v.chars();
    let base = match chars.next()?.to_ascii_lowercase() {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (shift, octave) = if let Some(r) = rest.strip_prefix('#') {
        (1, r)
    } else if let Some(r) = rest.strip_prefix('b').filter(|r| !r.is_empty()) {
        (-1, r)
    } else {
        (0, rest)
    };
    let octave: i32 = octave.parse().ok()?;
    Some((octave + 1) * 12 + base + shift)
}

/// Splits one line into headers and `opcode=value` pairs. A value runs to
/// the next opcode or header, so sample paths may hold spaces.
fn tokens(line: &str) -> Vec<(Option<&str>, &str)> {
    // Where each `name=` starts, and each `<header>`.
    let bytes = line.as_bytes();
    let mut marks: Vec<(usize, usize, bool)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<'
            && let Some(end) = line[i..].find('>')
        {
            marks.push((i, i + end + 1, true));
            i += end + 1;
            continue;
        }
        let starts_word = i == 0 || bytes[i - 1].is_ascii_whitespace() || bytes[i - 1] == b'>';
        if starts_word && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
        {
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'$')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                marks.push((i, j, false));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    let mut out = Vec::new();
    for (k, &(start, end, header)) in marks.iter().enumerate() {
        if header {
            out.push((None, &line[start + 1..end - 1]));
        } else {
            let value_end = marks.get(k + 1).map_or(line.len(), |m| m.0);
            out.push((Some(&line[start..end]), line[end + 1..value_end].trim()));
        }
    }
    out
}

/// `path`, relative to the folder `base`, as a path relative to the
/// instrument's folder with `/` between folders; `None` for paths that
/// leave the instrument's folder or name a generator such as `*sine`.
fn clean_path(base: &str, path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.starts_with('*') || path.starts_with('/') || path.contains(':') {
        return None;
    }
    let joined = format!("{base}/{path}");
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

struct Reader<'a> {
    /// The main file's folder, relative to the instrument's folder.
    base: &'a str,
    include: &'a mut dyn FnMut(&str) -> Result<String, String>,
    includes: usize,
    defines: Vec<(String, String)>,
    level: Level,
    /// The opcodes set at each level above the region, and the region's.
    scopes: [Opcodes; 5],
    regions: Vec<Opcodes>,
    in_region: bool,
}

impl Reader<'_> {
    fn substitute(&self, text: &str) -> String {
        if !text.contains('$') {
            return text.to_string();
        }
        let mut out = text.to_string();
        // Longest names first, so `$PATH2` is not read as `$PATH` and `2`.
        let mut defines: Vec<&(String, String)> = self.defines.iter().collect();
        defines.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
        for (name, value) in defines {
            out = out.replace(name.as_str(), value);
        }
        out
    }

    fn finish_region(&mut self) {
        if self.in_region {
            let mut merged = Opcodes::new();
            for scope in &self.scopes {
                for (k, v) in scope {
                    merged.insert(k.clone(), v.clone());
                }
            }
            self.regions.push(merged);
            self.in_region = false;
        }
    }

    fn header(&mut self, name: &str) {
        self.finish_region();
        let level = match name.trim().to_ascii_lowercase().as_str() {
            "control" => Level::Control,
            "global" => Level::Global,
            "master" => Level::Master,
            "group" => Level::Group,
            "region" => Level::Region,
            _ => Level::Other,
        };
        // A header clears its own level and every one below it.
        if level != Level::Other {
            for scope in &mut self.scopes[level as usize..] {
                scope.clear();
            }
        }
        self.in_region = level == Level::Region;
        self.level = level;
    }

    fn read(&mut self, text: &str, depth: usize) -> Result<(), String> {
        for line in strip_comments(text).lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("#define") {
                let rest = rest.trim();
                if let Some((name, value)) = rest.split_once(char::is_whitespace) {
                    let value = self.substitute(value.trim());
                    self.defines.retain(|(n, _)| n != name);
                    self.defines.push((name.to_string(), value));
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("#include") {
                let name = self.substitute(rest.trim().trim_matches('"'));
                self.includes += 1;
                if depth >= MAX_DEPTH || self.includes > MAX_INCLUDES {
                    return Err(format!("too many nested #include lines at {name}"));
                }
                let file = clean_path(self.base, &name)
                    .ok_or_else(|| format!("#include {name:?} is outside the instrument"))?;
                let included = (self.include)(&file)?;
                self.read(&included, depth + 1)?;
                continue;
            }
            let line = self.substitute(line);
            for (opcode, value) in tokens(&line) {
                match opcode {
                    None => self.header(value),
                    Some(_) if self.level == Level::Other => {}
                    Some(op) => {
                        self.scopes[self.level as usize]
                            .insert(op.to_ascii_lowercase(), value.to_string());
                    }
                }
            }
        }
        Ok(())
    }
}

fn number(ops: &Opcodes, name: &str) -> Option<f32> {
    ops.get(name)?.trim().parse().ok()
}

fn key_value(ops: &Opcodes, name: &str, shift: i32) -> Option<u8> {
    let n = note(ops.get(name)?)? + shift;
    Some(n.clamp(0, 127) as u8)
}

fn frames(ops: &Opcodes, names: &[&str]) -> Option<u32> {
    names
        .iter()
        .find_map(|n| ops.get(*n))
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|v| v.max(0.0).min(f64::from(u32::MAX)) as u32)
}

/// Opcodes [`region_zone`] reads, in any group.
const KNOWN: &[&str] = &[
    "sample",
    "default_path",
    "octave_offset",
    "note_offset",
    "lokey",
    "hikey",
    "key",
    "pitch_keycenter",
    "lovel",
    "hivel",
    "tune",
    "pitch",
    "transpose",
    "pitch_keytrack",
    "volume",
    "gain",
    "amplitude",
    "pan",
    "width",
    "trigger",
    "rt_decay",
    "loop_mode",
    "loopmode",
    "loop_start",
    "loopstart",
    "loop_end",
    "loopend",
    "offset",
    "end",
    "seq_length",
    "seq_position",
    "lorand",
    "hirand",
    "ampeg_release",
    "fil_type",
    "filtype",
    "cutoff",
    "resonance",
    // Velocity tracking belongs to the instrument on top, such as the
    // piano's dynamics; labels change nothing about the sound.
    "amp_veltrack",
    "region_label",
    "group_label",
    "master_label",
    "global_label",
];

/// What the sustain pedal does to start a region that `on_locc64` and
/// `on_hicc64` start: pressing it when the range is the top half, letting
/// go when it is the bottom half.
fn pedal_trigger(ops: &Opcodes) -> Option<Trigger> {
    let lo = number(ops, "on_locc64");
    let hi = number(ops, "on_hicc64");
    if lo.is_none() && hi.is_none() {
        return None;
    }
    let (lo, hi) = (lo.unwrap_or(0.0), hi.unwrap_or(127.0));
    if lo >= 64.0 {
        Some(Trigger::PedalDown)
    } else if hi <= 63.0 {
        Some(Trigger::PedalUp)
    } else {
        None
    }
}

/// Whether a region waits for a controller this player does not follow:
/// `on_loccN`/`on_hiccN` start it from a controller move (the sustain
/// pedal's are played, see [`pedal_trigger`]), and `loccN`/`hiccN` need the
/// controller inside a range that leaves out 0.
fn needs_controller(ops: &Opcodes) -> bool {
    let pedal = pedal_trigger(ops).is_some();
    ops.iter().any(|(k, v)| {
        if k.starts_with("on_locc") || k.starts_with("on_hicc") {
            return !(pedal && (k == "on_locc64" || k == "on_hicc64"));
        }
        let bound = v.trim().parse::<f32>().unwrap_or(0.0);
        (k.starts_with("locc") && bound > 0.0) || (k.starts_with("hicc") && bound < 0.0)
    })
}

/// The zone a region describes, given the index of its sample.
fn region_zone(ops: &Opcodes, sample: u32) -> Zone {
    let shift = number(ops, "octave_offset").unwrap_or(0.0) as i32 * 12
        + number(ops, "note_offset").unwrap_or(0.0) as i32;
    let key = key_value(ops, "key", shift);
    let mut z = Zone::new(sample, 60);
    z.lokey = key_value(ops, "lokey", shift).or(key).unwrap_or(0);
    z.hikey = key_value(ops, "hikey", shift).or(key).unwrap_or(127);
    z.root = key_value(ops, "pitch_keycenter", shift)
        .or(key)
        .unwrap_or(60);
    z.lovel = number(ops, "lovel").map_or(0, |v| v.clamp(0.0, 127.0) as u8);
    z.hivel = number(ops, "hivel").map_or(127, |v| v.clamp(0.0, 127.0) as u8);
    z.tune = number(ops, "transpose").unwrap_or(0.0)
        + (number(ops, "tune").or(number(ops, "pitch")).unwrap_or(0.0)) / 100.0;
    z.keytrack = number(ops, "pitch_keytrack").unwrap_or(100.0) / 100.0;
    z.gain_db = number(ops, "volume").or(number(ops, "gain")).unwrap_or(0.0)
        + number(ops, "amplitude").map_or(0.0, |a| {
            sky_dsp::util::gain_to_db((a / 100.0).clamp(0.0, 1.0))
        });
    z.pan = number(ops, "pan").unwrap_or(0.0).clamp(-100.0, 100.0) / 100.0;
    z.width = number(ops, "width").unwrap_or(100.0).clamp(-100.0, 100.0) / 100.0;
    z.trigger = pedal_trigger(ops).unwrap_or_else(|| {
        match ops.get("trigger").map(|t| t.trim().to_ascii_lowercase()) {
            Some(t) if t.starts_with("release") => Trigger::Release,
            _ => Trigger::Attack,
        }
    });
    z.rt_decay = number(ops, "rt_decay").unwrap_or(0.0).max(0.0);
    let mode = ops
        .get("loop_mode")
        .or_else(|| ops.get("loopmode"))
        .map(|m| m.trim().to_ascii_lowercase());
    z.loop_mode = match mode.as_deref() {
        Some("one_shot") => LoopMode::OneShot,
        Some("loop_continuous") => LoopMode::Continuous,
        Some("loop_sustain") => LoopMode::Sustain,
        _ => LoopMode::NoLoop,
    };
    z.loop_start = frames(ops, &["loop_start", "loopstart"]).unwrap_or(0);
    z.loop_end = frames(ops, &["loop_end", "loopend"]).unwrap_or(0);
    z.offset = frames(ops, &["offset"]).unwrap_or(0);
    z.end = frames(ops, &["end"]);
    z.seq_length = number(ops, "seq_length").map_or(1, |v| v.clamp(1.0, 100.0) as u8);
    z.seq_position = number(ops, "seq_position").map_or(1, |v| v.clamp(1.0, 100.0) as u8);
    z.lorand = number(ops, "lorand").unwrap_or(0.0);
    z.hirand = number(ops, "hirand").unwrap_or(1.0);
    z.release = number(ops, "ampeg_release").unwrap_or(0.001).max(0.001);
    let fil = ops
        .get("fil_type")
        .or_else(|| ops.get("filtype"))
        .map(|f| f.trim().to_ascii_lowercase());
    let poles = match fil.as_deref() {
        Some("lpf_1p") => Some(1),
        Some("lpf_2p") | None => Some(2),
        Some(_) => None,
    };
    z.filter = match (number(ops, "cutoff"), poles) {
        (Some(cutoff), Some(poles)) if cutoff > 0.0 => Some(ZoneFilter {
            cutoff,
            poles,
            resonance_db: number(ops, "resonance").unwrap_or(0.0),
        }),
        _ => None,
    };
    z
}

/// Reads an SFZ instrument whose file sits in the folder `base` of the
/// instrument's folder (`""` for the top). `include` returns the text of a
/// file an `#include` line names, given its path in the instrument's folder.
pub fn parse(
    text: &str,
    base: &str,
    include: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<SfzInstrument, String> {
    let mut reader = Reader {
        base,
        include,
        includes: 0,
        defines: Vec::new(),
        level: Level::Other,
        scopes: Default::default(),
        regions: Vec::new(),
        in_region: false,
    };
    reader.read(text, 0)?;
    reader.finish_region();

    let mut out = SfzInstrument::default();
    let mut index: BTreeMap<String, u32> = BTreeMap::new();
    let mut skipped = BTreeSet::new();
    for ops in &reader.regions {
        for k in ops.keys() {
            let controller = ["on_locc", "on_hicc", "locc", "hicc"]
                .iter()
                .any(|p| k.starts_with(p));
            if !controller && !KNOWN.contains(&k.as_str()) {
                skipped.insert(k.clone());
            }
        }
        if needs_controller(ops) {
            continue;
        }
        let Some(sample) = ops.get("sample") else {
            continue;
        };
        let default_path = ops.get("default_path").map_or("", String::as_str);
        let path = (!sample.starts_with('*'))
            .then(|| clean_path(base, &format!("{default_path}{sample}")))
            .flatten();
        let Some(path) = path else {
            skipped.insert(format!("sample={sample}"));
            continue;
        };
        let next = index.len() as u32;
        let i = *index.entry(path.clone()).or_insert_with(|| {
            out.samples.push(path);
            next
        });
        out.zones.push(region_zone(ops, i));
    }
    if out.zones.is_empty() {
        return Err("the SFZ file has no regions with samples".into());
    }
    out.skipped = skipped.into_iter().collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> SfzInstrument {
        parse(text, "", &mut |f| Err(format!("no {f}"))).expect("parses")
    }

    #[test]
    fn note_names_and_numbers() {
        assert_eq!(note("60"), Some(60));
        assert_eq!(note("c4"), Some(60));
        assert_eq!(note("A0"), Some(21));
        assert_eq!(note("f#3"), Some(54));
        assert_eq!(note("eb2"), Some(39));
        assert_eq!(note("c-1"), Some(0));
        assert_eq!(note("h4"), None);
    }

    #[test]
    fn values_run_to_the_next_opcode() {
        let t = tokens("<region> sample=My Piano/C4 soft.wav lokey=c4 hikey=62");
        assert_eq!(
            t,
            vec![
                (None, "region"),
                (Some("sample"), "My Piano/C4 soft.wav"),
                (Some("lokey"), "c4"),
                (Some("hikey"), "62"),
            ]
        );
    }

    #[test]
    fn regions_inherit_from_groups_and_globals() {
        let sfz = read(
            "// a comment\r\n<global> ampeg_release=0.6 /* block\r\ncomment */ width=100\r\n\
             <group> hivel=80 loop_mode=loop_continuous\r\n\
             <region> lokey=21 hikey=22 pitch_keycenter=21 loop_start=10 loop_end=99 \
             sample=samples\\A0vL.flac\r\n\
             <region> key=23 sample=samples/B0vL.flac fil_type=lpf_2p cutoff=2500\r\n\
             <group> lovel=81\r\n<region> key=a0 sample=samples/A0vH.flac\r\n",
        );
        assert_eq!(
            sfz.samples,
            [
                "samples/A0vL.flac",
                "samples/B0vL.flac",
                "samples/A0vH.flac"
            ]
        );
        let [a, b, c] = &sfz.zones[..] else {
            panic!("three zones");
        };
        assert_eq!((a.lokey, a.hikey, a.root, a.hivel), (21, 22, 21, 80));
        assert_eq!(
            (a.loop_mode, a.loop_start, a.loop_end),
            (LoopMode::Continuous, 10, 99)
        );
        assert_eq!(a.release, 0.6);
        assert_eq!((b.lokey, b.hikey, b.root), (23, 23, 23));
        assert_eq!(b.filter.map(|f| (f.cutoff, f.poles)), Some((2500.0, 2)));
        // A new group starts from the global level again.
        assert_eq!((c.lovel, c.hivel, c.root), (81, 127, 21));
        assert_eq!(c.loop_mode, LoopMode::NoLoop);
        assert!(sfz.skipped.is_empty(), "{:?}", sfz.skipped);
    }

    #[test]
    fn defines_includes_and_release_triggers() {
        let mut files = BTreeMap::new();
        files.insert(
            "inst/Programs/keys.sfz",
            "<region> sample=$DIR/c4.wav key=60",
        );
        let sfz = parse(
            "#define $DIR sub\n<control> default_path=../Samples/\n\
             <group> trigger=release rt_decay=3 set_hdcc64=0\n#include \"keys.sfz\"\n\
             <group> on_locc64=64 on_hicc64=127\n<region> sample=pedal.wav pitch_keycenter=21\n\
             <group> on_locc64=0 on_hicc64=63\n<region> sample=up1.wav hirand=0.5\n\
             <region> sample=up2.wav lorand=0.5\n\
             <group> on_locc1=64 on_hicc1=127\n<region> sample=wheel.wav\n\
             <group> on_locc64=30 on_hicc64=90\n<region> sample=half.wav\n\
             <group> locc64=64\n<region> sample=down.wav\n\
             <group>\n<region> sample=*sine\n",
            "inst/Programs",
            &mut |f| files.get(f).map(|s| s.to_string()).ok_or(f.to_string()),
        )
        .expect("parses");
        // The pedal's sounds play on pedal moves; other controllers' do not.
        assert_eq!(
            sfz.samples,
            [
                "inst/Samples/sub/c4.wav",
                "inst/Samples/pedal.wav",
                "inst/Samples/up1.wav",
                "inst/Samples/up2.wav",
            ]
        );
        let triggers: Vec<_> = sfz.zones.iter().map(|z| z.trigger).collect();
        assert_eq!(
            triggers,
            [
                Trigger::Release,
                Trigger::PedalDown,
                Trigger::PedalUp,
                Trigger::PedalUp
            ]
        );
        assert_eq!(sfz.zones[1].root, 21);
        assert_eq!(
            (
                sfz.zones[2].lorand,
                sfz.zones[2].hirand,
                sfz.zones[3].lorand
            ),
            (0.0, 0.5, 0.5)
        );
        assert_eq!(
            (sfz.zones[0].trigger, sfz.zones[0].root),
            (Trigger::Release, 60)
        );
        assert_eq!(sfz.zones[0].rt_decay, 3.0);
        assert!(sfz.skipped.contains(&"set_hdcc64".to_string()));
        assert!(sfz.skipped.iter().any(|s| s.starts_with("sample=*sine")));
    }

    #[test]
    fn salamander_style_files_read() {
        // The shape of the Salamander Grand Piano's SFZ file: CRLF lines,
        // `#` in sample names, pedal noises on key -1, and voice groups.
        let text = "//Salamander Grand Piano V3\r\n//Notes\r\n\
             <group> amp_veltrack=73 ampeg_release=1\r\n\
             <region> sample=samples/D#4v1.flac lokey=62 hikey=64 lovel=1 hivel=26 \
             pitch_keycenter=63\r\n\
             <region> sample=samples/C4v16.flac lokey=59 hikey=61 lovel=121\r\n\
             //Notes without dampers\r\n\
             <group> amp_veltrack=73 ampeg_release=5\r\n\
             <region> sample=samples/F#6v1.flac lokey=89 hikey=91 hivel=26 \
             pitch_keycenter=90\r\n\
             <group> trigger=release volume=-4 amp_veltrack=94 rt_decay=6\r\n\
             <region> sample=samples/harmLD#1.flac lokey=26 hikey=28 lovel=45 \
             pitch_keycenter=27\r\n\
             <group> trigger=release pitch_keytrack=0 volume=-37 amp_veltrack=82 rt_decay=2\r\n\
             <region> sample=samples/rel1.flac lokey=21 hikey=21\r\n\
             <group> group=1 hikey=-1 lokey=-1 on_locc64=126 on_hicc64=127 off_by=2 \
             volume=-20\r\n\
             <region> sample=samples/pedalD1.flac lorand=0 hirand=0.5\r\n\
             <region> sample=samples/pedalD2.flac lorand=0.5 hirand=1\r\n\
             <group> group=2 hikey=-1 lokey=-1 on_locc64=0 on_hicc64=1 volume=-19\r\n\
             <region> sample=samples/pedalU1.flac lorand=0 hirand=0.5\r\n";
        let sfz = parse(text, "Salamander+V3", &mut |f| Err(format!("no {f}"))).expect("parses");
        assert_eq!(sfz.samples.len(), 8);
        assert_eq!(sfz.samples[0], "Salamander+V3/samples/D#4v1.flac");
        assert_eq!(sfz.skipped, ["group", "off_by"]);
        let z = &sfz.zones;
        assert_eq!(
            (z[0].root, z[0].lovel, z[0].hivel, z[0].release),
            (63, 1, 26, 1.0)
        );
        // A region with neither a key centre nor a key plays at middle C.
        assert_eq!((z[1].root, z[1].lovel, z[1].hivel), (60, 121, 127));
        assert_eq!((z[2].lovel, z[2].hivel, z[2].release), (0, 26, 5.0));
        assert_eq!(
            (z[3].trigger, z[3].lovel, z[3].gain_db, z[3].rt_decay),
            (Trigger::Release, 45, -4.0, 6.0)
        );
        assert_eq!(
            (z[4].trigger, z[4].keytrack, z[4].gain_db),
            (Trigger::Release, 0.0, -37.0)
        );
        let pedal: Vec<_> = z[5..]
            .iter()
            .map(|z| (z.trigger, z.gain_db, z.lorand))
            .collect();
        assert_eq!(
            pedal,
            [
                (Trigger::PedalDown, -20.0, 0.0),
                (Trigger::PedalDown, -20.0, 0.5),
                (Trigger::PedalUp, -19.0, 0.0)
            ]
        );
    }

    #[test]
    fn paths_stay_inside_the_instrument() {
        assert_eq!(clean_path("", "a/./b/../c.wav").as_deref(), Some("a/c.wav"));
        assert_eq!(clean_path("p", "../c.wav").as_deref(), Some("c.wav"));
        assert_eq!(clean_path("", "../c.wav"), None);
        assert_eq!(clean_path("", "C:\\c.wav"), None);
        assert_eq!(clean_path("", "/etc/c.wav"), None);
    }

    #[test]
    fn empty_files_are_an_error() {
        assert!(parse("<group> volume=3", "", &mut |_| Err(String::new())).is_err());
    }
}
