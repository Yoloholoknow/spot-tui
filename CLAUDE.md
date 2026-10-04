# spot-tui

## Stack
- Rust 2024 (MSRV 1.88), tokio async
- TUI: ratatui + crossterm, ratatui-image (covers)
- Spotify: librespot 0.8 (core/connect vendored in `vendor/`), rspotify
- Lyrics: LRCLIB/Spicy/YTMusic/Spotify sources; romanization via lindera (embed-ipadic), pinyin, wana_kana
- Config: toml + serde, `directories` for paths

## Commands
- Build: `cargo build` (first build downloads IPADIC, needs network)
- Run: `cargo run`
- Release: `cargo build --release` (~72 MB binary)
- Test: `cargo test`
- Lint/format: `cargo clippy`, `cargo fmt`
- Rebuild code graph: `python3 scripts/graphify_build.py` (src + docs only, tests pruned; not plain `/graphify --update`)

## Conventions
- `[patch.crates-io]` in Cargo.toml must survive `cargo add/remove`; see docs/VENDORING.md
- Input handling per screen in `src/input/`
- Docs in `docs/` (architecture, config, keybindings)

## Layout
- `src/api/` Spotify API calls (album, track, playlists, search...)
- `src/ui/` ratatui rendering
- `src/input/` key handling per view
- `src/lyrics/` fetch, parse, cache, romanize pipeline
- `src/state/`, `src/services/` app state, background results
- `vendor/` patched librespot crates
