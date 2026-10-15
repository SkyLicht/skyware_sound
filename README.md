# Skyware Sound

A native Windows digital audio workstation, written in Rust, for making game audio: seamless loops, adaptive music, layered sound effects and engine-ready exports. Its MCP integration lets AI assistants and automation tools work on projects directly.

Status: **alpha** (0.1.0-alpha.1). The core works end to end: you can build a loop in the app or have an AI assistant build it over MCP, then export it as a seamless WAV with a manifest that the `sky-runtime` library plays in a game. Expect missing features and rough edges.

## What is in the alpha

- **Project model with undo.** Every change, from the UI or an AI client, is one serializable command with an exact inverse. Projects are `.skyproj` folders (`project.json`, an `audio/` pool, renders), saved atomically, with a crash journal that recovers unsaved changes on the next open, and an autosave copy every two minutes in the app's data folder (`autosave/`), so projects that were never saved survive a crash too.
- **Real-time engine.** Instrument and audio tracks, a mixer with volume, pan, mute, solo and insert effects, and a master bus. The audio thread never allocates, locks or panics; it plays immutable snapshots that the control side swaps in at block boundaries, keeping voices and effect tails across edits. Output uses WASAPI (shared mode) through `cpal`; ASIO is behind the `asio` feature.
- **Instruments:** a polyphonic subtractive synth, a monophonic Acid bass (saw or square through a resonant ladder filter, with accents that build up on consecutive notes and slides that glide without retriggering), a synthesized drum kit (808, 909 and industrial styles on General MIDI notes, with tune, decay and level for each piece and a kick with pitch sweep, click and drive), a sampler, and a sample bank that plays sounds by name as Strudel does (`bd sd:1 hh`).
- **Strudel sound packs:** Load sounds (in the app, or the `bank_load` tool) reads Strudel's `strudel.json` sample maps, such as its drum machines (`RolandTR808`), VCSL and the Salamander piano, or any map on the web or in a folder. Files are downloaded once into a cache and keep their pack's license in the project. Only VCSL is CC0, so nothing is bundled: loop exports write the credits that licenses ask for into the manifest and warn about packs that state no license or are share-alike.
- **Effects:** gain, filter, 3-band EQ, compressor, limiter, delay (free or synced to the tempo, including dotted and triplet values), reverb (with pre-delay and a gate for gated snares), distortion, and a duck that dips a track on another track's notes, the sidechain pumping of techno and house, sample-exact and working even when the kick is muted.
- **Clips:** MIDI clips, audio clips and pattern clips written in a Tidal-style mini-notation (`bd*4, ~ sd ~ sd, hh*8`) with exact rational timing. A pattern clip can carry control patterns in the same notation, read at each note: `velocity` for accents (`127 60 90 60`) and any instrument parameter for per-step changes (`cutoff: <400 800 1600>`), plus swing from straight to 75%. Step clips are a drum-machine grid: up to 16 lanes, each its own length (so a 12-step bass line drifts against a 16-step beat), with accent, slide, tie, ratchet, probability, conditions (every other pass, fills on the last pass), micro-timing and parameter locks for each step.
- **Templates:** an empty project, a demo loop, and a four-bar techno loop at 128 BPM made only from the built-in instruments (ducked bass, accented swung hats, a dotted-delay stab), and a four-bar acid loop made only from step clips (a 12-step Acid line with accents and slides over a 16-step beat).
- **Rendering and analysis:** offline render with the same engine, WAV export (16/24-bit with dither, or 32-bit float), loudness (LUFS), true peak, clipping and a loop seam check.
- **Game loop export:** renders the loop region twice and keeps the second pass, so reverb and release tails wrap into the start, then writes the WAV and records it in `skyware-manifest.json`.
- **Game assets:** a project keeps the list of what it exports for a game (Game assets window, `game_asset_set` tool): loops, with an optional intro that plays once, and SFX sets, whose variations are parts of the arrangement (such as one per beat) with a selection rule and random pitch and gain. One export writes them all to the game's folder (`assets/audio`, say) with one manifest, removes assets the project dropped, and checks the result in `sky-runtime`.
- **MCP server:** 40 tools over Streamable HTTP on localhost (while the app runs) or stdio. See [Connecting an AI assistant](#connecting-an-ai-assistant).
- **UI** in egui: transport, arrangement, track headers, piano roll, pattern editor, step editor, mixer with meters, instrument and effect editors, an AI activity panel where you can undo what an assistant did, and a command palette (Ctrl+K).
- **Browser** (Ctrl+B), a panel on the left with the project's audio, the known sample packs and folders of samples on disk, and one search across all of them. Click an item to hear it, drag it onto an audio track, a sampler, a sample bank or below the tracks for a new one, or double-click it to add it where Import audio would. Files and folders dropped on the window from the desktop come in too: one audio file goes where Import audio puts it, several go into the project's audio, a folder joins the browser and a `strudel.json` opens Load sounds.
- **Auditioning**, with the transport stopped or running and without touching the project: the piano roll's keys and a keyboard in the Instrument tab play the track's instrument, notes sound as you add or move them (Hear notes), a bank's sounds play when clicked, and the Load sounds dialog plays a pack's sounds before loading them. Game loops play as the game will (intro, then the body repeating) or from just before their seam (Join), SFX sets play one variation per Trigger with their random pitch and gain, and Export loop can play the loop region first. Previews play after the master channel, rendered by the same code as the export.
- **`sky-runtime`:** the MIT/Apache-2.0 library a game links to play exported loops and randomized SFX containers, split between the game thread and the audio callback with lock-free queues, with volume categories and live pan and pitch. See [crates/sky-runtime/README.md](crates/sky-runtime/README.md).

Not in the alpha yet: plugins (VST3/CLAP), audio recording, automation lanes, tempo changes inside a project, the C API for `sky-runtime`, compressed game exports (Ogg Vorbis, Opus), and minidump crash reports (panics write a text report).

## Building

You need Rust 1.95 or newer (`rustup update stable`). `sky-runtime` and `sky-core`, which games embed, build with Rust 1.88.

```sh
cargo build --release
./target/release/skyware            # opens the app with the demo project
./target/release/skyware --help     # all commands and options
```

On Linux the audio backend needs ALSA headers (`sudo apt install libasound2-dev`) and the window needs `libxkbcommon-x11` at runtime. The app targets Windows; Linux builds are used for development and CI.

Useful commands:

| Command | What it does |
|---|---|
| `skyware [PROJECT.skyproj]` | Open the app (and the MCP server on port 47800) |
| `skyware serve [PROJECT]` | Headless: MCP HTTP server and audio output, no window |
| `skyware mcp [PROJECT]` | MCP over stdio with its own headless project |
| `skyware render PROJECT -o out.wav` | Render to WAV (`--start-bar`, `--end-bar`, `--tail`, `--bits`) |
| `skyware export-loop PROJECT --name "Battle Loop"` | Export the loop region for the game |
| `skyware export-game PROJECT` | Export every game asset of a project to its game folder (`--dir`, `--only NAME`) |
| `skyware play-game EXPORT_FOLDER` | Load a game export into `sky-runtime`, check every asset, then play it through the device the way a game does (`--seconds`, `--every MS`, `--asset NAME`) |
| `skyware analyze file.wav` | Loudness and peak measurements as JSON |
| `skyware check` | The system check: play through the device at 128 frames and count dropouts, then export a 16-bar loop and verify it (see [Testing on Windows](#testing-on-windows)) |
| `skyware latency-test --buffer 128 --minutes 10` | The device half of the check on its own (`--mode auto\|shared\|low-latency\|exclusive`, see [Output modes](#output-modes)) |
| `skyware devices` | List output devices |
| `skyware token` | Print the MCP access token |

## Testing on Windows

`skyware check` is the release check on real hardware. It plays a test loop through the audio device at a 128-frame buffer (`--buffer`) for `--minutes` and fails on any dropout, on any callback that took longer than its buffer, or if the device did not really run at the buffer size asked for (or at its shortest possible period, when the request is below that; see [Output modes](#output-modes)). Then it exports a 16-bar loop (`--bars`) and verifies the export as a game would use it: exact length, the manifest entry, a clean seam after quantizing to 24-bit, no clipping, and sample-exact playback in `sky-runtime` across the loop point. `--silent` runs the engine as usual but sends silence to the device, `--json` prints the report as JSON, and `--project` checks one of your projects instead of the test loop. MCP clients can run the same check with the `system_check` tool; it uses a project of its own and leaves the open one alone.

`skyware play-game FOLDER` is the same kind of check for a game export, from the engine's side. It loads the folder's manifest into `sky-runtime`, checks offline that every loop plays its intro and then repeats its body sample for sample and that every SFX set sounds and ends, then plays everything through the device for `--seconds` (default 10): the runtime split into a game-thread controller and a renderer in the device callback, loops throughout and SFX fired in turn every `--every` milliseconds. It fails on any dropout, on a render that took longer than its buffer, or on a loop that stopped. `--seconds 0` runs only the offline part, and `skyware export-game --dir FOLDER` without a project exports the demo project's two game assets to try it on.

A dropout is a hand-off of audio to the device that comes later than the device's buffer lasts. On WASAPI the device buffer is measured from the first callback, which fills it while it is empty.

To test a checkout on a Windows PC in one unattended command (rustfmt, clippy, the tests, a release build, then 10 minutes of `skyware check`):

```powershell
pwsh scripts/local-test.ps1            # -Quick: release build and 30 s on the device only
```

Logs, `report.json`, the exported loop and `summary.txt` go to `target/local-test/<time>/`; the exit code is 0 only if every step passed.

### Output modes

Windows shared mode runs at the audio engine's period, 10 ms (480 frames at 48 kHz) on most devices, whatever buffer size is asked for. To go lower, `--mode` (and Mode in the app's audio settings) picks how the app shares the device:

| Mode | What it does |
|---|---|
| `auto` (default) | Shared mode, unless `--buffer` is smaller than shared mode can run: then low-latency shared mode if the driver allows that buffer, otherwise exclusive mode. If the device refuses, it falls back to the next mode down and says why |
| `shared` | Through the Windows mixer at its usual period |
| `low-latency` | Through the Windows mixer at a shorter period (`IAudioClient3`, Windows 10 and later) where the driver allows one; other programs keep playing |
| `exclusive` | The app takes the device for itself: the shortest period the hardware can do, but other programs go silent while it plays. Windows must allow it ("Allow applications to take exclusive control" in the device's Advanced properties) |

A request below what the hardware can do runs at the hardware's shortest period. The check's buffer size passes when the device ran at the size asked for, or, for a request below anything the device can do in any mode, at that shortest period (a headset whose exclusive minimum is 3 ms passes a 128-frame check with 144-frame buffers at 48 kHz, but not in shared mode at 480). The check reports the mode it used and the device's shortest period. ASIO (`--asio`) has no modes; the driver sets them.

## Connecting an AI assistant

While the app is open it serves MCP at `http://127.0.0.1:47800/mcp`. The server only listens on localhost, rejects requests from web pages, and needs a per-install token, which `skyware token` prints (the AI panel in the app shows it too). For Claude Code:

```sh
claude mcp add --transport http skyware http://127.0.0.1:47800/mcp \
  --header "Authorization: Bearer $(skyware token 2>/dev/null)"
```

Clients that start their own servers can use stdio instead; this runs a separate headless project rather than the one open in the app:

```sh
claude mcp add skyware -- skyware mcp
```

Every tool call is one undo step and is listed in the app's AI activity panel. `--permission read|edit|files` limits what clients may do (default `files`), and file tools can only reach the `Documents/Skyware Sound` folder, the open project's folder and any `--allow-dir` folders.

## Crates

| Crate | License | Purpose |
|---|---|---|
| `sky-core` | MIT OR Apache-2.0 | IDs, musical time, the export manifest schema |
| `sky-runtime` | MIT OR Apache-2.0 | Playback library for games |
| `sky-dsp` | GPL-3.0-or-later | Instruments, effects, filters, analysis |
| `sky-pattern` | GPL-3.0-or-later | Mini-notation parser and pattern queries |
| `sky-model` | GPL-3.0-or-later | Project data, commands, undo, `.skyproj` files |
| `sky-engine` | GPL-3.0-or-later | Real-time graph, snapshot swap, offline render |
| `sky-io` | GPL-3.0-or-later | Audio file decoding and encoding, device output |
| `sky-game` | GPL-3.0-or-later | Game exports and manifests |
| `sky-library` | GPL-3.0-or-later | Strudel sample maps, sound packs and their download cache |
| `sky-session` | GPL-3.0-or-later | The live session shared by the UI and MCP |
| `sky-check` | GPL-3.0-or-later | The system check behind `skyware check`, `skyware play-game` and the `system_check` tool |
| `sky-mcp` | GPL-3.0-or-later | MCP tools and transports |
| `sky-ui` | GPL-3.0-or-later | The egui app |
| `skyware` | GPL-3.0-or-later | The executable |

## License

Skyware Sound is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. See [LICENSE](LICENSE).

`sky-runtime` and `sky-core`, which games embed, are licensed under MIT OR Apache-2.0, so games that use them do not have to adopt the GPL. CI runs `cargo deny` to keep GPL, AGPL, LGPL and MPL code out of their dependency graph. Every source file carries an SPDX license header.
