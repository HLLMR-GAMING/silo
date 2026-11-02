//! Moving a mods folder's archive to a new location (#166). Each mod is recorded in the marker,
//! then copied to a temp name, verified, placed (never replacing anything), its link re-pointed,
//! and only then its old copy deleted. When both archives are on one drive each mod is renamed
//! into place instead (never over anything) and its link re-pointed at once (#401): the same
//! file, so there's nothing to verify or delete. Either way every mod boundary is a safe point
//! (the mods folder is complete and valid). Progress survives restarts through a marker in
//! `app_setting`; a move can pause (FS25 may be launched while paused), resume, or cancel —
//! which moves back exactly the mods Silo placed (`moved`), never anything else.

use crate::db::{self, OrganizedRow};
use crate::organize::{self, Layout, LinkPolicy};
use crate::scratch::{self, Journal};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

/// Suffix of an in-flight copy. `scan` skips entries ending in it — keep the two in step.
/// Each copy's name is its own (`scratch::unique_beside`), recorded before it's made.
const TMP_SUFFIX: &str = scratch::MOVE_TMP;
/// Suffix of an old copy being deleted (renamed first, so a half-deleted folder never keeps the
/// mod's real name). `scan` skips it too.
const OLD_SUFFIX: &str = scratch::MOVE_OLD;
/// Temp sibling for an atomic link replacement in the mods folder (organize's, already skipped
/// by `scan`).
const RELINK_SUFFIX: &str = organize::RELINK_SUFFIX;

const GAME_RUNNING: &str =
    "Close Farming Simulator 25 first. Silo won't move mods while the game is using them.";

/// Which way the mods are flowing: `Forward` = `from` → `to`; `Back` = a cancel, `to` → `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Forward,
    Back,
}

/// Persisted while a move is unfinished. `from` is the archive the move started from (home);
/// `to` is where it's going. Cancelling flips `direction`; pausing keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveMarker {
    pub from: PathBuf,
    pub to: PathBuf,
    pub direction: Direction,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub pause_reason: Option<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    /// techNames Silo has placed in `to` and not yet moved back. Cancel reverses only these.
    #[serde(default)]
    pub moved: Vec<String>,
    /// Category folders Silo created in `to` (by name), saved before each is made. A cancel
    /// removes those left empty — including ones made for a copy that was then abandoned —
    /// and no others. `None`: a marker from before Silo recorded them; a cancel then tidies
    /// the moved mods' categories, as it did then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<Vec<String>>,
    /// Mods-folder links that reached a mod right before Silo renamed it (#401), saved before
    /// the rename and dropped once re-pointed: a crash in between is mended from this, not from
    /// recognizing the link afterwards (`mend_renamed_links`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relinking: Vec<LinkNote>,
}

/// One `MoveMarker::relinking` entry: the mod, and the link's target as Silo read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkNote {
    pub tech_name: String,
    pub target: PathBuf,
}

impl MoveMarker {
    /// A fresh forward move.
    pub fn new(from: PathBuf, to: PathBuf) -> MoveMarker {
        MoveMarker {
            from,
            to,
            direction: Direction::Forward,
            paused: false,
            pause_reason: None,
            errors: Vec::new(),
            moved: Vec::new(),
            created: Some(Vec::new()),
            relinking: Vec::new(),
        }
    }
}

/// Prefix of every `archive_setting_key`; the rest of the key is the mods folder's key.
pub const ARCHIVE_SETTING_PREFIX: &str = "archive_root:";

/// `app_setting` key holding a mods folder's custom archive (absent ⇒ the default).
pub fn archive_setting_key(mods_key: &str) -> String {
    format!("{ARCHIVE_SETTING_PREFIX}{mods_key}")
}

fn marker_key(mods_key: &str) -> String {
    format!("archive_move:{mods_key}")
}

/// The unfinished move for a mods folder, if any.
pub fn load_marker(conn: &Connection, mods_key: &str) -> Option<MoveMarker> {
    db::get_app_setting(conn, &marker_key(mods_key)).and_then(|j| serde_json::from_str(&j).ok())
}

/// `load_marker`, but a marker Silo can't read or parse is an error, never "no move" (#415).
pub fn try_load_marker(conn: &Connection, mods_key: &str) -> Result<Option<MoveMarker>, String> {
    db::try_get_app_setting(conn, &marker_key(mods_key))?
        .map(|j| {
            serde_json::from_str(&j)
                .map_err(|e| format!("Silo can't read its record of the archive move: {e}"))
        })
        .transpose()
}

/// Persist the move marker (written before the first byte moves).
pub fn save_marker(conn: &Connection, mods_key: &str, m: &MoveMarker) -> Result<(), String> {
    let j = serde_json::to_string(m).map_err(|e| e.to_string())?;
    db::set_app_setting(conn, &marker_key(mods_key), Some(&j))
}

fn clear_marker(conn: &Connection, mods_key: &str) -> Result<(), String> {
    db::set_app_setting(conn, &marker_key(mods_key), None)
}

const PAUSE: u8 = 1;
const CANCEL: u8 = 2;

/// The command layer's pause/cancel requests (0 = none, 1 = pause, 2 = cancel). A move reads
/// whichever atomic its `RunCtx` names; production passes this one.
static REQUEST: AtomicU8 = AtomicU8::new(0);

/// The request flag the command layer drives (pass it as `RunCtx::request`).
pub fn requests() -> &'static AtomicU8 {
    &REQUEST
}

/// Ask the running move to pause at the next mod boundary (an in-flight copy is abandoned).
pub fn request_pause() {
    REQUEST.store(PAUSE, Ordering::SeqCst);
}

/// Ask the running move to turn around and move Silo's moved mods back.
pub fn request_cancel() {
    REQUEST.store(CANCEL, Ordering::SeqCst);
}

/// One move at a time, process-wide.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// A move is copying right now (not paused) — Launch is blocked.
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// Holds `RUNNING` for one `start` / `resume` / `cancel`; released on every exit path, including
/// a panic unwinding.
struct RunGuard;

impl RunGuard {
    fn take() -> Result<RunGuard, String> {
        RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| RunGuard)
            .map_err(|_| "An archive move is already running.".to_string())
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Emitted as the move goes (`archive-move-progress`).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: String,
    pub cancelling: bool,
}

/// Why a single mod's step stopped.
#[derive(Debug, PartialEq, Eq)]
enum StepErr {
    /// A pause/cancel request or the game starting — handle it at the boundary, then retry.
    Interrupted,
    /// A copy wouldn't fit on its drive (checked before it starts): the move pauses, saying so.
    NoRoom(String),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Moved,
    AlreadyDone,
}

fn exists(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// Both paths are the same file on disk (a hardlink pair, or one path twice). Never errors:
/// anything unreadable is "not the same".
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    matches!(
        (same_file::Handle::from_path(a), same_file::Handle::from_path(b)),
        (Ok(x), Ok(y)) if x == y
    )
}

/// Size of a mod on disk (file, or a folder recursively). Never follows links.
pub(crate) fn size_of(p: &Path) -> u64 {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum())
            .unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

fn remove_any(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(_) => Ok(()),
    }
}

fn link_inside(p: &Path) -> String {
    format!(
        "{} is a link inside a mod folder. Silo won't move links; remove it, then retry.",
        p.display()
    )
}

/// Copy a file in chunks, giving up (the caller removes the partial) when `abort()` turns true. The
/// copy keeps the original's modified time, so a moved mod keeps its date (and the scan cache,
/// keyed by path+mtime+size, isn't fooled into thinking it changed).
fn copy_file_abortable(src: &Path, dst: &Path, abort: &dyn Fn() -> bool) -> Result<(), StepErr> {
    let io = |e: std::io::Error| StepErr::Failed(format!("copy: {e}"));
    let mut r = std::fs::File::open(src).map_err(io)?;
    let modified = r.metadata().and_then(|m| m.modified()).ok();
    let mut w = std::fs::File::create(dst).map_err(io)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if abort() {
            // The partial copy is the caller's to clean up, once it's proven redundant.
            return Err(StepErr::Interrupted);
        }
        let n = r.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n]).map_err(io)?;
    }
    // Best effort: a filesystem that can't set times still gets a correct copy.
    if let Some(t) = modified {
        let _ = w.set_modified(t);
    }
    w.sync_all().map_err(io)
}

/// Copy a mod folder. Links inside it are refused (never followed).
fn copy_dir_abortable(src: &Path, dst: &Path, abort: &dyn Fn() -> bool) -> Result<(), StepErr> {
    let io = |e: std::io::Error| StepErr::Failed(format!("copy: {e}"));
    std::fs::create_dir_all(dst).map_err(io)?;
    for e in std::fs::read_dir(src).map_err(io)? {
        let e = e.map_err(io)?;
        let ft = e.file_type().map_err(io)?;
        let to = dst.join(e.file_name());
        if ft.is_symlink() {
            return Err(StepErr::Failed(format!("copy: {}", link_inside(&e.path()))));
        } else if ft.is_dir() {
            copy_dir_abortable(&e.path(), &to, abort)?;
        } else {
            copy_file_abortable(&e.path(), &to, abort)?;
        }
    }
    Ok(())
}

/// Every file of a mod folder as relative path → size. Never follows links; a link is an error.
fn listing(root: &Path) -> Result<BTreeMap<PathBuf, u64>, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, u64>) -> Result<(), String> {
        for e in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            let ft = e.file_type().map_err(|e| e.to_string())?;
            let p = e.path();
            if ft.is_symlink() {
                return Err(link_inside(&p));
            } else if ft.is_dir() {
                walk(root, &p, out)?;
            } else {
                let size = e.metadata().map_err(|e| e.to_string())?.len();
                let rel = p.strip_prefix(root).unwrap_or(&p).to_path_buf();
                out.insert(rel, size);
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

/// How closely [`verify`] compares a mod folder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Check {
    /// The same relative paths with the same sizes: enough right after Silo copied the folder.
    Listing,
    /// Also the same SHA-256 per file. For a copy found from an earlier run (a resume after a
    /// pause or crash), where the original may have been edited in between, keeping its sizes.
    Contents,
}

/// Same content? Zips: same size, then SHA-256. Folders: per `check`. `Err` when either side
/// can't be read (or a folder holds a link).
fn verify(src: &Path, dst: &Path, kind: &str, check: Check) -> Result<bool, String> {
    let fail = |e: String| format!("verify: {e}");
    if kind == "dir" {
        if !dst.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false) {
            return Ok(false);
        }
        let files = listing(src).map_err(fail)?;
        if files != listing(dst).map_err(fail)? {
            return Ok(false);
        }
        if check == Check::Contents {
            for rel in files.keys() {
                let sa = crate::provenance::sha256_file(&src.join(rel)).map_err(fail)?;
                let sb = crate::provenance::sha256_file(&dst.join(rel)).map_err(fail)?;
                if sa != sb {
                    return Ok(false);
                }
            }
        }
        return Ok(true);
    }
    let (a, b) = match (
        std::fs::symlink_metadata(src),
        std::fs::symlink_metadata(dst),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return Err(fail(e.to_string())),
    };
    if !a.is_file() || !b.is_file() || a.len() != b.len() {
        return Ok(false);
    }
    let sa = crate::provenance::sha256_file(src).map_err(fail)?;
    let sb = crate::provenance::sha256_file(dst).map_err(fail)?;
    Ok(sa == sb)
}

/// Put a verified temp copy at `dst` without ever replacing anything there. Zips: hardlink the
/// temp to `dst` (fails if `dst` exists), then drop the temp. On a filesystem without hardlinks,
/// and for folders, `paths::rename_no_replace`, which fails rather than replace `dst`.
fn place(tmp: &Path, dst: &Path, kind: &str) -> Result<(), String> {
    let taken = || {
        format!(
            "place: something appeared at {}. Move or rename it, then retry.",
            dst.display()
        )
    };
    if kind != "dir" {
        match std::fs::hard_link(tmp, dst) {
            Ok(()) => {
                let _ = std::fs::remove_file(tmp);
                crate::fslog::moved("place", tmp, dst);
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(taken()),
            Err(_) => {} // no hardlinks on this filesystem: no-replace rename below
        }
    }
    match crate::paths::rename_no_replace(tmp, dst) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(taken()),
        Err(e) => return Err(format!("place: {e}")),
    }
    crate::fslog::moved("place", tmp, dst);
    Ok(())
}

/// Copy `src` to a temp sibling of `dst`, verify the temp against `src`, then place it. The temp
/// has a name of its own, recorded in the scratch journal before it's made and made exclusively
/// (never over anything); a failure only ever removes that temp — never a file at the real
/// name, nor anything Silo didn't make. A copy that wouldn't fit on `dst`'s drive stops before
/// it starts (`StepErr::NoRoom`).
fn copy_verify_place(
    env: &Env,
    src: &Path,
    dst: &Path,
    kind: &str,
    abort: &dyn Fn() -> bool,
) -> Result<(), StepErr> {
    env.room(size_of(src), dst).map_err(StepErr::NoRoom)?;
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).map_err(|e| StepErr::Failed(format!("copy: {e}")))?;
    }
    let journal = env.journal();
    let tmp = scratch::begin(
        &journal,
        scratch::Role::MoveTmp,
        dst,
        TMP_SUFFIX,
        src,
        dst,
        kind,
    )
    .map_err(|e| StepErr::Failed(format!("copy: {e}")))?;
    // Made exclusively: something already at this new name isn't Silo's and is left alone.
    let made = if kind == "dir" {
        std::fs::create_dir(&tmp)
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map(drop)
    };
    if let Err(e) = made {
        journal.forget(&tmp);
        return Err(StepErr::Failed(format!("copy: {e}")));
    }
    let res = copy_to_temp_and_place(src, &tmp, dst, kind, abort);
    if res.is_err() {
        // Removed only while the original still holds every byte of it (the original could
        // have changed mid-copy); otherwise kept and recorded for a later sweep.
        scratch::discard(
            Some(&journal),
            &scratch::Entry {
                path: tmp.clone(),
                role: scratch::Role::MoveTmp,
                from: src.to_path_buf(),
                to: dst.to_path_buf(),
                kind: kind.to_string(),
                made: None,
            },
        );
    }
    // Placed (renamed, or hardlinked and dropped) or removed: the record goes. A temp still
    // there stays recorded for `scratch::sweep_where`.
    scratch::settle(&journal, &tmp);
    res
}

fn copy_to_temp_and_place(
    src: &Path,
    tmp: &Path,
    dst: &Path,
    kind: &str,
    abort: &dyn Fn() -> bool,
) -> Result<(), StepErr> {
    if kind == "dir" {
        copy_dir_abortable(src, tmp, abort)?;
    } else {
        copy_file_abortable(src, tmp, abort)?;
    }
    if !verify(src, tmp, kind, Check::Listing).map_err(StepErr::Failed)? {
        return Err(StepErr::Failed(
            "verify: the copy didn't match the original".into(),
        ));
    }
    place(tmp, dst, kind).map_err(StepErr::Failed)
}

/// How a mod got to its new place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    /// Renamed on one drive: the very same file or folder, so nothing is left to verify or delete.
    Renamed,
    /// Copied and verified: the original is still there, deleted once the link moves.
    Copied,
}

/// The OS error a rename gives when its two paths are on different drives (`EXDEV`, Windows'
/// `ERROR_NOT_SAME_DEVICE`).
#[cfg(unix)]
const CROSS_DEVICE: i32 = libc::EXDEV;
#[cfg(windows)]
const CROSS_DEVICE: i32 = 17;

/// The rename failed only because its paths are on different drives after all.
fn crosses_devices(e: &std::io::Error) -> bool {
    #[cfg(any(unix, windows))]
    {
        e.raw_os_error() == Some(CROSS_DEVICE)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

/// Put `src` at `dst`, never over anything. Where both archives are on one drive
/// (`Env::same_volume`) it's renamed: no copy, no extra space, and a mods-folder hardlink to it
/// stays valid (the same file). A folder mod with a link inside is refused first, as the copy
/// refuses it. Otherwise, or when the rename finds the two on different drives after all, it's
/// copied and verified (`copy_verify_place`), as across drives. Any other rename failure (a
/// file in use, no permission) stops the step: copying around it would only fail later, at the
/// delete, with two copies left.
fn rename_or_copy(
    env: &Env,
    src: &Path,
    dst: &Path,
    kind: &str,
    abort: &dyn Fn() -> bool,
) -> Result<Placed, StepErr> {
    if env.same_volume {
        if kind == "dir" {
            listing(src).map_err(|e| StepErr::Failed(format!("move: {e}")))?;
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| StepErr::Failed(format!("move: {e}")))?;
        }
        match (env.rename)(src, dst) {
            Ok(()) => {
                crate::fslog::moved("rename-moved", src, dst);
                return Ok(Placed::Renamed);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(occupied(dst)),
            Err(e) if crosses_devices(&e) => log::info!(
                "{} and {} are on different drives after all, so Silo copies it",
                src.display(),
                dst.display()
            ),
            Err(e) => {
                return Err(StepErr::Failed(format!(
                    "move: Silo couldn't move {} to {}: {e}. Close anything using it, then retry.",
                    src.display(),
                    dst.display()
                )))
            }
        }
    }
    copy_verify_place(env, src, dst, kind, abort).map(|()| Placed::Copied)
}

/// Write a rename and its re-link to disk before anything relies on them (an unrecord, a
/// dropped link note, the switch at the end of the move). Checked (`organize::sync_dir_checked`,
/// errors on Windows too): the folder the mod landed in and the one above it (a category Silo
/// made there), the folder it left (or its nearest existing parent when that isn't there:
/// never made, for a mod recorded but not renamed yet, or tidied away), and the mods folder
/// (its link). Folders further up as for a copy (`make_durable`). Nothing inside the mod
/// changed, so no file is flushed.
fn flush_rename(placed: &Path, left: &Path, mods: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if tests::FLUSH_FAULT.with(|f| f.get()) {
        return Err(std::io::Error::other("injected flush failure"));
    }
    let mut above = placed.ancestors().skip(1);
    for dir in above.by_ref().take(2) {
        organize::sync_dir_checked(dir)?;
    }
    for dir in above {
        organize::sync_dir(dir)?;
    }
    if let Some(dir) = left
        .ancestors()
        .skip(1)
        .find(|d| d.symlink_metadata().is_ok())
    {
        organize::sync_dir_checked(dir)?;
    }
    organize::sync_dir_checked(mods)
}

/// `Env::flush` as a step error.
fn flushed(env: &Env, placed: &Path, left: &Path) -> Result<(), StepErr> {
    (env.flush)(placed, left, env.mods).map_err(|e| {
        StepErr::Failed(format!(
            "move: Silo couldn't write the move of {} to disk: {e}. Check that drive, then resume.",
            placed.display()
        ))
    })
}

/// An active mod's mods-folder entry is a link that reaches `src` (however it's spelled):
/// asked right before `src` is renamed, while it still resolves.
fn links_to(env: &Env, row: &OrganizedRow, src: &Path) -> bool {
    let link = env.mods.join(&row.file_name);
    row.active
        && link
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        && resolves_to(&link, src)
}

/// `link` and `p` resolve to the same place.
fn resolves_to(link: &Path, p: &Path) -> bool {
    matches!(
        (std::fs::canonicalize(link), std::fs::canonicalize(p)),
        (Ok(a), Ok(b)) if a == b
    )
}

/// What's at a mods-folder entry, for a link note (`mend_renamed_links`).
enum Entry {
    Missing,
    NotALink,
    Link(PathBuf),
}

/// Look at the entry `link` without following it. `Err` only when it can't be read (the note
/// is then kept): a missing entry or another kind of entry is an answer.
fn inspect_entry(link: &Path) -> std::io::Result<Entry> {
    #[cfg(test)]
    if tests::INSPECT_FAULT.with(|f| f.take()) {
        return Err(std::io::Error::other("injected read failure"));
    }
    match link.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Entry::Missing),
        Err(e) => Err(e),
        Ok(m) if !m.file_type().is_symlink() => Ok(Entry::NotALink),
        Ok(_) => std::fs::read_link(link).map(Entry::Link),
    }
}

/// Point the link entry `link` at `to`, whatever it names (`from` is where the mod was): for a
/// link proven to reach the mod before its rename (`links_to`, `MoveMarker::relinking`) that
/// `repoint` can't recognize now. It must reach the copy afterwards.
fn relink_entry(
    env: &Env,
    row: &OrganizedRow,
    link: &Path,
    to: &Path,
    from: &Path,
    policy: &mut LinkPolicy,
) -> Result<(), StepErr> {
    if row.kind == "dir" {
        organize::replace_dir_link(link, to, from, policy).map_err(relink_err)?;
        return reaches_the_copy(link, &row.kind).map_err(StepErr::Failed);
    }
    replace_zip_link(env, row, link, to, policy)
}

/// Right after `src` was renamed to `dst`: re-point the mod's mods-folder entry at once (no
/// pause in between, so a boundary never leaves a link to the old place), then flush the rename
/// (`Env::flush`). `linked` (`Env::note_link`, asked before the rename): the entry was a link
/// reaching `src`, so one `repoint` left dangling is re-pointed anyway (`relink_entry`). If the re-point fails, the
/// mod is renamed back and its entry re-pointed back at `src` (whatever the failed re-point
/// left: a missing entry is made again), then flushed: `Err((_, true))`, nothing changed. If any
/// of that fails it stays where it ended up and the marker keeps it, so Resume or Cancel mends
/// it: `Err((_, false))`. A failed flush after a good re-point is also `Err((_, false))`.
fn relink_renamed(
    env: &Env,
    row: &OrganizedRow,
    src: &Path,
    dst: &Path,
    linked: bool,
    policy: &mut LinkPolicy,
) -> Result<(), (StepErr, bool)> {
    let link = env.mods.join(&row.file_name);
    let relinked = repoint(env, row, src, dst, policy).and_then(|()| {
        if linked && reaches_the_copy(&link, &row.kind).is_err() {
            // It reached the mod before the rename, but isn't recognized now (spelled through
            // the mod's own name, say): re-pointed on that proof.
            relink_entry(env, row, &link, dst, src, policy)?;
        }
        Ok(())
    });
    if let Err(e) = relinked {
        let restored = (env.rename)(dst, src).is_ok() && {
            crate::fslog::moved("rename-back", dst, src);
            repoint(env, row, dst, src, policy).is_ok() && flushed(env, src, dst).is_ok()
        };
        return Err((e, restored));
    }
    flushed(env, dst, src).map_err(|e| (e, false))
}

/// A crash between a rename and its re-link leaves that mod's mods-folder entry naming the
/// place it left (a symlink or junction), or missing (Windows' `replace_dir_link` removes the
/// old folder link before the new one goes in, on the copy path too). Before the first boundary
/// can pause the move, every such entry of a recorded active mod is re-pointed at (or made
/// again for) whichever of its two places holds the mod now, when that is the real copy (with
/// both there and the same, a missing entry is made as the step would make it). Runs on every run,
/// whatever the drives look like now: the work it mends was recorded earlier. Returns what
/// couldn't be mended, phrased like a step's errors: the run then pauses at once, listing them.
fn mend_renamed_links(env: &Env, marker: &mut MoveMarker) -> Vec<String> {
    let rows = db::load_organized(env.conn, env.mods_key);
    let journal = env.journal();
    let room = |src: &Path, entry: &Path| {
        env.room(size_of(src), entry)
            .map_err(|why| std::io::Error::other(organize::NoRoom(why)))
    };
    let mut errors = Vec::new();
    // The links noted before a rename (`Env::note_link`): only the very link noted, still
    // dangling, is re-pointed (anything else at its name isn't Silo's to change now); then the
    // note goes. One that can't be re-pointed yet is kept.
    let notes = std::mem::take(&mut marker.relinking);
    let noted = notes.len();
    // Notes with nothing left to vouch for: they go only once the state they leave behind is on
    // disk (`settle_note`, after the generic pass below), whoever made it.
    let mut retiring = Vec::new();
    for note in notes {
        let Some(row) = rows.iter().find(|r| r.tech_name == note.tech_name) else {
            continue;
        };
        let link = env.mods.join(&row.file_name);
        match inspect_entry(&link) {
            Ok(Entry::Link(t)) if t == note.target => {}
            // Gone (the generic pass below makes it again), or not the link Silo noted (perhaps
            // re-pointed by an earlier attempt): the note has nothing left to vouch for.
            Ok(_) => {
                retiring.push(note);
                continue;
            }
            Err(e) => {
                errors.push(format!("{}: relink: {e}", row.tech_name));
                marker.relinking.push(note);
                continue;
            }
        }
        if reaches_the_copy(&link, &row.kind).is_ok() {
            retiring.push(note);
            continue;
        }
        let a = organize::archive_path(&marker.from, &row.category, &row.file_name);
        let b = organize::archive_path(&marker.to, &row.category, &row.file_name);
        let (gone, here, archive) = match (exists(&a), exists(&b)) {
            (false, true) => (a, b, &marker.to),
            (true, false) => (b, a, &marker.from),
            (both, _) => {
                errors.push(format!(
                    "{}: relink: its link at {} reaches nothing, and Silo can't tell which copy it should reach ({}). Check both archive folders, then resume.",
                    row.tech_name,
                    link.display(),
                    if both { "there's one in each" } else { "there's none in either" }
                ));
                marker.relinking.push(note);
                continue;
            }
        };
        if real_copy(&here, &row.kind).is_none() {
            errors.push(format!(
                "{}: relink: its link at {} reaches nothing, and {} isn't the mod's copy (it's a link or something else). Check it, then resume.",
                row.tech_name,
                link.display(),
                here.display()
            ));
            marker.relinking.push(note);
            continue;
        }
        let layout = Layout {
            mods: env.mods.to_path_buf(),
            archive: archive.clone(),
        };
        let mut policy = LinkPolicy::new(&layout)
            .with_journal(&journal)
            .with_room_check(&room);
        match relink_entry(env, row, &link, &here, &gone, &mut policy) {
            Ok(()) => retiring.push(note),
            Err(e) => {
                errors.push(format!("{}: {}", row.tech_name, failed(e)));
                marker.relinking.push(note);
            }
        }
    }
    for row in rows
        .iter()
        .filter(|r| r.active && marker.moved.contains(&r.tech_name))
    {
        let link = env.mods.join(&row.file_name);
        let missing = match link.symlink_metadata() {
            Ok(m) if m.file_type().is_symlink() => false,
            Ok(_) => continue, // a hardlink or the mod's own copy: a rename never breaks it
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => {
                errors.push(format!("{}: relink: {e}", row.tech_name));
                continue;
            }
        };
        // A missing entry the mend can't make again is listed: the move pauses saying so.
        let cant = |errors: &mut Vec<String>, why: &str| {
            if missing {
                errors.push(format!(
                    "{}: relink: its entry in your mods folder is missing, and Silo can't make it again ({why}). Check both archive folders, then resume.",
                    row.tech_name
                ));
            }
        };
        let a = organize::archive_path(&marker.from, &row.category, &row.file_name);
        let b = organize::archive_path(&marker.to, &row.category, &row.file_name);
        let (gone, here, archive) = match (exists(&a), exists(&b)) {
            (false, true) => (a, b, &marker.to),
            (true, false) => (b, a, &marker.from),
            // Both (a copy not yet deleted): a missing entry is made again as the step would
            // make it, for the copy it moves to and with that archive's links, once the two
            // copies are proven the same (every file's SHA-256); copies that differ are listed
            // before either is linked. A link is the step's to sort out.
            (true, true) if missing => {
                let (from, to, archive) = match marker.direction {
                    Direction::Forward => (a, b, &marker.to),
                    Direction::Back => (b, a, &marker.from),
                };
                match verify(&from, &to, &row.kind, Check::Contents) {
                    Ok(true) => (from, to, archive),
                    Ok(false) => {
                        cant(
                            &mut errors,
                            &format!("{} and {} differ", from.display(), to.display()),
                        );
                        continue;
                    }
                    Err(e) => {
                        errors.push(format!("{}: relink: {e}", row.tech_name));
                        continue;
                    }
                }
            }
            (true, true) => continue,
            (false, false) => {
                cant(&mut errors, "neither archive holds it");
                continue;
            }
        };
        if real_copy(&here, &row.kind).is_none() {
            cant(
                &mut errors,
                &format!("{} isn't the mod's copy", here.display()),
            );
            continue;
        }
        let layout = Layout {
            mods: env.mods.to_path_buf(),
            archive: archive.clone(),
        };
        let mut policy = LinkPolicy::new(&layout)
            .with_journal(&journal)
            .with_room_check(&room);
        if let Err(e) = repoint(env, row, &gone, &here, &mut policy) {
            errors.push(format!("{}: {}", row.tech_name, failed(e)));
        }
    }
    for note in retiring {
        let Some(row) = rows.iter().find(|r| r.tech_name == note.tech_name) else {
            continue;
        };
        if let Err(e) = settle_note(env, row, marker) {
            errors.push(format!("{}: {}", row.tech_name, failed(e)));
            marker.relinking.push(note);
        }
    }
    if marker.relinking.len() != noted {
        if let Err(e) = env.save(marker) {
            errors.push(failed(e));
        }
    }
    errors
}

