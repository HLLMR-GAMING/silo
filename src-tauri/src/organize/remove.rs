//! Remove a mod from the library (#429): its file or folder goes to the Recycle Bin / Trash, an
//! active library mod is parked first, and its library row goes. Nothing is deleted outright, and
//! nothing Silo can't prove is what the user confirmed is touched.
//!
//! Two steps. [`plan_removal`] checks each asked-for mod and records what it is ([`Evidence`]: the
//! entry's file number, kind, size and time, a folder's whole listing, and where the mods folder
//! and library really are); the confirmation names what the plan holds. [`carry_out`] checks it
//! all again and refuses anything that changed since, before parking or moving anything.

use super::*;

/// One mod the Library asks to remove: its tech name and the path it shows for it (the archived
/// copy for a library mod, the file or folder in the mods folder for one that isn't filed).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveRequest {
    pub tech_name: String,
    pub path: String,
}

/// A mod a plan will remove, as the confirmation names it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Planned {
    pub tech_name: String,
    pub path: String,
    /// The file or folder name that goes to the bin.
    pub file_name: String,
    /// "zip" | "dir"
    pub kind: String,
    /// It's on now (an active library mod, or a loose mod the game loads): parked first.
    pub on: bool,
}

/// What a plan found: the mods it will remove, and why any asked for won't be (one line each,
/// starting with the tech name).
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovalPlan {
    pub items: Vec<Planned>,
    pub refused: Vec<String>,
    /// Keeping one copy (#423): whether the game loads the kept copy now (it's in the mods
    /// folder, or it's the copy Silo's link at its name points to). None: it couldn't be told.
    pub keep_on: Option<bool>,
}

/// What was true of a planned mod's entry when the user confirmed, compared again before anything
/// moves: the entry's file number, kind, size and time, a folder's listing (every entry's path,
/// kind, size and time), and each folder it depends on, resolved, with its file number.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    entry: Option<(u64, u64)>,
    kind: &'static str,
    size: u64,
    mtime_ms: u64,
    listing: Vec<ListedEntry>,
    roots: Vec<(PathBuf, Option<(u64, u64)>)>,
}

/// One entry under a planned folder: relative path, folder or not, size, time, file number.
type ListedEntry = (String, bool, u64, u64, (u64, u64));

/// What became of one item the bin was asked to take.
#[derive(Debug, Clone, PartialEq)]
pub enum Binned {
    /// In the Recycle Bin / Trash.
    Recycled,
    /// Still where it was; why.
    Kept(String),
    /// Gone from its folder without a Recycle Bin record: deleted permanently (where it can't
    /// recycle, Windows asks first), or the bin couldn't be read; what's known.
    Gone(String),
    /// Silo couldn't tell whether it's still there; why.
    Unchecked(String),
}

/// One mod that went, as it was asked for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Removed {
    pub tech_name: String,
    pub path: String,
    /// In the Recycle Bin / Trash (false: deleted permanently, or not confirmed in the bin).
    pub recycled: bool,
}

/// What carrying out a plan did: what went to the bin, why any didn't (one line each, starting
/// with the tech name), and anything else worth saying (where parking kept an edited copy).
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveReport {
    pub removed: Vec<Removed>,
    pub errors: Vec<String>,
    pub notices: Vec<String>,
}

/// Names in the mods folder that are Silo's own, never a mod to remove.
const RESERVED: &[&str] = &[ARCHIVE, "backups"];

/// Check each request and record what it is.
/// - A library mod is an organized row of that tech name whose archived copy is at the path: the
///   archive reachable, the copy really there (not a link, its category folder not a link and
///   directly in the archive, the default archive not a link either), and the row's kind.
/// - A mod that isn't filed is the named mod itself directly in the mods folder: a zip named after
///   it, or a folder of that name holding `modDesc.xml`; not a link, not one of Silo's own folders,
///   and not Silo's copy of a library mod (or one Silo can't tell).
///
/// Each path once. Returns the plan to show and each planned mod's evidence.
pub fn plan_removal(
    conn: &Connection,
    layout: &Layout,
    requests: &[RemoveRequest],
    archive_ok: bool,
) -> (RemovalPlan, Vec<(Planned, Evidence)>) {
    plan_removal_keeping(conn, layout, requests, None, archive_ok)
}

