# Configuration

Everything is optional. Without a config file spot-tui runs with defaults.

## `config.toml`

Looked up in this order, first match wins:

1. `spot-tui/config.toml` in the platform config directory
   (macOS: `~/Library/Application Support/spot-tui/`, Linux: `~/.config/spot-tui/`)
2. `ncspot-lyrics/config.toml` in the same parent directory. This is the app's
   earlier name and is still read so an existing file keeps working.

A file that fails to parse is ignored with a warning in the log. Unknown keys
are ignored too.

The file can hold a secret API key, so keep it private: `chmod 600`.

| Key | Default | Meaning |
|-----|---------|---------|
| `confirm_quit` | `true` | `Shift+Q` asks "Quit spot-tui? y/n" first. Set `false` to quit immediately. `Ctrl+C` always quits at once. |
| `romanize_lyrics` | `false` | Start with Japanese, Chinese and Korean lyrics shown in Latin letters. `t` toggles it at any time; this only sets the starting state. |
| `spicy_lyrics_key` | none | Key for the Spicy Lyrics developer API (`sl_sk_...`). Enables word-by-word synced lyrics from that source. Without it the source is skipped. |

```toml
confirm_quit = true
romanize_lyrics = false
spicy_lyrics_key = "sl_sk_..."
```

## Environment

| Variable | Meaning |
|----------|---------|
| `SPICY_LYRICS_API_KEY` | Overrides `spicy_lyrics_key`. A blank value counts as unset. |
| `RUST_LOG` | Log filter. Default `info,librespot=debug`. |

## Files

| Path | Contents |
|------|----------|
| `~/Library/Logs/spot-tui/spot-tui.log` | Log. The TUI owns the terminal, so nothing is written to stderr. |
| Cache dir (`~/Library/Caches/spot-tui/` on macOS) | Spotify token (`spotify_token.json`), pins (`pinned_*.json`), search and lyrics caches, librespot's volume and audio cache |
| `~/.cache/ncspot/librespot/` | **Read only.** Login credentials are reused from ncspot's cache. |

Pins are local to spot-tui. Spotify's public API exposes no pin state, so they
never sync with the official app.
