//! Adopt / restore a file the user swapped into the mods folder, and the `backups/` names
//! those (and other replacing steps) keep the old version under.

use super::*;

/// Adopt the file the user swapped into the mods folder as the new canonical version: back
/// up the old archived copy, promote the flat file into the archive, and (if the mod is
/// active) re-project it so it's Silo-managed again. Reversible — the old copy is kept under
/// `backups/`. If the promote fails (a file in use, say), the old copy is moved back into the
/// archive, so the failure changes nothing.
pub fn adopt_foreign(conn: &Connection, layout: &Layout, file_name: &str) -> Result<(), String> {
    let root = &layout.mods;
    let key = crate::modsroot::canonical_key(root);
    crate::paths::safe_file_name(file_name).map_err(|e| e.to_string())?;
    let row = db::load_organized(conn, &key)
        .into_iter()
        .find(|r| r.file_name == file_name)
        .ok_or_else(|| "Not a Silo-managed mod".to_string())?;
    let flat = root.join(file_name);
    let Ok(fmeta) = flat.symlink_metadata() else {
        return Err("There's no file in your mods folder to use.".into());
    };
    // Promoting a link would file the link itself (not a mod) into the archive and back up the
    // real archived copy out of the way. Refuse before touching anything.
    if fmeta.file_type().is_symlink() {
        return Err(
            "That's a link, not a file, so Silo can't use it. Choose Keep Silo's copy instead."
                .into(),
        );
    }
    let archive = archive_path(&layout.archive, &row.category, file_name);
    // Back up the old archived copy before replacing it.
    let mut backup = None;
    if archive.exists() {
        let bak = backup_path(root, file_name)?;
        move_path_as(&archive, &bak, "backup").map_err(|e| format!("backup: {e}"))?;
        backup = Some(bak);
    } else if let Some(parent) = archive.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Promote the swapped-in file into the archive, then re-project it back into the root.
    if let Err(e) = move_path(&flat, &archive) {
        let failed = format!("promote: {e}");
        return Err(match backup {
            Some(bak) => put_backup_back(&bak, &archive, failed),
            None => failed,
        });
    }
    if row.active {
        if let Err(e) = make_link(&archive, &flat, &row.kind, &mut LinkPolicy::new(layout)) {
            // The entry the records were about is in the archive now and the name is empty.
            db::forget_projection(conn, &key, file_name);
            return Err(format!("re-project: {e}"));
        }
    }
    note_resolved(conn, &key, &row, &flat, &archive);
    Ok(())
}

/// After adopt or restore put Silo's copy at the mod's name (or left the name empty, for a
/// parked mod): record it now, with the library copy's stamp, so the rescan right after shows it
/// as Silo's without waiting for the next foreign-file check; or drop the records for an empty
/// name.
fn note_resolved(conn: &Connection, key: &str, row: &OrganizedRow, flat: &Path, archive: &Path) {
    if row.active && row.kind == "zip" {
        note_zip_projection(conn, key, &row.file_name, flat, archive);
    } else {
        db::forget_projection(conn, key, &row.file_name);
    }
}

/// Restore Silo's managed copy over a file the user swapped in: back up their file, then
/// re-project the archived original (if the mod is active). Reversible — their file is kept
/// under `backups/`. If the re-project fails, their file is moved back into the mods folder,
/// so the failure changes nothing.
pub fn restore_projection(
    conn: &Connection,
    layout: &Layout,
    file_name: &str,
) -> Result<(), String> {
    let mut policy = LinkPolicy::new(layout);
    restore_projection_with(conn, layout, file_name, |src, link, kind| {
        make_link(src, link, kind, &mut policy)
    })
}

