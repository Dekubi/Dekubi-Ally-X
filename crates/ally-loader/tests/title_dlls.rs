// Adapted from victory-fork (https://github.com/fxceless/victory-fork, commit 9376fd6 + 106 VIctory fixes), a fork of
// VIctory (https://github.com/Marvisak/VIctory). GPL-3.0-or-later, by Marvisak, fxceless
// and contributors. Renamed for Dekubi X.
//! A title DLL that cannot be loaded is named in the error.
#![cfg(windows)]

#[test]
fn a_title_dll_that_does_not_prepare_is_named() {
    let title = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(title.path().join("broken.dll"), b"MZ not a PE").unwrap();
    let wanted = |_: &str| true;
    let loaded = ally_loader::load_title_dll("broken.dll", title.path(), cache.path(), &wanted);
    let Err(error) = loaded else {
        panic!("a DLL that is not a PE loaded");
    };
    let text = format!("{error:#}");
    assert!(text.contains("broken.dll"), "{text}");
}
