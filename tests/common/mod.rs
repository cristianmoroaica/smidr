//! Shared black-box test harness: spawns the built `mimodel` binary in
//! `--web` mode against a sandboxed HOME, and exposes its base URL.

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
}

pub fn spawn() -> Server {
    spawn_with_env(&[])
}

pub fn spawn_with_env(extra: &[(&str, &str)]) -> Server {
    let home = tempfile::TempDir::new().expect("tempdir");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mimodel"));
    cmd.args(["--web", "--port", "0", "--no-browser"])
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().expect("failed to spawn mimodel --web");

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
