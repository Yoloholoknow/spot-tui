//! spot-tui: a terminal Spotify client that plays audio itself (as a Spotify
//! Connect device, via librespot) and shows synced lyrics. See the README and
//! `docs/ARCHITECTURE.md`.

mod api;
mod config;
mod covers;
mod http;
mod input;
mod lyrics;
mod paths;
mod pins;
mod player;
mod position;
mod runtime;
mod services;
mod state;
mod terminal;
mod ui;

const HELP: &str = "\
spot-tui -- a terminal Spotify client with synced lyrics

USAGE:
    spot-tui [OPTIONS]

OPTIONS:
    -h, --help       Print this help
    -V, --version    Print the version

Press ? inside the app for key bindings. Logs go to ~/Library/Logs/spot-tui/spot-tui.log.";

/// Logs go to a file: the TUI owns the terminal, and anything written to
/// stderr would be interleaved into the screen.
fn init_logging() {
    let path = paths::log_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    // The default has to name `info` globally: a filter naming only
    // `librespot` turns every other target off, this app's own logging
    // included.
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,librespot=debug"),
    )
    .target(env_logger::Target::Pipe(Box::new(file)))
    .init();
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("-h" | "--help") => {
            println!("{HELP}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("spot-tui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(other) => {
            eprintln!("spot-tui: unknown option `{other}`\n\n{HELP}");
            std::process::exit(2);
        }
    }
    init_logging();
    runtime::run().await
}
