//! Tests for `organize/engine.rs`.

use super::*;

#[test]
fn cleaning_an_empty_archive_removes_silos_own_note() {
    let base = std::env::temp_dir().join(format!("silo_clean_note_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let archive = base.join("hdd");
    std::fs::create_dir_all(archive.join("Vehicles")).unwrap();
    std::fs::write(archive.join(crate::relocate::ARCHIVE_NOTE), b"{}").unwrap();
    let leftover = cleanup_empty_archive(&archive, true).unwrap();
    assert!(
        leftover.is_empty(),
        "the note is not reported as a leftover: {leftover:?}"
    );
    assert!(!archive.exists());
    // Kept when the root is kept (a user-chosen folder stays as it is).
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::write(archive.join(crate::relocate::ARCHIVE_NOTE), b"{}").unwrap();
    assert!(cleanup_empty_archive(&archive, false).unwrap().is_empty());
    assert!(archive.join(crate::relocate::ARCHIVE_NOTE).exists());
    // An archive that still holds something keeps its note too.
    let orphan = archive.join("Tools").join("orphan.zip");
    std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    std::fs::write(&orphan, b"PK").unwrap();
    assert_eq!(cleanup_empty_archive(&archive, true).unwrap(), vec![orphan]);
    assert!(archive.join(crate::relocate::ARCHIVE_NOTE).exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn cleanup_never_deletes_unknown_files() {
    let base = std::env::temp_dir().join(format!("silo_flatten_a_{}", std::process::id()));
    let archive = base.join(ARCHIVE);
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(archive.join("Maps")).unwrap(); // empty → should be removed
    std::fs::create_dir_all(archive.join("Vehicles")).unwrap();
    // An orphan the manifest doesn't know about (e.g. a move whose DB write crashed).
    let orphan = archive.join("Vehicles").join("orphan.zip");
    std::fs::write(&orphan, b"PK\x03\x04").unwrap();

    let leftover = cleanup_empty_archive(&archive, true).unwrap();

    assert!(orphan.exists(), "orphan file must NOT be deleted");
    assert_eq!(leftover.len(), 1);
    assert!(
        !archive.join("Maps").exists(),
        "empty dir should be removed"
    );
    assert!(archive.join("Vehicles").exists(), "non-empty dir kept");
    assert!(
        archive.exists(),
        "archive kept because it still holds the orphan"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn flatten_never_deletes_the_original_when_no_archive_copy_exists() {
    // Simulate the crash window: a manifest row was written but the move never happened,
    // so the ORIGINAL still sits in the flat root and there is no archived copy. Flatten
    // must keep the file and only clear the stale row — never delete the only copy.
    let base = std::env::temp_dir().join(format!("silo_crashwin_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();

    let key = crate::modsroot::canonical_key(&base);

    let row = OrganizedRow {
        tech_name: "FS25_Foo".into(),
        file_name: "FS25_Foo.zip".into(),
        kind: "zip".into(),
        category: "Vehicles".into(),
        subcategory: None,
        active: false,
    };
    db::upsert_organized(&conn, &key, &row).unwrap();
    let original = base.join("FS25_Foo.zip");
    std::fs::write(&original, b"PK\x03\x04 the user's only copy").unwrap();
    // No archive/Vehicles/FS25_Foo.zip exists.

    let rep = flatten(&conn, &Layout::default_for(&base));

    assert!(original.exists(), "the only copy must NOT be deleted");
    assert!(
        db::load_organized(&conn, &key).is_empty(),
        "the stale manifest row should be cleared"
    );
    assert!(
        rep.errors.iter().any(|e| e.contains("kept the file")),
        "the kept-file case should be reported, got: {:?}",
        rep.errors
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn cleanup_removes_a_fully_empty_archive() {
    let base = std::env::temp_dir().join(format!("silo_flatten_b_{}", std::process::id()));
    let archive = base.join(ARCHIVE);
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(archive.join("A").join("B")).unwrap();

    let leftover = cleanup_empty_archive(&archive, true).unwrap();

    assert!(leftover.is_empty());
    assert!(!archive.exists(), "a fully-empty archive tree is removed");

    let _ = std::fs::remove_dir_all(&base);
}

// A crash between projecting a mod and recording it leaves Silo's projection in place with
// `active = false`; the next activation recognises it and fixes the flag (and the record).
#[test]
fn activation_adopts_silos_own_unrecorded_projection() {
    let base = std::env::temp_dir().join(format!("silo_act_own_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let key = crate::modsroot::canonical_key(&base);
    let layout = Layout::default_for(&base);
    let src = base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip");
    std::fs::write(&src, b"PK\x03\x04 the archived build").unwrap();
    organize_row(&conn, &base, "FS25_Foo", "Vehicles", false);
    // A byte-identical copy (the cross-drive projection), with no projection record.
    let flat = base.join("FS25_Foo.zip");
    std::fs::copy(&src, &flat).unwrap();
    assert!(db::projection_stats(&conn, &key, "FS25_Foo.zip").is_none());

    let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
    let rep = set_active(&conn, &layout, &on);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.changed, 1);
    assert!(
        db::load_organized(&conn, &key)[0].active,
        "the flag is fixed"
    );
    assert!(
        matches_provenance(&conn, &key, &flat, "FS25_Foo.zip"),
        "the projection is recorded, so later checks need no hash"
    );

    // Nothing left to reconcile.
    let rep = set_active(&conn, &layout, &on);
    assert_eq!((rep.changed, rep.skipped), (0, 1));

    let _ = std::fs::remove_dir_all(&base);
}

// A different build at a wanted mod's name is reported by name, never counted as active, and
// never touched.
#[test]
fn activation_reports_a_foreign_file_in_the_mods_slot() {
    let base = std::env::temp_dir().join(format!("silo_act_foreign_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let key = crate::modsroot::canonical_key(&base);
    let layout = Layout::default_for(&base);
    std::fs::write(
        base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip"),
        b"PK\x03\x04 the archived build",
    )
    .unwrap();
    organize_row(&conn, &base, "FS25_Foo", "Vehicles", false);
    let flat = base.join("FS25_Foo.zip");
    std::fs::write(&flat, b"PK\x03\x04 the user's other build").unwrap();

    let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
    let rep = set_active(&conn, &layout, &on);
    assert_eq!(rep.errors.len(), 1);
    assert!(rep.errors[0].contains("FS25_Foo.zip"), "{:?}", rep.errors);
    assert_eq!(rep.changed, 0);
    assert!(!db::load_organized(&conn, &key)[0].active);
    assert_eq!(
        std::fs::read(&flat).unwrap(),
        b"PK\x03\x04 the user's other build"
    );

    let _ = std::fs::remove_dir_all(&base);
}

// Another mod of the same tech name already in the mods folder (an unzipped folder beside an
// archived zip, #421) is the copy the game loads: turning the name on never links a second copy
// beside it. Without that copy the archived zip is linked as usual.
#[test]
fn activation_never_links_a_second_copy_of_a_loaded_tech_name() {
    let base = temp_dir_for("act_second_copy");
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let key = crate::modsroot::canonical_key(&base);
    let layout = Layout::default_for(&base);
    std::fs::write(base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip"), b"PK").unwrap();
    // Still flagged active (it was linked before the folder came), so the flag is put right.
    organize_row(&conn, &base, "FS25_Foo", "Vehicles", true);
    let folder = base.join("FS25_Foo");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("modDesc.xml"), "<modDesc/>").unwrap();

    let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
    let rep = set_active(&conn, &layout, &on);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.changed, 1);
    assert!(!db::load_organized(&conn, &key)[0].active);
    let rep = set_active(&conn, &layout, &on);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!((rep.changed, rep.skipped), (0, 1));
    assert_eq!(
        rep.other_copies,
        vec!["FS25_Foo".to_string()],
        "the Library is told why"
    );
    assert!(base.join("FS25_Foo.zip").symlink_metadata().is_err());
    assert!(!db::load_organized(&conn, &key)[0].active);
    assert!(
        folder.join("modDesc.xml").is_file(),
        "the folder is left as it is"
    );

    // A folder that isn't a mod (no modDesc.xml) loads nothing, so it doesn't count.
    std::fs::remove_file(folder.join("modDesc.xml")).unwrap();
    let rep = set_active(&conn, &layout, &on);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.changed, 1);
    assert!(rep.other_copies.is_empty());
    assert!(base.join("FS25_Foo.zip").is_file());
    assert!(db::load_organized(&conn, &key)[0].active);

    let _ = std::fs::remove_dir_all(&base);
}

// The other half of an interrupted park: the link is gone but the row still says active.
#[test]
fn a_parked_mod_still_marked_active_is_reconciled() {
    let base = std::env::temp_dir().join(format!("silo_act_stale_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let key = crate::modsroot::canonical_key(&base);
    std::fs::write(base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip"), b"PK").unwrap();
    organize_row(&conn, &base, "FS25_Foo", "Vehicles", true);
    db::record_projection(&conn, &key, "FS25_Foo.zip", 2, 1);

    let rep = set_active(&conn, &Layout::default_for(&base), &HashSet::new());
    assert_eq!(rep.changed, 1);
    assert!(!db::load_organized(&conn, &key)[0].active);
    assert!(
        db::projection_stats(&conn, &key, "FS25_Foo.zip").is_none(),
        "the gone projection's record is dropped"
    );
    assert!(!base.join("FS25_Foo.zip").exists());

    // An inactive row can keep a record too (a crash after linking, before the flag, then
    // the file removed): dropped all the same, and nothing counted as changed.
    db::record_projection(&conn, &key, "FS25_Foo.zip", 2, 1);
    let rep = set_active(&conn, &Layout::default_for(&base), &HashSet::new());
    assert_eq!((rep.changed, rep.skipped), (0, 1));
    assert!(db::projection_stats(&conn, &key, "FS25_Foo.zip").is_none());

    let _ = std::fs::remove_dir_all(&base);
}

// A slot that can't be looked at isn't "nothing there": the flag and record stay as they are.
#[cfg(unix)]
#[test]
fn a_slot_that_cant_be_checked_is_reported_not_reconciled() {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!("silo_act_unreadable_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("mods");
    std::fs::create_dir_all(mods.join(ARCHIVE).join("Vehicles")).unwrap();
    std::fs::write(mods.join(ARCHIVE).join("Vehicles/FS25_Foo.zip"), b"PK").unwrap();
    std::fs::write(mods.join("FS25_Foo.zip"), b"PK").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let key = crate::modsroot::canonical_key(&mods);
    organize_row(&conn, &mods, "FS25_Foo", "Vehicles", true);
    db::record_projection(&conn, &key, "FS25_Foo.zip", 2, 1);
    std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o000)).unwrap();
    if mods.join("FS25_Foo.zip").symlink_metadata().is_ok() {
        // Running as root: permissions don't stop the look, so there's nothing to test.
        std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
        return;
    }
    let rep = set_active(&conn, &Layout::default_for(&mods), &HashSet::new());
    std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(rep.errors.len(), 1, "{:?}", rep.errors);
    assert_eq!(rep.changed, 0);
    assert!(db::load_organized(&conn, &key)[0].active);
    assert!(db::projection_stats(&conn, &key, "FS25_Foo.zip").is_some());
    assert!(mods.join("FS25_Foo.zip").exists());
    let _ = std::fs::remove_dir_all(&base);
}

// A copy-projection deactivates via the provenance fast-path (no hash) once recorded, and a
// file the user swapped in — same name, different bytes — is never removed.
#[test]
fn deactivate_uses_provenance_and_never_deletes_a_swapped_file() {
    let base = std::env::temp_dir().join(format!("silo_prov_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let key = crate::modsroot::canonical_key(&base);
    let cat = base.join(ARCHIVE).join("Vehicles");
    std::fs::create_dir_all(&cat).unwrap();
    let bytes = b"PK\x03\x04 canonical archived build";
    std::fs::write(cat.join("FS25_Foo.zip"), bytes).unwrap();
    db::upsert_organized(
        &conn,
        &key,
        &OrganizedRow {
            tech_name: "FS25_Foo".into(),
            file_name: "FS25_Foo.zip".into(),
            kind: "zip".into(),
            category: "Vehicles".into(),
            subcategory: None,
            active: false,
        },
    )
    .unwrap();

    // Activate: set_active copies (temp dir may support hardlinks, but either way it records
    // provenance) and the flat file exists.
    let mut want = std::collections::HashSet::new();
    want.insert("FS25_Foo".to_string());
    let rep = set_active(&conn, &Layout::default_for(&base), &want);
    assert_eq!(rep.changed, 1);
    let flat = base.join("FS25_Foo.zip");
    assert!(flat.exists());
    assert!(
        db::projection_stats(&conn, &key, "FS25_Foo.zip").is_some(),
        "provenance recorded on activate"
    );
    assert!(matches_provenance(&conn, &key, &flat, "FS25_Foo.zip"));

    // Deactivate: provenance matches → removed, provenance forgotten, archive untouched.
    let rep = set_active(
        &conn,
        &Layout::default_for(&base),
        &std::collections::HashSet::new(),
    );
    assert_eq!(rep.changed, 1);
    assert!(!flat.exists(), "our own projection is removed");
    assert!(db::projection_stats(&conn, &key, "FS25_Foo.zip").is_none());
    assert!(cat.join("FS25_Foo.zip").exists(), "archive kept");

    // Now the user drops in their OWN different build at that name, and marks it active in the
    // manifest (as if it were projected). A stale/absent provenance record must NOT let it be
    // deleted — different bytes, so both the fast path and the hash check refuse.
    std::fs::write(&flat, b"the user's OWN irreplaceable build").unwrap();
    db::set_organized_active(&conn, &key, "FS25_Foo", true).unwrap();
    let rep = set_active(
        &conn,
        &Layout::default_for(&base),
        &std::collections::HashSet::new(),
    );
    assert_eq!(rep.skipped, 1, "a swapped-in file is left untouched");
    assert_eq!(
        std::fs::read(&flat).unwrap(),
        b"the user's OWN irreplaceable build",
        "the user's file must survive"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn records_from_one_folder_never_authorize_changes_in_another() {
    let base = std::env::temp_dir().join(format!("silo_iso_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(a.join("archive").join("Vehicles")).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();

    // Folder A: one organized mod, projected (active).
    std::fs::write(a.join("archive/Vehicles/FS25_Foo.zip"), b"PK\x03\x04 foo").unwrap();
    let key_a = crate::modsroot::canonical_key(&a);
    let row = OrganizedRow {
        tech_name: "FS25_Foo".into(),
        file_name: "FS25_Foo.zip".into(),
        kind: "zip".into(),
        category: "Vehicles".into(),
        subcategory: None,
        active: false,
    };
    db::upsert_organized(&conn, &key_a, &row).unwrap();
    let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
    set_active(&conn, &Layout::default_for(&a), &on);
    assert!(a.join("FS25_Foo.zip").exists());

    // Folder B: the user's own file at the same name, unknown to Silo.
    std::fs::write(b.join("FS25_Foo.zip"), b"PK\x03\x04 the user's own file").unwrap();

    // Deactivating everything in B must not touch B's file, and must not touch A.
    set_active(&conn, &Layout::default_for(&b), &HashSet::new());
    flatten(&conn, &Layout::default_for(&b));
    assert!(
        b.join("FS25_Foo.zip").exists(),
        "B's file is not Silo's. Never removed"
    );
    assert!(
        a.join("FS25_Foo.zip").exists(),
        "A's projection is untouched"
    );
    assert!(
        db::load_organized(&conn, &key_a)[0].active,
        "A's state is untouched"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #322: filing a mod carries its scan-cache row to the archived path, so the scan after
/// filing reuses it instead of parsing every filed mod again.
#[test]
fn filing_keeps_the_scan_cache_so_the_next_scan_reuses_it() {
    let base = std::env::temp_dir().join(format!("silo_filecache_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("mods");
    let arch = mods.join("archive");
    std::fs::create_dir_all(&mods).unwrap();
    let mut conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: arch.clone(),
    };
    std::fs::write(mods.join("FS25_Foo.zip"), b"PK foo").unwrap();

    // First scan: parsed fresh, then cached the way scan_mods caches it.
    let scan = |conn: &Connection| {
        crate::scan::scan_layouts(
            vec![(mods.clone(), vec![arch.clone()])],
            &db::load_cache(conn),
            |_, _| {},
        )
    };
    let first = scan(&conn);
    assert_eq!(first.fresh_paths.len(), 1, "parsed once");
    let rows: Vec<(String, u64, u64, String)> = first
        .result
        .mods
        .iter()
        .map(|m| {
            (
                m.path.clone(),
                m.mtime_ms,
                m.size,
                serde_json::to_string(m).unwrap(),
            )
        })
        .collect();
    db::upsert_many(&mut conn, &rows).unwrap();

    let input = ModInput {
        tech_name: "FS25_Foo".into(),
        file_name: "FS25_Foo.zip".into(),
        kind: "zip".into(),
        category: "Vehicles".into(),
        subcategory: None,
    };
    let rep = apply_organize(&conn, &layout, &mods, std::slice::from_ref(&input));
    assert_eq!(rep.changed, 1);
    let filed = arch.join("Vehicles").join("FS25_Foo.zip");
    assert!(filed.exists());

    // The scan after filing finds the mod at its new path without parsing it again.
    let second = scan(&conn);
    assert!(
        second.fresh_paths.is_empty(),
        "reused the cache, parsed: {:?}",
        second.fresh_paths
    );
    let m = &second.result.mods[0];
    assert_eq!(m.path, filed.to_string_lossy());
    assert!(m.organized);
    let cache = db::load_cache(&conn);
    assert!(cache.contains_key(filed.to_string_lossy().as_ref()));
    assert!(!cache.contains_key(mods.join("FS25_Foo.zip").to_string_lossy().as_ref()));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn organize_activate_and_flatten_honor_a_custom_archive() {
    let base = std::env::temp_dir().join(format!("silo_customarch_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("mods");
    let arch = base.join("elsewhere");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::create_dir_all(&arch).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: arch.clone(),
    };
    std::fs::write(mods.join("FS25_Foo.zip"), b"PK\x03\x04 foo").unwrap();
    let input = ModInput {
        tech_name: "FS25_Foo".into(),
        file_name: "FS25_Foo.zip".into(),
        kind: "zip".into(),
        category: "Vehicles".into(),
        subcategory: None,
    };

    let rep = apply_organize(&conn, &layout, &mods, std::slice::from_ref(&input));
    assert_eq!(rep.changed, 1);
    assert!(
        arch.join("Vehicles/FS25_Foo.zip").exists(),
        "filed into the custom archive"
    );
    assert!(!mods.join("archive").exists(), "default archive untouched");

    let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
    let rep = set_active(&conn, &layout, &on);
    assert_eq!(rep.changed, 1);
    assert!(
        mods.join("FS25_Foo.zip").exists(),
        "projected into the mods folder"
    );

    let rep = flatten(&conn, &layout);
    assert!(rep.errors.is_empty(), "no errors: {:?}", rep.errors);
    assert_eq!(rep.changed, 1);
    let restored = mods.join("FS25_Foo.zip");
    let meta = std::fs::symlink_metadata(&restored).unwrap();
    assert!(
        meta.file_type().is_file() && !meta.file_type().is_symlink(),
        "restored as a regular file"
    );
    assert_eq!(std::fs::read(&restored).unwrap(), b"PK\x03\x04 foo");
    assert!(
        !arch.join("Vehicles").exists(),
        "the emptied category folder is removed"
    );
    assert!(
        arch.exists(),
        "a user-chosen archive folder is never removed"
    );
    assert!(
        db::load_organized(&conn, &crate::modsroot::canonical_key(&mods)).is_empty(),
        "the manifest is cleared"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn flatten_removes_an_emptied_default_archive() {
    let base = std::env::temp_dir().join(format!("silo_defarch_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("mods");
    std::fs::create_dir_all(&mods).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    std::fs::write(mods.join("FS25_Foo.zip"), b"PK\x03\x04 foo").unwrap();
    let input = ModInput {
        tech_name: "FS25_Foo".into(),
        file_name: "FS25_Foo.zip".into(),
        kind: "zip".into(),
        category: "Vehicles".into(),
        subcategory: None,
    };

    let rep = apply_organize(&conn, &layout, &mods, std::slice::from_ref(&input));
    assert_eq!(rep.changed, 1);
    assert!(mods.join("archive/Vehicles/FS25_Foo.zip").exists());

    let rep = flatten(&conn, &layout);
    assert!(rep.errors.is_empty(), "no errors: {:?}", rep.errors);
    assert!(mods.join("FS25_Foo.zip").exists());
    assert!(
        !mods.join("archive").exists(),
        "Silo's own emptied archive folder is removed"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn organize_never_overwrites_a_file_already_in_the_archive() {
    let base = std::env::temp_dir().join(format!("silo_noclobber_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("mods");
    let arch = base.join("hdd");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::create_dir_all(arch.join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: arch.clone(),
    };
    std::fs::write(mods.join("FS25_Foo.zip"), b"PK\x03\x04 the loose build").unwrap();
    std::fs::write(
        arch.join("Vehicles/FS25_Foo.zip"),
        b"PK\x03\x04 a different build already in the archive",
    )
    .unwrap();
    let input = ModInput {
        tech_name: "FS25_Foo".into(),
        file_name: "FS25_Foo.zip".into(),
        kind: "zip".into(),
        category: "Vehicles".into(),
        subcategory: None,
    };

    let rep = apply_organize(&conn, &layout, &mods, std::slice::from_ref(&input));

    assert_eq!(rep.changed, 0);
    assert_eq!(rep.skipped, 1);
    assert!(
        rep.errors.iter().any(|e| e.contains("already exists")),
        "the collision is reported, got: {:?}",
        rep.errors
    );
    assert_eq!(
        std::fs::read(mods.join("FS25_Foo.zip")).unwrap(),
        b"PK\x03\x04 the loose build"
    );
    assert_eq!(
        std::fs::read(arch.join("Vehicles/FS25_Foo.zip")).unwrap(),
        b"PK\x03\x04 a different build already in the archive"
    );
    assert!(
        db::load_organized(&conn, &crate::modsroot::canonical_key(&mods)).is_empty(),
        "no manifest row for a mod that wasn't moved"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn organize_never_moves_a_folder_into_itself() {
    let base = std::env::temp_dir().join(format!("silo_selfnest_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("mods");
    let arch = base.join("hdd");
    std::fs::create_dir_all(&mods).unwrap();
    // A loose folder mod at the archive's top level named exactly like its category.
    std::fs::create_dir_all(arch.join("Tools")).unwrap();
    std::fs::write(arch.join("Tools/modDesc.xml"), b"<modDesc/>").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: arch.clone(),
    };
    let input = ModInput {
        tech_name: "Tools".into(),
        file_name: "Tools".into(),
        kind: "dir".into(),
        category: "Tools".into(),
        subcategory: None,
    };

    let rep = apply_organize(&conn, &layout, &arch, std::slice::from_ref(&input));

    assert_eq!(rep.changed, 0);
    assert_eq!(rep.skipped, 1, "counted as skipped");
    assert_eq!(rep.errors.len(), 1, "reported: {:?}", rep.errors);
    assert!(arch.join("Tools/modDesc.xml").exists(), "left in place");
    assert!(!arch.join("Tools/Tools").exists(), "not nested into itself");
    assert!(db::load_organized(&conn, &crate::modsroot::canonical_key(&mods)).is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn flatten_leaves_a_user_archive_folders_own_content_alone() {
    let base = std::env::temp_dir().join(format!("silo_ownedonly_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("mods");
    let arch = base.join("hdd");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::create_dir_all(arch.join("ToSort")).unwrap();
    std::fs::write(arch.join("notes.txt"), b"my notes").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: arch.clone(),
    };
    std::fs::write(mods.join("FS25_Foo.zip"), b"PK\x03\x04 foo").unwrap();
    let input = ModInput {
        tech_name: "FS25_Foo".into(),
        file_name: "FS25_Foo.zip".into(),
        kind: "zip".into(),
        category: "Vehicles".into(),
        subcategory: None,
    };

    let rep = apply_organize(&conn, &layout, &mods, std::slice::from_ref(&input));
    assert_eq!(rep.changed, 1);
    let rep = flatten(&conn, &layout);

    assert!(
        arch.join("ToSort").is_dir(),
        "the user's empty folder is kept"
    );
    assert!(arch.join("notes.txt").is_file(), "the user's file is kept");
    assert!(
        !arch.join("Vehicles").exists(),
        "the emptied category folder is removed"
    );
    assert!(arch.exists(), "the archive root is kept");
    assert!(
        !rep.errors.iter().any(|e| e.contains("unrecognized")),
        "the user's own files aren't reported: {:?}",
        rep.errors
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn adopting_files_loose_archive_mods_into_categories() {
    // Distinct temp name: `silo_adopt_{pid}` is already used by the adopt_foreign test,
    // and tests run in parallel in one process.
    let base = std::env::temp_dir().join(format!("silo_adoptloose_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("mods");
    let arch = base.join("hdd");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::create_dir_all(&arch).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: arch.clone(),
    };
    std::fs::write(arch.join("FS25_Loose.zip"), b"PK\x03\x04 loose").unwrap();
    let input = ModInput {
        tech_name: "FS25_Loose".into(),
        file_name: "FS25_Loose.zip".into(),
        kind: "zip".into(),
        category: "Tools".into(),
        subcategory: None,
    };

    let plan = plan_organize(&arch, std::slice::from_ref(&input));
    assert_eq!(plan.len(), 1);
    let rep = apply_organize(&conn, &layout, &arch, std::slice::from_ref(&input));
    assert_eq!(rep.changed, 1);
    assert!(arch.join("Tools/FS25_Loose.zip").exists());
    assert!(!arch.join("FS25_Loose.zip").exists());
    assert!(
        !mods.join("FS25_Loose.zip").exists(),
        "adopted mods arrive parked"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Part of what park and flatten say when they keep a copy whose archived copy can't be shown
/// to hold the same bytes (#399).
const KEPT_UNPROVEN: &str =
    "stays in your mods folder: Silo couldn't confirm the copy in your library archive matches it";

/// What flatten says about a file in Silo's own archive that no mod it tracks owns.
const UNRECOGNIZED: &str = "Silo kept an unrecognized file in your library archive:";

/// An active zip `FS25_Foo` archived in `Vehicles` whose mods-folder entry is a byte copy with
/// its provenance recorded, as activation leaves it where hardlinks fail. Returns (src, flat).
fn zip_copy_fixture(base: &Path, conn: &Connection) -> (PathBuf, PathBuf) {
    let key = crate::modsroot::canonical_key(base);
    let cat = base.join(ARCHIVE).join("Vehicles");
    std::fs::create_dir_all(&cat).unwrap();
    let src = cat.join("FS25_Foo.zip");
    std::fs::write(&src, b"PK\x03\x04 the archived build").unwrap();
    organize_row(conn, base, "FS25_Foo", "Vehicles", true);
    let flat = base.join("FS25_Foo.zip");
    std::fs::copy(&src, &flat).unwrap();
    let (size, mtime) = file_stats(&flat).unwrap();
    db::record_projection(conn, &key, "FS25_Foo.zip", size, mtime);
    (src, flat)
}

/// Park a zip copy whose archived copy no longer holds the same bytes (cut short, rewritten at
/// the same size): the mods-folder copy may be the only good one, so it's kept and the mod
/// stays on. Flatten keeps it too, and never moves the archived copy over it.
#[test]
fn a_zip_copy_is_kept_when_the_archived_copy_doesnt_match() {
    let cases: [(&str, &[u8]); 2] = [
        ("cut", b"PK\x03\x04 the arch"),
        ("rewritten", b"PK\x03\x04 the archived bui1d"),
    ];
    for (tag, damaged) in cases {
        for flattening in [false, true] {
            let base = temp_dir_for(&format!("keep_zip_{tag}_{flattening}"));
            let conn = db::open(&base.join("silo.db")).unwrap();
            let key = crate::modsroot::canonical_key(&base);
            let (src, flat) = zip_copy_fixture(&base, &conn);
            std::fs::write(&src, damaged).unwrap();
            let layout = Layout::default_for(&base);
            let rep = if flattening {
                flatten(&conn, &layout)
            } else {
                set_active(&conn, &layout, &HashSet::new())
            };
            assert_eq!(rep.changed, 0, "{tag}: {:?}", rep.errors);
            // Flatten doesn't call the kept mod's archived zip unrecognized (#410).
            assert_eq!(rep.errors.len(), 1, "{tag}: {:?}", rep.errors);
            assert!(
                rep.errors[0].starts_with(&format!("FS25_Foo: FS25_Foo.zip {KEPT_UNPROVEN}")),
                "{tag}: {:?}",
                rep.errors
            );
            assert_eq!(
                std::fs::read(&flat).unwrap(),
                b"PK\x03\x04 the archived build",
                "{tag}: the mods-folder copy is kept"
            );
            assert_eq!(std::fs::read(&src).unwrap(), damaged, "{tag}");
            let rows = db::load_organized(&conn, &key);
            assert_eq!(rows.len(), 1, "{tag}");
            assert!(rows[0].active, "{tag}: the mod is still on");
            drop(conn);
            let _ = std::fs::remove_dir_all(&base);
        }
    }
}

/// Flatten keeps a zip copy its archived copy doesn't match (#399). Files in the archive no mod
/// Silo tracks owns are still listed as unrecognized, next to it in the same folder too, but the
/// kept mod's own archived zip isn't (#410).
#[test]
fn flatten_lists_only_archive_files_no_kept_mod_owns() {
    let base = temp_dir_for("flatten_kept_mix");
    let conn = db::open(&base.join("silo.db")).unwrap();
    let (src, flat) = zip_copy_fixture(&base, &conn);
    std::fs::write(&src, b"PK\x03\x04 the arch").unwrap(); // cut short: the copy is kept
    let cat = src.parent().unwrap();
    std::fs::write(cat.join("FS25_Foo.zip.bak"), b"mine").unwrap();
    std::fs::create_dir_all(base.join(ARCHIVE).join("Tools")).unwrap();
    std::fs::write(base.join(ARCHIVE).join("Tools").join("orphan.zip"), b"PK").unwrap();

    let rep = flatten(&conn, &Layout::default_for(&base));

    assert_eq!(rep.changed, 0, "{:?}", rep.errors);
    let (kept, unknown): (Vec<_>, Vec<_>) =
        rep.errors.iter().partition(|e| e.starts_with("FS25_Foo: "));
    assert_eq!(kept.len(), 1, "{:?}", rep.errors);
    assert!(kept[0].contains(KEPT_UNPROVEN), "{:?}", rep.errors);
    let mut unknown: Vec<&str> = unknown.iter().map(|e| e.as_str()).collect();
    unknown.sort_unstable();
    assert_eq!(
        unknown,
        [
            format!("{UNRECOGNIZED} FS25_Foo.zip.bak. Nothing was deleted."),
            format!("{UNRECOGNIZED} orphan.zip. Nothing was deleted."),
        ],
        "{:?}",
        rep.errors
    );
    assert!(flat.is_file() && src.is_file());
    assert!(cat.join("FS25_Foo.zip.bak").is_file());
    drop(conn);
    let _ = std::fs::remove_dir_all(&base);
}

/// A zip mod's archive entry is the zip alone: when a folder took its place (the wrong kind, so
/// the copy is kept), files inside that folder are still listed as unrecognized (#410).
#[test]
fn flatten_lists_files_in_a_folder_where_a_kept_zip_should_be() {
    let base = temp_dir_for("flatten_kept_zip_dir");
    let conn = db::open(&base.join("silo.db")).unwrap();
    let (src, flat) = zip_copy_fixture(&base, &conn);
    std::fs::remove_file(&src).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("unrelated.txt"), b"mine").unwrap();

    let rep = flatten(&conn, &Layout::default_for(&base));

    assert_eq!(rep.changed, 0, "{:?}", rep.errors);
    assert_eq!(rep.errors.len(), 2, "{:?}", rep.errors);
    assert!(rep.errors[0].contains(KEPT_UNPROVEN), "{:?}", rep.errors);
    assert_eq!(
        rep.errors[1],
        format!("{UNRECOGNIZED} unrelated.txt. Nothing was deleted.")
    );
    assert!(flat.is_file() && src.join("unrelated.txt").is_file());
    drop(conn);
    let _ = std::fs::remove_dir_all(&base);
}

/// A dead link at a mod's archive path leaves no archived copy, so flatten keeps the mods-folder
/// zip and says so; the dead link is that mod's, not unrecognized, while an orphan beside it
/// still is (#410).
#[test]
fn flatten_doesnt_list_a_dead_link_at_a_kept_mods_archive_path() {
    let base = temp_dir_for("flatten_kept_dead_link");
    let conn = db::open(&base.join("silo.db")).unwrap();
    let (src, flat) = zip_copy_fixture(&base, &conn);
    std::fs::remove_file(&src).unwrap();
    if symlink_file(&base.join("gone.zip"), &src).is_err() {
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
        return; // no file symlinks here (Windows without Developer Mode)
    }
    let orphan = src.with_file_name("orphan.zip");
    std::fs::write(&orphan, b"PK").unwrap();

    let rep = flatten(&conn, &Layout::default_for(&base));

    assert_eq!(
        rep.errors,
        [
            "FS25_Foo: there's no copy in your library archive, so Silo kept the file in your mods folder.".to_string(),
            format!("{UNRECOGNIZED} orphan.zip. Nothing was deleted."),
        ]
    );
    assert!(flat.is_file() && orphan.is_file());
    drop(conn);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_dir_all(&base);
}

/// Whether names in `dir` ignore case (NTFS, APFS by default), probed with a file of its own.
fn folds_case(dir: &Path) -> bool {
    let probe = dir.join("CaseProbe.tmp");
    std::fs::write(&probe, b"").unwrap();
    let folds = dir.join("caseprobe.tmp").exists();
    std::fs::remove_file(&probe).unwrap();
    folds
}

/// A kept mod's zip whose folder and name were renamed to other casing: where the filesystem
/// still finds it under the recorded name (NTFS, APFS), it's that mod's and isn't unrecognized
/// (#410). Where it doesn't (case-sensitive), there is no archived copy and the renamed file is
/// listed as unrecognized.
#[test]
fn flatten_matches_a_kept_mods_zip_under_other_casing_as_the_filesystem_does() {
    let base = temp_dir_for("flatten_kept_casing");
    let conn = db::open(&base.join("silo.db")).unwrap();
    let (src, flat) = zip_copy_fixture(&base, &conn);
    let cat = src.parent().unwrap();
    let lower_cat = cat.with_file_name("vehicles");
    std::fs::rename(&src, cat.join("fs25_foo.zip")).unwrap();
    std::fs::rename(cat, &lower_cat).unwrap();
    std::fs::write(lower_cat.join("fs25_foo.zip"), b"PK\x03\x04 the arch").unwrap();
    let folds = folds_case(&lower_cat);

    let rep = flatten(&conn, &Layout::default_for(&base));

    assert_eq!(rep.changed, usize::from(!folds), "{:?}", rep.errors);
    if folds {
        assert_eq!(rep.errors.len(), 1, "{:?}", rep.errors);
        assert!(rep.errors[0].contains(KEPT_UNPROVEN), "{:?}", rep.errors);
    } else {
        assert_eq!(
            rep.errors,
            [
                "FS25_Foo: there's no copy in your library archive, so Silo kept the file in your mods folder.".to_string(),
                format!("{UNRECOGNIZED} fs25_foo.zip. Nothing was deleted."),
            ]
        );
    }
    assert!(flat.is_file() && lower_cat.join("fs25_foo.zip").is_file());
    drop(conn);
    let _ = std::fs::remove_dir_all(&base);
}

/// In a case-sensitive folder, a second entry whose name differs from a kept mod's zip only in
/// case is a different entry, even as a hardlink to the same file: it's still listed as
/// unrecognized (#410). (Skipped where names ignore case: the two can't both exist.)
#[test]
fn flatten_lists_an_entry_differing_from_a_kept_mods_zip_only_in_case() {
    for hardlink in [false, true] {
        let base = temp_dir_for(&format!("flatten_kept_case_twin_{hardlink}"));
        let conn = db::open(&base.join("silo.db")).unwrap();
        let (src, flat) = zip_copy_fixture(&base, &conn);
        let cat = src.parent().unwrap();
        if folds_case(cat) {
            drop(conn);
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        std::fs::write(&src, b"PK\x03\x04 the arch").unwrap(); // cut short: the copy is kept
        let twin = cat.join("fs25_foo.zip");
        if hardlink {
            std::fs::hard_link(&src, &twin).unwrap();
        } else {
            std::fs::write(&twin, b"PK theirs").unwrap();
        }

        let rep = flatten(&conn, &Layout::default_for(&base));

        assert_eq!(rep.changed, 0, "{hardlink}: {:?}", rep.errors);
        assert_eq!(rep.errors.len(), 2, "{hardlink}: {:?}", rep.errors);
        assert!(rep.errors[0].contains(KEPT_UNPROVEN), "{:?}", rep.errors);
        assert_eq!(
            rep.errors[1],
            format!("{UNRECOGNIZED} fs25_foo.zip. Nothing was deleted.")
        );
        assert!(flat.is_file() && twin.is_file());
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// A kept mod's zip renamed to other casing, beside a hardlink of it whose name differs only in
/// how its accent is encoded: both names could be the entry, so Silo can't tell which and lists
/// both, never hiding the other one (#410). (Skipped where the twin can't be made: APFS treats
/// the two names as one.)
#[test]
fn flatten_hides_neither_name_when_two_could_be_a_kept_mods_zip() {
    for twin_first in [false, true] {
        let base = temp_dir_for(&format!("flatten_kept_accent_twin_{twin_first}"));
        let conn = db::open(&base.join("silo.db")).unwrap();
        let key = crate::modsroot::canonical_key(&base);
        let cat = base.join(ARCHIVE).join("Vehicles");
        std::fs::create_dir_all(&cat).unwrap();
        let tech = "FS25_Caf\u{e9}";
        let file = format!("{tech}.zip");
        db::upsert_organized(
            &conn,
            &key,
            &OrganizedRow {
                tech_name: tech.into(),
                file_name: file.clone(),
                kind: "zip".into(),
                category: "Vehicles".into(),
                subcategory: None,
                active: true,
            },
        )
        .unwrap();
        let flat = base.join(&file);
        std::fs::write(&flat, b"PK\x03\x04 the archived build").unwrap();
        let (size, mtime) = file_stats(&flat).unwrap();
        db::record_projection(&conn, &key, &file, size, mtime);
        // The archived zip, cut short, under other casing; and its hardlink, the accent decomposed.
        let (renamed, twin) = ("fs25_caf\u{e9}.zip", "fs25_cafe\u{301}.zip");
        let (first, second) = if twin_first {
            (twin, renamed)
        } else {
            (renamed, twin)
        };
        std::fs::write(cat.join(first), b"PK\x03\x04 the arch").unwrap();
        if std::fs::hard_link(cat.join(first), cat.join(second)).is_err()
            || std::fs::read_dir(&cat).unwrap().count() != 2
        {
            drop(conn);
            let _ = std::fs::remove_dir_all(&base);
            return;
        }

        let rep = flatten(&conn, &Layout::default_for(&base));

        for name in [renamed, twin] {
            assert!(
                rep.errors
                    .contains(&format!("{UNRECOGNIZED} {name}. Nothing was deleted.")),
                "{twin_first} {name:?}: {:?}",
                rep.errors
            );
        }
        assert!(flat.is_file());
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// What counts as inside a mod's archive entry: the entry itself; a folder mod's files below it;
/// nothing below a zip mod's entry; never a name that only starts the same, or another spelling.
#[test]
fn in_archive_entry_matches_the_entry_and_a_folder_mods_files() {
    let a = Path::new("archive");
    let zip = a.join("Vehicles").join("FS25_Foo.zip");
    let dir = a.join("Misc").join("FS25_Dir");
    assert!(in_archive_entry(&zip, &zip, false));
    assert!(!in_archive_entry(&zip.join("x.txt"), &zip, false));
    assert!(in_archive_entry(&dir, &dir, true));
    assert!(in_archive_entry(&dir.join("sub").join("a.dds"), &dir, true));
    assert!(!in_archive_entry(
        &a.join("Vehicles").join("FS25_Foo.zip.bak"),
        &zip,
        false
    ));
    assert!(!in_archive_entry(
        &a.join("Misc").join("FS25_Dir.zip"),
        &dir,
        true
    ));
    assert!(!in_archive_entry(&a.join("Misc"), &dir, true));
    assert!(!in_archive_entry(
        &a.join("Tools").join("FS25_Foo.zip"),
        &zip,
        false
    ));
    let other_case = a.join("vehicles").join("fs25_foo.zip");
    assert!(!in_archive_entry(&other_case, &zip, false));
}

/// An entry's spelling on disk: itself when listed; where the filesystem finds it under another
/// spelling, the listed name (and never a hardlink under another name); `None` when it isn't
/// there or isn't under the archive.
#[test]
fn spelled_on_disk_finds_the_listed_name_of_the_same_entry() {
    let base = temp_dir_for("spelled_on_disk");
    let cat = base.join("Vehicles");
    std::fs::create_dir_all(&cat).unwrap();
    let zip = cat.join("fs25_foo.zip");
    std::fs::write(&zip, b"PK").unwrap();
    std::fs::hard_link(&zip, cat.join("Copy.zip")).unwrap();
    assert_eq!(spelled_on_disk(&base, &zip), Some(zip.clone()));
    let asked = base.join("VEHICLES").join("FS25_Foo.zip");
    let expect = folds_case(&cat).then(|| zip.clone());
    assert_eq!(spelled_on_disk(&base, &asked), expect);
    assert_eq!(spelled_on_disk(&base, &cat.join("FS25_Bar.zip")), None);
    // Two listed names that could both be it (a hardlink whose name differs only in how its
    // accent is encoded): neither is chosen.
    let cafe = cat.join("caf\u{e9}.zip");
    std::fs::write(&cafe, b"PK").unwrap();
    if std::fs::hard_link(&cafe, cat.join("cafe\u{301}.zip")).is_ok() {
        assert_eq!(spelled_on_disk(&base, &cat.join("CAF\u{c9}.zip")), None);
    }
    assert_eq!(spelled_on_disk(&cat, &base.join("x.zip")), None);
    let _ = std::fs::remove_dir_all(&base);
}

/// A file in Silo's own archive that no organized mod owns is still listed as unrecognized,
/// and kept.
#[test]
fn flatten_lists_a_file_no_mod_owns_as_unrecognized() {
    let base = temp_dir_for("flatten_orphan");
    let conn = db::open(&base.join("silo.db")).unwrap();
    let orphan = base.join(ARCHIVE).join("Vehicles").join("orphan.zip");
    std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    std::fs::write(&orphan, b"PK").unwrap();

    let rep = flatten(&conn, &Layout::default_for(&base));

    assert_eq!(
        rep.errors,
        [format!("{UNRECOGNIZED} orphan.zip. Nothing was deleted.")]
    );
    assert!(orphan.is_file());
    drop(conn);
    let _ = std::fs::remove_dir_all(&base);
}

/// No archived copy, or a folder where the archived zip should be (the wrong kind): the zip
/// copy is kept.
#[test]
fn a_zip_copy_is_kept_when_there_is_no_archived_zip() {
    let base = temp_dir_for("keep_zip_missing");
    let conn = db::open(&base.join("silo.db")).unwrap();
    let (src, flat) = zip_copy_fixture(&base, &conn);
    std::fs::remove_file(&src).unwrap();
    let err = clear_projection(&flat, &src, &base).unwrap_err();
    assert!(err.to_string().contains(KEPT_UNPROVEN), "{err}");
    assert!(flat.is_file());

    std::fs::create_dir_all(&src).unwrap();
    let err = clear_projection(&flat, &src, &base).unwrap_err();
    assert!(err.to_string().contains(KEPT_UNPROVEN), "{err}");
    assert_eq!(
        std::fs::read(&flat).unwrap(),
        b"PK\x03\x04 the archived build"
    );
    drop(conn);
    let _ = std::fs::remove_dir_all(&base);
}

/// An archived zip that is a link to an identical file elsewhere isn't a copy: the mods-folder
/// copy is kept.
#[test]
fn a_zip_copy_is_kept_when_the_archived_zip_is_a_link() {
    let base = temp_dir_for("keep_zip_link");
    let conn = db::open(&base.join("silo.db")).unwrap();
    let (src, flat) = zip_copy_fixture(&base, &conn);
    let elsewhere = base.join("elsewhere.zip");
    std::fs::rename(&src, &elsewhere).unwrap();
    if symlink_file(&elsewhere, &src).is_err() {
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
        return; // no file symlinks here (Windows without Developer Mode)
    }
    let rep = set_active(&conn, &Layout::default_for(&base), &HashSet::new());
    assert_eq!(rep.changed, 0, "{:?}", rep.errors);
    assert!(
        rep.errors.len() == 1 && rep.errors[0].contains(KEPT_UNPROVEN),
        "{:?}",
        rep.errors
    );
    assert_eq!(
        std::fs::read(&flat).unwrap(),
        b"PK\x03\x04 the archived build"
    );
    drop(conn);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_dir_all(&base);
}

/// An intact archived copy still lets the projection go: a byte copy (proven by its bytes) and
/// a hardlink (one file under two names).
#[test]
fn a_zip_projection_goes_while_the_archive_holds_its_bytes() {
    for hardlink in [false, true] {
        let base = temp_dir_for(&format!("drop_zip_{hardlink}"));
        let conn = db::open(&base.join("silo.db")).unwrap();
        let key = crate::modsroot::canonical_key(&base);
        let (src, flat) = zip_copy_fixture(&base, &conn);
        if hardlink {
            std::fs::remove_file(&flat).unwrap();
            std::fs::hard_link(&src, &flat).unwrap();
            db::forget_projection(&conn, &key, "FS25_Foo.zip");
        }
        let rep = set_active(&conn, &Layout::default_for(&base), &HashSet::new());
        assert_eq!(rep.changed, 1, "{hardlink}: {:?}", rep.errors);
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert!(flat.symlink_metadata().is_err(), "{hardlink}: removed");
        assert_eq!(
            std::fs::read(&src).unwrap(),
            b"PK\x03\x04 the archived build"
        );
        assert!(!db::load_organized(&conn, &key)[0].active);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }
}

// #427: a mod deleted after the Organize preview is reported, not silently skipped.
#[test]
fn filing_a_mod_that_is_gone_says_so() {
    let base = std::env::temp_dir().join(format!("silo_file_gone_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&base);
    let gone = ModInput {
        tech_name: "FS25_Gone".into(),
        file_name: "FS25_Gone.zip".into(),
        kind: "zip".into(),
        category: "Vehicles".into(),
        subcategory: None,
    };
    let rep = apply_organize(&conn, &layout, &base, &[gone]);
    assert_eq!((rep.changed, rep.skipped), (0, 1));
    assert_eq!(
        rep.errors,
        vec!["FS25_Gone: FS25_Gone.zip is no longer there, so Silo didn't file it.".to_string()]
    );
    assert!(db::load_organized(&conn, &crate::modsroot::canonical_key(&base)).is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

// #427: a source Silo can't inspect is reported with its cause, never as "no longer there".
#[cfg(unix)]
#[test]
fn filing_a_mod_it_cant_inspect_says_why() {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!("silo_file_noperm_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let locked = base.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("FS25_Hidden.zip"), b"PK").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // Running as root: permissions don't apply, nothing to test.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
        return;
    }
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&base);
    let m = ModInput {
        tech_name: "FS25_Hidden".into(),
        file_name: "FS25_Hidden.zip".into(),
        kind: "zip".into(),
        category: "Vehicles".into(),
        subcategory: None,
    };
    let rep = apply_organize(&conn, &layout, &locked, &[m]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(rep.errors.len(), 1);
    assert!(
        rep.errors[0].starts_with("FS25_Hidden: can't check FS25_Hidden.zip ("),
        "{:?}",
        rep.errors
    );
    assert!(locked.join("FS25_Hidden.zip").exists(), "untouched");
    let _ = std::fs::remove_dir_all(&base);
}

// #427: a file at an untouched mod's name that Silo can't read is an error, not a standing
// "different build": Silo couldn't tell what it is.
#[cfg(unix)]
#[test]
fn an_unreadable_file_at_an_untouched_mods_name_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::temp_dir().join(format!("silo_standing_noread_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&base);
    std::fs::write(
        base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip"),
        b"PK same size",
    )
    .unwrap();
    organize_row(&conn, &base, "FS25_Foo", "Vehicles", true);
    let flat = base.join("FS25_Foo.zip");
    std::fs::write(&flat, b"PK same size").unwrap();
    std::fs::set_permissions(&flat, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&flat).is_ok() {
        // Running as root: permissions don't apply, nothing to test.
        let _ = std::fs::remove_dir_all(&base);
        return;
    }
    let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
    let other: HashSet<String> = ["FS25_Bar".to_string()].into_iter().collect();
    let rep = set_active_touched(&conn, &layout, &on, Some(&other));
    std::fs::set_permissions(&flat, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(rep.standing.is_empty(), "{:?}", rep.standing);
    assert_eq!(rep.errors.len(), 1);
    assert!(
        rep.errors[0].starts_with("FS25_Foo: can't check FS25_Foo.zip in your mods folder ("),
        "{:?}",
        rep.errors
    );
    assert!(flat.exists(), "left in place");
    let _ = std::fs::remove_dir_all(&base);
}

// #427: a file at a wanted mod's name that isn't Silo's copy is the change's problem only when
// the change touched that mod; otherwise it's a standing one (Diagnose lists it).
#[test]
fn a_foreign_file_at_an_untouched_mods_name_is_standing_not_an_error() {
    let base = std::env::temp_dir().join(format!("silo_standing_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&base);
    std::fs::write(
        base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip"),
        b"PK\x03\x04 archived",
    )
    .unwrap();
    organize_row(&conn, &base, "FS25_Foo", "Vehicles", true);
    let flat = base.join("FS25_Foo.zip");
    std::fs::write(&flat, b"the user's own build").unwrap();
    let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();

    let other: HashSet<String> = ["FS25_Bar".to_string()].into_iter().collect();
    let rep = set_active_touched(&conn, &layout, &on, Some(&other));
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.standing.len(), 1);
    assert!(
        rep.standing[0].starts_with("FS25_Foo: FS25_Foo.zip in your mods folder isn't Silo's copy")
    );

    let rep = set_active_touched(&conn, &layout, &on, Some(&on));
    assert_eq!(
        rep.errors.len(),
        1,
        "the change turned this mod on: its problem"
    );
    assert!(rep.standing.is_empty());

    let rep = set_active(&conn, &layout, &on);
    assert_eq!(
        rep.errors.len(),
        1,
        "without a touched set, reported as before"
    );

    assert_eq!(
        std::fs::read(&flat).unwrap(),
        b"the user's own build",
        "never replaced"
    );
    let _ = std::fs::remove_dir_all(&base);
}

// #423: Undo organizing never puts a zip back beside another copy of its tech name (an unzipped
// folder of the same mod): the game would load one of two. The zip stays in the library, parked,
// and the result says why; without that copy it's restored as usual.
#[test]
fn undo_organizing_leaves_a_zip_whose_name_another_copy_holds() {
    let base = temp_dir_for("undo_twin");
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let key = crate::modsroot::canonical_key(&base);
    let layout = Layout::default_for(&base);
    let src = base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip");
    std::fs::write(&src, b"PK\x03\x04 Foo").unwrap();
    organize_row(&conn, &base, "FS25_Foo", "Vehicles", false);
    // Linked while it was the only copy, then the folder came.
    let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
    assert!(set_active(&conn, &layout, &on).errors.is_empty());
    assert!(base.join("FS25_Foo.zip").exists());
    let folder = base.join("FS25_Foo");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("modDesc.xml"), "<modDesc/>").unwrap();

    let rep = flatten(&conn, &layout);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.notices.len(), 1, "{:?}", rep.notices);
    assert!(rep.notices[0].starts_with(
        "FS25_Foo: another copy of this mod, FS25_Foo, is in your mods folder, and the game loads only one, so Silo left FS25_Foo.zip in your library."
    ));
    assert!(src.is_file(), "still in the library");
    assert!(
        base.join("FS25_Foo.zip").symlink_metadata().is_err(),
        "its link is gone, nothing put back"
    );
    let rows = db::load_organized(&conn, &key);
    assert_eq!(rows.len(), 1, "still filed");
    assert!(!rows[0].active, "parked");
    assert!(
        folder.join("modDesc.xml").is_file(),
        "the folder is left as it is"
    );

    // Without the other copy, Undo organizing puts it back as usual.
    std::fs::remove_dir_all(&folder).unwrap();
    let rep = flatten(&conn, &layout);
    assert!(
        rep.errors.is_empty() && rep.notices.is_empty(),
        "{:?} {:?}",
        rep.errors,
        rep.notices
    );
    assert_eq!(
        std::fs::read(base.join("FS25_Foo.zip")).unwrap(),
        b"PK\x03\x04 Foo"
    );
    assert!(db::load_organized(&conn, &key).is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

// #423: the other copy is found whatever its case, and the mod's own projection, cleared during
// the pass, never counts as another copy even when the copies were read before it was cleared.
#[test]
fn undo_organizing_matches_copies_case_insensitively_and_not_its_own_link() {
    let base = temp_dir_for("undo_twin_case");
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&base);
    for t in ["FS25_Aaa", "FS25_Bar", "FS25_Foo"] {
        std::fs::write(
            base.join(ARCHIVE).join(format!("Vehicles/{t}.zip")),
            format!("PK {t}"),
        )
        .unwrap();
        organize_row(&conn, &base, t, "Vehicles", false);
    }
    // FS25_Aaa comes first and reads the copies while FS25_Bar's link is still there.
    let on: HashSet<String> = ["FS25_Bar".to_string(), "FS25_Foo".to_string()]
        .into_iter()
        .collect();
    assert!(set_active(&conn, &layout, &on).errors.is_empty());
    let folder = base.join("fs25_foo");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("modDesc.xml"), "<modDesc/>").unwrap();

    let rep = flatten(&conn, &layout);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.notices.len(), 1, "{:?}", rep.notices);
    assert!(
        rep.notices[0].starts_with("FS25_Foo: another copy of this mod, fs25_foo,"),
        "{:?}",
        rep.notices
    );
    assert_eq!(
        std::fs::read(base.join("FS25_Aaa.zip")).unwrap(),
        b"PK FS25_Aaa"
    );
    assert_eq!(
        std::fs::read(base.join("FS25_Bar.zip")).unwrap(),
        b"PK FS25_Bar",
        "its own link isn't another copy"
    );
    assert!(base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

// #423: a folder mod left in the default archive keeps its empty folders through cleanup.
#[test]
fn undo_organizing_keeps_a_left_folder_mod_whole() {
    let base = temp_dir_for("undo_twin_dir");
    let arch = base.join(ARCHIVE).join("Maps");
    std::fs::create_dir_all(arch.join("FS25_Map").join("data").join("empty")).unwrap();
    std::fs::write(arch.join("FS25_Map").join("modDesc.xml"), "<modDesc/>").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&base);
    db::upsert_organized(
        &conn,
        &crate::modsroot::canonical_key(&base),
        &OrganizedRow {
            tech_name: "FS25_Map".into(),
            file_name: "FS25_Map".into(),
            kind: "dir".into(),
            category: "Maps".into(),
            subcategory: None,
            active: false,
        },
    )
    .unwrap();
    std::fs::write(base.join("FS25_Map.zip"), b"PK the zip copy").unwrap();

    let rep = flatten(&conn, &layout);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.notices.len(), 1, "{:?}", rep.notices);
    assert!(
        arch.join("FS25_Map").join("data").join("empty").is_dir(),
        "the mod's empty folder survives"
    );
    assert!(arch.join("FS25_Map").join("modDesc.xml").is_file());
    assert!(base.join("FS25_Map.zip").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

// #423: a folder mod left in a user-chosen archive keeps its empty folders too (that archive's
// cleanup only removes empty category folders).
#[test]
fn undo_organizing_keeps_a_left_folder_mod_whole_in_a_custom_archive() {
    let base = temp_dir_for("undo_twin_dir_custom");
    let mods = base.join("mods");
    let arch = base.join("elsewhere");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::create_dir_all(
        arch.join("Maps")
            .join("FS25_Map")
            .join("data")
            .join("empty"),
    )
    .unwrap();
    std::fs::write(
        arch.join("Maps").join("FS25_Map").join("modDesc.xml"),
        "<modDesc/>",
    )
    .unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: arch.clone(),
    };
    db::upsert_organized(
        &conn,
        &crate::modsroot::canonical_key(&mods),
        &OrganizedRow {
            tech_name: "FS25_Map".into(),
            file_name: "FS25_Map".into(),
            kind: "dir".into(),
            category: "Maps".into(),
            subcategory: None,
            active: false,
        },
    )
    .unwrap();
    std::fs::write(mods.join("FS25_Map.zip"), b"PK the zip copy").unwrap();
    let rep = flatten(&conn, &layout);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.notices.len(), 1, "{:?}", rep.notices);
    assert!(arch
        .join("Maps")
        .join("FS25_Map")
        .join("data")
        .join("empty")
        .is_dir());
    assert_eq!(
        db::load_organized(&conn, &crate::modsroot::canonical_key(&mods)).len(),
        1
    );
    let _ = std::fs::remove_dir_all(&base);
}

// #423, Windows and macOS: the mod's own link, spelled differently on disk (FS25_Foo.ZIP) after
// the copies were read, is still its own slot, not another copy.
#[cfg(not(target_os = "linux"))]
#[test]
fn undo_organizing_treats_its_own_link_in_another_case_as_its_own() {
    let base = temp_dir_for("undo_twin_spelling");
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&base);
    for t in ["FS25_Aaa", "FS25_Foo"] {
        std::fs::write(
            base.join(ARCHIVE).join(format!("Vehicles/{t}.zip")),
            format!("PK {t}"),
        )
        .unwrap();
        organize_row(&conn, &base, t, "Vehicles", false);
    }
    let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
    assert!(set_active(&conn, &layout, &on).errors.is_empty());
    std::fs::rename(base.join("FS25_Foo.zip"), base.join("FS25_Foo.ZIP")).unwrap();
    let rep = flatten(&conn, &layout);
    assert!(
        rep.errors.is_empty() && rep.notices.is_empty(),
        "{:?} {:?}",
        rep.errors,
        rep.notices
    );
    assert!(
        base.join(ARCHIVE)
            .join("Vehicles/FS25_Foo.zip")
            .symlink_metadata()
            .is_err(),
        "restored"
    );
    let _ = std::fs::remove_dir_all(&base);
}

// #423, Linux: a separate zip whose name differs only in case is another copy, not the mod's slot.
#[cfg(target_os = "linux")]
#[test]
fn undo_organizing_sees_a_case_variant_zip_as_another_copy_on_linux() {
    let base = temp_dir_for("undo_twin_variant");
    std::fs::create_dir_all(base.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&base);
    std::fs::write(
        base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip"),
        b"PK archived",
    )
    .unwrap();
    organize_row(&conn, &base, "FS25_Foo", "Vehicles", false);
    std::fs::write(base.join("fs25_foo.zip"), b"PK the user's other copy").unwrap();
    let rep = flatten(&conn, &layout);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.notices.len(), 1, "{:?}", rep.notices);
    assert!(
        base.join(ARCHIVE).join("Vehicles/FS25_Foo.zip").is_file(),
        "left in the library"
    );
    assert!(base.join("FS25_Foo.zip").symlink_metadata().is_err());
    let _ = std::fs::remove_dir_all(&base);
}