/// The copy of a mod the user keeps while the others are removed (#423), as it was when they
/// confirmed: checked again before anything is parked or moved, and never touched.
#[derive(Debug, Clone)]
pub struct Keeper {
    path: PathBuf,
    roots: Vec<PathBuf>,
    evidence: Evidence,
}

/// Check the copy the user keeps (`keep`, the path the Library shows for it): a real file or folder
/// in the mods folder or directly in a category folder of the archive, and record what it is.
pub fn keeper(layout: &Layout, keep: &RemoveRequest) -> Result<Keeper, String> {
    let path = PathBuf::from(&keep.path);
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let roots = if parent == layout.mods {
        really_at(&path, &layout.mods, None)?;
        vec![layout.mods.clone()]
    } else if parent.parent() == Some(layout.archive.as_path()) {
        really_at(&path, &parent, Some(&layout.archive))?;
        vec![layout.mods.clone(), layout.archive.clone(), parent]
    } else {
        return Err("the copy you're keeping isn't in your mods folder or your library".into());
    };
    let refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let evidence = evidence_of(&path, &refs)?;
    Ok(Keeper {
        path,
        roots,
        evidence,
    })
}

/// Whether the game loads the kept copy now: one in the mods folder does; a library copy does when
/// it's what Silo's link (or copy) at its name in the mods folder is. None when that slot can't be
/// checked. Told from the files, not from what the Library rows show (two library copies of one
/// mod can both show as on).
pub fn keeper_loaded(layout: &Layout, k: &Keeper) -> Option<bool> {
    if k.path.parent() == Some(layout.mods.as_path()) {
        return Some(true);
    }
    let name = k.path.file_name()?;
    let slot = layout.mods.join(name);
    match slot.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
        Ok(_) => projection_verdict(&slot, &k.path, k.evidence.kind).ok(),
    }
}

/// The kept copy is still what it was when the user confirmed.
fn keeper_unchanged(k: &Keeper) -> bool {
    let refs: Vec<&Path> = k.roots.iter().map(PathBuf::as_path).collect();
    evidence_of(&k.path, &refs).is_ok_and(|now| now == k.evidence)
}

/// Why removing `planned` (with its evidence and library row, if any) would touch the kept copy:
/// it is that copy, the same file under another name, or the library mod whose link in the mods
/// folder the kept copy is.
fn touches_keeper(
    layout: &Layout,
    k: &Keeper,
    planned: &Planned,
    evidence: &Evidence,
    row: Option<&OrganizedRow>,
) -> Option<String> {
    if Path::new(&planned.path) == k.path {
        return Some("that's the copy you're keeping".into());
    }
    if evidence.entry.is_some() && evidence.entry == k.evidence.entry {
        return Some("it's the same file as the copy you're keeping".into());
    }
    let keeps_slot = row.is_some_and(|r| {
        k.path.parent() == Some(layout.mods.as_path())
            && k.path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&r.file_name))
    });
    keeps_slot.then(|| "the copy you're keeping is Silo's link to it in your mods folder".into())
}

/// [`plan_removal`] keeping `keep` (#423): a request that is the kept copy, or would touch it, is
/// refused.
pub fn plan_removal_keeping(
    conn: &Connection,
    layout: &Layout,
    requests: &[RemoveRequest],
    keep: Option<&Keeper>,
    archive_ok: bool,
) -> (RemovalPlan, Vec<(Planned, Evidence)>) {
    let key = crate::modsroot::canonical_key(&layout.mods);
    let mut plan = RemovalPlan::default();
    let mut found = Vec::new();
    let rows = match db::try_load_organized(conn, &key) {
        Ok(rows) => rows,
        Err(e) => {
            for r in requests {
                plan.refused.push(format!(
                    "{}: Silo couldn't read its library records ({e}), so it won't remove it.",
                    r.tech_name
                ));
            }
            return (plan, found);
        }
    };
    let mut seen = HashSet::new();
    for r in requests.iter().filter(|r| seen.insert(r.path.clone())) {
        let checked = check(layout, &rows, r, archive_ok).and_then(|(planned, evidence)| {
            let row = library_row(layout, &rows, r);
            match keep.and_then(|k| touches_keeper(layout, k, &planned, &evidence, row)) {
                Some(why) => Err(format!("{why}, so Silo left it.")),
                None => Ok((planned, evidence)),
            }
        });
        match checked {
            Ok(pair) => {
                plan.items.push(pair.0.clone());
                found.push(pair);
            }
            Err(why) => plan.refused.push(format!("{}: {why}", r.tech_name)),
        }
    }
    plan.keep_on = keep.and_then(|k| keeper_loaded(layout, k));
    (plan, found)
}

