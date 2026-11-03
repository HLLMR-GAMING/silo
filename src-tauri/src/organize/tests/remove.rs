//! Tests for `organize/remove.rs`.

use super::*;

/// A stand-in Recycle Bin: moves the entry into `bin` (so a test can see what went where).
fn bin_at(bin: &Path) -> impl Fn(&Path) -> Result<(), String> + '_ {
    move |p: &Path| {
        std::fs::create_dir_all(bin).map_err(|e| e.to_string())?;
        std::fs::rename(p, bin.join(p.file_name().unwrap())).map_err(|e| e.to_string())
    }
}

fn item(tech: &str, path: &Path) -> RemoveItem {
    RemoveItem {
        tech_name: tech.into(),
        path: path.to_string_lossy().into_owned(),
    }
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
    assert_eq!(rep.removed, vec!["FS25_Foo".to_string()]);
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
