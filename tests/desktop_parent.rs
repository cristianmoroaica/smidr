#[cfg(target_os = "linux")]
#[test]
fn backend_exits_if_declared_desktop_parent_is_already_gone() {
    use std::process::{Command, Stdio};
    use std::time::Duration;
    use wait_timeout::ChildExt;

    let mut child = Command::new(env!("CARGO_BIN_EXE_smidr"))
        .args(["--no-browser", "--port", "0", "--parent-pid", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn smidr with a non-parent owner pid");

    let status = child
        .wait_timeout(Duration::from_secs(3))
        .expect("wait for smidr")
        .unwrap_or_else(|| {
            let _ = child.kill();
            panic!("backend stayed alive after detecting the wrong desktop parent")
        });
    assert!(status.success(), "ownership mismatch should exit cleanly: {status}");
}
