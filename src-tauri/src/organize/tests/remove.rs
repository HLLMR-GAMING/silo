//! Tests for `organize/remove.rs`.

use super::*;

/// A stand-in Recycle Bin: moves the entry into `bin` (so a test can see what went where).
fn bin_at(bin: &Path) -> impl Fn(&Path) -> Result<(), String> + '_ {
    move |p: &Path| {
        std::fs::create_dir_all(bin).map_err(|e| e.to_string())?;
        std::fs::rename(p, bin.join(p.file_name().unwrap())).map_err(|e| e.to_string())
    }
}

/// An item as the confirmation would show `path` now (its kind, size and modified time).
fn item(tech: &str, path: &Path) -> RemoveItem {
    let (kind, size, mtime_ms) = match std::fs::metadata(path) {
        Ok(m) => (
            if m.is_dir() { "dir" } else { "zip" },
            m.len(),
            m.modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        ),
        Err(_) => ("zip", 0, 0),
    };
    RemoveItem {
        tech_name: tech.into(),
        path: path.to_string_lossy().into_owned(),
        kind: kind.into(),
        size,
        mtime_ms,
    }
}

/// A Recycle Bin that must never be reached.
fn no_bin(p: &Path) -> Result<(), String> {
    panic!("moved {} to the bin", p.display())
}

