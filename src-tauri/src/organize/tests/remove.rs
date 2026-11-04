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
fn the_users_own_build_at_its_name_stays_when_the_library_copy_goes() {
    // Flagged active, but the entry at its name is the user's own different build, not Silo's
    // link to it: nothing is parked, the library copy goes, and the user's file is untouched.
    let (base, conn, layout, src) = library_with_foo("rm_noprk", false);
    db::set_organized_active(&conn, &key_of(&layout), "FS25_Foo", true).unwrap();
    let theirs = layout.mods.join("FS25_Foo.zip");
    std::fs::write(&theirs, b"the user's own build").unwrap();
    let bin = base.join("bin");
    let (plan, rep) = remove(
        &conn,
        &layout,
        &[req("FS25_Foo", &src)],
        true,
        &bin_at(&bin),
    );
    assert!(
        !plan.items[0].on,
        "the entry isn't its copy, so it isn't parked"
    );
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert!(bin.join("FS25_Foo.zip").is_file());
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
    let kept = |_: &Path| Binned::Kept("couldn't move it to the Recycle Bin (busy)".into());
    let (_, rep) = remove(&conn, &layout, &[req("FS25_Foo", &src)], true, &kept);
    assert!(rep.removed.is_empty());
    assert_eq!(
        rep.errors,
        vec![
            "FS25_Foo: couldn't move it to the Recycle Bin (busy). It's still in your library, parked."
                .to_string()
        ]
    );
    assert_eq!(db::load_organized(&conn, &key_of(&layout)).len(), 1);
    // No in Windows' own prompt (#442): the user's choice, said as a notice, not a problem.
    let declined = |_: &Path| Binned::Declined;
    let (_, rep) = remove(&conn, &layout, &[req("FS25_Foo", &src)], true, &declined);
    assert!(
        rep.removed.is_empty() && rep.errors.is_empty(),
        "{:?}",
        rep.errors
    );
    assert_eq!(
        rep.notices,
        vec![
            "FS25_Foo.zip: Windows didn't remove it (you said No when it asked, or it stopped), so it's still in your library, parked."
                .to_string()
        ]
    );
    assert_eq!(db::load_organized(&conn, &key_of(&layout)).len(), 1);
    // A mod with no library row (here a loose zip) is still where it was, wherever that is.
    let loose = layout.mods.join("FS25_Bar.zip");
    std::fs::write(&loose, b"PK bar").unwrap();
    let (_, rep) = remove(&conn, &layout, &[req("FS25_Bar", &loose)], true, &declined);
    assert_eq!(
        rep.notices,
        vec![
            "FS25_Bar.zip: Windows didn't remove it (you said No when it asked, or it stopped), so it's still where it was."
                .to_string()
        ]
    );
    assert!(loose.exists());
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
    // Moving A takes a while; meanwhile B is replaced by another file.
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
        if replace {
            // Another copy put in its place, with the same content: told apart by which files
            // they are. The original is moved aside, not deleted, so its file numbers stay in
            // use: deleted, the new copy could get the same ones within the same millisecond on
            // Linux and match the snapshot exactly (#444).
            std::fs::rename(&folder, base.join("kept-original")).unwrap();
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("modDesc.xml"), "<modDesc/>").unwrap();
        } else {
            std::fs::remove_dir_all(&folder).unwrap();
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

#[test]
fn a_kept_copy_that_changes_partway_stops_the_rest_and_reports_what_went() {
    // Three copies: keep the folder, remove two library copies. The kept copy changes while the
    // first one moves: the first is gone (and reported so), the second stays.
    let (base, conn, layout, src, folder) = foo_twice("rm_keep_partway");
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let second = tools.join("FS25_Foo.zip");
    std::fs::write(&second, b"PK a second library copy").unwrap();
    let k = keeper(&layout, &req("FS25_Foo", &folder)).unwrap();
    let (plan, found) = plan_removal_keeping(
        &conn,
        &layout,
        &[req("FS25_Foo", &src), req("FS25_Foo", &second)],
        Some(&k),
        true,
    );
    assert_eq!(found.len(), 2, "{:?}", plan.refused);
    let bin = base.join("bin");
    let moves = std::cell::Cell::new(0);
    let changing_bin = |p: &Path| {
        moves.set(moves.get() + 1);
        std::fs::write(folder.join("readme.txt"), b"edited while removing").unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::rename(
            p,
            bin.join(format!(
                "{}-{}",
                moves.get(),
                p.file_name().unwrap().to_string_lossy()
            )),
        )
        .unwrap();
        Binned::Recycled
    };
    let rep = carry_out(&conn, &layout, &found, Some(&k), true, &changing_bin);
    assert_eq!(moves.get(), 1, "only the first moved");
    assert_eq!(rep.removed.len(), 1);
    assert_eq!(rep.errors.len(), 1, "{:?}", rep.errors);
    assert!(
        rep.errors[0].contains("changed partway through"),
        "{:?}",
        rep.errors
    );
    assert!(!rep.errors[0].contains("removed nothing"));
    assert!(second.is_file(), "the second copy stays");
    let _ = std::fs::remove_dir_all(&base);
}

/// Two library copies of FS25_Foo: the recorded one (Vehicles) linked into the mods folder, and an
/// unrecorded second copy (Tools). Both can show as on in the Library.
fn foo_two_library_copies(tag: &str) -> (PathBuf, Connection, Layout, PathBuf, PathBuf) {
    let (base, conn, layout, recorded) = library_with_foo(tag, true);
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let second = tools.join("FS25_Foo.zip");
    std::fs::write(&second, b"PK the second library copy").unwrap();
    (base, conn, layout, recorded, second)
}

#[test]
fn the_plan_tells_whether_the_kept_copy_is_loaded_in_both_directions() {
    // Keep the linked recorded copy, remove the second: it's loaded, and removing the second parks
    // nothing, so the mod stays on.
    let (base, conn, layout, recorded, second) = foo_two_library_copies("rm_keep_linked");
    let k = keeper(&layout, &req("FS25_Foo", &recorded)).unwrap();
    let (plan, found) =
        plan_removal_keeping(&conn, &layout, &[req("FS25_Foo", &second)], Some(&k), true);
    assert_eq!(found.len(), 1, "{:?}", plan.refused);
    assert_eq!(plan.keep_on, Some(true));
    assert!(!plan.items[0].on, "nothing to park");
    let _ = std::fs::remove_dir_all(&base);

    // Keep the second, remove the linked recorded copy: the kept one isn't loaded, and removing
    // the recorded one parks it, so the mod ends up off.
    let (base, conn, layout, recorded, second) = foo_two_library_copies("rm_keep_unlinked");
    let k = keeper(&layout, &req("FS25_Foo", &second)).unwrap();
    let (plan, found) = plan_removal_keeping(
        &conn,
        &layout,
        &[req("FS25_Foo", &recorded)],
        Some(&k),
        true,
    );
    assert_eq!(found.len(), 1, "{:?}", plan.refused);
    assert_eq!(plan.keep_on, Some(false));
    assert!(plan.items[0].on, "it's parked first");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn byte_identical_library_copies_tell_the_linked_one_from_the_spare() {
    // The recorded copy (Vehicles) is linked into the mods folder; the second copy (Tools) holds
    // the very same bytes. Keeping the recorded one removes the spare and leaves the link.
    let (base, conn, layout, recorded) = library_with_foo("rm_twin_bytes", true);
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let spare = tools.join("FS25_Foo.zip");
    std::fs::copy(&recorded, &spare).unwrap();
    let link = layout.mods.join("FS25_Foo.zip");
    let k = keeper(&layout, &req("FS25_Foo", &recorded)).unwrap();
    let (plan, found) =
        plan_removal_keeping(&conn, &layout, &[req("FS25_Foo", &spare)], Some(&k), true);
    assert_eq!(found.len(), 1, "{:?}", plan.refused);
    assert_eq!(plan.keep_on, Some(true), "the kept copy is the linked one");
    let bin = base.join("bin");
    let rep = carry_out(&conn, &layout, &found, Some(&k), true, &bin_at(&bin));
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert!(
        recorded.is_file() && link.exists(),
        "the kept copy and its link stay"
    );
    assert!(bin.join("FS25_Foo.zip").is_file());
    let _ = std::fs::remove_dir_all(&base);

    // The other way: keep the spare, remove the recorded copy. The spare isn't what's linked.
    let (base, conn, layout, recorded) = library_with_foo("rm_twin_bytes_inverse", true);
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let spare = tools.join("FS25_Foo.zip");
    std::fs::copy(&recorded, &spare).unwrap();
    let k = keeper(&layout, &req("FS25_Foo", &spare)).unwrap();
    let (plan, found) = plan_removal_keeping(
        &conn,
        &layout,
        &[req("FS25_Foo", &recorded)],
        Some(&k),
        true,
    );
    assert_eq!(found.len(), 1, "{:?}", plan.refused);
    assert_eq!(plan.keep_on, Some(false), "the spare isn't what Silo links");
    assert!(plan.items[0].on, "the recorded copy is parked first");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_slot_linked_to_the_spare_serves_the_spare_whatever_the_bytes() {
    // Byte-identical copies, but the mods-folder slot is a hardlink of the second (Tools) copy:
    // that copy is the loaded one, so it isn't a spare, and the recorded copy isn't loaded.
    let (base, conn, layout, recorded) = library_with_foo("rm_slot_spare", false);
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let second = tools.join("FS25_Foo.zip");
    std::fs::copy(&recorded, &second).unwrap();
    let slot = layout.mods.join("FS25_Foo.zip");
    std::fs::hard_link(&second, &slot).unwrap();
    let k = keeper(&layout, &req("FS25_Foo", &recorded)).unwrap();
    let (plan, found) =
        plan_removal_keeping(&conn, &layout, &[req("FS25_Foo", &second)], Some(&k), true);
    assert!(found.is_empty());
    assert!(
        plan.refused[0].contains("the game loads this copy"),
        "{:?}",
        plan.refused
    );
    assert_eq!(
        plan.keep_on,
        Some(false),
        "the recorded copy isn't the loaded one"
    );
    assert!(second.is_file() && slot.is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_slot_switched_to_the_spare_while_confirming_refuses_it() {
    // Planned with the slot linked to the recorded copy (the second is a spare); before it runs
    // the slot is relinked to the second copy, which is then loaded and must stay.
    let (base, conn, layout, recorded) = library_with_foo("rm_slot_switch", true);
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let second = tools.join("FS25_Foo.zip");
    std::fs::copy(&recorded, &second).unwrap();
    let slot = layout.mods.join("FS25_Foo.zip");
    let k = keeper(&layout, &req("FS25_Foo", &recorded)).unwrap();
    let (plan, found) =
        plan_removal_keeping(&conn, &layout, &[req("FS25_Foo", &second)], Some(&k), true);
    assert_eq!(found.len(), 1, "{:?}", plan.refused);
    std::fs::remove_file(&slot).unwrap();
    std::fs::hard_link(&second, &slot).unwrap();
    let rep = carry_out(&conn, &layout, &found, Some(&k), true, &no_bin);
    assert!(rep.removed.is_empty());
    assert!(!rep.errors.is_empty(), "refused");
    assert!(second.is_file());
    let _ = std::fs::remove_dir_all(&base);
}

/// Byte-identical library copies of FS25_Foo: the recorded one (Vehicles, `active` as given) and
/// an unrecorded second (Tools), with the mods-folder entry a hardlink of the second.
fn foo_linked_to_second(
    tag: &str,
    active: bool,
) -> (PathBuf, Connection, Layout, PathBuf, PathBuf, PathBuf) {
    let (base, conn, layout, recorded) = library_with_foo(tag, false);
    db::set_organized_active(&conn, &key_of(&layout), "FS25_Foo", active).unwrap();
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let second = tools.join("FS25_Foo.zip");
    std::fs::copy(&recorded, &second).unwrap();
    let slot = layout.mods.join("FS25_Foo.zip");
    std::fs::hard_link(&second, &slot).unwrap();
    (base, conn, layout, recorded, second, slot)
}

#[test]
fn removing_the_recorded_copy_never_takes_the_loaded_seconds_entry() {
    for active in [false, true] {
        let (base, conn, layout, recorded, second, slot) = foo_linked_to_second(
            if active {
                "rm_rec_active"
            } else {
                "rm_rec_parked"
            },
            active,
        );
        let k = keeper(&layout, &req("FS25_Foo", &second)).unwrap();
        let (plan, found) = plan_removal_keeping(
            &conn,
            &layout,
            &[req("FS25_Foo", &recorded)],
            Some(&k),
            true,
        );
        assert_eq!(found.len(), 1, "{:?}", plan.refused);
        assert!(
            !plan.items[0].on,
            "the entry is the second's, so nothing is parked"
        );
        assert_eq!(plan.keep_on, Some(true));
        let bin = base.join("bin");
        let rep = carry_out(&conn, &layout, &found, Some(&k), true, &bin_at(&bin));
        assert!(rep.errors.is_empty(), "active {active}: {:?}", rep.errors);
        assert!(bin.join("FS25_Foo.zip").is_file(), "the recorded copy went");
        assert!(
            second.is_file() && slot.is_file(),
            "the second and its entry stay"
        );
        assert_eq!(
            crate::scratch::entry_number(&slot),
            crate::scratch::entry_number(&second)
        );
        assert!(db::load_organized(&conn, &key_of(&layout)).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[test]
fn an_entry_switched_to_the_second_while_confirming_stops_removing_the_recorded_copy() {
    // Planned with the entry linked to the recorded copy (on: parked first); before it runs the
    // entry is relinked to the second. Removing the recorded copy now would park the second's
    // entry, so it's refused and nothing changes.
    let (base, conn, layout, recorded) = library_with_foo("rm_rec_switch", true);
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let second = tools.join("FS25_Foo.zip");
    std::fs::copy(&recorded, &second).unwrap();
    let slot = layout.mods.join("FS25_Foo.zip");
    let k = keeper(&layout, &req("FS25_Foo", &second)).unwrap();
    let (plan, found) = plan_removal_keeping(
        &conn,
        &layout,
        &[req("FS25_Foo", &recorded)],
        Some(&k),
        true,
    );
    assert_eq!(found.len(), 1, "{:?}", plan.refused);
    assert!(plan.items[0].on);
    std::fs::remove_file(&slot).unwrap();
    std::fs::hard_link(&second, &slot).unwrap();
    let rep = carry_out(&conn, &layout, &found, Some(&k), true, &no_bin);
    assert!(rep.removed.is_empty());
    assert!(!rep.errors.is_empty(), "refused");
    assert!(recorded.is_file() && second.is_file());
    assert_eq!(
        crate::scratch::entry_number(&slot),
        crate::scratch::entry_number(&second)
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn parking_for_a_batch_touches_only_the_mods_being_removed() {
    // Recorded A is flagged parked while an independent byte-identical second copy owns the
    // loaded entry; another mod X is on. Removing A and X parks only X: the second's entry stays.
    // X is turned on first, while FS25_Foo's name is still empty; then the second copy and its
    // entry appear (a whole-library pass now would take that entry: the point of the test).
    let (base, conn, layout, recorded) = library_with_foo("rm_batch_scope", false);
    let x = layout.archive.join("Vehicles/FS25_X.zip");
    std::fs::write(&x, b"PK X").unwrap();
    organize_row(&conn, &layout.mods, "FS25_X", "Vehicles", false);
    let on: HashSet<String> = ["FS25_X".to_string()].into_iter().collect();
    assert!(set_active(&conn, &layout, &on).errors.is_empty());
    let x_link = layout.mods.join("FS25_X.zip");
    assert!(x_link.exists());
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let second = tools.join("FS25_Foo.zip");
    std::fs::copy(&recorded, &second).unwrap();
    let slot = layout.mods.join("FS25_Foo.zip");
    std::fs::hard_link(&second, &slot).unwrap();
    let before = crate::scratch::entry_number(&slot);
    let k = keeper(&layout, &req("FS25_Foo", &second)).unwrap();
    let (plan, found) = plan_removal_keeping(
        &conn,
        &layout,
        &[req("FS25_Foo", &recorded), req("FS25_X", &x)],
        Some(&k),
        true,
    );
    assert_eq!(found.len(), 2, "{:?}", plan.refused);
    let bin = base.join("bin");
    let rep = carry_out(&conn, &layout, &found, Some(&k), true, &bin_at(&bin));
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert!(!x_link.exists(), "X was parked");
    assert!(slot.is_file(), "the second copy's entry stays");
    assert_eq!(
        crate::scratch::entry_number(&slot),
        before,
        "the very same file"
    );
    assert!(second.is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_loose_copy_sharing_the_parked_names_isnt_taken_for_the_parked_one() {
    // Three copies: the recorded one linked in, an unzipped folder of it in the mods folder, and a
    // second library copy kept. Removing the recorded one parks it; the folder (same name, never
    // parked) must still pass its own check and go too.
    let (base, conn, layout, recorded) = library_with_foo("rm_three_copies", true);
    let folder = layout.mods.join("FS25_Foo");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("modDesc.xml"), "<modDesc/>").unwrap();
    let tools = layout.archive.join("Tools");
    std::fs::create_dir_all(&tools).unwrap();
    let kept = tools.join("FS25_Foo.zip");
    std::fs::write(&kept, b"PK the kept copy").unwrap();
    let link = layout.mods.join("FS25_Foo.zip");
    let k = keeper(&layout, &req("FS25_Foo", &kept)).unwrap();
    let (plan, found) = plan_removal_keeping(
        &conn,
        &layout,
        &[req("FS25_Foo", &recorded), req("FS25_Foo", &folder)],
        Some(&k),
        true,
    );
    assert_eq!(found.len(), 2, "{:?}", plan.refused);
    let bin = base.join("bin");
    let rep = carry_out(&conn, &layout, &found, Some(&k), true, &bin_at(&bin));
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.removed.len(), 2);
    assert!(
        link.symlink_metadata().is_err(),
        "the recorded copy's link is gone"
    );
    assert!(!folder.exists() && !recorded.exists());
    assert_eq!(
        std::fs::read(&kept).unwrap(),
        b"PK the kept copy",
        "the kept copy is unchanged"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn keeping_the_loaded_library_copy_while_a_loaded_folder_goes_keeps_it_on() {
    // The recorded copy is linked in, and an unzipped folder of it sits in the mods folder too.
    // Keeping the recorded copy removes the folder; the recorded copy's link stays (still on).
    let (base, conn, layout, recorded) = library_with_foo("rm_keep_loaded_lib", true);
    let folder = layout.mods.join("FS25_Foo");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("modDesc.xml"), "<modDesc/>").unwrap();
    let link = layout.mods.join("FS25_Foo.zip");
    let before = crate::scratch::entry_number(&link);
    let k = keeper(&layout, &req("FS25_Foo", &recorded)).unwrap();
    let (plan, found) =
        plan_removal_keeping(&conn, &layout, &[req("FS25_Foo", &folder)], Some(&k), true);
    assert_eq!(found.len(), 1, "{:?}", plan.refused);
    assert_eq!(plan.keep_on, Some(true));
    assert!(plan.items[0].on, "the folder is loaded too");
    let bin = base.join("bin");
    let rep = carry_out(&conn, &layout, &found, Some(&k), true, &bin_at(&bin));
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert!(!folder.exists());
    assert!(
        recorded.is_file() && link.exists(),
        "the kept copy and its link stay"
    );
    assert_eq!(crate::scratch::entry_number(&link), before);
    assert!(
        db::load_organized(&conn, &key_of(&layout))[0].active,
        "still on"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #442: Windows' Recycle Bin reads paths of up to 259 characters. A mod with something deeper
/// is refused when the removal is planned, before anything is parked, and nothing moves.
#[cfg(windows)]
#[test]
fn a_mod_too_deep_for_the_recycle_bin_is_refused_before_anything_moves() {
    let base = temp_dir_for("rm_too_deep");
    // Lengths in characters (UTF-16 units), as Windows counts them, under a parent whose letters
    // take more bytes than characters.
    let mods = base.join("张三丰 mods");
    let folder = mods.join("FS25_Deep");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("modDesc.xml"), b"<modDesc/>").unwrap();
    let units = |p: &Path| p.to_string_lossy().encode_utf16().count();
    let budget = 259 - units(&folder);
    let deep = folder
        .join("x".repeat(budget / 2))
        .join("y".repeat(budget - budget / 2));
    assert_eq!(units(&deep), 261, "two characters over the limit");
    std::fs::create_dir_all(&deep).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let layout = Layout::default_for(&mods);
    let (plan, rep) = remove(&conn, &layout, &[req("FS25_Deep", &folder)], true, &no_bin);
    assert!(plan.items.is_empty());
    assert!(
        plan.refused[0].contains("259 characters"),
        "{:?}",
        plan.refused
    );
    assert!(rep.removed.is_empty() && rep.errors.is_empty());
    assert!(deep.exists());
    let _ = std::fs::remove_dir_all(&base);
}
