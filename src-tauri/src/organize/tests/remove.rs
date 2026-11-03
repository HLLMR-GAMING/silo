//! Tests for `organize/remove.rs`.

use super::*;

/// A stand-in Recycle Bin: moves the entry into `bin` (so a test can see what went where).
fn bin_at(bin: &Path) -> impl Fn(&Path) -> Binned + '_ {
    move |p: &Path| {
        std::fs::create_dir_all(bin).unwrap();
        match std::fs::rename(p, bin.join(p.file_name().unwrap())) {
            Ok(()) => Binned::Recycled,
            Err(e) => Binned::Kept(e.to_string()),
        }
    }
}

/// A Recycle Bin that must never be reached.
fn no_bin(p: &Path) -> Binned {
    panic!("moved {} to the bin", p.display())
}

fn req(tech: &str, path: &Path) -> RemoveRequest {
    RemoveRequest {
        tech_name: tech.into(),
        path: path.to_string_lossy().into_owned(),
    }
}

/// Plan, then carry out at once (as confirming straight away would).
fn remove(
    conn: &Connection,
    layout: &Layout,
    reqs: &[RemoveRequest],
    archive_ok: bool,
    bin: &dyn Fn(&Path) -> Binned,
) -> (RemovalPlan, RemoveReport) {
    let (plan, found) = plan_removal(conn, layout, reqs, archive_ok);
    let rep = carry_out(conn, layout, &found, None, archive_ok, bin);
    (plan, rep)
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

fn key_of(layout: &Layout) -> String {
    crate::modsroot::canonical_key(&layout.mods)
}

#[test]
fn removing_a_parked_library_mod_bins_its_archived_copy_and_drops_its_row() {
    let (base, conn, layout, src) = library_with_foo("rm_parked", false);
    let bin = base.join("bin");
    let (plan, rep) = remove(
        &conn,
        &layout,
        &[req("FS25_Foo", &src)],
        true,
        &bin_at(&bin),
    );
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].file_name, "FS25_Foo.zip");
    assert!(!plan.items[0].on);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(
        rep.removed,
        vec![Removed {
            tech_name: "FS25_Foo".into(),
            path: src.to_string_lossy().into_owned(),
            recycled: true,
        }]
    );
    assert_eq!(
        std::fs::read(bin.join("FS25_Foo.zip")).unwrap(),
        b"PK\x03\x04 Foo"
    );
    assert!(db::load_organized(&conn, &key_of(&layout)).is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn removing_an_active_library_mod_parks_it_first() {
    let (base, conn, layout, src) = library_with_foo("rm_active", true);
    let bin = base.join("bin");
    let (plan, rep) = remove(
        &conn,
        &layout,
        &[req("FS25_Foo", &src)],
        true,
        &bin_at(&bin),
    );
    assert!(plan.items[0].on, "the confirmation says it's parked first");
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert!(
        !layout.mods.join("FS25_Foo.zip").exists(),
        "no link left in the mods folder"
    );
    assert!(bin.join("FS25_Foo.zip").exists());
    assert!(db::load_organized(&conn, &key_of(&layout)).is_empty());
    assert!(db::projection_stats(&conn, &key_of(&layout), "FS25_Foo.zip").is_none());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_mod_that_cant_be_parked_stays() {
    // Active, with the user's own different build at its name: parking refuses, so nothing goes.
    let (base, conn, layout, src) = library_with_foo("rm_noprk", false);
    db::set_organized_active(&conn, &key_of(&layout), "FS25_Foo", true).unwrap();
    let theirs = layout.mods.join("FS25_Foo.zip");
    std::fs::write(&theirs, b"the user's own build").unwrap();
    let (_, rep) = remove(&conn, &layout, &[req("FS25_Foo", &src)], true, &no_bin);
    assert!(rep.removed.is_empty());
    assert!(
        rep.errors[0].starts_with("FS25_Foo: ") && rep.errors[0].ends_with("It wasn't removed."),
        "{:?}",
        rep.errors
    );
    assert!(src.exists());
    assert_eq!(std::fs::read(&theirs).unwrap(), b"the user's own build");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_library_mod_is_refused_while_the_archive_is_unreachable() {
    let (base, conn, layout, src) = library_with_foo("rm_noarch", false);
    let (plan, rep) = remove(&conn, &layout, &[req("FS25_Foo", &src)], false, &no_bin);
    assert!(plan.items.is_empty());
    assert!(
        plan.refused[0].contains("isn't reachable"),
        "{:?}",
        plan.refused
    );
    assert!(rep.removed.is_empty() && rep.errors.is_empty());
    assert!(src.exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_mod_the_bin_kept_stays_in_the_library_and_one_deleted_is_removed_and_said_so() {
    let (base, conn, layout, src) = library_with_foo("rm_bin_outcomes", false);
    let kept = |_: &Path| Binned::Kept("you chose No in Windows' prompt".into());
    let (_, rep) = remove(&conn, &layout, &[req("FS25_Foo", &src)], true, &kept);
    assert!(rep.removed.is_empty());
    assert_eq!(
        rep.errors,
        vec![
            "FS25_Foo: you chose No in Windows' prompt. It's still in your library, parked."
                .to_string()
        ]
    );
    assert_eq!(db::load_organized(&conn, &key_of(&layout)).len(), 1);
    // Deleted permanently (the user said Yes in Windows' prompt): removed, not recycled, and said.
    let deleted = |p: &Path| {
        std::fs::remove_file(p).unwrap();
        Binned::Gone(
            "Windows deleted it permanently instead of moving it to the Recycle Bin".into(),
        )
    };
    let (_, rep) = remove(&conn, &layout, &[req("FS25_Foo", &src)], true, &deleted);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.removed.len(), 1);
    assert!(!rep.removed[0].recycled);
    assert_eq!(
        rep.notices,
        vec![
            "FS25_Foo.zip: Windows deleted it permanently instead of moving it to the Recycle Bin."
                .to_string()
        ]
    );
    assert!(db::load_organized(&conn, &key_of(&layout)).is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_mod_silo_cant_check_after_the_move_stays_listed() {
    let (base, conn, layout, src) = library_with_foo("rm_unchecked", false);
    let unchecked =
        |_: &Path| Binned::Unchecked("Silo couldn't check where it is now (denied)".into());
    let (_, rep) = remove(&conn, &layout, &[req("FS25_Foo", &src)], true, &unchecked);
    assert!(rep.removed.is_empty());
    assert!(
        rep.errors[0].ends_with("It's still listed in your library."),
        "{:?}",
        rep.errors
    );
    assert_eq!(db::load_organized(&conn, &key_of(&layout)).len(), 1);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_later_item_replaced_while_an_earlier_one_moves_is_refused() {
    let base = temp_dir_for("rm_two_items");
    let mods = base.join("mods");
    std::fs::create_dir_all(&mods).unwrap();
    let a = mods.join("FS25_A.zip");
    let b = mods.join("FS25_B.zip");
    std::fs::write(&a, b"PK a").unwrap();
    std::fs::write(&b, b"PK the B you saw").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let (plan, found) = plan_removal(
        &conn,
        &layout,
        &[req("FS25_A", &a), req("FS25_B", &b)],
        true,
    );
    assert_eq!(plan.items.len(), 2);
    let bin = base.join("bin");
    let moved = std::cell::Cell::new(0);
    // Moving A takes a while; meanwhile B is replaced by another file of the same size.
    let slow_bin = |p: &Path| {
        moved.set(moved.get() + 1);
        assert_eq!(p, a.as_path(), "B must never reach the bin");
        std::fs::remove_file(&b).unwrap();
        std::fs::write(&b, b"PK another B, same!").unwrap();
        bin_at(&bin)(p)
    };
    let rep = carry_out(&conn, &layout, &found, None, true, &slow_bin);
    assert_eq!(moved.get(), 1);
    assert_eq!(rep.removed.len(), 1);
    assert!(
        rep.errors[0].starts_with("FS25_B: it changed since you confirmed"),
        "{:?}",
        rep.errors
    );
    assert_eq!(std::fs::read(&b).unwrap(), b"PK another B, same!");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_folder_file_replaced_with_one_of_the_same_size_and_time_is_refused() {
    let base = temp_dir_for("rm_folder_swap_same");
    let mods = base.join("mods");
    let dir = mods.join("FS25_Unzipped");
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("modDesc.xml"), b"<modDesc/>").unwrap();
    std::fs::write(dir.join("data/a.xml"), b"one").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let rep = changed_while_confirming(&conn, &layout, &[req("FS25_Unzipped", &dir)], || {
        let file = dir.join("data/a.xml");
        let when = std::fs::metadata(&file).unwrap().modified().unwrap();
        let folder_when = std::fs::metadata(dir.join("data"))
            .unwrap()
            .modified()
            .unwrap();
        let other = dir.join("data/b.tmp");
        std::fs::write(&other, b"two").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&other)
            .unwrap()
            .set_modified(when)
            .unwrap();
        std::fs::remove_file(&file).unwrap();
        std::fs::rename(&other, &file).unwrap();
        // Put the folder's own time back too, so only the file's identity tells.
        let _ = std::fs::File::open(dir.join("data")).and_then(|f| f.set_modified(folder_when));
    });
    assert!(
        rep.errors[0].contains("changed since you confirmed"),
        "{:?}",
        rep.errors
    );
    assert_eq!(std::fs::read(dir.join("data/a.xml")).unwrap(), b"two");
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
    let (plan, rep) = remove(
        &conn,
        &layout,
        &[
            req("FS25_Loose", &mods.join("FS25_Loose.zip")),
            req("FS25_Unzipped", &mods.join("FS25_Unzipped")),
        ],
        true,
        &bin_at(&bin),
    );
    assert!(
        plan.items.iter().all(|p| p.on),
        "loose mods are ones the game loads"
    );
    assert_eq!(plan.items[1].kind, "dir");
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
    let (plan, rep) = remove(
        &conn,
        &layout,
        &[
            req("FS25_Elsewhere", &elsewhere),
            req("FS25_Nested", &nested),
            req("archive", &mods.join(ARCHIVE)),
            req("FS25_Dots", &mods.join("..").join("FS25_Elsewhere.zip")),
        ],
        true,
        &no_bin,
    );
    assert!(plan.items.is_empty());
    assert_eq!(plan.refused.len(), 4, "{:?}", plan.refused);
    assert!(rep.removed.is_empty() && rep.errors.is_empty());
    assert!(elsewhere.exists() && nested.exists() && mods.join(ARCHIVE).is_dir());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_loose_item_must_be_the_named_mod() {
    let base = temp_dir_for("rm_notit");
    let mods = base.join("mods");
    std::fs::create_dir_all(mods.join("FS25_NoDesc")).unwrap();
    std::fs::write(mods.join("FS25_Other.zip"), b"PK other").unwrap();
    std::fs::write(mods.join("notes.txt"), b"hi").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let (plan, _) = remove(
        &conn,
        &layout,
        &[
            req("FS25_Foo", &mods.join("FS25_Other.zip")),
            req("FS25_NoDesc", &mods.join("FS25_NoDesc")),
            req("notes", &mods.join("notes.txt")),
        ],
        true,
        &no_bin,
    );
    assert!(plan.items.is_empty());
    assert_eq!(plan.refused.len(), 3, "{:?}", plan.refused);
    assert!(
        plan.refused.iter().all(|e| e.contains("isn't that mod")),
        "{:?}",
        plan.refused
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn silos_own_folders_are_refused_even_named_and_with_a_moddesc() {
    let base = temp_dir_for("rm_reserved");
    let mods = base.join("mods");
    for d in ["archive", "backups"] {
        std::fs::create_dir_all(mods.join(d)).unwrap();
        std::fs::write(mods.join(d).join("modDesc.xml"), "<modDesc/>").unwrap();
    }
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let (plan, _) = remove(
        &conn,
        &layout,
        &[
            req("archive", &mods.join("archive")),
            req("backups", &mods.join("backups")),
            req("FS25_Foo", &mods.join("backups")),
        ],
        true,
        &no_bin,
    );
    assert!(plan.items.is_empty());
    assert_eq!(plan.refused.len(), 2, "each path once: {:?}", plan.refused);
    assert!(
        plan.refused.iter().all(|e| e.contains("Silo's own")),
        "{:?}",
        plan.refused
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn silos_own_copy_of_a_library_mod_is_refused_as_a_loose_item() {
    let (base, conn, layout, _src) = library_with_foo("rm_projcopy", false);
    let copy = layout.mods.join("FS25_Foo.zip");
    std::fs::write(&copy, b"PK\x03\x04 Foo").unwrap();
    let (plan, _) = remove(&conn, &layout, &[req("FS25_Foo", &copy)], true, &no_bin);
    assert!(plan.items.is_empty());
    assert!(
        plan.refused[0].contains("Silo's copy of a mod in your library"),
        "{:?}",
        plan.refused
    );
    assert!(copy.exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_same_path_twice_is_removed_once() {
    let (base, conn, layout, src) = library_with_foo("rm_twice", false);
    let bin = base.join("bin");
    let (plan, rep) = remove(
        &conn,
        &layout,
        &[req("FS25_Foo", &src), req("FS25_Foo", &src)],
        true,
        &bin_at(&bin),
    );
    assert_eq!(plan.items.len(), 1);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.removed.len(), 1);
    let _ = std::fs::remove_dir_all(&base);
}

/// Plan `reqs`, run `between` (what happens while the confirmation is up), then carry out with a
/// bin that must never be reached: whatever changed must be refused before anything moves.
fn changed_while_confirming(
    conn: &Connection,
    layout: &Layout,
    reqs: &[RemoveRequest],
    between: impl FnOnce(),
) -> RemoveReport {
    let (plan, found) = plan_removal(conn, layout, reqs, true);
    assert_eq!(plan.items.len(), reqs.len(), "{:?}", plan.refused);
    between();
    carry_out(conn, layout, &found, None, true, &no_bin)
}

#[test]
fn a_zip_replaced_with_one_of_the_same_size_and_time_is_refused() {
    let base = temp_dir_for("rm_swap_same");
    let mods = base.join("mods");
    std::fs::create_dir_all(&mods).unwrap();
    let zip = mods.join("FS25_Loose.zip");
    std::fs::write(&zip, b"PK the build you saw").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let rep = changed_while_confirming(&conn, &layout, &[req("FS25_Loose", &zip)], || {
        let when = std::fs::metadata(&zip).unwrap().modified().unwrap();
        let other = mods.join("other.tmp");
        std::fs::write(&other, b"PK a different one!!").unwrap(); // same length
        std::fs::File::options()
            .write(true)
            .open(&other)
            .unwrap()
            .set_modified(when)
            .unwrap();
        std::fs::remove_file(&zip).unwrap();
        std::fs::rename(&other, &zip).unwrap();
    });
    assert!(
        rep.errors[0].contains("changed since you confirmed"),
        "{:?}",
        rep.errors
    );
    assert_eq!(std::fs::read(&zip).unwrap(), b"PK a different one!!");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_folder_whose_contents_changed_is_refused() {
    let base = temp_dir_for("rm_folder_changed");
    let mods = base.join("mods");
    let dir = mods.join("FS25_Unzipped");
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("modDesc.xml"), b"<modDesc/>").unwrap();
    std::fs::write(dir.join("data/a.xml"), b"one").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let rep = changed_while_confirming(&conn, &layout, &[req("FS25_Unzipped", &dir)], || {
        std::fs::write(dir.join("data/a.xml"), b"a longer file now").unwrap();
    });
    assert!(
        rep.errors[0].contains("changed since you confirmed"),
        "{:?}",
        rep.errors
    );
    assert!(dir.join("data/a.xml").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn an_archived_zip_replaced_by_a_folder_is_refused_before_parking() {
    let (base, conn, layout, src) = library_with_foo("rm_kindswap", true);
    let rep = changed_while_confirming(&conn, &layout, &[req("FS25_Foo", &src)], || {
        std::fs::remove_file(&src).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("notes.txt"), b"someone else's folder").unwrap();
    });
    assert!(!rep.errors.is_empty());
    assert!(src.join("notes.txt").is_file());
    assert!(
        db::load_organized(&conn, &key_of(&layout))[0].active,
        "not parked for a refused removal"
    );
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
    std::fs::write(target.join("modDesc.xml"), "<modDesc/>").unwrap();
    std::os::unix::fs::symlink(&target, mods.join("FS25_Target")).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let (plan, _) = remove(
        &conn,
        &layout,
        &[req("FS25_Target", &mods.join("FS25_Target"))],
        true,
        &no_bin,
    );
    assert!(plan.refused[0].contains("is a link"), "{:?}", plan.refused);
    assert!(target.is_dir());
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn a_category_folder_swapped_for_a_link_is_refused() {
    let (base, conn, layout, src) = library_with_foo("rm_catlink", false);
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("FS25_Foo.zip"), b"not the library's").unwrap();
    let cat = src.parent().unwrap().to_path_buf();
    std::fs::remove_dir_all(&cat).unwrap();
    std::os::unix::fs::symlink(&outside, &cat).unwrap();
    let (plan, _) = remove(&conn, &layout, &[req("FS25_Foo", &src)], true, &no_bin);
    assert!(
        plan.refused[0].contains("isn't where Silo expects it"),
        "{:?}",
        plan.refused
    );
    assert!(outside.join("FS25_Foo.zip").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn a_custom_archive_link_retargeted_while_confirming_is_refused() {
    // The library archive is a link the user chose; it's pointed at another tree holding the same
    // names before the removal runs: the confirmed library isn't the one there now.
    let base = temp_dir_for("rm_retarget");
    let mods = base.join("mods");
    std::fs::create_dir_all(&mods).unwrap();
    for t in ["real", "other"] {
        std::fs::create_dir_all(base.join(t).join("Vehicles")).unwrap();
        std::fs::write(base.join(t).join("Vehicles/FS25_Foo.zip"), b"PK Foo").unwrap();
    }
    let arch = base.join("lib");
    std::os::unix::fs::symlink(base.join("real"), &arch).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: arch.clone(),
    };
    organize_row(&conn, &mods, "FS25_Foo", "Vehicles", false);
    let src = arch.join("Vehicles/FS25_Foo.zip");
    let rep = changed_while_confirming(&conn, &layout, &[req("FS25_Foo", &src)], || {
        std::fs::remove_file(&arch).unwrap();
        std::os::unix::fs::symlink(base.join("other"), &arch).unwrap();
    });
    assert!(
        rep.errors[0].contains("changed since you confirmed"),
        "{:?}",
        rep.errors
    );
    assert!(base.join("other/Vehicles/FS25_Foo.zip").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn a_slot_silo_cant_inspect_refuses() {
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
    let (plan, found) = plan_removal(&conn, &layout, &[req("FS25_Foo", &src)], true);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.refused);
    std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o600)).unwrap();
    if matches!(
        mods.join("FS25_Foo.zip")
            .symlink_metadata()
            .map_err(|e| e.kind()),
        Err(std::io::ErrorKind::NotFound)
    ) {
        // Running as root: permissions don't apply, nothing to test.
        std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
        return;
    }
    let rep = carry_out(&conn, &layout, &found, None, true, &no_bin);
    std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(rep.removed.is_empty());
    assert!(!rep.errors.is_empty(), "refused");
    assert!(src.is_file());
    let _ = std::fs::remove_dir_all(&base);
}

// #423: two copies of a mod in the library, only one recorded. Keeping the recorded one removes
// the other, which has no row; another row's copy, or a stray file, is never taken that way.
#[test]
fn a_second_archived_copy_without_a_row_can_be_removed() {
    let (base, conn, layout, src) = library_with_foo("rm_second_copy", false);
    let other_cat = layout.archive.join("Tools");
    std::fs::create_dir_all(&other_cat).unwrap();
    let second = other_cat.join("FS25_Foo.zip");
    std::fs::write(&second, b"PK the second copy").unwrap();
    let bin = base.join("bin");
    let (plan, rep) = remove(
        &conn,
        &layout,
        &[req("FS25_Foo", &second)],
        true,
        &bin_at(&bin),
    );
    assert_eq!(plan.items.len(), 1, "{:?}", plan.refused);
    assert!(!plan.items[0].on, "not loaded, nothing to park");
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(
        std::fs::read(bin.join("FS25_Foo.zip")).unwrap(),
        b"PK the second copy"
    );
    assert!(src.is_file(), "the recorded copy stays");
    assert_eq!(
        db::load_organized(&conn, &key_of(&layout)).len(),
        1,
        "its row stays"
    );

    // Not the named mod, or another mod's recorded copy: refused.
    std::fs::write(other_cat.join("FS25_Bar.zip"), b"PK").unwrap();
    organize_row(&conn, &layout.mods, "FS25_Bar", "Tools", false);
    std::fs::write(other_cat.join("notes.zip"), b"PK").unwrap();
    let (plan, _) = remove(
        &conn,
        &layout,
        &[
            req("FS25_Foo", &other_cat.join("FS25_Bar.zip")),
            req("FS25_Foo", &other_cat.join("notes.zip")),
        ],
        true,
        &no_bin,
    );
    assert!(plan.items.is_empty());
    assert_eq!(plan.refused.len(), 2, "{:?}", plan.refused);
    assert!(other_cat.join("FS25_Bar.zip").is_file() && other_cat.join("notes.zip").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

/// The parked library copy of FS25_Foo plus an unzipped FS25_Foo folder in the mods folder.
fn foo_twice(tag: &str) -> (PathBuf, Connection, Layout, PathBuf, PathBuf) {
    let (base, conn, layout, src) = library_with_foo(tag, false);
    let folder = layout.mods.join("FS25_Foo");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("modDesc.xml"), "<modDesc/>").unwrap();
    (base, conn, layout, src, folder)
}

#[test]
fn keeping_one_copy_removes_the_other_and_never_the_kept_one() {
    let (base, conn, layout, src, folder) = foo_twice("rm_keep");
    let k = keeper(&layout, &req("FS25_Foo", &folder)).unwrap();
    let reqs = [req("FS25_Foo", &src), req("FS25_Foo", &folder)];
    let (plan, found) = plan_removal_keeping(&conn, &layout, &reqs, Some(&k), true);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.refused);
    assert!(
        plan.refused[0].contains("that's the copy you're keeping"),
        "{:?}",
        plan.refused
    );
    let bin = base.join("bin");
    let rep = carry_out(&conn, &layout, &found, Some(&k), true, &bin_at(&bin));
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert!(bin.join("FS25_Foo.zip").is_file());
    assert!(folder.join("modDesc.xml").is_file(), "the kept copy stays");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_kept_copy_gone_or_replaced_while_confirming_stops_everything() {
    for replace in [false, true] {
        let (base, conn, layout, src, folder) = foo_twice(if replace {
            "rm_keep_swap"
        } else {
            "rm_keep_gone"
        });
        let k = keeper(&layout, &req("FS25_Foo", &folder)).unwrap();
        let (_, found) =
            plan_removal_keeping(&conn, &layout, &[req("FS25_Foo", &src)], Some(&k), true);
        assert_eq!(found.len(), 1);
        std::fs::remove_dir_all(&folder).unwrap();
        if replace {
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("modDesc.xml"), "<modDesc/>").unwrap();
        }
        let rep = carry_out(&conn, &layout, &found, Some(&k), true, &no_bin);
        assert!(rep.removed.is_empty());
        assert!(
            rep.errors[0].contains("the copy you're keeping changed"),
            "{:?}",
            rep.errors
        );
        assert!(src.is_file(), "the other copy stays too");
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[test]
fn keeping_silos_link_to_a_library_copy_refuses_removing_that_copy() {
    // The "copy" in the mods folder is Silo's link to the library copy: removing the library copy
    // would park the mod, taking the kept one with it.
    let (base, conn, layout, src) = library_with_foo("rm_keep_link", true);
    let link = layout.mods.join("FS25_Foo.zip");
    let k = keeper(&layout, &req("FS25_Foo", &link)).unwrap();
    let (plan, found) =
        plan_removal_keeping(&conn, &layout, &[req("FS25_Foo", &src)], Some(&k), true);
    assert!(found.is_empty());
    assert!(
        plan.refused[0].contains("same file as the copy you're keeping")
            || plan.refused[0].contains("Silo's link to it"),
        "{:?}",
        plan.refused
    );
    assert!(link.exists() && src.is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_second_archived_copy_the_game_loads_is_refused() {
    // Two library copies; the recorded one parked, and the mods folder holding a byte-identical
    // copy of the second (Silo's projection of it): the game loads the second, so it isn't spare.
    let (base, conn, layout, _src) = library_with_foo("rm_second_loaded", false);
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let second = tools.join("FS25_Foo.zip");
    std::fs::write(&second, b"PK the loaded second copy").unwrap();
    std::fs::write(
        layout.mods.join("FS25_Foo.zip"),
        b"PK the loaded second copy",
    )
    .unwrap();
    let (plan, _) = remove(&conn, &layout, &[req("FS25_Foo", &second)], true, &no_bin);
    assert!(plan.items.is_empty());
    assert!(
        plan.refused[0].contains("the game loads this copy"),
        "{:?}",
        plan.refused
    );
    let _ = std::fs::remove_dir_all(&base);
}

// On a folder that ignores case, the recorded copy asked for under another spelling of its
// category is that row's, never a "second copy" removed without parking or tidying its records.
#[test]
fn the_recorded_copy_under_another_spelling_is_not_a_second_copy() {
    let (base, conn, layout, src) = library_with_foo("rm_spelling", false);
    let respelled = layout.archive.join("VEHICLES").join("FS25_Foo.zip");
    if !respelled.exists() {
        // A folder that tells case apart: that spelling is no file at all.
        let _ = std::fs::remove_dir_all(&base);
        return;
    }
    let (plan, _) = remove(
        &conn,
        &layout,
        &[req("FS25_Foo", &respelled)],
        true,
        &no_bin,
    );
    assert!(plan.items.is_empty());
    assert!(
        plan.refused[0].contains("the copy recorded in your library"),
        "{:?}",
        plan.refused
    );
    assert!(src.is_file());
    let _ = std::fs::remove_dir_all(&base);
}
