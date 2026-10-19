//! What each system decides for a DAW's bridge, checked on CI's macOS and Windows runs too (the `ci-os` set): whether a
//! pid is a running process of this user's (whether a bridge's discovery file is stale), and macOS's downloaded mark,
//! which Kumi's own copy of a bridge never keeps.

use daw_bridge::daw::Daw;
use daw_bridge::files::process_alive;
use daw_bridge::lifecycle::{Lifecycle, Target};

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

/// A bundle a browser downloaded carries macOS's mark into every copy made from it: the copy Kumi puts in the DAW's folder
/// loses it, on install and on rollback; the file it came from keeps it (not Kumi's to change).
#[cfg(target_os = "macos")]
#[test]
fn kumis_copy_of_a_bridge_never_keeps_macos_downloaded_mark() {
    use daw_bridge::detect::quarantined;
    use std::os::unix::ffi::OsStrExt;
    let mark = |path: &std::path::Path| {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let value = b"0083;00000000;Safari;";
        let set = unsafe { libc::setxattr(path.as_ptr(), c"com.apple.quarantine".as_ptr(), value.as_ptr().cast(), value.len(), 0, 0) };
        assert_eq!(set, 0, "{}", std::io::Error::last_os_error());
    };
    let root = tempfile::tempdir().unwrap();
    let plugins = root.path().join("REAPER").join("UserPlugins");
    let target = Target {
        daw: Daw::Reaper,
        dir: plugins.clone(),
        name: "reaper_kumi.dylib".into(),
        folder: false,
        reference: plugins.join("kumi-bridge-reference.json"),
    };
    let life = Lifecycle::new(&root.path().join("kumi"), target);
    let (one, two) = (root.path().join("one.dylib"), root.path().join("two.dylib"));
    for (source, bytes) in [(&one, b"one"), (&two, b"two")] {
        std::fs::write(source, bytes).unwrap();
        mark(source);
        assert!(quarantined(source));
    }
    life.install(&one, "0.1.0").unwrap();
    assert!(!quarantined(&plugins.join("reaper_kumi.dylib")));
    life.install(&two, "0.2.0").unwrap();
    // The rollback places the generation kept from the marked download: unmarked all the same.
    life.rollback().unwrap();
    assert_eq!(std::fs::read(plugins.join("reaper_kumi.dylib")).unwrap(), b"one");
    assert!(!quarantined(&plugins.join("reaper_kumi.dylib")));
    assert!(quarantined(&one), "the download stays as it was");
    // A tool folder, every file in it.
    let tools = root.path().join("Tools");
    let source = root.path().join("source").join("com.kumi.Bridge.xrnx");
    std::fs::create_dir_all(source.join("payload")).unwrap();
    std::fs::write(source.join("main.lua"), "-- main\n").unwrap();
    std::fs::write(source.join("payload").join("bridge.lua"), "-- bridge\n").unwrap();
    for path in [source.clone(), source.join("main.lua"), source.join("payload"), source.join("payload").join("bridge.lua")] {
        mark(&path);
    }
    let tool = Target {
        daw: Daw::Renoise,
        dir: tools.clone(),
        name: "com.kumi.Bridge.xrnx".into(),
        folder: true,
        reference: tools.join("com.kumi.Bridge.xrnx").join("bridge-reference.json"),
    };
    Lifecycle::new(&root.path().join("kumi"), tool).install(&source, "1.0.0").unwrap();
    for path in ["", "main.lua", "payload", "payload/bridge.lua"] {
        assert!(!quarantined(&tools.join("com.kumi.Bridge.xrnx").join(path)), "{path}");
    }
}
