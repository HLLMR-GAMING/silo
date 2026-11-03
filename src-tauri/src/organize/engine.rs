//! The layout engine: organize (file mods into the archive), activate (`set_active`), and
//! flatten back to a vanilla mods folder, with the archive cleanup they share.

use super::*;

/// Dry run: which mods in `source` (the flat mods folder, or the archive's top level when
/// adopting) would move into `<archive>/<Category>/`.
pub fn plan_organize(source: &Path, mods: &[ModInput]) -> Vec<PlannedMove> {
    let mut plan = Vec::new();
    for m in mods {
        let from = source.join(&m.file_name);
        // Only plan mods that are actually sitting in the source folder right now.
        if !from.exists() {
            continue;
        }
        plan.push(PlannedMove {
            tech_name: m.tech_name.clone(),
            file_name: m.file_name.clone(),
            category: m.category.clone(),
            rel_from: m.file_name.clone(),
            rel_to: format!("{ARCHIVE}/{}/{}", sanitize(&m.category), m.file_name),
        });
    }
    plan
}

/// Move mods from `source` (the flat mods folder, or the archive's top level when adopting)
/// into `<archive>/<Category>/` and record the manifest. Mods already recorded (organized)
/// are skipped.
pub fn apply_organize(
    conn: &Connection,
    layout: &Layout,
    source: &Path,
    mods: &[ModInput],
) -> Report {
    let root = &layout.mods;
    let key = crate::modsroot::canonical_key(root);
    let mut rep = Report::default();
    let already: HashSet<String> = db::load_organized(conn, &key)
        .into_iter()
        .map(|r| r.tech_name)
        .collect();

    for m in mods {
        if already.contains(&m.tech_name) {
            rep.skipped += 1;
            continue;
        }
        // file_name drives every join below — it must be a plain basename, never a path.
        if let Err(e) = crate::paths::safe_file_name(&m.file_name) {
            rep.errors.push(format!("{}: {e}", m.tech_name));
            continue;
        }
        let from = source.join(&m.file_name);
        // Gone since the caller listed it (deleted or moved after the preview): say so, or the
        // result reads "Filed 0 mods" with no reason (#427). Only a confirmed absence says "gone".
        match from.try_exists() {
            Ok(true) => {}
            Ok(false) => {
                rep.errors.push(format!(
                    "{}: {} is no longer there, so Silo didn't file it.",
                    m.tech_name, m.file_name
                ));
                rep.skipped += 1;
                continue;
            }
            Err(e) => {
                rep.errors.push(format!(
                    "{}: can't check {} ({e}), so Silo didn't file it.",
                    m.tech_name, m.file_name
                ));
                rep.skipped += 1;
                continue;
            }
        }
        let dir = archive_dir(&layout.archive, &m.category);
        let to = dir.join(&m.file_name);
        // Adopting a folder mod named like its category would move it into itself.
        if to.starts_with(&from) {
            rep.errors.push(format!(
                "{}: {} would be filed inside itself, so Silo left it untouched.",
                m.tech_name, m.file_name
            ));
            rep.skipped += 1;
            continue;
        }
        // Never overwrite: a rename replaces an existing destination, and whatever sits there
        // (another build, the user's own file) may be the only copy.
        if to.symlink_metadata().is_ok() {
            rep.errors.push(format!(
                "{}: {} already exists in the archive, so Silo left both untouched.",
                m.tech_name, m.file_name
            ));
            rep.skipped += 1;
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            rep.errors
                .push(format!("{}: mkdir {}: {e}", m.tech_name, dir.display()));
            continue;
        }
        // Manifest-first: record the row BEFORE moving the file. If the DB write fails we
        // never move, so we can't leave a file orphaned in the archive (which flatten
        // would later be unable to tell from junk). If the move then fails, roll the row
        // back. This holds under normal errors; a HARD crash between the row write and the
        // move can still leave a row with no archived file and the original still in the
        // root. That window is made non-destructive on the read side: flatten/set_active
        // never remove a root entry unless the archived copy exists behind it, so the only
        // copy is never deleted. (A journalled, startup-reconciled operation log would close
        // the window entirely — tracked as a follow-up.)
        let row = OrganizedRow {
            tech_name: m.tech_name.clone(),
            file_name: m.file_name.clone(),
            kind: m.kind.clone(),
            category: m.category.clone(),
            subcategory: m.subcategory.clone(),
            active: false,
        };
        if let Err(e) = db::upsert_organized(conn, &key, &row) {
            rep.errors.push(format!("{}: manifest: {e}", m.tech_name));
            continue;
        }
        match move_path(&from, &to) {
            Ok(()) => {
                rep.changed += 1;
                // Keep what the scan already knows about this mod, so the rescan after filing
                // doesn't parse it again (#322). Best effort: a miss only costs a fresh parse.
                let _ = db::move_cache_row(conn, &from.to_string_lossy(), &to.to_string_lossy());
            }
            Err(e) => {
                // Roll back the row we just wrote — nothing was moved.
                let _ = db::delete_organized(conn, &key, &m.tech_name);
                rep.errors.push(format!("{}: move: {e}", m.tech_name));
            }
        }
    }
    rep
}

