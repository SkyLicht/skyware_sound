# sky-runtime

The playback library that ships inside games made with [Skyware Sound](https://github.com/SkyLicht/skyware_sound).

Skyware Sound exports WAV files plus a `skyware-manifest.json` describing them. `sky-runtime` loads that manifest and plays the assets the way the editor meant them:

- **Loops** play their intro once (if they have one) and then repeat their body forever. The seam is sample-exact, and stays seamless when the file's sample rate differs from the output rate.
- **SFX containers** pick one of their variations per trigger (random, random without repeats, round robin or shuffle) and add a random pitch and gain offset within the ranges set in the editor.

It mixes everything into an interleaved stereo `f32` buffer that your engine sends to its audio device. It does not open an audio device itself.

In the editor, the Game assets window (or the `game_asset_set` and `game_export` MCP tools, or `skyware export-game`) defines a project's loops and SFX sets and exports them all into one folder, such as your game's `assets/audio`. Exporting again replaces the files in place and removes assets the project dropped, so the game only ever loads the folder.

## License

`sky-runtime` is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. The Skyware Sound editor is GPL-3.0-or-later, but this crate is not: you can embed it in any game, open source or commercial, without adopting the GPL. It depends only on permissively licensed crates (`sky-core`, `hound`, `rtrb`, `thiserror`) and must never depend on GPL, AGPL, LGPL or MPL code.

## Usage

```rust,no_run
use sky_runtime::{PlayParams, Runtime};

fn main() -> Result<(), sky_runtime::RuntimeError> {
    let mut runtime = Runtime::new(48_000);
    // The folder Skyware Sound exported to.
    runtime.load_manifest("assets/audio")?;

    let music = runtime.play_by_name("battle_loop", PlayParams::default())?;
    runtime.play_by_name(
        "footstep",
        PlayParams { gain_db: -3.0, pan: -0.4, ..PlayParams::default() },
    )?;

    // In the audio callback: fill the device buffer (left, right, left, ...).
    let mut buffer = vec![0.0f32; 2 * 512];
    runtime.render(&mut buffer);

    // Fade the music out over two seconds.
    runtime.stop(music, 2_000.0);
    Ok(())
}
```

`render` never allocates, frees, locks or panics, so you can call it from a real-time audio callback.

Output is deterministic: the same seed (see `Runtime::set_seed`) and the same calls always render the same samples.

## Game thread and audio thread

When game code and the audio callback run on different threads, split the runtime: the `Controller` stays on the game thread and the `Renderer` moves into the audio callback. They talk through lock-free queues, so neither ever waits for the other, and audio that voices no longer need is handed back and freed on the game thread. With [cpal](https://crates.io/crates/cpal):

```rust,ignore
use sky_runtime::{Category, PlayParams, Runtime};

let config = device.default_output_config()?; // two channels
let mut runtime = Runtime::new(config.sample_rate());
runtime.load_manifest("assets/audio")?;
let (mut audio, mut renderer) = runtime.split(256);

let stream = device.build_output_stream(
    config.config(),
    move |out: &mut [f32], _| renderer.render(out),
    |err| eprintln!("audio: {err}"),
    None,
)?;
stream.play()?;

// Game thread:
let music = audio.play_by_name("battle_loop", PlayParams::default())?;
let step = audio.play_by_name("footstep", PlayParams { pan: -0.4, ..Default::default() })?;
audio.set_voice_pan(step, 0.4, 250.0); // follow the player across the screen
audio.set_category_gain(Category::Music, -12.0, 300.0); // duck under dialogue
// Once a frame, so finished voices are freed promptly:
audio.update();
```

The controller has the whole API except `render`: loading, `play`, `stop`, voice gain, pan and pitch, master and category gain. A split runtime renders exactly the samples the same calls on one `Runtime` would.

`skyware play-game EXPORT_FOLDER` does exactly this on your audio device: it loads an export, checks that every loop repeats sample-exactly and every SFX set plays, then plays the loops with the SFX fired in turn from a game thread and reports dropouts.

## Loading from a pack

Games that keep their files in an archive can load from memory: `Runtime::load_manifest_with(manifest_json, |path| read_from_pack(path))` takes the manifest's text and a function that returns the bytes of each audio file, by its path in the manifest (such as `sfx/footstep_01.wav`).

## Volume categories

Every asset has a category from the manifest: music, sfx, ui, voice or ambience. `set_category_gain` sets one category's volume with a ramp, for the game's volume sliders or for ducking music under dialogue; it applies to voices already playing.

To listen to an export without a game, render an asset to a WAV file:

```text
cargo run -p sky-runtime --example render_manifest -- path/to/export battle_loop 30 battle_loop.wav
```

## Supported audio

WAV files with 8, 16, 24 or 32-bit integer samples or 32-bit float samples, mono or stereo. For files with more channels, only the first two are played. Files are decoded into memory when the manifest is loaded.

## Other engines

A C API, so engines that are not written in Rust can use the runtime, is planned for a later release.
