# Configuration

Everything is optional except your Spotify client ID. Without a config file
spot-tui runs with defaults, but it will not start until it has a client ID.

## Spotify client ID

spot-tui does not ship a Spotify app; you register your own (free, two minutes):

1. Open the [Spotify developer dashboard](https://developer.spotify.com/dashboard)
   and create an app.
2. Add this exact redirect URI: `http://127.0.0.1:8888/callback`
3. Tick **Web API** as the API in use.
4. Copy the app's **Client ID** (public, not a secret; you do not need the client secret).
5. Put it in `config.toml` as `spotify_client_id = "..."`, or export `SPOT_TUI_CLIENT_ID`.

The playback sign-in uses librespot's own client and needs no setup; its callback
(`http://127.0.0.1:8898/login`) only needs its port free.

While the app is in development mode only your own account can log in, which is
all you need. If you change the client ID, delete `spotify_token.json` from the
cache dir so the next run logs in again.

## `config.toml`

Looked up at `spot-tui/config.toml` in the platform config directory
(macOS: `~/Library/Application Support/spot-tui/`, Linux: `~/.config/spot-tui/`).

A file that fails to parse is ignored with a warning in the log. Unknown keys
are ignored too.

The file can hold a secret API key, so keep it private: `chmod 600`.

| Key | Default | Meaning |
|-----|---------|---------|
| `spotify_client_id` | none, **required** | Client ID of your own Spotify developer app (see above). |
| `confirm_quit` | `true` | `Shift+Q` asks "Quit spot-tui? y/n" first. Set `false` to quit immediately. `Ctrl+C` always quits at once. |
| `romanize_lyrics` | `false` | Start with Japanese, Chinese and Korean lyrics shown in Latin letters. `t` toggles it at any time; this only sets the starting state. |
| `media_controls` | `true` | macOS only: show the current track in Now Playing (Control Center, Boring Notch) and accept media keys. Set `false` to turn it off. |
| `big_lyrics` | `true` | Draw lyrics as an image so they can be larger than the terminal font. Needs a graphics-capable terminal (Kitty protocol, e.g. Ghostty) and a font on the system; otherwise the normal text is used. The word-by-word sweep still works: the current line is sent once as a green copy and a white copy, and the sweep switches between them one terminal cell at a time. A tiny image (a few KB) with a soft pixel-wide gradient is drawn over the few cells under the moving edge, so the edge glides instead of stepping. Set `false` for plain text. |
| `lyrics_scale_fullscreen` | `2.5` | Lyric size in fullscreen, as a multiple of the terminal's text size. |
| `lyrics_scale_compact` | `1.3` | Lyric size in the compact Now Playing pane. |
| `lyrics_font` | none | Path to a `.ttf`/`.otf`/`.ttc` used for big lyrics before the system fonts (macOS, Linux and Windows locations are searched, with per-character fallback for CJK and Hangul). |
| `word_sync_lead_ms` | `120` | How many milliseconds the word-by-word highlight runs ahead of playback, to offset display delay. Raise it if the highlight trails the singing, set `0` if it jumps ahead. |
| `spicy_lyrics_key` | none | Key for the Spicy Lyrics developer API (`sl_sk_...`). Enables word-by-word synced lyrics from that source. Without it the source is skipped. |

```toml
spotify_client_id = "your_client_id"
confirm_quit = true
romanize_lyrics = false
media_controls = true
big_lyrics = true
lyrics_scale_fullscreen = 2.5
lyrics_scale_compact = 1.3
spicy_lyrics_key = "sl_sk_..."
```

## Environment

| Variable | Meaning |
|----------|---------|
| `SPOT_TUI_CLIENT_ID` | Overrides `spotify_client_id`. A blank value counts as unset. |
| `SPICY_LYRICS_API_KEY` | Overrides `spicy_lyrics_key`. A blank value counts as unset. |
| `RUST_LOG` | Log filter. Default `info,librespot=debug`. |

## Files

| Path | Contents |
|------|----------|
| `~/Library/Logs/spot-tui/spot-tui.log` | Log. The TUI owns the terminal, so nothing is written to stderr. |
| Cache dir (`~/Library/Caches/spot-tui/` on macOS) | Spotify token (`spotify_token.json`, owner-only, deleted by Sign out), pins (`pinned_*.json`), search and lyrics caches, librespot's volume and audio cache |
| `<cache dir>/librespot/credentials.json` | The stored playback login. Owner-only. Deleted by Sign out. |

Pins are local to spot-tui. Spotify's public API exposes no pin state, so they
never sync with the official app.
