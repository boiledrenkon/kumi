//! Whether a pid is a running process of this user's, as this system answers: what decides whether a bridge's discovery
//! file is stale. CI's macOS and Windows runs have it (the `ci-os` set), so each system's own calls are checked.

use daw_bridge::files::process_alive;

#[test]
fn this_users_running_process_is_alive_and_an_ended_one_or_another_users_isnt() {
    assert!(process_alive(std::process::id()));
    let mut child = if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", "exit"]).spawn().unwrap()
    } else {
        std::process::Command::new("true").spawn().unwrap()
    };
    let ended = child.id();
    child.wait().unwrap();
    assert!(!process_alive(ended));
    assert!(!process_alive(0));
    // pid 1 is root's (init, launchd): another user's process, unless these tests run as root.
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        assert!(!process_alive(1));
    }
}
