//! Remove a mod from the library (#429): its file or folder goes to the Recycle Bin / Trash, an
//! active library mod is parked first, and its library row goes. Nothing is ever deleted outright,
//! and anything Silo can't prove is the mod the user confirmed is left where it is.

use super::*;

/// One mod to remove, as the confirmation showed it: its tech name, the path the Library shows
/// (the archived copy for a library mod, the file or folder in the mods folder for one that isn't
/// filed), whether it's a zip or a folder, and the size and modified time the scan read for it.
/// An entry that changed since (another kind, another size or time) is refused, not removed unseen.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveItem {
    pub tech_name: String,
    pub path: String,
    /// "zip" | "dir"
    pub kind: String,
    pub size: u64,
    pub mtime_ms: u64,
}

/// One mod that went to the Recycle Bin, as it was asked for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Removed {
    pub tech_name: String,
    pub path: String,
}

/// What a removal did: what went to the Recycle Bin, why any didn't (one line each, starting with
/// the tech name), and anything else worth saying (where parking kept an edited copy).
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveReport {
    pub removed: Vec<Removed>,
    pub errors: Vec<String>,
    pub notices: Vec<String>,
}

/// Names in the mods folder that are Silo's own, never a mod to remove.
const RESERVED: &[&str] = &[ARCHIVE, "backups"];

/// Move each item's file or folder to the Recycle Bin with `trash`.
///
/// Every item is checked before anything is parked or moved:
/// - a library mod is an organized row of that tech name and kind whose archived copy is at the
///   item's path, really there (not a link, its category folder not a link and directly in the
///   archive, the default archive not a link either);
/// - a mod that isn't filed is the named mod itself directly in the mods folder: a zip named after
///   it, or a folder of that name holding `modDesc.xml`, not a link, not one of Silo's own folders;
/// - either way it's still the kind, size and modified time the confirmation showed.
///
/// Then the active library mods among them are parked in one pass, as a switch would, and each
/// goes only once nothing of Silo's is left at its name (a slot Silo can't inspect refuses); its
/// row and projection record are then dropped. A loose mod that's Silo's copy of a library mod is
/// refused. Anything Silo can't check is refused. `archive_ok`: the library archive is reachable.
/// Per-tech-name details Silo keeps elsewhere (favorite, notes, source page) stay, as they do for
/// the same mod in another mods folder.
pub fn remove_from_library(
    conn: &Connection,
    layout: &Layout,
    items: &[RemoveItem],
    archive_ok: bool,
    trash: &dyn Fn(&Path) -> Result<(), String>,
) -> RemoveReport {
    let key = crate::modsroot::canonical_key(&layout.mods);
    let mut rep = RemoveReport::default();
    let rows = match db::try_load_organized(conn, &key) {
        Ok(rows) => rows,
        Err(e) => {
            for it in items {
                rep.errors.push(format!(
                    "{}: Silo couldn't read its library records ({e}), so it didn't remove it.",
                    it.tech_name
                ));
            }
            return rep;
        }
    };
    let filed = |it: &RemoveItem| {
        rows.iter().find(|r| {
            r.tech_name == it.tech_name
                && archive_path(&layout.archive, &r.category, &r.file_name) == Path::new(&it.path)
        })
    };

    // Check every item first (each path once): one that fails isn't parked either.
    let mut seen = HashSet::new();
    let mut ready: Vec<(&RemoveItem, Option<&OrganizedRow>, PathBuf)> = Vec::new();
    for it in items.iter().filter(|it| seen.insert(it.path.clone())) {
        let row = filed(it);
        let checked = match row {
            Some(row) => check_library(layout, row, it, archive_ok),
            None => check_loose(layout, &rows, it),
        };
        match checked {
            Ok(path) => ready.push((it, row, path)),
            Err(why) => rep.errors.push(format!("{}: {why}", it.tech_name)),
        }
    }

    // Park the active library mods among them in one pass, as a switch would.
    let leaving: HashSet<&str> = ready
        .iter()
        .filter_map(|(_, row, _)| row.filter(|r| r.active).map(|r| r.tech_name.as_str()))
        .collect();
    let mut park_errors: Vec<String> = Vec::new();
    if !leaving.is_empty() {
        let keep: HashSet<String> = rows
            .iter()
            .filter(|r| r.active && !leaving.contains(r.tech_name.as_str()))
            .map(|r| r.tech_name.clone())
            .collect();
        let parked = set_active(conn, layout, &keep);
        park_errors = parked.errors;
        // Where parking kept an edited folder copy: the user needs to know before its original goes.
        rep.notices.extend(parked.notices);
    }

    for (it, row, path) in ready {
        let name = it.tech_name.as_str();
        if let Some(row) = row {
            if let Err(why) = slot_clear(layout, row, &park_errors) {
                rep.errors.push(format!("{name}: {why}"));
                continue;
            }
        }
        if let Err(e) = trash(&path) {
            let still = if row.is_some() {
                " It's parked in your library."
            } else {
                ""
            };
            rep.errors.push(format!(
                "{name}: couldn't move it to the Recycle Bin ({e}).{still}"
            ));
            continue;
        }
        crate::fslog::changed("trash", &path);
        rep.removed.push(Removed {
            tech_name: name.to_string(),
            path: it.path.clone(),
        });
        if let Some(row) = row {
            if let Err(e) = db::drop_library_row(conn, &key, &row.tech_name, &row.file_name) {
                rep.errors.push(format!(
                    "{name}: moved to the Recycle Bin, but Silo couldn't update its library records ({e}). Re-read every mod in Settings → Maintenance to tidy them."
                ));
            }
        }
    }
    rep
}