/// Remove the planned mods: each is checked again and must match its evidence, or it's refused
/// before anything is parked or moved. Then the active library mods among them are parked in one
/// pass, as a switch would, and each goes only once nothing of Silo's is left at its name (a slot
/// Silo can't inspect refuses). `bin` moves an item and says what became of it. A library mod's
/// row and projection record are dropped once its archived copy is gone from the archive.
/// Per-tech-name details Silo keeps elsewhere (favorite, notes, source page) stay, as they do for
/// the same mod in another mods folder.
pub fn carry_out(
    conn: &Connection,
    layout: &Layout,
    planned: &[(Planned, Evidence)],
    keep: Option<&Keeper>,
    archive_ok: bool,
    bin: &dyn Fn(&Path) -> Binned,
) -> RemoveReport {
    let key = crate::modsroot::canonical_key(&layout.mods);
    let mut rep = RemoveReport::default();
    let rows = match db::try_load_organized(conn, &key) {
        Ok(rows) => rows,
        Err(e) => {
            for (p, _) in planned {
                rep.errors.push(format!(
                    "{}: Silo couldn't read its library records ({e}), so it didn't remove it.",
                    p.tech_name
                ));
            }
            return rep;
        }
    };

    // The copy the user keeps must still be the one they confirmed, or nothing goes (#423).
    if keep.is_some_and(|k| !keeper_unchanged(k)) {
        for (p, _) in planned {
            rep.errors.push(format!(
                "{}: the copy you're keeping changed since you confirmed, so Silo removed nothing. Try again.",
                p.tech_name
            ));
        }
        return rep;
    }

    // Everything again, against what was confirmed: one that changed isn't parked either.
    let still_confirmed = |p: &Planned, confirmed: &Evidence| -> Result<(), String> {
        let request = RemoveRequest {
            tech_name: p.tech_name.clone(),
            path: p.path.clone(),
        };
        match check(layout, &rows, &request, archive_ok) {
            Ok((now, evidence)) if now == *p && evidence == *confirmed => Ok(()),
            Ok(_) => Err("it changed since you confirmed, so Silo left it. Try again to remove it as it is now.".into()),
            Err(why) => Err(why),
        }
    };
    let mut ready: Vec<(&Planned, &Evidence, Option<OrganizedRow>, PathBuf)> = Vec::new();
    for (p, confirmed) in planned {
        match still_confirmed(p, confirmed) {
            Ok(()) => {
                let request = RemoveRequest {
                    tech_name: p.tech_name.clone(),
                    path: p.path.clone(),
                };
                let row = library_row(layout, &rows, &request).cloned();
                ready.push((p, confirmed, row, PathBuf::from(&p.path)));
            }
            Err(why) => rep.errors.push(format!("{}: {why}", p.tech_name)),
        }
    }

    let leaving: HashSet<&str> = ready
        .iter()
        .filter_map(|(_, _, row, _)| {
            row.as_ref()
                .filter(|r| r.active)
                .map(|r| r.tech_name.as_str())
        })
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

    for (p, confirmed, row, path) in ready {
        let name = p.tech_name.as_str();
        if let Some(row) = &row {
            if let Err(why) = slot_clear(layout, row, &park_errors) {
                rep.errors.push(format!("{name}: {why}"));
                continue;
            }
        }
        // Once more right before it moves: parking, or moving the ones before it, took a while.
        if let Err(why) = still_confirmed(p, confirmed) {
            rep.errors.push(format!("{name}: {why}"));
            continue;
        }
        if keep.is_some_and(|k| !keeper_unchanged(k)) {
            // Partway through: what's already gone stays gone (it's in `removed`); the rest stays.
            rep.errors.push(format!(
                "{name}: the copy you're keeping changed partway through, so Silo stopped and left this one. Try again."
            ));
            continue;
        }
        let gone = match bin(&path) {
            Binned::Recycled => {
                crate::fslog::changed("trash", &path);
                rep.removed.push(Removed {
                    tech_name: name.to_string(),
                    path: p.path.clone(),
                    recycled: true,
                });
                true
            }
            Binned::Kept(why) => {
                let still = if row.is_some() {
                    " It's still in your library, parked."
                } else {
                    ""
                };
                rep.errors.push(format!("{name}: {why}.{still}"));
                false
            }
            // Gone, as the user chose in Windows' own prompt, or not confirmed in the bin: removed,
            // and said how.
            Binned::Gone(what) => {
                crate::fslog::changed("delete", &path);
                rep.removed.push(Removed {
                    tech_name: name.to_string(),
                    path: p.path.clone(),
                    recycled: false,
                });
                rep.notices.push(format!("{}: {what}.", p.file_name));
                true
            }
            // Not known to be gone: its records stay, so nothing is lost track of.
            Binned::Unchecked(why) => {
                rep.errors
                    .push(format!("{name}: {why}. It's still listed in your library."));
                false
            }
        };
        // Gone from the archive either way: its records go too.
        if let (true, Some(row)) = (gone, &row) {
            if let Err(e) = db::drop_library_row(conn, &key, &row.tech_name, &row.file_name) {
                rep.errors.push(format!(
                    "{name}: Silo couldn't update its library records ({e}). Re-read every mod in Settings → Maintenance to tidy them."
                ));
            }
        }
    }
    rep
}