/// Reconcile the flat-root projection to exactly `active`: link the ones that
/// should be active, remove links for the ones that shouldn't.
pub fn set_active(conn: &Connection, layout: &Layout, active: &HashSet<String>) -> Report {
    set_active_touched(conn, layout, active, None)
}

/// [`set_active`] for a change to the mods in `touched` (the ones the user turned on or off).
/// A wanted mod outside it whose name already holds a file that isn't Silo's copy goes in
/// `standing`, not `errors`: the change didn't cause it and Diagnose → Problems lists it. `None`
/// reports it as an error, as before.
pub fn set_active_touched(
    conn: &Connection,
    layout: &Layout,
    active: &HashSet<String>,
    touched: Option<&HashSet<String>>,
) -> Report {
    let root = &layout.mods;
    let key = crate::modsroot::canonical_key(root);
    let mut rep = Report::default();
    let mut policy = LinkPolicy::new(layout);
    // The tech names of the mods in the mods folder, read once the first link is due.
    let mut in_root: Option<HashSet<String>> = None;
    for row in db::load_organized(conn, &key) {
        let link = root.join(&row.file_name);
        let src = archive_path(&layout.archive, &row.category, &row.file_name);
        let want = active.contains(&row.tech_name);
        // A link into a folder that's gone (a re-pointed archive's old drive) would otherwise
        // count as "already linked" and the mod could never be activated or parked again.
        let stale = match remove_dangling_link(&link) {
            Ok(removed) => removed,
            Err(e) => {
                rep.errors
                    .push(format!("{}: remove stale link: {e}", row.tech_name));
                continue;
            }
        };
        if stale {
            db::forget_projection(conn, &key, &row.file_name);
            if !want {
                if let Err(e) = db::set_organized_active(conn, &key, &row.tech_name, false) {
                    rep.errors.push(format!("{}: manifest: {e}", row.tech_name));
                }
                rep.changed += 1;
                continue;
            }
        }
        // Only a confirmed absence counts as "nothing there": a slot that can't be looked at may
        // still hold the projection, so it's reported and left (flag and record) as it is.
        let linked = match link.symlink_metadata() {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                rep.errors.push(format!(
                    "{}: can't check {}: {e}",
                    row.tech_name,
                    link.display()
                ));
                continue;
            }
        };

        // Nothing at the mod's own name, but another mod of its tech name is in the mods folder
        // (an unzipped folder beside this archived zip, say, #421): the game knows a mod by its
        // tech name, so the name is already on through that copy. A second copy is never linked
        // beside it, and this one stays parked. The Library shows it parked and says why.
        let other_copy = want
            && !linked
            && in_root
                .get_or_insert_with(|| crate::scan::root_tech_names(root))
                .contains(&row.tech_name);
        if other_copy {
            rep.other_copies.push(row.tech_name.clone());
            if row.active {
                match db::set_organized_active(conn, &key, &row.tech_name, false) {
                    Ok(()) => rep.changed += 1,
                    Err(e) => rep.errors.push(format!("{}: manifest: {e}", row.tech_name)),
                }
            } else {
                rep.skipped += 1;
            }
        } else if want && !linked {
            match make_link(&src, &link, &row.kind, &mut policy) {
                Ok(()) => {
                    // Record the projection's identity so a later deactivate can confirm it's
                    // ours (and skip the content hash). Zips only — dir projections are
                    // identified structurally (junction / marker), cheaply.
                    if row.kind == "zip" {
                        if let Some((size, mtime)) = file_stats(&link) {
                            db::record_projection(conn, &key, &row.file_name, size, mtime);
                        }
                    }
                    if let Err(e) = db::set_organized_active(conn, &key, &row.tech_name, true) {
                        rep.errors.push(format!("{}: manifest: {e}", row.tech_name));
                    }
                    rep.changed += 1;
                }
                Err(e) => rep.errors.push(format!("{}: link: {e}", row.tech_name)),
            }
        } else if !want && linked {
            // Two guards before removing anything from the root:
            //  1. The archived copy must exist behind it — a root entry with no archive is the
            //     only copy (e.g. a crash between manifest-write and move), never delete it.
            //  2. The entry must PROVABLY be Silo's projection. FAST path: its (size, mtime)
            //     still match what we recorded when we wrote it → our own untouched copy, no
            //     hash needed. Otherwise fall back to the careful structural/content check.
            //     If the user swapped in their own file at that name, neither matches — leave it.
            // Then `clear_projection` removes a copy only while the archived copy is a real copy
            // holding the same bytes (#399): ownership alone doesn't make it a spare.
            if !src.exists() {
                rep.errors.push(format!(
                    "{}: there's no copy in your library archive, so Silo kept the file in your mods folder. It won't delete the only copy.",
                    row.tech_name
                ));
                rep.skipped += 1;
            } else if !(matches_provenance(conn, &key, &link, &row.file_name)
                || is_silo_projection(&link, &src, &row.kind))
            {
                rep.errors.push(format!(
                    "{}: the file in your mods folder isn't the one Silo linked (did you replace it?), so Silo left it alone.",
                    row.tech_name
                ));
                rep.skipped += 1;
            } else {
                match clear_projection(&link, &src, root) {
                    Ok(kept) => {
                        if let Some(kept) = kept {
                            rep.notices
                                .push(kept_copy_notice(&row.file_name, &kept, false));
                        }
                        db::forget_projection(conn, &key, &row.file_name);
                        if let Err(e) = db::set_organized_active(conn, &key, &row.tech_name, false)
                        {
                            rep.errors.push(format!("{}: manifest: {e}", row.tech_name));
                        }
                        rep.changed += 1;
                    }
                    Err(e) => rep.errors.push(format!("{}: {e}", row.tech_name)),
                }
            }
        } else if want {
            // Already something at the mod's name. Same two checks as deactivate: Silo's own
            // projection gets its flag (and zip record) reconciled — a crash between linking and
            // recording leaves `active = false` behind; anything else is reported, never replaced.
            if !src.exists() {
                // No archived copy: the entry may be the only copy (a crash mid-organize). Leave it.
                rep.skipped += 1;
            } else if matches_provenance(conn, &key, &link, &row.file_name) {
                mark_active(conn, &key, &row, &mut rep);
            } else {
                match projection_verdict(&link, &src, &row.kind) {
                    Ok(true) => {
                        if row.kind == "zip" {
                            note_zip_projection(conn, &key, &row.file_name, &link);
                        }
                        mark_active(conn, &key, &row, &mut rep);
                    }
                    Ok(false) => {
                        let problem = format!(
                            "{}: {} in your mods folder isn't Silo's copy of this mod, so Silo left it there and the game loads that file instead. Pick which build to keep in Diagnose → Problems.",
                            row.tech_name, row.file_name
                        );
                        // Not this change's doing when it didn't touch the mod (#427).
                        if touched.is_some_and(|t| !t.contains(&row.tech_name)) {
                            rep.standing.push(problem);
                        } else {
                            rep.errors.push(problem);
                        }
                        rep.skipped += 1;
                    }
                    // Couldn't tell: an error whichever mods the change touched.
                    Err(err) => {
                        rep.errors.push(format!(
                            "{}: can't check {} in your mods folder ({err}), so Silo left it there.",
                            row.tech_name, row.file_name
                        ));
                        rep.skipped += 1;
                    }
                }
            }
        } else {
            // Nothing at the name and not wanted: a crash between linking or unlinking and
            // recording can leave `active = true`, or a gone projection's record, behind. The
            // record must never vouch for a file put at that name later. (Read first: most parked
            // mods have none, and a delete per mod would be a write per mod on every call.)
            if db::projection_stats(conn, &key, &row.file_name).is_some() {
                db::forget_projection(conn, &key, &row.file_name);
            }
            if row.active {
                match db::set_organized_active(conn, &key, &row.tech_name, false) {
                    Ok(()) => rep.changed += 1,
                    Err(e) => rep.errors.push(format!("{}: manifest: {e}", row.tech_name)),
                }
            } else {
                rep.skipped += 1;
            }
        }
    }
    rep
}

