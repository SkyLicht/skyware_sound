// SPDX-License-Identifier: GPL-3.0-or-later
//! The packs Strudel loads by default, with what their licenses say.
//!
//! Only VCSL is CC0. The others are either credited (piano), share-alike
//! (mridangam) or state no license at all, so none of them is bundled with
//! the app: each is downloaded when someone asks for it, and its audio keeps
//! its license in the project so exports can credit or warn.

use sky_model::License;

/// A known pack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackInfo {
    /// A short handle, such as `tidal-drum-machines`.
    pub id: &'static str,
    pub name: &'static str,
    /// The sample map's address.
    pub map: &'static str,
    pub license: License,
    /// The credit line the license asks for.
    pub credit: Option<&'static str>,
    /// Where the samples come from.
    pub homepage: &'static str,
    /// One line on what is in it.
    pub about: &'static str,
    /// The instrument library the pack is also, by id: into a new track it
    /// opens as that instrument instead of a sample bank.
    pub instrument: Option<&'static str>,
}

macro_rules! dough {
    ($file:literal) => {
        concat!(
            "https://raw.githubusercontent.com/felixroos/dough-samples/main/",
            $file
        )
    };
}

/// Strudel's default packs (from `felixroos/dough-samples`) and the clean
/// breaks its documentation suggests.
pub const PACKS: &[PackInfo] = &[
    PackInfo {
        id: "vcsl",
        name: "VCSL",
        map: dough!("vcsl.json"),
        license: License::Cc0,
        credit: None,
        homepage: "https://github.com/sgossner/VCSL",
        about: "Versilian Community Sample Library: orchestral, folk and percussion \
                instruments, many pitched across the keyboard",
        instrument: None,
    },
    PackInfo {
        id: "piano",
        name: "Salamander Grand Piano",
        map: dough!("piano.json"),
        license: License::CcBy3,
        credit: Some("Salamander Grand Piano by Alexander Holm (CC BY 3.0)"),
        homepage: "https://archive.org/details/SalamanderGrandPianoV3",
        about: "A Yamaha C5 grand piano, one sample every three semitones",
        instrument: Some(crate::instruments::STRUDEL_PIANO),
    },
    PackInfo {
        id: "tidal-drum-machines",
        name: "Tidal drum machines",
        map: dough!("tidal-drum-machines.json"),
        license: License::Unknown,
        credit: None,
        homepage: "https://github.com/ritchse/tidal-drum-machines",
        about: "71 classic drum machines as banks, such as RolandTR808, RolandTR909 \
                and LinnDrum",
        instrument: None,
    },
    PackInfo {
        id: "dirt-samples",
        name: "Dirt-Samples",
        map: dough!("Dirt-Samples.json"),
        license: License::Unknown,
        credit: None,
        homepage: "https://github.com/tidalcycles/Dirt-Samples",
        about: "Strudel's small selection from TidalCycles: casio, crow, insect, wind, \
                jazz, metal, east, space and numbers",
        instrument: None,
    },
    PackInfo {
        id: "emu-sp12",
        name: "E-mu SP-12",
        map: dough!("EmuSP12.json"),
        license: License::Unknown,
        credit: None,
        homepage: "https://github.com/ritchse/tidal-drum-machines",
        about: "The E-mu SP-12 drum machine as plain bd, sd, hh and more, with no bank \
                prefix",
        instrument: None,
    },
    PackInfo {
        id: "mridangam",
        name: "Mridangam",
        map: dough!("mridangam.json"),
        license: License::CcBySa4,
        credit: Some(
            "Mridangam samples (c) Arthur Carabott 2022, performed by Harishankar V Menon \
             (CC BY-SA 4.0)",
        ),
        homepage: "https://github.com/yaxu/mrid",
        about: "Strokes of the South Indian mridangam drum",
        instrument: None,
    },
    PackInfo {
        id: "clean-breaks",
        name: "Clean breaks",
        map: "https://raw.githubusercontent.com/yaxu/clean-breaks/main/strudel.json",
        license: License::Unknown,
        credit: None,
        homepage: "https://github.com/yaxu/clean-breaks",
        about: "Drum breaks cut from funk and soul records, whose rights stay with the \
                records' owners",
        instrument: None,
    },
];

/// The known pack with this id or name, ignoring case.
pub fn find(id_or_name: &str) -> Option<&'static PackInfo> {
    let want = id_or_name.trim();
    PACKS
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(want) || p.name.eq_ignore_ascii_case(want))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_have_unique_ids_and_credits_where_needed() {
        for (i, p) in PACKS.iter().enumerate() {
            assert!(PACKS[..i].iter().all(|q| q.id != p.id), "{}", p.id);
            assert_eq!(p.credit.is_some(), p.license.needs_credit(), "{}", p.id);
            assert!(p.map.starts_with("https://"), "{}", p.id);
        }
        assert_eq!(find("VCSL").map(|p| p.license), Some(License::Cc0));
        assert_eq!(find("Salamander Grand Piano").map(|p| p.id), Some("piano"));
        // The piano pack is also a piano library, with the same license.
        for p in PACKS {
            if let Some(id) = p.instrument {
                let lib = crate::find_instrument(id).expect("a known library");
                assert_eq!((lib.license, lib.credit), (p.license, p.credit), "{}", p.id);
            }
        }
        assert!(find("piano").is_some_and(|p| p.instrument.is_some()));
        assert!(find("nope").is_none());
    }
}
