// SPDX-License-Identifier: GPL-3.0-or-later
//! The recordings of Strudel's piano pack that Skyware Sound downloads:
//! each file of the `piano` sound in `felixroos/dough-samples`' `piano.json`,
//! its size and its SHA-256. Checked by a test against [`MANIFEST_SHA256`].

/// Where the files are, by their names appended to it.
pub const BASE: &str = "https://raw.githubusercontent.com/felixroos/dough-samples/main/piano/";

/// SHA-256 of the list as `path\tbytes\tsha256\n` lines, which names this
/// set of files the way an archive's hash names an archive.
pub const MANIFEST_SHA256: &str =
    "503094ad57e7ca1cbc0a1a59a163b663c73000bd570167ffe38e1c1b35b45729";

/// Each file's path, size and SHA-256, from the lowest note up.
#[rustfmt::skip]
pub const FILES: &[(&str, u64, &str)] = &[
    ("A0v8.mp3", 317_747, "0e7869511bd3342a63fcb7df2fa1e9c4ab656d93a32d6dc472adbf98e10364d3"),
    ("C1v8.mp3", 288_101, "eca440cecae64d3665235abc0001a1d2ec9e2717ef772268755115ba80acb785"),
    ("Ds1v8.mp3", 345_891, "2675110d0f94adfafdd66a0c6d273c6897ccfc0f7419762e9c61590e01a9a690"),
    ("Fs1v8.mp3", 304_678, "9e908e308a21501ca3cd936d22922a83041674a1076528a137a31135d1b0d1a1"),
    ("A1v8.mp3", 307_723, "ed415a636cd1ac0f461f55ab3997be6fa1f9877728077a91b89b1114e4339ae2"),
    ("C2v8.mp3", 314_035, "41664af78045c416e98c990d0bab0b6c26c61da63e2a7d62d6dd4dafc5a82167"),
    ("Ds2v8.mp3", 317_075, "acd6c32d9a88675c1ae326f14727aae0a3667cc70f9ef7a39b6bc5c1a3a0342c"),
    ("Fs2v8.mp3", 292_012, "710685dd1a82b0dfa849c0056b50f4e6836c818c393e582b3c07ff17ddcacd04"),
    ("A2v8.mp3", 239_448, "ea3b12700be31a2d7fa1db672ced04802368f75f08162ce3afd20ee2c56b6b1e"),
    ("C3v8.mp3", 212_578, "790bf54430365989ae14fad704264a46bce4baedced5298d6d700a1d3c7600c6"),
    ("Ds3v8.mp3", 225_299, "dcca048dc25de903f86efddfac6b4320597deb4f300cea70f215dd1d900cdca6"),
    ("Fs3v8.mp3", 227_534, "e632f2dc532a3158693c026b93698c8ed6c6fc03c9854bb75c887542cef118da"),
    ("A3v8.mp3", 231_394, "298d319fca1ca6934ea5b07d7100002f8dfdbf8cf36fa068c8df8b5a9eb49c13"),
    ("C4v8.mp3", 219_187, "497464f6d8e0455c6b72b35bf7fed1bbd14a382b90bb389c3c88ebf9ffc8a566"),
    ("Ds4v8.mp3", 197_277, "423aeb40f9719f32892eb313236b56e39a363e13616fccc7f721ab0427eb9150"),
    ("Fs4v8.mp3", 176_999, "eefc4e8bc71867c06dbb5c926bff50ccde942d19ee885e958ec110186cf631b1"),
    ("A4v8.mp3", 173_442, "c5270f435ab7100e6fa4a363d985b7a41ab1c20ea0d12dd8fc1106e7e753cd7c"),
    ("C5v8.mp3", 172_477, "9748a6d4668a571291983c3dbe3907cb3597a17f33cdb63672f25043c685b028"),
    ("Fs5v8.mp3", 130_413, "7a0830da67ed80a322f9bd3718ef0050c8ba8a4588f7f33599b1719ad0fbdb67"),
    ("A5v8.mp3", 135_673, "4d8c5386a638ec0fd1f70081bf4b51cae2b8f6acf7ffc849052c37bb49da142c"),
    ("C6v8.mp3", 95_919, "583f570482c22e94fe37df0e31cadb005ba9f9ce742cc465311ba06c235d994d"),
    ("Ds6v8.mp3", 104_470, "fecea39137ab19a7a45a5fe97e9b338eddbab7bd9bb15746cf185f49ddcfa4e9"),
    ("Fs6v8.mp3", 83_193, "f6dd2f92614e9e07f10e0928f31ec887f6f5540952a2991b478d824ad3ffb087"),
    ("A6v8.mp3", 73_955, "1b9106440f720f7120c7ef723e4b8c95bb887e4b3d3d57d6f84bf56d34ec9535"),
    ("C7v8.mp3", 48_513, "86faf0061f6aa4e47c8b50a656650bc3a274dbd3281668fcdd7bdebec9eaf9ec"),
    ("Ds7v8.mp3", 57_956, "93af8aeafe102ac69c7ccfc447589a118392b4eb37d3a03cecc222cab8842d44"),
    ("Fs7v8.mp3", 84_955, "47ded87b96753c7f763b2f70ee777fd406fb369e0e29d957375a07e8c91bbcc6"),
    ("A7v8.mp3", 44_296, "08d8de462746153691f3ce4b3d822633713a324960f8861a172d46ffbe1bb02a"),
    ("C8v8.mp3", 56_836, "e1ecc25a213df0fe9493de7ffb101678a5d974fbbee84f0cf21e05fa9e7e40fc"),
];