/// Before a link note goes: the mod's link and its archived copy, as they are now, are on disk
/// (`Env::flush` for whichever of its two places holds it, with the mods folder), so a power
/// cut can't bring back the state the note was kept for without the note.
fn settle_note(env: &Env, row: &OrganizedRow, marker: &MoveMarker) -> Result<(), StepErr> {
    let a = organize::archive_path(&marker.from, &row.category, &row.file_name);
    let b = organize::archive_path(&marker.to, &row.category, &row.file_name);
    let (placed, left) = if !exists(&a) && exists(&b) {
        (b, a)
    } else {
        (a, b)
    };
    if exists(&placed) {
        return flushed(env, &placed, &left);
    }
    organize::sync_dir_checked(env.mods).map_err(|e| {
        StepErr::Failed(format!(
            "relink: Silo couldn't write your mods folder's links to disk: {e}. Check that drive, then resume."
        ))
    })
}

/// Delete a moved mod's old copy `src`, now that `replacement` holds it: rename it aside to a
/// new name of its own (recorded in the scratch journal first, never over anything), so the
/// mod's name is gone at once; check `replacement` is still in place against it; only then
/// remove it. If the check fails it's put back under its name. A crash mid-delete leaves only
/// the recorded old copy, which `scratch::sweep_where` removes on the next start once
/// `replacement` verifies against it (a zip whose replacement is gone is put back instead).
fn delete_source(
    env: &Env,
    src: &Path,
    replacement: &Path,
    root: &Path,
    kind: &str,
) -> Result<(), StepErr> {
    let fail = |e: String| StepErr::Failed(format!("remove old copy: {e}"));
    // The replacement's placement is on disk first: a power cut can't keep the delete without
    // the copy.
    make_durable(replacement, root).map_err(|e| fail(e.to_string()))?;
    let journal = env.journal();
    let old = scratch::begin(
        &journal,
        scratch::Role::MoveOld,
        src,
        OLD_SUFFIX,
        src,
        replacement,
        kind,
    )
    .map_err(fail)?;
    if let Err(e) = crate::paths::rename_no_replace(src, &old) {
        journal.forget(&old);
        return Err(fail(e.to_string()));
    }
    // The last look before the old copy goes: the replacement is still the real thing and
    // holds the same bytes (every file's SHA-256, not just sizes).
    let checked = still_there(&old, replacement, kind).and_then(|()| {
        match verify(&old, replacement, kind, Check::Contents) {
            Ok(true) => Ok(()),
            Ok(false) => Err(StepErr::Failed(format!(
                "move: {} and {} no longer match, so Silo kept both. Check them, then resume.",
                src.display(),
                replacement.display()
            ))),
            Err(e) => Err(StepErr::Failed(e)),
        }
    });
    if let Err(e) = checked {
        if crate::paths::rename_no_replace(&old, src).is_ok() {
            journal.forget(&old);
        }
        return Err(e);
    }
    let removed = remove_any(&old);
    scratch::settle(&journal, &old);
    removed.map_err(|e| fail(e.to_string()))?;
    crate::fslog::changed("delete-moved", src);
    Ok(())
}

/// Flush a placed copy and every folder entry its placement made to disk: inside a folder mod,
/// up to the archive `root`, and every folder above it (the move may have created the archive
/// folder itself, and folders above it).
fn make_durable(p: &Path, root: &Path) -> std::io::Result<()> {
    fn subdirs(dir: &Path) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            if e.file_type()?.is_dir() {
                subdirs(&e.path())?;
                organize::sync_dir(&e.path())?;
            }
        }
        Ok(())
    }
    if p.symlink_metadata()?.is_dir() {
        subdirs(p)?;
    }
    organize::sync_placement(p, Some(root))?;
    for dir in root.ancestors().skip(1) {
        organize::sync_dir(dir)?;
    }
    Ok(())
}

/// Keep an active mod's mods-folder entry valid as its archived copy moves from `src` to `dst`:
/// a missing entry gets a fresh link to `dst`; a symlink into `src` is re-pointed (zips by an
/// atomic replace; folder links are removed and re-made, restoring the old link if that fails);
/// a zip hardlinked to `src` is atomically replaced by a hardlink or symlink to `dst` —
/// otherwise it would keep the old copy's bytes alive after that copy is deleted, and the move
/// would free nothing (where the new link might be a copy it's kept as the standalone copy
/// instead, see `LinkPolicy::can_only_copy`).
/// Copied zips, marker-copied folders and the user's own files are standalone and left alone;
/// a link to anything else is not ours to change.
fn repoint(
    env: &Env,
    row: &OrganizedRow,
    src: &Path,
    dst: &Path,
    policy: &mut LinkPolicy,
) -> Result<(), StepErr> {
    if !row.active {
        return Ok(());
    }
    let link = env.mods.join(&row.file_name);
    #[cfg(test)]
    if let Some(fault) = tests::REPOINT_FAULT.with(|f| f.take()) {
        fault(&link);
        return Err(StepErr::Failed("relink: injected failure".into()));
    }
    let Ok(meta) = link.symlink_metadata() else {
        organize::make_link(dst, &link, &row.kind, policy).map_err(relink_err)?;
        // A link that reaches nothing is left as it is (removing it could race whatever
        // replaces it); the retry checks it again before the old copy may go.
        reaches_the_copy(&link, &row.kind).map_err(StepErr::Failed)?;
        env.note_projection(row, &link);
        return Ok(());
    };
    if !meta.file_type().is_symlink() {
        // Only a hardlink of the very file being moved — provably Silo's projection. It's
        // swapped for a real link to the new copy (a hardlink, or a symlink where allowed) —
        // never for a copy. Where no link is known to work (`can_only_copy`: another drive
        // without symlinks, a mods folder that rejects hardlinks, a drive Silo can't identify),
        // or the link fails anyway, it's kept: once the old copy is deleted it simply becomes
        // the mod's standalone copy, with no second copy made in the mods folder (and its date
        // kept). `check_move` counts the same cases.
        if row.kind != "dir" && meta.is_file() && same_file(&link, src) && !policy.can_only_copy() {
            let linked =
                organize::replace_zip_link_if_linkable(&link, dst, policy).map_err(relink_err)?;
            if linked {
                env.note_projection(row, &link);
            }
        }
        return Ok(());
    }
    if !stale_symlink(&link, src).map_err(relink_err)? {
        // Already pointing at the new copy (a step that stopped after re-linking, now retried):
        // it must still reach it before the old copy may go. Any other link isn't ours.
        if stale_symlink(&link, dst).unwrap_or(false) {
            return reaches_the_copy(&link, &row.kind).map_err(StepErr::Failed);
        }
        return Ok(());
    }
    if row.kind == "dir" {
        // The new link is made beside the old one, then renamed over it where the OS allows
        // (atomic); elsewhere the old one goes first and a crash between heals on Resume (the
        // missing link is made again). See `organize::replace_dir_link`.
        organize::replace_dir_link(&link, dst, src, policy).map_err(relink_err)?;
        // A link that reaches nothing is left as it is (removing it could race whatever
        // replaces it); the step stops before the old copy goes, and the retry checks again.
        return reaches_the_copy(&link, &row.kind).map_err(StepErr::Failed);
    }
    replace_zip_link(env, row, &link, dst, policy)
}

/// A relink's I/O error as a step error: a copy that wouldn't fit (`organize::NoRoom`) pauses
/// the move; anything else fails the step.
fn relink_err(e: std::io::Error) -> StepErr {
    match organize::no_room(&e) {
        Some(why) => StepErr::NoRoom(why.to_string()),
        None => StepErr::Failed(format!("relink: {e}")),
    }
}

/// A symlink entry `link` that `repoint` makes again: it points at `src`, the copy being moved
/// — the same path (a relative target read from the link's folder, compared as an identity
/// key), the same file once both resolve, or the same name in the same real folder (which still
/// holds once `src` was renamed away). A link to anything else, dangling or not, is not ours to
/// change.
fn stale_symlink(link: &Path, src: &Path) -> std::io::Result<bool> {
    let target = std::fs::read_link(link)?;
    if target == src {
        return Ok(true);
    }
    let resolved = match link.parent() {
        Some(dir) if target.is_relative() => dir.join(&target),
        _ => target,
    };
    let key = |p: &Path| crate::modsroot::normalize_key(&p.to_string_lossy(), cfg!(windows));
    // Where `p`'s folder really is, with its name: still known once `p` itself was renamed
    // away (#401), for a target spelled through `..`, through another link to its folder, or
    // through links to the mod itself (followed to their end).
    let in_folder = |p: &Path| {
        let dir = std::fs::canonicalize(p.parent()?).ok()?;
        Some(key(&dir.join(p.file_name()?)))
    };
    Ok(key(&resolved) == key(src)
        || matches!(
            (std::fs::canonicalize(link), std::fs::canonicalize(src)),
            (Ok(a), Ok(b)) if a == b
        )
        || matches!(
            (chain_end(link).and_then(|e| in_folder(&e)), in_folder(src)),
            (Some(a), Some(b)) if a == b
        ))
}

/// The most links `chain_end` follows: more than any system resolves in one path (Linux 40,
/// Windows 63).
const MAX_CHAIN: usize = 64;

/// Where the chain of links that starts at `link` ends, followed one link at a time (at most
/// `MAX_CHAIN`) so an end that's gone still has a path. `None` for a longer chain (a loop) or an
/// unreadable link.
fn chain_end(link: &Path) -> Option<PathBuf> {
    let mut p = link.to_path_buf();
    for _ in 0..MAX_CHAIN {
        if !p
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Some(p);
        }
        let t = std::fs::read_link(&p).ok()?;
        p = match p.parent() {
            Some(d) if t.is_relative() => d.join(t),
            _ => t,
        };
    }
    // The last link's target, if it's not a link itself.
    (!p.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink()))
    .then_some(p)
}

/// What a move's re-links could cost in space, by case (`repoint`). Bytes, from the archived
/// copies in `archive`. Whether each case costs anything depends on the new archive
/// (`check_move`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelinkCost {
    /// Active zips hardlinked to their archived copy. Where no real link to the new copy can be
    /// made, `repoint` keeps that hardlink as the mod's standalone copy, so deleting the old
    /// copy frees nothing: on one drive these stay used alongside their new copies.
    pub kept: u64,
    /// Active zips whose mods-folder entry is missing or a symlink to the archived copy:
    /// `repoint` makes each again — a full copy in the mods folder where only a copy can be
    /// made (`LinkPolicy::can_only_copy`).
    pub copied_zips: u64,
    /// Active folder mods whose entry is missing or a symlink to the archived copy: made again,
    /// a full copy where a folder link may not work (`organize::dir_link_may_copy`).
    pub copied_dirs: u64,
    /// The largest mod in `rows` (active or not) that isn't counted in `kept`.
    pub largest_unkept: u64,
    /// The largest single entry in `copied_zips` / `copied_dirs`.
    pub largest_copied_zip: u64,
    pub largest_copied_dir: u64,
}

/// `RelinkCost` of the active mods in `rows`, with their mods-folder entries in `mods` and
/// archived copies in `archive` — the same cases `repoint` tells apart. Read-only.
pub fn relink_cost(mods: &Path, archive: &Path, rows: &[OrganizedRow]) -> RelinkCost {
    let mut cost = RelinkCost::default();
    for row in rows {
        let src = organize::archive_path(archive, &row.category, &row.file_name);
        let size = size_of(&src);
        if !row.active {
            cost.largest_unkept = cost.largest_unkept.max(size);
            continue;
        }
        let link = mods.join(&row.file_name);
        let dir = row.kind == "dir";
        let made_again = match link.symlink_metadata() {
            Err(_) => true,
            Ok(m) if m.file_type().is_symlink() => stale_symlink(&link, &src).unwrap_or(false),
            Ok(m) => {
                if !dir && m.is_file() && same_file(&link, &src) {
                    cost.kept += size;
                    continue;
                }
                false
            }
        };
        cost.largest_unkept = cost.largest_unkept.max(size);
        match (made_again, dir) {
            (true, true) => {
                cost.copied_dirs += size;
                cost.largest_copied_dir = cost.largest_copied_dir.max(size);
            }
            (true, false) => {
                cost.copied_zips += size;
                cost.largest_copied_zip = cost.largest_copied_zip.max(size);
            }
            _ => {}
        }
    }
    cost
}

/// Atomically swap a zip's mods-folder entry `link` for a new link to `dst`
/// (`organize::replace_zip_link`), then refresh its projection record.
fn replace_zip_link(
    env: &Env,
    row: &OrganizedRow,
    link: &Path,
    dst: &Path,
    policy: &mut LinkPolicy,
) -> Result<(), StepErr> {
    organize::replace_zip_link(link, dst, policy).map_err(relink_err)?;
    reaches_the_copy(link, &row.kind).map_err(StepErr::Failed)?;
    env.note_projection(row, link);
    Ok(())
}

/// The mods-folder entry `link` that `repoint` made reaches the kind of mod it should (a file
/// for a zip, a folder for a folder mod). A link to a new copy that went missing while it was
/// made (a sync client removing it, say) would dangle: that's an error, so the step stops before
/// the old copy is deleted. Only an observation: `forward_one` / `back_one` check the new copy
/// again right before deleting the old one.
fn reaches_the_copy(link: &Path, kind: &str) -> Result<(), String> {
    match std::fs::metadata(link) {
        Ok(m) if right_kind(&m, kind) => Ok(()),
        _ => Err(format!(
            "relink: Silo couldn't open the mod through its new link at {}. Check the new archive folder, then resume.",
            link.display()
        )),
    }
}

/// The active mods FS25 couldn't open from the mods folder `mods` (#415): each active mod whose
/// entry is missing, reaches nothing, or reaches the wrong kind of thing. Asked while an archive
/// move is unfinished, of every active mod, not only those the move still records (`moved`,
/// `relinking`): a cancel stops recording a mod once it's home, and its entry counts all the
/// same. What a paused move lists as links it couldn't mend, and, after a crash, what Resume
/// hasn't mended yet. Only looks (one `metadata` per active mod); sorted, each named once.
pub fn unreachable_mods(mods: &Path, rows: &[OrganizedRow]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .filter(|r| r.active)
        .filter(|r| reaches_the_copy(&mods.join(&r.file_name), &r.kind).is_err())
        .map(|r| r.tech_name.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A file for a zip, a folder for a folder mod.
fn right_kind(m: &std::fs::Metadata, kind: &str) -> bool {
    if kind == "dir" {
        m.is_dir()
    } else {
        m.is_file()
    }
}

/// Right before the old copy `old` is deleted, both copies are still themselves (not links), of
/// the mod's kind, and still match as they did when verified: the same size for a zip, the same
/// files and sizes for a folder mod (`Check::Listing`; no second hash of every byte). A copy
/// that vanished, was swapped for a link, or was replaced by something else since (a sync
/// client, say) stops the step with both kept. An observation, not a lock: an identical-size
/// swap in between isn't caught.
fn still_there(old: &Path, new: &Path, kind: &str) -> Result<(), StepErr> {
    let real = |p: &Path| real_copy(p, kind);
    let Some(n) = real(new) else {
        return Err(StepErr::Failed(format!(
            "move: the new copy at {} is gone, so Silo kept the original. Check that folder, then resume.",
            new.display()
        )));
    };
    let same = match real(old) {
        Some(_) if kind == "dir" => verify(old, new, kind, Check::Listing).unwrap_or(false),
        Some(o) => o.len() == n.len(),
        None => false,
    };
    if same {
        Ok(())
    } else {
        Err(StepErr::Failed(format!(
            "move: {} and {} no longer match, so Silo kept both. Check them, then resume.",
            old.display(),
            new.display()
        )))
    }
}

/// `p`'s own metadata when it is itself (not a link) a copy of the mod's kind: a file for a zip,
/// a folder for a folder mod. What "the copy is there" means wherever a copy is about to be
/// trusted (`still_there`, the end-of-move check).
fn real_copy(p: &Path, kind: &str) -> Option<std::fs::Metadata> {
    p.symlink_metadata()
        .ok()
        .filter(|m| !m.file_type().is_symlink() && right_kind(m, kind))
}

/// What a step needs from the run: the DB (to save the marker, and the scratch journal), the
/// mods folder, and free space per drive (`RunCtx::free_space`).
struct Env<'a> {
    conn: &'a Connection,
    mods: &'a Path,
    mods_key: &'a str,
    free_space: &'a dyn Fn(&Path) -> Option<u64>,
    /// Both archives are known to be on one drive (`RunCtx::same_volume`): mods are renamed,
    /// not copied.
    same_volume: bool,
    /// The no-replace rename (`paths::rename_no_replace`; tests inject failures).
    rename: fn(&Path, &Path) -> std::io::Result<()>,
    /// Writes a rename to disk (`flush_rename`; tests inject failures).
    flush: fn(&Path, &Path, &Path) -> std::io::Result<()>,
}

impl Env<'_> {
    /// Where this run records its scratch entries.
    fn journal(&self) -> scratch::DbJournal<'_> {
        scratch::DbJournal(self.conn)
    }

    /// A copy of `bytes` can be made at `at` (on its drive, plus the 1 GB margin the move's
    /// check keeps) — or the plain-words reason it can't. Free space Silo can't read never
    /// blocks.
    fn room(&self, bytes: u64, at: &Path) -> Result<(), String> {
        let Some(dir) = at
            .ancestors()
            .skip(1)
            .find(|a| a.symlink_metadata().is_ok())
        else {
            return Ok(());
        };
        match short_of(bytes, (self.free_space)(dir)) {
            Some(s) => Err(format!(
                "Not enough free space on the drive with {} to copy {}: {s}. Free up some \
                 space, then resume.",
                dir.display(),
                at.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )),
            None => Ok(()),
        }
    }

    fn save(&self, m: &MoveMarker) -> Result<(), StepErr> {
        save_marker(self.conn, self.mods_key, m).map_err(StepErr::Failed)
    }

    /// Record that Silo placed `tech` in `to` — saved before its link moves or its source goes.
    fn record(&self, m: &mut MoveMarker, tech: &str) -> Result<(), StepErr> {
        if !m.moved.iter().any(|t| t == tech) {
            m.moved.push(tech.to_string());
        }
        self.save(m)
    }

    fn unrecord(&self, m: &mut MoveMarker, tech: &str) -> Result<(), StepErr> {
        m.moved.retain(|t| t != tech);
        self.save(m)
    }

    /// Right before `row`'s mod is renamed away from `src`: when its entry is a link reaching it
    /// (`links_to`), note the link in the marker (saved), so it's re-pointed even if Silo can't
    /// recognize it once `src` is gone. Whether it's noted.
    fn note_link(
        &self,
        m: &mut MoveMarker,
        row: &OrganizedRow,
        src: &Path,
    ) -> Result<bool, StepErr> {
        if !self.same_volume || !links_to(self, row, src) {
            return Ok(false);
        }
        let target = std::fs::read_link(self.mods.join(&row.file_name)).map_err(relink_err)?;
        m.relinking.retain(|n| n.tech_name != row.tech_name);
        m.relinking.push(LinkNote {
            tech_name: row.tech_name.clone(),
            target,
        });
        self.save(m)?;
        Ok(true)
    }

    /// Drop `tech`'s `note_link` entry, if any (its link is re-pointed, or no rename happened).
    fn forget_link(&self, m: &mut MoveMarker, tech: &str) -> Result<(), StepErr> {
        if !m.relinking.iter().any(|n| n.tech_name == tech) {
            return Ok(());
        }
        m.relinking.retain(|n| n.tech_name != tech);
        self.save(m)
    }

    /// A zip's mods-folder entry was just (re)made by Silo: refresh its projection record so
    /// later deactivates prove it's Silo's without hashing. A symlink is identified
    /// structurally and keeps none.
    fn note_projection(&self, row: &OrganizedRow, link: &Path) {
        if row.kind == "dir" {
            return;
        }
        organize::note_zip_projection(self.conn, self.mods_key, &row.file_name, link);
    }
}

/// A mod's archived copy that is itself a link is refused — Silo moves files, never links.
fn refuse_link(src: &Path) -> Result<(), StepErr> {
    match src.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => Err(StepErr::Failed(format!(
            "move: {} is a link, not a file. Silo won't move links.",
            src.display()
        ))),
        _ => Ok(()),
    }
}

/// "Something else is at `at`" — said without guessing who put it there.
fn occupied(at: &Path) -> StepErr {
    StepErr::Failed(format!(
        "move: a different copy is already at {}. Silo won't replace it; move or rename it, then retry.",
        at.display()
    ))
}

/// A step stops while the scratch journal still holds an old copy of this mod (either of its
/// two places) that the sweep kept: renamed aside, its replacement then failed to verify
/// against it (changed or damaged while Silo was stopped). That old copy may be the only intact
/// one, so the move can't count the mod as moved, or copy the other one back over it, until
/// the user has looked at both. The journal decides, not a fresh look: an entry the sweep kept
/// because its folder couldn't be reached still counts if another mod's step has since made
/// that folder again.
fn unresolved_old(env: &Env, a: &Path, b: &Path) -> Result<(), StepErr> {
    let kept: Vec<PathBuf> = env
        .journal()
        .entries()
        .into_iter()
        .filter(|e| e.role == scratch::Role::MoveOld && (e.from == a || e.from == b))
        .map(|e| e.path)
        .collect();
    match kept.first() {
        None => Ok(()),
        // Not where it was kept: its folder was moved or can't be reached, so it can't be checked.
        Some(k) if !exists(k) => Err(StepErr::Failed(format!(
            "move: Silo kept an earlier copy of this mod at {}, but it can't be found there now. Reconnect or put back that folder, then resume.",
            k.display()
        ))),
        Some(k) => Err(StepErr::Failed(format!(
            "move: Silo kept an earlier copy of this mod at {} because the copy at {} no longer matches it. Check both, keep the right one under the mod's name, then resume.",
            k.display(),
            if exists(b) { b } else { a }.display()
        ))),
    }
}

/// One mod, `from` → `to`. Idempotent. The mod is recorded in `moved` before it's renamed or
/// its copy starts (and un-recorded if it isn't placed), so a destination counts as Silo's only
/// when it is recorded — and even then, next to an original, it must verify against it. Anything
/// else at the destination is refused and left untouched.
fn forward_one(
    env: &Env,
    row: &OrganizedRow,
    marker: &mut MoveMarker,
    policy: &mut LinkPolicy,
    abort: &dyn Fn() -> bool,
) -> Result<Step, StepErr> {
    let src = organize::archive_path(&marker.from, &row.category, &row.file_name);
    let dst = organize::archive_path(&marker.to, &row.category, &row.file_name);
    let tech = row.tech_name.as_str();
    let ours = marker.moved.iter().any(|t| t == tech);
    unresolved_old(env, &src, &dst)?;
    let has_src = exists(&src);
    if has_src {
        refuse_link(&src)?;
    }
    match (has_src, exists(&dst)) {
        (false, false) => return Err(missing_both(env, "move", &src, &dst)),
        (false, true) => {
            if !ours {
                return Err(StepErr::Failed(format!(
                    "move: the original is missing from the old location, and Silo won't adopt the copy at {}. Check it, then retry.",
                    dst.display()
                )));
            }
            repoint(env, row, &src, &dst, policy)?;
            // Maybe renamed by an earlier run (whatever the drives look like now): on disk before
            // the move can finish.
            flushed(env, &dst, &src)?;
            return Ok(Step::AlreadyDone);
        }
        (true, true) => {
            if !ours {
                return Err(occupied(&dst));
            }
            // Placed by an earlier run: the original may have changed since (same sizes), so
            // compare contents before its deletion.
            if !verify(&src, &dst, &row.kind, Check::Contents).map_err(StepErr::Failed)? {
                // Not a copy Silo can vouch for: stop claiming it, so a cancel leaves it alone.
                env.unrecord(marker, tech)?;
                return Err(occupied(&dst));
            }
        }
        (true, false) => {
            // The copy makes the category folder when it's missing: note it as Silo's first
            // (saved with the record below), so a cancel can remove it if it's left empty.
            let new_category = dst.parent().is_some_and(|d| !exists(d));
            if let Some(created) = marker.created.as_mut() {
                if new_category && !created.contains(&row.category) {
                    created.push(row.category.clone());
                }
            }
            env.record(marker, tech)?;
            let linked = env.note_link(marker, row, &src)?;
            let placed = rename_or_copy(env, &src, &dst, &row.kind, abort);
            if !matches!(placed, Ok(Placed::Renamed)) {
                env.forget_link(marker, tech)?;
            }
            match placed {
                Ok(Placed::Copied) => {}
                Ok(Placed::Renamed) => {
                    return match relink_renamed(env, row, &src, &dst, linked, policy) {
                        Ok(()) => {
                            env.forget_link(marker, tech)?;
                            Ok(Step::Moved)
                        }
                        Err((e, restored)) => {
                            if restored {
                                env.forget_link(marker, tech)?;
                                env.unrecord(marker, tech)?;
                            }
                            Err(e)
                        }
                    };
                }
                Err(e) => {
                    env.unrecord(marker, tech)?;
                    return Err(e);
                }
            }
        }
    }
    if abort() {
        return Err(StepErr::Interrupted);
    }
    repoint(env, row, &src, &dst, policy)?;
    if abort() {
        return Err(StepErr::Interrupted);
    }
    still_there(&src, &dst, &row.kind)?;
    delete_source(env, &src, &dst, &marker.to, &row.kind)?;
    Ok(Step::Moved)
}

/// One of Silo's moved mods, `to` → `from` (cancel). Removed from `moved` once it's home.
fn back_one(
    env: &Env,
    row: &OrganizedRow,
    marker: &mut MoveMarker,
    policy: &mut LinkPolicy,
    abort: &dyn Fn() -> bool,
) -> Result<Step, StepErr> {
    let src = organize::archive_path(&marker.to, &row.category, &row.file_name);
    let home = organize::archive_path(&marker.from, &row.category, &row.file_name);
    unresolved_old(env, &src, &home)?;
    let has_src = exists(&src);
    if has_src {
        refuse_link(&src)?;
    }
    match (has_src, exists(&home)) {
        (false, false) => return Err(missing_both(env, "move back", &src, &home)),
        (false, true) => {
            // Home must be the copy itself before the mod stops being Silo's to move back: a
            // link at its name isn't it (the same check the end of the move makes).
            if real_copy(&home, &row.kind).is_none() {
                return Err(StepErr::Failed(format!(
                    "move back: {} isn't the mod's copy (it's a link or something else). Check it, then resume.",
                    home.display()
                )));
            }
            repoint(env, row, &src, &home, policy)?;
            // Maybe renamed home by an earlier run: on disk before it's un-recorded.
            flushed(env, &home, &src)?;
            env.unrecord(marker, &row.tech_name)?;
            return Ok(Step::AlreadyDone);
        }
        (true, true) => {
            if !verify(&src, &home, &row.kind, Check::Contents).map_err(StepErr::Failed)? {
                return Err(occupied(&home));
            }
        }
        (true, false) => {
            let tech = row.tech_name.as_str();
            let linked = env.note_link(marker, row, &src)?;
            let placed = rename_or_copy(env, &src, &home, &row.kind, abort);
            if !matches!(placed, Ok(Placed::Renamed)) {
                env.forget_link(marker, tech)?;
            }
            if placed? == Placed::Renamed {
                // Renamed back: still recorded until its link is home too (a put-back leaves it
                // in `to`, recorded, for the retry).
                if let Err((e, restored)) = relink_renamed(env, row, &src, &home, linked, policy) {
                    if restored {
                        env.forget_link(marker, tech)?;
                    }
                    return Err(e);
                }
                env.forget_link(marker, tech)?;
                env.unrecord(marker, tech)?;
                return Ok(Step::Moved);
            }
        }
    }
    if abort() {
        return Err(StepErr::Interrupted);
    }
    repoint(env, row, &src, &home, policy)?;
    if abort() {
        return Err(StepErr::Interrupted);
    }
    still_there(&src, &home, &row.kind)?;
    delete_source(env, &src, &home, &marker.from, &row.kind)?;
    env.unrecord(marker, &row.tech_name)?;
    Ok(Step::Moved)
}