/// The library row `r` names: its tech name, with its archived copy at the path.
fn library_row<'a>(
    layout: &Layout,
    rows: &'a [OrganizedRow],
    r: &RemoveRequest,
) -> Option<&'a OrganizedRow> {
    rows.iter().find(|row| {
        row.tech_name == r.tech_name
            && archive_path(&layout.archive, &row.category, &row.file_name) == Path::new(&r.path)
    })
}

/// Check one request (see [`plan_removal`]) and gather its evidence.
fn check(
    layout: &Layout,
    rows: &[OrganizedRow],
    r: &RemoveRequest,
    archive_ok: bool,
) -> Result<(Planned, Evidence), String> {
    let given = Path::new(&r.path);
    match library_row(layout, rows, r) {
        Some(row) => check_library(layout, row, r, archive_ok),
        // In a category folder of the archive, with no row of its own: a second copy (#423).
        None if given.parent().and_then(Path::parent) == Some(layout.archive.as_path()) => {
            check_archived_copy(layout, rows, r, archive_ok)
        }
        None => check_loose(layout, rows, r),
    }
}

/// Whether the entry `name` (`meta` its own metadata) is mod `tech` itself: a zip named after it,
/// or a folder of that name holding `modDesc.xml`.
fn is_the_mod(path: &Path, name: &str, meta: &std::fs::Metadata, tech: &str) -> bool {
    if meta.is_dir() {
        name.eq_ignore_ascii_case(tech) && path.join("modDesc.xml").is_file()
    } else {
        name.len() > 4
            && name[name.len() - 4..].eq_ignore_ascii_case(".zip")
            && name[..name.len() - 4].eq_ignore_ascii_case(tech)
    }
}

