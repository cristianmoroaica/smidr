mod claude;
mod claude_bridge;
mod component;
mod config;
mod core;
mod engine_config;
mod phase_dispatch;
mod image;
mod mcp_client;
mod model_session;
mod openai_engine;
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

    /// Parent process that owns this server. The desktop shell uses this to
    /// ensure a crashed or force-closed shell cannot leave a stale backend.
    #[arg(long, hide = true)]
    parent_pid: Option<u32>,
}

/// Tie the backend lifetime to its desktop owner. Linux can ask the kernel to
/// deliver SIGTERM if the parent disappears; other Unix targets use a small
/// watchdog. Normal CLI/browser launches omit the flag and are unchanged.
fn monitor_parent(parent_pid: Option<u32>) {
    let Some(parent_pid) = parent_pid else { return };

    #[cfg(target_os = "linux")]
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
            eprintln!("Startup warning: failed to bind backend lifetime to desktop parent");
        }
        if libc::getppid() as u32 != parent_pid {
            std::process::exit(0);
        }
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let alive = unsafe { libc::kill(parent_pid as i32, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            std::process::exit(0);
        }
    });

    #[cfg(not(unix))]
    let _ = parent_pid;
}

/// Preview is now the in-browser three.js viewer, so there is no external
/// viewer binary to check for.
///
/// Engine-aware: a missing `claude` CLI no longer short-circuits the python
/// check (both always run), and never aborts startup — it only disables the
/// built-in Claude engine (surfaced as `available:false` by
/// `GET /api/engines`). Every warning is printed; when claude is missing AND
/// no local engines are configured, an additional setup hint is printed so
/// the user isn't left with a server that can run no engine at all.
fn startup_checks(config: &Config) {
    let claude_missing = if let Err(e) = claude::check_claude() {
        eprintln!("Startup warning: {e}");
        true
    } else {
        false
    };
    if let Err(e) = python::check_python(&config.python_path()) {
        eprintln!("Startup warning: {e}");
    }
    if claude_missing && engine_config::load_endpoints().is_empty() {
        eprintln!(
            "Hint: no engines available — install the claude CLI, or configure a local \
             endpoint in ~/.config/smidr/engines.toml"
        );
    }
}

fn main() {
    let cli = Cli::parse();
    let _ = cli.web; // deprecated no-op, accepted for backward compatibility
    monitor_parent(cli.parent_pid);

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

    // Non-fatal startup checks — warn but continue; the server always starts.
    startup_checks(&config);

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
