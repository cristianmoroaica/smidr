mod claude;
mod claude_bridge;
mod component;
mod config;
mod core;
mod phase_dispatch;
mod image;
mod model_session;
mod parser;
mod phase;
mod prompt_builder;
mod python;
mod reference;
mod reference_detect;
mod server;
mod session_manager;
mod spec;
mod storage;
#[cfg(test)]
mod test_util;

use crate::config::Config;

use clap::Parser;
use std::io::{IsTerminal, Read, Write};

/// CLI arguments.
#[derive(Debug, Parser)]
#[command(name = "smidr")]
struct Cli {
    /// Deprecated no-op: the server is now the only mode.
    #[arg(long, hide = true)]
    web: bool,

    /// Port to bind the web server to (0 = ephemeral, OS-chosen).
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// Don't open a browser window when starting the web server.
    #[arg(long)]
    no_browser: bool,
}

/// Preview is now the in-browser three.js viewer, so there is no external
/// viewer binary to check for.
fn startup_checks(config: &Config) -> Result<(), String> {
    claude::check_claude()?;
    python::check_python(&config.python_path())?;
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let _ = cli.web; // deprecated no-op, accepted for backward compatibility

    // Read piped stdin (briefing) before starting the server.
    let briefing: Option<String> = if !std::io::stdin().is_terminal() {
        let mut buf = Vec::new();
        let max_bytes: usize = 100 * 1024;
        std::io::stdin().lock().take(max_bytes as u64 + 1).read_to_end(&mut buf)
            .unwrap_or_else(|e| {
                eprintln!("Warning: failed to read piped input: {e}");
                0
            });
        let truncated = buf.len() > max_bytes;
        buf.truncate(max_bytes);
        let mut s = String::from_utf8_lossy(&buf).into_owned();
        if truncated {
            s.push_str("\n[...truncated at 100KB]");
        }
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    } else {
        None
    };

    let config = Config::load();

    // Non-fatal startup checks — warn but continue
    if let Err(e) = startup_checks(&config) {
        eprintln!("Startup warning: {e}");
    }

    let no_browser = cli.no_browser;
    let result = server::run_blocking(config, cli.port, briefing, move |addr| {
        println!("listening on http://{addr}");
        let _ = std::io::stdout().flush();
        if !no_browser {
            let _ = webbrowser::open(&format!("http://127.0.0.1:{}", addr.port()));
        }
    });
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
