//! Remove a mod from the library (#429): its file or folder goes to the Recycle Bin / Trash, an
//! active library mod is parked first, and its library row goes. Nothing is ever deleted outright,
//! and anything Silo can't prove is the mod the user confirmed is left where it is.

use super::*;

/// One mod to remove: its tech name, the path the Library shows for it (the archived copy for a
/// library mod, the file or folder in the mods folder for one that isn't filed), and, for a zip,
/// the size and modified time the confirmation was shown for, so a file changed or replaced since
/// is refused instead of removed unseen.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveItem {
    pub tech_name: String,
    pub path: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub mtime_ms: Option<u64>,
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

/// Move each item's file or folder to the Recycle Bin with `trash`.
///
/// A library mod (an organized row of that tech name whose archived copy is at the item's path)
/// is parked first if it's active, and goes only when nothing of Silo's is left at its name in the
/// mods folder; then its row and projection record are dropped. A mod that isn't filed must be the
/// named mod itself directly in the mods folder: a zip named after it or a folder of that name
/// holding `modDesc.xml`, not a link, and not Silo's copy of a library mod. Either way the entry
/// must really be where its path says (no link on the way, checked on the resolved folders), and
/// a zip must still have the size and time it was confirmed with. Anything Silo can't check is
/// refused. `archive_ok`: the library archive is reachable (a library mod is refused when it
/// isn't). Per-tech-name details Silo keeps elsewhere (favorite, notes, source page) stay, as they
/// do for the same mod in another mods folder.
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
    // Each path once, in the order asked.
    let mut seen = HashSet::new();
    let items: Vec<&RemoveItem> = items
        .iter()
        .filter(|it| seen.insert(it.path.clone()))
        .collect();
    let filed = |it: &RemoveItem| {
        rows.iter().find(|r| {
            r.tech_name == it.tech_name
                && archive_path(&layout.archive, &r.category, &r.file_name) == Path::new(&it.path)
        })
    };

    // Park every active library mod among them in one pass, as a switch would.
    let leaving: HashSet<&str> = items
        .iter()
        .filter_map(|it| filed(it).filter(|r| r.active).map(|r| r.tech_name.as_str()))
        .collect();
    let mut park_errors: Vec<String> = Vec::new();
    if archive_ok && !leaving.is_empty() {
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

    for it in items {
        let name = it.tech_name.as_str();
        let target = match filed(it) {
            Some(row) => library_target(layout, row, it, archive_ok, &park_errors),
            None => loose_target(layout, &rows, it),
        };
        let path = match target {
            Ok(p) => p,
            Err(why) => {
                rep.errors.push(format!("{name}: {why}"));
                continue;
            }
        };
        if let Err(e) = trash(&path) {
            let still = if filed(it).is_some() {
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
        if let Some(row) = filed(it) {
            if let Err(e) = db::drop_library_row(conn, &key, &row.tech_name, &row.file_name) {
                rep.errors.push(format!(
                    "{name}: moved to the Recycle Bin, but Silo couldn't update its library records ({e}). Re-read every mod in Settings → Maintenance to tidy them."
                ));
            }
        }
    }
    rep
}

/// The archived copy of library mod `row` (asked for as `it`), if Silo may move it: the archive is
/// reachable, parking didn't fail for it, nothing of Silo's is left at its name (or Silo can't
/// tell), it really is in its category folder of the archive, and it's unchanged since confirmed.
fn library_target(
    layout: &Layout,
    row: &OrganizedRow,
    it: &RemoveItem,
    archive_ok: bool,
    park_errors: &[String],
) -> Result<PathBuf, String> {
    if !archive_ok {
        return Err("your library archive isn't reachable, so Silo didn't remove it.".into());
    }
    let prefix = format!("{}: ", row.tech_name);
    if let Some(e) = park_errors.iter().find(|e| e.starts_with(&prefix)) {
        return Err(format!("{} It wasn't removed.", &e[prefix.len()..]));
    }
    let src = archive_path(&layout.archive, &row.category, &row.file_name);
    let link = layout.mods.join(&row.file_name);
    if link.symlink_metadata().is_ok() {
        match projection_verdict(&link, &src, &row.kind) {
            Ok(false) => {} // the user's own file at its name: not part of this library entry
            Ok(true) => return Err("Silo couldn't park it first, so it wasn't removed.".into()),
            Err(e) => {
                return Err(format!(
                    "Silo couldn't check {} in your mods folder ({e}), so it wasn't removed.",
                    row.file_name
                ))
            }
        }
    }
    let category_dir = src.parent().and_then(|p| p.file_name()).unwrap_or_default();
    really_at(
        &src,
        &layout.archive.join(category_dir),
        Some(&layout.archive),
    )?;
    unchanged_since_confirmed(&src, it)?;
    Ok(src)
}

/// The mods-folder entry an item that isn't a library mod names, if Silo may move it: the named
/// mod itself (a zip named after it, or a folder of that name holding `modDesc.xml`) directly in
/// the mods folder, not a link (a link is the user's to remove), not Silo's own copy of a library
/// mod at that name (or one Silo can't tell), and unchanged since confirmed.
fn loose_target(
    layout: &Layout,
    rows: &[OrganizedRow],
    it: &RemoveItem,
) -> Result<PathBuf, String> {
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
    unchanged_since_confirmed(&path, it)?;
    Ok(path)
}

/// `entry` is itself a real file or folder (not a link) whose folder, resolved, is `folder`
/// resolved, so no link on the way (a category folder swapped for a junction, say) can send the
/// move somewhere else. With `within`, `folder` must also resolve directly inside it.
fn really_at(entry: &Path, folder: &Path, within: Option<&Path>) -> Result<(), String> {
    let name = entry
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let meta = entry
        .symlink_metadata()
        .map_err(|_| format!("{name} is no longer there."))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{name} is a link, not the mod itself, so Silo left it."
        ));
    }
    let resolve = |p: &Path| {
        std::fs::canonicalize(p)
            .map_err(|e| format!("Silo couldn't check where {name} is ({e}), so it left it alone."))
    };
    let parent = entry
        .parent()
        .ok_or_else(|| format!("{name} has no folder."))?;
    let here = resolve(parent)?;
    if here != resolve(folder)? {
        return Err(format!(
            "{name} isn't where Silo expects it, so Silo left it alone."
        ));
    }
    if let Some(within) = within {
        if folder
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
            || here.parent() != Some(resolve(within)?.as_path())
        {
            return Err(format!(
                "{name} isn't where Silo expects it, so Silo left it alone."
            ));
        }
    }
    Ok(())
}

/// A zip still has the size and modified time the confirmation was shown for (as the scan reads
/// them). A folder isn't compared: the scan doesn't report one stable stamp for it.
fn unchanged_since_confirmed(path: &Path, it: &RemoveItem) -> Result<(), String> {
    let (Some(size), Some(mtime_ms)) = (it.size, it.mtime_ms) else {
        return Ok(());
    };
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        return Ok(());
    }
    let now_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if meta.len() != size || now_ms != mtime_ms {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Err(format!(
            "{name} changed since you confirmed, so Silo left it. Try again to remove the file as it is now."
        ));
    }
    Ok(())
}
