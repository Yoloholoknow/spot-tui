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
| `spicy_lyrics_key` | none | Key for the Spicy Lyrics developer API (`sl_sk_...`). Enables word-by-word synced lyrics from that source. Without it the source is skipped. |

```toml
spotify_client_id = "your_client_id"
confirm_quit = true
romanize_lyrics = false
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
