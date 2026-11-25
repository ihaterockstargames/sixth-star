//! Where a title's pieces are: four things the runner used to derive from
//! one path (the executable's folder), kept apart.
//!
//! - **executable**: the image file to map and run;
//! - **package root**: the console's package drive (`G:`), where the
//!   manifest and the title's data are;
//! - **DLL search path**: where title DLLs are looked for, in order (the
//!   executable's folder first, as the console's loader does, then the
//!   package root);
//! - **working directory**: the process's current directory while the title
//!   runs (the package root unless asked otherwise).
//!
//! By default the package root is the executable's folder, which is what
//! the runner did before: a layout made from the executable alone behaves
//! as before. Every path is made absolute against the directory the runner
//! was started in, so relative paths in a launcher's configuration mean the
//! same thing however the runner is invoked.

use std::path::{Path, PathBuf};

/// The manifest's file name in a package.
pub const MANIFEST: &str = "MicrosoftGame.config";

/// A title's layout, resolved and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchLayout {
    pub executable: PathBuf,
    pub package_root: PathBuf,
    /// The manifest in use, if one was found (package root first, then the
    /// executable's folder).
    pub manifest: Option<PathBuf>,
    /// Folders title DLLs are looked for in, in order, without repeats.
    pub dll_search: Vec<PathBuf>,
    pub working_dir: PathBuf,
}

/// What can be wrong with a requested layout; each names the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    ExecutableMissing(PathBuf),
    ExecutableNotAFile(PathBuf),
    PackageMissing(PathBuf),
    WorkingDirMissing(PathBuf),
    ManifestMissing(PathBuf),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExecutableMissing(p) => write!(f, "the executable does not exist: {}", p.display()),
            Self::ExecutableNotAFile(p) => write!(f, "the executable is not a file: {}", p.display()),
            Self::PackageMissing(p) => write!(f, "the package root is not a folder: {}", p.display()),
            Self::WorkingDirMissing(p) => write!(f, "the working directory is not a folder: {}", p.display()),
            Self::ManifestMissing(p) => write!(f, "the manifest given does not exist: {}", p.display()),
        }
    }
}

impl std::error::Error for LayoutError {}

/// `path` made absolute against `base` (unchanged if already absolute), with
/// `.` and `..` folded where that is purely textual.
pub fn absolute(path: &Path, base: &Path) -> PathBuf {
    let joined = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push(part);
                }
            }
            other => out.push(other),
        }
    }
    out
}

impl LaunchLayout {
    /// Resolves a layout. `base` is the folder relative paths are taken from
    /// (the runner's current directory). `package`, `working_dir` and
    /// `manifest` default as the module says.
    pub fn resolve(
        executable: &Path,
        package: Option<&Path>,
        working_dir: Option<&Path>,
        manifest: Option<&Path>,
        base: &Path,
    ) -> Result<LaunchLayout, LayoutError> {
        let executable = absolute(executable, base);
        if !executable.exists() {
            return Err(LayoutError::ExecutableMissing(executable));
        }
        if !executable.is_file() {
            return Err(LayoutError::ExecutableNotAFile(executable));
        }
        let exe_dir = executable.parent().map_or_else(|| base.to_path_buf(), Path::to_path_buf);
        let package_root = package.map_or_else(|| exe_dir.clone(), |p| absolute(p, base));
        if !package_root.is_dir() {
            return Err(LayoutError::PackageMissing(package_root));
        }
        let working_dir = working_dir.map_or_else(|| package_root.clone(), |p| absolute(p, base));
        if !working_dir.is_dir() {
            return Err(LayoutError::WorkingDirMissing(working_dir));
        }
        let manifest = match manifest {
            Some(m) => {
                let m = absolute(m, base);
                if !m.is_file() {
                    return Err(LayoutError::ManifestMissing(m));
                }
                Some(m)
            }
            None => [package_root.join(MANIFEST), exe_dir.join(MANIFEST)].into_iter().find(|m| m.is_file()),
        };
        let mut dll_search = vec![exe_dir];
        if !dll_search.contains(&package_root) {
            dll_search.push(package_root.clone());
        }
        Ok(LaunchLayout { executable, package_root, manifest, dll_search, working_dir })
    }

    /// The manifest's text, or empty if there is none (as before: a title
    /// without one gets id 0 and no storage quota).
    pub fn manifest_text(&self) -> String {
        self.manifest.as_deref().and_then(|m| std::fs::read_to_string(m).ok()).unwrap_or_default()
    }

    /// Where the title DLL `name` is: the first search folder that has it.
    pub fn find_dll(&self, name: &str) -> Option<PathBuf> {
        self.dll_search.iter().map(|d| d.join(name)).find(|p| p.is_file())
    }