/// Library mod `row` (asked for as `it`) may go, before parking: the archive is reachable, the
/// item is the row's kind, and the archived copy is really in its category folder of the archive,
/// as confirmed. Returns the archived copy.
fn check_library(
    layout: &Layout,
    row: &OrganizedRow,
    it: &RemoveItem,
    archive_ok: bool,
) -> Result<PathBuf, String> {
    if !archive_ok {
        return Err("your library archive isn't reachable, so Silo didn't remove it.".into());
    }
    if it.kind != row.kind {
        return Err("it changed since you confirmed, so Silo left it. Try again.".into());
    }
    if layout.archive_is_default() && is_link(&layout.archive) != Some(false) {
        return Err(
            "your library archive folder is a link or can't be read, so Silo left it.".into(),
        );
    }
    let src = archive_path(&layout.archive, &row.category, &row.file_name);
    let category_dir = src.parent().and_then(|p| p.file_name()).unwrap_or_default();
    really_at(
        &src,
        &layout.archive.join(category_dir),
        Some(&layout.archive),
    )?;
    as_confirmed(&src, it)?;
    Ok(src)
}

/// After parking: nothing of Silo's is left at library mod `row`'s name. Parking reported no
/// problem for it, and the slot is empty, or holds the user's own file (not part of this library
/// entry). A slot Silo can't inspect refuses.
fn slot_clear(layout: &Layout, row: &OrganizedRow, park_errors: &[String]) -> Result<(), String> {
    let prefix = format!("{}: ", row.tech_name);
    if let Some(e) = park_errors.iter().find(|e| e.starts_with(&prefix)) {
        return Err(format!("{} It wasn't removed.", &e[prefix.len()..]));
    }
    let src = archive_path(&layout.archive, &row.category, &row.file_name);
    let link = layout.mods.join(&row.file_name);
    match link.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "Silo couldn't check {} in your mods folder ({e}), so it wasn't removed.",
            row.file_name
        )),
        Ok(_) => match projection_verdict(&link, &src, &row.kind) {
            Ok(false) => Ok(()),
            Ok(true) => Err("Silo couldn't park it first, so it wasn't removed.".into()),
            Err(e) => Err(format!(
                "Silo couldn't check {} in your mods folder ({e}), so it wasn't removed.",
                row.file_name
            )),
        },
    }
}

