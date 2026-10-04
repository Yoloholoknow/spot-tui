# spot-tui

A terminal Spotify client that plays audio itself and shows synced lyrics.

spot-tui is a Spotify Connect device built on [librespot](https://github.com/librespot-org/librespot),
with a [ratatui](https://ratatui.rs) interface. It appears in your Spotify device
list as "spot-tui", resumes whatever you were last playing, and can also hand playback
to any other device.

## Features

- **Playback:** play, pause, skip, seek, volume and mute; shuffle (including smart
  shuffle) and repeat; the playbar always shows their state. Shuffle and repeat
  survive loading a new playlist or album.
- **Lyrics:** a full-sheet view that follows the song, with word-by-word
  highlighting where the source provides it. Sources are tried in order: Spicy
  Lyrics (optional, needs a key), Spotify's own, YouTube Music, then lrclib.
- **Romanized lyrics:** Japanese, Chinese and Korean lyrics in Latin letters
  (`t`), with the word highlight kept in step.
- **Album art** in terminals that support a graphics protocol (Kitty and iTerm2
  style; falls back to a text placeholder), with Ghostty and tmux handled.
- **Library:** Liked Songs, Saved Albums, Followed Artists, playlists, artist and
  album pages, the play queue and the device list, each with live filtering.
- **Playlists:** create, rename, delete, add (with a duplicate warning), remove,
  reorder (move mode), and pin. Like, follow and save from anywhere.
- **Search** with actions on results, and **quick jump** (`Ctrl+P`) to find any
  playlist, track, artist, album, device or screen by name.
- **Resilience:** reconnects with backoff when the session drops (sleep, Wi-Fi).

## Requirements

- **Spotify Premium.** librespot cannot stream on a free account.
- **ncspot, logged in once.** spot-tui reuses the login credentials ncspot
  caches in `~/.cache/ncspot/librespot/`. Install [ncspot](https://github.com/hrkfdn/ncspot),
  run it, and sign in. spot-tui never writes to that directory.
- **Rust 1.88 or newer** (edition 2024) to build.
- **Network access on the first build**, which downloads a Japanese dictionary
  (about 49 MB embedded in the binary, so a release build is roughly 59 MB).
- macOS is the tested platform. Logs use the macOS location; Linux should work
  but is untested.

## Install and run

```sh
cargo build --release
./target/release/spot-tui
```

On first run a browser window opens once for the Web API login (library,
playlists, queue and devices need it; playback does not). The login callback
uses `http://127.0.0.1:8888/callback`, so that port must be free. The token is
cached and refreshed automatically afterwards.

Press `?` in the app for key bindings, or see [docs/KEYBINDINGS.md](docs/KEYBINDINGS.md).

```
spot-tui --help
spot-tui --version
```

## Configuration

Optional. See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for `config.toml`
(quit confirmation, starting with romanized lyrics, the Spicy Lyrics key), the
environment variables, and where files and logs live.

## Documentation

| | |
|---|---|
| [docs/KEYBINDINGS.md](docs/KEYBINDINGS.md) | Every key, by screen |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | Config file, environment, file locations |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | How the code is organised, data flow, platform constraints |
| [docs/VENDORING.md](docs/VENDORING.md) | The patched librespot crates and how to re-apply them |
| [docs/mockup/index.html](docs/mockup/index.html) | The original HTML design preview |

## Development

```sh
cargo test      # unit tests; none need network or a Spotify account
cargo clippy --all-targets
```

The Spotify-facing code (`api/`, `player.rs`, the lyrics sources) is verified
by running the app, not by tests. Everything else (state, input helpers, result
handling, parsing, romanization, layout maths) is unit tested.

## Limitations

- The public Spotify Web API has no remove or reorder for the queue, no
  "top tracks" for artists, and cannot remove a single copy of a duplicated
  playlist track. The app works around what it can and says so where it cannot.
- Pins are local to spot-tui and never sync with the official app.
- Developer-mode Spotify apps are capped at 10 results per search and per
  catalogue page.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution you
intentionally submit for inclusion in this work, as defined in the Apache-2.0
license, is dual licensed as above, without any additional terms or conditions.

The patched librespot crates in `vendor/` stay under their original MIT license
([vendor/LICENSE-librespot-MIT](vendor/LICENSE-librespot-MIT)).
