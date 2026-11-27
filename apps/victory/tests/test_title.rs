//! The stand-in title from `tests/fixtures/test-title` (written for this
//! project; no game) run through `sixth-star run --execute` end to end:
//! the loader, its imports, the console's graphics interfaces, title
//! memory, copies and present, for 90 frames. It needs a host GPU and opens
//! a window, so it runs only with `SIXTH_REQUIRE_GPU=1` (set by `run.bat
//! tests` and `autotest.bat`); elsewhere it says so and passes.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Builds the stand-in title into its own target folder (the outer cargo
/// holds the lock on the main one while tests run).
fn build_title() -> PathBuf {
    let manifest = workspace_root().join("tests/fixtures/test-title/Cargo.toml");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("test-title");
    let status = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "--release", "--manifest-path"])
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .status()
        .expect("running cargo");
    assert!(status.success(), "building test-title failed");
    target.join("release").join("test-title.exe")
}

#[test]
fn the_stand_in_title_renders_through_the_runtime() {
    if std::env::var_os("SIXTH_REQUIRE_GPU").is_none() {
        eprintln!("the stand-in title needs a host GPU and a window: set SIXTH_REQUIRE_GPU=1 to run it");
        return;
    }
    let exe = build_title();
    // Outside the source tree: the runner refuses caches inside it.
    let dir = std::env::temp_dir().join(format!("sixth-star-test-title-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let title = dir.join("test-title.exe");
    std::fs::copy(&exe, &title).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sixth-star"))
        .args(["run", "--execute"])
        .arg(&title)
        .current_dir(&dir)
        .env("VICTORY_CACHE_DIR", dir.join("cache"))
        .env("VICTORY_DATA_DIR", dir.join("data"))
        .env("NO_COLOR", "1")
        .env("TEST_TITLE_FRAMES", "90")
        .env("TEST_TITLE_SECONDS", "60")
        .env("TEST_TITLE_COPIES", "2")
        .env_remove("VICTORY_CONFIG")
        .output()
        .expect("running sixth-star");
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("bound          test-title.exe: 1 hle, "), "{text}");
    assert!(text.contains(" 0 missing"), "{text}");
    assert!(text.contains("TEST-TITLE done frames=90 "), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