/// A wanted mod's slot already holds Silo's projection: record it as active if it isn't yet
/// (counted as a change), otherwise it's simply already in place (skipped).
fn mark_active(conn: &Connection, key: &str, row: &OrganizedRow, rep: &mut Report) {
    if row.active {
        rep.skipped += 1;
        return;
    }
    match db::set_organized_active(conn, key, &row.tech_name, true) {
        Ok(()) => rep.changed += 1,
        Err(e) => rep.errors.push(format!("{}: manifest: {e}", row.tech_name)),
    }
}

/// A plain file's `(size, mtime_ms)`, or `None` for a symlink/junction (identified structurally,
/// not by provenance) or an unreadable path.
pub(crate) fn file_stats(path: &Path) -> Option<(u64, i64)> {
    let m = std::fs::symlink_metadata(path).ok()?;
    if m.file_type().is_symlink() {
        return None;
    }
    let mtime = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some((m.len(), mtime))
}

/// Fast, hash-free ownership check: the flat file is provably Silo's own untouched projection if
/// its current `(size, mtime)` still match what we recorded when we wrote it. A file the user
/// swapped in has a different mtime and won't match — so it's never wrongly treated as ours.
pub(super) fn matches_provenance(
    conn: &Connection,
    key: &str,
    link: &Path,
    file_name: &str,
) -> bool {
    let Some((size, mtime)) = db::projection_stats(conn, key, file_name) else {
        return false;
    };
    matches!(file_stats(link), Some((s, m)) if s == size && m == mtime)
}

