//! A title DLL that cannot be loaded is named in the error.
#![cfg(windows)]

#[test]
fn a_title_dll_that_does_not_prepare_is_named() {
    let title = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(title.path().join("broken.dll"), b"MZ not a PE").unwrap();
    let wanted = |_: &str| true;
    let loaded = victory_loader::load_title_dll("broken.dll", title.path(), cache.path(), &wanted);
    let Err(error) = loaded else {
        panic!("a DLL that is not a PE loaded");
    };
    let text = format!("{error:#}");
    assert!(text.contains("broken.dll"), "{text}");
}

/// LaunchLayout's DLL search path: a DLL only in the second folder is found
/// there, and one in both is taken from the first. The files are synthetic
/// non-PEs; the error names the path the loader picked.
#[test]
fn title_dlls_are_looked_for_in_each_search_folder_in_order() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(second.path().join("only2.dll"), b"MZ second").unwrap();
    std::fs::write(first.path().join("both.dll"), b"MZ first").unwrap();
    std::fs::write(second.path().join("both.dll"), b"MZ second").unwrap();
    let wanted = |_: &str| true;
    let search = [first.path(), second.path()];
    let picked = |name: &str| {
        let Err(error) = victory_loader::load_title_dll_in(name, &search, cache.path(), &wanted) else {
            panic!("a synthetic non-PE loaded");
        };
        format!("{error:#}")
    };
    let only2 = picked("only2.dll");
    assert!(only2.contains(&second.path().join("only2.dll").display().to_string()), "{only2}");
    let both = picked("both.dll");
    assert!(both.contains(&first.path().join("both.dll").display().to_string()), "{both}");
    // Nowhere: nothing loaded, no error.
    assert!(victory_loader::load_title_dll_in("none.dll", &search, cache.path(), &wanted).unwrap().is_empty());
}