/// A mods folder with `FS25_Foo` filed under Vehicles; active means linked into the mods folder.
fn library_with_foo(tag: &str, active: bool) -> (PathBuf, Connection, Layout, PathBuf) {
    let base = temp_dir_for(tag);
    let mods = base.join("mods");
    std::fs::create_dir_all(mods.join(ARCHIVE).join("Vehicles")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let src = mods.join(ARCHIVE).join("Vehicles/FS25_Foo.zip");
    std::fs::write(&src, b"PK\x03\x04 Foo").unwrap();
    organize_row(&conn, &mods, "FS25_Foo", "Vehicles", false);
    if active {
        let on: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
        let rep = set_active(&conn, &layout, &on);
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert!(mods.join("FS25_Foo.zip").exists());
    }
    (base, conn, layout, src)
}

#[test]
fn removing_a_parked_library_mod_bins_its_archived_copy_and_drops_its_row() {
    let (base, conn, layout, src) = library_with_foo("rm_parked", false);
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[item("FS25_Foo", &src)],
        true,
        &bin_at(&bin),
    );
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(
        rep.removed,
        vec![Removed {
            tech_name: "FS25_Foo".into(),
            path: src.to_string_lossy().into_owned()
        }]
    );
    assert!(!src.exists());
    assert_eq!(
        std::fs::read(bin.join("FS25_Foo.zip")).unwrap(),
        b"PK\x03\x04 Foo",
        "in the bin, whole"
    );
    assert!(db::load_organized(&conn, &crate::modsroot::canonical_key(&layout.mods)).is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn removing_an_active_library_mod_parks_it_first() {
    let (base, conn, layout, src) = library_with_foo("rm_active", true);
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[item("FS25_Foo", &src)],
        true,
        &bin_at(&bin),
    );
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert!(
        !layout.mods.join("FS25_Foo.zip").exists(),
        "no link left in the mods folder"
    );
    assert!(bin.join("FS25_Foo.zip").exists());
    let key = crate::modsroot::canonical_key(&layout.mods);
    assert!(db::load_organized(&conn, &key).is_empty());
    assert!(db::projection_stats(&conn, &key, "FS25_Foo.zip").is_none());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_mod_that_cant_be_parked_stays() {
    // Active, with the user's own different build at its name: parking refuses, so nothing goes.
    let (base, conn, layout, src) = library_with_foo("rm_noprk", false);
    db::set_organized_active(
        &conn,
        &crate::modsroot::canonical_key(&layout.mods),
        "FS25_Foo",
        true,
    )
    .unwrap();
    let theirs = layout.mods.join("FS25_Foo.zip");
    std::fs::write(&theirs, b"the user's own build").unwrap();
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[item("FS25_Foo", &src)],
        true,
        &bin_at(&bin),
    );
    assert!(rep.removed.is_empty());
    assert_eq!(rep.errors.len(), 1, "{:?}", rep.errors);
    assert!(
        rep.errors[0].starts_with("FS25_Foo: ") && rep.errors[0].ends_with("It wasn't removed.")
    );
    assert!(src.exists(), "archived copy kept");
    assert_eq!(std::fs::read(&theirs).unwrap(), b"the user's own build");
    assert!(!bin.exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_library_mod_is_refused_while_the_archive_is_unreachable() {
    let (base, conn, layout, src) = library_with_foo("rm_noarch", false);
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[item("FS25_Foo", &src)],
        false,
        &bin_at(&bin),
    );
    assert!(rep.removed.is_empty());
    assert!(
        rep.errors[0].contains("isn't reachable"),
        "{:?}",
        rep.errors
    );
    assert!(src.exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_failed_move_to_the_bin_keeps_the_row() {
    let (base, conn, layout, src) = library_with_foo("rm_binfail", false);
    let fail = |_: &Path| Err("the Recycle Bin is full".to_string());
    let rep = remove_from_library(&conn, &layout, &[item("FS25_Foo", &src)], true, &fail);
    assert!(rep.removed.is_empty());
    assert!(
        rep.errors[0].contains("the Recycle Bin is full"),
        "{:?}",
        rep.errors
    );
    assert!(src.exists());
    assert_eq!(
        db::load_organized(&conn, &crate::modsroot::canonical_key(&layout.mods)).len(),
        1
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_loose_zip_and_folder_go_to_the_bin() {
    let base = temp_dir_for("rm_loose");
    let mods = base.join("mods");
    std::fs::create_dir_all(mods.join("FS25_Unzipped")).unwrap();
    std::fs::write(mods.join("FS25_Unzipped/modDesc.xml"), b"<modDesc/>").unwrap();
    std::fs::write(mods.join("FS25_Loose.zip"), b"PK loose").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[
            item("FS25_Loose", &mods.join("FS25_Loose.zip")),
            item("FS25_Unzipped", &mods.join("FS25_Unzipped")),
        ],
        true,
        &bin_at(&bin),
    );
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.removed.len(), 2);
    assert!(bin.join("FS25_Loose.zip").is_file());
    assert!(
        bin.join("FS25_Unzipped/modDesc.xml").is_file(),
        "the whole folder"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn paths_outside_the_mods_folder_and_the_archive_folder_itself_are_refused() {
    let base = temp_dir_for("rm_outside");
    let mods = base.join("mods");
    std::fs::create_dir_all(mods.join(ARCHIVE)).unwrap();
    let elsewhere = base.join("FS25_Elsewhere.zip");
    std::fs::write(&elsewhere, b"PK").unwrap();
    std::fs::create_dir_all(mods.join("sub")).unwrap();
    let nested = mods.join("sub").join("FS25_Nested.zip");
    std::fs::write(&nested, b"PK").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[
            item("FS25_Elsewhere", &elsewhere),
            item("FS25_Nested", &nested),
            item("archive", &mods.join(ARCHIVE)),
            item("FS25_Dots", &mods.join("..").join("FS25_Elsewhere.zip")),
        ],
        true,
        &bin_at(&bin),
    );
    assert!(rep.removed.is_empty(), "{:?}", rep.removed);
    assert_eq!(rep.errors.len(), 4, "{:?}", rep.errors);
    assert!(elsewhere.exists() && nested.exists() && mods.join(ARCHIVE).is_dir());
    assert!(!bin.exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn silos_own_copy_of_a_library_mod_is_refused_as_a_loose_item() {
    // A byte-identical copy at a library mod's name is Silo's projection: removing it as a loose
    // file would leave the library entry behind with no say.
    let (base, conn, layout, _src) = library_with_foo("rm_projcopy", false);
    let copy = layout.mods.join("FS25_Foo.zip");
    std::fs::write(&copy, b"PK\x03\x04 Foo").unwrap();
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[item("FS25_Foo", &copy)],
        true,
        &bin_at(&bin),
    );
    assert!(rep.removed.is_empty());
    assert!(
        rep.errors[0].contains("Silo's copy of a mod in your library"),
        "{:?}",
        rep.errors
    );
    assert!(copy.exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn a_loose_link_is_left_alone() {
    let base = temp_dir_for("rm_link");
    let mods = base.join("mods");
    std::fs::create_dir_all(&mods).unwrap();
    let target = base.join("FS25_Target");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, mods.join("FS25_Target")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[item("FS25_Target", &mods.join("FS25_Target"))],
        true,
        &bin_at(&bin),
    );
    assert!(rep.removed.is_empty());
    assert!(rep.errors[0].contains("is a link"), "{:?}", rep.errors);
    assert!(target.is_dir());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_loose_item_must_be_the_named_mod() {
    let base = temp_dir_for("rm_notit");
    let mods = base.join("mods");
    std::fs::create_dir_all(mods.join("backups")).unwrap();
    std::fs::write(mods.join("backups/FS25_Old.zip.1.bak"), b"PK").unwrap();
    std::fs::create_dir_all(mods.join("FS25_NoDesc")).unwrap();
    std::fs::write(mods.join("FS25_Other.zip"), b"PK other").unwrap();
    std::fs::write(mods.join("notes.txt"), b"hi").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let rep = remove_from_library(
        &conn,
        &layout,
        &[
            item("FS25_Foo", &mods.join("backups")),
            item("FS25_Foo", &mods.join("FS25_Other.zip")),
            item("FS25_NoDesc", &mods.join("FS25_NoDesc")),
            item("notes", &mods.join("notes.txt")),
        ],
        true,
        &no_bin,
    );
    assert!(rep.removed.is_empty());
    assert_eq!(rep.errors.len(), 4, "{:?}", rep.errors);
    assert!(
        rep.errors
            .iter()
            .all(|e| e.contains("isn't that mod") || e.contains("Silo's own")),
        "{:?}",
        rep.errors
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_zip_changed_since_confirmed_is_left() {
    let base = temp_dir_for("rm_changed");
    let mods = base.join("mods");
    std::fs::create_dir_all(&mods).unwrap();
    let zip = mods.join("FS25_Loose.zip");
    std::fs::write(&zip, b"PK the build you saw").unwrap();
    let seen = item("FS25_Loose", &zip);
    std::fs::write(&zip, b"PK another build put here meanwhile").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let rep = remove_from_library(&conn, &layout, &[seen], true, &no_bin);
    assert!(rep.removed.is_empty());
    assert!(
        rep.errors[0].contains("changed since you confirmed"),
        "{:?}",
        rep.errors
    );
    // As it is now, it goes.
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[item("FS25_Loose", &zip)],
        true,
        &bin_at(&bin),
    );
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert!(bin.join("FS25_Loose.zip").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_same_path_twice_is_removed_once() {
    let (base, conn, layout, src) = library_with_foo("rm_twice", false);
    let bin = base.join("bin");
    let rep = remove_from_library(
        &conn,
        &layout,
        &[item("FS25_Foo", &src), item("FS25_Foo", &src)],
        true,
        &bin_at(&bin),
    );
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.removed.len(), 1);
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn a_category_folder_swapped_for_a_link_is_refused() {
    // The archived copy's category folder replaced by a link to somewhere else holding a file of
    // that name: the move would land outside the library.
    let (base, conn, layout, src) = library_with_foo("rm_catlink", false);
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("FS25_Foo.zip"), b"not the library's").unwrap();
    let cat = src.parent().unwrap().to_path_buf();
    std::fs::remove_dir_all(&cat).unwrap();
    std::os::unix::fs::symlink(&outside, &cat).unwrap();
    let rep = remove_from_library(&conn, &layout, &[item("FS25_Foo", &src)], true, &no_bin);
    assert!(rep.removed.is_empty());
    assert!(
        rep.errors[0].contains("isn't where Silo expects it"),
        "{:?}",
        rep.errors
    );
    assert!(outside.join("FS25_Foo.zip").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn an_archived_copy_that_is_a_link_is_refused() {
    let (base, conn, layout, src) = library_with_foo("rm_leaflink", false);
    let outside = base.join("elsewhere.zip");
    std::fs::write(&outside, b"PK elsewhere").unwrap();
    std::fs::remove_file(&src).unwrap();
    std::os::unix::fs::symlink(&outside, &src).unwrap();
    let rep = remove_from_library(&conn, &layout, &[item("FS25_Foo", &src)], true, &no_bin);
    assert!(rep.removed.is_empty());
    assert!(rep.errors[0].contains("is a link"), "{:?}", rep.errors);
    assert!(outside.is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn an_archived_zip_replaced_by_a_folder_is_refused_before_parking() {
    let (base, conn, layout, src) = library_with_foo("rm_kindswap", true);
    let seen = item("FS25_Foo", &src);
    // Swapped for a folder after the confirmation: nothing may move, and the mod stays on.
    std::fs::remove_file(&src).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("notes.txt"), b"someone else's folder").unwrap();
    let rep = remove_from_library(&conn, &layout, &[seen], true, &no_bin);
    assert!(rep.removed.is_empty());
    assert!(
        rep.errors[0].contains("changed since you confirmed"),
        "{:?}",
        rep.errors
    );
    assert!(src.join("notes.txt").is_file());
    let key = crate::modsroot::canonical_key(&layout.mods);
    assert!(
        db::load_organized(&conn, &key)[0].active,
        "not parked for a refused removal"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn silos_own_folders_are_refused_even_named_and_with_a_moddesc() {
    let base = temp_dir_for("rm_reserved");
    let mods = base.join("mods");
    for d in ["archive", "backups", "Backups"] {
        std::fs::create_dir_all(mods.join(d)).unwrap();
        std::fs::write(mods.join(d).join("modDesc.xml"), "<modDesc/>").unwrap();
    }
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let rep = remove_from_library(
        &conn,
        &layout,
        &[
            item("archive", &mods.join("archive")),
            item("backups", &mods.join("backups")),
            item("Backups", &mods.join("Backups")),
        ],
        true,
        &no_bin,
    );
    assert!(rep.removed.is_empty());
    assert_eq!(rep.errors.len(), 3, "{:?}", rep.errors);
    assert!(
        rep.errors.iter().all(|e| e.contains("Silo's own")),
        "{:?}",
        rep.errors
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn a_slot_silo_cant_inspect_refuses() {
    // The archive elsewhere (readable), the mods folder unsearchable: the slot can't be checked.
    use std::os::unix::fs::PermissionsExt;
    let base = temp_dir_for("rm_noslot");
    let mods = base.join("mods");
    let arch = base.join("lib");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::create_dir_all(arch.join("Vehicles")).unwrap();
    let src = arch.join("Vehicles/FS25_Foo.zip");
    std::fs::write(&src, b"PK Foo").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: arch.clone(),
    };
    organize_row(&conn, &mods, "FS25_Foo", "Vehicles", false);
    let seen = item("FS25_Foo", &src);
    std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o600)).unwrap();
    if mods
        .join("FS25_Foo.zip")
        .symlink_metadata()
        .map_err(|e| e.kind())
        == Err(std::io::ErrorKind::NotFound)
    {
        // Running as root: permissions don't apply, nothing to test.
        std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
        return;
    }
    let rep = remove_from_library(&conn, &layout, &[seen], true, &no_bin);
    std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(rep.removed.is_empty());
    assert!(
        rep.errors[0].contains("couldn't check FS25_Foo.zip in your mods folder"),
        "{:?}",
        rep.errors
    );
    assert!(src.is_file());
    let _ = std::fs::remove_dir_all(&base);
}
