# Vendored and patched dependencies

Two librespot crates and one build dependency are patched. The patches are
wired up in the `[patch.crates-io]` section of `Cargo.toml`.

## `vergen-gitcl` (pinned to a git tag)

`librespot-core` declares `vergen-gitcl = "1.0"`. The newest version that
satisfies that, 1.0.8, has a self-inconsistent manifest (it depends on both
`vergen-lib` 0.1.6 directly and `vergen-lib` 9.1.0 through `vergen`), which is
an upstream bug: <https://github.com/librespot-org/librespot/issues/1681>. It
is pinned to the last good release through its git tag, because no matching
version exists on crates.io to pin directly.

Remove the patch once upstream publishes a fixed version.

## `vendor/librespot-core` and `vendor/librespot-connect` (0.8.0)

Stock librespot lacks a few things this app needs. The vendored copies differ
from stock in exactly the places below. **Re-apply every one on a librespot
upgrade.** The test `smart_shuffle_patch_tests` in `src/player.rs` fails (or
stops compiling) if the vendored crates are swapped back for stock ones.

### Smart shuffle

`SetOptionsCommand` drops the `modes` map the official app sends with
`set_options`, and smart shuffle is `modes.context_enhancement =
"RECOMMENDATION"`. Stock librespot therefore reverted smart shuffle to plain
shuffle.

| File | Change |
|------|--------|
| `librespot-core` `dealer/protocol/request.rs` | `modes` field on `SetOptionsCommand` |
| `librespot-connect` `state/options.rs` | `set_modes`, `set_smart_shuffle_mode`, the `smart_shuffle_active()` flag; turning shuffle off resets the mode (plus tests) |
| `librespot-connect` `state.rs` | re-export of `smart_shuffle_active` |
| `librespot-connect` `state/transfer.rs` | publish the flag after a transfer |
| `librespot-connect` `spirc.rs` | `modes` in the `SetOptions` arm, and `Spirc::smart_shuffle()` |

### Shuffle and repeat after a transfer

| File | Change |
|------|--------|
| `librespot-connect` `spirc.rs` | re-announce shuffle/repeat after a transfer applies its options. Stock announces the stale pre-transfer values, so a launch always showed shuffle off. |

### Next-track prefetch

The app warms the album art and lyrics caches for the track queued next, using
librespot's own already-maintained state instead of polling the Web API queue.

| File | Change |
|------|--------|
| `librespot-connect` `spirc.rs` | `SpircCommand::PeekNextTrack` (a oneshot query) and `Spirc::peek_next_track()`; `#[derive(Clone)]` on `Spirc` |
| `librespot-connect` `state/tracks.rs` | `peek_next_track_uri()` |

### Lint

| File | Change |
|------|--------|
| `librespot-core` `authentication.rs` | `#[expect(deprecated)]` changed to `#[allow(...)]`, silencing a lint that only warns because the crate is a path dependency |

## Licensing

The vendored librespot crates are MIT licensed, and their license text is kept
in `vendor/LICENSE-librespot-MIT`. Keep it when re-applying patches on an
upgrade. The rest of the repo is `MIT OR Apache-2.0` (see the README).
