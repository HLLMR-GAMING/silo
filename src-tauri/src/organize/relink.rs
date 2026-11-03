//! Relink: collapse copy-projections into links, and atomically re-point a zip's mods-folder
//! entry at a new archived file.

use super::*;

/// Repair copy-projections into hardlinks. A projection that was written as a byte-for-byte
/// **copy** (rather than a hardlink — e.g. the flat file pre-existed so `set_active` never
/// linked it) wastes the mod's bytes twice and, worse, forces every later activate/deactivate
/// to `sha256` the whole file to prove it's Silo's. Collapsing each such copy into a hardlink
/// to its archived twin reclaims the space and makes identity checks O(1) forever after.
///
/// When the archive is on another drive (no hardlinks possible), copies are upgraded to
/// symlinks instead, where the OS allows them.
///
/// Only ever touches a flat file that is **byte-identical** to the archive copy — never the
/// user's own replacement build. Crash-safe: hardlink (or symlink) to a temp sibling,
/// then atomically rename over the flat file (both the old copy and the new hardlink hold the
/// same bytes, so an interruption at any point leaves a valid file). `progress(done, total)`
/// is called once per active zip examined so a long pass can show a bar instead of a freeze.
pub fn relink_projections(
    conn: &Connection,
    layout: &Layout,
    progress: impl FnMut(usize, usize),
) -> RelinkReport {
    relink_with(conn, layout, relink_mode(layout), progress)
}

/// How a relink pass may link here. Same volume (`same_volume_for_links`: on a mount the disk
/// list doesn't know, where a probe hardlink works) → hardlinks, unless the folder is
/// cloud-synced (every attempt would error 396 — copies stay). Archive on another drive →
/// symlinks, when the OS allows them.
fn relink_mode(layout: &Layout) -> RelinkMode {
    if same_volume_for_links(layout) {
        if supports_hardlinks(&layout.mods) {
            RelinkMode::Hardlink
        } else {
            RelinkMode::SameVolumeNoHardlinks
        }
    } else if symlinks_usable(layout) {
        RelinkMode::Symlink
    } else {
        RelinkMode::CrossDriveNoSymlinks
    }
}

/// How [`relink_projections`] may link — decided once up front, injectable for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RelinkMode {
    Hardlink,
    Symlink,
    /// Same volume, but hardlinks are rejected (cloud-synced folder).
    SameVolumeNoHardlinks,
    /// Archive on another drive and symlinks unavailable.
    CrossDriveNoSymlinks,
}

pub(super) fn relink_with(
    conn: &Connection,
    layout: &Layout,
    mode: RelinkMode,
    mut progress: impl FnMut(usize, usize),
) -> RelinkReport {
    let root = &layout.mods;
    let key = crate::modsroot::canonical_key(root);
    let mut rep = RelinkReport::default();
    // Relink temps an earlier relink or move left behind (a crash): only recorded ones, each
    // proven redundant first. A move's own scratch is that move's to sweep.
    let journal = crate::scratch::DbJournal(conn);
    crate::scratch::sweep_where(&journal, &|e| e.role == crate::scratch::Role::RelinkTmp);
    // Nothing can be linked: report the situation cleanly instead of one failure per mod.
    let hardlinks = match mode {
        RelinkMode::Hardlink => true,
        RelinkMode::Symlink => false,
        RelinkMode::SameVolumeNoHardlinks => {
            rep.hardlinks_unsupported = true;
            return rep;
        }
        RelinkMode::CrossDriveNoSymlinks => {
            rep.cross_drive_copies = true;
            return rep;
        }
    };
    // Only active zips can be projected as a plain file; dir projections are marker-identified
    // and cheap already, so they never need repair.
    let rows: Vec<OrganizedRow> = db::load_organized(conn, &key)
        .into_iter()
        .filter(|r| r.active && r.kind == "zip")
        .collect();
    let total = rows.len();
    for (i, row) in rows.into_iter().enumerate() {
        progress(i, total);
        let flat = root.join(&row.file_name);
        let src = archive_path(&layout.archive, &row.category, &row.file_name);
        let Ok(fmeta) = flat.symlink_metadata() else {
            rep.skipped += 1;
            continue;
        };
        if !src.exists() {
            rep.skipped += 1;
            continue;
        }
        // A symlink resolving to the archive is already a link (a symlink pass across drives).
        if fmeta.file_type().is_symlink() {
            if is_silo_projection(&flat, &src, "zip") {
                rep.already_linked += 1;
            } else {
                rep.skipped += 1;
            }
            continue;
        }
        // Already sharing the archive's file id → a hardlink, nothing to reclaim.
        if let (Ok(a), Ok(b)) = (
            same_file::Handle::from_path(&flat),
            same_file::Handle::from_path(&src),
        ) {
            if a == b {
                rep.already_linked += 1;
                continue;
            }
        }
        // A copy is traded for a link only to a real archived zip: one that is itself a link
        // (to a file elsewhere) isn't a copy Silo can stand behind (#399).
        if !crate::scratch::real_copy(&src, "zip") {
            rep.skipped += 1;
            continue;
        }
        match same_bytes(&flat, &src) {
            Ok(true) => match relink_over(&src, &flat, hardlinks, Some(&journal)) {
                Ok(()) => {
                    rep.upgraded += 1;
                    rep.reclaimed_bytes += fmeta.len();
                    // The copy's recorded (size, mtime) no longer describes the flat entry. A
                    // hardlink gets its own fresh record (fast deactivate); a symlink is
                    // identified structurally and keeps none.
                    db::forget_projection(conn, &key, &row.file_name);
                    if hardlinks {
                        if let Some((size, mtime)) = file_stats(&flat) {
                            db::record_projection(
                                conn,
                                &key,
                                &row.file_name,
                                size,
                                mtime,
                                file_stats(&src),
                            );
                        }
                    }
                }
                Err(e) => rep.errors.push(format!("{}: relink: {e}", row.tech_name)),
            },
            // Distinct bytes → the user swapped in their own build; leave it untouched.
            Ok(false) => rep.skipped += 1,
            Err(e) => rep.errors.push(format!("{}: compare: {e}", row.tech_name)),
        }
    }
    progress(total, total);
    rep
}

