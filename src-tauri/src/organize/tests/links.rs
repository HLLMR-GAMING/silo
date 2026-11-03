//! Tests for `organize/links.rs`.

use super::*;

#[test]
fn dir_projection_copies_when_links_are_refused() {
    let base = std::env::temp_dir().join(format!("silo_dircopy_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let src = base.join("archived_dir");
    std::fs::create_dir_all(src.join("textures")).unwrap();
    std::fs::write(src.join("modDesc.xml"), b"<x/>").unwrap();
    std::fs::write(src.join("textures").join("a.dds"), b"dds").unwrap();
    let refuse = |_: &Path, _: &Path| -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported)) // e.g. exFAT
    };

    // Filesystem refuses links → a marked copy, which reads as ours.
    let link = base.join("FS25_FooDir");
    link_dir_or_copy(&src, &link, refuse).unwrap();
    assert_eq!(std::fs::read(link.join("modDesc.xml")).unwrap(), b"<x/>");
    assert_eq!(
        std::fs::read(link.join("textures").join("a.dds")).unwrap(),
        b"dds"
    );
    assert!(is_silo_projection(&link, &src, "dir"));

    // Name already taken (the user's own folder) → no copy merged into it, no marker.
    let taken = base.join("FS25_UserDir");
    std::fs::create_dir_all(&taken).unwrap();
    let exists = |_: &Path, _: &Path| -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
    };
    assert!(link_dir_or_copy(&src, &taken, exists).is_err());
    assert!(!taken.join("modDesc.xml").exists());
    assert!(!is_silo_projection(&taken, &src, "dir"));

    let _ = std::fs::remove_dir_all(&base);
}

fn other_err(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Other.into())
}

