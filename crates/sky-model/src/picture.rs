// SPDX-License-Identifier: GPL-3.0-or-later
//! Pictures that stand for tracks, instruments and sample packs: a symbol
//! on a colored tile, or an image someone picked.
//!
//! An image is kept as the bytes of a small PNG or JPEG, so a track's
//! picture travels inside `project.json` with no file beside it. The
//! interface shrinks images before they get here.

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ModelError;

/// The largest image a picture holds, in bytes.
pub const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
/// How far a picture's image can be zoomed in.
pub const MAX_ZOOM: f32 = 8.0;

/// The symbols a picture can show. Each is drawn by the app, so every one
/// is free to use anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Symbol {
    Synth,
    AcidBass,
    Drums,
    DrumMachine,
    Sampler,
    SampleBank,
    Audio,
    Microphone,
    Piano,
    Strings,
    HandDrum,
    Record,
    Groovebox,
    Noise,
    Speaker,
    Folder,
}

impl Symbol {
    pub const ALL: [Symbol; 16] = [
        Symbol::Synth,
        Symbol::AcidBass,
        Symbol::Drums,
        Symbol::DrumMachine,
        Symbol::Sampler,
        Symbol::SampleBank,
        Symbol::Audio,
        Symbol::Microphone,
        Symbol::Piano,
        Symbol::Strings,
        Symbol::HandDrum,
        Symbol::Record,
        Symbol::Groovebox,
        Symbol::Noise,
        Symbol::Speaker,
        Symbol::Folder,
    ];

    /// The name people and AI clients use, such as `drum machine`.
    pub fn label(self) -> &'static str {
        match self {
            Symbol::Synth => "synth",
            Symbol::AcidBass => "acid bass",
            Symbol::Drums => "drums",
            Symbol::DrumMachine => "drum machine",
            Symbol::Sampler => "sampler",
            Symbol::SampleBank => "sample bank",
            Symbol::Audio => "audio",
            Symbol::Microphone => "microphone",
            Symbol::Piano => "piano",
            Symbol::Strings => "strings",
            Symbol::HandDrum => "hand drum",
            Symbol::Record => "record",
            Symbol::Groovebox => "groovebox",
            Symbol::Noise => "noise",
            Symbol::Speaker => "speaker",
            Symbol::Folder => "folder",
        }
    }

    /// A symbol by its label, ignoring case, spaces, dashes and underscores.
    pub fn parse(text: &str) -> Option<Symbol> {
        let squash = |s: &str| {
            s.chars()
                .filter(|c| !matches!(c, ' ' | '-' | '_'))
                .flat_map(char::to_lowercase)
                .collect::<String>()
        };
        let want = squash(text);
        Symbol::ALL.into_iter().find(|s| squash(s.label()) == want)
    }
}

/// How an image fills its frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PictureFit {
    /// Covers the frame, cropping what does not fit.
    #[default]
    Fill,
    /// Shows all of the image, with the tile's color around it.
    Fit,
}

fn one() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn is_centered(v: &[f32; 2]) -> bool {
    *v == [0.0, 0.0]
}

/// An image someone picked for a picture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PictureImage {
    /// A PNG or JPEG file's bytes, base64 in JSON.
    #[serde(serialize_with = "to_base64", deserialize_with = "from_base64")]
    pub data: Arc<[u8]>,
    /// The file it came from, shown in the picture editor.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub fit: PictureFit,
    /// 1 shows the whole image (as `fit` allows); more zooms in.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub zoom: f32,
    /// Where the frame looks when zoomed or cropped: -1 to 1 across and
    /// down, 0 in the middle.
    #[serde(default, skip_serializing_if = "is_centered")]
    pub offset: [f32; 2],
    /// How much of the track's color is laid over a track's picture, 0 to 1.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tint: f32,
}

impl PictureImage {
    pub fn new(data: Vec<u8>, name: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            name: name.into(),
            fit: PictureFit::Fill,
            zoom: 1.0,
            offset: [0.0, 0.0],
            tint: 0.0,
        }
    }
}

/// What a picture shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Picture {
    /// A symbol on a tile shaded from a color. A track's picture leaves the
    /// color out and follows the track's color.
    Symbol {
        symbol: Symbol,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
    },
    Image(PictureImage),
}