/// A second archived copy of a mod, with no library row of its own (#423: two copies of one mod
/// in the library, only one of them recorded): the named mod itself, really in a category folder
/// of the archive (not a link, the category folder not a link, the default archive not a link),
/// and no row's archived copy. It isn't loaded, so nothing is parked.
fn check_archived_copy(
    layout: &Layout,
    rows: &[OrganizedRow],
    r: &RemoveRequest,
    archive_ok: bool,
) -> Result<(Planned, Evidence), String> {
    if !archive_ok {
        return Err("your library archive isn't reachable, so Silo won't remove it.".into());
    }
    if layout.archive_is_default() && is_link(&layout.archive) != Some(false) {
        return Err(
            "your library archive folder is a link or can't be read, so Silo won't remove it."
                .into(),
        );
    }
    let path = PathBuf::from(&r.path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::paths::safe_file_name(&name)?;
    if rows
        .iter()
        .any(|row| archive_path(&layout.archive, &row.category, &row.file_name) == path)
    {
        return Err(format!(
            "{name} is another mod's copy in your library, so Silo left it alone."
        ));
    }
    let meta = path
        .symlink_metadata()
        .map_err(|_| format!("{name} is no longer there."))?;
    if meta.file_type().is_symlink() || !is_the_mod(&path, &name, &meta, &r.tech_name) {
        return Err(format!("{name} isn't that mod, so Silo left it alone."));
    }
    let category = path.parent().map(Path::to_path_buf).unwrap_or_default();
    really_at(&path, &category, Some(&layout.archive))?;
    // A recorded copy under another spelling (`vehicles` for `Vehicles`) is that row's, never a
    // second copy: compared by which file it is, not by how the path is written.
    let here = crate::scratch::entry_number(&path)
        .ok_or_else(|| format!("Silo couldn't check which file {name} is, so it left it alone."))?;
    for row in rows {
        let recorded = archive_path(&layout.archive, &row.category, &row.file_name);
        if crate::scratch::entry_number(&recorded) == Some(here) {
            return Err(format!(
                "{name} is the copy recorded in your library, so Silo left it. Remove it from its own entry."
            ));
        }
    }
    // The game may load this copy through Silo's link at its name: then it isn't the spare one.
    let slot = layout.mods.join(&name);
    match slot.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "Silo couldn't check {name} in your mods folder ({e}), so it left it alone."
            ))
        }
        Ok(_) => match projection_verdict(&slot, &path, if meta.is_dir() { "dir" } else { "zip" }) {
            Ok(false) => {}
            Ok(true) => {
                return Err(
                    "the game loads this copy, through Silo's link in your mods folder, so Silo left it. Keep this one, or park the mod first."
                .into())
            }
            Err(e) => {
                return Err(format!(
                    "Silo couldn't check whether the game loads {name} ({e}), so it left it alone."
                ))
            }
        },
    }
    let evidence = evidence_of(&path, &[&layout.mods, &layout.archive, &category])?;
    Ok((
        Planned {
            tech_name: r.tech_name.clone(),
            path: r.path.clone(),
            file_name: name,
            kind: evidence.kind.into(),
            on: false,
        },
        evidence,
    ))
}

fn check_library(
    layout: &Layout,
    row: &OrganizedRow,
    r: &RemoveRequest,
    archive_ok: bool,
) -> Result<(Planned, Evidence), String> {
    if !archive_ok {
        return Err("your library archive isn't reachable, so Silo won't remove it.".into());
    }
    if layout.archive_is_default() && is_link(&layout.archive) != Some(false) {
        return Err(
            "your library archive folder is a link or can't be read, so Silo won't remove it."
                .into(),
        );
    }
    let src = archive_path(&layout.archive, &row.category, &row.file_name);
    let category = src.parent().map(Path::to_path_buf).unwrap_or_default();
    really_at(&src, &category, Some(&layout.archive))?;
    let evidence = evidence_of(&src, &[&layout.mods, &layout.archive, &category])?;
    if evidence.kind != row.kind {
        return Err(format!(
            "{} isn't the kind of mod Silo filed, so Silo left it.",
            row.file_name
        ));
    }
    Ok((
        Planned {
            tech_name: row.tech_name.clone(),
            // As asked for, so the Library can match the result to its row.
            path: r.path.clone(),
            file_name: row.file_name.clone(),
            kind: row.kind.clone(),
            on: row.active,
        },
        evidence,
    ))
}