#[test]
fn zip_projection_falls_back_hardlink_then_symlink_then_copy() {
    let base = std::env::temp_dir().join(format!("silo_zlink_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let src = base.join("a.zip");
    std::fs::write(&src, b"PK\x03\x04 a").unwrap();

    // Hardlink works → the symlink policy is never even consulted.
    let l0 = base.join("l0.zip");
    link_zip(
        &src,
        &l0,
        || panic!("policy must not be consulted when the hardlink works"),
        |s, l| std::fs::hard_link(s, l),
        |_, _| panic!("symlink must not be attempted"),
    )
    .unwrap();

    // Hardlink refused, symlink allowed → a symlink resolving to src.
    let l1 = base.join("l1.zip");
    link_zip(&src, &l1, || true, other_err, symlink_file).unwrap();
    assert!(l1.symlink_metadata().unwrap().file_type().is_symlink() || !cfg!(unix));
    assert!(is_silo_projection(&l1, &src, "zip"));

    // Hardlink and symlink refused → a byte copy.
    let l2 = base.join("l2.zip");
    link_zip(&src, &l2, || true, other_err, other_err).unwrap();
    assert_eq!(std::fs::read(&l2).unwrap(), b"PK\x03\x04 a");

    // Symlinks not allowed → never tried.
    let l3 = base.join("l3.zip");
    link_zip(
        &src,
        &l3,
        || false,
        other_err,
        |_, _| panic!("symlink must not be attempted"),
    )
    .unwrap();
    assert_eq!(std::fs::read(&l3).unwrap(), b"PK\x03\x04 a");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn zip_projection_never_overwrites_an_existing_file() {
    let base = std::env::temp_dir().join(format!("silo_znoclobber_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let src = base.join("a.zip");
    std::fs::write(&src, b"PK\x03\x04 the archived build").unwrap();
    let theirs = base.join("theirs.zip");
    std::fs::write(&theirs, b"the user's own file").unwrap();

    // Real strategies: the hardlink reports AlreadyExists → stop, nothing else is tried.
    let e = link_zip(
        &src,
        &theirs,
        || panic!("policy must not be consulted after AlreadyExists"),
        |s, l| std::fs::hard_link(s, l),
        |_, _| panic!("symlink must not be attempted after AlreadyExists"),
    )
    .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&theirs).unwrap(), b"the user's own file");

    // A symlink reporting AlreadyExists → no copy attempt either.
    let e = link_zip(
        &src,
        &theirs,
        || true,
        other_err,
        |_, _| Err(std::io::ErrorKind::AlreadyExists.into()),
    )
    .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);

    // Even if both link strategies fail for another reason, the copy fallback refuses to
    // truncate or replace the existing file.
    assert!(link_zip(&src, &theirs, || true, other_err, other_err).is_err());
    assert_eq!(std::fs::read(&theirs).unwrap(), b"the user's own file");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn copy_fallback_leaves_nothing_behind_when_the_source_is_missing() {
    let base = std::env::temp_dir().join(format!("silo_zcopyfail_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let dst = base.join("l.zip");
    assert!(link_zip(
        &base.join("missing.zip"),
        &dst,
        || true,
        other_err,
        other_err
    )
    .is_err());
    assert!(
        dst.symlink_metadata().is_err(),
        "no partial file under the name"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_missing_source_never_becomes_a_dangling_link() {
    let base = std::env::temp_dir().join(format!("silo_zdangle_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let missing = base.join("missing.zip");

    // Real hardlink (fails NotFound); a "symlink" that would succeed regardless of the
    // target, as a real one does on Linux/macOS. The policy allows symlinks.
    let l1 = base.join("l1.zip");
    let e = link_zip(
        &missing,
        &l1,
        || true,
        |s, l| std::fs::hard_link(s, l),
        |_, l| std::fs::write(l, b""),
    )
    .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    assert!(l1.symlink_metadata().is_err(), "nothing at the link path");

    // Even if the hardlink failed for another reason, no symlink is made to a missing file.
    let l2 = base.join("l2.zip");
    assert!(link_zip(
        &missing,
        &l2,
        || true,
        other_err,
        |_, l| std::fs::write(l, b"")
    )
    .is_err());
    assert!(l2.symlink_metadata().is_err(), "nothing at the link path");

    // make_link refuses a missing source up front, for folder mods too.
    let layout = Layout::default_for(&base);
    for (name, kind) in [("l3.zip", "zip"), ("l3dir", "dir")] {
        let link = base.join(name);
        let src = base.join(format!("missing_{name}"));
        assert!(make_link(&src, &link, kind, &mut LinkPolicy::new(&layout)).is_err());
        assert!(link.symlink_metadata().is_err(), "nothing at {name}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn folder_mods_in_a_mac_app_container_are_copied_not_linked() {
    let container = Layout {
        mods: PathBuf::from(
            "/Users/me/Library/Containers/com.astragon.farmingsim25/Data/Library/Application Support/FarmingSimulator2025/mods",
        ),
        archive: PathBuf::from("/Volumes/HDD/silo"),
    };
    assert!(LinkPolicy::new(&container).mods_in_container());
    let plain = Layout::default_for(Path::new(
        "/Users/me/Library/Application Support/FarmingSimulator2025/mods",
    ));
    assert!(!LinkPolicy::new(&plain).mods_in_container());
}

/// End to end on a real (fake) container path: the dir projection is a marked copy.
#[cfg(unix)]
#[test]
fn make_link_copies_a_folder_mod_inside_a_container() {
    let base = std::env::temp_dir().join(format!("silo_container_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("Library/Containers/com.example.fs25/Data/mods");
    let archive = base.join("hdd");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::create_dir_all(archive.join("Tools/FS25_Dir")).unwrap();
    std::fs::write(archive.join("Tools/FS25_Dir/modDesc.xml"), b"<modDesc/>").unwrap();
    let layout = Layout {
        mods: mods.clone(),
        archive: archive.clone(),
    };
    let link = mods.join("FS25_Dir");
    make_link(
        &archive.join("Tools/FS25_Dir"),
        &link,
        "dir",
        &mut LinkPolicy::new(&layout),
    )
    .unwrap();
    let meta = link.symlink_metadata().unwrap();
    assert!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "a real folder"
    );
    assert!(link.join(SILO_MARKER).is_file(), "marked as Silo's copy");
    assert!(link.join("modDesc.xml").is_file());
    // Never merges into an existing folder.
    assert!(make_link(
        &archive.join("Tools/FS25_Dir"),
        &link,
        "dir",
        &mut LinkPolicy::new(&layout)
    )
    .is_err());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn mac_app_store_containers_are_detected() {
    assert!(in_mac_app_container(Path::new(
        "/Users/me/Library/Containers/com.astragon.farmingsim25/Data/Library/Application Support/FarmingSimulator2025/mods"
    )));
    // APFS is case-insensitive by default.
    assert!(in_mac_app_container(Path::new(
        "/Users/me/library/CONTAINERS/com.astragon.farmingsim25/Data/mods"
    )));
    assert!(!in_mac_app_container(Path::new(
        "/Users/me/Library/Application Support/FarmingSimulator2025/mods"
    )));
    assert!(!in_mac_app_container(Path::new(
        r"C:\Users\me\Documents\My Games\FarmingSimulator2025\mods"
    )));
}

/// Same drive, a working hardlink: a new link is a hardlink, never a copy. Same drive where
/// the mods folder rejects hardlinks, or a drive Silo can't identify: it might be a copy.
/// Another drive with symlinks: a symlink.
#[test]
fn can_only_copy_unless_a_link_is_known_to_work() {
    let layout = Layout {
        mods: PathBuf::from("/m"),
        archive: PathBuf::from("/a"),
    };
    let d = |cross, sym, hard| LinkPolicy::decided_for_tests(&layout, cross, sym, hard);
    assert!(!d(false, false, true).can_only_copy());
    assert!(d(false, false, false).can_only_copy());
    assert!(!d(true, true, false).can_only_copy());
    assert!(d(true, false, false).can_only_copy());
}

/// A symlink at `flat` into a drive that's gone. `None` where file symlinks can't be made
/// (Windows without Developer Mode) — the test is skipped there.
fn dangling_link(base: &Path, flat: &Path) -> Option<()> {
    symlink_file(&base.join("unplugged").join("FS25_Foo.zip"), flat).ok()
}

#[test]
fn a_dangling_link_at_a_mods_name_is_stale_and_never_blocks_activation() {
    let (base, conn, src, flat) = symlink_fixture("dangle", false);
    let layout = Layout::default_for(&base);
    let key = crate::modsroot::canonical_key(&base);
    let foo: HashSet<String> = ["FS25_Foo".to_string()].into_iter().collect();
    if dangling_link(&base, &flat).is_none() {
        return;
    }
    // Activate: the stale link is removed and a working projection made.
    let rep = set_active(&conn, &layout, &foo);
    assert_eq!(rep.changed, 1, "errors: {:?}", rep.errors);
    assert_eq!(
        std::fs::read(&flat).unwrap(),
        b"PK\x03\x04 the archived build"
    );
    assert!(db::load_organized(&conn, &key)[0].active);
    // Deactivate over a dangling link: removed, and the manifest says inactive.
    std::fs::remove_file(&flat).unwrap();
    dangling_link(&base, &flat).unwrap();
    let rep = set_active(&conn, &layout, &HashSet::new());
    assert_eq!(rep.changed, 1, "errors: {:?}", rep.errors);
    assert!(flat.symlink_metadata().is_err());
    assert!(!db::load_organized(&conn, &key)[0].active);
    // Flatten over a dangling link: the archived file comes home.
    dangling_link(&base, &flat).unwrap();
    let rep = flatten(&conn, &layout);
    assert_eq!(rep.changed, 1, "errors: {:?}", rep.errors);
    assert!(flat.symlink_metadata().unwrap().file_type().is_file());
    assert!(!src.exists());
    let _ = std::fs::remove_dir_all(&base);
}

/// A dangling dir junction (no privilege needed, unlike symlinks): link, then delete the
/// target. False if `mklink` failed.
#[cfg(windows)]
fn dangling_junction(link: &Path, gone: &Path) -> bool {
    std::fs::create_dir_all(gone).unwrap();
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(gone)
        .output();
    std::fs::remove_dir_all(gone).unwrap();
    match out {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!(
                "mklink: {}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            false
        }
        Err(e) => {
            eprintln!("mklink: {e}");
            false
        }
    }
}

#[cfg(windows)]
#[test]
fn a_dangling_junction_at_a_folder_mods_name_never_blocks_activation() {
    let base = std::env::temp_dir().join(format!("silo_junction_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cat = base.join(ARCHIVE).join("Maps");
    std::fs::create_dir_all(cat.join("FS25_Map")).unwrap();
    std::fs::write(cat.join("FS25_Map/modDesc.xml"), b"<modDesc/>").unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let key = crate::modsroot::canonical_key(&base);
    db::upsert_organized(
        &conn,
        &key,
        &OrganizedRow {
            tech_name: "FS25_Map".into(),
            file_name: "FS25_Map".into(),
            kind: "dir".into(),
            category: "Maps".into(),
            subcategory: None,
            active: true,
        },
    )
    .unwrap();
    let flat = base.join("FS25_Map");
    // Junctions need no privilege on NTFS, so this always runs on Windows.
    let gone = base.join("unplugged").join("FS25_Map");
    assert!(dangling_junction(&flat, &gone), "mklink /J failed");
    let layout = Layout::default_for(&base);
    // Park: the stale junction goes, the manifest says parked.
    let rep = set_active(&conn, &layout, &HashSet::new());
    assert_eq!(rep.changed, 1, "errors: {:?}", rep.errors);
    assert!(flat.symlink_metadata().is_err());
    assert!(!db::load_organized(&conn, &key)[0].active);
    // Activate over another stale junction: a working projection replaces it.
    assert!(dangling_junction(&flat, &gone));
    let want: HashSet<String> = ["FS25_Map".to_string()].into_iter().collect();
    let rep = set_active(&conn, &layout, &want);
    assert_eq!(rep.changed, 1, "errors: {:?}", rep.errors);
    assert!(flat.join("modDesc.xml").is_file());
    assert!(
        cat.join("FS25_Map/modDesc.xml").is_file(),
        "the archived mod is untouched"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(windows)]
#[test]
fn a_folder_mod_is_projected_as_a_link_not_a_copy_on_windows() {
    // Junctions need no privilege on NTFS, so this must never fall back to a copy.
    let base = std::env::temp_dir().join(format!("silo_junction_proj_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mods = base.join("mods");
    let archive_mod = mods.join(ARCHIVE).join("Misc").join("FS25_Dir");
    std::fs::create_dir_all(&archive_mod).unwrap();
    std::fs::write(archive_mod.join("modDesc.xml"), b"<modDesc/>").unwrap();
    let link = mods.join("FS25_Dir");
    let layout = Layout::default_for(&mods);

    make_link(&archive_mod, &link, "dir", &mut LinkPolicy::new(&layout)).unwrap();

    let ft = link.symlink_metadata().unwrap().file_type();
    assert!(
        ft.is_symlink(),
        "expected a junction or symlink, got a real folder"
    );
    assert!(
        !link.join(SILO_MARKER).exists(),
        "must not be a marked copy"
    );
    assert!(is_silo_projection(&link, &archive_mod, "dir"));
    // An edit made through the link lands in the archived mod.
    std::fs::write(link.join("edit.txt"), b"hello").unwrap();
    assert!(archive_mod.join("edit.txt").is_file());
    // Removing the link never touches the archived mod.
    remove_link(&link).unwrap();
    assert!(link.symlink_metadata().is_err());
    assert!(archive_mod.join("modDesc.xml").is_file());
    assert!(archive_mod.join("edit.txt").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

/// The junction branch on its own: this machine may have Developer Mode (so `symlink_dir`
/// wins in `make_link`), but users and the CI runner usually don't.
#[cfg(windows)]
#[test]
fn junction_to_links_a_folder_without_privilege() {
    let base = std::env::temp_dir().join(format!("silo_junction_to_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let target = base.join(ARCHIVE).join("Misc").join("FS25_Dir");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("modDesc.xml"), b"<modDesc/>").unwrap();
    let link = base.join("FS25_Dir");

    junction_to(&target, &link).unwrap();

    assert!(junction::exists(&link).unwrap(), "a real NTFS junction");
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert!(is_silo_projection(&link, &target, "dir"));
    assert_eq!(
        std::fs::read(link.join("modDesc.xml")).unwrap(),
        b"<modDesc/>"
    );
    remove_link(&link).unwrap();
    assert!(link.symlink_metadata().is_err());
    assert!(
        target.join("modDesc.xml").is_file(),
        "the target is untouched"
    );

    // Never replaces or merges into something already at the link path.
    let theirs = base.join("FS25_Theirs");
    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::write(theirs.join("mine.txt"), b"keep").unwrap();
    let err = junction_to(&target, &theirs).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(theirs.join("mine.txt")).unwrap(), b"keep");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn remove_dangling_link_never_touches_a_file_or_a_working_link() {
    let (base, _conn, src, flat) = symlink_fixture("notdangle", true);
    std::fs::write(&flat, b"the user's own build").unwrap();
    assert!(!remove_dangling_link(&flat).unwrap());
    assert_eq!(std::fs::read(&flat).unwrap(), b"the user's own build");
    std::fs::remove_file(&flat).unwrap();
    if symlink_file(&src, &flat).is_ok() {
        assert!(!remove_dangling_link(&flat).unwrap());
        assert!(flat.exists(), "a working link stays");
    }
    assert!(!remove_dangling_link(&base.join("nothing-here")).unwrap());
    let _ = std::fs::remove_dir_all(&base);
}

/// A default-layout mods folder holding one organized zip `FS25_Foo.zip` in `Vehicles`.
fn symlink_fixture(tag: &str, active: bool) -> (PathBuf, Connection, PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("silo_sym_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let conn = db::open(&base.join("silo.db")).unwrap();
    let key = crate::modsroot::canonical_key(&base);
    let cat = base.join(ARCHIVE).join("Vehicles");
    std::fs::create_dir_all(&cat).unwrap();
    let src = cat.join("FS25_Foo.zip");
    std::fs::write(&src, b"PK\x03\x04 the archived build").unwrap();
    db::upsert_organized(
        &conn,
        &key,
        &OrganizedRow {
            tech_name: "FS25_Foo".into(),
            file_name: "FS25_Foo.zip".into(),
            kind: "zip".into(),
            category: "Vehicles".into(),
            subcategory: None,
            active,
        },
    )
    .unwrap();
    let flat = base.join("FS25_Foo.zip");
    (base, conn, src, flat)
}

#[cfg(unix)]
#[test]
fn deactivate_removes_a_silo_symlink_and_keeps_the_target() {
    let (base, conn, src, flat) = symlink_fixture("deact", true);
    std::os::unix::fs::symlink(&src, &flat).unwrap();
    let rep = set_active(&conn, &Layout::default_for(&base), &HashSet::new());
    assert_eq!(rep.changed, 1, "errors: {:?}", rep.errors);
    assert!(flat.symlink_metadata().is_err(), "the symlink is gone");
    assert_eq!(
        std::fs::read(&src).unwrap(),
        b"PK\x03\x04 the archived build"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn deactivate_removes_a_silo_folder_symlink_and_keeps_the_folder() {
    // rmdir on a symlink fails on Unix (ENOTDIR), so a folder link must be unlinked.
    let (base, conn, _, _) = symlink_fixture("deactdir", false);
    let src = base.join(ARCHIVE).join("Vehicles").join("FS25_Dir");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
    let key = crate::modsroot::canonical_key(&base);
    db::upsert_organized(
        &conn,
        &key,
        &OrganizedRow {
            tech_name: "FS25_Dir".into(),
            file_name: "FS25_Dir".into(),
            kind: "dir".into(),
            category: "Vehicles".into(),
            subcategory: None,
            active: true,
        },
    )
    .unwrap();
    let link = base.join("FS25_Dir");
    std::os::unix::fs::symlink(&src, &link).unwrap();
    let rep = set_active(&conn, &Layout::default_for(&base), &HashSet::new());
    assert_eq!(rep.changed, 1, "errors: {:?}", rep.errors);
    assert!(link.symlink_metadata().is_err(), "the folder link is gone");
    assert!(
        src.join("modDesc.xml").is_file(),
        "the archived folder stays"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn flatten_replaces_a_silo_symlink_with_the_archived_file() {
    let (base, conn, src, flat) = symlink_fixture("flat", true);
    std::os::unix::fs::symlink(&src, &flat).unwrap();
    let rep = flatten(&conn, &Layout::default_for(&base));
    assert_eq!(rep.changed, 1, "errors: {:?}", rep.errors);
    let meta = flat.symlink_metadata().unwrap();
    assert!(meta.file_type().is_file(), "a real file, not a link");
    assert_eq!(
        std::fs::read(&flat).unwrap(),
        b"PK\x03\x04 the archived build"
    );
    assert!(!src.exists(), "moved back out of the archive");
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn relink_across_drives_upgrades_a_copy_to_a_symlink() {
    let (base, conn, src, flat) = symlink_fixture("relink", true);
    std::fs::copy(&src, &flat).unwrap();
    let layout = Layout::default_for(&base);
    let key = crate::modsroot::canonical_key(&base);
    db::record_projection(&conn, &key, "FS25_Foo.zip", 1, 1, None);

    let rep = relink_with(&conn, &layout, RelinkMode::Symlink, |_, _| {});
    assert_eq!(rep.upgraded, 1, "errors: {:?}", rep.errors);
    assert!(flat.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(&flat).unwrap(), std::fs::read(&src).unwrap());
    assert!(is_silo_projection(&flat, &src, "zip"));
    assert!(
        db::projection_stats(&conn, &key, "FS25_Foo.zip").is_none(),
        "the stale copy's provenance is forgotten"
    );

    let rep2 = relink_with(&conn, &layout, RelinkMode::Symlink, |_, _| {});
    assert_eq!(rep2.upgraded, 0);
    assert_eq!(rep2.already_linked, 1);
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn relink_skips_a_foreign_symlink() {
    let (base, conn, _src, flat) = symlink_fixture("relinkforeign", true);
    let theirs = base.join("theirs.zip");
    std::fs::write(&theirs, b"PK\x03\x04 the archived build").unwrap();
    std::os::unix::fs::symlink(&theirs, &flat).unwrap();
    let rep = relink_with(
        &conn,
        &Layout::default_for(&base),
        RelinkMode::Symlink,
        |_, _| {},
    );
    assert_eq!(rep.skipped, 1);
    assert_eq!(rep.upgraded, 0);
    assert_eq!(std::fs::read_link(&flat).unwrap(), theirs, "left untouched");
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn adopt_refuses_a_symlink_in_the_mods_folder() {
    let (base, conn, src, flat) = symlink_fixture("adoptlink", true);
    let theirs = base.join("theirs.zip");
    std::fs::write(&theirs, b"PK\x03\x04 elsewhere").unwrap();
    std::os::unix::fs::symlink(&theirs, &flat).unwrap();
    let err = adopt_foreign(&conn, &Layout::default_for(&base), "FS25_Foo.zip").unwrap_err();
    assert!(err.contains("link"), "{err}");
    assert_eq!(std::fs::read_link(&flat).unwrap(), theirs, "link untouched");
    assert_eq!(
        std::fs::read(&src).unwrap(),
        b"PK\x03\x04 the archived build"
    );
    assert!(!base.join("backups").exists(), "nothing backed up");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn network_paths_are_recognised() {
    assert!(is_network_path(r"\\server\share\mods"));
    assert!(is_network_path(r"\\?\UNC\server\share\mods"));
    assert!(is_network_path(r"\\?\unc\server\share"));
    assert!(is_network_path(r"\\.\UNC\server\share"));
    assert!(!is_network_path(r"\\?\C:\Users\me\mods"));
    assert!(!is_network_path(r"\\.\C:\Users\me\mods"));
    assert!(!is_network_path(r"C:\Users\me\mods"));
    assert!(!is_network_path("/home/me/mods"));
    assert!(!is_network_path(r"\\?\UN"));
}

/// A projection that falls back to a full copy asks the policy's room check first; a "no"
/// stops it before anything is made (#394). Inside a Mac App Store container a folder mod is
/// always a copy (a container path is a Unix path).
#[cfg(unix)]
#[test]
fn a_copy_that_wont_fit_is_refused_before_it_starts() {
    let base = temp_dir_for("noroomcopy");
    let mods = base.join("Library/Containers/fs25/mods");
    std::fs::create_dir_all(&mods).unwrap();
    let layout = Layout::default_for(&mods);
    let src = base.join("FS25_D");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
    let asked = std::cell::Cell::new(0);
    let refuse = |s: &Path, entry: &Path| -> std::io::Result<()> {
        asked.set(asked.get() + 1);
        assert_eq!(s, src.as_path());
        assert_eq!(entry, mods.join("FS25_D").as_path());
        Err(std::io::Error::other(NoRoom("no room".into())))
    };
    let mut policy = LinkPolicy::new(&layout).with_room_check(&refuse);
    let e = make_link(&src, &mods.join("FS25_D"), "dir", &mut policy).unwrap_err();
    assert_eq!(no_room(&e), Some("no room"));
    assert_eq!(asked.get(), 1);
    assert!(
        mods.join("FS25_D").symlink_metadata().is_err(),
        "nothing made"
    );
    assert_eq!(std::fs::read_dir(&mods).unwrap().count(), 0, "no temp left");
    // Room: the copy is made.
    let fine = |_: &Path, _: &Path| -> std::io::Result<()> { Ok(()) };
    let mut policy = LinkPolicy::new(&layout).with_room_check(&fine);
    make_link(&src, &mods.join("FS25_D"), "dir", &mut policy).unwrap();
    assert!(mods.join("FS25_D/modDesc.xml").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

/// The zip fallback copy asks too, and only the copy: a hardlink or symlink costs no room.
#[test]
fn only_the_zip_fallback_copy_asks_for_room() {
    let base = temp_dir_for("noroomzip");
    let src = base.join("a.zip");
    std::fs::write(&src, b"PK").unwrap();
    let refused = |_: &Path, _: &Path| Err(std::io::Error::from_raw_os_error(396));
    let no = |_: &Path, _: &Path| -> std::io::Result<()> {
        Err(std::io::Error::other(NoRoom("full".into())))
    };
    let e = link_zip_or(
        &src,
        &base.join("l.zip"),
        || false,
        refused,
        refused,
        |s, l| {
            no(s, l)?;
            copy_new(s, l)
        },
    )
    .unwrap_err();
    assert_eq!(no_room(&e), Some("full"));
    assert!(!base.join("l.zip").exists());
    link_zip_or(
        &src,
        &base.join("h.zip"),
        || false,
        |s, l| std::fs::hard_link(s, l),
        refused,
        |_, _| panic!("a hardlink needs no copy"),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&base);
}

/// The folder fallback copy asks for room; a link that works needs none.
#[test]
fn only_the_folder_fallback_copy_asks_for_room() {
    let base = temp_dir_for("noroomdir");
    let src = base.join("FS25_D");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
    let refused = |_: &Path, _: &Path| Err(std::io::Error::from_raw_os_error(1));
    let e = link_dir_or(&src, &base.join("l"), refused, |_, _| {
        Err(std::io::Error::other(NoRoom("full".into())))
    })
    .unwrap_err();
    assert_eq!(no_room(&e), Some("full"));
    assert!(base.join("l").symlink_metadata().is_err());
    let _ = std::fs::remove_dir_all(&base);
}
