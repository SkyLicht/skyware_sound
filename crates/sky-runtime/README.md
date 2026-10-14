# sky-runtime

The playback library that ships inside games made with [Skyware Sound](https://github.com/SkyLicht/skyware_sound).

Skyware Sound exports WAV files plus a `skyware-manifest.json` describing them. `sky-runtime` loads that manifest and plays the assets the way the editor meant them:

- **Loops** play their intro once (if they have one) and then repeat their body forever. The seam is sample-exact, and stays seamless when the file's sample rate differs from the output rate.
- **SFX containers** pick one of their variations per trigger (random, random without repeats, round robin or shuffle) and add a random pitch and gain offset within the ranges set in the editor.

It mixes everything into an interleaved stereo `f32` buffer that your engine sends to its audio device. It does not open an audio device itself.

## License

`sky-runtime` is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. The Skyware Sound editor is GPL-3.0-or-later, but this crate is not: you can embed it in any game, open source or commercial, without adopting the GPL. It depends only on permissively licensed crates (`sky-core`, `hound`, `thiserror`) and must never depend on GPL, AGPL, LGPL or MPL code.

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

`render` never allocates, frees, locks or panics, so you can call it from a real-time audio callback. The runtime is `Send`. If your game code and audio callback run on different threads, put it behind your own lock or drive it through a message queue.

Output is deterministic: the same seed (see `Runtime::set_seed`) and the same calls always render the same samples.

To listen to an export without a game, render an asset to a WAV file:

```text
cargo run -p sky-runtime --example render_manifest -- path/to/export battle_loop 30 battle_loop.wav
```

## Supported audio

WAV files with 8, 16, 24 or 32-bit integer samples or 32-bit float samples, mono or stereo. For files with more channels, only the first two are played. Files are decoded into memory when the manifest is loaded.

## Other engines

A C API, so engines that are not written in Rust can use the runtime, is planned for a later release.
