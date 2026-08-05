//! Shared black-box test harness: spawns the built `smidr` binary (the
//! server is the binary's only mode now) against a sandboxed HOME, and
//! exposes its base URL.

#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Server {
    child: Child,
    pub base: String,
    pub home: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    pub fn ws_url(&self, path: &str) -> String {
        let port = self.base.rsplit(':').next().unwrap_or("0");
        format!("ws://127.0.0.1:{port}{path}")
    }

    /// Kill the child and wait for it to exit, returning its status. Used by
    /// tests that want to assert the process shuts down cleanly (i.e. this
    /// call returns promptly, never hangs) rather than relying on `Drop`.
    pub fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let _ = self.child.kill();
        self.child.wait().expect("wait on killed child should not fail")
    }
}

pub fn spawn() -> Server {
    spawn_with_env(&[])
}

pub fn spawn_with_env(extra: &[(&str, &str)]) -> Server {
    spawn_inner(&[], extra, None)
}

/// Like `spawn`, but runs `setup` on the sandboxed HOME directory before the
/// server starts — for tests that need pre-existing state (e.g. a legacy
/// ~/MiModel root awaiting migration).
pub fn spawn_with_home_setup(setup: impl FnOnce(&std::path::Path)) -> Server {
    let home = tempfile::TempDir::new().expect("tempdir");
    setup(home.path());
    spawn_in_home(home)
}

/// Like `spawn_with_env`, but pipes `stdin_text` to the child's stdin (then
/// closes it) before the listening line is parsed — exercises the
/// piped-stdin briefing path.
pub fn spawn_with_env_and_stdin(extra: &[(&str, &str)], stdin_text: &str) -> Server {
    spawn_inner(&[], extra, Some(stdin_text))
}

/// Spawns with the deprecated `--web` no-op flag, to keep that flag
/// exercised as accepted-but-ignored for backward compatibility.
pub fn spawn_with_web() -> Server {
    spawn_inner(&["--web"], &[], None)
}

fn spawn_in_home(home: tempfile::TempDir) -> Server {
    spawn_inner_in_home(&[], &[], None, home)
}

fn spawn_inner(extra_args: &[&str], extra: &[(&str, &str)], stdin_text: Option<&str>) -> Server {
    let home = tempfile::TempDir::new().expect("tempdir");
    spawn_inner_in_home(extra_args, extra, stdin_text, home)
}

fn spawn_inner_in_home(
    extra_args: &[&str],
    extra: &[(&str, &str)],
    stdin_text: Option<&str>,
    home: tempfile::TempDir,
) -> Server {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_smidr"));
    cmd.args(["--port", "0", "--no-browser"])
        .args(extra_args)
        .env("HOME", home.path())
        .stdin(if stdin_text.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().expect("failed to spawn smidr");

    if let Some(text) = stdin_text {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("no stdin");
        stdin.write_all(text.as_bytes()).expect("write stdin");
        // Dropping `stdin` here closes the pipe, signalling EOF to the
        // child so it stops waiting for more piped input.
        drop(stdin);
    }

    let stdout = child.stdout.take().expect("no stdout");
    let mut reader = BufReader::new(stdout);

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut port: Option<u16> = None;
    let mut line = String::new();
    while Instant::now() < deadline {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                if let Some(rest) = line.trim().strip_prefix("listening on http://127.0.0.1:") {
                    if let Ok(p) = rest.trim().parse::<u16>() {
                        port = Some(p);
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }

    let port = port.unwrap_or_else(|| {
        let _ = child.kill();
        panic!("server did not print listening line within 20s");
    });

    // Keep stdout draining in the background so the child never blocks on a
    // full pipe; we no longer need to read from it.
    std::thread::spawn(move || {
        let mut discard = String::new();
        loop {
            discard.clear();
            if reader.read_line(&mut discard).unwrap_or(0) == 0 {
                break;
            }
        }
    });

    Server {
        child,
        base: format!("http://127.0.0.1:{port}"),
        home,
    }
}