fn check_loose(
    layout: &Layout,
    rows: &[OrganizedRow],
    r: &RemoveRequest,
) -> Result<(Planned, Evidence), String> {
    let given = Path::new(&r.path);
    let name = given
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::paths::safe_file_name(&name)?;
    let path = layout.mods.join(&name);
    if path != given {
        return Err("it isn't in your mods folder or your library, so Silo left it alone.".into());
    }
    if RESERVED.iter().any(|x| name.eq_ignore_ascii_case(x)) || crate::scan::is_silo_temp(&name) {
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
    if !is_the_mod(&path, &name, &meta, &r.tech_name) {
        return Err(format!("{name} isn't that mod, so Silo left it alone."));
    }
    really_at(&path, &layout.mods, None)?;
    for row in rows
        .iter()
        .filter(|row| row.file_name.eq_ignore_ascii_case(&name))
    {
        let src = archive_path(&layout.archive, &row.category, &row.file_name);
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
    let evidence = evidence_of(&path, &[&layout.mods])?;
    Ok((
        Planned {
            tech_name: r.tech_name.clone(),
            path: path.to_string_lossy().into_owned(),
            file_name: name,
            kind: kind.into(),
            on: true, // a mod in the mods folder is one the game loads
        },
        evidence,
    ))
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

/// Whether `p` is a link (Some), or None when it can't be read.
fn is_link(p: &Path) -> Option<bool> {
    p.symlink_metadata()
        .ok()
        .map(|m| m.file_type().is_symlink())
}

/// `entry` is itself a real file or folder (not a link) whose folder, resolved, is `folder`
/// resolved. With `within`, `folder` must also not be a link and resolve directly inside it.
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

/// What `entry` is now (see [`Evidence`]), with `roots` resolved and numbered.
fn evidence_of(entry: &Path, roots: &[&Path]) -> Result<Evidence, String> {
    evidence_with(entry, roots, &crate::scratch::entry_number)
}

/// [`evidence_of`] with the file-number lookup given (a test passes one that fails). Every piece
/// is required: an entry or folder whose file number or modified time can't be read refuses, so a
/// missing piece can never compare equal to another missing one.
fn evidence_with(
    entry: &Path,
    roots: &[&Path],
    number_of: &dyn Fn(&Path) -> Option<(u64, u64)>,
) -> Result<Evidence, String> {
    let name = entry
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let refuse = |what: String| format!("Silo couldn't check {what}, so it left {name} alone.");
    let meta = entry
        .symlink_metadata()
        .map_err(|e| refuse(format!("{name} ({e})")))?;
    let stamp = |m: &std::fs::Metadata, what: &Path| -> Result<u64, String> {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .ok_or_else(|| refuse(format!("when {} changed", what.display())))
    };
    let number =
        |p: &Path| number_of(p).ok_or_else(|| refuse(format!("which file {} is", p.display())));
    let mut listing = Vec::new();
    if meta.is_dir() {
        for e in walkdir::WalkDir::new(entry)
            .follow_links(false)
            .min_depth(1)
            .sort_by_file_name()
        {
            let e = e.map_err(|e| refuse(format!("{name} ({e})")))?;
            let m = e.metadata().map_err(|e| refuse(format!("{name} ({e})")))?;
            let rel = e
                .path()
                .strip_prefix(entry)
                .unwrap_or(e.path())
                .to_string_lossy()
                .into_owned();
            listing.push((
                rel,
                m.is_dir(),
                if m.is_dir() { 0 } else { m.len() },
                stamp(&m, e.path())?,
                number(e.path())?,
            ));
        }
    }
    let mut numbered = Vec::new();
    for r in roots {
        let real =
            std::fs::canonicalize(r).map_err(|e| refuse(format!("{} ({e})", r.display())))?;
        let n = number(&real)?;
        numbered.push((real, Some(n)));
    }
    Ok(Evidence {
        entry: Some(number(entry)?),
        kind: if meta.is_dir() { "dir" } else { "zip" },
        size: if meta.is_dir() { 0 } else { meta.len() },
        mtime_ms: stamp(&meta, entry)?,
        listing,
        roots: numbered,
    })
}

#[cfg(test)]
mod evidence_tests {
    use super::*;

    #[test]
    fn evidence_without_a_file_number_refuses() {
        let d = std::env::temp_dir().join(format!("silo_evidence_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("FS25_Foo.zip");
        std::fs::write(&f, b"PK").unwrap();
        assert!(evidence_of(&f, &[&d]).is_ok());
        let none = |_: &Path| None;
        let e = evidence_with(&f, &[&d], &none).unwrap_err();
        assert!(e.starts_with("Silo couldn't check which file"), "{e}");
        // Only the folder's number missing refuses too.
        let only_entry = |p: &Path| (p == f.as_path()).then_some((1, 1));
        assert!(evidence_with(&f, &[&d], &only_entry).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
