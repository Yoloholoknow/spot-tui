# Architecture

spot-tui is one binary with two halves that share almost nothing:

- a **player**: librespot, registered as a Spotify Connect device, which decodes
  and plays audio and reports what it is doing through events;
- a **client**: the Spotify Web API, used for everything librespot does not
  expose (library, playlists, queue, devices, search).

The UI sits on top of both. Playback never depends on the Web API: if the
token fails to load, search and library browsing are unavailable and music and
lyrics still work.

## Module map

```
src/
  main.rs            entry: --help/--version, logging, hands off to runtime
  runtime.rs         the main loop: connect/reconnect, per-frame work, key dispatch
  player.rs          librespot connection and the playback controls
  position.rs        playback position as a state machine fed by player events
  covers.rs          album-art fetching, and prefetching the next track's
  terminal.rs        raw mode, alternate screen, graphics-protocol detection
  paths.rs           where files live
  config.rs          config.toml
  pins.rs            locally pinned playlists and tracks
  http.rs            bounded reads for third-party responses

  state/             data and pure logic. No drawing, no I/O.
    app.rs             AppState, display-order accessors, selected_track()
    nav.rs             Screen, Focus, the navigation stack
    list.rs            Fetch<T>, ListFilter, filtered_sorted, pinned_first, move helpers
    playback.rs        RepeatMode, ShuffleMode, LyricsState
    screens.rs         per-screen state (queue, devices, library, details)
    overlays.rs        confirm, text prompt, picker, quick jump
    search.rs, text.rs search state and shared cursor editing

  input/             key handling
    mod.rs             routing and the keys that work everywhere
    common.rs          keys shared by (almost) every screen
    overlays.rs        confirm, prompt, picker and quick-jump handlers
    sidebar.rs, search.rs, now_playing.rs, library.rs, playlist_detail.rs, browse.rs
    lists.rs, tracks.rs  behaviour shared across screens

  services/          Web API work off the UI thread
    mod.rs             Services: spawn a task per request
    results.rs         result types and how each is applied to AppState

  ui/                rendering (ratatui). Reads state, never changes it.
    mod.rs             render(), ScrollState, ImageState
    screens.rs, now_playing.rs, lyrics_view.rs, art.rs, playbar.rs,
    overlays.rs, help.rs, theme.rs

  lyrics/            one module per source, plus shared pieces
    pipeline.rs        the source chain and the lrclib thread
    spicy.rs, spotify.rs, ytmusic.rs, lrclib.rs   the four sources
    cache.rs, lrc.rs   on-disk cache; LRC parsing
    romanizer.rs       off-thread romanization jobs
    romanize/          Japanese, Chinese, Korean -> Latin

  api/               Spotify Web API wrappers, one file per area
vendor/              patched librespot crates (see VENDORING.md)
```

Dependencies point one way: `ui` and `input` read `state`; `input` calls
`services` and `player`; `runtime` owns everything and wires them together.
`state` depends on nothing but data types, which is what makes it testable.

## Threading and data flow

One thread runs the UI loop. Everything slow runs elsewhere and reports back
over `std::sync::mpsc` channels, which the loop drains without blocking.

```
key press ──► input::handle_key ──► Services::foo()  ──► tokio task ──► Web API
                     │                                        │
                     │ mutates AppState                       ▼
                     ▼                              CrudResult / LibraryFetchResult
                 ui::render ◄── AppState ◄── Services::apply_*  ◄── channel (drained each frame)

librespot PlayerEvent ──► Runtime::drain_player_events ──► AppState + PositionTracker
track change ──► lyrics request (debounced) ──► LyricsPipeline ──► channel ──► AppState
              └► cover fetch / next-track prefetch
```

Each frame, `Runtime::run_session` does, in order: drain player events, start a
due lyrics request, check whether the Web API client has loaded, poll the queue
if it is on screen, drain all result channels, update position and current lyric
line, draw, then wait up to one tick (100 ms, or 50 ms while a word-by-word sweep
is animating) for one terminal event.

## Invariants

- **One writer.** `AppState` is changed only on the UI thread. Tasks never touch
  it; they send results, and `Services::apply_*` applies them. There is no lock.
- **Stale answers are dropped.** Per-track async results (lyrics, cover,
  romanization) carry a *generation* counter bumped on every track change; a
  result for an older generation is discarded. Per-entity results (playlist
  tracks, artist, album) carry the entity's URI and are applied only if that
  entity is still the one on screen.