/// Byte-equal? Cheap size gate first, then a content hash of each.
fn same_bytes(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (ma, mb) = (std::fs::metadata(a)?, std::fs::metadata(b)?);
    if ma.len() != mb.len() {
        return Ok(false);
    }
    Ok(
        match (
            crate::provenance::sha256_file(a),
            crate::provenance::sha256_file(b),
        ) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        },
    )
}

/// Atomically replace `flat` with a hardlink (`hard`) or symlink to `src`. Link a temp sibling
/// in the same directory (a failure leaves `flat` untouched), then rename over `flat`. The temp
/// has a name of its own, recorded in `journal` first (see [`begin_relink_tmp`]).
fn relink_over(
    src: &Path,
    flat: &Path,
    hard: bool,
    journal: Option<&dyn crate::scratch::Journal>,
) -> std::io::Result<()> {
    let tmp = begin_relink_tmp(journal, flat, src, "zip")?;
    let made = if hard {
        std::fs::hard_link(src, &tmp)
    } else {
        symlink_file(src, &tmp)
    };
    if let Err(e) = made {
        abandon_relink_tmp(journal, &tmp);
        return Err(e);
    }
    match std::fs::rename(&tmp, flat) {
        Ok(()) => {
            settle_relink_tmp(journal, &tmp);
            crate::fslog::moved("link", src, flat);
            Ok(())
        }
        Err(e) => {
            discard_relink_tmp(journal, &tmp, src, "zip");
            Err(e)
        }
    }
}

/// Suffix of a relink temp in the mods folder. `scan` skips it.
pub(crate) const RELINK_SUFFIX: &str = crate::scratch::RELINK_TMP;

/// A new relink temp beside `link` (`<name>.<token>.silo-relink-tmp`), recorded in `journal`
/// (when there is one) before anything is made there, so a temp a crash leaves behind can be
/// proven Silo's and swept (`scratch::sweep_where`). Never removed by name: the name is new, and
/// every way it's made (`hard_link`, a symlink, a junction, `copy_new`) refuses to replace
/// anything. A journal that can't record stops the relink before it starts.
fn begin_relink_tmp(
    journal: Option<&dyn crate::scratch::Journal>,
    link: &Path,
    from: &Path,
    kind: &str,
) -> std::io::Result<PathBuf> {
    use crate::scratch::{begin, unique_beside, Role, RELINK_TMP};
    match journal {
        Some(j) => begin(j, Role::RelinkTmp, link, RELINK_TMP, from, link, kind)
            .map_err(std::io::Error::other),
        None => Ok(unique_beside(link, RELINK_TMP)),
    }
}

/// Making the temp failed: nothing at it is Silo's, so the record goes and nothing is removed.
fn abandon_relink_tmp(journal: Option<&dyn crate::scratch::Journal>, tmp: &Path) {
    if let Some(j) = journal {
        j.forget(tmp);
    }
}

