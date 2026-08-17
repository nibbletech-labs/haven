#![cfg(windows)]
//! E2E of the Windows running-exe swap (HV-297): `haven self install` must
//! replace a `haven.exe` that is currently EXECUTING. Windows forbids
//! overwriting a running image but permits renaming it; the swap is
//! rename-aside → move-in → sweep. This is the one mechanic the unit tests
//! cannot fake — a zero-sharing file lock behaves differently from a real
//! executing image — so this test runs the real thing.

use std::process::{Command, Stdio};

fn old_carcasses(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("haven.exe.") && n.ends_with(".old"))
        .collect()
}

#[test]
fn self_install_replaces_a_running_haven_exe() {
    let exe = env!("CARGO_BIN_EXE_haven");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("haven.exe");
    std::fs::copy(exe, &dest).unwrap();

    // Hold the installed copy RUNNING: `haven mcp` serves stdio until EOF.
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(&dest)
        .arg("mcp")
        .env("HAVEN_HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Ask the source binary to install itself over the running copy.
    let out = Command::new(exe)
        .args(["self", "install", "--force", "--dir"])
        .arg(dir.path())
        .env("HAVEN_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "self install over a running haven.exe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The plain replace must have failed (the image is executing), so the
    // rename-aside path ran: its carcass is still locked by the live child.
    assert!(
        !old_carcasses(dir.path()).is_empty(),
        "expected a haven.exe.<pid>.old carcass while the old image is running"
    );

    // Let the old image exit, then prove the swapped-in binary executes.
    drop(child.stdin.take());
    child.wait().unwrap();
    let ver = Command::new(&dest).arg("--version").output().unwrap();
    assert!(ver.status.success(), "swapped-in haven.exe does not run");

    // A later install sweeps the now-unlocked carcass.
    let out = Command::new(exe)
        .args(["self", "install", "--force", "--dir"])
        .arg(dir.path())
        .env("HAVEN_HOME", home.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        old_carcasses(dir.path()).is_empty(),
        "sweep should have removed the unlocked .old carcass"
    );
}