/// "The archived copy is missing from both locations", naming any copy Silo kept that may be an
/// earlier copy of it: a recorded scratch entry that couldn't be proven safe to remove, or an
/// older version's leftover (`scratch::kept_for`), so the user can find it.
fn missing_both(env: &Env, what: &str, a: &Path, b: &Path) -> StepErr {
    let kept = scratch::kept_for(&env.journal(), &[a, b]);
    let mut msg = format!("{what}: the archived copy is missing from both locations");
    if !kept.is_empty() {
        let list: Vec<String> = kept.iter().map(|p| p.display().to_string()).collect();
        msg.push_str(&format!(
            ". Silo kept what may be an earlier copy at {}. Check it, then resume.",
            list.join(", ")
        ));
    }
    StepErr::Failed(msg)
}

/// Before a run: clear what the scratch journal proves redundant (`scratch::sweep_where`: only
/// recorded entries, each checked first), then log scratch-like names under each category
/// folder of both archives and in the mods folder that aren't recorded — an older version's
/// leftovers, or not Silo's at all. Those are left in place, never deleted by name.
fn sweep_scratch(env: &Env, marker: &MoveMarker) {
    let journal = env.journal();
    scratch::sweep_where(&journal, &|e| {
        e.role == scratch::Role::RelinkTmp
            || scratch::inside(&e.path, &marker.from)
            || scratch::inside(&e.path, &marker.to)
    });
    for archive in [&marker.from, &marker.to] {
        if let Ok(cats) = std::fs::read_dir(archive) {
            for c in cats.flatten() {
                if c.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    scratch::log_unrecorded(&journal, &c.path(), &is_move_scratch);
                }
            }
        }
    }
    scratch::log_unrecorded(&journal, env.mods, &|n: &str| {
        n.to_lowercase().ends_with(RELINK_SUFFIX)
    });
}

/// A move's own scratch names, plus relink temps and organize's `.{name}.silo-tmp`: what
/// `sweep_scratch` logs when unrecorded (it never deletes by name). Unlike
/// `scan::is_silo_temp`, never a mod update's `.silo-update.part` or `.silo-update-old`
/// (`update::recover_interrupted` deals with those).
fn is_move_scratch(name: &str) -> bool {
    let l = name.to_lowercase();
    l.ends_with(TMP_SUFFIX)
        || l.ends_with(OLD_SUFFIX)
        || l.ends_with(RELINK_SUFFIX)
        || (l.starts_with('.') && l.ends_with(".silo-tmp"))
}

/// Finish or roll back interrupted mod updates in the mods folder and both archives' category
/// folders (the move holds the library lock; the user asked for the move, so every step is
/// retried). Returns why the move can't go on:
/// - forward: any previous version still set aside (moving around it would leave it behind);
/// - back (a cancel): only one whose mod is missing. Otherwise the mod is in place and nothing
///   deletes the leftover, so the cancel must be able to finish and give the library back.
fn settle_updates(env: &Env, marker: &MoveMarker, direction: Direction) -> Option<String> {
    let mods = env.mods;
    let mut all = crate::update::Recovery::default();
    // Recovery re-links like the move does: its temps recorded, a fallback copy checked for
    // room first (a short drive leaves the update set aside, which pauses the move, saying so).
    let journal = env.journal();
    let room = |src: &Path, entry: &Path| {
        env.room(size_of(src), entry)
            .map_err(|why| std::io::Error::other(organize::NoRoom(why)))
    };
    for archive in [&marker.from, &marker.to] {
        let layout = Layout {
            mods: mods.to_path_buf(),
            archive: archive.clone(),
        };
        let mut dirs = crate::update::layout_dirs(&layout);
        if archive != &marker.from {
            dirs.retain(|d| d != mods); // the mods folder once is enough
        }
        let r = crate::update::settle(
            &dirs,
            mods,
            &mut LinkPolicy::new(&layout)
                .with_journal(&journal)
                .with_room_check(&room),
            crate::update::Attempt::UserAsked,
        );
        for n in &r.notices {
            log::info!("{n}");
        }
        for p in &r.problems {
            log::warn!("{p}");
        }
        all.problems.extend(r.problems);
        all.leftovers.extend(r.leftovers);
        all.stranded.extend(r.stranded);
    }
    let blocking = match direction {
        Direction::Forward => &all.leftovers,
        Direction::Back => &all.stranded,
    };
    if blocking.is_empty() {
        None
    } else {
        Some(crate::update::leftover_message(&all, blocking))
    }
}

/// Of `unknown` (from `unknown_mods`), the file names that are a second copy of a library mod
/// filed under another category (same file name, or named after its techName). They get their
/// own message: rescanning can't fix a mod that's filed twice.
pub fn filed_twice(unknown: &[PathBuf], rows: &[OrganizedRow]) -> Vec<String> {
    unknown
        .iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            let l = name.to_lowercase();
            let stem = l.strip_suffix(".zip").unwrap_or(&l);
            rows.iter()
                .any(|r| r.file_name.to_lowercase() == l || r.tech_name.to_lowercase() == stem)
                .then_some(name)
        })
        .collect()
}

/// "N mod(s) in your archive aren't in Silo's library yet. `Then`".
pub fn unknown_count_message(n: usize, then: &str) -> String {
    let mut c = then.chars();
    let then: String = c
        .next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default();
    if n == 1 {
        format!("1 mod in your archive isn't in Silo's library yet. {then}")
    } else {
        format!("{n} mods in your archive aren't in Silo's library yet. {then}")
    }
}

/// One message per mod filed twice, then one for the rest of `unknown` (if any). `then_twice`
/// and `then_unknown` say what to do next in the caller's situation.
pub fn unknown_messages(
    unknown: &[PathBuf],
    rows: &[OrganizedRow],
    then_twice: &str,
    then_unknown: &str,
) -> Vec<String> {
    let twice = filed_twice(unknown, rows);
    let mut v: Vec<String> = twice
        .iter()
        .map(|n| format!("{n} is filed twice in your archive. Remove one copy, {then_twice}"))
        .collect();
    let rest = unknown.len() - twice.len();
    if rest > 0 {
        v.push(unknown_count_message(rest, then_unknown));
    }
    v
}

/// Record `archive` as the mods folder's archive: the setting is cleared when it's the default
/// `<mods>/archive` (compared canonically), so the default keeps following the mods folder.
pub fn store_archive_setting(
    conn: &Connection,
    mods_key: &str,
    mods: &Path,
    archive: &Path,
) -> Result<(), String> {
    let default = Layout::default_for(mods).archive;
    let value = if crate::modsroot::same_folder(archive, &default) {
        None
    } else {
        Some(archive.to_string_lossy().into_owned())
    };
    db::set_app_setting(conn, &archive_setting_key(mods_key), value.as_deref())?;
    note_archive(mods, value.as_deref().map(Path::new));
    Ok(())
}

/// Silo's note in a custom archive (naming its mods folder) and in that mods folder (naming the
/// archive). If Silo's database is lost, the pair lets it find the archive again. FS25 only
/// loads zips and folders, and a scan skips loose files, so neither reads it as a mod.
pub const ARCHIVE_NOTE: &str = ".silo-archive.json";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveNote {
    silo: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mods_folder: Option<PathBuf>,
}

fn read_note(dir: &Path) -> Option<ArchiveNote> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(ARCHIVE_NOTE)).ok()?).ok()
}

/// Write the pair for a custom `archive`, or remove the mods folder's note for the default
/// (`None`). Best effort: a missing note only costs rediscovery, never the change itself. A
/// file by that name that isn't a Silo note is the user's, and is never replaced or removed.
/// Each note is written to a scratch file and renamed into place, so it's never half-written.
pub(crate) fn note_archive(mods: &Path, archive: Option<&Path>) {
    let write = |dir: &Path, note: &ArchiveNote| {
        let path = dir.join(ARCHIVE_NOTE);
        if path.exists() && read_note(dir).is_none() {
            log::warn!("left {} alone: it isn't a note Silo wrote", path.display());
            return;
        }
        let tmp = dir.join(format!("{ARCHIVE_NOTE}.tmp"));
        let json = serde_json::to_string_pretty(note).unwrap_or_default();
        let written = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path));
        match written {
            Ok(()) => crate::fslog::changed("write-note", &path),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                log::warn!("couldn't write {} in {}: {e}", ARCHIVE_NOTE, dir.display());
            }
        }
    };
    match archive {
        Some(a) => {
            write(
                a,
                &ArchiveNote {
                    silo: 1,
                    archive: None,
                    mods_folder: Some(mods.to_path_buf()),
                },
            );
            write(
                mods,
                &ArchiveNote {
                    silo: 1,
                    archive: Some(a.to_path_buf()),
                    mods_folder: None,
                },
            );
        }
        None => {
            let note = mods.join(ARCHIVE_NOTE);
            if read_note(mods).is_some() && std::fs::remove_file(&note).is_ok() {
                crate::fslog::changed("remove-note", &note);
            }
        }
    }
}

/// The note Silo wrote in a folder it no longer uses as this mods folder's archive (a finished
/// move away from it, a cancelled move into it, or a re-point away from an empty archive). Nothing reads it any more: rediscovery
/// starts from the mods folder's own note (`rediscover_archive`), which now names another
/// archive or is gone, and a folder picked as an archive again gets a fresh note
/// (`note_archive`). So it's removed (#394), but only when it is Silo's note naming this mods
/// folder, and only once the folder holds no mods Silo filed (`filed_mods`, which must read the
/// folder). The folder and everything else in it are left as they are.
pub(crate) fn retire_note(left: &Path, mods: &Path) {
    let ours = read_note(left)
        .and_then(|n| n.mods_folder)
        .is_some_and(|m| crate::modsroot::same_folder(&m, mods));
    if !ours || !filed_mods(left).is_ok_and(|v| v.is_empty()) {
        return;
    }
    let note = left.join(ARCHIVE_NOTE);
    match std::fs::remove_file(&note) {
        Ok(()) => crate::fslog::changed("remove-note", &note),
        Err(e) => log::warn!("couldn't remove {}: {e}", note.display()),
    }
}

/// The archive the notes point to, for a mods folder the database knows no archive for: the
/// mods folder's note names a local, absolute folder that `Layout` accepts, and that folder's
/// own note names this mods folder back, or its drive isn't plugged in right now (it can't
/// confirm; the missing-archive banner then takes over). A folder missing from a drive that
/// is there was deleted, so it isn't taken up. Never while the default archive holds mods.
/// The caller still checks the fences (other mods folders, Silo's own folders).
pub fn rediscover_archive(mods: &Path) -> Option<PathBuf> {
    // The note first: without one (the usual case) the default archive isn't listed at all.
    let archive = read_note(mods)?.archive?;
    if !archive.is_absolute()
        || crate::organize::is_network_path(&archive.to_string_lossy())
        || Layout::new(mods.to_path_buf(), archive.clone()).is_err()
    {
        return None;
    }
    let default = Layout::default_for(mods).archive;
    if filed_mods(&default).map(|v| !v.is_empty()).unwrap_or(true) {
        return None;
    }
    if !archive.exists() {
        return drive_absent(&archive).then_some(archive);
    }
    let back = read_note(&archive)?.mods_folder?;
    crate::modsroot::same_folder(&back, mods).then_some(archive)
}

/// Whether the drive (Windows) or removable-media mount (elsewhere) that `p` lives on is
/// absent right now: an unplugged drive, rather than a folder that was deleted.
fn drive_absent(p: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::Component;
        let root: PathBuf = p
            .components()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .collect();
        !root.as_os_str().is_empty() && !root.exists()
    }
    #[cfg(not(windows))]
    {
        // Removable media mounts under these (`/media/<user>/<volume>` as well as
        // `/media/<volume>`); an unmounted mount point is missing, or an empty folder.
        const BASES: [(&str, usize); 4] = [
            ("/Volumes", 1),
            ("/run/media", 2),
            ("/media", 2),
            ("/mnt", 1),
        ];
        let Some((base, depth)) = BASES.iter().find(|(b, _)| p.starts_with(b)) else {
            return false;
        };
        let Ok(rest) = p.strip_prefix(base) else {
            return false;
        };
        let mut root = PathBuf::from(base);
        for c in rest.components().take(*depth) {
            root.push(c);
            let empty = std::fs::read_dir(&root).is_ok_and(|mut d| d.next().is_none());
            if !root.exists() || empty {
                return true;
            }
        }
        false
    }
}

/// Every mod (zip, or folder with a `modDesc.xml`) filed under `archive/<category>/`, skipping
/// Silo's own scratch entries (`scan::is_silo_temp`). A missing archive folder holds none; any
/// other read error — the archive or a category folder exists but can't be listed — is returned,
/// so a caller never mistakes an unreadable archive for an empty one.
pub fn filed_mods(archive: &Path) -> std::io::Result<Vec<PathBuf>> {
    let list = |p: &Path| -> Listing {
        std::fs::read_dir(p)?
            .map(|e| {
                let e = e?;
                Ok((e.path(), e.file_type()?.is_dir()))
            })
            .collect()
    };
    filed_mods_with(archive, &list)
}

/// A directory listing as (path, is_dir) pairs.
type Listing = std::io::Result<Vec<(PathBuf, bool)>>;