/// The temp was renamed into place (or is otherwise gone): drop its record.
fn settle_relink_tmp(journal: Option<&dyn crate::scratch::Journal>, tmp: &Path) {
    if let Some(j) = journal {
        crate::scratch::settle(j, tmp);
    }
}

/// Remove a temp Silo just made (a link to `from`, or a copy of it), with the same proof a
/// sweep needs (`scratch::discard`): a link holds no data; a copy goes only while `from` still
/// holds every byte of it. Anything else is kept, and stays recorded for a later sweep.
fn discard_relink_tmp(
    journal: Option<&dyn crate::scratch::Journal>,
    tmp: &Path,
    from: &Path,
    kind: &str,
) {
    let e = crate::scratch::Entry {
        path: tmp.to_path_buf(),
        role: crate::scratch::Role::RelinkTmp,
        from: from.to_path_buf(),
        to: tmp.to_path_buf(),
        kind: kind.to_string(),
        made: None,
    };
    crate::scratch::discard(journal, &e);
}

/// Atomically swap a zip's mods-folder entry `link` for a new link to `dst` (hardlink →
/// symlink → copy, per `policy`): make the link at a new temp sibling (`begin_relink_tmp`,
/// recorded in the policy's journal), then rename it over `link`. At every point `link` is
/// either the old working entry or the new one; a crash leaves at most the recorded temp, which
/// `scratch::sweep_where` removes once it's proven a link or a copy of `dst` (and `scan` skips).
pub(crate) fn replace_zip_link(
    link: &Path,
    dst: &Path,
    policy: &mut LinkPolicy,
) -> std::io::Result<()> {
    let journal = policy.journal();
    let tmp = begin_relink_tmp(journal, link, dst, "zip")?;
    if let Err(e) = make_link(dst, &tmp, "zip", policy) {
        abandon_relink_tmp(journal, &tmp);
        return Err(e);
    }
    // A new entry that reaches no file (the copy went while it was linked) never replaces the
    // working one.
    if !std::fs::metadata(&tmp).is_ok_and(|m| m.is_file()) {
        discard_relink_tmp(journal, &tmp, dst, "zip");
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the new copy of the mod isn't there",
        ));
    }
    if let Err(e) = std::fs::rename(&tmp, link) {
        discard_relink_tmp(journal, &tmp, dst, "zip");
        return Err(e);
    }
    settle_relink_tmp(journal, &tmp);
    crate::fslog::moved("move", &tmp, link);
    Ok(())
}

/// Re-point a folder mod's mods-folder link `link` at `dst` (#394): make the new link (or a
/// marked copy, per `make_link`) at a new temp sibling, check it reaches a folder, then rename
/// it over `link`. Where the OS renames a link over a link (Unix symlinks) that's atomic: `link`
/// is always the old link or the new one. Where it won't (Windows can't rename over a folder
/// link or junction, and a marked copy is a real folder), the old link is removed first and the
/// temp renamed into its place, as before: a crash in between leaves the mod's link missing
/// while both copies are kept, and resuming the move makes it again (a missing entry is
/// re-linked before the old copy may go). If the new entry can't be placed, the old link is put
/// back (when `old` is still there). Only a link is ever removed at `link` (`remove_link`,
/// which also accepts Silo's untouched marked copy).
pub(crate) fn replace_dir_link(
    link: &Path,
    dst: &Path,
    old: &Path,
    policy: &mut LinkPolicy,
) -> std::io::Result<()> {
    let journal = policy.journal();
    let tmp = begin_relink_tmp(journal, link, dst, "dir")?;
    if let Err(e) = make_link(dst, &tmp, "dir", policy) {
        abandon_relink_tmp(journal, &tmp);
        return Err(e);
    }
    if !std::fs::metadata(&tmp).is_ok_and(|m| m.is_dir()) {
        discard_relink_tmp(journal, &tmp, dst, "dir");
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the new copy of the mod isn't there",
        ));
    }
    let is_link = |p: &Path| {
        p.symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
    };
    let link_is_link = is_link(link);
    // Renaming never follows either link: it replaces `link` itself, or fails.
    if is_link(&tmp) && link_is_link && rename_dir_link(&tmp, link).is_ok() {
        settle_relink_tmp(journal, &tmp);
        crate::fslog::moved("link", dst, link);
        return Ok(());
    }
    // A dangling dir link can't report itself as a dir on Windows, so `remove_link` may pick
    // `remove_file`; `remove_dir` then removes the link itself (it is a link, never a folder).
    let unlinked = remove_link(link).or_else(|e| {
        if !link_is_link {
            return Err(e);
        }
        std::fs::remove_dir(link)?;
        crate::fslog::changed("unlink", link);
        Ok(())
    });
    if let Err(e) = unlinked {
        discard_relink_tmp(journal, &tmp, dst, "dir");
        return Err(e);
    }
    match crate::paths::rename_no_replace(&tmp, link) {
        Ok(()) => {
            settle_relink_tmp(journal, &tmp);
            crate::fslog::moved("link", dst, link);
            Ok(())
        }
        Err(e) => {
            discard_relink_tmp(journal, &tmp, dst, "dir");
            if old.exists() {
                let _ = make_link(old, link, "dir", policy);
            }
            Err(e)
        }
    }
}

