//! Remove a mod from the library (#429): its file or folder goes to the Recycle Bin / Trash, an
//! active library mod is parked first, and its library row goes. Nothing is ever deleted outright.

use super::*;

/// One mod to remove: its tech name and the path the Library shows for it (the archived copy for
/// a library mod, the file or folder in the mods folder for one that isn't filed).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveItem {
    pub tech_name: String,
    pub path: String,
}

/// What a removal did: the tech names whose file went to the Recycle Bin, and why any didn't.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveReport {
    pub removed: Vec<String>,
    pub errors: Vec<String>,
}

/// Move each item's file or folder to the Recycle Bin with `trash`. A library mod (an organized
/// row whose archived copy is at the item's path) is parked first if it's active, and only goes
/// once nothing of Silo's is left at its name in the mods folder; then its row is dropped. A mod
/// that isn't filed must be a plain file or folder directly in the mods folder, not a link and
/// not Silo's copy of a library mod. `archive_ok`: the library archive is reachable (a library
/// mod is refused when it isn't). Per-tech-name details Silo keeps elsewhere (favorite, notes,
/// source page) stay, as they do for a mod in another mods folder.
pub fn remove_from_library(
    conn: &Connection,
    layout: &Layout,
    items: &[RemoveItem],
    archive_ok: bool,
    trash: &dyn Fn(&Path) -> Result<(), String>,
) -> RemoveReport {
    let key = crate::modsroot::canonical_key(&layout.mods);
    let mut rep = RemoveReport::default();
    let rows = db::load_organized(conn, &key);
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
        park_errors = set_active(conn, layout, &keep).errors;
    }

    for it in items {
        let name = it.tech_name.as_str();
        match filed(it) {
            Some(row) => {
                if !archive_ok {
                    rep.errors.push(format!(
                        "{name}: your library archive isn't reachable, so Silo didn't remove it."
                    ));
                    continue;
                }
                if let Some(e) = park_errors
                    .iter()
                    .find(|e| e.starts_with(&format!("{name}: ")))
                {
                    rep.errors.push(format!("{e} It wasn't removed."));
                    continue;
                }
                let src = archive_path(&layout.archive, &row.category, &row.file_name);
                let link = layout.mods.join(&row.file_name);
                if link.symlink_metadata().is_ok() && is_silo_projection(&link, &src, &row.kind) {
                    rep.errors.push(format!(
                        "{name}: Silo couldn't park it first, so it wasn't removed."
                    ));
                    continue;
                }
                match trash(&src) {
                    Ok(()) => {
                        crate::fslog::changed("trash", &src);
                        let _ = db::delete_organized(conn, &key, name);
                        db::forget_projection(conn, &key, &row.file_name);
                        rep.removed.push(name.to_string());
                    }
                    Err(e) => rep.errors.push(format!(
                        "{name}: couldn't move it to the Recycle Bin ({e}). It's parked in your library."
                    )),
                }
            }
            None => match loose_target(layout, &rows, it) {
                Ok(path) => match trash(&path) {
                    Ok(()) => {
                        crate::fslog::changed("trash", &path);
                        rep.removed.push(name.to_string());
                    }
                    Err(e) => rep.errors.push(format!(
                        "{name}: couldn't move it to the Recycle Bin ({e})."
                    )),
                },
                Err(why) => rep.errors.push(format!("{name}: {why}")),
            },
        }
    }
    rep
}

/// The mods-folder entry an item that isn't a library mod names, if Silo may move it: a plain
/// file or folder directly in the mods folder, not a link (a link is the user's to remove), and
/// not Silo's own copy of a library mod at its name.
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
    if path != given || name.eq_ignore_ascii_case(ARCHIVE) {
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
    if rows.iter().any(|r| {
        r.file_name == name
            && is_silo_projection(
                &path,
                &archive_path(&layout.archive, &r.category, &r.file_name),
                kind,
            )
    }) {
        return Err(format!(
            "{name} is Silo's copy of a mod in your library. Remove that mod from the library instead."
        ));
    }
    Ok(path)
}
