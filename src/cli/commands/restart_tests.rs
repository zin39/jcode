use super::{
    maybe_run_pending_restart_restore_on_startup, run_restart_clear_command,
    run_restart_save_command,
};
use crate::session::Session;
use std::ffi::OsString;

struct TestEnvGuard {
    prev_home: Option<OsString>,
    prev_socket: Option<OsString>,
    _temp_home: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TestEnvGuard {
    fn new() -> anyhow::Result<Self> {
        let lock = crate::storage::lock_test_env();
        let temp_home = tempfile::Builder::new()
            .prefix("jcode-cli-restart-test-home-")
            .tempdir()?;
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", temp_home.path());

        // Redirecting JCODE_HOME alone does NOT make these tests hermetic.
        // `run_restart_save_command` first calls `capture_connected_restart_snapshot`,
        // which dials the debug socket, and that path resolves through
        // `storage::runtime_dir()` / `JCODE_SOCKET` -- neither of which is derived
        // from JCODE_HOME. So whenever the suite runs in a shell that has
        // JCODE_SOCKET exported (any self-dev session), or simply has a live
        // server in TMPDIR, the test talks to the developer's REAL jcode server
        // and fails with "Debug control is disabled".
        //
        // Point the socket at a path inside the temp home instead. Nothing
        // listens there, `Client::connect_debug()` returns Err, and the command
        // takes its intended offline branch (`Ok(None)` -> save a local snapshot).
        let prev_socket = std::env::var_os("JCODE_SOCKET");
        crate::env::set_var("JCODE_SOCKET", temp_home.path().join("jcode.sock"));

        Ok(Self {
            prev_home,
            prev_socket,
            _temp_home: temp_home,
            _lock: lock,
        })
    }
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        restore_var("JCODE_HOME", self.prev_home.as_ref());
        restore_var("JCODE_SOCKET", self.prev_socket.as_ref());
    }
}

fn restore_var(key: &str, prev: Option<&OsString>) {
    match prev {
        Some(value) => crate::env::set_var(key, value),
        None => crate::env::remove_var(key),
    }
}

#[tokio::test]
async fn restart_save_writes_empty_snapshot_with_auto_restore_flag() {
    let _guard = TestEnvGuard::new().expect("setup test env");

    run_restart_save_command(true)
        .await
        .expect("save restart snapshot");

    let snapshot = crate::restart_snapshot::load_snapshot().expect("load snapshot");
    assert!(snapshot.auto_restore_on_next_start);
    assert!(snapshot.sessions.is_empty());
}

#[tokio::test]
async fn pending_restore_returns_false_for_unarmed_snapshot() {
    let _guard = TestEnvGuard::new().expect("setup test env");

    run_restart_save_command(false)
        .await
        .expect("save restart snapshot");

    assert!(
        !maybe_run_pending_restart_restore_on_startup()
            .await
            .expect("check pending restore")
    );
    assert!(crate::restart_snapshot::load_snapshot().is_ok());
}

#[tokio::test]
async fn pending_restore_does_not_auto_restore_recent_crash_without_snapshot() {
    let _guard = TestEnvGuard::new().expect("setup test env");

    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn child");
    let dead_pid = child.id();
    let _ = child.wait().expect("wait for child");

    let mut crashed = Session::create_with_id(
        "session_no_startup_auto_restore_crash".to_string(),
        None,
        Some("Do Not Respawn".to_string()),
    );
    crashed.mark_active_with_pid(dead_pid);
    crashed.save().expect("save active session with dead pid");

    assert!(
        !maybe_run_pending_restart_restore_on_startup()
            .await
            .expect("check pending restore")
    );
    assert!(crate::restart_snapshot::load_snapshot().is_err());
}

#[tokio::test]
async fn restart_clear_removes_saved_snapshot() {
    let _guard = TestEnvGuard::new().expect("setup test env");

    run_restart_save_command(false)
        .await
        .expect("save restart snapshot");
    run_restart_clear_command().expect("clear restart snapshot");

    assert!(crate::restart_snapshot::load_snapshot().is_err());
}