- **`NowPlaying` is the permanent root of the navigation stack.** `goto`
  collapses to `[NowPlaying, X]`; `Esc` always has somewhere to land.
- **Row N means one thing.** Key handlers resolve "the selected row" through the
  same display-order accessors on `AppState` (filter, sort and pinning applied),
  and playback uses the original index, not the display row.
- **The membership cache only makes positive claims.** A missing entry means
  "unknown", never "absent", and the duplicate check before an add always asks
  Spotify live.
- **Remote input is bounded.** HTTP bodies are size-capped with timeouts
  (`http.rs`); lyric timestamps never panic; LRC tag and line counts are capped.

## Key handling

`input::handle_key` routes each key press:

1. `Ctrl+C` quits.
2. An open **overlay** (confirm, text prompt, picker, quick jump) owns the
   keyboard, so nothing leaks to the screen underneath.
3. `Tab` and `Ctrl+P`.
4. Global letters (`s`, `r`, `t`, `Shift+Q`), unless a text field is being typed
   into.
5. The focused pane's handler (sidebar, or the screen on top of the stack).
   Each returns whether it consumed the key.
6. If not, `common::handle` takes the keys shared by nearly every screen.

So a screen only describes what is special about it, and a key such as `Space`
is defined once. Order inside a screen matters: filter editing comes first, then
move mode, then screen-specific keys; the `Shift+L` style variants must precede
the plain letter because some terminals report Shift+L as `l` plus a modifier.

## Failure behaviour

| What fails | What happens |
|------------|--------------|
| Connect session drops (sleep, Wi-Fi) | The UI shows "reconnecting" and retries with exponential backoff, 500 ms up to 10 s. It reclaims the last active session on success. |
| No stored login (first run, signed out) | A sign-in screen replaces the app; `Enter` runs the two browser logins (playback, then library), `q` quits. No connect attempts are made until signed in. |
| Spotify rejects the stored login (revoked, password changed) | The stored login is deleted and the sign-in screen shows with a note. It is not retried, since retrying cannot help. |
| Web API token refresh fails while signed in | Search and library show a clear error; playback and lyrics are unaffected. Sign out and in again to recover. |
| Web API call fails | The result carries the error text, shown in the status line or in place of the list. Spotify's response body is logged, not just the status. |
| A lyrics source fails or has nothing | Falls through to the next source; lrclib is last and can answer not-found. Not-found is cached for 7 days. |
| Cover or lyrics response is huge or hangs | Capped and timed out (`http.rs`), then treated as a miss. |
| Graphics protocol unsupported | A text placeholder replaces album art. |
| A write is rejected or lags | Counts and membership are updated locally on success, because Spotify reads can lag writes; a failed reorder refetches to undo the optimistic move. |

## Platform constraints

These shape the design and are worth knowing before changing it.

- **`Spirc::transfer` only reclaims this device.** It cannot push playback to
  another one; transferring to a different device goes through the Web API
  (`device()` / `transfer_playback()`).
- **Removing a duplicated playlist track removes every copy.** The
  position-scoped removal call was tested against a real playlist with a track
  twice: without a `snapshot_id` it removed both, with one it removed neither.
  So the app warns before removing a track that appears more than once.
- **`insert_before` for a reorder is not the target index.** Moving down needs
  `to_index + 1`, moving up needs `to_index`. This is a pure function with a test.
- **Developer-mode apps are limited.** Search and catalogue pages cap at 10
  items, and the queue endpoint omits `external_ids` from tracks, which rspotify's
  strict model rejects (handled with a lenient fallback).
- **Spotify's reads lag its writes**, so counts are patched locally after a write.
- **librespot resets shuffle and repeat on every load** unless the request
  carries them, so every load hands the current values back in.
- **Smart shuffle, shuffle after a transfer, and next-track peek need patched
  librespot**, hence `vendor/` (see [VENDORING.md](VENDORING.md)).

## Where to make common changes

| To... | Edit |
|-------|------|
| Add a key to one screen | `input/<screen>.rs`, then `ui/help.rs` and `docs/KEYBINDINGS.md` |
| Add a key to every screen | `input/common.rs` |
| Add a Web API call | `api/`, a method on `Services` (`services/mod.rs`), a result variant and its arm in `services/results.rs` |
| Add a screen | a `Screen` variant (`state/nav.rs`), state in `state/screens.rs`, a handler in `input/`, a renderer in `ui/screens.rs` |
| Add a lyrics source | a module in `lyrics/`, a step in `lyrics/pipeline.rs` |
| Change the layout | `ui/` only; it reads state and never changes it |