/// Rename the folder link `tmp` over the folder link `link`, replacing it in one step. Unix:
/// `rename` replaces a symlink atomically. Windows won't replace a folder link (or junction)
/// by rename, so it isn't tried there: `replace_dir_link` falls back.
fn rename_dir_link(tmp: &Path, link: &Path) -> std::io::Result<()> {
    if cfg!(unix) {
        std::fs::rename(tmp, link)
    } else {
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// `replace_zip_link`, but only for a real link: a hardlink to `dst`, else a symlink where
/// `policy` allows one — never a copy. `Ok(false)` when neither could be made: `link` is left
/// exactly as it was (for a move, the mod's hardlink then stays as its standalone copy instead
/// of being traded for a second full copy). A `dst` that isn't there (any more) is an error,
/// never a link to nothing: like `link_zip`, a `NotFound` or `AlreadyExists` from the hardlink
/// stops here, and the new entry must reach a file before it replaces `link` and again after,
/// so a caller never deletes the old copy behind a dangling link.
pub(crate) fn replace_zip_link_if_linkable(
    link: &Path,
    dst: &Path,
    policy: &mut LinkPolicy,
) -> std::io::Result<bool> {
    let journal = policy.journal();
    replace_zip_link_if_linkable_with(
        link,
        dst,
        journal,
        || policy.allow_symlink(),
        |s, l| std::fs::hard_link(s, l),
        symlink_file,
    )
}

/// `replace_zip_link_if_linkable` with the journal and link strategies injected (tests).
pub(super) fn replace_zip_link_if_linkable_with(
    link: &Path,
    dst: &Path,
    journal: Option<&dyn crate::scratch::Journal>,
    allow_symlink: impl FnOnce() -> bool,
    hard: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    soft: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<bool> {
    use std::io::ErrorKind::{AlreadyExists, NotFound};
    let reaches_a_file = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.is_file());
    let gone = || std::io::Error::new(NotFound, "the new copy of the mod isn't there");
    if !reaches_a_file(dst) {
        return Err(gone());
    }
    let tmp = begin_relink_tmp(journal, link, dst, "zip")?;
    let linked = match hard(dst, &tmp) {
        Ok(()) => Ok(true),
        Err(e) if matches!(e.kind(), AlreadyExists | NotFound) => Err(e),
        Err(_) if allow_symlink() => match soft(dst, &tmp) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == AlreadyExists => Err(e),
            Err(_) => Ok(false),
        },
        Err(_) => Ok(false),
    };
    match linked {
        Ok(true) => {}
        other => {
            abandon_relink_tmp(journal, &tmp);
            return other;
        }
    }
    if !reaches_a_file(&tmp) {
        discard_relink_tmp(journal, &tmp, dst, "zip");
        return Err(gone());
    }
    if let Err(e) = std::fs::rename(&tmp, link) {
        discard_relink_tmp(journal, &tmp, dst, "zip");
        return Err(e);
    }
    settle_relink_tmp(journal, &tmp);
    crate::fslog::moved("link", dst, link);
    if !reaches_a_file(link) {
        return Err(gone());
    }
    Ok(true)
}

/// A zip's mods-folder entry `link` was just (re)made by Silo from archived `src`: refresh its
/// projection record (with `src`'s stamp, so a library copy changed later is noticed) so later
/// deactivates prove it's Silo's without hashing. A symlink is identified structurally and keeps
/// none.
pub(crate) fn note_zip_projection(
    conn: &Connection,
    key: &str,
    file_name: &str,
    link: &Path,
    src: &Path,
) {
    db::forget_projection(conn, key, file_name);
    if let Some((size, mtime)) = file_stats(link) {
        db::record_projection(conn, key, file_name, size, mtime, file_stats(src));
    }
}