/// Restore a vanilla flat `mods/`: remove every Silo link, move archived files back
/// to the root, remove only empty archive directories (never an unrecognized file), and
/// clear the manifest.
pub fn flatten(conn: &Connection, layout: &Layout) -> Report {
    let root = &layout.mods;
    let key = crate::modsroot::canonical_key(root);
    let mut rep = Report::default();
    let mut categories: HashSet<String> = HashSet::new();
    // The archive entry of each mod this pass reported by name and may have left in the archive
    // (and whether it's a folder mod): its files aren't unrecognized (#410).
    let mut reported: Vec<(PathBuf, bool)> = Vec::new();
    // Each tech name's copies in the mods folder by entry name (keyed in ASCII lower case, see
    // `scan::root_entries_by_tech`), read once the first restore is due, and kept up to date with
    // what this pass removes and restores.
    let mut in_root: Option<std::collections::HashMap<String, HashSet<String>>> = None;
    // Archived mods this pass leaves in the archive on purpose: cleanup must not touch them.
    let mut retained: Vec<PathBuf> = Vec::new();
    for row in db::load_organized(conn, &key) {
        categories.insert(row.category.clone());
        let link = root.join(&row.file_name);
        let src = archive_path(&layout.archive, &row.category, &row.file_name);
        let is_dir = row.kind == "dir";

        if src.exists() {
            // A stale link (its target gone) holds nothing: drop it, then restore as usual.
            if let Err(e) = remove_dangling_link(&link) {
                rep.errors
                    .push(format!("{}: remove stale link: {e}", row.tech_name));
                reported.push((src, is_dir));
                continue;
            }
            let occupied = link.symlink_metadata().is_ok();
            // If a file occupies the root slot but it isn't Silo's projection (the user swapped
            // their own build in), don't remove it and don't overwrite it with the archived
            // copy — leave both in place and report the anomaly for the user to resolve. The
            // same ownership proof as deactivate: a recorded untouched copy is Silo's even when
            // the archived copy no longer matches it (then `clear_projection` keeps it, #399).
            if occupied
                && !(matches_provenance(conn, &key, &link, &row.file_name)
                    || is_silo_projection(&link, &src, &row.kind))
            {
                rep.errors.push(format!(
                    "{}: the file in your mods folder isn't the one Silo linked, so Silo left it and the copy in your library archive alone.",
                    row.tech_name
                ));
                rep.skipped += 1;
                reported.push((src, is_dir));
                continue;
            }
            // Normal restore: drop our projection to free the slot, then move the archived
            // original back into the root.
            let mut kept = None;
            if occupied {
                match clear_projection(&link, &src, root) {
                    Ok(k) => {
                        kept = k;
                        // That slot is free now: it no longer counts as a copy.
                        if let Some(names) = in_root
                            .as_mut()
                            .and_then(|c| c.get_mut(&row.tech_name.to_ascii_lowercase()))
                        {
                            names.retain(|n| !n.eq_ignore_ascii_case(&row.file_name));
                        }
                    }
                    Err(e) => {
                        rep.errors.push(format!("{}: {e}", row.tech_name));
                        reported.push((src, is_dir));
                        continue;
                    }
                }
            }
            // Another copy of this tech name in the mods folder (an unzipped folder beside it) is
            // the one the game loads: putting this one back would make two. Leave it in the
            // library, filed and parked, and say so (#423).
            let copies = in_root.get_or_insert_with(|| crate::scan::root_entries_by_tech(root));
            let tech_key = row.tech_name.to_ascii_lowercase();
            let other = copies.get(&tech_key).and_then(|names| {
                names
                    .iter()
                    .find(|n| !n.eq_ignore_ascii_case(&row.file_name))
                    .cloned()
            });
            if let Some(other) = other {
                rep.notices.push(format!(
                    "{}: another copy of this mod, {other}, is in your mods folder, and the game loads only one, so Silo left {} in your library. To put it back, move {other} out of your mods folder and undo organizing again.",
                    row.tech_name, row.file_name
                ));
                if let Some(kept) = &kept {
                    rep.notices
                        .push(kept_copy_notice(&row.file_name, kept, false));
                }
                if row.active {
                    let _ = db::set_organized_active(conn, &key, &row.tech_name, false);
                }
                db::forget_projection(conn, &key, &row.file_name);
                rep.skipped += 1;
                retained.push(src.clone());
                reported.push((src, is_dir));
                continue;
            }
            let restored = move_path(&src, &link);
            if restored.is_ok() {
                copies
                    .entry(tech_key)
                    .or_default()
                    .insert(row.file_name.clone());
            }
            if let Some(kept) = &kept {
                rep.notices
                    .push(kept_copy_notice(&row.file_name, kept, restored.is_ok()));
            }
            if let Err(e) = restored {
                rep.errors.push(format!("{}: restore: {e}", row.tech_name));
                reported.push((src, is_dir));
                continue;
            }
        } else if link.exists() {
            // A file sits in the root slot but there is NO archived copy behind it. That is
            // the original, not a projection (a projection always has an archive) — this is
            // exactly the state a crash between the manifest write and the move leaves. Never
            // delete it: keep the file, drop the stale manifest row.
            rep.errors.push(format!(
                "{}: there's no copy in your library archive, so Silo kept the file in your mods folder.",
                row.tech_name
            ));
            // Whatever sits at the archive path anyway (a dead link) is that mod's.
            reported.push((src, is_dir));
        }
        // else: neither archive nor root file — nothing to restore; just clear the row.
        if let Err(e) = db::delete_organized(conn, &key, &row.tech_name) {
            rep.errors.push(format!("{}: manifest: {e}", row.tech_name));
            continue;
        }
        rep.changed += 1;
    }
    // A user-chosen archive is the user's folder: remove only the category folders Silo filed
    // into (and only when empty), never recurse, and don't report their other content.
    if !layout.archive_is_default() {
        remove_empty_category_dirs(&layout.archive, &categories);
        return rep;
    }
    // The default archive is Silo's own. NEVER `remove_dir_all` it — that would destroy any
    // file we don't have a manifest row for (e.g. a mod whose manifest write failed on a
    // crash). Remove only provably-empty directories, and surface anything left untouched.
    // A mod reported by name above (kept, left alone, or not restored) isn't repeated as its
    // archived files being unrecognized (#410).
    match cleanup_empty_archive_keeping(&layout.archive, true, &retained) {
        Ok(leftover) => {
            // Each entry as spelled on disk, so a leftover listed under the folder's real
            // spelling matches it (looked up only when something was left).
            let reported: Vec<(PathBuf, bool)> = if leftover.is_empty() {
                Vec::new()
            } else {
                reported
                    .into_iter()
                    .map(|(entry, is_dir)| {
                        let spelled = spelled_on_disk(&layout.archive, &entry);
                        (spelled.unwrap_or(entry), is_dir)
                    })
                    .collect()
            };
            for p in leftover.into_iter().filter(|p| {
                !reported
                    .iter()
                    .any(|(entry, is_dir)| in_archive_entry(p, entry, *is_dir))
            }) {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
                rep.errors.push(format!(
                    "Silo kept an unrecognized file in your library archive: {}. Nothing was deleted.",
                    name.unwrap_or_else(|| p.display().to_string())
                ));
            }
        }
        Err(e) => rep.errors.push(format!("archive cleanup: {e}")),
    }
    rep
}