/// [`restore_projection`] with the re-project (`make_link`) injected, so its rollback is
/// testable.
pub(super) fn restore_projection_with(
    conn: &Connection,
    layout: &Layout,
    file_name: &str,
    relink: impl FnOnce(&Path, &Path, &str) -> std::io::Result<()>,
) -> Result<(), String> {
    let root = &layout.mods;
    let key = crate::modsroot::canonical_key(root);
    crate::paths::safe_file_name(file_name).map_err(|e| e.to_string())?;
    let row = db::load_organized(conn, &key)
        .into_iter()
        .find(|r| r.file_name == file_name)
        .ok_or_else(|| "Not a Silo-managed mod".to_string())?;
    let archive = archive_path(&layout.archive, &row.category, file_name);
    if !archive.exists() {
        return Err("There's no copy in your library archive to put back.".into());
    }
    let flat = root.join(file_name);
    // Back up the user's file, then free the slot.
    let mut backup = None;
    if flat.symlink_metadata().is_ok() {
        let bak = backup_path(root, file_name)?;
        move_path_as(&flat, &bak, "backup").map_err(|e| format!("backup: {e}"))?;
        backup = Some(bak);
    }
    if row.active {
        if let Err(e) = relink(&archive, &flat, &row.kind) {
            let failed = format!("re-project: {e}");
            return Err(match backup {
                Some(bak) => put_backup_back(&bak, &flat, failed),
                None => failed,
            });
        }
    }
    note_resolved(conn, &key, &row, &flat, &archive);
    Ok(())
}

/// Undo a backup move after the step that followed it failed with `failed`: move `bak` back to
/// `to`, its old place, so the failure changes nothing. Only into a free `to` (never over what
/// the failed step may have left there). Returns the error to report: `failed`, plus where the
/// backup is if it couldn't be moved back.
fn put_backup_back(bak: &Path, to: &Path, failed: String) -> String {
    let undone = if to.symlink_metadata().is_ok() {
        Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
    } else {
        move_path_as(bak, to, "restore")
    };
    match undone {
        Ok(()) => failed,
        Err(e) => format!(
            "{failed}. Silo couldn't move the backup back ({e}); it's kept at {}.",
            bak.display()
        ),
    }
}

/// A free `<root>/backups/<file_name>.<unix_secs>.bak` for a file we're about to replace.
fn backup_path(root: &Path, file_name: &str) -> Result<PathBuf, String> {
    free_backup_path(root, file_name, ".bak").map_err(|e| e.to_string())
}

/// Suffix of the previous version an update keeps in `backups/`. Its own suffix so pruning old
/// update backups can never touch a file the user's adopt/restore kept there.
pub(crate) const UPDATE_BACKUP_SUFFIX: &str = ".update.bak";

/// A free `<root>/backups/<file_name>.<unix_secs>.update.bak` for the version an update replaced.
pub(crate) fn update_backup_path(root: &Path, file_name: &str) -> std::io::Result<PathBuf> {
    free_backup_path(root, file_name, UPDATE_BACKUP_SUFFIX)
}

/// A free `<root>/backups/<name>.<unix_secs>` folder path for a folder copy we're keeping
/// (`-2`, `-3`, … appended if that name is taken, so an earlier backup is never replaced).
pub(super) fn backup_dir_path(root: &Path, name: &str) -> std::io::Result<PathBuf> {
    free_backup_path(root, name, "")
}

fn free_backup_path(root: &Path, name: &str, suffix: &str) -> std::io::Result<PathBuf> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    backup_path_at(root, name, secs, suffix)
}

/// A free `<root>/backups/<name>.<secs>[-n]<suffix>` (creates `<root>/backups/`, not the entry).
pub(super) fn backup_path_at(
    root: &Path,
    name: &str,
    secs: u64,
    suffix: &str,
) -> std::io::Result<PathBuf> {
    let dir = root.join("backups");
    std::fs::create_dir_all(&dir)?;
    let mut candidate = dir.join(format!("{name}.{secs}{suffix}"));
    let mut n = 2;
    while candidate.symlink_metadata().is_ok() {
        candidate = dir.join(format!("{name}.{secs}-{n}{suffix}"));
        n += 1;
    }
    Ok(candidate)
}

/// What the user is told when a changed folder copy was moved aside instead of deleted.
/// `restored`: the archived original is back in the mods folder (flatten); otherwise the mod
/// was parked and its slot is simply empty.
pub(super) fn kept_copy_notice(name: &str, kept: &Path, restored: bool) -> String {
    if restored {
        format!(
            "{name} had changed since Silo copied it, so it was moved to {} instead of being deleted. Your mods folder now has Silo's original.",
            kept.display()
        )
    } else {
        format!(
            "{name} had changed since Silo copied it, so it was kept in {} instead of being deleted.",
            kept.display()
        )
    }
}
