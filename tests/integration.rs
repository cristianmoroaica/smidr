mod common;

/// The binary always starts the web server (there is no more TUI mode to
/// fall back to), serves something at `/`, and shuts down cleanly — i.e.
/// `wait_for_exit()` returns promptly after being killed rather than
/// hanging forever. HOME is sandboxed to a tempdir by `tests/common::spawn`.
#[test]
fn server_starts_and_shuts_down_cleanly() {
    let mut server = common::spawn();

    let resp = ureq::get(&server.base).call();
    assert!(resp.is_ok(), "server should serve the embedded UI at /");

    let status = server.wait_for_exit();
    // The process was killed (SIGKILL), so it never reports success — the
    // real assertion is that `wait_for_exit` returned at all instead of
    // blocking forever.
    assert!(!status.success());
}

/// `--web` is kept as an accepted-but-ignored flag for backward
/// compatibility with older invocations; make sure it still starts.
#[test]
fn web_flag_is_still_accepted_as_a_noop() {
    let mut server = common::spawn_with_web();

    let resp = ureq::get(&format!("{}/api/projects", server.base)).call();
    assert!(resp.is_ok(), "--web --port 0 --no-browser should still start the server");

    let _ = server.wait_for_exit();
}