/// Whether `path` is the archive entry `entry`, or a file inside it when it's a folder mod's
/// (`is_dir`; a zip mod's entry owns nothing below it). Compared as spelled: pass the entry as
/// [`spelled_on_disk`] finds it.
pub(crate) fn in_archive_entry(path: &Path, entry: &Path, is_dir: bool) -> bool {
    path == entry || (is_dir && path.starts_with(entry))
}

/// `entry` (a path under `archive`) spelled as its folders list it. A name listed exactly is
/// itself, even beside one that differs only in case (a case-sensitive folder: two entries). A
/// name not listed but found anyway (NTFS and APFS ignore case and, on APFS, how an accent is
/// encoded) is the one listed name that differs only that way and is the same entry on disk
/// (`scratch::entry_number`, never following a link). When more than one listed name could be
/// it (hardlinks whose names differ only that way), Silo can't tell which, so `None`, as when a
/// part can't be listed or found: the caller then matches the recorded spelling only, so no
/// other file is ever taken for the entry.
pub(crate) fn spelled_on_disk(archive: &Path, entry: &Path) -> Option<PathBuf> {
    let mut cur = archive.to_path_buf();
    for part in entry.strip_prefix(archive).ok()?.components() {
        let name = part.as_os_str();
        let listed: Vec<std::ffi::OsString> = std::fs::read_dir(&cur)
            .ok()?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<_>>()
            .ok()?;
        if listed.iter().any(|n| n == name) {
            cur.push(name);
            continue;
        }
        let found = crate::scratch::entry_number(&cur.join(name))?;
        let wanted = name.to_string_lossy();
        let mut candidates = listed.into_iter().filter(|n| {
            let n_str = n.to_string_lossy();
            (crate::fold_file_name(&n_str) == crate::fold_file_name(&wanted)
                || n_str.to_uppercase() == wanted.to_uppercase())
                && crate::scratch::entry_number(&cur.join(n)) == Some(found)
        });
        let spelled = candidates.next()?;
        if candidates.next().is_some() {
            return None;
        }
        cur.push(spelled);
    }
    Some(cur)
}

