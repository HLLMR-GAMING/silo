//! Tests for `organize/foreign.rs`.

use super::*;

#[test]
fn is_silo_projection_distinguishes_ours_from_the_users() {
    let base = std::env::temp_dir().join(format!("silo_proj_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let src = base.join("archived_FS25_Foo.zip");
    std::fs::write(&src, b"PK\x03\x04 canonical bytes").unwrap();
    let link = base.join("FS25_Foo.zip");

    // Our hardlink projection → ours.
    std::fs::hard_link(&src, &link).unwrap();
    assert!(is_silo_projection(&link, &src, "zip"), "hardlink is ours");
    std::fs::remove_file(&link).unwrap();

    // A byte-identical copy (cross-volume fallback) → ours.
    std::fs::copy(&src, &link).unwrap();
    assert!(
        is_silo_projection(&link, &src, "zip"),
        "identical copy is ours"
    );
    std::fs::remove_file(&link).unwrap();

    // The user's own DIFFERENT build at the same name → NOT ours (must be preserved).
    std::fs::write(&link, b"PK\x03\x04 the user's different, longer build").unwrap();
    assert!(
        !is_silo_projection(&link, &src, "zip"),
        "a user's replacement must not read as Silo's"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn is_silo_projection_dir_requires_marker() {
    let base = std::env::temp_dir().join(format!("silo_projdir_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let src = base.join("archived_dir");
    std::fs::create_dir_all(&src).unwrap();
    let link = base.join("FS25_FooDir");
    std::fs::create_dir_all(&link).unwrap();
    std::fs::write(link.join("modDesc.xml"), b"<x/>").unwrap();

    // A plain user directory at the projected name → NOT ours.
    assert!(!is_silo_projection(&link, &src, "dir"));
    // With Silo's ownership marker → ours.
    std::fs::write(link.join(SILO_MARKER), b"silo\n").unwrap();
    assert!(is_silo_projection(&link, &src, "dir"));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn detect_foreign_flags_only_non_projections() {
    let root = std::env::temp_dir().join(format!("silo_foreign_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let conn = db::open(&root.join("silo.db")).unwrap();
    let cat = root.join("archive").join("Vehicles");
    std::fs::create_dir_all(&cat).unwrap();
    let src = cat.join("FS25_Foo.zip");
    std::fs::write(&src, b"the real archived mod bytes").unwrap();
    organize_row(&conn, &root, "FS25_Foo", "Vehicles", false);

    // Nothing in the flat root → nothing flagged.
    assert!(detect_foreign_projections(&conn, &Layout::default_for(&root)).is_empty());

    // A genuine hardlink projection → NOT flagged (it's provably ours).
    let link = root.join("FS25_Foo.zip");
    std::fs::hard_link(&src, &link).unwrap();
    assert!(
        detect_foreign_projections(&conn, &Layout::default_for(&root)).is_empty(),
        "a real hardlink projection must not be flagged"
    );
    std::fs::remove_file(&link).unwrap();

    // A DIFFERENT file at that managed name → flagged as foreign.
    std::fs::write(&link, b"the user's own, different build").unwrap();
    let found = detect_foreign_projections(&conn, &Layout::default_for(&root));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].tech_name, "FS25_Foo");
    assert_eq!(found[0].file_name, "FS25_Foo.zip");

    // A loose zip with NO archived counterpart is an ordinary unorganized mod, not foreign.
    std::fs::write(root.join("FS25_Unrelated.zip"), b"loose").unwrap();
    assert_eq!(
        detect_foreign_projections(&conn, &Layout::default_for(&root)).len(),
        1
    );

    let _ = std::fs::remove_dir_all(&root);
}

// The manifest decides which archived copy a mods-folder name is compared with: a second
// file of the same name in another category, or an unmanaged file in an archive folder, is
// never anyone's counterpart.
#[test]
fn detect_foreign_compares_with_the_recorded_archive_copy_only() {
    let root = std::env::temp_dir().join(format!("silo_foreign_rows_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let archive = root.join(ARCHIVE);
    for c in ["Vehicles", "Tools", "Misc"] {
        std::fs::create_dir_all(archive.join(c)).unwrap();
    }
    let conn = db::open(&root.join("silo.db")).unwrap();
    let layout = Layout::default_for(&root);
    std::fs::write(archive.join("Vehicles/FS25_Foo.zip"), b"the managed build").unwrap();
    std::fs::write(archive.join("Tools/FS25_Foo.zip"), b"a stray second copy").unwrap();
    organize_row(&conn, &root, "FS25_Foo", "Vehicles", true);

    // A copy of the stray (not the recorded) archive file is foreign.
    let flat = root.join("FS25_Foo.zip");
    std::fs::write(&flat, b"a stray second copy").unwrap();
    let found = detect_foreign_projections(&conn, &layout);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].file_name, "FS25_Foo.zip");

    // A copy of the recorded archive file is Silo's projection.
    std::fs::write(&flat, b"the managed build").unwrap();
    assert!(detect_foreign_projections(&conn, &layout).is_empty());

    // An unmanaged file in an archive folder has no row, so its mods-folder twin isn't
    // flagged (Adopt would only answer "Not a Silo-managed mod").
    std::fs::write(archive.join("Misc/FS25_Loose.zip"), b"unmanaged").unwrap();
    std::fs::write(root.join("FS25_Loose.zip"), b"something else").unwrap();
    assert!(detect_foreign_projections(&conn, &layout).is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_foreign_file_or_link_is_never_mistaken_for_a_symlink_projection() {
    let base = std::env::temp_dir().join(format!("silo_zforeign_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let src = base.join("a.zip");
    std::fs::write(&src, b"PK\x03\x04 a").unwrap();
    let theirs = base.join("theirs.zip");
    std::fs::write(&theirs, b"PK\x03\x04 different").unwrap();
    assert!(!is_silo_projection(&theirs, &src, "zip"));

    #[cfg(unix)]
    {
        // A dangling symlink is not ours.
        let dangling = base.join("dangling.zip");
        std::os::unix::fs::symlink(base.join("gone.zip"), &dangling).unwrap();
        assert!(!is_silo_projection(&dangling, &src, "zip"));
        // A symlink to a different file is not ours — even one with identical bytes.
        let to_theirs = base.join("to_theirs.zip");
        std::os::unix::fs::symlink(&theirs, &to_theirs).unwrap();
        assert!(!is_silo_projection(&to_theirs, &src, "zip"));
        let twin = base.join("twin.zip");
        std::fs::write(&twin, b"PK\x03\x04 a").unwrap();
        let to_twin = base.join("to_twin.zip");
        std::os::unix::fs::symlink(&twin, &to_twin).unwrap();
        assert!(!is_silo_projection(&to_twin, &src, "zip"));
        // A symlink to the archived file is.
        let ours = base.join("ours.zip");
        std::os::unix::fs::symlink(&src, &ours).unwrap();
        assert!(is_silo_projection(&ours, &src, "zip"));
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Set a file's modified time `secs` seconds past the epoch, so stamps differ for sure.
fn set_mtime(path: &std::path::Path, secs: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap();
}

// #431: the mods-folder entry is still the one Silo linked, but the library's copy changed
// since. A changed library copy with the same bytes is still Silo's; different bytes are
// reported, as the library copy changing.
#[test]
fn detect_foreign_reports_a_library_copy_changed_since_it_was_linked() {
    let root = std::env::temp_dir().join(format!("silo_foreign_src_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cat = root.join(ARCHIVE).join("Vehicles");
    std::fs::create_dir_all(&cat).unwrap();
    let conn = db::open(&root.join("silo.db")).unwrap();
    let layout = Layout::default_for(&root);
    let key = crate::modsroot::canonical_key(&root);
    let src = cat.join("FS25_Foo.zip");
    std::fs::write(&src, b"the archived build").unwrap();
    set_mtime(&src, 1_700_000_000);
    organize_row(&conn, &root, "FS25_Foo", "Vehicles", true);
    // Linked as a copy (a cloud-synced folder takes no hardlinks), with both stamps recorded.
    let link = root.join("FS25_Foo.zip");
    std::fs::copy(&src, &link).unwrap();
    set_mtime(&link, 1_700_000_100);
    let (size, mtime) = file_stats(&link).unwrap();
    db::record_projection(&conn, &key, "FS25_Foo.zip", size, mtime, file_stats(&src));
    assert!(detect_foreign_projections(&conn, &layout).is_empty());

    // The library copy is rewritten with the same bytes: still Silo's, and the new stamp is kept.
    std::fs::write(&src, b"the archived build").unwrap();
    set_mtime(&src, 1_700_000_200);
    assert!(detect_foreign_projections(&conn, &layout).is_empty());
    assert_eq!(
        db::projection_src_stats(&conn, &key, "FS25_Foo.zip"),
        file_stats(&src)
    );

    // Now a different build replaces the library copy: reported, as the library copy changing.
    std::fs::write(&src, b"a newer build put in the library").unwrap();
    set_mtime(&src, 1_700_000_300);
    let found = detect_foreign_projections(&conn, &layout);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].archive_changed);
    assert_eq!(found[0].file_name, "FS25_Foo.zip");

    // Use the library's copy: it's put in the mods folder, the old one backed up, and the next
    // check finds nothing.
    restore_projection(&conn, &layout, "FS25_Foo.zip").unwrap();
    assert_eq!(
        std::fs::read(&link).unwrap(),
        b"a newer build put in the library"
    );
    assert!(detect_foreign_projections(&conn, &layout).is_empty());

    // Or keep the mods-folder copy: it becomes the library's, and the next check finds nothing.
    std::fs::remove_file(&link).unwrap();
    std::fs::write(&link, b"the archived build").unwrap();
    let (size, mtime) = file_stats(&link).unwrap();
    db::record_projection(&conn, &key, "FS25_Foo.zip", size, mtime, Some((1, 1)));
    let found = detect_foreign_projections(&conn, &layout);
    assert!(found.len() == 1 && found[0].archive_changed, "{found:?}");
    adopt_foreign(&conn, &layout, "FS25_Foo.zip").unwrap();
    assert_eq!(std::fs::read(&src).unwrap(), b"the archived build");
    assert!(detect_foreign_projections(&conn, &layout).is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

// #436: an entry a hash showed isn't Silo's copy is noted, with its stamp, so the scan can show
// it; Silo's own copy at the name clears the note.
#[test]
fn detect_foreign_notes_an_entry_that_is_not_silos() {
    let root = std::env::temp_dir().join(format!("silo_foreign_mark_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cat = root.join(ARCHIVE).join("Vehicles");
    std::fs::create_dir_all(&cat).unwrap();
    let conn = db::open(&root.join("silo.db")).unwrap();
    let layout = Layout::default_for(&root);
    let key = crate::modsroot::canonical_key(&root);
    let src = cat.join("FS25_Foo.zip");
    std::fs::write(&src, b"the archived build").unwrap();
    organize_row(&conn, &root, "FS25_Foo", "Vehicles", true);

    let link = root.join("FS25_Foo.zip");
    std::fs::write(&link, b"the user's own, different build").unwrap();
    let found = detect_foreign_projections(&conn, &layout);
    assert_eq!(found.len(), 1);
    assert!(!found[0].archive_changed, "the entry was never Silo's");
    let rec = db::load_link_records(&conn, &key)["FS25_Foo.zip"];
    assert_eq!(rec.foreign, file_stats(&link));
    assert_eq!(rec.stamp, None);

    // Silo's copy goes back in: the note goes.
    std::fs::copy(&src, &link).unwrap();
    assert!(detect_foreign_projections(&conn, &layout).is_empty());
    let rec = db::load_link_records(&conn, &key)["FS25_Foo.zip"];
    assert_eq!(rec.foreign, None);
    assert!(rec.stamp.is_some());

    let _ = std::fs::remove_dir_all(&root);
}