    /// The layout as lines for the runner's startup report.
    pub fn describe(&self) -> Vec<String> {
        let mut lines = vec![
            format!("executable     {}", self.executable.display()),
            format!("package root   {}", self.package_root.display()),
            format!("manifest       {}", self.manifest.as_ref().map_or("(none: title id 0)".to_string(), |m| m.display().to_string())),
        ];
        for (i, d) in self.dll_search.iter().enumerate() {
            lines.push(format!("dll search {}   {}", i + 1, d.display()));
        }
        lines.push(format!("working dir    {}", self.working_dir.display()));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic layout: an executable in `bin/`, a package elsewhere with
    /// its manifest, DLLs in either place. Every file is our own text.
    fn fixture() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        std::fs::create_dir_all(r.join("bin")).unwrap();
        std::fs::create_dir_all(r.join("pkg/data")).unwrap();
        std::fs::create_dir_all(r.join("work")).unwrap();
        std::fs::write(r.join("bin/title.exe"), b"synthetic").unwrap();
        std::fs::write(r.join("pkg/MicrosoftGame.config"), b"<Game><TitleId>0A0B0C0D</TitleId></Game>").unwrap();
        std::fs::write(r.join("bin/both.dll"), b"bin copy").unwrap();
        std::fs::write(r.join("pkg/both.dll"), b"pkg copy").unwrap();
        std::fs::write(r.join("pkg/pkgonly.dll"), b"pkg only").unwrap();
        t
    }

    /// Without a package the executable's folder is everything, as before.
    #[test]
    fn the_executable_alone_keeps_the_old_layout() {
        let t = fixture();
        let l = LaunchLayout::resolve(&t.path().join("bin/title.exe"), None, None, None, Path::new("/")).unwrap();
        assert_eq!(l.package_root, t.path().join("bin"));
        assert_eq!(l.working_dir, t.path().join("bin"));
        assert_eq!(l.dll_search, vec![t.path().join("bin")]);
        assert_eq!(l.manifest, None);
    }

    /// A separate package: manifest and G: from it, DLLs from the executable's
    /// folder first, then the package; the working directory is the package.
    #[test]
    fn a_separate_package_is_kept_apart_from_the_executable() {
        let t = fixture();
        let r = t.path();
        let l = LaunchLayout::resolve(&r.join("bin/title.exe"), Some(&r.join("pkg")), None, None, Path::new("/")).unwrap();
        assert_eq!(l.package_root, r.join("pkg"));
        assert_eq!(l.manifest, Some(r.join("pkg/MicrosoftGame.config")));
        assert!(l.manifest_text().contains("0A0B0C0D"));
        assert_eq!(l.dll_search, vec![r.join("bin"), r.join("pkg")]);
        assert_eq!(l.find_dll("both.dll"), Some(r.join("bin/both.dll")), "the executable's folder first");
        assert_eq!(l.find_dll("pkgonly.dll"), Some(r.join("pkg/pkgonly.dll")));
        assert_eq!(l.find_dll("missing.dll"), None);
        assert_eq!(l.working_dir, r.join("pkg"));
    }

    /// Relative paths are taken from the base folder, `..` folded.
    #[test]
    fn relative_paths_are_taken_from_the_base() {
        let t = fixture();
        let r = t.path();
        let l = LaunchLayout::resolve(Path::new("bin/title.exe"), Some(Path::new("bin/../pkg")), Some(Path::new("work")), None, r).unwrap();
        assert_eq!(l.executable, r.join("bin/title.exe"));
        assert_eq!(l.package_root, r.join("pkg"));
        assert_eq!(l.working_dir, r.join("work"));
    }

    /// Each wrong path is refused with its own error, naming it.
    #[test]
    fn wrong_paths_are_refused_by_name() {
        let t = fixture();
        let r = t.path();
        let exe = r.join("bin/title.exe");
        assert_eq!(LaunchLayout::resolve(&r.join("nope.exe"), None, None, None, r), Err(LayoutError::ExecutableMissing(r.join("nope.exe"))));
        assert_eq!(LaunchLayout::resolve(&r.join("bin"), None, None, None, r), Err(LayoutError::ExecutableNotAFile(r.join("bin"))));
        assert_eq!(LaunchLayout::resolve(&exe, Some(&r.join("nope")), None, None, r), Err(LayoutError::PackageMissing(r.join("nope"))));
        assert_eq!(LaunchLayout::resolve(&exe, None, Some(&r.join("nope")), None, r), Err(LayoutError::WorkingDirMissing(r.join("nope"))));
        assert_eq!(LaunchLayout::resolve(&exe, None, None, Some(&r.join("x.config")), r), Err(LayoutError::ManifestMissing(r.join("x.config"))));
    }

    /// An explicit manifest wins over the package's.
    #[test]
    fn an_explicit_manifest_wins() {
        let t = fixture();
        let r = t.path();
        std::fs::write(r.join("work/other.config"), b"<Game><TitleId>11223344</TitleId></Game>").unwrap();
        let l = LaunchLayout::resolve(&r.join("bin/title.exe"), Some(&r.join("pkg")), None, Some(&r.join("work/other.config")), r).unwrap();
        assert!(l.manifest_text().contains("11223344"));
    }
}