/// `filed_mods` over an injected directory lister.
fn filed_mods_with(
    archive: &Path,
    list: &dyn Fn(&Path) -> Listing,
) -> std::io::Result<Vec<PathBuf>> {
    let cats = match list(archive) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(cant_read(archive, e)),
    };
    let mut out = Vec::new();
    for (cat, is_dir) in cats {
        if !is_dir {
            continue;
        }
        for (p, is_dir) in list(&cat).map_err(|e| cant_read(&cat, e))? {
            let n = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if crate::scan::is_silo_temp(&n) {
                continue;
            }
            if n.ends_with(".zip") || has_moddesc(&p, is_dir)? {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// Whether `p` is a folder mod (holds a `modDesc.xml`). A folder that can't be looked into is
/// an error naming it, never "not a mod". Anything that isn't a plain folder (a file, a link)
/// is checked as before, best effort.
fn has_moddesc(p: &Path, is_dir: bool) -> std::io::Result<bool> {
    let desc = p.join("modDesc.xml");
    if !is_dir {
        return Ok(desc.is_file());
    }
    match std::fs::metadata(&desc) {
        Ok(m) => Ok(m.is_file()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(cant_read(p, e)),
    }
}

/// A listing error that names the folder Silo couldn't read (same kind as `e`).
fn cant_read(folder: &Path, e: std::io::Error) -> std::io::Error {
    std::io::Error::new(e.kind(), format!("{} ({e})", folder.display()))
}

/// The problem shown when `unknown_mods` can't list an archive folder (its error names the
/// folder): Silo can't tell whether it holds mods a move would leave behind.
pub fn unreadable_archive_message(e: &std::io::Error, then: &str) -> String {
    format!("Silo can't read {e}, so it can't tell which mods are in it. Check that the folder can be opened, {then}")
}

/// Mods filed under `archive/<category>/` (`filed_mods`) that have no organized row — a move
/// would leave them behind. Preflight calls this before any marker is written; a forward run
/// re-checks it at every start. An archive or category folder that can't be listed is an error
/// (`unreadable_archive_message`), never "no unknown mods": its unrecorded mods would be left
/// behind in an archive Silo no longer tracks.
pub fn unknown_mods(archive: &Path, rows: &[OrganizedRow]) -> std::io::Result<Vec<PathBuf>> {
    let windows = cfg!(windows);
    let key = |p: &Path| crate::modsroot::normalize_key(&p.to_string_lossy(), windows);
    let known: HashSet<String> = rows
        .iter()
        .map(|r| key(&organize::archive_path(archive, &r.category, &r.file_name)))
        .collect();
    Ok(filed_mods(archive)?
        .into_iter()
        .filter(|p| !known.contains(&key(p)))
        .collect())
}

/// Rows whose destination `<new>/<category>/<file>` already exists, or whose file name or
/// techName already appears one level below any of the destination's subfolders (another
/// category). Preflight reports these; the move itself refuses to replace them.
pub fn collisions(new_archive: &Path, rows: &[OrganizedRow]) -> Vec<String> {
    let mut present: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(subs) = std::fs::read_dir(new_archive) {
        for s in subs.flatten() {
            if !s.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if let Ok(es) = std::fs::read_dir(s.path()) {
                for e in es.flatten() {
                    present.push((e.file_name().to_string_lossy().to_lowercase(), e.path()));
                }
            }
        }
    }
    // "<folder> already has a different file named X." / "… a folder named X — another copy
    // of <tech>." (plain language, the path kept so the user can find it).
    let say = |p: &Path, exact: bool, tech: &str| {
        let dir = p
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let what = if p.is_dir() { "folder" } else { "file" };
        if exact {
            format!("{dir} already has a different {what} named {name}.")
        } else {
            format!("{dir} already has a {what} named {name}. Another copy of {tech}.")
        }
    };
    rows.iter()
        .filter_map(|r| {
            let exact = organize::archive_path(new_archive, &r.category, &r.file_name);
            if exists(&exact) {
                return Some(say(&exact, true, &r.tech_name));
            }
            let file = r.file_name.to_lowercase();
            let tech = r.tech_name.to_lowercase();
            present
                .iter()
                .find(|(n, _)| *n == file || n.strip_suffix(".zip").unwrap_or(n) == tech)
                .map(|(_, p)| say(p, false, &r.tech_name))
        })
        .collect()
}

pub struct RunCtx<'a> {
    pub conn: &'a Connection,
    pub mods: &'a Path,
    pub mods_key: &'a str,
    pub game_running: &'a dyn Fn() -> bool,
    /// `game_running` is asked at most this often (it scans processes); `Duration::ZERO` in tests.
    pub game_check_interval: Duration,
    /// Pause/cancel requests (`requests()` in production; tests pass their own).
    pub request: &'a AtomicU8,
    pub progress: &'a mut dyn FnMut(&Progress),
    /// Free bytes on the drive holding a folder (`organize::mount_of` in production), asked
    /// before each copy; `None` (unknown) never blocks.
    pub free_space: &'a dyn Fn(&Path) -> Option<u64>,
    /// Whether two folders are on one drive (`organize::volume_relation` in production):
    /// `Some(true)` renames each mod, anything else copies it.
    pub same_volume: &'a dyn Fn(&Path, &Path) -> Option<bool>,
}

/// Throttled "has FS25 started?" that stays true once seen, so the boundary after an aborted
/// copy still sees it even inside the throttle window.
struct GameCheck<'a> {
    running: &'a dyn Fn() -> bool,
    every: Duration,
    last: Cell<Option<Instant>>,
    seen: Cell<bool>,
}

impl GameCheck<'_> {
    fn started(&self) -> bool {
        if self.seen.get() {
            return true;
        }
        let now = Instant::now();
        if let Some(t) = self.last.get() {
            if now.duration_since(t) < self.every {
                return false;
            }
        }
        self.last.set(Some(now));
        let r = (self.running)();
        self.seen.set(r);
        r
    }
}

enum Boundary {
    Go,
    Pause,
    TurnAround,
}

/// The checks at every mod boundary: the game starting pauses; a pause request pauses; a cancel
/// turns a forward move around (a second cancel while already moving back is a no-op).
fn boundary(game: &GameCheck, request: &AtomicU8, marker: &mut MoveMarker) -> Boundary {
    if game.started() {
        marker.pause_reason = Some("Farming Simulator 25 started".into());
        return Boundary::Pause;
    }
    match request.swap(0, Ordering::SeqCst) {
        PAUSE => Boundary::Pause,
        CANCEL if marker.direction == Direction::Forward => Boundary::TurnAround,
        _ => Boundary::Go,
    }
}

fn pause(env: &Env, mut marker: MoveMarker) -> Result<Option<MoveMarker>, String> {
    marker.paused = true;
    save_marker(env.conn, env.mods_key, &marker)?;
    Ok(Some(marker))
}

fn failed(e: StepErr) -> String {
    match e {
        StepErr::Failed(s) | StepErr::NoRoom(s) => s,
        StepErr::Interrupted => "interrupted".into(),
    }
}

const ALREADY_UNFINISHED: &str =
    "An archive move is already unfinished. Resume or cancel it first.";

/// Start a new move of the archive `from` → `to` and run it until it finishes or pauses.
/// Refuses while another move is running or unfinished, or while the game runs (nothing is
/// written then). Returns the paused marker, or `None` when the move finished.
pub fn start(ctx: &mut RunCtx, from: PathBuf, to: PathBuf) -> Result<Option<MoveMarker>, String> {
    let held = RunGuard::take()?;
    if load_marker(ctx.conn, ctx.mods_key).is_some() {
        return Err(ALREADY_UNFINISHED.into());
    }
    if (ctx.game_running)() {
        return Err(GAME_RUNNING.into());
    }
    let marker = MoveMarker::new(from, to);
    // Before any marker exists: a leftover that can't be put away refuses the move outright,
    // so it can never leave an unfinished move (which blocks every library change) behind.
    let env = Env {
        conn: ctx.conn,
        mods: ctx.mods,
        mods_key: ctx.mods_key,
        free_space: ctx.free_space,
        same_volume: false,
        rename: crate::paths::rename_no_replace,
        flush: flush_rename,
    };
    if let Some(reason) = settle_updates(&env, &marker, Direction::Forward) {
        return Err(reason);
    }
    run_locked(ctx, &held, marker)
}

/// Resume the unfinished move (in whichever direction it was going).
pub fn resume(ctx: &mut RunCtx) -> Result<Option<MoveMarker>, String> {
    let held = RunGuard::take()?;
    let mut marker = load_marker(ctx.conn, ctx.mods_key)
        .ok_or_else(|| "There's no archive move to resume.".to_string())?;
    marker.paused = false;
    run_locked(ctx, &held, marker)
}

/// Cancel the unfinished move: move Silo's moved mods back home. If a move is running right
/// now, this only asks it to turn around (`ctx.request`) and returns `Ok(None)` at once.
pub fn cancel(ctx: &mut RunCtx) -> Result<Option<MoveMarker>, String> {
    let Ok(held) = RunGuard::take() else {
        ctx.request.store(CANCEL, Ordering::SeqCst);
        return Ok(None);
    };
    let mut marker = load_marker(ctx.conn, ctx.mods_key)
        .ok_or_else(|| "There's no archive move to cancel.".to_string())?;
    marker.direction = Direction::Back;
    marker.paused = false;
    run_locked(ctx, &held, marker)
}

/// Run `marker` until it finishes or pauses. The caller holds the one-move guard (`_held`) and
/// loaded the marker from the DB under it. Refuses while the game runs (before writing).
fn run_locked(
    ctx: &mut RunCtx,
    _held: &RunGuard,
    mut marker: MoveMarker,
) -> Result<Option<MoveMarker>, String> {
    if (ctx.game_running)() {
        return Err(GAME_RUNNING.into());
    }
    let env = Env {
        conn: ctx.conn,
        mods: ctx.mods,
        mods_key: ctx.mods_key,
        free_space: ctx.free_space,
        // Asked once: the same answer both ways, so a cancel renames back too. Unknown copies.
        same_volume: (ctx.same_volume)(&marker.from, &marker.to) == Some(true),
        rename: crate::paths::rename_no_replace,
        flush: flush_rename,
    };
    // Not reset here: a pause/cancel sent after the caller took its lock (before this point)
    // must be honored at the first boundary. The command layer clears stale requests when it
    // takes the library lock for a move.
    let request = ctx.request;
    marker.paused = false;
    marker.pause_reason = None;
    marker.errors.clear();
    save_marker(env.conn, env.mods_key, &marker)?;
    // What an earlier run left is cleared first (only what the journal proves redundant; an
    // only copy set aside is put back), so the mend sees every copy and has the space a finished
    // temp held. Then the mend, before anything can pause the move.
    sweep_scratch(&env, &marker);
    let unmended = mend_renamed_links(&env, &mut marker);
    if !unmended.is_empty() {
        marker.errors.extend(unmended);
        return pause(&env, marker);
    }
    if let Some(reason) = settle_updates(&env, &marker, marker.direction) {
        marker.pause_reason = Some(reason);
        return pause(&env, marker);
    }

    if marker.direction == Direction::Forward {
        let rows = db::load_organized(env.conn, env.mods_key);
        match unknown_mods(&marker.from, &rows) {
            Ok(unknown) if unknown.is_empty() => {}
            Ok(unknown) => {
                marker.errors.extend(unknown_messages(
                    &unknown,
                    &rows,
                    "then resume.",
                    "cancel the move, rescan, then start again.",
                ));
                return pause(&env, marker);
            }
            Err(e) => {
                marker
                    .errors
                    .push(unreadable_archive_message(&e, "then resume."));
                return pause(&env, marker);
            }
        }
    }

    let game = GameCheck {
        running: ctx.game_running,
        every: ctx.game_check_interval,
        last: Cell::new(Some(Instant::now())),
        seen: Cell::new(false),
    };

    'pass: loop {
        let back = marker.direction == Direction::Back;
        let mut rows = db::load_organized(env.conn, env.mods_key);
        rows.sort_by(|a, b| a.tech_name.cmp(&b.tech_name));
        let snapshot = marker.moved.clone();
        if back {
            for t in &snapshot {
                if !rows.iter().any(|r| &r.tech_name == t) {
                    marker.errors.push(format!(
                        "{t}: no longer in Silo's library, so Silo can't move it back."
                    ));
                }
            }
            rows.retain(|r| snapshot.contains(&r.tech_name));
        }
        let (src_root, dst_root) = if back {
            (marker.to.clone(), marker.from.clone())
        } else {
            (marker.from.clone(), marker.to.clone())
        };
        // Links are made into the destination archive; the policy probes (lazily, at most once)
        // only if a hardlink actually fails.
        let layout = Layout {
            mods: env.mods.to_path_buf(),
            archive: dst_root.clone(),
        };
        // Relink temps are recorded in the journal, and a copy into the mods folder (where a
        // link can't be made) is checked for room first, like every archive copy.
        let journal = env.journal();
        let room = |src: &Path, entry: &Path| {
            env.room(size_of(src), entry)
                .map_err(|why| std::io::Error::other(organize::NoRoom(why)))
        };
        let mut policy = LinkPolicy::new(&layout)
            .with_journal(&journal)
            .with_room_check(&room);
        let sizes: Vec<u64> = rows
            .iter()
            .map(|r| {
                let s = organize::archive_path(&src_root, &r.category, &r.file_name);
                let d = organize::archive_path(&dst_root, &r.category, &r.file_name);
                size_of(if exists(&s) { &s } else { &d })
            })
            .collect();
        let mut p = Progress {
            total: rows.len(),
            bytes_total: sizes.iter().sum(),
            cancelling: back,
            ..Default::default()
        };
        let mut i = 0;
        while i < rows.len() {
            match boundary(&game, request, &mut marker) {
                Boundary::Pause => return pause(&env, marker),
                Boundary::TurnAround => {
                    marker.direction = Direction::Back;
                    marker.errors.clear();
                    save_marker(env.conn, env.mods_key, &marker)?;
                    continue 'pass;
                }
                Boundary::Go => {}
            }
            let row = &rows[i];
            p.current = row.tech_name.clone();
            (ctx.progress)(&p);
            let abort = || request.load(Ordering::SeqCst) != 0 || game.started();
            let step = if back {
                back_one(&env, row, &mut marker, &mut policy, &abort)
            } else {
                forward_one(&env, row, &mut marker, &mut policy, &abort)
            };
            match step {
                // Handle the request / game at the boundary, then retry this same mod.
                Err(StepErr::Interrupted) => continue,
                // Nothing of this mod was copied: pause before the drive fills up.
                Err(StepErr::NoRoom(why)) => {
                    marker.pause_reason = Some(why);
                    return pause(&env, marker);
                }
                Err(e) => marker
                    .errors
                    .push(format!("{}: {}", row.tech_name, failed(e))),
                Ok(_) => {}
            }
            p.done += 1;
            p.bytes_done += sizes[i];
            (ctx.progress)(&p);
            i += 1;
        }
        // A request (or the game) that arrived during the last mod.
        match boundary(&game, request, &mut marker) {
            Boundary::Pause => return pause(&env, marker),
            Boundary::TurnAround => {
                marker.direction = Direction::Back;
                marker.errors.clear();
                save_marker(env.conn, env.mods_key, &marker)?;
                continue 'pass;
            }
            Boundary::Go => {}
        }
        // Never finish with a mod left behind: check every one is where it belongs.
        if marker.errors.is_empty() {
            for r in &rows {
                let s = organize::archive_path(&src_root, &r.category, &r.file_name);
                let d = organize::archive_path(&dst_root, &r.category, &r.file_name);
                // The copy itself, of the mod's kind: a link at its name doesn't count.
                if real_copy(&d, &r.kind).is_none() || exists(&s) {
                    marker.errors.push(format!(
                        "{}: not fully moved. {} should exist and {} shouldn't",
                        r.tech_name,
                        d.display(),
                        s.display()
                    ));
                }
            }
            if back {
                for t in &marker.moved {
                    marker.errors.push(format!("{t}: not moved back yet"));
                }
            }
        }
        if !marker.errors.is_empty() {
            return pause(&env, marker);
        }
        // The folder being left gets no more mods: its archive note goes now, while the marker
        // still exists, so a crash here is finished by Resume (`retire_note` is idempotent).
        let leaving_default = Layout {
            mods: env.mods.to_path_buf(),
            archive: src_root.clone(),
        }
        .archive_is_default();
        if !leaving_default {
            retire_note(&src_root, env.mods);
        }
        // Finished: switch the setting (only on a completed forward move) and clear the marker
        // together, then tidy the folder we left.
        let tx = env
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        if !back {
            store_archive_setting(&tx, env.mods_key, env.mods, &marker.to)?;
        }
        clear_marker(&tx, env.mods_key)?;
        tx.commit().map_err(|e| e.to_string())?;
        let left = Layout {
            mods: env.mods.to_path_buf(),
            archive: src_root.clone(),
        };
        if left.archive_is_default() {
            // Silo's own folder: remove every empty directory, and the folder itself if empty.
            let _ = organize::cleanup_empty_archive(&src_root, true);
        } else {
            // A folder the user chose: only now-empty category folders that are Silo's. After a
            // cancel, the ones Silo made there (also for copies it then abandoned), never one
            // that was already there. Otherwise (a finished move, or a cancel of a move started
            // before Silo recorded them) the categories Silo filed into.
            let categories: HashSet<String> = match (&marker.created, back) {
                (Some(created), true) => created.iter().cloned().collect(),
                _ => rows.iter().map(|r| r.category.clone()).collect(),
            };
            organize::remove_empty_category_dirs(&src_root, &categories);
        }
        return Ok(None);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preflight {
    pub mod_count: usize,
    pub bytes: u64,
    pub free_bytes: Option<u64>,
    pub same_drive: bool,
    pub symlinks_available: bool,
    pub loose_mods: usize,
    pub problems: Vec<String>,
    /// The current archive holds no mods and the folder passes every path/overlap fence, so
    /// Silo can use it as the archive as it is (`archive_repoint`, moves nothing) — the way to
    /// adopt a folder that already has mods filed in subfolders, which a move refuses.
    pub can_repoint: bool,
}

/// Path sanity for a new archive location: anything `Layout::new` rejects, plus "already
/// there", "inside the mods folder", and "another folder's archive". Pure apart from `same`
/// (inject `modsroot::same_folder`) and `Layout::new`'s canonical comparison.
pub fn path_problems(
    current: &Path,
    new: &Path,
    mods: &Path,
    other_archives: &[PathBuf],
    same: impl Fn(&Path, &Path) -> bool,
) -> Vec<String> {
    let mut v = Vec::new();
    if same(current, new) {
        v.push("That's already where your library archive is.".into());
    }
    let default = Layout::default_for(mods).archive;
    let inside = new.starts_with(mods) && !same(new, &default);
    if inside {
        v.push("Pick a folder outside your mods folder (or the default archive).".into());
    }
    if let Err(e) = Layout::new(mods.to_path_buf(), new.to_path_buf()) {
        // Don't repeat the "inside the mods folder" problem in other words.
        if !inside {
            v.push(e);
        }
    }
    if other_archives.iter().any(|a| same(a, new)) {
        v.push("Another mods folder already uses that archive.".into());
    }
    v
}

/// "needs X GB, Y GB free", if `free` can't hold `bytes` plus a 1 GB margin (unknown never
/// blocks).
fn short_of(bytes: u64, free: Option<u64>) -> Option<String> {
    const MARGIN: u64 = 1 << 30;
    match free {
        Some(f) if f < bytes + MARGIN => Some(format!(
            "needs {:.1} GB, {:.1} GB free",
            (bytes + MARGIN) as f64 / 1e9,
            f as f64 / 1e9
        )),
        _ => None,
    }
}

/// Space problem, if the target drive can't hold `bytes` plus a 1 GB margin.
pub fn space_problem(bytes: u64, free: Option<u64>) -> Option<String> {
    short_of(bytes, free).map(|s| format!("Not enough free space: {s}."))
}

/// Space problem on the mods folder's own drive (another drive than the new archive's), if it
/// can't hold the `bytes` of copies a move makes there plus a 1 GB margin.
pub fn mods_space_problem(bytes: u64, free: Option<u64>) -> Option<String> {
    short_of(bytes, free).map(|s| {
        format!("Not enough free space on your mods folder's drive for copies of your active mods: {s}.")
    })
}

/// Loose mods (zips / mod folders) at the top level of an archive folder — candidates to adopt.
pub fn loose_mods(archive: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(archive)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .map(|x| x.eq_ignore_ascii_case("zip"))
                        .unwrap_or(false)
                        || p.join("modDesc.xml").is_file()
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A move holds a process-wide "one move at a time" flag, so tests that run one take turns.
    static RUN_LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        /// The next `repoint` on this thread fails after calling this with its link (taken once).
        pub(super) static REPOINT_FAULT: Cell<Option<fn(&Path)>> = const { Cell::new(None) };
        /// The next `inspect_entry` on this thread fails (taken once).
        pub(super) static INSPECT_FAULT: Cell<bool> = const { Cell::new(false) };
        /// Every `flush_rename` on this thread fails while set.
        pub(super) static FLUSH_FAULT: Cell<bool> = const { Cell::new(false) };
    }

    struct Fx {
        base: PathBuf,
        mods: PathBuf,
        old: PathBuf,
        new: PathBuf,
        conn: Connection,
        key: String,
    }

    fn fx(tag: &str) -> Fx {
        let base = std::env::temp_dir().join(format!("silo_reloc_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mods = base.join("mods");
        let old = mods.join("archive");
        let new = base.join("hdd");
        std::fs::create_dir_all(old.join("Tools")).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let conn = db::open(&base.join("silo.db")).unwrap();
        let key = crate::modsroot::canonical_key(&mods);
        Fx {
            base,
            mods,
            old,
            new,
            conn,
            key,
        }
    }

    fn env(f: &Fx) -> Env<'_> {
        Env {
            conn: &f.conn,
            mods: &f.mods,
            mods_key: &f.key,
            free_space: &|_| None,
            same_volume: false,
            rename: crate::paths::rename_no_replace,
            flush: flush_rename,
        }
    }

    /// The destination layout a move into `f.new` links against.
    fn dest(f: &Fx) -> Layout {
        Layout {
            mods: f.mods.clone(),
            archive: f.new.clone(),
        }
    }

    fn row(tech: &str, file: &str, kind: &str, active: bool) -> OrganizedRow {
        OrganizedRow {
            tech_name: tech.into(),
            file_name: file.into(),
            kind: kind.into(),
            category: "Tools".into(),
            subcategory: None,
            active,
        }
    }

    fn add_zip(f: &Fx, tech: &str, active: bool) -> OrganizedRow {
        add_zip_in(f, tech, "Tools", active)
    }

    fn add_zip_in(f: &Fx, tech: &str, category: &str, active: bool) -> OrganizedRow {
        let file = format!("{tech}.zip");
        std::fs::create_dir_all(f.old.join(category)).unwrap();
        std::fs::write(
            f.old.join(category).join(&file),
            format!("PK\x03\x04 {tech}"),
        )
        .unwrap();
        let r = OrganizedRow {
            category: category.into(),
            ..row(tech, &file, "zip", active)
        };
        db::upsert_organized(&f.conn, &f.key, &r).unwrap();
        r
    }

    fn zip_at(dir: &Path, tech: &str) -> PathBuf {
        dir.join("Tools").join(format!("{tech}.zip"))
    }

    fn marker(f: &Fx) -> MoveMarker {
        MoveMarker::new(f.old.clone(), f.new.clone())
    }

    fn setting(f: &Fx) -> Option<String> {
        db::get_app_setting(&f.conn, &archive_setting_key(&f.key))
    }

    /// Which entry point a test drives.
    enum Op {
        /// `start(old → new)`
        Start,
        StartTo(PathBuf, PathBuf),
        Resume,
        Cancel,
    }

    fn ctx_for<'a>(
        f: &'a Fx,
        game: &'a dyn Fn() -> bool,
        request: &'a AtomicU8,
        progress: &'a mut dyn FnMut(&Progress),
    ) -> RunCtx<'a> {
        RunCtx {
            conn: &f.conn,
            mods: &f.mods,
            mods_key: &f.key,
            game_running: game,
            game_check_interval: Duration::ZERO,
            request,
            progress,
            free_space: &|_| None,
            // The copy path unless a test says otherwise (the fixture is really on one drive).
            same_volume: &|_, _| Some(false),
        }
    }

    fn drive(ctx: &mut RunCtx, f: &Fx, op: Op) -> Result<Option<MoveMarker>, String> {
        match op {
            Op::Start => start(ctx, f.old.clone(), f.new.clone()),
            Op::StartTo(from, to) => start(ctx, from, to),
            Op::Resume => resume(ctx),
            Op::Cancel => cancel(ctx),
        }
    }

    /// Drive an entry point with a request flag of our own; `on_progress` may set it.
    fn run_full(
        f: &Fx,
        op: Op,
        game: &dyn Fn() -> bool,
        request: &AtomicU8,
        on_progress: &mut dyn FnMut(&Progress),
    ) -> Result<Option<MoveMarker>, String> {
        let _turn = RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut ctx = ctx_for(f, game, request, on_progress);
        drive(&mut ctx, f, op)
    }

    fn run_with(f: &Fx, op: Op, game: &dyn Fn() -> bool) -> Result<Option<MoveMarker>, String> {
        run_full(f, op, game, &AtomicU8::new(0), &mut |_| {})
    }

    #[test]
    fn forward_step_handles_every_state() {
        let f = fx("states");
        let r = add_zip(&f, "FS25_A", false);
        let layout = dest(&f);
        let mut pol = LinkPolicy::new(&layout);
        let mut m = marker(&f);
        let never = || false;
        let (old_a, new_a) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
        // old only → moved and recorded
        assert_eq!(
            forward_one(&env(&f), &r, &mut m, &mut pol, &never),
            Ok(Step::Moved)
        );
        assert!(new_a.exists() && !old_a.exists());
        assert_eq!(m.moved, vec!["FS25_A".to_string()]);
        // new only, ours → already done
        assert_eq!(
            forward_one(&env(&f), &r, &mut m, &mut pol, &never),
            Ok(Step::AlreadyDone)
        );
        // new only, NOT ours → refused, untouched
        m.moved.clear();
        assert!(matches!(
            forward_one(&env(&f), &r, &mut m, &mut pol, &never),
            Err(StepErr::Failed(_))
        ));
        assert!(new_a.exists());
        // both, identical, NOT ours → still refused: a destination Silo didn't record is never
        // adopted (recording happens before the copy, so Silo's own is always recorded)
        std::fs::write(&old_a, b"PK\x03\x04 FS25_A").unwrap();
        let refused = forward_one(&env(&f), &r, &mut m, &mut pol, &never);
        assert!(
            matches!(&refused, Err(StepErr::Failed(e)) if e.contains("won't replace it")),
            "{refused:?}"
        );
        assert!(old_a.exists() && new_a.exists());
        assert!(m.moved.is_empty());
        // both, identical, ours (crash after placing, before deleting) → old copy removed
        env(&f).record(&mut m, "FS25_A").unwrap();
        assert_eq!(
            forward_one(&env(&f), &r, &mut m, &mut pol, &never),
            Ok(Step::Moved)
        );
        assert!(!old_a.exists());
        // both, different, even if recorded → refused, un-recorded; neither file is touched
        std::fs::write(&old_a, b"PK\x03\x04 FS25_A").unwrap();
        std::fs::write(&new_a, b"someone else's build").unwrap();
        assert!(matches!(
            forward_one(&env(&f), &r, &mut m, &mut pol, &never),
            Err(StepErr::Failed(_))
        ));
        assert_eq!(std::fs::read(&old_a).unwrap(), b"PK\x03\x04 FS25_A");
        assert_eq!(std::fs::read(&new_a).unwrap(), b"someone else's build");
        assert!(m.moved.is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn an_aborted_copy_leaves_no_temp_and_keeps_the_original() {
        let f = fx("abort");
        let r = add_zip(&f, "FS25_A", false);
        let layout = dest(&f);
        let mut m = marker(&f);
        // The mod is recorded (and saved) before the copy starts…
        let recorded_first = Cell::new(false);
        let abort = || {
            let saved = load_marker(&f.conn, &f.key);
            recorded_first.set(saved.is_some_and(|s| s.moved.iter().any(|t| t == "FS25_A")));
            true
        };
        let out = forward_one(&env(&f), &r, &mut m, &mut LinkPolicy::new(&layout), &abort);
        assert_eq!(out, Err(StepErr::Interrupted));
        assert!(recorded_first.get(), "recorded before the copy");
        assert!(zip_at(&f.old, "FS25_A").exists());
        assert!(!zip_at(&f.new, "FS25_A").exists());
        assert!(!f.new.join("Tools/FS25_A.zip.silo-move-tmp").exists());
        // …and un-recorded when it isn't placed.
        assert!(m.moved.is_empty());
        assert!(load_marker(&f.conn, &f.key).unwrap().moved.is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_full_move_switches_the_setting_and_clears_the_marker() {
        let f = fx("full");
        add_zip(&f, "FS25_A", false);
        add_zip(&f, "FS25_B", false);
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        assert!(zip_at(&f.new, "FS25_A").exists() && zip_at(&f.new, "FS25_B").exists());
        assert_eq!(
            setting(&f).as_deref(),
            Some(f.new.to_string_lossy().as_ref())
        );
        assert!(load_marker(&f.conn, &f.key).is_none());
        assert!(!f.old.exists(), "the emptied default archive is removed");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn moving_back_to_the_default_clears_the_setting_and_leaves_the_users_folder() {
        let f = fx("back");
        add_zip(&f, "FS25_A", false);
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        std::fs::write(f.new.join("notes.txt"), b"mine").unwrap();
        // Spelled differently (trailing separator) — still the default, compared canonically.
        let to = PathBuf::from(format!("{}{}", f.old.display(), std::path::MAIN_SEPARATOR));
        std::fs::create_dir_all(&f.old).unwrap();
        assert!(run_with(&f, Op::StartTo(f.new.clone(), to), &|| false)
            .unwrap()
            .is_none());
        assert!(zip_at(&f.old, "FS25_A").exists());
        assert!(setting(&f).is_none());
        assert!(
            !f.new.join("Tools").exists(),
            "our emptied category folder is removed"
        );
        assert!(
            f.new.join("notes.txt").exists(),
            "the user's own file stays"
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn refuses_to_start_while_the_game_runs() {
        let f = fx("game");
        add_zip(&f, "FS25_A", false);
        assert!(run_with(&f, Op::Start, &|| true).is_err());
        assert!(zip_at(&f.old, "FS25_A").exists(), "nothing moved");
        assert!(load_marker(&f.conn, &f.key).is_none(), "nothing written");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn pauses_when_the_game_starts_and_resume_finishes() {
        let f = fx("autopause");
        add_zip(&f, "FS25_A", false);
        add_zip(&f, "FS25_B", false);
        // The game "starts" the moment FS25_A lands in the new archive.
        let new_a = zip_at(&f.new, "FS25_A");
        let game = || new_a.exists();
        let paused = run_with(&f, Op::Start, &game).unwrap().unwrap();
        assert!(paused.paused);
        assert_eq!(paused.direction, Direction::Forward);
        assert!(paused.pause_reason.is_some());
        assert!(new_a.exists());
        assert!(zip_at(&f.old, "FS25_B").exists(), "B not moved yet");
        assert!(!zip_at(&f.new, "FS25_B").exists());
        assert_eq!(load_marker(&f.conn, &f.key), Some(paused.clone()));
        // Resume with the game closed.
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert!(zip_at(&f.new, "FS25_B").exists());
        assert!(!zip_at(&f.old, "FS25_A").exists() && !zip_at(&f.old, "FS25_B").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_move_puts_back_an_update_interrupted_before_the_new_file_landed() {
        let f = fx("upd_rollback");
        add_zip(&f, "FS25_A", false);
        let a = zip_at(&f.old, "FS25_A");
        let old = crate::update::old_path(&a);
        std::fs::rename(&a, &old).unwrap(); // stopped after step b
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        assert_eq!(
            std::fs::read(zip_at(&f.new, "FS25_A")).unwrap(),
            b"PK\x03\x04 FS25_A"
        );
        assert!(!old.exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_move_never_deletes_an_update_leftover_it_cant_resolve() {
        let f = fx("upd_leftover");
        add_zip(&f, "FS25_A", false);
        let a = zip_at(&f.old, "FS25_A");
        let old = crate::update::old_path(&a);
        std::fs::write(&old, b"the previous version").unwrap();
        // The backups folder can't be made, so the old version can't be put away.
        std::fs::write(f.mods.join("backups"), b"in the way").unwrap();
        let part = crate::update::part_path(&zip_at(&f.old, "FS25_B"));
        std::fs::write(&part, b"stale download").unwrap();

        let err = run_with(&f, Op::Start, &|| false).unwrap_err();

        // Refused before any marker exists, naming the file and the cause.
        assert!(err.contains(&old.display().to_string()), "{err}");
        assert!(err.contains("Free space on"), "{err}");
        assert_eq!(
            load_marker(&f.conn, &f.key),
            None,
            "no unfinished move left behind"
        );
        assert_eq!(std::fs::read(&old).unwrap(), b"the previous version");
        assert!(
            a.exists() && !zip_at(&f.new, "FS25_A").exists(),
            "nothing moved"
        );
        // An older version's fixed-name part, never recorded: left in place, logged (#400).
        assert_eq!(std::fs::read(&part).unwrap(), b"stale download");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_cancel_finishes_despite_an_update_leftover_whose_mod_is_in_place() {
        let f = fx("upd_cancel");
        add_zip(&f, "FS25_A", false);
        add_zip(&f, "FS25_B", false);
        let new_a = zip_at(&f.new, "FS25_A");
        let game = || new_a.exists();
        let paused = run_with(&f, Op::Start, &game).unwrap().unwrap();
        assert!(paused.paused);
        // Now an update leftover appears beside B, and backups can't be made.
        let old_b = crate::update::old_path(&zip_at(&f.old, "FS25_B"));
        std::fs::write(&old_b, b"B's previous version").unwrap();
        std::fs::write(f.mods.join("backups"), b"in the way").unwrap();

        // Resuming forward pauses on it, naming it...
        let again = run_with(&f, Op::Resume, &|| false).unwrap().unwrap();
        let reason = again.pause_reason.unwrap_or_default();
        assert!(reason.contains(&old_b.display().to_string()), "{reason}");
        // ...but cancelling still finishes and gives the library back.
        assert!(run_with(&f, Op::Cancel, &|| false).unwrap().is_none());
        assert_eq!(load_marker(&f.conn, &f.key), None);
        assert!(zip_at(&f.old, "FS25_A").exists() && !new_a.exists());
        assert_eq!(std::fs::read(&old_b).unwrap(), b"B's previous version");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn move_scratch_never_includes_update_leftovers() {
        assert!(is_move_scratch("FS25_A.zip.silo-move-tmp"));
        assert!(is_move_scratch("FS25_A.zip.SILO-MOVE-OLD"));
        assert!(is_move_scratch("FS25_A.zip.silo-relink-tmp"));
        assert!(is_move_scratch(".FS25_A.zip.silo-tmp"));
        assert!(!is_move_scratch("FS25_A.zip.silo-update-old"));
        assert!(!is_move_scratch("FS25_A.zip.silo-update.part"));
        assert!(!is_move_scratch("FS25_A.zip"));
    }

    #[test]
    fn a_pause_requested_mid_run_pauses_and_resume_finishes() {
        let f = fx("pausereq");
        add_zip(&f, "FS25_A", false);
        add_zip(&f, "FS25_B", false);
        let req = AtomicU8::new(0);
        let mut prog = |p: &Progress| {
            if p.current == "FS25_B" && p.done == 1 {
                req.store(PAUSE, Ordering::SeqCst);
            }
        };
        let paused = run_full(&f, Op::Start, &|| false, &req, &mut prog)
            .unwrap()
            .unwrap();
        assert!(paused.paused && paused.pause_reason.is_none());
        assert_eq!(paused.direction, Direction::Forward);
        assert!(zip_at(&f.new, "FS25_A").exists());
        assert!(zip_at(&f.old, "FS25_B").exists() && !zip_at(&f.new, "FS25_B").exists());
        assert!(!f.new.join("Tools/FS25_B.zip.silo-move-tmp").exists());
        assert!(setting(&f).is_none());
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert!(zip_at(&f.new, "FS25_B").exists());
        assert!(setting(&f).is_some());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_cancel_requested_mid_run_brings_everything_home() {
        let f = fx("cancelreq");
        add_zip(&f, "FS25_A", false);
        add_zip(&f, "FS25_B", false);
        let req = AtomicU8::new(0);
        let mut prog = |p: &Progress| {
            if !p.cancelling && p.current == "FS25_B" {
                req.store(CANCEL, Ordering::SeqCst);
            }
        };
        assert!(run_full(&f, Op::Start, &|| false, &req, &mut prog)
            .unwrap()
            .is_none());
        assert!(zip_at(&f.old, "FS25_A").exists() && zip_at(&f.old, "FS25_B").exists());
        assert!(!zip_at(&f.new, "FS25_A").exists() && !zip_at(&f.new, "FS25_B").exists());
        assert!(setting(&f).is_none(), "setting unchanged");
        assert!(load_marker(&f.conn, &f.key).is_none());
        assert!(
            f.new.is_dir(),
            "the user's chosen folder itself is never removed"
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_cancel_removes_the_empty_category_folders_silo_made_for_aborted_copies() {
        let f = fx("cancelcats");
        add_zip_in(&f, "FS25_A", "Tools", false);
        add_zip_in(&f, "FS25_B", "Maps", false);
        add_zip_in(&f, "FS25_C", "Vehicles", false);
        // The user's own empty folders in the destination, one named like a category.
        std::fs::create_dir_all(f.new.join("Vehicles")).unwrap();
        std::fs::create_dir_all(f.new.join("Keep")).unwrap();
        let req = AtomicU8::new(0);
        // Cancel as B's copy starts: Silo has made `Maps` for it, then the copy is abandoned.
        let mut prog = |p: &Progress| {
            if !p.cancelling && p.current == "FS25_B" {
                req.store(CANCEL, Ordering::SeqCst);
            }
        };
        assert!(run_full(&f, Op::Start, &|| false, &req, &mut prog)
            .unwrap()
            .is_none());
        for (t, cat) in [
            ("FS25_A", "Tools"),
            ("FS25_B", "Maps"),
            ("FS25_C", "Vehicles"),
        ] {
            assert!(
                f.old.join(cat).join(format!("{t}.zip")).exists(),
                "{t} home"
            );
        }
        assert!(!f.new.join("Tools").exists(), "a moved-back mod's folder");
        assert!(!f.new.join("Maps").exists(), "an aborted copy's folder");
        assert!(f.new.join("Vehicles").is_dir(), "not Silo's: kept");
        assert!(f.new.join("Keep").is_dir(), "not Silo's: kept");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_cancel_keeps_a_category_folder_that_was_already_there() {
        let f = fx("cancelprecat");
        add_zip_in(&f, "FS25_A", "Tools", false);
        add_zip_in(&f, "FS25_B", "Maps", false);
        // The user's own empty folder, named like A's category: A is placed in it.
        std::fs::create_dir_all(f.new.join("Tools")).unwrap();
        let req = AtomicU8::new(0);
        let mut prog = |p: &Progress| {
            if !p.cancelling && p.current == "FS25_B" {
                req.store(CANCEL, Ordering::SeqCst);
            }
        };
        assert!(run_full(&f, Op::Start, &|| false, &req, &mut prog)
            .unwrap()
            .is_none());
        assert!(zip_at(&f.old, "FS25_A").exists() && !zip_at(&f.new, "FS25_A").exists());
        assert!(f.new.join("Tools").is_dir(), "not made by Silo: kept");
        assert!(!f.new.join("Maps").exists(), "made by Silo: removed");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_cancel_of_a_move_from_before_folders_were_recorded_tidies_as_before() {
        let f = fx("cancellegacy");
        let a = add_zip_in(&f, "FS25_A", "Tools", false);
        // Half a move by an older Silo: A placed, its marker without `created`.
        let layout = dest(&f);
        let mut m = marker(&f);
        forward_one(&env(&f), &a, &mut m, &mut LinkPolicy::new(&layout), &|| {
            false
        })
        .unwrap();
        m.created = None;
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        let json = db::get_app_setting(&f.conn, &marker_key(&f.key)).unwrap();
        assert!(!json.contains("created"), "{json}");
        assert!(run_with(&f, Op::Cancel, &|| false).unwrap().is_none());
        assert!(zip_at(&f.old, "FS25_A").exists());
        assert!(
            !f.new.join("Tools").exists(),
            "the moved mod's emptied folder"
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_category_folder_silo_made_is_kept_when_something_is_in_it() {
        let f = fx("cancelcatsfull");
        add_zip_in(&f, "FS25_A", "Tools", false);
        add_zip_in(&f, "FS25_B", "Maps", false);
        let req = AtomicU8::new(0);
        let maps = f.new.join("Maps");
        let mut prog = |p: &Progress| {
            if !p.cancelling && p.current == "FS25_B" {
                req.store(CANCEL, Ordering::SeqCst);
            }
            // While moving back, the user drops a file into the folder Silo made.
            if p.cancelling && maps.is_dir() {
                let _ = std::fs::write(maps.join("mine.txt"), b"mine");
            }
        };
        assert!(run_full(&f, Op::Start, &|| false, &req, &mut prog)
            .unwrap()
            .is_none());
        assert_eq!(std::fs::read(maps.join("mine.txt")).unwrap(), b"mine");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_cancel_requested_before_start_turns_around_at_once() {
        let f = fx("earlycancel");
        add_zip(&f, "FS25_A", false);
        add_zip(&f, "FS25_B", false);
        let req = AtomicU8::new(CANCEL);
        assert!(run_full(&f, Op::Start, &|| false, &req, &mut |_| {})
            .unwrap()
            .is_none());
        for t in ["FS25_A", "FS25_B"] {
            assert!(zip_at(&f.old, t).exists(), "{t} never left");
            assert!(!zip_at(&f.new, t).exists());
        }
        assert!(setting(&f).is_none(), "setting unchanged");
        assert!(load_marker(&f.conn, &f.key).is_none(), "marker cleared");
        assert_eq!(req.load(Ordering::SeqCst), 0, "request consumed");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_pause_requested_before_start_pauses_at_the_first_boundary() {
        let f = fx("earlypause");
        add_zip(&f, "FS25_A", false);
        let req = AtomicU8::new(PAUSE);
        let paused = run_full(&f, Op::Start, &|| false, &req, &mut |_| {})
            .unwrap()
            .unwrap();
        assert!(paused.paused && paused.moved.is_empty());
        assert_eq!(paused.direction, Direction::Forward);
        assert!(zip_at(&f.old, "FS25_A").exists() && !zip_at(&f.new, "FS25_A").exists());
        assert_eq!(load_marker(&f.conn, &f.key), Some(paused));
        assert!(setting(&f).is_none());
        // Resume (the request was consumed) finishes the move.
        assert!(run_full(&f, Op::Resume, &|| false, &req, &mut |_| {})
            .unwrap()
            .is_none());
        assert!(zip_at(&f.new, "FS25_A").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_second_cancel_while_moving_back_skips_nothing() {
        let f = fx("dblcancel");
        for t in ["FS25_A", "FS25_B", "FS25_C"] {
            add_zip(&f, t, false);
        }
        let req = AtomicU8::new(0);
        let again = Cell::new(false);
        let mut prog = |p: &Progress| {
            if !p.cancelling && p.current == "FS25_C" {
                req.store(CANCEL, Ordering::SeqCst);
            } else if p.cancelling && p.current == "FS25_A" && !again.get() {
                again.set(true);
                req.store(CANCEL, Ordering::SeqCst);
            }
        };
        assert!(run_full(&f, Op::Start, &|| false, &req, &mut prog)
            .unwrap()
            .is_none());
        assert!(again.get(), "the second cancel was sent");
        for t in ["FS25_A", "FS25_B", "FS25_C"] {
            assert!(zip_at(&f.old, t).exists(), "{t} is home");
            assert!(!zip_at(&f.new, t).exists());
        }
        assert!(setting(&f).is_none());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn cancelling_a_paused_move_reverses_only_silos_mods() {
        let f = fx("cancel");
        let a = add_zip(&f, "FS25_A", false);
        add_zip(&f, "FS25_B", false);
        // Move A by hand to simulate a half-done move, then cancel.
        let layout = dest(&f);
        let mut m = marker(&f);
        forward_one(&env(&f), &a, &mut m, &mut LinkPolicy::new(&layout), &|| {
            false
        })
        .unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(run_with(&f, Op::Cancel, &|| false).unwrap().is_none());
        assert!(zip_at(&f.old, "FS25_A").exists() && zip_at(&f.old, "FS25_B").exists());
        assert!(!zip_at(&f.new, "FS25_A").exists());
        assert!(setting(&f).is_none(), "setting unchanged");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_failed_mod_keeps_its_original_and_blocks_the_switch() {
        let f = fx("fail");
        add_zip(&f, "FS25_A", false);
        let ghost = row("FS25_Ghost", "FS25_Ghost.zip", "zip", false);
        db::upsert_organized(&f.conn, &f.key, &ghost).unwrap(); // no file anywhere
        let out = run_with(&f, Op::Start, &|| false).unwrap().unwrap();
        assert!(out.paused);
        assert_eq!(out.errors.len(), 1);
        assert!(setting(&f).is_none());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_user_file_at_the_destination_is_refused_and_survives_cancel() {
        let f = fx("userfile");
        add_zip(&f, "FS25_A", false);
        std::fs::create_dir_all(f.new.join("Tools")).unwrap();
        std::fs::write(zip_at(&f.new, "FS25_A"), b"the user's own A").unwrap();
        // B exists only at the destination, and Silo never put it there.
        let b = row("FS25_B", "FS25_B.zip", "zip", false);
        db::upsert_organized(&f.conn, &f.key, &b).unwrap();
        std::fs::write(zip_at(&f.new, "FS25_B"), b"the user's own B").unwrap();
        let out = run_with(&f, Op::Start, &|| false).unwrap().unwrap();
        assert!(out.paused);
        assert_eq!(out.errors.len(), 2, "{:?}", out.errors);
        assert!(out.errors.iter().all(|e| !e.contains("didn't put")));
        assert!(out.moved.is_empty());
        assert!(setting(&f).is_none());
        // Cancel: nothing of Silo's to move back; the user's files stay exactly as they were.
        assert!(run_with(&f, Op::Cancel, &|| false).unwrap().is_none());
        assert_eq!(
            std::fs::read(zip_at(&f.new, "FS25_A")).unwrap(),
            b"the user's own A"
        );
        assert_eq!(
            std::fs::read(zip_at(&f.new, "FS25_B")).unwrap(),
            b"the user's own B"
        );
        assert_eq!(
            std::fs::read(zip_at(&f.old, "FS25_A")).unwrap(),
            b"PK\x03\x04 FS25_A"
        );
        assert!(setting(&f).is_none());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn an_unknown_mod_in_the_archive_refuses_to_start() {
        let f = fx("unknown");
        add_zip(&f, "FS25_A", false);
        std::fs::write(zip_at(&f.old, "FS25_Stray"), b"PK").unwrap();
        let out = run_with(&f, Op::Start, &|| false).unwrap().unwrap();
        assert!(out.paused);
        assert_eq!(out.errors.len(), 1);
        assert!(
            out.errors[0].starts_with("1 mod in your archive isn't"),
            "{:?}",
            out.errors
        );
        assert_eq!(
            unknown_mods(&f.old, &db::load_organized(&f.conn, &f.key)).unwrap(),
            vec![zip_at(&f.old, "FS25_Stray")],
            "the same check preflight runs"
        );
        assert!(zip_at(&f.old, "FS25_A").exists() && !zip_at(&f.new, "FS25_A").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_half_deleted_source_recovers_on_resume() {
        let f = fx("halfdel");
        let a = add_zip(&f, "FS25_A", false);
        add_zip(&f, "FS25_B", false);
        let mut m = marker(&f);
        // A placed and recorded, then the crash hit mid-delete: only the renamed old copy is left.
        copy_verify_place(
            &env(&f),
            &zip_at(&f.old, "FS25_A"),
            &zip_at(&f.new, "FS25_A"),
            "zip",
            &|| false,
        )
        .unwrap();
        env(&f).record(&mut m, &a.tech_name).unwrap();
        // The old copy renamed aside under a recorded name, as `delete_source` does.
        let src = zip_at(&f.old, "FS25_A");
        let old_name = recorded_old(&f, &src, &zip_at(&f.new, "FS25_A"), "zip");
        std::fs::rename(&src, &old_name).unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert!(!old_name.exists(), "the recorded old copy is cleaned up");
        assert!(env(&f).journal().entries().is_empty());
        assert!(zip_at(&f.new, "FS25_A").exists() && zip_at(&f.new, "FS25_B").exists());
        assert_eq!(
            setting(&f).as_deref(),
            Some(f.new.to_string_lossy().as_ref())
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// Record an old copy of `src` the way `delete_source` does, before it's renamed aside.
    fn recorded_old(f: &Fx, src: &Path, replacement: &Path, kind: &str) -> PathBuf {
        scratch::begin(
            &env(f).journal(),
            scratch::Role::MoveOld,
            src,
            OLD_SUFFIX,
            src,
            replacement,
            kind,
        )
        .unwrap()
    }

    #[test]
    fn an_older_versions_old_copy_is_left_in_place() {
        let f = fx("legacyold");
        let a = add_zip(&f, "FS25_A", false);
        let mut m = marker(&f);
        copy_verify_place(
            &env(&f),
            &zip_at(&f.old, "FS25_A"),
            &zip_at(&f.new, "FS25_A"),
            "zip",
            &|| false,
        )
        .unwrap();
        env(&f).record(&mut m, &a.tech_name).unwrap();
        // An older Silo crashed mid-delete: its fixed name, never recorded.
        let legacy = f.old.join("Tools/FS25_A.zip.silo-move-old");
        std::fs::rename(zip_at(&f.old, "FS25_A"), &legacy).unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert_eq!(
            std::fs::read(&legacy).unwrap(),
            b"PK\x03\x04 FS25_A",
            "never deleted by name"
        );
        assert!(zip_at(&f.new, "FS25_A").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// #394's crash: the original was renamed aside, Silo stopped, and the new copy was then
    /// lost. Resume puts the old copy (now the only one) back and moves it again.
    #[test]
    fn the_only_copy_left_aside_by_a_crash_is_put_back_and_moved_again() {
        let f = fx("onlycopy");
        let a = add_zip(&f, "FS25_A", false);
        let mut m = marker(&f);
        let (src, dst) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
        copy_verify_place(&env(&f), &src, &dst, "zip", &|| false).unwrap();
        env(&f).record(&mut m, &a.tech_name).unwrap();
        let old_name = recorded_old(&f, &src, &dst, "zip");
        std::fs::rename(&src, &old_name).unwrap();
        std::fs::remove_file(&dst).unwrap(); // the new copy is lost
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert_eq!(std::fs::read(&dst).unwrap(), b"PK\x03\x04 FS25_A");
        assert!(!old_name.exists() && !src.exists());
        assert!(env(&f).journal().entries().is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A crash mid-copy leaves a recorded partial temp: Resume removes it (the original is still
    /// there) and copies again.
    #[test]
    fn a_partial_copy_left_by_a_crash_is_swept_and_redone() {
        let f = fx("partialtmp");
        let a = add_zip(&f, "FS25_A", false);
        let mut m = marker(&f);
        let (src, dst) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
        env(&f).record(&mut m, &a.tech_name).unwrap();
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        let tmp = scratch::begin(
            &env(&f).journal(),
            scratch::Role::MoveTmp,
            &dst,
            TMP_SUFFIX,
            &src,
            &dst,
            "zip",
        )
        .unwrap();
        std::fs::write(&tmp, b"PK").unwrap();
        // An older version's temp beside it: left alone.
        let legacy = suffixed_for_tests(&dst, TMP_SUFFIX);
        std::fs::write(&legacy, b"PK").unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert!(!tmp.exists());
        assert!(legacy.exists());
        assert_eq!(std::fs::read(&dst).unwrap(), b"PK\x03\x04 FS25_A");
        assert!(env(&f).journal().entries().is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    fn suffixed_for_tests(p: &Path, suffix: &str) -> PathBuf {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        p.with_file_name(format!("{name}{suffix}"))
    }

    /// Every temp a move makes goes again: none is left recorded, and none at the old fixed
    /// names.
    #[test]
    fn a_move_leaves_no_scratch_and_no_records() {
        let f = fx("noscratch");
        add_zip(&f, "FS25_A", false);
        add_hardlinked(&f, "FS25_B");
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        assert!(env(&f).journal().entries().is_empty());
        for dir in [f.new.join("Tools"), f.mods.clone()] {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                assert!(
                    !crate::scan::is_silo_temp(&n),
                    "{n} left in {}",
                    dir.display()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_copy_that_wont_fit_pauses_before_it_starts_and_resume_finishes() {
        let f = fx("noroom");
        add_zip(&f, "FS25_A", false);
        let _turn = RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let req = AtomicU8::new(0);
        let mut quiet = |_: &Progress| {};
        let full = |_: &Path| Some(10u64);
        let mut ctx = RunCtx {
            free_space: &full,
            ..ctx_for(&f, &|| false, &req, &mut quiet)
        };
        let paused = start(&mut ctx, f.old.clone(), f.new.clone())
            .unwrap()
            .unwrap();
        let reason = paused.pause_reason.unwrap_or_default();
        assert!(reason.contains("Not enough free space"), "{reason}");
        assert!(paused.errors.is_empty() && paused.moved.is_empty());
        assert!(zip_at(&f.old, "FS25_A").exists());
        assert!(
            !f.new.join("Tools").exists()
                || std::fs::read_dir(f.new.join("Tools"))
                    .unwrap()
                    .next()
                    .is_none()
        );
        assert!(env(&f).journal().entries().is_empty());
        // Room again: the move picks up where it paused.
        let roomy = |_: &Path| Some(u64::MAX / 2);
        let mut quiet = |_: &Progress| {};
        let mut ctx = RunCtx {
            free_space: &roomy,
            ..ctx_for(&f, &|| false, &req, &mut quiet)
        };
        assert!(resume(&mut ctx).unwrap().is_none());
        assert!(zip_at(&f.new, "FS25_A").exists() && !zip_at(&f.old, "FS25_A").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_relink_copy_that_wont_fit_pauses_the_move() {
        let e = std::io::Error::other(organize::NoRoom("no room".into()));
        assert_eq!(relink_err(e), StepErr::NoRoom("no room".into()));
        let e = std::io::Error::other("denied");
        assert_eq!(relink_err(e), StepErr::Failed("relink: denied".into()));
    }

    /// The end-of-move check wants the copy itself: a link at the new copy's name isn't it.
    #[test]
    fn a_link_at_the_new_copys_name_doesnt_finish_the_move() {
        let f = fx("donelink");
        let a = add_zip(&f, "FS25_A", false);
        let mut m = marker(&f);
        let (src, dst) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
        let elsewhere = f.base.join("elsewhere.zip");
        std::fs::rename(&src, &elsewhere).unwrap();
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        if organize::symlink_file(&elsewhere, &dst).is_err() {
            // No symlinks here (Windows without Developer Mode): nothing to test.
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        }
        env(&f).record(&mut m, &a.tech_name).unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        let out = run_with(&f, Op::Resume, &|| false).unwrap().unwrap();
        assert!(
            out.errors.iter().any(|e| e.contains("not fully moved")),
            "{:?}",
            out.errors
        );
        assert!(setting(&f).is_none(), "the setting isn't switched");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// Windows' folder re-point removes the old link before renaming the new one in: a crash
    /// between leaves the link missing and the new link at its recorded temp. Resume makes the
    /// link again and sweeps the temp.
    #[test]
    fn a_folder_link_lost_mid_repoint_heals_on_resume() {
        let f = fx("dirgap");
        let src = f.old.join("Tools/FS25_D");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
        let r = row("FS25_D", "FS25_D", "dir", true);
        db::upsert_organized(&f.conn, &f.key, &r).unwrap();
        let mut m = marker(&f);
        let dst = f.new.join("Tools/FS25_D");
        copy_verify_place(&env(&f), &src, &dst, "dir", &|| false).unwrap();
        env(&f).record(&mut m, "FS25_D").unwrap();
        let layout = dest(&f);
        let link = f.mods.join("FS25_D");
        let tmp = scratch::begin(
            &env(&f).journal(),
            scratch::Role::RelinkTmp,
            &link,
            RELINK_SUFFIX,
            &dst,
            &link,
            "dir",
        )
        .unwrap();
        organize::make_link(&dst, &tmp, "dir", &mut LinkPolicy::new(&layout)).unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert!(link.join("modDesc.xml").is_file(), "the link is made again");
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&dst).unwrap()
        );
        assert!(
            tmp.symlink_metadata().is_err(),
            "the recorded temp is swept"
        );
        assert!(dst.join("modDesc.xml").is_file() && !src.exists());
        assert!(env(&f).journal().entries().is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn the_old_archives_note_goes_once_a_move_leaves_it_empty() {
        let f = fx("oldnote");
        // Start from a custom archive with Silo's notes, then move back to the default.
        let custom = f.base.join("custom");
        let default = f.old.clone();
        let r = add_zip(&f, "FS25_A", false);
        assert!(
            run_with(&f, Op::StartTo(default.clone(), custom.clone()), &|| false)
                .unwrap()
                .is_none()
        );
        assert!(custom.join(ARCHIVE_NOTE).is_file() && f.mods.join(ARCHIVE_NOTE).is_file());
        std::fs::write(custom.join("my notes.txt"), b"mine").unwrap();
        assert!(
            run_with(&f, Op::StartTo(custom.clone(), default.clone()), &|| false)
                .unwrap()
                .is_none()
        );
        assert!(
            !custom.join(ARCHIVE_NOTE).exists(),
            "Silo's note is removed"
        );
        assert!(
            custom.join("my notes.txt").is_file(),
            "nothing else is touched"
        );
        assert!(custom.is_dir(), "the folder the user chose stays");
        assert!(!f.mods.join(ARCHIVE_NOTE).exists());
        assert!(organize::archive_path(&default, &r.category, &r.file_name).exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn an_old_archives_note_stays_while_it_holds_a_mod_or_names_another_folder() {
        let f = fx("oldnotekept");
        let left = f.base.join("left");
        std::fs::create_dir_all(left.join("Tools")).unwrap();
        note_archive(&f.mods, Some(&left));
        std::fs::write(left.join("Tools/FS25_Unknown.zip"), b"PK").unwrap();
        retire_note(&left, &f.mods);
        assert!(left.join(ARCHIVE_NOTE).is_file(), "it still holds a mod");
        std::fs::remove_file(left.join("Tools/FS25_Unknown.zip")).unwrap();
        retire_note(&left, &f.base.join("another-mods"));
        assert!(
            left.join(ARCHIVE_NOTE).is_file(),
            "names another mods folder"
        );
        std::fs::write(left.join(ARCHIVE_NOTE), b"the user's own file").unwrap();
        retire_note(&left, &f.mods);
        assert_eq!(
            std::fs::read(left.join(ARCHIVE_NOTE)).unwrap(),
            b"the user's own file"
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// The last look before an old copy goes compares bytes, not sizes: a replacement swapped
    /// for different bytes of the same length (a sync client, say) keeps the original.
    #[test]
    fn a_same_size_swap_of_the_new_copy_keeps_the_original() {
        let f = fx("samesize");
        add_zip(&f, "FS25_A", false);
        let (src, dst) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
        copy_verify_place(&env(&f), &src, &dst, "zip", &|| false).unwrap();
        let same_len = vec![b'X'; std::fs::metadata(&src).unwrap().len() as usize];
        std::fs::remove_file(&dst).unwrap();
        std::fs::write(&dst, &same_len).unwrap();
        let e = delete_source(&env(&f), &src, &dst, &f.new, "zip").unwrap_err();
        assert!(failed(e).contains("no longer match"));
        assert_eq!(std::fs::read(&src).unwrap(), b"PK FS25_A");
        assert!(env(&f).journal().entries().is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// Cancel → Resume: a mod whose home copy was swapped for a link stays Silo's to move back
    /// (never un-recorded), so the cancel can't finish around it.
    #[test]
    fn a_link_at_home_never_finishes_a_cancel() {
        let f = fx("homelink");
        let a = add_zip(&f, "FS25_A", false);
        let mut m = marker(&f);
        let home = zip_at(&f.old, "FS25_A");
        let elsewhere = f.base.join("elsewhere.zip");
        std::fs::rename(&home, &elsewhere).unwrap();
        if organize::symlink_file(&elsewhere, &home).is_err() {
            // No symlinks here (Windows without Developer Mode): nothing to test.
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        }
        env(&f).record(&mut m, &a.tech_name).unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        for op in [Op::Cancel, Op::Resume] {
            let out = run_with(&f, op, &|| false).unwrap().unwrap();
            assert!(out.paused, "never finished");
            assert_eq!(
                out.moved,
                vec!["FS25_A".to_string()],
                "still Silo's to move back"
            );
        }
        assert!(load_marker(&f.conn, &f.key).is_some());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A crash after the old copy was renamed aside, then the new copy was damaged while Silo
    /// was stopped: the old copy is kept (it doesn't verify against the new one), and neither
    /// Resume nor Cancel can finish around it.
    #[test]
    fn a_kept_old_copy_stops_resume_and_cancel() {
        let f = fx("keptold");
        let a = add_zip(&f, "FS25_A", false);
        let mut m = marker(&f);
        let (src, dst) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
        copy_verify_place(&env(&f), &src, &dst, "zip", &|| false).unwrap();
        env(&f).record(&mut m, &a.tech_name).unwrap();
        let old_name = recorded_old(&f, &src, &dst, "zip");
        std::fs::rename(&src, &old_name).unwrap();
        std::fs::remove_file(&dst).unwrap();
        std::fs::write(&dst, b"PK truncated").unwrap(); // damaged while Silo was stopped
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        for op in [Op::Resume, Op::Cancel, Op::Resume] {
            let out = run_with(&f, op, &|| false).unwrap().unwrap();
            assert!(
                out.errors
                    .iter()
                    .any(|e| e.contains(&old_name.display().to_string())),
                "{:?}",
                out.errors
            );
            assert!(setting(&f).is_none(), "never finished");
        }
        assert_eq!(std::fs::read(&old_name).unwrap(), b"PK FS25_A");
        assert!(
            !src.exists(),
            "the damaged copy wasn't moved back over the name"
        );
        // The old archive goes out of reach: the kept copy still stops the move.
        let aside = f.base.join("archive-aside");
        std::fs::rename(&f.old, &aside).unwrap();
        let out = run_with(&f, Op::Resume, &|| false).unwrap().unwrap();
        assert!(
            out.errors
                .iter()
                .any(|e| e.contains(&old_name.display().to_string())),
            "{:?}",
            out.errors
        );
        assert!(setting(&f).is_none(), "never finished");
        std::fs::rename(&aside, &f.old).unwrap();
        assert_eq!(std::fs::read(&old_name).unwrap(), b"PK FS25_A");
        // The user keeps the right one under the mod's name: the cancel then finishes.
        std::fs::rename(&old_name, &src).unwrap();
        std::fs::remove_file(&dst).unwrap();
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert_eq!(std::fs::read(&src).unwrap(), b"PK FS25_A");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_kept_old_copy_still_stops_a_cancel_after_another_mod_remakes_its_folder() {
        let f = fx("keptoldtwo");
        let a = add_zip(&f, "FS25_A", false);
        let b = add_zip(&f, "FS25_B", false);
        let mut m = marker(&f);
        // A finished moving.
        let (a_src, a_dst) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
        copy_verify_place(&env(&f), &a_src, &a_dst, "zip", &|| false).unwrap();
        env(&f).record(&mut m, &a.tech_name).unwrap();
        std::fs::remove_file(&a_src).unwrap();
        // B's old copy was set aside, then its new copy was damaged while Silo was stopped.
        let (b_src, b_dst) = (zip_at(&f.old, "FS25_B"), zip_at(&f.new, "FS25_B"));
        copy_verify_place(&env(&f), &b_src, &b_dst, "zip", &|| false).unwrap();
        env(&f).record(&mut m, &b.tech_name).unwrap();
        let old_name = recorded_old(&f, &b_src, &b_dst, "zip");
        std::fs::rename(&b_src, &old_name).unwrap();
        std::fs::remove_file(&b_dst).unwrap();
        std::fs::write(&b_dst, b"PK truncated").unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        // The old archive goes out of reach; the cancel moves A home, making the folder again.
        let aside = f.base.join("archive-aside");
        std::fs::rename(&f.old, &aside).unwrap();
        let out = run_with(&f, Op::Cancel, &|| false).unwrap().unwrap();
        assert!(
            out.errors
                .iter()
                .any(|e| e.contains(&old_name.display().to_string())),
            "{:?}",
            out.errors
        );
        assert!(a_src.exists(), "A came home");
        assert!(!b_src.exists(), "the damaged B wasn't copied home");
        assert!(setting(&f).is_none(), "never finished");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn start_resume_or_another_start_during_a_run_is_refused() {
        let f = fx("twice");
        add_zip(&f, "FS25_A", false);
        let resumed: Cell<Option<Result<Option<MoveMarker>, String>>> = Cell::new(None);
        let started: Cell<Option<Result<Option<MoveMarker>, String>>> = Cell::new(None);
        let req = AtomicU8::new(0);
        let no_game = || false;
        let mut prog = |_: &Progress| {
            let other = AtomicU8::new(0);
            let mut quiet = |_: &Progress| {};
            let mut ctx = ctx_for(&f, &no_game, &other, &mut quiet);
            resumed.set(Some(resume(&mut ctx)));
            started.set(Some(start(&mut ctx, f.old.clone(), f.new.clone())));
        };
        assert!(run_full(&f, Op::Start, &|| false, &req, &mut prog)
            .unwrap()
            .is_none());
        let busy = Some(Err("An archive move is already running.".to_string()));
        assert_eq!(resumed.take(), busy);
        assert_eq!(started.take(), busy);
        // Every run happens under RUN_LOCK, so while we hold it nothing else is running.
        let _turn = RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!is_running(), "released when the first run ends");
        assert!(zip_at(&f.new, "FS25_A").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn cancel_while_running_only_asks_the_run_to_turn_around() {
        let f = fx("cancelrunning");
        add_zip(&f, "FS25_A", false);
        add_zip(&f, "FS25_B", false);
        let req = AtomicU8::new(0);
        let no_game = || false;
        let sent = Cell::new(false);
        let answer: Cell<Option<Result<Option<MoveMarker>, String>>> = Cell::new(None);
        let mut prog = |p: &Progress| {
            if !p.cancelling && p.current == "FS25_B" && !sent.get() {
                sent.set(true);
                // A second caller, sharing the running move's request flag.
                let mut quiet = |_: &Progress| {};
                let mut ctx = ctx_for(&f, &no_game, &req, &mut quiet);
                answer.set(Some(cancel(&mut ctx)));
            }
        };
        assert!(run_full(&f, Op::Start, &no_game, &req, &mut prog)
            .unwrap()
            .is_none());
        assert_eq!(answer.take(), Some(Ok(None)), "returned at once");
        assert!(zip_at(&f.old, "FS25_A").exists() && zip_at(&f.old, "FS25_B").exists());
        assert!(!zip_at(&f.new, "FS25_A").exists());
        assert!(
            setting(&f).is_none(),
            "the move was cancelled, not finished"
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn entry_points_check_for_an_unfinished_move() {
        let f = fx("entries");
        add_zip(&f, "FS25_A", false);
        assert!(
            run_with(&f, Op::Resume, &|| false).is_err(),
            "nothing to resume"
        );
        assert!(
            run_with(&f, Op::Cancel, &|| false).is_err(),
            "nothing to cancel"
        );
        let mut m = marker(&f);
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert_eq!(
            run_with(&f, Op::Start, &|| false),
            Err(ALREADY_UNFINISHED.to_string())
        );
        assert!(zip_at(&f.old, "FS25_A").exists(), "nothing moved");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_linked_zip_source_is_refused_with_its_own_message() {
        let f = fx("linksrc");
        let real = f.base.join("real.zip");
        std::fs::write(&real, b"PK").unwrap();
        // File symlinks need Developer Mode on Windows; skip where they can't be made.
        if organize::symlink_file(&real, &zip_at(&f.old, "FS25_L")).is_err() {
            return;
        }
        db::upsert_organized(&f.conn, &f.key, &row("FS25_L", "FS25_L.zip", "zip", false)).unwrap();
        let out = run_with(&f, Op::Start, &|| false).unwrap().unwrap();
        assert!(out.paused);
        assert!(
            out.errors[0].contains("is a link, not a file"),
            "{:?}",
            out.errors
        );
        assert!(!zip_at(&f.new, "FS25_L").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// An active zip projected as a hardlink to its archived copy (the same-drive default).
    fn add_hardlinked(f: &Fx, tech: &str) -> PathBuf {
        add_zip(f, tech, true);
        let flat = f.mods.join(format!("{tech}.zip"));
        std::fs::hard_link(zip_at(&f.old, tech), &flat).unwrap();
        flat
    }

    #[test]
    fn an_active_hardlinked_zip_is_relinked_to_its_new_copy() {
        let f = fx("hardlink");
        let flat = add_hardlinked(&f, "FS25_A");
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        assert_eq!(std::fs::read(&flat).unwrap(), b"PK\x03\x04 FS25_A");
        // Same volume: the entry now shares the NEW copy's identity, so deleting the old copy
        // really freed its bytes (nothing else holds them).
        assert!(same_file(&flat, &zip_at(&f.new, "FS25_A")));
        assert!(!flat.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!f.mods.join("FS25_A.zip.silo-relink-tmp").exists());
        // Its projection record describes the new entry (fast, hash-free deactivate).
        assert_eq!(
            db::projection_stats(&f.conn, &f.key, "FS25_A.zip"),
            organize::file_stats(&flat)
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn cancel_relinks_an_active_hardlinked_zip_back_home() {
        let f = fx("hardlinkback");
        let flat = add_hardlinked(&f, "FS25_A");
        add_zip(&f, "FS25_B", false);
        let req = AtomicU8::new(0);
        let mut prog = |p: &Progress| {
            if !p.cancelling && p.current == "FS25_B" {
                req.store(CANCEL, Ordering::SeqCst);
            }
        };
        assert!(run_full(&f, Op::Start, &|| false, &req, &mut prog)
            .unwrap()
            .is_none());
        assert!(!zip_at(&f.new, "FS25_A").exists());
        assert!(same_file(&flat, &zip_at(&f.old, "FS25_A")));
        assert_eq!(std::fs::read(&flat).unwrap(), b"PK\x03\x04 FS25_A");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_crash_before_the_relink_is_finished_on_resume() {
        let f = fx("relinkcrash");
        let flat = add_hardlinked(&f, "FS25_A");
        let mut m = marker(&f);
        // Placed and recorded, then the crash: the entry still hardlinks the old copy.
        copy_verify_place(
            &env(&f),
            &zip_at(&f.old, "FS25_A"),
            &zip_at(&f.new, "FS25_A"),
            "zip",
            &|| false,
        )
        .unwrap();
        env(&f).record(&mut m, "FS25_A").unwrap();
        // …a relink temp from the attempt that never got renamed (recorded, a copy of the new
        // copy), and an older version's fixed-name temp.
        let tmp = scratch::begin(
            &env(&f).journal(),
            scratch::Role::RelinkTmp,
            &flat,
            RELINK_SUFFIX,
            &zip_at(&f.new, "FS25_A"),
            &flat,
            "zip",
        )
        .unwrap();
        std::fs::write(&tmp, b"PK\x03\x04 FS25_A").unwrap();
        let legacy = f.mods.join("FS25_A.zip.silo-relink-tmp");
        std::fs::write(&legacy, b"partial").unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(
            same_file(&flat, &zip_at(&f.old, "FS25_A")),
            "old link works"
        );
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert!(same_file(&flat, &zip_at(&f.new, "FS25_A")));
        assert_eq!(std::fs::read(&flat).unwrap(), b"PK\x03\x04 FS25_A");
        assert!(!tmp.exists(), "the recorded temp is swept");
        assert!(legacy.exists(), "an older version's temp is left in place");
        assert!(!zip_at(&f.old, "FS25_A").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_crash_after_the_relink_only_removes_the_old_copy() {
        let f = fx("relinkdone");
        let flat = add_hardlinked(&f, "FS25_A");
        let mut m = marker(&f);
        copy_verify_place(
            &env(&f),
            &zip_at(&f.old, "FS25_A"),
            &zip_at(&f.new, "FS25_A"),
            "zip",
            &|| false,
        )
        .unwrap();
        env(&f).record(&mut m, "FS25_A").unwrap();
        // Relinked, then the crash before the old copy went.
        std::fs::remove_file(&flat).unwrap();
        std::fs::hard_link(zip_at(&f.new, "FS25_A"), &flat).unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(run_with(&f, Op::Resume, &|| false).unwrap().is_none());
        assert!(same_file(&flat, &zip_at(&f.new, "FS25_A")));
        assert!(!zip_at(&f.old, "FS25_A").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_hardlink_is_left_standalone_when_the_new_copy_could_only_be_copied() {
        let f = fx("copyonly");
        let flat = add_hardlinked(&f, "FS25_A");
        let r = db::load_organized(&f.conn, &f.key).remove(0);
        let before = same_file::Handle::from_path(&flat).unwrap();
        let layout = dest(&f);
        // Another drive and no symlinks: re-linking would just copy the mod again.
        let mut pol = LinkPolicy::copy_only_for_tests(&layout);
        let mut m = marker(&f);
        assert_eq!(
            forward_one(&env(&f), &r, &mut m, &mut pol, &|| false),
            Ok(Step::Moved)
        );
        assert!(!zip_at(&f.old, "FS25_A").exists());
        // The same file as before — now the only link to the old bytes, still readable.
        assert_eq!(same_file::Handle::from_path(&flat).unwrap(), before);
        assert!(!same_file(&flat, &zip_at(&f.new, "FS25_A")));
        assert_eq!(std::fs::read(&flat).unwrap(), b"PK\x03\x04 FS25_A");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// Same drive, but a hardlink into the new archive isn't known to work — the mods folder
    /// is cloud-synced and rejects it, or the drive can't be identified and no probe could run —
    /// and no symlinks: the hardlink is kept as the mod's standalone copy, never traded for a
    /// copy in the mods folder that no free-space check counted (#188).
    #[test]
    fn a_hardlink_is_kept_when_a_new_hardlink_isnt_known_to_work() {
        let f = fx("nohardlink");
        let flat = add_hardlinked(&f, "FS25_A");
        let r = db::load_organized(&f.conn, &f.key).remove(0);
        let before = same_file::Handle::from_path(&flat).unwrap();
        let layout = dest(&f);
        let mut pol = LinkPolicy::decided_for_tests(&layout, false, false, false);
        let mut m = marker(&f);
        assert_eq!(
            forward_one(&env(&f), &r, &mut m, &mut pol, &|| false),
            Ok(Step::Moved)
        );
        assert!(!zip_at(&f.old, "FS25_A").exists());
        assert!(zip_at(&f.new, "FS25_A").exists());
        assert_eq!(same_file::Handle::from_path(&flat).unwrap(), before);
        assert_eq!(std::fs::read(&flat).unwrap(), b"PK FS25_A");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// `reaches_the_copy` wants the right kind of thing behind the link: a file for a zip, a
    /// folder for a folder mod (#188).
    #[test]
    fn a_new_link_must_reach_the_right_kind_of_copy() {
        let f = fx("reaches");
        let file = f.mods.join("FS25_A.zip");
        std::fs::write(&file, b"PK").unwrap();
        assert_eq!(reaches_the_copy(&file, "zip"), Ok(()));
        assert!(reaches_the_copy(&file, "dir").is_err());
        assert_eq!(reaches_the_copy(&f.old, "dir"), Ok(()));
        assert!(
            reaches_the_copy(&f.old, "zip").is_err(),
            "a folder isn't a zip"
        );
        let e = reaches_the_copy(&f.mods.join("FS25_Gone.zip"), "zip").unwrap_err();
        assert!(
            e.contains("Silo couldn't open the mod through its new link"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// Right before the old copy goes, the new one must be itself and of the mod's kind: gone,
    /// the wrong kind, or swapped for a link all stop the step, and `forward_one` then keeps
    /// the original (#188).
    #[test]
    fn a_vanished_or_swapped_new_copy_keeps_the_original() {
        let f = fx("stillthere");
        let r = add_zip(&f, "FS25_A", false);
        let src = zip_at(&f.old, "FS25_A");
        let dst = zip_at(&f.new, "FS25_A");
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(&src, &dst).unwrap();
        assert!(still_there(&src, &dst, "zip").is_ok());
        assert!(still_there(&src, &dst, "dir").is_err());
        assert!(still_there(&src, &f.new, "zip").is_err());
        let gone = zip_at(&f.new, "FS25_Gone");
        match still_there(&src, &gone, "zip") {
            Err(StepErr::Failed(e)) => assert!(e.contains("so Silo kept the original"), "{e}"),
            other => panic!("{other:?}"),
        }
        // Replaced since it was verified (another size), on either side: both kept.
        std::fs::write(&dst, b"something else entirely").unwrap();
        match still_there(&src, &dst, "zip") {
            Err(StepErr::Failed(e)) => assert!(e.contains("so Silo kept both"), "{e}"),
            other => panic!("{other:?}"),
        }
        std::fs::copy(&src, &dst).unwrap();
        let other = f.base.join("other.zip");
        std::fs::write(&other, b"a different file").unwrap();
        assert!(still_there(&other, &dst, "zip").is_err());
        // A folder mod: a folder with other files than the original isn't it.
        let (fa, fb) = (f.base.join("fa"), f.base.join("fb"));
        std::fs::create_dir_all(&fa).unwrap();
        std::fs::create_dir_all(&fb).unwrap();
        std::fs::write(fa.join("modDesc.xml"), b"<modDesc/>").unwrap();
        assert!(
            still_there(&fa, &fb, "dir").is_err(),
            "an empty folder isn't the copy"
        );
        std::fs::write(fb.join("modDesc.xml"), b"<modDesc/>").unwrap();
        assert!(still_there(&fa, &fb, "dir").is_ok());
        // Swapped for a link back to the original: refused, and a step keeps the original.
        std::fs::remove_file(&dst).unwrap();
        if organize::symlink_file(&src, &dst).is_ok() {
            assert!(still_there(&src, &dst, "zip").is_err());
            let mut m = marker(&f);
            env(&f).record(&mut m, "FS25_A").unwrap();
            let layout = dest(&f);
            let mut pol = LinkPolicy::copy_only_for_tests(&layout);
            assert!(matches!(
                forward_one(&env(&f), &r, &mut m, &mut pol, &|| false),
                Err(StepErr::Failed(_))
            ));
            assert_eq!(std::fs::read(&src).unwrap(), b"PK\x03\x04 FS25_A");
        }
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A retried step whose mods-folder link already points at the new copy, which has gone
    /// again: the step stops (so the old copy isn't deleted) instead of treating the link as
    /// someone else's (#188).
    #[test]
    fn a_retried_link_to_a_vanished_new_copy_stops_the_step() {
        let f = fx("retrylink");
        let r = add_zip(&f, "FS25_A", true);
        let link = f.mods.join("FS25_A.zip");
        let dst = zip_at(&f.new, "FS25_A");
        if organize::symlink_file(&dst, &link).is_err() {
            // No symlinks here (Windows without Developer Mode): nothing to test.
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        }
        let layout = dest(&f);
        let mut pol = LinkPolicy::decided_for_tests(&layout, true, true, false);
        let e =
            failed(repoint(&env(&f), &r, &zip_at(&f.old, "FS25_A"), &dst, &mut pol).unwrap_err());
        assert!(
            e.contains("couldn't open the mod through its new link"),
            "{e}"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), dst, "left as it was");
        assert!(zip_at(&f.old, "FS25_A").exists());
        // Once the new copy is back, the retry passes.
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(zip_at(&f.old, "FS25_A"), &dst).unwrap();
        assert_eq!(
            repoint(&env(&f), &r, &zip_at(&f.old, "FS25_A"), &dst, &mut pol),
            Ok(())
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A symlink of the user's own in the mods folder, under an active mod's name, pointing at
    /// a file that isn't there (an unplugged drive): a move leaves it exactly as it is.
    #[test]
    fn a_dangling_symlink_of_the_users_own_survives_a_move() {
        let f = fx("userlink");
        add_zip(&f, "FS25_A", true);
        let link = f.mods.join("FS25_A.zip");
        let theirs = f.base.join("usb/FS25_A.zip");
        if organize::symlink_file(&theirs, &link).is_err() {
            // No symlinks here (Windows without Developer Mode): nothing to test.
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        }
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), theirs);
        assert!(zip_at(&f.new, "FS25_A").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// Where a hardlink is known to work, the hardlink is moved to the new copy as before.
    #[test]
    fn a_hardlink_follows_the_mod_when_a_new_hardlink_works() {
        let f = fx("hardlinkworks");
        let flat = add_hardlinked(&f, "FS25_A");
        let r = db::load_organized(&f.conn, &f.key).remove(0);
        let layout = dest(&f);
        let mut pol = LinkPolicy::decided_for_tests(&layout, false, false, true);
        let mut m = marker(&f);
        assert_eq!(
            forward_one(&env(&f), &r, &mut m, &mut pol, &|| false),
            Ok(Step::Moved)
        );
        assert!(same_file(&flat, &zip_at(&f.new, "FS25_A")));
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// `relink_cost` sorts active zips the way `repoint` does: a hardlink to the archived copy
    /// is kept, a missing entry (or a symlink into the archive) is made again. A standalone
    /// copy, a parked mod and a folder mod cost nothing (#188).
    #[test]
    fn relink_cost_counts_kept_hardlinks_and_entries_made_again() {
        let f = fx("relinkcost");
        let size = |tech: &str| std::fs::metadata(zip_at(&f.old, tech)).unwrap().len();
        add_hardlinked(&f, "FS25_Linked");
        add_zip(&f, "FS25_Missing", true);
        add_zip(&f, "FS25_Copied", true);
        std::fs::copy(
            zip_at(&f.old, "FS25_Copied"),
            f.mods.join("FS25_Copied.zip"),
        )
        .unwrap();
        add_zip(&f, "FS25_Parked", false);
        let dir = row("FS25_Dir", "FS25_Dir", "dir", true);
        std::fs::create_dir_all(f.old.join("Tools/FS25_Dir")).unwrap();
        std::fs::write(f.old.join("Tools/FS25_Dir/modDesc.xml"), b"<modDesc/>").unwrap();
        db::upsert_organized(&f.conn, &f.key, &dir).unwrap();
        let rows = db::load_organized(&f.conn, &f.key);
        let mut want = RelinkCost {
            kept: size("FS25_Linked"),
            copied_zips: size("FS25_Missing"),
            copied_dirs: size_of(&f.old.join("Tools/FS25_Dir")),
            largest_copied_zip: 0,
            largest_copied_dir: 0,
            largest_unkept: ["FS25_Missing", "FS25_Copied", "FS25_Parked"]
                .iter()
                .map(|t| size(t))
                .chain([size_of(&f.old.join("Tools/FS25_Dir"))])
                .max()
                .unwrap(),
        };
        want.largest_copied_zip = size("FS25_Missing");
        want.largest_copied_dir = want.copied_dirs;
        assert!(want.copied_dirs > 0);
        assert_eq!(relink_cost(&f.mods, &f.old, &rows), want);
        // A symlink to the archived copy is made again too; one pointing anywhere else —
        // dangling or not — is the user's and costs nothing.
        let sym = f.mods.join("FS25_Copied.zip");
        std::fs::remove_file(&sym).unwrap();
        if organize::symlink_file(&zip_at(&f.old, "FS25_Copied"), &sym).is_ok() {
            want.copied_zips += size("FS25_Copied");
            want.largest_copied_zip = want.largest_copied_zip.max(size("FS25_Copied"));
            assert_eq!(relink_cost(&f.mods, &f.old, &rows), want);
            std::fs::remove_file(&sym).unwrap();
            organize::symlink_file(&f.base.join("gone.zip"), &sym).unwrap();
            want.copied_zips -= size("FS25_Copied");
            want.largest_copied_zip = size("FS25_Missing");
            assert_eq!(relink_cost(&f.mods, &f.old, &rows), want);
        }
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn an_older_relink_temp_is_left_and_a_recorded_one_is_swept_at_start() {
        let f = fx("relinktmp");
        add_zip(&f, "FS25_A", false); // parked: its step never touches the mods folder
                                      // The fixed name older versions used: Silo can't prove it made it.
        let legacy = f.mods.join("FS25_A.zip.silo-relink-tmp");
        std::fs::write(&legacy, b"PK\x03\x04 FS25_A").unwrap();
        // A recorded one, a hardlink of the archived copy: provably redundant.
        let flat = f.mods.join("FS25_A.zip");
        let archived = zip_at(&f.old, "FS25_A");
        let tmp = scratch::begin(
            &env(&f).journal(),
            scratch::Role::RelinkTmp,
            &flat,
            RELINK_SUFFIX,
            &archived,
            &flat,
            "zip",
        )
        .unwrap();
        std::fs::hard_link(&archived, &tmp).unwrap();
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        assert!(!tmp.exists());
        assert!(legacy.exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn the_users_own_file_in_the_mods_folder_is_never_relinked() {
        let f = fx("userflat");
        add_zip(&f, "FS25_A", true);
        let flat = f.mods.join("FS25_A.zip");
        std::fs::write(&flat, b"the user's own build").unwrap();
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        assert_eq!(std::fs::read(&flat).unwrap(), b"the user's own build");
        assert!(!same_file(&flat, &zip_at(&f.new, "FS25_A")));
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn copies_keep_their_modified_time() {
        let f = fx("mtime");
        let when = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let set = |p: &Path| {
            std::fs::OpenOptions::new()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(when)
                .unwrap()
        };
        add_zip(&f, "FS25_A", false);
        set(&zip_at(&f.old, "FS25_A"));
        let dir = f.old.join("Tools/FS25_D");
        std::fs::create_dir_all(dir.join("xml")).unwrap();
        std::fs::write(dir.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::write(dir.join("xml/a.xml"), b"<a/>").unwrap();
        set(&dir.join("modDesc.xml"));
        set(&dir.join("xml/a.xml"));
        db::upsert_organized(&f.conn, &f.key, &row("FS25_D", "FS25_D", "dir", false)).unwrap();
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        let mtime = |p: PathBuf| std::fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(mtime(zip_at(&f.new, "FS25_A")), when);
        assert_eq!(mtime(f.new.join("Tools/FS25_D/modDesc.xml")), when);
        assert_eq!(mtime(f.new.join("Tools/FS25_D/xml/a.xml")), when);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn organize_temps_are_not_unknown_mods_and_a_move_leaves_them() {
        let f = fx("orgtemp");
        let r = add_zip(&f, "FS25_A", false);
        // A cross-volume organize's leftovers: a zip temp and a folder temp with a modDesc.
        let zip_tmp = f.old.join("Tools/.FS25_B.zip.silo-tmp");
        std::fs::write(&zip_tmp, b"PK").unwrap();
        let dir_tmp = f.old.join("Tools/.FS25_C.silo-tmp");
        std::fs::create_dir_all(&dir_tmp).unwrap();
        std::fs::write(dir_tmp.join("modDesc.xml"), b"<modDesc/>").unwrap();
        assert!(unknown_mods(&f.old, std::slice::from_ref(&r))
            .unwrap()
            .is_empty());
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        // Unrecorded: logged and left in place, never deleted by name (#394).
        assert!(zip_tmp.exists() && dir_tmp.join("modDesc.xml").exists());
        assert!(zip_at(&f.new, "FS25_A").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_mod_filed_twice_is_named_and_not_called_unknown() {
        let f = fx("twice2");
        let r = add_zip(&f, "FS25_A", false);
        std::fs::create_dir_all(f.old.join("Maps")).unwrap();
        std::fs::write(f.old.join("Maps/FS25_A.zip"), b"PK\x03\x04 FS25_A").unwrap();
        std::fs::write(zip_at(&f.old, "FS25_Stray"), b"PK").unwrap();
        let rows = vec![r];
        let unknown = unknown_mods(&f.old, &rows).unwrap();
        assert_eq!(unknown.len(), 2);
        assert_eq!(filed_twice(&unknown, &rows), vec!["FS25_A.zip".to_string()]);
        let out = run_with(&f, Op::Start, &|| false).unwrap().unwrap();
        assert!(out.paused);
        assert!(
            out.errors
                .iter()
                .any(|e| e.contains("FS25_A.zip is filed twice") && e.contains("Remove one copy")),
            "{:?}",
            out.errors
        );
        assert!(
            out.errors
                .iter()
                .any(|e| e.starts_with("1 mod in your archive")),
            "{:?}",
            out.errors
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn filed_mods_reports_any_read_error_and_treats_a_missing_archive_as_empty() {
        let f = fx("filed");
        add_zip(&f, "FS25_A", false);
        assert_eq!(filed_mods(&f.old).unwrap(), vec![zip_at(&f.old, "FS25_A")]);
        assert!(filed_mods(&f.base.join("nowhere")).unwrap().is_empty());
        // A category folder that can't be listed is an error, not "no mods".
        let list = |p: &Path| -> Listing {
            if p == f.old.as_path() {
                Ok(vec![(f.old.join("Tools"), true)])
            } else {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
        };
        // ... and it names the folder, for the "can't read" problem a move shows.
        let e = filed_mods_with(&f.old, &list).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
        let msg = unreadable_archive_message(&e, "then try again.");
        assert!(
            msg.starts_with("Silo can't read ")
                && msg.contains(&f.old.join("Tools").display().to_string()),
            "{msg}"
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    // A category folder Silo can't list may hold unrecorded mods: the move pauses with nothing
    // moved, rather than leaving them behind in an archive it no longer tracks.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_category_pauses_the_move_before_anything_moves() {
        use std::os::unix::fs::PermissionsExt;
        let f = fx("unreadable");
        add_zip(&f, "FS25_A", false);
        let locked = f.old.join("Maps");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("FS25_Stray.zip"), b"PK").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(&locked).is_ok() {
            // Running as root: permissions don't stop a listing, so there's nothing to test.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        }
        let rows = db::load_organized(&f.conn, &f.key);
        assert!(unknown_mods(&f.old, &rows).is_err());
        let out = run_with(&f, Op::Start, &|| false).unwrap().unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(out.paused);
        assert!(
            out.errors[0].starts_with("Silo can't read "),
            "{:?}",
            out.errors
        );
        assert!(zip_at(&f.old, "FS25_A").exists() && !zip_at(&f.new, "FS25_A").exists());
        assert!(locked.join("FS25_Stray.zip").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    // The category lists fine, but an unrecorded folder mod in it can't be looked into: that's
    // "can't read", not "not a mod".
    #[cfg(unix)]
    #[test]
    fn a_folder_mod_that_cant_be_looked_into_is_an_error_not_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let f = fx("unreadable_mod");
        let stray = f.old.join("Tools/FS25_Stray");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::set_permissions(&stray, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::metadata(stray.join("modDesc.xml")).is_ok() {
            // Running as root: permissions don't stop the look, so there's nothing to test.
            std::fs::set_permissions(&stray, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        }
        let res = unknown_mods(&f.old, &[]);
        std::fs::set_permissions(&stray, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = res.unwrap_err();
        assert!(e.to_string().contains("FS25_Stray"), "{e}");
        // A plain folder without a modDesc is still just "not a mod".
        std::fs::create_dir_all(f.old.join("Tools/notes")).unwrap();
        assert_eq!(unknown_mods(&f.old, &[]).unwrap(), vec![stray]);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    fn note_fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("silo_note_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (mods, hdd) = (base.join("mods"), base.join("hdd"));
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(&hdd).unwrap();
        (base, mods, hdd)
    }

    #[test]
    fn a_custom_archive_is_found_again_after_the_database_is_lost() {
        let (base, mods, hdd) = note_fixture("found");
        note_archive(&mods, Some(&hdd));
        assert!(mods.join(ARCHIVE_NOTE).is_file() && hdd.join(ARCHIVE_NOTE).is_file());
        assert_eq!(rediscover_archive(&mods).as_deref(), Some(hdd.as_path()));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_pointer_the_archive_doesnt_confirm_is_ignored() {
        let (base, mods, hdd) = note_fixture("unconfirmed");
        let other = base.join("other-mods");
        std::fs::create_dir_all(&other).unwrap();
        note_archive(&other, Some(&hdd)); // hdd's note now names other-mods
        std::fs::write(
            mods.join(ARCHIVE_NOTE),
            serde_json::json!({ "silo": 1, "archive": hdd }).to_string(),
        )
        .unwrap();
        assert_eq!(rediscover_archive(&mods), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// An archive on a drive (Windows) or removable-media mount (elsewhere) that isn't there.
    fn on_an_absent_drive() -> PathBuf {
        #[cfg(windows)]
        let root = ('D'..='Z')
            .rev()
            .map(|d| PathBuf::from(format!("{d}:\\")))
            .find(|r| !r.exists())
            .expect("a free drive letter");
        #[cfg(not(windows))]
        let root = PathBuf::from(format!("/media/silo-absent-{}", std::process::id()));
        root.join("SiloArchive")
    }

    fn point_mods_at(mods: &Path, archive: &str) {
        std::fs::write(
            mods.join(ARCHIVE_NOTE),
            serde_json::json!({ "silo": 1, "archive": archive }).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn an_unplugged_archive_is_still_found() {
        let (base, mods, _) = note_fixture("unplugged");
        let gone = on_an_absent_drive();
        note_archive(&mods, Some(&gone)); // only the mods folder's note can be written
        assert_eq!(rediscover_archive(&mods).as_deref(), Some(gone.as_path()));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_folder_on_a_present_drive_is_not_found() {
        let (base, mods, hdd) = note_fixture("deleted");
        note_archive(&mods, Some(&hdd));
        std::fs::remove_dir_all(&hdd).unwrap(); // deleted, not unplugged: the drive is there
        assert_eq!(rediscover_archive(&mods), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_pointer_that_isnt_a_usable_folder_is_ignored() {
        let (base, mods, _) = note_fixture("unusable");
        let inside = mods.join("stuff").to_string_lossy().into_owned();
        for archive in ["", "relative/archive", r"\\server\share\archive", &inside] {
            point_mods_at(&mods, archive);
            assert_eq!(rediscover_archive(&mods), None, "{archive:?}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_default_archive_holding_mods_wins_over_a_stale_pointer() {
        let (base, mods, hdd) = note_fixture("default_wins");
        note_archive(&mods, Some(&hdd));
        let cat = mods.join("archive").join("Misc");
        std::fs::create_dir_all(&cat).unwrap();
        std::fs::write(cat.join("FS25_A.zip"), b"zip").unwrap();
        assert_eq!(rediscover_archive(&mods), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_file_by_that_name_silo_didnt_write_is_never_replaced_or_removed() {
        let (base, mods, hdd) = note_fixture("foreign_note");
        std::fs::write(mods.join(ARCHIVE_NOTE), b"my own notes").unwrap();
        note_archive(&mods, Some(&hdd));
        assert_eq!(
            std::fs::read(mods.join(ARCHIVE_NOTE)).unwrap(),
            b"my own notes"
        );
        assert!(
            read_note(&hdd).is_some(),
            "the archive's note is still written"
        );
        note_archive(&mods, None);
        assert_eq!(
            std::fs::read(mods.join(ARCHIVE_NOTE)).unwrap(),
            b"my own notes"
        );
        for dir in [&mods, &hdd] {
            let scratch: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
                .collect();
            assert!(
                scratch.is_empty(),
                "no scratch file left in {}",
                dir.display()
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn going_back_to_the_default_archive_removes_the_pointer() {
        let (base, mods, hdd) = note_fixture("back_default");
        note_archive(&mods, Some(&hdd));
        note_archive(&mods, None);
        assert!(!mods.join(ARCHIVE_NOTE).exists());
        assert_eq!(rediscover_archive(&mods), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn storing_the_archive_setting_writes_and_clears_the_notes() {
        let f = fx("store_notes");
        store_archive_setting(&f.conn, &f.key, &f.mods, &f.new).unwrap();
        assert_eq!(
            rediscover_archive(&f.mods).as_deref(),
            Some(f.new.as_path())
        );
        store_archive_setting(&f.conn, &f.key, &f.mods, &f.mods.join("archive")).unwrap();
        assert!(!f.mods.join(ARCHIVE_NOTE).exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn store_archive_setting_clears_it_for_the_default() {
        let f = fx("store");
        store_archive_setting(&f.conn, &f.key, &f.mods, &f.new).unwrap();
        assert_eq!(
            setting(&f).as_deref(),
            Some(f.new.to_string_lossy().as_ref())
        );
        store_archive_setting(&f.conn, &f.key, &f.mods, &f.mods.join("archive")).unwrap();
        assert!(setting(&f).is_none());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn an_active_mod_missing_from_the_mods_folder_is_linked_to_its_new_copy() {
        let f = fx("missinglink");
        add_zip(&f, "FS25_A", true);
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        assert_eq!(
            std::fs::read(f.mods.join("FS25_A.zip")).unwrap(),
            b"PK\x03\x04 FS25_A"
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_folder_mod_moves_with_its_nested_files() {
        let f = fx("folder");
        let src = f.old.join("Tools/FS25_D");
        std::fs::create_dir_all(src.join("xml/deep")).unwrap();
        std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::write(src.join("xml/deep/a.xml"), b"<a/>").unwrap();
        db::upsert_organized(&f.conn, &f.key, &row("FS25_D", "FS25_D", "dir", false)).unwrap();
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        let moved = f.new.join("Tools/FS25_D");
        assert_eq!(
            std::fs::read(moved.join("xml/deep/a.xml")).unwrap(),
            b"<a/>"
        );
        assert!(moved.join("modDesc.xml").is_file());
        assert!(!src.exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn folder_verify_compares_each_path_not_just_totals() {
        let f = fx("dirverify");
        let (a, b) = (f.base.join("a"), f.base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("x.xml"), b"12").unwrap();
        std::fs::write(b.join("y.xml"), b"34").unwrap(); // same count + total size
        assert_eq!(verify(&a, &b, "dir", Check::Listing), Ok(false));
        assert_eq!(verify(&a, &b, "dir", Check::Contents), Ok(false));
        std::fs::remove_file(b.join("y.xml")).unwrap();
        std::fs::write(b.join("x.xml"), b"34").unwrap();
        // Same paths and sizes, different bytes: only a contents check tells them apart.
        assert_eq!(verify(&a, &b, "dir", Check::Listing), Ok(true));
        assert_eq!(verify(&a, &b, "dir", Check::Contents), Ok(false));
        std::fs::write(b.join("x.xml"), b"12").unwrap();
        assert_eq!(verify(&a, &b, "dir", Check::Contents), Ok(true));
        let _ = std::fs::remove_dir_all(&f.base);
    }

    // Paused after placing a folder mod's copy but before deleting the original: an edit to the
    // original that keeps every size must not pass the resume check, or the edit is deleted.
    #[test]
    fn a_same_size_edit_while_paused_keeps_the_folder_original() {
        let f = fx("dirresume");
        let src = f.old.join("Tools/FS25_D");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::write(src.join("a.xml"), b"<v>1</v>").unwrap();
        let r = row("FS25_D", "FS25_D", "dir", false);
        db::upsert_organized(&f.conn, &f.key, &r).unwrap();
        // The earlier run's placed copy, recorded as Silo's.
        let dst = f.new.join("Tools/FS25_D");
        copy_dir_abortable(&src, &dst, &|| false).unwrap();
        let mut m = marker(&f);
        env(&f).record(&mut m, "FS25_D").unwrap();
        // The user edits the original while the move is paused: same length, new value.
        std::fs::write(src.join("a.xml"), b"<v>2</v>").unwrap();

        let layout = dest(&f);
        let mut pol = LinkPolicy::new(&layout);
        let out = forward_one(&env(&f), &r, &mut m, &mut pol, &|| false);
        assert!(matches!(out, Err(StepErr::Failed(_))), "{out:?}");
        assert_eq!(std::fs::read(src.join("a.xml")).unwrap(), b"<v>2</v>");
        assert!(dst.exists(), "the copy is left for the user to check");
        assert!(
            m.moved.is_empty(),
            "no longer claimed, so a cancel leaves it alone"
        );

        // An untouched original still resumes: the copy matches, the original goes.
        std::fs::write(src.join("a.xml"), b"<v>1</v>").unwrap();
        env(&f).record(&mut m, "FS25_D").unwrap();
        assert_eq!(
            forward_one(&env(&f), &r, &mut m, &mut pol, &|| false),
            Ok(Step::Moved)
        );
        assert!(!src.exists());
        assert_eq!(std::fs::read(dst.join("a.xml")).unwrap(), b"<v>1</v>");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn collisions_finds_names_already_at_the_destination() {
        let f = fx("collide");
        std::fs::create_dir_all(f.new.join("Tools")).unwrap();
        std::fs::create_dir_all(f.new.join("Maps/FS25_Y")).unwrap();
        std::fs::write(f.new.join("Tools/FS25_X.zip"), b"PK").unwrap();
        std::fs::write(f.new.join("FS25_Top.zip"), b"PK").unwrap(); // top level isn't checked
        let rows = vec![
            row("FS25_X", "FS25_X.zip", "zip", false), // exact path
            row("FS25_Y", "FS25_Y.zip", "zip", false), // same techName as a folder elsewhere
            row("FS25_Top", "FS25_Top.zip", "zip", false),
            row("FS25_Free", "FS25_Free.zip", "zip", false),
        ];
        let hits = collisions(&f.new, &rows);
        assert_eq!(hits.len(), 2, "{hits:?}");
        let tools = f.new.join("Tools").display().to_string();
        let maps = f.new.join("Maps").display().to_string();
        assert_eq!(
            hits[0],
            format!("{tools} already has a different file named FS25_X.zip.")
        );
        assert_eq!(
            hits[1],
            format!("{maps} already has a folder named FS25_Y. Another copy of FS25_Y.")
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[cfg(unix)]
    #[test]
    fn an_active_symlinked_folder_mod_is_repointed() {
        let f = fx("dirlink");
        let src = f.old.join("Tools/FS25_D");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
        db::upsert_organized(&f.conn, &f.key, &row("FS25_D", "FS25_D", "dir", true)).unwrap();
        std::os::unix::fs::symlink(&src, f.mods.join("FS25_D")).unwrap();
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        let link = f.mods.join("FS25_D");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(f.new.join("Tools/FS25_D")).unwrap()
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[cfg(unix)]
    #[test]
    fn an_active_symlinked_zip_is_repointed() {
        let f = fx("ziplink");
        add_zip(&f, "FS25_A", true);
        let link = f.mods.join("FS25_A.zip");
        std::os::unix::fs::symlink(zip_at(&f.old, "FS25_A"), &link).unwrap();
        assert!(run_with(&f, Op::Start, &|| false).unwrap().is_none());
        // Re-linked to the new copy (a hardlink on the same volume, else a symlink).
        assert_eq!(
            same_file::Handle::from_path(&link).unwrap(),
            same_file::Handle::from_path(zip_at(&f.new, "FS25_A")).unwrap()
        );
        assert!(!f.mods.join("FS25_A.zip.silo-relink-tmp").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_a_folder_mod_is_refused() {
        let f = fx("innerlink");
        let src = f.old.join("Tools/FS25_D");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::os::unix::fs::symlink("/etc", src.join("escape")).unwrap();
        db::upsert_organized(&f.conn, &f.key, &row("FS25_D", "FS25_D", "dir", false)).unwrap();
        let out = run_with(&f, Op::Start, &|| false).unwrap().unwrap();
        assert!(
            out.paused && out.errors[0].contains("link"),
            "{:?}",
            out.errors
        );
        assert!(src.join("modDesc.xml").exists());
        assert!(!f.new.join("Tools/FS25_D").exists());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn marker_round_trips_and_reads_defaults() {
        let f = fx("marker");
        let mut m = marker(&f);
        m.direction = Direction::Back;
        m.moved = vec!["FS25_A".into()];
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert_eq!(load_marker(&f.conn, &f.key), Some(m));
        let minimal = r#"{"from":"/a","to":"/b","direction":"forward"}"#;
        let parsed: MoveMarker = serde_json::from_str(minimal).unwrap();
        assert!(!parsed.paused && parsed.moved.is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn path_and_space_checks() {
        let same = |a: &Path, b: &Path| a == b;
        // Absolute on every OS (Layout::new requires it); never created.
        let base = std::env::temp_dir().join(format!("silo_reloc_paths_{}", std::process::id()));
        let mods = base.join("m");
        let current = mods.join("archive");
        let hdd = base.join("hdd").join("a");
        // already there
        assert!(!path_problems(&current, &current, &mods, &[], same).is_empty());
        // inside the mods folder
        assert!(!path_problems(&current, &mods.join("sub"), &mods, &[], same).is_empty());
        // another folder's archive
        assert!(!path_problems(&current, &hdd, &mods, std::slice::from_ref(&hdd), same).is_empty());
        // Layout::new's rules: relative, the mods folder itself, a parent of the mods folder
        assert!(!path_problems(&current, Path::new("rel/a"), &mods, &[], same).is_empty());
        assert!(!path_problems(&current, &mods, &mods, &[], same).is_empty());
        assert!(!path_problems(&current, &base, &mods, &[], same).is_empty());
        // fine
        assert!(path_problems(&current, &hdd, &mods, &[], same).is_empty());
        assert!(space_problem(10 << 30, Some(5 << 30)).is_some());
        assert!(space_problem(1 << 30, Some(5 << 30)).is_none());
        assert!(space_problem(1 << 30, None).is_none());
    }

    #[test]
    fn finds_loose_mods_at_the_archive_top_level_only() {
        let f = fx("loose");
        std::fs::write(f.new.join("FS25_X.zip"), b"PK").unwrap();
        std::fs::create_dir_all(f.new.join("FS25_Y")).unwrap();
        std::fs::write(f.new.join("FS25_Y/modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::create_dir_all(f.new.join("Tools")).unwrap(); // a category folder, not a mod
        let mut found: Vec<_> = loose_mods(&f.new)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, vec!["FS25_X.zip", "FS25_Y"]);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    // Same-drive moves rename each mod (#401).

    /// `p`'s identity witness: a hardlink outside the archives (no handle kept open, so a
    /// folder holding it can still be renamed on Windows). A copy never shares it.
    fn witness(f: &Fx, p: &Path, name: &str) -> PathBuf {
        let w = f.base.join(format!("witness-{name}"));
        std::fs::hard_link(p, &w).unwrap();
        w
    }

    fn add_dir(f: &Fx, tech: &str, active: bool) -> OrganizedRow {
        let src = f.old.join("Tools").join(tech);
        std::fs::create_dir_all(src.join("textures")).unwrap();
        std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::write(src.join("textures").join("a.dds"), tech.as_bytes()).unwrap();
        let r = row(tech, tech, "dir", active);
        db::upsert_organized(&f.conn, &f.key, &r).unwrap();
        r
    }

    fn one_drive(_: &Path, _: &Path) -> Option<bool> {
        Some(true)
    }

    /// Drive an entry point with both archives on one drive and no free space at all: a copy
    /// would pause for room, a rename needs none.
    fn run_on_one_drive(
        f: &Fx,
        op: Op,
        request: &AtomicU8,
        on_progress: &mut dyn FnMut(&Progress),
    ) -> Result<Option<MoveMarker>, String> {
        let _turn = RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut ctx = RunCtx {
            same_volume: &one_drive,
            free_space: &|_| Some(0),
            ..ctx_for(f, &|| false, request, on_progress)
        };
        drive(&mut ctx, f, op)
    }

    fn one_drive_run(f: &Fx, op: Op) -> Result<Option<MoveMarker>, String> {
        run_on_one_drive(f, op, &AtomicU8::new(0), &mut |_| {})
    }

    fn no_scratch_left(f: &Fx) {
        assert!(env(f).journal().entries().is_empty());
        for dir in [f.old.join("Tools"), f.new.join("Tools"), f.mods.clone()] {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                assert!(!is_move_scratch(&n), "{n} left in {}", dir.display());
            }
        }
    }

    #[test]
    fn a_move_on_one_drive_renames_each_mod() {
        let f = fx("rename");
        add_zip(&f, "FS25_A", false);
        add_dir(&f, "FS25_D", false);
        let wa = witness(&f, &zip_at(&f.old, "FS25_A"), "a");
        let wd = witness(&f, &f.old.join("Tools/FS25_D/modDesc.xml"), "d");
        assert!(one_drive_run(&f, Op::Start).unwrap().is_none());
        // The very same files, with no room needed and nothing left to delete.
        assert!(same_file(&wa, &zip_at(&f.new, "FS25_A")));
        assert!(same_file(&wd, &f.new.join("Tools/FS25_D/modDesc.xml")));
        assert_eq!(
            std::fs::read(f.new.join("Tools/FS25_D/textures/a.dds")).unwrap(),
            b"FS25_D"
        );
        assert!(!f.old.exists(), "the emptied default archive is removed");
        assert_eq!(
            setting(&f).as_deref(),
            Some(f.new.to_string_lossy().as_ref())
        );
        assert!(load_marker(&f.conn, &f.key).is_none());
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_hardlinked_active_mod_stays_linked_through_a_rename() {
        let f = fx("renamelink");
        let flat = add_hardlinked(&f, "FS25_A");
        let before = flat.symlink_metadata().unwrap().modified().unwrap();
        assert!(one_drive_run(&f, Op::Start).unwrap().is_none());
        // The entry was never touched: still a hardlink of the (renamed) archived copy.
        assert!(same_file(&flat, &zip_at(&f.new, "FS25_A")));
        assert!(!flat.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(flat.symlink_metadata().unwrap().modified().unwrap(), before);
        assert_eq!(std::fs::read(&flat).unwrap(), b"PK\x03\x04 FS25_A");
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_linked_folder_mod_is_relinked_right_after_its_rename() {
        let f = fx("renamedir");
        add_dir(&f, "FS25_D", true);
        let (src, dst) = (f.old.join("Tools/FS25_D"), f.new.join("Tools/FS25_D"));
        let link = f.mods.join("FS25_D");
        // A symlink (a junction on Windows): it names the old place, so it must move.
        organize::link_dir_native(&src, &link).unwrap();
        let w = witness(&f, &src.join("modDesc.xml"), "d");
        assert!(one_drive_run(&f, Op::Start).unwrap().is_none());
        assert!(same_file(&w, &dst.join("modDesc.xml")));
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&dst).unwrap()
        );
        assert!(link.join("textures/a.dds").is_file());
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A crash right after the mod was recorded, before its rename: Resume renames it.
    #[test]
    fn a_crash_before_the_rename_is_finished_on_resume() {
        let f = fx("renamecrash1");
        add_zip(&f, "FS25_A", false);
        let w = witness(&f, &zip_at(&f.old, "FS25_A"), "a");
        let mut m = marker(&f);
        env(&f).record(&mut m, "FS25_A").unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(one_drive_run(&f, Op::Resume).unwrap().is_none());
        assert!(same_file(&w, &zip_at(&f.new, "FS25_A")));
        assert!(!zip_at(&f.old, "FS25_A").exists());
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// Recorded and renamed, then a crash before the folder link moved.
    fn crashed_after_rename(f: &Fx) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        add_dir(f, "FS25_D", true);
        let (src, dst) = (f.old.join("Tools/FS25_D"), f.new.join("Tools/FS25_D"));
        let link = f.mods.join("FS25_D");
        organize::link_dir_native(&src, &link).unwrap();
        let w = witness(f, &src.join("modDesc.xml"), "d");
        let mut m = marker(f);
        env(f).record(&mut m, "FS25_D").unwrap();
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        crate::paths::rename_no_replace(&src, &dst).unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        (src, dst, link, w)
    }

    #[test]
    fn a_crash_after_the_rename_relinks_on_resume() {
        let f = fx("renamecrash2");
        let (_, dst, link, w) = crashed_after_rename(&f);
        assert!(one_drive_run(&f, Op::Resume).unwrap().is_none());
        assert!(same_file(&w, &dst.join("modDesc.xml")));
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&dst).unwrap()
        );
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// The same crash, then Cancel instead: the mod is renamed home and its link follows.
    #[test]
    fn a_cancel_after_a_crash_mid_rename_renames_it_home() {
        let f = fx("renamecrash3");
        add_zip(&f, "FS25_E", false);
        let (src, dst, link, w) = crashed_after_rename(&f);
        assert!(one_drive_run(&f, Op::Cancel).unwrap().is_none());
        assert!(same_file(&w, &src.join("modDesc.xml")));
        assert!(!dst.exists());
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&src).unwrap()
        );
        assert!(
            zip_at(&f.old, "FS25_E").exists(),
            "never moved, never touched"
        );
        assert!(load_marker(&f.conn, &f.key).is_none() && setting(&f).is_none());
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_cancel_on_one_drive_renames_everything_home() {
        let f = fx("renamecancel");
        let flat = add_hardlinked(&f, "FS25_A");
        add_zip(&f, "FS25_B", false);
        let wa = witness(&f, &zip_at(&f.old, "FS25_A"), "a");
        let req = AtomicU8::new(0);
        let mut prog = |p: &Progress| {
            if !p.cancelling && p.current == "FS25_B" {
                req.store(CANCEL, Ordering::SeqCst);
            }
        };
        assert!(run_on_one_drive(&f, Op::Start, &req, &mut prog)
            .unwrap()
            .is_none());
        assert!(same_file(&wa, &zip_at(&f.old, "FS25_A")));
        assert!(same_file(&flat, &zip_at(&f.old, "FS25_A")));
        assert!(!zip_at(&f.new, "FS25_A").exists() && zip_at(&f.old, "FS25_B").exists());
        assert!(
            !f.new.join("Tools").exists(),
            "the category Silo made is removed"
        );
        assert!(load_marker(&f.conn, &f.key).is_none() && setting(&f).is_none());
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn another_drive_or_an_unknown_one_still_copies() {
        for (tag, relation) in [("renameno", Some(false)), ("renameunknown", None)] {
            let f = fx(tag);
            add_zip(&f, "FS25_A", false);
            let w = witness(&f, &zip_at(&f.old, "FS25_A"), "a");
            let _turn = RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let req = AtomicU8::new(0);
            let mut quiet = |_: &Progress| {};
            let rel = move |_: &Path, _: &Path| relation;
            let mut ctx = RunCtx {
                same_volume: &rel,
                ..ctx_for(&f, &|| false, &req, &mut quiet)
            };
            assert!(start(&mut ctx, f.old.clone(), f.new.clone())
                .unwrap()
                .is_none());
            let new_a = zip_at(&f.new, "FS25_A");
            assert_eq!(std::fs::read(&new_a).unwrap(), b"PK\x03\x04 FS25_A");
            assert!(!same_file(&w, &new_a), "{tag}: a copy, not the same file");
            assert!(!zip_at(&f.old, "FS25_A").exists());
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    fn rename_crosses_devices(_: &Path, _: &Path) -> std::io::Result<()> {
        Err(std::io::Error::from_raw_os_error(CROSS_DEVICE))
    }

    fn rename_is_denied(_: &Path, _: &Path) -> std::io::Result<()> {
        Err(std::io::ErrorKind::PermissionDenied.into())
    }

    fn rename_finds_it_taken(_: &Path, _: &Path) -> std::io::Result<()> {
        Err(std::io::ErrorKind::AlreadyExists.into())
    }

    #[test]
    fn a_rename_across_drives_after_all_falls_back_to_the_copy() {
        let f = fx("renamefail");
        let r = add_zip(&f, "FS25_A", false);
        let w = witness(&f, &zip_at(&f.old, "FS25_A"), "a");
        let layout = dest(&f);
        let mut m = marker(&f);
        let e = Env {
            same_volume: true,
            rename: rename_crosses_devices,
            ..env(&f)
        };
        assert_eq!(
            forward_one(&e, &r, &mut m, &mut LinkPolicy::new(&layout), &|| false),
            Ok(Step::Moved)
        );
        let new_a = zip_at(&f.new, "FS25_A");
        assert_eq!(std::fs::read(&new_a).unwrap(), b"PK\x03\x04 FS25_A");
        assert!(!same_file(&w, &new_a), "copied");
        assert!(!zip_at(&f.old, "FS25_A").exists());
        assert_eq!(m.moved, vec!["FS25_A".to_string()]);
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_rename_that_finds_its_name_taken_is_refused_not_copied() {
        let f = fx("renametaken");
        let r = add_zip(&f, "FS25_A", false);
        let layout = dest(&f);
        let mut m = marker(&f);
        let e = Env {
            same_volume: true,
            rename: rename_finds_it_taken,
            ..env(&f)
        };
        let out = forward_one(&e, &r, &mut m, &mut LinkPolicy::new(&layout), &|| false);
        assert!(
            matches!(&out, Err(StepErr::Failed(s)) if s.contains("won't replace it")),
            "{out:?}"
        );
        assert!(zip_at(&f.old, "FS25_A").exists() && !zip_at(&f.new, "FS25_A").exists());
        assert!(m.moved.is_empty());
        assert!(load_marker(&f.conn, &f.key).unwrap().moved.is_empty());
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    #[test]
    fn a_rename_refused_for_another_reason_stops_without_copying() {
        let f = fx("renamedenied");
        let r = add_zip(&f, "FS25_A", false);
        let w = witness(&f, &zip_at(&f.old, "FS25_A"), "a");
        let layout = dest(&f);
        let mut m = marker(&f);
        let e = Env {
            same_volume: true,
            rename: rename_is_denied,
            ..env(&f)
        };
        let out = forward_one(&e, &r, &mut m, &mut LinkPolicy::new(&layout), &|| false);
        assert!(
            matches!(&out, Err(StepErr::Failed(s)) if s.contains("couldn't move")),
            "{out:?}"
        );
        assert!(same_file(&w, &zip_at(&f.old, "FS25_A")));
        assert!(!zip_at(&f.new, "FS25_A").exists(), "no copy was made");
        assert!(m.moved.is_empty());
        no_scratch_left(&f);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// The re-point fails after the rename, having already removed the folder link (Windows'
    /// `replace_dir_link` can): the mod is renamed back and its link made again, so the pause
    /// leaves it playable and un-recorded.
    #[test]
    fn a_relink_that_fails_after_a_rename_puts_the_mod_and_its_link_back() {
        let f = fx("renameback");
        let r = add_dir(&f, "FS25_D", true);
        let (src, dst) = (f.old.join("Tools/FS25_D"), f.new.join("Tools/FS25_D"));
        let link = f.mods.join("FS25_D");
        organize::link_dir_native(&src, &link).unwrap();
        let w = witness(&f, &src.join("modDesc.xml"), "d");
        let layout = dest(&f);
        let mut m = marker(&f);
        REPOINT_FAULT.with(|c| {
            c.set(Some(|link: &Path| {
                organize::remove_link(link)
                    .or_else(|_| std::fs::remove_dir(link))
                    .unwrap();
            }))
        });
        let e = Env {
            same_volume: true,
            ..env(&f)
        };
        let out = forward_one(&e, &r, &mut m, &mut LinkPolicy::new(&layout), &|| false);
        assert_eq!(out, Err(StepErr::Failed("relink: injected failure".into())));
        assert!(same_file(&w, &src.join("modDesc.xml")), "renamed back");
        assert!(!dst.exists());
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&src).unwrap()
        );
        assert!(m.moved.is_empty());
        assert!(load_marker(&f.conn, &f.key).unwrap().moved.is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// Renamed back, but its link can't be made again (here: the mods folder is gone): it stays
    /// recorded, so a Cancel still mends it.
    #[test]
    fn a_mod_put_back_without_its_link_stays_recorded_for_cancel() {
        let f = fx("renamebacklost");
        let r = add_zip(&f, "FS25_A", true);
        let w = witness(&f, &zip_at(&f.old, "FS25_A"), "a");
        let layout = dest(&f);
        let mut m = marker(&f);
        let gone = f.base.join("no-such-mods-folder");
        let e = Env {
            mods: &gone,
            same_volume: true,
            ..env(&f)
        };
        let out = forward_one(&e, &r, &mut m, &mut LinkPolicy::new(&layout), &|| false);
        assert!(
            matches!(&out, Err(StepErr::Failed(s)) if s.starts_with("relink:")),
            "{out:?}"
        );
        assert!(same_file(&w, &zip_at(&f.old, "FS25_A")), "renamed back");
        assert!(!zip_at(&f.new, "FS25_A").exists());
        assert_eq!(m.moved, vec!["FS25_A".to_string()]);
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(one_drive_run(&f, Op::Cancel).unwrap().is_none());
        assert!(same_file(
            &f.mods.join("FS25_A.zip"),
            &zip_at(&f.old, "FS25_A")
        ));
        assert!(load_marker(&f.conn, &f.key).is_none());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    fn flush_fails(_: &Path, _: &Path, _: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("disk gone"))
    }

    /// A rename that can't be written to disk isn't counted: it stays recorded (forward) and
    /// isn't un-recorded (back) until a retry flushes it.
    #[test]
    fn a_rename_that_cant_be_flushed_stays_recorded_until_a_retry_flushes_it() {
        let f = fx("renameflush");
        let r = add_zip(&f, "FS25_A", false);
        let layout = dest(&f);
        let mut pol = LinkPolicy::new(&layout);
        let mut m = marker(&f);
        let bad = Env {
            same_volume: true,
            flush: flush_fails,
            ..env(&f)
        };
        let good = Env {
            same_volume: true,
            ..env(&f)
        };
        let out = forward_one(&bad, &r, &mut m, &mut pol, &|| false);
        assert!(
            matches!(&out, Err(StepErr::Failed(s)) if s.contains("to disk")),
            "{out:?}"
        );
        assert!(zip_at(&f.new, "FS25_A").exists() && !zip_at(&f.old, "FS25_A").exists());
        assert_eq!(m.moved, vec!["FS25_A".to_string()]);
        assert!(forward_one(&bad, &r, &mut m, &mut pol, &|| false).is_err());
        assert_eq!(
            forward_one(&good, &r, &mut m, &mut pol, &|| false),
            Ok(Step::AlreadyDone)
        );
        // Back home: renamed, but not un-recorded until it's flushed.
        let home = Layout {
            mods: f.mods.clone(),
            archive: f.old.clone(),
        };
        let mut pol = LinkPolicy::new(&home);
        assert!(back_one(&bad, &r, &mut m, &mut pol, &|| false).is_err());
        assert!(zip_at(&f.old, "FS25_A").exists() && !zip_at(&f.new, "FS25_A").exists());
        assert_eq!(m.moved, vec!["FS25_A".to_string()]);
        assert_eq!(
            back_one(&good, &r, &mut m, &mut pol, &|| false),
            Ok(Step::AlreadyDone)
        );
        assert!(m.moved.is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// Resume after a crash between a rename and its re-link, paused at once (before reaching
    /// that mod): the link is mended before the first boundary, so the pause leaves it working.
    #[test]
    fn a_link_left_by_a_crash_mid_rename_is_mended_before_the_first_pause() {
        let f = fx("renamemend");
        add_zip(&f, "FS25_A", false); // sorted first, not moved yet
        let (_, dst, link, _) = crashed_after_rename(&f);
        let req = AtomicU8::new(PAUSE);
        let paused = run_on_one_drive(&f, Op::Resume, &req, &mut |_| {})
            .unwrap()
            .unwrap();
        assert!(paused.paused && paused.errors.is_empty());
        assert!(zip_at(&f.old, "FS25_A").exists(), "A not reached");
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&dst).unwrap()
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A folder mod with a link inside is refused on one drive too, as the copy refuses it:
    /// renaming it could break what the link reaches.
    #[test]
    fn a_link_inside_a_folder_mod_is_refused_on_one_drive_too() {
        let f = fx("renameinner");
        add_dir(&f, "FS25_D", false);
        let src = f.old.join("Tools/FS25_D");
        let elsewhere = f.base.join("shared");
        std::fs::create_dir_all(&elsewhere).unwrap();
        organize::link_dir_native(&elsewhere, &src.join("escape")).unwrap();
        let out = one_drive_run(&f, Op::Start).unwrap().unwrap();
        assert!(
            out.paused && out.errors[0].contains("is a link inside a mod folder"),
            "{:?}",
            out.errors
        );
        assert!(src.join("modDesc.xml").exists());
        assert!(!f.new.join("Tools/FS25_D").exists());
        assert!(out.moved.is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// An active zip's symlink spelled another way than the archive path (through `..`, or
    /// through another link to the archive folder) is still re-pointed once its copy was
    /// renamed away. File symlinks need Developer Mode on Windows; skipped where they can't
    /// be made.
    #[test]
    fn a_symlink_spelled_another_way_is_still_relinked_after_a_rename() {
        for (tag, through_alias) in [("renamerel", false), ("renamealias", true)] {
            let f = fx(tag);
            add_zip(&f, "FS25_A", true);
            let link = f.mods.join("FS25_A.zip");
            let target = if through_alias {
                let alias = f.base.join("alias");
                if organize::link_dir_native(&f.old, &alias).is_err() {
                    return;
                }
                alias.join("Tools/FS25_A.zip")
            } else {
                PathBuf::from("archive/../archive/Tools/FS25_A.zip")
            };
            if organize::symlink_file(&target, &link).is_err() {
                let _ = std::fs::remove_dir_all(&f.base);
                return;
            }
            let w = witness(&f, &zip_at(&f.old, "FS25_A"), "a");
            assert!(one_drive_run(&f, Op::Start).unwrap().is_none(), "{tag}");
            assert!(same_file(&w, &zip_at(&f.new, "FS25_A")), "{tag}");
            assert!(
                same_file(&link, &zip_at(&f.new, "FS25_A")),
                "{tag}: the link reaches the renamed copy"
            );
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// A crash mid-`replace_dir_link` after the rename left the folder link missing: Resume
    /// and Cancel, paused at once, both make it again first.
    #[test]
    fn a_link_lost_by_a_crash_mid_rename_is_made_again_before_the_first_pause() {
        for (tag, op) in [("renamelost1", Op::Resume), ("renamelost2", Op::Cancel)] {
            let f = fx(tag);
            add_zip(&f, "FS25_A", false);
            let (_, dst, link, _) = crashed_after_rename(&f);
            organize::remove_link(&link)
                .or_else(|_| std::fs::remove_dir(&link))
                .unwrap();
            let req = AtomicU8::new(PAUSE);
            let paused = run_on_one_drive(&f, op, &req, &mut |_| {})
                .unwrap()
                .unwrap();
            assert!(paused.paused && paused.errors.is_empty(), "{tag}");
            assert_eq!(
                std::fs::canonicalize(&link).unwrap(),
                std::fs::canonicalize(&dst).unwrap(),
                "{tag}"
            );
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// A link that can't be mended before the first boundary isn't skipped: the run pauses at
    /// once, listing it.
    #[test]
    fn a_link_that_cant_be_mended_pauses_the_move_saying_so() {
        let f = fx("renamemendfail");
        let (_, _, _, _) = crashed_after_rename(&f);
        REPOINT_FAULT.with(|c| c.set(Some(|_: &Path| {})));
        let paused = one_drive_run(&f, Op::Resume).unwrap().unwrap();
        assert!(paused.paused);
        assert_eq!(
            paused.errors,
            vec!["FS25_D: relink: injected failure".to_string()]
        );
        // Nothing else was done: the mod stays recorded where it is.
        assert_eq!(paused.moved, vec!["FS25_D".to_string()]);
        assert!(f.new.join("Tools/FS25_D/modDesc.xml").is_file());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A folder mod linked through another link to the mod itself (not to its folder): its
    /// link is followed to the end, so it's re-pointed after the rename, and mended after a
    /// crash. Folder symlinks need Developer Mode on Windows (a junction would resolve the
    /// alias when made); skipped where they can't be made.
    #[test]
    fn a_link_through_an_alias_of_the_mod_is_relinked_after_a_rename() {
        for (tag, crash) in [("renamealiasd1", false), ("renamealiasd2", true)] {
            let f = fx(tag);
            add_dir(&f, "FS25_D", true);
            let (src, dst) = (f.old.join("Tools/FS25_D"), f.new.join("Tools/FS25_D"));
            let alias = f.base.join("alias_D");
            let link = f.mods.join("FS25_D");
            let made = symlink_dir(&src, &alias).and_then(|()| symlink_dir(&alias, &link));
            if made.is_err() {
                let _ = std::fs::remove_dir_all(&f.base);
                return;
            }
            if crash {
                let mut m = marker(&f);
                env(&f).record(&mut m, "FS25_D").unwrap();
                std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
                crate::paths::rename_no_replace(&src, &dst).unwrap();
                m.paused = true;
                save_marker(&f.conn, &f.key, &m).unwrap();
                let req = AtomicU8::new(PAUSE);
                let paused = run_on_one_drive(&f, Op::Resume, &req, &mut |_| {})
                    .unwrap()
                    .unwrap();
                assert!(paused.errors.is_empty(), "{tag}: {:?}", paused.errors);
            } else {
                assert!(one_drive_run(&f, Op::Start).unwrap().is_none(), "{tag}");
            }
            assert_eq!(
                std::fs::canonicalize(&link).unwrap(),
                std::fs::canonicalize(&dst).unwrap(),
                "{tag}"
            );
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// `n` links in a row ending at `end` (`<dir>/<name>-1` is the first); `None` where file
    /// symlinks can't be made (Windows without Developer Mode).
    fn link_chain(dir: &Path, name: &str, n: usize, end: &Path) -> Option<PathBuf> {
        let hop = |i: usize| dir.join(format!("{name}-{i}"));
        organize::symlink_file(end, &hop(n)).ok()?;
        for i in (1..n).rev() {
            organize::symlink_file(&hop(i + 1), &hop(i)).unwrap();
        }
        Some(hop(1))
    }

    #[test]
    fn a_link_chain_is_followed_to_its_end_and_a_loop_gives_up() {
        let f = fx("chain");
        let end = f.base.join("gone.zip");
        let (one, two) = (f.base.join("one"), f.base.join("two"));
        if organize::symlink_file(&end, &two).is_err() {
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        }
        organize::symlink_file(Path::new("two"), &one).unwrap();
        assert_eq!(chain_end(&one), Some(end.clone()));
        for n in [31, 32, 33, MAX_CHAIN] {
            let first = link_chain(&f.base, &format!("c{n}"), n, &end).unwrap();
            assert_eq!(chain_end(&first), Some(end.clone()), "{n} links");
        }
        let first = link_chain(&f.base, "long", MAX_CHAIN + 1, &end).unwrap();
        assert_eq!(chain_end(&first), None);
        let (x, y) = (f.base.join("x"), f.base.join("y"));
        organize::symlink_file(&y, &x).unwrap();
        organize::symlink_file(&x, &y).unwrap();
        assert_eq!(chain_end(&x), None);
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// An active zip linked through a chain of 33 links (more than the 32 an earlier version
    /// followed): re-pointed after the rename, and mended after a crash mid-rename.
    #[test]
    fn a_long_link_chain_is_relinked_after_a_rename_and_after_a_crash() {
        for (tag, crash) in [("renamechain1", false), ("renamechain2", true)] {
            let f = fx(tag);
            add_zip(&f, "FS25_A", true);
            let (src, dst) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
            let link = f.mods.join("FS25_A.zip");
            let Some(first) = link_chain(&f.base, "hop", 32, &src) else {
                let _ = std::fs::remove_dir_all(&f.base);
                return;
            };
            organize::symlink_file(&first, &link).unwrap();
            if crash {
                let mut m = marker(&f);
                env(&f).record(&mut m, "FS25_A").unwrap();
                std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
                crate::paths::rename_no_replace(&src, &dst).unwrap();
                m.paused = true;
                save_marker(&f.conn, &f.key, &m).unwrap();
                let req = AtomicU8::new(PAUSE);
                let paused = run_on_one_drive(&f, Op::Resume, &req, &mut |_| {})
                    .unwrap()
                    .unwrap();
                assert!(paused.errors.is_empty(), "{tag}: {:?}", paused.errors);
            } else {
                assert!(one_drive_run(&f, Op::Start).unwrap().is_none(), "{tag}");
            }
            assert!(same_file(&link, &dst), "{tag}");
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// A link that reached the mod before its rename but that Silo can't recognize afterwards
    /// (its target goes through the mod's own name, then `..`; Windows resolves `..` by name,
    /// so this is unix-only): re-pointed on the note saved before the rename, right away and
    /// after a crash in between (Resume or Cancel paused at once, Resume run through).
    #[cfg(unix)]
    #[test]
    fn a_link_silo_cant_recognize_after_the_rename_is_relinked_from_its_note() {
        let cases = [
            ("renamenote0", None),
            ("renamenote1", Some((Op::Resume, PAUSE))),
            ("renamenote2", Some((Op::Cancel, PAUSE))),
            ("renamenote3", Some((Op::Resume, 0))),
        ];
        for (tag, crash) in cases {
            let f = fx(tag);
            add_dir(&f, "FS25_D", true);
            let (src, dst) = (f.old.join("Tools/FS25_D"), f.new.join("Tools/FS25_D"));
            let link = f.mods.join("FS25_D");
            let target = src.join("..").join("FS25_D");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            match crash {
                None => assert!(one_drive_run(&f, Op::Start).unwrap().is_none(), "{tag}"),
                Some((op, request)) => {
                    // Noted, recorded and renamed, then the crash before the re-point.
                    let mut m = marker(&f);
                    env(&f).record(&mut m, "FS25_D").unwrap();
                    m.relinking.push(LinkNote {
                        tech_name: "FS25_D".into(),
                        target: target.clone(),
                    });
                    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
                    crate::paths::rename_no_replace(&src, &dst).unwrap();
                    m.paused = true;
                    save_marker(&f.conn, &f.key, &m).unwrap();
                    let req = AtomicU8::new(request);
                    let out = run_on_one_drive(&f, op, &req, &mut |_| {}).unwrap();
                    if let Some(m) = out {
                        assert!(m.errors.is_empty(), "{tag}: {:?}", m.errors);
                        assert!(m.relinking.is_empty(), "{tag}: the note goes");
                    }
                }
            }
            assert_eq!(
                std::fs::canonicalize(&link).unwrap(),
                std::fs::canonicalize(&dst).unwrap(),
                "{tag}"
            );
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    thread_local! {
        /// The DB and key `rename_after_looking` reads the saved marker from.
        static PROBE: std::cell::RefCell<Option<(PathBuf, String)>> =
            const { std::cell::RefCell::new(None) };
        /// How many link notes the saved marker held when the rename ran.
        static NOTES_AT_RENAME: Cell<Option<usize>> = const { Cell::new(None) };
    }

    fn rename_after_looking(a: &Path, b: &Path) -> std::io::Result<()> {
        PROBE.with(|p| {
            if let Some((db, key)) = &*p.borrow() {
                let conn = db::open(db).unwrap();
                let n = load_marker(&conn, key).map(|m| m.relinking.len());
                NOTES_AT_RENAME.with(|c| c.set(n));
            }
        });
        crate::paths::rename_no_replace(a, b)
    }

    /// A crash mid-rename whose link Silo can't recognize afterwards: it went through an alias
    /// of the mod that's gone since, so only its note can re-point it. Runs everywhere where
    /// folder symlinks can be made (Developer Mode on Windows); `None` elsewhere.
    fn crashed_with_unrecognizable_link(f: &Fx) -> Option<(PathBuf, PathBuf, PathBuf)> {
        add_dir(f, "FS25_D", true);
        let (src, dst) = (f.old.join("Tools/FS25_D"), f.new.join("Tools/FS25_D"));
        let (alias, link) = (f.base.join("alias_D"), f.mods.join("FS25_D"));
        symlink_dir(&src, &alias)
            .and_then(|()| symlink_dir(&alias, &link))
            .ok()?;
        let mut m = marker(f);
        env(f).record(&mut m, "FS25_D").unwrap();
        m.relinking.push(LinkNote {
            tech_name: "FS25_D".into(),
            target: alias.clone(),
        });
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        crate::paths::rename_no_replace(&src, &dst).unwrap();
        std::fs::remove_file(&alias)
            .or_else(|_| std::fs::remove_dir(&alias))
            .unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        Some((src, dst, link))
    }

    fn resume_paused_at_once(f: &Fx) -> MoveMarker {
        let req = AtomicU8::new(PAUSE);
        run_on_one_drive(f, Op::Resume, &req, &mut |_| {})
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_noted_link_is_relinked_after_a_crash_even_unrecognized() {
        let f = fx("renamenotealias");
        let Some((_, dst, link)) = crashed_with_unrecognizable_link(&f) else {
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        };
        let paused = resume_paused_at_once(&f);
        assert!(paused.errors.is_empty() && paused.relinking.is_empty());
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&dst).unwrap()
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// The noted link can't be read for a moment: the note is kept and the failure listed, and
    /// the next run re-points the link from it.
    #[test]
    fn a_note_survives_an_entry_it_cant_read_and_mends_it_on_retry() {
        let f = fx("renamenoteread");
        let Some((_, dst, link)) = crashed_with_unrecognizable_link(&f) else {
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        };
        INSPECT_FAULT.with(|c| c.set(true));
        let paused = resume_paused_at_once(&f);
        assert_eq!(
            paused.errors,
            vec!["FS25_D: relink: injected read failure".to_string()]
        );
        assert_eq!(paused.relinking.len(), 1, "the note is kept");
        assert_eq!(load_marker(&f.conn, &f.key).unwrap().relinking.len(), 1);
        let paused = resume_paused_at_once(&f);
        assert!(paused.errors.is_empty() && paused.relinking.is_empty());
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&dst).unwrap()
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A noted link that still reaches nothing, where Silo can't tell which copy it should
    /// reach (one in each archive, none in either, or only something that isn't the mod's
    /// copy): the run pauses at once saying so, keeping the note, and changes nothing.
    #[test]
    fn a_noted_link_with_no_clear_copy_pauses_the_move_saying_so() {
        for (tag, case) in [
            ("renamenoteboth", "both"),
            ("renamenoteneither", "neither"),
            ("renamenotenotcopy", "not a copy"),
        ] {
            let f = fx(tag);
            let Some((src, dst, link)) = crashed_with_unrecognizable_link(&f) else {
                let _ = std::fs::remove_dir_all(&f.base);
                return;
            };
            match case {
                "both" => {
                    std::fs::create_dir_all(&src).unwrap();
                    std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
                }
                "neither" => std::fs::remove_dir_all(&dst).unwrap(),
                _ => {
                    std::fs::remove_dir_all(&dst).unwrap();
                    std::fs::write(&dst, b"not a folder").unwrap();
                }
            }
            let target = std::fs::read_link(&link).unwrap();
            let paused = resume_paused_at_once(&f);
            assert_eq!(paused.errors.len(), 1, "{tag}: {:?}", paused.errors);
            assert!(
                paused.errors[0].starts_with("FS25_D: relink: its link at"),
                "{tag}: {:?}",
                paused.errors
            );
            assert_eq!(paused.relinking.len(), 1, "{tag}: the note is kept");
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                target,
                "{tag}: untouched"
            );
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// The note is saved before the rename and gone once the link is re-pointed.
    #[test]
    fn a_link_is_noted_before_the_rename_and_forgotten_after_it() {
        let f = fx("renamenoted");
        let r = add_dir(&f, "FS25_D", true);
        let src = f.old.join("Tools/FS25_D");
        organize::link_dir_native(&src, &f.mods.join("FS25_D")).unwrap();
        let layout = dest(&f);
        let mut m = marker(&f);
        let e = Env {
            same_volume: true,
            rename: rename_after_looking,
            ..env(&f)
        };
        PROBE.with(|p| p.replace(Some((f.base.join("silo.db"), f.key.clone()))));
        let out = forward_one(&e, &r, &mut m, &mut LinkPolicy::new(&layout), &|| false);
        PROBE.with(|p| p.replace(None));
        assert_eq!(out, Ok(Step::Moved));
        assert_eq!(
            NOTES_AT_RENAME.with(|c| c.get()),
            Some(1),
            "saved before the rename"
        );
        assert!(m.relinking.is_empty());
        assert!(load_marker(&f.conn, &f.key).unwrap().relinking.is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A noted link that was replaced by something else since (here: a link to another
    /// folder, now gone) isn't Silo's to change: the note is dropped, the entry left as it is.
    #[test]
    fn a_noted_link_replaced_since_is_left_alone() {
        let f = fx("renamenoteforeign");
        let (src, _, link, _) = crashed_after_rename(&f);
        let mut m = load_marker(&f.conn, &f.key).unwrap();
        m.relinking.push(LinkNote {
            tech_name: "FS25_D".into(),
            target: src.clone(),
        });
        save_marker(&f.conn, &f.key, &m).unwrap();
        organize::remove_link(&link)
            .or_else(|_| std::fs::remove_dir(&link))
            .unwrap();
        let elsewhere = f.base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        organize::link_dir_native(&elsewhere, &link).unwrap();
        std::fs::remove_dir(&elsewhere).unwrap();
        let theirs = std::fs::read_link(&link).unwrap();
        let req = AtomicU8::new(PAUSE);
        let paused = run_on_one_drive(&f, Op::Resume, &req, &mut |_| {})
            .unwrap()
            .unwrap();
        assert!(paused.relinking.is_empty(), "the note is dropped");
        assert_eq!(std::fs::read_link(&link).unwrap(), theirs, "left as it is");
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A recorded relink temp an earlier run finished is swept before the mend, even when the
    /// mend then pauses the move: its space is free for the retry.
    #[test]
    fn a_finished_relink_temp_is_swept_before_the_mend() {
        let f = fx("renamesweep");
        let (_, dst, link, _) = crashed_after_rename(&f);
        organize::remove_link(&link)
            .or_else(|_| std::fs::remove_dir(&link))
            .unwrap();
        let tmp = scratch::begin(
            &env(&f).journal(),
            scratch::Role::RelinkTmp,
            &link,
            RELINK_SUFFIX,
            &dst,
            &link,
            "dir",
        )
        .unwrap();
        organize::make_link(&dst, &tmp, "dir", &mut LinkPolicy::new(&dest(&f))).unwrap();
        REPOINT_FAULT.with(|c| c.set(Some(|_: &Path| {})));
        let paused = one_drive_run(&f, Op::Resume).unwrap().unwrap();
        assert_eq!(
            paused.errors,
            vec!["FS25_D: relink: injected failure".to_string()]
        );
        assert!(tmp.symlink_metadata().is_err(), "the temp is swept");
        assert!(env(&f).journal().entries().is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A real folder symlink (never a junction, which resolves its target when made).
    fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link)
        }
    }
    /// Drive `op` with `relation` as the drives' answer and `request` set before the run.
    fn run_on(
        f: &Fx,
        op: Op,
        relation: Relation,
        request: u8,
    ) -> Result<Option<MoveMarker>, String> {
        let _turn = RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let req = AtomicU8::new(request);
        let mut quiet = |_: &Progress| {};
        let mut ctx = RunCtx {
            same_volume: &relation,
            free_space: &|_| Some(0),
            ..ctx_for(f, &|| false, &req, &mut quiet)
        };
        drive(&mut ctx, f, op)
    }

    /// What `RunCtx::same_volume` answers.
    type Relation = fn(&Path, &Path) -> Option<bool>;

    fn other_drive(_: &Path, _: &Path) -> Option<bool> {
        Some(false)
    }

    fn unknown_drive(_: &Path, _: &Path) -> Option<bool> {
        None
    }

    /// A crash mid-rename is mended from its note even when the drives can't be told apart
    /// on Resume (or now look like two): paused at once, and run through to the end.
    #[test]
    fn a_crash_mid_rename_is_mended_whatever_the_drives_look_like_on_resume() {
        let cases: [(&str, Relation, u8); 4] = [
            ("renamevol1", unknown_drive, PAUSE),
            ("renamevol2", unknown_drive, 0),
            ("renamevol3", other_drive, PAUSE),
            ("renamevol4", other_drive, 0),
        ];
        for (tag, relation, request) in cases {
            let f = fx(tag);
            let Some((_, dst, link)) = crashed_with_unrecognizable_link(&f) else {
                let _ = std::fs::remove_dir_all(&f.base);
                return;
            };
            let out = run_on(&f, Op::Resume, relation, request).unwrap();
            match &out {
                Some(m) => assert!(m.errors.is_empty() && m.relinking.is_empty(), "{tag}"),
                None => assert!(load_marker(&f.conn, &f.key).is_none(), "{tag}"),
            }
            assert_eq!(out.is_some(), request == PAUSE, "{tag}");
            assert_eq!(
                std::fs::canonicalize(&link).unwrap(),
                std::fs::canonicalize(&dst).unwrap(),
                "{tag}"
            );
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// A folder link lost by a crash mid-rename, where the two archives don't say plainly where
    /// the mod is: with the same copy in each it's made again as the step would make it; with
    /// none, or only something that isn't the mod's copy, the move pauses at once saying so.
    #[test]
    fn a_missing_link_is_made_again_or_reported_before_the_first_pause() {
        for (tag, case) in [
            ("renamemissboth", "both"),
            ("renamemissneither", "neither"),
            ("renamemissnotcopy", "not a copy"),
        ] {
            let f = fx(tag);
            let (src, dst, link, _) = crashed_after_rename(&f);
            organize::remove_link(&link)
                .or_else(|_| std::fs::remove_dir(&link))
                .unwrap();
            match case {
                // An identical copy back at the old place (the same files): checked, then
                // linked as the step would, to the copy the move goes to.
                "both" => copy_verify_place(&env(&f), &dst, &src, "dir", &|| false).unwrap(),
                "neither" => std::fs::remove_dir_all(&dst).unwrap(),
                _ => {
                    std::fs::remove_dir_all(&dst).unwrap();
                    std::fs::write(&dst, b"not a folder").unwrap();
                }
            }
            let paused = resume_paused_at_once(&f);
            if case == "both" {
                assert!(paused.errors.is_empty(), "{tag}: {:?}", paused.errors);
                assert_eq!(
                    std::fs::canonicalize(&link).unwrap(),
                    std::fs::canonicalize(&dst).unwrap(),
                    "{tag}"
                );
            } else {
                assert_eq!(paused.errors.len(), 1, "{tag}: {:?}", paused.errors);
                assert!(
                    paused.errors[0].contains("is missing, and Silo can't make it again"),
                    "{tag}: {:?}",
                    paused.errors
                );
                assert!(link.symlink_metadata().is_err(), "{tag}");
            }
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// Cancel right after a mod was recorded, before its rename made its category folder in the
    /// new archive: the flush finds that folder missing and flushes its parent instead, so the
    /// cancel finishes (a missing folder made it fail on unix).
    #[test]
    fn a_cancel_before_the_first_rename_finishes() {
        let f = fx("renamecancelearly");
        add_zip(&f, "FS25_A", false);
        let mut m = marker(&f);
        env(&f).record(&mut m, "FS25_A").unwrap();
        m.paused = true;
        save_marker(&f.conn, &f.key, &m).unwrap();
        assert!(!f.new.join("Tools").exists());
        assert!(one_drive_run(&f, Op::Cancel).unwrap().is_none());
        assert!(zip_at(&f.old, "FS25_A").exists());
        assert!(load_marker(&f.conn, &f.key).is_none());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// The flush of a rename whose old folder is gone flushes that folder's parent.
    #[test]
    fn a_rename_flush_skips_a_folder_that_isnt_there() {
        let f = fx("renameflushgone");
        let placed = zip_at(&f.old, "FS25_A");
        std::fs::write(&placed, b"PK").unwrap();
        flush_rename(&placed, &f.new.join("Tools").join("FS25_A.zip"), &f.mods).unwrap();
        // A folder it must flush and can't open is an error, on Windows too.
        let nowhere = f.base.join("nowhere").join("Tools").join("FS25_A.zip");
        assert!(flush_rename(&nowhere, &placed, &f.mods).is_err());
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// A copy-path crash mid-relink: both archives hold the mod (placed, not yet deleted) and
    /// its active entry is missing. With the copies the same, the entry is made again as the
    /// step would make it: for the copy the move goes to, as a hardlink of that very copy
    /// (forward: the new one; cancel: home). With them different, neither is linked and the
    /// move pauses at once saying so, with or without a pause already asked for.
    #[test]
    fn a_missing_entry_beside_two_copies_is_made_as_the_step_would_or_reported() {
        let cases = [
            ("bothfwd", Op::Resume, true, PAUSE),
            ("bothback", Op::Cancel, true, PAUSE),
            ("bothbackrun", Op::Cancel, true, 0),
            ("bothdiff", Op::Cancel, false, PAUSE),
            ("bothdiffrun", Op::Cancel, false, 0),
        ];
        for (tag, op, same, request) in cases {
            let f = fx(tag);
            add_zip(&f, "FS25_A", true);
            let (old, new) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
            let flat = f.mods.join("FS25_A.zip");
            copy_verify_place(&env(&f), &old, &new, "zip", &|| false).unwrap();
            if !same {
                std::fs::write(&new, b"PK\x03\x04 changed since").unwrap();
            }
            let mut m = marker(&f);
            env(&f).record(&mut m, "FS25_A").unwrap();
            m.paused = true;
            save_marker(&f.conn, &f.key, &m).unwrap();
            let back = matches!(op, Op::Cancel);
            let out = run_on(&f, op, one_drive, request).unwrap();
            if same {
                let want = if back { &old } else { &new };
                assert!(same_file(&flat, want), "{tag}: linked as the step would");
                match out {
                    Some(m) => assert!(m.errors.is_empty(), "{tag}: {:?}", m.errors),
                    None => assert!(!new.exists(), "{tag}: the cancel finished"),
                }
            } else {
                let m = out.expect("paused");
                assert_eq!(m.errors.len(), 1, "{tag}: {:?}", m.errors);
                assert!(m.errors[0].contains("differ"), "{tag}: {:?}", m.errors);
                assert!(
                    flat.symlink_metadata().is_err(),
                    "{tag}: neither copy is linked"
                );
                assert!(old.exists() && new.exists(), "{tag}: both kept");
            }
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// An active zip's only copy, set aside by a crash mid-delete after its new copy was lost,
    /// with its mods-folder entry missing too: the sweep puts the copy back before the mend
    /// looks, so the mend links it (paused at once) and Resume moves it again (run through).
    #[test]
    fn the_only_copy_set_aside_is_put_back_before_the_mend_links_it() {
        for (tag, request) in [("onlycopylink1", PAUSE), ("onlycopylink2", 0)] {
            let f = fx(tag);
            let a = add_zip(&f, "FS25_A", true);
            let flat = f.mods.join("FS25_A.zip");
            let mut m = marker(&f);
            let (src, dst) = (zip_at(&f.old, "FS25_A"), zip_at(&f.new, "FS25_A"));
            copy_verify_place(&env(&f), &src, &dst, "zip", &|| false).unwrap();
            env(&f).record(&mut m, &a.tech_name).unwrap();
            let old_name = recorded_old(&f, &src, &dst, "zip");
            std::fs::rename(&src, &old_name).unwrap();
            std::fs::remove_file(&dst).unwrap(); // the new copy is lost
            m.paused = true;
            save_marker(&f.conn, &f.key, &m).unwrap();
            let out = run_on(&f, Op::Resume, one_drive, request).unwrap();
            match out {
                Some(m) => {
                    assert!(m.errors.is_empty(), "{tag}: {:?}", m.errors);
                    assert!(same_file(&flat, &src), "{tag}: linked to the copy put back");
                }
                None => assert!(same_file(&flat, &dst), "{tag}: moved, and linked"),
            }
            assert!(!old_name.exists(), "{tag}");
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// A note goes only once what it leaves behind is on disk. The link is repaired but the
    /// flush fails: the note stays, the failure is listed. Resumed (paused at once) with the
    /// flush still failing, the repaired link no longer matches the note, yet the note still
    /// waits for the flush, which is retried and listed again. Once the flush works it goes.
    /// The same for a note whose link went missing (the generic pass makes it again).
    #[test]
    fn a_note_goes_only_after_a_flush_of_what_it_leaves() {
        for (tag, lose_link) in [("notedurable1", false), ("notedurable2", true)] {
            let f = fx(tag);
            let Some((_, dst, link)) = crashed_with_unrecognizable_link(&f) else {
                let _ = std::fs::remove_dir_all(&f.base);
                return;
            };
            if lose_link {
                std::fs::remove_file(&link)
                    .or_else(|_| std::fs::remove_dir(&link))
                    .unwrap();
            }
            FLUSH_FAULT.with(|c| c.set(true));
            for attempt in 0..2 {
                let paused = resume_paused_at_once(&f);
                assert_eq!(
                    paused.errors.len(),
                    1,
                    "{tag} {attempt}: {:?}",
                    paused.errors
                );
                assert!(
                    paused.errors[0].contains("injected flush failure"),
                    "{tag} {attempt}: {:?}",
                    paused.errors
                );
                assert_eq!(
                    load_marker(&f.conn, &f.key).unwrap().relinking.len(),
                    1,
                    "{tag} {attempt}: the note is kept"
                );
                assert_eq!(
                    std::fs::canonicalize(&link).unwrap(),
                    std::fs::canonicalize(&dst).unwrap(),
                    "{tag} {attempt}: the link itself is mended"
                );
            }
            FLUSH_FAULT.with(|c| c.set(false));
            let paused = resume_paused_at_once(&f);
            assert!(paused.errors.is_empty(), "{tag}: {:?}", paused.errors);
            assert!(load_marker(&f.conn, &f.key).unwrap().relinking.is_empty());
            let _ = std::fs::remove_dir_all(&f.base);
        }
    }

    /// Play's check (#415): every active mod whose mods-folder entry FS25 can't open is named:
    /// missing, reaching nothing, or reaching the wrong kind of thing, whether or not the move
    /// still records it (a cancel stops recording a mod once it's home). A copy (or a hardlink)
    /// of the right kind and an inactive mod are left out.
    #[test]
    fn unreachable_mods_names_every_active_mod_whose_entry_fails() {
        let f = fx("unreach");
        let mut rows = Vec::new();
        let mut add = |tech: &str, kind: &str, active: bool| {
            let file = if kind == "dir" {
                tech.to_string()
            } else {
                format!("{tech}.zip")
            };
            rows.push(row(tech, &file, kind, active));
            f.mods.join(file)
        };
        add("FS25_Missing", "zip", true);
        add("FS25_Inactive", "zip", false);
        // An entry FS25 can open: a copy (or a hardlink) of the right kind.
        std::fs::write(add("FS25_Copy", "zip", true), b"PK").unwrap();
        std::fs::create_dir_all(add("FS25_Folder", "dir", true)).unwrap();
        // The wrong kind: a file where a folder mod should be, a folder where a zip should be.
        std::fs::write(add("FS25_Wrong", "dir", true), b"x").unwrap();
        std::fs::create_dir_all(add("FS25_WrongZip", "zip", true)).unwrap();
        // A folder link (a junction, if symlinks need Developer Mode) to a folder that's gone.
        let gone = f.base.join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        let dangling = add("FS25_Dangling", "dir", true);
        organize::link_dir_native(&gone, &dangling).unwrap();
        std::fs::remove_dir(&gone).unwrap();
        // A link to a folder that's there is fine.
        let there = f.base.join("there");
        std::fs::create_dir_all(&there).unwrap();
        organize::link_dir_native(&there, &add("FS25_Linked", "dir", true)).unwrap();
        assert_eq!(
            unreachable_mods(&f.mods, &rows),
            vec![
                "FS25_Dangling",
                "FS25_Missing",
                "FS25_Wrong",
                "FS25_WrongZip"
            ]
        );
        let _ = std::fs::remove_dir_all(&f.base);
    }

    /// After a crash between a rename and its re-link, before any Resume, and after a Resume
    /// that couldn't mend it, Play's check names the mod; once mended, nothing.
    #[test]
    fn unreachable_mods_follows_a_crashed_rename_until_it_is_mended() {
        let f = fx("unreachcrash");
        let Some((src, _, _)) = crashed_with_unrecognizable_link(&f) else {
            let _ = std::fs::remove_dir_all(&f.base);
            return;
        };
        let rows = db::load_organized(&f.conn, &f.key);
        assert!(
            load_marker(&f.conn, &f.key).is_some(),
            "the move is unfinished"
        );
        assert_eq!(unreachable_mods(&f.mods, &rows), vec!["FS25_D"]);
        // One copy in each archive: the mend can't tell which to reach, and pauses.
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
        let paused = resume_paused_at_once(&f);
        assert_eq!(paused.errors.len(), 1, "{:?}", paused.errors);
        assert_eq!(unreachable_mods(&f.mods, &rows), vec!["FS25_D"]);
        std::fs::remove_dir_all(&src).unwrap();
        let paused = resume_paused_at_once(&f);
        assert!(paused.errors.is_empty(), "{:?}", paused.errors);
        assert!(unreachable_mods(&f.mods, &rows).is_empty());
        let _ = std::fs::remove_dir_all(&f.base);
    }
}