impl Picture {
    pub fn symbol(symbol: Symbol) -> Self {
        Picture::Symbol {
            symbol,
            color: None,
        }
    }

    /// Checks the values a picture holds.
    pub fn check(&self) -> Result<(), ModelError> {
        let Picture::Image(image) = self else {
            return Ok(());
        };
        let bad = |what: String| Err(ModelError::InvalidValue(what));
        if image.data.is_empty() {
            return bad("a picture's image has no data".into());
        }
        if image.data.len() > MAX_IMAGE_BYTES {
            return bad(format!(
                "a picture's image is {} KB; the most is {} KB",
                image.data.len() / 1024,
                MAX_IMAGE_BYTES / 1024
            ));
        }
        if !(1.0..=MAX_ZOOM).contains(&image.zoom) {
            return bad(format!(
                "picture zoom {} is not between 1 and {MAX_ZOOM}",
                image.zoom
            ));
        }
        if image.offset.iter().any(|o| !(-1.0..=1.0).contains(o)) {
            return bad("a picture's offset must be between -1 and 1".into());
        }
        if !(0.0..=1.0).contains(&image.tint) {
            return bad(format!(
                "picture tint {} is not between 0 and 1",
                image.tint
            ));
        }
        Ok(())
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Reads standard base64, with or without padding; white space is skipped.
pub fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut n = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            break;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(format!("{:?} is not base64", c as char)),
        };
        n = (n << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits) as u8);
        }
    }
    Ok(out)
}

fn to_base64<S: Serializer>(data: &Arc<[u8]>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&base64_encode(data))
}

fn from_base64<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<[u8]>, D::Error> {
    let text = String::deserialize(d)?;
    base64_decode(&text)
        .map(Into::into)
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let text = base64_encode(&bytes);
            assert_eq!(text.len() % 4, 0);
            assert_eq!(base64_decode(&text).unwrap(), bytes);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_decode("TWE").unwrap(), b"Ma");
        assert!(base64_decode("T*E=").is_err());
    }

    #[test]
    fn pictures_keep_their_image_in_json() {
        let mut image = PictureImage::new(vec![0x89, b'P', b'N', b'G', 0, 255], "cover.png");
        image.zoom = 1.5;
        let picture = Picture::Image(image);
        let json = serde_json::to_string(&picture).unwrap();
        assert!(json.contains("\"type\":\"image\""), "{json}");
        assert!(json.contains("iVBORwD/"), "{json}");
        let back: Picture = serde_json::from_str(&json).unwrap();
        assert_eq!(back, picture);

        let symbol = Picture::symbol(Symbol::DrumMachine);
        let json = serde_json::to_string(&symbol).unwrap();
        assert_eq!(json, r#"{"type":"symbol","symbol":"drumMachine"}"#);
        assert_eq!(serde_json::from_str::<Picture>(&json).unwrap(), symbol);
    }

    #[test]
    fn symbols_parse_from_their_labels() {
        for s in Symbol::ALL {
            assert_eq!(Symbol::parse(s.label()), Some(s));
        }
        assert_eq!(Symbol::parse("Drum-Machine"), Some(Symbol::DrumMachine));
        assert_eq!(Symbol::parse("acid_bass"), Some(Symbol::AcidBass));
        assert_eq!(Symbol::parse("kazoo"), None);
    }

    #[test]
    fn checks_catch_bad_values() {
        let mut image = PictureImage::new(vec![1, 2, 3], "x.png");
        assert!(Picture::Image(image.clone()).check().is_ok());
        image.zoom = 0.5;
        assert!(Picture::Image(image.clone()).check().is_err());
        image.zoom = 1.0;
        image.offset = [0.0, 1.5];
        assert!(Picture::Image(image.clone()).check().is_err());
        image.offset = [0.0, 0.0];
        image.tint = 2.0;
        assert!(Picture::Image(image).check().is_err());
        assert!(
            Picture::Image(PictureImage::new(Vec::new(), "empty"))
                .check()
                .is_err()
        );
        assert!(Picture::symbol(Symbol::Synth).check().is_ok());
    }
}