/// Remove each category folder Silo filed into under `archive`, but only if it is empty.
/// Never recurses, never touches anything else, and reports nothing — for an archive folder
/// the user chose, whose other content is theirs.
pub(crate) fn remove_empty_category_dirs(archive: &Path, categories: &HashSet<String>) {
    for cat in categories {
        // `remove_dir` refuses a non-empty folder, which is exactly the guard we want.
        let _ = std::fs::remove_dir(archive_dir(archive, cat));
    }
}

/// Remove empty directories under `archive` bottom-up. NEVER deletes a file or a
/// non-empty directory — any file the manifest didn't restore is left in place and
/// returned so the caller can tell the user. This is the guard against the flatten
/// step eating an untracked mod. `archive` itself is removed (when empty) only if
/// `remove_root` — a user-chosen archive folder is never removed.
pub(crate) fn cleanup_empty_archive(
    archive: &Path,
    remove_root: bool,
) -> std::io::Result<Vec<PathBuf>> {
    cleanup_empty_archive_keeping(archive, remove_root, &[])
}

/// [`cleanup_empty_archive`], never entering or reporting the entries in `keep` (archived mods
/// left there on purpose, #423): an empty folder inside a kept folder mod is part of that mod.
pub(crate) fn cleanup_empty_archive_keeping(
    archive: &Path,
    remove_root: bool,
    keep: &[PathBuf],
) -> std::io::Result<Vec<PathBuf>> {
    let mut leftover = Vec::new();
    if !archive.exists() {
        return Ok(leftover);
    }
    // Returns true if `dir` is empty once processing finishes. At the top level, Silo's own
    // archive note (`relocate::ARCHIVE_NOTE`) counts as absent.
    fn walk(
        dir: &Path,
        top: bool,
        keep: &[PathBuf],
        leftover: &mut Vec<PathBuf>,
    ) -> std::io::Result<bool> {
        let mut empty = true;
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if keep
                .iter()
                .any(|k| k.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
            {
                empty = false;
                continue;
            }
            // Use symlink semantics: never follow/recurse a link.
            if entry.file_type()?.is_dir() {
                if walk(&path, false, keep, leftover)? {
                    let _ = std::fs::remove_dir(&path);
                } else {
                    empty = false;
                }
            } else if top && entry.file_name() == crate::relocate::ARCHIVE_NOTE {
                continue;
            } else {
                leftover.push(path);
                empty = false;
            }
        }
        Ok(empty)
    }
    if walk(archive, true, keep, &mut leftover)? && remove_root {
        let _ = std::fs::remove_file(archive.join(crate::relocate::ARCHIVE_NOTE));
        let _ = std::fs::remove_dir(archive);
    }
    Ok(leftover)
}