/// A mod that isn't filed may go: the named mod itself (a zip named after it, or a folder of that
/// name holding `modDesc.xml`) directly in the mods folder, not one of Silo's own folders, not a
/// link (a link is the user's to remove), not Silo's own copy of a library mod at that name (or
/// one Silo can't tell), and as confirmed. Returns its path.
fn check_loose(layout: &Layout, rows: &[OrganizedRow], it: &RemoveItem) -> Result<PathBuf, String> {
    let given = Path::new(&it.path);
    let name = given
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::paths::safe_file_name(&name)?;
    let path = layout.mods.join(&name);
    if path != given {
        return Err("it isn't in your mods folder or your library, so Silo left it alone.".into());
    }
    if RESERVED.iter().any(|r| name.eq_ignore_ascii_case(r)) || crate::scan::is_silo_temp(&name) {
        return Err(format!(
            "{name} is Silo's own, not a mod, so Silo left it alone."
        ));
    }
    let meta = path
        .symlink_metadata()
        .map_err(|_| format!("{name} is no longer there."))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{name} is a link, not the mod itself, so Silo left it. Remove it yourself if you like."
        ));
    }
    let kind = if meta.is_dir() { "dir" } else { "zip" };
    let is_it = if meta.is_dir() {
        name.eq_ignore_ascii_case(&it.tech_name) && path.join("modDesc.xml").is_file()
    } else {
        name.len() > 4
            && name[name.len() - 4..].eq_ignore_ascii_case(".zip")
            && name[..name.len() - 4].eq_ignore_ascii_case(&it.tech_name)
    };
    if !is_it {
        return Err(format!("{name} isn't that mod, so Silo left it alone."));
    }
    if it.kind != kind {
        return Err(format!(
            "{name} changed since you confirmed, so Silo left it. Try again."
        ));
    }
    really_at(&path, &layout.mods, None)?;
    for r in rows
        .iter()
        .filter(|r| r.file_name.eq_ignore_ascii_case(&name))
    {
        let src = archive_path(&layout.archive, &r.category, &r.file_name);
        match projection_verdict(&path, &src, kind) {
            Ok(false) => {}
            Ok(true) => {
                return Err(format!(
                    "{name} is Silo's copy of a mod in your library. Remove that mod from the library instead."
                ))
            }
            Err(e) => {
                return Err(format!(
                    "Silo couldn't tell whether {name} is its copy of a mod in your library ({e}), so it left it alone."
                ))
            }
        }
    }
    as_confirmed(&path, it)?;
    Ok(path)
}

/// Whether `p` is a link (Some), or None when it can't be read.
fn is_link(p: &Path) -> Option<bool> {
    p.symlink_metadata()
        .ok()
        .map(|m| m.file_type().is_symlink())
}

/// `entry` is itself a real file or folder (not a link) whose folder, resolved, is `folder`
/// resolved. With `within`, `folder` must also not be a link and resolve directly inside it, so a
/// category folder swapped for a link can't send the move somewhere else.
fn really_at(entry: &Path, folder: &Path, within: Option<&Path>) -> Result<(), String> {
    let name = entry
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match is_link(entry) {
        None => return Err(format!("{name} is no longer there.")),
        Some(true) => {
            return Err(format!(
                "{name} is a link, not the mod itself, so Silo left it."
            ))
        }
        Some(false) => {}
    }
    let resolve = |p: &Path| {
        std::fs::canonicalize(p)
            .map_err(|e| format!("Silo couldn't check where {name} is ({e}), so it left it alone."))
    };
    let moved = || format!("{name} isn't where Silo expects it, so Silo left it alone.");
    let parent = entry.parent().ok_or_else(moved)?;
    let here = resolve(parent)?;
    if here != resolve(folder)? {
        return Err(moved());
    }
    if let Some(within) = within {
        if is_link(folder) != Some(false) || here.parent() != Some(resolve(within)?.as_path()) {
            return Err(moved());
        }
    }
    Ok(())
}

/// The entry is still the kind, size and modified time the confirmation showed (read as the scan
/// reads them).
fn as_confirmed(path: &Path, it: &RemoveItem) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let meta = std::fs::metadata(path).map_err(|e| format!("Silo couldn't check {name} ({e})."))?;
    let now_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let kind = if meta.is_dir() { "dir" } else { "zip" };
    if kind != it.kind || meta.len() != it.size || now_ms != it.mtime_ms {
        return Err(format!(
            "{name} changed since you confirmed, so Silo left it. Try again to remove it as it is now."
        ));
    }
    Ok(())
}
