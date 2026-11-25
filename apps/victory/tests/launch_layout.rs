//! The runner's launch layout end to end, on synthetic files only: the
//! stand-in title from `tests/fixtures/hle-probe` (built from this
//! repository), a separate package folder with a manifest we write, and the
//! `sixth-star run` binary itself, without `--execute`. No game data.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Builds the stand-in title into its own target folder (the outer cargo
/// holds the lock on the main one while tests run).
fn build_probe() -> PathBuf {
    let manifest = workspace_root().join("tests/fixtures/hle-probe/Cargo.toml");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("hle-probe");
    let status = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "--manifest-path"])
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .status()
        .expect("running cargo");
    assert!(status.success(), "building hle-probe failed");
    target.join("debug").join("hle_probe.dll")
}

/// A fresh, empty folder for one test.
fn scratch(name: &str) -> PathBuf {
    // Outside the source tree: the runner refuses caches inside it.
    let dir = std::env::temp_dir().join(format!("sixth-star-launch-layout-{}", std::process::id())).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `sixth-star run` with its caches and data kept in `root`, from `cwd`.
fn victory(root: &Path, cwd: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sixth-star"))
        .arg("run")
        .args(args)
        .current_dir(cwd)
        .env("VICTORY_CACHE_DIR", root.join("cache"))
        .env("VICTORY_DATA_DIR", root.join("data"))
        .env_remove("VICTORY_CONFIG")
        .output()
        .expect("running victory");
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code().unwrap_or(-1), text)
}

/// The line of `text` that starts with `key`, its value trimmed.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim)
}

/// An executable in `bin/` and its package elsewhere, given by relative
/// paths: the manifest (title id) and `G:` come from the package, the
/// working directory defaults to it, and the layout is reported.
#[test]
fn a_package_apart_from_the_executable_is_used_for_the_manifest() {
    let probe = build_probe();
    let root = scratch("package");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    std::fs::copy(&probe, root.join("bin/standin.exe")).unwrap();
    std::fs::write(
        root.join("pkg/MicrosoftGame.config"),
        "<?xml version=\"1.0\"?>\n<Game configVersion=\"1\"><TitleId>5E5E5E5E</TitleId></Game>\n",
    )
    .unwrap();

    let (code, text) = victory(&root, &root, &["--package", "pkg", "bin/standin.exe"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(field(&text, "package root").map(PathBuf::from), Some(root.join("pkg")), "{text}");
    assert_eq!(field(&text, "working dir").map(PathBuf::from), Some(root.join("pkg")), "{text}");
    assert_eq!(field(&text, "manifest").map(PathBuf::from), Some(root.join("pkg").join("MicrosoftGame.config")), "{text}");
    assert_eq!(field(&text, "dll search 1").map(PathBuf::from), Some(root.join("bin")), "{text}");
    assert_eq!(field(&text, "dll search 2").map(PathBuf::from), Some(root.join("pkg")), "{text}");
    assert!(field(&text, "title").is_some_and(|t| t.starts_with("5E5E5E5E")), "{text}");
    assert!(text.contains("not executing"), "{text}");
}

/// Without `--package` everything is the executable's folder, as before:
/// no manifest there means title id 0.
#[test]
fn the_executable_alone_keeps_the_old_behaviour() {
    let probe = build_probe();
    let root = scratch("alone");
    std::fs::copy(&probe, root.join("standin.exe")).unwrap();
    let (code, text) = victory(&root, &root, &["standin.exe"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(field(&text, "package root").map(PathBuf::from), Some(root.clone()), "{text}");
    assert!(field(&text, "title").is_some_and(|t| t.starts_with("00000000")), "{text}");
}

/// A wrong path stops the runner before anything is loaded, with an error
/// that names the path, and a non-zero exit code a launcher can report.
#[test]
fn a_missing_package_is_refused_by_name() {
    let probe = build_probe();
    let root = scratch("missing");
    std::fs::copy(&probe, root.join("standin.exe")).unwrap();
    let (code, text) = victory(&root, &root, &["--package", "nowhere", "standin.exe"]);
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("package root is not a folder") && text.contains("nowhere"), "{text}");
    assert!(!text.contains("mapped at"), "{text}");
}
